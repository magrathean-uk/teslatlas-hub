// SPDX-License-Identifier: AGPL-3.0-only

fn provider_source(provider: CollectorProvider) -> SourceDescriptor {
    match provider {
        CollectorProvider::Legacy => {
            SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY)
        }
        CollectorProvider::Fleet => {
            SourceDescriptor::new(FLEET_API_SOURCE_KIND, FLEET_API_SOURCE_KEY)
        }
    }
}

/// What a complete inventory does to a known car it lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MissingVehicles {
    /// The car is retired at once: setup and the one-shot commands, where the list
    /// is the owner's own view of the account.
    Retire,
    /// Nothing is retired: the scheduled collector's lists are confirmed by the
    /// scheduler, which holds a car `Removed` only after spaced absences and keeps
    /// it in its set (C3 R53, R40). A retired car leaves the configured set at
    /// once, so retiring on the first list would drop the car before its second
    /// absence, stop its stream and take it out of the open-drive closure.
    Keep,
}

fn reconcile_complete_provider_inventory(
    store: &HubStore,
    provider: CollectorProvider,
    vehicles: &[Vehicle],
    missing: MissingVehicles,
) -> Result<(), CollectorError> {
    reconcile_complete_provider_inventory_guarded(store, provider, vehicles, missing, None)
}

/// The inventory of a supervised run: the source and the reconciliation are two
/// store mutations, each preceded by the run's write guard (B2 fix round, P3b fix 2,
/// R-P3bF2-1).
fn reconcile_complete_provider_inventory_guarded(
    store: &HubStore,
    provider: CollectorProvider,
    vehicles: &[Vehicle],
    missing: MissingVehicles,
    guard: Option<&RunWriteGuard>,
) -> Result<(), CollectorError> {
    let observed_at_ms = current_epoch_millis()?;
    check_optional_write(guard, "inventory-source")?;
    let source = store.register_source(&provider_source(provider), observed_at_ms)?;
    let inventory = vehicles
        .iter()
        .map(|vehicle| {
            let mut descriptor =
                VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string());
            descriptor.vin = Some(vehicle.vin.clone());
            descriptor
        })
        .collect::<Vec<_>>();
    check_optional_write(guard, "inventory-reconcile")?;
    match missing {
        MissingVehicles::Retire => {
            store.reconcile_provider_inventory(source.source_id, &inventory, observed_at_ms)?;
        }
        MissingVehicles::Keep => {
            store.reactivate_provider_inventory(source.source_id, &inventory, observed_at_ms)?;
        }
    }
    Ok(())
}

/// A complete list in setup or a one-shot command: a car it lacks is retired.
fn accept_complete_provider_inventory(
    store: &HubStore,
    provider: CollectorProvider,
    discovery: Result<Vec<Vehicle>, CollectorError>,
) -> Result<Vec<Vehicle>, CollectorError> {
    accept_provider_inventory(store, provider, discovery, MissingVehicles::Retire)
}

/// A complete list of the scheduled collector: the cars it names are reactivated,
/// and none is retired (C3 R53; see [`MissingVehicles::Keep`]).
#[cfg(test)]
fn accept_scheduled_provider_inventory(
    store: &HubStore,
    provider: CollectorProvider,
    discovery: Result<Vec<Vehicle>, CollectorError>,
) -> Result<Vec<Vehicle>, CollectorError> {
    accept_scheduled_provider_inventory_guarded(store, provider, discovery, None)
}

/// The scheduled inventory of a supervised run, behind its write guard.
fn accept_scheduled_provider_inventory_guarded(
    store: &HubStore,
    provider: CollectorProvider,
    discovery: Result<Vec<Vehicle>, CollectorError>,
    guard: Option<&RunWriteGuard>,
) -> Result<Vec<Vehicle>, CollectorError> {
    let vehicles = discovery?;
    reconcile_complete_provider_inventory_guarded(
        store,
        provider,
        &vehicles,
        MissingVehicles::Keep,
        guard,
    )?;
    Ok(vehicles)
}

fn accept_provider_inventory(
    store: &HubStore,
    provider: CollectorProvider,
    discovery: Result<Vec<Vehicle>, CollectorError>,
    missing: MissingVehicles,
) -> Result<Vec<Vehicle>, CollectorError> {
    let vehicles = discovery?;
    reconcile_complete_provider_inventory(store, provider, &vehicles, missing)?;
    Ok(vehicles)
}

fn require_supervised_vehicle_lineage(store: &HubStore) -> Result<(), CollectorError> {
    if store.has_configured_tesla_vehicle_lineage()? {
        Ok(())
    } else {
        Err(CollectorError::SelectedVehicleMissing)
    }
}

const fn provider_vehicle_data_record_type(provider: CollectorProvider) -> &'static str {
    match provider {
        CollectorProvider::Legacy => "owner_api_vehicle_data_v1",
        CollectorProvider::Fleet => "fleet_api_vehicle_data_v1",
    }
}

const fn provider_discovery_record_type(provider: CollectorProvider) -> &'static str {
    match provider {
        CollectorProvider::Legacy => "owner_api_discovery_v1",
        CollectorProvider::Fleet => "fleet_api_discovery_v1",
    }
}
const TERRAIN_PAGE_LIMIT: u32 = 1_000;
const TERRAIN_PERIOD: Duration = Duration::from_secs(6 * 60 * 60);
const TERRAIN_LOOKUP_BUDGET: Duration = Duration::from_millis(100);
const TERRAIN_RETRY_DELAY: Duration = TERRAIN_PERIOD;
const TERRAIN_FUSE_WINDOW: Duration = Duration::from_secs(3 * 60);
const TERRAIN_FUSE_RESET: Duration = Duration::from_secs(15 * 60);
/// How far a deadline is pushed when `Instant + Duration` would overflow (C3
/// R45): 100 years, far beyond any process lifetime and any deadline the spec
/// allows (the longest is `K_RETRY_AFTER_MAX`, one hour), so an overflowing
/// deadline means "never in this run" and never "retry now", which would be a
/// busy retry.
const RETRY_OVERFLOW_FALLBACK: Duration = Duration::from_secs(100 * 365 * 24 * 60 * 60);
/// How long a stopping stream task may take to end after its shutdown signal
/// before it is aborted (C3 section 8 item 13: `disconnect_vehicle_stream`
/// waits up to 5 s for `run`, then aborts). The spec fixes the value; it
/// bounds how long the loop can wait for one car's stream to end, so a stuck
/// task delays the loop by at most this long. Not measured.
const STREAM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Log-only threshold (not a scheduling input; provisional): a gap of 5 s
/// between a frame's source time and the previous frame's. At 50 km/h 5 s is
/// 69 m, more than one street block of positions (C3 R18), so a gap this long
/// is worth a warning.
const STREAM_SOURCE_GAP_WARN_MS: i64 = 5_000;
/// Log-only threshold (provisional): the intake queue lags by 5 s, half of
/// C4's 10 s wait for channel space (C4 R49), so the consumer is halfway to
/// stalling the stream.
const STREAM_QUEUE_LAG_WARN_MS: i64 = 5_000;
/// Log-only threshold (provisional): the queue lag is back to 1 s, four times
/// the plan's 250 ms intake target, so the warning clears with hysteresis and a
/// brief dip does not make it flap.
const STREAM_QUEUE_LAG_RECOVERED_MS: i64 = 1_000;
// Raw stream observations commit immediately. Derived client sync packs can
// coalesce briefly so a high-rate stream does not create one immutable pack
// per frame and repeatedly compact a nearly-full imported lineage.
const STREAM_EXPORT_REPLAY_INTERVAL: Duration = Duration::from_secs(60);
// Keep a continuously noisy stream from monopolising the collection loop.
// The channel absorbs bounded network/address work while the 100 ms active
// drain cadence keeps normal Tesla telemetry well below capacity.
/// The capacity of one stream run's event channel (C3 R28, C4 R56 g): at least
/// `K_STREAM_EVENT_CAPACITY`, which holds the 10 s that C4 waits for space at the
/// rate the intake target lets the collector keep up with. It replaces the retained
/// 256, which was above the least but let a stalled consumer buffer more than six
/// times as many events before the run ended loudly.
const STREAM_EVENT_CHANNEL_CAPACITY: usize = K_STREAM_EVENT_CAPACITY;
const MAX_STREAM_EVENTS_PER_DRAIN: usize = 16;

// The production collector deliberately uses conservative retry timing and
// cannot expose an interstitial gate state.  Keep the acceptance witness
// task-local and test-only: it neither changes a production call path nor
// leaks across concurrently running collector tests.
#[cfg(test)]
tokio::task_local! {
    static SUPERVISED_COLLECTOR_TEST_SEAM: Arc<SupervisedCollectorTestSeam>;
}

