// SPDX-License-Identifier: AGPL-3.0-only

// Sleep, suspend and stream evidence (C3 part K4): the idle window and the
// suspension (R31 to R33), the evidence before a read after the car reports online
// (R34, R35), stream evidence and the power gate calls (R36, R37), the stream's
// start and stop rule and run spacing (R28), the install that never ends (R41)
// and the stream-only status note (R54). The Hub never wakes a car (R38): no code
// here sends anything; each rule only decides whether and when a read is due.
//
// The scheduler core (`scheduler.rs`) calls into this file from `vehicle_succeeded`,
// `vehicle_failed_for_error`, `apply_reported_state` and `due_vehicles`; it is the
// same `impl VehicleScheduler`, in its own file so neither grows past the layout
// gate. Both are included into the collector module.

// ---------------------------------------------------------------------------
// Constants. Every value carries its reason and the requirement that gives it.
// ---------------------------------------------------------------------------

/// How long after a wake read, with no healthy stream, the Hub keeps reading before
/// it lets the car go to sleep again (C3 R31, `K_WAKE_WATCH`, **provisional**, gate
/// M-S3). It covers the time from a phone-key wake to shifting out of Park: a
/// healthy stream would reveal that use without reads, but without one the only way
/// to catch the drive start is to look, and five minutes is short enough to cost
/// little battery.
const K_WAKE_WATCH: Duration = Duration::from_secs(5 * 60);

/// How long an install may stay silent before the car is treated as offline (C3
/// R41, `K_UPDATE_STALE`). Tesla's example install lasts 2,700 s; two hours is
/// about 2.7 times that, so a longer silence is more likely a car that lost
/// connectivity than an install, and reads to it are billed 408s in Fleet polling.
const K_UPDATE_STALE: Duration = Duration::from_secs(2 * 60 * 60);

/// The size of a stream frame's `power` (kW, an integer, sign convention not
/// publicly documented, so the magnitude is used) at or above which a parked frame
/// is evidence of use (C3 R36, `K_STREAM_POWER_KW`). A parked awake car's
/// electronics alone round to 0 kW, while charging at 1.44 kW or more (a 120 V,
/// 12 A supply) or climate shows at least 1, and one read then classifies charging
/// versus climate. C4's power gate confirms on the same criterion (C4 R50), so it
/// reads this constant.
pub(crate) const K_STREAM_POWER_KW: i64 = 1;

/// The first spacing after a stream run that ended with an error (C3 R28,
/// `K_STREAM_RESTART_BASE`; C4 R56 a2). An error that ends a run (a stalled
/// consumer, a store fault) is not cured by an immediate retry, and a new run
/// starts with C4's counters at 0, so without spacing a fault loop would bypass
/// C4's own pacing. 30 s is far above C4's 1 s floor between attempts.
const K_STREAM_RESTART_BASE: Duration = Duration::from_secs(30);

/// The longest spacing after error-ended stream runs (C3 R28,
/// `K_STREAM_RESTART_MAX`; C4 R56 a2): it equals C4 R34's Transport cap, so a
/// persistent fault costs no more than a persistent network failure.
const K_STREAM_RESTART_MAX: Duration = Duration::from_secs(300);

/// The least capacity of the stream event channel (C3 R28, `K_STREAM_EVENT_CAPACITY`;
/// C4 R49, R56 g). C4 waits 10 s for space before it ends a run, and the intake
/// target lets the collector keep up with about four events a second, so 40 events
/// hold 10 s of that rate; a faster stream would outrun the consumer whatever the
/// capacity, and C4 then ends the run loudly.
const K_STREAM_EVENT_CAPACITY: usize = 40;

/// The status note of a paused car whose stream still delivers (C3 R54).
const STATUS_STREAM_ONLY: &str = "partial: stream only";

/// The status note of a car whose stream receipts cannot be written (C3 R28, C4
/// R56 e).
const STATUS_STREAM_AUDIT_FAILED: &str = "stream receipts cannot be written";

// ---------------------------------------------------------------------------
// Vocabulary.
// ---------------------------------------------------------------------------

/// What made a read due while the car was Suspended, so that the read is gated on
/// the live stream (C3 R36). Motion beats power: motion also moves the car to
/// Driving when the read is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GatedEvidence {
    Motion,
    Power,
}

/// Where evidence came from: the legacy stream (R36) or a telemetry push (R37).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvidenceSource {
    Stream,
    #[allow(dead_code)] // R37: no caller until a push source calls `telemetry_evidence`.
    Telemetry,
}

impl EvidenceSource {
    /// The reason code of a phase change this evidence causes (C3 section 7.4).
    const fn reason(self) -> &'static str {
        match self {
            Self::Stream => "use_evidence_stream",
            Self::Telemetry => "use_evidence_telemetry",
        }
    }
}

