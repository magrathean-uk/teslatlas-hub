// SPDX-License-Identifier: AGPL-3.0-only

async fn run_supervised_with_access<F>(
    store: &HubStore,
    config: &HubConfig,
    cadence: CollectorCadence,
    client: OwnerApi,
    auth: CollectionAuth,
    stream_endpoint: String,
    cursor_key: CursorKey,
    ready: Option<oneshot::Sender<CursorKey>>,
    runtime_admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
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
        runtime_admission.clone(),
    );
    let terrain_wake = terrain_worker.wake.clone();
    let mut startup_result = terrain_worker.wait_until_initialized().await;
    if startup_result.is_ok() && heartbeat_task.is_finished() {
        startup_result = Err(CollectorError::SupervisedHeartbeatTask);
    }
    if startup_result.is_ok() {
        startup_result = assert_runtime_sensitive_access(runtime_admission.as_deref());
    }
    if startup_result.is_ok()
        && let CollectionAuth::Legacy { manager, .. } = &auth
    {
        startup_result = manager
            .lock()
            .await
            .assert_sensitive_access()
            .map_err(CollectorError::from)
            .map_err(normalize_sensitive_access_error);
    }
    if startup_result.is_ok() {
        startup_result = terrain_worker.start();
    }
    if startup_result.is_ok() {
        startup_result = refresh_restored_legacy_auth(&client, &auth).await;
    }
    if startup_result.is_ok() && heartbeat_task.is_finished() {
        startup_result = Err(CollectorError::SupervisedHeartbeatTask);
    }
    if startup_result.is_ok() && terrain_worker.task.is_finished() {
        startup_result = Err(CollectorError::TerrainWorkerTask);
    }
    if startup_result.is_ok() {
        startup_result = assert_runtime_sensitive_access(runtime_admission.as_deref());
    }
    if startup_result.is_ok()
        && let CollectionAuth::Legacy { manager, .. } = &auth
    {
        startup_result = manager
            .lock()
            .await
            .assert_sensitive_access()
            .map_err(CollectorError::from)
            .map_err(normalize_sensitive_access_error);
    }
    let resident_control_authority = match &auth {
        CollectionAuth::Legacy {
            manager,
            fuse,
            refresh,
            allow_refresh: true,
            ..
        } => Some((Arc::clone(manager), Arc::clone(fuse), Arc::clone(refresh))),
        CollectionAuth::Legacy { .. } => None,
    };
    let resident_control_socket = if startup_result.is_ok() && resident_control_authority.is_some()
    {
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
    if startup_result.is_ok()
        && let Some(ready) = ready
    {
        startup_result = ready
            .send(cursor_key.clone())
            .map_err(|_| CollectorError::SupervisedStartupReadyDropped);
    }
    let mut scheduler = VehicleScheduler::new(cadence, Instant::now());
    let mut streams = Vec::new();
    // An Owner API response cannot establish that the Streaming API accepted
    // the same credential. Once a stream rejects authentication, keep the
    // durable collector state terminal until a later authenticated stream
    // handshake proves recovery. A restart reloads an explicitly replaced
    // credential and establishes its own stream-authentication proof.
    let mut stream_authentication_rejected = false;
    let mut logical_legacy_sign_out = false;
    let mut stream_projection_car_ids = HashMap::new();
    let mut next_stream_export_replay = Instant::now();

    let (mut collection_result, heartbeat_finished, terrain_finished) = {
        let resident_control_loop = async {
            match (resident_control_socket, resident_control_authority) {
                (Some(socket), Some((manager, fuse, refresh))) => {
                    socket
                        .serve(store.clone(), client.clone(), manager, fuse, refresh)
                        .await
                }
                _ => std::future::pending::<Result<(), CollectorError>>().await,
            }
        };
        tokio::pin!(resident_control_loop);
        let collection_loop = async {
            startup_result?;
            'collection: loop {
                if logical_legacy_sign_out {
                    // The process remains alive, but the blown legacy fuse
                    // authoritatively fences every later Owner/stream action.
                    sleep(LEGACY_REFRESH_RETRY).await;
                    continue;
                }
                let configured_vehicles = store.configured_tesla_vehicles()?;
                if configured_vehicles.is_empty() {
                    return Err(CollectorError::SelectedVehicleMissing);
                }
                for vehicle_id in
                    scheduler.apply_control_settings(&configured_vehicles, Instant::now())
                {
                    disconnect_vehicle_stream(&mut streams, vehicle_id).await;
                }
                let stream_drain = drain_stream_events_with_cache(
                    store,
                    &mut scheduler,
                    &mut streams,
                    &mut stream_projection_car_ids,
                )
                .await?;
                report_stream_authentication_transition(
                    &collector_state,
                    &mut stream_authentication_rejected,
                    stream_drain.transition,
                );
                refresh_after_stream_authentication_rejection(
                    &client,
                    &auth,
                    stream_drain.transition,
                )
                .await?;
                if let Some(error) = stream_drain.terminal_error {
                    return Err(error);
                }
                if stream_drain.backlog && !scheduler.has_due_stream_fallback(Instant::now()) {
                    // Drain a noisy stream to below the bounded queue before
                    // beginning Owner API, projection, or enrichment work.
                    // A proven stream outage is the exception: its Owner API
                    // fallback must not starve behind the same backlog it is
                    // meant to cover.
                    tokio::task::yield_now().await;
                    continue;
                }
                let now = Instant::now();
                if scheduler.discovery_due(now) {
                    match list_vehicles_for_auth(&client, &auth).await {
                        Ok(vehicles) => {
                            let vehicles =
                                filter_configured_vehicles(vehicles, &configured_vehicles);
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            let events = scheduler.accept_discovery(vehicles, Instant::now());
                            disconnect_streams_not_in_scheduler(&mut streams, &scheduler).await;
                            if !events.is_empty() {
                                persist_discovery_events_with_timeout(
                                    store,
                                    &cursor_key,
                                    &events,
                                    CollectorProvider::Legacy,
                                    cadence.offline_drive_timeout,
                                )
                                .await?;
                            }
                            for vehicle in scheduler.vehicles() {
                                if vehicle.is_online()
                                    && vehicle.settings.enabled
                                    && scheduler.should_start_stream(vehicle.id)
                                {
                                    ensure_vehicle_stream(
                                        store,
                                        &mut streams,
                                        vehicle.id,
                                        vehicle.stream_id,
                                        &auth,
                                        &client,
                                        cadence.stream_health_timeout,
                                        Some(&stream_endpoint),
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            if observer_auth_failure(&auth, &error)
                                || must_stop_supervised_collection(&error)
                            {
                                return Err(error);
                            }
                            report_terminal_auth_failure(&collector_state, &error);
                            if matches!(
                                error,
                                CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
                            ) {
                                logical_legacy_sign_out = true;
                                stop_and_clear_manual_probe_streams(&mut streams).await;
                                continue 'collection;
                            }
                            let now = Instant::now();
                            let delay = scheduler.discovery_failed_for_error(&error, now);
                            tracing::warn!(error = %error, "owner API discovery failed; backing off");
                            sleep(delay).await;
                            continue;
                        }
                    }
                }

                for vehicle_id in scheduler.due_offline_state_vehicles(Instant::now()) {
                    match vehicle_state_for_auth(&client, &auth, vehicle_id).await {
                        Ok(state) => {
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            let events =
                                scheduler.accept_vehicle_state(vehicle_id, state, Instant::now());
                            if !events.is_empty() {
                                persist_discovery_events_with_timeout(
                                    store,
                                    &cursor_key,
                                    &events,
                                    CollectorProvider::Legacy,
                                    cadence.offline_drive_timeout,
                                )
                                .await?;
                            }
                        }
                        Err(error) => {
                            if observer_auth_failure(&auth, &error)
                                || must_stop_supervised_collection(&error)
                            {
                                return Err(error);
                            }
                            report_terminal_auth_failure(&collector_state, &error);
                            if matches!(
                                error,
                                CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
                            ) {
                                logical_legacy_sign_out = true;
                                stop_and_clear_manual_probe_streams(&mut streams).await;
                                continue 'collection;
                            }
                            scheduler.offline_state_failed_for_error(
                                vehicle_id,
                                &error,
                                Instant::now(),
                            );
                            tracing::warn!(vehicle_id = vehicle_id.get(), error = %error, "vehicle state fetch failed; retry scheduled");
                        }
                    }
                }

                let due = scheduler.due_vehicles(Instant::now());
                for vehicle_id in scheduler.due_service_vehicles(Instant::now()) {
                    match vehicle_probe_for_auth(&client, &auth, vehicle_id).await {
                        Ok(true) => {
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            scheduler.service_retry(vehicle_id, Instant::now());
                        }
                        Ok(false) => {
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            scheduler.service_exited(vehicle_id, Instant::now());
                            if let Some(vehicle) = scheduler
                                .vehicles()
                                .into_iter()
                                .find(|vehicle| vehicle.id == vehicle_id)
                                && vehicle.settings.use_streaming_api
                            {
                                ensure_vehicle_stream(
                                    store,
                                    &mut streams,
                                    vehicle_id,
                                    vehicle.stream_id,
                                    &auth,
                                    &client,
                                    cadence.stream_health_timeout,
                                    Some(&stream_endpoint),
                                );
                            }
                        }
                        Err(error) if is_vehicle_in_service(&error) => {
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            scheduler.service_retry(vehicle_id, Instant::now());
                        }
                        Err(error) => {
                            if observer_auth_failure(&auth, &error)
                                || must_stop_supervised_collection(&error)
                            {
                                return Err(error);
                            }
                            report_terminal_auth_failure(&collector_state, &error);
                            if matches!(
                                error,
                                CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
                            ) {
                                logical_legacy_sign_out = true;
                                stop_and_clear_manual_probe_streams(&mut streams).await;
                                continue 'collection;
                            }
                            scheduler.vehicle_failed_for_error(vehicle_id, &error, Instant::now())
                        }
                    }
                }
                if !due.is_empty() {
                    let mut snapshots = Vec::new();
                    let mut failures = Vec::new();
                    let mut scheduler_events = Vec::new();
                    for vehicle_id in due {
                        let power_gate = if scheduler.requires_live_stream_power_gate(vehicle_id) {
                            let Some(power_gate) = streams
                                .iter()
                                .find(|stream| stream.vehicle_id == vehicle_id)
                                .map(|stream| Arc::clone(&stream.power_gate))
                            else {
                                scheduler.vehicle_failed_for_error(
                                    vehicle_id,
                                    &CollectorError::OwnerApi(
                                        OwnerApiError::StreamPowerNotConfirmed,
                                    ),
                                    Instant::now(),
                                );
                                continue;
                            };
                            Some(power_gate)
                        } else {
                            None
                        };
                        match vehicle_data_for_auth(
                            &client,
                            &auth,
                            vehicle_id,
                            power_gate.as_deref(),
                        )
                        .await
                        {
                            Ok(snapshot) => {
                                report_successful_owner_api_request(
                                    &collector_state,
                                    stream_authentication_rejected,
                                );
                                if snapshot_service_mode(&snapshot) == Some(true) {
                                    scheduler.enter_service_mode(vehicle_id, Instant::now());
                                    disconnect_vehicle_stream(&mut streams, vehicle_id).await;
                                }
                                if let Some(vehicle) = scheduler
                                    .vehicles()
                                    .into_iter()
                                    .find(|vehicle| vehicle.id == vehicle_id)
                                    && vehicle.is_online()
                                    && vehicle.settings.enabled
                                    && vehicle.settings.use_streaming_api
                                {
                                    ensure_vehicle_stream(
                                        store,
                                        &mut streams,
                                        vehicle.id,
                                        vehicle.stream_id,
                                        &auth,
                                        &client,
                                        cadence.stream_health_timeout,
                                        Some(&stream_endpoint),
                                    );
                                }
                                let req_not_unlocked = scheduler
                                    .vehicles()
                                    .into_iter()
                                    .find(|vehicle| vehicle.id == vehicle_id)
                                    .is_none_or(|vehicle| vehicle.settings.req_not_unlocked);
                                if let Some(event) = scheduler.vehicle_succeeded(
                                    vehicle_id,
                                    poll_phase(&snapshot),
                                    sleep_eligible_with_policy(&snapshot, req_not_unlocked),
                                    Instant::now(),
                                ) {
                                    scheduler_events.push(event);
                                }
                                snapshots.push(snapshot);
                            }
                            Err(error) if is_vehicle_in_service(&error) => {
                                report_successful_owner_api_request(
                                    &collector_state,
                                    stream_authentication_rejected,
                                );
                                scheduler.enter_service_mode(vehicle_id, Instant::now());
                                disconnect_vehicle_stream(&mut streams, vehicle_id).await;
                                force_close_vehicle_for_service(
                                    store,
                                    vehicle_id,
                                    current_epoch_millis()?,
                                )?;
                            }
                            Err(error) => {
                                if observer_auth_failure(&auth, &error)
                                    || must_stop_supervised_collection(&error)
                                {
                                    return Err(error);
                                }
                                report_terminal_auth_failure(&collector_state, &error);
                                if matches!(
                                    error,
                                    CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
                                ) {
                                    logical_legacy_sign_out = true;
                                    stop_and_clear_manual_probe_streams(&mut streams).await;
                                    continue 'collection;
                                }
                                scheduler.vehicle_failed_for_error(
                                    vehicle_id,
                                    &error,
                                    Instant::now(),
                                );
                                failures.push(VehicleCollectionFailure {
                                    vehicle_id,
                                    error: owner_failure_for_collector_error(error),
                                });
                            }
                        }
                    }
                    let collection = ManualCollection {
                        vehicles: scheduler.vehicles(),
                        snapshots,
                        failures,
                    };
                    let report = finish_collection(store, &cursor_key, &collection).await?;
                    #[cfg(test)]
                    supervised_test_collection_finished(&report).await;
                    let _ = terrain_wake.try_send(());
                    if !scheduler_events.is_empty() {
                        persist_discovery_events_with_timeout(
                            store,
                            &cursor_key,
                            &scheduler_events,
                            CollectorProvider::Legacy,
                            cadence.offline_drive_timeout,
                        )
                        .await?;
                    }
                    tracing::info!(
                        vehicles = report.vehicles_seen,
                        online = report.online_vehicles_seen,
                        inserted = report.observations_inserted,
                        drives = report.drives_closed,
                        charges = report.charges_closed,
                        failures = report.vehicle_failures,
                        "state-aware compatibility collection completed"
                    );
                }

                if stream_drain.backlog {
                    // After the outage fallback has had one chance to persist
                    // a current snapshot, return directly to the stream queue.
                    // Publication and enrichment can wait until the bounded
                    // receiver has caught up.
                    tokio::task::yield_now().await;
                    continue;
                }

                let delay = scheduler.delay_until_next_action(Instant::now());
                let vehicles = scheduler.vehicles();
                assert_runtime_sensitive_access(runtime_admission.as_deref())?;
                run_address_enrichment_once_with_runtime_admission(
                    store,
                    config,
                    &cursor_key,
                    &vehicles,
                    current_epoch_millis()?,
                    runtime_admission.as_ref(),
                )
                .await?;
                let export_now = Instant::now();
                if streams.is_empty() || export_now >= next_stream_export_replay {
                    replay_export_outbox_with_compaction_deferral(
                        store,
                        &cursor_key,
                        &vehicles,
                        current_epoch_millis()?,
                        !streams.is_empty(),
                    )
                    .await?;
                    next_stream_export_replay = export_now + STREAM_EXPORT_REPLAY_INTERVAL;
                }
                if !delay.is_zero() {
                    let cap = collection_sleep_cap(!streams.is_empty());
                    sleep(delay.min(cap)).await;
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), CollectorError>(())
        };
        tokio::pin!(collection_loop);
        let legacy_refresh_failure = wait_for_legacy_refresh_sensitive_failure(&auth);
        tokio::pin!(legacy_refresh_failure);
        tokio::pin!(shutdown);
        tokio::select! {
            biased;
            result = &mut legacy_refresh_failure => (result, false, false),
            result = &mut resident_control_loop => (result, false, false),
            result = &mut collection_loop => (result, false, false),
            () = &mut shutdown => (Ok(()), false, false),
            result = &mut heartbeat_task => {
                let result = result
                    .map_err(|_| CollectorError::SupervisedHeartbeatTask)
                    .and_then(|result| result);
                (result, true, false)
            }
            result = terrain_worker.wait_until_exit() => {
                (result, false, true)
            }
        }
    };

    // Dropping the active collection future stops new work. If it already
    // observed authority loss, close sockets before joining the refresh task.
    // Otherwise join first: a concurrent helper/persistence failure must win
    // over an orderly shutdown so the sender cannot issue an unsubscribe after
    // that authority has been revoked.
    let initial_sensitive_failure = collection_result
        .as_ref()
        .is_err_and(is_sensitive_access_failure);
    if initial_sensitive_failure {
        let error = collection_result
            .as_ref()
            .expect_err("sensitive collection result");
        report_terminal_auth_failure(&collector_state, error);
        abort_and_clear_manual_probe_streams_without_egress(&mut streams).await;
    }
    let refresh_result = wait_for_legacy_refresh_before_owner(&auth).await;
    shutdown_legacy_refresh(&auth).await;
    if let Err(error) = refresh_result {
        collection_result = Err(error);
    }
    if !initial_sensitive_failure {
        if let Err(error) = &collection_result
            && is_sensitive_access_failure(error)
        {
            report_terminal_auth_failure(&collector_state, error);
            abort_and_clear_manual_probe_streams_without_egress(&mut streams).await;
        } else {
            stop_and_clear_manual_probe_streams(&mut streams).await;
        }
    }
    // Stream tasks are joined and the refresh worker is stopped above. Drop the
    // last Owner API pool and credential authority before reporting collector
    // shutdown, so Serve cannot return with an idle outbound connection alive.
    drop(client);
    drop(auth);
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

async fn run_supervised_collector_heartbeat(
    store: HubStore,
    lease: SupervisedCollectorLease,
    mut state: watch::Receiver<SupervisedCollectorState>,
    mut shutdown: oneshot::Receiver<()>,
    interval: Duration,
) -> Result<(), CollectorError> {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                renew_supervised_collector_heartbeat(
                    &store,
                    lease,
                    *state.borrow_and_update(),
                )?;
            }
            changed = state.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                renew_supervised_collector_heartbeat(
                    &store,
                    lease,
                    *state.borrow_and_update(),
                )?;
            }
            _ = &mut shutdown => return Ok(()),
        }
    }
}

