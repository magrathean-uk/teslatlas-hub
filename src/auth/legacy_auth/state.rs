// SPDX-License-Identifier: AGPL-3.0-only

//! Durable status, clock floor, skew and restart of the legacy credential unit
//! (C5 part P2: R37, R38, R43, R44, R44a, R52, R52a, R54, R55, R57 steps 1 to 5
//! and 7 to 9, R58, R59, R62; section 3.3 and the state tables of section 5).
//!
//! Nothing here sends a request or decrypts a token. The refresh attempt (P3)
//! and the rejection handling (P4) decide *when* the status changes; this file
//! defines what a status is, what it needs to be written exactly when it
//! changes (R52a), how it is rebuilt at start from the stored row, the fencing
//! tables and the configuration (R57), and how the unit tells its own writes
//! of the pair row from an external one (R44a, R62). Times are epoch seconds
//! from `SystemTime` (R37) except where a store column is in milliseconds.

use std::fmt;

use uuid::Uuid;

use super::{
    AuthMode, ConfigurationReason, RefusalReason, RetryReason, SignOutReason,
    http_date::parse_http_date,
};
use crate::db::{
    CollectorAuthRow, HubStore, OutboundRequestReceiptId, StoreError, StoredLegacyGeneration,
    TeslaMateLegacyTokenStore,
};

/// 2020-01-01T00:00:00Z (R38). Every token the `oauth2/v3` issuers mint
/// postdates 2020, so a host clock before it is certainly wrong: a host
/// without a real-time clock can start far in the past until NTP synchronises
/// (F14), and a schedule written from such a clock would be meaningless.
pub(crate) const CLOCK_FLOOR_EPOCH_SECONDS: i64 = 1_577_836_800;

/// How often a host clock below the floor is read again (R38): about the time
/// NTP needs on a networked host to step the clock.
pub(crate) const CLOCK_RECHECK_SECONDS: i64 = 60;

/// The skew allowance (R54, R55, F14): quartz drift of up to about 4 s a day
/// covers a host that was offline for over two months, so a difference between
/// the host clock and the server's `Date` above this is a clock to fix.
pub(crate) const SKEW_ALLOWANCE_SECONDS: i64 = 300;

/// The minimum interval between token requests (R19, R58): it caps any refresh
/// loop at one rotation a minute, in one process (R19) and across restarts
/// through the receipt table (R58).
pub(crate) const MIN_REFRESH_INTERVAL_SECONDS: i64 = 60;

/// The window of the rejection fuse (R41): a rejection is forgotten when an
/// hour has passed without a further one, long against the 300 s confirmation
/// step and short against an 8 h token. R57 step 7 applies the same window to
/// a stored fuse.
pub(crate) const FUSE_WINDOW_SECONDS: i64 = 3_600;

/// The highest value of `consecutive` (R41): the third rejected generation
/// stops the account, so the stored column never needs a fourth.
pub(crate) const FUSE_LIMIT: u8 = 3;

/// How often a stopped or waiting unit reads the stored generation again
/// (R28, R39, R44a): one indexed single-row read a minute is negligible against
/// the idle target, and a minute is small against an 8 h token.
pub(crate) const EXTERNAL_WRITE_RECHECK_SECONDS: i64 = 60;

/// Retry delays, in seconds, of a status write that failed (R43) and of the
/// persistence of a validated successor (R24, which this schedule is shared
/// with). A brief lock or busy store clears within 5 s; doubling reaches the
/// 300 s cap in six steps, so a store that comes back is noticed within five
/// minutes without busy-waiting.
pub(crate) const PERSISTENCE_RETRY_SECONDS: [u64; 7] = [5, 10, 20, 40, 80, 160, 300];

/// The account state codes of R52. Closed, because readiness reads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AuthState {
    Ready,
    Refreshing,
    Retrying,
    PersistencePending,
    AwaitingConfirmation,
    ObserverWaiting,
    ClockInvalid,
    ConfigurationError,
    SignInRequired,
    SignedOut,
    ApiRefused,
}

impl AuthState {
    /// Every state, in the order of the `collector_auth_state` CHECK.
    pub(crate) const ALL: [Self; 11] = [
        Self::Ready,
        Self::Refreshing,
        Self::Retrying,
        Self::PersistencePending,
        Self::AwaitingConfirmation,
        Self::ObserverWaiting,
        Self::ClockInvalid,
        Self::ConfigurationError,
        Self::SignInRequired,
        Self::SignedOut,
        Self::ApiRefused,
    ];

    /// The wire code of R52, the value stored and shown.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Refreshing => "refreshing",
            Self::Retrying => "retrying",
            Self::PersistencePending => "persistence_pending",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::ObserverWaiting => "observer_waiting",
            Self::ClockInvalid => "clock_invalid",
            Self::ConfigurationError => "configuration_error",
            Self::SignInRequired => "sign_in_required",
            Self::SignedOut => "signed_out",
            Self::ApiRefused => "api_refused",
        }
    }

    pub(crate) fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.code() == code)
    }
}

/// The reasons of `observer_waiting` (R28, R52).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ObserverReason {
    /// A counted rejection (R41).
    TokenRejected,
    /// `usable_until` passed.
    TokenExpired,
    /// `usable_until` passed while the host clock is known to be wrong (R55).
    ClockSkew,
}

impl ObserverReason {
    pub(crate) const ALL: [Self; 3] = [Self::TokenRejected, Self::TokenExpired, Self::ClockSkew];

    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::TokenRejected => "token_rejected",
            Self::TokenExpired => "token_expired",
            Self::ClockSkew => "clock_skew",
        }
    }
}

/// The reason attached to a state (R52): one of five closed vocabularies, so a
/// code means the same thing wherever it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StatusReason {
    Retry(RetryReason),
    SignOut(SignOutReason),
    Refusal(RefusalReason),
    Configuration(ConfigurationReason),
    Observer(ObserverReason),
}

const ALL_RETRY_REASONS: [RetryReason; 11] = [
    RetryReason::Network,
    RetryReason::SensitiveAccess,
    RetryReason::TeslaUnavailable,
    RetryReason::RefreshBlocked,
    RetryReason::RefreshRequestRejected,
    RetryReason::TokenEndpointMisrouted,
    RetryReason::ClientCheck,
    RetryReason::ShortTokenLifetime,
    RetryReason::ClockSkew,
    RetryReason::RefreshNotRenewed,
    RetryReason::StoreUnavailable,
];

