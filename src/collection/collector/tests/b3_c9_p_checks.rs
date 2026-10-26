// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::test_assertions::LabelledResult;

#[tokio::test]
async fn c9_waiting_pass_rechecks_access_before_marker() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    for tidy_only in [false, true] {
        let (_dir, store, vehicle) = fixture(false);
        seed(&store, vehicle, vec![point(1, 1, 0, 20)]);
        let clock = Arc::new(ManualClock::new());
        struct Held {
            source: Source,
            entered: tokio::sync::Notify,
            resume: tokio::sync::Notify,
            first: AtomicBool,
        }
        impl TileSource for Held {
            fn acquire<'a>(
                &'a self,
                tile: TileId,
                guard: &'a dyn EgressGuard,
                inner: &'a dyn EgressGuard,
            ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>
            {
                Box::pin(async move {
                    if !self.first.swap(true, Ordering::SeqCst) {
                        self.entered.notify_one();
                        self.resume.notified().await;
                    }
                    self.source.acquire(tile, guard, inner).await
                })
            }
        }
        impl Drop for Held {
            fn drop(&mut self) {
                self.resume.notify_waiters();
                self.resume.notify_one();
            }
        }
        struct ObservedAccess {
            runtime: RuntimeAdmissionCheck,
            checked: tokio::sync::Notify,
        }
        impl SensitiveAccessCheck for ObservedAccess {
            fn check(&self) -> Result<(), CollectorError> {
                let result = self.runtime.check();
                self.checked.notify_one();
                result
            }
        }
        let held = Held {
            source: Source::new(&clock),
            entered: tokio::sync::Notify::new(),
            resume: tokio::sync::Notify::new(),
            first: AtomicBool::new(false),
        };
        let run = RunWriteGuard::for_run(None);
        let access = ObservedAccess {
            runtime: RuntimeAdmissionCheck::new(run.clone()),
            checked: tokio::sync::Notify::new(),
        };
        let mut first_state = TerrainWorkerState::default();
        let mut second_state = TerrainWorkerState::default();
        let reads = crate::db::TERRAIN_MARKER_READS.with(|n| n.get());
        let first = run_terrain_enrichment_pass(
            &store,
            &held,
            &mut first_state,
            &Access(true),
            &Permit(true),
            clock.as_ref(),
        );
        let second = async {
            held.entered.notified().await;
            if tidy_only {
                run_terrain_tidy_only_pass(&store, &mut second_state, &access, clock.as_ref()).await
            } else {
                run_terrain_enrichment_pass(
                    &store,
                    &held,
                    &mut second_state,
                    &access,
                    &access.runtime,
                    clock.as_ref(),
                )
                .await
            }
        };
        let revoke = async {
            let started = std::time::Instant::now();
            let ready = timeout(Duration::from_secs(10), access.checked.notified())
                .await
                .is_ok();
            terrain_test_timing!(
                "FR3_TIMING c9_waiting_pass_rechecks_access_before_marker admission_wait={:.6} bound=10",
                started.elapsed().as_secs_f64()
            );
            run.fence.set();
            held.resume.notify_one();
            ready
        };
        let started = std::time::Instant::now();
        let joined = timeout(Duration::from_secs(30), async {
            tokio::join!(first, second, revoke)
        })
        .await;
        held.resume.notify_one();
        terrain_test_timing!(
            "FR3_TIMING c9_waiting_pass_rechecks_access_before_marker join={:.6} bound=30",
            started.elapsed().as_secs_f64()
        );
        let (first, second, ready) = joined.labelled_ok(
            "c9_waiting_pass_rechecks_access_before_marker",
            "waiting passes complete within the bound",
        );
        assert!(
            ready,
            "assertion failed: c9_waiting_pass_rechecks_access_before_marker: waiting pass admission is acknowledged within the bound"
        );
        assert_eq!(first.unwrap().filled, 1);
        assert!(second.unwrap().access_denied);
        assert_eq!(held.source.count(), 1);
        assert_eq!(crate::db::TERRAIN_MARKER_READS.with(|n| n.get()) - reads, 1);
        assert_eq!(run.audit().refused("terrain-pass"), 1);
    }
}