fn renew_supervised_collector_heartbeat(
    store: &HubStore,
    lease: SupervisedCollectorLease,
    state: SupervisedCollectorState,
) -> Result<(), CollectorError> {
    match store.heartbeat_supervised_collector_lease(lease, state, current_epoch_millis()?) {
        Ok(()) => Ok(()),
        Err(StoreError::SupervisedCollectorLeaseWrite(error)) => {
            tracing::warn!(%error, "supervised collector heartbeat deferred by local SQLite contention");
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn set_supervised_collector_state(
    sender: &watch::Sender<SupervisedCollectorState>,
    next: SupervisedCollectorState,
) {
    sender.send_if_modified(|current| {
        if *current == next {
            false
        } else {
            *current = next;
            true
        }
    });
}

fn report_terminal_auth_failure(
    state: &watch::Sender<SupervisedCollectorState>,
    error: &CollectorError,
) {
    if is_terminal_auth_failure(error) {
        set_supervised_collector_state(state, SupervisedCollectorState::AuthenticationTerminal);
    }
}

fn report_successful_owner_api_request(
    state: &watch::Sender<SupervisedCollectorState>,
    stream_authentication_rejected: bool,
) {
    if !stream_authentication_rejected {
        set_supervised_collector_state(state, SupervisedCollectorState::Active);
    }
}

fn report_stream_authentication_transition(
    state: &watch::Sender<SupervisedCollectorState>,
    stream_authentication_rejected: &mut bool,
    transition: StreamAuthenticationTransition,
) {
    match transition {
        StreamAuthenticationTransition::NoChange => {}
        StreamAuthenticationTransition::Rejected => {
            *stream_authentication_rejected = true;
            set_supervised_collector_state(state, SupervisedCollectorState::AuthenticationTerminal);
        }
        StreamAuthenticationTransition::Authenticated => {
            *stream_authentication_rejected = false;
            set_supervised_collector_state(state, SupervisedCollectorState::Active);
        }
    }
}

fn is_terminal_auth_failure(error: &CollectorError) -> bool {
    matches!(
        error,
        CollectorError::SensitiveAccessUnavailable
            | CollectorError::OwnerApi(OwnerApiError::HttpStatus(401 | 403))
            | CollectorError::FleetApi(FleetApiError::HttpStatus(401 | 403))
            | CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
            | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(
                403
            )))
    ) || matches!(error, CollectorError::FleetCredential(error) if error.is_sensitive_access_failure())
}

async fn replay_export_outbox(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
) -> Result<usize, CollectorError> {
    replay_export_outbox_with_compaction_deferral(store, cursor_key, vehicles, now_ms, false).await
}

async fn replay_export_outbox_with_compaction_deferral(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
    defer_live_compaction: bool,
) -> Result<usize, CollectorError> {
    let publication_gate = store.acquire_publication_gate().await?;
    let store = store.clone();
    let cursor_key = cursor_key.clone();
    let vehicles = vehicles.to_vec();
    tokio::task::spawn_blocking(move || {
        replay_export_outbox_blocking(
            &store,
            &publication_gate,
            &cursor_key,
            &vehicles,
            now_ms,
            defer_live_compaction,
        )
    })
    .await
    .map_err(|_| CollectorError::ExportPublicationTask)?
}

fn replay_export_outbox_blocking(
    store: &HubStore,
    publication_gate: &crate::db::PublicationGate,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
    defer_live_compaction: bool,
) -> Result<usize, CollectorError> {
    let Some(claim) = store.claim_export_outbox(now_ms)? else {
        return Ok(0);
    };
    if store.vehicle_has_v2_base(claim.vehicle_id)? {
        let pack_count = store.v2_lineage_pack_count(claim.vehicle_id)?;
        if defer_live_compaction
            && live_delta_compaction_required(pack_count, ProtocolLimits::default())
        {
            store.release_export_outbox(&claim)?;
            tracing::info!(
                vehicle_id = %claim.vehicle_id,
                pack_count,
                "deferred derived lineage compaction until active streams stop"
            );
            return Ok(0);
        }
        let Some(sync_claim) = store.claim_sync_mutations(claim.vehicle_id, now_ms, 10_000)? else {
            if store.has_unpublished_sync_mutations(claim.vehicle_id)? {
                store.release_export_outbox(&claim)?;
                return Ok(0);
            }
            store.complete_export_outbox(&claim)?;
            return Ok(0);
        };
        return match publish_v2_delta(store, cursor_key, &sync_claim) {
            Ok(()) => {
                // A single outbox revision can represent more mutations than
                // one bounded claim. Keep it scheduled until every mutation
                // is durably published; otherwise the first 10,000 rows would
                // consume the outbox row and strand the remainder forever.
                if store.has_unpublished_sync_mutations(claim.vehicle_id)? {
                    store.release_export_outbox(&claim)?;
                } else {
                    store.complete_export_outbox(&claim)?;
                }
                Ok(1)
            }
            Err(error) => {
                store.release_sync_mutations(&sync_claim)?;
                store.fail_export_outbox(&claim, "publication_failed", now_ms)?;
                Err(error)
            }
        };
    }
    let Some(source_key) = store.source_vehicle_key(claim.vehicle_id)? else {
        store.fail_export_outbox(&claim, "vehicle_identity_missing", now_ms)?;
        return Ok(0);
    };
    let source_key_number = source_key
        .strip_prefix("eid:")
        .or_else(|| source_key.strip_prefix("vid:"))
        .unwrap_or(&source_key);
    let Ok(source_vehicle_id) = source_key_number.parse::<u64>() else {
        store.fail_export_outbox(&claim, "vehicle_identity_invalid", now_ms)?;
        return Ok(0);
    };
    let Some(vehicle) = vehicles
        .iter()
        .find(|vehicle| vehicle.id.get() == source_vehicle_id)
    else {
        store.fail_export_outbox(&claim, "vehicle_not_discovered", now_ms)?;
        return Ok(0);
    };
    let collection = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![],
        failures: vec![],
    };
    match publish_compatibility_snapshots(store, publication_gate, cursor_key, &collection, now_ms)
    {
        Ok(_) => {
            store.complete_export_outbox(&claim)?;
            Ok(1)
        }
        Err(_) => {
            store.fail_export_outbox(&claim, "publication_failed", now_ms)?;
            Ok(0)
        }
    }
}

