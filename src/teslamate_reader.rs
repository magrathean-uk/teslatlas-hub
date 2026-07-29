//! Read-only, bounded TeslaMate PostgreSQL history reader.
//!
//! This is deliberately a source adapter, not a TeslaMate clone. It accepts a
//! credential-free endpoint and a systemd-provided password, opens one TLS
//! connection, pins the source to a repeatable-read transaction, checks the
//! reviewed schema, then fetches only fixed projections with keyset pages.

use std::{path::Path, time::Duration};

use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use time::PrimitiveDateTime;
use tokio::time::timeout;
use tokio_postgres::{Client, Config, Row, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::{
    credentials::TeslaMatePostgresPassword,
    teslamate::ReadOnlySource,
    teslamate_projection::{
        TeslaMateAddress, TeslaMateCar, TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive,
        TeslaMateGeofence, TeslaMateHistory, TeslaMatePosition, TeslaMateUpdate,
    },
    teslamate_schema::{
        MIGRATION_VERSION_SQL, SCHEMA_PROBE_SQL, SourceTable, projection,
        validate_migration_version, validate_observed_schema,
    },
    teslamate_stage::{
        TeslaMateStage, TeslaMateStageError, TeslaMateStageLimits, TeslaMateStageTable,
    },
};

/// Resource caps for one read-only source snapshot. The bounds are checked
/// before a row is retained, so a hostile or unexpectedly huge selected-car
/// history cannot grow an import without limit. Every source query is scoped
/// to that selected car before pagination. The staged producer retains the
/// same query and validation contract without materialising this bounded
/// capture as one history vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeslaMateReadLimits {
    pub connect_timeout: Duration,
    pub page_size: i32,
    pub maximum_rows: usize,
    pub maximum_stage_bytes: u64,
    pub minimum_free_bytes: u64,
}

impl Default for TeslaMateReadLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            page_size: 2_000,
            maximum_rows: 1_000_000,
            maximum_stage_bytes: 4 * 1024 * 1024 * 1024,
            minimum_free_bytes: TeslaMateStageLimits::default().minimum_free_bytes,
        }
    }
}

impl TeslaMateReadLimits {
    pub fn validate(self) -> Result<(), TeslaMateReaderError> {
        if self.connect_timeout.is_zero() {
            return Err(TeslaMateReaderError::InvalidConnectTimeout);
        }
        if !(1..=10_000).contains(&self.page_size) {
            return Err(TeslaMateReaderError::InvalidPageSize);
        }
        if self.maximum_rows == 0 {
            return Err(TeslaMateReaderError::InvalidMaximumRows);
        }
        TeslaMateStageLimits {
            max_rows: u64::try_from(self.maximum_rows).expect("usize fits u64"),
            max_stage_bytes: self.maximum_stage_bytes,
            minimum_free_bytes: self.minimum_free_bytes,
        }
        .validate()
        .map_err(TeslaMateReaderError::Stage)?;
        Ok(())
    }
}

