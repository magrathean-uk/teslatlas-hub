// SPDX-License-Identifier: AGPL-3.0-only

use crate::{
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
const K_RETRY_SHIFT_MAX: i64 = 32;

/// The identity and origin of the bytes sampled by a group (C9 R53).
/// Shared with storage outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TileProvenance {
    pub tile_name: String,
    pub tile_hash: String,
    pub dataset_source: String,
    pub dataset_version: String,
}

/// Per-class retry timing shared with storage outcomes (C9 R17).
#[derive(Clone, Copy)]
pub(crate) struct RetrySchedule {
    pub base_ms: i64,
    pub cap_ms: i64,
}

impl RetrySchedule {
    /// Saturating exponential delay, including corrupt attempt counts (C9 R17).
    pub fn delay_ms(self, attempts_after: i64) -> i64 {
        let shift = (attempts_after.max(1) - 1).min(K_RETRY_SHIFT_MAX) as u32;
        self.base_ms.saturating_mul(1_i64 << shift).min(self.cap_ms)
    }
}

#[cfg_attr(not(test), expect(dead_code, reason = "C9 Part A: used by Part P"))]
pub(crate) trait TileSource: Send + Sync {
    fn acquire<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
        inner: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>;
}

#[cfg_attr(not(test), expect(dead_code, reason = "C9 Part A: used by Part P"))]
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

#[cfg_attr(not(test), expect(dead_code, reason = "C9 Part A: used by Part P"))]
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
        Box::pin(TerrainCache::has_cached_tile(self, tile))
    }
    fn lookup<'a>(
        &'a self,
        latitude: f64,
        longitude: f64,
        budget: Duration,
        guard: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<TerrainLookupResult, TerrainCacheError>> + Send + 'a>>
    {
        Box::pin(self.lookup_with_egress_guard(latitude, longitude, budget, guard))
    }
    fn get<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<HgtTile, TerrainCacheError>> + Send + 'a>> {
        Box::pin(self.get_with_egress_guard(tile, guard))
    }
}

#[cfg_attr(not(test), expect(dead_code, reason = "C9 Part A: used by Part P"))]
pub(crate) struct CacheTileSource<C: TileCache = TerrainCache> {
    cache: C,
}

impl<C: TileCache> TileSource for CacheTileSource<C> {
    fn acquire<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
        inner: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>> {
        Box::pin(async move {
            // C9 R19, C9 R20, C9 R22: cached tiles retain admission without a probe.
            let guard = if self.cache.has_cached_tile(&tile).await {
                inner
            } else {
                guard
            };
            // C9 R14: the centre selects the same tile at degree edges and poles.
            let result = self
                .cache
                .lookup(
                    f64::from(tile.latitude()) + 0.5,
                    f64::from(tile.longitude()) + 0.5,
                    K_ACQUIRE_BUDGET,
                    guard,
                )
                .await
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
            let loaded = timeout(K_LOCAL_TILE_BUDGET, self.cache.get(tile, guard)).await;
            let loaded = match loaded {
                Ok(Ok(loaded)) => loaded,
                Ok(Err(error)) => {
                    return Err(AcquireFailure {
                        kind: AcquireErrorKind::Cache(error),
                        provenance: None,
                    });
                }
                Err(_) => {
                    return Err(AcquireFailure {
                        kind: AcquireErrorKind::LocalTimeout,
                        provenance: None,
                    });
                }
            };
            let hash = loaded.sha256_hex().map_err(|error| AcquireFailure {
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

#[cfg_attr(not(test), expect(dead_code, reason = "C9 Part A: used by Part P"))]
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

/// Epoch-millisecond clock for terrain acquisition and scheduling (C9 R28).
pub(crate) trait TerrainClock: Send + Sync {
    fn now_ms(&self) -> Result<i64, CollectorError>;
}

/// In-memory acquisition policy and pass progress (C9 R18, C9 R19).
#[derive(Default)]
struct TerrainWorkerState {
    breaker: Mutex<AcquisitionBreaker>,
    memo: TileFailureMemo,
    #[expect(dead_code, reason = "C9 Part A: used by Part P")]
    rotation_after: Option<Uuid>,
    #[expect(dead_code, reason = "C9 Part A: used by Part P")]
    tidy_complete: bool,
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

#[cfg_attr(not(test), expect(dead_code, reason = "C9 Part A: used by Part P"))]
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

#[cfg_attr(not(test), expect(dead_code, reason = "C9 Part A: used by Part P"))]
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
