// SPDX-License-Identifier: AGPL-3.0-only

// Collector-level tests of the C5 fix round (findings C5-B2, C5-S7, C5-S8, C5T-01,
// C5T-09 to C5T-12 and the Codex findings R07, R08 and R12 on this unit): the
// collector starts, takes its lease and shows the state when the credential unit
// stops before any request (an unresolved attempt, a configuration error, a pair
// that fails the grammar), the owner's retry reaches the unit in that state, what
// the loop does for the unit between requests, and what a tick request counts.
// Every test uses an in-process fake over loopback; nothing leaves the machine.
// Synthetic data only.

/// A temporary Hub with an imported car, a Cloak key and a fake Owner API, for the
/// tests that run the real loop over a credential unit built in a stopped state.
struct StopBed {
    temporary: tempfile::TempDir,
    store: HubStore,
    fake: AuthFake,
    base: url::Url,
    client: OwnerApi,
    server: tokio::task::JoinHandle<()>,
    /// The unit's clock: the real one plus what a test skips, so the minute between
    /// two reads of the stored generation (R44a) passes without a wait.
    env: Arc<SkippingEnv>,
}

/// A collector running over a credential unit, and the way to stop it.
struct RunningLoop {
    task: JoinHandle<Result<(), CollectorError>>,
    shutdown: oneshot::Sender<()>,
}

impl RunningLoop {
    async fn stop(self) {
        let Self { mut task, shutdown } = self;
        shutdown.send(()).expect("shutdown");
        join_supervised_restart_task("collector join", &mut task)
            .await
            .expect("collector result");
    }
}

impl StopBed {
    async fn new() -> Self {
        crate::crypto::install_default_provider();
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        seed_supervised_restart_import(&store);
        crate::teslamate_credentials::replace_key(temporary.path(), b"chain-cloak-key")
            .expect("0600 Cloak key");
        let (fake, base, client, server) = auth_fake().await;
        Self {
            temporary,
            store,
            fake,
            base,
            client,
            server,
            env: Arc::new(SkippingEnv::default()),
        }
    }

    /// Store a pair as the owner would (an external write), with its generation and
    /// a schedule that is not due. Returns the generation.
    fn store_pair(&self, access: &str, refresh: &str) -> Uuid {
        let key = crate::teslamate_credentials::load_key(self.temporary.path()).expect("Cloak key");
        let tokens =
            OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned()).expect("pair");
        let generation = crate::teslamate_token::legacy_refresh_credential_generation(&tokens);
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
                .expect("Cloak pair");
        let (expires, next) = not_due_schedule();
        self.store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, expires, next)
                    .expect("a schedule that is not due")
                    .with_credential_generation(generation)
                    .expect("generation"),
            )
            .expect("store pair");
        generation
    }

    /// The credential unit as the collector's start path builds it.
    fn unit(&self, region: Option<StreamRegion>) -> LegacyAuthManager {
        LegacyAuthManager::from_hub_teslamate_store_for_test(
            self.store.clone(),
            self.temporary.path(),
            &crate::legacy_auth::AuthEndpointConfig {
                mode: crate::legacy_auth::AuthMode::Tesla,
                region,
                token_url: None,
                allow_loopback_http: true,
            },
            Some(self.base.join("oauth2/v3/token").expect("token endpoint")),
            false,
            Arc::clone(&self.env) as Arc<dyn crate::legacy_auth::attempt::AuthEnv>,
        )
        .expect("the unit starts, in whatever state the store puts it")
    }

    /// Run the collector over `manager` and wait for its readiness: the signal the
    /// service waits for before it opens the server (R43).
    async fn run(&self, manager: LegacyAuthManager) -> RunningLoop {
        let region = manager.region();
        let auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region,
        };
        let config = supervised_restart_test_config(self.temporary.path());
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task_store = self.store.clone();
        let client = self.client.clone();
        let mut task = tokio::spawn(async move {
            run_supervised_with_access(
                &task_store,
                &config,
                supervised_restart_test_cadence(),
                client,
                auth,
                "ws://127.0.0.1:9/streaming/".to_owned(),
                CursorKey::from_bytes([0xB2; 32]),
                Some(ready_tx),
                RunWriteGuard::for_run(None),
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        });
        let _cursor =
            wait_for_supervised_signal_or_abort("collector readiness", &mut task, ready_rx).await;
        RunningLoop {
            task,
            shutdown: shutdown_tx,
        }
    }

    fn lease_state(&self) -> String {
        self.store
            .open()
            .expect("catalogue")
            .query_row("SELECT state FROM supervised_collector_lease", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap_or_default()
    }

    fn auth_row(&self) -> Option<crate::db::CollectorAuthRow> {
        self.store.load_collector_auth_row().expect("row")
    }

    fn auth_state(&self) -> Option<(String, Option<String>)> {
        self.auth_row().map(|row| (row.state, row.reason))
    }
}

impl Drop for StopBed {
    fn drop(&mut self) {
        self.server.abort();
    }
}

// ---- C5-B2, spec test 68: an unresolved attempt at start --------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t68_an_unresolved_attempt_at_start_does_not_stop_the_collector_from_starting() {
    // The service waits for the collector's readiness before it opens the server
    // (R43): a collector that failed to start for this state would end the run
    // before it, and the service would restart in a loop without showing why. The
    // unit is built in its stopped state instead, so the collector starts, takes its
    // lease, sends nothing and shows `signed_out(refresh_outcome_unknown)`.
    let bed = StopBed::new().await;
    let generation = bed.store_pair("loop-access", "loop-refresh");
    // The previous run sent a refresh and never learned the outcome.
    bed.store
        .begin_legacy_refresh(generation)
        .expect("an attempt");
    assert!(
        bed.store
            .has_unresolved_legacy_refresh()
            .expect("unresolved")
    );
    let manager = bed.unit(Some(StreamRegion::Global));
    let mut running = bed.run(manager).await;
    k6_wait_for(&mut running.task, "the terminal lease", || {
        bed.lease_state() == "auth_terminal"
    })
    .await;
    assert_eq!(
        bed.auth_state(),
        Some((
            "signed_out".to_owned(),
            Some("refresh_outcome_unknown".to_owned())
        ))
    );
    assert_eq!(
        bed.store.collector_auth_readiness().expect("readiness"),
        Some(crate::db::CollectorAuthReadiness::SignedOut)
    );
    sleep(Duration::from_millis(1_500)).await;
    assert_eq!(bed.fake.owner_requests(), 0, "nothing is sent");
    assert_eq!(
        bed.fake.token_requests(),
        0,
        "the attempt is never repeated"
    );
    assert!(!running.task.is_finished(), "the collector keeps running");
    k6_wait_for(&mut running.task, "the stop's gap", || {
        open_auth_terminal_gaps(&bed.store) == 1
    })
    .await;
    // An external write resolves the attempt (K4): the stop lifts within the minute
    // the unit re-reads the stored generation, and collection resumes.
    bed.store_pair("new-access", "new-refresh");
    bed.env.skipped.fetch_add(61, Ordering::SeqCst);
    k6_wait_for(&mut running.task, "the list after the new login", || {
        bed.fake.owner_requests() >= 1
    })
    .await;
    k6_wait_for(&mut running.task, "an active lease", || {
        bed.lease_state() == "active"
    })
    .await;
    assert_eq!(bed.fake.token_requests(), 0, "the new pair is not due");
    running.stop().await;
}

// ---- C5-B2, spec test 72: a configuration error ----------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t72_a_configuration_error_keeps_the_collector_running_and_is_not_a_stored_stop() {
    let bed = StopBed::new().await;
    // An opaque token and no configured region: R5 (`region_required`).
    bed.store_pair("opaque-access-0001", "opaque-refresh-0001");
    let manager = bed.unit(None);
    let mut running = bed.run(manager).await;
    k6_wait_for(&mut running.task, "the status row", || {
        bed.auth_state()
            == Some((
                "configuration_error".to_owned(),
                Some("region_required".to_owned()),
            ))
    })
    .await;
    // The lease stays active (R52), readiness names the configuration, and nothing is
    // sent.
    assert_eq!(bed.lease_state(), "active");
    assert_eq!(
        bed.store.collector_auth_readiness().expect("readiness"),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthConfigurationError,
        ))
    );
    sleep(Duration::from_millis(1_500)).await;
    assert_eq!(bed.fake.owner_requests(), 0);
    assert_eq!(bed.fake.token_requests(), 0);
    assert!(!running.task.is_finished(), "the collector keeps running");
    assert!(
        bed.auth_row().expect("row").stopped_generation.is_none(),
        "a configuration error is not stored as a stop (R5)"
    );
    running.stop().await;
    // The key removed from the configuration (a region is given) and the collector
    // started again: ready, one list is sent, and no owner action was needed.
    let manager = bed.unit(Some(StreamRegion::Global));
    let mut running = bed.run(manager).await;
    k6_wait_for(&mut running.task, "the list", || {
        bed.fake.owner_requests() >= 1
    })
    .await;
    k6_wait_for(&mut running.task, "the ready row", || {
        bed.auth_state().is_some_and(|(state, _)| state == "ready")
    })
    .await;
    assert_eq!(bed.lease_state(), "active");
    running.stop().await;
}

