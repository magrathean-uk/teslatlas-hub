// SPDX-License-Identifier: AGPL-3.0-only

//! Pure policy of the legacy stream client (C4 P1): failure classes, back-off
//! schedules and jitter, classification of hello, error and upgrade answers,
//! liveness and data-silence limits, plausibility of a sample time, retry
//! counters, the session outcome and the power gate. Nothing here does I/O or
//! reads a clock except through [`WallClock`] and [`JitterSource`], so every
//! rule is tested with exact values.

use std::{
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    collector::K_STREAM_POWER_KW,
    db::StreamSessionTerminalOutcome,
    lifecycle::{K_EARLIEST_MS, K_SKEW_MS, normalise_enum_text},
};

use super::{CHINA_STREAM_ENDPOINT, GLOBAL_STREAM_ENDPOINT, StreamUpdate};

/// Ping interval when no hello has set one (R29): one small control frame
/// every 15 s is negligible traffic, and three intervals (45 s) detect a dead
/// socket long before C1 closes a drive for lack of evidence (`T_gap`).
const PING_INTERVAL_DEFAULT: Duration = Duration::from_secs(15);

/// Shortest ping interval a hello can set (R29): bounds traffic if a server
/// advertises a tiny idle limit.
const PING_INTERVAL_MIN: Duration = Duration::from_secs(5);

/// Shortest moving data-silence limit (R30): above the longest dead-socket
/// window (45 s), so a dead socket is always classified by the liveness rule
/// first.
const SILENCE_MOVING_MIN: Duration = Duration::from_secs(60);

/// Longest moving data-silence limit and the parked limit `H_park` (R30). A
/// parked car may legitimately send nothing, so a parked limit equal to the
/// moving one would reconnect a silent, parked, awake car about once a minute;
/// 300 s bounds that at 12 reconnects an hour, and a subscription lost during
/// C3's suspension hides a drive start for at most 5 min. It also equals the
/// `Transport` cap, so a silent subscription is treated no more patiently than
/// a failed one.
const SILENCE_PARKED: Duration = Duration::from_secs(300);

/// First `Transport` delay before jitter (R34): a short home-network or server
/// blip during a drive should cost seconds of data, and doubling sheds load
/// fast.
const TRANSPORT_BASE: Duration = Duration::from_secs(2);

/// Cap of the `Transport` schedule (R34): during a long server outage each
/// vehicle makes at most 12 attempts an hour while C3's polling covers the
/// data, and recovery is noticed within 5 min.
const TRANSPORT_CAP: Duration = Duration::from_secs(300);

/// First `Vehicle` delay before jitter (R34): the server has answered, so the
/// path works and fast retries only add load; typical causes (tunnel, garage,
/// sleep) last tens of seconds or more.
const VEHICLE_BASE: Duration = Duration::from_secs(15);

/// Cap of the `Vehicle` schedule (R34): a returning car is re-acquired within
/// 2 min, so a connectivity drop of up to 8 min still falls inside one drive
/// under `T_gap`.
const VEHICLE_CAP: Duration = Duration::from_secs(120);

/// Delay of the first `Refusal` (R34): tests the likely race of a token
/// rotated by another holder between read and subscribe, with a fresh
/// admission.
const REFUSAL_FIRST: Duration = Duration::from_secs(5);

/// Delay of the second and later `Refusal`s, and of the re-check after a
/// terminal credential error (R34, R6a): after a second refusal only a
/// credential change helps, and a 5 min probe notices a repaired credential
/// without loading Tesla's auth; it keeps the credential check at one lock
/// about every 5 min.
const REFUSAL_REPEATED: Duration = Duration::from_secs(300);

/// Longest `Retry-After` the unit honours (R34): the cap stops a wrong header
/// from stopping the stream for days.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(3_600);

/// Longest start jitter of a run (R42): the runs of several vehicles start
/// together when the Hub starts, and 1 s spreads them against any latency
/// need.
const START_JITTER: Duration = Duration::from_secs(1);

