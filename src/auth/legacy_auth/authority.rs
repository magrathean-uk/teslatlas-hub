// SPDX-License-Identifier: AGPL-3.0-only

//! What an attempt means for the in-memory authority (C5 R14 to R16, R19, R23,
//! R24, R27, R29, R29a, R31, R32, R43, R45; sections 5.1 and 5.4): the gates
//! that keep a token request from starting, the due rule, what a published
//! exchange result does to the pair, the schedule and the stop, and the status
//! the authority is in.
//!
//! Nothing here awaits, reads a clock or touches the store. Every function takes
//! the time as an argument, so a test can fix it exactly.

use uuid::Uuid;

use super::{
    LegacyAuth, LegacyAuthError,
    attempt::{BeginFailure, ExchangeKind, ExchangeResult, PendingClose, SuccessorPair},
    rejection::HolderKind,
    schedule::{CLIENT_REFUSAL_LIMIT, RetryPlan, plan_for_reason, refreshed_form_due, retry_delay},
    state::{
        AuthState, AuthStatus, AuthStop, CLOCK_RECHECK_SECONDS, FuseSnapshot,
        MIN_REFRESH_INTERVAL_SECONDS, ObserverReason, PERSISTENCE_RETRY_SECONDS, StatusReason,
        StoppedMode, clock_is_valid, skew_after_answer,
    },
    vocabulary::{RefusalReason, RetryReason, SignOutReason},
    wire::AttemptClass,
};

/// Why a token request may not start now (R16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    /// An exchange is in flight, or its result is not applied yet.
    InFlight,
    /// A validated successor is not yet persisted (R24).
    SuccessorPending,
    /// An attempt's closing failed and is still owed (C5-S1): its receipt is
    /// `started`, so the reservation of another attempt would be refused.
    ClosePending,
    /// A stop is set (R43).
    Stopped,
    /// The pair has no refresh path: an access-token-only provider (R8a).
    NoRefreshPath,
    /// The host clock is below the floor (R38).
    ClockInvalid,
    /// A time gate: the retry time (R31), the minimum interval (R19), the
    /// restart floor (R58) or the confirmation pause (R41). The value is the
    /// epoch second at which every one of them has opened.
    Until(i64),
}

/// What applying a published result asks of the manager.
#[derive(Debug)]
pub(crate) enum Applied {
    /// The result changed the authority in place. The error, if any, is what
    /// the attempt reports to its caller.
    Done(Option<LegacyAuthError>),
    /// The stored pair changed under the unit and must be adopted from the
    /// store (R44a, R62). `external` is false for a refresh the unit itself
    /// produced.
    Adopt { external: bool },
}

impl LegacyAuth {
    /// R29: the access token is sent only while `usable_until` is unknown or has
    /// not passed.
    pub(crate) fn is_usable(&self, now: i64) -> bool {
        self.usable_until.is_none_or(|until| now < until)
    }

    /// The stop in force at `now`, with the two stops that follow from time
    /// applied (R45, R8a): a `sign_in_required` whose token has passed
    /// `usable_until` is `signed_out` with the same reason, and an
    /// access-token-only pair whose token has passed it is
    /// `signed_out(provider_token_expired)`.
    pub(crate) fn effective_stop(&self, now: i64) -> Option<AuthStop> {
        match self.stop {
            Some(AuthStop::SignInRequired(reason)) if !self.is_usable(now) => {
                Some(AuthStop::SignedOut(reason))
            }
            None if self.token_endpoint.is_none() && !self.is_usable(now) => {
                Some(AuthStop::SignedOut(SignOutReason::ProviderTokenExpired))
            }
            stop => stop,
        }
    }

    /// Store the stop that [`effective_stop`](Self::effective_stop) shows, so a
    /// stop that time brought on is a stop from then on (R43).
    pub(crate) fn settle_stop(&mut self, now: i64) {
        if let Some(stop) = self.effective_stop(now) {
            self.stop = Some(stop);
        }
    }