const ALL_SIGN_OUT_REASONS: [SignOutReason; 7] = [
    SignOutReason::RefreshTokenRejected,
    SignOutReason::RefreshOutcomeUnknown,
    SignOutReason::RefreshResponseUnusable,
    SignOutReason::AccessTokenRejected,
    SignOutReason::StoredPairInvalid,
    SignOutReason::ProviderTokenRejected,
    SignOutReason::ProviderTokenExpired,
];

const ALL_REFUSAL_REASONS: [RefusalReason; 2] = [RefusalReason::OwnerApi, RefusalReason::Client];

const ALL_CONFIGURATION_REASONS: [ConfigurationReason; 5] = [
    ConfigurationReason::RegionConflict,
    ConfigurationReason::RegionRequired,
    ConfigurationReason::IssuerNotAllowed,
    ConfigurationReason::TokenUrlInvalid,
    ConfigurationReason::TokenUrlNotAllowed,
];

impl StatusReason {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Retry(reason) => reason.code(),
            Self::SignOut(reason) => reason.code(),
            Self::Refusal(reason) => reason.code(),
            Self::Configuration(reason) => reason.code(),
            Self::Observer(reason) => reason.code(),
        }
    }

    /// Read a stored reason code under its state. The state picks the
    /// vocabulary, because two codes (`clock_skew`, `access_token_rejected`)
    /// are valid in more than one state. A code the state does not admit is
    /// `None`, and so is any code of a state that carries no reason.
    pub(crate) fn parse(state: AuthState, code: &str) -> Option<Self> {
        match state {
            AuthState::Retrying => ALL_RETRY_REASONS
                .into_iter()
                .find(|reason| reason.code() == code)
                .map(Self::Retry),
            AuthState::ObserverWaiting => ObserverReason::ALL
                .into_iter()
                .find(|reason| reason.code() == code)
                .map(Self::Observer),
            AuthState::ConfigurationError => ALL_CONFIGURATION_REASONS
                .into_iter()
                .find(|reason| reason.code() == code)
                .map(Self::Configuration),
            AuthState::SignInRequired | AuthState::SignedOut | AuthState::AwaitingConfirmation => {
                ALL_SIGN_OUT_REASONS
                    .into_iter()
                    .find(|reason| reason.code() == code)
                    .map(Self::SignOut)
                    .filter(|reason| state.admits(Some(*reason)))
            }
            AuthState::ApiRefused => ALL_REFUSAL_REASONS
                .into_iter()
                .find(|reason| reason.code() == code)
                .map(Self::Refusal),
            AuthState::Ready
            | AuthState::Refreshing
            | AuthState::PersistencePending
            | AuthState::ClockInvalid => None,
        }
    }
}

impl AuthState {
    /// Whether R52's table allows this reason under this state.
    pub(crate) fn admits(self, reason: Option<StatusReason>) -> bool {
        match (self, reason) {
            (
                Self::Ready | Self::Refreshing | Self::PersistencePending | Self::ClockInvalid,
                None,
            ) => true,
            (Self::Retrying, Some(StatusReason::Retry(_)))
            | (Self::ObserverWaiting, Some(StatusReason::Observer(_)))
            | (Self::ConfigurationError, Some(StatusReason::Configuration(_)))
            | (Self::ApiRefused, Some(StatusReason::Refusal(_)))
            | (Self::SignedOut, Some(StatusReason::SignOut(_))) => true,
            (Self::AwaitingConfirmation, Some(StatusReason::SignOut(reason))) => {
                reason == SignOutReason::AccessTokenRejected
            }
            (Self::SignInRequired, Some(StatusReason::SignOut(reason))) => matches!(
                reason,
                SignOutReason::RefreshTokenRejected
                    | SignOutReason::RefreshOutcomeUnknown
                    | SignOutReason::RefreshResponseUnusable
            ),
            _ => false,
        }
    }

    /// The lease state this account state implies (R52, R43): `auth_terminal`
    /// exactly while `signed_out`, `active` otherwise.
    pub(crate) const fn lease_is_terminal(self) -> bool {
        matches!(self, Self::SignedOut)
    }
}

/// C3 R1's stored mode codes (`collector_auth_state.stopped_mode`, section
/// 3.3). A mode, not a generation, because a refusal lifts on the owner's retry
/// or a mode change and never on new credentials (R44, seam S3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StoppedMode {
    Legacy,
    Provider,
}

impl StoppedMode {
    /// Tesla mode is C3's `Legacy`; Provider mode is `Provider` (R1).
    pub(crate) const fn of(mode: AuthMode) -> Self {
        match mode {
            AuthMode::Tesla => Self::Legacy,
            AuthMode::Provider => Self::Provider,
        }
    }

    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Provider => "provider",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "legacy" => Some(Self::Legacy),
            "provider" => Some(Self::Provider),
            _ => None,
        }
    }
}

/// A stop of the account (C5 section 3.1, R43).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AuthStop {
    /// Token requests stop; collection continues while the token is usable.
    SignInRequired(SignOutReason),
    /// Everything stops until credentials change.
    SignedOut(SignOutReason),
    /// Everything stops until the owner retries or changes the mode.
    ApiRefused(RefusalReason),
}

impl AuthStop {
    pub(crate) const fn state(self) -> AuthState {
        match self {
            Self::SignInRequired(_) => AuthState::SignInRequired,
            Self::SignedOut(_) => AuthState::SignedOut,
            Self::ApiRefused(_) => AuthState::ApiRefused,
        }
    }

    pub(crate) const fn reason(self) -> StatusReason {
        match self {
            Self::SignInRequired(reason) | Self::SignedOut(reason) => StatusReason::SignOut(reason),
            Self::ApiRefused(reason) => StatusReason::Refusal(reason),
        }
    }

    /// Whether Owner API requests and stream admissions stop (R43):
    /// `sign_in_required` stops only token requests.
    pub(crate) const fn blocks_requests(self) -> bool {
        !matches!(self, Self::SignInRequired(_))
    }

    /// R44 (a): an external credential write with a different generation
    /// lifts every credential stop. A refusal does not lift on new
    /// credentials (R44, seam S3).
    pub(crate) const fn lifted_by_external_write(self) -> bool {
        !matches!(self, Self::ApiRefused(_))
    }

    /// R44 (b): the owner's retry control lifts only the stops whose refresh
    /// token, if any, is still valid: a rejected access token, a rejected
    /// provider token and a refusal. Resending a rejected, unknown or
    /// unusable-answer refresh token risks the family revocation of F12 and
    /// contradicts R50 (C5R-03).
    pub(crate) const fn lifted_by_owner_retry(self) -> bool {
        matches!(
            self,
            Self::SignedOut(
                SignOutReason::AccessTokenRejected | SignOutReason::ProviderTokenRejected
            ) | Self::ApiRefused(_)
        )
    }

