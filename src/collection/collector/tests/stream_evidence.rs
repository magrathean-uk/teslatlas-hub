// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C3 part K4, stream evidence, the power gate calls, run spacing and
// the Hub `no_data` event: CT27, CT28, CT31, CT49, CT50, CT61 and CT71, plus tests
// of the rules the part owns (R28, R36, R37, R40, R54). Synthetic data only; the
// stream's events are built by hand and no test touches the network.

/// A stream frame (C4's `StreamUpdate`) with the values R36 reads.
fn frame(
    timestamp_ms: i64,
    shift: Option<&str>,
    speed: Option<i64>,
    power: Option<i64>,
) -> crate::tesla_stream::StreamUpdate {
    crate::tesla_stream::StreamUpdate {
        tag: "1".to_owned(),
        timestamp_ms,
        speed: speed.map(|mph| mph as f64),
        odometer: Some(100.0),
        soc: Some(80.0),
        elevation: None,
        est_heading: None,
        est_lat: Some(51.5),
        est_lng: Some(-0.1),
        power,
        shift_state: shift.map(str::to_owned),
        range: Some(200.0),
        est_range: Some(210.0),
        heading: None,
    }
}

/// A test double of C4's power gate (C4 R50): confirmed by a frame, revoked by C3,
/// and recording its callers. C3 never calls `observe` (C4 R56 i), so that count
/// stays 0; the real method is private to C4's module, so C3 could not call it.
#[derive(Default)]
struct GateDouble {
    confirmed: AtomicBool,
    revokes: AtomicUsize,
    observes: AtomicUsize,
}

impl GateDouble {
    /// C4's side: a frame showing motion or power confirms the gate.
    fn confirm(&self) {
        self.confirmed.store(true, Ordering::SeqCst);
    }

    fn revoke(&self) {
        self.confirmed.store(false, Ordering::SeqCst);
        self.revokes.fetch_add(1, Ordering::SeqCst);
    }

    fn is_confirmed(&self) -> bool {
        self.confirmed.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    fn observe(&self) {
        self.observes.fetch_add(1, Ordering::SeqCst);
    }
}

/// What the loop does with a gated read at send time (C3 R36): it is sent only if the
/// gate is confirmed; otherwise the request function answers
/// `StreamPowerNotConfirmed` and sends nothing.
fn send_gated_read(
    scheduler: &mut VehicleScheduler,
    gate: &GateDouble,
    id: VehicleId,
    now: Instant,
    on_success: PollPhase,
) -> bool {
    if scheduler.requires_live_stream_power_gate(id) && !gate.is_confirmed() {
        scheduler.vehicle_failed_for_error(
            id,
            &CollectorError::OwnerApi(OwnerApiError::StreamPowerNotConfirmed),
            now,
        );
        return false;
    }
    scheduler.vehicle_succeeded(id, on_success, false, now);
    true
}

// ---- CT27: stream evidence, gated ------------------------------------------------------

#[test]
fn ct27_a_motion_frame_while_suspended_makes_a_gated_read_that_moves_the_car() {
    let log = DecisionLog::start();
    let mut core = suspended_core();
    let id = core_id(1);
    let gate = GateDouble::default();
    let t2 = core.at(20 * 60);
    clock_to(&core, t2);
    // Shift D at t2: the frame confirms the gate (C4 R50) and makes a read due.
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(12.0), None, t2);
    gate.confirm();
    assert!(core.scheduler.requires_live_stream_power_gate(id));
    // The car stays Suspended until the read is sent.
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    assert_eq!(core.scheduler.due_vehicles(t2), vec![id]);
    assert!(send_gated_read(
        &mut core.scheduler,
        &gate,
        id,
        t2,
        PollPhase::Driving
    ));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Driving);
    assert_eq!(core.scheduler.vehicles[&id].reason, "use_evidence_stream");
    assert!(!core.scheduler.requires_live_stream_power_gate(id));
    // The decision event of the change carries `gated = true`, the reason and the
    // car's Hub id of the car, never a Tesla id.
    assert_eq!(log.lines("INFO", "gated=true"), 1, "{}", log.contents());
    assert_eq!(
        log.lines("INFO", "use_evidence_stream"),
        1,
        "{}",
        log.contents()
    );
}

#[test]
fn ct27_a_revoked_gate_sends_nothing_and_counts_no_failure() {
    let mut core = suspended_core();
    let id = core_id(1);
    let gate = GateDouble::default();
    let t2 = core.at(20 * 60);
    clock_to(&core, t2);
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(12.0), None, t2);
    gate.confirm();
    assert_eq!(core.scheduler.due_vehicles(t2), vec![id]);
    // The session ends (C4) before the read is sent: the gate is revoked.
    gate.revoke();
    assert!(!send_gated_read(
        &mut core.scheduler,
        &gate,
        id,
        t2,
        PollPhase::Driving
    ));
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Suspended);
    assert_eq!(record.consecutive_failures, 0);
    assert_eq!(record.last_error, None);
    assert!(!record.in_flight);
    assert!(
        core.scheduler
            .due_vehicles(t2 + Duration::from_secs(5))
            .is_empty()
    );
    // A later motion frame confirms again and its gated read is sent.
    let t3 = t2 + Duration::from_secs(20);
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(14.0), None, t3);
    gate.confirm();
    assert_eq!(core.scheduler.due_vehicles(t3), vec![id]);
    assert!(send_gated_read(
        &mut core.scheduler,
        &gate,
        id,
        t3,
        PollPhase::Driving
    ));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Driving);
    assert_eq!(
        gate.observes.load(Ordering::SeqCst),
        0,
        "C3 never calls observe"
    );
}

#[test]
fn ct27_entering_suspended_revokes_the_gate_and_a_later_frame_confirms_it_again() {
    let mut core = Core::owner();
    let id = core_id(1);
    let gate = GateDouble::default();
    first_read_parked(&mut core);
    for minute in 1..=14 {
        let t = minutes(&core, minute);
        core.scheduler.due_vehicles(t);
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
    }
    // The gate was confirmed at t3 - 5 s by a frame the reads have already judged.
    let t3 = minutes(&core, 15);
    gate.confirm();
    assert!(gate.is_confirmed());
    // At t3 the phase becomes Suspended, and the loop revokes the gate.
    core.scheduler.due_vehicles(t3);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    apply_gate_revocations_with(&mut core.scheduler, |revoked| {
        assert_eq!(revoked, id);
        gate.revoke();
    });
    assert!(!gate.is_confirmed());
    assert_eq!(gate.revokes.load(Ordering::SeqCst), 1);
    // Each revocation is returned once.
    apply_gate_revocations_with(&mut core.scheduler, |_| panic!("nothing left to revoke"));
    // A later motion frame confirms the gate again and its gated read is sent.
    let t4 = t3 + Duration::from_secs(60);
    clock_to(&core, t4);
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(9.0), None, t4);
    gate.confirm();
    assert_eq!(core.scheduler.due_vehicles(t4), vec![id]);
    assert!(send_gated_read(
        &mut core.scheduler,
        &gate,
        id,
        t4,
        PollPhase::Driving
    ));
    assert_eq!(gate.observes.load(Ordering::SeqCst), 0);
}

// ---- CT28: parked power (R36) -----------------------------------------------------------

#[test]
fn ct28_a_parked_power_frame_makes_at_most_one_gated_read_per_online_interval() {
    let mut core = suspended_core();
    let id = core_id(1);
    let t = core.at(20 * 60);
    clock_to(&core, t);
    // Power -7 kW: one gated read.
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(0.0), Some(-7), t);
    assert!(core.scheduler.requires_live_stream_power_gate(id));
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, t);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    // A second frame with power -7 kW 30 s later: no second read.
    let t30 = t + Duration::from_secs(30);
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(0.0), Some(-7), t30);
    assert!(!core.scheduler.requires_live_stream_power_gate(id));
    assert!(core.scheduler.due_vehicles(t30).is_empty());
    // A frame with power 0: nothing.
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(0.0), Some(0), t30);
    assert!(core.scheduler.due_vehicles(t30).is_empty());
    // After an Online interval (60 s) the same frame makes a read again.
    let t60 = t + Duration::from_secs(60);
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(0.0), Some(-7), t60);
    assert_eq!(core.scheduler.due_vehicles(t60), vec![id]);
}

#[test]
fn the_power_threshold_is_one_kilowatt_in_either_sign() {
    let cases: [(Option<i64>, bool); 7] = [
        (Some(0), false),
        (None, false),
        (Some(1), true),
        (Some(-1), true),
        (Some(7), true),
        (Some(-7), true),
        (Some(i64::MIN), true),
    ];
    for (power, makes_read) in cases {
        let mut core = suspended_core();
        let id = core_id(1);
        let t = core.at(20 * 60);
        core.scheduler
            .schedule_stream_evidence(id, Some("P"), Some(0.0), power, t);
        assert_eq!(
            core.scheduler.due_vehicles(t).len(),
            usize::from(makes_read),
            "power {power:?}"
        );
    }
    assert_eq!(K_STREAM_POWER_KW, 1);
}

#[test]
fn power_makes_a_read_from_an_online_car_but_changes_nothing_in_a_session() {
    // Online: a read is due, within the minimum read spacing of the last read.
    let mut online = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut online);
    let t = online.at(5);
    online
        .scheduler
        .schedule_stream_evidence(id, None, None, Some(3), t);
    // `K_MIN_READ_SPACING` is 10 s after the read at t0; the Online interval would
    // have been 60 s.
    assert_eq!(
        online.scheduler.vehicles[&id].next_read_due,
        Some(online.at(10))
    );
    // Not gated: the car is not Suspended.
    assert!(!online.scheduler.requires_live_stream_power_gate(id));
    // Charging: the cadence already covers it.
    let mut charging = Core::owner();
    charging
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], charging.t0);
    charging.scheduler.due_vehicles(charging.t0);
    charging
        .scheduler
        .vehicle_succeeded(id, PollPhase::Charging, false, charging.t0);
    let before = charging.scheduler.vehicles[&id].next_read_due;
    charging
        .scheduler
        .schedule_stream_evidence(id, None, None, Some(11), charging.at(5));
    assert_eq!(charging.scheduler.vehicles[&id].next_read_due, before);
    assert_eq!(charging.scheduler.vehicles[&id].phase, PollPhase::Charging);
}

#[test]
fn a_parked_gear_frame_after_driving_makes_a_read_due() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler.stream_healthy(id, core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, core.t0);
    // With a healthy stream the Driving read is 60 s away.
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(60))
    );
    // Gear P with no power at +20 s: the read that captures the parked state is due
    // `K_MIN_READ_SPACING` after the last read, not 60 s.
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(0.0), Some(0), core.at(20));
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(20))
    );
    // An empty gear is not a parked statement.
    let mut empty = Core::owner();
    empty
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], empty.t0);
    empty.scheduler.due_vehicles(empty.t0);
    empty.scheduler.stream_healthy(id, empty.t0);
    empty
        .scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, empty.t0);
    empty
        .scheduler
        .schedule_stream_evidence(id, None, Some(0.0), Some(0), empty.at(20));
    assert_eq!(
        empty.scheduler.vehicles[&id].next_read_due,
        Some(empty.at(60))
    );
}

#[test]
fn motion_by_speed_alone_counts_only_without_a_gear() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    // Gear P with a speed (a bogus frame) is not motion.
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(5.0), None, core.at(30));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Online);
    // No gear and a positive speed is motion.
    core.scheduler
        .schedule_stream_evidence(id, None, Some(5.0), None, core.at(31));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Driving);
    assert_eq!(core.scheduler.vehicles[&id].last_in_use, Some(core.at(31)));
    // A gear word that is not a gear leaves the speed to decide.
    let mut other = Core::owner();
    first_read_parked(&mut other);
    other
        .scheduler
        .schedule_stream_evidence(id, Some("SNA"), Some(5.0), None, other.at(30));
    assert_eq!(other.scheduler.vehicles[&id].phase, PollPhase::Driving);
}

