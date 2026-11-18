// SPDX-License-Identifier: AGPL-3.0-only

// The B2 fix round's tests of the Fleet request counter at the tick (C3 R25, R27,
// R47): a credential stop cancels the requests queued behind it, taking back the
// starts they were admitted with. They share the Fleet bed of `request_counter.rs`.
// Synthetic data only.

/// What the in-process transport of a [`FleetBed`] records and injects (R-P3bF4-1).
/// Every request is handed to the double in the caller's task and awaited, so which
/// requests arrive is a function of the code under test and of nothing else.
#[derive(Clone, Default)]
struct FleetTransportControl {
    /// One line per request the client dispatched, in order, naming the car it was
    /// for: the request, then how it ended (the status the handler answered, or the
    /// transport error injected before the handler).
    attempts: Arc<Mutex<Vec<String>>>,
    /// Transport errors to inject once, before the handler is entered: the first
    /// request whose path contains the key fails with the error, which the real
    /// client would have reported for a request that never reached the server.
    failures: Arc<Mutex<Vec<(String, FleetApiError)>>>,
}

impl FleetTransportControl {
    /// The next request whose path contains `path_part` fails before any handler sees
    /// it, with `error`.
    fn fail_next_before_the_handler(&self, path_part: &str, error: FleetApiError) {
        self.failures
            .lock()
            .expect("failures")
            .push((path_part.to_owned(), error));
    }

    /// The per-car attempt trace, for an assertion message.
    fn trace(&self) -> Vec<String> {
        self.attempts.lock().expect("attempts").clone()
    }

    /// The transport the Fleet clients of a bed use: it answers through `router`.
    fn transport(&self, router: Router) -> crate::fleet_api::in_process::InProcessTransport {
        let control = self.clone();
        crate::fleet_api::in_process::InProcessTransport::new(move |request| {
            let control = control.clone();
            let router = router.clone();
            async move {
                let line = format!("{} {}", request.method(), request.uri().path());
                let failure = {
                    let mut failures = control.failures.lock().expect("failures");
                    failures
                        .iter()
                        .position(|(part, _)| line.contains(part.as_str()))
                        .map(|index| failures.remove(index).1)
                };
                if let Some(error) = failure {
                    control
                        .attempts
                        .lock()
                        .expect("attempts")
                        .push(format!("{line} -> {error:?} before the handler"));
                    return Err(error);
                }
                let (parts, body) = request.into_parts();
                let response = tower::ServiceExt::oneshot(
                    router,
                    http::Request::from_parts(parts, axum::body::Body::from(body)),
                )
                .await
                .expect("the router cannot fail");
                let (parts, body) = response.into_parts();
                let body = axum::body::to_bytes(body, usize::MAX)
                    .await
                    .expect("the fake's body")
                    .to_vec();
                control
                    .attempts
                    .lock()
                    .expect("attempts")
                    .push(format!("{line} -> {}", parts.status.as_u16()));
                Ok(http::Response::from_parts(parts, body))
            }
        })
    }
}

/// The Data counts of a ledger now: the category's count and the raw starts since
/// the process began.
fn ledger_data_counts(ledger: &RequestLedger) -> (CategoryCount, u64) {
    let state = ledger.lock();
    (
        state.counts[RequestCategory::Data.index()],
        state.started_since_start[RequestCategory::Data.index()],
    )
}

impl FleetBed {
    /// Commit a batch as the loop's tick transaction does, on the bed's clocks.
    async fn commit(&mut self, batch: TickBatch) -> TickOutcome {
        let now = self.clock.mono_now();
        let commit = TickCommit {
            store: &self.store,
            cursor_key: &self.cursor_key,
            provider: CollectorProvider::Fleet,
            offline_drive_timeout: Duration::from_secs(600),
        };
        commit_tick(
            &commit,
            &mut self.identities,
            &mut self.scheduler,
            batch,
            now,
        )
        .await
        .expect("commit tick")
    }

    /// The Data counts of the ledger now: the category's count and the raw starts
    /// since the process began.
    fn data_counts(&self) -> (CategoryCount, u64) {
        ledger_data_counts(&self.scheduler.request_ledger())
    }

    fn open_auth_terminal_gaps(&self) -> usize {
        self.store
            .collection_gaps(10)
            .expect("gaps")
            .iter()
            .filter(|gap| gap.kind == "auth_terminal" && gap.ended_at_ms.is_none())
            .count()
    }

