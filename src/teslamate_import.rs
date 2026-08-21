//! TeslaMate full-snapshot migration publisher.
//!
//! The PostgreSQL reader is intentionally separate from this module. Once a
//! reviewed, repeatable-read history exists, this module gives it a stable Hub
//! identity, maps only the selected car, and publishes either:
//! - the first immutable full-snapshot base; or
//! - a typed ordered import delta successor bound to that base when history
//!   changes. It never invents a second V2 base or wraps a full-snapshot pack
//!   with a foreign snapshot identity as a lineage delta.

use std::collections::HashSet;

#[path = "performance_profile.rs"]
mod performance_profile;
pub use performance_profile::{
    EffectiveImportProfile, PerformanceProfileError, derive_effective_import_profile,
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    credentials::TeslaMatePostgresPassword,
    db::{
        HubStore, PublicationGate, SourceDescriptor, StoreError,
        TeslaMateIdentityRegistrationCheckpoint, TeslaMateImportProjectionInventory,
        VehicleDescriptor,
    },
    hub_pack::{
        BuiltProjectionPack, ProjectionBinding, ProjectionCar, ProjectionCharge,
        ProjectionChargeSample, ProjectionDelta, ProjectionDeltaEntity, ProjectionDeltaPackRequest,
        ProjectionDrive, ProjectionPackError, ProjectionPackRequest, ProjectionPackWriter,
        ProjectionPosition, ProjectionState, ProjectionTombstone, ProjectionUpdate,
        signed_full_snapshot_manifest,
    },
    protocol::{
        CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V2, LineageDelta, OpaqueCursor, PROTOCOL_V1,
        ProtocolLimits, SequenceRange, Sha256Digest, canonical_delta_chain_digest,
    },
    teslamate::ReadOnlySource,
    teslamate_direct::{
        DirectSnapshotCapture, DirectUpdatesSourceV2_2, TeslaMateDirectError,
        capture_direct_snapshot_for_legacy_bridge,
        capture_direct_successor_diff_with_projection_state,
        write_direct_full_snapshot_with_projection_state,
    },
    teslamate_fragments::{
        StagedProjectionPacks, TeslaMateFragmentLimits,
        write_staged_full_snapshot_with_projection_state,
    },
    teslamate_projection::{
        ProjectionReport, TeslaMateCar, TeslaMateHistory, TeslaMateProjection, project_car,
        project_vehicle,
    },
    teslamate_projection::{TeslaMateOpenSession, TeslaMateSourceWatermark},
    teslamate_projection_state::{
        TeslaMateProjectionStateCapture, TeslaMateProjectionStateEntity,
        TeslaMateProjectionStateError, TeslaMateProjectionStateLimits,
    },
    teslamate_reader::{
        TeslaMateLegacyTokenCiphertexts, TeslaMateReadLimits, read_open_session, read_selected_car,
    },
    teslamate_stage::{TeslaMateStage, TeslaMateStageTable},
    updates_delivery::{
        ProductionUpdatesPublication, UpdatesDeliveryError, production_updates_head,
        publish_production_updates_schema_22_with_gate,
    },
};

/// Import scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeslaMateImportScope {
    Selected(i64),
}

/// Non-secret input that identifies one TeslaMate migration source and scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateImportRequest {
    /// Owner-chosen durable label. It must survive a hostname or port change;
    /// it is the stable Hub source key, never a PostgreSQL URL or password.
    pub source_key: String,
    pub scope: TeslaMateImportScope,
    pub imported_at_ms: i64,
}

/// Result of a successful immutable snapshot publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateImportReport {
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub snapshot_id: Uuid,
    pub sequence: u64,
    pub projection: ProjectionReport,
    pub projected_rows: u64,
    pub skipped: bool,
    pub cutover_unsettled: bool,
}

impl TeslaMateImportReport {
    /// Machine-readable disclosure for the selected THP1 projection. This is
    /// a method rather than a stored field so existing report construction and
    /// callers remain source-compatible.
    pub fn source_parity_report(&self) -> crate::teslamate_parity::TeslaMateSourceParityReport {
        crate::teslamate_parity::TeslaMateSourceParityReport::current()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeslaMateCutoverReconciliation {
    pub session: TeslaMateOpenSession,
    pub cutover_unsettled: bool,
}

/// Reconcile two bounded open-session reads by TeslaMate source row identity.
/// A still-open parent with new child rows is not claimed complete; a parent
/// that disappeared is a valid close transition and the second snapshot wins.
pub fn reconcile_open_session_cutover(
    first: &TeslaMateOpenSession,
    second: &TeslaMateOpenSession,
) -> Result<TeslaMateCutoverReconciliation, TeslaMateImportError> {
    first.validate().map_err(TeslaMateImportError::Projection)?;
    second
        .validate()
        .map_err(TeslaMateImportError::Projection)?;
    if first.car_id != second.car_id {
        return Err(TeslaMateImportError::CutoverCarMismatch);
    }
    let drive_continues =
        same_id(first.drive.as_ref(), second.drive.as_ref()) && has_new_positions(first, second);
    let charge_continues =
        same_id(first.charge.as_ref(), second.charge.as_ref()) && has_new_samples(first, second);
    // Never merge children across different source parents. A parent change
    // means the first parent may have just completed while the next began; the
    // caller must refresh completed history before it publishes either tail.
    let drive_parent_changed = active_parent_changed(first.drive.as_ref(), second.drive.as_ref());
    let charge_parent_changed =
        active_parent_changed(first.charge.as_ref(), second.charge.as_ref());
    let standalone_continues = second.standalone_positions.iter().any(|row| {
        !first
            .standalone_positions
            .iter()
            .any(|old| old.id == row.id)
    });
    let mut session = second.clone();
    if same_id(first.drive.as_ref(), second.drive.as_ref()) && second.drive.is_some() {
        session.drive_positions = union_positions(&first.drive_positions, &second.drive_positions);
    }
    if same_id(first.charge.as_ref(), second.charge.as_ref()) && second.charge.is_some() {
        session.charge_samples = union_samples(&first.charge_samples, &second.charge_samples);
    }
    session.standalone_positions =
        union_positions(&first.standalone_positions, &second.standalone_positions);
    session.watermarks = observed_open_watermarks(&session);
    Ok(TeslaMateCutoverReconciliation {
        session,
        cutover_unsettled: drive_continues
            || charge_continues
            || standalone_continues
            || drive_parent_changed
            || charge_parent_changed,
    })
}

fn same_id<T>(first: Option<&T>, second: Option<&T>) -> bool
where
    T: HasSourceId,
{
    first
        .zip(second)
        .is_some_and(|(left, right)| left.source_id() == right.source_id())
}

fn active_parent_changed<T>(first: Option<&T>, second: Option<&T>) -> bool
where
    T: HasSourceId,
{
    first
        .zip(second)
        .is_some_and(|(left, right)| left.source_id() != right.source_id())
}

trait HasSourceId {
    fn source_id(&self) -> i64;
}

impl HasSourceId for crate::teslamate_projection::TeslaMateDrive {
    fn source_id(&self) -> i64 {
        self.id
    }
}
impl HasSourceId for crate::teslamate_projection::TeslaMateChargingProcess {
    fn source_id(&self) -> i64 {
        self.id
    }
}

fn has_new_positions(first: &TeslaMateOpenSession, second: &TeslaMateOpenSession) -> bool {
    second
        .drive_positions
        .iter()
        .any(|row| !first.drive_positions.iter().any(|old| old.id == row.id))
        || second.watermarks.positions.max_id > first.watermarks.positions.max_id
}

fn has_new_samples(first: &TeslaMateOpenSession, second: &TeslaMateOpenSession) -> bool {
    second
        .charge_samples
        .iter()
        .any(|row| !first.charge_samples.iter().any(|old| old.id == row.id))
        || second.watermarks.charges.max_id > first.watermarks.charges.max_id
}

fn union_positions(
    first: &[crate::teslamate_projection::TeslaMatePosition],
    second: &[crate::teslamate_projection::TeslaMatePosition],
) -> Vec<crate::teslamate_projection::TeslaMatePosition> {
    let mut rows = first.to_vec();
    for row in second {
        if let Some(existing) = rows.iter_mut().find(|old| old.id == row.id) {
            *existing = row.clone();
        } else {
            rows.push(row.clone());
        }
    }
    rows.sort_by_key(|row| row.id);
    rows
}

fn union_samples(
    first: &[crate::teslamate_projection::TeslaMateCharge],
    second: &[crate::teslamate_projection::TeslaMateCharge],
) -> Vec<crate::teslamate_projection::TeslaMateCharge> {
    let mut rows = first.to_vec();
    for row in second {
        if let Some(existing) = rows.iter_mut().find(|old| old.id == row.id) {
            *existing = row.clone();
        } else {
            rows.push(row.clone());
        }
    }
    rows.sort_by_key(|row| row.id);
    rows
}

fn observed_open_watermarks(
    session: &TeslaMateOpenSession,
) -> crate::teslamate_projection::TeslaMateSourceWatermarks {
    let positions = session
        .drive_positions
        .iter()
        .chain(session.standalone_positions.iter());
    let max_position_id = positions.clone().map(|row| row.id).max();
    let max_position_timestamp = positions.map(|row| row.date_ms).max();
    crate::teslamate_projection::TeslaMateSourceWatermarks {
        drives: session
            .drive
            .as_ref()
            .map_or_else(TeslaMateSourceWatermark::default, |row| {
                TeslaMateSourceWatermark {
                    max_id: Some(row.id),
                    max_timestamp_ms: Some(row.start_date_ms),
                }
            }),
        positions: TeslaMateSourceWatermark {
            max_id: max_position_id,
            max_timestamp_ms: max_position_timestamp,
        },
        charging_processes: session.charge.as_ref().map_or_else(
            TeslaMateSourceWatermark::default,
            |row| TeslaMateSourceWatermark {
                max_id: Some(row.id),
                max_timestamp_ms: Some(row.start_date_ms),
            },
        ),
        charges: TeslaMateSourceWatermark {
            max_id: session.charge_samples.iter().map(|row| row.id).max(),
            max_timestamp_ms: session.charge_samples.iter().map(|row| row.date_ms).max(),
        },
        states: session
            .state
            .as_ref()
            .map_or_else(TeslaMateSourceWatermark::default, |row| {
                TeslaMateSourceWatermark {
                    max_id: Some(row.id),
                    max_timestamp_ms: Some(row.start_date_ms),
                }
            }),
        updates: TeslaMateSourceWatermark::default(),
    }
}

struct ImportGenerationGuard<'a> {
    store: &'a HubStore,
    run_id: Option<Uuid>,
}

struct TeslaMateIdentityRegistrationGuard<'a> {
    store: &'a HubStore,
    checkpoint: Option<TeslaMateIdentityRegistrationCheckpoint>,
}

impl TeslaMateIdentityRegistrationGuard<'_> {
    fn rollback(&mut self) -> Result<(), StoreError> {
        if let Some(checkpoint) = self.checkpoint.take() {
            self.store
                .rollback_teslamate_identity_registration(&checkpoint)?;
        }
        Ok(())
    }

    fn disarm(&mut self) {
        self.checkpoint = None;
    }
}

impl Drop for TeslaMateIdentityRegistrationGuard<'_> {
    fn drop(&mut self) {
        if let Some(checkpoint) = self.checkpoint.take()
            && let Err(error) = self
                .store
                .rollback_teslamate_identity_registration(&checkpoint)
        {
            tracing::error!(%error, "could not roll back unproved TeslaMate identity registration");
        }
    }
}

/// Delta packs are content-addressed files before their transaction commits.
/// Keep their ownership local to this importer so an error can never leave a
/// newly-written sparse successor available for accidental later adoption.
struct UnpublishedDirectDeltaPacks<'a> {
    store: &'a HubStore,
    publication_gate: &'a PublicationGate,
    chunks: Vec<BuiltProjectionPack>,
    published: bool,
}

impl<'a> UnpublishedDirectDeltaPacks<'a> {
    fn new(store: &'a HubStore, publication_gate: &'a PublicationGate) -> Self {
        Self {
            store,
            publication_gate,
            chunks: Vec::new(),
            published: false,
        }
    }

    fn push(&mut self, chunk: BuiltProjectionPack) {
        self.chunks.push(chunk);
    }

    fn published(&mut self) {
        self.published = true;
    }
}

impl Drop for UnpublishedDirectDeltaPacks<'_> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        for chunk in &self.chunks {
            if !chunk.may_remove_unpublished_file() {
                continue;
            }
            if let Err(error) = self.store.remove_unretained_pack(
                self.publication_gate,
                chunk.metadata.sha256,
                &chunk.path,
            ) {
                tracing::warn!(path = %chunk.path.display(), %error, "could not durably remove unpublished direct TeslaMate delta pack");
            }
        }
    }
}

impl ImportGenerationGuard<'_> {
    fn disarm(&mut self) {
        self.run_id = None;
    }
}

impl Drop for ImportGenerationGuard<'_> {
    fn drop(&mut self) {
        if let Some(run_id) = self.run_id.take() {
            let _ = self.store.abort_import_generation(run_id);
        }
    }
}

fn selected_car_id(request: &TeslaMateImportRequest) -> Result<i64, TeslaMateImportError> {
    match request.scope {
        TeslaMateImportScope::Selected(id) if id > 0 => Ok(id),
        TeslaMateImportScope::Selected(_) => Err(TeslaMateImportError::InvalidSelectedCarId),
    }
}

/// Build a fresh private state capture for one direct source-snapshot attempt.
/// Fragment-limit retries reopen the same exported PostgreSQL snapshot but
/// discard the attempted packs, so they must also discard and recreate the
/// state spool rather than record a candidate twice.
fn direct_projection_state_capture(
    store: &HubStore,
    publication_gate: &PublicationGate,
    run_id: Uuid,
    vehicle_id: Uuid,
    source_id: Uuid,
    selected_car_id: i64,
    successor: bool,
    read_limits: TeslaMateReadLimits,
) -> Result<TeslaMateProjectionStateCapture, TeslaMateDirectError> {
    let maximum_rows = u64::try_from(read_limits.maximum_rows)
        .map_err(|_| TeslaMateDirectError::TargetCapacityOverflow)?;
    let state = store.create_import_projection_state(
        publication_gate,
        run_id,
        TeslaMateProjectionStateLimits {
            max_rows: maximum_rows,
            max_state_bytes: read_limits.maximum_stage_bytes,
            max_changed_payload_bytes: read_limits.maximum_stage_bytes,
            minimum_free_bytes: read_limits.minimum_free_bytes,
        },
        DIRECT_DELTA_BATCH_PAYLOAD_BYTES,
    )?;
    if !successor {
        return Ok(TeslaMateProjectionStateCapture::for_initial_base(state));
    }
    let prior =
        store.teslamate_import_projection_state_lookup(vehicle_id, source_id, selected_car_id)?;
    Ok(TeslaMateProjectionStateCapture::for_successor(
        state,
        Box::new(prior),
    ))
}

/// Capture one direct source snapshot for either the normal import path or
/// the one-time inventory-only compatibility bridge. The bridge deliberately
/// uses initial-state capture and a retired physical fragment layout, but it
/// never writes a candidate pack.
#[allow(clippy::too_many_arguments)]
async fn capture_direct_import_snapshot(
    store: &HubStore,
    publication_gate: &PublicationGate,
    run_id: Uuid,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
    binding: ProjectionBinding,
    capture_snapshot_id: Uuid,
    capture_range: SequenceRange,
    source_id: Uuid,
    vehicle_id: Uuid,
    successor: bool,
    legacy_bridge: bool,
    capture_legacy_token: bool,
) -> Result<DirectSnapshotCapture, TeslaMateDirectError> {
    let writer = ProjectionPackWriter::new(store.packs_dir())
        .with_minimum_free_bytes(limits.minimum_free_bytes);
    if legacy_bridge {
        return capture_direct_snapshot_for_legacy_bridge(
            source,
            password,
            selected_car_id,
            limits,
            &writer,
            binding,
            capture_snapshot_id,
            capture_range,
            capture_legacy_token,
            || {
                direct_projection_state_capture(
                    store,
                    publication_gate,
                    run_id,
                    vehicle_id,
                    source_id,
                    selected_car_id,
                    false,
                    limits,
                )
            },
        )
        .await;
    }
    let capture_factory = || -> Result<TeslaMateProjectionStateCapture, TeslaMateDirectError> {
        direct_projection_state_capture(
            store,
            publication_gate,
            run_id,
            vehicle_id,
            source_id,
            selected_car_id,
            successor,
            limits,
        )
    };
    if successor {
        capture_direct_successor_diff_with_projection_state(
            source,
            password,
            selected_car_id,
            limits,
            &writer,
            binding,
            capture_snapshot_id,
            capture_range,
            capture_legacy_token,
            capture_factory,
        )
        .await
    } else {
        write_direct_full_snapshot_with_projection_state(
            source,
            password,
            selected_car_id,
            limits,
            &writer,
            binding,
            capture_snapshot_id,
            capture_range,
            capture_legacy_token,
            capture_factory,
        )
        .await
    }
}

fn legacy_direct_bridge_error(error: StoreError) -> TeslaMateImportError {
    match error {
        StoreError::TeslaMateLegacyDirectRebaseRequired(_) => {
            TeslaMateImportError::LegacyDirectImportRebaseRequired
        }
        error => error.into(),
    }
}

/// Read, validate, pack, sign, and publish one complete TeslaMate snapshot.
/// The caller supplies secrets as explicit local values; no secret is stored
/// in the Hub database or encoded in the generated pack.
pub async fn import_from_postgres(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    Ok(import_from_postgres_with_updates_capture(
        store, source, password, cursor_key, request, limits, false,
    )
    .await?
    .report)
}