    /// Whether a refresh is under way or owed in a way a 401 must not be
    /// counted against (R40 step 2): a retry pending, a startup refresh
    /// pending, or a successor not yet persisted. An exchange in flight is the
    /// manager's to add.
    pub(crate) fn remedy_pending(&self) -> bool {
        self.retry_at.is_some() || self.startup_refresh_pending || self.pending_successor.is_some()
    }

    /// R16: why a token request may not start now, or `None` when it may.
    pub(crate) fn gate(&self, now: i64, in_flight: bool) -> Option<Block> {
        if in_flight {
            return Some(Block::InFlight);
        }
        if self.pending_successor.is_some() {
            return Some(Block::SuccessorPending);
        }
        if self.pending_close.is_some() {
            return Some(Block::ClosePending);
        }
        if self.effective_stop(now).is_some() {
            return Some(Block::Stopped);
        }
        if self.token_endpoint.is_none() {
            return Some(Block::NoRefreshPath);
        }
        if !clock_is_valid(now) {
            return Some(Block::ClockInvalid);
        }
        // Every time gate that still holds. A retry time is the gate of R31;
        // the interval after the last success is R19's (in this process) and
        // the restart floor is R58's (across restarts); a confirmation pause is
        // R41's, and the refresh that follows the pause is allowed once it ends.
        let gates = [
            self.retry_at,
            self.last_success_at
                .map(|sent| sent.saturating_add(MIN_REFRESH_INTERVAL_SECONDS)),
            Some(self.token_requests_not_before),
            self.hold_until,
        ];
        gates
            .into_iter()
            .flatten()
            .filter(|until| *until > now)
            .max()
            .map(Block::Until)
    }

    /// Whether a refresh is due (R26, R27, R29a, R31). A startup refresh is owed
    /// to an imported pair until one succeeds; a retry is due once its time is
    /// reached (the gate holds it until then); a pair of the refreshed form is
    /// due by R27.
    pub(crate) fn is_due(&self, now: i64) -> bool {
        self.startup_refresh_pending
            || self.retry_at.is_some()
            // A token that is past its usability and can be refreshed is owed a
            // refresh whatever the stored schedule says: a row whose `next_refresh_at`
            // lies beyond the token's own `exp` (R25) would otherwise leave the
            // account without a usable token until the schedule came.
            || (self.token_endpoint.is_some() && !self.is_usable(now))
            || (self.expires_at > 0
                && refreshed_form_due(self.expires_at, self.next_refresh_at, now))
    }

    /// R29 and R43: whether the access token may be handed out now, or the
    /// reason it may not. The check reads no clock and changes nothing.
    pub(crate) fn token_available(&self, now: i64) -> Result<(), LegacyAuthError> {
        // An inert pair holds no token (C5-B2): the stop that made it inert
        // already refuses, and this is the last word if one did not.
        if self.access_token.is_empty() {
            return Err(LegacyAuthError::AuthUnavailable { retry_at: None });
        }
        if let Some(stop) = self.effective_stop(now)
            && stop.blocks_requests()
        {
            return Err(stop_error(stop));
        }
        if !clock_is_valid(now) {
            return Err(LegacyAuthError::AuthUnavailable {
                retry_at: Some(now.saturating_add(CLOCK_RECHECK_SECONDS)),
            });
        }
        // R41: requests and stream admissions pause while the confirmation
        // pause runs.
        if let Some(until) = self.hold_until.filter(|until| now < *until) {
            return Err(LegacyAuthError::AuthUnavailable {
                retry_at: Some(until),
            });
        }
        // R28: an observer whose token was rejected sends nothing until the
        // refresh owner's next pair is stored.
        if self.observer_rejected {
            return Err(LegacyAuthError::AuthUnavailable { retry_at: None });
        }
        if !self.is_usable(now) {
            return Err(LegacyAuthError::AuthUnavailable {
                retry_at: self.persist_retry_at.or(self.retry_at),
            });
        }
        Ok(())
    }

