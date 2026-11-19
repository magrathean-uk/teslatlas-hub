// SPDX-License-Identifier: AGPL-3.0-only

#[tokio::main]
async fn main() -> ExitCode {
    #[cfg(feature = "credential-relocation-fixture")]
    { return credential_relocation_fixture::entry().await; }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
    #[cfg(unix)]
    if let Some(code) = tesatlas_hub::platform::name_migration::run_if_requested() { return code; }
    let cli = Cli::parse();
    // The Fleet figures in effect (M09-T21, DEC-225) are answered first, before
    // the development journal is initialised, a command event is recorded or
    // logging is set up: the verb writes nothing, answers even when the journal
    // folder is unusable, and its streams carry only the contract's lines (a
    // program reads them). Not forwarded: no store, channel, input or rename
    // hand-over.
    #[cfg(unix)]
    if let Command::SetupFleet { step: Some(operator::cli::FleetSetupStep::Figures), .. } = &cli.command {
        let config_path = cli.config.clone().unwrap_or_else(default_config_path);
        return ExitCode::from(tesatlas_hub::runtime::fleet_figures::answer_fleet_figures(
            &config_path,
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
        ));
    }
    use tesatlas_hub::runtime::development_event_log as dev;
    if dev::initialize().is_err() {
        eprintln!("tesatlas-hub: development journal unavailable");
        return ExitCode::FAILURE;
    }
    let logging = tracing_subscriber::fmt()
        .with_env_filter(if dev::enabled() {
            EnvFilter::new("off")
        } else {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        })
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(false);
    if matches!(&cli.command,
        Command::Setup { .. } | Command::SetupFleet { .. } | Command::Migrate { history_only: false, .. }
        | Command::TakeOverSourceToken { .. }
        | Command::Control { command: ControlCommand::SignOut, .. }
        | Command::Control { command: ControlCommand::RotateCredentialKey, .. }) {
        logging.with_writer(std::io::stderr).init();
    } else { logging.init(); }
    #[cfg(target_os = "macos")]
    if matches!(&cli.command, Command::Serve) {
        tesatlas_hub::macos_launch_agent::compact_inherited_logs();
    }
    let kind = match &cli.command {
        Command::Migrate { .. } => dev::Kind::Import,
        Command::Serve => dev::Kind::Serve,
        _ => dev::Kind::Command,
    };
    let started = Instant::now();
    dev::record(dev::Event::new(kind, dev::Outcome::Start));
    let result = run(cli).await;
    let mut event = dev::Event::new(
        kind,
        if result.is_ok() {
            dev::Outcome::Complete
        } else {
            dev::Outcome::Failed
        },
    );
    event.duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    dev::record(event);
    if !dev::flush() {
        eprintln!("tesatlas-hub: development journal incomplete");
    }
    ExitCode::from(report_outcome(result, &mut std::io::stderr()))
    }
}

/// The exit status of a finished command (0, or 1 for an error) after the error
/// and each of its causes are written to `stderr` (C8 R40).
fn report_outcome(
    result: Result<(), Box<dyn std::error::Error>>,
    stderr: &mut impl std::io::Write,
) -> u8 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(stderr, "tesatlas-hub: {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                let _ = writeln!(stderr, "caused by: {cause}");
                source = cause.source();
            }
            1
        }
    }
}

fn control_target(
    store: &HubStore,
    requested_vehicle_id: Option<uuid::Uuid>,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let vehicles = store.published_vehicles()?;
    let vehicle_id = match requested_vehicle_id {
        Some(vehicle_id)
            if vehicles
                .iter()
                .any(|vehicle| vehicle.vehicle_id == vehicle_id) =>
        {
            vehicle_id
        }
        Some(_) => return Err("--vehicle-id does not identify a published car".into()),
        None if vehicles.len() == 1 => vehicles[0].vehicle_id,
        None => return Err("control command requires --vehicle-id with multiple cars".into()),
    };
    store.v2_projection_binding(vehicle_id)?;
    Ok(vehicle_id)
}

/// The `control paired-devices` document: every device with the vehicles it is
/// granted. It never contains bearer material.
fn paired_devices_json(store: &HubStore) -> Result<String, Box<dyn std::error::Error>> {
    Ok(serde_json::to_string(
        &store.list_paired_devices_with_vehicles()?,
    )?)
}