/// Production selected-car command path. The legacy schema-2.1 catalogue
/// transaction commits first. The exact physical updates retained from that
/// same exported PostgreSQL snapshot are then written as a manifest-last
/// schema-2.2 pair. A second-step failure is explicit and safe to retry.
pub async fn import_selected_from_postgres_with_schema_22(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateSelectedImportReport, TeslaMateImportError> {
    let captured = import_from_postgres_with_updates_capture(
        store, source, password, cursor_key, request, limits, false,
    )
    .await?;
    finish_selected_schema_22_publication(store, cursor_key, captured)
}

/// As [`import_selected_from_postgres_with_schema_22`], while retaining the
/// source's encrypted legacy token pair from the exact exported snapshot.
/// The ciphertext stays opaque here and is returned only to the migration CLI.
pub async fn import_selected_from_postgres_with_schema_22_and_legacy_token(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    limits: TeslaMateReadLimits,
) -> Result<
    (
        TeslaMateSelectedImportReport,
        TeslaMateLegacyTokenCiphertexts,
    ),
    TeslaMateImportError,
> {
    let mut captured = import_from_postgres_with_updates_capture(
        store, source, password, cursor_key, request, limits, true,
    )
    .await?;
    let legacy_tokens = captured
        .legacy_tokens
        .take()
        .ok_or(TeslaMateImportError::LegacyTokenCaptureMissing)?;
    let report = finish_selected_schema_22_publication(store, cursor_key, captured)?;
    Ok((report, legacy_tokens))
}

#[derive(Debug)]
struct CapturedTeslaMateImport {
    report: TeslaMateImportReport,
    binding: ProjectionBinding,
    updates_v2_2: DirectUpdatesSourceV2_2,
    legacy_tokens: Option<TeslaMateLegacyTokenCiphertexts>,
    publication_gate: PublicationGate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateSelectedImportReport {
    pub import: TeslaMateImportReport,
    pub updates_schema_22: ProductionUpdatesPublication,
}

fn finish_selected_schema_22_publication(
    store: &HubStore,
    cursor_key: &CursorKey,
    captured: CapturedTeslaMateImport,
) -> Result<TeslaMateSelectedImportReport, TeslaMateImportError> {
    let CapturedTeslaMateImport {
        report,
        binding,
        updates_v2_2,
        legacy_tokens: _,
        publication_gate,
    } = captured;
    let vehicle_id = report.vehicle_id;
    let legacy_snapshot_id = report.snapshot_id;
    // Observe the schema-2.2 head only after this command's legacy transaction
    // has committed. The same retained gate has covered source capture and the
    // legacy commit, so no other publisher can interleave before this check.
    let expected_schema_22_head = production_updates_head(store, vehicle_id).map_err(|source| {
        TeslaMateImportError::Schema22PostCommit {
            vehicle_id,
            legacy_snapshot_id,
            source,
        }
    })?;
    let updates_schema_22 = publish_production_updates_schema_22_with_gate(
        store,
        cursor_key,
        &binding,
        updates_v2_2,
        &publication_gate,
        &expected_schema_22_head,
        Some((legacy_snapshot_id, report.sequence)),
    )
    .map_err(|source| TeslaMateImportError::Schema22PostCommit {
        vehicle_id,
        legacy_snapshot_id,
        source,
    })?;
    Ok(TeslaMateSelectedImportReport {
        import: report,
        updates_schema_22,
    })
}

async fn import_from_postgres_with_updates_capture(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    limits: TeslaMateReadLimits,
    capture_legacy_token: bool,
) -> Result<CapturedTeslaMateImport, TeslaMateImportError> {
    let publication_gate = store.acquire_publication_gate().await?;
    let selected_car_id = selected_car_id(request)?;
    let car = read_selected_car(source, password, selected_car_id, limits).await?;
    let source_vehicle_key = stable_vehicle_key_for_car(&car)?;
    let source_descriptor = SourceDescriptor::new("teslamate", request.source_key.clone());
    let (registered_source, source_created) =
        store.register_teslamate_import_source(&source_descriptor, request.imported_at_ms)?;
    let deterministic_vehicle_id =
        Uuid::new_v5(&registered_source.source_id, source_vehicle_key.as_bytes());
    let identity_hint = VehicleDescriptor {
        source_id: registered_source.source_id,
        source_vehicle_key: source_vehicle_key.clone(),
        vin: nonblank(car.vin.as_deref()).map(ToOwned::to_owned),
        display_name: nonblank(car.name.as_deref()).map(ToOwned::to_owned),
        tesla_eid: (car.eid > 0).then_some(car.eid),
        tesla_vid: car.vid.filter(|value| *value > 0),
    };
    // The pre-read car selects the deterministic local key, but it is not yet
    // trusted to create VIN/EID/VID aliases.  Persist only an alias-free row;
    // the exported repeatable-read snapshot below must prove the exact tuple
    // before any strong identity becomes visible.
    let (vehicle, registration_checkpoint) = store.provision_teslamate_import_identity(
        &registered_source,
        source_created,
        &identity_hint,
        request.imported_at_ms,
        deterministic_vehicle_id,
    )?;
    let mut identity_registration_guard = TeslaMateIdentityRegistrationGuard {
        store,
        checkpoint: Some(registration_checkpoint),
    };
    let binding = ProjectionBinding {
        installation_id: store.installation_id()?,
        account_id: registered_source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: registered_source.generation,
        selected_car_id,
    };
    // Direct full-snapshot packs are a bounded capture transport even after
    // an immutable V2 base exists. Their identity must remain fresh so an
    // unneeded capture can never share a catalogued base object's path.
    let prior_v2_head = store.v2_head(vehicle.vehicle_id)?;
    let legacy_bridge = if prior_v2_head.is_some()
        && !store.teslamate_import_projection_state_exists(vehicle.vehicle_id)?
    {
        if !store.legacy_teslamate_direct_bridge_is_eligible(
            vehicle.vehicle_id,
            registered_source.source_id,
            selected_car_id,
        )? {
            return Err(TeslaMateImportError::LegacyDirectImportRebaseRequired);
        }
        true
    } else {
        false
    };
    let successor = prior_v2_head.is_some() && !legacy_bridge;
    let capture_snapshot_id = Uuid::new_v4();
    let capture_sequence = match prior_v2_head {
        Some((_, head_sequence, _)) => u64::try_from(head_sequence)
            .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
        None => store.reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)?,
    };
    let capture_range = SequenceRange {
        from_exclusive: capture_sequence,
        to_inclusive: capture_sequence,
    };
    let run_id = store.begin_import_generation(
        registered_source.source_id,
        vehicle.vehicle_id,
        selected_car_id,
        request.imported_at_ms,
    )?;
    let mut run_guard = ImportGenerationGuard {
        store,
        run_id: Some(run_id),
    };
    let mut open_session = match read_open_session(source, password, selected_car_id, limits).await
    {
        Ok(value) => value,
        Err(error) => {
            store.abort_import_generation(run_id)?;
            return Err(error.into());
        }
    };
    open_session.watermarks = observed_open_watermarks(&open_session);
    store.stage_import_generation_session(run_id, &open_session)?;
    // Capture completed history only after the first bounded tail read. If an
    // earlier active parent A already closed and B opened before this import
    // reached the tail, this repeatable-read snapshot includes completed A
    // rather than treating it as an open row to omit.
    let first_capture = match capture_direct_import_snapshot(
        store,
        &publication_gate,
        run_id,
        source,
        password,
        selected_car_id,
        limits,
        binding.clone(),
        capture_snapshot_id,
        capture_range,
        registered_source.source_id,
        vehicle.vehicle_id,
        successor,
        legacy_bridge,
        capture_legacy_token,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            store.abort_import_generation(run_id)?;
            return Err(error.into());
        }
    };
    if let Err(error) = validate_exported_vehicle_identity(&car, &first_capture.updates_v2_2) {
        store.abort_import_generation(run_id)?;
        identity_registration_guard.rollback()?;
        return Err(error);
    }
    let exported_car = &first_capture.updates_v2_2.car;
    let verified_descriptor = VehicleDescriptor {
        source_id: registered_source.source_id,
        source_vehicle_key: source_vehicle_key.clone(),
        vin: nonblank(exported_car.vin.as_deref()).map(ToOwned::to_owned),
        display_name: nonblank(exported_car.name.as_deref()).map(ToOwned::to_owned),
        tesla_eid: Some(exported_car.eid),
        tesla_vid: Some(exported_car.vid),
    };
    let verified_vehicle = store.register_vehicle_with_id(
        &verified_descriptor,
        request.imported_at_ms,
        deterministic_vehicle_id,
    )?;
    if verified_vehicle.vehicle_id != vehicle.vehicle_id {
        store.abort_import_generation(run_id)?;
        identity_registration_guard.rollback()?;
        return Err(StoreError::VehicleIdentityMismatch {
            expected: vehicle.vehicle_id,
            actual: verified_vehicle.vehicle_id,
        }
        .into());
    }
    identity_registration_guard.disarm();
    let mut direct = first_capture.packs;
    let mut updates_v2_2 = first_capture.updates_v2_2;
    let mut legacy_tokens = first_capture.legacy_tokens;
    let mut second_open_session =
        match read_open_session(source, password, selected_car_id, limits).await {
            Ok(value) => value,
            Err(error) => {
                store.abort_import_generation(run_id)?;
                return Err(error.into());
            }
        };
    second_open_session.watermarks = observed_open_watermarks(&second_open_session);
    let cutover = match reconcile_open_session_cutover(&open_session, &second_open_session) {
        Ok(value) => value,
        Err(error) => {
            store.abort_import_generation(run_id)?;
            return Err(error);
        }
    };
    let active_parent_changed = active_parent_changed(
        open_session.drive.as_ref(),
        second_open_session.drive.as_ref(),
    ) || active_parent_changed(
        open_session.charge.as_ref(),
        second_open_session.charge.as_ref(),
    );
    if active_parent_changed
        || open_session.drive.is_some() && second_open_session.drive.is_none()
        || open_session.charge.is_some() && second_open_session.charge.is_none()
    {
        // A parent either completed or was replaced between the two bounded
        // tail reads. Re-read completed history before committing the second
        // tail so the first parent cannot be lost. The reconciliation remains
        // unsettled, requiring the next bounded import to prove stability.
        let replacement = match capture_direct_import_snapshot(
            store,
            &publication_gate,
            run_id,
            source,
            password,
            selected_car_id,
            limits,
            binding.clone(),
            capture_snapshot_id,
            capture_range,
            registered_source.source_id,
            vehicle.vehicle_id,
            successor,
            legacy_bridge,
            capture_legacy_token,
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                store.abort_import_generation(run_id)?;
                return Err(error.into());
            }
        };
        if let Err(error) = validate_exported_vehicle_identity(&car, &replacement.updates_v2_2) {
            store.abort_import_generation(run_id)?;
            return Err(error);
        }
        direct = replacement.packs;
        updates_v2_2 = replacement.updates_v2_2;
        legacy_tokens = replacement.legacy_tokens;
    }
    // Always commit the reconciled second tail. `cutover_unsettled` reports
    // that the source kept changing during the bounded pass; it must not
    // discard rows already observed in that pass.
    store.stage_import_generation_session(run_id, &cutover.session)?;
    direct.fingerprint = direct_snapshot_fingerprint(&direct.fingerprint, &direct.geofences)?;
    if legacy_bridge {
        let legacy_physical_fingerprint = direct
            .legacy_physical_fingerprint
            .ok_or(TeslaMateImportError::LegacyDirectImportRebaseRequired)?;
        let legacy_fingerprint =
            direct_snapshot_fingerprint(&legacy_physical_fingerprint, &direct.geofences)?;
        let mut capture = direct
            .projection_state
            .take()
            .ok_or(TeslaMateImportError::LegacyDirectImportRebaseRequired)?;
        capture.seal()?;
        let projection_state = capture.into_state();
        let bridged = store
            .bridge_legacy_teslamate_direct_import(
                run_id,
                registered_source.source_id,
                vehicle.vehicle_id,
                selected_car_id,
                legacy_fingerprint,
                direct.fingerprint,
                &projection_state,
            )
            .map_err(legacy_direct_bridge_error)?;
        run_guard.disarm();
        return Ok(CapturedTeslaMateImport {
            report: TeslaMateImportReport {
                source_id: registered_source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id: bridged.snapshot_id,
                sequence: bridged.head_sequence,
                projection: direct.report,
                projected_rows: bridged.total_rows,
                skipped: true,
                cutover_unsettled: cutover.cutover_unsettled,
            },
            binding: binding.clone(),
            updates_v2_2,
            legacy_tokens,
            publication_gate,
        });
    }
    let transport_rows = transport_row_count(&direct.chunks)?;
    if store.source_fingerprint_matches(vehicle.vehicle_id, direct.fingerprint)? {
        promote_unchanged_direct_import(
            store,
            &publication_gate,
            run_id,
            registered_source.source_id,
            vehicle.vehicle_id,
            selected_car_id,
            request.imported_at_ms,
            &mut direct,
        )?;
        run_guard.disarm();
        if let Some((snapshot_id, head_sequence, _)) = store.v2_head(vehicle.vehicle_id)? {
            return Ok(CapturedTeslaMateImport {
                report: TeslaMateImportReport {
                    source_id: registered_source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    snapshot_id,
                    sequence: u64::try_from(head_sequence)
                        .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
                    projection: direct.report,
                    projected_rows: transport_rows,
                    skipped: true,
                    cutover_unsettled: cutover.cutover_unsettled,
                },
                binding: binding.clone(),
                updates_v2_2,
                legacy_tokens,
                publication_gate,
            });
        }
        if let Some(current) =
            store.manifest_for_snapshot_fingerprint(vehicle.vehicle_id, direct.fingerprint)?
        {
            return Ok(CapturedTeslaMateImport {
                report: TeslaMateImportReport {
                    source_id: registered_source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    snapshot_id: current.snapshot_id,
                    sequence: current.head_sequence,
                    projection: direct.report,
                    projected_rows: current.total_rows,
                    skipped: true,
                    cutover_unsettled: cutover.cutover_unsettled,
                },
                binding: binding.clone(),
                updates_v2_2,
                legacy_tokens,
                publication_gate,
            });
        }
    }
    if let Some((base_snapshot_id, head_sequence, head_digest)) = prior_v2_head {
        // These full snapshot capture packs cannot be published as deltas.
        // Remove their unreferenced files before writing the sparse typed
        // successor; catalogue checks preserve any coincident object.
        direct.keep_chunks();
        discard_unpublished_chunks(store, &publication_gate, &direct.chunks)?;
        let selected_car = direct
            .selected_car
            .clone()
            .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?;
        let mut capture = direct
            .projection_state
            .take()
            .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?;
        capture.seal()?;
        let from_sequence = u64::try_from(head_sequence)
            .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?;
        let existing = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)?
            .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
        let existing_pack_count = existing
            .base
            .packs
            .len()
            .checked_add(existing.deltas.len())
            .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
        let remaining_delta_packs = crate::protocol::ProtocolLimits::default()
            .max_chunks
            .checked_sub(existing_pack_count)
            .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
        if remaining_delta_packs == 0 {
            return Err(crate::db::StoreError::LineageCatalogConflict.into());
        }
        let mut next_ordinal = store.next_v2_pack_ordinal(base_snapshot_id)?;
        let mut parent_digest = head_digest;
        let mut prior_sequence = from_sequence;
        // Keep one decoded delta batch at a time.  The projection-state spool
        // owns the complete current history; retaining every decoded sparse
        // row here would turn a changed multi-million-row import back into an
        // in-memory history.
        let mut deltas = Vec::new();
        let mut delta_packs = UnpublishedDirectDeltaPacks::new(store, &publication_gate);
        direct_delta_rows_from_capture(
            &mut capture,
            &binding,
            &selected_car,
            remaining_delta_packs,
            |batch| {
                let to_sequence = store
                    .reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)?;
                if to_sequence <= prior_sequence {
                    return Err(crate::db::StoreError::LineageCatalogConflict.into());
                }
                let delta = batch.into_delta(
                    binding.clone(),
                    SequenceRange {
                        from_exclusive: prior_sequence,
                        to_inclusive: to_sequence,
                    },
                    parent_digest,
                );
                let built = ProjectionPackWriter::new(store.packs_dir())
                    .with_minimum_free_bytes(limits.minimum_free_bytes)
                    .write_delta(&ProjectionDeltaPackRequest {
                        pack_id: Uuid::new_v4(),
                        snapshot_id: base_snapshot_id,
                        ordinal: next_ordinal,
                        delta: &delta,
                    })?;
                let chain_digest =
                    canonical_delta_chain_digest(parent_digest, built.metadata.sha256);
                deltas.push(LineageDelta {
                    from_sequence: prior_sequence,
                    to_sequence,
                    parent_chain_digest: parent_digest,
                    chain_digest,
                    pack_digest: built.metadata.sha256,
                    pack: built.metadata.clone(),
                });
                delta_packs.push(built);
                prior_sequence = to_sequence;
                parent_digest = chain_digest;
                next_ordinal = next_ordinal
                    .checked_add(1)
                    .ok_or(crate::db::StoreError::PackOrdinalTooLarge)?;
                Ok(())
            },
        )?;
        let projection_state = capture.into_state();
        let terminal_cursor = OpaqueCursor::issue(
            cursor_key,
            CursorClaims {
                protocol: PROTOCOL_V1,
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding.installation_id,
                account_id: binding.account_id,
                vehicle_id: binding.vehicle_id,
                generation: binding.generation,
                sequence: prior_sequence,
            },
        )
        .map_err(crate::db::StoreError::Manifest)?;
        store.finalize_import_generation_delta_successors_with_projection_state(
            run_id,
            registered_source.source_id,
            vehicle.vehicle_id,
            selected_car_id,
            request.imported_at_ms,
            &deltas,
            cursor_key,
            &terminal_cursor,
            direct.fingerprint,
            &direct.geofences,
            &projection_state,
        )?;
        delta_packs.published();
        drop(delta_packs);
        run_guard.disarm();
        let projected_rows = deltas.iter().try_fold(0_u64, |total, delta| {
            total
                .checked_add(delta.pack.row_count)
                .ok_or(TeslaMateImportError::DirectDeltaBatchOverflow)
        })?;
        return Ok(CapturedTeslaMateImport {
            report: TeslaMateImportReport {
                source_id: registered_source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id: base_snapshot_id,
                sequence: prior_sequence,
                projection: direct.report,
                projected_rows,
                skipped: false,
                cutover_unsettled: cutover.cutover_unsettled,
            },
            binding: binding.clone(),
            updates_v2_2,
            legacy_tokens,
            publication_gate,
        });
    }
    let mut capture = direct
        .projection_state
        .take()
        .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?;
    capture.seal()?;
    let projection_state = capture.into_state();
    let manifest = match signed_full_snapshot_manifest(
        &binding,
        capture_snapshot_id,
        capture_range,
        &direct.chunks,
        transport_rows,
        cursor_key,
    ) {
        Ok(value) => value,
        Err(error) => {
            store.abort_import_generation(run_id)?;
            return Err(error.into());
        }
    };
    // A pre-commit failure can leave only unreferenced candidate packs; repair
    // may remove those safely. After the transaction commits they are catalogued.
    if let Err(error) = store.finalize_import_generation_with_projection_state(
        run_id,
        registered_source.source_id,
        vehicle.vehicle_id,
        selected_car_id,
        request.imported_at_ms,
        &manifest,
        direct.fingerprint,
        &direct.geofences,
        &binding,
        &projection_state,
    ) {
        reconcile_failed_full_snapshot_candidate(store, &publication_gate, &mut direct, &error)?;
        return Err(error.into());
    }
    direct.keep_chunks();
    run_guard.disarm();
    Ok(CapturedTeslaMateImport {
        report: TeslaMateImportReport {
            source_id: registered_source.source_id,
            vehicle_id: vehicle.vehicle_id,
            snapshot_id: capture_snapshot_id,
            sequence: capture_sequence,
            projection: direct.report,
            projected_rows: manifest.total_rows,
            skipped: false,
            cutover_unsettled: cutover.cutover_unsettled,
        },
        binding,
        updates_v2_2,
        legacy_tokens,
        publication_gate,
    })
}

#[cfg(test)]
fn publish_staged_history(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    stage: &TeslaMateStage,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    let open_session = TeslaMateOpenSession {
        car_id: selected_car_id(request)?,
        ..TeslaMateOpenSession::default()
    };
    publish_staged_history_with_limits(
        store,
        cursor_key,
        request,
        stage,
        &open_session,
        TeslaMateFragmentLimits::default(),
    )
}

/// Publish a sealed capture and the open-session image read from its same
/// repeatable-read source snapshot.
pub fn publish_staged_history_with_session(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    stage: &TeslaMateStage,
    open_session: &TeslaMateOpenSession,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    publish_staged_history_with_limits(
        store,
        cursor_key,
        request,
        stage,
        open_session,
        TeslaMateFragmentLimits::default(),
    )
}

fn publish_staged_history_with_limits(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    stage: &TeslaMateStage,
    open_session: &TeslaMateOpenSession,
    fragment_limits: TeslaMateFragmentLimits,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    let publication_gate = store.try_acquire_publication_gate()?;
    let selected_car_id = selected_car_id(request)?;
    let car = stage
        .get::<TeslaMateCar>(TeslaMateStageTable::Cars, selected_car_id)?
        .ok_or(TeslaMateImportError::SelectedCarMissing)?;
    let source = store.register_source(
        &SourceDescriptor::new("teslamate", request.source_key.clone()),
        request.imported_at_ms,
    )?;
    let source_vehicle_key = stable_vehicle_key_for_car(&car)?;
    let deterministic_vehicle_id = Uuid::new_v5(&source.source_id, source_vehicle_key.as_bytes());
    let vehicle = store.register_vehicle_with_id(
        &VehicleDescriptor {
            source_id: source.source_id,
            source_vehicle_key,
            vin: nonblank(car.vin.as_deref()).map(ToOwned::to_owned),
            display_name: nonblank(car.name.as_deref()).map(ToOwned::to_owned),
            tesla_eid: Some(car.eid),
            tesla_vid: car.vid,
        },
        request.imported_at_ms,
        deterministic_vehicle_id,
    )?;
    // Staged publication has no later lifecycle writer to materialise the
    // selected car. Persist the car/settings before the immutable base is
    // visible so Serve can select this imported vehicle immediately.
    let projected_car = project_car(&car, None)?;
    store.persist_materialised_car_if_absent(vehicle.vehicle_id, &projected_car)?;
    store.upsert_car_settings(vehicle.vehicle_id, selected_car_id, &car.settings)?;
    let prior_v2_head = store.v2_head(vehicle.vehicle_id)?;
    let sequence = match prior_v2_head.as_ref() {
        Some((_, head_sequence, _)) => u64::try_from(*head_sequence)
            .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
        None => store.reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)?,
    };
    let mut import_run = ImportGenerationGuard {
        store,
        run_id: Some(store.begin_import_generation(
            source.source_id,
            vehicle.vehicle_id,
            selected_car_id,
            request.imported_at_ms,
        )?),
    };
    store.stage_import_generation_session(
        import_run
            .run_id
            .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?,
        open_session,
    )?;
    let snapshot_id = Uuid::new_v4();
    let binding = ProjectionBinding {
        installation_id: store.installation_id()?,
        account_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: source.generation,
        selected_car_id,
    };
    let range = SequenceRange {
        from_exclusive: sequence,
        to_inclusive: sequence,
    };
    let stage_limits = stage.stats()?.limits;
    // The staged source, immutable pack writer, and digest spool can coexist
    // on one filesystem. Reserve the spool's complete bounded allocation in
    // the pack writer's free-space floor, just as the direct capture path
    // does, so the new durable state cannot overcommit a successful pack
    // publication.
    let combined_minimum_free_bytes = stage_limits
        .minimum_free_bytes
        .checked_add(stage_limits.max_stage_bytes)
        .ok_or(TeslaMateProjectionStateError::StateCapacityOverflow)?;
    let writer = ProjectionPackWriter::new(store.packs_dir())
        .with_minimum_free_bytes(combined_minimum_free_bytes);
    writer.ensure_full_snapshot_capacity_for_capture(
        stage_limits.max_stage_bytes,
        combined_minimum_free_bytes,
    )?;
    let mut staged = write_staged_full_snapshot_with_projection_state(
        stage,
        &writer,
        binding.clone(),
        snapshot_id,
        range,
        fragment_limits,
        || -> Result<TeslaMateProjectionStateCapture, TeslaMateImportError> {
            let limits = TeslaMateProjectionStateLimits {
                max_rows: stage_limits.max_rows,
                max_state_bytes: stage_limits.max_stage_bytes,
                max_changed_payload_bytes: stage_limits.max_stage_bytes,
                minimum_free_bytes: stage_limits.minimum_free_bytes,
            };
            let run_id = import_run
                .run_id
                .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?;
            let state = store.create_import_projection_state(
                &publication_gate,
                run_id,
                limits,
                DIRECT_DELTA_BATCH_PAYLOAD_BYTES,
            )?;
            if prior_v2_head.is_some() {
                let prior = store.teslamate_import_projection_state_lookup(
                    vehicle.vehicle_id,
                    source.source_id,
                    selected_car_id,
                )?;
                return Ok::<TeslaMateProjectionStateCapture, TeslaMateImportError>(
                    TeslaMateProjectionStateCapture::for_successor(state, Box::new(prior)),
                );
            }
            Ok::<TeslaMateProjectionStateCapture, TeslaMateImportError>(
                TeslaMateProjectionStateCapture::for_initial_base(state),
            )
        },
    )?;
    let transport_rows = transport_row_count(&staged.chunks)?;
    let fingerprint = direct_snapshot_fingerprint(&staged.fingerprint, &staged.geofences)?;
    if store.source_fingerprint_matches(vehicle.vehicle_id, fingerprint)? {
        promote_unchanged_direct_import(
            store,
            &publication_gate,
            import_run
                .run_id
                .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?,
            source.source_id,
            vehicle.vehicle_id,
            selected_car_id,
            request.imported_at_ms,
            &mut staged,
        )?;
        import_run.disarm();
        store.upsert_geofences(vehicle.vehicle_id, &staged.geofences)?;
        if let Some((snapshot_id, head_sequence, _)) = store.v2_head(vehicle.vehicle_id)? {
            return Ok(TeslaMateImportReport {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id,
                sequence: u64::try_from(head_sequence)
                    .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
                projection: staged.report,
                projected_rows: transport_rows,
                skipped: true,
                cutover_unsettled: false,
            });
        }
        if let Some(current) =
            store.manifest_for_snapshot_fingerprint(vehicle.vehicle_id, staged.fingerprint)?
        {
            return Ok(TeslaMateImportReport {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id: current.snapshot_id,
                sequence: current.head_sequence,
                projection: staged.report,
                projected_rows: current.total_rows,
                skipped: true,
                cutover_unsettled: false,
            });
        }
    }
    if prior_v2_head.is_some() {
        return publish_staged_history_successor(
            store,
            cursor_key,
            request,
            source.source_id,
            vehicle.vehicle_id,
            selected_car_id,
            binding,
            publication_gate,
            fingerprint,
            stage_limits.minimum_free_bytes,
            staged,
            import_run,
        );
    }
    let manifest = signed_full_snapshot_manifest(
        &binding,
        snapshot_id,
        range,
        &staged.chunks,
        transport_rows,
        cursor_key,
    )?;
    // `write_staged_full_snapshot_with_limits` is deliberately schema 2.1
    // for every ordinary sealed source capture. A staged migration therefore
    // always has an immutable V2 binding; it must never fall back to the
    // generic legacy finalizer just because today's source happens to have
    // empty additive tables.
    debug_assert_eq!(manifest.schema, HUB_PROJECTION_SCHEMA_V2);
    let mut capture = staged
        .projection_state
        .take()
        .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?;
    capture.seal()?;
    let projection_state = capture.into_state();
    if let Err(error) = store.finalize_import_generation_with_projection_state(
        import_run
            .run_id
            .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?,
        source.source_id,
        vehicle.vehicle_id,
        selected_car_id,
        request.imported_at_ms,
        &manifest,
        fingerprint,
        &staged.geofences,
        &binding,
        &projection_state,
    ) {
        reconcile_failed_full_snapshot_candidate(store, &publication_gate, &mut staged, &error)?;
        return Err(error.into());
    }
    import_run.disarm();
    staged.keep_chunks();
    Ok(TeslaMateImportReport {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        snapshot_id,
        sequence,
        projection: staged.report,
        projected_rows: manifest.total_rows,
        skipped: false,
        cutover_unsettled: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_staged_history_successor(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    source_id: Uuid,
    vehicle_id: Uuid,
    selected_car_id: i64,
    binding: ProjectionBinding,
    publication_gate: PublicationGate,
    fingerprint: Sha256Digest,
    minimum_free_bytes: u64,
    mut staged: StagedProjectionPacks,
    mut run_guard: ImportGenerationGuard<'_>,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    let base_snapshot_id = store
        .v2_head(vehicle_id)?
        .map(|(snapshot_id, _, _)| snapshot_id)
        .ok_or(crate::db::StoreError::ImportDeltaRequiresBaseBinding)?;
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)?
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    let remaining_delta_packs = ProtocolLimits::default()
        .max_chunks
        .checked_sub(lineage.base.packs.len() + lineage.deltas.len())
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    if remaining_delta_packs == 0 {
        return Err(TeslaMateImportError::DirectDeltaBatchLimitExceeded {
            maximum: remaining_delta_packs,
        });
    }
    let selected_car = staged
        .selected_car
        .clone()
        .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?;
    let mut capture = staged
        .projection_state
        .take()
        .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?;
    capture.seal()?;
    staged.keep_chunks();
    discard_unpublished_chunks(store, &publication_gate, &staged.chunks)?;

    let mut next_ordinal = store.next_v2_pack_ordinal(base_snapshot_id)?;
    let mut prior_sequence = lineage.head_sequence;
    let mut parent_digest = lineage.head_digest;
    let mut deltas = Vec::new();
    let mut delta_packs = UnpublishedDirectDeltaPacks::new(store, &publication_gate);
    direct_delta_rows_from_capture(
        &mut capture,
        &binding,
        &selected_car,
        remaining_delta_packs,
        |batch| {
            let to_sequence =
                store.reserve_next_full_snapshot_sequence(&publication_gate, vehicle_id)?;
            if to_sequence <= prior_sequence {
                return Err(crate::db::StoreError::LineageCatalogConflict.into());
            }
            let delta = batch.into_delta(
                binding.clone(),
                SequenceRange {
                    from_exclusive: prior_sequence,
                    to_inclusive: to_sequence,
                },
                parent_digest,
            );
            let built = ProjectionPackWriter::new(store.packs_dir())
                .with_minimum_free_bytes(minimum_free_bytes)
                .write_delta(&ProjectionDeltaPackRequest {
                    pack_id: Uuid::new_v4(),
                    snapshot_id: base_snapshot_id,
                    ordinal: next_ordinal,
                    delta: &delta,
                })?;
            let chain_digest = canonical_delta_chain_digest(parent_digest, built.metadata.sha256);
            deltas.push(LineageDelta {
                from_sequence: prior_sequence,
                to_sequence,
                parent_chain_digest: parent_digest,
                chain_digest,
                pack_digest: built.metadata.sha256,
                pack: built.metadata.clone(),
            });
            delta_packs.push(built);
            prior_sequence = to_sequence;
            parent_digest = chain_digest;
            next_ordinal = next_ordinal
                .checked_add(1)
                .ok_or(crate::db::StoreError::PackOrdinalTooLarge)?;
            Ok(())
        },
    )?;
    let projection_state = capture.into_state();
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: prior_sequence,
        },
    )
    .map_err(crate::db::StoreError::Manifest)?;
    let run_id = run_guard
        .run_id
        .ok_or(TeslaMateImportError::ProjectionStateCaptureMissing)?;
    store.finalize_import_generation_delta_successors_with_projection_state(
        run_id,
        source_id,
        vehicle_id,
        selected_car_id,
        request.imported_at_ms,
        &deltas,
        cursor_key,
        &terminal_cursor,
        fingerprint,
        &staged.geofences,
        &projection_state,
    )?;
    delta_packs.published();
    drop(delta_packs);
    run_guard.disarm();
    Ok(TeslaMateImportReport {
        source_id,
        vehicle_id,
        snapshot_id: base_snapshot_id,
        sequence: prior_sequence,
        projection: staged.report,
        projected_rows: deltas.iter().try_fold(0_u64, |total, delta| {
            total
                .checked_add(delta.pack.row_count)
                .ok_or(TeslaMateImportError::DirectDeltaBatchOverflow)
        })?,
        skipped: false,
        cutover_unsettled: false,
    })
}

