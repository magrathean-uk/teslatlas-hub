//! Bounded full-snapshot pack production from a sealed TeslaMate stage.
//!
//! The PostgreSQL reader owns capture consistency. This producer owns only
//! the local sealed stage and never reconstructs a whole `TeslaMateHistory`.
//! Every generated pack is self-contained: it repeats its car and the parent
//! drive or charge rows required by its children. The manifest is published by
//! the caller only after this module has verified every immutable pack.

use std::{
    collections::{HashMap, HashSet},
    fs,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    hub_pack::{
        BuiltProjectionPack, ProjectionBinding, ProjectionCar, ProjectionCharge,
        ProjectionChargeSample, ProjectionDrive, ProjectionPackRequest, ProjectionPackWriter,
        ProjectionPosition, ProjectionSnapshot, ProjectionState,
    },
    protocol::{ProtocolLimits, SequenceRange},
    teslamate_projection::{
        ChargeProjectionFacts, DriveRelations, ProjectionReport, TeslaMateAddress, TeslaMateCar,
        TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive, TeslaMateGeofence,
        TeslaMatePosition, TeslaMateProjectionError, TeslaMateState, TeslaMateUpdate, project_car,
        project_charge, project_charge_sample, project_drive, project_position, project_state,
    },
    teslamate_stage::{TeslaMateStage, TeslaMateStageError, TeslaMateStageTable},
};

const STAGE_PAGE_ROWS: u32 = 10_000;
const DEFAULT_MAX_ROWS_PER_FRAGMENT: u64 = 50_000;
const DEFAULT_MAX_PROJECTED_JSON_BYTES: u64 = 8 * 1024 * 1024;

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
    pub geofences: Vec<TeslaMateGeofence>,
    cleanup_on_drop: bool,
}

impl StagedProjectionPacks {
    pub(crate) fn new(
        chunks: Vec<BuiltProjectionPack>,
        report: ProjectionReport,
        fingerprint: crate::protocol::Sha256Digest,
        geofences: Vec<TeslaMateGeofence>,
    ) -> Self {
        Self {
            chunks,
            report,
            fingerprint,
            geofences,
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
            if let Err(error) = fs::remove_file(&chunk.path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %chunk.path.display(), %error, "could not remove unpublished TeslaMate candidate pack");
            }
        }
    }
}