/// Value at which every retry counter stops counting (R37). 32 is far past
/// every cap, so saturation never changes a delay.
const COUNTER_SATURATION: u32 = 32;

/// How long a gate confirmation stays valid, in milliseconds (R50). C3 sends
/// an evidence-triggered read at most 10 s after the evidence (C3 R22) plus
/// its drain cap (100 ms, C3 R11), so the bound never refuses a read C3 made
/// due on fresh evidence; frames of a continuing drive keep the gate fresh if
/// they come at least every 20 s; and it is a small fraction of the 15 min the
/// Hub lets a parked car idle before it may sleep (C3 R31).
const GATE_FRESH_MS: i64 = 30_000;

/// The class of the failure that ended a session or an attempt; it picks the
/// retry schedule (R34, table 8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureClass {
    /// Network or server trouble: socket error, end of stream, server Close,
    /// dead socket, data silence, connect failure.
    Transport,
    /// The server asked for patience (429, 503, Close 1013), with its
    /// `Retry-After` when it gave one.
    Busy { retry_after: Option<Duration> },
    /// The server answered but the car is not reachable through it.
    Vehicle,
    /// An authorisation refusal with a public meaning (R27, R31).
    Refusal,
    /// The peer broke the bounded wire protocol (R33).
    Protocol,
    /// Not understood; handled exactly as `Transport`, the safe class (R28).
    Unknown,
}

/// The counters that pick a schedule position (R37, table 8.6). All saturate
/// at 32. A run starts with all three at 0 (R44).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RetryState {
    /// Consecutive `Transport`-schedule ends since the last reset.
    pub(crate) transport_attempt: u32,
    /// Consecutive `Vehicle` ends since the last accepted frame.
    pub(crate) vehicle_attempt: u32,
    /// Consecutive `Refusal`s since the last accepted frame.
    pub(crate) refusal_streak: u32,
}

impl RetryState {
    fn bump(counter: &mut u32) {
        *counter = counter.saturating_add(1).min(COUNTER_SATURATION);
    }

    /// Applies a session end before its delay is computed (R37). A well-formed
    /// error frame or refusal proves the network path and server work, so
    /// transport back-off restarts for `Vehicle` and `Refusal`.
    pub(crate) fn on_session_end(&mut self, class: &FailureClass) {
        match class {
            FailureClass::Transport
            | FailureClass::Busy { .. }
            | FailureClass::Unknown
            | FailureClass::Protocol => Self::bump(&mut self.transport_attempt),
            FailureClass::Vehicle => {
                Self::bump(&mut self.vehicle_attempt);
                self.transport_attempt = 0;
            }
            FailureClass::Refusal => {
                Self::bump(&mut self.refusal_streak);
                self.transport_attempt = 0;
            }
        }
    }

    /// The first accepted frame of a session, a duplicate included (R26, R37):
    /// data proves the car is reachable and the credential accepted.
    pub(crate) fn on_first_accepted(&mut self) {
        self.vehicle_attempt = 0;
        self.refusal_streak = 0;
    }

    /// 60 s after the session's first accepted frame, while it is still open
    /// (R37). Transport back-off resets only after 60 s (four default ping
    /// intervals) of a working session, so a path that fails right after its
    /// first frame keeps backing off instead of retrying at the shortest
    /// delay. Called by the session's read loop when its stable-session timer
    /// fires (`SessionClocks::stable_at`).
    pub(crate) fn on_stable(&mut self) {
        self.transport_attempt = 0;
    }
}

/// `min(base * 2^(n - 1), cap)` for a counter `n`; a counter of 0 counts as 1.
fn doubling(base: Duration, cap: Duration, counter: u32) -> Duration {
    // 2^16 is past every cap over every base, and keeps the shift in range.
    let exponent = counter.max(1).saturating_sub(1).min(16);
    base.saturating_mul(1_u32 << exponent).min(cap)
}

