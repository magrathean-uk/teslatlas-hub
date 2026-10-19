// SPDX-License-Identifier: AGPL-3.0-only

// Scheduler core and cadence (C3 part K2): the types of C3 R3, the clock and
// jitter seams (R2, R44), the cadence and due logic (R16 to R22), the many-car
// rules (R12) and the loop's waiting rule (R11). The behaviour of the later
// parts is in their own files, named below.
//
// The Fleet request counter, estimate and cap (C3 part K7: R24 to R27) are in
// `scheduler_counters.rs`: the month key, the counts, the reservation and the
// estimate, and the scheduler's admission of each scheduled request.
//
// Errors, back-off and rate limits (C3 part K5) are in `scheduler_errors.rs`, and
// the account states, vehicle holds, probes and owner actions of part K6 in
// `scheduler_holds.rs`; all are the same `impl VehicleScheduler`.
//
// Sleep, suspend and stream evidence (C3 part K4) is the rest of the same
// `impl VehicleScheduler`, in `scheduler_sleep.rs`: the idle window and the
// suspension, the wake read, stream evidence and the power gate calls, the
// stream's start and stop rule and run spacing. `vehicle_succeeded`,
// `vehicle_failed_for_error`, `apply_reported_state` and `due_vehicles` call into
// it.
//
// Durable state, ticks, gaps and status (C3 part K3): the persisted views and
// their diff against memory (R4, R5), the restart load (R7), the commit
// protocol of a tick (R6), collection gaps (R8), the decision log fields (R9,
// R10) and the status value (R13). The scheduler never writes the store itself:
// `take_tick` hands over what changed, the tick transaction of `tick.rs` writes
// it, and `commit_succeeded` or `commit_failed` reports the outcome.

// ---------------------------------------------------------------------------
// Constants. Every value carries its reason and the requirement that gives it.
// ---------------------------------------------------------------------------

/// How often the supervised loop re-reads the owner's edits and actions
/// (C3 R11, R62, R65; `CONTROL_SETTINGS_REFRESH` = `collection_sleep_cap(false)`
/// in section 6). An owner edit or action must apply within seconds, and the
/// read is a small one, so five seconds costs almost nothing.
const CONTROL_SETTINGS_REFRESH: Duration = Duration::from_secs(5);

/// The longest the loop sleeps while streams are active, so queued stream
/// frames are drained well inside the plan's 250 ms intake target (C3 R11,
/// `K_STREAM_DRAIN_CAP` = `collection_sleep_cap(true)` in section 6).
const K_STREAM_DRAIN_CAP: Duration = Duration::from_millis(100);

/// The loop's longest sleep (C3 R11, `K_MAX_IDLE_WAIT`): it re-evaluates at
/// least once a minute, so a lost wake-up cannot stall it.
const K_MAX_IDLE_WAIT: Duration = Duration::from_secs(60);

/// Minimum spacing of two consecutive loop iterations that performed no
/// request and no write (C3 R11, `K_LOOP_FLOOR`). It bounds a scheduling bug
/// to ten iterations a second, which keeps the no-busy-loop invariant even if a
/// due time is wrong.
const K_LOOP_FLOOR: Duration = Duration::from_millis(100);

/// Most requests in flight at once (C3 R12, `K_MAX_IN_FLIGHT`). The plan's
/// household scope rarely exceeds four cars, so four concurrent requests serve
/// every car in one tick without a burst.
const K_MAX_IN_FLIGHT: usize = 4;

/// The divergence between the wall and monotonic clocks above which the
/// scheduler acts (C3 R2, `K_CLOCK_DIVERGENCE`): above timer and scheduling
/// jitter, below any gap worth recording.
pub(crate) const K_CLOCK_DIVERGENCE: Duration = Duration::from_secs(60);

/// [`K_CLOCK_DIVERGENCE`] in milliseconds, the unit of stored wall times.
const K_CLOCK_DIVERGENCE_MS: i64 = K_CLOCK_DIVERGENCE.as_millis() as i64;

/// Shortest silence worth recording as a `hub_down` gap, and how long an account
/// wait must last before it becomes a `rate_limited` or `tesla_unreachable` gap
/// (C3 R8, `K_GAP_MIN`): one lease heartbeat period plus `K_CLOCK_DIVERGENCE`.
/// The last heartbeat precedes a stop by up to one period, so a shorter measured
/// silence may be pure heartbeat phase.
const K_GAP_MIN: Duration =
    SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL.saturating_add(K_CLOCK_DIVERGENCE);

/// [`K_GAP_MIN`] in milliseconds, the unit of stored wall times.
const K_GAP_MIN_MS: i64 = K_GAP_MIN.as_millis() as i64;

/// The longest a persisted rate-limit wait loads as (C3 R2, R45,
/// `K_RETRY_AFTER_MAX`): an hour bounds a wrong `Retry-After` header and, as the
/// bound of the clamp on load, keeps a backward wall step from stretching a wait
/// beyond its own length.
const K_RETRY_AFTER_MAX: Duration = Duration::from_secs(3_600);

/// The longest a persisted account probe deadline loads as in a billing
/// suspension (C3 R2, R50, `K_BILLING_PROBE`): six hours bound the unattended
/// delay after the owner raises Tesla's limit to a quarter of a day, at four
/// list requests a day.
const K_BILLING_PROBE: Duration = Duration::from_secs(6 * 60 * 60);

/// The longest a persisted pause probe deadline loads as (C3 R2, R51,
/// `K_PAUSE_PROBE`): an hour is at most 24 requests a day, and only while the
/// car is listed online.
const K_PAUSE_PROBE: Duration = Duration::from_secs(60 * 60);

/// The longest a persisted service probe deadline loads as (C3 R2, R52,
/// `K_SERVICE_PROBE`): a car in service rarely leaves it within minutes, and a
/// quarter of an hour bounds the delay once it does.
const K_SERVICE_PROBE: Duration = Duration::from_secs(15 * 60);

/// Base of the one back-off function (C3 R44, `K_BACKOFF_BASE`): it equals the
/// fastest Owner-wire read interval (10 s), so the first retry is never earlier
/// than ordinary polling.
const K_BACKOFF_BASE: Duration = Duration::from_secs(10);

/// How long a Driving car's stream may stay silent before it counts as an
/// outage for cadence (C3 R28, `K_DRIVE_FRAME_SILENCE`, **provisional**, gate
/// M-S4): 30 s without a frame is 417 m at 50 km/h, and switching to the 10 s
/// read fallback then restores street-level spacing (R18).
const K_DRIVE_FRAME_SILENCE: Duration = Duration::from_secs(30);

/// Smallest value of the per-car minute settings the scheduler uses (C3 R61):
/// a minute is the finest granularity of the owner's setting, and a zero would
/// make an idle window or suspension vanish.
const SETTING_MIN_MINUTES: i64 = 1;

/// Largest value of the per-car minute settings the scheduler uses (C3 R61): a
/// day is a generous upper bound that keeps millisecond arithmetic far from
/// overflow.
const SETTING_MAX_MINUTES: i64 = 1440;

/// The longest sleep of the supervised loop for a given stream state (C3 R11).
const fn collection_sleep_cap(has_active_streams: bool) -> Duration {
    if has_active_streams {
        K_STREAM_DRAIN_CAP
    } else {
        CONTROL_SETTINGS_REFRESH
    }
}

/// `now + delay`, or a deadline so far away that it means "never" when the sum
/// overflows. An overflowing deadline must never mean "retry now" (C3 R45: a
/// wrong header must not cause a busy retry).
fn deadline_after(now: Instant, delay: Duration) -> Instant {
    now.checked_add(delay)
        .or_else(|| now.checked_add(RETRY_OVERFLOW_FALLBACK))
        .unwrap_or(now)
}

// ---------------------------------------------------------------------------
// Seams (C3 R2, R44).
// ---------------------------------------------------------------------------

/// The only source of time for scheduling (C3 R2). Monotonic instants drive
/// every due time; wall time is read only for stored values. Tests step the two
/// apart.
trait CollectorClock: Send + Sync {
    fn mono_now(&self) -> Instant;
    fn wall_now_ms(&self) -> Result<i64, CollectorError>;
}

/// The production clock: `tokio::time::Instant::now()` and the system wall
/// clock through `current_epoch_millis`.
struct SystemCollectorClock;

impl CollectorClock for SystemCollectorClock {
    fn mono_now(&self) -> Instant {
        Instant::now()
    }

    fn wall_now_ms(&self) -> Result<i64, CollectorError> {
        current_epoch_millis()
    }
}

/// A test clock whose wall time can be stepped apart from its monotonic time.
#[cfg(test)]
struct ManualClock {
    state: std::sync::Mutex<(Instant, i64)>,
}

#[cfg(test)]
impl ManualClock {
    fn new(mono: Instant, wall_ms: i64) -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new((mono, wall_ms)),
        })
    }

    /// Advance both clocks together, as real time does.
    fn advance(&self, by: Duration) {
        let mut state = self.state.lock().expect("clock lock");
        state.0 += by;
        state.1 += i64::try_from(by.as_millis()).expect("test duration");
    }

    /// Advance only the monotonic clock.
    fn advance_mono(&self, by: Duration) {
        self.state.lock().expect("clock lock").0 += by;
    }

    /// Step the wall clock, forward or back, leaving the monotonic clock alone.
    fn step_wall_ms(&self, delta_ms: i64) {
        self.state.lock().expect("clock lock").1 += delta_ms;
    }
}

#[cfg(test)]
impl CollectorClock for ManualClock {
    fn mono_now(&self) -> Instant {
        self.state.lock().expect("clock lock").0
    }

    fn wall_now_ms(&self) -> Result<i64, CollectorError> {
        Ok(self.state.lock().expect("clock lock").1)
    }
}

/// Source of the jitter fraction of the back-off function (C3 R44). `draw`
/// returns a value in `[0, 1)`.
trait JitterSource: Send {
    fn draw(&mut self) -> f64;
}

/// The production jitter: a per-scheduler xorshift64* generator seeded from the
/// operating system's random source. Jitter only spreads retries of several
/// vehicles and Hubs, so it needs no cryptographic quality; if the system
/// source is unavailable the seed falls back to the clock's nanoseconds, which
/// still differs between processes.
struct OsJitter {
    state: u64,
}

impl OsJitter {
    fn new() -> Self {
        let mut seed = [0u8; 8];
        let state = if getrandom::fill(&mut seed).is_ok() {
            u64::from_le_bytes(seed)
        } else {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0x9e37_79b9_7f4a_7c15, |elapsed| elapsed.as_nanos() as u64)
        };
        // A zero state would stay zero for ever.
        Self { state: state | 1 }
    }
}

