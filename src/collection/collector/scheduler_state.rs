// SPDX-License-Identifier: AGPL-3.0-only

// Durable scheduler state (C3 part K3): the persisted views of the scheduler
// and their diff against memory (R4, R5), the restart load (R7), the commit
// protocol of a tick (R6), collection gaps (R8) and the lifecycle facts R14 and
// R40 read. It is the second half of `scheduler.rs`, kept in its own file so
// neither grows past the layout gate; both are included into the collector
// module.

/// The kinds of collection gap the scheduler raises (C3 R4, R8). The gaps
/// `hub_down` and `hub_stopped` belong to the start and stop of a run and are
/// written by the store (`begin_collector_run`, `end_collector_run`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GapKind {
    HostSuspended,
    RateLimited,
    TeslaUnreachable,
    VehiclePaused,
    AuthTerminal,
    OwnerApiRefused,
    ProviderRefused,
    FleetAccessRefused,
    BillingSuspended,
    CapReached,
    ConfigError,
    AccountLocked,
}

impl GapKind {
    /// The value stored in `collection_gaps.kind` (C3 R4).
    const fn code(self) -> &'static str {
        match self {
            Self::HostSuspended => "host_suspended",
            Self::RateLimited => "rate_limited",
            Self::TeslaUnreachable => "tesla_unreachable",
            Self::VehiclePaused => "vehicle_paused",
            Self::AuthTerminal => "auth_terminal",
            Self::OwnerApiRefused => "owner_api_refused",
            Self::ProviderRefused => "provider_refused",
            Self::FleetAccessRefused => "fleet_access_refused",
            Self::BillingSuspended => "billing_suspended",
            Self::CapReached => "cap_reached",
            Self::ConfigError => "config_error",
            Self::AccountLocked => "account_locked",
        }
    }

    /// The gap kind an account state opens while it lasts (C3 R8): the state's
    /// own kind, and `auth_terminal` for C5's credential stop. `None` while
    /// collecting.
    const fn of_account_state(state: AccountState) -> Option<Self> {
        match state {
            AccountState::Collecting => None,
            AccountState::OwnerApiRefused => Some(Self::OwnerApiRefused),
            AccountState::ProviderRefused => Some(Self::ProviderRefused),
            AccountState::FleetAccessRefused => Some(Self::FleetAccessRefused),
            AccountState::BillingSuspended => Some(Self::BillingSuspended),
            AccountState::CapReached => Some(Self::CapReached),
            AccountState::ConfigError => Some(Self::ConfigError),
            AccountState::AccountLocked => Some(Self::AccountLocked),
            AccountState::CredentialStop => Some(Self::AuthTerminal),
        }
    }
}

/// A gap to store with the next tick (C3 R4, R8). `vehicle: None` means every
/// vehicle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GapRecord {
    kind: GapKind,
    vehicle: Option<VehicleId>,
    started_ms: i64,
    ended_ms: Option<i64>,
    alert: bool,
}

/// The identity of an open gap (C3 R4): at most one gap of a kind is open for a
/// vehicle, or for every vehicle, at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GapKey {
    kind: GapKind,
    vehicle: Option<VehicleId>,
}

/// An open gap as the scheduler knows it after the last commit. It keeps the
/// car's Hub id, so the gap can close after the car has left the scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenGap {
    started_ms: i64,
    alert: bool,
    hub_id: Option<Uuid>,
}

/// The persisted shape of the account record (C3 R4): what `collector_account_state`
/// holds, in the scheduler's own types, so that the scheduler can tell whether
/// memory differs from the store without reading it. A credential stop is derived
/// from the credential unit and never stored, so it persists as collecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AccountView {
    state: AccountState,
    reason: &'static str,
    since_ms: i64,
    rate_limited_until: Option<Instant>,
    next_probe: Option<Instant>,
}

/// The persisted shape of one vehicle (C3 R4): the fields of
/// `collector_vehicle_state`. Deadlines stay monotonic instants here, so two
/// views are equal exactly when the scheduler has not changed them; they become
/// wall times only when written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VehicleView {
    phase: PollPhase,
    hold: VehicleHold,
    reason: &'static str,
    phase_since_ms: i64,
    suspended_until: Option<Instant>,
    next_probe: Option<Instant>,
    last_wake_read: Option<Instant>,
}

impl VehicleView {
    /// Whether the view is what a car that has not been stored yet looks like:
    /// no phase, no hold, no deadline. Such a car needs no row.
    fn is_initial(&self) -> bool {
        self.phase == PollPhase::Unknown
            && self.hold == VehicleHold::None
            && self.reason == "collecting"
            && self.suspended_until.is_none()
            && self.next_probe.is_none()
            && self.last_wake_read.is_none()
    }
}

/// The account row of a tick, with the view it makes persisted (C3 R5).
#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountWrite {
    view: AccountView,
    row: crate::db::CollectorAccountRow,
}

/// One vehicle row of a tick, with the view it makes persisted (C3 R5).
#[derive(Clone, Debug, PartialEq, Eq)]
struct VehicleWrite {
    id: VehicleId,
    view: VehicleView,
    row: crate::db::CollectorVehicleRow,
}

/// A gap that opens with a tick (C3 R8): detected from the scheduler's state,
/// started at the wall time of its detection unless a wait gap carries its own
/// start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GapOpen {
    key: GapKey,
    hub_id: Option<Uuid>,
    started_ms: i64,
    alert: bool,
}

/// A gap that closes with a tick (C3 R8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GapClose {
    key: GapKey,
    hub_id: Option<Uuid>,
    ended_ms: i64,
    /// The length at which the gap becomes an alert whatever the cars did
    /// (`K_GAP_ALERT`, C3 R8).
    alert_at_or_over_ms: i64,
}

