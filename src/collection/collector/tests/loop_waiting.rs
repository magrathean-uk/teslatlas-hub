// SPDX-License-Identifier: AGPL-3.0-only

// Tests of the real Owner loop (C3 R11, R8): the waiting rule and the downtime gap
// read from the previous lease. The loop runs against this file's own in-process fake Owner API on
// loopback, with the loop's short test cadence; nothing touches the network and the
// data is synthetic.

/// What the fake Owner API answers to the account list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopListMode {
    /// 200 with no cars, every time.
    Empty,
    /// 429 with `Retry-After: 1` for the first list, then 200 with no cars.
    RateLimitedOnce,
    /// 200 with the one car, asleep, every time (CT55b: every car asleep).
    AsleepCar,
    /// 429 with `Retry-After: 1` for the first list, then 200 with the asleep car.
    AsleepCarAfterRateLimit,
}

/// The beds' HTTP client timeout (R-P2F3-2). It is test infrastructure and no acceptance
/// bound: real descheduling of the double or the loop on a loaded machine must not be
/// able to turn a request into a transport failure, so it is far longer than any
/// delay a loaded machine adds. The acceptance bounds are the fixed list bounds and
/// the production wait arithmetic, never this.
const LOOP_BED_HTTP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct LoopWire {
    mode: LoopListMode,
    calls: Arc<AtomicUsize>,
    /// While set, an answer is held back once the request has been counted.
    hold: Arc<AtomicBool>,
}

async fn loop_products(State(wire): State<LoopWire>) -> axum::response::Response {
    let call = wire.calls.fetch_add(1, Ordering::SeqCst);
    while wire.hold.load(Ordering::SeqCst) {
        sleep(Duration::from_millis(5)).await;
    }
    if matches!(
        wire.mode,
        LoopListMode::RateLimitedOnce | LoopListMode::AsleepCarAfterRateLimit
    ) && call == 0
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "1")],
            r#"{"error":"rate"}"#,
        )
            .into_response();
    }
    if matches!(
        wire.mode,
        LoopListMode::AsleepCar | LoopListMode::AsleepCarAfterRateLimit
    ) {
        return (
            StatusCode::OK,
            json!({
                "response": [{
                    "id": crate::fake_tesla::FIXTURE_EID,
                    "vehicle_id": crate::fake_tesla::FIXTURE_VID,
                    "vin": crate::fake_tesla::FIXTURE_VIN,
                    "state": "asleep",
                    "display_name": "Loop fixture",
                    "in_service": false,
                }],
                "count": 1
            })
            .to_string(),
        )
            .into_response();
    }
    (StatusCode::OK, r#"{"response":[],"count":0}"#).into_response()
}

async fn loop_token() -> axum::response::Response {
    (
        StatusCode::OK,
        json!({
            "access_token": "loop-rotated-access",
            "refresh_token": "loop-rotated-refresh",
            "token_type": "Bearer",
            "expires_in": 1_000_000_000u64,
            "created_at": 1_800_000_000i64,
        })
        .to_string(),
    )
        .into_response()
}

/// A running Owner loop over a store seeded with one imported car, the fake Owner
/// API and the test seam, ready to be shut down.
struct LoopBed {
    _temporary: tempfile::TempDir,
    store: HubStore,
    wire: LoopWire,
    seam: Arc<SupervisedCollectorTestSeam>,
    task: Option<JoinHandle<Result<(), CollectorError>>>,
    shutdown: Option<oneshot::Sender<()>>,
    server: JoinHandle<()>,
}

impl LoopBed {
    /// Start the loop. `before_start` runs against the store first (a seeded lease,
    /// say).
    async fn start(mode: LoopListMode, before_start: impl FnOnce(&HubStore)) -> Self {
        Self::start_with_admission(mode, before_start, None).await
    }

