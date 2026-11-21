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

use crate::runtime::legacy_names::SOURCE_HISTORY_V2_STANDALONE_POSITIONS;

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
/// seam (design M03-SEAM-R3, section 4): drives and charges the plan gave to
/// another row (or an inherited session whose stored row stays), and
/// positions and charge samples inside the Hub's coverage. Every changed
/// source row the publication read is either packed or counted here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceDbSeamSkipped {
    pub drives: u64,
    pub positions: u64,
    pub charges: u64,
    pub charge_samples: u64,
}

impl SourceDbSeamSkipped {
    /// All seam-skipped rows.
    pub fn total(&self) -> u64 {
        self.drives
            .saturating_add(self.positions)
            .saturating_add(self.charges)
            .saturating_add(self.charge_samples)
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
    Ok(SourceDbProjectionStateCapture::for_successor(state, Box::new(prior))
        .with_seam_lens(store.import_seam_capture_lens(vehicle_id)?))
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
    window_rebind: bool,
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
            let capture = direct_projection_state_capture(
                store,
                publication_gate,
                run_id,
                vehicle_id,
                source_id,
                selected_car_id,
                successor,
                state_limits,
            )?;
            if window_rebind {
                return Ok(capture.with_rebind_seam_lens(store.import_seam_capture_lens(vehicle_id)?));
            }
            Ok(capture)
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
    physical_seam: Option<crate::import::source_db::seam::SeamPackPlan>,
    /// The import's source sessions, kept with the raw physical stage for a
    /// later handback (M03-T22).
    physical_source: Option<(Vec<ProjectionDrive>, Vec<ProjectionCharge>)>,
    /// The source sessions the import read, for its final report.
    source_basis: SourceDbImportBasis,
    publication_gate: PublicationGate,
    /// The capture and intent recorded before the import's first commit,
    /// with which its publication finishes after a restart (issue #60).
    publication: Option<crate::db::ImportPublicationIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDbSelectedImportReport {
    pub import: SourceDbImportReport,
    pub updates_schema_22: ProductionUpdatesPublication,
    pub physical_v3: Option<crate::physical_publication::PhysicalV3Publication>,
    /// The checked source range and per-route counts for the final report.
    pub summary: SourceDbImportSummary,
}

/// What an import read from the source: the sessions, and the instants they
/// span in Unix milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SourceDbImportBasis {
    sessions: u64,
    range_ms: Option<(i64, i64)>,
}

impl SourceDbImportBasis {
    fn of(drives: &[ProjectionDrive], charges: &[ProjectionCharge]) -> Self {
        let spans = drives
            .iter()
            .map(|drive| (drive.start_date_ms, drive.end_date_ms.max(drive.start_date_ms)))
            .chain(charges.iter().map(|charge| {
                (
                    charge.start_date_ms,
                    charge.end_date_ms.unwrap_or(charge.start_date_ms).max(charge.start_date_ms),
                )
            }));
        let range_ms = spans.fold(None, |range, (start, end)| match range {
            None => Some((start, end)),
            Some((low, high)) => Some((low.min(start), high.max(end))),
        });
        Self {
            sessions: (drives.len() as u64).saturating_add(charges.len() as u64),
            range_ms,
        }
    }
}

/// Source sessions a phone route got from one import: packed into its
/// packs, kept on the Hub's side by the seam, or withheld from it (not
/// packed, or the route's head not served). Counts only: never a place, name
/// or address (DEC-246).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceDbRouteCounts {
    pub packed: u64,
    pub kept: u64,
    pub withheld: u64,
}

/// The import's final report: the source time range checked (Unix
/// milliseconds) and the counts of each phone route, the Legacy updates route
/// and the negotiated 1.4 route (M08-T41, DEC-246).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceDbImportSummary {
    pub range_ms: Option<(i64, i64)>,
    pub legacy: SourceDbRouteCounts,
    pub physical: SourceDbRouteCounts,
}