/// The nominal wait before the next attempt, before jitter (R34, table 8.5),
/// for a class and the counters after their update (R37).
pub(crate) fn nominal_delay(class: &FailureClass, state: &RetryState) -> Duration {
    match class {
        FailureClass::Transport
        | FailureClass::Busy { .. }
        | FailureClass::Unknown
        | FailureClass::Protocol => {
            doubling(TRANSPORT_BASE, TRANSPORT_CAP, state.transport_attempt)
        }
        FailureClass::Vehicle => doubling(VEHICLE_BASE, VEHICLE_CAP, state.vehicle_attempt),
        FailureClass::Refusal => {
            if state.refusal_streak <= 1 {
                REFUSAL_FIRST
            } else {
                REFUSAL_REPEATED
            }
        }
    }
}

/// `nominal * (0.5 + 0.5 * u)`, rounded down to whole milliseconds (R35). At
/// least half the nominal wait is always kept, so retries never fall below the
/// schedule's intent, and the upper half spreads vehicles and Hubs that failed
/// together, which is what prevents reconnect storms.
pub(crate) fn jittered(nominal: Duration, u: f64) -> Duration {
    // A draw outside [0, 1) (or NaN) is a fault of the source; the nearest
    // valid draw keeps the delay inside its documented range.
    let u = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
    let millis = (nominal.as_millis() as f64 * (0.5 + 0.5 * u)).floor();
    Duration::from_millis(millis as u64)
}

/// The wait before the next attempt (R34): the jittered nominal delay and, for
/// `Busy`, never less than the server's `Retry-After` capped at 3,600 s. The
/// floor is applied after jitter, so the server's request is always honoured.
pub(crate) fn next_delay(class: &FailureClass, state: &RetryState, u: f64) -> Duration {
    let delay = jittered(nominal_delay(class, state), u);
    match class {
        FailureClass::Busy {
            retry_after: Some(retry_after),
        } => delay.max((*retry_after).min(RETRY_AFTER_CAP)),
        _ => delay,
    }
}

/// The wait before a run's first attempt, `u * 1 s` (R42, table 8.5).
pub(crate) fn start_delay(u: f64) -> Duration {
    let u = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
    Duration::from_millis((START_JITTER.as_millis() as f64 * u).floor() as u64)
}

/// The wait before the next credential check after a terminal credential error,
/// `jittered(300 s, u)` with the counters unchanged (R6a, table 8.5).
pub(crate) fn terminal_credential_delay(u: f64) -> Duration {
    jittered(REFUSAL_REPEATED, u)
}

/// Whether an error type names a car that is not reachable: `^vehicle_[a-z0-9_]+$`.
fn is_vehicle_token(token: &str) -> bool {
    token.strip_prefix("vehicle_").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    })
}

/// Classifies a `data:error` by its `error_type` token only, never by its
/// `value` text (R28, table 8.3).
///
/// A token of the `vehicle_` family is `Vehicle`; anything else, including any
/// other prefix, an empty token or none, is `Unknown`, handled as `Transport`.
/// No public document defines the token values, so only the one structural
/// guess that is harmless if wrong is kept: it changes the retry schedule and
/// makes C3 run one non-waking state check, and claims nothing about the
/// credential. No token is ever `Refusal`: a wrong refusal would feed C5's
/// rejection fuse and could contribute to a sign-out, so refusal evidence comes
/// only from statuses with a public meaning (R27, R31).
pub(crate) fn classify_error(error_type: Option<&str>) -> FailureClass {
    match error_type {
        Some(token) if is_vehicle_token(token) => FailureClass::Vehicle,
        _ => FailureClass::Unknown,
    }
}

/// Classifies a `control:hello` (R27, table 8.3); `None` means success.
///
/// A hello with no `error_type` and a `code` that is absent, 0 or 200 to 299
/// is a success: 0 is the conventional "no error" value of a numeric status
/// that is not an HTTP status, and with no `error_type` the hello makes no
/// failure statement. Otherwise an `error_type` is classified by R28; else
/// 401 and 403 give `Refusal` and 429 and 503 give `Busy` (RFC 9110 sections
/// 15.5.2, 15.5.4 and 15.6.4, RFC 6585 section 4: the closest public meaning
/// for a numeric status), and any other code is not understood and takes the
/// safe class.
pub(crate) fn classify_hello(code: Option<i64>, error_type: Option<&str>) -> Option<FailureClass> {
    if error_type.is_some() {
        return Some(classify_error(error_type));
    }
    match code {
        None | Some(0) | Some(200..=299) => None,
        Some(401 | 403) => Some(FailureClass::Refusal),
        Some(429 | 503) => Some(FailureClass::Busy { retry_after: None }),
        Some(_) => Some(FailureClass::Unknown),
    }
}