/// Publish an already-read source history. This seam makes the pack/identity
/// path deterministic and testable without a live PostgreSQL server.
fn teslamate_projection_inventory(
    projected: &TeslaMateProjection,
    selected_car_id: i64,
) -> Result<Vec<ProjectionTombstone>, TeslaMateImportError> {
    if selected_car_id <= 0 {
        return Err(TeslaMateImportError::InvalidSelectedCarId);
    }
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |entity: ProjectionDeltaEntity, id: i64| -> Result<(), TeslaMateImportError> {
        if id <= 0 || !seen.insert((entity, id)) {
            return Err(crate::db::StoreError::LineageCatalogConflict.into());
        }
        rows.push(ProjectionTombstone {
            entity,
            id,
            car_id: selected_car_id,
        });
        Ok(())
    };
    for row in &projected.snapshot.drives {
        push(ProjectionDeltaEntity::Drive, row.id)?;
    }
    for row in &projected.snapshot.positions {
        push(ProjectionDeltaEntity::Position, row.id)?;
    }
    for row in &projected.snapshot.charges {
        push(ProjectionDeltaEntity::Charge, row.id)?;
    }
    for row in &projected.snapshot.charge_samples {
        push(ProjectionDeltaEntity::ChargeSample, row.id)?;
    }
    for row in &projected.states {
        push(ProjectionDeltaEntity::State, row.id)?;
    }
    for row in &projected.updates {
        push(ProjectionDeltaEntity::Update, row.id)?;
    }
    Ok(rows)
}

fn teslamate_inventory_tombstones(
    previous: &[ProjectionTombstone],
    current: &[ProjectionTombstone],
) -> Result<Vec<ProjectionTombstone>, TeslaMateImportError> {
    let current_ids = current
        .iter()
        .map(|row| (row.entity, row.id))
        .collect::<HashSet<_>>();
    let mut tombstones = previous
        .iter()
        .filter(|row| !current_ids.contains(&(row.entity, row.id)))
        .cloned()
        .collect::<Vec<_>>();
    tombstones.sort_unstable_by_key(|row| {
        let entity = match row.entity {
            ProjectionDeltaEntity::Drive => 0_u8,
            ProjectionDeltaEntity::Position => 1,
            ProjectionDeltaEntity::Charge => 2,
            ProjectionDeltaEntity::ChargeSample => 3,
            ProjectionDeltaEntity::State => 4,
            ProjectionDeltaEntity::Update => 5,
            ProjectionDeltaEntity::Car
            | ProjectionDeltaEntity::CarSetting
            | ProjectionDeltaEntity::Geofence
            | ProjectionDeltaEntity::Address => 255,
        };
        (entity, row.id)
    });
    if tombstones.iter().any(|row| row.id <= 0 || row.car_id <= 0) {
        return Err(crate::db::StoreError::LineageCatalogConflict.into());
    }
    Ok(tombstones)
}

fn same_teslamate_inventory(left: &[ProjectionTombstone], right: &[ProjectionTombstone]) -> bool {
    left.iter()
        .map(|row| (row.entity, row.id, row.car_id))
        .collect::<HashSet<_>>()
        == right
            .iter()
            .map(|row| (row.entity, row.id, row.car_id))
            .collect()
}

pub fn publish_history(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    history: &TeslaMateHistory,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    let publication_gate = store.try_acquire_publication_gate()?;
    let selected_car_id = selected_car_id(request)?;
    let projected = project_vehicle(history, selected_car_id)?;
    let inventory_rows = teslamate_projection_inventory(&projected, selected_car_id)?;
    let fingerprint = source_history_fingerprint(history, selected_car_id)?;
    let car = projected
        .snapshot
        .cars
        .first()
        .expect("projection guarantees one selected car");
    let source = store.register_source(
        &SourceDescriptor::new("teslamate", request.source_key.clone()),
        request.imported_at_ms,
    )?;
    let source_vehicle_key = stable_vehicle_key_for_car(
        history
            .cars
            .iter()
            .find(|candidate| candidate.id == selected_car_id)
            .ok_or(TeslaMateImportError::SelectedCarMissing)?,
    )?;
    let deterministic_vehicle_id = Uuid::new_v5(&source.source_id, source_vehicle_key.as_bytes());
    let vehicle = store.register_vehicle_with_id(
        &VehicleDescriptor {
            source_id: source.source_id,
            source_vehicle_key,
            vin: nonblank(car.vin.as_deref()).map(ToOwned::to_owned),
            display_name: Some(car.name.clone()),
            tesla_eid: car.source_eid,
            tesla_vid: car.source_vid,
        },
        request.imported_at_ms,
        deterministic_vehicle_id,
    )?;
    let inventory = TeslaMateImportProjectionInventory {
        source_id: source.source_id,
        selected_car_id,
        rows: inventory_rows,
    };
    // The durable Hub vehicle key is a Tesla EID or VIN, not the mutable
    // TeslaMate primary key. Persist the imported car record so a later
    // typed successor derives the same selected car ID as the immutable base.
    store.persist_materialised_car_if_absent(vehicle.vehicle_id, car)?;
    if store.source_fingerprint_matches(vehicle.vehicle_id, fingerprint)? {
        if let Some((snapshot_id, head_sequence, _)) = store.v2_head(vehicle.vehicle_id)? {
            let published_inventory = store.teslamate_import_projection_inventory(
                vehicle.vehicle_id,
                source.source_id,
                selected_car_id,
            )?;
            if !same_teslamate_inventory(&published_inventory.rows, &inventory.rows) {
                return Err(crate::db::StoreError::LineageCatalogConflict.into());
            }
            return Ok(TeslaMateImportReport {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id,
                sequence: u64::try_from(head_sequence)
                    .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
                projection: projected.report,
                projected_rows: projected.report.logical_row_count().unwrap_or(0),
                skipped: true,
                cutover_unsettled: false,
            });
        }
        if let Some(current) =
            store.manifest_for_snapshot_fingerprint(vehicle.vehicle_id, fingerprint)?
        {
            return Ok(TeslaMateImportReport {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id: current.snapshot_id,
                sequence: current.head_sequence,
                projection: projected.report,
                projected_rows: current.total_rows,
                skipped: true,
                cutover_unsettled: false,
            });
        }
    }
    if store.vehicle_has_v2_base(vehicle.vehicle_id)? {
        return publish_history_as_import_delta(
            store,
            &publication_gate,
            cursor_key,
            &source,
            vehicle.vehicle_id,
            selected_car_id,
            &projected,
            fingerprint,
            &history.geofences,
            &inventory,
        );
    }
    let sequence =
        store.reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)?;
    let snapshot_id = Uuid::new_v4();
    let pack_id = Uuid::new_v4();
    let binding = ProjectionBinding {
        installation_id: store.installation_id()?,
        account_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: source.generation,
        selected_car_id,
    };
    let pack_request = ProjectionPackRequest {
        pack_id,
        snapshot_id,
        ordinal: 0,
        binding: binding.clone(),
        // A full snapshot has no delta base. Its equal base/head marker
        // identifies this complete replacement in the catalog.
        sequence: SequenceRange {
            from_exclusive: sequence,
            to_inclusive: sequence,
        },
        snapshot: &projected.snapshot,
    };
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot_with_states_and_updates(
            &pack_request,
            &projected.states,
            &projected.updates,
        )?;
    let manifest = pack_request.signed_manifest_with_states_and_updates(
        &pack,
        &projected.states,
        &projected.updates,
        cursor_key,
    )?;
    // `write_full_snapshot` leaves its verified pack in the candidate store
    // before this transaction. A commit makes it catalogued; a failed commit
    // leaves only an unreferenced, repairable orphan.
    store.finalize_teslamate_import_snapshot(
        &manifest,
        fingerprint,
        &history.geofences,
        &binding,
        &inventory,
    )?;

    Ok(TeslaMateImportReport {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        snapshot_id,
        sequence,
        projection: projected.report,
        projected_rows: manifest.total_rows,
        skipped: false,
        cutover_unsettled: false,
    })
}

/// After an immutable base exists, changed TeslaMate history is published as a
/// typed delta under the base snapshot identity — never as a second base.
fn publish_history_as_import_delta(
    store: &HubStore,
    publication_gate: &crate::db::PublicationGate,
    cursor_key: &CursorKey,
    source: &crate::db::SourceRecord,
    vehicle_id: Uuid,
    selected_car_id: i64,
    projected: &TeslaMateProjection,
    fingerprint: Sha256Digest,
    geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    inventory: &TeslaMateImportProjectionInventory,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    let (base_snapshot_id, from_sequence_i64, parent_digest) = store
        .v2_head(vehicle_id)?
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    let from_sequence = u64::try_from(from_sequence_i64)
        .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?;
    let prior_inventory = store.teslamate_import_projection_inventory(
        vehicle_id,
        source.source_id,
        selected_car_id,
    )?;
    let tombstones = teslamate_inventory_tombstones(&prior_inventory.rows, &inventory.rows)?;
    let to_sequence = store.reserve_next_full_snapshot_sequence(publication_gate, vehicle_id)?;
    if to_sequence <= from_sequence {
        return Err(crate::db::StoreError::LineageCatalogConflict.into());
    }
    let sequence = SequenceRange {
        from_exclusive: from_sequence,
        to_inclusive: to_sequence,
    };
    let binding = ProjectionBinding {
        installation_id: store.installation_id()?,
        account_id: source.source_id,
        vehicle_id,
        generation: source.generation,
        selected_car_id,
    };
    let delta = ProjectionDelta {
        binding: binding.clone(),
        sequence,
        parent_digest,
        cars: projected.snapshot.cars.clone(),
        car_settings: Vec::new(),
        drives: projected.snapshot.drives.clone(),
        positions: projected.snapshot.positions.clone(),
        charges: projected.snapshot.charges.clone(),
        charge_samples: projected.snapshot.charge_samples.clone(),
        states: projected.states.clone(),
        updates: projected.updates.clone(),
        // The persisted source-owned inventory makes removed source rows
        // explicit.  Missing rows are otherwise deliberately non-deleting in
        // the typed protocol, so this is the only safe place to form a
        // history-rewrite tombstone.
        tombstones,
    };
    if delta.is_empty() {
        // Empty successor is not a valid typed delta; treat as no-op skip.
        return Ok(TeslaMateImportReport {
            source_id: source.source_id,
            vehicle_id,
            snapshot_id: base_snapshot_id,
            sequence: from_sequence,
            projection: projected.report,
            projected_rows: projected.report.logical_row_count().unwrap_or(0),
            skipped: true,
            cutover_unsettled: false,
        });
    }
    let ordinal = store.next_v2_pack_ordinal(base_snapshot_id)?;
    let built =
        ProjectionPackWriter::new(store.packs_dir()).write_delta(&ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: base_snapshot_id,
            ordinal,
            delta: &delta,
        })?;
    let chain_digest = canonical_delta_chain_digest(parent_digest, built.metadata.sha256);
    let lineage_delta = LineageDelta {
        from_sequence,
        to_sequence,
        parent_chain_digest: parent_digest,
        chain_digest,
        pack_digest: built.metadata.sha256,
        pack: built.metadata,
    };
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: to_sequence,
        },
    )
    .map_err(crate::db::StoreError::Manifest)?;
    store.finalize_teslamate_import_delta_successor(
        vehicle_id,
        &lineage_delta,
        cursor_key,
        &terminal_cursor,
        fingerprint,
        geofences,
        inventory,
    )?;
    Ok(TeslaMateImportReport {
        source_id: source.source_id,
        vehicle_id,
        snapshot_id: base_snapshot_id,
        sequence: to_sequence,
        projection: projected.report,
        projected_rows: projected.report.logical_row_count().unwrap_or(0),
        skipped: false,
        cutover_unsettled: false,
    })
}

fn stable_vehicle_key_for_car(car: &TeslaMateCar) -> Result<String, TeslaMateImportError> {
    let eid = car.eid;
    if eid > 0 {
        return Ok(format!("eid:{eid}"));
    }
    if let Some(vin) = nonblank(car.vin.as_deref()) {
        return Ok(format!("vin:{vin}"));
    }
    Err(TeslaMateImportError::StableVehicleIdentityMissing)
}

fn validate_exported_vehicle_identity(
    registered_car: &TeslaMateCar,
    exported: &DirectUpdatesSourceV2_2,
) -> Result<(), TeslaMateImportError> {
    if i64::from(exported.car.id) != registered_car.id
        || exported.car.eid != registered_car.eid
        || Some(exported.car.vid) != registered_car.vid
        || exported.car.vin != registered_car.vin
    {
        return Err(TeslaMateImportError::SourceVehicleIdentityChangedDuringCapture);
    }
    Ok(())
}

fn nonblank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn source_history_fingerprint(
    history: &TeslaMateHistory,
    selected_car_id: i64,
) -> Result<Sha256Digest, TeslaMateImportError> {
    let canonical = serde_json::to_vec(&(selected_car_id, history))?;
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-hub/teslamate-source-history/v2-standalone-positions");
    digest.update(
        u64::try_from(canonical.len())
            .map_err(|_| TeslaMateImportError::SourceFingerprintTooLarge)?
            .to_be_bytes(),
    );
    digest.update(canonical);
    Ok(Sha256Digest::from_bytes(digest.finalize().into()))
}

/// Bind side-channel geofence metadata to the direct pack identity. The pack
/// stream itself is intentionally history-only, but the metadata is committed
/// in the same import transaction and must therefore participate in duplicate
/// suppression as well.
fn direct_snapshot_fingerprint(
    snapshot_fingerprint: &Sha256Digest,
    geofences: &[crate::teslamate_projection::TeslaMateGeofence],
) -> Result<Sha256Digest, TeslaMateImportError> {
    let canonical = serde_json::to_vec(geofences)?;
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-hub/teslamate-direct-snapshot-with-geofences/v1");
    digest.update(snapshot_fingerprint.as_bytes());
    digest.update(
        u64::try_from(canonical.len())
            .map_err(|_| TeslaMateImportError::SourceFingerprintTooLarge)?
            .to_be_bytes(),
    );
    digest.update(canonical);
    Ok(Sha256Digest::from_bytes(digest.finalize().into()))
}

// These are deliberately well below the wire ceiling. A source delta is
// built from canonical JSON held in a private state spool, not from an
// in-memory history, so conservative bounds guarantee a single unexpected
// source field cannot produce an oversized SQLite delta pack.
const DIRECT_DELTA_BATCH_ROWS: u64 = 50_000;
const DIRECT_DELTA_BATCH_PAYLOAD_BYTES: u64 =
    crate::teslamate_projection_state::DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES;
const DIRECT_DELTA_PAGE_ROWS: u32 = 10_000;

#[derive(Debug, Default)]
struct DirectDeltaRows {
    cars: Vec<ProjectionCar>,
    drives: Vec<ProjectionDrive>,
    positions: Vec<ProjectionPosition>,
    charges: Vec<ProjectionCharge>,
    charge_samples: Vec<ProjectionChargeSample>,
    states: Vec<ProjectionState>,
    updates: Vec<ProjectionUpdate>,
    tombstones: Vec<ProjectionTombstone>,
    row_count: u64,
    payload_bytes: u64,
}

impl DirectDeltaRows {
    fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    fn into_delta(
        self,
        binding: ProjectionBinding,
        sequence: SequenceRange,
        parent_digest: Sha256Digest,
    ) -> ProjectionDelta {
        ProjectionDelta {
            binding,
            sequence,
            parent_digest,
            cars: self.cars,
            car_settings: Vec::new(),
            drives: self.drives,
            positions: self.positions,
            charges: self.charges,
            charge_samples: self.charge_samples,
            states: self.states,
            updates: self.updates,
            tombstones: self.tombstones,
        }
    }

    fn next_would_exceed(&self, payload_bytes: u64) -> Result<bool, TeslaMateImportError> {
        let next_rows = self
            .row_count
            .checked_add(1)
            .ok_or(TeslaMateImportError::DirectDeltaBatchOverflow)?;
        let next_bytes = self
            .payload_bytes
            .checked_add(payload_bytes)
            .ok_or(TeslaMateImportError::DirectDeltaBatchOverflow)?;
        Ok(next_rows > DIRECT_DELTA_BATCH_ROWS || next_bytes > DIRECT_DELTA_BATCH_PAYLOAD_BYTES)
    }

    fn reserve(&mut self, payload_bytes: u64) -> Result<(), TeslaMateImportError> {
        self.row_count = self
            .row_count
            .checked_add(1)
            .ok_or(TeslaMateImportError::DirectDeltaBatchOverflow)?;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(payload_bytes)
            .ok_or(TeslaMateImportError::DirectDeltaBatchOverflow)?;
        Ok(())
    }
}

