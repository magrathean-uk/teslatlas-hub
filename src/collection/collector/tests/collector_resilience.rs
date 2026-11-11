// SPDX-License-Identifier: AGPL-3.0-only

//! M04-T01: the collector through restart, network loss and token expiry, inside the
//! collection window only. One transaction per tick and no durable write for an
//! unchanged poll; a committed observation survives a network loss and a crash; a
//! token that expires after a rejected refresh routes to the re-login state (the
//! account's credential stop and the `auth_terminal` lease) while an expiry during
//! a network loss only waits; and no entry starts collection outside an Open window.

use super::*;
use crate::fleet_api::egress::test_support::TestWindow;

// ---- Fixtures ------------------------------------------------------------------------

/// A loopback double that counts every request it is sent and answers each with a
/// 503: a tripwire for "nothing was sent", and a harmless peer for a run that only
/// has to start.
struct Tripwire {
    url: url::Url,
    requests: Arc<AtomicUsize>,
    server: JoinHandle<()>,
}

impl Tripwire {
    async fn spawn() -> Self {
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&requests);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("tripwire");
        let address = listener.local_addr().expect("tripwire address");
        let router = Router::new().fallback(move || {
            let counted = Arc::clone(&counted);
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
            }
        });
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self {
            url: url::Url::parse(&format!("http://{address}/")).expect("tripwire URL"),
            requests,
            server,
        }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for Tripwire {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A stream endpoint of a port no other test uses (the reserved listener keeps it
/// so), for a window route of its own.
fn reserved_stream_endpoint() -> (String, std::net::TcpListener) {
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve a port");
    let port = reserved.local_addr().expect("address").port();
    (format!("ws://127.0.0.1:{port}/streaming/"), reserved)
}

/// A Hub with the imported car and an Owner pair, and the Legacy credential unit of
/// the run, whose token endpoint is the tripwire.
fn windowed_hub(
    tripwire: &Tripwire,
) -> (tempfile::TempDir, HubStore, CollectionAuth, OwnerApi) {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);
    let now = current_epoch_millis().expect("clock").div_euclid(1_000);
    crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(
        &store,
        temporary.path(),
        "window-access",
        "window-refresh",
        Some((now + 28_800, now + 14_400)),
    )
    .expect("supplied pair");
    let manager = LegacyAuthManager::from_hub_legacy_owner_store_with_issuer(
        store.clone(),
        temporary.path(),
        tripwire.url.join("oauth2/v3/").expect("issuer"),
    )
    .expect("load pair");
    let region = manager.region();
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region,
    };
    let client =
        OwnerApi::for_fake_http(tripwire.url.clone(), Duration::from_secs(2)).expect("client");
    (temporary, store, auth, client)
}

/// What a refused start must have left alone: no lease, no gap, no account, vehicle
/// or counter row.
fn assert_nothing_started(store: &HubStore, label: &str) {
    assert!(
        store
            .supervised_collector_lease_status()
            .expect("lease")
            .is_none(),
        "{label}: no lease"
    );
    let rows = store.load_collector_state_rows().expect("collector rows");
    assert!(rows.account.is_none(), "{label}: no account row");
    assert!(rows.vehicles.is_empty(), "{label}: no vehicle row");
    assert!(rows.counters.is_empty(), "{label}: no counter row");
    assert!(
        store.collection_gaps(100).expect("gaps").is_empty(),
        "{label}: no gap"
    );
}

/// Run the Owner-wire collector with its stream endpoint routed through `window`,
/// to its end, and return its result and whether it handed over its cursor key.
async fn run_windowed(
    store: &HubStore,
    directory: &std::path::Path,
    auth: CollectionAuth,
    client: OwnerApi,
    stream_endpoint: String,
) -> (Result<(), CollectorError>, bool) {
    let (ready_tx, ready_rx) = oneshot::channel();
    let result = timeout(
        SUPERVISED_RESTART_WAIT,
        run_supervised_with_access(
            store,
            &supervised_restart_test_config(directory),
            supervised_restart_test_cadence(),
            client,
            auth,
            stream_endpoint,
            CursorKey::from_bytes([0xA4; 32]),
            Some(ready_tx),
            RunWriteGuard::for_run(None),
            std::future::pending::<()>(),
        ),
    )
    .await
    .expect("a refused start returns at once");
    (result, ready_rx.await.is_ok())
}

