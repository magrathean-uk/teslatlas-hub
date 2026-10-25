// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::db::{
    K_PASS_CANDIDATES, K_RETRY_SLICE, K_SCAN_ROWS, TerrainBatch, TerrainBoundaryRaise,
    TerrainCandidate, TerrainOutcome, TerrainOutcomeKind, TerrainTidyRun,
};

const K_TX_OUTCOMES: usize = 250;
const K_PASS_TILES: usize = 4;
const K_RETRY_TILES_MIN: usize = 1;

/// Epoch-millisecond clock for terrain acquisition and scheduling (C9 R28).
pub(crate) trait TerrainClock: Send + Sync {
    fn now_ms(&self) -> Result<i64, CollectorError>;
}

/// Admission checked before any location read and immediately before a write (C9 R4).
pub(crate) trait SensitiveAccessCheck: Send + Sync {
    fn check(&self) -> Result<(), CollectorError>;
}

/// Runtime admission and the supervised run's irreversible mutation fence.
/// Used as both the access check and the inner acquisition guard.
pub(super) struct RuntimeAdmissionCheck {
    run: RunWriteGuard,
    inner: Box<dyn EgressGuard>,
}

impl RuntimeAdmissionCheck {
    pub(super) fn new(run: RunWriteGuard) -> Self {
        let inner: Box<dyn EgressGuard> = if let Some(admission) = &run.admission {
            Box::new(AdmittedUserEgressGuard::new(admission.clone()))
        } else {
            #[cfg(any(not(target_os = "macos"), test))]
            {
                Box::new(crate::geocoder::UnguardedEgress)
            }
            #[cfg(all(target_os = "macos", not(test)))]
            {
                Box::new(DenyTerrainEgress)
            }
        };
        Self { run, inner }
    }
}

#[cfg(all(target_os = "macos", not(test)))]
struct DenyTerrainEgress;
#[cfg(all(target_os = "macos", not(test)))]
impl EgressGuard for DenyTerrainEgress {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        Err(EgressGuardError)
    }
}

impl SensitiveAccessCheck for RuntimeAdmissionCheck {
    fn check(&self) -> Result<(), CollectorError> {
        self.run.check("terrain-pass")
    }
}
impl EgressGuard for RuntimeAdmissionCheck {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        TerrainCacheGuard(&self.run).assert_egress_allowed()?;
        self.inner
            .assert_egress_allowed()
            .inspect_err(|_| self.run.fence.set())
    }
}

/// Per-worker acquisition policy, rotation and per-pass drive decisions (C9 R7).
#[derive(Default)]
pub(super) struct TerrainWorkerState {
    pub(super) breaker: Mutex<AcquisitionBreaker>,
    pub(super) memo: TileFailureMemo,
    rotation_after: Option<Uuid>,
    pub(super) tidy_complete: bool,
    eligibility: HashMap<(Uuid, i64), bool>,
    /// C9 R59: publishes the marker read and completion within a pass.
    pub(super) marker_observer: Option<Box<dyn Fn(bool) + Send + Sync>>,
}

impl TerrainWorkerState {
    fn observe_tidy(&mut self, complete: bool) {
        self.tidy_complete = complete;
        if let Some(observer) = &self.marker_observer {
            observer(complete);
        }
    }

    /// C9 R19, C9 R24: an expired Open state is eligible for its next probe.
    pub(super) fn breaker_open_until(&self, now_ms: i64) -> Option<i64> {
        match self.breaker.lock().unwrap_or_else(|e| e.into_inner()).state {
            BreakerState::Open { until_ms } if now_ms < until_ms => Some(until_ms),
            _ => None,
        }
    }
}

