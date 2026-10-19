// SPDX-License-Identifier: AGPL-3.0-only

// Errors, back-off, rate limits and auth outcomes (C3 part K5): the one
// classifier of C3 R42, the account waits of R43, R45 and R47, the 408 rule of
// R46 and the failure paths of the scheduler that use them (R22, R56, R57).
//
// The classes whose action is a hold or an account state (R48 to R53, part K6)
// are classified here and acted on in `scheduler_holds.rs`.

/// The default wait after a 429 that names none (C3 R45, `K_429_DEFAULT`):
/// Tesla counts its limits per minute, so without a value from the server one
/// full minute is the shortest wait that guarantees a new window. Repeated 429s
/// grow from there through [`backoff_delay`].
const K_429_DEFAULT: Duration = Duration::from_secs(60);

use crate::fleet_api::{MOBILE_ACCESS_DISABLED_CODE, VEHICLE_IN_SERVICE_CODE};

// ---------------------------------------------------------------------------
// Classification (C3 R42, R43).
// ---------------------------------------------------------------------------

/// Classify a failed request (C3 R42): one function for every scheduler entry
/// point, the first matching row of the table applying. `request` says what was
/// asked, because the same status means different things for the account-wide
/// list and for one car's record, read or probe; `mode` tells Legacy from
/// Provider, because a refusal of the Owner API on Tesla's host is Tesla
/// withdrawing it, and a refusal on a configured provider is a matter at the
/// provider (R49). The spec's signature has no `mode`: it is added because the
/// rows that name "Legacy" and "Provider" cannot be applied without it.
///
/// The credential unit's own outcomes (C5 X1, R56) have their rows here:
/// `AuthUnavailable` is `AuthWait`, `VehicleAccessRejected` a client error of
/// the car, `OwnerApiRefused` the refusal of the mode, and every other terminal
/// error (`signed_out`, `configuration_error`) a `CredentialStop`.
fn classify(
    error: &CollectorError,
    request: RequestKind,
    mode: crate::config::CollectorMode,
) -> ErrorClass {
    let provider = mode == crate::config::CollectorMode::Provider;
    let class = match error {
        // The loss of the process's sensitive access is not a collection error
        // and not an account state (R42, R47, B2 clarification): it says the
        // process is no longer the admitted Hub instance, not that a credential
        // failed. The loops end the run on it before any scheduler entry point
        // sees it, as for a store fault (R6), so the class is never acted on.
        CollectorError::SensitiveAccessUnavailable => ErrorClass::Transient,
        CollectorError::FleetCredential(error) if error.is_admission_loss() => {
            ErrorClass::Transient
        }
        CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(error))
        | CollectorError::LegacyAuthManager(error)
            if error.is_sensitive_access_failure() =>
        {
            ErrorClass::Transient
        }
        // A blown fuse ("signed out") is a terminal credential outcome: the
        // account stops (R47).
        CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn) => {
            ErrorClass::CredentialStop
        }
        // The credential unit's own error, however it was wrapped (C5 R56, X1).
        CollectorError::LegacyAuth(error)
        | CollectorError::LegacyAuthManager(LegacyAuthManagerError::Auth(error))
        | CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
            error,
        ))) => classify_legacy_auth(error, provider),
        // Any other authentication outcome, after the credential unit's own
        // handling (R47).
        CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(_))
        | CollectorError::LegacyAuthManager(_)
        | CollectorError::FleetCredential(_) => ErrorClass::Auth,
        CollectorError::OwnerApi(error)
        | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(error)) => {
            classify_owner(error, request, provider)
        }
        CollectorError::FleetApi(error) => classify_fleet(error, request),
        // A store, lifecycle or projection error is not a collection error (R6,
        // the loops propagate it); anything else is a local fault of one pass.
        // Both take the ordinary back-off.
        _ => ErrorClass::Transient,
    };
    if request == RequestKind::List {
        list_class(class)
    } else {
        class
    }
}

/// The class of one of the credential unit's errors (C5 R56, X1).
fn classify_legacy_auth(error: &LegacyAuthError, provider: bool) -> ErrorClass {
    match error {
        LegacyAuthError::AuthUnavailable { .. } => ErrorClass::AuthWait,
        // One car's access was refused while the account's token works.
        LegacyAuthError::VehicleAccessRejected => ErrorClass::ClientError,
        LegacyAuthError::OwnerApiRefused { .. } => owner_refusal(provider),
        // The authority to use the credential is gone.
        LegacyAuthError::SensitiveAccessUnavailable
        | LegacyAuthError::SensitiveRotationOutcomeUnknown => ErrorClass::CredentialStop,
        // A write of a successor that failed is a wait for the write (C5 R45 row 5,
        // R24): the unit holds the successor and retries, and never reports it as
        // an error of a request, so this row is only the safe reading of one that
        // arrives: the ordinary authentication back-off.
        LegacyAuthError::Persistence | LegacyAuthError::SensitivePersistenceUnavailable => {
            ErrorClass::Auth
        }
        error if error.is_terminal_auth_failure() => ErrorClass::CredentialStop,
        _ => ErrorClass::Auth,
    }
}

