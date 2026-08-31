// SPDX-License-Identifier: AGPL-3.0-only

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
        if self.cleanup_on_drop
            && self
                .chunks
                .iter()
                .any(BuiltProjectionPack::may_remove_unpublished_file)
        {
            // A complete candidate may already be visible if SQLite returned
            // an ambiguous commit outcome. Drop has neither the publication
            // gate nor a durable catalogue witness, so deleting here would be
            // unsafe. Startup repair owns proof-based orphan cleanup.
            tracing::warn!(
                "retaining unpublished TeslaMate candidate packs for gated startup repair"
            );
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
        let drive_is_present = accumulator.drive_ids.contains(&drive.id);
        accumulator.prepare_with_parent(
            sink,
            drive_is_present,
            || Ok((1, serialized_bytes(&position)?)),
            || Ok((1, serialized_bytes(&drive)?)),
        )?;
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
            let charge_is_present = accumulator.charge_ids.contains(&charge.id);
            accumulator.prepare_with_parent(
                sink,
                charge_is_present,
                || Ok((1, serialized_bytes(&projected)?)),
                || Ok((1, serialized_bytes(charge)?)),
            )?;
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

impl PackBuildJob {
    fn build(self, writer: &ProjectionPackWriter) -> PackBuildResult {
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: self.snapshot_id,
            ordinal: self.ordinal,
            binding: self.binding,
            sequence: self.sequence,
            snapshot: &self.snapshot,
        };
        let built = if self.states.is_empty() && self.updates.is_empty() && !self.schema_2_1 {
            writer.write_full_snapshot(&request)
        } else {
            writer.write_full_snapshot_with_states_and_updates(
                &request,
                &self.states,
                &self.updates,
            )
        };
        PackBuildResult {
            ordinal: self.ordinal,
            snapshot: self.snapshot,
            updates: self.updates,
            built,
        }
    }
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
                    let result = job.build(&writer);
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
    capture_state_only: bool,
    synchronous_pack_builds: bool,
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
            capture_state_only: false,
            synchronous_pack_builds: false,
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

    /// Stream rows into the projection-state comparison spool without making
    /// a disposable full pack. Successor imports use this to produce only
    /// sparse deltas; their current base remains immutable and readable.
    pub(crate) fn capture_state_only(mut self) -> Self {
        self.capture_state_only = true;
        self
    }

    /// A direct import shares its filesystem with its comparison spool. Build
    /// one fragment inline, then record its state, so their temporary files
    /// cannot overlap and consume the free-space reserve together.
    pub(crate) fn with_synchronous_pack_builds(mut self) -> Self {
        self.synchronous_pack_builds = true;
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

    pub(crate) fn captures_state_only(&self) -> bool {
        self.capture_state_only
    }

    /// Record a streamed position directly into a successor comparison spool.
    /// Returning `false` tells a full-pack caller to use its fragment path.
    pub(crate) fn capture_state_only_position(
        &mut self,
        position: &ProjectionPosition,
    ) -> Result<bool, TeslaMateFragmentError> {
        if !self.capture_state_only {
            return Ok(false);
        }
        if let Some(capture) = self.projection_state.as_mut() {
            capture.record_position(position)?;
        }
        Ok(true)
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
        if self.capture_only || self.capture_state_only {
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
        if self.synchronous_pack_builds {
            self.accept_completed(job.build(self.writer))?;
            return Ok(());
        }
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

#[derive(Clone, Copy)]
struct PreparedFragmentAddition {
    rows: u64,
    bytes: u64,
}

impl PreparedFragmentAddition {
    fn combine(self, other: Self) -> Result<Self, TeslaMateFragmentError> {
        Ok(Self {
            rows: self
                .rows
                .checked_add(other.rows)
                .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?,
            bytes: self
                .bytes
                .checked_add(other.bytes)
                .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?,
        })
    }
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
        F: FnOnce(&Self) -> Result<(u64, u64), TeslaMateFragmentError>,
    {
        let (additional_rows, additional_bytes) = addition(self)?;
        let addition = PreparedFragmentAddition {
            rows: additional_rows,
            bytes: additional_bytes,
        };
        self.prepare_sized(sink, addition, || Ok(addition))
    }

    /// Prepare one child row whose parent is repeated only when absent from
    /// the active fragment. Child and parent JSON sizing closures are each
    /// evaluated at most once, including when the child forces a flush.
    pub(crate) fn prepare_with_parent<C, P>(
        &mut self,
        sink: &mut PackSink<'_>,
        parent_is_present: bool,
        child: C,
        parent: P,
    ) -> Result<(), TeslaMateFragmentError>
    where
        C: FnOnce() -> Result<(u64, u64), TeslaMateFragmentError>,
        P: FnOnce() -> Result<(u64, u64), TeslaMateFragmentError>,
    {
        let (child_rows, child_bytes) = child()?;
        let child = PreparedFragmentAddition {
            rows: child_rows,
            bytes: child_bytes,
        };
        if parent_is_present {
            return self.prepare_sized(sink, child, || {
                let (parent_rows, parent_bytes) = parent()?;
                child.combine(PreparedFragmentAddition {
                    rows: parent_rows,
                    bytes: parent_bytes,
                })
            });
        }
        let (parent_rows, parent_bytes) = parent()?;
        let with_parent = child.combine(PreparedFragmentAddition {
            rows: parent_rows,
            bytes: parent_bytes,
        })?;
        self.prepare_sized(sink, with_parent, || Ok(with_parent))
    }

    fn prepare_sized<F>(
        &mut self,
        sink: &mut PackSink<'_>,
        current: PreparedFragmentAddition,
        after_flush: F,
    ) -> Result<(), TeslaMateFragmentError>
    where
        F: FnOnce() -> Result<PreparedFragmentAddition, TeslaMateFragmentError>,
    {
        let addition = if self.exceeds(current.rows, current.bytes)? && self.has_data() {
            self.flush(sink)?;
            after_flush()?
        } else {
            current
        };
        if self.exceeds(addition.rows, addition.bytes)? {
            return Err(TeslaMateFragmentError::SingleFragmentValueExceedsTarget);
        }
        self.payload_bytes = self
            .payload_bytes
            .checked_add(addition.bytes)
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
#[path = "fragments/tests.rs"]
mod tests;