/// Whether an endpoint is an override, that is neither of Tesla's two (0.2).
pub(crate) fn is_override_endpoint(endpoint: &str) -> bool {
    endpoint != GLOBAL_STREAM_ENDPOINT && endpoint != CHINA_STREAM_ENDPOINT
}

/// Classifies the HTTP status of a WebSocket upgrade other than 101 (R31,
/// table 8.3). 429 and 503 give `Busy`; the caller fills in `Retry-After` when
/// the response had one. 401 and 403 give `Refusal` on an override endpoint and
/// `Transport` on a Tesla endpoint; any other status gives `Transport`.
///
/// No Tesla credential is sent during the upgrade (R4), so a refusal by
/// Tesla's host says nothing about the credential; on an override the URL may
/// carry a provider key (R59), so a 401 or 403 there is the provider's
/// statement about that key.
pub(crate) fn classify_upgrade(status: u16, override_endpoint: bool) -> FailureClass {
    match status {
        429 | 503 => FailureClass::Busy { retry_after: None },
        401 | 403 if override_endpoint => FailureClass::Refusal,
        _ => FailureClass::Transport,
    }
}

/// The ping interval for a hello's `connection_timeout` in milliseconds (R29):
/// `clamp(connection_timeout / 3, 5 s, 15 s)`, and 15 s when the value is
/// absent or not above 0. A third of the server's advertised idle limit puts at
/// least two pings inside it. The caller adopts the result only for a value
/// above 0 (R27).
pub(crate) fn ping_interval(connection_timeout_ms: Option<i64>) -> Duration {
    match connection_timeout_ms {
        Some(timeout_ms) if timeout_ms > 0 => {
            let third = Duration::from_millis((timeout_ms / 3) as u64);
            third.clamp(PING_INTERVAL_MIN, PING_INTERVAL_DEFAULT)
        }
        _ => PING_INTERVAL_DEFAULT,
    }
}

/// `H_move`: the configured health timeout clamped to [60 s, 300 s] (R30).
/// C3's configuration load is the one site that clamps and logs a value outside
/// that range; this is a silent guard that keeps the ordering of the limits
/// true for any caller that does not go through that load.
pub(crate) fn effective_health_timeout(configured: Duration) -> Duration {
    configured.clamp(SILENCE_MOVING_MIN, SILENCE_PARKED)
}

/// How long a subscribed, live session may receive no data frame before it is
/// ended (R30): `H_move` while moving, `H_park` (300 s) otherwise, including
/// before the run's first accepted frame. While the car moves its position,
/// odometer and speed change continuously, so silence is evidence that the
/// subscription was lost.
pub(crate) fn data_silence_limit(configured: Duration, moving: bool) -> Duration {
    if moving {
        effective_health_timeout(configured)
    } else {
        SILENCE_PARKED
    }
}

/// Whether a sample time is plausible (R20a): not before 2012-01-01T00:00:00Z
/// and not more than 60 s after the wall clock at receipt (C1's constants, so
/// stream intake and the projection apply one rule). The sample time becomes
/// `observed_at_ms`, which moves the storage stream watermark and C1's fact
/// watermarks: one far-future time would make every later frame a duplicate in
/// storage and every later fact stale in C1.
pub(crate) fn plausible_sample_time(t_ms: i64, wall_now_ms: i64) -> bool {
    K_EARLIEST_MS <= t_ms && t_ms <= wall_now_ms.saturating_add(K_SKEW_MS)
}

