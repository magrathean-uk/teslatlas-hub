// SPDX-License-Identifier: AGPL-3.0-only

//! One token attempt under the kept fencing, and the single-flight object that
//! runs it (C5 R15, R15a, R17, R18, R23, R24, R33a, R45, R50, R51, R60;
//! section 5.4).
//!
//! The exchange (reserve, guard, send, read, validate, first persist attempt)
//! runs in a task owned by [`Flight`], with its own copies of the refresh token,
//! the endpoint, the client and the store handle. It never takes the manager
//! lock and it is never aborted when a caller is dropped: aborting a sent
//! request makes the rotation outcome unknown (R45 row 6) and forces a new
//! sign-in, and a stream drops within seconds of an ordinary stop (R15a). The
//! task publishes an [`ExchangeResult`] to the flight, and whoever holds the
//! manager next applies it before any token use.
//!
//! This file decides what happened. What it means for the account (state,
//! schedule, stops) is decided where the result is applied.

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use reqwest::{Client, header::DATE};
use tokio::sync::Notify;
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::{
    LegacyAuthError,
    schedule::{Schedule, retry_after_seconds},
    state::PersistReread,
    vocabulary::RetryReason,
    wire::{
        AcceptedKind, AcceptedTokens, AttemptClass, Exchange, PreparedRequest, classify_answer,
    },
};
use crate::db::{OutboundRequestOutcome, OutboundRequestReceiptId};

/// The clock, the sleeper and the random source of the unit (R37, R19, R31).
///
/// One object serves every decision of an attempt, so the schedule, the gates
/// and the receipts cannot disagree about the time, and tests supply exact
/// times and no waiting.
pub(crate) trait AuthEnv: Send + Sync {
    /// Local epoch seconds (R37). A clock before 1970 reads as 0, which is
    /// below the clock floor of R38 and so reads as an invalid clock.
    fn now(&self) -> i64;

    /// Wait `seconds` (the waits of R19 and R58 that a forced refresh sits out).
    fn sleep(&self, seconds: i64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    /// A number in `[0, 1)` for the retry spread (R31). Tests return 0.
    fn spread_unit(&self) -> f64;
}

/// The wall clock, the runtime's timer and the operating system's entropy.
pub(crate) struct SystemEnv;

impl AuthEnv for SystemEnv {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
            .unwrap_or(0)
    }

    fn sleep(&self, seconds: i64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let seconds = u64::try_from(seconds).unwrap_or(0);
        Box::pin(tokio::time::sleep(Duration::from_secs(seconds)))
    }

    fn spread_unit(&self) -> f64 {
        let mut bytes = [0_u8; 4];
        if getrandom::fill(&mut bytes).is_err() {
            // No entropy: no spread. The spread only avoids lockstep (R31).
            return 0.0;
        }
        f64::from(u32::from_le_bytes(bytes)) / (f64::from(u32::MAX) + 1.0)
    }
}

/// One reserved attempt: the receipt and the input generation the fencing keeps
/// (K1). A manager without a store has neither a receipt nor a fence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub receipt: Option<OutboundRequestReceiptId>,
    pub input: Uuid,
}

/// Why the fencing refused to reserve an attempt (R44a, R57 step 3, K1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BeginFailure {
    /// The pair row holds a generation that neither this manager nor any of
    /// the unit's own refreshes produced: an external write (R44a), which
    /// resolves the unit, never signs it out.
    ExternalWrite,
    /// The row holds the output of a completed refresh that this manager did
    /// not run itself (R62): the unit adopts it.
    OwnOutput,
    /// An attempt is unresolved or the input is fenced (K1): the outcome of a
    /// consumed token is unknown, so nothing is sent (R50, R57 step 3).
    Unresolved,
    /// The store could not answer. Nothing was written and nothing is sent.
    Store,
}

/// How a closed attempt is recorded (R51).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Closing {
    pub outcome: OutboundRequestOutcome,
    pub http_status: Option<u16>,
    pub retry_after_seconds: Option<u64>,
    /// Whether the input may be sent again: the binding and the fence are
    /// deleted exactly as the kept cancel does (R51).
    pub release_input: bool,
}

