// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(test)]
use crate::db::K_RETRY_SHIFT_MAX;
use crate::{
    db::{RetrySchedule, TileProvenance},
    geocoder::{EgressGuard, EgressGuardError},
    terrain_cache::TerrainLookupResult,
};
use std::{collections::VecDeque, pin::Pin, sync::Mutex};

const K_ACQUIRE_BUDGET: Duration = Duration::from_secs(300);
const K_LOCAL_TILE_BUDGET: Duration = Duration::from_secs(30);
const K_TILE_FAIL_MEMO: i64 = 900_000;
const K_MEMO_ENTRIES: usize = 1_024;
const K_BREAKER_WINDOW: i64 = 900_000;
const K_BREAKER_BASE: i64 = 300_000;
const K_BREAKER_CAP: i64 = 21_600_000;

pub(crate) trait TileSource: Send + Sync {
    /// C9 R13: a timed-out local task must release its tile before another load.
    fn has_pending_local_work(&self) -> bool {
        false
    }
    fn acquire<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
        inner: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>;
}

pub(crate) struct AcquiredTile {
    pub tile: HgtTile,
    pub provenance: TileProvenance,
}
pub(crate) struct AcquireFailure {
    pub kind: AcquireErrorKind,
    pub provenance: Option<TileProvenance>,
}

#[derive(Debug)]
pub(crate) enum AcquireErrorKind {
    Cache(TerrainCacheError),
    LocalTimeout,
    TileMismatch,
    HashMismatch,
}

impl AcquireErrorKind {
    fn code(&self) -> &'static str {
        match self {
            Self::Cache(error) => terrain_error_code(error),
            Self::LocalTimeout => "io",
            Self::TileMismatch => "wrong_tile",
            Self::HashMismatch => "unproven_tile",
        }
    }
}

pub(crate) trait TileCache: Send + Sync {
    fn has_cached_tile<'a>(
        &'a self,
        tile: &'a TileId,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
    fn lookup<'a>(
        &'a self,
        latitude: f64,
        longitude: f64,
        budget: Duration,
        guard: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<TerrainLookupResult, TerrainCacheError>> + Send + 'a>>;
    fn get<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<HgtTile, TerrainCacheError>> + Send + 'a>>;
}

impl TileCache for TerrainCache {
    fn has_cached_tile<'a>(
        &'a self,
        tile: &'a TileId,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        let cache = self.clone();
        let tile = *tile;
        Box::pin(async move {
            let runtime = tokio::runtime::Handle::current();
            let job = TerrainBlockingJob::new();
            tokio::task::spawn_blocking(move || {
                let _job = job;
                #[cfg(test)]
                _job.enter_for_test();
                #[cfg(test)]
                terrain_hold_local_for_test(TerrainLocalStage::CachedState, tile);
                runtime.block_on(cache.has_cached_tile(&tile))
            })
            .await
            .unwrap_or(false)
        })
    }
    fn lookup<'a>(
        &'a self,
        latitude: f64,
        longitude: f64,
        budget: Duration,
        guard: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<TerrainLookupResult, TerrainCacheError>> + Send + 'a>>
    {
        let cache = self.clone();
        Box::pin(terrain_cache_blocking(guard, move |relay| async move {
            cache
                .lookup_with_egress_guard(latitude, longitude, budget, &relay)
                .await
        }))
    }
    fn get<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<HgtTile, TerrainCacheError>> + Send + 'a>> {
        let cache = self.clone();
        Box::pin(terrain_cache_blocking(guard, move |relay| async move {
            #[cfg(test)]
            terrain_hold_local_for_test(TerrainLocalStage::Get, tile);
            cache.get_with_egress_guard(tile, &relay).await
        }))
    }
}

pub(crate) struct CacheTileSource<C: TileCache = TerrainCache> {
    cache: C,
}

