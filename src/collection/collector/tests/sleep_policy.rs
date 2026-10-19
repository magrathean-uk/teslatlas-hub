// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C3 part K4, sleep and suspend policy: CT21 to CT26 and CT32, plus
// tests of the rules the part owns (R29 to R35, R38, R41) that the spec tests do not
// name. Synthetic data only; the scheduler runs on a manual clock and no test
// touches the network.

/// A fresh Owner-wire scheduler whose car 1 was listed online at t0 and read once,
/// parked, at t0 (the first-contact read of R34 a).
fn first_read_parked(core: &mut Core) {
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    assert_eq!(core.scheduler.due_vehicles(core.t0), vec![id]);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, core.t0);
}

/// Advance the manual clock to `now` (both clocks together, as real time does).
fn clock_to(core: &Core, now: Instant) {
    let current = core.clock.mono_now();
    if now > current {
        core.clock.advance(now - current);
    }
}

/// `n` minutes after t0.
fn minutes(core: &Core, n: u64) -> Instant {
    core.at(n * 60)
}

// ---- CT21: sleep eligibility (R29, R30) ------------------------------------------

#[test]
fn ct21_sleep_eligibility_follows_the_use_table() {
    let eligible = |changes: &[(&str, &str, Value)]| {
        sleep_eligible_with_policy(&snapshot_with(changes), false)
    };
    // The snapshot of section 9 is parked and eligible.
    assert!(eligible(&[]));
    // Use: not eligible.
    for (label, changes) in [
        ("shift D", vec![("drive_state", "shift_state", json!("D"))]),
        ("speed 5, no gear", vec![("drive_state", "speed", json!(5))]),
        (
            "Charging",
            vec![("charge_state", "charging_state", json!("Charging"))],
        ),
        (
            "Starting",
            vec![("charge_state", "charging_state", json!("Starting"))],
        ),
        (
            "installing",
            vec![(
                "vehicle_state",
                "software_update",
                json!({"status": "installing"}),
            )],
        ),
        (
            "climate on, keeper off",
            vec![("climate_state", "is_climate_on", json!(true))],
        ),
        (
            "user present",
            vec![("vehicle_state", "is_user_present", json!(true))],
        ),
        (
            "remote start",
            vec![("vehicle_state", "remote_start", json!(true))],
        ),
        (
            "service mode with shift D",
            vec![
                ("vehicle_state", "service_mode", json!(true)),
                ("drive_state", "shift_state", json!("D")),
            ],
        ),
    ] {
        assert!(!eligible(&changes), "{label} is use");
    }
    // Climate on with no keeper field at all is use too.
    let mut fields = parked_fields();
    fields["climate_state"] = json!({"is_climate_on": true});
    assert!(!sleep_eligible_with_policy(
        &VehicleData::for_test(1, fields),
        false
    ));
    // Not use: eligible.
    for (label, changes) in [
        (
            "Stopped",
            vec![("charge_state", "charging_state", json!("Stopped"))],
        ),
        (
            "NoPower",
            vec![("charge_state", "charging_state", json!("NoPower"))],
        ),
        (
            "Complete",
            vec![("charge_state", "charging_state", json!("Complete"))],
        ),
        (
            "sentry on",
            vec![("vehicle_state", "sentry_mode", json!(true))],
        ),
        (
            "climate on, keeper dog",
            vec![
                ("climate_state", "is_climate_on", json!(true)),
                ("climate_state", "climate_keeper_mode", json!("dog")),
            ],
        ),
        (
            "climate on, keeper camp",
            vec![
                ("climate_state", "is_climate_on", json!(true)),
                ("climate_state", "climate_keeper_mode", json!("camp")),
            ],
        ),
        (
            "unlocked, policy off",
            vec![("vehicle_state", "locked", json!(false))],
        ),
        ("front door open", vec![("vehicle_state", "df", json!(1))]),
    ] {
        assert!(eligible(&changes), "{label} is not use");
    }
    // With the car's opt-in an unlocked car is in use.
    assert!(!sleep_eligible_with_policy(
        &snapshot_with(&[("vehicle_state", "locked", json!(false))]),
        true
    ));
    // The opt-in changes nothing for a locked car.
    assert!(sleep_eligible_with_policy(&snapshot_with(&[]), true));
    // The test-only form is the default policy.
    assert!(sleep_eligible(&snapshot_with(&[(
        "vehicle_state",
        "locked",
        json!(false)
    )])));
}