    /// The owner's retry lifts the stop: every car is due again, none is in flight and
    /// none counted a failure.
    fn assert_every_car_is_due_and_unfailed_after_a_retry(&mut self) {
        let later = self.clock.mono_now();
        self.scheduler.clear_credential_stop(later);
        for id in 1..=3 {
            let record = &self.scheduler.vehicles[&core_id(id)];
            assert!(!record.in_flight, "car {id} is not in flight");
            assert_eq!(record.consecutive_failures, 0, "car {id} counted no failure");
        }
        let mut due = self.scheduler.due_vehicles(later);
        due.sort();
        assert_eq!(
            due,
            vec![core_id(1), core_id(2), core_id(3)],
            "the queued cars are due, and so is the one that met the stop"
        );
        // Asking marks the cars in flight; nothing was sent, so give the marks back.
        self.scheduler.release_in_flight();
    }
}

#[tokio::test]
async fn ct67c_a_read_refused_with_the_refreshed_token_is_a_credential_stop_with_no_second_refresh() {
    // The resent read gets 401 with token B: no second token request in the tick;
    // the outcome is the Fleet credential stop (C3 R47): `CredentialStop`, one
    // `auth_terminal` gap, no failure counted for any car. The stop cancels the reads
    // still queued behind the manager (B2 clarification 3): they are never sent and
    // never counted, their starts are taken back, and they stay due. A later retry
    // reads every car.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed =
                FleetBed::with_cars(k7_policy(1_000, crate::config::FLEET_PRICING), "online", 3)
                    .await;
            bed.warm_up_until_a_resend_is_covered().await;
            bed.fake.reject_every_token.store(true, Ordering::SeqCst);
            bed.advance(Duration::from_secs(120));
            let reads_before = bed.reads();
            let lists_before = bed.lists();
            let (counts_before, starts_before) = bed.data_counts();
            bed.try_tick().await.expect("the loop goes on");
            assert_eq!(
                bed.fake.token_requests.load(Ordering::SeqCst),
                1,
                "no second token request in the tick"
            );
            // The first read twice (A then B): two reads reached the wire, and the two
            // queued ones never did.
            assert_eq!(bed.reads() - reads_before, 2);
            assert_eq!(bed.lists(), lists_before, "the list was not due");
            let (counts_after, starts_after) = bed.data_counts();
            assert_eq!(counts_after.sent - counts_before.sent, 2, "Data delta 2");
            assert_eq!(starts_after - starts_before, 2, "the raw starts are the two sends");
            assert_eq!(counts_after.in_flight, 0, "no start is left open");
            assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
            assert_eq!(bed.open_auth_terminal_gaps(), 1, "one auth_terminal gap");
            assert!(bed.violations().is_empty(), "{:?}", bed.violations());
            // Nothing more is sent while the account is stopped, though the cars are due.
            let before = (bed.lists(), bed.reads());
            bed.tick().await;
            assert_eq!((bed.lists(), bed.reads()), before);
            // The flags are clear and the queued cars are due; the retry reads all.
            bed.assert_every_car_is_due_and_unfailed_after_a_retry();
            bed.fake.reject_every_token.store(false, Ordering::SeqCst);
            let reads_before = bed.reads();
            bed.tick().await;
            assert_eq!(bed.reads() - reads_before, 3, "the retry reads every car");
        })
        .await;
}

#[tokio::test]
async fn ct67c_a_self_refreshed_token_stop_cancels_queued_reads() {
    // The list is not due and the token's refresh is: the first read's own start
    // refreshes it, carries token B and is refused, which is a stop without a resend.
    // The two reads queued behind it are cancelled: no traffic, no count, no raw
    // start, and the cars stay due.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed =
                FleetBed::with_cars(k7_policy(1_000, crate::config::FLEET_PRICING), "online", 3)
                    .await;
            bed.warm_up_until_a_resend_is_covered().await;
            bed.fake.reject_every_token.store(true, Ordering::SeqCst);
            bed.manager.lock().await.mark_refresh_due();
            bed.advance(Duration::from_secs(120));
            assert!(
                !bed.scheduler.discovery_due(bed.clock.mono_now()),
                "the list is not due"
            );
            let reads_before = bed.reads();
            let lists_before = bed.lists();
            let (counts_before, starts_before) = bed.data_counts();
            bed.try_tick().await.expect("the loop goes on");
            assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 1, "one refresh");
            assert_eq!(bed.reads() - reads_before, 1, "one read, no resend, nothing queued");
            assert_eq!(bed.lists(), lists_before);
            let (counts_after, starts_after) = bed.data_counts();
            assert_eq!(counts_after.sent - counts_before.sent, 1);
            assert_eq!(starts_after - starts_before, 1, "raw starts: the one read");
            assert_eq!(counts_after.in_flight, 0);
            assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
            assert_eq!(bed.open_auth_terminal_gaps(), 1);
            bed.assert_every_car_is_due_and_unfailed_after_a_retry();
        })
        .await;
}