/// The vehicle action `command` asks for (wake, climate, charge, lock and the
/// rest) and whether it carries `--confirm`; `None` for every other control.
/// Only Wake, Flash Lights and Honk Horn are ever sent (DEC-248); the others
/// parse so that they meet a fixed refusal.
fn vehicle_action(
    command: &ControlCommand,
) -> Option<(tesatlas_hub::owner_api::LegacyVehicleAction, bool)> {
    use tesatlas_hub::owner_api::LegacyVehicleAction as Action;
    Some(match command {
        ControlCommand::Wake { confirm } => (Action::Wake, *confirm),
        ControlCommand::ClimateStart { confirm } => (Action::ClimateStart, *confirm),
        ControlCommand::ClimateStop { confirm } => (Action::ClimateStop, *confirm),
        ControlCommand::ChargeStart { confirm } => (Action::ChargeStart, *confirm),
        ControlCommand::ChargeStop { confirm } => (Action::ChargeStop, *confirm),
        ControlCommand::SetChargeLimit { percent, confirm } => {
            (Action::SetChargeLimit(*percent), *confirm)
        }
        ControlCommand::Lock { confirm } => (Action::Lock, *confirm),
        ControlCommand::Unlock { confirm } => (Action::Unlock, *confirm),
        ControlCommand::FlashLights { confirm } => (Action::FlashLights, *confirm),
        ControlCommand::HonkHorn { confirm } => (Action::HonkHorn, *confirm),
        _ => return None,
    })
}

/// The contracted refusals of a vehicle action (DEC-246): `code: text`, exit 1,
/// nothing on standard output and nothing sent.
const VEHICLE_COMMANDS_NOT_OFFERED_CODE: &str = "vehicle_commands_not_offered";
const VEHICLE_ACTION_UNCONFIRMED_CODE: &str = "vehicle_action_unconfirmed";
const VEHICLE_ACTION_UNCONFIRMED: &str =
    "a vehicle action is sent only with --confirm; nothing was sent";
const COLLECTION_WINDOW_CLOSED_CODE: &str = "collection_window_closed";
const VEHICLE_ACTION_UNKNOWN_CODE: &str = "vehicle_action_unknown";
const VEHICLE_ACTION_NOT_PERMITTED_CODE: &str = "vehicle_action_not_permitted";

/// The line a vehicle action ends with when the running service does not send
/// it. The window, the Fleet route, an action outside the permitted set and an
/// unknown action carry their contracted codes; a refused send or one whose
/// outcome is unknown keeps the service's own text.
fn vehicle_action_refusal_line(error: &collector::ResidentVehicleActionError) -> String {
    use collector::ResidentVehicleActionError as Refusal;
    match error {
        Refusal::CommandsNotOffered => format!("{VEHICLE_COMMANDS_NOT_OFFERED_CODE}: {error}"),
        Refusal::WindowClosed | Refusal::Unavailable => format!(
            "{COLLECTION_WINDOW_CLOSED_CODE}: {}",
            collector::VEHICLE_ACTION_WINDOW_CLOSED
        ),
        Refusal::UnknownAction => format!("{VEHICLE_ACTION_UNKNOWN_CODE}: {error}"),
        Refusal::NotPermitted => format!("{VEHICLE_ACTION_NOT_PERMITTED_CODE}: {error}"),
        _ => error.to_string(),
    }
}

/// `control <action> --confirm` (DEC-246, DEC-248). The Mac app never calls it.
/// A Fleet Hub refuses every action (polling only), with or without `--confirm`,
/// before the catalogue is opened. A Legacy Hub refuses every action other than
/// Wake, Flash Lights and Honk Horn, then one without `--confirm`,
/// then one with no running service collecting inside an Open window, all
/// before the catalogue is opened; otherwise the selected car's action goes to
/// the running service, which checks the window again, writes the outbound
/// audit receipt as an explicit vehicle command and sends it once through the
/// Legacy Owner route. Nothing here contacts Tesla itself.
async fn run_vehicle_action(
    config: &HubConfig,
    requested_vehicle_id: Option<uuid::Uuid>,
    action: tesatlas_hub::owner_api::LegacyVehicleAction,
    confirmed: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if config.collector.provider == CollectorProvider::Fleet {
        return Err(format!(
            "{VEHICLE_COMMANDS_NOT_OFFERED_CODE}: {}",
            collector::VEHICLE_COMMANDS_NOT_OFFERED
        )
        .into());
    }
    if !action.is_permitted() {
        return Err(format!(
            "{VEHICLE_ACTION_NOT_PERMITTED_CODE}: {}",
            collector::VEHICLE_ACTION_NOT_PERMITTED
        )
        .into());
    }
    if !confirmed {
        return Err(format!("{VEHICLE_ACTION_UNCONFIRMED_CODE}: {VEHICLE_ACTION_UNCONFIRMED}").into());
    }
    if !collector::resident_vehicle_control_present(&config.data_dir) {
        return Err(format!(
            "{COLLECTION_WINDOW_CLOSED_CODE}: {}",
            collector::VEHICLE_ACTION_WINDOW_CLOSED
        )
        .into());
    }
    let store = HubStore::open_existing(&config.data_dir)?;
    let vehicle_id = control_target(&store, requested_vehicle_id)?;
    drop(store);
    let report = collector::request_resident_vehicle_action(&config.data_dir, vehicle_id, action)
        .await
        .map_err(|error| vehicle_action_refusal_line(&error))?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn validate_streaming_setting(
    provider: CollectorProvider,
    streaming: Option<bool>,
) -> Result<(), &'static str> {
    if provider == CollectorProvider::Fleet && streaming == Some(true) {
        return Err("Fleet provider does not support legacy streaming");
    }
    Ok(())
}

