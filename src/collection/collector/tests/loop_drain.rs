// SPDX-License-Identifier: AGPL-3.0-only

// Tests of how the real Owner loop drains its stream queues (C3 R2, R11, R28, R36,
// B2 clarification 3; C4 R56 g): through a held list, a held credential unit, a held
// enrichment pass and a held export replay, through the ordinary sleep, and through
// the stop and restart of a stream's run. The loop runs against this file's own Owner
// API double on loopback and against stream runs the tests inject: the loop makes its
// normal admission, fuse and runtime checks, and the run's bounded event queue (of the
// production capacity) then goes to a producer task of the test instead of a WebSocket
// supervisor, so nothing here parses a socket or touches the network. The data is
// synthetic.

/// What the Owner API double answers, and what it saw.
#[derive(Clone)]
struct DrainWire {
    cars: usize,
    /// Every request for the account list, held or not.
    list_requests: Arc<AtomicUsize>,
    /// While set, a list request is held unanswered once it has been counted.
    list_hold: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
    tokens: Arc<AtomicUsize>,
    /// Scripted answers for the next requests for the account list, consumed in order
    /// at the request's arrival (before any hold): a test arms one for the request it
    /// expects, such as a confirmation read.
    script: Arc<Mutex<std::collections::VecDeque<DrainScript>>>,
    /// The scripted answers given so far.
    scripted_arrivals: Arc<AtomicUsize>,
}

/// One scripted answer of the double: its status, an optional `Retry-After`, and an
/// optional gate that holds the answer back while it is set.
#[derive(Clone)]
struct DrainScript {
    status: u16,
    retry_after: Option<u64>,
    hold: Option<Arc<AtomicBool>>,
}

fn drain_vin(id: usize) -> String {
    format!("5YJ3E1EA7KF00000{id}")
}

async fn drain_products(State(wire): State<DrainWire>) -> axum::response::Response {
    wire.list_requests.fetch_add(1, Ordering::SeqCst);
    let scripted = wire.script.lock().expect("script").pop_front();
    if let Some(script) = scripted {
        wire.scripted_arrivals.fetch_add(1, Ordering::SeqCst);
        if let Some(hold) = &script.hold {
            while hold.load(Ordering::SeqCst) {
                sleep(Duration::from_millis(10)).await;
            }
        }
        let mut response = (
            StatusCode::from_u16(script.status).expect("a status"),
            json!({"error": "scripted"}).to_string(),
        )
            .into_response();
        if let Some(seconds) = script.retry_after {
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                seconds.to_string().parse().expect("a header value"),
            );
        }
        return response;
    }
    while wire.list_hold.load(Ordering::SeqCst) {
        sleep(Duration::from_millis(10)).await;
    }
    let cars: Vec<Value> = (1..=wire.cars)
        .map(|id| {
            json!({
                "id": id,
                "vehicle_id": id,
                "vin": drain_vin(id),
                "state": "online",
                "display_name": format!("Drain {id}"),
                "in_service": false,
            })
        })
        .collect();
    (
        StatusCode::OK,
        json!({"response": cars, "count": wire.cars}).to_string(),
    )
        .into_response()
}

async fn drain_vehicle_data(State(wire): State<DrainWire>) -> axum::response::Response {
    wire.reads.fetch_add(1, Ordering::SeqCst);
    let now = current_epoch_millis().expect("clock");
    (
        StatusCode::OK,
        json!({
            "response": {
                "drive_state": {"timestamp": now},
                "charge_state": {"battery_level": 80, "timestamp": now}
            }
        })
        .to_string(),
    )
        .into_response()
}

async fn drain_token(State(wire): State<DrainWire>) -> axum::response::Response {
    wire.tokens.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        json!({
            "access_token": "drain-rotated-access",
            "refresh_token": "drain-rotated-refresh",
            "token_type": "Bearer",
            "expires_in": 28_800,
        })
        .to_string(),
    )
        .into_response()
}

/// The request timeout of the Owner API client of a [`DrainBed`]. A held list ends at
/// the latest when it runs out, which is the accepted remaining bound of the tests that
/// rely on it (the resident-socket tests use a hold that ends that way); the tests that
/// wait for the loop's progress while a list is held use [`DrainBed::start_holding`].
const DRAIN_CLIENT_TIMEOUT: Duration = Duration::from_secs(8);

/// What an injected producer did, as the test reads it.
#[derive(Default)]
struct ProducerStats {
    /// Events the loop took from the queue.
    sent: AtomicUsize,
    /// The producer could not put an event on the queue within C4's wait (one second
    /// here), so its run would have ended.
    starved: AtomicBool,
}

/// A telemetry frame of a car parked in gear P, `step` frames after `base_ms`.
fn drain_frame(base_ms: i64, step: i64) -> crate::tesla_stream::StreamUpdate {
    frame(base_ms + step * 50, Some("P"), Some(0), Some(0))
}

/// A factory whose runs send a telemetry frame every `every` and end when told to
/// stop. A run that cannot put a frame on the queue within a second ends with the
/// error C4 gives a full queue.
fn telemetry_runs(stats: &Arc<ProducerStats>, every: Duration) -> Arc<StreamRunFactory> {
    let stats = Arc::clone(stats);
    Arc::new(move |injection: StreamInjection| {
        let stats = Arc::clone(&stats);
        let seam = Arc::clone(&injection.seam);
        tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(seam, async move {
            let StreamInjection {
                events, mut stop, ..
            } = injection;
            let base_ms = current_epoch_millis().expect("clock") - 60_000;
            let mut step = 0;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stop => return Ok(()),
                    () = sleep(every) => {}
                }
                step += 1;
                let event = StreamEvent::Telemetry {
                    update: Box::new(drain_frame(base_ms, step)),
                    queued_at: Instant::now(),
                };
                match timeout(Duration::from_secs(1), events.send(event)).await {
                    Ok(Ok(())) => {
                        stats.sent.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(Err(_)) => return Ok(()),
                    Err(_) => {
                        stats.starved.store(true, Ordering::SeqCst);
                        return Err(crate::tesla_stream::StreamSupervisorError::EventQueueFull);
                    }
                }
            }
        }))
    })
}

/// [`telemetry_runs`], whose producer waits for room in the queue instead of ending the
/// run after a second (stop-aware: the stop ends it), as the producer of the sleep test
/// does (B2 apply, T2). The tests that hold loop work for a fixed number of acknowledged
/// frames use it, so a loaded machine that starves the loop for a while stalls the
/// producer rather than ending its run, and the frames-committed acknowledgement is what
/// the test waits for.
fn telemetry_runs_waiting(stats: &Arc<ProducerStats>, every: Duration) -> Arc<StreamRunFactory> {
    let stats = Arc::clone(stats);
    Arc::new(move |injection: StreamInjection| {
        let stats = Arc::clone(&stats);
        let seam = Arc::clone(&injection.seam);
        tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(seam, async move {
            let StreamInjection {
                events, mut stop, ..
            } = injection;
            let base_ms = current_epoch_millis().expect("clock") - 60_000;
            let mut step = 0;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stop => return Ok(()),
                    () = sleep(every) => {}
                }
                step += 1;
                let event = StreamEvent::Telemetry {
                    update: Box::new(drain_frame(base_ms, step)),
                    queued_at: Instant::now(),
                };
                tokio::select! {
                    biased;
                    _ = &mut stop => return Ok(()),
                    sent = events.send(event) => match sent {
                        Ok(()) => {
                            stats.sent.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(_) => return Ok(()),
                    },
                }
            }
        }))
    })
}

/// A running Owner loop over a store with `cars` configured cars, the double, the
/// credential unit the test can reach, and the test seam.
struct DrainBed {
    temporary: tempfile::TempDir,
    store: HubStore,
    wire: DrainWire,
    seam: Arc<SupervisedCollectorTestSeam>,
    manager: Arc<tokio::sync::Mutex<LegacyAuthManager>>,
    fuse: Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
    unit_shutdown: Arc<AtomicBool>,
    task: Option<JoinHandle<Result<(), CollectorError>>>,
    shutdown: Option<oneshot::Sender<()>>,
    server: JoinHandle<()>,
    /// The retained admission the loop holds, when it was started with one.
    admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
}

impl DrainBed {
    /// Start the loop. `before` runs first, against the double and the seam (holds and
    /// stream factories), so the loop meets them from its first iteration.
    async fn start(
        cars: usize,
        before: impl FnOnce(&DrainWire, &SupervisedCollectorTestSeam),
    ) -> Self {
        Self::start_with_admission(cars, false, before).await
    }

    /// [`Self::start`], where `seed` runs first, against the store (after the cars are
    /// configured) and the loop's test seam, before the loop starts: a step of the test
    /// that precedes everything the loop does, such as a replay of the export outbox.
    async fn start_seeded(
        cars: usize,
        seed: impl AsyncFnOnce(&HubStore, &Arc<SupervisedCollectorTestSeam>),
        before: impl FnOnce(&DrainWire, &SupervisedCollectorTestSeam),
    ) -> Self {
        Self::start_inner(cars, false, DRAIN_CLIENT_TIMEOUT, seed, before).await
    }

    /// [`Self::start`], for a test that holds a list and releases it only when the loop
    /// has made the progress the test waits for: the client's request timeout is longer
    /// than the stall watchdog of the waits, so a held list is held until the test
    /// releases it (or the watchdog names the stall), never until the client gives up on
    /// it (B2 apply, T2). A test that relies on the request timeout to end a hold uses
    /// [`Self::start`].
    async fn start_holding(
        cars: usize,
        before: impl FnOnce(&DrainWire, &SupervisedCollectorTestSeam),
    ) -> Self {
        Self::start_inner(
            cars,
            false,
            TRAVERSAL_STALL_WATCHDOG + Duration::from_secs(30),
            async |_, _| {},
            before,
        )
        .await
    }

    /// [`Self::start`], with the loop holding a retained admission of the data
    /// directory when `admitted` is set (the lock the tests then replace to deny it).
    async fn start_with_admission(
        cars: usize,
        admitted: bool,
        before: impl FnOnce(&DrainWire, &SupervisedCollectorTestSeam),
    ) -> Self {
        Self::start_inner(cars, admitted, DRAIN_CLIENT_TIMEOUT, async |_, _| {}, before).await
    }

