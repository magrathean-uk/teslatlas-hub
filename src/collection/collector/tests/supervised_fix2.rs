// SPDX-License-Identifier: AGPL-3.0-only

// Tests of the second fix round of B2 part P2 (collector timing, cancellation and
// shutdown propagation): the supervisors' shutdown against a retained admission that is
// denied at an exact, acknowledged point. Every hold is an acknowledged gate of the test
// seam (the task that reaches it says so, and goes on only when the test releases it); no
// case waits for a duration to elapse, so a loaded machine cannot change what one proves.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_denial_during_the_heartbeat_join_retains_the_lease() {
    // The renewal's shutdown arm does not look at the admission, so the admission can be
    // denied while the finaliser waits for the join and the heartbeat still returns
    // `Ok`. The revalidation before the release must come after the join: a denial that
    // arrives during it retains the lease and is the run's result. The same held join
    // with the admission intact releases it.
    for denied in [true, false] {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let admission =
            crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
        let lease = store
            .acquire_supervised_collector_lease(current_epoch_millis().expect("clock"))
            .expect("collector lease");
        let initial = store
            .supervised_collector_lease_status()
            .expect("lease query")
            .expect("active lease");
        let (heartbeat_shutdown, shutdown_received) = oneshot::channel::<()>();
        let (received_tx, received_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        // The renewal of this case does not look at the admission: it acknowledges the
        // shutdown it was sent, is held, and then returns `Ok`.
        let mut heartbeat = tokio::spawn(async move {
            let _ = shutdown_received.await;
            let _ = received_tx.send(());
            let _ = release_rx.await;
            Ok::<(), CollectorError>(())
        });
        let finish_store = store.clone();
        let finish_admission = Arc::clone(&admission);
        let mut finish = tokio::spawn(async move {
            finish_supervised_collector(
                &finish_store,
                lease,
                Ok(()),
                &mut heartbeat,
                Some(heartbeat_shutdown),
                false,
                StopRecord::Recorded,
                AdmissionLoss::default(),
                Some(&*finish_admission),
            )
            .await
        });
        // The renewal has been sent its shutdown: the checks before the join have
        // passed and the join is pending.
        timeout(SUPERVISED_RESTART_WAIT, received_rx)
            .await
            .expect("the join begins")
            .expect("the shutdown acknowledgement");
        assert!(!finish.is_finished(), "the finaliser waits for the join");
        if denied {
            swap_the_admission_lock_away(temporary.path());
        }
        release_tx.send(()).expect("release the held renewal");
        let result = join_supervised_restart_task("the finaliser", &mut finish).await;
        if denied {
            assert!(
                matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
                "a denial during the join is the run's result: {result:?}"
            );
            let retained = store
                .supervised_collector_lease_status()
                .expect("lease query")
                .expect("the lease is retained to expire");
            assert_eq!(retained.instance_id, initial.instance_id);
        } else {
            result.expect("an intact admission releases");
            assert!(
                store
                    .supervised_collector_lease_status()
                    .expect("lease query")
                    .is_none(),
                "the same held join releases the lease when the admission is intact"
            );
        }
    }
}

/// The entries into the run's last transaction a bed's seam counted.
fn count_final_transactions(seam: &SupervisedCollectorTestSeam) -> Arc<AtomicUsize> {
    let entries = Arc::new(AtomicUsize::new(0));
    let witness = Arc::clone(&entries);
    seam.set_finish_entry(Arc::new(move || {
        witness.fetch_add(1, Ordering::SeqCst);
    }));
    entries
}