/// A future that counts its polls and otherwise is the future it wraps.
struct PollWitness<'a> {
    inner: TickFuture<'a>,
    polls: Arc<AtomicUsize>,
}

impl Future for PollWitness<'_> {
    type Output = (TickKey, Result<TickPayload, CollectorError>);

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.inner.as_mut().poll(context)
    }
}

fn polled_through<'a>(inner: TickFuture<'a>, polls: &Arc<AtomicUsize>) -> TickFuture<'a> {
    Box::pin(PollWitness {
        inner,
        polls: Arc::clone(polls),
    })
}

#[tokio::test]
async fn r47_cancelling_a_never_polled_fleet_request_releases_its_admission() {
    // The loop admits a request (the ledger counts its start) and takes its guard when
    // it builds the future, in the production probe and read factories. A credential
    // stop that arrives before the queued request is ever polled drops it: nothing
    // reaches the wire, the start is taken back, the persisted count never held it and
    // the car stays due with no flag and no failure.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed =
                FleetBed::with_cars(k7_policy(1_000, crate::config::FLEET_PRICING), "online", 3)
                    .await;
            bed.allow_refresh = true;
            bed.tick().await;
            bed.advance(Duration::from_secs(120));
            let configured = bed.store.configured_tesla_vehicles().expect("configured");
            let now = bed.clock.mono_now();
            bed.scheduler.apply_control_settings(&configured, now);
            bed.scheduler.observe_clock(now);
            let ledger = bed.scheduler.request_ledger();
            let tick_refresh = TickRefresh::default();
            let (counts_before, starts_before) = bed.data_counts();
            let persisted_before = bed.row("data").expect("a stored count").sent;
            let reads_before = bed.reads();
            let env = FleetTickEnv {
                store: &bed.store,
                cursor_key: &bed.cursor_key,
                terrain_wake: &bed.terrain,
                offline_drive_timeout: Duration::from_secs(600),
                api: &bed.api,
                auth_api: &bed.auth_api,
                manager: &bed.manager,
                ledger: &ledger,
                allow_refresh: true,
                collector_state: &bed.collector_state,
                configured: &configured,
            };
            bed.scheduler.begin_tick();
            let mut due = bed.scheduler.due_vehicles(now);
            due.sort();
            assert_eq!(due, vec![core_id(1), core_id(2), core_id(3)]);
            let mut run = TickRun {
                scheduler: &mut bed.scheduler,
                identities: &mut bed.identities,
                commit: TickCommit {
                    store: &bed.store,
                    cursor_key: &bed.cursor_key,
                    provider: CollectorProvider::Fleet,
                    offline_drive_timeout: Duration::from_secs(600),
                },
                batch: TickBatch::default(),
                terrain_wake: &bed.terrain,
                any_wrote: false,
            };
            reserve_for(&mut run, &[(RequestCategory::Data, 2)])
                .await
                .expect("reserve");
            // The two queued requests are admitted as the loop admits them.
            for _ in 0..2 {
                assert_eq!(
                    run.scheduler.admit_request(RequestCategory::Data, now),
                    Admission::Admitted
                );
            }
            assert_eq!(ledger_data_counts(&ledger).1 - starts_before, 2);
            let queued_polls = Arc::new(AtomicUsize::new(0));
            let read_car = run.scheduler.vehicle_record(core_id(2)).expect("car 2");
            let probe_car = run.scheduler.vehicle_record(core_id(3)).expect("car 3");
            let read_log = RequestLog::for_vehicle(run.scheduler, core_id(2));
            let probe_log = RequestLog::for_vehicle(run.scheduler, core_id(3));
            let requests: Vec<TickFuture<'_>> = vec![
                // A credential stop that is ready before anything is polled.
                Box::pin(async {
                    (
                        TickKey::Read(core_id(1)),
                        Err(CollectorError::FleetApi(FleetApiError::HttpStatus(401))),
                    )
                }),
                polled_through(
                    fleet_read_future(read_log, &env, read_car, core_id(2), &tick_refresh),
                    &queued_polls,
                ),
                polled_through(
                    fleet_probe_future(
                        probe_log,
                        &env,
                        probe_car,
                        core_id(3),
                        ProbeKind::Read,
                        &tick_refresh,
                    ),
                    &queued_polls,
                ),
            ];
            let flow = drive_requests(
                &mut run,
                requests,
                None,
                |run: &mut TickRun<'_>, key, result, now| {
                    assert!(matches!(key, TickKey::Read(id) if id == core_id(1)));
                    assert!(result.as_ref().is_err_and(fleet_credential_stop));
                    run.scheduler.enter_credential_stop(now);
                    run.scheduler.request_withheld(core_id(1));
                    Ok(ReadFlow::SignedOut)
                },
            )
            .await
            .expect("tick");
            assert_eq!(flow, ReadFlow::SignedOut);
            drop(run);
            assert_eq!(queued_polls.load(Ordering::SeqCst), 0, "the queued requests were never polled");
            assert_eq!(bed.reads(), reads_before, "nothing reached the wire");
            let (counts_after, starts_after) = bed.data_counts();
            assert_eq!(starts_after, starts_before, "the raw starts are restored");
            assert_eq!(counts_after.sent, counts_before.sent);
            assert_eq!(counts_after.in_flight, 0);
            assert_eq!(
                bed.row("data").expect("a stored count").sent,
                persisted_before,
                "the persisted count never held the cancelled requests"
            );
            bed.assert_every_car_is_due_and_unfailed_after_a_retry();
        })
        .await;
}


