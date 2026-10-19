// SPDX-License-Identifier: AGPL-3.0-only

//! The manager's side of the rejection handling and of the collector boundary
//! (C5 part P4: R28, R39, R40, R41, R41a, R42, R44, R44a, R49, R55 and R57 step
//! 6).
//!
//! The fuse (`rejection.rs` of `legacy_auth`) decides what a 401 means. The
//! wrappers in `owner_api.rs` make the requests and carry out what the fuse
//! decides. This file is what they need from the manager to do it: the facts the
//! fuse is given, the way the fuse and the manager keep one another in step, the
//! stops and waits a decision sets, the question the collector asks before it
//! sends a request, the one refresh a refusal check asks for, and the account
//! state the collector's own start path reports.

use super::*;

/// What the collector may do with the next request (C5 R39).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthWait {
    /// A token can be handed out now.
    Ready,
    /// Send nothing, count nothing and fail nothing before this long has passed.
    For(Duration),
    /// An exchange is in flight: the request is sent after it, and is bounded by
    /// the exchange's own timeout.
    UntilExchangeEnds { at_most: Duration },
    /// A stop is set or the configuration is wrong: the collector stops.
    Stopped,
}

/// The shortest wait the collector is told (R39): it keeps a due time that has
/// just passed from spinning.
pub(crate) const WAIT_FLOOR_SECONDS: i64 = 1;

/// The longest wait the collector is told (R39): the longest any rule here sets
/// (R32, and the `Retry-After` ceiling of R31), so a longer value can only come
/// from a clock jump, and the clamp bounds its effect.
pub(crate) const WAIT_CEILING_SECONDS: i64 = 3_600;

/// How a refusal-check refresh ended (R49).
#[derive(Debug)]
pub(crate) enum RefusalRefresh {
    /// The pair is newer than the one the refused request carried, either
    /// because an exchange just ran or because one already had. The generation
    /// is the pair now in use (R49, test 61); the collector resends its list with
    /// whatever pair the unit holds, so only the tests read it.
    Refreshed(#[allow(dead_code)] uuid::Uuid),
    /// Observer mode refreshes nothing and changes no state.
    ObserverRefreshDisabled,
    /// The pair has no refresh path: an access token only, or an account in
    /// `sign_in_required` (R43).
    RefreshNotPossible,
    /// The refresh failed; the failure is classified by its own row of the
    /// outcome table, never as a refusal.
    Failed(LegacyAuthManagerError),
}

/// What the collector's own start path reports (C3 R4, R7; the B2 clarification
/// of 2026-10-12): whether it reset a refusal for a mode or fingerprint change,
/// and the account row as it stands after that reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CollectorStartView<'a> {
    /// C3's start path reset a refusal for a mode or fingerprint change.
    pub reset_refusal: bool,
    /// `(mode, state, reason_code)` of `collector_account_state`, if the row
    /// exists.
    pub account: Option<(&'a str, &'a str, &'a str)>,
}

/// Whether `state` of C3's account row is a refusal this unit holds as well.
fn is_c3_refusal_state(state: &str) -> bool {
    matches!(state, "owner_api_refused" | "provider_refused")
}

impl LegacyAuthManager {
    /// The unit's clock (R37): the one every decision reads, so a test fixes it.
    pub(crate) fn clock(&self) -> i64 {
        self.env.now()
    }

    /// The column of the table of R41 this holder falls in.
    pub(crate) fn holder(&self) -> HolderKind {
        if self.refresh_policy == LegacyAuthRefreshPolicy::Observer {
            HolderKind::Observer
        } else if self.auth.token_endpoint().is_none() {
            HolderKind::AccessTokenOnly
        } else {
            HolderKind::Managed
        }
    }

    /// The access token in use, for the one comparison the wrappers make: a 401
    /// to a token that is no longer the current one says nothing about the pair
    /// in use (R40 step 1), even when a refresh that kept the refresh token left
    /// the generation unchanged (R45 row 3).
    pub(crate) fn current_access_token(&self) -> &str {
        self.auth.access_token()
    }