/// Whether the frame shows motion (0.2, R30, R50): the gear is D, R or N after
/// C1 R40a normalisation, or there is no gear (empty or not one of P, R, N, D)
/// and `speed` is above 0. This is the motion rule of C3 R36; a parked gear
/// shows no motion whatever its speed. A gear text outside P, R, N, D (Tesla's
/// `SNA`, "signal not available", or anything unrecognised) says nothing about
/// the selector, so it counts as no gear and the speed decides: reading it as a
/// gear would make a stationary car with a stale signal look parked or moving at
/// random (B2 clarification of 2026-10-12 23:25, with C3 R36).
pub(crate) fn frame_shows_motion(update: &StreamUpdate) -> bool {
    let gear = update
        .shift_state
        .as_deref()
        .and_then(normalise_enum_text)
        .filter(|name| matches!(*name, "P" | "R" | "N" | "D"));
    match gear {
        Some("D" | "R" | "N") => true,
        Some(_) => false,
        None => update.speed.is_some_and(|speed| speed > 0.0),
    }
}

/// Why a session ended, one variant per row of table 8.4 that decides its
/// outcome (R14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndCause {
    /// The shutdown signal.
    Shutdown,
    /// DNS, TCP or TLS error, the connect limit, or an upgrade status.
    ConnectFailed,
    /// A credential error in Admitting.
    CredentialError,
    /// The subscribe frame could not be written within its limit.
    SubscribeWriteFailed,
    /// A `Vehicle` error.
    Vehicle,
    /// A `Refusal`.
    Refusal,
    /// An `Unknown` error or hello.
    Unknown,
    /// A `Busy` hello or Close 1013.
    Busy,
    /// A socket error, end of stream, server Close or dead socket.
    SocketEnded,
    /// No data frame within the silence limit.
    DataSilence,
    /// Three rejected frames in a row, or an oversize message.
    Protocol,
    /// The event queue stayed full.
    QueueFull,
    /// The event receiver is gone.
    ReceiverClosed,
    /// An audit write failed.
    AuditFailed,
}

/// How the session row is completed (R14, table 8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionOutcome {
    CancelledBeforeSubscription,
    OrderlyShutdown,
    TransportEnded,
    Failed,
}

impl SessionOutcome {
    /// The terminal outcome to store; `None` for the orderly shutdown, which
    /// is completed with its unsubscribe receipt instead.
    pub(crate) fn terminal(self) -> Option<StreamSessionTerminalOutcome> {
        match self {
            Self::CancelledBeforeSubscription => {
                Some(StreamSessionTerminalOutcome::CancelledBeforeSubscription)
            }
            Self::OrderlyShutdown => None,
            Self::TransportEnded => Some(StreamSessionTerminalOutcome::TransportEnded),
            Self::Failed => Some(StreamSessionTerminalOutcome::Failed),
        }
    }
}

/// The session outcome by cause (R14, table 8.4).
///
/// Grouping by cause keeps the audit summary's counters meaningful: orderly
/// ends are intentional stops, transport ends are outside events (data silence
/// is one: the server stopped sending), and failures need attention. A full
/// queue or a closed receiver is a failure inside the Hub that ends the
/// stream, so it is `Failed` even though an unsubscribe is sent. The schema
/// allows an unsubscribe link only for `OrderlyShutdown`.
pub(crate) fn session_outcome(
    subscribed: bool,
    cause: EndCause,
    unsubscribe_ok: bool,
) -> SessionOutcome {
    match cause {
        EndCause::Shutdown if !subscribed => SessionOutcome::CancelledBeforeSubscription,
        EndCause::Shutdown if unsubscribe_ok => SessionOutcome::OrderlyShutdown,
        EndCause::Shutdown => SessionOutcome::TransportEnded,
        EndCause::Vehicle
        | EndCause::Unknown
        | EndCause::Busy
        | EndCause::SocketEnded
        | EndCause::DataSilence
            if subscribed =>
        {
            SessionOutcome::TransportEnded
        }
        _ => SessionOutcome::Failed,
    }
}

