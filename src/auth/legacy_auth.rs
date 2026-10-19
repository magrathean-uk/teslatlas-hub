// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(all(test, unix))]
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::tesla_stream::StreamRegion;

pub(crate) mod attempt;
pub(crate) mod authority;
pub(crate) mod effective;
mod endpoint;
mod http_date;
pub(crate) mod rejection;
pub(crate) mod schedule;
pub(crate) mod state;
mod vocabulary;
mod wire;

use attempt::SuccessorPair;
pub use endpoint::{AuthEndpointConfig, AuthMode, EndpointSource};
pub(crate) use endpoint::{parse_jwt, trim_token_whitespace};
pub(crate) use rejection::{
    HolderKind, LegacyAuthFuse, RejectionAction, RejectionContext, RequestScope,
};
use state::AuthStop;
pub use vocabulary::{
    ConfigurationReason, LoadFailureState, RefusalReason, RetryReason, SignOutReason,
};
pub(crate) use wire::build_auth_client;
#[cfg(test)]
pub(crate) use wire::{
    AUTH_CONNECT_TIMEOUT, AUTH_TOTAL_TIMEOUT, AnswerBody, Exchange, HUB_USER_AGENT,
    MAX_TOKEN_RESPONSE_BYTES, REFRESH_REQUEST_HEADERS, RawAnswer,
    auth_client_builder_with_timeouts,
};

// This exists only in libtest builds. The parent characterisation test starts
// a clean-environment child with this exact value, then verifies that SIGKILL
// leaves the pre-response durable marker fenced on restart.
#[cfg(all(test, unix))]
pub(crate) const TEST_KILL_AFTER_VALIDATED_ROTATION_ENV: &str =
    "TESLATLAS_HUB_TEST_KILL_AFTER_VALIDATED_ROTATION";
#[cfg(all(test, unix))]
pub(crate) const TEST_KILL_AFTER_VALIDATED_ROTATION_VALUE: &str =
    "phase2-response-before-first-persist-v1";
#[cfg(all(test, unix))]
static TEST_KILL_AFTER_VALIDATED_ROTATION_USED: AtomicBool = AtomicBool::new(false);