#[cfg(test)]
#[derive(Default)]
struct SupervisedCollectorTestSeam {
    collection_completed: tokio::sync::Mutex<Option<SupervisedCollectionCompletion>>,
    /// The tick-commit hook (C3 section 8 item 9): the tick transactions that
    /// wrote and committed.
    tick_commits: AtomicUsize,
    /// The tick transactions that reached their commit, whether or not a fault
    /// stopped them.
    tick_commit_attempts: AtomicUsize,
    /// While set, every tick transaction that would commit fails instead and
    /// rolls back.
    tick_commit_fault: AtomicBool,
    /// While set, the start function fails right after its transaction committed and
    /// before it reads anything else (B2 P3: the durable lift must survive it).
    start_commit_fault: AtomicBool,
    /// The time of every tick transaction that reached its commit (C3 CT69: the
    /// bound on how long a result waits for its transaction).
    tick_commit_times: std::sync::Mutex<Vec<Instant>>,
    /// The iterations the supervised loops have started (C3 R11: a loop that has
    /// nothing to do must not go round faster than its waiting rule allows).
    loop_iterations: AtomicUsize,
    /// The start time of every loop iteration (C3 R11, CT55b: no two empty
    /// iterations closer than the floor).
    loop_iteration_times: std::sync::Mutex<Vec<Instant>>,
    /// The settings refreshes the loops have made (CT55b: one every
    /// `CONTROL_SETTINGS_REFRESH` at most).
    settings_refreshes: AtomicUsize,
    /// The settings refreshes that ran through their gap-count and owner-action
    /// reads (B2 P3: entry alone does not prove a refresh finished).
    settings_refreshes_completed: AtomicUsize,
    /// While set, the waiting rule finds a due time in the past with nothing to do
    /// (CT55b: a scheduling bug must not make the loop busy, R11).
    forced_due_in_the_past: AtomicBool,
    /// The decision time of every `no_data` event a committed tick transaction
    /// stored (C3 R40, B2 clarification 3): its observed and received time. The
    /// intake prunes raw rows once the lifecycle has folded them, so the store
    /// cannot show it afterwards.
    no_data_stored: std::sync::Mutex<Vec<i64>>,
    /// The tick transactions that committed as a lone reservation commit (C3 R5):
    /// counted where they are made, so a test can bound a window's commits by the
    /// lists sent plus these.
    lone_reservation_commits: AtomicUsize,
    /// What starts a car's stream run when a test installs it (C3 R28, R56): the
    /// loop makes its normal admission, fuse and runtime checks, and the run's
    /// bounded event queue and stop channel then go to this factory instead of a
    /// WebSocket supervisor. The queue has the production capacity.
    stream_runs: std::sync::Mutex<Option<Arc<StreamRunFactory>>>,
    /// The stream runs the loop has admitted, injected or not.
    stream_admissions: AtomicUsize,
    /// The account lists the loop has really sent (a refused or waiting request is
    /// no list), so a test can tell the unit's confirmation reads from them.
    lists_sent: AtomicUsize,
    /// Address-enrichment passes begun, and the gate in front of their transport.
    enrichment_calls: AtomicUsize,
    enrichment_hold: AtomicBool,
    enrichment_delay_ms: std::sync::atomic::AtomicU64,
    /// Export replays begun, and the hold of a replay's blocking job between its
    /// claim of the outbox row and its completion.
    replay_calls: AtomicUsize,
    /// Export replays whose blocking job has returned (B2 fix round, P3b fix 6,
    /// R-P3bF6-3): the replay's own acknowledgement that its claim is settled.
    replay_completed: AtomicUsize,
    /// A replay waits, asynchronously and before it takes the publication gate or starts
    /// its blocking job, while this is set (B2 fix round, P3b fix 5, R-P3bF5-4): a test
    /// that stamps the store keeps the only unit that runs beside the loop out of flight.
    replay_hold_before_worker: AtomicBool,
    replay_hold_after_claim: AtomicBool,
    replay_claimed: AtomicUsize,
    /// Called on entry to the run's last transaction (C3 R8), synchronously.
    finish_entry: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Called synchronously, with the frame's source time, right after each stream
    /// frame committed and before anything else is done (B2 fix round, P2, fourth
    /// pass): a test sets a fence exactly between two frame commits with it.
    frame_committed: std::sync::Mutex<Option<Arc<dyn Fn(i64) + Send + Sync>>>,
    /// What every ordinary sleep of the Owner loop decided and did (C3 R2, R11).
    iteration_sleeps: std::sync::Mutex<Vec<IterationSleep>>,
    /// The loop's own steps that a proof of order needs, as they happened: drains,
    /// ends of the ordinary sleep, stream events taken, frames committed and the
    /// renewal's join (B2 fix round, P2).
    loop_log: std::sync::Mutex<Vec<LoopEvent>>,
    /// The ticks the loops have completed (B2 fix round, P2), each as the loop saw it
    /// the moment its tick, commit included, was over.
    tick_checkpoints: std::sync::Mutex<Vec<TickCheckpoint>>,
    /// The cumulative count of requests the test's server has answered, read at each
    /// completed tick (the account lists, for the quiet-window proofs).
    requests_probe: std::sync::Mutex<Option<Arc<dyn Fn() -> usize + Send + Sync>>>,
    /// The irreversible admission-loss fence of the loop under test, registered by the
    /// supervisor when it makes it (B2 fix round, P3b fix 1): a test can set it at a
    /// chosen point, with the admission itself untouched.
    admission_fence: std::sync::Mutex<Option<AdmissionFence>>,
    /// What the run's write guard was consulted for (B2 fix round, P3b fix 2): the
    /// audit says which store boundaries the run reached and what they were told. It
    /// is the audit only, never the guard itself: the guard holds the retained
    /// admission, which the seam must not keep alive after the run.
    write_audit: std::sync::Mutex<Option<Arc<WriteGuardAudit>>>,
    /// The boundary a test wants the next run's fence set at, and what to do there.
    write_trip: std::sync::Mutex<Option<(&'static str, usize, Arc<dyn Fn() + Send + Sync>)>>,
    /// The points of the supervisors a test has armed (B2 fix round, P2, second pass):
    /// the task that reaches one says so and waits for the test's release.
    gates: std::sync::Mutex<HashMap<&'static str, Arc<TestGate>>>,
    /// The cadence a test controls: the scheduler's clock and the loop's timers (see
    /// [`CadenceControl`]).
    cadence: std::sync::Mutex<Option<Arc<CadenceControl>>>,
    /// The clock the loops' scheduler is built on instead of the system clock, when a
    /// test selects one.
    scheduler_clock: std::sync::Mutex<Option<Arc<ManualClock>>>,
}

/// A tick of a supervised loop that has completed, as the loop saw it then (B2 fix
/// round, P2). A request counter that moves when a request arrives is not a completed
/// tick: this is taken after the tick's own commit.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct TickCheckpoint {
    at: Instant,
    /// The requests the test's server had answered (see `requests_probe`).
    requests: usize,
    tick_commits: usize,
    lone_reservation_commits: usize,
    settings_refreshes_completed: usize,
    iterations: usize,
    /// The code of the account state the scheduler was in.
    account: &'static str,
}

/// One step the loop logs for the tests that prove their order (B2 fix round, P2).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopEvent {
    /// A stream drain began (the shared drain budget's mark).
    Drain(Instant),
    /// The loop's ordinary sleep ended.
    SleepEnded(Instant),
    /// A `Healthy` stream event was taken and applied.
    HealthyProcessed(Instant),
    /// A stream frame of the car (Tesla id) with this source time was committed.
    FrameCommitted { vehicle: u64, observed_at_ms: i64 },
    /// The lease's renewal was fenced and joined.
    RenewalJoined,
    /// An `AuthRejected` stream event was taken and applied.
    RejectionProcessed,
    /// The supervisor's outer wait ended: whether the renewal's result was the arm
    /// that ended it, and whether the run's result was `Ok`.
    SupervisorSelected { by_renewal: bool, ok: bool },
    /// The lease's renewal ended on its own and its result was taken by the
    /// supervisor's wait (the other way to end than a join).
    RenewalEnded,
    /// The renewal's own revalidation was denied: it set the shared fence.
    RenewalDenied,
    /// The shutdown is about to wait for the credential unit's cleanup (its manager).
    CleanupWaiting,
    /// A confirmation answer was applied to the scheduler, at this monotonic instant
    /// and wall time of its clock (C3 R45: the deadline counts from here).
    ConfirmationApplied { at: Instant, wall_ms: i64 },
    /// The Fleet shutdown began waiting for a token exchange to settle.
    FleetSettlementBegun,
    /// Loop work other than the drains returned pending for the first time in a wait
    /// the stream queues are pumped through.
    AwaitedLoopWork,
    /// An orderly stop signal was sent to the stream run of the car (Tesla id), at the
    /// synchronous send of the stop, whether or not the run ever polls for it.
    StopSignalSent { vehicle: u64 },
    /// A failed account list was folded into the scheduler (the discovery failure the
    /// loop logs and backs off from).
    DiscoveryFailed,
    /// The confirmation reads the credential unit made were counted into the request
    /// ledger, once, in the order of the loop's own steps (B2 fix round, P3b fix 6,
    /// R-P3bF6-2): logged only for a nonzero count, after the ledger was updated.
    ConfirmationReadsCounted { reads: u32 },
    /// The drain consumed the final result of a stream run's task and classified it
    /// as terminal (B2 fix round, P3b fix 6, R-P3bF6-3): the settlement of that end is
    /// what the loop then owes before any further work.
    TerminalRunSettled,
}