fn clear_provider_credentials(
    entry: &tesatlas_hub::CredentialVaultEntry,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = tesatlas_hub::auth::credential_sign_in::sign_out(entry);
    if result.scrub_pending { tracing::warn!(operation = "post_clear_scrub", outcome = "pending", "credential scrub"); }
    let mut failures = Vec::new();
    for (slot, code) in [("owner_api", result.owner_api), ("fleet_api", result.fleet_api)] {
        if let Some(code) = code {
            let text = tesatlas_hub::auth::credential_sign_in::credential_owner_message(code, None, None, None).unwrap_or_default();
            failures.push(format!("{slot}: {code}: {text}"));
        }
    }
    if result.rows_remain { failures.push("credential_rows_remain".to_owned()); }
    if failures.is_empty() { Ok(()) } else { Err(std::io::Error::other(failures.join("; ")).into()) }
}

#[cfg(unix)]
const MIGRATION_OUTCOME_AMBIGUOUS: &str = "TESATLAS_MIGRATION_OUTCOME_AMBIGUOUS";

#[cfg(unix)]
fn migration_outcome_ambiguous(
    action: &str,
    error: impl std::fmt::Display,
) -> Box<dyn std::error::Error> {
    std::io::Error::other(format!(
        "{MIGRATION_OUTCOME_AMBIGUOUS}: {action}: {error}; keep the migration handover gate and Hub stopped"
    ))
    .into()
}

/// What `control --vehicle-id X pair` says: that flag picks the car a vehicle
/// command acts on, and an invitation is scoped by `pair --vehicle` instead.
const CONTROL_PAIR_VEHICLE_ID_USAGE: &str = "--vehicle-id does not apply to `control pair`: it \
    selects the car of a vehicle command. Scope the invitation with `control pair --vehicle <UUID>` \
    (repeatable) or `--all-vehicles`";

/// The one deprecation line of an old `--mode` value (C2 R53a).
fn deprecated_charge_cost_mode_line(alias: &str, mode: ChargeCostMode) -> String {
    format!(
        "tesatlas-hub: --mode {alias} is deprecated and will stop working in the next \
         release; use --mode {}",
        mode.as_str()
    )
}

/// The store call each mode makes (C2 R53): `total` sets the total; the two
/// rate modes set a unit price and let the Hub work the total out.
fn set_charge_cost_by_mode(
    store: &HubStore,
    vehicle_id: uuid::Uuid,
    charge_id: i64,
    value: f64,
    mode: ChargeCostMode,
) -> Result<tesatlas_hub::hub_pack::ProjectionCharge, StoreError> {
    match mode {
        ChargeCostMode::Total => store.set_charge_cost(vehicle_id, charge_id, value),
        ChargeCostMode::PerKwh => {
            store.set_charge_cost_rate(vehicle_id, charge_id, value, GeofenceBillingType::PerKwh)
        }
        ChargeCostMode::PerMinute => {
            store.set_charge_cost_rate(vehicle_id, charge_id, value, GeofenceBillingType::PerMinute)
        }
    }
}