#[cfg(all(test, unix))]
fn test_kill_after_validated_rotation_if_armed() {
    if std::env::var(TEST_KILL_AFTER_VALIDATED_ROTATION_ENV)
        .ok()
        .as_deref()
        != Some(TEST_KILL_AFTER_VALIDATED_ROTATION_VALUE)
    {
        return;
    }
    if TEST_KILL_AFTER_VALIDATED_ROTATION_USED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    // SIGKILL gives the child no cleanup/destructor path. Use an absolute
    // system utility so this test-only path remains compatible with the
    // crate-wide `forbid(unsafe_code)` policy.
    let status = std::process::Command::new("/bin/kill")
        .arg("-KILL")
        .arg(std::process::id().to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("test SIGKILL helper must start");
    debug_assert!(status.success());
    std::process::abort();
}

/// Why a legacy auth operation failed (C5 R56). Every `Display` text is fixed
/// and carries no token, URL, response body or vehicle (R53).
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LegacyAuthError {
    #[error("legacy access token is invalid")]
    InvalidAccessToken,
    #[error("legacy access token issuer is invalid")]
    InvalidIssuer,
    #[error("legacy refresh token is invalid")]
    InvalidRefreshToken,
    #[error("legacy auth response is invalid")]
    InvalidResponse,
    #[error("legacy auth response is too large")]
    ResponseTooLarge,
    #[error("legacy auth request failed")]
    Transport,
    #[error("legacy refresh rotation outcome is unknown")]
    RotationOutcomeUnknown,
    #[error("legacy refresh rotation outcome is unknown while persistence authority is active")]
    SensitiveRotationOutcomeUnknown,
    #[error("legacy auth returned HTTP {0}")]
    HttpStatus(u16),
    #[error("legacy auth refresh is deferred")]
    RefreshDeferred,
    #[error("legacy auth clock is invalid")]
    InvalidClock,
    #[error("legacy auth persisted schedule is invalid")]
    InvalidPersistedSchedule,
    #[error("legacy credential persistence failed")]
    Persistence,
    #[error("legacy credential persistence authority is unavailable")]
    SensitivePersistenceUnavailable,
    #[error("legacy credential authority is unavailable")]
    SensitiveAccessUnavailable,
    /// R5: the token's issuer and the configured region name different regions.
    #[error("legacy auth token region conflicts with the configured region")]
    RegionConflict,
    /// R5: Tesla mode, an opaque token and no configured region.
    #[error("legacy auth region is required for this token")]
    RegionRequired,
    /// R7: provider mode without a usable `token_url`.
    #[error("legacy auth token url is invalid")]
    TokenUrlInvalid,
    /// R7: Tesla mode with `token_url` set.
    #[error("legacy auth token url is not allowed in this mode")]
    TokenUrlNotAllowed,
    /// R45 row 8: the refresh token is dead.
    #[error("legacy refresh token was rejected")]
    RefreshTokenRejected,
    /// R45 row 7: the token endpoint refused the client id.
    #[error("legacy auth client was refused by the token endpoint")]
    ClientRefused,
    /// R45 rows 7 (second answer) and 10, R49: the Owner API refused the account.
    #[error("legacy owner api refused the account ({})", .reason.code())]
    OwnerApiRefused { reason: RefusalReason },
    /// R41: the access token was rejected enough times to stop.
    #[error("legacy access token was rejected")]
    AccessTokenRejected,
    /// R8a: an access-token-only provider token was refused.
    #[error("legacy provider token was rejected")]
    ProviderTokenRejected,
    /// R8a: an access-token-only provider token expired.
    #[error("legacy provider token has expired")]
    ProviderTokenExpired,
    /// R45 row 14: the endpoint returned the pair it was given.
    #[error("legacy refresh returned the pair it was given")]
    RefreshNotRenewed,
    /// R14: the pair has no refresh path (observer or access-token-only).
    #[error("legacy refresh is not possible for this pair")]
    RefreshNotPossible,
    /// R41a: one vehicle's 401 was disproved by an account-wide read.
    #[error("legacy vehicle access was rejected")]
    VehicleAccessRejected,
    /// R29, R39: no token can be sent now; try again at `retry_at`.
    #[error("legacy auth is unavailable")]
    AuthUnavailable { retry_at: Option<i64> },
    /// R44: the owner's retry cannot lift this stop.
    #[error("legacy auth retry is not possible ({})", .reason.code())]
    RetryNotPossible { reason: SignOutReason },
    /// R43, R45, R57: the account is signed out for a reason that has no
    /// variant of its own above (a refresh token that was rejected, whose
    /// outcome is unknown or whose answer was unusable, or a stored pair that
    /// is invalid). Every request stops until credentials change.
    #[error("legacy auth is signed out ({})", .reason.code())]
    SignedOut { reason: SignOutReason },
}

impl LegacyAuthError {
    /// The configuration reason when this error puts the account in
    /// `configuration_error` (R5, R7).
    pub fn configuration_reason(&self) -> Option<ConfigurationReason> {
        match self {
            Self::RegionConflict => Some(ConfigurationReason::RegionConflict),
            Self::RegionRequired => Some(ConfigurationReason::RegionRequired),
            Self::InvalidIssuer => Some(ConfigurationReason::IssuerNotAllowed),
            Self::TokenUrlInvalid => Some(ConfigurationReason::TokenUrlInvalid),
            Self::TokenUrlNotAllowed => Some(ConfigurationReason::TokenUrlNotAllowed),
            _ => None,
        }
    }

    /// The state and reason a failure at load puts the account in: a
    /// configuration error (R5, R7) or `signed_out(stored_pair_invalid)` for a
    /// pair that fails the token grammar (R46). `None` for any other error.
    pub fn load_failure_state(&self) -> Option<LoadFailureState> {
        if let Some(reason) = self.configuration_reason() {
            return Some(LoadFailureState::configuration(reason));
        }
        matches!(self, Self::InvalidAccessToken | Self::InvalidRefreshToken)
            .then(|| LoadFailureState::signed_out(SignOutReason::StoredPairInvalid))
    }

    /// True for the errors of an account that is signed out (R43): the lease is
    /// `auth_terminal` exactly then. A configuration error and a refusal stop
    /// the account too, but the lease stays active for them (R52), and
    /// `InvalidAccessToken` and `InvalidRefreshToken` are load failures that the
    /// state, not the error, calls signed out.
    pub fn is_signed_out(&self) -> bool {
        matches!(
            self,
            Self::AccessTokenRejected
                | Self::ProviderTokenRejected
                | Self::ProviderTokenExpired
                | Self::SignedOut { .. }
        )
    }

    /// True exactly for errors that set `configuration_error`, `signed_out` or
    /// `api_refused` (R56): the collector must tell a stop from a wait without
    /// parsing text.
    ///
    /// `RefreshTokenRejected` is not terminal: it sets `sign_in_required`,
    /// which keeps collecting while the access token is usable (R45 rows 4, 6
    /// and 8). `ClientRefused` is not terminal either: its first answer only
    /// starts a confirmation, and the second answer is
    /// `OwnerApiRefused { reason: Client }`. `RetryNotPossible` reports that a
    /// stop continues; it does not set one.
    pub fn is_terminal_auth_failure(&self) -> bool {
        self.configuration_reason().is_some()
            || self.load_failure_state().is_some()
            || matches!(
                self,
                Self::AccessTokenRejected
                    | Self::ProviderTokenRejected
                    | Self::ProviderTokenExpired
                    | Self::OwnerApiRefused { .. }
                    | Self::SignedOut { .. }
            )
    }
}

/// The single in-memory owner-auth authority. It is intentionally not cloneable.
///
/// ```compile_fail
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<teslatlas_hub::legacy_auth::LegacyAuth>();
/// ```
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct LegacyAuth {
    /// A b64token (R46).
    access_token: Zeroizing<String>,
    /// 1*VSCHAR (R46); empty only for an access-token-only provider pair (R8a).
    refresh_token: Zeroizing<String>,
    /// Always `"Bearer"` after validation (R47).
    token_type: String,
    expires_at: i64,
    next_refresh_at: i64,
    /// The exact URL the refresh is POSTed to (R6, R8); `None` only for an
    /// access-token-only provider pair (R8a).
    #[zeroize(skip)]
    token_endpoint: Option<Url>,
    /// Where the endpoint came from; for diagnostics (section 3.1).
    #[zeroize(skip)]
    endpoint_source: EndpointSource,
    /// The issuer a JWT successor must carry (R9); `None` in provider mode.
    #[zeroize(skip)]
    pinned_issuer: Option<String>,
    #[zeroize(skip)]
    region: StreamRegion,
    /// Epoch second after which the access token is not sent (R29); `None`
    /// means unknown. Set at load from the token's own `exp` (R25) and after a
    /// refresh from its lifetime (R21).
    pub(crate) usable_until: Option<i64>,
    /// The earliest next token attempt while a retry is pending (R31, R32).
    /// `retry_at` and `retry_reason` are `Some` exactly while one is pending.
    pub(crate) retry_at: Option<i64>,
    /// The index into the R31 schedule.
    pub(crate) retry_step: u8,
    #[zeroize(skip)]
    pub(crate) retry_reason: Option<RetryReason>,
    pub(crate) startup_refresh_pending: bool,
    /// Send time of the last successful refresh in this process (R19).
    pub(crate) last_success_at: Option<i64>,
    /// Consecutive answers that refused the client id (R45 row 7).
    pub(crate) client_refusals: u8,
    /// The one stop every path sees (R43); the fuse no longer holds it.
    #[zeroize(skip)]
    pub(crate) stop: Option<AuthStop>,
    /// R55: the host clock differs from the server's by more than 300 s.
    pub(crate) clock_skew: bool,
    /// A validated successor that is not yet persisted (R23, R24). It is held
    /// for the single-flight object and zeroised on drop; the previous pair
    /// stays in use until the write succeeds.
    #[zeroize(skip)]
    pub(crate) pending_successor: Option<Box<SuccessorPair>>,
    /// Failed writes of the pending successor: the index into the R24 schedule.
    pub(crate) persist_failures: usize,
    /// When the pending successor is written again (R24).
    pub(crate) persist_retry_at: Option<i64>,
    /// A closing of an attempt that failed and is still owed (C5-S1). No new
    /// attempt begins while it is set.
    #[zeroize(skip)]
    pub(crate) pending_close: Option<attempt::PendingClose>,
    /// Failed tries of the pending closing: the index into the R24 schedule.
    pub(crate) close_failures: usize,
    /// When the pending closing is tried again.
    pub(crate) close_retry_at: Option<i64>,
    /// No token request before this epoch second after a restart (R58); 0 when
    /// no receipt holds one back.
    pub(crate) token_requests_not_before: i64,
    /// No token request before this epoch second while the confirmation pause
    /// of R41 runs; set by the rejection handling.
    pub(crate) hold_until: Option<i64>,
    /// The forced refresh that follows the confirmation pause is owed (R41
    /// count 2): it is made once when the pause has ended, before the next
    /// request, whether or not the schedule has made one due.
    pub(crate) forced_after_hold: bool,
    /// An observer's counted rejection (R28, R41): the Hub waits for the refresh
    /// owner's next pair and sends nothing until the stored generation changes.
    pub(crate) observer_rejected: bool,
    /// Set when the stored schedule failed the table's invariants and the pair
    /// loaded as the imported form instead (R25). Not a secret.
    #[zeroize(skip)]
    schedule_diagnostic: Option<LegacyAuthError>,
    /// Test only: the loopback URL the request is really sent to when the
    /// selected endpoint is a pinned Tesla host (section 8 fixtures). The
    /// selected endpoint stays the one asserted.
    #[cfg(test)]
    #[zeroize(skip)]
    wire_override: Option<Url>,
}

impl fmt::Debug for LegacyAuth {
    /// Only non-secret scheduling facts and the endpoint host (section 3.1,
    /// R53): never a token, a JWT segment or a URL path.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LegacyAuth")
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
            .field(
                "endpoint_host",
                &self.token_endpoint.as_ref().and_then(Url::host_str),
            )
            .field("endpoint_source", &self.endpoint_source)
            .field("region", &self.region)
            .field("usable_until", &self.usable_until)
            .field("retry_at", &self.retry_at)
            .field("retry_step", &self.retry_step)
            .field("retry_reason", &self.retry_reason.map(RetryReason::code))
            .field("stop", &self.stop.map(|stop| stop.reason().code()))
            .field("startup_refresh_pending", &self.startup_refresh_pending)
            .field("persistence_pending", &self.pending_successor.is_some())
            .field("clock_skew", &self.clock_skew)
            .finish()
    }
}

