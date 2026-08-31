// SPDX-License-Identifier: AGPL-3.0-only

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
    teslamate_progress::TeslaMateMigrationProgressReporter,
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
        ProductionUpdatesPublication, UpdatesDeliveryError,
        discard_prepared_initial_production_updates_schema_22,
        prepare_initial_production_updates_schema_22_with_gate, production_updates_head,
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

fn reconcile_direct_snapshot_cutover(
    captured: &TeslaMateOpenSession,
    observed_later: &TeslaMateOpenSession,
) -> Result<TeslaMateCutoverReconciliation, TeslaMateImportError> {
    let source_moved = captured != observed_later;
    let mut reconciliation = reconcile_open_session_cutover(captured, observed_later)?;
    // Direct publication is allowed only after two source snapshots are
    // identical. This covers completed-history watermarks, parent updates,
    // child value changes, open states, and short sessions that began and
    // ended between reads, not only newly appended child IDs.
    reconciliation.cutover_unsettled |= source_moved;
    reconciliation.session = captured.clone();
    Ok(reconciliation)
}

fn require_settled_direct_cutover(
    reconciliation: &TeslaMateCutoverReconciliation,
) -> Result<(), TeslaMateImportError> {
    if reconciliation.cutover_unsettled {
        Err(TeslaMateImportError::CutoverUnsettled)
    } else {
        Ok(())
    }
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
    state_limits: TeslaMateProjectionStateLimits,
) -> Result<TeslaMateProjectionStateCapture, TeslaMateDirectError> {
    let state = store.create_import_projection_state(
        publication_gate,
        run_id,
        state_limits,
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
    progress: TeslaMateMigrationProgressReporter,
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
            progress,
            |state_limits| {
                direct_projection_state_capture(
                    store,
                    publication_gate,
                    run_id,
                    vehicle_id,
                    source_id,
                    selected_car_id,
                    false,
                    state_limits,
                )
            },
        )
        .await;
    }
    let capture_factory =
        |state_limits| -> Result<TeslaMateProjectionStateCapture, TeslaMateDirectError> {
            direct_projection_state_capture(
                store,
                publication_gate,
                run_id,
                vehicle_id,
                source_id,
                selected_car_id,
                successor,
                state_limits,
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
            progress,
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
            progress,
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
        store,
        source,
        password,
        cursor_key,
        request,
        limits,
        false,
        false,
        TeslaMateMigrationProgressReporter::default(),
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
        store,
        source,
        password,
        cursor_key,
        request,
        limits,
        false,
        true,
        TeslaMateMigrationProgressReporter::default(),
    )
    .await?;
    finish_selected_schema_22_publication(store, cursor_key, captured)
}

/// Selected-car schema-2.2 import with optional machine-readable progress.
pub async fn import_selected_from_postgres_with_schema_22_and_progress(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    limits: TeslaMateReadLimits,
    progress: TeslaMateMigrationProgressReporter,
) -> Result<TeslaMateSelectedImportReport, TeslaMateImportError> {
    let captured = import_from_postgres_with_updates_capture(
        store, source, password, cursor_key, request, limits, false, true, progress,
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
        store,
        source,
        password,
        cursor_key,
        request,
        limits,
        true,
        true,
        TeslaMateMigrationProgressReporter::default(),
    )
    .await?;
    let legacy_tokens = captured
        .legacy_tokens
        .take()
        .ok_or(TeslaMateImportError::LegacyTokenCaptureMissing)?;
    let report = finish_selected_schema_22_publication(store, cursor_key, captured)?;
    Ok((report, legacy_tokens))
}

/// Token-retaining selected-car import with optional machine-readable progress.
pub async fn import_selected_from_postgres_with_schema_22_and_legacy_token_and_progress(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    limits: TeslaMateReadLimits,
    progress: TeslaMateMigrationProgressReporter,
) -> Result<
    (
        TeslaMateSelectedImportReport,
        TeslaMateLegacyTokenCiphertexts,
    ),
    TeslaMateImportError,
