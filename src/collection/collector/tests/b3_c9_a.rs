// SPDX-License-Identifier: AGPL-3.0-only

use super::super::*;
use crate::terrain::{SRTM3_SIDE, TERRAIN_DATASET_VERSION};
use crate::terrain_cache::TerrainCacheOptions;
use crate::test_assertions::LabelledResult;
use std::sync::atomic::{AtomicI64, AtomicUsize};

const T0: i64 = 1_760_000_000_000;

struct AcquisitionClock {
    now: AtomicI64,
    failed: AtomicBool,
}
impl AcquisitionClock {
    fn new() -> Self {
        Self {
            now: AtomicI64::new(T0),
            failed: AtomicBool::new(false),
        }
    }
    fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }
}
impl TerrainClock for AcquisitionClock {
    fn now_ms(&self) -> Result<i64, CollectorError> {
        if self.failed.load(Ordering::SeqCst) {
            Err(CollectorError::SystemClockBeforeEpoch)
        } else {
            Ok(self.now.load(Ordering::SeqCst))
        }
    }
}

struct TestGuard(bool);
impl EgressGuard for TestGuard {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        self.0.then_some(()).ok_or(EgressGuardError)
    }
}

fn id(name: &str) -> TileId {
    TileId::parse(name).unwrap()
}
fn bytes() -> Vec<u8> {
    vec![0; SRTM3_SIDE * SRTM3_SIDE * 2]
}
fn hgt(tile: TileId) -> HgtTile {
    HgtTile::from_bytes(&tile.name(), bytes()).unwrap()
}
fn provenance(tile: TileId) -> TileProvenance {
    TileProvenance {
        tile_name: tile.name(),
        tile_hash: hgt(tile).sha256_hex().unwrap(),
        dataset_source: "aws".into(),
        dataset_version: TERRAIN_DATASET_VERSION.into(),
    }
}

#[derive(Clone, Copy)]
enum Script {
    Success,
    Timeout,
    BadResponse,
    Io,
    Integrity,
    Store,
    Input,
    Denied,
}
struct FakeSource {
    script: Script,
    cached: bool,
    guard_calls: usize,
    calls: AtomicUsize,
    network: AtomicUsize,
}
impl FakeSource {
    fn new(script: Script) -> Self {
        Self {
            script,
            cached: false,
            guard_calls: 1,
            calls: AtomicUsize::new(0),
            network: AtomicUsize::new(0),
        }
    }
}
impl TileSource for FakeSource {
    fn acquire<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
        _: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if !self.cached {
                for _ in 0..self.guard_calls {
                    guard.assert_egress_allowed().map_err(|_| AcquireFailure {
                        kind: AcquireErrorKind::Cache(TerrainCacheError::EgressDenied),
                        provenance: None,
                    })?;
                    self.network.fetch_add(1, Ordering::SeqCst);
                }
            }
            let kind = match self.script {
                Script::Success => {
                    return Ok(AcquiredTile {
                        tile: hgt(tile),
                        provenance: provenance(tile),
                    });
                }
                Script::Timeout => AcquireErrorKind::Cache(TerrainCacheError::Timeout),
                Script::BadResponse => AcquireErrorKind::Cache(TerrainCacheError::BadResponse),
                Script::Io => AcquireErrorKind::Cache(TerrainCacheError::Io(
                    std::io::Error::other("synthetic"),
                )),
                Script::Integrity => AcquireErrorKind::Cache(TerrainCacheError::InvalidArchive),
                Script::Store => AcquireErrorKind::Cache(TerrainCacheError::Store(Box::new(
                    StoreError::CreateDataDir(std::io::Error::other("synthetic")),
                ))),
                Script::Input => AcquireErrorKind::Cache(TerrainCacheError::InvalidTile(
                    TerrainError::InvalidLatitude,
                )),
                Script::Denied => AcquireErrorKind::Cache(TerrainCacheError::EgressDenied),
            };
            Err(AcquireFailure {
                kind,
                provenance: None,
            })
        })
    }
}

struct EndStampedSource<'a> {
    source: FakeSource,
    clock: &'a AcquisitionClock,
    end_ms: i64,
}
impl TileSource for EndStampedSource<'_> {
    fn acquire<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
        inner: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>> {
        Box::pin(async move {
            let result = self.source.acquire(tile, guard, inner).await;
            self.clock.set(self.end_ms);
            result
        })
    }
}

struct FakeTileCache {
    name: String,
    hash: String,
    origin: String,
    version: String,
    payload: Vec<u8>,
    hang_get: bool,
    get_error: bool,
    lookups: Mutex<Vec<(f64, f64, Duration)>>,
    gets: Mutex<Vec<TileId>>,
}
impl FakeTileCache {
    fn new(tile: TileId) -> Self {
        Self {
            name: tile.name(),
            hash: hgt(tile).sha256_hex().unwrap(),
            origin: "aws".into(),
            version: TERRAIN_DATASET_VERSION.into(),
            payload: bytes(),
            hang_get: false,
            get_error: false,
            lookups: Mutex::new(Vec::new()),
            gets: Mutex::new(Vec::new()),
        }
    }
}
impl TileCache for FakeTileCache {
    fn has_cached_tile<'a>(
        &'a self,
        _: &'a TileId,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn lookup<'a>(
        &'a self,
        latitude: f64,
        longitude: f64,
        budget: Duration,
        _: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<TerrainLookupResult, TerrainCacheError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.lookups
                .lock()
                .unwrap()
                .push((latitude, longitude, budget));
            Ok(TerrainLookupResult {
                elevation_m: Some(9_999),
                tile_name: self.name.clone(),
                tile_hash: self.hash.clone(),
                dataset_source: self.origin.clone(),
                dataset_version: self.version.clone(),
            })
        })
    }
    fn get<'a>(
        &'a self,
        tile: TileId,
        _: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<HgtTile, TerrainCacheError>> + Send + 'a>> {
        Box::pin(async move {
            self.gets.lock().unwrap().push(tile);
            if self.hang_get {
                std::future::pending::<()>().await;
            }
            if self.get_error {
                return Err(TerrainCacheError::Io(std::io::Error::other(
                    "synthetic get",
                )));
            }
            Ok(HgtTile::from_bytes(&tile.name(), self.payload.clone()).unwrap())
        })
    }
}