fn hub_stopped_gaps(store: &HubStore) -> usize {
    store
        .collection_gaps(50)
        .expect("gaps")
        .iter()
        .filter(|gap| gap.kind == "hub_stopped")
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_owner_renewal_loss_survives_recovery_when_shutdown_wins() {
    // The renewal meets the denial (and sets the shared fence), the admission then
    // recovers (the lock file is put back), and ordinary shutdown wins the supervisor's
    // wait over the renewal's result. The supervisor's own revalidation passes again,
    // yet the loss the renewal met stays decisive: no last transaction, no replacement
    // gap, the result is the sensitive failure and the lease is retained. The order of
    // everything is acknowledged: the supervisor's wait and the renewal's candidate are
    // held, the renewal's denial is observed in the seam, and the wait that ended is
    // logged.
    let mut held = None;
    let mut finishes = None;
    let mut bed = DrainBed::start_with_admission(1, true, |_, seam| {
        held = Some((
            seam.arm_gate("supervisor-select"),
            seam.arm_gate("renewal-candidate"),
        ));
        finishes = Some(count_final_transactions(seam));
        // The credential unit shares the observed loss (B2 fix round, P3b fix 1), so a
        // loop that is polled meets it at once and ends first. The loop is held here,
        // pending, so that the shutdown, which is ready, is the arm that wins.
        let _ = seam.arm_gate("iteration-top");
    })
    .await;
    let (select, renewal) = held.expect("gates");
    let finishes = finishes.expect("witness");
    let seam = Arc::clone(&bed.seam);
    select.arrived("the supervisor's wait", 1).await;
    renewal.arrived("the renewal's candidate", 1).await;
    let initial = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("active lease");
    swap_the_admission_lock_away(bed.temporary.path());
    renewal.release(1);
    bed.wait_for("the renewal's own denial", || {
        seam.loop_log().contains(&LoopEvent::RenewalDenied)
    })
    .await;
    restore_the_admission_lock(bed.temporary.path());
    let admission = bed.admission.clone().expect("the retained admission");
    assert!(
        assert_runtime_sensitive_access(Some(&*admission)).is_ok(),
        "the admission passes again"
    );
    // Ordinary shutdown is ready before the wait is entered, so it wins over the
    // renewal's result (the wait is biased towards it).
    let _ = bed.shutdown.take().expect("shutdown").send(());
    select.open();
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the supervisor ends", &mut task).await;
    bed.server.abort();
    let log = seam.loop_log();
    assert!(
        log.contains(&LoopEvent::SupervisorSelected { by_renewal: false, ok: true }),
        "ordinary shutdown won the wait: {log:?}"
    );
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    assert_eq!(finishes.load(Ordering::SeqCst), 0, "no last transaction after the renewal's loss");
    assert_eq!(hub_stopped_gaps(&bed.store), 0, "no replacement gap");
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease is retained to expire");
    assert_eq!(retained.instance_id, initial.instance_id);
    assert_eq!(retained.heartbeat_at_ms, initial.heartbeat_at_ms, "nothing renewed it");
    assert!(
        log.contains(&LoopEvent::RenewalJoined) || log.contains(&LoopEvent::RenewalEnded),
        "the renewal is over: {log:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_fleet_renewal_loss_survives_recovery_when_shutdown_wins() {
    // The same as the Owner's case, on the Fleet supervisor: after the renewal's loss and
    // the admission's recovery an ordinary shutdown writes no last transaction and
    // settles no token exchange (the settlement is skipped after the shared loss).
    let mut held = None;
    let mut finishes = None;
    let mut bed = FleetLoopBed::start_with_seam("asleep", false, false, Duration::from_secs(2), |seam| {
        held = Some((
            seam.arm_gate("supervisor-select"),
            seam.arm_gate("renewal-candidate"),
        ));
        finishes = Some(count_final_transactions(seam));
        // The manager shares the observed loss (B2 fix round, P3b fix 1), so a loop that
        // is polled meets it at once and ends first. The loop is held here, pending, so
        // that the shutdown, which is ready, is the arm that wins.
        let _ = seam.arm_gate("iteration-top");
    })
    .await;
    let (select, renewal) = held.expect("gates");
    let finishes = finishes.expect("witness");
    let seam = Arc::clone(&bed.seam);
    select.arrived("the supervisor's wait", 1).await;
    renewal.arrived("the renewal's candidate", 1).await;
    let initial = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("active lease");
    swap_the_admission_lock_away(bed.temporary.path());
    renewal.release(1);
    bed.wait_for("the renewal's own denial", || {
        seam.loop_log().contains(&LoopEvent::RenewalDenied)
    })
    .await;
    restore_the_admission_lock(bed.temporary.path());
    assert!(
        assert_runtime_sensitive_access(Some(&*bed.admission)).is_ok(),
        "the admission passes again"
    );
    let _ = bed.shutdown.take().expect("shutdown").send(());
    select.open();
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the Fleet supervisor ends", &mut task).await;
    bed.server.abort();
    let log = seam.loop_log();
    assert!(
        log.contains(&LoopEvent::SupervisorSelected { by_renewal: false, ok: true }),
        "ordinary shutdown won the wait: {log:?}"
    );
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    assert_eq!(finishes.load(Ordering::SeqCst), 0, "no last transaction after the renewal's loss");
    assert_eq!(hub_stopped_gaps(&bed.store), 0, "no replacement gap");
    assert!(
        !log.contains(&LoopEvent::FleetSettlementBegun),
        "the settlement is skipped after the shared loss: {log:?}"
    );
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease is retained to expire");
    assert_eq!(retained.instance_id, initial.instance_id);
    assert_eq!(retained.heartbeat_at_ms, initial.heartbeat_at_ms, "nothing renewed it");
}

/// What the hung stream runs of a bed did, as the tests read it.
#[derive(Default)]
struct HungRuns {
    /// The runs that received an orderly stop signal.
    signalled: AtomicUsize,
    /// Set when the run's task ends (finished, aborted or dropped).
    ended: Arc<AtomicBool>,
    /// The sender of each run's queue, for the test to enqueue frames.
    queues: Mutex<Vec<mpsc::Sender<StreamEvent>>>,
}

impl HungRuns {
    /// Wait (bounded watchdog) for the runs' own acknowledgement that `count` of them
    /// received a stop signal: a run counts it after it resumed from its stop.
    async fn acknowledged(&self, label: &str, count: usize) {
        timeout(Duration::from_secs(30), async {
            while self.signalled.load(Ordering::SeqCst) < count {
                sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{label}: the run never acknowledged the stop signal"));
    }

    /// Enqueue a frame of the car, and say so once the queue took it.
    async fn enqueue(&self, base_ms: i64, offset_ms: i64) {
        let queue = self.queues.lock().expect("queues").first().cloned().expect("a run");
        queue
            .send(drain_stream_frame(base_ms, offset_ms))
            .await
            .expect("the queue took the frame");
    }

    fn factory(self: &Arc<Self>) -> Arc<StreamRunFactory> {
        let runs = Arc::clone(self);
        Arc::new(move |injection: StreamInjection| {
            let runs = Arc::clone(&runs);
            let seam = Arc::clone(&injection.seam);
            tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(seam, async move {
                let StreamInjection { events, stop, .. } = injection;
                let _end = EndedFlag(Arc::clone(&runs.ended));
                runs.queues.lock().expect("queues").push(events.clone());
                // The run ignores the stop signal, once it has counted it: only an
                // abort ends it. (A dropped sender is no signal.)
                if stop.await.is_ok() {
                    runs.signalled.fetch_add(1, Ordering::SeqCst);
                }
                let _events = events;
                std::future::pending::<()>().await;
                Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
            }))
        })
    }
}

/// The orderly stop signals the stops of the bed sent, counted at the synchronous send.
fn signals_sent(seam: &SupervisedCollectorTestSeam) -> usize {
    seam.loop_log()
        .iter()
        .filter(|event| matches!(event, LoopEvent::StopSignalSent { .. }))
        .count()
}

/// The frames of the bed's seam log committed at `observed_at_ms`.
fn commits_of_the_frame(seam: &SupervisedCollectorTestSeam, observed_at_ms: i64) -> usize {
    seam.loop_log()
        .iter()
        .filter(|event| {
            matches!(event, LoopEvent::FrameCommitted { observed_at_ms: at, .. } if *at == observed_at_ms)
        })
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_owner_denial_during_final_stream_stop_aborts_without_egress() {
    // Ordinary shutdown with the admission intact begins the orderly stop of the streams:
    // the run (which ignores its stop) is signalled and the stop waits for it, its next
    // drain held before it writes anything. A distinct final frame is queued; the
    // admission is then denied. The retained admission is watched all the while the
    // stop lasts: the watch selects the loss and drops the stop (acknowledged by the
    // dropped wait at the drain), the run is aborted and joined locally, the final frame
    // is never written, no further signal goes out, the result is sensitive and the
    // lease is retained. The renewal's candidates are held, so nothing else observes the
    // loss.
    let runs = Arc::new(HungRuns::default());
    let mut held = None;
    let mut bed = DrainBed::start_with_admission(1, true, |_, seam| {
        seam.inject_stream_runs(runs.factory());
        held = Some((seam.arm_gate("stop-drain"), seam.arm_gate("renewal-candidate")));
    })
    .await;
    let (drain, _renewal) = held.expect("gates");
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    bed.wait_for("the loop's iterations", || seam.loop_iterations() >= 3).await;
    let _ = bed.shutdown.take().expect("shutdown").send(());
    drain.arrived("the final stop's drain", 1).await;
    // The drain is held: the run acknowledges the signal it was sent on its own
    // schedule (the stop does not wait for it), so the test waits for that
    // acknowledgement and the synchronous send is witnessed at its boundary.
    runs.acknowledged("the final stop", 1).await;
    assert_eq!(
        signals_sent(&seam),
        1,
        "the final stop has signalled the run and is waiting for it"
    );
    assert_eq!(runs.signalled.load(Ordering::SeqCst), 1);
    assert!(!runs.ended.load(Ordering::SeqCst));
    let base_ms = current_epoch_millis().expect("clock") - 60_000;
    let final_frame_ms = base_ms + 7_000;
    runs.enqueue(base_ms, 7_000).await;
    invalidate_the_retained_admission(bed.temporary.path());
    drain.cancelled_at_least("the final stop's drain", 1).await;
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the supervisor ends", &mut task).await;
    bed.server.abort();
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    assert!(runs.ended.load(Ordering::SeqCst), "the run was aborted and joined");
    assert_eq!(commits_of_the_frame(&seam, final_frame_ms), 0, "the final frame is never written");
    assert_eq!(signals_sent(&seam), 1, "no signal after the denial");
    assert_eq!(runs.signalled.load(Ordering::SeqCst), 1, "no signal after the denial");
    assert!(
        bed.store
            .supervised_collector_lease_status()
            .expect("lease query")
            .is_some(),
        "the lease is retained to expire"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_owner_denial_before_final_frames_skips_the_orderly_stop() {
    // The shutdown's refresh and credential outcomes have completed without a loss and
    // the supervisor is held just before it revalidates the admission for the streams'
    // final frames. A distinct eligible frame is queued, the admission is denied and the
    // supervisor released: it enters no orderly stop (the run is never signalled, so the
    // stop's own admission watch, which waits a second before its first look, cannot
    // supply the cancellation), writes no frame, aborts and joins the run locally, ends
    // with the sensitive failure and retains the lease. The renewal's candidates are
    // held so it cannot supply the loss first.
    let runs = Arc::new(HungRuns::default());
    let mut held = None;
    let mut bed = DrainBed::start_with_admission(1, true, |_, seam| {
        seam.inject_stream_runs(runs.factory());
        held = Some((seam.arm_gate("before-final-frames"), seam.arm_gate("renewal-candidate")));
    })
    .await;
    let (before, _renewal) = held.expect("gates");
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    bed.wait_for("the loop's iterations", || seam.loop_iterations() >= 3).await;
    let _ = bed.shutdown.take().expect("shutdown").send(());
    before.arrived("the check before the final frames", 1).await;
    let base_ms = current_epoch_millis().expect("clock") - 60_000;
    let frame_ms = base_ms + 9_000;
    runs.enqueue(base_ms, 9_000).await;
    invalidate_the_retained_admission(bed.temporary.path());
    before.release(1);
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the supervisor ends", &mut task).await;
    bed.server.abort();
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    // The synchronous send of the stop is the witness: a run aborted before it was
    // ever polled could not count a signal it was sent.
    assert_eq!(signals_sent(&seam), 0, "no orderly stop was entered");
    assert_eq!(runs.signalled.load(Ordering::SeqCst), 0, "no orderly stop was entered");
    assert!(runs.ended.load(Ordering::SeqCst), "the run was aborted and joined locally");
    assert_eq!(commits_of_the_frame(&seam, frame_ms), 0, "the queued frame is never written");
    assert!(
        bed.store
            .supervised_collector_lease_status()
            .expect("lease query")
            .is_some(),
        "the lease is retained to expire"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_renewal_denial_during_the_final_stream_stop_survives_the_admission_recovering() {
    // R-P2F3-1. The final stop of the streams is held at its drain (an acknowledged
    // gate). A distinct final frame is queued. The renewal then meets the denial (and
    // sets the shared fence), the admission recovers (the lock file is put back, and
    // the revalidation passes again), and the drain is released. The stop's own
    // admission watch looks only at the admission, which is valid again, so the shared
    // fence alone has to end the stop: no frame is committed after the denial, the run
    // is aborted and joined, the result is the sensitive failure and the lease is
    // retained with its heartbeat unchanged. A second case never releases the drain:
    // the fence alone must drop the held stop.
    for released in [true, false] {
        let runs = Arc::new(HungRuns::default());
        let mut held = None;
        let mut bed = DrainBed::start_with_admission(1, true, |_, seam| {
            seam.inject_stream_runs(runs.factory());
            held = Some((seam.arm_gate("stop-drain"), seam.arm_gate("renewal-candidate")));
        })
        .await;
        let (drain, renewal) = held.expect("gates");
        let seam = Arc::clone(&bed.seam);
        bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
        bed.wait_for("the loop's iterations", || seam.loop_iterations() >= 3).await;
        renewal.arrived("the renewal's candidate", 1).await;
        let initial = bed
            .store
            .supervised_collector_lease_status()
            .expect("lease query")
            .expect("active lease");
        let _ = bed.shutdown.take().expect("shutdown").send(());
        drain.arrived("the final stop's drain", 1).await;
        // The run's own acknowledgement of the signal it was sent (it resumes on its
        // own schedule, so the drain's arrival does not imply it).
        runs.acknowledged("the final stop", 1).await;
        assert_eq!(signals_sent(&seam), 1, "the final stop is waiting for the run");
        let base_ms = current_epoch_millis().expect("clock") - 60_000;
        let final_frame_ms = base_ms + 11_000;
        runs.enqueue(base_ms, 11_000).await;
        swap_the_admission_lock_away(bed.temporary.path());
        renewal.release(1);
        bed.wait_for("the renewal's own denial", || {
            seam.loop_log().contains(&LoopEvent::RenewalDenied)
        })
        .await;
        restore_the_admission_lock(bed.temporary.path());
        let admission = bed.admission.clone().expect("the retained admission");
        assert!(
            assert_runtime_sensitive_access(Some(&*admission)).is_ok(),
            "the admission passes again"
        );
        if released {
            drain.open();
        } else {
            // Never released: the fence alone ends the held stop (the wait is dropped).
            drain.cancelled_at_least("the final stop's drain", 1).await;
        }
        let mut task = bed.task.take().expect("task");
        let result = join_supervised_restart_task("the supervisor ends", &mut task).await;
        bed.server.abort();
        assert!(
            matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
            "{result:?}"
        );
        assert_eq!(
            commits_of_the_frame(&seam, final_frame_ms),
            0,
            "no frame is committed after the denial"
        );
        assert!(runs.ended.load(Ordering::SeqCst), "the run was joined");
        let retained = bed
            .store
            .supervised_collector_lease_status()
            .expect("lease query")
            .expect("the lease is retained to expire");
        assert_eq!(retained.instance_id, initial.instance_id);
        assert_eq!(retained.heartbeat_at_ms, initial.heartbeat_at_ms, "nothing renewed it");
    }
}

#[tokio::test(start_paused = true)]
async fn b2_a_stop_whose_fence_is_set_while_its_drain_is_held_writes_no_frame() {
    // R-P2F3-1, the drain boundary on its own (no select arm watches the fence here).
    // The stop is held at its drain by the acknowledged gate; a distinct frame is
    // queued, the fence is set and the drain released: the boundary sees the fence, no
    // frame is committed, the run is aborted and joined, and the stop is sensitive.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(Arc::clone(&seam), async {
            let gate = seam.arm_gate("stop-drain");
            let directory = crate::private_tempdir().expect("temporary store");
            let store = HubStore::initialize(directory.path()).expect("store");
            let mut core = Core::owner();
            let t0 = core.t0;
            core.scheduler.accept_discovery(vec![core_vehicle(1, "online")], t0);
            let ended = Arc::new(AtomicBool::new(false));
            let (runtime, events) = hung_stream_runtime(1, &ended);
            let mut streams = vec![runtime];
            let mut cache = HashMap::new();
            let mut pending = StreamDrainResult::default();
            let fence = AdmissionFence::default();
            let mut budget = DrainBudget::new(Instant::now());
            budget.guard_writes_with(fence.clone());
            let base_ms = current_epoch_millis().expect("clock") - 60_000;
            events
                .send(drain_stream_frame(base_ms, 3_000))
                .await
                .expect("the queue took the frame");
            let result = {
                let stop = stop_all_streams_and_store(
                    &store,
                    &mut core.scheduler,
                    &mut streams,
                    &mut cache,
                    &mut pending,
                    &mut budget,
                );
                tokio::pin!(stop);
                tokio::select! {
                    result = &mut stop => panic!("the stop ended before its drain: {result:?}"),
                    () = gate.arrived("the stop's drain", 1) => {}
                }
                fence.set();
                gate.open();
                timeout(Duration::from_secs(30), &mut stop).await.expect("the stop ends")
            };
            assert!(matches!(result, Err(CollectorError::SensitiveAccessUnavailable)), "{result:?}");
            assert_eq!(commits_of_the_frame(&seam, base_ms + 3_000), 0, "no frame is written");
            assert!(ended.load(Ordering::SeqCst), "the run was aborted and joined");
            assert!(streams.is_empty());
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn b2_a_stop_whose_fence_is_set_before_its_queued_flush_writes_no_frame() {
    // R-P2F3-1, the queued-frame flush on its own. The run ends on its stop signal and
    // leaves a frame queued; the fence is already set: the flush stores nothing and the
    // stop is sensitive. The same stop with the fence unset stores the frame once.
    for fenced in [true, false] {
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        SUPERVISED_COLLECTOR_TEST_SEAM
            .scope(Arc::clone(&seam), async {
                let directory = crate::private_tempdir().expect("temporary store");
                let store = HubStore::initialize(directory.path()).expect("store");
                let mut core = Core::owner();
                let t0 = core.t0;
                core.scheduler.accept_discovery(vec![core_vehicle(1, "online")], t0);
                let (events, receiver) = mpsc::channel(K_STREAM_EVENT_CAPACITY);
                let (shutdown, stop) = oneshot::channel::<()>();
                let task = tokio::spawn(async move {
                    let _ = stop.await;
                    Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
                });
                let mut streams = vec![VehicleStreamRuntime {
                    vehicle_id: core_id(1),
                    power_gate: Arc::new(StreamPowerGate::default()),
                    sensitive_access_failure: Arc::new(AtomicBool::new(false)),
                    events: receiver,
                    _shutdown: Some(shutdown),
                    task: Some(task),
                }];
                let base_ms = current_epoch_millis().expect("clock") - 60_000;
                events
                    .send(drain_stream_frame(base_ms, 5_000))
                    .await
                    .expect("the queue took the frame");
                let mut cache = HashMap::new();
                let mut pending = StreamDrainResult::default();
                let fence = AdmissionFence::default();
                if fenced {
                    fence.set();
                }
                let mut budget = DrainBudget::new(Instant::now());
                budget.guard_writes_with(fence);
                let result = stop_all_streams_and_store(
                    &store,
                    &mut core.scheduler,
                    &mut streams,
                    &mut cache,
                    &mut pending,
                    &mut budget,
                )
                .await;
                let committed = commits_of_the_frame(&seam, base_ms + 5_000);
                if fenced {
                    assert!(matches!(result, Err(CollectorError::SensitiveAccessUnavailable)), "{result:?}");
                    assert_eq!(committed, 0, "the flush wrote no frame");
                } else {
                    result.expect("the unfenced stop");
                    assert_eq!(committed, 1, "the unfenced stop stores the queued frame once");
                }
                assert!(streams.is_empty());
            })
            .await;
    }
}

#[tokio::test(start_paused = true)]
async fn b2_a_fence_set_between_two_frame_commits_of_the_stop_drain_writes_no_second_frame() {
    // R-P2F4-1, the drain. Every frame is a transaction of its own, so a fence set after
    // one frame committed makes the next a new write. Two frames are queued; the seam's
    // hook sets the fence synchronously right after the first one committed (the hook's
    // own count is the acknowledgement that it ran): the second is not written, the stop
    // is the sensitive failure and the run is aborted and joined. The same stop with no
    // hook stores both frames, once each, through the same drain.
    for fenced in [true, false] {
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        SUPERVISED_COLLECTOR_TEST_SEAM
            .scope(Arc::clone(&seam), async {
                let directory = crate::private_tempdir().expect("temporary store");
                let store = HubStore::initialize(directory.path()).expect("store");
                let mut core = Core::owner();
                let t0 = core.t0;
                core.scheduler.accept_discovery(vec![core_vehicle(1, "online")], t0);
                let ended = Arc::new(AtomicBool::new(false));
                let (runtime, events) = hung_stream_runtime(1, &ended);
                let mut streams = vec![runtime];
                let mut cache = HashMap::new();
                let mut pending = StreamDrainResult::default();
                let fence = AdmissionFence::default();
                let mut budget = DrainBudget::new(Instant::now());
                budget.guard_writes_with(fence.clone());
                let base_ms = current_epoch_millis().expect("clock") - 60_000;
                let (first_ms, second_ms) = (base_ms + 3_000, base_ms + 4_000);
                let hook_ran = Arc::new(AtomicUsize::new(0));
                if fenced {
                    let (hook_fence, ran) = (fence.clone(), Arc::clone(&hook_ran));
                    seam.set_frame_committed_hook(Arc::new(move |at| {
                        if at == first_ms {
                            hook_fence.set();
                            ran.fetch_add(1, Ordering::SeqCst);
                        }
                    }));
                }
                for offset in [3_000, 4_000] {
                    events
                        .send(drain_stream_frame(base_ms, offset))
                        .await
                        .expect("the queue took the frame");
                }
                let result = stop_all_streams_and_store(
                    &store,
                    &mut core.scheduler,
                    &mut streams,
                    &mut cache,
                    &mut pending,
                    &mut budget,
                )
                .await;
                assert!(!budget.marks.is_empty(), "the frames went through the stop's drain");
                assert_eq!(commits_of_the_frame(&seam, first_ms), 1, "the first frame committed");
                assert!(ended.load(Ordering::SeqCst), "the run was aborted and joined");
                assert!(streams.is_empty());
                if fenced {
                    assert_eq!(hook_ran.load(Ordering::SeqCst), 1, "the fence was set after the first commit");
                    assert!(matches!(result, Err(CollectorError::SensitiveAccessUnavailable)), "{result:?}");
                    assert_eq!(commits_of_the_frame(&seam, second_ms), 0, "no second commit after the fence");
                } else {
                    result.expect("the unfenced stop");
                    assert_eq!(commits_of_the_frame(&seam, second_ms), 1, "the unfenced drain stores both");
                }
            })
            .await;
    }
}

#[tokio::test(start_paused = true)]
async fn b2_a_fence_set_between_two_frame_commits_of_the_queued_flush_writes_no_second_frame() {
    // R-P2F4-1, the queued-frame flush. The run ends on its stop signal at once and
    // leaves two frames queued, so they are stored by the flush (the stop's drain never
    // ran). The fence is set right after the first one committed: the second is not
    // written and the stop is the sensitive failure. The same stop with no hook stores
    // both frames, once each.
    for fenced in [true, false] {
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        SUPERVISED_COLLECTOR_TEST_SEAM
            .scope(Arc::clone(&seam), async {
                let directory = crate::private_tempdir().expect("temporary store");
                let store = HubStore::initialize(directory.path()).expect("store");
                let mut core = Core::owner();
                let t0 = core.t0;
                core.scheduler.accept_discovery(vec![core_vehicle(1, "online")], t0);
                let (events, receiver) = mpsc::channel(K_STREAM_EVENT_CAPACITY);
                let (shutdown, stop) = oneshot::channel::<()>();
                let ended = Arc::new(AtomicBool::new(false));
                let end = EndedFlag(Arc::clone(&ended));
                let task = tokio::spawn(async move {
                    let _end = end;
                    let _ = stop.await;
                    Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
                });
                let mut streams = vec![VehicleStreamRuntime {
                    vehicle_id: core_id(1),
                    power_gate: Arc::new(StreamPowerGate::default()),
                    sensitive_access_failure: Arc::new(AtomicBool::new(false)),
                    events: receiver,
                    _shutdown: Some(shutdown),
                    task: Some(task),
                }];
                let mut cache = HashMap::new();
                let mut pending = StreamDrainResult::default();
                let fence = AdmissionFence::default();
                let mut budget = DrainBudget::new(Instant::now());
                budget.guard_writes_with(fence.clone());
                let base_ms = current_epoch_millis().expect("clock") - 60_000;
                let (first_ms, second_ms) = (base_ms + 5_000, base_ms + 6_000);
                let hook_ran = Arc::new(AtomicUsize::new(0));
                if fenced {
                    let (hook_fence, ran) = (fence.clone(), Arc::clone(&hook_ran));
                    seam.set_frame_committed_hook(Arc::new(move |at| {
                        if at == first_ms {
                            hook_fence.set();
                            ran.fetch_add(1, Ordering::SeqCst);
                        }
                    }));
                }
                for offset in [5_000, 6_000] {
                    events
                        .send(drain_stream_frame(base_ms, offset))
                        .await
                        .expect("the queue took the frame");
                }
                let result = stop_all_streams_and_store(
                    &store,
                    &mut core.scheduler,
                    &mut streams,
                    &mut cache,
                    &mut pending,
                    &mut budget,
                )
                .await;
                assert!(budget.marks.is_empty(), "the stop's drain never ran: the flush stored the frames");
                assert_eq!(commits_of_the_frame(&seam, first_ms), 1, "the first frame committed");
                assert!(ended.load(Ordering::SeqCst), "the run had ended");
                assert!(streams.is_empty());
                if fenced {
                    assert_eq!(hook_ran.load(Ordering::SeqCst), 1, "the fence was set after the first commit");
                    assert!(matches!(result, Err(CollectorError::SensitiveAccessUnavailable)), "{result:?}");
                    assert_eq!(commits_of_the_frame(&seam, second_ms), 0, "no second commit after the fence");
                } else {
                    result.expect("the unfenced stop");
                    assert_eq!(commits_of_the_frame(&seam, second_ms), 1, "the unfenced flush stores both");
                }
            })
            .await;
    }
}

#[tokio::test(start_paused = true)]
async fn b2_a_fence_set_between_two_frame_commits_of_a_finished_streams_tail_writes_no_second_frame() {
    // R-P2F5-3, the tail of a finished run that is not winding down. The drain's bounded
    // loop takes `MAX_STREAM_EVENTS_PER_DRAIN` events; the rest of the finished run's
    // queue is stored by the tail loop, each frame in a transaction of its own. Eighteen
    // frames are queued, so the last two belong to the tail; the seam's hook sets the
    // fence right after the first of them committed: the second is not written and the
    // drain is the sensitive failure. The same drain with no hook stores every frame once.
    for fenced in [true, false] {
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        SUPERVISED_COLLECTOR_TEST_SEAM
            .scope(Arc::clone(&seam), async {
                let directory = crate::private_tempdir().expect("temporary store");
                let store = HubStore::initialize(directory.path()).expect("store");
                let mut core = Core::owner();
                let t0 = core.t0;
                core.scheduler.accept_discovery(vec![core_vehicle(1, "online")], t0);
                let (events, receiver) = mpsc::channel(K_STREAM_EVENT_CAPACITY);
                let (shutdown, _stop) = oneshot::channel::<()>();
                let task = tokio::spawn(async {
                    Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
                });
                let mut streams = vec![VehicleStreamRuntime {
                    vehicle_id: core_id(1),
                    power_gate: Arc::new(StreamPowerGate::default()),
                    sensitive_access_failure: Arc::new(AtomicBool::new(false)),
                    events: receiver,
                    _shutdown: Some(shutdown),
                    task: Some(task),
                }];
                let base_ms = current_epoch_millis().expect("clock") - 60_000;
                let frame_ms = |ordinal: i64| base_ms + 1_000 * ordinal;
                let (first_tail_ms, second_tail_ms) = (
                    frame_ms(i64::try_from(MAX_STREAM_EVENTS_PER_DRAIN).expect("bound") + 1),
                    frame_ms(i64::try_from(MAX_STREAM_EVENTS_PER_DRAIN).expect("bound") + 2),
                );
                let fence = AdmissionFence::default();
                let hook_ran = Arc::new(AtomicUsize::new(0));
                if fenced {
                    let (hook_fence, ran) = (fence.clone(), Arc::clone(&hook_ran));
                    seam.set_frame_committed_hook(Arc::new(move |at| {
                        if at == first_tail_ms {
                            hook_fence.set();
                            ran.fetch_add(1, Ordering::SeqCst);
                        }
                    }));
                }
                for ordinal in 1..=i64::try_from(MAX_STREAM_EVENTS_PER_DRAIN).expect("bound") + 2 {
                    events
                        .send(drain_stream_frame(base_ms, 1_000 * ordinal))
                        .await
                        .expect("the queue took the frame");
                }
                // The run has ended by itself: its queue is stable.
                while !streams[0].task.as_ref().is_some_and(JoinHandle::is_finished) {
                    tokio::task::yield_now().await;
                }
                let mut cache = HashMap::new();
                let result = drain_stream_events_while_stopping(
                    &store,
                    &mut core.scheduler,
                    &mut streams,
                    &mut cache,
                    &[],
                    Some(&RunWriteGuard::from(fence.clone())),
                )
                .await
                .map(|_| ());
                for ordinal in 1..=i64::try_from(MAX_STREAM_EVENTS_PER_DRAIN).expect("bound") + 1 {
                    assert_eq!(commits_of_the_frame(&seam, frame_ms(ordinal)), 1, "frame {ordinal}");
                }
                if fenced {
                    assert_eq!(hook_ran.load(Ordering::SeqCst), 1, "the fence was set after the first tail commit");
                    assert!(matches!(result, Err(CollectorError::SensitiveAccessUnavailable)), "{result:?}");
                    assert_eq!(commits_of_the_frame(&seam, second_tail_ms), 0, "no tail frame after the fence");
                } else {
                    result.expect("the unfenced drain");
                    assert_eq!(commits_of_the_frame(&seam, second_tail_ms), 1, "the unfenced tail stores every frame");
                }
            })
            .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_renewal_denial_between_two_frame_commits_of_the_final_stop_keeps_the_lease() {
    // R-P2F4-1 through the real supervisor. The final stop is held at its drain with two
    // distinct frames queued. When the drain is released, the hook that runs right after
    // the first frame committed lets the lease's renewal meet the denial (the admission
    // lock is swapped away, the renewal's candidate released) and waits for the renewal's
    // own acknowledgement of it (the seam's `RenewalDenied`) before the lock is put back,
    // so the admission is valid again and only the shared fence can stop the drain. The
    // second frame is not written, the run is aborted and joined, the result is the
    // sensitive failure and the lease is retained with its heartbeat unchanged.
    let runs = Arc::new(HungRuns::default());
    let mut held = None;
    let mut bed = DrainBed::start_with_admission(1, true, |_, seam| {
        seam.inject_stream_runs(runs.factory());
        held = Some((seam.arm_gate("stop-drain"), seam.arm_gate("renewal-candidate")));
    })
    .await;
    let (drain, renewal) = held.expect("gates");
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    bed.wait_for("the loop's iterations", || seam.loop_iterations() >= 3).await;
    renewal.arrived("the renewal's candidate", 1).await;
    let initial = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("active lease");
    let base_ms = current_epoch_millis().expect("clock") - 60_000;
    let (first_ms, second_ms) = (base_ms + 12_000, base_ms + 13_000);
    {
        let (weak_seam, data_dir, renewal) =
            (Arc::downgrade(&seam), bed.temporary.path().to_path_buf(), Arc::clone(&renewal));
        seam.set_frame_committed_hook(Arc::new(move |at| {
            if at != first_ms {
                return;
            }
            // The supervisor's task blocks its worker here, so the worker hands its
            // queued tasks over first (the renewal, woken by the release below, would
            // otherwise sit in this worker's own queue).
            tokio::task::block_in_place(|| {
                swap_the_admission_lock_away(&data_dir);
                renewal.release(1);
                let seam = weak_seam.upgrade().expect("the seam");
                let waited = std::time::Instant::now();
                while !seam.loop_log().contains(&LoopEvent::RenewalDenied) {
                    assert!(
                        waited.elapsed() < Duration::from_secs(30),
                        "the renewal never met the denial"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                restore_the_admission_lock(&data_dir);
            });
        }));
    }
    let _ = bed.shutdown.take().expect("shutdown").send(());
    drain.arrived("the final stop's drain", 1).await;
    runs.acknowledged("the final stop", 1).await;
    runs.enqueue(base_ms, 12_000).await;
    runs.enqueue(base_ms, 13_000).await;
    drain.open();
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the supervisor ends", &mut task).await;
    bed.server.abort();
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    assert_eq!(commits_of_the_frame(&seam, first_ms), 1, "the first frame committed before the denial");
    assert_eq!(commits_of_the_frame(&seam, second_ms), 0, "no second commit after the denial");
    assert!(runs.ended.load(Ordering::SeqCst), "the run was joined");
    let admission = bed.admission.clone().expect("the retained admission");
    assert!(
        assert_runtime_sensitive_access(Some(&*admission)).is_ok(),
        "the admission passed again: only the fence stopped the drain"
    );
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease is retained to expire");
    assert_eq!(retained.instance_id, initial.instance_id);
    assert_eq!(retained.heartbeat_at_ms, initial.heartbeat_at_ms, "nothing renewed it");
}

#[tokio::test(start_paused = true)]
async fn b2_a_runtime_dropped_with_a_live_stop_sender_logs_its_signal() {
    // R-P2F4-4. `Drop` is the third production send of a stop signal. It is witnessed
    // like the other two: a runtime dropped with its sender still in place logs one
    // `StopSignalSent` for its car, and one whose sender was taken (signalled) earlier
    // logs none when it drops.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(Arc::clone(&seam), async {
            let ended = Arc::new(AtomicBool::new(false));
            let (runtime, _events) = hung_stream_runtime(7, &ended);
            assert_eq!(signals_sent(&seam), 0);
            drop(runtime);
            assert_eq!(signals_sent(&seam), 1, "the drop signalled the run");
            assert!(
                seam.loop_log().contains(&LoopEvent::StopSignalSent { vehicle: 7 }),
                "the signal names the dropped runtime's car"
            );
            let (mut runtime, _events) = hung_stream_runtime(8, &ended);
            drop(runtime._shutdown.take());
            drop(runtime);
            assert_eq!(signals_sent(&seam), 1, "a runtime with no sender signals nothing");
        })
        .await;
}
