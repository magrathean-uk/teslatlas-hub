// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C3 part K2 (scheduler core and cadence): CT8 to CT12, CT51,
// CT55 and CT68, plus tests of the seams and rules the part owns (R2, R11, R12,
// R22, backoff_delay). Synthetic data only; no network.

/// The wall time every scheduler test starts at (C3 section 9 notation).
const CORE_W0: i64 = 1_760_000_000_000;

fn core_cadence() -> CollectorCadence {
    crate::config::CollectorConfig::default()
        .cadence()
        .expect("default cadence")
}

struct Core {
    scheduler: VehicleScheduler,
    clock: Arc<ManualClock>,
    t0: Instant,
}

impl Core {
    fn new(mode: crate::config::CollectorMode) -> Self {
        Self::with_fleet(mode, core_cadence().fleet_policy())
    }

    fn with_fleet(mode: crate::config::CollectorMode, fleet: crate::config::FleetPolicy) -> Self {
        let t0 = Instant::now();
        let clock = ManualClock::new(t0, CORE_W0);
        let scheduler = VehicleScheduler::new_with(
            core_cadence(),
            mode,
            fleet,
            t0,
            clock.clone(),
            FixedJitter::constant(0.0),
        );
        Self {
            scheduler,
            clock,
            t0,
        }
    }

    fn owner() -> Self {
        Self::new(crate::config::CollectorMode::Legacy)
    }

    fn fleet() -> Self {
        Self::new(crate::config::CollectorMode::FleetPolling)
    }

    fn at(&self, seconds: u64) -> Instant {
        self.t0 + Duration::from_secs(seconds)
    }
}

fn core_vehicle(id: u64, state: &str) -> Vehicle {
    Vehicle::for_test(id, &format!("VIN{id:014}"), state)
}

fn core_id(id: u64) -> VehicleId {
    VehicleId::from_test(id)
}

/// The parked snapshot of section 9.
fn parked_fields() -> Value {
    json!({
        "drive_state": {"shift_state": null, "speed": null, "power": 0},
        "charge_state": {"charging_state": "Disconnected"},
        "climate_state": {"is_climate_on": false, "climate_keeper_mode": "off"},
        "vehicle_state": {
            "locked": true,
            "is_user_present": false,
            "sentry_mode": false,
            "remote_start": false,
            "software_update": {"status": ""}
        }
    })
}

/// The parked snapshot with one field of one group replaced.
fn snapshot_with(changes: &[(&str, &str, Value)]) -> VehicleData {
    let mut fields = parked_fields();
    for (group, key, value) in changes {
        fields[*group][*key] = value.clone();
    }
    VehicleData::for_test(1, fields)
}

// ---- CT8 -----------------------------------------------------------------

#[test]
fn ct8_first_contact_online_is_due_at_once() {
    let mut core = Core::owner();
    let candidates = core
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    assert_eq!(candidates, vec![core_vehicle(1, "online")]);
    assert_eq!(core.scheduler.due_vehicles(core.t0), vec![core_id(1)]);
    assert_eq!(
        core.scheduler.vehicles[&core_id(1)].phase,
        PollPhase::Unknown
    );
}

#[test]
fn ct8_first_contact_asleep_waits_for_the_state_check() {
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    assert!(core.scheduler.due_vehicles(core.t0).is_empty());
    let almost = core.t0 + Duration::from_millis(29_999);
    assert!(core.scheduler.due_vehicles(almost).is_empty());
    assert!(!core.scheduler.discovery_due(almost));
    assert!(core.scheduler.discovery_due(core.at(30)));
    assert_eq!(
        core.scheduler.vehicles[&core_id(1)].phase,
        PollPhase::Asleep
    );
}

#[test]
fn first_list_is_due_at_start_and_a_second_online_list_does_not_read_twice() {
    let mut core = Core::owner();
    assert!(core.scheduler.discovery_due(core.t0));
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    // The first read is in flight; a second list saying online must not make a
    // second first-contact read.
    assert_eq!(core.scheduler.due_vehicles(core.t0), vec![core_id(1)]);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.at(30));
    assert!(core.scheduler.due_vehicles(core.at(30)).is_empty());
}

#[test]
fn list_states_move_phases_as_table_7_1() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    // A driving car that the list reports offline is offline from a session.
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, core.t0);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "offline")], core.at(10));
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Offline);
    assert!(record.offline_from_session);
    assert_eq!(record.next_read_due, None);
    // Asleep from any phase.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.at(40));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Asleep);
}

#[test]
fn an_install_that_reports_offline_is_unchanged_but_asleep_moves_it() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Updating, true, core.t0);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "offline")], core.at(10));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Updating);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.at(20));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Asleep);
}

#[test]
fn a_disabled_car_is_not_a_discovery_candidate_and_is_never_read() {
    let mut core = Core::owner();
    let mut vehicle = core_vehicle(1, "online");
    vehicle.settings.enabled = false;
    let candidates = core.scheduler.accept_discovery(vec![vehicle], core.t0);
    assert!(candidates.is_empty());
    assert!(core.scheduler.due_vehicles(core.t0).is_empty());
    assert_eq!(
        core.scheduler.vehicles[&core_id(1)].hold,
        VehicleHold::Disabled
    );
}

// ---- CT9 and CT68 ----------------------------------------------------------

#[test]
fn ct9_phase_table() {
    let phase = |changes: &[(&str, &str, Value)]| poll_phase(&snapshot_with(changes));
    assert_eq!(phase(&[]), PollPhase::Online);
    assert_eq!(
        phase(&[("drive_state", "shift_state", json!("D"))]),
        PollPhase::Driving
    );
    assert_eq!(
        phase(&[("drive_state", "speed", json!(5))]),
        PollPhase::Driving
    );
    assert_eq!(
        phase(&[("charge_state", "charging_state", json!("Charging"))]),
        PollPhase::Charging
    );
    assert_eq!(
        phase(&[("charge_state", "charging_state", json!("Starting"))]),
        PollPhase::Charging
    );
    for parked in ["Stopped", "NoPower", "Complete", "Disconnected"] {
        assert_eq!(
            phase(&[("charge_state", "charging_state", json!(parked))]),
            PollPhase::Online,
            "{parked}"
        );
    }
    assert_eq!(
        phase(&[(
            "vehicle_state",
            "software_update",
            json!({"status": "installing"})
        )]),
        PollPhase::Updating
    );
    // Drive over install, install over charge.
    assert_eq!(
        phase(&[
            (
                "vehicle_state",
                "software_update",
                json!({"status": "installing"})
            ),
            ("drive_state", "shift_state", json!("D")),
        ]),
        PollPhase::Driving
    );
    assert_eq!(
        phase(&[
            (
                "vehicle_state",
                "software_update",
                json!({"status": "installing"})
            ),
            ("charge_state", "charging_state", json!("Charging")),
        ]),
        PollPhase::Updating
    );
    // Service mode takes the session phases away.
    assert_eq!(
        phase(&[
            ("vehicle_state", "service_mode", json!(true)),
            ("drive_state", "shift_state", json!("D")),
        ]),
        PollPhase::Online
    );
    assert_eq!(
        phase(&[
            ("vehicle_state", "service_mode", json!(true)),
            ("charge_state", "charging_state", json!("Charging")),
        ]),
        PollPhase::Online
    );
}