    /// Schedule the next attempt after a retryable outcome (R31, R32): the delay
    /// of `plan` counted from `now`, the reason shown while it waits, and the
    /// step that follows.
    pub(crate) fn schedule_retry(
        &mut self,
        reason: RetryReason,
        plan: RetryPlan,
        now: i64,
        spread_unit: f64,
    ) {
        let (delay, next_step) = retry_delay(plan, self.retry_step, spread_unit);
        self.retry_at = Some(now.saturating_add(delay));
        self.retry_reason = Some(reason);
        self.retry_step = next_step;
    }

    /// End a pending retry (R23, R44, R44a): `retry_at` and `retry_reason` are
    /// `Some` exactly while one is pending and both are cleared together.
    pub(crate) fn clear_retry(&mut self) {
        self.retry_at = None;
        self.retry_reason = None;
        self.retry_step = 0;
    }

    /// R23: the successor has been written, so it becomes the pair in use.
    ///
    /// The pair, the stored times, `usable_until` and the time of the last
    /// success change only now, after the write returned `Ok`. A successor that
    /// cannot be renewed before it expires (R29a) is persisted as usual and
    /// then retried on the R31 schedule instead of `next_refresh_at`; the step
    /// is then not reset, so a provider that keeps issuing short tokens is
    /// rotated at most every 300 s (R29a) and not every 60.
    pub(crate) fn apply_persisted(
        &mut self,
        successor: &SuccessorPair,
        now: i64,
        spread_unit: f64,
    ) {
        self.access_token = successor.access_token.clone();
        self.refresh_token = successor.refresh_token.clone();
        self.token_type = "Bearer".to_owned();
        self.expires_at = successor.schedule.expires_at;
        self.next_refresh_at = successor.schedule.next_refresh_at;
        self.usable_until = Some(successor.schedule.usable_until);
        self.last_success_at = Some(successor.sent_at);
        self.startup_refresh_pending = false;
        self.client_refusals = 0;
        self.schedule_diagnostic = None;
        self.pending_successor = None;
        self.persist_failures = 0;
        self.persist_retry_at = None;
        if successor.schedule.is_short(successor.sent_at) {
            let reason = if self.clock_skew {
                RetryReason::ClockSkew
            } else {
                RetryReason::ShortTokenLifetime
            };
            self.schedule_retry(reason, plan_for_reason(reason), now, spread_unit);
        } else {
            self.clear_retry();
        }
    }

    /// R24: hold a validated successor whose write is pending, and when its next
    /// try is due. Retries follow 5, 10, 20, 40, 80, 160 s and then every 300 s,
    /// for the life of the process.
    pub(crate) fn hold_successor(&mut self, successor: Box<SuccessorPair>, now: i64) {
        self.pending_successor = Some(successor);
        self.note_persist_failure(now);
    }

    /// Count one failed write and set the time of the next try (R24).
    pub(crate) fn note_persist_failure(&mut self, now: i64) {
        let step = self
            .persist_failures
            .min(PERSISTENCE_RETRY_SECONDS.len() - 1);
        let delay = i64::try_from(PERSISTENCE_RETRY_SECONDS[step]).unwrap_or(i64::MAX);
        self.persist_retry_at = Some(now.saturating_add(delay));
        self.persist_failures = self.persist_failures.saturating_add(1);
    }

    /// Keep the closing of an attempt that could not be closed (C5-S1) and set
    /// the time of its next try on the schedule of R24, which this shares with
    /// the write of a successor: a brief lock clears within 5 s.
    pub(crate) fn hold_close(&mut self, pending: PendingClose, now: i64) {
        self.pending_close = Some(pending);
        self.note_close_failure(now);
    }

    /// Count one failed closing and set the time of the next try (R24).
    pub(crate) fn note_close_failure(&mut self, now: i64) {
        let step = self.close_failures.min(PERSISTENCE_RETRY_SECONDS.len() - 1);
        let delay = i64::try_from(PERSISTENCE_RETRY_SECONDS[step]).unwrap_or(i64::MAX);
        self.close_retry_at = Some(now.saturating_add(delay));
        self.close_failures = self.close_failures.saturating_add(1);
    }