// ---- No path starts collection outside an Open window --------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collector_resilience_closed_window_refuses_the_start_before_any_write_or_send() {
    let tripwire = Tripwire::spawn().await;
    let (temporary, store, auth, client) = windowed_hub(&tripwire);
    let window = TestWindow::closed();
    let (endpoint, _port) = reserved_stream_endpoint();
    let _route = window.route(&endpoint);
    let (result, ready) = run_windowed(&store, temporary.path(), auth, client, endpoint).await;
    assert!(
        matches!(result, Err(CollectorError::CollectionWindowClosed("window_closed"))),
        "{result:?}"
    );
    assert!(!ready, "no cursor key is handed over");
    assert_nothing_started(&store, "closed window");
    assert_eq!(tripwire.requests(), 0, "nothing is sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collector_resilience_window_past_its_deadline_refuses_the_start() {
    let tripwire = Tripwire::spawn().await;
    let (temporary, store, auth, client) = windowed_hub(&tripwire);
    // Opened for a minute; the host then slept past the deadline before the start,
    // so no supervisor step has closed the window yet: the gate sees it itself.
    let window = TestWindow::open(60);
    window.advance(61);
    let (endpoint, _port) = reserved_stream_endpoint();
    let _route = window.route(&endpoint);
    let (result, ready) = run_windowed(&store, temporary.path(), auth, client, endpoint).await;
    assert!(
        matches!(result, Err(CollectorError::CollectionWindowClosed("window_closed"))),
        "{result:?}"
    );
    assert!(!ready);
    assert_nothing_started(&store, "expired window");
    assert_eq!(tripwire.requests(), 0, "nothing is sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collector_resilience_open_window_lets_the_run_start_and_stop() {
    let tripwire = Tripwire::spawn().await;
    let (temporary, store, auth, client) = windowed_hub(&tripwire);
    let window = TestWindow::open(3_600);
    let (endpoint, _port) = reserved_stream_endpoint();
    let _route = window.route(&endpoint);
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task_store = store.clone();
    let config = supervised_restart_test_config(temporary.path());
    let mut task = tokio::spawn(async move {
        run_supervised_with_access(
            &task_store,
            &config,
            supervised_restart_test_cadence(),
            client,
            auth,
            endpoint,
            CursorKey::from_bytes([0xA5; 32]),
            Some(ready_tx),
            RunWriteGuard::for_run(None),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });
    let _cursor = wait_for_supervised_signal_or_abort("collector readiness", &mut task, ready_rx).await;
    assert!(
        store.supervised_collector_lease_status().expect("lease").is_some(),
        "the run holds the lease inside the window"
    );
    shutdown_tx.send(()).expect("shutdown");
    join_supervised_restart_task("collector join", &mut task)
        .await
        .expect("an orderly stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collector_resilience_admitted_entry_refuses_before_the_credential_vault_and_cursor_key() {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);
    let window = TestWindow::closed();
    let (endpoint, _port) = reserved_stream_endpoint();
    let _route = window.route(&endpoint);
    let mut config = supervised_restart_test_config(temporary.path());
    // A configured override must be `wss://`; the route is by host and port.
    config.collector.stream_endpoint_override = Some(endpoint.replacen("ws://", "wss://", 1));
    let admission =
        crate::hub_user_process::AdmittedUserHub::admit(temporary.path()).expect("admission");
    let (ready_tx, ready_rx) = oneshot::channel();
    let result = run_supervised_with_clients_for_admitted_user(
        &store,
        &config,
        admission,
        ready_tx,
        |_| -> Result<OwnerApi, CollectorError> {
            panic!("an Owner API client was built outside the window")
        },
        |_| -> Result<FleetClients, CollectorError> {
            panic!("Fleet clients were built outside the window")
        },
        std::future::pending::<()>(),
    )
    .await;
    assert!(
        matches!(result, Err(CollectorError::CollectionWindowClosed("window_closed"))),
        "{result:?}"
    );
    assert!(ready_rx.await.is_err(), "no cursor key is handed over");
    assert!(
        !crate::legacy_owner_credentials::cursor_key_path(temporary.path()).exists(),
        "no cursor key is created"
    );
    assert_nothing_started(&store, "admitted entry");
    assert!(
        store.load_collector_auth_row().expect("auth row").is_none(),
        "no credential status is written"
    );
}

// ---- One transaction per tick; no durable write for an unchanged poll ----------------

/// The process's first resolution of a car after a restart reads the stored identity
/// and writes nothing; a car whose descriptor changed is still registered.
#[test]
fn collector_resilience_restart_reads_an_unchanged_identity_without_writing() {
    let directory = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(directory.path()).expect("store");
    let car = core_vehicle(1, "asleep");
    let first = IdentityCache::default()
        .resolve(&store, CollectorProvider::Legacy, &car, 1_000)
        .expect("identity");
    let seen = |store: &HubStore| -> i64 {
        store
            .open()
            .expect("observer")
            .query_row("SELECT last_seen_at_ms FROM vehicles", [], |row| row.get(0))
            .expect("last seen")
    };
    // A restart: a new process's cache, the same car, later.
    store.instrumentation().reset();
    let mut restarted = IdentityCache::default();
    let again = restarted
        .resolve(&store, CollectorProvider::Legacy, &car, 9_000)
        .expect("identity after restart");
    assert_eq!(again, first, "the same identity");
    let counts = store.instrumentation().snapshot();
    assert_eq!(counts.registration_calls(), 0, "nothing is registered again");
    assert_eq!(counts.writable_opens, 0, "no connection opened for writing");
    assert_eq!(seen(&store), 1_000, "no durable write");
    // A renamed car is registered, under the same identity.
    let mut renamed = car.clone();
    renamed.display_name = Some("Renamed".to_owned());
    let changed = IdentityCache::default()
        .resolve(&store, CollectorProvider::Legacy, &renamed, 9_000)
        .expect("identity of the renamed car");
    assert_eq!(changed, first);
    assert_eq!(seen(&store), 9_000, "a change is registered");
}

/// A list equal to the stored lifecycle needs a transaction only to read the facts;
/// the Owner-wire counts the list made dirty do not ride on it, so it rolls back and
/// the poll writes nothing.
#[tokio::test]
async fn collector_resilience_unchanged_list_does_not_carry_the_owner_counts() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.scheduler.ledger.record_completed(RequestCategory::State, false);
            assert!(matches!(
                bed.list(vec![core_vehicle(3, "asleep")]).await,
                TickOutcome::Committed { wrote: true, .. }
            ));
            assert_eq!(seam.tick_commits(), 1, "the first list is stored with its count");
            let counted = bed.store.load_collector_state_rows().expect("rows").counters;
            assert_eq!(counted.len(), 1);
            // The next list repeats the state; the stored observation made the cached
            // view stale, so the facts are read in a transaction. The list's own count
            // is dirty but not due.
            bed.advance(Duration::from_secs(30));
            bed.scheduler.ledger.record_completed(RequestCategory::State, false);
            let observer = bed.store.open().expect("observer connection");
            let before: i64 = observer
                .query_row("PRAGMA data_version", [], |row| row.get(0))
                .expect("data version");
            assert!(matches!(
                bed.list(vec![core_vehicle(3, "asleep")]).await,
                TickOutcome::Committed { wrote: false, .. }
            ));
            let after: i64 = observer
                .query_row("PRAGMA data_version", [], |row| row.get(0))
                .expect("data version");
            assert_eq!(before, after, "no durable write");
            assert_eq!(seam.tick_commits(), 1, "nothing committed");
            assert_eq!(
                bed.store.load_collector_state_rows().expect("rows").counters,
                counted,
                "the counts wait"
            );
            // With the view cached the decision is made in memory.
            bed.advance(Duration::from_secs(30));
            assert!(matches!(
                bed.list(vec![core_vehicle(3, "asleep")]).await,
                TickOutcome::Idle
            ));
            // A tick that writes anyway carries them.
            bed.advance(Duration::from_secs(30));
            assert!(matches!(
                bed.list(vec![core_vehicle(3, "online")]).await,
                TickOutcome::Committed { wrote: true, .. }
            ));
            let rows = bed.store.load_collector_state_rows().expect("rows").counters;
            assert_eq!(rows.len(), 1);
            assert_ne!(rows, counted, "the waiting counts rode on the next commit");
        })
        .await;
}