/// The scheduler's pending durable changes of one tick (C3 R5, R6): the rows of
/// the R4 tables that changed since the last commit, and the gaps. It is the
/// pending set of R6: the scheduler applies a tick's transitions to memory at
/// once and remembers a baseline to return to (`commit_failed`), and moves its
/// persisted views and open gaps to this set only when `commit_succeeded` says the
/// transaction committed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PendingTick {
    /// Closed gaps found by the clock watch (`host_suspended`, C3 R2).
    gaps: Vec<GapRecord>,
    account: Option<AccountWrite>,
    vehicles: Vec<VehicleWrite>,
    gap_opens: Vec<GapOpen>,
    gap_closes: Vec<GapClose>,
    /// The month's counter rows (C3 R25, R27), attached by `attach_counters`.
    counters: Vec<crate::db::CollectorCounterRow>,
    /// Owner actions applied by this tick (C3 R65): the transaction marks them applied.
    applied_actions: Vec<ActionId>,
}

impl PendingTick {
    fn is_empty(&self) -> bool {
        self.gaps.is_empty()
            && self.account.is_none()
            && self.vehicles.is_empty()
            && self.gap_opens.is_empty()
            && self.gap_closes.is_empty()
            && self.counters.is_empty()
            && self.applied_actions.is_empty()
    }

    /// The store write that carries this tick's scheduler rows (C3 R5). `now_ms`
    /// is the commit's wall time.
    fn to_rows_write(&self, now_ms: i64) -> crate::db::CollectorRowsWrite {
        let mut gap_inserts: Vec<crate::db::CollectorGapInsert> = self
            .gaps
            .iter()
            .map(|gap| crate::db::CollectorGapInsert {
                // A gap of the clock watch concerns every vehicle.
                vehicle_id: None,
                kind: gap.kind.code().to_owned(),
                started_at_ms: gap.started_ms,
                ended_at_ms: gap.ended_ms,
                alert: gap.alert,
            })
            .collect();
        gap_inserts.extend(
            self.gap_opens
                .iter()
                .map(|open| crate::db::CollectorGapInsert {
                    vehicle_id: open.hub_id,
                    kind: open.key.kind.code().to_owned(),
                    started_at_ms: open.started_ms,
                    ended_at_ms: None,
                    alert: open.alert,
                }),
        );
        crate::db::CollectorRowsWrite {
            now_ms,
            account: self.account.as_ref().map(|write| write.row.clone()),
            vehicles: self
                .vehicles
                .iter()
                .map(|write| write.row.clone())
                .collect(),
            counters: self.counters.clone(),
            gap_inserts,
            gap_closes: self
                .gap_closes
                .iter()
                .map(|close| crate::db::CollectorGapClose {
                    vehicle_id: close.hub_id,
                    kind: close.key.kind.code().to_owned(),
                    ended_at_ms: close.ended_ms,
                    alert_at_or_over_ms: close.alert_at_or_over_ms,
                })
                .collect(),
            applied_actions: self.applied_actions.clone(),
        }
    }
}

/// One stored vehicle row joined with what the scheduler needs to restore it
/// (C3 R7 step 2): its Tesla id and the car's suspend length, the bound of its
/// persisted suspension and wake-read age.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RestoredVehicle {
    tesla_id: VehicleId,
    row: crate::db::CollectorVehicleRow,
    suspend: Duration,
}

/// The one store read of C3 R7, which the scheduler loads before the first
/// tick: the account row, the vehicle rows, the month's counters and the open
/// gaps, with the configuration fingerprint of this run.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CollectorStateSnapshot {
    account: Option<crate::db::CollectorAccountRow>,
    vehicles: Vec<RestoredVehicle>,
    counters: Vec<crate::db::CollectorCounterRow>,
    open_gaps: Vec<crate::db::CollectionGapRecord>,
    /// Hub vehicle ids of the configured cars, to name a gap's vehicle.
    vehicle_ids: BTreeMap<Uuid, VehicleId>,
    /// The fingerprint of this run's configuration (C3 R4).
    config_fingerprint: String,
}

impl Default for CollectorStateSnapshot {
    fn default() -> Self {
        Self {
            account: None,
            vehicles: Vec::new(),
            counters: Vec::new(),
            open_gaps: Vec::new(),
            vehicle_ids: BTreeMap::new(),
            config_fingerprint: NO_FINGERPRINT.to_owned(),
        }
    }
}

/// The fingerprint a scheduler carries until it loads one: 64 zeros pass the
/// column's length check and match no digest.
const NO_FINGERPRINT: &str = "0000000000000000000000000000000000000000000000000000000000000000";