/// A loop's tick, its commit included, is over: take its checkpoint for the test seam;
/// nothing in production. Under a controlled cadence the loop then waits for the test
/// to resume it.
#[cfg(not(test))]
async fn supervised_tick_completed(_account: &'static str) {}

#[cfg(test)]
async fn supervised_tick_completed(account: &'static str) {
    let Ok(seam) = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(Arc::clone) else {
        return;
    };
    let requests = seam
        .requests_probe
        .lock()
        .expect("requests probe")
        .as_ref()
        .map_or(0, |probe| probe());
    let checkpoint = TickCheckpoint {
        at: loop_now(),
        requests,
        tick_commits: seam.tick_commits.load(Ordering::SeqCst),
        lone_reservation_commits: seam.lone_reservation_commits.load(Ordering::SeqCst),
        settings_refreshes_completed: seam.settings_refreshes_completed.load(Ordering::SeqCst),
        iterations: seam.loop_iterations.load(Ordering::SeqCst),
        account,
    };
    seam.tick_checkpoints
        .lock()
        .expect("tick checkpoints")
        .push(checkpoint);
    let control = seam.cadence.lock().expect("cadence").clone();
    if let Some(control) = control {
        control.completed_tick(checkpoint).await;
    }
}

/// A stream frame committed: the test seam's hook for the commit, if one is installed;
/// nothing in production.
#[cfg(test)]
fn supervised_test_frame_committed(observed_at_ms: i64) {
    let hook = SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.frame_committed.lock().expect("frame hook").clone())
        .ok()
        .flatten();
    if let Some(hook) = hook {
        hook(observed_at_ms);
    }
}

/// Log a step of the loop for the test seam.
#[cfg(test)]
fn supervised_test_log(event: LoopEvent) {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        seam.loop_log.lock().expect("loop log").push(event);
    });
}

/// One ordinary sleep of the Owner loop, as the loop decided and ran it: the
/// previous drain's instant, the planned wait, when the sleep began, how long it
/// slept and when it ended. A test reads the loop's own decision instead of timing a
/// whole iteration, so a loaded machine cannot change what it proves.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct IterationSleep {
    planned: Duration,
    armed: bool,
    drain_mark: Instant,
    began: Instant,
    slept_for: Duration,
    ended: Instant,
    /// The events waiting in the streams' queues when the sleep ended (B2 fix round,
    /// P3b fix 6, S1): a drain after it must consume one only if one was queued.
    queued: usize,
}

/// How a test starts a stream run (see `stream_runs`).
#[cfg(test)]
type StreamRunFactory = dyn Fn(StreamInjection) -> JoinHandle<Result<(), crate::tesla_stream::StreamSupervisorError>>
    + Send
    + Sync;

/// What the loop hands an injected stream run.
#[cfg(test)]
struct StreamInjection {
    vehicle_id: VehicleId,
    /// The run's own admission number, from one.
    run: usize,
    events: mpsc::Sender<StreamEvent>,
    stop: oneshot::Receiver<()>,
    seam: Arc<SupervisedCollectorTestSeam>,
}

#[cfg(test)]
thread_local! {
    /// The seam of the export replay a blocking thread is running: task-locals do not
    /// reach `spawn_blocking`, so the replay passes it on.
    static REPLAY_TEST_SEAM: std::cell::RefCell<Option<Arc<SupervisedCollectorTestSeam>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct SupervisedCollectionCompletion {
    completed: oneshot::Sender<SupervisedCollectionCheckpoint>,
    resume: Option<oneshot::Receiver<()>>,
}

/// A test-owned receipt emitted only after `finish_collection` has committed.
/// It avoids opening a second SQLite connection while the supervised writer is
/// still active.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
struct SupervisedCollectionCheckpoint {
    snapshots_received: usize,
    observations_inserted: usize,
    observations_already_present: usize,
    snapshots_published: usize,
    vehicle_failures: usize,
    drives_closed: usize,
    charges_closed: usize,
    positions_materialised: usize,
    charge_samples_materialised: usize,
    lifecycle_quarantines: usize,
}

#[cfg(test)]
impl From<&ManualCollectionReport> for SupervisedCollectionCheckpoint {
    fn from(report: &ManualCollectionReport) -> Self {
        Self {
            snapshots_received: report.snapshots_received,
            observations_inserted: report.observations_inserted,
            observations_already_present: report.observations_already_present,
            snapshots_published: report.snapshots_published,
            vehicle_failures: report.vehicle_failures,
            drives_closed: report.drives_closed,
            charges_closed: report.charges_closed,
            positions_materialised: report.positions_materialised,
            charge_samples_materialised: report.charge_samples_materialised,
            lifecycle_quarantines: report.lifecycle_quarantines,
        }
    }
}

/// Count one iteration of a supervised loop for the test seam; nothing in
/// production.
#[cfg(not(test))]
fn supervised_loop_iteration() {}

#[cfg(test)]
fn supervised_loop_iteration() {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        seam.loop_iterations.fetch_add(1, Ordering::SeqCst);
        seam.loop_iteration_times
            .lock()
            .expect("loop iteration times")
            .push(loop_now());
    });
}

/// Count one settings refresh of a supervised loop for the test seam; nothing in
/// production.
#[cfg(not(test))]
fn supervised_settings_refresh() {}

#[cfg(test)]
fn supervised_settings_refresh() {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        seam.settings_refreshes.fetch_add(1, Ordering::SeqCst);
    });
}

/// Count one finished settings refresh of a supervised loop for the test seam;
/// nothing in production.
#[cfg(not(test))]
fn supervised_settings_refresh_completed() {}

#[cfg(test)]
fn supervised_settings_refresh_completed() {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        seam.settings_refreshes_completed
            .fetch_add(1, Ordering::SeqCst);
    });
}

/// Whether a test forces a due time into the past (C3 R11, CT55b); never in
/// production.
#[cfg(not(test))]
const fn supervised_forced_due_in_the_past() -> bool {
    false
}

#[cfg(test)]
fn supervised_forced_due_in_the_past() -> bool {
    SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.forced_due_in_the_past.load(Ordering::SeqCst))
        .unwrap_or(false)
}

#[cfg(test)]
impl SupervisedCollectorTestSeam {
    fn loop_iterations(&self) -> usize {
        self.loop_iterations.load(Ordering::SeqCst)
    }

    /// The start time of every iteration so far.
    fn loop_iteration_times(&self) -> Vec<Instant> {
        self.loop_iteration_times
            .lock()
            .expect("loop iteration times")
            .clone()
    }

    fn settings_refreshes(&self) -> usize {
        self.settings_refreshes.load(Ordering::SeqCst)
    }

    /// The settings refreshes that ran to their end.
    fn settings_refreshes_completed(&self) -> usize {
        self.settings_refreshes_completed.load(Ordering::SeqCst)
    }

    fn force_due_in_the_past(&self, forced: bool) {
        self.forced_due_in_the_past.store(forced, Ordering::SeqCst);
    }

    /// The decision times of the stored `no_data` events, oldest first.
    fn no_data_stored(&self) -> Vec<i64> {
        self.no_data_stored.lock().expect("no_data times").clone()
    }

    /// The completed ticks so far, oldest first.
    fn tick_checkpoints(&self) -> Vec<TickCheckpoint> {
        self.tick_checkpoints.lock().expect("tick checkpoints").clone()
    }

    /// Read `probe` at every completed tick from now on.
    fn set_requests_probe(&self, probe: Arc<dyn Fn() -> usize + Send + Sync>) {
        *self.requests_probe.lock().expect("requests probe") = Some(probe);
    }

    /// The loop's admission-loss fence, once the supervisor has made it.
    fn admission_fence(&self) -> AdmissionFence {
        self.admission_fence
            .lock()
            .expect("admission fence")
            .clone()
            .expect("the supervisor registered its fence")
    }

    /// Set the next run's fence when it reaches `point`, and run `action` then.
    fn trip_write_guard_at(
        &self,
        point: &'static str,
        nth: usize,
        action: Arc<dyn Fn() + Send + Sync>,
    ) {
        *self.write_trip.lock().expect("write trip") = Some((point, nth, action));
    }

    /// The audit of the loop's write guard, once the supervisor has made the guard.
    fn write_audit(&self) -> Arc<WriteGuardAudit> {
        self.write_audit
            .lock()
            .expect("write audit")
            .clone()
            .expect("the supervisor registered its write guard")
    }

