//! Direct, bounded TeslaMate PostgreSQL to immutable typed-pack production.
//!
//! The source stays inside one read-only repeatable-read transaction. Large
//! child tables are decoded one keyset page at a time and immediately folded
//! into bounded pack fragments; no JSON or whole-history stage is created.

use std::collections::HashMap;

use thiserror::Error;
use tokio_postgres::{Client, Row};
use uuid::Uuid;

use crate::{
    credentials::TeslaMatePostgresPassword,
    hub_pack::{
        ProjectionBinding, ProjectionCharge, ProjectionDrive, ProjectionPackWriter,
        ProjectionSnapshot,
    },
    protocol::SequenceRange,
    teslamate::ReadOnlySource,
    teslamate_fragments::{
        FragmentAccumulator, PackSink, StagedProjectionPacks, TeslaMateFragmentError,
        TeslaMateFragmentLimits, serialized_bytes,
    },
    teslamate_projection::{
        ChargeProjectionFacts, DriveRelations, ProjectionReport, TeslaMateAddress,
        TeslaMateChargingProcess, TeslaMateDrive, TeslaMateGeofence, TeslaMatePosition,
        TeslaMateProjectionError, TeslaMateUpdate, project_car, project_charge,
        project_charge_sample, project_drive, project_position,
    },
    teslamate_reader::{
        TeslaMateReadLimits, TeslaMateReaderError, decode_charge, decode_position,
        open_snapshot_session, read_addresses, read_cars, read_charging_processes, read_drives,
        read_geofences, read_updates,
    },
    teslamate_schema::{SourceTable, projection},
};