/// An attempt whose closing call failed, so its receipt is still `started`
/// (C5-S1). The input was not consumed in any of these cases (R45 rows 1, 7, 9
/// to 14; R60), so the closing is kept and tried again before any new attempt
/// begins: `begin_legacy_refresh` refuses while a `started` receipt exists
/// (K1), and reading that refusal as an unresolved attempt would sign the owner
/// out for a token nobody used. Both calls are idempotent: each requires a
/// `started` receipt bound to the input, so one that already ran (or that an
/// external write resolved, K4) is refused and is then known to be done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingClose {
    /// Nothing was sent: free the input with the kept cancel (K3, R17).
    Cancel(Attempt),
    /// A connection-phase failure or a complete answer: record it (R51).
    Close(Attempt, Closing),
}

/// A port refused or failed. It carries nothing: the store error text is not
/// needed here and no secret may leave (R53).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PortError;

/// What the exchange needs from the store and the sensitive-access guard. The
/// manager implements it over the kept fencing storage; a manager without a
/// store implements it without fences.
pub(crate) trait AttemptPorts: Send + Sync {
    /// The sensitive-access guard (R17).
    fn sensitive_access(&self) -> Result<(), LegacyAuthError>;
    /// Reserve the single-use input: the receipt, the input fence and the
    /// binding commit together (K1).
    fn begin(&self) -> Result<Attempt, BeginFailure>;
    /// Close an attempt that sent nothing (K3, R17, R45 row 1).
    fn cancel_unsent(&self, attempt: &Attempt) -> Result<(), PortError>;
    /// Close a sent attempt with a complete answer (R51).
    fn close(&self, attempt: &Attempt, closing: &Closing) -> Result<(), PortError>;
    /// Try a closing that failed before once more (C5-S1). `Ok` also when the
    /// attempt turns out to be resolved already, because an external write
    /// closed it (K4) or an earlier try had committed: nothing is left to do.
    fn finish(&self, pending: &PendingClose) -> Result<(), PortError>;
    /// Write the validated successor and close the receipt (K2, R23).
    fn persist(&self, attempt: &Attempt, successor: &SuccessorPair) -> Result<(), PortError>;
    /// Re-read the pair row before a retry of the write (R24, R62).
    fn reread(&self, attempt: &Attempt, successor: Uuid) -> PersistReread;
}

/// The validated successor of a refresh (R20 to R23). It is zeroised on drop.
///
/// It exists from the first valid 2xx until it is persisted (R23), and while it
/// does it is the only valid refresh token if the answer rotated (R24).
#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct SuccessorPair {
    pub(crate) access_token: Zeroizing<String>,
    pub(crate) refresh_token: Zeroizing<String>,
    #[zeroize(skip)]
    pub(crate) kind: AcceptedKind,
    #[zeroize(skip)]
    pub(crate) schedule: Schedule,
    /// The send time `t_s` (R18).
    #[zeroize(skip)]
    pub(crate) sent_at: i64,
    /// The generation of the pair this one replaces.
    #[zeroize(skip)]
    pub(crate) input: Uuid,
    /// The generation of this pair.
    #[zeroize(skip)]
    pub(crate) output: Uuid,
    #[zeroize(skip)]
    pub(crate) attempt: Attempt,
}

impl SuccessorPair {
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    pub(crate) fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
}

impl fmt::Debug for SuccessorPair {
    /// Times and codes only (R53): a generation is a digest of a refresh token.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SuccessorPair")
            .field("kind", &self.kind)
            .field("schedule", &self.schedule)
            .field("sent_at", &self.sent_at)
            .finish()
    }
}

/// How an attempt ended, before it means anything for the account.
#[derive(Debug)]
pub(crate) enum ExchangeKind {
    /// Row 1: nothing reached the server; the input is reusable.
    NotSent { reason: RetryReason },
    /// The collector's shutdown began before the request was sent; the attempt
    /// was cancelled and nothing changes (R33a, row 1).
    Aborted,
    /// The fencing refused to reserve an attempt.
    RowChanged(BeginFailure),
    /// Rows 2 and 3: the successor was written (R23).
    Persisted(Box<SuccessorPair>),
    /// Rows 2, 3 and 5: the answer was valid and its write is pending (R24).
    Pending {
        successor: Box<SuccessorPair>,
        error: LegacyAuthError,
    },
    /// The write found the row changed by an external writer: the successor was
    /// discarded and the attempt is resolved (R24, R44a, K4).
    External,
    /// Any other row of the outcome table.
    Failed {
        class: AttemptClass,
        error: LegacyAuthError,
        status: Option<u16>,
        /// The `Retry-After` header in seconds from the end of the exchange.
        retry_after: Option<i64>,
    },
}