// ---- C5-B2, R46: a stored pair that fails the grammar -----------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r46_a_stored_pair_that_fails_the_grammar_is_a_terminal_lease_on_a_running_collector() {
    let bed = StopBed::new().await;
    // A pasted pair with a space inside the access token: storable, never sendable.
    bed.store_pair("access with a space", "refresh-0001");
    let manager = bed.unit(Some(StreamRegion::Global));
    let mut running = bed.run(manager).await;
    k6_wait_for(
        &mut running.task,
        "the terminal lease (section 5.1)",
        || bed.lease_state() == "auth_terminal",
    )
    .await;
    assert_eq!(
        bed.auth_state(),
        Some((
            "signed_out".to_owned(),
            Some("stored_pair_invalid".to_owned())
        ))
    );
    sleep(Duration::from_millis(1_000)).await;
    assert_eq!(bed.fake.owner_requests(), 0);
    assert_eq!(bed.fake.token_requests(), 0);
    // A valid pair stored by the owner lifts it, with no restart.
    bed.store_pair("valid-access-0002", "valid-refresh-0002");
    bed.env.skipped.fetch_add(61, Ordering::SeqCst);
    k6_wait_for(&mut running.task, "the list after a valid pair", || {
        bed.fake.owner_requests() >= 1
    })
    .await;
    running.stop().await;
}

// ---- C5-S7, spec test 94 (d): the owner's retry over a refusal and an unresolved attempt ------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t94d_the_owner_retry_turns_a_restored_refusal_over_an_unresolved_attempt_into_a_sign_out()
{
    use crate::legacy_auth::{
        RefusalReason,
        state::{AuthStatus, AuthStop, FuseSnapshot, StoppedMode},
    };

    let bed = StopBed::new().await;
    let generation = bed.store_pair("loop-access", "loop-refresh");
    bed.store
        .begin_legacy_refresh(generation)
        .expect("an attempt");
    // The unit's own refusal from the previous run, with the Tesla mode.
    let now = epoch_seconds_of(SystemTime::now());
    let refused = AuthStatus::stopped(
        AuthStop::ApiRefused(RefusalReason::OwnerApi),
        generation,
        StoppedMode::Legacy,
        false,
        FuseSnapshot::default(),
        false,
    );
    bed.store
        .write_collector_auth_row(&refused.to_row(), now * 1_000)
        .expect("stored refusal");
    let manager = bed.unit(Some(StreamRegion::Global));
    let mut running = bed.run(manager).await;
    // The refusal shows first and nothing is sent; the lease is not terminal for a
    // refusal (R52).
    k6_wait_for(&mut running.task, "the refusal", || {
        bed.auth_state() == Some(("api_refused".to_owned(), Some("owner_api".to_owned())))
    })
    .await;
    sleep(Duration::from_millis(1_200)).await;
    assert_eq!(bed.fake.owner_requests(), 0);
    assert_eq!(bed.lease_state(), "active");
    // The owner's retry control reaches the unit through the loop, as in production:
    // the refusal lifts, and the attempt that was never resolved stops the account
    // again, as a sign-out, with the lease terminal and no token request.
    bed.store
        .insert_collector_owner_action(
            "retry_collection",
            None,
            current_epoch_millis().expect("clock"),
        )
        .expect("retry row");
    k6_wait_for(&mut running.task, "the sign-out", || {
        bed.auth_state()
            == Some((
                "signed_out".to_owned(),
                Some("refresh_outcome_unknown".to_owned()),
            ))
    })
    .await;
    k6_wait_for(&mut running.task, "the terminal lease", || {
        bed.lease_state() == "auth_terminal"
    })
    .await;
    assert_eq!(bed.fake.token_requests(), 0, "no token request");
    assert_eq!(bed.fake.owner_requests(), 0);
    assert!(bed.auth_row().expect("row").stopped_mode.is_none());
    running.stop().await;
}

// ---- C5T-01, Codex R08: what a tick request counts, and when the unit decides -----------------------

fn data_sent(ledger: &RequestLedger) -> u64 {
    ledger.view().requests[RequestCategory::Data.index()].1
}

fn state_sent(ledger: &RequestLedger) -> u64 {
    ledger.view().requests[RequestCategory::State.index()].1
}