#[allow(clippy::too_many_arguments)]
pub async fn write_direct_full_snapshot(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    read_limits: TeslaMateReadLimits,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
) -> Result<StagedProjectionPacks, TeslaMateDirectError> {
    let (session, selected_car_id_i16) =
        open_snapshot_session(source, password, selected_car_id, read_limits).await?;
    let result = write_from_session(
        &session.client,
        selected_car_id,
        selected_car_id_i16,
        read_limits,
        writer,
        binding,
        snapshot_id,
        sequence,
    )
    .await;
    let finish = session.finish().await;
    match (result, finish) {
        (Ok(packs), Ok(())) => Ok(packs),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_from_session(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    read_limits: TeslaMateReadLimits,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
) -> Result<StagedProjectionPacks, TeslaMateDirectError> {
    let mut retained_rows = 0_usize;
    let cars = read_cars(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    let car = cars
        .first()
        .ok_or(TeslaMateDirectError::SelectedCarMissing)?;
    let addresses =
        read_addresses(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    let geofences =
        read_geofences(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    let updates =
        read_updates(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    let drives = read_drives(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    let processes =
        read_charging_processes(client, selected_car_id_i16, read_limits, &mut retained_rows)
            .await?;

    let projected_car = project_car(car, latest_firmware(&updates, selected_car_id)?)?;
    let address_by_id: HashMap<_, _> = addresses.into_iter().map(|row| (row.id, row)).collect();
    let geofence_by_id: HashMap<_, _> = geofences.into_iter().map(|row| (row.id, row)).collect();
    let fragment_limits = TeslaMateFragmentLimits::default();
    let mut sink = PackSink::new(writer, binding, snapshot_id, sequence);
    let mut report = ProjectionReport::default();

    let projected_drives = write_drives(
        client,
        selected_car_id,
        selected_car_id_i16,
        drives,
        &address_by_id,
        &geofence_by_id,
        &projected_car,
        fragment_limits,
        &mut sink,
        &mut report,
    )
    .await?;
    write_positions(
        client,
        selected_car_id,
        selected_car_id_i16,
        read_limits,
        &mut retained_rows,
        &projected_car,
        &projected_drives,
        fragment_limits,
        &mut sink,
        &mut report,
    )
    .await?;
    write_charges(
        client,
        selected_car_id,
        selected_car_id_i16,
        read_limits,
        &mut retained_rows,
        processes,
        &address_by_id,
        &geofence_by_id,
        &projected_car,
        fragment_limits,
        &mut sink,
        &mut report,
    )
    .await?;

    if sink.chunks.is_empty() {
        sink.write(ProjectionSnapshot {
            cars: vec![projected_car],
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
        })?;
    }
    let fingerprint = sink.fingerprint();
    Ok(StagedProjectionPacks {
        chunks: sink.chunks,
        report,
        fingerprint,
    })
}

#[allow(clippy::too_many_arguments)]
async fn write_drives(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    drives: Vec<TeslaMateDrive>,
    addresses: &HashMap<i64, TeslaMateAddress>,
    geofences: &HashMap<i64, TeslaMateGeofence>,
    car: &crate::hub_pack::ProjectionCar,
    limits: TeslaMateFragmentLimits,
    sink: &mut PackSink<'_>,
    report: &mut ProjectionReport,
) -> Result<HashMap<i64, ProjectionDrive>, TeslaMateDirectError> {
    let mut projected_by_id = HashMap::new();
    let mut accumulator = FragmentAccumulator::new(car.clone(), limits)?;
    for drive in drives {
        let start_position =
            related_position(client, drive.start_position_id, selected_car_id_i16).await?;
        let end_position =
            related_position(client, drive.end_position_id, selected_car_id_i16).await?;
        let projected = project_drive(
            &drive,
            selected_car_id,
            DriveRelations {
                start_position: start_position.as_ref(),
                end_position: end_position.as_ref(),
                start_address: related(addresses, drive.start_address_id),
                end_address: related(addresses, drive.end_address_id),
                start_geofence: related(geofences, drive.start_geofence_id),
                end_geofence: related(geofences, drive.end_geofence_id),
            },
        )?;
        let Some(projected) = projected else {
            report.skipped_open_drives = checked_increment(report.skipped_open_drives)?;
            continue;
        };
        accumulator.prepare(sink, |_| Ok((1, serialized_bytes(&projected)?)))?;
        accumulator.drives.push(projected.clone());
        projected_by_id.insert(projected.id, projected);
        report.completed_drives = checked_increment(report.completed_drives)?;
    }
    accumulator.flush(sink)?;
    Ok(projected_by_id)
}

#[allow(clippy::too_many_arguments)]
async fn write_positions(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    read_limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    car: &crate::hub_pack::ProjectionCar,
    drives: &HashMap<i64, ProjectionDrive>,
    limits: TeslaMateFragmentLimits,
    sink: &mut PackSink<'_>,
    report: &mut ProjectionReport,
) -> Result<(), TeslaMateDirectError> {
    let mut accumulator = FragmentAccumulator::new(car.clone(), limits)?;
    for_each_integer_page(
        client,
        SourceTable::Positions,
        selected_car_id_i16,
        read_limits,
        retained_rows,
        decode_position,
        |position| {
            let Some(drive_id) = position.drive_id else {
                report.skipped_unattached_positions =
                    checked_increment(report.skipped_unattached_positions)?;
                return Ok(());
            };
            let Some(drive) = drives.get(&drive_id) else {
                report.skipped_unattached_positions =
                    checked_increment(report.skipped_unattached_positions)?;
                return Ok(());
            };
            let projected = project_position(&position, selected_car_id, true)?
                .expect("completed drive position projects");
            accumulator.prepare(sink, |current| {
                let parent_is_new = !current.drive_ids.contains(&drive.id);
                Ok((
                    1 + u64::from(parent_is_new),
                    serialized_bytes(&projected)?
                        .checked_add(if parent_is_new {
                            serialized_bytes(drive)?
                        } else {
                            0
                        })
                        .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?,
                ))
            })?;
            if accumulator.drive_ids.insert(drive.id) {
                accumulator.drives.push(drive.clone());
            }
            accumulator.positions.push(projected);
            report.projected_positions = checked_increment(report.projected_positions)?;
            Ok(())
        },
    )
    .await?;
    accumulator.flush(sink)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_charges(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    read_limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    processes: Vec<TeslaMateChargingProcess>,
    addresses: &HashMap<i64, TeslaMateAddress>,
    geofences: &HashMap<i64, TeslaMateGeofence>,
    car: &crate::hub_pack::ProjectionCar,
    limits: TeslaMateFragmentLimits,
    sink: &mut PackSink<'_>,
    report: &mut ProjectionReport,
) -> Result<(), TeslaMateDirectError> {
    let mut facts = HashMap::<i64, ChargeProjectionFacts>::new();
    let mut sample_counts = HashMap::<i64, u64>::new();
    for_each_integer_page(
        client,
        SourceTable::Charges,
        selected_car_id_i16,
        read_limits,
        retained_rows,
        decode_charge,
        |sample| {
            facts
                .entry(sample.charging_process_id)
                .or_default()
                .observe(&sample);
            let count = sample_counts.entry(sample.charging_process_id).or_default();
            *count = checked_increment(*count)?;
            Ok(())
        },
    )
    .await?;

    let mut projected_by_id = HashMap::<i64, ProjectionCharge>::new();
    let mut empty = FragmentAccumulator::new(car.clone(), limits)?;
    let empty_charge_facts = ChargeProjectionFacts::default();
    for process in processes {
        let position = related_position(client, process.position_id, selected_car_id_i16).await?;
        let projected = project_charge(
            &process,
            selected_car_id,
            position.as_ref(),
            related(addresses, process.address_id),
            related(geofences, process.geofence_id),
            facts.get(&process.id).unwrap_or(&empty_charge_facts),
        )?;
        report.projected_charges = checked_increment(report.projected_charges)?;
        if sample_counts.get(&process.id).copied().unwrap_or(0) == 0 {
            empty.prepare(sink, |_| Ok((1, serialized_bytes(&projected)?)))?;
            empty.charges.push(projected.clone());
        }
        projected_by_id.insert(process.id, projected);
    }
    empty.flush(sink)?;

    let mut samples = FragmentAccumulator::new(car.clone(), limits)?;
    let mut second_pass_rows = 0_usize;
    for_each_integer_page(
        client,
        SourceTable::Charges,
        selected_car_id_i16,
        TeslaMateReadLimits {
            maximum_rows: read_limits.maximum_rows,
            ..read_limits
        },
        &mut second_pass_rows,
        decode_charge,
        |sample| {
            let parent = projected_by_id.get(&sample.charging_process_id).ok_or(
                TeslaMateDirectError::MissingChargingProcess {
                    process_id: sample.charging_process_id,
                },
            )?;
            let projected = project_charge_sample(&sample);
            samples.prepare(sink, |current| {
                let parent_is_new = !current.charge_ids.contains(&parent.id);
                Ok((
                    1 + u64::from(parent_is_new),
                    serialized_bytes(&projected)?
                        .checked_add(if parent_is_new {
                            serialized_bytes(parent)?
                        } else {
                            0
                        })
                        .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?,
                ))
            })?;
            if samples.charge_ids.insert(parent.id) {
                samples.charges.push(parent.clone());
            }
            samples.charge_samples.push(projected);
            report.projected_charge_samples = checked_increment(report.projected_charge_samples)?;
            Ok(())
        },
    )
    .await?;
    samples.flush(sink)?;
    Ok(())
}

async fn related_position(
    client: &Client,
    id: Option<i64>,
    selected_car_id: i16,
) -> Result<Option<TeslaMatePosition>, TeslaMateDirectError> {
    let Some(id) = id else {
        return Ok(None);
    };
    let id_i32 = i32::try_from(id).map_err(|_| TeslaMateDirectError::InvalidRelatedPosition(id))?;
    let after_id = id_i32
        .checked_sub(1)
        .ok_or(TeslaMateDirectError::InvalidRelatedPosition(id))?;
    let rows = client
        .query(
            projection(SourceTable::Positions).sql,
            &[&after_id, &1_i64, &selected_car_id],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(TeslaMateDirectError::MissingRelatedPosition(id));
    };
    let position = decode_position(row)?;
    if position.id != id {
        return Err(TeslaMateDirectError::MissingRelatedPosition(id));
    }
    Ok(Some(position))
}

async fn for_each_integer_page<T, F>(
    client: &Client,
    table: SourceTable,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    decode: fn(&Row) -> Result<T, TeslaMateReaderError>,
    mut operation: F,
) -> Result<(), TeslaMateDirectError>
where
    F: FnMut(T) -> Result<(), TeslaMateDirectError>,
{
    let mut last_id = 0_i32;
    let page_size = i64::from(limits.page_size);
    loop {
        let progress_million_before = *retained_rows / 1_000_000;
        let rows = client
            .query(
                projection(table).sql,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = rows.len();
        for row in rows {
            let id: i32 = row.try_get("id")?;
            if id <= last_id {
                return Err(TeslaMateDirectError::NonProgressingPage(table.name()));
            }
            last_id = id;
            *retained_rows =
                retained_rows
                    .checked_add(1)
                    .ok_or(TeslaMateDirectError::MaximumRowsExceeded {
                        maximum: limits.maximum_rows,
                    })?;
            if *retained_rows > limits.maximum_rows {
                return Err(TeslaMateDirectError::MaximumRowsExceeded {
                    maximum: limits.maximum_rows,
                });
            }
            operation(decode(&row)?)?;
        }
        let progress_million_after = *retained_rows / 1_000_000;
        if progress_million_after > progress_million_before {
            tracing::info!(
                table = table.name(),
                source_rows = *retained_rows,
                "direct TeslaMate import progress"
            );
        }
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}

fn latest_firmware(
    updates: &[TeslaMateUpdate],
    selected_car_id: i64,
) -> Result<Option<String>, TeslaMateDirectError> {
    let mut latest = None::<((i64, i64, i64), String)>;
    for update in updates {
        if update.car_id != selected_car_id {
            return Err(TeslaMateDirectError::UpdateWrongCar {
                update_id: update.id,
                expected_car_id: selected_car_id,
                found_car_id: update.car_id,
            });
        }
        let (Some(end_date_ms), Some(version)) = (
            update.end_date_ms,
            update
                .version
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        ) else {
            continue;
        };
        let order = (end_date_ms, update.start_date_ms, update.id);
        if latest.as_ref().is_none_or(|(current, _)| order > *current) {
            latest = Some((order, version.to_owned()));
        }
    }
    Ok(latest.map(|(_, version)| version))
}

fn related<T>(rows: &HashMap<i64, T>, id: Option<i64>) -> Option<&T> {
    id.and_then(|id| rows.get(&id))
}

fn checked_increment(value: u64) -> Result<u64, TeslaMateDirectError> {
    value
        .checked_add(1)
        .ok_or(TeslaMateFragmentError::ReportOverflow.into())
}

#[derive(Debug, Error)]
pub enum TeslaMateDirectError {
    #[error("TeslaMate selected car disappeared during direct import")]
    SelectedCarMissing,
    #[error("TeslaMate direct import page did not progress for {0}")]
    NonProgressingPage(&'static str),
    #[error("TeslaMate direct import exceeded the {maximum} source-row ceiling")]
    MaximumRowsExceeded { maximum: usize },
    #[error("TeslaMate related position id {0} is invalid")]
    InvalidRelatedPosition(i64),
    #[error("TeslaMate related position id {0} is missing")]
    MissingRelatedPosition(i64),
    #[error("TeslaMate charge samples reference missing process {process_id}")]
    MissingChargingProcess { process_id: i64 },
    #[error("TeslaMate update {update_id} belongs to car {found_car_id}, not {expected_car_id}")]
    UpdateWrongCar {
        update_id: i64,
        expected_car_id: i64,
        found_car_id: i64,
    },
    #[error(transparent)]
    Reader(#[from] TeslaMateReaderError),
    #[error(transparent)]
    Postgres(#[from] tokio_postgres::Error),
    #[error(transparent)]
    Projection(#[from] TeslaMateProjectionError),
    #[error(transparent)]
    Fragment(#[from] TeslaMateFragmentError),
}