/// `control set-charge-cost` after the vehicle is chosen: the deprecation line
/// of an old mode value goes to `diagnostics` first (once, before the store
/// call), then the store call, then the document the command prints (C2 R54):
/// `{"status": "updated", "vehicleId": ..., "charge": <row>}`.
fn run_set_charge_cost(
    store: &HubStore,
    vehicle_id: uuid::Uuid,
    charge_id: i64,
    value: f64,
    choice: &ChargeCostModeChoice,
    diagnostics: &mut dyn Write,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if let Some(alias) = &choice.deprecated_alias {
        writeln!(
            diagnostics,
            "{}",
            deprecated_charge_cost_mode_line(alias, choice.mode)
        )?;
    }
    let charge = set_charge_cost_by_mode(store, vehicle_id, charge_id, value, choice.mode)?;
    Ok(serde_json::json!({
        "status": "updated",
        "vehicleId": vehicle_id,
        "charge": charge,
    }))
}

/// A billing type outside the stored vocabulary. The option's value parser
/// already refuses one, so this only guards a directly built command; it names
/// no value and no vocabulary (C8 R35).
#[derive(Debug)]
struct UnknownBillingType;

impl std::fmt::Display for UnknownBillingType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("unsupported billing type")
    }
}

impl std::error::Error for UnknownBillingType {}

/// `control set-geofence` after the vehicle is chosen (C8 R37): save the fence,
/// with every omitted option left omitted so the store applies its own rule
/// (C8 table 5.2) and `--clear-tariff` removes the stored tariff first; then,
/// only when `--recalculate-missing-costs` is set, fill the unknown costs of
/// the Hub's own charges that started inside it (C2 R49, R50). The document
/// carries the number of rows priced as `recalculatedChargeCosts` (0 without
/// the flag).
fn run_set_geofence(
    store: &HubStore,
    vehicle_id: uuid::Uuid,
    command: &ControlCommand,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let ControlCommand::SetGeofence {
        id,
        name,
        latitude,
        longitude,
        radius_m,
        billing_type,
        cost_per_unit,
        session_fee,
        clear_tariff,
        recalculate_missing_costs,
    } = command
    else {
        return Err("not a set-geofence command".into());
    };
    // C8 R35, C8 R37: an omitted billing type stays omitted, so the store keeps
    // the stored one (edit) or takes its own default (create).
    let billing_type = billing_type
        .as_deref()
        .map(str::parse::<GeofenceBillingType>)
        .transpose()
        .map_err(|()| UnknownBillingType)?;
    let geofence = store.save_geofence(
        vehicle_id,
        *id,
        tesatlas_hub::source_db_projection::SourceDbGeofence {
            id: id.unwrap_or_default(),
            name: name.clone(),
            latitude: Some(*latitude),
            longitude: Some(*longitude),
            radius_m: *radius_m,
            billing_type,
            cost_per_unit: *cost_per_unit,
            session_fee: *session_fee,
        },
        *clear_tariff,
    )?;
    let recalculated_charge_costs = if *recalculate_missing_costs {
        store.recalculate_missing_charge_costs(vehicle_id, geofence.id)?
    } else {
        0
    };
    Ok(serde_json::json!({
        "status": "updated",
        "vehicleId": vehicle_id,
        "geofence": geofence,
        "recalculatedChargeCosts": recalculated_charge_costs,
    }))
}

/// The control CLI's collector actions (C3 R65, R66, section 8 item 5). `retry` and
/// `resume` insert a row the running collector reads at its next settings refresh
/// and applies in one tick transaction; `gaps acknowledge` marks the alert gaps as
/// notified, which on Linux and Docker is how the owner answers the readiness detail
/// `collector_gap_unnotified`. Nothing here contacts Tesla, and the printed document
/// holds only the action, a count and Hub ids.
fn run_collector_command(
    config_path: &Path,
    command: &CollectorCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = HubConfig::load(config_path)?;
    let store = HubStore::open_existing(&config.data_dir)?;
    let now_ms = current_epoch_ms()?;
    let document = match command {
        CollectorCommand::Retry => {
            let action_id =
                store.insert_collector_owner_action("retry_collection", None, now_ms)?;
            serde_json::json!({"status": "requested", "action": "retry_collection", "actionId": action_id})
        }
        CollectorCommand::Resume { vehicle } => {
            if !store
                .configured_tesla_vehicles()?
                .iter()
                .any(|(hub_id, _, _)| hub_id == vehicle)
            {
                return Err("the vehicle UUID does not identify a configured car".into());
            }
            let action_id =
                store.insert_collector_owner_action("resume_vehicle", Some(*vehicle), now_ms)?;
            serde_json::json!({
                "status": "requested",
                "action": "resume_vehicle",
                "vehicleId": vehicle,
                "actionId": action_id,
            })
        }
        CollectorCommand::Gaps {
            command: CollectorGapsCommand::Acknowledge,
        } => {
            let acknowledged = store.acknowledge_collection_gaps(now_ms)?;
            serde_json::json!({"status": "acknowledged", "gaps": acknowledged})
        }
    };
    println!("{document}");
    Ok(())
}

