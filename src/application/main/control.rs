// SPDX-License-Identifier: AGPL-3.0-only

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("teslatlas-hub: {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
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

fn explicit_vehicle_action(
    command: &ControlCommand,
) -> Result<Option<LegacyVehicleAction>, Box<dyn std::error::Error>> {
    let (confirmed, action) = match command {
        ControlCommand::Wake { confirm } => (*confirm, LegacyVehicleAction::Wake),
        ControlCommand::ClimateStart { confirm } => (*confirm, LegacyVehicleAction::ClimateStart),
        ControlCommand::ClimateStop { confirm } => (*confirm, LegacyVehicleAction::ClimateStop),
        ControlCommand::ChargeStart { confirm } => (*confirm, LegacyVehicleAction::ChargeStart),
        ControlCommand::ChargeStop { confirm } => (*confirm, LegacyVehicleAction::ChargeStop),
        ControlCommand::SetChargeLimit { percent, confirm } => {
            (*confirm, LegacyVehicleAction::SetChargeLimit(*percent))
        }
        ControlCommand::Lock { confirm } => (*confirm, LegacyVehicleAction::Lock),
        ControlCommand::Unlock { confirm } => (*confirm, LegacyVehicleAction::Unlock),
        ControlCommand::FlashLights { confirm } => (*confirm, LegacyVehicleAction::FlashLights),
        ControlCommand::HonkHorn { confirm } => (*confirm, LegacyVehicleAction::HonkHorn),
        _ => return Ok(None),
    };
    if !confirmed {
        return Err("vehicle action requires --confirm".into());
    }
    Ok(Some(action))
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

#[cfg(unix)]
fn validate_legacy_setup_provider(provider: CollectorProvider) -> Result<(), &'static str> {
    if provider != CollectorProvider::Legacy {
        return Err("setup requires collector.provider = \"legacy\"");
    }
    Ok(())
}

fn clear_provider_credentials(
    data_dir: &Path,
    store: &HubStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut failures = Vec::new();
    if let Err(error) = remove_fleet_key_and_tokens(data_dir, store) {
        failures.push(format!("Fleet credentials: {error}"));
    }
    if let Err(error) = remove_key_and_tokens(data_dir, store) {
        failures.push(format!("Legacy credentials: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "provider credential removal incomplete: {}",
            failures.join("; ")
        ))
        .into())
    }
}

fn persist_legacy_setup_and_drop_fleet(
    data_dir: &Path,
    store: &HubStore,
    tokens: &OwnerTokens,
) -> Result<(), Box<dyn std::error::Error>> {
    let encryption_key = random_encryption_key()?;
    let (access, refresh) = encrypt_legacy_owner_tokens(&encryption_key, tokens)?;
    let stored = TeslaMateLegacyTokenStore::imported(access, refresh)?;
    replace_key_and_tokens(data_dir, store, &encryption_key, &stored).map_err(|error| {
        provider_switch_outcome_ambiguous("persisting Legacy credentials", error)
    })?;
    remove_fleet_key_and_tokens(data_dir, store).map_err(|error| {
        provider_switch_outcome_ambiguous("removing previous Fleet credentials", error)
    })?;
    Ok(())
}

/// Copy TeslaMate Owner tokens into Hub. Import never writes TeslaMate
/// PostgreSQL and never deletes Fleet credentials — those stay until an
/// explicit `setup` / `setup-fleet` / `sign-out`.
fn persist_migrated_legacy_tokens(
    data_dir: &Path,
    store: &HubStore,
    encryption_key: &[u8],
    stored: &TeslaMateLegacyTokenStore,
) -> Result<(), Box<dyn std::error::Error>> {
    replace_key_and_tokens(data_dir, store, encryption_key, stored)?;
    Ok(())
}

fn persist_fleet_setup_and_drop_legacy(
    data_dir: &Path,
    store: &HubStore,
    credentials: &FleetSetupCredentials,
    now: SystemTime,
) -> Result<(), Box<dyn std::error::Error>> {
    persist_fleet_setup_credentials(store, data_dir, credentials, now).map_err(|error| {
        provider_switch_outcome_ambiguous("persisting Fleet credentials", error)
    })?;
    remove_key_and_tokens(data_dir, store).map_err(|error| {
        provider_switch_outcome_ambiguous("removing previous Legacy credentials", error)
    })?;
    Ok(())
}