/// The supervised Owner-wire loop, restarted over an asleep car: neither the first
/// list after the restart nor the lists after it make a durable write or a write
/// transaction outside the tick (no identity registration, no inventory write, no
/// count on a read-only transaction).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collector_resilience_unchanged_polls_after_a_restart_write_nothing() {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);
    let now = current_epoch_millis().expect("clock").div_euclid(1_000);
    crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(
        &store,
        temporary.path(),
        "restart-access",
        "restart-refresh",
        Some((now + 28_800, now - 60)),
    )
    .expect("supplied pair");
    let fake = crate::collection::loopback_fixture::FakeTeslaSource::spawn_canonical(
        crate::collection::loopback_fixture::AdvanceMode::Manual,
    )
    .await
    .expect("loopback Tesla");
    fake.set_step(crate::collection::loopback_fixture::ScenarioStep::AsleepDiscovery);
    let fake = Arc::new(fake);
    let lists = {
        let fake = Arc::clone(&fake);
        move || {
            fake.audited_requests()
                .iter()
                .filter(|request| request.path == "/api/1/products")
                .count()
        }
    };

    // One run: the asleep car is listed and stored, then an orderly stop.
    let run = |store: HubStore, seed: u8| {
        let manager = LegacyAuthManager::from_hub_legacy_owner_store_with_issuer(
            store.clone(),
            temporary.path(),
            fake.oauth_issuer_url(),
        )
        .expect("load pair");
        let region = manager.region();
        let auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region,
        };
        let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
            .expect("client");
        let config = supervised_restart_test_config(temporary.path());
        let stream_endpoint = fake.stream_endpoint().to_owned();
        let clock = controlled_cadence_clock();
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        let control = seam.control_cadence(&clock);
        seam.set_requests_probe(Arc::new(lists.clone()));
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(seam, async move {
            run_supervised_with_access(
                &store,
                &config,
                supervised_restart_test_cadence(),
                client,
                auth,
                stream_endpoint,
                CursorKey::from_bytes([seed; 32]),
                Some(ready_tx),
                RunWriteGuard::for_run(None),
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        }));
        (task, control, clock, ready_rx, shutdown_tx)
    };
    let bound = 4 * CONTROL_SETTINGS_REFRESH;

    let (mut task, control, clock, ready_rx, shutdown_tx) = run(store.clone(), 0xB1);
    let _cursor = wait_for_supervised_signal_or_abort("first run readiness", &mut task, ready_rx).await;
    let first_lists = lists();
    warm_up_the_controlled_loop_within(&control, clock.mono_now(), bound, |tick| {
        tick.requests >= first_lists + 2 && tick.tick_commits >= 1
    })
    .await;
    shutdown_tx.send(()).expect("first shutdown");
    join_supervised_restart_task("first run join", &mut task)
        .await
        .expect("first run result");

    let one = |sql: &str| -> i64 {
        store
            .open()
            .expect("observer")
            .query_row(sql, [], |row| row.get(0))
            .expect("one value")
    };
    let last_seen = one("SELECT MAX(last_seen_at_ms) FROM vehicles");
    let counters_at = one("SELECT COALESCE(MAX(updated_at_ms), 0) FROM collector_request_counters");
    let observations = one("SELECT COUNT(*) FROM raw_observations");

    // The restart: a new process over the same store.
    let (mut task, control, clock, ready_rx, shutdown_tx) = run(store.clone(), 0xB2);
    let _cursor = wait_for_supervised_signal_or_abort("restart readiness", &mut task, ready_rx).await;
    let restart_lists = lists();
    let first = warm_up_the_controlled_loop_within(&control, clock.mono_now(), bound, |tick| {
        tick.requests > restart_lists
    })
    .await;
    let stamp = collector_rows_stamp(&store);
    let counts = store.instrumentation().snapshot();
    let commits = first.tick_commits;
    let later = step_the_controlled_loop_to_a_tick_within(&control, clock.mono_now(), bound, |tick| {
        tick.requests >= first.requests + 3
    })
    .await;
    let delta = store.instrumentation().snapshot();
    assert_eq!(
        delta.source_registration_calls, counts.source_registration_calls,
        "an unchanged list registers no source"
    );
    assert_eq!(
        delta.vehicle_registration_calls, counts.vehicle_registration_calls,
        "an unchanged car is not registered"
    );
    assert_eq!(
        delta.write_transaction_commits, counts.write_transaction_commits,
        "no collector transaction committed"
    );
    assert_eq!(later.tick_commits, commits, "no tick committed");
    assert_eq!(collector_rows_stamp(&store), stamp, "no collector row changed");
    // Since the first run stopped: the restart's first list did not register the car,
    // and no count rode on its read-only transaction.
    assert_eq!(one("SELECT MAX(last_seen_at_ms) FROM vehicles"), last_seen);
    assert_eq!(
        one("SELECT COALESCE(MAX(updated_at_ms), 0) FROM collector_request_counters"),
        counters_at
    );
    assert_eq!(one("SELECT COUNT(*) FROM raw_observations"), observations);
    assert!(fake.assert_no_wake_or_command().is_ok());
    shutdown_tx.send(()).expect("restart shutdown");
    join_supervised_restart_task("restart join", &mut task)
        .await
        .expect("restart result");
}

