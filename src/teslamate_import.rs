//! TeslaMate full-snapshot migration publisher.
//!
//! The PostgreSQL reader is intentionally separate from this module. Once a
//! reviewed, repeatable-read history exists, this module gives it a stable Hub
//! identity, maps only the selected car, writes one immutable typed pack, and
//! publishes a signed full-snapshot manifest. It does not create fake deltas.

use std::collections::HashSet;

#[path = "performance_profile.rs"]
mod performance_profile;
pub use performance_profile::{
    EffectiveImportProfile, PerformanceProfileError, derive_effective_import_profile,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    credentials::TeslaMatePostgresPassword,
    db::{HubStore, SourceDescriptor, VehicleDescriptor},
    hub_pack::{
        BuiltProjectionPack, ProjectionBinding, ProjectionPackError, ProjectionPackRequest,
        ProjectionPackWriter, signed_full_snapshot_manifest,
    },
    protocol::{CursorKey, SequenceRange, Sha256Digest},
    teslamate::ReadOnlySource,
    teslamate_direct::write_direct_full_snapshot,
    teslamate_fragments::{
        TeslaMateFragmentLimits, write_staged_full_snapshot_with_limits,
    },
    teslamate_projection::{ProjectionReport, TeslaMateCar, TeslaMateHistory, project_vehicle},
    teslamate_projection::{TeslaMateOpenSession, TeslaMateSourceWatermark},
    teslamate_reader::{TeslaMateReadLimits, read_car_ids, read_open_session, read_selected_car},
    teslamate_stage::{TeslaMateStage, TeslaMateStageTable},
};