async fn check_fenced_mutation(test: &str, path: &str, denied_at: usize) {
    let (_dir, store, vehicle) = fixture(path == "tidy");
    let rows = if path == "outcomes" {
        (1..=251).map(|id| point(id, 1, 0, 20)).collect()
    } else {
        let mut position = point(1, 1, 0, 20);
        if matches!(path, "retire" | "cursor") {
            position.elevation = Some(5);
        }
        vec![position]
    };
    seed(&store, vehicle, rows);
    if path == "retire" {
        plant(&store, vehicle, 1, "failed", None, T1 - 1);
    }
    if path == "boundary" {
        let imported = import_source(&store);
        watermark(&store, imported, vehicle, 1, T0 + 1000);
    }
    let initial = (durable(&store), store.terrain_tidy_progress().unwrap());
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let run = RunWriteGuard::for_run(None);
    let runtime = RuntimeAdmissionCheck::new(run.clone());
    let stamp = Arc::new(Mutex::new(None));
    let captured = stamp.clone();
    let copy = store.clone();
    run.audit().trip_at_nth("terrain-pass", denied_at, move || {
        *captured.lock().unwrap() = Some((durable(&copy), copy.terrain_tidy_progress().unwrap()));
    });
    let report = run_terrain_enrichment_pass(
        &store,
        &source,
        &mut TerrainWorkerState::default(),
        &runtime,
        &runtime,
        clock.as_ref(),
    )
    .await
    .unwrap();
    assert!(
        run.audit().tripped(),
        "assertion failed: {test}: the nominated mutation reaches the live admission fence; path={path}"
    );
    assert!(
        report.access_denied,
        "assertion failed: {test}: live admission refusal denies the pass; path={path}"
    );
    assert!(
        !report.full_batch,
        "assertion failed: {test}: refusal must not request continuation; path={path}"
    );
    assert_eq!(report.failed, 0, "{test}: {path}");
    assert_eq!(report.wrote, path == "outcomes", "{test}: {path}");
    assert_eq!(
        (durable(&store), store.terrain_tidy_progress().unwrap()),
        stamp.lock().unwrap().take().unwrap(),
        "{test}: {path}"
    );
    if path != "outcomes" {
        assert_eq!(
            (durable(&store), store.terrain_tidy_progress().unwrap()),
            initial,
            "{test}: no writes before the refusal; path={path}"
        );
    }
    assert_eq!(
        report.filled,
        if path == "outcomes" { 250 } else { 0 },
        "{test}: {path}"
    );
    assert_eq!(
        cursor(&store, vehicle),
        if path == "outcomes" { 250 } else { 0 },
        "{test}: {path}"
    );
}

#[tokio::test]
async fn c9_fence_tidy() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    check_fenced_mutation("c9_fence_tidy", "tidy", 3).await;
}

#[tokio::test]
async fn c9_fence_boundary() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    check_fenced_mutation("c9_fence_boundary", "boundary", 3).await;
}

#[tokio::test]
async fn c9_fence_retire() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    check_fenced_mutation("c9_fence_retire", "retire", 3).await;
}

#[tokio::test]
async fn c9_fence_cursor() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    check_fenced_mutation("c9_fence_cursor", "cursor", 3).await;
}

#[tokio::test]
async fn c9_fence_outcomes() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    check_fenced_mutation("c9_fence_outcomes", "outcomes", 4).await;
}

#[tokio::test]
async fn c9_mixed_groups_retry_ties_and_sample_order() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_dir, store, a) = fixture(false);
    let b = vehicle(&store, "two");
    let mut vehicles = [a, b];
    vehicles.sort();
    let [forward, retry] = vehicles;
    seed(
        &store,
        forward,
        vec![point(1, 1, 0, 22), point(2, 1, 0, 20), point(3, 1, 0, 22)],
    );
    plant(&store, forward, 3, "failed", None, T1 - 1);
    seed(
        &store,
        retry,
        vec![
            point(7, 1, 0, 22),
            point(8, 1, 0, 21),
            point(9, 1, 0, 19),
            point(10, 1, 0, 23),
        ],
    );
    for id in 7..=10 {
        plant(&store, retry, id, "failed", None, T1 - 1);
    }
    let waiting = provenance(&store, retry, 10);
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let report = pass(&store, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(
        (
            report.filled,
            report.tiles,
            report.deferred,
            report.full_batch
        ),
        (6, 4, 1, true)
    );
    assert_eq!(
        *source.calls.lock().unwrap(),
        ["N10E022", "N10E020", "N10E019", "N10E021"]
    );
    assert_eq!(provenance(&store, retry, 10), waiting);
    let connection = store.open().unwrap();
    let mut query = connection.prepare("SELECT vehicle_id, entity_id FROM sync_mutations WHERE entity='position' ORDER BY revision").unwrap();
    let written: Vec<(String, i64)> = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        written,
        [
            (forward.to_string(), 1),
            (forward.to_string(), 3),
            (retry.to_string(), 7),
            (forward.to_string(), 2),
            (retry.to_string(), 9),
            (retry.to_string(), 8)
        ]
    );
    assert_eq!(cursor(&store, forward), 3);
}

