// SPDX-License-Identifier: AGPL-3.0-only

use super::terrain_pass::{RuntimeAdmissionCheck, SensitiveAccessCheck};
use super::*;

const K_MIN_PASS_GAP: Duration = Duration::from_secs(10);
const K_CONTINUE_PAUSE: Duration = Duration::from_secs(2);
const K_IDLE_BACKSTOP_MS: i64 = 3_600_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerrainWorkerPhase {
    Disabled,
    Idle,
    Working,
    WaitingForAccess,
    BreakerOpen { until_ms: i64 },
    Backoff { code: &'static str, until_ms: i64 },
    Halted { code: &'static str },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerrainStatus {
    pub state: TerrainWorkerPhase,
    pub last_pass_at_ms: Option<i64>,
    /// C9 R24: unknown if the clock failed after a pass that filled.
    pub last_fill_at_ms: Option<i64>,
    pub last_error_code: Option<&'static str>,
    /// C9 R24: a store error can hide fills committed earlier in that pass.
    pub filled_since_start: u64,
    pub rows_unexamined: u64,
    pub retries_due: u64,
    pub retries_waiting: u64,
    pub tidy_complete: bool,
}
impl TerrainStatus {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            state: if enabled {
                TerrainWorkerPhase::Idle
            } else {
                TerrainWorkerPhase::Disabled
            },
            last_pass_at_ms: None,
            last_fill_at_ms: None,
            last_error_code: None,
            filled_since_start: 0,
            rows_unexamined: 0,
            retries_due: 0,
            retries_waiting: 0,
            tidy_complete: false,
        }
    }
}

pub(super) struct TerrainWorkerDeps {
    pub source: Option<Arc<dyn TileSource>>,
    pub clock: Arc<dyn TerrainClock>,
    pub access: Arc<dyn SensitiveAccessCheck>,
    pub inner_guard: Arc<dyn EgressGuard>,
    #[cfg(test)]
    pub events: Option<mpsc::UnboundedSender<TerrainStatus>>,
}

struct EpochClock;
impl TerrainClock for EpochClock {
    fn now_ms(&self) -> Result<i64, CollectorError> {
        current_epoch_millis()
    }
}

/// C9 R1: use the supervised run's store and one shared admission check.
pub(super) fn spawn_terrain_worker(
    data_dir: PathBuf,
    config: TerrainConfig,
    _cursor_key: CursorKey,
    run: RunWriteGuard,
    store: HubStore,
) -> TerrainWorker {
    let admission = Arc::new(RuntimeAdmissionCheck::new(run.clone()));
    spawn_terrain_worker_with(
        data_dir,
        config,
        _cursor_key,
        run,
        store,
        TerrainWorkerDeps {
            source: None,
            clock: Arc::new(EpochClock),
            access: admission.clone(),
            inner_guard: admission,
            #[cfg(test)]
            events: None,
        },
    )
}

#[derive(Clone)]
struct Publisher {
    sender: watch::Sender<TerrainStatus>,
    #[cfg(test)]
    events: Option<mpsc::UnboundedSender<TerrainStatus>>,
}
impl Publisher {
    fn update(&self, change: impl FnOnce(&mut TerrainStatus)) {
        self.sender.send_modify(change);
        #[cfg(test)]
        if let Some(events) = &self.events {
            let _ = events.send(self.sender.borrow().clone());
        }
    }
    fn phase(&self, phase: TerrainWorkerPhase) {
        self.update(|status| status.state = phase);
    }
}

fn source_for(
    data_dir: &Path,
    config: &TerrainConfig,
    store: &HubStore,
    run: &RunWriteGuard,
) -> Result<Arc<dyn TileSource>, &'static str> {
    let options =
        crate::terrain_cache::TerrainCacheOptions::from_config(config, data_dir, store.clone())
            .map_err(|error| terrain_error_code(&error))?;
    let cache = TerrainCache::new_with_guard(options, &TerrainCacheGuard(run))
        .map_err(|error| terrain_error_code(&error))?;
    debug_assert!(cache.root_path().is_absolute());
    Ok(Arc::new(CacheTileSource { cache }))
}

