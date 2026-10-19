// SPDX-License-Identifier: AGPL-3.0-only

// Refusals, holds and owner actions (C3 part K6): the account states of R48 to
// R50 (Tesla or a provider refusing the account, billing, configuration and
// locks), the vehicle holds of R51 to R53 (pause, service mode, a car removed from
// the account), the probes that end a hold, and the owner's two actions of R65.
//
// It is the third half of `impl VehicleScheduler`, in its own file so neither
// `scheduler.rs` nor `scheduler_errors.rs` grows past the layout gate. The
// classification of an error into a class is `scheduler_errors.rs` (R42); here the
// classes that change a state or a hold are acted on. The scheduler still never
// sends a request: each rule decides only whether a car is read and when, and the
// loops (`owner_supervision.rs`, `fleet_supervision.rs`) send what the scheduler
// says is due.

// ---------------------------------------------------------------------------
// Constants. Every value carries its reason and the requirement that gives it.
// ---------------------------------------------------------------------------

/// Failures in a row of one car's own requests that make a persistent fault (C3
/// R51, `K_PAUSE_FAILURES`): a dozen independent attempts. Together with
/// [`K_PAUSE_MIN_AGE`] it means a fault that outlasted a dozen tries and an hour,
/// not a short outage.
const K_PAUSE_FAILURES: u32 = 12;

/// How long a car's failures must have lasted before it is paused (C3 R51,
/// `K_PAUSE_MIN_AGE`). With `max(interval, back-off)` a dozen failures take about
/// 20 to 40 minutes in the Online phase and less while Driving, so the count alone
/// would not tell a lasting fault from a short outage; Tesla's 540 text ("might
/// need a reboot, OTA update, or service") describes faults on the scale of hours.
const K_PAUSE_MIN_AGE: Duration = Duration::from_secs(60 * 60);

/// Consecutive lists that must lack a configured car before it is Removed (C3
/// R53, `K_REMOVE_LISTS`): one absent entry may be a partial or paginated answer
/// (Tesla pages the vehicle list with a default page size of 100), two
/// absences confirm. History is never deleted by the collector.
const K_REMOVE_LISTS: u8 = 2;

/// The status note of a car that left the Tesla account (C3 R53).
const STATUS_NOT_IN_ACCOUNT: &str = "not in the Tesla account";

// ---------------------------------------------------------------------------
// Vocabulary.
// ---------------------------------------------------------------------------

/// What made the account-wide read refuse (C3 R48, R49): it picks the account
/// state by mode and the reason code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefusalCause {
    /// A 403 that refuses the account-wide list: Fleet-named, a plain one that
    /// repeated after the credential unit's refresh, or every car refused (R48 a,
    /// c, e).
    Forbidden,
    /// A 410 on any scheduled request (R48 b, R49).
    Gone,
    /// A provider's 402 (R49).
    PaymentRequired,
    /// The credential unit's token-endpoint refusal of kind `OwnerApi` (C5 X1).
    CredentialUnitOwnerApi,
    /// The credential unit's refusal of kind `Client` (C5 X1).
    CredentialUnitClient,
}

/// The kinds of refusal the credential unit (C5) reports (C5 X1, C3 R42, R47).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CredentialRefusal {
    OwnerApi,
    Client,
}

/// The outcomes of the credential unit (C5) the scheduler consumes (C3 R42, R47):
/// `credential_outcome_for` reads them from a collector error and they enter
/// through this one seam, which the tests use by these names too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CredentialOutcome {
    /// `AuthUnavailable { retry_at }` (C5 R56), as the time left to wait: the
    /// whole account waits for it (R47).
    AuthUnavailable { retry_after: Duration },
    /// `OwnerApiRefused { reason }` (C5 R45 rows 7 and 10): the account state of R48
    /// or R49 by mode.
    OwnerApiRefused(CredentialRefusal),
    /// `VehicleAccessRejected` (C5 R41a): one car's own failure, never an account
    /// stop.
    VehicleAccessRejected,
    /// Another terminal outcome (C5 R56: `signed_out`, `configuration_error`).
    TerminalStop,
}

/// What the scheduler reads of the credential unit's state at the top of an
/// iteration (C3 R47): a stop reached outside a scheduled request is seen at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CredentialStateSeen {
    Ready,
    TerminalStop,
    ApiRefused(CredentialRefusal),
}

/// What a car's failed read did to the car, for the caller that stores the
/// result (C3 R52): entering service mode is the one effect with a stored record,
/// the Hub event `service_refused`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureEffect {
    Counted,
    /// The car entered service mode with this failure: the caller appends the
    /// `service_refused` Hub event in the tick transaction (C1 R15).
    ServiceEntered,
}

/// Whether an error is an Owner-wire 403 of either kind (C3 R48): a status the
/// car's own requests turn into a list check.
fn is_owner_wire_forbidden(error: &CollectorError) -> bool {
    matches!(
        owner_api_error(error),
        Some(OwnerApiError::Forbidden { .. } | OwnerApiError::HttpStatus(403))
    )
}

/// Whether an error is a 403 whose body does not name the Fleet API (C3 R42,
/// R48 c): the one answer on the list that asks the credential unit for a
/// refresh before it counts as a refusal.
fn is_plain_owner_forbidden(error: &CollectorError) -> bool {
    matches!(
        owner_api_error(error),
        Some(
            OwnerApiError::Forbidden {
                fleet_api_named: false
            } | OwnerApiError::HttpStatus(403)
        )
    )
}

/// The reason code stored with a pause (C3 section 7.4).
const fn pause_reason_code(reason: PauseReason) -> &'static str {
    match reason {
        PauseReason::PersistentFailure => "paused_persistent_failure",
        PauseReason::MobileAccessDisabled => "paused_mobile_access_disabled",
        PauseReason::LegalPrivacy => "paused_legal_privacy",
        PauseReason::AccessRefused => "paused_access_refused",
    }
}

