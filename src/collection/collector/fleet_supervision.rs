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
    write_guard: RunWriteGuard,
    allow_refresh: bool,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    // The run's write guard exists before the credential manager and the cursor key
    // (B2 fix round, P3b fix 3, R-P3bF3-1): the entry point made it, and it has been
    // consulted at every write since. The renewal revalidates the retained admission
    // before every write (R47, B2 clarification 3) and, after a denial, writes nothing
    // more, and every store mutation of the run is preceded by the same guard. A Fleet
    // run always holds a retained admission.
    let admission = write_guard
        .admission
        .clone()
        .ok_or(CollectorError::SensitiveAccessUnavailable)?;
    let admission_fence = write_guard.fence.clone();
    // The credential manager, and the exchange jobs it detaches, share the guard (B2
    // fix round, P3b fix 1, R-P3bF-1; fix 2): they persist nothing once the
    // supervisor has observed the loss, or the retained admission is gone now, even
    // when the admission passes again.
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
    // C3 R7 step 1 and R8, before any other start step. Fleet polling has no
    // stream endpoint.
    write_guard.check("start-run")?;
    let collector_start = start_collector_run(store, config, &cadence, "", previous_heartbeat_ms)?;
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
            Some(Arc::clone(&admission)),
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
    if startup_result.is_ok() {
        startup_result = assert_runtime_sensitive_access(Some(&*admission));
    }
    // A fenced manager (an identified unresolved refresh) refreshes nothing, at
    // start or ever: its stop is entered below, before the first tick.
    let input_fenced = manager.lock().await.is_input_fenced();
    if startup_result.is_ok() && allow_refresh && !input_fenced {
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
    let mut scheduler = VehicleScheduler::new_for_mode(
        cadence,
        crate::config::CollectorMode::FleetPolling,
        Instant::now(),
    );
    scheduler.guard_writes_with(write_guard.clone());
    // C3 R2: every read of time in the loop goes through the scheduler's clock.
    let clock = Arc::clone(&scheduler.clock);
    scheduler.load(&collector_start.snapshot, clock.mono_now());
    if input_fenced {
        // C3 R47, B2 clarification 3: the account stops before anything is sent,
        // over any retained stop the ordinary start loaded; the lease is active and
        // the settings, owner actions, clock watch and status go on.
        scheduler.enter_fleet_refresh_stop_at_start();
    }
    // C3 R25: the request counter is shared with the resident actions and the
    // helpers that resend a request after a token refresh.
    let ledger = scheduler.request_ledger();
    let mut identities = IdentityCache::default();
    let mut next_status_refresh = clock.mono_now();
    // The owner's unapplied actions, read at each settings refresh and applied in
    // the next tick (C3 R65).
    let mut owner_actions: Vec<OwnerActionRow> = Vec::new();
    let (mut collection_result, heartbeat_finished, terrain_finished) = {
        let resident_control_loop = async {
            match resident_socket {
                Some(socket) => {
                    socket
                        .serve_fleet_guarded(
                            store.clone(),
                            api.clone(),
                            auth_api.clone(),
                            command_proxy.clone(),
                            Arc::clone(&manager),
                            ledger.clone(),
                            Some(write_guard.clone()),
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
                    &ledger,
                    &write_guard,
                )
                .await;
            }
            loop {
                supervised_loop_iteration();
                // The test seam may hold the loop here, pending, so that another arm of
                // the supervisor's wait is the one that is ready.
                supervised_test_gate("iteration-top").await;
                // C3 R47, B2 clarification: a failed revalidation is the loss of the
                // process's sensitive access (`SensitiveAccessUnavailable`), which
                // ends the run with no stop, no gap and no further collector write.
                write_guard.check("iteration-top")?;
                let configured = store.configured_tesla_vehicles()?;
                scheduler.apply_control_settings(&configured, clock.mono_now());
                // C3 R2: a host sleep is recorded as a gap and a backward wall step
                // is logged once, before this iteration plans anything.
                scheduler.observe_clock(clock.mono_now());
                refresh_status_inputs(
                    store,
                    &mut scheduler,
                    &mut next_status_refresh,
                    &mut owner_actions,
                );

                // One tick (C3 R5): the list and the reads of this iteration,
                // committed in one transaction. Fleet polling has no targeted
                // record checks: they are lists (C3 R46).
                let FleetTickOutcome {
                    requests_sent,
                    tick_wrote,
                } = run_fleet_tick(
                    &FleetTickEnv {
                        store,
                        cursor_key: &cursor_key,
                        terrain_wake: &terrain_wake,
                        offline_drive_timeout: cadence.offline_drive_timeout,
                        api: &api,
                        auth_api: &auth_api,
                        manager: &manager,
                        ledger: &ledger,
                        allow_refresh,
                        collector_state: &collector_state,
                        configured: &configured,
                    },
                    &mut scheduler,
                    &mut identities,
                    &owner_actions,
                )
                .await?;
                supervised_tick_completed(scheduler.account.state.code()).await;
                owner_actions.clear();
                if tick_wrote || requests_sent {
                    publish_collector_status(scheduler.status(clock.mono_now()));
                }
                // C3 R11: wait for the earliest due action, never less than
                // `K_LOOP_FLOOR` after an iteration that sent nothing and wrote
                // nothing.
                let wait =
                    scheduler.next_wait(clock.mono_now(), false, !(requests_sent || tick_wrote));
                let vehicles = scheduler.vehicles();
                assert_run_may_write("before-enrichment", &write_guard).await?;
                run_address_enrichment_once_guarded(
                    store,
                    config,
                    &cursor_key,
                    &vehicles,
                    clock.wall_now_ms()?,
                    Some(&admission),
                    Some(&write_guard),
                )
                .await?;
                assert_run_may_write("before-export-replay", &write_guard).await?;
                replay_export_outbox_guarded(
                    store,
                    &cursor_key,
                    &vehicles,
                    clock.wall_now_ms()?,
                    false,
                    Some(&write_guard),
                )
                .await?;
                if !wait.is_zero() {
                    cadence_sleep(wait).await;
                }
            }
        };
        tokio::pin!(collection_loop);
        tokio::pin!(shutdown);
        // The retained admission is watched whatever the loop waits for: a denial
        // ends the run within a second, even while a request or an exchange is held
        // (R47, B2 clarification 3).
        let admission_lost = admission.wait_until_invalid();
        tokio::pin!(admission_lost);
        supervised_test_gate("supervisor-select").await;
        tokio::select! {
            biased;
            _ = &mut admission_lost => (
                Err(CollectorError::SensitiveAccessUnavailable),
                false,
                false,
            ),
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
            result = terrain_worker.wait_until_exit() => (result, false, true),
        }
    };
    #[cfg(test)]
    supervised_test_log(LoopEvent::SupervisorSelected {
        by_renewal: heartbeat_finished,
        ok: collection_result.is_ok(),
    });
    // The loss of the retained admission is one monotonic fact for the whole
    // shutdown (R47, R8, B2 clarification 3): the run's own result, the settlement's
    // outcome and the check at each write boundary feed it, and nothing erases it.
    // The check here catches a denial the supervisor met before it noticed the loss
    // itself, as an orderly shutdown that wins the race.
    let mut loss = AdmissionLoss::with_fence(admission_fence);
    loss.note_result(&collection_result);
    loss.check(Some(&*admission));
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
    // or its stop went unrecorded, before the settlement or any other cleanup can
    // wait (R8, B2 clarification 3); a loss a later outcome first reveals does the
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
    if loss.observed() {
        loss.merge(&mut collection_result, Err(CollectorError::SensitiveAccessUnavailable));
    }
    if let Err(error) = &collection_result {
        report_terminal_auth_failure(&collector_state, error);
    }
    // The collection and resident-control futures have been dropped. A token
    // exchange they had started runs in a task of its own: wait for it to store
    // its successor before the clients go (Codex R06). After an observed loss of the
    // process's sensitive access nothing is settled: the admission that guards the
    // store is what failed (C3 R47). What the settlement returns reaches the
    // finaliser: a late denial is the run's sensitive result, and nothing later
    // erases it.
    if !loss.observed() {
        // The retained admission is watched while the exchange is waited for: a
        // denial that arrives meanwhile is noted, and the renewal is fenced and
        // joined at once, while the exchange is still held, and the wait goes on
        // (R8, B2 clarification 3).
        let settlement = settle_fleet_exchange(&manager, Some(&*admission));
        tokio::pin!(settlement);
        let lost = async {
            supervised_test_gate("cleanup-watch").await;
            admission.wait_until_invalid().await
        };
        tokio::pin!(lost);
        let settled = tokio::select! {
            biased;
            settled = &mut settlement => settled,
            _ = &mut lost => {
                loss.note();
                fence_renewal!(false);
                settlement.await
            }
        }
        .into_result();
        if let Err(error) = &settled {
            report_terminal_auth_failure(&collector_state, error);
        }
        loss.merge(&mut collection_result, settled);
        fence_renewal!(false);
    }
    // Release every Fleet HTTP pool/proxy handle before the worker can report
    // stopped.
    drop(command_proxy);
    drop(auth_api);
    drop(api);
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
        Some(&*admission),
    )
    .await
}