#[test]
fn climate_keeper_spellings_and_wrong_types_follow_r29() {
    let eligible_with_keeper = |keeper: Value| {
        sleep_eligible_with_policy(
            &snapshot_with(&[
                ("climate_state", "is_climate_on", json!(true)),
                ("climate_state", "climate_keeper_mode", keeper),
            ]),
            false,
        )
    };
    // "off" in any ASCII case, null and a wrong type read as no keeper: the cabin
    // is being conditioned for a drive, so it is use.
    for keeper in [
        json!("off"),
        json!("OFF"),
        json!("Off"),
        json!(null),
        json!(3),
    ] {
        assert!(!eligible_with_keeper(keeper.clone()), "{keeper}");
    }
    // Any other mode holds the cabin for hours, whatever its case.
    for keeper in [json!("dog"), json!("Camp"), json!("DOG"), json!("party")] {
        assert!(eligible_with_keeper(keeper.clone()), "{keeper}");
    }
    // A field of the wrong JSON type gives no evidence.
    for (group, key, value) in [
        ("climate_state", "is_climate_on", json!("true")),
        ("vehicle_state", "is_user_present", json!(1)),
        ("vehicle_state", "remote_start", json!("yes")),
        ("drive_state", "shift_state", json!(4)),
        ("drive_state", "speed", json!("5")),
        ("charge_state", "charging_state", json!(true)),
    ] {
        assert!(
            sleep_eligible_with_policy(&snapshot_with(&[(group, key, value.clone())]), false),
            "{group}.{key} = {value}"
        );
    }
    // The gear and charging spellings are C1 R40a's.
    assert!(!sleep_eligible_with_policy(
        &snapshot_with(&[("drive_state", "shift_state", json!("ShiftStateD"))]),
        false
    ));
    assert!(!sleep_eligible_with_policy(
        &snapshot_with(&[(
            "charge_state",
            "charging_state",
            json!("DetailedChargeStateCharging")
        )]),
        false
    ));
    // Gear P is a statement whatever the speed.
    assert!(sleep_eligible_with_policy(
        &snapshot_with(&[
            ("drive_state", "shift_state", json!("P")),
            ("drive_state", "speed", json!(5)),
        ]),
        false
    ));
    // An empty snapshot shows nothing.
    assert!(sleep_eligible_with_policy(
        &VehicleData::for_test(1, json!({})),
        false
    ));
}

#[test]
fn a_read_with_use_evidence_sets_last_in_use_and_one_without_does_not() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    assert_eq!(core.scheduler.vehicles[&id].last_in_use, None);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, t);
    assert_eq!(core.scheduler.vehicles[&id].last_in_use, Some(t));
    // Use evidence clears the idle window (R31).
    assert_eq!(core.scheduler.vehicles[&id].suspend_at, None);
    // The next read without use restarts the window from the last use, not from
    // now.
    let t2 = core.at(120);
    assert_eq!(core.scheduler.due_vehicles(t2), vec![id]);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, t2);
    assert_eq!(
        core.scheduler.vehicles[&id].suspend_at,
        Some(core.at(60 + 15 * 60))
    );
}

// ---- CT22: the idle window and the suspension (R31 to R33) -----------------------

#[test]
fn ct22_the_idle_window_then_a_suspension_then_a_read_at_its_end() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    assert_eq!(
        core.scheduler.vehicles[&id].suspend_at,
        Some(minutes(&core, 15)),
        "no use is known, so the window runs from the first read"
    );
    // Reads at t0+60 s ... t0+14 min.
    for minute in 1..=14 {
        let t = minutes(&core, minute);
        assert_eq!(core.scheduler.due_vehicles(t), vec![id], "minute {minute}");
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
        assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Online);
    }
    // At t0+15 min the phase is Suspended until t0+45 min, with no read.
    let t15 = minutes(&core, 15);
    assert!(core.scheduler.due_vehicles(t15).is_empty());
    {
        let record = &core.scheduler.vehicles[&id];
        assert_eq!(record.phase, PollPhase::Suspended);
        assert_eq!(record.suspended_until, Some(minutes(&core, 45)));
        assert_eq!(record.reason, "suspended_idle");
        assert_eq!(record.next_read_due, None);
        assert_eq!(record.suspend_at, None);
    }
    // Entering Suspended revokes the stream gate (R36).
    assert_eq!(core.scheduler.take_gate_revocations(), vec![id]);
    // State checks continue and say online; that is no ground for a read (R34).
    for (label, t) in [
        ("t0+15 min", t15),
        ("t0+30 min", minutes(&core, 30)),
        (
            "t0+44:59.999",
            minutes(&core, 45) - Duration::from_millis(1),
        ),
    ] {
        core.scheduler
            .accept_discovery(vec![core_vehicle(1, "online")], t);
        assert!(core.scheduler.due_vehicles(t).is_empty(), "{label}");
        assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    }
    // At t0+45 min the car is still listed online: one read, not gated.
    let t45 = minutes(&core, 45);
    assert_eq!(core.scheduler.due_vehicles(t45), vec![id]);
    assert_eq!(
        core.scheduler.vehicles[&id].reason,
        "suspension_ended_still_online"
    );
    assert!(!core.scheduler.requires_live_stream_power_gate(id));
    // Parked: Suspended again at once until t0+75 min.
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, t45);
    {
        let record = &core.scheduler.vehicles[&id];
        assert_eq!(record.phase, PollPhase::Suspended);
        assert_eq!(record.suspended_until, Some(minutes(&core, 75)));
        assert_eq!(record.next_read_due, None);
    }
    assert!(
        core.scheduler
            .due_vehicles(minutes(&core, 75) - Duration::from_millis(1))
            .is_empty()
    );
    assert_eq!(
        core.scheduler.due_vehicles(minutes(&core, 75)),
        vec![id],
        "one read per suspension while the car stays online"
    );
}

