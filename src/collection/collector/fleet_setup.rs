// SPDX-License-Identifier: AGPL-3.0-only

/// Restore the latest complete Fleet snapshot for one configured VIN. Native
/// Fleet Telemetry sends deltas, so restart must seed the in-memory
/// accumulator from this durable allowlisted state.
pub(crate) fn fleet_telemetry_seed_for_vin(
    store: &HubStore,
    vin: &str,
) -> Result<Option<Value>, CollectorError> {
    let (vehicle_id, _, _) = configured_fleet_vehicle_for_vin(store, vin)?;
    let observations = store.current_observations_for_vehicle(vehicle_id)?;
    Ok(observations.into_iter().rev().find_map(|observation| {
        (observation
            .payload
            .get("record_type")
            .and_then(Value::as_str)
            == Some(provider_vehicle_data_record_type(CollectorProvider::Fleet)))
        .then(|| {
            observation
                .payload
                .get("provider_raw_json")
                .and_then(|raw| raw.get("response"))
                .cloned()
        })
        .flatten()
    }))
}

/// Commit one accumulated native Fleet Telemetry snapshot through the same
/// atomic lifecycle and projection path used by ordinary Fleet responses.
/// Returning success is the receiver's permission to acknowledge the vehicle.
pub(crate) async fn persist_fleet_telemetry_snapshot(
    store: &HubStore,
    cursor_key: &CursorKey,
    snapshot: &FleetTelemetrySnapshot,
) -> Result<ManualCollectionReport, CollectorError> {
    let (_, eid, settings) = configured_fleet_vehicle_for_vin(store, &snapshot.vin)?;
    let source_vehicle_id =
        VehicleId::try_from_i64(eid).ok_or(CollectorError::SelectedVehicleMissing)?;
    let stream_id =
        StreamVehicleId::try_from_i64(eid).ok_or(CollectorError::SelectedVehicleMissing)?;
    let source_vehicle_state = snapshot
        .owner_data
        .get("state")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64)
        .unwrap_or("unknown")
        .to_owned();
    let data = VehicleData::from_provider_raw_json(
        source_vehicle_id,
        serde_json::json!({"response": snapshot.owner_data}),
    )?;
    let vehicle = Vehicle {
        id: source_vehicle_id,
        stream_id,
        vin: snapshot.vin.clone(),
        state: source_vehicle_state,
        display_name: None,
        settings,
    };
    finish_collection_for_provider(
        store,
        cursor_key,
        &ManualCollection {
            vehicles: vec![vehicle],
            snapshots: vec![data],
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FleetTelemetrySetupReport {
    pub vehicles_configured: usize,
    pub vehicles_skipped: usize,
    pub vehicles_revoked: usize,
    pub expires_at: u64,
}

/// Send the fixed low-cost Fleet Telemetry field policy through Tesla's local
/// command proxy. VINs come only from configured Hub identities; no discovery
/// or vehicle-data request is made by this operation.
#[cfg(unix)]
pub async fn configure_fleet_telemetry_for_admitted_user(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
) -> Result<FleetTelemetrySetupReport, CollectorError> {
    let mut manager = FleetAuthManager::from_store_for_admitted_user(
        store.clone(),
        &config.data_dir,
        Arc::clone(&admission),
    )?;
    let auth_api = FleetAuthApi::new(
        manager.region(),
        Duration::from_secs(config.collector.request_timeout_seconds),
    )?;
    apply_fleet_telemetry_configuration(store, config, &mut manager, &auth_api, &admission).await
}

#[cfg(unix)]
async fn apply_fleet_telemetry_configuration(
    store: &HubStore,
    config: &HubConfig,
    manager: &mut FleetAuthManager,
    auth_api: &FleetAuthApi,
    admission: &Arc<crate::hub_user_process::AdmittedUserHub>,
) -> Result<FleetTelemetrySetupReport, CollectorError> {
    admission.assert_sensitive_access()?;
    let telemetry = config
        .collector
        .fleet_telemetry
        .as_ref()
        .ok_or(ConfigError::InvalidFleetTelemetry)?;
    let proxy = fleet_command_proxy(config)?.ok_or(ConfigError::InvalidFleetTelemetry)?;
    let certificate =
        read_fleet_proxy_root_certificate(&telemetry.ca_certificate_path, 128 * 1024)?;
    let certificate =
        std::str::from_utf8(&certificate).map_err(|_| FleetApiConfigError::InvalidTelemetryCa)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CollectorError::InvalidReceiptTimestamp)?
        .as_secs();
    let expires_at = now
        .checked_add(FLEET_TELEMETRY_CONFIG_LIFETIME_SECONDS)
        .ok_or(CollectorError::InvalidReceiptTimestamp)?;
    let destination = FleetTelemetryConfigBuilder::new(
        telemetry.hostname.clone(),
        telemetry.port,
        certificate,
        expires_at,
    )
    .with_recommended_fields()
    .build()?;
    let mut enabled_vins = Vec::new();
    let mut disabled_vins = Vec::new();
    for (vehicle_id, _, settings) in store.configured_tesla_vehicles()? {
        let Some((_, Some(vin))) = store.configured_tesla_vehicle_identity(vehicle_id)? else {
            return Err(CollectorError::SelectedVehicleMissing);
        };
        if settings.enabled {
            enabled_vins.push(vin);
        } else {
            disabled_vins.push(VehicleVin::parse(&vin)?);
        }
    }
    manager.refresh_if_due(auth_api, SystemTime::now()).await?;

    let (vehicles_configured, vehicles_skipped) = if enabled_vins.is_empty() {
        (0, 0)
    } else {
        let vins = FleetTelemetryVins::parse(&enabled_vins)?;
        admission.assert_sensitive_access()?;
        let first = proxy
            .configure_fleet_telemetry(
                manager.access_token_for_sensitive_use()?,
                &vins,
                &destination,
            )
            .await;
        let result = if matches!(first, Err(FleetApiError::HttpStatus(401 | 403))) {
            manager.mark_refresh_due();
            manager.refresh_if_due(auth_api, SystemTime::now()).await?;
            admission.assert_sensitive_access()?;
            proxy
                .configure_fleet_telemetry(
                    manager.access_token_for_sensitive_use()?,
                    &vins,
                    &destination,
                )
                .await?
        } else {
            first?
        };
        (result.updated_vehicles, result.skipped_vehicles.len())
    };

    let mut vehicles_revoked = 0;
    for vin in disabled_vins {
        admission.assert_sensitive_access()?;
        let first = proxy
            .remove_fleet_telemetry(manager.access_token_for_sensitive_use()?, &vin)
            .await;
        let removal = if matches!(first, Err(FleetApiError::HttpStatus(401 | 403))) {
            manager.mark_refresh_due();
            manager.refresh_if_due(auth_api, SystemTime::now()).await?;
            admission.assert_sensitive_access()?;
            proxy
                .remove_fleet_telemetry(manager.access_token_for_sensitive_use()?, &vin)
                .await
        } else {
            first
        };
        match removal {
            Ok(()) => vehicles_revoked += 1,
            Err(error) if error.http_status() == Some(404) => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(FleetTelemetrySetupReport {
        vehicles_configured,
        vehicles_skipped,
        vehicles_revoked,
        expires_at,
    })
}

fn configured_fleet_vehicle_for_vin(
    store: &HubStore,
    vin: &str,
) -> Result<(Uuid, i64, crate::hub_pack::ProjectionCarSettings), CollectorError> {
    let mut matched = None;
    for (vehicle_id, eid, settings) in store.configured_tesla_vehicles()? {
        let Some((identity_eid, configured_vin)) =
            store.configured_tesla_vehicle_identity(vehicle_id)?
        else {
            continue;
        };
        if identity_eid == eid && configured_vin.as_deref() == Some(vin) {
            if !settings.enabled {
                continue;
            }
            if matched.is_some() {
                return Err(CollectorError::SelectedVehicleMissing);
            }
            matched = Some((vehicle_id, eid, settings));
        }
    }
    matched.ok_or(CollectorError::SelectedVehicleMissing)
}

/// Configure one clean Hub directly from a bounded legacy token pair. This
/// performs products discovery only: no vehicle-data read, wake, or command.
pub async fn setup_native_vehicle(
    store: &HubStore,
    config: &HubConfig,
    tokens: &OwnerTokens,
    requested_vehicle_id: Option<i64>,
) -> Result<NativeSetupReport, CollectorError> {
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::NativeSetupStoreMismatch);
    }
    if !config.collector.legacy_auth.enabled {
        return Err(CollectorError::NativeSetupLegacyAuthRequired);
    }

    let auth = LegacyAuth::from_access_token(
        tokens.access_token().to_owned(),
        tokens.refresh_token().to_owned(),
    )?;
    let client = OwnerApi::new(
        config
            .collector
            .owner_api_options_for_region(auth.region())?,
    )?;
    setup_native_vehicle_with_client(
        store,
        &config.data_dir,
        &client,
        &auth,
        requested_vehicle_id,
    )
    .await
}

/// Configure every discovered vehicle from one account-wide legacy pair.
/// Discovery is one bounded products request and never wakes a vehicle.
pub async fn setup_native_vehicles(
    store: &HubStore,
    config: &HubConfig,
    tokens: &OwnerTokens,
) -> Result<NativeSetupBatchReport, CollectorError> {
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::NativeSetupStoreMismatch);
    }
    if !config.collector.legacy_auth.enabled {
        return Err(CollectorError::NativeSetupLegacyAuthRequired);
    }
    let auth = LegacyAuth::from_access_token(
        tokens.access_token().to_owned(),
        tokens.refresh_token().to_owned(),
    )?;
    let client = OwnerApi::new(
        config
            .collector
            .owner_api_options_for_region(auth.region())?,
    )?;
    setup_native_vehicles_with_client(store, &config.data_dir, &client, &auth).await
}