#[tokio::test]
async fn c9_cursor_waits_for_the_later_group() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_dir, store, vehicle) = fixture(false);
    seed(
        &store,
        vehicle,
        (1..=6)
            .map(|id| point(id, 1, 0, if [1, 2, 5].contains(&id) { 20 } else { 21 }))
            .collect(),
    );
    store.open().unwrap().execute_batch("CREATE TRIGGER fail_later BEFORE INSERT ON terrain_elevation_provenance WHEN NEW.position_id=3 BEGIN SELECT RAISE(ABORT,'synthetic later group'); END;").unwrap();
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let mut state = TerrainWorkerState::default();
    let result = run_terrain_enrichment_pass(
        &store,
        &source,
        &mut state,
        &Access(true),
        &Permit(true),
        clock.as_ref(),
    )
    .await;
    assert!(matches!(result, Err(CollectorError::Store(_))));
    assert_eq!(cursor(&store, vehicle), 2);
    for id in [1, 2, 5] {
        assert_eq!(elevation(&store, vehicle, id), Some(0));
        assert_eq!(provenance(&store, vehicle, id).1, 1);
    }
    for id in [3, 4, 6] {
        assert_eq!(elevation(&store, vehicle, id), None);
    }
    store
        .open()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_later;")
        .unwrap();
    let report = pass(&store, &source, &mut state).await;
    assert_eq!((report.filled, cursor(&store, vehicle)), (3, 6));
    assert_eq!(
        *source.calls.lock().unwrap(),
        ["N10E020", "N10E021", "N10E021"]
    );
}

#[tokio::test]
async fn c9_clock_errors_never_write_prepared_work() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    struct FailingClock {
        calls: AtomicUsize,
        fail_at: usize,
    }
    impl TerrainClock for FailingClock {
        fn now_ms(&self) -> Result<i64, CollectorError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_at {
                Err(CollectorError::SystemClockBeforeEpoch)
            } else {
                Ok(T1)
            }
        }
    }
    for (pending, fail_at) in [(true, 1), (true, 2), (false, 1), (false, 4), (false, 5)] {
        let (_dir, store, vehicle) = fixture(pending);
        seed(&store, vehicle, vec![point(1, 1, 0, 20)]);
        let before = (durable(&store), store.terrain_tidy_progress().unwrap());
        let source = Source::new(&Arc::new(ManualClock::new()));
        let clock = FailingClock {
            calls: AtomicUsize::new(0),
            fail_at,
        };
        let mut state = TerrainWorkerState::default();
        state.breaker.lock().unwrap().state = BreakerState::HalfOpen { permit_held: false };
        let report = run_terrain_enrichment_pass(
            &store,
            &source,
            &mut state,
            &Access(true),
            &Permit(true),
            &clock,
        )
        .await
        .unwrap();
        assert_eq!(report.worker_condition, Some("clock"));
        assert!(!report.full_batch);
        assert!(!report.wrote);
        assert_eq!(clock.calls.load(Ordering::SeqCst), fail_at);
        assert_eq!(source.count(), usize::from(!pending && fail_at >= 4));
        assert_eq!(
            (durable(&store), store.terrain_tidy_progress().unwrap()),
            before
        );
        if fail_at == 4 {
            assert_eq!(
                state.breaker.lock().unwrap().state,
                BreakerState::HalfOpen { permit_held: false }
            );
        }
    }
}

#[tokio::test]
async fn c9_drive_memo_is_cleared_between_passes() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_dir, store, vehicle) = fixture(false);
    seed(
        &store,
        vehicle,
        vec![point(1, 1, 0, 20), point(2, 1, 0, 20)],
    );
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let mut state = TerrainWorkerState::default();
    assert_eq!(pass(&store, &source, &mut state).await.filled, 2);
    let mut observed = point(1, 1, 0, 20);
    observed.elevation = Some(55);
    commit(
        &store,
        vehicle,
        Vec::new(),
        vec![observed, point(3, 1, 0, 20)],
    );
    let report = pass(&store, &source, &mut state).await;
    assert_eq!(
        (
            report.filled,
            report.stale,
            source.count(),
            cursor(&store, vehicle)
        ),
        (0, 0, 1, 3)
    );
    assert_eq!(elevation(&store, vehicle, 1), Some(55));
    assert_eq!(elevation(&store, vehicle, 3), None);
}

#[tokio::test]
async fn c9_breaker_eligibility_uses_pass_start() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    struct AdvancingClock(AtomicI64);
    impl TerrainClock for AdvancingClock {
        fn now_ms(&self) -> Result<i64, CollectorError> {
            Ok(self.0.fetch_add(1, Ordering::SeqCst))
        }
    }
    let (_dir, store, vehicle) = fixture(false);
    seed(
        &store,
        vehicle,
        vec![point(1, 1, 0, 20), point(2, 1, 0, 21)],
    );
    plant(&store, vehicle, 1, "failed", None, T1);
    let waiting = provenance(&store, vehicle, 1);
    let source = Source::new(&Arc::new(ManualClock::new()));
    let clock = AdvancingClock(AtomicI64::new(T1));
    let mut state = TerrainWorkerState::default();
    state.breaker.lock().unwrap().state = BreakerState::Open { until_ms: T1 + 1 };
    let report = run_terrain_enrichment_pass(
        &store,
        &source,
        &mut state,
        &Access(true),
        &Permit(true),
        &clock,
    )
    .await
    .unwrap();
    assert_eq!(
        (report.filled, source.count(), cursor(&store, vehicle)),
        (1, 1, 2)
    );
    assert_eq!(provenance(&store, vehicle, 1), waiting);
    assert_eq!(state.breaker.lock().unwrap().state, BreakerState::Closed);
}