    /// The logged steps of the loop so far, in the order they happened.
    fn loop_log(&self) -> Vec<LoopEvent> {
        self.loop_log.lock().expect("loop log").clone()
    }

    /// Start every car's stream run with `factory` from now on.
    fn inject_stream_runs(&self, factory: Arc<StreamRunFactory>) {
        *self.stream_runs.lock().expect("stream runs") = Some(factory);
    }

    fn stream_admissions(&self) -> usize {
        self.stream_admissions.load(Ordering::SeqCst)
    }

    fn lists_sent(&self) -> usize {
        self.lists_sent.load(Ordering::SeqCst)
    }

    fn enrichment_calls(&self) -> usize {
        self.enrichment_calls.load(Ordering::SeqCst)
    }

    fn replay_calls(&self) -> usize {
        self.replay_calls.load(Ordering::SeqCst)
    }

    fn replay_completions(&self) -> usize {
        self.replay_completed.load(Ordering::SeqCst)
    }

    fn replay_claims(&self) -> usize {
        self.replay_claimed.load(Ordering::SeqCst)
    }

    fn iteration_sleeps(&self) -> Vec<IterationSleep> {
        self.iteration_sleeps.lock().expect("iteration sleeps").clone()
    }

    fn set_finish_entry(&self, observer: Arc<dyn Fn() + Send + Sync>) {
        *self.finish_entry.lock().expect("finish entry") = Some(observer);
    }

    /// Run `hook` synchronously, with the frame's source time, right after every stream
    /// frame committed (the commit is over, nothing after it has begun).
    fn set_frame_committed_hook(&self, hook: Arc<dyn Fn(i64) + Send + Sync>) {
        *self.frame_committed.lock().expect("frame hook") = Some(hook);
    }

    /// Run just before a tick transaction commits: count the attempt and inject
    /// the fault when one is set.
    fn tick_commit_gate(&self) -> Result<(), CollectorError> {
        self.tick_commit_attempts.fetch_add(1, Ordering::SeqCst);
        self.tick_commit_times
            .lock()
            .expect("tick commit times")
            .push(Instant::now());
        if self.tick_commit_fault.load(Ordering::SeqCst) {
            Err(CollectorError::Store(StoreError::SyncMutation(
                "injected tick commit fault".to_owned(),
            )))
        } else {
            Ok(())
        }
    }

    fn tick_commits(&self) -> usize {
        self.tick_commits.load(Ordering::SeqCst)
    }

    fn set_tick_commit_fault(&self, fault: bool) {
        self.tick_commit_fault.store(fault, Ordering::SeqCst);
    }

    fn set_start_commit_fault(&self, fault: bool) {
        self.start_commit_fault.store(fault, Ordering::SeqCst);
    }

    async fn arm_paused_collection_completion(
        &self,
    ) -> (
        oneshot::Receiver<SupervisedCollectionCheckpoint>,
        oneshot::Sender<()>,
    ) {
        let (completed_tx, completed_rx) = oneshot::channel();
        let (resume_tx, resume_rx) = oneshot::channel();
        let mut completed = self.collection_completed.lock().await;
        assert!(
            completed.is_none(),
            "only one collection completion witness may be armed"
        );
        *completed = Some(SupervisedCollectionCompletion {
            completed: completed_tx,
            resume: Some(resume_rx),
        });
        (completed_rx, resume_tx)
    }

    async fn collection_finished(&self, report: &ManualCollectionReport) {
        let completed = self.collection_completed.lock().await.take();
        if let Some(completed) = completed {
            let _ = completed.completed.send(report.into());
            if let Some(resume) = completed.resume {
                let _ = resume.await;
            }
        }
    }
}

#[cfg(test)]
async fn supervised_test_collection_finished(report: &ManualCollectionReport) {
    let seam = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(Arc::clone).ok();
    if let Some(seam) = seam {
        seam.collection_finished(report).await;
    }
}

/// The loop admits a car's stream run (C3 R28): the test seam counts it and, when a
/// factory is installed, takes the run over after the normal checks. Returns the run's
/// task, or the stop channel back when nothing is injected.
#[cfg(test)]
fn supervised_test_stream_run(
    vehicle_id: VehicleId,
    events: &mpsc::Sender<StreamEvent>,
    stop: oneshot::Receiver<()>,
) -> Result<JoinHandle<Result<(), crate::tesla_stream::StreamSupervisorError>>, oneshot::Receiver<()>>
{
    let Ok(seam) = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(Arc::clone) else {
        return Err(stop);
    };
    let run = seam.stream_admissions.fetch_add(1, Ordering::SeqCst) + 1;
    let factory = seam.stream_runs.lock().expect("stream runs").clone();
    match factory {
        Some(factory) => Ok(factory(StreamInjection {
            vehicle_id,
            run,
            events: events.clone(),
            stop,
            seam,
        })),
        None => Err(stop),
    }
}

/// An account list is about to be sent (test seam).
#[cfg(test)]
fn supervised_test_list_sent() {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        seam.lists_sent.fetch_add(1, Ordering::SeqCst);
    });
}

/// The gate in front of an address-enrichment pass's transport (test seam): it counts
/// the pass, holds it while a test says so and delays it by what a test sets. A pass
/// that has not reached its transport has touched no provider.
#[cfg(test)]
async fn supervised_test_enrichment_gate() {
    let Ok(seam) = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(Arc::clone) else {
        return;
    };
    seam.enrichment_calls.fetch_add(1, Ordering::SeqCst);
    while seam.enrichment_hold.load(Ordering::SeqCst) {
        sleep(Duration::from_millis(5)).await;
    }
    let delay = seam.enrichment_delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        sleep(Duration::from_millis(delay)).await;
    }
}

/// An export replay begins (test seam): counted, and its blocking job gets the seam.
#[cfg(test)]
fn supervised_test_replay_begun() -> Option<Arc<SupervisedCollectorTestSeam>> {
    let seam = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(Arc::clone).ok()?;
    seam.replay_calls.fetch_add(1, Ordering::SeqCst);
    Some(seam)
}

/// A replay's blocking job has claimed the outbox row (test seam): it counts the claim
/// and waits while a test holds it, before the job publishes and completes the claim.
#[cfg(test)]
fn supervised_test_replay_claimed() {
    let Some(seam) = REPLAY_TEST_SEAM.with(|cell| cell.borrow().clone()) else {
        return;
    };
    seam.replay_claimed.fetch_add(1, Ordering::SeqCst);
    while seam.replay_hold_after_claim.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// An ordinary sleep of the Owner loop ended (test seam).
#[cfg(test)]
fn supervised_test_iteration_slept(sleep: IterationSleep) {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        seam.iteration_sleeps
            .lock()
            .expect("iteration sleeps")
            .push(sleep);
    });
}

/// Called by the start function right after its transaction committed (test seam):
/// the scoped fault, when a test sets one; nothing in production.
#[cfg(not(test))]
const fn supervised_test_after_start_commit() -> Result<(), CollectorError> {
    Ok(())
}

#[cfg(test)]
fn supervised_test_after_start_commit() -> Result<(), CollectorError> {
    let fault = SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.start_commit_fault.load(Ordering::SeqCst))
        .unwrap_or(false);
    if fault {
        Err(CollectorError::Store(StoreError::SyncMutation(
            "injected start fault after commit".to_owned(),
        )))
    } else {
        Ok(())
    }
}

/// The run's last transaction is entered (test seam).
#[cfg(test)]
fn supervised_test_finish_entered() {
    let observer = SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.finish_entry.lock().expect("finish entry").clone())
        .ok()
        .flatten();
    if let Some(observer) = observer {
        observer();
    }
}

/// A `no_data` event was stored by a committed tick transaction (test seam).
#[cfg(test)]
fn supervised_test_no_data_stored(decided_at_ms: i64) {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        seam.no_data_stored
            .lock()
            .expect("no_data times")
            .push(decided_at_ms);
    });
}

/// A lone reservation commit was made (test seam).
#[cfg(test)]
fn supervised_test_lone_reservation_commit() {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        seam.lone_reservation_commits.fetch_add(1, Ordering::SeqCst);
    });
}

/// A tick transaction committed (the counter of the tick-commit hook).
#[cfg(test)]
fn supervised_tick_committed() {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        seam.tick_commits.fetch_add(1, Ordering::SeqCst);
    });
}

/// Signal every run, then wait for all of them together: the five-second deadlines
/// are concurrent, so stuck runs cost five seconds in all, not five each (C3 R28,
/// B2 clarification 3). The handles stay in their runtimes until each task has ended,
/// so a wait that is dropped leaves them to the runtimes' own abort on drop.
#[cfg(test)]
async fn stop_manual_probe_streams(streams: &mut [VehicleStreamRuntime]) {
    for stream in streams.iter_mut() {
        if let Some(shutdown) = stream._shutdown.take() {
            let _ = shutdown.send(());
        }
    }
    let deadline = Instant::now() + STREAM_SHUTDOWN_TIMEOUT;
    let all: Vec<VehicleId> = streams.iter().map(|stream| stream.vehicle_id).collect();
    join_or_abort_selected_at(streams, &all, deadline).await;
}