impl<C: TileCache> TileSource for CacheTileSource<C> {
    fn has_pending_local_work(&self) -> bool {
        TERRAIN_BLOCKING_JOBS.load(Ordering::SeqCst) != 0
    }
    fn acquire<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
        inner: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>> {
        Box::pin(async move {
            // C9 R19, C9 R20, C9 R22: cached tiles retain admission without a probe.
            let acquisition_started = Instant::now();
            let cached = timeout(K_LOCAL_TILE_BUDGET, self.cache.has_cached_tile(&tile))
                .await
                .map_err(|_| AcquireFailure {
                    kind: AcquireErrorKind::LocalTimeout,
                    provenance: None,
                })?;
            let guard = if cached { inner } else { guard };
            // C9 R14: the centre selects the same tile at degree edges and poles.
            let result = timeout(
                K_ACQUIRE_BUDGET.saturating_sub(acquisition_started.elapsed()),
                self.cache.lookup(
                    f64::from(tile.latitude()) + 0.5,
                    f64::from(tile.longitude()) + 0.5,
                    K_ACQUIRE_BUDGET,
                    guard,
                ),
            )
            .await
            .map_err(|_| AcquireFailure {
                kind: AcquireErrorKind::Cache(TerrainCacheError::Timeout),
                provenance: None,
            })?
            .map_err(|error| AcquireFailure {
                kind: AcquireErrorKind::Cache(error),
                provenance: None,
            })?;
            let provenance = TileProvenance {
                tile_name: result.tile_name,
                tile_hash: result.tile_hash,
                dataset_source: result.dataset_source,
                dataset_version: result.dataset_version,
            };
            if TileId::parse(&provenance.tile_name).ok() != Some(tile) {
                return Err(AcquireFailure {
                    kind: AcquireErrorKind::TileMismatch,
                    provenance: None,
                });
            }
            // C9 R14: synchronous cache reads and hashing stay inside the local budget.
            let loaded = timeout(K_LOCAL_TILE_BUDGET, async {
                let loaded = self
                    .cache
                    .get(tile, guard)
                    .await
                    .map_err(|error| AcquireFailure {
                        kind: AcquireErrorKind::Cache(error),
                        provenance: None,
                    })?;
                let job = TerrainBlockingJob::new();
                tokio::task::spawn_blocking(move || {
                    #[cfg(test)]
                    job.enter_for_test();
                    #[cfg(test)]
                    {
                        terrain_hold_hash_for_test(loaded.tile_id());
                        terrain_hold_local_for_test(TerrainLocalStage::Hash, loaded.tile_id());
                    }
                    let hash = loaded.sha256_hex();
                    (loaded, hash, job)
                })
                .await
                .map_err(|_| AcquireFailure {
                    kind: AcquireErrorKind::Cache(TerrainCacheError::Io(std::io::Error::other(
                        "tile task failed",
                    ))),
                    provenance: None,
                })
            })
            .await
            .map_err(|_| AcquireFailure {
                kind: AcquireErrorKind::LocalTimeout,
                provenance: None,
            })??;
            let (loaded, hash, _job) = loaded;
            let hash = hash.map_err(|error| AcquireFailure {
                kind: AcquireErrorKind::Cache(TerrainCacheError::InvalidTile(error)),
                provenance: None,
            })?;
            if hash != provenance.tile_hash {
                return Err(AcquireFailure {
                    kind: AcquireErrorKind::HashMismatch,
                    provenance: Some(provenance),
                });
            }
            Ok(AcquiredTile {
                tile: loaded,
                provenance,
            })
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureClass {
    Provider,
    Absent,
    Integrity,
    Local,
    Input,
    Worker,
}

fn failure_class(code: &str) -> FailureClass {
    match code {
        "timeout" | "source_unavailable" | "invalid_response" => FailureClass::Provider,
        "tile_unavailable" => FailureClass::Absent,
        "insufficient_free_space" | "cache_quota_exceeded" | "io" => FailureClass::Local,
        "nonfinite_coordinate" | "invalid_latitude" | "invalid_longitude" => FailureClass::Input,
        "store" | "invalid_config" | "untrusted_root" => FailureClass::Worker,
        _ => FailureClass::Integrity,
    }
}

fn schedule_for(class: FailureClass) -> Option<RetrySchedule> {
    let (base_ms, cap_ms) = match class {
        FailureClass::Provider => (900_000, 604_800_000),
        FailureClass::Absent => (604_800_000, 2_592_000_000),
        FailureClass::Integrity | FailureClass::Input => (21_600_000, 2_592_000_000),
        FailureClass::Local => (3_600_000, 86_400_000),
        FailureClass::Worker => return None,
    };
    Some(RetrySchedule { base_ms, cap_ms })
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the C9 R17 class schedule evaluator, checked by C9 ST22"
    )
)]
fn retry_delay_ms(class: FailureClass, attempts_after: i64) -> i64 {
    schedule_for(class).map_or(0, |schedule| schedule.delay_ms(attempts_after))
}

#[derive(Default)]
struct TileFailureMemo {
    entries: VecDeque<(TileId, i64, &'static str)>,
}

impl TileFailureMemo {
    fn lookup(&mut self, tile: TileId, now_ms: i64) -> Option<&'static str> {
        self.entries.retain(|(_, until, _)| now_ms < *until);
        self.entries
            .iter()
            .find(|(id, _, _)| *id == tile)
            .map(|(_, _, code)| *code)
    }
    fn remember(&mut self, tile: TileId, now_ms: i64, code: &'static str) {
        self.entries.retain(|(id, _, _)| *id != tile);
        self.entries
            .push_back((tile, now_ms.saturating_add(K_TILE_FAIL_MEMO), code));
        while self.entries.len() > K_MEMO_ENTRIES {
            self.entries.pop_front();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BreakerState {
    Closed,
    Open { until_ms: i64 },
    HalfOpen { permit_held: bool },
}

struct AcquisitionBreaker {
    state: BreakerState,
    backoff_ms: i64,
    provider_failures: Vec<(TileId, i64)>,
}

impl Default for AcquisitionBreaker {
    fn default() -> Self {
        Self {
            state: BreakerState::Closed,
            backoff_ms: K_BREAKER_BASE,
            provider_failures: Vec::new(),
        }
    }
}

impl AcquisitionBreaker {
    fn settle(&mut self, tile: TileId, now_ms: i64, holder: bool, class: Option<FailureClass>) {
        if holder {
            match class {
                None | Some(FailureClass::Absent | FailureClass::Integrity) => {
                    self.state = BreakerState::Closed;
                    self.backoff_ms = K_BREAKER_BASE;
                    self.provider_failures.clear();
                }
                Some(FailureClass::Provider | FailureClass::Local) => {
                    self.backoff_ms = self.backoff_ms.saturating_mul(2).min(K_BREAKER_CAP);
                    self.state = BreakerState::Open {
                        until_ms: now_ms.saturating_add(self.backoff_ms),
                    };
                }
                Some(FailureClass::Input | FailureClass::Worker) => {
                    self.state = BreakerState::HalfOpen { permit_held: false }
                }
            }
        } else if self.state == BreakerState::Closed {
            match class {
                Some(FailureClass::Provider) => {
                    self.provider_failures.retain(|(id, at)| {
                        *id != tile
                            && *at <= now_ms
                            && now_ms.saturating_sub(*at) <= K_BREAKER_WINDOW
                    });
                    let distinct_failure = !self.provider_failures.is_empty();
                    self.provider_failures.push((tile, now_ms));
                    if distinct_failure {
                        self.state = BreakerState::Open {
                            until_ms: now_ms.saturating_add(self.backoff_ms),
                        };
                    }
                }
                Some(FailureClass::Local) => {
                    self.state = BreakerState::Open {
                        until_ms: now_ms.saturating_add(self.backoff_ms),
                    }
                }
                _ => {}
            }
        }
    }
}

/// One acquisition's egress permission and half-open ownership (C9 R20).
struct BreakerGuard<'a> {
    breaker: &'a Mutex<AcquisitionBreaker>,
    inner: &'a dyn EgressGuard,
    clock: &'a dyn TerrainClock,
    holds_permit: AtomicBool,
    denied_by_breaker: AtomicBool,
    denied_by_inner: AtomicBool,
    consulted: AtomicBool,
    clock_failed: AtomicBool,
}

impl<'a> BreakerGuard<'a> {
    fn new(
        breaker: &'a Mutex<AcquisitionBreaker>,
        inner: &'a dyn EgressGuard,
        clock: &'a dyn TerrainClock,
    ) -> Self {
        Self {
            breaker,
            inner,
            clock,
            holds_permit: AtomicBool::new(false),
            denied_by_breaker: AtomicBool::new(false),
            denied_by_inner: AtomicBool::new(false),
            consulted: AtomicBool::new(false),
            clock_failed: AtomicBool::new(false),
        }
    }
    fn settle(&self, tile: TileId, now_ms: i64, class: Option<FailureClass>) {
        if self.consulted.load(Ordering::SeqCst) {
            let mut breaker = self
                .breaker
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let holder = self.holds_permit.swap(false, Ordering::SeqCst);
            breaker.settle(tile, now_ms, holder, class);
        }
    }
}

impl EgressGuard for BreakerGuard<'_> {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        let now_ms = self.clock.now_ms().map_err(|_| {
            self.clock_failed.store(true, Ordering::SeqCst);
            EgressGuardError
        })?;
        {
            let mut breaker = self
                .breaker
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let BreakerState::Open { until_ms } = breaker.state {
                if now_ms < until_ms {
                    self.denied_by_breaker.store(true, Ordering::SeqCst);
                    return Err(EgressGuardError);
                }
                breaker.state = BreakerState::HalfOpen { permit_held: false };
            }
            if let BreakerState::HalfOpen { permit_held } = breaker.state {
                if permit_held && !self.holds_permit.load(Ordering::SeqCst) {
                    self.denied_by_breaker.store(true, Ordering::SeqCst);
                    return Err(EgressGuardError);
                }
                breaker.state = BreakerState::HalfOpen { permit_held: true };
                self.holds_permit.store(true, Ordering::SeqCst);
            }
        }
        self.consulted.store(true, Ordering::SeqCst);
        self.inner.assert_egress_allowed().inspect_err(|_| {
            self.denied_by_inner.store(true, Ordering::SeqCst);
        })
    }
}