#[test]
fn ct9_enum_spellings_follow_c1_r40a() {
    let phase = |changes: &[(&str, &str, Value)]| poll_phase(&snapshot_with(changes));
    // C1 R40a strips one of the prefixes DetailedChargeState, ChargeState and
    // ShiftState, ignoring ASCII case, and trims.
    for spelling in [
        "ChargeStateCharging",
        "DetailedChargeStateCharging",
        "  charging ",
        "CHARGING",
    ] {
        assert_eq!(
            phase(&[("charge_state", "charging_state", json!(spelling))]),
            PollPhase::Charging,
            "{spelling:?}"
        );
    }
    for spelling in ["ShiftStateD", " d ", "shiftstater", "n"] {
        assert_eq!(
            phase(&[("drive_state", "shift_state", json!(spelling))]),
            PollPhase::Driving,
            "{spelling:?}"
        );
    }
    // CT9 as written lists "ChargingStateCharging". That is not one of C1
    // R40a's prefixes, so the text names no charging state and R15, which
    // normalises by R40a, gives Online. The spec row is reported as a defect.
    assert_eq!(
        phase(&[(
            "charge_state",
            "charging_state",
            json!("ChargingStateCharging")
        )]),
        PollPhase::Online
    );
}

#[test]
fn ct9_gear_and_speed_rules() {
    let phase = |changes: &[(&str, &str, Value)]| poll_phase(&snapshot_with(changes));
    // Gear P is a statement whatever the speed.
    assert_eq!(
        phase(&[
            ("drive_state", "shift_state", json!("P")),
            ("drive_state", "speed", json!(5)),
        ]),
        PollPhase::Online
    );
    // Words that are not gears leave the speed to decide.
    assert_eq!(
        phase(&[
            ("drive_state", "shift_state", json!("SNA")),
            ("drive_state", "speed", json!(5)),
        ]),
        PollPhase::Driving
    );
    assert_eq!(
        phase(&[("drive_state", "speed", json!(0))]),
        PollPhase::Online
    );
    // Neutral and reverse are drive gears.
    for gear in ["N", "R"] {
        assert_eq!(
            phase(&[("drive_state", "shift_state", json!(gear))]),
            PollPhase::Driving,
            "{gear}"
        );
    }
    // The install status is trimmed and compared ignoring case.
    assert_eq!(
        phase(&[(
            "vehicle_state",
            "software_update",
            json!({"status": " Installing "})
        )]),
        PollPhase::Updating
    );
    // A field of the wrong JSON type gives no evidence.
    assert_eq!(
        phase(&[("drive_state", "shift_state", json!(4))]),
        PollPhase::Online
    );
    assert_eq!(
        snapshot_service_mode(&snapshot_with(&[(
            "vehicle_state",
            "service_mode",
            json!(true)
        )])),
        Some(true)
    );
    assert_eq!(
        snapshot_service_mode(&snapshot_with(&[(
            "vehicle_state",
            "service_mode",
            json!(false)
        )])),
        Some(false)
    );
    assert_eq!(
        snapshot_service_mode(&snapshot_with(&[(
            "vehicle_state",
            "service_mode",
            json!("true")
        )])),
        None
    );
    assert_eq!(snapshot_service_mode(&snapshot_with(&[])), None);
    // An empty snapshot is online.
    assert_eq!(
        poll_phase(&VehicleData::for_test(1, json!({}))),
        PollPhase::Online
    );
}

#[test]
fn ct68_phase_conflict_follows_the_later_timestamp() {
    let drive_t = 1_760_000_000_000_i64;
    let phase = |gear: &str, charging: &str, drive: Option<Value>, charge: Option<Value>| {
        let mut changes = vec![
            ("drive_state", "shift_state", json!(gear)),
            ("charge_state", "charging_state", json!(charging)),
        ];
        if let Some(value) = drive {
            changes.push(("drive_state", "timestamp", value));
        }
        if let Some(value) = charge {
            changes.push(("charge_state", "timestamp", value));
        }
        poll_phase(&snapshot_with(&changes))
    };
    // The later of the two statements wins.
    assert_eq!(
        phase(
            "D",
            "Charging",
            Some(json!(drive_t)),
            Some(json!(drive_t + 5_000))
        ),
        PollPhase::Charging
    );
    assert_eq!(
        phase(
            "D",
            "Charging",
            Some(json!(drive_t + 5_000)),
            Some(json!(drive_t))
        ),
        PollPhase::Driving
    );
    // Motion wins a tie.
    assert_eq!(
        phase("D", "Charging", Some(json!(drive_t)), Some(json!(drive_t))),
        PollPhase::Driving
    );
    // A later `Complete` is plugged evidence that discards the motion, and it is
    // not a charging state, so the car is parked.
    assert_eq!(
        phase(
            "D",
            "Complete",
            Some(json!(drive_t)),
            Some(json!(drive_t + 1_000))
        ),
        PollPhase::Online
    );
    // Without timestamps nothing is later, so Motion survives.
    assert_eq!(phase("D", "Complete", None, None), PollPhase::Driving);
    assert_eq!(phase("D", "Charging", None, None), PollPhase::Driving);
    // One missing timestamp, or one that is not an integer, also leaves Motion.
    assert_eq!(
        phase("D", "Charging", Some(json!(drive_t)), None),
        PollPhase::Driving
    );
    assert_eq!(
        phase(
            "D",
            "Charging",
            Some(json!(drive_t)),
            Some(json!(1.76e12 + 5_000.0))
        ),
        PollPhase::Driving
    );
    // A disconnected cable is not plugged evidence, however late.
    assert_eq!(
        phase(
            "D",
            "Disconnected",
            Some(json!(drive_t)),
            Some(json!(drive_t + 9_000))
        ),
        PollPhase::Driving
    );
}

// ---- CT10 ------------------------------------------------------------------

/// After `vehicle_succeeded` at t0 with `phase` the next read is due `seconds`
/// later (section 9, CT10).
fn assert_next_read(core: &mut Core, phase: PollPhase, stream_healthy: bool, seconds: u64) {
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    if stream_healthy {
        core.scheduler.stream_healthy(id, core.t0);
    }
    core.scheduler.vehicle_succeeded(id, phase, true, core.t0);
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(seconds)),
        "{phase:?} stream_healthy={stream_healthy}"
    );
    assert_eq!(core.scheduler.vehicles[&id].phase, phase);
}

#[test]
fn ct10_owner_wire_intervals() {
    for (phase, healthy, seconds) in [
        (PollPhase::Driving, true, 60),
        (PollPhase::Driving, false, 10),
        (PollPhase::Charging, false, 30),
        (PollPhase::Updating, false, 120),
        (PollPhase::Online, false, 60),
    ] {
        assert_next_read(&mut Core::owner(), phase, healthy, seconds);
    }
}

#[test]
fn ct10_fleet_intervals_never_assume_a_stream() {
    for (phase, healthy, seconds) in [
        (PollPhase::Driving, false, 30),
        // R64: a healthy stream changes nothing in Fleet polling.
        (PollPhase::Driving, true, 30),
        (PollPhase::Charging, false, 120),
        (PollPhase::Online, false, 120),
        (PollPhase::Updating, false, 300),
    ] {
        assert_next_read(&mut Core::fleet(), phase, healthy, seconds);
    }
}

#[test]
fn a_silent_stream_while_driving_falls_back_after_the_silence_limit() {
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
    // A frame 29 s before the next read keeps the stream healthy: the silence
    // outage is at the 30 s mark (R28, CT50), so 29 s is inside the window.
    core.scheduler
        .should_persist_stream_telemetry(id, None, core.at(31));
    core.scheduler.due_vehicles(core.at(60));
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, core.at(60));
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(120))
    );
    // A frame exactly 30 s before is silence: the 10 s fallback applies.
    core.scheduler
        .should_persist_stream_telemetry(id, None, core.at(90));
    core.scheduler.due_vehicles(core.at(120));
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, core.at(120));
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(130)),
        "a stream silent for exactly 30 s is not healthy, so the fallback interval applies"
    );
    assert_eq!(K_DRIVE_FRAME_SILENCE, Duration::from_secs(30));
}

