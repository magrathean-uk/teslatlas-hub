// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs,
    io::{IsTerminal, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{ExitCode, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::future::Future;

use clap::{Parser, Subcommand};
use qrcode::{QrCode, render::unicode::Dense1x2};
use rustix::fs::{FileType, Mode, OFlags, fcntl_getfl, fcntl_setfl, fstat, open};
use rustix::process::getuid;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use teslatlas_hub::hub_user_process::AdmittedUserHub;
#[cfg(unix)]
use teslatlas_hub::protocol::CursorKey;
#[cfg(unix)]
use teslatlas_hub::teslamate_import::derive_effective_import_profile;
use teslatlas_hub::{
    collector,
    config::{CollectorProvider, HubConfig},
    credential_recovery::{RECOVERY_ENCRYPTION_KEY_BYTES, export_credentials, restore_credentials},
    credentials::{OwnerTokens, TeslaMatePostgresPassword},
    data_recovery::{create_data_backup, restore_data_backup, verify_data_backup},
    db::{HubStore, ObservationVerificationError, StoreError, TeslaMateLegacyTokenStore},
    diagnostics::{inspect_hub, log_runtime_inventory},
    fleet_api::FleetRegion,
    fleet_credentials::{
        FleetCredentialError, FleetSetupCredentials, migrate_legacy_fleet_credentials,
        persist_fleet_setup_credentials, remove_fleet_key_and_tokens, stored_fleet_scope_summary,
        validate_stored_fleet_credentials,
    },
    gpx::export_drive_gpx,
    hub_pack::GeofenceBillingType,
    owner_api::LegacyVehicleAction,
    server,
    teslamate::ReadOnlySource,
    teslamate_credentials::{
        load_or_create_cursor_key, random_encryption_key, remove_key_and_tokens,
        replace_key_and_tokens,
    },
    teslamate_import::{
        TeslaMateImportReport, TeslaMateImportRequest, TeslaMateImportScope,
        import_selected_from_postgres_with_schema_22,
        import_selected_from_postgres_with_schema_22_and_legacy_token,
    },
    teslamate_reader::{
        TeslaMateCheckSnapshot, TeslaMateLegacyTokenCiphertexts, TeslaMateReadLimits,
        TeslaMateReaderError, check_teslamate_compatibility,
    },
    teslamate_schema::{
        MAX_VALIDATED_MIGRATION, SchemaCompatibilityError, TESLAMATE_V4_MIGRATION_COUNT,
        TESLAMATE_V4_SOURCE_REVISION,
    },
    teslamate_token::{
        decrypt_legacy_owner_tokens, encrypt_legacy_owner_token_files, encrypt_legacy_owner_tokens,
    },
    teslamate_writeback::{TeslaMateCost, write_back_charge_cost},
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
include!("main/control.rs");
include!("main/teslamate_check.rs");
include!("main/dispatch.rs");
include!("main/migration.rs");
include!("main/pairing.rs");

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