/// The refusal class of a withdrawn Owner wire: Tesla withdrawing the Owner API
/// (Legacy) or a provider refusing the Hub (Provider), R42 and R49.
const fn owner_refusal(provider: bool) -> ErrorClass {
    if provider {
        ErrorClass::ProviderRefusal
    } else {
        ErrorClass::OwnerApiRefusal
    }
}

fn classify_owner(error: &OwnerApiError, request: RequestKind, provider: bool) -> ErrorClass {
    match error {
        OwnerApiError::RateLimited { .. } => ErrorClass::RateLimited,
        OwnerApiError::VehicleNotFound => ErrorClass::NotFound,
        OwnerApiError::VehicleInService => ErrorClass::InService,
        OwnerApiError::Forbidden { fleet_api_named } => {
            owner_forbidden(*fleet_api_named, request, provider)
        }
        OwnerApiError::Gone => owner_refusal(provider),
        OwnerApiError::RequestTimeout | OwnerApiError::Transport => ErrorClass::Unreachable,
        OwnerApiError::StreamPowerNotConfirmed => ErrorClass::GateNotConfirmed,
        OwnerApiError::CredentialAuthorityUnavailable | OwnerApiError::LegacyAuth => {
            ErrorClass::Auth
        }
        OwnerApiError::HttpStatus(status) => owner_status(*status, request, provider),
        // The wire answered in a shape the Hub does not accept, or a request
        // could not be built: the response is discarded and the failure counts
        // (R55).
        OwnerApiError::ResponseTooLarge
        | OwnerApiError::ResponseRead
        | OwnerApiError::InvalidResponseEnvelope
        | OwnerApiError::InvalidVehicleListCount
        | OwnerApiError::InvalidVehicleRecord
        | OwnerApiError::InvalidVehicleDataEnvelope
        | OwnerApiError::SensitiveDataInResponse
        | OwnerApiError::InvalidEndpoint
        | OwnerApiError::InvalidCommandResponse
        | OwnerApiError::InvalidCommand
        | OwnerApiError::CommandRejected => ErrorClass::Protocol,
    }
}

/// An Owner-wire 403 (R42, R48): on the account-wide list a Fleet-named body is
/// the refusal and a plain one is left to R48 (c), which first asks the
/// credential unit for one refresh (`list_with_refusal_check_using`) and so classifies as a client error
/// here; on a record, read or probe a Fleet-named body refuses that car and a
/// plain one is a client error of that car.
const fn owner_forbidden(
    fleet_api_named: bool,
    request: RequestKind,
    provider: bool,
) -> ErrorClass {
    match (request, fleet_api_named) {
        (RequestKind::List, true) => owner_refusal(provider),
        (_, false) => ErrorClass::ClientError,
        (_, true) => ErrorClass::AccessRefused,
    }
}

/// The class of an Owner-wire status that carries no typed variant (R42).
fn owner_status(status: u16, request: RequestKind, provider: bool) -> ErrorClass {
    match status {
        401 => ErrorClass::Auth,
        402 if provider => ErrorClass::ProviderRefusal,
        403 => owner_forbidden(false, request, provider),
        404 => ErrorClass::NotFound,
        408 => ErrorClass::Unavailable,
        410 => owner_refusal(provider),
        429 => ErrorClass::RateLimited,
        451 => ErrorClass::LegalPrivacy,
        500..=599 => ErrorClass::Transient,
        400..=499 => ErrorClass::ClientError,
        // A status that is neither an error of the client nor of the server (a
        // redirect, which the client never follows) is not an answer the Hub
        // understands.
        _ => ErrorClass::Protocol,
    }
}

fn classify_fleet(error: &FleetApiError, request: RequestKind) -> ErrorClass {
    match error {
        FleetApiError::RateLimited { .. } => ErrorClass::RateLimited,
        FleetApiError::RequestTimeout
        | FleetApiError::RequestNotSent
        | FleetApiError::Transport => ErrorClass::Unreachable,
        FleetApiError::HttpStatus(status) => fleet_status(*status, None, request),
        FleetApiError::ProviderHttpStatus { status, error, .. } => {
            fleet_status(*status, Some(error), request)
        }
        FleetApiError::ResponseTooLarge
        | FleetApiError::ResponseRead
        | FleetApiError::InvalidResponse
        | FleetApiError::InvalidEndpoint
        | FleetApiError::InvalidCommand
        | FleetApiError::CommandRejected
        | FleetApiError::InvalidTelemetryConfig
        | FleetApiError::TelemetryConfigTooLarge
        | FleetApiError::CommandProxyUnavailable
        | FleetApiError::InvalidChargeLimit => ErrorClass::Protocol,
    }
}