#[test]
fn state_check_and_inventory_intervals_follow_the_mode_and_the_pricing() {
    // Owner wire: an asleep car is checked every 30 s, awake cars only get the
    // inventory list every 300 s.
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    assert!(!core.scheduler.discovery_due(core.at(29)));
    assert!(core.scheduler.discovery_due(core.at(30)));
    let mut awake = Core::owner();
    awake
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], awake.t0);
    awake.scheduler.due_vehicles(awake.t0);
    awake
        .scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, true, awake.t0);
    assert!(!awake.scheduler.discovery_due(awake.at(299)));
    assert!(awake.scheduler.discovery_due(awake.at(300)));
    // A Suspended car uses its own interval.
    let mut suspended = Core::owner();
    suspended
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], suspended.t0);
    suspended
        .scheduler
        .vehicles
        .get_mut(&core_id(1))
        .unwrap()
        .phase = PollPhase::Suspended;
    assert!(suspended.scheduler.discovery_due(suspended.at(30)));

    // Fleet polling, state checks billed: 600 s and 3,600 s.
    let mut billed = Core::fleet();
    billed
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], billed.t0);
    assert!(!billed.scheduler.discovery_due(billed.at(599)));
    assert!(billed.scheduler.discovery_due(billed.at(600)));
    let mut online = Core::fleet();
    online
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], online.t0);
    online.scheduler.due_vehicles(online.t0);
    online
        .scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, true, online.t0);
    assert!(!online.scheduler.discovery_due(online.at(3_599)));
    assert!(online.scheduler.discovery_due(online.at(3_600)));
    // Fleet polling with unbilled state checks: 30 s and 300 s.
    let free = crate::config::FleetPolicy::resolve(
        &core_cadence().fleet,
        crate::config::FleetPricing {
            state: None,
            ..crate::config::FLEET_PRICING
        },
    );
    let mut unbilled = Core::with_fleet(crate::config::CollectorMode::FleetPolling, free);
    unbilled
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], unbilled.t0);
    assert!(!unbilled.scheduler.discovery_due(unbilled.at(29)));
    assert!(unbilled.scheduler.discovery_due(unbilled.at(30)));
}

// ---- CT11 ------------------------------------------------------------------

#[test]
fn ct11_a_phase_change_shortens_the_next_read() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, core.t0);
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(60))
    );
    core.scheduler
        .schedule_stream_charging_poll(id, Some("D"), Some(5), core.at(5));
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Driving);
    // K_MIN_READ_SPACING (10 s) after the read at t0, not 60 s.
    assert_eq!(record.next_read_due, Some(core.at(10)));
    assert!(core.scheduler.due_vehicles(core.at(9)).is_empty());
    assert_eq!(core.scheduler.due_vehicles(core.at(10)), vec![id]);
}

#[test]
fn stream_motion_in_fleet_polling_uses_the_fleet_spacing() {
    let mut core = Core::fleet();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, core.t0);
    core.scheduler
        .schedule_stream_charging_poll(id, Some("D"), None, core.at(5));
    // K_MIN_READ_SPACING is the mode's fastest Driving interval: 30 s.
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(30))
    );
}

#[test]
fn motion_frames_while_driving_keep_the_long_stream_interval() {
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
    for second in 1..30 {
        assert!(
            core.scheduler
                .should_persist_stream_telemetry(id, Some(10), core.at(second))
        );
        core.scheduler
            .schedule_stream_charging_poll(id, Some("D"), Some(10), core.at(second));
    }
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(60)),
        "frames of a Driving car must not make reads due"
    );
    assert_eq!(core.scheduler.vehicles[&id].last_in_use, Some(core.at(29)));
}

#[test]
fn a_frame_of_a_disabled_or_unknown_car_is_not_stored() {
    let mut core = Core::owner();
    let mut vehicle = core_vehicle(1, "online");
    vehicle.settings.enabled = false;
    core.scheduler.accept_discovery(vec![vehicle], core.t0);
    assert!(
        !core
            .scheduler
            .should_persist_stream_telemetry(core_id(1), None, core.t0)
    );
    assert!(
        !core
            .scheduler
            .should_persist_stream_telemetry(core_id(9), None, core.t0)
    );
}

#[test]
fn a_stream_outage_is_reported_once_and_recovery_once() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler.stream_healthy(id, core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, core.t0);
    assert_eq!(
        core.scheduler.stream_healthy(id, core.at(1)),
        StreamRecovery::Unchanged
    );
    let StreamOutage::Active(status) = core.scheduler.stream_unhealthy(id, core.at(5)) else {
        panic!("the first unhealthy event reports the outage");
    };
    assert_eq!(status.consecutive_failures, 1);
    assert_eq!(status.phase, PollPhase::Driving);
    assert!(status.owner_api_fallback_scheduled);
    assert_eq!(
        status.owner_api_fallback_after,
        Some(Duration::from_secs(10))
    );
    assert!(!status.suspended);
    assert_eq!(
        core.scheduler.stream_unhealthy(id, core.at(6)),
        StreamOutage::Ignored
    );
    // The fallback read is due 10 s after the last read, not 60 s.
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(core.at(10))
    );
    assert!(!core.scheduler.has_due_stream_fallback(core.at(9)));
    assert!(core.scheduler.has_due_stream_fallback(core.at(10)));
    assert_eq!(
        core.scheduler.stream_healthy(id, core.at(20)),
        StreamRecovery::Recovered(StreamRecoveryStatus {
            failures: 2,
            outage_duration: Duration::from_secs(15),
        })
    );
    assert_eq!(
        core.scheduler.stream_healthy(id, core.at(21)),
        StreamRecovery::Unchanged
    );
}

// ---- CT12 and R12 ----------------------------------------------------------

#[test]
fn ct12_many_cars_run_four_at_a_time_without_waiting() {
    let mut core = Core::owner();
    let vehicles: Vec<Vehicle> = (1..=10).map(|id| core_vehicle(id, "online")).collect();
    core.scheduler.accept_discovery(vehicles, core.t0);
    let in_flight = |core: &Core| {
        core.scheduler
            .vehicles
            .values()
            .filter(|record| record.in_flight)
            .count()
    };
    let mut ticks = Vec::new();
    for _ in 0..3 {
        // The next tick starts at once while work remains.
        assert_eq!(
            core.scheduler.delay_until_next_action(core.t0),
            Duration::ZERO
        );
        let tick = core.scheduler.due_vehicles(core.t0);
        assert!(in_flight(&core) <= K_MAX_IN_FLIGHT);
        for id in &tick {
            core.scheduler
                .vehicle_succeeded(*id, PollPhase::Online, true, core.t0);
        }
        ticks.push(tick.iter().map(|id| id.get()).collect::<Vec<_>>());
    }
    assert_eq!(ticks, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8], vec![9, 10]]);
    assert!(core.scheduler.due_vehicles(core.t0).is_empty());
}

#[test]
fn the_in_flight_cap_holds_across_calls_until_results_arrive() {
    let mut core = Core::owner();
    let vehicles: Vec<Vehicle> = (1..=6).map(|id| core_vehicle(id, "online")).collect();
    core.scheduler.accept_discovery(vehicles, core.t0);
    assert_eq!(core.scheduler.due_vehicles(core.t0).len(), 4);
    assert!(core.scheduler.due_vehicles(core.t0).is_empty());
    core.scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, true, core.t0);
    // One slot is free again, and the earliest due car that is not in flight
    // takes it.
    assert_eq!(core.scheduler.due_vehicles(core.t0), vec![core_id(5)]);
}

#[test]
fn due_work_is_ordered_by_due_instant_then_id() {
    let mut core = Core::owner();
    let vehicles: Vec<Vehicle> = (1..=3).map(|id| core_vehicle(id, "online")).collect();
    core.scheduler.accept_discovery(vehicles, core.t0);
    // Car 3 became due earlier than cars 1 and 2.
    core.scheduler
        .vehicles
        .get_mut(&core_id(3))
        .unwrap()
        .next_read_due = Some(core.at(0));
    core.scheduler
        .vehicles
        .get_mut(&core_id(1))
        .unwrap()
        .next_read_due = Some(core.at(2));
    core.scheduler
        .vehicles
        .get_mut(&core_id(2))
        .unwrap()
        .next_read_due = Some(core.at(2));
    assert_eq!(
        core.scheduler.due_vehicles(core.at(5)),
        vec![core_id(3), core_id(1), core_id(2)]
    );
}