fn run_credential_control_with(
    config: &HubConfig,
    command: &ControlCommand,
    stop: impl FnOnce() -> Result<(), Box<dyn std::error::Error>>,
    open: impl FnOnce(HubStore, std::sync::Arc<AdmittedUserHub>, &Path) -> Result<tesatlas_hub::CredentialVaultEntry, tesatlas_hub::CredentialVaultEntryError>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    stop()?;
    let admission = AdmittedUserHub::admit(&config.data_dir)?;
    // The existing-store lane is deliberately read-only and carries no
    // credential connection. Validate it, then obtain the admitted write lane.
    drop(HubStore::open_existing(&config.data_dir)?);
    let store = HubStore::initialize(&config.data_dir)?;
    let entry = open(store.clone(), admission, &config.data_dir)
        .map_err(tesatlas_hub::auth::credential_sign_in::credential_entry_error)?;
    match command {
        ControlCommand::SignOut => {
            let mut checkpoint = CatalogueCheckpointGuard::new(store);
            clear_provider_credentials(&entry)?;
            checkpoint.finish()?;
            Ok(serde_json::json!({"status":"signed_out","service":"stopped"}))
        }
        ControlCommand::RotateCredentialKey => Ok(serde_json::to_value(tesatlas_hub::auth::credential_sign_in::rotate_credential_key(&entry)?)?),
        _ => unreachable!("credential controls are selected before this boundary"),
    }
}

fn open_control_credential_vault(store: HubStore, admission: std::sync::Arc<AdmittedUserHub>, data: &Path) -> Result<tesatlas_hub::CredentialVaultEntry, tesatlas_hub::CredentialVaultEntryError> {
    #[cfg(feature = "credential-relocation-fixture")]
    { tesatlas_hub::CredentialVaultEntry::open_for_test(store, admission, data, true) }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    { tesatlas_hub::open_credential_vault_for_entry(store, admission, data) }
}