> {
    let mut captured = import_from_postgres_with_updates_capture(
        store, source, password, cursor_key, request, limits, true, true, progress,
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
    atomic_schema_22: Option<ProductionUpdatesPublication>,
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
        atomic_schema_22,
        publication_gate,
    } = captured;
    if let Some(updates_schema_22) = atomic_schema_22 {
        return Ok(TeslaMateSelectedImportReport {
            import: report,
            updates_schema_22,
        });
    }
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
    prepare_schema_22: bool,
    progress: TeslaMateMigrationProgressReporter,
) -> Result<CapturedTeslaMateImport, TeslaMateImportError> {
    tracing::info!(
        host = source.host(),
        port = source.port(),
        database = source.database_name(),
        capture_legacy_token,
        prepare_schema_22,
        "TeslaMate import opening a read-only PostgreSQL snapshot; source rows are never deleted"
    );
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
    // Capture completed history and its open tail from one exported,
    // repeatable-read source snapshot. A later bounded tail read detects
    // movement but can never be mixed into these captured history packs.
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
        progress,
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
    let open_session = first_capture.open_session;
    store.stage_import_generation_session(run_id, &open_session)?;
    let mut direct = first_capture.packs;
    let updates_v2_2 = first_capture.updates_v2_2;
    let legacy_tokens = first_capture.legacy_tokens;
    let second_open_session =
        match read_open_session(source, password, selected_car_id, limits).await {
            Ok(value) => value,
            Err(error) => {
                store.abort_import_generation(run_id)?;
                return Err(error.into());
            }
        };
    let cutover = match reconcile_direct_snapshot_cutover(&open_session, &second_open_session) {
        Ok(value) => value,
        Err(error) => {
            store.abort_import_generation(run_id)?;
            return Err(error);
        }
    };
    // Publish only a tail captured atomically with the selected direct history.
    // Any later movement aborts this unpublished generation so credentials and
    // Hub startup cannot proceed until a bounded retry observes a settled tail.
    require_settled_direct_cutover(&cutover)?;
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
            atomic_schema_22: None,
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
                atomic_schema_22: None,
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
                atomic_schema_22: None,
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
            false,
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
            atomic_schema_22: None,
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
    let prepared_schema_22 = if prepare_schema_22 {
        match prepare_initial_production_updates_schema_22_with_gate(
            store,
            cursor_key,
            &binding,
            updates_v2_2.clone(),
            &publication_gate,
            capture_sequence,
        ) {
            Ok(value) => Some(value),
            Err(error) => {
                store.abort_import_generation(run_id)?;
                return Err(TeslaMateImportError::Schema22Preparation(error));
            }
        }
    } else {
        None
    };
    // A pre-commit failure can leave only unreferenced candidate packs; repair
    // may remove those safely. After the transaction commits they are catalogued.
    let finalization = if let Some(prepared) = prepared_schema_22.as_ref() {
        store.finalize_import_generation_with_projection_state_and_schema_22(
            &publication_gate,
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
            &prepared.manifest,
            &prepared.noop,
        )
    } else {
        store.finalize_import_generation_with_projection_state(
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
            false,
        )
    };
    if let Err(error) = finalization {
        if let Some(prepared) = prepared_schema_22.as_ref()
            && !matches!(
                error,
                crate::db::StoreError::AmbiguousCatalogueCommit { .. }
            )
        {
            discard_prepared_initial_production_updates_schema_22(
                store,
                &publication_gate,
                prepared,
            );
        }
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
        atomic_schema_22: prepared_schema_22.map(|prepared| prepared.publication),
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
        true,
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
        true,
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
        "TeslaMate changed during the bounded cutover snapshot; retry before publishing credentials or starting Hub"
    )]
    CutoverUnsettled,
    #[error(
        "legacy TeslaMate import committed for vehicle {vehicle_id} snapshot {legacy_snapshot_id}, but schema-2.2 publication failed; retry the same selected-car import: {source}"
    )]
    Schema22PostCommit {
        vehicle_id: Uuid,
        legacy_snapshot_id: Uuid,
        #[source]
        source: UpdatesDeliveryError,
    },
    #[error("schema-2.2 publication candidate could not be prepared: {0}")]
    Schema22Preparation(#[source] UpdatesDeliveryError),
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
#[path = "importer/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "importer/open_cutover_tests.rs"]
mod open_cutover_tests;
