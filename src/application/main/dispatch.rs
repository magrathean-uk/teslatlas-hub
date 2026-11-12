// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, PartialEq, Eq)]
struct CurrentVehicleStatusFields {
    activity_state: Option<String>,
    battery_level: Option<i64>,
    location_name: Option<String>,
    telemetry_status: &'static str,
}

fn current_vehicle_status_fields(
    store: &HubStore,
    vehicle_id: uuid::Uuid,
    now_ms: i64,
    transient_max_age_ms: i64,
) -> Result<CurrentVehicleStatusFields, StoreError> {
    let observations = store.current_observations_for_vehicle(vehicle_id)?;
    let lifecycle = store.load_lifecycle_state(vehicle_id)?;
    let mut summary = build_current_vehicle_summary(
        vehicle_id,
        &observations,
        None,
        lifecycle.as_ref(),
        None,
        now_ms,
        transient_max_age_ms,
    );
    if let (Some(latitude), Some(longitude)) = (summary.latitude, summary.longitude) {
        summary.geofence_name = store.geofence_name_at(vehicle_id, latitude, longitude)?;
    }
    Ok(CurrentVehicleStatusFields {
        // The summary's activity vocabulary is the one C6 R29 defines: it never
        // holds `unavailable`, so no text filter applies.
        activity_state: summary
            .activity_state
            .map(|state| state.as_str().to_owned()),
        battery_level: summary.battery_level,
        location_name: summary.geofence_name,
        // Current observations are durable history. They prove cached telemetry,
        // not a live vehicle or provider connection.
        telemetry_status: if summary.observed_at_ms.is_some() {
            "cached"
        } else {
            "unknown"
        },
    })
}

/// The `status` JSON of one store under one loaded configuration (the whole
/// command output, so tests run the production assembly). Readiness comes from
/// the shared configuration-aware view: `ready` and `readinessReason` are the
/// service tier only; `collectionAttention` is the single owner-resolvable
/// condition, `readinessDetail` the healthy-only detail, `collectorRunning` the
/// lease witness, and `collectorCadenceError` appears only for an active invalid
/// cadence. The redacted lease timing stays in `collector`.
fn build_status_report(
    store: &HubStore,
    config: &HubConfig,
    now_ms: i64,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let vehicles = store.published_vehicles()?;
    let configured = store.configured_tesla_vehicles()?;
    let mut vehicle_summaries = Vec::with_capacity(vehicles.len());
    for vehicle in &vehicles {
        let binding = store.v2_projection_binding(vehicle.vehicle_id)?;
        let latest =
            store.latest_current_observation_metadata_for_vehicle(vehicle.vehicle_id)?;
        let current = current_vehicle_status_fields(
            store,
            vehicle.vehicle_id,
            now_ms,
            teslatlas_hub::current_state::offline_drive_timeout_ms(
                config.collector.offline_drive_timeout_seconds,
            ),
        )?;
        let tesla_eid = configured
            .iter()
            .find_map(|(vehicle_id, eid, _)| (*vehicle_id == vehicle.vehicle_id).then_some(*eid));
        vehicle_summaries.push(serde_json::json!({
            "vehicleId": vehicle.vehicle_id,
            "displayName": vehicle.display_name,
            "sourceCarId": binding.selected_car_id,
            "teslaEid": tesla_eid,
            "latestObservationId": latest.as_ref().map_or(0, |observation| observation.observation_id),
            "latestObservedAtMs": latest.as_ref().map(|observation| observation.observed_at_ms),
            "latestReceivedAtMs": latest.as_ref().map(|observation| observation.received_at_ms),
            "activityState": current.activity_state,
            "batteryLevel": current.battery_level,
            "locationName": current.location_name,
            "telemetryStatus": current.telemetry_status,
        }));
    }
    let vehicle = (vehicle_summaries.len() == 1).then(|| vehicle_summaries[0].clone());
    let credentials = teslatlas_hub::auth::credential_sign_in::credential_status(store, &config.data_dir, now_ms)?;
    let legacy_credentials = &credentials.legacy_credentials;
    let fleet_credentials = &credentials.fleet_credentials;
    let selected_credentials_present = match config.collector.provider {
        CollectorProvider::Legacy => legacy_credentials.present,
        CollectorProvider::Fleet => fleet_credentials.present,
    };
    // One configuration-aware read of the three readiness tiers: the loaded mode
    // decides whether the credential status applies, an active cadence fault is
    // attention, and a fault in an added read is reported as
    // `catalogue_unavailable` inside the view instead of failing the command.
    let collector_required = config.collector.interval_seconds > 0;
    let cadence_violation = collector_required
        .then(|| config.collector.cadence_violation())
        .flatten();
    let view = store.collector_readiness_view(&ReadinessContext::configured(
        collector_required,
        now_ms,
        collector_mode(&config.collector),
        cadence_violation.clone(),
    ));
    let collector = store.supervised_collector_lease_status()?;
    let database_bytes = fs::metadata(store.database_path())?.len();
    let credential_error = teslatlas_hub::auth::credential_sign_in::credential_store_unhealthy(&credentials,
        collector_required && config.collector.provider == CollectorProvider::Legacy,
        collector_required && config.collector.provider == CollectorProvider::Fleet);
    let readiness_reason = if credential_error { Some("credential_store_error") } else { view.service.as_ref().err().map(|failure| failure.code.code()) };

    let mut report = serde_json::json!({
        "status": "ok",
        "version": teslatlas_hub::BUILD_VERSION,
        "database": {
            "path": store.database_path(),
            "bytes": database_bytes,
        },
        "ready": view.service.is_ok() && !credential_error,
        "readinessReason": readiness_reason,
        "collectionAttention": view.attention,
        "readinessDetail": view.detail,
        "collectorRunning": view.running,
        "collector": collector,
        "provider": config.collector.provider,
        "vehicle": vehicle,
        "vehicles": vehicle_summaries,
        "credentials": {
            "present": selected_credentials_present,
        },
        "legacyCredentials": legacy_credentials,
        "fleetCredentials": {
            "present": fleet_credentials.present,
            "expiresAt": fleet_credentials.expires_at,
            "nextRefreshAt": fleet_credentials.next_refresh_at,
            "origin": fleet_credentials.origin,
            "refreshOwner": fleet_credentials.refresh_owner,
            "storeState": fleet_credentials.store_state,
            "scopes": null,
            "scopeStatus": if fleet_credentials.present { Some("unavailable") } else { None },
        },
        "credentialStore": credentials.credential_store,
        "fleetTelemetry": {
            "enabled": config.collector.fleet_telemetry.is_some(),
            "configured": config.collector.fleet_telemetry.is_some(),
            "mode": if config.collector.fleet_telemetry.is_some() {
                "native_push_configured"
            } else {
                "disabled"
            },
            "operationalState": if config.collector.fleet_telemetry.is_some() {
                "requires_receiver_and_vehicle_receipt_proof"
            } else {
                "disabled"
            },
            "paidVehicleDataPolling": config.collector.provider == CollectorProvider::Fleet
                && config.collector.interval_seconds > 0
                && config.collector.fleet_telemetry.is_none(),
            "deliveryPolicy": config.collector.fleet_telemetry.as_ref().map(|_| "latest"),
        },
    });
    if let Some(violation) = cadence_violation {
        report["collectorCadenceError"] = serde_json::to_value(violation.report())?;
    }
    // Refused credentials are kept until the owner removes them (DEC-083);
    // status names the command while any is kept.
    if let Some(hint) = operator::purge::hint(store)? {
        report["refusedCredentialPurge"] = hint;
    }
    Ok(report)
}