/// Read every fixed history projection inside one repeatable-read, read-only
/// transaction. It neither writes to PostgreSQL nor receives a source URL
/// containing credentials.
pub async fn read_history(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateHistory, TeslaMateReaderError> {
    limits.validate()?;
    if selected_car_id <= 0 {
        return Err(TeslaMateReaderError::InvalidSelectedCarId);
    }
    crate::crypto::install_default_provider();
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let user = source
        .user()
        .ok_or(TeslaMateReaderError::SourceUserRequired)?;
    let (tls, certificate_errors) = MakeRustlsConnect::with_native_certs()
        .map_err(|_| TeslaMateReaderError::NativeTrustStoreUnavailable)?;
    if !certificate_errors.is_empty() {
        tracing::warn!(
            count = certificate_errors.len(),
            "some native TLS certificates could not be loaded"
        );
    }

    let mut configuration = Config::new();
    configuration
        .host(source.host())
        .port(source.port())
        .user(user)
        .password(password.as_str())
        .dbname(source.database_name())
        .ssl_mode(SslMode::Require);
    let (client, connection) = timeout(limits.connect_timeout, configuration.connect(tls))
        .await
        .map_err(|_| TeslaMateReaderError::ConnectTimedOut)??;
    let connection_task = tokio::spawn(async move {
        // Query operations surface their own errors to the caller. This task
        // only keeps the protocol I/O alive and must never render credentials.
        let _ = connection.await;
    });

    let result = read_history_in_session(&client, source, selected_car_id, limits).await;
    let rollback = client.batch_execute("ROLLBACK").await;
    drop(client);
    let _ = connection_task.await;
    let history = result?;
    rollback?;
    Ok(history)
}

/// Capture a source-consistent TeslaMate snapshot into one private local
/// SQLite stage. PostgreSQL rows are decoded and committed page-by-page; no
/// complete history vector exists while the source transaction is open.
///
/// An interrupted capture is explicitly discarded. PostgreSQL repeatable-read
/// snapshots cannot be safely resumed after a reconnect, so only a sealed
/// stage may move on to later pack production.
pub async fn capture_history_to_stage(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
    imports_dir: &Path,
) -> Result<TeslaMateStage, TeslaMateReaderError> {
    limits.validate()?;
    if selected_car_id <= 0 {
        return Err(TeslaMateReaderError::InvalidSelectedCarId);
    }
    crate::crypto::install_default_provider();
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let user = source
        .user()
        .ok_or(TeslaMateReaderError::SourceUserRequired)?;
    let (tls, certificate_errors) = MakeRustlsConnect::with_native_certs()
        .map_err(|_| TeslaMateReaderError::NativeTrustStoreUnavailable)?;
    if !certificate_errors.is_empty() {
        tracing::warn!(
            count = certificate_errors.len(),
            "some native TLS certificates could not be loaded"
        );
    }
    let mut stage = TeslaMateStage::create(
        imports_dir,
        TeslaMateStageLimits {
            max_rows: u64::try_from(limits.maximum_rows).expect("usize fits u64"),
            max_stage_bytes: limits.maximum_stage_bytes,
            minimum_free_bytes: limits.minimum_free_bytes,
        },
    )?;

    let mut configuration = Config::new();
    configuration
        .host(source.host())
        .port(source.port())
        .user(user)
        .password(password.as_str())
        .dbname(source.database_name())
        .ssl_mode(SslMode::Require);
    let (client, connection) =
        match timeout(limits.connect_timeout, configuration.connect(tls)).await {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                let _ = stage.discard();
                return Err(TeslaMateReaderError::Postgres(error));
            }
            Err(_) => {
                let _ = stage.discard();
                return Err(TeslaMateReaderError::ConnectTimedOut);
            }
        };
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });

    let capture =
        capture_history_in_session(&client, source, selected_car_id, limits, &mut stage).await;
    let rollback = client.batch_execute("ROLLBACK").await;
    drop(client);
    let _ = connection_task.await;
    if let Err(error) = capture {
        let _ = stage.discard();
        return Err(error);
    }
    if let Err(error) = rollback {
        let _ = stage.discard();
        return Err(TeslaMateReaderError::Postgres(error));
    }
    if let Err(error) = stage.seal() {
        let _ = stage.discard();
        return Err(TeslaMateReaderError::Stage(error));
    }
    Ok(stage)
}

/// Materialize a sealed capture for isolated compatibility tests only. Normal
/// import publication uses the staged fragment producer and never calls this
/// all-memory helper.
pub fn materialize_small_staged_history(
    stage: &TeslaMateStage,
    maximum_rows: usize,
) -> Result<TeslaMateHistory, TeslaMateReaderError> {
    let stats = stage.stats()?;
    if stats.row_count > u64::try_from(maximum_rows).expect("usize fits u64") {
        return Err(TeslaMateReaderError::MaximumRowsExceeded {
            maximum: maximum_rows,
        });
    }
    Ok(TeslaMateHistory {
        cars: collect_staged_rows(stage, TeslaMateStageTable::Cars)?,
        drives: collect_staged_rows(stage, TeslaMateStageTable::Drives)?,
        positions: collect_staged_rows(stage, TeslaMateStageTable::Positions)?,
        charging_processes: collect_staged_rows(stage, TeslaMateStageTable::ChargingProcesses)?,
        charges: collect_staged_rows(stage, TeslaMateStageTable::Charges)?,
        addresses: collect_staged_rows(stage, TeslaMateStageTable::Addresses)?,
        geofences: collect_staged_rows(stage, TeslaMateStageTable::Geofences)?,
        updates: collect_staged_rows(stage, TeslaMateStageTable::Updates)?,
    })
}