impl Drop for BreakerGuard<'_> {
    fn drop(&mut self) {
        if self.holds_permit.swap(false, Ordering::SeqCst) {
            let mut breaker = self
                .breaker
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if matches!(breaker.state, BreakerState::HalfOpen { .. }) {
                breaker.state = BreakerState::HalfOpen { permit_held: false };
            }
        }
    }
}

enum GroupAcquisition {
    Tile(AcquiredTile),
    Failed {
        code: &'static str,
        class: FailureClass,
        provenance: Option<TileProvenance>,
    },
    Deferred,
    AccessDenied,
    Worker {
        code: &'static str,
    },
}

async fn acquire_group(
    state: &mut TerrainWorkerState,
    source: &dyn TileSource,
    inner: &dyn EgressGuard,
    clock: &dyn TerrainClock,
    tile: TileId,
) -> GroupAcquisition {
    let Ok(now_ms) = clock.now_ms() else {
        return GroupAcquisition::Worker { code: "clock" };
    };
    if let Some(code) = state.memo.lookup(tile, now_ms) {
        return GroupAcquisition::Failed {
            code,
            class: failure_class(code),
            provenance: None,
        };
    }
    let guard = BreakerGuard::new(&state.breaker, inner, clock);
    let result = source.acquire(tile, &guard, inner).await;
    let Ok(ended_ms) = clock.now_ms() else {
        return GroupAcquisition::Worker { code: "clock" };
    };
    if guard.clock_failed.load(Ordering::SeqCst) {
        return GroupAcquisition::Worker { code: "clock" };
    }
    match result {
        Ok(tile) => {
            guard.settle(tile.tile.tile_id(), ended_ms, None);
            GroupAcquisition::Tile(tile)
        }
        Err(failure) => {
            let code = failure.kind.code();
            if code == "egress_denied" {
                return if guard.denied_by_breaker.load(Ordering::SeqCst) {
                    GroupAcquisition::Deferred
                } else {
                    GroupAcquisition::AccessDenied
                };
            }
            let class = failure_class(code);
            guard.settle(tile, ended_ms, Some(class));
            if class == FailureClass::Worker {
                return GroupAcquisition::Worker { code };
            }
            state.memo.remember(tile, ended_ms, code);
            GroupAcquisition::Failed {
                code,
                class,
                provenance: failure.provenance,
            }
        }
    }
}

