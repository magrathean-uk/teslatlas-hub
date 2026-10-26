// SPDX-License-Identifier: AGPL-3.0-only

use super::super::terrain_pass::{SensitiveAccessCheck, run_terrain_enrichment_pass};
use super::super::terrain_worker::{TerrainWorkerDeps, spawn_terrain_worker_with};
use super::super::*;
use crate::db::{IMPORT_SOURCE_KIND, LifecycleCommit, TileProvenance};
use crate::hub_pack::{ProjectionDrive, ProjectionPosition};
use crate::terrain::{SRTM3_SIDE, TERRAIN_DATASET_VERSION};
use crate::test_assertions::LabelledResult;
use rusqlite::params;
use serde_json::json;
use std::sync::atomic::AtomicUsize;

const T0: i64 = 1_760_000_000_000;
const NOW: i64 = T0 + 60_000_000;
const WORKER_OBSERVATION_HORIZON: Duration = Duration::from_secs(30 * 24 * 60 * 60);
use super::TERRAIN_PASS_TEST_SERIAL as SERIAL;

struct TokioOffsetClock {
    start: Instant,
    failure: AtomicBool,
}
impl TokioOffsetClock {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            failure: AtomicBool::new(false),
        }
    }
}
impl TerrainClock for TokioOffsetClock {
    fn now_ms(&self) -> Result<i64, CollectorError> {
        if self.failure.load(Ordering::SeqCst) {
            return Err(CollectorError::InvalidReceiptTimestamp);
        }
        Ok(NOW + self.start.elapsed().as_millis() as i64)
    }
}
struct Access(AtomicBool);
impl SensitiveAccessCheck for Access {
    fn check(&self) -> Result<(), CollectorError> {
        self.0
            .load(Ordering::SeqCst)
            .then_some(())
            .ok_or(CollectorError::SensitiveAccessUnavailable)
    }
}
struct Permit;
impl EgressGuard for Permit {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        Ok(())
    }
}
struct Source {
    calls: AtomicUsize,
    network: AtomicUsize,
    block: AtomicBool,
    pending: AtomicBool,
    defer_after_acquire: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
    failures: Mutex<BTreeMap<String, &'static str>>,
}
impl Source {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            network: AtomicUsize::new(0),
            block: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            defer_after_acquire: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
            failures: Mutex::new(BTreeMap::new()),
        })
    }
    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
impl TileSource for Source {
    fn has_pending_local_work(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }
    fn acquire<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
        _inner: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            guard.assert_egress_allowed().map_err(|_| AcquireFailure {
                kind: AcquireErrorKind::Cache(TerrainCacheError::EgressDenied),
                provenance: None,
            })?;
            self.network.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            if self.block.swap(false, Ordering::SeqCst) {
                self.release.acquire().await.unwrap().forget();
            }
            let failure = self.failures.lock().unwrap().get(&tile.name()).copied();
            if let Some(code) = failure {
                let error = match code {
                    "store" => {
                        TerrainCacheError::Store(Box::new(StoreError::InvalidTerrainTileDigest))
                    }
                    "invalid_config" => TerrainCacheError::InvalidConfig,
                    "io" => TerrainCacheError::Io(std::io::Error::other("synthetic")),
                    _ => TerrainCacheError::BadResponse,
                };
                return Err(AcquireFailure {
                    kind: AcquireErrorKind::Cache(error),
                    provenance: None,
                });
            }
            if self.defer_after_acquire.load(Ordering::SeqCst) {
                self.pending.store(true, Ordering::SeqCst);
            }
            let mut bytes = Vec::with_capacity(SRTM3_SIDE * SRTM3_SIDE * 2);
            for _ in 0..SRTM3_SIDE * SRTM3_SIDE {
                bytes.extend_from_slice(&100_i16.to_be_bytes());
            }
            let hash = Sha256Digest::of_bytes(&bytes).to_string();
            Ok(AcquiredTile {
                tile: HgtTile::from_bytes(&tile.name(), bytes).unwrap(),
                provenance: TileProvenance {
                    tile_name: tile.name(),
                    tile_hash: hash,
                    dataset_source: "aws".into(),
                    dataset_version: TERRAIN_DATASET_VERSION.into(),
                },
            })
        })
    }
}
fn fixture(pending: bool) -> (tempfile::TempDir, HubStore, Uuid) {
    let dir = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(dir.path()).unwrap();
    let source = store
        .register_source(&SourceDescriptor::new("owner_api_compat", "c9-worker"), T0)
        .unwrap();
    let v = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "synthetic"), T0)
        .unwrap()
        .vehicle_id;
    if !pending {
        store.apply_terrain_tidy_chunk(250, NOW).unwrap();
    }
    (dir, store, v)
}
fn point(id: i64, drive: i64, longitude: f64, elevation: Option<i64>) -> ProjectionPosition {
    serde_json::from_value(
        json!({"id":id,"drive_id":drive,"car_id":1,"date_ms":T0+id*1000,
        "latitude":10.5,"longitude":longitude,"elevation":elevation}),
    )
    .unwrap()
}
fn seed(s: &HubStore, v: Uuid, positions: Vec<ProjectionPosition>) {
    let drives = positions
        .iter()
        .filter_map(|p| p.drive_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| {
            serde_json::from_value::<ProjectionDrive>(
                json!({"id":id,"car_id":1,"start_date_ms":T0-1,
            "end_date_ms":NOW,"ascent":77,"descent":77}),
            )
            .unwrap()
        })
        .collect();
    let state = OpenSessionState::new().encode().unwrap();
    s.commit_lifecycle_delta(&LifecycleCommit {
        vehicle_id: v,
        car_id: 1,
        open_session_json: &state,
        last_observation_id: 0,
        quarantined: false,
        updated_at_ms: NOW,
        delta: &crate::lifecycle::LifecycleDelta {
            drives,
            positions,
            ..Default::default()
        },
    })
    .unwrap();
    s.open()
        .unwrap()
        .execute_batch("DELETE FROM sync_mutations; DELETE FROM export_outbox;")
        .unwrap();
}
fn scalar(s: &HubStore, sql: &str) -> i64 {
    s.open().unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
}
fn version(c: &rusqlite::Connection) -> i64 {
    c.query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap()
}
fn plant(s: &HubStore, v: Uuid, id: i64, drive: i64, value: Option<i64>, status: &str, retry: i64) {
    s.open().unwrap().execute("INSERT INTO terrain_elevation_provenance(vehicle_id,position_id,drive_id,latitude,longitude,elevation_m,tile_name,tile_hash,dataset_source,dataset_version,status,attempts,attempted_at_ms,retry_after_ms,error_code) VALUES(?1,?2,?3,10.5,20.5,?4,'N10E020','synthetic','aws','1',?5,1,?6,?7,?8)",params![v.to_string(),id,drive,value,status,NOW,retry,(status=="failed").then_some("timeout")]).unwrap();
}
fn launch(
    s: &HubStore,
    dir: &Path,
    enabled: bool,
    source: Option<Arc<dyn TileSource>>,
    clock: Arc<dyn TerrainClock>,
    access: Arc<dyn SensitiveAccessCheck>,
) -> (TerrainWorker, mpsc::UnboundedReceiver<TerrainStatus>) {
    let config = TerrainConfig {
        enabled,
        cache_dir: Some(dir.join("cache")),
        ..Default::default()
    };
    launch_config(s, dir, config, source, clock, access)
}
fn launch_config(
    s: &HubStore,
    dir: &Path,
    config: TerrainConfig,
    source: Option<Arc<dyn TileSource>>,
    clock: Arc<dyn TerrainClock>,
    access: Arc<dyn SensitiveAccessCheck>,
) -> (TerrainWorker, mpsc::UnboundedReceiver<TerrainStatus>) {
    let (events, rx) = mpsc::unbounded_channel();
    let worker = spawn_terrain_worker_with(
        dir.into(),
        config,
        CursorKey::from_bytes([9; 32]),
        RunWriteGuard::for_run(None),
        s.clone(),
        TerrainWorkerDeps {
            source,
            clock,
            access,
            inner_guard: Arc::new(Permit),
            events: Some(events),
        },
    );
    (worker, rx)
}
fn access() -> Arc<Access> {
    Arc::new(Access(AtomicBool::new(true)))
}
async fn start(w: &mut TerrainWorker) {
    w.wait_until_initialized().await.unwrap();
    w.start().unwrap();
}
struct WorkerRelease {
    abort: tokio::task::AbortHandle,
    source: Option<Arc<Source>>,
}
impl Drop for WorkerRelease {
    fn drop(&mut self) {
        if let Some(source) = &self.source {
            source.block.store(false, Ordering::SeqCst);
            source.release.add_permits(1);
        }
        self.abort.abort();
    }
}
async fn bounded_worker_step<T>(test: &str, step: &str, future: impl Future<Output = T>) -> T {
    let started = std::time::Instant::now();
    let result = timeout(Duration::from_secs(10), future).await;
    terrain_test_timing!(
        "FR3_TIMING {test} step={step} seconds={:.6} bound=10",
        started.elapsed().as_secs_f64()
    );
    result.labelled_ok(test, step)
}
async fn seen_labelled(
    test: &str,
    rx: &mut mpsc::UnboundedReceiver<TerrainStatus>,
    at: i64,
    phase: TerrainWorkerPhase,
) -> TerrainStatus {
    let started = std::time::Instant::now();
    let result = timeout(WORKER_OBSERVATION_HORIZON, async {
        loop {
            let status = rx.recv().await;
            assert!(status.is_some(), "assertion failed: {test}: worker status channel stays open while waiting");
            let status = status.unwrap();
            assert!(status.last_pass_at_ms.is_none_or(|t| t <= at),
                "assertion failed: {test}: worker status does not skip the expected pass deadline; status={status:?}");
            if status.last_pass_at_ms == Some(at) && status.state == phase { return status; }
        }
    }).await;
    terrain_test_timing!(
        "FR3_TIMING {test} seen_loop={:.6} virtual_horizon_days=30",
        started.elapsed().as_secs_f64()
    );
    result.labelled_ok(
        test,
        "expected worker phase arrives within the bounded wait",
    )
}
async fn tidy_observed_labelled(
    test: &str,
    rx: &mut mpsc::UnboundedReceiver<TerrainStatus>,
    store: &HubStore,
    cleared: i64,
    complete: bool,
    at: i64,
) {
    let started = std::time::Instant::now();
    let result = timeout(WORKER_OBSERVATION_HORIZON, async {
        loop {
            let status = rx.recv().await;
            assert!(status.is_some(), "assertion failed: {test}: tidy status channel stays open while waiting");
            let status = status.unwrap();
            assert!(status.last_pass_at_ms.is_none_or(|started| started <= at),
                "assertion failed: {test}: tidy progress does not skip its expected deadline; status={status:?}");
            if status.tidy_complete == complete && store.terrain_tidy_progress().unwrap().cleared == cleared { return; }
        }
    }).await;
    terrain_test_timing!(
        "FR3_TIMING {test} tidy_loop={:.6} virtual_horizon_days=30",
        started.elapsed().as_secs_f64()
    );
    result.labelled_ok(
        test,
        "expected tidy progress arrives within the bounded wait",
    )
}
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}
async fn advance(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    settle().await;
}
fn drain(rx: &mut mpsc::UnboundedReceiver<TerrainStatus>) -> Vec<TerrainStatus> {
    let mut out = Vec::new();
    while let Ok(s) = rx.try_recv() {
        out.push(s);
    }
    out
}

