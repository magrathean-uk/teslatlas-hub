// SPDX-License-Identifier: AGPL-3.0-only

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
    config::{CadenceViolationReport, CollectorMode, CollectorProvider, HubConfig, collector_mode},
    db::{
        CatalogueInventory, HubStore, ReadinessContext, SCHEMA_VERSION, StoreError,
        StreamAuditSummary, SupervisedCollectorLeaseStatus,
    },
    fleet_credentials::{
        FleetCredentialError, inspect_stored_fleet_credentials, stored_fleet_scope_summary,
        validate_stored_fleet_credentials,
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
    /// The credential unit's state code (C5 R52), read from the store so any
    /// process can show it. `None` before the unit has ever run.
    pub auth_state: Option<String>,
    /// The state's reason code, or `None` (C5 R52).
    pub auth_reason: Option<String>,
    /// Epoch seconds of the next token attempt while one is pending (C5 R31).
    pub retry_at: Option<i64>,
    /// Whether the account collects at the moment of the last status write
    /// (C5 R52a). False before the unit has ever run.
    pub collecting: bool,
    /// The host clock differs from Tesla's by more than 300 s (C5 R55).
    pub clock_skew: bool,
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
    /// The one condition that needs the owner while the service stays up (B2
    /// clarification 3): the final attention registry, selected by its fixed
    /// precedence and read independently of the lease. `None` for a disabled
    /// collector. It never changes `readiness` and never fails the report.
    pub collection_attention: Option<String>,
    /// Healthy collector states worth showing: a paused car, a rate-limit wait,
    /// a Tesla outage, an alert gap the owner has not been told about (C3 R13,
    /// R66). They never change `readiness` and never repeat the attention.
    pub readiness_detail: Vec<String>,
    /// A required collector holds a coherent, unexpired lease (a terminal lease
    /// still witnesses a running process). Lease timing is not an exact stop
    /// time.
    pub running: bool,
    /// The redacted lease timing, or `None` without a lease row.
    pub lease: Option<SupervisedCollectorLeaseStatus>,
    /// The first broken cadence rule of this configuration while collection is
    /// active (managed Serve waits for a correction and restart), else `None`.
    pub cadence_error: Option<CadenceViolationReport>,
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
    pub stream_audit: StreamAuditSummary,
    pub tls: TlsDiagnostics,
    pub vehicles: Vec<VehicleDiagnostics>,
    pub safety: SafetyDiagnostics,
}

