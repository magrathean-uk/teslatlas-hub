// SPDX-License-Identifier: AGPL-3.0-only

//! M04-T01: "Owner API refused" is a state of its own. It is not the re-login state
//! (no credential stop, no `auth_terminal` lease), it sends nothing more, it starts
//! no stream and no other source, its status offers none, and a restart restores it
//! without a list.

use super::*;

/// The scheduler's side: Tesla's refusal of the Owner API (a 403 whose body names
/// the Fleet API) is its own account state, stored under its own code, stops every
/// request and stream, is neither lifted by a ready credential unit nor replaced by
/// a credential stop, and its status names no other source.
#[test]
fn owner_api_refused_is_its_own_state_and_starts_no_other_source() {
    let mut core = ep_online_core(1);
    let at = core.at(60);
    ep_sync_clock(&core, at);
    core.scheduler
        .discovery_failed_for_error(&ep_forbidden(true), at);
    assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
    assert_eq!(AccountState::OwnerApiRefused.code(), "owner_api_refused");
    assert_ne!(
        AccountState::OwnerApiRefused.code(),
        AccountState::CredentialStop.code(),
        "not the re-login state"
    );
    // No polling and no stream, now or an hour later.
    let later = at + Duration::from_secs(3_600);
    ep_sync_clock(&core, later);
    assert!(!core.scheduler.requests_allowed(later));
    assert!(!core.scheduler.discovery_due(later));
    assert!(core.scheduler.due_vehicles(later).is_empty());
    assert!(!core.scheduler.should_start_stream(core_id(1)));
    // The credential unit does not move it: a ready unit does not lift it, and a
    // terminal stop of the unit does not turn it into the re-login state.
    core.scheduler
        .credential_state_seen(CredentialStateSeen::Ready, later);
    assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
    core.scheduler
        .credential_state_seen(CredentialStateSeen::TerminalStop, later);
    assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
    assert!(!core.scheduler.requests_allowed(later));
    // It is stored as its own row.
    let account = core
        .scheduler
        .take_tick()
        .account
        .expect("the refusal's account row");
    assert_eq!(account.row.state, "owner_api_refused");
    assert_eq!(account.row.reason_code, "owner_api_refused");
    // The status says what happened and offers no other source (no provider, no Fleet
    // polling) and no credential remedy.
    let text = core.scheduler.status(later).account_text.to_lowercase();
    assert!(text.contains("refused the owner api"), "{text}");
    for other in ["fleet", "provider", "credential", "sign in"] {
        assert!(!text.contains(other), "{other:?} in {text:?}");
    }
}

/// The lease of a refused run stays `active`: a refusal is not the re-login state,
/// whose lease is `auth_terminal`.
#[tokio::test]
async fn owner_api_refused_keeps_the_lease_active() {
    let rig = fence_rig(200, false).await;
    let auth = fence_collection_auth(rig.manager());
    let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
    scheduler.discovery_failed_for_error(&ep_forbidden(true), Instant::now());
    assert_eq!(scheduler.account.state, AccountState::OwnerApiRefused);
    let (state, lease_state) = watch::channel(SupervisedCollectorState::Active);
    assert!(
        !apply_credential_state(&auth, &mut scheduler, &state, Instant::now()),
        "a refusal is not a terminal credential stop"
    );
    assert_eq!(*lease_state.borrow(), SupervisedCollectorState::Active);
    assert_eq!(scheduler.account.state, AccountState::OwnerApiRefused);
    assert_eq!(rig.calls(), 0, "no token request either");
}

/// A stream endpoint that counts the connections it accepts and never answers.
struct CountingStreamPeer {
    endpoint: String,
    accepted: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl CountingStreamPeer {
    async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("stream peer");
        let address = listener.local_addr().expect("stream address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                drop(socket);
            }
        });
        Self {
            endpoint: format!("ws://{address}/streaming/"),
            accepted,
            task,
        }
    }
}

