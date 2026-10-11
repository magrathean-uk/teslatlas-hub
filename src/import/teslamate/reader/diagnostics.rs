// SPDX-License-Identifier: AGPL-3.0-only

/// Forbid or re-allow TeslaMate PostgreSQL connect/query (R06 handoff).
pub fn set_postgres_source_queries_allowed(allowed: bool) {
    POSTGRES_SOURCE_QUERIES_ALLOWED.store(allowed, Ordering::SeqCst);
}

/// Whether TeslaMate PostgreSQL connects are currently permitted.
pub fn postgres_source_queries_allowed() -> bool {
    POSTGRES_SOURCE_QUERIES_ALLOWED.load(Ordering::SeqCst)
}

/// Largest raw size of one retained source row (the whole PostgreSQL DataRow
/// body, or the summed field lengths of a COPY row), further capped by the
/// page budget. The pinned TeslaMate schema has no column near this: the
/// widest bounded text is `addresses.display_name varchar(512)`, the
/// unbounded `cars` and `settings` texts are short names, and the legacy token
/// ciphertexts are capped at 16 KiB by the length probe that precedes them.
pub(crate) const MAX_SOURCE_ROW_BYTES: usize = 16 * 1024 * 1024;

/// Ceiling of the byte budget for one page (or one whole collected read).
pub(crate) const MAX_SOURCE_PAGE_BYTES: usize = 64 * 1024 * 1024;

/// Floor of the byte budget for one page, so a small `maximum_stage_bytes`
/// never undercuts a legitimate page.
pub(crate) const MIN_SOURCE_PAGE_BYTES: usize = 1024 * 1024;

/// How long a read that went over a budget waits for its cancel request to be
/// delivered to the server before it reports the budget error anyway.
pub(crate) const SOURCE_CANCEL_TIMEOUT: Duration = Duration::from_secs(2);

/// Resource caps for one read-only source snapshot. The bounds are checked
/// before a row is retained, so a hostile or unexpectedly huge selected-car
/// history cannot grow an import without limit. Every source query is scoped
/// to that selected car before pagination. The staged producer retains the
/// same query and validation contract without materialising this bounded
/// capture as one history vector.
///
/// # Decode budgets
///
/// Every production PostgreSQL read under `import::teslamate` that collects
/// rows into memory goes through one bounded reader (`query_bounded`, or the
/// COPY equivalent `open_bounded_copy`). Rows are streamed and three budgets
/// are checked, in this order, before a row is retained or decoded; the first
/// excess ends the read with
/// [`TeslaMateReaderError::SourceReadBudgetExceeded`] naming the table and the
/// limit, never a value.
///
/// * Rows: the query's own `LIMIT`. A keyset page is `page_size` rows; the
///   settings singleton and the token pair are `LIMIT 2` (one row plus the
///   lookahead that detects ambiguity), the single-car reads `LIMIT 1`. A
///   query without a `LIMIT` gets `LIMIT cap + 1` and fails at row cap + 1:
///   4096 migration rows, 65536 schema-column rows and 4096 enum labels, and
///   for the COPY readers the rows left under `maximum_rows`.
/// * Bytes per row: `min(page budget, 16 MiB)`.
/// * Bytes per page: `min(64 MiB, max(1 MiB, maximum_stage_bytes))`; the
///   fixed-size probes and token reads, which have no limits value, use
///   64 MiB. A COPY reader collects one table in a single pass, so that whole
///   collection is one page.
///
/// Row sizes are measured with `Row::raw_size_bytes`, or for COPY rows from the
/// field byte lengths. The driver buffers one DataRow before the reader sees
/// it, so these budgets bound what is retained and the cumulative size of a
/// read, not the transient buffer of the first oversized row. Dropping a
/// stream does not stop the statement behind it (the driver keeps discarding
/// its output), so on the first excess the read also sends the PostgreSQL
/// cancel request of its connection before it reports the error. That request
/// is best effort and its failure is logged, not reported; the output the
/// server has already queued is still bounded by the query's `LIMIT`.
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
            page_size: 10_000,
            maximum_rows: 20_000_000,
            maximum_stage_bytes: 4 * 1024 * 1024 * 1024,
            minimum_free_bytes: TeslaMateStageLimits::default().minimum_free_bytes,
            parallel_copy_lanes: 4,
        }
    }
}

