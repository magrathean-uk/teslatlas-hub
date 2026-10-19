// SPDX-License-Identifier: AGPL-3.0-only

// B2 fix round, P3b.5 and fix 1 (C5 R23, R33a; C3 R8, B2 clarification 3): the retained
// admission is denied while the Owner loop's credential unit is still finishing its
// work at shutdown, and (R-P3bF-1) the observed loss stays decisive when the admission
// passes again. The loop runs against this file's own token endpoint on loopback,
// whose answer the test holds back and then releases, over a store-backed credential
// unit that checks the same retained admission. Every step is ordered by an
// acknowledged event of the loop, never by a time. The data is synthetic and defined
// here: nothing of the compatibility fake is used.

/// The synthetic car this file's runs import (R-P3bF-4): identifiers of its own.
const LATE_EID: i64 = 7_711_000_000_101;
const LATE_VID: i64 = 7_711_000_000_102;
const LATE_VIN: &str = "LATEDNL0000000001";

/// Import the one selected car the supervised start needs as its vehicle lineage.
fn seed_the_late_import(store: &HubStore) {
    use crate::teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history};
    use crate::teslamate_projection::{TeslaMateCar, TeslaMateHistory};

    let history = TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: LATE_EID,
            vid: Some(LATE_VID),
            vin: Some(LATE_VIN.to_owned()),
            name: Some("Late denial car".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: Some("74d".to_owned()),
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: crate::hub_pack::ProjectionCarSettings {
                enabled: true,
                use_streaming_api: true,
                ..Default::default()
            },
        }],
        drives: Vec::new(),
        positions: Vec::new(),
        charging_processes: Vec::new(),
        charges: Vec::new(),
        addresses: Vec::new(),
        geofences: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
    };
    publish_history(
        store,
        &CursorKey::from_bytes([0xD2; 32]),
        &TeslaMateImportRequest {
            source_key: "late-denial-fixture".to_owned(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: current_epoch_millis().expect("clock") - 60_000,
        },
        &history,
    )
    .expect("seed the selected imported car");
}

/// What the credential unit's persistence has written (R-P3bF-2): the account's auth
/// state row with its update time, every token receipt, its binding and fence, and the
/// stored pair itself. `collector_rows_stamp` leaves the auth state out.
fn auth_and_receipt_stamp(store: &HubStore) -> String {
    let connection = store.open().expect("connection");
    let text = |sql: &str| -> String {
        connection
            .query_row(sql, [], |row| row.get::<_, Option<String>>(0))
            .expect("stamp")
            .unwrap_or_default()
    };
    [
        text(
            "SELECT group_concat(state || '|' || COALESCE(reason, '') || '|' || collecting
                    || '|' || COALESCE(retry_at, '') || '|' || fuse_consecutive
                    || '|' || clock_skew || '|' || updated_at_ms, ';')
               FROM collector_auth_state",
        ),
        text(
            "SELECT group_concat(id || '|' || outcome || '|' || COALESCE(completed_at_ms, '')
                    || '|' || COALESCE(http_status, ''), ';')
               FROM (SELECT * FROM outbound_request_receipts ORDER BY id)",
        ),
        text(
            "SELECT group_concat(receipt_id || '|' || input_credential_generation
                    || '|' || COALESCE(output_credential_generation, ''), ';')
               FROM legacy_refresh_receipt_bindings",
        ),
        text("SELECT group_concat(input_credential_generation, ';') FROM legacy_refresh_input_fences"),
        text(
            "SELECT group_concat(hex(access) || '|' || hex(refresh) || '|' || expires_at
                    || '|' || next_refresh_at || '|' || COALESCE(credential_generation, ''), ';')
               FROM teslamate_legacy_tokens",
        ),
    ]
    .join("\n")
}

/// What the token endpoint double answers when it is released.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LateAnswer {
    /// The answer fails after the status line, as a reset connection does: whether the
    /// server rotated the token is unknown (C5 R45 row 6).
    Reset,
    /// A complete, valid answer with a rotated pair: the successor is validated and
    /// would be encrypted, written and its receipt closed.
    Rotated,
}

/// What the token endpoint double saw, and the gate that holds its answer.
#[derive(Clone)]
struct LateWire {
    posts: Arc<AtomicUsize>,
    arrived: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    answer: LateAnswer,
}