    async fn start_inner(
        cars: usize,
        admitted: bool,
        client_timeout: Duration,
        seed: impl AsyncFnOnce(&HubStore, &Arc<SupervisedCollectorTestSeam>),
        before: impl FnOnce(&DrainWire, &SupervisedCollectorTestSeam),
    ) -> Self {
        crate::crypto::install_default_provider();
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let cursor_key = CursorKey::from_bytes([0x5D; 32]);
        finish_collection_for_provider(
            &store,
            &cursor_key,
            &ManualCollection {
                vehicles: (1..=cars)
                    .map(|id| Vehicle::for_test(id as u64, &drain_vin(id), "online"))
                    .collect(),
                snapshots: Vec::new(),
                failures: Vec::new(),
            },
            CollectorProvider::Legacy,
        )
        .await
        .expect("configure the cars");
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        seed(&store, &seam).await;
        let wire = DrainWire {
            cars,
            list_requests: Arc::new(AtomicUsize::new(0)),
            list_hold: Arc::new(AtomicBool::new(false)),
            reads: Arc::new(AtomicUsize::new(0)),
            tokens: Arc::new(AtomicUsize::new(0)),
            script: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            scripted_arrivals: Arc::new(AtomicUsize::new(0)),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let router = Router::new()
            .route("/api/1/products", get(drain_products))
            .route("/api/1/vehicles/{id}/vehicle_data", get(drain_vehicle_data))
            .route("/oauth2/v3/token", post(drain_token))
            .with_state(wire.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("Owner API double");
        });
        let base = url::Url::parse(&format!("http://{address}/")).expect("base");
        let auth = auth_for(&base, false);
        let CollectionAuth::Legacy { manager, fuse, .. } = &auth;
        let (manager, fuse) = (Arc::clone(manager), Arc::clone(fuse));
        let unit_shutdown = manager.lock().await.shutdown_flag();
        let client = OwnerApi::for_fake_http(base, client_timeout).expect("client");
        let config = supervised_restart_test_config(temporary.path());
        before(&wire, &seam);
        let runtime_admission = admitted.then(|| {
            crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit")
        });
        let admission = runtime_admission.clone();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
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
                cursor_key,
                Some(ready_tx),
                RunWriteGuard::for_run(runtime_admission),
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
            manager,
            fuse,
            unit_shutdown,
            task: Some(task),
            shutdown: Some(shutdown_tx),
            server,
            admission,
        }
    }

    /// Wait for `condition`, as `k6_wait_for` does, and on a stall or an early end of the
    /// collector name what the loop did: the tail of the seam's log of the loop's own
    /// steps (B2 fix round, P3b fix 2, R-P3bF2-5). The wait is a liveness watchdog and not
    /// an acceptance bound: it is the 120 s stall watchdog of the loop traversals, so a
    /// machine whose load starves the loop for tens of seconds does not fail a test whose
    /// claim (a count of acknowledged frames, a logged order of steps) holds all the same.
    async fn wait_for(&mut self, label: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + TRAVERSAL_STALL_WATCHDOG;
        while !condition() {
            let task = self.task.as_mut().expect("task");
            let ended = task.is_finished();
            if ended || Instant::now() >= deadline {
                let log = self.seam.loop_log();
                let tail = &log[log.len().saturating_sub(60)..];
                let outcome = if ended {
                    format!("ended first: {:?}", (&mut *task).await)
                } else {
                    "stalled".to_owned()
                };
                panic!(
                    "{label}: {outcome}; {} loop steps logged, the last {}: {tail:?}",
                    log.len(),
                    tail.len()
                );
            }
            sleep(Duration::from_millis(20)).await;
        }
    }

    /// Ask the loop to stop and return its result. The wait for its end is the same
    /// liveness watchdog, and a stall names the loop's last steps.
    async fn stop(&mut self) -> Result<(), CollectorError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let mut task = self.task.take().expect("task");
        let result = match timeout(TRAVERSAL_STALL_WATCHDOG, &mut task).await {
            Ok(result) => result.expect("supervised collector task join"),
            Err(_) => {
                let log = self.seam.loop_log();
                let tail = &log[log.len().saturating_sub(60)..];
                task.abort();
                let _ = task.await;
                panic!(
                    "loop join: the collector had not ended; {} loop steps logged, the last {}: {tail:?}",
                    log.len(),
                    tail.len()
                );
            }
        };
        self.server.abort();
        result
    }

    /// The newest stored stream frame's observed time, or 0.
    fn latest_stream_frame_ms(&self) -> i64 {
        self.store
            .open()
            .expect("connection")
            .query_row(
                "SELECT COALESCE(MAX(observed_at_ms), 0) FROM current_observations
                  WHERE record_type = 'tesla_stream_update_v1'",
                [],
                |row| row.get(0),
            )
            .expect("stream frame time")
    }

    /// The persisted account wait, if any: `rate_limited_until_ms`.
    fn persisted_rate_limit_until(&self) -> Option<i64> {
        self.store
            .open()
            .expect("connection")
            .query_row(
                "SELECT rate_limited_until_ms FROM collector_account_state",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
    }

    /// The persisted `state` request count of the Owner wire.
    fn persisted_state_requests(&self) -> i64 {
        self.store
            .load_collector_state_rows()
            .expect("rows")
            .counters
            .iter()
            .filter(|row| row.category == "state")
            .map(|row| row.sent)
            .sum()
    }

    /// The confirmation reads the loop has counted into its request ledger so far, from
    /// the loop's own log (B2 fix round, P3b fix 6, R-P3bF6-2): a count taken at one
    /// point of the loop's order, never a difference of two counters on two tasks.
    fn logged_confirmation_reads(&self) -> u32 {
        self.seam
            .loop_log()
            .iter()
            .map(|event| match event {
                LoopEvent::ConfirmationReadsCounted { reads } => *reads,
                _ => 0,
            })
            .sum()
    }
}

/// Hold a running loop's next scheduled list and return once it is held.
async fn hold_the_next_list(bed: &mut DrainBed) {
    bed.wire.list_hold.store(true, Ordering::SeqCst);
    let requests = Arc::clone(&bed.wire.list_requests);
    let before = requests.load(Ordering::SeqCst);
    bed.wait_for("a held list", || requests.load(Ordering::SeqCst) > before)
        .await;
}

/// The frames the producer must have had acknowledged as committed (by the seam) while a
/// held piece of loop work lasts, whatever the machine's load (R-P2F4-5 b): a fixed
/// number of committed frames, never a count of sends over a span of wall-clock time.
const FRAMES_WHILE_HELD: usize = 40;

