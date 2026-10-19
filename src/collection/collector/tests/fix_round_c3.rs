// SPDX-License-Identifier: AGPL-3.0-only

// The C3 fix round for the Codex findings R04 (the stream queues are drained while
// Owner API or credential work is pending) and R05 (an account stop leaves no car
// marked in flight). Synthetic data only; paused time, no network.

/// A scheduler with three online cars, all due, and a store.
struct PumpBed {
    _directory: tempfile::TempDir,
    store: HubStore,
    scheduler: VehicleScheduler,
    identities: IdentityCache,
    cursor_key: CursorKey,
    terrain: mpsc::Sender<()>,
    _terrain_rx: mpsc::Receiver<()>,
    t0: Instant,
}

impl PumpBed {
    fn new() -> Self {
        let directory = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(directory.path()).expect("store");
        let t0 = Instant::now();
        let mut scheduler = VehicleScheduler::new_with(
            core_cadence(),
            crate::config::CollectorMode::Legacy,
            core_cadence().fleet_policy(),
            t0,
            Arc::new(SystemCollectorClock),
            FixedJitter::constant(0.0),
        );
        let cars = [1, 2, 3].map(|id| core_vehicle(id, "online"));
        scheduler.accept_discovery(cars.to_vec(), t0);
        let (terrain, terrain_rx) = mpsc::channel(8);
        Self {
            _directory: directory,
            store,
            scheduler,
            identities: IdentityCache::default(),
            cursor_key: CursorKey::from_bytes([0x47; 32]),
            terrain,
            _terrain_rx: terrain_rx,
            t0,
        }
    }

    fn run(&mut self) -> TickRun<'_> {
        TickRun {
            scheduler: &mut self.scheduler,
            identities: &mut self.identities,
            commit: TickCommit {
                store: &self.store,
                cursor_key: &self.cursor_key,
                provider: CollectorProvider::Legacy,
                offline_drive_timeout: Duration::from_secs(600),
            },
            batch: TickBatch::default(),
            terrain_wake: &self.terrain,
            any_wrote: false,
        }
    }
}

/// A read of `id` that answers `after` later.
fn slow_read(id: u64, after: Duration) -> TickFuture<'static> {
    Box::pin(async move {
        tokio::time::sleep(after).await;
        (
            TickKey::Read(core_id(id)),
            Ok(TickPayload::Data(VehicleData::for_test(id, parked_fields()))),
        )
    })
}

/// A stream runtime for car 1 whose queue holds two events, and a producer that
/// sends `count` events with C4's 10 s wait for room (C4 R49). Returns the runtime,
/// the number of events the producer has put on the queue, and the producer.
fn stream_with_producer(
    count: usize,
) -> (
    Vec<VehicleStreamRuntime>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<mpsc::Sender<StreamEvent>>,
) {
    let (sender, receiver) = mpsc::channel(2);
    let streams = vec![VehicleStreamRuntime {
        vehicle_id: core_id(1),
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: None,
        task: None,
    }];
    let delivered = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&delivered);
    let producer = tokio::spawn(async move {
        for _ in 0..count {
            // The supervisor ends the run when this wait runs out (C4 R49).
            timeout(Duration::from_secs(10), sender.send(StreamEvent::Healthy))
                .await
                .expect("room in the queue within C4's 10 s wait")
                .expect("the receiver is alive");
            counter.fetch_add(1, Ordering::SeqCst);
        }
        sender
    });
    (streams, delivered, producer)
}

#[tokio::test(start_paused = true)]
async fn r56g_the_stream_queue_is_drained_while_the_ticks_requests_are_pending() {
    let mut bed = PumpBed::new();
    let due = bed.scheduler.due_vehicles(bed.t0);
    assert_eq!(due.len(), 3);
    let (mut streams, delivered, producer) = stream_with_producer(10);
    let mut projection = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let mut budget = DrainBudget::new(Instant::now());
    let store = bed.store.clone();
    let started = Instant::now();
    let mut run = bed.run();
    // Every read answers after 5 s: far beyond the 100 ms drain bound, and half of
    // C4's wait for room in a queue of two.
    let requests = due
        .iter()
        .map(|id| slow_read(id.get(), Duration::from_secs(5)))
        .collect::<Vec<_>>();
    drive_requests(
        &mut run,
        requests,
        Some(StreamPump {
            store: &store,
            streams: &mut streams,
            projection_car_ids: &mut projection,
            pending: &mut pending,
            budget: &mut budget,
        }),
        |run: &mut TickRun<'_>, key, result, now| {
            let (TickKey::Read(id), Ok(TickPayload::Data(snapshot))) = (key, result) else {
                panic!("a read result");
            };
            run.scheduler
                .vehicle_succeeded(id, poll_phase(&snapshot), true, now);
            Ok(ReadFlow::Continue)
        },
    )
    .await
    .expect("tick");
    assert_eq!(
        Instant::now() - started,
        Duration::from_secs(5),
        "the drain does not delay the requests"
    );
    drop(run);
    // Every event reached the loop's side of the queue while the reads waited, so
    // the producer never waited long enough for the supervisor to end the run.
    tokio::task::yield_now().await;
    assert_eq!(delivered.load(Ordering::SeqCst), 10);
    assert!(streams[0].events.is_empty());
    assert_eq!(pending.transition, StreamAuthenticationTransition::Authenticated);
    drop(producer.await.expect("producer"));
}

#[tokio::test(start_paused = true)]
async fn r56g_the_stream_queue_is_drained_while_a_list_or_a_refresh_is_pending() {
    let mut bed = PumpBed::new();
    let (mut streams, delivered, producer) = stream_with_producer(10);
    let mut projection = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let mut budget = DrainBudget::new(Instant::now());
    let store = bed.store.clone();
    let output = StreamPump {
        store: &store,
        streams: &mut streams,
        projection_car_ids: &mut projection,
        pending: &mut pending,
        budget: &mut budget,
    }
    .while_waiting(&mut bed.scheduler, async {
        // A list, or a token refresh the loop's maintenance starts, that takes
        // 30 s: three times C4's wait for room.
        tokio::time::sleep(Duration::from_secs(30)).await;
        "the answer"
    })
    .await
    .expect("no store fault");
    // The work's own output is returned untouched, and nothing was lost meanwhile.
    assert_eq!(output, "the answer");
    assert_eq!(delivered.load(Ordering::SeqCst), 10);
    assert!(streams[0].events.is_empty());
    assert_eq!(pending.transition, StreamAuthenticationTransition::Authenticated);
    drop(producer.await.expect("producer"));
}