impl AccountState {
    /// The status text of the state in plain words (C3 R13, R48): it says what
    /// happened and what the owner can do, and for Tesla's or a provider's refusal
    /// it never says the credentials failed, because the credentials did not.
    const fn plain_words(self) -> &'static str {
        match self {
            Self::Collecting => "Collecting.",
            Self::OwnerApiRefused => {
                "Tesla has refused the Owner API. Collection is stopped; use a data provider or consented Fleet API polling."
            }
            Self::ProviderRefused => {
                "The data provider has refused the Hub (a subscription or authorisation matter at the provider). Collection is stopped until it is resolved there and collection is retried."
            }
            Self::FleetAccessRefused => {
                "Tesla's Fleet API refused the account's vehicle list (missing scope or application access). Collection is stopped until access is granted again and collection is retried."
            }
            Self::BillingSuspended => {
                "Tesla has suspended Fleet API use because the account's billing limit was reached. Raise the limit at Tesla or wait for the next billing month."
            }
            Self::CapReached => {
                "Polling stopped: local monthly cap reached. Collection resumes next month, after the cap is raised, or when collection is retried."
            }
            Self::ConfigError => {
                "Tesla refused the Fleet API application's configuration (partner registration or region). Collection is stopped until it is fixed and collection is retried."
            }
            Self::AccountLocked => {
                "Tesla has locked the account. It must be unlocked by Tesla; then retry collection."
            }
            Self::CredentialStop => {
                "Collection is stopped until the Tesla account is signed in again."
            }
        }
    }
}

impl ErrorClass {
    /// The classes that change the account's state rather than make it wait (C3
    /// R48 to R50): a refusal of the account, billing, configuration and locks.
    const fn is_account_state_class(self) -> bool {
        matches!(
            self,
            Self::OwnerApiRefusal
                | Self::ProviderRefusal
                | Self::FleetAccess
                | Self::Billing
                | Self::Config
                | Self::Locked
        )
    }
}

impl ScheduledVehicle {
    /// Tesla last reported the car `online` (C3 R51, R52: a probe read is sent
    /// only then).
    fn reported_online(&self) -> bool {
        self.reported_state.as_deref() == Some("online")
    }

    /// When the held car's probe may be sent, if it may (C3 R51, R52). An
    /// Owner-wire service probe is a record request, which wakes nothing and is
    /// sent when due. Every other probe is a read, sent only while the latest list
    /// or record says `online` (a read of a sleeping car would be a wake in
    /// effect, R38). A pause by `AccessRefused` has no probe: its remedy is the
    /// owner's.
    fn probe_due_at(&self, owner_wire: bool) -> Option<Instant> {
        let due = self.next_probe_due?;
        match self.hold {
            VehicleHold::Service if owner_wire => Some(due),
            VehicleHold::Service => self.reported_online().then_some(due),
            VehicleHold::Paused(PauseReason::AccessRefused) => None,
            VehicleHold::Paused(_) => self.reported_online().then_some(due),
            VehicleHold::None | VehicleHold::Disabled | VehicleHold::Removed => None,
        }
    }

    /// The reason code the car shows and stores (C3 R4, R13): a held car's is its
    /// hold's own (a pause by `AccessRefused` must come back as that pause after a
    /// restart, whatever phase report or stream event came since); otherwise an audit
    /// failure of the car's stream shows until a run emits `Healthy` (R28, C4 R56 e),
    /// whatever transition came since; otherwise the last transition's.
    fn shown_reason(&self) -> &'static str {
        match self.hold {
            VehicleHold::Paused(reason) => pause_reason_code(reason),
            VehicleHold::Service => "service_entered",
            VehicleHold::Removed => "vehicle_removed",
            VehicleHold::Disabled => "vehicle_disabled",
            VehicleHold::None if self.stream.audit_failed => "stream_audit_failed",
            VehicleHold::None => self.reason,
        }
    }

    /// The note the status shows beside the car (C3 R53, R54): a car that left
    /// the account says so; otherwise the stream note of R28 and R54.
    fn status_detail(&self, now: Instant) -> Option<&'static str> {
        if self.hold == VehicleHold::Removed {
            Some(STATUS_NOT_IN_ACCOUNT)
        } else {
            self.stream_detail(now)
        }
    }

    /// Drop every timer that would send a request for a car that is now held, and
    /// the decisions that qualified one (C3 R51, R52).
    fn stop_timers(&mut self) {
        self.next_read_due = None;
        self.next_check_due = None;
        self.suspend_at = None;
        self.suspended_until = None;
        self.gated_read = None;
        self.wake_read_pending = false;
        self.suspension_read = false;
        self.unavailable_check = false;
    }
}

// ---------------------------------------------------------------------------
// The scheduler.
// ---------------------------------------------------------------------------

impl VehicleScheduler {
    // ---- Account states (C3 R48 to R50) ---------------------------------------

    /// The account state and reason code a refusal gives in the current mode (C3
    /// R48, R49): Tesla withdrawing the Owner API in Legacy, a provider's refusal in
    /// Provider mode, which is a matter at the provider and gets its own state.
    fn refusal_for(&self, cause: RefusalCause) -> (AccountState, &'static str) {
        if self.mode == crate::config::CollectorMode::Provider {
            let reason = match cause {
                RefusalCause::Forbidden | RefusalCause::CredentialUnitOwnerApi => {
                    "provider_forbidden"
                }
                RefusalCause::Gone => "provider_gone",
                RefusalCause::PaymentRequired => "provider_payment_required",
                RefusalCause::CredentialUnitClient => "provider_client_refused",
            };
            (AccountState::ProviderRefused, reason)
        } else {
            let reason = match cause {
                RefusalCause::Forbidden
                | RefusalCause::CredentialUnitOwnerApi
                | RefusalCause::PaymentRequired => "owner_api_refused",
                RefusalCause::Gone => "owner_api_gone",
                RefusalCause::CredentialUnitClient => "owner_api_client_refused",
            };
            (AccountState::OwnerApiRefused, reason)
        }
    }

