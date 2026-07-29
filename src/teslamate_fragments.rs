//! Bounded full-snapshot pack production from a sealed TeslaMate stage.
//!
//! The PostgreSQL reader owns capture consistency. This producer owns only
//! the local sealed stage and never reconstructs a whole `TeslaMateHistory`.
//! Every generated pack is self-contained: it repeats its car and the parent
//! drive or charge rows required by its children. The manifest is published by
//! the caller only after this module has verified every immutable pack.

use std::collections::HashSet;

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    hub_pack::{
        BuiltProjectionPack, ProjectionBinding, ProjectionCar, ProjectionCharge,
        ProjectionChargeSample, ProjectionDrive, ProjectionPackRequest, ProjectionPackWriter,
        ProjectionPosition, ProjectionSnapshot,
    },
    protocol::{ProtocolLimits, SequenceRange},
    teslamate_projection::{
        ChargeProjectionFacts, DriveRelations, ProjectionReport, TeslaMateAddress, TeslaMateCar,
        TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive, TeslaMateGeofence,
        TeslaMatePosition, TeslaMateProjectionError, TeslaMateUpdate, project_car, project_charge,
        project_charge_sample, project_drive, project_position,
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
    let _ = stage.stats()?;
    let car =
        required_row::<TeslaMateCar>(stage, TeslaMateStageTable::Cars, binding.selected_car_id)?;
    let projected_car = project_car(&car, latest_firmware(stage, binding.selected_car_id)?)?;
    let mut sink = PackSink::new(writer, binding, snapshot_id, sequence);
    let mut report = ProjectionReport::default();

    write_drive_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;
    write_position_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;
    write_charge_fragments(stage, &projected_car, &mut sink, limits, &mut report)?;

    // A new car with no history is still a legitimate complete replacement.
    if sink.chunks.is_empty() {
        let accumulator = FragmentAccumulator::new(projected_car, limits)?;
        sink.write(accumulator.finish())?;
    }
    Ok(StagedProjectionPacks {
        chunks: sink.chunks,
        report,
    })
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
    let mut accumulator = FragmentAccumulator::new(car.clone(), limits)?;
    for_each_page::<TeslaMatePosition, _>(stage, TeslaMateStageTable::Positions, |position| {
        let Some((drive, projected)) = project_position_from_stage(stage, &position, car.id)?
        else {
            report.skipped_unattached_positions = report
                .skipped_unattached_positions
                .checked_add(1)
                .ok_or(TeslaMateFragmentError::ReportOverflow)?;
            return Ok(());
        };
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
            accumulator.drives.push(drive);
        }
        accumulator.positions.push(projected);
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

fn project_position_from_stage(
    stage: &TeslaMateStage,
    position: &TeslaMatePosition,
    selected_car_id: i64,
) -> Result<Option<(ProjectionDrive, ProjectionPosition)>, TeslaMateFragmentError> {
    let Some(drive_id) = position.drive_id else {
        return Ok(None);
    };
    let drive = required_row::<TeslaMateDrive>(stage, TeslaMateStageTable::Drives, drive_id)?;
    let Some(projected_drive) = project_drive_from_stage(stage, &drive, selected_car_id)? else {
        return Ok(None);
    };
    let projected_position = project_position(position, selected_car_id, true)?
        .expect("position with a completed drive must project");
    Ok(Some((projected_drive, projected_position)))
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

struct PackSink<'a> {
    writer: &'a ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    chunks: Vec<BuiltProjectionPack>,
}

impl<'a> PackSink<'a> {
    fn new(
        writer: &'a ProjectionPackWriter,
        binding: ProjectionBinding,
        snapshot_id: Uuid,
        sequence: SequenceRange,
    ) -> Self {
        Self {
            writer,
            binding,
            snapshot_id,
            sequence,
            chunks: Vec::new(),
        }
    }

    fn write(&mut self, snapshot: ProjectionSnapshot) -> Result<(), TeslaMateFragmentError> {
        if self.chunks.len() >= ProtocolLimits::default().max_chunks {
            return Err(TeslaMateFragmentError::TooManyFragments);
        }
        let ordinal = u32::try_from(self.chunks.len())
            .map_err(|_| TeslaMateFragmentError::TooManyFragments)?;
        let built = self.writer.write_full_snapshot(&ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: self.snapshot_id,
            ordinal,
            binding: self.binding.clone(),
            sequence: self.sequence,
            snapshot: &snapshot,
        })?;
        self.chunks.push(built);
        Ok(())
    }
}

struct FragmentAccumulator {
    car: ProjectionCar,
    limits: TeslaMateFragmentLimits,
    payload_bytes: u64,
    drives: Vec<ProjectionDrive>,
    positions: Vec<ProjectionPosition>,
    charges: Vec<ProjectionCharge>,
    charge_samples: Vec<ProjectionChargeSample>,
    drive_ids: HashSet<i64>,
    charge_ids: HashSet<i64>,
}

impl FragmentAccumulator {
    fn new(
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

    fn prepare<F>(
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

    fn has_data(&self) -> bool {
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

    fn flush(&mut self, sink: &mut PackSink<'_>) -> Result<(), TeslaMateFragmentError> {
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

fn serialized_bytes<T: Serialize>(value: &T) -> Result<u64, TeslaMateFragmentError> {
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
        hub_pack::{ProjectionBinding, signed_full_snapshot_manifest},
        protocol::CursorKey,
        teslamate_stage::TeslaMateStageLimits,
    };

    fn stage() -> (tempfile::TempDir, TeslaMateStage) {
        let temporary = tempfile::tempdir().unwrap();
        let mut stage = TeslaMateStage::create(
            temporary.path(),
            TeslaMateStageLimits {
                max_rows: 100,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .unwrap();
        let car = TeslaMateCar {
            id: 1,
            eid: 99,
            vin: Some("5YJTESTVIN1234567".into()),
            name: Some("Road car".into()),
            model: Some("Model 3".into()),
            trim_badging: None,
            marketing_name: None,
            efficiency_wh_per_km: Some(0.145),
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
            power: Some(10),
            odometer: None,
            ideal_battery_range_km: None,
            rated_battery_range_km: Some(390.0),
            battery_level: Some(78),
            usable_battery_level: Some(77),
            is_climate_on: Some(false),
            outside_temp: Some(18.0),
            inside_temp: Some(20.0),
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
            speed_max: Some(80),
            start_rated_range_km: Some(400.0),
            end_rated_range_km: Some(390.0),
            start_km: None,
            end_km: None,
            distance_km: Some(12.0),
            duration_min: Some(10),
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
        stage.seal().unwrap();
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
            &CursorKey::from_bytes([7; 32]),
        )
        .unwrap();
        assert_eq!(manifest.chunk_count as usize, built.chunks.len());
        assert_eq!(manifest.total_rows, 8);
    }
}