#[tokio::test]
async fn c5t01_a_wait_leaves_the_probe_the_state_check_and_the_read_each_unsent_and_uncounted() {
    // CT72 (c), case 1, for each of the three request builders of the tick: the unit
    // says `For(90 s)` when the tick starts, so nothing is sent or counted, and the
    // error is the unit's own, which the scheduler classifies as a wait.
    let (fake, base, client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    {
        let CollectionAuth::Legacy { manager, .. } = &auth;
        manager.lock().await.auth_for_test().hold_until =
            Some(epoch_seconds_of(SystemTime::now()) + 90);
    }
    let ledger = Core::owner().scheduler.request_ledger();
    let car = VehicleId::from_test(1);
    let counted = |category| {
        Some(RequestAccounting {
            ledger: &ledger,
            category,
        })
    };
    let waits = |error: CollectorError| {
        matches!(
            legacy_auth_error_of(&error),
            Some(LegacyAuthError::AuthUnavailable { retry_at: Some(_) })
        )
    };
    assert!(waits(
        vehicle_probe_counted(&client, &auth, car, counted(RequestCategory::State))
            .await
            .expect_err("held")
    ));
    assert!(waits(
        vehicle_state_counted(&client, &auth, car, counted(RequestCategory::State))
            .await
            .expect_err("held")
    ));
    assert!(waits(
        vehicle_data_counted(&client, &auth, car, None, counted(RequestCategory::Data))
            .await
            .expect_err("held")
    ));
    assert!(waits(
        list_vehicles_counted(&client, &auth, counted(RequestCategory::State))
            .await
            .expect_err("held")
    ));
    assert_eq!(fake.owner_requests(), 0, "nothing reached Tesla");
    assert_eq!(state_sent(&ledger), 0);
    assert_eq!(data_sent(&ledger), 0);
    server.abort();
}

/// One counted request of each builder the tick uses, by number: the list, a car's
/// record probe, a car's record check and a car's read.
async fn counted_request(
    kind: u8,
    client: &OwnerApi,
    auth: &CollectionAuth,
    ledger: &RequestLedger,
) -> Result<(), CollectorError> {
    let car = VehicleId::from_test(1);
    let counted = |category| {
        Some(RequestAccounting {
            ledger,
            category,
        })
    };
    match kind {
        0 => list_vehicles_counted(client, auth, counted(RequestCategory::State))
            .await
            .map(|_| ()),
        1 => vehicle_probe_counted(client, auth, car, counted(RequestCategory::State))
            .await
            .map(|_| ()),
        2 => vehicle_state_counted(client, auth, car, counted(RequestCategory::State))
            .await
            .map(|_| ()),
        _ => vehicle_data_counted(client, auth, car, None, counted(RequestCategory::Data))
            .await
            .map(|_| ()),
    }
}

#[tokio::test]
async fn r08_a_request_that_queued_behind_another_meets_a_pause_at_its_own_start_and_is_not_counted()
 {
    // CT72 (c), case 2, for each builder of the tick: a pause that begins while a
    // request is queued behind the one that holds the unit leaves the queued request
    // unsent and uncounted. A request is not counted when it is planned with the
    // others, but when the unit really sends it.
    for kind in 0..4_u8 {
        let (fake, base, client, server) = auth_fake().await;
        let auth = auth_for(&base, false);
        let ledger = Core::owner().scheduler.request_ledger();
        let CollectionAuth::Legacy { manager, .. } = &auth;
        let mut holder = manager.lock().await;
        let queued = tokio::spawn({
            let (client, auth, ledger) = (client.clone(), auth.clone(), ledger.clone());
            async move { counted_request(kind, &client, &auth, &ledger).await }
        });
        // The queued request asked while the unit was busy, which is `Ready` (it
        // queues); now the request that holds the unit starts a confirmation pause.
        sleep(Duration::from_millis(100)).await;
        holder.auth_for_test().hold_until = Some(epoch_seconds_of(SystemTime::now()) + 300);
        drop(holder);
        let error = queued
            .await
            .expect("task")
            .expect_err("held at its own start");
        assert!(
            matches!(
                legacy_auth_error_of(&error),
                Some(LegacyAuthError::AuthUnavailable { retry_at: Some(_) })
            ),
            "kind {kind}: {error:?}"
        );
        assert_eq!(fake.owner_requests(), 0, "kind {kind}");
        assert_eq!(state_sent(&ledger), 0, "kind {kind}: never sent, never counted");
        assert_eq!(data_sent(&ledger), 0, "kind {kind}");
        server.abort();
    }
}

#[tokio::test]
async fn r08_each_request_the_unit_sends_is_counted_once_at_its_send_the_resend_included() {
    let (fake, base, client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    let ledger = Core::owner().scheduler.request_ledger();
    let counted = || {
        Some(RequestAccounting {
            ledger: &ledger,
            category: RequestCategory::State,
        })
    };
    list_vehicles_counted(&client, &auth, counted())
        .await
        .expect("a list");
    assert_eq!(state_sent(&ledger), 1);
    // The stored token is refused: the unit counts the rejection, refreshes, and sends
    // the list once more with the new token. Two lists were sent, so two are counted.
    fake.rejected
        .lock()
        .unwrap()
        .insert("stored-access".to_owned());
    list_vehicles_counted(&client, &auth, counted())
        .await
        .expect("a list after the refresh");
    assert_eq!(fake.owner_requests(), 3);
    assert_eq!(fake.token_requests(), 1);
    assert_eq!(state_sent(&ledger), 3);
    // Every start was completed when its request ended: none is left in flight.
    assert_eq!(
        ledger.lock().counts[RequestCategory::State.index()].in_flight,
        0
    );
    server.abort();
}

// ---- C5T-09: a stream `Authenticated` leaves the fuse as it is ---------------------------------------

#[tokio::test]
async fn c5t09_a_stream_authenticated_transition_does_not_reset_a_counted_rejection() {
    let (fake, base, client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    fake.reject_all.store(true, Ordering::SeqCst);
    refresh_after_stream_authentication_rejection(
        &client,
        &auth,
        StreamAuthenticationTransition::Rejected,
    )
    .await
    .expect("handled by the unit");
    assert_eq!(fuse_count(&auth).await, 1);
    // The withdrawn rule of revision 1 reset the fuse here. Without a generation on
    // the transition that could credit a superseded token, and an Owner API 2xx
    // follows soon on a working account (R40).
    refresh_after_stream_authentication_rejection(
        &client,
        &auth,
        StreamAuthenticationTransition::Authenticated,
    )
    .await
    .expect("nothing to do");
    assert_eq!(fuse_count(&auth).await, 1, "unchanged");
    server.abort();
}

// ---- Codex R07: the loop writes what the unit owes between requests ---------------------------------

#[tokio::test]
async fn r07_the_loops_maintenance_writes_a_successor_without_a_request_or_a_wait_to_end() {
    // A refresh whose write failed: the previous pair is still usable, so the unit is
    // `Ready` and nothing asks for the write. The loop's maintenance retries it when its
    // time has come, without any request.
    let (fake, base, client, server) = auth_fake().await;
    let writes = Arc::new(AtomicUsize::new(0));
    let failing = Arc::new(AtomicBool::new(true));
    // The schedule's refresh came due ten seconds ago.
    let now = epoch_seconds_of(SystemTime::now());
    let auth_pair = crate::legacy_auth::LegacyAuth::for_test(
        base.join("oauth2/v3").unwrap(),
        "stored-access",
        "stored-refresh",
    )
    .with_test_schedule(now + 28_800, now - 10);
    let env = Arc::new(SkippingEnv::default());
    let manager = LegacyAuthManager::for_test_with_env(
        auth_pair,
        {
            let (writes, failing) = (Arc::clone(&writes), Arc::clone(&failing));
            Arc::new(move |_, _| {
                writes.fetch_add(1, Ordering::SeqCst);
                if failing.load(Ordering::SeqCst) {
                    Err(CredentialError::LegacyTokenStateWrite)
                } else {
                    Ok(())
                }
            })
        },
        Arc::clone(&env) as Arc<dyn crate::legacy_auth::attempt::AuthEnv>,
    );
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    };
    // The first request refreshes (an imported pair) and the write fails.
    list_vehicles_counted(&client, &auth, None)
        .await
        .expect("a list");
    assert_eq!(fake.token_requests(), 1);
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    let CollectionAuth::Legacy { manager, .. } = &auth;
    assert_eq!(
        manager.lock().await.status_now().state,
        AuthState::PersistencePending
    );
    // The store is back, and five seconds pass: the loop's maintenance alone writes.
    failing.store(false, Ordering::SeqCst);
    maintain_legacy_credential(&client, &auth).await;
    assert_eq!(writes.load(Ordering::SeqCst), 1, "not due yet");
    env.skipped.fetch_add(5, Ordering::SeqCst);
    let requests_before = fake.owner_requests();
    maintain_legacy_credential(&client, &auth).await;
    assert_eq!(
        writes.load(Ordering::SeqCst),
        2,
        "written by the maintenance"
    );
    assert_eq!(manager.lock().await.status_now().state, AuthState::Ready);
    assert_eq!(
        fake.owner_requests(),
        requests_before,
        "no request was made"
    );
    assert_eq!(fake.token_requests(), 1, "no second token request");
    server.abort();
}

// ---- Codex R12: the shutdown flag goes up before the manager is awaited -----------------------------

#[tokio::test]
async fn r12_shutdown_raises_the_flag_before_it_waits_for_a_manager_a_task_still_holds() {
    let (_fake, base, _client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    let CollectionAuth::Legacy {
        manager, refresh, ..
    } = &auth;
    let flag = manager.lock().await.shutdown_flag();
    refresh.register_shutdown(Arc::clone(&flag));
    // A stream's credential check owns the manager when the collector's shutdown begins.
    let holder = manager.lock().await;
    let shutdown = tokio::spawn({
        let auth = auth.clone();
        async move { shutdown_legacy_refresh(&auth).await }
    });
    sleep(Duration::from_millis(100)).await;
    assert!(
        flag.load(Ordering::SeqCst),
        "no new token request may start while the shutdown waits for the lock"
    );
    assert!(!shutdown.is_finished(), "it waits for the holder");
    drop(holder);
    shutdown
        .await
        .expect("shutdown finishes")
        .expect("the shutdown keeps its admission");
    server.abort();
}

// ---- C5-S4: the confirmation's answer reaches the collector's own classification ---------------------

#[tokio::test]
async fn s4_a_429_from_the_confirmation_read_sets_the_account_wait_with_its_retry_after() {
    let (_fake, base, _client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    {
        let CollectionAuth::Legacy { fuse, .. } = &auth;
        fuse.lock()
            .await
            .note_confirmation_error(OwnerApiError::RateLimited {
                retry_after_seconds: 120,
            });
    }
    let mut core = ep_online_core(1);
    let now = core.at(0);
    apply_confirmation_answers(
        take_confirmation_outcomes(&auth).await.answers,
        &mut core.scheduler,
        now,
    );
    assert_eq!(
        core.scheduler.account.wait,
        Some((now + Duration::from_secs(120), WaitReason::RateLimited))
    );
    // Read once: nothing is applied twice.
    let CollectionAuth::Legacy { fuse, .. } = &auth;
    assert!(fuse.lock().await.take_confirmation_errors().is_empty());
    server.abort();
}

#[tokio::test]
async fn s4_a_fleet_named_403_from_the_confirmation_read_is_the_accounts_refusal() {
    let (_fake, base, _client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    {
        let CollectionAuth::Legacy { fuse, .. } = &auth;
        fuse.lock()
            .await
            .note_confirmation_error(OwnerApiError::Forbidden {
                fleet_api_named: true,
            });
    }
    let mut core = Core::owner();
    let now = core.at(0);
    apply_confirmation_answers(
        take_confirmation_outcomes(&auth).await.answers,
        &mut core.scheduler,
        now,
    );
    assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
    server.abort();
}

// ---- P2 (B2 fix round): credential conditions are not the loss of the admission ---------------------

impl FleetBed {
    /// The credential manager is built with the process's retained admission, which
    /// stays valid: a Fleet credential condition then is not the admission's failure.
    fn admit_manager(&mut self) -> Arc<crate::hub_user_process::AdmittedUserHub> {
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(self._dir.path())
            .expect("admit");
        let manager = FleetAuthManager::from_store_for_admitted_user(
            self.store.clone(),
            self._dir.path(),
            Arc::clone(&admission),
            None,
        )
        .expect("manager");
        self.manager = Arc::new(tokio::sync::Mutex::new(manager));
        admission
    }

    fn lease_state(&self) -> SupervisedCollectorState {
        *self._collector_state_rx.borrow()
    }
}

#[tokio::test]
async fn fleet_credential_errors_are_not_admission_loss() {
    use FleetCredentialError as Condition;
    // A Fleet credential that is missing, lacks its key, scopes or claims, or whose
    // refresh outcome is unknown is a credential condition with the admission intact:
    // it is no loss of the retained admission and publishes no terminal lease, and the
    // classifier gives it the authentication handling. Only the denial itself is the
    // loss, which ends the run.
    let conditions = vec![
        Condition::Missing,
        Condition::KeyMissing,
        Condition::MigrationRequired,
        Condition::UnsafeKeyMaterial,
        Condition::InvalidAccessTokenClaims,
        Condition::MissingCollectionScopes,
        Condition::RotationOutcomeUnknown,
        Condition::Store(StoreError::NilVehicleId),
        Condition::KeyIo {
            operation: "reading",
            path: std::path::PathBuf::from("key"),
            source: std::io::Error::other("unreadable"),
        },
    ];
    for condition in conditions {
        assert!(!condition.is_admission_loss(), "{condition:?}");
        let error = CollectorError::FleetCredential(condition);
        assert!(!loses_sensitive_access(&error), "{error:?}");
        assert!(!is_terminal_auth_failure(&error), "{error:?}");
        assert_eq!(
            classify(
                &error,
                RequestKind::Read,
                crate::config::CollectorMode::FleetPolling
            ),
            ErrorClass::Auth,
            "{error:?}"
        );
    }
    let denial = CollectorError::FleetCredential(Condition::SensitiveAccessUnavailable);
    assert!(loses_sensitive_access(&denial));
    assert!(is_terminal_auth_failure(&denial));
    assert_eq!(
        classify(
            &denial,
            RequestKind::Read,
            crate::config::CollectorMode::FleetPolling
        ),
        ErrorClass::Transient
    );

    // The credential condition in the tick, admission intact: a refresh Tesla answered
    // with an error fences the stored pair; the next request meets the fence. The
    // account stops inside the loop, the run goes on, the lease stays active, nothing
    // more is sent, and the stop is one decision event that carries the attention word.
    let log = DecisionLog::start();
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    let _admission = bed.admit_manager();
    bed.allow_refresh = true;
    bed.manager.lock().await.mark_refresh_due();
    bed.fake.token_status.store(400, Ordering::SeqCst);
    bed.try_tick().await.expect("the failed exchange does not end the run");
    assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 1);
    assert_eq!(bed.lease_state(), SupervisedCollectorState::Active);
    assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
    bed.advance(Duration::from_secs(3_600));
    bed.try_tick().await.expect("the fence does not end the run");
    assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
    assert!(bed.store.identified_unresolved_fleet_refresh().expect("receipt"));
    assert_eq!(bed.lease_state(), SupervisedCollectorState::Active, "no terminal lease");
    assert_eq!(
        log.lines("INFO", "collector_fleet_refresh_outcome_unknown"),
        1,
        "{}",
        log.contents()
    );
    // No further traffic of any kind, and no second transition entry.
    let wire = (bed.lists(), bed.reads(), bed.fake.token_requests.load(Ordering::SeqCst));
    for _ in 0..2 {
        bed.advance(Duration::from_secs(3_600));
        bed.try_tick().await.expect("the stopped account goes round");
    }
    assert_eq!(
        (bed.lists(), bed.reads(), bed.fake.token_requests.load(Ordering::SeqCst)),
        wire
    );
    assert_eq!(log.lines("INFO", "collector_fleet_refresh_outcome_unknown"), 1);
    assert_eq!(bed.lease_state(), SupervisedCollectorState::Active);

    // The control: the denial of the retained admission itself exits, and its lease
    // is the terminal one.
    let mut denied =
        FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    denied.lose_sensitive_access().await;
    let error = denied.try_tick().await.expect_err("an actual denial exits");
    assert!(loses_sensitive_access(&error), "{error:?}");
    assert_eq!(denied.lease_state(), SupervisedCollectorState::AuthenticationTerminal);
    assert_eq!(denied.scheduler.account.state, AccountState::Collecting);
}

/// The stored state word of the lease row.
fn lease_state_text(store: &HubStore) -> String {
    store
        .open()
        .expect("catalogue")
        .query_row("SELECT state FROM supervised_collector_lease", [], |row| row.get(0))
        .expect("the lease row")
}

/// Replace the data directory's lifetime lock file by another file: the retained
/// admission of the process then fails its next revalidation, as when another
/// process has taken the instance over.
fn invalidate_the_retained_admission(data_dir: &std::path::Path) {
    use std::os::unix::fs::OpenOptionsExt;
    let lock = data_dir.join(crate::user_lifetime_lock::LOCK_FILE_NAME);
    std::fs::remove_file(&lock).expect("remove the lock file");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock)
        .expect("a different lock file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_first_admission_loss_skips_shutdown_writes_and_settlement() {
    // The retained handle is invalidated while the Fleet collector holds a token
    // exchange in flight. The supervisor ends on the loss first: no final
    // transaction, no heartbeat, no lease state, no release, no `hub_stopped` or
    // `auth_terminal` gap, and no wait for the held exchange to settle (its request
    // timeout is far beyond this test, so a settlement would outlast it).
    let mut bed = FleetLoopBed::start_with_auth_timeout(
        "asleep",
        false,
        true,
        Duration::from_secs(120),
    )
    .await;
    let lists_seen = Arc::clone(&bed.fake.lists);
    bed.wait_for("the first lists", || lists_seen.load(Ordering::SeqCst) >= 2).await;
    // The next request starts an exchange the token endpoint holds.
    bed.fake.token_hold.store(true, Ordering::SeqCst);
    bed.manager.lock().await.mark_refresh_due();
    let tokens = Arc::clone(&bed.fake.token_requests);
    bed.wait_for("the held exchange", || tokens.load(Ordering::SeqCst) >= 1).await;
    invalidate_the_retained_admission(bed.temporary.path());
    // Everything the run could still write is read after the invalidation: nothing
    // may change from here.
    let lease_at_loss = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the active lease");
    let gaps_at_loss = bed.store.collection_gaps(50).expect("gaps").len();
    let commits_at_loss = bed.seam.tick_commits();
    let account = |store: &HubStore| -> Option<(String, String, i64)> {
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT state, reason_code, updated_at_ms FROM collector_account_state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok()
    };
    let account_at_loss = account(&bed.store);
    let mut task = bed.task.take().expect("task");
    // The run ends by itself. The 20 s is only the watchdog of a stall (a settlement
    // of the held exchange would outlast it); what is proved is the acknowledged
    // events below, not a span of time.
    let result = timeout(Duration::from_secs(20), &mut task)
        .await
        .expect("the supervisor ends on the loss, not after a settlement")
        .expect("join");
    assert!(
        !bed.seam.loop_log().contains(&LoopEvent::FleetSettlementBegun),
        "the shutdown never began waiting for the held exchange to settle: {:?}",
        bed.seam.loop_log()
    );
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    let lease = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the lease is retained to expire");
    assert_eq!(lease.instance_id, lease_at_loss.instance_id);
    assert_eq!(lease.heartbeat_at_ms, lease_at_loss.heartbeat_at_ms, "no heartbeat after the loss");
    assert_eq!(lease.lease_until_ms, lease_at_loss.lease_until_ms);
    assert_eq!(lease_state_text(&bed.store), "active", "no lease state written");
    let gaps = bed.store.collection_gaps(50).expect("gaps");
    assert_eq!(gaps.len(), gaps_at_loss, "no gap row: {gaps:?}");
    assert!(gaps.iter().all(|gap| gap.kind != "auth_terminal" && gap.kind != "hub_stopped"));
    assert_eq!(bed.seam.tick_commits(), commits_at_loss, "no final transaction");
    assert_eq!(account(&bed.store), account_at_loss, "no account row written");

    // The held exchange is let go so nothing outlives the test. (That the detached
    // exchange then also refuses to persist its successor is proved with the
    // credential persistence in part P3b.)
    bed.fake.token_hold.store(false, Ordering::SeqCst);
    bed.server.abort();
}

#[tokio::test]
async fn fleet_runtime_refresh_stop_survives_failed_commit_once() {
    // The account is collecting when a refresh Tesla answered with an error fences the
    // stored pair, and the request that meets the fence is an identified unresolved
    // refresh: the account stops inside the loop. The commit of that tick fails. The
    // stop stands (it is derived, never stored), so nothing is sent and the one
    // reconnect transition is not entered again while the store recovers.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let witness = Arc::clone(&seam);
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async move {
            let log = DecisionLog::start();
            let mut bed =
                FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
            let _admission = bed.admit_manager();
            bed.allow_refresh = true;
            bed.manager.lock().await.mark_refresh_due();
            bed.fake.token_status.store(400, Ordering::SeqCst);
            bed.try_tick().await.expect("the failed exchange does not end the run");
            assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
            bed.advance(Duration::from_secs(3_600));
            witness.set_tick_commit_fault(true);
            bed.try_tick().await.expect("the fence does not end the run");
            assert!(bed.store.identified_unresolved_fleet_refresh().expect("receipt"));
            assert_eq!(
                bed.scheduler.account.state,
                AccountState::CredentialStop,
                "the failed commit does not undo the stop"
            );
            assert_eq!(
                log.lines("INFO", "collector_fleet_refresh_outcome_unknown"),
                1,
                "{}",
                log.contents()
            );
            // The fault lasts, then is repaired: no traffic and no second transition.
            let wire = (bed.lists(), bed.reads(), bed.fake.token_requests.load(Ordering::SeqCst));
            for _ in 0..3 {
                bed.advance(Duration::from_secs(3_600));
                bed.try_tick().await.expect("the stopped account goes round");
            }
            witness.set_tick_commit_fault(false);
            for _ in 0..2 {
                bed.advance(Duration::from_secs(3_600));
                bed.try_tick().await.expect("the stopped account goes round");
            }
            assert_eq!(
                (bed.lists(), bed.reads(), bed.fake.token_requests.load(Ordering::SeqCst)),
                wire,
                "no traffic of any kind"
            );
            assert_eq!(
                log.lines("INFO", "collector_fleet_refresh_outcome_unknown"),
                1,
                "{}",
                log.contents()
            );
            assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
            assert_eq!(bed.lease_state(), SupervisedCollectorState::Active);
        })
        .await;
}

// ---- B2 P3: an identified unresolved Fleet refresh starts as a running credential stop -------------

/// Every request that reaches the Fleet fake, whatever its method or path. The fake
/// answers 404: the tests that use it expect nothing to arrive.
#[derive(Clone, Default)]
struct FleetTripwire {
    requests: Arc<Mutex<Vec<String>>>,
}

impl FleetTripwire {
    fn seen(&self) -> Vec<String> {
        self.requests.lock().expect("requests").clone()
    }
}

async fn fleet_tripwire_record(
    State(tripwire): State<FleetTripwire>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    tripwire
        .requests
        .lock()
        .expect("requests")
        .push(format!("{} {}", request.method(), request.uri().path()));
    next.run(request).await
}

/// A Fleet hub whose stored pair is sound and whose refresh has an unresolved,
/// identified outcome (a started token receipt bound to the pair, with its input
/// fence): the pair, key, cipher, generation, claims and scopes all hold.
fn p3_unresolved_fleet_hub() -> (tempfile::TempDir, HubStore, CursorKey, Uuid) {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    cadence_wait_seed_car(&store);
    let generation = p3_add_unresolved_fleet_refresh(&store, temporary.path());
    (temporary, store, cursor_key, generation)
}

/// Add a sound Fleet pair to `store` and begin a refresh that never completes.
fn p3_add_unresolved_fleet_refresh(store: &HubStore, data_dir: &std::path::Path) -> Uuid {
    crate::teslamate_credentials::load_or_create_cursor_key(data_dir).expect("cursor key");
    crate::fleet_credentials::persist_fleet_setup_credentials(
        store,
        data_dir,
        &FleetSetupCredentials::new(
            CT67_SCOPED_ACCESS.to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("setup credentials"),
        SystemTime::now(),
    )
    .expect("persist the pair");
    let generation = store
        .load_fleet_tokens()
        .expect("tokens")
        .expect("a pair")
        .credential_generation()
        .expect("a bound generation");
    store.begin_fleet_refresh(generation).expect("an unresolved refresh");
    generation
}

fn p3_month_now() -> String {
    let now = ::time::OffsetDateTime::now_utc();
    format!("{:04}-{:02}", now.year(), u8::from(now.month()))
}

/// What the evidence of the unresolved refresh and the stored pair look like, as
/// text, so a test can show a run changed neither.
fn p3_fleet_evidence(store: &HubStore) -> String {
    let connection = store.open().expect("catalogue");
    let receipts: Vec<(i64, String)> = connection
        .prepare("SELECT id, outcome FROM outbound_request_receipts ORDER BY id")
        .expect("receipts")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("receipt rows")
        .collect::<Result<_, _>>()
        .expect("receipt values");
    let fences: i64 = connection
        .query_row("SELECT COUNT(*) FROM fleet_refresh_input_fences", [], |row| row.get(0))
        .expect("fences");
    let pair = store.load_fleet_tokens().expect("tokens").expect("pair");
    format!(
        "{receipts:?}|{fences}|{:?}|{:?}|{}|{}|{:?}",
        pair.access(),
        pair.refresh(),
        pair.expires_at(),
        pair.next_refresh_at(),
        pair.credential_generation().expect("generation")
    )
}

/// The Fleet cadence of these tests: the default intervals, with the cap given.
fn p3_fleet_cadence(monthly_cap_cents: u32) -> CollectorCadence {
    CollectorCadence {
        fleet: crate::config::FleetCadence {
            monthly_cap_cents,
            ..crate::config::FleetCadence::DEFAULT
        },
        ..supervised_restart_test_cadence()
    }
}

#[derive(Clone, Copy, Debug)]
enum P3Retained {
    /// Nothing retained, a positive cap and work due at once.
    Nothing,
    /// A cap stop retained by the previous run, with the cap now zero.
    CapStopWithZeroCap,
    /// A billing stop with its wait and a due probe.
    BillingStop,
}

#[tokio::test]
async fn fleet_unresolved_refresh_starts_a_running_credential_stop() {
    // The production startup path: the manager comes from the supervised start's own
    // constructor, the run from the Fleet supervisor, and the Fleet clients are
    // pointed at a tripwire that records every request of any method and path.
    // An identified unresolved refresh hands the cursor key over within two
    // seconds, keeps an active, renewing lease, completes three settings refreshes
    // within twenty seconds, sends nothing and leaves the pair and the evidence
    // untouched, even over a retained cap or billing stop; an owner retry replays
    // nothing.
    crate::crypto::install_default_provider();
    for retained in [P3Retained::Nothing, P3Retained::CapStopWithZeroCap, P3Retained::BillingStop] {
        // The run's scheduler and timers are on a controlled clock (R-P2F4-5 a): the
        // handoff bound of two seconds and the bounds of the refreshes are measured on
        // the scheduler's clock, which only the timers the loop asks for move.
        let clock = controlled_cadence_clock();
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        let control = seam.control_cadence(&clock);
        let log = DecisionLog::start();
        let (temporary, store, cursor_key, _generation) = p3_unresolved_fleet_hub();
        let mut config = supervised_restart_test_config(temporary.path());
        config.collector.provider = CollectorProvider::Fleet;
        let cap = if matches!(retained, P3Retained::CapStopWithZeroCap) { 0 } else { 1_000 };
        let cadence = p3_fleet_cadence(cap);
        // The retained account stop, written with the fingerprint the start computes
        // so the ordinary start keeps it.
        let fingerprint = p3_start(&store, &config).expect("a probe start").snapshot.config_fingerprint;
        let now_ms = current_epoch_millis().expect("clock");
        let vehicle = store.configured_tesla_vehicles().expect("cars")[0].0;
        let (state, kind) = match retained {
            P3Retained::Nothing => (None, ""),
            P3Retained::CapStopWithZeroCap => (Some("cap_reached"), "cap_reached"),
            P3Retained::BillingStop => (Some("billing_suspended"), "billing_suspended"),
        };
        if let Some(state) = state {
            store
                .collector_write_transaction(
                    |transaction| {
                        let wrote = HubStore::write_collector_rows_in_transaction(
                            transaction,
                            &crate::db::CollectorRowsWrite {
                                now_ms: now_ms - 60_000,
                                account: Some(crate::db::CollectorAccountRow {
                                    mode: "fleet_polling".to_owned(),
                                    config_fingerprint: fingerprint.clone(),
                                    state: state.to_owned(),
                                    reason_code: state.to_owned(),
                                    since_ms: now_ms - 60_000,
                                    rate_limited_until_ms: matches!(retained, P3Retained::BillingStop)
                                        .then_some(now_ms + 600_000),
                                    next_probe_ms: matches!(retained, P3Retained::BillingStop)
                                        .then_some(now_ms - 1_000),
                                }),
                                vehicles: vec![crate::db::CollectorVehicleRow {
                                    vehicle_id: vehicle,
                                    phase: "online".to_owned(),
                                    hold: "none".to_owned(),
                                    reason_code: "collecting".to_owned(),
                                    phase_since_ms: now_ms - 60_000,
                                    suspended_until_ms: None,
                                    next_probe_ms: None,
                                    last_wake_read_at_ms: None,
                                }],
                                counters: vec![
                                    counter_row(&p3_month_now(), "fleet_polling", "data", 6, 4, 4),
                                    counter_row(&p3_month_now(), "fleet_polling", "state", 2, 2, 2),
                                ],
                                gap_inserts: vec![crate::db::CollectorGapInsert {
                                    vehicle_id: None,
                                    kind: kind.to_owned(),
                                    started_at_ms: now_ms - 60_000,
                                    ended_at_ms: None,
                                    alert: false,
                                }],
                                ..crate::db::CollectorRowsWrite::default()
                            },
                        )?;
                        Ok::<_, StoreError>(((), wrote))
                    },
                    || Ok(()),
                )
                .expect("the retained stop");
        }
        let rows_before = store.load_collector_state_rows().expect("rows");
        let evidence_before = p3_fleet_evidence(&store);
        assert!(store.identified_unresolved_fleet_refresh().expect("evidence"));
        // The low-level constructor still refuses the unresolved input.
        assert!(matches!(
            FleetAuthManager::from_store(store.clone(), temporary.path()),
            Err(FleetCredentialError::RotationOutcomeUnknown)
        ));

        let tripwire = FleetTripwire::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let router = Router::new()
            .fallback(|| async { StatusCode::NOT_FOUND })
            .layer(axum::middleware::from_fn_with_state(
                tripwire.clone(),
                fleet_tripwire_record,
            ))
            .with_state(tripwire.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("tripwire server");
        });
        let base = url::Url::parse(&format!("http://{address}/")).expect("base");
        let api = FleetApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("client");
        let auth_api = FleetAuthApi::for_fake_http(
            base.join("oauth2/v3/token").expect("token URL"),
            Duration::from_secs(2),
        )
        .expect("auth client");
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let manager = FleetAuthManager::from_store_for_supervised_start(
            store.clone(),
            temporary.path(),
            Arc::clone(&admission),
            None,
        )
        .expect("the supervised start admits the identified unresolved refresh");
        assert!(manager.is_input_fenced());
        let manager = Arc::new(tokio::sync::Mutex::new(manager));

        let origin = clock.mono_now();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let task_store = store.clone();
        let task_seam = Arc::clone(&seam);
        let task_admission = Arc::clone(&admission);
        let task_manager = Arc::clone(&manager);
        let mut task = tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
            run_fleet_supervised_with_access(
                &task_store,
                &config,
                cadence,
                api,
                auth_api,
                None,
                task_manager,
                cursor_key,
                ready_tx,
                RunWriteGuard::for_run(Some(task_admission)),
                true,
                async move {
                    let _ = stop_rx.await;
                },
            )
            .await
        }));
        let refreshes_before = seam.settings_refreshes_completed();
        // The handoff is awaited at the loop's own holds: the loop is stepped by the
        // timers it asks for until the cursor key has been handed over, and the time
        // the handoff took is the scheduler's clock at the first hold that saw it
        // (before that hold's own timer is granted). The two seconds are enforced
        // inside the traversal (R-P2F5-1): no timer past them is granted, so a handoff
        // that never comes fails on the scheduler's clock, and a separate stall
        // watchdog ends a traversal that does not move; either way the run is aborted
        // and joined.
        let mut ready_rx = ready_rx;
        let handoff = step_the_controlled_loop_until_within(
            &control,
            origin,
            Duration::from_secs(2),
            || match ready_rx.try_recv() {
                Ok(_) => Ok(true),
                Err(oneshot::error::TryRecvError::Empty) => Ok(false),
                Err(oneshot::error::TryRecvError::Closed) => {
                    Err("Fleet startup handoff dropped".to_owned())
                }
            },
        )
        .await;
        let handoff_elapsed = match handoff {
            Ok(elapsed) => elapsed,
            Err(failure) => {
                task.abort();
                let _ = (&mut task).await;
                panic!("{retained:?}: the handoff: {failure}");
            }
        };
        assert!(
            handoff_elapsed <= Duration::from_secs(2),
            "{retained:?}: handoff took {handoff_elapsed:?} on the scheduler's clock"
        );
        let lease_first = store
            .supervised_collector_lease_status()
            .expect("lease")
            .expect("an active lease");
        assert_eq!(lease_state_text(&store), "active", "{retained:?}");
        // Three completed settings refreshes within twenty seconds of the scheduler's
        // clock (the entry counter alone would not show them finished).
        step_the_controlled_loop_to_a_tick_within(
            &control,
            clock.mono_now(),
            Duration::from_secs(20),
            |tick| tick.settings_refreshes_completed - refreshes_before >= 3,
        )
        .await;
        assert!(!task.is_finished(), "{retained:?}: the run stays alive");
        assert!(
            clock.mono_now().saturating_duration_since(origin)
                <= Duration::from_secs(20) + Duration::from_secs(2),
            "{retained:?}"
        );
        // The lease renews (its heartbeat runs on real time, so it is waited for, not
        // bounded), stays active, and the run is still going.
        let first_heartbeat = lease_first.heartbeat_at_ms;
        wait_for_supervised_restart_condition("the lease renews", || {
            store
                .supervised_collector_lease_status()
                .ok()
                .flatten()
                .is_some_and(|lease| lease.heartbeat_at_ms > first_heartbeat)
        })
        .await;
        let lease_now = store
            .supervised_collector_lease_status()
            .expect("lease")
            .expect("an active lease");
        assert_eq!(lease_state_text(&store), "active", "{retained:?}");
        assert!(lease_now.heartbeat_at_ms > lease_first.heartbeat_at_ms, "{retained:?}: renewing");
        assert!(!task.is_finished(), "{retained:?}");
        // The reconnect attention is the one transition event, and nothing else of
        // the stop was logged as a terminal condition.
        assert_eq!(
            log.lines("INFO", "collector_fleet_refresh_outcome_unknown"),
            1,
            "{retained:?}: {}",
            log.contents()
        );
        // The retained policy, before any retry: the stored account row, its wait and
        // probe times, the counts and the reservations are as the stop found them,
        // and the retained stop's own gap is still open beside the credential stop's.
        let rows_during = store.load_collector_state_rows().expect("rows");
        assert_eq!(rows_during.account, rows_before.account, "{retained:?}: the account row");
        assert_eq!(rows_during.counters, rows_before.counters, "{retained:?}: counts and reservations");
        let open_gaps: Vec<String> = store
            .collection_gaps(100)
            .expect("gaps")
            .into_iter()
            .filter(|gap| gap.ended_at_ms.is_none())
            .map(|gap| gap.kind)
            .collect();
        if state.is_some() {
            assert!(open_gaps.iter().any(|open| open == kind), "{retained:?}: {open_gaps:?}");
        }
        assert!(open_gaps.iter().any(|open| open == "auth_terminal"), "{retained:?}: {open_gaps:?}");
        assert_eq!(tripwire.seen(), Vec::<String>::new(), "{retained:?}: no request of any kind");
        // The owner's retry lifts the stop; the fenced manager meets its fence and
        // the account stops again; nothing is replayed. (One variant suffices: the
        // retry path does not depend on what the stop retained.)
        if matches!(retained, P3Retained::Nothing) {
            store
                .insert_collector_owner_action("retry_collection", None, current_epoch_millis().expect("clock"))
                .expect("retry action");
            step_the_controlled_loop_to_a_tick_within(
                &control,
                clock.mono_now(),
                Duration::from_secs(20),
                |_| store.pending_collector_owner_actions().expect("actions").is_empty(),
            )
            .await;
            let refreshes = seam.settings_refreshes_completed();
            step_the_controlled_loop_to_a_tick_within(
                &control,
                clock.mono_now(),
                Duration::from_secs(20),
                |tick| tick.settings_refreshes_completed >= refreshes + 2,
            )
            .await;
            assert!(!task.is_finished(), "{retained:?}");
            assert_eq!(tripwire.seen(), Vec::<String>::new(), "{retained:?}: the retry replays nothing");
        }

        stop_tx.send(()).expect("shutdown");
        join_supervised_restart_task("Fleet running stop shutdown", &mut task)
            .await
            .expect("an orderly stop");
        assert_eq!(tripwire.seen(), Vec::<String>::new(), "{retained:?}");
        // Credentials and evidence are exactly as they were.
        assert_eq!(p3_fleet_evidence(&store), evidence_before, "{retained:?}");
        assert!(store.identified_unresolved_fleet_refresh().expect("evidence"));
        drop(admission);
        server.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn fleet_telemetry_maintenance_keeps_running_over_an_identified_unresolved_refresh() {
    // Native Fleet Telemetry keeps its push collection and its lease while the stored
    // pair's refresh outcome is unknown: the maintenance loop sends nothing (its token
    // use is fenced), logs it and tries again a minute later, instead of ending the
    // run and sending the service into a restart loop.
    crate::crypto::install_default_provider();
    // What the loop logs is captured (this runtime runs the task on this thread).
    #[derive(Clone, Default)]
    struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sink = Sink::default();
    let writer = sink.clone();
    let _logging = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish(),
    );
    tracing::callsite::rebuild_interest_cache();
    let (temporary, store, cursor_key, _generation) = p3_unresolved_fleet_hub();
    let config = supervised_restart_test_config(temporary.path());
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let manager = FleetAuthManager::from_store_for_supervised_start(
        store.clone(),
        temporary.path(),
        Arc::clone(&admission),
        None,
    )
    .expect("the supervised start admits it");
    assert!(manager.is_input_fenced());
    let manager = Arc::new(tokio::sync::Mutex::new(manager));
    let auth_api = FleetAuthApi::for_fake_http(
        url::Url::parse("http://127.0.0.1:9/oauth2/v3/token").expect("token URL"),
        Duration::from_secs(1),
    )
    .expect("auth client");
    let (terrain_wake, _terrain_rx) = mpsc::channel(8);
    let scheduler = VehicleScheduler::new_for_mode(
        p3_fleet_cadence(1_000),
        crate::config::CollectorMode::FleetPolling,
        Instant::now(),
    );
    let ledger = scheduler.request_ledger();
    let task_store = store.clone();
    let task_admission = Arc::clone(&admission);
    let evidence_before = p3_fleet_evidence(&store);
    let task = tokio::spawn(async move {
        run_fleet_telemetry_maintenance_loop(
            &task_store,
            &config,
            &auth_api,
            &manager,
            &cursor_key,
            &terrain_wake,
            &task_admission,
            true,
            &ledger,
            &RunWriteGuard::new(AdmissionFence::default(), Some(Arc::clone(&task_admission))),
        )
        .await
    });
    for _ in 0..5 {
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(!task.is_finished(), "the run goes on over the unknown outcome");
    }
    task.abort();
    assert_eq!(p3_fleet_evidence(&store), evidence_before, "credentials and evidence untouched");
    // The standing condition is reported once for the episode, as polling mode's
    // single attention transition is, not once a minute.
    let logged = String::from_utf8(sink.0.lock().unwrap().clone()).expect("log text");
    assert_eq!(
        logged.matches("Fleet token refresh outcome is unknown").count(),
        1,
        "one warning for the episode: {logged}"
    );
}

// ---- P3b (B2 fix round): a denial that arrives while the Fleet settlement is held ----------------------

/// A Fleet loop is shut down in order while a token exchange it started is held at the
/// token endpoint, so its stop is recorded and its shutdown waits for the exchange
/// (the settlement). The retained admission is then denied, the renewal is ended and
/// joined while the exchange is still held, and the held answer is released: the
/// exchange refuses to encrypt or write, the settlement returns the sensitive failure,
/// and the run ends with it and keeps its lease.
///
/// `case` says which observer meets the denial first: `None` lets either do, and the
/// others order them by acknowledged steps (the renewal's own candidate and the
/// supervisor's watch are held at their gates). With `restored` the admission passes
/// again before the answer is released (R-P3bF-1): the observed loss alone must keep
/// the job from encrypting or writing it.
async fn assert_the_late_fleet_denial(case: Option<RenewalEndsFirst>, restored: bool) {
    let mut gates = None;
    let mut bed = FleetLoopBed::start_with_seam(
        "asleep",
        false,
        true,
        Duration::from_secs(120),
        |seam| {
            gates = case.map(|first| {
                (
                    seam.arm_gate("renewal-candidate"),
                    (first == RenewalEndsFirst::Renewal).then(|| seam.arm_gate("cleanup-watch")),
                )
            });
        },
    )
    .await;
    let lists_seen = Arc::clone(&bed.fake.lists);
    bed.wait_for("the first lists", || lists_seen.load(Ordering::SeqCst) >= 2).await;
    if let Some((renewal, _)) = &gates {
        renewal.arrived("the renewal's first candidate", 1).await;
    }
    let encryption = bed.manager.lock().await.observe_exchange_encryption();
    // The next request starts an exchange the token endpoint holds.
    bed.fake.token_hold.store(true, Ordering::SeqCst);
    bed.manager.lock().await.mark_refresh_due();
    let tokens = Arc::clone(&bed.fake.token_requests);
    bed.wait_for("the held exchange", || tokens.load(Ordering::SeqCst) >= 1).await;
    let pair_before = bed
        .store
        .load_fleet_tokens()
        .expect("Fleet row")
        .expect("Fleet credentials");
    let instance = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease")
        .instance_id;
    bed.shutdown.take().expect("shutdown").send(()).expect("shutdown");
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the shutdown waits for the exchange", || {
        seam.loop_log().contains(&LoopEvent::FleetSettlementBegun)
    })
    .await;
    assert!(
        !seam.loop_log().contains(&LoopEvent::RenewalJoined),
        "the renewal runs while the admission holds"
    );
    // The stop was recorded before the settlement began, with the admission intact.
    assert_eq!(
        bed.store
            .collection_gaps(50)
            .expect("gaps")
            .iter()
            .filter(|gap| gap.kind == "hub_stopped" && gap.ended_at_ms.is_none())
            .count(),
        1,
        "the ordinary stop is recorded"
    );
    swap_the_admission_lock_away(bed.temporary.path());
    if let Some((renewal, Some(watch))) = &gates {
        // The renewal meets the denial first: its candidate is released and the
        // denial is acknowledged, and only then is the supervisor's watch released.
        renewal.release(1);
        bed.wait_for("the renewal's own denial", || {
            seam.loop_log().contains(&LoopEvent::RenewalDenied)
        })
        .await;
        watch.open();
    }
    bed.wait_for("the renewal is joined while the exchange is held", || {
        seam.loop_log().contains(&LoopEvent::RenewalJoined)
    })
    .await;
    let log = seam.loop_log();
    match case {
        Some(RenewalEndsFirst::Supervisor) => {
            assert!(!log.contains(&LoopEvent::RenewalDenied), "the supervisor was first: {log:?}");
        }
        Some(RenewalEndsFirst::Renewal) => {
            assert!(log.contains(&LoopEvent::RenewalDenied), "the renewal was first: {log:?}");
        }
        None => {}
    }
    assert!(
        bed.task.as_ref().is_some_and(|task| !task.is_finished()),
        "the exchange is still held when the renewal is over"
    );
    let joined_at = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease")
        .heartbeat_at_ms;
    let rows_after_join = collector_rows_stamp(&bed.store);
    sleep(SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL + Duration::from_millis(300)).await;
    assert_eq!(
        bed.store
            .supervised_collector_lease_status()
            .expect("lease query")
            .expect("the lease")
            .heartbeat_at_ms,
        joined_at,
        "no renewal after the join"
    );

    if restored {
        restore_the_admission_lock(bed.temporary.path());
        assert!(
            assert_runtime_sensitive_access(Some(&*bed.admission)).is_ok(),
            "the admission passes again"
        );
    }
    // The validated rotated response is released.
    bed.fake.token_hold.store(false, Ordering::SeqCst);
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the Fleet run ends", &mut task).await;
    bed.server.abort();
    assert!(
        matches!(
            &result,
            Err(CollectorError::FleetCredential(FleetCredentialError::SensitiveAccessUnavailable))
        ),
        "the settlement's sensitive failure is the run's result: {result:?}"
    );
    let pair_after = bed
        .store
        .load_fleet_tokens()
        .expect("Fleet row")
        .expect("Fleet credentials");
    assert_eq!(pair_after.access(), pair_before.access(), "the pair is not replaced");
    assert_eq!(pair_after.refresh(), pair_before.refresh());
    assert_eq!(pair_after.credential_generation(), pair_before.credential_generation());
    assert_eq!(
        (pair_after.expires_at(), pair_after.next_refresh_at()),
        (pair_before.expires_at(), pair_before.next_refresh_at())
    );
    assert_eq!(
        encryption.load(Ordering::SeqCst),
        0,
        "the denied job neither encrypts nor writes"
    );
    assert!(
        bed.store.has_unresolved_fleet_refresh().expect("receipts"),
        "the attempt stays fenced"
    );
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease is retained to expire");
    assert_eq!(retained.instance_id, instance);
    assert_eq!(retained.heartbeat_at_ms, joined_at, "nothing renewed or released it");
    assert_eq!(
        collector_rows_stamp(&bed.store),
        rows_after_join,
        "no collector write after the denial"
    );
    // A later admitted start, beyond the retained lease, closes the recorded stop
    // and adds no downtime of its own.
    let started = bed
        .store
        .begin_collector_run(&crate::db::CollectorRunStartRequest {
            mode: "fleet_polling",
            config_fingerprint: &"0".repeat(64),
            start_ms: retained.heartbeat_at_ms + 60_000,
            previous_heartbeat_ms: Some(retained.heartbeat_at_ms),
            gap_min_ms: K_GAP_MIN_MS,
            gap_alert_ms: 600_000,
        })
        .expect("a later start");
    assert!(
        started.stopped_gap_closed && !started.hub_down_inserted,
        "the recorded stop is closed and no downtime is added"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_late_fleet_settlement_denial_preserves_the_lease_and_sensitive_error() {
    assert_the_late_fleet_denial(None, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_the_fleet_supervisor_fences_renewal_on_loss_before_held_cleanup() {
    assert_the_late_fleet_denial(Some(RenewalEndsFirst::Supervisor), false).await;
    assert_the_late_fleet_denial(Some(RenewalEndsFirst::Renewal), false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_restored_admission_after_an_observed_loss_persists_nothing_in_the_fleet_unit() {
    // R-P3bF-1: the loss is observed while the held answer is out (by the supervisor's
    // watch, and by the renewal first), the admission passes again, and the released
    // answer is still neither encrypted nor written.
    assert_the_late_fleet_denial(Some(RenewalEndsFirst::Supervisor), true).await;
    assert_the_late_fleet_denial(Some(RenewalEndsFirst::Renewal), true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_an_ordinary_fleet_settlement_failure_reaches_the_result_without_being_a_loss() {
    // The same shutdown with the admission intact and an exchange that fails the
    // ordinary way (Tesla answers the held request with an error): the failure is the
    // run's result as the exchange named it, it is no loss of the admission, and the
    // recorded stop still releases the lease.
    let mut bed = FleetLoopBed::start_with_auth_timeout(
        "asleep",
        false,
        true,
        Duration::from_secs(120),
    )
    .await;
    let lists_seen = Arc::clone(&bed.fake.lists);
    bed.wait_for("the first lists", || lists_seen.load(Ordering::SeqCst) >= 2).await;
    bed.fake.token_hold.store(true, Ordering::SeqCst);
    bed.fake.token_status.store(500, Ordering::SeqCst);
    bed.manager.lock().await.mark_refresh_due();
    let tokens = Arc::clone(&bed.fake.token_requests);
    bed.wait_for("the held exchange", || tokens.load(Ordering::SeqCst) >= 1).await;
    bed.shutdown.take().expect("shutdown").send(()).expect("shutdown");
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the shutdown waits for the exchange", || {
        seam.loop_log().contains(&LoopEvent::FleetSettlementBegun)
    })
    .await;
    bed.fake.token_hold.store(false, Ordering::SeqCst);
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the Fleet run ends", &mut task).await;
    bed.server.abort();
    let error = result.expect_err("the exchange's own failure is the run's result");
    assert!(
        matches!(&error, CollectorError::FleetCredential(FleetCredentialError::Api(_))),
        "{error:?}"
    );
    assert!(!loses_sensitive_access(&error), "it is no loss of the admission");
    assert!(
        bed.store
            .supervised_collector_lease_status()
            .expect("lease query")
            .is_none(),
        "a recorded stop with the admission intact releases the lease"
    );
    assert!(
        bed.store.has_unresolved_fleet_refresh().expect("receipts"),
        "the failed exchange stays fenced for the next start"
    );
}

// ---- P3b (B2 fix round): a withheld resend and the second forced refresh -------------------------

/// The refusal of CT74a for the case its own script leaves open: the first list
/// whose own request is the last the persisted reservation covers (`reserved ==
/// ordinal`, read while that list is unanswered), so that its resend is not covered,
/// is answered 401, and the status stays armed. The lists before it are answered as
/// usual.
fn refuse_the_list_whose_resend_is_not_covered(
    fake: &FleetFake,
    seam: &Arc<SupervisedCollectorTestSeam>,
    notes: &Arc<Mutex<Vec<ListArrival>>>,
) -> ListScript {
    let probe = fake.clone();
    let seam = Arc::clone(seam);
    let status = Arc::clone(&fake.list_status);
    let notes = Arc::clone(notes);
    let refused = Arc::new(AtomicBool::new(false));
    Arc::new(move |ordinal| {
        if refused.load(Ordering::SeqCst) {
            return None;
        }
        let (reserved, sent) = probe.state_reservation();
        let ordinal_i = i64::try_from(ordinal).expect("ordinal");
        if reserved == ordinal_i {
            refused.store(true, Ordering::SeqCst);
            status.store(401, Ordering::SeqCst);
            notes.lock().expect("notes").push(ListArrival::Refused {
                ordinal,
                iteration: seam.loop_iterations(),
                reserved,
                sent,
            });
            Some(401)
        } else {
            notes.lock().expect("notes").push(ListArrival::Declined);
            None
        }
    })
}

/// C3 R47: no tick refreshes the Fleet token twice. `counts` are the cumulative
/// refreshes at consecutive tick boundaries (the first is the baseline before the
/// first tick), and every tick's increment is at most one.
fn assert_each_tick_refreshes_at_most_once(counts: &[usize]) {
    for (tick, pair) in counts.windows(2).enumerate() {
        let refreshed = pair[1]
            .checked_sub(pair[0])
            .expect("a cumulative count never falls");
        assert!(
            refreshed <= 1,
            "tick {tick} refreshed {refreshed} times: {counts:?}"
        );
    }
}

#[test]
fn each_tick_refreshes_at_most_once_is_judged_per_tick_not_in_total() {
    // Two refreshes in one tick are rejected even though the totals look the same as
    // two ticks with one each (R-P3bF-5): a count of completed ticks against a count
    // of refreshes could not tell them apart. The test holds a temporary store like
    // every other new test (work order section 1; B2 fix round, P3b fix 2, Opus N3).
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let _store = HubStore::initialize(temporary.path()).expect("Hub store");
    assert_each_tick_refreshes_at_most_once(&[0, 1, 1, 2, 2]);
    let two_in_one_tick = std::panic::catch_unwind(|| {
        assert_each_tick_refreshes_at_most_once(&[0, 0, 2, 2]);
    });
    assert!(two_in_one_tick.is_err(), "both refreshes in one tick are refused");
}

#[tokio::test(start_paused = true)]
async fn b2_k3_the_forced_refresh_of_a_withheld_resend_spends_no_spacing_wait() {
    // The same course as the test that follows, on the in-process bed and under paused
    // tokio time (B2 apply, K3): every token is refused, the first tick's resend is not
    // covered by the persisted reservation and is withheld, and the next tick forces a
    // second refresh. With time paused, a wait of any length inside a tick (a spacing
    // wait between two token requests, say) is an advance of the clock, so each tick of
    // the course must end at the very instant it began, exactly. Nothing here is a
    // span of real time, and the stall watchdog of the other bed does not decide it.
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    bed.allow_refresh = true;
    bed.fake.rotate_refresh.store(true, Ordering::SeqCst);
    bed.fake.reject_every_token.store(true, Ordering::SeqCst);
    let mut token_counts = vec![bed.fake.token_requests.load(Ordering::SeqCst)];
    for tick in 0..6 {
        let began = tokio::time::Instant::now();
        bed.try_tick().await.expect("the tick");
        assert_eq!(
            tokio::time::Instant::now(),
            began,
            "tick {tick}: a wait stood between its requests"
        );
        token_counts.push(bed.fake.token_requests.load(Ordering::SeqCst));
        if bed.scheduler.account.state == AccountState::CredentialStop {
            break;
        }
        bed.advance(Duration::from_secs(1));
    }
    assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
    assert_each_tick_refreshes_at_most_once(&token_counts);
    let refreshing: Vec<usize> = token_counts
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[1] > pair[0])
        .map(|(index, _)| index)
        .collect();
    assert!(refreshing.len() >= 2, "two refreshes at least: {token_counts:?}");
    assert!(
        refreshing.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "each forced refresh is of the very next tick: {token_counts:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_withheld_resend_then_a_second_forced_refresh_waits_on_no_spacing_and_holds_no_manager() {
    // CT74a's own script refuses a list whose resend the persisted reservation
    // covers. This is the case it leaves open: the list's resend is not covered, so
    // after the tick's refresh it is withheld (C3 R47: no answer, no failure, no
    // wait, and it stays due), and the next tick's list meets the same refusal and
    // forces a second refresh at once.
    //
    // Disposition (P3b carried item; coordinator ruling R-P3bF-5 keeps it): not a
    // defect. The 60-second spacing between token requests, and the wait for it, belong
    // to the Legacy credential unit (C5 R19, R49, whose forced refresh waits and is
    // accepted to do so under the locks of its fused wrapper, R41a). A Fleet token has
    // no such spacing in the specification: C3 R47 allows one refresh per tick, and the
    // Fleet manager has no gate between refreshes. The second refresh therefore costs
    // the next tick, not a minute, and the manager is held only for the request it
    // serves. This pins that, on the case the P2 report described: the double hands
    // every refresh a refresh token of its own, so the second refresh RECORDS A
    // SUCCESSOR. It is sent within the cadence, in a tick of its own and not the
    // first's (the refresh count is taken at each tick boundary and no tick adds more
    // than one), the stop that follows comes in seconds (a spacing wait would take
    // 60), the stored pair has been replaced twice, no attempt stays unresolved and
    // the manager is free when it is over.
    let mut bed =
        FleetLoopBed::start_with_auth_timeout("online", true, true, Duration::from_secs(30)).await;
    bed.fake.rotate_refresh.store(true, Ordering::SeqCst);
    let reads = Arc::clone(&bed.fake.reads);
    bed.wait_for("the drive's reads", || reads.load(Ordering::SeqCst) >= 3).await;
    // Each completed tick now reports the token requests answered so far. The ticks
    // that completed before the probe was replaced are not used.
    let tokens = Arc::clone(&bed.fake.token_requests);
    bed.seam
        .set_requests_probe(Arc::new(move || tokens.load(Ordering::SeqCst)));
    let seen = bed.seam.tick_checkpoints().len();
    wait_for_a_completed_tick_after(&bed.seam, seen + 1).await;
    let first_counted_tick = bed.seam.tick_checkpoints().len();
    let generation_before = bed.manager.lock().await.credential_generation();
    let notes = Arc::new(Mutex::new(Vec::<ListArrival>::new()));
    bed.fake
        .set_list_script(Some(refuse_the_list_whose_resend_is_not_covered(
            &bed.fake,
            &bed.seam,
            &notes,
        )));
    let gaps_store = bed.store.clone();
    // The wait is the stall watchdog of the bed; the stop's promptness is proved below
    // by the ticks that made the two refreshes being consecutive, not by a span.
    bed.wait_for("the credential stop", || open_auth_terminal_gaps(&gaps_store) == 1).await;
    let refusals = notes
        .lock()
        .expect("notes")
        .iter()
        .filter(|note| matches!(note, ListArrival::Refused { .. }))
        .count();
    assert_eq!(refusals, 1, "one refusal was scripted");
    let seen = bed.seam.tick_checkpoints().len();
    let latest = wait_for_a_completed_tick_after(&bed.seam, seen).await;
    assert_eq!(latest.account, "credential_stop");
    // The refresh count at every tick boundary from the baseline before the first
    // counted tick: each tick adds at most one, and two ticks added one each.
    let ticks = bed.seam.tick_checkpoints();
    let mut counts = vec![ticks[first_counted_tick - 1].requests];
    counts.extend(ticks[first_counted_tick..].iter().map(|tick| tick.requests));
    assert_eq!(counts[0], 0, "no refresh before the script: {counts:?}");
    assert_each_tick_refreshes_at_most_once(&counts);
    let ticks_that_refreshed = counts.windows(2).filter(|pair| pair[1] > pair[0]).count();
    assert_eq!(ticks_that_refreshed, 2, "the two refreshes are in two ticks: {counts:?}");
    // The second refresh is made by the very next tick after the first's, the tick the
    // withheld resend left the list due for (a gate that refused it would put it later).
    // That no wait stood inside that tick is proved exactly by the paused-time test
    // above (B2 apply, K3): this bed runs on real time under a stall watchdog, which a
    // wait of a minute would not trip.
    let refreshing_ticks: Vec<usize> = counts
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[1] > pair[0])
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        refreshing_ticks[1],
        refreshing_ticks[0] + 1,
        "the second forced refresh is of the next tick: {counts:?}"
    );
    let tokens = bed.fake.token_requests.load(Ordering::SeqCst);
    assert_eq!(
        tokens, 2,
        "the withheld tick's refresh, and the second refresh of the next tick"
    );
    // The second refresh recorded a successor: the stored pair was replaced twice, and
    // no attempt is left unresolved (the stop is the 401 the refreshed token met, not
    // an unknown outcome).
    assert_ne!(
        bed.manager.lock().await.credential_generation(),
        generation_before,
        "the pair was replaced"
    );
    let stored = bed
        .store
        .load_fleet_tokens()
        .expect("Fleet row")
        .expect("Fleet credentials");
    assert_eq!(
        stored.credential_generation(),
        Some(bed.manager.lock().await.credential_generation()),
        "the second successor is the stored pair"
    );
    assert!(
        !bed.store.has_unresolved_fleet_refresh().expect("receipts"),
        "no attempt stays unresolved"
    );
    assert_eq!(open_auth_terminal_gaps(&bed.store), 1, "one stop, nothing failed on the way");
    // The manager is not held by a wait.
    drop(
        timeout(Duration::from_secs(2), bed.manager.lock())
            .await
            .expect("the manager is free once the second refresh is over"),
    );
    assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), tokens, "nothing more is sent");
    bed.stop().await;
}