impl TeslaMateReadLimits {
    /// Byte budget for one page: `min(64 MiB, max(1 MiB, maximum_stage_bytes))`.
    pub(crate) fn page_byte_budget(&self) -> usize {
        usize::try_from(self.maximum_stage_bytes)
            .unwrap_or(usize::MAX)
            .clamp(MIN_SOURCE_PAGE_BYTES, MAX_SOURCE_PAGE_BYTES)
    }

    /// Budget of a read that returns at most `max_rows` rows.
    pub(crate) fn budget_for_rows(&self, max_rows: usize) -> SourceReadBudget {
        SourceReadBudget::new(max_rows, self.page_byte_budget())
    }

    /// Budget of a `LIMIT 1` read of one row.
    pub(crate) fn singleton_budget(&self) -> SourceReadBudget {
        self.budget_for_rows(1)
    }

    /// Budget of a `LIMIT 2` read: one row plus the lookahead row that lets the
    /// caller report ambiguity, so a second row is not a budget error.
    pub(crate) fn ambiguity_budget(&self) -> SourceReadBudget {
        self.budget_for_rows(1).with_lookahead()
    }

    /// Budget of one keyset page of `LIMIT page_size`.
    pub(crate) fn page_budget(&self) -> SourceReadBudget {
        self.budget_for_rows(usize::try_from(self.page_size).unwrap_or(0).max(1))
    }