    /// The closing is done (or was resolved elsewhere).
    pub(crate) fn clear_close(&mut self) {
        self.pending_close = None;
        self.close_failures = 0;
        self.close_retry_at = None;
    }

    /// Whether the pending closing is due for another try.
    pub(crate) fn close_retry_due(&self, now: i64) -> bool {
        self.pending_close.is_some() && self.close_retry_at.is_none_or(|at| now >= at)
    }

    /// Whether the write of the pending successor is due for another try.
    pub(crate) fn persist_retry_due(&self, now: i64) -> bool {
        self.pending_successor.is_some() && self.persist_retry_at.is_none_or(|at| now >= at)
    }

    /// Set a stop (R43). A stop ends any pending retry, because no token
    /// request follows it.
    pub(crate) fn set_stop(&mut self, stop: AuthStop) {
        self.stop = Some(stop);
        self.retry_at = None;
        self.retry_reason = None;
    }

    /// The store could not answer (a busy or failing database): the next
    /// attempt is scheduled on the ordinary backoff, and the error is the wait
    /// every request already understands. It is neither terminal nor a lost
    /// sensitive access, so the collector carries on while the token is usable
    /// (R30) and the run is never ended for a transient store fault.
    pub(crate) fn store_unavailable(&mut self, now: i64, spread_unit: f64) -> LegacyAuthError {
        self.client_refusals = 0;
        self.schedule_retry(
            RetryReason::StoreUnavailable,
            plan_for_reason(RetryReason::StoreUnavailable),
            now,
            spread_unit,
        );
        LegacyAuthError::AuthUnavailable {
            retry_at: self.retry_at,
        }
    }

    /// Apply a published exchange result (sections 5.1 and 5.4, R45).
    ///
    /// `spread_unit` is the random share of R31. The skew flag is updated from
    /// the answer's `Date` first (R55), whatever the outcome.
    pub(crate) fn apply_result(&mut self, result: ExchangeResult, spread_unit: f64) -> Applied {
        let now = result.finished_at;
        self.clock_skew = skew_after_answer(self.clock_skew, result.server_date.as_deref(), now);
        if let Some(pending) = result.pending_close {
            // C5-S1: the closing failed, so no new attempt begins until it is done.
            self.hold_close(pending, now);
        }
        match result.kind {
            ExchangeKind::NotSent { reason } => {
                self.client_refusals = 0;
                self.schedule_retry(reason, plan_for_reason(reason), now, spread_unit);
                Applied::Done(Some(match reason {
                    RetryReason::SensitiveAccess => LegacyAuthError::SensitiveAccessUnavailable,
                    _ => LegacyAuthError::Transport,
                }))
            }
            ExchangeKind::Aborted => Applied::Done(Some(LegacyAuthError::RefreshDeferred)),
            ExchangeKind::RowChanged(BeginFailure::ExternalWrite) | ExchangeKind::External => {
                Applied::Adopt { external: true }
            }
            ExchangeKind::RowChanged(BeginFailure::OwnOutput) => Applied::Adopt { external: false },
            ExchangeKind::RowChanged(BeginFailure::Unresolved) => {
                // K1: an attempt is unresolved, so the outcome of a consumed
                // token is unknown (R57 step 3). Lifted only by an external
                // write (K4).
                let reason = SignOutReason::RefreshOutcomeUnknown;
                self.set_stop(AuthStop::SignedOut(reason));
                Applied::Done(Some(LegacyAuthError::SignedOut { reason }))
            }
            ExchangeKind::RowChanged(BeginFailure::Store) => {
                // The store could not answer, so nothing was reserved and nothing
                // was sent. That is a wait, not a loss of the process's sensitive
                // access (R17, R45 row 1 name only the guard's refusal as one),
                // so the caller is told only to try again, and the reason shown
                // is the honest one.
                Applied::Done(Some(self.store_unavailable(now, spread_unit)))
            }
            ExchangeKind::Persisted(successor) => {
                self.apply_persisted(&successor, now, spread_unit);
                Applied::Done(None)
            }
            ExchangeKind::Pending { successor, error } => {
                self.hold_successor(successor, now);
                // The write is retried and the previous pair stays in use while
                // it is usable (R24); the caller sees no failure.
                let _ = error;
                Applied::Done(None)
            }
            ExchangeKind::Failed {
                class,
                error,
                status,
                retry_after,
            } => self.apply_failure(class, error, status, retry_after, now, spread_unit),
        }
    }