#[test]
fn a_suspension_end_read_that_shows_use_leaves_suspended_for_online() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    // Idle window to its end.
    for minute in 1..=14 {
        let t = minutes(&core, minute);
        core.scheduler.due_vehicles(t);
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
    }
    core.scheduler.due_vehicles(minutes(&core, 15));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    let t45 = minutes(&core, 45);
    assert_eq!(core.scheduler.due_vehicles(t45), vec![id]);
    // Climate is on (use, keeper off): the car is Online again, with no idle
    // window while the use lasts.
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, t45);
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Online);
    assert_eq!(record.reason, "use_evidence_snapshot");
    assert_eq!(record.suspend_at, None);
    assert_eq!(record.suspended_until, None);
    assert_eq!(record.next_read_due, Some(core.at(45 * 60 + 60)));
    // A session phase takes the car out of Suspended as well.
    let mut driving = Core::owner();
    first_read_parked(&mut driving);
    for minute in 1..=14 {
        let t = minutes(&driving, minute);
        driving.scheduler.due_vehicles(t);
        driving
            .scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
    }
    driving.scheduler.due_vehicles(minutes(&driving, 15));
    let t45 = minutes(&driving, 45);
    driving.scheduler.due_vehicles(t45);
    driving
        .scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, t45);
    assert_eq!(driving.scheduler.vehicles[&id].phase, PollPhase::Driving);
}

#[test]
fn the_suspension_end_of_a_car_not_reported_online_is_a_new_period_not_a_read() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    for minute in 1..=14 {
        let t = minutes(&core, minute);
        core.scheduler.due_vehicles(t);
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
    }
    core.scheduler.due_vehicles(minutes(&core, 15));
    // Tesla reports a state this scheduler has no phase for: nothing is read on a
    // state that is not `online` (R38).
    core.scheduler.vehicles.get_mut(&id).unwrap().reported_state = Some("waking".to_owned());
    let t45 = minutes(&core, 45);
    assert!(core.scheduler.due_vehicles(t45).is_empty());
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Suspended);
    assert_eq!(record.suspended_until, Some(minutes(&core, 75)));
}

// ---- CT23: asleep during a suspension ---------------------------------------------

#[test]
fn ct23_a_listed_asleep_ends_the_suspension_and_stops_the_stream() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    // The car is online with a stream wanted.
    assert!(core.scheduler.streams_to_stop().is_empty());
    assert!(core.scheduler.should_start_stream(id));
    for minute in 1..=14 {
        let t = minutes(&core, minute);
        core.scheduler.due_vehicles(t);
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
    }
    core.scheduler.due_vehicles(minutes(&core, 15));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    // A suspended car keeps its subscription (R28): the stream reveals use.
    clock_to(&core, minutes(&core, 15));
    assert!(core.scheduler.should_start_stream(id));
    assert!(core.scheduler.streams_to_stop().is_empty());
    // At t0+20 min the list says asleep.
    let t20 = minutes(&core, 20);
    clock_to(&core, t20);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t20);
    {
        let record = &core.scheduler.vehicles[&id];
        assert_eq!(record.phase, PollPhase::Asleep);
        assert_eq!(record.reason, "reported_asleep");
        assert_eq!(record.suspended_until, None);
        assert_eq!(record.next_read_due, None);
    }
    assert!(!core.scheduler.should_start_stream(id));
    assert_eq!(core.scheduler.streams_to_stop(), vec![id]);
    // Returned once.
    assert!(core.scheduler.streams_to_stop().is_empty());
    // No read until a later `online`.
    for seconds in [0, 30, 300, 1_800] {
        assert!(
            core.scheduler
                .due_vehicles(t20 + Duration::from_secs(seconds))
                .is_empty()
        );
    }
    // The later `online` is a wake read, not a suspension (the last wake read is
    // absent).
    let t30 = minutes(&core, 30);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t30);
    assert_eq!(core.scheduler.due_vehicles(t30), vec![id]);
}