impl CollectorStateSnapshot {
    /// Join the stored rows with the configured cars (C3 R7): a row of a car
    /// that is no longer configured is not restored, and a car's suspend length
    /// comes from its settings.
    fn from_rows(
        rows: crate::db::CollectorStateRows,
        config_fingerprint: String,
        configured: &[(Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
    ) -> Self {
        let mut vehicle_ids = BTreeMap::new();
        let mut suspends = BTreeMap::new();
        for (hub_id, tesla_id, settings) in configured {
            if let Some(id) = VehicleId::try_from_i64(*tesla_id) {
                vehicle_ids.insert(*hub_id, id);
                suspends.insert(*hub_id, EffectiveSettings::from_car(settings).suspend);
            }
        }
        let vehicles = rows
            .vehicles
            .into_iter()
            .filter_map(|row| {
                let tesla_id = *vehicle_ids.get(&row.vehicle_id)?;
                let suspend = suspends.get(&row.vehicle_id).copied()?;
                Some(RestoredVehicle {
                    tesla_id,
                    row,
                    suspend,
                })
            })
            .collect();
        Self {
            account: rows.account,
            vehicles,
            counters: rows.counters,
            open_gaps: rows.open_gaps,
            vehicle_ids,
            config_fingerprint,
        }
    }
}

// ---------------------------------------------------------------------------
// Durable state, ticks and gaps (C3 part K3).
// ---------------------------------------------------------------------------

/// The closed list of reason codes (C3 section 7.4). Rows store a reason as
/// bounded text (R4), and a restored row maps back to one of these so a code
/// stays a `&'static str` in memory and in the decision log.
const REASON_CODES: &[&str] = &[
    "first_contact_read",
    "wake_read",
    "wake_read_limited",
    "session_return",
    "use_evidence_snapshot",
    "use_evidence_stream",
    "use_evidence_telemetry",
    "idle_window_started",
    "suspended_idle",
    "suspension_ended_still_online",
    "reported_asleep",
    "reported_offline",
    "reported_online",
    "update_stale",
    "stream_outage",
    "stream_recovered",
    "stream_restart_delayed",
    "stream_audit_failed",
    "rest_failure",
    "refusal_check",
    "paused_persistent_failure",
    "paused_mobile_access_disabled",
    "paused_legal_privacy",
    "paused_access_refused",
    "pause_probe",
    "resumed",
    "service_entered",
    "service_probe",
    "service_exited",
    "rate_limited",
    "tesla_unreachable",
    "tesla_reachable",
    "auth_wait",
    "auth_terminal",
    "owner_api_refused",
    "owner_api_client_refused",
    "owner_api_gone",
    "provider_payment_required",
    "provider_gone",
    "provider_forbidden",
    "provider_client_refused",
    "fleet_access_refused",
    "billing_suspended",
    "billing_probe",
    "billing_resumed",
    "config_partner_unregistered",
    "config_wrong_region",
    "account_locked",
    "cap_warning",
    "cap_reached",
    "cap_cleared",
    "month_rolled_over",
    "month_change_deferred",
    "vehicle_missing_from_list",
    "vehicle_removed",
    "vehicle_reappeared",
    "vehicle_disabled",
    "vehicle_enabled",
    "settings_applied",
    "owner_retry",
    "owner_resume",
    "config_changed",
    "force_close_no_evidence",
    "gap_opened",
    "gap_closed",
    "config_key_ignored",
    "config_value_clamped",
    "wall_clock_stepped_back",
    "store_failure",
    "store_recovered",
    "collecting",
];

/// The reason a stored code stands for. A code outside the closed list cannot be
/// written by this scheduler, so it reads as the plain state.
fn intern_reason(code: &str) -> &'static str {
    REASON_CODES
        .iter()
        .copied()
        .find(|known| *known == code)
        .unwrap_or("collecting")
}

fn account_state_from_code(code: &str) -> AccountState {
    match code {
        "owner_api_refused" => AccountState::OwnerApiRefused,
        "provider_refused" => AccountState::ProviderRefused,
        "fleet_access_refused" => AccountState::FleetAccessRefused,
        "billing_suspended" => AccountState::BillingSuspended,
        "cap_reached" => AccountState::CapReached,
        "config_error" => AccountState::ConfigError,
        "account_locked" => AccountState::AccountLocked,
        _ => AccountState::Collecting,
    }
}

fn phase_from_code(code: &str) -> PollPhase {
    match code {
        "asleep" => PollPhase::Asleep,
        "offline" => PollPhase::Offline,
        "online" => PollPhase::Online,
        "suspended" => PollPhase::Suspended,
        "driving" => PollPhase::Driving,
        "charging" => PollPhase::Charging,
        "updating" => PollPhase::Updating,
        _ => PollPhase::Unknown,
    }
}

fn pause_reason_from_code(code: &str) -> PauseReason {
    match code {
        "paused_mobile_access_disabled" => PauseReason::MobileAccessDisabled,
        "paused_legal_privacy" => PauseReason::LegalPrivacy,
        "paused_access_refused" => PauseReason::AccessRefused,
        _ => PauseReason::PersistentFailure,
    }
}

/// The wall time (epoch milliseconds) of a monotonic instant, given a reading of
/// both clocks (C3 R4: persisted deadlines are wall times). Never negative.
fn wall_of_instant(mono_now: Instant, wall_now_ms: i64, instant: Instant) -> i64 {
    let millis = |duration: Duration| i128::try_from(duration.as_millis()).unwrap_or(i128::MAX);
    let delta = if instant >= mono_now {
        millis(instant - mono_now)
    } else {
        -millis(mono_now - instant)
    };
    i64::try_from((i128::from(wall_now_ms) + delta).max(0)).unwrap_or(i64::MAX)
}

/// What a car's record held before a tick began, the fields a tick's results
/// change (C3 R6). Facts about requests (`in_flight`, `last_read`,
/// `last_wake_read`), the stream, the lifecycle cache and the car's settings are
/// not here: they are never rolled back.
#[derive(Clone, Debug)]
struct VehicleBaseline {
    state: String,
    phase: PollPhase,
    phase_since: (Instant, i64),
    hold: VehicleHold,
    reason: &'static str,
    reported_state: Option<String>,
    next_read_due: Option<Instant>,
    next_check_due: Option<Instant>,
    next_probe_due: Option<Instant>,
    suspend_at: Option<Instant>,
    suspended_until: Option<Instant>,
    last_in_use: Option<Instant>,
    last_success: Option<(Instant, i64)>,
    offline_from_session: bool,
    consecutive_failures: u32,
    first_failure: Option<Instant>,
    last_error: Option<ErrorClass>,
    missing_lists: u8,
    missing_last: Option<Instant>,
    not_found_check: bool,
    placeholder: bool,
}

/// What the account record held before a tick began (C3 R6). The waits Tesla's
/// answers set, the 429 and unreachable counts, the store failure count and the
/// request counters are facts and stay.
#[derive(Clone, Debug)]
struct AccountBaseline {
    state: AccountState,
    state_reason: &'static str,
    state_since_ms: i64,
    last_discovery: Option<Instant>,
    list_floor: Option<Instant>,
    list_wanted: Option<Instant>,
    discovery_failures: u32,
    next_billing_probe: Option<Instant>,
    telemetry_check_due: bool,
}

/// The state a failed commit returns the scheduler to (C3 R6).
#[derive(Clone, Debug)]
struct TickBaseline {
    vehicles: BTreeMap<VehicleId, VehicleBaseline>,
    account: AccountBaseline,
}

impl VehicleScheduler {
    // ---- Restart (C3 R7) ----------------------------------------------------

