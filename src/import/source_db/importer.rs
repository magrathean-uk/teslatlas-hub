// SPDX-License-Identifier: AGPL-3.0-only

//! Source database full-snapshot migration publisher.
//!
//! The PostgreSQL reader is intentionally separate from this module. Once a
//! reviewed, repeatable-read history exists, this module gives it a stable Hub
//! identity, maps only the selected car, and publishes either:
//! - the first immutable full-snapshot base; or
//! - a typed ordered import delta successor bound to that base when history
//!   changes; or an explicitly retained base rotation when that import lineage
//!   has no room for the changed rows. Full captures never masquerade as deltas.

use std::collections::{HashMap, HashSet};

#[path = "performance_profile.rs"]
mod performance_profile;
pub use performance_profile::{
    EffectiveImportProfile, PerformanceProfileError, derive_effective_import_profile,
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    credentials::SourceDbPassword,
    db::{
        HubStore, IMPORT_SOURCE_KIND, PublicationGate, SourceDbIdentityRegistrationCheckpoint,
        SourceDbImportProjectionInventory, SourceDescriptor, StoreError, VehicleDescriptor,
    },
    hub_pack::{
        BuiltProjectionPack, ProjectionBinding, ProjectionCar, ProjectionCharge,
        ProjectionChargeSample, ProjectionDelta, ProjectionDeltaEntity, ProjectionDeltaPackRequest,
        ProjectionDrive, ProjectionPackError, ProjectionPackRequest, ProjectionPackWriter,
        ProjectionPosition, ProjectionState, ProjectionTombstone, ProjectionUpdate,
        signed_full_snapshot_manifest,
    },
    import_projection_state::{
        SourceDbProjectionStateCapture, SourceDbProjectionStateEntity,
        SourceDbProjectionStateError, SourceDbProjectionStateLimits,
    },
    protocol::{
        CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V2, LineageDelta, OpaqueCursor, PROTOCOL_V1,
        ProtocolLimits, SequenceRange, Sha256Digest, canonical_delta_chain_digest,
    },
    source_db_connection::ReadOnlySource,
    source_db_direct::{
        DirectSnapshotCapture, DirectUpdatesSourceV2_2, SourceDbDirectError,
        capture_direct_snapshot_for_legacy_bridge,
        capture_direct_successor_diff_with_projection_state,
        write_direct_full_snapshot_with_projection_state,
    },
    source_db_fragments::{
        SourceDbFragmentLimits, StagedProjectionPacks,
        write_staged_full_snapshot_with_projection_state,
    },
    source_db_progress::SourceDbMigrationProgressReporter,
    source_db_projection::{
        ProjectionReport, SourceDbCar, SourceDbHistory, SourceDbProjection, project_car,
        project_vehicle,
    },
    source_db_projection::{SourceDbOpenSession, SourceDbWatermark},
    source_db_reader::{
        SourceDbReadLimits, read_open_session, read_selected_car,
    },
    source_db_stage::{SourceDbStage, SourceDbStageFormat, SourceDbStageTable},
    updates_delivery::{
        ProductionUpdatesPublication, UpdatesDeliveryError,
        discard_prepared_initial_production_updates_schema_22,
        prepare_initial_production_updates_schema_22_with_gate, production_updates_head,
        publish_production_updates_schema_22_with_gate,
    },
};

/// Import scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDbImportScope {
    Selected(i64),
}

/// Non-secret input that identifies one source database migration source and scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDbImportRequest {
    /// Explicit permission to register another import source.
    pub new_import_source: bool,
    /// Owner-chosen durable label. It must survive a hostname or port change;
    /// it is the stable Hub source key, never a PostgreSQL URL or password.
    pub source_key: String,
    pub scope: SourceDbImportScope,
    pub imported_at_ms: i64,
}

/// Result of a successful immutable snapshot publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDbImportReport {
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub snapshot_id: Uuid,
    pub sequence: u64,
    pub projection: ProjectionReport,
    pub projected_rows: u64,
    pub skipped: bool,
    pub cutover_unsettled: bool,
    /// Source rows this publication did not pack because the import seam
    /// gave them to the Hub's own collection.
    pub seam_skipped: SourceDbSeamSkipped,
}

/// Source rows an import publication leaves out of its packs because of the
/// seam (design M03-SEAM-R3, section 4): drives the plan gave to another row
/// (or an inherited session whose stored row stays) and positions inside the
/// Hub's coverage. Every changed source row the publication read is either
/// packed or counted here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceDbSeamSkipped {
    pub drives: u64,
    pub positions: u64,
}

impl SourceDbSeamSkipped {
    /// All seam-skipped rows.
    pub fn total(&self) -> u64 {
        self.drives.saturating_add(self.positions)
    }
}