const STREAM_AUDIT_DIAGNOSTIC_WINDOW_MS: i64 = 24 * 60 * 60 * 1_000;

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
            attention = self.collector.collection_attention.as_deref(),
            running = self.collector.running,
            cadence_valid = self.collector.cadence_error.is_none(),
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
        tracing::info!(
            since_ms = self.stream_audit.since_ms,
            connect_attempts = self.stream_audit.connect_attempts,
            successful_connects = self.stream_audit.successful_connects,
            subscribe_attempts = self.stream_audit.subscribe_attempts,
            successful_subscriptions = self.stream_audit.successful_subscriptions,
            transport_errors = self.stream_audit.transport_errors,
            authentication_rejections = self.stream_audit.authentication_rejections,
            protocol_errors = self.stream_audit.protocol_errors,
            unresolved_attempts = self.stream_audit.unresolved_attempts,
            unresolved_sessions = self.stream_audit.unresolved_sessions,
            last_subscription_success_at_ms = self.stream_audit.last_subscription_success_at_ms,
            last_failure_at_ms = self.stream_audit.last_failure_at_ms,
            "Hub doctor recent stream audit"
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
    let now_ms = diagnostic_epoch_ms();
    // The loaded configuration, not the account row, decides whether the
    // credential unit's status applies (B2 clarification 3): Legacy and Provider
    // read it, effective at this moment; Fleet polling ignores a retained one and
    // never reads Legacy credential material for it.
    let loaded_mode = collector_mode(&config.collector);
    let auth_status = if loaded_mode == CollectorMode::FleetPolling {
        None
    } else {
        match store.effective_collector_auth(loaded_mode, now_ms) {
            Ok(effective) => effective.map(|effective| effective.row),
            // A key, cipher or store fault: show what is stored; the credential
            // validation below reports the fault itself.
            Err(_) => store.load_collector_auth_row()?,
        }
    };
    // C5 section 3.5: a stop, a refusal or a configuration fault means the
    // stored pair cannot collect, however well it decrypts.
    let auth_blocks_collection = auth_status.as_ref().is_some_and(|row| {
        matches!(
            row.state.as_str(),
            "signed_out" | "configuration_error" | "api_refused"
        )
    });
    // A Fleet configuration never touches the Legacy key material: the retained
    // pair is reported as present and not valid for collection, without a key
    // read or a decryption (C3 R1, B2 clarification 3).
    let legacy_key_valid = loaded_mode != CollectorMode::FleetPolling
        && legacy_tokens.as_ref().is_some_and(|tokens| {
            validate_stored_legacy_credentials_read_only(&config.data_dir, tokens).is_ok()
        });
    // Operational validity: whether collection could use the pair now.
    let legacy_valid_for_collection =
        !unresolved_legacy_refresh && !auth_blocks_collection && legacy_key_valid;
    // Diagnostic admission: the pair is present and decrypts, no Legacy refresh is
    // unresolved and the applicable status is not signed out. A refusal or a
    // credential-unit configuration fault alone is attention, not a failure.
    let legacy_admitted = legacy_tokens.is_some()
        && legacy_key_valid
        && !unresolved_legacy_refresh
        && !auth_status
            .as_ref()
            .is_some_and(|row| row.state == "signed_out");
    let legacy = CredentialPresence {
        present: legacy_tokens.is_some(),
        expires_at: legacy_tokens.as_ref().map(|tokens| tokens.expires_at()),
        next_refresh_at: legacy_tokens
            .as_ref()
            .map(|tokens| tokens.next_refresh_at()),
        valid_for_collection: legacy_valid_for_collection,
        auth_state: auth_status.as_ref().map(|row| row.state.clone()),
        auth_reason: auth_status.as_ref().and_then(|row| row.reason.clone()),
        retry_at: auth_status.as_ref().and_then(|row| row.retry_at),
        collecting: auth_status.as_ref().is_some_and(|row| row.collecting),
        clock_skew: auth_status.as_ref().is_some_and(|row| row.clock_skew),
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
    // Independent integrity of the stored Fleet pair (key, ciphertext,
    // generation, claims, scopes), then whether an identified refresh is
    // unresolved. The unresolved input is fenced, so it never collects
    // (`valid_for_collection`), but the pair is sound and admissible.
    let mut fleet_admitted = false;
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
        fleet_admitted = inspect_stored_fleet_credentials(store, &config.data_dir).is_ok();
    }

    let selected_credentials_present = match config.collector.provider {
        CollectorProvider::Legacy => legacy.present && legacy.valid_for_collection,
        CollectorProvider::Fleet => fleet.present && fleet.valid_for_collection,
    };
    let selected_credentials_admitted = match config.collector.provider {
        CollectorProvider::Legacy => legacy_admitted,
        CollectorProvider::Fleet => fleet.present && fleet_admitted,
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
    let direct_provider_credentials_required = collector_required;
    let can_start = collector_required && !configured.is_empty() && selected_credentials_present;
    // The doctor's own start check judges admission, not operational validity: an
    // owner-resolvable condition (a refusal, a credential-unit configuration
    // fault, an identified unresolved Fleet refresh) is attention, so it must not
    // fail the report, while `can_start` keeps its prerequisite meaning.
    let diagnostic_can_start =
        collector_required && !configured.is_empty() && selected_credentials_admitted;
    let selected_credentials_configured = match config.collector.provider {
        CollectorProvider::Legacy => legacy.present,
        CollectorProvider::Fleet => fleet.present,
    };
    let stream_audit = store
        .stream_audit_summary_since(now_ms.saturating_sub(STREAM_AUDIT_DIAGNOSTIC_WINDOW_MS))?;
    let stream_recovered = stream_audit_recovered(&stream_audit);
    let cadence_violation = collector_required
        .then(|| config.collector.cadence_violation())
        .flatten();
    let view = store.collector_readiness_view(&ReadinessContext::configured(
        collector_required,
        now_ms,
        loaded_mode,
        cadence_violation.clone(),
    ));
    let readiness = match view.service {
        Ok(()) => "ready".to_owned(),
        Err(failure) => failure.code.code().to_owned(),
    };
    let attention = view.attention.map(|code| code.code().to_owned());
    let readiness_detail: Vec<String> = view.detail.iter().map(|word| (*word).to_owned()).collect();
    let lease = store.supervised_collector_lease_status()?;

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
    let credential_attribution = match config.collector.provider {
        CollectorProvider::Legacy => format!(
            "authState={} unresolvedRefresh={}",
            legacy.auth_state.as_deref().unwrap_or("none"),
            unresolved_legacy_refresh
        ),
        CollectorProvider::Fleet => {
            "authState=not_applicable unresolvedRefresh=not_applicable".to_owned()
        }
    };
    let credentials_check = DoctorCheck {
        name: "selectedProviderCredentials".to_owned(),
        passed: (!direct_provider_credentials_required && !selected_credentials_configured)
            || selected_credentials_admitted,
        detail: format!(
            "provider={:?} legacyPresent={} legacyCollection={} fleetPresent={} fleetCollection={} required={} {}",
            config.collector.provider,
            legacy.present,
            legacy.valid_for_collection,
            fleet.present,
            fleet.valid_for_collection,
            collector_required,
            credential_attribution
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
        passed: collector_readiness_passes(collector_required, diagnostic_can_start, &readiness),
        detail: format!(
            "required={} initOnly={} canStart={} readiness={} attention={} detail={}",
            collector_required,
            init_only,
            can_start,
            readiness,
            attention.as_deref().unwrap_or("none"),
            readiness_detail.join(",")
        ),
    };
    // The cadence is informational here: an invalid one is attention, and the
    // doctor's other checks decide pass or failure (B2 clarification 3).
    let cadence_check = DoctorCheck {
        name: "collectorCadence".to_owned(),
        passed: true,
        detail: match (&cadence_violation, collector_required) {
            (_, false) => {
                "inactive: supervised collection is not required, so no cadence applies".to_owned()
            }
            (None, true) => "valid".to_owned(),
            (Some(violation), true) => format!(
                "invalid: {violation}; managed Serve waits without collecting until the setting is corrected and the service restarted"
            ),
        },
    };
    let stream_check = DoctorCheck {
        name: "streamRecovery".to_owned(),
        passed: stream_audit.authentication_rejections == 0
            && stream_audit.protocol_errors == 0
            && stream_audit.unresolved_attempts == 0
            && stream_audit.unresolved_sessions == 0
            && stream_recovered,
        detail: format!(
            "windowHours=24 connects={}/{} subscribes={}/{} transportErrors={} authRejected={} protocolErrors={} unresolvedAttempts={} unresolvedSessions={} recovered={}",
            stream_audit.successful_connects,
            stream_audit.connect_attempts,
            stream_audit.successful_subscriptions,
            stream_audit.subscribe_attempts,
            stream_audit.transport_errors,
            stream_audit.authentication_rejections,
            stream_audit.protocol_errors,
            stream_audit.unresolved_attempts,
            stream_audit.unresolved_sessions,
            stream_recovered,
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
        cadence_check,
        stream_check,
        token_preservation,
        teslamate_safety,
    ];
    if config.collector.provider == CollectorProvider::Fleet {
        checks.insert(
            3,
            DoctorCheck {
                name: "fleetCollectionScopes".to_owned(),
                passed: (!direct_provider_credentials_required && !fleet.present) || fleet_admitted,
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
            collection_attention: attention,
            readiness_detail,
            running: view.running,
            lease,
            cadence_error: cadence_violation
                .as_ref()
                .map(|violation| violation.report()),
            geocoder_enabled: config.geocoder.enabled,
            terrain_enabled: config.terrain.enabled,
            bind: config.bind.to_string(),
        },
        stream_audit,
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

fn stream_audit_recovered(audit: &StreamAuditSummary) -> bool {
    match (
        audit.last_subscription_success_at_ms,
        audit.last_failure_at_ms,
    ) {
        (_, None) => true,
        (Some(success), Some(failure)) => success >= failure,
        (None, Some(_)) => false,
    }
}

/// Cheap collector/serve startup log. It avoids retained-manifest parsing and
/// pack-directory walks so a large catalogue cannot delay HTTP readiness.
pub fn log_runtime_inventory(store: &HubStore, config: &HubConfig) {
    match store.runtime_inventory() {
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

    include!("diagnostics/readiness_tests.rs");

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
        assert_eq!(report.stream_audit.connect_attempts, 0);
        assert_eq!(report.stream_audit.unresolved_sessions, 0);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "streamRecovery" && check.passed)
        );
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
        assert_eq!(json["streamAudit"]["transportErrors"], 0);
    }

    #[test]
    fn doctor_flags_an_unrecovered_stream_failure_then_clears_after_success() {
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
        let correlation_id = Uuid::new_v4();
        let complete_subscribe = |outcome| {
            let receipt = store
                .begin_outbound_request(&crate::db::OutboundRequestStart {
                    correlation_id,
                    vehicle_tesla_id: Some(9),
                    transport: crate::db::OutboundRequestTransport::Stream,
                    operation: crate::db::OutboundRequestOperation::StreamSubscribe,
                    safety_class: crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                    precondition: crate::db::OutboundRequestPrecondition::NotRequired,
                })
                .expect("subscribe receipt");
            store
                .complete_outbound_request(
                    receipt,
                    &crate::db::OutboundRequestCompletion {
                        outcome,
                        http_status: None,
                        retry_after_seconds: None,
                    },
                )
                .expect("subscribe completion");
        };

        complete_subscribe(crate::db::OutboundRequestOutcome::TransportError);
        let degraded = inspect_hub(&store, &config).expect("degraded report");
        assert_eq!(degraded.status, "failed");
        assert!(
            degraded
                .checks
                .iter()
                .any(|check| check.name == "streamRecovery" && !check.passed)
        );

        complete_subscribe(crate::db::OutboundRequestOutcome::Success);
        let recovered = inspect_hub(&store, &config).expect("recovered report");
        assert_eq!(recovered.status, "ok");
        assert!(
            recovered
                .checks
                .iter()
                .any(|check| check.name == "streamRecovery" && check.passed)
        );
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

    /// A Hub directory with a valid stored pair (imported form) and a doctor
    /// configuration for it, for the credential status tests below.
    fn legacy_doctor_fixture() -> (tempfile::TempDir, HubStore, HubConfig) {
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
        (temporary, store, config)
    }

    #[test]
    fn doctor_shows_the_credential_status_to_a_process_that_only_opens_the_store() {
        let (temporary, store, config) = legacy_doctor_fixture();
        // Before the credential unit has run there is no status to show.
        let before = inspect_hub(&store, &config).expect("report before");
        let json = serde_json::to_value(&before.credentials.legacy).expect("JSON");
        assert_eq!(json["authState"], serde_json::Value::Null);
        assert_eq!(json["authReason"], serde_json::Value::Null);
        assert_eq!(json["retryAt"], serde_json::Value::Null);
        assert_eq!(json["collecting"], false);
        assert_eq!(json["clockSkew"], false);
        assert_eq!(json["validForCollection"], true);
        // The collector process stores `retrying(network)` with the token no
        // longer usable (test 89, C5R-12).
        let collector = HubStore::initialize(temporary.path()).expect("collector handle");
        let row = crate::db::CollectorAuthRow {
            state: "retrying".to_owned(),
            reason: Some("network".to_owned()),
            collecting: false,
            retry_at: Some(1_800_000_060),
            stopped_generation: None,
            stopped_mode: None,
            fuse_consecutive: 0,
            fuse_generation: None,
            fuse_last_rejection_at: None,
            fuse_pause_until: None,
            fuse_rejected_access_digest: None,
            clock_skew: true,
        };
        collector
            .write_collector_auth_row(&row, 1_800_000_000_000)
            .expect("status");
        // A second process that opens the store reads it from the diagnostics.
        let second = HubStore::initialize(temporary.path()).expect("second process");
        let report = inspect_hub(&second, &config).expect("report");
        let json = serde_json::to_value(&report).expect("report JSON");
        let legacy = &json["credentials"]["legacy"];
        assert_eq!(legacy["authState"], "retrying");
        assert_eq!(legacy["authReason"], "network");
        assert_eq!(legacy["retryAt"], 1_800_000_060_i64);
        assert_eq!(legacy["collecting"], false);
        assert_eq!(legacy["clockSkew"], true);
        // The existing keys keep their names and the new ones are additive.
        let mut keys: Vec<&str> = legacy
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "authReason",
                "authState",
                "clockSkew",
                "collecting",
                "expiresAt",
                "nextRefreshAt",
                "present",
                "retryAt",
                "validForCollection"
            ]
        );
        // Only codes, times and flags: every string value of the legacy block is a code.
        for value in legacy.as_object().expect("object").values() {
            if let Some(text) = value.as_str() {
                assert!(matches!(text, "retrying" | "network"), "{text}");
            }
        }
        assert!(!json.to_string().contains("exact TeslaMate key"));
    }

    #[test]
    fn valid_for_collection_is_false_only_for_signed_out_configuration_error_and_api_refused() {
        let (_temporary, store, config) = legacy_doctor_fixture();
        // The stops are tied to the stored pair's own generation: a stop of another
        // generation is lifted in the effective view (C5 R44a).
        let generation = store
            .load_teslamate_legacy_tokens()
            .expect("tokens")
            .expect("row")
            .credential_generation()
            .expect("bound generation");
        let row = |state: &str, reason: Option<&str>| crate::db::CollectorAuthRow {
            state: state.to_owned(),
            reason: reason.map(str::to_owned),
            collecting: matches!(state, "ready" | "refreshing" | "sign_in_required"),
            retry_at: None,
            stopped_generation: matches!(state, "sign_in_required" | "signed_out")
                .then_some(generation),
            stopped_mode: (state == "api_refused").then(|| "legacy".to_owned()),
            fuse_consecutive: 0,
            fuse_generation: None,
            fuse_last_rejection_at: None,
            fuse_pause_until: None,
            fuse_rejected_access_digest: None,
            clock_skew: false,
        };
        let cases: [(&str, Option<&str>, bool); 11] = [
            ("ready", None, true),
            ("refreshing", None, true),
            ("retrying", Some("network"), true),
            ("persistence_pending", None, true),
            ("awaiting_confirmation", Some("access_token_rejected"), true),
            ("observer_waiting", Some("token_expired"), true),
            ("clock_invalid", None, true),
            ("sign_in_required", Some("refresh_token_rejected"), true),
            ("signed_out", Some("access_token_rejected"), false),
            ("configuration_error", Some("region_conflict"), false),
            ("api_refused", Some("owner_api"), false),
        ];
        for (state, reason, valid) in cases {
            let mut stored = row(state, reason);
            if state == "awaiting_confirmation" {
                stored.fuse_consecutive = 2;
                stored.fuse_generation = Some(generation);
                stored.fuse_pause_until = Some(1);
            }
            store
                .write_collector_auth_row(&stored, 1_000)
                .expect("status row");
            let report = inspect_hub(&store, &config).expect("report");
            let legacy = &report.credentials.legacy;
            assert_eq!(legacy.valid_for_collection, valid, "{state}");
            assert_eq!(legacy.auth_state.as_deref(), Some(state));
            assert_eq!(legacy.auth_reason.as_deref(), reason);
            assert_eq!(legacy.collecting, stored.collecting, "{state}");
        }
        // An unresolved refresh still blocks, whatever the status says.
        store
            .write_collector_auth_row(&row("ready", None), 2_000)
            .expect("ready");
        let tokens = store
            .load_teslamate_legacy_tokens()
            .expect("tokens")
            .expect("row");
        store
            .begin_legacy_refresh(tokens.credential_generation().expect("generation"))
            .expect("begin");
        let report = inspect_hub(&store, &config).expect("report");
        assert!(!report.credentials.legacy.valid_for_collection);
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
