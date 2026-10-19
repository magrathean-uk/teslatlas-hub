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
    write_guard: RunWriteGuard,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    // The run's write guard exists before the credential unit and the cursor key (B2
    // fix round, P3b fix 3, R-P3bF3-1): the entry point made it, and it has been
    // consulted at every write since. The renewal revalidates the retained admission
    // before every write (R47, B2 clarification 3) and, after a denial, writes nothing
    // more, and every store mutation of the run is preceded by the same guard.
    let runtime_admission = write_guard.admission.clone();
    let admission_fence = write_guard.fence.clone();
    // The credential unit shares the guard (B2 fix round, P3b fix 1, R-P3bF-1; fix 2):
    // every step by which it persists or writes refuses once the supervisor has
    // observed the loss, or the retained admission is gone now, even when the
    // admission passes again. It is shared before the start application below, which
    // may write the account's status row.
    let CollectionAuth::Legacy { manager, refresh, .. } = &auth;
    manager
        .lock()
        .await
        .share_loss_fence(write_guard.observer("credential"));
    require_supervised_vehicle_lineage(store)?;
    // The previous run's last heartbeat is the start of its downtime gap (C3 R8),
    // and this run's lease replaces it, so read it first.
    let previous_heartbeat_ms = store
        .supervised_collector_lease_status()?
        .map(|lease| lease.heartbeat_at_ms);
    write_guard.check("start-lease")?;
    let collector_lease = store.acquire_supervised_collector_lease(current_epoch_millis()?)?;
    // C3 R7 step 1 and R8: reset a stored refusal when the mode or the
    // configuration changed, and close or record the downtime gap, before any
    // other start step.
    write_guard.check("start-run")?;
    let collector_start = start_collector_run(
        store,
        config,
        &cadence,
        &stream_endpoint,
        previous_heartbeat_ms,
    )?;
    // C5 R57 step 6 and the B2 clarification: the credential unit was built before
    // this start path ran, so it is told here whether C3 reset a refusal and what
    // its account row holds now.
    {
        let mut manager = manager.lock().await;
        // C5 R33a: the unit's shutdown flag is held outside its lock, so the run can
        // raise it the moment its shutdown begins.
        refresh.register_shutdown(manager.shutdown_flag());
        manager.apply_collector_start(&collector_start.credential_view());
    }
    let (collector_state, collector_state_rx) = watch::channel(SupervisedCollectorState::Active);
    let (heartbeat_shutdown, heartbeat_stop) = oneshot::channel();
    let mut heartbeat_task = tokio::spawn(supervised_test_scoped(
        run_supervised_collector_heartbeat_with(
            store.clone(),
            collector_lease,
            collector_state_rx,
            heartbeat_stop,
            SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
            Arc::new(SystemCollectorClock),
            runtime_admission.clone(),
            admission_fence.clone(),
        ),
    ));
    let mut terrain_worker = spawn_terrain_worker(
        config.data_dir.clone(),
        config.terrain.clone(),
        cursor_key.clone(),
        write_guard.clone(),
        store.clone(),
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
    // C5 R26: no token request is sent at start. An imported pair is refreshed
    // when the first Owner API request or stream admission needs a token, so a
    // collector that C3 has stopped sends none at all.
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
    let mut scheduler =
        VehicleScheduler::new_for_mode(cadence, collector_start.mode, Instant::now());
    scheduler.guard_writes_with(write_guard.clone());
    // C3 R2: every read of time in the loop goes through the scheduler's clock.
    let clock = Arc::clone(&scheduler.clock);
    scheduler.load(&collector_start.snapshot, clock.mono_now());
    // C3 R25: the Owner-wire requests are counted by category, never billed.
    let ledger = scheduler.request_ledger();
    // C3 R28: Legacy has the Tesla stream; Provider has a stream only when the
    // owner configured its URL (a provider must not be sent to Tesla's host);
    // Fleet polling has none and never reaches this loop.
    scheduler.set_stream_delivery(
        collector_start.mode == crate::config::CollectorMode::Legacy
            || config.collector.stream_endpoint_override.is_some(),
    );
    let mut identities = IdentityCache::default();
    let mut next_status_refresh = clock.mono_now();
    // The owner's unapplied actions, read at each settings refresh and applied in
    // the next tick (C3 R65).
    let mut owner_actions: Vec<OwnerActionRow> = Vec::new();
    let mut streams = Vec::new();
    // What the drains made while Owner API or credential work waited found, settled
    // at the loop's next drain (C3 R11, C4 R56 g).
    let mut pending_drain = StreamDrainResult::default();
    // The previous drain's instant, shared by every wait of the loop (C3 R2, R11).
    let mut drain_budget = DrainBudget::new(Instant::now());
    // Every drain of the run writes its frames only while the guard allows.
    drain_budget.guard_writes_with(write_guard.clone());
    // C5 R43: the account is stopped for good until the owner acts (a sign-out
    // or a configuration error); the loop then sends nothing. A stream rejection
    // is never a stop by itself: the credential unit confirms it first (R40).
    let mut logical_legacy_sign_out = false;
    let mut stream_projection_car_ids = HashMap::new();
    let mut next_stream_export_replay = Instant::now();

    let (mut collection_result, heartbeat_finished, terrain_finished) = {
        let resident_control_loop = async {
            match (resident_control_socket, resident_control_authority) {
                (Some(socket), Some((manager, fuse, refresh))) => {
                    socket
                        .serve_guarded(
                            store.clone(),
                            client.clone(),
                            manager,
                            fuse,
                            refresh,
                            Some(write_guard.clone()),
                        )
                        .await
                }
                _ => std::future::pending::<Result<(), CollectorError>>().await,
            }
        };
        tokio::pin!(resident_control_loop);
        let collection_loop = async {
            startup_result?;
            'collection: loop {
                supervised_loop_iteration();
                // The test seam may hold the loop here, pending, so that another arm of
                // the supervisor's wait is the one that is ready.
                supervised_test_gate("iteration-top").await;
                let iteration_started = loop_now();
                // C5 R28, R39, R44a: a stopped or waiting credential unit reads the
                // stored generation every 60 s, and a unit with nothing to hand out
                // yet is asked to refresh now.
                // The refresh it may start can take tens of seconds; the stream
                // queues are drained meanwhile (C3 R11, C4 R56 g).
                let confirmations = StreamPump {
                    store,
                    streams: &mut streams,
                    projection_car_ids: &mut stream_projection_car_ids,
                    pending: &mut pending_drain,
                    budget: &mut drain_budget,
                }
                .while_waiting(&mut scheduler, async {
                    maintain_legacy_credential(&client, &auth).await;
                    // The confirmation count and answers are taken once, here, where
                    // waiting for the fuse is pumped too (a confirmation holds it for
                    // tens of seconds), and applied synchronously below.
                    take_confirmation_outcomes(&auth).await
                })
                .await?;
                count_confirmation_reads(confirmations.reads, &ledger);
                apply_confirmation_answers(
                    confirmations.answers,
                    &mut scheduler,
                    clock.mono_now(),
                );
                // C5 R43, R47, C3 R47, R48: the unit's stop or refusal, reached
                // outside any request, is the account's state within one iteration,
                // and a lifted stop collects again.
                let credential_stopped = apply_credential_state(
                    &auth,
                    &mut scheduler,
                    &collector_state,
                    clock.mono_now(),
                );
                if credential_stopped && !logical_legacy_sign_out {
                    stop_all_streams_and_store(
                        store,
                        &mut scheduler,
                        &mut streams,
                        &mut stream_projection_car_ids,
                        &mut pending_drain,
                        &mut drain_budget,
                    )
                    .await?;
                }
                logical_legacy_sign_out = credential_stopped;
                if logical_legacy_sign_out {
                    // The process remains alive, and the credential unit
                    // authoritatively fences every later Owner/stream action. The
                    // account is in `CredentialStop` (C3 R47), which sends nothing
                    // but still goes round the loop's own work.
                    let wait = run_credential_stop_iteration(
                        store,
                        &mut scheduler,
                        &mut identities,
                        &cursor_key,
                        &terrain_wake,
                        cadence.offline_drive_timeout,
                        &mut next_status_refresh,
                        &mut owner_actions,
                        &auth,
                    )
                    .await?;
                    sleep(wait).await;
                    continue;
                }
                let configured_vehicles = store.configured_tesla_vehicles()?;
                let settings_stops =
                    scheduler.apply_control_settings(&configured_vehicles, clock.mono_now());
                stop_vehicle_streams(
                    store,
                    &mut scheduler,
                    &mut streams,
                    &settings_stops,
                    &mut stream_projection_car_ids,
                    &mut pending_drain,
                    &mut drain_budget,
                )
                .await?;
                let mut stream_drain = drain_streams_now(
                    &mut drain_budget,
                    store,
                    &mut scheduler,
                    &mut streams,
                    &mut stream_projection_car_ids,
                )
                .await?;
                // What the drains made during the waits since the last settling
                // found (a rejection, a run that ended, a terminal error).
                stream_drain.absorb(std::mem::take(&mut pending_drain));
                // C3 R28, R36: a run that ended with an error leaves its runtime
                // behind until the loop removes it (its next run is spaced by the
                // scheduler), and the gates the drain's events asked to revoke are
                // revoked before anything is sent.
                for vehicle_id in &stream_drain.ended {
                    stop_vehicle_stream(
                        store,
                        &mut scheduler,
                        &mut streams,
                        *vehicle_id,
                        &mut stream_projection_car_ids,
                    )
                    .await?;
                }
                apply_gate_revocations(&mut scheduler, &streams);
                reconcile_streams(
                    store,
                    &mut scheduler,
                    &mut streams,
                    &auth,
                    &client,
                    cadence.stream_health_timeout,
                    &stream_endpoint,
                    &mut stream_projection_car_ids,
                    &mut pending_drain,
                    &mut drain_budget,
                )
                .await?;
                StreamPump {
                    store,
                    streams: &mut streams,
                    projection_car_ids: &mut stream_projection_car_ids,
                    pending: &mut pending_drain,
                    budget: &mut drain_budget,
                }
                .while_waiting(
                    &mut scheduler,
                    refresh_after_stream_authentication_rejection(
                        &client,
                        &auth,
                        stream_drain.transition,
                    ),
                )
                .await??;
                if let Some(error) = stream_drain.terminal_error {
                    return Err(error);
                }
                if stream_drain.backlog && !scheduler.has_due_stream_fallback(clock.mono_now()) {
                    // Drain a noisy stream to below the bounded queue before
                    // beginning Owner API, projection, or enrichment work.
                    // A proven stream outage is the exception: its Owner API
                    // fallback must not starve behind the same backlog it is
                    // meant to cover.
                    tokio::task::yield_now().await;
                    continue;
                }
                // C3 R2: a host sleep is recorded as a gap and a backward wall step
                // is logged once, before this iteration plans anything.
                scheduler.observe_clock(clock.mono_now());
                refresh_status_inputs(
                    store,
                    &mut scheduler,
                    &mut next_status_refresh,
                    &mut owner_actions,
                );

                // One tick (C3 R5): the list, the targeted checks and the reads of
                // this iteration, committed in one transaction.
                let tick_now = clock.mono_now();
                scheduler.begin_tick();
                // C3 R65: the owner's actions apply inside the tick, so the
                // transaction that stores their effects also marks them applied.
                if !owner_actions.is_empty() {
                    if owner_actions
                        .iter()
                        .any(|action| action.kind == OwnerActionKind::RetryCollection)
                    {
                        // A retry may start a refresh; the queues are drained
                        // meanwhile (C3 R11, C4 R56 g).
                        StreamPump {
                            store,
                            streams: &mut streams,
                            projection_car_ids: &mut stream_projection_car_ids,
                            pending: &mut pending_drain,
                            budget: &mut drain_budget,
                        }
                        .while_waiting(&mut scheduler, retry_legacy_credential(&auth))
                        .await?;
                    }
                    scheduler.apply_owner_actions(&owner_actions, tick_now);
                    owner_actions.clear();
                }
                let mut requests_sent = false;
                let mut signed_out = false;
                let tick_wrote: bool;
                let mut disconnect_after: Vec<VehicleId> = Vec::new();
                {
                    let mut run = TickRun {
                        scheduler: &mut scheduler,
                        identities: &mut identities,
                        commit: TickCommit {
                            store,
                            cursor_key: &cursor_key,
                            provider: CollectorProvider::Legacy,
                            offline_drive_timeout: cadence.offline_drive_timeout,
                        },
                        batch: TickBatch::default(),
                        terrain_wake: &terrain_wake,
                        any_wrote: false,
                    };
                    // C3 R40: a drive whose car has been silent for more than `T_gap`
                    // is closed by a Hub event in this tick's transaction, whatever
                    // the car's phase, hold, account state or wait.
                    plan_no_data_events(store, run.scheduler, &mut run.batch)?;
                    if run.scheduler.discovery_due(tick_now) {
                        requests_sent = true;
                        // C3 R48 (c): a plain 403 on the list asks for one refresh and
                        // resends the list once; a 403 to the resent list refuses.
                        let owner_wire = run.scheduler.mode.is_owner_wire();
                        let list_log = RequestLog::for_account(run.scheduler);
                        let list_started = list_log.clock.mono_now();
                        // C3 R11, C4 R56 g: the stream queues are drained while the
                        // list (and the refresh it may ask for) is pending.
                        let checked = list_with_stream_drain(
                            StreamPump {
                                store,
                                streams: &mut streams,
                                projection_car_ids: &mut stream_projection_car_ids,
                                pending: &mut pending_drain,
                                budget: &mut drain_budget,
                            },
                            &mut *run.scheduler,
                            &client,
                            &auth,
                            owner_wire,
                            &ledger,
                        )
                        .await?;
                        list_log.emit("list", RequestKind::List, list_started, &checked.result);
                        let refused_after_check = checked.refused_after_check;
                        let listed = accept_scheduled_provider_inventory_guarded(
                            store,
                            CollectorProvider::Legacy,
                            checked.result,
                            run.scheduler.write_guard(),
                        );
                        match listed {
                            Ok(vehicles) => {
                                let vehicles = filter_configured_vehicles_with_identity(
                                    store,
                                    vehicles,
                                    &configured_vehicles,
                                    CollectorProvider::Legacy,
                                )?;
                                report_successful_owner_api_request(&collector_state);
                                let list_time = clock.mono_now();
                                let received_at_ms = clock.wall_now_ms()?;
                                let candidates =
                                    run.scheduler.accept_discovery(vehicles, list_time);
                                // Name the cars the list introduced, so their rows
                                // can be written in this tick.
                                disconnect_after.extend(
                                    run.scheduler
                                        .apply_control_settings(&configured_vehicles, list_time),
                                );
                                run.batch.discoveries.extend(candidates.into_iter().map(
                                    |vehicle| DiscoveryItem {
                                        vehicle,
                                        received_at_ms,
                                    },
                                ));
                                run.batch.note_result(list_time);
                            }
                            Err(error) => {
                                if loses_sensitive_access(&error) {
                                    // The process is no longer the admitted Hub
                                    // instance (C3 R47, B2 clarification): not an
                                    // account state. The run ends at once, with no
                                    // stop, no `auth_terminal` gap and no further
                                    // collector write; the next admitted start
                                    // records the downtime (R8).
                                    report_terminal_auth_failure(&collector_state, &error);
                                    return Err(error);
                                }
                                report_terminal_auth_failure(&collector_state, &error);
                                if is_credential_stop(&error) {
                                    let stopped_at = run.scheduler.clock.mono_now();
                                    run.scheduler.enter_credential_stop(stopped_at);
                                    signed_out = true;
                                } else {
                                    let delay = if refused_after_check {
                                        run.scheduler.list_refused_after_check(clock.mono_now())
                                    } else {
                                        run.scheduler
                                            .discovery_failed_for_error(&error, clock.mono_now())
                                    };
                                    tracing::warn!(
                                        retry_seconds = delay.as_secs(),
                                        "Owner API discovery failed; backing off"
                                    );
                                }
                            }
                        }
                    }
                    if !signed_out {
                        let work_now = clock.mono_now();
                        let checks = run.scheduler.due_offline_state_vehicles(work_now);
                        // C3 R51, R52: the probes of held cars share the tick with the
                        // reads and count against the same in-flight cap.
                        let probes = run.scheduler.due_probe_vehicles(work_now);
                        let due = run.scheduler.due_vehicles(work_now);
                        requests_sent |=
                            !checks.is_empty() || !probes.is_empty() || !due.is_empty();
                        let client_ref = &client;
                        let auth_ref = &auth;
                        let ledger_ref = &ledger;
                        let mut requests: Vec<TickFuture<'_>> = Vec::new();
                        // C3 R25: the month rolls over before the tick counts anything.
                        run.scheduler.refresh_month(work_now);
                        for (vehicle_id, kind) in probes {
                            // C3 R25: a record probe is a `state` request, a read probe a
                            // `data` request.
                            let category = match kind {
                                ProbeKind::ServiceRecord => RequestCategory::State,
                                ProbeKind::Read => RequestCategory::Data,
                            };
                            let log = RequestLog::for_vehicle(run.scheduler, vehicle_id);
                            // C5 R39, Codex R08: the credential unit decides at the
                            // request's own start whether it may go, and the request is
                            // counted when it is really sent. A wait or a stop leaves it
                            // unsent, uncounted and still due, with no failure.
                            requests.push(Box::pin(logged_request(log, async move {
                                let accounting = Some(RequestAccounting {
                                    ledger: ledger_ref,
                                    category,
                                });
                                let result = match kind {
                                    // The record probe is non-waking: it asks only
                                    // whether the car is in service (R52).
                                    ProbeKind::ServiceRecord => vehicle_probe_counted(
                                        client_ref, auth_ref, vehicle_id, accounting,
                                    )
                                    .await
                                    .map(TickPayload::InService),
                                    // A paused car's probe is a read, sent only while
                                    // the car is listed online (R51).
                                    ProbeKind::Read => vehicle_data_counted(
                                        client_ref, auth_ref, vehicle_id, None, accounting,
                                    )
                                    .await
                                    .map(TickPayload::Data),
                                };
                                (TickKey::Probe(vehicle_id, kind), result)
                            })));
                        }
                        for vehicle_id in checks {
                            // C3 R25: a targeted record check is a `state` request.
                            let log = RequestLog::for_vehicle(run.scheduler, vehicle_id);
                            requests.push(Box::pin(logged_request(log, async move {
                                let result = vehicle_state_counted(
                                    client_ref,
                                    auth_ref,
                                    vehicle_id,
                                    Some(RequestAccounting {
                                        ledger: ledger_ref,
                                        category: RequestCategory::State,
                                    }),
                                )
                                .await
                                .map(TickPayload::State);
                                (TickKey::Check(vehicle_id), result)
                            })));
                        }
                        apply_gate_revocations(run.scheduler, &streams);
                        for vehicle_id in due {
                            // C3 R36: a read made due by stream evidence while the
                            // car is Suspended rests on the live stream: the car's
                            // gate goes to the read, which checks `is_confirmed()` at
                            // send time. A gated read of a car whose stream is gone
                            // has no gate to confirm it and is not sent.
                            let gated = run.scheduler.requires_live_stream_power_gate(vehicle_id);
                            let power_gate = if gated {
                                streams
                                    .iter()
                                    .find(|stream| stream.vehicle_id == vehicle_id)
                                    .map(|stream| Arc::clone(&stream.power_gate))
                            } else {
                                None
                            };
                            requests.push(owner_read_future(
                                run.scheduler,
                                vehicle_id,
                                gated,
                                power_gate,
                                client_ref,
                                auth_ref,
                                ledger_ref,
                            ));
                        }
                        // The result handler revokes gates through these handles, so
                        // the queues can be lent to the pump while requests are
                        // pending (C3 R11, C4 R56 g).
                        let gates = stream_gates(&streams);
                        let flow = drive_requests(
                            &mut run,
                            requests,
                            Some(StreamPump {
                                store,
                                streams: &mut streams,
                                projection_car_ids: &mut stream_projection_car_ids,
                                pending: &mut pending_drain,
                                budget: &mut drain_budget,
                            }),
                            |run: &mut TickRun<'_>, key, result, now| {
                                match (key, result) {
                                    (TickKey::Check(vehicle_id), Ok(TickPayload::State(state))) => {
                                        report_successful_owner_api_request(&collector_state);
                                        let received_at_ms = clock.wall_now_ms()?;
                                        let candidates = run
                                            .scheduler
                                            .accept_vehicle_state(vehicle_id, state, now);
                                        run.batch.discoveries.extend(candidates.into_iter().map(
                                            |vehicle| DiscoveryItem {
                                                vehicle,
                                                received_at_ms,
                                            },
                                        ));
                                    }
                                    (
                                        TickKey::Probe(vehicle_id, _),
                                        Ok(TickPayload::InService(in_service)),
                                    ) => {
                                        report_successful_owner_api_request(&collector_state);
                                        // C3 R52: the car is in service still, or it left.
                                        run.scheduler.probe_finished(
                                            vehicle_id,
                                            ProbeOutcome::InService(in_service),
                                            now,
                                        );
                                    }
                                    (
                                        TickKey::Probe(vehicle_id, _),
                                        Ok(TickPayload::Data(snapshot)),
                                    ) => {
                                        report_successful_owner_api_request(&collector_state);
                                        // C3 R51, R52: a probe read that succeeded ends
                                        // the hold, and its snapshot is stored as any
                                        // read's.
                                        let vehicle = run.scheduler.vehicle_record(vehicle_id);
                                        let req_not_unlocked =
                                            vehicle.as_ref().is_none_or(|vehicle| {
                                                vehicle.settings.req_not_unlocked
                                            });
                                        run.scheduler.probe_finished(
                                            vehicle_id,
                                            ProbeOutcome::Read(
                                                poll_phase(&snapshot),
                                                sleep_eligible_with_policy(
                                                    &snapshot,
                                                    req_not_unlocked,
                                                ),
                                            ),
                                            now,
                                        );
                                        revoke_requested_gates(run.scheduler, &gates);
                                        let received_at_ms = clock.wall_now_ms()?;
                                        if let Some(vehicle) = vehicle {
                                            run.batch.snapshots.push(SnapshotItem {
                                                vehicle,
                                                snapshot,
                                                received_at_ms,
                                            });
                                        }
                                    }
                                    (
                                        TickKey::Read(vehicle_id),
                                        Ok(TickPayload::Data(snapshot)),
                                    ) => {
                                        report_successful_owner_api_request(&collector_state);
                                        let vehicle = run
                                            .scheduler
                                            .vehicles()
                                            .into_iter()
                                            .find(|vehicle| vehicle.id == vehicle_id);
                                        let req_not_unlocked =
                                            vehicle.as_ref().is_none_or(|vehicle| {
                                                vehicle.settings.req_not_unlocked
                                            });
                                        run.scheduler.vehicle_succeeded(
                                            vehicle_id,
                                            poll_phase(&snapshot),
                                            sleep_eligible_with_policy(&snapshot, req_not_unlocked),
                                            now,
                                        );
                                        // Entering Suspended revokes the car's gate (R36).
                                        revoke_requested_gates(run.scheduler, &gates);
                                        let received_at_ms = clock.wall_now_ms()?;
                                        if let Some(vehicle) = vehicle {
                                            run.batch.snapshots.push(SnapshotItem {
                                                vehicle,
                                                snapshot,
                                                received_at_ms,
                                            });
                                        }
                                    }
                                    (key, Err(error)) => {
                                        if loses_sensitive_access(&error) {
                                            // C3 R47, B2 clarification: as for the list;
                                            // the run ends and `drive_requests` stores
                                            // nothing.
                                            return Err(error);
                                        }
                                        report_terminal_auth_failure(&collector_state, &error);
                                        if is_credential_stop(&error) {
                                            run.scheduler.enter_credential_stop(now);
                                            logical_legacy_sign_out = true;
                                            return Ok(ReadFlow::SignedOut);
                                        }
                                        match key {
                                            TickKey::Read(vehicle_id) => {
                                                let effect =
                                                    run.scheduler.vehicle_failed_for_error(
                                                        vehicle_id, &error, now,
                                                    );
                                                // C3 R52: a car Tesla refuses as in service gets
                                                // its Hub event in this tick's transaction, once
                                                // per entry.
                                                if effect == FailureEffect::ServiceEntered {
                                                    push_service_refused_event(run, vehicle_id);
                                                }
                                                // A gated read the gate refused sent no request
                                                // and is no failure (R36); an in-service refusal
                                                // is a hold, not a failure.
                                                if !is_stream_power_not_confirmed(&error)
                                                    && !is_vehicle_in_service(&error)
                                                {
                                                    run.batch.failures.push(
                                                        VehicleCollectionFailure {
                                                            vehicle_id,
                                                            error:
                                                                owner_failure_for_collector_error(
                                                                    error,
                                                                ),
                                                        },
                                                    );
                                                }
                                            }
                                            TickKey::Check(vehicle_id) => {
                                                run.scheduler.offline_state_failed_for_error(
                                                    vehicle_id, &error, now,
                                                );
                                            }
                                            TickKey::Probe(vehicle_id, _) => {
                                                if run.scheduler.probe_finished(
                                                    vehicle_id,
                                                    ProbeOutcome::Failed(&error),
                                                    now,
                                                ) {
                                                    push_service_refused_event(run, vehicle_id);
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                                Ok(ReadFlow::Continue)
                            },
                        )
                        .await?;
                        if flow == ReadFlow::SignedOut {
                            signed_out = true;
                        }
                    } else {
                        run.commit_now(clock.mono_now()).await?;
                    }
                    tick_wrote = run.any_wrote;
                }
                supervised_tick_completed(scheduler.account.state.code()).await;
                if signed_out {
                    // The process remains alive, but the blown legacy fuse
                    // authoritatively fences every later Owner/stream action.
                    logical_legacy_sign_out = true;
                    stop_all_streams_and_store(
                        store,
                        &mut scheduler,
                        &mut streams,
                        &mut stream_projection_car_ids,
                        &mut pending_drain,
                        &mut drain_budget,
                    )
                    .await?;
                    continue 'collection;
                }
                stop_vehicle_streams(
                    store,
                    &mut scheduler,
                    &mut streams,
                    &disconnect_after,
                    &mut stream_projection_car_ids,
                    &mut pending_drain,
                    &mut drain_budget,
                )
                .await?;
                // What the drains made while the tick's requests were pending: a run
                // that ended is removed now, so a later run can start (C3 R28), and a
                // terminal error ends the run. A rejection stays pending and is
                // settled at the next drain.
                settle_pending_stream_drain(
                    store,
                    &mut scheduler,
                    &mut streams,
                    &mut stream_projection_car_ids,
                    &mut pending_drain,
                )
                .await?;
                // C3 R28: streams start for cars the tick found online and stop for
                // cars it found asleep; the gates the tick asked to revoke are
                // revoked.
                apply_gate_revocations(&mut scheduler, &streams);
                reconcile_streams(
                    store,
                    &mut scheduler,
                    &mut streams,
                    &auth,
                    &client,
                    cadence.stream_health_timeout,
                    &stream_endpoint,
                    &mut stream_projection_car_ids,
                    &mut pending_drain,
                    &mut drain_budget,
                )
                .await?;
                if tick_wrote || requests_sent {
                    publish_collector_status(scheduler.status(clock.mono_now()));
                }

                if stream_drain.backlog {
                    // After the outage fallback has had one chance to persist
                    // a current snapshot, return directly to the stream queue.
                    // Publication and enrichment can wait until the bounded
                    // receiver has caught up.
                    tokio::task::yield_now().await;
                    continue;
                }

                // C3 R11: wait for the earliest due action, never less than
                // `K_LOOP_FLOOR` after an iteration that sent nothing and wrote
                // nothing.
                let wait = scheduler.next_wait(
                    clock.mono_now(),
                    !streams.is_empty(),
                    !(requests_sent || tick_wrote),
                );
                let vehicles = scheduler.vehicles();
                assert_run_may_write("before-enrichment", &write_guard).await?;
                // Enrichment, export replay and the sleep are all waits the stream
                // queues are drained through, at the shared deadline (C3 R2, R11,
                // C4 R56 g): the interval between two drains does not grow by what the
                // tail of an iteration takes.
                let enrichment_now = clock.wall_now_ms()?;
                StreamPump {
                    store,
                    streams: &mut streams,
                    projection_car_ids: &mut stream_projection_car_ids,
                    pending: &mut pending_drain,
                    budget: &mut drain_budget,
                }
                .while_waiting(
                    &mut scheduler,
                    run_address_enrichment_once_guarded(
                        store,
                        config,
                        &cursor_key,
                        &vehicles,
                        enrichment_now,
                        runtime_admission.as_ref(),
                        Some(&write_guard),
                    ),
                )
                .await??;
                let export_now = clock.mono_now();
                if streams.is_empty() || export_now >= next_stream_export_replay {
                    assert_run_may_write("before-export-replay", &write_guard).await?;
                    let replay_now = clock.wall_now_ms()?;
                    let defer_live_compaction = !streams.is_empty();
                    StreamPump {
                        store,
                        streams: &mut streams,
                        projection_car_ids: &mut stream_projection_car_ids,
                        pending: &mut pending_drain,
                        budget: &mut drain_budget,
                    }
                    .while_waiting(
                        &mut scheduler,
                        replay_export_outbox_guarded(
                            store,
                            &cursor_key,
                            &vehicles,
                            replay_now,
                            defer_live_compaction,
                            Some(&write_guard),
                        ),
                    )
                    .await??;
                    next_stream_export_replay = export_now + STREAM_EXPORT_REPLAY_INTERVAL;
                }
                // The sleep ends by the shared drain deadline, and an iteration that
                // did nothing still lasts the loop floor, whatever drained meanwhile
                // (R11: earlier draining never permits an early empty iteration).
                end_iteration_sleep(
                    StreamPump {
                        store,
                        streams: &mut streams,
                        projection_car_ids: &mut stream_projection_car_ids,
                        pending: &mut pending_drain,
                        budget: &mut drain_budget,
                    },
                    &mut scheduler,
                    wait,
                    iteration_started,
                    !(requests_sent || tick_wrote),
                )
                .await?;
            }
            #[allow(unreachable_code)]
            Ok::<(), CollectorError>(())
        };
        tokio::pin!(collection_loop);
        let legacy_refresh_failure = wait_for_legacy_refresh_sensitive_failure(&auth);
        tokio::pin!(legacy_refresh_failure);
        tokio::pin!(shutdown);
        // The retained admission is watched whatever the loop waits for: a denial
        // ends the run within a second, even while a request or an exchange is held
        // (R47, B2 clarification 3).
        let admission_lost = async {
            match runtime_admission.as_deref() {
                Some(admission) => {
                    admission.wait_until_invalid().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(admission_lost);
        supervised_test_gate("supervisor-select").await;
        tokio::select! {
            biased;
            () = &mut admission_lost => (
                Err(CollectorError::SensitiveAccessUnavailable),
                false,
                false,
            ),
            result = &mut legacy_refresh_failure => (result, false, false),
            result = &mut resident_control_loop => (result, false, false),
            result = &mut collection_loop => (result, false, false),
            () = &mut shutdown => (Ok(()), false, false),
            result = &mut heartbeat_task => {
                let result = result
                    .map_err(|_| CollectorError::SupervisedHeartbeatTask)
                    .and_then(|result| result);
                #[cfg(test)]
                supervised_test_log(LoopEvent::RenewalEnded);
                (result, true, false)
            }
            result = terrain_worker.wait_until_exit() => {
                (result, false, true)
            }
        }
    };
    #[cfg(test)]
    supervised_test_log(LoopEvent::SupervisorSelected {
        by_renewal: heartbeat_finished,
        ok: collection_result.is_ok(),
    });

    // C5 R33a: no new token request starts from the moment the run decided to
    // end, whatever ended it. A stream's credential check that already owns the
    // manager would otherwise start one while the rest of the shutdown runs.
    {
        let CollectionAuth::Legacy { refresh, .. } = &auth;
        refresh.raise_shutdown();
    }
    // The loss of the retained admission is one monotonic fact for the whole
    // shutdown (R47, R8, B2 clarification 3): the run's own result, the credential
    // unit's shutdown outcome and the check at each write boundary all feed it, and
    // nothing erases it. The check here catches a denial the supervisor met before
    // it noticed the loss itself, as an orderly shutdown that wins the race.
    let mut loss = AdmissionLoss::with_fence(admission_fence);
    loss.note_result(&collection_result);
    loss.check(runtime_admission.as_deref());
    // C3 R8: the run's last rows and its `hub_stopped` gap, in one transaction,
    // before the lease is released. Not after the loss of the process's sensitive
    // access: the admission that guards the store is what failed, so no collector
    // write follows it, and the next admitted start records the downtime.
    let stop = if loss.observed() {
        StopRecord::Unrecorded
    } else {
        finish_collector_run(store, &mut scheduler)
    };
    // The renewal is fenced and joined at once when the run has lost its admission
    // or its stop went unrecorded, before any of the cleanup below can wait: a lease
    // that is retained to expire must not be renewed by the process that gave it up
    // (R8, B2 clarification 3). A loss that a later outcome first reveals does the
    // same where it is merged.
    let mut heartbeat_shutdown = Some(heartbeat_shutdown);
    let mut heartbeat_running = !heartbeat_finished;
    macro_rules! fence_renewal {
        ($unrecorded:expr) => {
            fence_renewal_if_due(
                &mut heartbeat_task,
                &mut heartbeat_shutdown,
                &mut heartbeat_running,
                &mut collection_result,
                &mut loss,
                $unrecorded,
            )
            .await
        };
    }
    fence_renewal!(stop == StopRecord::Unrecorded);

    // Dropping the active collection future stops new work. If the loss is already
    // observed, close sockets before joining the refresh task. Otherwise join
    // first: a concurrent helper/persistence failure must win over an orderly
    // shutdown so the sender cannot issue an unsubscribe after that authority has
    // been revoked.
    if loss.observed() {
        loss.merge(&mut collection_result, Err(CollectorError::SensitiveAccessUnavailable));
        if let Err(error) = &collection_result {
            report_terminal_auth_failure(&collector_state, error);
        }
        abort_and_clear_manual_probe_streams_without_egress(&mut streams).await;
    }
    #[cfg(test)]
    supervised_test_log(LoopEvent::CleanupWaiting);
    // The retained admission is watched while the credential unit is waited for
    // (R8, B2 clarification 3): a denial that arrives meanwhile is noted, and the
    // renewal is fenced and joined at once, while the unit's work is still held, and
    // the wait goes on.
    let (refresh_result, shutdown_result) = {
        let cleanup = async {
            let refresh_result = wait_for_legacy_refresh_before_owner(&auth).await;
            let shutdown_result = shutdown_legacy_refresh(&auth).await;
            (refresh_result, shutdown_result)
        };
        tokio::pin!(cleanup);
        match runtime_admission.as_deref() {
            Some(admission) if !loss.observed() => {
                let lost = async {
                    supervised_test_gate("cleanup-watch").await;
                    admission.wait_until_invalid().await
                };
                tokio::pin!(lost);
                tokio::select! {
                    biased;
                    cleaned = &mut cleanup => cleaned,
                    _ = &mut lost => {
                        loss.note();
                        fence_renewal!(false);
                        cleanup.await
                    }
                }
            }
            _ => cleanup.await,
        }
    };
    loss.merge(&mut collection_result, refresh_result);
    loss.merge(&mut collection_result, shutdown_result);
    fence_renewal!(false);
    // Revalidate before the streams' final frames are written.
    supervised_test_gate("before-final-frames").await;
    loss.check(runtime_admission.as_deref());
    if loss.observed() {
        if let Err(error) = &collection_result {
            report_terminal_auth_failure(&collector_state, error);
        }
        fence_renewal!(false);
        abort_and_clear_manual_probe_streams_without_egress(&mut streams).await;
    } else {
        // The orderly stop stores the frames the streams left queued (C3 R36), with
        // the loop's drain deadline kept while the runs wind down. The retained
        // admission is watched all the while: a denial drops the stop (the runs'
        // handles stay with their runtimes) and the runs are aborted locally, with
        // no frame written and no further signal sent.
        let stopped = {
            let admission_gone = async {
                match runtime_admission.as_deref() {
                    Some(admission) => {
                        admission.wait_until_invalid().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::pin!(admission_gone);
            // The shared fence is watched as well: a denial that only the renewal
            // met stays decisive when the admission passes again, and the stop's
            // frame writes check it (B2 fix round, P2, third pass).
            let shared_fence = loss.fence.clone();
            drain_budget.guard_writes_with(write_guard.clone());
            let fence_set = shared_fence.wait_set();
            tokio::pin!(fence_set);
            let stop_streams = stop_all_streams_and_store(
                store,
                &mut scheduler,
                &mut streams,
                &mut stream_projection_car_ids,
                &mut pending_drain,
                &mut drain_budget,
            );
            tokio::pin!(stop_streams);
            tokio::select! {
                biased;
                () = &mut admission_gone => None,
                () = &mut fence_set => None,
                stored = &mut stop_streams => Some(stored),
            }
        };
        match stopped {
            Some(stored) => {
                if let Some(error) = pending_drain.terminal_error.take() {
                    loss.merge(&mut collection_result, Err(error));
                }
                // A frame-write boundary of the stop met the shared fence: that loss
                // is the run's result like any other (B2 fix round, P2, third pass).
                loss.merge(&mut collection_result, stored);
                if loss.observed()
                    && let Err(error) = &collection_result
                {
                    report_terminal_auth_failure(&collector_state, error);
                }
                fence_renewal!(false);
            }
            None => {
                loss.note();
                loss.merge(&mut collection_result, Err(CollectorError::SensitiveAccessUnavailable));
                if let Err(error) = &collection_result {
                    report_terminal_auth_failure(&collector_state, error);
                }
                fence_renewal!(false);
                abort_and_clear_manual_probe_streams_without_egress(&mut streams).await;
            }
        }
    }
    // Stream tasks are joined and the refresh worker is stopped above. Drop the
    // last Owner API pool and credential authority before reporting collector
    // shutdown, so Serve cannot return with an idle outbound connection alive.
    drop(client);
    drop(auth);
    let terrain_result = terrain_worker.shutdown(terrain_finished).await;
    loss.merge(&mut collection_result, terrain_result);
    fence_renewal!(false);
    finish_supervised_collector(
        store,
        collector_lease,
        collection_result,
        &mut heartbeat_task,
        heartbeat_shutdown,
        !heartbeat_running,
        stop,
        loss,
        runtime_admission.as_deref(),
    )
    .await
}

/// Append the `service_refused` Hub event for a car that entered service mode with
/// this tick (C3 R52, C1 R15): it joins the tick's batch, so the intake and the
/// projection store it in the tick transaction with the hold that caused it. The
/// time is the Hub's wall clock at the decision; C1 closes the open drive and charge
/// at their last evidence.
fn push_service_refused_event(run: &mut TickRun<'_>, vehicle_id: VehicleId) {
    if let Some(vehicle) = run.scheduler.vehicle_record(vehicle_id) {
        let observed_at_ms = run.scheduler.wall_or_last();
        run.batch.hub_events.push(HubEventItem {
            vehicle,
            event: HUB_EVENT_SERVICE_REFUSED,
            observed_at_ms,
        });
    }
}

/// The owner's unapplied actions, read at a settings refresh (C3 R65). A read that
/// fails is logged and skipped: the rows stay in the table and the next refresh
/// reads them again.
fn read_owner_actions(store: &HubStore) -> Vec<OwnerActionRow> {
    match store.pending_collector_owner_actions() {
        Ok(records) => records
            .iter()
            .filter_map(OwnerActionRow::from_record)
            .collect(),
        Err(error) => {
            tracing::warn!(error = %error, "owner actions could not be read");
            Vec::new()
        }
    }
}

/// The tick's list with the refusal check of R48 (c), while the stream queues are
/// drained at least every `K_STREAM_DRAIN_CAP` (C3 R11; C4 R56 g). The list may wait
/// for a token exchange of tens of seconds (C5 R33), far more than the 10 s C4
/// waits for room in a queue, so nothing may wait for it before draining.
async fn list_with_stream_drain(
    mut pump: StreamPump<'_>,
    scheduler: &mut VehicleScheduler,
    client: &OwnerApi,
    auth: &CollectionAuth,
    owner_wire: bool,
    ledger: &RequestLedger,
) -> Result<CheckedList, CollectorError> {
    pump.while_waiting(
        scheduler,
        list_vehicles_with_refusal_check(client, auth, owner_wire, ledger),
    )
    .await
}

/// One scheduled `vehicle_data` read of the Owner loop (C3 R25, R36). `gated` is
/// whether the read rests on the live stream (made due by stream evidence while the
/// car is Suspended), and `power_gate` is that car's gate, absent when its stream is
/// gone. The gate is checked before the read is admitted: a gated read whose gate is
/// missing, revoked or not confirmed sends nothing and counts nothing, so the car
/// stays Suspended and no failure is counted. A read that passed the check and is
/// refused at the last moment (the gate is rechecked at send time) takes its start
/// back.
fn owner_read_future<'a>(
    scheduler: &mut VehicleScheduler,
    vehicle_id: VehicleId,
    gated: bool,
    power_gate: Option<Arc<StreamPowerGate>>,
    client: &'a OwnerApi,
    auth: &'a CollectionAuth,
    ledger: &'a RequestLedger,
) -> TickFuture<'a> {
    let gate_refuses = gated
        && !power_gate
            .as_ref()
            .is_some_and(|gate| gate.is_confirmed());
    if gate_refuses {
        return Box::pin(async move {
            (
                TickKey::Read(vehicle_id),
                Err(CollectorError::OwnerApi(
                    OwnerApiError::StreamPowerNotConfirmed,
                )),
            )
        });
    }
    let log = RequestLog::for_vehicle(scheduler, vehicle_id);
    // C5 R39, Codex R08: the credential unit decides at the read's own start whether
    // it may go, and the read is counted when it is really sent. A wait or a stop
    // leaves it unsent, uncounted and still due, with no failure, and so does a
    // read the gate refuses at the last moment (R25, R36).
    Box::pin(logged_request(log, async move {
        let result = vehicle_data_counted(
            client,
            auth,
            vehicle_id,
            power_gate.as_deref(),
            Some(RequestAccounting {
                ledger,
                category: RequestCategory::Data,
            }),
        )
        .await
        .map(TickPayload::Data);
        (TickKey::Read(vehicle_id), result)
    }))
}

/// One iteration of a signed-out account (C3 R47): `CredentialStop` sends nothing,
/// but the loop still reads the settings and the owner's actions (a "retry
/// collection" is applied and marked), watches the clock (R2), closes drives that
/// lost their evidence (R40), stores the scheduler's rows (the `auth_terminal` gap,
/// R8) and publishes its status. Returns how long the loop then waits (R11).
#[allow(clippy::too_many_arguments)]
async fn run_credential_stop_iteration(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    identities: &mut IdentityCache,
    cursor_key: &CursorKey,
    terrain_wake: &mpsc::Sender<()>,
    offline_drive_timeout: Duration,
    next_status_refresh: &mut Instant,
    owner_actions: &mut Vec<OwnerActionRow>,
    auth: &CollectionAuth,
) -> Result<Duration, CollectorError> {
    let configured = store.configured_tesla_vehicles()?;
    scheduler.apply_control_settings(&configured, scheduler.clock.mono_now());
    scheduler.observe_clock(scheduler.clock.mono_now());
    refresh_status_inputs(store, scheduler, next_status_refresh, owner_actions);
    let tick_now = scheduler.clock.mono_now();
    scheduler.begin_tick();
    if !owner_actions.is_empty() {
        apply_owner_actions_with_credentials(scheduler, auth, owner_actions, tick_now).await;
        owner_actions.clear();
    }
    let wrote = {
        let mut run = TickRun {
            scheduler,
            identities,
            commit: TickCommit {
                store,
                cursor_key,
                provider: CollectorProvider::Legacy,
                offline_drive_timeout,
            },
            batch: TickBatch::default(),
            terrain_wake,
            any_wrote: false,
        };
        plan_no_data_events(store, run.scheduler, &mut run.batch)?;
        let now = run.scheduler.clock.mono_now();
        run.commit_now(now).await?;
        run.any_wrote
    };
    let now = scheduler.clock.mono_now();
    if wrote {
        publish_collector_status(scheduler.status(now));
    }
    Ok(scheduler.next_wait(now, false, !wrote))
}

/// The settings refresh's reads (C3 R11, R65): at most once per
/// `CONTROL_SETTINGS_REFRESH`, the count of unnotified gaps for the status and the
/// owner's unapplied actions, which the next tick applies.
fn refresh_status_inputs(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    next_status_refresh: &mut Instant,
    owner_actions: &mut Vec<OwnerActionRow>,
) {
    let now = scheduler.clock.mono_now();
    if now < *next_status_refresh {
        return;
    }
    *next_status_refresh = now + CONTROL_SETTINGS_REFRESH;
    supervised_settings_refresh();
    match store.collection_gap_counts() {
        Ok((_, unnotified)) => scheduler.set_alert_gap_count(unnotified),
        Err(error) => {
            tracing::warn!(error = %error, "collection gap counts could not be read");
        }
    }
    *owner_actions = read_owner_actions(store);
    supervised_settings_refresh_completed();
}

/// The last step of a supervised run: stop the renewal, drain the publications
/// already started, and release the lease (C3 R8, B2 clarification 3).
///
/// The lease is released only after a recorded ordinary stop and only if no loss of
/// the retained admission was observed at any point, including after a successful
/// stop, during the credential unit's completion or while a publication drains. A
/// stop that was skipped or failed, or an observed loss, retains the lease to expire:
/// the invalidated process writes no replacement gap, and the next admitted start
/// reads the retained heartbeat before it replaces the expired authority.
///
/// Renewal stops differently by case. After an ordinary recorded stop with the
/// admission intact it keeps renewing while the already-started publications drain,
/// so a long drain cannot let the lease lapse under a process that is still writing.
/// After an unrecorded stop or an observed loss it is fenced and joined at once,
/// before the drain is waited for. A loss that arises during the drain fences and
/// joins it the moment it is seen and stays decisive. The publication drain itself
/// is preserved in every case.
#[allow(clippy::too_many_arguments)]
async fn finish_supervised_collector(
    store: &HubStore,
    collector_lease: SupervisedCollectorLease,
    mut collection_result: Result<(), CollectorError>,
    heartbeat_task: &mut JoinHandle<Result<(), CollectorError>>,
    heartbeat_shutdown: Option<oneshot::Sender<()>>,
    heartbeat_finished: bool,
    stop: StopRecord,
    mut loss: AdmissionLoss,
    admission: Option<&crate::hub_user_process::AdmittedUserHub>,
) -> Result<(), CollectorError> {
    loss.note_result(&collection_result);
    let mut heartbeat_shutdown = heartbeat_shutdown;
    let mut heartbeat_running = !heartbeat_finished;
    // The admission is revalidated here whatever the stop or the renewal came to.
    loss.check(admission);
    // Fence the renewal before anything waits for the drain.
    fence_renewal_if_due(
        heartbeat_task,
        &mut heartbeat_shutdown,
        &mut heartbeat_running,
        &mut collection_result,
        &mut loss,
        stop == StopRecord::Unrecorded,
    )
    .await;
    // Each blocking replay owns this gate before it is spawned. Reacquiring it
    // drains those jobs after collection stops; hold it through lease release so
    // removal cannot precede commit. The retained admission is watched while the
    // drain is waited for, whatever the renewal or the stop record came to: a denial
    // is noted the moment it is seen, ahead of a gate that is ready at the same time,
    // and the drain goes on.
    let acquire = store.acquire_publication_gate();
    tokio::pin!(acquire);
    let publication_gate = match admission {
        Some(admission) => {
            let lost = admission.wait_until_invalid();
            tokio::pin!(lost);
            tokio::select! {
                biased;
                _ = &mut lost => {
                    // The loss arose while a publication drains: fence and join the
                    // renewal now, and keep draining.
                    loss.note();
                    fence_renewal_if_due(
                        heartbeat_task,
                        &mut heartbeat_shutdown,
                        &mut heartbeat_running,
                        &mut collection_result,
                        &mut loss,
                        false,
                    )
                    .await;
                    acquire.await
                }
                gate = &mut acquire => gate,
            }
        }
        None => acquire.await,
    };
    // After the gate is held: the admission is revalidated, the renewal that kept the
    // lease alive while the drain lasted is stopped (its shutdown arm does not look
    // at the admission), and the admission is revalidated again after the join, with
    // nothing awaited between that check and the release: a denial during the join
    // must retain the lease. A recorded stop that met no denial releases; anything
    // else retains the lease.
    loss.check(admission);
    join_supervised_heartbeat(
        heartbeat_task,
        &mut heartbeat_shutdown,
        &mut heartbeat_running,
        &mut collection_result,
        &mut loss,
    )
    .await;
    let lost_after_join = loss.check(admission);
    let releasable = stop == StopRecord::Recorded && !lost_after_join;
    let release_result = match publication_gate {
        Ok(_publication_gate) if releasable => store
            .release_supervised_collector_lease(collector_lease)
            .map_err(CollectorError::from),
        Ok(_publication_gate) => Ok(()),
        Err(error) => Err(CollectorError::from(error)),
    };
    if collection_result.is_ok() {
        collection_result = release_result;
    }
    // A loss observed at any point stays the run's result, whatever else came after.
    if loss.observed() {
        loss.merge(
            &mut collection_result,
            Err(CollectorError::SensitiveAccessUnavailable),
        );
    }
    collection_result
}

/// Fence and join the renewal when the run has lost its admission or its stop went
/// unrecorded (`unrecorded`): the renewal then writes nothing more, and the lease is
/// left to expire. It does nothing while neither holds, or once the renewal has been
/// joined.
async fn fence_renewal_if_due(
    heartbeat_task: &mut JoinHandle<Result<(), CollectorError>>,
    heartbeat_shutdown: &mut Option<oneshot::Sender<()>>,
    heartbeat_running: &mut bool,
    collection_result: &mut Result<(), CollectorError>,
    loss: &mut AdmissionLoss,
    unrecorded: bool,
) {
    if *heartbeat_running && (unrecorded || loss.observed()) {
        join_supervised_heartbeat(
            heartbeat_task,
            heartbeat_shutdown,
            heartbeat_running,
            collection_result,
            loss,
        )
        .await;
    }
}

/// Stop the heartbeat task and join it, once, folding its result into the run's.
async fn join_supervised_heartbeat(
    heartbeat_task: &mut JoinHandle<Result<(), CollectorError>>,
    heartbeat_shutdown: &mut Option<oneshot::Sender<()>>,
    heartbeat_running: &mut bool,
    collection_result: &mut Result<(), CollectorError>,
    loss: &mut AdmissionLoss,
) {
    if !*heartbeat_running {
        return;
    }
    *heartbeat_running = false;
    if let Some(shutdown) = heartbeat_shutdown.take() {
        let _ = shutdown.send(());
    }
    let joined = heartbeat_task
        .await
        .map_err(|_| CollectorError::SupervisedHeartbeatTask)
        .and_then(|result| result);
    #[cfg(test)]
    supervised_test_log(LoopEvent::RenewalJoined);
    loss.merge(collection_result, joined);
}

#[cfg(test)]
async fn run_supervised_collector_heartbeat(
    store: HubStore,
    lease: SupervisedCollectorLease,
    state: watch::Receiver<SupervisedCollectorState>,
    shutdown: oneshot::Receiver<()>,
    interval: Duration,
) -> Result<(), CollectorError> {
    run_supervised_collector_heartbeat_with(
        store,
        lease,
        state,
        shutdown,
        interval,
        Arc::new(SystemCollectorClock),
        None,
        AdmissionFence::default(),
    )
    .await
}

/// Whether the renewal may write now: the run's loss fact is the first word (an
/// observed loss is irreversible, whatever a later revalidation says), then the
/// retained admission is revalidated, and its own denial sets the fence.
fn assert_renewal_allowed(
    admission: Option<&crate::hub_user_process::AdmittedUserHub>,
    fence: &AdmissionFence,
) -> Result<(), CollectorError> {
    if fence.is_set() {
        return Err(CollectorError::SensitiveAccessUnavailable);
    }
    assert_runtime_sensitive_access(admission).inspect_err(|_| {
        fence.set();
        #[cfg(test)]
        supervised_test_log(LoopEvent::RenewalDenied);
    })
}

/// The heartbeat with its two seams: the wall time of every renewal comes from
/// the collector clock (advancing Tokio's timer alone does not move a stored
/// lease), and an admitted runtime is revalidated before each renewal, so a
/// process that lost its admission writes nothing more and ends with the
/// sensitive-access failure (R47, B2 clarification). The `fence` is the run's
/// monotonic loss fact: a loss the supervisor observed ends the renewal at its next
/// step even when a later revalidation passes again.
#[allow(clippy::too_many_arguments)]
async fn run_supervised_collector_heartbeat_with(
    store: HubStore,
    lease: SupervisedCollectorLease,
    mut state: watch::Receiver<SupervisedCollectorState>,
    mut shutdown: oneshot::Receiver<()>,
    interval: Duration,
    clock: Arc<dyn CollectorClock>,
    admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
    fence: AdmissionFence,
) -> Result<(), CollectorError> {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            // The test seam may hold a renewal candidate before it is ready (the
            // shutdown arm stays pollable meanwhile); nothing in production.
            () = async { ticker.tick().await; supervised_test_gate("renewal-candidate").await } => {
                assert_renewal_allowed(admission.as_deref(), &fence)?;
                renew_supervised_collector_heartbeat_at(
                    &store,
                    lease,
                    *state.borrow_and_update(),
                    clock.wall_now_ms()?,
                )?;
            }
            changed = async {
                let changed = state.changed().await;
                supervised_test_gate("renewal-candidate").await;
                changed
            } => {
                if changed.is_err() {
                    return Ok(());
                }
                assert_renewal_allowed(admission.as_deref(), &fence)?;
                renew_supervised_collector_heartbeat_at(
                    &store,
                    lease,
                    *state.borrow_and_update(),
                    clock.wall_now_ms()?,
                )?;
            }
            _ = &mut shutdown => return Ok(()),
        }
    }
}

fn renew_supervised_collector_heartbeat_at(
    store: &HubStore,
    lease: SupervisedCollectorLease,
    state: SupervisedCollectorState,
    now_ms: i64,
) -> Result<(), CollectorError> {
    match store.heartbeat_supervised_collector_lease(lease, state, now_ms) {
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

/// A request succeeded: the lease is active (C5 R43: it is `auth_terminal` only
/// while the account is signed out, and a signed-out account sends nothing).
fn report_successful_owner_api_request(state: &watch::Sender<SupervisedCollectorState>) {
    set_supervised_collector_state(state, SupervisedCollectorState::Active);
}

/// Whether an error puts the lease in `auth_terminal` (C5 R43): the loss of the
/// authority to use the credential, a sign-out, and the old typed statuses.
fn is_terminal_auth_failure(error: &CollectorError) -> bool {
    legacy_auth_error_of(error).is_some_and(LegacyAuthError::is_signed_out)
        || matches!(
        error,
        CollectorError::OwnerApi(OwnerApiError::HttpStatus(401 | 403))
            | CollectorError::FleetApi(FleetApiError::HttpStatus(401))
            | CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
            | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(
                403
            )))
    ) || is_sensitive_access_failure(error)
        || matches!(error, CollectorError::FleetCredential(error) if error.is_admission_loss())
}

#[cfg(test)]
async fn replay_export_outbox(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
) -> Result<usize, CollectorError> {
    replay_export_outbox_with_compaction_deferral(store, cursor_key, vehicles, now_ms, false).await
}

#[cfg(test)]
async fn replay_export_outbox_with_compaction_deferral(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
    defer_live_compaction: bool,
) -> Result<usize, CollectorError> {
    replay_export_outbox_guarded(
        store,
        cursor_key,
        vehicles,
        now_ms,
        defer_live_compaction,
        None,
    )
    .await
}

/// The export replay of a supervised run (B2 fix round, P3b fix 2, R-P3bF2-1): the
/// outbox claim, the sync claim, the pack output, the catalogue commit and every
/// release, completion and failure are separate store mutations, each preceded by
/// the run's write guard inside the blocking worker, with no await between the guard
/// and the mutation. A refused write leaves the claims as they are: the cleanup is a
/// mutation too.
async fn replay_export_outbox_guarded(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
    defer_live_compaction: bool,
    guard: Option<&RunWriteGuard>,
) -> Result<usize, CollectorError> {
    #[cfg(test)]
    let replay_seam = supervised_test_replay_begun();
    #[cfg(test)]
    if let Some(seam) = &replay_seam {
        while seam.replay_hold_before_worker.load(Ordering::SeqCst) {
            sleep(Duration::from_millis(5)).await;
        }
    }
    let publication_gate = store.acquire_publication_gate().await?;
    let store = store.clone();
    let cursor_key = cursor_key.clone();
    let vehicles = vehicles.to_vec();
    let guard = guard.cloned();
    #[cfg(test)]
    let replay_ack = replay_seam.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        REPLAY_TEST_SEAM.with(|cell| *cell.borrow_mut() = replay_seam);
        replay_export_outbox_blocking(
            &store,
            &publication_gate,
            &cursor_key,
            &vehicles,
            now_ms,
            defer_live_compaction,
            guard.as_ref(),
        )
    })
    .await;
    #[cfg(test)]
    if let Some(seam) = &replay_ack {
        seam.replay_completed.fetch_add(1, Ordering::SeqCst);
    }
    outcome.map_err(|_| CollectorError::ExportPublicationTask)?
}

fn replay_export_outbox_blocking(
    store: &HubStore,
    publication_gate: &crate::db::PublicationGate,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
    defer_live_compaction: bool,
    guard: Option<&RunWriteGuard>,
) -> Result<usize, CollectorError> {
    check_optional_write(guard, "export-claim")?;
    let Some(claim) = store.claim_export_outbox(now_ms)? else {
        return Ok(0);
    };
    #[cfg(test)]
    supervised_test_replay_claimed();
    if store.vehicle_has_v2_base(claim.vehicle_id)? {
        let pack_count = store.v2_lineage_pack_count(claim.vehicle_id)?;
        if defer_live_compaction
            && live_delta_compaction_required(pack_count, ProtocolLimits::default())
        {
            check_optional_write(guard, "export-release")?;
            store.release_export_outbox(&claim)?;
            tracing::info!(
                vehicle_id = %claim.vehicle_id,
                pack_count,
                "deferred derived lineage compaction until active streams stop"
            );
            return Ok(0);
        }
        check_optional_write(guard, "export-sync-claim")?;
        let Some(sync_claim) = store.claim_sync_mutations(claim.vehicle_id, now_ms, 10_000)? else {
            if store.has_unpublished_sync_mutations(claim.vehicle_id)? {
                check_optional_write(guard, "export-release")?;
                store.release_export_outbox(&claim)?;
                return Ok(0);
            }
            check_optional_write(guard, "export-complete")?;
            store.complete_export_outbox(&claim)?;
            return Ok(0);
        };
        return match publish_v2_delta_guarded(store, cursor_key, &sync_claim, guard) {
            Ok(()) => {
                // A single outbox revision can represent more mutations than
                // one bounded claim. Keep it scheduled until every mutation
                // is durably published; otherwise the first 10,000 rows would
                // consume the outbox row and strand the remainder forever.
                if store.has_unpublished_sync_mutations(claim.vehicle_id)? {
                    check_optional_write(guard, "export-release")?;
                    store.release_export_outbox(&claim)?;
                } else {
                    check_optional_write(guard, "export-complete")?;
                    store.complete_export_outbox(&claim)?;
                }
                Ok(1)
            }
            Err(error) => {
                // The cleanup writes are mutations like any other: after the loss
                // the claims stay as they are and the next start reclaims them.
                check_optional_write(guard, "export-failure-release-sync")?;
                store.release_sync_mutations(&sync_claim)?;
                check_optional_write(guard, "export-failure-delta")?;
                store.fail_export_outbox(&claim, "publication_failed", now_ms)?;
                Err(error)
            }
        };
    }
    let Some(source_key) = store.source_vehicle_key(claim.vehicle_id)? else {
        check_optional_write(guard, "export-failure-identity")?;
        store.fail_export_outbox(&claim, "vehicle_identity_missing", now_ms)?;
        return Ok(0);
    };
    let source_key_number = source_key
        .strip_prefix("eid:")
        .or_else(|| source_key.strip_prefix("vid:"))
        .unwrap_or(&source_key);
    let Ok(source_vehicle_id) = source_key_number.parse::<u64>() else {
        check_optional_write(guard, "export-failure-identity")?;
        store.fail_export_outbox(&claim, "vehicle_identity_invalid", now_ms)?;
        return Ok(0);
    };
    let Some(vehicle) = vehicles
        .iter()
        .find(|vehicle| vehicle.id.get() == source_vehicle_id)
    else {
        check_optional_write(guard, "export-failure-identity")?;
        store.fail_export_outbox(&claim, "vehicle_not_discovered", now_ms)?;
        return Ok(0);
    };
    let collection = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![],
        failures: vec![],
    };
    match publish_compatibility_snapshots_for_provider_guarded(
        store,
        publication_gate,
        cursor_key,
        &collection,
        now_ms,
        CollectorProvider::Legacy,
        guard,
    ) {
        Ok(_) => {
            check_optional_write(guard, "export-complete")?;
            store.complete_export_outbox(&claim)?;
            Ok(1)
        }
        // The loss of the run is the run's result, not a failed publication.
        Err(error) if loses_sensitive_access(&error) => Err(error),
        Err(_) => {
            check_optional_write(guard, "export-failure-publication")?;
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
    publish_v2_delta_guarded(store, cursor_key, claim, None)
}

fn publish_v2_delta_guarded(
    store: &HubStore,
    cursor_key: &CursorKey,
    claim: &crate::db::SyncMutationClaim,
    guard: Option<&RunWriteGuard>,
) -> Result<(), CollectorError> {
    compact_v2_lineage_if_needed_guarded(store, cursor_key, claim.vehicle_id, guard)?;
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
    // The pack file is output of the run: written only while the guard allows.
    check_optional_write(guard, "export-delta-pack")?;
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
    check_optional_write(guard, "export-delta-commit")?;
    store.commit_v2_delta_claim(claim, &lineage_delta, cursor_key, &terminal_cursor)?;
    Ok(())
}

fn compact_v2_lineage_if_needed_guarded(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    guard: Option<&RunWriteGuard>,
) -> Result<(), CollectorError> {
    compact_v2_lineage_if_needed_with_limits_guarded(
        store,
        cursor_key,
        vehicle_id,
        ProtocolLimits::default(),
        guard,
    )
}

#[cfg(test)]
fn compact_v2_lineage_if_needed_with_limits(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    limits: ProtocolLimits,
) -> Result<(), CollectorError> {
    compact_v2_lineage_if_needed_with_limits_guarded(store, cursor_key, vehicle_id, limits, None)
}

fn compact_v2_lineage_if_needed_with_limits_guarded(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    limits: ProtocolLimits,
    guard: Option<&RunWriteGuard>,
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
    check_optional_write(guard, "export-compaction-pack")?;
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
    check_optional_write(guard, "export-compaction-commit")?;
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