/// Whether the stored schedule is the imported form `(0, 0)`, a sound refreshed
/// form `expires_at > next_refresh_at > 0`, or neither (R25).
enum StoredSchedule {
    Imported,
    Refreshed,
    Invalid,
}

fn classify_stored_schedule(expires_at: i64, next_refresh_at: i64) -> StoredSchedule {
    if expires_at == 0 && next_refresh_at == 0 {
        StoredSchedule::Imported
    } else if next_refresh_at > 0 && expires_at > next_refresh_at {
        StoredSchedule::Refreshed
    } else {
        StoredSchedule::Invalid
    }
}

/// R25: the epoch second after which a stored access token is not sent, or
/// `None` when unknown. Both forms apply the token's own `exp`, less the skew
/// allowance; a refreshed pair is also bounded by its stored expiry.
fn usable_until_of(access_token: &str, refreshed: bool, expires_at: i64) -> Option<i64> {
    let exp_limit = parse_jwt(access_token)
        .and_then(|claims| claims.exp)
        .map(|exp| exp - state::SKEW_ALLOWANCE_SECONDS);
    if refreshed {
        Some(exp_limit.map_or(expires_at, |limit| limit.min(expires_at)))
    } else {
        exp_limit
    }
}

/// [`usable_until_of`] for a stored pair read without loading the manager: the
/// stored schedule decides the form, exactly as `from_persisted_state` does
/// (an unsound schedule loads as the imported form). Surrounding whitespace is
/// trimmed as the load trims it. Read-only, secret-free output (B2
/// clarification 3, C5 effective auth).
pub(crate) fn stored_usable_until(
    access_token: &str,
    expires_at: i64,
    next_refresh_at: i64,
) -> Option<i64> {
    let refreshed = matches!(
        classify_stored_schedule(expires_at, next_refresh_at),
        StoredSchedule::Refreshed
    );
    usable_until_of(
        endpoint::trim_token_whitespace(access_token),
        refreshed,
        expires_at,
    )
}