async fn late_token(State(wire): State<LateWire>) -> axum::response::Response {
    wire.posts.fetch_add(1, Ordering::SeqCst);
    wire.arrived.notify_one();
    wire.release.notified().await;
    match wire.answer {
        // The answer fails after the status line: the attempt stays unresolved, and
        // the unit's result is the unknown outcome, sensitive once the admission is
        // gone.
        LateAnswer::Reset => axum::response::Response::builder()
            .status(StatusCode::OK)
            .body(axum::body::Body::from_stream(futures_util::stream::once(
                async {
                    Err::<axum::body::Bytes, std::io::Error>(std::io::Error::other(
                        "connection reset",
                    ))
                },
            )))
            .expect("a failing answer"),
        LateAnswer::Rotated => (
            StatusCode::OK,
            r#"{"access_token":"late-access-2","refresh_token":"late-refresh-2","expires_in":28800,"token_type":"Bearer"}"#,
        )
            .into_response(),
    }
}

async fn late_products() -> axum::response::Response {
    (StatusCode::OK, r#"{"response":[],"count":0}"#).into_response()
}

/// A running Owner loop over a store with a due Legacy pair, holding a retained
/// admission its credential unit checks, and a token endpoint that holds its answer.
struct LateBed {
    temporary: tempfile::TempDir,
    store: HubStore,
    wire: LateWire,
    seam: Arc<SupervisedCollectorTestSeam>,
    task: Option<JoinHandle<Result<(), CollectorError>>>,
    shutdown: Option<oneshot::Sender<()>>,
    server: JoinHandle<()>,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    /// The switch that denies the credential unit's admission check once, when the bed
    /// was started with it (B2 fix round, P3b fix 3, Opus N4).
    denial: Option<Arc<AtomicBool>>,
}

impl LateBed {
    async fn start() -> Self {
        Self::start_answering(LateAnswer::Reset).await
    }

    async fn start_answering(answer: LateAnswer) -> Self {
        Self::start_with(answer, false).await
    }

    async fn start_with(answer: LateAnswer, deny_once: bool) -> Self {
        crate::crypto::install_default_provider();
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        seed_the_late_import(&store);
        crate::teslamate_credentials::replace_key(temporary.path(), b"late-cloak-key")
            .expect("0600 Cloak key");
        let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
        let initial =
            OwnerTokens::from_secret_parts("late-access".to_owned(), "late-refresh".to_owned())
                .expect("initial pair");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
                .expect("Cloak pair");
        // A pair whose refresh is due: the loop's first request starts the exchange.
        let now_seconds = current_epoch_millis().expect("clock") / 1_000;
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    access,
                    refresh,
                    now_seconds + 36_000,
                    now_seconds - 100,
                )
                .expect("a due refresh schedule"),
            )
            .expect("store pair");
        let wire = LateWire {
            posts: Arc::new(AtomicUsize::new(0)),
            arrived: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            answer,
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake listener");
        let address = listener.local_addr().expect("fake address");
        let router = Router::new()
            .route("/api/1/products", get(late_products))
            .route("/oauth2/v3/token", post(late_token))
            .with_state(wire.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("fake Owner");
        });
        let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
        let admission =
            crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
        let mut manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            store.clone(),
            temporary.path(),
            base.join("oauth2/v3/").expect("issuer"),
        )
        .expect("load pair")
        .with_runtime_admission(Arc::clone(&admission));
        let denial = deny_once.then(|| manager.deny_sensitive_access_once_for_test());
        let region = manager.region();
        let auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region,
        };
        let client = crate::owner_api::OwnerApi::for_fake_http(base, Duration::from_secs(2))
            .expect("client");
        let config = supervised_restart_test_config(temporary.path());
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        let task_store = store.clone();
        let task_seam = Arc::clone(&seam);
        let task_admission = Arc::clone(&admission);
        let mut task = tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
            run_supervised_with_access(
                &task_store,
                &config,
                supervised_restart_test_cadence(),
                client,
                auth,
                "ws://127.0.0.1:9/streaming/".to_owned(),
                CursorKey::from_bytes([0xB2; 32]),
                Some(ready_tx),
                RunWriteGuard::for_run(Some(task_admission)),
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        }));
        match timeout(SUPERVISED_RESTART_WAIT, ready_rx).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                let result = (&mut task).await;
                panic!("the loop ended before it was ready: {result:?}");
            }
            Err(_) => {
                task.abort();
                panic!("loop readiness timeout");
            }
        }
        Self {
            temporary,
            store,
            wire,
            seam,
            task: Some(task),
            shutdown: Some(shutdown_tx),
            server,
            admission,
            denial,
        }
    }

    async fn wait_for(&mut self, label: &str, condition: impl FnMut() -> bool) {
        let task = self.task.as_mut().expect("task");
        k6_wait_for(task, label, condition).await;
    }

    fn heartbeat_at(&self) -> i64 {
        self.store
            .supervised_collector_lease_status()
            .expect("lease query")
            .expect("the lease")
            .heartbeat_at_ms
    }
}