// ---- Token expiry routes to the re-login state; a network loss only waits ------------

/// A Hub whose stored Owner pair stays usable for `usable_for` seconds more, with its
/// refresh due now, and whose token endpoint answers `status`, or is unreachable (a
/// network loss) when `status` is `None`.
async fn expiry_rig(status: Option<u16>, usable_for: i64) -> FenceRig {
    crate::crypto::install_default_provider();
    let directory = crate::private_tempdir().expect("data directory");
    let store = HubStore::initialize(directory.path()).expect("store");
    let now = epoch_seconds_of(SystemTime::now());
    crate::auth::credential_vault::fixtures::seed_legacy_supplied_pair(
        directory.path(),
        &fence_jwt(),
        "expiry-refresh-0",
        Some((now + usable_for, now)),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let (token_url, server) = match status {
        Some(status) => {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let address = listener.local_addr().expect("address");
            let router = Router::new()
                .route("/oauth2/v3/token", post(fence_token))
                .with_state(FenceTokenMock {
                    calls: Arc::clone(&calls),
                    status,
                });
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });
            (format!("http://{address}/oauth2/v3/token"), server)
        }
        None => {
            // A port that was free a moment ago and has no listener: the connection
            // is refused, as on a network loss.
            let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
            let address = closed.local_addr().expect("address");
            drop(closed);
            (format!("http://{address}/oauth2/v3/token"), tokio::spawn(async {}))
        }
    };
    FenceRig {
        directory,
        store,
        token_url: url::Url::parse(&token_url).expect("token URL"),
        calls,
        server,
    }
}

