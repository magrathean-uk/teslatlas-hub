//! Bounded full-snapshot pack production from a sealed TeslaMate stage.
//!
//! The PostgreSQL reader owns capture consistency. This producer owns only
//! the local sealed stage and never reconstructs a whole `TeslaMateHistory`.
//! Every generated pack is self-contained: it repeats its car and the parent
//! drive or charge rows required by its children. The manifest is published by
//! the caller only after this module has verified every immutable pack.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    hub_pack::{
        BuiltProjectionPack, ProjectionBinding, ProjectionCar, ProjectionCharge,
        ProjectionChargeSample, ProjectionDrive, ProjectionPackRequest, ProjectionPackWriter,
        ProjectionPosition, ProjectionSnapshot, ProjectionState, ProjectionUpdate,
    },
    protocol::{ProtocolLimits, SequenceRange},
    teslamate_parity::{TeslaMateSourceEvidenceError, TeslaMateSourceEvidenceFingerprint},
    teslamate_projection::{
        ChargeProjectionFacts, DriveRelations, ProjectionReport, TeslaMateAddress, TeslaMateCar,
        TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive, TeslaMateGeofence,
        TeslaMatePosition, TeslaMateProjectionError, TeslaMateState, TeslaMateUpdate, project_car,
        project_charge, project_charge_sample, project_drive, project_position, project_state,
        project_update,
    },
    teslamate_projection_state::{TeslaMateProjectionStateCapture, TeslaMateProjectionStateError},
    teslamate_stage::{TeslaMateStage, TeslaMateStageError, TeslaMateStageTable},
};

const STAGE_PAGE_ROWS: u32 = 10_000;
const DEFAULT_MAX_ROWS_PER_FRAGMENT: u64 = 50_000;
const DEFAULT_MAX_PROJECTED_JSON_BYTES: u64 = 8 * 1024 * 1024;
const DENSE_STAGE_ROW_THRESHOLD: u64 = 10_000_000;

/// Production target beneath the protocol ceilings. It is deliberately an
/// encoder target rather than a client promise: pack verification remains the
/// final boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeslaMateFragmentLimits {
    pub max_rows_per_fragment: u64,
    pub max_projected_json_bytes: u64,
}

impl Default for TeslaMateFragmentLimits {
    fn default() -> Self {
        Self {
            max_rows_per_fragment: DEFAULT_MAX_ROWS_PER_FRAGMENT,
            max_projected_json_bytes: DEFAULT_MAX_PROJECTED_JSON_BYTES,
        }
    }
}

impl TeslaMateFragmentLimits {
    fn validate(self) -> Result<(), TeslaMateFragmentError> {
        if self.max_rows_per_fragment < 3 {
            return Err(TeslaMateFragmentError::FragmentRowTargetTooSmall);
        }
        if self.max_rows_per_fragment > ProtocolLimits::default().max_rows_per_pack {
            return Err(TeslaMateFragmentError::FragmentRowTargetTooLarge);
        }
        if self.max_projected_json_bytes == 0
            || self.max_projected_json_bytes > ProtocolLimits::default().max_uncompressed_pack_bytes
        {
            return Err(TeslaMateFragmentError::FragmentByteTargetInvalid);
        }
        Ok(())
    }
}

/// Verified objects and exact accounting from one sealed stage. The caller
/// signs the manifest with these chunks, then publishes it atomically.
#[derive(Debug)]
pub struct StagedProjectionPacks {
    pub chunks: Vec<BuiltProjectionPack>,
    pub report: ProjectionReport,
    pub fingerprint: crate::protocol::Sha256Digest,
    /// Only the one-time legacy-direct bridge requests this historical,
    /// fragment-layout-dependent digest. New direct imports deliberately use
    /// `fingerprint` above, whose domain is independent of fragment layout.
    pub(crate) legacy_physical_fingerprint: Option<crate::protocol::Sha256Digest>,
    pub geofences: Vec<TeslaMateGeofence>,
    /// The selected car emitted by every verified fragment. It is retained so
    /// a geofence-only source change can still produce a legal non-empty V2
    /// delta without retaining every otherwise unchanged payload.
    pub(crate) selected_car: Option<ProjectionCar>,
    /// A private current-run digest index, retained only until the caller
    /// atomically publishes or discards this candidate.
    pub(crate) projection_state: Option<TeslaMateProjectionStateCapture>,
    cleanup_on_drop: bool,
}

impl StagedProjectionPacks {
    #[cfg(test)]
    pub(crate) fn new(
        chunks: Vec<BuiltProjectionPack>,
        report: ProjectionReport,
        fingerprint: crate::protocol::Sha256Digest,
        geofences: Vec<TeslaMateGeofence>,
    ) -> Self {
        Self::new_with_projection_state(chunks, report, fingerprint, None, geofences, None, None)
    }

    pub(crate) fn new_with_projection_state(
        chunks: Vec<BuiltProjectionPack>,
        report: ProjectionReport,
        fingerprint: crate::protocol::Sha256Digest,
        legacy_physical_fingerprint: Option<crate::protocol::Sha256Digest>,
        geofences: Vec<TeslaMateGeofence>,
        projection_state: Option<TeslaMateProjectionStateCapture>,
        selected_car: Option<ProjectionCar>,
    ) -> Self {
        Self {
            chunks,
            report,
            fingerprint,
            legacy_physical_fingerprint,
            geofences,
            selected_car,
            projection_state,
            cleanup_on_drop: true,
        }
    }

    /// Published chunks are owned by the Hub catalogue, not this candidate.
    pub(crate) fn keep_chunks(&mut self) {
        self.cleanup_on_drop = false;
    }
}

impl Drop for StagedProjectionPacks {
    fn drop(&mut self) {
        if !self.cleanup_on_drop {
            return;
        }
        for chunk in self.chunks.drain(..) {
            if !chunk.may_remove_unpublished_file() {
                continue;
            }
            if let Err(error) = fs::remove_file(&chunk.path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %chunk.path.display(), %error, "could not remove unpublished TeslaMate candidate pack");
            }
        }
    }
}

/// Produce all schema-2.1 typed packs for a complete staged TeslaMate
/// snapshot. This function writes content-addressed objects but does not alter
/// the Hub catalog. If it returns an error, no manifest can point at partial
/// output.
///
/// A sealed source stage is a full-fidelity migration boundary, not a legacy
/// seed encoder. Always choose schema 2.1 here: detecting just a few known
/// widened fields would inevitably miss a later additive field and silently
/// turn a complete source capture into a lossy schema-2.0 pack. The explicit
/// legacy-direct bridge is the only separate historical-layout path.
pub fn write_staged_full_snapshot(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
) -> Result<StagedProjectionPacks, TeslaMateFragmentError> {
    write_staged_full_snapshot_with_limits(
        stage,
        writer,
        binding,
        snapshot_id,
        sequence,
        TeslaMateFragmentLimits::default(),
    )
}

pub fn write_staged_full_snapshot_with_limits(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    limits: TeslaMateFragmentLimits,
) -> Result<StagedProjectionPacks, TeslaMateFragmentError> {
    write_staged_full_snapshot_with_capture_factory(
        stage,
        writer,
        binding,
        snapshot_id,
        sequence,
        limits,
        || Ok(None),
    )
}

/// As [`write_staged_full_snapshot_with_limits`], while retaining one sealed
/// digest capture for the exact full-snapshot candidate. A fragment-limit
/// retry obtains a fresh capture, so a failed attempt cannot contaminate the
/// durable state of its replacement attempt.
pub(crate) fn write_staged_full_snapshot_with_projection_state<F, E>(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    limits: TeslaMateFragmentLimits,
    mut capture_factory: F,
) -> Result<StagedProjectionPacks, E>
where
    F: FnMut() -> Result<TeslaMateProjectionStateCapture, E>,
    E: From<TeslaMateFragmentError>,
{
    write_staged_full_snapshot_with_capture_factory(
        stage,
        writer,
        binding,
        snapshot_id,
        sequence,
        limits,
        || capture_factory().map(Some),
    )
}

fn write_staged_full_snapshot_with_capture_factory<F, E>(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    limits: TeslaMateFragmentLimits,
    mut capture_factory: F,
) -> Result<StagedProjectionPacks, E>
where
    F: FnMut() -> Result<Option<TeslaMateProjectionStateCapture>, E>,
    E: From<TeslaMateFragmentError>,
{
    limits.validate().map_err(E::from)?;
    let stage_rows = stage
        .stats()
        .map_err(TeslaMateFragmentError::from)
        .map_err(E::from)?
        .row_count;
    let mut effective_limits = initial_staged_fragment_limits(stage_rows, limits);
    loop {
        let projection_state = capture_factory()?;
        match write_staged_full_snapshot_once(
            stage,
            writer,
            binding.clone(),
            snapshot_id,
            sequence,
            effective_limits,
            projection_state,
        ) {
            Ok(packs) => return Ok(packs),
            Err(TeslaMateFragmentError::TooManyFragments) => {
                effective_limits = next_fragment_limits(effective_limits)
                    .ok_or(TeslaMateFragmentError::TooManyFragments)
                    .map_err(E::from)?;
            }
            Err(error) => return Err(E::from(error)),
        }
    }
}

fn initial_staged_fragment_limits(
    stage_rows: u64,
    requested: TeslaMateFragmentLimits,
) -> TeslaMateFragmentLimits {
    if stage_rows >= DENSE_STAGE_ROW_THRESHOLD && requested == TeslaMateFragmentLimits::default() {
        next_fragment_limits(requested).unwrap_or(requested)
    } else {
        requested
    }
}

pub(crate) fn next_fragment_limits(
    current: TeslaMateFragmentLimits,
) -> Option<TeslaMateFragmentLimits> {
    let protocol = ProtocolLimits::default();
    let next = TeslaMateFragmentLimits {
        max_rows_per_fragment: current
            .max_rows_per_fragment
            .saturating_mul(2)
            .min(protocol.max_rows_per_pack),
        max_projected_json_bytes: current
            .max_projected_json_bytes
            .saturating_mul(2)
            .min(protocol.max_uncompressed_pack_bytes),
    };
    (next != current).then_some(next)
}

fn write_staged_full_snapshot_once(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    limits: TeslaMateFragmentLimits,
    projection_state: Option<TeslaMateProjectionStateCapture>,
) -> Result<StagedProjectionPacks, TeslaMateFragmentError> {
    if snapshot_id.is_nil() {
        return Err(TeslaMateFragmentError::NilSnapshotId);
    }
    if binding.selected_car_id <= 0 {
        return Err(TeslaMateFragmentError::InvalidSelectedCarId);
    }
    if !sequence.is_ordered() {
        return Err(TeslaMateFragmentError::UnorderedSequence);
    }
    // `stats` also rejects an unsealed stage before any pack writes occur.
    let stage_stats = stage.stats()?;
    writer.ensure_full_snapshot_capacity_for_capture(
        stage_stats.limits.max_stage_bytes,
        stage_stats.limits.minimum_free_bytes,
    )?;
    let car =
        required_row::<TeslaMateCar>(stage, TeslaMateStageTable::Cars, binding.selected_car_id)?;
    let update_summary = staged_update_summary(stage, binding.selected_car_id)?;
    let source_evidence_fingerprint = staged_source_evidence_fingerprint(stage)?;
    let projected_car = project_car(&car, update_summary.latest_firmware.clone())?;
    let states = project_staged_states(stage, binding.selected_car_id)?;
    let projected_states =
        u64::try_from(states.len()).map_err(|_| TeslaMateFragmentError::ReportOverflow)?;
    let geofences = staged_geofences(stage)?;
    let mut sink =
        PackSink::new_with_schema_2_1(writer, binding, snapshot_id, sequence, states, true)
            .with_source_evidence_fingerprint(source_evidence_fingerprint);
    if let Some(projection_state) = projection_state {
        sink = sink.with_projection_state_capture(projection_state);
    }
    let mut report = ProjectionReport {
        projected_states,
        ..ProjectionReport::default()
    };

    write_drive_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;
    write_position_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;
    write_charge_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;
    let emitted_updates =
        write_staged_update_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;
    if emitted_updates != update_summary {
        return Err(TeslaMateFragmentError::UpdateProjectionReconciliation);
    }

    // A new car with no history is still a legitimate complete replacement.
    if sink.submitted_fragments == 0 {
        let accumulator = FragmentAccumulator::new(projected_car, limits)?;
        sink.write(accumulator.finish())?;
    }
    sink.finish()?;
    let fingerprint = sink
        .fingerprint()
        .expect("staged PackSink retains its physical fingerprint");
    let (chunks, projection_state, selected_car) = sink.into_parts();
    Ok(StagedProjectionPacks::new_with_projection_state(
        chunks,
        report,
        fingerprint,
        None,
        geofences,
        projection_state,
        selected_car,
    ))
}