#[test]
fn stream_motion_from_an_unreachable_car_makes_it_driving_with_a_read() {
    // R36: frames come only from an awake car, so a frame from a car the scheduler
    // thought Asleep is evidence it is not.
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(20.0), None, core.at(10));
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Driving);
    assert_eq!(record.reason, "use_evidence_stream");
    assert!(record.next_read_due.is_some());
    assert!(!core.scheduler.requires_live_stream_power_gate(id));
}

#[test]
fn a_held_car_keeps_its_hold_and_gets_no_read_from_a_frame() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    core.scheduler.vehicles.get_mut(&id).unwrap().hold =
        VehicleHold::Paused(PauseReason::PersistentFailure);
    core.scheduler.vehicles.get_mut(&id).unwrap().next_read_due = None;
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(20.0), None, core.at(30));
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(
        record.hold,
        VehicleHold::Paused(PauseReason::PersistentFailure)
    );
    assert_eq!(
        record.phase,
        PollPhase::Driving,
        "the frame says the car moves"
    );
    assert!(core.scheduler.due_vehicles(core.at(300)).is_empty());
    // A Disabled car decides nothing.
    let mut disabled = Core::owner();
    let mut vehicle = core_vehicle(1, "online");
    vehicle.settings.enabled = false;
    disabled
        .scheduler
        .accept_discovery(vec![vehicle], disabled.t0);
    disabled
        .scheduler
        .schedule_stream_evidence(id, Some("D"), Some(20.0), None, disabled.at(30));
    assert_eq!(disabled.scheduler.vehicles[&id].phase, PollPhase::Unknown);
    assert_eq!(disabled.scheduler.vehicles[&id].last_in_use, None);
}

// ---- R37: telemetry evidence ----------------------------------------------------------------

#[test]
fn telemetry_evidence_applies_the_same_rules_as_the_stream() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    // Charging makes a read due, within the spacing, from an Online car.
    core.scheduler.telemetry_evidence(
        id,
        Some("P"),
        Some("DetailedChargeStateCharging"),
        core.at(30),
    );
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(30))
    );
    // Gear D after C1 R40a normalisation is motion.
    let mut driving = Core::owner();
    first_read_parked(&mut driving);
    driving
        .scheduler
        .telemetry_evidence(id, Some("ShiftStateD"), None, driving.at(30));
    assert_eq!(driving.scheduler.vehicles[&id].phase, PollPhase::Driving);
    assert_eq!(
        driving.scheduler.vehicles[&id].reason,
        "use_evidence_telemetry"
    );
    // A parked, idle message makes nothing.
    let mut idle = Core::owner();
    first_read_parked(&mut idle);
    let before = idle.scheduler.vehicles[&id].next_read_due;
    idle.scheduler
        .telemetry_evidence(id, Some("P"), Some("Disconnected"), idle.at(30));
    assert_eq!(idle.scheduler.vehicles[&id].next_read_due, before);
}

// ---- R28: when a stream runs --------------------------------------------------------------------

/// Car 1 online with a first read, in the given mode.
fn stream_core(mode: crate::config::CollectorMode) -> Core {
    let mut core = Core::new(mode);
    first_read_parked(&mut core);
    core
}

#[test]
fn should_start_stream_follows_mode_setting_hold_account_and_phase() {
    let id = core_id(1);
    // Legacy, Online, streaming on: yes.
    let core = stream_core(crate::config::CollectorMode::Legacy);
    assert!(core.scheduler.should_start_stream(id));
    // The car's own setting.
    let mut off = stream_core(crate::config::CollectorMode::Legacy);
    off.scheduler
        .vehicles
        .get_mut(&id)
        .unwrap()
        .settings
        .use_streaming_api = false;
    assert!(!off.scheduler.should_start_stream(id));
    // Provider: only with a stream URL (the loop says so).
    let mut provider = stream_core(crate::config::CollectorMode::Provider);
    assert!(!provider.scheduler.should_start_stream(id));
    provider.scheduler.set_stream_delivery(true);
    assert!(provider.scheduler.should_start_stream(id));
    // Fleet polling has no stream (R64).
    let fleet = stream_core(crate::config::CollectorMode::FleetPolling);
    assert!(!fleet.scheduler.should_start_stream(id));
    // An unknown car.
    assert!(!core.scheduler.should_start_stream(core_id(9)));
    // Phases: awake phases yes, Asleep, Offline and Unknown no.
    for (phase, expected) in [
        (PollPhase::Online, true),
        (PollPhase::Suspended, true),
        (PollPhase::Driving, true),
        (PollPhase::Charging, true),
        (PollPhase::Updating, true),
        (PollPhase::Asleep, false),
        (PollPhase::Offline, false),
        (PollPhase::Unknown, false),
    ] {
        let mut core = stream_core(crate::config::CollectorMode::Legacy);
        core.scheduler.vehicles.get_mut(&id).unwrap().phase = phase;
        assert_eq!(
            core.scheduler.should_start_stream(id),
            expected,
            "{phase:?}"
        );
    }
    // Holds: Disabled and Removed no; Paused and Service yes while Tesla listed the
    // car online, whatever its phase.
    for (hold, reported, expected) in [
        (VehicleHold::Disabled, "online", false),
        (VehicleHold::Removed, "online", false),
        (VehicleHold::Service, "online", true),
        (
            VehicleHold::Paused(PauseReason::PersistentFailure),
            "online",
            true,
        ),
        (
            VehicleHold::Paused(PauseReason::AccessRefused),
            "online",
            true,
        ),
        (VehicleHold::Service, "asleep", false),
        (
            VehicleHold::Paused(PauseReason::LegalPrivacy),
            "offline",
            false,
        ),
    ] {
        let mut core = stream_core(crate::config::CollectorMode::Legacy);
        let record = core.scheduler.vehicles.get_mut(&id).unwrap();
        record.hold = hold;
        record.reported_state = Some(reported.to_owned());
        assert_eq!(
            core.scheduler.should_start_stream(id),
            expected,
            "{hold:?} {reported}"
        );
    }
}

#[test]
fn an_account_wait_keeps_the_stream_but_a_stopped_account_ends_it() {
    let id = core_id(1);
    // A 429 or an unreachable REST host stops no stream: it reaches another host.
    for reason in [
        WaitReason::RateLimited,
        WaitReason::TeslaUnreachable,
        WaitReason::AuthWait,
        WaitReason::Store,
    ] {
        let mut core = stream_core(crate::config::CollectorMode::Legacy);
        core.scheduler.account.wait = Some((core.at(3_600), reason));
        assert!(core.scheduler.should_start_stream(id), "{reason:?}");
    }
    // An account that is not Collecting stops every stream.
    for state in [
        AccountState::OwnerApiRefused,
        AccountState::ProviderRefused,
        AccountState::CredentialStop,
        AccountState::ConfigError,
    ] {
        let mut core = stream_core(crate::config::CollectorMode::Legacy);
        core.scheduler.account.state = state;
        assert!(!core.scheduler.should_start_stream(id), "{state:?}");
    }
}

#[test]
fn a_car_that_stops_being_wanted_is_returned_once() {
    let mut core = stream_core(crate::config::CollectorMode::Legacy);
    let id = core_id(1);
    // The first call learns the car is wanted; nothing to stop.
    assert!(core.scheduler.streams_to_stop().is_empty());
    core.scheduler.stream_healthy(id, core.t0);
    assert!(core.scheduler.vehicles[&id].stream.connected);
    // Setting off: returned once, and its stream health ends.
    core.scheduler
        .vehicles
        .get_mut(&id)
        .unwrap()
        .settings
        .use_streaming_api = false;
    assert_eq!(core.scheduler.streams_to_stop(), vec![id]);
    assert!(core.scheduler.streams_to_stop().is_empty());
    assert!(!core.scheduler.vehicles[&id].stream.connected);
    // On again: wanted, no stop; off again: stop again.
    core.scheduler
        .vehicles
        .get_mut(&id)
        .unwrap()
        .settings
        .use_streaming_api = true;
    assert!(core.scheduler.streams_to_stop().is_empty());
    core.scheduler
        .vehicles
        .get_mut(&id)
        .unwrap()
        .settings
        .use_streaming_api = false;
    assert_eq!(core.scheduler.streams_to_stop(), vec![id]);
    // A stream C3 stopped itself reports no outage afterwards: its state is clean.
    assert!(!core.scheduler.vehicles[&id].stream.outage_reported);
}

// ---- CT71: run spacing, channel, endpoint -----------------------------------------------------

#[test]
fn ct71_run_spacing_after_error_ended_runs_doubles_to_300_seconds() {
    let mut core = stream_core(crate::config::CollectorMode::Legacy);
    let id = core_id(1);
    assert!(core.scheduler.should_start_stream(id));
    // 30 s, 60 s, 120 s, 240 s, then 300 s for every later run.
    for (failures, seconds) in [
        (1u32, 30u64),
        (2, 60),
        (3, 120),
        (4, 240),
        (5, 300),
        (6, 300),
        (9, 300),
    ] {
        let t = core.clock.mono_now();
        while core.scheduler.vehicles[&id].stream.task_failures + 1 < failures {
            core.scheduler.stream_task_ended(id, t);
        }
        core.scheduler.stream_task_ended(id, t);
        let record = &core.scheduler.vehicles[&id];
        assert_eq!(record.stream.task_failures, failures);
        assert_eq!(
            record.stream.restart_not_before,
            Some(t + Duration::from_secs(seconds)),
            "failure {failures}"
        );
        assert_eq!(record.reason, "stream_restart_delayed");
        // Not before the delay, from the delay on.
        assert!(!core.scheduler.should_start_stream(id));
        core.clock
            .advance(Duration::from_secs(seconds) - Duration::from_millis(1));
        assert!(
            !core.scheduler.should_start_stream(id),
            "{seconds} s - 1 ms"
        );
        core.clock.advance(Duration::from_millis(1));
        assert!(core.scheduler.should_start_stream(id), "{seconds} s");
    }
}

#[test]
fn ct71_a_run_that_emits_healthy_resets_the_spacing() {
    let mut core = stream_core(crate::config::CollectorMode::Legacy);
    let id = core_id(1);
    let t = core.clock.mono_now();
    for _ in 0..3 {
        core.scheduler.stream_task_ended(id, t);
    }
    assert_eq!(core.scheduler.vehicles[&id].stream.task_failures, 3);
    core.clock.advance(Duration::from_secs(300));
    // The next run emits Healthy: the count is 0, so the next error-ended run
    // waits 30 s again.
    let t1 = core.clock.mono_now();
    core.scheduler.stream_healthy(id, t1);
    assert_eq!(core.scheduler.vehicles[&id].stream.task_failures, 0);
    core.scheduler.stream_task_ended(id, t1);
    assert_eq!(
        core.scheduler.vehicles[&id].stream.restart_not_before,
        Some(t1 + Duration::from_secs(30))
    );
}

#[test]
fn ct71_a_run_stopped_by_c3_counts_nothing_and_restarts_without_delay() {
    let mut core = stream_core(crate::config::CollectorMode::Legacy);
    let id = core_id(1);
    assert!(core.scheduler.streams_to_stop().is_empty());
    // The list says asleep: the stream is stopped through `streams_to_stop`.
    let t = core.at(60);
    clock_to(&core, t);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t);
    assert_eq!(core.scheduler.streams_to_stop(), vec![id]);
    assert_eq!(core.scheduler.vehicles[&id].stream.task_failures, 0);
    assert_eq!(core.scheduler.vehicles[&id].stream.restart_not_before, None);
    // The car is listed online again, its wake read is made and the car is Online:
    // a stream starts at once.
    let t2 = core.at(120);
    clock_to(&core, t2);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t2);
    assert_eq!(core.scheduler.due_vehicles(t2), vec![id]);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, t2);
    assert!(core.scheduler.should_start_stream(id));
}