/// Produce all typed packs for a complete staged TeslaMate snapshot. This
/// function writes content-addressed objects but does not alter the Hub
/// catalog. If it returns an error, no manifest can point at partial output.
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
    limits.validate()?;
    let mut effective_limits = limits;
    loop {
        match write_staged_full_snapshot_once(
            stage,
            writer,
            binding.clone(),
            snapshot_id,
            sequence,
            effective_limits,
        ) {
            Err(TeslaMateFragmentError::TooManyFragments) => {
                effective_limits = next_fragment_limits(effective_limits)
                    .ok_or(TeslaMateFragmentError::TooManyFragments)?;
            }
            result => return result,
        }
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
    writer.ensure_full_snapshot_capacity(stage_stats.limits.minimum_free_bytes)?;
    let car =
        required_row::<TeslaMateCar>(stage, TeslaMateStageTable::Cars, binding.selected_car_id)?;
    let projected_car = project_car(&car, latest_firmware(stage, binding.selected_car_id)?)?;
    let states = project_staged_states(stage, binding.selected_car_id)?;
    let geofences = staged_geofences(stage)?;
    let force_schema_2_1 = has_standalone_positions(stage)?;
    let mut sink = if force_schema_2_1 {
        PackSink::new_with_schema_2_1(writer, binding, snapshot_id, sequence, states, true)
    } else {
        PackSink::new(writer, binding, snapshot_id, sequence, states)
    };
    let mut report = ProjectionReport::default();

    write_drive_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;
    write_position_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;
    write_charge_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;

    // A new car with no history is still a legitimate complete replacement.
    if sink.chunks.is_empty() {
        let accumulator = FragmentAccumulator::new(projected_car, limits)?;
        sink.write(accumulator.finish())?;
    }
    let fingerprint = sink.fingerprint();
    Ok(StagedProjectionPacks::new(
        sink.into_chunks(),
        report,
        fingerprint,
        geofences,
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

fn has_standalone_positions(stage: &TeslaMateStage) -> Result<bool, TeslaMateFragmentError> {
    let mut found = false;
    for_each_page::<TeslaMatePosition, _>(stage, TeslaMateStageTable::Positions, |position| {
        if position.drive_id.is_none() {
            found = true;
        }
        Ok(())
    })?;
    Ok(found)
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
    let mut accumulator = FragmentAccumulator::new(car.clone(), limits)?;
    for_each_page::<TeslaMatePosition, _>(stage, TeslaMateStageTable::Positions, |position| {
        let Some(drive_id) = position.drive_id else {
            let projected = crate::lifecycle::imported_position(&position);
            accumulator.prepare(sink, |_| Ok((1, serialized_bytes(&projected)?)))?;
            accumulator.positions.push(projected);
            report.projected_positions = report
                .projected_positions
                .checked_add(1)
                .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            return Ok(());
        };
        let Some(drive) = projected_drives.get(&drive_id) else {
            report.skipped_unattached_positions = report
                .skipped_unattached_positions
                .checked_add(1)
                .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            return Ok(());
        };
        let projected = project_position(&position, car.id, true)?
            .expect("position with a completed drive must project");
        accumulator.prepare(sink, |current| {
            let drive_is_new = !current.drive_ids.contains(&drive.id);
            let added_rows = 1 + u64::from(drive_is_new);
            let added_bytes = serialized_bytes(&projected)?
                .checked_add(if drive_is_new {
                    serialized_bytes(&drive)?
                } else {
                    0
                })
                .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?;
            Ok((added_rows, added_bytes))
        })?;
        if accumulator.drive_ids.insert(drive.id) {
            accumulator.drives.push(drive.clone());
        }
        accumulator.positions.push(projected);
        report.projected_positions = report
            .projected_positions
            .checked_add(1)
            .ok_or(TeslaMateFragmentError::ReportOverflow)?;
        Ok(())
    })?;
    accumulator.flush(sink)
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

fn latest_firmware(
    stage: &TeslaMateStage,
    selected_car_id: i64,
) -> Result<Option<String>, TeslaMateFragmentError> {
    let mut latest = None::<((i64, i64, i64), String)>;
    for_each_page::<TeslaMateUpdate, _>(stage, TeslaMateStageTable::Updates, |update| {
        if update.car_id != selected_car_id {
            return Err(TeslaMateFragmentError::UpdateWrongCar {
                update_id: update.id,
                expected_car_id: selected_car_id,
                found_car_id: update.car_id,
            });
        }
        let Some(end_date_ms) = update.end_date_ms else {
            return Ok(());
        };
        let Some(version) = update
            .version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(());
        };
        let order = (end_date_ms, update.start_date_ms, update.id);
        if latest.as_ref().is_none_or(|(current, _)| order > *current) {
            latest = Some((order, version.to_owned()));
        }
        Ok(())
    })?;
    Ok(latest.map(|(_, version)| version))
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

pub(crate) struct PackSink<'a> {
    writer: &'a ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    states: Vec<ProjectionState>,
    states_fingerprinted: bool,
    schema_2_1: bool,
    fingerprint: Sha256,
    pub(crate) chunks: Vec<BuiltProjectionPack>,
}

impl<'a> PackSink<'a> {
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
            schema_2_1,
            fingerprint,
            chunks: Vec::new(),
        }
    }

    pub(crate) fn fingerprint(&self) -> crate::protocol::Sha256Digest {
        crate::protocol::Sha256Digest::from_bytes(self.fingerprint.clone().finalize().into())
    }

    /// Transfer verified candidate fragments to the caller. Until this method
    /// is called, the sink removes its Hub-owned unpublished files on drop.
    pub(crate) fn into_chunks(mut self) -> Vec<BuiltProjectionPack> {
        std::mem::take(&mut self.chunks)
    }

    pub(crate) fn write(
        &mut self,
        snapshot: ProjectionSnapshot,
    ) -> Result<(), TeslaMateFragmentError> {
        if self.chunks.len() >= ProtocolLimits::default().max_chunks {
            return Err(TeslaMateFragmentError::TooManyFragments);
        }
        let ordinal = u32::try_from(self.chunks.len())
            .map_err(|_| TeslaMateFragmentError::TooManyFragments)?;
        if !self.states_fingerprinted {
            let canonical_states = serde_json::to_vec(&self.states)
                .map_err(TeslaMateFragmentError::SerializeProjectedValue)?;
            self.fingerprint
                .update(b"teslatlas-hub/teslamate-logical-states/v1");
            self.fingerprint.update(
                u64::try_from(canonical_states.len())
                    .map_err(|_| TeslaMateFragmentError::FragmentSizeOverflow)?
                    .to_be_bytes(),
            );
            self.fingerprint.update(&canonical_states);
            self.states_fingerprinted = true;
        }
        let canonical = serde_json::to_vec(&snapshot)
            .map_err(TeslaMateFragmentError::SerializeProjectedValue)?;
        self.fingerprint.update(
            u64::try_from(canonical.len())
                .map_err(|_| TeslaMateFragmentError::FragmentSizeOverflow)?
                .to_be_bytes(),
        );
        self.fingerprint.update(&canonical);
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: self.snapshot_id,
            ordinal,
            binding: self.binding.clone(),
            sequence: self.sequence,
            snapshot: &snapshot,
        };
        let built = if self.states.is_empty() && !self.schema_2_1 {
            self.writer.write_full_snapshot(&request)?
        } else {
            let states = if self.chunks.is_empty() {
                self.states.as_slice()
            } else {
                &[]
            };
            self.writer
                .write_full_snapshot_with_states(&request, states)?
        };
        tracing::info!(
            ordinal = built.metadata.ordinal,
            rows = built.metadata.row_count,
            compressed_bytes = built.metadata.compressed_bytes,
            "wrote verified TeslaMate import pack"
        );
        self.chunks.push(built);
        Ok(())
    }
}