    /// R44 (c): a mode change lifts a refusal only.
    pub(crate) const fn lifted_by_mode_change(self) -> bool {
        matches!(self, Self::ApiRefused(_))
    }
}

/// The three values of the rejection fuse that outlive a process (R42, R57
/// step 7, section 3.3). A crash loop must not reset the fuse (C5R-13).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FuseSnapshot {
    /// Consecutive rejected generations, 0 to [`FUSE_LIMIT`].
    pub consecutive: u8,
    /// The last generation counted as rejected; `Some` exactly when
    /// `consecutive` is above zero.
    pub generation: Option<Uuid>,
    /// Epoch seconds of the last counted rejection.
    pub last_rejection_at: Option<i64>,
    /// The end of the `awaiting_confirmation` pause (R41).
    pub pause_until: Option<i64>,
    /// The digest of the access token that was counted as rejected (hub fix pass 3),
    /// stored with every counted rejection that is not a stop, whichever holder counted
    /// it. A restart owes the forced refresh exactly while the fuse window is open and the
    /// stored pair still holds this token (see [`fuse_owes_forced_refresh`]); a refresh
    /// that replaced the token, an external write or a 2xx change the data or clear the
    /// fuse, and nothing has to keep a flag in step. `Some` only while `consecutive` is
    /// above zero.
    pub rejected_access: Option<Uuid>,
}

impl fmt::Debug for FuseSnapshot {
    /// A generation identifies a refresh token (a digest of it), so only its
    /// presence is shown (R53).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FuseSnapshot")
            .field("consecutive", &self.consecutive)
            .field("has_generation", &self.generation.is_some())
            .field("last_rejection_at", &self.last_rejection_at)
            .field("pause_until", &self.pause_until)
            .field("has_rejected_access", &self.rejected_access.is_some())
            .finish()
    }
}

impl FuseSnapshot {
    /// Whether the columns tie together the way the table's CHECK demands.
    fn is_consistent(&self) -> bool {
        self.consecutive <= FUSE_LIMIT
            && (self.consecutive == 0) == self.generation.is_none()
            && (self.rejected_access.is_none() || self.consecutive > 0)
    }
}

/// Whether the last counted rejection still counts (R41): it is forgotten when
/// more than [`FUSE_WINDOW_SECONDS`] have passed since it. A rejection dated
/// after `now` (the clock stepped back) has had no time pass and still counts.
pub(crate) fn rejection_window_open(last_rejection_at: i64, now: i64) -> bool {
    now.saturating_sub(last_rejection_at) <= FUSE_WINDOW_SECONDS
}

/// The status of the account as written to `collector_auth_state` (R52,
/// R52a): the derived values the store, readiness and diagnostics read. The
/// secret-bearing `LegacyAuth` itself is never persisted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuthStatus {
    pub state: AuthState,
    pub reason: Option<StatusReason>,
    /// R52a: whether the state collects at the moment, which for `retrying`,
    /// `persistence_pending` and `sign_in_required` depends on whether the
    /// access token is still usable (R29).
    pub collecting: bool,
    /// R31: when a retry is pending, the earliest next token attempt.
    pub retry_at: Option<i64>,
    /// R43: the generation current when a credential stop was set.
    pub stopped_generation: Option<Uuid>,
    /// R43: the mode in force when a refusal was set.
    pub stopped_mode: Option<StoppedMode>,
    pub fuse: FuseSnapshot,
    /// R55.
    pub clock_skew: bool,
}

impl fmt::Debug for AuthStatus {
    /// Codes, times and presence only (R53).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthStatus")
            .field("state", &self.state.code())
            .field("reason", &self.reason.map(StatusReason::code))
            .field("collecting", &self.collecting)
            .field("retry_at", &self.retry_at)
            .field("stopped", &self.stopped_generation.is_some())
            .field("stopped_mode", &self.stopped_mode.map(StoppedMode::code))
            .field("fuse", &self.fuse)
            .field("clock_skew", &self.clock_skew)
            .finish()
    }
}

impl AuthStatus {
    /// A collecting `ready` account with the given fuse and skew flag.
    pub(crate) fn ready(fuse: FuseSnapshot, clock_skew: bool) -> Self {
        Self {
            state: AuthState::Ready,
            reason: None,
            collecting: true,
            retry_at: None,
            stopped_generation: None,
            stopped_mode: None,
            fuse,
            clock_skew,
        }
    }

    /// A status in a state that carries no stop and collects nothing.
    fn idle(
        state: AuthState,
        reason: Option<StatusReason>,
        fuse: FuseSnapshot,
        skew: bool,
    ) -> Self {
        Self {
            state,
            reason,
            collecting: false,
            retry_at: None,
            stopped_generation: None,
            stopped_mode: None,
            fuse,
            clock_skew: skew,
        }
    }

    /// An observer that waits for the refresh owner's next pair (R28): it sends
    /// no Owner API request and admits no stream.
    pub(crate) fn observer_waiting(reason: ObserverReason, fuse: FuseSnapshot, skew: bool) -> Self {
        Self::idle(
            AuthState::ObserverWaiting,
            Some(StatusReason::Observer(reason)),
            fuse,
            skew,
        )
    }

    /// The confirmation pause after a second counted rejection, or after the
    /// first of an access-token-only provider (R41, R8a): requests and stream
    /// admissions wait.
    pub(crate) fn awaiting_confirmation(fuse: FuseSnapshot, skew: bool) -> Self {
        Self::idle(
            AuthState::AwaitingConfirmation,
            Some(StatusReason::SignOut(SignOutReason::AccessTokenRejected)),
            fuse,
            skew,
        )
    }

    /// The status of a stop (R43). `generation` is the generation current at
    /// the stop and `mode` the configured mode; each is stored only where the
    /// table ties it to the state. `usable` says whether the access token can
    /// still be used, which decides whether `sign_in_required` collects.
    pub(crate) fn stopped(
        stop: AuthStop,
        generation: Uuid,
        mode: StoppedMode,
        usable: bool,
        fuse: FuseSnapshot,
        clock_skew: bool,
    ) -> Self {
        let mut status = Self::idle(stop.state(), Some(stop.reason()), fuse, clock_skew);
        match stop {
            AuthStop::SignInRequired(_) => {
                status.stopped_generation = Some(generation);
                status.collecting = usable;
            }
            AuthStop::SignedOut(_) => status.stopped_generation = Some(generation),
            AuthStop::ApiRefused(_) => status.stopped_mode = Some(mode),
        }
        status
    }