#[test]
fn ct71_the_event_channel_holds_at_least_forty_events() {
    let (sender, _receiver) = new_stream_event_channel();
    assert!(sender.capacity() >= K_STREAM_EVENT_CAPACITY);
    const { assert!(K_STREAM_EVENT_CAPACITY >= 40) };
    const { assert!(STREAM_EVENT_CHANNEL_CAPACITY >= K_STREAM_EVENT_CAPACITY) };
    // The loop's drain cap keeps well inside C4's 10 s wait for space.
    assert!(K_STREAM_DRAIN_CAP <= Duration::from_millis(100));
}

#[test]
fn ct71_the_endpoint_follows_the_auth_region_or_the_override() {
    use crate::tesla_stream::{StreamRegion, streaming_endpoint};
    // `stream_region = "china"` configured, the credential unit's region is Global:
    // the supervisor gets the global endpoint.
    let mut config = crate::config::CollectorConfig {
        stream_region: Some(StreamRegion::China),
        ..Default::default()
    };
    assert_eq!(
        config
            .stream_endpoint(StreamRegion::Global)
            .expect("endpoint"),
        streaming_endpoint(StreamRegion::Global)
    );
    assert_ne!(
        streaming_endpoint(StreamRegion::Global),
        streaming_endpoint(StreamRegion::China)
    );
    // The credential unit's China region gets China's endpoint, whatever the
    // configured region says.
    config.stream_region = Some(StreamRegion::Global);
    assert_eq!(
        config
            .stream_endpoint(StreamRegion::China)
            .expect("endpoint"),
        streaming_endpoint(StreamRegion::China)
    );
    // The override wins.
    config.stream_endpoint_override = Some("wss://stream.provider.example.test/".to_owned());
    assert_eq!(
        config
            .stream_endpoint(StreamRegion::Global)
            .expect("endpoint"),
        "wss://stream.provider.example.test/"
    );
}

#[tokio::test]
async fn ct71_no_second_run_starts_while_the_previous_run_has_not_ended() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let vehicle_id = core_id(1);
    let (_events, receiver) = mpsc::channel(4);
    let (shutdown, stop) = oneshot::channel();
    // The previous run's task has not ended: it ends when told to stop.
    let task = tokio::spawn(async move {
        let _ = stop.await;
        Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
    });
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: Some(shutdown),
        task: Some(task),
    }];
    let auth_value = crate::legacy_auth::LegacyAuth::for_test(
        url::Url::parse("https://auth.tesla.com/oauth2/v3/token").unwrap(),
        "access-secret",
        "refresh-secret",
    );
    let manager = LegacyAuthManager::for_test(auth_value, Arc::new(|_, _| Ok(())));
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    };
    let client = crate::owner_api::OwnerApi::for_fake_http(
        url::Url::parse("http://127.0.0.1:9/").unwrap(),
        Duration::from_secs(1),
    )
    .expect("client");
    let first_task = streams[0].task.as_ref().expect("task").id();
    for _ in 0..3 {
        assert!(ensure_vehicle_stream(
            &store,
            &mut streams,
            vehicle_id,
            StreamVehicleId::from_test(1),
            &auth,
            &client,
            Duration::from_secs(60),
            Some("wss://127.0.0.1:9/"),
            None,
        ));
        assert_eq!(streams.len(), 1, "one run at a time per car");
        let task = streams[0]
            .task
            .as_ref()
            .expect("the previous run's task is untouched");
        // The same task: not aborted and replaced, and not told to stop.
        assert_eq!(task.id(), first_task);
        assert!(!task.is_finished());
        assert!(streams[0]._shutdown.is_some(), "the stop signal was not sent");
    }
    // Once `disconnect_vehicle_stream` has returned the task has ended and the
    // runtime is gone, so a later run may start.
    stop_and_clear_manual_probe_streams(&mut streams).await;
    assert!(streams.is_empty());
}

/// A run that records that it was dropped, so a test can tell the task has ended.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn ct71_a_stop_waits_for_the_timeout_then_aborts_and_returns_only_after_the_task_ended() {
    // A run that ignores its stop signal: `disconnect_vehicle_stream` waits for
    // `STREAM_SHUTDOWN_TIMEOUT`, aborts it and returns only once the task has ended,
    // so no second run can start for the car before the first is gone (C4 R56 a1).
    let id = core_id(1);
    let (_events, receiver) = mpsc::channel(4);
    let (shutdown, _stop) = oneshot::channel();
    let ended = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(Arc::clone(&ended));
    let task = tokio::spawn(async move {
        let _flag = flag;
        std::future::pending::<()>().await;
        Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
    });
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id: id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: Some(shutdown),
        task: Some(task),
    }];
    tokio::task::yield_now().await;
    assert!(!ended.load(Ordering::SeqCst));
    let started = tokio::time::Instant::now();
    let queued = disconnect_vehicle_stream(&mut streams, id).await;
    assert!(queued.is_empty());
    assert!(
        tokio::time::Instant::now() - started >= STREAM_SHUTDOWN_TIMEOUT,
        "it waited for the whole timeout"
    );
    assert!(ended.load(Ordering::SeqCst), "the task had ended when it returned");
    assert!(streams.is_empty(), "and the runtime is gone");
}

#[tokio::test]
async fn r36_frames_queued_when_a_stream_is_stopped_are_stored() {
    // Lossless storage: the stop used to drop the receiver with every event still
    // queued. The frames among them are stored once the task has ended.
    let mut bed = StreamBed::new();
    let id = core_id(1);
    let (events, receiver) = mpsc::channel(64);
    let (shutdown, stop) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = stop.await;
        Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
    });
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id: id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: Some(shutdown),
        task: Some(task),
    }];
    let base = StreamBed::now_ms();
    for step in 0..3 {
        events
            .send(bed.telemetry(frame(base + step * 100, Some("P"), Some(0), Some(0))))
            .await
            .expect("queued");
    }
    events.send(StreamEvent::Healthy).await.expect("queued");
    let mut cache = HashMap::new();
    stop_vehicle_stream(
        &bed.store,
        &mut bed.core.scheduler,
        &mut streams,
        id,
        &mut cache,
    )
    .await
    .expect("the stop");
    assert!(streams.is_empty());
    // The last queued frame is the stream's watermark: all three were stored in
    // order, the control event changed nothing.
    assert_eq!(cache[&id].last_stream_timestamp_ms, Some(base + 200));
    assert_eq!(bed.stored_frames(), 1);
}

#[tokio::test]
async fn t13_the_audit_failure_shows_in_status_and_readiness_even_when_the_store_refuses_the_row() {
    // A store that refused the receipt would refuse the row that says so, so the
    // in-process status is the source (C3 R28, section 8 item 6): the tick commit
    // fails, and the status and readiness still show `collector_stream_audit_failed`.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = StreamBed::new();
            let id = core_id(1);
            // The car has a Hub identity, as in the loop, so its row can be written.
            let source = bed
                .store
                .register_source(&provider_source(CollectorProvider::Legacy), CORE_W0)
                .expect("source");
            let registered = bed
                .store
                .register_vehicle(
                    &VehicleDescriptor::new(source.source_id, "1")
                        .with_tesla_identity(Some(1), None),
                    CORE_W0,
                )
                .expect("vehicle");
            bed.core.scheduler.vehicles.get_mut(&id).unwrap().hub_id = Some(registered.vehicle_id);
            let (_events, receiver) = mpsc::channel(4);
            let task = tokio::spawn(async {
                Err::<(), _>(crate::tesla_stream::StreamSupervisorError::Audit(
                    StoreError::NilVehicleId,
                ))
            });
            let mut streams = vec![VehicleStreamRuntime {
                vehicle_id: id,
                power_gate: Arc::new(StreamPowerGate::default()),
                sensitive_access_failure: Arc::new(AtomicBool::new(false)),
                events: receiver,
                _shutdown: None,
                task: Some(task),
            }];
            while !streams[0].task.as_ref().is_some_and(JoinHandle::is_finished) {
                tokio::task::yield_now().await;
            }
            let mut projection = HashMap::new();
            drain_stream_events_with_cache(
                &bed.store,
                &mut bed.core.scheduler,
                &mut streams,
                &mut projection,
            )
            .await
            .expect("an audit failure is not a collector error");
            // The store refuses every write: the tick that would store the row fails.
            seam.set_tick_commit_fault(true);
            let now = bed.core.clock.mono_now();
            bed.core.scheduler.begin_tick();
            let batch = TickBatch::default();
            let commit = TickCommit {
                store: &bed.store,
                cursor_key: &CursorKey::from_bytes([0x13; 32]),
                provider: CollectorProvider::Legacy,
                offline_drive_timeout: Duration::from_secs(600),
            };
            let mut identities = IdentityCache::default();
            let outcome = commit_tick(&commit, &mut identities, &mut bed.core.scheduler, batch, now)
                .await
                .expect("commit");
            seam.set_tick_commit_fault(false);
            assert!(matches!(outcome, TickOutcome::Failed), "{outcome:?}");
            let stored: i64 = bed
                .store
                .open()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM collector_vehicle_state WHERE reason_code = 'stream_audit_failed'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(stored, 0, "the store refused the row");
            let status = bed.core.scheduler.status(now);
            assert_eq!(status.vehicles[0].detail, Some(STATUS_STREAM_AUDIT_FAILED));
            assert_eq!(
                bed.store.collector_readiness_at(CORE_W0).unwrap().failure,
                Some(crate::db::CollectionAttentionCode::CollectorStreamAuditFailed)
            );
        })
        .await;
}

#[derive(Clone)]
struct GateWire {
    hits: Arc<AtomicUsize>,
}

async fn gate_wire_vehicle_data(State(wire): State<GateWire>) -> axum::response::Response {
    wire.hits.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        r#"{"response":{"id":1,"vehicle_id":1,"drive_state":{"timestamp":1700000000000},"charge_state":{"battery_level":80}}}"#,
    )
        .into_response()
}

#[tokio::test]
async fn t2_a_gated_read_the_real_gate_does_not_confirm_sends_nothing_and_counts_nothing() {
    // The production read path with C4's real power gate against a fake Owner server:
    // a gate that is missing, never confirmed, or confirmed and then revoked sends no
    // request and is not counted (R36, R25); a confirmed gate sends one.
    let wire = GateWire {
        hits: Arc::new(AtomicUsize::new(0)),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new()
        .route("/api/1/vehicles/{id}/vehicle_data", get(gate_wire_vehicle_data))
        .with_state(wire.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Owner");
    });
    let client = crate::owner_api::OwnerApi::for_fake_http(
        url::Url::parse(&format!("http://{address}/")).unwrap(),
        Duration::from_secs(2),
    )
    .expect("client");
    let auth_value = crate::legacy_auth::LegacyAuth::for_test(
        url::Url::parse("https://auth.tesla.com/oauth2/v3/token").unwrap(),
        "access-secret",
        "refresh-secret",
    );
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
            auth_value,
            Arc::new(|_, _| Ok(())),
        ))),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    };
    let mut core = Core::owner();
    first_read_parked(&mut core);
    let id = core_id(1);
    let ledger = core.scheduler.request_ledger();
    let sent = |ledger: &RequestLedger| ledger.view().requests[RequestCategory::Data.index()].1;
    let before = sent(&ledger);
    let wall = current_epoch_millis().expect("clock");
    let confirmed_then_revoked = {
        let gate = Arc::new(StreamPowerGate::default());
        gate.observe(&frame(wall, Some("D"), Some(20), Some(12)), wall);
        assert!(gate.is_confirmed());
        gate.revoke();
        gate
    };
    for (label, gate) in [
        ("no gate: the car's stream is gone", None),
        ("a gate no frame confirmed", Some(Arc::new(StreamPowerGate::default()))),
        ("confirmed, then revoked", Some(confirmed_then_revoked)),
    ] {
        let (key, result) = owner_read_future(
            &mut core.scheduler,
            id,
            true,
            gate,
            &client,
            &auth,
            &ledger,
        )
        .await;
        assert_eq!(key, TickKey::Read(id));
        assert!(is_stream_power_not_confirmed_result(&result), "{label}");
        assert_eq!(wire.hits.load(Ordering::SeqCst), 0, "{label}: nothing was sent");
        assert_eq!(sent(&ledger), before, "{label}: nothing was counted");
    }
    // A confirmed gate lets the read go out and counts it.
    let gate = Arc::new(StreamPowerGate::default());
    gate.observe(&frame(wall, Some("D"), Some(20), Some(12)), wall);
    let (_, result) = owner_read_future(
        &mut core.scheduler,
        id,
        true,
        Some(gate),
        &client,
        &auth,
        &ledger,
    )
    .await;
    assert!(!is_stream_power_not_confirmed_result(&result), "{result:?}");
    assert_eq!(wire.hits.load(Ordering::SeqCst), 1);
    assert_eq!(sent(&ledger), before + 1);
    // The gate is checked before the read is admitted: with the local cap exhausted (a
    // cap of zero here) an unconfirmed gate must not even ask to start a request, or
    // the account would enter `CapReached` for a read that sends nothing (R27, R36).
    let mut capped = Core::with_fleet(
        crate::config::CollectorMode::FleetPolling,
        k7_policy(0, crate::config::FLEET_PRICING),
    );
    first_read_parked(&mut capped);
    let capped_ledger = capped.scheduler.request_ledger();
    let (_, result) = owner_read_future(
        &mut capped.scheduler,
        id,
        true,
        Some(Arc::new(StreamPowerGate::default())),
        &client,
        &auth,
        &capped_ledger,
    )
    .await;
    assert!(is_stream_power_not_confirmed_result(&result));
    assert_eq!(capped.scheduler.account.state, AccountState::Collecting);
    assert_eq!(wire.hits.load(Ordering::SeqCst), 1, "nothing more was sent");
    server.abort();
}