#[tokio::test(start_paused = true)]
async fn r56g_without_a_stream_the_wait_is_the_work_alone() {
    let mut bed = PumpBed::new();
    let mut streams: Vec<VehicleStreamRuntime> = Vec::new();
    let mut projection = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let mut budget = DrainBudget::new(Instant::now());
    let store = bed.store.clone();
    let started = Instant::now();
    let output = StreamPump {
        store: &store,
        streams: &mut streams,
        projection_car_ids: &mut projection,
        pending: &mut pending,
        budget: &mut budget,
    }
    .while_waiting(&mut bed.scheduler, async {
        tokio::time::sleep(Duration::from_millis(250)).await;
        7
    })
    .await
    .expect("no store fault");
    assert_eq!(output, 7);
    assert_eq!(Instant::now() - started, Duration::from_millis(250));
}

/// The loop's own list, through the real credential unit: the stored access token is
/// refused, the unit exchanges the refresh token at an issuer that takes 1.2 s, and
/// the list is resent. The stream queue (room for two) is drained all the while, so a
/// producer that C4 would end after 10 s without room delivers all ten events before
/// the list returns.
#[tokio::test]
async fn r56g_the_loops_list_drains_the_stream_queue_while_its_refresh_is_pending() {
    let (fake, base, client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    fake.token_delay.store(1_200, Ordering::SeqCst);
    fake.rejected
        .lock()
        .unwrap()
        .insert("stored-access".to_owned());
    let mut bed = PumpBed::new();
    let (mut streams, delivered, producer) = stream_with_producer(10);
    let mut projection = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let mut budget = DrainBudget::new(Instant::now());
    let store = bed.store.clone();
    let ledger = bed.scheduler.request_ledger();
    let checked = list_with_stream_drain(
        StreamPump {
            store: &store,
            streams: &mut streams,
            projection_car_ids: &mut projection,
            pending: &mut pending,
            budget: &mut budget,
        },
        &mut bed.scheduler,
        &client,
        &auth,
        true,
        &ledger,
    )
    .await
    .expect("no store fault");
    assert!(checked.result.is_ok(), "the list succeeds after the refresh");
    assert_eq!(fake.token_requests(), 1, "the list waited for one token exchange");
    assert_eq!(delivered.load(Ordering::SeqCst), 10);
    assert!(streams[0].events.is_empty());
    assert_eq!(pending.transition, StreamAuthenticationTransition::Authenticated);
    drop(producer.await.expect("producer"));
    server.abort();
}

#[test]
fn r56g_drain_results_fold_in_the_order_the_loop_needs() {
    use StreamAuthenticationTransition::{Authenticated, NoChange, Rejected};
    let drained = |transition, ended: Vec<u64>, backlog| StreamDrainResult {
        transition,
        backlog,
        terminal_error: None,
        ended: ended.into_iter().map(core_id).collect(),
    };
    let mut folded = StreamDrainResult::default();
    assert_eq!(folded.transition, NoChange);
    folded.absorb(drained(Authenticated, vec![1], true));
    folded.absorb(drained(Rejected, vec![2], false));
    folded.absorb(drained(Authenticated, vec![], false));
    folded.absorb(drained(NoChange, vec![], false));
    // A rejection outranks an authentication, the ended runs add up, and the
    // backlog is the last drain's.
    assert_eq!(folded.transition, Rejected);
    assert_eq!(folded.ended, vec![core_id(1), core_id(2)]);
    assert!(!folded.backlog);
    // The first terminal error stays.
    folded.absorb(StreamDrainResult {
        terminal_error: Some(CollectorError::SensitiveAccessUnavailable),
        ..Default::default()
    });
    folded.absorb(StreamDrainResult {
        terminal_error: Some(CollectorError::SelectedVehicleMissing),
        ..Default::default()
    });
    assert!(matches!(
        folded.terminal_error,
        Some(CollectorError::SensitiveAccessUnavailable)
    ));
}

#[tokio::test(start_paused = true)]
async fn r56e_an_ended_run_met_during_a_wait_is_removed_after_the_tick_and_a_terminal_error_ends_the_collector() {
    let mut bed = PumpBed::new();
    let (mut streams, _delivered, producer) = stream_with_producer(0);
    drop(producer.await.expect("producer"));
    assert_eq!(streams.len(), 1);
    let mut projection = HashMap::new();
    let store = bed.store.clone();
    // Nothing pending: nothing changes.
    let mut pending = StreamDrainResult::default();
    settle_pending_stream_drain(&store, &mut bed.scheduler, &mut streams, &mut projection, &mut pending)
        .await
        .expect("nothing to settle");
    assert_eq!(streams.len(), 1);
    // A run that ended during a wait is removed, and a rejection is left for the
    // loop's next drain.
    pending.ended.push(core_id(1));
    pending.transition = StreamAuthenticationTransition::Rejected;
    settle_pending_stream_drain(&store, &mut bed.scheduler, &mut streams, &mut projection, &mut pending)
        .await
        .expect("settled");
    assert!(streams.is_empty());
    assert!(pending.ended.is_empty());
    assert_eq!(pending.transition, StreamAuthenticationTransition::Rejected);
    // A terminal error met during a wait ends the collector, once.
    pending.terminal_error = Some(CollectorError::SensitiveAccessUnavailable);
    let error = settle_pending_stream_drain(
        &store,
        &mut bed.scheduler,
        &mut streams,
        &mut projection,
        &mut pending,
    )
    .await
    .expect_err("terminal");
    assert!(matches!(error, CollectorError::SensitiveAccessUnavailable));
    assert!(pending.terminal_error.is_none());
}

// ---- R05 -------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn r47_a_terminal_read_leaves_no_car_marked_in_flight_and_every_car_due_after_the_stop() {
    let mut bed = PumpBed::new();
    let due = bed.scheduler.due_vehicles(bed.t0);
    assert_eq!(due, vec![core_id(1), core_id(2), core_id(3)]);
    let started = Instant::now();
    let mut run = bed.run();
    // Car 1's read comes back at once with an answer that stops the account; the
    // other two are still waiting for their answers.
    let requests: Vec<TickFuture<'_>> = vec![
        Box::pin(async {
            (
                TickKey::Read(core_id(1)),
                Err(CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)),
            )
        }),
        slow_read(2, Duration::from_secs(300)),
        slow_read(3, Duration::from_secs(300)),
    ];
    let flow = drive_requests(
        &mut run,
        requests,
        None,
        |run: &mut TickRun<'_>, key, result, now| {
            assert!(matches!(key, TickKey::Read(id) if id == core_id(1)));
            assert!(result.is_err());
            run.scheduler.enter_credential_stop(now);
            Ok(ReadFlow::SignedOut)
        },
    )
    .await
    .expect("tick");
    assert_eq!(flow, ReadFlow::SignedOut);
    assert!(
        Instant::now() - started < Duration::from_secs(1),
        "the stop does not wait for the other reads"
    );
    drop(run);
    for id in [1, 2, 3] {
        assert!(
            !bed.scheduler.vehicles[&core_id(id)].in_flight,
            "car {id} is not in flight any more"
        );
    }
    // Once the stop is lifted every car is due again: nothing hides it from the
    // next tick.
    let later = bed.t0 + Duration::from_secs(10);
    bed.scheduler.clear_credential_stop(later);
    assert_eq!(
        bed.scheduler.due_vehicles(later),
        vec![core_id(1), core_id(2), core_id(3)]
    );
}