/// C9 R1, C9 R23: initialisation and joined shutdown acknowledge worker ownership.
pub(super) fn spawn_terrain_worker_with(
    data_dir: PathBuf,
    config: TerrainConfig,
    _cursor_key: CursorKey,
    run: RunWriteGuard,
    store: HubStore,
    deps: TerrainWorkerDeps,
) -> TerrainWorker {
    let (wake, mut wakes) = mpsc::channel(1);
    let (initialized_tx, initialized) = oneshot::channel();
    let (start, mut started) = oneshot::channel();
    let (stop, mut stopped) = oneshot::channel();
    let (sender, status) = watch::channel(TerrainStatus::new(config.enabled));
    let task = tokio::spawn(supervised_test_scoped(async move {
        let publisher = Publisher {
            sender,
            #[cfg(test)]
            events: deps.events.clone(),
        };
        // C9 R1: probe the supplied connection without running migrations.
        if let Err(error) = store.open() {
            let _ = initialized_tx.send(Err(CollectorError::TerrainWorkerStartup));
            return Err(CollectorError::Store(error));
        }
        let mut source = if config.enabled {
            deps.source.clone()
        } else {
            None
        };
        let mut condition = None;
        if config.enabled && source.is_none() {
            match source_for(&data_dir, &config, &store, &run) {
                Ok(built) => source = Some(built),
                Err(code) => condition = Some(code),
            }
        }
        let mut backoff_ms = K_BREAKER_BASE;
        let initial_now = deps.clock.now_ms().ok();
        let mut last_good = (initial_now.unwrap_or(0), Instant::now());
        let mut startup_deadline =
            condition.and_then(|_| initial_now.map(|now| now.saturating_add(backoff_ms)));
        if let Some(code) = condition
            && (code == "invalid_config" || startup_deadline.is_some())
        {
            publish_condition(
                &publisher,
                config.enabled,
                code,
                startup_deadline.unwrap_or(0),
            );
        }
        let _ = initialized_tx.send(Ok(()));
        tokio::select! {
            biased;
            _ = &mut stopped => return Ok(()),
            start = &mut started => if start.is_err() { return Ok(()); },
        }
        let observer = publisher.clone();
        let mut state = TerrainWorkerState::default();
        state.marker_observer = Some(Box::new(move |complete| {
            if observer.sender.borrow().tidy_complete != complete {
                observer.update(|status| status.tidy_complete = complete);
            }
        }));
        let mut counted = false;
        let mut local_pause = K_CONTINUE_PAUSE;
        loop {
            if let Some(code) = condition.take() {
                let now = epoch_or_elapsed(&deps, &mut last_good);
                // C9 R21: waiting for start cannot extend an advertised backoff.
                let until_ms = startup_deadline
                    .take()
                    .unwrap_or_else(|| now.saturating_add(backoff_ms));
                publish_condition(&publisher, config.enabled, code, until_ms);
                if code == "invalid_config" {
                    let _ = (&mut stopped).await;
                    return Ok(());
                }
                tokio::select! {
                    biased;
                    _ = &mut stopped => return Ok(()),
                    _ = sleep(Duration::from_millis(until_ms.saturating_sub(now).max(0) as u64)) => {},
                }
                backoff_ms = backoff_ms.saturating_mul(2).min(K_BREAKER_CAP);
            }
            if config.enabled && source.is_none() {
                match source_for(&data_dir, &config, &store, &run) {
                    Ok(built) => source = Some(built),
                    Err(code) => {
                        condition = Some(code);
                        continue;
                    }
                }
            }
            let _ = wakes.try_recv();
            let pass_started = Instant::now();
            let pass_epoch = match deps.clock.now_ms() {
                Ok(now) => {
                    last_good = (now, Instant::now());
                    now
                }
                Err(_) => {
                    condition = Some("clock");
                    continue;
                }
            };
            publisher.update(|status| {
                status.last_pass_at_ms = Some(pass_epoch);
                status.state = if config.enabled {
                    TerrainWorkerPhase::Working
                } else {
                    TerrainWorkerPhase::Disabled
                };
            });
            let pass = async {
                supervised_test_gate("terrain-worker-pass").await;
                if config.enabled {
                    terrain_pass::run_terrain_enrichment_pass(
                        &store,
                        source
                            .as_deref()
                            .expect("enabled terrain source is initialised"),
                        &mut state,
                        deps.access.as_ref(),
                        deps.inner_guard.as_ref(),
                        deps.clock.as_ref(),
                    )
                    .await
                } else {
                    terrain_pass::run_terrain_tidy_only_pass(
                        &store,
                        &mut state,
                        deps.access.as_ref(),
                        deps.clock.as_ref(),
                    )
                    .await
                }
            };
            let result = tokio::select! {
                biased;
                _ = &mut stopped => return Ok(()),
                result = pass => result,
            };
            let report = match result {
                Ok(report) => report,
                Err(_) => {
                    // C9 R24: earlier chunks can have committed before the error.
                    counted = false;
                    condition = Some("store");
                    continue;
                }
            };
            counted &= !report.wrote;
            let evaluated = deps.clock.now_ms();
            publisher.update(|status| {
                status.tidy_complete = state.tidy_complete;
                status.filled_since_start = status
                    .filled_since_start
                    .saturating_add(u64::from(report.filled));
                if report.filled > 0 {
                    status.last_fill_at_ms = evaluated.as_ref().ok().copied();
                }
                status.last_error_code = report.worker_condition;
            });
            let now = match evaluated {
                Ok(now) => {
                    last_good = (now, Instant::now());
                    now
                }
                Err(_) => {
                    condition = Some("clock");
                    continue;
                }
            };
            // C9 R24: cursor-only writes also change the bounded backlog.
            if !counted {
                match store.terrain_backlog(now, crate::db::K_STATUS_COUNT_CAP) {
                    Ok(backlog) => {
                        counted = true;
                        publisher.update(|status| {
                            status.rows_unexamined = backlog.rows_unexamined;
                            status.retries_due = backlog.retries_due;
                            status.retries_waiting = backlog.retries_waiting;
                        });
                    }
                    Err(_) => {
                        condition = Some("store");
                        continue;
                    }
                }
            }
            if !report.local_deferred {
                local_pause = K_CONTINUE_PAUSE;
            }
            if let Some(code) = report.worker_condition {
                condition = Some(code);
                continue;
            }
            let breaker = state.breaker_open_until(now);
            publisher.phase(if !config.enabled {
                TerrainWorkerPhase::Disabled
            } else if report.access_denied {
                TerrainWorkerPhase::WaitingForAccess
            } else if let Some(until_ms) = breaker {
                TerrainWorkerPhase::BreakerOpen { until_ms }
            } else if report.full_batch {
                TerrainWorkerPhase::Working
            } else {
                TerrainWorkerPhase::Idle
            });
            // C9 R3, C9 R59: completed disabled tidy returns to idle timing.
            let continuation = report.full_batch
                && (config.enabled || !state.tidy_complete)
                && (report.wrote
                    || !state.tidy_complete
                    || (breaker.is_none() && !report.local_deferred));
            if !report.access_denied && (report.local_deferred || continuation) {
                backoff_ms = K_BREAKER_BASE;
                let pause = if continuation {
                    K_CONTINUE_PAUSE
                } else {
                    let pause = local_pause;
                    local_pause = (local_pause * 2).min(K_MIN_PASS_GAP);
                    pause
                };
                tokio::select! {
                    biased;
                    _ = &mut stopped => return Ok(()),
                    _ = sleep(pause) => {},
                }
                continue;
            }
            let mut deadline_ms = pass_epoch.saturating_add(K_IDLE_BACKSTOP_MS);
            if config.enabled {
                match store.terrain_next_retry_at() {
                    Ok(Some(retry)) => {
                        deadline_ms =
                            deadline_ms.min(breaker.map_or(retry, |until| retry.max(until)));
                    }
                    Ok(None) => {}
                    Err(_) => {
                        condition = Some("store");
                        continue;
                    }
                }
                if let Some(until) = breaker {
                    deadline_ms = deadline_ms.min(until);
                }
                if report.breaker_deferred && breaker.is_none() {
                    deadline_ms = deadline_ms.min(now);
                }
            }
            let deadline = pass_started + K_MIN_PASS_GAP;
            let sleep_now = match deps.clock.now_ms() {
                Ok(now) => now,
                Err(_) => {
                    condition = Some("clock");
                    continue;
                }
            };
            // C9 R21: scheduling reads must succeed before resetting worker backoff.
            backoff_ms = K_BREAKER_BASE;
            let delay = Duration::from_millis(deadline_ms.saturating_sub(sleep_now).max(0) as u64);
            let scheduled = (Instant::now() + delay).max(deadline);
            tokio::select! {
                biased;
                _ = &mut stopped => return Ok(()),
                _ = tokio::time::sleep_until(scheduled) => {},
                wake = wakes.recv() => {
                    if wake.is_none() { let _ = (&mut stopped).await; return Ok(()); }
                    tokio::select! {
                        biased;
                        _ = &mut stopped => return Ok(()),
                        _ = tokio::time::sleep_until(deadline) => {},
                    }
                },
            }
        }
    }));
    TerrainWorker {
        status,
        logged_phase: None,
        wake,
        initialized: Some(initialized),
        start: Some(start),
        stop: Some(stop),
        task,
    }
}