fn publish_v2_delta(
    store: &HubStore,
    cursor_key: &CursorKey,
    claim: &crate::db::SyncMutationClaim,
) -> Result<(), CollectorError> {
    compact_v2_lineage_if_needed(store, cursor_key, claim.vehicle_id)?;
    let (base_snapshot_id, head_sequence, parent_digest) = store
        .v2_head(claim.vehicle_id)?
        .ok_or_else(|| CollectorError::Store(StoreError::LineageCatalogConflict))?;
    let from_sequence = u64::try_from(head_sequence)
        .map_err(|_| CollectorError::Store(StoreError::InvalidStoredSequence))?;
    let to_sequence = from_sequence
        .checked_add(
            u64::try_from(claim.mutations.len()).map_err(|_| StoreError::SequenceTooLarge)?,
        )
        .ok_or_else(|| CollectorError::Store(StoreError::SequenceTooLarge))?;
    let binding = store.v2_projection_binding(claim.vehicle_id)?;
    let sequence = SequenceRange {
        from_exclusive: from_sequence,
        to_inclusive: to_sequence,
    };
    let ordinal = store.next_v2_pack_ordinal(base_snapshot_id)?;
    let delta =
        store.projection_delta_for_mutations(claim, binding.clone(), sequence, parent_digest)?;
    let request = ProjectionDeltaPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: base_snapshot_id,
        ordinal,
        delta: &delta,
    };
    let built = ProjectionPackWriter::new(store.packs_dir()).write_delta(&request)?;
    let chain_digest = canonical_delta_chain_digest(parent_digest, built.metadata.sha256);
    let lineage_delta = LineageDelta {
        from_sequence,
        to_sequence,
        parent_chain_digest: parent_digest,
        chain_digest,
        pack_digest: built.metadata.sha256,
        pack: built.metadata,
    };
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: to_sequence,
        },
    )
    .map_err(StoreError::Manifest)?;
    store.commit_v2_delta_claim(claim, &lineage_delta, cursor_key, &terminal_cursor)?;
    Ok(())
}