    /// Load the persisted state before the first tick (C3 R7): the account row,
    /// the cars' rows and the open gaps, converting stored wall deadlines to
    /// monotonic ones through the clamp of R2.
    ///
    /// - A C3-owned non-collecting account state is restored unless the mode or
    ///   the configuration fingerprint differs; then it resets to collecting
    ///   (`config_changed`), and the next tick writes the reset and closes its
    ///   gap. The start path commits that reset before the credential unit starts
    ///   (`begin_collector_run`), so here it normally finds a matching row.
    /// - A future `suspended_until_ms` restores Suspended; a `paused` or `service`
    ///   hold and a `removed` hold are restored with their probe deadlines, not
    ///   due at once; every other phase restores as Unknown; the last wake read
    ///   loads as a past instant.
    /// - A car's row is applied when the car first appears in a list, or at once
    ///   when it is already known.
    ///
    /// Reasons: a crash loop must not keep a car awake by reading it at every
    /// start, and a restart must not reset a cap, a wait or a probe schedule
    /// (C3 R7).
    fn load(&mut self, snapshot: &CollectorStateSnapshot, now: Instant) {
        let wall = self.wall_or_last();
        self.config_fingerprint = snapshot.config_fingerprint.clone();
        if let Some(row) = &snapshot.account {
            let state = account_state_from_code(&row.state);
            let reason = intern_reason(&row.reason_code);
            let rate_limited_until = row
                .rate_limited_until_ms
                .map(|ms| deadline_from_wall(now, wall, ms, K_RETRY_AFTER_MAX));
            let next_probe = row
                .next_probe_ms
                .map(|ms| deadline_from_wall(now, wall, ms, K_BILLING_PROBE));
            self.persisted_account = AccountView {
                state,
                reason,
                since_ms: row.since_ms,
                rate_limited_until,
                next_probe,
            };
            let same_configuration = row.mode == self.mode.code()
                && row.config_fingerprint == snapshot.config_fingerprint;
            if same_configuration {
                self.account.state = state;
                self.account.state_reason = reason;
                self.account.state_since_ms = row.since_ms;
                self.account.wait = rate_limited_until
                    .filter(|until| *until > now)
                    .map(|until| (until, WaitReason::RateLimited));
                self.account.next_billing_probe = next_probe;
            } else if state != AccountState::Collecting {
                self.account.state = AccountState::Collecting;
                self.account.state_reason = "config_changed";
                self.account.state_since_ms = wall;
                tracing::info!(
                    target: "collector.decision",
                    vehicle = "account",
                    from = state.code(),
                    to = AccountState::Collecting.code(),
                    reason = "config_changed",
                    "collector transition"
                );
            }
        }
        for restored in &snapshot.vehicles {
            let view = Self::view_from_row(restored, now, wall);
            self.persisted_vehicles.insert(restored.tesla_id, view);
            if let Some(vehicle) = self.vehicles.get_mut(&restored.tesla_id) {
                Self::apply_restored(vehicle, restored, now, wall);
            } else {
                self.restored.insert(restored.tesla_id, restored.clone());
            }
        }
        for gap in &snapshot.open_gaps {
            let Some(kind) = Self::gap_kind_from_code(&gap.kind) else {
                continue;
            };
            let vehicle = match gap.vehicle_id {
                Some(hub_id) => match snapshot.vehicle_ids.get(&hub_id) {
                    Some(id) => Some(*id),
                    None => continue,
                },
                None => None,
            };
            self.open_gaps.insert(
                GapKey { kind, vehicle },
                OpenGap {
                    started_ms: gap.started_at_ms,
                    alert: gap.alert,
                    hub_id: gap.vehicle_id,
                },
            );
        }
        self.alert_gaps_unnotified = snapshot
            .open_gaps
            .iter()
            .filter(|gap| gap.alert && gap.notified_at_ms.is_none())
            .count();
        // The month's counts and the reservation (R7 step 3, R27), after the account
        // row so that a stop by the cap can be lifted by a raised cap or a new month.
        self.load_counters(&snapshot.counters, now);
    }

    fn gap_kind_from_code(code: &str) -> Option<GapKind> {
        [
            GapKind::HostSuspended,
            GapKind::RateLimited,
            GapKind::TeslaUnreachable,
            GapKind::VehiclePaused,
            GapKind::AuthTerminal,
            GapKind::OwnerApiRefused,
            GapKind::ProviderRefused,
            GapKind::FleetAccessRefused,
            GapKind::BillingSuspended,
            GapKind::CapReached,
            GapKind::ConfigError,
            GapKind::AccountLocked,
        ]
        .into_iter()
        .find(|kind| kind.code() == code)
    }

    /// The probe bound of a stored hold (C3 R2): the duration that produced the
    /// deadline.
    fn probe_bound(hold: &str) -> Duration {
        if hold == "service" {
            K_SERVICE_PROBE
        } else {
            K_PAUSE_PROBE
        }
    }