fn collect_staged_rows<T: DeserializeOwned>(
    stage: &TeslaMateStage,
    table: TeslaMateStageTable,
) -> Result<Vec<T>, TeslaMateReaderError> {
    let mut after_id = 0_i64;
    let mut output = Vec::new();
    loop {
        let page = stage.page(table, after_id, 10_000)?;
        output.extend(page.rows.into_iter().map(|row| row.value));
        match page.next_after_id {
            Some(next_after_id) => after_id = next_after_id,
            None => return Ok(output),
        }
    }
}

async fn capture_history_in_session(
    client: &Client,
    source: &ReadOnlySource,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    prepare_read_only_snapshot(client, source).await?;

    let mut retained_rows = 0_usize;
    let cars = capture_smallint_pages(
        client,
        StageProjection {
            source_table: SourceTable::Cars,
            stage_table: TeslaMateStageTable::Cars,
            decode: decode_car,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    if cars == 0 {
        return Err(TeslaMateReaderError::SelectedCarMissing {
            selected_car_id: i64::from(selected_car_id),
        });
    }
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Drives,
            stage_table: TeslaMateStageTable::Drives,
            decode: decode_drive,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Positions,
            stage_table: TeslaMateStageTable::Positions,
            decode: decode_position,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::ChargingProcesses,
            stage_table: TeslaMateStageTable::ChargingProcesses,
            decode: decode_charging_process,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Charges,
            stage_table: TeslaMateStageTable::Charges,
            decode: decode_charge,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Addresses,
            stage_table: TeslaMateStageTable::Addresses,
            decode: decode_address,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Geofences,
            stage_table: TeslaMateStageTable::Geofences,
            decode: decode_geofence,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Updates,
            stage_table: TeslaMateStageTable::Updates,
            decode: decode_update,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    Ok(())
}

struct StageProjection<T> {
    source_table: SourceTable,
    stage_table: TeslaMateStageTable,
    decode: fn(&Row) -> Result<T, TeslaMateReaderError>,
}

async fn capture_smallint_pages<T: Serialize>(
    client: &Client,
    projection_descriptor: StageProjection<T>,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<usize, TeslaMateReaderError> {
    let mut last_id = 0_i16;
    let mut captured_rows = 0_usize;
    loop {
        let page = client
            .query(
                projection(projection_descriptor.source_table).sql,
                &[&last_id, &limits.page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in page {
            let id = required_i16(&row, projection_descriptor.source_table.name(), "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage {
                    table: projection_descriptor.source_table.name(),
                });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            decoded.push((i64::from(id), (projection_descriptor.decode)(&row)?));
        }
        captured_rows = captured_rows.checked_add(page_len).ok_or(
            TeslaMateReaderError::MaximumRowsExceeded {
                maximum: limits.maximum_rows,
            },
        )?;
        stage.insert_page(projection_descriptor.stage_table, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(captured_rows);
        }
    }
}

async fn capture_integer_pages<T: Serialize>(
    client: &Client,
    projection_descriptor: StageProjection<T>,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<usize, TeslaMateReaderError> {
    let mut last_id = 0_i32;
    let mut captured_rows = 0_usize;
    loop {
        let page = client
            .query(
                projection(projection_descriptor.source_table).sql,
                &[&last_id, &limits.page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in page {
            let id = required_i32(&row, projection_descriptor.source_table.name(), "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage {
                    table: projection_descriptor.source_table.name(),
                });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            decoded.push((i64::from(id), (projection_descriptor.decode)(&row)?));
        }
        captured_rows = captured_rows.checked_add(page_len).ok_or(
            TeslaMateReaderError::MaximumRowsExceeded {
                maximum: limits.maximum_rows,
            },
        )?;
        stage.insert_page(projection_descriptor.stage_table, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(captured_rows);
        }
    }
}

async fn read_history_in_session(
    client: &Client,
    source: &ReadOnlySource,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateHistory, TeslaMateReaderError> {
    prepare_read_only_snapshot(client, source).await?;

    let mut retained_rows = 0_usize;
    let cars = read_cars(client, selected_car_id, limits, &mut retained_rows).await?;
    if cars.is_empty() {
        return Err(TeslaMateReaderError::SelectedCarMissing {
            selected_car_id: i64::from(selected_car_id),
        });
    }
    let drives = read_drives(client, selected_car_id, limits, &mut retained_rows).await?;
    let positions = read_positions(client, selected_car_id, limits, &mut retained_rows).await?;
    let charging_processes =
        read_charging_processes(client, selected_car_id, limits, &mut retained_rows).await?;
    let charges = read_charges(client, selected_car_id, limits, &mut retained_rows).await?;
    let addresses = read_addresses(client, selected_car_id, limits, &mut retained_rows).await?;
    let geofences = read_geofences(client, selected_car_id, limits, &mut retained_rows).await?;
    let updates = read_updates(client, selected_car_id, limits, &mut retained_rows).await?;

    Ok(TeslaMateHistory {
        cars,
        drives,
        positions,
        charging_processes,
        charges,
        addresses,
        geofences,
        updates,
    })
}

async fn prepare_read_only_snapshot(
    client: &Client,
    source: &ReadOnlySource,
) -> Result<(), TeslaMateReaderError> {
    for statement in source.session_sql() {
        client.batch_execute(statement).await?;
    }

    let migration = client
        .query_one(MIGRATION_VERSION_SQL, &[])
        .await?
        .try_get::<_, Option<i64>>("version")?
        .ok_or(TeslaMateReaderError::MissingMigrationVersion)?;
    validate_migration_version(migration)?;

    let rows = client.query(SCHEMA_PROBE_SQL, &[]).await?;
    let observed = rows
        .iter()
        .map(|row| {
            Ok(crate::teslamate_schema::ObservedColumn {
                table: row.try_get("table_name")?,
                name: row.try_get("column_name")?,
                type_name: row.try_get("type_name")?,
                nullable: row.try_get("is_nullable")?,
            })
        })
        .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
    validate_observed_schema(&observed)?;
    Ok(())
}

async fn read_cars(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateCar>, TeslaMateReaderError> {
    let rows = read_smallint_pages(
        client,
        SourceTable::Cars,
        selected_car_id,
        limits,
        retained_rows,
    )
    .await?;
    rows.iter().map(decode_car).collect()
}

async fn read_drives(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateDrive>, TeslaMateReaderError> {
    let rows = read_integer_pages(
        client,
        SourceTable::Drives,
        selected_car_id,
        limits,
        retained_rows,
    )
    .await?;
    rows.iter().map(decode_drive).collect()
}

async fn read_positions(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMatePosition>, TeslaMateReaderError> {
    let rows = read_integer_pages(
        client,
        SourceTable::Positions,
        selected_car_id,
        limits,
        retained_rows,
    )
    .await?;
    rows.iter().map(decode_position).collect()
}

async fn read_charging_processes(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateChargingProcess>, TeslaMateReaderError> {
    let rows = read_integer_pages(
        client,
        SourceTable::ChargingProcesses,
        selected_car_id,
        limits,
        retained_rows,
    )
    .await?;
    rows.iter().map(decode_charging_process).collect()
}

async fn read_charges(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateCharge>, TeslaMateReaderError> {
    let rows = read_integer_pages(
        client,
        SourceTable::Charges,
        selected_car_id,
        limits,
        retained_rows,
    )
    .await?;
    rows.iter().map(decode_charge).collect()
}

async fn read_addresses(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateAddress>, TeslaMateReaderError> {
    let rows = read_integer_pages(
        client,
        SourceTable::Addresses,
        selected_car_id,
        limits,
        retained_rows,
    )
    .await?;
    rows.iter().map(decode_address).collect()
}

async fn read_geofences(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateGeofence>, TeslaMateReaderError> {
    let rows = read_integer_pages(
        client,
        SourceTable::Geofences,
        selected_car_id,
        limits,
        retained_rows,
    )
    .await?;
    rows.iter().map(decode_geofence).collect()
}

async fn read_updates(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateUpdate>, TeslaMateReaderError> {
    let rows = read_integer_pages(
        client,
        SourceTable::Updates,
        selected_car_id,
        limits,
        retained_rows,
    )
    .await?;
    rows.iter().map(decode_update).collect()
}

async fn read_smallint_pages(
    client: &Client,
    table: SourceTable,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<Row>, TeslaMateReaderError> {
    let mut last_id = 0_i16;
    let mut result = Vec::new();
    loop {
        let page = client
            .query(
                projection(table).sql,
                &[&last_id, &limits.page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        for row in page {
            let id = required_i16(&row, table.name(), "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage {
                    table: table.name(),
                });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            result.push(row);
        }
        if page_len < limits.page_size as usize {
            return Ok(result);
        }
    }
}

async fn read_integer_pages(
    client: &Client,
    table: SourceTable,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<Row>, TeslaMateReaderError> {
    let mut last_id = 0_i32;
    let mut result = Vec::new();
    loop {
        let page = client
            .query(
                projection(table).sql,
                &[&last_id, &limits.page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        for row in page {
            let id = required_i32(&row, table.name(), "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage {
                    table: table.name(),
                });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            result.push(row);
        }
        if page_len < limits.page_size as usize {
            return Ok(result);
        }
    }
}

fn retain_row(total: &mut usize, maximum: usize) -> Result<(), TeslaMateReaderError> {
    *total = total
        .checked_add(1)
        .ok_or(TeslaMateReaderError::MaximumRowsExceeded { maximum })?;
    if *total > maximum {
        return Err(TeslaMateReaderError::MaximumRowsExceeded { maximum });
    }
    Ok(())
}

fn decode_car(row: &Row) -> Result<TeslaMateCar, TeslaMateReaderError> {
    Ok(TeslaMateCar {
        id: i64::from(required_i16(row, "cars", "id")?),
        eid: required_i64(row, "cars", "eid")?,
        vin: optional_text(row, "cars", "vin")?,
        name: optional_text(row, "cars", "name")?,
        model: optional_text(row, "cars", "model")?,
        trim_badging: optional_text(row, "cars", "trim_badging")?,
        marketing_name: optional_text(row, "cars", "marketing_name")?,
        efficiency_wh_per_km: optional_float(row, "cars", "efficiency")?,
    })
}

fn decode_drive(row: &Row) -> Result<TeslaMateDrive, TeslaMateReaderError> {
    Ok(TeslaMateDrive {
        id: i64::from(required_i32(row, "drives", "id")?),
        car_id: i64::from(required_i16(row, "drives", "car_id")?),
        start_date_ms: required_timestamp_ms(row, "drives", "start_date")?,
        end_date_ms: optional_timestamp_ms(row, "drives", "end_date")?,
        start_position_id: optional_i32(row, "drives", "start_position_id")?.map(i64::from),
        end_position_id: optional_i32(row, "drives", "end_position_id")?.map(i64::from),
        start_address_id: optional_i32(row, "drives", "start_address_id")?.map(i64::from),
        end_address_id: optional_i32(row, "drives", "end_address_id")?.map(i64::from),
        start_geofence_id: optional_i32(row, "drives", "start_geofence_id")?.map(i64::from),
        end_geofence_id: optional_i32(row, "drives", "end_geofence_id")?.map(i64::from),
        outside_temp_avg: optional_decimal(row, "drives", "outside_temp_avg")?,
        speed_max: optional_i16(row, "drives", "speed_max")?.map(i64::from),
        start_rated_range_km: optional_decimal(row, "drives", "start_rated_range_km")?,
        end_rated_range_km: optional_decimal(row, "drives", "end_rated_range_km")?,
        start_km: optional_float(row, "drives", "start_km")?,
        end_km: optional_float(row, "drives", "end_km")?,
        distance_km: optional_float(row, "drives", "distance")?,
        duration_min: optional_i16(row, "drives", "duration_min")?.map(i64::from),
    })
}

fn decode_position(row: &Row) -> Result<TeslaMatePosition, TeslaMateReaderError> {
    Ok(TeslaMatePosition {
        id: i64::from(required_i32(row, "positions", "id")?),
        car_id: i64::from(required_i16(row, "positions", "car_id")?),
        drive_id: optional_i32(row, "positions", "drive_id")?.map(i64::from),
        date_ms: required_timestamp_ms(row, "positions", "date")?,
        latitude: required_decimal(row, "positions", "latitude")?,
        longitude: required_decimal(row, "positions", "longitude")?,
        elevation: optional_i16(row, "positions", "elevation")?.map(i64::from),
        speed: optional_i16(row, "positions", "speed")?.map(i64::from),
        power: optional_i16(row, "positions", "power")?.map(i64::from),
        odometer: optional_float(row, "positions", "odometer")?,
        ideal_battery_range_km: optional_decimal(row, "positions", "ideal_battery_range_km")?,
        rated_battery_range_km: optional_decimal(row, "positions", "rated_battery_range_km")?,
        battery_level: optional_i16(row, "positions", "battery_level")?.map(i64::from),
        usable_battery_level: optional_i16(row, "positions", "usable_battery_level")?
            .map(i64::from),
        is_climate_on: optional_bool(row, "positions", "is_climate_on")?,
        outside_temp: optional_decimal(row, "positions", "outside_temp")?,
        inside_temp: optional_decimal(row, "positions", "inside_temp")?,
    })
}

fn decode_charging_process(row: &Row) -> Result<TeslaMateChargingProcess, TeslaMateReaderError> {
    Ok(TeslaMateChargingProcess {
        id: i64::from(required_i32(row, "charging_processes", "id")?),
        car_id: i64::from(required_i16(row, "charging_processes", "car_id")?),
        position_id: optional_i32(row, "charging_processes", "position_id")?.map(i64::from),
        address_id: optional_i32(row, "charging_processes", "address_id")?.map(i64::from),
        geofence_id: optional_i32(row, "charging_processes", "geofence_id")?.map(i64::from),
        start_date_ms: required_timestamp_ms(row, "charging_processes", "start_date")?,
        end_date_ms: optional_timestamp_ms(row, "charging_processes", "end_date")?,
        charge_energy_added: optional_decimal(row, "charging_processes", "charge_energy_added")?,
        start_battery_level: optional_i16(row, "charging_processes", "start_battery_level")?
            .map(i64::from),
        end_battery_level: optional_i16(row, "charging_processes", "end_battery_level")?
            .map(i64::from),
        duration_min: optional_i16(row, "charging_processes", "duration_min")?.map(i64::from),
        outside_temp_avg: optional_decimal(row, "charging_processes", "outside_temp_avg")?,
        start_rated_range_km: optional_decimal(row, "charging_processes", "start_rated_range_km")?,
        end_rated_range_km: optional_decimal(row, "charging_processes", "end_rated_range_km")?,
    })
}

fn decode_charge(row: &Row) -> Result<TeslaMateCharge, TeslaMateReaderError> {
    Ok(TeslaMateCharge {
        id: i64::from(required_i32(row, "charges", "id")?),
        charging_process_id: i64::from(required_i32(row, "charges", "charging_process_id")?),
        date_ms: required_timestamp_ms(row, "charges", "date")?,
        battery_heater: optional_bool(row, "charges", "battery_heater")?,
        battery_heater_on: optional_bool(row, "charges", "battery_heater_on")?,
        battery_heater_no_power: optional_bool(row, "charges", "battery_heater_no_power")?,
        battery_level: optional_i16(row, "charges", "battery_level")?.map(i64::from),
        usable_battery_level: optional_i16(row, "charges", "usable_battery_level")?.map(i64::from),
        charge_energy_added_kwh: optional_decimal(row, "charges", "charge_energy_added")?,
        charger_actual_current: optional_i16(row, "charges", "charger_actual_current")?
            .map(f64::from),
        charger_phases: optional_i16(row, "charges", "charger_phases")?.map(i64::from),
        charger_pilot_current: optional_i16(row, "charges", "charger_pilot_current")?
            .map(f64::from),
        charger_power_kw: optional_i16(row, "charges", "charger_power")?.map(f64::from),
        charger_voltage: optional_i16(row, "charges", "charger_voltage")?.map(f64::from),
        charge_cable: optional_text(row, "charges", "conn_charge_cable")?,
        fast_charger_present: optional_bool(row, "charges", "fast_charger_present")?,
        fast_charger_brand: optional_text(row, "charges", "fast_charger_brand")?,
        fast_charger_type: optional_text(row, "charges", "fast_charger_type")?,
        ideal_range_km: optional_decimal(row, "charges", "ideal_battery_range_km")?,
        rated_range_km: optional_decimal(row, "charges", "rated_battery_range_km")?,
        not_enough_power_to_heat: optional_bool(row, "charges", "not_enough_power_to_heat")?,
        outside_temp_c: optional_decimal(row, "charges", "outside_temp")?,
    })
}

fn decode_address(row: &Row) -> Result<TeslaMateAddress, TeslaMateReaderError> {
    Ok(TeslaMateAddress {
        id: i64::from(required_i32(row, "addresses", "id")?),
        display_name: optional_text(row, "addresses", "display_name")?,
        name: optional_text(row, "addresses", "name")?,
    })
}

fn decode_geofence(row: &Row) -> Result<TeslaMateGeofence, TeslaMateReaderError> {
    Ok(TeslaMateGeofence {
        id: i64::from(required_i32(row, "geofences", "id")?),
        name: required_text(row, "geofences", "name")?,
    })
}

fn decode_update(row: &Row) -> Result<TeslaMateUpdate, TeslaMateReaderError> {
    Ok(TeslaMateUpdate {
        id: i64::from(required_i32(row, "updates", "id")?),
        car_id: i64::from(required_i16(row, "updates", "car_id")?),
        start_date_ms: required_timestamp_ms(row, "updates", "start_date")?,
        end_date_ms: optional_timestamp_ms(row, "updates", "end_date")?,
        version: optional_text(row, "updates", "version")?,
    })
}

fn required_i16(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i16, TeslaMateReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_i16(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i16>, TeslaMateReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn required_i32(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i32, TeslaMateReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_i32(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i32>, TeslaMateReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn required_i64(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i64, TeslaMateReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_bool(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<bool>, TeslaMateReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_text(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<String>, TeslaMateReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn required_text(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<String, TeslaMateReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_float(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<f64>, TeslaMateReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn required_decimal(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<f64, TeslaMateReaderError> {
    let value: Decimal = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    value
        .to_f64()
        .ok_or(TeslaMateReaderError::DecimalOutOfRange { table, column })
}

fn optional_decimal(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<f64>, TeslaMateReaderError> {
    let value: Option<Decimal> = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    value
        .map(|value| {
            value
                .to_f64()
                .ok_or(TeslaMateReaderError::DecimalOutOfRange { table, column })
        })
        .transpose()
}

fn required_timestamp_ms(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i64, TeslaMateReaderError> {
    let value: PrimitiveDateTime = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    timestamp_ms(value, table, column)
}

fn optional_timestamp_ms(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i64>, TeslaMateReaderError> {
    let value: Option<PrimitiveDateTime> = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    value
        .map(|value| timestamp_ms(value, table, column))
        .transpose()
}

fn timestamp_ms(
    value: PrimitiveDateTime,
    table: &'static str,
    column: &'static str,
) -> Result<i64, TeslaMateReaderError> {
    (value.assume_utc().unix_timestamp_nanos() / 1_000_000)
        .try_into()
        .map_err(|_| TeslaMateReaderError::TimestampOutOfRange { table, column })
}

fn selected_source_car_id(selected_car_id: i64) -> Result<i16, TeslaMateReaderError> {
    i16::try_from(selected_car_id).map_err(|_| TeslaMateReaderError::SelectedCarIdOutOfRange)
}

fn cell(
    table: &'static str,
    column: &'static str,
    source: tokio_postgres::Error,
) -> TeslaMateReaderError {
    TeslaMateReaderError::Cell {
        table,
        column,
        source,
    }
}

#[derive(Debug, Error)]
pub enum TeslaMateReaderError {
    #[error("TeslaMate source user is required")]
    SourceUserRequired,
    #[error("TeslaMate selected car id must be positive")]
    InvalidSelectedCarId,
    #[error("TeslaMate selected car id exceeds the source smallint domain")]
    SelectedCarIdOutOfRange,
    #[error("TeslaMate selected car {selected_car_id} does not exist in the source")]
    SelectedCarMissing { selected_car_id: i64 },
    #[error("TeslaMate PostgreSQL connect timeout must be greater than zero")]
    InvalidConnectTimeout,
    #[error("TeslaMate PostgreSQL page size must be in 1..=10000")]
    InvalidPageSize,
    #[error("TeslaMate PostgreSQL maximum rows must be greater than zero")]
    InvalidMaximumRows,
    #[error("could not load a usable native TLS trust store")]
    NativeTrustStoreUnavailable,
    #[error("TeslaMate PostgreSQL connection timed out")]
    ConnectTimedOut,
    #[error("TeslaMate schema has no migration version")]
    MissingMigrationVersion,
    #[error("TeslaMate {table} page did not advance its keyset cursor")]
    NonProgressingPage { table: &'static str },
    #[error("TeslaMate source exceeds the {maximum} row import limit")]
    MaximumRowsExceeded { maximum: usize },
    #[error("TeslaMate {table}.{column} decimal cannot be represented as a finite f64")]
    DecimalOutOfRange {
        table: &'static str,
        column: &'static str,
    },
    #[error("TeslaMate {table}.{column} timestamp cannot be represented as epoch milliseconds")]
    TimestampOutOfRange {
        table: &'static str,
        column: &'static str,
    },
    #[error("TeslaMate {table}.{column} could not be decoded")]
    Cell {
        table: &'static str,
        column: &'static str,
        #[source]
        source: tokio_postgres::Error,
    },
    #[error(transparent)]
    Schema(#[from] crate::teslamate_schema::SchemaCompatibilityError),
    #[error(transparent)]
    Stage(#[from] TeslaMateStageError),
    #[error(transparent)]
    Postgres(#[from] tokio_postgres::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        teslamate_projection::TeslaMateCar,
        teslamate_stage::{TeslaMateStageLimits, TeslaMateStageTable},
    };

    #[test]
    fn import_limits_reject_unbounded_or_oversized_pages() {
        assert!(matches!(
            TeslaMateReadLimits {
                page_size: 0,
                ..TeslaMateReadLimits::default()
            }
            .validate(),
            Err(TeslaMateReaderError::InvalidPageSize)
        ));
        assert!(matches!(
            TeslaMateReadLimits {
                maximum_rows: 0,
                ..TeslaMateReadLimits::default()
            }
            .validate(),
            Err(TeslaMateReaderError::InvalidMaximumRows)
        ));
    }

    #[test]
    fn row_budget_is_hard_before_retention() {
        let mut total = 2;
        assert!(matches!(
            retain_row(&mut total, 2),
            Err(TeslaMateReaderError::MaximumRowsExceeded { maximum: 2 })
        ));
        assert_eq!(total, 3);
    }

    #[test]
    fn selected_car_id_must_fit_the_source_smallint_domain() {
        assert!(matches!(
            selected_source_car_id(i64::from(i16::MAX)),
            Ok(value) if value == i16::MAX
        ));
        assert!(matches!(
            selected_source_car_id(i64::from(i16::MAX) + 1),
            Err(TeslaMateReaderError::SelectedCarIdOutOfRange)
        ));
    }

    #[test]
    fn sealed_stage_round_trips_the_small_snapshot_reader_contract() {
        let temporary = tempfile::tempdir().expect("temporary stage directory");
        let mut stage = TeslaMateStage::create(
            temporary.path(),
            TeslaMateStageLimits {
                max_rows: 10,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("stage");
        let car = TeslaMateCar {
            id: 1,
            eid: 88,
            vin: Some("5YJTESTVIN1234567".to_owned()),
            name: Some("Road car".to_owned()),
            model: Some("Model 3".to_owned()),
            trim_badging: None,
            marketing_name: None,
            efficiency_wh_per_km: Some(0.145),
        };
        stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .expect("stage car");
        stage.seal().expect("sealed");

        let history = materialize_small_staged_history(&stage, 10).expect("history");
        assert_eq!(history.cars, vec![car]);
        assert!(history.drives.is_empty());
        assert!(history.positions.is_empty());
        assert!(matches!(
            materialize_small_staged_history(&stage, 0),
            Err(TeslaMateReaderError::MaximumRowsExceeded { maximum: 0 })
        ));
    }
}