/// Visit bounded sparse row batches from a sealed current-run spool. The
/// visitor owns each decoded batch before the next page is read, keeping the
/// importer bounded even when every source row changed. It also enforces the
/// remaining protocol pack capacity before an unbounded number of candidate
/// pack files can be constructed.
fn direct_delta_rows_from_capture<F>(
    capture: &mut TeslaMateProjectionStateCapture,
    binding: &ProjectionBinding,
    metadata_only_car: &ProjectionCar,
    maximum_batches: usize,
    mut visit: F,
) -> Result<(), TeslaMateImportError>
where
    F: FnMut(DirectDeltaRows) -> Result<(), TeslaMateImportError>,
{
    if maximum_batches == 0 {
        return Err(TeslaMateImportError::DirectDeltaBatchLimitExceeded {
            maximum: maximum_batches,
        });
    }
    let mut emitted_batches = 0_usize;
    let mut current = DirectDeltaRows::default();
    let mut changed_after = None;
    loop {
        let page = capture.changed_page_with_payload_limit(
            changed_after,
            DIRECT_DELTA_PAGE_ROWS,
            DIRECT_DELTA_BATCH_PAYLOAD_BYTES,
        )?;
        for row in page.rows {
            let payload_bytes = u64::try_from(row.canonical_payload.len())
                .map_err(|_| TeslaMateImportError::DirectDeltaBatchOverflow)?;
            if payload_bytes > DIRECT_DELTA_BATCH_PAYLOAD_BYTES {
                return Err(TeslaMateImportError::DirectDeltaRowTooLarge { payload_bytes });
            }
            if current.next_would_exceed(payload_bytes)? && !current.is_empty() {
                emit_direct_delta_batch(
                    &mut emitted_batches,
                    maximum_batches,
                    current,
                    &mut visit,
                )?;
                current = DirectDeltaRows::default();
            }
            append_changed_direct_delta_row(&mut current, &row, binding)?;
        }
        match page.next_after {
            Some(next_after) => changed_after = Some(next_after),
            None => break,
        }
    }

    let mut tombstone_after = None;
    loop {
        let (rows, next_after) = capture.tombstone_page(tombstone_after, DIRECT_DELTA_PAGE_ROWS)?;
        for row in rows {
            let payload_bytes = u64::try_from(
                serde_json::to_vec(&row)
                    .map_err(TeslaMateImportError::SerializeDirectDeltaRow)?
                    .len(),
            )
            .map_err(|_| TeslaMateImportError::DirectDeltaBatchOverflow)?;
            if current.next_would_exceed(payload_bytes)? && !current.is_empty() {
                emit_direct_delta_batch(
                    &mut emitted_batches,
                    maximum_batches,
                    current,
                    &mut visit,
                )?;
                current = DirectDeltaRows::default();
            }
            if row.id <= 0
                || row.car_id != binding.selected_car_id
                || matches!(row.entity, ProjectionDeltaEntity::Car)
            {
                return Err(TeslaMateImportError::InvalidDirectDeltaTombstone);
            }
            current.reserve(payload_bytes)?;
            current.tombstones.push(row);
        }
        match next_after {
            Some(next_after) => tombstone_after = Some(next_after),
            None => break,
        }
    }

    if current.is_empty() && emitted_batches == 0 {
        let payload_bytes = u64::try_from(
            serde_json::to_vec(metadata_only_car)
                .map_err(TeslaMateImportError::SerializeDirectDeltaRow)?
                .len(),
        )
        .map_err(|_| TeslaMateImportError::DirectDeltaBatchOverflow)?;
        current.reserve(payload_bytes)?;
        current.cars.push(metadata_only_car.clone());
    }
    if !current.is_empty() {
        emit_direct_delta_batch(&mut emitted_batches, maximum_batches, current, &mut visit)?;
    }
    Ok(())
}

fn emit_direct_delta_batch<F>(
    emitted_batches: &mut usize,
    maximum_batches: usize,
    batch: DirectDeltaRows,
    visit: &mut F,
) -> Result<(), TeslaMateImportError>
where
    F: FnMut(DirectDeltaRows) -> Result<(), TeslaMateImportError>,
{
    if *emitted_batches >= maximum_batches {
        return Err(TeslaMateImportError::DirectDeltaBatchLimitExceeded {
            maximum: maximum_batches,
        });
    }
    visit(batch)?;
    *emitted_batches = emitted_batches
        .checked_add(1)
        .ok_or(TeslaMateImportError::DirectDeltaBatchOverflow)?;
    Ok(())
}

