// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs,
    io::{IsTerminal, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{ExitCode, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::future::Future;

use clap::{Parser, Subcommand, ValueEnum};
use qrcode::{QrCode, render::unicode::Dense1x2};
use rustix::fs::{FileType, Mode, OFlags, fcntl_getfl, fcntl_setfl, fstat, open};
use rustix::process::getuid;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use teslatlas_hub::hub_user_process::AdmittedUserHub;
#[cfg(unix)]
use teslatlas_hub::protocol::CursorKey;
#[cfg(unix)]
use teslatlas_hub::source_db_import::derive_effective_import_profile;
use teslatlas_hub::{
    collector,
    config::{CollectorProvider, HubConfig, collector_mode},
    credential_recovery::{RECOVERY_ENCRYPTION_KEY_BYTES, export_credentials, restore_credentials},
    credentials::{OwnerTokens, SourceDbPassword},
    current_state::build_current_vehicle_summary,
    data_recovery::{create_data_backup, restore_data_backup, verify_data_backup},
    db::{
        HubStore,  ObservationVerificationError, ReadinessContext, StoreError,
    },
    diagnostics::{inspect_hub, log_runtime_inventory},
    fleet_api::FleetRegion,
    fleet_credentials::{
        FleetCredentialError, FleetSetupCredentials, inspect_stored_fleet_credentials,
    },
    gpx::export_drive_gpx,
    hub_pack::GeofenceBillingType,
    legacy_owner_credentials::{
        load_or_create_cursor_key,  
        
    },
    owner_api::LegacyVehicleAction,
    server,
    source_db_connection::ReadOnlySource,
    source_db_import::{
        SourceDbImportReport, SourceDbImportRequest, SourceDbImportScope,
        import_selected_from_postgres_with_schema_22_and_legacy_token_and_progress,
        import_selected_from_postgres_with_schema_22_and_progress,
        import_selected_history_snapshot_from_postgres_with_schema_22_and_progress,
    },
    source_db_progress::{
        SourceDbMigrationPhase, SourceDbMigrationProgressEvent, SourceDbMigrationProgressReporter,
    },
    source_db_reader::{
        LegacyOwnerTokenCiphertexts, SourceDbCheckSnapshot, SourceDbReadLimits,
        SourceDbReaderError, check_source_compatibility, check_source_history_compatibility,
    },
    source_db_schema::{
        MAX_VALIDATED_MIGRATION, SOURCE_DB_V4_MIGRATION_COUNT, SOURCE_DB_V4_SOURCE_REVISION,
        SchemaCompatibilityError,
    },
    source_token_envelope::{
        decrypt_legacy_owner_tokens,  
    },
};
use tracing_subscriber::EnvFilter;

#[cfg(unix)]
use rustix::io::Errno;

const IMMUTABLE_DIAGNOSTIC_ATTEMPTS: usize = 2;
const IMMUTABLE_DIAGNOSTIC_OPEN_ATTEMPTS: usize = 21;
#[cfg(not(test))]
const IMMUTABLE_DIAGNOSTIC_OPEN_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
const IMMUTABLE_DIAGNOSTIC_OPEN_DELAY: Duration = Duration::ZERO;

include!("main/macos_service.rs");
include!("main/cli.rs");
include!("main/healthcheck.rs");
include!("main/control.rs");
include!("main/source_check.rs");
include!("main/dispatch.rs");
include!("main/migration.rs");
include!("main/pairing.rs");

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
