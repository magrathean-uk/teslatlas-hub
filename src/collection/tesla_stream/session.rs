// SPDX-License-Identifier: AGPL-3.0-only

//! The supervisor's run (C4 part P2): the attempt loop with its waits, the
//! credential check and admission, connect, subscribe, the accepted-frame path
//! up to `Healthy` and `Telemetry`, the Hub-initiated close, shutdown, run
//! termination, the audit receipts and the events of the ends it handles.
//!
//! One run is a sequence of attempts. Each attempt follows R6: wait (section 4),
//! check credentials without network use, write the audit rows, connect, read
//! the token, subscribe, read until the session ends, close. The pure policy
//! (classes, schedules, counters) is `policy.rs`; the frame grammar is
//! `wire.rs`.

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime},
};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{OwnedMutexGuard, oneshot},
    time::{Instant, sleep, sleep_until, timeout, timeout_at},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
    tungstenite::{
        Error as WebSocketError, Message,
        client::IntoClientRequest,
        error::UrlError,
        handshake::client::Request,
        protocol::frame::{CloseFrame, coding::CloseCode},
    },
};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    credentials::{LegacyAuthManager, LegacyAuthManagerError},
    db::{
        HubStore, OutboundRequestCompletion, OutboundRequestOperation, OutboundRequestOutcome,
        OutboundRequestPrecondition, OutboundRequestReceiptId, OutboundRequestSafetyClass,
        OutboundRequestStart, OutboundRequestTransport, StoreError, StreamSessionReceiptId,
    },
    owner_api::VehicleId,
};

use super::{
    CLOSE_WAIT, K_MAX_MESSAGE, StreamCredential, StreamEvent, StreamPowerGate,
    StreamSupervisorError, TeslaStreamSupervisor, WriteRefusal,
    echo::EchoGuard,
    liveness::SessionClocks,
    policy::{
        EndCause, FailureClass, RetryState, SessionOutcome, classify_error, classify_hello,
        classify_upgrade, data_silence_limit, frame_shows_motion, is_override_endpoint, next_delay,
        ping_interval, plausible_sample_time, session_outcome, start_delay,
        terminal_credential_delay,
    },
    stream_socket_config, unsubscribe_frame,
    wire::{StreamUpdate, parse_data_update_counted, subscribe_frame_zeroizing},
};

// R52: the WebSocket crate logs each frame it reads and writes at trace level
// (the subscribe frame with the access token, every data frame with its values,
// the upgrade request with an override URL's key) through the `log` crate. The
// crate's static level, set in `Cargo.toml`, compiles those records out so no
// environment variable can switch them on; the build fails if the cap is lost.
const _: () = assert!(
    log::STATIC_MAX_LEVEL as usize <= log::LevelFilter::Info as usize,
    "the log crate's static level must stay at info or below (C4 R52)"
);

/// The byte stream a WebSocket runs over: a TCP socket in production. Tests
/// substitute an in-memory pipe through [`TestSeams`], because under paused time
/// the runtime advances the clock whenever it is idle, and a real socket's
/// readiness arrives after the runtime has gone idle, so every armed timer
/// (the 10 s connect limit, for one) would fire spuriously.
pub(crate) trait StreamIo: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> StreamIo for T {}

pub(crate) type Transport = Box<dyn StreamIo>;

type StreamSocket = WebSocketStream<MaybeTlsStream<Transport>>;

/// A test connector: makes the transport of one connection (a failure is a
/// connect error).
#[cfg(test)]
pub(crate) type Connector = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn Future<Output = std::io::Result<Transport>> + Send>>
        + Send
        + Sync,
>;

/// Longest time for DNS, TCP, TLS and the WebSocket upgrade together (R32). A
/// working handshake needs a few round trips (TCP, TLS 1.3, upgrade), about 1 s
/// even on a slow intercontinental path; the platform resolver waits 5 s before
/// retrying a query (resolv.conf(5) default `timeout`), so 10 s allows one
/// resolver retry; a longer wait delays the move to the next attempt without
/// improving success.
const CONNECT_LIMIT: Duration = Duration::from_secs(10);

/// Longest time to write and flush the subscribe frame (R32): the same reasoning
/// as [`CONNECT_LIMIT`], for one small frame on an open socket.
const WRITE_LIMIT: Duration = Duration::from_secs(10);

/// Longest wait to take the shared manager's lock (R6 steps 2 and 5). The
/// longest operation another holder runs under the lock is one token request,
/// bounded at 60 s in total by C5 R33; 30 s more covers its persistence step
/// (C5 R23). A longer wait means a holder is stuck, so the attempt ends and is
/// retried.
const LOCK_WAIT: Duration = Duration::from_secs(90);

/// Longest time to write and flush the unsubscribe frame (R10): many round trips
/// on any working link; with [`CLOSE_WAIT`] it keeps every close under 5 s, the
/// shutdown budget (R11).
const UNSUBSCRIBE_WRITE_LIMIT: Duration = Duration::from_secs(2);

/// Longest wait for room in the event channel (R49). With C3 draining at least
/// every 100 ms (R56 g) only a stalled consumer fills the queue; 10 s is 40
/// times the intake target (250 ms p95), so a slow commit does not trip it,
/// while buffering further would be unbounded. It is below the 15 s minimum
/// liveness window, so a full queue is not misread as a dead socket.
const EVENT_QUEUE_WAIT: Duration = Duration::from_secs(10);

/// Consecutive rejected frames that end a session (R33). One garbled frame
/// should cost one sample, not a reconnect; three in a row is a pattern (a
/// format change, or a clock fault, rejects every frame). The rule counts
/// frames, not seconds, because the frame rate is undocumented.
const INVALID_FRAME_STREAK: u32 = 3;

/// The points of the admission steps (R6 steps 2 and 5) that a test can observe
/// to count calls and order them against peer events (1.2). `LockAcquired` and
/// `RefreshStarted` and `RefreshFinished` belong to step 2 (`RefreshStarted` when
/// `refresh_if_due` is called, whether or not C5 then sends a token request);
/// `TokenRead` belongs to step 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionEvent {
    LockAcquired,
    RefreshStarted,
    RefreshFinished,
    TokenRead,
}

/// The test seams of the supervisor that exist only in test builds (1.2): the
/// admission hook, and a stand-in for a manager whose token read fails with a
/// terminal credential error (R6a) until C5's terminal state exists.
#[derive(Clone, Default)]
pub(super) struct TestSeams {
    #[cfg(test)]
    pub(super) connector: Option<Connector>,
    #[cfg(test)]
    pub(super) hook: Option<Arc<dyn Fn(AdmissionEvent) + Send + Sync>>,
    #[cfg(test)]
    pub(super) terminal: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Where each ended session's counts are also recorded, so a test reads them
    /// without a log capture.
    #[cfg(test)]
    pub(super) summaries: Option<Arc<std::sync::Mutex<Vec<FrameCounts>>>>,
}

impl TestSeams {
    fn notify(&self, event: AdmissionEvent) {
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(event);
        }
        #[cfg(not(test))]
        let _ = event;
    }

    fn terminal_credential(&self) -> bool {
        #[cfg(test)]
        if let Some(flag) = &self.terminal {
            return flag.load(std::sync::atomic::Ordering::SeqCst);
        }
        false
    }
}

/// The shutdown signal (IF a2). Sending and dropping the sender both mean stop.
/// Once it has fired it stays fired, so a later wait returns at once and the
/// receiver is never polled after completion.
pub(super) struct Shutdown {
    receiver: oneshot::Receiver<()>,
    fired: bool,
}

impl Shutdown {
    pub(super) fn new(receiver: oneshot::Receiver<()>) -> Self {
        Self {
            receiver,
            fired: false,
        }
    }

    async fn wait(&mut self) {
        if !self.fired {
            let _ = (&mut self.receiver).await;
            self.fired = true;
        }
    }
}

/// Runs `work` until the shutdown signal; `None` means shutdown came first
/// (R11: the signal interrupts every phase at once). Dropping `work` loses
/// nothing for the futures used here: lock waits, sleeps, socket reads and
/// writes, and a `JoinHandle`, which detaches.
async fn until<T>(shutdown: &mut Shutdown, work: impl Future<Output = T>) -> Option<T> {
    tokio::select! {
        biased;
        () = shutdown.wait() => None,
        value = work => Some(value),
    }
}

/// How a credential step failed (R6a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialFailure {
    /// The authority to use the credential is gone: the run ends.
    SensitiveAccess,
    /// C5 is stopped (`configuration_error`, `signed_out`, `api_refused`): no
    /// new stream connection, no event, a re-check after 300 s.
    Terminal,
    /// Anything else, including the lock wait: class `Transport`.
    Unavailable,
}