#[tokio::test(start_paused = true)]
async fn c9_st32_stop() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let source = Source::new();
    source.block.store(true, Ordering::SeqCst);
    let clock = Arc::new(TokioOffsetClock::new());
    let (mut w, _rx) = launch(&s, d.path(), true, Some(source.clone()), clock, access());
    start(&mut w).await;
    source.entered.notified().await;
    assert_eq!(w.status.borrow().state, TerrainWorkerPhase::Working);
    advance(100).await;
    let stopped_at = Instant::now();
    let abort = w.task.abort_handle();
    let shutdown = tokio::time::timeout(Duration::from_secs(1), w.shutdown(false)).await;
    if shutdown.is_err() {
        source.block.store(false, Ordering::SeqCst);
        source.release.add_permits(1);
        abort.abort();
        settle().await;
    }
    assert!(
        shutdown.is_ok(),
        "assertion failed: c9_st32_stop: stop completes within the bounded wait without writing pending work"
    );
    shutdown
        .labelled_ok(
            "c9_st32_stop",
            "stop completes within the bounded wait without writing pending work",
        )
        .labelled_ok("c9_st32_stop", "worker shutdown succeeds");
    assert_eq!(Instant::now() - stopped_at, Duration::ZERO);
    assert_eq!(
        scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
        0
    );
    assert_eq!(scalar(&s, "SELECT COUNT(*) FROM sync_mutations"), 0);
}