/// The bounded observation of one admitted Hub: the observer collector hands
/// over its cursor key, then the server starts, until `control` ends it. The
/// observer never takes the managed configuration wait: a cadence the
/// configuration breaks refuses before the handoff, so no listener is ever
/// built and the command fails with the exact cadence message instead of
/// appearing to succeed (B2 clarification 3).
#[cfg(unix)]
async fn run_observe_orchestration<Control>(
    store: HubStore,
    config: HubConfig,
    config_sha256: teslatlas_hub::protocol::Sha256Digest,
    admission: std::sync::Arc<AdmittedUserHub>,
    control: Control,
) -> std::io::Result<()>
where
    Control: Future<Output = MacServeControl>,
{
    let collector_store = store.clone();
    let collector_config = config.clone();
    let collector_admission = std::sync::Arc::clone(&admission);
    let server_config = config;
    let server_admission = admission;
    run_macos_serve_supervisor(
        true,
        move |ready, shutdown| async move {
            collector::run_observer_for_admitted_user(
                &collector_store,
                &collector_config,
                collector_admission,
                ready,
                async move {
                    let _ = shutdown.await;
                },
            )
            .await
            .map_err(std::io::Error::other)
        },
        move |cursor_key, shutdown| async move {
            server::serve_for_admitted_user(
                store,
                &server_config,
                config_sha256,
                server_admission,
                cursor_key,
                async move {
                    let _ = shutdown.await;
                },
            )
            .await
        },
        control,
    )
    .await
}

use teslatlas_hub::runtime::operator::{
    self as operator,
    admission::{OperatorRefusal, Route},
    channel::{OperatorRequest, SetupMaterial},
};

/// Largest `setup` input: the sign-in code material as one JSON object.
const SETUP_INPUT_MAX_BYTES: usize = 4096;
/// How long a forwarded command waits for the service's answer.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(180);

fn operator_home() -> Result<PathBuf, OperatorRefusal> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| OperatorRefusal::new("home_unavailable", "HOME is not set"))
}

fn setup_input_invalid(message: &str) -> OperatorRefusal {
    OperatorRefusal::new(operator::admission::SETUP_INPUT_INVALID, message)
}

fn code_text_valid(text: &str, maximum: usize) -> bool {
    !text.is_empty()
        && text.len() <= maximum
        && text.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Parses the sign-in code material (`code`, `codeVerifier`, `redirectUri`).
/// Any token-shaped key is a source credential and refused as one; no token
/// is ever accepted (design 1.1).
fn parse_setup_material(bytes: &[u8]) -> Result<SetupMaterial, OperatorRefusal> {
    let object: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(bytes)
        .map_err(|_| setup_input_invalid("setup reads one JSON object with code, codeVerifier and redirectUri"))?;
    if object.keys().any(|key| key.to_ascii_lowercase().contains("token")) {
        let error = teslatlas_hub::auth::credential_sign_in::source_credential_disabled();
        return Err(OperatorRefusal::new(error.code(), error.to_string()));
    }
    let field = |name: &str| object.get(name).and_then(serde_json::Value::as_str);
    if object.len() != 3 {
        return Err(setup_input_invalid("setup reads exactly code, codeVerifier and redirectUri"));
    }
    let (Some(code), Some(verifier), Some(redirect)) =
        (field("code"), field("codeVerifier"), field("redirectUri"))
    else {
        return Err(setup_input_invalid("setup reads exactly code, codeVerifier and redirectUri"));
    };
    if !code_text_valid(code, 2048) {
        return Err(setup_input_invalid("the authorisation code is empty, too long or not printable"));
    }
    if !(43..=128).contains(&verifier.len())
        || !verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
    {
        return Err(setup_input_invalid("the PKCE verifier is not 43 to 128 unreserved characters"));
    }
    let redirect_valid = code_text_valid(redirect, 2048)
        && url::Url::parse(redirect).is_ok_and(|url| {
            url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
        });
    if !redirect_valid {
        return Err(setup_input_invalid("the redirect URI is not an https or loopback address"));
    }
    Ok(SetupMaterial {
        code: code.to_owned(),
        code_verifier: verifier.to_owned(),
        redirect_uri: redirect.to_owned(),
    })
}

/// Reads one bounded, non-secret input file (a grant or a trust document).
fn read_bounded_input(path: &Path, maximum: usize, what: &str) -> Result<Vec<u8>, OperatorRefusal> {
    let file = fs::File::open(path)
        .map_err(|_| OperatorRefusal::new("input_unreadable", format!("the {what} file cannot be read")))?;
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| OperatorRefusal::new("input_unreadable", format!("the {what} file cannot be read")))?;
    if bytes.len() > maximum {
        return Err(OperatorRefusal::new(
            "input_too_large",
            format!("the {what} file is larger than {maximum} bytes"),
        ));
    }
    Ok(bytes)
}