fn staged_geofences(
    stage: &TeslaMateStage,
) -> Result<Vec<TeslaMateGeofence>, TeslaMateFragmentError> {
    let mut geofences = Vec::new();
    for_each_page::<TeslaMateGeofence, _>(stage, TeslaMateStageTable::Geofences, |geofence| {
        geofences.push(geofence);
        Ok(())
    })?;
    Ok(geofences)
}

/// Bind facts from the sealed typed stage before any lossy THP1 projection.
/// Drives and states are independently keyset-ordered, and the evidence
/// accumulator combines those streams in a fixed type order. Other capture
/// lanes therefore cannot perturb the digest.
fn staged_source_evidence_fingerprint(
    stage: &TeslaMateStage,
) -> Result<crate::protocol::Sha256Digest, TeslaMateFragmentError> {
    let mut evidence = TeslaMateSourceEvidenceFingerprint::new();
    for_each_page::<TeslaMateDrive, _>(stage, TeslaMateStageTable::Drives, |drive| {
        evidence.record_drive(&drive)?;
        Ok(())
    })?;
    for_each_page::<TeslaMateState, _>(stage, TeslaMateStageTable::States, |state| {
        evidence.record_state(&state)?;
        Ok(())
    })?;
    Ok(evidence.finish())
}

fn project_staged_states(
    stage: &TeslaMateStage,
    selected_car_id: i64,
) -> Result<Vec<ProjectionState>, TeslaMateFragmentError> {
    let mut states = Vec::new();
    for_each_page::<TeslaMateState, _>(stage, TeslaMateStageTable::States, |state| {
        if let Some(projected) = project_state(&state, selected_car_id)? {
            states.push(projected);
        }
        Ok(())
    })?;
    Ok(states)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StagedUpdateSummary {
    latest_firmware: Option<String>,
    projected_updates: u64,
    skipped_incomplete_updates: u64,
}

fn staged_update_summary(
    stage: &TeslaMateStage,
    selected_car_id: i64,
) -> Result<StagedUpdateSummary, TeslaMateFragmentError> {
    let mut summary = StagedUpdateSummary {
        latest_firmware: None,
        projected_updates: 0,
        skipped_incomplete_updates: 0,
    };
    let mut latest = None::<((i64, i64, i64), String)>;
    for_each_page::<TeslaMateUpdate, _>(stage, TeslaMateStageTable::Updates, |update| {
        if update.car_id != selected_car_id {
            return Err(TeslaMateFragmentError::UpdateWrongCar {
                update_id: update.id,
                expected_car_id: selected_car_id,
                found_car_id: update.car_id,
            });
        }
        match project_update(&update, selected_car_id)? {
            Some(projected) => {
                summary.projected_updates = summary
                    .projected_updates
                    .checked_add(1)
                    .ok_or(TeslaMateFragmentError::ReportOverflow)?;
                let order = (projected.end_date_ms, projected.start_date_ms, projected.id);
                if latest.as_ref().is_none_or(|(current, _)| order > *current) {
                    latest = Some((order, projected.version));
                }
            }
            None => {
                summary.skipped_incomplete_updates = summary
                    .skipped_incomplete_updates
                    .checked_add(1)
                    .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            }
        }
        Ok(())
    })?;
    summary.latest_firmware = latest.map(|(_, version)| version);
    Ok(summary)
}

fn write_staged_update_fragments(
    stage: &TeslaMateStage,
    car: &ProjectionCar,
    sink: &mut PackSink<'_>,
    limits: TeslaMateFragmentLimits,
    report: &mut ProjectionReport,
) -> Result<StagedUpdateSummary, TeslaMateFragmentError> {
    let mut summary = StagedUpdateSummary {
        latest_firmware: None,
        projected_updates: 0,
        skipped_incomplete_updates: 0,
    };
    let mut latest = None::<((i64, i64, i64), String)>;
    let mut accumulator = UpdateFragmentAccumulator::new(car.clone(), limits)?;
    for_each_page::<TeslaMateUpdate, _>(stage, TeslaMateStageTable::Updates, |update| {
        if update.car_id != car.id {
            return Err(TeslaMateFragmentError::UpdateWrongCar {
                update_id: update.id,
                expected_car_id: car.id,
                found_car_id: update.car_id,
            });
        }
        match project_update(&update, car.id)? {
            Some(projected) => {
                let order = (projected.end_date_ms, projected.start_date_ms, projected.id);
                if latest.as_ref().is_none_or(|(current, _)| order > *current) {
                    latest = Some((order, projected.version.clone()));
                }
                accumulator.push(sink, projected)?;
                summary.projected_updates = summary
                    .projected_updates
                    .checked_add(1)
                    .ok_or(TeslaMateFragmentError::ReportOverflow)?;
                report.projected_updates = report
                    .projected_updates
                    .checked_add(1)
                    .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            }
            None => {
                summary.skipped_incomplete_updates = summary
                    .skipped_incomplete_updates
                    .checked_add(1)
                    .ok_or(TeslaMateFragmentError::ReportOverflow)?;
                report.skipped_incomplete_updates = report
                    .skipped_incomplete_updates
                    .checked_add(1)
                    .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            }
        }
        Ok(())
    })?;
    accumulator.flush(sink)?;
    summary.latest_firmware = latest.map(|(_, version)| version);
    Ok(summary)
}

fn write_drive_fragments(
    stage: &TeslaMateStage,
    car: &ProjectionCar,
    sink: &mut PackSink<'_>,
    limits: TeslaMateFragmentLimits,
    report: &mut ProjectionReport,
) -> Result<(), TeslaMateFragmentError> {
    let mut accumulator = FragmentAccumulator::new(car.clone(), limits)?;
    for_each_page::<TeslaMateDrive, _>(stage, TeslaMateStageTable::Drives, |drive| {
        let Some(projected) = project_drive_from_stage(stage, &drive, car.id)? else {
            report.skipped_open_drives = report
                .skipped_open_drives
                .checked_add(1)
                .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            return Ok(());
        };
        accumulator.prepare(sink, |_| Ok((1, serialized_bytes(&projected)?)))?;
        accumulator.drives.push(projected);
        report.completed_drives = report
            .completed_drives
            .checked_add(1)
            .ok_or(TeslaMateFragmentError::ReportOverflow)?;
        Ok(())
    })?;
    accumulator.flush(sink)
}

fn write_position_fragments(
    stage: &TeslaMateStage,
    car: &ProjectionCar,
    sink: &mut PackSink<'_>,
    limits: TeslaMateFragmentLimits,
    report: &mut ProjectionReport,
) -> Result<(), TeslaMateFragmentError> {
    let mut projected_drives = HashMap::new();
    for_each_page::<TeslaMateDrive, _>(stage, TeslaMateStageTable::Drives, |drive| {
        if let Some(projected) = project_drive_from_stage(stage, &drive, car.id)? {
            projected_drives.insert(drive.id, projected);
        }
        Ok(())
    })?;
    let projected_drives = Arc::new(projected_drives);
    let mut projection_pool = PositionProjectionPool::new(car.id, projected_drives.clone());
    let mut accumulator = FragmentAccumulator::new(car.clone(), limits)?;
    let mut ordinal = 0_u64;
    let mut after_id = 0_i64;
    loop {
        let page = stage.page::<TeslaMatePosition>(
            TeslaMateStageTable::Positions,
            after_id,
            STAGE_PAGE_ROWS,
        )?;
        let next_after_id = page.next_after_id;
        let completed = projection_pool.submit(PositionProjectionJob {
            ordinal,
            rows: page.rows,
            car_id: car.id,
        })?;
        ordinal = ordinal
            .checked_add(1)
            .ok_or(TeslaMateFragmentError::ReportOverflow)?;
        if let Some(completed) = completed {
            append_projected_positions(completed, &mut accumulator, sink, report)?;
        }
        match next_after_id {
            Some(next_after_id) => after_id = next_after_id,
            None => break,
        }
    }
    for completed in projection_pool.finish()? {
        append_projected_positions(completed, &mut accumulator, sink, report)?;
    }
    accumulator.flush(sink)
}

fn append_projected_positions(
    page: PositionProjectionResult,
    accumulator: &mut FragmentAccumulator,
    sink: &mut PackSink<'_>,
    report: &mut ProjectionReport,
) -> Result<(), TeslaMateFragmentError> {
    report.skipped_unattached_positions = report
        .skipped_unattached_positions
        .checked_add(page.skipped_unattached)
        .ok_or(TeslaMateFragmentError::ReportOverflow)?;
    for projected in page.rows? {
        let position = projected.position;
        let Some(drive) = projected.drive else {
            accumulator.prepare(sink, |_| Ok((1, serialized_bytes(&position)?)))?;
            accumulator.positions.push(position);
            report.projected_positions = report
                .projected_positions
                .checked_add(1)
                .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            continue;
        };
        accumulator.prepare(sink, |current| {
            let drive_is_new = !current.drive_ids.contains(&drive.id);
            let added_rows = 1 + u64::from(drive_is_new);
            let added_bytes = serialized_bytes(&position)?
                .checked_add(if drive_is_new {
                    serialized_bytes(&drive)?
                } else {
                    0
                })
                .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?;
            Ok((added_rows, added_bytes))
        })?;
        if accumulator.drive_ids.insert(drive.id) {
            accumulator.drives.push(drive);
        }
        accumulator.positions.push(position);
        report.projected_positions = report
            .projected_positions
            .checked_add(1)
            .ok_or(TeslaMateFragmentError::ReportOverflow)?;
    }
    Ok(())
}

fn write_charge_fragments(
    stage: &TeslaMateStage,
    car: &ProjectionCar,
    sink: &mut PackSink<'_>,
    limits: TeslaMateFragmentLimits,
    report: &mut ProjectionReport,
) -> Result<(), TeslaMateFragmentError> {
    let mut empty_processes = FragmentAccumulator::new(car.clone(), limits)?;
    let mut samples = FragmentAccumulator::new(car.clone(), limits)?;
    for_each_page::<TeslaMateChargingProcess, _>(
        stage,
        TeslaMateStageTable::ChargingProcesses,
        |process| {
            let (projected_charge, sample_facts) =
                project_charge_from_stage(stage, &process, car.id)?;
            report.projected_charges = report
                .projected_charges
                .checked_add(1)
                .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            let sample_count =
                append_charge_samples(stage, &projected_charge, &mut samples, sink, report)?;
            if sample_count == 0 {
                empty_processes.prepare(sink, |_| Ok((1, serialized_bytes(&projected_charge)?)))?;
                empty_processes.charges.push(projected_charge);
            }
            // Prevent accidental refactors from dropping the bounded aggregate
            // scan before the parent charge has been derived and checked.
            let _ = sample_facts;
            Ok(())
        },
    )?;
    empty_processes.flush(sink)?;
    samples.flush(sink)
}

fn append_charge_samples(
    stage: &TeslaMateStage,
    charge: &ProjectionCharge,
    accumulator: &mut FragmentAccumulator,
    sink: &mut PackSink<'_>,
    report: &mut ProjectionReport,
) -> Result<u64, TeslaMateFragmentError> {
    let mut after_id = 0_i64;
    let mut count = 0_u64;
    loop {
        let page = stage.charge_samples_for_process::<TeslaMateCharge>(
            charge.id,
            after_id,
            STAGE_PAGE_ROWS,
        )?;
        for row in page.rows {
            if row.value.charging_process_id != charge.id {
                return Err(TeslaMateFragmentError::SampleProcessMismatch {
                    sample_id: row.source_id,
                    expected_process_id: charge.id,
                    found_process_id: row.value.charging_process_id,
                });
            }
            let projected = project_charge_sample(&row.value);
            accumulator.prepare(sink, |current| {
                let charge_is_new = !current.charge_ids.contains(&charge.id);
                let added_rows = 1 + u64::from(charge_is_new);
                let added_bytes = serialized_bytes(&projected)?
                    .checked_add(if charge_is_new {
                        serialized_bytes(charge)?
                    } else {
                        0
                    })
                    .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?;
                Ok((added_rows, added_bytes))
            })?;
            if accumulator.charge_ids.insert(charge.id) {
                accumulator.charges.push(charge.clone());
            }
            accumulator.charge_samples.push(projected);
            count = count
                .checked_add(1)
                .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            report.projected_charge_samples = report
                .projected_charge_samples
                .checked_add(1)
                .ok_or(TeslaMateFragmentError::ReportOverflow)?;
        }
        match page.next_after_id {
            Some(next_after_id) => after_id = next_after_id,
            None => return Ok(count),
        }
    }
}

fn project_drive_from_stage(
    stage: &TeslaMateStage,
    drive: &TeslaMateDrive,
    selected_car_id: i64,
) -> Result<Option<ProjectionDrive>, TeslaMateFragmentError> {
    let start_position: Option<TeslaMatePosition> = optional_related_row(
        stage,
        TeslaMateStageTable::Positions,
        drive.start_position_id,
    )?;
    let end_position: Option<TeslaMatePosition> =
        optional_related_row(stage, TeslaMateStageTable::Positions, drive.end_position_id)?;
    let start_address: Option<TeslaMateAddress> = optional_related_row(
        stage,
        TeslaMateStageTable::Addresses,
        drive.start_address_id,
    )?;
    let end_address: Option<TeslaMateAddress> =
        optional_related_row(stage, TeslaMateStageTable::Addresses, drive.end_address_id)?;
    let start_geofence: Option<TeslaMateGeofence> = optional_related_row(
        stage,
        TeslaMateStageTable::Geofences,
        drive.start_geofence_id,
    )?;
    let end_geofence: Option<TeslaMateGeofence> =
        optional_related_row(stage, TeslaMateStageTable::Geofences, drive.end_geofence_id)?;
    project_drive(
        drive,
        selected_car_id,
        DriveRelations {
            start_position: start_position.as_ref(),
            end_position: end_position.as_ref(),
            start_address: start_address.as_ref(),
            end_address: end_address.as_ref(),
            start_geofence: start_geofence.as_ref(),
            end_geofence: end_geofence.as_ref(),
        },
    )
    .map_err(Into::into)
}

fn project_charge_from_stage(
    stage: &TeslaMateStage,
    process: &TeslaMateChargingProcess,
    selected_car_id: i64,
) -> Result<(ProjectionCharge, ChargeProjectionFacts), TeslaMateFragmentError> {
    let facts = charge_facts(stage, process.id)?;
    let position: Option<TeslaMatePosition> =
        optional_related_row(stage, TeslaMateStageTable::Positions, process.position_id)?;
    let address: Option<TeslaMateAddress> =
        optional_related_row(stage, TeslaMateStageTable::Addresses, process.address_id)?;
    let geofence: Option<TeslaMateGeofence> =
        optional_related_row(stage, TeslaMateStageTable::Geofences, process.geofence_id)?;
    let charge = project_charge(
        process,
        selected_car_id,
        position.as_ref(),
        address.as_ref(),
        geofence.as_ref(),
        &facts,
    )?;
    Ok((charge, facts))
}

fn charge_facts(
    stage: &TeslaMateStage,
    process_id: i64,
) -> Result<ChargeProjectionFacts, TeslaMateFragmentError> {
    let mut facts = ChargeProjectionFacts::default();
    let mut after_id = 0_i64;
    loop {
        let page = stage.charge_samples_for_process::<TeslaMateCharge>(
            process_id,
            after_id,
            STAGE_PAGE_ROWS,
        )?;
        for row in page.rows {
            if row.value.charging_process_id != process_id {
                return Err(TeslaMateFragmentError::SampleProcessMismatch {
                    sample_id: row.source_id,
                    expected_process_id: process_id,
                    found_process_id: row.value.charging_process_id,
                });
            }
            facts.observe(&row.value);
        }
        match page.next_after_id {
            Some(next_after_id) => after_id = next_after_id,
            None => return Ok(facts),
        }
    }
}

fn optional_related_row<T: serde::de::DeserializeOwned>(
    stage: &TeslaMateStage,
    table: TeslaMateStageTable,
    id: Option<i64>,
) -> Result<Option<T>, TeslaMateFragmentError> {
    let Some(id) = id else {
        return Ok(None);
    };
    stage
        .get(table, id)?
        .ok_or(TeslaMateFragmentError::MissingStageRelation {
            table: table.as_str(),
            source_id: id,
        })
        .map(Some)
}

fn required_row<T: serde::de::DeserializeOwned>(
    stage: &TeslaMateStage,
    table: TeslaMateStageTable,
    id: i64,
) -> Result<T, TeslaMateFragmentError> {
    stage
        .get(table, id)?
        .ok_or(TeslaMateFragmentError::MissingStageRelation {
            table: table.as_str(),
            source_id: id,
        })
}

fn for_each_page<T, F>(
    stage: &TeslaMateStage,
    table: TeslaMateStageTable,
    mut operation: F,
) -> Result<(), TeslaMateFragmentError>
where
    T: serde::de::DeserializeOwned,
    F: FnMut(T) -> Result<(), TeslaMateFragmentError>,
{
    let mut after_id = 0_i64;
    loop {
        let page = stage.page::<T>(table, after_id, STAGE_PAGE_ROWS)?;
        for row in page.rows {
            operation(row.value)?;
        }
        match page.next_after_id {
            Some(next_after_id) => after_id = next_after_id,
            None => return Ok(()),
        }
    }
}

struct PackBuildJob {
    ordinal: u32,
    snapshot_id: Uuid,
    binding: ProjectionBinding,
    sequence: SequenceRange,
    snapshot: ProjectionSnapshot,
    states: Vec<ProjectionState>,
    updates: Vec<ProjectionUpdate>,
    schema_2_1: bool,
}

struct PackBuildResult {
    ordinal: u32,
    snapshot: ProjectionSnapshot,
    updates: Vec<ProjectionUpdate>,
    built: Result<BuiltProjectionPack, crate::hub_pack::ProjectionPackError>,
}

struct PositionProjectionJob {
    ordinal: u64,
    rows: Vec<crate::teslamate_stage::TeslaMateStageRow<TeslaMatePosition>>,
    car_id: i64,
}

struct ProjectedPosition {
    position: ProjectionPosition,
    drive: Option<ProjectionDrive>,
}

struct PositionProjectionResult {
    ordinal: u64,
    rows: Result<Vec<ProjectedPosition>, TeslaMateFragmentError>,
    skipped_unattached: u64,
}

/// Project position pages on two bounded workers. Stage reads and fragment
/// assembly remain ordered and serialized; only the pure JSON/domain mapping
/// leaves the coordinator. At most two 10k-row pages are in flight.
struct PositionProjectionPool {
    sender: Option<mpsc::SyncSender<PositionProjectionJob>>,
    results: Option<mpsc::Receiver<PositionProjectionResult>>,
    workers: Vec<JoinHandle<()>>,
    pending: BTreeMap<u64, PositionProjectionResult>,
    next_ordinal: u64,
    in_flight: usize,
    capacity: usize,
}

impl PositionProjectionPool {
    fn new(car_id: i64, drives: Arc<HashMap<i64, ProjectionDrive>>) -> Self {
        let workers = thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1)
            .min(2);
        let capacity = workers.max(1);
        let (sender, receiver) = mpsc::sync_channel::<PositionProjectionJob>(capacity);
        let (result_sender, result_receiver) = mpsc::channel::<PositionProjectionResult>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let receiver = Arc::clone(&receiver);
            let result_sender = result_sender.clone();
            let drives = Arc::clone(&drives);
            handles.push(thread::spawn(move || {
                loop {
                    let job = match receiver.lock().expect("position receiver lock").recv() {
                        Ok(job) => job,
                        Err(_) => return,
                    };
                    debug_assert_eq!(job.car_id, car_id);
                    let mut skipped_unattached = 0_u64;
                    let rows = job
                        .rows
                        .into_iter()
                        .filter_map(|row| {
                            let drive = match row.value.drive_id {
                                None => None,
                                Some(drive_id) => match drives.get(&drive_id) {
                                    Some(drive) => Some(drive.clone()),
                                    None => {
                                        skipped_unattached += 1;
                                        return None;
                                    }
                                },
                            };
                            let position = match &drive {
                                Some(_) => match project_position(&row.value, car_id, true) {
                                    Ok(Some(projected)) => Ok(projected),
                                    Ok(None) => {
                                        Err(TeslaMateFragmentError::PositionProjectionMissing)
                                    }
                                    Err(error) => Err(error.into()),
                                },
                                None => Ok(crate::lifecycle::imported_position(&row.value)),
                            };
                            Some(position.map(|position| ProjectedPosition { position, drive }))
                        })
                        .collect::<Result<Vec<_>, _>>();
                    let result = PositionProjectionResult {
                        ordinal: job.ordinal,
                        rows,
                        skipped_unattached,
                    };
                    if result_sender.send(result).is_err() {
                        return;
                    }
                }
            }));
        }
        drop(result_sender);
        Self {
            sender: Some(sender),
            results: Some(result_receiver),
            workers: handles,
            pending: BTreeMap::new(),
            next_ordinal: 0,
            in_flight: 0,
            capacity,
        }
    }

    fn submit(
        &mut self,
        job: PositionProjectionJob,
    ) -> Result<Option<PositionProjectionResult>, TeslaMateFragmentError> {
        self.sender
            .as_ref()
            .expect("position projection pool is open")
            .send(job)
            .map_err(|_| TeslaMateFragmentError::PositionProjectionWorkerStopped)?;
        self.in_flight += 1;
        if self.in_flight >= self.capacity {
            self.receive_next().map(Some)
        } else {
            Ok(None)
        }
    }

    fn receive_next(&mut self) -> Result<PositionProjectionResult, TeslaMateFragmentError> {
        loop {
            if let Some(result) = self.pending.remove(&self.next_ordinal) {
                self.next_ordinal += 1;
                self.in_flight -= 1;
                return Ok(result);
            }
            let result = self
                .results
                .as_ref()
                .expect("position result receiver is open")
                .recv()
                .map_err(|_| TeslaMateFragmentError::PositionProjectionWorkerStopped)?;
            if result.ordinal == self.next_ordinal {
                self.next_ordinal += 1;
                self.in_flight -= 1;
                return Ok(result);
            }
            self.pending.insert(result.ordinal, result);
        }
    }

    fn finish(&mut self) -> Result<Vec<PositionProjectionResult>, TeslaMateFragmentError> {
        self.sender.take();
        let mut completed = Vec::new();
        while self.in_flight != 0 {
            completed.push(self.receive_next()?);
        }
        for worker in self.workers.drain(..) {
            worker
                .join()
                .map_err(|_| TeslaMateFragmentError::PositionProjectionWorkerPanicked)?;
        }
        Ok(completed)
    }
}