    /// The access token for scrubbing a provider's answer of anything that echoes
    /// it: behind the same admission as every token use, but not subject to the
    /// waits of R29, because an answer that has arrived is scrubbed whatever the
    /// clock says by then.
    pub(crate) fn access_token_for_scrubbing(&self) -> Result<&str, LegacyAuthManagerError> {
        self.assert_sensitive_access()?;
        Ok(self.auth.access_token())
    }

    /// What the fuse is given when a 401 arrives (section 3.2).
    ///
    /// An observer never has a remedy pending: it refreshes nothing, so a retry
    /// or a startup refresh that was never owed to it must not make its
    /// rejection uncountable (R28, R41).
    pub(crate) fn rejection_context(
        &self,
        scope: RequestScope,
        carried: uuid::Uuid,
        now: i64,
    ) -> RejectionContext {
        let holder = self.holder();
        let sign_in = match self.auth.stop {
            Some(AuthStop::SignInRequired(reason)) => Some(reason),
            _ => None,
        };
        RejectionContext {
            scope,
            generation: carried,
            current: self.generation(),
            token_usable: clock_is_valid(now) && self.auth.is_usable(now),
            remedy_pending: holder != HolderKind::Observer
                && (self.auth.remedy_pending() || self.flight.is_running()),
            refresh_possible: holder == HolderKind::Managed
                && sign_in.is_none()
                && self.auth.stop.is_none(),
            holder,
            access_digest: crate::teslamate_token::legacy_access_token_digest(
                self.auth.access_token(),
            ),
            sign_in,
        }
    }

    /// Keep the fuse object and the manager in step (R42, R52a, R57 step 7). The
    /// fuse is the counting and the manager is the status and the pause:
    ///
    /// - the first fuse to ask receives the fuse restored at start, so a crash
    ///   loop cannot evade it (C5R-13);
    /// - an external credential write or the owner's retry reset the fuse (R42),
    ///   and the pause that the forced refresh ended is spent;
    /// - the pause gates requests, and any change of the stored values is
    ///   written (R52a).
    pub(crate) fn sync_fuse(&mut self, fuse: &mut LegacyAuthFuse) {
        if self.fuse_reset_pending {
            fuse.reset();
            self.fuse_reset_pending = false;
            self.fuse_handoff = None;
            self.fuse = FuseSnapshot::default();
        }
        if let Some(restored) = self.fuse_handoff.take()
            && fuse.snapshot() == FuseSnapshot::default()
        {
            *fuse = LegacyAuthFuse::from_snapshot(restored);
        }
        if self.fuse_pause_spent {
            fuse.clear_pause();
            self.fuse_pause_spent = false;
        }
        fuse.note_stop(self.auth.stop.is_some());
        let snapshot = fuse.snapshot();
        let changed = snapshot != self.fuse;
        // R41: a pause that begins now owes the forced refresh that follows it (a
        // pair that cannot refresh confirms with its next request instead).
        if snapshot.pause_until.is_some()
            && snapshot.pause_until != self.fuse.pause_until
            && self.holder() == HolderKind::Managed
        {
            self.auth.forced_after_hold = true;
        }
        self.fuse = snapshot;
        // The hold is the pause of count 2 or, while a restored count-1 fuse still owes
        // its refresh, the time that refresh may run (B2 apply, R3C-01), whichever is
        // later.
        let owed_hold = (self.auth.forced_after_hold && self.owed_refresh_not_before > 0)
            .then_some(self.owed_refresh_not_before);
        self.auth.hold_until = match (snapshot.pause_until, owed_hold) {
            (Some(pause), Some(owed)) => Some(pause.max(owed)),
            (pause, owed) => pause.or(owed),
        };
        if changed {
            let now = self.env.now();
            self.publish_status(now);
        }
    }

