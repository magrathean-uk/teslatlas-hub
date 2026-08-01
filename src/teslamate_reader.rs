//! Read-only, bounded TeslaMate PostgreSQL history reader.
//!
//! This is deliberately a source adapter, not a TeslaMate clone. It accepts a
//! credential-free endpoint and a systemd-provided password, opens one TLS
//! connection, pins the source to a repeatable-read transaction, checks the
//! reviewed schema, then fetches only fixed projections with keyset pages.

use std::{path::Path, pin::pin, time::Duration};

use futures_util::TryStreamExt;
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::PrimitiveDateTime;
use tokio::time::timeout;
use tokio_postgres::{
    Client, Config, NoTls, Row,
    binary_copy::BinaryCopyOutRow,
    config::SslMode,
    types::{FromSql, Type},
};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::{
    credentials::TeslaMatePostgresPassword,
    hub_pack::{GeofenceBillingType, ProjectionCarSettings},
    teslamate::ReadOnlySource,
    teslamate_projection::{
        TeslaMateAddress, TeslaMateCar, TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive,
        TeslaMateGeofence, TeslaMateHistory, TeslaMateOpenSession, TeslaMatePosition,
        TeslaMateProjectionError, TeslaMateSourceWatermark, TeslaMateSourceWatermarks,
        TeslaMateState, TeslaMateUpdate,
    },
    teslamate_schema::{
        MAX_VALIDATED_MIGRATION, MIGRATION_VERSION_SQL, MIN_SUPPORTED_MIGRATION, SCHEMA_PROBE_SQL,
        SourceTable, projection, validate_migration_version, validate_observed_schema,
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
    pub copy_statement_timeout: Duration,
    pub page_size: i32,
    pub maximum_rows: usize,
    pub maximum_stage_bytes: u64,
    pub minimum_free_bytes: u64,
    pub parallel_copy_lanes: usize,
}

impl Default for TeslaMateReadLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            copy_statement_timeout: Duration::from_secs(2 * 60 * 60),
            page_size: 2_000,
            maximum_rows: 20_000_000,
            maximum_stage_bytes: 4 * 1024 * 1024 * 1024,
            minimum_free_bytes: TeslaMateStageLimits::default().minimum_free_bytes,
            parallel_copy_lanes: 4,
        }
    }
}

impl TeslaMateReadLimits {
    pub fn validate(self) -> Result<(), TeslaMateReaderError> {
        if self.connect_timeout.is_zero() {
            return Err(TeslaMateReaderError::InvalidConnectTimeout);
        }
        if self.copy_statement_timeout < Duration::from_secs(60)
            || self.copy_statement_timeout > Duration::from_secs(24 * 60 * 60)
        {
            return Err(TeslaMateReaderError::InvalidCopyStatementTimeout);
        }
        if !(1..=10_000).contains(&self.page_size) {
            return Err(TeslaMateReaderError::InvalidPageSize);
        }
        if self.maximum_rows == 0 {
            return Err(TeslaMateReaderError::InvalidMaximumRows);
        }
        if !(1..=8).contains(&self.parallel_copy_lanes) {
            return Err(TeslaMateReaderError::InvalidParallelCopyLanes);
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

#[derive(Debug, Clone, Serialize)]
pub struct TeslaMateSchemaInfo {
    #[serde(rename = "observedMigrationVersion")]
    pub observed_migration_version: i64,
    #[serde(rename = "minimumSupportedMigrationVersion")]
    pub minimum_supported_migration_version: i64,
    #[serde(rename = "maximumValidatedMigrationVersion")]
    pub maximum_validated_migration_version: i64,
    pub fingerprint: String,
}

pub(crate) struct TeslaMateSnapshotSession {
    pub(crate) client: Client,
    connection_task: tokio::task::JoinHandle<()>,
}

/// The encrypted TeslaMate legacy OAuth pair. Ciphertext is sensitive even
/// though it is not plaintext, so this type deliberately has no derived
/// formatter or serializer.
pub struct TeslaMateLegacyTokenCiphertexts {
    pub access: Vec<u8>,
    pub refresh: Vec<u8>,
}

impl std::fmt::Debug for TeslaMateLegacyTokenCiphertexts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TeslaMateLegacyTokenCiphertexts([redacted])")
    }
}

impl TeslaMateSnapshotSession {
    pub(crate) async fn finish(self) -> Result<(), TeslaMateReaderError> {
        let rollback = self.client.batch_execute("ROLLBACK").await;
        drop(self.client);
        let _ = self.connection_task.await;
        rollback.map_err(Into::into)
    }
}

/// A read-only source transaction that keeps one PostgreSQL snapshot available
/// for bounded capture lanes. Dropping the lease releases the source snapshot;
/// callers must retain it until every lane has completed.
pub(crate) struct ExportedSnapshotLease {
    session: TeslaMateSnapshotSession,
    snapshot_id: String,
}

impl ExportedSnapshotLease {
    pub(crate) fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    pub(crate) async fn finish(self) -> Result<(), TeslaMateReaderError> {
        self.session.finish().await
    }
}

async fn connect_source(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    limits: TeslaMateReadLimits,
) -> Result<(Client, tokio::task::JoinHandle<()>), TeslaMateReaderError> {
    let user = source
        .user()
        .ok_or(TeslaMateReaderError::SourceUserRequired)?;
    let mut configuration = Config::new();
    configuration
        .host(source.host())
        .port(source.port())
        .user(user)
        .password(password.as_str())
        .dbname(source.database_name());
    if source.is_loopback() {
        configuration.ssl_mode(SslMode::Disable);
        let (client, connection) = timeout(limits.connect_timeout, configuration.connect(NoTls))
            .await
            .map_err(|_| TeslaMateReaderError::ConnectTimedOut)??;
        let connection_task = tokio::spawn(async move {
            let _ = connection.await;
        });
        return Ok((client, connection_task));
    }

    crate::crypto::install_default_provider();
    let (tls, certificate_errors) = MakeRustlsConnect::with_native_certs()
        .map_err(|_| TeslaMateReaderError::NativeTrustStoreUnavailable)?;
    if !certificate_errors.is_empty() {
        tracing::warn!(
            count = certificate_errors.len(),
            "some native TLS certificates could not be loaded"
        );
    }
    configuration.ssl_mode(SslMode::Require);

    let (client, connection) = timeout(limits.connect_timeout, configuration.connect(tls))
        .await
        .map_err(|_| TeslaMateReaderError::ConnectTimedOut)??;
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, connection_task))
}