impl Drop for PositionProjectionPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        self.results.take();
        self.pending.clear();
    }
}

/// Bounded compression/build pool. Results are released in ordinal order so
/// manifest order, fingerprinting, and projection-state capture stay exact.
struct PackBuildQueue {
    sender: Option<mpsc::SyncSender<PackBuildJob>>,
    results: Option<mpsc::Receiver<PackBuildResult>>,
    workers: Vec<JoinHandle<()>>,
    pending: BTreeMap<u32, PackBuildResult>,
    next_ordinal: u32,
    in_flight: usize,
    capacity: usize,
}

impl PackBuildQueue {
    fn new(writer: &ProjectionPackWriter) -> Self {
        let workers = thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1)
            .min(2);
        let capacity = workers.max(1);
        let (sender, receiver) = mpsc::sync_channel::<PackBuildJob>(capacity);
        // At most `capacity` jobs can be submitted before the coordinator
        // consumes one result, so this channel remains bounded by the same
        // in-flight job limit without making abort/join deadlock on a full
        // result queue.
        let (result_sender, result_receiver) = mpsc::channel::<PackBuildResult>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let receiver = Arc::clone(&receiver);
            let result_sender = result_sender.clone();
            let writer = writer.clone();
            handles.push(thread::spawn(move || {
                loop {
                    let job = match receiver.lock().expect("pack job receiver lock").recv() {
                        Ok(job) => job,
                        Err(_) => return,
                    };
                    let request = ProjectionPackRequest {
                        pack_id: Uuid::new_v4(),
                        snapshot_id: job.snapshot_id,
                        ordinal: job.ordinal,
                        binding: job.binding,
                        sequence: job.sequence,
                        snapshot: &job.snapshot,
                    };
                    let built =
                        if job.states.is_empty() && job.updates.is_empty() && !job.schema_2_1 {
                            writer.write_full_snapshot(&request)
                        } else {
                            writer.write_full_snapshot_with_states_and_updates(
                                &request,
                                &job.states,
                                &job.updates,
                            )
                        };
                    let result = PackBuildResult {
                        ordinal: job.ordinal,
                        snapshot: job.snapshot,
                        updates: job.updates,
                        built,
                    };
                    if let Err(error) = result_sender.send(result) {
                        cleanup_built_pack(error.0.built);
                        return;
                    }
                }
            }));
        }
        drop(result_sender);
        Self {
            sender: Some(sender),
            results: Some(result_receiver),
            workers: handles,
            pending: BTreeMap::new(),
            next_ordinal: 0,
            in_flight: 0,
            capacity,
        }
    }

    fn submit(
        &mut self,
        job: PackBuildJob,
    ) -> Result<Option<PackBuildResult>, TeslaMateFragmentError> {
        self.sender
            .as_ref()
            .expect("pack queue is open")
            .send(job)
            .map_err(|_| TeslaMateFragmentError::PackBuildWorkerStopped)?;
        self.in_flight += 1;
        if self.in_flight >= self.capacity {
            self.receive_next().map(Some)
        } else {
            Ok(None)
        }
    }

    fn receive_next(&mut self) -> Result<PackBuildResult, TeslaMateFragmentError> {
        loop {
            if let Some(result) = self.pending.remove(&self.next_ordinal) {
                self.next_ordinal += 1;
                self.in_flight -= 1;
                return Ok(result);
            }
            let result = self
                .results
                .as_ref()
                .expect("pack result receiver is open")
                .recv()
                .map_err(|_| TeslaMateFragmentError::PackBuildWorkerStopped)?;
            if result.ordinal == self.next_ordinal {
                self.next_ordinal += 1;
                self.in_flight -= 1;
                return Ok(result);
            }
            self.pending.insert(result.ordinal, result);
        }
    }

    fn finish(&mut self) -> Result<Vec<PackBuildResult>, TeslaMateFragmentError> {
        self.sender.take();
        let mut completed = Vec::new();
        while self.in_flight != 0 {
            completed.push(self.receive_next()?);
        }
        for worker in self.workers.drain(..) {
            worker
                .join()
                .map_err(|_| TeslaMateFragmentError::PackBuildWorkerPanicked)?;
        }
        Ok(completed)
    }
}