impl Drop for PackSink<'_> {
    fn drop(&mut self) {
        for chunk in self.chunks.drain(..) {
            if let Err(error) = fs::remove_file(&chunk.path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %chunk.path.display(), %error, "could not remove unpublished TeslaMate pack");
            }
        }
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
    #[error("projection report accounting overflowed")]
    ReportOverflow,
    #[error("cannot size a projected fragment value: {0}")]
    SerializeProjectedValue(serde_json::Error),
    #[error(transparent)]
    Stage(#[from] TeslaMateStageError),
    #[error(transparent)]
    Projection(#[from] TeslaMateProjectionError),
    #[error(transparent)]
    Pack(#[from] crate::hub_pack::ProjectionPackError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        hub_pack::{
            ProjectionBinding, ProjectionCar, ProjectionSnapshot, signed_full_snapshot_manifest,
        },
        protocol::CursorKey,
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
            temporary.path(),
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
        let candidate = sink.chunks[0].path.clone();
        assert!(candidate.is_file());
        drop(sink);
        assert!(!candidate.exists());
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
            position.date_ms += i64::from(id);
            stage
                .insert(TeslaMateStageTable::Positions, id, &position)
                .unwrap();
        }
        stage.seal().unwrap();

        let built = write_staged_full_snapshot_with_limits(
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
        )
        .unwrap();

        assert!(built.chunks.len() < ProtocolLimits::default().max_chunks);
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
        let manifest = signed_full_snapshot_manifest(
            &binding(),
            snapshot_id,
            sequence,
            &built.chunks,
            built.report.logical_row_count().unwrap(),
            &CursorKey::from_bytes([7; 32]),
        )
        .unwrap();
        assert_eq!(manifest.chunk_count as usize, built.chunks.len());
        assert_eq!(manifest.total_rows, 5);
        assert!(
            built
                .chunks
                .iter()
                .map(|chunk| chunk.metadata.row_count)
                .sum::<u64>()
                > 5
        );
    }
}