/// The held work's claim, once its hold (a real sleep of at least the prescribed four
/// seconds) has run: `FRAMES_WHILE_HELD` frames committed since the log position
/// `held_from`, waited for as long as it takes (a bounded watchdog, not an acceptance
/// bound), the producer never starved, and the newest stored frame at least that many
/// frames beyond `frames_before`.
async fn assert_frames_committed_while_held(
    bed: &mut DrainBed,
    stats: &ProducerStats,
    held_from: usize,
    frames_before: i64,
    what: &str,
) {
    let seam = Arc::clone(&bed.seam);
    bed.wait_for(what, || {
        seam.loop_log()[held_from..]
            .iter()
            .filter(|event| matches!(event, LoopEvent::FrameCommitted { .. }))
            .count()
            >= FRAMES_WHILE_HELD
    })
    .await;
    assert!(!stats.starved.load(Ordering::SeqCst), "the queue filled up");
    assert!(
        bed.latest_stream_frame_ms() >= frames_before + 50 * (FRAMES_WHILE_HELD as i64 - 1),
        "{what}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r56g_the_owner_loop_drains_while_a_list_is_held() {
    // A scheduled list is held for four seconds. The loop waits for it, and the queue
    // of the car's stream, which C4 lets a producer wait one second to fill, is
    // drained and its telemetry committed all the while.
    let stats = Arc::new(ProducerStats::default());
    let mut bed = DrainBed::start_holding(1, |_, seam| {
        seam.inject_stream_runs(telemetry_runs(&stats, Duration::from_millis(50)));
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    hold_the_next_list(&mut bed).await;
    let frames_before = bed.latest_stream_frame_ms();
    let held_from = bed.seam.loop_log().len();
    sleep(Duration::from_secs(4)).await;
    // Before the list is released: the producer was never starved and the frames of
    // the hold were committed (a fixed number of them, acknowledged by the seam).
    assert_frames_committed_while_held(
        &mut bed,
        &stats,
        held_from,
        frames_before,
        "the telemetry was committed while the list was held",
    )
    .await;
    bed.wire.list_hold.store(false, Ordering::SeqCst);
    bed.stop().await.expect("an orderly stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r56c_the_owner_loop_folds_a_rejection_drained_during_a_list() {
    // The stream is rejected while a successful list is held. The rejection is drained
    // during the wait, kept, and folded into the loop's next drain: the credential
    // unit gets exactly one confirmation read for it, which is counted.
    let reject = Arc::new(AtomicBool::new(false));
    let enqueued = Arc::new(AtomicBool::new(false));
    let mut bed = DrainBed::start_holding(1, |_, seam| {
        let reject = Arc::clone(&reject);
        let enqueued = Arc::clone(&enqueued);
        seam.inject_stream_runs(Arc::new(move |injection: StreamInjection| {
            let reject = Arc::clone(&reject);
            let enqueued = Arc::clone(&enqueued);
            tokio::spawn(async move {
                let StreamInjection {
                    events, mut stop, ..
                } = injection;
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut stop => return Ok(()),
                        () = sleep(Duration::from_millis(20)) => {}
                    }
                    if reject.swap(false, Ordering::SeqCst)
                        && events.send(StreamEvent::AuthRejected).await.is_ok()
                    {
                        enqueued.store(true, Ordering::SeqCst);
                    }
                }
            })
        }));
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    assert_eq!(bed.logged_confirmation_reads(), 0, "no confirmation yet");
    hold_the_next_list(&mut bed).await;
    // The held list belongs to a tick that is not over: no completed tick may come
    // between this point and the loop's taking of the rejection.
    let ticks_at_hold = seam.tick_checkpoints().len();
    reject.store(true, Ordering::SeqCst);
    // The queue accepted the rejection, and the loop took it while the list is still
    // held: the witness is the loop's own application of the event, not the producer's
    // flag.
    let accepted = Arc::clone(&enqueued);
    bed.wait_for("the rejection is queued", || accepted.load(Ordering::SeqCst)).await;
    let observer = Arc::clone(&bed.seam);
    bed.wait_for("the rejection is drained while the list is held", || {
        observer.loop_log().contains(&LoopEvent::RejectionProcessed)
    })
    .await;
    // The held list answers only when the test releases it, so its tick (the list, the
    // commit, the tick's checkpoint) is over only after the release: the rejection was
    // taken inside the wait, in the loop's own order, and not after the list's end.
    assert_eq!(
        seam.tick_checkpoints().len(),
        ticks_at_hold,
        "the rejection was taken before the held list's tick was over"
    );
    assert_eq!(
        bed.logged_confirmation_reads(),
        0,
        "no confirmation before the list is released"
    );
    bed.wire.list_hold.store(false, Ordering::SeqCst);
    // The folded rejection reaches the unit at the next drain, and the loop counts the
    // read at its next look at the unit: the loop logs the count after the ledger update.
    let observer = Arc::clone(&bed.seam);
    bed.wait_for("the confirmation read is counted", || {
        observer
            .loop_log()
            .iter()
            .any(|event| matches!(event, LoopEvent::ConfirmationReadsCounted { .. }))
    })
    .await;
    assert_eq!(
        bed.logged_confirmation_reads(),
        1,
        "exactly one confirmation read for the one rejection"
    );
    // Later processing is observed through completed ticks, not through a span of time:
    // two more ticks later, and after an orderly stop, still the one.
    let ticks = seam.tick_checkpoints().len();
    let observer = Arc::clone(&bed.seam);
    bed.wait_for("two further completed ticks", || {
        observer.tick_checkpoints().len() >= ticks + 2
    })
    .await;
    bed.stop().await.expect("an orderly stop");
    assert_eq!(
        bed.logged_confirmation_reads(),
        1,
        "still exactly one confirmation read for the one rejection"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r56e_the_owner_loop_settles_a_terminal_run_before_tail_work() {
    // A stream run ends with `CredentialAuthorityUnavailable` while a list is held. The
    // independent sensitive-access flag of the runtime is clear, so the end of the run
    // is the only evidence: the collector settles it as soon as the tick that waited
    // is over, before enrichment, export replay or another iteration.
    let end = Arc::new(AtomicBool::new(false));
    let mut bed = DrainBed::start_holding(1, |_, seam| {
        let end = Arc::clone(&end);
        seam.inject_stream_runs(Arc::new(move |injection: StreamInjection| {
            let end = Arc::clone(&end);
            tokio::spawn(async move {
                let StreamInjection { mut stop, .. } = injection;
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut stop => return Ok(()),
                        () = sleep(Duration::from_millis(20)) => {}
                    }
                    if end.load(Ordering::SeqCst) {
                        return Err(
                            crate::tesla_stream::StreamSupervisorError::CredentialAuthorityUnavailable,
                        );
                    }
                }
            })
        }));
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    // The first iteration's tail has run: nothing else will count as tail work before
    // the settlement under test.
    bed.wait_for("the first tail", || seam.enrichment_calls() >= 1).await;
    hold_the_next_list(&mut bed).await;
    end.store(true, Ordering::SeqCst);
    // The drain that runs while the list is held consumes the run's final result and
    // classifies it as terminal: the loop logs that, and only then is the list released
    // (an acknowledgement of the loop's own step, not a span of time, R-P3bF6-3).
    let observer = Arc::clone(&bed.seam);
    bed.wait_for("the terminal task result is consumed and classified", || {
        observer.loop_log().contains(&LoopEvent::TerminalRunSettled)
    })
    .await;
    let tail_before = (seam.enrichment_calls(), seam.replay_calls(), seam.loop_iterations());
    bed.wire.list_hold.store(false, Ordering::SeqCst);
    let mut task = bed.task.take().expect("task");
    let result = timeout(TRAVERSAL_STALL_WATCHDOG, &mut task)
        .await
        .expect("the collector ends on the terminal run")
        .expect("join");
    bed.server.abort();
    assert!(
        matches!(
            result,
            Err(CollectorError::StreamTask(StreamTaskOutcome::Supervisor(
                crate::tesla_stream::StreamSupervisorError::CredentialAuthorityUnavailable
            )))
        ),
        "{result:?}"
    );
    assert_eq!(
        (seam.enrichment_calls(), seam.replay_calls(), seam.loop_iterations()),
        tail_before,
        "no enrichment, no replay and no iteration came after the tick's end"
    );
}

#[tokio::test(start_paused = true)]
async fn r28_a_non_terminal_run_is_removed_and_its_restart_is_spaced() {
    // A run that ends with an error that is not terminal is removed once, the next
    // run waits out the spacing, and after the deadline exactly one replacement is
    // admitted within two completed iterations, however the loop's own steps
    // interleave. Production drain, settlement and reconciliation are called as the
    // loop calls them; the scheduler's clock is manual, the spacing is its.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(Arc::clone(&seam), async {
            // Run 1 ends with an error at once; later runs live until told to stop.
            seam.inject_stream_runs(Arc::new(|injection: StreamInjection| {
                assert_eq!(injection.vehicle_id, core_id(1), "the run is the car's");
                let first = injection.run == 1;
                tokio::spawn(async move {
                    let StreamInjection { mut stop, .. } = injection;
                    if first {
                        return Err(crate::tesla_stream::StreamSupervisorError::EventQueueFull);
                    }
                    let _ = (&mut stop).await;
                    Ok(())
                })
            }));
            let mut core = Core::owner();
            first_read_parked(&mut core);
            let directory = crate::private_tempdir().expect("temporary store");
            let store = HubStore::initialize(directory.path()).expect("store");
            let (_fake, base, client, server) = auth_fake().await;
            let auth = auth_for(&base, false);
            let mut streams: Vec<VehicleStreamRuntime> = Vec::new();
            let mut cache = HashMap::new();
            let mut pending = StreamDrainResult::default();
            let mut budget = DrainBudget::new(Instant::now());
            // One iteration of the loop's stream steps, in the order the Owner loop takes
            // them (owner_supervision.rs: drain_streams_now, fold the pending result,
            // remove each ended run, apply the gate revocations, reconcile_streams):
            // if that order changes, change it here (the loop's own spacing is real
            // time, which this paused-time proof cannot use).
            async fn iteration(
                store: &HubStore,
                core: &mut Core,
                streams: &mut Vec<VehicleStreamRuntime>,
                cache: &mut HashMap<VehicleId, StreamContext>,
                pending: &mut StreamDrainResult,
                budget: &mut DrainBudget,
                auth: &CollectionAuth,
                client: &OwnerApi,
            ) {
                let mut drained =
                    drain_streams_now(budget, store, &mut core.scheduler, streams, cache)
                        .await
                        .expect("drain");
                drained.absorb(std::mem::take(pending));
                for id in &drained.ended {
                    stop_vehicle_stream(store, &mut core.scheduler, streams, *id, cache)
                        .await
                        .expect("removal");
                }
                apply_gate_revocations(&mut core.scheduler, streams);
                reconcile_streams(
                    store,
                    &mut core.scheduler,
                    streams,
                    auth,
                    client,
                    Duration::from_secs(10),
                    "ws://127.0.0.1:9/streaming/",
                    cache,
                    pending,
                    budget,
                )
                .await
                .expect("reconcile");
            }
            // Iteration 1 admits the first run, which ends at once.
            iteration(&store, &mut core, &mut streams, &mut cache, &mut pending, &mut budget, &auth, &client).await;
            assert_eq!(seam.stream_admissions(), 1);
            tokio::task::yield_now().await;
            // Iteration 2 finds it ended: removed once, its restart spaced.
            core.clock.advance(Duration::from_millis(100));
            iteration(&store, &mut core, &mut streams, &mut cache, &mut pending, &mut budget, &auth, &client).await;
            assert!(streams.is_empty(), "the ended run was removed");
            assert_eq!(seam.stream_admissions(), 1, "no early replacement");
            let restart_at = core.scheduler.vehicles[&core_id(1)]
                .stream
                .restart_not_before
                .expect("the restart is spaced");
            assert_eq!(
                restart_at - core.clock.mono_now(),
                K_STREAM_RESTART_BASE,
                "one failure: the base spacing"
            );
            // Just before the deadline, any number of iterations admit nothing.
            core.clock.advance(K_STREAM_RESTART_BASE - Duration::from_millis(1));
            for _ in 0..3 {
                iteration(&store, &mut core, &mut streams, &mut cache, &mut pending, &mut budget, &auth, &client).await;
            }
            assert_eq!(seam.stream_admissions(), 1, "still no replacement");
            assert!(streams.is_empty());
            // At the deadline: exactly one replacement within two completed iterations,
            // and one runtime.
            core.clock.advance(Duration::from_millis(1));
            for _ in 0..2 {
                iteration(&store, &mut core, &mut streams, &mut cache, &mut pending, &mut budget, &auth, &client).await;
            }
            assert_eq!(seam.stream_admissions(), 2, "exactly one replacement");
            assert_eq!(streams.len(), 1, "one runtime");
            // The replacement is a healthy run: more iterations neither remove nor
            // replace it.
            for _ in 0..3 {
                iteration(&store, &mut core, &mut streams, &mut cache, &mut pending, &mut budget, &auth, &client).await;
            }
            assert_eq!((seam.stream_admissions(), streams.len()), (2, 1));
            stop_all_streams_and_store(
                &store,
                &mut core.scheduler,
                &mut streams,
                &mut cache,
                &mut pending,
                &mut budget,
            )
            .await
                .expect("stop");
            server.abort();
        })
        .await;
}

/// Judge one ordinary sleep of the actual loop against the previous drain's deadline
/// (the checks of `r56g_the_owner_loop_sleep_honours_the_previous_drain_deadline`), and
/// say whether it met the premise of a sleep that proves the cap: the deadline is what
/// limits it, because it is nearer than the planned wait.
fn judge_an_iteration_sleep(sleep: &IterationSleep, cap: Duration) -> bool {
    assert!(sleep.armed, "a stream runs: {sleep:?}");
    let since_drain = sleep.began.saturating_duration_since(sleep.drain_mark);
    // The premise of a sleep that proves the cap: the previous drain's deadline is
    // what limits it, because it is nearer than the planned wait (the tail of the
    // iteration took the time since the drain). Only such sleeps are counted.
    let remaining = cap.saturating_sub(since_drain);
    let capped = remaining < sleep.planned;
    // The sleep is what is left of the interval (nothing at all once a backlog made
    // the next drain due at once), and never more than was planned.
    assert!(
        sleep.slept_for <= sleep.planned.min(remaining),
        "the sleep ran past the previous drain's deadline ({since_drain:?} after the \
         drain, then {:?} asleep of {:?} planned)",
        sleep.slept_for,
        sleep.planned
    );
    if !sleep.slept_for.is_zero() {
        // Not before the deadline either: the loop does not drain early and then
        // start an empty iteration early (R11).
        let drained_to_woken = sleep.ended.saturating_duration_since(sleep.drain_mark);
        assert!(
            drained_to_woken + Duration::from_millis(1) >= cap.min(since_drain + sleep.planned),
            "the sleep ended {drained_to_woken:?} after the drain, before its deadline: {sleep:?}"
        );
    }
    // The exact remaining time is judged only for a capped sleep (R-P2F3-6): an
    // uncapped one sleeps its planned wait by the production rule.
    if capped {
        assert!(
            sleep.slept_for.is_zero() || sleep.slept_for == remaining,
            "the sleep ended before the drain deadline: {:?} of {remaining:?} left",
            sleep.slept_for
        );
    }
    capped
}

#[test]
fn r56g_the_exact_remaining_check_judges_capped_sleeps_only() {
    // R-P2F3-6. A sleep that is not capped (the planned wait is shorter than what is left
    // of the drain interval) is ordinary: the production rule sleeps the planned wait,
    // and the judgement must not call that a failure. The exact-remaining check belongs
    // to the capped sleeps, where it still bites.
    let cap = K_STREAM_DRAIN_CAP;
    let drain_mark = Instant::now();
    let sleep_of = |since_drain_ms: u64, planned_ms: u64, slept_ms: u64| {
        let began = drain_mark + Duration::from_millis(since_drain_ms);
        IterationSleep {
            planned: Duration::from_millis(planned_ms),
            armed: true,
            drain_mark,
            began,
            slept_for: Duration::from_millis(slept_ms),
            ended: began + Duration::from_millis(slept_ms),
            queued: 0,
        }
    };
    // Uncapped: 20 ms after the drain, 30 ms planned (80 ms left), and the loop slept 30.
    assert!(!judge_an_iteration_sleep(&sleep_of(20, 30, 30), cap), "an uncapped sleep is not judged capped");
    // Capped and exact: 60 ms after the drain, 100 ms planned, 40 ms slept.
    assert!(judge_an_iteration_sleep(&sleep_of(60, 100, 40), cap));
    // Capped and cut short, or run past the deadline: both still fail.
    for slept_ms in [39, 41] {
        let sleep = sleep_of(60, 100, slept_ms);
        assert!(
            std::panic::catch_unwind(|| judge_an_iteration_sleep(&sleep, cap)).is_err(),
            "a capped sleep of {slept_ms} ms instead of 40 ms is a failure"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r56g_the_owner_loop_sleep_honours_the_previous_drain_deadline() {
    // The tail of every iteration takes at least 60 ms (a delay in front of the
    // enrichment pass) and the ordinary sleep is nominally the drain cap, 100 ms. A
    // producer keeps a `Healthy` event queued all the while. The actual loop's ordinary
    // sleep must end by the previous drain's deadline, so the tail's 60 ms is taken
    // out of it, and the drain that follows the sleep consumes the queued event.
    // Nothing here times a whole iteration on a machine of unknown load: what is
    // proved is the sleep the loop itself decided and ran (recorded by the test seam),
    // that no sleep ended before its deadline, and that the drain after the sleep takes
    // the queued event, in the order of the loop's own logged steps, which no load
    // changes. The uncapped sleep (160 ms from a drain: the tail and then the whole
    // nominal sleep) fails the first of the checks. The exact +99/+100 ms boundary of
    // the production sleep and of the drain after it is proved on paused time in
    // `fix_round_c3.rs` (r11_the_iteration_sleep_ends_at_the_drain_deadline_...), since
    // a loop that does real loopback I/O cannot run on a paused clock.
    //
    // The sleeps judged are those after the stream's admission: the factory, which runs
    // on the loop's own task at the admission, records how many ordinary sleeps the loop
    // had made by then (R-P3bF6-1), so no count of start-up sleeps is assumed. The
    // producer waits for room in the queue instead of giving up, so the run lives until
    // the stop, and the seam records the queue's length at the end of every sleep: the
    // drain after a sleep is required to consume an event only when one was queued.
    let sent = Arc::new(AtomicUsize::new(0));
    let admitted_after: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let mut bed = DrainBed::start(1, |_, seam| {
        seam.enrichment_delay_ms.store(60, Ordering::SeqCst);
        let sent = Arc::clone(&sent);
        let admitted_after = Arc::clone(&admitted_after);
        seam.inject_stream_runs(Arc::new(move |injection: StreamInjection| {
            let sent = Arc::clone(&sent);
            let seam = Arc::clone(&injection.seam);
            {
                let mut admitted = admitted_after.lock().expect("admission marker");
                assert!(admitted.is_none(), "the stream was admitted twice");
                *admitted = Some(seam.iteration_sleeps().len());
            }
            tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(seam, async move {
                let StreamInjection {
                    events, mut stop, ..
                } = injection;
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut stop => return Ok(()),
                        () = sleep(Duration::from_millis(5)) => {}
                    }
                    tokio::select! {
                        biased;
                        _ = &mut stop => return Ok(()),
                        sent_event = events.send(StreamEvent::Healthy) => match sent_event {
                            Ok(()) => {
                                sent.fetch_add(1, Ordering::SeqCst);
                            }
                            Err(_) => return Ok(()),
                        },
                    }
                }
            }))
        }));
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    let marker = Arc::clone(&admitted_after);
    bed.wait_for("the stream's admission", || {
        marker.lock().expect("admission marker").is_some()
    })
    .await;
    let from = marker.lock().expect("admission marker").expect("the admission marker");
    let cap = K_STREAM_DRAIN_CAP;
    // Progress, not time: twelve sleeps after the admission, six of them capped by the
    // previous drain's deadline and ended with an event queued.
    let proving = |sleeps: &[IterationSleep]| {
        sleeps
            .iter()
            .filter(|sleep| {
                let since_drain = sleep.began.saturating_duration_since(sleep.drain_mark);
                cap.saturating_sub(since_drain) < sleep.planned && sleep.queued > 0
            })
            .count()
    };
    bed.wait_for("twelve ordinary sleeps after the admission", || {
        let sleeps = seam.iteration_sleeps();
        sleeps.len() >= from + 12 && proving(&sleeps[from..]) >= 6
    })
    .await;
    bed.stop().await.expect("an orderly stop");
    let log = bed.seam.loop_log();
    let sleeps: Vec<IterationSleep> = bed.seam.iteration_sleeps().split_off(from);
    assert!(sleeps.len() >= 12, "{sleeps:?}");
    assert_eq!(seam.stream_admissions(), 1, "the one run lived until the stop");
    assert!(sent.load(Ordering::SeqCst) > 0, "the producer kept events queued");
    let mut judged = 0;
    let mut consumed = 0;
    for sleep in &sleeps {
        let capped = judge_an_iteration_sleep(sleep, cap);
        if !capped {
            continue;
        }
        // The order of the loop's own steps: the first step after the sleep is a drain,
        // and the queued event is consumed by that drain, not by a later one.
        // The unfiltered log: nothing, including loop work awaited without being pumped
        // (`AwaitedLoopWork`), stands between the end of the capped sleep and the drain.
        let at = log
            .iter()
            .position(|event| *event == LoopEvent::SleepEnded(sleep.ended))
            .expect("the end of the sleep is logged");
        let after = &log[at + 1..];
        let Some(LoopEvent::Drain(_)) = after.first() else {
            panic!(
                "the step after the sleep is not a drain: {:?}",
                &after[..after.len().min(4)]
            );
        };
        judged += 1;
        if sleep.queued == 0 {
            // Nothing was queued when this sleep ended, so there is nothing for its
            // drain to consume.
            continue;
        }
        let next_drain = after[1..]
            .iter()
            .position(|event| matches!(event, LoopEvent::Drain(_)))
            .map_or(after.len(), |position| position + 1);
        assert!(
            after[..next_drain]
                .iter()
                .any(|event| matches!(event, LoopEvent::HealthyProcessed(_))),
            "the drain after the sleep consumed the queued event ({} queued): {:?}",
            sleep.queued,
            &after[..next_drain.min(6)]
        );
        consumed += 1;
    }
    assert!(judged >= 6, "{judged} capped sleeps judged");
    assert!(consumed >= 6, "{consumed} capped sleeps ended with an event queued");
    // The delayed enrichment pass really ran in the iterations judged.
    assert!(bed.seam.enrichment_calls() >= sleeps.len());
}

/// A factory whose runs send a telemetry frame every `every` and, once, a rejection
/// when `reject` is set, and end when told to stop.
fn rejecting_telemetry_runs(
    stats: &Arc<ProducerStats>,
    every: Duration,
    reject: &Arc<AtomicBool>,
) -> Arc<StreamRunFactory> {
    let stats = Arc::clone(stats);
    let reject = Arc::clone(reject);
    Arc::new(move |injection: StreamInjection| {
        let stats = Arc::clone(&stats);
        let reject = Arc::clone(&reject);
        let seam = Arc::clone(&injection.seam);
        tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(seam, async move {
            let StreamInjection {
                events, mut stop, ..
            } = injection;
            let base_ms = current_epoch_millis().expect("clock") - 60_000;
            let mut step = 0;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stop => return Ok(()),
                    () = sleep(every) => {}
                }
                step += 1;
                if reject.swap(false, Ordering::SeqCst)
                    && timeout(Duration::from_secs(1), events.send(StreamEvent::AuthRejected))
                        .await
                        .is_err()
                {
                    return Err(crate::tesla_stream::StreamSupervisorError::EventQueueFull);
                }
                let event = StreamEvent::Telemetry {
                    update: Box::new(drain_frame(base_ms, step)),
                    queued_at: Instant::now(),
                };
                match timeout(Duration::from_secs(1), events.send(event)).await {
                    Ok(Ok(())) => {
                        stats.sent.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(Err(_)) => return Ok(()),
                    Err(_) => {
                        stats.starved.store(true, Ordering::SeqCst);
                        return Err(crate::tesla_stream::StreamSupervisorError::EventQueueFull);
                    }
                }
            }
        }))
    })
}

/// Make the real loop send its production confirmation read, answered by the double
/// with HTTP 429 and `Retry-After: 120`, held back while `hold` is set. The next
/// scheduled list is held first so the loop is known to be waiting for it, pumping its
/// queues; a rejection the injected run sends then is drained and kept, the list is
/// released, and the loop's next drain folds the rejection into the confirmation. The
/// returned instant is when the rejection was injected.
async fn provoke_a_confirmation_429(
    bed: &mut DrainBed,
    reject: &Arc<AtomicBool>,
    hold: Option<&Arc<AtomicBool>>,
) -> i64 {
    hold_the_next_list(bed).await;
    bed.wire.script.lock().expect("script").push_back(DrainScript {
        status: 429,
        retry_after: Some(120),
        hold: hold.cloned(),
    });
    let injected_at = current_epoch_millis().expect("clock");
    reject.store(true, Ordering::SeqCst);
    let reject = Arc::clone(reject);
    bed.wait_for("the rejection is sent and drained", || !reject.load(Ordering::SeqCst))
        .await;
    bed.wire.list_hold.store(false, Ordering::SeqCst);
    injected_at
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r56g_the_owner_loop_drains_while_confirmation_reports_wait_on_the_fuse() {
    // A stream rejection makes the credential unit's production confirmation read, and
    // the double holds that read for four seconds, so the unit keeps the fuse and the
    // manager all the while (the production wrapper holds them through the whole read).
    // It is answered 429 with Retry-After 120. The loop waits for the confirmation where
    // the stream queues are drained, so the telemetry is committed while it is held, and
    // afterwards counts the read once as a `state` request and applies the account wait
    // once.
    let stats = Arc::new(ProducerStats::default());
    let reject = Arc::new(AtomicBool::new(false));
    let mut bed = DrainBed::start_holding(1, |_, seam| {
        seam.inject_stream_runs(rejecting_telemetry_runs(
            &stats,
            Duration::from_millis(50),
            &reject,
        ));
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    let gate = Arc::new(AtomicBool::new(true));
    let injected_at = provoke_a_confirmation_429(&mut bed, &reject, Some(&gate)).await;
    let arrivals = Arc::clone(&bed.wire.scripted_arrivals);
    bed.wait_for("the confirmation is held at the double", || {
        arrivals.load(Ordering::SeqCst) >= 1
    })
    .await;
    // The fuse and the manager are held by the production confirmation.
    assert!(bed.fuse.try_lock().is_err() || bed.manager.try_lock().is_err());
    let frames_before = bed.latest_stream_frame_ms();
    let held_from = bed.seam.loop_log().len();
    sleep(Duration::from_secs(4)).await;
    assert_frames_committed_while_held(
        &mut bed,
        &stats,
        held_from,
        frames_before,
        "the queues were drained and committed while the confirmation was held",
    )
    .await;
    assert!(bed.persisted_rate_limit_until().is_none(), "nothing applied yet");
    let released_at = current_epoch_millis().expect("clock");
    gate.store(false, Ordering::SeqCst);
    let store = bed.store.clone();
    bed.wait_for("the account wait", || {
        store
            .open()
            .expect("connection")
            .query_row(
                "SELECT rate_limited_until_ms FROM collector_account_state",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
            .is_some()
    })
    .await;
    let observed_at = current_epoch_millis().expect("clock");
    let until = bed.persisted_rate_limit_until().expect("the wait");
    let applied_at = until - 120_000;
    assert!(
        applied_at >= released_at && applied_at <= observed_at,
        "applied once the confirmation was answered: between {released_at} and {observed_at}, \
         not {applied_at} (injected at {injected_at})"
    );
    bed.stop().await.expect("an orderly stop");
    assert_eq!(bed.wire.scripted_arrivals.load(Ordering::SeqCst), 1, "one confirmation");
    assert_eq!(
        bed.persisted_state_requests(),
        i64::try_from(bed.seam.lists_sent()).expect("count") + 1,
        "the confirmation read is one counted `state` request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r56g_the_owner_loop_drains_while_it_waits_for_the_fuse_at_the_top_of_an_iteration() {
    // The loop takes the confirmation outcomes at the top of every iteration, which
    // needs the fuse, and a confirmation holds the fuse for tens of seconds (here the
    // test holds it, as a confirmation or a resident vehicle action does). The loop is
    // held at the top of its second iteration, where its stream run is already up, the
    // fuse is taken, and the loop is let through. It waits for the fuse right there, with
    // no list sent, and the queue of the car's stream is drained and its telemetry
    // committed all the while. The producer waits for room in the queue
    // ([`telemetry_runs_waiting`]) rather than ending its run after a second: while the
    // loop is held at the top of the iteration the queue fills, and a loaded machine that
    // delays a correct drain must stall the producer, not end it, so the only thing the
    // test waits for is the acknowledged commits (bounded by the watchdog).
    let stats = Arc::new(ProducerStats::default());
    let mut bed = DrainBed::start(1, |_, seam| {
        seam.inject_stream_runs(telemetry_runs_waiting(&stats, Duration::from_millis(50)));
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    let top = bed.seam.arm_gate("iteration-top");
    top.arrived("the top of an iteration", 1).await;
    let fuse = Arc::clone(&bed.fuse);
    let held = fuse.lock().await;
    let lists_before = bed.wire.list_requests.load(Ordering::SeqCst);
    let frames_before = bed.latest_stream_frame_ms();
    let held_from = bed.seam.loop_log().len();
    top.open();
    sleep(Duration::from_secs(4)).await;
    assert_frames_committed_while_held(
        &mut bed,
        &stats,
        held_from,
        frames_before,
        "the telemetry was committed while the loop waited for the fuse",
    )
    .await;
    assert_eq!(
        bed.wire.list_requests.load(Ordering::SeqCst),
        lists_before,
        "the loop is still waiting at the top of its iteration"
    );
    drop(held);
    bed.stop().await.expect("an orderly stop");
}

/// Hold a running loop's next scheduled list, on a scheduler clock the test moves: the
/// clock is advanced a second at a time until the list is due and has arrived, and then
/// stands still. Returns once the list is held.
async fn hold_the_next_list_on_the_clock(bed: &mut DrainBed, clock: &ManualClock) {
    bed.wire.list_hold.store(true, Ordering::SeqCst);
    let requests = Arc::clone(&bed.wire.list_requests);
    let before = requests.load(Ordering::SeqCst);
    let mut tries = 0;
    bed.wait_for("a held list", || {
        if requests.load(Ordering::SeqCst) > before {
            return true;
        }
        tries += 1;
        assert!(tries < 1_000, "no list became due");
        clock.advance(Duration::from_secs(1));
        false
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r41a_the_owner_loop_applies_a_confirmation_429_once() {
    // The production confirmation read the loop makes for a stream rejection is answered
    // 429 with Retry-After 120. It is counted once and applied once, at the iteration that
    // finds it: the persisted deadline is the instant of that application plus exactly the
    // Retry-After, and the later iterations neither count it again nor extend the wait.
    // The loop's scheduler reads a clock the test holds still from the moment the list is
    // held until the wait is persisted, so the instant of the application is the one the
    // loop logged when it applied the answer (the actual argument of the application) and
    // the deadline is compared with it exactly; nothing is reconstructed from the deadline.
    // The stream drains stay on the runtime's timer.
    let stats = Arc::new(ProducerStats::default());
    let reject = Arc::new(AtomicBool::new(false));
    let clock = controlled_cadence_clock();
    let mut bed = DrainBed::start_holding(1, |_, seam| {
        seam.select_scheduler_clock(&clock);
        seam.inject_stream_runs(rejecting_telemetry_runs(
            &stats,
            Duration::from_millis(50),
            &reject,
        ));
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    hold_the_next_list_on_the_clock(&mut bed, &clock).await;
    bed.wire.script.lock().expect("script").push_back(DrainScript {
        status: 429,
        retry_after: Some(120),
        hold: None,
    });
    reject.store(true, Ordering::SeqCst);
    let rejected = Arc::clone(&reject);
    bed.wait_for("the rejection is sent", || !rejected.load(Ordering::SeqCst))
        .await;
    // The held list is released only once the loop has taken the rejection (its own
    // logged step), so the confirmation read the test judges is the one the rejection
    // asked for, whatever the drain timing of the machine (B2 apply, T3).
    let observer = Arc::clone(&bed.seam);
    bed.wait_for("the rejection is drained while the list is held", || {
        observer.loop_log().contains(&LoopEvent::RejectionProcessed)
    })
    .await;
    bed.wire.list_hold.store(false, Ordering::SeqCst);
    let store = bed.store.clone();
    bed.wait_for("the account wait", || {
        store
            .open()
            .expect("connection")
            .query_row(
                "SELECT rate_limited_until_ms FROM collector_account_state",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
            .is_some()
    })
    .await;
    let first = bed.persisted_rate_limit_until().expect("the wait");
    let applied: Vec<(Instant, i64)> = seam
        .loop_log()
        .into_iter()
        .filter_map(|event| match event {
            LoopEvent::ConfirmationApplied { at, wall_ms } => Some((at, wall_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(applied.len(), 1, "the answer is applied once: {applied:?}");
    let (at, wall_ms) = applied[0];
    assert_eq!(at, clock.mono_now(), "applied at the held instant of the loop's clock");
    assert_eq!(wall_ms, clock.wall_now_ms().expect("wall"), "and its wall time");
    assert_eq!(
        first,
        wall_ms + 120_000,
        "the deadline is the application instant plus exactly the Retry-After"
    );
    // Later iterations: a few more, none of which changes the persisted deadline.
    let iterations = bed.seam.loop_iterations();
    let observer = Arc::clone(&bed.seam);
    bed.wait_for("later iterations", || observer.loop_iterations() >= iterations + 5).await;
    assert_eq!(bed.persisted_rate_limit_until(), Some(first), "not extended");
    bed.stop().await.expect("an orderly stop");
    assert_eq!(bed.wire.scripted_arrivals.load(Ordering::SeqCst), 1, "one confirmation");
    assert_eq!(
        bed.persisted_state_requests(),
        i64::try_from(bed.seam.lists_sent()).expect("count") + 1,
        "counted once"
    );
    assert_eq!(bed.persisted_rate_limit_until(), Some(first), "applied once");
    let applied_events = bed
        .seam
        .loop_log()
        .iter()
        .filter(|event| matches!(event, LoopEvent::ConfirmationApplied { .. }))
        .count();
    assert_eq!(applied_events, 1, "never reapplied by a later iteration");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r56g_the_owner_loop_drains_while_enrichment_is_held() {
    // The gate in front of the enrichment pass's transport is held for four seconds.
    // The loop waits for the pass where the stream queues are drained and committed,
    // and no provider is touched: the gate stands before any transport. The hold is
    // armed by the stream's admission (the factory runs on the loop's own task, with
    // nothing else of the loop in flight), together with the counters' baselines, so a
    // tail that ran before the admission (an iteration without a stream) is neither
    // held nor counted (R-P3bF6-3).
    let stats = Arc::new(ProducerStats::default());
    let baselines: Arc<Mutex<Option<(usize, usize)>>> = Arc::new(Mutex::new(None));
    let mut bed = DrainBed::start(1, |_, seam| {
        let stats = Arc::clone(&stats);
        let baselines = Arc::clone(&baselines);
        let telemetry = telemetry_runs_waiting(&stats, Duration::from_millis(50));
        seam.inject_stream_runs(Arc::new(move |injection: StreamInjection| {
            let seam = Arc::clone(&injection.seam);
            *baselines.lock().expect("baselines") =
                Some((seam.enrichment_calls(), seam.replay_calls()));
            seam.enrichment_hold.store(true, Ordering::SeqCst);
            telemetry(injection)
        }));
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    let marker = Arc::clone(&baselines);
    bed.wait_for("the stream's admission", || {
        marker.lock().expect("baselines").is_some()
    })
    .await;
    let (enrichment_before, replay_before) = marker
        .lock()
        .expect("baselines")
        .expect("the admission baselines");
    bed.wait_for("the held enrichment pass", || {
        seam.enrichment_calls() > enrichment_before
    })
    .await;
    assert_eq!(seam.stream_admissions(), 1, "the stream runs while the pass is held");
    let frames_before = bed.latest_stream_frame_ms();
    let held_from = bed.seam.loop_log().len();
    sleep(Duration::from_secs(4)).await;
    assert_frames_committed_while_held(
        &mut bed,
        &stats,
        held_from,
        frames_before,
        "stream telemetry was committed while enrichment was held",
    )
    .await;
    assert_eq!(
        seam.replay_calls(),
        replay_before,
        "the loop is still inside the pass"
    );
    seam.enrichment_hold.store(false, Ordering::SeqCst);
    bed.stop().await.expect("an orderly stop");
}

/// Set the owner's `free_supercharging` setting of car 1: a stored change that marks
/// its export dirty, a newer revision of the outbox row.
fn commit_a_newer_export_revision(store: &HubStore, enabled: bool) {
    let (vehicle, _, mut settings) = store
        .configured_tesla_vehicles()
        .expect("configured")
        .into_iter()
        .next()
        .expect("a car");
    settings.free_supercharging = enabled;
    store
        .replace_car_settings(vehicle, &settings)
        .expect("a newer stored change");
}

fn export_outbox_row(store: &HubStore) -> Option<(i64, i64)> {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT dirty_revision, claimed_until_ms FROM export_outbox",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r56g_the_owner_loop_drains_while_export_replay_is_held() {
    // The publication gate is held by the test, independently of the replay, so the
    // replay waits for it while the stream queues are drained and committed. The hold
    // lasts until a fixed number of the producer's frames have been acknowledged as
    // committed by the seam while the replay is waiting (progress, never a span of
    // wall-clock time, R-P2F3-9). Once the gate is released, the replay's blocking job
    // is paused after claiming the outbox row, a newer revision is committed, and the
    // job completes: the newer revision survives the completion. The enrichment hold
    // that lets the test take the gate first is armed by the stream's admission, with
    // the counters' baselines (R-P3bF6-3, B2 apply T1: the claims included): a tail
    // before the admission is neither held nor counted. To show it, a waiting outbox
    // row is seeded and really claimed and replayed by the production replay before the
    // loop starts, so the claims the admission finds are not zero; the replay under test
    // is judged against them.
    let stats = Arc::new(ProducerStats::default());
    let baselines: Arc<Mutex<Option<(usize, usize, usize)>>> = Arc::new(Mutex::new(None));
    let mut bed = DrainBed::start_seeded(
        1,
        async |store, seam| {
            commit_a_newer_export_revision(store, true);
            let vehicles = vec![Vehicle::for_test(1, &drain_vin(1), "online")];
            SUPERVISED_COLLECTOR_TEST_SEAM
                .scope(Arc::clone(seam), async {
                    replay_export_outbox(
                        store,
                        &CursorKey::from_bytes([0x5D; 32]),
                        &vehicles,
                        current_epoch_millis().expect("clock"),
                    )
                    .await
                    .expect("the seeded replay");
                })
                .await;
            assert_eq!(seam.replay_claims(), 1, "the seeded row was claimed");
            assert_eq!(seam.replay_completions(), 1, "and its replay completed");
        },
        |_, seam| {
            let stats = Arc::clone(&stats);
            let baselines = Arc::clone(&baselines);
            let telemetry = telemetry_runs_waiting(&stats, Duration::from_millis(50));
            seam.inject_stream_runs(Arc::new(move |injection: StreamInjection| {
                let seam = Arc::clone(&injection.seam);
                *baselines.lock().expect("baselines") = Some((
                    seam.enrichment_calls(),
                    seam.replay_calls(),
                    seam.replay_claims(),
                ));
                seam.enrichment_hold.store(true, Ordering::SeqCst);
                telemetry(injection)
            }));
        },
    )
    .await;
    let seam = Arc::clone(&bed.seam);
    let marker = Arc::clone(&baselines);
    bed.wait_for("the stream's admission", || {
        marker.lock().expect("baselines").is_some()
    })
    .await;
    let (enrichment_before, replay_before, claims_before) = marker
        .lock()
        .expect("baselines")
        .expect("the admission baselines");
    assert!(claims_before >= 1, "the pre-admission replay claimed the seeded row");
    bed.wait_for("the held enrichment pass", || {
        seam.enrichment_calls() > enrichment_before
    })
    .await;
    commit_a_newer_export_revision(&bed.store, false);
    // The test takes the gate while the loop is still in the enrichment pass.
    let gate = bed.store.acquire_publication_gate().await.expect("the gate");
    seam.enrichment_hold.store(false, Ordering::SeqCst);
    bed.wait_for("the replay", || seam.replay_calls() > replay_before).await;
    let frames_before = bed.latest_stream_frame_ms();
    let held_from = seam.loop_log().len();
    // The frames the seam acknowledges as committed from here on, while the replay waits
    // for the gate: a fixed number of them, however long the machine takes.
    let committed_while_held = || {
        seam.loop_log()[held_from..]
            .iter()
            .filter(|event| matches!(event, LoopEvent::FrameCommitted { .. }))
            .count()
    };
    bed.wait_for("frames committed while the replay is held", || {
        committed_while_held() >= FRAMES_WHILE_HELD
    })
    .await;
    assert!(!stats.starved.load(Ordering::SeqCst), "the queue filled up");
    assert!(
        bed.latest_stream_frame_ms() >= frames_before + 50 * (FRAMES_WHILE_HELD as i64 - 1),
        "stream telemetry was committed while the replay waited for the gate"
    );
    assert_eq!(
        seam.replay_claims(),
        claims_before,
        "the replay has not claimed anything since the admission"
    );
    // Release the gate with the job's hold in place: it claims, and pauses.
    seam.replay_hold_after_claim.store(true, Ordering::SeqCst);
    drop(gate);
    bed.wait_for("the claim", || seam.replay_claims() > claims_before).await;
    let (claimed_revision, claimed_until) = export_outbox_row(&bed.store).expect("the outbox row");
    assert!(claimed_until > 0, "the row is claimed by the job");
    // A newer revision is committed while the job holds its claim.
    commit_a_newer_export_revision(&bed.store, true);
    let (newer_revision, _) = export_outbox_row(&bed.store).expect("the outbox row");
    assert!(newer_revision > claimed_revision, "{newer_revision} > {claimed_revision}");
    // The next replay (an iteration away) is held before it takes the gate or starts
    // its job, so nothing claims the row again while the test reads it; the job under
    // test completes its claim and says so (the replay's own acknowledgement).
    let completed_before = seam.replay_completions();
    seam.replay_hold_before_worker.store(true, Ordering::SeqCst);
    seam.replay_hold_after_claim.store(false, Ordering::SeqCst);
    // The newer revision stays for the next replay.
    bed.wait_for("the replay completed", || {
        seam.replay_completions() > completed_before
    })
    .await;
    let (revision, claimed_until) = export_outbox_row(&bed.store)
        .expect("the newer revision survives the completion of the older claim");
    assert!(revision >= newer_revision, "{revision} >= {newer_revision}");
    assert_eq!(claimed_until, 0, "released for the next replay");
    seam.replay_hold_before_worker.store(false, Ordering::SeqCst);
    bed.stop().await.expect("an orderly stop");
}

#[tokio::test(start_paused = true)]
async fn r56g_two_stops_preserve_the_third_stream_and_their_final_frames() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(Arc::clone(&seam), two_stops_preserve_the_third_stream(seam))
        .await;
}

async fn two_stops_preserve_the_third_stream(seam: Arc<SupervisedCollectorTestSeam>) {
    // Three cars' runs. Two are stopped, and their tasks ignore the stop signal. Both
    // signals go out before either is joined, the two five-second deadlines run
    // together (five seconds in all, not ten), the third stream and the stopping
    // runs' own telemetry are drained meanwhile, each overdue task is aborted and
    // joined before its runtime goes, and the third run is left running.
    let directory = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(directory.path()).expect("store");
    let mut core = Core::owner();
    let t0 = core.t0;
    core.scheduler.accept_discovery(
        vec![
            core_vehicle(1, "online"),
            core_vehicle(2, "online"),
            core_vehicle(3, "online"),
        ],
        t0,
    );
    // Cars 1 and 2 carry earlier run failures; car 3's count is clear.
    for id in [1, 2] {
        core.scheduler
            .vehicles
            .get_mut(&core_id(id))
            .expect("the car")
            .stream
            .task_failures = 2;
    }
    let signalled: Arc<Mutex<Vec<(u64, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let aborted: Arc<Mutex<Vec<(u64, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let base_ms = current_epoch_millis().expect("clock") - 60_000;
    let mut streams = Vec::new();
    let mut producers = Vec::new();
    struct EndNote(u64, Arc<Mutex<Vec<(u64, Instant)>>>);
    impl Drop for EndNote {
        fn drop(&mut self) {
            self.1.lock().expect("notes").push((self.0, Instant::now()));
        }
    }
    for id in [1u64, 2, 3] {
        let (events, receiver) = mpsc::channel(K_STREAM_EVENT_CAPACITY);
        let (shutdown, stop) = oneshot::channel::<()>();
        let note_signal = Arc::clone(&signalled);
        let note_end = Arc::clone(&aborted);
        let task = tokio::spawn(async move {
            let _end = EndNote(id, note_end);
            let mut stop = Some(stop);
            if id == 3 {
                // The continuing run just lives.
                std::future::pending::<()>().await;
            } else {
                // A stopping run notes the signal and then ignores it.
                let _ = stop.take().expect("stop").await;
                note_signal.lock().expect("notes").push((id, Instant::now()));
                std::future::pending::<()>().await;
            }
            Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
        });
        streams.push(VehicleStreamRuntime {
            vehicle_id: core_id(id),
            power_gate: Arc::new(StreamPowerGate::default()),
            sensitive_access_failure: Arc::new(AtomicBool::new(false)),
            events: receiver,
            _shutdown: Some(shutdown),
            task: Some(task),
        });
        // Every run keeps delivering a frame every 50 ms for as long as its queue
        // exists. Only a frame the queue accepted is counted (with its source time);
        // the producer ends when the runtime's queue is gone, so it is bound to the
        // runtime's lifetime.
        let delivered: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::clone(&delivered);
        producers.push((
            id,
            delivered,
            tokio::spawn(async move {
                for step in 0..200 {
                    sleep(Duration::from_millis(50)).await;
                    let frame_at = base_ms + id as i64 + step * 50;
                    let event = StreamEvent::Telemetry {
                        update: Box::new(drain_frame(base_ms + id as i64, step)),
                        queued_at: Instant::now(),
                    };
                    match timeout(Duration::from_secs(1), events.send(event)).await {
                        Ok(Ok(())) => accepted.lock().expect("accepted").push(frame_at),
                        Ok(Err(_)) | Err(_) => return,
                    }
                    // A control event among the frames: for a stopping run it
                    // describes a run that is ending and must change nothing.
                    if step % 5 == 0
                        && !matches!(
                            timeout(Duration::from_secs(1), events.send(StreamEvent::Healthy)).await,
                            Ok(Ok(()))
                        )
                    {
                        return;
                    }
                }
            }),
        ));
    }
    tokio::task::yield_now().await;
    let mut cache = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let mut budget = DrainBudget::new(Instant::now());
    let started = Instant::now();
    stop_vehicle_streams(
        &store,
        &mut core.scheduler,
        &mut streams,
        &[core_id(1), core_id(2)],
        &mut cache,
        &mut pending,
        &mut budget,
    )
    .await
    .expect("the stop");
    let elapsed = Instant::now() - started;
    // Concurrent deadlines: five seconds in all.
    assert!(
        elapsed >= STREAM_SHUTDOWN_TIMEOUT && elapsed < STREAM_SHUTDOWN_TIMEOUT + Duration::from_secs(1),
        "{elapsed:?}"
    );
    // Both signals went out together, at the start, before either deadline.
    let signals = signalled.lock().expect("notes").clone();
    assert_eq!(signals.len(), 2, "{signals:?}");
    for (_, at) in &signals {
        assert!(*at - started < Duration::from_millis(100), "{:?}", *at - started);
    }
    // Each overdue task was aborted and joined before its runtime went, at the deadline.
    let ends = aborted.lock().expect("notes").clone();
    let ended: Vec<u64> = {
        let mut ids: Vec<u64> = ends.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(ended, vec![1, 2], "the third run was not touched: {ends:?}");
    for (_, at) in &ends {
        assert!(
            *at - started >= STREAM_SHUTDOWN_TIMEOUT - Duration::from_millis(5),
            "no task ended before its deadline: {:?}",
            *at - started
        );
    }
    // The third stream stays, drained through the wait: its producer was never
    // starved by it and the frames were stored.
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].vehicle_id, core_id(3));
    assert!(streams[0].task.is_some(), "the continuing run is left running");
    assert!(budget.marks.len() >= 45, "drained all through the wait: {} drains", budget.marks.len());
    assert!(
        cache.contains_key(&core_id(3)),
        "the continuing stream's telemetry was serviced during the wait"
    );
    // The stopping runs' own frames, the final ones included, were stored once each:
    // everything their queues accepted is stored exactly once, the last frame of each
    // among them (a distinct final frame per car), and nothing else is.
    let committed_for = |vehicle: u64| -> Vec<i64> {
        let mut frames: Vec<i64> = seam
            .loop_log()
            .into_iter()
            .filter_map(|event| match event {
                LoopEvent::FrameCommitted { vehicle: car, observed_at_ms } if car == vehicle => {
                    Some(observed_at_ms)
                }
                _ => None,
            })
            .collect();
        frames.sort_unstable();
        frames
    };
    for (id, delivered, _) in &producers {
        let mut accepted = delivered.lock().expect("accepted").clone();
        accepted.sort_unstable();
        let committed = committed_for(*id);
        assert!(accepted.len() >= 90, "car {id}: its queue accepted frames through the wait ({})", accepted.len());
        if *id == 3 {
            // The continuing run: what was stored is a subset of what its queue
            // accepted, each once; the frames still queued are stored later.
            assert!(committed.len() >= 45, "car 3 was drained through the wait");
            let mut seen = committed.clone();
            seen.dedup();
            assert_eq!(seen, committed, "car 3: no frame twice");
            assert!(committed.iter().all(|frame| accepted.contains(frame)));
            continue;
        }
        assert_eq!(
            committed, accepted,
            "car {id}: every frame its queue accepted was stored, exactly once"
        );
        let last = *accepted.last().expect("a final frame");
        assert_eq!(
            committed.iter().filter(|frame| **frame == last).count(),
            1,
            "car {id}: its final frame was stored once"
        );
        let hub = cache[&core_id(*id)].registered_vehicle_id;
        assert!(
            store
                .load_lifecycle_state(hub)
                .expect("lifecycle")
                .expect("state")
                .last_observation_id
                >= 1,
            "car {id} stored frames"
        );
    }
    for (_, _, producer) in producers {
        producer.abort();
    }
    // No early replacement: the runtimes of the stopped runs were removed only after
    // their tasks had ended, and nothing admitted another in the meantime.
    assert!(streams.iter().all(|stream| stream.vehicle_id == core_id(3)));
    assert!(pending.ended.is_empty(), "nothing else ended meanwhile");
    // The control events of the stopping runs re-admitted nothing: a `Healthy` would
    // have reset the failure count the cars carry, while the continuing run's events
    // were applied as ever.
    for id in [1, 2] {
        assert_eq!(
            core.scheduler.vehicles[&core_id(id)].stream.task_failures,
            2,
            "car {id}: a stopping run's Healthy is not applied"
        );
    }
    assert_eq!(core.scheduler.vehicles[&core_id(3)].stream.task_failures, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r33a_the_owner_loop_raises_shutdown_before_final_store_work() {
    // At the entry of the run's last store transaction, the credential unit's shutdown
    // flag is already up: it was raised the moment the run decided to end, before the
    // loop waited for the manager (which the test holds) or did anything else. No token
    // request follows the decision.
    let mut bed = DrainBed::start(1, |_, _| {}).await;
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the loop's first iterations", || seam.loop_iterations() >= 2).await;
    let observed = Arc::new(Mutex::new(Vec::<bool>::new()));
    {
        let flag = Arc::clone(&bed.unit_shutdown);
        let observed = Arc::clone(&observed);
        seam.set_finish_entry(Arc::new(move || {
            observed.lock().expect("observed").push(flag.load(Ordering::SeqCst));
        }));
    }
    assert!(!bed.unit_shutdown.load(Ordering::SeqCst), "not before the run ends");
    let tokens_before = bed.wire.tokens.load(Ordering::SeqCst);
    // The manager is held, so the unit's own shutdown would have to wait for it.
    let manager = Arc::clone(&bed.manager);
    let held = manager.lock().await;
    bed.shutdown.take().expect("shutdown").send(()).expect("shutdown");
    let mut task = bed.task.take().expect("task");
    let observed_probe = Arc::clone(&observed);
    timeout(Duration::from_secs(10), async {
        while observed_probe.lock().expect("observed").is_empty() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the last transaction is entered while the manager is still held");
    assert_eq!(
        observed.lock().expect("observed").clone(),
        vec![true],
        "the shutdown flag was already raised at the entry of the final store work"
    );
    drop(held);
    join_supervised_restart_task("the stop", &mut task)
        .await
        .expect("an orderly stop");
    bed.server.abort();
    assert_eq!(
        bed.wire.tokens.load(Ordering::SeqCst),
        tokens_before,
        "no token request after the decision to stop"
    );
    // (The data directory outlives the run so the store above stayed open.)
    let _ = &bed.temporary;
}

// ---- B2 fix round, P2 fix pass: ownership of the runs a stop is winding down -----------

/// Sets its flag when it is dropped: the task that held it has ended.
struct EndedFlag(Arc<AtomicBool>);

impl Drop for EndedFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A run of the car that ignores its stop signal and sets `ended` when its task ends
/// (finishes, is aborted or is dropped). It returns the runtime and the sender of the
/// run's bounded queue.
fn hung_stream_runtime(
    id: u64,
    ended: &Arc<AtomicBool>,
) -> (VehicleStreamRuntime, mpsc::Sender<StreamEvent>) {
    let (events, receiver) = mpsc::channel(K_STREAM_EVENT_CAPACITY);
    let (shutdown, stop) = oneshot::channel::<()>();
    let end = EndedFlag(Arc::clone(ended));
    let task = tokio::spawn(async move {
        let _end = end;
        let _ = stop.await;
        std::future::pending::<()>().await;
        Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
    });
    (
        VehicleStreamRuntime {
            vehicle_id: core_id(id),
            power_gate: Arc::new(StreamPowerGate::default()),
            sensitive_access_failure: Arc::new(AtomicBool::new(false)),
            events: receiver,
            _shutdown: Some(shutdown),
            task: Some(task),
        },
        events,
    )
}

/// Poll a future once: its output if it is ready, else `None`.
async fn poll_once<F: Future + Unpin>(future: &mut F) -> Option<F::Output> {
    tokio::select! {
        biased;
        output = &mut *future => Some(output),
        () = std::future::ready(()) => None,
    }
}

/// A telemetry frame of a car, its source time `offset_ms` after `base_ms`.
fn drain_stream_frame(base_ms: i64, offset_ms: i64) -> StreamEvent {
    StreamEvent::Telemetry {
        update: Box::new(frame(base_ms + offset_ms, Some("P"), Some(0), Some(0))),
        queued_at: Instant::now(),
    }
}

#[tokio::test(start_paused = true)]
async fn b2_cancelling_a_bulk_stop_preserves_child_ownership_for_no_egress_cleanup() {
    // Two runs ignore their stop signal. The stop is dropped while it waits for them
    // (the supervisor's loss or shutdown cancels the collection future that holds it).
    // The runs' handles must still be owned by their runtimes then, so the cleanup
    // that follows can abort and join them: a handle moved into the dropped wait would
    // leave the credential-bearing tasks detached and the runtimes with nothing to join.
    let directory = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(directory.path()).expect("store");
    let mut core = Core::owner();
    let t0 = core.t0;
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online"), core_vehicle(2, "online")], t0);
    let ended: Vec<Arc<AtomicBool>> = (0..2).map(|_| Arc::new(AtomicBool::new(false))).collect();
    let (first, _first_events) = hung_stream_runtime(1, &ended[0]);
    let (second, _second_events) = hung_stream_runtime(2, &ended[1]);
    let mut streams = vec![first, second];
    tokio::task::yield_now().await;
    let mut cache = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let mut budget = DrainBudget::new(Instant::now());
    let cancelled = timeout(
        Duration::from_secs(2),
        stop_vehicle_streams(
            &store,
            &mut core.scheduler,
            &mut streams,
            &[core_id(1), core_id(2)],
            &mut cache,
            &mut pending,
            &mut budget,
        ),
    )
    .await;
    assert!(cancelled.is_err(), "the stop was still waiting when it was dropped");
    assert_eq!(streams.len(), 2, "both runtimes remain");
    for (index, stream) in streams.iter().enumerate() {
        let task = stream
            .task
            .as_ref()
            .unwrap_or_else(|| panic!("run {index}: its handle was lost with the dropped stop"));
        assert!(!task.is_finished(), "run {index} is still alive");
        assert!(!ended[index].load(Ordering::SeqCst), "run {index} has not ended");
    }
    // The no-egress cleanup then aborts and joins both before it returns.
    abort_and_clear_manual_probe_streams_without_egress(&mut streams).await;
    assert!(streams.is_empty());
    for (index, flag) in ended.iter().enumerate() {
        assert!(flag.load(Ordering::SeqCst), "run {index} had ended when the cleanup returned");
    }
}

#[tokio::test(start_paused = true)]
async fn b2_cancelling_a_single_and_a_manual_stop_preserves_child_ownership() {
    // The single-car stop and the stop-all helper wait for the run the same way: a
    // dropped wait leaves the handle to its runtime.
    let directory = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(directory.path()).expect("store");
    let _ = &store;
    for manual in [false, true] {
        let ended = Arc::new(AtomicBool::new(false));
        let (runtime, _events) = hung_stream_runtime(1, &ended);
        let mut streams = vec![runtime];
        tokio::task::yield_now().await;
        let cancelled = if manual {
            timeout(Duration::from_secs(2), stop_manual_probe_streams(&mut streams))
                .await
                .is_err()
        } else {
            timeout(
                Duration::from_secs(2),
                disconnect_vehicle_stream(&mut streams, core_id(1)),
            )
            .await
            .is_err()
        };
        assert!(cancelled, "manual {manual}: still waiting when dropped");
        let task = streams[0]
            .task
            .as_ref()
            .unwrap_or_else(|| panic!("manual {manual}: the handle was lost"));
        assert!(!task.is_finished() && !ended.load(Ordering::SeqCst));
        abort_and_clear_manual_probe_streams_without_egress(&mut streams).await;
        assert!(ended.load(Ordering::SeqCst), "manual {manual}: joined");
    }
}

#[tokio::test(start_paused = true)]
async fn b2_a_stream_drain_error_joins_selected_children_before_returning() {
    // Two selected runs ignore their stop and a third continues. A frame of a
    // selected car is eligible, and the store fails when the stop's drain writes it.
    // The stop returns the store's error, but only after both selected tasks have
    // ended (aborted and joined): none may outlive the call with the credential
    // manager. The continuing run is untouched.
    let directory = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(directory.path()).expect("store");
    let mut core = Core::owner();
    let t0 = core.t0;
    core.scheduler.accept_discovery(
        vec![
            core_vehicle(1, "online"),
            core_vehicle(2, "online"),
            core_vehicle(3, "online"),
        ],
        t0,
    );
    let ended: Vec<Arc<AtomicBool>> = (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
    let mut streams = Vec::new();
    let mut senders = Vec::new();
    for id in 1..=3u64 {
        let (runtime, events) = hung_stream_runtime(id, &ended[(id - 1) as usize]);
        streams.push(runtime);
        senders.push(events);
    }
    tokio::task::yield_now().await;
    let base_ms = current_epoch_millis().expect("clock") - 60_000;
    senders[0]
        .send(drain_stream_frame(base_ms, 1_000))
        .await
        .expect("a queued frame");
    store.inject_stream_fault(StreamFaultPoint::RawInsert);
    let mut cache = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let mut budget = DrainBudget::new(Instant::now());
    let result = stop_vehicle_streams(
        &store,
        &mut core.scheduler,
        &mut streams,
        &[core_id(1), core_id(2)],
        &mut cache,
        &mut pending,
        &mut budget,
    )
    .await;
    assert!(
        matches!(result, Err(CollectorError::Store(_))),
        "the store's own error is returned: {result:?}"
    );
    assert!(ended[0].load(Ordering::SeqCst), "selected run 1 had ended when the stop returned");
    assert!(ended[1].load(Ordering::SeqCst), "selected run 2 had ended when the stop returned");
    assert!(!ended[2].load(Ordering::SeqCst), "the continuing run is untouched");
    assert!(
        streams
            .iter()
            .filter(|stream| stream.vehicle_id != core_id(3))
            .all(|stream| stream.task.is_none()),
        "no selected handle is left unjoined"
    );
    let third = streams
        .iter()
        .find(|stream| stream.vehicle_id == core_id(3))
        .expect("the continuing runtime");
    assert!(third.task.as_ref().is_some_and(|task| !task.is_finished()));
}

#[tokio::test(start_paused = true)]
async fn b2_an_all_stream_stop_uses_the_previous_drain_deadline() {
    // The all-stream stop (a credential stop, the orderly shutdown) shares the loop's
    // drain deadline like any other wait. The last drain was at t0 and the stop starts
    // 60 ms later with two runs that ignore their stop signal and eligible frames
    // queued: nothing is stored at +99 ms, both frames are stored at +100 ms while the
    // runs are still alive, and the final frames queued meanwhile are stored exactly
    // once when the runs are aborted at their own deadline.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(Arc::clone(&seam), async {
            let directory = crate::private_tempdir().expect("temporary store");
            let store = HubStore::initialize(directory.path()).expect("store");
            let mut core = Core::owner();
            let t0 = core.t0;
            core.scheduler
                .accept_discovery(vec![core_vehicle(1, "online"), core_vehicle(2, "online")], t0);
            let ended: Vec<Arc<AtomicBool>> =
                (0..2).map(|_| Arc::new(AtomicBool::new(false))).collect();
            let (first, first_events) = hung_stream_runtime(1, &ended[0]);
            let (second, second_events) = hung_stream_runtime(2, &ended[1]);
            let mut streams = vec![first, second];
            tokio::task::yield_now().await;
            let base_ms = current_epoch_millis().expect("clock") - 60_000;
            first_events.send(drain_stream_frame(base_ms, 1_000)).await.expect("frame");
            second_events.send(drain_stream_frame(base_ms, 2_000)).await.expect("frame");
            let drain_t0 = Instant::now();
            let mut budget = DrainBudget::new(drain_t0);
            tokio::time::advance(Duration::from_millis(60)).await;
            let mut cache = HashMap::new();
            let mut pending = StreamDrainResult::default();
            let stop_began = Instant::now();
            let mut stop = Box::pin(stop_all_streams_and_store(
                &store,
                &mut core.scheduler,
                &mut streams,
                &mut cache,
                &mut pending,
                &mut budget,
            ));
            let committed = |seam: &SupervisedCollectorTestSeam| -> Vec<(u64, i64)> {
                seam.loop_log()
                    .into_iter()
                    .filter_map(|event| match event {
                        LoopEvent::FrameCommitted { vehicle, observed_at_ms } => {
                            Some((vehicle, observed_at_ms))
                        }
                        _ => None,
                    })
                    .collect()
            };
            assert!(poll_once(&mut stop).await.is_none(), "the stop waits for the runs");
            // +99 ms after the last drain: the deadline has not come.
            tokio::time::advance(Duration::from_millis(39)).await;
            assert!(poll_once(&mut stop).await.is_none());
            assert!(committed(&seam).is_empty(), "nothing is stored before the deadline");
            // +101 ms (a timer is rounded up to its millisecond): the drain was due at
            // +100 and has serviced both frames while the runs are still alive.
            tokio::time::advance(Duration::from_millis(2)).await;
            assert!(poll_once(&mut stop).await.is_none());
            let mut stored = committed(&seam);
            stored.sort_unstable();
            assert_eq!(
                stored,
                vec![(1, base_ms + 1_000), (2, base_ms + 2_000)],
                "both eligible frames are stored at the shared deadline"
            );
            assert!(
                ended.iter().all(|flag| !flag.load(Ordering::SeqCst)),
                "the runs are still winding down"
            );
            // Distinct final frames, queued before the runs are aborted.
            first_events.send(drain_stream_frame(base_ms, 3_000)).await.expect("final");
            second_events.send(drain_stream_frame(base_ms, 4_000)).await.expect("final");
            tokio::time::advance(STREAM_SHUTDOWN_TIMEOUT).await;
            let mut stopped = None;
            for _ in 0..100 {
                stopped = poll_once(&mut stop).await;
                if stopped.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            stopped.expect("the runs were aborted at their own deadline").expect("the stop");
            drop(stop);
            assert!(Instant::now() - stop_began >= STREAM_SHUTDOWN_TIMEOUT);
            assert!(ended.iter().all(|flag| flag.load(Ordering::SeqCst)));
            let mut stored = committed(&seam);
            stored.sort_unstable();
            assert_eq!(
                stored,
                vec![
                    (1, base_ms + 1_000),
                    (1, base_ms + 3_000),
                    (2, base_ms + 2_000),
                    (2, base_ms + 4_000),
                ],
                "every frame once, the final ones included"
            );
            assert!(budget.marks.len() >= 2, "{:?}", budget.marks);
            assert_instant_near(budget.marks[0], drain_t0 + Duration::from_millis(100), "the first drain");
            assert!(streams.is_empty());
        })
        .await;
}

/// The position of the first logged step that `wanted` accepts.
fn first_logged(log: &[LoopEvent], wanted: impl Fn(&LoopEvent) -> bool) -> Option<usize> {
    log.iter().position(wanted)
}

/// Which observer ends the renewal first in the cases of
/// [`b2_the_owner_supervisor_fences_renewal_on_loss_before_held_cleanup`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenewalEndsFirst {
    /// The supervisor's wait sees the loss first and joins the renewal.
    Supervisor,
    /// The renewal's own revalidation is denied first and it ends on its own.
    Renewal,
}

async fn assert_the_renewal_is_over_before_the_held_cleanup(first: RenewalEndsFirst) {
    // The retained admission is denied while the credential unit's manager is held, so
    // the supervisor's shutdown cannot get past its wait for the unit. Whichever of the
    // two observers (the supervisor's wait or the renewal itself) meets the loss first,
    // the renewal has ended, joined or ended on its own, before the cleanup begins to
    // wait: the order is of acknowledged steps of the seam, never a window of time.
    // Released, the run ends with the sensitive failure and keeps its lease. The
    // renewal's candidates are held before they are ready, so the shutdown arm of the
    // renewal stays pollable and it cannot renew or observe anything the test has not
    // released.
    let mut gates = None;
    let mut bed = DrainBed::start_with_admission(1, true, |_, seam| {
        let select = (first == RenewalEndsFirst::Renewal)
            .then(|| seam.arm_gate("supervisor-select"));
        gates = Some((select, seam.arm_gate("renewal-candidate")));
    })
    .await;
    let (select, renewal) = gates.expect("gates");
    let seam = Arc::clone(&bed.seam);
    renewal.arrived("the renewal's first candidate", 1).await;
    match &select {
        Some(select) => select.arrived("the supervisor's wait", 1).await,
        None => {
            bed.wait_for("the loop's first iterations", || seam.loop_iterations() >= 2)
                .await;
        }
    }
    let heartbeat_before = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("active lease")
        .heartbeat_at_ms;
    let manager = Arc::clone(&bed.manager);
    let held = manager.lock().await;
    invalidate_the_retained_admission(bed.temporary.path());
    if let Some(select) = &select {
        // The renewal meets the denial first: its candidate is released, the denial is
        // acknowledged, and only then is the supervisor's wait released.
        renewal.release(1);
        bed.wait_for("the renewal's own denial", || {
            seam.loop_log().contains(&LoopEvent::RenewalDenied)
        })
        .await;
        select.open();
    }
    bed.wait_for("the shutdown waits for the held cleanup", || {
        seam.loop_log().contains(&LoopEvent::CleanupWaiting)
    })
    .await;
    let log = seam.loop_log();
    let waiting = first_logged(&log, |event| *event == LoopEvent::CleanupWaiting).expect("waiting");
    let over = first_logged(&log, |event| {
        matches!(event, LoopEvent::RenewalJoined | LoopEvent::RenewalEnded)
    })
    .unwrap_or_else(|| panic!("the renewal was neither joined nor ended: {log:?}"));
    assert!(over < waiting, "the renewal is over before the cleanup waits: {log:?}");
    match first {
        RenewalEndsFirst::Supervisor => {
            assert!(log.contains(&LoopEvent::RenewalJoined), "the supervisor joined it: {log:?}");
            assert!(!log.contains(&LoopEvent::RenewalDenied), "{log:?}");
        }
        RenewalEndsFirst::Renewal => {
            // The renewal's own denial is acknowledged (it set the fence). Whether its
            // result or the supervisor's own loss watch ends the supervisor's wait is
            // not an acknowledged order (the watch fires a second after the wait
            // opens, the renewal's result microseconds after its denial), so the
            // outcome asserted is the one both orders share (R-P2F3-7): the wait ended
            // with the run's failure, and the renewal was over (ended on its own or
            // joined) before the cleanup waited, as checked above.
            assert!(log.contains(&LoopEvent::RenewalDenied), "{log:?}");
            assert!(
                log.contains(&LoopEvent::SupervisorSelected { by_renewal: true, ok: false })
                    || log.contains(&LoopEvent::SupervisorSelected { by_renewal: false, ok: false }),
                "the wait ended with the run's failure, by the renewal's result or the loss watch: {log:?}"
            );
            assert!(
                !log.iter().any(|event| matches!(event, LoopEvent::SupervisorSelected { ok: true, .. })),
                "{log:?}"
            );
        }
    }
    assert!(
        bed.task.as_ref().is_some_and(|task| !task.is_finished()),
        "the cleanup is still held when the renewal is over"
    );
    drop(held);
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the supervisor ends", &mut task).await;
    bed.server.abort();
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease is retained to expire");
    assert_eq!(
        retained.heartbeat_at_ms, heartbeat_before,
        "no renewal write after the denial"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_the_owner_supervisor_fences_renewal_on_loss_before_held_cleanup() {
    assert_the_renewal_is_over_before_the_held_cleanup(RenewalEndsFirst::Supervisor).await;
    assert_the_renewal_is_over_before_the_held_cleanup(RenewalEndsFirst::Renewal).await;
}