/// What one frame or telemetry message says, in the terms R36 acts on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FrameEvidence {
    /// The gear is D, R or N, or there is no gear and the car is moving.
    motion: bool,
    /// The gear is P (after Driving, a read captures the parked state).
    parked_gear: bool,
    /// A parked frame whose power (or a charging state) says energy is flowing:
    /// one read then classifies charging versus climate.
    wants_read: bool,
}

impl FrameEvidence {
    /// A legacy stream frame (C3 R36): motion is gear D, R or N after C1 R40a
    /// normalisation, or an empty gear with `speed > 0`; a parked frame (everything
    /// else, so gear P whatever its speed, as C1 R40 reads Park) shows use when
    /// `|power| >= K_STREAM_POWER_KW`.
    fn from_stream(shift_state: Option<&str>, speed: Option<f64>, power: Option<i64>) -> Self {
        let gear = shift_state
            .and_then(crate::lifecycle::normalise_enum_text)
            .filter(|name| matches!(*name, "P" | "R" | "N" | "D"));
        let motion = match gear {
            Some("D" | "R" | "N") => true,
            Some(_) => false,
            None => speed.is_some_and(|speed| speed > 0.0),
        };
        Self {
            motion,
            parked_gear: gear == Some("P"),
            wants_read: !motion
                && power
                    .is_some_and(|power| power.unsigned_abs() >= K_STREAM_POWER_KW.unsigned_abs()),
        }
    }