#[tokio::test]
async fn r9_the_stream_frame_path_logs_the_hub_id_and_never_a_tesla_id() {
    // CT56's capture, on the stream path: the ingestion warnings and the rejected
    // frames name the car by its Hub id, never by the Tesla id (R9).
    let log = DecisionLog::start();
    let mut bed = StreamBed::new();
    let base = StreamBed::now_ms();
    bed.dispatch(bed.telemetry(frame(base, Some("P"), Some(0), Some(0))));
    // A frame that waited 7 s in the queue after a 12 s gap in the source's time:
    // both ingestion warnings fire.
    let queued_at = bed.core.clock.mono_now();
    bed.core.clock.advance(Duration::from_secs(7));
    bed.dispatch(StreamEvent::Telemetry {
        update: Box::new(frame(base + 12_000, Some("P"), Some(0), Some(0))),
        queued_at,
    });
    // A frame that is not later than the last stored one is rejected (DEBUG), and so
    // is one dated in the future.
    bed.dispatch(bed.telemetry(frame(base + 12_000, Some("P"), Some(0), Some(0))));
    bed.dispatch(bed.telemetry(frame(base + 86_400_000, Some("P"), Some(0), Some(0))));
    let text = log.contents();
    let own: Vec<&str> = text
        .lines()
        .filter(|line| line.contains(" teslatlas_hub::") || line.contains(" collector."))
        .collect();
    for reason in [
        "upstream_stream_gap",
        "local_stream_processing_lag",
        "non_monotonic_timestamp",
        "clock_or_future_timestamp",
    ] {
        assert!(
            own.iter().any(|line| line.contains(reason)),
            "{reason} is logged:\n{text}"
        );
    }
    for line in &own {
        assert!(!line.contains("vehicle_id"), "{line}");
        if let Some(rest) = line.split("vehicle=").nth(1) {
            let id = rest.split_whitespace().next().unwrap_or_default();
            assert!(
                Uuid::parse_str(id).is_ok() || matches!(id, "unmapped" | "account"),
                "the Hub id, not a Tesla id: {line}"
            );
        }
    }
}

// ---- The dispatcher, the drain and the loop's stream bookkeeping -----------------------------------

/// A scheduler on a manual clock with car 1 online, a real store, and a stream runtime
/// for car 1, to run events through the dispatcher.
struct StreamBed {
    _dir: tempfile::TempDir,
    store: HubStore,
    core: Core,
    runtime: VehicleStreamRuntime,
    _events: mpsc::Sender<StreamEvent>,
    cache: HashMap<VehicleId, StreamContext>,
}

impl StreamBed {
    fn new() -> Self {
        let dir = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(dir.path()).expect("store");
        let mut core = Core::owner();
        first_read_parked(&mut core);
        let (events, receiver) = mpsc::channel(64);
        let runtime = VehicleStreamRuntime {
            vehicle_id: core_id(1),
            power_gate: Arc::new(StreamPowerGate::default()),
            sensitive_access_failure: Arc::new(AtomicBool::new(false)),
            events: receiver,
            _shutdown: None,
            task: None,
        };
        Self {
            _dir: dir,
            store,
            core,
            runtime,
            _events: events,
            cache: HashMap::new(),
        }
    }

    /// Run one event through the dispatcher; returns whether the stream rejected
    /// authentication and whether it showed authenticated.
    fn dispatch(&mut self, event: StreamEvent) -> (bool, bool) {
        let mut rejected = false;
        let mut authenticated = false;
        process_stream_event(
            &self.store,
            &mut self.core.scheduler,
            &self.runtime,
            event,
            &mut self.cache,
            &mut rejected,
            &mut authenticated,
        )
        .expect("event");
        (rejected, authenticated)
    }

    fn telemetry(&self, update: crate::tesla_stream::StreamUpdate) -> StreamEvent {
        StreamEvent::Telemetry {
            update: Box::new(update),
            queued_at: self.core.clock.mono_now(),
        }
    }

    fn now_ms() -> i64 {
        current_epoch_millis().expect("clock") - 1_000
    }

    fn stored_frames(&self) -> i64 {
        self.store
            .open()
            .expect("connection")
            .query_row(
                "SELECT COUNT(*) FROM current_observations
                  WHERE record_type = 'tesla_stream_update_v1'",
                [],
                |row| row.get(0),
            )
            .expect("count")
    }
}

#[test]
fn the_dispatcher_stores_a_frame_and_judges_it_as_evidence() {
    let mut bed = StreamBed::new();
    let id = core_id(1);
    bed.core.clock.advance(Duration::from_secs(30));
    let wall = bed.core.clock.wall_now_ms().unwrap();
    let (rejected, authenticated) =
        bed.dispatch(bed.telemetry(frame(StreamBed::now_ms(), Some("D"), Some(20), Some(12))));
    assert!(!rejected);
    assert!(authenticated, "a frame proves the stream authenticated");
    assert_eq!(bed.stored_frames(), 1);
    let record = &bed.core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Driving);
    assert_eq!(record.last_in_use, Some(bed.core.clock.mono_now()));
    // The stored frame is the car's latest observation for R40, and its lifecycle
    // view is stale.
    assert_eq!(record.last_stored_obs_ms, Some(wall));
    assert!(record.lifecycle_stale);
    assert!(record.stream.connected);
}

#[test]
fn the_dispatcher_stores_the_frames_of_a_held_car_and_not_a_disabled_ones() {
    // Held (Paused): stored, no read, the hold kept (R36, B3).
    let mut bed = StreamBed::new();
    let id = core_id(1);
    bed.core.scheduler.vehicles.get_mut(&id).unwrap().hold =
        VehicleHold::Paused(PauseReason::PersistentFailure);
    bed.dispatch(bed.telemetry(frame(StreamBed::now_ms(), Some("P"), Some(0), Some(0))));
    assert_eq!(bed.stored_frames(), 1);
    // An account that waits or has stopped stores frames too: a REST failure says
    // nothing about the stream's data.
    bed.core.scheduler.account.state = AccountState::OwnerApiRefused;
    bed.core.scheduler.account.wait = Some((bed.core.at(3_600), WaitReason::RateLimited));
    bed.dispatch(bed.telemetry(frame(
        StreamBed::now_ms() + 100,
        Some("P"),
        Some(0),
        Some(0),
    )));
    assert_eq!(bed.stored_frames(), 1, "one current row per record type");
    assert!(
        bed.core.scheduler.vehicles[&id]
            .last_stored_obs_ms
            .is_some()
    );
    // Disabled: not stored.
    let mut disabled = StreamBed::new();
    disabled.core.scheduler.vehicles.get_mut(&id).unwrap().hold = VehicleHold::Disabled;
    disabled.dispatch(disabled.telemetry(frame(
        StreamBed::now_ms(),
        Some("D"),
        Some(20),
        Some(12),
    )));
    assert_eq!(disabled.stored_frames(), 0);
}

#[test]
fn the_dispatcher_asks_for_gate_revocations_on_the_events_r36_names() {
    let mut bed = StreamBed::new();
    let id = core_id(1);
    for event in [
        StreamEvent::VehicleOffline,
        StreamEvent::TransportUnavailable,
        StreamEvent::ProtocolViolation,
        StreamEvent::AuthRejected,
    ] {
        assert!(bed.core.scheduler.take_gate_revocations().is_empty());
        bed.dispatch(event.clone());
        assert_eq!(
            bed.core.scheduler.take_gate_revocations(),
            vec![id],
            "{event:?}"
        );
    }
    // Healthy and Telemetry revoke nothing: the confirming frame is C4's.
    bed.dispatch(StreamEvent::Healthy);
    bed.dispatch(bed.telemetry(frame(StreamBed::now_ms(), Some("D"), Some(20), Some(3))));
    assert!(bed.core.scheduler.take_gate_revocations().is_empty());
}

#[test]
fn a_vehicle_offline_event_asks_for_a_targeted_state_check_now() {
    let mut bed = StreamBed::new();
    let id = core_id(1);
    let now = bed.core.clock.mono_now();
    assert!(
        bed.core
            .scheduler
            .due_offline_state_vehicles(now)
            .is_empty()
    );
    bed.dispatch(StreamEvent::VehicleOffline);
    assert_eq!(bed.core.scheduler.due_offline_state_vehicles(now), vec![id]);
}

#[test]
fn outage_events_report_once_per_outage_and_a_healthy_event_reports_the_recovery() {
    let log = DecisionLog::start();
    let mut bed = StreamBed::new();
    let id = core_id(1);
    bed.dispatch(StreamEvent::Healthy);
    // The first unhealthy event of an outage reports it; the second is ignored.
    let (rejected, _) = bed.dispatch(StreamEvent::TransportUnavailable);
    assert!(!rejected);
    bed.dispatch(StreamEvent::TransportUnavailable);
    bed.dispatch(StreamEvent::ProtocolViolation);
    assert_eq!(log.lines("INFO", "stream_outage"), 1, "{}", log.contents());
    assert_eq!(bed.core.scheduler.vehicles[&id].reason, "stream_outage");
    assert!(!bed.core.scheduler.vehicles[&id].stream.connected);
    // AuthRejected is evidence for the credential unit.
    let (rejected, authenticated) = bed.dispatch(StreamEvent::AuthRejected);
    assert!(rejected);
    assert!(!authenticated);
    // The first healthy event after the outage reports the recovery once.
    let (_, authenticated) = bed.dispatch(StreamEvent::Healthy);
    assert!(authenticated);
    bed.dispatch(StreamEvent::Healthy);
    assert_eq!(
        log.lines("INFO", "stream_recovered"),
        1,
        "{}",
        log.contents()
    );
    assert_eq!(bed.core.scheduler.vehicles[&id].reason, "stream_recovered");
    // No identifier of the car is logged: its Hub id or "unmapped", never its Tesla
    // id or VIN.
    let text = log.contents();
    assert!(!text.contains("VIN00000000000001"), "{text}");
    assert!(!text.contains("vehicle_id"), "{text}");
}