/// How long a graceful shutdown waits for a token exchange that had started (C5
/// R33a, Codex R06): the 60-second bound of the token request itself (headers and
/// body included) plus a five-second allowance for the completion that follows the
/// answer. It is not a total deadline of the synchronous store work the completion
/// then does, and it proves no platform acceptance.
const FLEET_EXCHANGE_SHUTDOWN_WAIT: Duration = Duration::from_secs(65);

/// The least shutdown allowance the service definitions give the Hub (C5 R33a): the
/// settlement fits inside it, and the token request fits inside the settlement.
const SERVICE_SHUTDOWN_ALLOWANCE: Duration = Duration::from_secs(90);

const _: () = assert!(
    crate::fleet_api::FLEET_TOKEN_REQUEST_MAX_TIMEOUT.as_secs()
        < FLEET_EXCHANGE_SHUTDOWN_WAIT.as_secs()
        && FLEET_EXCHANGE_SHUTDOWN_WAIT.as_secs() < SERVICE_SHUTDOWN_ALLOWANCE.as_secs()
);

/// What waiting for a Fleet token exchange at shutdown came to (C3 R8, C5 R23, B2
/// clarification 3). The settlement returns its actual outcome instead of dropping
/// it, so a late denial of the retained admission reaches the finaliser.
#[derive(Debug)]
enum FleetSettlement {
    /// Nothing was in flight, or the exchange finished and its outcome was applied.
    Settled,
    /// The exchange finished in an error of the credential unit.
    Failed(FleetCredentialError),
    /// The allowance ran out before the exchange finished.
    TimedOut,
}