/// Configure one vehicle from a bounded Fleet OAuth credential object.
/// Discovery is read-only and never wakes a vehicle.
pub async fn setup_fleet_vehicle(
    store: &HubStore,
    config: &HubConfig,
    credentials: &FleetSetupCredentials,
    admission: &crate::hub_user_process::AdmittedUserHub,
    requested_vehicle_id: Option<i64>,
) -> Result<NativeSetupReport, CollectorError> {
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::NativeSetupStoreMismatch);
    }
    if config.collector.provider != CollectorProvider::Fleet {
        return Err(CollectorError::NativeSetupFleetProviderRequired);
    }
    let client = FleetApi::new(
        credentials.region(),
        Duration::from_secs(config.collector.request_timeout_seconds),
    )?;
    let access_token = credentials.access_token()?;
    admission.assert_sensitive_access()?;
    let vehicles = client.list_vehicles(&access_token).await?;
    ensure_fleet_inventory_contains_configured(store, &vehicles)?;
    let mut vehicle = select_native_setup_vehicle(vehicles, requested_vehicle_id)?;
    let existing = store.configured_tesla_vehicles()?;
    if let Some(settings) = configured_settings_for_discovered_vehicle(store, &existing, &vehicle)?
    {
        vehicle.settings = settings;
    }
    vehicle.settings.use_streaming_api = false;
    let selected_vehicle_id =
        i64::try_from(vehicle.id.get()).map_err(|_| CollectorError::NativeSetupVehicleIdInvalid)?;
    let display_name = vehicle.display_name.clone();
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let report = finish_collection_for_provider(
        store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![vehicle],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await?;
    Ok(NativeSetupReport {
        selected_vehicle_id,
        display_name,
        snapshots_published: report.snapshots_published,
    })
}