fn cached_source(store: &HubStore, tile: TileId) -> (tempfile::TempDir, CacheTileSource) {
    let root = crate::private_tempdir().unwrap();
    let bytes = vec![0; SRTM3_SIDE * SRTM3_SIDE * 2];
    let loaded = HgtTile::from_bytes(&tile.name(), bytes.clone()).unwrap();
    std::fs::write(root.path().join(format!("{}.hgt", tile.name())), bytes).unwrap();
    std::fs::write(root.path().join(format!("{}.source", tile.name())), "aws").unwrap();
    store
        .record_terrain_tile_digest(&tile.name(), &loaded.sha256_hex().unwrap(), T1)
        .unwrap();
    let cache = TerrainCache::new(crate::terrain_cache::TerrainCacheOptions {
        root: root.path().to_owned(),
        store: store.clone(),
        min_free_bytes: 1,
        max_cache_bytes: 64 * 1024 * 1024,
        connect_timeout: Duration::from_secs(1),
        read_timeout: Duration::from_secs(1),
        aws_base: "http://127.0.0.1:9/aws/".into(),
        esa_base: "http://127.0.0.1:9/esa/".into(),
        free_space_override: Some(u64::MAX),
    })
    .unwrap();
    (root, CacheTileSource { cache })
}

// The supervisor owns cancellation and release outside the executor under test.
const LOCAL_WATCHDOG: Duration = Duration::from_secs(10);
type LocalAcquisition = Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send>>;