    /// The error a request that cannot be sent now reports (R29, R40 steps 2 and
    /// 3, R41): `AuthUnavailable` with the time the unit will next be ready, when
    /// it knows one.
    pub(crate) fn unavailable_error(&self, now: i64) -> LegacyAuthError {
        let retry_at = self
            .auth
            .hold_until
            .filter(|until| now < *until)
            .or(self.auth.persist_retry_at)
            .or(self.auth.retry_at);
        LegacyAuthError::AuthUnavailable { retry_at }
    }

    /// R41 count 3, R8a, R41's sign-in rule: the account is signed out. The stop
    /// is set in memory first and written in the same step (R43).
    pub(crate) fn stop_account(&mut self, reason: SignOutReason) {
        let now = self.env.now();
        self.auth.set_stop(AuthStop::SignedOut(reason));
        self.auth.hold_until = None;
        self.auth.forced_after_hold = false;
        self.publish_status(now);
    }

    /// R28: an observer's counted rejection. Nothing is sent until the stored
    /// generation changes.
    pub(crate) fn observer_rejected(&mut self) {
        let now = self.env.now();
        self.auth.observer_rejected = true;
        self.last_stored_check = now;
        self.publish_status(now);
    }

    /// R55: the `Date` header of an Owner API answer, compared with the local
    /// receive time. A change of the flag is written.
    pub(crate) fn note_server_date(&mut self, date: Option<&str>) {
        let now = self.env.now();
        let skew = skew_after_answer(self.auth.clock_skew, date, now);
        if skew != self.auth.clock_skew {
            self.auth.clock_skew = skew;
            self.publish_status(now);
        }
    }

    /// R40 step 3: wait for the exchange under way, then apply its result.
    /// Returns whether anything changed the pair.
    pub(crate) async fn wait_for_exchange(&mut self) {
        if self.flight.is_running() {
            self.flight.wait().await;
        }
        let now = self.env.now();
        let _ = self.settle(now);
    }

    /// R28: while an observer waits, the stored generation is read every 60 s and
    /// a changed one (an external write) loads the new pair and ends the wait.
    pub(super) fn recheck_waiting_observer(&mut self, now: i64) {
        let waiting = self.auth.observer_rejected || !self.auth.is_usable(now);
        if waiting && now.saturating_sub(self.last_stored_check) >= EXTERNAL_WRITE_RECHECK_SECONDS {
            self.last_stored_check = now;
            if self.adopt_if_stored_pair_changed() {
                let _ = self.settle(self.env.now());
            }
        }
    }

    /// What the unit does for itself between requests (C5 R15a, R24, R43, R52a,
    /// Codex R07): apply what the detached exchange published, try again the
    /// write of a successor or the closing of an attempt that is owed when its
    /// time has come, let time bring on a stop, and write the status (a write
    /// that failed is retried on its own schedule, R43). The
    /// supervisor calls it at the top of every iteration, so none of these waits
    /// for a request: a successor that could not be written is the only valid
    /// refresh token, and a hour of rate-limit waiting must not leave it
    /// unwritten when the store is back after five seconds.
    pub(crate) fn service(&mut self) {
        let now = self.env.now();
        let _ = self.settle(now);
    }

    /// R44a, R39: while stopped or waiting the supervisor reads the stored
    /// generation every 60 s, so a new login lifts a stop without a restart.
    /// Returns whether the pair was replaced.
    pub(crate) fn recheck_external_write(&mut self) -> bool {
        let now = self.env.now();
        if now.saturating_sub(self.last_stored_check) < EXTERNAL_WRITE_RECHECK_SECONDS {
            return false;
        }
        self.last_stored_check = now;
        let changed = self.adopt_if_stored_pair_changed();
        if changed {
            let _ = self.settle(self.env.now());
        }
        changed
    }

    /// The status at `now`, as it would be written (R52): the lease state and
    /// readiness follow from it.
    pub(crate) fn status_now(&self) -> AuthStatus {
        self.current_status(self.env.now())
    }