    /// The persisted view of a stored vehicle row, with its deadlines loaded by
    /// the clamp of R2. It must match what [`Self::apply_restored`] puts in the
    /// car's record, or the next tick would rewrite the row for no reason.
    fn view_from_row(restored: &RestoredVehicle, now: Instant, wall: i64) -> VehicleView {
        let row = &restored.row;
        let hold = match row.hold.as_str() {
            "disabled" => VehicleHold::Disabled,
            "service" => VehicleHold::Service,
            "paused" => VehicleHold::Paused(pause_reason_from_code(&row.reason_code)),
            "removed" => VehicleHold::Removed,
            _ => VehicleHold::None,
        };
        VehicleView {
            phase: phase_from_code(&row.phase),
            hold,
            reason: intern_reason(&row.reason_code),
            phase_since_ms: row.phase_since_ms,
            suspended_until: row
                .suspended_until_ms
                .map(|ms| deadline_from_wall(now, wall, ms, restored.suspend)),
            next_probe: row
                .next_probe_ms
                .map(|ms| deadline_from_wall(now, wall, ms, Self::probe_bound(&row.hold))),
            last_wake_read: row
                .last_wake_read_at_ms
                .and_then(|ms| past_instant_from_wall(now, wall, ms, restored.suspend)),
        }
    }

    /// Apply a stored row to a car's record (C3 R7 step 2). A Disabled car stays
    /// Disabled: the owner's `enabled` setting is the one source of that hold.
    fn apply_restored(
        vehicle: &mut ScheduledVehicle,
        restored: &RestoredVehicle,
        now: Instant,
        wall: i64,
    ) {
        let row = &restored.row;
        vehicle.hub_id = Some(row.vehicle_id);
        vehicle.last_wake_read = row
            .last_wake_read_at_ms
            .and_then(|ms| past_instant_from_wall(now, wall, ms, restored.suspend));
        if vehicle.hold == VehicleHold::Disabled {
            return;
        }
        let reason = intern_reason(&row.reason_code);
        let probe = |bound: Duration| {
            row.next_probe_ms
                .map(|ms| deadline_from_wall(now, wall, ms, bound))
                .unwrap_or_else(|| deadline_after(now, bound))
        };
        // A held car keeps the time its stored row says it entered its phase, so a
        // restart rewrites no row for it (C3 R10).
        if matches!(row.hold.as_str(), "removed" | "paused" | "service") {
            vehicle.phase_since = (now, row.phase_since_ms);
        }
        match row.hold.as_str() {
            "removed" => {
                vehicle.hold = VehicleHold::Removed;
                vehicle.reason = reason;
            }
            "paused" => {
                let pause = pause_reason_from_code(&row.reason_code);
                vehicle.hold = VehicleHold::Paused(pause);
                // A pause by `AccessRefused` has no probe (C3 R51): the owner's
                // remedy is the only way out, so a restart invents no deadline.
                vehicle.next_probe_due =
                    (pause != PauseReason::AccessRefused).then(|| probe(K_PAUSE_PROBE));
                vehicle.reason = reason;
            }
            "service" => {
                vehicle.hold = VehicleHold::Service;
                vehicle.next_probe_due = Some(probe(K_SERVICE_PROBE));
                vehicle.reason = reason;
            }
            _ => {
                if row.phase == "suspended"
                    && let Some(until_ms) = row.suspended_until_ms.filter(|ms| *ms > wall)
                {
                    vehicle.phase = PollPhase::Suspended;
                    vehicle.suspended_until =
                        Some(deadline_from_wall(now, wall, until_ms, restored.suspend));
                    vehicle.phase_since = (
                        past_instant_from_wall(now, wall, row.phase_since_ms, restored.suspend)
                            .unwrap_or(now),
                        row.phase_since_ms,
                    );
                    // A car is Suspended only while online, and the alert rule of
                    // a gap reads the reported state.
                    vehicle.reported_state = Some("online".to_owned());
                    vehicle.reason = reason;
                }
            }
        }
    }

    // ---- The tick's commit protocol (C3 R5, R6) --------------------------------

    /// Remember the state a failed commit returns to (C3 R6). The loop calls it
    /// before a tick sends requests; `commit_succeeded` renews it.
    fn begin_tick(&mut self) {
        self.account.probe_time_of_the_tick = None;
        self.baseline = Some(self.capture_baseline());
    }

    /// Take a new baseline now: nothing was written, so what a later failure
    /// returns to is the present state.
    fn renew_baseline(&mut self) {
        self.baseline = Some(self.capture_baseline());
    }

    fn capture_baseline(&self) -> TickBaseline {
        TickBaseline {
            vehicles: self
                .vehicles
                .iter()
                .map(|(id, vehicle)| {
                    (
                        *id,
                        VehicleBaseline {
                            state: vehicle.vehicle.state.clone(),
                            phase: vehicle.phase,
                            phase_since: vehicle.phase_since,
                            hold: vehicle.hold,
                            reason: vehicle.reason,
                            reported_state: vehicle.reported_state.clone(),
                            next_read_due: vehicle.next_read_due,
                            next_check_due: vehicle.next_check_due,
                            next_probe_due: vehicle.next_probe_due,
                            suspend_at: vehicle.suspend_at,
                            suspended_until: vehicle.suspended_until,
                            last_in_use: vehicle.last_in_use,
                            last_success: vehicle.last_success,
                            offline_from_session: vehicle.offline_from_session,
                            consecutive_failures: vehicle.consecutive_failures,
                            first_failure: vehicle.first_failure,
                            last_error: vehicle.last_error,
                            missing_lists: vehicle.missing_lists,
                            missing_last: vehicle.missing_last,
                            not_found_check: vehicle.not_found_check,
                            placeholder: vehicle.placeholder,
                        },
                    )
                })
                .collect(),
            account: AccountBaseline {
                state: self.account.state,
                state_reason: self.account.state_reason,
                state_since_ms: self.account.state_since_ms,
                last_discovery: self.account.last_discovery,
                list_floor: self.account.list_floor,
                list_wanted: self.account.list_wanted,
                discovery_failures: self.account.discovery_failures,
                next_billing_probe: self.account.next_billing_probe,
                telemetry_check_due: self.account.telemetry_check_due,
            },
        }
    }

