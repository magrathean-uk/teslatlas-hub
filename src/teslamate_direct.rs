//! Direct, bounded TeslaMate PostgreSQL to immutable typed-pack production.
//!
//! The source stays inside one read-only repeatable-read transaction. Large
//! child tables are decoded one keyset page at a time and immediately folded
//! into bounded pack fragments; no JSON or whole-history stage is created.

use std::{collections::{BTreeSet, HashMap}, path::{Path, PathBuf}, pin::pin};

use futures_util::TryStreamExt;
use rustix::fs::statvfs;
use serde::Serialize;
use thiserror::Error;
use tokio_postgres::Client;
use uuid::Uuid;

use crate::{
    credentials::TeslaMatePostgresPassword,
    hub_pack::{
        ProjectionBinding, ProjectionCharge, ProjectionDrive, ProjectionPackError,
        ProjectionPackWriter, ProjectionSnapshot,
    },
    protocol::SequenceRange,
    teslamate::ReadOnlySource,
    teslamate_fragments::{
        FragmentAccumulator, PackSink, StagedProjectionPacks, TeslaMateFragmentError,
        TeslaMateFragmentLimits, next_fragment_limits, serialized_bytes,
    },
    teslamate_projection::{
        ChargeProjectionFacts, DriveRelations, ProjectionReport, TeslaMateAddress,
        TeslaMateChargingProcess, TeslaMateDrive, TeslaMateGeofence, TeslaMatePosition,
        TeslaMateProjectionError, TeslaMateState, TeslaMateUpdate, project_car, project_charge,
        project_charge_sample, project_drive, project_position, project_state,
    },
    teslamate_reader::{
        TeslaMateReadLimits, TeslaMateReaderError, binary_copy_sql, charge_copy_types,
        decode_binary_charge, decode_binary_position,
        TeslaMateSchemaInfo, open_exported_snapshot_lease, open_snapshot_capture_lane,
        open_snapshot_session_with_schema, position_copy_types, related_positions_binary_copy_sql,
        read_addresses, read_cars, read_charging_processes, read_drives, read_geofences,
        read_open_session, read_updates,
    },
    teslamate_schema::SourceTable,
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
    let (lease, selected_car_id_i16) =
        open_exported_snapshot_lease(source, password, selected_car_id, read_limits).await?;
    let snapshot_token = lease.snapshot_id().to_owned();
    let mut fragment_limits = TeslaMateFragmentLimits::default();
    let result = loop {
        match write_direct_full_snapshot_once(
            source,
            password,
            selected_car_id,
            selected_car_id_i16,
            read_limits,
            writer,
            binding.clone(),
            snapshot_id,
            sequence.clone(),
            &snapshot_token,
            fragment_limits,
        )
        .await
        {
            Err(TeslaMateDirectError::Fragment(TeslaMateFragmentError::TooManyFragments)) => {
                let next = next_fragment_limits(fragment_limits).ok_or(
                    TeslaMateDirectError::Fragment(TeslaMateFragmentError::TooManyFragments),
                )?;
                tracing::warn!(
                    selected_car_id,
                    previous_max_rows_per_fragment = fragment_limits.max_rows_per_fragment,
                    next_max_rows_per_fragment = next.max_rows_per_fragment,
                    previous_max_projected_json_bytes = fragment_limits.max_projected_json_bytes,
                    next_max_projected_json_bytes = next.max_projected_json_bytes,
                    "restarting TeslaMate history capture with larger bounded fragments"
                );
                fragment_limits = next;
            }
            result => break result,
        }
    };
    let lease_finish = lease.finish().await;
    match (result, lease_finish) {
        (Ok(packs), Ok(())) => Ok(packs),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_direct_full_snapshot_once(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    read_limits: TeslaMateReadLimits,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    snapshot_token: &str,
    fragment_limits: TeslaMateFragmentLimits,
) -> Result<StagedProjectionPacks, TeslaMateDirectError> {
    writer.ensure_full_snapshot_capacity(read_limits.minimum_free_bytes)?;
    let metadata = if read_limits.parallel_copy_lanes >= 3 {
        tokio::try_join!(
            read_addresses_lane(
                source,
                password,
                &snapshot_token,
                selected_car_id_i16,
                read_limits
            ),
            read_geofences_lane(
                source,
                password,
                &snapshot_token,
                selected_car_id_i16,
                read_limits
            ),
            read_updates_lane(
                source,
                password,
                &snapshot_token,
                selected_car_id_i16,
                read_limits
            ),
        )
    } else {
        let addresses = read_addresses_lane(
            source,
            password,
            &snapshot_token,
            selected_car_id_i16,
            read_limits,
        )
        .await?;
        let geofences = read_geofences_lane(
            source,
            password,
            &snapshot_token,
            selected_car_id_i16,
            read_limits,
        )
        .await?;
        let updates = read_updates_lane(
            source,
            password,
            &snapshot_token,
            selected_car_id_i16,
            read_limits,
        )
        .await?;
        Ok((addresses, geofences, updates))
    };
    let (addresses, geofences, updates) = match metadata {
        Ok(metadata) => metadata,
        Err(error) => {
            return Err(error);
        }
    };
    let metadata_rows = addresses
        .len()
        .checked_add(geofences.len())
        .and_then(|count| count.checked_add(updates.len()))
        .ok_or(TeslaMateDirectError::MaximumRowsExceeded {
            maximum: read_limits.maximum_rows,
        })?;
    if metadata_rows > read_limits.maximum_rows {
        return Err(TeslaMateDirectError::MaximumRowsExceeded {
            maximum: read_limits.maximum_rows,
        });
    }
    let lane = match open_snapshot_capture_lane(source, password, snapshot_token, read_limits)
        .await
    {
        Ok(lane) => lane,
        Err(error) => {
            return Err(error.into());
        }
    };
    tracing::debug!(
        snapshot_id = %snapshot_id,
        "capturing TeslaMate source snapshot"
    );
    let result = write_from_session(
        &lane.client,
        selected_car_id,
        selected_car_id_i16,
        read_limits,
        metadata_rows,
        addresses,
        geofences,
        updates,
        writer,
        binding,
        snapshot_id,
        sequence,
        fragment_limits,
    )
    .await;
    let lane_finish = lane.finish().await;
    match (result, lane_finish) {
        (Ok(packs), Ok(())) => Ok(packs),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

/// Read one car's active TeslaMate sessions through the same source adapter as
/// direct pack production. No pack is written by this bridge.
pub async fn read_direct_open_session(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<crate::teslamate_projection::TeslaMateOpenSession, TeslaMateDirectError> {
    read_open_session(source, password, selected_car_id, limits)
        .await
        .map_err(Into::into)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TeslaMateSourceCounts {
    pub cars: u64,
    pub drives: u64,
    pub positions: u64,
    #[serde(rename = "chargingProcesses")]
    pub charging_processes: u64,
    pub charges: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeslaMatePreflightAdmission {
    pub passed: bool,
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeslaMatePreflightReport {
    #[serde(rename = "selectedCarId")]
    pub selected_car_id: i64,
    #[serde(rename = "sourceDatabaseBytes")]
    pub source_database_bytes: u64,
    pub schema: TeslaMateSchemaInfo,
    #[serde(rename = "sourceRowCounts")]
    pub source_row_counts: TeslaMateSourceCounts,
    #[serde(rename = "targetAvailableBytes")]
    pub target_available_bytes: u64,
    #[serde(rename = "configuredMaximumRows")]
    pub configured_maximum_rows: usize,
    #[serde(rename = "configuredStagingLimitBytes")]
    pub configured_staging_limit_bytes: u64,
    #[serde(rename = "configuredStagingReserveBytes")]
    pub configured_staging_reserve_bytes: u64,
    pub admission: TeslaMatePreflightAdmission,
}

pub async fn preflight_teslamate_import(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    read_limits: TeslaMateReadLimits,
    target_packs_dir: &Path,
) -> Result<TeslaMatePreflightReport, TeslaMateDirectError> {
    let (session, selected_car_id_i16, schema) =
        open_snapshot_session_with_schema(source, password, selected_car_id, read_limits).await?;
    let result = async {
        let source_database_bytes = read_source_database_size(&session.client).await?;
        let source_row_counts =
            read_direct_source_counts(&session.client, selected_car_id_i16).await?;
        let (target_available_bytes, capacity_passed) =
            preflight_target_capacity(target_packs_dir, read_limits.minimum_free_bytes)?;
        Ok(TeslaMatePreflightReport {
            selected_car_id,
            source_database_bytes,
            schema,
            source_row_counts,
            target_available_bytes,
            configured_maximum_rows: read_limits.maximum_rows,
            configured_staging_limit_bytes: read_limits.maximum_stage_bytes,
            configured_staging_reserve_bytes: read_limits.minimum_free_bytes,
            admission: TeslaMatePreflightAdmission {
                passed: capacity_passed,
                reason: (!capacity_passed).then_some("insufficient_target_capacity"),
            },
        })
    }
    .await;
    let finish = session.finish().await;
    match (result, finish) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

async fn read_source_database_size(client: &Client) -> Result<u64, TeslaMateDirectError> {
    let row = client
        .query_one(
            "SELECT pg_database_size(current_database())::bigint AS \"bytes\"",
            &[],
        )
        .await?;
    let bytes: i64 = row.try_get("bytes")?;
    u64::try_from(bytes)
        .map_err(|_| TeslaMateDirectError::InvalidSourceDatabaseSize { bytes })
}

fn preflight_target_capacity(
    target_packs_dir: &Path,
    minimum_free_bytes: u64,
) -> Result<(u64, bool), TeslaMateDirectError> {
    let writer = ProjectionPackWriter::new(target_packs_dir);
    match writer.ensure_full_snapshot_capacity(minimum_free_bytes) {
        Ok(()) => Ok((target_available_bytes(target_packs_dir)?, true)),
        Err(ProjectionPackError::InsufficientFreeSpace { available, .. }) => {
            Ok((available, false))
        }
        Err(error) => Err(error.into()),
    }
}

fn target_available_bytes(path: &Path) -> Result<u64, TeslaMateDirectError> {
    let stats = statvfs(path).map_err(|source| TeslaMateDirectError::TargetFilesystemSpace {
        path: path.to_path_buf(),
        source,
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(TeslaMateDirectError::TargetCapacityOverflow)
}

async fn read_addresses_lane(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    snapshot_id: &str,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<Vec<TeslaMateAddress>, TeslaMateDirectError> {
    let lane = open_snapshot_capture_lane(source, password, snapshot_id, limits).await?;
    let mut retained = 0;
    let result = read_addresses(&lane.client, selected_car_id, limits, &mut retained).await;
    let finish = lane.finish().await;
    match (result, finish) {
        (Ok(rows), Ok(())) => Ok(rows),
        (Err(error), _) => Err(error.into()),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

async fn read_geofences_lane(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    snapshot_id: &str,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<Vec<TeslaMateGeofence>, TeslaMateDirectError> {
    let lane = open_snapshot_capture_lane(source, password, snapshot_id, limits).await?;
    let mut retained = 0;
    let result = read_geofences(&lane.client, selected_car_id, limits, &mut retained).await;
    let finish = lane.finish().await;
    match (result, finish) {
        (Ok(rows), Ok(())) => Ok(rows),
        (Err(error), _) => Err(error.into()),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

async fn read_updates_lane(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    snapshot_id: &str,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<Vec<TeslaMateUpdate>, TeslaMateDirectError> {
    let lane = open_snapshot_capture_lane(source, password, snapshot_id, limits).await?;
    let mut retained = 0;
    let result = read_updates(&lane.client, selected_car_id, limits, &mut retained).await;
    let finish = lane.finish().await;
    match (result, finish) {
        (Ok(rows), Ok(())) => Ok(rows),
        (Err(error), _) => Err(error.into()),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_from_session(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    read_limits: TeslaMateReadLimits,
    mut retained_rows: usize,
    addresses: Vec<TeslaMateAddress>,
    geofences: Vec<TeslaMateGeofence>,
    updates: Vec<TeslaMateUpdate>,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    fragment_limits: TeslaMateFragmentLimits,
) -> Result<StagedProjectionPacks, TeslaMateDirectError> {
    let cars = read_cars(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    let car = cars
        .first()
        .ok_or(TeslaMateDirectError::SelectedCarMissing)?;
    let drives = read_drives(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    let processes =
        read_charging_processes(client, selected_car_id_i16, read_limits, &mut retained_rows)
            .await?;
    let states =
        read_direct_states(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    let source_counts = read_direct_source_counts(client, selected_car_id_i16).await?;

    let projected_car = project_car(car, latest_firmware(&updates, selected_car_id)?)?;
    let imported_geofences = geofences.clone();
    let address_by_id: HashMap<_, _> = addresses.into_iter().map(|row| (row.id, row)).collect();
    let geofence_by_id: HashMap<_, _> = geofences.into_iter().map(|row| (row.id, row)).collect();
    let projected_states = states
        .iter()
        .map(|state| project_state(state, selected_car_id))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut sink = PackSink::new_with_schema_2_1(
        writer,
        binding,
        snapshot_id,
        sequence,
        projected_states,
        true,
    );
    let mut report = ProjectionReport::default();
    let mut related_positions = HashMap::new();
    prefetch_related_positions(
        client,
        selected_car_id_i16,
        read_limits,
        &drives,
        &processes,
        &mut related_positions,
    )
    .await?;

    let projected_drives = write_drives(
        selected_car_id,
        drives,
        &address_by_id,
        &geofence_by_id,
        &mut related_positions,
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
        &mut related_positions,
        &projected_car,
        fragment_limits,
        &mut sink,
        &mut report,
    )
    .await?;
    validate_direct_source_counts(source_counts, report)?;

    if sink.chunks.is_empty() {
        sink.write(ProjectionSnapshot {
            cars: vec![projected_car.clone()],
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
        })?;
    }
    let fingerprint = sink.fingerprint();
    Ok(StagedProjectionPacks::new(
        sink.into_chunks(),
        report,
        fingerprint,
        imported_geofences,
    ))
}

async fn read_direct_states(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateState>, TeslaMateDirectError> {
    const DIRECT_STATES_SQL: &str = r#"
SELECT
  "id",
  "car_id",
  "state"::text AS "state",
  (EXTRACT(EPOCH FROM "start_date") * 1000)::bigint AS "start_date_ms",
  CASE
    WHEN "end_date" IS NULL THEN NULL
    ELSE (EXTRACT(EPOCH FROM "end_date") * 1000)::bigint
  END AS "end_date_ms"
FROM "public"."states"
WHERE "car_id" = $1 AND "id" > $2
ORDER BY "id" ASC
LIMIT $3
"#;
    let mut last_id = 0_i32;
    let mut states = Vec::new();
    let page_size = i64::from(limits.page_size);
    loop {
        let page = client
            .query(DIRECT_STATES_SQL, &[&selected_car_id, &last_id, &page_size])
            .await?;
        let page_len = page.len();
        for row in page {
            let id: i32 = row.try_get("id")?;
            if id <= last_id {
                return Err(TeslaMateDirectError::NonProgressingPage("states"));
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
            states.push(TeslaMateState {
                id: i64::from(id),
                car_id: i64::from(row.try_get::<_, i16>("car_id")?),
                state: row.try_get("state")?,
                start_date_ms: row.try_get("start_date_ms")?,
                end_date_ms: row.try_get("end_date_ms")?,
            });
        }
        if page_len < limits.page_size as usize {
            return Ok(states);
        }
    }
}

async fn read_direct_source_counts(
    client: &Client,
    selected_car_id: i16,
) -> Result<TeslaMateSourceCounts, TeslaMateDirectError> {
    const DIRECT_SOURCE_COUNT_SQL: &str = r#"
SELECT
  (SELECT COUNT(*)::bigint FROM "public"."cars" WHERE "id" = $1) AS "cars",
  (SELECT COUNT(*)::bigint FROM "public"."drives" WHERE "car_id" = $1) AS "drives",
  (SELECT COUNT(*)::bigint FROM "public"."positions" WHERE "car_id" = $1) AS "positions",
  (SELECT COUNT(*)::bigint FROM "public"."charging_processes" WHERE "car_id" = $1)
    AS "charging_processes",
  (
    SELECT COUNT(*)::bigint
    FROM "public"."charges" AS "charge"
    JOIN "public"."charging_processes" AS "process"
      ON "process"."id" = "charge"."charging_process_id"
    WHERE "process"."car_id" = $1
  ) AS "charges"
"#;
    let row = client
        .query_one(DIRECT_SOURCE_COUNT_SQL, &[&selected_car_id])
        .await?;
    Ok(TeslaMateSourceCounts {
        cars: source_count(&row, "cars")?,
        drives: source_count(&row, "drives")?,
        positions: source_count(&row, "positions")?,
        charging_processes: source_count(&row, "charging_processes")?,
        charges: source_count(&row, "charges")?,
    })
}

fn source_count(
    row: &tokio_postgres::Row,
    column: &'static str,
) -> Result<u64, TeslaMateDirectError> {
    let count: i64 = row.try_get(column)?;
    u64::try_from(count).map_err(|_| TeslaMateDirectError::InvalidSourceCount { column, count })
}

fn validate_direct_source_counts(
    source: TeslaMateSourceCounts,
    report: ProjectionReport,
) -> Result<(), TeslaMateDirectError> {
    validate_direct_count("cars", source.cars, 1)?;
    validate_direct_count(
        "drives",
        source.drives,
        report
            .completed_drives
            .checked_add(report.skipped_open_drives)
            .ok_or(TeslaMateDirectError::CountOverflow { table: "drives" })?,
    )?;
    validate_direct_count(
        "positions",
        source.positions,
        report
            .projected_positions
            .checked_add(report.skipped_unattached_positions)
            .ok_or(TeslaMateDirectError::CountOverflow { table: "positions" })?,
    )?;
    validate_direct_count(
        "charging_processes",
        source.charging_processes,
        report.projected_charges,
    )?;
    validate_direct_count("charges", source.charges, report.projected_charge_samples)
}

fn validate_direct_count(
    table: &'static str,
    source: u64,
    accounted: u64,
) -> Result<(), TeslaMateDirectError> {
    if source == accounted {
        Ok(())
    } else {
        Err(TeslaMateDirectError::UnexplainedSourceRows {
            table,
            source_rows: source,
            accounted,
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_drives(
    selected_car_id: i64,
    drives: Vec<TeslaMateDrive>,
    addresses: &HashMap<i64, TeslaMateAddress>,
    geofences: &HashMap<i64, TeslaMateGeofence>,
    related_positions: &mut HashMap<i64, TeslaMatePosition>,
    car: &crate::hub_pack::ProjectionCar,
    limits: TeslaMateFragmentLimits,
    sink: &mut PackSink<'_>,
    report: &mut ProjectionReport,
) -> Result<HashMap<i64, ProjectionDrive>, TeslaMateDirectError> {
    let mut projected_by_id = HashMap::new();
    let mut accumulator = FragmentAccumulator::new(car.clone(), limits)?;
    for drive in drives {
        let start_position = related_position(drive.start_position_id, related_positions)?;
        let end_position = related_position(drive.end_position_id, related_positions)?;
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
    let mut source_position_rows = 0usize;
    tracing::info!(selected_car_id, "starting TeslaMate position history capture");
    let stream = client
        .copy_out(&binary_copy_sql(
            SourceTable::Positions,
            selected_car_id_i16,
        ))
        .await?;
    tracing::info!(selected_car_id, "TeslaMate position history stream opened");
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        position_copy_types()
    ));
    while let Some(row) = rows.as_mut().try_next().await? {
        source_position_rows = source_position_rows.checked_add(1).ok_or(
            TeslaMateDirectError::MaximumRowsExceeded {
                maximum: read_limits.maximum_rows,
            },
        )?;
        *retained_rows =
            retained_rows
                .checked_add(1)
                .ok_or(TeslaMateDirectError::MaximumRowsExceeded {
                    maximum: read_limits.maximum_rows,
                })?;
        if *retained_rows > read_limits.maximum_rows {
            return Err(TeslaMateDirectError::MaximumRowsExceeded {
                maximum: read_limits.maximum_rows,
            });
        }
        if source_position_rows % 250_000 == 0 {
            tracing::info!(
                selected_car_id,
                source_position_rows,
                projected_positions = report.projected_positions,
                "staging TeslaMate position history"
            );
        }
        let position = decode_binary_position(&row)?;
        let Some(drive_id) = position.drive_id else {
            let projected = crate::lifecycle::imported_position(&position);
            accumulator.prepare(sink, |_| Ok((1, serialized_bytes(&projected)?)))?;
            accumulator.positions.push(projected);
            report.projected_positions = checked_increment(report.projected_positions)?;
            continue;
        };
        let Some(drive) = drives.get(&drive_id) else {
            report.skipped_unattached_positions =
                checked_increment(report.skipped_unattached_positions)?;
            continue;
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
    }
    accumulator.flush(sink)?;
    tracing::info!(
        selected_car_id,
        source_position_rows,
        projected_positions = report.projected_positions,
        "finished TeslaMate position history capture"
    );
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
    related_positions: &mut HashMap<i64, TeslaMatePosition>,
    car: &crate::hub_pack::ProjectionCar,
    limits: TeslaMateFragmentLimits,
    sink: &mut PackSink<'_>,
    report: &mut ProjectionReport,
) -> Result<(), TeslaMateDirectError> {
    let mut facts = HashMap::<i64, ChargeProjectionFacts>::new();
    let mut sample_counts = HashMap::<i64, u64>::new();
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Charges, selected_car_id_i16))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        charge_copy_types()
    ));
    while let Some(row) = rows.as_mut().try_next().await? {
        *retained_rows =
            retained_rows
                .checked_add(1)
                .ok_or(TeslaMateDirectError::MaximumRowsExceeded {
                    maximum: read_limits.maximum_rows,
                })?;
        if *retained_rows > read_limits.maximum_rows {
            return Err(TeslaMateDirectError::MaximumRowsExceeded {
                maximum: read_limits.maximum_rows,
            });
        }
        let sample = decode_binary_charge(&row)?;
        facts
            .entry(sample.charging_process_id)
            .or_default()
            .observe(&sample);
        let count = sample_counts.entry(sample.charging_process_id).or_default();
        *count = checked_increment(*count)?;
    }

    let mut projected_by_id = HashMap::<i64, ProjectionCharge>::new();
    let mut empty = FragmentAccumulator::new(car.clone(), limits)?;
    let empty_charge_facts = ChargeProjectionFacts::default();
    for process in processes {
        let position = related_position(process.position_id, related_positions)?;
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
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Charges, selected_car_id_i16))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        charge_copy_types()
    ));
    while let Some(row) = rows.as_mut().try_next().await? {
        second_pass_rows =
            second_pass_rows
                .checked_add(1)
                .ok_or(TeslaMateDirectError::MaximumRowsExceeded {
                    maximum: read_limits.maximum_rows,
                })?;
        if second_pass_rows > read_limits.maximum_rows {
            return Err(TeslaMateDirectError::MaximumRowsExceeded {
                maximum: read_limits.maximum_rows,
            });
        }
        let sample = decode_binary_charge(&row)?;
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
    }
    samples.flush(sink)?;
    Ok(())
}

const RELATED_POSITION_BATCH_SIZE: usize = 256;
/// Relation rows carry complete position records. This cap bounds the cache to
/// a modest historical set while the normal position stream handles the full
/// telemetry corpus without retaining it in memory.
const MAX_RELATED_POSITION_CACHE_IDS: usize = 100_000;

async fn prefetch_related_positions(
    client: &Client,
    selected_car_id: i16,
    read_limits: TeslaMateReadLimits,
    drives: &[TeslaMateDrive],
    processes: &[TeslaMateChargingProcess],
    cache: &mut HashMap<i64, TeslaMatePosition>,
) -> Result<(), TeslaMateDirectError> {
    let maximum = read_limits.maximum_rows.min(MAX_RELATED_POSITION_CACHE_IDS);
    if cache.len() > maximum {
        return Err(TeslaMateDirectError::RelatedPositionCacheLimitExceeded { maximum });
    }
    let mut ids = BTreeSet::new();
    for id in drives
        .iter()
        .flat_map(|drive| [drive.start_position_id, drive.end_position_id])
        .chain(processes.iter().map(|process| process.position_id))
        .flatten()
    {
        let id_i32 = checked_related_position_id(id)?;
        if !cache.contains_key(&id) {
            ids.insert(id_i32);
            let requested = cache.len().checked_add(ids.len()).ok_or(
                TeslaMateDirectError::RelatedPositionCacheLimitExceeded { maximum },
            )?;
            if requested > maximum {
                return Err(TeslaMateDirectError::RelatedPositionCacheLimitExceeded { maximum });
            }
        }
    }

    let ids = ids.into_iter().collect::<Vec<_>>();
    for batch in ids.chunks(RELATED_POSITION_BATCH_SIZE) {
        let sql = related_positions_binary_copy_sql(selected_car_id, batch);
        let stream = client.copy_out(&sql).await?;
        let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
            stream,
            position_copy_types()
        ));
        while let Some(row) = rows.as_mut().try_next().await? {
            let position = decode_binary_position(&row)?;
            if cache.contains_key(&position.id) {
                continue;
            }
            cache.insert(position.id, position);
        }
    }
    Ok(())
}

fn checked_related_position_id(id: i64) -> Result<i32, TeslaMateDirectError> {
    let id = i32::try_from(id).map_err(|_| TeslaMateDirectError::InvalidRelatedPosition(id))?;
    if id == i32::MIN {
        return Err(TeslaMateDirectError::InvalidRelatedPosition(i64::from(id)));
    }
    Ok(id)
}

fn related_position(
    id: Option<i64>,
    cache: &mut HashMap<i64, TeslaMatePosition>,
) -> Result<Option<TeslaMatePosition>, TeslaMateDirectError> {
    let Some(id) = id else {
        return Ok(None);
    };
    checked_related_position_id(id)?;
    cache
        .get(&id)
        .cloned()
        .map(Some)
        .ok_or(TeslaMateDirectError::MissingRelatedPosition(id))
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
    #[error("TeslaMate related-position cache exceeds its {maximum} unique-position limit")]
    RelatedPositionCacheLimitExceeded { maximum: usize },
    #[error("TeslaMate charge samples reference missing process {process_id}")]
    MissingChargingProcess { process_id: i64 },
    #[error("TeslaMate direct source count {column} is invalid: {count}")]
    InvalidSourceCount { column: &'static str, count: i64 },
    #[error("TeslaMate direct source {table} count overflowed while accounting for rows")]
    CountOverflow { table: &'static str },
    #[error(
        "TeslaMate direct source {table} has {source_rows} rows but only {accounted} are accounted for"
    )]
    UnexplainedSourceRows {
        table: &'static str,
        source_rows: u64,
        accounted: u64,
    },
    #[error("TeslaMate source database size is invalid: {bytes}")]
    InvalidSourceDatabaseSize { bytes: i64 },
    #[error("could not inspect target free space at {path}: {source}")]
    TargetFilesystemSpace {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("target free-space calculation overflowed")]
    TargetCapacityOverflow,
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
    Pack(#[from] crate::hub_pack::ProjectionPackError),
    #[error(transparent)]
    Fragment(#[from] TeslaMateFragmentError),
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        credentials::{CredentialDirectory, TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL},
        teslamate::ReadOnlySource,
    };

    #[test]
    fn preflight_report_is_bounded_and_redacted() {
        let report = TeslaMatePreflightReport {
            selected_car_id: 17,
            source_database_bytes: 123,
            schema: TeslaMateSchemaInfo {
                observed_migration_version: 20260411070212,
                minimum_supported_migration_version: 20260411070212,
                maximum_validated_migration_version: 20260718160000,
                fingerprint: "abc".to_owned(),
            },
            source_row_counts: TeslaMateSourceCounts {
                cars: 1,
                drives: 2,
                positions: 3,
                charging_processes: 4,
                charges: 5,
            },
            target_available_bytes: 456,
            configured_maximum_rows: 20_000_000,
            configured_staging_limit_bytes: 4 * 1024 * 1024 * 1024,
            configured_staging_reserve_bytes: 512 * 1024 * 1024,
            admission: TeslaMatePreflightAdmission {
                passed: true,
                reason: None,
            },
        };
        let value = serde_json::to_value(report).expect("preflight JSON");
        assert_eq!(value["selectedCarId"], 17);
        assert_eq!(value["sourceRowCounts"]["positions"], 3);
        assert_eq!(value["admission"]["passed"], true);
        assert!(value.get("password").is_none());
        assert!(value.get("sourceUrl").is_none());
        assert!(value.get("snapshotId").is_none());
    }

    #[test]
    fn direct_count_gate_accepts_named_projection_and_skip_reasons() {
        let report = ProjectionReport {
            completed_drives: 1,
            skipped_open_drives: 2,
            skipped_unattached_positions: 3,
            projected_positions: 4,
            projected_charges: 5,
            projected_charge_samples: 6,
        };
        assert!(
            validate_direct_source_counts(
                TeslaMateSourceCounts {
                    cars: 1,
                    drives: 3,
                    positions: 7,
                    charging_processes: 5,
                    charges: 6,
                },
                report,
            )
            .is_ok()
        );
    }

    #[test]
    fn direct_count_gate_rejects_unexplained_loss() {
        let error = validate_direct_source_counts(
            TeslaMateSourceCounts {
                cars: 1,
                drives: 0,
                positions: 1,
                charging_processes: 0,
                charges: 0,
            },
            ProjectionReport::default(),
        )
        .expect_err("position must be accounted for");
        assert!(matches!(
            error,
            TeslaMateDirectError::UnexplainedSourceRows {
                table: "positions",
                source_rows: 1,
                accounted: 0,
            }
        ));
    }

    #[test]

    #[tokio::test]
    async fn native_complete_corpus_direct_import_projects_every_kind_when_configured() {
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
        let packs = tempfile::tempdir().expect("pack directory");
        let result = write_direct_full_snapshot(
            &source,
            &password,
            1,
            TeslaMateReadLimits {
                maximum_rows: 32,
                parallel_copy_lanes: 3,
                ..TeslaMateReadLimits::default()
            },
            &ProjectionPackWriter::new(packs.path()),
            ProjectionBinding {
                installation_id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                vehicle_id: Uuid::new_v4(),
                generation: 1,
                selected_car_id: 1,
            },
            Uuid::new_v4(),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
        )
        .await
        .expect("complete direct import");

        assert!(!result.chunks.is_empty());
        assert_eq!(result.report.completed_drives, 1);
        assert_eq!(result.report.projected_positions, 2);
        assert_eq!(result.report.skipped_unattached_positions, 1);
        assert_eq!(result.report.projected_charges, 1);
        assert_eq!(result.report.projected_charge_samples, 1);
    }

    #[tokio::test]
    async fn native_ten_million_corpus_direct_import_meets_target_when_enabled() {
        if std::env::var("TESLATLAS_HUB_RUN_10M").as_deref() != Ok("1") {
            return;
        }
        let url = std::env::var("TESLATLAS_HUB_TEST_POSTGRES_URL").expect("10m test source URL");
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
        let packs = tempfile::tempdir().expect("pack directory");
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(600),
            write_direct_full_snapshot(
                &source,
                &password,
                1,
                TeslaMateReadLimits {
                    maximum_rows: 10_000_100,
                    parallel_copy_lanes: 3,
                    ..TeslaMateReadLimits::default()
                },
                &ProjectionPackWriter::new(packs.path()),
                ProjectionBinding {
                    installation_id: Uuid::new_v4(),
                    account_id: Uuid::new_v4(),
                    vehicle_id: Uuid::new_v4(),
                    generation: 1,
                    selected_car_id: 1,
                },
                Uuid::new_v4(),
                SequenceRange {
                    from_exclusive: 0,
                    to_inclusive: 1,
                },
            ),
        )
        .await
        .expect("ten-million direct import timed out")
        .expect("ten-million direct import");

        assert!(started.elapsed() < Duration::from_secs(600));
        assert_eq!(result.report.completed_drives, 1);
        assert_eq!(result.report.projected_positions, 10_000_002);
        assert_eq!(result.report.skipped_unattached_positions, 1);
        assert_eq!(result.report.projected_charges, 1);
        assert_eq!(result.report.projected_charge_samples, 1);
    }
}