impl FleetSettlement {
    /// The settlement as the run's outcome: the exchange's own error reaches the
    /// finaliser as it was. A denial of the retained admission is the loss that
    /// retains the lease; any other failure of the exchange is the credential
    /// unit's business (it stays fenced and the next start reads it) and is not the
    /// loss of the admission. A timeout is logged where it happens and is no error
    /// of the run: the exchange goes on in its own task.
    fn into_result(self) -> Result<(), CollectorError> {
        match self {
            Self::Failed(error) => Err(CollectorError::FleetCredential(error)),
            Self::Settled | Self::TimedOut => Ok(()),
        }
    }
}

/// Waits for a Fleet token exchange that was started before shutdown, so that a
/// rotated refresh token is written and the owner does not have to sign in again
/// for an ordinary stop. Only a refresh the process itself started has anything to
/// wait for; with none in flight it returns at once. The allowance is for the
/// exchange to complete; it is not a total deadline of the synchronous store work
/// the completion then does. The retained admission is revalidated at the boundary
/// where the wait ends, whether the exchange finished or the allowance ran out.
async fn settle_fleet_exchange(
    manager: &Arc<tokio::sync::Mutex<FleetAuthManager>>,
    admission: Option<&crate::hub_user_process::AdmittedUserHub>,
) -> FleetSettlement {
    #[cfg(test)]
    supervised_test_log(LoopEvent::FleetSettlementBegun);
    let settle = async { manager.lock().await.finish_exchange().await };
    let outcome = match timeout(FLEET_EXCHANGE_SHUTDOWN_WAIT, settle).await {
        Ok(Ok(())) => FleetSettlement::Settled,
        Ok(Err(error)) => FleetSettlement::Failed(error),
        Err(_) => {
            tracing::warn!("a Fleet token exchange did not finish before shutdown");
            FleetSettlement::TimedOut
        }
    };
    if assert_runtime_sensitive_access(admission).is_err() {
        return FleetSettlement::Failed(FleetCredentialError::SensitiveAccessUnavailable);
    }
    outcome
}