fn compact_v2_lineage_if_needed(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
) -> Result<(), CollectorError> {
    compact_v2_lineage_if_needed_with_limits(
        store,
        cursor_key,
        vehicle_id,
        ProtocolLimits::default(),
    )
}

fn compact_v2_lineage_if_needed_with_limits(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    limits: ProtocolLimits,
) -> Result<(), CollectorError> {
    let pack_count = store.v2_lineage_pack_count(vehicle_id)?;
    // Preserve enough headroom that a failed or temporarily unavailable
    // compaction does not make an otherwise valid lineage unservable. The
    // hard pre-commit validation remains the final authority at 512.
    if !live_delta_compaction_required(pack_count, limits) {
        return Ok(());
    }
    let Some(plan) = store.plan_live_delta_compaction(vehicle_id)? else {
        if pack_count.saturating_add(1) > limits.max_chunks {
            return Err(StoreError::LineageCapacityExhausted.into());
        }
        return Ok(());
    };
    let binding = store.v2_projection_binding(vehicle_id)?;
    let projection = store.projection_delta_for_compaction(&plan, binding.clone())?;
    let built = match ProjectionPackWriter::new(store.packs_dir()).write_delta(
        &ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: plan.base_snapshot_id,
            ordinal: plan.first_ordinal,
            delta: &projection,
        },
    ) {
        Ok(built) => built,
        Err(error) if may_defer_compaction_capacity_error(&error, pack_count, limits) => {
            tracing::warn!(
                vehicle_id = %vehicle_id,
                pack_count,
                %error,
                "deferred oversized lineage compaction while aggregate slots remain"
            );
            return Ok(());
        }
        Err(error) if is_compaction_pack_capacity_error(&error) => {
            return Err(StoreError::LineageCapacityExhausted.into());
        }
        Err(error) => return Err(error.into()),
    };
    let chain_digest = canonical_delta_chain_digest(plan.anchor_digest, built.metadata.sha256);
    let compacted = LineageDelta {
        from_sequence: plan.anchor_sequence,
        to_sequence: plan.head_sequence,
        parent_chain_digest: plan.anchor_digest,
        chain_digest,
        pack_digest: built.metadata.sha256,
        pack: built.metadata,
    };
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: plan.head_sequence,
        },
    )
    .map_err(StoreError::Manifest)?;
    let replaced = plan.replaced_spans.len();
    match store.commit_live_delta_compaction(&plan, &compacted, cursor_key, &terminal_cursor) {
        Ok(()) => {}
        Err(error)
            if is_compaction_catalog_capacity_error(&error)
                && pack_count.saturating_add(1) <= limits.max_chunks =>
        {
            tracing::warn!(
                vehicle_id = %vehicle_id,
                pack_count,
                %error,
                "deferred aggregate-oversized lineage compaction while a slot remains"
            );
            return Ok(());
        }
        Err(error) if is_compaction_catalog_capacity_error(&error) => {
            return Err(StoreError::LineageCapacityExhausted.into());
        }
        Err(error) => return Err(error.into()),
    }
    tracing::info!(
        vehicle_id = %vehicle_id,
        replaced_delta_packs = replaced,
        compacted_delta_packs = 1,
        "compacted collector-owned V2 lineage suffix"
    );
    Ok(())
}

fn live_delta_compaction_required(pack_count: usize, limits: ProtocolLimits) -> bool {
    let trigger = limits.max_chunks.saturating_sub(limits.max_chunks.min(8));
    pack_count.saturating_add(1) > trigger
}

fn is_compaction_pack_capacity_error(error: &ProjectionPackError) -> bool {
    matches!(
        error,
        ProjectionPackError::TooManyRows
            | ProjectionPackError::Protocol(
                ProtocolError::CompressedSizeOutOfBounds(_)
                    | ProtocolError::UncompressedSizeOutOfBounds(_)
                    | ProtocolError::RowCountOutOfBounds(_)
                    | ProtocolError::PackTooLarge
            )
    )
}

fn may_defer_compaction_capacity_error(
    error: &ProjectionPackError,
    pack_count: usize,
    limits: ProtocolLimits,
) -> bool {
    is_compaction_pack_capacity_error(error) && pack_count.saturating_add(1) <= limits.max_chunks
}

fn is_compaction_catalog_capacity_error(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Manifest(ProtocolError::LineageAggregateLimitExceeded)
    )
}