// ---- R06 -------------------------------------------------------------------------

/// A graceful shutdown drops the collection future, and with it the caller of a
/// Fleet token refresh that Tesla has not yet answered. The shutdown waits for the
/// exchange to store its successor before the clients are dropped, so an ordinary
/// stop does not cost the refresh chain (C3 R8, Codex R06).
#[tokio::test]
async fn r06_shutdown_waits_for_a_started_fleet_exchange_to_store_its_successor() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "fleet-old-access".to_owned(),
        "fleet-old-refresh".to_owned(),
        "fleet-client".to_owned(),
        crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
        60,
    )
    .expect("setup credentials");
    crate::fleet_credentials::persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("fake auth listener");
    let address = listener.local_addr().expect("fake auth address");
    let router = Router::new().route(
        "/oauth2/v3/token",
        post(|| async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                r#"{"access_token":"fleet-next-access","refresh_token":"fleet-next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
            )
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake auth server");
    });
    let auth_api = FleetAuthApi::for_fake_http(
        url::Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL"),
        Duration::from_secs(5),
    )
    .expect("fake Fleet auth client");
    let manager = Arc::new(tokio::sync::Mutex::new(
        FleetAuthManager::from_store(store.clone(), temporary.path()).expect("Fleet manager"),
    ));
    // The collection future is dropped 50 ms into the exchange.
    {
        let mut guard = manager.lock().await;
        assert!(
            timeout(
                Duration::from_millis(50),
                guard.refresh_now(&auth_api, SystemTime::now())
            )
            .await
            .is_err()
        );
    }
    assert!(store.has_unresolved_fleet_refresh().expect("open attempt"));

    let settlement = settle_fleet_exchange(&manager, None).await;
    assert!(
        matches!(settlement, FleetSettlement::Settled),
        "the exchange completed: {settlement:?}"
    );
    settlement.into_result().expect("settled");

    assert!(
        !store.has_unresolved_fleet_refresh().expect("closed attempt"),
        "the successor is stored before the clients are dropped"
    );
    let stored = store
        .load_fleet_tokens()
        .expect("Fleet row")
        .expect("Fleet credentials");
    assert_eq!(
        stored.credential_generation(),
        Some(manager.lock().await.credential_generation())
    );
    server.abort();
    let _ = server.await;
}

// ---- P3b (B2 fix round): the Fleet settlement's typed outcome ---------------------------------------

/// Fleet credentials that are due, and a manager over them whose detached exchange
/// a test scripts: it takes a rotated answer after a hold, and runs the real
/// validation, admission check, encryption and guarded write (P3b.3).
struct SettlementBed {
    temporary: tempfile::TempDir,
    store: HubStore,
    manager: Arc<tokio::sync::Mutex<FleetAuthManager>>,
    auth_api: FleetAuthApi,
    admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
    encryption: Arc<AtomicUsize>,
}

impl SettlementBed {
    async fn new(admitted: bool) -> Self {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let credentials = FleetSetupCredentials::new(
            "fleet-old-access".to_owned(),
            "fleet-old-refresh".to_owned(),
            "fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            60,
        )
        .expect("setup credentials");
        crate::fleet_credentials::persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            UNIX_EPOCH + Duration::from_secs(1_000),
        )
        .expect("persist due credentials");
        let admission = admitted.then(|| {
            crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit")
        });
        let mut manager = match &admission {
            Some(admission) => FleetAuthManager::from_store_for_admitted_user(
                store.clone(),
                temporary.path(),
                Arc::clone(admission),
                None,
            ),
            None => FleetAuthManager::from_store(store.clone(), temporary.path()),
        }
        .expect("Fleet manager");
        let encryption = manager.observe_exchange_encryption();
        let auth_api = FleetAuthApi::for_fake_http(
            url::Url::parse("http://127.0.0.1:9/oauth2/v3/token").expect("unused auth URL"),
            Duration::from_secs(1),
        )
        .expect("Fleet auth client");
        Self {
            temporary,
            store,
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            auth_api,
            admission,
            encryption,
        }
    }

    /// Start a detached exchange that takes a rotated answer once `hold` ends.
    async fn start_exchange(&self, hold: impl Future<Output = ()> + Send + 'static) {
        let scripted = crate::fleet_credentials::ScriptedExchange {
            refreshed: crate::fleet_api::FleetRefreshedTokens {
                access_token: crate::fleet_api::FleetAccessToken::new("fleet-next-access")
                    .expect("access token"),
                refresh_token: crate::fleet_api::FleetRefreshToken::new("fleet-next-refresh")
                    .expect("refresh token"),
                expires_in_seconds: 28_800,
            },
            hold: Box::pin(hold),
        };
        self.manager
            .lock()
            .await
            .start_scripted_exchange(&self.auth_api, SystemTime::now(), scripted)
            .expect("the exchange starts");
    }

    fn encryption_entries(&self) -> usize {
        self.encryption.load(Ordering::SeqCst)
    }

    /// Deny the retained admission by replacing its lock file.
    fn deny_admission(&self) {
        let lock_path = self
            .temporary
            .path()
            .join(crate::user_lifetime_lock::LOCK_FILE_NAME);
        fs::remove_file(&lock_path).expect("remove the admitted lock path");
        fs::write(&lock_path, b"").expect("replace the admitted lock path");
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
            .expect("replacement lock mode");
    }

    /// The stored pair's generation and the state of the refresh attempts.
    fn stored(&self) -> (Option<Uuid>, Vec<String>) {
        let generation = self
            .store
            .load_fleet_tokens()
            .expect("Fleet row")
            .expect("Fleet credentials")
            .credential_generation();
        let outcomes = self
            .store
            .open()
            .expect("catalogue")
            .prepare(
                "SELECT outcome FROM outbound_request_receipts
                  WHERE transport = 'fleet_api' AND operation = 'token_refresh' ORDER BY id",
            )
            .expect("receipts query")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("receipts")
            .collect::<Result<Vec<_>, _>>()
            .expect("receipts read");
        (generation, outcomes)
    }
}

