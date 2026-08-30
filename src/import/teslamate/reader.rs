// SPDX-License-Identifier: AGPL-3.0-only

//! Read-only, bounded TeslaMate PostgreSQL history reader.
//!
//! This is deliberately a source adapter, not a TeslaMate clone. It accepts a
//! credential-free endpoint and a protected local password, opens bounded TLS
//! connections, pins the source to a repeatable-read transaction, checks the
//! reviewed schema, then fetches only fixed projections with keyset pages.

use std::{
    io,
    path::Path,
    pin::pin,
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
    Client, Config, NoTls, Row,
    binary_copy::BinaryCopyOutRow,
    config::SslMode,
    types::{FromSql, Type},
};
use tokio_postgres_rustls::MakeRustlsConnect;
use zeroize::Zeroize;

use crate::{
    credentials::TeslaMatePostgresPassword,
    hub_pack::{
        GeofenceBillingType, POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US,
        POSTGRES_TIMESTAMP_FINITE_MIN_US, ProjectionCarSettings, ProjectionPreferredRangeV2_2,
        ProjectionUnitOfLengthV2_2, ProjectionUnitOfPressureV2_2, ProjectionUnitOfTemperatureV2_2,
    },
    teslamate::ReadOnlySource,
    teslamate_projection::{
        TeslaMateAddress, TeslaMateCar, TeslaMateCarPhysicalV2_2, TeslaMateCarSettingsPhysicalV2_2,
        TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive, TeslaMateGeofence,
        TeslaMateHistory, TeslaMateOpenSession, TeslaMatePosition, TeslaMateProjectionError,
        TeslaMateSettingsPhysicalV2_2, TeslaMateSourceWatermark, TeslaMateSourceWatermarks,
        TeslaMateState, TeslaMateUpdate, TeslaMateUpdatePhysicalV2_2,
    },
    teslamate_schema::{
        ENUM_PROBE_SQL, MAX_VALIDATED_MIGRATION, MIGRATION_VERSIONS_SQL, MIN_SUPPORTED_MIGRATION,
        SCHEMA_PROBE_SQL, SETTINGS_RELATIONSHIP_SQL, SourceTable,
        TESLAMATE_V4_MIGRATION_SET_SHA256, TESLAMATE_V4_SOURCE_REVISION, projection,
        validate_migration_versions, validate_observed_enums, validate_observed_schema,
        validate_settings_relationship,
    },
    teslamate_stage::{
        TeslaMateStage, TeslaMateStageError, TeslaMateStageLimits, TeslaMateStageTable,
    },
    teslamate_token::MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES,
};

/// After migration handoff, live collection must never re-open TeslaMate
/// PostgreSQL. Tests (and future production handoff) flip this gate closed so
/// any accidental source query fails closed rather than silently succeeding.
static POSTGRES_SOURCE_QUERIES_ALLOWED: AtomicBool = AtomicBool::new(true);

const SNAPSHOT_ROLLBACK_TIMEOUT: Duration = Duration::from_secs(5);
const SNAPSHOT_CONNECTION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MATERIALIZED_HISTORY_POSITIONS: usize = 100_000;
const MAX_MATERIALIZED_OPEN_POSITIONS: usize = 100_000;

include!("reader/diagnostics.rs");
include!("reader/connection.rs");
include!("reader/credentials.rs");
include!("reader/open_session.rs");
include!("reader/capture.rs");
include!("reader/staging.rs");
include!("reader/schema.rs");
include!("reader/binary_rows.rs");
include!("reader/v2_rows.rs");
include!("reader/decoding.rs");

#[cfg(test)]
#[path = "reader/tests.rs"]
mod tests;