impl SourceDbImportSummary {
    /// The report's JSON: the checked range as Unix milliseconds (null when
    /// the source held no session) and each route's counts, nothing else.
    pub fn to_json(&self) -> serde_json::Value {
        let counts = |route: &SourceDbRouteCounts| {
            serde_json::json!({
                "packed": route.packed,
                "kept": route.kept,
                "withheld": route.withheld,
            })
        };
        serde_json::json!({
            "checkedFromMs": self.range_ms.map(|(from, _)| from),
            "checkedToMs": self.range_ms.map(|(_, to)| to),
            "routes": {
                "legacy": counts(&self.legacy),
                "physical": counts(&self.physical),
            },
        })
    }

    fn new(
        basis: SourceDbImportBasis,
        seam: Option<&crate::import::source_db::seam::SeamPackPlan>,
        physical_served: bool,
    ) -> Self {
        let legacy = match seam {
            Some(packs) => {
                let (packed, kept, withheld) = packs.session_counts();
                SourceDbRouteCounts { packed, kept, withheld }
            }
            None => SourceDbRouteCounts {
                packed: basis.sessions,
                kept: 0,
                withheld: 0,
            },
        };
        let physical = if physical_served {
            legacy
        } else {
            SourceDbRouteCounts {
                packed: 0,
                kept: 0,
                withheld: legacy.packed.saturating_add(legacy.withheld),
            }
        };
        Self {
            range_ms: basis.range_ms,
            legacy,
            physical,
        }
    }
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
    let publication_gate = store.acquire_publication_gate().await?;
    match import_pass(
        store,
        source,
        password,
        cursor_key,
        request,
        limits,
        prepare_schema_22,
        progress,
        cutover_policy,
        publication_gate,
    )
    .await?
    {
        ImportPass::Captured(captured) => Ok(*captured),
    }
}

/// How a direct import goes on from the vehicle's V2 head.
#[derive(Debug)]
enum DirectImportRoute {
    /// No V2 head yet: the import publishes the first base.
    Base,
    /// An import base with its projection state: typed deltas on it.
    Successor,
    /// The exact legacy direct-import shape, upgraded in place.
    LegacyBridge,
    /// The collector's base on a Hub that never imported (DEC-150): the
    /// import publishes its base in that one's place, then the seam on it
    /// from the same capture, under the same gate.
    WindowRebind(Box<crate::protocol::LineageManifestV2>),
}

/// Decides [`DirectImportRoute`]. Any other base without projection state,
/// such as a direct-import base the bridge cannot prove unchanged, is
/// refused with `rebase_required`.
fn direct_import_route(
    store: &HubStore,
    vehicle_id: Uuid,
    source_id: Uuid,
    selected_car_id: i64,
) -> Result<DirectImportRoute, SourceDbImportError> {
    if store.v2_head(vehicle_id)?.is_none() {
        return Ok(DirectImportRoute::Base);
    }
    if store.source_db_import_projection_state_exists(vehicle_id)? {
        return Ok(DirectImportRoute::Successor);
    }
    if store.legacy_source_db_direct_bridge_is_eligible(vehicle_id, source_id, selected_car_id)? {
        return Ok(DirectImportRoute::LegacyBridge);
    }
    if store.window_only_v2_base_is_rebindable(vehicle_id)? {
        let expected = store
            .lineage_manifest_for_vehicle(vehicle_id)?
            .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
        return Ok(DirectImportRoute::WindowRebind(Box::new(expected)));
    }
    Err(SourceDbImportError::LegacyDirectImportRebaseRequired)
}

/// What one import published.
enum ImportPass {
    Captured(Box<CapturedSourceDbImport>),
}

impl From<CapturedSourceDbImport> for ImportPass {
    fn from(captured: CapturedSourceDbImport) -> Self {
        Self::Captured(Box::new(captured))
    }
}