impl LateBed {
    /// The loop's first request starts a token exchange and the double holds its
    /// answer; the shutdown is asked for, so the run's stop is recorded (an ordinary
    /// result, the admission still intact) and the shutdown then waits for the
    /// credential unit's exchange. Returns the lease's instance.
    async fn shut_down_over_the_held_exchange(&mut self) -> String {
        timeout(SUPERVISED_RESTART_WAIT, self.wire.arrived.notified())
            .await
            .expect("the loop's exchange reaches the token endpoint");
        assert_eq!(self.wire.posts.load(Ordering::SeqCst), 1);
        let instance = self
            .store
            .supervised_collector_lease_status()
            .expect("lease query")
            .expect("the lease")
            .instance_id;
        self.shutdown.take().expect("shutdown").send(()).expect("shutdown");
        let seam = Arc::clone(&self.seam);
        self.wait_for("the shutdown waits for the credential unit", || {
            seam.loop_log().contains(&LoopEvent::CleanupWaiting)
        })
        .await;
        assert!(
            !seam.loop_log().contains(&LoopEvent::RenewalJoined),
            "the renewal runs while the admission holds"
        );
        // The stop was recorded before the cleanup began.
        assert_eq!(
            self.store
                .collection_gaps(50)
                .expect("gaps")
                .iter()
                .filter(|gap| gap.kind == "hub_stopped" && gap.ended_at_ms.is_none())
                .count(),
            1,
            "the ordinary stop is recorded"
        );
        instance
    }