    fn restore_baseline(&mut self, baseline: TickBaseline) {
        // A car the failed tick first saw is forgotten; its list is due again.
        self.vehicles
            .retain(|id, _| baseline.vehicles.contains_key(id));
        for (id, base) in baseline.vehicles {
            let Some(vehicle) = self.vehicles.get_mut(&id) else {
                continue;
            };
            vehicle.vehicle.state = base.state;
            vehicle.phase = base.phase;
            vehicle.phase_since = base.phase_since;
            vehicle.hold = base.hold;
            vehicle.reason = base.reason;
            vehicle.reported_state = base.reported_state;
            vehicle.next_read_due = base.next_read_due;
            vehicle.next_check_due = base.next_check_due;
            vehicle.next_probe_due = base.next_probe_due;
            vehicle.suspend_at = base.suspend_at;
            vehicle.suspended_until = base.suspended_until;
            vehicle.last_in_use = base.last_in_use;
            vehicle.last_success = base.last_success;
            vehicle.offline_from_session = base.offline_from_session;
            vehicle.consecutive_failures = base.consecutive_failures;
            vehicle.first_failure = base.first_failure;
            vehicle.last_error = base.last_error;
            vehicle.missing_lists = base.missing_lists;
            vehicle.missing_last = base.missing_last;
            vehicle.not_found_check = base.not_found_check;
            vehicle.placeholder = base.placeholder;
            // The decisions that qualify a due read (C3 R34, R36) return with the
            // due times they belong to (part K4).
            vehicle.gated_read = None;
            vehicle.wake_read_pending = false;
            vehicle.suspension_read = false;
        }
        let account = baseline.account;
        self.account.state = account.state;
        self.account.state_reason = account.state_reason;
        self.account.state_since_ms = account.state_since_ms;
        self.account.last_discovery = account.last_discovery;
        self.account.list_floor = account.list_floor;
        self.account.list_wanted = account.list_wanted;
        self.account.discovery_failures = account.discovery_failures;
        self.account.next_billing_probe = account.next_billing_probe;
        self.account.telemetry_check_due = account.telemetry_check_due;
    }

    /// The persisted shape of the account record now (C3 R4). A credential stop
    /// is derived and never stored, so it leaves the stored view as it is.
    fn account_view(&self) -> AccountView {
        if self.account.state == AccountState::CredentialStop {
            return self.persisted_account;
        }
        AccountView {
            state: self.account.state,
            reason: self.account.state_reason,
            since_ms: self.account.state_since_ms,
            rate_limited_until: self
                .account
                .wait
                .filter(|(_, reason)| *reason == WaitReason::RateLimited)
                .map(|(until, _)| until),
            next_probe: self.account.next_billing_probe,
        }
    }

    fn vehicle_view(vehicle: &ScheduledVehicle) -> VehicleView {
        VehicleView {
            phase: vehicle.phase,
            hold: vehicle.hold,
            reason: vehicle.shown_reason(),
            phase_since_ms: vehicle.phase_since.1,
            suspended_until: if vehicle.phase == PollPhase::Suspended {
                vehicle.suspended_until
            } else {
                None
            },
            next_probe: if matches!(vehicle.hold, VehicleHold::Service | VehicleHold::Paused(_)) {
                vehicle.next_probe_due
            } else {
                None
            },
            last_wake_read: vehicle.last_wake_read,
        }
    }

    /// Hand over the tick's pending durable changes (C3 R5, R6): the rows whose
    /// persisted fields differ from the store's, the gaps that opened or closed,
    /// and the gaps found by the clock watch. Nothing is written for an unchanged
    /// poll. The scheduler's persisted views move only when `commit_succeeded`
    /// reports the transaction.
    fn take_tick(&mut self) -> PendingTick {
        let mut tick = std::mem::take(&mut self.pending);
        let now = self.clock.mono_now();
        let wall = self.wall_or_last();
        let account_view = self.account_view();
        if account_view != self.persisted_account {
            tick.account = Some(AccountWrite {
                view: account_view,
                row: crate::db::CollectorAccountRow {
                    mode: self.mode.code().to_owned(),
                    config_fingerprint: self.config_fingerprint.clone(),
                    state: account_view.state.code().to_owned(),
                    reason_code: account_view.reason.to_owned(),
                    since_ms: account_view.since_ms.max(0),
                    rate_limited_until_ms: account_view
                        .rate_limited_until
                        .map(|until| wall_of_instant(now, wall, until)),
                    next_probe_ms: account_view
                        .next_probe
                        .map(|probe| wall_of_instant(now, wall, probe)),
                },
            });
        }
        for (id, vehicle) in &self.vehicles {
            // A car whose Hub identity is not known yet has no row to write; its
            // change stays pending until `apply_control_settings` names it.
            let Some(hub_id) = vehicle.hub_id else {
                continue;
            };
            // A car no list has named yet has seen nothing this run: its stored row
            // stands until the list that names it (a restart rewrites no row, R10),
            // unless its hold changed (an owner's retry released it).
            if vehicle.placeholder
                && self
                    .persisted_vehicles
                    .get(id)
                    .is_none_or(|persisted| persisted.hold == vehicle.hold)
            {
                continue;
            }
            let view = Self::vehicle_view(vehicle);
            let changed = match self.persisted_vehicles.get(id) {
                Some(persisted) => *persisted != view,
                None => !view.is_initial(),
            };
            if !changed {
                continue;
            }
            tick.vehicles.push(VehicleWrite {
                id: *id,
                view,
                row: crate::db::CollectorVehicleRow {
                    vehicle_id: hub_id,
                    phase: view.phase.code().to_owned(),
                    hold: view.hold.code().to_owned(),
                    reason_code: view.reason.to_owned(),
                    phase_since_ms: view.phase_since_ms.max(0),
                    suspended_until_ms: view
                        .suspended_until
                        .map(|until| wall_of_instant(now, wall, until)),
                    next_probe_ms: view
                        .next_probe
                        .map(|probe| wall_of_instant(now, wall, probe)),
                    last_wake_read_at_ms: view
                        .last_wake_read
                        .map(|read| wall_of_instant(now, wall, read)),
                },
            });
        }
        self.derive_gaps(&mut tick, now, wall);
        tick
    }