#[allow(clippy::too_many_arguments)]
async fn import_pass(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    cursor_key: &CursorKey,
    request: &SourceDbImportRequest,
    limits: SourceDbReadLimits,
    prepare_schema_22: bool,
    progress: SourceDbMigrationProgressReporter,
    cutover_policy: CaptureCutoverPolicy,
    publication_gate: PublicationGate,
) -> Result<ImportPass, SourceDbImportError> {
    tracing::info!(
        host = source.host(),
        port = source.port(),
        database = source.database_name(),
        prepare_schema_22,
        "Source database import opening a read-only PostgreSQL snapshot; source rows are never deleted"
    );
    let selected_car_id = selected_car_id(request)?;
    // Issue #60: an earlier import's interrupted publication is finished (or
    // dropped, or refused) before this one may record its own.
    resume_import_publications(store, cursor_key, &publication_gate)?;
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
    let route = direct_import_route(
        store,
        vehicle.vehicle_id,
        registered_source.source_id,
        selected_car_id,
    )?;
    let legacy_bridge = matches!(route, DirectImportRoute::LegacyBridge);
    let window_lineage = match route {
        DirectImportRoute::WindowRebind(expected) => Some(*expected),
        _ => None,
    };
    let window_rebind = window_lineage.is_some();
    let successor = prior_v2_head.is_some() && !legacy_bridge && !window_rebind;
    // Only a successor plans the import seam ahead of its packs; a base or
    // the legacy bridge may not publish over the Hub's own collection. A
    // rebind plans it on its new base before the import returns.
    if !successor && !window_rebind {
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
    // M03-T26: no plan may use the previous repair index once this import
    // can commit; the import records its own capture after it commits.
    store.invalidate_drive_repair_index(vehicle.vehicle_id)?;
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
        window_rebind,
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
    let materialised_charges = first_capture.materialised_charges;
    store.stage_import_generation_session(run_id, &open_session)?;
    let mut direct = first_capture.packs;
    let updates_v2_2 = first_capture.updates_v2_2;
    let mut physical_stage = first_capture.physical_stage;
    let physical_source = physical_stage
        .as_ref()
        .map(|_| (materialised_drives.clone(), materialised_charges.clone()));
    let source_basis = SourceDbImportBasis::of(&materialised_drives, &materialised_charges);
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
    let mut physical_seam = None;
    if successor {
        (direct.fingerprint, physical_seam) = seam_folded_fingerprint_and_plan(
            store,
            &publication_gate,
            vehicle.vehicle_id,
            selected_car_id,
            &materialised_drives,
            &materialised_charges,
            direct.fingerprint,
        );
    }
    // Issue #60: the capture a restart finishes this import's publication
    // from is durable, and its intent recorded, before anything commits.
    let publication = match physical_stage.as_mut() {
        Some(stage) => match capture_import_publication(
            store,
            &publication_gate,
            run_id,
            &binding,
            stage.stage_mut(),
            &materialised_drives,
            &materialised_charges,
            &updates_v2_2,
        ) {
            Ok(intent) => Some(intent),
            Err(error) => {
                store.abort_import_generation(run_id)?;
                return Err(error);
            }
        },
        None => None,
    };
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
        return Ok(ImportPass::from(CapturedSourceDbImport {
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
            physical_seam,
            physical_source,
            source_basis,
            publication_gate,
            publication,
        }));
    }
    let transport_rows = transport_row_count(&direct.chunks)?;
    if !window_rebind
        && store.source_fingerprint_matches(vehicle.vehicle_id, direct.fingerprint)?
    {
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
            return Ok(ImportPass::from(CapturedSourceDbImport {
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
                physical_seam,
                physical_source,
                source_basis,
                publication_gate,
                publication,
            }));
        }
        if let Some(current) =
            store.manifest_for_snapshot_fingerprint(vehicle.vehicle_id, direct.fingerprint)?
        {
            return Ok(ImportPass::from(CapturedSourceDbImport {
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
                physical_seam,
                physical_source,
                source_basis,
                publication_gate,
                publication,
            }));
        }
    }
    let mut capture = direct
        .projection_state
        .take()
        .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
    capture.seal()?;
    let mut replacement = None;
    if let Some((base_snapshot_id, _, _)) = prior_v2_head.filter(|_| !window_rebind) {
        let selected_car = direct
            .selected_car
            .clone()
            .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
        // The seam goes ahead of the packs: its Hub-side changes are applied
        // and published first, then the packs carry the planned rows only.
        // Decide before discarding the complete capture or writing delta packs.
        // Only pack-capacity exhaustion permits an explicit new base, and
        // never once the Hub has collected the vehicle (DEC-103).
        let route = route_direct_successor_for_run(
            store,
            &publication_gate,
            cursor_key,
            vehicle.vehicle_id,
            selected_car_id,
            &materialised_drives,
            &materialised_charges,
            &mut capture,
            &binding,
            &selected_car,
            ProtocolLimits::default().max_chunks,
            &mut direct,
            &mut run_guard,
            |_| Ok(()),
        )?;
        if let DirectSuccessorRoute::NewBase(planned) = route {
            let mut event = crate::runtime::development_event_log::Event::new(
                crate::runtime::development_event_log::Kind::CapacityFallback,
                crate::runtime::development_event_log::Outcome::FullBase,
            );
            event.base_sequence = Some(planned.head_sequence);
            event.chunks = planned.existing_pack_count as u64;
            crate::runtime::development_event_log::record(event);
            replacement = Some(planned.existing);
        } else if let DirectSuccessorRoute::Deltas(mut planned) = route {
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
                    materialised_charges: &materialised_charges,
                },
                &binding,
                &selected_car,
                capture,
                &mut planned,
            )?;
            run_guard.disarm();
            record_repair_candidates(
                store,
                registered_source.source_id,
                vehicle.vehicle_id,
                &mut direct,
                request.imported_at_ms,
            );
            let physical_seam = Some(planned.seam.packs.clone());
            return Ok(ImportPass::from(CapturedSourceDbImport {
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
                physical_seam,
                physical_source,
                source_basis,
                publication_gate,
                publication,
            }));
        }
    }
    if let Some(expected) = window_lineage.as_ref() {
        // The rebind publishes its base and the seam on it from this capture
        // in one catalogue transaction (DEC-150, DEC-151 follow-up).
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
        let selected_car = direct
            .selected_car
            .clone()
            .ok_or(SourceDbImportError::ProjectionStateCaptureMissing)?;
        let rebound = publish_window_rebind(
            store,
            &publication_gate,
            cursor_key,
            PlannedSuccessorRun {
                run_id,
                source_id: registered_source.source_id,
                imported_at_ms: request.imported_at_ms,
                base_snapshot_id: capture_snapshot_id,
                minimum_free_bytes: limits.minimum_free_bytes,
                fingerprint: direct.fingerprint,
                geofences: &direct.geofences,
                materialised_car: &materialised_car,
                materialised_drives: &materialised_drives,
                materialised_charges: &materialised_charges,
            },
            expected,
            &manifest,
            &binding,
            &selected_car,
            capture,
        );
        let (published, seam) = match rebound {
            Ok(value) => value,
            Err(error) => {
                // Only a store error can be an ambiguous commit; anything
                // else left the base's packs unreferenced.
                let outcome = match &error {
                    SourceDbImportError::Store(store_error) => store_error,
                    _ => &crate::db::StoreError::ImportGenerationConflict,
                };
                reconcile_failed_full_snapshot_candidate(store, &publication_gate, &mut direct, outcome)?;
                return Err(error);
            }
        };
        direct.keep_chunks();
        run_guard.disarm();
        record_repair_candidates(
            store,
            registered_source.source_id,
            vehicle.vehicle_id,
            &mut direct,
            request.imported_at_ms,
        );
        return Ok(ImportPass::from(CapturedSourceDbImport {
            report: SourceDbImportReport {
                source_id: registered_source.source_id,
                vehicle_id: vehicle.vehicle_id,
                snapshot_id: capture_snapshot_id,
                sequence: published.sequence,
                projection: direct.report,
                projected_rows: transport_rows
                    .checked_add(published.projected_rows)
                    .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?,
                skipped: false,
                cutover_unsettled: cutover.cutover_unsettled,
                seam_skipped: seam.skipped,
            },
            binding: binding.clone(),
            updates_v2_2,
            atomic_schema_22: None,
            physical_stage,
            physical_seam: Some(seam.packs),
            physical_source,
            source_basis,
            publication_gate,
            publication,
        }));
    }
    let projection_state = capture.state();
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
    let prepared_schema_22 = if prepare_schema_22 && replacement.is_none() && !window_rebind {
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
            projection_state,
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
            projection_state,
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
            projection_state,
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
    record_repair_candidates(
        store,
        registered_source.source_id,
        vehicle.vehicle_id,
        &mut direct,
        request.imported_at_ms,
    );
    Ok(ImportPass::from(CapturedSourceDbImport {
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
        physical_seam,
        physical_source,
        source_basis,
        publication_gate,
        publication,
    }))
}