#[tokio::test(start_paused = true)]
async fn r06_settlement_covers_completion_before_65_seconds() {
    // An exchange whose answer is due at 64 seconds, through the real receipt,
    // validation, encryption and guarded write, with no socket: the settlement's
    // allowance covers it, and the successor is stored and adopted at 64 seconds.
    let bed = SettlementBed::new(false).await;
    let input = bed.stored().0;
    let began = Instant::now();
    bed.start_exchange(tokio::time::sleep(Duration::from_secs(64))).await;

    let settlement = settle_fleet_exchange(&bed.manager, None).await;

    assert!(matches!(settlement, FleetSettlement::Settled), "{settlement:?}");
    assert_instant_near(Instant::now(), began + Duration::from_secs(64), "the completion");
    let (generation, outcomes) = bed.stored();
    assert_ne!(generation, input, "the successor is stored");
    assert_eq!(outcomes, ["success"]);
    assert_eq!(
        generation,
        Some(bed.manager.lock().await.credential_generation()),
        "and adopted"
    );
    assert_eq!(bed.encryption_entries(), 1);
}

#[tokio::test(start_paused = true)]
async fn r06_settlement_expires_at_65_seconds() {
    // The same exchange due at 66 seconds: the settlement gives up at 65 with its
    // typed timeout, the attempt unresolved and the original pair stored. The job is
    // aborted and joined before time moves on.
    let bed = SettlementBed::new(false).await;
    let input = bed.stored().0;
    let began = Instant::now();
    bed.start_exchange(tokio::time::sleep(Duration::from_secs(66))).await;

    let settlement = settle_fleet_exchange(&bed.manager, None).await;

    assert!(matches!(settlement, FleetSettlement::TimedOut), "{settlement:?}");
    assert_instant_near(Instant::now(), began + Duration::from_secs(65), "the timeout");
    bed.manager.lock().await.abort_exchange_for_test().await;
    let (generation, outcomes) = bed.stored();
    assert_eq!(generation, input, "the original pair stands");
    assert_eq!(outcomes, ["started"], "the attempt stays unresolved");
    assert!(bed.store.has_unresolved_fleet_refresh().expect("receipts"));
    assert_eq!(bed.encryption_entries(), 0);
    assert!(settlement.into_result().is_ok(), "a timeout is no error of the run");
}

#[tokio::test(start_paused = true)]
async fn fleet_settlement_returns_late_sensitive_failure() {
    // A held real job whose admission was intact when it started and is denied
    // before the encryption: the settlement returns the sensitive failure typed, and
    // the run's result carries it. The settlement's own boundary check is off in the
    // first case (no admission is passed), so the failure is the job's.
    let bed = SettlementBed::new(true).await;
    let input = bed.stored().0;
    let (release, held) = oneshot::channel::<()>();
    bed.start_exchange(async move {
        let _ = held.await;
    })
    .await;
    bed.deny_admission();
    release.send(()).expect("the job is held");
    let settlement = settle_fleet_exchange(&bed.manager, None).await;
    assert!(
        matches!(
            &settlement,
            FleetSettlement::Failed(FleetCredentialError::SensitiveAccessUnavailable)
        ),
        "{settlement:?}"
    );
    assert_eq!(bed.encryption_entries(), 0, "a denied job does not encrypt");
    assert_eq!(bed.stored(), (input, vec!["started".to_owned()]), "the attempt stays fenced");
    let result = settlement.into_result();
    assert!(
        matches!(&result, Err(error) if loses_sensitive_access(error)),
        "{result:?}"
    );

    // Positive control: the same job with the admission intact is settled.
    let control = SettlementBed::new(true).await;
    let (release, held) = oneshot::channel::<()>();
    control
        .start_exchange(async move {
            let _ = held.await;
        })
        .await;
    release.send(()).expect("the job is held");
    let settlement = settle_fleet_exchange(&control.manager, control.admission.as_deref()).await;
    assert!(matches!(settlement, FleetSettlement::Settled), "{settlement:?}");
    assert_eq!(control.encryption_entries(), 1);

    // The denial at the settlement's own boundary stays sensitive when the wait ends
    // by the allowance: the timeout does not hide it.
    let late = SettlementBed::new(true).await;
    late.start_exchange(tokio::time::sleep(Duration::from_secs(66))).await;
    late.deny_admission();
    let settlement = settle_fleet_exchange(&late.manager, late.admission.as_deref()).await;
    assert!(
        matches!(
            &settlement,
            FleetSettlement::Failed(FleetCredentialError::SensitiveAccessUnavailable)
        ),
        "{settlement:?}"
    );
    late.manager.lock().await.abort_exchange_for_test().await;
}

// ---- P2 (B2 fix round): one drain deadline for every wait --------------------------------------------

/// A stream runtime for car 1 with an empty queue, and the sender that keeps it open.
fn idle_stream() -> (Vec<VehicleStreamRuntime>, mpsc::Sender<StreamEvent>) {
    let (events, receiver) = mpsc::channel(8);
    let streams = vec![VehicleStreamRuntime {
        vehicle_id: core_id(1),
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: None,
        task: None,
    }];
    (streams, events)
}

/// Paused time rounds a timer up to its millisecond tick, so an instant is the
/// expected one or at most two milliseconds after it.
fn assert_instant_near(actual: Instant, expected: Instant, label: &str) {
    assert!(
        actual >= expected && actual - expected <= Duration::from_millis(2),
        "{label}: {:?} after the expected instant",
        actual.checked_duration_since(expected)
    );
}

fn assert_marks(budget: &DrainBudget, t0: Instant, offsets_ms: &[u64]) {
    assert_eq!(budget.marks.len(), offsets_ms.len(), "{:?}", budget.marks);
    for (mark, offset) in budget.marks.iter().zip(offsets_ms) {
        assert_instant_near(*mark, t0 + Duration::from_millis(*offset), &format!("drain at +{offset}"));
    }
}