fn failed(
    result: GroupAcquisition,
    expected: &'static str,
    expected_class: FailureClass,
) -> Option<TileProvenance> {
    match result {
        GroupAcquisition::Failed {
            code,
            class,
            provenance,
        } => {
            assert_eq!(code, expected);
            assert_eq!(class, expected_class);
            provenance
        }
        _ => panic!("expected failed acquisition"),
    }
}
fn breaker(state: &TerrainWorkerState) -> (BreakerState, i64, Vec<(TileId, i64)>) {
    let b = state.breaker.lock().unwrap();
    (b.state, b.backoff_ms, b.provider_failures.clone())
}
fn half_open() -> TerrainWorkerState {
    let state = TerrainWorkerState::default();
    state.breaker.lock().unwrap().state = BreakerState::HalfOpen { permit_held: false };
    state
}

#[test]
fn c9_st22_schedules() {
    use FailureClass::*;
    // C9 section 5: pin policy values independently of their consumers.
    assert_eq!(K_ACQUIRE_BUDGET, Duration::from_secs(300));
    assert_eq!(K_LOCAL_TILE_BUDGET, Duration::from_secs(30));
    assert_eq!(K_TILE_FAIL_MEMO, 900_000);
    assert_eq!(K_MEMO_ENTRIES, 1_024);
    assert_eq!(K_BREAKER_WINDOW, 900_000);
    assert_eq!(K_BREAKER_BASE, 300_000);
    assert_eq!(K_BREAKER_CAP, 21_600_000);
    assert_eq!(K_RETRY_SHIFT_MAX, 32);
    let cases: &[(FailureClass, &[i64])] = &[
        (
            Provider,
            &[
                900_000,
                1_800_000,
                3_600_000,
                7_200_000,
                14_400_000,
                28_800_000,
                57_600_000,
                115_200_000,
                230_400_000,
                460_800_000,
                604_800_000,
                604_800_000,
            ],
        ),
        (
            Local,
            &[
                3_600_000, 7_200_000, 14_400_000, 28_800_000, 57_600_000, 86_400_000, 86_400_000,
            ],
        ),
        (
            Integrity,
            &[
                21_600_000,
                43_200_000,
                86_400_000,
                172_800_000,
                345_600_000,
                691_200_000,
                1_382_400_000,
                2_592_000_000,
                2_592_000_000,
            ],
        ),
        (
            Absent,
            &[
                604_800_000,
                1_209_600_000,
                2_419_200_000,
                2_592_000_000,
                2_592_000_000,
            ],
        ),
    ];
    for &(class, delays) in cases {
        let classes: &[FailureClass] = if class == Integrity {
            &[Integrity, Input]
        } else {
            &[class]
        };
        for &class in classes {
            let schedule = schedule_for(class).unwrap();
            assert_eq!(schedule.base_ms, delays[0]);
            assert_eq!(schedule.cap_ms, *delays.last().unwrap());
            for (index, delay) in delays.iter().enumerate() {
                assert_eq!(retry_delay_ms(class, index as i64 + 1), *delay);
            }
            for n in delays.len() as i64..=64 {
                assert_eq!(retry_delay_ms(class, n), *delays.last().unwrap());
            }
            assert_eq!(retry_delay_ms(class, i64::MAX), *delays.last().unwrap());
            for n in [0, -1, i64::MIN] {
                assert_eq!(retry_delay_ms(class, n), delays[0]);
            }
        }
    }
    assert!(schedule_for(Worker).is_none());
    assert_eq!(retry_delay_ms(Worker, i64::MAX), 0);
    assert_eq!(
        RetrySchedule {
            base_ms: i64::MAX,
            cap_ms: i64::MAX
        }
        .delay_ms(64),
        i64::MAX
    );
    for (codes, class) in [
        (
            &["timeout", "source_unavailable", "invalid_response"][..],
            Provider,
        ),
        (&["tile_unavailable"][..], Absent),
        (
            &[
                "invalid_archive",
                "unproven_tile",
                "invalid_tile_name",
                "wrong_tile",
                "invalid_hgt_length",
                "x",
                "position_rewritten",
                "locking_unsupported",
            ][..],
            Integrity,
        ),
        (
            &["io", "insufficient_free_space", "cache_quota_exceeded"][..],
            Local,
        ),
        (
            &[
                "nonfinite_coordinate",
                "invalid_latitude",
                "invalid_longitude",
            ][..],
            Input,
        ),
        (&["store", "invalid_config", "untrusted_root"][..], Worker),
    ] {
        for code in codes {
            assert_eq!(failure_class(code), class, "{code}");
        }
    }
}