// The import seam ahead of a direct successor's packs (M03-T16).
include!("importer/seam_ahead.rs");

// The window rebind and its seam in one transaction (M03-T24).
include!("importer/window_rebind.rs");

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
    // M03-T26: no plan may use the previous repair index once this import
    // can commit; the import records its own capture after it commits.
    store.invalidate_drive_repair_index(vehicle.vehicle_id)?;
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
    record_repair_candidates(
        store,
        source.source_id,
        vehicle.vehicle_id,
        &mut staged,
        request.imported_at_ms,
    );
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
    record_repair_candidates(store, source_id, vehicle_id, &mut staged, request.imported_at_ms);
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
    digest.update(SOURCE_HISTORY_V2_STANDALONE_POSITIONS.as_bytes());
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

// A direct successor's typed delta rows (M03-T24 split).
include!("importer/direct_delta_rows.rs");

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
/// M03-T26: keep the repair candidate index captured with a published or
/// promoted import. The import has already committed and withdrew the
/// previous index before it could commit, so a failure here leaves no index
/// to plan from until a later import records one; it is logged without row
/// values.
fn record_repair_candidates(
    store: &HubStore,
    source_id: Uuid,
    vehicle_id: Uuid,
    packs: &mut StagedProjectionPacks,
    at_ms: i64,
) {
    let Some(index) = packs.repair_candidates.take() else {
        return;
    };
    if let Err(error) = store.record_drive_repair_index(source_id, vehicle_id, &index, at_ms) {
        tracing::warn!(%error, "drive repair candidate index was not recorded");
    }
}

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
    let promoted = match materialisation {
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
    };
    record_repair_candidates(store, source_id, vehicle_id, direct, updated_at_ms);
    Ok(promoted)
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
    #[error(
        "history committed for vehicle {vehicle_id} snapshot {legacy_snapshot_id}, but phone sync could not be published with the collection coverage"
    )]
    PhysicalHeadWithheld {
        vehicle_id: Uuid,
        legacy_snapshot_id: Uuid,
    },
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
    #[error(
        "the import's publication capture for vehicle {vehicle_id} could not be written, so nothing was committed; retry the selected-car import: {reason}"
    )]
    PublicationCapture { vehicle_id: Uuid, reason: String },
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
    #[error(
        "import refused: its changes no longer fit in the vehicle's sync history ({used} of {limit} packs in use), so it would need a new import base, and the import base of a vehicle the Hub has collected is never replaced because clients would lose the Hub's own rows; nothing was changed"
    )]
    ImportBaseReplacementRefused { used: usize, limit: usize },
}