fn absolute_path(path: &Path) -> Result<PathBuf, OperatorRefusal> {
    std::path::absolute(path)
        .map_err(|_| OperatorRefusal::new("input_unreadable", "a path cannot be made absolute"))
}

fn encoded(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Forwards one service verb to the running service (ruling R5). Only the
/// configuration is read before the channel is connected and the service on
/// it authenticated, so with no service running, or another program on the
/// socket, the command is refused having read no secret path or standard
/// input and opened no network connection; its input then travels on that
/// same authenticated connection. It never takes the data-directory
/// lifetime lock.
fn run_forwarded(config_path: &Path, command: &Command) -> Result<(), Box<dyn std::error::Error>> {
    let config = HubConfig::load(config_path)?;
    let data = config.data_dir.as_path();
    let stream = operator::channel::connect(data)?;
    let request = match command {
        #[cfg(unix)]
        Command::Setup {
            vehicle_id,
            all_vehicles,
        } => {
            let bytes = read_bounded_migration_secret(std::io::stdin(), SETUP_INPUT_MAX_BYTES)
                .map_err(|_| setup_input_invalid("setup input is unreadable or larger than 4 KiB"))?;
            OperatorRequest::Setup {
                material: parse_setup_material(&bytes)?,
                vehicle_id: *vehicle_id,
                all_vehicles: *all_vehicles,
            }
        }
        Command::Window { command } => match command {
            WindowCommand::Status => OperatorRequest::WindowStatus,
            WindowCommand::Close => OperatorRequest::WindowClose,
            WindowCommand::Trust { file } => OperatorRequest::WindowTrust {
                document: encoded(&read_bounded_input(
                    file,
                    operator::authority::TRUST_DOCUMENT_MAX_BYTES,
                    "trust document",
                )?),
            },
            WindowCommand::Open { grant } => OperatorRequest::WindowOpen {
                grant: encoded(&read_bounded_input(
                    grant,
                    teslatlas_hub::collection::window::GRANT_MAX_BYTES,
                    "grant",
                )?),
            },
            _ => return Err("this window verb runs on the issuer, not the service".into()),
        },
        // The recovery key stays where it is: the service reads it.
        Command::ExportRecoveryCredentials {
            destination,
            recovery_key_file,
        } => OperatorRequest::RecoveryExport {
            destination: absolute_path(destination)?,
            recovery_key_file: absolute_path(recovery_key_file)?,
        },
        Command::RestoreRecoveryCredentials {
            source,
            recovery_key_file,
        } => OperatorRequest::RecoveryRestore {
            source: absolute_path(source)?,
            recovery_key_file: absolute_path(recovery_key_file)?,
        },
        Command::PurgeRefusedCredentials { confirm } => {
            OperatorRequest::PurgeRefusedCredentials { confirm: *confirm }
        }
        // The password stays in its file: the service reads it.
        Command::BackupNow {
            destination,
            password_file,
        } => OperatorRequest::BackupNow {
            destination: destination.as_deref().map(absolute_path).transpose()?,
            password_file: password_file.as_deref().map(absolute_path).transpose()?,
        },
        Command::BackupStatus => OperatorRequest::BackupStatus,
        _ => return Err("this command is not forwarded to the service".into()),
    };
    let result = operator::channel::exchange(stream, &request, FORWARD_TIMEOUT)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

/// The issuer verbs and `window host-id`, on this Mac with no service.
fn run_issuer(command: &Command) -> Result<(), Box<dyn std::error::Error>> {
    let Command::Window { command } = command else {
        return Err("not an issuer verb".into());
    };
    let report = operator::issuer::run_issuer_command(command, &operator_home()?)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

/// Import and data restore wait until no window is open on this host and
/// the selected Hub's window is Closed with its handback recorded (design
/// 2.10). A data restore with no Hub configuration is still bound to the
/// host's collection authority: it proceeds only while no window holds the
/// host claim, and holds it shared until it ends.
fn admit_import_command(
    command: &Command,
    config_path: &Path,
) -> Result<Option<operator::admission::ImportAdmission>, Box<dyn std::error::Error>> {
    let home = operator_home()?;
    if matches!(command, Command::RestoreData { .. })
        && matches!(fs::symlink_metadata(config_path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(Some(operator::admission::admit_import(None, &home)?));
    }
    let config = HubConfig::load(config_path)?;
    Ok(Some(operator::admission::admit_import(Some(&config.data_dir), &home)?))
}

/// What only the binary does inside the running service.
#[cfg(unix)]
struct HubServiceBackend {
    store: HubStore,
    data_dir: PathBuf,
    admission: std::sync::Arc<AdmittedUserHub>,
    backup: std::sync::Arc<teslatlas_hub::runtime::backup_schedule::BackupScheduler>,
}

#[cfg(unix)]
impl HubServiceBackend {
    fn recovery_key(
        path: &Path,
    ) -> Result<zeroize::Zeroizing<[u8; RECOVERY_ENCRYPTION_KEY_BYTES]>, OperatorRefusal> {
        read_recovery_encryption_key(path)
            .map_err(|error| OperatorRefusal::new("recovery_key_invalid", error.to_string()))
    }
}

#[cfg(unix)]
impl operator::authority::ServiceBackend for HubServiceBackend {
    fn setup(
        &self,
        material: SetupMaterial,
        vehicle_id: Option<i64>,
        all_vehicles: bool,
    ) -> Result<serde_json::Value, OperatorRefusal> {
        // DEC-088: the service exchanges the owner's code itself, with the
        // standard public owner client, at the owner token endpoint. The
        // window is Open here, and the exchange still asks the egress gate
        // immediately before its one send. Only that exchange's answer is
        // stored, with origin hub_login; any refusal leaves the store as it
        // was and names a fixed code, never the material.
        let entry = open_command_credential_vault(
            self.store.clone(),
            std::sync::Arc::clone(&self.admission),
            &self.data_dir,
        )
        .map_err(|error| {
            setup_refusal(teslatlas_hub::auth::credential_sign_in::credential_entry_error(error))
        })?;
        run_owner_exchange(|| {
            teslatlas_hub::auth::credential_sign_in::exchange_owner_code(
                &entry,
                &material.code,
                &material.code_verifier,
                &material.redirect_uri,
            )
        })
        .map_err(setup_refusal)?;
        drop(material);
        // Vehicle discovery follows inside the same window. Its entry
        // (collector native setup) still refuses every pair and is rewired
        // separately, so the selection is reported as not yet run.
        let _ = (vehicle_id, all_vehicles);
        Ok(owner_setup_report())
    }

    fn recovery_export(
        &self,
        destination: &Path,
        recovery_key_file: &Path,
    ) -> Result<serde_json::Value, OperatorRefusal> {
        let key = Self::recovery_key(recovery_key_file)?;
        let report = export_credentials(&self.store, &self.data_dir, destination, &key[..])
            .map_err(|error| OperatorRefusal::new("recovery_failed", error.to_string()))?;
        serde_json::to_value(&report)
            .map_err(|_| OperatorRefusal::new("recovery_failed", "the report is not encodable"))
    }

    fn recovery_restore(
        &self,
        source: &Path,
        recovery_key_file: &Path,
    ) -> Result<serde_json::Value, OperatorRefusal> {
        let key = Self::recovery_key(recovery_key_file)?;
        let entry = open_command_credential_vault(
            self.store.clone(),
            std::sync::Arc::clone(&self.admission),
            &self.data_dir,
        )
        .map_err(|error| {
            let error = teslatlas_hub::auth::credential_sign_in::credential_entry_error(error);
            OperatorRefusal::new(error.code(), error.to_string())
        })?;
        let report = teslatlas_hub::credential_recovery::restore_credentials_from_entry(
            &entry, source, &key[..],
        )
        .map_err(|error| OperatorRefusal::new("recovery_failed", error.to_string()))?;
        serde_json::to_value(&report)
            .map_err(|_| OperatorRefusal::new("recovery_failed", "the report is not encodable"))
    }

    fn refused_credentials(&self) -> Result<serde_json::Value, OperatorRefusal> {
        operator::purge::list(&self.store, &self.data_dir)
    }

    fn purge_refused_credentials(&self) -> Result<serde_json::Value, OperatorRefusal> {
        let entry = open_command_credential_vault(
            self.store.clone(),
            std::sync::Arc::clone(&self.admission),
            &self.data_dir,
        )
        .map_err(|error| {
            let error = teslatlas_hub::auth::credential_sign_in::credential_entry_error(error);
            OperatorRefusal::new(error.code(), error.to_string())
        })?;
        operator::purge::purge(&entry)
    }

    fn backup_now(
        &self,
        destination: Option<&Path>,
        password_file: Option<&Path>,
    ) -> Result<serde_json::Value, OperatorRefusal> {
        self.backup.run_now(destination, password_file)
    }

    fn backup_status(&self) -> Result<serde_json::Value, OperatorRefusal> {
        Ok(self.backup.status())
    }
}

/// The refusal `setup` returns: the fixed code and its fixed owner text.
#[cfg(unix)]
fn setup_refusal(
    error: teslatlas_hub::auth::credential_sign_in::CredentialCommandError,
) -> OperatorRefusal {
    OperatorRefusal::new(error.code(), error.to_string())
}

/// What a successful `setup` reports: no token, code or account detail.
#[cfg(unix)]
fn owner_setup_report() -> serde_json::Value {
    serde_json::json!({
        "status": "signed_in",
        "slot": "owner_api",
        "origin": "hub_login",
        "vehicleDiscovery": "not_run",
    })
}

/// Runs one code exchange to completion on a thread and runtime of its own,
/// so the channel's handler thread never blocks inside another runtime.
#[cfg(unix)]
fn run_owner_exchange<'a, F>(
    exchange: impl FnOnce() -> F + Send + 'a,
) -> Result<(), teslatlas_hub::auth::credential_sign_in::CredentialCommandError>
where
    F: std::future::Future<
            Output = Result<(), teslatlas_hub::auth::credential_sign_in::CredentialCommandError>,
        > + 'a,
{
    use teslatlas_hub::auth::credential_sign_in::sign_in_unavailable;
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| sign_in_unavailable())?;
                runtime.block_on(exchange())
            })
            .join()
    })
    .unwrap_or_else(|_| Err(sign_in_unavailable()))
}