    /// The error a request meets while the account is stopped for good (R39,
    /// R56): the configuration error, or the stop, or `None` when none is set.
    /// A sign-in stop that still collects (R43) is not one.
    pub(crate) fn stop_view(&self, now: i64) -> Option<LegacyAuthError> {
        if let Some(reason) = self.visible_configuration_error() {
            return Some(configuration_error_of(reason));
        }
        self.auth
            .effective_stop(now)
            .filter(|stop| stop.blocks_requests())
            .map(stop_error)
    }

    /// R39: what the collector may do with its next request. The answer reads
    /// the unit's own state and the time given, sends nothing and changes
    /// nothing.
    pub(crate) fn wait_view(&self, now: i64) -> AuthWait {
        if self.stop_view(now).is_some() {
            return AuthWait::Stopped;
        }
        if self.flight.is_running() {
            return AuthWait::UntilExchangeEnds {
                at_most: EXCHANGE_WAIT_AT_MOST,
            };
        }
        match self.auth.token_available(now) {
            Ok(()) => AuthWait::Ready,
            Err(LegacyAuthError::AuthUnavailable { retry_at }) => {
                // The time is the unit's own (a retry, the pause, the next write
                // of a successor) or one minute on, which is when an observer
                // and an invalid clock look again (R28, R38).
                let at =
                    retry_at.unwrap_or_else(|| now.saturating_add(EXTERNAL_WRITE_RECHECK_SECONDS));
                let seconds = at
                    .saturating_sub(now)
                    .clamp(WAIT_FLOOR_SECONDS, WAIT_CEILING_SECONDS);
                AuthWait::For(Duration::from_secs(u64::try_from(seconds).unwrap_or(1)))
            }
            Err(_) => AuthWait::Stopped,
        }
    }

    /// R49: the one refresh a refusal check asks for after a plain 403 on a read
    /// that carried `carried`. If the pair in use is already newer, no exchange
    /// is started and the current generation is returned; otherwise one forced
    /// refresh runs through the gates (so it waits for R19 or R58 if needed) and
    /// its outcome is returned. Concurrent asks for the same generation reach
    /// the manager one after the other, so the second finds the pair already
    /// replaced and shares the first's exchange. An observer and an
    /// access-token-only pair refresh nothing and change no state.
    pub(crate) async fn refusal_check_refresh(
        &mut self,
        client: &Client,
        carried: Option<uuid::Uuid>,
    ) -> RefusalRefresh {
        match self.holder() {
            HolderKind::Observer => return RefusalRefresh::ObserverRefreshDisabled,
            HolderKind::AccessTokenOnly => return RefusalRefresh::RefreshNotPossible,
            HolderKind::Managed => {}
        }
        let before = self.generation();
        if carried.is_some_and(|carried| carried != before) {
            return RefusalRefresh::Refreshed(before);
        }
        let succeeded_before = self.auth.last_success_at;
        match self.refresh(client, true).await {
            Ok(()) => {
                let after = self.generation();
                if after != before || self.auth.last_success_at != succeeded_before {
                    RefusalRefresh::Refreshed(after)
                } else {
                    // Nothing was sent (shutdown began, or a stop appeared).
                    RefusalRefresh::Failed(
                        LegacyAuthError::AuthUnavailable { retry_at: None }.into(),
                    )
                }
            }
            // No refresh is possible for a pair in `sign_in_required` either (R43,
            // C5-S2): the same answer as for an access token only, and no state
            // changes.
            Err(LegacyAuthManagerError::Auth(LegacyAuthError::RefreshNotPossible)) => {
                RefusalRefresh::RefreshNotPossible
            }
            Err(error) => RefusalRefresh::Failed(error),
        }
    }