#[tokio::test]
async fn a_run_that_ends_with_an_error_is_spaced_and_leaves_the_collector_running() {
    let mut bed = StreamBed::new();
    let id = core_id(1);
    for (error, expect_audit) in [
        (
            crate::tesla_stream::StreamSupervisorError::EventQueueFull,
            false,
        ),
        (
            crate::tesla_stream::StreamSupervisorError::EventReceiverClosed,
            false,
        ),
        (
            crate::tesla_stream::StreamSupervisorError::Audit(StoreError::NilVehicleId),
            true,
        ),
    ] {
        let (_events, receiver) = mpsc::channel(4);
        let task = tokio::spawn(async move { Err::<(), _>(error) });
        let mut streams = vec![VehicleStreamRuntime {
            vehicle_id: id,
            power_gate: Arc::new(StreamPowerGate::default()),
            sensitive_access_failure: Arc::new(AtomicBool::new(false)),
            events: receiver,
            _shutdown: None,
            task: Some(task),
        }];
        while !streams[0]
            .task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            tokio::task::yield_now().await;
        }
        let failures_before = bed.core.scheduler.vehicles[&id].stream.task_failures;
        let mut projection = HashMap::new();
        let result = drain_stream_events_with_cache(
            &bed.store,
            &mut bed.core.scheduler,
            &mut streams,
            &mut projection,
        )
        .await
        .expect("an error-ended run is not a collector error");
        assert!(
            result.terminal_error.is_none(),
            "{error_label}",
            error_label = expect_audit
        );
        assert_eq!(result.ended, vec![id]);
        let record = &bed.core.scheduler.vehicles[&id];
        assert_eq!(record.stream.task_failures, failures_before + 1);
        assert!(record.stream.restart_not_before.is_some());
        assert_eq!(record.stream.audit_failed, expect_audit);
        assert!(!bed.core.scheduler.should_start_stream(id));
        // The loop removes the ended runtime so that a later run can start.
        assert!(streams[0].task.is_none());
    }
}

#[tokio::test]
async fn a_credential_authority_failure_still_stops_the_collector() {
    let mut bed = StreamBed::new();
    let id = core_id(1);
    let (_events, receiver) = mpsc::channel(4);
    let task = tokio::spawn(async {
        Err::<(), _>(crate::tesla_stream::StreamSupervisorError::CredentialAuthorityUnavailable)
    });
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id: id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(true)),
        events: receiver,
        _shutdown: None,
        task: Some(task),
    }];
    while !streams[0]
        .task
        .as_ref()
        .is_some_and(JoinHandle::is_finished)
    {
        tokio::task::yield_now().await;
    }
    let mut projection = HashMap::new();
    let result = drain_stream_events_with_cache(
        &bed.store,
        &mut bed.core.scheduler,
        &mut streams,
        &mut projection,
    )
    .await
    .expect("drain");
    assert!(matches!(
        result.terminal_error,
        Some(CollectorError::SensitiveAccessUnavailable)
    ));
    assert!(result.ended.is_empty());
    assert_eq!(bed.core.scheduler.vehicles[&id].stream.task_failures, 0);
}

#[tokio::test]
async fn a_panicked_or_cancelled_run_is_spaced_like_any_other_error() {
    for kind in ["panic", "normal"] {
        let mut bed = StreamBed::new();
        let id = core_id(1);
        let (_events, receiver) = mpsc::channel(4);
        let task = if kind == "panic" {
            tokio::spawn(async { panic!("stream run panicked") })
        } else {
            // A run that returns normally though nobody asked it to stop would
            // restart in a tight loop if it were not spaced.
            tokio::spawn(async { Ok::<(), crate::tesla_stream::StreamSupervisorError>(()) })
        };
        let mut streams = vec![VehicleStreamRuntime {
            vehicle_id: id,
            power_gate: Arc::new(StreamPowerGate::default()),
            sensitive_access_failure: Arc::new(AtomicBool::new(false)),
            events: receiver,
            _shutdown: None,
            task: Some(task),
        }];
        while !streams[0]
            .task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            tokio::task::yield_now().await;
        }
        let mut projection = HashMap::new();
        let result = drain_stream_events_with_cache(
            &bed.store,
            &mut bed.core.scheduler,
            &mut streams,
            &mut projection,
        )
        .await
        .expect("drain");
        assert!(result.terminal_error.is_none(), "{kind}");
        assert_eq!(result.ended, vec![id], "{kind}");
        assert_eq!(bed.core.scheduler.vehicles[&id].stream.task_failures, 1);
    }
}

#[tokio::test]
async fn an_audit_failure_shows_in_status_and_readiness_until_a_run_emits_healthy() {
    let mut bed = StreamBed::new();
    let id = core_id(1);
    // The car has a Hub identity and a row, as in the loop.
    let source = bed
        .store
        .register_source(&provider_source(CollectorProvider::Legacy), CORE_W0)
        .expect("source");
    let registered = bed
        .store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "1").with_tesla_identity(Some(1), None),
            CORE_W0,
        )
        .expect("vehicle");
    bed.core.scheduler.vehicles.get_mut(&id).unwrap().hub_id = Some(registered.vehicle_id);
    let now = bed.core.clock.mono_now();
    bed.core.scheduler.stream_audit_failed(id);
    bed.core.scheduler.stream_task_ended(id, now);
    let status = bed.core.scheduler.status(now);
    assert_eq!(status.vehicles[0].detail, Some(STATUS_STREAM_AUDIT_FAILED));
    assert_eq!(
        STATUS_STREAM_AUDIT_FAILED,
        "stream receipts cannot be written"
    );
    // The row the next tick writes carries the reason that readiness reads, whatever
    // transition came since.
    bed.core.scheduler.vehicles.get_mut(&id).unwrap().reason = "reported_online";
    let tick = bed.core.scheduler.take_tick();
    let row = &tick.vehicles[0].row;
    assert_eq!(row.reason_code, "stream_audit_failed");
    write_collector_rows(&bed.store, &tick.to_rows_write(CORE_W0));
    assert_eq!(
        bed.store.collector_readiness_at(CORE_W0).unwrap().failure,
        Some(crate::db::CollectionAttentionCode::CollectorStreamAuditFailed)
    );
    // A later run emits Healthy: the failure clears.
    bed.core.scheduler.commit_succeeded(tick, now);
    bed.dispatch(StreamEvent::Healthy);
    assert!(!bed.core.scheduler.vehicles[&id].stream.audit_failed);
    let status = bed.core.scheduler.status(now);
    assert_eq!(status.vehicles[0].detail, None);
    let tick = bed.core.scheduler.take_tick();
    write_collector_rows(&bed.store, &tick.to_rows_write(CORE_W0 + 1));
    assert_eq!(
        bed.store.collector_readiness_at(CORE_W0).unwrap().failure,
        None
    );
}

#[tokio::test]
async fn a_silent_driving_stream_is_one_outage_at_thirty_seconds() {
    let log = DecisionLog::start();
    let mut bed = StreamBed::new();
    let id = core_id(1);
    let mut projection = HashMap::new();
    let mut streams: Vec<VehicleStreamRuntime> = Vec::new();
    // The car is Driving with a frame at t.
    bed.dispatch(bed.telemetry(frame(StreamBed::now_ms(), Some("D"), Some(20), Some(5))));
    assert_eq!(bed.core.scheduler.vehicles[&id].phase, PollPhase::Driving);
    let silent = |bed: &StreamBed| bed.core.scheduler.vehicles[&id].stream.outage_reported;
    // At 29.999 s the frame is still within the silence window.
    bed.core.clock.advance(Duration::from_millis(29_999));
    let result = drain_stream_events_with_cache(
        &bed.store,
        &mut bed.core.scheduler,
        &mut streams,
        &mut projection,
    )
    .await
    .expect("drain");
    assert!(result.terminal_error.is_none());
    assert!(!silent(&bed), "29.999 s");
    // At exactly 30 s it is silent: one outage, reported once (R28, CT50).
    bed.core.clock.advance(Duration::from_millis(1));
    for _ in 0..3 {
        drain_stream_events_with_cache(
            &bed.store,
            &mut bed.core.scheduler,
            &mut streams,
            &mut projection,
        )
        .await
        .expect("drain");
    }
    assert!(silent(&bed));
    assert_eq!(log.lines("INFO", "stream_outage"), 1, "{}", log.contents());
    assert_eq!(log.lines("INFO", "frame_silence"), 1, "{}", log.contents());
}

#[test]
fn gate_revocations_are_applied_to_the_streams_that_were_named() {
    let mut core = Core::owner();
    first_read_parked(&mut core);
    let id = core_id(1);
    core.scheduler.revoke_gate(id);
    // A car the scheduler does not know needs none.
    core.scheduler.revoke_gate(core_id(9));
    let mut revoked = Vec::new();
    apply_gate_revocations_with(&mut core.scheduler, |vehicle| revoked.push(vehicle));
    assert_eq!(revoked, vec![id]);
    apply_gate_revocations_with(&mut core.scheduler, |_| panic!("applied once"));
}

// ---- CT49 and CT50: REST and the stream fail independently (R54) ------------------------------------

#[test]
fn ct49_a_healthy_stream_keeps_the_data_flowing_while_rest_fails() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler.stream_healthy(id, core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, core.t0);
    assert!(core.scheduler.should_start_stream(id));
    assert!(core.scheduler.streams_to_stop().is_empty());
    // Frames every 2 s; every read gets 503. The Driving phase holds, the frames are
    // stored and the REST back-off grows (u = 0): 60 s (the interval) while the
    // back-off is shorter, then the back-off from 80 s.
    let mut due_times = Vec::new();
    let mut next_frame = 2u64;
    let mut failures = 0u32;
    let error = CollectorError::OwnerApi(OwnerApiError::HttpStatus(503));
    for second in 1..=3_700u64 {
        let t = core.at(second);
        if second == next_frame {
            assert!(
                core.scheduler
                    .should_persist_stream_telemetry(id, Some(5), t),
                "frames of a driving car are stored"
            );
            core.scheduler
                .schedule_stream_evidence(id, Some("D"), Some(20.0), Some(5), t);
            next_frame += 2;
        }
        let due = core.scheduler.due_vehicles(t);
        if due.is_empty() {
            continue;
        }
        due_times.push(second);
        core.scheduler.vehicle_failed_for_error(id, &error, t);
        failures += 1;
        assert_eq!(core.scheduler.vehicles[&id].consecutive_failures, failures);
        assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Driving);
        // REST failure stops no stream.
        assert!(core.scheduler.should_start_stream(id));
    }
    // Reads at 60 s intervals while the back-off (5, 10, 20, 40 s with u = 0) is
    // shorter, then +80 s (nominal 160 s), then +150 s (the cap's half), with the
    // stream's own motion frames not making reads faster than the Driving cadence
    // they already have.
    assert_eq!(&due_times[..8], &[60, 120, 180, 240, 300, 380, 530, 680]);
    // The thresholds of R51 (12 failures in a row over at least an hour, the first
    // at 60 s) pause the car at its 13th failure after 3,660 s: the frames are
    // still stored, the stream stays wanted, and the status says the stream is the
    // only source.
    let t = core.at(3_700);
    assert_eq!(
        core.scheduler.vehicles[&id].hold,
        VehicleHold::Paused(PauseReason::PersistentFailure)
    );
    assert!(
        core.scheduler.vehicles[&id].consecutive_failures >= K_PAUSE_FAILURES,
        "twelve failures in a row"
    );
    core.scheduler.stream_healthy(id, t);
    assert!(
        core.scheduler
            .should_persist_stream_telemetry(id, Some(5), t)
    );
    assert!(core.scheduler.should_start_stream(id));
    clock_to(&core, t);
    assert!(core.scheduler.streams_to_stop().is_empty());
    let status = core.scheduler.status(t);
    assert_eq!(status.vehicles[0].detail, Some("partial: stream only"));
    assert_eq!(STATUS_STREAM_ONLY, "partial: stream only");
    // A paused car without a healthy stream says nothing of the kind.
    core.scheduler.stream_unhealthy(id, t);
    assert_eq!(core.scheduler.status(t).vehicles[0].detail, None);
}