#[cfg(test)]
async fn stop_and_clear_manual_probe_streams(streams: &mut Vec<VehicleStreamRuntime>) {
    stop_manual_probe_streams(streams).await;
    streams.clear();
}

/// A sensitive authority failure forbids every later credential-bearing stream
/// operation. Abort locally first, then await the child before dropping its
/// sender: `VehicleStreamRuntime::Drop` treats a live sender as an orderly
/// shutdown request and would otherwise send `data:unsubscribe`. The handles stay in
/// their runtimes until each task has ended, so a cancelled wait loses none.
async fn abort_and_clear_manual_probe_streams_without_egress(
    streams: &mut Vec<VehicleStreamRuntime>,
) {
    let all: Vec<VehicleId> = streams.iter().map(|stream| stream.vehicle_id).collect();
    abort_selected_tasks(streams, &all);
    join_selected_tasks(streams, &all).await;
    for stream in streams.iter_mut() {
        let _ = stream._shutdown.take();
    }
    streams.clear();
}

impl TerrainFuse {
    fn available(&mut self, now: Instant) -> bool {
        if self.blown_until.is_some_and(|until| now >= until) {
            self.blown_until = None;
            self.failures.clear();
        }
        self.blown_until.is_none()
    }

    fn failure(&mut self, now: Instant) {
        if !self.available(now) {
            return;
        }
        self.failures
            .retain(|at| now.saturating_duration_since(*at) < TERRAIN_FUSE_WINDOW);
        self.failures.push(now);
        if self.failures.len() >= 2 {
            self.blown_until = now.checked_add(TERRAIN_FUSE_RESET);
        }
    }
}

/// Result safe to print from a one-shot collection service. It contains local
/// UUIDs and numeric vehicle ids, but never a bearer token, URL, or response.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ManualCollectionReport {
    pub source_id: Uuid,
    pub request_audit_correlation_id: Uuid,
    pub vehicles_seen: usize,
    pub online_vehicles_seen: usize,
    pub snapshots_received: usize,
    pub observations_inserted: usize,
    pub observations_already_present: usize,
    pub snapshots_published: usize,
    pub vehicle_failures: usize,
    pub drives_closed: usize,
    pub charges_closed: usize,
    pub positions_materialised: usize,
    pub charge_samples_materialised: usize,
    pub lifecycle_quarantines: usize,
}

/// Safe first-run receipt. It contains only the selected numeric vehicle id,
/// its optional display name, and whether the initial V2 base was published.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct NativeSetupReport {
    pub selected_vehicle_id: i64,
    pub display_name: Option<String>,
    pub snapshots_published: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct NativeSetupBatchReport {
    pub vehicles: Vec<NativeSetupVehicle>,
    pub snapshots_published: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct NativeSetupVehicle {
    pub vehicle_id: i64,
    pub display_name: Option<String>,
}

#[derive(Clone)]
enum CollectionAuth {
    Legacy {
        manager: Arc<tokio::sync::Mutex<LegacyAuthManager>>,
        fuse: Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
        refresh: Arc<LegacyRefreshCoordinator>,
        allow_refresh: bool,
        region: StreamRegion,
    },
}

/// The fence the loss of sensitive access raises (C5 R15a, R33a). A 401 is no
/// longer recovered by a queued task: the credential unit handles it inside the
/// request (R40, R41), so what remains here is the typed terminal state of a
/// failed helper, persistence or compare-and-set. It is kept apart from every
/// request so a later request cannot forget it.
#[derive(Default)]
struct LegacyRefreshCoordinator {
    sensitive_failure: Arc<AtomicBool>,
    sensitive_failure_notify: Arc<tokio::sync::Notify>,
    /// The credential unit's shutdown flag (C5 R33a), registered when the run
    /// starts. It lives outside the manager's lock, so the collector can raise it
    /// at once while a stream's credential check or a request still holds the
    /// manager (Codex R12).
    shutdown: std::sync::OnceLock<Arc<AtomicBool>>,
}

impl LegacyRefreshCoordinator {
    /// Hold the unit's shutdown flag; the first one wins.
    fn register_shutdown(&self, flag: Arc<AtomicBool>) {
        let _ = self.shutdown.set(flag);
    }

    /// No new token request starts from now on (C5 R33a). Called the moment the
    /// collector's shutdown begins, before anything waits for the manager.
    fn raise_shutdown(&self) {
        if let Some(flag) = self.shutdown.get() {
            flag.store(true, Ordering::Release);
        }
    }

    fn has_sensitive_failure(&self) -> bool {
        self.sensitive_failure.load(Ordering::Acquire)
    }

    fn sensitive_failure_result(&self) -> Result<(), CollectorError> {
        if self.has_sensitive_failure() {
            Err(CollectorError::SensitiveAccessUnavailable)
        } else {
            Ok(())
        }
    }

    /// Publish the terminal fence and wake the waiter, once.
    fn raise_sensitive_failure(&self) {
        if !self.sensitive_failure.swap(true, Ordering::AcqRel) {
            self.sensitive_failure_notify.notify_waiters();
        }
    }

    async fn wait_for_sensitive_failure(&self) {
        loop {
            let notified = self.sensitive_failure_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.has_sensitive_failure() {
                return;
            }
            notified.await;
        }
    }

    async fn wait_for_prior(&self) -> Result<(), CollectorError> {
        self.sensitive_failure_result()
    }
}

fn normalize_sensitive_access_error(error: CollectorError) -> CollectorError {
    match error {
        CollectorError::LegacyAuthManager(error) if error.is_sensitive_access_failure() => {
            CollectorError::SensitiveAccessUnavailable
        }
        CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(error))
            if error.is_sensitive_access_failure() =>
        {
            CollectorError::SensitiveAccessUnavailable
        }
        CollectorError::OwnerApi(OwnerApiError::CredentialAuthorityUnavailable) => {
            CollectorError::SensitiveAccessUnavailable
        }
        error => error,
    }
}

fn is_sensitive_access_failure(error: &CollectorError) -> bool {
    matches!(error, CollectorError::SensitiveAccessUnavailable)
        // The credential unit's own sensitive outcome, kept as it was by its shutdown
        // (C5 R33a, B2 clarification 3): it names what the unit found, and it is as
        // much the loss of the authority as the bare denial is.
        || matches!(error, CollectorError::LegacyAuthManager(error) if error.is_sensitive_access_failure())
}

/// The credential unit's error inside a collector error, however it was wrapped
/// (C5 R56): by the manager, by a request wrapper, or bare. `None` for any
/// other error, and for the lost authority, which is a credential error and not
/// one of the unit's.
fn legacy_auth_error_of(error: &CollectorError) -> Option<&LegacyAuthError> {
    match error {
        CollectorError::LegacyAuth(error)
        | CollectorError::LegacyAuthManager(LegacyAuthManagerError::Auth(error))
        | CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
            error,
        ))) => Some(error),
        _ => None,
    }
}

/// Whether the credential unit has stopped the account for good until the owner
/// acts (C5 R56, R43): a sign-out or a configuration error. A refusal is the
/// scheduler's account state instead (C3 R48, R49), and a wait is only a wait.
fn is_credential_stop(error: &CollectorError) -> bool {
    legacy_auth_error_of(error).is_some_and(|error| {
        error.is_terminal_auth_failure() && !matches!(error, LegacyAuthError::OwnerApiRefused { .. })
    })
}

async fn wait_for_legacy_refresh_before_owner(auth: &CollectionAuth) -> Result<(), CollectorError> {
    let CollectionAuth::Legacy { refresh, .. } = auth;
    refresh.wait_for_prior().await?;
    Ok(())
}

async fn wait_for_legacy_refresh_sensitive_failure(
    auth: &CollectionAuth,
) -> Result<(), CollectorError> {
    let CollectionAuth::Legacy { refresh, .. } = auth;
    refresh.wait_for_sensitive_failure().await;
    Err(CollectorError::SensitiveAccessUnavailable)
}

/// The stream's rejection hand-off (C5 R40): a stream `Rejected` transition
/// carries no generation and is never counted by itself, so it asks the
/// credential unit for one confirmation read with the pair in use. Only the loss
/// of sensitive access is an error of the run; every other outcome is a state of
/// the unit, which the loop sees through its waits.
async fn refresh_after_stream_authentication_rejection(
    client: &OwnerApi,
    auth: &CollectionAuth,
    transition: StreamAuthenticationTransition,
) -> Result<(), CollectorError> {
    if transition != StreamAuthenticationTransition::Rejected {
        return Ok(());
    }
    let CollectionAuth::Legacy {
        manager,
        fuse,
        refresh,
        ..
    } = auth;
    let result = {
        let mut fuse = fuse.lock().await;
        let mut manager = manager.lock().await;
        client
            .confirm_stream_rejection(&mut manager, &mut fuse)
            .await
            .map_err(CollectorError::from)
            .map_err(normalize_sensitive_access_error)
    };
    match result {
        Err(error) if is_sensitive_access_failure(&error) => {
            refresh.raise_sensitive_failure();
            Err(error)
        }
        _ => Ok(()),
    }
}