    /// A telemetry message (C3 R37): the same rules, with the gear alone deciding
    /// motion (D, R or N after C1 R40a) and a charging state of `Charging` or
    /// `Starting` in place of the stream's power.
    #[allow(dead_code)] // R37: no caller until a push source calls `telemetry_evidence`.
    fn from_telemetry(gear: Option<&str>, charging_state: Option<&str>) -> Self {
        let gear = gear
            .and_then(crate::lifecycle::normalise_enum_text)
            .filter(|name| matches!(*name, "P" | "R" | "N" | "D"));
        let motion = matches!(gear, Some("D" | "R" | "N"));
        let charging = charging_state.and_then(crate::lifecycle::normalise_enum_text);
        Self {
            motion,
            parked_gear: gear == Some("P"),
            wants_read: !motion && matches!(charging, Some("Charging" | "Starting")),
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers.
// ---------------------------------------------------------------------------

/// Whether an error is the gate's refusal of a gated read (C3 R36): the live
/// stream no longer proves the car awake, so no request was sent.
fn is_stream_power_not_confirmed(error: &CollectorError) -> bool {
    matches!(
        error,
        CollectorError::OwnerApi(OwnerApiError::StreamPowerNotConfirmed)
            | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
                OwnerApiError::StreamPowerNotConfirmed
            ))
    )
}

/// Withdraw every read that was due because the car was thought awake (C3 R38): a
/// car reported asleep or offline is read on no ground. A read already in flight
/// still reports back.
fn clear_pending_reads(vehicle: &mut ScheduledVehicle) {
    vehicle.next_read_due = None;
    vehicle.wake_read_pending = false;
    vehicle.gated_read = None;
    vehicle.suspension_read = false;
}

/// The idle window began (C3 R31). It is not a transition of R10 (the car's phase
/// does not change and nothing is stored), so it logs at DEBUG, with the code of
/// section 7.4.
fn log_idle_window(vehicle: &ScheduledVehicle, now: Instant) {
    let hub_id = vehicle
        .hub_id
        .map_or_else(|| "unmapped".to_owned(), |id| id.to_string());
    let suspend_in_ms = vehicle
        .suspend_at
        .map(|at| u64::try_from(at.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX));
    tracing::debug!(
        target: "collector.decision",
        vehicle = %hub_id,
        reason = "idle_window_started",
        suspend_in_ms,
        "collector idle window"
    );
}

/// Put a car into Suspended until `until` (C3 R31, R33, R35): no timed read, the
/// idle window cleared. A car that is already Suspended gets a new period, which is
/// not a transition of R10 (the phase is unchanged) and so logs at DEBUG; a car
/// that was not Suspended changes phase and logs once at INFO.
fn begin_suspension(
    timing: &Timing,
    vehicle: &mut ScheduledVehicle,
    now: Instant,
    wall_ms: i64,
    until: Instant,
    reason: &'static str,
    gated: bool,
) {
    let changed = enter_phase_gated(
        timing,
        vehicle,
        PollPhase::Suspended,
        now,
        wall_ms,
        reason,
        gated,
    );
    vehicle.suspended_until = Some(until);
    vehicle.suspend_at = None;
    vehicle.next_read_due = None;
    vehicle.gated_read = None;
    if !changed {
        let hub_id = vehicle
            .hub_id
            .map_or_else(|| "unmapped".to_owned(), |id| id.to_string());
        tracing::debug!(
            target: "collector.decision",
            vehicle = %hub_id,
            reason,
            "collector suspension period renewed"
        );
    }
}

/// A list or record says `online` for a car that was Asleep or Offline (C3 R34,
/// R35). Returns whether the car's stream power gate must be revoked (it entered
/// Suspended).
///
/// - Offline after a session that the offline report interrupted
///   (`offline_from_session`): `session_return`, one read at once and without the
///   R35 limit, so a car returning from a dead zone mid-drive is read within `T_gap`
///   and C1 can bridge the drive (C1 R45).
/// - Otherwise `wake_read`: one read at the next tick, allowed when the last wake
///   read is absent or at least `suspend_min` old. The car woke on its own (owner,
///   schedule, phone key), its own sleep timer restarted at that wake, so a read
///   after the detection delay extends the wake by at most that delay, while it
///   tells the Hub at once whether a drive or charge has begun.
/// - A wake read inside `suspend_min` of the last one is not made: the car goes to
///   Suspended until `last_wake_read + suspend_min` (`wake_read_limited`). A car
///   that flaps between asleep and online every few minutes would otherwise get one
///   read per flap; this bounds wake reads to one per `suspend_min` per car, and
///   with it their Fleet cost.
fn online_after_sleep(
    timing: &Timing,
    vehicle: &mut ScheduledVehicle,
    now: Instant,
    wall_ms: i64,
) -> bool {
    if vehicle.phase == PollPhase::Offline && vehicle.offline_from_session {
        vehicle.next_read_due = Some(now);
        vehicle.reason = "session_return";
        log_vehicle_transition(
            vehicle,
            PollPhase::Offline.code(),
            PollPhase::Offline.code(),
            "session_return",
            now,
        );
        return false;
    }
    let suspend = vehicle.settings.suspend;
    let allowed = vehicle
        .last_wake_read
        .is_none_or(|last| now.saturating_duration_since(last) >= suspend);
    if allowed {
        vehicle.last_wake_read = Some(now);
        vehicle.wake_read_pending = true;
        vehicle.next_read_due = Some(now);
        vehicle.reason = "wake_read";
        log_vehicle_transition(
            vehicle,
            vehicle.phase.code(),
            vehicle.phase.code(),
            "wake_read",
            now,
        );
        return false;
    }
    let until = vehicle
        .last_wake_read
        .map_or(now, |last| deadline_after(last, suspend));
    begin_suspension(
        timing,
        vehicle,
        now,
        wall_ms,
        until,
        "wake_read_limited",
        false,
    );
    true
}

// ---------------------------------------------------------------------------
// The scheduler's sleep, suspend and stream rules.
// ---------------------------------------------------------------------------

impl ScheduledVehicle {
    /// When a Driving car's connected stream counts as silent (C3 R28, CT50): at
    /// `K_DRIVE_FRAME_SILENCE` since its last frame. `None` unless that outage is
    /// still to be reported.
    fn silence_deadline(&self) -> Option<Instant> {
        if self.phase != PollPhase::Driving || !self.stream.connected || self.stream.outage_reported
        {
            return None;
        }
        self.stream
            .last_frame
            .map(|frame| deadline_after(frame, K_DRIVE_FRAME_SILENCE))
    }

    /// When an install that has had no successful read goes stale (C3 R41): the
    /// later of the install's start and its last successful read, plus
    /// `K_UPDATE_STALE`.
    fn update_stale_deadline(&self) -> Option<Instant> {
        if self.phase != PollPhase::Updating || self.hold != VehicleHold::None {
            return None;
        }
        let anchor = self
            .last_success
            .map_or(self.phase_since.0, |(read, _)| read.max(self.phase_since.0));
        Some(deadline_after(anchor, K_UPDATE_STALE))
    }

    /// The note the status shows for the car's data sources (C3 R28, R54): a paused
    /// car whose stream still delivers is "partial: stream only"; a car whose last
    /// stream run ended with an audit failure says its receipts cannot be written.
    fn stream_detail(&self, now: Instant) -> Option<&'static str> {
        if self.stream.audit_failed {
            Some(STATUS_STREAM_AUDIT_FAILED)
        } else if matches!(self.hold, VehicleHold::Paused(_))
            && self.stream_healthy_for_cadence(now)
        {
            Some(STATUS_STREAM_ONLY)
        } else {
            None
        }
    }
}