fn check_local_stage(test: &str, stage: TerrainLocalStage, name: &str) {
    let _serial = super::super::TERRAIN_CACHE_TEST_SERIAL.blocking_lock();
    let started = std::time::Instant::now();
    let drain_started = std::time::Instant::now();
    while TERRAIN_BLOCKING_JOBS.load(Ordering::SeqCst) != 0
        && drain_started.elapsed() < LOCAL_WATCHDOG
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    let quiescent = TERRAIN_BLOCKING_JOBS.load(Ordering::SeqCst);
    let initial_drain_seconds = drain_started.elapsed().as_secs_f64();
    let (_dir, store, _) = fixture(false);
    let tile = TileId::parse(name).unwrap();
    let (_root, source) = cached_source(&store, tile);
    let source = Arc::new(source);
    let (_other_root, other) = cached_source(&store, TileId::parse("N33E077").unwrap());
    let (entered_send, mut entered_receive) = oneshot::channel();
    let (release_send, release_receive) = std::sync::mpsc::sync_channel(1);
    struct Release {
        tile: TileId,
        send: std::sync::mpsc::SyncSender<()>,
        owner: Arc<Mutex<Option<LocalAcquisition>>>,
    }
    impl Drop for Release {
        fn drop(&mut self) {
            // Release first: an inline adapter may hold the owner mutex while blocked.
            let _ = self.send.try_send(());
            TERRAIN_LOCAL_HOLD
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|(_, id, ..)| *id != self.tile);
            match self.owner.try_lock() {
                Ok(mut owner) => {
                    owner.take();
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    error.into_inner().take();
                }
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
        }
    }
    let owned_source = source.clone();
    let owner: Arc<Mutex<Option<LocalAcquisition>>> =
        Arc::new(Mutex::new(Some(Box::pin(async move {
            owned_source
                .acquire(tile, &Permit(false), &Permit(true))
                .await
        }))));
    let release = Release {
        tile,
        send: release_send,
        owner: owner.clone(),
    };
    let advance = Arc::new(AtomicBool::new(false));
    let finish = Arc::new(AtomicBool::new(false));
    let (caller_send, caller_receive) = std::sync::mpsc::sync_channel(1);
    let (deadline_send, deadline_receive) = std::sync::mpsc::sync_channel(1);
    let (done_send, done_receive) = std::sync::mpsc::sync_channel(1);
    let thread_owner = owner.clone();
    let thread_advance = advance.clone();
    let thread_finish = finish.clone();
    let executor = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let caller = std::thread::current().id();
        TERRAIN_LOCAL_HOLD.lock().unwrap().push((
            stage,
            tile,
            caller,
            entered_send,
            release_receive,
        ));
        let _ = caller_send.send(caller);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async {
                let mut result = None;
                while !thread_finish.load(Ordering::SeqCst) {
                    std::future::poll_fn(|cx| {
                        let mut slot = thread_owner.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(acquire) = slot.as_mut()
                            && let std::task::Poll::Ready(value) = acquire.as_mut().poll(cx)
                        {
                            result = Some(value);
                            slot.take();
                        }
                        std::task::Poll::Ready(())
                    })
                    .await;
                    if thread_advance.swap(false, Ordering::SeqCst) {
                        // Drive the contractual deadline independently of acquisition.
                        // One millisecond is past 30 s and strictly before a 31 s mutant.
                        tokio::time::advance(K_LOCAL_TILE_BUDGET + Duration::from_millis(1)).await;
                        let poll_started = std::time::Instant::now();
                        while result.is_none() && poll_started.elapsed() < LOCAL_WATCHDOG {
                            std::future::poll_fn(|cx| {
                                let mut slot =
                                    thread_owner.lock().unwrap_or_else(|e| e.into_inner());
                                if let Some(acquire) = slot.as_mut()
                                    && let std::task::Poll::Ready(value) = acquire.as_mut().poll(cx)
                                {
                                    result = Some(value);
                                    slot.take();
                                }
                                std::task::Poll::Ready(())
                            })
                            .await;
                            tokio::task::yield_now().await;
                        }
                        let local_timeout = matches!(
                            result,
                            Some(Err(AcquireFailure {
                                kind: AcquireErrorKind::LocalTimeout,
                                ..
                            }))
                        );
                        let _ = deadline_send.send(local_timeout);
                    }
                    // Keep paused time from auto-advancing before the acknowledgement.
                    tokio::task::yield_now().await;
                }
            });
        }));
        thread_owner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        runtime.shutdown_timeout(LOCAL_WATCHDOG);
        let _ = done_send.send(outcome.is_ok());
    });
    let startup_started = std::time::Instant::now();
    let caller = caller_receive.recv_timeout(LOCAL_WATCHDOG);
    let startup_seconds = startup_started.elapsed().as_secs_f64();
    let entry_started = std::time::Instant::now();
    let entry = loop {
        match entered_receive.try_recv() {
            Ok(value) => break Some(value),
            Err(oneshot::error::TryRecvError::Closed) => break None,
            Err(oneshot::error::TryRecvError::Empty)
                if entry_started.elapsed() < LOCAL_WATCHDOG =>
            {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(_) => break None,
        }
    };
    let entry_seconds = entry_started.elapsed().as_secs_f64();
    let at_entry = (
        TERRAIN_BLOCKING_JOBS.load(Ordering::SeqCst),
        source.has_pending_local_work(),
        other.has_pending_local_work(),
    );
    let off_executor = entry
        .as_ref()
        .zip(caller.as_ref().ok())
        .is_some_and(|((worker, _), caller)| worker != caller);
    let deadline_started = std::time::Instant::now();
    let local_timeout = if off_executor {
        advance.store(true, Ordering::SeqCst);
        deadline_receive
            .recv_timeout(LOCAL_WATCHDOG)
            .unwrap_or(false)
    } else {
        false
    };
    let deadline_seconds = deadline_started.elapsed().as_secs_f64();
    let at_deadline = (
        TERRAIN_BLOCKING_JOBS.load(Ordering::SeqCst),
        source.has_pending_local_work(),
        other.has_pending_local_work(),
    );
    let owned = entry
        .as_ref()
        .is_some_and(|(_, dropped)| !dropped.load(Ordering::SeqCst));
    drop(release);
    let cleanup_started = std::time::Instant::now();
    while entry
        .as_ref()
        .is_some_and(|(_, dropped)| !dropped.load(Ordering::SeqCst))
        && cleanup_started.elapsed() < LOCAL_WATCHDOG
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    let drained = entry
        .as_ref()
        .is_none_or(|(_, dropped)| dropped.load(Ordering::SeqCst));
    let drain_seconds = cleanup_started.elapsed().as_secs_f64();
    let shutdown_started = std::time::Instant::now();
    finish.store(true, Ordering::SeqCst);
    let outcome = done_receive.recv_timeout(LOCAL_WATCHDOG);
    let stopped = outcome == Ok(true);
    if outcome.is_ok() {
        executor.join().unwrap();
    }
    let cleanup_seconds = cleanup_started.elapsed().as_secs_f64();
    let shutdown_seconds = shutdown_started.elapsed().as_secs_f64();
    terrain_test_timing!(
        "FR3_TIMING {test} initial_drain={initial_drain_seconds:.6} startup={startup_seconds:.6} drain={drain_seconds:.6} shutdown={shutdown_seconds:.6} entry={entry_seconds:.6} deadline={deadline_seconds:.6} cleanup={cleanup_seconds:.6} total={:.6} per_step_bound=10",
        started.elapsed().as_secs_f64()
    );
    assert!(
        entry.is_some(),
        "assertion failed: {test}: real cache stage entry completes within the bound"
    );
    assert!(
        off_executor,
        "assertion failed: {test}: real cache stage runs off the caller executor"
    );
    assert!(
        quiescent == 0 && at_entry == (1, true, true) && at_deadline == (1, true, true),
        "assertion failed: {test}: held local stage remains counted as owned through its deadline; baseline={quiescent} entry={at_entry:?} deadline={at_deadline:?}"
    );
    assert!(
        owned,
        "assertion failed: {test}: held local job is not dropped before release"
    );
    assert!(
        local_timeout,
        "assertion failed: {test}: real cache acquisition reports LocalTimeout at the local deadline"
    );
    assert!(
        drained && stopped,
        "assertion failed: {test}: released local stage drains and executor stops within the bound"
    );
    assert!(
        !TERRAIN_LOCAL_HOLD
            .lock()
            .unwrap()
            .iter()
            .any(|(_, held, ..)| *held == tile),
        "assertion failed: {test}: local stage hold is consumed or removed"
    );
}