    /// After the denial: the supervisor's watch notes the loss and joins the renewal
    /// while the exchange is still held. Returns the heartbeat the join left.
    async fn wait_for_the_renewal_join(&mut self) -> i64 {
        let seam = Arc::clone(&self.seam);
        self.wait_for("the renewal is joined while the exchange is held", || {
            seam.loop_log().contains(&LoopEvent::RenewalJoined)
        })
        .await;
        assert!(
            self.task.as_ref().is_some_and(|task| !task.is_finished()),
            "the exchange is still held"
        );
        let joined_at = self.heartbeat_at();
        sleep(SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL + Duration::from_millis(300)).await;
        assert_eq!(self.heartbeat_at(), joined_at, "no renewal after the join");
        joined_at
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_late_credential_shutdown_denial_preserves_the_lease_and_sensitive_error() {
    // The retained admission is denied while the shutdown waits for the exchange: the
    // supervisor fences and joins the renewal at once, with the exchange still held,
    // and the lease is kept. Released, the exchange ends unknown, which the denial
    // makes sensitive: the run ends with that outcome, as the unit named it. The unit
    // applies it in memory and journals it, and writes nothing: not the account's
    // auth state, not a receipt, not the pair (R-P3bF-2).
    let mut bed = LateBed::start().await;
    let instance = bed.shut_down_over_the_held_exchange().await;
    swap_the_admission_lock_away(bed.temporary.path());
    let joined_at = bed.wait_for_the_renewal_join().await;
    let rows_after_join = collector_rows_stamp(&bed.store);
    let credential_rows_after_join = auth_and_receipt_stamp(&bed.store);

    bed.wire.release.notify_one();
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the run ends", &mut task).await;
    bed.server.abort();
    assert!(
        matches!(
            &result,
            Err(CollectorError::LegacyAuthManager(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            )))
        ),
        "the unit's own sensitive outcome is the run's result: {result:?}"
    );
    assert_eq!(bed.wire.posts.load(Ordering::SeqCst), 1, "no second request");
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
    assert_eq!(
        auth_and_receipt_stamp(&bed.store),
        credential_rows_after_join,
        "no auth state, receipt or pair write after the denial"
    );
    // A later admitted start, beyond the retained lease's expiry, closes the
    // recorded stop and adds no downtime of its own.
    let started = bed
        .store
        .begin_collector_run(&crate::db::CollectorRunStartRequest {
            mode: "legacy",
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
async fn b2_a_restored_admission_after_an_observed_loss_persists_nothing_in_the_owner_unit() {
    // R-P3bF-1: the exchange's answer is held at the token endpoint, the loss is
    // observed (the supervisor's watch notes it and joins the renewal), and the
    // admission is then RESTORED. The answer is valid and rotated; released, the
    // unit must still neither encrypt nor write it: the pair row, the started receipt
    // and its fence stay as they were, nothing is written for the account's state,
    // the lease is retained, and the run ends with a sensitive result.
    let mut bed = LateBed::start_answering(LateAnswer::Rotated).await;
    let instance = bed.shut_down_over_the_held_exchange().await;
    swap_the_admission_lock_away(bed.temporary.path());
    let joined_at = bed.wait_for_the_renewal_join().await;
    restore_the_admission_lock(bed.temporary.path());
    assert!(
        assert_runtime_sensitive_access(Some(&*bed.admission)).is_ok(),
        "the admission passes again"
    );
    let credential_rows_after_join = auth_and_receipt_stamp(&bed.store);
    let rows_after_join = collector_rows_stamp(&bed.store);

    bed.wire.release.notify_one();
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the run ends", &mut task).await;
    bed.server.abort();
    let error = result.expect_err("the observed loss is the run's result");
    assert!(loses_sensitive_access(&error), "a sensitive result: {error:?}");
    assert_eq!(bed.wire.posts.load(Ordering::SeqCst), 1, "no second request");
    assert_eq!(
        auth_and_receipt_stamp(&bed.store),
        credential_rows_after_join,
        "no pair, receipt, fence or auth state was written after the observed loss"
    );
    assert!(
        bed.store.has_unresolved_legacy_refresh().expect("receipts"),
        "the attempt stays unresolved"
    );
    assert_eq!(collector_rows_stamp(&bed.store), rows_after_join);
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease is retained to expire");
    assert_eq!(retained.instance_id, instance);
    assert_eq!(retained.heartbeat_at_ms, joined_at, "nothing renewed or released it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_restored_admission_cannot_let_a_detached_fleet_exchange_persist_after_a_supervisor_first_loss(
) {
    // R-P3bF-1, Fleet: the supervisor ends on the loss while a token exchange is held
    // (it settles nothing), the admission is restored, and the held answer is released.
    // The detached job still refuses to encrypt or write.
    let mut bed =
        FleetLoopBed::start_with_auth_timeout("asleep", false, true, Duration::from_secs(120))
            .await;
    let lists_seen = Arc::clone(&bed.fake.lists);
    bed.wait_for("the first lists", || lists_seen.load(Ordering::SeqCst) >= 2).await;
    let encryption = bed.manager.lock().await.observe_exchange_encryption();
    bed.fake.token_hold.store(true, Ordering::SeqCst);
    bed.manager.lock().await.mark_refresh_due();
    let tokens = Arc::clone(&bed.fake.token_requests);
    bed.wait_for("the held exchange", || tokens.load(Ordering::SeqCst) >= 1).await;
    let pair_before = bed
        .store
        .load_fleet_tokens()
        .expect("Fleet row")
        .expect("Fleet credentials");
    swap_the_admission_lock_away(bed.temporary.path());
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the supervisor ends on the loss", &mut task).await;
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    restore_the_admission_lock(bed.temporary.path());
    assert!(
        assert_runtime_sensitive_access(Some(&*bed.admission)).is_ok(),
        "the admission passes again"
    );
    bed.fake.token_hold.store(false, Ordering::SeqCst);
    let settled = timeout(SUPERVISED_RESTART_WAIT, async {
        bed.manager.lock().await.finish_exchange().await
    })
    .await
    .expect("the detached exchange ends");
    bed.server.abort();
    assert!(
        matches!(settled, Err(FleetCredentialError::SensitiveAccessUnavailable)),
        "{settled:?}"
    );
    assert_eq!(encryption.load(Ordering::SeqCst), 0, "the job neither encrypts nor writes");
    let pair_after = bed
        .store
        .load_fleet_tokens()
        .expect("Fleet row")
        .expect("Fleet credentials");
    assert_eq!(pair_after.access(), pair_before.access());
    assert_eq!(pair_after.refresh(), pair_before.refresh());
    assert_eq!(pair_after.credential_generation(), pair_before.credential_generation());
    assert_eq!(
        (pair_after.expires_at(), pair_after.next_refresh_at()),
        (pair_before.expires_at(), pair_before.next_refresh_at())
    );
    assert!(
        bed.store.has_unresolved_fleet_refresh().expect("receipts"),
        "the attempt stays fenced"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_tick_commit_is_refused_after_the_fence_is_set_while_the_admission_holds() {
    // R-P3bF-1, the loop's own write paths: a loss only the renewal saw leaves the
    // fence set while the admission passes again. A tick held just before its store
    // write is released after the fence is set: no transaction is begun, the loop
    // ends with the sensitive failure and the lease is retained. (The admission here
    // never changed, which is the state a restored one is in: only the fence can
    // refuse the write.)
    let mut gate = None;
    let mut bed =
        FleetLoopBed::start_with_seam("asleep", false, false, Duration::from_secs(2), |seam| {
            gate = Some(seam.arm_gate("tick-write"));
        })
        .await;
    let gate = gate.expect("the gate");
    gate.arrived("a tick before its write", 1).await;
    let commits = bed.seam.tick_commits();
    let rows = collector_rows_stamp(&bed.store);
    bed.seam.admission_fence().set();
    assert!(
        assert_runtime_sensitive_access(Some(&*bed.admission)).is_ok(),
        "the admission itself passes"
    );
    gate.open();
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the loop ends on the fence", &mut task).await;
    bed.server.abort();
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    assert_eq!(bed.seam.tick_commits(), commits, "no tick transaction after the fence");
    assert_eq!(collector_rows_stamp(&bed.store), rows, "no collector row written");
    assert!(
        bed.store
            .supervised_collector_lease_status()
            .expect("lease query")
            .is_some(),
        "the lease is retained to expire, not released"
    );
}

/// The write boundaries of a loop's iteration, after its tick, that consult the loss
/// fence (R-P3bF-1): the first guards the address enrichment, the second the export
/// replay. (The tick's own commit has its gate, `tick-write`.)
const LOOP_WRITE_BOUNDARIES: [&str; 2] = ["before-enrichment", "before-export-replay"];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_fleet_loop_crosses_no_write_boundary_after_the_fence_is_set_while_the_admission_holds()
{
    // Each boundary in turn: the loop is held at it, the fence is set with the
    // admission untouched, and the loop is let go. It ends with the sensitive failure
    // and the guarded step is not made: no enrichment pass, no export replay, no further
    // list request.
    for point in LOOP_WRITE_BOUNDARIES {
        let mut gate = None;
        let mut bed =
            FleetLoopBed::start_with_seam("asleep", false, false, Duration::from_secs(2), |seam| {
                gate = Some(seam.arm_gate(point));
            })
            .await;
        let gate = gate.expect("the gate");
        gate.arrived(point, 1).await;
        let (lists, enrichments, replays) = (
            bed.lists(),
            bed.seam.enrichment_calls(),
            bed.seam.replay_calls(),
        );
        bed.seam.admission_fence().set();
        assert!(
            assert_runtime_sensitive_access(Some(&*bed.admission)).is_ok(),
            "{point}: the admission itself passes"
        );
        gate.open();
        let mut task = bed.task.take().expect("task");
        let result = join_supervised_restart_task("the loop ends on the fence", &mut task).await;
        bed.server.abort();
        assert!(
            matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
            "{point}: {result:?}"
        );
        assert_eq!(bed.lists(), lists, "{point}: no list request after the fence");
        assert_eq!(bed.seam.enrichment_calls(), enrichments, "{point}: no enrichment");
        assert_eq!(bed.seam.replay_calls(), replays, "{point}: no export replay");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_an_owner_loop_crosses_no_write_boundary_after_the_fence_is_set_while_the_admission_holds()
{
    // The same for the Owner loop, over the Owner API double of the drain rig.
    for point in LOOP_WRITE_BOUNDARIES {
        let mut gate = None;
        let mut bed = DrainBed::start_with_admission(1, true, |_, seam| {
            gate = Some(seam.arm_gate(point));
        })
        .await;
        let gate = gate.expect("the gate");
        gate.arrived(point, 1).await;
        let (lists, enrichments, replays) = (
            bed.wire.list_requests.load(Ordering::SeqCst),
            bed.seam.enrichment_calls(),
            bed.seam.replay_calls(),
        );
        bed.seam.admission_fence().set();
        let admission = bed.admission.clone().expect("the retained admission");
        assert!(
            assert_runtime_sensitive_access(Some(&*admission)).is_ok(),
            "{point}: the admission itself passes"
        );
        gate.open();
        let mut task = bed.task.take().expect("task");
        let result = join_supervised_restart_task("the loop ends on the fence", &mut task).await;
        bed.server.abort();
        assert!(
            matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
            "{point}: {result:?}"
        );
        assert_eq!(
            bed.wire.list_requests.load(Ordering::SeqCst),
            lists,
            "{point}: no list request after the fence"
        );
        assert_eq!(bed.seam.enrichment_calls(), enrichments, "{point}: no enrichment");
        assert_eq!(bed.seam.replay_calls(), replays, "{point}: no export replay");
    }
}