    /// The rows of the outcome table that end in a failure (R45 rows 4, 6 to 14).
    fn apply_failure(
        &mut self,
        class: AttemptClass,
        error: LegacyAuthError,
        status: Option<u16>,
        retry_after: Option<i64>,
        now: i64,
        spread_unit: f64,
    ) -> Applied {
        if class != AttemptClass::ClientRefused {
            self.client_refusals = 0;
        }
        let mut error = error;
        match class {
            AttemptClass::ConsumedWithoutSuccessor => {
                self.set_stop(AuthStop::SignInRequired(
                    SignOutReason::RefreshResponseUnusable,
                ));
            }
            AttemptClass::Unknown => {
                self.set_stop(AuthStop::SignInRequired(
                    SignOutReason::RefreshOutcomeUnknown,
                ));
            }
            AttemptClass::Dead => {
                self.set_stop(AuthStop::SignInRequired(
                    SignOutReason::RefreshTokenRejected,
                ));
            }
            AttemptClass::ClientRefused => {
                self.client_refusals = self.client_refusals.saturating_add(1);
                if self.client_refusals >= CLIENT_REFUSAL_LIMIT {
                    self.set_stop(AuthStop::ApiRefused(RefusalReason::Client));
                    error = LegacyAuthError::OwnerApiRefused {
                        reason: RefusalReason::Client,
                    };
                } else {
                    self.schedule_retry(
                        RetryReason::ClientCheck,
                        RetryPlan::ClientCheck,
                        now,
                        spread_unit,
                    );
                }
            }
            AttemptClass::Refused => {
                self.set_stop(AuthStop::ApiRefused(RefusalReason::OwnerApi));
            }
            AttemptClass::RequestRejected => self.schedule_retry(
                RetryReason::RefreshRequestRejected,
                RetryPlan::Slow,
                now,
                spread_unit,
            ),
            AttemptClass::Misrouted => self.schedule_retry(
                RetryReason::TokenEndpointMisrouted,
                RetryPlan::Slow,
                now,
                spread_unit,
            ),
            AttemptClass::Blocked => self.schedule_retry(
                RetryReason::RefreshBlocked,
                RetryPlan::Backoff,
                now,
                spread_unit,
            ),
            AttemptClass::Transient => {
                // R31: `Retry-After` replaces the delay on a 429 and a 503.
                let plan = match (status, retry_after) {
                    (Some(429 | 503), Some(seconds)) => RetryPlan::RetryAfter(seconds),
                    _ => RetryPlan::Backoff,
                };
                self.schedule_retry(RetryReason::TeslaUnavailable, plan, now, spread_unit);
            }
            AttemptClass::NotRenewed => self.schedule_retry(
                RetryReason::RefreshNotRenewed,
                RetryPlan::Backoff,
                now,
                spread_unit,
            ),
            // These classes never reach a failure.
            AttemptClass::NotSent | AttemptClass::Rotated | AttemptClass::NotRotated => {}
        }
        Applied::Done(Some(error))
    }