impl JitterSource for OsJitter {
    fn draw(&mut self) -> f64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        let scrambled = x.wrapping_mul(0x2545_f491_4f6c_dd1d);
        // The top 53 bits give an exactly representable fraction below 1.
        (scrambled >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A test jitter that cycles through a fixed sequence.
#[cfg(test)]
struct FixedJitter {
    values: Vec<f64>,
    next: usize,
}

#[cfg(test)]
impl FixedJitter {
    fn constant(value: f64) -> Box<dyn JitterSource + Send> {
        Box::new(Self {
            values: vec![value],
            next: 0,
        })
    }
}

#[cfg(test)]
impl JitterSource for FixedJitter {
    fn draw(&mut self) -> f64 {
        let value = self.values[self.next % self.values.len()];
        self.next += 1;
        value
    }
}

/// The one back-off function (C3 R44): exponential with "equal jitter", between
/// half and all of the nominal delay. Callers increment their counter before the
/// call; `failures = 0` gives the same value as 1. `maximum` is
/// `max_backoff_seconds`. A `u` outside `[0, 1]`, or not a number, is treated as
/// 0 so a faulty source cannot panic `Duration::mul_f64`.
fn backoff_delay(failures: u32, maximum: Duration, u: f64) -> Duration {
    let exponent = failures.saturating_sub(1).min(31);
    let nominal = K_BACKOFF_BASE.saturating_mul(1u32 << exponent).min(maximum);
    let u = if u.is_finite() {
        u.clamp(0.0, 1.0)
    } else {
        0.0
    };
    nominal / 2 + nominal.mul_f64(u) / 2
}

/// Load a persisted future deadline (C3 R2): `mono_now + clamp(deadline_ms -
/// wall_now_ms, 0, bound)`, where `bound` is the duration that produced the
/// deadline. A backward wall step must not stretch a deadline beyond its own
/// length, and a forward step only shortens it.
fn deadline_from_wall(
    mono_now: Instant,
    wall_now_ms: i64,
    deadline_ms: i64,
    bound: Duration,
) -> Instant {
    let bound_ms = i64::try_from(bound.as_millis()).unwrap_or(i64::MAX);
    let remaining_ms = deadline_ms
        .saturating_sub(wall_now_ms)
        .clamp(0, bound_ms)
        .unsigned_abs();
    deadline_after(mono_now, Duration::from_millis(remaining_ms))
}

/// Load a persisted past instant (C3 R2): its age is `clamp(wall_now_ms - p, 0,
/// bound)`; it is absent when the age reaches `bound`, else `mono_now` minus the
/// age (or `mono_now` itself when that is not representable). After a backward
/// step the age is clamped to 0, so a limit holds for its full length rather than
/// vanishing; a value older than its bound no longer matters.
fn past_instant_from_wall(
    mono_now: Instant,
    wall_now_ms: i64,
    persisted_ms: i64,
    bound: Duration,
) -> Option<Instant> {
    let bound_ms = i64::try_from(bound.as_millis()).unwrap_or(i64::MAX);
    let age_ms = wall_now_ms.saturating_sub(persisted_ms).clamp(0, bound_ms);
    if age_ms >= bound_ms {
        return None;
    }
    Some(
        mono_now
            .checked_sub(Duration::from_millis(age_ms.unsigned_abs()))
            .unwrap_or(mono_now),
    )
}

// ---------------------------------------------------------------------------
// Vocabulary of C3 R3.
// ---------------------------------------------------------------------------

/// What a car is doing, as far as cadence is concerned (C3 R3, R15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollPhase {
    Unknown,
    Asleep,
    Offline,
    Online,
    Suspended,
    Driving,
    Charging,
    Updating,
}

impl PollPhase {
    /// The value stored in `collector_vehicle_state.phase` (C3 R4).
    const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Asleep => "asleep",
            Self::Offline => "offline",
            Self::Online => "online",
            Self::Suspended => "suspended",
            Self::Driving => "driving",
            Self::Charging => "charging",
            Self::Updating => "updating",
        }
    }

    /// Driving, Charging and Updating: the phases that have their own read
    /// cadence (R16, R17).
    const fn is_session(self) -> bool {
        matches!(self, Self::Driving | Self::Charging | Self::Updating)
    }

    /// The phases in which `vehicle_data` is read on a timer (R16, R17).
    const fn is_read_phase(self) -> bool {
        matches!(self, Self::Online) || self.is_session()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VehicleHold {
    None,
    Disabled,
    Service,
    Paused(PauseReason),
    Removed,
}

impl VehicleHold {
    /// The value stored in `collector_vehicle_state.hold` (C3 R4).
    const fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Disabled => "disabled",
            Self::Service => "service",
            Self::Paused(_) => "paused",
            Self::Removed => "removed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PauseReason {
    PersistentFailure,
    MobileAccessDisabled,
    LegalPrivacy,
    AccessRefused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccountState {
    Collecting,
    OwnerApiRefused,
    ProviderRefused,
    FleetAccessRefused,
    BillingSuspended,
    CapReached,
    ConfigError,
    AccountLocked,
    /// Derived from C5, never persisted by C3.
    CredentialStop,
}

impl AccountState {
    /// The value stored in `collector_account_state.state` (C3 R4); the derived
    /// credential stop has no stored value and names itself for status only.
    const fn code(self) -> &'static str {
        match self {
            Self::Collecting => "collecting",
            Self::OwnerApiRefused => "owner_api_refused",
            Self::ProviderRefused => "provider_refused",
            Self::FleetAccessRefused => "fleet_access_refused",
            Self::BillingSuspended => "billing_suspended",
            Self::CapReached => "cap_reached",
            Self::ConfigError => "config_error",
            Self::AccountLocked => "account_locked",
            Self::CredentialStop => "credential_stop",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitReason {
    RateLimited,
    TeslaUnreachable,
    AuthWait,
    Store,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorClass {
    Auth,
    /// The credential unit's `AuthUnavailable` (C5 R56, C3 R42, R47): the account waits
    /// for the unit's own time.
    AuthWait,
    CredentialStop,
    Unavailable,
    RateLimited,
    Unreachable,
    Transient,
    Protocol,
    NotFound,
    InService,
    OwnerApiRefusal,
    ProviderRefusal,
    FleetAccess,
    AccessRefused,
    Billing,
    Config,
    Locked,
    MobileAccessDisabled,
    LegalPrivacy,
    ClientError,
    GateNotConfirmed,
}

/// What a probe of a held vehicle is (C3 R3a, R51, R52).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeKind {
    /// Owner-wire Service hold: a non-waking record request.
    ServiceRecord,
    /// Fleet Service hold and every Paused probe: a read, only when the latest
    /// list or record state is `online`.
    Read,
}

/// The result of a probe (C3 R3a).
#[derive(Clone, Copy, Debug)]
enum ProbeOutcome<'a> {
    InService(bool),
    Read(PollPhase, bool),
    Failed(&'a CollectorError),
}

/// The kind of scheduled request an error came from (C3 R42).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestKind {
    List,
    Record,
    Read,
    Probe,
}

/// Billing categories of scheduled requests (C3 R24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestCategory {
    Data,
    State,
    Wake,
    Command,
}

/// The Fleet cost view shown to the owner (C3 R26, R27): the month's gross and
/// net cost in cents, the local cap, the projection and the date polling stops
/// around (both withheld until a day has passed), and the cap warning. The
/// figures are whole cents from integer cost units (`scheduler_counters.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CostEstimate {
    gross_cents: u64,
    /// Gross less Tesla's monthly discount, never below zero; never used for the
    /// cap (R26).
    net_cents: u64,
    cap_cents: u32,
    /// The gross has reached `K_CAP_WARN` of the cap (R27).
    cap_warning: bool,
    projected_cents: Option<u64>,
    stop_ms: Option<i64>,
    /// "projection after the first day" until the projection is shown (R26).
    note: Option<&'static str>,
    /// What the cap does and does not govern (R27).
    scope_note: &'static str,
}

/// Identifier of a stored owner action (C3 R4, R65).
type ActionId = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerActionKind {
    RetryCollection,
    ResumeVehicle,
}

/// One unapplied row of `collector_owner_actions` (C3 R4, R65). The car is named
/// by its Hub id, the only id the control CLI and the GUI know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OwnerActionRow {
    id: ActionId,
    kind: OwnerActionKind,
    vehicle: Option<Uuid>,
}

impl OwnerActionRow {
    /// The scheduler's view of a stored row; `None` for an action name this
    /// version does not know (the table's CHECK admits only the two it does).
    fn from_record(record: &crate::db::CollectorActionRecord) -> Option<Self> {
        let kind = match record.action.as_str() {
            "retry_collection" => OwnerActionKind::RetryCollection,
            "resume_vehicle" => OwnerActionKind::ResumeVehicle,
            _ => return None,
        };
        Some(Self {
            id: record.action_id,
            kind,
            vehicle: record.vehicle_id,
        })
    }
}

/// What the scheduler found out about the clocks at one loop iteration (C3 R2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockEvent {
    /// Wall time ran ahead of monotonic time by more than `K_CLOCK_DIVERGENCE`:
    /// the host slept (or the wall clock stepped forward). The gap covers the
    /// wall interval and is already in the pending tick.
    HostSuspended { from_wall_ms: i64, to_wall_ms: i64 },
    /// Wall time ran behind monotonic time by more than `K_CLOCK_DIVERGENCE`:
    /// logged once, nothing else changes, because a backward step loses no data.
    WallClockSteppedBack,
}

/// What the scheduler remembers of a car's lifecycle (C3 R14, R40).
#[derive(Clone, Debug, PartialEq, Eq)]
struct LifecycleView {
    last_reach: Option<String>,
    drive_open: bool,
}

/// The stream facts the cadence reads (C3 R3, R28).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StreamView {
    connected: bool,
    last_frame: Option<Instant>,
    outage_reported: bool,
    task_failures: u32,
    restart_not_before: Option<Instant>,
    /// When the current outage began, for the recovery report.
    outage_started: Option<Instant>,
    /// Unhealthy events in the current outage, for the reports.
    outage_failures: u32,
    /// The last run of this car's stream ended with an audit failure (C3 R28,
    /// C4 R56 e): stream receipts cannot be written. It is in memory only, and
    /// clears when a later run emits `Healthy`.
    audit_failed: bool,
}

/// The per-car settings the scheduler uses, clamped (C3 R61).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EffectiveSettings {
    enabled: bool,
    use_streaming_api: bool,
    suspend_after_idle: Duration,
    suspend: Duration,
    req_not_unlocked: bool,
}

impl EffectiveSettings {
    /// `suspend_after_idle_min` and `suspend_min` are clamped to 1..=1440
    /// minutes (C3 R61); `idle_suspend_after_seconds` in `[collector]` is not
    /// read, so each policy has one source.
    fn from_car(settings: &crate::hub_pack::ProjectionCarSettings) -> Self {
        let minutes = |value: i64| {
            Duration::from_secs(
                value
                    .clamp(SETTING_MIN_MINUTES, SETTING_MAX_MINUTES)
                    .unsigned_abs()
                    * 60,
            )
        };
        Self {
            enabled: settings.enabled,
            use_streaming_api: settings.use_streaming_api,
            suspend_after_idle: minutes(settings.suspend_after_idle_min),
            suspend: minutes(settings.suspend_min),
            req_not_unlocked: settings.req_not_unlocked,
        }
    }
}

/// One row of the in-process status per car (C3 R13).
#[derive(Clone, Debug, PartialEq, Eq)]
struct VehicleStatus {
    hub_id: Option<Uuid>,
    vehicle_id: VehicleId,
    phase: PollPhase,
    hold: VehicleHold,
    reason: &'static str,
    since_ms: i64,
    next_in_ms: Option<u64>,
    last_success_ms: Option<i64>,
    last_error: Option<ErrorClass>,
    /// A plain-words note about the car's data sources (C3 R28, R54): the stream
    /// is the only source while REST is paused, or its receipts cannot be written.
    detail: Option<&'static str>,
}

