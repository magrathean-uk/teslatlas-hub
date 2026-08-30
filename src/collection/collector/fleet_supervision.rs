// SPDX-License-Identifier: AGPL-3.0-only

#[allow(clippy::too_many_arguments)]
async fn run_fleet_supervised_with_access<F>(
    store: &HubStore,
    config: &HubConfig,
    cadence: CollectorCadence,
    api: FleetApi,
    auth_api: FleetAuthApi,
    command_proxy: Option<FleetCommandProxy>,
    manager: Arc<tokio::sync::Mutex<FleetAuthManager>>,
    cursor_key: CursorKey,
    ready: oneshot::Sender<CursorKey>,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    allow_refresh: bool,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    if store.configured_tesla_vehicles()?.is_empty() {
        return Err(CollectorError::SelectedVehicleMissing);
    }
    let collector_lease = store.acquire_supervised_collector_lease(current_epoch_millis()?)?;
    let (collector_state, collector_state_rx) = watch::channel(SupervisedCollectorState::Active);
    let (heartbeat_shutdown, heartbeat_stop) = oneshot::channel();
    let mut heartbeat_task = tokio::spawn(run_supervised_collector_heartbeat(
        store.clone(),
        collector_lease,
        collector_state_rx,
        heartbeat_stop,
        SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
    ));
    let mut terrain_worker = spawn_terrain_worker(
        config.data_dir.clone(),
        config.terrain.clone(),
        cursor_key.clone(),
        Some(Arc::clone(&admission)),
    );
    let terrain_wake = terrain_worker.wake.clone();
    let mut startup_result = terrain_worker.wait_until_initialized().await;
    if startup_result.is_ok() {
        startup_result = admission
            .assert_sensitive_access()
            .map_err(CollectorError::from);
    }
    if startup_result.is_ok() && allow_refresh {
        startup_result = manager
            .lock()
            .await
            .refresh_if_due(&auth_api, SystemTime::now())
            .await
            .map_err(Into::into);
    }
    if startup_result.is_ok() {
        startup_result = terrain_worker.start();
    }
    if startup_result.is_ok() && (heartbeat_task.is_finished() || terrain_worker.task.is_finished())
    {
        startup_result = Err(CollectorError::SupervisedHeartbeatTask);
    }
    let resident_socket = if startup_result.is_ok() && allow_refresh {
        match ResidentControlSocket::bind(&config.data_dir) {
            Ok(socket) => Some(socket),
            Err(error) => {
                startup_result = Err(error);
                None
            }
        }
    } else {
        None
    };
    if startup_result.is_ok() {
        startup_result = ready
            .send(cursor_key.clone())
            .map_err(|_| CollectorError::SupervisedStartupReadyDropped);
    }
    let mut scheduler = VehicleScheduler::new(cadence, Instant::now());
    let (mut collection_result, heartbeat_finished, terrain_finished) = {
        let resident_control_loop = async {
            match resident_socket {
                Some(socket) => {
                    socket
                        .serve_fleet(
                            store.clone(),
                            api.clone(),
                            auth_api.clone(),
                            command_proxy.clone(),
                            Arc::clone(&manager),
                        )
                        .await
                }
                None => std::future::pending::<Result<(), CollectorError>>().await,
            }
        };
        tokio::pin!(resident_control_loop);
        let collection_loop = async {
            startup_result?;
            if config.collector.fleet_telemetry.is_some() {
                return run_fleet_telemetry_maintenance_loop(
                    store,
                    config,
                    &auth_api,
                    &manager,
                    &cursor_key,
                    &terrain_wake,
                    &admission,
                    allow_refresh,
                )
                .await;
            }
            loop {
                admission.assert_sensitive_access()?;
                let configured = store.configured_tesla_vehicles()?;
                if configured.is_empty() {
                    return Err(CollectorError::SelectedVehicleMissing);
                }
                scheduler.apply_control_settings(&configured, Instant::now());
                let now = Instant::now();
                if scheduler.discovery_due(now) {
                    match fleet_list_vehicles_with_auth(&api, &auth_api, &manager, allow_refresh)
                        .await
                    {
                        Ok(vehicles) => {
                            let vehicles = filter_configured_vehicles_for_provider(
                                vehicles,
                                &configured,
                                CollectorProvider::Fleet,
                            );
                            report_successful_owner_api_request(&collector_state, false);
                            let events = scheduler.accept_discovery(vehicles, Instant::now());
                            if !events.is_empty() {
                                persist_discovery_events_with_timeout(
                                    store,
                                    &cursor_key,
                                    &events,
                                    CollectorProvider::Fleet,
                                    cadence.offline_drive_timeout,
                                )
                                .await?;
                            }
                        }
                        Err(error) => {
                            report_terminal_auth_failure(&collector_state, &error);
                            if fleet_collection_must_stop(&error) {
                                return Err(error);
                            }
                            let delay =
                                scheduler.discovery_failed_for_error(&error, Instant::now());
                            tracing::warn!(error = %error, "Fleet discovery failed; backing off");
                            sleep(delay).await;
                            continue;
                        }
                    }
                }

                let offline_due = scheduler.due_offline_state_vehicles(Instant::now());
                if !offline_due.is_empty() {
                    match fleet_list_vehicles_with_auth(&api, &auth_api, &manager, allow_refresh)
                        .await
                    {
                        Ok(discovered) => {
                            let discovered = filter_configured_vehicles_for_provider(
                                discovered,
                                &configured,
                                CollectorProvider::Fleet,
                            );
                            let mut events = Vec::new();
                            for vehicle_id in offline_due {
                                if let Some(vehicle) =
                                    discovered.iter().find(|vehicle| vehicle.id == vehicle_id)
                                {
                                    events.extend(scheduler.accept_vehicle_state(
                                        vehicle_id,
                                        vehicle.state.clone(),
                                        Instant::now(),
                                    ));
                                }
                            }
                            if !events.is_empty() {
                                persist_discovery_events_with_timeout(
                                    store,
                                    &cursor_key,
                                    &events,
                                    CollectorProvider::Fleet,
                                    cadence.offline_drive_timeout,
                                )
                                .await?;
                            }
                        }
                        Err(error) => {
                            if fleet_collection_must_stop(&error) {
                                return Err(error);
                            }
                            for vehicle_id in offline_due {
                                scheduler.vehicle_failed_for_error(
                                    vehicle_id,
                                    &error,
                                    Instant::now(),
                                );
                            }
                        }
                    }
                }

                for vehicle_id in scheduler.due_service_vehicles(Instant::now()) {
                    let Some(vehicle) = scheduler
                        .vehicles()
                        .into_iter()
                        .find(|vehicle| vehicle.id == vehicle_id)
                    else {
                        continue;
                    };
                    match fleet_vehicle_data_with_auth(
                        &api,
                        &auth_api,
                        &manager,
                        &vehicle,
                        allow_refresh,
                    )
                    .await
                    {
                        Ok(snapshot) if snapshot_service_mode(&snapshot) == Some(true) => {
                            scheduler.service_retry(vehicle_id, Instant::now());
                        }
                        Ok(_) => scheduler.service_exited(vehicle_id, Instant::now()),
                        Err(error) => {
                            if fleet_collection_must_stop(&error) {
                                return Err(error);
                            }
                            scheduler.vehicle_failed_for_error(vehicle_id, &error, Instant::now());
                        }
                    }
                }

                let due = scheduler.due_vehicles(Instant::now());
                if !due.is_empty() {
                    let mut snapshots = Vec::new();
                    let mut failures = Vec::new();
                    let mut scheduler_events = Vec::new();
                    for vehicle_id in due {
                        let Some(vehicle) = scheduler
                            .vehicles()
                            .into_iter()
                            .find(|vehicle| vehicle.id == vehicle_id)
                        else {
                            continue;
                        };
                        match fleet_vehicle_data_with_auth(
                            &api,
                            &auth_api,
                            &manager,
                            &vehicle,
                            allow_refresh,
                        )
                        .await
                        {
                            Ok(snapshot) => {
                                report_successful_owner_api_request(&collector_state, false);
                                if snapshot_service_mode(&snapshot) == Some(true) {
                                    scheduler.enter_service_mode(vehicle_id, Instant::now());
                                    force_close_vehicle_for_service_provider(
                                        store,
                                        vehicle_id,
                                        current_epoch_millis()?,
                                        CollectorProvider::Fleet,
                                    )?;
                                }
                                if let Some(event) = scheduler.vehicle_succeeded(
                                    vehicle_id,
                                    poll_phase(&snapshot),
                                    sleep_eligible_with_policy(
                                        &snapshot,
                                        vehicle.settings.req_not_unlocked,
                                    ),
                                    Instant::now(),
                                ) {
                                    scheduler_events.push(event);
                                }
                                snapshots.push(snapshot);
                            }
                            Err(error) => {
                                report_terminal_auth_failure(&collector_state, &error);
                                if fleet_collection_must_stop(&error) {
                                    return Err(error);
                                }
                                scheduler.vehicle_failed_for_error(
                                    vehicle_id,
                                    &error,
                                    Instant::now(),
                                );
                                failures.push(VehicleCollectionFailure {
                                    vehicle_id,
                                    error: fleet_failure_as_owner_error(&error),
                                });
                            }
                        }
                    }
                    let collection = ManualCollection {
                        vehicles: scheduler.vehicles(),
                        snapshots,
                        failures,
                    };
                    let _report = finish_collection_for_provider(
                        store,
                        &cursor_key,
                        &collection,
                        CollectorProvider::Fleet,
                    )
                    .await?;
                    #[cfg(test)]
                    supervised_test_collection_finished(&_report).await;
                    let _ = terrain_wake.try_send(());
                    if !scheduler_events.is_empty() {
                        persist_discovery_events_with_timeout(
                            store,
                            &cursor_key,
                            &scheduler_events,
                            CollectorProvider::Fleet,
                            cadence.offline_drive_timeout,
                        )
                        .await?;
                    }
                }
                let delay = scheduler.delay_until_next_action(Instant::now());
                let vehicles = scheduler.vehicles();
                run_address_enrichment_once_with_runtime_admission(
                    store,
                    config,
                    &cursor_key,
                    &vehicles,
                    current_epoch_millis()?,
                    Some(&admission),
                )
                .await?;
                replay_export_outbox(store, &cursor_key, &vehicles, current_epoch_millis()?)
                    .await?;
                if !delay.is_zero() {
                    sleep(delay.min(CONTROL_SETTINGS_REFRESH)).await;
                }
            }
        };
        tokio::pin!(collection_loop);
        tokio::pin!(shutdown);
        tokio::select! {
            biased;
            result = &mut resident_control_loop => (result, false, false),
            result = &mut collection_loop => (result, false, false),
            () = &mut shutdown => (Ok(()), false, false),
            result = &mut heartbeat_task => {
                let result = result
                    .map_err(|_| CollectorError::SupervisedHeartbeatTask)
                    .and_then(|result| result);
                (result, true, false)
            }
            result = terrain_worker.wait_until_exit() => (result, false, true),
        }
    };
    if let Err(error) = &collection_result {
        report_terminal_auth_failure(&collector_state, error);
    }
    // The collection and resident-control futures have been dropped. Release
    // every Fleet HTTP pool/proxy handle before the worker can report stopped.
    drop(command_proxy);
    drop(auth_api);
    drop(api);
    let terrain_result = terrain_worker.shutdown(terrain_finished).await;
    if collection_result.is_ok() {
        collection_result = terrain_result;
    }
    if !heartbeat_finished {
        let _ = heartbeat_shutdown.send(());
        let heartbeat_result = heartbeat_task
            .await
            .map_err(|_| CollectorError::SupervisedHeartbeatTask)
            .and_then(|result| result);
        if collection_result.is_ok() {
            collection_result = heartbeat_result;
        }
    }
    let release_result = store
        .release_supervised_collector_lease(collector_lease)
        .map_err(CollectorError::from);
    if collection_result.is_ok() {
        collection_result = release_result;
    }
    collection_result
}