    /// The gaps that should be open now (C3 R8), compared with the gaps the store
    /// holds open: a gap opens when its condition appears and closes when it
    /// ends. The conditions are the account state (its own kind; `auth_terminal`
    /// for a credential stop), a car's Paused hold, and an account wait episode
    /// (429 or unreachable) that has lasted `K_GAP_MIN`, counted from its first
    /// failure.
    fn derive_gaps(&mut self, tick: &mut PendingTick, now: Instant, wall: i64) {
        let mut wanted: BTreeMap<GapKey, i64> = BTreeMap::new();
        if let Some(kind) = GapKind::of_account_state(self.account.state) {
            // A credential stop's gap begins when the stop was entered, even when
            // the tick that entered it failed to commit and the gap is stored with a
            // later one (B2 fix round, P2).
            let began_ms = if self.account.state == AccountState::CredentialStop {
                self.account.state_since_ms
            } else {
                wall
            };
            wanted.insert(
                GapKey {
                    kind,
                    vehicle: None,
                },
                began_ms,
            );
        }
        // A credential stop derived over a retained account stop (a cap or billing
        // stop the ordinary start loaded) leaves that stop's stored row and its
        // open gap alone: the policy survives until the stop lifts and is judged
        // again.
        if self.account.state == AccountState::CredentialStop
            && let Some(kind) = GapKind::of_account_state(self.persisted_account.state)
        {
            wanted.insert(
                GapKey {
                    kind,
                    vehicle: None,
                },
                wall,
            );
        }
        for (id, vehicle) in &self.vehicles {
            if matches!(vehicle.hold, VehicleHold::Paused(_)) && vehicle.hub_id.is_some() {
                wanted.insert(
                    GapKey {
                        kind: GapKind::VehiclePaused,
                        vehicle: Some(*id),
                    },
                    wall,
                );
            }
        }
        for (kind, active) in [
            (GapKind::RateLimited, self.account.consecutive_429 > 0),
            (
                GapKind::TeslaUnreachable,
                self.account.consecutive_unreachable > 0,
            ),
        ] {
            if !active {
                self.gap_episodes.remove(&kind);
                continue;
            }
            let (began, began_ms) = *self.gap_episodes.entry(kind).or_insert((now, wall));
            if now.saturating_duration_since(began) >= K_GAP_MIN {
                wanted.insert(
                    GapKey {
                        kind,
                        vehicle: None,
                    },
                    began_ms,
                );
            }
        }
        for (key, started_ms) in &wanted {
            if self.open_gaps.contains_key(key) {
                continue;
            }
            let hub_id = key
                .vehicle
                .and_then(|id| self.vehicles.get(&id))
                .and_then(|vehicle| vehicle.hub_id);
            tick.gap_opens.push(GapOpen {
                key: *key,
                hub_id,
                started_ms: *started_ms,
                alert: self.any_car_awake(key.vehicle),
            });
        }
        let alert_at_or_over_ms =
            i64::try_from(self.timing.offline_drive_timeout.as_millis()).unwrap_or(i64::MAX);
        for (key, open) in &self.open_gaps {
            if wanted.contains_key(key) {
                continue;
            }
            tick.gap_closes.push(GapClose {
                key: *key,
                hub_id: open.hub_id,
                ended_ms: wall,
                alert_at_or_over_ms,
            });
        }
    }

    /// The tick's transaction committed (C3 R6): the persisted views and the open
    /// gaps become the tick's, a store back-off ends, and the baseline renews so a
    /// later failure rolls back only what is not yet committed.
    fn commit_succeeded(&mut self, tick: PendingTick, now: Instant) {
        let _ = now;
        self.account.probe_time_of_the_tick = None;
        // The counter rows are the store's now, and so is the reservation they carry
        // (R27: sends rely only on a committed reservation).
        self.ledger.mark_persisted(&tick.counters);
        if let Some(write) = tick.account {
            self.persisted_account = write.view;
        }
        for write in tick.vehicles {
            self.persisted_vehicles.insert(write.id, write.view);
        }
        for open in tick.gap_opens {
            self.open_gaps.insert(
                open.key,
                OpenGap {
                    started_ms: open.started_ms,
                    alert: open.alert,
                    hub_id: open.hub_id,
                },
            );
            if open.alert {
                self.alert_gaps_unnotified = self.alert_gaps_unnotified.saturating_add(1);
            }
        }
        for close in tick.gap_closes {
            self.open_gaps.remove(&close.key);
        }
        if self.account.store_failures > 0 {
            self.account.store_failures = 0;
            if self
                .account
                .wait
                .is_some_and(|(_, reason)| reason == WaitReason::Store)
            {
                self.account.wait = None;
            }
            tracing::info!(
                target: "collector.decision",
                vehicle = "account",
                from = "store_wait",
                to = "collecting",
                reason = "store_recovered",
                "collector transition"
            );
        }
        // The stored rows of the cars this commit wrote are the store's own now;
        // a stored row of a car no list has named yet stays for its first list
        // (R7 step 2).
        let vehicles = &self.vehicles;
        self.restored
            .retain(|id, _| vehicles.get(id).is_none_or(|vehicle| vehicle.placeholder));
        self.baseline = Some(self.capture_baseline());
    }

