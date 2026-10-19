// SPDX-License-Identifier: AGPL-3.0-only

//! The rejection fuse of the legacy credential unit (C5 part P4: R8a, R28, R40,
//! R41, R41a, R42, R61; section 3.2 and the fuse table of section 5.3).
//!
//! A 401 from the Owner API says one of several things, and the unit counts a
//! rejection only when it is new evidence that the account refuses the token it
//! holds. This file decides that, with no clock, no network and no store: every
//! function takes the time and the facts as arguments, so a test can fix them
//! exactly. What the answer means for requests, tokens and status (waiting for
//! an exchange, forcing a refresh, the confirmation read, the stop) is done
//! where the request is made, from the [`RejectionAction`] returned here.
//!
//! The fuse is not the stop. The stop lives on the authority, so every path sees
//! one (R43); the fuse holds the counting (R41), the pause (R41), the marks of
//! cars whose own 401 an account-wide read disproved (R41a) and the memory of a
//! stream rejection already disproved (R40).

use std::{collections::HashMap, fmt, sync::Arc};

use uuid::Uuid;

use super::{
    state::{FUSE_LIMIT, FuseSnapshot, rejection_window_open},
    vocabulary::SignOutReason,
};
use crate::owner_api::{OwnerApiError, VehicleId};

/// How long requests and stream admissions pause after the second counted
/// rejection, and after the first of an access-token-only provider (R41, R8a).
/// The pause gives any propagation of a new token through the service time to
/// complete, and it outlasts a short Owner API fault: the third rejected
/// generation then means two tokens, each minted by a successful refresh at least
/// five minutes apart, were refused account-wide.
pub(crate) const REJECTION_PAUSE_SECONDS: i64 = 300;

/// The least time between two confirmation reads (R41a). It bounds the traffic a
/// 401 storm can add to one list request a minute, whatever the number of cars,
/// and equals the minimum refresh interval so the two cannot be asked to move
/// faster than each other.
pub(crate) const CONFIRMATION_SPACING_SECONDS: i64 = 60;

/// What the rejected request was (R40): the account-wide read, one car's read or
/// probe, or one car's resident action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestScope {
    /// The vehicle list, the account-wide read of R41a.
    Account,
    /// A read, probe or state request about one car.
    Vehicle(VehicleId),
    /// A resident action for one car. It is never re-sent (a repeated
    /// mutation after an ambiguous answer would be unsafe), so every action that
    /// would re-send it fails with `AuthUnavailable` instead.
    Action(VehicleId),
}

impl RequestScope {
    /// The car this request was about, if it was about one.
    pub(crate) const fn vehicle(self) -> Option<VehicleId> {
        match self {
            Self::Account => None,
            Self::Vehicle(vehicle) | Self::Action(vehicle) => Some(vehicle),
        }
    }
}

/// Who holds the credential, which decides what a counted rejection does (the
/// columns of the table of R41).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HolderKind {
    /// The Hub is the refresh owner (R14).
    Managed,
    /// The source system still refreshes; the Hub never does (R14, R28).
    Observer,
    /// A provider pair with no refresh token (R8a): nothing can be refreshed.
    AccessTokenOnly,
}

/// What the unit knows about itself when a 401 arrives (section 3.2). The first
/// six fields are the specification's; `holder` and `sign_in` are the two facts
/// the specification's table of R41 needs and a boolean cannot carry: which
/// column of the table applies, and whether the account is already in
/// `sign_in_required` (R41, last sentence of the reasons).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct RejectionContext {
    /// What the rejected request was.
    pub scope: RequestScope,
    /// The generation it carried.
    pub generation: Uuid,
    /// The unit's current generation.
    pub current: Uuid,
    /// R29 at the time of the answer.
    pub token_usable: bool,
    /// A refresh in flight, a retry pending, a startup refresh pending, or a
    /// successor not yet persisted.
    pub remedy_pending: bool,
    /// Managed and not access-token-only, and not in `sign_in_required`.
    pub refresh_possible: bool,
    /// The column of the table of R41.
    pub holder: HolderKind,
    /// The digest of the access token the unit holds, which is the one the request
    /// carried when the 401 is counted (hub fix pass 3).
    pub access_digest: Uuid,
    /// The account is in `sign_in_required` with this reason: no refresh is
    /// possible, so any counted rejection stops it at once (R41, R45).
    pub sign_in: Option<SignOutReason>,
}