#[tokio::test]
async fn c9_st48_memo() {
    let a = id("N10E020");
    let clock = AcquisitionClock::new();
    let mut state = TerrainWorkerState::default();
    let source = FakeSource::new(Script::BadResponse);
    for (offset, calls, stamp) in [
        (0, 1, T0),
        (60_000, 1, T0),
        (899_999, 1, T0),
        (900_000, 2, T0 + 900_000),
    ] {
        clock.set(T0 + offset);
        failed(
            acquire_group(&mut state, &source, &TestGuard(true), &clock, a).await,
            "invalid_response",
            FailureClass::Provider,
        );
        assert_eq!(source.calls.load(Ordering::SeqCst), calls);
        assert_eq!(
            breaker(&state),
            (BreakerState::Closed, K_BREAKER_BASE, vec![(a, stamp)])
        );
    }
    let mut memo = TileFailureMemo::default();
    for index in 0..=1_024 {
        let tile =
            TileId::from_coordinates(-80.0 + (index / 360) as f64, -180.0 + (index % 360) as f64)
                .unwrap();
        memo.remember(tile, T0 + index as i64, "io");
    }
    assert_eq!(memo.entries.len(), 1_024);
    assert_eq!(memo.lookup(id("S80W180"), T0 + 1_024), None);
    assert_eq!(memo.lookup(id("S80W179"), T0 + 1_024), Some("io"));
    // C9 R18: refreshing a tile replaces its entry and makes it newest.
    memo.remember(id("S80W179"), T0 + 2_000, "invalid_archive");
    assert_eq!(memo.entries.len(), 1_024);
    assert_eq!(
        memo.entries.back(),
        Some(&(id("S80W179"), T0 + 902_000, "invalid_archive"))
    );
    memo.remember(a, i64::MAX - 1, "io");
    assert_eq!(memo.lookup(a, i64::MAX - 1), Some("io"));
    assert_eq!(memo.lookup(a, i64::MAX), None);
    // C9 R18, C9 R28: expiry and provider history start at acquisition end.
    clock.set(T0);
    let source = EndStampedSource {
        source: FakeSource::new(Script::BadResponse),
        clock: &clock,
        end_ms: T0 + 400_000,
    };
    let mut state = TerrainWorkerState::default();
    failed(
        acquire_group(&mut state, &source, &TestGuard(true), &clock, a).await,
        "invalid_response",
        FailureClass::Provider,
    );
    assert_eq!(
        state.memo.entries.front(),
        Some(&(a, T0 + 1_300_000, "invalid_response"))
    );
    assert_eq!(breaker(&state).2, vec![(a, T0 + 400_000)]);
    clock.set(T0 + 1_299_999);
    let before = breaker(&state);
    failed(
        acquire_group(&mut state, &source, &TestGuard(true), &clock, a).await,
        "invalid_response",
        FailureClass::Provider,
    );
    assert_eq!(source.source.calls.load(Ordering::SeqCst), 1);
    assert_eq!(breaker(&state), before);
    // C9 R18: every scheduled failure is memoised, including local and input.
    for (script, code, class) in [
        (Script::Timeout, "timeout", FailureClass::Provider),
        (
            Script::Integrity,
            "invalid_archive",
            FailureClass::Integrity,
        ),
        (Script::Io, "io", FailureClass::Local),
        (Script::Input, "invalid_latitude", FailureClass::Input),
    ] {
        clock.set(T0);
        let source = FakeSource::new(script);
        let mut state = TerrainWorkerState::default();
        failed(
            acquire_group(&mut state, &source, &TestGuard(true), &clock, a).await,
            code,
            class,
        );
        assert_eq!(state.memo.entries.front(), Some(&(a, T0 + 900_000, code)));
        let before = breaker(&state);
        clock.set(T0 + 1);
        failed(
            acquire_group(&mut state, &source, &TestGuard(true), &clock, a).await,
            code,
            class,
        );
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        assert_eq!(breaker(&state), before);
    }
}

#[tokio::test]
async fn c9_st47_hash() {
    let _serial = super::TERRAIN_CACHE_TEST_SERIAL.lock().await;
    let a = id("N10E020");
    let mut cache = FakeTileCache::new(a);
    // C9 R14: changing only the last node must change the exact-byte hash.
    assert_eq!(
        cache.hash,
        "b2517a7ba04d246ffda4a0f19a4b7e62608ffc9c71a54366308b900d7a97c616"
    );
    let end = cache.payload.len();
    cache.payload[end - 2..].copy_from_slice(&321_i16.to_be_bytes());
    assert_eq!(
        HgtTile::from_bytes(&a.name(), cache.payload.clone())
            .unwrap()
            .sha256_hex()
            .unwrap(),
        "1fcd4a3bfa41a5028a9374a72dbfddf44479cffc6120785639ecd01a417822aa"
    );
    cache.origin = "esa".into();
    cache.version = "synthetic-version".into();
    let source = CacheTileSource { cache };
    let expected = TileProvenance {
        tile_hash: "b2517a7ba04d246ffda4a0f19a4b7e62608ffc9c71a54366308b900d7a97c616".into(),
        dataset_source: "esa".into(),
        dataset_version: "synthetic-version".into(),
        ..provenance(a)
    };
    let result = source.acquire(a, &TestGuard(true), &TestGuard(true)).await;
    result
        .as_ref()
        .map_err(|_| ())
        .labelled_err("c9_st47_hash", "mismatched exact-byte tile hash is refused");
    let error = match result {
        Err(error) => error,
        Ok(_) => unreachable!("labelled refusal assertion passed"),
    };
    assert!(matches!(error.kind, AcquireErrorKind::HashMismatch));
    assert_eq!(error.provenance, Some(expected.clone()));
    let mut state = TerrainWorkerState::default();
    assert_eq!(
        failed(
            acquire_group(
                &mut state,
                &source,
                &TestGuard(true),
                &AcquisitionClock::new(),
                a
            )
            .await,
            "unproven_tile",
            FailureClass::Integrity
        ),
        Some(expected.clone())
    );
    assert_eq!(
        breaker(&state),
        (BreakerState::Closed, K_BREAKER_BASE, vec![])
    );
    // C9 R49, C9 R53: a matching hash keeps all four provenance fields unchanged.
    let mut cache = source.cache;
    cache.hash = "1fcd4a3bfa41a5028a9374a72dbfddf44479cffc6120785639ecd01a417822aa".into();
    let source = CacheTileSource { cache };
    let acquired = source
        .acquire(a, &TestGuard(true), &TestGuard(true))
        .await
        .ok()
        .unwrap();
    assert_eq!(acquired.tile.try_cell(1_200, 1_200).unwrap(), Some(321));
    assert_eq!(
        acquired.provenance,
        TileProvenance {
            tile_hash: "1fcd4a3bfa41a5028a9374a72dbfddf44479cffc6120785639ecd01a417822aa".into(),
            ..expected
        }
    );
    // C9 R45: a get failure carries none of the successful lookup's provenance.
    let mut cache = FakeTileCache::new(a);
    cache.get_error = true;
    let source = CacheTileSource { cache };
    let error = source
        .acquire(a, &TestGuard(true), &TestGuard(true))
        .await
        .err()
        .unwrap();
    assert!(matches!(
        error.kind,
        AcquireErrorKind::Cache(TerrainCacheError::Io(_))
    ));
    assert_eq!(error.kind.code(), "io");
    assert_eq!(error.provenance, None);
    let mut state = TerrainWorkerState::default();
    assert_eq!(
        failed(
            acquire_group(
                &mut state,
                &source,
                &TestGuard(true),
                &AcquisitionClock::new(),
                a,
            )
            .await,
            "io",
            FailureClass::Local,
        ),
        None
    );
    assert_eq!(*source.cache.gets.lock().unwrap(), vec![a, a]);
}