/// Forward blocking cache guard calls to the live admission owner (C9 R20).
struct TerrainGuardRelay(
    mpsc::UnboundedSender<std::sync::mpsc::SyncSender<Result<(), EgressGuardError>>>,
);
impl EgressGuard for TerrainGuardRelay {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        self.0.send(send).map_err(|_| EgressGuardError)?;
        receive.recv().unwrap_or(Err(EgressGuardError))
    }
}

/// Run cache I/O off the async executor; a dropped owner denies later permits.
async fn terrain_cache_blocking<T, F, Fut>(
    guard: &dyn EgressGuard,
    operation: F,
) -> Result<T, TerrainCacheError>
where
    T: Send + 'static,
    F: FnOnce(TerrainGuardRelay) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, TerrainCacheError>> + Send + 'static,
{
    let (send, mut receive) = mpsc::unbounded_channel();
    let runtime = tokio::runtime::Handle::current();
    let job = TerrainBlockingJob::new();
    let mut task = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        job.enter_for_test();
        (runtime.block_on(operation(TerrainGuardRelay(send))), job)
    });
    loop {
        tokio::select! {
            result = &mut task => return result.map_err(|_| TerrainCacheError::Io(std::io::Error::other("cache task failed")))?.0,
            request = receive.recv() => {
                if let Some(reply) = request {
                    let _ = reply.send(guard.assert_egress_allowed());
                } else {
                    return task.await.map_err(|_| TerrainCacheError::Io(std::io::Error::other("cache task failed")))?.0;
                }
            }
        }
    }
}