// ---- CT24 and CT25: the wake read and its limit (R34 b, R35) ----------------------

/// The car is Asleep from the first list at t0, and the list says online at t1.
fn asleep_then_online(core: &mut Core, t1: Instant) {
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    assert_eq!(
        core.scheduler.vehicles[&core_id(1)].phase,
        PollPhase::Asleep
    );
    clock_to(core, t1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t1);
}

#[test]
fn ct24_a_wake_read_with_a_healthy_stream_suspends_at_once() {
    let mut core = Core::owner();
    let id = core_id(1);
    let t1 = core.at(600);
    asleep_then_online(&mut core, t1);
    assert_eq!(core.scheduler.due_vehicles(t1), vec![id]);
    assert_eq!(core.scheduler.vehicles[&id].reason, "wake_read");
    assert_eq!(core.scheduler.vehicles[&id].last_wake_read, Some(t1));
    // The stream is healthy when the parked result arrives.
    core.scheduler.stream_healthy(id, t1);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, t1);
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Suspended);
    assert_eq!(
        record.suspended_until,
        Some(t1 + Duration::from_secs(30 * 60))
    );
    assert_eq!(record.suspend_at, None);
    assert_eq!(record.next_read_due, None);
    assert!(core.scheduler.take_gate_revocations().contains(&id));
}

#[test]
fn ct24_a_wake_read_without_a_stream_watches_for_five_minutes() {
    let mut core = Core::owner();
    let id = core_id(1);
    let t1 = core.at(600);
    asleep_then_online(&mut core, t1);
    assert_eq!(core.scheduler.due_vehicles(t1), vec![id]);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, t1);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Online);
    assert_eq!(
        core.scheduler.vehicles[&id].suspend_at,
        Some(t1 + K_WAKE_WATCH)
    );
    assert_eq!(K_WAKE_WATCH, Duration::from_secs(5 * 60));
    // Reads at t1+60 s ... t1+240 s.
    for seconds in [60, 120, 180, 240] {
        let t = t1 + Duration::from_secs(seconds);
        assert_eq!(core.scheduler.due_vehicles(t), vec![id], "{seconds} s");
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
        assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Online);
    }
    // At t1+5 min the car is Suspended until t1+35 min, with no read.
    let t5 = t1 + Duration::from_secs(300);
    assert!(core.scheduler.due_vehicles(t5).is_empty());
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Suspended);
    assert_eq!(
        record.suspended_until,
        Some(t1 + Duration::from_secs(35 * 60))
    );
}

#[test]
fn a_wake_read_that_shows_use_is_an_ordinary_read() {
    let mut core = Core::owner();
    let id = core_id(1);
    let t1 = core.at(600);
    asleep_then_online(&mut core, t1);
    core.scheduler.due_vehicles(t1);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, t1);
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Driving);
    assert_eq!(record.suspend_at, None);
    assert_eq!(record.last_in_use, Some(t1));
    assert_eq!(record.next_read_due, Some(t1 + Duration::from_secs(10)));
    assert!(!record.wake_read_pending);
}

#[test]
fn ct25_a_wake_read_inside_the_suspension_length_is_limited() {
    let mut core = Core::owner();
    let id = core_id(1);
    let t1 = core.at(600);
    asleep_then_online(&mut core, t1);
    assert_eq!(core.scheduler.due_vehicles(t1), vec![id]);
    core.scheduler.stream_healthy(id, t1);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, t1);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    core.scheduler.take_gate_revocations();
    // The list says asleep at t1+2 min and online at t1+4 min: no read; Suspended
    // until t1+30 min.
    let t2 = t1 + Duration::from_secs(120);
    clock_to(&core, t2);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t2);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Asleep);
    let t4 = t1 + Duration::from_secs(240);
    clock_to(&core, t4);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t4);
    assert!(core.scheduler.due_vehicles(t4).is_empty());
    {
        let record = &core.scheduler.vehicles[&id];
        assert_eq!(record.phase, PollPhase::Suspended);
        assert_eq!(record.reason, "wake_read_limited");
        assert_eq!(
            record.suspended_until,
            Some(t1 + Duration::from_secs(30 * 60))
        );
        assert_eq!(
            record.last_wake_read,
            Some(t1),
            "the limit counts wake reads only"
        );
    }
    // Entering Suspended revokes the gate.
    assert_eq!(core.scheduler.take_gate_revocations(), vec![id]);
    // At t1+30 min the car is still online: one read.
    let t30 = t1 + Duration::from_secs(30 * 60);
    assert_eq!(core.scheduler.due_vehicles(t30), vec![id]);
    assert_eq!(
        core.scheduler.vehicles[&id].reason,
        "suspension_ended_still_online"
    );
}