#[test]
fn one_cars_failure_does_not_delay_another_car() {
    let mut core = Core::owner();
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "online")],
        core.t0,
    );
    assert_eq!(core.scheduler.due_vehicles(core.t0).len(), 2);
    core.scheduler.vehicle_failed_for_error(
        core_id(1),
        &CollectorError::OwnerApi(OwnerApiError::HttpStatus(503)),
        core.t0,
    );
    core.scheduler
        .vehicle_succeeded(core_id(2), PollPhase::Online, true, core.t0);
    assert_eq!(
        core.scheduler.vehicles[&core_id(2)].next_read_due,
        Some(core.at(60))
    );
    assert!(core.scheduler.vehicles[&core_id(1)].next_read_due > Some(core.at(0)));
}

// ---- R22 and the back-off function -------------------------------------------

#[test]
fn a_failure_never_makes_a_car_more_frequent_than_its_cadence() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    // The first read shows use, so the idle window of part K4 (C3 R31) does not
    // suspend the car during this test's long steps.
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, core.t0);
    let error = CollectorError::OwnerApi(OwnerApiError::HttpStatus(503));
    // Failures 1 to 4: nominal 10, 20, 40, 80 s, half with u = 0, so the Online
    // interval of 60 s holds until the back-off exceeds it.
    let mut now = core.t0;
    let mut waits = Vec::new();
    for _ in 0..6 {
        now += Duration::from_secs(1_000);
        core.scheduler.due_vehicles(now);
        core.scheduler.vehicle_failed_for_error(id, &error, now);
        let due = core.scheduler.vehicles[&id].next_read_due.expect("retry");
        waits.push(due.saturating_duration_since(now).as_secs());
    }
    assert_eq!(waits, vec![60, 60, 60, 60, 80, 150]);
    assert_eq!(core.scheduler.vehicles[&id].consecutive_failures, 6);
    // A success resets the count and returns to the cadence.
    now += Duration::from_secs(1_000);
    core.scheduler.due_vehicles(now);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, now);
    assert_eq!(core.scheduler.vehicles[&id].consecutive_failures, 0);
    assert_eq!(core.scheduler.vehicles[&id].first_failure, None);
    assert_eq!(
        core.scheduler.vehicles[&id].next_read_due,
        Some(now + Duration::from_secs(60))
    );
}

#[test]
fn backoff_is_exponential_with_equal_jitter_and_a_cap() {
    let cap = Duration::from_secs(300);
    let secs = |failures: u32, u: f64| backoff_delay(failures, cap, u).as_secs_f64();
    // Nominal 10 s at the first failure, and failures = 0 gives the same.
    assert_eq!(secs(1, 0.0), 5.0);
    assert_eq!(secs(0, 0.0), 5.0);
    assert_eq!(secs(1, 0.5), 7.5);
    assert_eq!(secs(1, 1.0), 10.0);
    assert_eq!(secs(2, 0.0), 10.0);
    assert_eq!(secs(3, 0.0), 20.0);
    // The cap applies to the nominal value: 300 s nominal from the sixth
    // failure on, so between 150 and 300 s.
    assert_eq!(secs(6, 0.0), 150.0);
    assert_eq!(secs(6, 1.0), 300.0);
    assert_eq!(secs(40, 0.0), 150.0);
    assert_eq!(secs(u32::MAX, 1.0), 300.0);
    // A faulty jitter source cannot panic or exceed the bounds.
    assert_eq!(secs(1, f64::NAN), 5.0);
    assert_eq!(secs(1, -3.0), 5.0);
    assert_eq!(secs(1, 7.0), 10.0);
    // A cap below the base is respected.
    assert_eq!(
        backoff_delay(1, Duration::from_secs(4), 1.0),
        Duration::from_secs(4)
    );
}

#[test]
fn the_os_jitter_stays_in_the_unit_interval_and_varies() {
    let mut jitter = OsJitter::new();
    let draws: Vec<f64> = (0..1_000).map(|_| jitter.draw()).collect();
    assert!(draws.iter().all(|value| (0.0..1.0).contains(value)));
    let distinct: std::collections::HashSet<u64> =
        draws.iter().map(|value| value.to_bits()).collect();
    assert!(distinct.len() > 900);
    let mean = draws.iter().sum::<f64>() / draws.len() as f64;
    assert!((0.4..0.6).contains(&mean), "mean {mean}");
}

#[test]
fn the_failed_list_waits_the_larger_of_the_interval_and_the_back_off() {
    let mut core = Core::owner();
    let error = CollectorError::OwnerApi(OwnerApiError::HttpStatus(503));
    let mut waits = Vec::new();
    let mut now = core.t0;
    for _ in 0..8 {
        let delay = core.scheduler.discovery_failed_for_error(&error, now);
        assert!(
            !core
                .scheduler
                .discovery_due(now + delay - Duration::from_nanos(1))
        );
        assert!(core.scheduler.discovery_due(now + delay));
        waits.push(delay.as_secs());
        now += delay;
    }
    // The state interval (300 s inventory, no cars) is the floor; the back-off
    // grows to 150 s at u = 0 and never reaches it.
    assert_eq!(waits, vec![300; 8]);
    // A success resets the count.
    core.scheduler.accept_discovery(Vec::new(), now);
    assert_eq!(core.scheduler.account.discovery_failures, 0);
}

// ---- R2: clocks -------------------------------------------------------------

#[test]
fn persisted_future_deadlines_load_clamped_to_their_own_length() {
    let t0 = Instant::now();
    let bound = Duration::from_secs(1_800);
    let w = CORE_W0;
    // A deadline 10 minutes ahead loads 10 minutes ahead.
    assert_eq!(
        deadline_from_wall(t0, w, w + 600_000, bound),
        t0 + Duration::from_secs(600)
    );
    // After a backward wall step the stored deadline looks 2 h away; it still
    // loads no further than its bound.
    assert_eq!(
        deadline_from_wall(t0, w - 7_200_000, w + 600_000, bound),
        t0 + bound
    );
    // A forward step only shortens it, and a past deadline loads as now.
    assert_eq!(
        deadline_from_wall(t0, w + 400_000, w + 600_000, bound),
        t0 + Duration::from_secs(200)
    );
    assert_eq!(deadline_from_wall(t0, w + 700_000, w + 600_000, bound), t0);
}

#[test]
fn persisted_past_instants_load_by_a_clamped_age() {
    let t0 = Instant::now() + Duration::from_secs(10_000);
    let bound = Duration::from_secs(1_800);
    let w = CORE_W0;
    // Five minutes old loads five minutes back.
    assert_eq!(
        past_instant_from_wall(t0, w + 300_000, w, bound),
        Some(t0 - Duration::from_secs(300))
    );
    // At or beyond the bound the value no longer matters.
    assert_eq!(past_instant_from_wall(t0, w + 1_800_000, w, bound), None);
    assert_eq!(past_instant_from_wall(t0, w + 9_000_000, w, bound), None);
    // After a backward step the age is clamped to 0, so the limit holds for its
    // full length.
    assert_eq!(
        past_instant_from_wall(t0, w - 3_600_000, w, bound),
        Some(t0)
    );
    // The fallback to `mono_now` when the age cannot be subtracted is not
    // reachable here: the platform's instants accept even absurd ages.
}