impl Drop for PackBuildQueue {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        if let Some(results) = self.results.take() {
            for result in results.try_iter() {
                cleanup_built_pack(result.built);
            }
        }
        for result in std::mem::take(&mut self.pending).into_values() {
            cleanup_built_pack(result.built);
        }
    }
}

fn cleanup_built_pack(result: Result<BuiltProjectionPack, crate::hub_pack::ProjectionPackError>) {
    if let Ok(pack) = result
        && pack.may_remove_unpublished_file()
    {
        let _ = fs::remove_file(pack.path);
    }
}

pub(crate) struct PackSink<'a> {
    writer: &'a ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    states: Vec<ProjectionState>,
    states_fingerprinted: bool,
    states_captured: bool,
    updates_fingerprinted: bool,
    schema_2_1: bool,
    fingerprint: Option<Sha256>,
    /// The legacy upgrade bridge must reproduce the old physical fingerprint
    /// without constructing a second set of immutable packs. It still walks
    /// the exact fragment layout and records the same captured state.
    capture_only: bool,
    build_queue: Option<PackBuildQueue>,
    submitted_fragments: usize,
    written_fragments: usize,
    pub(crate) chunks: Vec<BuiltProjectionPack>,
    projection_state: Option<TeslaMateProjectionStateCapture>,
    selected_car: Option<ProjectionCar>,
}

impl<'a> PackSink<'a> {
    #[cfg(test)]
    pub(crate) fn new(
        writer: &'a ProjectionPackWriter,
        binding: ProjectionBinding,
        snapshot_id: Uuid,
        sequence: SequenceRange,
        states: Vec<ProjectionState>,
    ) -> Self {
        Self::new_with_schema_2_1(writer, binding, snapshot_id, sequence, states, false)
    }

    pub(crate) fn new_with_schema_2_1(
        writer: &'a ProjectionPackWriter,
        binding: ProjectionBinding,
        snapshot_id: Uuid,
        sequence: SequenceRange,
        states: Vec<ProjectionState>,
        schema_2_1: bool,
    ) -> Self {
        let mut fingerprint = Sha256::new();
        fingerprint.update(b"teslatlas-hub/teslamate-logical-snapshot/v1");
        Self {
            writer,
            binding,
            snapshot_id,
            sequence,
            states,
            states_fingerprinted: false,
            states_captured: false,
            updates_fingerprinted: false,
            schema_2_1,
            fingerprint: Some(fingerprint),
            capture_only: false,
            build_queue: None,
            submitted_fragments: 0,
            written_fragments: 0,
            chunks: Vec::new(),
            projection_state: None,
            selected_car: None,
        }
    }

    /// Attach one current-run state capture. The sink owns it so a failed
    /// candidate always drops both unreferenced packs and its private state
    /// file together.
    pub(crate) fn with_projection_state_capture(
        mut self,
        projection_state: TeslaMateProjectionStateCapture,
    ) -> Self {
        self.projection_state = Some(projection_state);
        self
    }

    /// Extend only current staged duplicate detection with reviewed source
    /// facts that do not fit THP1 schema 2.1. The retired physical bridge does
    /// not call this method, so its historical compatibility digest is intact.
    pub(crate) fn with_source_evidence_fingerprint(
        mut self,
        source_evidence_fingerprint: crate::protocol::Sha256Digest,
    ) -> Self {
        if let Some(fingerprint) = self.fingerprint.as_mut() {
            fingerprint.update(b"teslatlas-hub/teslamate-staged-source-evidence-binding/v1");
            fingerprint.update(source_evidence_fingerprint.as_bytes());
        }
        self
    }

    /// Direct capture keeps a separate logical history digest so fragment
    /// boundaries and repeated parent rows cannot affect duplicate detection.
    /// Staged imports retain the physical-pack fingerprint by default.
    pub(crate) fn without_physical_fingerprint(mut self) -> Self {
        self.fingerprint = None;
        self
    }

    /// Reproduce the retired direct-import physical fingerprint and capture
    /// its durable state, but never write a candidate pack. This is only safe
    /// for the one-time compatibility bridge: its caller compares the digest
    /// to the already catalogued base before making any state visible.
    pub(crate) fn capture_only(mut self) -> Self {
        self.capture_only = true;
        self
    }

    pub(crate) fn fingerprint(&self) -> Option<crate::protocol::Sha256Digest> {
        self.fingerprint.as_ref().map(|fingerprint| {
            crate::protocol::Sha256Digest::from_bytes(fingerprint.clone().finalize().into())
        })
    }

    pub(crate) fn has_written_fragments(&self) -> bool {
        self.written_fragments != 0
    }

    pub(crate) fn into_parts(
        mut self,
    ) -> (
        Vec<BuiltProjectionPack>,
        Option<TeslaMateProjectionStateCapture>,
        Option<ProjectionCar>,
    ) {
        self.finish()
            .expect("parallel pack build queue must finish");
        (
            std::mem::take(&mut self.chunks),
            self.projection_state.take(),
            self.selected_car.take(),
        )
    }

    pub(crate) fn write(
        &mut self,
        snapshot: ProjectionSnapshot,
    ) -> Result<(), TeslaMateFragmentError> {
        self.write_with_updates(snapshot, &[])
    }