#[tokio::test]
async fn ct67c_a_failed_stop_commit_keeps_fleet_stopped_without_queued_sends() {
    // The tick's resent read is refused with the refreshed token, which is a Fleet
    // credential stop, and the commit that would store the stop fails. The stop is
    // derived in memory and never stored (R4), so the rollback leaves it standing: the
    // account sends nothing again after the store's back-off, however long the fault
    // lasts, until the owner's retry. The durable gap that went with it rolled back and
    // is stored, with its original onset, by the first commit that works.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let witness = Arc::clone(&seam);
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async move {
            let mut bed =
                FleetBed::with_cars(k7_policy(1_000, crate::config::FLEET_PRICING), "online", 3)
                    .await;
            bed.warm_up_until_a_resend_is_covered().await;
            bed.fake.reject_every_token.store(true, Ordering::SeqCst);
            bed.advance(Duration::from_secs(120));
            let reads_before = bed.reads();
            let lists_before = bed.lists();
            let (counts_before, starts_before) = bed.data_counts();
            // The store fails the commit that follows the refused read.
            witness.set_tick_commit_fault(true);
            bed.try_tick().await.expect("the loop goes on");
            assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 1, "one refresh");
            assert_eq!(bed.reads() - reads_before, 2, "the read and its one resend");
            let (counts_after, starts_after) = bed.data_counts();
            assert_eq!(counts_after.sent - counts_before.sent, 2);
            assert_eq!(starts_after - starts_before, 2, "no queued request was started");
            assert_eq!(counts_after.in_flight, 0);
            assert_eq!(
                bed.scheduler.account.state,
                AccountState::CredentialStop,
                "the failed commit does not undo the stop"
            );
            let onset = bed.scheduler.account.state_since_ms;
            assert_eq!(bed.open_auth_terminal_gaps(), 0, "the gap rolled back");
            // The fault lasts: the store's back-off ends and a long time passes, and the
            // stopped account sends nothing of any kind.
            let wire = (
                bed.lists(),
                bed.reads(),
                bed.fake.token_requests.load(Ordering::SeqCst),
            );
            for _ in 0..12 {
                bed.advance(Duration::from_secs(300));
                bed.tick().await;
                assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
            }
            assert_eq!(
                (bed.lists(), bed.reads(), bed.fake.token_requests.load(Ordering::SeqCst)),
                wire,
                "no traffic after the back-off"
            );
            assert_eq!(lists_before, bed.lists(), "the list was not due before and sent none");
            // The store is repaired: the next commit stores the gap and the counts.
            witness.set_tick_commit_fault(false);
            bed.advance(Duration::from_secs(300));
            bed.tick().await;
            assert_eq!(bed.open_auth_terminal_gaps(), 1, "the gap is stored once");
            let gap = bed
                .store
                .collection_gaps(10)
                .expect("gaps")
                .into_iter()
                .find(|gap| gap.kind == "auth_terminal")
                .expect("the gap");
            assert_eq!(gap.started_at_ms, onset, "the gap begins when the stop was entered");
            assert_eq!(
                bed.row("data").expect("data").sent,
                i64::try_from(counts_after.sent).expect("count"),
                "the counts of the failed tick are stored"
            );
            assert_eq!(
                (bed.lists(), bed.reads(), bed.fake.token_requests.load(Ordering::SeqCst)),
                wire,
                "still no traffic: only the owner's retry lifts the stop"
            );
            assert!(bed.violations().is_empty(), "{:?}", bed.violations());
            // No car is in flight and none counted a failure: the retry starts clean.
            let later = bed.clock.mono_now();
            bed.scheduler.clear_credential_stop(later);
            for id in 1..=3 {
                let record = &bed.scheduler.vehicles[&core_id(id)];
                assert!(!record.in_flight, "car {id} is not in flight");
                assert_eq!(record.consecutive_failures, 0, "car {id} counted no failure");
            }
        })
        .await;
}