/// Import scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeslaMateImportScope {
    All,
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
        drives: session.drive.as_ref().map_or_else(
            TeslaMateSourceWatermark::default,
            |row| TeslaMateSourceWatermark {
                max_id: Some(row.id),
                max_timestamp_ms: Some(row.start_date_ms),
            },
        ),
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
        states: session.state.as_ref().map_or_else(
            TeslaMateSourceWatermark::default,
            |row| TeslaMateSourceWatermark {
                max_id: Some(row.id),
                max_timestamp_ms: Some(row.start_date_ms),
            },
        ),
        updates: TeslaMateSourceWatermark::default(),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TeslaMateCarImportSummary {
    pub car_id: i64,
    pub status: String,
    pub vehicle_id: Option<Uuid>,
    pub snapshot_id: Option<Uuid>,
    pub sequence: Option<u64>,
    pub projected_rows: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TeslaMateMultiCarImportSummary {
    pub discovered_car_count: usize,
    pub succeeded_car_count: usize,
    pub skipped_car_count: usize,
    pub failed_car_count: usize,
    pub cars: Vec<TeslaMateCarImportSummary>,
}

struct ImportGenerationGuard<'a> {
    store: &'a HubStore,
    run_id: Option<Uuid>,
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

impl TeslaMateMultiCarImportSummary {
    pub fn has_failures(&self) -> bool {
        self.failed_car_count != 0
    }
}

fn selected_car_id(request: &TeslaMateImportRequest) -> Result<i64, TeslaMateImportError> {
    match request.scope {
        TeslaMateImportScope::Selected(id) if id > 0 => Ok(id),
        TeslaMateImportScope::Selected(_) => Err(TeslaMateImportError::InvalidSelectedCarId),
        TeslaMateImportScope::All => Err(TeslaMateImportError::AllScopeNotReady),
    }
}

/// Read, validate, pack, sign, and publish one complete TeslaMate snapshot.
/// The caller supplies secrets as systemd-derived values; no secret is stored
/// in the Hub database or encoded in the generated pack.
pub async fn import_from_postgres(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    let publication_gate = store.acquire_publication_gate().await?;
    let selected_car_id = selected_car_id(request)?;
    let car = read_selected_car(source, password, selected_car_id, limits).await?;
    let registered_source = store.register_source(
        &SourceDescriptor::new("teslamate", request.source_key.clone()),
        request.imported_at_ms,
    )?;
    let source_vehicle_key = stable_vehicle_key_for_car(&car)?;
    let deterministic_vehicle_id =
        Uuid::new_v5(&registered_source.source_id, source_vehicle_key.as_bytes());
    let vehicle = store.register_vehicle_with_id(
        &VehicleDescriptor {
            source_id: registered_source.source_id,
            source_vehicle_key,
            vin: nonblank(car.vin.as_deref()).map(ToOwned::to_owned),
            display_name: nonblank(car.name.as_deref()).map(ToOwned::to_owned),
            tesla_eid: Some(car.eid),
            tesla_vid: car.vid,
        },
        request.imported_at_ms,
        deterministic_vehicle_id,
    )?;
    let sequence = store.reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)?;
    let snapshot_id = Uuid::new_v4();
    let binding = ProjectionBinding {
        installation_id: store.installation_id()?,
        account_id: registered_source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: registered_source.generation,
        selected_car_id,
    };
    let range = SequenceRange {
        from_exclusive: sequence,
        to_inclusive: sequence,
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
    let mut open_session = match read_open_session(source, password, selected_car_id, limits).await {
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
    let mut direct = match write_direct_full_snapshot(
        source,
        password,
        selected_car_id,
        limits,
        &ProjectionPackWriter::new(store.packs_dir())
            .with_minimum_free_bytes(limits.minimum_free_bytes),
        binding.clone(),
        snapshot_id,
        range,
    )
    .await {
        Ok(value) => value,
        Err(error) => {
            store.abort_import_generation(run_id)?;
            return Err(error.into());
        }
    };
    let mut second_open_session = match read_open_session(source, password, selected_car_id, limits).await {
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
            return Err(error.into());
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
        direct = match write_direct_full_snapshot(
            source,
            password,
            selected_car_id,
            limits,
            &ProjectionPackWriter::new(store.packs_dir())
                .with_minimum_free_bytes(limits.minimum_free_bytes),
            binding.clone(),
            snapshot_id,
            range,
        )
        .await {
            Ok(value) => value,
            Err(error) => {
                store.abort_import_generation(run_id)?;
                return Err(error.into());
            }
        };
    }
    // Always commit the reconciled second tail. `cutover_unsettled` reports
    // that the source kept changing during the bounded pass; it must not
    // discard rows already observed in that pass.
    store.stage_import_generation_session(run_id, &cutover.session)?;
    direct.fingerprint = direct_snapshot_fingerprint(&direct.fingerprint, &direct.geofences)?;
    let logical_rows = direct
        .report
        .logical_row_count()
        .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
    if let Some(current) =
        store.manifest_for_snapshot_fingerprint(vehicle.vehicle_id, direct.fingerprint)?
    {
        direct.keep_chunks();
        discard_unpublished_chunks(store, &direct.chunks)?;
        store.abort_import_generation(run_id)?;
        run_guard.disarm();
        return Ok(TeslaMateImportReport {
            source_id: registered_source.source_id,
            vehicle_id: vehicle.vehicle_id,
            snapshot_id: current.snapshot_id,
            sequence: current.head_sequence,
            projection: direct.report,
            projected_rows: current.total_rows,
            skipped: true,
            cutover_unsettled: cutover.cutover_unsettled,
        });
    }
    let manifest = match signed_full_snapshot_manifest(
        &binding,
        snapshot_id,
        range,
        &direct.chunks,
        logical_rows,
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
    direct.keep_chunks();
    store.finalize_import_generation(
        run_id,
        registered_source.source_id,
        vehicle.vehicle_id,
        selected_car_id,
        request.imported_at_ms,
        &manifest,
        direct.fingerprint,
        &direct.geofences,
    )?;
    run_guard.disarm();
    Ok(TeslaMateImportReport {
        source_id: registered_source.source_id,
        vehicle_id: vehicle.vehicle_id,
        snapshot_id,
        sequence,
        projection: direct.report,
        projected_rows: manifest.total_rows,
        skipped: false,
        cutover_unsettled: cutover.cutover_unsettled,
    })
}

/// Discover all cars once, then run the trusted single-car importer in stable
/// ID order. A car-local projection/identity failure is recorded and the next
/// car continues; source/schema/auth/storage failures stop the coordinator.
pub async fn import_all_from_postgres(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    cursor_key: &CursorKey,
    source_key: &str,
    imported_at_ms: i64,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateMultiCarImportSummary, TeslaMateImportError> {
    let mut car_ids = read_car_ids(source, password, limits).await?;
    car_ids.sort_unstable();
    let mut results = Vec::with_capacity(car_ids.len());
    for car_id in car_ids.iter().copied() {
        let request = TeslaMateImportRequest {
            source_key: source_key.to_owned(),
            scope: TeslaMateImportScope::Selected(car_id),
            imported_at_ms,
        };
        match import_from_postgres(store, source, password, cursor_key, &request, limits).await {
            Ok(report) => results.push((car_id, Ok(report))),
            Err(error) if error.is_global_failure() => return Err(error),
            Err(error) => results.push((car_id, Err(error))),
        }
    }
    finish_multi_car_import(store, &car_ids, results)
}

fn finish_multi_car_import(
    store: &HubStore,
    discovered_car_ids: &[i64],
    results: Vec<(i64, Result<TeslaMateImportReport, TeslaMateImportError>)>,
) -> Result<TeslaMateMultiCarImportSummary, TeslaMateImportError> {
    let mut hub_vehicles = HashSet::new();
    let mut cars = Vec::with_capacity(results.len());
    let mut succeeded = 0;
    let mut skipped = 0;
    let mut failed = 0;

    for (car_id, result) in results {
        match result {
            Ok(report) => {
                if !hub_vehicles.insert(report.vehicle_id) {
                    return Err(TeslaMateImportError::DuplicateHubVehicle);
                }
                let manifest = store
                    .manifest_for_vehicle(report.vehicle_id)?
                    .ok_or(TeslaMateImportError::ManifestMissing { car_id })?;
                if manifest.snapshot_id != report.snapshot_id
                    || manifest.vehicle_id != report.vehicle_id
                    || manifest.head_sequence != report.sequence
                {
                    return Err(TeslaMateImportError::ManifestMismatch { car_id });
                }
                if report.skipped {
                    skipped += 1;
                } else {
                    succeeded += 1;
                }
                cars.push(TeslaMateCarImportSummary {
                    car_id,
                    status: if report.skipped {
                        "skipped".to_owned()
                    } else {
                        "success".to_owned()
                    },
                    vehicle_id: Some(report.vehicle_id),
                    snapshot_id: Some(report.snapshot_id),
                    sequence: Some(report.sequence),
                    projected_rows: Some(report.projected_rows),
                    reason: None,
                });
            }
            Err(error) => {
                failed += 1;
                cars.push(TeslaMateCarImportSummary {
                    car_id,
                    status: "failure".to_owned(),
                    vehicle_id: None,
                    snapshot_id: None,
                    sequence: None,
                    projected_rows: None,
                    reason: Some(error.safe_category().to_owned()),
                });
            }
        }
    }

    store.repair()?;
    store.catalogue_check()?;
    let summary = TeslaMateMultiCarImportSummary {
        discovered_car_count: discovered_car_ids.len(),
        succeeded_car_count: succeeded,
        skipped_car_count: skipped,
        failed_car_count: failed,
        cars,
    };
    Ok(summary)
}

/// Publish a complete sealed capture without materialising its historical
/// vectors. The sealed stage is retained until every immutable fragment has
/// passed its pack verifier and the signed manifest has been stored.
pub fn publish_staged_history(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    stage: &TeslaMateStage,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    publish_staged_history_with_limits(
        store,
        cursor_key,
        request,
        stage,
        TeslaMateFragmentLimits::default(),
    )
}

fn publish_staged_history_with_limits(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    stage: &TeslaMateStage,
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
    let sequence = store.reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)?;
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
    let mut staged = write_staged_full_snapshot_with_limits(
        stage,
        &ProjectionPackWriter::new(store.packs_dir())
            .with_minimum_free_bytes(stage.stats()?.limits.minimum_free_bytes),
        binding.clone(),
        snapshot_id,
        range,
        fragment_limits,
    )?;
    let logical_rows = staged
        .report
        .logical_row_count()
        .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
    if let Some(current) =
        store.manifest_for_snapshot_fingerprint(vehicle.vehicle_id, staged.fingerprint)?
    {
        staged.keep_chunks();
        discard_unpublished_chunks(store, &staged.chunks)?;
        store.upsert_geofences(vehicle.vehicle_id, &staged.geofences)?;
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
    let manifest = signed_full_snapshot_manifest(
        &binding,
        snapshot_id,
        range,
        &staged.chunks,
        logical_rows,
        cursor_key,
    )?;
    staged.keep_chunks();
    store.finalize_import_snapshot(&manifest, staged.fingerprint, &staged.geofences)?;
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

/// Publish an already-read source history. This seam makes the pack/identity
/// path deterministic and testable without a live PostgreSQL server.
pub fn publish_history(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    history: &TeslaMateHistory,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    let publication_gate = store.try_acquire_publication_gate()?;
    let selected_car_id = selected_car_id(request)?;
    let projected = project_vehicle(history, selected_car_id)?;
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
    let sequence = store.reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)?;
    let snapshot_id = Uuid::new_v4();
    let pack_id = Uuid::new_v4();
    let pack = ProjectionPackWriter::new(store.packs_dir()).write_full_snapshot_with_states(
        &ProjectionPackRequest {
            pack_id,
            snapshot_id,
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id: store.installation_id()?,
                account_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                generation: source.generation,
                selected_car_id,
            },
            // A full snapshot has no delta base. Its equal base/head marker
            // identifies this complete replacement in the catalog.
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &projected.snapshot,
        },
        &projected.states,
    )?;
    let manifest = ProjectionPackRequest {
        pack_id,
        snapshot_id,
        ordinal: 0,
        binding: ProjectionBinding {
            installation_id: store.installation_id()?,
            account_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: source.generation,
            selected_car_id,
        },
        sequence: SequenceRange {
            from_exclusive: sequence,
            to_inclusive: sequence,
        },
        snapshot: &projected.snapshot,
    }
    .signed_manifest(&pack, cursor_key)?;
    // `write_full_snapshot` leaves its verified pack in the candidate store
    // before this transaction. A commit makes it catalogued; a failed commit
    // leaves only an unreferenced, repairable orphan.
    store.finalize_import_snapshot(&manifest, fingerprint, &history.geofences)?;

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

fn discard_unpublished_chunks(
    store: &HubStore,
    chunks: &[BuiltProjectionPack],
) -> Result<(), TeslaMateImportError> {
    for chunk in chunks {
        if store.pack_sha256_is_catalogued(&chunk.metadata.sha256.to_string())? {
            continue;
        }
        match std::fs::remove_file(&chunk.path) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(TeslaMateImportError::DiscardUnpublishedPack {
                    path: chunk.path.clone(),
                    source,
                });
            }
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum TeslaMateImportError {
    #[error("all-car import scope is discovery-only until the coordinator is enabled")]
    AllScopeNotReady,
    #[error("one or more TeslaMate cars failed to import")]
    PartialCarFailures,
    #[error("multi-car import produced duplicate Hub vehicle identity")]
    DuplicateHubVehicle,
    #[error("multi-car import has no manifest for car {car_id}")]
    ManifestMissing { car_id: i64 },
    #[error("multi-car import manifest mismatch for car {car_id}")]
    ManifestMismatch { car_id: i64 },
    #[error("TeslaMate selected car id must be positive")]
    InvalidSelectedCarId,
    #[error("TeslaMate selected car disappeared before publication")]
    SelectedCarMissing,
    #[error("current TeslaMate snapshot fingerprint has no published manifest")]
    CurrentSnapshotMissing,
    #[error("TeslaMate source snapshot is too large to fingerprint")]
    SourceFingerprintTooLarge,
    #[error("cannot serialize TeslaMate source snapshot fingerprint: {0}")]
    SourceFingerprint(#[from] serde_json::Error),
    #[error("cannot discard unchanged unpublished pack {path}: {source}")]
    DiscardUnpublishedPack {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("TeslaMate selected car has neither a VIN nor a valid EID")]
    StableVehicleIdentityMissing,
    #[error("TeslaMate cutover snapshots belong to different cars")]
    CutoverCarMismatch,
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
}

impl TeslaMateImportError {
    fn is_global_failure(&self) -> bool {
        match self {
            Self::Store(_)
            | Self::Pack(_)
            | Self::Stage(_)
            | Self::Fragments(_)
            | Self::DiscardUnpublishedPack { .. }
            | Self::DuplicateHubVehicle
            | Self::ManifestMissing { .. }
            | Self::ManifestMismatch { .. } => true,
            Self::SourceFingerprintTooLarge | Self::SourceFingerprint(_) => true,
            Self::Reader(error) => reader_failure_is_global(error),
            Self::Direct(error) => direct_failure_is_global(error),
            Self::Projection(_)
            | Self::SelectedCarMissing
            | Self::InvalidSelectedCarId
            | Self::StableVehicleIdentityMissing
            | Self::AllScopeNotReady
            | Self::PartialCarFailures
            | Self::CutoverCarMismatch => false,
            Self::CurrentSnapshotMissing => true,
        }
    }

    fn safe_category(&self) -> &'static str {
        match self {
            Self::SelectedCarMissing => "selected_car_missing",
            Self::StableVehicleIdentityMissing => "stable_vehicle_identity_missing",
            Self::Projection(_) => "projection_invalid",
            Self::Direct(_) => "car_data_invalid",
            Self::Reader(_) => "car_read_failed",
            Self::InvalidSelectedCarId => "invalid_car_id",
            _ => "import_failed",
        }
    }
}

fn reader_failure_is_global(error: &crate::teslamate_reader::TeslaMateReaderError) -> bool {
    use crate::teslamate_reader::TeslaMateReaderError as Error;
    matches!(
        error,
        Error::SourceUserRequired
            | Error::InvalidConnectTimeout
            | Error::InvalidPageSize
            | Error::InvalidMaximumRows
            | Error::InvalidParallelCopyLanes
            | Error::NativeTrustStoreUnavailable
            | Error::ConnectTimedOut
            | Error::MissingMigrationVersion
            | Error::InvalidExportedSnapshot
            | Error::Schema(_)
            | Error::Stage(_)
            | Error::Postgres(_)
    )
}

fn direct_failure_is_global(error: &crate::teslamate_direct::TeslaMateDirectError) -> bool {
    use crate::teslamate_direct::TeslaMateDirectError as Error;
    match error {
        Error::Postgres(_)
        | Error::Pack(_)
        | Error::Fragment(_)
        | Error::InvalidSourceCount { .. }
        | Error::CountOverflow { .. }
        | Error::UnexplainedSourceRows { .. }
        | Error::NonProgressingPage(_) => true,
        Error::Reader(reader) => reader_failure_is_global(reader),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::*;
    use crate::{
        credentials::{CredentialDirectory, TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL},
        db::HubStore,
        protocol::CursorKey,
        teslamate::ReadOnlySource,
        teslamate_projection::{TeslaMateCar, TeslaMateGeofence, TeslaMateHistory, TeslaMatePosition},
        teslamate_fragments::TeslaMateFragmentLimits,
        teslamate_stage::{TeslaMateStage, TeslaMateStageLimits, TeslaMateStageTable},
    };

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

    fn history_for(car_id: i64, vin: &str) -> TeslaMateHistory {
        let mut source = history();
        source.cars[0].id = car_id;
        source.cars[0].eid = car_id + 100;
        source.cars[0].vin = Some(vin.to_owned());
        source
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
        let cursor_key = CursorKey::from_bytes([7; 32]);
        let first = publish_history(&store, &cursor_key, &request, &history()).unwrap();
        let second = publish_history(&store, &cursor_key, &request, &history()).unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, first.sequence);
        assert!(second.skipped);
        assert_eq!(first.vehicle_id, second.vehicle_id);
        assert_eq!(first.projected_rows, 1);
        let manifest = store
            .manifest_for_vehicle(first.vehicle_id)
            .unwrap()
            .expect("latest manifest");
        assert_eq!(manifest.head_sequence, 2);
        assert_eq!(
            manifest.chunks[0].format,
            crate::protocol::PackFormat::HubProjectionSqlite
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
    fn import_scope_keeps_all_separate_from_selected_validation() {
        assert!(matches!(
            selected_car_id(&TeslaMateImportRequest {
                source_key: "test".into(),
                scope: TeslaMateImportScope::All,
                imported_at_ms: 0,
            }),
            Err(TeslaMateImportError::AllScopeNotReady)
        ));
        assert!(matches!(
            selected_car_id(&TeslaMateImportRequest {
                source_key: "test".into(),
                scope: TeslaMateImportScope::Selected(0),
                imported_at_ms: 0,
            }),
            Err(TeslaMateImportError::InvalidSelectedCarId)
        ));
    }

    #[test]
    fn multi_car_all_success_validates_one_manifest_per_vehicle() {
        let data = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(data.path()).unwrap();
        let key = CursorKey::from_bytes([19; 32]);
        let first = publish_history(
            &store,
            &key,
            &TeslaMateImportRequest {
                source_key: "fixture".into(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms: 1_700_000_000_000,
            },
            &history_for(1, "5YJTESTVIN000001"),
        )
        .unwrap();
        let second = publish_history(
            &store,
            &key,
            &TeslaMateImportRequest {
                source_key: "fixture".into(),
                scope: TeslaMateImportScope::Selected(2),
                imported_at_ms: 1_700_000_000_001,
            },
            &history_for(2, "5YJTESTVIN000002"),
        )
        .unwrap();
        let summary =
            finish_multi_car_import(&store, &[1, 2], vec![(1, Ok(first)), (2, Ok(second))])
                .unwrap();
        assert_eq!(summary.succeeded_car_count, 2);
        assert_eq!(summary.failed_car_count, 0);
        assert_eq!(summary.cars.len(), 2);
    }

    #[test]
    fn multi_car_middle_failure_preserves_completed_manifests() {
        let data = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(data.path()).unwrap();
        let key = CursorKey::from_bytes([20; 32]);
        let first = publish_history(
            &store,
            &key,
            &TeslaMateImportRequest {
                source_key: "fixture".into(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms: 1_700_000_000_000,
            },
            &history_for(1, "5YJTESTVIN000011"),
        )
        .unwrap();
        let third = publish_history(
            &store,
            &key,
            &TeslaMateImportRequest {
                source_key: "fixture".into(),
                scope: TeslaMateImportScope::Selected(3),
                imported_at_ms: 1_700_000_000_002,
            },
            &history_for(3, "5YJTESTVIN000013"),
        )
        .unwrap();
        let summary = finish_multi_car_import(
            &store,
            &[1, 2, 3],
            vec![
                (1, Ok(first.clone())),
                (2, Err(TeslaMateImportError::SelectedCarMissing)),
                (3, Ok(third.clone())),
            ],
        )
        .unwrap();
        assert_eq!(summary.succeeded_car_count, 2);
        assert_eq!(summary.failed_car_count, 1);
        assert_eq!(
            summary.cars[1].reason.as_deref(),
            Some("selected_car_missing")
        );
        assert!(
            store
                .manifest_for_vehicle(first.vehicle_id)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .manifest_for_vehicle(third.vehicle_id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn multi_car_rerun_marks_unchanged_success_as_skipped() {
        let data = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(data.path()).unwrap();
        let key = CursorKey::from_bytes([21; 32]);
        let published = publish_history(
            &store,
            &key,
            &TeslaMateImportRequest {
                source_key: "fixture".into(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms: 1_700_000_000_000,
            },
            &history_for(1, "5YJTESTVIN000021"),
        )
        .unwrap();
        let rerun = TeslaMateImportReport {
            skipped: true,
            ..published
        };
        let summary = finish_multi_car_import(&store, &[1], vec![(1, Ok(rerun))]).unwrap();
        assert_eq!(summary.skipped_car_count, 1);
        assert_eq!(summary.cars[0].status, "skipped");
    }

    #[test]
    fn selected_scope_and_zero_car_scope_are_isolated() {
        let selected_data = tempfile::tempdir().unwrap();
        let selected_store = HubStore::initialize(selected_data.path()).unwrap();
        let key = CursorKey::from_bytes([22; 32]);
        let selected = publish_history(
            &selected_store,
            &key,
            &TeslaMateImportRequest {
                source_key: "fixture".into(),
                scope: TeslaMateImportScope::Selected(7),
                imported_at_ms: 1_700_000_000_000,
            },
            &history_for(7, "5YJTESTVIN000027"),
        )
        .unwrap();
        let selected_summary =
            finish_multi_car_import(&selected_store, &[7], vec![(7, Ok(selected))]).unwrap();
        assert_eq!(selected_summary.discovered_car_count, 1);
        assert_eq!(selected_summary.cars[0].car_id, 7);

        let empty_data = tempfile::tempdir().unwrap();
        let empty_store = HubStore::initialize(empty_data.path()).unwrap();
        let empty = finish_multi_car_import(&empty_store, &[], Vec::new()).unwrap();
        assert_eq!(empty.discovered_car_count, 0);
        assert_eq!(empty.failed_car_count, 0);
        assert!(empty.cars.is_empty());
    }

    #[test]
    fn sealed_stage_publication_never_needs_an_in_memory_history() {
        let data = tempfile::tempdir().unwrap();
        let imports = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(data.path()).unwrap();
        let mut stage = TeslaMateStage::create(
            imports.path(),
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
    fn staged_publication_adapts_before_the_protocol_chunk_ceiling() {
        let data = tempfile::tempdir().unwrap();
        let imports = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(data.path()).unwrap();
        let mut stage = TeslaMateStage::create(
            imports.path(),
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
            position.date_ms += i64::from(id);
            stage
                .insert(TeslaMateStageTable::Positions, id, &position)
                .unwrap();
        }
        stage.seal().unwrap();

        let report = publish_staged_history_with_limits(
            &store,
            &CursorKey::from_bytes([12; 32]),
            &TeslaMateImportRequest {
                source_key: "home-teslamate".into(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms: 1_700_000_000_000,
            },
            &stage,
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
        let credentials = tempfile::tempdir().expect("credential directory");
        let credential_path = credentials
            .path()
            .join(TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL);
        fs::write(&credential_path, "fixture-password").expect("fixture password");
        fs::set_permissions(&credential_path, fs::Permissions::from_mode(0o600))
            .expect("private fixture password");
        let password = CredentialDirectory::from_path(credentials.path())
            .teslamate_postgres_password()
            .expect("fixture password credential");
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

        assert_eq!(report.projected_rows, 6);
        let manifest = store
            .manifest_for_vehicle(report.vehicle_id)
            .expect("manifest query")
            .expect("published manifest");
        assert_eq!(manifest.snapshot_id, report.snapshot_id);
        assert_eq!(manifest.total_rows, 6);
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
            drives: TeslaMateSourceWatermark { max_id: Some(7), max_timestamp_ms: Some(1_000) },
            positions: position,
            charging_processes: TeslaMateSourceWatermark { max_id: Some(8), max_timestamp_ms: Some(1_000) },
            charges: charge,
            states: TeslaMateSourceWatermark { max_id: Some(20), max_timestamp_ms: Some(1_000) },
            updates: TeslaMateSourceWatermark::default(),
        }
    }

    fn open_session(position_ids: &[i64], sample_ids: &[i64], standalone_ids: &[i64]) -> TeslaMateOpenSession {
        TeslaMateOpenSession {
            car_id: 1,
            drive: Some(drive()),
            drive_positions: position_ids.iter().map(|id| position(*id, Some(7))).collect(),
            charge: Some(process()),
            charge_samples: sample_ids.iter().map(|id| sample(*id)).collect(),
            state: Some(state()),
            standalone_positions: standalone_ids.iter().map(|id| position(*id, None)).collect(),
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
        assert!(store
            .seed_imported_open_session(
                source.source_id,
                vehicle.vehicle_id,
                1,
                &cutover.session,
                2_000,
            )
            .expect("duplicate merge")
            .no_op);

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
        assert!(reopened
            .seed_imported_open_session(
                source.source_id,
                vehicle.vehicle_id,
                1,
                &invalid,
                3_000,
            )
            .is_err());
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