/// Durable counts and deferrals from one bounded pass (C9 R11, C9 R57).
#[derive(Debug, Default)]
pub(super) struct TerrainPassReport {
    pub(super) filled: u32,
    pub(super) void: u32,
    pub(super) out_of_range: u32,
    pub(super) failed: u32,
    pub(super) stale: u32,
    pub(super) deferred: u32,
    pub(super) retired: u32,
    pub(super) tiles: u32,
    pub(super) full_batch: bool,
    pub(super) breaker_deferred: bool,
    /// C9 R13: a detached local task still owns a tile; re-check shortly.
    pub(super) local_deferred: bool,
    /// C9 R24: durable progress requires a backlog refresh.
    pub(super) wrote: bool,
    pub(super) access_denied: bool,
    pub(super) worker_condition: Option<&'static str>,
    pub(super) tidy_rows: u32,
}

// C9 R27: covers the marker read, selection, acquisition and all chunk commits.
static TERRAIN_PASS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct PassCandidate {
    candidate: TerrainCandidate,
    forward: bool,
    retry_after_ms: i64,
}
struct TileGroup {
    tile: TileId,
    candidates: Vec<PassCandidate>,
    first_forward: Option<i64>,
    earliest_retry: i64,
}

fn terrain_groups(candidates: Vec<PassCandidate>) -> (Vec<TileGroup>, Vec<TerrainOutcome>) {
    let mut rejected = Vec::new();
    let mut groups: Vec<TileGroup> = Vec::new();
    for candidate in candidates {
        // C9 R5, C9 R30: selectors already rejected invalid coordinates.
        let tile = match terrain_tile_for(
            candidate.candidate.position.latitude,
            candidate.candidate.position.longitude,
        ) {
            Ok(tile) => tile,
            Err(error) => {
                let code = terrain_error_code(&TerrainCacheError::InvalidTile(error));
                rejected.push(TerrainOutcome {
                    candidate: candidate.candidate,
                    forward: candidate.forward,
                    kind: TerrainOutcomeKind::Failed {
                        error_code: code,
                        schedule: schedule_for(FailureClass::Input)
                            .expect("Input failures always have a retry schedule"),
                        tile: None,
                    },
                });
                continue;
            }
        };
        let group = if let Some(index) = groups.iter().position(|group| group.tile == tile) {
            &mut groups[index]
        } else {
            groups.push(TileGroup {
                tile,
                candidates: Vec::new(),
                first_forward: None,
                earliest_retry: i64::MAX,
            });
            groups.last_mut().unwrap()
        };
        if candidate.forward {
            let id = candidate.candidate.position.id;
            group.first_forward = Some(group.first_forward.map_or(id, |old| old.min(id)));
        } else {
            group.earliest_retry = group.earliest_retry.min(candidate.retry_after_ms);
        }
        group.candidates.push(candidate);
    }
    groups.sort_by(|a, b| match (a.first_forward, b.first_forward) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => (a.earliest_retry, a.tile.name()).cmp(&(b.earliest_retry, b.tile.name())),
    });
    for group in &mut groups {
        group
            .candidates
            .sort_by_key(|c| (c.candidate.vehicle_id, c.candidate.position.id));
    }
    (groups, rejected)
}