    /// The stop this status is, if it is one.
    pub(crate) fn stop(&self) -> Option<AuthStop> {
        match (self.state, self.reason) {
            (AuthState::SignInRequired, Some(StatusReason::SignOut(reason))) => {
                Some(AuthStop::SignInRequired(reason))
            }
            (AuthState::SignedOut, Some(StatusReason::SignOut(reason))) => {
                Some(AuthStop::SignedOut(reason))
            }
            (AuthState::ApiRefused, Some(StatusReason::Refusal(reason))) => {
                Some(AuthStop::ApiRefused(reason))
            }
            _ => None,
        }
    }

    /// R52a: the passing or return of `usable_until` is a change of the
    /// Collects column for the three states that collect only while the token
    /// is usable (`retrying`, `persistence_pending`, `sign_in_required`). Other
    /// states are returned unchanged.
    pub(crate) fn with_usability(mut self, usable: bool) -> Self {
        if matches!(
            self.state,
            AuthState::Retrying | AuthState::PersistencePending | AuthState::SignInRequired
        ) {
            self.collecting = usable;
        }
        self
    }

    /// R45, R57 step 5 (a): a `sign_in_required` whose token is no longer
    /// usable becomes `signed_out` with the same reason and generation.
    /// Any other status is returned unchanged.
    pub(crate) fn with_sign_in_expired(mut self) -> Self {
        if let (AuthState::SignInRequired, Some(reason)) = (self.state, self.reason) {
            self.state = AuthState::SignedOut;
            self.reason = Some(reason);
            self.collecting = false;
        }
        self
    }

    /// Whether the invariants of section 3.3 and R52 hold: the reason fits the
    /// state, the stop columns are tied to the state exactly as the table's
    /// CHECKs demand, the fuse columns agree, and states that never collect
    /// say so.
    pub(crate) fn is_consistent(&self) -> bool {
        let sign_stop = matches!(self.state, AuthState::SignInRequired | AuthState::SignedOut);
        let always_collects = matches!(self.state, AuthState::Ready | AuthState::Refreshing);
        let never_collects = matches!(
            self.state,
            AuthState::AwaitingConfirmation
                | AuthState::ObserverWaiting
                | AuthState::ClockInvalid
                | AuthState::ConfigurationError
                | AuthState::SignedOut
                | AuthState::ApiRefused
        );
        self.state.admits(self.reason)
            && sign_stop == self.stopped_generation.is_some()
            && (self.state == AuthState::ApiRefused) == self.stopped_mode.is_some()
            && self.fuse.is_consistent()
            && (!always_collects || self.collecting)
            && (!never_collects || !self.collecting)
            && (self.state == AuthState::Retrying || self.retry_at.is_none())
    }

    /// The row to store (R52a).
    pub(crate) fn to_row(self) -> CollectorAuthRow {
        CollectorAuthRow {
            state: self.state.code().to_owned(),
            reason: self.reason.map(|reason| reason.code().to_owned()),
            collecting: self.collecting,
            retry_at: self.retry_at,
            stopped_generation: self.stopped_generation,
            stopped_mode: self.stopped_mode.map(|mode| mode.code().to_owned()),
            fuse_consecutive: self.fuse.consecutive,
            fuse_generation: self.fuse.generation,
            fuse_last_rejection_at: self.fuse.last_rejection_at,
            fuse_pause_until: self.fuse.pause_until,
            fuse_rejected_access_digest: self.fuse.rejected_access,
            clock_skew: self.clock_skew,
        }
    }

    /// Read a stored row. A row whose state or reason this version does not
    /// know, or that breaks an invariant (a newer or damaged writer), is
    /// refused rather than guessed at: a guessed state could lift a stop.
    pub(crate) fn from_row(row: &CollectorAuthRow) -> Result<Self, StoreError> {
        let state = AuthState::from_code(&row.state).ok_or(StoreError::InvalidStoredAuthStatus)?;
        let reason = match row.reason.as_deref() {
            Some(code) => {
                Some(StatusReason::parse(state, code).ok_or(StoreError::InvalidStoredAuthStatus)?)
            }
            None => None,
        };
        let stopped_mode = match row.stopped_mode.as_deref() {
            Some(code) => {
                Some(StoppedMode::from_code(code).ok_or(StoreError::InvalidStoredAuthStatus)?)
            }
            None => None,
        };
        let status = Self {
            state,
            reason,
            collecting: row.collecting,
            retry_at: row.retry_at,
            stopped_generation: row.stopped_generation,
            stopped_mode,
            fuse: FuseSnapshot {
                consecutive: row.fuse_consecutive,
                generation: row.fuse_generation,
                last_rejection_at: row.fuse_last_rejection_at,
                pause_until: row.fuse_pause_until,
                rejected_access: row.fuse_rejected_access_digest,
            },
            clock_skew: row.clock_skew,
        };
        if status.is_consistent() {
            Ok(status)
        } else {
            Err(StoreError::InvalidStoredAuthStatus)
        }
    }
}

// ---------------------------------------------------------------------------
// R38 clock floor, R55 skew
// ---------------------------------------------------------------------------

/// R38: whether the host clock can be trusted at all. Below the floor the unit
/// is `clock_invalid`: no token request, no schedule written, no token handed
/// out, and the clock is read again every [`CLOCK_RECHECK_SECONDS`].
pub(crate) const fn clock_is_valid(now: i64) -> bool {
    now >= CLOCK_FLOOR_EPOCH_SECONDS
}

/// R55: the skew flag after an answer that passed through the unit, from the
/// token endpoint or an Owner API wrapper. An answer with a valid `Date` that
/// differs from the local receive time by more than [`SKEW_ALLOWANCE_SECONDS`]
/// sets it and one within clears it; an answer with no valid `Date` leaves it
/// as it was. The flag changes no schedule: it selects the reason code
/// `clock_skew` where R28 and R29a report one.
pub(crate) fn skew_after_answer(
    current: bool,
    date_header: Option<&str>,
    received_at: i64,
) -> bool {
    let Some(server) = date_header.and_then(parse_http_date) else {
        return current;
    };
    server.abs_diff(received_at) > SKEW_ALLOWANCE_SECONDS.unsigned_abs()
}

// ---------------------------------------------------------------------------
// R52a status writes (memory first, R43)
// ---------------------------------------------------------------------------

/// Where the status is written. `HubStore` is the production sink; tests put a
/// sink that fails on demand in its place.
pub(crate) trait StatusSink {
    /// Write the row; the answer says whether a stored row changed.
    fn write_status(&self, row: &CollectorAuthRow, now_ms: i64) -> Result<bool, StoreError>;
}