#[cfg(test)]
#[path = "importer/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "importer/open_cutover_tests.rs"]
mod open_cutover_tests;

#[cfg(test)]
#[path = "importer/import_summary_tests.rs"]
mod import_summary_tests;

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
                SourceDbImportError::PhysicalHeadWithheld { .. } => (),
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
                SourceDbImportError::PublicationCapture { .. } => (),
                SourceDbImportError::Reader(..) => (),
                SourceDbImportError::Direct(..) => (),
                SourceDbImportError::Stage(..) => (),
                SourceDbImportError::Projection(..) => (),
                SourceDbImportError::Store(..) => (),
                SourceDbImportError::Pack(..) => (),
                SourceDbImportError::Fragments(..) => (),
                SourceDbImportError::ProjectionState(..) => (),
                SourceDbImportError::SeamPublication(..) => (),
                SourceDbImportError::ImportBaseReplacementRefused { .. } => (),
            }
            assert_name_free(&error);
        }
        check_source_db_import_error(SourceDbImportError::PhysicalPublication(example()));
        check_source_db_import_error(SourceDbImportError::PhysicalHeadWithheld {
            vehicle_id: example(),
            legacy_snapshot_id: example(),
        });
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
        check_source_db_import_error(SourceDbImportError::PublicationCapture {
            vehicle_id: example(),
            reason: "the capture directory is full".to_owned(),
        });
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
        check_source_db_import_error(SourceDbImportError::ImportBaseReplacementRefused {
            used: example(),
            limit: example(),
        });
    }
}

#[cfg(test)]
#[path = "importer/import_seam_pack_tests.rs"]
mod import_seam_pack_tests;