/// Everything one Fleet tick reads besides the scheduler (C3 R5): the store and the
/// commit context, the Fleet clients and credentials, the request counter and the
/// cars the owner configured.
struct FleetTickEnv<'a> {
    store: &'a HubStore,
    cursor_key: &'a CursorKey,
    terrain_wake: &'a mpsc::Sender<()>,
    offline_drive_timeout: Duration,
    api: &'a FleetApi,
    auth_api: &'a FleetAuthApi,
    manager: &'a Arc<tokio::sync::Mutex<FleetAuthManager>>,
    ledger: &'a RequestLedger,
    allow_refresh: bool,
    collector_state: &'a watch::Sender<SupervisedCollectorState>,
    configured: &'a [(Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
}

/// What one Fleet tick did, for the loop's status and waiting rule (C3 R11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FleetTickOutcome {
    /// A request was sent.
    requests_sent: bool,
    /// A transaction of the tick committed something.
    tick_wrote: bool,
}

/// One probe of a held car as a tick request (C3 R51, R52). The request's counter
/// guard is taken here, synchronously, before the future is boxed (C3 R47, B2
/// clarification): the loop admitted the request, so the guard must exist whether or
/// not the future is ever polled, and dropping it unpolled takes the start back.
fn fleet_probe_future<'a>(
    log: RequestLog,
    env: &'a FleetTickEnv<'a>,
    vehicle: Vehicle,
    vehicle_id: VehicleId,
    kind: ProbeKind,
    tick: &'a TickRefresh,
) -> TickFuture<'a> {
    let counted = CountedStart::admitted(Some(env.ledger), RequestCategory::Data);
    Box::pin(logged_request(log, async move {
        (
            TickKey::Probe(vehicle_id, kind),
            fleet_vehicle_data_with_auth(
                env.api,
                env.auth_api,
                env.manager,
                &vehicle,
                env.allow_refresh,
                counted,
                tick,
            )
            .await
            .map(TickPayload::Data),
        )
    }))
}

/// One scheduled `vehicle_data` read as a tick request; see [`fleet_probe_future`]
/// for the guard.
fn fleet_read_future<'a>(
    log: RequestLog,
    env: &'a FleetTickEnv<'a>,
    vehicle: Vehicle,
    vehicle_id: VehicleId,
    tick: &'a TickRefresh,
) -> TickFuture<'a> {
    let counted = CountedStart::admitted(Some(env.ledger), RequestCategory::Data);
    Box::pin(logged_request(log, async move {
        (
            TickKey::Read(vehicle_id),
            fleet_vehicle_data_with_auth(
                env.api,
                env.auth_api,
                env.manager,
                &vehicle,
                env.allow_refresh,
                counted,
                tick,
            )
            .await
            .map(TickPayload::Data),
        )
    }))
}