/// Whether a stored pair passes the token grammar of R46 under `mode`, with the
/// same trimming and the same check the load applies (`PairLoad::Invalid` is the
/// opposite answer). Configuration faults are not part of it: they never make a
/// pair invalid. Read-only and secret-free output (B2 clarification 3, C5
/// effective auth).
pub(crate) fn stored_pair_passes_grammar(
    access_token: &str,
    refresh_token: &str,
    mode: state::StoppedMode,
) -> bool {
    let mode = match mode {
        state::StoppedMode::Legacy => AuthMode::Tesla,
        state::StoppedMode::Provider => AuthMode::Provider,
    };
    endpoint::check_token_grammar(
        endpoint::trim_token_whitespace(access_token),
        endpoint::trim_token_whitespace(refresh_token),
        mode,
    )
    .is_ok()
}

impl LegacyAuth {
    /// Build the pair for a one-off use such as native setup, in imported form.
    /// Tokens are trimmed and checked (R46) and the endpoint is selected
    /// (R4 to R8a) before any network use (R5). No startup refresh is pending:
    /// the caller reads once and never refreshes.
    pub fn from_access_token(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        endpoint: &AuthEndpointConfig,
    ) -> Result<Self, LegacyAuthError> {
        let mut auth = Self::from_persisted_state(access_token, refresh_token, 0, 0, endpoint)?;
        auth.startup_refresh_pending = false;
        Ok(auth)
    }