fn append_changed_direct_delta_row(
    batch: &mut DirectDeltaRows,
    row: &crate::teslamate_projection_state::TeslaMateProjectionStateChangedRow,
    binding: &ProjectionBinding,
) -> Result<(), TeslaMateImportError> {
    let payload_bytes = u64::try_from(row.canonical_payload.len())
        .map_err(|_| TeslaMateImportError::DirectDeltaBatchOverflow)?;
    match row.state.entity {
        TeslaMateProjectionStateEntity::Car => {
            let value: ProjectionCar = serde_json::from_slice(&row.canonical_payload)
                .map_err(TeslaMateImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.cars.push(value);
        }
        TeslaMateProjectionStateEntity::Drive => {
            let value: ProjectionDrive = serde_json::from_slice(&row.canonical_payload)
                .map_err(TeslaMateImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.drives.push(value);
        }
        TeslaMateProjectionStateEntity::Position => {
            let value: ProjectionPosition = serde_json::from_slice(&row.canonical_payload)
                .map_err(TeslaMateImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.positions.push(value);
        }
        TeslaMateProjectionStateEntity::Charge => {
            let value: ProjectionCharge = serde_json::from_slice(&row.canonical_payload)
                .map_err(TeslaMateImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.charges.push(value);
        }
        TeslaMateProjectionStateEntity::ChargeSample => {
            let value: ProjectionChargeSample = serde_json::from_slice(&row.canonical_payload)
                .map_err(TeslaMateImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, row.state.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.charge_samples.push(value);
        }
        TeslaMateProjectionStateEntity::State => {
            let value: ProjectionState = serde_json::from_slice(&row.canonical_payload)
                .map_err(TeslaMateImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.states.push(value);
        }
        TeslaMateProjectionStateEntity::Update => {
            let value: ProjectionUpdate = serde_json::from_slice(&row.canonical_payload)
                .map_err(TeslaMateImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.updates.push(value);
        }
    }
    Ok(())
}

fn validate_direct_delta_identity(
    id: i64,
    car_id: i64,
    row: &crate::teslamate_projection_state::TeslaMateProjectionStateChangedRow,
    binding: &ProjectionBinding,
) -> Result<(), TeslaMateImportError> {
    if id != row.state.id
        || car_id != row.state.car_id
        || car_id != binding.selected_car_id
        || id <= 0
    {
        return Err(TeslaMateImportError::InvalidDirectDeltaRow);
    }
    Ok(())
}

fn discard_unpublished_chunks(
    store: &HubStore,
    publication_gate: &PublicationGate,
    chunks: &[BuiltProjectionPack],
) -> Result<(), TeslaMateImportError> {
    for chunk in chunks {
        if !chunk.may_remove_unpublished_file() {
            continue;
        }
        store.remove_unretained_pack(publication_gate, chunk.metadata.sha256, &chunk.path)?;
    }
    Ok(())
}

fn reconcile_failed_full_snapshot_candidate(
    store: &HubStore,
    publication_gate: &PublicationGate,
    staged: &mut StagedProjectionPacks,
    error: &crate::db::StoreError,
) -> Result<(), TeslaMateImportError> {
    if !matches!(
        error,
        crate::db::StoreError::AmbiguousCatalogueCommit { .. }
    ) {
        // A proven-prior result has no catalogue ownership. Query again under
        // the still-held publication gate before unlinking so a coincident
        // content object used by current or retained lineage is preserved.
        discard_unpublished_chunks(store, publication_gate, &staged.chunks)?;
    }
    // Either cleanup completed/retained every object, or the commit outcome is
    // deliberately ambiguous and startup repair must preserve its evidence.
    staged.keep_chunks();
    Ok(())
}

/// An unchanged completed-history fingerprint deliberately skips lineage
/// publication, but the bounded source pass may still have observed a newer
/// open drive, charge, or state tail. Retain no candidate pack and atomically
/// make that reconciled tail durable before consuming the generation.
#[allow(clippy::too_many_arguments)]
fn promote_unchanged_direct_import(
    store: &HubStore,
    publication_gate: &PublicationGate,
    run_id: Uuid,
    source_id: Uuid,
    vehicle_id: Uuid,
    car_id: i64,
    updated_at_ms: i64,
    direct: &mut StagedProjectionPacks,
) -> Result<crate::db::OpenSessionSeedReport, TeslaMateImportError> {
    // A full direct capture cannot become a delta. Keep the candidate from
    // removing the files on drop, then remove only packs which no catalogue
    // entry references before the no-publication generation promotion.
    direct.keep_chunks();
    discard_unpublished_chunks(store, publication_gate, &direct.chunks)?;
    Ok(store.promote_import_generation(run_id, source_id, vehicle_id, car_id, updated_at_ms)?)
}

fn transport_row_count(chunks: &[BuiltProjectionPack]) -> Result<u64, ProjectionPackError> {
    chunks.iter().try_fold(0_u64, |total, chunk| {
        total
            .checked_add(chunk.metadata.row_count)
            .ok_or(ProjectionPackError::ManifestTotalsOverflow)
    })
}

#[derive(Debug, Error)]
pub enum TeslaMateImportError {
    #[error("TeslaMate selected car id must be positive")]
    InvalidSelectedCarId,
    #[error("TeslaMate selected car disappeared before publication")]
    SelectedCarMissing,
    #[error("direct TeslaMate capture did not retain its required projection state")]
    ProjectionStateCaptureMissing,
    #[error("direct TeslaMate capture did not retain the requested legacy token pair")]
    LegacyTokenCaptureMissing,
    #[error("existing TeslaMate direct-import base cannot be proved unchanged; rebase_required")]
    LegacyDirectImportRebaseRequired,
    #[error("direct TeslaMate delta batch accounting overflowed")]
    DirectDeltaBatchOverflow,
    #[error("direct TeslaMate delta requires more than {maximum} remaining protocol packs")]
    DirectDeltaBatchLimitExceeded { maximum: usize },
    #[error("direct TeslaMate delta row is too large ({payload_bytes} bytes)")]
    DirectDeltaRowTooLarge { payload_bytes: u64 },
    #[error("cannot decode canonical direct TeslaMate delta row: {0}")]
    DecodeDirectDeltaRow(serde_json::Error),
    #[error("cannot encode direct TeslaMate delta row: {0}")]
    SerializeDirectDeltaRow(serde_json::Error),
    #[error("direct TeslaMate delta row does not match its selected-car state identity")]
    InvalidDirectDeltaRow,
    #[error("direct TeslaMate delta tombstone is invalid")]
    InvalidDirectDeltaTombstone,
    #[error("TeslaMate source snapshot is too large to fingerprint")]
    SourceFingerprintTooLarge,
    #[error("cannot serialize TeslaMate source snapshot fingerprint: {0}")]
    SourceFingerprint(#[from] serde_json::Error),
    #[error("TeslaMate selected car has neither a VIN nor a valid EID")]
    StableVehicleIdentityMissing,
    #[error("TeslaMate selected-car VIN/EID/VID changed before the exported snapshot capture")]
    SourceVehicleIdentityChangedDuringCapture,
    #[error("TeslaMate cutover snapshots belong to different cars")]
    CutoverCarMismatch,
    #[error(
        "legacy TeslaMate import committed for vehicle {vehicle_id} snapshot {legacy_snapshot_id}, but schema-2.2 publication failed; retry the same selected-car import: {source}"
    )]
    Schema22PostCommit {
        vehicle_id: Uuid,
        legacy_snapshot_id: Uuid,
        #[source]
        source: UpdatesDeliveryError,
    },
    #[error(transparent)]
    Reader(#[from] crate::teslamate_reader::TeslaMateReaderError),
    #[error(transparent)]
    Direct(#[from] crate::teslamate_direct::TeslaMateDirectError),
    #[error(transparent)]
    Stage(#[from] crate::teslamate_stage::TeslaMateStageError),
    #[error(transparent)]
    Projection(#[from] crate::teslamate_projection::TeslaMateProjectionError),
    #[error(transparent)]
    Store(#[from] crate::db::StoreError),
    #[error(transparent)]
    Pack(#[from] crate::hub_pack::ProjectionPackError),
    #[error(transparent)]
    Fragments(#[from] crate::teslamate_fragments::TeslaMateFragmentError),
    #[error(transparent)]
    ProjectionState(#[from] TeslaMateProjectionStateError),
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs, fs::File, path::Path};

    use super::*;
    use crate::{
        credentials::TeslaMatePostgresPassword,
        db::{HubStore, ObservationInput, SourceDescriptor, StoreError, VehicleDescriptor},
        hub_pack::ProjectionPackOwnership,
        protocol::{CursorKey, ProtocolLimits},
        teslamate::ReadOnlySource,
        teslamate_fragments::TeslaMateFragmentLimits,
        teslamate_projection::{
            TeslaMateCar, TeslaMateGeofence, TeslaMateHistory, TeslaMatePosition, TeslaMateUpdate,
        },
        teslamate_projection_state::TeslaMateProjectionState,
        teslamate_stage::{TeslaMateStage, TeslaMateStageLimits, TeslaMateStageTable},
    };

    #[derive(Debug)]
    struct EmptyPriorProjectionState;

    impl crate::teslamate_projection_state::PriorProjectionStateLookup for EmptyPriorProjectionState {
        fn digest(
            &mut self,
            _entity: TeslaMateProjectionStateEntity,
            _id: i64,
        ) -> Result<Option<Sha256Digest>, Box<dyn Error + Send + Sync>> {
            Ok(None)
        }

        fn page_after(
            &mut self,
            _after: Option<crate::teslamate_projection_state::TeslaMateProjectionStateCursor>,
            _limit: u32,
        ) -> Result<
            crate::teslamate_projection_state::TeslaMateProjectionStateDigestPage,
            Box<dyn Error + Send + Sync>,
        > {
            Ok(
                crate::teslamate_projection_state::TeslaMateProjectionStateDigestPage {
                    rows: Vec::new(),
                    next_after: None,
                },
            )
        }
    }

    fn history() -> TeslaMateHistory {
        TeslaMateHistory {
            cars: vec![TeslaMateCar {
                id: 1,
                eid: 88,
                vid: Some(99),
                vin: Some("5YJTESTVIN1234567".into()),
                name: Some("Road car".into()),
                model: Some("Model 3".into()),
                trim_badging: Some("74d".into()),
                marketing_name: Some("LR AWD".into()),
                exterior_color: Some("Pearl White".into()),
                wheel_type: Some("Apollo".into()),
                spoiler_type: Some("None".into()),
                efficiency_wh_per_km: Some(0.145),
                settings: Default::default(),
            }],
            drives: vec![],
            positions: vec![],
            charging_processes: vec![],
            charges: vec![],
            addresses: vec![],
            geofences: vec![],
            states: vec![],
            updates: vec![],
        }
    }

    fn identity_registry_image(store: &HubStore) -> [String; 4] {
        let connection = store.open().expect("identity registry");
        [
            connection
                .query_row(
                    "SELECT COALESCE(json_group_array(json_array(
                        source_id, source_kind, generation, created_at_ms)), '[]')
                       FROM (SELECT * FROM sources ORDER BY source_id)",
                    [],
                    |row| row.get(0),
                )
                .expect("sources image"),
            connection
                .query_row(
                    "SELECT COALESCE(json_group_array(json_array(
                        source_id, source_kind, source_key)), '[]')
                       FROM (SELECT * FROM source_identities ORDER BY source_id)",
                    [],
                    |row| row.get(0),
                )
                .expect("source identities image"),
            connection
                .query_row(
                    "SELECT COALESCE(json_group_array(json_array(
                        vehicle_id, source_id, source_vehicle_key, vin, display_name,
                        created_at_ms, last_seen_at_ms)), '[]')
                       FROM (SELECT * FROM vehicles ORDER BY vehicle_id)",
                    [],
                    |row| row.get(0),
                )
                .expect("vehicles image"),
            connection
                .query_row(
                    "SELECT COALESCE(json_group_array(json_array(
                        alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)), '[]')
                       FROM (SELECT * FROM vehicle_identity_aliases
                             ORDER BY alias_kind, alias_value)",
                    [],
                    |row| row.get(0),
                )
                .expect("aliases image"),
        ]
    }

    fn teslamate_identity_hint(source_id: Uuid, source_vehicle_key: &str) -> VehicleDescriptor {
        VehicleDescriptor {
            source_id,
            source_vehicle_key: source_vehicle_key.into(),
            vin: Some("STABLEVIN".into()),
            display_name: Some("stable".into()),
            tesla_eid: Some(88),
            tesla_vid: Some(99),
        }
    }

    fn update_rows_from_pack(
        inspection_directory: &Path,
        pack_path: &Path,
        name: &str,
    ) -> Vec<(i64, i64, i64, i64, String)> {
        let inspection_path = inspection_directory.join(format!("{name}.sqlite"));
        fs::write(
            &inspection_path,
            zstd::stream::decode_all(File::open(pack_path).expect("open pack"))
                .expect("decode pack"),
        )
        .expect("write inspection database");
        let connection = rusqlite::Connection::open(inspection_path).expect("open inspection");
        connection
            .prepare(
                "SELECT id, car_id, start_date_ms, end_date_ms, version
                 FROM updates ORDER BY id",
            )
            .expect("schema-2.1 updates table exists")
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .expect("query updates")
            .collect::<Result<Vec<_>, _>>()
            .expect("read update rows")
    }

    #[test]
    fn direct_successor_state_emits_typed_sparse_car_delta() {
        let temporary = tempfile::tempdir().unwrap();
        let mut projected = project_vehicle(&history(), 1).unwrap();
        let car = projected.snapshot.cars.remove(0);
        let state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 64 * 1024,
                max_changed_payload_bytes: 64 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .unwrap();
        let mut capture = TeslaMateProjectionStateCapture::for_successor(
            state,
            Box::new(EmptyPriorProjectionState),
        );
        capture.record_car(&car).unwrap();
        capture.seal().unwrap();
        let binding = ProjectionBinding {
            installation_id: Uuid::from_u128(1),
            account_id: Uuid::from_u128(2),
            vehicle_id: Uuid::from_u128(3),
            generation: 0,
            selected_car_id: 1,
        };
        let mut batches = Vec::new();
        direct_delta_rows_from_capture(&mut capture, &binding, &car, 1, |batch| {
            batches.push(batch);
            Ok(())
        })
        .unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].cars, vec![car]);
        assert!(batches[0].drives.is_empty());
        assert!(batches[0].tombstones.is_empty());
    }

    #[test]
    fn direct_successor_state_emits_typed_update_delta() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut source = history();
        source.updates.push(TeslaMateUpdate {
            id: 71,
            car_id: 1,
            start_date_ms: 1_700_000_000_000,
            end_date_ms: Some(1_700_000_060_000),
            version: Some("2026.44.1".into()),
        });
        let projected = project_vehicle(&source, 1).expect("project source update");
        let state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 64 * 1024,
                max_changed_payload_bytes: 64 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        let mut capture = TeslaMateProjectionStateCapture::for_successor(
            state,
            Box::new(EmptyPriorProjectionState),
        );
        record_projected_direct_state(&mut capture, &projected, 1);
        capture.seal().expect("seal update capture");
        let binding = ProjectionBinding {
            installation_id: Uuid::from_u128(11),
            account_id: Uuid::from_u128(12),
            vehicle_id: Uuid::from_u128(13),
            generation: 1,
            selected_car_id: 1,
        };
        let car = projected.snapshot.cars[0].clone();
        let mut batches = Vec::new();
        direct_delta_rows_from_capture(&mut capture, &binding, &car, 1, |batch| {
            batches.push(batch);
            Ok(())
        })
        .expect("emit sparse update delta");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].updates, projected.updates);
        assert!(batches[0].tombstones.is_empty());
    }

    #[test]
    fn direct_delta_batch_limit_refuses_another_batch_before_visiting_it() {
        let mut visited = 0_usize;
        let mut emitted = 0_usize;
        emit_direct_delta_batch(&mut emitted, 1, DirectDeltaRows::default(), &mut |_| {
            visited += 1;
            Ok(())
        })
        .expect("the single available batch is visited");
        let error =
            emit_direct_delta_batch(&mut emitted, 1, DirectDeltaRows::default(), &mut |_| {
                visited += 1;
                Ok(())
            })
            .expect_err("a protocol-full import must stop before another batch is written");
        assert!(matches!(
            error,
            TeslaMateImportError::DirectDeltaBatchLimitExceeded { maximum: 1 }
        ));
        assert_eq!(visited, 1);
    }

    #[test]
    fn discard_unpublished_chunks_keeps_catalogued_and_reused_content() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let projected = project_vehicle(&history(), 1).expect("project source history");
        let binding = ProjectionBinding {
            installation_id: Uuid::from_u128(1),
            account_id: Uuid::from_u128(2),
            vehicle_id: Uuid::from_u128(3),
            generation: 1,
            selected_car_id: 1,
        };
        let request = ProjectionPackRequest {
            pack_id: Uuid::from_u128(4),
            snapshot_id: Uuid::from_u128(5),
            ordinal: 0,
            binding,
            sequence: SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            snapshot: &projected.snapshot,
        };
        let writer = ProjectionPackWriter::new(store.packs_dir());
        let created = writer
            .write_full_snapshot(&request)
            .expect("first producer writes pack");
        assert_eq!(created.ownership(), ProjectionPackOwnership::Created);
        let manifest = request
            .signed_manifest(&created, &CursorKey::from_bytes([46; 32]))
            .expect("sign first producer pack");
        store
            .publish_manifest(&manifest)
            .expect("catalogue first producer pack");
        let path = created.path.clone();
        let publication_gate = store.try_acquire_publication_gate().expect("gate");

        discard_unpublished_chunks(&store, &publication_gate, std::slice::from_ref(&created))
            .expect("catalogue check protects a created, now-published pack");
        assert!(path.is_file(), "catalogued created pack remains present");

        let reused = writer
            .write_full_snapshot(&request)
            .expect("second producer reuses catalogued content");
        assert_eq!(reused.ownership(), ProjectionPackOwnership::ReusedExisting);
        discard_unpublished_chunks(&store, &publication_gate, std::slice::from_ref(&reused))
            .expect("reused descriptors are never unlinked by importer cleanup");
        assert!(path.is_file(), "catalogued reused pack remains present");
        store
            .catalogue_check()
            .expect("catalogue remains valid after cleanup checks");

        let fresh_request = ProjectionPackRequest {
            pack_id: Uuid::from_u128(6),
            snapshot_id: Uuid::from_u128(7),
            ordinal: 0,
            binding: request.binding.clone(),
            sequence: request.sequence,
            snapshot: &projected.snapshot,
        };
        let fresh = writer
            .write_full_snapshot(&fresh_request)
            .expect("fresh unpublished producer pack");
        assert_eq!(fresh.ownership(), ProjectionPackOwnership::Created);
        let fresh_path = fresh.path.clone();
        discard_unpublished_chunks(&store, &publication_gate, std::slice::from_ref(&fresh))
            .expect("created unpublished pack is discarded");
        assert!(
            !fresh_path.exists(),
            "importer cleanup still removes a fresh unreferenced candidate"
        );
    }

    #[test]
    fn proven_prior_full_snapshot_failure_cleans_candidate_under_gate() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let projected = project_vehicle(&history(), 1).expect("project source history");
        let request = ProjectionPackRequest {
            pack_id: Uuid::from_u128(81),
            snapshot_id: Uuid::from_u128(82),
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id: Uuid::from_u128(83),
                account_id: Uuid::from_u128(84),
                vehicle_id: Uuid::from_u128(85),
                generation: 1,
                selected_car_id: 1,
            },
            sequence: SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            snapshot: &projected.snapshot,
        };
        let built = ProjectionPackWriter::new(store.packs_dir())
            .write_full_snapshot(&request)
            .expect("fresh candidate");
        let path = built.path.clone();
        let mut staged = StagedProjectionPacks::new(
            vec![built],
            ProjectionReport::default(),
            Sha256Digest::of_bytes(b"prior-candidate"),
            Vec::new(),
        );
        let gate = store.try_acquire_publication_gate().expect("gate");
        let error = crate::db::StoreError::CatalogueDurability(std::io::Error::other(
            "test pre-commit result",
        ));
        reconcile_failed_full_snapshot_candidate(&store, &gate, &mut staged, &error)
            .expect("proven prior cleanup");
        assert!(!path.exists(), "proven-prior candidate is removed");
        drop(staged);
        assert!(!path.exists(), "drop cannot recreate the candidate");
    }

    fn open_live_tail(
        position_ids: &[i64],
        charge_sample_ids: &[i64],
        state_id: i64,
    ) -> TeslaMateOpenSession {
        let drive = serde_json::from_value(serde_json::json!({
            "id": 70,
            "car_id": 1,
            "start_date_ms": 1_700_000_000_000_i64,
        }))
        .expect("open drive");
        let charge = serde_json::from_value(serde_json::json!({
            "id": 80,
            "car_id": 1,
            "start_date_ms": 1_700_000_000_000_i64,
        }))
        .expect("open charge");
        let drive_positions = position_ids
            .iter()
            .map(|id| {
                serde_json::from_value(serde_json::json!({
                    "id": id,
                    "car_id": 1,
                    "drive_id": 70,
                    "date_ms": id * 1_000,
                    "latitude": 51.0,
                    "longitude": -0.1,
                }))
                .expect("open drive position")
            })
            .collect();
        let charge_samples = charge_sample_ids
            .iter()
            .map(|id| {
                serde_json::from_value(serde_json::json!({
                    "id": id,
                    "charging_process_id": 80,
                    "date_ms": id * 1_000,
                }))
                .expect("open charge sample")
            })
            .collect();
        let mut session = TeslaMateOpenSession {
            car_id: 1,
            drive: Some(drive),
            drive_positions,
            charge: Some(charge),
            charge_samples,
            state: Some(
                serde_json::from_value(serde_json::json!({
                    "id": state_id,
                    "car_id": 1,
                    "state": "online",
                    "start_date_ms": state_id * 1_000,
                }))
                .expect("open state"),
            ),
            ..Default::default()
        };
        session.watermarks = observed_open_watermarks(&session);
        session
    }

    #[test]
    fn unchanged_direct_fingerprint_promotes_reconciled_live_tail_and_cleans_candidate_across_restart()
     {
        const BASE_TIME_MS: i64 = 1_700_000_000_000;
        const TAIL_TIME_MS: i64 = BASE_TIME_MS + 1_000;

        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let request = TeslaMateImportRequest {
            source_key: "unchanged-direct-live-tail".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: BASE_TIME_MS,
        };
        let completed_history = history();
        let cursor_key = CursorKey::from_bytes([63; 32]);
        let base = publish_history(&store, &cursor_key, &request, &completed_history)
            .expect("publish completed-history base");
        let source = store
            .register_source(
                &SourceDescriptor::new("teslamate", request.source_key.clone()),
                BASE_TIME_MS,
            )
            .expect("registered source");
        assert_eq!(source.source_id, base.source_id);
        let base_manifest = store
            .manifest_for_vehicle(base.vehicle_id)
            .expect("base manifest lookup")
            .expect("base manifest");
        let direct_fingerprint = direct_snapshot_fingerprint(
            &source_history_fingerprint(&completed_history, 1)
                .expect("completed-history fingerprint"),
            &completed_history.geofences,
        )
        .expect("direct completed-history fingerprint");
        // A direct base stores the direct-source fingerprint; seed that exact
        // persisted shape while keeping completed history unchanged below.
        store
            .record_snapshot_fingerprint(&base_manifest, direct_fingerprint)
            .expect("record direct base fingerprint");

        let first_tail = open_live_tail(&[101], &[201], 301);
        store
            .seed_imported_open_session(
                source.source_id,
                base.vehicle_id,
                1,
                &first_tail,
                BASE_TIME_MS,
            )
            .expect("seed first open tail");
        let run_id = store
            .begin_import_generation(source.source_id, base.vehicle_id, 1, TAIL_TIME_MS)
            .expect("begin unchanged generation");
        let second_tail = open_live_tail(&[102], &[202], 302);
        let reconciled = reconcile_open_session_cutover(&first_tail, &second_tail)
            .expect("reconcile bounded tails")
            .session;
        store
            .stage_import_generation_session(run_id, &reconciled)
            .expect("stage reconciled tail");

        let binding = ProjectionBinding {
            installation_id: store.installation_id().expect("installation id"),
            account_id: source.source_id,
            vehicle_id: base.vehicle_id,
            generation: source.generation,
            selected_car_id: 1,
        };
        let projection = project_vehicle(&completed_history, 1).expect("completed projection");
        let candidate = ProjectionPackWriter::new(store.packs_dir())
            .write_full_snapshot(&ProjectionPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: Uuid::new_v4(),
                ordinal: 0,
                binding,
                sequence: SequenceRange {
                    from_exclusive: base.sequence,
                    to_inclusive: base.sequence,
                },
                snapshot: &projection.snapshot,
            })
            .expect("write unreferenced direct candidate");
        let candidate_path = candidate.path.clone();
        let mut direct = StagedProjectionPacks::new(
            vec![candidate],
            projection.report,
            direct_fingerprint,
            Vec::new(),
        );
        assert!(
            store
                .source_fingerprint_matches(base.vehicle_id, direct_fingerprint)
                .expect("unchanged direct fingerprint matches")
        );

        promote_unchanged_direct_import(
            &store,
            &store.try_acquire_publication_gate().expect("gate"),
            run_id,
            source.source_id,
            base.vehicle_id,
            1,
            TAIL_TIME_MS,
            &mut direct,
        )
        .expect("promote unchanged direct tail without publication");
        drop(direct);
        assert!(
            !candidate_path.exists(),
            "unchanged direct capture removes its unreferenced candidate pack"
        );
        let lineage = store
            .lineage_manifest_for_vehicle(base.vehicle_id)
            .expect("unchanged lineage lookup")
            .expect("immutable base remains");
        assert_eq!(lineage.base.snapshot_id, base.snapshot_id);
        assert!(
            lineage.deltas.is_empty(),
            "unchanged history publishes no delta"
        );
        assert_eq!(
            store.v2_head(base.vehicle_id).expect("v2 head"),
            Some((
                base.snapshot_id,
                i64::try_from(base.sequence).expect("base sequence fits i64"),
                lineage.head_digest,
            )),
            "unchanged history publishes no replacement base"
        );
        assert_eq!(
            store
                .load_imported_open_session(source.source_id, base.vehicle_id)
                .expect("load promoted live tail"),
            Some(reconciled.clone()),
            "the changed drive, charge, and state tail is atomically promoted"
        );
        let generation_count: i64 = store
            .open()
            .expect("open catalogue")
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("generation count");
        assert_eq!(generation_count, 0, "successful promotion consumes staging");

        drop(store);
        let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
        assert_eq!(
            restarted
                .load_imported_open_session(source.source_id, base.vehicle_id)
                .expect("load restarted live tail"),
            Some(reconciled.clone()),
            "the no-publication tail survives restart"
        );
        let repeat_run = restarted
            .begin_import_generation(source.source_id, base.vehicle_id, 1, TAIL_TIME_MS + 1)
            .expect("begin repeat unchanged generation");
        restarted
            .stage_import_generation_session(repeat_run, &reconciled)
            .expect("stage idempotent tail");
        let mut no_candidate = StagedProjectionPacks::new(
            Vec::new(),
            ProjectionReport::default(),
            direct_fingerprint,
            Vec::new(),
        );
        promote_unchanged_direct_import(
            &restarted,
            &restarted.try_acquire_publication_gate().expect("gate"),
            repeat_run,
            source.source_id,
            base.vehicle_id,
            1,
            TAIL_TIME_MS,
            &mut no_candidate,
        )
        .expect("repeat unchanged tail promotion");
        assert_eq!(
            restarted
                .load_imported_open_session(source.source_id, base.vehicle_id)
                .expect("load idempotent tail"),
            Some(reconciled),
            "repeating the same unchanged tail does not duplicate or lose it"
        );
        let restarted_lineage = restarted
            .lineage_manifest_for_vehicle(base.vehicle_id)
            .expect("restarted lineage lookup")
            .expect("restarted immutable base");
        assert_eq!(restarted_lineage.base.snapshot_id, base.snapshot_id);
        assert!(
            restarted_lineage.deltas.is_empty(),
            "idempotent unchanged tail promotion publishes no delta"
        );
    }

    fn completed_drive(id: i64) -> crate::teslamate_projection::TeslaMateDrive {
        crate::teslamate_projection::TeslaMateDrive {
            id,
            car_id: 1,
            start_date_ms: 2_000,
            end_date_ms: Some(3_000),
            outside_temp_avg: None,
            speed_max: Some(50),
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_rated_range_km: Some(300.0),
            end_rated_range_km: Some(280.0),
            start_km: Some(10.0),
            end_km: Some(20.0),
            distance_km: Some(10.0),
            duration_min: Some(1),
            start_address_id: None,
            end_address_id: None,
            start_geofence_id: None,
            end_geofence_id: None,
            start_position_id: None,
            end_position_id: None,
            ascent: None,
            descent: None,
            inside_temp_avg: None,
        }
    }

    fn direct_state_test_limits() -> TeslaMateReadLimits {
        TeslaMateReadLimits {
            maximum_rows: 16,
            maximum_stage_bytes: 64 * 1024,
            minimum_free_bytes: 0,
            ..TeslaMateReadLimits::default()
        }
    }

    fn record_projected_direct_state(
        capture: &mut TeslaMateProjectionStateCapture,
        projected: &TeslaMateProjection,
        selected_car_id: i64,
    ) {
        for car in &projected.snapshot.cars {
            capture.record_car(car).expect("capture car");
        }
        for drive in &projected.snapshot.drives {
            capture.record_drive(drive).expect("capture drive");
        }
        for position in &projected.snapshot.positions {
            capture.record_position(position).expect("capture position");
        }
        for charge in &projected.snapshot.charges {
            capture.record_charge(charge).expect("capture charge");
        }
        for sample in &projected.snapshot.charge_samples {
            capture
                .record_charge_sample(selected_car_id, sample)
                .expect("capture charge sample");
        }
        for state in &projected.states {
            capture.record_state(state).expect("capture state");
        }
        for update in &projected.updates {
            capture.record_update(update).expect("capture update");
        }
    }

    fn begin_direct_state_generation(
        store: &HubStore,
        binding: &ProjectionBinding,
        created_at_ms: i64,
    ) -> Uuid {
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                binding.vehicle_id,
                binding.selected_car_id,
                created_at_ms,
            )
            .expect("begin direct-state generation");
        store
            .stage_import_generation_session(
                run_id,
                &TeslaMateOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("stage direct-state session");
        run_id
    }

    #[test]
    fn stateful_direct_capture_publishes_base_skips_unchanged_then_publishes_changed_successor_across_restart()
     {
        const BASE_TIME_MS: i64 = 1_700_000_000_000;
        const UNCHANGED_TIME_MS: i64 = BASE_TIME_MS + 1;
        const CHANGED_TIME_MS: i64 = BASE_TIME_MS + 2;

        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let source = store
            .register_source(
                &SourceDescriptor::new("teslamate", "direct-stateful-regression"),
                BASE_TIME_MS,
            )
            .expect("fixture source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "direct-stateful-regression-car"),
                BASE_TIME_MS,
            )
            .expect("fixture vehicle");
        let binding = ProjectionBinding {
            installation_id: store.installation_id().expect("installation id"),
            account_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: source.generation,
            selected_car_id: 1,
        };
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("direct-state test publication gate");
        let cursor_key = CursorKey::from_bytes([37; 32]);
        let base_history = history();
        let base_projection =
            project_vehicle(&base_history, binding.selected_car_id).expect("base projection");
        let base_run = begin_direct_state_generation(&store, &binding, BASE_TIME_MS);
        let mut base_capture = direct_projection_state_capture(
            &store,
            &publication_gate,
            base_run,
            binding.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
            false,
            direct_state_test_limits(),
        )
        .expect("initial direct state capture");
        record_projected_direct_state(&mut base_capture, &base_projection, binding.selected_car_id);
        assert_eq!(
            base_capture.mode(),
            crate::teslamate_projection_state::TeslaMateProjectionStateCaptureMode::InitialBase
        );
        base_capture.seal().expect("seal base state capture");
        let base_state = base_capture.into_state();
        let base_sequence = store
            .reserve_next_full_snapshot_sequence(&publication_gate, binding.vehicle_id)
            .expect("reserve base sequence");
        let base_snapshot_id = Uuid::new_v4();
        let base_request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: base_snapshot_id,
            ordinal: 0,
            binding: binding.clone(),
            sequence: SequenceRange {
                from_exclusive: base_sequence,
                to_inclusive: base_sequence,
            },
            snapshot: &base_projection.snapshot,
        };
        let base_pack = ProjectionPackWriter::new(store.packs_dir())
            .write_full_snapshot_with_states_and_updates(
                &base_request,
                &base_projection.states,
                &[],
            )
            .expect("write direct base pack");
        assert_eq!(
            base_pack.ownership(),
            ProjectionPackOwnership::Created,
            "the initial direct-import candidate owns its fresh pack until finalization"
        );
        let base_manifest = base_request
            .signed_manifest_with_states_and_updates(
                &base_pack,
                &base_projection.states,
                &[],
                &cursor_key,
            )
            .expect("sign direct base manifest");
        let base_fingerprint = direct_snapshot_fingerprint(
            &source_history_fingerprint(&base_history, binding.selected_car_id)
                .expect("base source fingerprint"),
            &base_history.geofences,
        )
        .expect("base direct fingerprint");
        let base_pack_path = base_pack.path.clone();
        let base_candidate = StagedProjectionPacks::new(
            vec![base_pack],
            ProjectionReport::default(),
            base_fingerprint,
            Vec::new(),
        );
        // Both injected failures happen after the SQLite transaction owns the
        // candidate. The durable receipt must reconcile the commit to success,
        // and dropping a still-armed candidate must never unlink its pack.
        let _commit_fault = crate::durability_fault::inject(
            crate::durability_fault::DurabilityFaultPoint::CatalogueAfterCommit,
        );
        store.inject_projection_state_detach_fault();
        store
            .finalize_import_generation_with_projection_state(
                base_run,
                binding.account_id,
                binding.vehicle_id,
                binding.selected_car_id,
                BASE_TIME_MS,
                &base_manifest,
                base_fingerprint,
                &base_history.geofences,
                &binding,
                &base_state,
            )
            .expect("committed direct base survives post-commit detach failure");
        drop(base_candidate);
        assert!(
            base_pack_path.is_file(),
            "candidate cleanup cannot delete the newly catalogued created pack"
        );
        store
            .catalogue_check()
            .expect("committed direct base remains readable after candidate cleanup");

        let unchanged_run = begin_direct_state_generation(&store, &binding, UNCHANGED_TIME_MS);
        let mut unchanged_capture = direct_projection_state_capture(
            &store,
            &publication_gate,
            unchanged_run,
            binding.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
            true,
            direct_state_test_limits(),
        )
        .expect("durable prior state enables unchanged direct capture");
        record_projected_direct_state(
            &mut unchanged_capture,
            &base_projection,
            binding.selected_car_id,
        );
        assert_eq!(
            unchanged_capture.mode(),
            crate::teslamate_projection_state::TeslaMateProjectionStateCaptureMode::Successor
        );
        let unchanged_stats = unchanged_capture
            .seal()
            .expect("seal unchanged direct capture");
        assert_eq!(unchanged_stats.changed_row_count, 0);
        // Production guards on the persisted source fingerprint before it
        // invokes `direct_delta_rows_from_capture`: an empty capture would
        // otherwise intentionally emit a metadata-only car delta.
        assert!(
            store
                .source_fingerprint_matches(binding.vehicle_id, base_fingerprint)
                .expect("unchanged fingerprint lookup"),
            "the unchanged direct run must take the fingerprint guard"
        );
        store
            .abort_import_generation(unchanged_run)
            .expect("fingerprint-guarded generation is discarded");
        let unchanged_lineage = store
            .lineage_manifest_for_vehicle(binding.vehicle_id)
            .expect("unchanged lineage lookup")
            .expect("base lineage remains published");
        assert!(
            unchanged_lineage.deltas.is_empty(),
            "the fingerprint guard must not publish a successor"
        );
        drop(unchanged_capture);

        let mut changed_history = history();
        changed_history.drives.push(completed_drive(99));
        let changed_projection =
            project_vehicle(&changed_history, binding.selected_car_id).expect("changed projection");
        let changed_fingerprint = direct_snapshot_fingerprint(
            &source_history_fingerprint(&changed_history, binding.selected_car_id)
                .expect("changed source fingerprint"),
            &changed_history.geofences,
        )
        .expect("changed direct fingerprint");
        assert_ne!(changed_fingerprint, base_fingerprint);
        assert!(
            !store
                .source_fingerprint_matches(binding.vehicle_id, changed_fingerprint)
                .expect("changed fingerprint lookup"),
            "changed source history must not take the unchanged guard"
        );
        let successor_run = begin_direct_state_generation(&store, &binding, CHANGED_TIME_MS);
        let mut changed_capture = direct_projection_state_capture(
            &store,
            &publication_gate,
            successor_run,
            binding.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
            true,
            direct_state_test_limits(),
        )
        .expect("durable prior state enables changed direct capture");
        record_projected_direct_state(
            &mut changed_capture,
            &changed_projection,
            binding.selected_car_id,
        );
        let changed_stats = changed_capture.seal().expect("seal changed direct capture");
        assert_eq!(changed_stats.changed_row_count, 1);

        let base_lineage = store
            .lineage_manifest_for_vehicle(binding.vehicle_id)
            .expect("base lineage lookup")
            .expect("base lineage");
        let mut deltas = Vec::new();
        let mut next_ordinal = store
            .next_v2_pack_ordinal(base_snapshot_id)
            .expect("next successor ordinal");
        let mut parent_digest = base_lineage.head_digest;
        let mut prior_sequence = base_lineage.head_sequence;
        let selected_car = changed_projection
            .snapshot
            .cars
            .first()
            .expect("changed projection car");
        direct_delta_rows_from_capture(&mut changed_capture, &binding, selected_car, 1, |batch| {
            let to_sequence = store
                .reserve_next_full_snapshot_sequence(&publication_gate, binding.vehicle_id)
                .map_err(TeslaMateImportError::from)?;
            let delta = batch.into_delta(
                binding.clone(),
                SequenceRange {
                    from_exclusive: prior_sequence,
                    to_inclusive: to_sequence,
                },
                parent_digest,
            );
            let built = ProjectionPackWriter::new(store.packs_dir()).write_delta(
                &ProjectionDeltaPackRequest {
                    pack_id: Uuid::new_v4(),
                    snapshot_id: base_snapshot_id,
                    ordinal: next_ordinal,
                    delta: &delta,
                },
            )?;
            let chain_digest = canonical_delta_chain_digest(parent_digest, built.metadata.sha256);
            deltas.push(LineageDelta {
                from_sequence: prior_sequence,
                to_sequence,
                parent_chain_digest: parent_digest,
                chain_digest,
                pack_digest: built.metadata.sha256,
                pack: built.metadata,
            });
            prior_sequence = to_sequence;
            parent_digest = chain_digest;
            next_ordinal = next_ordinal
                .checked_add(1)
                .ok_or(crate::db::StoreError::PackOrdinalTooLarge)?;
            Ok(())
        })
        .expect("emit changed direct sparse delta");
        assert_eq!(deltas.len(), 1, "one changed drive fits one typed delta");
        let successor_state = changed_capture.into_state();
        let terminal_cursor = OpaqueCursor::issue(
            &cursor_key,
            CursorClaims {
                protocol: PROTOCOL_V1,
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding.installation_id,
                account_id: binding.account_id,
                vehicle_id: binding.vehicle_id,
                generation: binding.generation,
                sequence: prior_sequence,
            },
        )
        .expect("successor terminal cursor");
        store
            .finalize_import_generation_delta_successors_with_projection_state(
                successor_run,
                binding.account_id,
                binding.vehicle_id,
                binding.selected_car_id,
                CHANGED_TIME_MS,
                &deltas,
                &cursor_key,
                &terminal_cursor,
                changed_fingerprint,
                &changed_history.geofences,
                &successor_state,
            )
            .expect("atomically publish direct sparse successor and state");
        assert!(
            store
                .source_fingerprint_matches(binding.vehicle_id, changed_fingerprint)
                .expect("published changed fingerprint lookup")
        );

        let lineage = store
            .lineage_manifest_for_vehicle(binding.vehicle_id)
            .expect("changed lineage lookup")
            .expect("changed lineage");
        lineage
            .validate()
            .expect("published direct lineage validates");
        assert_eq!(lineage.base.snapshot_id, base_snapshot_id);
        assert_eq!(lineage.deltas.len(), 1);
        assert_eq!(lineage.head_sequence, prior_sequence);
        let delta_pack = store
            .pack_for_digest(lineage.deltas[0].pack.sha256)
            .expect("typed delta pack lookup")
            .expect("typed delta pack");
        lineage.deltas[0]
            .pack
            .verify_reader(
                File::open(&delta_pack.path).expect("open typed delta pack"),
                ProtocolLimits::default(),
            )
            .expect("typed delta pack validates");
        let inspection_path = temporary.path().join("stateful-direct-successor.sqlite");
        fs::write(
            &inspection_path,
            zstd::stream::decode_all(File::open(delta_pack.path).expect("decode typed delta pack"))
                .expect("decode typed delta pack"),
        )
        .expect("write typed delta inspection database");
        let inspection = rusqlite::Connection::open(&inspection_path)
            .expect("open typed delta inspection database");
        let mode: String = inspection
            .query_row(
                "SELECT value FROM hub_pack_metadata WHERE key = 'mode'",
                [],
                |row| row.get(0),
            )
            .expect("typed delta mode");
        let changed_drive: i64 = inspection
            .query_row("SELECT id FROM drives", [], |row| row.get(0))
            .expect("changed direct drive in typed delta");
        assert_eq!(mode, "typed_delta");
        assert_eq!(changed_drive, 99);
        drop(inspection);

        let state = store
            .teslamate_import_projection_state_lookup(
                binding.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("published successor state lookup");
        assert_eq!(state.header().base_snapshot_id, base_snapshot_id);
        assert_eq!(state.header().head_sequence, lineage.head_sequence);
        drop(state);
        let connection = store.open().expect("Hub catalogue");
        let base_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sync_bases WHERE vehicle_id = ?1",
                rusqlite::params![binding.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("base count");
        let generation_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("generation count");
        assert_eq!(
            base_count, 1,
            "the successor must retain one immutable base"
        );
        assert_eq!(
            generation_count, 0,
            "base, guarded unchanged, and successor generations must be consumed"
        );
        drop(connection);
        drop(successor_state);
        drop(base_state);
        drop(publication_gate);
        drop(store);

        let restarted = HubStore::initialize(temporary.path()).expect("reopen Hub store");
        let restarted_lineage = restarted
            .lineage_manifest_for_vehicle(binding.vehicle_id)
            .expect("restarted lineage lookup")
            .expect("restarted lineage");
        restarted_lineage
            .validate()
            .expect("restarted direct lineage validates");
        assert_eq!(restarted_lineage.base.snapshot_id, base_snapshot_id);
        assert_eq!(restarted_lineage.deltas.len(), 1);
        assert_eq!(restarted_lineage.head_sequence, prior_sequence);
        let mut restarted_state = restarted
            .teslamate_import_projection_state_lookup(
                binding.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("restarted successor state lookup");
        assert_eq!(
            restarted_state.header().base_snapshot_id,
            restarted_lineage.base.snapshot_id
        );
        assert_eq!(restarted_state.header().head_sequence, prior_sequence);
        assert!(
            crate::teslamate_projection_state::PriorProjectionStateLookup::digest(
                &mut restarted_state,
                TeslaMateProjectionStateEntity::Drive,
                99,
            )
            .expect("restarted changed-drive digest lookup")
            .is_some(),
            "restarted durable state must retain the changed drive digest"
        );
    }

    #[test]
    fn publishes_stable_vehicle_full_snapshots_with_rising_markers() {
        let temporary = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(temporary.path()).unwrap();
        let request = TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let mut source = history();
        source
            .states
            .push(crate::teslamate_projection::TeslaMateState {
                id: 20,
                car_id: 1,
                state: "online".into(),
                start_date_ms: 1_000,
                end_date_ms: None,
            });
        let cursor_key = CursorKey::from_bytes([7; 32]);
        let first = publish_history(&store, &cursor_key, &request, &source).unwrap();
        let second = publish_history(&store, &cursor_key, &request, &source).unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, first.sequence);
        assert!(second.skipped);
        assert_eq!(first.vehicle_id, second.vehicle_id);
        assert_eq!(first.projected_rows, 2);
        let manifest = store
            .manifest_for_vehicle(first.vehicle_id)
            .unwrap()
            .expect("latest manifest");
        assert_eq!(manifest.head_sequence, 1);
        assert_eq!(manifest.schema, crate::hub_pack::HUB_PROJECTION_SCHEMA_V2);
        assert_eq!(
            manifest.chunks[0].format,
            crate::protocol::PackFormat::HubProjectionSqlite
        );
    }

    #[test]
    fn selected_command_post_commit_finalizer_is_explicit_and_retry_safe() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let cursor_key = CursorKey::from_bytes([57; 32]);
        let request = TeslaMateImportRequest {
            source_key: format!("selected-command-{}", Uuid::new_v4()),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let legacy = publish_history(&store, &cursor_key, &request, &history())
            .expect("legacy schema-2.1 import commits");
        let binding = store
            .v2_projection_binding(legacy.vehicle_id)
            .expect("legacy binding");
        let source = DirectUpdatesSourceV2_2 {
            postgres_snapshot_sha256: crate::updates_logical::hex_sha256(Uuid::new_v4().as_bytes()),
            schema: crate::teslamate_reader::TeslaMateSchemaInfo {
                observed_migration_version: crate::teslamate_schema::MAX_VALIDATED_MIGRATION,
                observed_migration_count: crate::teslamate_schema::TESLAMATE_V4_MIGRATION_COUNT,
                minimum_supported_migration_version:
                    crate::teslamate_schema::MIN_SUPPORTED_MIGRATION,
                maximum_validated_migration_version:
                    crate::teslamate_schema::MAX_VALIDATED_MIGRATION,
                pinned_source_revision: crate::teslamate_schema::TESLAMATE_V4_SOURCE_REVISION,
                pinned_migration_set_sha256:
                    crate::teslamate_schema::TESLAMATE_V4_MIGRATION_SET_SHA256,
                fingerprint: crate::updates_logical::hex_sha256(b"selected-command-schema"),
            },
            global_settings: crate::teslamate_projection::TeslaMateSettingsPhysicalV2_2 {
                id: 1,
                unit_of_length: crate::hub_pack::ProjectionUnitOfLengthV2_2::Kilometers,
                unit_of_temperature: crate::hub_pack::ProjectionUnitOfTemperatureV2_2::Celsius,
                unit_of_pressure: crate::hub_pack::ProjectionUnitOfPressureV2_2::Bar,
                preferred_range: crate::hub_pack::ProjectionPreferredRangeV2_2::Rated,
                base_url: None,
                grafana_url: None,
                language: "en".into(),
                theme_mode: "system".into(),
                inserted_at_pg_us: 0,
                updated_at_pg_us: 0,
            },
            car: crate::teslamate_projection::TeslaMateCarPhysicalV2_2 {
                id: 1,
                eid: 88,
                vid: 99,
                vin: Some("5YJTESTVIN1234567".into()),
                name: Some("Road car".into()),
                model: Some("Model 3".into()),
                efficiency: Some(0.145),
                trim_badging: Some("74d".into()),
                marketing_name: Some("LR AWD".into()),
                exterior_color: Some("Pearl White".into()),
                wheel_type: Some("Apollo".into()),
                spoiler_type: Some("None".into()),
                display_priority: 0,
                inserted_at_pg_us: 0,
                updated_at_pg_us: 0,
                settings_id: 1,
            },
            car_settings: crate::teslamate_projection::TeslaMateCarSettingsPhysicalV2_2 {
                id: 1,
                suspend_min: 21,
                suspend_after_idle_min: 15,
                req_not_unlocked: false,
                free_supercharging: false,
                use_streaming_api: true,
                enabled: true,
                lfp_battery: false,
            },
            updates: Vec::new(),
        };
        let registered_car = history().cars.remove(0);
        validate_exported_vehicle_identity(&registered_car, &source)
            .expect("same exported VIN/EID/VID tuple");
        let mut changed_eid = source.clone();
        changed_eid.car.eid += 1;
        let mut changed_vid = source.clone();
        changed_vid.car.vid += 1;
        let mut changed_vin = source.clone();
        changed_vin.car.vin = Some("DIFFERENTVIN".into());
        for changed in [&changed_eid, &changed_vid, &changed_vin] {
            assert!(matches!(
                validate_exported_vehicle_identity(&registered_car, changed),
                Err(TeslaMateImportError::SourceVehicleIdentityChangedDuringCapture)
            ));
        }
        let legacy_manifest = store
            .manifest_for_vehicle(legacy.vehicle_id)
            .expect("legacy lookup")
            .expect("legacy manifest");
        let unadmitted_gate = store
            .try_acquire_publication_gate()
            .expect("unadmitted schema head gate");
        let expected_schema_22_head =
            production_updates_head(&store, legacy.vehicle_id).expect("schema-2.2 head");
        let error = publish_production_updates_schema_22_with_gate(
            &store,
            &cursor_key,
            &binding,
            source.clone(),
            &unadmitted_gate,
            &expected_schema_22_head,
            None,
        )
        .expect_err("other-schema head needs an exact admitted legacy commit");
        drop(unadmitted_gate);
        assert!(error.message.contains("unadmitted other-schema head"));
        assert_eq!(
            store
                .manifest_for_vehicle(legacy.vehicle_id)
                .expect("manifest after unadmitted rejection")
                .expect("legacy manifest retained"),
            legacy_manifest
        );
        let mut wrong_binding = binding.clone();
        wrong_binding.selected_car_id = 2;
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("post-commit publication gate");
        let error = finish_selected_schema_22_publication(
            &store,
            &cursor_key,
            CapturedTeslaMateImport {
                report: legacy.clone(),
                binding: wrong_binding,
                updates_v2_2: source.clone(),
                legacy_tokens: None,
                publication_gate,
            },
        )
        .expect_err("post-commit publication failure is explicit");
        assert!(matches!(
            error,
            TeslaMateImportError::Schema22PostCommit {
                vehicle_id,
                legacy_snapshot_id,
                ..
            } if vehicle_id == legacy.vehicle_id && legacy_snapshot_id == legacy.snapshot_id
        ));
        assert_eq!(
            store
                .manifest_for_vehicle(legacy.vehicle_id)
                .expect("manifest after failure")
                .expect("legacy manifest retained"),
            legacy_manifest
        );

        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("retry publication gate");
        let completed = finish_selected_schema_22_publication(
            &store,
            &cursor_key,
            CapturedTeslaMateImport {
                report: legacy.clone(),
                binding: binding.clone(),
                updates_v2_2: source.clone(),
                legacy_tokens: None,
                publication_gate,
            },
        )
        .expect("retry publishes the schema-2.2 pair");
        assert_eq!(completed.import, legacy);
        assert!(completed.updates_schema_22.sequence > completed.import.sequence);
        assert_eq!(
            completed.updates_schema_22.source_witness.source_row_count,
            0
        );
        let current = store
            .manifest_for_vehicle(completed.import.vehicle_id)
            .expect("schema-2.2 lookup")
            .expect("schema-2.2 manifest");
        assert_eq!(current.schema, crate::protocol::HUB_PROJECTION_SCHEMA_V3);
        assert_eq!(current.generation, binding.generation);
        assert_eq!(current.snapshot_id, completed.updates_schema_22.snapshot_id);
        assert_eq!(current.head_sequence, completed.updates_schema_22.sequence);
        crate::updates_delivery::schema_22_signed_artifacts(
            &store,
            current.vehicle_id,
            &cursor_key,
        )
        .expect("canonical signed manifest/no-op pair");

        let delta_temporary = tempfile::tempdir().expect("delta Hub store");
        let delta_store = HubStore::initialize(delta_temporary.path()).expect("delta store");
        let delta_request = TeslaMateImportRequest {
            source_key: format!("selected-delta-{}", Uuid::new_v4()),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_100,
        };
        let base = publish_history(&delta_store, &cursor_key, &delta_request, &history())
            .expect("schema-2.1 base");
        let mut changed_history = history();
        changed_history.updates.push(TeslaMateUpdate {
            id: 71,
            car_id: 1,
            start_date_ms: 1_700_000_000_000,
            end_date_ms: Some(1_700_000_060_000),
            version: Some("2026.44.1".into()),
        });
        let delta = publish_history(
            &delta_store,
            &cursor_key,
            &TeslaMateImportRequest {
                imported_at_ms: 1_700_000_000_101,
                ..delta_request
            },
            &changed_history,
        )
        .expect("schema-2.1 typed delta");
        assert_eq!(delta.snapshot_id, base.snapshot_id);
        assert!(delta.sequence > base.sequence);
        let immutable_base = delta_store
            .manifest_for_vehicle(delta.vehicle_id)
            .expect("base manifest lookup")
            .expect("immutable base manifest");
        assert!(immutable_base.head_sequence < delta.sequence);
        let delta_binding = delta_store
            .v2_projection_binding(delta.vehicle_id)
            .expect("delta binding");
        let mut delta_updates = source;
        delta_updates.postgres_snapshot_sha256 =
            crate::updates_logical::hex_sha256(Uuid::new_v4().as_bytes());
        delta_updates
            .updates
            .push(crate::teslamate_projection::TeslaMateUpdatePhysicalV2_2 {
                id: 71,
                car_id: 1,
                start_date_pg_us: 0,
                end_date_pg_us: Some(1),
                version: Some("2026.44.1".into()),
            });
        let publication_gate = delta_store
            .try_acquire_publication_gate()
            .expect("delta bootstrap gate");
        let upgraded = finish_selected_schema_22_publication(
            &delta_store,
            &cursor_key,
            CapturedTeslaMateImport {
                report: delta.clone(),
                binding: delta_binding.clone(),
                updates_v2_2: delta_updates,
                legacy_tokens: None,
                publication_gate,
            },
        )
        .expect("current V2 delta head admits schema-2.2 bootstrap");
        assert!(upgraded.updates_schema_22.sequence > delta.sequence);
        assert_eq!(
            upgraded.updates_schema_22.source_witness.selected_car_id,
            delta_binding.selected_car_id
        );
        assert_eq!(
            upgraded.updates_schema_22.source_witness.head_sequence,
            upgraded.updates_schema_22.sequence
        );
    }

    #[test]
    fn rejected_exported_identity_restores_source_vehicle_and_alias_registry() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let empty_registry = identity_registry_image(&store);
        let source_descriptor =
            SourceDescriptor::new("teslamate", format!("identity-{}", Uuid::new_v4()));
        let (source, source_created) = store
            .register_teslamate_import_source(&source_descriptor, 1_000)
            .expect("provisional source");
        assert!(source_created);
        let vehicle_id = Uuid::new_v5(&source.source_id, b"eid:88");
        let identity_hint = teslamate_identity_hint(source.source_id, "eid:88");
        let (vehicle, checkpoint) = store
            .provision_teslamate_import_identity(
                &source,
                source_created,
                &identity_hint,
                1_000,
                vehicle_id,
            )
            .expect("provisional vehicle");
        assert_eq!(vehicle.vin, None);
        assert_eq!(vehicle.display_name, None);
        assert_eq!(identity_registry_image(&store)[3], "[]");
        assert!(
            store
                .published_vehicles()
                .expect("published vehicles")
                .is_empty()
        );
        let run_id = store
            .begin_import_generation(source.source_id, vehicle.vehicle_id, 1, 1_000)
            .expect("provisional generation");
        store
            .abort_import_generation(run_id)
            .expect("abort mismatched generation");
        store
            .rollback_teslamate_identity_registration(&checkpoint)
            .expect("rollback mismatched identity");
        assert_eq!(identity_registry_image(&store), empty_registry);
    }

    #[test]
    fn crash_residue_is_alias_free_nonpublished_and_reused_after_reopen() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let source_descriptor =
            SourceDescriptor::new("teslamate", format!("crash-{}", Uuid::new_v4()));
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let (source_before_crash, source_created) = store
            .register_teslamate_import_source(&source_descriptor, 2_000)
            .expect("source commit before crash");
        assert!(source_created);
        drop(store);

        let reopened = HubStore::initialize(temporary.path()).expect("reopen after source commit");
        let (source, source_created) = reopened
            .register_teslamate_import_source(&source_descriptor, 3_000)
            .expect("reuse source residue");
        assert_eq!(source, source_before_crash);
        assert!(!source_created);
        let vehicle_id = Uuid::new_v5(&source.source_id, b"eid:88");
        let identity_hint = teslamate_identity_hint(source.source_id, "eid:88");
        let (vehicle_before_crash, _) = reopened
            .provision_teslamate_import_identity(&source, false, &identity_hint, 3_000, vehicle_id)
            .expect("vehicle commit before crash");
        drop(reopened);

        let retried = HubStore::initialize(temporary.path()).expect("reopen after vehicle commit");
        let (source, source_created) = retried
            .register_teslamate_import_source(&source_descriptor, 4_000)
            .expect("reuse source on retry");
        let (vehicle, checkpoint) = retried
            .provision_teslamate_import_identity(
                &source,
                source_created,
                &identity_hint,
                4_000,
                vehicle_id,
            )
            .expect("reuse vehicle on retry");
        assert_eq!(vehicle, vehicle_before_crash);
        assert_eq!(identity_registry_image(&retried)[3], "[]");
        assert!(
            retried
                .published_vehicles()
                .expect("published vehicles")
                .is_empty()
        );
        retried
            .rollback_teslamate_identity_registration(&checkpoint)
            .expect("reused identity has nothing provisional to delete");
        assert_eq!(
            retried
                .register_teslamate_import_source(&source_descriptor, 5_000)
                .expect("source remains reusable")
                .0,
            source
        );
    }

    #[test]
    fn rollback_preserves_interleaved_collector_alias_and_observation() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let source_descriptor =
            SourceDescriptor::new("teslamate", format!("interleaved-{}", Uuid::new_v4()));
        let (source, source_created) = store
            .register_teslamate_import_source(&source_descriptor, 2_000)
            .expect("provisional source");
        let vehicle_id = Uuid::new_v5(&source.source_id, b"eid:88");
        let identity_hint = teslamate_identity_hint(source.source_id, "eid:88");
        let (_, checkpoint) = store
            .provision_teslamate_import_identity(
                &source,
                source_created,
                &identity_hint,
                2_000,
                vehicle_id,
            )
            .expect("provisional vehicle");
        let collector_descriptor = VehicleDescriptor {
            source_id: source.source_id,
            source_vehicle_key: "eid:88".into(),
            vin: Some("STABLEVIN".into()),
            display_name: Some("stable".into()),
            tesla_eid: Some(88),
            tesla_vid: Some(99),
        };
        let collector_vehicle = store
            .register_vehicle_with_id(&collector_descriptor, 3_000, vehicle_id)
            .expect("collector registers proven identity");
        store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: collector_vehicle.vehicle_id,
                    observed_at_ms: 3_000,
                    payload: serde_json::json!({"state": "online"}),
                },
                3_001,
            )
            .expect("collector observation");
        let collector_registry = identity_registry_image(&store);
        assert!(matches!(
            store.rollback_teslamate_identity_registration(&checkpoint),
            Err(StoreError::VehicleIdentityConflict)
        ));
        assert_eq!(identity_registry_image(&store), collector_registry);
        let observation_count: i64 = store
            .open()
            .expect("observation registry")
            .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                row.get(0)
            })
            .expect("observation count");
        assert_eq!(observation_count, 1);
        assert!(
            store
                .published_vehicles()
                .expect("published vehicles")
                .is_empty()
        );
    }

    #[test]
    fn teslamate_import_reuses_existing_cross_source_vehicle_identity() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let owner_source = store
            .register_source(
                &SourceDescriptor::new("owner_api", format!("owner-{}", Uuid::new_v4())),
                1_000,
            )
            .expect("owner source");
        let owner_descriptor = VehicleDescriptor {
            source_id: owner_source.source_id,
            source_vehicle_key: "owner-88".into(),
            vin: Some("STABLEVIN".into()),
            display_name: Some("owner car".into()),
            tesla_eid: Some(88),
            tesla_vid: Some(99),
        };
        let owner_vehicle = store
            .register_vehicle(&owner_descriptor, 1_001)
            .expect("owner vehicle");

        let teslamate_descriptor =
            SourceDescriptor::new("teslamate", format!("history-{}", Uuid::new_v4()));
        let (teslamate_source, source_created) = store
            .register_teslamate_import_source(&teslamate_descriptor, 2_000)
            .expect("TeslaMate source");
        let identity_hint = teslamate_identity_hint(teslamate_source.source_id, "eid:88");
        let deterministic_vehicle_id = Uuid::new_v5(&teslamate_source.source_id, b"eid:88");
        let (provisioned, _) = store
            .provision_teslamate_import_identity(
                &teslamate_source,
                source_created,
                &identity_hint,
                2_000,
                deterministic_vehicle_id,
            )
            .expect("reuse owner identity before capture");
        assert_eq!(provisioned.vehicle_id, owner_vehicle.vehicle_id);

        let verified = store
            .register_vehicle_with_id(&identity_hint, 2_001, deterministic_vehicle_id)
            .expect("attach TeslaMate identity after frozen proof");
        assert_eq!(verified.vehicle_id, owner_vehicle.vehicle_id);
        let vehicle_count: i64 = store
            .open()
            .expect("vehicle registry")
            .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
            .expect("vehicle count");
        assert_eq!(vehicle_count, 1);
    }

    #[test]
    fn crash_residue_converges_with_later_cross_source_collector_identity() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let teslamate_descriptor =
            SourceDescriptor::new("teslamate", format!("crash-merge-{}", Uuid::new_v4()));
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let (teslamate_source, source_created) = store
            .register_teslamate_import_source(&teslamate_descriptor, 1_000)
            .expect("TeslaMate source");
        let identity_hint = teslamate_identity_hint(teslamate_source.source_id, "eid:88");
        let provisional_id = Uuid::new_v5(&teslamate_source.source_id, b"eid:88");
        let (provisional, _) = store
            .provision_teslamate_import_identity(
                &teslamate_source,
                source_created,
                &identity_hint,
                1_000,
                provisional_id,
            )
            .expect("alias-free vehicle before crash");
        assert_eq!(provisional.vehicle_id, provisional_id);
        drop(store);

        let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
        let owner_source = restarted
            .register_source(
                &SourceDescriptor::new("owner_api", format!("owner-{}", Uuid::new_v4())),
                2_000,
            )
            .expect("owner source");
        let owner_descriptor = VehicleDescriptor {
            source_id: owner_source.source_id,
            source_vehicle_key: "owner-88".into(),
            vin: Some("STABLEVIN".into()),
            display_name: Some("owner car".into()),
            tesla_eid: Some(88),
            tesla_vid: Some(99),
        };
        let owner_vehicle = restarted
            .register_vehicle(&owner_descriptor, 2_001)
            .expect("collector identity after crash");
        restarted
            .append_observation(
                &ObservationInput {
                    source_id: owner_source.source_id,
                    vehicle_id: owner_vehicle.vehicle_id,
                    observed_at_ms: 2_002,
                    payload: serde_json::json!({"state": "online"}),
                },
                2_003,
            )
            .expect("collector observation");

        let (teslamate_source, source_created) = restarted
            .register_teslamate_import_source(&teslamate_descriptor, 3_000)
            .expect("reuse TeslaMate source");
        assert!(!source_created);
        let identity_hint = teslamate_identity_hint(teslamate_source.source_id, "eid:88");
        let (converged, _) = restarted
            .provision_teslamate_import_identity(
                &teslamate_source,
                source_created,
                &identity_hint,
                3_000,
                provisional_id,
            )
            .expect("converge crash residue with collector identity");
        assert_eq!(converged.vehicle_id, owner_vehicle.vehicle_id);
        let connection = restarted.open().expect("converged registry");
        let vehicle_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
            .expect("vehicle count");
        let observation_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                row.get(0)
            })
            .expect("observation count");
        assert_eq!(vehicle_count, 1);
        assert_eq!(observation_count, 1);
    }

    #[test]
    fn publish_history_keeps_thirty_nine_exact_updates_in_signed_base_and_changed_delta() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let request = TeslaMateImportRequest {
            source_key: "update-history-regression".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let cursor_key = CursorKey::from_bytes([61; 32]);
        let mut source = history();
        let expected = (0_i64..39)
            .map(|offset| {
                let id = 31_000 + offset;
                let start_date_ms = 1_800_000_000_000 + offset * 10_000;
                let end_date_ms = start_date_ms + 9_000;
                let version = format!("2026.44.{}", offset + 1);
                source.updates.push(TeslaMateUpdate {
                    id,
                    car_id: 1,
                    start_date_ms,
                    end_date_ms: Some(end_date_ms),
                    version: Some(version.clone()),
                });
                (id, 1, start_date_ms, end_date_ms, version)
            })
            .collect::<Vec<_>>();

        let first = publish_history(&store, &cursor_key, &request, &source)
            .expect("publish signed schema-2.1 base");
        assert!(!first.skipped);
        assert_eq!(first.projection.projected_updates, 39);
        let base_manifest = store
            .manifest_for_vehicle(first.vehicle_id)
            .expect("base manifest query")
            .expect("base manifest");
        assert_eq!(base_manifest.schema, HUB_PROJECTION_SCHEMA_V2);
        base_manifest
            .validate_terminal_cursor(&cursor_key)
            .expect("base manifest has a valid signature");
        let base_pack = store
            .pack_for_digest(base_manifest.chunks[0].sha256)
            .expect("base pack lookup")
            .expect("base pack");
        assert_eq!(
            update_rows_from_pack(temporary.path(), &base_pack.path, "base-updates"),
            expected,
            "base pack preserves every source update ID, version, and complete date range"
        );

        let changed_offset = 20_usize;
        let changed_end_date_ms = expected[changed_offset].3 + 5_000;
        let changed_version = "2026.44.21-hotfix".to_owned();
        source.updates[changed_offset].end_date_ms = Some(changed_end_date_ms);
        source.updates[changed_offset].version = Some(changed_version.clone());
        let mut expected_changed = expected.clone();
        expected_changed[changed_offset].3 = changed_end_date_ms;
        expected_changed[changed_offset].4 = changed_version;

        let second = publish_history(&store, &cursor_key, &request, &source)
            .expect("changed update publishes a typed successor");
        assert!(!second.skipped);
        assert_eq!(second.snapshot_id, first.snapshot_id);
        assert!(second.sequence > first.sequence);
        let lineage = store
            .lineage_manifest_for_vehicle(first.vehicle_id)
            .expect("lineage lookup")
            .expect("lineage");
        lineage
            .validate()
            .expect("changed update lineage remains signed and valid");
        assert_eq!(lineage.deltas.len(), 1);
        let delta_pack = store
            .pack_for_digest(lineage.deltas[0].pack.sha256)
            .expect("delta pack lookup")
            .expect("typed delta pack");
        assert_eq!(lineage.deltas[0].pack.schema, HUB_PROJECTION_SCHEMA_V2);
        assert_eq!(
            update_rows_from_pack(temporary.path(), &delta_pack.path, "changed-updates"),
            expected_changed,
            "changed firmware history is carried by the typed delta instead of being dropped"
        );
    }

    #[test]
    fn imported_geofences_survive_publication_and_restart() {
        let data = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(data.path()).unwrap();
        let request = TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let mut source = history();
        source.geofences = vec![
            TeslaMateGeofence {
                id: 1,
                name: "Home".into(),
                latitude: Some(51.0),
                longitude: Some(-0.1),
                radius_m: Some(150.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
                cost_per_unit: Some(0.30),
                session_fee: Some(2.0),
            },
            TeslaMateGeofence {
                id: 2,
                name: "Work".into(),
                latitude: Some(51.001),
                longitude: Some(-0.101),
                radius_m: Some(150.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerMinute),
                cost_per_unit: Some(0.10),
                session_fee: Some(1.0),
            },
        ];
        let cursor_key = CursorKey::from_bytes([8; 32]);
        let published = publish_history(&store, &cursor_key, &request, &source).unwrap();
        let count: i64 = store
            .open()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM geofences WHERE vehicle_id = ?1",
                [published.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        drop(store);

        let reopened = HubStore::initialize(data.path()).unwrap();
        let names: Vec<String> = {
            let connection = reopened.open().unwrap();
            let mut statement = connection
                .prepare(
                    "SELECT name FROM geofences WHERE vehicle_id = ?1 ORDER BY source_geofence_id",
                )
                .unwrap();
            statement
                .query_map([published.vehicle_id.to_string()], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(names, ["Home", "Work"]);
    }

    #[test]
    fn eid_fallback_never_uses_the_local_car_id() {
        let mut source = history();
        source.cars[0].vin = None;
        source.cars[0].eid = 9001;
        assert_eq!(
            stable_vehicle_key_for_car(&source.cars[0]).unwrap(),
            "eid:9001"
        );
    }

    #[test]
    fn changed_history_publishes_import_delta_successor_without_second_base() {
        let temporary = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(temporary.path()).unwrap();
        let request = TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let cursor_key = CursorKey::from_bytes([7; 32]);
        let first_source = history();
        let first = publish_history(&store, &cursor_key, &request, &first_source).unwrap();
        assert!(!first.skipped);
        assert_eq!(first.sequence, 1);

        let base_lineage = store
            .lineage_manifest_for_vehicle(first.vehicle_id)
            .expect("lineage after first import")
            .expect("base lineage present");
        base_lineage
            .validate()
            .expect("first import lineage must validate");
        assert!(base_lineage.deltas.is_empty());
        assert_eq!(base_lineage.base.snapshot_id, first.snapshot_id);
        assert_eq!(base_lineage.head_sequence, first.sequence);

        // A SyncManifest can describe a complete snapshot but cannot prove
        // that its SQLite payload is a typed delta. Do not let a caller relabel
        // the base bytes with a successor range: only the dedicated typed-delta
        // finalizer may extend this lineage.
        let before_forged_successor = base_lineage.clone();
        let mut forged_successor = store
            .manifest_for_vehicle(first.vehicle_id)
            .expect("base manifest lookup")
            .expect("base manifest present");
        forged_successor.mode = crate::protocol::TransferMode::Incremental;
        forged_successor.base_sequence = first.sequence;
        forged_successor.head_sequence = first.sequence + 1;
        forged_successor.chunks[0].sequence = SequenceRange {
            from_exclusive: first.sequence,
            to_inclusive: forged_successor.head_sequence,
        };
        forged_successor.terminal_cursor = OpaqueCursor::issue(
            &cursor_key,
            CursorClaims {
                protocol: PROTOCOL_V1,
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: forged_successor.installation_id,
                account_id: forged_successor.account_id,
                vehicle_id: forged_successor.vehicle_id,
                generation: forged_successor.generation,
                sequence: forged_successor.head_sequence,
            },
        )
        .expect("forged cursor shape");
        assert!(matches!(
            store.publish_manifest(&forged_successor),
            Err(crate::db::StoreError::ImmutableBaseBindingMissing(vehicle_id))
                if vehicle_id == first.vehicle_id
        ));
        assert_eq!(
            store
                .lineage_manifest_for_vehicle(first.vehicle_id)
                .expect("lineage after rejected forged successor"),
            Some(before_forged_successor),
            "rejected full-snapshot relabel must not mutate the published lineage"
        );

        let mut changed = history();
        // Change source history fingerprint by adding a completed drive.
        changed
            .drives
            .push(crate::teslamate_projection::TeslaMateDrive {
                id: 99,
                car_id: 1,
                start_date_ms: 2_000,
                end_date_ms: Some(3_000),
                outside_temp_avg: None,
                speed_max: Some(50),
                power_max: None,
                power_min: None,
                start_ideal_range_km: None,
                end_ideal_range_km: None,
                start_rated_range_km: Some(300.0),
                end_rated_range_km: Some(280.0),
                start_km: Some(10.0),
                end_km: Some(20.0),
                distance_km: Some(10.0),
                duration_min: Some(1),
                start_address_id: None,
                end_address_id: None,
                start_geofence_id: None,
                end_geofence_id: None,
                start_position_id: None,
                end_position_id: None,
                ascent: None,
                descent: None,
                inside_temp_avg: None,
            });
        let second = publish_history(&store, &cursor_key, &request, &changed)
            .expect("changed history must publish as import delta successor");
        assert!(!second.skipped, "changed fingerprint must not skip");
        assert!(second.sequence > first.sequence);
        assert_eq!(second.vehicle_id, first.vehicle_id);
        assert_eq!(
            second.snapshot_id, first.snapshot_id,
            "changed history must keep the immutable base snapshot identity"
        );

        let changed_lineage = store
            .lineage_manifest_for_vehicle(second.vehicle_id)
            .expect("lineage after changed import")
            .expect("changed lineage present");
        changed_lineage
            .validate()
            .expect("changed-history lineage must validate for public retrieval");
        assert_eq!(changed_lineage.base.snapshot_id, first.snapshot_id);
        assert_eq!(changed_lineage.deltas.len(), 1);
        assert_eq!(
            changed_lineage.deltas[0].pack.snapshot_id,
            first.snapshot_id
        );
        assert_eq!(
            changed_lineage.deltas[0].pack.sequence.from_exclusive,
            first.sequence
        );
        assert_eq!(
            changed_lineage.deltas[0].pack.sequence.to_inclusive,
            second.sequence
        );
        assert_eq!(changed_lineage.head_sequence, second.sequence);
        let delta_pack = store
            .pack_for_digest(changed_lineage.deltas[0].pack.sha256)
            .expect("delta pack lookup")
            .expect("delta pack is servable");
        changed_lineage.deltas[0]
            .pack
            .verify_reader(
                File::open(&delta_pack.path).expect("open delta pack"),
                ProtocolLimits::default(),
            )
            .expect("servable delta bytes validate against the published manifest");
        let inspection_path = temporary.path().join("changed-history-delta.sqlite");
        fs::write(
            &inspection_path,
            zstd::stream::decode_all(
                File::open(delta_pack.path).expect("open delta for inspection"),
            )
            .expect("decode typed delta"),
        )
        .expect("write typed-delta inspection database");
        let inspection = rusqlite::Connection::open(inspection_path).expect("open typed delta");
        let mode: String = inspection
            .query_row(
                "SELECT value FROM hub_pack_metadata WHERE key = 'mode'",
                [],
                |row| row.get(0),
            )
            .expect("delta mode");
        let parent_digest: String = inspection
            .query_row(
                "SELECT value FROM hub_pack_metadata WHERE key = 'parent_digest'",
                [],
                |row| row.get(0),
            )
            .expect("delta parent");
        let changed_drive: i64 = inspection
            .query_row("SELECT id FROM drives", [], |row| row.get(0))
            .expect("changed drive is present in the apply payload");
        assert_eq!(mode, "typed_delta");
        assert_eq!(parent_digest, base_lineage.head_digest.to_string());
        assert_eq!(changed_drive, 99);
        assert_eq!(
            store
                .open()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM sync_bases WHERE vehicle_id = ?1",
                    rusqlite::params![first.vehicle_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1,
            "must not create a second immutable base"
        );

        let third = publish_history(&store, &cursor_key, &request, &changed).unwrap();
        assert!(third.skipped, "identical changed fingerprint skips");
        assert_eq!(third.sequence, second.sequence);
        assert_eq!(third.snapshot_id, first.snapshot_id);

        // Restart: reopen the same store directory and retrieve lineage again.
        let restarted = HubStore::initialize(temporary.path()).unwrap();
        let restarted_lineage = restarted
            .lineage_manifest_for_vehicle(first.vehicle_id)
            .expect("lineage after restart")
            .expect("lineage survives restart");
        restarted_lineage
            .validate()
            .expect("restarted lineage must remain valid");
        assert_eq!(restarted_lineage.base.snapshot_id, first.snapshot_id);
        assert_eq!(restarted_lineage.deltas.len(), 1);
        assert_eq!(restarted_lineage.head_sequence, second.sequence);
    }

    #[test]
    fn changed_history_tombstones_removed_teslamate_rows_without_second_base() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let request = TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let cursor_key = CursorKey::from_bytes([31; 32]);
        let mut first_history = history();
        first_history.drives.push(completed_drive(44));
        let first = publish_history(&store, &cursor_key, &request, &first_history)
            .expect("first source history publishes a base");
        let base = store
            .lineage_manifest_for_vehicle(first.vehicle_id)
            .expect("base lineage lookup")
            .expect("base lineage");

        let mut rewritten = first_history.clone();
        rewritten.drives.clear();
        let second = publish_history(&store, &cursor_key, &request, &rewritten)
            .expect("removing a published row produces a typed successor");
        assert_eq!(second.snapshot_id, first.snapshot_id);
        assert!(second.sequence > first.sequence);

        let lineage = store
            .lineage_manifest_for_vehicle(first.vehicle_id)
            .expect("rewritten lineage lookup")
            .expect("rewritten lineage");
        lineage
            .validate()
            .expect("tombstone successor remains client-valid lineage");
        assert_eq!(lineage.base.snapshot_id, first.snapshot_id);
        assert_eq!(lineage.deltas.len(), 1);
        let delta_pack = store
            .pack_for_digest(lineage.deltas[0].pack.sha256)
            .expect("delta pack lookup")
            .expect("delta pack exists");
        lineage.deltas[0]
            .pack
            .verify_reader(
                File::open(&delta_pack.path).expect("open delta pack"),
                ProtocolLimits::default(),
            )
            .expect("tombstone delta transport is valid");
        let inspection_path = temporary.path().join("removed-history-delta.sqlite");
        fs::write(
            &inspection_path,
            zstd::stream::decode_all(File::open(delta_pack.path).expect("open delta pack"))
                .expect("decode delta pack"),
        )
        .expect("write inspection pack");
        let inspection = rusqlite::Connection::open(inspection_path).expect("open inspection");
        let tombstones: Vec<(String, i64, i64)> = inspection
            .prepare("SELECT entity, entity_id, car_id FROM tombstones ORDER BY entity, entity_id")
            .expect("prepare tombstone query")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query tombstones")
            .collect::<Result<_, _>>()
            .expect("read tombstones");
        assert_eq!(tombstones, vec![("drive".into(), 44, 1)]);
        assert!(
            store
                .teslamate_import_projection_inventory(first.vehicle_id, first.source_id, 1)
                .expect("current import inventory")
                .rows
                .is_empty()
        );
        assert_eq!(
            store
                .open()
                .expect("open catalogue")
                .query_row(
                    "SELECT COUNT(*) FROM sync_bases WHERE vehicle_id = ?1",
                    rusqlite::params![first.vehicle_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("base count"),
            1,
            "history rewrites must extend, never replace, the immutable base"
        );
        assert_eq!(base.base.snapshot_id, lineage.base.snapshot_id);
    }

    #[test]
    fn changed_history_without_prior_inventory_fails_before_lineage_mutation() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let request = TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let cursor_key = CursorKey::from_bytes([32; 32]);
        let mut first_history = history();
        first_history.drives.push(completed_drive(45));
        let first = publish_history(&store, &cursor_key, &request, &first_history)
            .expect("first source history publishes a base");
        let before = store
            .lineage_manifest_for_vehicle(first.vehicle_id)
            .expect("base lineage lookup")
            .expect("base lineage");
        store
            .open()
            .expect("open catalogue")
            .execute(
                "DELETE FROM teslamate_import_projection_heads WHERE vehicle_id = ?1",
                rusqlite::params![first.vehicle_id.to_string()],
            )
            .expect("simulate a pre-inventory legacy base");

        let mut rewritten = first_history;
        rewritten.drives.clear();
        let error = publish_history(&store, &cursor_key, &request, &rewritten)
            .expect_err("a legacy base without exact provenance must fail closed");
        assert!(matches!(
            error,
            TeslaMateImportError::Store(crate::db::StoreError::TeslaMateImportInventoryMissing(id))
                if id == first.vehicle_id
        ));
        assert_eq!(
            store
                .lineage_manifest_for_vehicle(first.vehicle_id)
                .expect("lineage after rejection"),
            Some(before),
            "the failed rewrite must not reserve or publish a successor"
        );
    }

    #[test]
    fn owner_compat_identity_before_teslamate_base_keeps_the_teslamate_binding() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let owner = store
            .register_source(
                &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
                1_700_000_000_000,
            )
            .expect("owner compatibility source");
        let owner_vehicle = store
            .register_vehicle(
                &VehicleDescriptor {
                    source_id: owner.source_id,
                    source_vehicle_key: "88".into(),
                    vin: Some("5YJTESTVIN1234567".into()),
                    display_name: Some("Owner compatibility car".into()),
                    tesla_eid: Some(88),
                    tesla_vid: None,
                },
                1_700_000_000_000,
            )
            .expect("owner compatibility vehicle");
        store
            .upsert_car_settings(
                owner_vehicle.vehicle_id,
                88,
                &crate::hub_pack::ProjectionCarSettings::default(),
            )
            .expect("owner compatibility settings");

        let request = TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_001,
        };
        let cursor_key = CursorKey::from_bytes([33; 32]);
        let first = publish_history(&store, &cursor_key, &request, &history())
            .expect("TeslaMate base can unify with owner identity");
        assert_eq!(first.vehicle_id, owner_vehicle.vehicle_id);
        let binding = store
            .v2_projection_binding(first.vehicle_id)
            .expect("immutable base binding");
        assert_eq!(binding.account_id, first.source_id);
        assert_eq!(binding.selected_car_id, 1);
        assert_ne!(binding.account_id, owner.source_id);

        let mut changed = history();
        changed.drives.push(completed_drive(46));
        let second = publish_history(&store, &cursor_key, &request, &changed)
            .expect("changed TeslaMate history extends the cross-source base");
        let lineage = store
            .lineage_manifest_for_vehicle(first.vehicle_id)
            .expect("lineage lookup")
            .expect("lineage");
        lineage
            .validate()
            .expect("cross-source successor remains client-valid");
        assert_eq!(second.snapshot_id, first.snapshot_id);
        assert_eq!(lineage.account_id, first.source_id);
        assert_eq!(lineage.deltas.len(), 1);
    }

    #[test]
    fn sealed_stage_publication_never_needs_an_in_memory_history() {
        let data = tempfile::tempdir().unwrap();
        let imports = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(data.path()).unwrap();
        let mut stage = TeslaMateStage::create(
            imports.path().join("imports"),
            TeslaMateStageLimits {
                max_rows: 10,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .unwrap();
        let car = history().cars.remove(0);
        stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .unwrap();
        stage.seal().unwrap();

        let report = publish_staged_history(
            &store,
            &CursorKey::from_bytes([9; 32]),
            &TeslaMateImportRequest {
                source_key: "home-teslamate".into(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms: 1_700_000_000_000,
            },
            &stage,
        )
        .unwrap();
        let manifest = store
            .manifest_for_vehicle(report.vehicle_id)
            .unwrap()
            .expect("published staged manifest");
        assert_eq!(manifest.chunk_count, 1);
        assert_eq!(manifest.total_rows, 1);
        assert_eq!(report.projection, ProjectionReport::default());
        assert_eq!(
            store.selected_tesla_eid().expect("selected imported car"),
            Some((88, crate::hub_pack::ProjectionCarSettings::default()))
        );

        let unchanged = publish_staged_history(
            &store,
            &CursorKey::from_bytes([9; 32]),
            &TeslaMateImportRequest {
                source_key: "home-teslamate".into(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms: 1_700_000_060_000,
            },
            &stage,
        )
        .unwrap();
        assert_eq!(unchanged.snapshot_id, report.snapshot_id);
        assert_eq!(unchanged.sequence, report.sequence);
        assert_eq!(
            store
                .manifest_for_vehicle(report.vehicle_id)
                .unwrap()
                .unwrap(),
            manifest
        );
    }

    #[test]
    fn staged_unchanged_history_promotes_newer_open_drive_session() {
        let data = tempfile::tempdir().expect("Hub data directory");
        let imports = tempfile::tempdir().expect("staging directory");
        let store = HubStore::initialize(data.path()).expect("Hub store");
        let request = TeslaMateImportRequest {
            source_key: "staged-open-drive".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let cursor_key = CursorKey::from_bytes([70; 32]);
        let mut stage = TeslaMateStage::create(
            imports.path().join("imports"),
            TeslaMateStageLimits {
                max_rows: 16,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("create staged source");
        let car = history().cars.remove(0);
        stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .expect("stage selected car");
        stage.seal().expect("seal staged source");

        let mut first_drive = completed_drive(900);
        first_drive.end_date_ms = None;
        let first_session = TeslaMateOpenSession {
            car_id: 1,
            drive: Some(first_drive),
            ..TeslaMateOpenSession::default()
        };
        let base = publish_staged_history_with_session(
            &store,
            &cursor_key,
            &request,
            &stage,
            &first_session,
        )
        .expect("publish staged base and active drive");
        assert!(!base.skipped);
        assert_eq!(
            store
                .load_imported_open_session(base.source_id, base.vehicle_id)
                .expect("load atomically published active drive"),
            Some(first_session)
        );

        let mut newer_drive = completed_drive(901);
        newer_drive.start_date_ms = 4_000;
        newer_drive.end_date_ms = None;
        let newer_session = TeslaMateOpenSession {
            car_id: 1,
            drive: Some(newer_drive),
            ..TeslaMateOpenSession::default()
        };
        let unchanged_request = TeslaMateImportRequest {
            imported_at_ms: request.imported_at_ms + 1,
            ..request.clone()
        };
        let unchanged = publish_staged_history_with_session(
            &store,
            &cursor_key,
            &unchanged_request,
            &stage,
            &newer_session,
        )
        .expect("promote newer active drive without a new history lineage");
        assert!(
            unchanged.skipped,
            "identical staged history takes fingerprint path"
        );
        assert_eq!(unchanged.snapshot_id, base.snapshot_id);
        assert_eq!(unchanged.sequence, base.sequence);
        assert_eq!(
            store
                .load_imported_open_session(base.source_id, base.vehicle_id)
                .expect("load promoted newer active drive"),
            Some(newer_session)
        );
    }

    #[test]
    fn staged_v21_base_persists_inventory_for_a_changed_history_successor() {
        let data = tempfile::tempdir().expect("Hub data directory");
        let imports = tempfile::tempdir().expect("staging directory");
        let store = HubStore::initialize(data.path()).expect("Hub store");
        let request = TeslaMateImportRequest {
            source_key: "staged-successor".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let cursor_key = CursorKey::from_bytes([71; 32]);
        let mut stage = TeslaMateStage::create(
            imports.path().join("imports"),
            TeslaMateStageLimits {
                max_rows: 16,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("create sealed source stage");
        let car = history().cars.remove(0);
        stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .expect("stage selected car");
        stage.seal().expect("seal staged source");

        let base = publish_staged_history(&store, &cursor_key, &request, &stage)
            .expect("publish staged schema-2.1 base");
        assert!(
            store
                .teslamate_import_projection_inventory(base.vehicle_id, base.source_id, 1)
                .expect("staged base inventory")
                .rows
                .is_empty(),
            "the car-only staged base has no source-owned child inventory"
        );
        let prior = store
            .teslamate_import_projection_state_lookup(base.vehicle_id, base.source_id, 1)
            .expect("staged base durable state");
        assert_eq!(prior.header().base_snapshot_id, base.snapshot_id);
        assert_eq!(prior.header().head_sequence, base.sequence);
        drop(prior);

        let mut changed = history();
        changed.drives.push(completed_drive(77));
        let successor = publish_history(&store, &cursor_key, &request, &changed)
            .expect("changed history extends the staged base");
        assert_eq!(successor.snapshot_id, base.snapshot_id);
        assert!(successor.sequence > base.sequence);
        let lineage = store
            .lineage_manifest_for_vehicle(base.vehicle_id)
            .expect("staged successor lineage lookup")
            .expect("staged successor lineage");
        lineage
            .validate()
            .expect("staged successor remains a valid typed lineage");
        assert_eq!(lineage.deltas.len(), 1);
    }

    #[test]
    fn migration_final_snapshot_after_initial_base_is_servable() {
        let data = tempfile::tempdir().expect("Hub data directory");
        let imports = tempfile::tempdir().expect("staging directory");
        let store = HubStore::initialize(data.path()).expect("Hub store");
        let request = TeslaMateImportRequest {
            source_key: "migration-final-snapshot".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let cursor_key = CursorKey::from_bytes([73; 32]);
        let mut stage = TeslaMateStage::create(
            imports.path().join("imports"),
            TeslaMateStageLimits {
                max_rows: 16,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("create initial migration stage");
        let car = history().cars.remove(0);
        stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .expect("stage initial selected car");
        stage.seal().expect("seal initial migration stage");
        let initial = publish_staged_history(&store, &cursor_key, &request, &stage)
            .expect("initial migration base");

        let mut final_stage = TeslaMateStage::create(
            imports.path().join("imports"),
            TeslaMateStageLimits {
                max_rows: 16,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("create final migration stage");
        let mut final_history = history();
        final_history.drives.push(completed_drive(173));
        let final_car = final_history.cars.remove(0);
        final_stage
            .insert(TeslaMateStageTable::Cars, final_car.id, &final_car)
            .expect("stage final selected car");
        for drive in final_history.drives {
            final_stage
                .insert(TeslaMateStageTable::Drives, drive.id, &drive)
                .expect("stage final drive");
        }
        final_stage.seal().expect("seal final migration stage");
        let mut active_drive = completed_drive(900);
        active_drive.end_date_ms = None;
        let final_session = TeslaMateOpenSession {
            car_id: 1,
            drive: Some(active_drive),
            ..TeslaMateOpenSession::default()
        };
        let final_report = publish_staged_history_with_session(
            &store,
            &cursor_key,
            &request,
            &final_stage,
            &final_session,
        )
        .expect("changed final migration snapshot");
        assert_eq!(final_report.snapshot_id, initial.snapshot_id);
        assert!(final_report.sequence > initial.sequence);
        let persisted_session = store
            .load_imported_open_session(initial.source_id, initial.vehicle_id)
            .expect("load final imported session")
            .expect("final imported session");
        assert_eq!(
            persisted_session.drive.as_ref().map(|row| row.id),
            Some(900)
        );
        assert_eq!(
            persisted_session
                .drive
                .as_ref()
                .and_then(|row| row.end_date_ms),
            None
        );
        assert!(persisted_session.drive_positions.is_empty());
        assert!(persisted_session.charge_samples.is_empty());

        let lineage = store
            .lineage_manifest_for_vehicle(initial.vehicle_id)
            .expect("servable lineage lookup")
            .expect("servable lineage");
        lineage.validate().expect("Serve-compatible lineage");
        assert_eq!(lineage.deltas.len(), 1);
        let delta = &lineage.deltas[0].pack;
        let delta_path = store
            .pack_for_digest(delta.sha256)
            .expect("servable delta lookup")
            .expect("servable delta pack")
            .path;
        delta
            .verify_reader(
                File::open(delta_path).expect("open servable delta"),
                ProtocolLimits::default(),
            )
            .expect("Serve-compatible delta bytes");
    }

    #[test]
    fn staged_v21_base_hands_off_to_a_direct_successor_capture() {
        let data = tempfile::tempdir().expect("Hub data directory");
        let imports = tempfile::tempdir().expect("staging directory");
        let store = HubStore::initialize(data.path()).expect("Hub store");
        let request = TeslaMateImportRequest {
            source_key: "staged-direct-handoff".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        };
        let cursor_key = CursorKey::from_bytes([72; 32]);
        let mut stage = TeslaMateStage::create(
            imports.path().join("imports"),
            TeslaMateStageLimits {
                max_rows: 16,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("create sealed source stage");
        let car = history().cars.remove(0);
        stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .expect("stage selected car");
        stage.seal().expect("seal staged source");
        let base = publish_staged_history(&store, &cursor_key, &request, &stage)
            .expect("publish staged schema-2.1 base");
        let binding = store
            .v2_projection_binding(base.vehicle_id)
            .expect("staged immutable binding");
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("acquire direct handoff gate");
        let run_id = store
            .begin_import_generation(base.source_id, base.vehicle_id, 1, 1_700_000_060_000)
            .expect("begin direct successor generation");
        let mut capture = direct_projection_state_capture(
            &store,
            &publication_gate,
            run_id,
            base.vehicle_id,
            base.source_id,
            1,
            true,
            direct_state_test_limits(),
        )
        .expect("staged base provides the direct successor's durable prior state");
        assert_eq!(
            capture.mode(),
            crate::teslamate_projection_state::TeslaMateProjectionStateCaptureMode::Successor
        );

        let mut changed = history();
        changed.drives.push(completed_drive(78));
        let projected = project_vehicle(&changed, 1).expect("project direct successor source");
        let selected_car = projected.snapshot.cars[0].clone();
        record_projected_direct_state(&mut capture, &projected, 1);
        let state = capture.seal().expect("seal direct successor capture");
        assert_eq!(state.changed_row_count, 1);
        let mut batches = Vec::new();
        direct_delta_rows_from_capture(&mut capture, &binding, &selected_car, 1, |batch| {
            batches.push(batch);
            Ok(())
        })
        .expect("direct successor can emit a sparse typed delta after staged handoff");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].drives, projected.snapshot.drives);

        drop(capture);
        store
            .abort_import_generation(run_id)
            .expect("discard direct handoff test generation");
    }

    #[test]
    fn staged_publication_adapts_before_the_protocol_chunk_ceiling() {
        let data = tempfile::tempdir().unwrap();
        let imports = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(data.path()).unwrap();
        let mut stage = TeslaMateStage::create(
            imports.path().join("imports"),
            TeslaMateStageLimits {
                max_rows: 2_000,
                max_stage_bytes: 4 * 1024 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .unwrap();
        let car = history().cars.remove(0);
        stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .unwrap();
        let base_position = TeslaMatePosition {
            id: 20,
            car_id: 1,
            drive_id: None,
            date_ms: 1_700_000_030_000,
            latitude: 51.5,
            longitude: -0.1,
            elevation: None,
            speed: Some(50),
            power: Some(10.0),
            odometer: None,
            ideal_battery_range_km: None,
            est_battery_range_km: None,
            rated_battery_range_km: Some(390.0),
            battery_level: Some(78),
            usable_battery_level: Some(77),
            fan_status: None,
            driver_temp_setting: None,
            passenger_temp_setting: None,
            is_climate_on: Some(false),
            is_rear_defroster_on: None,
            is_front_defroster_on: None,
            outside_temp: Some(18.0),
            inside_temp: Some(20.0),
            battery_heater: None,
            battery_heater_on: None,
            battery_heater_no_power: None,
            tpms_pressure_fl: None,
            tpms_pressure_fr: None,
            tpms_pressure_rl: None,
            tpms_pressure_rr: None,
        };
        for id in 20..=1060 {
            let mut position = base_position.clone();
            position.id = id;
            position.date_ms += id;
            stage
                .insert(TeslaMateStageTable::Positions, id, &position)
                .unwrap();
        }
        stage.seal().unwrap();
        let open_session = TeslaMateOpenSession {
            car_id: 1,
            ..TeslaMateOpenSession::default()
        };

        let report = publish_staged_history_with_limits(
            &store,
            &CursorKey::from_bytes([12; 32]),
            &TeslaMateImportRequest {
                source_key: "home-teslamate".into(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms: 1_700_000_000_000,
            },
            &stage,
            &open_session,
            TeslaMateFragmentLimits {
                max_rows_per_fragment: 3,
                max_projected_json_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        let manifest = store
            .manifest_for_vehicle(report.vehicle_id)
            .unwrap()
            .expect("published staged manifest");
        assert_eq!(report.projection.projected_positions, 1_041);
        assert!(manifest.chunk_count < 512);
        assert!(manifest.total_rows > 1_000);
    }

    #[tokio::test]
    async fn native_complete_corpus_publishes_a_durable_manifest_when_configured() {
        let Ok(url) = std::env::var("TESLATLAS_HUB_TEST_POSTGRES_URL") else {
            return;
        };
        let source = ReadOnlySource::parse(&url).expect("credential-free source URL");
        let password =
            TeslaMatePostgresPassword::from_bytes(b"fixture-password").expect("fixture password");
        let data = tempfile::tempdir().expect("Hub data directory");
        let store = HubStore::initialize(data.path()).expect("Hub store");
        let report = import_from_postgres(
            &store,
            &source,
            &password,
            &CursorKey::from_bytes([11; 32]),
            &TeslaMateImportRequest {
                source_key: "native-corpus".into(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms: 1_767_225_600_000,
            },
            TeslaMateReadLimits {
                maximum_rows: 32,
                parallel_copy_lanes: 3,
                ..TeslaMateReadLimits::default()
            },
        )
        .await
        .expect("full native publication");

        // car + drive + 3 positions + charging process + charge + state +
        // update + 2 addresses + geofence
        assert_eq!(report.projected_rows, 13);
        let manifest = store
            .manifest_for_vehicle(report.vehicle_id)
            .expect("manifest query")
            .expect("published manifest");
        assert_eq!(manifest.snapshot_id, report.snapshot_id);
        assert_eq!(manifest.total_rows, 13);
        assert!(!manifest.chunks.is_empty());
        assert_eq!(store.repair().expect("Hub repair check").status, "ok");

        let backup_parent = tempfile::tempdir().expect("backup parent");
        let backup_root = backup_parent.path().join("restored-hub");
        store.backup_to(&backup_root).expect("complete backup");
        let restored = HubStore::initialize(&backup_root).expect("restored Hub store");
        restored.quick_check().expect("restored integrity");
        let restored_manifest = restored
            .manifest_for_vehicle(report.vehicle_id)
            .expect("restored manifest query")
            .expect("restored manifest");
        assert_eq!(restored_manifest, manifest);
        for chunk in restored_manifest.chunks {
            assert!(
                restored
                    .pack_for_digest(chunk.sha256)
                    .expect("restored pack lookup")
                    .expect("restored pack")
                    .path
                    .is_file()
            );
        }
    }
}

#[cfg(test)]
mod open_cutover_tests {
    use super::*;
    use crate::teslamate_projection::{
        TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive, TeslaMatePosition,
        TeslaMateSourceWatermark, TeslaMateSourceWatermarks, TeslaMateState,
    };

    fn drive() -> TeslaMateDrive {
        TeslaMateDrive {
            id: 7,
            car_id: 1,
            start_date_ms: 1_000,
            end_date_ms: None,
            start_position_id: Some(1),
            end_position_id: None,
            start_address_id: None,
            end_address_id: None,
            start_geofence_id: None,
            end_geofence_id: None,
            outside_temp_avg: None,
            inside_temp_avg: None,
            speed_max: Some(20),
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_rated_range_km: None,
            end_rated_range_km: None,
            start_km: Some(10.0),
            end_km: None,
            distance_km: None,
            duration_min: None,
            ascent: None,
            descent: None,
        }
    }

    fn position(id: i64, drive_id: Option<i64>) -> TeslaMatePosition {
        TeslaMatePosition {
            id,
            car_id: 1,
            drive_id,
            date_ms: id * 1_000,
            latitude: 51.0,
            longitude: -0.1,
            elevation: None,
            speed: Some(20),
            power: None,
            odometer: Some(10.0 + id as f64),
            ideal_battery_range_km: None,
            est_battery_range_km: None,
            rated_battery_range_km: None,
            battery_level: Some(80),
            usable_battery_level: None,
            fan_status: None,
            driver_temp_setting: None,
            passenger_temp_setting: None,
            is_climate_on: None,
            is_rear_defroster_on: None,
            is_front_defroster_on: None,
            outside_temp: None,
            inside_temp: None,
            battery_heater: None,
            battery_heater_on: None,
            battery_heater_no_power: None,
            tpms_pressure_fl: None,
            tpms_pressure_fr: None,
            tpms_pressure_rl: None,
            tpms_pressure_rr: None,
        }
    }

    fn process() -> TeslaMateChargingProcess {
        TeslaMateChargingProcess {
            id: 8,
            car_id: 1,
            position_id: None,
            address_id: None,
            geofence_id: None,
            start_date_ms: 1_000,
            end_date_ms: None,
            charge_energy_added: Some(1.0),
            charge_energy_used_kwh: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_battery_level: Some(50),
            end_battery_level: None,
            duration_min: None,
            outside_temp_avg: None,
            start_rated_range_km: None,
            end_rated_range_km: None,
            cost: None,
        }
    }

    fn sample(id: i64) -> TeslaMateCharge {
        TeslaMateCharge {
            id,
            charging_process_id: 8,
            date_ms: id * 1_000,
            battery_heater: None,
            battery_heater_on: None,
            battery_heater_no_power: None,
            battery_level: Some(50),
            usable_battery_level: None,
            charge_energy_added_kwh: Some(id as f64),
            charger_actual_current: None,
            charger_phases: None,
            charger_pilot_current: None,
            charger_power_kw: None,
            charger_voltage: None,
            charge_cable: None,
            fast_charger_present: None,
            fast_charger_brand: None,
            fast_charger_type: None,
            ideal_range_km: None,
            rated_range_km: None,
            not_enough_power_to_heat: None,
            outside_temp_c: None,
        }
    }

    fn state() -> TeslaMateState {
        TeslaMateState {
            id: 20,
            car_id: 1,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: None,
        }
    }

    fn watermarks(position: i64, charge: i64) -> TeslaMateSourceWatermarks {
        let position = TeslaMateSourceWatermark {
            max_id: Some(position),
            max_timestamp_ms: Some(position * 1_000),
        };
        let charge = TeslaMateSourceWatermark {
            max_id: Some(charge),
            max_timestamp_ms: Some(charge * 1_000),
        };
        TeslaMateSourceWatermarks {
            drives: TeslaMateSourceWatermark {
                max_id: Some(7),
                max_timestamp_ms: Some(1_000),
            },
            positions: position,
            charging_processes: TeslaMateSourceWatermark {
                max_id: Some(8),
                max_timestamp_ms: Some(1_000),
            },
            charges: charge,
            states: TeslaMateSourceWatermark {
                max_id: Some(20),
                max_timestamp_ms: Some(1_000),
            },
            updates: TeslaMateSourceWatermark::default(),
        }
    }

    fn open_session(
        position_ids: &[i64],
        sample_ids: &[i64],
        standalone_ids: &[i64],
    ) -> TeslaMateOpenSession {
        TeslaMateOpenSession {
            car_id: 1,
            drive: Some(drive()),
            drive_positions: position_ids
                .iter()
                .map(|id| position(*id, Some(7)))
                .collect(),
            charge: Some(process()),
            charge_samples: sample_ids.iter().map(|id| sample(*id)).collect(),
            state: Some(state()),
            standalone_positions: standalone_ids
                .iter()
                .map(|id| position(*id, None))
                .collect(),
            watermarks: watermarks(
                position_ids.iter().copied().max().unwrap_or_default(),
                sample_ids.iter().copied().max().unwrap_or_default(),
            ),
        }
    }

    #[test]
    fn second_open_tail_is_merged_unsettled_restartable_and_idempotent() {
        let first = open_session(&[1, 2], &[10, 11], &[30]);
        let mut second = open_session(&[2, 3], &[11, 12], &[30, 31]);
        second.watermarks.positions.max_id = Some(999);
        second.watermarks.charges.max_id = Some(999);
        let cutover = reconcile_open_session_cutover(&first, &second).expect("cutover");
        assert!(cutover.cutover_unsettled);
        assert_eq!(cutover.session.drive_positions.len(), 3);
        assert_eq!(cutover.session.charge_samples.len(), 3);
        assert_eq!(cutover.session.standalone_positions.len(), 2);
        assert_eq!(cutover.session.watermarks.positions.max_id, Some(31));
        assert_eq!(cutover.session.watermarks.charges.max_id, Some(12));

        let data = tempfile::tempdir().expect("data");
        let store = HubStore::initialize(data.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("teslamate", "cutover"), 1_000)
            .expect("source");
        let vehicle = store
            .register_vehicle(&VehicleDescriptor::new(source.source_id, "1"), 1_000)
            .expect("vehicle");
        store
            .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 1, &first, 1_000)
            .expect("first seed");
        store
            .seed_imported_open_session(
                source.source_id,
                vehicle.vehicle_id,
                1,
                &cutover.session,
                2_000,
            )
            .expect("second merge");
        let loaded = store
            .load_imported_open_session(source.source_id, vehicle.vehicle_id)
            .expect("load merged")
            .expect("merged session");
        assert_eq!(loaded.drive_positions.len(), 3);
        assert_eq!(loaded.charge_samples.len(), 3);
        assert_eq!(loaded.standalone_positions.len(), 2);
        assert!(
            store
                .seed_imported_open_session(
                    source.source_id,
                    vehicle.vehicle_id,
                    1,
                    &cutover.session,
                    2_000,
                )
                .expect("duplicate merge")
                .no_op
        );

        drop(store);
        let reopened = HubStore::initialize(data.path()).expect("restart");
        let resumed = reopened
            .load_imported_open_session(source.source_id, vehicle.vehicle_id)
            .expect("load after restart")
            .expect("resumed session");
        assert_eq!(resumed.drive_positions.len(), 3);
        assert_eq!(resumed.charge_samples.len(), 3);
        assert_eq!(resumed.standalone_positions.len(), 2);

        let mut invalid = cutover.session.clone();
        invalid.drive_positions[0].car_id = 99;
        assert!(
            reopened
                .seed_imported_open_session(
                    source.source_id,
                    vehicle.vehicle_id,
                    1,
                    &invalid,
                    3_000,
                )
                .is_err()
        );
        let preserved = reopened
            .load_imported_open_session(source.source_id, vehicle.vehicle_id)
            .expect("load after failed merge")
            .expect("preserved session");
        assert_eq!(preserved.drive_positions.len(), 3);
        assert_eq!(preserved.charge_samples.len(), 3);
    }

    #[test]
    fn open_to_closed_cutover_removes_provisional_parent_once() {
        let first = open_session(&[1, 2], &[10, 11], &[30]);
        let second = TeslaMateOpenSession {
            car_id: 1,
            watermarks: watermarks(3, 12),
            ..TeslaMateOpenSession::default()
        };
        let cutover = reconcile_open_session_cutover(&first, &second).expect("close cutover");
        assert!(!cutover.cutover_unsettled);
        assert!(cutover.session.drive.is_none());
        assert!(cutover.session.charge.is_none());
    }
}