/// Keep Fleet credentials, deferred publication, and local enrichment healthy
/// while native Fleet Telemetry supplies observations. This mode deliberately
/// makes no Fleet list or vehicle-data requests and has no polling fallback.
#[allow(clippy::too_many_arguments)]
async fn run_fleet_telemetry_maintenance_loop(
    store: &HubStore,
    config: &HubConfig,
    auth_api: &FleetAuthApi,
    manager: &Arc<tokio::sync::Mutex<FleetAuthManager>>,
    cursor_key: &CursorKey,
    terrain_wake: &mpsc::Sender<()>,
    admission: &Arc<crate::hub_user_process::AdmittedUserHub>,
    allow_refresh: bool,
) -> Result<(), CollectorError> {
    const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
    tracing::info!("Fleet Telemetry push collection active; paid vehicle-data polling is disabled");
    let mut next_configuration_renewal = Instant::now();
    loop {
        admission.assert_sensitive_access()?;
        let vehicles = configured_fleet_telemetry_vehicles(store)?;
        if allow_refresh && Instant::now() >= next_configuration_renewal {
            let result = {
                let mut manager = manager.lock().await;
                apply_fleet_telemetry_configuration(
                    store,
                    config,
                    &mut manager,
                    auth_api,
                    admission,
                )
                .await
            };
            let retry_after = match result {
                Ok(report) if report.vehicles_skipped == 0 => {
                    tracing::info!(
                        vehicles_configured = report.vehicles_configured,
                        vehicles_revoked = report.vehicles_revoked,
                        expires_at = report.expires_at,
                        "Fleet Telemetry configuration renewed"
                    );
                    FLEET_TELEMETRY_CONFIG_RENEWAL_INTERVAL
                }
                Ok(report) => {
                    tracing::warn!(
                        vehicles_configured = report.vehicles_configured,
                        vehicles_skipped = report.vehicles_skipped,
                        vehicles_revoked = report.vehicles_revoked,
                        "Fleet Telemetry configuration skipped vehicles; retrying"
                    );
                    FLEET_TELEMETRY_CONFIG_RETRY_INTERVAL
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Fleet Telemetry configuration renewal failed; existing push collection remains active");
                    FLEET_TELEMETRY_CONFIG_RETRY_INTERVAL
                }
            };
            next_configuration_renewal = Instant::now() + retry_after;
        }
        if allow_refresh {
            let refresh = manager
                .lock()
                .await
                .refresh_if_due(auth_api, SystemTime::now())
                .await;
            if let Err(error) = refresh {
                if let Some(delay) = fleet_refresh_retry_delay(&error) {
                    tracing::warn!(
                        retry_seconds = delay.as_secs(),
                        "Fleet token refresh request was not sent; push collection remains active"
                    );
                    sleep(delay).await;
                    continue;
                }
                return Err(error.into());
            }
        }
        if vehicles.is_empty() {
            sleep(MAINTENANCE_INTERVAL).await;
            continue;
        }
        let now_ms = current_epoch_millis()?;
        run_address_enrichment_once_with_runtime_admission(
            store,
            config,
            cursor_key,
            &vehicles,
            now_ms,
            Some(admission),
        )
        .await?;
        replay_export_outbox(store, cursor_key, &vehicles, now_ms).await?;
        let _ = terrain_wake.try_send(());
        sleep(MAINTENANCE_INTERVAL).await;
    }
}