/// C5's predicate for a stopped credential (R57 b, R56): an error that sets
/// `configuration_error`, `signed_out` or `api_refused`. `AuthUnavailable` is
/// neither: it is a wait, and the attempt is a transport outage (R6a).
fn manager_error_is_terminal(error: &LegacyAuthManagerError) -> bool {
    error.is_terminal_auth_failure()
}

fn credential_failure(error: &LegacyAuthManagerError) -> CredentialFailure {
    if error.is_sensitive_access_failure() {
        CredentialFailure::SensitiveAccess
    } else if manager_error_is_terminal(error) {
        CredentialFailure::Terminal
    } else {
        CredentialFailure::Unavailable
    }
}

enum Check {
    Ready,
    Shutdown,
    Failed(CredentialFailure),
}

enum Admit {
    Token(Zeroizing<String>),
    Failed(CredentialFailure),
}

/// How a wait on an open socket before the subscribe frame ended (R29: the
/// Pings and the liveness clock run from the moment the socket opens).
enum Live<T> {
    /// The awaited work finished.
    Done(T),
    /// The shutdown signal arrived.
    Shutdown,
    /// The socket ended or was found dead, with the class of the end (R31).
    Gone(FailureClass),
    /// A message the Hub reads as the end of the attempt on a socket that is
    /// still usable (C4 R27, R28, R33): `class` and the code the Hub closes with.
    Ended {
        class: FailureClass,
        close_code: CloseCode,
    },
}

enum LockResult {
    Locked(OwnedMutexGuard<LegacyAuthManager>),
    Shutdown,
    TimedOut,
}

/// What one attempt asks of the run loop.
enum Outcome {
    /// The run ends with this result.
    Stop(Result<(), StreamSupervisorError>),
    /// The attempt ended with this failure class: counters, event, delay.
    Failed(FailureClass),
    /// A terminal credential error: no event, counters unchanged, a re-check
    /// after `jittered(300 s, u)` (R6a).
    TerminalCredential,
}

/// The result of putting one event on the channel.
enum Emit {
    Sent,
    Shutdown,
    ReceiverClosed,
    QueueFull,
}

/// State that lives across the attempts of one run (R44: it starts fresh with
/// every run).
struct RunState {
    retry: RetryState,
    /// The last delivered frame, for duplicate suppression (R26); per run only.
    last_delivered: Option<StreamUpdate>,
    /// The last accepted frame of the run shows motion (0.2): it picks the
    /// data-silence limit (R30), also for the first frames of a later session.
    /// Per run only, like the duplicate memory (R44).
    moving: bool,
}

/// Counts of one session's inbound messages, logged once when it ends (R52).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FrameCounts {
    pub(crate) accepted: u32,
    pub(crate) duplicates: u32,
    pub(crate) rejected: u32,
    pub(crate) implausible_time: u32,
    pub(crate) fields_invalid: u32,
    pub(crate) unknown_messages: u32,
    pub(crate) ignored_errors: u32,
}

/// State of one subscribed session.
struct SessionState {
    /// The subscribe receipt, open until the session's first answer (8.4).
    subscribe_receipt: Option<OutboundRequestReceiptId>,
    subscribe_open: bool,
    /// An accepted frame (a duplicate included) has arrived in this session.
    accepted_seen: bool,
    /// Consecutive rejected frames with no accepted frame between them (R33).
    rejected_streak: u32,
    /// The Ping schedule, the dead-socket and data-silence deadlines and the
    /// host-sleep check (R29, R30, R58).
    clocks: SessionClocks,
    counts: FrameCounts,
    /// Finds the session's own token reflected in a field the Hub keeps or logs
    /// (R24, R52). It holds a digest, never the token.
    echo: EchoGuard,
}

/// The decision that ends a subscribed session: why, with which class, and how
/// the socket and the open receipts are closed (tables 8.1 and 8.4).
struct SessionEnd {
    cause: EndCause,
    /// The failure class that picks the next delay; `None` for shutdown and for
    /// ends that stop the run.
    class: Option<FailureClass>,
    /// Outcome of the subscribe receipt when it is still open.
    subscribe_outcome: OutboundRequestOutcome,
    /// The socket can still be written (it did not end by a socket error, end of
    /// stream, a server Close or the dead-socket rule, 0.2).
    usable: bool,
    /// The Hub sends an unsubscribe (R9).
    unsubscribe: bool,
    close_code: CloseCode,
    /// The run stops with this error after the session is closed (R12).
    stop: Option<StreamSupervisorError>,
}

impl SessionEnd {
    fn base(cause: EndCause) -> Self {
        Self {
            cause,
            class: None,
            subscribe_outcome: OutboundRequestOutcome::TransportError,
            usable: true,
            unsubscribe: true,
            close_code: CloseCode::Normal,
            stop: None,
        }
    }

    fn shutdown() -> Self {
        Self {
            subscribe_outcome: OutboundRequestOutcome::Cancelled,
            ..Self::base(EndCause::Shutdown)
        }
    }

    /// An end by a classified answer of the server (R27, R28).
    fn for_class(class: FailureClass, accepted_seen: bool) -> Self {
        match class {
            FailureClass::Vehicle => Self {
                class: Some(class),
                subscribe_outcome: OutboundRequestOutcome::Success,
                ..Self::base(EndCause::Vehicle)
            },
            // A refusal before any data means the server did not accept the
            // subscription for this credential, and one more frame for a refused
            // credential adds nothing; after data it ends a subscription that
            // did exist, so it is released (R9).
            FailureClass::Refusal => Self {
                class: Some(class),
                subscribe_outcome: OutboundRequestOutcome::AuthenticationRejected,
                unsubscribe: accepted_seen,
                ..Self::base(EndCause::Refusal)
            },
            FailureClass::Busy { .. } => Self {
                class: Some(class),
                ..Self::base(EndCause::Busy)
            },
            FailureClass::Protocol => Self::protocol(false),
            FailureClass::Transport | FailureClass::Unknown => Self {
                class: Some(class),
                subscribe_outcome: OutboundRequestOutcome::ProtocolError,
                ..Self::base(EndCause::Unknown)
            },
        }
    }

    /// Three rejected frames in a row, or an oversize message (R33).
    fn protocol(oversize: bool) -> Self {
        Self {
            class: Some(FailureClass::Protocol),
            subscribe_outcome: OutboundRequestOutcome::ProtocolError,
            close_code: if oversize {
                CloseCode::Size
            } else {
                CloseCode::Normal
            },
            ..Self::base(EndCause::Protocol)
        }
    }

    /// No data frame within the silence limit (R30): the server stopped sending
    /// on a socket that still answers. The Hub ends the session, so the
    /// unsubscribe and the Close are sent; it is an outside event (class
    /// `Transport`, session `transport_ended`).
    fn data_silence() -> Self {
        Self {
            class: Some(FailureClass::Transport),
            ..Self::base(EndCause::DataSilence)
        }
    }

    /// The socket ended by itself: error, end of stream, a server Close or the
    /// dead-socket rule. Nothing more can be sent on it.
    fn socket_gone(class: FailureClass) -> Self {
        Self {
            class: Some(class),
            usable: false,
            unsubscribe: false,
            ..Self::base(match class {
                FailureClass::Busy { .. } => EndCause::Busy,
                _ => EndCause::SocketEnded,
            })
        }
    }

    /// An end by an error inside the Hub that stops the run (R12).
    fn stopping(cause: EndCause, error: StreamSupervisorError) -> Self {
        Self {
            subscribe_outcome: OutboundRequestOutcome::Cancelled,
            stop: Some(error),
            ..Self::base(cause)
        }
    }

    /// An audit write failed: no further request is sent and the socket is
    /// dropped without an unsubscribe, which would be unaudited (R13).
    fn audit_failed(error: StreamSupervisorError) -> Self {
        Self {
            usable: false,
            unsubscribe: false,
            stop: Some(error),
            ..Self::base(EndCause::AuditFailed)
        }
    }
}

/// What the read loop does after one message.
enum Step {
    Continue,
    End(SessionEnd),
}

/// The audit rows of one run (R13): a correlation id that links every row, one
/// session row per attempt that passes the credential check, one request row per
/// outbound request. Without a store (test constructors) nothing is written.
struct RunAudit {
    store: Option<HubStore>,
    correlation_id: Uuid,
    vehicle_tesla_id: i64,
    /// The run's write guard: a receipt is neither started nor completed after it
    /// refuses, and what was started stays unresolved, as it would after a crash
    /// (B2 fix round, P3b fix 2, R-P3bF2-1).
    write_guard: Option<WriteRefusal>,
}