#[tokio::test(start_paused = true)]
async fn r56g_a_wait_uses_the_previous_drain_deadline() {
    let mut bed = PumpBed::new();
    // The scheduler's clock is frozen: the drain deadline reads the runtime's timer,
    // never the scheduler's clock.
    let manual = ManualClock::new(Instant::now(), CORE_W0);
    bed.scheduler.clock = manual.clone();
    let frozen = manual.mono_now();
    let (mut streams, _events) = idle_stream();
    let mut projection = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let store = bed.store.clone();
    let t0 = Instant::now();
    let mut budget = DrainBudget::new(t0);
    budget.mark();
    tokio::time::advance(Duration::from_millis(60)).await;
    // The wait begins 60 ms into the interval and lasts 300 ms: the drains are due
    // 100 ms after the previous one, not 100 ms after the wait began.
    StreamPump {
        store: &store,
        streams: &mut streams,
        projection_car_ids: &mut projection,
        pending: &mut pending,
        budget: &mut budget,
    }
    .while_waiting(&mut bed.scheduler, async {
        tokio::time::sleep(Duration::from_millis(300)).await;
    })
    .await
    .expect("no store fault");
    assert_marks(&budget, t0, &[0, 100, 200, 300]);
    // Another wait, which begins at +360 with the deadline at +400, and ends before
    // it, drains nothing.
    StreamPump {
        store: &store,
        streams: &mut streams,
        projection_car_ids: &mut projection,
        pending: &mut pending,
        budget: &mut budget,
    }
    .while_waiting(&mut bed.scheduler, async {
        tokio::time::sleep(Duration::from_millis(20)).await;
    })
    .await
    .expect("no store fault");
    assert_marks(&budget, t0, &[0, 100, 200, 300]);
    // An expired deadline is serviced even when the work is ready at once: the time is
    // +500 ms now, and the last drain (+300 ms) made the deadline +400 ms.
    tokio::time::advance(Duration::from_millis(120)).await;
    let output = StreamPump {
        store: &store,
        streams: &mut streams,
        projection_car_ids: &mut projection,
        pending: &mut pending,
        budget: &mut budget,
    }
    .while_waiting(&mut bed.scheduler, async { 5 })
    .await
    .expect("no store fault");
    assert_eq!(output, 5);
    assert_eq!(budget.marks.len(), 5, "the expired deadline was serviced first");
    assert_instant_near(
        *budget.marks.last().expect("a drain"),
        t0 + Duration::from_millis(500),
        "the drain of the ready wait",
    );
    // Repeated: the same wait again, with the interval just restarted, is not
    // drained, and one that begins late is drained at once and then every 100 ms.
    let output = StreamPump {
        store: &store,
        streams: &mut streams,
        projection_car_ids: &mut projection,
        pending: &mut pending,
        budget: &mut budget,
    }
    .while_waiting(&mut bed.scheduler, async { 6 })
    .await
    .expect("no store fault");
    assert_eq!(output, 6);
    assert_eq!(budget.marks.len(), 5, "no drain is due again yet");
    assert_eq!(manual.mono_now(), frozen, "the scheduler's clock never moved");
}

#[tokio::test(start_paused = true)]
async fn r56g_drive_requests_uses_the_previous_drain_deadline() {
    let mut bed = PumpBed::new();
    let due = bed.scheduler.due_vehicles(bed.t0);
    assert_eq!(due.len(), 3);
    let (mut streams, events) = idle_stream();
    let mut projection = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let store = bed.store.clone();
    let t0 = Instant::now();
    let mut budget = DrainBudget::new(t0);
    budget.mark();
    tokio::time::advance(Duration::from_millis(60)).await;
    {
        let mut run = bed.run();
        // One read is held 350 ms, beginning 60 ms into the interval.
        drive_requests(
            &mut run,
            vec![slow_read(1, Duration::from_millis(350))],
            Some(StreamPump {
                store: &store,
                streams: &mut streams,
                projection_car_ids: &mut projection,
                pending: &mut pending,
                budget: &mut budget,
            }),
            |run: &mut TickRun<'_>, key, result, now| {
                let (TickKey::Read(id), Ok(TickPayload::Data(snapshot))) = (key, result) else {
                    panic!("a read result");
                };
                run.scheduler
                    .vehicle_succeeded(id, poll_phase(&snapshot), true, now);
                Ok(ReadFlow::Continue)
            },
        )
        .await
        .expect("tick");
    }
    // 100 ms after each previous drain, not after the tick began.
    assert_marks(&budget, t0, &[0, 100, 200, 300, 400]);
    // The deadline has expired and a request is ready at once: the drain is serviced
    // first, so the handler already sees what the drain applied.
    events.send(StreamEvent::Healthy).await.expect("queued");
    tokio::time::advance(Duration::from_millis(250)).await;
    let marks_before = budget.marks.len();
    let seen_connected = Arc::new(AtomicBool::new(false));
    {
        let observer = Arc::clone(&seen_connected);
        let mut run = bed.run();
        let ready: TickFuture<'_> = Box::pin(async {
            (
                TickKey::Read(core_id(2)),
                Ok(TickPayload::Data(VehicleData::for_test(2, parked_fields()))),
            )
        });
        drive_requests(
            &mut run,
            vec![ready],
            Some(StreamPump {
                store: &store,
                streams: &mut streams,
                projection_car_ids: &mut projection,
                pending: &mut pending,
                budget: &mut budget,
            }),
            move |run: &mut TickRun<'_>, key, result, now| {
                let (TickKey::Read(id), Ok(TickPayload::Data(snapshot))) = (key, result) else {
                    panic!("a read result");
                };
                observer.store(
                    run.scheduler.vehicles[&core_id(1)].stream.connected,
                    Ordering::SeqCst,
                );
                run.scheduler
                    .vehicle_succeeded(id, poll_phase(&snapshot), true, now);
                Ok(ReadFlow::Continue)
            },
        )
        .await
        .expect("tick");
    }
    assert!(budget.marks.len() > marks_before, "the expired deadline was drained");
    assert!(
        seen_connected.load(Ordering::SeqCst),
        "the drain came before the ready request's result"
    );
}