    /// Budget of a COPY reader, which collects one table in a single pass.
    /// Its row count is capped separately by `retain_row` and the query's
    /// `LIMIT`, so only the byte budgets apply here.
    pub(crate) fn copy_budget(&self) -> SourceReadBudget {
        self.budget_for_rows(usize::MAX)
    }

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
    #[serde(rename = "observedMigrationCount")]
    pub observed_migration_count: usize,
    #[serde(rename = "minimumSupportedMigrationVersion")]
    pub minimum_supported_migration_version: i64,
    #[serde(rename = "maximumValidatedMigrationVersion")]
    pub maximum_validated_migration_version: i64,
    #[serde(rename = "pinnedSourceRevision")]
    pub pinned_source_revision: &'static str,
    #[serde(rename = "pinnedMigrationSetSha256")]
    pub pinned_migration_set_sha256: &'static str,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateConnectionDiagnostics {
    pub current_user: String,
    pub database: String,
    pub server_address: String,
    pub server_port: u16,
    pub postmaster_start_epoch_seconds: i64,
    pub transaction_read_only: bool,
    pub private_schema_usage: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateSelectedCarDiagnostics {
    pub id: i64,
    pub name: Option<String>,
    pub model: Option<String>,
    pub vin_present: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateOpenSessionCounts {
    pub drives: usize,
    pub charging_processes: usize,
    pub states: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateSelectedCarCounts {
    pub drives: u64,
    pub positions: u64,
    pub charging_processes: u64,
    pub charges: u64,
    pub states: u64,
    pub updates: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateLegacyTokenPairDiagnostics {
    pub relation: String,
    pub access_ciphertext_bytes: u64,
    pub refresh_ciphertext_bytes: u64,
}

/// Read-only TeslaMate diagnosis used by `teslamate-check`. This never writes
/// the source and never reads token ciphertext.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateCheckSnapshot {
    pub schema: TeslaMateSchemaInfo,
    pub connection: TeslaMateConnectionDiagnostics,
    pub selected_car: TeslaMateSelectedCarDiagnostics,
    pub open_sessions: TeslaMateOpenSessionCounts,
    pub selected_car_counts: TeslaMateSelectedCarCounts,
    pub source_totals: TeslaMateSourceTotals,
    pub source_tokens_relation_present: Option<bool>,
    pub legacy_token_pair: Option<TeslaMateLegacyTokenPairDiagnostics>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateSourceTotals {
    pub cars: u64,
    pub drives: u64,
    pub positions: u64,
    pub charging_processes: u64,
    pub charges: u64,
    pub states: u64,
    pub updates: u64,
    pub schema_migrations: u64,
}

impl TeslaMateCheckSnapshot {
    pub fn log(&self) {
        tracing::info!(
            user = %self.connection.current_user,
            database = %self.connection.database,
            server_address = %self.connection.server_address,
            server_port = self.connection.server_port,
            transaction_read_only = self.connection.transaction_read_only,
            private_schema_usage = self.connection.private_schema_usage,
            "TeslaMate PostgreSQL connection (read-only snapshot)"
        );
        tracing::info!(
            observed_migration_version = self.schema.observed_migration_version,
            observed_migration_count = self.schema.observed_migration_count,
            pinned_source_revision = self.schema.pinned_source_revision,
            "TeslaMate schema"
        );
        tracing::info!(
            car_id = self.selected_car.id,
            name = self.selected_car.name.as_deref(),
            model = self.selected_car.model.as_deref(),
            vin_present = self.selected_car.vin_present,
            open_drives = self.open_sessions.drives,
            open_charges = self.open_sessions.charging_processes,
            open_states = self.open_sessions.states,
            selected_drives = self.selected_car_counts.drives,
            selected_positions = self.selected_car_counts.positions,
            selected_charges = self.selected_car_counts.charges,
            source_positions = self.source_totals.positions,
            token_checks_performed = self.legacy_token_pair.is_some(),
            "TeslaMate selected car (source is not mutated)"
        );
    }
}

const SELECTED_CAR_COUNT_SQL: &str = r#"
SELECT
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
  ) AS "charges",
  (SELECT COUNT(*)::bigint FROM "public"."states" WHERE "car_id" = $1) AS "states",
  (SELECT COUNT(*)::bigint FROM "public"."updates" WHERE "car_id" = $1) AS "updates"
"#;

/// Validate one selected TeslaMate car against the exact pinned source
/// contract without creating or opening any Hub target state. The schema,
/// connection witness, selected-car probe, and table counts run in one
/// read-only, repeatable-read source transaction. Token ciphertext is never
/// read.
pub async fn check_teslamate_compatibility(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateCheckSnapshot, TeslaMateReaderError> {
    check_teslamate_compatibility_with_token_scope(source, password, selected_car_id, limits, true)
        .await
}

/// Check public history without probing the private TeslaMate token schema.
pub async fn check_teslamate_history_compatibility(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateCheckSnapshot, TeslaMateReaderError> {
    check_teslamate_compatibility_with_token_scope(source, password, selected_car_id, limits, false)
        .await
}

async fn check_teslamate_compatibility_with_token_scope(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
    inspect_tokens: bool,
) -> Result<TeslaMateCheckSnapshot, TeslaMateReaderError> {
    tracing::info!(
        host = source.host(),
        port = source.port(),
        database = source.database_name(),
        user = source.user(),
        selected_car_id,
        "connecting to TeslaMate PostgreSQL for a read-only compatibility check"
    );
    let (session, selected_car_id_i16, schema) =
        open_snapshot_session_with_schema(source, password, selected_car_id, limits).await?;
    let mut retained_rows = 0_usize;
    let result = async {
        let witness_row = session
            .client()
            .query_one(LIVE_SOURCE_WITNESS_SQL, &[])
            .await?;
        let connection = parse_live_source_connection(&witness_row)?;
        if !connection.transaction_read_only {
            return Err(TeslaMateReaderError::WitnessTransactionWritable);
        }
        let source_totals = TeslaMateSourceTotals {
            cars: source_witness_count(&witness_row, "cars")?,
            drives: source_witness_count(&witness_row, "drives")?,
            positions: source_witness_count(&witness_row, "positions")?,
            charging_processes: source_witness_count(&witness_row, "charging_processes")?,
            charges: source_witness_count(&witness_row, "charges")?,
            states: source_witness_count(&witness_row, "states")?,
            updates: source_witness_count(&witness_row, "updates")?,
            schema_migrations: source_witness_count(&witness_row, "schema_migrations")?,
        };
        let cars = read_cars(
            session.client(),
            selected_car_id_i16,
            limits,
            &mut retained_rows,
        )
        .await?;
        if cars.is_empty() {
            return Err(TeslaMateReaderError::SelectedCarMissing { selected_car_id });
        }
        let car = &cars[0];
        let drives = read_open_drives(
            session.client(),
            selected_car_id_i16,
            limits,
            &mut retained_rows,
        )
        .await?;
        let processes = read_open_charging_processes(
            session.client(),
            selected_car_id_i16,
            limits,
            &mut retained_rows,
        )
        .await?;
        let states = read_open_states(
            session.client(),
            selected_car_id_i16,
            limits,
            &mut retained_rows,
        )
        .await?;
        let count_row = session
            .client()
            .query_one(SELECTED_CAR_COUNT_SQL, &[&selected_car_id_i16])
            .await?;
        let (source_tokens_relation_present, legacy_token_pair) = if inspect_tokens {
            (Some(true), Some(inspect_legacy_token_pair_in_client(session.client()).await?))
        } else {
            (None, None)
        };
        Ok(TeslaMateCheckSnapshot {
            schema,
            connection,
            selected_car: TeslaMateSelectedCarDiagnostics {
                id: car.id,
                name: car.name.clone(),
                model: car.model.clone(),
                vin_present: car.vin.as_ref().is_some_and(|vin| !vin.trim().is_empty()),
            },
            open_sessions: TeslaMateOpenSessionCounts {
                drives: drives.len(),
                charging_processes: processes.len(),
                states: states.len(),
            },
            selected_car_counts: TeslaMateSelectedCarCounts {
                drives: source_witness_count(&count_row, "drives")?,
                positions: source_witness_count(&count_row, "positions")?,
                charging_processes: source_witness_count(&count_row, "charging_processes")?,
                charges: source_witness_count(&count_row, "charges")?,
                states: source_witness_count(&count_row, "states")?,
                updates: source_witness_count(&count_row, "updates")?,
            },
            source_totals,
            source_tokens_relation_present,
            legacy_token_pair,
        })
    }
    .await;
    let finish = session.finish().await;
    match (result, finish) {
        (Err(error), _) => Err(error),
        (Ok(value), Ok(())) => {
            value.log();
            Ok(value)
        }
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Non-secret identity and exact table counts witnessed from one validated,
/// read-only, repeatable-read TeslaMate PostgreSQL transaction. This never
/// reads `private.tokens` (or any private relation contents).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateLiveSourceWitness {
    pub current_user: String,
    pub database: String,
    pub server_address: String,
    pub server_port: u16,
    pub postmaster_start_epoch_seconds: i64,
    pub transaction_read_only: bool,
    pub private_schema_usage: bool,
    pub cars: u64,
    pub drives: u64,
    pub positions: u64,
    pub charging_processes: u64,
    pub charges: u64,
    pub states: u64,
    pub updates: u64,
    pub schema_migrations: u64,
}

const LIVE_SOURCE_WITNESS_SQL: &str = r#"
SELECT
  current_user::text AS "current_user",
  current_database()::text AS "database",
  pg_catalog.host(pg_catalog.inet_server_addr())::text AS "server_address",
  pg_catalog.inet_server_port()::integer AS "server_port",
  floor(extract(epoch FROM pg_catalog.pg_postmaster_start_time()))::bigint AS "postmaster_start_epoch_seconds",
  pg_catalog.current_setting('transaction_read_only')::boolean AS "transaction_read_only",
  pg_catalog.has_schema_privilege(current_user, 'private', 'USAGE') AS "private_schema_usage",
  (SELECT COUNT(*)::bigint FROM "public"."cars") AS "cars",
  (SELECT COUNT(*)::bigint FROM "public"."drives") AS "drives",
  (SELECT COUNT(*)::bigint FROM "public"."positions") AS "positions",
  (SELECT COUNT(*)::bigint FROM "public"."charging_processes") AS "charging_processes",
  (SELECT COUNT(*)::bigint FROM "public"."charges") AS "charges",
  (SELECT COUNT(*)::bigint FROM "public"."states") AS "states",
  (SELECT COUNT(*)::bigint FROM "public"."updates") AS "updates",
  (SELECT COUNT(*)::bigint FROM "public"."schema_migrations") AS "schema_migrations"
"#;

/// Inspect the live source without changing it. The source is first admitted
/// through the normal schema validation path, then all identity facts and
/// counts are read from the same repeatable-read transaction.
pub async fn inspect_teslamate_live_source(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateLiveSourceWitness, TeslaMateReaderError> {
    limits.validate()?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let session = TeslaMateSnapshotSession::new(client, connection_task);
    let result = async {
        prepare_read_only_snapshot(session.client(), source, limits).await?;
        let row = session
            .client()
            .query_one(LIVE_SOURCE_WITNESS_SQL, &[])
            .await?;
        let witness = parse_live_source_witness(&row)?;
        if witness.private_schema_usage {
            return Err(TeslaMateReaderError::PrivateSchemaUsageGranted);
        }
        if !witness.transaction_read_only {
            return Err(TeslaMateReaderError::WitnessTransactionWritable);
        }
        Ok(witness)
    }
    .await;
    let finish = session.finish().await;
    match (result, finish) {
        (Err(error), _) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn parse_live_source_connection(
    row: &Row,
) -> Result<TeslaMateConnectionDiagnostics, TeslaMateReaderError> {
    let server_address: Option<String> = row.try_get("server_address")?;
    let server_port: Option<i32> = row.try_get("server_port")?;
    let server_port =
        server_port.and_then(|port| u16::try_from(port).ok().filter(|port| *port > 0));
    Ok(TeslaMateConnectionDiagnostics {
        current_user: row.try_get("current_user")?,
        database: row.try_get("database")?,
        server_address: server_address.unwrap_or_else(|| "local".to_owned()),
        server_port: server_port.unwrap_or(0),
        postmaster_start_epoch_seconds: row.try_get("postmaster_start_epoch_seconds")?,
        transaction_read_only: row.try_get("transaction_read_only")?,
        private_schema_usage: row.try_get("private_schema_usage")?,
    })
}

fn parse_live_source_witness(
    row: &Row,
) -> Result<TeslaMateLiveSourceWitness, TeslaMateReaderError> {
    let server_address: Option<String> = row.try_get("server_address")?;
    let server_address = server_address.ok_or(TeslaMateReaderError::MissingServerAddress)?;
    let server_port: i32 = row.try_get("server_port")?;
    let server_port = u16::try_from(server_port)
        .map_err(|_| TeslaMateReaderError::InvalidServerPort { port: server_port })?;
    if server_port == 0 {
        return Err(TeslaMateReaderError::InvalidServerPort { port: 0 });
    }
    Ok(TeslaMateLiveSourceWitness {
        current_user: row.try_get("current_user")?,
        database: row.try_get("database")?,
        server_address,
        server_port,
        postmaster_start_epoch_seconds: row.try_get("postmaster_start_epoch_seconds")?,
        transaction_read_only: row.try_get("transaction_read_only")?,
        private_schema_usage: row.try_get("private_schema_usage")?,
        cars: source_witness_count(row, "cars")?,
        drives: source_witness_count(row, "drives")?,
        positions: source_witness_count(row, "positions")?,
        charging_processes: source_witness_count(row, "charging_processes")?,
        charges: source_witness_count(row, "charges")?,
        states: source_witness_count(row, "states")?,
        updates: source_witness_count(row, "updates")?,
        schema_migrations: source_witness_count(row, "schema_migrations")?,
    })
}

fn source_witness_count(row: &Row, column: &'static str) -> Result<u64, TeslaMateReaderError> {
    let count: i64 = row.try_get(column)?;
    u64::try_from(count).map_err(|_| TeslaMateReaderError::InvalidSourceCount { column, count })
}