#[test]
fn ct50_a_failing_stream_makes_rest_take_over_at_the_fallback_interval() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler.stream_healthy(id, core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, core.t0);
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(60))
    );
    // Healthy stream: frames keep arriving and reads are 60 s apart.
    for minute in 1..=2u64 {
        let t = core.at(60 * minute);
        core.scheduler
            .should_persist_stream_telemetry(id, Some(5), t - Duration::from_secs(5));
        assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Driving, false, t);
        assert_eq!(
            core.scheduler.vehicles[&id].next_read_due,
            Some(t + Duration::from_secs(60))
        );
    }
    // TransportUnavailable at +125 s: exactly one outage, with a 10 s fallback.
    let t_outage = core.at(125);
    let StreamOutage::Active(status) = core.scheduler.stream_unhealthy(id, t_outage) else {
        panic!("the first unhealthy event reports the outage");
    };
    assert_eq!(
        status.owner_api_fallback_after,
        Some(Duration::from_secs(10))
    );
    assert!(status.owner_api_fallback_scheduled);
    assert_eq!(status.phase, PollPhase::Driving);
    // The fallback read is 10 s after the last read (+120 s), not 60 s.
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(130))
    );
    assert!(!core.scheduler.has_due_stream_fallback(t_outage));
    assert!(core.scheduler.has_due_stream_fallback(core.at(130)));
    // A second outage event is ignored.
    assert_eq!(
        core.scheduler.stream_unhealthy(id, core.at(126)),
        StreamOutage::Ignored
    );
    // Reads every 10 s while the outage lasts: +130 s and +140 s.
    for offset in [130u64, 140] {
        let t = core.at(offset);
        assert_eq!(core.scheduler.due_vehicles(t), vec![id], "{offset} s");
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Driving, false, t);
        assert_eq!(
            core.scheduler.vehicles[&id].next_read_due,
            Some(t + Duration::from_secs(10)),
            "{offset} s"
        );
    }
    // The next Telemetry frame, at +143 s, recovers the stream: once.
    let t_frame = core.at(143);
    assert!(
        core.scheduler
            .should_persist_stream_telemetry(id, Some(5), t_frame)
    );
    assert!(matches!(
        core.scheduler.stream_healthy(id, t_frame),
        StreamRecovery::Recovered(_)
    ));
    assert_eq!(
        core.scheduler.stream_healthy(id, core.at(144)),
        StreamRecovery::Unchanged
    );
    // The read at +150 s is on the fallback schedule; its successor is 60 s away.
    let t = core.at(150);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, t);
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(210))
    );
}

#[test]
fn ct50_frames_that_stop_while_driving_are_one_outage_at_thirty_seconds() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler.stream_healthy(id, core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, core.t0);
    // The last frame arrives at t0 + 10 s.
    let last = core.at(10);
    core.scheduler
        .should_persist_stream_telemetry(id, Some(5), last);
    assert!(core.scheduler.silent_drive_streams(last).is_empty());
    // No outage at 29.999 s of silence; one at exactly 30 s (R28, CT50).
    assert!(
        core.scheduler
            .silent_drive_streams(last + Duration::from_millis(29_999))
            .is_empty()
    );
    let late = last + K_DRIVE_FRAME_SILENCE;
    assert_eq!(core.scheduler.silent_drive_streams(late), vec![id]);
    // The loop is woken for it.
    assert_eq!(core.scheduler.vehicles[&id].silence_deadline(), Some(late));
    let StreamOutage::Active(status) = core.scheduler.stream_unhealthy(id, late) else {
        panic!("a silent driving stream is an outage");
    };
    assert_eq!(
        status.owner_api_fallback_after,
        Some(Duration::from_secs(10))
    );
    // Reported once: it is no longer silent in the sense of R28's report.
    assert!(
        core.scheduler
            .silent_drive_streams(late + Duration::from_secs(60))
            .is_empty()
    );
    // A parked car's stream may be silent: no outage.
    let mut parked = Core::owner();
    first_read_parked(&mut parked);
    parked.scheduler.stream_healthy(id, parked.t0);
    assert!(
        parked
            .scheduler
            .silent_drive_streams(parked.at(3_600))
            .is_empty()
    );
}

// ---- R40 and CT31, CT61: a drive that ended while the car was unreachable -------------------------

impl TickBed {
    /// A drive with two positions whose latest observation was received at `w0`
    /// (the bed's wall time), stored through the tick transaction: the car's last
    /// stored observation and last fold are both `w0`.
    async fn store_open_drive(&mut self) -> Uuid {
        let w0 = self.wall();
        self.list(vec![core_vehicle(1, "online")]).await;
        let hub = self.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
        let vehicle = self.scheduler.vehicles[&core_id(1)].vehicle.clone();
        let item = |timestamp: i64, latitude: f64, odometer: f64| SnapshotItem {
            vehicle: vehicle.clone(),
            snapshot: VehicleData::for_test(
                1,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 20,
                        "latitude": latitude,
                        "longitude": 19.0,
                        "timestamp": timestamp
                    },
                    "vehicle_state": {"odometer": odometer}
                }),
            ),
            received_at_ms: w0,
        };
        let now = self.now();
        self.scheduler.begin_tick();
        let batch = TickBatch {
            snapshots: vec![item(w0 - 1_000, 47.5, 1000.0), item(w0, 47.51, 1000.1)],
            first_result_at: Some(now),
            ..TickBatch::default()
        };
        self.commit(batch).await;
        hub
    }

    /// One tick that plans the `no_data` events and commits them; returns the
    /// planned events.
    async fn no_data_tick(&mut self) -> Vec<HubEventItem> {
        let now = self.now();
        self.scheduler.begin_tick();
        let mut batch = TickBatch::default();
        plan_no_data_events(&self.store, &mut self.scheduler, &mut batch).expect("plan");
        let planned = batch.hub_events.clone();
        if !planned.is_empty() {
            batch.first_result_at = Some(now);
        }
        self.commit(batch).await;
        planned
    }

    fn last_observation_id(&self, hub: Uuid) -> i64 {
        self.store
            .load_lifecycle_state(hub)
            .expect("lifecycle")
            .expect("state")
            .last_observation_id
    }

    fn hub_event_current_rows(&self) -> i64 {
        self.query("SELECT COUNT(*) FROM current_observations WHERE record_type = 'hub_lifecycle_event_v1'")
    }
}

#[tokio::test]
async fn ct31_an_open_drive_without_evidence_for_longer_than_t_gap_gets_one_no_data_event() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let w0 = CORE_W0;
            let id = core_id(1);
            assert!(
                bed.store
                    .collector_lifecycle_snapshot(hub)
                    .unwrap()
                    .drive_open
            );
            assert_eq!(
                bed.scheduler.vehicles[&id].last_stored_obs_ms,
                Some(w0),
                "the car's evidence is the read received at w0"
            );
            // The car is Suspended with no hold.
            bed.scheduler.vehicles.get_mut(&id).unwrap().phase = PollPhase::Suspended;
            // The phase is a persisted field: one tick stores it first.
            assert!(bed.no_data_tick().await.is_empty());
            let observation = bed.last_observation_id(hub);
            let commits = seam.tick_commits();
            // A tick at w0 + 600,000 ms (T_gap): no event.
            bed.advance(Duration::from_millis(600_000));
            assert_eq!(bed.wall(), w0 + 600_000);
            assert!(bed.no_data_tick().await.is_empty());
            assert_eq!(bed.last_observation_id(hub), observation);
            assert_eq!(seam.tick_commits(), commits);
            // At w0 + 600,001 ms: exactly one event with the Hub wall time.
            bed.advance(Duration::from_millis(1));
            let planned = bed.no_data_tick().await;
            assert_eq!(planned.len(), 1);
            assert_eq!(planned[0].event, "no_data");
            assert_eq!(planned[0].observed_at_ms, w0 + 600_001);
            assert_eq!(planned[0].vehicle.id, id);
            assert_eq!(
                bed.no_data_rows(),
                vec![w0 + 600_001],
                "the accepted time is the transaction's decision time"
            );
            // The intake took it: the cursor moved by one observation, the event
            // wrote no current row, and the drive ended at its last fold (w0).
            assert_eq!(bed.last_observation_id(hub), observation + 1);
            assert_eq!(bed.hub_event_current_rows(), 0);
            let drives = bed.store.materialised_history(hub).unwrap().drives;
            assert_eq!(drives.len(), 1);
            assert_eq!(drives[0].end_date_ms, w0);
            assert!(
                !bed.store
                    .collector_lifecycle_snapshot(hub)
                    .unwrap()
                    .drive_open
            );
            assert_eq!(seam.tick_commits(), commits + 1);
            assert!(bed.scheduler.vehicles[&id].force_closed_at_gap);
            // Ticks at +11 minutes and +20 minutes: no further event.
            for minutes_later in [11u64, 20] {
                let target =
                    Duration::from_secs(minutes_later * 60) - Duration::from_millis(600_001);
                bed.advance(target);
                assert!(bed.no_data_tick().await.is_empty());
                assert_eq!(bed.last_observation_id(hub), observation + 1);
            }
            assert_eq!(seam.tick_commits(), commits + 1);
            assert_eq!(bed.no_data_rows().len(), 1, "no duplicate");
        })
        .await;
}

#[tokio::test]
async fn ct31_a_current_open_drive_view_gets_no_event_at_exactly_t_gap() {
    // CT31 above reads its lifecycle view late, so only the reload gate judges the
    // boundary there. Here the view is current and open, as after a recent read, and
    // the due deadline is the only judge: nothing at exactly T_gap, one event 1 ms
    // later.
    let mut bed = TickBed::owner();
    let hub = bed.store_open_drive().await;
    let w0 = CORE_W0;
    let id = core_id(1);
    let snapshot = bed.store.collector_lifecycle_snapshot(hub).unwrap();
    assert!(snapshot.drive_open);
    assert_eq!(snapshot.last_stored_obs_ms, Some(w0));
    bed.scheduler.set_lifecycle_snapshot(id, &snapshot);
    assert!(!bed.scheduler.vehicles[&id].lifecycle_stale);
    bed.advance(Duration::from_millis(600_000));
    assert_eq!(bed.wall(), w0 + 600_000);
    assert!(
        bed.no_data_tick().await.is_empty(),
        "no event at exactly T_gap"
    );
    bed.advance(Duration::from_millis(1));
    let planned = bed.no_data_tick().await;
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].observed_at_ms, w0 + 600_001);
}

#[tokio::test]
async fn a_failed_commit_plans_the_no_data_event_again_and_a_later_observation_clears_the_mark() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let id = core_id(1);
            let observation = bed.last_observation_id(hub);
            bed.advance(Duration::from_millis(600_001));
            // The commit fails: the event is not stored and the car is not marked.
            seam.set_tick_commit_fault(true);
            assert_eq!(bed.no_data_tick().await.len(), 1);
            seam.set_tick_commit_fault(false);
            assert_eq!(bed.last_observation_id(hub), observation);
            assert!(!bed.scheduler.vehicles[&id].force_closed_at_gap);
            assert!(bed.no_data_rows().is_empty(), "the failed commit stored no event");
            // The store waits (R6); after the wait the event is planned again and
            // stored.
            bed.advance(Duration::from_secs(300));
            assert_eq!(bed.no_data_tick().await.len(), 1);
            assert_eq!(bed.last_observation_id(hub), observation + 1);
            assert!(bed.scheduler.vehicles[&id].force_closed_at_gap);
            assert_eq!(bed.no_data_rows().len(), 1, "the retry stored one event");
            // The next stored observation other than a Hub event clears the mark.
            bed.scheduler.note_observation_stored(id, bed.wall());
            assert!(!bed.scheduler.vehicles[&id].force_closed_at_gap);
        })
        .await;
}

/// Run CT61's common course: an open drive whose last stored observation was
/// received at w0, a state set by `stop` at w0 + 1 min, and the event at
/// w0 + 600,001 ms.
async fn no_data_during(mode: crate::config::CollectorMode, stop: impl FnOnce(&mut TickBed)) {
    let mut bed = TickBed::new(mode);
    let hub = bed.store_open_drive().await;
    let id = core_id(1);
    let w0 = CORE_W0;
    let observation = bed.last_observation_id(hub);
    bed.advance(Duration::from_secs(60));
    stop(&mut bed);
    // Nothing is requested: due reads, state checks and the list are all held.
    let now = bed.now();
    assert!(!bed.scheduler.requests_allowed(now));
    assert!(bed.scheduler.due_vehicles(now).is_empty());
    assert!(bed.scheduler.due_offline_state_vehicles(now).is_empty());
    // At w0 + 600,000 ms there is no event yet; at 600,001 ms there is one.
    bed.advance(Duration::from_millis(600_000 - 60_000));
    assert!(bed.no_data_tick().await.is_empty());
    bed.advance(Duration::from_millis(1));
    let planned = bed.no_data_tick().await;
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].observed_at_ms, w0 + 600_001);
    assert_eq!(bed.last_observation_id(hub), observation + 1);
    let drives = bed.store.materialised_history(hub).unwrap().drives;
    assert_eq!(drives.len(), 1);
    assert_eq!(drives[0].end_date_ms, w0, "the drive ends at its last fold");
    assert_eq!(bed.hub_event_current_rows(), 0);
    let _ = id;
}