/// Shut the credential unit down and return its outcome (C5 R33a, B2 clarification
/// 3): `Err` carries only the loss of the authority. The unit's own sensitive
/// outcome comes first, kept as it was (an exchange whose result was unknown after
/// the admission was denied stays `SensitiveRotationOutcomeUnknown`, not the bare
/// denial), and the final revalidation of the admission follows it, made by the
/// unit at the boundary where the shutdown ends, so a denial that arrived while the
/// unit finished its exchange is never reported as a clean stop.
async fn shutdown_legacy_refresh(auth: &CollectionAuth) -> Result<(), CollectorError> {
    let CollectionAuth::Legacy { manager, refresh, .. } = auth;
    // The flag goes up before the manager is awaited: a task that already owns the
    // lock must not start a token request after the shutdown was chosen (Codex R12).
    refresh.raise_shutdown();
    // C5 R33a: no new token request starts, an exchange in flight is waited for
    // and its outcome handled, a successor still waiting for its write gets one
    // last try, and the status is flushed.
    let mut manager = manager.lock().await;
    manager.shutdown().await.map_err(|error| match error {
        // The bare loss of the admission, as every other path reports it.
        LegacyAuthManagerError::Credential(_) => CollectorError::SensitiveAccessUnavailable,
        error => CollectorError::LegacyAuthManager(error),
    })
}

/// The credential unit's wait and its shutdown, as one outcome: a sensitive loss
/// outranks an ordinary error, and the first sensitive one wins (a later success or
/// generic denial cannot replace it).
#[cfg(test)]
async fn drain_and_shutdown_legacy_refresh(auth: &CollectionAuth) -> Result<(), CollectorError> {
    let result = wait_for_legacy_refresh_before_owner(auth).await;
    let shutdown = shutdown_legacy_refresh(auth).await;
    match (result, shutdown) {
        (Err(error), _) if is_sensitive_access_failure(&error) => Err(error),
        (_, Err(error)) if is_sensitive_access_failure(&error) => Err(error),
        (Err(error), _) => Err(error),
        (Ok(()), shutdown) => shutdown,
    }
}

/// A request's result, with the credential unit's loss of sensitive access
/// raised as the terminal fence and every error normalised for the loops.
fn settle_legacy_result<T>(
    auth: &CollectionAuth,
    result: Result<T, OwnerApiAuthError>,
) -> Result<T, CollectorError> {
    let result = result
        .map_err(CollectorError::from)
        .map_err(normalize_sensitive_access_error);
    if let Err(error) = &result
        && is_sensitive_access_failure(error)
    {
        let CollectionAuth::Legacy { refresh, .. } = auth;
        refresh.raise_sensitive_failure();
    }
    result
}

/// What the collector may do with its next request (C5 R39): read from the
/// credential unit's own state, sending nothing. A busy manager means a request
/// or an exchange is under way, and the request queues behind it, so it is
/// `Ready` here; the unit makes the real decision when the request reaches it.
fn legacy_auth_wait(auth: &CollectionAuth, wall_now: SystemTime) -> AuthWait {
    let CollectionAuth::Legacy { manager, .. } = auth;
    let Ok(manager) = manager.try_lock() else {
        return AuthWait::Ready;
    };
    manager.wait_view(epoch_seconds_of(wall_now))
}

/// Epoch seconds of a wall time (C5 R37); a time before 1970 reads as 0, which
/// is below the clock floor of R38.
fn epoch_seconds_of(wall_now: SystemTime) -> i64 {
    wall_now
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

/// The check each tick request makes at its own start, before it is counted (C5
/// R39, C3 R47): a wait or a stop leaves the request unsent, uncounted and still
/// due, with no failure; the error it returns is the unit's own, so the
/// scheduler's classifier applies the same row as for a request that reached the
/// unit. A request already handed to the unit is never cancelled.
fn legacy_auth_gate(auth: &CollectionAuth) -> Option<CollectorError> {
    let now = SystemTime::now();
    if matches!(
        legacy_auth_wait(auth, now),
        AuthWait::Ready | AuthWait::UntilExchangeEnds { .. }
    ) {
        return None;
    }
    // A busy manager answers `Ready` above: a request or an exchange is under way
    // and this one queues behind it. It asks again at its own start, once it
    // holds the manager (`legacy_auth_start_error`).
    let CollectionAuth::Legacy { manager, .. } = auth;
    let manager = manager.try_lock().ok()?;
    legacy_auth_start_error(&manager, now)
}

/// R39 for a request that holds the credential unit: `None` when it may start;
/// otherwise the unit's own error, so the scheduler's classifier applies the same
/// row as for a request that reached the unit. Nothing is sent, nothing is
/// counted and the car stays due.
fn legacy_auth_start_error(manager: &LegacyAuthManager, now: SystemTime) -> Option<CollectorError> {
    match manager.wait_view(epoch_seconds_of(now)) {
        AuthWait::Ready | AuthWait::UntilExchangeEnds { .. } => None,
        AuthWait::For(wait) => Some(CollectorError::LegacyAuth(
            LegacyAuthError::AuthUnavailable {
                retry_at: Some(epoch_seconds_of(now).saturating_add(
                    i64::try_from(wait.as_secs()).unwrap_or(i64::MAX),
                )),
            },
        )),
        AuthWait::Stopped => Some(CollectorError::LegacyAuth(
            manager
                .stop_view(epoch_seconds_of(now))
                .unwrap_or(LegacyAuthError::AuthUnavailable { retry_at: None }),
        )),
    }
}

/// How one tick request is counted (C3 R24, R25, C5 R39): when the credential unit
/// really hands it to the network, and not before (Codex R08).
#[derive(Clone, Copy)]
struct RequestAccounting<'a> {
    ledger: &'a RequestLedger,
    category: RequestCategory,
}

/// The account's fuse, held for one request, with the request counter armed. Each
/// request the unit sends while it is armed is counted at the moment it is sent
/// (`LegacyAuthFuse::note_send`) and completed when the guard ends, whether the
/// request finished or its future was dropped: a request that the unit refused
/// unsent (a pause that began while this one queued, a stop) was never sent and
/// is never counted.
struct CountedFuse<'a> {
    fuse: tokio::sync::MutexGuard<'a, LegacyAuthFuse>,
    counted: Option<(RequestLedger, RequestCategory)>,
}

impl<'a> CountedFuse<'a> {
    fn arm(
        mut fuse: tokio::sync::MutexGuard<'a, LegacyAuthFuse>,
        accounting: Option<RequestAccounting<'_>>,
    ) -> Self {
        let counted = accounting.map(|accounting| (accounting.ledger.clone(), accounting.category));
        if let Some((ledger, category)) = &counted {
            let ledger = ledger.clone();
            let category = *category;
            fuse.arm_send_counter(Arc::new(move || {
                let _ = ledger.start(category);
            }));
        }
        Self { fuse, counted }
    }
}

impl std::ops::Deref for CountedFuse<'_> {
    type Target = LegacyAuthFuse;

    fn deref(&self) -> &LegacyAuthFuse {
        &self.fuse
    }
}

impl std::ops::DerefMut for CountedFuse<'_> {
    fn deref_mut(&mut self) -> &mut LegacyAuthFuse {
        &mut self.fuse
    }
}

impl Drop for CountedFuse<'_> {
    fn drop(&mut self) {
        let sent = self.fuse.disarm_send_counter();
        if let Some((ledger, category)) = &self.counted {
            for _ in 0..sent {
                ledger.complete(*category, false);
            }
        }
    }
}

/// What the loop does for the credential unit at the top of an iteration (C5
/// R28, R39, R44a): a stopped or waiting unit reads the stored generation every
/// 60 s, so a new login lifts a stop and an observer's next pair ends its wait
/// without a restart; a unit with nothing to hand out yet (a retry that has
/// come, an imported pair's first need, an expired token) is asked to refresh
/// now, which the gates of R16 allow or refuse. The request waits for nothing
/// here: a busy manager means a request is doing this itself.
async fn maintain_legacy_credential(client: &OwnerApi, auth: &CollectionAuth) {
    let CollectionAuth::Legacy { manager, refresh, .. } = auth;
    let Ok(mut manager) = manager.try_lock() else {
        return;
    };
    let now = SystemTime::now();
    // C5 R15a, R24, R43: what the unit owes between requests, whether or not a
    // request is due: a result its detached exchange published, the write of a
    // successor or the closing of an attempt that failed, a status write that
    // failed. Without this a successor whose write failed waits for the next
    // request to retry (Codex R07).
    manager.service();
    // A sensitive outcome the unit met while it applied its exchange's result ends the
    // run like any other loss of the authority (B2 fix round, P3b fix 2, R-P3bF2-2).
    if manager.sensitive_outcome().is_some() {
        refresh.raise_sensitive_failure();
        return;
    }
    match manager.wait_view(epoch_seconds_of(now)) {
        AuthWait::Ready | AuthWait::UntilExchangeEnds { .. } => {}
        AuthWait::Stopped => {
            manager.recheck_external_write();
        }
        AuthWait::For(_) => {
            if let Err(error) = manager
                .refresh_if_due(&client.legacy_auth_http_client(), now)
                .await
                && error.is_sensitive_access_failure()
            {
                refresh.raise_sensitive_failure();
            }
        }
    }
}