#[test]
fn ct25_stream_evidence_still_triggers_a_gated_read_inside_the_limit() {
    let mut core = Core::owner();
    let id = core_id(1);
    let t1 = core.at(600);
    asleep_then_online(&mut core, t1);
    core.scheduler.due_vehicles(t1);
    core.scheduler.stream_healthy(id, t1);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, t1);
    let t2 = t1 + Duration::from_secs(120);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t2);
    let t4 = t1 + Duration::from_secs(240);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t4);
    assert_eq!(core.scheduler.vehicles[&id].reason, "wake_read_limited");
    // A motion frame at t1+6 min.
    let t6 = t1 + Duration::from_secs(360);
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(12.0), Some(30), t6);
    assert!(core.scheduler.requires_live_stream_power_gate(id));
    assert_eq!(core.scheduler.due_vehicles(t6), vec![id]);
}

#[test]
fn a_wake_read_after_the_suspension_length_is_allowed_again() {
    let id = core_id(1);
    // 29:59.999 after the wake read: limited. Exactly 30 min: allowed.
    for (offset_ms, allowed) in [(30 * 60 * 1000 - 1, false), (30 * 60 * 1000, true)] {
        let mut sibling = Core::owner();
        let t1 = sibling.at(600);
        asleep_then_online(&mut sibling, t1);
        sibling.scheduler.due_vehicles(t1);
        sibling.scheduler.stream_healthy(id, t1);
        sibling
            .scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t1);
        let t2 = t1 + Duration::from_secs(60);
        sibling
            .scheduler
            .accept_discovery(vec![core_vehicle(1, "asleep")], t2);
        let t = t1 + Duration::from_millis(offset_ms);
        clock_to(&sibling, t);
        sibling
            .scheduler
            .accept_discovery(vec![core_vehicle(1, "online")], t);
        assert_eq!(
            sibling.scheduler.vehicles[&id].wake_read_pending, allowed,
            "{offset_ms} ms after the last wake read"
        );
        assert_eq!(
            sibling.scheduler.vehicles[&id].phase,
            if allowed {
                PollPhase::Asleep
            } else {
                PollPhase::Suspended
            }
        );
    }
}

#[test]
fn a_car_returning_online_from_a_dead_zone_mid_session_is_read_at_once() {
    // R34 (e): the car was Driving, the list said offline, and it comes back:
    // one read at once, without the wake-read limit (a wake read happened a minute
    // ago).
    let mut core = Core::owner();
    let id = core_id(1);
    let t1 = core.at(600);
    asleep_then_online(&mut core, t1);
    core.scheduler.due_vehicles(t1);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, t1);
    let t2 = t1 + Duration::from_secs(10);
    core.scheduler.due_vehicles(t2);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, t2);
    let t3 = t2 + Duration::from_secs(5);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "offline")], t3);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Offline);
    assert!(core.scheduler.vehicles[&id].offline_from_session);
    let t4 = t3 + Duration::from_secs(30);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t4);
    assert_eq!(core.scheduler.due_vehicles(t4), vec![id]);
    assert_eq!(core.scheduler.vehicles[&id].reason, "session_return");
    assert_ne!(
        core.scheduler.vehicles[&id].last_wake_read,
        Some(t4),
        "a session return is not a wake read"
    );
}

#[test]
fn an_offline_car_that_was_not_in_a_session_gets_a_wake_read() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "offline")], core.t0);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Offline);
    assert!(!core.scheduler.vehicles[&id].offline_from_session);
    let t = core.at(30);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    assert_eq!(core.scheduler.vehicles[&id].reason, "wake_read");
    assert!(core.scheduler.vehicles[&id].wake_read_pending);
}

#[test]
fn a_second_list_online_does_not_repeat_the_wake_read_and_asleep_withdraws_it() {
    let mut core = Core::owner();
    let id = core_id(1);
    let t1 = core.at(600);
    asleep_then_online(&mut core, t1);
    // The read is due but not yet sent; a second list online changes nothing.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t1 + Duration::from_secs(1));
    assert_eq!(core.scheduler.vehicles[&id].next_read_due, Some(t1));
    assert_eq!(core.scheduler.vehicles[&id].last_wake_read, Some(t1));
    // The list says asleep again before the read went out: no read on an asleep
    // car (R38).
    let t2 = t1 + Duration::from_secs(2);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t2);
    assert!(core.scheduler.due_vehicles(t2).is_empty());
    assert_eq!(core.scheduler.vehicles[&id].next_read_due, None);
    assert!(!core.scheduler.vehicles[&id].wake_read_pending);
}