#[tokio::test]
async fn ct61_the_event_is_stored_while_the_cap_has_stopped_scheduled_polling() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            no_data_during(crate::config::CollectorMode::FleetPolling, |bed| {
                bed.scheduler.account.state = AccountState::CapReached;
                bed.scheduler.account.state_reason = "cap_reached";
                bed.scheduler.vehicles.get_mut(&core_id(1)).unwrap().phase = PollPhase::Driving;
            })
            .await;
        })
        .await;
}

#[tokio::test]
async fn ct61_the_event_is_stored_during_a_rate_limit_wait() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            no_data_during(crate::config::CollectorMode::Legacy, |bed| {
                // 429 with `Retry-After: 3600` at w0 + 1 min.
                let until = bed.now() + Duration::from_secs(3_600);
                bed.scheduler.account.wait = Some((until, WaitReason::RateLimited));
                bed.scheduler.vehicles.get_mut(&core_id(1)).unwrap().phase = PollPhase::Driving;
            })
            .await;
        })
        .await;
}

#[tokio::test]
async fn ct61_the_event_is_stored_for_a_driving_car_whose_reads_fail_with_back_off() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            // Phase Driving, no hold, no failure count to speak of, account
            // Collecting, no observation stored (the stream is silent and the reads
            // answer 503 with back-off).
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let id = core_id(1);
            let w0 = CORE_W0;
            let observation = bed.last_observation_id(hub);
            bed.scheduler.vehicles.get_mut(&id).unwrap().phase = PollPhase::Driving;
            bed.scheduler.vehicles.get_mut(&id).unwrap().next_read_due = Some(bed.now());
            let error = CollectorError::OwnerApi(OwnerApiError::HttpStatus(503));
            // Reads fail every back-off step until the event is due.
            let mut failures = 0;
            let end = Duration::from_millis(600_001);
            let mut elapsed = Duration::ZERO;
            while elapsed < end {
                let now = bed.now();
                let due = bed.scheduler.due_vehicles(now);
                if !due.is_empty() {
                    bed.scheduler.vehicle_failed_for_error(id, &error, now);
                    failures += 1;
                }
                let step = Duration::from_secs(10).min(end - elapsed);
                bed.advance(step);
                elapsed += step;
            }
            assert!(failures > 3, "{failures}");
            assert!(
                bed.scheduler.vehicles[&id].consecutive_failures < K_PAUSE_FAILURES_HINT,
                "the car is not paused: the count stays below the pause threshold"
            );
            assert_eq!(bed.scheduler.vehicles[&id].hold, VehicleHold::None);
            assert_eq!(bed.scheduler.vehicles[&id].phase, PollPhase::Driving);
            let planned = bed.no_data_tick().await;
            assert_eq!(planned.len(), 1);
            assert_eq!(planned[0].observed_at_ms, w0 + 600_001);
            assert_eq!(bed.last_observation_id(hub), observation + 1);
        })
        .await;
}

#[tokio::test]
async fn r47_a_credential_stop_iteration_applies_owner_actions_opens_its_gap_and_closes_a_lost_drive() {
    // A signed-out account is `CredentialStop`: it sends nothing, but each iteration
    // of the loop still reads the owner's actions and applies them, closes a drive
    // that lost its evidence (R40) and stores the scheduler's rows, the
    // `auth_terminal` gap among them (R8). The loop used to sleep and do none of it.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let observation = bed.last_observation_id(hub);
            let id = core_id(1);
            let now = bed.now();
            bed.scheduler.enter_credential_stop(now);
            bed.scheduler.apply_control_settings(
                &bed.store.configured_tesla_vehicles().expect("configured"),
                now,
            );
            bed.store
                .insert_collector_owner_action("retry_collection", None, bed.wall())
                .expect("retry row");
            // Past `T_gap` without evidence.
            bed.advance(Duration::from_millis(600_001));
            let mut next_status_refresh = bed.now();
            let mut owner_actions = Vec::new();
            let auth = CollectionAuth::Legacy {
                manager: Arc::new(tokio::sync::Mutex::new(
                    LegacyAuthManager::for_test_with_active_pair(
                        crate::legacy_auth::LegacyAuth::for_test(
                            url::Url::parse("https://auth.tesla.com/oauth2/v3").unwrap(),
                            "access-secret",
                            "refresh-secret",
                        ),
                    )
                    .expect("active test pair"),
                )),
                fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
                refresh: Arc::new(LegacyRefreshCoordinator::default()),
                allow_refresh: true,
                region: StreamRegion::Global,
            };
            let wait = run_credential_stop_iteration(
                &bed.store,
                &mut bed.scheduler,
                &mut bed.identities,
                &bed.cursor_key,
                &bed.terrain,
                Duration::from_secs(600),
                &mut next_status_refresh,
                &mut owner_actions,
                &auth,
            )
            .await
            .expect("an iteration of the stop");
            // The loop waits at most one settings refresh.
            assert!(wait <= CONTROL_SETTINGS_REFRESH, "{wait:?}");
            // The drive that lost its evidence is closed by a Hub event.
            assert_eq!(bed.last_observation_id(hub), observation + 1);
            assert!(bed.scheduler.vehicles[&id].force_closed_at_gap);
            // The owner's action is read at the settings refresh and applied.
            let applied: i64 = bed.query(
                "SELECT COUNT(*) FROM collector_owner_actions WHERE applied_at_ms IS NOT NULL",
            );
            assert_eq!(applied, 1);
            // The stop is stored with its gap kind, and still stands.
            let gaps = bed.store.collection_gaps(10).expect("gaps");
            assert!(
                gaps.iter()
                    .any(|gap| gap.kind == "auth_terminal" && gap.ended_at_ms.is_none()),
                "{gaps:?}"
            );
            assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
        })
        .await;
}

/// A restart in an account state that sends no list (C3 R7 step 6, R40): the
/// scheduler is rebuilt from the store, and the first settings refresh gives every
/// configured car its record, so the drive that lost its evidence is closed by a Hub
/// event with no list ever sent, and the status shows the car.
async fn no_data_after_a_restart_in(
    mode: crate::config::CollectorMode,
    state: AccountState,
    reason: &'static str,
) {
    let mut bed = TickBed::new(mode);
    let hub = bed.store_open_drive().await;
    let observation = bed.last_observation_id(hub);
    // The stop is committed with the tick's rows, as the loop does.
    bed.scheduler.account.state = state;
    bed.scheduler.account.state_reason = reason;
    bed.scheduler.begin_tick();
    bed.commit(TickBatch::default()).await;
    // The process restarts: a new scheduler loads what the store holds.
    let now = bed.now();
    let mut restarted = VehicleScheduler::new_with(
        core_cadence(),
        mode,
        core_cadence().fleet_policy(),
        now,
        bed.clock.clone(),
        FixedJitter::constant(0.0),
    );
    restarted.config_fingerprint = DS_FP_A.to_owned();
    let configured = bed.store.configured_tesla_vehicles().expect("configured");
    restarted.load(
        &CollectorStateSnapshot::from_rows(
            bed.store.load_collector_state_rows().expect("rows"),
            DS_FP_A.to_owned(),
            &configured,
        ),
        now,
    );
    assert_eq!(restarted.account.state, state, "the stop is restored");
    bed.scheduler = restarted;
    bed.identities = IdentityCache::default();
    // No list is sent in this state; the first settings refresh builds the record.
    bed.scheduler.apply_control_settings(&configured, now);
    assert!(!bed.scheduler.discovery_due(now));
    assert_eq!(bed.scheduler.status(now).vehicles.len(), 1);
    // `T_gap` after the last evidence the Hub event closes the drive.
    bed.advance(Duration::from_millis(600_001));
    let planned = bed.no_data_tick().await;
    assert_eq!(planned.len(), 1, "{state:?}");
    assert_eq!(bed.last_observation_id(hub), observation + 1);
    assert!(
        !bed.store
            .collector_lifecycle_snapshot(hub)
            .unwrap()
            .drive_open
    );
}

#[tokio::test]
async fn ct61_a_restart_while_fleet_access_is_refused_still_closes_the_drive() {
    // (A stored cap stop is lifted at the start when the counters of the month allow
    // a request, R27, so it is not a state a restart keeps for an idle month.)
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            no_data_after_a_restart_in(
                crate::config::CollectorMode::FleetPolling,
                AccountState::FleetAccessRefused,
                "fleet_access_refused",
            )
            .await;
        })
        .await;
}

#[tokio::test]
async fn ct61_a_restart_while_the_owner_api_is_refused_still_closes_the_drive() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            no_data_after_a_restart_in(
                crate::config::CollectorMode::Legacy,
                AccountState::OwnerApiRefused,
                "owner_api_refused",
            )
            .await;
        })
        .await;
}

#[tokio::test]
async fn ct61_a_car_that_left_the_tesla_account_still_gets_its_drive_closed() {
    // R40 applies whatever the car's hold: a Removed car makes no evidence either,
    // and a Hub event is not a request. (A Disabled car takes it too: CT57.)
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let observation = bed.last_observation_id(hub);
            let id = core_id(1);
            {
                let car = bed.scheduler.vehicles.get_mut(&id).unwrap();
                car.hold = VehicleHold::Removed;
                car.reason = "vehicle_removed";
            }
            bed.advance(Duration::from_millis(600_001));
            let planned = bed.no_data_tick().await;
            assert_eq!(planned.len(), 1);
            assert_eq!(bed.last_observation_id(hub), observation + 1);
            // The loop's waiting rule and the plan agree: no second event is due.
            let car = &bed.scheduler.vehicles[&id];
            assert_eq!(bed.scheduler.no_data_deadline_ms(car), None);
        })
        .await;
}

/// One position stored at the bed's wall time through the tick transaction, as a
/// read would be (a later observation for the car).
async fn store_a_position_now(bed: &mut TickBed, latitude: f64, odometer: f64) {
    let now_ms = bed.wall();
    let vehicle = bed.scheduler.vehicles[&core_id(1)].vehicle.clone();
    let now = bed.now();
    bed.scheduler.begin_tick();
    let batch = TickBatch {
        snapshots: vec![SnapshotItem {
            vehicle,
            snapshot: VehicleData::for_test(
                1,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 20,
                        "latitude": latitude,
                        "longitude": 19.0,
                        "timestamp": now_ms
                    },
                    "vehicle_state": {"odometer": odometer}
                }),
            ),
            received_at_ms: now_ms,
        }],
        first_result_at: Some(now),
        ..TickBatch::default()
    };
    bed.commit(batch).await;
}

/// The settings rows a settings refresh reads for car 1, with `enabled` as given.
fn ct57_configured(hub: Uuid, enabled: bool) -> Vec<(Uuid, i64, crate::hub_pack::ProjectionCarSettings)> {
    let settings = crate::hub_pack::ProjectionCarSettings {
        enabled,
        ..crate::hub_pack::ProjectionCarSettings::default()
    };
    vec![(hub, 1, settings)]
}