    /// Move the account into a state that waits for the owner or for Tesla (C3
    /// R48 to R50). One event, one row write and one gap per entry (R10, R8):
    /// repeating the state emits nothing, except that a billing suspension
    /// re-arms its probe, which every further 402 answers. Only a collecting
    /// account enters a state: a request still in flight when the first refusal
    /// arrived reports back into a state that already sends nothing, and the first
    /// state wins.
    fn enter_account_state(
        &mut self,
        state: AccountState,
        reason: &'static str,
        class: ErrorClass,
        http_status: Option<u16>,
        now: Instant,
    ) {
        if self.account.state == state && self.account.state_reason == reason {
            if state == AccountState::BillingSuspended {
                self.account.next_billing_probe = Some(deadline_after(now, K_BILLING_PROBE));
            }
            return;
        }
        if self.account.state != AccountState::Collecting {
            return;
        }
        let wall_ms = self.wall_or_last();
        self.account.state = state;
        self.account.state_reason = reason;
        self.account.state_since_ms = wall_ms;
        self.account.next_billing_probe =
            (state == AccountState::BillingSuspended).then(|| deadline_after(now, K_BILLING_PROBE));
        self.account.list_wanted = None;
        self.account.list_floor = None;
        let next_in_ms = self.account.next_billing_probe.map(|probe| {
            u64::try_from(probe.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX)
        });
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = AccountState::Collecting.code(),
            to = state.code(),
            reason,
            next_in_ms,
            gated = false,
            class = ?class,
            http_status,
            "collector transition"
        );
    }

    /// The account's own failure of a class that changes its state (C3 R48 to
    /// R50), and the delay the failed list should wait (R57): the state's probe
    /// interval when it has one, else the settings refresh.
    ///
    /// - A 410 is the Owner API gone (RFC 9110: no longer available, likely
    ///   permanent) and a provider's 402 is its payment required (R48 b, R49).
    /// - A Fleet 402 is the billing suspension, whose probe is a list (R50); 403 on
    ///   the list is the Fleet access refusal; 412 and 421 are configuration (a
    ///   partner account not registered, the wrong region); 423 is a locked
    ///   account.
    fn account_state_failed(
        &mut self,
        class: ErrorClass,
        error: &CollectorError,
        now: Instant,
    ) -> Duration {
        let status = http_status_of(error);
        match class {
            ErrorClass::OwnerApiRefusal | ErrorClass::ProviderRefusal => {
                let cause = match (legacy_auth_error_of(error), status) {
                    // The credential unit's own refusal carries its reason (C5 X1).
                    (Some(LegacyAuthError::OwnerApiRefused { reason }), _) => match reason {
                        RefusalReason::OwnerApi => RefusalCause::CredentialUnitOwnerApi,
                        RefusalReason::Client => RefusalCause::CredentialUnitClient,
                    },
                    (_, Some(402)) => RefusalCause::PaymentRequired,
                    (_, Some(410)) => RefusalCause::Gone,
                    _ => RefusalCause::Forbidden,
                };
                let (state, reason) = self.refusal_for(cause);
                self.enter_account_state(state, reason, class, status, now);
                CONTROL_SETTINGS_REFRESH
            }
            ErrorClass::FleetAccess => {
                self.enter_account_state(
                    AccountState::FleetAccessRefused,
                    "fleet_access_refused",
                    class,
                    status,
                    now,
                );
                CONTROL_SETTINGS_REFRESH
            }
            ErrorClass::Billing => {
                self.enter_account_state(
                    AccountState::BillingSuspended,
                    "billing_suspended",
                    class,
                    status,
                    now,
                );
                K_BILLING_PROBE
            }
            ErrorClass::Config => {
                let reason = if status == Some(421) {
                    "config_wrong_region"
                } else {
                    "config_partner_unregistered"
                };
                self.enter_account_state(AccountState::ConfigError, reason, class, status, now);
                CONTROL_SETTINGS_REFRESH
            }
            ErrorClass::Locked => {
                self.enter_account_state(
                    AccountState::AccountLocked,
                    "account_locked",
                    class,
                    status,
                    now,
                );
                CONTROL_SETTINGS_REFRESH
            }
            // Not a class of this part: the caller never asks.
            _ => CONTROL_SETTINGS_REFRESH,
        }
    }

    /// The list answered 403 again after the credential unit's refresh and the one
    /// resend (C3 R48 c): the account is refused, with reason `owner_api_refused`
    /// (Legacy) or `provider_forbidden` (Provider). Returns the delay the failed
    /// list waits, which is the settings refresh (R57). A 403 that is Fleet-named
    /// or plain counts alike, because the repetition after a successful refresh is
    /// the evidence the plan names.
    fn list_refused_after_check(&mut self, now: Instant) -> Duration {
        self.note_answered(now);
        let (state, reason) = self.refusal_for(RefusalCause::Forbidden);
        let class = if state == AccountState::ProviderRefused {
            ErrorClass::ProviderRefusal
        } else {
            ErrorClass::OwnerApiRefusal
        };
        self.enter_account_state(state, reason, class, Some(403), now);
        CONTROL_SETTINGS_REFRESH
    }

    /// A car's own 403 asks for the account-wide list now (C3 R48 d): the list
    /// decides whether the account or only the car was refused, in one tick however
    /// many cars were refused, and on the Owner wire it is non-waking and unbilled.
    /// It is not a transition (R10), so it logs at DEBUG with its reason code.
    fn request_refusal_check(&mut self, now: Instant) {
        self.account.list_wanted = Some(now);
        tracing::debug!(
            target: "collector.decision",
            vehicle = "account",
            reason = "refusal_check",
            "a car's 403 makes the next list due now"
        );
    }

    /// Every collected car is refused (C3 R48 e): when every configured car that is
    /// neither Disabled nor Removed holds `Paused(AccessRefused)`, no car can be
    /// read on this wire and the account is refused although the list still answers.
    /// Checked at every hold change, on the Owner wire only, where `AccessRefused`
    /// arises from R48 d.
    fn check_every_vehicle_refused(&mut self, now: Instant) {
        if !self.mode.is_owner_wire() || self.account.state != AccountState::Collecting {
            return;
        }
        let mut any = false;
        for vehicle in self
            .vehicles
            .values()
            .filter(|vehicle| !vehicle.is_inactive())
        {
            any = true;
            if vehicle.hold != VehicleHold::Paused(PauseReason::AccessRefused) {
                return;
            }
        }
        if !any {
            return;
        }
        let (state, reason) = self.refusal_for(RefusalCause::Forbidden);
        let class = if state == AccountState::ProviderRefused {
            ErrorClass::ProviderRefusal
        } else {
            ErrorClass::OwnerApiRefusal
        };
        self.enter_account_state(state, reason, class, Some(403), now);
    }

    /// Any successful answer to the billing probe resumes collection (C3 R50): the
    /// account collects again, `telemetry_check_due` is set and the decision log
    /// carries `billing_resumed`, which the Fleet Telemetry maintenance path of
    /// G2-6 uses to check the cars' configurations (section 8 item 14). Only the
    /// probe's list resumes: a read still in flight when the 402 arrived is not
    /// evidence that Tesla has lifted the suspension.
    fn resume_from_billing(&mut self) {
        if self.account.state != AccountState::BillingSuspended {
            return;
        }
        let wall_ms = self.wall_or_last();
        self.account.state = AccountState::Collecting;
        self.account.state_reason = "billing_resumed";
        self.account.state_since_ms = wall_ms;
        self.account.next_billing_probe = None;
        // The tick that resumed has no probe time to keep (B2 apply, hub fix pass 2,
        // F2-2): if its commit fails, the baseline's due probe asks again once the
        // store's wait is over, as before R3C-03 (a re-arm of the answered probe would
        // hold collection suspended for a whole period after one transient store fault).
        self.account.probe_time_of_the_tick = None;
        self.account.telemetry_check_due = true;
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = AccountState::BillingSuspended.code(),
            to = AccountState::Collecting.code(),
            reason = "billing_resumed",
            gated = false,
            "collector transition"
        );
    }

    /// A new billing month began (C3 R50): a billing suspension is probed at the
    /// first tick of the month, because Tesla's cycle may have lifted it. The month
    /// tracker of the request counter (R25) calls this once per month.
    fn note_new_month(&mut self, now: Instant) {
        if self.account.state == AccountState::BillingSuspended {
            self.account.next_billing_probe = Some(now);
            tracing::debug!(
                target: "collector.decision",
                vehicle = "account",
                reason = "billing_probe",
                "a new month makes the billing probe due now"
            );
        }
    }

    // ---- The credential unit's outcomes (C3 R42, R47, R48) ---------------------

    /// One outcome of the credential unit for a scheduled request (C3 R42, R47):
    /// the rows of R42's table for C5's types, which have none in the tree yet.
    /// `id` is the car whose request it was, when it was one. Returns the delay a
    /// failed list should wait.
    fn credential_outcome(
        &mut self,
        id: Option<VehicleId>,
        outcome: CredentialOutcome,
        now: Instant,
    ) -> Duration {
        match outcome {
            CredentialOutcome::AuthUnavailable { retry_after } => {
                let delay = self.auth_failed(ErrorClass::AuthWait, Some(retry_after), None, now);
                if let Some(id) = id {
                    self.release_after_account_failure(id, ErrorClass::AuthWait, now);
                }
                delay
            }
            CredentialOutcome::OwnerApiRefused(kind) => {
                self.note_answered(now);
                let cause = match kind {
                    CredentialRefusal::OwnerApi => RefusalCause::CredentialUnitOwnerApi,
                    CredentialRefusal::Client => RefusalCause::CredentialUnitClient,
                };
                let (state, reason) = self.refusal_for(cause);
                let class = if state == AccountState::ProviderRefused {
                    ErrorClass::ProviderRefusal
                } else {
                    ErrorClass::OwnerApiRefusal
                };
                self.enter_account_state(state, reason, class, None, now);
                if let Some(id) = id {
                    self.release_after_account_failure(id, class, now);
                }
                CONTROL_SETTINGS_REFRESH
            }
            CredentialOutcome::VehicleAccessRejected => {
                self.note_answered(now);
                if let Some(id) = id {
                    self.vehicle_class_failure(id, ErrorClass::ClientError, Some(401), now);
                }
                Duration::ZERO
            }
            CredentialOutcome::TerminalStop => {
                self.enter_credential_stop(now);
                if let Some(id) = id {
                    self.release_after_account_failure(id, ErrorClass::CredentialStop, now);
                }
                CONTROL_SETTINGS_REFRESH
            }
        }
    }

    /// The credential unit's state as read at a settings refresh (C3 R47, R48): a
    /// stop or refusal it reached outside a scheduled request, such as in a
    /// proactive refresh, is seen within one refresh; a lifted stop collects again.
    fn credential_state_seen(&mut self, seen: CredentialStateSeen, now: Instant) {
        match seen {
            CredentialStateSeen::Ready => self.clear_credential_stop(now),
            CredentialStateSeen::TerminalStop => self.enter_credential_stop(now),
            CredentialStateSeen::ApiRefused(kind) => {
                let cause = match kind {
                    CredentialRefusal::OwnerApi => RefusalCause::CredentialUnitOwnerApi,
                    CredentialRefusal::Client => RefusalCause::CredentialUnitClient,
                };
                let (state, reason) = self.refusal_for(cause);
                let class = if state == AccountState::ProviderRefused {
                    ErrorClass::ProviderRefusal
                } else {
                    ErrorClass::OwnerApiRefusal
                };
                self.enter_account_state(state, reason, class, None, now);
            }
        }
    }

    // ---- Vehicle holds (C3 R51 to R53) -------------------------------------------

    /// Hold a car `Paused` (C3 R51): no scheduled reads, its timers dropped, its
    /// stream kept (K4), one `vehicle_paused` gap (the gap derivation of K3), and a
    /// probe an hour away except for `AccessRefused`, whose remedy is the owner's
    /// (adding the application's virtual key, sharing the car or a mode change) and
    /// whose probe in Fleet polling would be a billed refusal. On the Owner wire an
    /// `AccessRefused` pause may make every car refused (R48 e).
    fn pause_vehicle(
        &mut self,
        id: VehicleId,
        reason: PauseReason,
        class: ErrorClass,
        http_status: Option<u16>,
        now: Instant,
    ) {
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return;
        };
        vehicle.in_flight = false;
        vehicle.last_error = Some(class);
        if vehicle.is_inactive() {
            return;
        }
        let already = vehicle.hold == VehicleHold::Paused(reason);
        let from = if vehicle.hold == VehicleHold::None {
            vehicle.phase.code()
        } else {
            vehicle.hold.code()
        };
        vehicle.stop_timers();
        vehicle.hold = VehicleHold::Paused(reason);
        vehicle.reason = pause_reason_code(reason);
        vehicle.next_probe_due = match reason {
            PauseReason::AccessRefused => None,
            _ => Some(deadline_after(now, K_PAUSE_PROBE)),
        };
        if !already {
            let hub_id = vehicle
                .hub_id
                .map_or_else(|| "unmapped".to_owned(), |hub| hub.to_string());
            tracing::info!(
                target: "collector.decision",
                vehicle = %hub_id,
                from,
                to = vehicle.hold.code(),
                reason = vehicle.reason,
                next_in_ms = vehicle.next_probe_due.map(|probe| {
                    u64::try_from(probe.saturating_duration_since(now).as_millis())
                        .unwrap_or(u64::MAX)
                }),
                gated = false,
                class = ?class,
                http_status,
                "collector transition"
            );
        }
        if reason == PauseReason::AccessRefused {
            self.check_every_vehicle_refused(now);
        }
    }

    /// Pause a car whose own failures have lasted (C3 R51): at least
    /// `K_PAUSE_FAILURES` in a row, the first at least `K_PAUSE_MIN_AGE` ago. Both
    /// thresholds together mean a fault that outlasted a dozen independent attempts
    /// and an hour, not a short outage.
    fn pause_if_persistent(&mut self, id: VehicleId, now: Instant) {
        let Some(vehicle) = self.vehicles.get(&id) else {
            return;
        };
        if vehicle.hold != VehicleHold::None || vehicle.consecutive_failures < K_PAUSE_FAILURES {
            return;
        }
        let lasted = vehicle
            .first_failure
            .is_some_and(|first| now.saturating_duration_since(first) >= K_PAUSE_MIN_AGE);
        if !lasted {
            return;
        }
        let class = vehicle.last_error.unwrap_or(ErrorClass::Transient);
        self.pause_vehicle(id, PauseReason::PersistentFailure, class, None, now);
    }

    /// A car leaves `Paused` or `Service` (C3 R51, R52, R65): hold `None`, phase
    /// `Unknown` (R34 a: the next list that says `online` makes its first-contact
    /// read), its failure run forgotten and its probe dropped. `reason` is the
    /// code of the way out. Returns whether the car was held.
    fn release_hold(&mut self, id: VehicleId, reason: &'static str, now: Instant) -> bool {
        let wall_ms = self.wall_or_last();
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return false;
        };
        if !matches!(vehicle.hold, VehicleHold::Service | VehicleHold::Paused(_)) {
            return false;
        }
        let from = vehicle.hold.code();
        vehicle.hold = VehicleHold::None;
        vehicle.next_probe_due = None;
        vehicle.in_flight = false;
        vehicle.consecutive_failures = 0;
        vehicle.first_failure = None;
        vehicle.last_error = None;
        vehicle.offline_from_session = false;
        vehicle.stop_timers();
        vehicle.phase = PollPhase::Unknown;
        vehicle.phase_since = (now, wall_ms);
        vehicle.reason = reason;
        let hub_id = vehicle
            .hub_id
            .map_or_else(|| "unmapped".to_owned(), |hub| hub.to_string());
        tracing::info!(
            target: "collector.decision",
            vehicle = %hub_id,
            from,
            to = PollPhase::Unknown.code(),
            reason,
            gated = false,
            "collector transition"
        );
        true
    }

    /// A read, record or probe answered `NotFound` (C3 R53): the car may have left
    /// the account, so the list is due now and it counts an absence at once. The
    /// answer is not the car's failure; its next read stays one interval away, so a
    /// car that really is gone costs no more than the two lists that confirm it.
    fn vehicle_not_found(&mut self, id: VehicleId, now: Instant) {
        let timing = self.timing;
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return;
        };
        vehicle.in_flight = false;
        vehicle.last_error = Some(ErrorClass::NotFound);
        vehicle.not_found_check = true;
        if vehicle.hold == VehicleHold::None
            && !(vehicle.phase == PollPhase::Suspended && !vehicle.suspension_read)
            && vehicle.next_read_due.is_some()
        {
            vehicle.next_read_due = Some(deadline_after(now, vehicle.interval(&timing, now)));
        }
        if vehicle.next_check_due.is_some() {
            vehicle.next_check_due =
                Some(deadline_after(now, timing.state_interval(vehicle.phase)));
        }
        let hub_id = self.hub_label(id);
        self.account.list_wanted = Some(now);
        tracing::debug!(
            target: "collector.decision",
            vehicle = %hub_id,
            reason = "vehicle_missing_from_list",
            class = ?ErrorClass::NotFound,
            "a read found no such car; the list is due now"
        );
    }

    /// The list named the car (C3 R53): its absences reset, a pending `NotFound`
    /// check is answered, and a car that was Removed is back (`vehicle_reappeared`,
    /// phase `Unknown`, so R34 a gives its first-contact read).
    fn note_listed(vehicle: &mut ScheduledVehicle, now: Instant, wall_ms: i64) {
        vehicle.missing_lists = 0;
        vehicle.missing_last = None;
        vehicle.not_found_check = false;
        if vehicle.hold != VehicleHold::Removed {
            return;
        }
        vehicle.hold = VehicleHold::None;
        vehicle.phase = PollPhase::Unknown;
        vehicle.phase_since = (now, wall_ms);
        vehicle.reason = "vehicle_reappeared";
        log_vehicle_transition(
            vehicle,
            VehicleHold::Removed.code(),
            PollPhase::Unknown.code(),
            "vehicle_reappeared",
            now,
        );
    }

    /// The cars a successful list did not name (C3 R53): each counts one absence,
    /// but two count only when they are an inventory interval apart, or when the
    /// second was asked for by a `NotFound` read, so a burst of lists cannot confirm
    /// a removal that one partial answer caused. At `K_REMOVE_LISTS` absences the
    /// car is `Removed`: no requests, its stream stopped, its history kept, the
    /// status saying it is not in the Tesla account.
    fn note_unlisted(&mut self, present: &BTreeSet<VehicleId>, now: Instant) {
        let inventory = self.timing.inventory;
        let ids: Vec<VehicleId> = self
            .vehicles
            .iter()
            .filter(|(id, vehicle)| {
                !present.contains(id)
                    && matches!(
                        vehicle.hold,
                        VehicleHold::None | VehicleHold::Service | VehicleHold::Paused(_)
                    )
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let Some(vehicle) = self.vehicles.get_mut(&id) else {
                continue;
            };
            let spaced = vehicle.not_found_check
                || vehicle
                    .missing_last
                    .is_none_or(|last| now.saturating_duration_since(last) >= inventory);
            vehicle.not_found_check = false;
            if !spaced {
                continue;
            }
            vehicle.missing_lists = vehicle.missing_lists.saturating_add(1);
            vehicle.missing_last = Some(now);
            if vehicle.missing_lists < K_REMOVE_LISTS {
                let absences = vehicle.missing_lists;
                let hub_id = self.hub_label(id);
                tracing::debug!(
                    target: "collector.decision",
                    vehicle = %hub_id,
                    reason = "vehicle_missing_from_list",
                    absences,
                    "the list does not name the car"
                );
                continue;
            }
            let from = if vehicle.hold == VehicleHold::None {
                vehicle.phase.code()
            } else {
                vehicle.hold.code()
            };
            vehicle.hold = VehicleHold::Removed;
            vehicle.reason = "vehicle_removed";
            vehicle.next_probe_due = None;
            vehicle.stop_timers();
            log_vehicle_transition(
                &self.vehicles[&id],
                from,
                VehicleHold::Removed.code(),
                "vehicle_removed",
                now,
            );
        }
    }

    // ---- Service mode (C3 R52) --------------------------------------------------

    /// A read was refused as in service (C3 R52): hold `Service`, reason
    /// `service_entered`, the next probe `K_SERVICE_PROBE` away, every read timer
    /// dropped. Returns whether the car newly entered the hold: the caller then
    /// appends the `service_refused` Hub event in the tick transaction (C1 R15), once
    /// per entry. The car's stream is kept while it is online and its frames stored
    /// (R28, R36); a service visit lasts hours to days, so a quarter of an hour
    /// bounds the delay to noticing its end.
    fn enter_service_mode(&mut self, id: VehicleId, now: Instant) -> bool {
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return false;
        };
        vehicle.in_flight = false;
        if vehicle.is_inactive() || vehicle.hold == VehicleHold::Service {
            return false;
        }
        let from = if vehicle.hold == VehicleHold::None {
            vehicle.phase.code()
        } else {
            vehicle.hold.code()
        };
        vehicle.stop_timers();
        vehicle.hold = VehicleHold::Service;
        vehicle.reason = "service_entered";
        vehicle.next_probe_due = Some(deadline_after(now, K_SERVICE_PROBE));
        vehicle.consecutive_failures = 0;
        vehicle.first_failure = None;
        vehicle.last_error = Some(ErrorClass::InService);
        let hub_id = vehicle
            .hub_id
            .map_or_else(|| "unmapped".to_owned(), |hub| hub.to_string());
        tracing::info!(
            target: "collector.decision",
            vehicle = %hub_id,
            from,
            to = VehicleHold::Service.code(),
            reason = "service_entered",
            next_in_ms = u64::try_from(K_SERVICE_PROBE.as_millis()).unwrap_or(u64::MAX),
            gated = false,
            class = ?ErrorClass::InService,
            http_status = 405u16,
            "collector transition"
        );
        true
    }

    // ---- Probes (C3 R51, R52) -------------------------------------------------------

    /// The held cars whose probe is due, with its kind (C3 R3a, R51, R52), marked
    /// in flight. Nothing is due while the account waits or is in a state that
    /// sends nothing. An Owner-wire service probe is a record request; every other
    /// probe is a read sent only while the latest list or record says `online`, so
    /// a probe never reads a sleeping car (R38). The probes share the in-flight cap
    /// of R12 with the reads.
    fn due_probe_vehicles(&mut self, now: Instant) -> Vec<(VehicleId, ProbeKind)> {
        if !self.requests_allowed(now) {
            return Vec::new();
        }
        let owner_wire = self.mode.is_owner_wire();
        let in_flight = self
            .vehicles
            .values()
            .filter(|vehicle| vehicle.in_flight)
            .count();
        let capacity = K_MAX_IN_FLIGHT.saturating_sub(in_flight);
        let mut due: Vec<(Instant, VehicleId, ProbeKind)> = self
            .vehicles
            .iter()
            .filter(|(_, vehicle)| !vehicle.in_flight)
            .filter_map(|(id, vehicle)| {
                let at = vehicle.probe_due_at(owner_wire).filter(|at| *at <= now)?;
                let kind = if vehicle.hold == VehicleHold::Service && owner_wire {
                    ProbeKind::ServiceRecord
                } else {
                    ProbeKind::Read
                };
                Some((at, *id, kind))
            })
            .collect();
        due.sort_by_key(|(at, id, _)| (*at, *id));
        due.truncate(capacity);
        for (_, id, _) in &due {
            if let Some(vehicle) = self.vehicles.get_mut(id) {
                vehicle.in_flight = true;
            }
        }
        due.into_iter().map(|(_, id, kind)| (id, kind)).collect()
    }

    /// A probe finished (C3 R3a, R51, R52). Returns whether the car newly entered
    /// service mode, for the caller's `service_refused` event.
    ///
    /// - An Owner-wire record probe that says the car is still in service re-arms
    ///   the probe; one that says it is not releases the hold (`service_exited`),
    ///   and a car Tesla reports online is read at once, because a read is what
    ///   lets C1 clear its service flag (C1 section 7 item 11).
    /// - A probe read that succeeded releases a Service hold and its snapshot is
    ///   processed as any read (`service_exited`), and releases a pause (`resumed`)
    ///   to phase `Unknown`: only the car's own answer proves it reachable again, so
    ///   a list success never clears a pause.
    /// - A probe that failed re-arms its probe. An account-scope failure sets the
    ///   account's wait like any request's and leaves the probe due after it. A 405
    ///   "in service" from a paused car's probe moves it to Service; a refusal,
    ///   mobile-access or legal answer pauses it for that reason; a 404 asks for the
    ///   list.
    fn probe_finished(&mut self, id: VehicleId, outcome: ProbeOutcome<'_>, now: Instant) -> bool {
        if let Some(vehicle) = self.vehicles.get_mut(&id) {
            vehicle.in_flight = false;
        }
        match outcome {
            ProbeOutcome::InService(true) => {
                self.note_success(now);
                if let Some(vehicle) = self.vehicles.get_mut(&id)
                    && vehicle.hold == VehicleHold::Service
                {
                    vehicle.next_probe_due = Some(deadline_after(now, K_SERVICE_PROBE));
                    let hub_id = self.hub_label(id);
                    tracing::debug!(
                        target: "collector.decision",
                        vehicle = %hub_id,
                        reason = "service_probe",
                        "the car is still in service"
                    );
                }
                false
            }
            ProbeOutcome::InService(false) => {
                self.note_success(now);
                if self.release_hold(id, "service_exited", now)
                    && let Some(vehicle) = self.vehicles.get_mut(&id)
                    && vehicle.reported_online()
                {
                    vehicle.next_read_due = Some(now);
                }
                false
            }
            ProbeOutcome::Read(phase, sleep_eligible) => {
                let hold = self.vehicles.get(&id).map(|vehicle| vehicle.hold);
                match hold {
                    Some(VehicleHold::Service) => {
                        self.release_hold(id, "service_exited", now);
                        self.vehicle_succeeded(id, phase, sleep_eligible, now);
                    }
                    Some(VehicleHold::Paused(_)) => {
                        self.note_success(now);
                        let wall_ms = self.wall_or_last();
                        self.release_hold(id, "resumed", now);
                        if let Some(vehicle) = self.vehicles.get_mut(&id) {
                            vehicle.last_read = Some(now);
                            vehicle.last_success = Some((now, wall_ms));
                        }
                    }
                    _ => {
                        self.vehicle_succeeded(id, phase, sleep_eligible, now);
                    }
                }
                false
            }
            ProbeOutcome::Failed(error) => self.probe_failed(id, error, now),
        }
    }

    /// A probe's failure (C3 R51, R52): see [`Self::probe_finished`].
    fn probe_failed(&mut self, id: VehicleId, error: &CollectorError, now: Instant) -> bool {
        let class = classify(error, RequestKind::Probe, self.mode);
        if class != ErrorClass::Unreachable {
            self.note_answered(now);
        }
        if self.account_failed(class, error, now).is_some() {
            // The probe stays due; the account's wait holds it back.
            return false;
        }
        if self.mode.is_owner_wire() && is_owner_wire_forbidden(error) {
            self.request_refusal_check(now);
        }
        let status = http_status_of(error);
        let hold = self.vehicles.get(&id).map(|vehicle| vehicle.hold);
        let rearm = |vehicle: &mut ScheduledVehicle| {
            let bound = if vehicle.hold == VehicleHold::Service {
                K_SERVICE_PROBE
            } else {
                K_PAUSE_PROBE
            };
            if matches!(vehicle.hold, VehicleHold::Service | VehicleHold::Paused(_))
                && !matches!(
                    vehicle.hold,
                    VehicleHold::Paused(PauseReason::AccessRefused)
                )
            {
                vehicle.next_probe_due = Some(deadline_after(now, bound));
            }
            vehicle.last_error = Some(class);
        };
        match class {
            ErrorClass::InService if hold != Some(VehicleHold::Service) => {
                self.enter_service_mode(id, now)
            }
            ErrorClass::MobileAccessDisabled => {
                self.pause_vehicle(id, PauseReason::MobileAccessDisabled, class, status, now);
                false
            }
            ErrorClass::LegalPrivacy => {
                self.pause_vehicle(id, PauseReason::LegalPrivacy, class, status, now);
                false
            }
            ErrorClass::AccessRefused => {
                self.pause_vehicle(id, PauseReason::AccessRefused, class, status, now);
                false
            }
            ErrorClass::NotFound => {
                self.vehicle_not_found(id, now);
                if let Some(vehicle) = self.vehicles.get_mut(&id) {
                    rearm(vehicle);
                }
                false
            }
            _ => {
                if let Some(vehicle) = self.vehicles.get_mut(&id) {
                    rearm(vehicle);
                }
                false
            }
        }
    }

    // ---- Owner actions (C3 R65) -------------------------------------------------------

    /// Apply the owner's unapplied actions (C3 R65) and return the ids applied. They
    /// join the tick's pending set, so the transaction that stores their effects
    /// also sets `applied_at_ms`; a failed commit returns the scheduler to its
    /// baseline and the rows are read again at the next refresh.
    ///
    /// - `retry_collection` is valid in every mode: it clears C3's non-collecting
    ///   state (the credential unit does its own part), every account wait and every
    ///   pause, and makes one list due at once (`owner_retry`).
    /// - `resume_vehicle` clears that car's pause (`owner_resume`), phase `Unknown`;
    ///   for a car that is not paused it is applied and changes nothing.
    ///
    /// Neither sends anything beyond that one list: R34 then applies. The cap is
    /// configuration read at the next start (R7), so retry only re-arms the check.
    fn apply_owner_actions(&mut self, actions: &[OwnerActionRow], now: Instant) -> Vec<ActionId> {
        let mut applied = Vec::new();
        for action in actions {
            if self.pending.applied_actions.contains(&action.id) {
                continue;
            }
            match action.kind {
                OwnerActionKind::RetryCollection => self.owner_retry(now),
                OwnerActionKind::ResumeVehicle => self.owner_resume(action.vehicle, now),
            }
            self.pending.applied_actions.push(action.id);
            applied.push(action.id);
        }
        applied
    }

    /// `retry_collection` (C3 R65).
    fn owner_retry(&mut self, now: Instant) {
        let wall_ms = self.wall_or_last();
        let from = self.account.state;
        // The credential unit lifts a `CredentialStop` of the Owner wire (C5 R44).
        // In Fleet polling there is no unit: the stop is C3's own, and the owner's
        // retry (or a start with a changed fingerprint) is what lifts it (R47).
        let fleet_stop = from == AccountState::CredentialStop
            && self.mode == crate::config::CollectorMode::FleetPolling;
        if fleet_stop
            || !matches!(
                from,
                AccountState::Collecting | AccountState::CredentialStop
            )
        {
            self.account.state = AccountState::Collecting;
            self.account.state_reason = "owner_retry";
            self.account.state_since_ms = wall_ms;
            self.account.next_billing_probe = None;
        }
        self.account.wait = None;
        self.account.list_wanted = Some(now);
        self.account.list_floor = None;
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = from.code(),
            to = self.account.state.code(),
            reason = "owner_retry",
            gated = false,
            "collector transition"
        );
        let paused: Vec<VehicleId> = self
            .vehicles
            .iter()
            .filter(|(_, vehicle)| matches!(vehicle.hold, VehicleHold::Paused(_)))
            .map(|(id, _)| *id)
            .collect();
        for id in paused {
            self.release_hold(id, "owner_retry", now);
        }
        self.restored
            .retain(|_, restored| restored.row.hold != "paused");
    }

    /// `resume_vehicle` (C3 R65): the car is named by its Hub id.
    fn owner_resume(&mut self, hub_id: Option<Uuid>, now: Instant) {
        let Some(hub_id) = hub_id else {
            return;
        };
        let known = self
            .vehicles
            .iter()
            .find(|(_, vehicle)| vehicle.hub_id == Some(hub_id))
            .map(|(id, _)| *id);
        match known {
            Some(id) => {
                if self
                    .vehicles
                    .get(&id)
                    .is_some_and(|vehicle| matches!(vehicle.hold, VehicleHold::Paused(_)))
                {
                    self.release_hold(id, "owner_resume", now);
                }
            }
            // A car restored as paused that no list has named yet: drop the stored
            // hold so it does not come back when the car first appears.
            None => self.restored.retain(|_, restored| {
                !(restored.row.vehicle_id == hub_id && restored.row.hold == "paused")
            }),
        }
    }

    /// The car as the scheduler holds it, for the loop that stores a Hub event about
    /// it (C3 R52).
    fn vehicle_record(&self, id: VehicleId) -> Option<Vehicle> {
        self.vehicles
            .get(&id)
            .map(|vehicle| vehicle.vehicle.clone())
    }
}