#[test]
fn c9_real_cache_cachedstate_deadline_and_ownership() {
    check_local_stage(
        "c9_real_cache_cachedstate_deadline_and_ownership",
        TerrainLocalStage::CachedState,
        "N34E078",
    );
}

#[test]
fn c9_real_cache_get_deadline_and_ownership() {
    check_local_stage(
        "c9_real_cache_get_deadline_and_ownership",
        TerrainLocalStage::Get,
        "N34E079",
    );
}

#[test]
fn c9_real_cache_hash_deadline_and_ownership() {
    check_local_stage(
        "c9_real_cache_hash_deadline_and_ownership",
        TerrainLocalStage::Hash,
        "N34E080",
    );
}

#[test]
fn c9_real_cache_get_revalidates_live_admission() {
    supervise_cache_test(
        "c9_real_cache_get_revalidates_live_admission",
        false,
        async move {
            let (_dir, store, _) = fixture(false);
            let tile = TileId::parse("N35E079").unwrap();
            let (_root, source) = cached_source(&store, tile);
            let caller = std::thread::current().id();
            let (entered_send, entered_receive) = oneshot::channel();
            let (release_send, release_receive) = std::sync::mpsc::sync_channel(1);
            let _release = CacheTestRelease {
                send: release_send.clone(),
                local: Some(tile),
                hash: None,
            }
            .registered();
            TERRAIN_LOCAL_HOLD.lock().unwrap().push((
                TerrainLocalStage::Get,
                tile,
                caller,
                entered_send,
                release_receive,
            ));
            let run = RunWriteGuard::for_run(None);
            let runtime = RuntimeAdmissionCheck::new(run.clone());
            let clock = ManualClock::new();
            let mut state = TerrainWorkerState::default();
            let connection = store.open().unwrap();
            let version = data_version(&connection);
            let acquire = acquire_group(&mut state, &source, &runtime, &clock, tile);
            let revoke = async {
                let worker = entered_receive.await.unwrap();
                run.fence.set();
                let _ = release_send.send(());
                worker
            };
            let (result, (worker, _dropped)) = tokio::join!(acquire, revoke);
            assert_ne!(worker, caller);
            assert!(matches!(result, GroupAcquisition::AccessDenied));
            assert_eq!(state.breaker.lock().unwrap().state, BreakerState::Closed);
            assert!(state.memo.entries.is_empty());
            assert_eq!(data_version(&connection), version);
            assert!(run.audit().refused("terrain-cache") > 0);

            // C9 R20: a queued permit request loses its reply owner, not its receiver.
            let (send, mut receive) = mpsc::unbounded_channel();
            let relay = TerrainGuardRelay(send);
            let task = tokio::task::spawn_blocking(move || relay.assert_egress_allowed());
            let queue_started = std::time::Instant::now();
            let reply = timeout(Duration::from_secs(10), receive.recv())
                .await
                .labelled_ok(
                    "c9_real_cache_get_revalidates_live_admission",
                    "relay request arrives within the bound",
                );
            terrain_test_timing!(
                "FR3_TIMING c9_real_cache_get_revalidates_live_admission relay_queue={:.6} bound=10",
                queue_started.elapsed().as_secs_f64()
            );
            assert!(
                reply.is_some(),
                "assertion failed: c9_real_cache_get_revalidates_live_admission: relay request is queued before reply disconnection"
            );
            drop(reply);
            let reply_started = std::time::Instant::now();
            let result = timeout(Duration::from_secs(10), task)
                .await
                .labelled_ok(
                    "c9_real_cache_get_revalidates_live_admission",
                    "disconnected relay completes within the bound",
                )
                .labelled_ok(
                    "c9_real_cache_get_revalidates_live_admission",
                    "disconnected relay task completes",
                );
            terrain_test_timing!(
                "FR3_TIMING c9_real_cache_get_revalidates_live_admission relay_reply={:.6} bound=10",
                reply_started.elapsed().as_secs_f64()
            );
            assert!(
                result.is_err(),
                "assertion failed: c9_real_cache_get_revalidates_live_admission: a disconnected admission reply denies the blocking cache operation"
            );
            drop(receive);
        },
    );
}