#[tokio::test]
async fn ct67b_a_read_lost_before_the_handler_is_one_unreachable_car_with_no_failure_counted() {
    // The control of the ct67b_a flake (R-P3bF4-1). The old bed served a socket with a
    // five-second client deadline, and under load one of the three first-contact reads
    // could end before the double's handler was entered: the double then counted two
    // arrivals, the car met a transport error (an account-level `Unreachable`, which
    // counts no car failure), and the tick still returned. The bed is in process now,
    // so that outcome no longer depends on the machine; this test causes it on purpose,
    // for one chosen car, and states exactly what the Hub does with it: two reads
    // arrive, three are accounted as sent, no start is left open, no car counts a
    // failure and the chosen car's last error is `Unreachable`.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed =
                FleetBed::with_cars(k7_policy(1_000, crate::config::FLEET_PRICING), "online", 3)
                    .await;
            bed.allow_refresh = true;
            bed.fake.token_expired.store(true, Ordering::SeqCst);
            // The first tick: list 401, refresh, resend withheld (as in ct67b_a).
            let outcome = bed.try_tick().await.expect("no error: nothing failed");
            assert!(outcome.requests_sent);
            assert_eq!(bed.lists(), 1, "the resend was not sent");
            let (counts_before, _) = bed.data_counts();
            // Car 2's read is lost before any handler sees it.
            bed.fake.transport.fail_next_before_the_handler(
                "KF000002/vehicle_data",
                FleetApiError::RequestTimeout,
            );
            let outcome = bed.try_tick().await.expect("the tick returns");
            assert!(outcome.requests_sent);
            assert_eq!(bed.lists(), 2);
            let trace = bed.fake.transport.trace();
            assert_eq!(bed.reads(), 2, "two reads arrived: {trace:#?}");
            assert!(
                trace.iter().any(|line| line.contains("KF000002/vehicle_data")
                    && line.contains("RequestTimeout before the handler")),
                "{trace:#?}"
            );
            let (counts_after, _) = bed.data_counts();
            assert_eq!(counts_after.sent - counts_before.sent, 3, "three reads were sent");
            assert_eq!(counts_after.in_flight, 0, "no start is left open");
            let failures: Vec<_> = (1..=3)
                .map(|id| bed.scheduler.vehicles[&core_id(id)].consecutive_failures)
                .collect();
            assert_eq!(failures, [0, 0, 0], "a transport error counts no car failure");
            assert_eq!(
                bed.scheduler.vehicles[&core_id(2)].last_error,
                Some(ErrorClass::Unreachable)
            );
            assert!(bed.violations().is_empty(), "{:?}", bed.violations());
        })
        .await;
}