    /// Emit one self-contained fragment with optional schema-2.1 firmware
    /// update rows. Update history is sidecar data like state history, but it
    /// may be streamed in later bounded batches rather than retained for the
    /// entire import.
    pub(crate) fn write_with_updates(
        &mut self,
        snapshot: ProjectionSnapshot,
        updates: &[ProjectionUpdate],
    ) -> Result<(), TeslaMateFragmentError> {
        // The one-time legacy bridge must reproduce the already-catalogued
        // historical shape exactly. It deliberately never captures update
        // rows; accepting one here would create a state/inventory shape that
        // cannot correspond to that old immutable base.
        if self.capture_only && !updates.is_empty() {
            return Err(TeslaMateFragmentError::LegacyBridgeUpdateHistory);
        }
        if self.submitted_fragments >= ProtocolLimits::default().max_chunks {
            return Err(TeslaMateFragmentError::TooManyFragments);
        }
        let ordinal = u32::try_from(self.submitted_fragments)
            .map_err(|_| TeslaMateFragmentError::TooManyFragments)?;
        if !self.states_fingerprinted {
            if let Some(fingerprint) = self.fingerprint.as_mut() {
                let canonical_states = serde_json::to_vec(&self.states)
                    .map_err(TeslaMateFragmentError::SerializeProjectedValue)?;
                fingerprint.update(b"teslatlas-hub/teslamate-logical-states/v1");
                fingerprint.update(
                    u64::try_from(canonical_states.len())
                        .map_err(|_| TeslaMateFragmentError::FragmentSizeOverflow)?
                        .to_be_bytes(),
                );
                fingerprint.update(&canonical_states);
            }
            self.states_fingerprinted = true;
        }
        if !updates.is_empty()
            && let Some(fingerprint) = self.fingerprint.as_mut()
        {
            if !self.updates_fingerprinted {
                fingerprint.update(b"teslatlas-hub/teslamate-logical-updates/v1");
                self.updates_fingerprinted = true;
            }
            for update in updates {
                let canonical = serde_json::to_vec(update)
                    .map_err(TeslaMateFragmentError::SerializeProjectedValue)?;
                fingerprint.update(
                    u64::try_from(canonical.len())
                        .map_err(|_| TeslaMateFragmentError::FragmentSizeOverflow)?
                        .to_be_bytes(),
                );
                fingerprint.update(&canonical);
            }
        }
        if let Some(fingerprint) = self.fingerprint.as_mut() {
            let canonical = serde_json::to_vec(&snapshot)
                .map_err(TeslaMateFragmentError::SerializeProjectedValue)?;
            fingerprint.update(
                u64::try_from(canonical.len())
                    .map_err(|_| TeslaMateFragmentError::FragmentSizeOverflow)?
                    .to_be_bytes(),
            );
            fingerprint.update(&canonical);
        }
        if self.capture_only {
            self.submitted_fragments += 1;
            self.accept_snapshot(snapshot, updates, None)?;
            return Ok(());
        }
        self.submitted_fragments += 1;
        let states = if self.submitted_fragments == 1 {
            self.states.clone()
        } else {
            Vec::new()
        };
        let job = PackBuildJob {
            ordinal,
            snapshot_id: self.snapshot_id,
            binding: self.binding.clone(),
            sequence: self.sequence,
            snapshot,
            states,
            updates: updates.to_vec(),
            schema_2_1: self.schema_2_1,
        };
        let completed = self
            .build_queue
            .get_or_insert_with(|| PackBuildQueue::new(self.writer))
            .submit(job)?;
        if let Some(completed) = completed {
            self.accept_completed(completed)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), TeslaMateFragmentError> {
        let completed = match self.build_queue.as_mut() {
            Some(queue) => queue.finish()?,
            None => Vec::new(),
        };
        let mut completed = completed.into_iter();
        while let Some(result) = completed.next() {
            if let Err(error) = self.accept_completed(result) {
                for remaining in completed {
                    cleanup_built_pack(remaining.built);
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn accept_completed(
        &mut self,
        completed: PackBuildResult,
    ) -> Result<(), TeslaMateFragmentError> {
        #[cfg(test)]
        let mut pack_build_phase =
            crate::teslamate_direct::native_ten_million_phase_trace::mark(
                crate::teslamate_direct::native_ten_million_phase_trace::NativeTenMillionPhase::PackBuild,
            );
        let built = completed.built?;
        #[cfg(test)]
        pack_build_phase.complete();
        self.accept_snapshot(completed.snapshot, &completed.updates, Some(built))
    }

    fn accept_snapshot(
        &mut self,
        snapshot: ProjectionSnapshot,
        updates: &[ProjectionUpdate],
        built: Option<BuiltProjectionPack>,
    ) -> Result<(), TeslaMateFragmentError> {
        if let Some(built) = built {
            self.chunks.push(built);
        }
        let selected_car = snapshot
            .cars
            .first()
            .expect("full-snapshot writer accepted one selected car")
            .clone();
        if let Some(existing) = &self.selected_car
            && existing != &selected_car
        {
            return Err(TeslaMateFragmentError::ProjectionStateCarConflict {
                car_id: selected_car.id,
            });
        }
        self.selected_car = Some(selected_car);
        #[cfg(test)]
        let mut projection_state_capture_phase =
            crate::teslamate_direct::native_ten_million_phase_trace::mark(
                crate::teslamate_direct::native_ten_million_phase_trace::NativeTenMillionPhase::ProjectionStateCapture,
            );
        let captured = self.capture_written_snapshot(&snapshot, updates);
        #[cfg(test)]
        projection_state_capture_phase.complete();
        captured?;
        self.written_fragments = self
            .written_fragments
            .checked_add(1)
            .ok_or(TeslaMateFragmentError::TooManyFragments)?;
        Ok(())
    }

    fn capture_written_snapshot(
        &mut self,
        snapshot: &ProjectionSnapshot,
        updates: &[ProjectionUpdate],
    ) -> Result<(), TeslaMateFragmentError> {
        let Some(capture) = self.projection_state.as_mut() else {
            return Ok(());
        };

        // `states` live outside ProjectionSnapshot and are emitted only with
        // the first pack. Capture them only after that pack exists so a
        // state-capture failure cannot leave a state file that describes an
        // unpublished snapshot.
        if !self.states_captured {
            for state in &self.states {
                capture.record_state(state)?;
            }
            self.states_captured = true;
        }
        for car in &snapshot.cars {
            capture.record_car(car)?;
        }
        for drive in &snapshot.drives {
            capture.record_drive(drive)?;
        }
        for position in &snapshot.positions {
            capture.record_position(position)?;
        }
        let mut charge_car_ids = HashMap::with_capacity(snapshot.charges.len());
        for charge in &snapshot.charges {
            if let Some(existing) = charge_car_ids.insert(charge.id, charge.car_id)
                && existing != charge.car_id
            {
                return Err(TeslaMateFragmentError::ProjectionStateChargeConflict {
                    charge_id: charge.id,
                });
            }
            capture.record_charge(charge)?;
        }
        for sample in &snapshot.charge_samples {
            let car_id = charge_car_ids
                .get(&sample.charge_process_id)
                .copied()
                .ok_or(TeslaMateFragmentError::ProjectionStateSampleParentMissing {
                    sample_id: sample.id,
                    charge_id: sample.charge_process_id,
                })?;
            capture.record_charge_sample(car_id, sample)?;
        }
        for update in updates {
            capture.record_update(update)?;
        }
        Ok(())
    }
}

impl Drop for PackSink<'_> {
    fn drop(&mut self) {
        self.build_queue.take();
        for chunk in self.chunks.drain(..) {
            if !chunk.may_remove_unpublished_file() {
                continue;
            }
            if let Err(error) = fs::remove_file(&chunk.path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %chunk.path.display(), %error, "could not remove unpublished TeslaMate pack");
            }
        }
    }
}

/// A bounded sidecar-fragment builder for firmware-update history. Every
/// emitted update fragment repeats only the selected car, so it remains a
/// valid self-contained schema-2.1 full snapshot without retaining the full
/// update table in memory.
pub(crate) struct UpdateFragmentAccumulator {
    car: ProjectionCar,
    limits: TeslaMateFragmentLimits,
    payload_bytes: u64,
    updates: Vec<ProjectionUpdate>,
}

impl UpdateFragmentAccumulator {
    pub(crate) fn new(
        car: ProjectionCar,
        limits: TeslaMateFragmentLimits,
    ) -> Result<Self, TeslaMateFragmentError> {
        Ok(Self {
            payload_bytes: serialized_bytes(&car)?,
            car,
            limits,
            updates: Vec::new(),
        })
    }

    pub(crate) fn push(
        &mut self,
        sink: &mut PackSink<'_>,
        update: ProjectionUpdate,
    ) -> Result<(), TeslaMateFragmentError> {
        let update_bytes = serialized_bytes(&update)?;
        if self.exceeds(1, update_bytes)? && !self.updates.is_empty() {
            self.flush(sink)?;
        }
        if self.exceeds(1, update_bytes)? {
            return Err(TeslaMateFragmentError::SingleFragmentValueExceedsTarget);
        }
        self.payload_bytes = self
            .payload_bytes
            .checked_add(update_bytes)
            .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?;
        self.updates.push(update);
        Ok(())
    }

    pub(crate) fn flush(&mut self, sink: &mut PackSink<'_>) -> Result<(), TeslaMateFragmentError> {
        if self.updates.is_empty() {
            return Ok(());
        }
        sink.write_with_updates(
            ProjectionSnapshot {
                cars: vec![self.car.clone()],
                drives: Vec::new(),
                positions: Vec::new(),
                charges: Vec::new(),
                charge_samples: Vec::new(),
            },
            &self.updates,
        )?;
        self.updates.clear();
        self.payload_bytes = serialized_bytes(&self.car)?;
        Ok(())
    }

    fn exceeds(
        &self,
        additional_rows: u64,
        additional_bytes: u64,
    ) -> Result<bool, TeslaMateFragmentError> {
        let rows = 1_u64
            .checked_add(u64::try_from(self.updates.len()).expect("usize fits u64"))
            .and_then(|rows| rows.checked_add(additional_rows))
            .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?;
        let bytes = self
            .payload_bytes
            .checked_add(additional_bytes)
            .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?;
        Ok(
            rows > self.limits.max_rows_per_fragment
                || bytes > self.limits.max_projected_json_bytes,
        )
    }
}

pub(crate) struct FragmentAccumulator {
    car: ProjectionCar,
    limits: TeslaMateFragmentLimits,
    payload_bytes: u64,
    pub(crate) drives: Vec<ProjectionDrive>,
    pub(crate) positions: Vec<ProjectionPosition>,
    pub(crate) charges: Vec<ProjectionCharge>,
    pub(crate) charge_samples: Vec<ProjectionChargeSample>,
    pub(crate) drive_ids: HashSet<i64>,
    pub(crate) charge_ids: HashSet<i64>,
}

impl FragmentAccumulator {
    pub(crate) fn new(
        car: ProjectionCar,
        limits: TeslaMateFragmentLimits,
    ) -> Result<Self, TeslaMateFragmentError> {
        Ok(Self {
            payload_bytes: serialized_bytes(&car)?,
            car,
            limits,
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
            drive_ids: HashSet::new(),
            charge_ids: HashSet::new(),
        })
    }

    pub(crate) fn prepare<F>(
        &mut self,
        sink: &mut PackSink<'_>,
        addition: F,
    ) -> Result<(), TeslaMateFragmentError>
    where
        F: Fn(&Self) -> Result<(u64, u64), TeslaMateFragmentError>,
    {
        let (additional_rows, additional_bytes) = addition(self)?;
        if self.exceeds(additional_rows, additional_bytes)? && self.has_data() {
            self.flush(sink)?;
        }
        // Parent rows are repeated after a flush, so calculate the unit again
        // against the new fragment rather than reusing a stale dedup result.
        let (additional_rows, additional_bytes) = addition(self)?;
        if self.exceeds(additional_rows, additional_bytes)? {
            return Err(TeslaMateFragmentError::SingleFragmentValueExceedsTarget);
        }
        self.payload_bytes = self
            .payload_bytes
            .checked_add(additional_bytes)
            .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?;
        Ok(())
    }

    fn exceeds(
        &self,
        additional_rows: u64,
        additional_bytes: u64,
    ) -> Result<bool, TeslaMateFragmentError> {
        let rows = self
            .row_count()
            .checked_add(additional_rows)
            .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?;
        let bytes = self
            .payload_bytes
            .checked_add(additional_bytes)
            .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?;
        Ok(
            rows > self.limits.max_rows_per_fragment
                || bytes > self.limits.max_projected_json_bytes,
        )
    }

    pub(crate) fn has_data(&self) -> bool {
        !(self.drives.is_empty()
            && self.positions.is_empty()
            && self.charges.is_empty()
            && self.charge_samples.is_empty())
    }

    fn row_count(&self) -> u64 {
        1 + u64::try_from(self.drives.len()).expect("usize fits u64")
            + u64::try_from(self.positions.len()).expect("usize fits u64")
            + u64::try_from(self.charges.len()).expect("usize fits u64")
            + u64::try_from(self.charge_samples.len()).expect("usize fits u64")
    }

    pub(crate) fn flush(&mut self, sink: &mut PackSink<'_>) -> Result<(), TeslaMateFragmentError> {
        if self.has_data() {
            sink.write(self.finish())?;
            self.reset()?;
        }
        Ok(())
    }

    fn finish(&self) -> ProjectionSnapshot {
        ProjectionSnapshot {
            cars: vec![self.car.clone()],
            drives: self.drives.clone(),
            positions: self.positions.clone(),
            charges: self.charges.clone(),
            charge_samples: self.charge_samples.clone(),
        }
    }

    fn reset(&mut self) -> Result<(), TeslaMateFragmentError> {
        self.payload_bytes = serialized_bytes(&self.car)?;
        self.drives.clear();
        self.positions.clear();
        self.charges.clear();
        self.charge_samples.clear();
        self.drive_ids.clear();
        self.charge_ids.clear();
        Ok(())
    }
}

pub(crate) fn serialized_bytes<T: Serialize>(value: &T) -> Result<u64, TeslaMateFragmentError> {
    serde_json::to_vec(value)
        .map(|encoded| u64::try_from(encoded.len()).expect("usize fits u64"))
        .map_err(TeslaMateFragmentError::SerializeProjectedValue)
}

#[derive(Debug, Error)]
pub enum TeslaMateFragmentError {
    #[error("TeslaMate staged pack production needs a non-nil snapshot ID")]
    NilSnapshotId,
    #[error("TeslaMate selected car id must be positive")]
    InvalidSelectedCarId,
    #[error("TeslaMate full snapshot sequence is unordered")]
    UnorderedSequence,
    #[error("fragment row target must allow a car, parent, and child")]
    FragmentRowTargetTooSmall,
    #[error("fragment row target exceeds the protocol ceiling")]
    FragmentRowTargetTooLarge,
    #[error("fragment projected-byte target is invalid")]
    FragmentByteTargetInvalid,
    #[error("staged TeslaMate relation {table}/{source_id} is missing")]
    MissingStageRelation { table: &'static str, source_id: i64 },
    #[error(
        "staged TeslaMate update {update_id} belongs to car {found_car_id}, not {expected_car_id}"
    )]
    UpdateWrongCar {
        update_id: i64,
        expected_car_id: i64,
        found_car_id: i64,
    },
    #[error(
        "staged charge sample {sample_id} belongs to process {found_process_id}, not {expected_process_id}"
    )]
    SampleProcessMismatch {
        sample_id: i64,
        expected_process_id: i64,
        found_process_id: i64,
    },
    #[error("fragment row or byte accounting overflowed")]
    FragmentSizeOverflow,
    #[error("one projected parent/child unit exceeds the fragment target")]
    SingleFragmentValueExceedsTarget,
    #[error("a staged full snapshot would exceed the protocol chunk ceiling")]
    TooManyFragments,
    #[error("parallel TeslaMate pack build worker stopped")]
    PackBuildWorkerStopped,
    #[error("parallel TeslaMate pack build worker panicked")]
    PackBuildWorkerPanicked,
    #[error("parallel TeslaMate position projection worker stopped")]
    PositionProjectionWorkerStopped,
    #[error("parallel TeslaMate position projection worker panicked")]
    PositionProjectionWorkerPanicked,
    #[error("a position with a completed drive did not project")]
    PositionProjectionMissing,
    #[error("projection report accounting overflowed")]
    ReportOverflow,
    #[error("sealed staged update projection changed between validation and emission")]
    UpdateProjectionReconciliation,
    #[error("the legacy direct bridge cannot capture schema-2.1 update history")]
    LegacyBridgeUpdateHistory,
    #[error("projection-state charge {charge_id} occurs with conflicting cars in one pack")]
    ProjectionStateChargeConflict { charge_id: i64 },
    #[error("projection-state car {car_id} changed across full-snapshot fragments")]
    ProjectionStateCarConflict { car_id: i64 },
    #[error(
        "projection-state sample {sample_id} references missing charge {charge_id} in its pack"
    )]
    ProjectionStateSampleParentMissing { sample_id: i64, charge_id: i64 },
    #[error("cannot size a projected fragment value: {0}")]
    SerializeProjectedValue(serde_json::Error),
    #[error(transparent)]
    SourceEvidence(#[from] TeslaMateSourceEvidenceError),
    #[error(transparent)]
    Stage(#[from] TeslaMateStageError),
    #[error(transparent)]
    Projection(#[from] TeslaMateProjectionError),
    #[error(transparent)]
    ProjectionState(#[from] TeslaMateProjectionStateError),
    #[error(transparent)]
    Pack(#[from] crate::hub_pack::ProjectionPackError),
}

#[cfg(test)]
mod tests {
    use std::{fs::File, path::Path};

    use rusqlite::Connection;

    use super::*;

    #[test]
    fn dense_stage_starts_at_the_first_bounded_retry_target() {
        let default = TeslaMateFragmentLimits::default();
        assert_eq!(
            initial_staged_fragment_limits(DENSE_STAGE_ROW_THRESHOLD - 1, default),
            default
        );
        assert_eq!(
            initial_staged_fragment_limits(DENSE_STAGE_ROW_THRESHOLD, default),
            next_fragment_limits(default).expect("default has a dense target")
        );
    }

    #[test]
    fn position_projection_pool_returns_pages_in_source_order() {
        let mut pool = PositionProjectionPool::new(1, Arc::new(HashMap::new()));
        let mut pages = Vec::new();
        for ordinal in 0..4 {
            if let Some(page) = pool
                .submit(PositionProjectionJob {
                    ordinal,
                    rows: Vec::new(),
                    car_id: 1,
                })
                .expect("submit empty position page")
            {
                pages.push(page);
            }
        }
        pages.extend(pool.finish().expect("finish position projection pool"));
        assert_eq!(
            pages.iter().map(|page| page.ordinal).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }
    use crate::{
        hub_pack::{
            ProjectionBinding, ProjectionCar, ProjectionPackError, ProjectionPackOwnership,
            ProjectionPackRequest, ProjectionSnapshot, ProjectionUpdate,
            signed_full_snapshot_manifest,
        },
        protocol::CursorKey,
        teslamate_projection_state::{
            TeslaMateProjectionState, TeslaMateProjectionStateCapture,
            TeslaMateProjectionStateEntity, TeslaMateProjectionStateLimits,
        },
        teslamate_stage::TeslaMateStageLimits,
    };

    fn stage() -> (tempfile::TempDir, TeslaMateStage) {
        stage_with_sealed(true)
    }

    fn mutable_stage() -> (tempfile::TempDir, TeslaMateStage) {
        stage_with_sealed(false)
    }

    fn stage_with_sealed(seal: bool) -> (tempfile::TempDir, TeslaMateStage) {
        let temporary = tempfile::tempdir().unwrap();
        let mut stage = TeslaMateStage::create(
            temporary.path().join("imports"),
            TeslaMateStageLimits {
                max_rows: 2_000,
                max_stage_bytes: 4 * 1024 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .unwrap();
        let car = TeslaMateCar {
            id: 1,
            eid: 99,
            vid: Some(199),
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
        };
        let position = TeslaMatePosition {
            id: 20,
            car_id: 1,
            drive_id: Some(10),
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
        let drive = TeslaMateDrive {
            id: 10,
            car_id: 1,
            start_date_ms: 1_700_000_000_000,
            end_date_ms: Some(1_700_000_060_000),
            start_position_id: Some(20),
            end_position_id: Some(20),
            start_address_id: Some(100),
            end_address_id: Some(100),
            start_geofence_id: Some(200),
            end_geofence_id: Some(200),
            outside_temp_avg: Some(18.0),
            inside_temp_avg: Some(21.0),
            speed_max: Some(80),
            power_max: Some(36.0),
            power_min: Some(-7.0),
            start_ideal_range_km: Some(338.8),
            end_ideal_range_km: Some(334.8),
            start_rated_range_km: Some(400.0),
            end_rated_range_km: Some(390.0),
            start_km: None,
            end_km: None,
            distance_km: Some(12.0),
            duration_min: Some(10),
            ascent: Some(60),
            descent: Some(30),
        };
        let process = TeslaMateChargingProcess {
            id: 30,
            car_id: 1,
            position_id: Some(20),
            address_id: Some(100),
            geofence_id: Some(200),
            start_date_ms: 1_700_001_000_000,
            end_date_ms: Some(1_700_001_360_000),
            charge_energy_added: None,
            charge_energy_used_kwh: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            cost: None,
            start_battery_level: Some(50),
            end_battery_level: None,
            duration_min: Some(60),
            outside_temp_avg: Some(18.0),
            start_rated_range_km: None,
            end_rated_range_km: None,
        };
        let sample = TeslaMateCharge {
            id: 40,
            charging_process_id: 30,
            date_ms: 1_700_001_100_000,
            battery_heater: Some(false),
            battery_heater_on: Some(false),
            battery_heater_no_power: Some(false),
            battery_level: Some(80),
            usable_battery_level: Some(79),
            charge_energy_added_kwh: Some(20.0),
            charger_actual_current: Some(30.0),
            charger_phases: Some(1),
            charger_pilot_current: Some(32.0),
            charger_power_kw: Some(7.0),
            charger_voltage: Some(230.0),
            charge_cable: Some("Type 2".into()),
            fast_charger_present: Some(false),
            fast_charger_brand: None,
            fast_charger_type: None,
            ideal_range_km: Some(300.0),
            rated_range_km: Some(298.0),
            not_enough_power_to_heat: Some(false),
            outside_temp_c: Some(18.0),
        };
        stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .unwrap();
        stage
            .insert(TeslaMateStageTable::Positions, position.id, &position)
            .unwrap();
        stage
            .insert(TeslaMateStageTable::Drives, drive.id, &drive)
            .unwrap();
        stage
            .insert(TeslaMateStageTable::ChargingProcesses, process.id, &process)
            .unwrap();
        stage
            .insert(TeslaMateStageTable::Charges, sample.id, &sample)
            .unwrap();
        stage
            .insert(
                TeslaMateStageTable::Addresses,
                100,
                &TeslaMateAddress {
                    id: 100,
                    display_name: Some("Home, London".into()),
                    name: Some("Home".into()),
                },
            )
            .unwrap();
        stage
            .insert(
                TeslaMateStageTable::Geofences,
                200,
                &TeslaMateGeofence {
                    id: 200,
                    name: "Home".into(),
                    latitude: Some(51.0),
                    longitude: Some(-0.1),
                    radius_m: Some(100.0),
                    billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
                    cost_per_unit: Some(0.30),
                    session_fee: Some(2.0),
                },
            )
            .unwrap();
        stage
            .insert(
                TeslaMateStageTable::Updates,
                300,
                &TeslaMateUpdate {
                    id: 300,
                    car_id: 1,
                    start_date_ms: 1_699_000_000_000,
                    end_date_ms: Some(1_699_000_060_000),
                    version: Some("2026.20.1".into()),
                },
            )
            .unwrap();
        if seal {
            stage.seal().unwrap();
        }
        (temporary, stage)
    }

    fn binding() -> ProjectionBinding {
        ProjectionBinding {
            installation_id: Uuid::from_u128(1),
            account_id: Uuid::from_u128(2),
            vehicle_id: Uuid::from_u128(3),
            generation: 1,
            selected_car_id: 1,
        }
    }

    fn update_rows_from_chunks(
        inspection_directory: &Path,
        chunks: &[BuiltProjectionPack],
    ) -> Vec<(i64, i64, i64, i64, String)> {
        let mut updates = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let sqlite = zstd::stream::decode_all(File::open(&chunk.path).expect("open pack"))
                .expect("decode pack");
            let inspection_path = inspection_directory.join(format!("updates-{index}.sqlite"));
            std::fs::write(&inspection_path, sqlite).expect("write inspection database");
            let connection = Connection::open(inspection_path).expect("open inspection database");
            let mut statement = connection
                .prepare(
                    "SELECT id, car_id, start_date_ms, end_date_ms, version
                     FROM updates ORDER BY id",
                )
                .expect("schema-2.1 updates table exists");
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .expect("query emitted updates")
                .collect::<Result<Vec<_>, _>>()
                .expect("read emitted updates");
            updates.extend(rows);
        }
        updates
    }

    fn capturable_snapshot(stage: &TeslaMateStage) -> (ProjectionSnapshot, Vec<ProjectionState>) {
        let car = stage
            .get::<TeslaMateCar>(TeslaMateStageTable::Cars, 1)
            .expect("car lookup")
            .expect("car");
        let position = stage
            .get::<TeslaMatePosition>(TeslaMateStageTable::Positions, 20)
            .expect("position lookup")
            .expect("position");
        let drive = stage
            .get::<TeslaMateDrive>(TeslaMateStageTable::Drives, 10)
            .expect("drive lookup")
            .expect("drive");
        let process = stage
            .get::<TeslaMateChargingProcess>(TeslaMateStageTable::ChargingProcesses, 30)
            .expect("process lookup")
            .expect("process");
        let sample = stage
            .get::<TeslaMateCharge>(TeslaMateStageTable::Charges, 40)
            .expect("sample lookup")
            .expect("sample");
        let address = stage
            .get::<TeslaMateAddress>(TeslaMateStageTable::Addresses, 100)
            .expect("address lookup")
            .expect("address");
        let geofence = stage
            .get::<TeslaMateGeofence>(TeslaMateStageTable::Geofences, 200)
            .expect("geofence lookup")
            .expect("geofence");
        let projected_car = project_car(&car, None).expect("project car");
        let projected_drive = project_drive(
            &drive,
            1,
            DriveRelations {
                start_position: Some(&position),
                end_position: Some(&position),
                start_address: Some(&address),
                end_address: Some(&address),
                start_geofence: Some(&geofence),
                end_geofence: Some(&geofence),
            },
        )
        .expect("project drive")
        .expect("completed drive");
        let projected_position = project_position(&position, 1, true)
            .expect("project position")
            .expect("included position");
        let projected_charge = project_charge(
            &process,
            1,
            Some(&position),
            Some(&address),
            Some(&geofence),
            &ChargeProjectionFacts::default(),
        )
        .expect("project charge");
        let projected_sample = project_charge_sample(&sample);
        (
            ProjectionSnapshot {
                cars: vec![projected_car],
                drives: vec![projected_drive],
                positions: vec![projected_position],
                charges: vec![projected_charge],
                charge_samples: vec![projected_sample],
            },
            vec![ProjectionState {
                id: 50,
                car_id: 1,
                state: "online".into(),
                start_date_ms: 1_700_002_000_000,
                end_date_ms: None,
            }],
        )
    }

    #[test]
    fn dropped_candidate_sink_removes_unpublished_pack() {
        let temporary = tempfile::tempdir().unwrap();
        let writer = ProjectionPackWriter::new(temporary.path());
        let mut sink = PackSink::new(
            &writer,
            binding(),
            Uuid::from_u128(4),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            Vec::new(),
        );
        sink.write(ProjectionSnapshot {
            cars: vec![ProjectionCar {
                id: 1,
                name: "Corpus One".into(),
                model: "Model S".into(),
                vin: None,
                source_eid: None,
                source_vid: None,
                trim_badging: None,
                marketing_name: None,
                exterior_color: None,
                wheel_type: None,
                spoiler_type: None,
                firmware_version: None,
                efficiency_wh_per_km: None,
                settings: Default::default(),
            }],
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
        })
        .unwrap();
        sink.finish().unwrap();
        assert_eq!(
            sink.chunks[0].ownership(),
            ProjectionPackOwnership::Created,
            "a fresh candidate sink must own its unpublished pack cleanup"
        );
        let candidate = sink.chunks[0].path.clone();
        assert!(candidate.is_file());
        drop(sink);
        assert!(!candidate.exists());
    }

    #[test]
    fn reused_catalogued_pack_survives_sink_and_staged_candidate_cleanup() {
        let temporary = tempfile::tempdir().expect("temporary Hub store");
        let store = crate::db::HubStore::initialize(temporary.path()).expect("Hub store");
        let writer = ProjectionPackWriter::new(store.packs_dir());
        let (_stage_directory, stage) = stage();
        let (snapshot, _states) = capturable_snapshot(&stage);
        let request = ProjectionPackRequest {
            pack_id: Uuid::from_u128(0x77),
            snapshot_id: Uuid::from_u128(0x88),
            ordinal: 0,
            binding: binding(),
            sequence: SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            snapshot: &snapshot,
        };

        let created = writer
            .write_full_snapshot(&request)
            .expect("first producer writes the deterministic pack");
        assert_eq!(created.ownership(), ProjectionPackOwnership::Created);
        let manifest = request
            .signed_manifest(&created, &CursorKey::from_bytes([45; 32]))
            .expect("sign deterministic first producer pack");
        store
            .publish_manifest(&manifest)
            .expect("catalogue first producer pack");
        let catalogued_path = created.path.clone();
        let digest = created.metadata.sha256;

        let clone = created.clone();
        assert_eq!(
            clone.ownership(),
            ProjectionPackOwnership::ReusedExisting,
            "cloning a created descriptor must not duplicate its deletion right"
        );

        let reused_for_sink = writer
            .write_full_snapshot(&request)
            .expect("second producer reuses existing immutable pack");
        assert_eq!(
            reused_for_sink.ownership(),
            ProjectionPackOwnership::ReusedExisting
        );
        let mut sink = PackSink::new(
            &writer,
            binding(),
            Uuid::from_u128(0x99),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            Vec::new(),
        );
        sink.chunks.push(reused_for_sink);
        drop(sink);
        assert!(
            catalogued_path.is_file(),
            "dropping a candidate that reused a catalogued file must not unlink it"
        );
        assert!(
            store
                .pack_sha256_is_catalogued(&digest.to_string())
                .expect("catalogue lookup after sink cleanup"),
            "the reused pack remains catalogued after PackSink cleanup"
        );

        let reused_for_staged = writer
            .write_full_snapshot(&request)
            .expect("another producer reuses existing immutable pack");
        assert_eq!(
            reused_for_staged.ownership(),
            ProjectionPackOwnership::ReusedExisting
        );
        let staged = StagedProjectionPacks::new(
            vec![reused_for_staged],
            ProjectionReport::default(),
            crate::protocol::Sha256Digest::of_bytes(b"reused-pack-cleanup"),
            Vec::new(),
        );
        drop(staged);
        assert!(
            catalogued_path.is_file(),
            "dropping staged reused chunks must not unlink a catalogued file"
        );
        store
            .catalogue_check()
            .expect("catalogue remains valid after reused-pack cleanup");
    }

    #[test]
    fn legacy_bridge_capture_replays_the_physical_digest_without_writing_a_pack() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (_stage_directory, stage) = stage();
        let (snapshot, states) = capturable_snapshot(&stage);
        let writer = ProjectionPackWriter::new(temporary.path());
        let mut historical = PackSink::new_with_schema_2_1(
            &writer,
            binding(),
            Uuid::from_u128(41),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            states.clone(),
            true,
        );
        historical
            .write(snapshot.clone())
            .expect("historical physical fragment");
        historical.finish().expect("historical pack build");
        let historical_fingerprint = historical
            .fingerprint()
            .expect("historical physical fingerprint");
        let historical_pack = historical.chunks[0].path.clone();
        drop(historical);
        assert!(
            !historical_pack.exists(),
            "the comparison fixture must not leave a candidate pack behind"
        );

        let state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 16,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("bridge state capture");
        let mut bridge = PackSink::new_with_schema_2_1(
            &writer,
            binding(),
            Uuid::from_u128(42),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            states,
            true,
        )
        .capture_only()
        .with_projection_state_capture(TeslaMateProjectionStateCapture::for_initial_base(state));
        bridge.write(snapshot).expect("packless bridge fragment");
        assert!(bridge.chunks.is_empty(), "bridge must not write a new pack");
        assert!(bridge.has_written_fragments());
        assert_eq!(
            bridge.fingerprint(),
            Some(historical_fingerprint),
            "bridge must replay the retired physical digest exactly"
        );
        let (_chunks, capture, _selected_car) = bridge.into_parts();
        let mut capture = capture.expect("bridge capture retained");
        capture.seal().expect("seal bridge state");
        assert_eq!(
            capture
                .page(None, 16)
                .expect("bridge state page")
                .rows
                .len(),
            6,
            "packless capture must still retain every projected fact"
        );
    }

    #[test]
    fn staged_fingerprint_binds_preprojection_source_evidence() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let writer = ProjectionPackWriter::new(temporary.path());
        let make_sink = |evidence: crate::protocol::Sha256Digest| {
            PackSink::new(
                &writer,
                binding(),
                Uuid::from_u128(0x5151),
                SequenceRange {
                    from_exclusive: 0,
                    to_inclusive: 1,
                },
                Vec::new(),
            )
            .with_source_evidence_fingerprint(evidence)
            .fingerprint()
            .expect("staged fingerprint")
        };

        let first = make_sink(crate::protocol::Sha256Digest::of_bytes(b"first evidence"));
        let repeated = make_sink(crate::protocol::Sha256Digest::of_bytes(b"first evidence"));
        let changed = make_sink(crate::protocol::Sha256Digest::of_bytes(b"changed evidence"));
        assert_eq!(first, repeated);
        assert_ne!(
            repeated, changed,
            "a source-only fact change must invalidate staged duplicate suppression"
        );
    }

    #[test]
    fn legacy_bridge_refuses_update_rows_to_preserve_the_historical_layout() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (_stage_directory, stage) = stage();
        let (snapshot, states) = capturable_snapshot(&stage);
        let writer = ProjectionPackWriter::new(temporary.path());
        let mut bridge = PackSink::new_with_schema_2_1(
            &writer,
            binding(),
            Uuid::from_u128(43),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            states,
            true,
        )
        .capture_only();

        let error = bridge
            .write_with_updates(
                snapshot,
                &[ProjectionUpdate {
                    id: 300,
                    car_id: 1,
                    start_date_ms: 1_699_000_000_000,
                    end_date_ms: 1_699_000_060_000,
                    version: "2026.20.1".into(),
                }],
            )
            .expect_err("legacy bridge cannot add a new update table fact");
        assert!(matches!(
            error,
            TeslaMateFragmentError::LegacyBridgeUpdateHistory
        ));
        assert!(bridge.chunks.is_empty(), "bridge must not write a new pack");
        assert!(
            !bridge.has_written_fragments(),
            "a rejected update must not alter historical bridge accounting"
        );
    }

    #[test]
    fn captures_each_projected_fact_once_after_verified_fragment_writes() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (_stage_directory, stage) = stage();
        let (snapshot, states) = capturable_snapshot(&stage);
        let state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 16,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state capture");
        let writer = ProjectionPackWriter::new(temporary.path());
        let mut sink = PackSink::new_with_schema_2_1(
            &writer,
            binding(),
            Uuid::from_u128(40),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            states,
            true,
        )
        .with_projection_state_capture(TeslaMateProjectionStateCapture::for_initial_base(state));
        sink.write(snapshot.clone()).expect("first fragment");
        sink.write(snapshot).expect("repeat parent fragment");
        let (_chunks, capture, _selected_car) = sink.into_parts();
        let mut capture = capture.expect("capture retained with candidate");
        capture.seal().expect("seal capture");
        let page = capture.page(None, 16).expect("captured state page");
        assert_eq!(page.rows.len(), 6, "repeated fragment parents deduplicate");
        assert_eq!(
            page.rows
                .iter()
                .find(|row| row.entity == TeslaMateProjectionStateEntity::ChargeSample)
                .expect("captured charge sample")
                .car_id,
            1,
            "charge samples take their source-car identity from their charge parent"
        );
        assert_eq!(
            page.rows.iter().map(|row| row.entity).collect::<Vec<_>>(),
            vec![
                TeslaMateProjectionStateEntity::Car,
                TeslaMateProjectionStateEntity::Drive,
                TeslaMateProjectionStateEntity::Position,
                TeslaMateProjectionStateEntity::Charge,
                TeslaMateProjectionStateEntity::ChargeSample,
                TeslaMateProjectionStateEntity::State,
            ]
        );
    }

    #[test]
    fn state_capture_failure_removes_already_written_candidate_pack() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (_stage_directory, stage) = stage();
        let (snapshot, states) = capturable_snapshot(&stage);
        let state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 1,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state capture");
        let writer = ProjectionPackWriter::new(temporary.path());
        let mut sink = PackSink::new_with_schema_2_1(
            &writer,
            binding(),
            Uuid::from_u128(41),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            states,
            true,
        )
        .with_projection_state_capture(TeslaMateProjectionStateCapture::for_initial_base(state));
        sink.write(snapshot)
            .expect("pack build is deferred until queue finish");
        let error = sink
            .finish()
            .expect_err("state row ceiling rejects candidate after write");
        assert!(matches!(
            error,
            TeslaMateFragmentError::ProjectionState(
                TeslaMateProjectionStateError::RowLimitExceeded { maximum: 1 }
            )
        ));
        let candidate = sink.chunks[0].path.clone();
        assert!(
            candidate.is_file(),
            "pack existed before capture failure cleanup"
        );
        drop(sink);
        assert!(!candidate.exists(), "failed candidate pack is cleaned up");
    }

    #[test]
    fn dropped_completed_candidate_removes_unpublished_packs() {
        let temporary = tempfile::tempdir().unwrap();
        let (_stage_directory, stage) = stage();
        let candidate = write_staged_full_snapshot(
            &stage,
            &ProjectionPackWriter::new(temporary.path()),
            binding(),
            Uuid::from_u128(4),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
        )
        .unwrap();
        let paths: Vec<_> = candidate
            .chunks
            .iter()
            .map(|chunk| chunk.path.clone())
            .collect();
        assert!(
            candidate
                .chunks
                .iter()
                .all(|chunk| chunk.ownership() == ProjectionPackOwnership::Created),
            "a fresh staged candidate must retain cleanup rights for every pack"
        );
        assert!(paths.iter().all(|path| path.is_file()));
        drop(candidate);
        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn adapts_a_snapshot_that_would_exceed_the_legacy_chunk_ceiling() {
        let temporary = tempfile::tempdir().unwrap();
        let (_stage_directory, mut stage) = mutable_stage();
        let base_position = TeslaMatePosition {
            id: 20,
            car_id: 1,
            drive_id: Some(10),
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
        for id in 21..=1060 {
            let mut position = base_position.clone();
            position.id = id;
            position.drive_id = None;
            position.date_ms += id;
            stage
                .insert(TeslaMateStageTable::Positions, id, &position)
                .unwrap();
        }
        stage.seal().unwrap();

        let mut capture_paths = Vec::new();
        let mut built = write_staged_full_snapshot_with_projection_state(
            &stage,
            &ProjectionPackWriter::new(temporary.path()),
            binding(),
            Uuid::from_u128(4),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            TeslaMateFragmentLimits {
                max_rows_per_fragment: 3,
                max_projected_json_bytes: 1024 * 1024,
            },
            || -> Result<TeslaMateProjectionStateCapture, TeslaMateFragmentError> {
                let state = TeslaMateProjectionState::create(
                    temporary.path(),
                    TeslaMateProjectionStateLimits {
                        max_rows: 2_000,
                        max_state_bytes: 4 * 1024 * 1024,
                        max_changed_payload_bytes: 4 * 1024 * 1024,
                        minimum_free_bytes: 0,
                    },
                )
                .expect("fresh retry state capture");
                capture_paths.push(state.path_for_test().to_path_buf());
                Ok::<TeslaMateProjectionStateCapture, TeslaMateFragmentError>(
                    TeslaMateProjectionStateCapture::for_initial_base(state),
                )
            },
        )
        .unwrap();

        assert!(built.chunks.len() < ProtocolLimits::default().max_chunks);
        assert!(
            capture_paths.len() > 1,
            "the original small fragment target must retry with a fresh capture"
        );
        assert!(
            capture_paths[..capture_paths.len() - 1]
                .iter()
                .all(|path| !path.exists()),
            "failed retry captures must remove only their own private spools"
        );
        let final_capture_path = capture_paths.last().unwrap().clone();
        assert!(final_capture_path.exists());
        let mut capture = built
            .projection_state
            .take()
            .expect("successful staged candidate retains its final capture");
        capture.seal().expect("seal final retry capture");
        assert!(capture.stats().row_count > 1_000);
        drop(capture);
        assert!(
            !final_capture_path.exists(),
            "dropping an unpublished staged candidate state removes only its final spool"
        );
    }

    #[test]
    fn writes_fk_complete_fragments_and_a_signed_complete_manifest() {
        let temporary = tempfile::tempdir().unwrap();
        let (_stage_directory, stage) = stage();
        let snapshot_id = Uuid::from_u128(4);
        let sequence = SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        };
        let built = write_staged_full_snapshot_with_limits(
            &stage,
            &ProjectionPackWriter::new(temporary.path()),
            binding(),
            snapshot_id,
            sequence,
            TeslaMateFragmentLimits {
                max_rows_per_fragment: 3,
                max_projected_json_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        assert!(built.chunks.len() >= 3);
        assert_eq!(built.report.completed_drives, 1);
        assert_eq!(built.report.projected_charge_samples, 1);
        let transport_rows = built
            .chunks
            .iter()
            .map(|chunk| chunk.metadata.row_count)
            .sum::<u64>();
        let manifest = signed_full_snapshot_manifest(
            &binding(),
            snapshot_id,
            sequence,
            &built.chunks,
            transport_rows,
            &CursorKey::from_bytes([7; 32]),
        )
        .unwrap();
        assert_eq!(manifest.chunk_count as usize, built.chunks.len());
        assert_eq!(manifest.total_rows, transport_rows);
        assert!(transport_rows > built.report.logical_row_count().unwrap());
        assert!(matches!(
            signed_full_snapshot_manifest(
                &binding(),
                snapshot_id,
                sequence,
                &built.chunks,
                built.report.logical_row_count().unwrap(),
                &CursorKey::from_bytes([7; 32]),
            ),
            Err(ProjectionPackError::Invalid(_))
        ));
    }

    #[test]
    fn staged_thirty_nine_update_rows_are_exact_in_signed_schema_2_1_packs() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (_stage_directory, mut stage) = mutable_stage();
        let mut expected = vec![(
            300,
            1,
            1_699_000_000_000,
            1_699_000_060_000,
            "2026.20.1".to_owned(),
        )];
        for id in 301_i64..=338 {
            let offset = id - 301;
            let start_date_ms = 1_800_000_000_000 + offset * 1_000;
            let end_date_ms = start_date_ms + 900;
            let version = format!("2026.44.{}", offset + 1);
            stage
                .insert(
                    TeslaMateStageTable::Updates,
                    id,
                    &TeslaMateUpdate {
                        id,
                        car_id: 1,
                        start_date_ms,
                        end_date_ms: Some(end_date_ms),
                        version: Some(version.clone()),
                    },
                )
                .expect("stage complete update");
            expected.push((id, 1, start_date_ms, end_date_ms, version));
        }
        stage.seal().expect("seal stage with 39 updates");
        let snapshot_id = Uuid::from_u128(44);
        let sequence = SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        };
        let built = write_staged_full_snapshot(
            &stage,
            &ProjectionPackWriter::new(temporary.path().join("packs")),
            binding(),
            snapshot_id,
            sequence,
        )
        .expect("stage with update history builds");

        assert_eq!(built.report.projected_updates, 39);
        assert_eq!(built.report.skipped_incomplete_updates, 0);
        assert!(
            built
                .chunks
                .iter()
                .all(|chunk| chunk.metadata.schema == crate::protocol::HUB_PROJECTION_SCHEMA_V2),
            "publishable update history requires schema 2.1 for every fragment"
        );
        assert_eq!(
            update_rows_from_chunks(temporary.path(), &built.chunks),
            expected,
            "source IDs, firmware versions, and complete date ranges survive staged pack emission"
        );

        let transport_rows = built
            .chunks
            .iter()
            .map(|chunk| chunk.metadata.row_count)
            .sum::<u64>();
        let manifest = signed_full_snapshot_manifest(
            &binding(),
            snapshot_id,
            sequence,
            &built.chunks,
            transport_rows,
            &CursorKey::from_bytes([41; 32]),
        )
        .expect("sign complete update-history manifest");
        manifest.validate().expect("signed manifest validates");
        assert_eq!(manifest.total_rows, transport_rows);
    }

    #[test]
    fn staged_full_snapshot_uses_schema_2_1_with_an_empty_update_table() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (_fixture_directory, fixture_stage) = stage();
        let car = fixture_stage
            .get::<TeslaMateCar>(TeslaMateStageTable::Cars, 1)
            .expect("read fixture car")
            .expect("fixture car");
        let mut empty_stage = TeslaMateStage::create(
            temporary.path().join("empty-stage"),
            TeslaMateStageLimits {
                max_rows: 4,
                max_stage_bytes: 64 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("create empty-update stage");
        empty_stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .expect("stage car without updates");
        empty_stage.seal().expect("seal empty-update stage");

        let snapshot_id = Uuid::from_u128(45);
        let sequence = SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        };
        let built = write_staged_full_snapshot(
            &empty_stage,
            &ProjectionPackWriter::new(temporary.path().join("packs")),
            binding(),
            snapshot_id,
            sequence,
        )
        .expect("empty update history still builds a complete source pack");

        assert_eq!(built.report.projected_updates, 0);
        assert_eq!(built.chunks.len(), 1);
        assert_eq!(
            built.chunks[0].metadata.schema,
            crate::protocol::HUB_PROJECTION_SCHEMA_V2,
            "ordinary staged full snapshots never silently fall back to schema 2.0"
        );
        assert!(
            update_rows_from_chunks(temporary.path(), &built.chunks).is_empty(),
            "schema 2.1 keeps the updates table even when no source rows exist"
        );

        let key = CursorKey::from_bytes([42; 32]);
        let manifest = signed_full_snapshot_manifest(
            &binding(),
            snapshot_id,
            sequence,
            &built.chunks,
            built.chunks[0].metadata.row_count,
            &key,
        )
        .expect("sign empty-update schema-2.1 manifest");
        manifest
            .validate_terminal_cursor(&key)
            .expect("empty update schema-2.1 manifest remains signed and valid");
    }
}