fn completion(outcome: OutboundRequestOutcome) -> OutboundRequestCompletion {
    OutboundRequestCompletion {
        outcome,
        http_status: None,
        retry_after_seconds: None,
    }
}

impl RunAudit {
    fn new(
        store: Option<HubStore>,
        vehicle_id: VehicleId,
        write_guard: Option<WriteRefusal>,
    ) -> Result<Self, StreamSupervisorError> {
        let vehicle_tesla_id = i64::try_from(vehicle_id.get())
            .map_err(|_| StoreError::InvalidOutboundRequestVehicleId)?;
        Ok(Self {
            store,
            correlation_id: Uuid::new_v4(),
            vehicle_tesla_id,
            write_guard,
        })
    }

    /// The run's write guard, immediately before an audit mutation: a refusal is the
    /// loss of the credential authority, which ends the run for good (R12).
    fn guard_write(&self) -> Result<(), StreamSupervisorError> {
        if self.store.is_some() && self.write_guard.as_ref().is_some_and(|refused| refused()) {
            return Err(StreamSupervisorError::CredentialAuthorityUnavailable);
        }
        Ok(())
    }

    fn begin_session(&self) -> Result<Option<StreamSessionReceiptId>, StreamSupervisorError> {
        self.guard_write()?;
        self.store
            .as_ref()
            .map(|store| store.begin_stream_session(self.correlation_id, self.vehicle_tesla_id))
            .transpose()
            .map_err(StreamSupervisorError::Audit)
    }

    /// Writes the request row before the request is sent. The fixed values:
    /// `transport` `stream`, `safety_class` `non_wake_endpoint` (the
    /// subscription goes to Tesla's stream server, not to the car; SG-L6
    /// measures whether it wakes a sleeping car), `precondition`
    /// `not_required`.
    fn begin_request(
        &self,
        operation: OutboundRequestOperation,
    ) -> Result<Option<OutboundRequestReceiptId>, StreamSupervisorError> {
        self.guard_write()?;
        self.store
            .as_ref()
            .map(|store| {
                store.begin_outbound_request(&OutboundRequestStart {
                    correlation_id: self.correlation_id,
                    vehicle_tesla_id: Some(self.vehicle_tesla_id),
                    transport: OutboundRequestTransport::Stream,
                    operation,
                    safety_class: OutboundRequestSafetyClass::NonWakeEndpoint,
                    precondition: OutboundRequestPrecondition::NotRequired,
                })
            })
            .transpose()
            .map_err(StreamSupervisorError::Audit)
    }

    fn complete_request(
        &self,
        receipt: Option<OutboundRequestReceiptId>,
        completion: OutboundRequestCompletion,
    ) -> Result<(), StreamSupervisorError> {
        let (Some(store), Some(receipt)) = (self.store.as_ref(), receipt) else {
            return Ok(());
        };
        self.guard_write()?;
        store
            .complete_outbound_request(receipt, &completion)
            .map_err(StreamSupervisorError::Audit)
    }

    /// Completes the session row by outcome (R14); an orderly shutdown carries
    /// its unsubscribe receipt, which the schema allows for no other outcome.
    fn complete_session(
        &self,
        session: Option<StreamSessionReceiptId>,
        outcome: SessionOutcome,
        unsubscribe: Option<OutboundRequestReceiptId>,
    ) -> Result<(), StreamSupervisorError> {
        let (Some(store), Some(session)) = (self.store.as_ref(), session) else {
            return Ok(());
        };
        self.guard_write()?;
        match (outcome.terminal(), unsubscribe) {
            (Some(terminal), _) => store.complete_stream_session_terminal(session, terminal),
            (None, Some(unsubscribe)) => {
                store.complete_stream_session_orderly(session, unsubscribe)
            }
            (None, None) => return Err(StreamSupervisorError::OrderlyShutdownUnavailable),
        }
        .map_err(StreamSupervisorError::Audit)
    }
}

/// Ends the run with an audit error after trying to complete the session row as
/// `failed`; if that write fails too the row stays `started`, which is truthful
/// evidence of an unclean end (R54).
fn stop_failed(
    audit: &RunAudit,
    session: Option<StreamSessionReceiptId>,
    error: StreamSupervisorError,
) -> Outcome {
    let _ = audit.complete_session(session, SessionOutcome::Failed, None);
    Outcome::Stop(Err(error))
}

/// Revokes the power gate when it is created and again when the run's future
/// ends or is dropped, so a task abort revokes it synchronously (R50).
struct GateGuard(Option<Arc<StreamPowerGate>>);

impl GateGuard {
    fn new(gate: Option<Arc<StreamPowerGate>>) -> Self {
        if let Some(gate) = &gate {
            gate.revoke();
        }
        Self(gate)
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        if let Some(gate) = &self.0 {
            gate.revoke();
        }
    }
}

/// The event that reports a session or attempt that ended with `class`, after
/// the counters of R37 were updated (R45, table in 5). A `Refusal` is reported
/// only when it is the second in a row: one refusal can be a token-rotation
/// race (R46).
fn event_for(class: &FailureClass, state: &RetryState) -> Option<StreamEvent> {
    match class {
        FailureClass::Transport | FailureClass::Busy { .. } | FailureClass::Unknown => {
            Some(StreamEvent::TransportUnavailable)
        }
        FailureClass::Vehicle => Some(StreamEvent::VehicleOffline),
        FailureClass::Refusal => (state.refusal_streak >= 2).then_some(StreamEvent::AuthRejected),
        FailureClass::Protocol => Some(StreamEvent::ProtocolViolation),
    }
}

/// Two samples are the same when their time and all 11 values are equal, with
/// `f64` compared bitwise (R26).
fn same_sample(a: &StreamUpdate, b: &StreamUpdate) -> bool {
    let bits = |value: Option<f64>| value.map(f64::to_bits);
    a.timestamp_ms == b.timestamp_ms
        && bits(a.elevation) == bits(b.elevation)
        && bits(a.est_lat) == bits(b.est_lat)
        && bits(a.est_lng) == bits(b.est_lng)
        && bits(a.est_range) == bits(b.est_range)
        && bits(a.heading) == bits(b.heading)
        && bits(a.odometer) == bits(b.odometer)
        && a.power == b.power
        && bits(a.range) == bits(b.range)
        && a.shift_state == b.shift_state
        && bits(a.soc) == bits(b.soc)
        && bits(a.speed) == bits(b.speed)
}

/// The `error_type` token as it may be logged (R52): only a token of the
/// grammar `^[a-z0-9_]{1,32}$`, else `other`, because error text may echo the
/// request.
pub(super) fn loggable_error_token(token: Option<&str>) -> &str {
    match token {
        Some(token)
            if (1..=32).contains(&token.len())
                && token
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') =>
        {
            token
        }
        _ => "other",
    }
}

/// Reads a `Retry-After` header value into whole seconds (RFC 9110 section
/// 10.2.3): delay-seconds, or an HTTP-date converted against the wall clock and
/// floored at 0. Only the preferred date form (IMF-fixdate) is read; the two
/// obsolete forms are treated as absent, which gives the schedule's own delay.
/// A value too large for the audit column is saturated; the schedule caps the
/// delay it honours at 3,600 s anyway (R34).
pub(super) fn parse_retry_after(value: &str, wall_now_ms: i64) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        let seconds = value.parse::<u64>().unwrap_or(u64::MAX);
        return Some(seconds.min(i64::MAX as u64));
    }
    let at_ms = parse_imf_fixdate(value)?;
    Some((at_ms.saturating_sub(wall_now_ms).max(0) / 1000) as u64)
}

/// Epoch milliseconds of an IMF-fixdate such as `Sun, 06 Nov 1994 08:49:37 GMT`.
fn parse_imf_fixdate(value: &str) -> Option<i64> {
    let mut parts = value.split(' ');
    let weekday = parts.next()?;
    if weekday.len() != 4 || !weekday.ends_with(',') {
        return None;
    }
    let day: i64 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let clock = parts.next()?;
    if parts.next()? != "GMT" || parts.next().is_some() {
        return None;
    }
    let mut fields = clock.split(':');
    let hour: i64 = fields.next()?.parse().ok()?;
    let minute: i64 = fields.next()?.parse().ok()?;
    let second: i64 = fields.next()?.parse().ok()?;
    if fields.next().is_some()
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..=60).contains(&second)
        || !(1970..=9999).contains(&year)
    {
        return None;
    }
    // Days since 1970-01-01 of a proleptic Gregorian date.
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000)
}