/// One Fleet tick (C3 R5): the list and the reads of this iteration, committed in
/// one transaction, each request admitted under the local cap and its reservation
/// (R27). Fleet polling has no targeted record checks: they are lists (R46). It is
/// the body of the supervised loop's iteration, a function so that the tests drive
/// the production tick against a fake Fleet server.
async fn run_fleet_tick(
    env: &FleetTickEnv<'_>,
    scheduler: &mut VehicleScheduler,
    identities: &mut IdentityCache,
    owner_actions: &[OwnerActionRow],
) -> Result<FleetTickOutcome, CollectorError> {
    let store = env.store;
    // C3 R2: every read of time in the tick goes through the scheduler's clock.
    let clock = Arc::clone(&scheduler.clock);
    let tick_now = clock.mono_now();
    scheduler.begin_tick();
    // C3 R65: the owner's actions apply inside the tick, so the
    // transaction that stores their effects also marks them applied.
    if !owner_actions.is_empty() {
        scheduler.apply_owner_actions(owner_actions, tick_now);
    }
    let mut requests_sent = false;
    let tick_wrote: bool;
    // C3 R47: at most one token refresh per tick, shared by the tick's requests.
    let tick_refresh = TickRefresh::default();
    {
        let mut run = TickRun {
            scheduler,
            identities,
            commit: TickCommit {
                store,
                cursor_key: env.cursor_key,
                provider: CollectorProvider::Fleet,
                offline_drive_timeout: env.offline_drive_timeout,
            },
            batch: TickBatch::default(),
            terrain_wake: env.terrain_wake,
            any_wrote: false,
        };
        // C3 R40: a drive whose car has been silent for more than `T_gap`
        // is closed by a Hub event in this tick's transaction, whatever
        // the car's phase, hold, account state or wait.
        plan_no_data_events(store, run.scheduler, &mut run.batch)?;
        if run.scheduler.discovery_due(tick_now) {
            // C3 R27: the persisted reservation covers the list before it
            // is sent (a lone reservation commit when no result waits),
            // and the cap decides whether it is sent at all.
            // C3 R47, R27 (B2 clarification, option H): no slot is planned for a
            // resend after a mid-tick token refresh. A resend is a billed request
            // like any other: it is sent only within the persisted reservation,
            // and one the reservation does not cover stays due and goes out after
            // the next commit raises it.
            reserve_for(&mut run, &[(RequestCategory::State, 1)]).await?;
            if run
                .scheduler
                .admit_request(RequestCategory::State, clock.mono_now())
                == Admission::Admitted
            {
                requests_sent = true;
                let list_log = RequestLog::for_account(run.scheduler);
                let list_started = list_log.clock.mono_now();
                let listed = fleet_list_vehicles_with_auth(
                    env.api,
                    env.auth_api,
                    env.manager,
                    env.allow_refresh,
                    Some(env.ledger),
                    &tick_refresh,
                )
                .await;
                list_log.emit("list", RequestKind::List, list_started, &listed);
                match accept_scheduled_provider_inventory_guarded(
                    store,
                    CollectorProvider::Fleet,
                    listed,
                    run.scheduler.write_guard(),
                ) {
                    Ok(vehicles) => {
                        let vehicles = filter_configured_vehicles_with_identity(
                            store,
                            vehicles,
                            env.configured,
                            CollectorProvider::Fleet,
                        )?;
                        report_successful_owner_api_request(env.collector_state);
                        let list_time = clock.mono_now();
                        let received_at_ms = clock.wall_now_ms()?;
                        let candidates = run.scheduler.accept_discovery(vehicles, list_time);
                        // Name the cars the list introduced, so their rows
                        // can be written in this tick.
                        run.scheduler
                            .apply_control_settings(env.configured, list_time);
                        run.batch
                            .discoveries
                            .extend(candidates.into_iter().map(|vehicle| DiscoveryItem {
                                vehicle,
                                received_at_ms,
                            }));
                        run.batch.note_result(list_time);
                    }
                    Err(error) => {
                        report_terminal_auth_failure(env.collector_state, &error);
                        if loses_sensitive_access(&error) {
                            // C3 R47, B2 clarification: the process is no longer the
                            // admitted Hub instance. Not an account state: no stop,
                            // no gap and no further collector write; the next
                            // admitted start records the downtime (R8).
                            return Err(error);
                        }
                        if matches!(error, CollectorError::FleetResendWithheld) {
                            // C3 R27, R47: the list went out and its resend is not
                            // covered by the persisted reservation. Nothing failed
                            // and nothing waits: the list stays due, and the raise
                            // rides on this tick's commit.
                            run.scheduler.list_resend_withheld(clock.mono_now());
                        } else if fleet_credential_stop(&error) {
                            // C3 R47: a 401 that survived the tick's one refresh and
                            // resend. The account stops inside the loop
                            // (`CredentialStop`, the `auth_terminal` gap): the
                            // iteration goes on with the settings refresh, the
                            // owner's actions, the clock watch, R40 and status, and
                            // sends nothing. The tick's rows are stored below.
                            run.scheduler.enter_credential_stop(clock.mono_now());
                        } else if fleet_refresh_outcome_unknown(store, &error) {
                            // C5 R47, C3 R47: an identified unresolved refresh. The
                            // account stops inside the loop with the reconnect
                            // attention and an active lease; nothing is sent.
                            run.scheduler
                                .enter_fleet_refresh_stop(clock.mono_now());
                        } else {
                            let delay = run
                                .scheduler
                                .discovery_failed_for_error(&error, clock.mono_now());
                            tracing::warn!(
                                retry_seconds = delay.as_secs(),
                                "Fleet discovery failed; backing off"
                            );
                        }
                    }
                }
            }
        }
        let work_now = clock.mono_now();
        // C3 R51, R52: Fleet polling has no `in_service` probe among its
        // calls, so every probe is a read, sent only while the latest list
        // says `online`; it shares the tick and the in-flight cap with the
        // reads.
        let probes = run.scheduler.due_probe_vehicles(work_now);
        let due = run.scheduler.due_vehicles(work_now);
        // C3 R27: the reservation for every read this tick plans is
        // committed before the first is sent; each read is then admitted
        // under the cap, and one that is withheld stays due.
        let planned_reads = u64::try_from(probes.len() + due.len()).unwrap_or(u64::MAX);
        // No slot for a resend (R47, B2 clarification, option H): see the list.
        reserve_for(&mut run, &[(RequestCategory::Data, planned_reads)]).await?;
        let mut requests: Vec<TickFuture<'_>> = Vec::new();
        let tick_ref = &tick_refresh;
        for (vehicle_id, kind) in probes {
            let Some(vehicle) = run.scheduler.vehicle_record(vehicle_id) else {
                continue;
            };
            if run
                .scheduler
                .admit_request(RequestCategory::Data, clock.mono_now())
                != Admission::Admitted
            {
                run.scheduler.request_withheld(vehicle_id);
                continue;
            }
            let log = RequestLog::for_vehicle(run.scheduler, vehicle_id);
            requests.push(fleet_probe_future(
                log, env, vehicle, vehicle_id, kind, tick_ref,
            ));
        }
        for vehicle_id in due {
            let Some(vehicle) = run
                .scheduler
                .vehicles()
                .into_iter()
                .find(|vehicle| vehicle.id == vehicle_id)
            else {
                continue;
            };
            if run
                .scheduler
                .admit_request(RequestCategory::Data, clock.mono_now())
                != Admission::Admitted
            {
                run.scheduler.request_withheld(vehicle_id);
                continue;
            }
            let log = RequestLog::for_vehicle(run.scheduler, vehicle_id);
            requests.push(fleet_read_future(log, env, vehicle, vehicle_id, tick_ref));
        }
        requests_sent |= !requests.is_empty();
        // Fleet polling has no stream, so there is no queue to drain.
        drive_requests(
            &mut run,
            requests,
            None,
            |run: &mut TickRun<'_>, key, result, now| {
                let (vehicle_id, probe) = match key {
                    TickKey::Read(vehicle_id) | TickKey::Check(vehicle_id) => (vehicle_id, false),
                    TickKey::Probe(vehicle_id, _) => (vehicle_id, true),
                };
                match result {
                    Ok(TickPayload::Data(snapshot)) => {
                        report_successful_owner_api_request(env.collector_state);
                        let Some(vehicle) = run
                            .scheduler
                            .vehicles()
                            .into_iter()
                            .find(|vehicle| vehicle.id == vehicle_id)
                        else {
                            return Ok(ReadFlow::Continue);
                        };
                        let phase = poll_phase(&snapshot);
                        let eligible = sleep_eligible_with_policy(
                            &snapshot,
                            vehicle.settings.req_not_unlocked,
                        );
                        if probe {
                            // C3 R51, R52: a probe read that succeeded ends
                            // the hold; its snapshot is stored as any read's.
                            run.scheduler.probe_finished(
                                vehicle_id,
                                ProbeOutcome::Read(phase, eligible),
                                now,
                            );
                        } else {
                            run.scheduler
                                .vehicle_succeeded(vehicle_id, phase, eligible, now);
                        }
                        let received_at_ms = clock.wall_now_ms()?;
                        run.batch.snapshots.push(SnapshotItem {
                            vehicle,
                            snapshot,
                            received_at_ms,
                        });
                    }
                    Ok(TickPayload::State(_) | TickPayload::InService(_)) => {}
                    Err(error) => {
                        report_terminal_auth_failure(env.collector_state, &error);
                        if loses_sensitive_access(&error) {
                            // C3 R47, B2 clarification: as for the list; the run ends
                            // and `drive_requests` stores nothing.
                            return Err(error);
                        }
                        if matches!(error, CollectorError::FleetResendWithheld) {
                            // C3 R27, R47: the read's resend is not covered by the
                            // persisted reservation. It counts no failure and sets
                            // no wait; the car stays due and the raise rides on
                            // this tick's commit.
                            run.scheduler.request_withheld(vehicle_id);
                            run.scheduler
                                .resend_withheld(RequestCategory::Data, now);
                            return Ok(ReadFlow::Continue);
                        }
                        if fleet_credential_stop(&error) {
                            // C3 R47: a 401 that survived the tick's one refresh and
                            // resend. The account stops inside the loop and no
                            // further request is made: `drive_requests` stores what
                            // the tick received with the stop's gap, drops the
                            // requests still queued (never sent, never counted, still
                            // due) and clears every car's flag.
                            run.scheduler.enter_credential_stop(now);
                            run.scheduler.request_withheld(vehicle_id);
                            return Ok(ReadFlow::SignedOut);
                        }
                        if fleet_refresh_outcome_unknown(run.commit.store, &error) {
                            // C5 R47, C3 R47: an identified unresolved refresh stops
                            // the account in the loop, as a credential stop does.
                            run.scheduler.enter_fleet_refresh_stop(now);
                            run.scheduler.request_withheld(vehicle_id);
                            return Ok(ReadFlow::SignedOut);
                        }
                        if probe {
                            if run.scheduler.probe_finished(
                                vehicle_id,
                                ProbeOutcome::Failed(&error),
                                now,
                            ) {
                                push_service_refused_event(run, vehicle_id);
                            }
                            return Ok(ReadFlow::Continue);
                        }
                        let effect = run
                            .scheduler
                            .vehicle_failed_for_error(vehicle_id, &error, now);
                        // C3 R52: a car Tesla refuses as in service gets its
                        // Hub event in this tick's transaction, once per entry.
                        if effect == FailureEffect::ServiceEntered {
                            push_service_refused_event(run, vehicle_id);
                        }
                        if !is_vehicle_in_service(&error) {
                            run.batch.failures.push(VehicleCollectionFailure {
                                vehicle_id,
                                error: fleet_failure_as_owner_error(&error),
                            });
                        }
                    }
                }
                Ok(ReadFlow::Continue)
            },
        )
        .await?;
        tick_wrote = run.any_wrote;
    }
    Ok(FleetTickOutcome {
        requests_sent,
        tick_wrote,
    })
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
    counter: &RequestLedger,
    write_guard: &RunWriteGuard,
) -> Result<(), CollectorError> {
    const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
    tracing::info!("Fleet Telemetry push collection active; paid vehicle-data polling is disabled");
    let mut next_configuration_renewal = Instant::now();
    // The identified unresolved refresh is reported once per episode (as polling
    // mode's single attention transition is), and again only after it has cleared.
    let mut unknown_outcome_reported = false;
    loop {
        write_guard.check("telemetry-iteration")?;
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
                    counter,
                )
                .await
            };
            // This mode makes no ticks to carry the count of the configuration
            // requests (C3 R25), so they are written now; a store fault only delays
            // them to the next renewal, and the cap governs no request here.
            flush_telemetry_request_counts(store, counter, write_guard)?;
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
            match refresh {
                Err(error)
                    if matches!(error, FleetCredentialError::RotationOutcomeUnknown)
                        && store.identified_unresolved_fleet_refresh().unwrap_or(false) =>
                {
                    // C5 R47: an identified unresolved refresh sends nothing; the
                    // push collection and the lease go on until the owner
                    // reconnects and restarts. Only the token refresh is skipped
                    // (B2 round 3, I3-01): the Hub's own tail work below (address
                    // enrichment, export-outbox replay, the terrain wake) sends no
                    // Fleet request and goes on every maintenance interval.
                    if !unknown_outcome_reported {
                        unknown_outcome_reported = true;
                        tracing::warn!(
                            "Fleet token refresh outcome is unknown; no Fleet request is sent until the owner reconnects"
                        );
                    }
                }
                Err(error) => {
                    unknown_outcome_reported = false;
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
                Ok(()) => unknown_outcome_reported = false,
            }
        }
        if vehicles.is_empty() {
            sleep(MAINTENANCE_INTERVAL).await;
            continue;
        }
        let now_ms = current_epoch_millis()?;
        run_address_enrichment_once_guarded(
            store,
            config,
            cursor_key,
            &vehicles,
            now_ms,
            Some(admission),
            Some(write_guard),
        )
        .await?;
        replay_export_outbox_guarded(store, cursor_key, &vehicles, now_ms, false, Some(write_guard))
            .await?;
        let _ = terrain_wake.try_send(());
        sleep(MAINTENANCE_INTERVAL).await;
    }
}