/// Apply the credential unit's state until `reached`, or fail after ten seconds.
async fn apply_until(
    auth: &CollectionAuth,
    scheduler: &mut VehicleScheduler,
    state: &watch::Sender<SupervisedCollectorState>,
    label: &str,
    reached: impl Fn(bool, &VehicleScheduler) -> bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let stopped = apply_credential_state(auth, scheduler, state, Instant::now());
        if reached(stopped, scheduler) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{label} timeout");
        sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collector_resilience_token_expiry_after_a_rejected_refresh_routes_to_relogin() {
    let rig = expiry_rig(Some(400), 3).await;
    let mut manager = rig.manager();
    manager
        .refresh_now(&fence_client(), SystemTime::now())
        .await
        .expect_err("the refresh token is rejected");
    assert_eq!(rig.calls(), 1);
    assert!(matches!(
        manager.status_now().stop(),
        Some(crate::legacy_auth::state::AuthStop::SignInRequired(
            crate::legacy_auth::SignOutReason::RefreshTokenRejected
        ))
    ));
    let auth = fence_collection_auth(manager);
    let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
    let (state, lease_state) = watch::channel(SupervisedCollectorState::Active);
    // While the access token is usable, collection goes on (C5 R43).
    assert!(!apply_credential_state(&auth, &mut scheduler, &state, Instant::now()));
    assert_eq!(*lease_state.borrow(), SupervisedCollectorState::Active);
    assert_eq!(scheduler.account.state, AccountState::Collecting);
    // At its expiry the run is in the re-login state: the unit is signed out, the
    // account stops and the lease is `auth_terminal`.
    apply_until(&auth, &mut scheduler, &state, "expiry", |stopped, _| stopped).await;
    assert_eq!(*lease_state.borrow(), SupervisedCollectorState::AuthenticationTerminal);
    assert_eq!(scheduler.account.state, AccountState::CredentialStop);
    assert!(!scheduler.requests_allowed(Instant::now()));
    {
        let CollectionAuth::Legacy { manager, .. } = &auth;
        let mut manager = manager.lock().await;
        assert!(matches!(
            manager.status_now().stop(),
            Some(crate::legacy_auth::state::AuthStop::SignedOut(_))
        ));
        // It never retries.
        let _ = manager.refresh_if_due(&fence_client(), SystemTime::now()).await;
        let _ = manager.refresh_now(&fence_client(), SystemTime::now()).await;
    }
    assert_eq!(rig.calls(), 1, "no retry after the rejection");
    // A restart keeps it: the new unit presents nothing and reaches the same state.
    let restarted = fence_collection_auth(rig.manager());
    {
        let CollectionAuth::Legacy { manager, .. } = &restarted;
        let _ = manager
            .lock()
            .await
            .refresh_now(&fence_client(), SystemTime::now())
            .await;
    }
    let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
    let (state, lease_state) = watch::channel(SupervisedCollectorState::Active);
    apply_until(&restarted, &mut scheduler, &state, "restart", |stopped, _| stopped).await;
    assert_eq!(*lease_state.borrow(), SupervisedCollectorState::AuthenticationTerminal);
    assert_eq!(scheduler.account.state, AccountState::CredentialStop);
    assert_eq!(rig.calls(), 1, "nothing is sent after the restart");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collector_resilience_token_expiry_during_a_network_loss_waits_without_relogin() {
    let rig = expiry_rig(None, 3).await;
    let mut manager = rig.manager();
    let _ = manager.refresh_now(&fence_client(), SystemTime::now()).await;
    assert!(
        manager.status_now().stop().is_none(),
        "a refresh that could not reach Tesla is no stop"
    );
    let auth = fence_collection_auth(manager);
    let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
    let (state, lease_state) = watch::channel(SupervisedCollectorState::Active);
    // Past the expiry the token is not handed out, and the run waits: no re-login,
    // the lease stays active and the account is not stopped.
    sleep(Duration::from_secs(4)).await;
    assert!(!apply_credential_state(&auth, &mut scheduler, &state, Instant::now()));
    assert_eq!(*lease_state.borrow(), SupervisedCollectorState::Active);
    assert_ne!(scheduler.account.state, AccountState::CredentialStop);
    let CollectionAuth::Legacy { manager, .. } = &auth;
    let manager = manager.lock().await;
    assert!(manager.status_now().stop().is_none());
    assert_ne!(
        manager.wait_view(epoch_seconds_of(SystemTime::now())),
        AuthWait::Ready,
        "the expired token is not handed out"
    );
}

// ---- Network loss and restart lose no committed observation --------------------------

/// A TCP relay to the loopback Tesla's HTTP side that can drop every connection, as
/// a network loss does.
struct LossyRelay {
    url: url::Url,
    up: Arc<std::sync::atomic::AtomicBool>,
    dropped: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl LossyRelay {
    async fn spawn(target: &url::Url) -> Self {
        let target = format!(
            "{}:{}",
            target.host_str().expect("target host"),
            target.port().expect("target port")
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("relay");
        let address = listener.local_addr().expect("relay address");
        let up = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let dropped = Arc::new(AtomicUsize::new(0));
        let task = {
            let up = Arc::clone(&up);
            let dropped = Arc::clone(&dropped);
            tokio::spawn(async move {
                while let Ok((mut inbound, _)) = listener.accept().await {
                    if !up.load(Ordering::SeqCst) {
                        dropped.fetch_add(1, Ordering::SeqCst);
                        drop(inbound);
                        continue;
                    }
                    let target = target.clone();
                    tokio::spawn(async move {
                        if let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await {
                            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                        }
                    });
                }
            })
        };
        Self {
            url: url::Url::parse(&format!("http://{address}/")).expect("relay URL"),
            up,
            dropped,
            task,
        }
    }
}

impl Drop for LossyRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collector_resilience_network_loss_and_restart_keep_every_committed_observation() {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);
    let now = current_epoch_millis().expect("clock").div_euclid(1_000);
    crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(
        &store,
        temporary.path(),
        "loss-access",
        "loss-refresh",
        Some((now + 28_800, now - 60)),
    )
    .expect("supplied pair");
    let fake = crate::collection::loopback_fixture::FakeTeslaSource::spawn_canonical(
        crate::collection::loopback_fixture::AdvanceMode::Manual,
    )
    .await
    .expect("loopback Tesla");
    fake.set_step(crate::collection::loopback_fixture::ScenarioStep::Parked);
    fake.set_stream_available(false);
    let relay = LossyRelay::spawn(fake.http_base_url()).await;
    let owner_reads = |store: &HubStore| -> i64 {
        store
            .open()
            .expect("observer")
            .query_row(
                "SELECT COUNT(*) FROM raw_observations WHERE json_extract(payload_json, '$.record_type') = 'owner_api_vehicle_data_v1'",
                [],
                |row| row.get(0),
            )
            .expect("owner reads")
    };
    let start = |store: HubStore, seed: u8| {
        let manager = LegacyAuthManager::from_hub_legacy_owner_store_with_issuer(
            store.clone(),
            temporary.path(),
            fake.oauth_issuer_url(),
        )
        .expect("load pair");
        let region = manager.region();
        let auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region,
        };
        let client =
            OwnerApi::for_fake_http(relay.url.clone(), Duration::from_secs(2)).expect("client");
        let config = supervised_restart_test_config(temporary.path());
        let stream_endpoint = fake.stream_endpoint().to_owned();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            run_supervised_with_access(
                &store,
                &config,
                supervised_restart_test_cadence(),
                client,
                auth,
                stream_endpoint,
                CursorKey::from_bytes([seed; 32]),
                Some(ready_tx),
                RunWriteGuard::for_run(None),
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        });
        (task, ready_rx, shutdown_tx)
    };

    let (mut task, ready_rx, shutdown_tx) = start(store.clone(), 0xE1);
    let _cursor = wait_for_supervised_signal_or_abort("readiness", &mut task, ready_rx).await;
    k6_wait_for(&mut task, "first reads", || owner_reads(&store) >= 2).await;
    // The network goes: every connection to Tesla is dropped. The run goes on, and
    // nothing it committed goes away.
    relay.up.store(false, Ordering::SeqCst);
    let before_loss = owner_reads(&store);
    k6_wait_for(&mut task, "requests during the loss", || {
        relay.dropped.load(Ordering::SeqCst) >= 3
    })
    .await;
    let during_loss = owner_reads(&store);
    assert!(during_loss >= before_loss, "no committed observation is lost");
    assert!(!task.is_finished(), "the run survives the network loss");
    // The network comes back: collection resumes.
    relay.up.store(true, Ordering::SeqCst);
    k6_wait_for(&mut task, "reads after the loss", || {
        owner_reads(&store) > during_loss
    })
    .await;
    shutdown_tx.send(()).expect("shutdown");
    join_supervised_restart_task("first join", &mut task)
        .await
        .expect("an orderly stop");
    let committed = owner_reads(&store);
    let committed_rows: Vec<i64> = store
        .open()
        .expect("observer")
        .prepare("SELECT observation_id FROM raw_observations ORDER BY observation_id")
        .expect("statement")
        .query_map([], |row| row.get(0))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("observation ids");
    drop(store);

    // A restart: a new process opens the Hub. Every committed observation is there,
    // and collection goes on from it.
    let reopened = HubStore::initialize(temporary.path()).expect("reopen Hub");
    assert_eq!(owner_reads(&reopened), committed, "every committed read survives");
    let (mut task, ready_rx, shutdown_tx) = start(reopened.clone(), 0xE2);
    let _cursor = wait_for_supervised_signal_or_abort("restart readiness", &mut task, ready_rx).await;
    k6_wait_for(&mut task, "reads after the restart", || {
        owner_reads(&reopened) > committed
    })
    .await;
    shutdown_tx.send(()).expect("restart shutdown");
    join_supervised_restart_task("restart join", &mut task)
        .await
        .expect("an orderly stop after the restart");
    let after: Vec<i64> = reopened
        .open()
        .expect("observer")
        .prepare("SELECT observation_id FROM raw_observations ORDER BY observation_id")
        .expect("statement")
        .query_map([], |row| row.get(0))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("observation ids");
    for id in &committed_rows {
        assert!(after.contains(id), "a committed observation went missing");
    }
    assert!(fake.assert_no_wake_or_command().is_ok());
}