struct TerrainCommit<'a> {
    store: &'a HubStore,
    access: &'a dyn SensitiveAccessCheck,
    clock: &'a dyn TerrainClock,
    run: TerrainTidyRun,
    forward: Option<(Uuid, i64)>,
    undecided: BTreeSet<i64>,
    cursor_written: Option<i64>,
    rearmed: bool,
}
impl TerrainCommit<'_> {
    fn apply(
        &mut self,
        vehicle_id: Uuid,
        outcomes: &[TerrainOutcome],
        retire: &[i64],
        report: &mut TerrainPassReport,
    ) -> Result<bool, CollectorError> {
        for outcome in outcomes.iter().filter(|o| o.forward) {
            self.undecided.remove(&outcome.candidate.position.id);
        }
        let cursor_to = self
            .forward
            .filter(|(id, _)| *id == vehicle_id)
            .map(|(_, examined)| {
                self.undecided
                    .first()
                    .map_or(examined, |first| examined.min(first.saturating_sub(1)))
            });
        let committed_at_ms = match self.clock.now_ms() {
            Ok(now) => now,
            Err(_) => {
                report.worker_condition = Some("clock");
                return Ok(false);
            }
        };
        if self.access.check().is_err() {
            report.access_denied = true;
            return Ok(false);
        }
        let applied = self.store.apply_terrain_batch_for_tidy_run(
            &TerrainBatch {
                vehicle_id,
                outcomes,
                retire,
                cursor_to,
                committed_at_ms,
            },
            self.run,
        )?;
        if applied.rearmed {
            self.rearmed = true;
            report.full_batch = true;
            return Ok(false);
        }
        report.wrote |= applied.cursor_written.is_some()
            || applied.filled + applied.void + applied.failed + applied.retired > 0;
        if applied.cursor_written.is_some() {
            self.cursor_written = applied.cursor_written;
        }
        self.undecided
            .extend(applied.stale_held.iter().copied().filter(|id| {
                outcomes
                    .iter()
                    .any(|outcome| outcome.forward && outcome.candidate.position.id == *id)
            }));
        report.filled += applied.filled;
        report.void += applied.void;
        report.out_of_range += applied.out_of_range;
        report.failed += applied.failed;
        report.stale += applied.stale_dropped + applied.stale_held.len() as u32;
        report.retired += applied.retired;
        Ok(true)
    }
}

/// C9 R4, C9 R11 to C9 R15: select, acquire and commit one bounded terrain pass.
pub(super) async fn run_terrain_enrichment_pass(
    store: &HubStore,
    source: &dyn TileSource,
    state: &mut TerrainWorkerState,
    access: &dyn SensitiveAccessCheck,
    inner_guard: &dyn EgressGuard,
    clock: &dyn TerrainClock,
) -> Result<TerrainPassReport, CollectorError> {
    run_terrain_pass(store, state, access, clock, Some((source, inner_guard))).await
}

/// C9 R57, C9 R59: observe the marker and run pending tidy chunks only.
pub(super) async fn run_terrain_tidy_only_pass(
    store: &HubStore,
    state: &mut TerrainWorkerState,
    access: &dyn SensitiveAccessCheck,
    clock: &dyn TerrainClock,
) -> Result<TerrainPassReport, CollectorError> {
    run_terrain_pass(store, state, access, clock, None).await
}

async fn run_terrain_pass(
    store: &HubStore,
    state: &mut TerrainWorkerState,
    access: &dyn SensitiveAccessCheck,
    clock: &dyn TerrainClock,
    enrichment: Option<(&dyn TileSource, &dyn EgressGuard)>,
) -> Result<TerrainPassReport, CollectorError> {
    let started = Instant::now();
    let mut report = TerrainPassReport::default();
    let result = terrain_pass_body(store, state, access, clock, enrichment, &mut report).await;
    if result.is_err() {
        report.worker_condition = Some("store");
    }
    if report.access_denied || report.worker_condition.is_some() {
        report.full_batch = false;
    }
    terrain_pass_log(&report, started);
    result.map(|()| report)
}