impl fmt::Debug for RejectionContext {
    /// Generations and the access digest identify tokens (R53): only whether the request
    /// carried the current generation is shown.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RejectionContext")
            .field("scope", &self.scope)
            .field("carried_current", &(self.generation == self.current))
            .field("token_usable", &self.token_usable)
            .field("remedy_pending", &self.remedy_pending)
            .field("refresh_possible", &self.refresh_possible)
            .field("holder", &self.holder)
            .field("sign_in", &self.sign_in)
            .finish()
    }
}

/// What the request that met a 401 does next (section 3.2, R40, R41).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RejectionAction {
    /// R40 step 1: the pair has been replaced; re-send once with the current one.
    RetryWithCurrent,
    /// R40 step 2: not evidence; fail with `AuthUnavailable`.
    NotCounted,
    /// R40 step 3: already counted; wait for the action under way.
    Coalesced,
    /// R40 step 4, R41a: the account-wide read disproved this car's 401.
    VehicleRejected(VehicleId),
    /// R40 step 5 and the stream case: one account-wide read decides (R41a).
    Confirm,
    /// R41 count 1, managed: force a refresh, then re-send once.
    ForceRefreshThenRetry,
    /// R41 count 2, managed: pause, then force a refresh.
    PauseThenRefresh { until: i64 },
    /// R8a, access-token-only count 1: pause, then one confirmation read.
    PauseThenConfirm { until: i64 },
    /// R41 count 3, R8a, R41 sign-in rule: the account is signed out.
    Stop(SignOutReason),
    /// R28: an observer waits for the refresh owner's next pair.
    ObserverWait,
}

/// The one confirmation read in flight, and when the last one started (R41a).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConfirmationState {
    /// Epoch second at which the last confirmation read started.
    pub last_started_at: Option<i64>,
    /// A confirmation read is being made. With the account's locks held across
    /// a request this is never seen from outside, but a trigger that arrives
    /// while one is in flight must wait for it, not start another (R41a).
    pub in_flight: bool,
}

/// The rejection fuse (section 3.2). Not `Clone`; `Debug` shows counts and
/// presence only, because a generation is a digest of a refresh token (R53).
#[derive(Default)]
pub(crate) struct LegacyAuthFuse {
    /// The last generation counted as rejected.
    rejected_generation: Option<Uuid>,
    /// Consecutive rejected generations, 0 to [`FUSE_LIMIT`] (R41).
    consecutive: u8,
    /// Epoch seconds of the last counted rejection.
    last_rejection_at: Option<i64>,
    /// The end of the `awaiting_confirmation` pause (R41).
    pause_until: Option<i64>,
    /// The digest of the access token the last counted rejection rejected (hub fix
    /// pass 3); `None` after a stop and when nothing is counted.
    rejected_access: Option<Uuid>,
    /// Cars whose 401 the account-wide confirmation disproved, with the
    /// generation it was disproved for (R41a). Not persisted.
    vehicle_rejected: HashMap<VehicleId, Uuid>,
    /// The generation for which a stream rejection was already disproved (R40).
    stream_cleared_generation: Option<Uuid>,
    /// The confirmation read (R41a).
    confirmation: ConfirmationState,
    /// A stop was reached (R43): `is_blown` is true for any stop.
    blown: bool,
    /// The generation the last account-wide request carried: the generation a
    /// data-plane 403 on the list carried, for the refusal check (R49).
    last_account_generation: Option<Uuid>,
    /// Confirmation reads sent and not yet counted in the collector's request
    /// counter (C3 R24: each is one `state` request).
    confirmations_unreported: u32,
    /// The answers of confirmation reads that were neither a 2xx nor a 401 (R41a:
    /// "the collector's own classification handles the confirmation's answer like
    /// any list answer"), not yet read by the collector. The request that
    /// triggered the read fails with `AuthUnavailable`; the answer is kept here so
    /// a 429 with its `Retry-After`, a 410 or a 403 that names the Fleet API
    /// reaches the collector's rate-limit and refusal rules and is not lost
    /// (C5-S4). At most one confirmation starts a minute, so the list is short;
    /// it is bounded anyway.
    confirmation_errors: Vec<OwnerApiError>,
    /// What the collector runs at the moment a request is really handed to the
    /// network (C5 R39, C3 R25): it counts the request then, not when the request
    /// was planned, so one that the unit refuses unsent (a pause that began
    /// while it queued) is never counted (Codex R08). Armed by the collector for
    /// the duration of one request.
    send_counter: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Requests sent since the counter was armed.
    sends: u32,
}