    /// The tick's transaction failed (C3 R6): its transitions are dropped (memory
    /// returns to the baseline, so the due work stays due), the facts of its
    /// requests stay, and the account waits `backoff_delay(store_failures)` with
    /// reason `Store` before any further Tesla request. Reason: a lasting store
    /// fault would otherwise repeat the tick's requests at loop speed, and results
    /// that cannot be stored only spend Tesla's tolerance or money.
    fn commit_failed(&mut self, tick: PendingTick, now: Instant) {
        // A clock gap that was not stored is stored with the next tick.
        let mut gaps = tick.gaps;
        gaps.append(&mut self.pending.gaps);
        self.pending.gaps = gaps;
        // A credential stop the failed tick entered is derived in memory, never
        // stored (R4), so the rollback has nothing of it to undo: the unit's evidence
        // for it stands. Restoring the account to collecting would send again what
        // the stop forbids (a 401-stopped account asking Tesla once the store backs
        // off, an unresolved refresh logging its transition again), and the rolled
        // back durable gap is derived again with the next tick's rows.
        let entered_stop = (self.account.state == AccountState::CredentialStop)
            .then_some((self.account.state_reason, self.account.state_since_ms));
        if let Some(baseline) = self.baseline.take() {
            self.restore_baseline(baseline);
        }
        if let Some((reason, since_ms)) = entered_stop {
            self.account.state = AccountState::CredentialStop;
            self.account.state_reason = reason;
            self.account.state_since_ms = since_ms;
        }
        // B2 apply, R3C-03: the probe the failed tick admitted was sent and counted, so
        // its re-arm (or its owed time, for a withheld resend) survives the rollback of
        // the account. The next probe is one period after the one that went out, not
        // at the end of the store's wait.
        if let Some(armed) = self.account.probe_time_of_the_tick.take()
            && self.account.state == AccountState::BillingSuspended
        {
            self.account.next_billing_probe = Some(armed);
        }
        let first = self.account.store_failures == 0;
        self.account.store_failures = self.account.store_failures.saturating_add(1);
        let draw = self.jitter.draw();
        let delay = backoff_delay(self.account.store_failures, self.timing.max_backoff, draw);
        let until = deadline_after(now, delay);
        match self.account.wait {
            Some((current, _)) if current >= until => {}
            _ => self.account.wait = Some((until, WaitReason::Store)),
        }
        if first {
            tracing::info!(
                target: "collector.decision",
                vehicle = "account",
                from = "collecting",
                to = "store_wait",
                reason = "store_failure",
                class = "store",
                next_in_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                "collector transition"
            );
        } else {
            tracing::debug!(
                target: "collector.decision",
                vehicle = "account",
                reason = "store_failure",
                class = "store",
                failures = self.account.store_failures,
                "collector commit failed again"
            );
        }
    }

    // ---- Lifecycle views and observations (C3 R14, R40) --------------------------

    /// Store a car's freshly read lifecycle snapshot (C3 R14, R40): its view, the
    /// state of its current discovery row and its latest stored observation time.
    fn set_lifecycle_snapshot(
        &mut self,
        id: VehicleId,
        snapshot: &crate::db::CollectorLifecycleSnapshot,
    ) {
        self.set_lifecycle_view(
            id,
            LifecycleView {
                last_reach: snapshot.last_reach.clone(),
                drive_open: snapshot.drive_open,
            },
            snapshot.last_stored_obs_ms,
        );
        if let Some(vehicle) = self.vehicles.get_mut(&id) {
            vehicle.current_discovery = Some(snapshot.current_discovery_state.clone());
        }
    }

    /// The cached lifecycle facts R14 compares a discovery with, when they are
    /// known and current: the lifecycle's last reachability and the state of the
    /// car's current discovery row. `None` when the view is missing or stale, and
    /// the tick transaction then reads it (C3 R14).
    fn cached_discovery_view(&self, id: VehicleId) -> Option<(Option<String>, Option<String>)> {
        let vehicle = self.vehicles.get(&id)?;
        if vehicle.lifecycle_stale {
            return None;
        }
        let view = vehicle.lifecycle.as_ref()?;
        let current = vehicle.current_discovery.as_ref()?;
        Some((view.last_reach.clone(), current.clone()))
    }

    /// The collector committed an observation for the car (C3 R14, R40): its
    /// latest stored observation time advances, `force_closed_at_gap` clears, and
    /// its lifecycle view is stale.
    fn note_observation_stored(&mut self, id: VehicleId, received_at_ms: i64) {
        if let Some(vehicle) = self.vehicles.get_mut(&id) {
            vehicle.last_stored_obs_ms = Some(
                vehicle
                    .last_stored_obs_ms
                    .map_or(received_at_ms, |stored| stored.max(received_at_ms)),
            );
            vehicle.force_closed_at_gap = false;
            vehicle.lifecycle_stale = true;
        }
    }

    /// The count of alert gaps not yet shown to the owner, as the loop last read
    /// it from the store (C3 R13).
    fn set_alert_gap_count(&mut self, unnotified: usize) {
        self.alert_gaps_unnotified = unnotified;
    }
}