    /// As [`Self::start`]; with a flag, the credential unit's check of the process's
    /// sensitive access fails from the moment the flag is set (C3 R47: the loss of
    /// sensitive access).
    async fn start_with_admission(
        mode: LoopListMode,
        before_start: impl FnOnce(&HubStore),
        lose_access: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Self {
        Self::start_with_seam(mode, before_start, lose_access, |_| {}).await
    }

    /// As [`Self::start_with_admission`]; `setup` runs against the loop's test seam
    /// before the loop starts (a controlled cadence, say).
    async fn start_with_seam(
        mode: LoopListMode,
        before_start: impl FnOnce(&HubStore),
        lose_access: Option<Arc<std::sync::atomic::AtomicBool>>,
        setup: impl FnOnce(&SupervisedCollectorTestSeam),
    ) -> Self {
        crate::crypto::install_default_provider();
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        seed_supervised_restart_import(&store);
        crate::teslamate_credentials::replace_key(temporary.path(), b"loop-cloak-key")
            .expect("0600 Cloak key");
        let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
        let initial =
            OwnerTokens::from_secret_parts("loop-access".to_owned(), "loop-refresh".to_owned())
                .expect("initial pair");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
                .expect("Cloak pair");
        // A pair whose refresh is not due for hours: the loop makes no token request
        // at its start (a successful refresh would reset a blown fuse).
        let now_seconds = current_epoch_millis().expect("clock") / 1_000;
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    access,
                    refresh,
                    now_seconds + 36_000,
                    now_seconds + 28_800,
                )
                .expect("a later refresh schedule"),
            )
            .expect("store pair");
        before_start(&store);
        let wire = LoopWire {
            mode,
            calls: Arc::new(AtomicUsize::new(0)),
            hold: Arc::new(AtomicBool::new(false)),
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake listener");
        let address = listener.local_addr().expect("fake address");
        let router = Router::new()
            .route("/api/1/products", get(loop_products))
            .route("/oauth2/v3/token", post(loop_token))
            .with_state(wire.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("fake Owner");
        });
        let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
        let manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            store.clone(),
            temporary.path(),
            base.join("oauth2/v3/").expect("issuer"),
        )
        .expect("load pair");
        let region = manager.region();
        let manager = match lose_access {
            None => manager,
            Some(flag) => {
                // The same pair, held in memory, behind a check that can fail.
                let (expires, next) = not_due_schedule();
                let auth = crate::legacy_auth::LegacyAuth::for_test(
                    base.join("oauth2/v3").expect("issuer"),
                    "loop-access",
                    "loop-refresh",
                )
                .with_test_schedule(expires, next);
                LegacyAuthManager::for_test_with_sensitive_access(
                    auth,
                    Arc::new(|_, _| Ok(())),
                    Arc::new(move || {
                        if flag.load(Ordering::SeqCst) {
                            Err(CredentialError::TeslaMateTokenStoreMissing)
                        } else {
                            Ok(())
                        }
                    }),
                )
            }
        };
        let auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region,
        };
        let client = crate::owner_api::OwnerApi::for_fake_http(base, LOOP_BED_HTTP_TIMEOUT)
            .expect("client");
        let config = supervised_restart_test_config(temporary.path());
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        {
            // Each completed tick reads the lists the double has answered.
            let calls = Arc::clone(&wire.calls);
            seam.set_requests_probe(Arc::new(move || calls.load(Ordering::SeqCst)));
        }
        setup(&seam);
        let task_store = store.clone();
        let task_seam = Arc::clone(&seam);
        let mut task = tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
            run_supervised_with_access(
                &task_store,
                &config,
                supervised_restart_test_cadence(),
                client,
                auth,
                "ws://127.0.0.1:9/streaming/".to_owned(),
                CursorKey::from_bytes([0xB1; 32]),
                Some(ready_tx),
                RunWriteGuard::for_run(None),
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        }));
        // If the loop ends before it is ready, say how: the shared wait only aborts.
        let _cursor = match timeout(SUPERVISED_RESTART_WAIT, ready_rx).await {
            Ok(Ok(cursor)) => cursor,
            Ok(Err(_)) => {
                let result = (&mut task).await;
                panic!("the loop ended before it was ready: {result:?}");
            }
            Err(_) => {
                task.abort();
                panic!("loop readiness timeout");
            }
        };
        Self {
            _temporary: temporary,
            store,
            wire,
            seam,
            task: Some(task),
            shutdown: Some(shutdown_tx),
            server,
        }
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let mut task = self.task.take().expect("task");
        join_supervised_restart_task("loop join", &mut task)
            .await
            .expect("loop result");
        self.server.abort();
    }

    async fn wait_for(&mut self, label: &str, condition: impl FnMut() -> bool) {
        let task = self.task.as_mut().expect("task");
        k6_wait_for(task, label, condition).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct55b_the_owner_loop_does_not_busy_loop_after_a_429_has_ended() {
    // The first list is a 429 with `Retry-After: 1`. The wait is kept after it ends
    // (a success does not clear it, R45), and an ended wait used to be a due time
    // of zero, so the loop went round at the floor rate (ten times a second) for
    // the rest of the process. With the inventory list due every second under the
    // test cadence, a loop that waits for its next due time goes round about once a
    // second.
    let bed = LoopBed::start(LoopListMode::RateLimitedOnce, |_| {}).await;
    tokio::time::sleep(Duration::from_millis(4_500)).await;
    let iterations = bed.seam.loop_iterations();
    let lists = bed.wire.calls.load(Ordering::SeqCst);
    assert!(lists >= 3, "{lists} lists: the loop kept listing after the wait");
    assert!(
        iterations <= 15,
        "{iterations} iterations in 4.5 s: an ended wait must not keep the loop at the floor rate"
    );
    bed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct52_a_restart_after_a_crash_records_one_hub_down_gap_from_the_old_heartbeat() {
    // The previous run died without a graceful stop: its lease stays, with its last
    // heartbeat an hour ago. The loop reads that heartbeat before it acquires its own
    // lease (R8); a regression that reads it afterwards, or never, finds no silence.
    let heartbeat_ms = current_epoch_millis().expect("clock") - 3_600_000;
    let mut bed = LoopBed::start(LoopListMode::Empty, |store| {
        let _crashed = store
            .acquire_supervised_collector_lease(heartbeat_ms)
            .expect("the dead run's lease");
    })
    .await;
    let store = bed.store.clone();
    bed.wait_for("the hub_down gap", || {
        store
            .collection_gaps(10)
            .map(|gaps| gaps.iter().any(|gap| gap.kind == "hub_down"))
            .unwrap_or(false)
    })
    .await;
    let gaps = bed.store.collection_gaps(10).expect("gaps");
    let down: Vec<_> = gaps.iter().filter(|gap| gap.kind == "hub_down").collect();
    assert_eq!(down.len(), 1, "{gaps:?}");
    assert_eq!(down[0].started_at_ms, heartbeat_ms);
    let ended = down[0].ended_at_ms.expect("closed at the start");
    assert!(ended > heartbeat_ms + 3_000_000, "{ended}");
    bed.stop().await;
}

// ---- CT55b: the real loops do not busy-loop (C3 R11) --------------------------------------------

/// What a real loop did over a window of real time.
struct LoopWindow {
    lists: usize,
    settings_refreshes: usize,
    tick_commits: usize,
    /// The tick commits that were lone reservation commits, counted where they were
    /// made (C3 R5).
    lone_reservation_commits: usize,
    iterations: usize,
    /// The gaps between consecutive iterations that started in the window.
    iteration_gaps: Vec<Duration>,
    rows_before: CollectorRowsStamp,
    rows_after: CollectorRowsStamp,
}

/// A stamp of every durable collector write: the newest `updated_at_ms` of each
/// collector table, the number of gaps and of stored observations. The fields are named,
/// so a comparison can leave out exactly the one it means to (see
/// [`CollectorRowsStamp::but_for_counters`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CollectorRowsStamp {
    account_updated_at_ms: i64,
    vehicle_updated_at_ms: i64,
    counters_updated_at_ms: i64,
    gaps: i64,
    raw_observations: i64,
    current_observations: i64,
}

impl CollectorRowsStamp {
    /// The stamp with the counters table's timestamp left out and nothing else: a Fleet
    /// list is billed, so the tick that sent it writes its count (R5, R25, R27), and
    /// that write is the only one a quiet window may make. The account and vehicle
    /// scheduler timestamps, the gaps and the observations still have to be equal.
    fn but_for_counters(&self) -> Self {
        Self {
            counters_updated_at_ms: 0,
            ..self.clone()
        }
    }
}

fn collector_rows_stamp(store: &HubStore) -> CollectorRowsStamp {
    let connection = store.open().expect("connection");
    let one = |sql: &str| -> i64 {
        connection
            .query_row(sql, [], |row| row.get(0))
            .expect("stamp")
    };
    CollectorRowsStamp {
        account_updated_at_ms: one(
            "SELECT COALESCE(MAX(updated_at_ms), 0) FROM collector_account_state",
        ),
        vehicle_updated_at_ms: one(
            "SELECT COALESCE(MAX(updated_at_ms), 0) FROM collector_vehicle_state",
        ),
        counters_updated_at_ms: one(
            "SELECT COALESCE(MAX(updated_at_ms), 0) FROM collector_request_counters",
        ),
        gaps: one("SELECT COUNT(*) FROM collection_gaps"),
        raw_observations: one("SELECT COUNT(*) FROM raw_observations"),
        current_observations: one("SELECT COUNT(*) FROM current_observations"),
    }
}

#[tokio::test]
async fn ct55b_the_counters_exempt_stamp_sees_every_other_scheduler_write() {
    // R-P2F4-2. The Fleet quiet-window and delayed-list checks compare the store but for
    // the counters table's timestamp, which the billed list's own tick writes. Each of
    // the other fields must still show a change: the account and the vehicle scheduler
    // timestamps (the writes that hid behind a `skip(3)` of the joined text), a gap and
    // an observation row.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let cursor_key = CursorKey::from_bytes([0x5D; 32]);
    finish_collection_for_provider(
        &store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online")],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await
    .expect("configure the car");
    let vehicle = store.configured_tesla_vehicles().expect("cars")[0].0;
    let write = |now_ms: i64, account: bool, car: bool, counters: bool, gap: bool| {
        store
            .collector_write_transaction(
                |transaction| {
                    let wrote = HubStore::write_collector_rows_in_transaction(
                        transaction,
                        &crate::db::CollectorRowsWrite {
                            now_ms,
                            account: account.then(|| crate::db::CollectorAccountRow {
                                mode: "fleet_polling".to_owned(),
                                config_fingerprint: "0".repeat(64),
                                state: "collecting".to_owned(),
                                reason_code: "collecting".to_owned(),
                                since_ms: now_ms,
                                rate_limited_until_ms: None,
                                next_probe_ms: None,
                            }),
                            vehicles: if car {
                                vec![crate::db::CollectorVehicleRow {
                                    vehicle_id: vehicle,
                                    phase: "online".to_owned(),
                                    hold: "none".to_owned(),
                                    reason_code: "collecting".to_owned(),
                                    phase_since_ms: now_ms,
                                    suspended_until_ms: None,
                                    next_probe_ms: None,
                                    last_wake_read_at_ms: None,
                                }]
                            } else {
                                Vec::new()
                            },
                            counters: if counters {
                                vec![counter_row(&p3_month_now(), "fleet_polling", "state", 1, 1, 1)]
                            } else {
                                Vec::new()
                            },
                            gap_inserts: if gap {
                                vec![crate::db::CollectorGapInsert {
                                    vehicle_id: None,
                                    kind: "hub_down".to_owned(),
                                    started_at_ms: now_ms - 10,
                                    ended_at_ms: Some(now_ms),
                                    alert: false,
                                }]
                            } else {
                                Vec::new()
                            },
                            ..crate::db::CollectorRowsWrite::default()
                        },
                    )?;
                    Ok::<_, StoreError>(((), wrote))
                },
                || Ok(()),
            )
            .expect("the write");
    };
    write(1_000, true, true, true, false);
    let stamp = collector_rows_stamp(&store);
    // The counted list's own write: only the counters table moves.
    write(2_000, false, false, true, false);
    let counted = collector_rows_stamp(&store);
    assert_ne!(stamp, counted, "the counters write is a change");
    assert_eq!(stamp.but_for_counters(), counted.but_for_counters(), "and the only one allowed");
    // Either scheduler timestamp changing, alone or with the counters, is seen; so is a
    // gap row.
    let mut previous = counted;
    for (now_ms, account, car, counters, gap, what) in [
        (3_000, true, false, false, false, "the account timestamp alone"),
        (4_000, false, true, false, false, "the vehicle timestamp alone"),
        (5_000, true, true, true, false, "both timestamps with the counters"),
        (6_000, false, false, false, true, "a gap row"),
    ] {
        write(now_ms, account, car, counters, gap);
        let moved = collector_rows_stamp(&store);
        assert_ne!(previous.but_for_counters(), moved.but_for_counters(), "{what} is seen");
        previous = moved;
    }
}

/// The first completed tick after the `seen` ticks already counted, waited for.
async fn wait_for_a_completed_tick_after(
    seam: &SupervisedCollectorTestSeam,
    seen: usize,
) -> TickCheckpoint {
    timeout(Duration::from_secs(20), async {
        loop {
            let all = seam.tick_checkpoints();
            if all.len() > seen {
                return all[seen];
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the loop completes another tick")
}

/// The last completed tick of a window of `seconds` that began at the completed tick
/// `first`: the newest checkpoint taken after `first` and no later than `seconds` after
/// it. A tick that completes after the window has closed is not in it, however soon
/// after (the window is `seconds` long, and is never extended to the next completion).
fn last_completed_tick_in_window(
    first: TickCheckpoint,
    all: &[TickCheckpoint],
    seconds: u64,
) -> TickCheckpoint {
    let closes = first.at + Duration::from_secs(seconds);
    all.iter()
        .copied()
        .rfind(|tick| tick.at > first.at && tick.at <= closes)
        .unwrap_or(first)
}

/// A controlled clock for a bed, starting at the real instant and wall time.
fn controlled_cadence_clock() -> Arc<ManualClock> {
    ManualClock::new(Instant::now(), current_epoch_millis().expect("clock"))
}

/// Step a loop under a controlled cadence (see [`CadenceControl`]) until it completes a
/// tick for which `reached` holds, answering every timer it asks for at the duration it
/// asked for (the loop's waiting rule decides, the test never supplies a cadence). The
/// loop is held right after that tick: the caller resumes it. At most two completed
/// ticks may follow one another with no timer between them (a loop that went round
/// without waiting would otherwise never let logical time pass).
async fn warm_up_the_controlled_loop(
    control: &CadenceControl,
    reached: impl Fn(&TickCheckpoint) -> bool,
) -> TickCheckpoint {
    warm_up_the_controlled_loop_by(control, None, reached)
        .await
        .expect("without a deadline the loop's next qualifying tick is awaited")
}

/// [`warm_up_the_controlled_loop`] with a logical `deadline` on the scheduler's clock
/// (R-P2F4-3): it returns `None`, without granting the timer, as soon as the loop asks
/// for one that would fire after the deadline while no qualifying tick has completed,
/// so a loop that goes on ticking and never does what the test waits for fails at the
/// deadline on the scheduler's clock and not by running for ever. A qualifying tick that
/// completes at the deadline is in time.
async fn warm_up_the_controlled_loop_by(
    control: &CadenceControl,
    deadline: Option<Instant>,
    reached: impl Fn(&TickCheckpoint) -> bool,
) -> Option<TickCheckpoint> {
    let mut since_timer = 0;
    loop {
        match control.next().await {
            CadenceEvent::Tick(checkpoint) => {
                since_timer += 1;
                assert!(
                    since_timer <= 2,
                    "the loop went round {since_timer} times at one logical instant"
                );
                if reached(&checkpoint) {
                    return Some(checkpoint);
                }
                control.resume();
            }
            CadenceEvent::Wait { at, duration } => {
                since_timer = 0;
                let wake = at + duration;
                if deadline.is_some_and(|deadline| wake > deadline) {
                    return None;
                }
                let now = control.clock().mono_now();
                if wake > now {
                    control.clock().advance(wake - now);
                }
                control.resume();
            }
        }
    }
}

/// Run a controlled loop on up to the logical instant `closes` and collect the ticks it
/// completes (CT55b, B2 clarification 3). `held` is the event the loop is held at, if
/// any. Every timer the loop asks for is granted at the duration it asked for, the
/// clock moving by exactly that and no tick being run before it: a timer that would fire
/// after `closes` is not granted (the clock moves to `closes` and the loop stays held at
/// it, still unable to run an outside-window tick), and a timer that fires at `closes`
/// is, so a tick that completes at `closes` is inside and one after it is never run.
/// Descheduling of the loop or of the test moves no clock, fires no timer and cannot
/// complete a tick, so what is counted is the logical time the loop's own rule produced.
async fn run_the_controlled_loop_until(
    control: &CadenceControl,
    held: &mut Option<CadenceEvent>,
    closes: Instant,
) -> Vec<TickCheckpoint> {
    let mut ticks = Vec::new();
    let mut since_timer = 0;
    loop {
        let event = match held.take() {
            Some(event) => event,
            None => control.next().await,
        };
        match event {
            CadenceEvent::Tick(checkpoint) => {
                since_timer += 1;
                assert!(
                    since_timer <= 2,
                    "the loop went round {since_timer} times at one logical instant"
                );
                assert!(checkpoint.at <= closes, "a tick ran after the window closed");
                ticks.push(checkpoint);
                control.resume();
            }
            CadenceEvent::Wait { at, duration } => {
                since_timer = 0;
                let wake = at + duration;
                let now = control.clock().mono_now();
                if wake > closes {
                    if closes > now {
                        control.clock().advance(closes - now);
                    }
                    *held = Some(event);
                    return ticks;
                }
                if wake > now {
                    control.clock().advance(wake - now);
                }
                control.resume();
            }
        }
    }
}

/// Watch a controlled loop over a window of completed ticks (CT55b, B2 clarification
/// 3): `first` is the completed tick (its commit included) the loop is held after, and
/// the window is `seconds` of the loop's logical time long; what the loop did in it is
/// the difference between `first` and the last tick that completed at or before the
/// close. `held` is left holding the loop at the timer that would pass the close.
async fn observe_controlled_window(
    seam: &SupervisedCollectorTestSeam,
    control: &CadenceControl,
    held: &mut Option<CadenceEvent>,
    store: &HubStore,
    first: TickCheckpoint,
    seconds: u64,
) -> LoopWindow {
    let rows_before = collector_rows_stamp(store);
    let closes = first.at + Duration::from_secs(seconds);
    control.resume();
    let ticks = run_the_controlled_loop_until(control, held, closes).await;
    // The loop is held: nothing writes while the snapshot is taken.
    let last = last_completed_tick_in_window(first, &ticks, seconds);
    let times: Vec<Instant> = seam
        .loop_iteration_times()
        .into_iter()
        .filter(|time| *time > first.at && *time <= closes)
        .collect();
    LoopWindow {
        lists: last.requests - first.requests,
        settings_refreshes: last.settings_refreshes_completed - first.settings_refreshes_completed,
        tick_commits: last.tick_commits - first.tick_commits,
        lone_reservation_commits: last.lone_reservation_commits - first.lone_reservation_commits,
        iterations: last.iterations - first.iterations,
        iteration_gaps: times
            .windows(2)
            .map(|pair| pair[1].saturating_duration_since(pair[0]))
            .collect(),
        rows_before,
        rows_after: collector_rows_stamp(store),
    }
}

/// The bounds of CT55b for a quiet window of `seconds` under a state-check interval of
/// one second: the lists are in `lists` (C3 clarification 3: the Owner loop makes `W - 1`
/// to `W + 1`, the Fleet loop `W - 2` to `W + 1`; the upper bound is the guard against a
/// busy loop); the settings refresh runs at most once in five seconds, plus one;
/// nothing is written; the loop goes round once for each thing it has to do and never
/// closer together than the floor. The bounds are the specification's, never derived
/// from what the loop measured.
fn assert_a_quiet_window(
    window: &LoopWindow,
    seconds: usize,
    writes_allowed: bool,
    lists: std::ops::RangeInclusive<usize>,
) {
    assert!(
        lists.contains(&window.lists),
        "{} lists in a window of {seconds} s (expected {lists:?})",
        window.lists
    );
    assert!(
        window.settings_refreshes <= seconds / 5 + 1,
        "{} settings refreshes in {seconds} s",
        window.settings_refreshes
    );
    if !writes_allowed {
        assert_eq!(window.tick_commits, 0, "no tick commit after the first");
        assert_eq!(window.rows_before, window.rows_after, "no other collector write");
    }
    assert!(
        window.iterations <= window.lists + 3,
        "{} iterations for {} lists: the loop must wait for its next due time",
        window.iterations,
        window.lists
    );
    let closest = window.iteration_gaps.iter().min().copied().unwrap_or(K_LOOP_FLOOR);
    assert!(
        closest >= K_LOOP_FLOOR,
        "two iterations {closest:?} apart: closer than the floor"
    );
}

/// With a due time forced into the past and nothing to do, the controlled loop goes
/// round at most ten times a second beyond the iterations that do work (C3 R11): two
/// logical seconds, exactly as the loop's floor spaces them.
async fn assert_the_floor_holds_with_a_due_time_in_the_past(
    seam: &SupervisedCollectorTestSeam,
    control: &CadenceControl,
    held: &mut Option<CadenceEvent>,
    lists: impl Fn() -> usize,
) {
    let (lists_before, iterations_before) = (lists(), seam.loop_iterations());
    seam.force_due_in_the_past(true);
    let closes = control.clock().mono_now() + Duration::from_secs(2);
    run_the_controlled_loop_until(control, held, closes).await;
    seam.force_due_in_the_past(false);
    let (iterations, lists) = (seam.loop_iterations() - iterations_before, lists() - lists_before);
    assert!(
        iterations >= 5,
        "{iterations} iterations: the forced due time did make the loop go round"
    );
    assert!(
        iterations <= 10 * 2 + lists + 2,
        "{iterations} iterations in 2 s with a due time in the past: the floor must hold it to ten a second"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct55b_the_owner_loop_with_every_car_asleep_makes_one_list_a_second_and_writes_nothing() {
    // The loop runs under a controlled cadence: its scheduler reads a clock the test moves
    // by exactly the timers the loop asks for, so the window is W seconds of the loop's
    // own logical time and the bounds are the specification's, whatever the machine does.
    let clock = controlled_cadence_clock();
    let mut control = None;
    let bed = LoopBed::start_with_seam(LoopListMode::AsleepCar, |_| {}, None, |seam| {
        control = Some(seam.control_cadence(&clock));
    })
    .await;
    let control = control.expect("the controlled cadence");
    let first = warm_up_the_controlled_loop(&control, |tick| tick.requests >= 2).await;
    let mut held = None;
    let lists = || bed.wire.calls.load(Ordering::SeqCst);
    // A window of completed ticks, W seconds: the Owner loop makes W - 1 to W + 1 lists.
    let window =
        observe_controlled_window(&bed.seam, &control, &mut held, &bed.store, first, 6).await;
    assert_a_quiet_window(&window, 6, false, 6 - 1..=6 + 1);
    assert_the_floor_holds_with_a_due_time_in_the_past(&bed.seam, &control, &mut held, lists).await;
    bed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct55b_the_owner_loop_keeps_the_same_bounds_once_a_429_wait_has_ended() {
    let clock = controlled_cadence_clock();
    let mut control = None;
    let bed =
        LoopBed::start_with_seam(LoopListMode::AsleepCarAfterRateLimit, |_| {}, None, |seam| {
            control = Some(seam.control_cadence(&clock));
        })
        .await;
    let control = control.expect("the controlled cadence");
    // The first list is the 429 (Retry-After: 1); the lists after its wait run (the
    // warm-up grants the timers the loop asks for, the 429's second included).
    let first = warm_up_the_controlled_loop(&control, |tick| tick.requests >= 4).await;
    let mut held = None;
    let window =
        observe_controlled_window(&bed.seam, &control, &mut held, &bed.store, first, 6).await;
    // The 429's own row (the wait, persisted) was written before the window.
    assert_a_quiet_window(&window, 6, false, 6 - 1..=6 + 1);
    bed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct55b_request_arrival_does_not_open_a_completed_tick_window() {
    // The double counts a list request when it arrives. A window that began at that
    // counter would open on an unfinished tick, with a commit still to come. Here the
    // answer is held back after the counter moved: no tick completes, so no checkpoint
    // is taken, until the answer is released and the tick, commit included, is over.
    let mut bed = LoopBed::start(LoopListMode::AsleepCar, |_| {}).await;
    let calls = Arc::clone(&bed.wire.calls);
    bed.wait_for("the first lists", || calls.load(Ordering::SeqCst) >= 2).await;
    // A tick has just completed and the loop is asleep until its next list is due,
    // a second away: hold the answer of that list before it is sent.
    let opened_by = wait_for_a_completed_tick_after(&bed.seam, bed.seam.tick_checkpoints().len()).await;
    bed.wire.hold.store(true, Ordering::SeqCst);
    let seen = bed.seam.tick_checkpoints().len();
    let arrived = calls.load(Ordering::SeqCst);
    bed.wait_for("a held list", || calls.load(Ordering::SeqCst) > arrived).await;
    sleep(Duration::from_millis(900)).await;
    assert_eq!(
        bed.seam.tick_checkpoints().len(),
        seen,
        "the request has arrived and its counter has moved, but its tick is not complete \
         (the window would have opened on {opened_by:?})"
    );
    let during_hold = seen;
    bed.wire.hold.store(false, Ordering::SeqCst);
    let opened = wait_for_a_completed_tick_after(&bed.seam, during_hold).await;
    assert!(
        opened.requests >= calls.load(Ordering::SeqCst).saturating_sub(1),
        "the checkpoint is the completed tick of the held request: {opened:?}"
    );
    bed.stop().await;
}

#[test]
fn ct55b_the_window_bounds_are_the_specifications_and_never_follow_the_loop() {
    // A loop that is slower than its cadence makes fewer lists in its window, and that
    // is a failure: the lower bound is not lowered by how slow the loop measured itself
    // to be (its longest gap between iterations is as long as its window is, here).
    let window = |lists: usize| LoopWindow {
        lists,
        settings_refreshes: 1,
        tick_commits: 0,
        lone_reservation_commits: 0,
        iterations: lists,
        iteration_gaps: vec![Duration::from_millis(1_500); lists.saturating_sub(1)],
        rows_before: CollectorRowsStamp::default(),
        rows_after: CollectorRowsStamp::default(),
    };
    let fails = |lists: usize, bounds: std::ops::RangeInclusive<usize>| {
        std::panic::catch_unwind(|| assert_a_quiet_window(&window(lists), 6, false, bounds)).is_err()
    };
    // The Owner loop: W - 1 to W + 1.
    assert!(fails(4, 5..=7), "four lists in six seconds");
    assert!(!fails(5, 5..=7) && !fails(6, 5..=7) && !fails(7, 5..=7));
    assert!(fails(8, 5..=7), "a busy loop");
    // The Fleet loop: W - 2 to W + 1.
    assert!(fails(3, 4..=7), "three lists in six seconds");
    assert!(!fails(4, 4..=7) && !fails(7, 4..=7));
}

#[tokio::test(start_paused = true)]
async fn ct55b_completed_tick_window_does_not_extend_past_w() {
    // A window is W seconds long. A tick that completes after it has closed is not in
    // it, however soon after, and the window is not stretched to wait for one.
    let t0 = Instant::now();
    let checkpoint = |millis: u64, requests: usize| TickCheckpoint {
        at: t0 + Duration::from_millis(millis),
        requests,
        tick_commits: requests,
        lone_reservation_commits: 0,
        settings_refreshes_completed: 0,
        iterations: requests,
        account: "collecting",
    };
    let first = checkpoint(0, 10);
    let ticks = vec![
        checkpoint(1_000, 11),
        checkpoint(2_000, 12),
        checkpoint(3_000, 13),
        checkpoint(4_000, 14),
        checkpoint(5_000, 15),
        // Completes 1 ms after a 6 s window closed, and one more a second later.
        checkpoint(6_001, 16),
        checkpoint(7_001, 17),
    ];
    let last = last_completed_tick_in_window(first, &ticks, 6);
    assert_eq!(last.requests, 15, "the tick after W is excluded: {last:?}");
    // A completion exactly at the close is inside it.
    let mut inclusive = ticks.clone();
    inclusive[5] = checkpoint(6_000, 16);
    assert_eq!(last_completed_tick_in_window(first, &inclusive, 6).requests, 16);
    // A window with no completion inside it is the opening tick alone.
    assert_eq!(last_completed_tick_in_window(first, &ticks[5..], 6).requests, 10);
    // The paused clock moves exactly: a window closes at W and no later.
    tokio::time::sleep_until(first.at + Duration::from_secs(6)).await;
    assert_eq!(Instant::now() - t0, Duration::from_secs(6));
}

/// A running Fleet loop over a store with one configured Fleet car, the in-process
/// fake Fleet server of this module and the test seam. The cadence is one second
/// for every interval, so a window of seconds shows the loop's waiting rule.
struct FleetLoopBed {
    temporary: tempfile::TempDir,
    store: HubStore,
    /// The loop's credential manager, so a test can start an exchange or look at it.
    manager: Arc<tokio::sync::Mutex<FleetAuthManager>>,
    fake: FleetFake,
    seam: Arc<SupervisedCollectorTestSeam>,
    task: Option<JoinHandle<Result<(), CollectorError>>>,
    shutdown: Option<oneshot::Sender<()>>,
    server: JoinHandle<()>,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
}

impl FleetLoopBed {
    async fn start(reported_state: &str, driving: bool, allow_refresh: bool) -> Self {
        Self::start_with_auth_timeout(reported_state, driving, allow_refresh, Duration::from_secs(2))
            .await
    }

    /// As [`Self::start`], with the token endpoint's request timeout set: a test that
    /// holds an exchange in flight needs it to outlast the test.
    async fn start_with_auth_timeout(
        reported_state: &str,
        driving: bool,
        allow_refresh: bool,
        auth_timeout: Duration,
    ) -> Self {
        Self::start_with_seam(reported_state, driving, allow_refresh, auth_timeout, |_| {}).await
    }

    /// As [`Self::start_with_auth_timeout`]; `setup` runs against the loop's test seam
    /// before the loop starts (gates, hooks and a controlled cadence).
    async fn start_with_seam(
        reported_state: &str,
        driving: bool,
        allow_refresh: bool,
        auth_timeout: Duration,
        setup: impl FnOnce(&SupervisedCollectorTestSeam),
    ) -> Self {
        crate::crypto::install_default_provider();
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        finish_collection_for_provider(
            &store,
            &cursor_key,
            &ManualCollection {
                vehicles: vec![Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online")],
                snapshots: Vec::new(),
                failures: Vec::new(),
            },
            CollectorProvider::Fleet,
        )
        .await
        .expect("configure the car");
        let credentials = FleetSetupCredentials::new(
            CT67_SCOPED_ACCESS.to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("setup credentials");
        crate::fleet_credentials::persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            SystemTime::now(),
        )
        .expect("persist credentials");
        let admission =
            crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
        let manager = FleetAuthManager::from_store_for_admitted_user(
            store.clone(),
            temporary.path(),
            Arc::clone(&admission),
            None,
        )
        .expect("manager");
        let manager = Arc::new(tokio::sync::Mutex::new(manager));
        let fake = FleetFake {
            lists: Arc::new(AtomicUsize::new(0)),
            reads: Arc::new(AtomicUsize::new(0)),
            state: Arc::new(Mutex::new(reported_state.to_owned())),
            timestamp_ms: Arc::new(std::sync::atomic::AtomicI64::new(0)),
            driving: Arc::new(std::sync::atomic::AtomicBool::new(driving)),
            store: store.clone(),
            audit_state: true,
            violations: Arc::new(Mutex::new(Vec::new())),
            list_status: Arc::new(std::sync::atomic::AtomicU16::new(200)),
            read_status: Arc::new(std::sync::atomic::AtomicU16::new(200)),
            token_expired: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reject_every_token: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            token_requests: Arc::new(AtomicUsize::new(0)),
            token_status: Arc::new(std::sync::atomic::AtomicU16::new(200)),
            token_hold: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            list_hold: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rotate_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            authorisations: Arc::new(Mutex::new(Vec::new())),
            cars: 1,
            list_script: Arc::new(Mutex::new(None)),
            list_responses: Arc::new(Mutex::new(Vec::new())),
            list_sequence: Arc::new(AtomicUsize::new(0)),
            transport: FleetTransportControl::default(),
        };
        let router = Router::new()
            .route("/api/1/vehicles", get(fleet_fake_list))
            .route("/api/1/vehicles/{vin}/vehicle_data", get(fleet_fake_read))
            .route("/oauth2/v3/token", post(fleet_fake_token))
            .with_state(fake.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("fake server");
        });
        let base = url::Url::parse(&format!("http://{address}/")).expect("base");
        let api = FleetApi::for_fake_http(base.clone(), LOOP_BED_HTTP_TIMEOUT).expect("client");
        let auth_api = FleetAuthApi::for_fake_http(
            base.join("oauth2/v3/token").expect("token URL"),
            auth_timeout,
        )
        .expect("auth client");
        let mut config = supervised_restart_test_config(temporary.path());
        config.collector.provider = crate::config::CollectorProvider::Fleet;
        let one_second = Duration::from_secs(1);
        let cadence = CollectorCadence {
            fleet: crate::config::FleetCadence {
                driving: one_second,
                charging: one_second,
                updating: one_second,
                online: one_second,
                state_check: Some(one_second),
                inventory: Some(one_second),
                ..crate::config::FleetCadence::DEFAULT
            },
            ..supervised_restart_test_cadence()
        };
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        {
            // Each completed tick reads the lists the fake has answered.
            let lists = Arc::clone(&fake.lists);
            seam.set_requests_probe(Arc::new(move || lists.load(Ordering::SeqCst)));
        }
        setup(&seam);
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
                allow_refresh,
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
                panic!("the Fleet loop ended before it was ready: {result:?}");
            }
            Err(_) => {
                task.abort();
                panic!("Fleet loop readiness timeout");
            }
        }
        Self {
            temporary,
            store,
            manager,
            fake,
            seam,
            task: Some(task),
            shutdown: Some(shutdown_tx),
            server,
            admission,
        }
    }

    fn lists(&self) -> usize {
        self.fake.lists.load(Ordering::SeqCst)
    }

    fn reads(&self) -> usize {
        self.fake.reads.load(Ordering::SeqCst)
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let mut task = self.task.take().expect("task");
        join_supervised_restart_task("Fleet loop join", &mut task)
            .await
            .expect("loop result");
        self.server.abort();
    }

    async fn wait_for(&mut self, label: &str, condition: impl FnMut() -> bool) {
        let task = self.task.as_mut().expect("task");
        k6_wait_for(task, label, condition).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct55b_the_fleet_loop_with_every_car_asleep_makes_one_list_a_second_and_writes_only_counts() {
    let clock = controlled_cadence_clock();
    let mut control = None;
    let bed = FleetLoopBed::start_with_seam(
        "asleep",
        false,
        false,
        Duration::from_secs(2),
        |seam| {
            control = Some(seam.control_cadence(&clock));
        },
    )
    .await;
    let control = control.expect("the controlled cadence");
    let first = warm_up_the_controlled_loop(&control, |tick| tick.requests >= 2).await;
    let mut held = None;
    let lists = || bed.fake.lists.load(Ordering::SeqCst);
    // A window of completed ticks, W seconds: the Fleet loop makes W - 2 to W + 1 lists.
    let window =
        observe_controlled_window(&bed.seam, &control, &mut held, &bed.store, first, 6).await;
    // A Fleet list is a billed request (R24): its count is written by the tick that
    // sent it, because the cost estimate and the cap must survive a crash (R5, R25,
    // R27). No scheduler row and no observation is written for an empty poll, and a
    // tick commit comes with a list, never without one: the commits are at most the
    // lists the window measured plus the lone reservation commits it actually made.
    assert_a_quiet_window(&window, 6, true, 6 - 2..=6 + 1);
    assert!(
        window.tick_commits <= window.lists + window.lone_reservation_commits,
        "{} commits for {} lists and {} lone reservation commits",
        window.tick_commits,
        window.lists,
        window.lone_reservation_commits
    );
    assert_eq!(
        window.rows_before.but_for_counters(),
        window.rows_after.but_for_counters(),
        "no gap, no observation and no scheduler row is written for an empty poll"
    );
    assert_eq!(bed.reads(), 0, "the car sleeps: no read");
    assert_the_floor_holds_with_a_due_time_in_the_past(&bed.seam, &control, &mut held, lists).await;
    bed.stop().await;
}

/// Step a controlled loop (see [`CadenceControl`]), granting every timer it asks for at
/// the duration it asked for, until `arrived` says its next list request has reached
/// the double. The loop is then inside that request, so no timer is outstanding and
/// the clock stands still.
async fn step_the_controlled_loop_until(control: &CadenceControl, arrived: impl Fn() -> bool) {
    loop {
        if arrived() {
            return;
        }
        let event = tokio::select! {
            event = control.next() => event,
            () = async {
                while !arrived() {
                    sleep(Duration::from_millis(2)).await;
                }
            } => return,
        };
        if let CadenceEvent::Wait { at, duration } = event {
            let wake = at + duration;
            let now = control.clock().mono_now();
            if wake > now {
                control.clock().advance(wake - now);
            }
        }
        control.resume();
    }
}

/// The acknowledged delayed-I/O control of CT55b (R-P2F3-2). One list response is held
/// for 3 s of real time while the cadence clock stands still: the loop sits inside the
/// request, asks for no timer, and the clock must not have moved when the response is
/// released. The held list then completes like any other: no discovery failure is
/// logged (the beds' HTTP timeout is far longer than the hold), the account state is
/// the same, and the window's counts and the store are unchanged. `hold` is the
/// double's switch, `lists` its list counter, `first` the tick the loop is held after.
async fn assert_a_delayed_list_changes_nothing(
    seam: &SupervisedCollectorTestSeam,
    control: &CadenceControl,
    store: &HubStore,
    hold: &AtomicBool,
    lists: impl Fn() -> usize,
    first: TickCheckpoint,
    skip_counters: bool,
) {
    let rows_before = collector_rows_stamp(store);
    let failures = |seam: &SupervisedCollectorTestSeam| {
        seam.loop_log()
            .iter()
            .filter(|event| **event == LoopEvent::DiscoveryFailed)
            .count()
    };
    let failures_before = failures(seam);
    let lists_before = lists();
    hold.store(true, Ordering::SeqCst);
    control.resume();
    step_the_controlled_loop_until(control, || lists() > lists_before).await;
    let frozen = control.clock().mono_now();
    // Three seconds of real time with the response held: the loop is inside the
    // request and the logical clock is not touched by it.
    sleep(Duration::from_secs(3)).await;
    assert_eq!(control.clock().mono_now(), frozen, "the cadence clock stood still");
    assert_eq!(failures(seam), failures_before, "no discovery failure while the list is held");
    hold.store(false, Ordering::SeqCst);
    let CadenceEvent::Tick(tick) = control.next().await else {
        panic!("a timer was asked for before the held list's tick");
    };
    assert_eq!(tick.at, frozen, "the held list's tick completed at the same logical instant");
    assert_eq!(tick.requests, first.requests + 1, "exactly the held list was answered");
    assert_eq!(tick.account, first.account, "the account state is unchanged");
    assert_eq!(failures(seam), failures_before, "no discovery failure was logged");
    // Owner: nothing is written. Fleet: the billed list's count is the only write (as
    // in the quiet window of CT55b), so the counters' stamp is left out of the check.
    let rows_after = collector_rows_stamp(store);
    if skip_counters {
        assert_eq!(rows_before.but_for_counters(), rows_after.but_for_counters(), "the store is unchanged");
    } else {
        assert_eq!(rows_before, rows_after, "the store is unchanged");
    }
    if skip_counters {
        assert!(tick.tick_commits <= first.tick_commits + 1 + (tick.lone_reservation_commits - first.lone_reservation_commits));
    } else {
        assert_eq!(tick.tick_commits, first.tick_commits, "no tick commit");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct55b_a_delayed_owner_list_response_changes_nothing_in_the_logical_window() {
    let clock = controlled_cadence_clock();
    let mut control = None;
    let bed = LoopBed::start_with_seam(LoopListMode::AsleepCar, |_| {}, None, |seam| {
        control = Some(seam.control_cadence(&clock));
    })
    .await;
    let control = control.expect("the controlled cadence");
    let first = warm_up_the_controlled_loop(&control, |tick| tick.requests >= 2).await;
    assert_a_delayed_list_changes_nothing(
        &bed.seam,
        &control,
        &bed.store,
        &bed.wire.hold,
        || bed.wire.calls.load(Ordering::SeqCst),
        first,
        false,
    )
    .await;
    bed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct55b_a_delayed_fleet_list_response_changes_nothing_in_the_logical_window() {
    let clock = controlled_cadence_clock();
    let mut control = None;
    let bed = FleetLoopBed::start_with_seam("asleep", false, false, Duration::from_secs(2), |seam| {
        control = Some(seam.control_cadence(&clock));
    })
    .await;
    let control = control.expect("the controlled cadence");
    let first = warm_up_the_controlled_loop(&control, |tick| tick.requests >= 2).await;
    assert_a_delayed_list_changes_nothing(
        &bed.seam,
        &control,
        &bed.store,
        &bed.fake.list_hold,
        || bed.fake.lists.load(Ordering::SeqCst),
        first,
        true,
    )
    .await;
    bed.stop().await;
}

// ---- CT74: must-stop outcomes (C3 R47, B2 clarification) --------------------------------------

/// The Hub id of the one configured car.
fn only_configured_hub_id(store: &HubStore) -> Uuid {
    store
        .configured_tesla_vehicles()
        .expect("configured")
        .first()
        .expect("one car")
        .0
}

fn open_auth_terminal_gaps(store: &HubStore) -> usize {
    store
        .collection_gaps(50)
        .expect("gaps")
        .iter()
        .filter(|gap| gap.kind == "auth_terminal" && gap.ended_at_ms.is_none())
        .count()
}

/// What the CT74a scripts saw at the arrival of a list, in the order they saw it. Every
/// note names the list by its ordinal and the loop iteration it arrived in.
#[derive(Clone, Copy, Debug)]
enum ListArrival {
    /// The list's resend was not covered by the persisted reservation: answered as
    /// usual, the refusal waits for a list whose resend is covered.
    Declined,
    /// The list met the refusal, and so does its resend (the status is armed before the
    /// first response goes out). `reserved` and `sent` are the counters it met.
    Refused { ordinal: usize, iteration: usize, reserved: i64, sent: i64 },
    /// The first list after the refused one: the refused list's resend.
    Resend { ordinal: usize, iteration: usize, reserved: i64 },
    /// The retry's list: the loop iteration it is in, and the iteration of the newest
    /// completed tick at that moment.
    Retry { ordinal: usize, in_iteration: usize, last_completed_iteration: Option<usize> },
}

/// The refusal of CT74a, scripted for the account list (C3 R47): the first list whose own
/// request and resend are both covered by the persisted reservation (`reserved - sent >=
/// 2` and `reserved >= ordinal + 1`, read while that list is unanswered) is answered 401
/// and the status is armed for its resend before the response goes out; the lists before
/// it are answered as usual. The refused list and its resend are noted with their
/// ordinals and iterations, so the test can follow those exact requests through the
/// double's response log.
fn refuse_the_list_whose_resend_is_covered(
    fake: &FleetFake,
    seam: &Arc<SupervisedCollectorTestSeam>,
    notes: &Arc<Mutex<Vec<ListArrival>>>,
) -> ListScript {
    let probe = fake.clone();
    let seam = Arc::clone(seam);
    let status = Arc::clone(&fake.list_status);
    let notes = Arc::clone(notes);
    let refused = Arc::new(AtomicBool::new(false));
    let resend_noted = Arc::new(AtomicBool::new(false));
    Arc::new(move |ordinal| {
        let (reserved, sent) = probe.state_reservation();
        let iteration = seam.loop_iterations();
        if refused.load(Ordering::SeqCst) {
            if !resend_noted.swap(true, Ordering::SeqCst) {
                notes.lock().expect("notes").push(ListArrival::Resend {
                    ordinal,
                    iteration,
                    reserved,
                });
            }
            return None;
        }
        let ordinal_i = i64::try_from(ordinal).expect("ordinal");
        if reserved - sent >= 2 && reserved > ordinal_i {
            refused.store(true, Ordering::SeqCst);
            status.store(401, Ordering::SeqCst);
            notes.lock().expect("notes").push(ListArrival::Refused {
                ordinal,
                iteration,
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

/// The retry's list of CT74a, scripted: the first list to arrive is noted with the
/// loop's iteration and the newest completed tick as they are while it is unanswered,
/// and is answered as usual.
fn note_the_retry_list(
    seam: &Arc<SupervisedCollectorTestSeam>,
    notes: &Arc<Mutex<Vec<ListArrival>>>,
) -> ListScript {
    let seam = Arc::clone(seam);
    let notes = Arc::clone(notes);
    let noted = Arc::new(AtomicBool::new(false));
    Arc::new(move |ordinal| {
        if !noted.swap(true, Ordering::SeqCst) {
            notes.lock().expect("notes").push(ListArrival::Retry {
                ordinal,
                in_iteration: seam.loop_iterations(),
                last_completed_iteration: seam
                    .tick_checkpoints()
                    .last()
                    .map(|tick| tick.iterations),
            });
        }
        None
    })
}

/// Resume a controlled loop held at an event and step it, granting every timer it asks
/// for, until a completed tick for which `reached` holds (see
/// [`warm_up_the_controlled_loop`]); the loop is then held right after that tick. It is
/// bounded in the loop's logical time (R-P2F4-3):
/// the traversal fails when the scheduler's clock reaches the `bound` after `origin`
/// without a qualifying completed tick (a retry that is never applied leaves the loop
/// ticking and waiting for ever). A separate watchdog over the whole traversal, on real
/// time and not an acceptance bound, catches a traversal that stalls.
async fn step_the_controlled_loop_to_a_tick_within(
    control: &CadenceControl,
    origin: Instant,
    bound: Duration,
    reached: impl Fn(&TickCheckpoint) -> bool,
) -> TickCheckpoint {
    control.resume();
    warm_up_the_controlled_loop_within(control, origin, bound, reached).await
}

/// [`warm_up_the_controlled_loop`] bounded as [`step_the_controlled_loop_to_a_tick_within`]
/// is, for a loop that is not held yet (R-P2F5-2).
async fn warm_up_the_controlled_loop_within(
    control: &CadenceControl,
    origin: Instant,
    bound: Duration,
    reached: impl Fn(&TickCheckpoint) -> bool,
) -> TickCheckpoint {
    match timeout(
        TRAVERSAL_STALL_WATCHDOG,
        warm_up_the_controlled_loop_by(control, Some(origin + bound), reached),
    )
    .await
    {
        Ok(Some(tick)) => tick,
        Ok(None) => panic!(
            "no qualifying tick completed within {bound:?} of the origin on the scheduler's clock"
        ),
        Err(_) => panic!("the traversal stalled: the watchdog ended it (not an acceptance bound)"),
    }
}

/// The real-time watchdog over one bounded traversal of a controlled loop: it is no
/// acceptance bound, it only ends a traversal that does not move at all.
const TRAVERSAL_STALL_WATCHDOG: Duration = Duration::from_secs(120);

/// Step a controlled loop that is not held yet, granting every timer it asks for at the
/// duration it asked for, until `done` holds at a hold of the loop (R-P2F5-1). Returns
/// the time the scheduler's clock had passed since `origin` when the hold that saw it
/// was reached (before that hold's own timer is granted); the loop stays held at that
/// event, its timer granted, and the caller resumes it. The traversal is bounded on the
/// scheduler's clock inside itself: it is `Err` as soon as the loop asks for a timer
/// that would fire after `origin + bound` while `done` does not hold, with that timer
/// not granted, so a loop that goes on ticking never lets the clock pass the bound.
/// `done` may itself fail (`Err`). A separate real-time watchdog over the whole
/// traversal (no acceptance bound) is `Err` too. The caller aborts and joins the run
/// it started when this is `Err`.
async fn step_the_controlled_loop_until_within(
    control: &CadenceControl,
    origin: Instant,
    bound: Duration,
    mut done: impl FnMut() -> Result<bool, String>,
) -> Result<Duration, String> {
    let deadline = origin + bound;
    let traversal = async {
        loop {
            let event = control.next().await;
            let handed_over = done()?;
            let elapsed = control.clock().mono_now().saturating_duration_since(origin);
            if let CadenceEvent::Wait { at, duration } = event {
                let wake = at + duration;
                if !handed_over && wake > deadline {
                    return Err(format!(
                        "not done within {bound:?} of the origin on the scheduler's clock \
                         (the loop asked for a timer ending {:?} after it; none was granted)",
                        wake.saturating_duration_since(origin)
                    ));
                }
                let now = control.clock().mono_now();
                if wake > now {
                    control.clock().advance(wake - now);
                }
            }
            if handed_over {
                // The loop stays held at this event: the next step resumes it.
                return Ok(elapsed);
            }
            control.resume();
        }
    };
    match timeout(TRAVERSAL_STALL_WATCHDOG, traversal).await {
        Ok(outcome) => outcome,
        Err(_) => Err("the traversal stalled: the watchdog ended it (not an acceptance bound)".to_owned()),
    }
}

/// Poll `condition` (bounded) with the loop held: for work of the test's tasks that
/// needs no step of the loop (the watchdog is not an acceptance bound).
async fn wait_for_background_work(label: &str, mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(30), async {
        while !condition() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label}: not reached"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r_p2f5_a_handoff_that_never_comes_fails_on_the_scheduler_clock_while_ticks_continue() {
    // R-P2F5-1. A controlled loop goes on ticking while a handoff sender stays alive and
    // sends nothing: the traversal that waits for the handoff fails at its two seconds
    // on the scheduler's clock, with no timer past them granted and in bounded real time
    // (the loop's ticks and per-event waits alone would keep it going for ever). The same
    // traversal with a handoff sent at the third hold returns the time it was seen.
    for sent in [false, true] {
        let clock = controlled_cadence_clock();
        let mut control = None;
        let bed = FleetLoopBed::start_with_seam(
            "asleep",
            false,
            false,
            Duration::from_secs(2),
            |seam| {
                control = Some(seam.control_cadence(&clock));
            },
        )
        .await;
        let control = control.expect("the controlled cadence");
        let (handoff_tx, mut handoff_rx) = oneshot::channel::<()>();
        let mut handoff_tx = Some(handoff_tx);
        let mut holds = 0;
        let origin = clock.mono_now();
        let bound = Duration::from_secs(2);
        let outcome = step_the_controlled_loop_until_within(&control, origin, bound, || {
            holds += 1;
            if sent && holds == 3 {
                handoff_tx.take().expect("sent once").send(()).expect("the receiver is held");
            }
            match handoff_rx.try_recv() {
                Ok(()) => Ok(true),
                Err(oneshot::error::TryRecvError::Empty) => Ok(false),
                Err(oneshot::error::TryRecvError::Closed) => Err("handoff dropped".to_owned()),
            }
        })
        .await;
        // The sender is alive in the unsent case all the while (it is dropped here).
        assert_eq!(handoff_tx.is_some(), !sent);
        let moved = clock.mono_now().saturating_duration_since(origin);
        assert!(moved <= bound, "no timer past the bound was granted: {moved:?}");
        if sent {
            let elapsed = outcome.expect("the handoff was seen");
            assert!(elapsed <= bound, "{elapsed:?}");
            assert_eq!(holds, 3);
        } else {
            let failure = outcome.expect_err("a handoff that never comes fails");
            assert!(
                failure.contains("on the scheduler's clock"),
                "the traversal failed by its logical bound: {failure}"
            );
            assert!(holds >= 2, "the loop went on while the sender stayed live: {holds} holds");
            assert!(
                !bed.seam.tick_checkpoints().is_empty(),
                "ticks continued while the handoff was missing"
            );
        }
        bed.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct74a_a_fleet_401_after_the_refresh_is_a_credential_stop_inside_the_loop() {
    // The car drives, so a drive is open. The credential is then refused whatever
    // token a request carries: the read's resend after the tick's refresh gets 401.
    // `CredentialStop` and one `auth_terminal` gap; no request while stopped, though
    // the loop goes on (settings refresh, owner actions, R40, status); the open drive
    // gets its `no_data` event at `T_gap` (the test cadence's one second); a
    // `retry_collection` row is applied by the next settings refresh, with exactly one
    // list sent.
    // The loop runs under a controlled cadence (R-P2F3-8): its scheduler reads the clock
    // the test moves by exactly the timers the loop asks for, and the retry's bound,
    // `CONTROL_SETTINGS_REFRESH + 2 s`, is measured on that clock from one origin taken
    // before the retry row's insertion, so no load of the machine can move it.
    // The token exchange's own timeout is far beyond what a loaded machine can make
    // it miss: an exchange whose answer is late would leave an unresolved refresh (a
    // different stop, which the owner's retry does not lift), and that is not what this
    // test is about.
    let clock = controlled_cadence_clock();
    let mut control = None;
    let bed = FleetLoopBed::start_with_seam(
        "online",
        true,
        true,
        Duration::from_secs(30),
        |seam| {
            control = Some(seam.control_cadence(&clock));
        },
    )
    .await;
    let control = control.expect("the controlled cadence");
    let hub = only_configured_hub_id(&bed.store);
    let store = bed.store.clone();
    let drive_open = |store: &HubStore| {
        store
            .collector_lifecycle_snapshot(hub)
            .map(|view| view.drive_open)
            .unwrap_or(false)
    };
    // The drive's reads, and the open drive.
    let reads = Arc::clone(&bed.fake.reads);
    // Every step of this test is bounded in the loop's logical time (R-P2F5-2): a
    // missing event fails on the scheduler's clock, never by running until a guard ends
    // the test. The bounds are generous (several cadences of the bed's one second and of
    // the settings refresh), each measured from the clock at the step's start.
    let step_bound = 4 * CONTROL_SETTINGS_REFRESH;
    let tick_bound = CONTROL_SETTINGS_REFRESH + Duration::from_secs(2);
    warm_up_the_controlled_loop_within(&control, clock.mono_now(), step_bound, |_| {
        reads.load(Ordering::SeqCst) >= 3 && drive_open(&store)
    })
    .await;
    // The account list is refused whatever token it carries: the list's resend after the
    // tick's refresh gets 401, which is the credential stop. Only the list is refused, so
    // the stop is made by one request, the first of the tick.
    //
    // The refusal is chosen by the double while that list is unanswered, and only for a
    // list whose resend the persisted reservation also covers (C3 R27: a long-running
    // process reserves in blocks). P3b: a withheld resend followed by an immediate second
    // forced refresh waits out the credential unit's spacing while holding the manager.
    // This setup selects the covered-resend case to prove CT74a's own claim (a 401 after
    // a refresh is a credential stop inside the loop); it does not close that P3b item.
    let notes = Arc::new(Mutex::new(Vec::<ListArrival>::new()));
    bed.fake
        .set_list_script(Some(refuse_the_list_whose_resend_is_covered(&bed.fake, &bed.seam, &notes)));
    step_the_controlled_loop_to_a_tick_within(&control, clock.mono_now(), step_bound, |_| {
        open_auth_terminal_gaps(&store) == 1
    })
    .await;
    // The exact refused request and its resend (R-P2F3-3): the arrival notes name them
    // by ordinal and iteration with the coverage they met; the double's response log
    // says what each was released with and in which order. Both were answered 401, the
    // resend arrived after the refused request was released, in the same iteration.
    let (refused, resend) = {
        let all = notes.lock().expect("notes");
        let refused: Vec<ListArrival> = all
            .iter()
            .copied()
            .filter(|note| matches!(note, ListArrival::Refused { .. }))
            .collect();
        let resends: Vec<ListArrival> = all
            .iter()
            .copied()
            .filter(|note| matches!(note, ListArrival::Resend { .. }))
            .collect();
        let ([refused], [resend]) = (&refused[..], &resends[..]) else {
            panic!("one refusal and its resend were scripted: {all:?}");
        };
        (*refused, *resend)
    };
    let ListArrival::Refused { ordinal: refused_ordinal, iteration: refused_iteration, reserved, sent } =
        refused
    else {
        unreachable!("filtered")
    };
    let ListArrival::Resend { ordinal: resend_ordinal, iteration: resend_iteration, reserved: resend_reserved } =
        resend
    else {
        unreachable!("filtered")
    };
    let responses = bed.fake.list_responses.lock().expect("list responses").clone();
    let response_of = |ordinal: usize| {
        responses
            .iter()
            .copied()
            .find(|response| response.ordinal == ordinal)
            .unwrap_or_else(|| panic!("list {ordinal} was released: {responses:?}"))
    };
    let (first_response, second_response) = (response_of(refused_ordinal), response_of(resend_ordinal));
    assert_eq!(first_response.status, 401, "the inspected list itself got the refusal: {responses:?}");
    assert_eq!(second_response.status, 401, "its resend got the refusal: {responses:?}");
    assert!(
        first_response.arrived < first_response.released
            && first_response.released < second_response.arrived
            && second_response.arrived < second_response.released,
        "the refused list was released before its resend arrived: {responses:?}"
    );
    assert!(reserved - sent >= 2, "the refusal met a list and a resend that were both covered");
    assert_eq!(resend_ordinal, refused_ordinal + 1, "the resend is the very next list");
    assert!(
        i64::try_from(resend_ordinal).expect("ordinal") <= resend_reserved,
        "the resend was covered by the reservation it met"
    );
    assert_eq!(resend_iteration, refused_iteration, "the resend is of the refused list's iteration");
    // The refresh the tick made has stored its successor (its completion runs in a task
    // of its own): a 401 after a refresh is a credential stop, not an unresolved
    // refresh, which the owner's retry would not lift.
    let receipt_store = bed.store.clone();
    wait_for_background_work("the refresh exchange is settled", || {
        !receipt_store.identified_unresolved_fleet_refresh().unwrap_or(true)
    })
    .await;
    // The tick that stopped the account is over once a later tick has completed, and
    // that one, with the account stopped, is the baseline: nothing more is sent while
    // the account is stopped (the loop's completed ticks are the witness, not a sleep).
    step_the_controlled_loop_to_a_tick_within(&control, clock.mono_now(), tick_bound, |_| true).await;
    let stopped =
        step_the_controlled_loop_to_a_tick_within(&control, clock.mono_now(), tick_bound, |_| true).await;
    assert_eq!(stopped.account, "credential_stop", "the completed tick ran with the account stopped");
    let (reads_now, refreshes) = (bed.reads(), stopped.settings_refreshes_completed);
    assert_eq!(stopped.requests, bed.lists(), "no list in flight at a completed tick");
    // The open drive is closed by the Hub's own event at `T_gap`, with no request.
    // (`T_gap` is the test cadence's one second, then a tick: one more cadence.)
    step_the_controlled_loop_to_a_tick_within(
        &control,
        clock.mono_now(),
        Duration::from_secs(1) + tick_bound,
        |_| !drive_open(&store),
    )
    .await;
    // The loop goes on while stopped: the settings refresh (and with it the owner's
    // actions) completes again, and still nothing is sent.
    step_the_controlled_loop_to_a_tick_within(&control, clock.mono_now(), tick_bound, |tick| {
        tick.settings_refreshes_completed > refreshes
    })
    .await;
    let latest =
        step_the_controlled_loop_to_a_tick_within(&control, clock.mono_now(), tick_bound, |_| true).await;
    assert_eq!(
        (latest.requests, bed.reads()),
        (stopped.requests, reads_now),
        "no request while stopped"
    );
    assert_eq!(open_auth_terminal_gaps(&bed.store), 1, "one gap, however long the stop");
    // The owner's retry, with the credential accepted again. The double notes the retry's
    // list while it is unanswered: the loop is inside the iteration of that list and no
    // completed tick belongs to that iteration yet.
    bed.fake.list_status.store(200, Ordering::SeqCst);
    bed.fake.set_list_script(Some(note_the_retry_list(&bed.seam, &notes)));
    let lists_before = latest.requests;
    // One origin on the scheduler's clock, taken before the retry row is inserted: it
    // selects the checkpoint (a tick that completed before it is no answer to the row)
    // and starts the unchanged bound, the next settings refresh and two seconds.
    let origin = control.clock().mono_now();
    store
        .insert_collector_owner_action(
            "retry_collection",
            None,
            current_epoch_millis().expect("clock"),
        )
        .expect("retry row");
    let deadline = origin + CONTROL_SETTINGS_REFRESH + Duration::from_secs(2);
    // The retry tick is the first completed tick, at or after the origin, that sent a
    // list; the loop is stepped by its own timers and is held right after it.
    let retry_tick = step_the_controlled_loop_to_a_tick_within(
        &control,
        origin,
        CONTROL_SETTINGS_REFRESH + Duration::from_secs(2),
        |tick| tick.at >= origin && tick.requests > lists_before,
    )
    .await;
    assert!(
        retry_tick.at <= deadline,
        "the retry tick completed {:?} after the origin on the scheduler's clock (bound {:?})",
        retry_tick.at.saturating_duration_since(origin),
        CONTROL_SETTINGS_REFRESH + Duration::from_secs(2)
    );
    let retry = notes
        .lock()
        .expect("notes")
        .iter()
        .copied()
        .find(|note| matches!(note, ListArrival::Retry { .. }))
        .expect("the retry list");
    let ListArrival::Retry { ordinal, in_iteration, last_completed_iteration } = retry else {
        unreachable!("filtered");
    };
    assert_eq!(ordinal, lists_before + 1, "the first list after the stop is the retry's");
    assert!(
        last_completed_iteration.is_none_or(|completed| completed < in_iteration),
        "no completed tick belongs to the iteration of the unanswered retry list \
         (iteration {in_iteration}, newest completed tick of iteration {last_completed_iteration:?})"
    );
    assert_eq!(retry_tick.requests, lists_before + 1, "exactly one list after the retry");
    assert!(
        retry_tick.iterations >= in_iteration,
        "the retry tick is the iteration of the retry's list or a later one"
    );
    let applied: i64 = bed
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT COUNT(*) FROM collector_owner_actions WHERE applied_at_ms IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("applied actions");
    assert_eq!(applied, 1, "the retry row is applied");
    assert_eq!(open_auth_terminal_gaps(&bed.store), 0, "the gap closed");
    bed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct74b_the_loss_of_sensitive_access_ends_the_run_with_no_stop_no_gap_and_no_write() {
    // The admission's revalidation fails while the loop runs: the run returns
    // `SensitiveAccessUnavailable` within that iteration, the account row is
    // unchanged, no gap row is written (no `auth_terminal`, no `hub_stopped`) and no
    // collector row is written after the failure. A later start with the last
    // heartbeat more than `K_GAP_MIN` old records one `hub_down` gap.
    let mut bed = FleetLoopBed::start("asleep", false, false).await;
    let lists_seen = Arc::clone(&bed.fake.lists);
    bed.wait_for("the first lists", || lists_seen.load(Ordering::SeqCst) >= 2).await;
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
    let account_before = account(&bed.store);
    let gaps_before = bed.store.collection_gaps(50).expect("gaps").len();
    let lease_before = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the active lease");
    // The lock file of the data directory is replaced by another file, as when
    // another process has taken the instance over.
    let lock = bed.temporary.path().join(crate::user_lifetime_lock::LOCK_FILE_NAME);
    std::fs::remove_file(&lock).expect("remove the lock file");
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock)
            .expect("a different lock file");
    }
    let mut task = bed.task.take().expect("task");
    let result = timeout(Duration::from_secs(10), &mut task)
        .await
        .expect("the run ends within a few iterations")
        .expect("join");
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    assert_eq!(account(&bed.store), account_before, "the account row is unchanged");
    let gaps = bed.store.collection_gaps(50).expect("gaps");
    assert_eq!(gaps.len(), gaps_before, "no gap row is written: {gaps:?}");
    assert!(gaps.iter().all(|gap| gap.kind != "auth_terminal" && gap.kind != "hub_stopped"));
    // The lease is retained with the heartbeat of its last renewal, unchanged by the
    // loss, and a later admitted start beyond its expiry and the minimum records the
    // downtime from that heartbeat, as one `[H, S]` gap (R8).
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the lease is retained to expire");
    assert_eq!(retained.instance_id, lease_before.instance_id);
    assert!(retained.heartbeat_at_ms >= lease_before.heartbeat_at_ms);
    assert_eq!(
        retained.lease_until_ms - retained.heartbeat_at_ms,
        lease_before.lease_until_ms - lease_before.heartbeat_at_ms,
        "the expiry follows the stored heartbeat"
    );
    assert_an_unrecorded_stop_recovers_with_one_downtime_gap(&bed.store, "fleet_polling");
    bed.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct74b_the_owner_loop_ends_on_the_loss_of_sensitive_access_with_no_stop_no_gap_and_no_write() {
    // The same as the Fleet loop's case, on the Owner wire: the credential unit's
    // check of the process's sensitive access fails while the loop runs. The run
    // returns `SensitiveAccessUnavailable`; no `CredentialStop`, no `auth_terminal`
    // gap, no `hub_stopped` gap, and no collector row is written by the run's end.
    let lose = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut bed =
        LoopBed::start_with_admission(LoopListMode::AsleepCar, |_| {}, Some(Arc::clone(&lose)))
            .await;
    let calls = Arc::clone(&bed.wire.calls);
    bed.wait_for("the first lists", || calls.load(Ordering::SeqCst) >= 2).await;
    let account_before: Option<(String, i64)> = bed
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT state, updated_at_ms FROM collector_account_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    let gaps_before = bed.store.collection_gaps(50).expect("gaps").len();
    let lease_before = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the active lease");
    lose.store(true, Ordering::SeqCst);
    let mut task = bed.task.take().expect("task");
    let result = timeout(Duration::from_secs(10), &mut task)
        .await
        .expect("the run ends within a few iterations")
        .expect("join");
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    let account_after: Option<(String, i64)> = bed
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT state, updated_at_ms FROM collector_account_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    assert_eq!(account_after, account_before, "the account row is unchanged");
    let gaps = bed.store.collection_gaps(50).expect("gaps");
    assert_eq!(gaps.len(), gaps_before, "no gap row is written: {gaps:?}");
    assert!(gaps.iter().all(|gap| gap.kind != "auth_terminal" && gap.kind != "hub_stopped"));
    // The lease is retained with the heartbeat of its last renewal, and a later
    // admitted start beyond its expiry and the minimum records the downtime from that
    // heartbeat, as one `[H, S]` gap (R8).
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the lease is retained to expire");
    assert_eq!(retained.instance_id, lease_before.instance_id);
    assert!(retained.heartbeat_at_ms >= lease_before.heartbeat_at_ms);
    assert_eq!(
        retained.lease_until_ms - retained.heartbeat_at_ms,
        lease_before.lease_until_ms - lease_before.heartbeat_at_ms,
        "the expiry follows the stored heartbeat"
    );
    assert_an_unrecorded_stop_recovers_with_one_downtime_gap(&bed.store, "legacy");
    bed.server.abort();
}