/// The credential unit's state at the top of an iteration, applied to the
/// scheduler and the lease (C5 R43, R47, R52; C3 R47, R48): a stop reached
/// outside a scheduled request is seen at once, a refusal the unit holds becomes
/// the account's state, a lifted stop collects again, and the lease is
/// `auth_terminal` exactly while the account is signed out. Returns whether
/// the account is stopped for good until the owner acts (the loop then sends
/// nothing).
fn apply_credential_state(
    auth: &CollectionAuth,
    scheduler: &mut VehicleScheduler,
    state: &watch::Sender<SupervisedCollectorState>,
    now: Instant,
) -> bool {
    let CollectionAuth::Legacy { manager, .. } = auth;
    let Ok(manager) = manager.try_lock() else {
        return false;
    };
    let status = manager.status_now();
    set_supervised_collector_state(
        state,
        if status.state.lease_is_terminal() {
            SupervisedCollectorState::AuthenticationTerminal
        } else {
            SupervisedCollectorState::Active
        },
    );
    let seen = credential_state_of(&status);
    scheduler.credential_state_seen(seen, now);
    // A wait the scheduler took for the unit's time ends when the unit says it can
    // hand out a token (C5 R39): an observer's next pair, a refresh that came.
    if manager.wait_view(epoch_seconds_of(SystemTime::now())) == AuthWait::Ready {
        scheduler.end_auth_wait(now);
    }
    matches!(seen, CredentialStateSeen::TerminalStop)
}

/// The scheduler's reading of the credential unit's status (C3 R47, R48).
fn credential_state_of(status: &AuthStatus) -> CredentialStateSeen {
    match status.stop() {
        Some(AuthStop::ApiRefused(RefusalReason::OwnerApi)) => {
            CredentialStateSeen::ApiRefused(CredentialRefusal::OwnerApi)
        }
        Some(AuthStop::ApiRefused(RefusalReason::Client)) => {
            CredentialStateSeen::ApiRefused(CredentialRefusal::Client)
        }
        Some(AuthStop::SignedOut(_)) => CredentialStateSeen::TerminalStop,
        _ if status.state == AuthState::ConfigurationError => CredentialStateSeen::TerminalStop,
        _ => CredentialStateSeen::Ready,
    }
}

/// The owner's "retry collection" reaches the credential unit (C5 R44 (b), C3
/// R65): it lifts the stops whose refresh token is still valid, and the fuse is
/// reset. A stop that needs a new sign-in stays and the unit says so.
async fn retry_legacy_credential(auth: &CollectionAuth) {
    let CollectionAuth::Legacy { manager, fuse, .. } = auth;
    let mut fuse = fuse.lock().await;
    let mut manager = manager.lock().await;
    if manager.owner_retry().is_ok() {
        manager.sync_fuse(&mut fuse);
    }
}

/// The owner's actions apply in the tick (C3 R65); a `retry_collection` among
/// them first reaches the credential unit, so both units lift together.
async fn apply_owner_actions_with_credentials(
    scheduler: &mut VehicleScheduler,
    auth: &CollectionAuth,
    actions: &[OwnerActionRow],
    now: Instant,
) {
    if actions
        .iter()
        .any(|action| action.kind == OwnerActionKind::RetryCollection)
    {
        retry_legacy_credential(auth).await;
    }
    scheduler.apply_owner_actions(actions, now);
}

/// What the credential unit's fuse kept for the collector since the last look (C5
/// R41a, C3 R24, R25): the confirmation reads it made, each one `state` request, and
/// the inconclusive answers of those reads.
struct ConfirmationOutcomes {
    reads: u32,
    answers: Vec<OwnerApiError>,
}

/// Take the confirmation count and the kept answers once, together. Acquiring the
/// fuse can wait for a confirmation that holds it for tens of seconds (the production
/// wrapper keeps the fuse and the manager through the read), so the loop runs this
/// inside a pumped wait and applies the outcome synchronously afterwards (C3 R11,
/// C4 R56 g, C5 R41a).
async fn take_confirmation_outcomes(auth: &CollectionAuth) -> ConfirmationOutcomes {
    let CollectionAuth::Legacy { fuse, .. } = auth;
    let mut fuse = fuse.lock().await;
    ConfirmationOutcomes {
        reads: fuse.take_confirmation_count(),
        answers: fuse.take_confirmation_errors(),
    }
}

/// The collector's request counter counts each confirmation read the unit made as
/// one `state` request (C5 R41a, C3 R24, R25).
fn count_confirmation_reads(reads: u32, counter: &RequestLedger) {
    for _ in 0..reads {
        counter.start(RequestCategory::State);
        counter.complete(RequestCategory::State, false);
    }
    #[cfg(test)]
    if reads > 0 {
        supervised_test_log(LoopEvent::ConfirmationReadsCounted { reads });
    }
}

/// R41a: the answer of a confirmation read that was neither a 2xx nor a 401 is
/// handled by the collector's own classification like any list answer: a 429
/// reaches the rate-limit wait with its `Retry-After`, a 410 or a 403 that names
/// the Fleet API reaches the refusal rules, and anything else takes the list's
/// back-off (C5-S4). The request that triggered the read has already failed with
/// `AuthUnavailable`.
fn apply_confirmation_answers(
    answers: Vec<OwnerApiError>,
    scheduler: &mut VehicleScheduler,
    now: Instant,
) {
    for error in answers {
        #[cfg(test)]
        supervised_test_log(LoopEvent::ConfirmationApplied {
            at: now,
            wall_ms: scheduler.wall_or_last(),
        });
        scheduler.discovery_failed_for_error(&CollectorError::OwnerApi(error), now);
    }
}

#[cfg(test)]
async fn list_vehicles_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
) -> Result<Vec<Vehicle>, CollectorError> {
    list_vehicles_counted(client, auth, None).await
}

/// One list of the account. `accounting` counts each request the unit really
/// sends, the resend after a refresh included, as one `state` request (C3 R24,
/// R25).
async fn list_vehicles_counted(
    client: &OwnerApi,
    auth: &CollectionAuth,
    accounting: Option<RequestAccounting<'_>>,
) -> Result<Vec<Vehicle>, CollectorError> {
    wait_for_legacy_refresh_before_owner(auth).await?;
    if let Some(error) = legacy_auth_gate(auth) {
        return Err(error);
    }
    let CollectionAuth::Legacy { manager, fuse, .. } = auth;
    let vehicles = {
        let mut fuse = CountedFuse::arm(fuse.lock().await, accounting);
        let mut manager = manager.lock().await;
        // R39 again at the request's own start, now that it holds the unit: a pause
        // or a stop that began while it queued leaves it unsent and uncounted.
        if let Some(error) = legacy_auth_start_error(&manager, SystemTime::now()) {
            return Err(error);
        }
        #[cfg(test)]
        supervised_test_list_sent();
        client
            .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
            .await
    };
    let mut vehicles = settle_legacy_result(auth, vehicles)?;
    for vehicle in &mut vehicles {
        // A live car's suspend length is final (C3 R60): no model lookup.
        vehicle.settings.suspend_min_resolved = true;
    }
    Ok(vehicles)
}

/// What the credential unit answered when asked for its refusal-check refresh (C3
/// R48 c, C5 R49): it refreshed the pair once (or it already had been, which is
/// the same to the list), or it may not refresh at all (observer mode, an access
/// token only), or the refresh failed.
#[derive(Debug)]
enum RefusalCheck {
    /// The pair was refreshed; the list may be sent once more.
    Refreshed,
    /// Observer mode refreshes nothing: a plain 403 is only a client error.
    ObserverRefreshDisabled,
    /// The credentials cannot be refreshed at all (an access token only).
    RefreshNotPossible,
    /// The refresh failed; the failure is classified by its own authentication row,
    /// never as a refusal.
    Failed(CollectorError),
}