async fn run_control(
    config_path: &Path,
    requested_vehicle_id: Option<uuid::Uuid>,
    command: &ControlCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    // Refused up front, before any configuration or catalogue is opened: an
    // invitation that silently ignored the flag would grant every vehicle.
    if requested_vehicle_id.is_some() && matches!(command, ControlCommand::Pair { .. }) {
        return Err(CONTROL_PAIR_VEHICLE_ID_USAGE.into());
    }
    let config = HubConfig::load(config_path)?;
    if let Some((action, confirmed)) = vehicle_action(command) {
        return run_vehicle_action(&config, requested_vehicle_id, action, confirmed).await;
    }
    if matches!(command, ControlCommand::SignOut | ControlCommand::RotateCredentialKey) {
        #[cfg(feature = "credential-relocation-fixture")]
        credential_relocation_fixture::require_entry_path(&config.data_dir)?;
        let result = run_credential_control_with(&config, command, || {
            #[cfg(feature = "credential-relocation-fixture")]
            tesatlas_hub::platform::r78_recording::with_fixture_backend(|backend| backend.persistent_stop())?;
            #[cfg(all(target_os = "macos", not(feature = "credential-relocation-fixture")))]
            tesatlas_hub::macos_launch_agent::stop_installed()?;
            #[cfg(all(target_os = "linux", not(feature = "credential-relocation-fixture")))]
            tesatlas_hub::linux_systemd::apply(tesatlas_hub::linux_systemd::ServiceAction::Stop)?;
            Ok(())
        }, open_control_credential_vault)?;
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    let store = HubStore::open_existing(&config.data_dir)?;
    match command {
        ControlCommand::PairedDevices => {
            println!("{}", paired_devices_json(&store)?);
            return Ok(());
        }
        ControlCommand::Pair {
            label,
            expires_in_seconds,
            json,
            vehicles,
            all_vehicles,
        } => {
            // Like the other control commands this takes short SQLite
            // transactions and no instance lock, so pairing works while the
            // Hub serves. The invitation is single-use and carries no scope.
            let mut stdout = std::io::stdout().lock();
            pair_with_configured_tls(
                &store,
                config.tls.as_ref(),
                label,
                *expires_in_seconds,
                *json,
                pairing_scope_from_flags(vehicles.clone(), *all_vehicles),
                &mut stdout,
            )
            .await?;
            return Ok(());
        }
        ControlCommand::RevokeDevice { device_id } => {
            store.revoke_device(*device_id)?;
            println!(
                "{}",
                serde_json::json!({"status": "revoked", "deviceId": device_id})
            );
            return Ok(());
        }
        ControlCommand::SignOut | ControlCommand::RotateCredentialKey => unreachable!("credential control returned before store opening"),
        _ => {}
    }
    let vehicle_id = control_target(&store, requested_vehicle_id)?;
    match command {
        ControlCommand::PairedDevices
        | ControlCommand::Pair { .. }
        | ControlCommand::RevokeDevice { .. }
        | ControlCommand::SignOut
        | ControlCommand::RotateCredentialKey => {
            unreachable!("global controls returned before vehicle selection")
        }
        ControlCommand::Wake { .. }
        | ControlCommand::ClimateStart { .. }
        | ControlCommand::ClimateStop { .. }
        | ControlCommand::ChargeStart { .. }
        | ControlCommand::ChargeStop { .. }
        | ControlCommand::SetChargeLimit { .. }
        | ControlCommand::Lock { .. }
        | ControlCommand::Unlock { .. }
        | ControlCommand::FlashLights { .. }
        | ControlCommand::HonkHorn { .. } => {
            unreachable!("vehicle actions returned before the catalogue was opened")
        }
        ControlCommand::Settings {
            enabled,
            streaming,
            suspend_after_idle_min,
            suspend_min,
            require_locked,
            free_supercharging,
            lfp_battery,
        } => {
            validate_streaming_setting(config.collector.provider, *streaming)?;
            let mut settings = store.load_car_settings(vehicle_id)?;
            let changed = enabled.is_some()
                || streaming.is_some()
                || suspend_after_idle_min.is_some()
                || suspend_min.is_some()
                || require_locked.is_some()
                || free_supercharging.is_some()
                || lfp_battery.is_some();
            if let Some(value) = enabled {
                settings.enabled = *value;
            }
            if let Some(value) = streaming {
                settings.use_streaming_api = *value;
            }
            if let Some(value) = suspend_after_idle_min {
                settings.suspend_after_idle_min = *value;
            }
            if let Some(value) = suspend_min {
                settings.suspend_min = *value;
                settings.suspend_min_resolved = true;
            }
            if let Some(value) = require_locked {
                settings.req_not_unlocked = *value;
            }
            if let Some(value) = free_supercharging {
                settings.free_supercharging = *value;
            }
            if let Some(value) = lfp_battery {
                settings.lfp_battery = *value;
            }
            if changed {
                store.replace_car_settings(vehicle_id, &settings)?;
            }
            println!(
                "{}",
                serde_json::json!({
                    "status": if changed { "updated" } else { "ok" },
                    "vehicleId": vehicle_id,
                    "settings": settings,
                })
            );
        }
        ControlCommand::Pause | ControlCommand::Resume => {
            let mut settings = store.load_car_settings(vehicle_id)?;
            settings.enabled = matches!(command, ControlCommand::Resume);
            store.replace_car_settings(vehicle_id, &settings)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": if settings.enabled { "running" } else { "paused" },
                    "vehicleId": vehicle_id,
                })
            );
        }
        ControlCommand::Geofences => {
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "vehicleId": vehicle_id,
                    "geofences": store.geofences(vehicle_id)?,
                })
            );
        }
        ControlCommand::SetGeofence { .. } => {
            println!("{}", run_set_geofence(&store, vehicle_id, command)?);
        }
        ControlCommand::DeleteGeofence { id } => {
            store.delete_geofence(vehicle_id, *id)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "deleted",
                    "vehicleId": vehicle_id,
                    "geofenceId": id,
                })
            );
        }
        ControlCommand::SetChargeCost {
            charge_id,
            cost,
            mode,
        } => {
            let document = run_set_charge_cost(
                &store,
                vehicle_id,
                *charge_id,
                *cost,
                mode,
                &mut std::io::stderr(),
            )?;
            println!("{document}");
        }
        ControlCommand::ExportGpx { drive_id } => {
            let stdout = std::io::stdout();
            let mut writer = std::io::BufWriter::new(stdout.lock());
            export_drive_gpx(&store, vehicle_id, *drive_id, &mut writer)?;
            writer.flush()?;
        }
    }
    Ok(())
}