/// The published result of one exchange.
#[derive(Debug)]
pub(crate) struct ExchangeResult {
    pub kind: ExchangeKind,
    /// The generation the attempt presented. Audit facts of the published
    /// result: the manager reads what the result means from `kind`, and the
    /// receipts hold the rest, so no caller reads these two outside a debugger.
    #[allow(dead_code)]
    pub input: Uuid,
    /// When the request was handed to the client (R18), if it was.
    #[allow(dead_code)]
    pub sent_at: Option<i64>,
    /// When the exchange ended; retry times count from it (R31).
    pub finished_at: i64,
    /// The `Date` header of the answer, if there was one (R55). Not a secret.
    pub server_date: Option<String>,
    /// A closing that failed and is still owed (C5-S1).
    pub pending_close: Option<PendingClose>,
}

/// Everything the detached task owns (R15a).
pub(crate) struct ExchangeInput {
    pub client: Client,
    pub endpoint: Url,
    pub access_token: Zeroizing<String>,
    pub refresh_token: Zeroizing<String>,
    pub pinned_issuer: Option<String>,
    pub ports: Arc<dyn AttemptPorts>,
    pub env: Arc<dyn AuthEnv>,
    pub shutdown: Arc<AtomicBool>,
    /// The generation the unit holds, for results that happen before a
    /// reservation exists.
    pub generation: Uuid,
    /// Test only: what the send came to, in place of the network. The
    /// connection-phase timeout of R33 cannot be produced on loopback in a
    /// way that holds on every host, so a test says what the client reported.
    #[cfg(test)]
    pub injected: Option<Exchange>,
}

/// The fencing row of each class of the outcome table (R45 "Receipt", "Fence").
/// `None` leaves the attempt unresolved (rows 4 and 6): the consumed token can
/// never be sent again, in this process or after a restart (R50, R60).
fn closing_for(class: AttemptClass, status: u16, retry_after: Option<i64>) -> Option<Closing> {
    let http = |outcome, release_input| Closing {
        outcome,
        http_status: Some(status),
        retry_after_seconds: None,
        release_input,
    };
    match class {
        // Row 8: the token is dead. Closed with its input kept fenced, so no
        // later path can resend it and a re-import of it is refused (R51).
        AttemptClass::Dead => Some(http(OutboundRequestOutcome::AuthenticationRejected, false)),
        // Rows 7 and 9 to 12: a complete error answer; the server issued no
        // grant, so the input is released.
        AttemptClass::ClientRefused
        | AttemptClass::RequestRejected
        | AttemptClass::Refused
        | AttemptClass::Misrouted
        | AttemptClass::Blocked => Some(http(OutboundRequestOutcome::HttpError, true)),
        // Row 13: the same, with the wait the server asked for when it gave one.
        AttemptClass::Transient => Some(Closing {
            retry_after_seconds: retry_after.and_then(|seconds| u64::try_from(seconds).ok()),
            ..http(OutboundRequestOutcome::HttpError, true)
        }),
        // Row 14: the endpoint answered with the pair it was given, so nothing
        // changed and nothing was consumed (F6).
        AttemptClass::NotRenewed => Some(http(OutboundRequestOutcome::Success, true)),
        // Rows 1, 2, 3, 4 and 6 are closed elsewhere or not at all.
        AttemptClass::NotSent
        | AttemptClass::Rotated
        | AttemptClass::NotRotated
        | AttemptClass::ConsumedWithoutSuccessor
        | AttemptClass::Unknown => None,
    }
}