impl VehicleScheduler {
    // ---- Timers (C3 R31, R33, R41) ---------------------------------------------

    /// Run the timers that change a car's phase without a request. Called at every
    /// tick, before the due reads are chosen.
    ///
    /// - Online at `suspend_at`: Suspended until `now + suspend_min` (R31,
    ///   `suspended_idle`), the stream power gate revoked.
    /// - Suspended at `suspended_until`: if Tesla still reports the car online, one
    ///   read (R33, `suspension_ended_still_online`, not gated). Reason: if
    ///   `suspend_min` exceeds the time a car needs without requests to fall asleep,
    ///   a car still awake at the end is being kept awake by something else (sentry,
    ///   climate keeper, the owner's app), so the read costs no sleep; and a REST-only
    ///   Hub must look at least once per suspension, or a drive by a car that never
    ///   slept would be missed entirely. A car not reported online starts a new
    ///   period instead (nothing is read on a state that is not `online`, R38).
    /// - Updating with no successful read for `K_UPDATE_STALE`: Offline
    ///   (`update_stale`, R41): reads stop and the state checks continue.
    ///
    /// Only cars with no hold are timed: a held car is not read (R51, R52).
    fn advance_timers(&mut self, now: Instant) {
        let wall_ms = self.wall_or_last();
        let timing = self.timing;
        let mut revoke = Vec::new();
        for (id, vehicle) in &mut self.vehicles {
            if vehicle.hold != VehicleHold::None {
                continue;
            }
            match vehicle.phase {
                PollPhase::Online => {
                    if vehicle.suspend_at.is_some_and(|at| at <= now) {
                        let until = deadline_after(now, vehicle.settings.suspend);
                        begin_suspension(
                            &timing,
                            vehicle,
                            now,
                            wall_ms,
                            until,
                            "suspended_idle",
                            false,
                        );
                        revoke.push(*id);
                    }
                }
                PollPhase::Suspended => {
                    if vehicle.suspended_until.is_some_and(|at| at <= now) {
                        if vehicle.reported_state.as_deref() == Some("online") {
                            vehicle.suspended_until = None;
                            vehicle.suspension_read = true;
                            vehicle.gated_read = None;
                            vehicle.next_read_due = Some(now);
                            vehicle.reason = "suspension_ended_still_online";
                            log_vehicle_transition(
                                vehicle,
                                PollPhase::Suspended.code(),
                                PollPhase::Suspended.code(),
                                "suspension_ended_still_online",
                                now,
                            );
                        } else {
                            let until = deadline_after(now, vehicle.settings.suspend);
                            begin_suspension(
                                &timing,
                                vehicle,
                                now,
                                wall_ms,
                                until,
                                "suspended_idle",
                                false,
                            );
                            revoke.push(*id);
                        }
                    }
                }
                PollPhase::Updating
                    if vehicle
                        .update_stale_deadline()
                        .is_some_and(|stale| stale <= now) =>
                {
                    enter_phase(
                        &timing,
                        vehicle,
                        PollPhase::Offline,
                        now,
                        wall_ms,
                        "update_stale",
                    );
                    clear_pending_reads(vehicle);
                }
                _ => {}
            }
        }
        self.gate_revocations.extend(revoke);
    }

