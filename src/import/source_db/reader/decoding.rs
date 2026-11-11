// SPDX-License-Identifier: AGPL-3.0-only

fn required_i16(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i16, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_i16(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i16>, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_smallint(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i16>, SourceDbReaderError> {
    optional_i32(row, table, column)?
        .map(|value| narrow_smallint(value, table, column))
        .transpose()
}

fn narrow_smallint(
    value: i32,
    table: &'static str,
    column: &'static str,
) -> Result<i16, SourceDbReaderError> {
    i16::try_from(value).map_err(|_| SourceDbReaderError::IntegerOutOfRange { table, column })
}

fn optional_i64(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i64>, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn required_i32(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i32, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_i32(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i32>, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn required_i64(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i64, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_bool(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<bool>, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn required_bool(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<bool, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_text(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<String>, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn required_text(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<String, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_float(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<f64>, SourceDbReaderError> {
    row.try_get(column)
        .map_err(|source| cell(table, column, source))
}

fn optional_float_bits(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<ProjectionFloat64BitsV2_2>, SourceDbReaderError> {
    optional_float(row, table, column).map(|value| value.map(ProjectionFloat64BitsV2_2::from_f64))
}

fn parse_fixed_numeric_v2_2(
    value: &str,
    scale: u32,
    table: &'static str,
    column: &'static str,
) -> Result<ProjectionFixedNumericV2_2, SourceDbReaderError> {
    if value == "NaN" {
        return Ok(ProjectionFixedNumericV2_2::NaN);
    }
    let decimal = value
        .parse::<Decimal>()
        .map_err(|_| SourceDbReaderError::DecimalFixedScale { table, column })?;
    if decimal.scale() > scale {
        return Err(SourceDbReaderError::DecimalFixedScale { table, column });
    }
    let factor = 10_i128
        .checked_pow(scale - decimal.scale())
        .ok_or(SourceDbReaderError::DecimalFixedRange { table, column })?;
    let scaled = decimal
        .mantissa()
        .checked_mul(factor)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(SourceDbReaderError::DecimalFixedRange { table, column })?;
    Ok(ProjectionFixedNumericV2_2::Finite(scaled))
}

fn required_fixed_numeric_v2_2(
    row: &Row,
    table: &'static str,
    column: &'static str,
    scale: u32,
) -> Result<ProjectionFixedNumericV2_2, SourceDbReaderError> {
    parse_fixed_numeric_v2_2(&required_text(row, table, column)?, scale, table, column)
}

fn optional_fixed_numeric_v2_2(
    row: &Row,
    table: &'static str,
    column: &'static str,
    scale: u32,
) -> Result<Option<ProjectionFixedNumericV2_2>, SourceDbReaderError> {
    optional_text(row, table, column)?
        .map(|value| parse_fixed_numeric_v2_2(&value, scale, table, column))
        .transpose()
}

fn required_decimal(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<f64, SourceDbReaderError> {
    let value: Decimal = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    value
        .to_f64()
        .ok_or(SourceDbReaderError::DecimalOutOfRange { table, column })
}

fn optional_decimal(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<f64>, SourceDbReaderError> {
    let value: Option<Decimal> = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    value
        .map(|value| {
            value
                .to_f64()
                .ok_or(SourceDbReaderError::DecimalOutOfRange { table, column })
        })
        .transpose()
}

fn required_timestamp_ms(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<i64, SourceDbReaderError> {
    let value: PrimitiveDateTime = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    timestamp_ms(value, table, column)
}

fn optional_timestamp_ms(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i64>, SourceDbReaderError> {
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
) -> Result<i64, SourceDbReaderError> {
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
) -> Result<i64, SourceDbReaderError> {
    let value = required_timestamp_pg_us(row, table, column)?;
    validate_timestamp_0_pg_us(value, table, column)?;
    Ok(value)
}

fn validate_timestamp_0_pg_us(
    value: i64,
    table: &'static str,
    column: &'static str,
) -> Result<(), SourceDbReaderError> {
    let is_infinity = matches!(value, i64::MIN | i64::MAX);
    let is_finite_second = (POSTGRES_TIMESTAMP_FINITE_MIN_US
        ..POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US)
        .contains(&value)
        && value.rem_euclid(1_000_000) == 0;
    if !is_infinity && !is_finite_second {
        return Err(SourceDbReaderError::TimestampZeroPhysicalDomain { table, column });
    }
    Ok(())
}

#[allow(dead_code)] // reached only from intentionally unlinked candidate readers.
fn optional_timestamp_pg_us(
    row: &Row,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i64>, SourceDbReaderError> {
    let value: Option<PostgresTimestampUs> = row
        .try_get(column)
        .map_err(|source| cell(table, column, source))?;
    Ok(value.map(|value| value.0))
}

fn timestamp_ms(
    value: PrimitiveDateTime,
    table: &'static str,
    column: &'static str,
) -> Result<i64, SourceDbReaderError> {
    (value.assume_utc().unix_timestamp_nanos() / 1_000_000)
        .try_into()
        .map_err(|_| SourceDbReaderError::TimestampOutOfRange { table, column })
}

fn selected_source_car_id(selected_car_id: i64) -> Result<i16, SourceDbReaderError> {
    i16::try_from(selected_car_id).map_err(|_| SourceDbReaderError::SelectedCarIdOutOfRange)
}

fn cell(
    table: &'static str,
    column: &'static str,
    source: tokio_postgres::Error,
) -> SourceDbReaderError {
    SourceDbReaderError::Cell {
        table,
        column,
        source,
    }
}

/// Which decode budget a bounded source read ran into. The error carries the
/// limit's name and size only, never a row value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBudgetLimit {
    /// More rows than the query's own row budget.
    Rows,
    /// One row larger than the per-row byte cap.
    RowBytes,
    /// More cumulative bytes than the per-page byte budget.
    PageBytes,
}

impl std::fmt::Display for SourceBudgetLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Rows => "row-count",
            Self::RowBytes => "per-row byte",
            Self::PageBytes => "per-page byte",
        })
    }
}

#[derive(Debug, Error)]
pub enum SourceDbReaderError {
    #[error("History import failed: {primary}; staging cleanup failed ({cleanup})")]
    StageCleanupFailure {
        #[source]
        primary: Box<SourceDbReaderError>,
        cleanup: SourceDbStageCleanupFailureKind,
    },
    #[error("Source database PostgreSQL queries are forbidden after Hub handoff")]
    SourceQueriesForbiddenAfterHandoff,
    #[error("Source database user is required")]
    SourceUserRequired,
    #[error("Source database selected car id must be positive")]
    InvalidSelectedCarId,
    #[error("Source database selected car id exceeds the source smallint domain")]
    SelectedCarIdOutOfRange,
    #[error("Source database selected car {selected_car_id} does not exist in the source")]
    SelectedCarMissing { selected_car_id: i64 },
    #[error("Source database selected car references missing car_settings row {settings_id}")]
    CarSettingsMissing { settings_id: i64 },
    #[error(
        "Source database selected car has {drives} open drives, {charges} open charging processes, and {states} open states; import requires at most one of each"
    )]
    AmbiguousOpenSession {
        drives: usize,
        charges: usize,
        states: usize,
    },
    #[error("Source database settings singleton is missing")]
    SettingsSingletonMissing,
    #[error("Source database has more than one settings singleton row")]
    SettingsSingletonAmbiguous,
    #[error("Source database role has USAGE on the private schema")]
    PrivateSchemaUsageGranted,
    #[error("Source database witness transaction is not read-only")]
    WitnessTransactionWritable,
    #[error("Source database PostgreSQL connection has no server address")]
    MissingServerAddress,
    #[error("Source database PostgreSQL server port is invalid: {port}")]
    InvalidServerPort { port: i32 },
    #[error("Source database count is invalid for {column}: {count}")]
    InvalidSourceCount { column: &'static str, count: i64 },
    #[error("Source database PostgreSQL connect timeout must be greater than zero")]
    InvalidConnectTimeout,
    #[error("Source database PostgreSQL page size must be in 1..=10000")]
    InvalidPageSize,
    #[error("Source database PostgreSQL maximum rows must be greater than zero")]
    InvalidMaximumRows,
    #[error("Source database PostgreSQL parallel COPY lanes must be in 1..=8")]
    InvalidParallelCopyLanes,
    #[error("could not load a usable native TLS trust store")]
    NativeTrustStoreUnavailable,
    #[error("Source database PostgreSQL connection timed out")]
    ConnectTimedOut,
    #[error(
        "Source database PostgreSQL COPY statement timeout must be between 60 seconds and 24 hours"
    )]
    InvalidCopyStatementTimeout,
    #[error("Source database PostgreSQL snapshot rollback timed out")]
    SnapshotRollbackTimedOut,
    #[error("Source database PostgreSQL connection task did not stop before it was aborted")]
    SnapshotConnectionShutdownTimedOut,
    #[error("Source database PostgreSQL connection task did not stop after abort")]
    SnapshotConnectionAbortTimedOut,
    #[error(
        "Source database PostgreSQL connection task failed (cancelled={cancelled}, panicked={panicked})"
    )]
    SnapshotConnectionTaskFailed { cancelled: bool, panicked: bool },
    #[error("Source database schema has no migration version")]
    MissingMigrationVersion,
    #[error("Source database exported an invalid PostgreSQL snapshot identifier")]
    InvalidExportedSnapshot,
    #[error("Source database parallel capture lane panicked")]
    ParallelLanePanicked,
    #[error("Source database parallel capture lane was cancelled")]
    ParallelLaneCancelled,
    #[error("Source database staged row serialization failed: {0}")]
    SerializeStageRow(#[from] serde_json::Error),
    #[error("Source database {table} page did not advance its keyset cursor")]
    NonProgressingPage { table: &'static str },
    #[error("Source database physical {table} source id must be positive (actual {id})")]
    PhysicalSourceIdNotPositive { table: &'static str, id: i64 },
    #[error(
        "Source database {table}.{column} FLOAT8 cannot be represented bit-exactly in a JSON stage"
    )]
    PhysicalFloatNotStageable {
        table: &'static str,
        column: &'static str,
    },
    #[error("Source database exceeds the {maximum} row import limit")]
    MaximumRowsExceeded { maximum: usize },
    #[error("source {table} read exceeded its {limit} budget of {maximum}")]
    SourceReadBudgetExceeded {
        table: &'static str,
        limit: SourceBudgetLimit,
        maximum: usize,
    },
    #[error(
        "Source database compatibility history has {count} positions, exceeding the {maximum} materialization limit"
    )]
    MaterializedHistoryPositionLimitExceeded { maximum: usize, count: usize },
    #[error("Source database open session exceeds the {maximum} position materialization limit")]
    MaterializedOpenPositionLimitExceeded { maximum: usize },
    #[error("Source database open-session projection validation failed: {0}")]
    OpenSessionProjection(#[source] SourceDbProjectionError),
    #[error("Source database {table}.{column} decimal cannot be represented as a finite f64")]
    DecimalOutOfRange {
        table: &'static str,
        column: &'static str,
    },
    #[error("Source database {table}.{column} decimal does not fit its pinned fixed scale")]
    DecimalFixedScale {
        table: &'static str,
        column: &'static str,
    },
    #[error("Source database {table}.{column} decimal is outside its pinned source range")]
    DecimalFixedRange {
        table: &'static str,
        column: &'static str,
    },
    #[error(
        "Source database {table}.{column} timestamp cannot be represented as epoch milliseconds"
    )]
    TimestampOutOfRange {
        table: &'static str,
        column: &'static str,
    },
    #[error(
        "Source database {table}.{column} timestamp is outside its physical timestamp(0) source domain"
    )]
    TimestampZeroPhysicalDomain {
        table: &'static str,
        column: &'static str,
    },
    #[error("Source database {table}.{column} integer cannot be represented as a smallint")]
    IntegerOutOfRange {
        table: &'static str,
        column: &'static str,
    },
    #[error("Source database {table}.{column} could not be decoded")]
    Cell {
        table: &'static str,
        column: &'static str,
        #[source]
        source: tokio_postgres::Error,
    },
    #[error("Source database geofence billing type is invalid")]
    InvalidGeofenceBillingType,
    #[error("Source database states_status enum value is invalid")]
    InvalidStateStatus,
    #[error("Source database settings.{column} enum value is invalid")]
    InvalidSettingsEnum { column: &'static str },
    #[error(transparent)]
    Schema(#[from] crate::source_db_schema::SchemaCompatibilityError),
    #[error(transparent)]
    Stage(#[from] SourceDbStageError),
    #[error(transparent)]
    Postgres(#[from] tokio_postgres::Error),
}

#[cfg(test)]
mod n1_src_import_source_db_reader_decoding_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for SourceBudgetLimit {
        fn error_example() -> Self {
            SourceBudgetLimit::Rows
        }
    }

    impl ErrorExample for SourceDbReaderError {
        fn error_example() -> Self {
            SourceDbReaderError::SourceQueriesForbiddenAfterHandoff
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_source_budget_limit(error: SourceBudgetLimit) {
            match &error {
                SourceBudgetLimit::Rows => (),
                SourceBudgetLimit::RowBytes => (),
                SourceBudgetLimit::PageBytes => (),
            }
            assert_name_free(&error);
        }
        check_source_budget_limit(SourceBudgetLimit::Rows);
        check_source_budget_limit(SourceBudgetLimit::RowBytes);
        check_source_budget_limit(SourceBudgetLimit::PageBytes);
        fn check_source_db_reader_error(error: SourceDbReaderError) {
            match &error {
                SourceDbReaderError::StageCleanupFailure { .. } => (),
                SourceDbReaderError::SourceQueriesForbiddenAfterHandoff => (),
                SourceDbReaderError::SourceUserRequired => (),
                SourceDbReaderError::InvalidSelectedCarId => (),
                SourceDbReaderError::SelectedCarIdOutOfRange => (),
                SourceDbReaderError::SelectedCarMissing { .. } => (),
                SourceDbReaderError::CarSettingsMissing { .. } => (),
                SourceDbReaderError::AmbiguousOpenSession { .. } => (),
                SourceDbReaderError::SettingsSingletonMissing => (),
                SourceDbReaderError::SettingsSingletonAmbiguous => (),
                SourceDbReaderError::PrivateSchemaUsageGranted => (),
                SourceDbReaderError::WitnessTransactionWritable => (),
                SourceDbReaderError::MissingServerAddress => (),
                SourceDbReaderError::InvalidServerPort { .. } => (),
                SourceDbReaderError::InvalidSourceCount { .. } => (),
                SourceDbReaderError::InvalidConnectTimeout => (),
                SourceDbReaderError::InvalidPageSize => (),
                SourceDbReaderError::InvalidMaximumRows => (),
                SourceDbReaderError::InvalidParallelCopyLanes => (),
                SourceDbReaderError::NativeTrustStoreUnavailable => (),
                SourceDbReaderError::ConnectTimedOut => (),
                SourceDbReaderError::InvalidCopyStatementTimeout => (),
                SourceDbReaderError::SnapshotRollbackTimedOut => (),
                SourceDbReaderError::SnapshotConnectionShutdownTimedOut => (),
                SourceDbReaderError::SnapshotConnectionAbortTimedOut => (),
                SourceDbReaderError::SnapshotConnectionTaskFailed { .. } => (),
                SourceDbReaderError::MissingMigrationVersion => (),
                SourceDbReaderError::InvalidExportedSnapshot => (),
                SourceDbReaderError::ParallelLanePanicked => (),
                SourceDbReaderError::ParallelLaneCancelled => (),
                SourceDbReaderError::SerializeStageRow(..) => (),
                SourceDbReaderError::NonProgressingPage { .. } => (),
                SourceDbReaderError::PhysicalSourceIdNotPositive { .. } => (),
                SourceDbReaderError::PhysicalFloatNotStageable { .. } => (),
                SourceDbReaderError::MaximumRowsExceeded { .. } => (),
                SourceDbReaderError::SourceReadBudgetExceeded { .. } => (),
                SourceDbReaderError::MaterializedHistoryPositionLimitExceeded { .. } => (),
                SourceDbReaderError::MaterializedOpenPositionLimitExceeded { .. } => (),
                SourceDbReaderError::OpenSessionProjection(..) => (),
                SourceDbReaderError::DecimalOutOfRange { .. } => (),
                SourceDbReaderError::DecimalFixedScale { .. } => (),
                SourceDbReaderError::DecimalFixedRange { .. } => (),
                SourceDbReaderError::TimestampOutOfRange { .. } => (),
                SourceDbReaderError::TimestampZeroPhysicalDomain { .. } => (),
                SourceDbReaderError::IntegerOutOfRange { .. } => (),
                SourceDbReaderError::Cell { .. } => (),
                SourceDbReaderError::InvalidGeofenceBillingType => (),
                SourceDbReaderError::InvalidStateStatus => (),
                SourceDbReaderError::InvalidSettingsEnum { .. } => (),
                SourceDbReaderError::Schema(..) => (),
                SourceDbReaderError::Stage(..) => (),
                SourceDbReaderError::Postgres(..) => (),
            }
            assert_name_free(&error);
        }
        check_source_db_reader_error(SourceDbReaderError::StageCleanupFailure {
            primary: example(),
            cleanup: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::SourceQueriesForbiddenAfterHandoff);
        check_source_db_reader_error(SourceDbReaderError::SourceUserRequired);
        check_source_db_reader_error(SourceDbReaderError::InvalidSelectedCarId);
        check_source_db_reader_error(SourceDbReaderError::SelectedCarIdOutOfRange);
        check_source_db_reader_error(SourceDbReaderError::SelectedCarMissing {
            selected_car_id: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::CarSettingsMissing {
            settings_id: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::AmbiguousOpenSession {
            drives: example(),
            charges: example(),
            states: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::SettingsSingletonMissing);
        check_source_db_reader_error(SourceDbReaderError::SettingsSingletonAmbiguous);
        check_source_db_reader_error(SourceDbReaderError::PrivateSchemaUsageGranted);
        check_source_db_reader_error(SourceDbReaderError::WitnessTransactionWritable);
        check_source_db_reader_error(SourceDbReaderError::MissingServerAddress);
        check_source_db_reader_error(SourceDbReaderError::InvalidServerPort { port: example() });
        check_source_db_reader_error(SourceDbReaderError::InvalidSourceCount {
            column: example(),
            count: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::InvalidConnectTimeout);
        check_source_db_reader_error(SourceDbReaderError::InvalidPageSize);
        check_source_db_reader_error(SourceDbReaderError::InvalidMaximumRows);
        check_source_db_reader_error(SourceDbReaderError::InvalidParallelCopyLanes);
        check_source_db_reader_error(SourceDbReaderError::NativeTrustStoreUnavailable);
        check_source_db_reader_error(SourceDbReaderError::ConnectTimedOut);
        check_source_db_reader_error(SourceDbReaderError::InvalidCopyStatementTimeout);
        check_source_db_reader_error(SourceDbReaderError::SnapshotRollbackTimedOut);
        check_source_db_reader_error(SourceDbReaderError::SnapshotConnectionShutdownTimedOut);
        check_source_db_reader_error(SourceDbReaderError::SnapshotConnectionAbortTimedOut);
        check_source_db_reader_error(SourceDbReaderError::SnapshotConnectionTaskFailed {
            cancelled: example(),
            panicked: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::MissingMigrationVersion);
        check_source_db_reader_error(SourceDbReaderError::InvalidExportedSnapshot);
        check_source_db_reader_error(SourceDbReaderError::ParallelLanePanicked);
        check_source_db_reader_error(SourceDbReaderError::ParallelLaneCancelled);
        check_source_db_reader_error(SourceDbReaderError::SerializeStageRow(example()));
        check_source_db_reader_error(SourceDbReaderError::NonProgressingPage { table: example() });
        check_source_db_reader_error(SourceDbReaderError::PhysicalSourceIdNotPositive {
            table: example(),
            id: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::PhysicalFloatNotStageable {
            table: example(),
            column: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::MaximumRowsExceeded {
            maximum: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::SourceReadBudgetExceeded {
            table: example(),
            limit: example(),
            maximum: example(),
        });
        check_source_db_reader_error(
            SourceDbReaderError::MaterializedHistoryPositionLimitExceeded {
                maximum: example(),
                count: example(),
            },
        );
        check_source_db_reader_error(SourceDbReaderError::MaterializedOpenPositionLimitExceeded {
            maximum: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::OpenSessionProjection(example()));
        check_source_db_reader_error(SourceDbReaderError::DecimalOutOfRange {
            table: example(),
            column: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::DecimalFixedScale {
            table: example(),
            column: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::DecimalFixedRange {
            table: example(),
            column: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::TimestampOutOfRange {
            table: example(),
            column: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::TimestampZeroPhysicalDomain {
            table: example(),
            column: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::IntegerOutOfRange {
            table: example(),
            column: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::Cell {
            table: example(),
            column: example(),
            source: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::InvalidGeofenceBillingType);
        check_source_db_reader_error(SourceDbReaderError::InvalidStateStatus);
        check_source_db_reader_error(SourceDbReaderError::InvalidSettingsEnum {
            column: example(),
        });
        check_source_db_reader_error(SourceDbReaderError::Schema(example()));
        check_source_db_reader_error(SourceDbReaderError::Stage(example()));
        check_source_db_reader_error(SourceDbReaderError::Postgres(example()));
    }
}