#[cfg(all(test, unix))]
type CommandEntryOpener = fn(HubStore, std::sync::Arc<AdmittedUserHub>, &Path)
    -> Result<teslatlas_hub::CredentialVaultEntry, teslatlas_hub::CredentialVaultEntryError>;
#[cfg(all(test, unix))]
thread_local! {
    static COMMAND_ENTRY_OPENER: std::cell::Cell<Option<CommandEntryOpener>> = const { std::cell::Cell::new(None) };
}
#[cfg(unix)]
fn open_command_credential_vault(store: HubStore, admission: std::sync::Arc<AdmittedUserHub>, data: &Path)
    -> Result<teslatlas_hub::CredentialVaultEntry, teslatlas_hub::CredentialVaultEntryError>
{
    #[cfg(test)]
    if let Some(open) = COMMAND_ENTRY_OPENER.with(std::cell::Cell::get) { return open(store, admission, data); }
    #[cfg(feature = "credential-relocation-fixture")]
    { return teslatlas_hub::CredentialVaultEntry::open_for_test(store, admission, data, true); }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
    teslatlas_hub::open_credential_vault_for_entry(store, admission, data)
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "credential-relocation-fixture")]
    credential_relocation_fixture::require_dispatch(&cli)?;
    let config_path = cli.config.unwrap_or_else(default_config_path);
    // Admission (M02-T10, rulings R5 and R6): every command is routed before
    // it reads a secret path, standard input or the network.
    let route = operator::admission::route(&cli.command);
    match route {
        Route::Refused(code) => {
            return Err(match code {
                operator::admission::PROVIDER_PARKED => OperatorRefusal::provider_parked(),
                other => OperatorRefusal::new(other, "this command is refused"),
            }
            .into());
        }
        Route::Service => return run_forwarded(&config_path, &cli.command),
        Route::Issuer => return run_issuer(&cli.command),
        Route::Import | Route::Local => {}
    }

    if let Command::Healthcheck {
        ca_file,
        server_name,
    } = &cli.command
    {
        return run_container_healthcheck(ca_file, server_name).await;
    }

    #[cfg(unix)]
    if let Command::SourceCheck {
        source,
        car_id,
        postgres_password_file,
        accept_schema_evidence,
    } = &cli.command
    {
        // Source checks never inspect source credentials (M02-T02).
        return run_source_check(
            source,
            *car_id,
            postgres_password_file,
            true,
            *accept_schema_evidence,
        )
        .await;
    }

    #[cfg(unix)]
    if let Command::Migrate {
        accept_schema_evidence: false,
        history_only,
        ..
    } = &cli.command
    {
        return Err(if *history_only {
            "history-only import requires --accept-schema-evidence; the database schema is reviewed but the source application version remains unknown"
        } else {
            "History import requires --accept-schema-evidence after confirming source application release 4.2.0 or later"
        }.into());
    }

    // Import and data restore are admitted only with the window Closed; the
    // admission is held until the command ends.
    let _import_admission = if route == Route::Import {
        admit_import_command(&cli.command, &config_path)?
    } else {
        None
    };

    #[cfg(target_os = "macos")]
    if let Command::ServePreflight { mode, history_only } = &cli.command {
        let config = HubConfig::load(&config_path)?;
        if *history_only {
            if *mode != DevelopmentServeModeArgument::Standalone {
                return Err("history-only Serve preflight requires standalone mode".into());
            }
            validate_history_only_config(&config)?;
        }
        teslatlas_hub::macos_launch_agent::preflight_hub_for_serve(
            &config,
            Some(mode.serve_mode()),
        )?;
        println!(
            "{}",
            serde_json::json!({
                "status": "ready",
                "mode": mode.as_str(),
            })
        );
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    if let Command::Service { command } = &cli.command {
        let status = match command {
            ServiceCommand::Status => teslatlas_hub::linux_systemd::status()?,
            ServiceCommand::Start => teslatlas_hub::linux_systemd::apply(
                teslatlas_hub::linux_systemd::ServiceAction::Start,
            )?,
            ServiceCommand::Stop => teslatlas_hub::linux_systemd::apply(
                teslatlas_hub::linux_systemd::ServiceAction::Stop,
            )?,
            ServiceCommand::Restart => teslatlas_hub::linux_systemd::apply(
                teslatlas_hub::linux_systemd::ServiceAction::Restart,
            )?,
        };
        println!(
            "{}",
            serde_json::json!({
                "status": status.status(),
                "unit": status.unit,
                "loadState": status.load_state,
                "activeState": status.active_state,
                "subState": status.sub_state,
            })
        );
        return Ok(());
    }

    if let Command::Control {
        vehicle_id,
        command,
    } = &cli.command
    {
        return run_control(&config_path, *vehicle_id, command).await;
    }

    // Like `control` this takes one short SQLite transaction and no instance lock,
    // so it works while the collector runs.
    if let Command::Collector { command } = &cli.command {
        return run_collector_command(&config_path, command);
    }

    #[cfg(not(unix))]
    if matches!(&cli.command, Command::Serve) {
        return Err("serve requires a Unix platform".into());
    }

    // Long-lived service, import, credential, and recovery commands take the
    // local instance lock. `control` uses short SQLite transactions so it can
    // intentionally operate while the collector is running.
    #[cfg(unix)]
    let admitted_user_hub = if operator::admission::requires_user_hub_admission(&cli.command) {
        let config = HubConfig::load(&config_path)?;
        if matches!(
            &cli.command,
            Command::Migrate {
                history_only: true,
                ..
            }
        ) {
            validate_history_only_config(&config)?;
        }
        Some(AdmittedUserHub::admit(&config.data_dir)?)
    } else {
        None
    };

    #[cfg(unix)]
    if let Command::Migrate {
        source,
        car_id,
        source_key,
        new_import_source,
        postgres_password_file,
        history_only,
        online_snapshot,
        accept_schema_evidence: _,
    } = &cli.command
    {
        let start_hub = run_macos_migration_prepared(
            admitted_user_hub
                .as_ref()
                .ok_or("migration reached runtime without user admission")?,
            MacMigrationInput {
                config_path: &config_path,
                source_url: source,
                car_id: *car_id,
                source_key: source_key.as_deref(),
                new_import_source: *new_import_source,
                postgres_password_file,
                history_only: *history_only,
                // Source credentials are disabled: no key or token input exists.
                encryption_key_file: None,
                access_token_file: None,
                refresh_token_file: None,
                online_snapshot: *online_snapshot,
                preserve_existing_credentials: false,
            },
            None,
            false,
        )
        .await?;
        drop(admitted_user_hub);
        #[cfg(target_os = "macos")]
        if start_hub {
            println!("{}", macos_migration_start_request());
        }
        #[cfg(target_os = "linux")]
        if start_hub {
            println!(
                "{}",
                serde_json::json!({
                    "serviceStartRequested": true,
                    "next": "sudo systemctl start teslatlas-hub.service",
                })
            );
        }
        return Ok(());
    }

    match &cli.command {
        Command::VerifyBackup { source } => {
            let report = verify_data_backup(source)?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        Command::RestoreData {
            source,
            destination,
        } => {
            let report = restore_data_backup(source, destination)?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        _ => {}
    }

    match &cli.command {
        Command::Legal => {
            println!("{}", teslatlas_hub::legal_notice());
            return Ok(());
        }
        Command::Source => {
            let source = teslatlas_hub::corresponding_source_url()
                .ok_or(teslatlas_hub::UNBOUND_SOURCE_ERROR)?;
            println!("{source}");
            return Ok(());
        }
        Command::Doctor => {
            let config = HubConfig::load(&config_path)?;
            let (report, purge_hint) = run_immutable_diagnostic(&config.data_dir, |store| {
                Ok((inspect_hub(store, &config)?, operator::purge::hint(store)?))
            })?;
            report.log();
            // The doctor names the purge command while refused credentials
            // are kept (DEC-083).
            let mut printed = serde_json::to_value(&report)?;
            if let Some(hint) = purge_hint {
                printed["refusedCredentialPurge"] = hint;
            }
            println!("{}", serde_json::to_string_pretty(&printed)?);
            if !report.is_ok() {
                return Err(std::io::Error::other("doctor found failures; see JSON report").into());
            }
            return Ok(());
        }
        Command::Status => {
            #[cfg(feature = "credential-relocation-fixture")]
            teslatlas_hub::platform::r78_recording::with_fixture_backend(|backend| {
                backend.loaded_query()?;
                backend.pid_query()?;
                backend.plist_decode()?;
                Ok(())
            })?;
            let config = HubConfig::load(&config_path)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let report = build_status_report(&store, &config, current_epoch_ms()?)?;
            println!("{report}");
            return Ok(());
        }
        #[cfg(unix)]
        Command::Preflight { history_only } => {
            let config = HubConfig::load(&config_path)?;
            if *history_only {
                validate_history_only_config(&config)?;
                let published = run_immutable_diagnostic(&config.data_dir, |store| {
                    store
                        .service_readiness_at(false, current_epoch_ms()?)
                        .map_err(|error| {
                            format!("published history is not ready: {:?}", error.code)
                        })?;
                    let published = store.published_vehicles()?;
                    if published.is_empty() {
                        return Err("no signed history is published".into());
                    }
                    Ok(published.len())
                })?;
                println!(
                    "{}",
                    serde_json::json!({"status": "ready", "credentialScope": "history-only", "publishedVehicles": published, "collectorEnabled": false})
                );
                return Ok(());
            }
            run_immutable_diagnostic(&config.data_dir, |store| {
                store.catalogue_check()?;
                let configured = store.configured_tesla_vehicles()?;
                if configured.is_empty() {
                    return Err("at least one configured vehicle is required".into());
                }
                let report = inspect_hub(store, &config)?;
                let present = match config.collector.provider {
                    CollectorProvider::Legacy => report.credentials.legacy.present,
                    CollectorProvider::Fleet => report.credentials.fleet.present,
                };
                if !present || !report.checks.iter().any(|check| check.name == "selectedProviderCredentials" && check.passed) {
                    let status = teslatlas_hub::auth::credential_sign_in::credential_status(store, &config.data_dir, current_epoch_ms()?)?;
                    return Err(teslatlas_hub::auth::credential_sign_in::diagnostic_admission_error(&status, config.collector.provider == CollectorProvider::Fleet).into());
                }
                Ok(())
            })?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "ready",
                    "version": teslatlas_hub::BUILD_VERSION,
                    "provider": config.collector.provider,
                })
            );
            return Ok(());
        }
        Command::ObservationWatermark { car_id } => {
            let config = HubConfig::load(&config_path)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let watermark = match store.observation_watermark(*car_id) {
                Ok(watermark) => watermark,
                Err(error) => {
                    return Err(observation_command_error(
                        "observation-watermark",
                        *car_id,
                        error,
                    ));
                }
            };
            println!(
                "{}",
                serde_json::json!({
                    "status": "captured",
                    "command": "observation-watermark",
                    "carId": watermark.source_car_id,
                    "sourceId": watermark.source_id,
                    "vehicleId": watermark.vehicle_id,
                    "watermark": watermark.observation_id,
                    "observedAtMs": watermark.observed_at_ms,
                    "receivedAtMs": watermark.received_at_ms,
                })
            );
            return Ok(());
        }
        Command::VerifyObservation { car_id, watermark } => {
            let config = HubConfig::load(&config_path)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let verification = match store.verify_observation_after(*car_id, *watermark) {
                Ok(verification) => verification,
                Err(error) => {
                    return Err(observation_command_error(
                        "verify-observation",
                        *car_id,
                        error,
                    ));
                }
            };
            let verified = verification.verified();
            println!(
                "{}",
                serde_json::json!({
                    "status": if verified { "verified" } else { "not_verified" },
                    "command": "verify-observation",
                    "verified": verified,
                    "carId": verification.source_car_id,
                    "sourceId": verification.source_id,
                    "vehicleId": verification.vehicle_id,
                    "afterWatermark": verification.after_observation_id,
                    "latestObservationId": verification.latest_observation_id,
                    "latestObservedAtMs": verification.latest_observed_at_ms,
                    "latestReceivedAtMs": verification.latest_received_at_ms,
                })
            );
            if !verified {
                return Err("no strictly newer durable observation".into());
            }
            return Ok(());
        }
        _ => {}
    }

    #[cfg(unix)]
    if let Some(admission) = admitted_user_hub.as_ref() {
        admission.assert_sensitive_access()?;
    }
    let (config, config_sha256) = HubConfig::load_with_digest(&config_path)?;
    #[cfg(unix)]
    if let Some(admission) = admitted_user_hub.as_ref() {
        admission.assert_store_path(&config.data_dir)?;
    }
    let store = HubStore::initialize(&config.data_dir)?;
    let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
    match cli.command {
        Command::Init => {
            println!("initialized {}", store.database_path().display());
        }
        #[cfg(unix)]
        Command::Bootstrap => {
            let admission = admitted_user_hub.as_ref().cloned().ok_or("Bootstrap reached runtime without user admission")?;
            let _entry = open_command_credential_vault(store.clone(), admission, &config.data_dir)
                .map_err(teslatlas_hub::auth::credential_sign_in::credential_entry_error)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "bootstrapped",
                    "version": teslatlas_hub::BUILD_VERSION,
                    "database": store.database_path(),
                })
            );
        }
        #[cfg(unix)]
        Command::Setup { .. } | Command::SetupFleet | Command::ConfigureFleetTelemetry => {
            unreachable!("setup is forwarded to the service and Fleet verbs are refused before Hub state opens")
        }
        Command::Legal
        | Command::Source
        | Command::Healthcheck { .. }
        | Command::Doctor
        | Command::Status
        | Command::SourceCheck { .. }
        | Command::Control { .. }
        | Command::Collector { .. }
        | Command::ObservationWatermark { .. }
        | Command::VerifyObservation { .. }
        | Command::Window { .. } => {
            unreachable!("read-only commands return before opening writable Hub state")
        }
        #[cfg(target_os = "linux")]
        Command::Service { .. } => {
            unreachable!("service control returns before opening writable Hub state")
        }
        #[cfg(unix)]
        Command::Preflight { .. } => {
            unreachable!("preflight returns before opening writable Hub state")
        }
        #[cfg(target_os = "macos")]
        Command::ServePreflight { .. } => {
            unreachable!("serve preflight returns before opening writable Hub state")
        }
        Command::Serve => {
            #[cfg(unix)]
            {
                #[cfg(target_os = "macos")]
                let summary_now_ms = {
                    store.checkpoint_catalogue_for_immutable_read()?;
                    let development_mode =
                        teslatlas_hub::macos_launch_agent::development_serve_mode(
                            std::env::var_os(
                                teslatlas_hub::macos_launch_agent::DEVELOPMENT_SERVE_ENV,
                            )
                            .as_deref(),
                            std::env::var_os(
                                teslatlas_hub::macos_launch_agent::DEVELOPMENT_SERVE_MODE_ENV,
                            )
                            .as_deref(),
                        )?;
                    teslatlas_hub::macos_launch_agent::preflight_hub_for_serve(
                        &config,
                        development_mode,
                    )?;
                    // The Fixture mode serves the fixture's own measurement
                    // time (C6 R28 (c)); every other mode keeps the system clock.
                    teslatlas_hub::macos_launch_agent::fixture_summary_now_ms(
                        development_mode,
                        &config.data_dir,
                    )?
                };
                #[cfg(not(target_os = "macos"))]
                let summary_now_ms: Option<i64> = None;
                let admission =
                    admitted_user_hub.ok_or("Serve reached runtime without user admission")?;
                admission.assert_sensitive_access()?;
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm = signal(SignalKind::terminate())?;

                // The service holds the window authority: one controller on
                // the process-wide egress gate. The collector runs only inside
                // an Open window, under the supervisor; the API is served
                // whatever the collector does (design 2.8, ruling R5).
                let collector_ready = collector_can_start(&store, &config)?;
                log_runtime_inventory(&store, &config);
                if collector_ready {
                    // Once per start: a clamped stream timeout or an ignored
                    // key (C3 R21).
                    config.collector.log_start_notices();
                }
                // A data-only restore leaves signed data without its cursor
                // key: the service then waits in its recovery state, before
                // the API could create a new key, until the operator's
                // restore-recovery-credentials restores it (review finding 2).
                let recovery = operator::authority::RecoveryGate::new(
                    operator::authority::credential_recovery_pending(&config.data_dir),
                );
                let (authority, resumed) = operator::authority::start_service_authority(
                    &config.data_dir,
                    &operator_home()?,
                )?;
                tracing::info!(
                    collector_ready,
                    window = ?authority.status().phase,
                    resumed = ?resumed,
                    provider = ?config.collector.provider,
                    interval_seconds = config.collector.interval_seconds,
                    bind = %config.bind,
                    source_db_current_enabled = config.import_source.current.is_some(),
                    "Hub serve starting"
                );
                let backup_schedule = start_backup_schedule(&config, &store, &recovery)?;
                let supervisor = operator::authority::SupervisorThread::spawn(
                    std::sync::Arc::clone(&authority),
                    ServiceCollectorSlot::new(
                        tokio::runtime::Handle::current(),
                        store.clone(),
                        config.clone(),
                        std::sync::Arc::clone(&admission),
                        std::sync::Arc::clone(&recovery),
                    ),
                )?;
                let channel = operator::channel::ChannelServer::bind(
                    &config.data_dir,
                    std::sync::Arc::new(operator::authority::OperatorService::new(
                        std::sync::Arc::clone(&authority),
                        std::sync::Arc::new(HubServiceBackend {
                            store: store.clone(),
                            data_dir: config.data_dir.clone(),
                            admission: std::sync::Arc::clone(&admission),
                            backup: std::sync::Arc::clone(&backup_schedule),
                        }),
                        supervisor.kicker(),
                        operator::authority::service_trust_authority(authority.host_id()),
                        &config.data_dir,
                    )
                    .with_recovery(std::sync::Arc::clone(&recovery))),
                )?;
                let recovery_stop = if recovery.pending() {
                    tracing::warn!(
                        "this Hub's signed data has no cursor key; the API starts after restore-recovery-credentials"
                    );
                    tokio::select! {
                        () = recovery.restored() => None,
                        error = admission.wait_until_invalid() => {
                            Some(Err(std::io::Error::other(error)))
                        }
                        _ = tokio::signal::ctrl_c() => Some(Ok(())),
                        _ = sigterm.recv() => Some(Ok(())),
                    }
                } else {
                    None
                };
                let server_config = config;
                let server_admission = std::sync::Arc::clone(&admission);
                let control_admission = std::sync::Arc::clone(&admission);
                #[cfg(target_os = "macos")]
                let command_proxy = mac_command_proxy_spec(&server_config)?;
                #[cfg(not(target_os = "macos"))]
                let command_proxy = None;
                let serve_result = if let Some(stopped) = recovery_stop {
                    stopped
                } else {
                    run_macos_serve_with_optional_proxy(
                        command_proxy,
                        false,
                        |_ready, _shutdown| async { Ok(()) },
                        move |cursor_key, shutdown| async move {
                            server::serve_for_admitted_user_at(
                                store,
                                &server_config,
                                config_sha256,
                                server_admission,
                                cursor_key,
                                summary_now_ms,
                                async move {
                                    let _ = shutdown.await;
                                },
                            )
                            .await
                        },
                        async move {
                            tokio::select! {
                                error = control_admission.wait_until_invalid() => {
                                    MacServeControl::AdmissionInvalidated(std::io::Error::other(error))
                                }
                                _ = tokio::signal::ctrl_c() => MacServeControl::Shutdown,
                                _ = sigterm.recv() => MacServeControl::Shutdown,
                            }
                        },
                    )
                    .await
                };
                drop(channel);
                tokio::task::spawn_blocking(move || backup_schedule.stop()).await?;
                tokio::task::spawn_blocking(move || supervisor.stop()).await?;
                drop(authority);
                match &serve_result {
                    Ok(()) => tracing::info!("Hub serve stopped cleanly"),
                    Err(error) => tracing::error!(%error, "Hub serve stopped unexpectedly"),
                }
                serve_result?;
            }
        }
        #[cfg(unix)]
        Command::Observe { duration_seconds } => {
            let admission =
                admitted_user_hub.ok_or("Observe reached runtime without user admission")?;
            admission.assert_sensitive_access()?;
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm = signal(SignalKind::terminate())?;

            // Observation sends Tesla traffic, so it needs an Open window,
            // held here for the observation's life and capped at its expiry.
            let (authority, _) = operator::authority::start_service_authority(
                &config.data_dir,
                &operator_home()?,
            )?;
            let window = authority.status();
            if window.phase != teslatlas_hub::collection::window::Phase::Open {
                return Err(OperatorRefusal::new(
                    operator::authority::WINDOW_CLOSED,
                    "observe needs an open collection window",
                )
                .into());
            }
            let left = window.deadline.unwrap_or(0) - authority.clock().wall();
            let duration_seconds = duration_seconds.min(u64::try_from(left).unwrap_or(0));
            let control_admission = std::sync::Arc::clone(&admission);
            tracing::info!(
                duration_seconds,
                provider = ?config.collector.provider,
                "Hub bounded observation starting"
            );
            // The window is supervised for the observation's whole life: its
            // clock and time checks run throughout, a window that leaves Open
            // stops the observation, and a close is handed back only after
            // the observation has released its collector lease.
            let (observe_result, handback) = operator::authority::supervise_observation(
                std::sync::Arc::clone(&authority),
                Duration::from_secs(teslatlas_hub::collection::window::SUPERVISOR_POLL_SECONDS as u64),
                |window_left| {
                    run_observe_orchestration(
                        store,
                        config,
                        config_sha256,
                        admission,
                        async move {
                            tokio::select! {
                                error = control_admission.wait_until_invalid() => {
                                    MacServeControl::AdmissionInvalidated(std::io::Error::other(error))
                                }
                                _ = tokio::signal::ctrl_c() => MacServeControl::Shutdown,
                                _ = sigterm.recv() => MacServeControl::Shutdown,
                                _ = tokio::time::sleep(Duration::from_secs(duration_seconds)) => MacServeControl::Shutdown,
                                _ = window_left => MacServeControl::Shutdown,
                            }
                        },
                    )
                },
            )
            .await;
            match &observe_result {
                Ok(()) => tracing::info!("Hub bounded observation stopped cleanly"),
                Err(error) => {
                    tracing::error!(%error, "Hub bounded observation stopped unexpectedly")
                }
            }
            match handback {
                Ok(Some(handback)) => tracing::info!(reason = handback.reason.code(), "collection window closed"),
                Ok(None) => {}
                Err(refusal) => tracing::warn!(code = %refusal.code, "collection window handback failed"),
            }
            drop(authority);
            observe_result?;
        }
        #[cfg(unix)]
        Command::Migrate { .. } => {
            unreachable!("migration returns before opening common Hub state")
        }
        Command::Pair {
            label,
            expires_in_seconds,
            json,
            vehicles,
            all_vehicles,
        } => {
            let mut stdout = std::io::stdout().lock();
            pair_with_configured_tls(
                &store,
                config.tls.as_ref(),
                &label,
                expires_in_seconds,
                json,
                pairing_scope_from_flags(vehicles, all_vehicles),
                &mut stdout,
            )
            .await?;
        }
        Command::Repair => {
            let report = store.repair()?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Backup { destination } => {
            // A refusal caused by refused pre-upgrade credentials names the
            // command that removes them (DEC-083).
            let report = create_data_backup(&store, &destination).map_err(
                |error| -> Box<dyn std::error::Error> {
                    match teslatlas_hub::data_recovery::refused_credential_backup_message(&store, &error) {
                        Some(message) => message.into(),
                        None => error.into(),
                    }
                },
            )?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::ExportRecoveryCredentials { .. }
        | Command::RestoreRecoveryCredentials { .. }
        | Command::PurgeRefusedCredentials { .. }
        | Command::BackupNow { .. }
        | Command::BackupStatus => {
            unreachable!("credential recovery, the purge and the backup verbs are forwarded to the running service")
        }
        Command::VerifyBackup { .. } | Command::RestoreData { .. } => {
            unreachable!("immutable data-recovery commands return before writable Hub state")
        }
    }
    catalogue_checkpoint.finish()?;
    Ok(())
}