#[tokio::test(start_paused = true)]
async fn c9_st58_tile_centre() {
    let _serial = super::TERRAIN_CACHE_TEST_SERIAL.lock().await;
    let a = terrain_tile_for(11.0, 20.5).unwrap();
    assert_eq!(a.name(), "N11E020");
    let source = CacheTileSource {
        cache: FakeTileCache::new(a),
    };
    let acquired = source
        .acquire(a, &TestGuard(true), &TestGuard(true))
        .await
        .ok()
        .unwrap();
    assert_eq!(
        *source.cache.lookups.lock().unwrap(),
        vec![(11.5, 20.5, Duration::from_secs(300))]
    );
    assert_eq!(*source.cache.gets.lock().unwrap(), vec![a]);
    assert_eq!(acquired.tile.tile_id(), a);
    assert_eq!(acquired.provenance, provenance(a));
    let mut cache = FakeTileCache::new(a);
    cache.name = "N10E020".into();
    let wrong = CacheTileSource { cache };
    let result = wrong.acquire(a, &TestGuard(true), &TestGuard(true)).await;
    result
        .as_ref()
        .map_err(|_| ())
        .labelled_err("c9_st58_tile_centre", "mismatched tile identity is refused");
    let error = match result {
        Err(error) => error,
        Ok(_) => unreachable!("labelled refusal assertion passed"),
    };
    assert!(matches!(error.kind, AcquireErrorKind::TileMismatch));
    assert_eq!(error.kind.code(), "wrong_tile");
    assert_eq!(error.provenance, None);
    assert!(wrong.cache.gets.lock().unwrap().is_empty());
    // C9 R14: normalisation at negative edges, the antimeridian and both poles.
    for (latitude, longitude, name, centre) in [
        (-11.0, -20.5, "S11W021", (-10.5, -20.5)),
        (10.5, 180.0, "N10W180", (10.5, -179.5)),
        (90.0, 179.9, "N89E179", (89.5, 179.5)),
        (-90.0, -180.0, "S90W180", (-89.5, -179.5)),
    ] {
        let tile = terrain_tile_for(latitude, longitude).unwrap();
        assert_eq!(tile.name(), name);
        let source = CacheTileSource {
            cache: FakeTileCache::new(tile),
        };
        assert!(
            source
                .acquire(tile, &TestGuard(true), &TestGuard(true))
                .await
                .is_ok()
        );
        assert_eq!(
            *source.cache.lookups.lock().unwrap(),
            vec![(centre.0, centre.1, Duration::from_secs(300))]
        );
        assert_eq!(*source.cache.gets.lock().unwrap(), vec![tile]);
    }
    let mut cache = FakeTileCache::new(a);
    cache.hang_get = true;
    let hanging = CacheTileSource { cache };
    let started = Instant::now();
    let future = hanging.acquire(a, &TestGuard(true), &TestGuard(true));
    tokio::pin!(future);
    assert!(futures_util::poll!(future.as_mut()).is_pending());
    tokio::time::advance(Duration::from_millis(29_999)).await;
    assert!(futures_util::poll!(future.as_mut()).is_pending());
    tokio::time::advance(Duration::from_millis(1)).await;
    let error = future.await.err().unwrap();
    assert_eq!(Instant::now() - started, Duration::from_secs(30));
    assert!(matches!(error.kind, AcquireErrorKind::LocalTimeout));
    assert_eq!(error.kind.code(), "io");
    assert_eq!(error.provenance, None);
    assert_eq!(*hanging.cache.gets.lock().unwrap(), vec![a]);
}