/// Read exactly one encrypted legacy OAuth pair from TeslaMate's private
/// schema. The fixed query executes in the same repeatable-read, read-only
/// session as history migration; it never asks TeslaMate to refresh or write
/// credentials.
pub async fn read_legacy_token_ciphertexts(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateLegacyTokenCiphertexts, TeslaMateReaderError> {
    limits.validate()?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let session = TeslaMateSnapshotSession {
        client,
        connection_task,
    };
    let result = async {
        prepare_read_only_snapshot(&session.client, source, limits).await?;
        let rows = session
            .client
            .query(
                "SELECT \"token\".\"access\" AS \"access\", \"token\".\"refresh\" AS \"refresh\" \
                 FROM \"private\".\"tokens\" AS \"token\" ORDER BY \"token\".\"id\" ASC LIMIT 2",
                &[],
            )
            .await?;
        if rows.is_empty() {
            return Err(TeslaMateReaderError::LegacyTokenPairMissing);
        }
        if rows.len() != 1 {
            return Err(TeslaMateReaderError::LegacyTokenPairAmbiguous);
        }
        let row = &rows[0];
        let access: Vec<u8> = row
            .try_get("access")
            .map_err(|source| cell("private.tokens", "access", source))?;
        let refresh: Vec<u8> = row
            .try_get("refresh")
            .map_err(|source| cell("private.tokens", "refresh", source))?;
        if access.is_empty() || refresh.is_empty() {
            return Err(TeslaMateReaderError::LegacyTokenPairEmpty);
        }
        Ok(TeslaMateLegacyTokenCiphertexts { access, refresh })
    }
    .await;
    let finish = session.finish().await;
    match (result, finish) {
        (Err(error), _) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
    }
}

pub(crate) async fn open_snapshot_session(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<(TeslaMateSnapshotSession, i16), TeslaMateReaderError> {
    let (session, selected_car_id, _) =
        open_snapshot_session_with_schema(source, password, selected_car_id, limits).await?;
    Ok((session, selected_car_id))
}

pub(crate) async fn open_snapshot_session_with_schema(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<(TeslaMateSnapshotSession, i16, TeslaMateSchemaInfo), TeslaMateReaderError> {
    limits.validate()?;
    if selected_car_id <= 0 {
        return Err(TeslaMateReaderError::InvalidSelectedCarId);
    }
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let schema = match prepare_read_only_snapshot(&client, source, limits).await {
        Ok(schema) => schema,
        Err(error) => {
            drop(client);
            let _ = connection_task.await;
            return Err(error);
        }
    };
    Ok((
        TeslaMateSnapshotSession {
            client,
            connection_task,
        },
        selected_car_id,
        schema,
    ))
}

/// Open and validate the owner transaction for a future parallel capture.
/// PostgreSQL invalidates exported snapshots as soon as this lease ends, so a
/// caller cannot accidentally continue capture after its consistent source view
/// is gone.
pub(crate) async fn open_exported_snapshot_lease(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<(ExportedSnapshotLease, i16), TeslaMateReaderError> {
    let (session, selected_car_id) =
        open_snapshot_session(source, password, selected_car_id, limits).await?;
    let exported = session
        .client
        .query_one("SELECT pg_export_snapshot() AS snapshot_id", &[])
        .await
        .and_then(|row| row.try_get::<_, String>("snapshot_id"));
    let snapshot_id = match exported {
        Ok(snapshot_id) => match validate_exported_snapshot_id(snapshot_id) {
            Ok(snapshot_id) => snapshot_id,
            Err(error) => {
                let _ = session.finish().await;
                return Err(error);
            }
        },
        Err(error) => {
            let _ = session.finish().await;
            return Err(error.into());
        }
    };
    Ok((
        ExportedSnapshotLease {
            session,
            snapshot_id,
        },
        selected_car_id,
    ))
}

fn validate_exported_snapshot_id(snapshot_id: String) -> Result<String, TeslaMateReaderError> {
    let valid = snapshot_id.len() <= 64
        && snapshot_id.contains('-')
        && snapshot_id.split('-').all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_hexdigit())
        });
    valid
        .then_some(snapshot_id)
        .ok_or(TeslaMateReaderError::InvalidExportedSnapshot)
}

/// Open one bounded capture connection on an already-exported source view.
/// The owner lease must outlive this returned session.
pub(crate) async fn open_snapshot_capture_lane(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    snapshot_id: &str,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateSnapshotSession, TeslaMateReaderError> {
    limits.validate()?;
    let snapshot_sql = snapshot_import_sql(snapshot_id)?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let prepared = async {
        client.batch_execute(source.session_sql()[0]).await?;
        client.batch_execute(source.session_sql()[1]).await?;
        client.batch_execute(&snapshot_sql).await?;
        for statement in &source.session_sql()[2..] {
            client.batch_execute(statement).await?;
        }
        client
            .batch_execute(&copy_statement_timeout_sql(limits.copy_statement_timeout))
            .await?;
        validate_source_schema(&client).await
    }
    .await;
    if let Err(error) = prepared {
        drop(client);
        let _ = connection_task.await;
        return Err(error);
    }
    Ok(TeslaMateSnapshotSession {
        client,
        connection_task,
    })
}

fn snapshot_import_sql(snapshot_id: &str) -> Result<String, TeslaMateReaderError> {
    let snapshot_id = validate_exported_snapshot_id(snapshot_id.to_owned())?;
    Ok(format!("SET TRANSACTION SNAPSHOT '{snapshot_id}'"))
}

/// Build a source-safe binary `COPY TO STDOUT` statement for one reviewed
/// projection. PostgreSQL does not permit query parameters in `COPY`; both the
/// table and every SQL fragment are fixed and `selected_car_id` is already an
/// `i16` from the validated source domain.
pub(crate) fn binary_copy_sql(table: SourceTable, selected_car_id: i16) -> String {
    let query = render_projection_query(
        table,
        ProjectionQueryBindings {
            last_id: 0,
            limit: ProjectionLimit::All,
            selected_car_id,
        },
    );
    format!("COPY ({query}) TO STDOUT WITH (FORMAT BINARY)")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectionLimit {
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectionQueryBindings {
    last_id: i32,
    limit: ProjectionLimit,
    selected_car_id: i16,
}

/// Render the canonical reviewed projection with typed, integer-only bindings.
/// Unknown placeholder tokens remain unchanged so a future schema-template
/// change fails at PostgreSQL prepare time instead of being silently rewritten.
fn render_projection_query(table: SourceTable, bindings: ProjectionQueryBindings) -> String {
    let template = projection(table).sql;
    let mut rendered = String::with_capacity(template.len() + 16);
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$'
            || index + 1 >= bytes.len()
            || !bytes[index + 1].is_ascii_digit()
        {
            rendered.push(bytes[index] as char);
            index += 1;
            continue;
        }

        let start = index;
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let token = &template[start..index];
        match token {
            "$1" => rendered.push_str(&bindings.last_id.to_string()),
            "$2" => match bindings.limit {
                ProjectionLimit::All => rendered.push_str("ALL"),
            },
            "$3" => rendered.push_str(&bindings.selected_car_id.to_string()),
            _ => rendered.push_str(token),
        }
    }
    rendered
}

/// Build a source-safe binary `COPY TO STDOUT` statement for a bounded set of
/// reviewed position IDs. The inner query is the canonical positions
/// projection, so changes to its columns or casts stay coupled to every binary
/// position decoder. The caller supplies only validated `int4` identifiers.
pub(crate) fn related_positions_binary_copy_sql(
    selected_car_id: i16,
    position_ids: &[i32],
) -> String {
    debug_assert!(!position_ids.is_empty());
    let ids = position_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let positions = render_projection_query(
        SourceTable::Positions,
        ProjectionQueryBindings {
            last_id: 0,
            limit: ProjectionLimit::All,
            selected_car_id,
        },
    );
    let query = format!(
        "SELECT \"related\".* FROM ({positions}) AS \"related\" \
         WHERE \"related\".\"id\" = ANY(ARRAY[{ids}]::int4[]) \
         ORDER BY \"related\".\"id\" ASC"
    );
    format!("COPY ({query}) TO STDOUT WITH (FORMAT BINARY)")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenPositionBranch {
    Standalone,
    ActiveDrive(i64),
}

fn open_position_branch_copy_sql(
    selected_car_id: i16,
    branch: OpenPositionBranch,
) -> String {
    let positions = render_projection_query(
        SourceTable::Positions,
        ProjectionQueryBindings {
            last_id: 0,
            limit: ProjectionLimit::All,
            selected_car_id,
        },
    );
    let predicate = match branch {
        OpenPositionBranch::Standalone => "\"branch\".\"drive_id\" IS NULL".to_owned(),
        OpenPositionBranch::ActiveDrive(drive_id) => {
            format!("\"branch\".\"drive_id\" = {drive_id}")
        }
    };
    let query = format!(
        "SELECT \"branch\".* FROM ({positions}) AS \"branch\" \
         WHERE {predicate} ORDER BY \"branch\".\"id\" ASC"
    );
    format!("COPY ({query}) TO STDOUT WITH (FORMAT BINARY)")
}

const CAR_IDS_SQL: &str = "SELECT \"source\".\"id\" AS \"id\" FROM \"public\".\"cars\" AS \"source\" ORDER BY \"source\".\"id\" ASC";

pub async fn read_selected_car(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateCar, TeslaMateReaderError> {
    let (session, selected_car_id_i16) =
        open_snapshot_session(source, password, selected_car_id, limits).await?;
    let mut retained_rows = 0_usize;
    let result = read_cars(
        &session.client,
        selected_car_id_i16,
        limits,
        &mut retained_rows,
    )
    .await
    .and_then(|cars| {
        cars.into_iter()
            .next()
            .ok_or(TeslaMateReaderError::SelectedCarMissing { selected_car_id })
    });
    let finish = session.finish().await;
    match (result, finish) {
        (Ok(car), Ok(())) => Ok(car),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Read all TeslaMate car IDs from one validated, read-only repeatable-read
/// snapshot. IDs are returned in stable source order for future multi-car
/// orchestration.
pub async fn read_car_ids(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    limits: TeslaMateReadLimits,
) -> Result<Vec<i64>, TeslaMateReaderError> {
    limits.validate()?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let result = async {
        prepare_read_only_snapshot(&client, source, limits).await?;
        let rows = client.query(CAR_IDS_SQL, &[]).await?;
        if rows.len() > limits.maximum_rows {
            return Err(TeslaMateReaderError::MaximumRowsExceeded {
                maximum: limits.maximum_rows,
            });
        }
        let mut ids = rows
            .into_iter()
            .map(|row| {
                row.try_get::<_, i16>("id")
                    .map(i64::from)
                    .map_err(|error| cell("cars", "id", error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if ids.iter().any(|id| *id <= 0) {
            return Err(TeslaMateReaderError::InvalidSelectedCarId);
        }
        ids.sort_unstable();
        Ok(ids)
    }
    .await;
    let rollback = client.batch_execute("ROLLBACK").await;
    drop(client);
    let _ = connection_task.await;
    match (result, rollback) {
        (Err(error), _) => Err(error),
        (Ok(ids), Ok(())) => Ok(ids),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

/// Read active TeslaMate sessions and their attached rows from one validated,
/// read-only repeatable-read snapshot. Completed history is intentionally not
/// returned here.
pub async fn read_open_session(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateOpenSession, TeslaMateReaderError> {
    let (session, selected_car_id_i16) =
        open_snapshot_session(source, password, selected_car_id, limits).await?;
    let result = read_open_session_in_client(&session.client, selected_car_id_i16, limits).await;
    let finish = session.finish().await;
    match (result, finish) {
        (Err(error), _) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
    }
}

pub(crate) async fn read_open_session_in_client(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateOpenSession, TeslaMateReaderError> {
    let mut retained_rows = 0_usize;
    let drives = read_open_drives(client, selected_car_id, limits, &mut retained_rows).await?;
    let drive = resolve_open_row("drives", drives);
    let positions = read_open_positions(
        client,
        selected_car_id,
        drive.as_ref().map(|drive| drive.id),
        limits,
        &mut retained_rows,
    )
    .await?;
    let (drive_positions, standalone_positions): (Vec<_>, Vec<_>) = positions
        .into_iter()
        .partition(|position| position.drive_id.is_some());
    let processes =
        read_open_charging_processes(client, selected_car_id, limits, &mut retained_rows).await?;
    let charge = resolve_open_row("charging_processes", processes);
    let charge_samples = if charge.is_some() {
        read_open_charges(client, selected_car_id, limits, &mut retained_rows).await?
    } else {
        Vec::new()
    };
    let states = read_open_states(client, selected_car_id, limits, &mut retained_rows).await?;
    let state = resolve_open_row("states", states);
    let watermarks = read_source_watermarks(client, selected_car_id).await?;
    let result = TeslaMateOpenSession {
        car_id: i64::from(selected_car_id),
        drive,
        drive_positions,
        charge,
        charge_samples,
        state,
        standalone_positions,
        watermarks,
    };
    result
        .validate()
        .map_err(TeslaMateReaderError::OpenSessionProjection)?;
    Ok(result)
}

/// Resolve a live TeslaMate row only when the source has exactly one candidate.
/// Multiple unfinished rows are ambiguous stale state, not a valid active session.
/// Historical rows were captured separately and remain intact; a later live poll
/// establishes the authoritative session.
fn resolve_open_row<T>(table: &'static str, mut rows: Vec<T>) -> Option<T> {
    match rows.len() {
        0 => None,
        1 => rows.pop(),
        row_count => {
            tracing::warn!(
                table,
                row_count,
                "TeslaMate open session is ambiguous; ignoring open rows until live collection establishes truth"
            );
            None
        }
    }
}

fn open_rows_sql(table: SourceTable, predicate: &str) -> String {
    let sql = projection(table).sql;
    sql.replacen(
        "WHERE \"source\".\"id\" > $1",
        &format!("WHERE {predicate} AND \"source\".\"id\" > $1"),
        1,
    )
}

async fn read_open_drives(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateDrive>, TeslaMateReaderError> {
    read_open_rows(
        client,
        SourceTable::Drives,
        "\"source\".\"end_date\" IS NULL",
        selected_car_id,
        limits,
        retained_rows,
        |row| decode_drive(row),
    )
    .await
}

async fn read_open_positions(
    client: &Client,
    selected_car_id: i16,
    active_drive_id: Option<i64>,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMatePosition>, TeslaMateReaderError> {
    let mut positions = read_open_position_branch(
        client,
        selected_car_id,
        OpenPositionBranch::Standalone,
        limits,
        retained_rows,
    )
    .await?;
    if let Some(active_drive_id) = active_drive_id {
        positions.extend(
            read_open_position_branch(
                client,
                selected_car_id,
                OpenPositionBranch::ActiveDrive(active_drive_id),
                limits,
                retained_rows,
            )
            .await?,
        );
    }
    positions.sort_unstable_by_key(|position| position.id);
    Ok(positions)
}

async fn read_open_position_branch(
    client: &Client,
    selected_car_id: i16,
    branch: OpenPositionBranch,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMatePosition>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&open_position_branch_copy_sql(selected_car_id, branch))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        position_copy_types()
    ));
    let mut positions = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        positions.push(decode_binary_position(&row)?);
    }
    Ok(positions)
}

async fn read_open_charging_processes(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateChargingProcess>, TeslaMateReaderError> {
    read_open_rows(
        client,
        SourceTable::ChargingProcesses,
        "\"source\".\"end_date\" IS NULL",
        selected_car_id,
        limits,
        retained_rows,
        |row| decode_charging_process(row),
    )
    .await
}

async fn read_open_charges(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateCharge>, TeslaMateReaderError> {
    read_open_rows(
        client,
        SourceTable::Charges,
        "\"process\".\"end_date\" IS NULL",
        selected_car_id,
        limits,
        retained_rows,
        |row| decode_charge(row),
    )
    .await
}

async fn read_open_states(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateState>, TeslaMateReaderError> {
    read_open_rows(
        client,
        SourceTable::States,
        "\"source\".\"end_date\" IS NULL",
        selected_car_id,
        limits,
        retained_rows,
        |row| decode_state(row),
    )
    .await
}

async fn read_open_rows<T, F>(
    client: &Client,
    table: SourceTable,
    predicate: &str,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    decode: F,
) -> Result<Vec<T>, TeslaMateReaderError>
where
    F: Fn(&Row) -> Result<T, TeslaMateReaderError>,
{
    let sql = open_rows_sql(table, predicate);
    let page_size = i64::from(limits.page_size);
    let mut last_id = 0_i32;
    let mut result = Vec::new();
    loop {
        let page = client
            .query(&sql, &[&last_id, &page_size, &selected_car_id])
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
            result.push(decode(&row)?);
        }
        if page_len < limits.page_size as usize {
            return Ok(result);
        }
    }
}

async fn read_source_watermarks(
    client: &Client,
    selected_car_id: i16,
) -> Result<TeslaMateSourceWatermarks, TeslaMateReaderError> {
    Ok(TeslaMateSourceWatermarks {
        drives: read_interval_watermark(
            client,
            "drives",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"start_date\") AS \"max_start\", MAX(\"end_date\") AS \"max_end\" FROM \"public\".\"drives\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        positions: read_date_watermark(
            client,
            "positions",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"date\") AS \"max_timestamp\" FROM \"public\".\"positions\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        charging_processes: read_interval_watermark(
            client,
            "charging_processes",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"start_date\") AS \"max_start\", MAX(\"end_date\") AS \"max_end\" FROM \"public\".\"charging_processes\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        charges: read_date_watermark(
            client,
            "charges",
            "SELECT MAX(\"charge\".\"id\") AS \"max_id\", MAX(\"charge\".\"date\") AS \"max_timestamp\" FROM \"public\".\"charges\" AS \"charge\" JOIN \"public\".\"charging_processes\" AS \"process\" ON \"process\".\"id\" = \"charge\".\"charging_process_id\" WHERE \"process\".\"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        states: read_interval_watermark(
            client,
            "states",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"start_date\") AS \"max_start\", MAX(\"end_date\") AS \"max_end\" FROM \"public\".\"states\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        updates: read_interval_watermark(
            client,
            "updates",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"start_date\") AS \"max_start\", MAX(\"end_date\") AS \"max_end\" FROM \"public\".\"updates\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
    })
}

async fn read_interval_watermark(
    client: &Client,
    table: &'static str,
    sql: &'static str,
    selected_car_id: i16,
) -> Result<TeslaMateSourceWatermark, TeslaMateReaderError> {
    let row = client.query_one(sql, &[&selected_car_id]).await?;
    let max_id = row
        .try_get::<_, Option<i32>>("max_id")
        .map_err(|source| cell(table, "id", source))?
        .map(i64::from);
    let start = row
        .try_get::<_, Option<PrimitiveDateTime>>("max_start")
        .map_err(|source| cell(table, "start_date", source))?
        .map(|value| timestamp_ms(value, table, "start_date"))
        .transpose()?;
    let end = row
        .try_get::<_, Option<PrimitiveDateTime>>("max_end")
        .map_err(|source| cell(table, "end_date", source))?
        .map(|value| timestamp_ms(value, table, "end_date"))
        .transpose()?;
    Ok(TeslaMateSourceWatermark {
        max_id,
        max_timestamp_ms: match (start, end) {
            (Some(start), Some(end)) => Some(start.max(end)),
            (Some(start), None) => Some(start),
            (None, Some(end)) => Some(end),
            (None, None) => None,
        },
    })
}

async fn read_date_watermark(
    client: &Client,
    table: &'static str,
    sql: &'static str,
    selected_car_id: i16,
) -> Result<TeslaMateSourceWatermark, TeslaMateReaderError> {
    let row = client.query_one(sql, &[&selected_car_id]).await?;
    let max_id = row
        .try_get::<_, Option<i32>>("max_id")
        .map_err(|source| cell(table, "id", source))?
        .map(i64::from);
    let timestamp = row
        .try_get::<_, Option<PrimitiveDateTime>>("max_timestamp")
        .map_err(|source| cell(table, "date", source))?
        .map(|value| timestamp_ms(value, table, "date"))
        .transpose()?;
    Ok(TeslaMateSourceWatermark {
        max_id,
        max_timestamp_ms: timestamp,
    })
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
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let (client, connection_task) = connect_source(source, password, limits).await?;

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
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let mut stage = TeslaMateStage::create(
        imports_dir,
        TeslaMateStageLimits {
            max_rows: u64::try_from(limits.maximum_rows).expect("usize fits u64"),
            max_stage_bytes: limits.maximum_stage_bytes,
            minimum_free_bytes: limits.minimum_free_bytes,
        },
    )?;

    let (client, connection_task) = match connect_source(source, password, limits).await {
        Ok(connection) => connection,
        Err(error) => {
            let _ = stage.discard();
            return Err(error);
        }
    };

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
        states: collect_staged_rows(stage, TeslaMateStageTable::States)?,
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
    prepare_read_only_snapshot(client, source, limits).await?;

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
    capture_geofence_pages(client, selected_car_id, limits, &mut retained_rows, stage).await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::States,
            stage_table: TeslaMateStageTable::States,
            decode: decode_state,
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
    let page_size = i64::from(limits.page_size);
    loop {
        let page = client
            .query(
                projection(projection_descriptor.source_table).sql,
                &[&last_id, &page_size, &selected_car_id],
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
    let page_size = i64::from(limits.page_size);
    loop {
        let page = client
            .query(
                projection(projection_descriptor.source_table).sql,
                &[&last_id, &page_size, &selected_car_id],
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

const GEOFENCE_GEOMETRY_SQL: &str = r#"
SELECT
  source.id,
  source.name,
  source.latitude::double precision AS latitude,
  source.longitude::double precision AS longitude,
  source.radius::double precision AS radius_m,
  source.billing_type::text AS billing_type,
  source.cost_per_unit::double precision AS cost_per_unit,
  source.session_fee::double precision AS session_fee
FROM public.geofences AS source
WHERE source.id > $1
  AND (
    EXISTS (
      SELECT 1 FROM public.drives AS drive
      WHERE drive.car_id = $3
        AND (drive.start_geofence_id = source.id OR drive.end_geofence_id = source.id)
    )
    OR EXISTS (
      SELECT 1 FROM public.charging_processes AS process
      WHERE process.car_id = $3 AND process.geofence_id = source.id
    )
  )
ORDER BY source.id ASC
LIMIT $2
"#;

async fn capture_geofence_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<usize, TeslaMateReaderError> {
    let mut last_id = 0_i32;
    let mut captured_rows = 0_usize;
    let page_size = i64::from(limits.page_size);
    loop {
        let page = client
            .query(
                GEOFENCE_GEOMETRY_SQL,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in page {
            let id = required_i32(&row, "geofences", "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "geofences" });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            decoded.push((i64::from(id), decode_geofence(&row)?));
        }
        captured_rows = captured_rows.checked_add(page_len).ok_or(
            TeslaMateReaderError::MaximumRowsExceeded {
                maximum: limits.maximum_rows,
            },
        )?;
        stage.insert_page(TeslaMateStageTable::Geofences, decoded)?;
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
    prepare_read_only_snapshot(client, source, limits).await?;

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
    let states = read_states(client, selected_car_id, limits, &mut retained_rows).await?;
    let updates = read_updates(client, selected_car_id, limits, &mut retained_rows).await?;

    Ok(TeslaMateHistory {
        cars,
        drives,
        positions,
        charging_processes,
        charges,
        addresses,
        geofences,
        states,
        updates,
    })
}

pub(crate) async fn prepare_read_only_snapshot(
    client: &Client,
    source: &ReadOnlySource,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateSchemaInfo, TeslaMateReaderError> {
    for statement in source.session_sql() {
        client.batch_execute(statement).await?;
    }
    client
        .batch_execute(&copy_statement_timeout_sql(limits.copy_statement_timeout))
        .await?;

    validate_source_schema(client).await
}

fn copy_statement_timeout_sql(timeout: Duration) -> String {
    format!("SET LOCAL statement_timeout = '{}ms'", timeout.as_millis())
}

async fn validate_source_schema(
    client: &Client,
) -> Result<TeslaMateSchemaInfo, TeslaMateReaderError> {
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

    let mut digest = Sha256::new();
    for column in &observed {
        digest.update(column.table.as_bytes());
        digest.update([0]);
        digest.update(column.name.as_bytes());
        digest.update([0]);
        digest.update(column.type_name.as_bytes());
        digest.update([u8::from(column.nullable)]);
    }
    Ok(TeslaMateSchemaInfo {
        observed_migration_version: migration,
        minimum_supported_migration_version: MIN_SUPPORTED_MIGRATION,
        maximum_validated_migration_version: MAX_VALIDATED_MIGRATION,
        fingerprint: format!("{:x}", digest.finalize()),
    })
}

pub(crate) async fn read_cars(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateCar>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Cars, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        car_copy_types()
    ));
    let mut cars = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        cars.push(decode_binary_car(&row)?);
    }
    Ok(cars)
}

fn car_copy_types() -> &'static [Type] {
    const TYPES: [Type; 22] = [
        Type::INT2,
        Type::INT8,
        Type::INT8,
        Type::TEXT,
        Type::TEXT,
        Type::TEXT,
        Type::FLOAT8,
        Type::INT4,
        Type::INT4,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::TEXT,
        Type::TEXT,
        Type::TEXT,
        Type::TEXT,
        Type::TEXT,
        Type::INT2,
        Type::TIMESTAMP,
        Type::TIMESTAMP,
    ];
    &TYPES
}

fn decode_binary_car(row: &BinaryCopyOutRow) -> Result<TeslaMateCar, TeslaMateReaderError> {
    let id: i16 = binary_cell(row, 0, "cars", "id")?;
    Ok(TeslaMateCar {
        id: i64::from(id),
        eid: binary_cell(row, 1, "cars", "eid")?,
        vid: binary_cell(row, 2, "cars", "vid")?,
        vin: binary_cell::<Option<&str>>(row, 3, "cars", "vin")?.map(ToOwned::to_owned),
        name: binary_cell::<Option<&str>>(row, 4, "cars", "name")?.map(ToOwned::to_owned),
        model: binary_cell::<Option<&str>>(row, 5, "cars", "model")?.map(ToOwned::to_owned),
        trim_badging: binary_cell::<Option<&str>>(row, 14, "cars", "trim_badging")?
            .map(ToOwned::to_owned),
        marketing_name: binary_cell::<Option<&str>>(row, 15, "cars", "marketing_name")?
            .map(ToOwned::to_owned),
        exterior_color: binary_cell::<Option<&str>>(row, 16, "cars", "exterior_color")?
            .map(ToOwned::to_owned),
        wheel_type: binary_cell::<Option<&str>>(row, 17, "cars", "wheel_type")?
            .map(ToOwned::to_owned),
        spoiler_type: binary_cell::<Option<&str>>(row, 18, "cars", "spoiler_type")?
            .map(ToOwned::to_owned),
        efficiency_wh_per_km: binary_cell(row, 6, "cars", "efficiency")?,
        settings: decode_car_settings_binary(row)?,
    })
}

fn decode_car_settings_binary(
    row: &BinaryCopyOutRow,
) -> Result<ProjectionCarSettings, TeslaMateReaderError> {
    let defaults = ProjectionCarSettings::default();
    Ok(ProjectionCarSettings {
        suspend_min_resolved: true,
        suspend_min: binary_optional_smallint(row, 7, "car_settings", "suspend_min")?
            .map(i64::from)
            .unwrap_or(defaults.suspend_min),
        suspend_after_idle_min: binary_optional_smallint(
            row,
            8,
            "car_settings",
            "suspend_after_idle_min",
        )?
        .map(i64::from)
        .unwrap_or(defaults.suspend_after_idle_min),
        req_not_unlocked: binary_cell::<Option<bool>>(row, 9, "cars", "req_not_unlocked")?
            .unwrap_or(defaults.req_not_unlocked),
        free_supercharging: binary_cell::<Option<bool>>(row, 10, "cars", "free_supercharging")?
            .unwrap_or(defaults.free_supercharging),
        use_streaming_api: binary_cell::<Option<bool>>(row, 11, "cars", "use_streaming_api")?
            .unwrap_or(defaults.use_streaming_api),
        enabled: binary_cell::<Option<bool>>(row, 12, "cars", "enabled")?
            .unwrap_or(defaults.enabled),
        lfp_battery: binary_cell::<Option<bool>>(row, 13, "cars", "lfp_battery")?
            .unwrap_or(defaults.lfp_battery),
    })
}

fn binary_cell<'a, T: FromSql<'a>>(
    row: &'a BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<T, TeslaMateReaderError> {
    row.try_get(index)
        .map_err(|source| cell(table, column, source))
}

fn binary_optional_smallint(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i16>, TeslaMateReaderError> {
    binary_cell::<Option<i32>>(row, index, table, column)?
        .map(|value| narrow_smallint(value, table, column))
        .transpose()
}

fn binary_optional_decimal(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<Option<f64>, TeslaMateReaderError> {
    binary_cell::<Option<Decimal>>(row, index, table, column)?
        .map(|value| {
            value
                .to_f64()
                .ok_or(TeslaMateReaderError::DecimalOutOfRange { table, column })
        })
        .transpose()
}

fn binary_required_decimal(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<f64, TeslaMateReaderError> {
    binary_cell::<Decimal>(row, index, table, column)?
        .to_f64()
        .ok_or(TeslaMateReaderError::DecimalOutOfRange { table, column })
}

fn binary_required_timestamp_ms(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<i64, TeslaMateReaderError> {
    timestamp_ms(binary_cell(row, index, table, column)?, table, column)
}

fn binary_optional_timestamp_ms(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i64>, TeslaMateReaderError> {
    binary_cell::<Option<PrimitiveDateTime>>(row, index, table, column)?
        .map(|value| timestamp_ms(value, table, column))
        .transpose()
}

pub(crate) async fn read_drives(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateDrive>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Drives, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        drive_copy_types()
    ));
    let mut drives = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        drives.push(decode_binary_drive(&row)?);
    }
    Ok(drives)
}

fn drive_copy_types() -> &'static [Type] {
    const TYPES: [Type; 25] = [
        Type::INT4,
        Type::INT2,
        Type::TIMESTAMP,
        Type::TIMESTAMP,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::FLOAT8,
        Type::FLOAT8,
        Type::FLOAT8,
        Type::INT2,
        Type::INT2,
        Type::INT2,
    ];
    &TYPES
}

fn decode_binary_drive(row: &BinaryCopyOutRow) -> Result<TeslaMateDrive, TeslaMateReaderError> {
    let id: i32 = binary_cell(row, 0, "drives", "id")?;
    let car_id: i16 = binary_cell(row, 1, "drives", "car_id")?;
    Ok(TeslaMateDrive {
        id: i64::from(id),
        car_id: i64::from(car_id),
        start_date_ms: binary_required_timestamp_ms(row, 2, "drives", "start_date")?,
        end_date_ms: binary_optional_timestamp_ms(row, 3, "drives", "end_date")?,
        start_position_id: binary_cell::<Option<i32>>(row, 4, "drives", "start_position_id")?
            .map(i64::from),
        end_position_id: binary_cell::<Option<i32>>(row, 5, "drives", "end_position_id")?
            .map(i64::from),
        start_address_id: binary_cell::<Option<i32>>(row, 6, "drives", "start_address_id")?
            .map(i64::from),
        end_address_id: binary_cell::<Option<i32>>(row, 7, "drives", "end_address_id")?
            .map(i64::from),
        start_geofence_id: binary_cell::<Option<i32>>(row, 8, "drives", "start_geofence_id")?
            .map(i64::from),
        end_geofence_id: binary_cell::<Option<i32>>(row, 9, "drives", "end_geofence_id")?
            .map(i64::from),
        outside_temp_avg: binary_optional_decimal(row, 10, "drives", "outside_temp_avg")?,
        inside_temp_avg: binary_optional_decimal(row, 11, "drives", "inside_temp_avg")?,
        speed_max: binary_cell::<Option<i16>>(row, 12, "drives", "speed_max")?.map(i64::from),
        power_max: binary_cell::<Option<i16>>(row, 13, "drives", "power_max")?.map(f64::from),
        power_min: binary_cell::<Option<i16>>(row, 14, "drives", "power_min")?.map(f64::from),
        start_ideal_range_km: binary_optional_decimal(row, 15, "drives", "start_ideal_range_km")?,
        end_ideal_range_km: binary_optional_decimal(row, 16, "drives", "end_ideal_range_km")?,
        start_rated_range_km: binary_optional_decimal(row, 17, "drives", "start_rated_range_km")?,
        end_rated_range_km: binary_optional_decimal(row, 18, "drives", "end_rated_range_km")?,
        start_km: binary_cell(row, 19, "drives", "start_km")?,
        end_km: binary_cell(row, 20, "drives", "end_km")?,
        distance_km: binary_cell(row, 21, "drives", "distance")?,
        duration_min: binary_cell::<Option<i16>>(row, 22, "drives", "duration_min")?.map(i64::from),
        ascent: binary_cell::<Option<i16>>(row, 23, "drives", "ascent")?.map(i64::from),
        descent: binary_cell::<Option<i16>>(row, 24, "drives", "descent")?.map(i64::from),
    })
}

pub(crate) fn position_copy_types() -> &'static [Type] {
    const TYPES: [Type; 30] = [
        Type::INT4,
        Type::INT2,
        Type::INT8,
        Type::TIMESTAMP,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT8,
        Type::INT8,
        Type::FLOAT8,
        Type::FLOAT8,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT8,
        Type::INT8,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT8,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
    ];
    &TYPES
}

pub(crate) fn decode_binary_position(
    row: &BinaryCopyOutRow,
) -> Result<TeslaMatePosition, TeslaMateReaderError> {
    let id: i32 = binary_cell(row, 0, "positions", "id")?;
    let car_id: i16 = binary_cell(row, 1, "positions", "car_id")?;
    Ok(TeslaMatePosition {
        id: i64::from(id),
        car_id: i64::from(car_id),
        drive_id: binary_cell::<Option<i64>>(row, 2, "positions", "drive_id")?,
        date_ms: binary_required_timestamp_ms(row, 3, "positions", "date")?,
        latitude: binary_required_decimal(row, 4, "positions", "latitude")?,
        longitude: binary_required_decimal(row, 5, "positions", "longitude")?,
        elevation: binary_cell::<Option<i64>>(row, 6, "positions", "elevation")?,
        speed: binary_cell::<Option<i64>>(row, 7, "positions", "speed")?,
        power: binary_cell(row, 8, "positions", "power")?,
        odometer: binary_cell(row, 9, "positions", "odometer")?,
        ideal_battery_range_km: binary_optional_decimal(
            row,
            10,
            "positions",
            "ideal_battery_range_km",
        )?,
        est_battery_range_km: binary_optional_decimal(
            row,
            11,
            "positions",
            "est_battery_range_km",
        )?,
        rated_battery_range_km: binary_optional_decimal(
            row,
            12,
            "positions",
            "rated_battery_range_km",
        )?,
        battery_level: binary_cell::<Option<i64>>(row, 13, "positions", "battery_level")?,
        usable_battery_level: binary_cell::<Option<i64>>(
            row,
            14,
            "positions",
            "usable_battery_level",
        )?
        ,
        battery_heater: binary_cell(row, 15, "positions", "battery_heater")?,
        battery_heater_on: binary_cell(row, 16, "positions", "battery_heater_on")?,
        battery_heater_no_power: binary_cell(row, 17, "positions", "battery_heater_no_power")?,
        is_climate_on: binary_cell(row, 23, "positions", "is_climate_on")?,
        outside_temp: binary_optional_decimal(row, 18, "positions", "outside_temp")?,
        inside_temp: binary_optional_decimal(row, 19, "positions", "inside_temp")?,
        fan_status: binary_cell(row, 20, "positions", "fan_status")?,
        driver_temp_setting: binary_optional_decimal(row, 21, "positions", "driver_temp_setting")?,
        passenger_temp_setting: binary_optional_decimal(
            row,
            22,
            "positions",
            "passenger_temp_setting",
        )?,
        is_rear_defroster_on: binary_cell(row, 24, "positions", "is_rear_defroster_on")?,
        is_front_defroster_on: binary_cell(row, 25, "positions", "is_front_defroster_on")?,
        tpms_pressure_fl: binary_optional_decimal(row, 26, "positions", "tpms_pressure_fl")?,
        tpms_pressure_fr: binary_optional_decimal(row, 27, "positions", "tpms_pressure_fr")?,
        tpms_pressure_rl: binary_optional_decimal(row, 28, "positions", "tpms_pressure_rl")?,
        tpms_pressure_rr: binary_optional_decimal(row, 29, "positions", "tpms_pressure_rr")?,
    })
}

async fn read_positions(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMatePosition>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Positions, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        position_copy_types()
    ));
    let mut positions = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        positions.push(decode_binary_position(&row)?);
    }
    Ok(positions)
}

pub(crate) async fn read_charging_processes(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateChargingProcess>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(
            SourceTable::ChargingProcesses,
            selected_car_id,
        ))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        charging_process_copy_types()
    ));
    let mut processes = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        processes.push(decode_binary_charging_process(&row)?);
    }
    Ok(processes)
}

fn charging_process_copy_types() -> &'static [Type] {
    const TYPES: [Type; 18] = [
        Type::INT4,
        Type::INT2,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::TIMESTAMP,
        Type::TIMESTAMP,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::NUMERIC,
        Type::NUMERIC,
    ];
    &TYPES
}

fn decode_binary_charging_process(
    row: &BinaryCopyOutRow,
) -> Result<TeslaMateChargingProcess, TeslaMateReaderError> {
    let id: i32 = binary_cell(row, 0, "charging_processes", "id")?;
    let car_id: i16 = binary_cell(row, 1, "charging_processes", "car_id")?;
    Ok(TeslaMateChargingProcess {
        id: i64::from(id),
        car_id: i64::from(car_id),
        position_id: binary_cell::<Option<i32>>(row, 2, "charging_processes", "position_id")?
            .map(i64::from),
        address_id: binary_cell::<Option<i32>>(row, 3, "charging_processes", "address_id")?
            .map(i64::from),
        geofence_id: binary_cell::<Option<i32>>(row, 4, "charging_processes", "geofence_id")?
            .map(i64::from),
        start_date_ms: binary_required_timestamp_ms(row, 5, "charging_processes", "start_date")?,
        end_date_ms: binary_optional_timestamp_ms(row, 6, "charging_processes", "end_date")?,
        charge_energy_added: binary_optional_decimal(
            row,
            7,
            "charging_processes",
            "charge_energy_added",
        )?,
        charge_energy_used_kwh: binary_optional_decimal(
            row,
            8,
            "charging_processes",
            "charge_energy_used",
        )?,
        start_ideal_range_km: binary_optional_decimal(
            row,
            9,
            "charging_processes",
            "start_ideal_range_km",
        )?,
        end_ideal_range_km: binary_optional_decimal(
            row,
            10,
            "charging_processes",
            "end_ideal_range_km",
        )?,
        start_battery_level: binary_cell::<Option<i16>>(
            row,
            13,
            "charging_processes",
            "start_battery_level",
        )?
        .map(i64::from),
        end_battery_level: binary_cell::<Option<i16>>(
            row,
            14,
            "charging_processes",
            "end_battery_level",
        )?
        .map(i64::from),
        duration_min: binary_cell::<Option<i16>>(row, 15, "charging_processes", "duration_min")?
            .map(i64::from),
        outside_temp_avg: binary_optional_decimal(
            row,
            16,
            "charging_processes",
            "outside_temp_avg",
        )?,
        cost: binary_optional_decimal(row, 17, "charging_processes", "cost")?,
        start_rated_range_km: binary_optional_decimal(
            row,
            11,
            "charging_processes",
            "start_rated_range_km",
        )?,
        end_rated_range_km: binary_optional_decimal(
            row,
            12,
            "charging_processes",
            "end_rated_range_km",
        )?,
    })
}

async fn read_charges(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateCharge>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Charges, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        charge_copy_types()
    ));
    let mut charges = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        charges.push(decode_binary_charge(&row)?);
    }
    Ok(charges)
}

pub(crate) fn charge_copy_types() -> &'static [Type] {
    const TYPES: [Type; 22] = [
        Type::INT4,
        Type::INT4,
        Type::TIMESTAMP,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::INT2,
        Type::INT2,
        Type::NUMERIC,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::TEXT,
        Type::BOOL,
        Type::TEXT,
        Type::TEXT,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::BOOL,
        Type::NUMERIC,
    ];
    &TYPES
}

pub(crate) fn decode_binary_charge(
    row: &BinaryCopyOutRow,
) -> Result<TeslaMateCharge, TeslaMateReaderError> {
    let id: i32 = binary_cell(row, 0, "charges", "id")?;
    let process_id: i32 = binary_cell(row, 1, "charges", "charging_process_id")?;
    Ok(TeslaMateCharge {
        id: i64::from(id),
        charging_process_id: i64::from(process_id),
        date_ms: binary_required_timestamp_ms(row, 2, "charges", "date")?,
        battery_heater: binary_cell(row, 3, "charges", "battery_heater")?,
        battery_heater_on: binary_cell(row, 4, "charges", "battery_heater_on")?,
        battery_heater_no_power: binary_cell(row, 5, "charges", "battery_heater_no_power")?,
        battery_level: binary_cell::<Option<i16>>(row, 6, "charges", "battery_level")?
            .map(i64::from),
        usable_battery_level: binary_cell::<Option<i16>>(
            row,
            7,
            "charges",
            "usable_battery_level",
        )?
        .map(i64::from),
        charge_energy_added_kwh: binary_optional_decimal(row, 8, "charges", "charge_energy_added")?,
        charger_actual_current: binary_cell::<Option<i16>>(
            row,
            9,
            "charges",
            "charger_actual_current",
        )?
        .map(f64::from),
        charger_phases: binary_cell::<Option<i16>>(row, 10, "charges", "charger_phases")?
            .map(i64::from),
        charger_pilot_current: binary_cell::<Option<i16>>(
            row,
            11,
            "charges",
            "charger_pilot_current",
        )?
        .map(f64::from),
        charger_power_kw: binary_cell::<Option<i16>>(row, 12, "charges", "charger_power")?
            .map(f64::from),
        charger_voltage: binary_cell::<Option<i16>>(row, 13, "charges", "charger_voltage")?
            .map(f64::from),
        charge_cable: binary_cell::<Option<&str>>(row, 14, "charges", "conn_charge_cable")?
            .map(ToOwned::to_owned),
        fast_charger_present: binary_cell(row, 15, "charges", "fast_charger_present")?,
        fast_charger_brand: binary_cell::<Option<&str>>(row, 16, "charges", "fast_charger_brand")?
            .map(ToOwned::to_owned),
        fast_charger_type: binary_cell::<Option<&str>>(row, 17, "charges", "fast_charger_type")?
            .map(ToOwned::to_owned),
        ideal_range_km: binary_optional_decimal(row, 18, "charges", "ideal_battery_range_km")?,
        rated_range_km: binary_optional_decimal(row, 19, "charges", "rated_battery_range_km")?,
        not_enough_power_to_heat: binary_cell(row, 20, "charges", "not_enough_power_to_heat")?,
        outside_temp_c: binary_optional_decimal(row, 21, "charges", "outside_temp")?,
    })
}

pub(crate) async fn read_addresses(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateAddress>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Addresses, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        address_copy_types()
    ));
    let mut addresses = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        let id: i32 = binary_cell(&row, 0, "addresses", "id")?;
        addresses.push(TeslaMateAddress {
            id: i64::from(id),
            display_name: binary_cell::<Option<&str>>(&row, 1, "addresses", "display_name")?
                .map(ToOwned::to_owned),
            name: binary_cell::<Option<&str>>(&row, 2, "addresses", "name")?.map(ToOwned::to_owned),
        });
    }
    Ok(addresses)
}

fn address_copy_types() -> &'static [Type] {
    const TYPES: [Type; 3] = [Type::INT4, Type::TEXT, Type::TEXT];
    &TYPES
}

pub(crate) async fn read_geofences(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateGeofence>, TeslaMateReaderError> {
    let mut geofences = Vec::new();
    let mut last_id = 0_i32;
    let page_size = i64::from(limits.page_size);
    loop {
        let rows = client
            .query(
                GEOFENCE_GEOMETRY_SQL,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = rows.len();
        for row in rows {
            let id = required_i32(&row, "geofences", "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "geofences" });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            geofences.push(decode_geofence(&row)?);
        }
        if page_len < limits.page_size as usize {
            break;
        }
    }
    Ok(geofences)
}

pub(crate) async fn read_updates(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateUpdate>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Updates, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        update_copy_types()
    ));
    let mut updates = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        let id: i32 = binary_cell(&row, 0, "updates", "id")?;
        let car_id: i16 = binary_cell(&row, 1, "updates", "car_id")?;
        updates.push(TeslaMateUpdate {
            id: i64::from(id),
            car_id: i64::from(car_id),
            start_date_ms: binary_required_timestamp_ms(&row, 2, "updates", "start_date")?,
            end_date_ms: binary_optional_timestamp_ms(&row, 3, "updates", "end_date")?,
            version: binary_cell::<Option<&str>>(&row, 4, "updates", "version")?
                .map(ToOwned::to_owned),
        });
    }
    Ok(updates)
}

async fn read_states(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateState>, TeslaMateReaderError> {
    let mut last_id = 0_i32;
    let page_size = i64::from(limits.page_size);
    let mut states = Vec::new();
    loop {
        let page = client
            .query(
                projection(SourceTable::States).sql,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        for row in page {
            let id = required_i32(&row, "states", "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "states" });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            states.push(decode_state(&row)?);
        }
        if page_len < limits.page_size as usize {
            return Ok(states);
        }
    }
}

fn update_copy_types() -> &'static [Type] {
    const TYPES: [Type; 5] = [
        Type::INT4,
        Type::INT2,
        Type::TIMESTAMP,
        Type::TIMESTAMP,
        Type::TEXT,
    ];
    &TYPES
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
        vid: optional_i64(row, "cars", "vid")?,
        vin: optional_text(row, "cars", "vin")?,
        name: optional_text(row, "cars", "name")?,
        model: optional_text(row, "cars", "model")?,
        trim_badging: optional_text(row, "cars", "trim_badging")?,
        marketing_name: optional_text(row, "cars", "marketing_name")?,
        exterior_color: optional_text(row, "cars", "exterior_color")?,
        wheel_type: optional_text(row, "cars", "wheel_type")?,
        spoiler_type: optional_text(row, "cars", "spoiler_type")?,
        efficiency_wh_per_km: optional_float(row, "cars", "efficiency")?,
        settings: decode_car_settings_row(row)?,
    })
}

fn decode_car_settings_row(row: &Row) -> Result<ProjectionCarSettings, TeslaMateReaderError> {
    let defaults = ProjectionCarSettings::default();
    Ok(ProjectionCarSettings {
        suspend_min_resolved: true,
        suspend_min: optional_smallint(row, "car_settings", "suspend_min")?
            .map(i64::from)
            .unwrap_or(defaults.suspend_min),
        suspend_after_idle_min: optional_smallint(row, "car_settings", "suspend_after_idle_min")?
            .map(i64::from)
            .unwrap_or(defaults.suspend_after_idle_min),
        req_not_unlocked: optional_bool(row, "cars", "req_not_unlocked")?
            .unwrap_or(defaults.req_not_unlocked),
        free_supercharging: optional_bool(row, "cars", "free_supercharging")?
            .unwrap_or(defaults.free_supercharging),
        use_streaming_api: optional_bool(row, "cars", "use_streaming_api")?
            .unwrap_or(defaults.use_streaming_api),
        enabled: optional_bool(row, "cars", "enabled")?.unwrap_or(defaults.enabled),
        lfp_battery: optional_bool(row, "cars", "lfp_battery")?.unwrap_or(defaults.lfp_battery),
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
        inside_temp_avg: optional_decimal(row, "drives", "inside_temp_avg")?,
        speed_max: optional_i16(row, "drives", "speed_max")?.map(i64::from),
        power_max: optional_i16(row, "drives", "power_max")?.map(f64::from),
        power_min: optional_i16(row, "drives", "power_min")?.map(f64::from),
        start_ideal_range_km: optional_decimal(row, "drives", "start_ideal_range_km")?,
        end_ideal_range_km: optional_decimal(row, "drives", "end_ideal_range_km")?,
        start_rated_range_km: optional_decimal(row, "drives", "start_rated_range_km")?,
        end_rated_range_km: optional_decimal(row, "drives", "end_rated_range_km")?,
        start_km: optional_float(row, "drives", "start_km")?,
        end_km: optional_float(row, "drives", "end_km")?,
        distance_km: optional_float(row, "drives", "distance")?,
        duration_min: optional_i16(row, "drives", "duration_min")?.map(i64::from),
        ascent: optional_i16(row, "drives", "ascent")?.map(i64::from),
        descent: optional_i16(row, "drives", "descent")?.map(i64::from),
    })
}

pub(crate) fn decode_position(row: &Row) -> Result<TeslaMatePosition, TeslaMateReaderError> {
    Ok(TeslaMatePosition {
        id: i64::from(required_i32(row, "positions", "id")?),
        car_id: i64::from(required_i16(row, "positions", "car_id")?),
        drive_id: optional_i64(row, "positions", "drive_id")?,
        date_ms: required_timestamp_ms(row, "positions", "date")?,
        latitude: required_decimal(row, "positions", "latitude")?,
        longitude: required_decimal(row, "positions", "longitude")?,
        elevation: optional_i64(row, "positions", "elevation")?,
        speed: optional_i64(row, "positions", "speed")?,
        power: optional_float(row, "positions", "power")?,
        odometer: optional_float(row, "positions", "odometer")?,
        ideal_battery_range_km: optional_decimal(row, "positions", "ideal_battery_range_km")?,
        est_battery_range_km: optional_decimal(row, "positions", "est_battery_range_km")?,
        rated_battery_range_km: optional_decimal(row, "positions", "rated_battery_range_km")?,
        battery_level: optional_i64(row, "positions", "battery_level")?,
        usable_battery_level: optional_i64(row, "positions", "usable_battery_level")?,
        fan_status: optional_i64(row, "positions", "fan_status")?,
        driver_temp_setting: optional_decimal(row, "positions", "driver_temp_setting")?,
        passenger_temp_setting: optional_decimal(row, "positions", "passenger_temp_setting")?,
        is_climate_on: optional_bool(row, "positions", "is_climate_on")?,
        is_rear_defroster_on: optional_bool(row, "positions", "is_rear_defroster_on")?,
        is_front_defroster_on: optional_bool(row, "positions", "is_front_defroster_on")?,
        outside_temp: optional_decimal(row, "positions", "outside_temp")?,
        inside_temp: optional_decimal(row, "positions", "inside_temp")?,
        battery_heater: optional_bool(row, "positions", "battery_heater")?,
        battery_heater_on: optional_bool(row, "positions", "battery_heater_on")?,
        battery_heater_no_power: optional_bool(row, "positions", "battery_heater_no_power")?,
        tpms_pressure_fl: optional_decimal(row, "positions", "tpms_pressure_fl")?,
        tpms_pressure_fr: optional_decimal(row, "positions", "tpms_pressure_fr")?,
        tpms_pressure_rl: optional_decimal(row, "positions", "tpms_pressure_rl")?,
        tpms_pressure_rr: optional_decimal(row, "positions", "tpms_pressure_rr")?,
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
        charge_energy_used_kwh: optional_decimal(row, "charging_processes", "charge_energy_used")?,
        start_ideal_range_km: optional_decimal(row, "charging_processes", "start_ideal_range_km")?,
        end_ideal_range_km: optional_decimal(row, "charging_processes", "end_ideal_range_km")?,
        start_battery_level: optional_i16(row, "charging_processes", "start_battery_level")?
            .map(i64::from),
        end_battery_level: optional_i16(row, "charging_processes", "end_battery_level")?
            .map(i64::from),
        duration_min: optional_i16(row, "charging_processes", "duration_min")?.map(i64::from),
        outside_temp_avg: optional_decimal(row, "charging_processes", "outside_temp_avg")?,
        cost: optional_decimal(row, "charging_processes", "cost")?,
        start_rated_range_km: optional_decimal(row, "charging_processes", "start_rated_range_km")?,
        end_rated_range_km: optional_decimal(row, "charging_processes", "end_rated_range_km")?,
    })
}

pub(crate) fn decode_charge(row: &Row) -> Result<TeslaMateCharge, TeslaMateReaderError> {
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
        latitude: row
            .try_get("latitude")
            .map_err(|source| cell("geofences", "latitude", source))?,
        longitude: row
            .try_get("longitude")
            .map_err(|source| cell("geofences", "longitude", source))?,
        radius_m: row
            .try_get("radius_m")
            .map_err(|source| cell("geofences", "radius", source))?,
        billing_type: optional_text(row, "geofences", "billing_type")?
            .map(|value| value.parse::<GeofenceBillingType>())
            .transpose()
            .map_err(|_| TeslaMateReaderError::InvalidGeofenceBillingType)?,
        cost_per_unit: optional_float(row, "geofences", "cost_per_unit")?,
        session_fee: optional_float(row, "geofences", "session_fee")?,
    })
}

fn decode_state(row: &Row) -> Result<TeslaMateState, TeslaMateReaderError> {
    let state: TeslaMateStateStatus = row
        .try_get("state")
        .map_err(|source| cell("states", "state", source))?;
    Ok(TeslaMateState {
        id: i64::from(required_i32(row, "states", "id")?),
        car_id: i64::from(required_i16(row, "states", "car_id")?),
        state: state.0,
        start_date_ms: required_timestamp_ms(row, "states", "start_date")?,
        end_date_ms: optional_timestamp_ms(row, "states", "end_date")?,
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

struct TeslaMateStateStatus(String);

impl<'a> FromSql<'a> for TeslaMateStateStatus {
    fn from_sql(
        _ty: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(std::str::from_utf8(raw)?.to_owned()))
    }

    fn accepts(ty: &Type) -> bool {
        ty.name() == "states_status"
    }
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

fn optional_smallint(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i16>, TeslaMateReaderError> {
    optional_i32(row, table, column)?
        .map(|value| narrow_smallint(value, table, column))
        .transpose()
}

fn narrow_smallint(
    value: i32,
    table: &'static str,
    column: &'static str,
) -> Result<i16, TeslaMateReaderError> {
    i16::try_from(value).map_err(|_| TeslaMateReaderError::IntegerOutOfRange { table, column })
}

fn optional_i64(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i64>, TeslaMateReaderError> {
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
    #[error("TeslaMate source has no legacy OAuth token pair")]
    LegacyTokenPairMissing,
    #[error("TeslaMate source has more than one legacy OAuth token pair")]
    LegacyTokenPairAmbiguous,
    #[error("TeslaMate legacy OAuth token pair is empty")]
    LegacyTokenPairEmpty,
    #[error("TeslaMate PostgreSQL connect timeout must be greater than zero")]
    InvalidConnectTimeout,
    #[error("TeslaMate PostgreSQL page size must be in 1..=10000")]
    InvalidPageSize,
    #[error("TeslaMate PostgreSQL maximum rows must be greater than zero")]
    InvalidMaximumRows,
    #[error("TeslaMate PostgreSQL parallel COPY lanes must be in 1..=8")]
    InvalidParallelCopyLanes,
    #[error("could not load a usable native TLS trust store")]
    NativeTrustStoreUnavailable,
    #[error("TeslaMate PostgreSQL connection timed out")]
    ConnectTimedOut,
    #[error("TeslaMate PostgreSQL COPY statement timeout must be between 60 seconds and 24 hours")]
    InvalidCopyStatementTimeout,
    #[error("TeslaMate schema has no migration version")]
    MissingMigrationVersion,
    #[error("TeslaMate exported an invalid PostgreSQL snapshot identifier")]
    InvalidExportedSnapshot,
    #[error("TeslaMate {table} page did not advance its keyset cursor")]
    NonProgressingPage { table: &'static str },
    #[error("TeslaMate source exceeds the {maximum} row import limit")]
    MaximumRowsExceeded { maximum: usize },
    #[error("TeslaMate has more than one open row in {table}")]
    MultipleOpenRows { table: &'static str },
    #[error("TeslaMate open-session projection validation failed: {0}")]
    OpenSessionProjection(#[source] TeslaMateProjectionError),
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
    #[error("TeslaMate {table}.{column} integer cannot be represented as a smallint")]
    IntegerOutOfRange {
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
    #[error("TeslaMate geofence billing type is invalid")]
    InvalidGeofenceBillingType,
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
        assert!(matches!(
            TeslaMateReadLimits {
                parallel_copy_lanes: 0,
                ..TeslaMateReadLimits::default()
            }
            .validate(),
            Err(TeslaMateReaderError::InvalidParallelCopyLanes)
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
    fn exported_snapshot_ids_are_strictly_safe_for_future_lane_sql() {
        assert_eq!(
            validate_exported_snapshot_id("000003A0-1".to_owned()).expect("snapshot ID"),
            "000003A0-1"
        );
        assert!(validate_exported_snapshot_id("000003A0-1-2".to_owned()).is_ok());
        for invalid in ["", "000003A0", "000003A0-'; SELECT 1", "-1"] {
            assert!(matches!(
                validate_exported_snapshot_id(invalid.to_owned()),
                Err(TeslaMateReaderError::InvalidExportedSnapshot)
            ));
        }
    }

    #[test]
    fn capture_lane_sql_accepts_only_validated_postgres_snapshot_ids() {
        assert_eq!(
            snapshot_import_sql("000003A0-1").expect("snapshot SQL"),
            "SET TRANSACTION SNAPSHOT '000003A0-1'"
        );
        assert!(matches!(
            snapshot_import_sql("000003A0-1'; SELECT 1"),
            Err(TeslaMateReaderError::InvalidExportedSnapshot)
        ));
    }

    #[test]
    fn binary_copy_statements_are_fixed_full_projection_queries() {
        for table in SourceTable::ALL {
            let sql = binary_copy_sql(table, 17);
            assert!(sql.starts_with("COPY ("));
            assert!(sql.ends_with("TO STDOUT WITH (FORMAT BINARY)"));
            assert!(sql.contains("17"));
            assert!(sql.contains("LIMIT ALL"));
            assert!(!sql.contains('$'));
            assert!(!sql.contains(';'));
        }
    }

    #[test]
    fn related_position_copy_statement_wraps_the_reviewed_positions_projection() {
        let sql = related_positions_binary_copy_sql(7, &[3, 11]);
        assert!(sql.starts_with("COPY (SELECT \"related\".* FROM (SELECT"));
        assert!(sql.contains("FROM \"public\".\"positions\" AS \"source\""));
        assert!(sql.contains("\"source\".\"car_id\" = 7"));
        assert!(sql.contains("\"related\".\"id\" = ANY(ARRAY[3,11]::int4[])"));
        assert!(sql.ends_with("TO STDOUT WITH (FORMAT BINARY)"));
        assert!(!sql.contains('$'));
        assert!(!sql.contains(';'));
    }

    #[test]
    fn open_position_copy_branches_are_fixed_and_do_not_use_exists() {
        let standalone = open_position_branch_copy_sql(7, OpenPositionBranch::Standalone);
        assert!(standalone.contains("\"branch\".\"drive_id\" IS NULL"));
        assert!(!standalone.contains("OR EXISTS"));
        assert!(standalone.ends_with("TO STDOUT WITH (FORMAT BINARY)"));
        assert!(!standalone.contains('$'));
        assert!(!standalone.contains(';'));

        let active = open_position_branch_copy_sql(7, OpenPositionBranch::ActiveDrive(42));
        assert!(active.contains("\"branch\".\"drive_id\" = 42"));
        assert!(!active.contains("OR EXISTS"));
        assert!(!active.contains('$'));
        assert!(!active.contains(';'));
    }

    #[test]
    fn car_id_discovery_query_is_qualified_and_stably_ordered() {
        assert!(CAR_IDS_SQL.contains("FROM \"public\".\"cars\""));
        assert!(CAR_IDS_SQL.contains("ORDER BY \"source\".\"id\" ASC"));
        assert!(!CAR_IDS_SQL.contains('$'));
    }

    #[test]
    fn open_queries_are_scoped_to_active_rows_and_keep_standalone_positions() {
        let drives = open_rows_sql(SourceTable::Drives, "\"source\".\"end_date\" IS NULL");
        let charges = open_rows_sql(SourceTable::Charges, "\"process\".\"end_date\" IS NULL");
        let states = open_rows_sql(SourceTable::States, "\"source\".\"end_date\" IS NULL");
        for sql in [&drives, &charges, &states] {
            assert!(sql.contains("\"public\""));
            assert!(sql.contains("\"source\".\"id\" > $1"));
            assert!(
                sql.contains("\"source\".\"car_id\" = $3")
                    || sql.contains("\"process\".\"car_id\" = $3")
            );
            assert!(sql.contains("ORDER BY \"source\".\"id\" ASC"));
        }
        let standalone = open_position_branch_copy_sql(7, OpenPositionBranch::Standalone);
        assert!(standalone.contains("\"branch\".\"drive_id\" IS NULL"));
        assert!(!standalone.contains("OR EXISTS"));
    }

    #[test]
    fn car_binary_copy_types_match_the_reviewed_projection_width() {
        assert_eq!(
            car_copy_types().len(),
            projection(SourceTable::Cars).columns.len()
        );
        assert_eq!(car_copy_types()[0], Type::INT2);
        assert_eq!(car_copy_types()[1], Type::INT8);
        assert_eq!(car_copy_types()[6], Type::FLOAT8);
        assert_eq!(car_copy_types()[7], Type::INT4);
        assert_eq!(car_copy_types()[8], Type::INT4);
    }

    #[test]
    fn legacy_car_settings_integer_values_are_range_checked_before_narrowing() {
        assert_eq!(
            narrow_smallint(i16::MIN as i32, "car_settings", "suspend_min")
                .expect("i16 minimum is representable"),
            i16::MIN
        );
        assert_eq!(
            narrow_smallint(i16::MAX as i32, "car_settings", "suspend_min")
                .expect("i16 maximum is representable"),
            i16::MAX
        );
        assert!(matches!(
            narrow_smallint(i32::from(i16::MAX) + 1, "car_settings", "suspend_min"),
            Err(TeslaMateReaderError::IntegerOutOfRange {
                table: "car_settings",
                column: "suspend_min",
            })
        ));
    }

    #[test]
    fn drive_binary_copy_types_match_the_reviewed_projection_width() {
        assert_eq!(
            drive_copy_types().len(),
            projection(SourceTable::Drives).columns.len()
        );
        assert_eq!(drive_copy_types()[0], Type::INT4);
        assert_eq!(drive_copy_types()[10], Type::NUMERIC);
        assert_eq!(drive_copy_types()[19], Type::FLOAT8);
    }

    #[test]
    fn position_binary_copy_types_match_the_reviewed_projection_width() {
        assert_eq!(
            position_copy_types().len(),
            projection(SourceTable::Positions).columns.len()
        );
        assert_eq!(position_copy_types()[3], Type::TIMESTAMP);
        assert_eq!(position_copy_types()[4], Type::NUMERIC);
        assert_eq!(position_copy_types()[2], Type::INT8);
        assert_eq!(position_copy_types()[6], Type::INT8);
        assert_eq!(position_copy_types()[7], Type::INT8);
        assert_eq!(position_copy_types()[8], Type::FLOAT8);
        assert_eq!(position_copy_types()[9], Type::FLOAT8);
        assert_eq!(position_copy_types()[13], Type::INT8);
        assert_eq!(position_copy_types()[14], Type::INT8);
        assert_eq!(position_copy_types()[20], Type::INT8);
        assert_eq!(position_copy_types()[23], Type::BOOL);
    }

    #[test]
    fn charging_process_binary_copy_types_match_the_reviewed_projection_width() {
        assert_eq!(
            charging_process_copy_types().len(),
            projection(SourceTable::ChargingProcesses).columns.len()
        );
        assert_eq!(charging_process_copy_types()[5], Type::TIMESTAMP);
        assert_eq!(charging_process_copy_types()[7], Type::NUMERIC);
    }

    #[test]
    fn charge_binary_copy_types_match_the_reviewed_projection_width() {
        assert_eq!(
            charge_copy_types().len(),
            projection(SourceTable::Charges).columns.len()
        );
        assert_eq!(charge_copy_types()[2], Type::TIMESTAMP);
        assert_eq!(charge_copy_types()[8], Type::NUMERIC);
        assert_eq!(charge_copy_types()[14], Type::TEXT);
    }

    #[test]
    fn address_binary_copy_types_match_the_reviewed_projection_width() {
        assert_eq!(
            address_copy_types().len(),
            projection(SourceTable::Addresses).columns.len()
        );
        assert_eq!(address_copy_types(), &[Type::INT4, Type::TEXT, Type::TEXT]);
    }

    #[test]
    fn geofence_geometry_projection_contains_required_columns() {
        assert!(GEOFENCE_GEOMETRY_SQL.contains("latitude"));
        assert!(GEOFENCE_GEOMETRY_SQL.contains("longitude"));
        assert!(GEOFENCE_GEOMETRY_SQL.contains("radius_m"));
    }

    #[test]
    fn update_binary_copy_types_match_the_reviewed_projection_width() {
        assert_eq!(
            update_copy_types().len(),
            projection(SourceTable::Updates).columns.len()
        );
        assert_eq!(
            update_copy_types(),
            &[
                Type::INT4,
                Type::INT2,
                Type::TIMESTAMP,
                Type::TIMESTAMP,
                Type::TEXT
            ]
        );
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
            vid: Some(99),
            vin: Some("5YJTESTVIN1234567".to_owned()),
            name: Some("Road car".to_owned()),
            model: Some("Model 3".to_owned()),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: Some(0.145),
            settings: Default::default(),
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

    #[tokio::test]
    async fn native_postgres_binary_car_copy_round_trips_when_configured() {
        let Ok(url) = std::env::var("TESLATLAS_HUB_TEST_POSTGRES_URL") else {
            return;
        };
        let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
        let connection_task = tokio::spawn(async move { connection.await.expect("connection") });
        client.batch_execute(
            "CREATE TABLE cars (id smallint PRIMARY KEY, eid bigint NOT NULL, vid bigint NOT NULL, vin text, name text, model text, efficiency double precision, trim_badging text, marketing_name text, exterior_color text, wheel_type text, spoiler_type text, display_priority smallint NOT NULL, inserted_at timestamp NOT NULL, updated_at timestamp NOT NULL); INSERT INTO cars VALUES (1, 88, 99, '5YJTESTVIN1234567', 'Road car', 'Model 3', 0.145, NULL, NULL, NULL, NULL, NULL, 0, '2026-01-01', '2026-01-01'), (2, 89, 100, '5YJOTHER12345678', 'Other', 'Model Y', NULL, NULL, NULL, NULL, NULL, NULL, 0, '2026-01-01', '2026-01-01');"
        ).await.expect("seed source");
        client
            .batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY;")
            .await
            .expect("read only snapshot");
        let snapshot_id: String = client
            .query_one("SELECT pg_export_snapshot() AS snapshot_id", &[])
            .await
            .expect("export snapshot")
            .try_get("snapshot_id")
            .expect("snapshot ID");
        let mut retained = 0;
        let cars = read_cars(
            &client,
            1,
            TeslaMateReadLimits {
                maximum_rows: 2,
                ..TeslaMateReadLimits::default()
            },
            &mut retained,
        )
        .await
        .expect("binary copy");
        assert_eq!(retained, 1);
        assert_eq!(cars.len(), 1);
        assert_eq!(cars[0].id, 1);
        assert_eq!(cars[0].eid, 88);
        assert_eq!(cars[0].name.as_deref(), Some("Road car"));
        let (first_lane, second_lane, third_lane) = tokio::try_join!(
            native_car_copy_lane(&url, &snapshot_id),
            native_car_copy_lane(&url, &snapshot_id),
            native_car_copy_lane(&url, &snapshot_id),
        )
        .expect("parallel lanes");
        assert_eq!(first_lane, cars);
        assert_eq!(second_lane, cars);
        assert_eq!(third_lane, cars);
        assert!(client
            .execute("INSERT INTO cars (id, eid, vid, display_priority, inserted_at, updated_at) VALUES (3, 90, 101, 0, '2026-01-01', '2026-01-01')", &[])
            .await
            .is_err());
        client.batch_execute("ROLLBACK").await.expect("rollback");
        drop(client);
        connection_task.await.expect("task");
    }

    async fn native_car_copy_lane(
        url: &str,
        snapshot_id: &str,
    ) -> Result<Vec<TeslaMateCar>, TeslaMateReaderError> {
        let (lane, connection) = tokio_postgres::connect(url, NoTls).await?;
        let task = tokio::spawn(connection);
        lane.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY;")
            .await?;
        lane.batch_execute(&snapshot_import_sql(snapshot_id)?)
            .await?;
        let mut retained = 0;
        let result = read_cars(
            &lane,
            1,
            TeslaMateReadLimits {
                maximum_rows: 2,
                ..TeslaMateReadLimits::default()
            },
            &mut retained,
        )
        .await;
        let rollback = lane.batch_execute("ROLLBACK").await;
        drop(lane);
        let _ = task.await;
        let rows = result?;
        rollback?;
        Ok(rows)
    }
}
