// SPDX-License-Identifier: AGPL-3.0-only

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

fn required_bool(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<bool, TeslaMateReaderError> {
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

/// PostgreSQL's binary `timestamp` wire payload is one signed big-endian i64
/// of microseconds relative to 2000-01-01. Preserve all eight bytes directly:
/// `i64::MIN`/`i64::MAX` are the server's infinity sentinels, not errors or
/// wall-clock values to normalize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PostgresTimestampUs(i64);

impl<'a> FromSql<'a> for PostgresTimestampUs {
    fn from_sql(
        ty: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        if ty != &Type::TIMESTAMP {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected PostgreSQL timestamp",
            )));
        }
        if raw.len() != 8 {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                "PostgreSQL timestamp payload must be eight bytes",
            )));
        }
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(raw);
        Ok(Self(i64::from_be_bytes(bytes)))
    }

    fn accepts(ty: &Type) -> bool {
        ty == &Type::TIMESTAMP
    }
}

#[allow(dead_code)] // reached only from intentionally unlinked candidate readers.
fn required_timestamp_pg_us(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i64, TeslaMateReaderError> {
    let value: PostgresTimestampUs = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    Ok(value.0)
}

#[allow(dead_code)] // reached only from intentionally unlinked candidate readers.
fn required_timestamp_0_pg_us(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i64, TeslaMateReaderError> {
    let value = required_timestamp_pg_us(row, table, column)?;
    validate_timestamp_0_pg_us(value, table, column)?;
    Ok(value)
}

fn validate_timestamp_0_pg_us(
    value: i64,
    table: &'static str,
    column: &'static str,
) -> Result<(), TeslaMateReaderError> {
    let is_infinity = matches!(value, i64::MIN | i64::MAX);
    let is_finite_second = (POSTGRES_TIMESTAMP_FINITE_MIN_US
        ..POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US)
        .contains(&value)
        && value.rem_euclid(1_000_000) == 0;
    if !is_infinity && !is_finite_second {
        return Err(TeslaMateReaderError::TimestampZeroPhysicalDomain { table, column });
    }
    Ok(())
}

#[allow(dead_code)] // reached only from intentionally unlinked candidate readers.
fn optional_timestamp_pg_us(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i64>, TeslaMateReaderError> {
    let value: Option<PostgresTimestampUs> = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    Ok(value.map(|value| value.0))
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
    #[error("TeslaMate migration failed: {primary}; staging cleanup failed ({cleanup})")]
    StageCleanupFailure {
        #[source]
        primary: Box<TeslaMateReaderError>,
        cleanup: TeslaMateStageCleanupFailureKind,
    },
    #[error("TeslaMate PostgreSQL queries are forbidden after Hub handoff")]
    SourceQueriesForbiddenAfterHandoff,
    #[error("TeslaMate source user is required")]
    SourceUserRequired,
    #[error("TeslaMate selected car id must be positive")]
    InvalidSelectedCarId,
    #[error("TeslaMate selected car id exceeds the source smallint domain")]
    SelectedCarIdOutOfRange,
    #[error("TeslaMate selected car {selected_car_id} does not exist in the source")]
    SelectedCarMissing { selected_car_id: i64 },
    #[error(
        "TeslaMate selected car has {drives} open drives, {charges} open charging processes, and {states} open states; import requires at most one of each"
    )]
    AmbiguousOpenSession {
        drives: usize,
        charges: usize,
        states: usize,
    },
    #[error("TeslaMate source settings singleton is missing")]
    SettingsSingletonMissing,
    #[error("TeslaMate source has more than one settings singleton row")]
    SettingsSingletonAmbiguous,
    #[error("TeslaMate source has no legacy OAuth token pair")]
    LegacyTokenPairMissing,
    #[error("TeslaMate source has more than one legacy OAuth token pair")]
    LegacyTokenPairAmbiguous,
    #[error("TeslaMate legacy OAuth token pair is empty")]
    LegacyTokenPairEmpty,
    #[error("TeslaMate {relation}.{column} ciphertext exceeds {maximum} bytes (actual {actual})")]
    LegacyTokenCiphertextTooLarge {
        relation: &'static str,
        column: &'static str,
        maximum: i64,
        actual: i64,
    },
    #[error("TeslaMate source role has USAGE on the private schema")]
    PrivateSchemaUsageGranted,
    #[error("TeslaMate source witness transaction is not read-only")]
    WitnessTransactionWritable,
    #[error("TeslaMate PostgreSQL connection has no server address")]
    MissingServerAddress,
    #[error("TeslaMate PostgreSQL server port is invalid: {port}")]
    InvalidServerPort { port: i32 },
    #[error("TeslaMate source count is invalid for {column}: {count}")]
    InvalidSourceCount { column: &'static str, count: i64 },
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
    #[error("TeslaMate PostgreSQL snapshot rollback timed out")]
    SnapshotRollbackTimedOut,
    #[error("TeslaMate PostgreSQL connection task did not stop before it was aborted")]
    SnapshotConnectionShutdownTimedOut,
    #[error("TeslaMate PostgreSQL connection task did not stop after abort")]
    SnapshotConnectionAbortTimedOut,
    #[error(
        "TeslaMate PostgreSQL connection task failed (cancelled={cancelled}, panicked={panicked})"
    )]
    SnapshotConnectionTaskFailed { cancelled: bool, panicked: bool },
    #[error("TeslaMate schema has no migration version")]
    MissingMigrationVersion,
    #[error("TeslaMate exported an invalid PostgreSQL snapshot identifier")]
    InvalidExportedSnapshot,
    #[error("TeslaMate parallel capture lane panicked")]
    ParallelLanePanicked,
    #[error("TeslaMate parallel capture lane was cancelled")]
    ParallelLaneCancelled,
    #[error("TeslaMate staged row serialization failed: {0}")]
    SerializeStageRow(#[from] serde_json::Error),
    #[error("TeslaMate {table} page did not advance its keyset cursor")]
    NonProgressingPage { table: &'static str },
    #[error("TeslaMate source exceeds the {maximum} row import limit")]
    MaximumRowsExceeded { maximum: usize },
    #[error(
        "TeslaMate compatibility history has {count} positions, exceeding the {maximum} materialization limit"
    )]
    MaterializedHistoryPositionLimitExceeded { maximum: usize, count: usize },
    #[error("TeslaMate open session exceeds the {maximum} position materialization limit")]
    MaterializedOpenPositionLimitExceeded { maximum: usize },
    #[error("TeslaMate open-session projection validation failed: {0}")]
    OpenSessionProjection(#[source] TeslaMateProjectionError),
    #[error("TeslaMate {table}.{column} decimal cannot be represented as a finite f64")]
    DecimalOutOfRange {
        table: &'static str,
        column: &'static str,
    },
    #[error("TeslaMate {table}.{column} decimal does not fit its pinned fixed scale")]
    DecimalFixedScale {
        table: &'static str,
        column: &'static str,
    },
    #[error("TeslaMate {table}.{column} decimal is outside its pinned source range")]
    DecimalFixedRange {
        table: &'static str,
        column: &'static str,
    },
    #[error("TeslaMate {table}.{column} timestamp cannot be represented as epoch milliseconds")]
    TimestampOutOfRange {
        table: &'static str,
        column: &'static str,
    },
    #[error(
        "TeslaMate {table}.{column} timestamp is outside its physical timestamp(0) source domain"
    )]
    TimestampZeroPhysicalDomain {
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
    #[error("TeslaMate states_status enum value is invalid")]
    InvalidStateStatus,
    #[error("TeslaMate settings.{column} enum value is invalid")]
    InvalidSettingsEnum { column: &'static str },
    #[error(transparent)]
    Schema(#[from] crate::teslamate_schema::SchemaCompatibilityError),
    #[error(transparent)]
    Stage(#[from] TeslaMateStageError),
    #[error(transparent)]
    Postgres(#[from] tokio_postgres::Error),
}