/// The class of a session the server closed (R31, table 8.3): code 1013 (Try
/// Again Later, IANA WebSocket close code registry) asks for patience and is
/// `Busy` with no retry-after; any other Close, or none, is a `Transport` end.
pub(super) fn close_failure_class(frame: Option<&CloseFrame>) -> FailureClass {
    if frame.is_some_and(|frame| frame.code == CloseCode::Again) {
        FailureClass::Busy { retry_after: None }
    } else {
        FailureClass::Transport
    }
}

/// What one inbound message means to a socket that has no subscription yet
/// (C4 R6, R27 to R29, R33). Text and UTF-8 Binary messages are decoded alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmittingStep {
    /// Only the liveness clock moves: a Ping, Pong, malformed or non-UTF-8
    /// input, a value that is not an object, a message with no string
    /// `msg_type`, a foreign or present non-string `tag` on a `data:error`, and
    /// every other message. No frame count, rejection streak, `Healthy` or
    /// `Telemetry` follows before the subscribe succeeded.
    Liveness,
    /// A successful `control:hello` with a positive `connection_timeout`: the
    /// ping interval it asks for.
    PingInterval(Duration),
    /// A non-success `control:hello`, or an applicable `data:error`: the
    /// attempt ends with this class and the Hub closes with 1000.
    Classified(FailureClass),
    /// A message above the size bound: protocol violation, close 1009.
    Oversize,
    /// The server's Close frame, with the class of the end (R31).
    ServerClose(FailureClass),
}

/// The pure dispatch of the Admitting phase. A hello's `error_type`, whatever
/// string it is (the empty one too), classifies it before its `code`; a hello
/// without one is classified by `code` alone (`classify_hello`). A `data:error`
/// is classified by its `error_type` token only (R28) and is applicable when it
/// carries no `tag` or the session's own.
fn admitting_step(message: &Message, tag: &str) -> AdmittingStep {
    let text = match message {
        Message::Text(text) => {
            if text.len() > K_MAX_MESSAGE {
                return AdmittingStep::Oversize;
            }
            text.as_str()
        }
        Message::Binary(bytes) => {
            if bytes.len() > K_MAX_MESSAGE {
                return AdmittingStep::Oversize;
            }
            match std::str::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => return AdmittingStep::Liveness,
            }
        }
        Message::Close(frame) => {
            return AdmittingStep::ServerClose(close_failure_class(frame.as_ref()));
        }
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
            return AdmittingStep::Liveness;
        }
    };
    let Ok(Value::Object(members)) = serde_json::from_str::<Value>(text) else {
        return AdmittingStep::Liveness;
    };
    let error_type = members.get("error_type").and_then(Value::as_str);
    match members.get("msg_type").and_then(Value::as_str) {
        Some("control:hello") => {
            let code = members.get("code").and_then(Value::as_i64);
            match classify_hello(code, error_type) {
                Some(class) => AdmittingStep::Classified(class),
                None => members
                    .get("connection_timeout")
                    .and_then(Value::as_i64)
                    .filter(|timeout_ms| *timeout_ms > 0)
                    .map_or(AdmittingStep::Liveness, |timeout_ms| {
                        AdmittingStep::PingInterval(ping_interval(Some(timeout_ms)))
                    }),
            }
        }
        Some("data:error") => {
            if members
                .get("tag")
                .is_some_and(|member| member.as_str() != Some(tag))
            {
                AdmittingStep::Liveness
            } else {
                AdmittingStep::Classified(classify_error(error_type))
            }
        }
        _ => AdmittingStep::Liveness,
    }
}

#[derive(Deserialize)]
struct MessageHead<'a> {
    #[serde(borrow)]
    msg_type: std::borrow::Cow<'a, str>,
}

impl TeslaStreamSupervisor {
    /// The run (R5 to R14): attempts one after the other until shutdown or an
    /// error that makes continuing impossible or unsafe (R12). Every other
    /// condition ends one session or check and is retried by the schedules of
    /// section 4, so C3 does not have to rebuild the supervisor.
    pub(crate) async fn run(
        self,
        shutdown: oneshot::Receiver<()>,
    ) -> Result<(), StreamSupervisorError> {
        // R50: revoked at run start and whenever the run ends, even by an abort.
        let _gate = GateGuard::new(self.power_gate.clone());
        let mut shutdown = Shutdown::new(shutdown);
        let audit = RunAudit::new(
            self.audit_store.clone(),
            self.vehicle_id,
            self.write_guard.clone(),
        )?;
        let mut run = RunState {
            retry: RetryState::default(),
            last_delivered: None,
            moving: false,
        };
        let mut delay = start_delay(self.draw());
        loop {
            if until(&mut shutdown, sleep(delay)).await.is_none() {
                return Ok(());
            }
            match self.attempt(&mut shutdown, &audit, &mut run).await {
                Outcome::Stop(result) => return result,
                Outcome::TerminalCredential => delay = terminal_credential_delay(self.draw()),
                Outcome::Failed(class) => {
                    // R37: counters first, then the delay; the event follows the
                    // rows of the ended session (R47).
                    run.retry.on_session_end(&class);
                    if let Some(event) = event_for(&class, &run.retry) {
                        match self.emit(event, &mut shutdown).await {
                            Emit::Sent => {}
                            Emit::Shutdown => return Ok(()),
                            Emit::ReceiverClosed => {
                                return Err(StreamSupervisorError::EventReceiverClosed);
                            }
                            Emit::QueueFull => return Err(StreamSupervisorError::EventQueueFull),
                        }
                    }
                    delay = next_delay(&class, &run.retry, self.draw());
                    tracing::debug!(
                        ?class,
                        transport_attempt = run.retry.transport_attempt,
                        vehicle_attempt = run.retry.vehicle_attempt,
                        refusal_streak = run.retry.refusal_streak,
                        delay_ms = delay.as_millis() as u64,
                        "stream attempt ended"
                    );
                }
            }
        }
    }

    /// One jitter draw (R35): fresh for every delay.
    fn draw(&self) -> f64 {
        self.jitter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .draw()
    }

    /// Puts one event on the channel, waiting at most 10 s for room (R49).
    async fn emit(&self, event: StreamEvent, shutdown: &mut Shutdown) -> Emit {
        let sent = until(shutdown, timeout(EVENT_QUEUE_WAIT, self.events.send(event))).await;
        match sent {
            None => Emit::Shutdown,
            Some(Err(_elapsed)) => Emit::QueueFull,
            Some(Ok(Err(_closed))) => Emit::ReceiverClosed,
            Some(Ok(Ok(()))) => Emit::Sent,
        }
    }

    fn credential_outcome(failure: CredentialFailure) -> Outcome {
        match failure {
            CredentialFailure::SensitiveAccess => {
                Outcome::Stop(Err(StreamSupervisorError::CredentialAuthorityUnavailable))
            }
            CredentialFailure::Terminal => Outcome::TerminalCredential,
            CredentialFailure::Unavailable => Outcome::Failed(FailureClass::Transport),
        }
    }

    /// Takes the manager lock as an owned guard, waiting at most 90 s (R6).
    /// Dropping a lock wait loses nothing.
    async fn lock_within_limit(&self) -> LockResult {
        let StreamCredential::Legacy(manager) = &self.credential;
        match timeout(LOCK_WAIT, Arc::clone(manager).lock_owned()).await {
            Err(_elapsed) => LockResult::TimedOut,
            Ok(guard) => LockResult::Locked(guard),
        }
    }

    /// [`Self::lock_within_limit`], interrupted by the shutdown signal (R11).
    async fn lock_manager(&self, shutdown: &mut Shutdown) -> LockResult {
        until(shutdown, self.lock_within_limit())
            .await
            .unwrap_or(LockResult::Shutdown)
    }