/// Run one exchange (section 5.4): reserve the input, run the guard, read the
/// send time, send, classify the answer, close the receipt and persist.
pub(crate) async fn run_exchange(input: ExchangeInput) -> ExchangeResult {
    let ExchangeInput {
        client,
        endpoint,
        access_token,
        refresh_token,
        pinned_issuer,
        ports,
        env,
        shutdown,
        generation,
        #[cfg(test)]
        injected,
    } = input;
    let ended = |kind: ExchangeKind, input: Uuid, sent_at: Option<i64>, date: Option<String>| {
        ExchangeResult {
            kind,
            input,
            sent_at,
            finished_at: env.now(),
            server_date: date,
            pending_close: None,
        }
    };

    let attempt = match ports.begin() {
        Ok(attempt) => attempt,
        Err(failure) => {
            return ended(ExchangeKind::RowChanged(failure), generation, None, None);
        }
    };
    let input = attempt.input;

    // R17: nothing is sent unless the guard passes; the attempt is closed with
    // the kept cancel (K3). If even that fails nothing was sent and the input is
    // untouched, so the cancel is kept, owed, and tried again before any new
    // reservation begins (C5-S1); it is not an unresolved attempt.
    let cancel = |kind: ExchangeKind| {
        let mut result = ended(kind, input, None, None);
        if ports.cancel_unsent(&attempt).is_err() {
            result.pending_close = Some(PendingClose::Cancel(attempt));
        }
        result
    };
    // A closing that fails is kept, owed, for the same reason (C5-S1).
    let close = |closing: Closing| {
        ports
            .close(&attempt, &closing)
            .is_err()
            .then_some(PendingClose::Close(attempt, closing))
    };
    let Some(prepared) = PreparedRequest::new(&client, endpoint, &refresh_token) else {
        return cancel(ExchangeKind::NotSent {
            reason: RetryReason::Network,
        });
    };
    if ports.sensitive_access().is_err() {
        return cancel(ExchangeKind::NotSent {
            reason: RetryReason::SensitiveAccess,
        });
    }
    // R33a: once shutdown has begun no request starts.
    if shutdown.load(Ordering::Acquire) {
        return cancel(ExchangeKind::Aborted);
    }
    // R18: the send time is read after the guard and immediately before the
    // request is handed to the client, so the lifetime never starts early.
    let sent_at = env.now();
    #[cfg(test)]
    let sent = match injected {
        Some(exchange) => exchange,
        None => prepared.send().await,
    };
    #[cfg(not(test))]
    let sent = prepared.send().await;
    let answer = match sent {
        failed @ (Exchange::NotSent | Exchange::ConnectTimedOut) => {
            // Connection-phase failure: the body was never sent (R33), so the
            // token is untouched. The receipt records which kind it was (R51).
            let outcome = if matches!(failed, Exchange::ConnectTimedOut) {
                OutboundRequestOutcome::Timeout
            } else {
                OutboundRequestOutcome::TransportError
            };
            let pending = close(Closing {
                outcome,
                http_status: None,
                retry_after_seconds: None,
                release_input: true,
            });
            let mut result = ended(
                ExchangeKind::NotSent {
                    reason: RetryReason::Network,
                },
                input,
                Some(sent_at),
                None,
            );
            result.pending_close = pending;
            return result;
        }
        Exchange::Unknown => {
            // Row 6: whether the server rotated is unknown, and resending a
            // possibly rotated token risks F12, so the attempt stays unresolved.
            let error = if ports.sensitive_access().is_err() {
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            } else {
                LegacyAuthError::RotationOutcomeUnknown
            };
            return ended(
                ExchangeKind::Failed {
                    class: AttemptClass::Unknown,
                    error,
                    status: None,
                    retry_after: None,
                },
                input,
                Some(sent_at),
                None,
            );
        }
        Exchange::Answer(answer) => answer,
    };
    let server_date = answer
        .headers
        .get(DATE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let status = answer.status.as_u16();
    let accepted = match classify_answer(
        &answer,
        &access_token,
        &refresh_token,
        pinned_issuer.as_deref(),
    ) {
        Ok(accepted) => accepted,
        Err(failure) => {
            let retry_after = retry_after_seconds(&answer.headers, env.now());
            let pending = closing_for(failure.class, status, retry_after).and_then(&close);
            let mut result = ended(
                ExchangeKind::Failed {
                    class: failure.class,
                    error: failure.error,
                    status: Some(status),
                    retry_after,
                },
                input,
                Some(sent_at),
                server_date,
            );
            result.pending_close = pending;
            return result;
        }
    };
    if accepted.kind == AcceptedKind::NotRenewed {
        let pending = closing_for(AttemptClass::NotRenewed, status, None).and_then(&close);
        let mut result = ended(
            ExchangeKind::Failed {
                class: AttemptClass::NotRenewed,
                error: LegacyAuthError::RefreshNotRenewed,
                status: Some(status),
                retry_after: None,
            },
            input,
            Some(sent_at),
            server_date,
        );
        result.pending_close = pending;
        return result;
    }

    #[cfg(all(test, unix))]
    super::test_kill_after_validated_rotation_if_armed();

    let Some(successor) = successor_of(accepted, sent_at, attempt) else {
        // The answer passed every check of the wire part but its pair cannot
        // be encoded for the store: it cannot be used, which is row 4.
        return ended(
            ExchangeKind::Failed {
                class: AttemptClass::ConsumedWithoutSuccessor,
                error: LegacyAuthError::InvalidResponse,
                status: Some(status),
                retry_after: None,
            },
            input,
            Some(sent_at),
            server_date,
        );
    };
    let kind = match persist_step(ports.as_ref(), &successor, false) {
        PersistStep::Done => ExchangeKind::Persisted(successor),
        PersistStep::Pending(error) => ExchangeKind::Pending { successor, error },
        PersistStep::External => ExchangeKind::External,
    };
    ended(kind, input, Some(sent_at), server_date)
}

/// Build the successor of an accepted answer: its schedule (R20 to R22) and its
/// generation. For row 3 the new pair is the new access token with the
/// presented refresh token.
fn successor_of(
    accepted: AcceptedTokens,
    sent_at: i64,
    attempt: Attempt,
) -> Option<Box<SuccessorPair>> {
    let tokens = crate::credentials::OwnerTokens::from_secret_parts(
        accepted.access_token.to_string(),
        accepted.refresh_token.to_string(),
    )
    .ok()?;
    let output = crate::teslamate_token::legacy_refresh_credential_generation(&tokens);
    // A rotated successor has a refresh token of its own, so a different
    // generation (K2); one that has the input's generation is not one.
    //
    // One that kept the refresh token has, in general, the input's generation,
    // and is then written under it. It may also have a different one (C5-S5):
    // the presented refresh token is the copy trimmed at load (R46) while the
    // input generation is that of the stored, untrimmed bytes, so a pasted token
    // with a trailing newline keeps its token but not its digest. The pair is
    // then a changed generation, and is completed like a rotation (K2: the output
    // differs from the input), which also stores the token without the padding.
    // The token itself was not consumed (F6) either way.
    if accepted.kind == AcceptedKind::Rotated && output == attempt.input {
        return None;
    }
    let schedule = Schedule::after_send(
        sent_at,
        accepted.expires_in.as_ref(),
        &accepted.access_token,
    );
    Some(Box::new(SuccessorPair {
        access_token: accepted.access_token.clone(),
        refresh_token: accepted.refresh_token.clone(),
        kind: accepted.kind,
        schedule,
        sent_at,
        input: attempt.input,
        output,
        attempt,
    }))
}

/// What one try at writing a successor came to.
#[derive(Debug)]
pub(crate) enum PersistStep {
    /// The row holds the successor: the pair may be used (R23).
    Done,
    /// The write is not done; it is retried (R24). The error says why.
    Pending(LegacyAuthError),
    /// An external writer changed the row: the successor is discarded (R24).
    External,
}

/// Write the successor (R23, R24, R62). With `reread_first` the pair row is read
/// before the write, as every retry does: a row that already holds the
/// successor's generation means an earlier write that reported an error had in
/// fact committed, so nothing is written twice; a generation that is neither the
/// input nor the successor means an external writer resolved the attempt (K4).
///
/// The guard is checked before the write: if sensitive access has been lost the
/// write is not tried and the successor stays pending (R45 row 5).
pub(crate) fn persist_step(
    ports: &dyn AttemptPorts,
    successor: &SuccessorPair,
    reread_first: bool,
) -> PersistStep {
    if reread_first {
        match ports.reread(&successor.attempt, successor.output) {
            // A successor that kept the refresh token has the generation of its
            // input (the generation identifies the refresh token), so the row
            // holding that generation proves nothing about its new access
            // token: the write is idempotent and is made again. One whose
            // generation differs from its input's (a rotation, or a token that
            // lost its padding, C5-S5) is proved by the row holding it.
            PersistReread::AlreadyPersisted
                if successor.kind == AcceptedKind::Rotated
                    || successor.output != successor.input =>
            {
                return PersistStep::Done;
            }
            // CL-14: the row is the one this attempt wrote and the attempt's own
            // receipt is closed as a success, so the pair write and the closing both
            // committed: nothing is written a second time.
            PersistReread::ClosedForThisAttempt => return PersistStep::Done,
            PersistReread::External => return PersistStep::External,
            PersistReread::AlreadyPersisted
            | PersistReread::StillInput
            | PersistReread::Missing => {}
        }
    }
    if ports.sensitive_access().is_err() {
        return PersistStep::Pending(LegacyAuthError::SensitivePersistenceUnavailable);
    }
    match ports.persist(&successor.attempt, successor) {
        Ok(()) => PersistStep::Done,
        Err(PortError) => PersistStep::Pending(LegacyAuthError::Persistence),
    }
}

/// The single-flight object of one account (R15, R15a): at most one exchange in
/// flight, a published result for the next holder of the manager, and a way for
/// any number of callers to wait for the same outcome.
#[derive(Clone)]
pub(crate) struct Flight {
    shared: Arc<FlightShared>,
}

struct FlightShared {
    state: Mutex<FlightState>,
    changed: Notify,
}

enum FlightState {
    Idle,
    Running,
    Finished(Box<ExchangeResult>),
}

impl Default for Flight {
    fn default() -> Self {
        Self {
            shared: Arc::new(FlightShared {
                state: Mutex::new(FlightState::Idle),
                changed: Notify::new(),
            }),
        }
    }
}

impl Flight {
    fn lock(&self) -> MutexGuard<'_, FlightState> {
        // A poisoned lock still holds a consistent enum: the guard below
        // publishes a result even when the task panics.
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether an exchange is running (R16 gate, R40 "remedy pending").
    pub(crate) fn is_running(&self) -> bool {
        matches!(*self.lock(), FlightState::Running)
    }

    /// Start the exchange in its own task. Returns `false`, and starts nothing,
    /// while one is running or its result has not been applied (R16).
    pub(crate) fn start(&self, input: ExchangeInput) -> bool {
        {
            let mut state = self.lock();
            if !matches!(*state, FlightState::Idle) {
                return false;
            }
            *state = FlightState::Running;
        }
        let guard = FinishGuard {
            flight: self.clone(),
            input: input.generation,
            env: Arc::clone(&input.env),
            finished: false,
        };
        tokio::spawn(async move {
            let mut guard = guard;
            let result = run_exchange(input).await;
            guard.finish(result);
        });
        true
    }

    /// Take the published result, leaving the flight idle.
    pub(crate) fn take_result(&self) -> Option<Box<ExchangeResult>> {
        let mut state = self.lock();
        match std::mem::replace(&mut *state, FlightState::Idle) {
            FlightState::Finished(result) => Some(result),
            other => {
                *state = other;
                None
            }
        }
    }

    /// Wait until no exchange is running (R15). The wait has no timer of its
    /// own: the exchange is bounded by the auth client's total timeout (R33,
    /// 60 s), so every waiter is too, and a second timer of the same length
    /// would only race the first. Dropping this future leaves the exchange
    /// running (R15a).
    pub(crate) async fn wait(&self) {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.is_running() {
                return;
            }
            notified.await;
        }
    }

    fn publish(&self, result: Box<ExchangeResult>) {
        *self.lock() = FlightState::Finished(result);
        self.shared.changed.notify_waiters();
    }
}

/// Publishes a result if the exchange task ends without one (a panic): the
/// request may have been sent, so the outcome is unknown (row 6) and waiters
/// are released instead of waiting for ever.
struct FinishGuard {
    flight: Flight,
    input: Uuid,
    env: Arc<dyn AuthEnv>,
    finished: bool,
}

impl FinishGuard {
    fn finish(&mut self, result: ExchangeResult) {
        self.finished = true;
        self.flight.publish(Box::new(result));
    }
}

impl Drop for FinishGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.flight.publish(Box::new(ExchangeResult {
            kind: ExchangeKind::Failed {
                class: AttemptClass::Unknown,
                error: LegacyAuthError::RotationOutcomeUnknown,
                status: None,
                retry_after: None,
            },
            input: self.input,
            sent_at: None,
            finished_at: self.env.now(),
            server_date: None,
            pending_close: None,
        }));
    }
}