impl StatusSink for HubStore {
    fn write_status(&self, row: &CollectorAuthRow, now_ms: i64) -> Result<bool, StoreError> {
        self.write_collector_auth_row(row, now_ms)
    }
}

/// What a status change did to the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusWrite {
    /// The store already holds this status: no write was made (R52a).
    Unchanged,
    /// The row was written.
    Written,
    /// The write failed or is waiting for its retry; the status holds in
    /// memory and the write is tried again at `retry_at_ms` (R43).
    Deferred { retry_at_ms: i64 },
}

/// The in-memory status of the account and the one writer of its row (R43,
/// R52a). The status is set in memory first, so a failed write can never let
/// requests continue (R43); the row is written only when it differs from the
/// one the store is known to hold; a failed write is retried, always with the
/// latest status, on the [`PERSISTENCE_RETRY_SECONDS`] schedule.
pub(crate) struct StatusRecorder<S: StatusSink> {
    sink: S,
    current: AuthStatus,
    /// The row the store is known to hold, or `None` when it is not known.
    written: Option<CollectorAuthRow>,
    /// Failed writes since the last success: the index into the schedule.
    failures: usize,
    next_attempt_ms: Option<i64>,
}

impl<S: StatusSink> StatusRecorder<S> {
    /// `stored` is the row read at start (R57 step 5), so a restart that
    /// reaches the same status writes nothing.
    pub(crate) fn new(sink: S, initial: AuthStatus, stored: Option<CollectorAuthRow>) -> Self {
        Self {
            sink,
            current: initial,
            written: stored,
            failures: 0,
            next_attempt_ms: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> &AuthStatus {
        &self.current
    }

    /// The sink, for tests that count its calls.
    #[cfg(test)]
    pub(crate) fn sink_for_test(&self) -> &S {
        &self.sink
    }

    /// Make `status` the current one without writing it: the unit may not mutate the
    /// store (B2 fix round, P3b fix 1, R-P3bF-1). The status holds in memory, as it does
    /// while a write waits for its retry; nothing is attempted and no retry is armed.
    pub(crate) fn hold(&mut self, status: AuthStatus) {
        debug_assert!(status.is_consistent(), "status {status:?} breaks R52");
        self.current = status;
    }

    /// Whether the store holds the current status.
    pub(crate) fn is_synced(&self) -> bool {
        self.written.as_ref() == Some(&self.current.to_row())
    }

    /// When the next write attempt is due, if one is pending (R43, R39).
    #[cfg(test)]
    pub(crate) fn retry_due_at_ms(&self) -> Option<i64> {
        self.next_attempt_ms
    }

    /// Make `status` the current one and write it if the store does not hold
    /// it. While a failed write waits for its retry time, a new status waits
    /// with it, so a store that is failing is not written at every transition.
    pub(crate) fn set(&mut self, status: AuthStatus, now_ms: i64) -> StatusWrite {
        debug_assert!(status.is_consistent(), "status {status:?} breaks R52");
        self.current = status;
        if self.is_synced() {
            self.failures = 0;
            self.next_attempt_ms = None;
            return StatusWrite::Unchanged;
        }
        if let Some(retry_at_ms) = self.next_attempt_ms
            && now_ms < retry_at_ms
        {
            return StatusWrite::Deferred { retry_at_ms };
        }
        self.attempt(now_ms)
    }

    /// Retry a pending write when its time has come. The manager does not use it:
    /// `set` retries a due write with the newest status, so a retry never writes a
    /// superseded one (R-P3F3-1); it stays for the recorder's own tests.
    #[cfg(test)]
    pub(crate) fn flush_if_due(&mut self, now_ms: i64) -> StatusWrite {
        if self.is_synced() {
            self.failures = 0;
            self.next_attempt_ms = None;
            return StatusWrite::Unchanged;
        }
        match self.next_attempt_ms {
            Some(retry_at_ms) if now_ms < retry_at_ms => StatusWrite::Deferred { retry_at_ms },
            _ => self.attempt(now_ms),
        }
    }

    /// One write attempt of the current status, due or not: the final write of
    /// a graceful shutdown (R33a) uses this.
    pub(crate) fn attempt(&mut self, now_ms: i64) -> StatusWrite {
        let row = self.current.to_row();
        match self.sink.write_status(&row, now_ms) {
            Ok(_) => {
                self.written = Some(row);
                self.failures = 0;
                self.next_attempt_ms = None;
                StatusWrite::Written
            }
            Err(_) => {
                let step = self.failures.min(PERSISTENCE_RETRY_SECONDS.len() - 1);
                let delay_ms = i64::try_from(PERSISTENCE_RETRY_SECONDS[step])
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1_000);
                let retry_at_ms = now_ms.saturating_add(delay_ms);
                self.failures = self.failures.saturating_add(1);
                self.next_attempt_ms = Some(retry_at_ms);
                StatusWrite::Deferred { retry_at_ms }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// R44a, R62: the pair row against the unit's own writes
// ---------------------------------------------------------------------------

/// How the stored pair row relates to the unit's current generation (R44a,
/// R62).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoredPairRelation {
    /// The row holds the generation the unit holds.
    Current,
    /// The row holds another generation that one of the unit's own refreshes
    /// wrote (the output of a completed binding): a result persisted by a
    /// detached exchange whose callers were gone (R15a). The unit adopts it.
    OwnOutput(Uuid),
    /// The row holds a generation the unit did not produce, or none: an
    /// external credential write (R44a).
    External,
    /// The row is gone.
    Missing,
}

/// Compare the stored pair row with `own`, the generation the unit holds
/// (R44a). It reads the generation column only and needs no sensitive access.
pub(crate) fn stored_pair_relation(
    store: &HubStore,
    own: Uuid,
) -> Result<StoredPairRelation, StoreError> {
    Ok(match store.stored_legacy_credential_generation()? {
        StoredLegacyGeneration::NoRow => StoredPairRelation::Missing,
        StoredLegacyGeneration::Unbound => StoredPairRelation::External,
        StoredLegacyGeneration::Bound(stored) if stored == own => StoredPairRelation::Current,
        StoredLegacyGeneration::Bound(stored) => {
            if store.legacy_generation_is_refresh_output(stored)? {
                StoredPairRelation::OwnOutput(stored)
            } else {
                StoredPairRelation::External
            }
        }
    })
}

/// What a re-read of the pair row says about a validated successor whose write
/// is being retried (R24, R62).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistReread {
    /// The row holds the successor: an earlier write that reported an error had in
    /// fact committed, so persistence counts as done and nothing is written again.
    /// Only a successor whose generation differs from its input's is proved this
    /// way; one that kept the refresh token needs [`Self::ClosedForThisAttempt`].
    AlreadyPersisted,
    /// The pair row is exactly the row this attempt wrote and the attempt's own
    /// receipt is closed as a successful token refresh: the atomic write of the
    /// pair and the closing of the receipt both committed (CL-14). Nothing is
    /// written again.
    ClosedForThisAttempt,
    /// The row still holds the input: the write may be tried again.
    StillInput,
    /// The row holds a generation that is neither: an external write resolved
    /// the attempt (K4). The successor is discarded and R44a applies.
    External,
    /// The row is gone.
    Missing,
}

/// The pair row as one write of the unit put it: both ciphertexts, the schedule
/// and the generation, kept in memory from before the write (R-P3b-12). An equal
/// input and output generation does not tell this write's row from an external
/// replacement that kept the refresh token, because the generation identifies the
/// refresh token only; the image does. It is ciphertext, bounded by the store's
/// own limit, and never formatted.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PairImage {
    access: Vec<u8>,
    refresh: Vec<u8>,
    expires_at: i64,
    next_refresh_at: i64,
    generation: Uuid,
}

impl PairImage {
    /// The image of a row about to be written under `generation`.
    pub(crate) fn of(row: &TeslaMateLegacyTokenStore, generation: Uuid) -> Self {
        Self {
            access: row.access().to_vec(),
            refresh: row.refresh().to_vec(),
            expires_at: row.expires_at(),
            next_refresh_at: row.next_refresh_at(),
            generation,
        }
    }

    /// The row this image describes, rebuilt from the ciphertext it holds: a retry of
    /// the same attempt's write puts this very row again (R-P3bF-3), so the witness
    /// never names an image that was not stored. `None` when the image is not of the
    /// write with this `generation` and schedule.
    pub(crate) fn row_for_a_retry(
        &self,
        generation: Uuid,
        expires_at: i64,
        next_refresh_at: i64,
    ) -> Option<TeslaMateLegacyTokenStore> {
        if self.generation != generation
            || self.expires_at != expires_at
            || self.next_refresh_at != next_refresh_at
        {
            return None;
        }
        TeslaMateLegacyTokenStore::refreshed(
            self.access.clone(),
            self.refresh.clone(),
            self.expires_at,
            self.next_refresh_at,
        )
        .and_then(|row| row.with_credential_generation(self.generation))
        .ok()
    }

    /// Whether a stored row is exactly this image.
    fn is_the_row(&self, row: &TeslaMateLegacyTokenStore) -> bool {
        row.credential_generation() == Some(self.generation)
            && row.access() == self.access.as_slice()
            && row.refresh() == self.refresh.as_slice()
            && row.expires_at() == self.expires_at
            && row.next_refresh_at() == self.next_refresh_at
    }
}

impl fmt::Debug for PairImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairImage([ciphertext])")
    }
}