/// The most inconclusive confirmation answers held for the collector. One read
/// starts at most every 60 s (R41a) and the collector reads them every
/// iteration, a few seconds apart, so this is never reached in practice; it
/// bounds the memory if the collector stopped reading.
const MAX_CONFIRMATION_ERRORS: usize = 8;

impl fmt::Debug for LegacyAuthFuse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LegacyAuthFuse")
            .field("consecutive", &self.consecutive)
            .field(
                "has_rejected_generation",
                &self.rejected_generation.is_some(),
            )
            .field("last_rejection_at", &self.last_rejection_at)
            .field("pause_until", &self.pause_until)
            .field("has_rejected_access", &self.rejected_access.is_some())
            .field("vehicle_marks", &self.vehicle_rejected.len())
            .field("confirmation", &self.confirmation)
            .field("blown", &self.blown)
            .finish()
    }
}

impl LegacyAuthFuse {
    /// The fuse as stored with the status (R42, R57 step 7).
    pub(crate) fn from_snapshot(snapshot: FuseSnapshot) -> Self {
        Self {
            rejected_generation: snapshot.generation,
            consecutive: snapshot.consecutive.min(FUSE_LIMIT),
            last_rejection_at: snapshot.last_rejection_at,
            pause_until: snapshot.pause_until,
            rejected_access: snapshot.rejected_access,
            ..Self::default()
        }
    }

    /// The three values that outlive a process, and the pause (section 3.3).
    pub(crate) fn snapshot(&self) -> FuseSnapshot {
        FuseSnapshot {
            consecutive: self.consecutive,
            generation: self.rejected_generation,
            last_rejection_at: self.last_rejection_at,
            pause_until: self.pause_until,
            rejected_access: self.rejected_access,
        }
    }

    /// True for any stop of the account (section 3.2): the fuse reached its
    /// threshold, or another path stopped the account and said so.
    pub(crate) fn is_blown(&self) -> bool {
        self.blown
    }

    /// A stop was set by a path other than a count of this fuse (a refresh that
    /// found the refresh token dead, a refusal), so every path that asks the
    /// fuse sees one stop (section 3.2).
    pub(crate) fn note_stop(&mut self, stopped: bool) {
        self.blown = stopped;
    }

    /// Consecutive counted rejections (R41).
    #[cfg(test)]
    pub(crate) fn consecutive(&self) -> u8 {
        self.consecutive
    }

    /// The pause has been spent: the forced refresh that follows it began (R41,
    /// R57 step 8), so a stored pause is not owed again after a restart.
    pub(crate) fn clear_pause(&mut self) {
        self.pause_until = None;
    }

    /// Clear the counting, the pause, the marks and the stream memory. Called
    /// only by R44 (the owner's retry) and R44a (an external credential write),
    /// whose new credentials make every mark and count history (R42).
    pub(crate) fn reset(&mut self) {
        self.rejected_generation = None;
        self.consecutive = 0;
        self.last_rejection_at = None;
        self.pause_until = None;
        self.rejected_access = None;
        self.vehicle_rejected.clear();
        self.stream_cleared_generation = None;
        self.blown = false;
    }

    /// R42: a 2xx answer carrying the current generation disproves rejection:
    /// the count, the pause and the generation counted are cleared. A 2xx for one
    /// car clears that car's mark (R41a). A refresh-produced generation does not
    /// come here: counting across the unit's own refreshes is the point of the
    /// fuse. The caller checks that `generation` is the current one.
    pub(crate) fn record_success(&mut self, _generation: Uuid, scope: RequestScope) {
        self.rejected_generation = None;
        self.consecutive = 0;
        self.last_rejection_at = None;
        self.pause_until = None;
        self.rejected_access = None;
        if let Some(vehicle) = scope.vehicle() {
            self.vehicle_rejected.remove(&vehicle);
        }
    }

