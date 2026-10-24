// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

#[tokio::test]
async fn c9_waiting_pass_rechecks_access_before_marker() {
    for tidy_only in [false, true] {
        let (_dir, store, vehicle) = fixture(false);
        seed(&store, vehicle, vec![point(1, 1, 0, 20)]);
        let clock = Arc::new(ManualClock::new());
        struct Held {
            source: Source,
            entered: tokio::sync::Notify,
            resume: tokio::sync::Notify,
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
                    self.entered.notify_one();
                    self.resume.notified().await;
                    self.source.acquire(tile, guard, inner).await
                })
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
            access.checked.notified().await;
            run.fence.set();
            held.resume.notify_one();
        };
        let (first, second, ()) = tokio::join!(first, second, revoke);
        assert_eq!(first.unwrap().filled, 1);
        assert!(second.unwrap().access_denied);
        assert_eq!(held.source.count(), 1);
        assert_eq!(crate::db::TERRAIN_MARKER_READS.with(|n| n.get()) - reads, 1);
        assert_eq!(run.audit().refused("terrain-pass"), 1);
    }
}

async fn check_fenced_mutation(path: &str, denied_at: usize) {
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
    assert!(run.audit().tripped(), "{path}");
    assert!(report.access_denied, "{path}");
    assert!(
        !report.full_batch,
        "{path} refusal must not request continuation"
    );
    assert_eq!(report.failed, 0, "{path}");
    assert_eq!(report.wrote, path == "outcomes", "{path}");
    assert_eq!(
        (durable(&store), store.terrain_tidy_progress().unwrap()),
        stamp.lock().unwrap().take().unwrap(),
        "{path}"
    );
    if path != "outcomes" {
        assert_eq!(
            (durable(&store), store.terrain_tidy_progress().unwrap()),
            initial,
            "{path} wrote before the refusal"
        );
    }
    assert_eq!(
        report.filled,
        if path == "outcomes" { 250 } else { 0 },
        "{path}"
    );
    assert_eq!(
        cursor(&store, vehicle),
        if path == "outcomes" { 250 } else { 0 },
        "{path}"
    );
}

#[tokio::test]
async fn c9_fence_tidy() {
    check_fenced_mutation("tidy", 3).await;
}

#[tokio::test]
async fn c9_fence_boundary() {
    check_fenced_mutation("boundary", 3).await;
}

#[tokio::test]
async fn c9_fence_retire() {
    check_fenced_mutation("retire", 3).await;
}

#[tokio::test]
async fn c9_fence_cursor() {
    check_fenced_mutation("cursor", 3).await;
}

#[tokio::test]
async fn c9_fence_outcomes() {
    check_fenced_mutation("outcomes", 4).await;
}

#[tokio::test]
async fn c9_mixed_groups_retry_ties_and_sample_order() {
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

async fn check_local_stage(stage: TerrainLocalStage, name: &str) {
    let (_dir, store, _) = fixture(false);
    let tile = TileId::parse(name).unwrap();
    let (_root, source) = cached_source(&store, tile);
    let caller = std::thread::current().id();
    let (entered_send, entered_receive) = oneshot::channel();
    let (release_send, release_receive) = std::sync::mpsc::sync_channel(1);
    TERRAIN_LOCAL_HOLD
        .lock()
        .unwrap()
        .push((stage, tile, caller, entered_send, release_receive));
    let acquire = timeout(
        K_LOCAL_TILE_BUDGET,
        source.acquire(tile, &Permit(false), &Permit(true)),
    );
    let advance = async {
        let worker = entered_receive.await.unwrap();
        tokio::time::advance(K_LOCAL_TILE_BUDGET).await;
        worker
    };
    let (result, (worker, dropped)) = tokio::join!(acquire, advance);
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
    assert!(
        !TERRAIN_LOCAL_HOLD
            .lock()
            .unwrap()
            .iter()
            .any(|(_, held, ..)| *held == tile)
    );
}

#[tokio::test(start_paused = true)]
async fn c9_real_cache_cachedstate_deadline_and_ownership() {
    check_local_stage(TerrainLocalStage::CachedState, "N34E078").await;
}

#[tokio::test(start_paused = true)]
async fn c9_real_cache_get_deadline_and_ownership() {
    check_local_stage(TerrainLocalStage::Get, "N34E079").await;
}

#[tokio::test(start_paused = true)]
async fn c9_real_cache_hash_deadline_and_ownership() {
    check_local_stage(TerrainLocalStage::Hash, "N34E080").await;
}

#[tokio::test]
async fn c9_real_cache_get_revalidates_live_admission() {
    let (_dir, store, _) = fixture(false);
    let tile = TileId::parse("N35E079").unwrap();
    let (_root, source) = cached_source(&store, tile);
    let caller = std::thread::current().id();
    let (entered_send, entered_receive) = oneshot::channel();
    let (release_send, release_receive) = std::sync::mpsc::sync_channel(1);
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
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn c9_detached_cached_probe_never_repairs_the_store() {
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
        K_LOCAL_TILE_BUDGET,
        source.acquire(tile, &runtime, &runtime),
    );
    let revoke = async {
        let worker = entered_receive.await.unwrap();
        run.fence.set();
        tokio::time::advance(K_LOCAL_TILE_BUDGET).await;
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
}

#[tokio::test]
async fn c9_window_cut_requires_rows_beyond_upper() {
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
    for script in [Script::Value(0), Script::BadResponse] {
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
        assert_eq!(r.filled + r.failed, 1);
        assert_eq!(cursor(&store, v), 1);
        assert_eq!(source.count(), 1);
        assert!(
            r.wrote,
            "retry-only outcomes write without advancing a cursor"
        );
    }
}

#[tokio::test]
async fn c9_every_early_exit_logs_once() {
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
