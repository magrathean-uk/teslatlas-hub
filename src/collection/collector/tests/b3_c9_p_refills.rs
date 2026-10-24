// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

#[tokio::test]
async fn c9_st56_st76_refills() {
    let (_d, s, v) = fixture(false);
    seed(&s, v, st27_rows());
    let clock = Arc::new(ManualClock::new());
    let source = st27_source(&clock);
    let mut state = TerrainWorkerState::default();
    pass(&s, &source, &mut state).await;
    let mut p = point(2, 1, 3, 20);
    p.date_ms = T0 + 2000;
    clock.set(T1 + 10000);
    commit(&s, v, Vec::new(), vec![p]);
    assert_eq!(elevation(&s, v, 2), None);
    let row = provenance(&s, v, 2);
    assert_eq!(
        (row.0, row.1, row.2, row.4),
        ("failed".into(), 1, Some("position_rewritten".into()), T1)
    );
    assert_eq!(pair(&s, v, 1), (Some(6), Some(0)));
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, source.count()), (1, 2));
    assert_eq!(elevation(&s, v, 2), Some(150));
    assert_eq!(provenance(&s, v, 2).1, 2);
    assert_eq!(pair(&s, v, 1), (Some(55), Some(49)));
    // C9 ST76 (a): lifecycle replacement with null elevation is due for a refill.
    let (_d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 0, 20), point(2, 1, 6, 20)]);
    let source = st27_source(&clock);
    let mut state = TerrainWorkerState::default();
    pass(&s, &source, &mut state).await;
    let unchanged = provenance(&s, v, 2);
    commit(&s, v, Vec::new(), vec![point(1, 1, 0, 20)]);
    assert_eq!(elevation(&s, v, 1), None);
    assert_eq!(provenance(&s, v, 1).2, Some("position_rewritten".into()));
    let r = pass(&s, &source, &mut state).await;
    assert_eq!(r.filled, 1);
    assert_eq!(elevation(&s, v, 1), Some(100));
    assert_eq!(provenance(&s, v, 1).1, 2);
    assert_eq!(provenance(&s, v, 2), unchanged);
    // C9 ST76 (e): refill uses the relocated drive and leaves its old drive alone.
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        vec![
            point(1, 1, 0, 20),
            point(2, 1, 6, 20),
            point(3, 2, 12, 20),
            point(4, 2, 18, 20),
        ],
    );
    let source = Source::new(&clock);
    source.script(
        "N10E020",
        Script::Nodes(vec![(0, 100), (6, 110), (12, 120), (18, 130)]),
    );
    let mut state = TerrainWorkerState::default();
    pass(&s, &source, &mut state).await;
    assert_eq!(pair(&s, v, 1), (Some(10), Some(0)));
    assert_eq!(pair(&s, v, 2), (Some(10), Some(0)));
    let mut p = point(2, 2, 6, 20);
    p.elevation = Some(110);
    commit(&s, v, Vec::new(), vec![p]);
    assert_eq!(elevation(&s, v, 2), None);
    assert_eq!(pair(&s, v, 1), (None, None));
    assert_eq!(pair(&s, v, 2), (Some(10), Some(0)));
    assert_eq!(
        scalar(
            &s,
            "SELECT drive_id FROM terrain_elevation_provenance WHERE position_id=2"
        ),
        2
    );
    clear_log(&s);
    let old = provenance(&s, v, 1);
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, source.count()), (1, 2));
    assert_eq!(elevation(&s, v, 2), Some(110));
    assert_eq!(provenance(&s, v, 2).1, 2);
    assert_eq!(pair(&s, v, 1), (None, None));
    assert_eq!(pair(&s, v, 2), (Some(20), Some(0)));
    assert_eq!(provenance(&s, v, 1), old);
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='drive' AND entity_id=2"
        ),
        1
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='drive' AND entity_id=1"
        ),
        0
    );
}