#[tokio::test]
async fn ct57_a_car_disabled_with_an_open_drive_gets_one_no_data_event_and_nothing_else() {
    // R40 and R62 (B2 clarification 2026-10-12): vehicle 1 is Driving with an open
    // drive and its last stored observation received at w0; `enabled = false` at
    // w0 + 1 min. Afterwards no request, no discovery and no stored frame; at
    // w0 + 600,000 no event; at w0 + 600,001 exactly one `no_data` Hub event, no
    // current row, and the drive ends at w0 (its last fold); no further event.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let id = core_id(1);
            let w0 = CORE_W0;
            bed.scheduler.vehicles.get_mut(&id).unwrap().phase = PollPhase::Driving;
            let observation = bed.last_observation_id(hub);
            bed.advance(Duration::from_secs(60));
            let disconnect = bed
                .scheduler
                .apply_control_settings(&ct57_configured(hub, false), bed.now());
            assert_eq!(disconnect, vec![id]);
            assert_eq!(bed.scheduler.vehicles[&id].hold, VehicleHold::Disabled);
            // The hold is a persisted field: one tick stores it first.
            assert!(bed.no_data_tick().await.is_empty());
            let commits = seam.tick_commits();
            // Until w0 + 600,000 ms nothing is requested, discovered or stored.
            for step in (65_000i64..=600_000).step_by(5_000) {
                let behind = u64::try_from(w0 + step - bed.wall()).expect("forward");
                bed.advance(Duration::from_millis(behind));
                let now = bed.now();
                assert!(bed.scheduler.due_vehicles(now).is_empty());
                assert!(bed.scheduler.due_probe_vehicles(now).is_empty());
                assert!(bed.scheduler.due_offline_state_vehicles(now).is_empty());
                assert!(
                    !bed.scheduler
                        .should_persist_stream_telemetry(id, Some(5), now),
                    "no frame is stored"
                );
                assert!(bed.no_data_tick().await.is_empty(), "no event at {step} ms");
            }
            assert_eq!(bed.wall(), w0 + 600_000);
            assert_eq!(bed.last_observation_id(hub), observation);
            assert_eq!(seam.tick_commits(), commits);
            bed.advance(Duration::from_millis(1));
            let planned = bed.no_data_tick().await;
            assert_eq!(planned.len(), 1, "exactly one event");
            assert_eq!(planned[0].event, "no_data");
            assert_eq!(planned[0].observed_at_ms, w0 + 600_001);
            assert_eq!(bed.last_observation_id(hub), observation + 1);
            assert_eq!(bed.hub_event_current_rows(), 0);
            // The loop's wait and the plan use one predicate (R11): nothing more is
            // due for the car.
            let car = &bed.scheduler.vehicles[&id];
            assert_eq!(bed.scheduler.no_data_deadline_ms(car), None);
            let drives = bed.store.materialised_history(hub).unwrap().drives;
            assert_eq!(drives.len(), 1);
            assert_eq!(drives[0].end_date_ms, w0, "the drive ends at its last fold");
            assert!(
                !bed.store
                    .collector_lifecycle_snapshot(hub)
                    .unwrap()
                    .drive_open
            );
            // Nothing else was stored for it and no further event follows.
            for minutes_later in [11u64, 20] {
                let target =
                    Duration::from_secs(minutes_later * 60) - Duration::from_millis(600_001);
                bed.advance(target);
                assert!(bed.no_data_tick().await.is_empty());
            }
            assert_eq!(bed.last_observation_id(hub), observation + 1);
            assert_eq!(seam.tick_commits(), commits + 1);
        })
        .await;
}

#[tokio::test]
async fn ct57_a_car_re_enabled_inside_t_gap_with_a_later_read_gets_no_event_and_keeps_one_drive() {
    // The variant of CT57: re-enabled at w0 + 4 min with a read stored at w0 + 5 min,
    // which continues the drive (C1 R43 item 4 (g)): no event, one drive.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let id = core_id(1);
            let w0 = CORE_W0;
            bed.scheduler.vehicles.get_mut(&id).unwrap().phase = PollPhase::Driving;
            bed.advance(Duration::from_secs(60));
            bed.scheduler
                .apply_control_settings(&ct57_configured(hub, false), bed.now());
            bed.advance(Duration::from_secs(3 * 60));
            assert_eq!(bed.wall(), w0 + 4 * 60_000);
            assert!(bed.no_data_tick().await.is_empty());
            bed.scheduler
                .apply_control_settings(&ct57_configured(hub, true), bed.now());
            assert_eq!(bed.scheduler.vehicles[&id].hold, VehicleHold::None);
            bed.advance(Duration::from_secs(60));
            store_a_position_now(&mut bed, 47.52, 1000.2).await;
            assert_eq!(bed.scheduler.vehicles[&id].last_stored_obs_ms, Some(w0 + 5 * 60_000));
            // The first moment an event would have been due, and well after.
            bed.advance(Duration::from_millis(5 * 60_000 + 1));
            assert!(bed.no_data_tick().await.is_empty(), "the read moved the evidence");
            assert!(
                bed.store
                    .collector_lifecycle_snapshot(hub)
                    .unwrap()
                    .drive_open,
                "the drive is still open: not closed and not split"
            );
            // It ends once, `T_gap` after the read of w0 + 5 min, at that read.
            bed.advance(Duration::from_millis(5 * 60_000));
            assert_eq!(bed.no_data_tick().await.len(), 1);
            let drives = bed.store.materialised_history(hub).unwrap().drives;
            assert_eq!(drives.len(), 1, "one drive");
            assert_eq!(drives[0].end_date_ms, w0 + 5 * 60_000);
        })
        .await;
}

#[tokio::test]
async fn a_run_that_ends_on_the_loss_of_sensitive_access_stores_nothing_and_any_other_end_keeps_the_tick() {
    // C3 R47 (B2 clarification): the loss of the process's sensitive access writes no
    // collector row, not even the snapshots the tick already received; a result that
    // ends the run for another reason keeps them, best effort.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            for (error, kept) in [
                (CollectorError::SensitiveAccessUnavailable, false),
                (CollectorError::OwnerApi(OwnerApiError::Transport), true),
            ] {
                let mut bed = TickBed::owner();
                bed.list(vec![core_vehicle(1, "online"), core_vehicle(2, "online")]).await;
                let now = bed.now();
                let received_at_ms = bed.wall();
                let due = bed.scheduler.due_vehicles(now);
                assert_eq!(due.len(), 2);
                let (first, second) = (due[0], due[1]);
                bed.scheduler.begin_tick();
                let commits = seam.tick_commits();
                // The first car's read was received earlier in the tick and waits for
                // its transaction; the second car's request ends the run.
                let received = VehicleData::for_test(
                    1,
                    json!({"drive_state": {"shift_state": null, "speed": null, "power": 0,
                                           "timestamp": received_at_ms},
                           "charge_state": {"charging_state": "Disconnected"}}),
                );
                let vehicle = bed.scheduler.vehicle_record(first).expect("car");
                let requests: Vec<TickFuture<'_>> = vec![Box::pin(async move {
                    (TickKey::Read(second), Err(CollectorError::OwnerApi(OwnerApiError::Transport)))
                })];
                let mut run = TickRun {
                    scheduler: &mut bed.scheduler,
                    identities: &mut bed.identities,
                    commit: TickCommit {
                        store: &bed.store,
                        cursor_key: &bed.cursor_key,
                        provider: bed.provider,
                        offline_drive_timeout: Duration::from_secs(600),
                    },
                    batch: TickBatch::default(),
                    terrain_wake: &bed.terrain,
                    any_wrote: false,
                };
                run.batch.snapshots.push(SnapshotItem {
                    vehicle,
                    snapshot: received,
                    received_at_ms,
                });
                let ending = std::cell::Cell::new(Some(error));
                let result = drive_requests(&mut run, requests, None, |_run: &mut TickRun<'_>, _key, _result, _now| {
                    Err(ending.take().expect("one ending"))
                })
                .await;
                assert!(result.is_err());
                let stored: i64 = bed.query(
                    "SELECT COUNT(*) FROM current_observations WHERE record_type LIKE '%vehicle_data%'",
                );
                assert_eq!(stored, i64::from(kept), "kept = {kept}");
                assert_eq!(seam.tick_commits() - commits, usize::from(kept));
            }
        })
        .await;
}

/// The pause threshold of R51 (part K6): this test only needs to show that the
/// failures it makes do not reach it. Twelve failures in an hour or more.
const K_PAUSE_FAILURES_HINT: u32 = 12;

#[test]
fn a_disabled_car_gets_the_no_data_event_like_any_other_and_a_car_without_a_view_loads_one() {
    // R40 applies whatever the car's hold, Disabled included (B2 clarification
    // 2026-10-12): the event collects nothing from Tesla, so R62's "no observations"
    // is not touched.
    let mut core = Core::owner();
    let id = core_id(1);
    let mut vehicle = core_vehicle(1, "online");
    vehicle.settings.enabled = false;
    core.scheduler.accept_discovery(vec![vehicle], core.t0);
    {
        let car = core.scheduler.vehicles.get_mut(&id).unwrap();
        car.hub_id = Some(Uuid::new_v4());
        car.hold = VehicleHold::Disabled;
    }
    // A car without a view is read, Disabled or not.
    assert_eq!(core.scheduler.lifecycle_views_for_no_data(CORE_W0), vec![id]);
    core.scheduler.set_lifecycle_view(
        id,
        LifecycleView {
            last_reach: None,
            drive_open: true,
        },
        Some(CORE_W0 - 3_600_000),
    );
    assert_eq!(core.scheduler.due_lifecycle_events(CORE_W0), vec![id]);
    // The loop's wait counts its deadline too (one predicate for both, R11).
    let car = &core.scheduler.vehicles[&id];
    assert_eq!(
        core.scheduler.no_data_deadline_ms(car),
        Some(CORE_W0 - 3_600_000 + 600_000)
    );
    // An enabled car with no view gets one read at the first iteration; with a fresh
    // view and recent evidence it needs none.
    let mut enabled = Core::owner();
    enabled
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], enabled.t0);
    enabled.scheduler.vehicles.get_mut(&id).unwrap().hub_id = Some(Uuid::new_v4());
    assert_eq!(
        enabled.scheduler.lifecycle_views_for_no_data(CORE_W0),
        vec![id]
    );
    enabled.scheduler.set_lifecycle_view(
        id,
        LifecycleView {
            last_reach: Some("online".to_owned()),
            drive_open: false,
        },
        Some(CORE_W0),
    );
    assert!(
        enabled
            .scheduler
            .lifecycle_views_for_no_data(CORE_W0 + 3_600_000)
            .is_empty()
    );
    // A stale view is read only once its evidence is older than `T_gap`.
    enabled.scheduler.lifecycle_changed(id);
    assert!(
        enabled
            .scheduler
            .lifecycle_views_for_no_data(CORE_W0 + 600_000)
            .is_empty()
    );
    assert_eq!(
        enabled
            .scheduler
            .lifecycle_views_for_no_data(CORE_W0 + 600_001),
        vec![id]
    );
}

// ---- R11: no due time that has already passed -------------------------------------------------------

#[test]
fn a_passed_restart_time_is_no_due_time_and_does_not_make_the_loop_spin() {
    let mut core = stream_core(crate::config::CollectorMode::Legacy);
    let id = core_id(1);
    core.scheduler.stream_task_ended(id, core.t0);
    // While the delay runs, its end is a due time: the next read (60 s) comes after
    // it (30 s).
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(10)),
        Duration::from_secs(20)
    );
    // After it has passed the loop has nothing due until the read at 60 s.
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(31)),
        Duration::from_secs(29)
    );
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(59)),
        Duration::from_secs(1)
    );
}

#[test]
fn a_held_cars_idle_window_is_no_due_time() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    assert_eq!(core.scheduler.vehicles[&id].suspend_at, Some(core.at(900)));
    core.scheduler.vehicles.get_mut(&id).unwrap().hold =
        VehicleHold::Paused(PauseReason::PersistentFailure);
    core.scheduler.vehicles.get_mut(&id).unwrap().next_probe_due = None;
    // The list of a held car is its state check (30 s); its stale idle window, which
    // no code ends while it is held, must not make the loop wake at once.
    let now = core.at(1_000);
    core.scheduler.account.last_discovery = Some(now);
    assert_eq!(
        core.scheduler.delay_until_next_action(now),
        Duration::from_secs(30)
    );
}