#[cfg(unix)]
#[test]
fn c9_detached_cached_probe_never_repairs_the_store() {
    supervise_cache_test(
        "c9_detached_cached_probe_never_repairs_the_store",
        true,
        async move {
            use std::os::unix::fs::PermissionsExt;

            let (_dir, store, _) = fixture(false);
            let tile = TileId::parse("N36E081").unwrap();
            let (_root, source) = cached_source(&store, tile);
            let database = store.database_path();
            // C9 R4, C9 R14: a read-only probe must leave even repairable metadata alone
            // after the acquisition owner timed out and its runtime admission was lost.
            std::fs::set_permissions(database, std::fs::Permissions::from_mode(0o640)).unwrap();
            let writable = store
                .instrumentation()
                .writable_opens
                .load(Ordering::SeqCst);
            let readonly = store
                .instrumentation()
                .read_only_opens
                .load(Ordering::SeqCst);
            let caller = std::thread::current().id();
            let (entered_send, entered_receive) = oneshot::channel();
            let (release_send, release_receive) = std::sync::mpsc::sync_channel(1);
            let _release = CacheTestRelease {
                send: release_send.clone(),
                local: Some(tile),
                hash: None,
            }
            .registered();
            TERRAIN_LOCAL_HOLD.lock().unwrap().push((
                TerrainLocalStage::CachedState,
                tile,
                caller,
                entered_send,
                release_receive,
            ));
            let run = RunWriteGuard::for_run(None);
            let runtime = RuntimeAdmissionCheck::new(run.clone());
            runtime.check().unwrap();
            let acquire = timeout(
                K_LOCAL_TILE_BUDGET + Duration::from_millis(1),
                source.acquire(tile, &runtime, &runtime),
            );
            let revoke = async {
                let worker = entered_receive.await.unwrap();
                run.fence.set();
                tokio::time::advance(K_LOCAL_TILE_BUDGET + Duration::from_millis(1)).await;
                worker
            };
            let (result, (worker, dropped)) = tokio::join!(acquire, revoke);
            let owned = !dropped.load(Ordering::SeqCst);
            let _ = release_send.send(());
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            assert_ne!(worker, caller);
            assert!(owned);
            assert!(matches!(
                result,
                Ok(Err(AcquireFailure {
                    kind: AcquireErrorKind::LocalTimeout,
                    ..
                }))
            ));
            assert!(run.fence.is_set());
            assert_eq!(
                std::fs::metadata(database).unwrap().permissions().mode() & 0o777,
                0o640
            );
            assert_eq!(
                store
                    .instrumentation()
                    .writable_opens
                    .load(Ordering::SeqCst),
                writable
            );
            assert_eq!(
                store
                    .instrumentation()
                    .read_only_opens
                    .load(Ordering::SeqCst),
                readonly + 1
            );
        },
    );
}

#[tokio::test]
async fn c9_window_cut_requires_rows_beyond_upper() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_dir, store, v) = fixture(false);
    seed(&store, v, (1..=2).map(|id| point(id, 1, 0, 20)).collect());
    let vehicle = store.terrain_vehicles().unwrap().remove(0);
    let (window, cut) = store
        .terrain_forward_window_with_eligibility(&vehicle, 2, 10, &mut HashMap::new())
        .unwrap();
    assert_eq!(window.examined_through, 2);
    assert!(!cut, "exactly two remaining rows need no continuation");
    commit(&store, v, vec![], vec![point(3, 1, 0, 20)]);
    let (window, cut) = store
        .terrain_forward_window_with_eligibility(&vehicle, 2, 10, &mut HashMap::new())
        .unwrap();
    assert_eq!(window.examined_through, 2);
    assert!(cut);
}