#[tokio::test]
async fn c9_r7_shared_memo_and_stale_cursor() {
    let (_d, s, v) = fixture(false);
    seed(&s, v, (1..=600).map(|id| point(id, 1, 0, 20)).collect());
    for id in 1..=10 {
        plant(&s, v, id, "failed", None, T1 - 1);
    }
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let before = crate::db::TERRAIN_ELIGIBILITY_READS.with(|n| n.get());
    let r = pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(r.filled, 600);
    assert_eq!(
        crate::db::TERRAIN_ELIGIBILITY_READS.with(|n| n.get()) - before,
        1
    );
    // C9 R9: a stale retry in a mixed tile must not cap later forward chunks.
    let (_d, mixed, mv) = fixture(false);
    seed(
        &mixed,
        mv,
        vec![point(1, 1, 0, 20), point(2, 1, 0, 20), point(3, 1, 0, 21)],
    );
    plant(&mixed, mv, 1, "failed", None, T1 - 1);
    let mixed_source = Source::new(&clock);
    let copy = mixed.clone();
    *mixed_source.after.lock().unwrap() = Some(Box::new(move |tile| {
        if tile.name() == "N10E020" {
            copy.open().unwrap().execute("UPDATE materialised_positions SET position_json=json_set(position_json,'$.latitude',10.6) WHERE position_id=1", []).unwrap();
        }
    }));
    let mixed_report = pass(&mixed, &mixed_source, &mut TerrainWorkerState::default()).await;
    assert_eq!(
        (mixed_report.filled, mixed_report.stale, cursor(&mixed, mv)),
        (2, 1, 3)
    );
    assert_eq!(provenance(&mixed, mv, 1).1, 1);

    // C9 R9, C9 R15: an earlier stale-held row caps later chunks too.
    let (_d, s, v) = fixture(false);
    seed(&s, v, (1..=600).map(|id| point(id, 1, 0, 20)).collect());
    let source = Source::new(&clock);
    let copy = s.clone();
    *source.after.lock().unwrap() = Some(Box::new(move |_| {
        copy.open().unwrap().execute("UPDATE materialised_positions SET position_json=json_set(position_json,'$.latitude',10.6) WHERE position_id=2",[]).unwrap();
    }));
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, r.stale, cursor(&s, v)), (599, 1, 1));
    assert_eq!(elevation(&s, v, 2), None);
    source.after.lock().unwrap().take();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, cursor(&s, v)), (1, 600));
    // C9 R35: a stale out-of-range sample does not count another row's void.
    let (_d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 0, 20), point(2, 1, 10, 20)]);
    let source = Source::new(&clock);
    source.script("N10E020", Script::Nodes(vec![(0, 10000), (10, i16::MIN)]));
    let copy = s.clone();
    *source.after.lock().unwrap() = Some(Box::new(move |_| {
        copy.open().unwrap().execute("UPDATE materialised_positions SET position_json=json_set(position_json,'$.latitude',10.6) WHERE position_id=1",[]).unwrap();
    }));
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.void, r.out_of_range, r.stale), (1, 0, 1));
    source.after.lock().unwrap().take();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.void, r.out_of_range, r.stale), (1, 1, 0));
}

#[tokio::test]
async fn c9_runtime_fence_before_commit() {
    let (_d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 0, 20)]);
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let run = RunWriteGuard::for_run(None);
    let runtime = RuntimeAdmissionCheck::new(run.clone());
    let fence = run.fence.clone();
    *source.after.lock().unwrap() = Some(Box::new(move |_| fence.set()));
    let before = durable(&s);
    let r = run_terrain_enrichment_pass(
        &s,
        &source,
        &mut TerrainWorkerState::default(),
        &runtime,
        &runtime,
        clock.as_ref(),
    )
    .await
    .unwrap();
    assert!(r.access_denied);
    assert_eq!(r.failed, 0);
    assert_eq!(before, durable(&s));
    assert!(runtime.assert_egress_allowed().is_err());
    // C9 R4: a revoked fence also denies cached acquisitions and never resets.
    source.cached("N10E020");
    let count = source.count();
    let r = run_terrain_enrichment_pass(
        &s,
        &source,
        &mut TerrainWorkerState::default(),
        &runtime,
        &runtime,
        clock.as_ref(),
    )
    .await
    .unwrap();
    assert!(r.access_denied);
    assert_eq!(source.count(), count);
}