impl Drop for CountingStreamPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The Owner-wire loop over a wire whose list Tesla refuses with a 403 naming the
/// Fleet API: one list refuses the account at once, and from then on, and across a
/// restart, no list, read, stream or other request is sent; the lease stays held
/// and the owner sees the refusal, not a sign-in request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_api_refused_never_switches_on_polling_or_another_source_across_a_restart() {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);
    let initial = OwnerTokens::from_secret_parts("ar-access".to_owned(), "ar-refresh".to_owned())
        .expect("initial pair");
    let _generation = crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(
        &store,
        temporary.path(),
        initial.access_token(),
        initial.refresh_token(),
        None,
    )
    .expect("supplied pair");
    let wire = K6ListWire {
        status: 403,
        body: r#"{"error":"Owner API endpoint is no longer available; use the Fleet API"}"#,
        calls: Arc::new(AtomicUsize::new(0)),
        token_calls: Arc::new(AtomicUsize::new(0)),
    };
    let others = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = {
        let others = Arc::clone(&others);
        Router::new()
            .route("/api/1/products", get(k6_products))
            .route("/oauth2/v3/token", post(k6_token))
            .fallback(move || {
                let others = Arc::clone(&others);
                async move {
                    others.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::NOT_FOUND, "not here")
                }
            })
            .with_state(wire.clone())
    };
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Owner");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
    let stream = CountingStreamPeer::spawn().await;

    let run = |seed: u8| {
        let manager = LegacyAuthManager::from_hub_source_db_store_for_test(
            store.clone(),
            temporary.path(),
            &crate::legacy_auth::AuthEndpointConfig {
                mode: crate::legacy_auth::AuthMode::Tesla,
                region: Some(StreamRegion::Global),
                token_url: None,
                allow_loopback_http: true,
            },
            Some(base.join("oauth2/v3/token").expect("token endpoint")),
            false,
            Arc::new(SkippingEnv::default()),
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
        let client = OwnerApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("client");
        let config = supervised_restart_test_config(temporary.path());
        let stream_endpoint = stream.endpoint.clone();
        let clock = controlled_cadence_clock();
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        let control = seam.control_cadence(&clock);
        {
            let calls = Arc::clone(&wire.calls);
            seam.set_requests_probe(Arc::new(move || calls.load(Ordering::SeqCst)));
        }
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task_store = store.clone();
        let task = tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(seam, async move {
            run_supervised_with_access(
                &task_store,
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
    let step_bound = 4 * CONTROL_SETTINGS_REFRESH;
    let attention = || {
        store
            .collector_readiness_at(current_epoch_millis().expect("clock"))
            .expect("readiness")
            .failure
    };
    let stored_state = || -> (String, String) {
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT state, reason_code FROM collector_account_state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("account row")
    };

    // The first run: one list, refused at once (the body names the Fleet API, so no
    // refresh check), then nothing more for two of the loop's settings refreshes.
    let (mut task, control, clock, ready_rx, shutdown_tx) = run(0xD1);
    let _cursor = wait_for_supervised_signal_or_abort("first readiness", &mut task, ready_rx).await;
    let refused = warm_up_the_controlled_loop_within(&control, clock.mono_now(), step_bound, |tick| {
        tick.account == "owner_api_refused"
    })
    .await;
    assert_eq!(wire.calls.load(Ordering::SeqCst), 1, "one list");
    step_the_controlled_loop_to_a_tick_within(&control, clock.mono_now(), step_bound, |tick| {
        tick.settings_refreshes_completed >= refused.settings_refreshes_completed + 2
    })
    .await;
    assert_eq!(wire.calls.load(Ordering::SeqCst), 1, "no list while refused");
    assert_eq!(others.load(Ordering::SeqCst), 0, "no read or other request");
    assert_eq!(stream.accepted.load(Ordering::SeqCst), 0, "no stream");
    assert!(
        store.supervised_collector_lease_status().expect("lease").is_some(),
        "the run goes on holding its lease"
    );
    assert_eq!(
        stored_state(),
        ("owner_api_refused".to_owned(), "owner_api_refused".to_owned())
    );
    assert_eq!(
        attention(),
        Some(crate::db::CollectionAttentionCode::CollectorApiRefused),
        "the owner sees the refusal, not a sign-in request"
    );
    let tokens = wire.token_calls.load(Ordering::SeqCst);
    shutdown_tx.send(()).expect("first shutdown");
    join_supervised_restart_task("first join", &mut task)
        .await
        .expect("first result");

    // The restart with the same login and configuration: the refusal is restored and
    // the run sends nothing at all.
    let (mut task, control, clock, ready_rx, shutdown_tx) = run(0xD2);
    let _cursor = wait_for_supervised_signal_or_abort("restart readiness", &mut task, ready_rx).await;
    let first = warm_up_the_controlled_loop_within(&control, clock.mono_now(), step_bound, |_| true).await;
    assert_eq!(first.account, "owner_api_refused", "restored at once");
    step_the_controlled_loop_to_a_tick_within(&control, clock.mono_now(), step_bound, |tick| {
        tick.settings_refreshes_completed >= first.settings_refreshes_completed + 2
    })
    .await;
    assert_eq!(wire.calls.load(Ordering::SeqCst), 1, "no list after the restart");
    assert_eq!(others.load(Ordering::SeqCst), 0);
    assert_eq!(stream.accepted.load(Ordering::SeqCst), 0);
    assert_eq!(
        wire.token_calls.load(Ordering::SeqCst),
        tokens,
        "no token request after the restart"
    );
    assert_eq!(
        stored_state(),
        ("owner_api_refused".to_owned(), "owner_api_refused".to_owned())
    );
    assert_eq!(
        attention(),
        Some(crate::db::CollectionAttentionCode::CollectorApiRefused)
    );
    shutdown_tx.send(()).expect("restart shutdown");
    join_supervised_restart_task("restart join", &mut task)
        .await
        .expect("restart result");
    server.abort();
}