/// Ask for one refresh of the pair, to rule out a stale token that a server
/// answers with 403 instead of 401 (C3 R48 c, C5 R49). The credential unit
/// decides: a pair already newer than the one the refused list carried needs no
/// exchange, otherwise one forced refresh runs through its gates; concurrent asks
/// for the same generation reach it one after the other and share the exchange.
async fn refusal_check_refresh_for_auth(client: &OwnerApi, auth: &CollectionAuth) -> RefusalCheck {
    let CollectionAuth::Legacy {
        manager,
        fuse,
        refresh,
        ..
    } = auth;
    let carried = fuse.lock().await.last_account_generation();
    let outcome = manager
        .lock()
        .await
        .refusal_check_refresh(&client.legacy_auth_http_client(), carried)
        .await;
    match outcome {
        RefusalRefresh::Refreshed(_) => RefusalCheck::Refreshed,
        RefusalRefresh::ObserverRefreshDisabled => RefusalCheck::ObserverRefreshDisabled,
        RefusalRefresh::RefreshNotPossible => RefusalCheck::RefreshNotPossible,
        RefusalRefresh::Failed(error) if error.is_sensitive_access_failure() => {
            refresh.raise_sensitive_failure();
            RefusalCheck::Failed(CollectorError::SensitiveAccessUnavailable)
        }
        RefusalRefresh::Failed(error) => RefusalCheck::Failed(CollectorError::LegacyAuthManager(error)),
    }
}

/// The answer to a list after C3 R48 (c)'s check: the list's own result, and
/// whether the resent list answered 403 again after a successful refresh, which is
/// the repetition that refuses the account.
struct CheckedList {
    result: Result<Vec<Vehicle>, CollectorError>,
    refused_after_check: bool,
}

/// List the account's cars with the refusal check of C3 R48 (c) through the
/// retained credential manager. See [`list_with_refusal_check_using`] for the rule;
/// `owner_wire` is the scheduler's mode test, passed by value so the scheduler is not
/// held across the requests. `counter` is the request counter (C3 R25): Owner-wire
/// requests are counted and never billed.
async fn list_vehicles_with_refusal_check(
    client: &OwnerApi,
    auth: &CollectionAuth,
    owner_wire: bool,
    counter: &RequestLedger,
) -> CheckedList {
    // C5 R39: a wait or a stop leaves the list unsent, uncounted and still due,
    // with no failure; a list already handed to the unit is never cancelled.
    if let Some(error) = legacy_auth_gate(auth) {
        return CheckedList {
            result: Err(error),
            refused_after_check: false,
        };
    }
    list_with_refusal_check_using(
        owner_wire,
        // Each list sent, the resend after a refresh included, is one request of
        // category `state` (C3 R24, R25), counted when it is really sent.
        || {
            list_vehicles_counted(
                client,
                auth,
                Some(RequestAccounting {
                    ledger: counter,
                    category: RequestCategory::State,
                }),
            )
        },
        || refusal_check_refresh_for_auth(client, auth),
    )
    .await
}

/// The rule of C3 R48 (c), with the list request and the credential unit's refresh
/// as parameters so the unit's outcomes can be doubled until it lands: a plain 403
/// on the list asks for one refresh and resends the list once, in the same tick
/// (R56); a 403 to the resent list, plain or Fleet-named, is the refusal. Any other
/// answer is the list's own answer and nothing is remembered. When no refresh is
/// possible nothing is resent and the plain 403 stays a client error of the list; a
/// failed refresh is classified by its own authentication row, never as a refusal.
async fn list_with_refusal_check_using<List, ListFuture, Check, CheckFuture>(
    owner_wire: bool,
    mut list: List,
    check: Check,
) -> CheckedList
where
    List: FnMut() -> ListFuture,
    ListFuture: Future<Output = Result<Vec<Vehicle>, CollectorError>>,
    Check: FnOnce() -> CheckFuture,
    CheckFuture: Future<Output = RefusalCheck>,
{
    let first = list().await;
    let wanted = owner_wire && matches!(&first, Err(error) if is_plain_owner_forbidden(error));
    if !wanted {
        return CheckedList {
            result: first,
            refused_after_check: false,
        };
    }
    match check().await {
        RefusalCheck::Refreshed => {
            let second = list().await;
            let refused_after_check =
                matches!(&second, Err(error) if is_owner_wire_forbidden(error));
            CheckedList {
                result: second,
                refused_after_check,
            }
        }
        RefusalCheck::ObserverRefreshDisabled | RefusalCheck::RefreshNotPossible => CheckedList {
            result: first,
            refused_after_check: false,
        },
        RefusalCheck::Failed(error) => CheckedList {
            result: Err(error),
            refused_after_check: false,
        },
    }
}

#[cfg(test)]
async fn vehicle_data_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
    power_gate: Option<&StreamPowerGate>,
) -> Result<VehicleData, CollectorError> {
    vehicle_data_counted(client, auth, vehicle_id, power_gate, None).await
}

/// One vehicle read. The unit decides at the request's own start whether it may
/// go (C5 R39), and `accounting` counts it when it is really sent (Codex R08).
async fn vehicle_data_counted(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
    power_gate: Option<&StreamPowerGate>,
    accounting: Option<RequestAccounting<'_>>,
) -> Result<VehicleData, CollectorError> {
    wait_for_legacy_refresh_before_owner(auth).await?;
    if power_gate.is_some_and(|gate| !gate.is_confirmed()) {
        return Err(CollectorError::OwnerApi(
            OwnerApiError::StreamPowerNotConfirmed,
        ));
    }
    if let Some(error) = legacy_auth_gate(auth) {
        return Err(error);
    }
    let CollectionAuth::Legacy { manager, fuse, .. } = auth;
    let result = {
        let mut fuse = CountedFuse::arm(fuse.lock().await, accounting);
        let mut manager = manager.lock().await;
        if let Some(error) = legacy_auth_start_error(&manager, SystemTime::now()) {
            return Err(error);
        }
        client
            .vehicle_data_with_legacy_auth_fused(&mut manager, &mut fuse, vehicle_id, power_gate)
            .await
    };
    settle_legacy_result(auth, result)
}

#[cfg(test)]
async fn vehicle_state_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
) -> Result<String, CollectorError> {
    vehicle_state_counted(client, auth, vehicle_id, None).await
}

async fn vehicle_state_counted(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
    accounting: Option<RequestAccounting<'_>>,
) -> Result<String, CollectorError> {
    wait_for_legacy_refresh_before_owner(auth).await?;
    if let Some(error) = legacy_auth_gate(auth) {
        return Err(error);
    }
    let CollectionAuth::Legacy { manager, fuse, .. } = auth;
    let result = {
        let mut fuse = CountedFuse::arm(fuse.lock().await, accounting);
        let mut manager = manager.lock().await;
        if let Some(error) = legacy_auth_start_error(&manager, SystemTime::now()) {
            return Err(error);
        }
        client
            .vehicle_state_with_legacy_auth_fused(&mut manager, &mut fuse, vehicle_id)
            .await
    };
    settle_legacy_result(auth, result)
}

async fn vehicle_probe_counted(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
    accounting: Option<RequestAccounting<'_>>,
) -> Result<bool, CollectorError> {
    wait_for_legacy_refresh_before_owner(auth).await?;
    if let Some(error) = legacy_auth_gate(auth) {
        return Err(error);
    }
    let CollectionAuth::Legacy { manager, fuse, .. } = auth;
    let result = {
        let mut fuse = CountedFuse::arm(fuse.lock().await, accounting);
        let mut manager = manager.lock().await;
        if let Some(error) = legacy_auth_start_error(&manager, SystemTime::now()) {
            return Err(error);
        }
        client
            .vehicle_probe_with_legacy_auth_fused(&mut manager, &mut fuse, vehicle_id)
            .await
    };
    settle_legacy_result(auth, result)
}

async fn finish_collection(
    store: &HubStore,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
) -> Result<ManualCollectionReport, CollectorError> {
    finish_collection_for_provider(store, cursor_key, collection, CollectorProvider::Legacy).await
}

async fn finish_collection_for_provider(
    store: &HubStore,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
    provider: CollectorProvider,
) -> Result<ManualCollectionReport, CollectorError> {
    // The warning carries the failure's class and no Tesla identifier (C3 R9).
    for failure in &collection.failures {
        tracing::warn!(
            error = %failure.error,
            "owner API vehicle collection failed"
        );
    }
    let publication_gate = store.acquire_publication_gate().await?;
    let received_at_ms = current_epoch_millis()?;
    let mut report =
        persist_collection_atomic_for_provider(store, collection, received_at_ms, provider)?;
    let lifecycle =
        materialise_lifecycle_for_collection_provider(store, collection, received_at_ms, provider)?;
    report.drives_closed += lifecycle.drives_closed;
    report.charges_closed += lifecycle.charges_closed;
    report.positions_materialised += lifecycle.positions_materialised;
    report.charge_samples_materialised += lifecycle.charge_samples_materialised;
    report.lifecycle_quarantines += lifecycle.lifecycle_quarantines;
    report.snapshots_published = publish_compatibility_snapshots_for_provider(
        store,
        &publication_gate,
        cursor_key,
        collection,
        received_at_ms,
        provider,
    )?;
    Ok(report)
}