impl SourceDbImportReport {
    /// Machine-readable disclosure for the selected THP1 projection. This is
    /// a method rather than a stored field so existing report construction and
    /// callers remain source-compatible.
    pub fn source_parity_report(&self) -> crate::source_db_parity::SourceDbParityReport {
        crate::source_db_parity::SourceDbParityReport::current()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceDbCutoverReconciliation {
    pub session: SourceDbOpenSession,
    pub cutover_unsettled: bool,
}

/// Reconcile two bounded open-session reads by source database row identity.
/// A still-open parent with new child rows is not claimed complete; a parent
/// that disappeared is a valid close transition and the second snapshot wins.
pub fn reconcile_open_session_cutover(
    first: &SourceDbOpenSession,
    second: &SourceDbOpenSession,
) -> Result<SourceDbCutoverReconciliation, SourceDbImportError> {
    first.validate().map_err(SourceDbImportError::Projection)?;
    second.validate().map_err(SourceDbImportError::Projection)?;
    if first.car_id != second.car_id {
        return Err(SourceDbImportError::CutoverCarMismatch);
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
    let standalone_ids: HashSet<_> = first
        .standalone_positions
        .iter()
        .map(|row| row.id)
        .collect();
    let standalone_continues = second
        .standalone_positions
        .iter()
        .any(|row| !standalone_ids.contains(&row.id));
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
    Ok(SourceDbCutoverReconciliation {
        session,
        cutover_unsettled: drive_continues
            || charge_continues
            || standalone_continues
            || drive_parent_changed
            || charge_parent_changed,
    })
}

fn reconcile_direct_snapshot_cutover(
    captured: &SourceDbOpenSession,
    observed_later: &SourceDbOpenSession,
) -> Result<SourceDbCutoverReconciliation, SourceDbImportError> {
    captured
        .validate()
        .map_err(SourceDbImportError::Projection)?;
    observed_later
        .validate()
        .map_err(SourceDbImportError::Projection)?;
    if captured.car_id != observed_later.car_id {
        return Err(SourceDbImportError::CutoverCarMismatch);
    }
    // Direct publication is allowed only after two source snapshots are
    // identical. This covers completed-history watermarks, parent updates,
    // child value changes, open states, and short sessions that began and
    // ended between reads, not only newly appended child IDs.
    Ok(SourceDbCutoverReconciliation {
        cutover_unsettled: captured != observed_later,
        session: captured.clone(),
    })
}

fn require_settled_direct_cutover(
    reconciliation: &SourceDbCutoverReconciliation,
) -> Result<(), SourceDbImportError> {
    if reconciliation.cutover_unsettled {
        Err(SourceDbImportError::CutoverUnsettled)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum CaptureCutoverPolicy {
    RequireSettled,
    HistorySnapshot,
}

async fn settle_captured_session<F, Fut>(
    captured: SourceDbOpenSession,
    policy: CaptureCutoverPolicy,
    observe_later: F,
) -> Result<SourceDbCutoverReconciliation, SourceDbImportError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<
            Output = Result<SourceDbOpenSession, crate::source_db_reader::SourceDbReaderError>,
        >,
{
    captured
        .validate()
        .map_err(SourceDbImportError::Projection)?;
    if matches!(policy, CaptureCutoverPolicy::HistorySnapshot) {
        // This path never transfers Tesla credentials or collector authority.
        // Completed history and open rows already belong to one exported
        // repeatable-read snapshot; later source movement cannot invalidate it.
        return Ok(SourceDbCutoverReconciliation {
            session: captured,
            cutover_unsettled: false,
        });
    }
    let observed_later = observe_later().await?;
    let cutover = reconcile_direct_snapshot_cutover(&captured, &observed_later)?;
    require_settled_direct_cutover(&cutover)?;
    Ok(cutover)
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

impl HasSourceId for crate::source_db_projection::SourceDbDrive {
    fn source_id(&self) -> i64 {
        self.id
    }
}
impl HasSourceId for crate::source_db_projection::SourceDbChargingProcess {
    fn source_id(&self) -> i64 {
        self.id
    }
}

fn has_new_positions(first: &SourceDbOpenSession, second: &SourceDbOpenSession) -> bool {
    let ids: HashSet<_> = first.drive_positions.iter().map(|row| row.id).collect();
    second
        .drive_positions
        .iter()
        .any(|row| !ids.contains(&row.id))
        || second.watermarks.positions.max_id > first.watermarks.positions.max_id
}

fn has_new_samples(first: &SourceDbOpenSession, second: &SourceDbOpenSession) -> bool {
    let ids: HashSet<_> = first.charge_samples.iter().map(|row| row.id).collect();
    second
        .charge_samples
        .iter()
        .any(|row| !ids.contains(&row.id))
        || second.watermarks.charges.max_id > first.watermarks.charges.max_id
}

fn union_positions(
    first: &[crate::source_db_projection::SourceDbPosition],
    second: &[crate::source_db_projection::SourceDbPosition],
) -> Vec<crate::source_db_projection::SourceDbPosition> {
    let mut rows = first.to_vec();
    let mut indices = HashMap::new();
    for (index, row) in first.iter().enumerate() {
        indices.entry(row.id).or_insert(index);
    }
    for row in second {
        if let Some(index) = indices.get(&row.id) {
            rows[*index] = row.clone();
        } else {
            indices.insert(row.id, rows.len());
            rows.push(row.clone());
        }
    }
    rows.sort_by_key(|row| row.id);
    rows
}

fn union_samples(
    first: &[crate::source_db_projection::SourceDbCharge],
    second: &[crate::source_db_projection::SourceDbCharge],
) -> Vec<crate::source_db_projection::SourceDbCharge> {
    let mut rows = first.to_vec();
    let mut indices = HashMap::new();
    for (index, row) in first.iter().enumerate() {
        indices.entry(row.id).or_insert(index);
    }
    for row in second {
        if let Some(index) = indices.get(&row.id) {
            rows[*index] = row.clone();
        } else {
            indices.insert(row.id, rows.len());
            rows.push(row.clone());
        }
    }
    rows.sort_by_key(|row| row.id);
    rows
}

fn observed_open_watermarks(
    session: &SourceDbOpenSession,
) -> crate::source_db_projection::SourceDbWatermarks {
    let positions = session
        .drive_positions
        .iter()
        .chain(session.standalone_positions.iter());
    let max_position_id = positions.clone().map(|row| row.id).max();
    let max_position_timestamp = positions.map(|row| row.date_ms).max();
    crate::source_db_projection::SourceDbWatermarks {
        drives: session
            .drive
            .as_ref()
            .map_or_else(SourceDbWatermark::default, |row| SourceDbWatermark {
                max_id: Some(row.id),
                max_timestamp_ms: Some(row.start_date_ms),
            }),
        positions: SourceDbWatermark {
            max_id: max_position_id,
            max_timestamp_ms: max_position_timestamp,
        },
        charging_processes: session.charge.as_ref().map_or_else(
            SourceDbWatermark::default,
            |row| SourceDbWatermark {
                max_id: Some(row.id),
                max_timestamp_ms: Some(row.start_date_ms),
            },
        ),
        charges: SourceDbWatermark {
            max_id: session.charge_samples.iter().map(|row| row.id).max(),
            max_timestamp_ms: session.charge_samples.iter().map(|row| row.date_ms).max(),
        },
        states: session
            .state
            .as_ref()
            .map_or_else(SourceDbWatermark::default, |row| SourceDbWatermark {
                max_id: Some(row.id),
                max_timestamp_ms: Some(row.start_date_ms),
            }),
        updates: SourceDbWatermark::default(),
    }
}

struct ImportGenerationGuard<'a> {
    store: &'a HubStore,
    run_id: Option<Uuid>,
}

struct SourceDbIdentityRegistrationGuard<'a> {
    store: &'a HubStore,
    checkpoint: Option<SourceDbIdentityRegistrationCheckpoint>,
}

impl SourceDbIdentityRegistrationGuard<'_> {
    fn rollback(&mut self) -> Result<(), StoreError> {
        if let Some(checkpoint) = self.checkpoint.take() {
            self.store
                .rollback_source_db_identity_registration(&checkpoint)?;
        }
        Ok(())
    }

    fn disarm(&mut self) {
        self.checkpoint = None;
    }
}

impl Drop for SourceDbIdentityRegistrationGuard<'_> {
    fn drop(&mut self) {
        if let Some(checkpoint) = self.checkpoint.take()
            && let Err(error) = self
                .store
                .rollback_source_db_identity_registration(&checkpoint)
        {
            tracing::error!(%error, "could not roll back unproved source database identity registration");
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
                tracing::warn!(path = %chunk.path.display(), %error, "could not durably remove unpublished direct source database delta pack");
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

/// Admit one source id (M03-T12): the Hub allocates its own rows from
/// `HUB_ID_BASE` upwards, so a source id at or above the base would share an id
/// with a row the Hub collects, and the import is refused, naming the source
/// table and the id. Every source table keeps 4-byte ids, so the refusal needs
/// a source with over 2^30 ids in one table.
fn admit_source_id(table: &'static str, id: Option<i64>) -> Result<(), SourceDbImportError> {
    match id {
        Some(id) if id >= crate::db::hub_ids::HUB_ID_BASE => {
            Err(SourceDbImportError::InvalidImportGeneration { table, id })
        }
        _ => Ok(()),
    }
}

/// Admit the ids of one captured open session and, through its watermarks,
/// every row of the selected car in the six tables: each watermark is the
/// largest id of its table for that car in the same source snapshot.
pub(crate) fn admit_session_source_ids(
    session: &SourceDbOpenSession,
) -> Result<(), SourceDbImportError> {
    let watermarks = &session.watermarks;
    admit_source_id("drives", watermarks.drives.max_id)?;
    admit_source_id("positions", watermarks.positions.max_id)?;
    admit_source_id("charging_processes", watermarks.charging_processes.max_id)?;
    admit_source_id("charges", watermarks.charges.max_id)?;
    admit_source_id("states", watermarks.states.max_id)?;
    admit_source_id("updates", watermarks.updates.max_id)?;
    admit_source_id("drives", session.drive.as_ref().map(|row| row.id))?;
    admit_source_id(
        "charging_processes",
        session.charge.as_ref().map(|row| row.id),
    )?;
    admit_source_id("states", session.state.as_ref().map(|row| row.id))?;
    for row in session
        .drive_positions
        .iter()
        .chain(session.standalone_positions.iter())
    {
        admit_source_id("positions", Some(row.id))?;
    }
    for row in &session.charge_samples {
        admit_source_id("charges", Some(row.id))?;
    }
    Ok(())
}

/// Admit every row of a whole source history in the six tables.
pub(crate) fn admit_history_source_ids(
    history: &SourceDbHistory,
) -> Result<(), SourceDbImportError> {
    let drives = history.drives.iter().map(|row| row.id).max();
    let positions = history.positions.iter().map(|row| row.id).max();
    let processes = history.charging_processes.iter().map(|row| row.id).max();
    let charges = history.charges.iter().map(|row| row.id).max();
    let states = history.states.iter().map(|row| row.id).max();
    let updates = history.updates.iter().map(|row| row.id).max();
    admit_source_id("drives", drives)?;
    admit_source_id("positions", positions)?;
    admit_source_id("charging_processes", processes)?;
    admit_source_id("charges", charges)?;
    admit_source_id("states", states)?;
    admit_source_id("updates", updates)
}

/// Admit every staged row of the six tables: one keyset seek per table for
/// the first source id at or above the base.
pub(crate) fn admit_stage_source_ids(stage: &SourceDbStage) -> Result<(), SourceDbImportError> {
    for (table, name) in [
        (SourceDbStageTable::Drives, "drives"),
        (SourceDbStageTable::Positions, "positions"),
        (SourceDbStageTable::ChargingProcesses, "charging_processes"),
        (SourceDbStageTable::Charges, "charges"),
        (SourceDbStageTable::States, "states"),
        (SourceDbStageTable::Updates, "updates"),
    ] {
        let page =
            stage.page::<serde_json::Value>(table, crate::db::hub_ids::HUB_ID_BASE - 1, 1)?;
        admit_source_id(name, page.rows.first().map(|row| row.source_id))?;
    }
    Ok(())
}

fn selected_car_id(request: &SourceDbImportRequest) -> Result<i64, SourceDbImportError> {
    match request.scope {
        SourceDbImportScope::Selected(id) if id > 0 => Ok(id),
        SourceDbImportScope::Selected(_) => Err(SourceDbImportError::InvalidSelectedCarId),
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
    state_limits: SourceDbProjectionStateLimits,
) -> Result<SourceDbProjectionStateCapture, SourceDbDirectError> {
    let state = store.create_import_projection_state(
        publication_gate,
        run_id,
        state_limits,
        DIRECT_DELTA_BATCH_PAYLOAD_BYTES,
    )?;
    if !successor {
        return Ok(SourceDbProjectionStateCapture::for_initial_base(state));
    }
    let prior =
        store.source_db_import_projection_state_lookup(vehicle_id, source_id, selected_car_id)?;
    Ok(SourceDbProjectionStateCapture::for_successor(
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
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
    binding: ProjectionBinding,
    capture_snapshot_id: Uuid,
    capture_range: SequenceRange,
    source_id: Uuid,
    vehicle_id: Uuid,
    successor: bool,
    legacy_bridge: bool,
    capture_physical: bool,
    progress: SourceDbMigrationProgressReporter,
) -> Result<DirectSnapshotCapture, SourceDbDirectError> {
    let writer = ProjectionPackWriter::new(store.packs_dir())
        .with_minimum_free_bytes(limits.minimum_free_bytes);
    let imports_dir = store.imports_dir();
    let physical_imports_dir = capture_physical.then_some(imports_dir.as_path());
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
            physical_imports_dir,
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
        |state_limits| -> Result<SourceDbProjectionStateCapture, SourceDbDirectError> {
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
            physical_imports_dir,
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
            physical_imports_dir,
            progress,
            capture_factory,
        )
        .await
    }
}

fn legacy_direct_bridge_error(error: StoreError) -> SourceDbImportError {
    match error {
        StoreError::SourceDbLegacyDirectRebaseRequired(_) => {
            SourceDbImportError::LegacyDirectImportRebaseRequired
        }
        error => error.into(),
    }
}

/// Read, validate, pack, sign, and publish one complete source database snapshot.
/// The caller supplies secrets as explicit local values; no secret is stored
/// in the Hub database or encoded in the generated pack.
pub async fn import_from_postgres(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    limits: SourceDbReadLimits,
) -> Result<SourceDbImportReport, SourceDbImportError> {
    Ok(import_from_postgres_with_updates_capture(
        store,
        source,
        password,
        cursor_key,
        request,
        limits,
        false,
        SourceDbMigrationProgressReporter::default(),
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
    password: &SourceDbPassword,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    limits: SourceDbReadLimits,
) -> Result<SourceDbSelectedImportReport, SourceDbImportError> {
    let captured = import_from_postgres_with_updates_capture(
        store,
        source,
        password,
        cursor_key,
        request,
        limits,
        true,
        SourceDbMigrationProgressReporter::default(),
    )
    .await?;
    finish_selected_schema_22_publication(store, cursor_key, captured)
}

/// Selected-car schema-2.2 import with optional machine-readable progress.
pub async fn import_selected_from_postgres_with_schema_22_and_progress(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    limits: SourceDbReadLimits,
    progress: SourceDbMigrationProgressReporter,
) -> Result<SourceDbSelectedImportReport, SourceDbImportError> {
    let captured = import_from_postgres_with_updates_capture(
        store, source, password, cursor_key, request, limits, true, progress,
    )
    .await?;
    finish_selected_schema_22_publication(store, cursor_key, captured)
}

/// Credential-free history publication from one coherent online source
/// snapshot. Source rows may advance during capture; no collector handover or
/// Tesla token capture occurs, and all other importer routes remain settled.
pub async fn import_selected_history_snapshot_from_postgres_with_schema_22_and_progress(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    limits: SourceDbReadLimits,
    progress: SourceDbMigrationProgressReporter,
) -> Result<SourceDbSelectedImportReport, SourceDbImportError> {
    let captured = import_from_postgres_with_updates_capture_policy(
        store,
        source,
        password,
        cursor_key,
        request,
        limits,
        true,
        progress,
        CaptureCutoverPolicy::HistorySnapshot,
    )
    .await?;
    finish_selected_schema_22_publication(store, cursor_key, captured)
}

#[derive(Debug)]
struct CapturedSourceDbImport {
    report: SourceDbImportReport,
    binding: ProjectionBinding,
    updates_v2_2: DirectUpdatesSourceV2_2,
    atomic_schema_22: Option<ProductionUpdatesPublication>,
    physical_stage: Option<crate::source_db_stage::OwnedSourceDbStage>,
    publication_gate: PublicationGate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDbSelectedImportReport {
    pub import: SourceDbImportReport,
    pub updates_schema_22: ProductionUpdatesPublication,
    pub physical_v3: Option<crate::physical_publication::PhysicalV3Publication>,
}

fn finish_selected_schema_22_publication(
    store: &HubStore,
    cursor_key: &CursorKey,
    captured: CapturedSourceDbImport,
) -> Result<SourceDbSelectedImportReport, SourceDbImportError> {
    let CapturedSourceDbImport {
        report,
        binding,
        updates_v2_2,
        atomic_schema_22,
        physical_stage,
        publication_gate,
    } = captured;
    let vehicle_id = report.vehicle_id;
    let legacy_snapshot_id = report.snapshot_id;
    // Observe the schema-2.2 head only after this command's legacy transaction
    // has committed. The same retained gate has covered source capture and the
    // legacy commit, so no other publisher can interleave before this check.
    let updates_schema_22 = if let Some(publication) = atomic_schema_22 {
        publication
    } else {
        let expected_schema_22_head =
            production_updates_head(store, vehicle_id).map_err(|source| {
                SourceDbImportError::Schema22PostCommit {
                    vehicle_id,
                    legacy_snapshot_id,
                    source,
                }
            })?;

        publish_production_updates_schema_22_with_gate(
            store,
            cursor_key,
            &binding,
            updates_v2_2,
            &publication_gate,
            &expected_schema_22_head,
            Some((legacy_snapshot_id, report.sequence)),
        )
        .map_err(|source| SourceDbImportError::Schema22PostCommit {
            vehicle_id,
            legacy_snapshot_id,
            source,
        })?
    };
    let physical_v3 = physical_stage
        .map(|stage| {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|value| i64::try_from(value.as_millis()).ok())
                .ok_or(crate::physical_publication::SourceDbPhysicalPublicationError::Clock)?;
            crate::physical_publication::publish_sealed_physical_v3_stage_with_gate_at(
                store,
                cursor_key,
                binding,
                stage.take(),
                &publication_gate,
                now_ms,
            )
        })
        .transpose()?;
    Ok(SourceDbSelectedImportReport {
        import: report,
        updates_schema_22,
        physical_v3,
    })
}

async fn import_from_postgres_with_updates_capture(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    limits: SourceDbReadLimits,
    prepare_schema_22: bool,
    progress: SourceDbMigrationProgressReporter,
) -> Result<CapturedSourceDbImport, SourceDbImportError> {
    import_from_postgres_with_updates_capture_policy(
        store,
        source,
        password,
        cursor_key,
        request,
        limits,
        prepare_schema_22,
        progress,
        CaptureCutoverPolicy::RequireSettled,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn import_from_postgres_with_updates_capture_policy(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    limits: SourceDbReadLimits,
    prepare_schema_22: bool,
    progress: SourceDbMigrationProgressReporter,
    cutover_policy: CaptureCutoverPolicy,
) -> Result<CapturedSourceDbImport, SourceDbImportError> {
    tracing::info!(
        host = source.host(),
        port = source.port(),
        database = source.database_name(),
        prepare_schema_22,
        "Source database import opening a read-only PostgreSQL snapshot; source rows are never deleted"
    );
    let publication_gate = store.acquire_publication_gate().await?;
    let selected_car_id = selected_car_id(request)?;
    let car = read_selected_car(source, password, selected_car_id, limits).await?;
    let source_vehicle_key = stable_vehicle_key_for_car(&car)?;
    let source_descriptor = SourceDescriptor::new(IMPORT_SOURCE_KIND, request.source_key.clone());
    let (registered_source, source_created) = store.register_source_db_import_source(
        &source_descriptor,
        request.imported_at_ms,
        request.new_import_source,
    )?;
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
    let (vehicle, registration_checkpoint) = store.provision_source_db_import_identity(
        &registered_source,
        source_created,
        &identity_hint,
        request.imported_at_ms,
        deterministic_vehicle_id,
    )?;
    let mut identity_registration_guard = SourceDbIdentityRegistrationGuard {
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
        && !store.source_db_import_projection_state_exists(vehicle.vehicle_id)?
    {
        if !store.legacy_source_db_direct_bridge_is_eligible(
            vehicle.vehicle_id,
            registered_source.source_id,
            selected_car_id,
        )? {
            return Err(SourceDbImportError::LegacyDirectImportRebaseRequired);
        }
        true
    } else {
        false
    };
    let successor = prior_v2_head.is_some() && !legacy_bridge;
    // Only a successor plans the import seam ahead of its packs; a base or
    // the legacy bridge may not publish over the Hub's own collection.
    if !successor {
        store.refuse_unplanned_import_seam(vehicle.vehicle_id)?;
    }
    let capture_snapshot_id = Uuid::new_v4();
    let capture_sequence = match prior_v2_head {
        Some((_, head_sequence, _)) if legacy_bridge => u64::try_from(head_sequence)
            .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
        _ => store.reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)?,
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
        prepare_schema_22,
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
    if let Err(error) = admit_session_source_ids(&first_capture.open_session) {
        store.abort_import_generation(run_id)?;
        return Err(error);
    }
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
    let materialised_car = first_capture.materialised_car;
    let materialised_drives = first_capture.materialised_drives;
    store.stage_import_generation_session(run_id, &open_session)?;
    let mut direct = first_capture.packs;
    let updates_v2_2 = first_capture.updates_v2_2;
    let physical_stage = first_capture.physical_stage;
    let cutover = match settle_captured_session(open_session, cutover_policy, || {
        read_open_session(source, password, selected_car_id, limits)
    })
    .await
    {
        Ok(value) => value,
        Err(error) => {
            store.abort_import_generation(run_id)?;
            return Err(error);
        }
    };
    // The tail always comes from the history snapshot. Credential/collector
    // handover additionally requires a settled source; explicit history-only
    // publication needs no ownership transfer or later source observation.
    store.stage_import_generation_session(run_id, &cutover.session)?;
    direct.fingerprint = direct_snapshot_fingerprint(&direct.fingerprint, &direct.geofences)?;
    if legacy_bridge {
        let legacy_physical_fingerprint = direct
            .legacy_physical_fingerprint
            .ok_or(SourceDbImportError::LegacyDirectImportRebaseRequired)?;
        let legacy_fingerprint =
            direct_snapshot_fingerprint(&legacy_physical_fingerprint, &direct.geofences)?;
        let mut capture = direct
            .projection_state
            .take()
            .ok_or(SourceDbImportError::LegacyDirectImportRebaseRequired)?;
        capture.seal()?;
        let projection_state = capture.into_state();
        let bridged = store
            .bridge_legacy_source_db_direct_import_with_materialisation(
                run_id,
                registered_source.source_id,
                vehicle.vehicle_id,
                selected_car_id,
                legacy_fingerprint,
                direct.fingerprint,
                &projection_state,
                &materialised_car,
                &materialised_drives,
            )
            .map_err(legacy_direct_bridge_error)?;
        run_guard.disarm();
        return Ok(CapturedSourceDbImport {
            report: SourceDbImportReport {
                source_id: registered_source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id: bridged.snapshot_id,
                sequence: bridged.head_sequence,
                projection: direct.report,
                projected_rows: bridged.total_rows,
                skipped: true,
                cutover_unsettled: cutover.cutover_unsettled,
                seam_skipped: SourceDbSeamSkipped::default(),
            },
            binding: binding.clone(),
            updates_v2_2,
            atomic_schema_22: None,
            physical_stage,
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
            Some((&materialised_car, &materialised_drives)),
        )?;
        run_guard.disarm();
        if let Some((snapshot_id, head_sequence, _)) = store.v2_head(vehicle.vehicle_id)? {
            return Ok(CapturedSourceDbImport {
                report: SourceDbImportReport {
                    source_id: registered_source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    snapshot_id,
                    sequence: u64::try_from(head_sequence)
                        .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
                    projection: direct.report,
                    projected_rows: transport_rows,
                    skipped: true,
                    cutover_unsettled: cutover.cutover_unsettled,
                    seam_skipped: SourceDbSeamSkipped::default(),
                },
                binding: binding.clone(),
                updates_v2_2,
                atomic_schema_22: None,
                physical_stage,
                publication_gate,
            });
        }
        if let Some(current) =
            store.manifest_for_snapshot_fingerprint(vehicle.vehicle_id, direct.fingerprint)?
        {
            return Ok(CapturedSourceDbImport {
                report: SourceDbImportReport {
                    source_id: registered_source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    snapshot_id: current.snapshot_id,
                    sequence: current.head_sequence,
                    projection: direct.report,
                    projected_rows: current.total_rows,
                    skipped: true,
                    cutover_unsettled: cutover.cutover_unsettled,
                    seam_skipped: SourceDbSeamSkipped::default(),
                },
                binding: binding.clone(),
                updates_v2_2,
                atomic_schema_22: None,
                physical_stage,
                publication_gate,
            });
        }
    }
    let mut capture = direct
        .projection_state
        .take()
        .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
    capture.seal()?;
    let mut replacement = None;
    if let Some((base_snapshot_id, _, _)) = prior_v2_head {
        let selected_car = direct
            .selected_car
            .clone()
            .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
        // The seam goes ahead of the packs: its Hub-side changes are applied
        // and published first, then the packs carry the planned rows only.
        let mut planned = plan_direct_successor(
            store,
            &publication_gate,
            cursor_key,
            vehicle.vehicle_id,
            selected_car_id,
            &materialised_drives,
        )?;
        // Decide before discarding the complete capture or writing delta packs.
        // Only pack-capacity exhaustion permits an explicit new base.
        if direct_delta_capacity_exhausted_with_seam(
            &mut capture,
            &binding,
            &selected_car,
            planned.remaining_delta_packs,
            Some(&planned.seam),
        )? {
            let mut event = crate::runtime::development_event_log::Event::new(
                crate::runtime::development_event_log::Kind::CapacityFallback,
                crate::runtime::development_event_log::Outcome::FullBase,
            );
            event.base_sequence = Some(planned.from_sequence);
            event.chunks = planned.existing_pack_count as u64;
            crate::runtime::development_event_log::record(event);
            replacement = Some(planned.existing);
        } else {
            direct.keep_chunks();
            discard_unpublished_chunks(store, &publication_gate, &direct.chunks)?;
            let published = publish_planned_direct_successor(
                store,
                &publication_gate,
                cursor_key,
                PlannedSuccessorRun {
                    run_id,
                    source_id: registered_source.source_id,
                    imported_at_ms: request.imported_at_ms,
                    base_snapshot_id,
                    minimum_free_bytes: limits.minimum_free_bytes,
                    fingerprint: direct.fingerprint,
                    geofences: &direct.geofences,
                    materialised_car: &materialised_car,
                    materialised_drives: &materialised_drives,
                },
                &binding,
                &selected_car,
                capture,
                &mut planned,
            )?;
            run_guard.disarm();
            return Ok(CapturedSourceDbImport {
                report: SourceDbImportReport {
                    source_id: registered_source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    snapshot_id: base_snapshot_id,
                    sequence: published.sequence,
                    projection: direct.report,
                    projected_rows: published.projected_rows,
                    skipped: false,
                    cutover_unsettled: cutover.cutover_unsettled,
                    seam_skipped: planned.seam.skipped,
                },
                binding: binding.clone(),
                updates_v2_2,
                atomic_schema_22: None,
                physical_stage,
                publication_gate,
            });
        }
    }
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
    let prepared_schema_22 = if prepare_schema_22 && replacement.is_none() {
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
                return Err(SourceDbImportError::Schema22Preparation(error));
            }
        }
    } else {
        None
    };
    // A pre-commit failure can leave only unreferenced candidate packs; repair
    // may remove those safely. After the transaction commits they are catalogued.
    let finalization = if let Some(expected) = replacement.as_ref() {
        store.finalize_import_generation_replacing_base_with_materialisation(
            &publication_gate,
            expected,
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
            &materialised_car,
            &materialised_drives,
        )
    } else if let Some(prepared) = prepared_schema_22.as_ref() {
        store.finalize_import_generation_with_projection_state_and_schema_22_and_materialisation(
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
            &materialised_car,
            &materialised_drives,
            &prepared.manifest,
            &prepared.noop,
        )
    } else {
        store.finalize_import_generation_with_projection_state_and_materialisation(
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
            &materialised_car,
            &materialised_drives,
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
    Ok(CapturedSourceDbImport {
        report: SourceDbImportReport {
            source_id: registered_source.source_id,
            vehicle_id: vehicle.vehicle_id,
            snapshot_id: capture_snapshot_id,
            sequence: capture_sequence,
            projection: direct.report,
            projected_rows: manifest.total_rows,
            skipped: false,
            cutover_unsettled: cutover.cutover_unsettled,
            seam_skipped: SourceDbSeamSkipped::default(),
        },
        binding,
        updates_v2_2,
        atomic_schema_22: prepared_schema_22.map(|prepared| prepared.publication),
        physical_stage,
        publication_gate,
    })
}

/// A direct successor after the seam went ahead of its packs.
struct PlannedDirectSuccessor {
    seam: DeltaSeam,
    seam_digest: [u8; 32],
    from_sequence: u64,
    parent_digest: Sha256Digest,
    existing: crate::protocol::LineageManifestV2,
    existing_pack_count: usize,
    remaining_delta_packs: usize,
}

/// Applies the import seam ahead of a direct successor's packs (design
/// M03-SEAM-R3, section 4) and publishes the sync mutations it made, so every
/// head a client can read holds each session once: a Hub drive the source
/// replaces is tombstoned at a head before the packs that carry its
/// replacement, and an earlier import's copy of a skipped drive goes the
/// same way. The packs then chain onto the head that publication left.
fn plan_direct_successor(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    selected_car_id: i64,
    drives: &[ProjectionDrive],
) -> Result<PlannedDirectSuccessor, SourceDbImportError> {
    let ahead =
        store.apply_import_seam_ahead_of_packs(publication_gate, vehicle_id, selected_car_id, drives)?;
    publish_sync_mutations_ahead_of_packs(store, publication_gate, cursor_key, vehicle_id)?;
    let (_, head_sequence, head_digest) = store
        .v2_head(vehicle_id)?
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    let from_sequence =
        u64::try_from(head_sequence).map_err(|_| crate::db::StoreError::InvalidStoredSequence)?;
    let existing = store
        .lineage_manifest_for_vehicle(vehicle_id)?
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    let existing_pack_count = existing
        .base
        .packs
        .len()
        .checked_add(existing.deltas.len())
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    let remaining_delta_packs = ProtocolLimits::default()
        .max_chunks
        .checked_sub(existing_pack_count)
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    Ok(PlannedDirectSuccessor {
        seam: DeltaSeam::new(&ahead, drives),
        seam_digest: ahead.packs.digest(),
        from_sequence,
        parent_digest: head_digest,
        existing,
        existing_pack_count,
        remaining_delta_packs,
    })
}

/// Publishes every unpublished sync mutation of the vehicle, the seam's
/// among them, as live deltas under the publication gate the importer holds.
fn publish_sync_mutations_ahead_of_packs(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
) -> Result<(), SourceDbImportError> {
    const CLAIM_LIMIT: usize = 10_000;
    loop {
        if !store.has_unpublished_sync_mutations(vehicle_id)? {
            return Ok(());
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|value| i64::try_from(value.as_millis()).ok())
            .ok_or_else(|| SourceDbImportError::SeamPublication("the clock is unusable".to_owned()))?;
        let turn = crate::collection::collector::publish_unpublished_sync_mutations(
            store,
            cursor_key,
            vehicle_id,
            now_ms,
            publication_gate,
            CLAIM_LIMIT,
        )
        .map_err(|error| match error {
            crate::collection::collector::CollectorError::Store(error) => error.into(),
            other => SourceDbImportError::SeamPublication(other.to_string()),
        })?;
        if turn.published == 0 {
            // Mutations remain but none could be claimed (another claim
            // holds them): the packs must not go ahead of them.
            return Err(SourceDbImportError::SeamPublication(
                "unpublished sync mutations are claimed elsewhere".to_owned(),
            ));
        }
    }
}

/// The run a planned direct successor belongs to.
struct PlannedSuccessorRun<'a> {
    run_id: Uuid,
    source_id: Uuid,
    imported_at_ms: i64,
    base_snapshot_id: Uuid,
    minimum_free_bytes: u64,
    fingerprint: Sha256Digest,
    geofences: &'a [crate::source_db_projection::SourceDbGeofence],
    materialised_car: &'a ProjectionCar,
    materialised_drives: &'a [ProjectionDrive],
}

/// What a planned direct successor published.
#[derive(Debug)]
struct PublishedDirectSuccessor {
    sequence: u64,
    projected_rows: u64,
}

/// Writes a planned direct successor's delta packs (the planned source rows
/// only) and publishes them; publication plans the seam again and refuses a
/// different plan.
#[allow(clippy::too_many_arguments)]
fn publish_planned_direct_successor(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    run: PlannedSuccessorRun<'_>,
    binding: &ProjectionBinding,
    selected_car: &ProjectionCar,
    mut capture: SourceDbProjectionStateCapture,
    planned: &mut PlannedDirectSuccessor,
) -> Result<PublishedDirectSuccessor, SourceDbImportError> {
    let vehicle_id = binding.vehicle_id;
    let mut next_ordinal = store.next_v2_pack_ordinal(run.base_snapshot_id)?;
    let mut parent_digest = planned.parent_digest;
    let mut prior_sequence = planned.from_sequence;
    // Keep one decoded delta batch at a time.  The projection-state spool
    // owns the complete current history; retaining every decoded sparse
    // row here would turn a changed multi-million-row import back into an
    // in-memory history.
    let mut deltas = Vec::new();
    let mut delta_packs = UnpublishedDirectDeltaPacks::new(store, publication_gate);
    direct_delta_rows_from_capture_with_seam(
        &mut capture,
        binding,
        selected_car,
        planned.remaining_delta_packs,
        Some(&mut planned.seam),
        |batch| {
            let to_sequence = store.reserve_next_full_snapshot_sequence(publication_gate, vehicle_id)?;
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
                .with_minimum_free_bytes(run.minimum_free_bytes)
                .write_delta(&ProjectionDeltaPackRequest {
                    pack_id: Uuid::new_v4(),
                    snapshot_id: run.base_snapshot_id,
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
    store.finalize_import_generation_delta_successors_with_projection_state_and_seam_plan(
        run.run_id,
        run.source_id,
        vehicle_id,
        binding.selected_car_id,
        run.imported_at_ms,
        &deltas,
        cursor_key,
        &terminal_cursor,
        run.fingerprint,
        run.geofences,
        &projection_state,
        run.materialised_car,
        run.materialised_drives,
        planned.seam_digest,
    )?;
    delta_packs.published();
    drop(delta_packs);
    let projected_rows = deltas.iter().try_fold(0_u64, |total, delta| {
        total
            .checked_add(delta.pack.row_count)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)
    })?;
    Ok(PublishedDirectSuccessor {
        sequence: prior_sequence,
        projected_rows,
    })
}

#[cfg(test)]
fn publish_staged_history(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    stage: &SourceDbStage,
) -> Result<SourceDbImportReport, SourceDbImportError> {
    let open_session = SourceDbOpenSession {
        car_id: selected_car_id(request)?,
        ..SourceDbOpenSession::default()
    };
    publish_staged_history_with_limits(
        store,
        cursor_key,
        request,
        stage,
        &open_session,
        SourceDbFragmentLimits::default(),
    )
}

/// Publish a sealed capture and the open-session image read from its same
/// repeatable-read source snapshot.
pub fn publish_staged_history_with_session(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    stage: &SourceDbStage,
    open_session: &SourceDbOpenSession,
) -> Result<SourceDbImportReport, SourceDbImportError> {
    publish_staged_history_with_limits(
        store,
        cursor_key,
        request,
        stage,
        open_session,
        SourceDbFragmentLimits::default(),
    )
}

fn publish_staged_history_with_limits(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    stage: &SourceDbStage,
    open_session: &SourceDbOpenSession,
    fragment_limits: SourceDbFragmentLimits,
) -> Result<SourceDbImportReport, SourceDbImportError> {
    if stage.format()? != SourceDbStageFormat::CompatibilityV2 {
        return Err(crate::source_db_fragments::SourceDbFragmentError::WrongStageFormat.into());
    }
    admit_session_source_ids(open_session)?;
    admit_stage_source_ids(stage)?;
    let publication_gate = store.try_acquire_publication_gate()?;
    let selected_car_id = selected_car_id(request)?;
    let car = stage
        .get::<SourceDbCar>(SourceDbStageTable::Cars, selected_car_id)?
        .ok_or(SourceDbImportError::SelectedCarMissing)?;
    let (source, _) = store.register_source_db_import_source(
        &SourceDescriptor::new(IMPORT_SOURCE_KIND, request.source_key.clone()),
        request.imported_at_ms,
        request.new_import_source,
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
    // Staged publication does not plan the import seam, so it may not publish
    // over the Hub's own collection; refuse before any pack is written.
    store.refuse_unplanned_import_seam(vehicle.vehicle_id)?;
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
            .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?,
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
        .ok_or(SourceDbProjectionStateError::StateCapacityOverflow)?;
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
        || -> Result<SourceDbProjectionStateCapture, SourceDbImportError> {
            let limits = SourceDbProjectionStateLimits {
                max_rows: stage_limits.max_rows,
                max_state_bytes: stage_limits.max_stage_bytes,
                max_changed_payload_bytes: stage_limits.max_stage_bytes,
                minimum_free_bytes: stage_limits.minimum_free_bytes,
            };
            let run_id = import_run
                .run_id
                .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
            let state = store.create_import_projection_state(
                &publication_gate,
                run_id,
                limits,
                DIRECT_DELTA_BATCH_PAYLOAD_BYTES,
            )?;
            if prior_v2_head.is_some() {
                let prior = store.source_db_import_projection_state_lookup(
                    vehicle.vehicle_id,
                    source.source_id,
                    selected_car_id,
                )?;
                return Ok::<SourceDbProjectionStateCapture, SourceDbImportError>(
                    SourceDbProjectionStateCapture::for_successor(state, Box::new(prior)),
                );
            }
            Ok::<SourceDbProjectionStateCapture, SourceDbImportError>(
                SourceDbProjectionStateCapture::for_initial_base(state),
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
                .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?,
            source.source_id,
            vehicle.vehicle_id,
            selected_car_id,
            request.imported_at_ms,
            &mut staged,
            None,
        )?;
        import_run.disarm();
        store.upsert_geofences(vehicle.vehicle_id, &staged.geofences)?;
        if let Some((snapshot_id, head_sequence, _)) = store.v2_head(vehicle.vehicle_id)? {
            return Ok(SourceDbImportReport {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id,
                sequence: u64::try_from(head_sequence)
                    .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
                projection: staged.report,
                projected_rows: transport_rows,
                skipped: true,
                cutover_unsettled: false,
                seam_skipped: SourceDbSeamSkipped::default(),
            });
        }
        if let Some(current) =
            store.manifest_for_snapshot_fingerprint(vehicle.vehicle_id, staged.fingerprint)?
        {
            return Ok(SourceDbImportReport {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id: current.snapshot_id,
                sequence: current.head_sequence,
                projection: staged.report,
                projected_rows: current.total_rows,
                skipped: true,
                cutover_unsettled: false,
                seam_skipped: SourceDbSeamSkipped::default(),
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
        .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
    capture.seal()?;
    let projection_state = capture.into_state();
    if let Err(error) = store.finalize_import_generation_with_projection_state(
        import_run
            .run_id
            .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?,
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
    Ok(SourceDbImportReport {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        snapshot_id,
        sequence,
        projection: staged.report,
        projected_rows: manifest.total_rows,
        skipped: false,
        cutover_unsettled: false,
        seam_skipped: SourceDbSeamSkipped::default(),
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_staged_history_successor(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    source_id: Uuid,
    vehicle_id: Uuid,
    selected_car_id: i64,
    binding: ProjectionBinding,
    publication_gate: PublicationGate,
    fingerprint: Sha256Digest,
    minimum_free_bytes: u64,
    mut staged: StagedProjectionPacks,
    mut run_guard: ImportGenerationGuard<'_>,
) -> Result<SourceDbImportReport, SourceDbImportError> {
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
        return Err(SourceDbImportError::DirectDeltaBatchLimitExceeded {
            maximum: remaining_delta_packs,
        });
    }
    let selected_car = staged
        .selected_car
        .clone()
        .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
    let mut capture = staged
        .projection_state
        .take()
        .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
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
        .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
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
    Ok(SourceDbImportReport {
        source_id,
        vehicle_id,
        snapshot_id: base_snapshot_id,
        sequence: prior_sequence,
        projection: staged.report,
        projected_rows: deltas.iter().try_fold(0_u64, |total, delta| {
            total
                .checked_add(delta.pack.row_count)
                .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)
        })?,
        skipped: false,
        cutover_unsettled: false,
        seam_skipped: SourceDbSeamSkipped::default(),
    })
}

/// Publish an already-read source history. This seam makes the pack/identity
/// path deterministic and testable without a live PostgreSQL server.
fn source_db_projection_inventory(
    projected: &SourceDbProjection,
    selected_car_id: i64,
) -> Result<Vec<ProjectionTombstone>, SourceDbImportError> {
    if selected_car_id <= 0 {
        return Err(SourceDbImportError::InvalidSelectedCarId);
    }
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |entity: ProjectionDeltaEntity, id: i64| -> Result<(), SourceDbImportError> {
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

fn source_db_inventory_tombstones(
    previous: &[ProjectionTombstone],
    current: &[ProjectionTombstone],
) -> Result<Vec<ProjectionTombstone>, SourceDbImportError> {
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

fn same_source_db_inventory(left: &[ProjectionTombstone], right: &[ProjectionTombstone]) -> bool {
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
    request: &SourceDbImportRequest,
    history: &SourceDbHistory,
) -> Result<SourceDbImportReport, SourceDbImportError> {
    admit_history_source_ids(history)?;
    let publication_gate = store.try_acquire_publication_gate()?;
    let selected_car_id = selected_car_id(request)?;
    let projected = project_vehicle(history, selected_car_id)?;
    let inventory_rows = source_db_projection_inventory(&projected, selected_car_id)?;
    let fingerprint = source_history_fingerprint(history, selected_car_id)?;
    let car = projected
        .snapshot
        .cars
        .first()
        .expect("projection guarantees one selected car");
    let (source, _) = store.register_source_db_import_source(
        &SourceDescriptor::new(IMPORT_SOURCE_KIND, request.source_key.clone()),
        request.imported_at_ms,
        request.new_import_source,
    )?;
    let source_vehicle_key = stable_vehicle_key_for_car(
        history
            .cars
            .iter()
            .find(|candidate| candidate.id == selected_car_id)
            .ok_or(SourceDbImportError::SelectedCarMissing)?,
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
    let inventory = SourceDbImportProjectionInventory {
        source_id: source.source_id,
        selected_car_id,
        rows: inventory_rows,
    };
    // The durable Hub vehicle key is a Tesla EID or VIN, not the mutable
    // Source database primary key. Persist the imported car record so a later
    // typed successor derives the same selected car ID as the immutable base.
    store.persist_materialised_car_if_absent(vehicle.vehicle_id, car)?;
    if store.source_fingerprint_matches(vehicle.vehicle_id, fingerprint)? {
        if let Some((snapshot_id, head_sequence, _)) = store.v2_head(vehicle.vehicle_id)? {
            let published_inventory = store.source_db_import_projection_inventory(
                vehicle.vehicle_id,
                source.source_id,
                selected_car_id,
            )?;
            if !same_source_db_inventory(&published_inventory.rows, &inventory.rows) {
                return Err(crate::db::StoreError::LineageCatalogConflict.into());
            }
            return Ok(SourceDbImportReport {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id,
                sequence: u64::try_from(head_sequence)
                    .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
                projection: projected.report,
                projected_rows: projected.report.logical_row_count().unwrap_or(0),
                skipped: true,
                cutover_unsettled: false,
                seam_skipped: SourceDbSeamSkipped::default(),
            });
        }
        if let Some(current) =
            store.manifest_for_snapshot_fingerprint(vehicle.vehicle_id, fingerprint)?
        {
            return Ok(SourceDbImportReport {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id: current.snapshot_id,
                sequence: current.head_sequence,
                projection: projected.report,
                projected_rows: current.total_rows,
                skipped: true,
                cutover_unsettled: false,
                seam_skipped: SourceDbSeamSkipped::default(),
            });
        }
    }
    // An in-memory history does not plan the import seam, so it may not
    // publish over the Hub's own collection; refuse before any pack is written.
    store.refuse_unplanned_import_seam(vehicle.vehicle_id)?;
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
    store.finalize_source_db_import_snapshot(
        &manifest,
        fingerprint,
        &history.geofences,
        &binding,
        &inventory,
    )?;

    Ok(SourceDbImportReport {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        snapshot_id,
        sequence,
        projection: projected.report,
        projected_rows: manifest.total_rows,
        skipped: false,
        cutover_unsettled: false,
        seam_skipped: SourceDbSeamSkipped::default(),
    })
}

/// After an immutable base exists, changed source database history is published as a
/// typed delta under the base snapshot identity — never as a second base.
fn publish_history_as_import_delta(
    store: &HubStore,
    publication_gate: &crate::db::PublicationGate,
    cursor_key: &CursorKey,
    source: &crate::db::SourceRecord,
    vehicle_id: Uuid,
    selected_car_id: i64,
    projected: &SourceDbProjection,
    fingerprint: Sha256Digest,
    geofences: &[crate::source_db_projection::SourceDbGeofence],
    inventory: &SourceDbImportProjectionInventory,
) -> Result<SourceDbImportReport, SourceDbImportError> {
    let (base_snapshot_id, from_sequence_i64, parent_digest) = store
        .v2_head(vehicle_id)?
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    let from_sequence = u64::try_from(from_sequence_i64)
        .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?;
    let prior_inventory = store.source_db_import_projection_inventory(
        vehicle_id,
        source.source_id,
        selected_car_id,
    )?;
    let tombstones = source_db_inventory_tombstones(&prior_inventory.rows, &inventory.rows)?;
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
        return Ok(SourceDbImportReport {
            source_id: source.source_id,
            vehicle_id,
            snapshot_id: base_snapshot_id,
            sequence: from_sequence,
            projection: projected.report,
            projected_rows: projected.report.logical_row_count().unwrap_or(0),
            skipped: true,
            cutover_unsettled: false,
            seam_skipped: SourceDbSeamSkipped::default(),
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
    store.finalize_source_db_import_delta_successor(
        vehicle_id,
        &lineage_delta,
        cursor_key,
        &terminal_cursor,
        fingerprint,
        geofences,
        inventory,
    )?;
    Ok(SourceDbImportReport {
        source_id: source.source_id,
        vehicle_id,
        snapshot_id: base_snapshot_id,
        sequence: to_sequence,
        projection: projected.report,
        projected_rows: projected.report.logical_row_count().unwrap_or(0),
        skipped: false,
        cutover_unsettled: false,
        seam_skipped: SourceDbSeamSkipped::default(),
    })
}

fn stable_vehicle_key_for_car(car: &SourceDbCar) -> Result<String, SourceDbImportError> {
    let eid = car.eid;
    if eid > 0 {
        return Ok(format!("eid:{eid}"));
    }
    if let Some(vin) = nonblank(car.vin.as_deref()) {
        return Ok(format!("vin:{vin}"));
    }
    Err(SourceDbImportError::StableVehicleIdentityMissing)
}

fn validate_exported_vehicle_identity(
    registered_car: &SourceDbCar,
    exported: &DirectUpdatesSourceV2_2,
) -> Result<(), SourceDbImportError> {
    if i64::from(exported.car.id) != registered_car.id
        || exported.car.eid != registered_car.eid
        || Some(exported.car.vid) != registered_car.vid
        || exported.car.vin != registered_car.vin
    {
        return Err(SourceDbImportError::SourceVehicleIdentityChangedDuringCapture);
    }
    Ok(())
}

fn nonblank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn source_history_fingerprint(
    history: &SourceDbHistory,
    selected_car_id: i64,
) -> Result<Sha256Digest, SourceDbImportError> {
    let canonical = serde_json::to_vec(&(selected_car_id, history))?;
    let mut digest = Sha256::new();
    digest.update(crate::runtime::legacy_names::SOURCE_HISTORY_V2_STANDALONE_POSITIONS.as_bytes());
    digest.update(
        u64::try_from(canonical.len())
            .map_err(|_| SourceDbImportError::SourceFingerprintTooLarge)?
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
    geofences: &[crate::source_db_projection::SourceDbGeofence],
) -> Result<Sha256Digest, SourceDbImportError> {
    let canonical = serde_json::to_vec(geofences)?;
    let mut digest = Sha256::new();
    digest.update(crate::runtime::legacy_names::DIRECT_SNAPSHOT_WITH_GEOFENCES_V1.as_bytes());
    digest.update(snapshot_fingerprint.as_bytes());
    digest.update(
        u64::try_from(canonical.len())
            .map_err(|_| SourceDbImportError::SourceFingerprintTooLarge)?
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
    crate::import_projection_state::DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES;
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

    fn next_would_exceed(&self, payload_bytes: u64) -> Result<bool, SourceDbImportError> {
        let next_rows = self
            .row_count
            .checked_add(1)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
        let next_bytes = self
            .payload_bytes
            .checked_add(payload_bytes)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
        Ok(next_rows > DIRECT_DELTA_BATCH_ROWS || next_bytes > DIRECT_DELTA_BATCH_PAYLOAD_BYTES)
    }

    fn reserve(&mut self, payload_bytes: u64) -> Result<(), SourceDbImportError> {
        self.row_count = self
            .row_count
            .checked_add(1)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(payload_bytes)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
        Ok(())
    }
}

/// Visit bounded sparse row batches from a sealed current-run spool. The
/// visitor owns each decoded batch before the next page is read, keeping the
/// importer bounded even when every source row changed. It also enforces the
/// remaining protocol pack capacity before an unbounded number of candidate
/// pack files can be constructed.
#[cfg(test)]
fn direct_delta_capacity_exhausted(
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    car: &ProjectionCar,
    maximum_batches: usize,
) -> Result<bool, SourceDbImportError> {
    direct_delta_capacity_exhausted_with_seam(capture, binding, car, maximum_batches, None)
}

fn direct_delta_capacity_exhausted_with_seam(
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    car: &ProjectionCar,
    maximum_batches: usize,
    seam: Option<&DeltaSeam>,
) -> Result<bool, SourceDbImportError> {
    // A dry run: the seam's counts belong to the real run.
    let mut seam = seam.cloned();
    match direct_delta_rows_from_capture_with_seam(
        capture,
        binding,
        car,
        maximum_batches,
        seam.as_mut(),
        |_| Ok(()),
    ) {
        Ok(()) => Ok(false),
        Err(SourceDbImportError::DirectDeltaBatchLimitExceeded { .. }) => Ok(true),
        Err(error) => Err(error),
    }
}

/// The seam plan applied to one successor's delta rows (design M03-SEAM-R3,
/// section 4): the packs carry the planned source rows only. A skipped drive
/// is left out, a packed drive carries the bounds the store writes, a source
/// position inside coverage is left out and one whose drive was skipped is
/// packed under the drive that stands for it. Packed drives whose stored row
/// is not yet the planned row are restated even when the source did not
/// change them, so the packs and the store hold the same drives.
#[derive(Debug, Clone)]
struct DeltaSeam {
    packs: crate::import::source_db::seam::SeamPackPlan,
    restated: Vec<ProjectionDrive>,
    emitted_drives: HashSet<i64>,
    skipped: SourceDbSeamSkipped,
}

impl DeltaSeam {
    fn new(ahead: &crate::db::ImportSeamAhead, drives: &[ProjectionDrive]) -> Self {
        use crate::import::source_db::seam::PackedSession;
        let restated_ids: HashSet<i64> = ahead.restated_drives.iter().copied().collect();
        let restated = drives
            .iter()
            .filter(|drive| restated_ids.contains(&drive.id))
            .filter_map(|drive| match ahead.packs.drive(drive.id) {
                Some(PackedSession::Keep { bounds }) => {
                    Some(crate::db::planned_import_drive(drive, bounds))
                }
                _ => None,
            })
            .collect();
        Self {
            packs: ahead.packs.clone(),
            restated,
            emitted_drives: HashSet::new(),
            skipped: SourceDbSeamSkipped::default(),
        }
    }
}

fn direct_delta_rows_from_capture<F>(
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    metadata_only_car: &ProjectionCar,
    maximum_batches: usize,
    visit: F,
) -> Result<(), SourceDbImportError>
where
    F: FnMut(DirectDeltaRows) -> Result<(), SourceDbImportError>,
{
    direct_delta_rows_from_capture_with_seam(
        capture,
        binding,
        metadata_only_car,
        maximum_batches,
        None,
        visit,
    )
}

/// As [`direct_delta_rows_from_capture`], with the seam plan applied to the
/// rows when one is given.
fn direct_delta_rows_from_capture_with_seam<F>(
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    metadata_only_car: &ProjectionCar,
    maximum_batches: usize,
    mut seam: Option<&mut DeltaSeam>,
    mut visit: F,
) -> Result<(), SourceDbImportError>
where
    F: FnMut(DirectDeltaRows) -> Result<(), SourceDbImportError>,
{
    if maximum_batches == 0 {
        return Err(SourceDbImportError::DirectDeltaBatchLimitExceeded {
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
                .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
            if payload_bytes > DIRECT_DELTA_BATCH_PAYLOAD_BYTES {
                return Err(SourceDbImportError::DirectDeltaRowTooLarge { payload_bytes });
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
            append_changed_direct_delta_row(&mut current, &row, binding, seam.as_deref_mut())?;
        }
        match page.next_after {
            Some(next_after) => changed_after = Some(next_after),
            None => break,
        }
    }

    // Packed drives the source did not change but the store does not yet
    // hold as planned.
    if let Some(seam) = seam.as_deref_mut() {
        for drive in &seam.restated {
            if seam.emitted_drives.contains(&drive.id) {
                continue;
            }
            let payload_bytes = u64::try_from(
                serde_json::to_vec(drive)
                    .map_err(SourceDbImportError::SerializeDirectDeltaRow)?
                    .len(),
            )
            .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
            if payload_bytes > DIRECT_DELTA_BATCH_PAYLOAD_BYTES {
                return Err(SourceDbImportError::DirectDeltaRowTooLarge { payload_bytes });
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
            if drive.car_id != binding.selected_car_id || drive.id <= 0 {
                return Err(SourceDbImportError::InvalidDirectDeltaRow);
            }
            current.reserve(payload_bytes)?;
            current.drives.push(drive.clone());
        }
    }

    let mut tombstone_after = None;
    loop {
        let (rows, next_after) = capture.tombstone_page(tombstone_after, DIRECT_DELTA_PAGE_ROWS)?;
        for row in rows {
            let payload_bytes = u64::try_from(
                serde_json::to_vec(&row)
                    .map_err(SourceDbImportError::SerializeDirectDeltaRow)?
                    .len(),
            )
            .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
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
                return Err(SourceDbImportError::InvalidDirectDeltaTombstone);
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
                .map_err(SourceDbImportError::SerializeDirectDeltaRow)?
                .len(),
        )
        .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
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
) -> Result<(), SourceDbImportError>
where
    F: FnMut(DirectDeltaRows) -> Result<(), SourceDbImportError>,
{
    if *emitted_batches >= maximum_batches {
        return Err(SourceDbImportError::DirectDeltaBatchLimitExceeded {
            maximum: maximum_batches,
        });
    }
    visit(batch)?;
    *emitted_batches = emitted_batches
        .checked_add(1)
        .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
    Ok(())
}

fn append_changed_direct_delta_row(
    batch: &mut DirectDeltaRows,
    row: &crate::import_projection_state::SourceDbProjectionStateChangedRow,
    binding: &ProjectionBinding,
    seam: Option<&mut DeltaSeam>,
) -> Result<(), SourceDbImportError> {
    use crate::import::source_db::seam::{ChildRow, PackedChild, PackedSession};
    let payload_bytes = u64::try_from(row.canonical_payload.len())
        .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
    match row.state.entity {
        SourceDbProjectionStateEntity::Car => {
            let value: ProjectionCar = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.cars.push(value);
        }
        SourceDbProjectionStateEntity::Drive => {
            let mut value: ProjectionDrive = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            if let Some(seam) = seam {
                match seam.packs.drive(value.id) {
                    None => return Err(SourceDbImportError::InvalidDirectDeltaRow),
                    Some(PackedSession::Skip) => {
                        seam.skipped.drives = seam.skipped.drives.saturating_add(1);
                        return Ok(());
                    }
                    Some(PackedSession::Keep { bounds }) => {
                        value = crate::db::planned_import_drive(&value, bounds);
                        seam.emitted_drives.insert(value.id);
                    }
                }
            }
            batch.reserve(payload_bytes)?;
            batch.drives.push(value);
        }
        SourceDbProjectionStateEntity::Position => {
            let mut value: ProjectionPosition = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            if let Some(seam) = seam {
                let child = ChildRow {
                    id: value.id,
                    at_ms: value.date_ms,
                    parent: value.drive_id,
                };
                match seam.packs.position(child) {
                    PackedChild::Skip => {
                        seam.skipped.positions = seam.skipped.positions.saturating_add(1);
                        return Ok(());
                    }
                    PackedChild::Keep { parent } => value.drive_id = parent,
                }
            }
            batch.reserve(payload_bytes)?;
            batch.positions.push(value);
        }
        SourceDbProjectionStateEntity::Charge => {
            let value: ProjectionCharge = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.charges.push(value);
        }
        SourceDbProjectionStateEntity::ChargeSample => {
            let value: ProjectionChargeSample = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, row.state.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.charge_samples.push(value);
        }
        SourceDbProjectionStateEntity::State => {
            let value: ProjectionState = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.states.push(value);
        }
        SourceDbProjectionStateEntity::Update => {
            let value: ProjectionUpdate = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
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
    row: &crate::import_projection_state::SourceDbProjectionStateChangedRow,
    binding: &ProjectionBinding,
) -> Result<(), SourceDbImportError> {
    if id != row.state.id
        || car_id != row.state.car_id
        || car_id != binding.selected_car_id
        || id <= 0
    {
        return Err(SourceDbImportError::InvalidDirectDeltaRow);
    }
    Ok(())
}

fn discard_unpublished_chunks(
    store: &HubStore,
    publication_gate: &PublicationGate,
    chunks: &[BuiltProjectionPack],
) -> Result<(), SourceDbImportError> {
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
) -> Result<(), SourceDbImportError> {
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
    materialisation: Option<(&ProjectionCar, &[ProjectionDrive])>,
) -> Result<crate::db::OpenSessionSeedReport, SourceDbImportError> {
    // A full direct capture cannot become a delta. Keep the candidate from
    // removing the files on drop, then remove only packs which no catalogue
    // entry references before the no-publication generation promotion.
    direct.keep_chunks();
    discard_unpublished_chunks(store, publication_gate, &direct.chunks)?;
    Ok(match materialisation {
        Some((car, drives)) => store.promote_import_generation_with_materialisation(
            run_id,
            source_id,
            vehicle_id,
            car_id,
            updated_at_ms,
            car,
            drives,
        )?,
        None => {
            store.promote_import_generation(run_id, source_id, vehicle_id, car_id, updated_at_ms)?
        }
    })
}

fn transport_row_count(chunks: &[BuiltProjectionPack]) -> Result<u64, ProjectionPackError> {
    chunks.iter().try_fold(0_u64, |total, chunk| {
        total
            .checked_add(chunk.metadata.row_count)
            .ok_or(ProjectionPackError::ManifestTotalsOverflow)
    })
}

#[derive(Debug, Error)]
pub enum SourceDbImportError {
    #[error(
        "physical V3 publication failed after legacy import; retry the selected-car import: {0}"
    )]
    PhysicalPublication(#[from] crate::physical_publication::SourceDbPhysicalPublicationError),
    #[error("Source database selected car id must be positive")]
    InvalidSelectedCarId,
    #[error(
        "import generation is invalid: source {table} id {id} lies in the Hub's reserved id range"
    )]
    InvalidImportGeneration { table: &'static str, id: i64 },
    #[error("Source database selected car disappeared before publication")]
    SelectedCarMissing,
    #[error("direct source database capture did not retain its required projection state")]
    ProjectionStateCaptureMissing,
    #[error(
        "existing source database direct-import base cannot be proved unchanged; rebase_required"
    )]
    LegacyDirectImportRebaseRequired,
    #[error("direct source database delta batch accounting overflowed")]
    DirectDeltaBatchOverflow,
    #[error("direct source database delta requires more than {maximum} remaining protocol packs")]
    DirectDeltaBatchLimitExceeded { maximum: usize },
    #[error("direct source database delta row is too large ({payload_bytes} bytes)")]
    DirectDeltaRowTooLarge { payload_bytes: u64 },
    #[error("cannot decode canonical direct source database delta row: {0}")]
    DecodeDirectDeltaRow(serde_json::Error),
    #[error("cannot encode direct source database delta row: {0}")]
    SerializeDirectDeltaRow(serde_json::Error),
    #[error("direct source database delta row does not match its selected-car state identity")]
    InvalidDirectDeltaRow,
    #[error("direct source database delta tombstone is invalid")]
    InvalidDirectDeltaTombstone,
    #[error("Source database snapshot is too large to fingerprint")]
    SourceFingerprintTooLarge,
    #[error("cannot serialize source database snapshot fingerprint: {0}")]
    SourceFingerprint(#[from] serde_json::Error),
    #[error("Source database selected car has neither a VIN nor a valid EID")]
    StableVehicleIdentityMissing,
    #[error(
        "Source database selected-car VIN/EID/VID changed before the exported snapshot capture"
    )]
    SourceVehicleIdentityChangedDuringCapture,
    #[error("Source database cutover snapshots belong to different cars")]
    CutoverCarMismatch,
    #[error(
        "Source database changed during the bounded cutover snapshot; retry before publishing credentials or starting Hub"
    )]
    CutoverUnsettled,
    #[error(
        "legacy source database import committed for vehicle {vehicle_id} snapshot {legacy_snapshot_id}, but schema-2.2 publication failed; retry the same selected-car import: {source}"
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
    Reader(#[from] crate::source_db_reader::SourceDbReaderError),
    #[error(transparent)]
    Direct(#[from] crate::source_db_direct::SourceDbDirectError),
    #[error(transparent)]
    Stage(#[from] crate::source_db_stage::SourceDbStageError),
    #[error(transparent)]
    Projection(#[from] crate::source_db_projection::SourceDbProjectionError),
    #[error(transparent)]
    Store(#[from] crate::db::StoreError),
    #[error(transparent)]
    Pack(#[from] crate::hub_pack::ProjectionPackError),
    #[error(transparent)]
    Fragments(#[from] crate::source_db_fragments::SourceDbFragmentError),
    #[error(transparent)]
    ProjectionState(#[from] SourceDbProjectionStateError),
    #[error("the import seam's own changes could not be published ahead of the import packs: {0}")]
    SeamPublication(String),
}

#[cfg(test)]
#[path = "importer/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "importer/open_cutover_tests.rs"]
mod open_cutover_tests;

#[cfg(test)]
mod n1_src_import_source_db_importer_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for SourceDbImportError {
        fn error_example() -> Self {
            SourceDbImportError::InvalidSelectedCarId
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_source_db_import_error(error: SourceDbImportError) {
            match &error {
                SourceDbImportError::PhysicalPublication(..) => (),
                SourceDbImportError::InvalidSelectedCarId => (),
                SourceDbImportError::InvalidImportGeneration { .. } => (),
                SourceDbImportError::SelectedCarMissing => (),
                SourceDbImportError::ProjectionStateCaptureMissing => (),
                SourceDbImportError::LegacyDirectImportRebaseRequired => (),
                SourceDbImportError::DirectDeltaBatchOverflow => (),
                SourceDbImportError::DirectDeltaBatchLimitExceeded { .. } => (),
                SourceDbImportError::DirectDeltaRowTooLarge { .. } => (),
                SourceDbImportError::DecodeDirectDeltaRow(..) => (),
                SourceDbImportError::SerializeDirectDeltaRow(..) => (),
                SourceDbImportError::InvalidDirectDeltaRow => (),
                SourceDbImportError::InvalidDirectDeltaTombstone => (),
                SourceDbImportError::SourceFingerprintTooLarge => (),
                SourceDbImportError::SourceFingerprint(..) => (),
                SourceDbImportError::StableVehicleIdentityMissing => (),
                SourceDbImportError::SourceVehicleIdentityChangedDuringCapture => (),
                SourceDbImportError::CutoverCarMismatch => (),
                SourceDbImportError::CutoverUnsettled => (),
                SourceDbImportError::Schema22PostCommit { .. } => (),
                SourceDbImportError::Schema22Preparation(..) => (),
                SourceDbImportError::Reader(..) => (),
                SourceDbImportError::Direct(..) => (),
                SourceDbImportError::Stage(..) => (),
                SourceDbImportError::Projection(..) => (),
                SourceDbImportError::Store(..) => (),
                SourceDbImportError::Pack(..) => (),
                SourceDbImportError::Fragments(..) => (),
                SourceDbImportError::ProjectionState(..) => (),
                SourceDbImportError::SeamPublication(..) => (),
            }
            assert_name_free(&error);
        }
        check_source_db_import_error(SourceDbImportError::PhysicalPublication(example()));
        check_source_db_import_error(SourceDbImportError::InvalidSelectedCarId);
        check_source_db_import_error(SourceDbImportError::InvalidImportGeneration {
            table: "drives",
            id: crate::db::hub_ids::HUB_ID_BASE,
        });
        check_source_db_import_error(SourceDbImportError::SelectedCarMissing);
        check_source_db_import_error(SourceDbImportError::ProjectionStateCaptureMissing);
        check_source_db_import_error(SourceDbImportError::LegacyDirectImportRebaseRequired);
        check_source_db_import_error(SourceDbImportError::DirectDeltaBatchOverflow);
        check_source_db_import_error(SourceDbImportError::DirectDeltaBatchLimitExceeded {
            maximum: example(),
        });
        check_source_db_import_error(SourceDbImportError::DirectDeltaRowTooLarge {
            payload_bytes: example(),
        });
        check_source_db_import_error(SourceDbImportError::DecodeDirectDeltaRow(example()));
        check_source_db_import_error(SourceDbImportError::SerializeDirectDeltaRow(example()));
        check_source_db_import_error(SourceDbImportError::InvalidDirectDeltaRow);
        check_source_db_import_error(SourceDbImportError::InvalidDirectDeltaTombstone);
        check_source_db_import_error(SourceDbImportError::SourceFingerprintTooLarge);
        check_source_db_import_error(SourceDbImportError::SourceFingerprint(example()));
        check_source_db_import_error(SourceDbImportError::StableVehicleIdentityMissing);
        check_source_db_import_error(
            SourceDbImportError::SourceVehicleIdentityChangedDuringCapture,
        );
        check_source_db_import_error(SourceDbImportError::CutoverCarMismatch);
        check_source_db_import_error(SourceDbImportError::CutoverUnsettled);
        check_source_db_import_error(SourceDbImportError::Schema22PostCommit {
            vehicle_id: example(),
            legacy_snapshot_id: example(),
            source: example(),
        });
        check_source_db_import_error(SourceDbImportError::Schema22Preparation(example()));
        check_source_db_import_error(SourceDbImportError::Reader(example()));
        check_source_db_import_error(SourceDbImportError::Direct(example()));
        check_source_db_import_error(SourceDbImportError::Stage(example()));
        check_source_db_import_error(SourceDbImportError::Projection(example()));
        check_source_db_import_error(SourceDbImportError::Store(example()));
        check_source_db_import_error(SourceDbImportError::Pack(example()));
        check_source_db_import_error(SourceDbImportError::Fragments(example()));
        check_source_db_import_error(SourceDbImportError::ProjectionState(example()));
        check_source_db_import_error(SourceDbImportError::SeamPublication(
            "the publication gate is held".to_owned(),
        ));
    }
}

/// The import seam ahead of the packs (design M03-SEAM-R3, section 6,
/// M03-T16): the importer applies the plan before it writes packs, the
/// packs carry the planned source rows only, the Hub's own rows and the
/// seam's tombstones travel as sync, and every head a client can read holds
/// each session once.
#[cfg(test)]
mod import_seam_pack_tests {
    use std::collections::BTreeMap;

    use rusqlite::{Connection, params};

    use super::*;
    use crate::db::hub_ids::HUB_ID_BASE;
    use crate::import::source_db::seam::SEAM_SLACK_MS;
    use crate::source_db_projection::{SourceDbDrive, SourceDbPosition};

    const T: i64 = 1_800_000_000_000;
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const CAR: i64 = 1;

    struct Fixture {
        _temporary: tempfile::TempDir,
        store: HubStore,
        binding: ProjectionBinding,
        car: ProjectionCar,
        gate: PublicationGate,
        cursor_key: CursorKey,
    }

    fn source_car() -> SourceDbCar {
        SourceDbCar {
            id: CAR,
            eid: 88,
            vid: Some(99),
            vin: Some("5YJTESTVIN1234567".into()),
            name: Some("Road car".into()),
            model: Some("Model 3".into()),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: Some(0.145),
            settings: Default::default(),
        }
    }

    fn source_drive(id: i64, start_ms: i64, end_ms: i64) -> SourceDbDrive {
        SourceDbDrive {
            id,
            car_id: CAR,
            start_date_ms: start_ms,
            end_date_ms: Some(end_ms),
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

    fn source_position(id: i64, drive_id: Option<i64>, date_ms: i64) -> SourceDbPosition {
        serde_json::from_value(serde_json::json!({
            "id": id, "car_id": CAR, "drive_id": drive_id, "date_ms": date_ms,
            "latitude": 47.5, "longitude": 19.0
        }))
        .expect("source position")
    }

    fn history(drives: Vec<SourceDbDrive>, positions: Vec<SourceDbPosition>) -> SourceDbHistory {
        SourceDbHistory {
            cars: vec![source_car()],
            drives,
            positions,
            charging_processes: vec![],
            charges: vec![],
            addresses: vec![],
            geofences: vec![],
            states: vec![],
            updates: vec![],
        }
    }

    /// The base: one drive long before any window.
    fn base_history() -> SourceDbHistory {
        history(vec![source_drive(10, T - 5 * HOUR, T - 5 * HOUR + 10 * MINUTE)], vec![])
    }

    /// The source after the Hub's window: drive 11 is the Hub's drive inside
    /// the window, drive 12 lies after it, drive 13 starts inside the window
    /// and crosses its end.
    fn successor_history() -> SourceDbHistory {
        let mut drives = base_history().drives;
        drives.push(source_drive(11, T + 10 * MINUTE, T + 20 * MINUTE));
        drives.push(source_drive(12, T + 2 * HOUR, T + 2 * HOUR + 10 * MINUTE));
        drives.push(source_drive(13, T + 50 * MINUTE, T + 75 * MINUTE));
        history(
            drives,
            vec![
                source_position(1101, Some(11), T + 12 * MINUTE),
                source_position(1201, Some(12), T + 2 * HOUR + 5 * MINUTE),
                source_position(1301, Some(13), T + 52 * MINUTE),
                source_position(1302, Some(13), T + 70 * MINUTE),
            ],
        )
    }

    fn state_limits() -> SourceDbProjectionStateLimits {
        SourceDbProjectionStateLimits {
            max_rows: 64,
            max_state_bytes: 256 * 1024,
            max_changed_payload_bytes: 256 * 1024,
            minimum_free_bytes: 0,
        }
    }

    fn begin_run(fixture: &Fixture, at_ms: i64) -> Uuid {
        let binding = &fixture.binding;
        let run_id = fixture
            .store
            .begin_import_generation(binding.account_id, binding.vehicle_id, CAR, at_ms)
            .expect("generation");
        fixture
            .store
            .stage_import_generation_session(
                run_id,
                &SourceDbOpenSession {
                    car_id: CAR,
                    ..Default::default()
                },
            )
            .expect("session");
        run_id
    }

    fn capture(
        fixture: &Fixture,
        run_id: Uuid,
        successor: bool,
        projected: &SourceDbProjection,
    ) -> SourceDbProjectionStateCapture {
        let binding = &fixture.binding;
        let mut capture = direct_projection_state_capture(
            &fixture.store,
            &fixture.gate,
            run_id,
            binding.vehicle_id,
            binding.account_id,
            CAR,
            successor,
            state_limits(),
        )
        .expect("capture");
        for car in &projected.snapshot.cars {
            capture.record_car(car).expect("car");
        }
        for drive in &projected.snapshot.drives {
            capture.record_drive(drive).expect("drive");
        }
        for position in &projected.snapshot.positions {
            capture.record_position(position).expect("position");
        }
        capture.seal().expect("seal");
        capture
    }

    /// A store with a published direct base of [`base_history`].
    fn fixture() -> Fixture {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let source = store
            .register_source(
                &SourceDescriptor::new(crate::db::IMPORT_SOURCE_KIND, "import-seam-packs"),
                T,
            )
            .expect("source");
        let vehicle = store
            .register_vehicle(&VehicleDescriptor::new(source.source_id, "import-seam-car"), T)
            .expect("vehicle");
        let binding = ProjectionBinding {
            installation_id: store.installation_id().expect("installation"),
            account_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: source.generation,
            selected_car_id: CAR,
        };
        let gate = store.try_acquire_publication_gate().expect("gate");
        let projected = project_vehicle(&base_history(), CAR).expect("base projection");
        let car = projected.snapshot.cars[0].clone();
        let fixture = Fixture {
            _temporary: temporary,
            store,
            binding,
            car,
            gate,
            cursor_key: CursorKey::from_bytes([41; 32]),
        };
        let run_id = begin_run(&fixture, T - HOUR);
        let state = capture(&fixture, run_id, false, &projected).into_state();
        let sequence = fixture
            .store
            .reserve_next_full_snapshot_sequence(&fixture.gate, fixture.binding.vehicle_id)
            .expect("base sequence");
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: fixture.binding.clone(),
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &projected.snapshot,
        };
        let pack = ProjectionPackWriter::new(fixture.store.packs_dir())
            .write_full_snapshot_with_states_and_updates(&request, &[], &[])
            .expect("base pack");
        let manifest = request
            .signed_manifest_with_states_and_updates(&pack, &[], &[], &fixture.cursor_key)
            .expect("base manifest");
        fixture
            .store
            .finalize_import_generation_with_projection_state_and_materialisation(
                run_id,
                fixture.binding.account_id,
                fixture.binding.vehicle_id,
                CAR,
                T - HOUR,
                &manifest,
                Sha256Digest::of_bytes(b"import-seam-base"),
                &[],
                &fixture.binding,
                &state,
                &fixture.car,
                &projected.snapshot.drives,
                false,
            )
            .expect("base");
        fixture
    }

    /// The Hub's window `[T, T + 1 h)`, handed back, with its drives and
    /// positions as the collector stores and journals them, published to
    /// clients as live sync: drive H1 is the source's drive 11, drive H2 the
    /// part of the source's drive 13 inside the window.
    fn collect(fixture: &Fixture) -> (i64, i64) {
        let (h1, h2) = (HUB_ID_BASE + 1, HUB_ID_BASE + 2);
        let projected = project_vehicle(&successor_history(), CAR).expect("projection");
        let template = projected.snapshot.drives[0].clone();
        let hub_drive = |id: i64, start_ms: i64, end_ms: i64| {
            let mut drive = template.clone();
            drive.id = id;
            drive.start_date_ms = start_ms;
            drive.end_date_ms = end_ms;
            drive
        };
        let hub_position = |id: i64, drive_id: i64, date_ms: i64| {
            let mut position =
                crate::lifecycle::imported_position(&source_position(id, Some(drive_id), date_ms));
            position.drive_id = Some(drive_id);
            position
        };
        let vehicle = fixture.binding.vehicle_id.to_string();
        let mut connection = fixture.store.open().expect("catalogue");
        let transaction = connection.transaction().expect("transaction");
        transaction
            .execute(
                "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
                 VALUES (1, 'grant-1', ?1, ?2, '[]', 'handback')",
                params![T, T + HOUR],
            )
            .expect("coverage");
        let journal = |entity: &str, id: i64, json: &str| {
            let revision: i64 = transaction
                .query_row(
                    "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision) VALUES (?1, 2)
                     ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = next_revision + 1
                     RETURNING next_revision - 1",
                    params![vehicle.as_str()],
                    |row| row.get(0),
                )
                .expect("revision");
            transaction
                .execute(
                    "INSERT INTO sync_mutations(vehicle_id, revision, entity, entity_id, car_id,
                        operation, payload_json, published, claimed_until_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'upsert', ?6, 0, 0)",
                    params![vehicle.as_str(), revision, entity, id, CAR, json],
                )
                .expect("mutation");
        };
        for drive in [
            hub_drive(h1, T + 10 * MINUTE, T + 20 * MINUTE),
            hub_drive(h2, T + 50 * MINUTE, T + 60 * MINUTE),
        ] {
            let json = serde_json::to_string(&drive).expect("drive json");
            transaction
                .execute(
                    "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![vehicle.as_str(), drive.id, CAR, json],
                )
                .expect("Hub drive");
            journal("drive", drive.id, &json);
        }
        for position in [
            hub_position(HUB_ID_BASE + 1, h1, T + 15 * MINUTE),
            hub_position(HUB_ID_BASE + 2, h2, T + 55 * MINUTE),
        ] {
            let json = serde_json::to_string(&position).expect("position json");
            transaction
                .execute(
                    "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![vehicle.as_str(), position.id, position.drive_id, CAR, json],
                )
                .expect("Hub position");
            journal("position", position.id, &json);
        }
        transaction.commit().expect("commit");
        publish_sync_mutations_ahead_of_packs(
            &fixture.store,
            &fixture.gate,
            &fixture.cursor_key,
            fixture.binding.vehicle_id,
        )
        .expect("live sync of the Hub's collection");
        (h1, h2)
    }

    /// The planned successor of `projected`, as far as its plan.
    fn plan(fixture: &Fixture, projected: &SourceDbProjection) -> PlannedDirectSuccessor {
        plan_direct_successor(
            &fixture.store,
            &fixture.gate,
            &fixture.cursor_key,
            fixture.binding.vehicle_id,
            CAR,
            &projected.snapshot.drives,
        )
        .expect("plan ahead of the packs")
    }

    /// Writes and publishes the planned successor of `projected`.
    fn publish(
        fixture: &Fixture,
        projected: &SourceDbProjection,
        run_id: Uuid,
        planned: &mut PlannedDirectSuccessor,
    ) -> Result<PublishedDirectSuccessor, SourceDbImportError> {
        let capture = capture(fixture, run_id, true, projected);
        let base_snapshot_id = fixture
            .store
            .v2_head(fixture.binding.vehicle_id)
            .expect("head")
            .expect("base")
            .0;
        publish_planned_direct_successor(
            &fixture.store,
            &fixture.gate,
            &fixture.cursor_key,
            PlannedSuccessorRun {
                run_id,
                source_id: fixture.binding.account_id,
                imported_at_ms: T + 3 * HOUR,
                base_snapshot_id,
                minimum_free_bytes: 0,
                fingerprint: Sha256Digest::of_bytes(format!("successor-{run_id}").as_bytes()),
                geofences: &[],
                materialised_car: &fixture.car,
                materialised_drives: &projected.snapshot.drives,
            },
            &fixture.binding,
            &fixture.car,
            capture,
            planned,
        )
    }

    /// One pack's drives (id, start, end), positions (id, drive, instant)
    /// and tombstones (entity, id).
    #[derive(Debug, Default)]
    struct PackRows {
        drives: Vec<(i64, i64, i64)>,
        positions: Vec<(i64, Option<i64>, i64)>,
        tombstones: Vec<(String, i64)>,
    }

    fn pack_rows(store: &HubStore, digest: Sha256Digest) -> PackRows {
        let path = ProjectionPackWriter::new(store.packs_dir()).content_path(digest);
        let sqlite =
            zstd::stream::decode_all(std::fs::File::open(&path).expect("pack")).expect("pack bytes");
        let temporary = crate::private_tempdir().expect("inspection");
        let file = temporary.path().join("pack.sqlite");
        std::fs::write(&file, sqlite).expect("inspection copy");
        let connection = Connection::open(&file).expect("pack database");
        let table = |name: &str| -> bool {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    params![name],
                    |row| row.get(0),
                )
                .expect("schema")
        };
        let mut rows = PackRows::default();
        if table("drives") {
            let mut statement = connection
                .prepare("SELECT id, start_date_ms, end_date_ms FROM drives ORDER BY id")
                .expect("drives");
            rows.drives = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .expect("drive rows")
                .collect::<Result<_, _>>()
                .expect("drive rows");
        }
        if table("positions") {
            let mut statement = connection
                .prepare("SELECT id, drive_id, date_ms FROM positions ORDER BY id")
                .expect("positions");
            rows.positions = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .expect("position rows")
                .collect::<Result<_, _>>()
                .expect("position rows");
        }
        if table("tombstones") {
            let mut statement = connection
                .prepare("SELECT entity, entity_id FROM tombstones ORDER BY entity, entity_id")
                .expect("tombstones");
            rows.tombstones = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("tombstone rows")
                .collect::<Result<_, _>>()
                .expect("tombstone rows");
        }
        rows
    }

    /// Every advertised head of the vehicle in order: the base's packs, then
    /// one entry per delta, each with its head sequence.
    fn heads(store: &HubStore, vehicle_id: Uuid) -> Vec<(u64, PackRows)> {
        let lineage = store
            .lineage_manifest_for_vehicle(vehicle_id)
            .expect("lineage")
            .expect("published lineage");
        let mut base = PackRows::default();
        for pack in &lineage.base.packs {
            let rows = pack_rows(store, pack.sha256);
            base.drives.extend(rows.drives);
            base.positions.extend(rows.positions);
            base.tombstones.extend(rows.tombstones);
        }
        let mut heads = vec![(lineage.base.sequence, base)];
        for delta in &lineage.deltas {
            heads.push((delta.to_sequence, pack_rows(store, delta.pack.sha256)));
        }
        heads
    }

    /// A client's mirror of one vehicle: drives and positions.
    #[derive(Debug, Default, Clone, PartialEq, Eq)]
    struct Client {
        drives: BTreeMap<i64, (i64, i64)>,
        positions: BTreeMap<i64, (Option<i64>, i64)>,
    }

    impl Client {
        fn apply(&mut self, rows: &PackRows) {
            for (id, start, end) in &rows.drives {
                self.drives.insert(*id, (*start, *end));
            }
            for (id, drive, at) in &rows.positions {
                self.positions.insert(*id, (*drive, *at));
            }
            for (entity, id) in &rows.tombstones {
                if entity == "drive" {
                    self.drives.remove(id);
                } else if entity == "position" {
                    self.positions.remove(id);
                }
            }
        }

        /// No two drives overlap by more than the seam's slack: the client
        /// holds each session once.
        fn assert_each_session_once(&self, at: &str) {
            let drives: Vec<_> = self.drives.iter().collect();
            for (index, (a, (a_start, a_end))) in drives.iter().enumerate() {
                for (b, (b_start, b_end)) in &drives[index + 1..] {
                    let overlap = a_end.min(b_end) - a_start.max(b_start);
                    assert!(
                        overlap <= SEAM_SLACK_MS,
                        "{at}: drives {a} and {b} are the same session"
                    );
                }
            }
        }
    }

    fn store_drives(store: &HubStore, vehicle_id: Uuid) -> BTreeMap<i64, (i64, i64)> {
        let connection = store.open().expect("catalogue");
        let mut statement = connection
            .prepare(
                "SELECT drive_id,
                        CAST(json_extract(drive_json, '$.start_date_ms') AS INTEGER),
                        CAST(json_extract(drive_json, '$.end_date_ms') AS INTEGER)
                   FROM materialised_drives WHERE vehicle_id = ?1",
            )
            .expect("drives");
        statement
            .query_map(params![vehicle_id.to_string()], |row| {
                Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
            })
            .expect("drive rows")
            .collect::<Result<_, _>>()
            .expect("drive rows")
    }

    fn store_hub_positions(store: &HubStore, vehicle_id: Uuid) -> BTreeMap<i64, (Option<i64>, i64)> {
        let connection = store.open().expect("catalogue");
        let mut statement = connection
            .prepare(
                "SELECT position_id, drive_id,
                        CAST(json_extract(position_json, '$.date_ms') AS INTEGER)
                   FROM materialised_positions WHERE vehicle_id = ?1 AND position_id >= ?2",
            )
            .expect("positions");
        statement
            .query_map(params![vehicle_id.to_string(), HUB_ID_BASE], |row| {
                Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
            })
            .expect("position rows")
            .collect::<Result<_, _>>()
            .expect("position rows")
    }

    fn mutation_count(store: &HubStore, vehicle_id: Uuid) -> i64 {
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("mutations")
    }

    /// The source's drive 11 lost to the Hub's H1 inside the window: it is in
    /// no pack, and neither is its position inside coverage.
    #[test]
    fn import_seam_skipped_source_drive_is_not_in_the_pack() {
        let fixture = fixture();
        let vehicle_id = fixture.binding.vehicle_id;
        let (h1, _) = collect(&fixture);
        let projected = project_vehicle(&successor_history(), CAR).expect("projection");
        let mut planned = plan(&fixture, &projected);
        let before = heads(&fixture.store, vehicle_id).len();
        let run_id = begin_run(&fixture, T + 3 * HOUR);
        publish(&fixture, &projected, run_id, &mut planned).expect("publication");
        let import: Vec<_> = heads(&fixture.store, vehicle_id).split_off(before);
        assert!(!import.is_empty(), "the import published its packs");
        let packed: Vec<i64> = import
            .iter()
            .flat_map(|(_, rows)| rows.drives.iter().map(|row| row.0))
            .collect();
        let positions: Vec<i64> = import
            .iter()
            .flat_map(|(_, rows)| rows.positions.iter().map(|row| row.0))
            .collect();
        assert_eq!(packed, vec![12, 13], "drive 11 is skipped; 10 is unchanged");
        assert_eq!(positions, vec![1201, 1302], "positions inside coverage stay the Hub's");
        assert!(store_drives(&fixture.store, vehicle_id).contains_key(&h1));
        assert!(!store_drives(&fixture.store, vehicle_id).contains_key(&11));
    }

    /// Every changed source row the publication read is packed or counted
    /// as seam-skipped.
    #[test]
    fn import_seam_parity_accounts_for_seam_skipped_rows() {
        let fixture = fixture();
        let vehicle_id = fixture.binding.vehicle_id;
        collect(&fixture);
        let projected = project_vehicle(&successor_history(), CAR).expect("projection");
        let mut planned = plan(&fixture, &projected);
        let before = heads(&fixture.store, vehicle_id).len();
        let run_id = begin_run(&fixture, T + 3 * HOUR);
        publish(&fixture, &projected, run_id, &mut planned).expect("publication");
        let import: Vec<_> = heads(&fixture.store, vehicle_id).split_off(before);
        let packed_drives: u64 = import.iter().map(|(_, rows)| rows.drives.len() as u64).sum();
        let packed_positions: u64 = import
            .iter()
            .map(|(_, rows)| rows.positions.len() as u64)
            .sum();
        let skipped = planned.seam.skipped;
        assert_eq!(
            skipped,
            SourceDbSeamSkipped {
                drives: 1,
                positions: 2
            }
        );
        // Changed source rows: drives 11, 12, 13 and positions 1101, 1201,
        // 1301, 1302.
        assert_eq!(packed_drives + skipped.drives, 3);
        assert_eq!(packed_positions + skipped.positions, 4);
        assert_eq!(skipped.total(), 3);
    }

    /// Sync carries the Hub's rows and the seam's tombstones, packs carry
    /// source rows only, and together they give the client the store's
    /// drives and the Hub's positions with the same ids.
    #[test]
    fn import_seam_sync_ids_against_pack_ids() {
        let fixture = fixture();
        let vehicle_id = fixture.binding.vehicle_id;
        let (h1, h2) = collect(&fixture);
        let collected = heads(&fixture.store, vehicle_id).len();
        let projected = project_vehicle(&successor_history(), CAR).expect("projection");
        let mut planned = plan(&fixture, &projected);
        let planned_heads = heads(&fixture.store, vehicle_id).len();
        let run_id = begin_run(&fixture, T + 3 * HOUR);
        publish(&fixture, &projected, run_id, &mut planned).expect("publication");
        let all = heads(&fixture.store, vehicle_id);
        let seam_sync = &all[collected..planned_heads];
        let packs = &all[planned_heads..];
        assert!(!seam_sync.is_empty(), "the seam's changes went out as sync first");

        // Sync: Hub-range upserts and the tombstone of the Hub drive the
        // source replaced; never a source row.
        for (_, rows) in seam_sync {
            assert!(rows.drives.iter().all(|row| row.0 >= HUB_ID_BASE));
            assert!(rows.positions.iter().all(|row| row.0 >= HUB_ID_BASE));
        }
        let sync_tombstones: Vec<_> = seam_sync
            .iter()
            .flat_map(|(_, rows)| rows.tombstones.clone())
            .collect();
        assert_eq!(sync_tombstones, vec![("drive".to_owned(), h2)]);
        let moved: Vec<_> = seam_sync
            .iter()
            .flat_map(|(_, rows)| rows.positions.clone())
            .collect();
        assert_eq!(moved, vec![(HUB_ID_BASE + 2, Some(13), T + 55 * MINUTE)]);
        // Packs: source rows only, and no tombstone of a Hub row.
        for (_, rows) in packs {
            assert!(rows.drives.iter().all(|row| row.0 < HUB_ID_BASE));
            assert!(rows.positions.iter().all(|row| row.0 < HUB_ID_BASE));
            assert!(rows.tombstones.iter().all(|row| row.1 < HUB_ID_BASE));
        }

        // The client that applied every head holds what the store holds.
        let mut client = Client::default();
        for (_, rows) in &all {
            client.apply(rows);
        }
        assert_eq!(client.drives, store_drives(&fixture.store, vehicle_id));
        let mut hub_positions = client.positions.clone();
        hub_positions.retain(|id, _| *id >= HUB_ID_BASE);
        assert_eq!(hub_positions, store_hub_positions(&fixture.store, vehicle_id));
        assert_eq!(
            client.drives.keys().copied().collect::<Vec<_>>(),
            vec![10, 12, 13, h1]
        );
        assert!(
            !fixture
                .store
                .has_unpublished_sync_mutations(vehicle_id)
                .expect("journal"),
            "nothing the packs rely on is left unpublished"
        );
    }

    /// A fresh client at each advertised head, and an existing client
    /// applying the new heads one by one, never hold a session twice; a
    /// replay of the same source changes nothing and keeps that true.
    #[test]
    fn import_seam_every_advertised_head_lists_each_session_once() {
        let fixture = fixture();
        let vehicle_id = fixture.binding.vehicle_id;
        collect(&fixture);
        let existing_at = heads(&fixture.store, vehicle_id).len();
        let projected = project_vehicle(&successor_history(), CAR).expect("projection");
        let mut planned = plan(&fixture, &projected);
        let run_id = begin_run(&fixture, T + 3 * HOUR);
        publish(&fixture, &projected, run_id, &mut planned).expect("publication");

        // Replay: the same source again.
        let journalled = mutation_count(&fixture.store, vehicle_id);
        let drives = store_drives(&fixture.store, vehicle_id);
        let mut replayed = plan(&fixture, &projected);
        let run_id = begin_run(&fixture, T + 4 * HOUR);
        publish(&fixture, &projected, run_id, &mut replayed).expect("replay");
        assert_eq!(mutation_count(&fixture.store, vehicle_id), journalled);
        assert_eq!(store_drives(&fixture.store, vehicle_id), drives);
        assert_eq!(replayed.seam.skipped, SourceDbSeamSkipped::default());

        let all = heads(&fixture.store, vehicle_id);
        // Fresh client: everything up to each head.
        for end in 1..=all.len() {
            let mut client = Client::default();
            for (_, rows) in &all[..end] {
                client.apply(rows);
            }
            client.assert_each_session_once(&format!("fresh client at head {}", all[end - 1].0));
        }
        // Existing client: at the head before the import, then one by one.
        let mut client = Client::default();
        for (_, rows) in &all[..existing_at] {
            client.apply(rows);
        }
        client.assert_each_session_once("existing client before the import");
        for (sequence, rows) in &all[existing_at..] {
            client.apply(rows);
            client.assert_each_session_once(&format!("existing client at head {sequence}"));
        }
        assert_eq!(client.drives, store_drives(&fixture.store, vehicle_id));
    }

    /// The store changed between the plan and publication (here a second
    /// window): publication plans again, finds another plan and refuses
    /// without publishing anything.
    #[test]
    fn import_seam_plan_digest_mismatch_refuses() {
        let fixture = fixture();
        let vehicle_id = fixture.binding.vehicle_id;
        collect(&fixture);
        let projected = project_vehicle(&successor_history(), CAR).expect("projection");
        let mut planned = plan(&fixture, &projected);
        let head = fixture.store.v2_head(vehicle_id).expect("head");
        let drives = store_drives(&fixture.store, vehicle_id);
        fixture
            .store
            .open()
            .expect("catalogue")
            .execute(
                "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
                 VALUES (2, 'grant-2', ?1, ?2, '[]', 'handback')",
                params![T + 2 * HOUR - 5 * MINUTE, T + 3 * HOUR],
            )
            .expect("second window");
        let run_id = begin_run(&fixture, T + 3 * HOUR);
        let refused = publish(&fixture, &projected, run_id, &mut planned);
        assert!(
            matches!(
                refused,
                Err(SourceDbImportError::Store(StoreError::ImportGenerationConflict))
            ),
            "{refused:?}"
        );
        assert_eq!(fixture.store.v2_head(vehicle_id).expect("head"), head);
        assert_eq!(store_drives(&fixture.store, vehicle_id), drives);
    }

    /// Publication paths that do not plan the seam refuse once the Hub has
    /// collected the vehicle (review finding 9): the in-memory history
    /// importer before it writes a pack, the open-session seed, and the
    /// store's own refusal, which holds only after collection.
    #[test]
    fn import_seam_unplanned_publication_paths_refuse() {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let cursor_key = CursorKey::from_bytes([43; 32]);
        let request = SourceDbImportRequest {
            new_import_source: true,
            source_key: "import-seam-unplanned".into(),
            scope: SourceDbImportScope::Selected(CAR),
            imported_at_ms: T - HOUR,
        };
        let base = publish_history(&store, &cursor_key, &request, &base_history()).expect("base");
        store
            .refuse_unplanned_import_seam(base.vehicle_id)
            .expect("before collection the unplanned paths are exact");

        let vehicle = base.vehicle_id.to_string();
        let connection = store.open().expect("catalogue");
        connection
            .execute(
                "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
                 VALUES (1, 'grant-1', ?1, ?2, '[]', 'handback')",
                params![T, T + HOUR],
            )
            .expect("coverage");
        let mut drive = project_vehicle(&base_history(), CAR).expect("projection").snapshot.drives[0]
            .clone();
        drive.id = HUB_ID_BASE + 1;
        drive.start_date_ms = T + 10 * MINUTE;
        drive.end_date_ms = T + 20 * MINUTE;
        connection
            .execute(
                "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    vehicle.as_str(),
                    drive.id,
                    CAR,
                    serde_json::to_string(&drive).expect("drive json")
                ],
            )
            .expect("Hub drive");
        drop(connection);
        let lineage = store.lineage_manifest_for_vehicle(base.vehicle_id).expect("lineage");

        assert!(matches!(
            store.refuse_unplanned_import_seam(base.vehicle_id),
            Err(StoreError::ImportGenerationConflict)
        ));
        let changed = SourceDbImportRequest {
            new_import_source: false,
            imported_at_ms: T + 3 * HOUR,
            ..request
        };
        let refused = publish_history(&store, &cursor_key, &changed, &successor_history());
        assert!(
            matches!(
                refused,
                Err(SourceDbImportError::Store(StoreError::ImportGenerationConflict))
            ),
            "{refused:?}"
        );
        assert_eq!(
            store.lineage_manifest_for_vehicle(base.vehicle_id).expect("lineage"),
            lineage,
            "nothing was published"
        );
        let session = SourceDbOpenSession {
            car_id: CAR,
            standalone_positions: vec![source_position(1501, None, T + 30 * MINUTE)],
            ..Default::default()
        };
        assert!(matches!(
            store.seed_imported_open_session(base.source_id, base.vehicle_id, CAR, &session, T + 3 * HOUR),
            Err(StoreError::ImportGenerationConflict)
        ));
    }
}