#[test]
fn ct51_a_backward_wall_step_changes_no_due_time() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, core.t0);
    let due = core.scheduler.vehicles[&id].next_read_due;
    let list_due = core.scheduler.list_due_at();
    assert_eq!(core.scheduler.observe_clock(core.t0), None);
    core.clock.advance(Duration::from_secs(5 * 60));
    assert_eq!(core.scheduler.observe_clock(core.at(300)), None);
    core.clock.step_wall_ms(-7_200_000);
    assert_eq!(
        core.scheduler.observe_clock(core.at(300)),
        Some(ClockEvent::WallClockSteppedBack)
    );
    // Reported once, and nothing else changed.
    assert_eq!(core.scheduler.observe_clock(core.at(300)), None);
    assert!(core.scheduler.take_tick().is_empty());
    assert_eq!(core.scheduler.vehicles[&id].next_read_due, due);
    assert_eq!(core.scheduler.list_due_at(), list_due);
    // Time then passes normally from the stepped wall clock.
    core.clock.advance(Duration::from_secs(10));
    assert_eq!(core.scheduler.observe_clock(core.at(310)), None);
}

#[test]
fn ct51_a_forward_wall_jump_is_a_host_suspended_gap() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, core.t0);
    let due = core.scheduler.vehicles[&id].next_read_due;
    let w_prev = CORE_W0;
    // The wall clock jumps 2 h while the monotonic clock advances 1 s.
    core.clock.advance_mono(Duration::from_secs(1));
    core.clock.step_wall_ms(7_200_000);
    assert_eq!(
        core.scheduler.observe_clock(core.at(1)),
        Some(ClockEvent::HostSuspended {
            from_wall_ms: w_prev,
            to_wall_ms: w_prev + 7_200_000,
        })
    );
    assert_eq!(
        core.scheduler.take_tick().gaps,
        vec![GapRecord {
            kind: GapKind::HostSuspended,
            vehicle: None,
            started_ms: w_prev,
            ended_ms: Some(w_prev + 7_200_000),
            // The car was Online when the gap began.
            alert: true,
        }]
    );
    // The divergence froze the month anchor of the request counter (C3 R25).
    assert!(core.scheduler.ledger.lock().frozen);
    // No due time moved (none is skipped twice), and nothing more is reported.
    assert_eq!(core.scheduler.vehicles[&id].next_read_due, due);
    assert_eq!(core.scheduler.observe_clock(core.at(1)), None);
    assert!(core.scheduler.take_tick().is_empty());
}

#[test]
fn a_host_suspended_gap_alerts_only_for_awake_cars_or_long_gaps() {
    let mut asleep = Core::owner();
    asleep
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], asleep.t0);
    // 5 minutes with every car asleep is shorter than T_gap (600 s): no alert.
    asleep.clock.advance_mono(Duration::from_secs(1));
    asleep.clock.step_wall_ms(300_000);
    asleep.scheduler.observe_clock(asleep.at(1));
    assert!(!asleep.scheduler.take_tick().gaps[0].alert);
    // 11 minutes is at least T_gap: alert.
    asleep.clock.advance_mono(Duration::from_secs(1));
    asleep.clock.step_wall_ms(660_000);
    asleep.scheduler.observe_clock(asleep.at(2));
    assert!(asleep.scheduler.take_tick().gaps[0].alert);
    // A Suspended car that Tesla reports online counts as awake.
    let mut suspended = Core::owner();
    suspended
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], suspended.t0);
    suspended
        .scheduler
        .vehicles
        .get_mut(&core_id(1))
        .unwrap()
        .phase = PollPhase::Suspended;
    suspended.clock.advance_mono(Duration::from_secs(1));
    suspended.clock.step_wall_ms(120_000);
    suspended.scheduler.observe_clock(suspended.at(1));
    assert!(suspended.scheduler.take_tick().gaps[0].alert);
    // A drift below the divergence bound is not a gap.
    let mut small = Core::owner();
    small.clock.advance_mono(Duration::from_secs(1));
    small.clock.step_wall_ms(59_000);
    assert_eq!(small.scheduler.observe_clock(small.at(1)), None);
}

// ---- R11: waiting and CT55 ------------------------------------------------------

#[test]
fn delay_until_next_action_is_the_earliest_due_time_capped_at_a_minute() {
    let mut core = Core::owner();
    // No list yet: due now.
    assert_eq!(
        core.scheduler.delay_until_next_action(core.t0),
        Duration::ZERO
    );
    core.scheduler.accept_discovery(Vec::new(), core.t0);
    // Nothing due before the inventory list in 300 s, but the loop never sleeps
    // longer than K_MAX_IDLE_WAIT.
    assert_eq!(
        core.scheduler.delay_until_next_action(core.t0),
        K_MAX_IDLE_WAIT
    );
    assert_eq!(K_MAX_IDLE_WAIT, Duration::from_secs(60));
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    assert_eq!(
        core.scheduler.delay_until_next_action(core.t0),
        Duration::from_secs(30)
    );
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(12)),
        Duration::from_secs(18)
    );
    // Past due saturates at zero.
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(40)),
        Duration::ZERO
    );
}

#[test]
fn an_account_wait_blocks_every_request_and_bounds_the_delay_from_below() {
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.account.wait = Some((core.at(20), WaitReason::RateLimited));
    assert!(core.scheduler.due_vehicles(core.at(1)).is_empty());
    assert!(!core.scheduler.discovery_due(core.at(19)));
    assert!(
        core.scheduler
            .due_offline_state_vehicles(core.at(19))
            .is_empty()
    );
    // The read was due at t0, but nothing is sent before the wait ends.
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(1)),
        Duration::from_secs(19)
    );
    assert_eq!(core.scheduler.due_vehicles(core.at(20)), vec![core_id(1)]);
    assert_eq!(
        core.scheduler.status(core.at(1)).wait,
        Some((WaitReason::RateLimited, CORE_W0 + 19_000))
    );
}

#[test]
fn an_owner_action_state_sends_nothing_but_a_probe() {
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.account.state = AccountState::OwnerApiRefused;
    assert!(core.scheduler.due_vehicles(core.at(1)).is_empty());
    assert!(!core.scheduler.discovery_due(core.at(1_000)));
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(1)),
        K_MAX_IDLE_WAIT
    );
    core.scheduler.account.next_billing_probe = Some(core.at(100));
    assert!(core.scheduler.discovery_due(core.at(100)));
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(98)),
        Duration::from_secs(2)
    );
}

#[test]
fn the_loop_wait_has_a_floor_after_an_idle_iteration_and_caps() {
    let mut core = Core::owner();
    core.scheduler.accept_discovery(Vec::new(), core.t0);
    // Settings refresh every 5 s without streams, 100 ms drain cap with them.
    assert_eq!(collection_sleep_cap(false), CONTROL_SETTINGS_REFRESH);
    assert_eq!(collection_sleep_cap(true), K_STREAM_DRAIN_CAP);
    assert_eq!(CONTROL_SETTINGS_REFRESH, Duration::from_secs(5));
    assert_eq!(K_STREAM_DRAIN_CAP, Duration::from_millis(100));
    assert_eq!(
        core.scheduler.next_wait(core.t0, false, false),
        Duration::from_secs(5)
    );
    assert_eq!(
        core.scheduler.next_wait(core.t0, true, false),
        K_STREAM_DRAIN_CAP
    );
    // A due time forced into the past: after work the loop may go round at once,
    // after an idle iteration it waits at least K_LOOP_FLOOR.
    core.scheduler.vehicles.insert(
        core_id(1),
        ScheduledVehicle::new(core_vehicle(1, "online"), core.t0, CORE_W0),
    );
    core.scheduler
        .vehicles
        .get_mut(&core_id(1))
        .unwrap()
        .next_check_due = Some(core.t0);
    core.scheduler.account.last_discovery = Some(core.t0);
    let later = core.at(100);
    assert_eq!(
        core.scheduler.delay_until_next_action(later),
        Duration::ZERO
    );
    assert_eq!(
        core.scheduler.next_wait(later, false, false),
        Duration::ZERO
    );
    assert_eq!(core.scheduler.next_wait(later, false, true), K_LOOP_FLOOR);
    assert_eq!(K_LOOP_FLOOR, Duration::from_millis(100));
}