    /// Handle a 401 by the first matching step of R40.
    pub(crate) fn record_rejection(&mut self, now: i64, ctx: &RejectionContext) -> RejectionAction {
        // 1. The pair has been replaced since the request was made: the answer
        //    says nothing about the pair in use (R40, R61).
        if ctx.generation != ctx.current {
            return RejectionAction::RetryWithCurrent;
        }
        // 2. A token known to be expired, or a remedy already under way, only
        //    produces 401s that would be misread as evidence.
        if !ctx.token_usable || ctx.remedy_pending {
            return RejectionAction::NotCounted;
        }
        // 3. Already counted: every request carrying the same token is one piece
        //    of evidence, so a household with five cars signs out no faster than
        //    one with one. A rejection counted more than the fuse window ago is
        //    stale and counts again. An access-token-only pair has no new
        //    generation to wait for, so a rejection after the pause is the
        //    confirmation's own answer (R8a).
        if self.rejected_generation == Some(ctx.generation)
            && self
                .last_rejection_at
                .is_some_and(|at| rejection_window_open(at, now))
        {
            let pause_over = ctx.holder == HolderKind::AccessTokenOnly
                && self.pause_until.is_some_and(|until| now >= until);
            if !pause_over {
                return RejectionAction::Coalesced;
            }
            // R8a: when the pause is over one account-wide confirmation read with
            // the same token decides. A 401 to an account-wide read is that read's
            // own answer. A 401 to ONE CAR'S request is not: it may mean only that
            // this car's access was withdrawn (R40 step 5, R41a), so it falls
            // through to the per-vehicle steps below, which send the confirmation
            // read and stop only on its 401 (C5-S3).
            if ctx.scope == RequestScope::Account {
                self.blown = true;
                return RejectionAction::Stop(SignOutReason::ProviderTokenRejected);
            }
        }
        if let Some(vehicle) = ctx.scope.vehicle() {
            // 4. A car whose own 401 an account-wide read disproved for this
            //    generation.
            if self.vehicle_rejected.get(&vehicle) == Some(&ctx.generation) {
                return RejectionAction::VehicleRejected(vehicle);
            }
            // 5. A per-vehicle 401 may mean only that this car's access was
            //    withdrawn, so the account is tested before the account is blamed.
            return RejectionAction::Confirm;
        }
        // 6. An account-wide read: a counted rejection.
        self.count_rejection(now, ctx)
    }

    /// A counted rejection of `ctx.generation` (R41): the count, the evidence
    /// and the action of the table. Called for an account-wide 401 and for the
    /// 401 that answers a confirmation read.
    pub(crate) fn count_rejection(&mut self, now: i64, ctx: &RejectionContext) -> RejectionAction {
        if self
            .last_rejection_at
            .is_some_and(|at| !rejection_window_open(at, now))
        {
            self.consecutive = 0;
        }
        self.consecutive = self.consecutive.saturating_add(1).min(FUSE_LIMIT);
        self.rejected_generation = Some(ctx.generation);
        self.last_rejection_at = Some(now);
        let pause = now.saturating_add(REJECTION_PAUSE_SECONDS);
        let action = match ctx.holder {
            // The refresh owner's next pair is the only remedy (R28).
            HolderKind::Observer => RejectionAction::ObserverWait,
            HolderKind::AccessTokenOnly => {
                if self.consecutive == 1 {
                    self.pause_until = Some(pause);
                    RejectionAction::PauseThenConfirm { until: pause }
                } else {
                    RejectionAction::Stop(SignOutReason::ProviderTokenRejected)
                }
            }
            HolderKind::Managed => match ctx.sign_in {
                // No refresh is possible in `sign_in_required`, so any counted
                // rejection signs the account out with the same reason.
                Some(reason) => RejectionAction::Stop(reason),
                None => match self.consecutive {
                    1 => RejectionAction::ForceRefreshThenRetry,
                    2 => {
                        self.pause_until = Some(pause);
                        RejectionAction::PauseThenRefresh { until: pause }
                    }
                    _ => RejectionAction::Stop(SignOutReason::AccessTokenRejected),
                },
            },
        };
        // The token that was rejected is kept by its digest for every counted rejection
        // that is not a stop, whichever holder counted it (hub fix pass 3): a restart
        // compares it with the stored pair to know whether the rejected token is still
        // the one in use.
        if matches!(action, RejectionAction::Stop(_)) {
            self.rejected_access = None;
            self.blown = true;
        } else {
            self.rejected_access = Some(ctx.access_digest);
        }
        action
    }

    // ---- R41a: the confirmation read and the marks of cars ------------------