static TERRAIN_BLOCKING_JOBS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Process-wide ownership across all cache instances, including after timeout.
struct TerrainBlockingJob {
    #[cfg(test)]
    dropped: Arc<AtomicBool>,
}
impl TerrainBlockingJob {
    fn new() -> Self {
        TERRAIN_BLOCKING_JOBS.fetch_add(1, Ordering::SeqCst);
        Self {
            #[cfg(test)]
            dropped: Arc::new(AtomicBool::new(false)),
        }
    }
    #[cfg(test)]
    fn enter_for_test(&self) {
        TERRAIN_CURRENT_JOB.with(|slot| *slot.borrow_mut() = Some(self.dropped.clone()));
    }
}
#[cfg(test)]
thread_local! {
    static TERRAIN_CURRENT_JOB: std::cell::RefCell<Option<Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
fn terrain_job_drop_for_test() -> Arc<AtomicBool> {
    TERRAIN_CURRENT_JOB.with(|slot| {
        slot.borrow()
            .as_ref()
            .expect("blocking stage owns a job")
            .clone()
    })
}
impl Drop for TerrainBlockingJob {
    fn drop(&mut self) {
        TERRAIN_BLOCKING_JOBS.fetch_sub(1, Ordering::SeqCst);
        #[cfg(test)]
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
type TerrainHashHold = (
    TileId,
    oneshot::Sender<Arc<AtomicBool>>,
    std::sync::mpsc::Receiver<()>,
);
#[cfg(test)]
static TERRAIN_HASH_HOLD: Mutex<Option<TerrainHashHold>> = Mutex::new(None);
#[cfg(test)]
fn terrain_hold_hash_for_test(tile: TileId) {
    // C9 R13, C9 R28: acknowledge a loaded tile held across its hash deadline.
    let hold = {
        let mut slot = TERRAIN_HASH_HOLD.lock().unwrap();
        if slot.as_ref().is_some_and(|(id, _, _)| *id == tile) {
            slot.take()
        } else {
            None
        }
    };
    if let Some((_, entered, release)) = hold {
        let _ = entered.send(terrain_job_drop_for_test());
        let _ = release.recv();
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TerrainLocalStage {
    CachedState,
    Get,
    Hash,
}
#[cfg(test)]
type TerrainLocalHold = (
    TerrainLocalStage,
    TileId,
    std::thread::ThreadId,
    oneshot::Sender<(std::thread::ThreadId, Arc<AtomicBool>)>,
    std::sync::mpsc::Receiver<()>,
);
#[cfg(test)]
static TERRAIN_LOCAL_HOLD: Mutex<Vec<TerrainLocalHold>> = Mutex::new(Vec::new());
#[cfg(test)]
fn terrain_hold_local_for_test(stage: TerrainLocalStage, tile: TileId) {
    // C9 R14, C9 R28: acknowledge the actual cache adapter's blocking stage.
    let hold = {
        let mut slot = TERRAIN_LOCAL_HOLD.lock().unwrap();
        slot.iter()
            .position(|(held_stage, id, ..)| *held_stage == stage && *id == tile)
            .map(|index| slot.remove(index))
    };
    if let Some((_, _, caller, entered, release)) = hold {
        let worker = std::thread::current().id();
        let _ = entered.send((worker, terrain_job_drop_for_test()));
        // A broken adapter running here on the caller must fail the assertion
        // without blocking the executor that releases the acknowledged worker.
        if worker != caller {
            let _ = release.recv();
        }
    }
}