/// CT55a's course: two asleep cars for an hour on the manual clock, driven by the
/// loop's own rules (`discovery_due`, `due_vehicles`, `next_wait`). It asserts that
/// nothing is read, nothing is written after the first pass and no two empty
/// iterations are closer than the floor, and returns the lists and the iterations.
fn all_asleep_for_an_hour(core: &mut Core) -> (u32, u32) {
    let end = core.t0 + Duration::from_secs(3_600);
    let (mut lists, mut iterations) = (0u32, 0u32);
    let mut last_idle_at: Option<Instant> = None;
    loop {
        let now = core.clock.mono_now();
        if now > end {
            break;
        }
        iterations += 1;
        let mut worked = false;
        if core.scheduler.discovery_due(now) {
            lists += 1;
            core.scheduler.accept_discovery(
                vec![core_vehicle(1, "asleep"), core_vehicle(2, "asleep")],
                now,
            );
            worked = true;
        }
        assert!(
            core.scheduler.due_vehicles(now).is_empty(),
            "asleep cars get no read"
        );
        // Nothing is written after the first pass (a wait the test starts with is
        // stored once, with the account row).
        let tick = core.scheduler.take_tick();
        assert!(iterations == 1 || tick.is_empty(), "no durable change");
        core.scheduler.commit_succeeded(tick, now);
        if !worked {
            if let Some(previous) = last_idle_at {
                assert!(
                    now.saturating_duration_since(previous) >= K_LOOP_FLOOR,
                    "two empty iterations closer than the floor"
                );
            }
            last_idle_at = Some(now);
        }
        let wait = core.scheduler.next_wait(now, false, !worked);
        assert!(wait <= CONTROL_SETTINGS_REFRESH);
        core.clock.advance(wait.max(Duration::from_millis(1)));
    }
    // Both cars stay Asleep and no read was ever made due.
    for id in [1, 2] {
        let record = &core.scheduler.vehicles[&core_id(id)];
        assert_eq!(record.phase, PollPhase::Asleep);
        assert_eq!(record.next_read_due, None);
        assert_eq!(record.last_read, None);
    }
    (lists, iterations)
}

#[test]
fn ct55a_all_asleep_for_an_hour_makes_120_lists_and_no_busy_loop() {
    let mut core = Core::owner();
    let (lists, iterations) = all_asleep_for_an_hour(&mut core);
    assert!((120..=121).contains(&lists), "{lists} lists");
    assert!(iterations <= 721, "{iterations} loop iterations");
}

#[test]
fn ct55a_an_ended_account_wait_does_not_keep_the_loop_at_the_floor_rate() {
    // R11: a rate-limit wait is kept after it ends (a success does not clear it, R45)
    // and a store wait stays until a commit writes something (R6). Each is a due
    // time only while it is still to come; an ended one made the delay zero for the
    // rest of the process and the loop ran ten times a second on an all-asleep Hub.
    for reason in [WaitReason::RateLimited, WaitReason::Store, WaitReason::TeslaUnreachable] {
        let mut core = Core::owner();
        core.scheduler.account.wait = Some((core.at(1), reason));
        let (lists, iterations) = all_asleep_for_an_hour(&mut core);
        assert!((119..=121).contains(&lists), "{reason:?}: {lists} lists");
        assert!(iterations <= 725, "{reason:?}: {iterations} loop iterations");
    }
}

#[test]
fn a_wait_still_to_come_is_a_due_time_and_an_ended_one_is_not() {
    let mut core = Core::owner();
    core.scheduler.accept_discovery(Vec::new(), core.t0);
    core.scheduler.account.wait = Some((core.at(3), WaitReason::RateLimited));
    // 3 s ahead and nothing else due before the idle cap.
    assert_eq!(
        core.scheduler.delay_until_next_action(core.t0),
        Duration::from_secs(3)
    );
    // At the wait's end and after it, the wait counts for nothing.
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(3)),
        K_MAX_IDLE_WAIT
    );
    assert_eq!(
        core.scheduler.delay_until_next_action(core.at(10)),
        K_MAX_IDLE_WAIT
    );
}

// ---- Settings (R61, R62) ------------------------------------------------------

#[test]
fn effective_settings_are_clamped_to_one_minute_and_one_day() {
    let settings = |idle: i64, suspend: i64| {
        EffectiveSettings::from_car(&crate::hub_pack::ProjectionCarSettings {
            suspend_after_idle_min: idle,
            suspend_min: suspend,
            ..crate::hub_pack::ProjectionCarSettings::default()
        })
    };
    let live = settings(15, 30);
    assert_eq!(live.suspend_after_idle, Duration::from_secs(15 * 60));
    assert_eq!(live.suspend, Duration::from_secs(30 * 60));
    let low = settings(0, -5);
    assert_eq!(low.suspend_after_idle, Duration::from_secs(60));
    assert_eq!(low.suspend, Duration::from_secs(60));
    let high = settings(i64::MAX, 100_000);
    assert_eq!(high.suspend_after_idle, Duration::from_secs(1440 * 60));
    assert_eq!(high.suspend, Duration::from_secs(1440 * 60));
}

#[test]
fn control_settings_disable_and_enable_a_car() {
    let mut core = Core::owner();
    let id = core_id(1);
    let hub = Uuid::new_v4();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    let live = crate::hub_pack::ProjectionCarSettings::default();
    let disabled = crate::hub_pack::ProjectionCarSettings {
        enabled: false,
        ..live.clone()
    };
    // The first application only maps the Hub id.
    assert!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, live.clone())], core.at(1))
            .is_empty()
    );
    assert_eq!(core.scheduler.vehicles[&id].hub_id, Some(hub));
    assert_eq!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, disabled.clone())], core.at(2)),
        vec![id]
    );
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.hold, VehicleHold::Disabled);
    assert_eq!(record.next_read_due, None);
    // Re-applying the same settings changes nothing and disconnects nothing.
    assert!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, disabled)], core.at(3))
            .is_empty()
    );
    // Enabling clears the hold, restarts at Unknown and asks for a fresh list.
    assert!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, live)], core.at(4))
            .is_empty()
    );
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.hold, VehicleHold::None);
    assert_eq!(record.phase, PollPhase::Unknown);
    assert!(core.scheduler.discovery_due(core.at(4)));
}

#[test]
fn control_settings_disconnect_streams_and_drop_unconfigured_cars() {
    let mut core = Core::owner();
    let hub = Uuid::new_v4();
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "online")],
        core.t0,
    );
    let live = crate::hub_pack::ProjectionCarSettings::default();
    let no_stream = crate::hub_pack::ProjectionCarSettings {
        use_streaming_api: false,
        ..live.clone()
    };
    // Car 1 stops streaming; car 2 is no longer configured and is dropped.
    assert_eq!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, no_stream)], core.at(1)),
        vec![core_id(2), core_id(1)]
    );
    assert!(!core.scheduler.vehicles.contains_key(&core_id(2)));
    // Turning the stream back on disconnects nothing.
    assert!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, live)], core.at(2))
            .is_empty()
    );
}