    /// The status the authority is in at `now` (R52, section 5.1). Observer
    /// waits and the confirmation pause belong to the rejection handling and
    /// are not derived here.
    pub(crate) fn derive_status(
        &self,
        now: i64,
        generation: Uuid,
        mode: StoppedMode,
        in_flight: bool,
        fuse: FuseSnapshot,
    ) -> AuthStatus {
        let usable = clock_is_valid(now) && self.is_usable(now);
        if let Some(stop) = self.effective_stop(now) {
            return AuthStatus::stopped(stop, generation, mode, usable, fuse, self.clock_skew);
        }
        let idle = |state: AuthState, reason: Option<StatusReason>| AuthStatus {
            state,
            reason,
            collecting: false,
            retry_at: None,
            stopped_generation: None,
            stopped_mode: None,
            fuse,
            clock_skew: self.clock_skew,
        };
        if !clock_is_valid(now) {
            return idle(AuthState::ClockInvalid, None);
        }
        if self.pending_successor.is_some() {
            return idle(AuthState::PersistencePending, None).with_usability(usable);
        }
        if in_flight {
            return AuthStatus {
                state: AuthState::Refreshing,
                collecting: true,
                ..idle(AuthState::Refreshing, None)
            };
        }
        if let (Some(reason), Some(retry_at)) = (self.retry_reason, self.retry_at) {
            return AuthStatus {
                retry_at: Some(retry_at),
                ..idle(AuthState::Retrying, Some(StatusReason::Retry(reason)))
            }
            .with_usability(usable);
        }
        AuthStatus::ready(fuse, self.clock_skew)
    }
}

impl LegacyAuth {
    /// The status the authority is in at `now`, with the two waits the
    /// rejection handling adds to [`derive_status`](Self::derive_status) (R28,
    /// R41): an observer waits when its token was rejected or has expired, and
    /// every other holder waits during the confirmation pause. A stop and an
    /// invalid clock come first, as they do there.
    pub(crate) fn derive_full_status(
        &self,
        now: i64,
        generation: Uuid,
        mode: StoppedMode,
        in_flight: bool,
        fuse: FuseSnapshot,
        holder: HolderKind,
    ) -> AuthStatus {
        let base = self.derive_status(now, generation, mode, in_flight, fuse);
        if matches!(
            base.state,
            AuthState::SignedOut
                | AuthState::SignInRequired
                | AuthState::ApiRefused
                | AuthState::ClockInvalid
        ) {
            return base;
        }
        if holder == HolderKind::Observer {
            // R28, R55: a token past `usable_until` ends observer collection with
            // a visible reason; a known-wrong host clock is named as such.
            let reason = if self.observer_rejected {
                Some(ObserverReason::TokenRejected)
            } else if !self.is_usable(now) {
                Some(if self.clock_skew {
                    ObserverReason::ClockSkew
                } else {
                    ObserverReason::TokenExpired
                })
            } else {
                None
            };
            if let Some(reason) = reason {
                return AuthStatus::observer_waiting(reason, fuse, self.clock_skew);
            }
            return base;
        }
        if self.hold_until.is_some_and(|until| now < until) {
            return AuthStatus::awaiting_confirmation(fuse, self.clock_skew);
        }
        base
    }
}

/// The error a stop reports to a request (R56): the dedicated variant where the
/// vocabulary has one, `SignedOut` for the other reasons.
///
/// `sign_in_required` stops token requests only (R43) and sets no terminal error
/// (R56: terminal exactly for the errors that set `configuration_error`,
/// `signed_out` or `api_refused`), so what a request that wants a token request
/// from it is told is that none is possible, which is a wait for the caller
/// (C5-S2). It becomes `signed_out` with the same reason when its token passes
/// `usable_until` or a rejection is counted, and the error is then the
/// sign-out's.
pub(crate) fn stop_error(stop: AuthStop) -> LegacyAuthError {
    match stop {
        AuthStop::ApiRefused(reason) => LegacyAuthError::OwnerApiRefused { reason },
        AuthStop::SignInRequired(_) => LegacyAuthError::RefreshNotPossible,
        AuthStop::SignedOut(reason) => match reason {
            SignOutReason::AccessTokenRejected => LegacyAuthError::AccessTokenRejected,
            SignOutReason::ProviderTokenRejected => LegacyAuthError::ProviderTokenRejected,
            SignOutReason::ProviderTokenExpired => LegacyAuthError::ProviderTokenExpired,
            other => LegacyAuthError::SignedOut { reason: other },
        },
    }
}