/// Configure every vehicle returned by one Fleet account without waking any.
pub async fn setup_fleet_vehicles(
    store: &HubStore,
    config: &HubConfig,
    credentials: &FleetSetupCredentials,
    admission: &crate::hub_user_process::AdmittedUserHub,
) -> Result<NativeSetupBatchReport, CollectorError> {
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::NativeSetupStoreMismatch);
    }
    if config.collector.provider != CollectorProvider::Fleet {
        return Err(CollectorError::NativeSetupFleetProviderRequired);
    }
    let client = FleetApi::new(
        credentials.region(),
        Duration::from_secs(config.collector.request_timeout_seconds),
    )?;
    let access_token = credentials.access_token()?;
    admission.assert_sensitive_access()?;
    let mut vehicles = client.list_vehicles(&access_token).await?;
    ensure_fleet_inventory_contains_configured(store, &vehicles)?;
    if vehicles.is_empty() {
        return Err(CollectorError::NativeSetupNoVehicles);
    }
    vehicles.sort_by_key(|vehicle| vehicle.id);
    vehicles.dedup_by_key(|vehicle| vehicle.id);
    let existing = store.configured_tesla_vehicles()?;
    for vehicle in &mut vehicles {
        if let Some(settings) =
            configured_settings_for_discovered_vehicle(store, &existing, vehicle)?
        {
            vehicle.settings = settings;
        }
        vehicle.settings.use_streaming_api = false;
    }
    let configured = vehicles
        .iter()
        .map(|vehicle| {
            i64::try_from(vehicle.id.get())
                .map(|vehicle_id| NativeSetupVehicle {
                    vehicle_id,
                    display_name: vehicle.display_name.clone(),
                })
                .map_err(|_| CollectorError::NativeSetupVehicleIdInvalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let report = finish_collection_for_provider(
        store,
        &cursor_key,
        &ManualCollection {
            vehicles,
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await?;
    Ok(NativeSetupBatchReport {
        vehicles: configured,
        snapshots_published: report.snapshots_published,
    })
}

fn configured_settings_for_discovered_vehicle(
    store: &HubStore,
    configured: &[(Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
    discovered: &Vehicle,
) -> Result<Option<crate::hub_pack::ProjectionCarSettings>, CollectorError> {
    let mut matched = None;
    for (hub_vehicle_id, configured_eid, settings) in configured {
        let (_, configured_vin) = store
            .configured_tesla_vehicle_identity(*hub_vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let identity_matches = *configured_eid as u64 == discovered.id.get()
            || configured_vin
                .as_deref()
                .filter(|vin| !vin.is_empty())
                .is_some_and(|vin| vin.eq_ignore_ascii_case(&discovered.vin));
        if identity_matches {
            if matched.is_some() {
                return Err(CollectorError::FleetSetupInventoryMismatch);
            }
            matched = Some(settings.clone());
        }
    }
    Ok(matched)
}

fn ensure_fleet_inventory_contains_configured(
    store: &HubStore,
    discovered: &[Vehicle],
) -> Result<(), CollectorError> {
    let mut matched_discovered = HashSet::new();
    for (hub_vehicle_id, configured_eid, _) in store.configured_tesla_vehicles()? {
        let (_, configured_vin) = store
            .configured_tesla_vehicle_identity(hub_vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let matches = discovered
            .iter()
            .enumerate()
            .filter_map(|(index, vehicle)| {
                (vehicle.id.get() == configured_eid as u64
                    || configured_vin
                        .as_deref()
                        .filter(|vin| !vin.is_empty())
                        .is_some_and(|vin| vin.eq_ignore_ascii_case(&vehicle.vin)))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 || !matched_discovered.insert(matches[0]) {
            return Err(CollectorError::FleetSetupInventoryMismatch);
        }
    }
    Ok(())
}

/// Execute one explicitly confirmed legacy vehicle action. This path never
/// refreshes or retries credentials and is not reachable from the collector.
async fn execute_resident_legacy_vehicle_action(
    store: &HubStore,
    client: &OwnerApi,
    manager: &Arc<tokio::sync::Mutex<LegacyAuthManager>>,
    fuse: &Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
    refresh: &Arc<LegacyRefreshCoordinator>,
    hub_vehicle_id: Uuid,
    action: LegacyVehicleAction,
) -> Result<LegacyVehicleActionReport, ResidentActionExecutionError> {
    refresh
        .wait_for_prior()
        .await
        .map_err(|_| ResidentActionExecutionError::Authentication)?;
    let tesla_eid = store
        .configured_tesla_vehicles()?
        .into_iter()
        .find_map(|(vehicle_id, eid, _)| (vehicle_id == hub_vehicle_id).then_some(eid))
        .ok_or(ResidentActionExecutionError::VehicleMissing)?;
    let vehicle_id =
        VehicleId::try_from_i64(tesla_eid).ok_or(ResidentActionExecutionError::VehicleMissing)?;
    let receipt_id = store.begin_outbound_request(&OutboundRequestStart {
        correlation_id: Uuid::new_v4(),
        vehicle_tesla_id: Some(tesla_eid),
        transport: OutboundRequestTransport::OwnerApi,
        operation: if action == LegacyVehicleAction::Wake {
            OutboundRequestOperation::VehicleWake
        } else {
            OutboundRequestOperation::VehicleCommand
        },
        safety_class: if action == LegacyVehicleAction::Wake {
            OutboundRequestSafetyClass::DirectWakeCommand
        } else {
            OutboundRequestSafetyClass::ExplicitVehicleCommand
        },
        precondition: OutboundRequestPrecondition::NotRequired,
    })?;
    let result = {
        let mut fuse = fuse.lock().await;
        let mut manager = manager.lock().await;
        client
            .execute_vehicle_action_with_legacy_auth_fused(
                &mut manager,
                &mut fuse,
                vehicle_id,
                action,
            )
            .await
    };
    if matches!(
        &result,
        Err(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401)))
    ) {
        refresh
            .enqueue(client.clone(), Arc::clone(manager), Arc::clone(fuse))
            .await;
    }
    let completion = legacy_action_completion(result.as_ref().err());
    store
        .complete_outbound_request(receipt_id, &completion)
        .map_err(ResidentActionExecutionError::CompletionUnknown)?;
    Ok(LegacyVehicleActionReport {
        provider: CollectorProvider::Legacy,
        hub_vehicle_id,
        tesla_eid,
        action,
        result: result?,
        audit_receipt_id: receipt_id.0,
    })
}

async fn execute_resident_fleet_vehicle_action(
    store: &HubStore,
    api: &FleetApi,
    auth_api: &FleetAuthApi,
    command_proxy: Option<&FleetCommandProxy>,
    manager: &Arc<tokio::sync::Mutex<FleetAuthManager>>,
    hub_vehicle_id: Uuid,
    action: LegacyVehicleAction,
) -> Result<LegacyVehicleActionReport, ResidentActionExecutionError> {
    let (tesla_eid, vin) = store
        .configured_tesla_vehicle_identity(hub_vehicle_id)?
        .ok_or(ResidentActionExecutionError::VehicleMissing)?;
    let vin = vin
        .as_deref()
        .ok_or(ResidentActionExecutionError::VehicleMissing)
        .and_then(|vin| {
            VehicleVin::parse(vin).map_err(|_| ResidentActionExecutionError::VehicleMissing)
        })?;
    let mut manager = manager.lock().await;
    manager
        .refresh_if_due(auth_api, SystemTime::now())
        .await
        .map_err(ResidentActionExecutionError::FleetCredential)?;
    let receipt_id = store.begin_outbound_request(&OutboundRequestStart {
        correlation_id: Uuid::new_v4(),
        vehicle_tesla_id: Some(tesla_eid),
        transport: OutboundRequestTransport::FleetApi,
        operation: if action == LegacyVehicleAction::Wake {
            OutboundRequestOperation::VehicleWake
        } else {
            OutboundRequestOperation::VehicleCommand
        },
        safety_class: if action == LegacyVehicleAction::Wake {
            OutboundRequestSafetyClass::DirectWakeCommand
        } else {
            OutboundRequestSafetyClass::ExplicitVehicleCommand
        },
        precondition: OutboundRequestPrecondition::NotRequired,
    })?;
    let access_token = match manager.access_token_for_sensitive_use() {
        Ok(token) => token,
        Err(error) => {
            store
                .complete_outbound_request(
                    receipt_id,
                    &OutboundRequestCompletion {
                        outcome: OutboundRequestOutcome::Cancelled,
                        http_status: None,
                        retry_after_seconds: None,
                    },
                )
                .map_err(ResidentActionExecutionError::CompletionUnknown)?;
            return Err(ResidentActionExecutionError::FleetCredential(error));
        }
    };
    let result = match action {
        LegacyVehicleAction::Wake => api
            .wake(access_token, &vin)
            .await
            .map(|WakeResult { state }| LegacyVehicleActionResult { state: Some(state) }),
        action => {
            let proxy = command_proxy.ok_or(FleetApiError::CommandProxyUnavailable);
            match proxy {
                Ok(proxy) => proxy
                    .execute(access_token, &vin, fleet_command(action)?)
                    .await
                    .map(|FleetCommandResult { .. }| LegacyVehicleActionResult { state: None }),
                Err(error) => Err(error),
            }
        }
    };
    if matches!(result, Err(FleetApiError::HttpStatus(401 | 403))) {
        manager.mark_refresh_due();
    }
    let completion = fleet_action_completion(result.as_ref().err());
    store
        .complete_outbound_request(receipt_id, &completion)
        .map_err(ResidentActionExecutionError::CompletionUnknown)?;
    Ok(LegacyVehicleActionReport {
        provider: CollectorProvider::Fleet,
        hub_vehicle_id,
        tesla_eid,
        action,
        result: result.map_err(ResidentActionExecutionError::FleetProvider)?,
        audit_receipt_id: receipt_id.0,
    })
}

fn fleet_command(
    action: LegacyVehicleAction,
) -> Result<FleetCommand, ResidentActionExecutionError> {
    match action {
        LegacyVehicleAction::Wake => Err(ResidentActionExecutionError::FleetProvider(
            FleetApiError::InvalidCommand,
        )),
        LegacyVehicleAction::ClimateStart => Ok(FleetCommand::ClimateStart),
        LegacyVehicleAction::ClimateStop => Ok(FleetCommand::ClimateStop),
        LegacyVehicleAction::ChargeStart => Ok(FleetCommand::ChargeStart),
        LegacyVehicleAction::ChargeStop => Ok(FleetCommand::ChargeStop),
        LegacyVehicleAction::SetChargeLimit(percent) => {
            Ok(FleetCommand::SetChargeLimit { percent })
        }
        LegacyVehicleAction::Lock => Ok(FleetCommand::Lock),
        LegacyVehicleAction::Unlock => Ok(FleetCommand::Unlock),
        LegacyVehicleAction::FlashLights => Ok(FleetCommand::FlashLights),
        LegacyVehicleAction::HonkHorn => Ok(FleetCommand::HonkHorn),
    }
}

#[derive(Debug, Error)]
enum ResidentActionExecutionError {
    #[error("vehicle command target is not configured")]
    VehicleMissing,
    #[error("vehicle command audit could not start")]
    Audit(#[from] StoreError),
    #[error("resident vehicle credential authority is unavailable")]
    Authentication,
    #[error("vehicle provider rejected the command")]
    Provider(#[from] OwnerApiAuthError),
    #[error("Fleet provider rejected the command")]
    FleetProvider(#[from] FleetApiError),
    #[error("Fleet credential authority is unavailable")]
    FleetCredential(#[from] FleetCredentialError),
    #[error("vehicle command outcome is ambiguous because its audit could not complete")]
    CompletionUnknown(StoreError),
}

fn legacy_action_completion(error: Option<&OwnerApiAuthError>) -> OutboundRequestCompletion {
    let (outcome, http_status, retry_after_seconds) = match error {
        None => (OutboundRequestOutcome::Success, Some(200), None),
        Some(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401)))
        | Some(OwnerApiAuthError::NotSignedIn) => (
            OutboundRequestOutcome::AuthenticationRejected,
            Some(401),
            None,
        ),
        Some(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(status))) => {
            (OutboundRequestOutcome::HttpError, Some(*status), None)
        }
        Some(OwnerApiAuthError::Owner(OwnerApiError::RateLimited {
            retry_after_seconds,
        })) => (
            OutboundRequestOutcome::HttpError,
            Some(429),
            Some(*retry_after_seconds),
        ),
        Some(OwnerApiAuthError::Owner(OwnerApiError::RequestTimeout)) => {
            (OutboundRequestOutcome::Timeout, None, None)
        }
        Some(OwnerApiAuthError::Owner(OwnerApiError::Transport | OwnerApiError::ResponseRead)) => {
            (OutboundRequestOutcome::TransportError, None, None)
        }
        Some(OwnerApiAuthError::Owner(OwnerApiError::ResponseTooLarge)) => {
            (OutboundRequestOutcome::ResponseTooLarge, None, None)
        }
        Some(_) => (OutboundRequestOutcome::ProtocolError, None, None),
    };
    OutboundRequestCompletion {
        outcome,
        http_status,
        retry_after_seconds,
    }
}

fn fleet_action_completion(error: Option<&FleetApiError>) -> OutboundRequestCompletion {
    let (outcome, http_status, retry_after_seconds) = match error {
        None => (OutboundRequestOutcome::Success, Some(200), None),
        Some(FleetApiError::HttpStatus(status @ (401 | 403)))
        | Some(FleetApiError::ProviderHttpStatus {
            status: status @ (401 | 403),
            ..
        }) => (
            OutboundRequestOutcome::AuthenticationRejected,
            Some(*status),
            None,
        ),
        Some(
            FleetApiError::HttpStatus(status) | FleetApiError::ProviderHttpStatus { status, .. },
        ) => (OutboundRequestOutcome::HttpError, Some(*status), None),
        Some(FleetApiError::RateLimited {
            retry_after_seconds,
        }) => (
            OutboundRequestOutcome::HttpError,
            Some(429),
            Some(*retry_after_seconds),
        ),
        Some(FleetApiError::RequestTimeout) => (OutboundRequestOutcome::Timeout, None, None),
        Some(
            FleetApiError::RequestNotSent | FleetApiError::Transport | FleetApiError::ResponseRead,
        ) => (OutboundRequestOutcome::TransportError, None, None),
        Some(FleetApiError::ResponseTooLarge) => {
            (OutboundRequestOutcome::ResponseTooLarge, None, None)
        }
        Some(_) => (OutboundRequestOutcome::ProtocolError, None, None),
    };
    OutboundRequestCompletion {
        outcome,
        http_status,
        retry_after_seconds,
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyVehicleActionReport {
    pub provider: CollectorProvider,
    pub hub_vehicle_id: Uuid,
    pub tesla_eid: i64,
    pub action: LegacyVehicleAction,
    pub result: LegacyVehicleActionResult,
    pub audit_receipt_id: i64,
}

const RESIDENT_CONTROL_PROTOCOL: u8 = 1;
const RESIDENT_CONTROL_REQUEST_BYTES: u64 = 8 * 1024;
const RESIDENT_CONTROL_RESPONSE_BYTES: u64 = 16 * 1024;
const RESIDENT_CONTROL_IO_TIMEOUT: Duration = Duration::from_secs(30);
const RESIDENT_CONTROL_SOCKET_NAME: &str = ".vehicle-control.sock";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResidentVehicleActionRequest {
    protocol: u8,
    hub_vehicle_id: Uuid,
    action: LegacyVehicleAction,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ResidentVehicleActionFailure {
    InvalidRequest,
    VehicleMissing,
    AuthenticationRejected,
    ProviderRejected,
    AuditUnavailable,
    OutcomeAmbiguous,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ResidentVehicleActionResponse {
    Ok { report: LegacyVehicleActionReport },
    Error { code: ResidentVehicleActionFailure },
}

#[derive(Debug, Error)]
pub enum ResidentVehicleActionError {
    #[error("resident Hub vehicle-control service is unavailable")]
    Unavailable,
    #[error("resident Hub vehicle-control request timed out")]
    Timeout,
    #[error("resident Hub vehicle-control protocol failed")]
    Protocol,
    #[error("vehicle command target is not configured")]
    VehicleMissing,
    #[error("resident Hub credentials rejected the vehicle command")]
    AuthenticationRejected,
    #[error("vehicle provider rejected the command")]
    ProviderRejected,
    #[error("vehicle command audit is unavailable")]
    AuditUnavailable,
    #[error("vehicle command outcome is ambiguous; do not repeat it")]
    OutcomeAmbiguous,
}

pub async fn request_resident_vehicle_action(
    data_dir: &Path,
    hub_vehicle_id: Uuid,
    action: LegacyVehicleAction,
) -> Result<LegacyVehicleActionReport, ResidentVehicleActionError> {
    let request = ResidentVehicleActionRequest {
        protocol: RESIDENT_CONTROL_PROTOCOL,
        hub_vehicle_id,
        action,
    };
    let request = serde_json::to_vec(&request).map_err(|_| ResidentVehicleActionError::Protocol)?;
    if request.len() as u64 > RESIDENT_CONTROL_REQUEST_BYTES {
        return Err(ResidentVehicleActionError::Protocol);
    }
    let path = data_dir.join(RESIDENT_CONTROL_SOCKET_NAME);
    let response = tokio::time::timeout(RESIDENT_CONTROL_IO_TIMEOUT, async move {
        let mut socket = UnixStream::connect(path)
            .await
            .map_err(|_| ResidentVehicleActionError::Unavailable)?;
        socket
            .write_all(&request)
            .await
            .map_err(|_| ResidentVehicleActionError::Unavailable)?;
        socket
            .shutdown()
            .await
            .map_err(|_| ResidentVehicleActionError::Unavailable)?;
        let mut response = Vec::new();
        (&mut socket)
            .take(RESIDENT_CONTROL_RESPONSE_BYTES + 1)
            .read_to_end(&mut response)
            .await
            .map_err(|_| ResidentVehicleActionError::Protocol)?;
        if response.len() as u64 > RESIDENT_CONTROL_RESPONSE_BYTES {
            return Err(ResidentVehicleActionError::Protocol);
        }
        serde_json::from_slice::<ResidentVehicleActionResponse>(&response)
            .map_err(|_| ResidentVehicleActionError::Protocol)
    })
    .await
    .map_err(|_| ResidentVehicleActionError::Timeout)??;

    match response {
        ResidentVehicleActionResponse::Ok { report } => Ok(report),
        ResidentVehicleActionResponse::Error { code } => Err(match code {
            ResidentVehicleActionFailure::InvalidRequest => ResidentVehicleActionError::Protocol,
            ResidentVehicleActionFailure::VehicleMissing => {
                ResidentVehicleActionError::VehicleMissing
            }
            ResidentVehicleActionFailure::AuthenticationRejected => {
                ResidentVehicleActionError::AuthenticationRejected
            }
            ResidentVehicleActionFailure::ProviderRejected => {
                ResidentVehicleActionError::ProviderRejected
            }
            ResidentVehicleActionFailure::AuditUnavailable => {
                ResidentVehicleActionError::AuditUnavailable
            }
            ResidentVehicleActionFailure::OutcomeAmbiguous => {
                ResidentVehicleActionError::OutcomeAmbiguous
            }
        }),
    }
}

struct ResidentControlSocket {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl ResidentControlSocket {
    fn bind(data_dir: &Path) -> Result<Self, CollectorError> {
        let path = data_dir.join(RESIDENT_CONTROL_SOCKET_NAME);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && metadata.uid() == rustix::process::getuid().as_raw()
                    && metadata.nlink() == 1 =>
            {
                std::fs::remove_file(&path).map_err(|_| CollectorError::ResidentControlSocket)?;
            }
            Ok(_) => return Err(CollectorError::ResidentControlSocket),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(CollectorError::ResidentControlSocket),
        }
        let listener =
            UnixListener::bind(&path).map_err(|_| CollectorError::ResidentControlSocket)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| CollectorError::ResidentControlSocket)?;
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| CollectorError::ResidentControlSocket)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(CollectorError::ResidentControlSocket);
        }
        Ok(Self {
            listener,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    async fn serve(
        self,
        store: HubStore,
        client: OwnerApi,
        manager: Arc<tokio::sync::Mutex<LegacyAuthManager>>,
        fuse: Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
        refresh: Arc<LegacyRefreshCoordinator>,
    ) -> Result<(), CollectorError> {
        loop {
            let (mut socket, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| CollectorError::ResidentControlSocket)?;
            let response = match tokio::time::timeout(
                RESIDENT_CONTROL_IO_TIMEOUT,
                read_resident_vehicle_action_request(&mut socket),
            )
            .await
            {
                Ok(Ok(request)) if request.protocol == RESIDENT_CONTROL_PROTOCOL => {
                    match execute_resident_legacy_vehicle_action(
                        &store,
                        &client,
                        &manager,
                        &fuse,
                        &refresh,
                        request.hub_vehicle_id,
                        request.action,
                    )
                    .await
                    {
                        Ok(report) => ResidentVehicleActionResponse::Ok { report },
                        Err(error) => ResidentVehicleActionResponse::Error {
                            code: classify_resident_action_error(&error),
                        },
                    }
                }
                _ => ResidentVehicleActionResponse::Error {
                    code: ResidentVehicleActionFailure::InvalidRequest,
                },
            };
            let response =
                serde_json::to_vec(&response).map_err(|_| CollectorError::ResidentControlSocket)?;
            if response.len() as u64 > RESIDENT_CONTROL_RESPONSE_BYTES {
                return Err(CollectorError::ResidentControlSocket);
            }
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        }
    }

    async fn serve_fleet(
        self,
        store: HubStore,
        api: FleetApi,
        auth_api: FleetAuthApi,
        command_proxy: Option<FleetCommandProxy>,
        manager: Arc<tokio::sync::Mutex<FleetAuthManager>>,
    ) -> Result<(), CollectorError> {
        loop {
            let (mut socket, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| CollectorError::ResidentControlSocket)?;
            let response = match tokio::time::timeout(
                RESIDENT_CONTROL_IO_TIMEOUT,
                read_resident_vehicle_action_request(&mut socket),
            )
            .await
            {
                Ok(Ok(request)) if request.protocol == RESIDENT_CONTROL_PROTOCOL => {
                    match execute_resident_fleet_vehicle_action(
                        &store,
                        &api,
                        &auth_api,
                        command_proxy.as_ref(),
                        &manager,
                        request.hub_vehicle_id,
                        request.action,
                    )
                    .await
                    {
                        Ok(report) => ResidentVehicleActionResponse::Ok { report },
                        Err(error) => ResidentVehicleActionResponse::Error {
                            code: classify_resident_action_error(&error),
                        },
                    }
                }
                _ => ResidentVehicleActionResponse::Error {
                    code: ResidentVehicleActionFailure::InvalidRequest,
                },
            };
            let response =
                serde_json::to_vec(&response).map_err(|_| CollectorError::ResidentControlSocket)?;
            if response.len() as u64 > RESIDENT_CONTROL_RESPONSE_BYTES {
                return Err(CollectorError::ResidentControlSocket);
            }
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        }
    }
}

impl Drop for ResidentControlSocket {
    fn drop(&mut self) {
        let removable = std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.uid() == rustix::process::getuid().as_raw()
                && metadata.nlink() == 1
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        });
        if removable {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn read_resident_vehicle_action_request(
    socket: &mut UnixStream,
) -> Result<ResidentVehicleActionRequest, ()> {
    let mut request = Vec::new();
    socket
        .take(RESIDENT_CONTROL_REQUEST_BYTES + 1)
        .read_to_end(&mut request)
        .await
        .map_err(|_| ())?;
    if request.len() as u64 > RESIDENT_CONTROL_REQUEST_BYTES {
        return Err(());
    }
    serde_json::from_slice(&request).map_err(|_| ())
}

fn classify_resident_action_error(
    error: &ResidentActionExecutionError,
) -> ResidentVehicleActionFailure {
    match error {
        ResidentActionExecutionError::VehicleMissing => {
            ResidentVehicleActionFailure::VehicleMissing
        }
        ResidentActionExecutionError::Audit(_) => ResidentVehicleActionFailure::AuditUnavailable,
        ResidentActionExecutionError::Authentication => {
            ResidentVehicleActionFailure::AuthenticationRejected
        }
        ResidentActionExecutionError::CompletionUnknown(_) => {
            ResidentVehicleActionFailure::OutcomeAmbiguous
        }
        ResidentActionExecutionError::Provider(OwnerApiAuthError::NotSignedIn)
        | ResidentActionExecutionError::Provider(OwnerApiAuthError::Owner(
            OwnerApiError::HttpStatus(401 | 403),
        )) => ResidentVehicleActionFailure::AuthenticationRejected,
        ResidentActionExecutionError::Provider(_) => ResidentVehicleActionFailure::ProviderRejected,
        ResidentActionExecutionError::FleetCredential(_) => {
            ResidentVehicleActionFailure::AuthenticationRejected
        }
        ResidentActionExecutionError::FleetProvider(FleetApiError::HttpStatus(401 | 403)) => {
            ResidentVehicleActionFailure::AuthenticationRejected
        }
        ResidentActionExecutionError::FleetProvider(_) => {
            ResidentVehicleActionFailure::ProviderRejected
        }
    }
}

async fn setup_native_vehicle_with_client(
    store: &HubStore,
    data_dir: &std::path::Path,
    client: &OwnerApi,
    auth: &LegacyAuth,
    requested_vehicle_id: Option<i64>,
) -> Result<NativeSetupReport, CollectorError> {
    let existing = store.configured_tesla_vehicles()?;
    let effective_vehicle_id =
        requested_vehicle_id.or_else(|| (existing.len() == 1).then(|| existing[0].1));
    let vehicles = client.list_vehicles_with_legacy_auth_once(auth).await?;
    let mut vehicle = select_native_setup_vehicle(vehicles, effective_vehicle_id)?;
    let selected_vehicle_id =
        i64::try_from(vehicle.id.get()).map_err(|_| CollectorError::NativeSetupVehicleIdInvalid)?;

    if let Some((_, _, settings)) = existing
        .into_iter()
        .find(|(_, eid, _)| *eid == selected_vehicle_id)
    {
        vehicle.settings = settings;
    }

    let display_name = vehicle.display_name.clone();
    let cursor_key =
        crate::teslamate_credentials::load_or_create_cursor_key(data_dir).map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let report = finish_collection(
        store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![vehicle],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
    )
    .await?;

    Ok(NativeSetupReport {
        selected_vehicle_id,
        display_name,
        snapshots_published: report.snapshots_published,
    })
}

async fn setup_native_vehicles_with_client(
    store: &HubStore,
    data_dir: &Path,
    client: &OwnerApi,
    auth: &LegacyAuth,
) -> Result<NativeSetupBatchReport, CollectorError> {
    let existing = store.configured_tesla_vehicles()?;
    let mut vehicles = client.list_vehicles_with_legacy_auth_once(auth).await?;
    if vehicles.is_empty() {
        return Err(CollectorError::NativeSetupNoVehicles);
    }
    vehicles.sort_by_key(|vehicle| vehicle.id);
    vehicles.dedup_by_key(|vehicle| vehicle.id);
    for vehicle in &mut vehicles {
        if let Some((_, _, settings)) = existing
            .iter()
            .find(|(_, eid, _)| *eid as u64 == vehicle.id.get())
        {
            vehicle.settings = settings.clone();
        }
    }
    let configured = vehicles
        .iter()
        .map(|vehicle| {
            i64::try_from(vehicle.id.get())
                .map(|vehicle_id| NativeSetupVehicle {
                    vehicle_id,
                    display_name: vehicle.display_name.clone(),
                })
                .map_err(|_| CollectorError::NativeSetupVehicleIdInvalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cursor_key =
        crate::teslamate_credentials::load_or_create_cursor_key(data_dir).map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let report = finish_collection(
        store,
        &cursor_key,
        &ManualCollection {
            vehicles,
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
    )
    .await?;
    Ok(NativeSetupBatchReport {
        vehicles: configured,
        snapshots_published: report.snapshots_published,
    })
}

fn select_native_setup_vehicle(
    mut vehicles: Vec<Vehicle>,
    requested_vehicle_id: Option<i64>,
) -> Result<Vehicle, CollectorError> {
    if vehicles.is_empty() {
        return Err(CollectorError::NativeSetupNoVehicles);
    }
    if let Some(requested_vehicle_id) = requested_vehicle_id {
        let requested = u64::try_from(requested_vehicle_id)
            .ok()
            .filter(|id| *id > 0)
            .ok_or(CollectorError::NativeSetupVehicleIdInvalid)?;
        return vehicles
            .into_iter()
            .find(|vehicle| vehicle.id.get() == requested)
            .ok_or(CollectorError::NativeSetupVehicleNotFound(
                requested_vehicle_id,
            ));
    }
    if vehicles.len() != 1 {
        return Err(CollectorError::NativeSetupVehicleSelectionRequired {
            discovered: vehicles.len(),
        });
    }
    Ok(vehicles.pop().expect("one discovered vehicle"))
}

struct TerrainWorker {
    wake: mpsc::Sender<()>,
    initialized: Option<oneshot::Receiver<Result<(), ()>>>,
    start: Option<oneshot::Sender<()>>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), CollectorError>>,
}

impl TerrainWorker {
    async fn wait_until_initialized(&mut self) -> Result<(), CollectorError> {
        self.initialized
            .take()
            .ok_or(CollectorError::TerrainWorkerStartup)?
            .await
            .map_err(|_| CollectorError::TerrainWorkerStartup)?
            .map_err(|_| CollectorError::TerrainWorkerStartup)
    }

    fn start(&mut self) -> Result<(), CollectorError> {
        self.start
            .take()
            .ok_or(CollectorError::TerrainWorkerTask)?
            .send(())
            .map_err(|_| CollectorError::TerrainWorkerTask)
    }

    async fn wait_until_exit(&mut self) -> Result<(), CollectorError> {
        (&mut self.task)
            .await
            .map_err(|_| CollectorError::TerrainWorkerTask)?
    }

    async fn shutdown(mut self, already_finished: bool) -> Result<(), CollectorError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if already_finished {
            return Ok(());
        }
        self.wait_until_exit().await
    }
}

impl Drop for TerrainWorker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}