#[tokio::test]
async fn c9_st33_cache_guard() {
    let _serial = super::TERRAIN_CACHE_TEST_SERIAL.lock().await;
    use axum::{
        Router,
        body::Body,
        http::{StatusCode, Uri},
        response::Response,
        routing::any,
    };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let counter = requests.clone();
    // C9 ST33: the cache-level server uses synthetic bytes and loopback only.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/{*path}",
        any(move |uri: Uri| {
            counter.lock().unwrap().push(uri.path().to_owned());
            async {
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::empty())
                    .unwrap()
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let data = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(data.path()).unwrap();
    let root = crate::private_tempdir().unwrap();
    let a = id("N10E020");
    let b = id("N10E021");
    std::fs::write(root.path().join("N10E020.hgt"), bytes()).unwrap();
    std::fs::write(root.path().join("N10E020.source"), "esa").unwrap();
    store
        .record_terrain_tile_digest(&a.name(), &hgt(a).sha256_hex().unwrap(), T0)
        .unwrap();
    let cache = TerrainCache::new(TerrainCacheOptions {
        root: root.path().to_owned(),
        store: store.clone(),
        min_free_bytes: 1,
        max_cache_bytes: 512 * 1024 * 1024,
        connect_timeout: Duration::from_secs(2),
        read_timeout: Duration::from_secs(2),
        aws_base: format!("{endpoint}aws/"),
        esa_base: format!("{endpoint}esa/"),
        free_space_override: Some(u64::MAX),
    })
    .unwrap();
    let source = CacheTileSource { cache };
    let cache = &source.cache;
    let clock = AcquisitionClock::new();
    let refuse = TestGuard(false);
    let mut state = TerrainWorkerState::default();
    let before = breaker(&state);
    {
        let guard = BreakerGuard::new(&state.breaker, &refuse, &clock);
        // C9 ST33: cached reads retain the post-lock admission check.
        assert!(matches!(
            cache
                .lookup_with_egress_guard(10.5, 20.5, Duration::from_secs(2), &guard)
                .await,
            Err(TerrainCacheError::EgressDenied)
        ));
        assert!(matches!(
            cache.get_with_egress_guard(a, &guard).await,
            Err(TerrainCacheError::EgressDenied)
        ));
    }
    assert_eq!(breaker(&state), before);
    assert!(matches!(
        cache
            .lookup_with_egress_guard(10.5, 21.5, Duration::from_secs(2), &TestGuard(false))
            .await,
        Err(TerrainCacheError::EgressDenied)
    ));
    assert!(matches!(
        cache.get_with_egress_guard(b, &TestGuard(false)).await,
        Err(TerrainCacheError::EgressDenied)
    ));
    assert!(requests.lock().unwrap().is_empty());
    // C9 R20: both adapter methods forward denial; no memo or breaker penalty.
    {
        let guard = BreakerGuard::new(&state.breaker, &refuse, &clock);
        assert!(matches!(
            TileCache::lookup(cache, 10.5, 20.5, Duration::from_secs(2), &guard).await,
            Err(TerrainCacheError::EgressDenied)
        ));
        assert!(matches!(
            TileCache::get(cache, a, &guard).await,
            Err(TerrainCacheError::EgressDenied)
        ));
    }
    assert_eq!(breaker(&state), before);
    for tile in [a, b] {
        for initial in [
            BreakerState::Closed,
            BreakerState::HalfOpen { permit_held: false },
        ] {
            state.breaker.lock().unwrap().state = initial;
            let before = breaker(&state);
            assert!(matches!(
                acquire_group(&mut state, &source, &refuse, &clock, tile).await,
                GroupAcquisition::AccessDenied
            ));
            assert_eq!(breaker(&state), before);
            assert!(state.memo.entries.is_empty());
        }
    }
    assert!(requests.lock().unwrap().is_empty());
    // C9 R19, C9 R20, C9 R22: real cached reads never consult the breaker.
    assert!(cache.has_cached_tile(&a).await);
    assert!(!cache.has_cached_tile(&b).await);
    let allow = TestGuard(true);
    for initial in [
        BreakerState::Open {
            until_ms: T0 + 10_000,
        },
        BreakerState::HalfOpen { permit_held: false },
    ] {
        let mut state = TerrainWorkerState::default();
        {
            let mut breaker = state.breaker.lock().unwrap();
            breaker.state = initial;
            breaker.backoff_ms = 1_200_000;
            breaker.provider_failures = vec![(b, T0 - 1)];
        }
        let before = breaker(&state);
        {
            let guard = BreakerGuard::new(&state.breaker, &allow, &clock);
            let acquired = source.acquire(a, &guard, &allow).await.ok().unwrap();
            assert_eq!(acquired.tile.tile_id(), a);
            assert!(!guard.consulted.load(Ordering::SeqCst));
            assert!(!guard.holds_permit.load(Ordering::SeqCst));
            assert!(!guard.denied_by_breaker.load(Ordering::SeqCst));
        }
        assert!(matches!(
            acquire_group(&mut state, &source, &allow, &clock, a).await,
            GroupAcquisition::Tile(_)
        ));
        assert_eq!(breaker(&state), before);
        assert!(state.memo.entries.is_empty());
        assert!(requests.lock().unwrap().is_empty());
        assert!(matches!(
            acquire_group(&mut state, &source, &refuse, &clock, a).await,
            GroupAcquisition::AccessDenied
        ));
        assert_eq!(breaker(&state), before);
        assert!(state.memo.entries.is_empty());
        assert!(requests.lock().unwrap().is_empty());
    }
    // C9 R20: an uncached tile is deferred by an open breaker before admission.
    let mut state = TerrainWorkerState::default();
    state.breaker.lock().unwrap().state = BreakerState::Open {
        until_ms: T0 + 10_000,
    };
    let before = breaker(&state);
    {
        let guard = BreakerGuard::new(&state.breaker, &allow, &clock);
        let error = source.acquire(b, &guard, &allow).await.err().unwrap();
        assert!(matches!(
            error.kind,
            AcquireErrorKind::Cache(TerrainCacheError::EgressDenied)
        ));
        assert!(guard.denied_by_breaker.load(Ordering::SeqCst));
        assert!(!guard.denied_by_inner.load(Ordering::SeqCst));
        assert!(!guard.consulted.load(Ordering::SeqCst));
    }
    assert!(matches!(
        acquire_group(&mut state, &source, &allow, &clock, b).await,
        GroupAcquisition::Deferred
    ));
    assert_eq!(breaker(&state), before);
    assert!(state.memo.entries.is_empty());
    assert!(requests.lock().unwrap().is_empty());
    // C9 R19, C9 ST33: unproven files stay untouched while the breaker is open.
    for digest in [None, Some("0".repeat(64))] {
        let path = root.path().join("N10E021.hgt");
        std::fs::write(&path, bytes()).unwrap();
        if let Some(digest) = digest {
            store
                .record_terrain_tile_digest(&b.name(), &digest, T0)
                .unwrap();
        } else {
            assert!(store.terrain_tile_digest(&b.name()).unwrap().is_none());
        }
        assert!(!cache.has_cached_tile(&b).await);
        assert!(matches!(
            acquire_group(&mut state, &source, &allow, &clock, b).await,
            GroupAcquisition::Deferred
        ));
        assert!(requests.lock().unwrap().is_empty());
        assert!(state.memo.entries.is_empty());
        assert_eq!(breaker(&state), before);
        assert!(path.is_file());
        assert_eq!(std::fs::read(&path).unwrap(), bytes());
    }
    std::fs::remove_file(root.path().join("N10E021.hgt")).unwrap();
    // C9 R19, C9 ST33: a replaced file cannot reuse a stale memoised digest.
    let path = root.path().join("N10E020.hgt");
    let replacement = root.path().join("replacement.hgt");
    let mut changed_bytes = bytes();
    changed_bytes[0] = 1;
    std::fs::write(&replacement, &changed_bytes).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert!(!cache.has_cached_tile(&a).await);
    assert!(matches!(
        acquire_group(&mut state, &source, &allow, &clock, a).await,
        GroupAcquisition::Deferred
    ));
    assert!(requests.lock().unwrap().is_empty());
    assert!(state.memo.entries.is_empty());
    assert_eq!(breaker(&state), before);
    assert!(path.is_file());
    assert_eq!(std::fs::read(&path).unwrap(), changed_bytes);
    std::fs::write(&path, bytes()).unwrap();
    // C9 R49, C9 R53: cached origins and the fallback vocabulary pass through.
    for (marker, origin) in [
        (Some("aws"), "aws"),
        (Some("esa"), "esa"),
        (None, "cache"),
        (Some("unknown"), "cache"),
    ] {
        let path = root.path().join("N10E020.source");
        if let Some(marker) = marker {
            std::fs::write(path, marker).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        let acquired = source
            .acquire(a, &TestGuard(true), &TestGuard(true))
            .await
            .ok()
            .unwrap();
        assert_eq!(
            acquired.provenance,
            TileProvenance {
                dataset_source: origin.into(),
                ..provenance(a)
            }
        );
    }
    assert!(requests.lock().unwrap().is_empty());
    // C9 R48: permitting controls exercise AWS first, then ESA for both APIs.
    assert!(matches!(
        cache
            .lookup_with_egress_guard(10.5, 21.5, Duration::from_secs(2), &TestGuard(true))
            .await,
        Err(TerrainCacheError::BadResponse)
    ));
    assert!(matches!(
        cache.get_with_egress_guard(b, &TestGuard(true)).await,
        Err(TerrainCacheError::BadResponse)
    ));
    assert_eq!(
        *requests.lock().unwrap(),
        vec![
            "/aws/skadi/N10/N10E021.hgt.gz",
            "/esa/N10E021.hgt.zip",
            "/aws/skadi/N10/N10E021.hgt.gz",
            "/esa/N10E021.hgt.zip",
        ]
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[test]
fn c9_st46_acquisition_codes() {
    assert_eq!(AcquireErrorKind::LocalTimeout.code(), "io");
    assert_eq!(AcquireErrorKind::TileMismatch.code(), "wrong_tile");
    assert_eq!(AcquireErrorKind::HashMismatch.code(), "unproven_tile");
}

#[tokio::test]
async fn c9_st53_breaker() {
    let a = id("N10E020");
    let b = id("N10E021");
    let clock = AcquisitionClock::new();
    let allow = TestGuard(true);
    let mut state = TerrainWorkerState::default();
    // C9 ST53(a): distinct provider failures open at acquisition end.
    failed(
        acquire_group(
            &mut state,
            &FakeSource::new(Script::Timeout),
            &allow,
            &clock,
            a,
        )
        .await,
        "timeout",
        FailureClass::Provider,
    );
    clock.set(T0 + 60_000);
    failed(
        acquire_group(
            &mut state,
            &FakeSource::new(Script::BadResponse),
            &allow,
            &clock,
            b,
        )
        .await,
        "invalid_response",
        FailureClass::Provider,
    );
    assert_eq!(
        breaker(&state).0,
        BreakerState::Open {
            until_ms: T0 + 360_000
        }
    );
    // C9 ST53(b): one tile never supplies two distinct failures.
    let mut state = TerrainWorkerState::default();
    clock.set(T0);
    failed(
        acquire_group(
            &mut state,
            &FakeSource::new(Script::Timeout),
            &allow,
            &clock,
            a,
        )
        .await,
        "timeout",
        FailureClass::Provider,
    );
    clock.set(T0 + 960_000);
    failed(
        acquire_group(
            &mut state,
            &FakeSource::new(Script::Timeout),
            &allow,
            &clock,
            a,
        )
        .await,
        "timeout",
        FailureClass::Provider,
    );
    assert_eq!(
        breaker(&state),
        (
            BreakerState::Closed,
            K_BREAKER_BASE,
            vec![(a, T0 + 960_000)]
        )
    );
    // C9 ST53(c): local failure opens immediately.
    let mut state = TerrainWorkerState::default();
    clock.set(T0);
    failed(
        acquire_group(&mut state, &FakeSource::new(Script::Io), &allow, &clock, a).await,
        "io",
        FailureClass::Local,
    );
    assert_eq!(
        breaker(&state).0,
        BreakerState::Open {
            until_ms: T0 + 300_000
        }
    );
    // C9 ST53(d,e,f): holder closes or reopens with doubled backoff.
    for script in [
        Script::Success,
        Script::Integrity,
        Script::Timeout,
        Script::Io,
    ] {
        let mut state = half_open();
        {
            let mut breaker = state.breaker.lock().unwrap();
            breaker.backoff_ms = 600_000;
            breaker.provider_failures.push((b, T0 - 1));
        }
        let result = acquire_group(&mut state, &FakeSource::new(script), &allow, &clock, a).await;
        match script {
            Script::Success => match result {
                GroupAcquisition::Tile(acquired) => {
                    assert_eq!(acquired.tile.tile_id(), a);
                    assert_eq!(acquired.provenance, provenance(a));
                }
                _ => panic!("expected acquired tile"),
            },
            Script::Integrity => {
                failed(result, "invalid_archive", FailureClass::Integrity);
            }
            Script::Timeout => {
                failed(result, "timeout", FailureClass::Provider);
            }
            Script::Io => {
                failed(result, "io", FailureClass::Local);
            }
            _ => unreachable!(),
        }
        if matches!(script, Script::Timeout | Script::Io) {
            assert_eq!(
                breaker(&state).0,
                BreakerState::Open {
                    until_ms: T0 + 1_200_000
                }
            );
            assert_eq!(breaker(&state).1, 1_200_000);
        } else {
            assert_eq!(
                breaker(&state),
                (BreakerState::Closed, K_BREAKER_BASE, vec![])
            );
        }
    }
    // C9 ST53(g,h): worker and input failures return ownership.
    for script in [Script::Store, Script::Input] {
        let mut state = half_open();
        let result = acquire_group(&mut state, &FakeSource::new(script), &allow, &clock, a).await;
        if matches!(script, Script::Store) {
            assert!(matches!(result, GroupAcquisition::Worker { code: "store" }));
            assert!(state.memo.entries.is_empty());
        } else {
            failed(result, "invalid_latitude", FailureClass::Input);
        }
        assert_eq!(
            breaker(&state).0,
            BreakerState::HalfOpen { permit_held: false }
        );
        let next = FakeSource::new(Script::Success);
        assert!(matches!(
            acquire_group(&mut state, &next, &allow, &clock, b).await,
            GroupAcquisition::Tile(_)
        ));
        assert_eq!(next.network.load(Ordering::SeqCst), 1);
        assert_eq!(breaker(&state).0, BreakerState::Closed);
    }
    // C9 ST53(i): cached results do not settle the breaker.
    for initial in [
        BreakerState::HalfOpen { permit_held: false },
        BreakerState::Open {
            until_ms: T0 + 10_000,
        },
    ] {
        let mut state = TerrainWorkerState::default();
        state.breaker.lock().unwrap().state = initial;
        let mut source = FakeSource::new(Script::Success);
        source.cached = true;
        assert!(matches!(
            acquire_group(&mut state, &source, &TestGuard(false), &clock, a).await,
            GroupAcquisition::Tile(_)
        ));
        assert_eq!(breaker(&state).0, initial);
        assert_eq!(source.network.load(Ordering::SeqCst), 0);
    }
    // C9 R19: guard-free local and provider failures leave Closed unchanged.
    for script in [Script::Io, Script::Timeout] {
        let mut state = TerrainWorkerState::default();
        let mut source = FakeSource::new(script);
        source.cached = true;
        clock.set(T0);
        let (code, class) = if matches!(script, Script::Io) {
            ("io", FailureClass::Local)
        } else {
            ("timeout", FailureClass::Provider)
        };
        for (tile, now) in [(a, T0), (b, T0 + 60_000)] {
            clock.set(now);
            assert_eq!(
                failed(
                    acquire_group(&mut state, &source, &TestGuard(false), &clock, tile).await,
                    code,
                    class,
                ),
                None
            );
            assert_eq!(
                breaker(&state),
                (BreakerState::Closed, K_BREAKER_BASE, vec![])
            );
            assert_eq!(source.network.load(Ordering::SeqCst), 0);
        }
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    }
    clock.set(T0);
    // C9 ST53(j): a memo hit makes no breaker transition.
    let mut state = TerrainWorkerState::default();
    state.memo.remember(a, T0, "timeout");
    let source = FakeSource::new(Script::Success);
    failed(
        acquire_group(&mut state, &source, &allow, &clock, a).await,
        "timeout",
        FailureClass::Provider,
    );
    assert_eq!(
        breaker(&state),
        (BreakerState::Closed, K_BREAKER_BASE, vec![])
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    // C9 ST53(k): both calls belong to one holder.
    let mut state = half_open();
    let mut source = FakeSource::new(Script::Success);
    source.guard_calls = 2;
    assert!(matches!(
        acquire_group(&mut state, &source, &allow, &clock, a).await,
        GroupAcquisition::Tile(_)
    ));
    assert_eq!(source.network.load(Ordering::SeqCst), 2);
    assert_eq!(breaker(&state).0, BreakerState::Closed);
    // C9 ST53(l): flags are fresh, expiry exact, a competitor cannot take a held permit.
    let mutex = Mutex::new(AcquisitionBreaker {
        state: BreakerState::Open { until_ms: T0 + 1 },
        ..Default::default()
    });
    let first = BreakerGuard::new(&mutex, &allow, &clock);
    assert!(first.assert_egress_allowed().is_err());
    assert!(first.denied_by_breaker.load(Ordering::SeqCst));
    drop(first);
    clock.set(T0 + 1);
    let holder = BreakerGuard::new(&mutex, &allow, &clock);
    assert!(!holder.denied_by_breaker.load(Ordering::SeqCst));
    assert!(!holder.denied_by_inner.load(Ordering::SeqCst));
    assert!(holder.assert_egress_allowed().is_ok());
    assert!(holder.assert_egress_allowed().is_ok());
    let other = BreakerGuard::new(&mutex, &allow, &clock);
    assert!(other.assert_egress_allowed().is_err());
    assert!(other.denied_by_breaker.load(Ordering::SeqCst));
    assert!(!other.denied_by_inner.load(Ordering::SeqCst));
    drop(other);
    drop(holder);
    assert_eq!(
        mutex.lock().unwrap().state,
        BreakerState::HalfOpen { permit_held: false }
    );
    let mut state = TerrainWorkerState::default();
    state.breaker.lock().unwrap().state = BreakerState::Open {
        until_ms: T0 + 10_000,
    };
    assert!(matches!(
        acquire_group(&mut state, &source, &allow, &clock, a).await,
        GroupAcquisition::Deferred
    ));
    assert!(state.memo.entries.is_empty());
    // C9 ST53(m): admission and unflagged denial are never provider failures.
    let refused_mutex = Mutex::new(AcquisitionBreaker::default());
    let refuse = TestGuard(false);
    let refused = BreakerGuard::new(&refused_mutex, &refuse, &clock);
    assert!(refused.assert_egress_allowed().is_err());
    assert!(refused.denied_by_inner.load(Ordering::SeqCst));
    assert!(!refused.denied_by_breaker.load(Ordering::SeqCst));
    for initial in [
        BreakerState::Closed,
        BreakerState::HalfOpen { permit_held: false },
    ] {
        let mut state = TerrainWorkerState::default();
        state.breaker.lock().unwrap().state = initial;
        assert!(matches!(
            acquire_group(&mut state, &source, &TestGuard(false), &clock, a).await,
            GroupAcquisition::AccessDenied
        ));
        assert_eq!(breaker(&state), (initial, K_BREAKER_BASE, vec![]));
        assert!(state.memo.entries.is_empty());
    }
    let mut state = TerrainWorkerState::default();
    let mut denied = FakeSource::new(Script::Denied);
    denied.cached = true;
    assert!(matches!(
        acquire_group(&mut state, &denied, &allow, &clock, a).await,
        GroupAcquisition::AccessDenied
    ));
    assert!(state.memo.entries.is_empty());
    // C9 R19: the provider window includes its endpoint, never future timestamps.
    for (prior, opens) in [(T0 - 900_000, true), (T0 - 900_001, false), (T0 + 1, false)] {
        let mut window = AcquisitionBreaker::default();
        window.provider_failures.push((a, prior));
        window.settle(b, T0, false, Some(FailureClass::Provider));
        assert_eq!(matches!(window.state, BreakerState::Open { .. }), opens);
    }
    // C9 R19: even repeated failures inside the window count only one tile.
    let mut same = AcquisitionBreaker::default();
    for offset in [0, 1, 2] {
        same.settle(a, T0 + offset, false, Some(FailureClass::Provider));
        assert_eq!(same.state, BreakerState::Closed);
        assert_eq!(same.provider_failures, vec![(a, T0 + offset)]);
    }
    // C9 section 6.2: unrelated closed outcomes leave existing history untouched.
    for class in [
        None,
        Some(FailureClass::Absent),
        Some(FailureClass::Integrity),
        Some(FailureClass::Input),
        Some(FailureClass::Worker),
    ] {
        let mut unchanged = AcquisitionBreaker {
            provider_failures: vec![(b, T0 - 1)],
            ..Default::default()
        };
        unchanged.settle(a, T0, false, class);
        assert_eq!(unchanged.state, BreakerState::Closed);
        assert_eq!(unchanged.backoff_ms, 300_000);
        assert_eq!(unchanged.provider_failures, vec![(b, T0 - 1)]);
    }
    // C9 R19: absent closes, cap saturates, and dropping a pending acquisition returns its permit.
    let state = half_open();
    {
        let mut b = state.breaker.lock().unwrap();
        b.settle(a, T0, true, Some(FailureClass::Absent));
    }
    assert_eq!(
        breaker(&state),
        (BreakerState::Closed, K_BREAKER_BASE, vec![])
    );
    let mut b = AcquisitionBreaker {
        state: BreakerState::HalfOpen { permit_held: true },
        backoff_ms: K_BREAKER_CAP,
        ..Default::default()
    };
    b.settle(a, i64::MAX - 1, true, Some(FailureClass::Provider));
    assert_eq!(b.state, BreakerState::Open { until_ms: i64::MAX });
    assert_eq!(b.backoff_ms, K_BREAKER_CAP);
    // C9 section 6.2: the backoff reaches and stays at the exact cap.
    b.backoff_ms = 300_000;
    for backoff in [
        600_000, 1_200_000, 2_400_000, 4_800_000, 9_600_000, 19_200_000, 21_600_000, 21_600_000,
    ] {
        b.state = BreakerState::HalfOpen { permit_held: true };
        b.settle(a, T0, true, Some(FailureClass::Provider));
        assert_eq!(b.backoff_ms, backoff);
        assert_eq!(
            b.state,
            BreakerState::Open {
                until_ms: T0 + backoff
            }
        );
    }
    struct PendingSource;
    impl TileSource for PendingSource {
        fn acquire<'a>(
            &'a self,
            _: TileId,
            guard: &'a dyn EgressGuard,
            _: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>
        {
            Box::pin(async move {
                guard.assert_egress_allowed().unwrap();
                std::future::pending().await
            })
        }
    }
    let mut state = half_open();
    {
        let mut future = Box::pin(acquire_group(&mut state, &PendingSource, &allow, &clock, a));
        assert!(futures_util::poll!(future.as_mut()).is_pending());
    }
    assert_eq!(
        breaker(&state).0,
        BreakerState::HalfOpen { permit_held: false }
    );
    assert!(state.memo.entries.is_empty());
    assert!(matches!(
        acquire_group(
            &mut state,
            &FakeSource::new(Script::Success),
            &allow,
            &clock,
            id("N10E021")
        )
        .await,
        GroupAcquisition::Tile(_)
    ));
    // C9 R19: unwinding poisons the mutex before Drop; ownership still returns.
    let poisoned = Mutex::new(AcquisitionBreaker {
        state: BreakerState::HalfOpen { permit_held: false },
        ..Default::default()
    });
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let guard = BreakerGuard::new(&poisoned, &allow, &clock);
        guard.assert_egress_allowed().unwrap();
        let _lock = poisoned.lock().unwrap();
        panic!("synthetic acquisition unwind");
    }));
    assert!(unwind.is_err());
    {
        let recovered = poisoned.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(
            recovered.state,
            BreakerState::HalfOpen { permit_held: false }
        );
    }
    // C9 R28: a local opening uses the end time, never the pre-acquisition read.
    clock.set(T0);
    let mut state = TerrainWorkerState::default();
    failed(
        acquire_group(
            &mut state,
            &EndStampedSource {
                source: FakeSource::new(Script::Io),
                clock: &clock,
                end_ms: T0 + 400_000,
            },
            &allow,
            &clock,
            a,
        )
        .await,
        "io",
        FailureClass::Local,
    );
    assert_eq!(
        breaker(&state).0,
        BreakerState::Open {
            until_ms: T0 + 700_000
        }
    );
    clock.set(T0);
    // C9 R28: a transient failure inside a guard remains a worker condition.
    struct GuardClockFailureSource<'a>(&'a AcquisitionClock);
    impl TileSource for GuardClockFailureSource<'_> {
        fn acquire<'a>(
            &'a self,
            _: TileId,
            guard: &'a dyn EgressGuard,
            _: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>
        {
            Box::pin(async move {
                guard.assert_egress_allowed().unwrap();
                self.0.failed.store(true, Ordering::SeqCst);
                assert!(guard.assert_egress_allowed().is_err());
                self.0.failed.store(false, Ordering::SeqCst);
                Err(AcquireFailure {
                    kind: AcquireErrorKind::Cache(TerrainCacheError::EgressDenied),
                    provenance: None,
                })
            })
        }
    }
    let mut state = half_open();
    assert!(matches!(
        acquire_group(
            &mut state,
            &GuardClockFailureSource(&clock),
            &allow,
            &clock,
            a
        )
        .await,
        GroupAcquisition::Worker { code: "clock" }
    ));
    assert_eq!(
        breaker(&state).0,
        BreakerState::HalfOpen { permit_held: false }
    );
    assert!(state.memo.entries.is_empty());
    // C9 R28: a clock failure has no memo or breaker effect and returns ownership.
    struct ClockFailureSource<'a>(&'a AcquisitionClock);
    impl TileSource for ClockFailureSource<'_> {
        fn acquire<'a>(
            &'a self,
            _: TileId,
            guard: &'a dyn EgressGuard,
            _: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>
        {
            Box::pin(async move {
                guard.assert_egress_allowed().unwrap();
                self.0.failed.store(true, Ordering::SeqCst);
                Err(AcquireFailure {
                    kind: AcquireErrorKind::LocalTimeout,
                    provenance: None,
                })
            })
        }
    }
    let mut state = half_open();
    assert!(matches!(
        acquire_group(&mut state, &ClockFailureSource(&clock), &allow, &clock, a).await,
        GroupAcquisition::Worker { code: "clock" }
    ));
    assert_eq!(
        breaker(&state).0,
        BreakerState::HalfOpen { permit_held: false }
    );
    assert!(state.memo.entries.is_empty());
}