/// What the unit knows of its own write: the receipt of the attempt and the row it
/// wrote.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OwnWrite<'a> {
    pub(crate) receipt: OutboundRequestReceiptId,
    pub(crate) image: &'a PairImage,
}

/// Re-read the pair row before retrying the write of `successor`, the
/// successor of `input` (R24).
///
/// A successor whose generation differs from its input's is recognised by the
/// generation column alone. One that kept the refresh token has the generation of
/// its input and can carry a new access token and schedule, so the generation
/// proves nothing about it: with the unit's `own` write, the row and the attempt's
/// receipt are read in one snapshot, and only a row that is exactly the image the
/// attempt wrote, with that receipt closed as a successful Legacy token refresh,
/// proves the write (CL-14, R-P3b-12). A missing or unbound row, or one that
/// differs from the image once the attempt is no longer open, outranks any receipt.
/// Without `own` the generation alone decides, as for any other successor.
pub(crate) fn persist_reread(
    store: &HubStore,
    input: Uuid,
    successor: Uuid,
    own: Option<OwnWrite<'_>>,
) -> Result<PersistReread, StoreError> {
    if let Some(own) = own.filter(|_| input == successor) {
        let snapshot = store.legacy_pair_and_receipt_snapshot(own.receipt)?;
        let Some(row) = snapshot.row else {
            return Ok(PersistReread::Missing);
        };
        let Some(generation) = row.credential_generation() else {
            return Ok(PersistReread::External);
        };
        if generation != successor {
            return Ok(PersistReread::External);
        }
        let receipt = snapshot.receipt;
        let is_the_write = own.image.is_the_row(&row);
        let still_open = receipt.as_ref().is_some_and(|facts| facts.is_started());
        return Ok(if is_the_write {
            if receipt.is_some_and(|facts| facts.is_token_refresh_success()) {
                PersistReread::ClosedForThisAttempt
            } else {
                PersistReread::StillInput
            }
        } else if still_open {
            PersistReread::StillInput
        } else {
            PersistReread::External
        });
    }
    Ok(match store.stored_legacy_credential_generation()? {
        StoredLegacyGeneration::NoRow => PersistReread::Missing,
        StoredLegacyGeneration::Unbound => PersistReread::External,
        StoredLegacyGeneration::Bound(stored) if stored == successor => {
            PersistReread::AlreadyPersisted
        }
        StoredLegacyGeneration::Bound(stored) if stored == input => PersistReread::StillInput,
        StoredLegacyGeneration::Bound(_) => PersistReread::External,
    })
}

// ---------------------------------------------------------------------------
// R57 restart, R44a external write
// ---------------------------------------------------------------------------

/// The result of step 2 of R57, loading and validating the stored pair (R25,
/// R46). Step 2 needs decryption and therefore sensitive access; the caller
/// waits for it (the kept guard) before calling the restore, so a generation
/// is never compared before it could be computed (C5R-21 item 10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PairLoad {
    /// The pair loaded. `usable_until` is R25's bound on the access token
    /// (`None` when unknown).
    Loaded {
        generation: Uuid,
        usable_until: Option<i64>,
    },
    /// The pair fails the token grammar (R46), or Tesla mode holds an empty
    /// refresh token (R8a).
    Invalid { generation: Uuid },
}

impl PairLoad {
    const fn generation(&self) -> Uuid {
        match self {
            Self::Loaded { generation, .. } | Self::Invalid { generation } => *generation,
        }
    }