// ---- CT26: a car listed online while Suspended is never read for that ----------------

#[test]
fn ct26_lists_saying_online_never_make_a_read_while_suspended() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    for minute in 1..=14 {
        let t = minutes(&core, minute);
        core.scheduler.due_vehicles(t);
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
    }
    core.scheduler.due_vehicles(minutes(&core, 15));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    // Lists every 30 s for 29 minutes: zero reads.
    let mut reads = 0;
    for step in 0..=58u64 {
        let t = minutes(&core, 15) + Duration::from_secs(30 * step);
        core.scheduler
            .accept_discovery(vec![core_vehicle(1, "online")], t);
        reads += core.scheduler.due_vehicles(t).len();
        assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    }
    assert_eq!(reads, 0);
}

// ---- R38: never wake, never command --------------------------------------------------

#[test]
fn no_read_is_due_for_a_car_that_tesla_does_not_list_online() {
    // Walk the scheduler through every way a read can become due and check that a
    // car whose latest list or record says asleep or offline is never among them
    // (R38), except the stream-evidence and session-return grounds the rule names.
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    for seconds in [0, 30, 60, 600, 3_600] {
        let t = core.at(seconds);
        clock_to(&core, t);
        core.scheduler
            .accept_discovery(vec![core_vehicle(1, "asleep")], t);
        assert!(core.scheduler.due_vehicles(t).is_empty(), "{seconds} s");
    }
    let mut offline = Core::owner();
    offline
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "offline")], offline.t0);
    for seconds in [0, 30, 60, 600, 3_600] {
        let t = offline.at(seconds);
        offline
            .scheduler
            .accept_discovery(vec![core_vehicle(1, "offline")], t);
        assert!(offline.scheduler.due_vehicles(t).is_empty(), "{seconds} s");
    }
    assert_eq!(offline.scheduler.vehicles[&id].phase, PollPhase::Offline);
}

// ---- R41 and CT32: an install that never ends -------------------------------------------

#[test]
fn ct32_an_install_that_stays_silent_for_two_hours_goes_offline() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Updating, false, core.t0);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Updating);
    assert_eq!(K_UPDATE_STALE, Duration::from_secs(2 * 60 * 60));
    // Every read gets 408 (the car disconnects during an install). The Updating
    // interval is 120 s.
    let error = CollectorError::OwnerApi(OwnerApiError::HttpStatus(408));
    let mut t = core.t0;
    loop {
        let next = core.scheduler.vehicles[&id]
            .next_read_due
            .expect("an install is read on its interval");
        assert_eq!(next, t + Duration::from_secs(120));
        t = next;
        if t >= core.at(2 * 60 * 60) {
            break;
        }
        assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
        core.scheduler.vehicle_failed_for_error(id, &error, t);
        let record = &core.scheduler.vehicles[&id];
        assert_eq!(
            record.consecutive_failures, 0,
            "a 408 while Updating is no failure"
        );
        assert_eq!(record.last_error, None);
        assert_eq!(record.phase, PollPhase::Updating);
    }
    // Just before two hours the phase is still Updating; at two hours it is
    // Offline and reads stop.
    let just_before = core.at(2 * 60 * 60) - Duration::from_millis(1);
    assert!(core.scheduler.due_vehicles(just_before).is_empty());
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Updating);
    let end = core.at(2 * 60 * 60);
    assert!(core.scheduler.due_vehicles(end).is_empty());
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Offline);
    assert_eq!(record.reason, "update_stale");
    assert_eq!(record.next_read_due, None);
    // State checks continue (the list interval of an Offline car).
    assert!(core.scheduler.discovery_due(end + Duration::from_secs(30)));
}

#[test]
fn a_successful_read_during_an_install_restarts_the_stale_clock() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Updating, false, core.t0);
    // A read succeeds at 1 h 50 min.
    let t = core.at(110 * 60);
    core.scheduler.due_vehicles(t);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Updating, false, t);
    // Two hours after the start the install is not stale.
    let two_hours = core.at(2 * 60 * 60);
    core.scheduler.due_vehicles(two_hours);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Updating);
    // Two hours after that read it is.
    let stale = t + K_UPDATE_STALE;
    core.scheduler.due_vehicles(stale);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Offline);
}

#[test]
fn the_stale_install_deadline_is_a_due_time_of_the_loop() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Updating, false, core.t0);
    assert_eq!(
        core.scheduler.vehicles[&id].update_stale_deadline(),
        Some(core.t0 + K_UPDATE_STALE)
    );
    // A car in any other phase has none.
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, core.t0);
    assert_eq!(core.scheduler.vehicles[&id].update_stale_deadline(), None);
}