/// The source of jitter draws, uniform in [0, 1) (R35): OS-seeded in
/// production, a fixed sequence in tests.
pub(crate) trait JitterSource: Send {
    fn draw(&mut self) -> f64;
}

/// Production jitter: one OS entropy draw per delay.
pub(crate) struct OsJitter;

impl JitterSource for OsJitter {
    fn draw(&mut self) -> f64 {
        let mut bytes = [0_u8; 8];
        let bits = if getrandom::fill(&mut bytes).is_ok() {
            u64::from_le_bytes(bytes)
        } else {
            // Entropy is unavailable only in an exceptionally broken host; the
            // clock's sub-second part still spreads vehicles apart.
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| u64::from(elapsed.subsec_nanos()) << 20)
        };
        // 53 bits fill an f64 mantissa, so the draw is exactly uniform on a
        // grid in [0, 1).
        (bits >> 11) as f64 / (1_u64 << 53) as f64
    }
}

/// The wall clock in epoch milliseconds: `SystemTime` in production, a
/// settable value in tests.
pub(crate) trait WallClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// Production wall clock.
pub(crate) struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
            Err(before) => i64::try_from(before.duration().as_millis()).map_or(i64::MIN, |ms| -ms),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct GateState {
    confirmed: bool,
    confirmed_at_ms: i64,
}

/// Whether the car shows motion or use right now, written by the stream unit
/// and read by C3's gated read (R50, C3 R36).
///
/// The gate exists so that a read made due by stream evidence while the car
/// was suspended is sent only if that evidence still stands (a sleeping vehicle
/// stays asleep). C3's criterion defines "confirmed": a frame that shows
/// motion, or `|power| >= K_STREAM_POWER_KW`. An all-empty frame shows neither
/// and confirms nothing. The stream unit observes at the socket, so no queued
/// `Telemetry` processed after a session ended can re-confirm the gate for
/// that ended session. Freshness uses the wall clock because the monotonic
/// clock does not advance while the host sleeps (C3 R2): a gate confirmed
/// before a host sleep is already stale at the first read after it.
pub(crate) struct StreamPowerGate {
    state: Mutex<GateState>,
    clock: Mutex<Arc<dyn WallClock>>,
}

impl Default for StreamPowerGate {
    fn default() -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            clock: Mutex::new(Arc::new(SystemWallClock)),
        }
    }
}

impl StreamPowerGate {
    /// Confirms the gate when the frame shows motion or `|power|` is at least
    /// `K_STREAM_POWER_KW`, recording `wall_ms`; any other frame leaves the
    /// gate as it is.
    pub(crate) fn observe(&self, update: &StreamUpdate, wall_ms: i64) {
        let power_shows_use = update
            .power
            .is_some_and(|power| power.unsigned_abs() >= K_STREAM_POWER_KW.unsigned_abs());
        if frame_shows_motion(update) || power_shows_use {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            *state = GateState {
                confirmed: true,
                confirmed_at_ms: wall_ms,
            };
        }
    }

    /// Clears the gate.
    pub(crate) fn revoke(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.confirmed = false;
    }

    /// True only when the gate is confirmed and
    /// `0 <= wall_ms - confirmed_at <= 30 s`. A wall clock earlier than the
    /// confirmation (a backward step) reads as not confirmed: fail safe.
    pub(crate) fn is_confirmed_at(&self, wall_ms: i64) -> bool {
        let state = *self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.confirmed {
            return false;
        }
        let age = wall_ms.saturating_sub(state.confirmed_at_ms);
        (0..=GATE_FRESH_MS).contains(&age)
    }

    /// [`Self::is_confirmed_at`] on the wall clock at call time.
    pub(crate) fn is_confirmed(&self) -> bool {
        let clock = self
            .clock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.is_confirmed_at(clock.now_ms())
    }

    /// Sets the wall clock the gate reads; tests only.
    #[cfg(test)]
    pub(crate) fn set_clock(&self, clock: Arc<dyn WallClock>) {
        *self.clock.lock().unwrap_or_else(PoisonError::into_inner) = clock;
    }
}