    /// Load a stored pair (R25, R46).
    ///
    /// Surrounding ASCII whitespace is trimmed from both tokens in memory (the
    /// generation the caller computed from the stored bytes is unchanged),
    /// then the tokens are checked (R46) and the endpoint is selected (R4 to
    /// R8a). A schedule that fails the table's invariants loads as the imported
    /// form with an `InvalidPersistedSchedule` diagnostic.
    pub(crate) fn from_persisted_state(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        expires_at: i64,
        next_refresh_at: i64,
        endpoint: &AuthEndpointConfig,
    ) -> Result<Self, LegacyAuthError> {
        let access_token = Zeroizing::new(access_token.into());
        let refresh_token = Zeroizing::new(refresh_token.into());
        let access_token =
            Zeroizing::new(endpoint::trim_token_whitespace(&access_token).to_owned());
        let refresh_token =
            Zeroizing::new(endpoint::trim_token_whitespace(&refresh_token).to_owned());
        let selection = endpoint::select_endpoint(&access_token, &refresh_token, endpoint)?;
        let (expires_at, next_refresh_at, schedule_diagnostic, refreshed) =
            match classify_stored_schedule(expires_at, next_refresh_at) {
                StoredSchedule::Refreshed => (expires_at, next_refresh_at, None, true),
                StoredSchedule::Imported => (0, 0, None, false),
                StoredSchedule::Invalid => {
                    (0, 0, Some(LegacyAuthError::InvalidPersistedSchedule), false)
                }
            };
        let usable_until = usable_until_of(&access_token, refreshed, expires_at);
        Ok(Self {
            startup_refresh_pending: !refreshed && !refresh_token.is_empty(),
            access_token,
            refresh_token,
            token_type: "Bearer".to_owned(),
            expires_at,
            next_refresh_at,
            token_endpoint: selection.token_endpoint,
            endpoint_source: selection.source,
            pinned_issuer: selection.pinned_issuer,
            region: selection.region,
            usable_until,
            retry_at: None,
            retry_step: 0,
            retry_reason: None,
            last_success_at: None,
            client_refusals: 0,
            stop: None,
            clock_skew: false,
            pending_successor: None,
            persist_failures: 0,
            persist_retry_at: None,
            pending_close: None,
            close_failures: 0,
            close_retry_at: None,
            token_requests_not_before: 0,
            hold_until: None,
            forced_after_hold: false,
            observer_rejected: false,
            schedule_diagnostic,
            #[cfg(test)]
            wire_override: None,
        })
    }

    /// A pair that holds no token, for an account that stopped before any
    /// request could be made (C5-B2): a configuration error (R5, R7), a stored
    /// pair that fails the grammar (R46) or an unresolved attempt that is
    /// restored as a stop (R57). The collector must start in these states, take
    /// its lease and wait for the owner (R43, R39), so the manager exists
    /// without a usable pair. Every path that could hand out its empty token is
    /// stopped by the state that made it, and [`token_available`] refuses an
    /// empty token as well, so nothing can be sent with it. `region` is the
    /// configured region, or the global one, which only names the data hosts the
    /// stopped collector does not call.
    pub(crate) fn inert(region: StreamRegion) -> Self {
        Self {
            access_token: Zeroizing::new(String::new()),
            refresh_token: Zeroizing::new(String::new()),
            token_type: "Bearer".to_owned(),
            expires_at: 0,
            next_refresh_at: 0,
            token_endpoint: None,
            endpoint_source: EndpointSource::ConfiguredRegion,
            pinned_issuer: None,
            region,
            usable_until: None,
            retry_at: None,
            retry_step: 0,
            retry_reason: None,
            startup_refresh_pending: false,
            last_success_at: None,
            client_refusals: 0,
            stop: None,
            clock_skew: false,
            pending_successor: None,
            persist_failures: 0,
            persist_retry_at: None,
            pending_close: None,
            close_failures: 0,
            close_retry_at: None,
            token_requests_not_before: 0,
            hold_until: None,
            forced_after_hold: false,
            observer_rejected: false,
            schedule_diagnostic: None,
            #[cfg(test)]
            wire_override: None,
        }
    }