fn epoch_or_elapsed(deps: &TerrainWorkerDeps, last: &mut (i64, Instant)) -> i64 {
    if let Ok(now) = deps.clock.now_ms() {
        *last = (now, Instant::now());
    }
    last.0
        .saturating_add(last.1.elapsed().as_millis().min(i64::MAX as u128) as i64)
}
fn publish_condition(publisher: &Publisher, enabled: bool, code: &'static str, until_ms: i64) {
    publisher.update(|status| {
        status.last_error_code = Some(code);
        status.state = if !enabled {
            TerrainWorkerPhase::Disabled
        } else if code == "invalid_config" {
            TerrainWorkerPhase::Halted { code }
        } else {
            TerrainWorkerPhase::Backoff { code, until_ms }
        };
    });
}

/// C9 R63: the supervised run records phases without location-bearing values.
pub(super) fn log_phase(status: &TerrainStatus) {
    if matches!(
        status.state,
        TerrainWorkerPhase::Backoff { .. }
            | TerrainWorkerPhase::Halted { .. }
            | TerrainWorkerPhase::WaitingForAccess
            | TerrainWorkerPhase::BreakerOpen { .. }
    ) {
        tracing::info!(phase = ?status.state, code = status.last_error_code.unwrap_or("none"),
            filled = status.filled_since_start, rows_unexamined = status.rows_unexamined,
            retries_due = status.retries_due, retries_waiting = status.retries_waiting,
            "terrain worker phase");
    } else {
        tracing::debug!(phase = ?status.state, code = status.last_error_code.unwrap_or("none"),
            filled = status.filled_since_start, rows_unexamined = status.rows_unexamined,
            retries_due = status.retries_due, retries_waiting = status.retries_waiting,
            "terrain worker phase");
    }
}