/// The in-process view of the collector (C3 R13). The R4 tables are the status
/// other processes read (R66); part K3 fills the gap counts and publishes it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CollectorStatus {
    mode: crate::config::CollectorMode,
    account_state: AccountState,
    account_reason: &'static str,
    /// The state in plain words (C3 R13, R48): never "credentials failed" for a
    /// refusal of the Owner API or a provider.
    account_text: &'static str,
    account_since_ms: i64,
    wait: Option<(WaitReason, i64)>,
    month: String,
    requests: [(RequestCategory, u64); 4],
    estimate: Option<CostEstimate>,
    vehicles: Vec<VehicleStatus>,
    open_gaps: usize,
    alert_gaps_unnotified: usize,
    reservation_commits: u64,
    telemetry_check_due: bool,
}

// Shapes of the stream reports the loops log (C3 R10, R28).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamOutageStatus {
    consecutive_failures: u32,
    outage_duration: Duration,
    owner_api_fallback_scheduled: bool,
    owner_api_fallback_after: Option<Duration>,
    suspended: bool,
    live_power_gate: bool,
    phase: PollPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamRecoveryStatus {
    failures: u32,
    outage_duration: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamRecovery {
    Unchanged,
    Recovered(StreamRecoveryStatus),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamOutage {
    Ignored,
    Active(StreamOutageStatus),
}

// ---------------------------------------------------------------------------
// Timing: the cadence tables of C3 R16 and R17 in one shape.
// ---------------------------------------------------------------------------

/// The intervals of the active mode (C3 R16 for Legacy and Provider, R17 for
/// Fleet polling), resolved once at construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Timing {
    /// Driving read interval with no healthy stream (the fallback).
    driving: Duration,
    /// Driving read interval while the stream is healthy; `None` in Fleet
    /// polling, where the cadence never assumes a stream (C3 R64).
    driving_stream: Option<Duration>,
    charging: Duration,
    online: Duration,
    updating: Duration,
    /// State check interval for a Suspended car.
    state_suspended: Duration,
    /// State check interval for Asleep, Offline and Unknown cars.
    state_other: Duration,
    /// Interval of the inventory list when no state check is due.
    inventory: Duration,
    max_backoff: Duration,
    /// `K_MIN_READ_SPACING`: no evidence-triggered read is faster than the
    /// mode's fastest Driving interval (C3 R22).
    min_read_spacing: Duration,
    /// `T_gap`, the offline drive timeout (C3 section 0).
    offline_drive_timeout: Duration,
}

impl Timing {
    fn new(
        cadence: &CollectorCadence,
        mode: crate::config::CollectorMode,
        fleet: &crate::config::FleetPolicy,
    ) -> Self {
        let mut timing = if mode.is_owner_wire() {
            Self {
                driving: cadence.driving,
                driving_stream: Some(cadence.driving_stream),
                charging: cadence.charging,
                online: cadence.online,
                updating: cadence.updating,
                state_suspended: cadence.suspended,
                state_other: cadence.sleeping,
                inventory: cadence.inventory,
                max_backoff: cadence.maximum_backoff,
                min_read_spacing: Duration::ZERO,
                offline_drive_timeout: cadence.offline_drive_timeout,
            }
        } else {
            let intervals = &fleet.intervals;
            Self {
                driving: intervals.driving,
                driving_stream: None,
                charging: intervals.charging,
                online: intervals.online,
                updating: intervals.updating,
                state_suspended: intervals.state_check,
                state_other: intervals.state_check,
                inventory: intervals.inventory,
                max_backoff: cadence.maximum_backoff,
                min_read_spacing: Duration::ZERO,
                offline_drive_timeout: cadence.offline_drive_timeout,
            }
        };
        timing.min_read_spacing = timing
            .driving
            .min(timing.driving_stream.unwrap_or(timing.driving));
        timing
    }

    /// The read interval of a phase, or `None` for a phase that is not read on
    /// a timer. `stream_healthy` selects the Owner-wire Driving row.
    fn read_interval(&self, phase: PollPhase, stream_healthy: bool) -> Option<Duration> {
        match phase {
            PollPhase::Driving => Some(match (stream_healthy, self.driving_stream) {
                (true, Some(interval)) => interval,
                _ => self.driving,
            }),
            PollPhase::Charging => Some(self.charging),
            PollPhase::Updating => Some(self.updating),
            PollPhase::Online => Some(self.online),
            PollPhase::Unknown | PollPhase::Asleep | PollPhase::Offline | PollPhase::Suspended => {
                None
            }
        }
    }

    /// The state check interval of a phase that is not read on a timer.
    fn state_interval(&self, phase: PollPhase) -> Duration {
        if phase == PollPhase::Suspended {
            self.state_suspended
        } else {
            self.state_other
        }
    }
}

// ---------------------------------------------------------------------------
// Records.
// ---------------------------------------------------------------------------

/// One record per configured vehicle (C3 R3). `vehicle.settings` is the one copy
/// of the car's settings; `settings` is its clamped view (R61).
#[derive(Clone, Debug)]
struct ScheduledVehicle {
    vehicle: Vehicle,
    /// The Hub's own id for the car, the only id the decision log carries (R9).
    hub_id: Option<Uuid>,
    settings: EffectiveSettings,
    phase: PollPhase,
    /// When the phase began, monotonic and wall.
    phase_since: (Instant, i64),
    hold: VehicleHold,
    /// The reason code of the last transition (C3 section 7.4).
    reason: &'static str,
    /// Tesla's last list or record `state`.
    reported_state: Option<String>,
    /// The next timed read; `None` when no read is scheduled.
    next_read_due: Option<Instant>,
    /// Owner-wire targeted record check (R36, R46).
    next_check_due: Option<Instant>,
    /// Pause and service probes (R51, R52).
    next_probe_due: Option<Instant>,
    /// Online only: when the idle window ends and the suspension begins (R31).
    suspend_at: Option<Instant>,
    /// Suspended only: when the suspension ends (R32, R33).
    suspended_until: Option<Instant>,
    last_in_use: Option<Instant>,
    last_read: Option<Instant>,
    last_wake_read: Option<Instant>,
    last_success: Option<(Instant, i64)>,
    offline_from_session: bool,
    consecutive_failures: u32,
    first_failure: Option<Instant>,
    last_error: Option<ErrorClass>,
    stream: StreamView,
    missing_lists: u8,
    /// When the last absence that counted towards `missing_lists` was seen (C3
    /// R53): two absences count only when they are an inventory interval apart,
    /// unless a `NotFound` read asked for the second list.
    missing_last: Option<Instant>,
    /// A read, record or probe answered `NotFound` and the list it asked for has
    /// not answered yet (C3 R53): that list counts an absence at once.
    not_found_check: bool,
    lifecycle: Option<LifecycleView>,
    lifecycle_stale: bool,
    last_stored_obs_ms: Option<i64>,
    force_closed_at_gap: bool,
    /// `source_vehicle_state` of the car's current discovery row, read with its
    /// lifecycle view (C3 R14): the outer `None` means not read yet. It is valid
    /// only while the lifecycle view is.
    current_discovery: Option<Option<String>>,
    /// A request for this car is in flight (C3 R12, R22).
    in_flight: bool,
    /// The record was made from the owner's configuration alone and no list has
    /// named the car yet (C3 R3: one record per configured car). It lets the
    /// lifecycle check (R40), the restored holds and the status work whatever the
    /// account state, without a list; it asks for no state check and is not a car
    /// the loops pass to enrichment or export until a list names it.
    placeholder: bool,
    /// The pending or in-flight read was made due by stream evidence while the car
    /// was Suspended, so it is sent only if the live stream still proves the car
    /// awake (C3 R36).
    gated_read: Option<GatedEvidence>,
    /// A wake read (R34 b) is due or in flight: its result starts the wake watch
    /// (R31).
    wake_read_pending: bool,
    /// The read at the end of a suspension (R33) is due or in flight.
    suspension_read: bool,
    /// When a parked power frame last made a read due: at most one per Online
    /// interval (R36).
    last_power_trigger: Option<Instant>,
    /// What `should_start_stream` said at the last `streams_to_stop`, so a car
    /// that stopped being wanted is returned once (R28).
    stream_wanted: bool,
    /// A read answered 408 and its targeted state check has not answered yet
    /// (C3 R46): an `online` answer then makes the 408 a transient failure.
    unavailable_check: bool,
}

impl ScheduledVehicle {
    fn new(vehicle: Vehicle, now: Instant, wall_ms: i64) -> Self {
        let settings = EffectiveSettings::from_car(&vehicle.settings);
        let hold = if settings.enabled {
            VehicleHold::None
        } else {
            VehicleHold::Disabled
        };
        Self {
            vehicle,
            hub_id: None,
            settings,
            phase: PollPhase::Unknown,
            phase_since: (now, wall_ms),
            hold,
            reason: if settings.enabled {
                "collecting"
            } else {
                "vehicle_disabled"
            },
            reported_state: None,
            next_read_due: None,
            next_check_due: None,
            next_probe_due: None,
            suspend_at: None,
            suspended_until: None,
            last_in_use: None,
            last_read: None,
            last_wake_read: None,
            last_success: None,
            offline_from_session: false,
            consecutive_failures: 0,
            first_failure: None,
            last_error: None,
            stream: StreamView::default(),
            missing_lists: 0,
            missing_last: None,
            not_found_check: false,
            lifecycle: None,
            lifecycle_stale: true,
            last_stored_obs_ms: None,
            force_closed_at_gap: false,
            current_discovery: None,
            in_flight: false,
            placeholder: false,
            gated_read: None,
            wake_read_pending: false,
            suspension_read: false,
            last_power_trigger: None,
            stream_wanted: false,
            unavailable_check: false,
        }
    }

    /// The record of a configured car that no list has named yet (C3 R3, R40).
    /// Its identity is the configured Tesla id only: the list supplies the rest
    /// when it names the car, and until then nothing is sent for it.
    fn configured_only(
        id: VehicleId,
        hub_id: Uuid,
        settings: &crate::hub_pack::ProjectionCarSettings,
        now: Instant,
        wall_ms: i64,
    ) -> Option<Self> {
        let stream_id = crate::owner_api::StreamVehicleId::try_from_i64(i64::try_from(id.get()).ok()?)?;
        let mut vehicle = Self::new(
            Vehicle {
                id,
                stream_id,
                vin: String::new(),
                state: "unknown".to_owned(),
                display_name: None,
                settings: settings.clone(),
            },
            now,
            wall_ms,
        );
        vehicle.hub_id = Some(hub_id);
        vehicle.placeholder = true;
        Some(vehicle)
    }

    /// A car the collector neither reads nor stores Tesla's data for (R62, R53); R40's
    /// Hub event, which collects nothing from Tesla, still closes its open drive.
    fn is_inactive(&self) -> bool {
        matches!(self.hold, VehicleHold::Disabled | VehicleHold::Removed)
    }

    /// The Driving cadence's stream test (C3 R28): connected and, while
    /// Driving, a frame within `K_DRIVE_FRAME_SILENCE` (strictly less: the outage
    /// is at the 30 s mark, C3 R28, CT50).
    fn stream_healthy_for_cadence(&self, now: Instant) -> bool {
        self.stream.connected
            && (self.phase != PollPhase::Driving
                || self.stream.last_frame.is_some_and(|frame| {
                    now.saturating_duration_since(frame) < K_DRIVE_FRAME_SILENCE
                }))
    }

    /// The cadence interval of the car's current phase: its read interval, or
    /// its state check interval when it is not read on a timer.
    fn interval(&self, timing: &Timing, now: Instant) -> Duration {
        let phase = self.phase;
        timing
            .read_interval(phase, self.stream_healthy_for_cadence(now))
            .unwrap_or_else(|| timing.state_interval(phase))
    }

    /// Whether the car's phase needs the account list for its state (C3 R16,
    /// R17: Suspended, Asleep, Offline and Unknown; a held car keeps the list
    /// running because its probe waits for a listed `online`, R51, R52).
    fn needs_state_check(&self) -> bool {
        match self.hold {
            VehicleHold::Disabled | VehicleHold::Removed => false,
            // A car no list has named is covered by the inventory list; it asks
            // for no faster state check of its own (R53, R16).
            _ if self.placeholder => false,
            VehicleHold::Service | VehicleHold::Paused(_) => true,
            VehicleHold::None => !self.phase.is_read_phase(),
        }
    }

    /// The earliest instant any of the car's own timers fires.
    fn next_due(&self) -> Option<Instant> {
        [
            self.next_read_due,
            self.next_check_due,
            self.next_probe_due,
            self.suspend_at,
            self.suspended_until,
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

/// The account record (C3 R3). Part K7 adds the month key, counters and
/// reservation.
#[derive(Clone, Debug)]
struct AccountRecord {
    state: AccountState,
    state_reason: &'static str,
    state_since_ms: i64,
    /// While set and in the future no request is sent (R43, R45, R47).
    wait: Option<(Instant, WaitReason)>,
    consecutive_429: u32,
    consecutive_unreachable: u32,
    /// Authentication outcomes in a row that were not a stop (C3 R47); the count
    /// the authentication wait's back-off reads. It is memory only, like every
    /// failure count (R4).
    auth_failures: u32,
    store_failures: u32,
    discovery_failures: u32,
    /// The last successful list.
    last_discovery: Option<Instant>,
    /// A list is not due before this (a failed list's back-off).
    list_floor: Option<Instant>,
    /// A list is wanted at or after this: a Fleet targeted check (R46), an
    /// owner retry (R65), a re-enabled car (R62).
    list_wanted: Option<Instant>,
    /// The next list probe of a billing suspension (R50): the only owner-action
    /// state that is probed.
    next_billing_probe: Option<Instant>,
    /// What the running tick left as the probe's time: the re-arm made when the probe
    /// list was admitted, or the owed time of a probe whose resend was withheld (B2
    /// apply, R3C-03). A failed tick commit returns the account to its baseline and
    /// then puts this back, as it keeps a credential stop: the probe went out and was
    /// counted, and a store fault after the answer must not make it due again at once.
    /// Memory only; cleared when a tick begins and when its commit succeeds.
    probe_time_of_the_tick: Option<Instant>,
    reservation_commits: u64,
    telemetry_check_due: bool,
}

impl AccountRecord {
    /// The persisted shape of a collecting account (C3 R4), the scheduler's
    /// starting point before it loads a stored row.
    fn view(&self) -> AccountView {
        AccountView {
            state: self.state,
            reason: self.state_reason,
            since_ms: self.state_since_ms,
            rate_limited_until: None,
            next_probe: self.next_billing_probe,
        }
    }

    fn new(wall_ms: i64) -> Self {
        Self {
            state: AccountState::Collecting,
            state_reason: "collecting",
            state_since_ms: wall_ms,
            wait: None,
            consecutive_429: 0,
            consecutive_unreachable: 0,
            auth_failures: 0,
            store_failures: 0,
            discovery_failures: 0,
            last_discovery: None,
            list_floor: None,
            list_wanted: None,
            next_billing_probe: None,
            probe_time_of_the_tick: None,
            reservation_commits: 0,
            telemetry_check_due: false,
        }
    }
}

/// The retry delay a rate-limited error carries, if it is one (C3 R45).
fn rate_limit_retry_after_seconds(error: &CollectorError) -> Option<u64> {
    match error {
        CollectorError::OwnerApi(OwnerApiError::RateLimited {
            retry_after_seconds,
        })
        | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(OwnerApiError::RateLimited {
            retry_after_seconds,
        }))
        | CollectorError::FleetApi(FleetApiError::RateLimited {
            retry_after_seconds,
        }) => Some(*retry_after_seconds),
        _ => None,
    }
}

/// One transition of the decision log (C3 R9, R10): exactly one INFO event of
/// target `collector.decision` with the car's Hub id (never a Tesla id, VIN or
/// name), the states and the reason, the time to the car's next action when it
/// has one, and whether a gated read caused it (R36 passes `true`). The
/// event never carries a VIN, a Tesla id, a display name, a coordinate, a token,
/// a URL or a response body (R9).
fn log_vehicle_transition(
    vehicle: &ScheduledVehicle,
    from: &'static str,
    to: &'static str,
    reason: &'static str,
    now: Instant,
) {
    log_vehicle_transition_gated(vehicle, from, to, reason, now, false);
}

/// [`log_vehicle_transition`] with the `gated` field of R36 given.
fn log_vehicle_transition_gated(
    vehicle: &ScheduledVehicle,
    from: &'static str,
    to: &'static str,
    reason: &'static str,
    now: Instant,
    gated: bool,
) {
    let hub_id = vehicle
        .hub_id
        .map_or_else(|| "unmapped".to_owned(), |id| id.to_string());
    let next_in_ms = vehicle.next_due().map(|due| {
        u64::try_from(due.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX)
    });
    tracing::info!(
        target: "collector.decision",
        vehicle = %hub_id,
        from,
        to,
        reason,
        next_in_ms,
        gated,
        "collector transition"
    );
}

/// The numeric HTTP status an error carries, if it is an answer of Tesla's (C3
/// R9: the decision log names the status, never the body or a header other than
/// the numeric Retry-After).
fn http_status_of(error: &CollectorError) -> Option<u16> {
    match error {
        CollectorError::OwnerApi(error)
        | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(error)) => match error {
            OwnerApiError::HttpStatus(status) => Some(*status),
            OwnerApiError::RateLimited { .. } => Some(429),
            OwnerApiError::Forbidden { .. } => Some(403),
            OwnerApiError::Gone => Some(410),
            OwnerApiError::VehicleNotFound => Some(404),
            OwnerApiError::VehicleInService => Some(405),
            _ => None,
        },
        CollectorError::FleetApi(error) => error.http_status(),
        _ => None,
    }
}

/// The decision-log line of a failed read (C3 R9, R10). The first failure of a
/// run of failures is a transition and logs at INFO with its class and status;
/// the repeats log at DEBUG, so a lasting fault emits one event, not one per
/// poll.
fn log_read_failure(vehicle: &ScheduledVehicle, http_status: Option<u16>, now: Instant) {
    let hub_id = vehicle
        .hub_id
        .map_or_else(|| "unmapped".to_owned(), |id| id.to_string());
    let class = format!("{:?}", vehicle.last_error.unwrap_or(ErrorClass::Transient));
    let next_in_ms = vehicle.next_read_due.map(|due| {
        u64::try_from(due.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX)
    });
    if vehicle.consecutive_failures == 1 {
        tracing::info!(
            target: "collector.decision",
            vehicle = %hub_id,
            from = vehicle.phase.code(),
            to = vehicle.phase.code(),
            reason = "rest_failure",
            next_in_ms,
            gated = false,
            class = %class,
            http_status,
            "collector transition"
        );
    } else {
        tracing::debug!(
            target: "collector.decision",
            vehicle = %hub_id,
            reason = "rest_failure",
            failures = vehicle.consecutive_failures,
            class = %class,
            http_status,
            "collector read failed again"
        );
    }
}

/// Move a car into `new`, applying R22's rule that a phase change applies the
/// new interval from the moment of the change. Returns whether the phase
/// changed. The caller sets any read time that must differ.
fn enter_phase(
    timing: &Timing,
    vehicle: &mut ScheduledVehicle,
    new: PollPhase,
    now: Instant,
    wall_ms: i64,
    reason: &'static str,
) -> bool {
    enter_phase_gated(timing, vehicle, new, now, wall_ms, reason, false)
}

/// [`enter_phase`] with the `gated` field of the decision log (C3 R36): true when
/// a read made due by stream evidence while the car was Suspended caused the
/// change.
fn enter_phase_gated(
    timing: &Timing,
    vehicle: &mut ScheduledVehicle,
    new: PollPhase,
    now: Instant,
    wall_ms: i64,
    reason: &'static str,
    gated: bool,
) -> bool {
    let old = vehicle.phase;
    if old == new {
        return false;
    }
    vehicle.phase = new;
    vehicle.phase_since = (now, wall_ms);
    vehicle.reason = reason;
    if matches!(
        new,
        PollPhase::Asleep | PollPhase::Offline | PollPhase::Suspended
    ) {
        vehicle.next_read_due = None;
    }
    if new != PollPhase::Online {
        vehicle.suspend_at = None;
    }
    if new != PollPhase::Suspended {
        vehicle.suspended_until = None;
    }
    if let Some(due) = vehicle.next_read_due
        && let Some(interval) = timing.read_interval(new, vehicle.stream_healthy_for_cadence(now))
    {
        vehicle.next_read_due = Some(due.min(deadline_after(now, interval)));
    }
    log_vehicle_transition_gated(vehicle, old.code(), new.code(), reason, now, gated);
    true
}

/// Apply Tesla's reported `state` from a list or record to a car (C3 R32, R34 a,
/// table 7.1). Asleep and offline move every phase that table moves; `online`
/// makes a first-contact read due for an Unknown car, and after Asleep or
/// Offline a wake read, a session-return read or a Suspended period (R34 b, e;
/// R35, part K4).
///
/// Returns whether the car's stream power gate must be revoked: the report moved
/// it into Suspended (C3 R35, R36).
fn apply_reported_state(
    timing: &Timing,
    vehicle: &mut ScheduledVehicle,
    state: &str,
    now: Instant,
    wall_ms: i64,
) -> bool {
    let previously_online = vehicle.reported_state.as_deref() == Some("online");
    // A listed change of a paused car's state to `online` is new evidence that the
    // fault may have cleared, so its probe is due at once (C3 R51). The first list
    // of a process knows no earlier state, so a restart makes no probe due early
    // (R7).
    if state == "online"
        && matches!(vehicle.hold, VehicleHold::Paused(reason) if reason != PauseReason::AccessRefused)
        && vehicle
            .reported_state
            .as_deref()
            .is_some_and(|earlier| earlier != "online")
    {
        vehicle.next_probe_due = Some(now);
    }
    vehicle.vehicle.state = state.to_owned();
    vehicle.reported_state = Some(state.to_owned());
    if vehicle.is_inactive() {
        return false;
    }
    match state {
        "asleep" => {
            enter_phase(
                timing,
                vehicle,
                PollPhase::Asleep,
                now,
                wall_ms,
                "reported_asleep",
            );
            // A car reported asleep is read on no ground (R38): any read made due
            // while it was thought awake is withdrawn.
            clear_pending_reads(vehicle);
        }
        "offline" => match vehicle.phase {
            // Table 7.1: an install that reports offline is not changed; R41
            // ends a stale install.
            PollPhase::Updating | PollPhase::Offline => {}
            phase => {
                if matches!(phase, PollPhase::Driving | PollPhase::Charging) {
                    vehicle.offline_from_session = true;
                }
                enter_phase(
                    timing,
                    vehicle,
                    PollPhase::Offline,
                    now,
                    wall_ms,
                    "reported_offline",
                );
                clear_pending_reads(vehicle);
            }
        },
        "online" => {
            // A held car is read by its probe alone (R51, R52), so no first-contact
            // read is made due for it.
            if vehicle.phase == PollPhase::Unknown
                && vehicle.hold == VehicleHold::None
                && !vehicle.in_flight
                && vehicle.next_read_due.is_none()
            {
                // R34 (a), first_contact_read: one read, at the next tick.
                vehicle.next_read_due = Some(now);
                vehicle.reason = "first_contact_read";
                log_vehicle_transition(
                    vehicle,
                    PollPhase::Unknown.code(),
                    PollPhase::Unknown.code(),
                    "first_contact_read",
                    now,
                );
            } else if !previously_online
                && vehicle.hold == VehicleHold::None
                && !vehicle.in_flight
                && matches!(vehicle.phase, PollPhase::Asleep | PollPhase::Offline)
            {
                // R34 (b) wake read and (e) session return, or R35's limit.
                return online_after_sleep(timing, vehicle, now, wall_ms);
            }
        }
        _ => {}
    }
    false
}

// ---------------------------------------------------------------------------
// The scheduler.
// ---------------------------------------------------------------------------

struct VehicleScheduler {
    mode: crate::config::CollectorMode,
    timing: Timing,
    clock: Arc<dyn CollectorClock>,
    jitter: Box<dyn JitterSource + Send>,
    vehicles: BTreeMap<VehicleId, ScheduledVehicle>,
    account: AccountRecord,
    /// When the scheduler was built: the first list is due then.
    created_at: Instant,
    /// The last (monotonic, wall) pair seen by `observe_clock` (R2).
    clock_watch: (Instant, i64),
    /// The request counter, estimate and cap (C3 R24 to R27, part K7): one shared
    /// handle, because the Fleet helpers and the resident actions count through it
    /// too.
    ledger: RequestLedger,
    pending: PendingTick,
    /// The account row as the store holds it (C3 R4, R5): memory differs from it
    /// exactly when a row must be written.
    persisted_account: AccountView,
    /// The vehicle rows as the store holds them; a car without an entry has no
    /// row.
    persisted_vehicles: BTreeMap<VehicleId, VehicleView>,
    /// The gaps the store holds open (C3 R8).
    open_gaps: BTreeMap<GapKey, OpenGap>,
    /// When an account wait episode (429 or unreachable) began, to open its gap
    /// once it has lasted `K_GAP_MIN` (C3 R8).
    gap_episodes: BTreeMap<GapKind, (Instant, i64)>,
    /// This run's configuration fingerprint (C3 R4), written with the account row.
    config_fingerprint: String,
    /// Rows loaded for cars the scheduler has not seen yet; applied when the car
    /// first appears in a list (C3 R7 step 2).
    restored: BTreeMap<VehicleId, RestoredVehicle>,
    /// The state to return to when the tick's transaction fails (C3 R6).
    baseline: Option<TickBaseline>,
    /// Alert gaps the owner has not been told about, as the loop last read them
    /// from the store (C3 R13).
    alert_gaps_unnotified: usize,
    /// The mode delivers a push stream: Legacy always, Provider only with a stream
    /// URL, never Fleet polling (C3 R28, R64). The Owner loop sets it.
    stream_delivery: bool,
    /// The cars whose stream power gate the loop must revoke (C3 R36): the
    /// dispatcher's events and every transition into Suspended. The scheduler only
    /// records them; the loop owns the gates.
    gate_revocations: BTreeSet<VehicleId>,
    /// The run's write guard (B2 fix round, P3b fix 2, R-P3bF2-1): the irreversible
    /// admission-loss fence with the retained admission. Every store mutation the loop
    /// makes through the scheduler (the tick's identities and transaction, the stream
    /// frames, the scheduled inventory) is preceded by it, even when the admission
    /// passes again after the fence was set.
    write_guard: Option<RunWriteGuard>,
}

impl VehicleScheduler {
    /// The Owner-wire constructor the Owner loop calls: Legacy mode, the system
    /// clock and an operating-system seeded jitter (C3 R3a). The loops call
    /// `new_for_mode`; this constructor is the tests'.
    #[cfg(test)]
    fn new(cadence: CollectorCadence, now: Instant) -> Self {
        Self::new_for_mode(cadence, crate::config::CollectorMode::Legacy, now)
    }

    /// The production constructor for a mode (C3 R3a). The Fleet loop passes
    /// `FleetPolling`; the Fleet policy comes from the cadence's configuration.
    fn new_for_mode(
        cadence: CollectorCadence,
        mode: crate::config::CollectorMode,
        now: Instant,
    ) -> Self {
        let fleet = cadence.fleet_policy();
        // A test may build the loops' scheduler on a clock of its own (and its start).
        let (clock, now): (Arc<dyn CollectorClock>, Instant) =
            match supervised_test_scheduler_clock() {
                Some(clock) => {
                    let started = clock.mono_now();
                    (clock, started)
                }
                None => (Arc::new(SystemCollectorClock), now),
            };
        Self::new_with(cadence, mode, fleet, now, clock, Box::new(OsJitter::new()))
    }

    /// The constructor with every seam (C3 R2, R24, R44, R3a). The mode is an
    /// addition to the spec's argument list: the scheduler must know which of
    /// the two cadence tables applies.
    fn new_with(
        cadence: CollectorCadence,
        mode: crate::config::CollectorMode,
        fleet: crate::config::FleetPolicy,
        now: Instant,
        clock: Arc<dyn CollectorClock>,
        jitter: Box<dyn JitterSource + Send>,
    ) -> Self {
        let wall_ms = clock.wall_now_ms().unwrap_or(0);
        let account = AccountRecord::new(wall_ms);
        let persisted_account = account.view();
        let ledger = RequestLedger::new(mode, &fleet, clock.clone(), now);
        Self {
            timing: Timing::new(&cadence, mode, &fleet),
            mode,
            clock,
            jitter,
            vehicles: BTreeMap::new(),
            account,
            created_at: now,
            clock_watch: (now, wall_ms),
            ledger,
            pending: PendingTick::default(),
            persisted_account,
            persisted_vehicles: BTreeMap::new(),
            open_gaps: BTreeMap::new(),
            gap_episodes: BTreeMap::new(),
            config_fingerprint: NO_FINGERPRINT.to_owned(),
            restored: BTreeMap::new(),
            baseline: None,
            alert_gaps_unnotified: 0,
            stream_delivery: mode == crate::config::CollectorMode::Legacy,
            gate_revocations: BTreeSet::new(),
            write_guard: None,
        }
    }

    /// Guard the store writes of this run with its write guard.
    fn guard_writes_with(&mut self, guard: impl Into<RunWriteGuard>) {
        self.write_guard = Some(guard.into());
    }

    /// The run's write guard, if it has one: a scheduler built without one (a test, a
    /// one-shot command) is not a supervised run.
    fn write_guard(&self) -> Option<&RunWriteGuard> {
        self.write_guard.as_ref()
    }

    /// The run's write guard at `point`; nothing is refused without one.
    fn check_write(&self, point: &'static str) -> Result<(), CollectorError> {
        check_optional_write(self.write_guard(), point)
    }

    /// Wall time for stored values; the last wall time seen when the clock
    /// cannot be read.
    fn wall_or_last(&self) -> i64 {
        self.clock.wall_now_ms().unwrap_or(self.clock_watch.1)
    }

    /// Compare the wall and monotonic clocks at a loop iteration (C3 R2):
    /// `drift = (wall_now - wall_prev) - (mono_now - mono_prev)`. A drift above
    /// `K_CLOCK_DIVERGENCE` is a host sleep and records a `host_suspended` gap
    /// from `wall_prev` to `wall_now`; below its negative the wall clock stepped
    /// back, which is logged once and changes nothing else. Reason: on macOS and
    /// Linux the monotonic clock does not advance while the host sleeps, and a
    /// backward step loses no data.
    fn observe_clock(&mut self, now: Instant) -> Option<ClockEvent> {
        let event = self.observe_clock_drift(now);
        // The month follows the clocks in the same iteration (C3 R25), after the
        // watch has seen a divergence and frozen the Fleet month anchor.
        self.refresh_month(now);
        event
    }

    /// The drift test of [`Self::observe_clock`].
    fn observe_clock_drift(&mut self, now: Instant) -> Option<ClockEvent> {
        let wall_ms = self.clock.wall_now_ms().ok()?;
        let (prev_mono, prev_wall) = self.clock_watch;
        self.clock_watch = (now, wall_ms);
        let wall_delta = i128::from(wall_ms) - i128::from(prev_wall);
        let mono_delta = i128::try_from(now.saturating_duration_since(prev_mono).as_millis())
            .unwrap_or(i128::MAX);
        let drift = wall_delta - mono_delta;
        if drift > i128::from(K_CLOCK_DIVERGENCE_MS) {
            self.ledger.freeze_month_anchor();
            let length = Duration::from_millis(u64::try_from(wall_delta).unwrap_or(u64::MAX));
            let alert = self.any_car_awake(None) || length >= self.timing.offline_drive_timeout;
            self.pending.gaps.push(GapRecord {
                kind: GapKind::HostSuspended,
                vehicle: None,
                started_ms: prev_wall,
                ended_ms: Some(wall_ms),
                alert,
            });
            tracing::info!(
                target: "collector.decision",
                vehicle = "account",
                from = "collecting",
                to = "collecting",
                reason = "gap_opened",
                kind = GapKind::HostSuspended.code(),
                "collector transition"
            );
            Some(ClockEvent::HostSuspended {
                from_wall_ms: prev_wall,
                to_wall_ms: wall_ms,
            })
        } else if drift < -i128::from(K_CLOCK_DIVERGENCE_MS) {
            tracing::info!(
                target: "collector.decision",
                vehicle = "account",
                from = "collecting",
                to = "collecting",
                reason = "wall_clock_stepped_back",
                "collector transition"
            );
            Some(ClockEvent::WallClockSteppedBack)
        } else {
            None
        }
    }

    /// Whether any collected car (or the given one) is awake: Online, Driving,
    /// Charging or Updating, or Suspended with Tesla reporting it online (C3 R8,
    /// the alert rule for gaps).
    fn any_car_awake(&self, only: Option<VehicleId>) -> bool {
        self.vehicles
            .iter()
            .filter(|(id, vehicle)| {
                only.is_none_or(|wanted| wanted == **id) && !vehicle.is_inactive()
            })
            .any(|(_, vehicle)| match vehicle.phase {
                PollPhase::Online
                | PollPhase::Driving
                | PollPhase::Charging
                | PollPhase::Updating => true,
                PollPhase::Suspended => vehicle.reported_state.as_deref() == Some("online"),
                PollPhase::Unknown | PollPhase::Asleep | PollPhase::Offline => false,
            })
    }

    /// Whether the account may send a request now: collecting, with no wait in
    /// the future (C3 R22, R43, R45).
    fn requests_allowed(&self, now: Instant) -> bool {
        self.account.state == AccountState::Collecting
            && self.account.wait.is_none_or(|(until, _)| now >= until)
    }

    /// `t`, or the end of the account wait when that is later.
    fn after_wait(&self, due: Instant) -> Instant {
        match self.account.wait {
            Some((until, _)) => due.max(until),
            None => due,
        }
    }

    // ---- Discovery (the account list) ------------------------------------

    /// The interval of the account list now: the shortest state check interval
    /// of any car that needs one, else the inventory interval (C3 R16, R17,
    /// R20). One list covers every car (R12). Never below `K_LOOP_FLOOR`.
    fn list_interval(&self) -> Duration {
        self.vehicles
            .values()
            .filter(|vehicle| vehicle.needs_state_check())
            .map(|vehicle| self.timing.state_interval(vehicle.phase))
            .min()
            .unwrap_or(self.timing.inventory)
            .max(K_LOOP_FLOOR)
    }

    /// When the next list is due, or `None` when none is. In an owner-action
    /// state only a list probe is due (R50: the billing suspension's).
    fn list_due_at(&self) -> Option<Instant> {
        let account = &self.account;
        // A credential stop sends nothing, so no list is due, whatever probe
        // schedule a retained account stop beneath it keeps.
        if account.state == AccountState::CredentialStop {
            return None;
        }
        if account.state != AccountState::Collecting {
            return account
                .next_billing_probe
                .map(|probe| self.after_wait(probe));
        }
        let routine = account.last_discovery.map_or(self.created_at, |last| {
            deadline_after(last, self.list_interval())
        });
        let mut due = routine;
        if let Some(wanted) = account.list_wanted {
            due = due.min(wanted);
        }
        if let Some(floor) = account.list_floor {
            due = due.max(floor);
        }
        Some(self.after_wait(due))
    }

    /// A list is due (C3 R3a): the routine state check or inventory (R16, R17),
    /// a wanted list (Fleet targeted check R46, owner retry R65, a re-enabled
    /// car R62) or a list probe (R50 to R52). False while the account waits.
    fn discovery_due(&self, now: Instant) -> bool {
        self.list_due_at().is_some_and(|due| due <= now)
    }

    /// Apply the car-settings rows (C3 R62). Returns the cars whose stream must
    /// be disconnected.
    fn apply_control_settings(
        &mut self,
        configured: &[(Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
        now: Instant,
    ) -> Vec<VehicleId> {
        let wall_ms = self.wall_or_last();
        let mut disconnect = Vec::new();
        let mut targets: HashMap<VehicleId, (Uuid, &crate::hub_pack::ProjectionCarSettings)> =
            HashMap::new();
        for (hub_id, tesla_id, settings) in configured {
            if let Some(id) = VehicleId::try_from_i64(*tesla_id) {
                targets.insert(id, (*hub_id, settings));
            }
        }
        // Every configured car has a record from the first settings refresh, so
        // the lifecycle check, the restored holds and the status work in every
        // account state, with or without a list (R3, R7 step 6, R40). A stored
        // hold is restored now; a stored suspension waits for the list that names
        // the car, because its read needs the car's identity (R7 step 2).
        for (id, (hub_id, settings)) in &targets {
            if self.vehicles.contains_key(id) {
                continue;
            }
            let Some(mut vehicle) =
                ScheduledVehicle::configured_only(*id, *hub_id, settings, now, wall_ms)
            else {
                continue;
            };
            if let Some(restored) = self.restored.get(id)
                && matches!(restored.row.hold.as_str(), "removed" | "paused" | "service")
            {
                Self::apply_restored(&mut vehicle, restored, now, wall_ms);
            }
            self.vehicles.insert(*id, vehicle);
        }
        // A car no longer configured is dropped and its stream disconnected.
        let dropped: Vec<VehicleId> = self
            .vehicles
            .keys()
            .copied()
            .filter(|id| !targets.contains_key(id))
            .collect();
        for id in dropped {
            if let Some(vehicle) = self.vehicles.remove(&id) {
                log_vehicle_transition(
                    &vehicle,
                    vehicle.phase.code(),
                    "removed",
                    "settings_applied",
                    now,
                );
            }
            disconnect.push(id);
        }
        let timing = self.timing;
        let mut list_wanted = None;
        for (id, vehicle) in &mut self.vehicles {
            let Some((hub_id, settings)) = targets.get(id) else {
                continue;
            };
            vehicle.hub_id = Some(*hub_id);
            let old = vehicle.vehicle.settings.clone();
            if old == **settings {
                continue;
            }
            vehicle.vehicle.settings = (*settings).clone();
            vehicle.settings = EffectiveSettings::from_car(settings);
            let mut logged = false;
            if old.enabled && !settings.enabled {
                vehicle.hold = VehicleHold::Disabled;
                vehicle.next_read_due = None;
                vehicle.next_check_due = None;
                vehicle.next_probe_due = None;
                vehicle.suspend_at = None;
                vehicle.suspended_until = None;
                vehicle.reason = "vehicle_disabled";
                log_vehicle_transition(
                    vehicle,
                    vehicle.phase.code(),
                    "disabled",
                    "vehicle_disabled",
                    now,
                );
                if !disconnect.contains(id) {
                    disconnect.push(*id);
                }
                logged = true;
            } else if !old.enabled && settings.enabled {
                if vehicle.hold == VehicleHold::Disabled {
                    vehicle.hold = VehicleHold::None;
                    vehicle.next_read_due = None;
                    enter_phase(
                        &timing,
                        vehicle,
                        PollPhase::Unknown,
                        now,
                        wall_ms,
                        "vehicle_enabled",
                    );
                    // The stored Tesla state is stale after a period of being
                    // disabled; ask for a fresh list at once.
                    list_wanted = Some(now);
                }
                logged = true;
            }
            if old.use_streaming_api && !settings.use_streaming_api && !disconnect.contains(id) {
                disconnect.push(*id);
            }
            if old.suspend_after_idle_min != settings.suspend_after_idle_min
                && vehicle.suspend_at.is_some()
                && let Some(anchor) = vehicle.last_in_use
            {
                vehicle.suspend_at =
                    Some(deadline_after(anchor, vehicle.settings.suspend_after_idle));
            }
            if old.suspend_min != settings.suspend_min && vehicle.phase == PollPhase::Suspended {
                let until = deadline_after(vehicle.phase_since.0, vehicle.settings.suspend);
                vehicle.suspended_until = Some(until.max(now));
            }
            if !logged {
                log_vehicle_transition(
                    vehicle,
                    vehicle.phase.code(),
                    vehicle.phase.code(),
                    "settings_applied",
                    now,
                );
            }
        }
        if let Some(wanted) = list_wanted {
            self.account.list_wanted = Some(wanted);
        }
        disconnect
    }

    /// Accept a successful list (C3 R3a): update the cars' phases from Tesla's
    /// states and return the discovery candidates, the cars of the list that are
    /// neither Disabled nor Removed, with Tesla's state as received. A car the list
    /// names resets its absences and returns from Removed (R53); a car it does not
    /// name counts an absence; a car a pause or service hold keeps held changes
    /// phase only through its probe (R51, R52); a successful list is the answer
    /// that ends a billing suspension (R50).
    fn accept_discovery(&mut self, vehicles: Vec<Vehicle>, now: Instant) -> Vec<Vehicle> {
        let wall_ms = self.wall_or_last();
        let timing = self.timing;
        let mut candidates = Vec::new();
        let mut revoke = Vec::new();
        let mut unavailable_online = Vec::new();
        let mut present = BTreeSet::new();
        for incoming in vehicles {
            let id = incoming.id;
            present.insert(id);
            let state = incoming.state.clone();
            // The stored row stays until a commit has made it the store's own
            // again (R6, R7): a failed tick returns the car to the state before
            // it and the next list restores the row once more.
            let restored = self.restored.get(&id).cloned();
            let vehicle = self.vehicles.entry(id).or_insert_with(|| {
                let mut vehicle = ScheduledVehicle::new(incoming.clone(), now, wall_ms);
                if let Some(restored) = &restored {
                    Self::apply_restored(&mut vehicle, restored, now, wall_ms);
                }
                vehicle
            });
            if vehicle.placeholder {
                // The first list that names a configured car: its stored state
                // comes back in full, and it is a listed car from now on.
                vehicle.placeholder = false;
                if let Some(restored) = &restored {
                    Self::apply_restored(vehicle, restored, now, wall_ms);
                }
            }
            // The settings of a known car change only through
            // `apply_control_settings`, so one policy has one source (R61).
            let settings = vehicle.vehicle.settings.clone();
            vehicle.vehicle = incoming;
            vehicle.vehicle.settings = settings;
            Self::note_listed(vehicle, now, wall_ms);
            if apply_reported_state(&timing, vehicle, &state, now, wall_ms) {
                revoke.push(id);
            }
            if Self::settle_unavailable(vehicle, &state) {
                unavailable_online.push(id);
            }
            if !vehicle.is_inactive() {
                candidates.push(vehicle.vehicle.clone());
            }
        }
        self.gate_revocations.extend(revoke);
        // A car whose 408 the list did not explain failed transiently (R46).
        for id in unavailable_online {
            self.count_vehicle_failure(id, ErrorClass::Transient, Some(408), now);
        }
        self.note_unlisted(&present, now);
        self.note_success(now);
        self.resume_from_billing();
        self.account.last_discovery = Some(now);
        self.account.list_floor = None;
        self.account.list_wanted = None;
        self.account.discovery_failures = 0;
        candidates
    }

    /// Accept one Owner-wire record (C3 R3a): the same as a list for one car,
    /// with zero or one candidate. The list's own schedule is untouched.
    fn accept_vehicle_state(
        &mut self,
        vehicle_id: VehicleId,
        state: String,
        now: Instant,
    ) -> Vec<Vehicle> {
        let wall_ms = self.wall_or_last();
        let timing = self.timing;
        let Some(vehicle) = self.vehicles.get_mut(&vehicle_id) else {
            return Vec::new();
        };
        vehicle.next_check_due = None;
        Self::note_listed(vehicle, now, wall_ms);
        let revoke = apply_reported_state(&timing, vehicle, &state, now, wall_ms);
        let unavailable_online = Self::settle_unavailable(vehicle, &state);
        let candidates = if vehicle.is_inactive() {
            Vec::new()
        } else {
            vec![vehicle.vehicle.clone()]
        };
        if revoke {
            self.gate_revocations.insert(vehicle_id);
        }
        if unavailable_online {
            self.count_vehicle_failure(vehicle_id, ErrorClass::Transient, Some(408), now);
        }
        self.note_success(now);
        candidates
    }

    /// A list failed with a failure that the ordinary back-off answers (C3 R57,
    /// the `Transient`, `Protocol` and `ClientError` rows): the next one waits
    /// `max(interval, back-off)`. The classes with a wait of their own are in
    /// `discovery_failed_for_error`. Returns the delay.
    fn discovery_failed(&mut self, now: Instant) -> Duration {
        self.account.discovery_failures = self.account.discovery_failures.saturating_add(1);
        let draw = self.jitter.draw();
        let delay = self.list_interval().max(backoff_delay(
            self.account.discovery_failures,
            self.timing.max_backoff,
            draw,
        ));
        self.account.list_floor = Some(deadline_after(now, delay));
        delay
    }

    // ---- Reads -------------------------------------------------------------

    /// The cars whose `vehicle_data` read is due (C3 R22, R12): hold `None`,
    /// the account collecting with no wait in the future, `next_read_due <=
    /// now`, not already in flight, ordered by due instant then id, at most
    /// `K_MAX_IN_FLIGHT` in flight in all. They are marked in flight until
    /// `vehicle_succeeded` or `vehicle_failed_for_error` reports them.
    fn due_vehicles(&mut self, now: Instant) -> Vec<VehicleId> {
        // The timers that change a car's phase need no request, so they run
        // whether or not the account may send one (C3 R31, R33, R41).
        self.advance_timers(now);
        if !self.requests_allowed(now) {
            return Vec::new();
        }
        let in_flight = self
            .vehicles
            .values()
            .filter(|vehicle| vehicle.in_flight)
            .count();
        let capacity = K_MAX_IN_FLIGHT.saturating_sub(in_flight);
        let mut due: Vec<(Instant, VehicleId)> = self
            .vehicles
            .iter()
            .filter(|(_, vehicle)| vehicle.hold == VehicleHold::None && !vehicle.in_flight)
            .filter_map(|(id, vehicle)| {
                vehicle
                    .next_read_due
                    .filter(|due| *due <= now)
                    .map(|due| (due, *id))
            })
            .collect();
        due.sort();
        due.truncate(capacity);
        for (_, id) in &due {
            if let Some(vehicle) = self.vehicles.get_mut(id) {
                vehicle.in_flight = true;
            }
        }
        due.into_iter().map(|(_, id)| id).collect()
    }

    /// A tick ended before every one of its requests resolved (an account stop
    /// dropped the rest, C3 R47): no request of this tick is in flight any more.
    /// Every car's mark is cleared, so a car whose read was dropped unsent, or
    /// whose own terminal answer ended the tick, is still due and is not hidden
    /// from `due_vehicles` and `due_probe_vehicles` after the stop is lifted. The
    /// cars' due times and failure counts are untouched: an unsent request is
    /// neither a success nor a failure (C3 R12, R47).
    fn release_in_flight(&mut self) {
        for vehicle in self.vehicles.values_mut() {
            vehicle.in_flight = false;
        }
    }

    /// A read succeeded (C3 R22, R30, R31, R33, R35, table 7.1): the phase of its
    /// snapshot applies and the next read is due one interval of that phase later.
    /// Always `None`: the Hub writes no discovery for its own decisions (R14).
    ///
    /// What follows from the snapshot's use evidence (R29, R30):
    /// - a session phase (Driving, Charging, Updating) is the new phase, and the
    ///   idle window is cleared;
    /// - Online with use evidence is Online with the idle window cleared (R31);
    /// - Online without use evidence, from a Suspended car (the read of R33 or a
    ///   gated read of R36), is a new suspension period (R31, table 7.1);
    /// - Online without use evidence, otherwise, starts the idle window at
    ///   `(last_in_use or now) + suspend_after_idle_min` unless it is already
    ///   running, except after a wake read, where the car goes to sleep at once
    ///   when a stream is healthy (its frames reveal use without reads) and after
    ///   `K_WAKE_WATCH` otherwise.
    fn vehicle_succeeded(
        &mut self,
        id: VehicleId,
        phase: PollPhase,
        sleep_eligible: bool,
        now: Instant,
    ) -> Option<Vehicle> {
        let wall_ms = self.wall_or_last();
        let timing = self.timing;
        self.note_success(now);
        let vehicle = self.vehicles.get_mut(&id)?;
        let previous = vehicle.phase;
        let gated = vehicle.gated_read.take().is_some();
        let wake_read = std::mem::take(&mut vehicle.wake_read_pending);
        vehicle.suspension_read = false;
        vehicle.in_flight = false;
        vehicle.last_read = Some(now);
        vehicle.last_success = Some((now, wall_ms));
        vehicle.consecutive_failures = 0;
        vehicle.first_failure = None;
        vehicle.last_error = None;
        vehicle.unavailable_check = false;
        vehicle.offline_from_session = false;
        if !sleep_eligible {
            // R30: the car showed use.
            vehicle.last_in_use = Some(now);
        }
        let mut revoke = false;
        let mut suspend_again = false;
        if phase.is_session() {
            let reason = if gated {
                "use_evidence_stream"
            } else {
                "use_evidence_snapshot"
            };
            enter_phase_gated(&timing, vehicle, phase, now, wall_ms, reason, gated);
        } else if !sleep_eligible {
            let reason = if gated {
                "use_evidence_stream"
            } else if previous == PollPhase::Suspended {
                "use_evidence_snapshot"
            } else if wake_read {
                "wake_read"
            } else {
                "reported_online"
            };
            enter_phase_gated(&timing, vehicle, phase, now, wall_ms, reason, gated);
            vehicle.suspend_at = None;
        } else if previous == PollPhase::Suspended {
            suspend_again = true;
        } else {
            let reason = if wake_read {
                "wake_read"
            } else {
                "reported_online"
            };
            enter_phase(&timing, vehicle, phase, now, wall_ms, reason);
            if wake_read {
                vehicle.suspend_at = Some(if vehicle.stream.connected {
                    now
                } else {
                    deadline_after(now, K_WAKE_WATCH)
                });
            } else if vehicle.suspend_at.is_none() {
                let anchor = vehicle.last_in_use.unwrap_or(now);
                vehicle.suspend_at =
                    Some(deadline_after(anchor, vehicle.settings.suspend_after_idle));
                log_idle_window(vehicle, now);
            }
        }
        if let Some(interval) =
            timing.read_interval(vehicle.phase, vehicle.stream_healthy_for_cadence(now))
        {
            vehicle.next_read_due = Some(deadline_after(now, interval));
        }
        if suspend_again {
            let until = deadline_after(now, vehicle.settings.suspend);
            begin_suspension(
                &timing,
                vehicle,
                now,
                wall_ms,
                until,
                "suspended_idle",
                gated,
            );
            revoke = true;
        } else if vehicle.hold == VehicleHold::None
            && vehicle.phase == PollPhase::Online
            && vehicle.suspend_at.is_some_and(|at| at <= now)
        {
            let until = deadline_after(now, vehicle.settings.suspend);
            begin_suspension(
                &timing,
                vehicle,
                now,
                wall_ms,
                until,
                "suspended_idle",
                false,
            );
            revoke = true;
        }
        if revoke {
            self.gate_revocations.insert(id);
        }
        None
    }

    /// The read time an evidence-triggered read gets (C3 R22): not before now,
    /// and never faster than `K_MIN_READ_SPACING` after the last read.
    fn evidence_read_time(timing: &Timing, vehicle: &ScheduledVehicle, now: Instant) -> Instant {
        vehicle.last_read.map_or(now, |last| {
            now.max(deadline_after(last, timing.min_read_spacing))
        })
    }

    // ---- Owner-wire targeted checks ----------------------------------------

    /// A stream `VehicleOffline` event wants a state check now (C3 R36, R46):
    /// a record request on the Owner wire, the next list in Fleet polling.
    fn schedule_offline_state_fetch(&mut self, id: VehicleId, now: Instant) {
        let owner_wire = self.mode.is_owner_wire();
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return;
        };
        if vehicle.is_inactive() {
            return;
        }
        if owner_wire {
            vehicle.next_check_due = Some(now);
        } else {
            self.account.list_wanted = Some(now);
        }
    }

    /// The cars whose targeted record check is due (Owner wire); always empty in
    /// Fleet polling, where targeted checks are lists (C3 R3a, R12, R46).
    fn due_offline_state_vehicles(&mut self, now: Instant) -> Vec<VehicleId> {
        if !self.mode.is_owner_wire() || !self.requests_allowed(now) {
            return Vec::new();
        }
        let mut due: Vec<(Instant, VehicleId)> = self
            .vehicles
            .iter()
            .filter(|(_, vehicle)| !vehicle.is_inactive())
            .filter_map(|(id, vehicle)| {
                vehicle
                    .next_check_due
                    .filter(|due| *due <= now)
                    .map(|due| (due, *id))
            })
            .collect();
        due.sort();
        due.truncate(K_MAX_IN_FLIGHT);
        due.into_iter().map(|(_, id)| id).collect()
    }

    // ---- Streams -------------------------------------------------------------

    /// The stream is healthy (C3 R28, R10): it is connected with a frame now. A run
    /// that is healthy has proved itself, so the run spacing restarts at its first
    /// step (C4 R56 a2) and an audit failure clears (C4 R56 e). The first healthy
    /// event after an outage reports the recovery, once.
    fn stream_healthy(&mut self, id: VehicleId, now: Instant) -> StreamRecovery {
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return StreamRecovery::Unchanged;
        };
        vehicle.stream.connected = true;
        vehicle.stream.last_frame = Some(now);
        vehicle.stream.task_failures = 0;
        let audit_cleared = std::mem::take(&mut vehicle.stream.audit_failed);
        if !vehicle.stream.outage_reported {
            if audit_cleared {
                vehicle.reason = "stream_recovered";
            }
            return StreamRecovery::Unchanged;
        }
        let status = StreamRecoveryStatus {
            failures: vehicle.stream.outage_failures,
            outage_duration: vehicle
                .stream
                .outage_started
                .map_or(Duration::ZERO, |started| {
                    now.saturating_duration_since(started)
                }),
        };
        vehicle.stream.outage_reported = false;
        vehicle.stream.outage_started = None;
        vehicle.stream.outage_failures = 0;
        vehicle.reason = "stream_recovered";
        StreamRecovery::Recovered(status)
    }

    /// The stream is unhealthy (C3 R28, R10): `Active` is returned only on the
    /// first unhealthy event of an outage. A Driving car then falls back to the
    /// fast read interval, from the moment of the outage.
    fn stream_unhealthy(&mut self, id: VehicleId, now: Instant) -> StreamOutage {
        let timing = self.timing;
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return StreamOutage::Ignored;
        };
        vehicle.stream.connected = false;
        vehicle.stream.outage_failures = vehicle.stream.outage_failures.saturating_add(1);
        if vehicle.stream.outage_reported {
            return StreamOutage::Ignored;
        }
        vehicle.stream.outage_reported = true;
        vehicle.stream.outage_started = Some(now);
        vehicle.reason = "stream_outage";
        let fallback = vehicle.phase == PollPhase::Driving && vehicle.hold == VehicleHold::None;
        if fallback && let Some(due) = vehicle.next_read_due {
            // The fallback interval counts from the last read, and a read is
            // never due in the past.
            let fallback_due = vehicle
                .last_read
                .map_or(now, |last| now.max(deadline_after(last, timing.driving)));
            vehicle.next_read_due = Some(due.min(fallback_due));
        }
        let gated = vehicle.gated_read.is_some() && vehicle.phase == PollPhase::Suspended;
        StreamOutage::Active(StreamOutageStatus {
            consecutive_failures: vehicle.stream.outage_failures,
            outage_duration: Duration::ZERO,
            owner_api_fallback_scheduled: fallback,
            owner_api_fallback_after: fallback.then_some(timing.driving),
            suspended: vehicle.phase == PollPhase::Suspended,
            live_power_gate: gated,
            phase: vehicle.phase,
        })
    }

    /// A Driving car in a stream outage has a read due (C3 R28), which the loop
    /// must not starve behind a stream backlog.
    fn has_due_stream_fallback(&mut self, now: Instant) -> bool {
        self.vehicles.values().any(|vehicle| {
            vehicle.hold == VehicleHold::None
                && vehicle.phase == PollPhase::Driving
                && !vehicle.in_flight
                && (vehicle.stream.outage_reported
                    || (vehicle.stream.connected && !vehicle.stream_healthy_for_cadence(now)))
                && vehicle.next_read_due.is_some_and(|due| due <= now)
        })
    }

    /// Whether the frame is stored (C3 R36): every frame of a car that is
    /// scheduled and neither Disabled nor Removed, whatever its other holds. A
    /// frame also shows the stream healthy.
    fn should_persist_stream_telemetry(
        &mut self,
        id: VehicleId,
        power: Option<i64>,
        now: Instant,
    ) -> bool {
        let _ = power;
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return false;
        };
        vehicle.stream.connected = true;
        vehicle.stream.last_frame = Some(now);
        !vehicle.is_inactive()
    }

    /// Whether the frame of a run that is being stopped is stored (C3 R36): the same
    /// rule as [`Self::should_persist_stream_telemetry`], without showing the stream
    /// healthy. `streams_to_stop` has just cleared the car's stream health, and a
    /// frame the stopped run left queued says nothing about a stream that is over
    /// (B2 round 3, C4R3-N2).
    fn should_persist_stopping_stream_telemetry(&self, id: VehicleId) -> bool {
        self.vehicles
            .get(&id)
            .is_some_and(|vehicle| !vehicle.is_inactive())
    }

    // ---- Lifecycle views (C3 R14, R40) --------------------------------------

    /// The collector committed an observation for the car: its lifecycle view is
    /// stale (C3 R14).
    fn lifecycle_changed(&mut self, id: VehicleId) {
        if let Some(vehicle) = self.vehicles.get_mut(&id) {
            vehicle.lifecycle_stale = true;
        }
    }

    /// The cars whose lifecycle view is missing or stale and must be read
    /// before the next use (C3 R14, R40). The loops use `lifecycle_views_for_no_data`.
    #[cfg(test)]
    fn lifecycle_views_needed(&self) -> Vec<VehicleId> {
        self.vehicles
            .iter()
            .filter(|(_, vehicle)| {
                !vehicle.is_inactive() && (vehicle.lifecycle.is_none() || vehicle.lifecycle_stale)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Store a freshly read lifecycle view and the car's latest stored
    /// observation time (C3 R14, R40).
    fn set_lifecycle_view(
        &mut self,
        id: VehicleId,
        view: LifecycleView,
        last_stored_obs_ms: Option<i64>,
    ) {
        if let Some(vehicle) = self.vehicles.get_mut(&id) {
            vehicle.lifecycle = Some(view);
            vehicle.lifecycle_stale = false;
            vehicle.last_stored_obs_ms = last_stored_obs_ms;
        }
    }

    /// The cars with an open drive and no stored observation for longer than
    /// `T_gap`, whose `no_data` event is due (C3 R40). Pure; the loop that
    /// appends the event calls `note_force_closed`.
    fn due_lifecycle_events(&self, wall_now_ms: i64) -> Vec<VehicleId> {
        self.vehicles
            .iter()
            .filter(|(_, vehicle)| {
                self.no_data_deadline_ms(vehicle)
                    .is_some_and(|due| wall_now_ms > due)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// The wall time after which a `no_data` event is due for a car (C3 R40), if
    /// it has an open drive and none has been appended since its last stored
    /// observation.
    ///
    /// R40 applies to a car whatever its phase or hold, a Disabled or Removed car
    /// included (B2 clarification, 2026-10-12): R62's reason is about collecting from
    /// Tesla, and the event collects nothing. It is the Hub's own record at the Hub's
    /// own time, it needs no request and carries no position or Tesla id, and the
    /// owner's stop leaves no evidence either, exactly like an account-scope stop.
    fn no_data_deadline_ms(&self, vehicle: &ScheduledVehicle) -> Option<i64> {
        if vehicle.force_closed_at_gap
            || !vehicle
                .lifecycle
                .as_ref()
                .is_some_and(|view| view.drive_open)
        {
            return None;
        }
        let gap_ms =
            i64::try_from(self.timing.offline_drive_timeout.as_millis()).unwrap_or(i64::MAX);
        vehicle
            .last_stored_obs_ms
            .map(|stored| stored.saturating_add(gap_ms))
    }

    /// The loop appended the car's `no_data` event (C3 R40): its view is stale
    /// and no second event is due until a later observation.
    fn note_force_closed(&mut self, id: VehicleId) {
        let now = self.clock.mono_now();
        if let Some(vehicle) = self.vehicles.get_mut(&id) {
            vehicle.force_closed_at_gap = true;
            vehicle.lifecycle_stale = true;
            // One decision event (R9, R10); the car's phase is unchanged, and the
            // event itself is the stored record.
            log_vehicle_transition(
                vehicle,
                vehicle.phase.code(),
                vehicle.phase.code(),
                "force_close_no_evidence",
                now,
            );
        }
    }

    // ---- Ticks, owner actions and status ------------------------------------

    /// The in-process status (C3 R13): the month's counts per category in every
    /// mode, and the estimate in Fleet polling (R25, R26).
    fn status(&self, now: Instant) -> CollectorStatus {
        let wall_ms = self.wall_or_last();
        let ledger = self.ledger_view();
        let remaining_ms = |until: Instant| {
            i64::try_from(until.saturating_duration_since(now).as_millis()).unwrap_or(i64::MAX)
        };
        CollectorStatus {
            mode: self.mode,
            account_state: self.account.state,
            account_reason: self.account.state_reason,
            account_text: self.account.state.plain_words(),
            account_since_ms: self.account.state_since_ms,
            wait: self
                .account
                .wait
                .filter(|(until, _)| *until > now)
                .map(|(until, reason)| (reason, wall_ms.saturating_add(remaining_ms(until)))),
            month: ledger.month,
            requests: ledger.requests,
            estimate: ledger.estimate,
            vehicles: self
                .vehicles
                .iter()
                .map(|(id, vehicle)| VehicleStatus {
                    hub_id: vehicle.hub_id,
                    vehicle_id: *id,
                    phase: vehicle.phase,
                    hold: vehicle.hold,
                    reason: vehicle.shown_reason(),
                    since_ms: vehicle.phase_since.1,
                    next_in_ms: vehicle.next_due().map(|due| {
                        u64::try_from(due.saturating_duration_since(now).as_millis())
                            .unwrap_or(u64::MAX)
                    }),
                    last_success_ms: vehicle.last_success.map(|(_, wall)| wall),
                    last_error: vehicle.last_error,
                    detail: vehicle.status_detail(now),
                })
                .collect(),
            open_gaps: self.open_gaps.len(),
            alert_gaps_unnotified: self.alert_gaps_unnotified,
            reservation_commits: self.account.reservation_commits,
            telemetry_check_due: self.account.telemetry_check_due,
        }
    }

    /// The configured, scheduled cars (C3 R3a), in id order, with Tesla's state
    /// as last received.
    fn vehicles(&self) -> Vec<Vehicle> {
        self.vehicles
            .values()
            .filter(|vehicle| !vehicle.placeholder)
            .map(|vehicle| vehicle.vehicle.clone())
            .collect()
    }

    // ---- Waiting (C3 R11) -----------------------------------------------------

    /// The time until the scheduler next has something to do (C3 R11): the
    /// minimum over the cars and the account of every pending due instant (read,
    /// targeted check, probe, list, suspension timers, a stream's restart time,
    /// the R40 deadline of an open drive, the end of the account wait),
    /// saturating at zero and at most `K_MAX_IDLE_WAIT`. A car's request is not
    /// due before the account wait ends, so the result is at least the
    /// remaining wait whenever the only work is blocked by it.
    fn delay_until_next_action(&self, now: Instant) -> Duration {
        let mut earliest: Option<Instant> = None;
        let mut consider = |due: Instant| {
            earliest = Some(earliest.map_or(due, |current| current.min(due)));
        };
        // A test forces a due time into the past with nothing behind it (CT55b), so
        // the loop's floor is what keeps it from spinning.
        if supervised_forced_due_in_the_past() {
            consider(now);
        }
        if self.account.state == AccountState::Collecting {
            for vehicle in self.vehicles.values() {
                if vehicle.hold == VehicleHold::None
                    && !vehicle.in_flight
                    && let Some(due) = vehicle.next_read_due
                {
                    consider(self.after_wait(due));
                }
                if !vehicle.is_inactive()
                    && let Some(due) = vehicle.next_check_due
                {
                    consider(self.after_wait(due));
                }
                // A probe is a due time only when it may be sent: a probe read
                // waits for a list that says `online` (the list's own due time is
                // considered below), so a past-due probe of a car that is not
                // listed online must not make the loop spin (R11, R51, R52).
                if let Some(due) = vehicle.probe_due_at(self.mode.is_owner_wire()) {
                    consider(self.after_wait(due));
                }
            }
        }
        for vehicle in self.vehicles.values() {
            if !vehicle.is_inactive() {
                // Timers that change the phase need no request. A held car is not
                // timed (R51, R52), so a stale timer of one is no due time.
                if vehicle.hold == VehicleHold::None {
                    for due in [vehicle.suspend_at, vehicle.suspended_until]
                        .into_iter()
                        .flatten()
                    {
                        consider(due);
                    }
                }
                // A stream's restart time is a due time only while it is still to
                // come: once it has passed, the loop's next pass starts the stream.
                if let Some(due) = vehicle
                    .stream
                    .restart_not_before
                    .filter(|restart| *restart > now)
                {
                    consider(due);
                }
                // The deadlines R28 and R41 name: a Driving stream that falls
                // silent is an outage at `K_DRIVE_FRAME_SILENCE`, and an
                // install that stays silent for `K_UPDATE_STALE` ends.
                if let Some(due) = vehicle.silence_deadline() {
                    consider(due);
                }
                if let Some(due) = vehicle.update_stale_deadline() {
                    consider(due);
                }
            }
            if let Some(deadline_ms) = self.no_data_deadline_ms(vehicle) {
                let wall_ms = self.wall_or_last();
                // The event is due strictly after the deadline.
                let remaining_ms = deadline_ms.saturating_sub(wall_ms).saturating_add(1).max(0);
                consider(deadline_after(
                    now,
                    Duration::from_millis(remaining_ms.unsigned_abs()),
                ));
            }
        }
        if let Some(due) = self.list_due_at() {
            consider(due);
        }
        // An account wait counts only while it is still to come. A rate-limit
        // wait is kept after it ends (a success does not clear it, R45) and a
        // store wait stays until a commit writes something (R6), so an ended wait
        // must not be a due time: it would be zero for the rest of the process
        // and the loop would run at the floor rate for ever (R11).
        if let Some((until, _)) = self.account.wait.filter(|(until, _)| *until > now) {
            consider(until);
        }
        earliest
            .map_or(K_MAX_IDLE_WAIT, |due| due.saturating_duration_since(now))
            .min(K_MAX_IDLE_WAIT)
    }

    /// How long the supervised loop sleeps before its next iteration (C3 R11):
    /// the earlier of `delay_until_next_action` and the loop's own cap
    /// (`collection_sleep_cap`: the stream drain cap while streams are active,
    /// else the settings refresh). After an iteration that performed no request
    /// and no write the sleep is at least `K_LOOP_FLOOR`, so a due time forced
    /// into the past cannot make the loop spin; after work it may be zero, so
    /// the next tick of a many-car backlog starts at once (R12).
    fn next_wait(
        &self,
        now: Instant,
        has_active_streams: bool,
        previous_iteration_idle: bool,
    ) -> Duration {
        let wait = self
            .delay_until_next_action(now)
            .min(collection_sleep_cap(has_active_streams));
        if previous_iteration_idle {
            wait.max(K_LOOP_FLOOR)
        } else {
            wait
        }
    }
}