#[test]
fn control_settings_recompute_the_suspension_timers_from_their_anchors() {
    let mut core = Core::owner();
    let id = core_id(1);
    let hub = Uuid::new_v4();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    let live = crate::hub_pack::ProjectionCarSettings::default();
    core.scheduler
        .apply_control_settings(&[(hub, 1, live.clone())], core.t0);
    {
        let (used, idle_end) = (core.at(100), core.at(100 + 15 * 60));
        let record = core.scheduler.vehicles.get_mut(&id).unwrap();
        record.last_in_use = Some(used);
        record.suspend_at = Some(idle_end);
    }
    let shorter = crate::hub_pack::ProjectionCarSettings {
        suspend_after_idle_min: 5,
        ..live.clone()
    };
    core.scheduler
        .apply_control_settings(&[(hub, 1, shorter.clone())], core.at(120));
    assert_eq!(
        core.scheduler.vehicles[&id].suspend_at,
        Some(core.at(100 + 5 * 60))
    );
    // A suspension keeps its start and takes the new length, never earlier than
    // now.
    {
        let (began, ends) = (core.at(200), core.at(200 + 30 * 60));
        let record = core.scheduler.vehicles.get_mut(&id).unwrap();
        record.suspend_at = None;
        record.phase = PollPhase::Suspended;
        record.phase_since = (began, CORE_W0 + 200_000);
        record.suspended_until = Some(ends);
    }
    let longer = crate::hub_pack::ProjectionCarSettings {
        suspend_min: 45,
        ..shorter.clone()
    };
    core.scheduler
        .apply_control_settings(&[(hub, 1, longer)], core.at(300));
    assert_eq!(
        core.scheduler.vehicles[&id].suspended_until,
        Some(core.at(200 + 45 * 60))
    );
    let tiny = crate::hub_pack::ProjectionCarSettings {
        suspend_min: 1,
        ..shorter
    };
    core.scheduler
        .apply_control_settings(&[(hub, 1, tiny)], core.at(1_000));
    assert_eq!(
        core.scheduler.vehicles[&id].suspended_until,
        Some(core.at(1_000)),
        "a shortened suspension never ends before now"
    );
}

// ---- Targeted checks and the rest of the interface -----------------------------

#[test]
fn owner_wire_targeted_checks_are_records_and_fleet_targeted_checks_are_lists() {
    let mut owner = Core::owner();
    owner
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], owner.t0);
    owner
        .scheduler
        .schedule_offline_state_fetch(core_id(1), owner.at(1));
    assert_eq!(
        owner.scheduler.due_offline_state_vehicles(owner.at(1)),
        vec![core_id(1)]
    );
    // The check is answered by a record: it clears.
    let candidates =
        owner
            .scheduler
            .accept_vehicle_state(core_id(1), "asleep".to_owned(), owner.at(2));
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].state, "asleep");
    assert!(
        owner
            .scheduler
            .due_offline_state_vehicles(owner.at(2))
            .is_empty()
    );
    assert_eq!(
        owner.scheduler.vehicles[&core_id(1)].phase,
        PollPhase::Asleep
    );
    // A failed check is retried one state interval later, phase unchanged.
    owner
        .scheduler
        .schedule_offline_state_fetch(core_id(1), owner.at(3));
    owner.scheduler.offline_state_failed_for_error(
        core_id(1),
        &CollectorError::OwnerApi(OwnerApiError::HttpStatus(503)),
        owner.at(3),
    );
    assert!(
        owner
            .scheduler
            .due_offline_state_vehicles(owner.at(32))
            .is_empty()
    );
    assert_eq!(
        owner.scheduler.due_offline_state_vehicles(owner.at(33)),
        vec![core_id(1)]
    );
    assert_eq!(
        owner.scheduler.vehicles[&core_id(1)].phase,
        PollPhase::Asleep
    );

    let mut fleet = Core::fleet();
    fleet
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], fleet.t0);
    fleet.scheduler.due_vehicles(fleet.t0);
    fleet
        .scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, true, fleet.t0);
    assert!(!fleet.scheduler.discovery_due(fleet.at(5)));
    fleet
        .scheduler
        .schedule_offline_state_fetch(core_id(1), fleet.at(5));
    assert!(fleet.scheduler.discovery_due(fleet.at(5)));
    assert!(
        fleet
            .scheduler
            .due_offline_state_vehicles(fleet.at(5))
            .is_empty()
    );
}

#[test]
fn status_reports_phase_hold_and_times_without_tesla_ids() {
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.clock.advance(Duration::from_secs(3));
    core.scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Charging, true, core.at(3));
    let status = core.scheduler.status(core.at(3));
    assert_eq!(status.mode, crate::config::CollectorMode::Legacy);
    assert_eq!(status.account_state, AccountState::Collecting);
    assert_eq!(status.account_state.code(), "collecting");
    assert_eq!(status.vehicles.len(), 1);
    let row = &status.vehicles[0];
    assert_eq!(row.phase, PollPhase::Charging);
    assert_eq!(row.phase.code(), "charging");
    assert_eq!(row.hold, VehicleHold::None);
    assert_eq!(row.reason, "use_evidence_snapshot");
    assert_eq!(row.since_ms, CORE_W0 + 3_000);
    assert_eq!(row.last_success_ms, Some(CORE_W0 + 3_000));
    assert_eq!(row.next_in_ms, Some(30_000));
    assert_eq!(core.scheduler.vehicles().len(), 1);
}

#[test]
fn lifecycle_views_are_loaded_once_and_go_stale_on_a_commit() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    assert_eq!(core.scheduler.lifecycle_views_needed(), vec![id]);
    let view = LifecycleView {
        last_reach: Some("online".to_owned()),
        drive_open: true,
    };
    core.scheduler
        .set_lifecycle_view(id, view, Some(CORE_W0 - 1_000));
    assert!(core.scheduler.lifecycle_views_needed().is_empty());
    core.scheduler.lifecycle_changed(id);
    assert_eq!(core.scheduler.lifecycle_views_needed(), vec![id]);
}

#[test]
fn an_open_drive_without_evidence_is_due_after_the_offline_drive_timeout() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    let stored = CORE_W0 - 100_000;
    core.scheduler.set_lifecycle_view(
        id,
        LifecycleView {
            last_reach: Some("asleep".to_owned()),
            drive_open: true,
        },
        Some(stored),
    );
    // T_gap is 600 s: the event is due strictly after stored + 600 s.
    let deadline = stored + 600_000;
    assert!(core.scheduler.due_lifecycle_events(deadline).is_empty());
    assert_eq!(core.scheduler.due_lifecycle_events(deadline + 1), vec![id]);
    // The loop's wake-up includes that deadline: 500 s from now, plus 1 ms,
    // which is later than the 30 s state check, so the check is first.
    assert_eq!(
        core.scheduler.delay_until_next_action(core.t0),
        Duration::from_secs(30)
    );
    core.scheduler.note_force_closed(id);
    assert!(core.scheduler.due_lifecycle_events(deadline + 1).is_empty());
    assert_eq!(core.scheduler.lifecycle_views_needed(), vec![id]);
}

#[test]
fn the_rate_limit_delay_is_read_from_every_wrapping_of_the_error() {
    let owner = CollectorError::OwnerApi(OwnerApiError::RateLimited {
        retry_after_seconds: 17,
    });
    let wrapped =
        CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(OwnerApiError::RateLimited {
            retry_after_seconds: 18,
        }));
    let fleet = CollectorError::FleetApi(FleetApiError::RateLimited {
        retry_after_seconds: 19,
    });
    assert_eq!(rate_limit_retry_after_seconds(&owner), Some(17));
    assert_eq!(rate_limit_retry_after_seconds(&wrapped), Some(18));
    assert_eq!(rate_limit_retry_after_seconds(&fleet), Some(19));
    assert_eq!(
        rate_limit_retry_after_seconds(&CollectorError::OwnerApi(OwnerApiError::HttpStatus(503))),
        None
    );
}

#[test]
fn the_production_constructors_use_the_system_clock_and_the_mode_tables() {
    let now = Instant::now();
    let owner = VehicleScheduler::new(core_cadence(), now);
    assert_eq!(owner.mode, crate::config::CollectorMode::Legacy);
    assert_eq!(owner.timing.driving, Duration::from_secs(10));
    assert_eq!(owner.timing.driving_stream, Some(Duration::from_secs(60)));
    assert_eq!(owner.timing.min_read_spacing, Duration::from_secs(10));
    let fleet = VehicleScheduler::new_for_mode(
        core_cadence(),
        crate::config::CollectorMode::FleetPolling,
        now,
    );
    assert_eq!(fleet.timing.driving, Duration::from_secs(30));
    assert_eq!(fleet.timing.driving_stream, None);
    assert_eq!(fleet.timing.min_read_spacing, Duration::from_secs(30));
    assert_eq!(fleet.timing.state_other, Duration::from_secs(600));
    assert_eq!(fleet.timing.inventory, Duration::from_secs(3_600));
    assert_eq!(owner.timing.state_other, Duration::from_secs(30));
    assert_eq!(owner.timing.state_suspended, Duration::from_secs(30));
    assert_eq!(owner.timing.inventory, Duration::from_secs(300));
    assert_eq!(owner.timing.max_backoff, Duration::from_secs(300));
}