    /// R29: whether the access token can be used at `now`. An unknown bound
    /// is usable, an invalid pair is not.
    fn is_usable(&self, now: i64) -> bool {
        match self {
            Self::Loaded { usable_until, .. } => usable_until.is_none_or(|until| now < until),
            Self::Invalid { .. } => false,
        }
    }
}

/// What the caller knows at start that the store cannot say (R57).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StartInputs {
    /// Local epoch seconds (R37).
    pub now: i64,
    /// The configured mode (R1).
    pub mode: AuthMode,
    /// Observer mode: the Hub is not the refresh owner, so it never pauses for
    /// a confirmation (R41 table) and sends no token request (R14).
    pub observer: bool,
    /// Step 2.
    pub pair: PairLoad,
    /// Step 4: the outcome of endpoint selection (R4 to R8) for the current
    /// configuration and the stored token.
    pub configuration: Result<(), ConfigurationReason>,
    /// The B2 clarification of 2026-10-12 for step 5 (b): C3's start path
    /// reset its own refusal for a mode or fingerprint change, so this unit
    /// clears `api_refused` instead of restoring it. One owner decides when a
    /// refusal lifts (C3 R7).
    pub c3_reset_refusal: bool,
}

/// The facts of the store that R57 reads. Separate from the store calls so
/// every branch of the restore can be tested without one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartFacts {
    /// Step 3: a fencing attempt is unresolved.
    pub unresolved_attempt: bool,
    /// Step 5: the stored status row.
    pub stored: Option<CollectorAuthRow>,
    /// Step 7: the current generation is the output of a completed binding
    /// (R62).
    pub current_is_refresh_output: bool,
    /// Step 9: the start time, in milliseconds, of the latest token receipt
    /// whatever its outcome (R58).
    pub last_receipt_started_at_ms: Option<i64>,
}

impl StartFacts {
    /// Read the facts from the store.
    pub(crate) fn read(store: &HubStore, current: Uuid) -> Result<Self, StoreError> {
        Ok(Self {
            unresolved_attempt: store.has_unresolved_legacy_refresh()?,
            stored: store.load_collector_auth_row()?,
            current_is_refresh_output: store.legacy_generation_is_refresh_output(current)?,
            last_receipt_started_at_ms: store.latest_legacy_refresh_started_at_ms()?,
        })
    }
}

/// The unit's state after R57's steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RestoredAuth {
    /// The status to hold and write (R52a).
    pub status: AuthStatus,
    /// Step 1: the host clock is at or above the floor. When it is not, the
    /// caller reads the clock again every [`CLOCK_RECHECK_SECONDS`] and calls
    /// the restore again once it is (section 5.1: "re-evaluated from the
    /// stored pair").
    pub clock_valid: bool,
    /// Step 9: no token request before this epoch second (R58), or 0.
    pub token_requests_not_before: i64,
    /// Step 8: a restored pause has already ended, so the forced refresh that
    /// follows it (R41 count 2) is owed.
    pub pause_elapsed: bool,
}

/// Restore the unit's state at start (R57 steps 1 to 5 and 7 to 9). Step 6, the
/// refusal that C3 detected, is part P4's and applies after this when step 5
/// (b) restored nothing.
pub(crate) fn restore_at_start(
    store: &HubStore,
    inputs: &StartInputs,
) -> Result<RestoredAuth, StoreError> {
    let facts = StartFacts::read(store, inputs.pair.generation())?;
    restore_from_facts(inputs, &facts)
}

/// R57 over facts already read.
pub(crate) fn restore_from_facts(
    inputs: &StartInputs,
    facts: &StartFacts,
) -> Result<RestoredAuth, StoreError> {
    let now = inputs.now;
    let current = inputs.pair.generation();
    let mode = StoppedMode::of(inputs.mode);
    // Step 1.
    let clock_valid = clock_is_valid(now);
    let usable = clock_valid && inputs.pair.is_usable(now);
    let stored = facts
        .stored
        .as_ref()
        .map(AuthStatus::from_row)
        .transpose()?;

    // Step 7: the fuse survives while it still describes this pair's history.
    let fuse = restored_fuse(
        stored.as_ref(),
        current,
        facts.current_is_refresh_output,
        now,
        clock_valid,
    );
    // Step 8: a restored pause. Observers never pause (R41 table).
    let pause = fuse.pause_until.filter(|_| !inputs.observer);
    let awaiting = clock_valid && pause.is_some_and(|until| now < until);
    let pause_elapsed = clock_valid && pause.is_some_and(|until| now >= until);

    // Step 5 (b): a refusal outlives a change of credentials and lifts only on
    // the owner's retry or a mode change (R44), whichever unit detected it. A
    // stored mode that differs from the configured one, or C3's report that it
    // reset its own row, clears it: the status written below no longer holds it.
    let refusal = stored.as_ref().and_then(|status| match status.stop() {
        Some(AuthStop::ApiRefused(reason))
            if status.stopped_mode == Some(mode) && !inputs.c3_reset_refusal =>
        {
            Some(reason)
        }
        _ => None,
    });

    // Steps 2 and 3: stops that follow from durable inputs.
    let pair_stop = match inputs.pair {
        PairLoad::Invalid { .. } => Some(AuthStop::SignedOut(SignOutReason::StoredPairInvalid)),
        PairLoad::Loaded { .. } if facts.unresolved_attempt => {
            Some(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown))
        }
        PairLoad::Loaded { .. } => None,
    };
    // Step 5 (a): a stored credential stop holds while its generation is the
    // current one; a different generation means credentials changed while
    // stopped (R44a) and the stop is cleared.
    let stored_stop = stored.as_ref().and_then(|status| match status.stop() {
        Some(stop @ (AuthStop::SignInRequired(_) | AuthStop::SignedOut(_)))
            if status.stopped_generation == Some(current) =>
        {
            Some(stop)
        }
        _ => None,
    });

    // The refusal replaces a stop that steps 3 and 4 would show, because all of
    // them already stop every request (R43) and the refusal is the condition
    // the owner must resolve first; a pair that cannot load at all (step 2)
    // stays, because nothing about it can be used.
    let stop = if matches!(inputs.pair, PairLoad::Invalid { .. }) {
        pair_stop
    } else if let Some(reason) = refusal {
        Some(AuthStop::ApiRefused(reason))
    } else {
        pair_stop.or(stored_stop)
    };
    // A sign-in stop whose token is no longer usable has become a sign-out
    // (R45, step 5 (a)). With an invalid clock the token cannot be judged, so
    // the stored word stands.
    let sign_in_expired = clock_valid && !usable;

    let skew = stored.as_ref().is_some_and(|status| status.clock_skew);
    let status = if let Some(stop) = stop {
        let status = AuthStatus::stopped(stop, current, mode, usable, fuse, skew);
        if sign_in_expired {
            status.with_sign_in_expired()
        } else {
            status
        }
    } else if !clock_valid {
        AuthStatus::idle(AuthState::ClockInvalid, None, fuse, skew)
    } else if let Err(reason) = inputs.configuration {
        // Step 4: not a stored stop; re-evaluated at every start (R5).
        AuthStatus::idle(
            AuthState::ConfigurationError,
            Some(StatusReason::Configuration(reason)),
            fuse,
            skew,
        )
    } else if awaiting {
        AuthStatus::idle(
            AuthState::AwaitingConfirmation,
            Some(StatusReason::SignOut(SignOutReason::AccessTokenRejected)),
            fuse,
            skew,
        )
    } else {
        AuthStatus::ready(fuse, skew)
    };
    debug_assert!(
        status.is_consistent(),
        "restored status {status:?} breaks R52"
    );

    Ok(RestoredAuth {
        status,
        clock_valid,
        token_requests_not_before: restart_floor(now, facts.last_receipt_started_at_ms),
        pause_elapsed,
    })
}