// ---- Gated reads that fail, and 408 outside an install ------------------------------------

#[test]
fn a_gated_read_the_gate_refused_is_no_failure_and_leaves_the_car_suspended() {
    let mut core = suspended_core();
    let id = core_id(1);
    let t = core.at(20 * 60);
    clock_to(&core, t);
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(12.0), None, t);
    assert!(core.scheduler.requires_live_stream_power_gate(id));
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    let refused = CollectorError::OwnerApi(OwnerApiError::StreamPowerNotConfirmed);
    core.scheduler.vehicle_failed_for_error(id, &refused, t);
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Suspended);
    assert_eq!(record.consecutive_failures, 0);
    assert_eq!(record.last_error, None);
    assert_eq!(record.first_failure, None);
    assert!(!record.in_flight);
    assert_eq!(record.next_read_due, None);
    assert!(!core.scheduler.requires_live_stream_power_gate(id));
    assert!(
        core.scheduler
            .due_vehicles(t + Duration::from_secs(600))
            .is_empty()
    );
    // The same refusal wrapped by the credential unit's error is recognised too.
    assert!(is_stream_power_not_confirmed(
        &CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
            OwnerApiError::StreamPowerNotConfirmed
        ))
    ));
    assert!(!is_stream_power_not_confirmed(&CollectorError::OwnerApi(
        OwnerApiError::Transport
    )));
}

/// Car 1 first-contacted at t0 and read parked on its way into Suspended at
/// t0 + 15 min, with a stream wanted.
fn suspended_core() -> Core {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    for minute in 1..=14 {
        let t = minutes(&core, minute);
        core.scheduler.due_vehicles(t);
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
    }
    core.scheduler.due_vehicles(minutes(&core, 15));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    core.scheduler.take_gate_revocations();
    core
}

#[test]
fn a_failed_gated_read_after_motion_still_makes_the_car_driving() {
    let mut core = suspended_core();
    let id = core_id(1);
    let t = core.at(20 * 60);
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(12.0), None, t);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    // The read was sent and Tesla answered 503: the car is moving, so it is
    // Driving, with the Driving back-off.
    let error = CollectorError::OwnerApi(OwnerApiError::HttpStatus(503));
    core.scheduler.vehicle_failed_for_error(id, &error, t);
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Driving);
    assert_eq!(record.consecutive_failures, 1);
    assert_eq!(record.next_read_due, Some(t + Duration::from_secs(10)));
}

#[test]
fn a_suspended_car_gets_no_timed_read_after_a_failed_gated_power_read() {
    let mut core = suspended_core();
    let id = core_id(1);
    let t = core.at(20 * 60);
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(0.0), Some(-7), t);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    let error = CollectorError::OwnerApi(OwnerApiError::HttpStatus(503));
    core.scheduler.vehicle_failed_for_error(id, &error, t);
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Suspended);
    assert_eq!(record.consecutive_failures, 1);
    assert_eq!(
        record.next_read_due, None,
        "a Suspended car has no timed read"
    );
    // Until the suspension ends at t0 + 45 min, nothing is due.
    assert!(
        core.scheduler
            .due_vehicles(t + Duration::from_secs(600))
            .is_empty()
    );
    assert_eq!(
        core.scheduler.due_vehicles(core.at(45 * 60)),
        vec![id],
        "the read at the end of the suspension"
    );
}

#[test]
fn a_gated_read_that_shows_no_use_starts_a_new_suspension() {
    let mut core = suspended_core();
    let id = core_id(1);
    let t = core.at(20 * 60);
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(0.0), Some(-7), t);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    // The read classifies the power as nothing the Hub polls for.
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, t);
    let record = &core.scheduler.vehicles[&id];
    assert_eq!(record.phase, PollPhase::Suspended);
    assert_eq!(
        record.suspended_until,
        Some(t + Duration::from_secs(30 * 60))
    );
    assert_eq!(record.next_read_due, None);
    assert!(record.gated_read.is_none());
    assert_eq!(core.scheduler.take_gate_revocations(), vec![id]);
}

// ---- Settings and the suspension (R61, R62) -----------------------------------------------

#[test]
fn the_idle_window_and_suspension_follow_the_cars_own_settings() {
    let mut core = Core::owner();
    let id = core_id(1);
    let mut vehicle = core_vehicle(1, "online");
    vehicle.settings.suspend_after_idle_min = 5;
    vehicle.settings.suspend_min = 12;
    core.scheduler.accept_discovery(vec![vehicle], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, true, core.t0);
    assert_eq!(
        core.scheduler.vehicles[&id].suspend_at,
        Some(core.at(5 * 60))
    );
    for minute in 1..=4 {
        let t = minutes(&core, minute);
        core.scheduler.due_vehicles(t);
        core.scheduler
            .vehicle_succeeded(id, PollPhase::Online, true, t);
    }
    core.scheduler.due_vehicles(minutes(&core, 5));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Suspended);
    assert_eq!(
        core.scheduler.vehicles[&id].suspended_until,
        Some(core.at(17 * 60))
    );
}