// ---- Spec tests that replace the scheduler tests written before the rewrite ----------------------

#[test]
fn r14_a_sleeping_car_gets_discovery_only_and_never_a_read() {
    // R20, R38: an asleep car is seen through the list; nothing is read for it, and
    // the list is the car's only evidence until the list says online.
    let mut core = Core::owner();
    let candidates = core
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    assert_eq!(candidates, vec![core_vehicle(1, "asleep")]);
    for seconds in [0, 30, 60, 600] {
        let now = core.at(seconds);
        assert!(core.scheduler.due_vehicles(now).is_empty(), "{seconds} s");
        assert!(core.scheduler.due_offline_state_vehicles(now).is_empty());
    }
    assert_eq!(core.scheduler.vehicles[&core_id(1)].next_read_due, None);
    assert_eq!(core.scheduler.vehicles[&core_id(1)].last_read, None);
}

#[tokio::test]
async fn r14_an_offline_discovery_after_t_gap_closes_the_open_drive_at_its_last_fold() {
    // C1 R43 item 4 through R14: a discovery observation that arrives more than
    // `T_gap` after the drive's last evidence ends the drive at that evidence.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let w0 = CORE_W0;
            assert!(
                bed.store
                    .collector_lifecycle_snapshot(hub)
                    .unwrap()
                    .drive_open
            );
            bed.advance(Duration::from_millis(600_001));
            bed.list(vec![core_vehicle(1, "offline")]).await;
            let drives = bed.store.materialised_history(hub).unwrap().drives;
            assert_eq!(drives.len(), 1);
            assert_eq!(drives[0].end_date_ms, w0, "the drive ends at its last fold");
            assert!(
                !bed.store
                    .collector_lifecycle_snapshot(hub)
                    .unwrap()
                    .drive_open
            );
        })
        .await;
}

#[tokio::test]
async fn r14_an_unchanged_discovery_stores_nothing_after_the_first() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "asleep")]).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            let first = bed.discovery_row(hub).expect("the first discovery is stored");
            assert_eq!(seam.tick_commits(), 1);
            for _ in 0..5 {
                bed.advance(Duration::from_secs(30));
                bed.list(vec![core_vehicle(1, "asleep")]).await;
            }
            assert_eq!(bed.discovery_row(hub), Some(first), "no second observation");
            assert_eq!(seam.tick_commits(), 1, "and no second transaction");
        })
        .await;
}

#[test]
fn r36_stream_frames_that_show_no_use_leave_the_rest_schedule_untouched() {
    // A parked frame is not evidence (R36), so it must not move the read a failure
    // pushed out by its back-off (R22, R44): the stream does not shorten the wait
    // the account's REST failures earned.
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_status(503), core.t0);
    let due = core.scheduler.vehicles[&core_id(1)].next_read_due;
    assert!(due.is_some_and(|due| due > core.at(1)));
    for step in 1..=5u64 {
        let now = core.at(step);
        core.scheduler.stream_healthy(core_id(1), now);
        core.scheduler
            .should_persist_stream_telemetry(core_id(1), Some(0), now);
        core.scheduler
            .schedule_stream_evidence(core_id(1), Some("P"), Some(0.0), Some(0), now);
    }
    assert_eq!(core.scheduler.vehicles[&core_id(1)].next_read_due, due);
}

#[test]
fn r62_control_settings_stop_streams_and_discovery_resumes_when_a_car_is_enabled_again() {
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    let hub = Uuid::new_v4();
    let settings = |enabled: bool, streaming: bool| crate::hub_pack::ProjectionCarSettings {
        enabled,
        use_streaming_api: streaming,
        ..crate::hub_pack::ProjectionCarSettings::default()
    };
    let t = core.at(10);
    // The rows the loop reads at its settings refresh: nothing changes.
    assert!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, settings(true, true))], t)
            .is_empty()
    );
    // Streaming off: the car's stream is to be disconnected.
    assert_eq!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, settings(true, false))], t),
        vec![core_id(1)]
    );
    // Streaming on again changes no stream at once (it starts at the next R28 check).
    assert!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, settings(true, true))], t)
            .is_empty()
    );
    // Disabled: hold Disabled, the stream to be disconnected, nothing read.
    assert_eq!(
        core.scheduler
            .apply_control_settings(&[(hub, 1, settings(false, true))], t),
        vec![core_id(1)]
    );
    assert_eq!(core.scheduler.vehicles[&core_id(1)].hold, VehicleHold::Disabled);
    assert!(core.scheduler.due_vehicles(core.at(11)).is_empty());
    // Enabled again: the hold clears, the phase is Unknown and one list is wanted at
    // once, because the stored Tesla state is stale.
    core.scheduler
        .apply_control_settings(&[(hub, 1, settings(true, true))], core.at(20));
    let record = &core.scheduler.vehicles[&core_id(1)];
    assert_eq!((record.hold, record.phase), (VehicleHold::None, PollPhase::Unknown));
    assert!(core.scheduler.discovery_due(core.at(20)));
}

#[test]
fn r3_every_configured_car_keeps_its_own_settings_whether_or_not_a_list_names_it() {
    let mut core = Core::owner();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let settings = |suspend_min: i64| crate::hub_pack::ProjectionCarSettings {
        suspend_min,
        ..crate::hub_pack::ProjectionCarSettings::default()
    };
    core.scheduler
        .apply_control_settings(&[(a, 1, settings(31)), (b, 2, settings(42))], core.t0);
    // The list names only the first car.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    assert_eq!(core.scheduler.vehicles[&core_id(1)].vehicle.settings, settings(31));
    assert_eq!(core.scheduler.vehicles[&core_id(2)].vehicle.settings, settings(42));
    assert_eq!(core.scheduler.vehicles[&core_id(1)].settings.suspend.as_secs(), 31 * 60);
    assert_eq!(core.scheduler.vehicles[&core_id(2)].settings.suspend.as_secs(), 42 * 60);
    // Both cars are the scheduler's and show in the status, with their Hub ids.
    assert_eq!(core.scheduler.status(core.t0).vehicles.len(), 2);
    assert_eq!(core.scheduler.vehicles[&core_id(2)].hub_id, Some(b));
    // The car no list has named is not handed to enrichment or export as a listed car.
    assert_eq!(core.scheduler.vehicles().len(), 1);
}

#[test]
fn r53_a_configured_car_the_list_never_names_waits_for_the_inventory_cadence_and_is_removed() {
    let mut core = Core::owner();
    let hub = Uuid::new_v4();
    core.scheduler.apply_control_settings(
        &[(hub, 2, crate::hub_pack::ProjectionCarSettings::default())],
        core.t0,
    );
    let inventory = core_cadence().inventory;
    // Car 1 is awake, so no state check is due: the car that was never listed must
    // not make the list any faster than the inventory cadence.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, false, core.t0);
    assert_eq!(core.scheduler.list_interval(), inventory);
    assert_eq!(core.scheduler.vehicles[&core_id(2)].hold, VehicleHold::None);
    // The second absence, an inventory interval later, removes it (R53).
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online")],
        core.t0 + inventory,
    );
    assert_eq!(core.scheduler.vehicles[&core_id(2)].hold, VehicleHold::Removed);
    assert_eq!(core.scheduler.vehicles[&core_id(2)].reason, "vehicle_removed");
}