    /// A gated read was refused by the stream power gate at send time (C3 R36):
    /// the live stream no longer proves the car awake. No request was sent, no
    /// failure is counted and the car stays Suspended with no timed read; stream
    /// evidence still triggers a gated read when it comes again.
    fn gate_not_confirmed(&mut self, id: VehicleId) {
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return;
        };
        vehicle.in_flight = false;
        vehicle.gated_read = None;
        if vehicle.phase == PollPhase::Suspended && !vehicle.suspension_read {
            vehicle.next_read_due = None;
        }
        let hub_id = vehicle
            .hub_id
            .map_or_else(|| "unmapped".to_owned(), |id| id.to_string());
        tracing::debug!(
            target: "collector.decision",
            vehicle = %hub_id,
            reason = "use_evidence_stream",
            gated = true,
            class = "GateNotConfirmed",
            "gated read not sent: the stream no longer proves the car awake"
        );
    }

    // ---- Stream evidence (C3 R36, R37) ----------------------------------------

    /// A stream frame as evidence (C3 R36): see [`Self::apply_frame_evidence`].
    /// `shift_state`, `speed` and `power` are the frame's own values.
    fn schedule_stream_evidence(
        &mut self,
        id: VehicleId,
        shift_state: Option<&str>,
        speed: Option<f64>,
        power: Option<i64>,
        now: Instant,
    ) {
        self.apply_frame_evidence(
            id,
            FrameEvidence::from_stream(shift_state, speed, power),
            EvidenceSource::Stream,
            now,
        );
    }

    /// A stream frame as evidence when its speed is not known (C3 R3a): the gear
    /// and power rules of R36. The collector calls `schedule_stream_evidence`, which
    /// also reads the speed; this name is the interface of R3a, kept for callers
    /// that have no speed.
    #[cfg(test)]
    fn schedule_stream_charging_poll(
        &mut self,
        id: VehicleId,
        shift_state: Option<&str>,
        power: Option<i64>,
        now: Instant,
    ) {
        self.schedule_stream_evidence(id, shift_state, None, power, now);
    }

    /// Telemetry (a push source other than the stream) as evidence (C3 R37): the
    /// same rules as the stream (gear D, R or N after C1 R40a is motion; a charging
    /// state of `Charging` or `Starting` makes a read due). It has no caller until
    /// a Fleet Telemetry or provider-stream path calls it (IF a6); one rule serves
    /// every push source.
    #[allow(dead_code)] // R37: no caller until a push source calls it.
    fn telemetry_evidence(
        &mut self,
        id: VehicleId,
        gear: Option<&str>,
        charging_state: Option<&str>,
        now: Instant,
    ) {
        self.apply_frame_evidence(
            id,
            FrameEvidence::from_telemetry(gear, charging_state),
            EvidenceSource::Telemetry,
            now,
        );
    }

    /// Act on what a frame shows (C3 R36). A car that is Disabled or Removed is not
    /// collected, so its frames decide nothing. Reads are made due only for a car
    /// with no hold (a held car's frames are stored but not read, R51, R52).
    ///
    /// - Motion: `last_in_use` is now. From Driving nothing changes (the long
    ///   stream interval holds). From Suspended a read is due and **gated** on the
    ///   live stream; the car becomes Driving when that read is sent (see
    ///   `vehicle_failed_for_error` and `vehicle_succeeded`), and stays Suspended if
    ///   the gate is not confirmed. From any other phase the car becomes Driving at
    ///   once, with a read due; a held car keeps its hold.
    /// - Gear P while Driving: a read is due, which captures the parked state (C1
    ///   closes the drive on the stream's own P frame).
    /// - A parked frame with `|power| >= K_STREAM_POWER_KW`: a read is due, at most
    ///   one per Online interval, gated if Suspended. The Driving, Charging and
    ///   Updating phases are already read on their own cadence and change nothing.
    ///
    /// A read made due this way is not faster than `K_MIN_READ_SPACING` after the
    /// last read (R22): evidence never makes the Hub faster than its fastest
    /// configured cadence.
    fn apply_frame_evidence(
        &mut self,
        id: VehicleId,
        evidence: FrameEvidence,
        source: EvidenceSource,
        now: Instant,
    ) {
        let wall_ms = self.wall_or_last();
        let timing = self.timing;
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return;
        };
        if vehicle.is_inactive() {
            return;
        }
        let reads_allowed = vehicle.hold == VehicleHold::None;
        let make_read_due = |vehicle: &mut ScheduledVehicle| {
            let at = Self::evidence_read_time(&timing, vehicle, now);
            vehicle.next_read_due = Some(vehicle.next_read_due.map_or(at, |due| due.min(at)));
        };
        if evidence.motion {
            vehicle.last_in_use = Some(now);
            match vehicle.phase {
                PollPhase::Driving => {}
                PollPhase::Suspended => {
                    if reads_allowed {
                        vehicle.gated_read = Some(GatedEvidence::Motion);
                        make_read_due(vehicle);
                    }
                }
                _ => {
                    enter_phase(
                        &timing,
                        vehicle,
                        PollPhase::Driving,
                        now,
                        wall_ms,
                        source.reason(),
                    );
                    if reads_allowed {
                        make_read_due(vehicle);
                    }
                }
            }
            return;
        }
        if !reads_allowed {
            return;
        }
        if evidence.parked_gear && vehicle.phase == PollPhase::Driving {
            make_read_due(vehicle);
            return;
        }
        if evidence.wants_read
            && matches!(
                vehicle.phase,
                PollPhase::Unknown
                    | PollPhase::Asleep
                    | PollPhase::Offline
                    | PollPhase::Online
                    | PollPhase::Suspended
            )
        {
            let spaced = vehicle
                .last_power_trigger
                .is_none_or(|last| now.saturating_duration_since(last) >= timing.online);
            if spaced {
                vehicle.last_power_trigger = Some(now);
                if vehicle.phase == PollPhase::Suspended {
                    vehicle.gated_read.get_or_insert(GatedEvidence::Power);
                }
                make_read_due(vehicle);
            }
        }
    }

    // ---- The power gate (C3 R36) ------------------------------------------------

    /// Whether the car's pending or in-flight read needs the live stream power
    /// gate (C3 R36): exactly when the read was made due by stream evidence while
    /// the car is Suspended. The loop then passes the car's gate to the read, which
    /// checks `is_confirmed()` at send time.
    fn requires_live_stream_power_gate(&self, id: VehicleId) -> bool {
        self.vehicles.get(&id).is_some_and(|vehicle| {
            vehicle.gated_read.is_some() && vehicle.phase == PollPhase::Suspended
        })
    }

    /// Record that the car's stream power gate must be revoked (C3 R36): on the
    /// dispatcher's events (`VehicleOffline`, `TransportUnavailable`,
    /// `ProtocolViolation`, `AuthRejected`) and on every transition into Suspended.
    /// The scheduler never holds a gate; the loop applies the revocations
    /// (`take_gate_revocations`). A car the scheduler does not know needs none.
    fn revoke_gate(&mut self, id: VehicleId) {
        if self.vehicles.contains_key(&id) {
            self.gate_revocations.insert(id);
        }
    }

    /// The cars whose gate must be revoked now, each once (C3 R36). Reason for
    /// revoking on entering Suspended: a suspension starts when reads have just
    /// shown no use, so a confirmation from a frame before that decision is
    /// evidence those reads already judged, and a gated read must rest on a frame
    /// that arrived after the suspension began. A continuing drive or charge
    /// re-confirms at its next frame, so the revoke costs at most one frame's
    /// delay.
    fn take_gate_revocations(&mut self) -> Vec<VehicleId> {
        std::mem::take(&mut self.gate_revocations)
            .into_iter()
            .collect()
    }

    // ---- Stream start, stop and run spacing (C3 R28) ----------------------------

    /// Whether the mode delivers a push stream (C3 R28): Legacy always, Provider
    /// only with a stream URL, never Fleet polling (R64).
    fn set_stream_delivery(&mut self, delivers: bool) {
        self.stream_delivery = delivers;
    }

    /// Whether a stream should run for the car (C3 R28): the mode delivers a stream,
    /// the car's `use_streaming_api` is on, its hold is `None`, `Paused` or `Service`
    /// (not Disabled or Removed), the account is Collecting (an account wait such as
    /// a 429 or an unreachable REST host does not stop the stream, which reaches
    /// another host), the car's stream restart time has passed, and Tesla last
    /// reported the car online: its phase is Online, Suspended, Driving, Charging or
    /// Updating, or, for a held car, its reported state is `online`. False for
    /// Asleep, Offline and Unknown.
    ///
    /// Reasons: frames come only from an awake, connected car (C1 R37), so
    /// subscribing for a sleeping car only produces reconnect traffic, and C4
    /// cannot tell asleep from briefly unreachable; a held car keeps its stream
    /// because its frames are still data (R36, R54); keeping the subscription
    /// during Suspended lets the stream reveal use without a read (gate M-S2
    /// checks that it does not delay sleep).
    fn should_start_stream(&self, id: VehicleId) -> bool {
        let Some(vehicle) = self.vehicles.get(&id) else {
            return false;
        };
        if !self.stream_delivery
            || !vehicle.settings.use_streaming_api
            || self.account.state != AccountState::Collecting
        {
            return false;
        }
        let now = self.clock.mono_now();
        if vehicle
            .stream
            .restart_not_before
            .is_some_and(|restart| now < restart)
        {
            return false;
        }
        match vehicle.hold {
            VehicleHold::None => matches!(
                vehicle.phase,
                PollPhase::Online
                    | PollPhase::Suspended
                    | PollPhase::Driving
                    | PollPhase::Charging
                    | PollPhase::Updating
            ),
            VehicleHold::Paused(_) | VehicleHold::Service => {
                vehicle.reported_state.as_deref() == Some("online")
            }
            VehicleHold::Disabled | VehicleHold::Removed => false,
        }
    }

    /// The cars for which `should_start_stream` turned false since the last call:
    /// the loop disconnects their streams (C3 R28; C4 R56 a, b). A car that stops
    /// being wanted for a reason of C3's own (asleep, offline, a setting) also loses
    /// its stream health and any outage: no outage is reported for a stream C3
    /// stopped itself. A car waiting out run spacing keeps both, so its recovery is
    /// still reported when a later run emits `Healthy`.
    fn streams_to_stop(&mut self) -> Vec<VehicleId> {
        let ids: Vec<VehicleId> = self.vehicles.keys().copied().collect();
        let now = self.clock.mono_now();
        let mut stop = Vec::new();
        for id in ids {
            let wanted = self.should_start_stream(id);
            let Some(vehicle) = self.vehicles.get_mut(&id) else {
                continue;
            };
            if vehicle.stream_wanted && !wanted {
                stop.push(id);
            }
            vehicle.stream_wanted = wanted;
            if !wanted {
                vehicle.stream.connected = false;
                let waiting = vehicle
                    .stream
                    .restart_not_before
                    .is_some_and(|restart| now < restart);
                if !waiting {
                    vehicle.stream.outage_reported = false;
                    vehicle.stream.outage_started = None;
                    vehicle.stream.outage_failures = 0;
                }
            }
        }
        stop
    }

    /// A stream run ended with an error (C3 R28 run spacing; C4 R12, R56 a2, e):
    /// the car's `task_failures` counts it, and the next run is not started before
    /// `K_STREAM_RESTART_BASE x 2^min(task_failures - 1, 4)`, at most
    /// `K_STREAM_RESTART_MAX`: 30 s, 60 s, 120 s, 240 s, then 300 s, with no jitter.
    /// Called once per run, after its task has ended (C4 R56 a1: one run at a
    /// time). A run that emits `Healthy` resets the count (`stream_healthy`). A run
    /// C3 stopped itself is not an error-ended run: it counts nothing.
    fn stream_task_ended(&mut self, id: VehicleId, now: Instant) {
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return;
        };
        vehicle.stream.connected = false;
        vehicle.stream.task_failures = vehicle.stream.task_failures.saturating_add(1);
        let exponent = vehicle.stream.task_failures.saturating_sub(1).min(4);
        let delay = K_STREAM_RESTART_BASE
            .saturating_mul(1u32 << exponent)
            .min(K_STREAM_RESTART_MAX);
        vehicle.stream.restart_not_before = Some(deadline_after(now, delay));
        vehicle.reason = "stream_restart_delayed";
        let hub_id = vehicle
            .hub_id
            .map_or_else(|| "unmapped".to_owned(), |id| id.to_string());
        tracing::info!(
            target: "collector.decision",
            vehicle = %hub_id,
            from = vehicle.phase.code(),
            to = vehicle.phase.code(),
            reason = "stream_restart_delayed",
            next_in_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            gated = false,
            "collector transition"
        );
    }

    /// A stream run ended with an audit failure (C3 R28, C4 R56 e): the car's status
    /// says the stream's receipts cannot be written, and readiness carries
    /// `collector_stream_audit_failed`, until a later run of the car emits `Healthy`.
    /// It lives in memory (and in the stored reason of the car's row, which
    /// `vehicle_view` derives from it), because the store that refused the receipt
    /// would refuse a dedicated write too.
    ///
    /// The first failure of a car, from false to true, is one WARN decision event, so
    /// the journal says so whether or not any durable row could be written (C3 R28,
    /// R36, B2 clarification 3). It names only the car's Hub id (`unmapped` before it
    /// has one), never a Tesla id, a token or an error text. A repeated failure logs
    /// nothing; a later `Healthy` clears the state and permits the next transition.
    /// Returns whether this call was that transition.
    fn stream_audit_failed(&mut self, id: VehicleId) -> bool {
        let label = self.hub_label(id);
        let Some(vehicle) = self.vehicles.get_mut(&id) else {
            return false;
        };
        let transition = !std::mem::replace(&mut vehicle.stream.audit_failed, true);
        if transition {
            tracing::warn!(
                target: "collector.decision",
                vehicle = %label,
                from = vehicle.phase.code(),
                to = vehicle.phase.code(),
                reason = "stream_audit_failed",
                gated = false,
                "collector transition"
            );
        }
        transition
    }

    /// How many collected cars show `stream_audit_failed` now (C3 R28): the
    /// in-process source of readiness's `collector_stream_audit_failed`.
    fn stream_audit_failed_count(&self) -> usize {
        self.vehicles
            .values()
            .filter(|vehicle| !vehicle.is_inactive() && vehicle.stream.audit_failed)
            .count()
    }

    /// The cars whose Driving stream has been silent for more than
    /// `K_DRIVE_FRAME_SILENCE` with no event (C3 R28): an outage for cadence, to be
    /// reported once through `stream_unhealthy`. 30 s without a frame while driving
    /// is 417 m at 50 km/h; switching to the 10 s read fallback then restores
    /// street-level spacing.
    fn silent_drive_streams(&self, now: Instant) -> Vec<VehicleId> {
        self.vehicles
            .iter()
            .filter(|(_, vehicle)| {
                !vehicle.is_inactive()
                    && vehicle.phase == PollPhase::Driving
                    && vehicle.stream.connected
                    && !vehicle.stream.outage_reported
                    && vehicle.stream.last_frame.is_some_and(|frame| {
                        now.saturating_duration_since(frame) >= K_DRIVE_FRAME_SILENCE
                    })
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// The time to the car's next action in milliseconds, for the decision log (C3
    /// R9), when it has one.
    fn next_action_in_ms(&self, id: VehicleId, now: Instant) -> Option<u64> {
        self.vehicles
            .get(&id)
            .and_then(ScheduledVehicle::next_due)
            .map(|due| {
                u64::try_from(due.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX)
            })
    }

    /// The decision-log label of a car: its Hub id, or `unmapped` before it has one
    /// (C3 R9: the log never carries a Tesla id).
    fn hub_label(&self, id: VehicleId) -> String {
        self.vehicles
            .get(&id)
            .and_then(|vehicle| vehicle.hub_id)
            .map_or_else(|| "unmapped".to_owned(), |hub| hub.to_string())
    }
}

// ---------------------------------------------------------------------------
// R40: closing a drive that ended while the car was unreachable.
// ---------------------------------------------------------------------------

impl VehicleScheduler {
    /// The cars whose lifecycle view the R40 check must read now (C3 R14, R40): a
    /// car without a view (the start load, R7 step 6), or a car whose view is stale
    /// and whose evidence is already older than `T_gap`, so that the view's open
    /// drive could now matter. A car whose evidence is recent needs no read: its
    /// next possible event is `T_gap` after that evidence, and the read waits for
    /// it. A sleeping Hub that stores nothing therefore reads nothing. A Disabled
    /// or Removed car is read like any other (R40, B2 clarification): the view is a
    /// store read and the event it may lead to is the Hub's own record, so R62's
    /// "no observations from Tesla" is not touched.
    fn lifecycle_views_for_no_data(&self, wall_now_ms: i64) -> Vec<VehicleId> {
        let gap_ms =
            i64::try_from(self.timing.offline_drive_timeout.as_millis()).unwrap_or(i64::MAX);
        self.vehicles
            .iter()
            .filter(|(_, vehicle)| vehicle.hub_id.is_some())
            .filter(|(_, vehicle)| match vehicle.lifecycle {
                None => true,
                Some(_) => {
                    vehicle.lifecycle_stale
                        && !vehicle.force_closed_at_gap
                        && vehicle
                            .last_stored_obs_ms
                            .is_some_and(|stored| wall_now_ms > stored.saturating_add(gap_ms))
                }
            })
            .map(|(id, _)| *id)
            .collect()
    }
}

/// Plan the Hub `no_data` events of this iteration (C3 R40; C1 R15, R43 item 4,
/// R44): for every collected car whose lifecycle says a drive is open and whose
/// latest stored observation (a read, a stream frame or a discovery, never a Hub
/// event) was received more than `T_gap` ago, one `hub_lifecycle_event_v1`
/// observation `{"event": "no_data"}` with `observed_at_ms` = the Hub wall time now
/// joins the tick's batch, to be stored in the tick transaction through the
/// ordinary intake. It applies in every phase, hold, account state (a cap, a
/// refusal, a credential stop) and account wait, and makes no Tesla request: an
/// account-scope stop leaves no evidence either, so the phase must not matter.
///
/// Reasons: C1 ends such a drive at its last fold when the next observation
/// arrives, so the event produces the same row and makes it visible without
/// waiting days for a car in a basement garage to return; as a stored observation
/// the decision is replayable (C1 R1, R15); `received_at_ms` is the Hub clock, the
/// domain of `wall_now`. The scheduler marks the car `force_closed_at_gap` only
/// when the transaction has committed, so a failed commit plans the event again;
/// the next stored observation other than a Hub event clears the mark. A charge is
/// not closed by `no_data` (C1 R15).
///
/// Production code never calls `force_close_open_sessions` (C1 R15).
fn plan_no_data_events(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    batch: &mut TickBatch,
) -> Result<(), CollectorError> {
    let wall_ms = scheduler.wall_or_last();
    for id in scheduler.lifecycle_views_for_no_data(wall_ms) {
        let Some(hub_id) = scheduler
            .vehicles
            .get(&id)
            .and_then(|vehicle| vehicle.hub_id)
        else {
            continue;
        };
        let snapshot = store.collector_lifecycle_snapshot(hub_id)?;
        scheduler.set_lifecycle_snapshot(id, &snapshot);
    }
    for id in scheduler.due_lifecycle_events(wall_ms) {
        let Some(vehicle) = scheduler.vehicles.get(&id) else {
            continue;
        };
        batch.hub_events.push(HubEventItem {
            vehicle: vehicle.vehicle.clone(),
            event: HUB_EVENT_NO_DATA,
            observed_at_ms: wall_ms,
        });
    }
    Ok(())
}