    /// R57 step 6 and the B2 clarification: what the collector's start path found
    /// and did, applied after its transaction committed and before the first
    /// request. The manager is built before that path runs, so its restore (step
    /// 5 (b)) saw the account row as the previous run left it, and a refusal it
    /// dropped for another mode is still in the store until this runs (the
    /// start's helper lifted it, or this call writes the status).
    ///
    /// - When the start lifted a refusal (`reset_refusal`, the three-condition
    ///   decision of C3 R7), a refusal this unit restored is cleared and the
    ///   status written.
    /// - When the unit restored no refusal and C3's row holds one for the
    ///   configured mode, it is restored here, with its reason (`client` when
    ///   C3's reason names a client refusal, else `owner_api`) and the mode
    ///   (R43), so step 5 (b) holds it from then on. A row for another mode is
    ///   not restored: the owner changed the mode (R44 (c)). A `signed_out` stop
    ///   is kept, and nothing is written, unless an unresolved refresh attempt
    ///   exists (a sign-in stop is always open to the overlay); an unreadable
    ///   fact keeps the sign-out. A refusal this unit holds itself keeps its
    ///   precedence.
    pub(crate) fn apply_collector_start(&mut self, start: &CollectorStartView<'_>) {
        let now = self.env.now();
        let refused = matches!(self.auth.stop, Some(AuthStop::ApiRefused(_)));
        if start.reset_refusal {
            // The start lifted the refusal the constructor left in the store.
            self.retained_refusal = None;
            // R44 (c), (d): a mode or fingerprint change lifts a refusal, and only a
            // refusal; whatever else stops the account stays.
            if self.auth.stop.is_some_and(AuthStop::lifted_by_mode_change) {
                self.auth.stop = None;
                self.auth.clear_retry();
                self.settle_after_lift(now);
            } else {
                self.publish_status(now);
            }
            return;
        }
        if refused {
            return;
        }
        let kind = start.account.and_then(|(mode, state, reason)| {
            (is_c3_refusal_state(state) && mode == StoppedMode::of(self.mode).code()).then(|| {
                if reason.ends_with("client_refused") {
                    RefusalReason::Client
                } else {
                    RefusalReason::OwnerApi
                }
            })
        });
        let Some(kind) = kind else {
            self.publish_status(now);
            return;
        };
        // R57 step 6: a sign-out stands unless the unresolved attempt that
        // explains it is still there; an unreadable fact is no evidence.
        if matches!(self.auth.stop, Some(AuthStop::SignedOut(_)))
            && !self.has_unresolved_legacy_refresh()
        {
            return;
        }
        self.auth.set_stop(AuthStop::ApiRefused(kind));
        self.publish_status(now);
    }

    /// C3 R48, R49 and C5 R43 (B2 clarification 3): the admission of a resident
    /// action. A matching standing C3 refusal (`owner_api_refused` or
    /// `provider_refused` for the configured mode), or a failure to read the
    /// account row, rejects a new action before any authentication is prepared or
    /// token is handed out. It changes no C5 state and cancels nothing already
    /// sent: the check reads one row and returns. A unit without a Hub store has no
    /// C3 row to consult.
    pub(crate) fn assert_resident_admission(&self) -> Result<(), LegacyAuthManagerError> {
        let Some(persistence) = self.persistence.hub() else {
            return Ok(());
        };
        match persistence.store.collector_account_stop_view() {
            Ok(Some((mode, state, reason)))
                if is_c3_refusal_state(&state) && mode == StoppedMode::of(self.mode).code() =>
            {
                Err(LegacyAuthError::OwnerApiRefused {
                    reason: if reason.ends_with("client_refused") {
                        RefusalReason::Client
                    } else {
                        RefusalReason::OwnerApi
                    },
                }
                .into())
            }
            Ok(_) => Ok(()),
            Err(_) => Err(LegacyAuthError::AuthUnavailable { retry_at: None }.into()),
        }
    }

    /// Whether a fencing attempt is unresolved in the store; false when it cannot
    /// be read (R57 step 6) or the unit has no Hub store.
    pub(super) fn has_unresolved_legacy_refresh(&self) -> bool {
        self.persistence.hub().is_some_and(|persistence| {
            persistence
                .store
                .has_unresolved_legacy_refresh()
                .unwrap_or(false)
        })
    }
}