/// R57 step 7. The stored fuse is restored when the current generation is the
/// one it counted or one of the unit's own refresh outputs (a refresh does not
/// reset the fuse, R42, and a crash after a forced refresh must not forget the
/// rejection that caused it, C5R-13) and its last rejection is within the
/// window; otherwise it is zero. A new pair that an external writer stored is
/// neither, so it starts a clean fuse (R42). With an invalid clock the window
/// cannot be judged and the stored fuse is kept untouched, so a clock fault
/// cannot erase it.
fn restored_fuse(
    stored: Option<&AuthStatus>,
    current: Uuid,
    current_is_refresh_output: bool,
    now: i64,
    clock_valid: bool,
) -> FuseSnapshot {
    let Some(fuse) = stored.map(|status| status.fuse) else {
        return FuseSnapshot::default();
    };
    if fuse.consecutive == 0 {
        return FuseSnapshot::default();
    }
    let same_history = fuse.generation == Some(current) || current_is_refresh_output;
    let recent = !clock_valid
        || fuse
            .last_rejection_at
            .is_some_and(|at| rejection_window_open(at, now));
    if same_history && recent {
        fuse
    } else {
        FuseSnapshot::default()
    }
}

/// Whether a restart owes the forced refresh that a counted rejection of a managed unit
/// asked for (hub fix pass 3; B2 round 3, C5-R3-01): the obligation is derived from stored
/// data, never from a flag. It exists exactly when the stored fuse counts a rejection of
/// the current generation, its window is still open, and the stored pair's access token
/// is still the one that was rejected (`current_access` is the digest of it). A refresh
/// that returned a new access token (rotated or not), an external write that changed the
/// token, and a 2xx of the current token (which clears the fuse) all end it by changing
/// the data; an answer that renewed nothing (C5 R45 row 14) leaves it, and a status row
/// written late cannot bring it back, because the decision compares the stored pair, not a
/// written flag. With an invalid clock the window cannot be judged and the fuse is kept
/// whole, as [`restored_fuse`] keeps it.
pub(crate) fn fuse_owes_forced_refresh(
    fuse: &FuseSnapshot,
    current: Uuid,
    current_access: Uuid,
    now: i64,
    clock_valid: bool,
) -> bool {
    fuse.consecutive >= 1
        && fuse.generation == Some(current)
        && fuse.rejected_access == Some(current_access)
        && (!clock_valid
            || fuse
                .last_rejection_at
                .is_some_and(|at| rejection_window_open(at, now)))
}

/// R58 and R57 step 9: the first token request waits until 60 s after the
/// start of the latest token receipt, whatever its outcome, and forced
/// refreshes wait too. The result is whole epoch seconds, rounded up so the
/// minute is never short, and 0 when no receipt is kept. A receipt dated after
/// `now` shows the clock moved backwards since it; the floor is then bounded by
/// a minute from now, because the interval exists to stop a loop of rotations,
/// not to park a refresh for as long as the clock was wrong.
pub(crate) fn restart_floor(now: i64, last_receipt_started_at_ms: Option<i64>) -> i64 {
    let Some(started_ms) = last_receipt_started_at_ms else {
        return 0;
    };
    let interval_ms = MIN_REFRESH_INTERVAL_SECONDS * 1_000;
    let floor = started_ms
        .saturating_add(interval_ms)
        .saturating_add(999)
        .div_euclid(1_000);
    floor.min(now.saturating_add(MIN_REFRESH_INTERVAL_SECONDS))
}

/// The status after an external credential write has been found (R44a, R42):
/// the new pair is loaded, the fuse and every retry field are cleared, and
/// every stop the table of R44 lifts on (a) is lifted; configuration is
/// evaluated again for the new pair (R5, R7) and the clock is looked at again.
/// A refusal stays: new credentials do not lift it (R44, seam S3), except that
/// a pair that cannot load at all is shown first (R57 step 5 (b)). The skew
/// flag is a fact about the host clock and is kept.
///
/// The table of R44 as one function, which the state tests pin; the manager
/// reaches the same status through the fields the write changes
/// (`adopt_stored_pair`), so it has no caller outside the tests.
#[cfg(test)]
pub(crate) fn status_after_external_write(
    previous: &AuthStatus,
    mode: AuthMode,
    pair: &PairLoad,
    configuration: Result<(), ConfigurationReason>,
    clock_valid: bool,
) -> AuthStatus {
    let fuse = FuseSnapshot::default();
    let skew = previous.clock_skew;
    let stored_mode = StoppedMode::of(mode);
    if matches!(pair, PairLoad::Invalid { .. }) {
        return AuthStatus::stopped(
            AuthStop::SignedOut(SignOutReason::StoredPairInvalid),
            pair.generation(),
            stored_mode,
            false,
            fuse,
            skew,
        );
    }
    match previous.stop() {
        Some(stop @ AuthStop::ApiRefused(_)) if !stop.lifted_by_external_write() => {
            return AuthStatus::stopped(stop, pair.generation(), stored_mode, false, fuse, skew);
        }
        _ => {}
    }
    if !clock_valid {
        AuthStatus::idle(AuthState::ClockInvalid, None, fuse, skew)
    } else if let Err(reason) = configuration {
        AuthStatus::idle(
            AuthState::ConfigurationError,
            Some(StatusReason::Configuration(reason)),
            fuse,
            skew,
        )
    } else {
        AuthStatus::ready(fuse, skew)
    }
}