#[test]
fn a_held_or_disabled_car_has_no_idle_window_or_suspension_timer() {
    let mut core = Core::owner();
    let id = core_id(1);
    first_read_parked(&mut core);
    core.scheduler.vehicles.get_mut(&id).unwrap().hold =
        VehicleHold::Paused(PauseReason::PersistentFailure);
    // The window's end passes: a held car is not timed (R51), so nothing changes.
    core.scheduler.due_vehicles(minutes(&core, 20));
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Online);
}

#[test]
fn the_k4_constants_have_the_values_the_spec_gives() {
    assert_eq!(K_WAKE_WATCH, Duration::from_secs(300));
    assert_eq!(K_UPDATE_STALE, Duration::from_secs(7_200));
    assert_eq!(K_STREAM_POWER_KW, 1);
    assert_eq!(K_STREAM_RESTART_BASE, Duration::from_secs(30));
    assert_eq!(K_STREAM_RESTART_MAX, Duration::from_secs(300));
    assert_eq!(K_STREAM_EVENT_CAPACITY, 40);
}

// ---- CT59, second half -----------------------------------------------------------

/// CT59: the filter that provider bodies pass through keeps every field the phase
/// and sleep rules read, so the classifiers answer the same on the filtered
/// snapshot as on the body's own fields (C3 R15, R23, R29).
#[test]
fn ct59_the_classifiers_answer_the_same_on_the_allow_listed_snapshot() {
    let driving = json!({
        "vehicle_id": 7,
        "state": "online",
        "drive_state": {
            "shift_state": "D", "speed": 42, "power": 11,
            "latitude": 51.5, "longitude": -0.12,
            "timestamp": 1_800_000_000_123_i64
        },
        "charge_state": {"charging_state": "Disconnected", "timestamp": 1_800_000_000_100_i64},
        "climate_state": {"is_climate_on": false, "climate_keeper_mode": "off"},
        "vehicle_state": {
            "locked": true, "is_user_present": true, "sentry_mode": false,
            "remote_start": false, "software_update": {"status": ""}
        },
        "vehicle_config": {"car_type": "model3"}
    });
    let parked_unlocked = json!({
        "vehicle_id": 7,
        "state": "online",
        "drive_state": {"shift_state": null, "speed": null, "power": 0},
        "charge_state": {"charging_state": "Disconnected"},
        "climate_state": {"is_climate_on": false, "climate_keeper_mode": "off"},
        "vehicle_state": {
            "locked": false, "is_user_present": false, "sentry_mode": false,
            "remote_start": false, "software_update": {"status": ""}
        }
    });
    let charging = json!({
        "vehicle_id": 7,
        "state": "online",
        "drive_state": {"shift_state": "P", "speed": null, "power": -11, "timestamp": 1_800_000_000_000_i64},
        "charge_state": {"charging_state": "Charging", "timestamp": 1_800_000_000_500_i64},
        "climate_state": {"is_climate_on": false, "climate_keeper_mode": "off"},
        "vehicle_state": {
            "locked": true, "is_user_present": false, "sentry_mode": false,
            "remote_start": false, "software_update": {"status": ""}
        }
    });
    // (body, phase, eligible with the default policy, eligible when an unlocked car
    // counts as in use)
    for (body, phase, eligible, eligible_when_locked_required) in [
        (driving, PollPhase::Driving, false, false),
        (parked_unlocked, PollPhase::Online, true, false),
        (charging, PollPhase::Charging, false, false),
    ] {
        let filtered = crate::owner_api::allowlisted_provider_response_v1(&json!({
            "response": body.clone()
        }))
        .expect("the allow-listed response");
        let through_filter = VehicleData::for_test(7, Value::Object(filtered));
        let direct = VehicleData::for_test(7, body);
        assert_eq!(poll_phase(&direct), phase);
        assert_eq!(poll_phase(&through_filter), phase);
        assert_eq!(sleep_eligible_with_policy(&direct, false), eligible);
        assert_eq!(sleep_eligible_with_policy(&through_filter, false), eligible);
        assert_eq!(
            sleep_eligible_with_policy(&direct, true),
            eligible_when_locked_required
        );
        assert_eq!(
            sleep_eligible_with_policy(&through_filter, true),
            eligible_when_locked_required
        );
    }
}