#[cfg(unix)]
const PROVIDER_SWITCH_OUTCOME_AMBIGUOUS: &str = "TESLATLAS_PROVIDER_SWITCH_OUTCOME_AMBIGUOUS";

#[cfg(unix)]
const MIGRATION_OUTCOME_AMBIGUOUS: &str = "TESLATLAS_MIGRATION_OUTCOME_AMBIGUOUS";

#[cfg(unix)]
fn provider_switch_outcome_ambiguous(
    action: &str,
    error: impl std::fmt::Display,
) -> Box<dyn std::error::Error> {
    std::io::Error::other(format!(
        "{PROVIDER_SWITCH_OUTCOME_AMBIGUOUS}: {action}: {error}; Hub must remain stopped until status and diagnostics confirm the selected provider"
    ))
    .into()
}

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

async fn run_control(
    config_path: &Path,
    requested_vehicle_id: Option<uuid::Uuid>,
    command: &ControlCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = HubConfig::load(config_path)?;
    let store = HubStore::open_existing(&config.data_dir)?;
    match command {
        ControlCommand::PairedDevices => {
            println!("{}", serde_json::to_string(&store.list_paired_devices()?)?);
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
        ControlCommand::SignOut => {
            #[cfg(target_os = "macos")]
            teslatlas_hub::macos_launch_agent::stop_installed()?;
            #[cfg(target_os = "linux")]
            teslatlas_hub::linux_systemd::apply(teslatlas_hub::linux_systemd::ServiceAction::Stop)?;

            #[cfg(unix)]
            let _admission = AdmittedUserHub::admit(&config.data_dir)?;
            let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
            clear_provider_credentials(&config.data_dir, &store)?;
            catalogue_checkpoint.finish()?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "signed_out",
                    "service": "stopped",
                })
            );
            return Ok(());
        }
        _ => {}
    }
    let vehicle_id = control_target(&store, requested_vehicle_id)?;
    if let Some(action) = explicit_vehicle_action(command)? {
        let report =
            collector::request_resident_vehicle_action(&config.data_dir, vehicle_id, action)
                .await?;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    match command {
        ControlCommand::PairedDevices
        | ControlCommand::RevokeDevice { .. }
        | ControlCommand::SignOut => {
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
            unreachable!("vehicle actions returned before local controls")
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
        ControlCommand::SetGeofence {
            id,
            name,
            latitude,
            longitude,
            radius_m,
            billing_type,
            cost_per_unit,
            session_fee,
            recalculate_missing_costs,
        } => {
            let billing_type = billing_type
                .parse::<GeofenceBillingType>()
                .map_err(|_| "--billing-type must be per_kwh or per_minute")?;
            let geofence = store.save_geofence(
                vehicle_id,
                *id,
                teslatlas_hub::teslamate_projection::TeslaMateGeofence {
                    id: id.unwrap_or_default(),
                    name: name.clone(),
                    latitude: Some(*latitude),
                    longitude: Some(*longitude),
                    radius_m: Some(*radius_m),
                    billing_type: Some(billing_type),
                    cost_per_unit: *cost_per_unit,
                    session_fee: *session_fee,
                },
            )?;
            let recalculated_charge_costs = if *recalculate_missing_costs {
                store.recalculate_missing_charge_costs(vehicle_id, geofence.id)?
            } else {
                0
            };
            println!(
                "{}",
                serde_json::json!({
                    "status": "updated",
                    "vehicleId": vehicle_id,
                    "geofence": geofence,
                    "recalculatedChargeCosts": recalculated_charge_costs,
                })
            );
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
            let charge = match mode.as_str() {
                "total" => store.set_charge_cost(vehicle_id, *charge_id, *cost)?,
                "per_kwh" => store.set_charge_cost_rate(
                    vehicle_id,
                    *charge_id,
                    *cost,
                    GeofenceBillingType::PerKwh,
                )?,
                "per_minute" => store.set_charge_cost_rate(
                    vehicle_id,
                    *charge_id,
                    *cost,
                    GeofenceBillingType::PerMinute,
                )?,
                _ => return Err("--mode must be total, per_kwh, or per_minute".into()),
            };
            println!(
                "{}",
                serde_json::json!({
                    "status": "updated",
                    "vehicleId": vehicle_id,
                    "charge": charge,
                })
            );
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