    /// R6 step 2: check the authority, then, in a task that owns the lock,
    /// refresh if due and learn whether a token can be issued. No network use
    /// other than C5's own refresh request, and no audit row.
    ///
    /// The refresh runs in its own task and is never aborted: once the auth
    /// server has rotated the refresh token, the only valid successor exists in
    /// the refresh until C5 persists it, and dropping the refresh at that point
    /// would lose it and force a new sign-in (C5 R45 row 6). A task keeps it
    /// alive whatever happens to the supervisor (shutdown, C3's abort, an error
    /// ending the run); C5 R33 bounds how long it can take, so the supervisor
    /// waits for it with no limit of its own.
    async fn check_credentials(&self, shutdown: &mut Shutdown) -> Check {
        let mut guard = match self.lock_manager(shutdown).await {
            LockResult::Locked(guard) => guard,
            LockResult::Shutdown => return Check::Shutdown,
            LockResult::TimedOut => return Check::Failed(CredentialFailure::Unavailable),
        };
        self.seams.notify(AdmissionEvent::LockAcquired);
        if let Err(error) = guard.assert_sensitive_access() {
            return Check::Failed(credential_failure(&error));
        }
        let Some(client) = self.client.clone() else {
            return Check::Failed(CredentialFailure::Unavailable);
        };
        let seams = self.seams.clone();
        let task = tokio::spawn(async move {
            seams.notify(AdmissionEvent::RefreshStarted);
            let refresh = guard.refresh_if_due(&client, SystemTime::now()).await;
            seams.notify(AdmissionEvent::RefreshFinished);
            if let Err(error) = refresh {
                match credential_failure(&error) {
                    CredentialFailure::Unavailable => {
                        // Neither a lost authority nor a stop: the token read
                        // decides, because C5 keeps collection running on the
                        // current token while it is usable (C5 R30, R45).
                        tracing::warn!("stream credential refresh did not complete");
                    }
                    failure => return Err(failure),
                }
            }
            if seams.terminal_credential() {
                return Err(CredentialFailure::Terminal);
            }
            guard
                .access_token_for_sensitive_use()
                .map(|_| ())
                .map_err(|error| credential_failure(&error))
        });
        match until(shutdown, task).await {
            None => Check::Shutdown,
            Some(Ok(Ok(()))) => Check::Ready,
            Some(Ok(Err(failure))) => Check::Failed(failure),
            Some(Err(_join_error)) => Check::Failed(CredentialFailure::Unavailable),
        }
    }

    /// R6 step 5: take the lock, copy the token into a zeroising buffer and
    /// unlock. The token is read here, immediately before the subscribe frame,
    /// so a token rotated by another holder of the shared manager during the
    /// connect is the one sent, and no token is held while waiting on the
    /// network (R7). The caller keeps the socket alive meanwhile
    /// ([`Self::live_wait`]) and handles the shutdown signal.
    async fn admit(&self) -> Admit {
        let guard = match self.lock_within_limit().await {
            LockResult::Locked(guard) => guard,
            LockResult::Shutdown | LockResult::TimedOut => {
                return Admit::Failed(CredentialFailure::Unavailable);
            }
        };
        if self.seams.terminal_credential() {
            return Admit::Failed(CredentialFailure::Terminal);
        }
        let token = match guard.access_token_for_sensitive_use() {
            Ok(token) => Zeroizing::new(token.to_owned()),
            Err(error) => return Admit::Failed(credential_failure(&error)),
        };
        drop(guard);
        self.seams.notify(AdmissionEvent::TokenRead);
        Admit::Token(token)
    }

