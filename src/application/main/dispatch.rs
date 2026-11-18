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
            tesatlas_hub::current_state::offline_drive_timeout_ms(
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
    let credentials = tesatlas_hub::auth::credential_sign_in::credential_status(store, &config.data_dir, now_ms)?;
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
    let credential_error = tesatlas_hub::auth::credential_sign_in::credential_store_unhealthy(&credentials,
        collector_required && config.collector.provider == CollectorProvider::Legacy,
        collector_required && config.collector.provider == CollectorProvider::Fleet);
    let readiness_reason = if credential_error { Some("credential_store_error") } else { view.service.as_ref().err().map(|failure| failure.code.code()) };

    let mut report = serde_json::json!({
        "status": "ok",
        "version": tesatlas_hub::BUILD_VERSION,
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
    config_sha256: tesatlas_hub::protocol::Sha256Digest,
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

use tesatlas_hub::runtime::operator::{
    self as operator,
    admission::{OperatorRefusal, Route},
    channel::{OperatorRequest, SetupMaterial},
};

/// Largest `setup` input: the sign-in code material as one JSON object.
const SETUP_INPUT_MAX_BYTES: usize = 4096;
/// How long a forwarded command waits for the service's answer.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(180);

fn operator_home() -> Result<PathBuf, OperatorRefusal> {
    #[cfg(test)]
    if let Some(home) = TEST_OPERATOR_HOME.with(|cell| cell.borrow().clone()) {
        return Ok(home);
    }
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
        let error = tesatlas_hub::auth::credential_sign_in::source_credential_disabled();
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
    // The one owner redirect the Mac app's sign-in window registers
    // (DEC-088): the exchange sends exactly this value, so no other is
    // forwarded to the service.
    if redirect != tesatlas_hub::auth::credential_sign_in::OWNER_REDIRECT_URI {
        return Err(setup_input_invalid("the redirect URI is not the owner sign-in redirect"));
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
                    tesatlas_hub::collection::window::GRANT_MAX_BYTES,
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

/// `take-over-source-token` (DEC-172): see the module.
#[cfg(unix)]
#[path = "source_takeover.rs"]
mod source_takeover;

/// Test doubles of the fresh-install journey (M02-T21, DEC-116): test builds
/// only, see the module.
#[cfg(all(unix, feature = "journey-doubles"))]
#[path = "journey_doubles.rs"]
mod journey_doubles;

/// The service's window authority. In a test build whose test named journey
/// doubles, it keeps its anchor in the test's folder and reads a fixed time
/// offset; every other build has only the production authority.
#[cfg(unix)]
fn start_window_authority(
    data_dir: &Path,
    home: &Path,
) -> Result<
    (
        std::sync::Arc<tesatlas_hub::collection::window::WindowController>,
        tesatlas_hub::collection::window::ResumeOutcome,
    ),
    Box<dyn std::error::Error>,
> {
    #[cfg(feature = "journey-doubles")]
    if let Some(doubles) = journey_doubles::active()? {
        return Ok(doubles.start_authority(data_dir, home)?);
    }
    Ok(operator::authority::start_service_authority(data_dir, home)?)
}

/// The service's trust authority (the issuer pin and the owner's
/// confirmation), with the same test-build exception as
/// [`start_window_authority`].
#[cfg(unix)]
fn window_trust_authority(
    host_id: &str,
) -> Result<tesatlas_hub::runtime::operator::trust::TrustAuthority, Box<dyn std::error::Error>> {
    #[cfg(feature = "journey-doubles")]
    if let Some(doubles) = journey_doubles::active()? {
        return Ok(doubles.trust_authority(host_id));
    }
    Ok(operator::authority::service_trust_authority(host_id))
}

/// What only the binary does inside the running service.
#[cfg(unix)]
struct HubServiceBackend {
    store: HubStore,
    data_dir: PathBuf,
    /// The served configuration: setup's car discovery reads the Owner API
    /// settings and the provider from it.
    config: HubConfig,
    admission: std::sync::Arc<AdmittedUserHub>,
    backup: std::sync::Arc<tesatlas_hub::runtime::backup_schedule::BackupScheduler>,
    /// The vault custody the window supervisor honours, and its wake-up.
    custody: tesatlas_hub::collection::window::CredentialCustody,
    kick: Box<dyn Fn() + Send + Sync>,
    /// Wakes the supervisor after a collection reload and clears its start
    /// backoff (M02-T24).
    reload_kick: Box<dyn Fn() + Send + Sync>,
    /// The configuration file the setup completes (M02-T21, DEC-116).
    config_path: PathBuf,
    /// Whether this service was admitted idle (never configured at its start).
    admitted_idle: bool,
    /// The collector slot the setup reloads with the completed configuration.
    collector: ServiceCollectorSlot,
}

/// The collection interval the fresh-install setup sets (M02-T21, DEC-116):
/// the `[collector] interval_seconds` value the configuration guide documents.
#[cfg(unix)]
const SUPPORTED_COLLECTION_INTERVAL_SECONDS: u64 = 300;

/// Sets `[collector] interval_seconds` line by line, by the same rule as the
/// Mac app's `settingCollectorInterval`, so every other line stays as written.
#[cfg(unix)]
fn setting_collector_interval(content: &str, seconds: u64) -> String {
    fn uncommented(line: &str) -> &str {
        line.split('#').next().unwrap_or_default().trim()
    }
    fn is_table_header(line: &str) -> bool {
        let value = uncommented(line);
        value.starts_with('[') && value.contains(']')
    }
    fn is_interval(line: &str) -> bool {
        uncommented(line)
            .split_once('=')
            .is_some_and(|(key, _)| key.trim() == "interval_seconds")
    }
    let mut lines: Vec<String> = content.split('\n').map(str::to_owned).collect();
    let setting = format!("interval_seconds = {seconds}");
    if let Some(table) = lines.iter().position(|line| uncommented(line) == "[collector]") {
        let end = lines[table + 1..]
            .iter()
            .position(|line| is_table_header(line))
            .map_or(lines.len(), |offset| table + 1 + offset);
        match lines[table + 1..end].iter().position(|line| is_interval(line)) {
            Some(offset) => {
                let line = &mut lines[table + 1 + offset];
                let indentation: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                *line = format!("{indentation}{setting}");
            }
            None => lines.insert(table + 1, setting),
        }
    } else {
        if lines.last().is_some_and(|line| !line.is_empty()) {
            lines.push(String::new());
        }
        lines.push("[collector]".to_owned());
        lines.push(setting);
    }
    if content.ends_with('\n') && lines.last().is_some_and(|line| !line.is_empty()) {
        lines.push(String::new());
    }
    lines.join("\n")
}

/// Writes the collection interval into the configuration file: the result is
/// parsed as a Hub configuration before it replaces the file, written 0600
/// through a synced temporary file beside it, and returned.
#[cfg(unix)]
fn write_collector_interval(
    config_path: &Path,
    seconds: u64,
) -> Result<HubConfig, Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let content = fs::read_to_string(config_path)?;
    let updated = setting_collector_interval(&content, seconds);
    let folder = config_path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staged = folder.join(format!(".config.{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| -> Result<HubConfig, Box<dyn std::error::Error>> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)?;
        file.write_all(updated.as_bytes())?;
        file.sync_all()?;
        let config = HubConfig::load(&staged)?;
        fs::rename(&staged, config_path)?;
        fs::File::open(folder)?.sync_all()?;
        Ok(config)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&staged);
    }
    written
}

#[cfg(unix)]
impl HubServiceBackend {
    fn recovery_key(
        path: &Path,
    ) -> Result<zeroize::Zeroizing<[u8; RECOVERY_ENCRYPTION_KEY_BYTES]>, OperatorRefusal> {
        read_recovery_encryption_key(path)
            .map_err(|error| OperatorRefusal::new("recovery_key_invalid", error.to_string()))
    }

    /// Completes a fresh install (M02-T21, DEC-116, finding 1): a service
    /// admitted idle whose sign-in configured a car sets the supported
    /// collection interval in its configuration file (only while it is 0) and
    /// reloads the collector slot with the written configuration before the
    /// setup answers. The collector then starts inside the Open window, and
    /// only there; after a stop or a reboot the full admission passes.
    fn complete_fresh_install(&self) -> Result<u64, OperatorRefusal> {
        let refusal = |message: String| OperatorRefusal::new("collection_interval_unavailable", message);
        let current = HubConfig::load(&self.config_path)
            .map_err(|error| refusal(format!("the configuration cannot be read: {error}")))?;
        if current.data_dir != self.data_dir {
            return Err(refusal("the configuration names another data folder".to_owned()));
        }
        #[allow(unused_mut)]
        let mut config = if current.collector.interval_seconds == 0 {
            write_collector_interval(&self.config_path, SUPPORTED_COLLECTION_INTERVAL_SECONDS)
                .map_err(|error| refusal(format!("the collection interval could not be written: {error}")))?
        } else {
            current
        };
        #[cfg(feature = "journey-doubles")]
        if let Some(doubles) = journey_doubles::active().ok().flatten() {
            doubles.apply(&mut config);
        }
        let seconds = config.collector.interval_seconds;
        tracing::info!(interval_seconds = seconds, "setup completed the fresh install: collection reloaded");
        self.collector.reload(config);
        (self.reload_kick)();
        Ok(seconds)
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
        use tesatlas_hub::auth::credential_sign_in as sign_in;
        // The car choice is checked first: a bad one refuses with nothing
        // sent and the code unspent.
        let selection = sign_in::owner_car_selection(vehicle_id, all_vehicles).map_err(setup_refusal)?;
        // DEC-088: the service exchanges the owner's code itself, with the
        // standard public owner client, at the owner token endpoint. The
        // window is Open here, and the exchange still asks the egress gate
        // immediately before its one send. Only that exchange's answer is
        // stored, with origin hub_login; any refusal leaves the store as it
        // was and names a fixed code, never the material. M02-T13: the same
        // material again, while that pair is still stored, resumes the
        // sign-in with no request to Tesla. Car discovery then runs inside
        // the same custody and window, with the vault's own pair.
        let report = owner_setup(
            &self.custody,
            &*self.kick,
            || open_command_credential_vault(
                self.store.clone(),
                std::sync::Arc::clone(&self.admission),
                &self.data_dir,
            ),
            |entry| run_owner_exchange(|| async {
                // A test build's journey doubles name a loopback token
                // endpoint; every other build exchanges at Tesla's.
                #[cfg(feature = "journey-doubles")]
                if let Some(endpoint) = journey_doubles::token_endpoint() {
                    return sign_in::sign_in_or_resume_owner_code_with_stub_for_test(
                        entry,
                        &material.code,
                        &material.code_verifier,
                        &material.redirect_uri,
                        endpoint,
                    )
                    .await;
                }
                sign_in::sign_in_or_resume_owner_code(
                    entry,
                    &material.code,
                    &material.code_verifier,
                    &material.redirect_uri,
                )
                .await
            }),
            |entry| run_owner_exchange(|| {
                sign_in::discover_owner_cars(entry, &self.store, &self.config, selection)
            }),
        );
        drop(material);
        let mut report = report?;
        // The Hub's own login is stored: this Hub is set up (finding 2).
        let installation = self.store.installation_id().map_err(|error| {
            OperatorRefusal::new("setup_marker_unavailable", error.to_string())
        })?;
        tesatlas_hub::storage::setup_marker::record(&self.data_dir, installation)
            .map_err(|error| OperatorRefusal::new("setup_marker_unavailable", error.to_string()))?;
        if self.admitted_idle && report["vehicleDiscovery"] == "configured" {
            report["collectionIntervalSeconds"] = self.complete_fresh_install()?.into();
        }
        Ok(report)
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
            let error = tesatlas_hub::auth::credential_sign_in::credential_entry_error(error);
            OperatorRefusal::new(error.code(), error.to_string())
        })?;
        let report = tesatlas_hub::credential_recovery::restore_credentials_from_entry(
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
            let error = tesatlas_hub::auth::credential_sign_in::credential_entry_error(error);
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

/// The owner sign-in inside the service (DEC-088, review finding 2): the
/// running collector holds the process's one credential vault, so the
/// sign-in first takes custody of it. The window supervisor stops the
/// collector and hands custody over once it has exited, so no refresh runs
/// beside the exchange; the sign-in then opens the vault, exchanges and
/// stores, lists and records the cars with that vault's own pair (M02-T13),
/// and closes the vault before custody ends. The supervisor then starts the
/// collector again, which opens the vault afresh and so loads the stored
/// pair and the configured cars.
///
/// A refused sign-in refuses `setup`. A discovery failure after the pair is
/// stored does not: the report says the sign-in is stored and discovery
/// failed with its fixed code, and the same `setup` again resumes it.
#[cfg(unix)]
fn owner_setup(
    custody: &tesatlas_hub::collection::window::CredentialCustody,
    kick: &dyn Fn(),
    open_entry: impl FnOnce() -> Result<tesatlas_hub::CredentialVaultEntry, tesatlas_hub::CredentialVaultEntryError>,
    exchange: impl FnOnce(&tesatlas_hub::CredentialVaultEntry)
        -> Result<tesatlas_hub::auth::credential_sign_in::OwnerSignIn, tesatlas_hub::auth::credential_sign_in::CredentialCommandError>,
    discover: impl FnOnce(&tesatlas_hub::CredentialVaultEntry)
        -> Result<tesatlas_hub::auth::credential_sign_in::OwnerDiscovery, tesatlas_hub::auth::credential_sign_in::CredentialCommandError>,
) -> Result<serde_json::Value, OperatorRefusal> {
    use tesatlas_hub::collection::window::custody::{CUSTODY_WAIT, SIGN_IN_BUSY};
    let held = custody.acquire(CUSTODY_WAIT, kick).map_err(|code| {
        let message = if code == SIGN_IN_BUSY {
            "another sign-in is in progress"
        } else {
            "the collector did not stop for the sign-in; try again"
        };
        OperatorRefusal::new(code, message)
    })?;
    let result = open_entry()
        .map_err(tesatlas_hub::auth::credential_sign_in::credential_entry_error)
        .and_then(|entry| {
            let result = exchange(&entry).map(|signed_in| (signed_in, discover(&entry)));
            // The vault is closed before custody ends.
            drop(entry);
            result
        });
    drop(held);
    kick();
    let (signed_in, discovery) = result.map_err(setup_refusal)?;
    Ok(owner_setup_report(signed_in, discovery))
}

/// The refusal `setup` returns: the fixed code and its fixed owner text.
#[cfg(unix)]
fn setup_refusal(
    error: tesatlas_hub::auth::credential_sign_in::CredentialCommandError,
) -> OperatorRefusal {
    OperatorRefusal::new(error.code(), error.to_string())
}

/// What a successful `setup` reports: no token, code, VIN, name or account
/// detail. The pair is stored either way; `signIn` says whether this request
/// exchanged a code or resumed a stored sign-in, `aftercare` whether the
/// cleanup behind the write completed or waits for the vault's next open,
/// and `vehicleDiscovery` whether the cars were configured (with their
/// count) or discovery failed (with its fixed code and owner text).
#[cfg(unix)]
fn owner_setup_report(
    signed_in: tesatlas_hub::auth::credential_sign_in::OwnerSignIn,
    discovery: Result<
        tesatlas_hub::auth::credential_sign_in::OwnerDiscovery,
        tesatlas_hub::auth::credential_sign_in::CredentialCommandError,
    >,
) -> serde_json::Value {
    let mut report = serde_json::json!({
        "status": "signed_in",
        "slot": "owner_api",
        "origin": "hub_login",
        "signIn": signed_in.sign_in(),
        "aftercare": signed_in.aftercare(),
    });
    match discovery {
        Ok(discovery) => {
            report["vehicleDiscovery"] = "configured".into();
            report["vehiclesConfigured"] = discovery.vehicles_configured.into();
        }
        Err(error) => {
            report["vehicleDiscovery"] = "failed".into();
            report["discoveryCode"] = error.code().into();
            report["discoveryMessage"] = tesatlas_hub::auth::credential_sign_in::credential_owner_message(error.code(), None, None, None)
                .unwrap_or_default()
                .into();
        }
    }
    report
}

/// Runs one sign-in step (the code exchange or the car discovery) to
/// completion on a thread and runtime of its own, so the channel's handler
/// thread never blocks inside another runtime.
#[cfg(unix)]
fn run_owner_exchange<'a, T, F>(
    exchange: impl FnOnce() -> F + Send + 'a,
) -> Result<T, tesatlas_hub::auth::credential_sign_in::CredentialCommandError>
where
    T: Send,
    F: std::future::Future<
            Output = Result<T, tesatlas_hub::auth::credential_sign_in::CredentialCommandError>,
        > + 'a,
{
    use tesatlas_hub::auth::credential_sign_in::sign_in_unavailable;
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
    -> Result<tesatlas_hub::CredentialVaultEntry, tesatlas_hub::CredentialVaultEntryError>;
#[cfg(all(test, unix))]
thread_local! {
    static COMMAND_ENTRY_OPENER: std::cell::Cell<Option<CommandEntryOpener>> = const { std::cell::Cell::new(None) };
}
#[cfg(unix)]
fn open_command_credential_vault(store: HubStore, admission: std::sync::Arc<AdmittedUserHub>, data: &Path)
    -> Result<tesatlas_hub::CredentialVaultEntry, tesatlas_hub::CredentialVaultEntryError>
{
    #[cfg(test)]
    if let Some(open) = COMMAND_ENTRY_OPENER.with(std::cell::Cell::get) { return open(store, admission, data); }
    #[cfg(feature = "credential-relocation-fixture")]
    { return tesatlas_hub::CredentialVaultEntry::open_for_test(store, admission, data, true); }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
    tesatlas_hub::open_credential_vault_for_entry(store, admission, data)
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "credential-relocation-fixture")]
    credential_relocation_fixture::require_dispatch(&cli)?;
    let config_path = cli.config.unwrap_or_else(default_config_path);
    #[cfg(unix)]
    tesatlas_hub::platform::name_migration::prepare_config_path(&config_path)?; // rename hand-over backstop (DEC-160)
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

    if let Command::LanTls { host } = &cli.command {
        #[cfg(unix)]
        {
            let report = setup_lan_tls(&config_path, host, SystemTime::now())?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            let _ = host;
            return Err("LAN TLS setup needs a Unix host".into());
        }
    }

    if let Command::ServingCheck { wait_seconds } = &cli.command {
        #[cfg(target_os = "macos")]
        {
            let wait = Duration::from_secs((*wait_seconds).min(SERVING_MAX_WAIT_SECONDS));
            let report = check_serving(&config_path, wait).await?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = wait_seconds;
            return Err("the serving check needs the macOS Hub service".into());
        }
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
            "History import requires --accept-schema-evidence after confirming source application release 4.4.0 or later"
        }.into());
    }

    #[cfg(unix)]
    if let Command::TakeOverSourceToken {
        accept_source_stopped: false,
        ..
    } = &cli.command
    {
        return Err("take-over-source-token requires --accept-source-stopped: the source application stays stopped once the Hub holds its login".into());
    }

    // Import and data restore are admitted only with the window Closed; the
    // admission is held until the command ends.
    let _import_admission = if route == Route::Import {
        admit_import_command(&cli.command, &config_path)?
    } else {
        None
    };

    // The source-token take-over (DEC-172) runs under the import admission
    // above and takes the data-directory lifetime lock itself, so it is
    // refused while the Hub service runs.
    #[cfg(unix)]
    if let Command::TakeOverSourceToken { source, .. } = &cli.command {
        let report = source_takeover::run_take_over(&config_path, source).await?;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    if let Command::ServePreflight { mode, history_only } = &cli.command {
        let config = HubConfig::load(&config_path)?;
        if *history_only {
            if *mode != DevelopmentServeModeArgument::Standalone {
                return Err("history-only Serve preflight requires standalone mode".into());
            }
            validate_history_only_config(&config)?;
        }
        tesatlas_hub::macos_launch_agent::preflight_hub_for_serve(
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
            ServiceCommand::Status => tesatlas_hub::linux_systemd::status()?,
            ServiceCommand::Start => tesatlas_hub::linux_systemd::apply(
                tesatlas_hub::linux_systemd::ServiceAction::Start,
            )?,
            ServiceCommand::Stop => tesatlas_hub::linux_systemd::apply(
                tesatlas_hub::linux_systemd::ServiceAction::Stop,
            )?,
            ServiceCommand::Restart => tesatlas_hub::linux_systemd::apply(
                tesatlas_hub::linux_systemd::ServiceAction::Restart,
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
    let admitted_user_hub = if operator::admission::requires_user_hub_admission(&cli.command)
        || matches!(&cli.command, Command::BackupPlaintextDirectory { .. })
    {
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
                    "next": "sudo systemctl start tesatlas-hub.service",
                })
            );
        }
        return Ok(());
    }

    // The recovery flow takes the encrypted bundle by default; an older
    // plaintext directory is refused before the password is read unless the
    // operator names that format (M06-T08).
    match &cli.command {
        Command::VerifyBackup {
            source,
            plaintext_directory,
        } => {
            let report = match admit_recovery_source(source, *plaintext_directory)? {
                BackupSourceFormat::PlaintextDirectory => verify_data_backup(source)?,
                BackupSourceFormat::Encrypted => {
                    let password = recovery_password(false)?;
                    tesatlas_hub::data_recovery::verify_encrypted_backup(source, &password)?
                }
            };
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        Command::RestoreData {
            source,
            destination,
            plaintext_directory,
        } => {
            let report = match admit_recovery_source(source, *plaintext_directory)? {
                BackupSourceFormat::PlaintextDirectory => restore_data_backup(source, destination)?,
                BackupSourceFormat::Encrypted => {
                    let password = recovery_password(false)?;
                    tesatlas_hub::data_recovery::restore_encrypted_backup(
                        source,
                        &password,
                        destination,
                    )?
                }
            };
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        Command::BackupPassword { action, account } => return run_backup_password(*action, account),
        _ => {}
    }

    match &cli.command {
        Command::Legal => {
            println!("{}", tesatlas_hub::legal_notice());
            return Ok(());
        }
        Command::Source => {
            let source = tesatlas_hub::corresponding_source_url()
                .ok_or(tesatlas_hub::UNBOUND_SOURCE_ERROR)?;
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
            tesatlas_hub::platform::r78_recording::with_fixture_backend(|backend| {
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
            // The Mac app's Start: the whole check, with its set-up marker
            // check and its reader, is the library's (M08-T25).
            if !*history_only {
                let answer = tesatlas_hub::macos_launch_agent::start_preflight(&config)?;
                println!("{answer}");
                return Ok(());
            }
            // A Hub that was set up and lost its catalogue is refused, never
            // idle and never new (M02-T21, DEC-116).
            tesatlas_hub::storage::setup_marker::check_catalogue(&config.data_dir)?;
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
    // M02-T21 (DEC-116, finding 2): a Hub that was set up keeps its
    // catalogue. A missing, emptied or replaced one is refused here, before
    // `hub.sqlite` is created or initialised, so it never comes up as new.
    tesatlas_hub::storage::setup_marker::check_catalogue(&config.data_dir)?;
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
                .map_err(tesatlas_hub::auth::credential_sign_in::credential_entry_error)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "bootstrapped",
                    "version": tesatlas_hub::BUILD_VERSION,
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
        | Command::LanTls { .. }
        | Command::ServingCheck { .. }
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
                let (summary_now_ms, admitted_idle) = {
                    store.checkpoint_catalogue_for_immutable_read()?;
                    let development_mode =
                        tesatlas_hub::macos_launch_agent::development_serve_mode(
                            tesatlas_hub::platform::name_migration::env::var_os(
                                tesatlas_hub::macos_launch_agent::DEVELOPMENT_SERVE_ENV,
                            )
                            .as_deref(),
                            tesatlas_hub::platform::name_migration::env::var_os(
                                tesatlas_hub::macos_launch_agent::DEVELOPMENT_SERVE_MODE_ENV,
                            )
                            .as_deref(),
                        )?;
                    // M02-T21: whether the production admission starts this
                    // Hub idle (never configured).
                    let admitted_idle = development_mode.is_none()
                        && tesatlas_hub::macos_launch_agent::hub_is_never_configured(&config)?;
                    tesatlas_hub::macos_launch_agent::preflight_hub_for_serve(
                        &config,
                        development_mode,
                    )?;
                    // A Hub the full production admission passed is set up:
                    // recorded outside its catalogue (DEC-116, finding 2).
                    if development_mode.is_none() && !admitted_idle {
                        tesatlas_hub::storage::setup_marker::record(
                            &config.data_dir,
                            store.installation_id()?,
                        )?;
                    }
                    // The Fixture mode serves the fixture's own measurement
                    // time (C6 R28 (c)); every other mode keeps the system clock.
                    (
                        tesatlas_hub::macos_launch_agent::fixture_summary_now_ms(
                            development_mode,
                            &config.data_dir,
                        )?,
                        admitted_idle,
                    )
                };
                #[cfg(not(target_os = "macos"))]
                let (summary_now_ms, admitted_idle): (Option<i64>, bool) = (None, false);
                // A test build's journey doubles put the stream double in the
                // served configuration (test builds only).
                #[cfg(feature = "journey-doubles")]
                let config = {
                    let mut config = config;
                    if let Some(doubles) = journey_doubles::active()? {
                        doubles.apply(&mut config);
                    }
                    config
                };
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
                // The runtime inventory counts every observation and pack
                // reference, so its cost grows with the store: it runs on a
                // blocking task once the operator channel is bound and the
                // listener is open, and can never delay serving (M08-T22).
                let log_inventory: server::ListeningHook = {
                    let store = store.clone();
                    let config = config.clone();
                    Box::new(move || {
                        drop(tokio::task::spawn_blocking(move || {
                            log_runtime_inventory(&store, &config);
                        }));
                    })
                };
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
                let (authority, resumed) =
                    start_window_authority(&config.data_dir, &operator_home()?)?;
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
                let collector_slot = ServiceCollectorSlot::new(
                    tokio::runtime::Handle::current(),
                    store.clone(),
                    config.clone(),
                    std::sync::Arc::clone(&admission),
                    std::sync::Arc::clone(&recovery),
                );
                let supervisor = operator::authority::SupervisorThread::spawn(
                    std::sync::Arc::clone(&authority),
                    collector_slot.clone(),
                )?;
                let channel = operator::channel::ChannelServer::bind(
                    &config.data_dir,
                    std::sync::Arc::new(operator::authority::OperatorService::new(
                        std::sync::Arc::clone(&authority),
                        std::sync::Arc::new(HubServiceBackend {
                            store: store.clone(),
                            data_dir: config.data_dir.clone(),
                            config: config.clone(),
                            admission: std::sync::Arc::clone(&admission),
                            backup: std::sync::Arc::clone(&backup_schedule),
                            custody: tesatlas_hub::collection::window::CredentialCustody::process_wide().clone(),
                            kick: supervisor.kicker(),
                            reload_kick: supervisor.reload_kicker(),
                            config_path: config_path.clone(),
                            admitted_idle,
                            collector: collector_slot,
                        }),
                        supervisor.kicker(),
                        window_trust_authority(authority.host_id())?,
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
                                Some(log_inventory),
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
            if window.phase != tesatlas_hub::collection::window::Phase::Open {
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
                Duration::from_secs(tesatlas_hub::collection::window::SUPERVISOR_POLL_SECONDS as u64),
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
        Command::Migrate { .. } | Command::TakeOverSourceToken { .. } => {
            unreachable!("migration and the source-token take-over return before opening common Hub state")
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
        // Encrypted by default (M06-T08): one authenticated bundle in the
        // destination folder, keeping every earlier generation.
        Command::Backup { destination } => {
            let password = recovery_password(true)?;
            let mut request =
                tesatlas_hub::data_recovery::EncryptedBackupRequest::new(&destination, &password);
            // The command's existing private folder: owner-only, by mode and
            // by ACL, where the volume enforces ownership (M06-T14).
            request.private_destination = true;
            let report = tesatlas_hub::data_recovery::create_encrypted_backup(&store, &request)
                .map_err(|error| backup_refusal(&store, error))?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::BackupPlaintextDirectory { destination } => {
            let report = create_data_backup(&store, &destination)
                .map_err(|error| backup_refusal(&store, error))?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::ExportRecoveryCredentials { .. }
        | Command::RestoreRecoveryCredentials { .. }
        | Command::PurgeRefusedCredentials { .. }
        | Command::BackupNow { .. }
        | Command::BackupStatus => {
            unreachable!("credential recovery, the purge and the backup verbs are forwarded to the running service")
        }
        Command::VerifyBackup { .. } | Command::RestoreData { .. } | Command::BackupPassword { .. } => {
            unreachable!("immutable data-recovery commands return before writable Hub state")
        }
    }
    catalogue_checkpoint.finish()?;
    Ok(())
}

/// A refusal caused by refused pre-upgrade credentials names the command that
/// removes them (DEC-083).
fn backup_refusal(
    store: &HubStore,
    error: tesatlas_hub::data_recovery::DataRecoveryError,
) -> Box<dyn std::error::Error> {
    match tesatlas_hub::data_recovery::refused_credential_backup_message(store, &error) {
        Some(message) => message.into(),
        None => error.into(),
    }
}

use tesatlas_hub::data_recovery::{BackupSourceFormat, admit_recovery_source};

/// The recovery password for `backup`, `verify-backup` and `restore-data`:
/// a no-echo prompt on a terminal (twice for a new backup), otherwise one line
/// of standard input. Never an argument or an environment variable, never
/// logged. Tests offer standard input through [`set_password_input`].
fn recovery_password(
    confirm: bool,
) -> Result<zeroize::Zeroizing<String>, tesatlas_hub::data_recovery::DataRecoveryError> {
    #[cfg(test)]
    {
        let _ = confirm;
        let input = zeroize::Zeroizing::new(take_password_input().unwrap_or_default());
        tesatlas_hub::data_recovery::recovery_password_from(input.as_slice())
    }
    #[cfg(not(test))]
    {
        tesatlas_hub::data_recovery::read_recovery_password(confirm)
    }
}

#[cfg(test)]
thread_local! {
    static TEST_PASSWORD_INPUT: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
    static TEST_OPERATOR_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: the bytes the next recovery-password read takes as standard input.
#[cfg(test)]
fn set_password_input(input: Option<Vec<u8>>) {
    TEST_PASSWORD_INPUT.with(|cell| *cell.borrow_mut() = input);
}

/// Test seam: takes what [`set_password_input`] left unread.
#[cfg(test)]
fn take_password_input() -> Option<Vec<u8>> {
    TEST_PASSWORD_INPUT.with(|cell| cell.borrow_mut().take())
}

/// Test seam: the operator home for import admission, inside the test's root.
#[cfg(test)]
fn set_operator_home(home: Option<PathBuf>) {
    TEST_OPERATOR_HOME.with(|cell| *cell.borrow_mut() = home);
}

#[cfg(all(test, unix))]
mod backup_command_tests {
    include!("backup_command_tests.rs");
}

// M06-T14: REV-CX-M06-T08 findings 1, 2 and 4 through the shipped commands.
// Each test drives the real dispatcher (`run`) on a small synthetic Hub in its
// own temporary root, with standard input offered through the test seam and
// parsed exactly as standard input is. Synthetic passwords only.
#[cfg(all(test, unix))]
mod backup_recovery_repair_tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::{Arc, atomic::AtomicBool},
    };

    use tesatlas_hub::{
        db::HubStore,
        runtime::backup_schedule::{BackupJob, BackupRunner, StoreBackupRunner},
    };

    use crate::{Cli, Command, run, set_operator_home, set_password_input};

    const PASSWORD: &str = "correct horse battery staple";
    /// A password file the schedule accepts today that one line cannot
    /// carry: LF, CR and NUL inside, and a trailing line end it keeps.
    const SCHEDULED_PASSWORD_FILE: &[u8] = b"line one\nline two\rcarriage\0nul end\n\n";
    /// The first line that selects the password-file mode.
    const FILE_HEADER: &[u8] = b"#pw-file\n";

    fn private_directory(path: &Path) {
        fs::create_dir_all(path).expect("directory");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("private directory");
    }

    fn line(password: &str) -> Vec<u8> {
        format!("{password}\n").into_bytes()
    }

    struct Fixture {
        _temporary: tempfile::TempDir,
        root: PathBuf,
        data: PathBuf,
        config: PathBuf,
        installation_id: uuid::Uuid,
    }

    /// A synthetic Hub under `base` (the configured temporary folder when
    /// `None`).
    fn fixture_in(base: Option<&Path>) -> Fixture {
        let temporary = match base {
            Some(base) => tempfile::Builder::new().prefix("m06-t14-").tempdir_in(base),
            None => tempfile::tempdir(),
        }
        .expect("temporary root");
        let root = temporary.path().canonicalize().expect("canonical root");
        private_directory(&root);
        let data = root.join("data");
        private_directory(&data);
        let installation_id = HubStore::initialize(&data)
            .expect("store")
            .installation_id()
            .expect("installation");
        let config = root.join("config.toml");
        fs::write(&config, format!("data_dir = {data:?}\nbind = '127.0.0.1:18443'\n")).expect("config");
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).expect("private config");
        private_directory(&root.join("home"));
        Fixture {
            _temporary: temporary,
            root,
            data,
            config,
            installation_id,
        }
    }

    impl Fixture {
        /// Runs one command with `input` offered as standard input.
        async fn run(&self, command: Command, input: Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
            set_password_input(Some(input));
            set_operator_home(Some(self.root.join("home")));
            let outcome = run(Cli {
                config: Some(self.config.clone()),
                command,
            })
            .await;
            set_operator_home(None);
            set_password_input(None);
            outcome
        }

        fn bundles(&self, destination: &Path) -> usize {
            fs::read_dir(destination)
                .expect("destination listing")
                .filter(|entry| {
                    entry
                        .as_ref()
                        .is_ok_and(|entry| entry.path().extension().is_some_and(|extension| extension == "tlbk"))
                })
                .count()
        }
    }

    fn verify(source: &Path) -> Command {
        Command::VerifyBackup {
            source: source.to_path_buf(),
            plaintext_directory: false,
        }
    }

    /// Finding 1: a bundle the scheduled runner wrote with a password holding
    /// LF, CR and NUL verifies and restores through the shipped commands in
    /// the password-file mode; the line mode refuses that input, it does not
    /// guess at it.
    #[tokio::test]
    async fn backup_f1_scheduled_bundle_with_line_ends_verifies_and_restores() {
        let hub = fixture_in(None);
        let destination = hub.root.join("backups");
        private_directory(&destination);
        let password_file = hub.root.join("backup-password");
        fs::write(&password_file, SCHEDULED_PASSWORD_FILE).expect("password file");
        fs::set_permissions(&password_file, fs::Permissions::from_mode(0o600)).expect("mode 0600");
        let report = StoreBackupRunner::new(HubStore::initialize(&hub.data).expect("store"))
            .run(&BackupJob {
                destination: destination.clone(),
                password: tesatlas_hub::runtime::backup_schedule::PasswordSource::File(password_file),
                keep: None,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .expect("the scheduled runner writes a bundle");
        let bundle = PathBuf::from(report["path"].as_str().expect("bundle path"));
        let mut input = FILE_HEADER.to_vec();
        input.extend_from_slice(SCHEDULED_PASSWORD_FILE);

        hub.run(verify(&bundle), input.clone())
            .await
            .expect("verify-backup takes the schedule's password");
        let restored = hub.root.join("restored");
        hub.run(
            Command::RestoreData {
                source: bundle.clone(),
                destination: restored.clone(),
                plaintext_directory: false,
            },
            input,
        )
        .await
        .expect("restore-data takes the schedule's password");
        let store = HubStore::initialize(&restored).expect("restored Hub opens");
        assert_eq!(store.installation_id().expect("installation"), hub.installation_id);

        let error = hub
            .run(verify(&bundle), SCHEDULED_PASSWORD_FILE.to_vec())
            .await
            .expect_err("the line mode refuses input after the first line");
        assert!(!error.to_string().contains("line one"), "the password is never echoed");
    }

    /// Finding 2: `backup` refuses a destination that is a symbolic link to a
    /// private folder, and writes nothing behind it.
    #[tokio::test]
    async fn backup_f2_command_refuses_a_destination_link() {
        let hub = fixture_in(None);
        let target = hub.root.join("private-target");
        private_directory(&target);
        let link = hub.root.join("backups-link");
        std::os::unix::fs::symlink(&target, &link).expect("link");
        let error = hub
            .run(Command::Backup { destination: link }, line(PASSWORD))
            .await
            .expect_err("a destination link is refused");
        assert!(error.to_string().contains("symbolic link"), "{error}");
        assert_eq!(hub.bundles(&target), 0, "nothing written behind the link");
        hub.run(Command::Backup { destination: target.clone() }, line(PASSWORD))
            .await
            .expect("the folder itself");
        assert_eq!(hub.bundles(&target), 1);
    }

    /// Finding 4: on a volume that enforces ownership, `backup` refuses a
    /// destination that other accounts can read, list or search, by mode or
    /// by an ACL, and admits an owner-only folder.
    #[tokio::test]
    async fn backup_f4_command_destination_must_be_owner_only() {
        let Some(base) = ownership_enforcing_base() else {
            eprintln!("skipped: no temporary folder on a volume that enforces ownership");
            return;
        };
        let hub = fixture_in(Some(&base));
        for mode in [0o755, 0o750, 0o711] {
            let destination = hub.root.join(format!("mode-{mode:o}"));
            fs::create_dir(&destination).expect("folder");
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode)).expect("mode");
            let error = hub
                .run(Command::Backup { destination: destination.clone() }, line(PASSWORD))
                .await
                .expect_err("foreign access is refused");
            assert!(error.to_string().contains("private folder"), "{mode:o}: {error}");
            assert_eq!(hub.bundles(&destination), 0, "{mode:o}: nothing written");
        }
        #[cfg(target_os = "macos")]
        {
            let listed = hub.root.join("acl-list");
            private_directory(&listed);
            let status = std::process::Command::new("/bin/chmod")
                .args(["+a", "group:everyone allow list,search"])
                .arg(&listed)
                .status()
                .expect("chmod");
            assert!(status.success(), "ACL set");
            let error = hub
                .run(Command::Backup { destination: listed.clone() }, line(PASSWORD))
                .await
                .expect_err("an ACL grant is refused");
            assert!(error.to_string().contains("private folder"), "{error}");
            assert_eq!(hub.bundles(&listed), 0, "nothing written");
        }
        let private = hub.root.join("private");
        private_directory(&private);
        hub.run(Command::Backup { destination: private.clone() }, line(PASSWORD))
            .await
            .expect("an owner-only folder");
        assert_eq!(hub.bundles(&private), 1);
    }

    /// A temporary base on a local volume that enforces ownership: the
    /// configured temporary folder may be on one that ignores it, where the
    /// POSIX checks rightly do not apply.
    fn ownership_enforcing_base() -> Option<PathBuf> {
        let mut candidates = vec![std::env::temp_dir()];
        #[cfg(target_os = "macos")]
        if let Ok(output) = std::process::Command::new("/usr/bin/getconf")
            .arg("DARWIN_USER_TEMP_DIR")
            .output()
            && output.status.success()
        {
            candidates.push(PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()));
        }
        candidates.into_iter().find(|candidate| enforces_ownership(candidate))
    }

    #[cfg(target_os = "macos")]
    fn enforces_ownership(path: &Path) -> bool {
        // <sys/mount.h> flags as statfs(2) reports them.
        const MNT_LOCAL: u32 = 0x0000_1000;
        const MNT_IGNORE_OWNERSHIP: u32 = 0x0020_0000;
        rustix::fs::statfs(path)
            .is_ok_and(|stats| stats.f_flags & MNT_LOCAL != 0 && stats.f_flags & MNT_IGNORE_OWNERSHIP == 0)
    }

    #[cfg(not(target_os = "macos"))]
    fn enforces_ownership(path: &Path) -> bool {
        path.is_dir()
    }
}