    /// Begin a confirmation read at `now`, or say when the next may start (R41a).
    /// At most one is in flight and at most one starts per
    /// [`CONFIRMATION_SPACING_SECONDS`]; a trigger inside that interval fails its
    /// request with `AuthUnavailable { retry_at: Some(start + 60) }` and leaves
    /// the fuse unchanged. The answer is `Err(retry_at)` in that case.
    pub(crate) fn begin_confirmation(&mut self, now: i64) -> Result<(), i64> {
        if let Some(started) = self.confirmation.last_started_at {
            let next = started.saturating_add(CONFIRMATION_SPACING_SECONDS);
            if self.confirmation.in_flight || now < next {
                return Err(next);
            }
        }
        self.confirmation.last_started_at = Some(now);
        self.confirmation.in_flight = true;
        self.confirmations_unreported = self.confirmations_unreported.saturating_add(1);
        Ok(())
    }

    /// When the next confirmation read may start: one spacing after the last one
    /// began (R41a). A request whose confirmation was inconclusive reports it.
    pub(crate) fn confirmation_retry_at(&self) -> Option<i64> {
        self.confirmation
            .last_started_at
            .map(|started| started.saturating_add(CONFIRMATION_SPACING_SECONDS))
    }

    /// The confirmation read has ended.
    pub(crate) fn finish_confirmation(&mut self) {
        self.confirmation.in_flight = false;
    }

    /// The confirmation disproved this car's 401 for `generation` (R41a).
    pub(crate) fn mark_vehicle_rejected(&mut self, vehicle: VehicleId, generation: Uuid) {
        self.vehicle_rejected.insert(vehicle, generation);
    }

    /// Whether a car holds a mark for this generation (R41a).
    #[cfg(test)]
    pub(crate) fn vehicle_mark(&self, vehicle: VehicleId) -> Option<Uuid> {
        self.vehicle_rejected.get(&vehicle).copied()
    }

    /// A stream rejection was disproved for `generation` (R40).
    pub(crate) fn note_stream_cleared(&mut self, generation: Uuid) {
        self.stream_cleared_generation = Some(generation);
    }

    /// Whether a stream rejection is already disproved for `generation` (R40).
    pub(crate) fn stream_cleared(&self, generation: Uuid) -> bool {
        self.stream_cleared_generation == Some(generation)
    }

    /// The generation the last account-wide request carried (R49).
    pub(crate) fn last_account_generation(&self) -> Option<Uuid> {
        self.last_account_generation
    }

    /// Remember the generation an account-wide request carried (R49).
    pub(crate) fn note_account_generation(&mut self, generation: Uuid) {
        self.last_account_generation = Some(generation);
    }

    /// The confirmation reads sent since the last call, so the collector's
    /// counter can count each as one `state` request (C3 R24, R25).
    pub(crate) fn take_confirmation_count(&mut self) -> u32 {
        std::mem::take(&mut self.confirmations_unreported)
    }

    /// Arm the counter the collector wants run at each send of a request (Codex
    /// R08). Replaces any earlier one.
    pub(crate) fn arm_send_counter(&mut self, counter: Arc<dyn Fn() + Send + Sync>) {
        self.send_counter = Some(counter);
        self.sends = 0;
    }

    /// Disarm the counter and return how many requests were sent while it was
    /// armed.
    pub(crate) fn disarm_send_counter(&mut self) -> u32 {
        self.send_counter = None;
        std::mem::take(&mut self.sends)
    }

    /// A request is about to be handed to the network: the counter runs. A
    /// confirmation read is not announced here; it is counted by
    /// [`take_confirmation_count`](Self::take_confirmation_count).
    pub(crate) fn note_send(&mut self) {
        self.sends = self.sends.saturating_add(1);
        if let Some(counter) = &self.send_counter {
            counter();
        }
    }

    /// Keep the answer of an inconclusive confirmation read for the collector's
    /// own classification (R41a, C5-S4). A 401 and a 2xx are the fuse's own
    /// answers and are never kept.
    pub(crate) fn note_confirmation_error(&mut self, error: OwnerApiError) {
        if self.confirmation_errors.len() < MAX_CONFIRMATION_ERRORS {
            self.confirmation_errors.push(error);
        }
    }

    /// The answers kept since the last call, oldest first.
    pub(crate) fn take_confirmation_errors(&mut self) -> Vec<OwnerApiError> {
        std::mem::take(&mut self.confirmation_errors)
    }
}
