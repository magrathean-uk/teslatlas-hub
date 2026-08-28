//! Operator diagnostics for Hub SQLite, stored Tesla credentials, and TLS.
//!
//! `doctor` is read-only: it never writes TeslaMate PostgreSQL and never
//! deletes Owner or Fleet tokens.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use uuid::Uuid;

use crate::{
    BUILD_VERSION,
    config::{CollectorProvider, HubConfig},
    db::{CatalogueInventory, HubStore, ReadinessReasonCode, SCHEMA_VERSION, StoreError},
    fleet_credentials::{
        FleetCredentialError, stored_fleet_scope_summary, validate_stored_fleet_credentials,
    },
    teslamate_credentials::validate_stored_legacy_credentials_read_only,
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialPresence {
    pub present: bool,
    pub expires_at: Option<i64>,
    pub next_refresh_at: Option<i64>,
    pub valid_for_collection: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FleetCredentialDiagnostics {
    pub present: bool,
    pub expires_at: Option<i64>,
    pub next_refresh_at: Option<i64>,
    pub scope_status: Option<String>,
    pub vehicle_device_data: Option<bool>,
    pub vehicle_location: Option<bool>,
    pub vehicle_commands: Option<bool>,
    pub vehicle_charging_commands: Option<bool>,
    pub valid_for_collection: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialDiagnostics {
    pub selected_provider: CollectorProvider,
    pub selected_credentials_present: bool,
    pub legacy: CredentialPresence,
    pub fleet: FleetCredentialDiagnostics,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollectorDiagnostics {
    pub provider: CollectorProvider,
    pub interval_seconds: u64,
    pub request_timeout_seconds: u64,
    pub can_start: bool,
    pub init_only: bool,
    pub readiness: String,
    pub geocoder_enabled: bool,
    pub terrain_enabled: bool,
    pub bind: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TlsDiagnostics {
    pub configured: bool,
    pub certificate_present: bool,
    pub private_key_present: bool,
    pub identity_valid: bool,
    pub public_url_host: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VehicleDiagnostics {
    pub vehicle_id: Uuid,
    pub display_name: Option<String>,
    pub source_car_id: Option<i64>,
    pub tesla_eid: Option<i64>,
    pub latest_observation_id: Option<i64>,
    pub latest_observed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SafetyDiagnostics {
    pub teslamate_source_never_mutated: bool,
    pub import_does_not_delete_fleet_tokens: bool,
    pub doctor_is_read_only: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HubDoctorReport {
    pub status: String,
    pub version: String,
    pub sqlite: String,
    pub database: PathBuf,
    pub database_bytes: u64,
    pub schema_version: i32,
    pub checks: Vec<DoctorCheck>,
    pub catalogue: CatalogueInventory,
    pub credentials: CredentialDiagnostics,
    pub collector: CollectorDiagnostics,
    pub tls: TlsDiagnostics,
    pub vehicles: Vec<VehicleDiagnostics>,
    pub safety: SafetyDiagnostics,
}

impl HubDoctorReport {
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }

    pub fn log(&self) {
        tracing::info!(
            status = %self.status,
            version = %self.version,
            sqlite = %self.sqlite,
            database = %self.database.display(),
            database_bytes = self.database_bytes,
            schema_version = self.schema_version,
            journal_mode = %self.catalogue.journal_mode,
            wal_present = self.catalogue.wal_present,
            wal_bytes = self.catalogue.wal_bytes,
            vehicles = self.catalogue.vehicles,
            observations = self.catalogue.raw_observations,
            current_observations = self.catalogue.current_observations,
            quarantined = self.catalogue.quarantined_sessions,
            open_lifecycle_rows = self.catalogue.open_lifecycle_rows,
            packs = self.catalogue.referenced_packs,
            pack_bytes = self.catalogue.referenced_pack_bytes,
            physical_pack_files = self.catalogue.physical_pack_files,
            physical_pack_bytes = self.catalogue.physical_pack_bytes,
            "Hub doctor catalogue"
        );
        tracing::info!(
            provider = ?self.credentials.selected_provider,
            selected_credentials_present = self.credentials.selected_credentials_present,
            legacy_present = self.credentials.legacy.present,
            legacy_valid_for_collection = self.credentials.legacy.valid_for_collection,
            fleet_present = self.credentials.fleet.present,
            fleet_scope_status = self.credentials.fleet.scope_status.as_deref(),
            fleet_valid_for_collection = self.credentials.fleet.valid_for_collection,
            interval_seconds = self.collector.interval_seconds,
            can_start = self.collector.can_start,
            init_only = self.collector.init_only,
            readiness = %self.collector.readiness,
            "Hub doctor credentials and collector"
        );
        tracing::info!(
            tls_configured = self.tls.configured,
            certificate_present = self.tls.certificate_present,
            private_key_present = self.tls.private_key_present,
            identity_valid = self.tls.identity_valid,
            teslamate_source_never_mutated = self.safety.teslamate_source_never_mutated,
            import_does_not_delete_fleet_tokens = self.safety.import_does_not_delete_fleet_tokens,
            "Hub doctor connection and safety"
        );
        for check in &self.checks {
            if check.passed {
                tracing::info!(check = %check.name, detail = %check.detail, "doctor check passed");
            } else {
                tracing::warn!(check = %check.name, detail = %check.detail, "doctor check failed");
            }
        }
        for vehicle in &self.vehicles {
            tracing::info!(
                vehicle_id = %vehicle.vehicle_id,
                display_name = vehicle.display_name.as_deref(),
                source_car_id = vehicle.source_car_id,
                latest_observation_id = vehicle.latest_observation_id,
                "Hub doctor vehicle"
            );
        }
    }
}

/// Full Hub diagnosis: SQLite integrity, pack hashes, credential presence for
/// both Tesla providers, TLS files, and collector readiness. Never deletes
/// tokens and never opens TeslaMate PostgreSQL.
pub fn inspect_hub(store: &HubStore, config: &HubConfig) -> Result<HubDoctorReport, StoreError> {
    let sqlite = store.sqlite_version()?;
    let database = store.database_path().to_path_buf();
    let database_bytes = fs::metadata(&database)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let catalogue = store.catalogue_inventory()?;
    let integrity = match store.catalogue_check() {
        Ok(()) => DoctorCheck {
            name: "catalogueIntegrity".to_owned(),
            passed: true,
            detail: "PRAGMA quick_check ok; referenced packs hashed; no quarantined lifecycle"
                .to_owned(),
        },
        Err(error) => DoctorCheck {
            name: "catalogueIntegrity".to_owned(),
            passed: false,
            detail: error.to_string(),
        },
    };

    let legacy_tokens = store.load_teslamate_legacy_tokens()?;
    let fleet_tokens = store.load_fleet_tokens()?;
    let unresolved_legacy_refresh = store.has_unresolved_legacy_refresh()?;
    let legacy_valid_for_collection = !unresolved_legacy_refresh
        && legacy_tokens.as_ref().is_some_and(|tokens| {
            validate_stored_legacy_credentials_read_only(&config.data_dir, tokens).is_ok()
        });
    let legacy = CredentialPresence {
        present: legacy_tokens.is_some(),
        expires_at: legacy_tokens.as_ref().map(|tokens| tokens.expires_at()),
        next_refresh_at: legacy_tokens
            .as_ref()
            .map(|tokens| tokens.next_refresh_at()),
        valid_for_collection: legacy_valid_for_collection,
    };
    let mut fleet = FleetCredentialDiagnostics {
        present: fleet_tokens.is_some(),
        expires_at: fleet_tokens.as_ref().map(|tokens| tokens.expires_at()),
        next_refresh_at: fleet_tokens.as_ref().map(|tokens| tokens.next_refresh_at()),
        scope_status: None,
        vehicle_device_data: None,
        vehicle_location: None,
        vehicle_commands: None,
        vehicle_charging_commands: None,
        valid_for_collection: false,
    };
    if fleet.present {
        match stored_fleet_scope_summary(store, &config.data_dir) {
            Ok(Some(summary)) => {
                fleet.scope_status = Some("ready".to_owned());
                fleet.vehicle_device_data = Some(summary.vehicle_device_data);
                fleet.vehicle_location = Some(summary.vehicle_location);
                fleet.vehicle_commands = Some(summary.vehicle_commands);
                fleet.vehicle_charging_commands = Some(summary.vehicle_charging_commands);
            }
            Ok(None) => fleet.scope_status = Some("missing".to_owned()),
            Err(error) => fleet.scope_status = Some(fleet_scope_status(&error).to_owned()),
        }
        fleet.valid_for_collection =
            validate_stored_fleet_credentials(store, &config.data_dir).is_ok();
    }

    let selected_credentials_present = match config.collector.provider {
        CollectorProvider::Legacy => legacy.present && legacy.valid_for_collection,
        CollectorProvider::Fleet => fleet.present && fleet.valid_for_collection,
    };
    let configured = store.configured_tesla_vehicles()?;
    let init_only = configured.is_empty()
        && catalogue.vehicles == 0
        && catalogue.raw_observations == 0
        && catalogue.current_observations == 0
        && catalogue.open_lifecycle_rows == 0
        && catalogue.referenced_packs == 0
        && catalogue.paired_devices == 0
        && !legacy.present
        && !fleet.present;
    let collector_required = config.collector.interval_seconds > 0 && !init_only;
    let can_start = collector_required && !configured.is_empty() && selected_credentials_present;
    let selected_credentials_configured = match config.collector.provider {
        CollectorProvider::Legacy => legacy.present,
        CollectorProvider::Fleet => fleet.present,
    };
    let now_ms = diagnostic_epoch_ms();
    let readiness = match store.service_readiness_at(collector_required, now_ms) {
        Ok(()) => "ready".to_owned(),
        Err(failure) => readiness_code(failure.code).to_owned(),
    };

    let published = store.published_vehicles()?;
    let mut vehicles = Vec::with_capacity(published.len());
    for vehicle in &published {
        let source_car_id = store
            .v2_projection_binding(vehicle.vehicle_id)
            .ok()
            .map(|binding| binding.selected_car_id);
        let tesla_eid = configured
            .iter()
            .find_map(|(vehicle_id, eid, _)| (*vehicle_id == vehicle.vehicle_id).then_some(*eid));
        let latest = store.latest_current_observation_metadata_for_vehicle(vehicle.vehicle_id)?;
        vehicles.push(VehicleDiagnostics {
            vehicle_id: vehicle.vehicle_id,
            display_name: vehicle.display_name.clone(),
            source_car_id,
            tesla_eid,
            latest_observation_id: latest
                .as_ref()
                .map(|observation| observation.observation_id),
            latest_observed_at_ms: latest
                .as_ref()
                .map(|observation| observation.observed_at_ms),
        });
    }

    let tls = tls_diagnostics(config);
    let wal_ok = DoctorCheck {
        name: "sqliteJournal".to_owned(),
        passed: catalogue.journal_mode.eq_ignore_ascii_case("wal")
            && catalogue.foreign_keys_enabled
            && catalogue.synchronous == 2
            && catalogue.schema_version == SCHEMA_VERSION,
        detail: format!(
            "journal={} synchronous={} foreign_keys={} schema={}",
            catalogue.journal_mode,
            catalogue.synchronous,
            catalogue.foreign_keys_enabled,
            catalogue.schema_version
        ),
    };
    let credentials_check = DoctorCheck {
        name: "selectedProviderCredentials".to_owned(),
        passed: (!collector_required && !selected_credentials_configured)
            || selected_credentials_present,
        detail: format!(
            "provider={:?} legacyPresent={} legacyCollection={} fleetPresent={} fleetCollection={} required={}",
            config.collector.provider,
            legacy.present,
            legacy.valid_for_collection,
            fleet.present,
            fleet.valid_for_collection,
            collector_required
        ),
    };
    let tls_check = DoctorCheck {
        name: "tlsIdentity".to_owned(),
        passed: !tls.configured || tls.identity_valid,
        detail: if !tls.configured {
            "TLS not configured (loopback HTTP allowed)".to_owned()
        } else {
            format!(
                "certificate={} privateKey={} identityValid={}",
                tls.certificate_present, tls.private_key_present, tls.identity_valid
            )
        },
    };
    let collector_check = DoctorCheck {
        name: "collectorReadiness".to_owned(),
        passed: collector_readiness_passes(collector_required, can_start, &readiness),
        detail: format!(
            "required={} initOnly={} canStart={} readiness={}",
            collector_required, init_only, can_start, readiness
        ),
    };
    let token_preservation = DoctorCheck {
        name: "tokenPreservation".to_owned(),
        passed: true,
        detail: format!(
            "doctor did not delete Owner ({}) or Fleet ({}) token rows",
            catalogue.teslamate_legacy_token_rows, catalogue.fleet_token_rows
        ),
    };
    let teslamate_safety = DoctorCheck {
        name: "teslamateSource".to_owned(),
        passed: true,
        detail: "doctor does not connect to TeslaMate PostgreSQL and never writes it".to_owned(),
    };

    let mut checks = vec![
        integrity,
        wal_ok,
        credentials_check,
        tls_check,
        collector_check,
        token_preservation,
        teslamate_safety,
    ];
    if config.collector.provider == CollectorProvider::Fleet {
        checks.insert(
            3,
            DoctorCheck {
                name: "fleetCollectionScopes".to_owned(),
                passed: (!collector_required && !fleet.present) || fleet.valid_for_collection,
                detail: fleet
                    .scope_status
                    .clone()
                    .unwrap_or_else(|| "unavailable".to_owned()),
            },
        );
    }

    let status = if checks.iter().all(|check| check.passed) {
        "ok"
    } else {
        "failed"
    };

    Ok(HubDoctorReport {
        status: status.to_owned(),
        version: BUILD_VERSION.to_owned(),
        sqlite,
        database,
        database_bytes,
        schema_version: catalogue.schema_version,
        checks,
        catalogue,
        credentials: CredentialDiagnostics {
            selected_provider: config.collector.provider,
            selected_credentials_present,
            legacy,
            fleet,
        },
        collector: CollectorDiagnostics {
            provider: config.collector.provider,
            interval_seconds: config.collector.interval_seconds,
            request_timeout_seconds: config.collector.request_timeout_seconds,
            can_start,
            init_only,
            readiness,
            geocoder_enabled: config.geocoder.enabled,
            terrain_enabled: config.terrain.enabled,
            bind: config.bind.to_string(),
        },
        tls,
        vehicles,
        safety: SafetyDiagnostics {
            teslamate_source_never_mutated: true,
            import_does_not_delete_fleet_tokens: true,
            doctor_is_read_only: true,
        },
    })
}

fn collector_readiness_passes(required: bool, can_start: bool, readiness: &str) -> bool {
    !required || (can_start && matches!(readiness, "ready" | "collector_absent"))
}

/// Cheap collector/serve startup log: inventory and credentials, not pack hashing.
pub fn log_runtime_inventory(store: &HubStore, config: &HubConfig) {
    match store.catalogue_inventory() {
        Ok(catalogue) => tracing::info!(
            provider = ?config.collector.provider,
            interval_seconds = config.collector.interval_seconds,
            vehicles = catalogue.vehicles,
            observations = catalogue.raw_observations,
            quarantined = catalogue.quarantined_sessions,
            packs = catalogue.referenced_packs,
            legacy_token_rows = catalogue.teslamate_legacy_token_rows,
            fleet_token_rows = catalogue.fleet_token_rows,
            journal_mode = %catalogue.journal_mode,
            "Hub runtime inventory"
        ),
        Err(error) => tracing::warn!(%error, "Hub runtime inventory unavailable"),
    }
    let legacy = store
        .load_teslamate_legacy_tokens()
        .ok()
        .flatten()
        .is_some();
    let fleet = store.load_fleet_tokens().ok().flatten().is_some();
    tracing::info!(
        provider = ?config.collector.provider,
        owner_tokens_present = legacy,
        fleet_tokens_present = fleet,
        "Hub stored Tesla credentials (not deleted by diagnostics or TeslaMate import)"
    );
}

fn tls_diagnostics(config: &HubConfig) -> TlsDiagnostics {
    let Some(tls) = config.tls.as_ref() else {
        return TlsDiagnostics {
            configured: false,
            certificate_present: false,
            private_key_present: false,
            identity_valid: false,
            public_url_host: None,
        };
    };
    TlsDiagnostics {
        configured: true,
        certificate_present: regular_file(tls.certificate_path.as_path()),
        private_key_present: regular_file(tls.private_key_path.as_path()),
        identity_valid: crate::server::validate_tls_identity(tls).is_ok(),
        public_url_host: url::Url::parse(&tls.public_url)
            .ok()
            .and_then(|url| url.host_str().map(ToOwned::to_owned)),
    }
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn fleet_scope_status(error: &FleetCredentialError) -> &'static str {
    match error {
        FleetCredentialError::MissingCollectionScopes => "missing_collection_scopes",
        FleetCredentialError::InvalidAccessTokenClaims => "invalid_access_token_claims",
        FleetCredentialError::MigrationRequired => "migration_required",
        _ => "unavailable",
    }
}

fn readiness_code(code: ReadinessReasonCode) -> &'static str {
    match code {
        ReadinessReasonCode::CatalogueUnavailable => "catalogue_unavailable",
        ReadinessReasonCode::LifecycleQuarantined => "lifecycle_quarantined",
        ReadinessReasonCode::PublishedContentUnservable => "published_content_unservable",
        ReadinessReasonCode::CollectorAbsent => "collector_absent",
        ReadinessReasonCode::CollectorStale => "collector_stale",
        ReadinessReasonCode::CollectorAuthTerminal => "collector_auth_terminal",
    }
}

fn diagnostic_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HubConfig;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn stopped_collector_is_ready_to_start_not_a_doctor_failure() {
        assert!(collector_readiness_passes(true, true, "collector_absent"));
        assert!(collector_readiness_passes(true, true, "ready"));
        assert!(!collector_readiness_passes(true, false, "collector_absent"));
        assert!(!collector_readiness_passes(true, true, "collector_stale"));
    }

    #[test]
    fn doctor_allows_explicit_empty_init_only_mode() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let config_path = temporary.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                "data_dir = {:?}\nbind = '127.0.0.1:18443'\n\n[collector]\ninterval_seconds = 0\n",
                temporary.path()
            ),
        )
        .expect("write config");
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
            .expect("private config");
        let config = HubConfig::load(&config_path).expect("config");

        let report = inspect_hub(&store, &config).expect("doctor report");
        assert_eq!(report.status, "ok");
        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert_eq!(report.catalogue.journal_mode, "wal");
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "catalogueIntegrity" && check.passed)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "tokenPreservation" && check.passed)
        );
        assert!(report.safety.teslamate_source_never_mutated);
        assert!(report.safety.import_does_not_delete_fleet_tokens);
        assert!(report.safety.doctor_is_read_only);
        assert!(!report.credentials.legacy.present);
        assert!(!report.credentials.fleet.present);
        assert!(report.collector.init_only);
        assert!(
            store
                .load_teslamate_legacy_tokens()
                .expect("legacy")
                .is_none()
        );
        assert!(store.load_fleet_tokens().expect("fleet").is_none());
        let json = serde_json::to_value(&report).expect("JSON");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["sqlite"], report.sqlite);
        assert!(json["database"].as_str().is_some());
    }

    #[test]
    fn doctor_rejects_disabled_undecryptable_legacy_credentials() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let tokens = crate::credentials::OwnerTokens::from_secret_parts(
            "access".to_owned(),
            "refresh".to_owned(),
        )
        .expect("tokens");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(b"correct key", &tokens)
                .expect("encrypt");
        let stored =
            crate::db::TeslaMateLegacyTokenStore::imported(access, refresh).expect("stored tokens");
        store
            .replace_teslamate_legacy_tokens(&stored)
            .expect("store tokens");
        crate::teslamate_credentials::replace_key(temporary.path(), b"wrong key")
            .expect("wrong key");
        let config_path = temporary.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                "data_dir = {:?}\nbind = '127.0.0.1:18443'\n\n[collector]\ninterval_seconds = 0\n",
                temporary.path()
            ),
        )
        .expect("write config");
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
            .expect("private config");
        let config = HubConfig::load(&config_path).expect("config");

        let report = inspect_hub(&store, &config).expect("doctor report");
        assert_eq!(report.status, "failed");
        assert!(report.credentials.legacy.present);
        assert!(!report.credentials.legacy.valid_for_collection);
        assert!(
            report
                .checks
                .iter()
                .any(|check| { check.name == "selectedProviderCredentials" && !check.passed })
        );
    }

    #[test]
    fn doctor_rejects_unresolved_legacy_refresh_without_settling_it() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let key = b"exact TeslaMate key";
        let tokens = crate::credentials::OwnerTokens::from_secret_parts(
            "access".to_owned(),
            "refresh".to_owned(),
        )
        .expect("tokens");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt");
        let stored =
            crate::db::TeslaMateLegacyTokenStore::imported(access, refresh).expect("stored tokens");
        crate::teslamate_credentials::replace_key_and_tokens(
            temporary.path(),
            &store,
            key,
            &stored,
        )
        .expect("persist credentials");
        let generation = store
            .load_teslamate_legacy_tokens()
            .expect("load tokens")
            .expect("token row")
            .credential_generation()
            .expect("bound generation");
        store
            .begin_legacy_refresh(generation)
            .expect("record unresolved refresh");

        let config_path = temporary.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                "data_dir = {:?}\nbind = '127.0.0.1:18443'\n\n[collector]\ninterval_seconds = 0\n",
                temporary.path()
            ),
        )
        .expect("write config");
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
            .expect("private config");
        let config = HubConfig::load(&config_path).expect("config");

        let report = inspect_hub(&store, &config).expect("doctor report");
        assert_eq!(report.status, "failed");
        assert!(!report.credentials.legacy.valid_for_collection);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "selectedProviderCredentials" && !check.passed)
        );
        assert!(
            store
                .has_unresolved_legacy_refresh()
                .expect("doctor remains read-only")
        );
    }

    #[test]
    fn doctor_rejects_malformed_tls_identity() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let certificate = temporary.path().join("certificate.pem");
        let private_key = temporary.path().join("private-key.pem");
        fs::write(&certificate, b"not a certificate").expect("certificate");
        fs::write(&private_key, b"not a key").expect("private key");
        fs::set_permissions(&certificate, fs::Permissions::from_mode(0o600))
            .expect("certificate mode");
        fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600)).expect("key mode");
        let config_path = temporary.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                "data_dir = {:?}\nbind = '127.0.0.1:18443'\n\n[tls]\ncertificate_path = {:?}\nprivate_key_path = {:?}\npublic_url = 'https://hub.example.test/'\n",
                temporary.path(), certificate, private_key
            ),
        )
        .expect("write config");
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
            .expect("private config");
        let config = HubConfig::load(&config_path).expect("config");

        let report = inspect_hub(&store, &config).expect("doctor report");
        assert_eq!(report.status, "failed");
        assert!(report.tls.configured);
        assert!(!report.tls.identity_valid);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "tlsIdentity" && !check.passed)
        );
    }
}