/// The class of a Fleet status (R42). `code` is the allowlisted error code the
/// client kept, if any: `mobile_access_disabled` for a 401 and
/// `vehicle_in_service` for a 405 are the two the table reads.
fn fleet_status(status: u16, code: Option<&str>, request: RequestKind) -> ErrorClass {
    match status {
        401 if code == Some(MOBILE_ACCESS_DISABLED_CODE) => ErrorClass::MobileAccessDisabled,
        401 => ErrorClass::Auth,
        402 => ErrorClass::Billing,
        403 if request == RequestKind::List => ErrorClass::FleetAccess,
        403 => ErrorClass::AccessRefused,
        404 => ErrorClass::NotFound,
        405 if code == Some(VEHICLE_IN_SERVICE_CODE) => ErrorClass::InService,
        408 => ErrorClass::Unavailable,
        412 | 421 => ErrorClass::Config,
        423 => ErrorClass::Locked,
        429 => ErrorClass::RateLimited,
        451 => ErrorClass::LegalPrivacy,
        500..=599 => ErrorClass::Transient,
        400..=499 => ErrorClass::ClientError,
        _ => ErrorClass::Protocol,
    }
}

/// A list names no car, so the classes that are about one car become the
/// account-scope class of the same kind (R43): a 408 is a slow answer, and any
/// other car-level refusal is a client error of the request.
const fn list_class(class: ErrorClass) -> ErrorClass {
    match class {
        ErrorClass::Unavailable => ErrorClass::Transient,
        ErrorClass::NotFound
        | ErrorClass::InService
        | ErrorClass::MobileAccessDisabled
        | ErrorClass::LegalPrivacy
        | ErrorClass::AccessRefused => ErrorClass::ClientError,
        other => other,
    }
}

impl ErrorClass {
    /// Whether the class stops or delays every car's requests (C3 R43): the
    /// credential, rate-limit, reachability and account-state classes always;
    /// `Transient`, `Protocol` and `ClientError` only for a list, which no car
    /// can be checked without.
    #[cfg(test)]
    const fn is_account_scope(self, request: RequestKind) -> bool {
        match self {
            Self::Auth
            | Self::AuthWait
            | Self::CredentialStop
            | Self::RateLimited
            | Self::Unreachable
            | Self::OwnerApiRefusal
            | Self::ProviderRefusal
            | Self::FleetAccess
            | Self::Billing
            | Self::Config
            | Self::Locked => true,
            Self::Transient | Self::Protocol | Self::ClientError => {
                matches!(request, RequestKind::List)
            }
            Self::Unavailable
            | Self::NotFound
            | Self::InService
            | Self::AccessRefused
            | Self::MobileAccessDisabled
            | Self::LegalPrivacy
            | Self::GateNotConfirmed => false,
        }
    }

    /// The account classes that make the account wait (R43, R45, R47) or stop on a
    /// credential. The refusal, billing, configuration and lock classes change an
    /// account state instead (R48 to R50, `is_account_state_class`).
    const fn is_account_wait_class(self) -> bool {
        matches!(
            self,
            Self::Auth
                | Self::AuthWait
                | Self::CredentialStop
                | Self::RateLimited
                | Self::Unreachable
        )
    }
}

// ---------------------------------------------------------------------------
// Account waits and the failure paths of the scheduler (C3 R43, R45, R46, R47,
// R56, R57).
// ---------------------------------------------------------------------------

impl VehicleScheduler {
    /// Whether a wait of `reason` is running at `now`.
    fn wait_running(&self, reason: WaitReason, now: Instant) -> bool {
        self.account
            .wait
            .is_some_and(|(until, current)| current == reason && now < until)
    }

    /// Start or lengthen the account's wait: a longer wait of any reason stays,
    /// so one answer never shortens another's wait.
    fn extend_wait(&mut self, until: Instant, reason: WaitReason) {
        match self.account.wait {
            Some((current, _)) if current >= until => {}
            _ => self.account.wait = Some((until, reason)),
        }
    }

    /// What is left of the account's wait at `now`.
    fn wait_remaining(&self, now: Instant) -> Duration {
        self.account.wait.map_or(Duration::ZERO, |(until, _)| {
            until.saturating_duration_since(now)
        })
    }