#[tokio::test(start_paused = true)]
async fn c9_blocking_timeout_ownership() {
    let (entered_send, entered_receive) = oneshot::channel();
    let (release_send, release_receive) = std::sync::mpsc::sync_channel(1);
    let (ended_send, ended_receive) = oneshot::channel();
    let caller = std::thread::current().id();
    let operation = terrain_cache_blocking(&Permit(true), move |relay| async move {
        let tile = HgtTile::from_bytes("N10E020", vec![0; SRTM3_SIDE * SRTM3_SIDE * 2]).unwrap();
        let _ = entered_send.send((std::thread::current().id(), terrain_job_drop_for_test()));
        release_receive.recv().unwrap();
        let refused = relay.assert_egress_allowed().is_err();
        drop(tile);
        let _ = ended_send.send(refused);
        Ok(())
    });
    let run = timeout(Duration::from_secs(30), operation);
    let entered = async {
        let (worker, dropped) = entered_receive.await.unwrap();
        assert_ne!(worker, caller);
        tokio::time::advance(Duration::from_secs(30)).await;
        dropped
    };
    let (expired, dropped) = tokio::join!(run, entered);
    assert!(expired.is_err());
    assert!(!dropped.load(Ordering::SeqCst));
    struct ReadyCache(AtomicUsize);
    impl TileCache for ReadyCache {
        fn has_cached_tile<'a>(
            &'a self,
            _: &'a TileId,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async { true })
        }
        fn lookup<'a>(
            &'a self,
            _: f64,
            _: f64,
            _: Duration,
            _: &'a dyn EgressGuard,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            crate::terrain_cache::TerrainLookupResult,
                            TerrainCacheError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                let tile =
                    HgtTile::from_bytes("N10E020", vec![0; SRTM3_SIDE * SRTM3_SIDE * 2]).unwrap();
                Ok(crate::terrain_cache::TerrainLookupResult {
                    elevation_m: None,
                    tile_name: "N10E020".into(),
                    tile_hash: tile.sha256_hex().unwrap(),
                    dataset_source: "aws".into(),
                    dataset_version: TERRAIN_DATASET_VERSION.into(),
                })
            })
        }
        fn get<'a>(
            &'a self,
            _: TileId,
            _: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<HgtTile, TerrainCacheError>> + Send + 'a>> {
            Box::pin(async {
                Ok(HgtTile::from_bytes("N10E020", vec![0; SRTM3_SIDE * SRTM3_SIDE * 2]).unwrap())
            })
        }
    }

    // C9 ST59: other tests can have their own independent cache tasks in flight.
    struct FirstLookup<'a, C: TileCache>(&'a CacheTileSource<C>, AtomicBool);
    impl<C: TileCache> TileSource for FirstLookup<'_, C> {
        fn has_pending_local_work(&self) -> bool {
            self.1.swap(true, Ordering::SeqCst) && self.0.has_pending_local_work()
        }
        fn acquire<'a>(
            &'a self,
            t: TileId,
            g: &'a dyn EgressGuard,
            i: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>
        {
            self.0.acquire(t, g, i)
        }
    }
    let (_d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 0, 20)]);
    let clock = Arc::new(ManualClock::new());
    let source = CacheTileSource {
        cache: ReadyCache(AtomicUsize::new(0)),
    };
    let mut state = TerrainWorkerState::default();
    let r = run_terrain_enrichment_pass(
        &s,
        &source,
        &mut state,
        &Access(true),
        &Permit(true),
        clock.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(
        (
            r.filled,
            r.failed,
            r.deferred,
            source.cache.0.load(Ordering::SeqCst),
            cursor(&s, v)
        ),
        (0, 0, 1, 0, 0)
    );
    release_send.send(()).unwrap();
    assert!(ended_receive.await.unwrap());
    assert!(!r.breaker_deferred);
    assert!(r.local_deferred);
    while !dropped.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    let r = run_terrain_enrichment_pass(
        &s,
        &FirstLookup(&source, AtomicBool::new(false)),
        &mut state,
        &Access(true),
        &Permit(true),
        clock.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!((r.filled, source.cache.0.load(Ordering::SeqCst)), (1, 1));

    // C9 R13, C9 R28: the actual hash deadline retains ownership until release.
    struct HashCache(AtomicUsize);
    impl TileCache for HashCache {
        fn has_cached_tile<'a>(
            &'a self,
            _: &'a TileId,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async { true })
        }
        fn lookup<'a>(
            &'a self,
            lat: f64,
            lon: f64,
            _: Duration,
            _: &'a dyn EgressGuard,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            crate::terrain_cache::TerrainLookupResult,
                            TerrainCacheError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                let id = terrain_tile_for(lat, lon).unwrap();
                let tile =
                    HgtTile::from_bytes(&id.name(), vec![0; SRTM3_SIDE * SRTM3_SIDE * 2]).unwrap();
                Ok(crate::terrain_cache::TerrainLookupResult {
                    elevation_m: None,
                    tile_name: id.name(),
                    tile_hash: tile.sha256_hex().unwrap(),
                    dataset_source: "aws".into(),
                    dataset_version: TERRAIN_DATASET_VERSION.into(),
                })
            })
        }
        fn get<'a>(
            &'a self,
            id: TileId,
            _: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<HgtTile, TerrainCacheError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(HgtTile::from_bytes(&id.name(), vec![0; SRTM3_SIDE * SRTM3_SIDE * 2]).unwrap())
            })
        }
    }
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=2)
            .map(|id| {
                let mut p = point(id, 1, 0, 76 + id as i32);
                p.latitude = 33.5;
                p
            })
            .collect(),
    );
    let (entered_send, entered_receive) = oneshot::channel();
    let (release_send, release_receive) = std::sync::mpsc::sync_channel(1);
    *TERRAIN_HASH_HOLD.lock().unwrap() = Some((
        TileId::parse("N33E077").unwrap(),
        entered_send,
        release_receive,
    ));
    let source = CacheTileSource {
        cache: HashCache(AtomicUsize::new(0)),
    };
    let mut state = TerrainWorkerState::default();
    let owner = FirstLookup(&source, AtomicBool::new(false));
    let run = run_terrain_enrichment_pass(
        &s,
        &owner,
        &mut state,
        &Access(true),
        &Permit(true),
        clock.as_ref(),
    );
    let advance = async {
        let dropped = entered_receive.await.unwrap();
        tokio::time::advance(K_LOCAL_TILE_BUDGET).await;
        dropped
    };
    let (result, dropped) = tokio::join!(run, advance);
    let r = result.unwrap();
    assert_eq!(
        (
            r.filled,
            r.failed,
            r.deferred,
            source.cache.0.load(Ordering::SeqCst)
        ),
        (0, 1, 1, 1)
    );
    assert!(!dropped.load(Ordering::SeqCst));
    assert_eq!(provenance(&s, v, 1).2, Some("io".into()));
    assert_eq!(state.breaker.lock().unwrap().state, BreakerState::Closed);
    release_send.send(()).unwrap();
    while !dropped.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    assert!(TERRAIN_HASH_HOLD.lock().unwrap().is_none());
    assert!(
        !r.breaker_deferred,
        "local ownership does not defer for the breaker"
    );
    assert!(r.local_deferred);
}