    /// Waits for `work` on an open socket that has no subscription yet (R6
    /// steps 5 and 6) while keeping it alive as R29 requires from the moment it
    /// opens: Pings are sent on the clock's schedule, every inbound frame resets
    /// the liveness clock, and a socket that stays silent for three ping
    /// intervals or ends is reported as gone. A hello adopts the server's idle
    /// limit as the ping interval at once (R27, R29); a terminating hello or
    /// error, or an oversize message, ends the wait ([`admitting_step`]); every
    /// other frame only feeds liveness, because no subscription exists for it to
    /// answer. The shutdown signal interrupts the wait (R11).
    async fn live_wait<T>(
        &self,
        socket: &mut StreamSocket,
        shutdown: &mut Shutdown,
        clocks: &mut SessionClocks,
        work: impl Future<Output = T>,
    ) -> Live<T> {
        enum Wake<T> {
            Shutdown,
            Done(T),
            Message(Option<Result<Message, WebSocketError>>),
            Dead,
            Ping,
        }
        tokio::pin!(work);
        loop {
            let dead_at = clocks.dead_at();
            let ping_at = clocks.next_ping();
            let wake = tokio::select! {
                biased;
                () = shutdown.wait() => Wake::Shutdown,
                value = &mut work => Wake::Done(value),
                message = socket.next() => Wake::Message(message),
                () = sleep_until(dead_at) => Wake::Dead,
                () = sleep_until(ping_at) => Wake::Ping,
            };
            match wake {
                Wake::Shutdown => return Live::Shutdown,
                Wake::Done(value) => return Live::Done(value),
                Wake::Dead => {
                    tracing::debug!(host = %self.endpoint_host, "stream socket is dead");
                    return Live::Gone(FailureClass::Transport);
                }
                Wake::Ping => {
                    if let Some(end) = self.send_ping(socket, shutdown, clocks).await {
                        return match end.cause {
                            EndCause::Shutdown => Live::Shutdown,
                            _ => Live::Gone(end.class.unwrap_or(FailureClass::Transport)),
                        };
                    }
                }
                Wake::Message(message) => {
                    let arrived = Instant::now();
                    match message {
                        None
                        | Some(Err(
                            WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed,
                        )) => return Live::Gone(FailureClass::Transport),
                        Some(Err(WebSocketError::Capacity(_))) => {
                            return Live::Ended {
                                class: FailureClass::Protocol,
                                close_code: CloseCode::Size,
                            };
                        }
                        Some(Err(_)) => return Live::Gone(FailureClass::Transport),
                        Some(Ok(message)) => {
                            clocks.on_inbound(arrived);
                            match admitting_step(&message, &self.tag) {
                                AdmittingStep::Liveness => {}
                                AdmittingStep::PingInterval(interval) => {
                                    clocks.adopt_ping_interval(arrived, interval);
                                }
                                AdmittingStep::Classified(class) => {
                                    return Live::Ended {
                                        class,
                                        close_code: CloseCode::Normal,
                                    };
                                }
                                AdmittingStep::Oversize => {
                                    return Live::Ended {
                                        class: FailureClass::Protocol,
                                        close_code: CloseCode::Size,
                                    };
                                }
                                AdmittingStep::ServerClose(class) => {
                                    return Live::Gone(class);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// One attempt in the order of R6.
    async fn attempt(
        &self,
        shutdown: &mut Shutdown,
        audit: &RunAudit,
        run: &mut RunState,
    ) -> Outcome {
        // Step 2: credential before network. Any error here means no network use
        // and no audit row.
        match self.check_credentials(shutdown).await {
            Check::Ready => {}
            Check::Shutdown => return Outcome::Stop(Ok(())),
            Check::Failed(failure) => {
                tracing::debug!(?failure, "stream credential check failed");
                return Self::credential_outcome(failure);
            }
        }

        // Step 3: the session row, then the connect receipt, both before the
        // request they describe.
        let session = match audit.begin_session() {
            Ok(session) => session,
            Err(error) => return Outcome::Stop(Err(error)),
        };
        let connect_receipt = match audit.begin_request(OutboundRequestOperation::StreamConnect) {
            Ok(receipt) => receipt,
            Err(error) => return stop_failed(audit, session, error),
        };

        // Step 4: DNS, TCP, TLS and the upgrade within 10 s in total. The
        // access token is not in the URL or in any handshake header (R4).
        let connected = until(shutdown, timeout(CONNECT_LIMIT, self.open_socket())).await;
        let mut socket = match connected {
            None => {
                return self.end_unsubscribed(
                    audit,
                    session,
                    Some((
                        connect_receipt,
                        completion(OutboundRequestOutcome::Cancelled),
                    )),
                    EndCause::Shutdown,
                    Outcome::Stop(Ok(())),
                );
            }
            Some(Err(_elapsed)) => {
                return self.connect_failed(
                    audit,
                    session,
                    connect_receipt,
                    None,
                    None,
                    FailureClass::Transport,
                );
            }
            Some(Ok(Err(error))) => {
                let (class, status, retry_after) = self.upgrade_failure(&error);
                return self.connect_failed(
                    audit,
                    session,
                    connect_receipt,
                    status,
                    retry_after,
                    class,
                );
            }
            Some(Ok(Ok(socket))) => socket,
        };
        // R29: the liveness clocks run from the moment the socket opens, so the
        // Pings continue while the attempt waits for the manager lock.
        let mut clocks = SessionClocks::start(Instant::now(), self.wall_clock.now_ms());
        if let Err(error) =
            audit.complete_request(connect_receipt, completion(OutboundRequestOutcome::Success))
        {
            return stop_failed(audit, session, error);
        }

        // Step 5: admitting.
        let admitted = self
            .live_wait(&mut socket, shutdown, &mut clocks, self.admit())
            .await;
        let token = match admitted {
            Live::Done(Admit::Token(token)) => token,
            Live::Shutdown => {
                return self.end_unsubscribed(
                    audit,
                    session,
                    None,
                    EndCause::Shutdown,
                    Outcome::Stop(Ok(())),
                );
            }
            Live::Gone(class) => {
                return self.end_unsubscribed(
                    audit,
                    session,
                    None,
                    EndCause::SocketEnded,
                    Outcome::Failed(class),
                );
            }
            Live::Ended { class, close_code } => {
                // C4 R27, R28, R33: a terminating hello or error, or an oversize
                // message, ends this attempt. The connect succeeded and no
                // subscribe was begun, so only the session row is `failed`. The
                // socket is usable: it is closed with the two-second handshake.
                // Dropping the admission wait here abandons only this attempt's
                // lock request.
                tracing::debug!(?class, "stream attempt ended while admitting");
                close_handshake(&mut socket, close_code).await;
                drop(socket);
                return self.end_unsubscribed(
                    audit,
                    session,
                    None,
                    SessionEnd::for_class(class, false).cause,
                    Outcome::Failed(class),
                );
            }
            Live::Done(Admit::Failed(failure)) => {
                tracing::debug!(?failure, "stream admission failed");
                // Table 8.1: a lost authority drops the socket; a stopped
                // credential or any other error closes it without a subscribe.
                if failure != CredentialFailure::SensitiveAccess {
                    close_handshake(&mut socket, CloseCode::Normal).await;
                }
                drop(socket);
                return self.end_unsubscribed(
                    audit,
                    session,
                    None,
                    EndCause::CredentialError,
                    Self::credential_outcome(failure),
                );
            }
        };

        // Step 6: subscribing. The frame lives only until it is written. It is
        // built once, at its exact length, in a buffer that is wiped on drop and
        // never reallocated (R6 step 6). Accepted limit: the WebSocket library
        // copies the text into its own buffers (message, frame, masked output)
        // and the TLS layer holds the plaintext while encrypting; neither copy
        // can be wiped from here.
        let frame = match subscribe_frame_zeroizing(&self.tag, &token) {
            Ok(frame) => frame,
            Err(_) => {
                // A token outside the grammar of R8 is a corrupted credential.
                return self.end_unsubscribed(
                    audit,
                    session,
                    None,
                    EndCause::CredentialError,
                    Outcome::Failed(FailureClass::Transport),
                );
            }
        };
        // The guard keeps a digest of the token, so the session can recognise it
        // when a server reflects it, and no copy of it beyond the frame.
        let echo = EchoGuard::of(&token);
        drop(token);
        let Ok(text) = std::str::from_utf8(&frame) else {
            return self.end_unsubscribed(
                audit,
                session,
                None,
                EndCause::CredentialError,
                Outcome::Failed(FailureClass::Transport),
            );
        };
        let subscribe_receipt = match audit.begin_request(OutboundRequestOperation::StreamSubscribe)
        {
            Ok(receipt) => receipt,
            Err(error) => return stop_failed(audit, session, error),
        };
        // The write is bounded by its own 10 s limit and by the dead-socket
        // deadline: a peer that is not reading cannot be sent a Ping either, so
        // a stalled write is the dead-socket case (R29, R32).
        let write_deadline = (Instant::now() + WRITE_LIMIT).min(clocks.dead_at());
        let written = until(
            shutdown,
            timeout_at(write_deadline, socket.send(Message::text(text))),
        )
        .await;
        drop(frame);
        match written {
            None => {
                return self.end_unsubscribed(
                    audit,
                    session,
                    Some((
                        subscribe_receipt,
                        completion(OutboundRequestOutcome::Cancelled),
                    )),
                    EndCause::Shutdown,
                    Outcome::Stop(Ok(())),
                );
            }
            Some(Err(_) | Ok(Err(_))) => {
                return self.end_unsubscribed(
                    audit,
                    session,
                    Some((
                        subscribe_receipt,
                        completion(OutboundRequestOutcome::TransportError),
                    )),
                    EndCause::SubscribeWriteFailed,
                    Outcome::Failed(FailureClass::Transport),
                );
            }
            Some(Ok(Ok(()))) => {}
        }

        // Step 7: the read loop, then step 8, the close.
        let mut state = SessionState {
            subscribe_receipt,
            subscribe_open: true,
            accepted_seen: false,
            rejected_streak: 0,
            clocks,
            counts: FrameCounts::default(),
            echo,
        };
        // R30: silence is counted from the subscribe write.
        state.clocks.on_subscribed(Instant::now());
        let end = self
            .read_loop(&mut socket, shutdown, audit, run, &mut state)
            .await;
        self.log_summary(&state.counts);
        self.finish_subscribed(socket, end, audit, session, &state)
            .await
    }

    /// DNS, TCP, TLS and the WebSocket upgrade (R6 step 4). TLS certificates are
    /// always validated against the platform's roots, with no option to turn
    /// that off (R4). Small frames matter more than throughput, so Nagle's
    /// algorithm is off.
    async fn open_socket(&self) -> Result<StreamSocket, WebSocketError> {
        let request = self.endpoint.as_str().into_client_request()?;
        let transport = self.open_transport(&request).await?;
        let (socket, _response) =
            client_async_tls_with_config(request, transport, Some(stream_socket_config()), None)
                .await?;
        Ok(socket)
    }

    async fn open_transport(&self, request: &Request) -> Result<Transport, WebSocketError> {
        #[cfg(test)]
        if let Some(connector) = &self.seams.connector {
            return connector().await.map_err(WebSocketError::Io);
        }
        let uri = request.uri();
        let host = uri
            .host()
            .ok_or(WebSocketError::Url(UrlError::NoHostName))?;
        #[cfg(test)]
        crate::api::test_egress_guard::refuse_production_host(host);
        let port = uri
            .port_u16()
            .or(match uri.scheme_str() {
                Some("wss") => Some(443),
                Some("ws") => Some(80),
                _ => None,
            })
            .ok_or(WebSocketError::Url(UrlError::UnsupportedUrlScheme))?;
        let tcp = TcpStream::connect(format!("{host}:{port}")).await?;
        tcp.set_nodelay(true)?;
        Ok(Box::new(tcp))
    }

    /// Completes the open receipt, if any, and the session row of an attempt
    /// that never subscribed, then gives `outcome` (table 8.4).
    fn end_unsubscribed(
        &self,
        audit: &RunAudit,
        session: Option<StreamSessionReceiptId>,
        open: Option<(Option<OutboundRequestReceiptId>, OutboundRequestCompletion)>,
        cause: EndCause,
        outcome: Outcome,
    ) -> Outcome {
        if let Some((receipt, completion)) = open
            && let Err(error) = audit.complete_request(receipt, completion)
        {
            return stop_failed(audit, session, error);
        }
        match audit.complete_session(session, session_outcome(false, cause, false), None) {
            Ok(()) => outcome,
            Err(error) => Outcome::Stop(Err(error)),
        }
    }

    /// A connect that failed (DNS, TCP, TLS, the limit or an upgrade status):
    /// the connect receipt carries the outcome and any HTTP status and
    /// `Retry-After`; the session is `failed` (table 8.4).
    fn connect_failed(
        &self,
        audit: &RunAudit,
        session: Option<StreamSessionReceiptId>,
        receipt: Option<OutboundRequestReceiptId>,
        http_status: Option<u16>,
        retry_after_seconds: Option<u64>,
        class: FailureClass,
    ) -> Outcome {
        tracing::debug!(?class, host = %self.endpoint_host, ?http_status, "stream connect failed");
        let outcome = if class == FailureClass::Refusal {
            OutboundRequestOutcome::AuthenticationRejected
        } else {
            OutboundRequestOutcome::TransportError
        };
        let class = match class {
            FailureClass::Busy { .. } => FailureClass::Busy {
                retry_after: retry_after_seconds.map(Duration::from_secs),
            },
            other => other,
        };
        self.end_unsubscribed(
            audit,
            session,
            Some((
                receipt,
                OutboundRequestCompletion {
                    outcome,
                    http_status,
                    retry_after_seconds,
                },
            )),
            EndCause::ConnectFailed,
            Outcome::Failed(class),
        )
    }

    /// The class, HTTP status and `Retry-After` of a failed connect (R13, R31).
    /// An upgrade answered with a status other than 101 is classified by
    /// `classify_upgrade`; every other error is `Transport`.
    fn upgrade_failure(&self, error: &WebSocketError) -> (FailureClass, Option<u16>, Option<u64>) {
        let WebSocketError::Http(response) = error else {
            return (FailureClass::Transport, None, None);
        };
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(http::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| parse_retry_after(value, self.wall_clock.now_ms()));
        // The audit column takes only a real HTTP status (100 to 599), but the
        // WebSocket library accepts any three digits from a peer. A status
        // outside the range is still a failed upgrade of class `Transport`;
        // only its number is not recorded, so a peer's odd answer cannot turn
        // into an audit fault that ends the run (R12, R13).
        let recordable = (100..=599).contains(&status).then_some(status);
        (
            classify_upgrade(status, is_override_endpoint(&self.endpoint)),
            recordable,
            retry_after,
        )
    }

    /// One summary line per session (R52): counts only, never a frame value, a
    /// sample time, the tag or the endpoint's path.
    fn log_summary(&self, counts: &FrameCounts) {
        #[cfg(test)]
        if let Some(summaries) = &self.seams.summaries {
            summaries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(*counts);
        }
        tracing::debug!(
            host = %self.endpoint_host,
            region = ?self.region,
            accepted = counts.accepted,
            duplicates = counts.duplicates,
            rejected = counts.rejected,
            implausible_time = counts.implausible_time,
            fields_invalid = counts.fields_invalid,
            unknown_messages = counts.unknown_messages,
            ignored_errors = counts.ignored_errors,
            "stream session summary"
        );
    }

    /// The read loop of one session (section 3). Messages are processed one at
    /// a time in arrival order (R47). Between messages it waits for the earliest
    /// of the Ping tick (R29), the dead-socket deadline (R29), the
    /// data-silence deadline (R30) and the stable-session reset (R37); a message
    /// that is ready wins over a deadline that falls at the same instant,
    /// because it resets the clock.
    async fn read_loop(
        &self,
        socket: &mut StreamSocket,
        shutdown: &mut Shutdown,
        audit: &RunAudit,
        run: &mut RunState,
        state: &mut SessionState,
    ) -> SessionEnd {
        enum Wake {
            Shutdown,
            Message(Option<Result<Message, WebSocketError>>),
            DeadSocket,
            Silence,
            Ping,
            Stable,
        }
        loop {
            let dead_at = state.clocks.dead_at();
            let silent_at = state
                .clocks
                .silent_at(data_silence_limit(self.health_timeout, run.moving));
            let ping_at = state.clocks.next_ping();
            let stable_at = state.clocks.stable_at();
            let wake = tokio::select! {
                biased;
                () = shutdown.wait() => Wake::Shutdown,
                message = socket.next() => Wake::Message(message),
                () = sleep_until(dead_at) => Wake::DeadSocket,
                () = sleep_until(silent_at) => Wake::Silence,
                () = sleep_until(ping_at) => Wake::Ping,
                () = async {
                    match stable_at {
                        Some(at) => sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => Wake::Stable,
            };
            match wake {
                Wake::Shutdown => return SessionEnd::shutdown(),
                Wake::DeadSocket => {
                    // No inbound frame of any kind for three ping intervals (or
                    // one after a host sleep): the socket is not used again, so
                    // no unsubscribe is sent (R29).
                    tracing::debug!(host = %self.endpoint_host, "stream socket is dead");
                    return SessionEnd::socket_gone(FailureClass::Transport);
                }
                Wake::Silence => {
                    tracing::debug!(
                        host = %self.endpoint_host,
                        moving = run.moving,
                        "stream data silence"
                    );
                    return SessionEnd::data_silence();
                }
                Wake::Ping => {
                    if let Some(end) = self.send_ping(socket, shutdown, &mut state.clocks).await {
                        return end;
                    }
                }
                Wake::Stable => {
                    // R37: 60 s after the first accepted frame, with the session
                    // still open, transport back-off starts over. It is the last
                    // branch of the wait, so a message, a dead socket, data
                    // silence or a Ping due at the same instant is handled first:
                    // a session that the dead-socket or silence rule ends at
                    // exactly 60 s did not work for that minute, and keeps its
                    // back-off.
                    run.retry.on_stable();
                    state.clocks.on_stable_applied();
                    tracing::debug!(
                        host = %self.endpoint_host,
                        "stream session is stable, transport back-off reset"
                    );
                }
                Wake::Message(message) => {
                    let queued_at = Instant::now();
                    let message = match message {
                        None
                        | Some(Err(
                            WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed,
                        )) => {
                            return SessionEnd::socket_gone(FailureClass::Transport);
                        }
                        Some(Err(WebSocketError::Capacity(_))) => {
                            return SessionEnd::protocol(true);
                        }
                        Some(Err(_)) => return SessionEnd::socket_gone(FailureClass::Transport),
                        Some(Ok(message)) => message,
                    };
                    if let Step::End(end) = self
                        .handle_message(message, queued_at, shutdown, audit, run, state)
                        .await
                    {
                        return end;
                    }
                }
            }
        }
    }

    /// A ping tick (R29, R58): runs the host-sleep check, then sends a Ping with
    /// an empty payload. On a host sleep the gate is revoked first, because a
    /// confirmation made before the sleep is already stale (R50); the Ping is the
    /// tick's own, so it is sent at once, and the dead-socket deadline is now one
    /// ping interval away (`SessionClocks::on_ping_tick`). The write is bounded
    /// by the dead-socket deadline, so a peer that stops reading cannot hold the
    /// loop past it. `Some` is the end the tick causes.
    async fn send_ping(
        &self,
        socket: &mut StreamSocket,
        shutdown: &mut Shutdown,
        clocks: &mut SessionClocks,
    ) -> Option<SessionEnd> {
        let slept = clocks.on_ping_tick(Instant::now(), self.wall_clock.now_ms());
        if slept {
            tracing::info!(host = %self.endpoint_host, "stream host sleep detected");
            if let Some(gate) = &self.power_gate {
                gate.revoke();
            }
        }
        let sent = until(
            shutdown,
            timeout_at(
                clocks.dead_at(),
                socket.send(Message::Ping(Default::default())),
            ),
        )
        .await;
        match sent {
            None => Some(SessionEnd::shutdown()),
            Some(Err(_) | Ok(Err(_))) => Some(SessionEnd::socket_gone(FailureClass::Transport)),
            Some(Ok(Ok(()))) => None,
        }
    }

    /// Dispatch of one inbound message (table 8.2).
    async fn handle_message(
        &self,
        message: Message,
        queued_at: Instant,
        shutdown: &mut Shutdown,
        audit: &RunAudit,
        run: &mut RunState,
        state: &mut SessionState,
    ) -> Step {
        // R29: every inbound frame of any kind resets the liveness clock.
        state.clocks.on_inbound(queued_at);
        let text = match &message {
            // The library answers a Ping with a Pong of the same payload (RFC
            // 6455 section 5.5.3) on its next read, which this loop makes at
            // once; a Pong only feeds liveness (R17).
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => return Step::Continue,
            Message::Close(frame) => {
                return Step::End(SessionEnd::socket_gone(close_failure_class(frame.as_ref())));
            }
            Message::Text(text) => text.as_str(),
            Message::Binary(bytes) => match std::str::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => return self.reject(state),
            },
        };
        // R17: a message is a top-level JSON object with a string `msg_type`. serde
        // also reads a struct from a sequence, so `["control:other"]` would parse as a
        // head; a frame whose first token is not `{` is rejected before it is parsed
        // (B2 round 3, C4R3-N3).
        if !text
            .trim_start_matches([' ', '\t', '\n', '\r'])
            .starts_with('{')
        {
            return self.reject(state);
        }
        let Ok(head) = serde_json::from_str::<MessageHead<'_>>(text) else {
            return self.reject(state);
        };
        match head.msg_type.as_ref() {
            "data:update" => {
                self.handle_update(text, queued_at, shutdown, audit, run, state)
                    .await
            }
            "data:error" => self.handle_error(text, state),
            "control:hello" => self.handle_hello(text, queued_at, state),
            _ => {
                state.counts.unknown_messages += 1;
                Step::Continue
            }
        }
    }

    /// A rejected frame is dropped and counted; three in a row, with no accepted
    /// frame between them, end the session (R33).
    fn reject(&self, state: &mut SessionState) -> Step {
        state.counts.rejected += 1;
        state.rejected_streak += 1;
        if state.rejected_streak >= INVALID_FRAME_STREAK {
            Step::End(SessionEnd::protocol(false))
        } else {
            Step::Continue
        }
    }

    /// A `data:update` (R19 to R26, R20a, R45, R50).
    async fn handle_update(
        &self,
        text: &str,
        queued_at: Instant,
        shutdown: &mut Shutdown,
        audit: &RunAudit,
        run: &mut RunState,
        state: &mut SessionState,
    ) -> Step {
        let Ok((mut update, mut fields_invalid)) = parse_data_update_counted(text) else {
            return self.reject(state);
        };
        // R24: a gear text that is the session's own token is a reflected
        // credential, not a reading. It is dropped as an invalid field before
        // the frame is observed, emitted or stored.
        if update
            .shift_state
            .as_deref()
            .is_some_and(|gear| state.echo.reflects(gear))
        {
            update.shift_state = None;
            fields_invalid = fields_invalid.saturating_add(1);
        }
        // R25: another vehicle's data is never attributed to this one.
        if update.tag != self.tag {
            return self.reject(state);
        }
        // R20a: one far-future time would make every later frame of the vehicle
        // a duplicate in storage.
        let wall_ms = self.wall_clock.now_ms();
        if !plausible_sample_time(update.timestamp_ms, wall_ms) {
            state.counts.implausible_time += 1;
            return self.reject(state);
        }
        state.rejected_streak = 0;
        // R30: a frame that is not rejected is data, duplicate or not, with
        // invalid fields or not; the last accepted frame decides whether the
        // car is moving, which picks the silence limit.
        state.clocks.on_data(queued_at);
        run.moving = frame_shows_motion(&update);
        state.counts.fields_invalid = state.counts.fields_invalid.saturating_add(fields_invalid);
        let duplicate = run
            .last_delivered
            .as_ref()
            .is_some_and(|last| same_sample(last, &update));
        // R50: observed at the socket, before the events are queued.
        if let Some(gate) = &self.power_gate {
            gate.observe(&update, wall_ms);
        }
        if duplicate {
            state.counts.duplicates += 1;
        } else {
            state.counts.accepted += 1;
        }
        if !state.accepted_seen {
            state.accepted_seen = true;
            run.retry.on_first_accepted();
            state.clocks.on_first_accepted(queued_at);
            if state.subscribe_open {
                state.subscribe_open = false;
                if let Err(error) = audit.complete_request(
                    state.subscribe_receipt,
                    completion(OutboundRequestOutcome::Success),
                ) {
                    return Step::End(SessionEnd::audit_failed(error));
                }
            }
            if let Some(end) = self.deliver(StreamEvent::Healthy, shutdown).await {
                return Step::End(end);
            }
        }
        if !duplicate {
            let telemetry = StreamEvent::Telemetry {
                update: Box::new(update.clone()),
                queued_at,
            };
            if let Some(end) = self.deliver(telemetry, shutdown).await {
                return Step::End(end);
            }
            run.last_delivered = Some(update);
        }
        Step::Continue
    }

    /// Puts an in-session event on the channel; `Some` is the end it causes.
    async fn deliver(&self, event: StreamEvent, shutdown: &mut Shutdown) -> Option<SessionEnd> {
        match self.emit(event, shutdown).await {
            Emit::Sent => None,
            Emit::Shutdown => Some(SessionEnd::shutdown()),
            Emit::QueueFull => Some(SessionEnd::stopping(
                EndCause::QueueFull,
                StreamSupervisorError::EventQueueFull,
            )),
            Emit::ReceiverClosed => Some(SessionEnd::stopping(
                EndCause::ReceiverClosed,
                StreamSupervisorError::EventReceiverClosed,
            )),
        }
    }

    /// The `error_type` as the log may carry it (R52): `other` when it is
    /// exactly the session's own stream tag, which the token grammar alone
    /// would let through, else [`EchoGuard::loggable`]. Only the logged text
    /// changes; the class is always taken from the original value.
    fn loggable_error_type<'a>(
        &self,
        state: &SessionState,
        error_type: Option<&'a str>,
    ) -> &'a str {
        match error_type {
            Some(token) if token == self.tag => "other",
            _ => state.echo.loggable(error_type),
        }
    }

    /// A `data:error` (R28). A frame whose tag is present and differs from the
    /// session's is ignored and counted; any other ends the session with the
    /// class of its `error_type` token, never of its text.
    fn handle_error(&self, text: &str, state: &mut SessionState) -> Step {
        let Ok(Value::Object(members)) = serde_json::from_str::<Value>(text) else {
            return self.reject(state);
        };
        if members
            .get("tag")
            .is_some_and(|tag| tag.as_str() != Some(self.tag.as_str()))
        {
            state.counts.ignored_errors += 1;
            return Step::Continue;
        }
        let error_type = members.get("error_type").and_then(Value::as_str);
        tracing::debug!(
            error_type = self.loggable_error_type(state, error_type),
            "stream error frame"
        );
        Step::End(SessionEnd::for_class(
            classify_error(error_type),
            state.accepted_seen,
        ))
    }

    /// A `control:hello` (R27). It may arrive at any time and more than once. A
    /// success adopts the server's idle limit as the ping interval; a
    /// non-success hello ends the session with its class. A member of the wrong
    /// JSON type counts as absent.
    fn handle_hello(&self, text: &str, queued_at: Instant, state: &mut SessionState) -> Step {
        let Ok(Value::Object(members)) = serde_json::from_str::<Value>(text) else {
            return self.reject(state);
        };
        let code = members.get("code").and_then(Value::as_i64);
        let error_type = members.get("error_type").and_then(Value::as_str);
        let connection_timeout = members.get("connection_timeout").and_then(Value::as_i64);
        match classify_hello(code, error_type) {
            None => {
                if connection_timeout.is_some_and(|timeout_ms| timeout_ms > 0) {
                    state
                        .clocks
                        .adopt_ping_interval(queued_at, ping_interval(connection_timeout));
                }
                Step::Continue
            }
            Some(class) => {
                tracing::debug!(
                    ?class,
                    error_type = self.loggable_error_type(state, error_type),
                    "stream hello was not a success"
                );
                Step::End(SessionEnd::for_class(class, state.accepted_seen))
            }
        }
    }

    /// Step 8 for a subscribed session (R9, R10, R13, R14): revoke the gate,
    /// complete the open subscribe receipt, unsubscribe and close when the Hub
    /// ends the session on a usable socket, then complete the session row.
    async fn finish_subscribed(
        &self,
        mut socket: StreamSocket,
        mut end: SessionEnd,
        audit: &RunAudit,
        session: Option<StreamSessionReceiptId>,
        state: &SessionState,
    ) -> Outcome {
        // R50: revoked whenever a session ends, for any reason.
        if let Some(gate) = &self.power_gate {
            gate.revoke();
        }
        if end.cause == EndCause::AuditFailed {
            drop(socket);
            let error = end
                .stop
                .take()
                .unwrap_or(StreamSupervisorError::OrderlyShutdownUnavailable);
            return stop_failed(audit, session, error);
        }
        if state.subscribe_open
            && let Err(error) =
                audit.complete_request(state.subscribe_receipt, completion(end.subscribe_outcome))
        {
            drop(socket);
            return stop_failed(audit, session, error);
        }
        let mut unsubscribe_receipt = None;
        let mut unsubscribe_ok = false;
        if end.usable {
            let mut writable = true;
            if end.unsubscribe {
                let receipt = match audit.begin_request(OutboundRequestOperation::StreamUnsubscribe)
                {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        drop(socket);
                        return stop_failed(audit, session, error);
                    }
                };
                // The frame was validated with the tag at construction.
                let written = match unsubscribe_frame(&self.tag) {
                    Ok(frame) => {
                        timeout(UNSUBSCRIBE_WRITE_LIMIT, socket.send(Message::text(frame)))
                            .await
                            .is_ok_and(|result| result.is_ok())
                    }
                    Err(_) => false,
                };
                unsubscribe_ok = written;
                writable = written;
                unsubscribe_receipt = receipt;
                let outcome = if written {
                    OutboundRequestOutcome::Success
                } else {
                    OutboundRequestOutcome::TransportError
                };
                if let Err(error) = audit.complete_request(receipt, completion(outcome)) {
                    drop(socket);
                    return stop_failed(audit, session, error);
                }
            }
            if writable {
                close_handshake(&mut socket, end.close_code).await;
            }
        }
        drop(socket);
        let outcome = session_outcome(true, end.cause, unsubscribe_ok);
        if let Err(error) = audit.complete_session(session, outcome, unsubscribe_receipt) {
            return Outcome::Stop(Err(error));
        }
        if let Some(error) = end.stop.take() {
            return Outcome::Stop(Err(error));
        }
        match end.class {
            None => Outcome::Stop(Ok(())),
            Some(class) => Outcome::Failed(class),
        }
    }
}

/// The closing handshake of RFC 6455 section 7.1.1 (R10): send a Close frame and
/// wait for the server's, together at most 2 s, then the caller drops the
/// socket. The server sends no acknowledgement of an unsubscribe that the unit
/// could wait for, so this is the only wait.
async fn close_handshake(socket: &mut StreamSocket, code: CloseCode) {
    let handshake = async {
        let frame = CloseFrame {
            code,
            reason: "".into(),
        };
        if socket.send(Message::Close(Some(frame))).await.is_err() {
            return;
        }
        while let Some(Ok(message)) = socket.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    };
    let _ = timeout(CLOSE_WAIT, handshake).await;
}

#[cfg(test)]
#[path = "admitting_tests.rs"]
mod admitting_tests;