    /// The decision-log line of an account wait that begins (C3 R9, R10): once
    /// per episode, never per repeated answer.
    fn log_account_wait(
        &self,
        to: &'static str,
        reason: &'static str,
        class: ErrorClass,
        http_status: Option<u16>,
        delay: Duration,
    ) {
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = self.account.state.code(),
            to,
            reason,
            next_in_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            gated = false,
            class = ?class,
            http_status,
            "collector transition"
        );
    }

    /// A 429 (C3 R45): the account waits until `now + d`, where `d` is the wait
    /// Tesla asked for (at most `K_RETRY_AFTER_MAX`) or, when it named none, the
    /// longer of `K_429_DEFAULT` and the back-off of the count of 429s. No car is
    /// sent anything before it, and the end is persisted by the account view.
    ///
    /// The answers of requests that were in flight together are one piece of
    /// evidence: while a rate-limit wait is already running, a further 429 only
    /// lengthens it and does not count again. Reason: three cars read in one
    /// tick that all get 429 must not grow the back-off three steps at once.
    fn rate_limited(&mut self, retry_after_seconds: u64, now: Instant) -> Duration {
        let burst = self.wait_running(WaitReason::RateLimited, now);
        if !burst {
            self.account.consecutive_429 = self.account.consecutive_429.saturating_add(1);
        }
        let named = Duration::from_secs(retry_after_seconds).min(K_RETRY_AFTER_MAX);
        let delay = if !named.is_zero() {
            named
        } else if burst {
            Duration::ZERO
        } else {
            let draw = self.jitter.draw();
            K_429_DEFAULT.max(backoff_delay(
                self.account.consecutive_429,
                self.timing.max_backoff,
                draw,
            ))
        };
        self.extend_wait(deadline_after(now, delay), WaitReason::RateLimited);
        if !burst {
            self.log_account_wait(
                "rate_limit_wait",
                "rate_limited",
                ErrorClass::RateLimited,
                Some(429),
                delay,
            );
        }
        self.wait_remaining(now)
    }

    /// A request that produced no HTTP status (C3 R43): the account waits
    /// `backoff_delay(consecutive_unreachable)` with reason `TeslaUnreachable`.
    /// No car's failure count changes and every phase is kept, because below
    /// HTTP the Hub cannot tell a host network fault from a per-request one.
    /// The results of one tick count once, as for a 429.
    fn unreachable(&mut self, now: Instant) -> Duration {
        let burst = self.wait_running(WaitReason::TeslaUnreachable, now);
        if !burst {
            self.account.consecutive_unreachable =
                self.account.consecutive_unreachable.saturating_add(1);
            let draw = self.jitter.draw();
            let delay = backoff_delay(
                self.account.consecutive_unreachable,
                self.timing.max_backoff,
                draw,
            );
            self.extend_wait(deadline_after(now, delay), WaitReason::TeslaUnreachable);
            self.log_account_wait(
                "unreachable_wait",
                "tesla_unreachable",
                ErrorClass::Unreachable,
                None,
                delay,
            );
        }
        self.wait_remaining(now)
    }

    /// The credential unit's own time for a wait it asked for (C5 R39, R56): the
    /// whole account waits until `retry_at`, at least one second and at most the
    /// longest wait any rule sets, or a minute when the unit names no time (its
    /// own look-again time for an observer and an invalid clock, R39).
    fn legacy_auth_retry_after(&self, error: &CollectorError) -> Option<Duration> {
        let LegacyAuthError::AuthUnavailable { retry_at } = legacy_auth_error_of(error)? else {
            return None;
        };
        let wall_seconds = self.wall_or_last().div_euclid(1_000);
        let seconds = retry_at
            .map_or(60, |at| at.saturating_sub(wall_seconds))
            .clamp(1, 3_600);
        Some(Duration::from_secs(u64::try_from(seconds).unwrap_or(1)))
    }

    /// The credential unit's outcome in a collector error (C5 R56, C3 R42, R47),
    /// if it is one of the four rows of the seam in `scheduler_holds`.
    fn credential_outcome_for(&self, error: &CollectorError) -> Option<CredentialOutcome> {
        let error = legacy_auth_error_of(error)?;
        match error {
            LegacyAuthError::AuthUnavailable { .. } => Some(CredentialOutcome::AuthUnavailable {
                retry_after: self.legacy_auth_retry_after(&CollectorError::LegacyAuth(
                    error.clone(),
                ))?,
            }),
            LegacyAuthError::OwnerApiRefused { reason } => {
                Some(CredentialOutcome::OwnerApiRefused(match reason {
                    RefusalReason::OwnerApi => CredentialRefusal::OwnerApi,
                    RefusalReason::Client => CredentialRefusal::Client,
                }))
            }
            LegacyAuthError::VehicleAccessRejected => Some(CredentialOutcome::VehicleAccessRejected),
            error if error.is_terminal_auth_failure() => Some(CredentialOutcome::TerminalStop),
            _ => None,
        }
    }

    /// An authentication outcome that is not a stop (C3 R47): the account waits
    /// `backoff_delay(auth_failures)` with reason `AuthWait`, counting no car's
    /// failure. `credential_retry` is the credential unit's own retry time when
    /// it has one (`fleet_refresh_retry_delay`: a Fleet token refresh that was not
    /// sent), which replaces the back-off (R57). The credential unit refreshes in
    /// the background and the next request waits for it, so the wait only spaces
    /// the retry; the answers of one tick count once.
    fn auth_failed(
        &mut self,
        class: ErrorClass,
        credential_retry: Option<Duration>,
        http_status: Option<u16>,
        now: Instant,
    ) -> Duration {
        let burst = self.wait_running(WaitReason::AuthWait, now);
        if !burst {
            self.account.auth_failures = self.account.auth_failures.saturating_add(1);
            let draw = self.jitter.draw();
            let delay = credential_retry.unwrap_or_else(|| {
                backoff_delay(self.account.auth_failures, self.timing.max_backoff, draw)
            });
            self.extend_wait(deadline_after(now, delay), WaitReason::AuthWait);
            self.log_account_wait("auth_wait", "auth_wait", class, http_status, delay);
        }
        self.wait_remaining(now)
    }

    /// The credential unit says it can hand out a token (C5 R39 `Ready`): an
    /// authentication wait that was set for its time has no reason left, so it
    /// ends and a list is due at once (C3 R47). A rate-limit or reachability wait
    /// is Tesla's and runs its course.
    fn end_auth_wait(&mut self, now: Instant) {
        if matches!(self.account.wait, Some((_, WaitReason::AuthWait))) {
            self.account.wait = None;
            self.account.list_wanted = Some(now);
            tracing::info!(
                target: "collector.decision",
                vehicle = "account",
                from = "auth_wait",
                to = AccountState::Collecting.code(),
                reason = "auth_ready",
                gated = false,
                "collector transition"
            );
        }
    }

    /// The account stops on a terminal credential outcome (C3 R47): state
    /// `CredentialStop`, so no request is sent, and an `auth_terminal` gap opens
    /// with the next tick's rows (R8). The state is derived and never stored by
    /// the scheduler (R4); the credential unit owns the lifting
    /// ([`Self::clear_credential_stop`]). Only a collecting account stops: a
    /// refusal state of C3's own already sends nothing.
    fn enter_credential_stop(&mut self, now: Instant) {
        let _ = now;
        if self.account.state != AccountState::Collecting {
            return;
        }
        let wall_ms = self.wall_or_last();
        self.account.state = AccountState::CredentialStop;
        self.account.state_reason = "auth_terminal";
        self.account.state_since_ms = wall_ms;
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = AccountState::Collecting.code(),
            to = AccountState::CredentialStop.code(),
            reason = "auth_terminal",
            class = ?ErrorClass::CredentialStop,
            "collector transition"
        );
    }

    /// The account stops on an identified unresolved Fleet refresh (C5 R47, C3
    /// R47, B2 clarification 3): the same in-memory `CredentialStop` and stored
    /// state as any credential stop, which a restart does not restore, but its one
    /// transition event carries the attention word the owner is shown, not
    /// `auth_terminal`: the lease stays active and the stop is the reconnect
    /// attention. Only a collecting account enters the state, so a repeated error
    /// emits nothing again.
    fn enter_fleet_refresh_stop(&mut self, now: Instant) {
        let _ = now;
        if self.account.state != AccountState::Collecting {
            return;
        }
        self.enter_fleet_refresh_stop_now();
    }

    /// The same stop at the start of a run whose startup found the identified
    /// unresolved refresh (C3 R47, B2 clarification 3 and the coordinator ruling
    /// on retained policy): it is derived in memory over whatever the ordinary
    /// start loaded, including a retained cap or billing stop. The stored account
    /// view stays as the store holds it (`account_view` returns it while the
    /// credential stop lasts), as do the waits, the probe schedule, the
    /// reservations and the counts; no request, token or list goes out while the
    /// unresolved evidence remains. A lift of the stop (the owner's retry) then
    /// re-evaluates the policy before anything is collected.
    fn enter_fleet_refresh_stop_at_start(&mut self) {
        if self.account.state == AccountState::CredentialStop {
            return;
        }
        self.enter_fleet_refresh_stop_now();
    }

    fn enter_fleet_refresh_stop_now(&mut self) {
        let from = self.account.state;
        let wall_ms = self.wall_or_last();
        self.account.state = AccountState::CredentialStop;
        self.account.state_reason = "auth_terminal";
        self.account.state_since_ms = wall_ms;
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = from.code(),
            to = AccountState::CredentialStop.code(),
            reason = crate::db::CollectionAttentionCode::CollectorFleetRefreshOutcomeUnknown.code(),
            class = ?ErrorClass::CredentialStop,
            "collector transition"
        );
    }

    /// The credential unit lifted its stop (C3 R47): the account collects again
    /// and one list is due at once.
    fn clear_credential_stop(&mut self, now: Instant) {
        if self.account.state != AccountState::CredentialStop {
            return;
        }
        let wall_ms = self.wall_or_last();
        self.account.state = AccountState::Collecting;
        self.account.state_reason = "collecting";
        self.account.state_since_ms = wall_ms;
        self.account.list_wanted = Some(now);
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = AccountState::CredentialStop.code(),
            to = AccountState::Collecting.code(),
            reason = "collecting",
            "collector transition"
        );
    }

    /// A request got an HTTP status, even an error (C3 R43): the account is
    /// reachable, so a `TeslaUnreachable` wait ends and its count resets.
    fn note_answered(&mut self, now: Instant) {
        let _ = now;
        if self.account.consecutive_unreachable == 0 {
            return;
        }
        self.account.consecutive_unreachable = 0;
        if matches!(self.account.wait, Some((_, WaitReason::TeslaUnreachable))) {
            self.account.wait = None;
        }
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = "unreachable_wait",
            to = AccountState::Collecting.code(),
            reason = "tesla_reachable",
            gated = false,
            "collector transition"
        );
    }

    /// A request succeeded (C3 R43, R45, R47): everything [`Self::note_answered`]
    /// does, and the counts of 429s and authentication failures reset, an
    /// authentication wait ends, and a rate-limit wait runs its course (it is
    /// Tesla's, not ours to cut short).
    fn note_success(&mut self, now: Instant) {
        self.note_answered(now);
        self.account.consecutive_429 = 0;
        self.account.auth_failures = 0;
        if matches!(self.account.wait, Some((_, WaitReason::AuthWait))) {
            self.account.wait = None;
        }
    }

    /// Apply an account-scope failure of this part's classes and return the
    /// account's remaining wait; `None` for a class that is not this part's.
    fn account_failed(
        &mut self,
        class: ErrorClass,
        error: &CollectorError,
        now: Instant,
    ) -> Option<Duration> {
        // A refusal, billing, configuration or lock changes the account's state
        // (R48 to R50, part K6).
        if class.is_account_state_class() {
            return Some(self.account_state_failed(class, error, now));
        }
        if !class.is_account_wait_class() {
            return None;
        }
        Some(match class {
            ErrorClass::RateLimited => {
                self.rate_limited(rate_limit_retry_after_seconds(error).unwrap_or(0), now)
            }
            ErrorClass::Unreachable => self.unreachable(now),
            ErrorClass::CredentialStop => {
                self.enter_credential_stop(now);
                CONTROL_SETTINGS_REFRESH
            }
            _ => {
                let credential_retry = match error {
                    CollectorError::FleetCredential(error) => fleet_refresh_retry_delay(error),
                    error => self.legacy_auth_retry_after(error),
                };
                self.auth_failed(class, credential_retry, http_status_of(error), now)
            }
        })
    }

    /// A car's read stopped by an account-scope failure (C3 R43, R45, R47): it
    /// counts no failure, stays due after the account's wait, and a gated read
    /// keeps its gate, so it is still sent only while the live stream proves the
    /// car awake (R36). Only a car moving by the stream's evidence still becomes
    /// Driving, as for any failed gated read.
    fn release_after_account_failure(&mut self, id: VehicleId, class: ErrorClass, now: Instant) {
        let wall_ms = self.wall_or_last();
        let timing = self.timing;
        let until = self.account.wait.map(|(until, _)| until);
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return;
        };
        vehicle.in_flight = false;
        vehicle.last_error = Some(class);
        let gated = vehicle.gated_read.take();
        if gated == Some(GatedEvidence::Motion) && vehicle.phase == PollPhase::Suspended {
            enter_phase_gated(
                &timing,
                vehicle,
                PollPhase::Driving,
                now,
                wall_ms,
                "use_evidence_stream",
                true,
            );
        } else {
            vehicle.gated_read = gated;
        }
        if vehicle.phase == PollPhase::Suspended && !vehicle.suspension_read {
            return;
        }
        // The retry is the account's wait, not a deadline of the car's own: the
        // read stays due, and is never due before the wait ends.
        let due = vehicle.next_read_due.unwrap_or(now);
        vehicle.next_read_due = Some(until.map_or(due, |until| due.max(until)));
    }

    /// A car's failure that counts (C3 R22, R51): the next read waits the longer
    /// of the phase's interval and the back-off of the car's consecutive
    /// failures, so a failure never makes a car's requests more frequent than its
    /// cadence. `http_status` is for the decision log only.
    fn count_vehicle_failure(
        &mut self,
        id: VehicleId,
        class: ErrorClass,
        http_status: Option<u16>,
        now: Instant,
    ) {
        let draw = self.jitter.draw();
        let timing = self.timing;
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return;
        };
        vehicle.consecutive_failures = vehicle.consecutive_failures.saturating_add(1);
        vehicle.first_failure.get_or_insert(now);
        vehicle.last_error = Some(class);
        let delay = vehicle.interval(&timing, now).max(backoff_delay(
            vehicle.consecutive_failures,
            timing.max_backoff,
            draw,
        ));
        vehicle.next_read_due = if vehicle.phase == PollPhase::Suspended && !vehicle.suspension_read
        {
            None
        } else {
            Some(deadline_after(now, delay))
        };
        log_read_failure(vehicle, http_status, now);
        self.pause_if_persistent(id, now);
    }

    /// A read answered 408 (C3 R46): not retried. Tesla's 408 means the car is
    /// not online, which is a state, not a fault, and a repeat read would cost
    /// money in Fleet polling and wake nothing. The read is next due one
    /// interval of its phase later, and the targeted state check is due now: a
    /// record on the Owner wire, the next list in Fleet polling. Its answer moves
    /// the car (asleep, offline); an `online` answer makes the 408 a transient
    /// failure of the car ([`Self::unavailable_checked`]), because Tesla says a
    /// car with poor connectivity may still answer 408.
    fn read_unavailable(&mut self, id: VehicleId, now: Instant) {
        let timing = self.timing;
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return;
        };
        let first = !vehicle.unavailable_check;
        vehicle.unavailable_check = true;
        vehicle.next_read_due = if vehicle.phase == PollPhase::Suspended && !vehicle.suspension_read
        {
            None
        } else {
            Some(deadline_after(now, vehicle.interval(&timing, now)))
        };
        let hub_id = vehicle
            .hub_id
            .map_or_else(|| "unmapped".to_owned(), |id| id.to_string());
        let phase = vehicle.phase.code();
        if first {
            tracing::info!(
                target: "collector.decision",
                vehicle = %hub_id,
                from = phase,
                to = phase,
                reason = "rest_failure",
                gated = false,
                class = ?ErrorClass::Unavailable,
                http_status = 408u16,
                "collector transition"
            );
        }
        self.schedule_offline_state_fetch(id, now);
    }

    /// The state check that a 408 made due has answered (C3 R46): the car's
    /// pending 408 is settled. Returns whether Tesla still reports the car
    /// `online`, which makes the 408 a transient failure of the car with the
    /// back-off of its failures; the caller counts it after its own borrow ends.
    fn settle_unavailable(vehicle: &mut ScheduledVehicle, state: &str) -> bool {
        std::mem::take(&mut vehicle.unavailable_check) && state == "online"
    }

    /// The retry delay after a failed list (C3 R57). `discovery_failures` is
    /// incremented inside the back-off rows.
    ///
    /// - `RateLimited`, `Unreachable`, `Auth` and `AuthWait` give the account's
    ///   own wait (R45, R43, R47); a terminal credential stop gives
    ///   `CONTROL_SETTINGS_REFRESH`.
    /// - `Transient`, `Protocol` and `ClientError` give `max(state interval,
    ///   backoff_delay(discovery_failures))`.
    /// - The refusal, billing, configuration and lock classes give the account its
    ///   state (R48 to R50): the state's probe interval when it has one (the
    ///   billing suspension's list probe), else `CONTROL_SETTINGS_REFRESH`.
    ///
    /// While the account is in a state that waits for the owner, the value is at
    /// most `CONTROL_SETTINGS_REFRESH`: the loop sleeps on it, and it must keep
    /// reading settings and owner actions (R57).
    fn discovery_failed_for_error(&mut self, error: &CollectorError, now: Instant) -> Duration {
        #[cfg(test)]
        supervised_test_log(LoopEvent::DiscoveryFailed);
        if let Some(outcome) = self.credential_outcome_for(error) {
            let delay = self.credential_outcome(None, outcome, now);
            return if self.account.state == AccountState::Collecting {
                delay
            } else {
                delay.min(CONTROL_SETTINGS_REFRESH)
            };
        }
        let class = classify(error, RequestKind::List, self.mode);
        if class != ErrorClass::Unreachable {
            self.note_answered(now);
        }
        let delay = match self.account_failed(class, error, now) {
            Some(delay) => delay,
            None => self.discovery_failed(now),
        };
        if self.account.state == AccountState::Collecting {
            delay
        } else {
            delay.min(CONTROL_SETTINGS_REFRESH)
        }
    }

    /// A targeted record check failed (C3 R3a): the account classes set the
    /// account's wait; a car's own failure retries the check one state-check
    /// interval later with its phase unchanged, and counts towards a pause (R51)
    /// when it is of a class that counts (`Transient`, `Protocol`, `ClientError`).
    /// A 404 asks for the list (R53); a refusal, mobile-access or legal answer
    /// pauses the car at once (R48 d, R51); an Owner-wire 403 of either kind makes
    /// the next list due now (R48 d).
    fn offline_state_failed_for_error(
        &mut self,
        id: VehicleId,
        error: &CollectorError,
        now: Instant,
    ) {
        let class = classify(error, RequestKind::Record, self.mode);
        if class != ErrorClass::Unreachable && class != ErrorClass::GateNotConfirmed {
            self.note_answered(now);
        }
        if self.account_failed(class, error, now).is_some() {
            // The check stays due; the account's wait holds it back.
            return;
        }
        if self.mode.is_owner_wire() && is_owner_wire_forbidden(error) {
            self.request_refusal_check(now);
        }
        let status = http_status_of(error);
        let timing = self.timing;
        let reschedule = |vehicle: &mut ScheduledVehicle| {
            vehicle.next_check_due =
                Some(deadline_after(now, timing.state_interval(vehicle.phase)));
        };
        match class {
            ErrorClass::NotFound => self.vehicle_not_found(id, now),
            ErrorClass::MobileAccessDisabled => {
                self.pause_vehicle(id, PauseReason::MobileAccessDisabled, class, status, now);
            }
            ErrorClass::LegalPrivacy => {
                self.pause_vehicle(id, PauseReason::LegalPrivacy, class, status, now);
            }
            ErrorClass::AccessRefused => {
                self.pause_vehicle(id, PauseReason::AccessRefused, class, status, now);
            }
            ErrorClass::Transient | ErrorClass::Protocol | ErrorClass::ClientError => {
                if let Some(vehicle) = self.vehicles.get_mut(&id) {
                    vehicle.consecutive_failures = vehicle.consecutive_failures.saturating_add(1);
                    vehicle.first_failure.get_or_insert(now);
                    vehicle.last_error = Some(class);
                    reschedule(vehicle);
                }
                self.pause_if_persistent(id, now);
            }
            _ => {
                if let Some(vehicle) = self.vehicles.get_mut(&id) {
                    vehicle.last_error = Some(class);
                    reschedule(vehicle);
                }
            }
        }
        // A pause or a removal ends the check's schedule.
        if let Some(vehicle) = self.vehicles.get_mut(&id)
            && vehicle.hold != VehicleHold::None
        {
            vehicle.next_check_due = None;
        }
    }

    /// A read failed (C3 R22, R42 to R47, R56): classified once, then
    ///
    /// - a gated read the stream gate refused sent no request and is no failure
    ///   (R36);
    /// - an account-scope class (rate limit, unreachable, authentication, and the
    ///   refusals and states of R48 to R50) holds the whole account back and counts
    ///   no car's failure (R43, R45, R47);
    /// - a 408 is not retried and asks for the state check (R46); while Updating
    ///   it changes nothing and is not a failure, because the car disconnects
    ///   during an install (R20) and R41 ends an install that stays silent;
    /// - the classes that hold a car act on it: `NotFound` asks for the list (R53),
    ///   `InService` enters service mode (R52), a refusal, mobile-access or legal
    ///   answer pauses it at once (R48 d, R51);
    /// - every other class is the car's own failure: the next read waits
    ///   `max(interval of the phase, back-off(consecutive_failures))`, so a failure
    ///   never makes a car's requests more frequent than its cadence, and a long run
    ///   of them pauses the car (R51).
    ///
    /// An Owner-wire 403 of either kind makes the next list due now (R48 d). No
    /// class resends within the tick (R56): the car's read is next due after
    /// an interval or a wait, never at once. A gated read that was sent and
    /// failed still moves a car that the stream showed in motion to Driving
    /// (R36), and a Suspended car gets no timed read other than the one at the
    /// end of its suspension (R32, R33).
    fn vehicle_failed_for_error(
        &mut self,
        id: VehicleId,
        error: &CollectorError,
        now: Instant,
    ) -> FailureEffect {
        // The credential unit's own outcomes enter by the seam of their rows (C5
        // R56, C3 R42, R47).
        if let Some(outcome) = self.credential_outcome_for(error) {
            self.credential_outcome(Some(id), outcome, now);
            return FailureEffect::Counted;
        }
        let class = classify(error, RequestKind::Read, self.mode);
        if class == ErrorClass::GateNotConfirmed {
            self.gate_not_confirmed(id);
            return FailureEffect::Counted;
        }
        if class != ErrorClass::Unreachable {
            self.note_answered(now);
        }
        if self.account_failed(class, error, now).is_some() {
            self.release_after_account_failure(id, class, now);
            return FailureEffect::Counted;
        }
        if self.mode.is_owner_wire() && is_owner_wire_forbidden(error) {
            self.request_refusal_check(now);
        }
        self.vehicle_class_failure(id, class, http_status_of(error), now)
    }

    /// The part of a read's failure that is the car's own (C3 R22, R46, R51 to R53):
    /// see [`Self::vehicle_failed_for_error`]. `http_status` is for the decision log
    /// only.
    fn vehicle_class_failure(
        &mut self,
        id: VehicleId,
        class: ErrorClass,
        http_status: Option<u16>,
        now: Instant,
    ) -> FailureEffect {
        let wall_ms = self.wall_or_last();
        let timing = self.timing;
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return FailureEffect::Counted;
        };
        let gated = vehicle.gated_read.take();
        vehicle.in_flight = false;
        if vehicle.phase == PollPhase::Updating && class == ErrorClass::Unavailable {
            let interval = vehicle.interval(&timing, now);
            vehicle.next_read_due = Some(deadline_after(now, interval));
            return FailureEffect::Counted;
        }
        if gated == Some(GatedEvidence::Motion) && vehicle.phase == PollPhase::Suspended {
            enter_phase_gated(
                &timing,
                vehicle,
                PollPhase::Driving,
                now,
                wall_ms,
                "use_evidence_stream",
                true,
            );
        }
        match class {
            ErrorClass::Unavailable => self.read_unavailable(id, now),
            ErrorClass::NotFound => self.vehicle_not_found(id, now),
            ErrorClass::InService => {
                return if self.enter_service_mode(id, now) {
                    FailureEffect::ServiceEntered
                } else {
                    FailureEffect::Counted
                };
            }
            ErrorClass::MobileAccessDisabled => {
                self.pause_vehicle(
                    id,
                    PauseReason::MobileAccessDisabled,
                    class,
                    http_status,
                    now,
                );
            }
            ErrorClass::LegalPrivacy => {
                self.pause_vehicle(id, PauseReason::LegalPrivacy, class, http_status, now);
            }
            ErrorClass::AccessRefused => {
                self.pause_vehicle(id, PauseReason::AccessRefused, class, http_status, now);
            }
            _ => self.count_vehicle_failure(id, class, http_status, now),
        }
        FailureEffect::Counted
    }
}