async fn terrain_pass_body(
    store: &HubStore,
    state: &mut TerrainWorkerState,
    access: &dyn SensitiveAccessCheck,
    clock: &dyn TerrainClock,
    enrichment: Option<(&dyn TileSource, &dyn EgressGuard)>,
    report: &mut TerrainPassReport,
) -> Result<(), CollectorError> {
    if access.check().is_err() {
        report.access_denied = true;
        return Ok(());
    }
    let _pass = TERRAIN_PASS_LOCK.lock().await;
    if access.check().is_err() {
        report.access_denied = true;
        return Ok(());
    }
    let pass_start_ms = match clock.now_ms() {
        Ok(now) => now,
        Err(_) => {
            report.worker_condition = Some("clock");
            return Ok(());
        }
    };
    state.eligibility.clear();
    let marker = store.terrain_tidy_progress()?;
    let run = marker.identity();
    state.observe_tidy(marker.completed_at_ms.is_some());
    if !state.tidy_complete {
        for _ in 0..K_PASS_CANDIDATES as usize / K_TX_OUTCOMES {
            let now = match clock.now_ms() {
                Ok(now) => now,
                Err(_) => {
                    report.worker_condition = Some("clock");
                    break;
                }
            };
            if access.check().is_err() {
                report.access_denied = true;
                break;
            }
            let applied = store.apply_terrain_tidy_chunk_for_run(K_TX_OUTCOMES as u32, now, run)?;
            report.full_batch = true;
            report.wrote = true;
            if applied.rearmed {
                state.observe_tidy(false);
                break;
            }
            report.tidy_rows += applied.examined;
            if applied.complete {
                state.observe_tidy(true);
                break;
            }
        }
        return Ok(());
    }

    let Some((source, inner_guard)) = enrichment else {
        return Ok(());
    };
    let vehicles = store.terrain_vehicles()?;
    // C9 R6, C9 R59: raise before either selector can classify that vehicle.
    for vehicle in &vehicles {
        if vehicle.watermark_boundary.max_position_id > vehicle.stored_boundary.max_position_id
            || vehicle.watermark_boundary.max_date_ms > vehicle.stored_boundary.max_date_ms
        {
            let now = match clock.now_ms() {
                Ok(now) => now,
                Err(_) => {
                    report.worker_condition = Some("clock");
                    return Ok(());
                }
            };
            if access.check().is_err() {
                report.access_denied = true;
                return Ok(());
            }
            let raised = store.record_terrain_import_boundary(
                vehicle.vehicle_id,
                vehicle.watermark_boundary,
                now,
            )?;
            report.wrote |= raised != TerrainBoundaryRaise::Unchanged;
            if raised == TerrainBoundaryRaise::RaisedAndRearmed {
                state.observe_tidy(false);
                report.full_batch = true;
                return Ok(());
            }
        }
    }
    let open_at_start = state.breaker_open_until(pass_start_ms).is_some();
    let retries = if open_at_start {
        Vec::new()
    } else {
        store.terrain_due_retries_with_eligibility(
            pass_start_ms,
            K_RETRY_SLICE,
            &mut state.eligibility,
        )?
    };
    let limit = K_PASS_CANDIDATES - retries.len() as u32;
    let mut selected = Vec::new();
    let mut retirements: BTreeMap<Uuid, Vec<i64>> = BTreeMap::new();
    for retry in &retries {
        if let Some(candidate) = &retry.candidate {
            selected.push(PassCandidate {
                candidate: candidate.clone(),
                forward: false,
                retry_after_ms: retry.retry_after_ms,
            });
        } else {
            retirements
                .entry(retry.vehicle_id)
                .or_default()
                .push(retry.position_id);
        }
    }
    let mut forward = None;
    let mut undecided = BTreeSet::new();
    let start = state
        .rotation_after
        .and_then(|after| vehicles.iter().position(|v| v.vehicle_id > after))
        .unwrap_or(0);
    for vehicle in vehicles.iter().cycle().skip(start).take(vehicles.len()) {
        let (window, cut) = store.terrain_forward_window_with_eligibility(
            vehicle,
            K_SCAN_ROWS,
            limit,
            &mut state.eligibility,
        )?;
        if window.examined_through == vehicle.cursor_position_id {
            continue;
        }
        state.rotation_after = Some(vehicle.vehicle_id);
        report.full_batch |= cut;
        forward = Some((vehicle.vehicle_id, window.examined_through));
        for candidate in window.candidates {
            undecided.insert(candidate.position.id);
            selected.push(PassCandidate {
                candidate,
                forward: true,
                retry_after_ms: 0,
            });
        }
        break;
    }
    report.full_batch |= selected.len() + retirements.values().map(Vec::len).sum::<usize>()
        >= K_PASS_CANDIDATES as usize;
    let (groups, rejected) = terrain_groups(selected);
    let forward_groups = groups.iter().filter(|g| g.first_forward.is_some()).count();
    let retry_groups = groups.len() - forward_groups;
    let forward_slots = forward_groups.min(K_PASS_TILES - retry_groups.min(K_RETRY_TILES_MIN));
    let retry_slots = retry_groups.min(K_PASS_TILES - forward_slots);
    let mut commit = TerrainCommit {
        store,
        access,
        clock,
        run,
        forward,
        undecided,
        cursor_written: None,
        rearmed: false,
    };
    let mut rejected_by_vehicle: BTreeMap<Uuid, Vec<TerrainOutcome>> = BTreeMap::new();
    for outcome in rejected {
        rejected_by_vehicle
            .entry(outcome.candidate.vehicle_id)
            .or_default()
            .push(outcome);
    }
    for (vehicle, outcomes) in rejected_by_vehicle {
        for chunk in outcomes.chunks(K_TX_OUTCOMES) {
            if !commit.apply(vehicle, chunk, &[], report)? {
                if commit.rearmed {
                    state.observe_tidy(false);
                }
                return Ok(());
            }
        }
    }
    for (vehicle, retire) in retirements {
        for chunk in retire.chunks(K_TX_OUTCOMES) {
            if !commit.apply(vehicle, &[], chunk, report)? {
                if commit.rearmed {
                    state.observe_tidy(false);
                }
                return Ok(());
            }
        }
    }
    let mut forward_used = 0;
    let mut retry_used = 0;
    let mut stopped = false;
    for group in groups {
        let in_slot = if group.first_forward.is_some() {
            forward_used += 1;
            forward_used <= forward_slots
        } else {
            retry_used += 1;
            retry_used <= retry_slots
        };
        if stopped || !in_slot {
            report.deferred += group.candidates.len() as u32;
            report.full_batch |= !stopped && !in_slot;
            continue;
        }
        if source.has_pending_local_work() {
            report.deferred += group.candidates.len() as u32;
            report.local_deferred = true;
            continue;
        }
        report.tiles += 1;
        let acquisition = acquire_group(state, source, inner_guard, clock, group.tile).await;
        let mut outcomes: BTreeMap<Uuid, Vec<TerrainOutcome>> = BTreeMap::new();
        match acquisition {
            GroupAcquisition::Tile(acquired) => {
                for candidate in group.candidates {
                    let kind = match sample_elevation(
                        &acquired.tile,
                        candidate.candidate.position.latitude,
                        candidate.candidate.position.longitude,
                    ) {
                        Ok(SampleOutcome::Elevation(elevation_m)) => TerrainOutcomeKind::Filled {
                            elevation_m,
                            tile: acquired.provenance.clone(),
                        },
                        Ok(SampleOutcome::Void) => TerrainOutcomeKind::Void {
                            tile: acquired.provenance.clone(),
                            error_code: None,
                        },
                        Ok(SampleOutcome::OutOfRange) => TerrainOutcomeKind::Void {
                            tile: acquired.provenance.clone(),
                            error_code: Some("out_of_range"),
                        },
                        Err(error) => {
                            let code = terrain_error_code(&TerrainCacheError::InvalidTile(error));
                            TerrainOutcomeKind::Failed {
                                error_code: code,
                                schedule: schedule_for(failure_class(code)).expect(
                                    "sampling errors are Input, Integrity or Local, never Worker",
                                ),
                                tile: Some(acquired.provenance.clone()),
                            }
                        }
                    };
                    outcomes
                        .entry(candidate.candidate.vehicle_id)
                        .or_default()
                        .push(TerrainOutcome {
                            candidate: candidate.candidate,
                            forward: candidate.forward,
                            kind,
                        });
                }
                // C9 R13: the tile is dropped before a transaction or another acquisition.
            }
            GroupAcquisition::Failed {
                code,
                class,
                provenance,
            } => {
                for candidate in group.candidates {
                    outcomes
                        .entry(candidate.candidate.vehicle_id)
                        .or_default()
                        .push(TerrainOutcome {
                            candidate: candidate.candidate,
                            forward: candidate.forward,
                            kind: TerrainOutcomeKind::Failed {
                                error_code: code,
                                schedule: schedule_for(class)
                                    .expect("GroupAcquisition::Failed excludes Worker failures"),
                                tile: provenance.clone(),
                            },
                        });
                }
            }
            GroupAcquisition::Deferred => {
                report.deferred += group.candidates.len() as u32;
                report.breaker_deferred = true;
            }
            GroupAcquisition::AccessDenied => {
                report.deferred += group.candidates.len() as u32;
                report.access_denied = true;
                stopped = true;
            }
            GroupAcquisition::Worker { code } => {
                report.deferred += group.candidates.len() as u32;
                report.worker_condition = Some(code);
                stopped = true;
            }
        }
        for (vehicle, rows) in outcomes {
            for chunk in rows.chunks(K_TX_OUTCOMES) {
                if !commit.apply(vehicle, chunk, &[], report)? {
                    if commit.rearmed {
                        state.observe_tidy(false);
                    }
                    return Ok(());
                }
            }
        }
    }
    if !report.access_denied
        && report.worker_condition.is_none()
        && let Some((vehicle, examined)) = forward
    {
        let lower = vehicles
            .iter()
            .find(|v| v.vehicle_id == vehicle)
            .map_or(0, |v| v.cursor_position_id);
        // C9 R9: skipped windows advance; deferred forward rows retain the cap.
        let cursor = commit
            .undecided
            .first()
            .map_or(examined, |f| examined.min(f.saturating_sub(1)));
        if cursor > commit.cursor_written.unwrap_or(lower)
            && !commit.apply(vehicle, &[], &[], report)?
            && commit.rearmed
        {
            state.observe_tidy(false);
        }
    }
    Ok(())
}