#[tokio::test(start_paused = true)]
async fn c9_st37_conditions() {
    let _serial = SERIAL.lock().await;
    for untrusted in [false, true] {
        let (d, s, _) = fixture(false);
        let c = s.open().unwrap();
        let before = version(&c);
        let clock = Arc::new(TokioOffsetClock::new());
        let root = d.path().join("cache");
        let config = if untrusted {
            std::os::unix::fs::symlink(d.path(), &root).unwrap();
            TerrainConfig {
                enabled: true,
                cache_dir: Some(root),
                ..Default::default()
            }
        } else {
            TerrainConfig {
                enabled: true,
                min_free_bytes: 0,
                ..Default::default()
            }
        };
        let (mut w, _) = launch_config(&s, d.path(), config, None, clock, access());
        let _release = WorkerRelease {
            abort: w.task.abort_handle(),
            source: None,
        };
        bounded_worker_step(
            "c9_st37_conditions",
            "initialisation",
            w.wait_until_initialized(),
        )
        .await
        .unwrap();
        let expected = if untrusted {
            TerrainWorkerPhase::Backoff {
                code: "untrusted_root",
                until_ms: NOW + 300_000,
            }
        } else {
            TerrainWorkerPhase::Halted {
                code: "invalid_config",
            }
        };
        let observed = w.status.borrow().state.clone();
        // C9 ST37: other startup work may delay start, without moving this deadline.
        if untrusted {
            advance(100_000).await;
        }
        w.start().unwrap();
        settle().await;
        assert!(!w.task.is_finished());
        assert_eq!(version(&c), before);
        if untrusted {
            advance(199_999).await;
            assert_eq!(w.status.borrow().state, expected);
            advance(1).await;
            assert_eq!(
                w.status.borrow().state,
                TerrainWorkerPhase::Backoff {
                    code: "untrusted_root",
                    until_ms: NOW + 900_000
                }
            );
        }
        let started = std::time::Instant::now();
        timeout(Duration::from_secs(10), w.shutdown(false))
            .await
            .labelled_ok("c9_st37_conditions", "worker shuts down within the bound")
            .labelled_ok("c9_st37_conditions", "worker shutdown succeeds");
        terrain_test_timing!(
            "FR3_TIMING c9_st37_conditions shutdown={:.6} bound=10",
            started.elapsed().as_secs_f64()
        );
        if untrusted {
            assert_eq!(
                observed, expected,
                "c9_st37_conditions: untrusted cache root enters Backoff"
            );
        } else {
            assert_eq!(
                observed, expected,
                "c9_st37_conditions: invalid configuration enters Halted rather than Backoff"
            );
        }
    }
    // C9 R1: only a store-open failure fails the startup receipt.
    let (d, s, _) = fixture(false);
    std::fs::remove_file(s.database_path()).unwrap();
    std::fs::create_dir(s.database_path()).unwrap();
    let (mut w, _) = launch(
        &s,
        d.path(),
        false,
        None,
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    let _release = WorkerRelease {
        abort: w.task.abort_handle(),
        source: None,
    };
    assert!(matches!(
        bounded_worker_step(
            "c9_st37_conditions",
            "initialisation",
            w.wait_until_initialized()
        )
        .await,
        Err(CollectorError::TerrainWorkerStartup)
    ));
    assert!(matches!(
        bounded_worker_step(
            "c9_st37_conditions",
            "store_failure_exit",
            w.wait_until_exit()
        )
        .await,
        Err(CollectorError::Store(_))
    ));
    // C9 ST37: post-start store, configuration and clock failures stay contained.
    for code in ["store", "invalid_config", "clock", "store_read"] {
        let (d, s, v) = fixture(false);
        seed(&s, v, vec![point(1, 1, 20.5, None)]);
        let source = Source::new();
        source.block.store(true, Ordering::SeqCst);
        if code != "clock" && code != "store_read" {
            source
                .failures
                .lock()
                .unwrap()
                .insert("N10E020".into(), code);
        }
        let clock = Arc::new(TokioOffsetClock::new());
        let (mut w, mut rx) = launch(
            &s,
            d.path(),
            true,
            Some(source.clone()),
            clock.clone(),
            access(),
        );
        let _release = WorkerRelease {
            abort: w.task.abort_handle(),
            source: Some(source.clone()),
        };
        bounded_worker_step("c9_st37_conditions", "worker_start", start(&mut w)).await;
        bounded_worker_step(
            "c9_st37_conditions",
            "source_entry",
            source.entered.notified(),
        )
        .await;
        if code == "clock" {
            clock.failure.store(true, Ordering::SeqCst);
        }
        if code == "store_read" {
            s.open()
                .unwrap()
                .execute_batch("DROP TABLE terrain_tidy_state;")
                .unwrap();
        }
        source.release.add_permits(1);
        let expected_code = if code == "store_read" { "store" } else { code };
        bounded_worker_step("c9_st37_conditions", "failure_phase", async {
            loop {
                let st = rx.recv().await.unwrap();
                if st.last_error_code == Some(expected_code)
                    && matches!(
                        st.state,
                        TerrainWorkerPhase::Backoff { .. } | TerrainWorkerPhase::Halted { .. }
                    )
                {
                    break;
                }
            }
        })
        .await;
        assert!(!w.task.is_finished());
        assert_eq!(
            scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
            0
        );
        if code == "store" {
            source.failures.lock().unwrap().clear();
            advance(300_000).await;
            let st = seen_labelled(
                "c9_st37_conditions",
                &mut rx,
                NOW + 300_000,
                TerrainWorkerPhase::Idle,
            )
            .await;
            assert_eq!(st.filled_since_start, 1);
            assert_eq!(st.last_error_code, None);
        }
        bounded_worker_step(
            "c9_st37_conditions",
            "contained_failure_shutdown",
            w.shutdown(false),
        )
        .await
        .labelled_ok("c9_st37_conditions", "contained worker shutdown succeeds");
    }
}

#[tokio::test(start_paused = true)]
async fn c9_st38_disabled() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let source = Source::new();
    let c = s.open().unwrap();
    let before = version(&c);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        false,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    w.wait_until_initialized().await.unwrap();
    assert_eq!(w.status.borrow().state, TerrainWorkerPhase::Disabled);
    w.start().unwrap();
    tidy_observed_labelled("c9_st38_disabled", &mut rx, &s, 0, true, NOW).await;
    assert!(w.status.borrow().tidy_complete);
    assert_eq!(version(&c), before);
    assert_eq!(source.count(), 0);
    assert!(!d.path().join("cache").exists());
    assert!(!w.task.is_finished());
    w.shutdown(false).await.unwrap();
    // C9 R1: no source supplied and invalid cache options still build no cache.
    let (mut w, mut rx) = launch_config(
        &s,
        d.path(),
        TerrainConfig {
            enabled: false,
            cache_dir: Some(d.path().join("cache")),
            min_free_bytes: 0,
            ..Default::default()
        },
        None,
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    tidy_observed_labelled("c9_st38_disabled", &mut rx, &s, 0, true, NOW).await;
    settle().await;
    assert_eq!(w.status.borrow().state, TerrainWorkerPhase::Disabled);
    assert_eq!(version(&c), before);
    assert!(!d.path().join("cache").exists());
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_st60_cadence() {
    let _serial = SERIAL.lock().await;
    // C9 ST60 (a): the empty-store backstop is measured from the first pass start.
    let (d, s, _) = fixture(false);
    let clock = Arc::new(TokioOffsetClock::new());
    let (mut w, mut rx) = launch(&s, d.path(), true, Some(Source::new()), clock, access());
    start(&mut w).await;
    seen_labelled("c9_st60_cadence", &mut rx, NOW, TerrainWorkerPhase::Idle).await;
    advance(3_599_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    seen_labelled(
        "c9_st60_cadence",
        &mut rx,
        NOW + 3_600_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    // C9 ST60 (b,c): wakes inside the minimum gap coalesce at its exact end.
    let base = NOW + 3_600_000;
    advance(1_000).await;
    w.wake.try_send(()).unwrap();
    settle().await;
    for _ in 0..5 {
        let _ = w.wake.try_send(());
    }
    advance(8_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    seen_labelled(
        "c9_st60_cadence",
        &mut rx,
        base + 10_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    advance(10_000).await;
    assert!(drain(&mut rx).is_empty());
    w.shutdown(false).await.unwrap();
    // C9 ST60 (d): earliest stored retry is used without a wake.
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    plant(&s, v, 1, 1, None, "failed", NOW + 1_200_000);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(Source::new()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    seen_labelled("c9_st60_cadence", &mut rx, NOW, TerrainWorkerPhase::Idle).await;
    advance(1_199_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    let st = seen_labelled(
        "c9_st60_cadence",
        &mut rx,
        NOW + 1_200_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!(st.filled_since_start, 1);
    w.shutdown(false).await.unwrap();
    // C9 ST60 (e): a full pass continues two seconds after it ends.
    let (d, s, v) = fixture(false);
    seed(&s, v, (1..=1001).map(|i| point(i, 1, 20.5, None)).collect());
    let source = Source::new();
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    settle().await;
    while scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance") < 1000 {
        tokio::task::yield_now().await;
    }
    settle().await;
    drain(&mut rx);
    advance(1_999).await;
    assert_eq!(source.count(), 1);
    advance(1).await;
    seen_labelled(
        "c9_st60_cadence",
        &mut rx,
        NOW + 2_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!(source.count(), 2);
    w.shutdown(false).await.unwrap();
    // C9 ST60 (f): an expired Open value is eligible at its deadline.
    let (d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=3)
            .map(|i| point(i, 1, 19.5 + i as f64, None))
            .collect(),
    );
    let source = Source::new();
    for tile in ["N10E020", "N10E021"] {
        source
            .failures
            .lock()
            .unwrap()
            .insert(tile.into(), "provider");
    }
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    seen_labelled(
        "c9_st60_cadence",
        &mut rx,
        NOW,
        TerrainWorkerPhase::BreakerOpen {
            until_ms: NOW + 300_000,
        },
    )
    .await;
    assert_eq!(source.network.load(Ordering::SeqCst), 2);
    advance(299_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    seen_labelled(
        "c9_st60_cadence",
        &mut rx,
        NOW + 300_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!(source.network.load(Ordering::SeqCst), 3);
    w.shutdown(false).await.unwrap();
    // C9 R3, C9 R24: access refusal keeps idle retry timing and store-only counts.
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    plant(&s, v, 1, 1, None, "failed", NOW + 1_200_000);
    let denied = access();
    denied.0.store(false, Ordering::SeqCst);
    let source = Source::new();
    let c = s.open().unwrap();
    let before = version(&c);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        denied,
    );
    start(&mut w).await;
    let status = seen_labelled(
        "c9_st60_cadence",
        &mut rx,
        NOW,
        TerrainWorkerPhase::WaitingForAccess,
    )
    .await;
    assert_eq!(
        (
            status.rows_unexamined,
            status.retries_due,
            status.retries_waiting
        ),
        (1, 0, 1)
    );
    advance(1_199_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    seen_labelled(
        "c9_st60_cadence",
        &mut rx,
        NOW + 1_200_000,
        TerrainWorkerPhase::WaitingForAccess,
    )
    .await;
    assert_eq!(version(&c), before);
    assert_eq!(source.count(), 0);
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_st61_wake() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let source = Source::new();
    source.block.store(true, Ordering::SeqCst);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    source.entered.notified().await;
    let mut queued = 0;
    for _ in 0..1000 {
        if w.wake.try_send(()).is_ok() {
            queued += 1;
        }
    }
    assert_eq!(queued, 1);
    assert_eq!(w.wake.capacity(), 0);
    source.release.add_permits(1);
    seen_labelled("c9_st61_wake", &mut rx, NOW, TerrainWorkerPhase::Idle).await;
    advance(9_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    seen_labelled(
        "c9_st61_wake",
        &mut rx,
        NOW + 10_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    advance(10_000).await;
    assert!(drain(&mut rx).is_empty());
    assert_eq!(source.count(), 1);
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_st66_status() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let source = Source::new();
    source.block.store(true, Ordering::SeqCst);
    let allowed = access();
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        allowed.clone(),
    );
    start(&mut w).await;
    source.entered.notified().await;
    assert_eq!(w.status.borrow().state, TerrainWorkerPhase::Working);
    source.release.add_permits(1);
    let st = seen_labelled("c9_st66_status", &mut rx, NOW, TerrainWorkerPhase::Idle).await;
    assert_eq!(
        (
            st.filled_since_start,
            st.rows_unexamined,
            st.retries_due,
            st.retries_waiting
        ),
        (1, 0, 0, 0)
    );
    assert_eq!(
        (st.last_pass_at_ms, st.last_fill_at_ms),
        (Some(NOW), Some(NOW))
    );
    allowed.0.store(false, Ordering::SeqCst);
    w.wake.try_send(()).unwrap();
    advance(10_000).await;
    seen_labelled(
        "c9_st66_status",
        &mut rx,
        NOW + 10_000,
        TerrainWorkerPhase::WaitingForAccess,
    )
    .await;
    assert_eq!(source.count(), 1);
    w.shutdown(false).await.unwrap();
    // C9 ST66: runtime phase reflects a breaker deferral at its active deadline.
    let (d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=3)
            .map(|i| point(i, 1, 19.5 + i as f64, None))
            .collect(),
    );
    let source = Source::new();
    for tile in ["N10E020", "N10E021"] {
        source
            .failures
            .lock()
            .unwrap()
            .insert(tile.into(), "provider");
    }
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    let phase = TerrainWorkerPhase::BreakerOpen {
        until_ms: NOW + 300_000,
    };
    let st = seen_labelled("c9_st66_status", &mut rx, NOW, phase.clone()).await;
    assert_eq!(w.status.borrow().state, phase);
    assert_eq!(
        (st.rows_unexamined, st.retries_due, st.retries_waiting),
        (1, 0, 2)
    );
    w.shutdown(false).await.unwrap();
    // C9 ST66: cursor-only progress refreshes counts, then empty idle polls write nothing.
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, Some(44))]);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(Source::new()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    let st = seen_labelled("c9_st66_status", &mut rx, NOW, TerrainWorkerPhase::Idle).await;
    assert_eq!(st.rows_unexamined, 0);
    seed(&s, v, vec![point(2, 2, 20.5, Some(55))]);
    w.wake.try_send(()).unwrap();
    advance(10_000).await;
    let st = seen_labelled(
        "c9_st66_status",
        &mut rx,
        NOW + 10_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!(st.rows_unexamined, 0);
    plant(&s, v, 2, 2, None, "failed", NOW + 86_400_000);
    let c = s.open().unwrap();
    let before = version(&c);
    w.wake.try_send(()).unwrap();
    advance(10_000).await;
    let st = seen_labelled(
        "c9_st66_status",
        &mut rx,
        NOW + 20_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!(version(&c), before);
    assert_eq!(
        s.terrain_backlog(NOW + 20_000, crate::db::K_STATUS_COUNT_CAP)
            .unwrap()
            .retries_waiting,
        1
    );
    assert_eq!(
        st.retries_waiting, 0,
        "C9 R24: a non-writing pass retains the last count"
    );
    w.shutdown(false).await.unwrap();
}

fn import_source(s: &HubStore) -> Uuid {
    s.register_source(
        &SourceDescriptor::new(IMPORT_SOURCE_KIND, "c9-synthetic-import"),
        T0,
    )
    .unwrap()
    .source_id
}
fn raise(s: &HubStore, v: Uuid, source: Uuid, id: i64) {
    let mut c = s.open().unwrap();
    let tx = c.transaction().unwrap();
    tx.execute("INSERT INTO lifecycle_source_watermarks(source_id,vehicle_id,domain,max_source_row_id,max_timestamp_ms) VALUES(?1,?2,'positions',?3,?4) ON CONFLICT(source_id,vehicle_id,domain) DO UPDATE SET max_source_row_id=excluded.max_source_row_id,max_timestamp_ms=excluded.max_timestamp_ms",params![source.to_string(),v.to_string(),id,T0+id*1000]).unwrap();
    crate::db::terrain_after_import_boundary_raise_in_transaction(&tx, v, NOW).unwrap();
    tx.commit().unwrap();
}
fn layout69() -> (tempfile::TempDir, HubStore, Uuid) {
    let (d, s, v) = fixture(true);
    let imported = import_source(&s);
    raise(&s, v, imported, 1200);
    seed(
        &s,
        v,
        (1..=1210)
            .map(|i| {
                point(
                    i,
                    if i <= 1200 { 6 } else { 7 },
                    20.5,
                    (i <= 1200).then_some(100),
                )
            })
            .collect(),
    );
    s.open().unwrap().execute(
        "INSERT INTO terrain_elevation_provenance(vehicle_id,position_id,drive_id,latitude,longitude,elevation_m,tile_name,tile_hash,dataset_source,dataset_version,status,attempts,attempted_at_ms,retry_after_ms) SELECT vehicle_id,position_id,drive_id,10.5,20.5,100,'N10E020','synthetic','aws','1','success',1,?1,0 FROM materialised_positions WHERE position_id<=1200", [NOW]
    ).unwrap();
    (d, s, v)
}
#[tokio::test(start_paused = true)]
async fn c9_st70_disabled_tidy() {
    let _serial = SERIAL.lock().await;
    // C9 ST70 (b): a completed marker is observed with no durable writes.
    let (d, s, _) = fixture(false);
    let c = s.open().unwrap();
    let before = version(&c);
    let source = Source::new();
    let (mut w, mut ready) = launch(
        &s,
        d.path(),
        false,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    tidy_observed_labelled("c9_st70_disabled_tidy", &mut ready, &s, 0, true, NOW).await;
    assert!(w.status.borrow().tidy_complete);
    assert_eq!(w.status.borrow().state, TerrainWorkerPhase::Disabled);
    assert_eq!(version(&c), before);
    assert_eq!(source.count(), 0);
    w.shutdown(false).await.unwrap();
    drop(c);
    // C9 ST70 (c): an unreadable supplied database fails the startup receipt.
    std::fs::remove_file(s.database_path()).unwrap();
    std::fs::create_dir(s.database_path()).unwrap();
    let (mut w, _) = launch(
        &s,
        d.path(),
        false,
        None,
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    assert!(matches!(
        w.wait_until_initialized().await,
        Err(CollectorError::TerrainWorkerStartup)
    ));
    assert!(matches!(
        w.wait_until_exit().await,
        Err(CollectorError::Store(_))
    ));
    for enabled in [false, true] {
        let (d, s, v) = layout69();
        let source = Source::new();
        let (mut w, mut rx) = launch(
            &s,
            d.path(),
            enabled,
            Some(source.clone()),
            Arc::new(TokioOffsetClock::new()),
            access(),
        );
        w.wait_until_initialized().await.unwrap();
        if !enabled {
            assert_eq!(w.status.borrow().state, TerrainWorkerPhase::Disabled);
            assert!(!w.status.borrow().tidy_complete);
            assert_eq!(w.status.borrow().last_pass_at_ms, None);
        }
        w.start().unwrap();
        tidy_observed_labelled("c9_st70_disabled_tidy", &mut rx, &s, 1000, false, NOW).await;
        assert_eq!(s.terrain_tidy_progress().unwrap().cleared, 1000);
        assert!(!w.status.borrow().tidy_complete);
        assert_eq!(
            w.status.borrow().state,
            if enabled {
                TerrainWorkerPhase::Working
            } else {
                TerrainWorkerPhase::Disabled
            }
        );
        advance(1_999).await;
        assert_eq!(s.terrain_tidy_progress().unwrap().cleared, 1000);
        advance(1).await;
        tidy_observed_labelled(
            "c9_st70_disabled_tidy",
            &mut rx,
            &s,
            1200,
            true,
            NOW + 2_000,
        )
        .await;
        assert_eq!(s.terrain_tidy_progress().unwrap().cleared, 1200);
        assert!(w.status.borrow().tidy_complete);
        assert_eq!(source.count(), 0);
        assert_eq!(
            scalar(
                &s,
                "SELECT COUNT(*) FROM materialised_positions WHERE position_id<=1200 AND json_extract(position_json,'$.elevation') IS NULL"
            ),
            1200
        );
        assert_eq!(
            scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
            0
        );
        assert_eq!(
            scalar(
                &s,
                "SELECT COUNT(*) FROM sync_mutations WHERE entity='position'"
            ),
            1200
        );
        let pair: (Option<i64>, Option<i64>) = s
            .open()
            .unwrap()
            .query_row(
                "SELECT ascent,descent FROM materialised_drives WHERE vehicle_id=?1 AND drive_id=6",
                [v.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(pair, (None, None));
        advance(2_000).await;
        if enabled {
            seen_labelled(
                "c9_st70_disabled_tidy",
                &mut rx,
                NOW + 4_000,
                TerrainWorkerPhase::Idle,
            )
            .await;
            assert_eq!(source.count(), 1);
            assert_eq!(w.status.borrow().filled_since_start, 10);
        } else {
            assert_eq!(source.count(), 0);
            assert_eq!(source.network.load(Ordering::SeqCst), 0);
            assert_eq!(
                w.status.borrow().last_pass_at_ms,
                Some(NOW + 2_000),
                "completed disabled tidy returns to idle timing immediately"
            );
        }
        assert!(!d.path().join("cache").exists());
        w.shutdown(false).await.unwrap();
    }
}
#[tokio::test(start_paused = true)]
async fn c9_st74_disabled_rearm() {
    let _serial = SERIAL.lock().await;
    for wake in [true, false] {
        let (d, s, v) = fixture(false);
        let imported = import_source(&s);
        raise(&s, v, imported, 10);
        seed(
            &s,
            v,
            (11..=15)
                .map(|i| {
                    point(
                        i,
                        8,
                        20.5,
                        if i < 15 {
                            Some(100 + (i - 11) * 10)
                        } else {
                            None
                        },
                    )
                })
                .collect(),
        );
        for i in 11..=14 {
            plant(&s, v, i, 8, Some(100 + (i - 11) * 10), "success", 0);
        }
        plant(&s, v, 15, 8, None, "void", 0);
        s.open()
            .unwrap()
            .execute_batch("UPDATE materialised_drives SET ascent=30,descent=0;")
            .unwrap();
        let retained_void: String = s.open().unwrap().query_row(
            "SELECT json_object('vehicle',vehicle_id,'position',position_id,'drive',drive_id,'latitude',latitude,'longitude',longitude,'elevation',elevation_m,'tile',tile_name,'hash',tile_hash,'source',dataset_source,'version',dataset_version,'status',status,'attempts',attempts,'attempted',attempted_at_ms,'retry',retry_after_ms,'error',error_code) FROM terrain_elevation_provenance WHERE position_id=15", [], |r| r.get(0)).unwrap();
        let source = Source::new();
        let (mut w, mut rx) = launch(
            &s,
            d.path(),
            false,
            Some(source.clone()),
            Arc::new(TokioOffsetClock::new()),
            access(),
        );
        let c = s.open().unwrap();
        let before = version(&c);
        start(&mut w).await;
        loop {
            if w.status.borrow().tidy_complete {
                break;
            }
            rx.recv().await.unwrap();
        }
        settle().await;
        assert_eq!(version(&c), before);
        let old = drain(&mut rx);
        assert!(old.last().unwrap().tidy_complete);
        advance(1_000).await;
        raise(&s, v, imported, 12);
        assert_eq!(s.terrain_tidy_progress().unwrap().run, 2);
        if wake {
            w.wake.try_send(()).labelled_ok(
                "c9_st74_disabled_rearm",
                "disabled worker stays alive to accept a rearm wake",
            );
        }
        let at = if wake { 10_000 } else { 3_600_000 };
        advance(at - 1_001).await;
        assert!(s.terrain_tidy_progress().unwrap().completed_at_ms.is_none());
        assert!(drain(&mut rx).is_empty());
        advance(1).await;
        settle().await;
        assert_eq!(
            w.status.borrow().last_pass_at_ms,
            Some(NOW + at as i64),
            "disabled re-arm pass acknowledged at its exact deadline"
        );
        let mut statuses = Vec::new();
        while s.terrain_tidy_progress().unwrap().completed_at_ms.is_none() {
            statuses.push(rx.recv().await.unwrap());
        }
        statuses.extend(drain(&mut rx));
        let flags = statuses
            .iter()
            .map(|s| s.tidy_complete)
            .fold(Vec::new(), |mut flags, v| {
                if flags.last() != Some(&v) {
                    flags.push(v);
                }
                flags
            });
        assert_eq!(flags, vec![true, false, true]);
        assert!(
            statuses
                .iter()
                .all(|s| s.state == TerrainWorkerPhase::Disabled)
        );
        assert!(
            statuses
                .iter()
                .all(|s| s.last_pass_at_ms == Some(NOW + at as i64))
        );
        let p = s.terrain_tidy_progress().unwrap();
        assert_eq!(
            (
                p.run,
                p.examined,
                p.cleared,
                p.kept,
                p.requeued,
                p.removed,
                p.drives_changed
            ),
            (2, 5, 2, 2, 0, 0, 1)
        );
        assert!(p.completed_at_ms.is_some());
        assert_eq!(
            scalar(
                &s,
                "SELECT COUNT(*) FROM materialised_positions WHERE position_id IN (11,12) AND json_extract(position_json,'$.elevation') IS NULL"
            ),
            2
        );
        assert_eq!(
            scalar(
                &s,
                "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE status='success'"
            ),
            2
        );
        assert_eq!(
            scalar(
                &s,
                "SELECT COUNT(*) FROM sync_mutations WHERE entity='position'"
            ),
            2
        );
        assert_eq!(
            scalar(
                &s,
                "SELECT COUNT(*) FROM sync_mutations WHERE entity='drive'"
            ),
            1
        );
        let pair: (i64, i64) = s
            .open()
            .unwrap()
            .query_row(
                "SELECT ascent,descent FROM materialised_drives WHERE drive_id=8",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(pair, (10, 0));
        for (id, elevation) in [(13, 120), (14, 130)] {
            assert_eq!(
                scalar(
                    &s,
                    &format!(
                        "SELECT json_extract(position_json,'$.elevation') FROM materialised_positions WHERE position_id={id}"
                    )
                ),
                elevation
            );
            assert_eq!(
                scalar(
                    &s,
                    &format!(
                        "SELECT elevation_m FROM terrain_elevation_provenance WHERE position_id={id} AND status='success'"
                    )
                ),
                elevation
            );
        }
        assert_eq!(
            scalar(
                &s,
                "SELECT COUNT(*) FROM materialised_positions WHERE position_id=15 AND json_extract(position_json,'$.elevation') IS NULL"
            ),
            1
        );
        let current_void: String = s.open().unwrap().query_row(
            "SELECT json_object('vehicle',vehicle_id,'position',position_id,'drive',drive_id,'latitude',latitude,'longitude',longitude,'elevation',elevation_m,'tile',tile_name,'hash',tile_hash,'source',dataset_source,'version',dataset_version,'status',status,'attempts',attempts,'attempted',attempted_at_ms,'retry',retry_after_ms,'error',error_code) FROM terrain_elevation_provenance WHERE position_id=15", [], |r| r.get(0)).unwrap();
        assert_eq!(current_void, retained_void);
        assert_eq!(source.count(), 0);
        assert_eq!(source.network.load(Ordering::SeqCst), 0);
        assert!(!d.path().join("cache").exists());
        w.shutdown(false).await.unwrap();
    }
}

/// C9 ST77: exercise the pass directly, without spawning a worker or receiver.
pub(crate) async fn fill_diagnostic_fixture(store: &HubStore) {
    let source = Source::new();
    let mut state = TerrainWorkerState::default();
    let clock = TokioOffsetClock::new();
    for _ in 0..3 {
        run_terrain_enrichment_pass(
            store,
            source.as_ref(),
            &mut state,
            access().as_ref(),
            &Permit,
            &clock,
        )
        .await
        .unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn c9_worker_local_deferral() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let source = Source::new();
    source.pending.store(true, Ordering::SeqCst);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    seen_labelled(
        "c9_worker_local_deferral",
        &mut rx,
        NOW,
        TerrainWorkerPhase::Idle,
    )
    .await;
    let mut at = NOW;
    for delay in [2_000, 4_000, 8_000, 10_000, 10_000] {
        advance(delay - 1).await;
        assert!(
            drain(&mut rx).is_empty(),
            "assertion failed: c9_worker_local_deferral: local ownership recheck does not start before its backoff deadline"
        );
        advance(1).await;
        at += delay as i64;
        assert_eq!(
            w.status.borrow().last_pass_at_ms,
            Some(at),
            "local ownership must be re-checked at its bounded deadline"
        );
        seen_labelled(
            "c9_worker_local_deferral",
            &mut rx,
            at,
            TerrainWorkerPhase::Idle,
        )
        .await;
        assert_eq!(source.count(), 0);
    }
    source.pending.store(false, Ordering::SeqCst);
    advance(10_000).await;
    at += 10_000;
    seen_labelled(
        "c9_worker_local_deferral",
        &mut rx,
        at,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!(source.count(), 1);
    seed(&s, v, vec![point(2, 2, 20.5, None)]);
    source.pending.store(true, Ordering::SeqCst);
    w.wake.try_send(()).unwrap();
    advance(10_000).await;
    at += 10_000;
    seen_labelled(
        "c9_worker_local_deferral",
        &mut rx,
        at,
        TerrainWorkerPhase::Idle,
    )
    .await;
    advance(1_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    assert_eq!(
        w.status.borrow().last_pass_at_ms,
        Some(at + 2_000),
        "a pass without local deferral resets its short backoff"
    );
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_open_without_deferred_group() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let source = Source::new();
    source
        .failures
        .lock()
        .unwrap()
        .insert("N10E020".into(), "io");
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    seen_labelled(
        "c9_worker_open_without_deferred_group",
        &mut rx,
        NOW,
        TerrainWorkerPhase::BreakerOpen {
            until_ms: NOW + 300_000,
        },
    )
    .await;
    drain(&mut rx);
    advance(299_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    assert_eq!(
        w.status.borrow().last_pass_at_ms,
        Some(NOW + 300_000),
        "the worker re-checks an active breaker at until_ms"
    );
    assert_eq!(source.network.load(Ordering::SeqCst), 1);
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_refresh_after_partial_store_error() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let source = Source::new();
    let allowed = access();
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        allowed.clone(),
    );
    start(&mut w).await;
    seen_labelled(
        "c9_worker_refresh_after_partial_store_error",
        &mut rx,
        NOW,
        TerrainWorkerPhase::Idle,
    )
    .await;
    seed(
        &s,
        v,
        (2..=252).map(|id| point(id, 2, 20.5, None)).collect(),
    );
    s.open().unwrap().execute_batch("CREATE TRIGGER c9_fail_last BEFORE INSERT ON terrain_elevation_provenance WHEN NEW.position_id=252 BEGIN SELECT RAISE(ABORT,'synthetic chunk'); END;").unwrap();
    w.wake.try_send(()).unwrap();
    advance(10_000).await;
    seen_labelled(
        "c9_worker_refresh_after_partial_store_error",
        &mut rx,
        NOW + 10_000,
        TerrainWorkerPhase::Backoff {
            code: "store",
            until_ms: NOW + 310_000,
        },
    )
    .await;
    assert_eq!(
        scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
        251
    );
    assert_eq!(
        w.status.borrow().filled_since_start,
        1,
        "a store error can hide fills from earlier chunks of that pass"
    );
    s.open()
        .unwrap()
        .execute_batch("DROP TRIGGER c9_fail_last;")
        .unwrap();
    allowed.0.store(false, Ordering::SeqCst);
    advance(299_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    let st = seen_labelled(
        "c9_worker_refresh_after_partial_store_error",
        &mut rx,
        NOW + 310_000,
        TerrainWorkerPhase::WaitingForAccess,
    )
    .await;
    assert_eq!(
        (st.rows_unexamined, st.retries_due, st.retries_waiting),
        (1, 0, 0),
        "the first successful count read after a partial pass must refresh despite no new writes"
    );
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_backoff_schedule() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let source = Source::new();
    source
        .failures
        .lock()
        .unwrap()
        .insert("N10E020".into(), "store");
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    let mut at = NOW;
    let mut delay = 300_000_u64;
    for _ in 0..9 {
        seen_labelled(
            "c9_worker_backoff_schedule",
            &mut rx,
            at,
            TerrainWorkerPhase::Backoff {
                code: "store",
                until_ms: at + delay as i64,
            },
        )
        .await;
        let calls = source.count();
        let _ = w.wake.try_send(());
        advance(delay - 1).await;
        assert_eq!(source.count(), calls, "a wake cannot bypass worker backoff");
        advance(1).await;
        at += delay as i64;
        delay = (delay * 2).min(21_600_000);
    }
    seen_labelled(
        "c9_worker_backoff_schedule",
        &mut rx,
        at,
        TerrainWorkerPhase::Backoff {
            code: "store",
            until_ms: at + delay as i64,
        },
    )
    .await;
    source.failures.lock().unwrap().clear();
    advance(delay).await;
    at += delay as i64;
    seen_labelled(
        "c9_worker_backoff_schedule",
        &mut rx,
        at,
        TerrainWorkerPhase::Idle,
    )
    .await;
    seed(&s, v, vec![point(2, 2, 21.5, None)]);
    source
        .failures
        .lock()
        .unwrap()
        .insert("N10E021".into(), "store");
    w.wake.try_send(()).unwrap();
    advance(10_000).await;
    at += 10_000;
    seen_labelled(
        "c9_worker_backoff_schedule",
        &mut rx,
        at,
        TerrainWorkerPhase::Backoff {
            code: "store",
            until_ms: at + 300_000,
        },
    )
    .await;
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_stop_at_waits() {
    let _serial = SERIAL.lock().await;
    for stage in [
        "start",
        "idle",
        "minimum",
        "continuation",
        "backoff",
        "halted",
        "local",
    ] {
        let (d, s, v) = fixture(false);
        let source = Source::new();
        if matches!(stage, "continuation" | "backoff" | "halted" | "local") {
            seed(
                &s,
                v,
                (1..=if stage == "continuation" { 1001 } else { 1 })
                    .map(|id| point(id, 1, 20.5, None))
                    .collect(),
            );
        }
        if matches!(stage, "backoff" | "halted") {
            source.failures.lock().unwrap().insert(
                "N10E020".into(),
                if stage == "halted" {
                    "invalid_config"
                } else {
                    "store"
                },
            );
        }
        source.pending.store(stage == "local", Ordering::SeqCst);
        let (mut w, mut rx) = launch(
            &s,
            d.path(),
            true,
            Some(source.clone()),
            Arc::new(TokioOffsetClock::new()),
            access(),
        );
        let _release = WorkerRelease {
            abort: w.task.abort_handle(),
            source: Some(source.clone()),
        };
        bounded_worker_step(
            "c9_worker_stop_at_waits",
            "initialisation",
            w.wait_until_initialized(),
        )
        .await
        .unwrap();
        settle().await;
        assert_eq!(
            w.status.borrow().last_pass_at_ms,
            None,
            "start is acknowledged before work"
        );
        let phase_started = std::time::Instant::now();
        let phase_ready = timeout(WORKER_OBSERVATION_HORIZON, async {
            if stage != "start" {
                w.start().unwrap();
                let phase = match stage {
                    "continuation" => TerrainWorkerPhase::Working,
                    "backoff" => TerrainWorkerPhase::Backoff {
                        code: "store",
                        until_ms: NOW + 300_000,
                    },
                    "halted" => TerrainWorkerPhase::Halted {
                        code: "invalid_config",
                    },
                    _ => TerrainWorkerPhase::Idle,
                };
                // A full pass publishes Working before and after work, so wait for its fills too.
                loop {
                    let st =
                        seen_labelled("c9_worker_stop_at_waits", &mut rx, NOW, phase.clone()).await;
                    if stage != "continuation" || st.filled_since_start == 1000 {
                        break;
                    }
                }
                settle().await;
                if stage == "minimum" {
                    advance(1_000).await;
                    w.wake.try_send(()).unwrap();
                    settle().await;
                }
            }
        })
        .await
        .is_ok();
        terrain_test_timing!(
            "FR3_TIMING c9_worker_stop_at_waits phase={:.6} bound=20 stage={stage}",
            phase_started.elapsed().as_secs_f64()
        );
        source.block.store(false, Ordering::SeqCst);
        source.release.add_permits(1);
        let at = Instant::now();
        let abort = w.task.abort_handle();
        let shutdown_started = std::time::Instant::now();
        let shutdown = tokio::time::timeout(Duration::from_secs(1), w.shutdown(false)).await;
        if shutdown.is_err() {
            abort.abort();
            settle().await;
        }
        terrain_test_timing!(
            "FR3_TIMING c9_worker_stop_at_waits shutdown={:.6} bound=1 stage={stage}",
            shutdown_started.elapsed().as_secs_f64()
        );
        assert!(
            phase_ready,
            "assertion failed: c9_worker_stop_at_waits: expected worker phase arrives within the bounded wait; stage={stage}"
        );
        assert!(
            shutdown.is_ok(),
            "assertion failed: c9_worker_stop_at_waits: stop completes within the bounded wait at every worker stage"
        );
        shutdown
            .labelled_ok(
                "c9_worker_stop_at_waits",
                "stop completes within the bounded wait at every worker stage",
            )
            .labelled_ok("c9_worker_stop_at_waits", "worker shutdown succeeds");
        assert_eq!(Instant::now() - at, Duration::ZERO, "stop at {stage}");
    }
}

#[tokio::test(start_paused = true)]
async fn c9_worker_clock_after_commit() {
    let _serial = SERIAL.lock().await;
    struct ClockAfterWrite {
        store: HubStore,
        offset: TokioOffsetClock,
        fail: AtomicBool,
    }
    impl TerrainClock for ClockAfterWrite {
        fn now_ms(&self) -> Result<i64, CollectorError> {
            if self.fail.load(Ordering::SeqCst)
                && scalar(
                    &self.store,
                    "SELECT COUNT(*) FROM terrain_elevation_provenance",
                ) > 0
            {
                Err(CollectorError::InvalidReceiptTimestamp)
            } else {
                self.offset.now_ms()
            }
        }
    }
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let clock = Arc::new(ClockAfterWrite {
        store: s.clone(),
        offset: TokioOffsetClock::new(),
        fail: AtomicBool::new(true),
    });
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(Source::new()),
        clock.clone(),
        access(),
    );
    start(&mut w).await;
    let st = seen_labelled(
        "c9_worker_clock_after_commit",
        &mut rx,
        NOW,
        TerrainWorkerPhase::Backoff {
            code: "clock",
            until_ms: NOW + 300_000,
        },
    )
    .await;
    assert_eq!(
        st.filled_since_start, 1,
        "a completed report's known fills survive a later clock failure"
    );
    assert_eq!(
        st.last_fill_at_ms, None,
        "the fill time is unknown when the clock fails"
    );
    clock.fail.store(false, Ordering::SeqCst);
    advance(300_000).await;
    let st = seen_labelled(
        "c9_worker_clock_after_commit",
        &mut rx,
        NOW + 300_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!((st.filled_since_start, st.rows_unexamined), (1, 0));
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_retry_minimum() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    plant(&s, v, 1, 1, None, "failed", NOW + 5_000);
    let source = Source::new();
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    seen_labelled(
        "c9_worker_retry_minimum",
        &mut rx,
        NOW,
        TerrainWorkerPhase::Idle,
    )
    .await;
    advance(9_999).await;
    assert!(
        drain(&mut rx).is_empty(),
        "assertion failed: c9_worker_retry_minimum: near retry still respects the minimum pass gap"
    );
    assert_eq!(source.count(), 0);
    advance(1).await;
    let st = seen_labelled(
        "c9_worker_retry_minimum",
        &mut rx,
        NOW + 10_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!(st.filled_since_start, 1);
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_continuation_after_pass_end() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=1001).map(|id| point(id, 1, 20.5, None)).collect(),
    );
    let source = Source::new();
    source.block.store(true, Ordering::SeqCst);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    source.entered.notified().await;
    advance(30_000).await;
    source.release.add_permits(1);
    loop {
        let st = seen_labelled(
            "c9_worker_continuation_after_pass_end",
            &mut rx,
            NOW,
            TerrainWorkerPhase::Working,
        )
        .await;
        if st.filled_since_start == 1000 {
            break;
        }
    }
    settle().await;
    drain(&mut rx);
    advance(1_999).await;
    assert!(
        drain(&mut rx).is_empty(),
        "continuation is measured from completion"
    );
    assert_eq!(source.count(), 1);
    advance(1).await;
    seen_labelled(
        "c9_worker_continuation_after_pass_end",
        &mut rx,
        NOW + 32_000,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!(source.count(), 2);
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_status_count_cap() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=10001).map(|id| point(id, 1, 20.5, None)).collect(),
    );
    let denied = access();
    denied.0.store(false, Ordering::SeqCst);
    let source = Source::new();
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        denied,
    );
    start(&mut w).await;
    let st = seen_labelled(
        "c9_worker_status_count_cap",
        &mut rx,
        NOW,
        TerrainWorkerPhase::WaitingForAccess,
    )
    .await;
    assert_eq!(
        (st.rows_unexamined, st.retries_due, st.retries_waiting),
        (10_000, 0, 0)
    );
    assert_eq!(source.count(), 0);
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_scheduling_condition_backoff() {
    let _serial = SERIAL.lock().await;
    struct ScheduleClock {
        offset: TokioOffsetClock,
        status: Mutex<Option<watch::Receiver<TerrainStatus>>>,
    }
    impl TerrainClock for ScheduleClock {
        fn now_ms(&self) -> Result<i64, CollectorError> {
            let status = self.status.lock().unwrap();
            if status.as_ref().is_some_and(|s| {
                let s = s.borrow();
                s.state == TerrainWorkerPhase::Idle && s.last_pass_at_ms.is_some()
            }) {
                Err(CollectorError::InvalidReceiptTimestamp)
            } else {
                self.offset.now_ms()
            }
        }
    }
    let (d, s, _) = fixture(false);
    let clock = Arc::new(ScheduleClock {
        offset: TokioOffsetClock::new(),
        status: Mutex::new(None),
    });
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(Source::new()),
        clock.clone(),
        access(),
    );
    w.wait_until_initialized().await.unwrap();
    *clock.status.lock().unwrap() = Some(w.status.clone());
    w.start().unwrap();
    seen_labelled(
        "c9_worker_scheduling_condition_backoff",
        &mut rx,
        NOW,
        TerrainWorkerPhase::Backoff {
            code: "clock",
            until_ms: NOW + 300_000,
        },
    )
    .await;
    advance(300_000).await;
    seen_labelled(
        "c9_worker_scheduling_condition_backoff",
        &mut rx,
        NOW + 300_000,
        TerrainWorkerPhase::Backoff {
            code: "clock",
            until_ms: NOW + 900_000,
        },
    )
    .await;
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_phase_logging() {
    let _serial = SERIAL.lock().await;
    let log = super::DecisionLog::start();
    let (d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 20.5, None)]);
    let source = Source::new();
    source.block.store(true, Ordering::SeqCst);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    w.wait_until_initialized().await.unwrap();
    w.log_status_phase();
    assert_eq!(log.lines("DEBUG", "terrain worker phase"), 1);
    w.start().unwrap();
    source.entered.notified().await;
    for _ in 0..2 {
        tokio::select! {
            biased;
            result = w.wait_until_exit() => panic!("worker exited: {result:?}"),
            () = tokio::task::yield_now() => {},
        }
        assert_eq!(
            log.lines("DEBUG", "terrain worker phase"),
            2,
            "the current Working phase is logged once even before polling and after cancellation"
        );
    }
    source.release.add_permits(1);
    seen_labelled(
        "c9_worker_phase_logging",
        &mut rx,
        NOW,
        TerrainWorkerPhase::Idle,
    )
    .await;
    tokio::select! {
        biased;
        result = w.wait_until_exit() => panic!("worker exited: {result:?}"),
        () = tokio::task::yield_now() => {},
    }
    assert_eq!(log.lines("DEBUG", "terrain worker phase"), 3);
    w.shutdown(false).await.unwrap();
    assert_eq!(
        log.lines("DEBUG", "terrain worker phase"),
        3,
        "shutdown does not repeat the last observed phase"
    );
}

#[tokio::test(start_paused = true)]
async fn c9_worker_open_breaker_full_batch() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    // C9 R3: five untouched tiles retain a cut window after both failures.
    seed(
        &s,
        v,
        (1..=1003)
            .map(|id| {
                point(
                    id,
                    1,
                    if id <= 2 {
                        19.5 + id as f64
                    } else {
                        22.5 + ((id - 3) % 5) as f64
                    },
                    None,
                )
            })
            .collect(),
    );
    let source = Source::new();
    for tile in ["N10E020", "N10E021"] {
        source
            .failures
            .lock()
            .unwrap()
            .insert(tile.into(), "provider");
    }
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    let phase = TerrainWorkerPhase::BreakerOpen {
        until_ms: NOW + 300_000,
    };
    seen_labelled(
        "c9_worker_open_breaker_full_batch",
        &mut rx,
        NOW,
        phase.clone(),
    )
    .await;
    assert_eq!(source.network.load(Ordering::SeqCst), 2);
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE status='failed'"
        ),
        2
    );
    advance(1_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    seen_labelled(
        "c9_worker_open_breaker_full_batch",
        &mut rx,
        NOW + 2_000,
        phase.clone(),
    )
    .await;
    advance(297_999).await;
    assert_eq!(
        w.status.borrow().last_pass_at_ms,
        Some(NOW + 2_000),
        "C9 R3: an open breaker cannot spin full batches without progress"
    );
    assert_eq!(w.status.borrow().state, phase);
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    assert_eq!(w.status.borrow().last_pass_at_ms, Some(NOW + 300_000));
    assert!(
        drain(&mut rx)
            .iter()
            .any(|st| st.last_pass_at_ms == Some(NOW + 300_000))
    );
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_open_breaker_due_retries() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    let mut positions: Vec<_> = (1..=251).map(|id| point(id, 1, 20.5, None)).collect();
    positions.extend([point(252, 2, 21.5, None), point(253, 2, 22.5, None)]);
    seed(&s, v, positions);
    for id in 1..=251 {
        plant(&s, v, id, 1, None, "failed", NOW - 1);
    }
    let source = Source::new();
    for tile in ["N10E021", "N10E022"] {
        source
            .failures
            .lock()
            .unwrap()
            .insert(tile.into(), "provider");
    }
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    let phase = TerrainWorkerPhase::BreakerOpen {
        until_ms: NOW + 300_000,
    };
    let st = seen_labelled(
        "c9_worker_open_breaker_due_retries",
        &mut rx,
        NOW,
        phase.clone(),
    )
    .await;
    assert_eq!(st.retries_due, 251);
    advance(299_999).await;
    assert_eq!(
        w.status.borrow().last_pass_at_ms,
        Some(NOW),
        "C9 R3: due retries wait for the open breaker"
    );
    assert_eq!(w.status.borrow().state, phase);
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    assert_eq!(w.status.borrow().last_pass_at_ms, Some(NOW + 300_000));
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_expired_breaker_deferral() {
    let _serial = SERIAL.lock().await;
    struct CrossingSource {
        source: Arc<Source>,
        delay: AtomicBool,
    }
    impl TileSource for CrossingSource {
        fn acquire<'a>(
            &'a self,
            tile: TileId,
            guard: &'a dyn EgressGuard,
            inner: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>
        {
            Box::pin(async move {
                if tile.name() == "N10E023" && self.delay.swap(false, Ordering::SeqCst) {
                    sleep(Duration::from_millis(2)).await;
                }
                self.source.acquire(tile, guard, inner).await
            })
        }
    }
    let (d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=4)
            .map(|id| point(id, 1, 19.5 + id as f64, None))
            .collect(),
    );
    let source = Source::new();
    for tile in ["N10E020", "N10E021"] {
        source
            .failures
            .lock()
            .unwrap()
            .insert(tile.into(), "provider");
    }
    let crossing = Arc::new(CrossingSource {
        source: source.clone(),
        delay: AtomicBool::new(false),
    });
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(crossing.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    seen_labelled(
        "c9_worker_expired_breaker_deferral",
        &mut rx,
        NOW,
        TerrainWorkerPhase::BreakerOpen {
            until_ms: NOW + 300_000,
        },
    )
    .await;
    crossing.delay.store(true, Ordering::SeqCst);
    advance(299_999).await;
    w.wake.try_send(()).unwrap();
    settle().await;
    assert_eq!(w.status.borrow().last_pass_at_ms, Some(NOW + 299_999));
    advance(2).await;
    let st = seen_labelled(
        "c9_worker_expired_breaker_deferral",
        &mut rx,
        NOW + 299_999,
        TerrainWorkerPhase::Idle,
    )
    .await;
    assert_eq!(st.filled_since_start, 1);
    drain(&mut rx);
    advance(9_997).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    assert_eq!(
        w.status.borrow().last_pass_at_ms,
        Some(NOW + 309_999),
        "C9 R3: an expired breaker deferral retries at the minimum gap"
    );
    w.shutdown(false).await.unwrap();
}

async fn disabled_condition(test: &str, code: &str) {
    let (d, s, _) = layout69();
    let clock = Arc::new(TokioOffsetClock::new());
    let (mut w, mut rx) = launch(&s, d.path(), false, None, clock.clone(), access());
    w.wait_until_initialized().await.unwrap();
    if code == "store" {
        s.open().unwrap().execute_batch("CREATE TRIGGER c9_fail_tidy BEFORE UPDATE ON terrain_tidy_state BEGIN SELECT RAISE(ABORT,'synthetic tidy'); END;").unwrap();
    } else {
        clock.failure.store(true, Ordering::SeqCst);
    }
    w.start().unwrap();
    settle().await;
    assert!(!w.task.is_finished());
    let states = drain(&mut rx);
    assert_eq!(w.status.borrow().last_error_code, Some(code));
    assert!(
        states
            .iter()
            .all(|st| st.state == TerrainWorkerPhase::Disabled),
        "C9 R57: disabled conditions retain Disabled: {states:?}"
    );
    if code == "store" {
        s.open()
            .unwrap()
            .execute_batch("DROP TRIGGER c9_fail_tidy;")
            .unwrap();
    }
    clock.failure.store(false, Ordering::SeqCst);
    w.wake.try_send(()).unwrap();
    advance(299_999).await;
    assert_eq!(s.terrain_tidy_progress().unwrap().cleared, 0);
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    tidy_observed_labelled(test, &mut rx, &s, 1000, false, NOW + 300_000).await;
    assert_eq!(w.status.borrow().last_pass_at_ms, Some(NOW + 300_000));
    assert_eq!(w.status.borrow().last_error_code, None);
    assert_eq!(w.status.borrow().state, TerrainWorkerPhase::Disabled);
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_disabled_store_condition() {
    let _serial = SERIAL.lock().await;
    disabled_condition("c9_worker_disabled_store_condition", "store").await;
}

#[tokio::test(start_paused = true)]
async fn c9_worker_disabled_clock_condition() {
    let _serial = SERIAL.lock().await;
    disabled_condition("c9_worker_disabled_clock_condition", "clock").await;
}

#[tokio::test(start_paused = true)]
async fn c9_worker_disabled_refused_access() {
    let _serial = SERIAL.lock().await;
    for wake in [true, false] {
        let (d, s, _) = layout69();
        let allowed = access();
        allowed.0.store(false, Ordering::SeqCst);
        let source = Source::new();
        let (mut w, mut rx) = launch(
            &s,
            d.path(),
            false,
            Some(source.clone()),
            Arc::new(TokioOffsetClock::new()),
            allowed.clone(),
        );
        start(&mut w).await;
        settle().await;
        assert_eq!(w.status.borrow().last_pass_at_ms, Some(NOW));
        assert_eq!(w.status.borrow().state, TerrainWorkerPhase::Disabled);
        assert_eq!(w.status.borrow().last_error_code, None);
        assert_eq!(s.terrain_tidy_progress().unwrap().cleared, 0);
        let states = drain(&mut rx);
        assert!(
            states
                .iter()
                .all(|st| st.state == TerrainWorkerPhase::Disabled)
        );
        allowed.0.store(true, Ordering::SeqCst);
        if wake {
            w.wake.try_send(()).unwrap();
        }
        let delay = if wake { 10_000 } else { 3_600_000 };
        advance(delay - 1).await;
        assert!(drain(&mut rx).is_empty());
        advance(1).await;
        tidy_observed_labelled(
            "c9_worker_disabled_refused_access",
            &mut rx,
            &s,
            1000,
            false,
            NOW + delay as i64,
        )
        .await;
        assert_eq!(w.status.borrow().last_pass_at_ms, Some(NOW + delay as i64));
        assert_eq!(w.status.borrow().state, TerrainWorkerPhase::Disabled);
        assert_eq!(source.count(), 0);
        assert!(!w.task.is_finished());
        w.shutdown(false).await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn c9_worker_full_local_continuation() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=1001)
            .map(|id| point(id, 1, if id == 1 { 20.5 } else { 21.5 }, None))
            .collect(),
    );
    let source = Source::new();
    source.defer_after_acquire.store(true, Ordering::SeqCst);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    seen_labelled(
        "c9_worker_full_local_continuation",
        &mut rx,
        NOW,
        TerrainWorkerPhase::Working,
    )
    .await;
    settle().await;
    assert_eq!(w.status.borrow().filled_since_start, 1);
    assert_eq!(source.count(), 1);
    drain(&mut rx);
    advance(1_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    assert_eq!(
        w.status.borrow().last_pass_at_ms,
        Some(NOW + 2_000),
        "C9 R3: a write before local deferral keeps the continuation pause"
    );
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_full_local_no_progress_backoff() {
    let _serial = SERIAL.lock().await;
    let (d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=1001).map(|id| point(id, 1, 20.5, None)).collect(),
    );
    let source = Source::new();
    source.pending.store(true, Ordering::SeqCst);
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    seen_labelled(
        "c9_worker_full_local_no_progress_backoff",
        &mut rx,
        NOW,
        TerrainWorkerPhase::Working,
    )
    .await;
    settle().await;
    drain(&mut rx);
    let mut previous = 0;
    for at in [2_000, 6_000, 14_000] {
        advance(at - previous - 1).await;
        assert!(
            drain(&mut rx).is_empty(),
            "C9 R13: no progress uses local backoff"
        );
        advance(1).await;
        assert_eq!(w.status.borrow().last_pass_at_ms, Some(NOW + at as i64));
        settle().await;
        drain(&mut rx);
        previous = at;
    }
    assert_eq!(source.count(), 0);
    assert_eq!(w.status.borrow().filled_since_start, 0);
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_invalid_config_with_failed_spawn_clock() {
    let _serial = SERIAL.lock().await;
    let (d, s, _) = fixture(false);
    let clock = Arc::new(TokioOffsetClock::new());
    clock.failure.store(true, Ordering::SeqCst);
    let config = TerrainConfig {
        enabled: true,
        min_free_bytes: 0,
        cache_dir: Some(d.path().join("cache")),
        ..Default::default()
    };
    let (mut w, mut rx) = launch_config(&s, d.path(), config, None, clock, access());
    w.wait_until_initialized().await.unwrap();
    assert_eq!(
        w.status.borrow().state,
        TerrainWorkerPhase::Halted {
            code: "invalid_config"
        },
        "C9 R21: invalid configuration is published before start even without a clock"
    );
    assert_eq!(w.status.borrow().last_error_code, Some("invalid_config"));
    assert_eq!(w.status.borrow().last_pass_at_ms, None);
    assert!(drain(&mut rx).iter().any(|st| st.state
        == TerrainWorkerPhase::Halted {
            code: "invalid_config"
        }));
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_rearmed_tidy_with_open_breaker() {
    let _serial = SERIAL.lock().await;
    struct RearmSource {
        source: Arc<Source>,
        cached: AtomicBool,
        store: HubStore,
        vehicle: Uuid,
        imported: Uuid,
    }
    impl TileSource for RearmSource {
        fn acquire<'a>(
            &'a self,
            tile: TileId,
            guard: &'a dyn EgressGuard,
            inner: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>
        {
            Box::pin(async move {
                if tile.name() == "N10E022" && self.cached.swap(false, Ordering::SeqCst) {
                    // C9 R59: cached acquisition commits an import re-arm before the pass can write.
                    raise(&self.store, self.vehicle, self.imported, 4);
                    return self.source.acquire(tile, &Permit, inner).await;
                }
                self.source.acquire(tile, guard, inner).await
            })
        }
    }
    let (d, s, v) = fixture(false);
    let imported = import_source(&s);
    seed(
        &s,
        v,
        (1..=4)
            .map(|id| {
                point(
                    id,
                    1,
                    if id == 4 { 20.5 } else { 19.5 + id as f64 },
                    (id == 4).then_some(100),
                )
            })
            .collect(),
    );
    plant(&s, v, 4, 1, Some(100), "success", 0);
    let source = Source::new();
    for tile in ["N10E020", "N10E021"] {
        source
            .failures
            .lock()
            .unwrap()
            .insert(tile.into(), "provider");
    }
    let source = Arc::new(RearmSource {
        source,
        cached: AtomicBool::new(false),
        store: s.clone(),
        vehicle: v,
        imported,
    });
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(source.clone()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    start(&mut w).await;
    let phase = TerrainWorkerPhase::BreakerOpen {
        until_ms: NOW + 300_000,
    };
    seen_labelled(
        "c9_worker_rearmed_tidy_with_open_breaker",
        &mut rx,
        NOW,
        phase.clone(),
    )
    .await;
    settle().await;
    source.cached.store(true, Ordering::SeqCst);
    w.wake.try_send(()).unwrap();
    advance(10_000).await;
    seen_labelled(
        "c9_worker_rearmed_tidy_with_open_breaker",
        &mut rx,
        NOW + 10_000,
        phase,
    )
    .await;
    settle().await;
    assert!(s.terrain_tidy_progress().unwrap().completed_at_ms.is_none());
    assert!(!w.status.borrow().tidy_complete);
    assert_eq!(w.status.borrow().filled_since_start, 0);
    drain(&mut rx);
    advance(1_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    assert_eq!(
        w.status.borrow().last_pass_at_ms,
        Some(NOW + 12_000),
        "C9 R59: pending tidy continues at two seconds through an open breaker"
    );
    assert!(s.terrain_tidy_progress().unwrap().completed_at_ms.is_some());
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_startup_clock_recovery() {
    let _serial = SERIAL.lock().await;
    let (d, s, _) = fixture(false);
    std::os::unix::fs::symlink(d.path(), d.path().join("cache")).unwrap();
    let clock = Arc::new(TokioOffsetClock::new());
    clock.failure.store(true, Ordering::SeqCst);
    let (mut w, mut rx) = launch(&s, d.path(), true, None, clock.clone(), access());
    w.wait_until_initialized().await.unwrap();
    clock.failure.store(false, Ordering::SeqCst);
    w.start().unwrap();
    settle().await;
    assert_eq!(
        w.status.borrow().state,
        TerrainWorkerPhase::Backoff {
            code: "untrusted_root",
            until_ms: NOW + 300_000
        }
    );
    drain(&mut rx);
    advance(299_999).await;
    assert!(drain(&mut rx).is_empty());
    advance(1).await;
    assert_eq!(
        w.status.borrow().state,
        TerrainWorkerPhase::Backoff {
            code: "untrusted_root",
            until_ms: NOW + 900_000
        }
    );
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_marker_failure_after_start() {
    let _serial = SERIAL.lock().await;
    let (d, s, _) = fixture(false);
    s.open()
        .unwrap()
        .execute_batch("DROP TABLE terrain_tidy_state;")
        .unwrap();
    let (mut w, mut rx) = launch(
        &s,
        d.path(),
        true,
        Some(Source::new()),
        Arc::new(TokioOffsetClock::new()),
        access(),
    );
    w.wait_until_initialized()
        .await
        .expect("C9 R1: the connection opens without reading the marker");
    assert_eq!(w.status.borrow().last_pass_at_ms, None);
    w.start().unwrap();
    seen_labelled(
        "c9_worker_marker_failure_after_start",
        &mut rx,
        NOW,
        TerrainWorkerPhase::Backoff {
            code: "store",
            until_ms: NOW + 300_000,
        },
    )
    .await;
    assert_eq!(w.status.borrow().last_error_code, Some("store"));
    assert!(!w.task.is_finished());
    w.shutdown(false).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c9_worker_phase_log_levels() {
    let log = super::DecisionLog::start();
    let phases = [
        TerrainWorkerPhase::Backoff {
            code: "store",
            until_ms: NOW,
        },
        TerrainWorkerPhase::Halted {
            code: "invalid_config",
        },
        TerrainWorkerPhase::WaitingForAccess,
        TerrainWorkerPhase::BreakerOpen { until_ms: NOW },
    ];
    for phase in phases {
        let mut status = TerrainStatus::new(true);
        status.state = phase;
        super::super::terrain_worker::log_phase(&status);
    }
    assert_eq!(
        log.lines("INFO", "terrain worker phase"),
        4,
        "C9 R63: conditions, the open breaker and refused access are visible at info"
    );
    for phase in [
        TerrainWorkerPhase::Working,
        TerrainWorkerPhase::Idle,
        TerrainWorkerPhase::Disabled,
    ] {
        let mut status = TerrainStatus::new(true);
        status.state = phase;
        super::super::terrain_worker::log_phase(&status);
    }
    assert_eq!(log.lines("DEBUG", "terrain worker phase"), 3);
    let contents = log.contents();
    for forbidden in [
        "latitude",
        "longitude",
        "vehicle_id",
        "position_id",
        "N10E",
        "10.5",
        "20.5",
    ] {
        assert!(!contents.contains(forbidden));
    }
}

#[tokio::test(start_paused = true)]
async fn c9_worker_phase_log_error_dedup() {
    let log = super::DecisionLog::start();
    let (sender, status) = watch::channel(TerrainStatus::new(false));
    let (wake, _wakes) = mpsc::channel(1);
    let mut w = TerrainWorker {
        status,
        logged_phase: None,
        wake,
        initialized: None,
        start: None,
        stop: None,
        task: tokio::spawn(std::future::pending()),
    };
    w.log_status_phase();
    w.log_status_phase();
    assert_eq!(log.lines("DEBUG", "terrain worker phase"), 1);
    sender.send_modify(|status| status.last_error_code = Some("store"));
    w.log_status_phase();
    w.log_status_phase();
    assert_eq!(
        log.lines("DEBUG", "terrain worker phase"),
        2,
        "C9 R63: a changed error code is logged within the same phase"
    );
    sender.send_modify(|status| status.last_error_code = None);
    w.log_status_phase();
    assert_eq!(log.lines("DEBUG", "terrain worker phase"), 3);
}