fn fleet_refresh_retry_delay(error: &FleetCredentialError) -> Option<Duration> {
    matches!(
        error,
        FleetCredentialError::Api(FleetApiError::RequestNotSent)
    )
    .then_some(FLEET_REFRESH_REQUEST_NOT_SENT_RETRY)
}

fn configured_fleet_telemetry_vehicles(store: &HubStore) -> Result<Vec<Vehicle>, CollectorError> {
    let mut vehicles = Vec::new();
    for (vehicle_id, eid, settings) in store.configured_tesla_vehicles()? {
        if !settings.enabled {
            continue;
        }
        let Some((identity_eid, vin)) = store.configured_tesla_vehicle_identity(vehicle_id)? else {
            return Err(CollectorError::SelectedVehicleMissing);
        };
        if identity_eid != eid {
            return Err(CollectorError::SelectedVehicleMissing);
        }
        let vin = vin.ok_or(CollectorError::SelectedVehicleMissing)?;
        let id = VehicleId::try_from_i64(eid).ok_or(CollectorError::SelectedVehicleMissing)?;
        let stream_id = crate::owner_api::StreamVehicleId::try_from_i64(eid)
            .ok_or(CollectorError::SelectedVehicleMissing)?;
        let materialised = store.materialised_car_for_vehicle(vehicle_id)?;
        vehicles.push(Vehicle {
            id,
            stream_id,
            vin,
            state: "online".to_owned(),
            display_name: materialised.map(|car| car.name),
            settings,
        });
    }
    Ok(vehicles)
}

fn owner_failure_for_collector_error(error: CollectorError) -> OwnerApiError {
    match error {
        CollectorError::OwnerApi(error)
        | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(error)) => error,
        CollectorError::OwnerApiAuth(_) => OwnerApiError::LegacyAuth,
        _ => OwnerApiError::Transport,
    }
}

fn filter_configured_vehicles(
    vehicles: Vec<Vehicle>,
    configured: &[(uuid::Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
) -> Vec<Vehicle> {
    filter_configured_vehicles_for_provider(vehicles, configured, CollectorProvider::Legacy)
}

fn filter_configured_vehicles_for_provider(
    vehicles: Vec<Vehicle>,
    configured: &[(uuid::Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
    provider: CollectorProvider,
) -> Vec<Vehicle> {
    vehicles
        .into_iter()
        .filter_map(|mut vehicle| {
            configured
                .iter()
                .find(|(_, eid, _)| vehicle.id.get() == *eid as u64)
                .map(|(_, _, settings)| {
                    vehicle.settings = settings.clone();
                    if provider == CollectorProvider::Fleet {
                        vehicle.settings.use_streaming_api = false;
                    }
                    vehicle
                })
        })
        .collect()
}
