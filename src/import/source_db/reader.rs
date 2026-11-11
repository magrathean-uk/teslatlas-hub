// SPDX-License-Identifier: AGPL-3.0-only

//! Read-only, bounded source database PostgreSQL history reader.
//!
//! This is deliberately a source adapter, not a source database clone. It accepts a
//! credential-free endpoint and a protected local password, opens bounded TLS
//! connections, pins the source to a repeatable-read transaction, checks the
//! reviewed schema, then fetches only fixed projections with keyset pages.

use std::{
    io,
    path::Path,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use futures_util::TryStreamExt;
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::PrimitiveDateTime;
use tokio::time::timeout;
use tokio::{sync::mpsc, task::JoinSet};
use tokio_postgres::{
    CancelToken, Client, Config, NoTls, Row,
    binary_copy::BinaryCopyOutRow,
    config::SslMode,
    types::{FromSql, ToSql, Type},
};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::{
    credentials::SourceDbPassword,
    hub_pack::{
        GeofenceBillingType, POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US,
        POSTGRES_TIMESTAMP_FINITE_MIN_US, ProjectionCarSettings, ProjectionFixedNumericV2_2,
        ProjectionFloat64BitsV2_2, ProjectionPreferredRangeV2_2, ProjectionStateStatusV2_2,
        ProjectionUnitOfLengthV2_2, ProjectionUnitOfPressureV2_2, ProjectionUnitOfTemperatureV2_2,
    },
    source_db_connection::ReadOnlySource,
    source_db_projection::{
        SourceDbAddress, SourceDbAddressPhysicalV2_2, SourceDbCar, SourceDbCarPhysicalV2_2,
        SourceDbCarSettingsPhysicalV2_2, SourceDbCharge, SourceDbChargePhysicalV2_2,
        SourceDbChargingProcess, SourceDbChargingProcessPhysicalV2_2, SourceDbDrive,
        SourceDbDrivePhysicalV2_2, SourceDbGeofence, SourceDbGeofencePhysicalV2_2, SourceDbHistory,
        SourceDbOpenSession, SourceDbPosition, SourceDbPositionPhysicalV2_2,
        SourceDbProjectionError, SourceDbSettingsPhysicalV2_2, SourceDbState,
        SourceDbStatePhysicalV2_2, SourceDbUpdate, SourceDbUpdatePhysicalV2_2, SourceDbWatermark,
        SourceDbWatermarks,
    },
    source_db_schema::{
        ENUM_PROBE_SQL, MAX_ENUM_PROBE_ROWS, MAX_MIGRATION_PROBE_ROWS, MAX_SCHEMA_PROBE_ROWS,
        MAX_VALIDATED_MIGRATION, MIGRATION_VERSIONS_SQL, MIN_SUPPORTED_MIGRATION, SCHEMA_PROBE_SQL,
        SETTINGS_RELATIONSHIP_SQL, SOURCE_DB_V4_MIGRATION_SET_SHA256, SOURCE_DB_V4_SOURCE_REVISION,
        SourceTable, projection, validate_migration_versions, validate_observed_enums,
        validate_observed_schema, validate_settings_relationship,
    },
    source_db_stage::{SourceDbStage, SourceDbStageError, SourceDbStageLimits, SourceDbStageTable},
};

/// After migration handoff, live collection must never re-open source database
/// PostgreSQL. Tests (and future production handoff) flip this gate closed so
/// any accidental source query fails closed rather than silently succeeding.
static POSTGRES_SOURCE_QUERIES_ALLOWED: AtomicBool = AtomicBool::new(true);

const SNAPSHOT_ROLLBACK_TIMEOUT: Duration = Duration::from_secs(5);
const SNAPSHOT_CONNECTION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MATERIALIZED_HISTORY_POSITIONS: usize = 100_000;
const MAX_MATERIALIZED_OPEN_POSITIONS: usize = 100_000;

include!("reader/diagnostics.rs");
include!("reader/bounded.rs");
include!("reader/connection.rs");
include!("reader/credentials.rs");
include!("reader/open_session.rs");
include!("reader/capture.rs");
include!("reader/staging.rs");
include!("reader/physical_capture.rs");
include!("reader/schema.rs");
include!("reader/current_position.rs");
include!("reader/binary_rows.rs");
include!("reader/v2_rows.rs");
include!("reader/decoding.rs");

#[cfg(test)]
#[path = "reader/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "reader/bounded_tests.rs"]
mod bounded_tests;