    /// The exact URL a refresh is POSTed to, `None` for an access-token-only
    /// provider pair (R6, R8, R8a).
    pub(crate) fn token_endpoint(&self) -> Option<&Url> {
        self.token_endpoint.as_ref()
    }

    /// Where the token endpoint came from (section 3.1).
    pub fn endpoint_source(&self) -> EndpointSource {
        self.endpoint_source
    }

    /// The epoch second after which the access token is not sent, if known.
    pub fn usable_until(&self) -> Option<i64> {
        self.usable_until
    }

    /// `InvalidPersistedSchedule` when the stored schedule was unsound and the
    /// pair loaded as the imported form (R25).
    pub fn schedule_diagnostic(&self) -> Option<&LegacyAuthError> {
        self.schedule_diagnostic.as_ref()
    }

    /// The URL a request is really sent to: the selected endpoint, or in tests
    /// the loopback stand-in for a pinned Tesla host.
    pub(crate) fn request_url(&self) -> Option<Url> {
        #[cfg(test)]
        if let (Some(_), Some(wire_override)) = (&self.token_endpoint, &self.wire_override) {
            return Some(wire_override.clone());
        }
        self.token_endpoint.clone()
    }

    /// The issuer a JWT successor must carry (R9); `None` in provider mode.
    pub(crate) fn pinned_issuer(&self) -> Option<&str> {
        self.pinned_issuer.as_deref()
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    pub fn token_type(&self) -> &str {
        &self.token_type
    }

    pub fn region(&self) -> StreamRegion {
        self.region
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    pub fn next_refresh_at(&self) -> i64 {
        self.next_refresh_at
    }

    pub fn retry_at(&self) -> Option<i64> {
        self.retry_at
    }

    /// Whether a refresh is due at `now` (R26, R27): owed to an imported pair
    /// until one succeeds, due when a retry's time is reached, and due for a
    /// pair of the refreshed form by the rule of R27. This is the due rule only;
    /// whether a request may start is the gates' (R16).
    pub fn refresh_due(&self, now: SystemTime) -> Result<bool, LegacyAuthError> {
        Ok(self.is_due(epoch_seconds(now)?))
    }

    /// Test constructor (section 3.4): endpoint is the issuer with `token`
    /// appended, region Global, imported form, no token checks.
    #[cfg(test)]
    pub(crate) fn for_test(issuer: Url, access_token: &str, refresh_token: &str) -> Self {
        let selection = endpoint::test_override_selection(&issuer);
        Self {
            access_token: Zeroizing::new(access_token.to_owned()),
            refresh_token: Zeroizing::new(refresh_token.to_owned()),
            token_type: "Bearer".to_owned(),
            expires_at: 0,
            next_refresh_at: 0,
            token_endpoint: selection.token_endpoint,
            endpoint_source: selection.source,
            pinned_issuer: selection.pinned_issuer,
            region: selection.region,
            usable_until: None,
            retry_at: None,
            retry_step: 0,
            retry_reason: None,
            startup_refresh_pending: false,
            last_success_at: None,
            client_refusals: 0,
            stop: None,
            clock_skew: false,
            pending_successor: None,
            persist_failures: 0,
            persist_retry_at: None,
            pending_close: None,
            close_failures: 0,
            close_retry_at: None,
            token_requests_not_before: 0,
            hold_until: None,
            forced_after_hold: false,
            observer_rejected: false,
            schedule_diagnostic: None,
            wire_override: None,
        }
    }

    /// Test only: send to `loopback` instead of the selected Tesla host while
    /// the selection stays the one asserted (section 8 fixtures).
    #[cfg(test)]
    pub(crate) fn with_wire_override_for_test(mut self, loopback: Url) -> Self {
        self.wire_override = Some(loopback);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_schedule(mut self, expires_at: i64, next_refresh_at: i64) -> Self {
        assert!(next_refresh_at > 0 && next_refresh_at < expires_at);
        self.expires_at = expires_at;
        self.next_refresh_at = next_refresh_at;
        self
    }
}

fn epoch_seconds(now: SystemTime) -> Result<i64, LegacyAuthError> {
    now.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or(LegacyAuthError::InvalidClock)
}

#[cfg(test)]
#[path = "legacy_auth/tests.rs"]
mod tests;