fn terrain_pass_log(report: &TerrainPassReport, started: Instant) {
    tracing::debug!(
        phase = "terrain",
        filled = report.filled,
        void = report.void,
        failed = report.failed,
        stale = report.stale,
        deferred = report.deferred,
        retired = report.retired,
        tiles = report.tiles,
        tidy_rows = report.tidy_rows,
        elapsed_ms = started.elapsed().as_millis() as u64,
        code = report.worker_condition.unwrap_or("none"),
        "terrain pass complete"
    );
}

#[cfg(test)]
mod b3_c9_p {
    use super::*;

    #[test]
    fn c9_underivable_tiles_have_input_outcomes() {
        for (latitude, longitude, code) in [
            (91.0, 20.0, "invalid_latitude"),
            (10.0, 181.0, "invalid_longitude"),
            (f64::NAN, 20.0, "nonfinite_coordinate"),
        ] {
            for forward in [false, true] {
                let mut position: crate::hub_pack::ProjectionPosition = serde_json::from_value(
                    serde_json::json!({"id":1,"drive_id":1,"car_id":1,"date_ms":1,"latitude":10.0,"longitude":20.0})
                ).unwrap();
                position.latitude = latitude;
                position.longitude = longitude;
                let candidate = TerrainCandidate {
                    vehicle_id: Uuid::nil(),
                    position,
                };
                let (groups, rejected) = terrain_groups(vec![PassCandidate {
                    candidate,
                    forward,
                    retry_after_ms: 0,
                }]);
                assert!(groups.is_empty());
                assert_eq!(rejected.len(), 1);
                let outcome = &rejected[0];
                assert_eq!(
                    (outcome.forward, outcome.candidate.position.id),
                    (forward, 1)
                );
                let TerrainOutcomeKind::Failed {
                    error_code,
                    schedule,
                    tile,
                } = &outcome.kind
                else {
                    panic!("C9 Input rejection must be a failure");
                };
                assert_eq!(*error_code, code);
                assert_eq!(failure_class(error_code), FailureClass::Input);
                assert_eq!(
                    (schedule.base_ms, schedule.cap_ms),
                    (21_600_000, 2_592_000_000)
                );
                assert_eq!(
                    (schedule.delay_ms(1), schedule.delay_ms(8)),
                    (21_600_000, 2_592_000_000)
                );
                assert!(tile.is_none());
            }
        }
    }
}