/// Write the Fleet Telemetry maintenance loop's request counts (C3 R25). The flush is a
/// store mutation of its own, after the configuration work that preceded it awaited:
/// the run's write guard stands in front of it (B2 fix round, P3b fix 2, R-P3bF2-1),
/// and a refused write is the run's loss, not a fault of the counts. A store fault only
/// delays the counts to the next renewal.
fn flush_telemetry_request_counts(
    store: &HubStore,
    counter: &RequestLedger,
    write_guard: &RunWriteGuard,
) -> Result<(), CollectorError> {
    write_guard.check("telemetry-counter-flush")?;
    if let Err(error) = counter.flush_to_store(store) {
        tracing::warn!(error = %error, "Fleet request counts could not be written");
    }
    Ok(())
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

fn filter_configured_vehicles_with_identity(
    store: &HubStore,
    vehicles: Vec<Vehicle>,
    configured: &[(uuid::Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
    provider: CollectorProvider,
) -> Result<Vec<Vehicle>, CollectorError> {
    let identities = configured
        .iter()
        .map(|(vehicle_id, eid, settings)| {
            let (_, vin) = store
                .configured_tesla_vehicle_identity(*vehicle_id)?
                .ok_or(StoreError::LineageCatalogConflict)?;
            Ok((*eid, vin, settings))
        })
        .collect::<Result<Vec<_>, CollectorError>>()?;
    let mut filtered = Vec::new();
    for mut vehicle in vehicles {
        let matches = identities
            .iter()
            .filter(|(eid, vin, _)| {
                vehicle.id.get() == *eid as u64
                    || vin
                        .as_deref()
                        .filter(|vin| !vin.is_empty())
                        .is_some_and(|vin| vin.eq_ignore_ascii_case(&vehicle.vin))
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => continue,
            [(_, _, settings)] => vehicle.settings = (*settings).clone(),
            _ => return Err(StoreError::LineageCatalogConflict.into()),
        }
        if provider == CollectorProvider::Fleet {
            vehicle.settings.use_streaming_api = false;
        }
        filtered.push(vehicle);
    }
    Ok(filtered)
}