#[tokio::test(start_paused = true)]
async fn r11_sleep_ends_by_the_remaining_drain_budget() {
    let t0 = Instant::now();
    let mut budget = DrainBudget::new(t0);
    budget.mark();
    tokio::time::advance(Duration::from_millis(60)).await;
    let remaining = budget.remaining(Instant::now());
    assert_eq!(remaining, Duration::from_millis(40));
    let hundred = Duration::from_millis(100);
    // Requested 100 ms with 60 ms elapsed: 40 ms.
    assert_eq!(drain_bounded_sleep(hundred, remaining, true), Duration::from_millis(40));
    // An expired deadline: nothing is slept.
    tokio::time::advance(Duration::from_millis(50)).await;
    let expired = budget.remaining(Instant::now());
    assert_eq!(expired, Duration::ZERO);
    assert_eq!(drain_bounded_sleep(hundred, expired, true), Duration::ZERO);
    // No stream: the planned wait is unchanged, whatever the budget says.
    assert_eq!(drain_bounded_sleep(hundred, expired, false), hundred);
    // A wait shorter than the budget is retained.
    assert_eq!(
        drain_bounded_sleep(Duration::from_millis(30), Duration::from_millis(40), true),
        Duration::from_millis(30)
    );
    // A backlog makes the next drain due at once.
    budget.mark();
    budget.note_backlog(true);
    assert_eq!(budget.remaining(Instant::now()), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn r11_the_iteration_sleep_ends_at_the_drain_deadline_and_the_next_drain_takes_the_event() {
    // The production sleep of an iteration, on a store-backed bed: the last drain was
    // at t0, a `Healthy` event is queued after it, the tail of the iteration takes 60
    // ms and the nominal sleep is the 100 ms drain cap. The sleep is incomplete at
    // +99 ms and complete at +100 ms (a timer is rounded up to its millisecond), the
    // event is untouched until then, and the drain that follows takes it. The sleep that
    // ignored the deadline would end at +160 ms.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(Arc::clone(&seam), async {
            let mut bed = PumpBed::new();
            let (mut streams, events) = idle_stream();
            let mut projection = HashMap::new();
            let mut pending = StreamDrainResult::default();
            let store = bed.store.clone();
            let t0 = Instant::now();
            let mut budget = DrainBudget::new(t0);
            budget.mark();
            events.send(StreamEvent::Healthy).await.expect("a queued event");
            tokio::time::advance(Duration::from_millis(60)).await;
            let healthy_taken = |seam: &SupervisedCollectorTestSeam| {
                seam.loop_log()
                    .iter()
                    .filter(|event| matches!(event, LoopEvent::HealthyProcessed(_)))
                    .count()
            };
            let mut sleeping = Box::pin(end_iteration_sleep(
                StreamPump {
                    store: &store,
                    streams: &mut streams,
                    projection_car_ids: &mut projection,
                    pending: &mut pending,
                    budget: &mut budget,
                },
                &mut bed.scheduler,
                K_STREAM_DRAIN_CAP,
                t0,
                false,
            ));
            assert!(poll_once(&mut sleeping).await.is_none(), "the sleep has begun");
            tokio::time::advance(Duration::from_millis(39)).await;
            assert!(poll_once(&mut sleeping).await.is_none(), "+99 ms: not complete");
            assert_eq!(healthy_taken(&seam), 0, "+99 ms: the event is not taken");
            tokio::time::advance(Duration::from_millis(2)).await;
            poll_once(&mut sleeping)
                .await
                .expect("+101 ms: the sleep is complete")
                .expect("no store fault");
            drop(sleeping);
            assert_eq!(healthy_taken(&seam), 0, "the sleep itself takes nothing");
            // The loop's next iteration drains first.
            drain_streams_now(&mut budget, &store, &mut bed.scheduler, &mut streams, &mut projection)
                .await
                .expect("the drain");
            assert_eq!(healthy_taken(&seam), 1, "the next drain takes the queued event");
            assert_instant_near(
                *budget.marks.last().expect("a drain"),
                t0 + Duration::from_millis(100),
                "the drain after the sleep",
            );

            // The same production sleep in the other cases of the helper: an expired
            // deadline sleeps nothing, a shorter planned wait is kept, and with no
            // stream the planned wait is unchanged.
            let began = Instant::now();
            budget.mark();
            tokio::time::advance(Duration::from_millis(150)).await;
            let mut expired = Box::pin(end_iteration_sleep(
                StreamPump {
                    store: &store,
                    streams: &mut streams,
                    projection_car_ids: &mut projection,
                    pending: &mut pending,
                    budget: &mut budget,
                },
                &mut bed.scheduler,
                K_STREAM_DRAIN_CAP,
                began,
                false,
            ));
            assert!(poll_once(&mut expired).await.expect("no sleep is owed").is_ok());
            drop(expired);
            budget.mark();
            let mut shorter = Box::pin(end_iteration_sleep(
                StreamPump {
                    store: &store,
                    streams: &mut streams,
                    projection_car_ids: &mut projection,
                    pending: &mut pending,
                    budget: &mut budget,
                },
                &mut bed.scheduler,
                Duration::from_millis(30),
                Instant::now(),
                false,
            ));
            assert!(poll_once(&mut shorter).await.is_none());
            tokio::time::advance(Duration::from_millis(29)).await;
            assert!(poll_once(&mut shorter).await.is_none(), "30 ms is kept, not 100");
            tokio::time::advance(Duration::from_millis(2)).await;
            assert!(poll_once(&mut shorter).await.expect("done").is_ok());
            drop(shorter);
            let mut no_streams: Vec<VehicleStreamRuntime> = Vec::new();
            let mut unarmed = Box::pin(end_iteration_sleep(
                StreamPump {
                    store: &store,
                    streams: &mut no_streams,
                    projection_car_ids: &mut projection,
                    pending: &mut pending,
                    budget: &mut budget,
                },
                &mut bed.scheduler,
                K_STREAM_DRAIN_CAP,
                Instant::now(),
                false,
            ));
            assert!(poll_once(&mut unarmed).await.is_none());
            tokio::time::advance(Duration::from_millis(99)).await;
            assert!(
                poll_once(&mut unarmed).await.is_none(),
                "without a stream the planned wait is the whole 100 ms"
            );
            tokio::time::advance(Duration::from_millis(2)).await;
            assert!(poll_once(&mut unarmed).await.expect("done").is_ok());
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn r11_the_shared_drain_deadline_preserves_the_empty_iteration_floor() {
    let mut bed = PumpBed::new();
    let (mut streams, _events) = idle_stream();
    let mut projection = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let store = bed.store.clone();
    // An empty iteration: it began at `t0`, drained at once, and its tail took 30 ms.
    // The sleep ends by the drain deadline, which is also the floor: 100 ms in all.
    let t0 = Instant::now();
    let mut budget = DrainBudget::new(t0);
    budget.mark();
    tokio::time::advance(Duration::from_millis(30)).await;
    end_iteration_sleep(
        StreamPump {
            store: &store,
            streams: &mut streams,
            projection_car_ids: &mut projection,
            pending: &mut pending,
            budget: &mut budget,
        },
        &mut bed.scheduler,
        K_LOOP_FLOOR,
        t0,
        true,
    )
    .await
    .expect("sleep");
    assert_instant_near(Instant::now(), t0 + K_LOOP_FLOOR, "the first empty iteration");
    // The next empty iteration begins here, but a drain happened 50 ms before it began
    // (the previous iteration's tail): the drain deadline is 50 ms away, earlier than
    // the floor. The iteration still lasts the floor, drained at its deadline.
    let started = Instant::now();
    let mut earlier = DrainBudget::new(started - Duration::from_millis(50));
    earlier.mark_at_for_test(started - Duration::from_millis(50));
    end_iteration_sleep(
        StreamPump {
            store: &store,
            streams: &mut streams,
            projection_car_ids: &mut projection,
            pending: &mut pending,
            budget: &mut earlier,
        },
        &mut bed.scheduler,
        K_LOOP_FLOOR,
        started,
        true,
    )
    .await
    .expect("sleep");
    assert_instant_near(
        Instant::now(),
        started + K_LOOP_FLOOR,
        "the next empty iteration is a floor after the previous start",
    );
    assert_eq!(earlier.marks.len(), 2, "drained at its deadline on the way: {:?}", earlier.marks);
    // An iteration that did work has no floor: only the drain deadline ends its sleep.
    let started = Instant::now();
    let mut worked = DrainBudget::new(started);
    worked.mark();
    end_iteration_sleep(
        StreamPump {
            store: &store,
            streams: &mut streams,
            projection_car_ids: &mut projection,
            pending: &mut pending,
            budget: &mut worked,
        },
        &mut bed.scheduler,
        Duration::from_millis(40),
        started,
        false,
    )
    .await
    .expect("sleep");
    assert_instant_near(Instant::now(), started + Duration::from_millis(40), "a worked iteration");
}

// ---- P2 (B2 fix round): what a run's last records do to its lease ------------------------------------

/// After a run that did not record its stop: the lease is retained with the heartbeat
/// `H` of its last renewal and an expiry that follows from it, and a later start that
/// is past the expiry and the minimum records exactly one downtime gap `[H, S]`.
fn assert_an_unrecorded_stop_recovers_with_one_downtime_gap(store: &HubStore, mode: &str) {
    let lease = store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the lease is retained to expire");
    let (heartbeat, expiry) = (lease.heartbeat_at_ms, lease.lease_until_ms);
    let hub_down = |store: &HubStore| -> Vec<(i64, Option<i64>)> {
        store
            .collection_gaps(50)
            .expect("gaps")
            .into_iter()
            .filter(|gap| gap.kind == "hub_down")
            .map(|gap| (gap.started_at_ms, gap.ended_at_ms))
            .collect()
    };
    assert!(hub_down(store).is_empty(), "the invalidated run wrote no replacement gap");
    // Until the expiry the singleton is still held: a second collector is refused.
    assert!(matches!(
        store.acquire_supervised_collector_lease(expiry - 1),
        Err(StoreError::SupervisedCollectorLeaseHeld)
    ));
    // A start past the expiry and the minimum takes the lease and reads the retained
    // heartbeat before it replaces it (R8).
    let start_ms = expiry + K_GAP_MIN_MS + 1_000;
    store
        .acquire_supervised_collector_lease(start_ms)
        .expect("the expired lease is replaced");
    let started = store
        .begin_collector_run(&crate::db::CollectorRunStartRequest {
            mode,
            config_fingerprint: &"0".repeat(64),
            start_ms,
            previous_heartbeat_ms: Some(heartbeat),
            gap_min_ms: K_GAP_MIN_MS,
            gap_alert_ms: 600_000,
        })
        .expect("start");
    assert!(started.hub_down_inserted && !started.stopped_gap_closed);
    assert_eq!(hub_down(store), vec![(heartbeat, Some(start_ms))], "one [H, S] gap");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_an_unrecorded_stop_preserves_the_actual_lease_heartbeat() {
    // A failed stop: the transaction that records it is made to abort, as a store
    // fault would. The run still ends and joins its renewal, and the lease is kept.
    let mut bed = FleetLoopBed::start("asleep", false, false).await;
    let lists_seen = Arc::clone(&bed.fake.lists);
    bed.wait_for("the first lists", || lists_seen.load(Ordering::SeqCst) >= 2).await;
    bed.store
        .open()
        .expect("catalogue")
        .execute_batch(
            "CREATE TRIGGER refuse_the_stop BEFORE INSERT ON collection_gaps
               WHEN NEW.kind = 'hub_stopped'
             BEGIN SELECT RAISE(ABORT, 'the stop is refused'); END;",
        )
        .expect("trigger");
    let account = |store: &HubStore| -> Option<(String, i64)> {
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT state, updated_at_ms FROM collector_account_state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()
    };
    let instance = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("active lease")
        .instance_id;
    let account_before = account(&bed.store);
    bed.shutdown.take().expect("shutdown").send(()).expect("shutdown");
    let mut task = bed.task.take().expect("task");
    join_supervised_restart_task("the run with a refused stop", &mut task)
        .await
        .expect("the run ends in order though its stop was refused");
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the lease is retained");
    assert_eq!(retained.instance_id, instance);
    // The renewal was joined: a whole heartbeat period later nothing has renewed it.
    sleep(SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL + Duration::from_millis(500)).await;
    let later = bed.store.supervised_collector_lease_status().expect("lease").expect("lease");
    assert_eq!(later.heartbeat_at_ms, retained.heartbeat_at_ms, "joined heartbeat");
    // Rolled back: no stop gap and no row of the stop transaction.
    assert!(
        bed.store
            .collection_gaps(50)
            .expect("gaps")
            .iter()
            .all(|gap| gap.kind != "hub_stopped"),
        "no stop was recorded"
    );
    assert_eq!(account(&bed.store), account_before, "the stop's rows rolled back");
    // Recovery: with the fault gone, a later start beyond the expiry records one gap.
    bed.store
        .open()
        .expect("catalogue")
        .execute_batch("DROP TRIGGER refuse_the_stop;")
        .expect("drop trigger");
    assert_an_unrecorded_stop_recovers_with_one_downtime_gap(&bed.store, "fleet_polling");
    bed.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_skipped_stop_preserves_the_actual_lease_heartbeat() {
    // A skipped stop: the retained admission is invalidated, so the run records
    // nothing at all. The same retention and recovery follow.
    let mut bed = FleetLoopBed::start("asleep", false, false).await;
    let lists_seen = Arc::clone(&bed.fake.lists);
    bed.wait_for("the first lists", || lists_seen.load(Ordering::SeqCst) >= 2).await;
    invalidate_the_retained_admission(bed.temporary.path());
    let mut task = bed.task.take().expect("task");
    let result = timeout(Duration::from_secs(10), &mut task)
        .await
        .expect("the run ends on the loss")
        .expect("join");
    assert!(matches!(result, Err(CollectorError::SensitiveAccessUnavailable)), "{result:?}");
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the lease is retained");
    sleep(SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL + Duration::from_millis(500)).await;
    let later = bed.store.collection_gaps(50).expect("gaps");
    assert!(later.iter().all(|gap| gap.kind != "hub_stopped"), "{later:?}");
    assert_eq!(
        bed.store
            .supervised_collector_lease_status()
            .expect("lease")
            .expect("lease")
            .heartbeat_at_ms,
        retained.heartbeat_at_ms,
        "no renewal after the denial"
    );
    assert_an_unrecorded_stop_recovers_with_one_downtime_gap(&bed.store, "fleet_polling");
    bed.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_recorded_ordinary_stop_releases_its_lease() {
    let mut bed = FleetLoopBed::start("asleep", false, false).await;
    let lists_seen = Arc::clone(&bed.fake.lists);
    bed.wait_for("the first lists", || lists_seen.load(Ordering::SeqCst) >= 2).await;
    bed.shutdown.take().expect("shutdown").send(()).expect("shutdown");
    let mut task = bed.task.take().expect("task");
    join_supervised_restart_task("the ordinary stop", &mut task)
        .await
        .expect("an ordinary result");
    assert!(
        bed.store
            .supervised_collector_lease_status()
            .expect("lease")
            .is_none(),
        "a recorded stop releases the lease"
    );
    let stops: Vec<_> = bed
        .store
        .collection_gaps(50)
        .expect("gaps")
        .into_iter()
        .filter(|gap| gap.kind == "hub_stopped")
        .collect();
    assert_eq!(stops.len(), 1, "one stop: {stops:?}");
    assert!(stops[0].ended_at_ms.is_none(), "it stays open until the next start");
    // The next start closes it and records no downtime besides: the released lease
    // leaves no heartbeat to measure a silence from.
    let started = bed
        .store
        .begin_collector_run(&crate::db::CollectorRunStartRequest {
            mode: "fleet_polling",
            config_fingerprint: &"0".repeat(64),
            start_ms: current_epoch_millis().expect("clock") + 1_000,
            previous_heartbeat_ms: None,
            gap_min_ms: K_GAP_MIN_MS,
            gap_alert_ms: 600_000,
        })
        .expect("start");
    assert!(started.stopped_gap_closed && !started.hub_down_inserted);
    let gaps = bed.store.collection_gaps(50).expect("gaps");
    assert!(gaps.iter().all(|gap| gap.kind != "hub_down"), "{gaps:?}");
    assert!(
        gaps.iter()
            .filter(|gap| gap.kind == "hub_stopped")
            .all(|gap| gap.ended_at_ms.is_some()),
        "{gaps:?}"
    );
    bed.server.abort();
}

// ---- P2 (B2 fix round): a returned publication-gate error (C3-S9) ------------------------------------

#[tokio::test]
async fn c3_s9_publication_gate_error_rolls_back_tick_and_backs_off() {
    // The tick's publication gate fails to open (`.publication.lock` is a directory, so
    // the acquisition returns an error at once): the tick is `Failed` through
    // `commit_failed`, nothing durable is written, the pending transitions are dropped
    // so the work stays due, and the account waits for the store. This proves the
    // returned-error branch only; it sets no deadline for ordinary gate contention.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "online")]).await;
            let id = core_id(1);
            let now = bed.now();
            bed.scheduler.begin_tick();
            let baseline_phase = bed.scheduler.vehicles[&id].phase;
            let baseline_due = bed.scheduler.vehicles[&id].next_read_due;
            assert_eq!(bed.scheduler.due_vehicles(now), vec![id]);
            // The read and a discovery arrive: transitions pending in memory.
            let pending_batch = |bed: &TickBed| {
                let mut vehicle = bed.scheduler.vehicles[&id].vehicle.clone();
                vehicle.state = "waking".to_owned();
                TickBatch {
                    discoveries: vec![DiscoveryItem {
                        vehicle,
                        received_at_ms: bed.wall(),
                    }],
                    snapshots: vec![bed.fresh_read_item(bed.wall())],
                    first_result_at: Some(bed.now()),
                    ..TickBatch::default()
                }
            };
            bed.scheduler
                .vehicle_succeeded(id, PollPhase::Driving, false, now);
            assert_ne!(bed.scheduler.vehicles[&id].phase, baseline_phase, "a pending transition");
            let batch = pending_batch(&bed);
            let gate = bed._dir.path().join(".publication.lock");
            let _ = std::fs::remove_file(&gate);
            std::fs::create_dir(&gate).expect("a directory where the gate file belongs");
            let attempts = seam.tick_commit_attempts.load(Ordering::SeqCst);
            let commits = seam.tick_commits();
            let rows_before = collector_rows_stamp(&bed.store);
            let outcome = bed.commit(batch).await;
            assert!(matches!(outcome, TickOutcome::Failed), "{outcome:?}");
            assert_eq!(
                seam.tick_commit_attempts.load(Ordering::SeqCst),
                attempts,
                "the failure was the gate's returned error: no transaction started"
            );
            assert_eq!(seam.tick_commits(), commits);
            assert_eq!(
                collector_rows_stamp(&bed.store),
                rows_before,
                "no durable tick write"
            );
            // The pending transitions are dropped, so the work stays due.
            assert_eq!(bed.scheduler.vehicles[&id].phase, baseline_phase);
            assert_eq!(bed.scheduler.vehicles[&id].next_read_due, baseline_due);
            // The account backs off for the store: nothing is sent until then.
            assert_eq!(bed.scheduler.account.store_failures, 1);
            let (until, reason) = bed.scheduler.account.wait.expect("a store wait");
            assert_eq!(reason, WaitReason::Store);
            assert!(bed.scheduler.due_vehicles(bed.now()).is_empty(), "held by the wait");
            // After the backoff the same read is due again.
            bed.advance(until.saturating_duration_since(bed.now()) + Duration::from_millis(1));
            let due_again = bed.scheduler.due_vehicles(bed.now());
            assert_eq!(due_again, vec![id], "the work stayed due");
            bed.scheduler.release_in_flight();
            // The gate is repaired: one bounded retry commits once.
            std::fs::remove_dir(&gate).expect("repair the gate");
            bed.scheduler.begin_tick();
            let _ = bed.scheduler.due_vehicles(bed.now());
            bed.scheduler
                .vehicle_succeeded(id, PollPhase::Driving, false, bed.now());
            let retry = pending_batch(&bed);
            let outcome = bed.commit(retry).await;
            assert!(
                matches!(outcome, TickOutcome::Committed { wrote: true, .. }),
                "{outcome:?}"
            );
            assert_eq!(seam.tick_commits(), commits + 1, "committed once");
            assert_eq!(bed.scheduler.account.store_failures, 0, "the store recovered");
        })
        .await;
}