#[test]
fn c9_window_remains_uses_a_primary_key_probe() {
    let (_dir, store, v) = fixture(false);
    let conn = store.open().unwrap();
    let sql = format!(
        "EXPLAIN QUERY PLAN {}",
        crate::db::TERRAIN_WINDOW_REMAINS_SQL
    );
    let mut query = conn.prepare(&sql).unwrap();
    let plan: Vec<String> = query
        .query_map(params![v.to_string(), 0], |row| row.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let detail = plan
        .iter()
        .find(|line| line.contains("materialised_positions"))
        .unwrap();
    assert!(
        detail.contains("SEARCH") && detail.contains("COVERING INDEX"),
        "{plan:?}"
    );
    assert!(
        detail.contains("vehicle_id=? AND position_id>?"),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(
            |line| line.contains("SCAN materialised_positions") || line.contains("TEMP B-TREE")
        ),
        "{plan:?}"
    );
}

#[tokio::test]
async fn c9_wrote_tracks_boundary_retirement_and_outcomes() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_dir, store, v) = fixture(false);
    let imported = import_source(&store);
    watermark(&store, imported, v, 0, T0);
    let source = Source::new(&Arc::new(ManualClock::new()));
    let mut state = TerrainWorkerState::default();
    let r = pass(&store, &source, &mut state).await;
    assert!(r.wrote);
    assert_eq!(
        (r.retired, r.filled, r.failed, source.count()),
        (0, 0, 0, 0)
    );
    assert!(!pass(&store, &source, &mut state).await.wrote);
    let mut p = point(1, 1, 0, 20);
    p.elevation = Some(5);
    seed(&store, v, vec![p]);
    plant(&store, v, 1, "failed", None, T1 - 1);
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE terrain_enrichment_state SET cursor_position_id=1",
            [],
        )
        .unwrap();
    let r = pass(&store, &source, &mut state).await;
    assert_eq!((r.retired, r.filled, r.failed), (1, 0, 0));
    assert!(r.wrote);
    for script in [
        Script::Value(0),
        Script::Value(i16::MIN),
        Script::BadResponse,
    ] {
        let (_dir, store, v) = fixture(false);
        seed(&store, v, vec![point(1, 1, 0, 20)]);
        plant(&store, v, 1, "failed", None, T1 - 1);
        store.open().unwrap().execute(
            "INSERT INTO terrain_enrichment_state(vehicle_id,cursor_position_id,updated_at_ms) VALUES (?1,1,?2)",
            params![v.to_string(), T1],
        ).unwrap();
        let source = Source::new(&Arc::new(ManualClock::new()));
        source.script("N10E020", script);
        let r = pass(&store, &source, &mut TerrainWorkerState::default()).await;
        assert_eq!(r.filled + r.void + r.failed, 1);
        assert_eq!(cursor(&store, v), 1);
        assert_eq!(source.count(), 1);
        assert!(
            r.wrote,
            "assertion failed: c9_wrote_tracks_boundary_retirement_and_outcomes: retry-only outcomes write without advancing a cursor"
        );
    }
    // C9 R24: a stale retry-only outcome commits neither provenance nor cursor.
    let (_dir, store, v) = fixture(false);
    seed(&store, v, vec![point(1, 1, 0, 20)]);
    plant(&store, v, 1, "failed", None, T1 - 1);
    store.open().unwrap().execute(
        "INSERT INTO terrain_enrichment_state(vehicle_id,cursor_position_id,updated_at_ms) VALUES (?1,1,?2)",
        params![v.to_string(), T1],
    ).unwrap();
    let before = table(&store, "terrain_elevation_provenance");
    let source = Source::new(&Arc::new(ManualClock::new()));
    let changed = store.clone();
    *source.after.lock().unwrap() = Some(Box::new(move |_| {
        changed.open().unwrap().execute(
            "UPDATE materialised_positions SET position_json=json_set(position_json,'$.latitude',10.6)",
            [],
        ).unwrap();
    }));
    let r = pass(&store, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(
        (r.stale, r.filled, r.void, r.failed, r.retired),
        (1, 0, 0, 0, 0)
    );
    assert!(!r.wrote, "stale-only retry commits no worker progress");
    assert_eq!(cursor(&store, v), 1);
    assert_eq!(table(&store, "terrain_elevation_provenance"), before);

    // C9 R24, ST26: deleting a retry candidate yields stale_dropped.
    let (_dir, store, v) = fixture(false);
    seed(&store, v, vec![point(1, 1, 0, 20)]);
    plant(&store, v, 1, "failed", None, T1 - 1);
    store.open().unwrap().execute(
        "INSERT INTO terrain_enrichment_state(vehicle_id,cursor_position_id,updated_at_ms) VALUES (?1,1,?2)",
        params![v.to_string(), T1],
    ).unwrap();
    let source = Source::new(&Arc::new(ManualClock::new()));
    let changed = store.clone();
    let snapshot = Arc::new(Mutex::new(None));
    let captured = snapshot.clone();
    *source.after.lock().unwrap() = Some(Box::new(move |_| {
        changed
            .open()
            .unwrap()
            .execute(
                "DELETE FROM materialised_positions WHERE vehicle_id=?1 AND position_id=1",
                params![v.to_string()],
            )
            .unwrap();
        *captured.lock().unwrap() = Some(durable(&changed));
    }));
    let r = pass(&store, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(
        (r.stale, r.filled, r.void, r.failed, r.retired),
        (1, 0, 0, 0, 0)
    );
    assert!(
        !r.wrote,
        "assertion failed: c9_wrote_tracks_boundary_retirement_and_outcomes: stale-dropped retry-only work does not count as a worker write"
    );
    assert_eq!(cursor(&store, v), 1);
    assert_eq!(durable(&store), snapshot.lock().unwrap().take().unwrap());
}

#[tokio::test]
async fn c9_every_early_exit_logs_once() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let log = super::super::DecisionLog::start();
    let (_dir, store, _) = fixture(true);
    let clock = ManualClock::new();
    let r = run_terrain_tidy_only_pass(
        &store,
        &mut TerrainWorkerState::default(),
        &Access(false),
        &clock,
    )
    .await
    .unwrap();
    assert!(r.access_denied && !r.full_batch && !r.wrote);
    store
        .open()
        .unwrap()
        .execute("DELETE FROM terrain_tidy_state", [])
        .unwrap();
    assert!(
        run_terrain_tidy_only_pass(
            &store,
            &mut TerrainWorkerState::default(),
            &Access(true),
            &clock
        )
        .await
        .is_err()
    );
    assert_eq!(
        log.contents()
            .lines()
            .filter(|line| line.contains("terrain pass complete"))
            .count(),
        2
    );
}
