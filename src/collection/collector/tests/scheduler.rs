// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn classifies_teslamate_poll_phases() {
    let driving = VehicleData::for_test(1, json!({"drive_state":{"shift_state":"D","speed":1}}));
    let charging = VehicleData::for_test(
        1,
        json!({
            "drive_state":{"shift_state":"D","speed":1},
            "charge_state":{"charging_state":"Charging"}
        }),
    );
    let online = VehicleData::for_test(1, json!({"drive_state":{"shift_state":"P","speed":0}}));
    let updating = VehicleData::for_test(
        1,
        json!({
            "drive_state":{"shift_state":"D","speed":1},
            "charge_state":{"charging_state":"Charging"},
            "vehicle_state":{"software_update":{"status":"installing"}}
        }),
    );

    assert_eq!(poll_phase(&driving), PollPhase::Driving);
    assert_eq!(poll_phase(&charging), PollPhase::Charging);
    assert_eq!(poll_phase(&updating), PollPhase::Updating);
    assert_eq!(poll_phase(&online), PollPhase::Online);

    let speed_without_shift =
        VehicleData::for_test(1, json!({"drive_state":{"shift_state":null,"speed":25}}));
    let parked_with_speed =
        VehicleData::for_test(1, json!({"drive_state":{"shift_state":"P","speed":40}}));
    assert_eq!(poll_phase(&speed_without_shift), PollPhase::Online);
    assert_eq!(poll_phase(&parked_with_speed), PollPhase::Online);
    let reverse = VehicleData::for_test(1, json!({"drive_state":{"shift_state":"R","speed":0}}));
    assert_eq!(poll_phase(&reverse), PollPhase::Driving);
}

#[test]
fn scheduler_keeps_failure_backoff_per_vehicle() {
    let now = Instant::now();
    let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let second = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
    let first_id = first.id;
    let second_id = second.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![first, second], now);
    scheduler.pre_online_power(first_id, Some(0), now);
    scheduler.pre_online_power(second_id, Some(0), now);

    assert_eq!(scheduler.due_vehicles(now), vec![first_id, second_id]);
    scheduler.vehicle_failed(first_id, now);
    scheduler.vehicle_succeeded(second_id, PollPhase::Driving, false, now);

    assert_eq!(
        scheduler.due_vehicles(now + Duration::from_secs(5)),
        vec![second_id]
    );
    assert!(
        !scheduler
            .due_vehicles(now + Duration::from_secs(30))
            .contains(&first_id)
    );
}

#[test]
fn sleeping_vehicle_gets_discovery_only() {
    let now = Instant::now();
    let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![asleep], now);

    assert!(scheduler.due_vehicles(now).is_empty());
    assert_eq!(
        scheduler.delay_until_next_action(now),
        Duration::from_secs(30)
    );
}

#[test]
fn newly_online_vehicle_waits_for_pre_online_confirmation() {
    let now = Instant::now();
    let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
    let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let online_id = online.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![asleep], now);
    scheduler.accept_discovery(vec![online], now + Duration::from_secs(30));

    assert!(
        scheduler
            .due_vehicles(now + Duration::from_secs(30))
            .is_empty()
    );
    scheduler.pre_online_power(online_id, Some(0), now + Duration::from_secs(31));
    assert_eq!(
        scheduler.due_vehicles(now + Duration::from_secs(31)),
        vec![online_id]
    );
    assert!(scheduler.requires_live_stream_power_gate(online_id));
}

#[test]
fn silent_pre_online_stream_falls_back_to_vehicle_data_at_deadline() {
    let now = Instant::now();
    let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
    let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let id = online.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![asleep], now);
    scheduler.accept_discovery(vec![online], now + Duration::from_secs(30));

    scheduler.stream_unhealthy(id, now + Duration::from_secs(31));
    assert!(
        scheduler
            .due_vehicles(now + Duration::from_secs(59))
            .is_empty()
    );
    assert_eq!(
        scheduler.due_vehicles(now + Duration::from_secs(60)),
        vec![id]
    );
    assert!(
        !scheduler.requires_live_stream_power_gate(id),
        "the bounded silent-stream fallback must not demand absent power"
    );
}

#[test]
fn established_drive_keeps_owner_api_fallback_when_stream_fails() {
    let now = Instant::now();
    let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
    let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let id = online.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![asleep], now);
    scheduler.accept_discovery(vec![online], now + Duration::from_secs(30));
    scheduler.pre_online_power(id, Some(1), now + Duration::from_secs(31));
    assert!(scheduler.requires_live_stream_power_gate(id));

    scheduler.stream_healthy(id, now + Duration::from_secs(31));
    scheduler.vehicle_succeeded(id, PollPhase::Driving, false, now + Duration::from_secs(32));
    assert!(!scheduler.requires_live_stream_power_gate(id));

    let outage = now + Duration::from_secs(33);
    let StreamOutage::Active(first) = scheduler.stream_unhealthy(id, outage) else {
        panic!("stream outage status");
    };
    assert_eq!(first.consecutive_failures, 1);
    assert!(first.owner_api_fallback_scheduled);
    assert!(!first.live_power_gate);
    assert_eq!(first.phase, PollPhase::Driving);
    assert_eq!(scheduler.due_vehicles(outage), vec![id]);
    assert!(scheduler.has_due_stream_fallback(outage));
    assert_eq!(
        scheduler.vehicles[&id].pre_online,
        PreOnlineCheck::OwnerApiReady
    );

    let StreamOutage::Active(second) =
        scheduler.stream_unhealthy(id, outage + Duration::from_secs(2))
    else {
        panic!("second stream outage status");
    };
    assert_eq!(second.consecutive_failures, 2);
    assert_eq!(second.outage_duration, Duration::from_secs(2));
    let StreamRecovery::Recovered(recovery) =
        scheduler.stream_healthy(id, outage + Duration::from_secs(3))
    else {
        panic!("stream recovery status");
    };
    assert_eq!(recovery.failures, 2);
    assert_eq!(recovery.outage_duration, Duration::from_secs(3));
    assert!(!scheduler.has_due_stream_fallback(outage + Duration::from_secs(3)));
}

#[test]
fn nil_power_pre_online_stream_remains_gated_after_deadline() {
    let now = Instant::now();
    let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
    let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let id = online.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![asleep], now);
    scheduler.accept_discovery(vec![online], now + Duration::from_secs(30));

    scheduler.pre_online_power(id, None, now + Duration::from_secs(31));
    assert!(
        scheduler
            .due_vehicles(now + Duration::from_secs(60))
            .is_empty()
    );
}

#[test]
fn restart_online_discovery_starts_gate_without_duplicate_fetch() {
    let now = Instant::now();
    let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let id = online.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![online], now);

    assert!(scheduler.should_start_stream(id));
    assert!(scheduler.due_vehicles(now).is_empty());
    scheduler.pre_online_power(id, Some(1), now + Duration::from_secs(1));
    assert_eq!(
        scheduler.due_vehicles(now + Duration::from_secs(1)),
        vec![id]
    );
    scheduler.vehicle_succeeded(id, PollPhase::Online, false, now + Duration::from_secs(1));
    assert!(
        scheduler
            .due_vehicles(now + Duration::from_secs(1))
            .is_empty()
    );
}

#[test]
fn offline_discovery_emits_transition_and_one_timeout_checkpoint() {
    let now = Instant::now();
    let offline = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "offline");
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);

    assert_eq!(
        scheduler.accept_discovery(vec![offline.clone()], now),
        vec![offline.clone()]
    );
    assert!(
        scheduler
            .accept_discovery(vec![offline.clone()], now + Duration::from_secs(30))
            .is_empty()
    );
    assert_eq!(
        scheduler.accept_discovery(vec![offline.clone()], now + Duration::from_secs(15 * 60)),
        vec![offline.clone()]
    );
    assert!(
        scheduler
            .accept_discovery(vec![offline], now + Duration::from_secs(16 * 60))
            .is_empty()
    );
}

#[test]
fn stream_offline_state_fetch_coalesces_and_retries_before_timeout_checkpoint() {
    let now = Instant::now();
    let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let id = online.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![online], now);

    scheduler.schedule_offline_state_fetch(id, now);
    scheduler.schedule_offline_state_fetch(id, now + Duration::from_secs(1));
    assert_eq!(scheduler.due_offline_state_vehicles(now), vec![id]);

    scheduler.offline_state_failed_for_error(
        id,
        &CollectorError::OwnerApi(OwnerApiError::Transport),
        now,
    );
    assert!(
        scheduler
            .due_offline_state_vehicles(now + Duration::from_secs(29))
            .is_empty()
    );
    let retry_at = now + GENERIC_OTHER_RETRY;
    assert_eq!(scheduler.due_offline_state_vehicles(retry_at), vec![id]);

    let events = scheduler.accept_vehicle_state(id, "offline".to_owned(), retry_at);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].state, "offline");
    assert!(scheduler.due_offline_state_vehicles(retry_at).is_empty());
}

#[test]
fn offline_discovery_event_materialises_timed_out_drive() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let now = current_epoch_millis().expect("clock");
    let last_position = now - 15 * 60 * 1_000;
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
    let driving = ManualCollection {
        vehicles: vec![vehicle],
        snapshots: vec![
            VehicleData::for_test(
                9,
                json!({
                    "drive_state":{
                        "shift_state":"D",
                        "speed":20,
                        "latitude":47.5,
                        "longitude":19.0,
                        "timestamp":last_position - 1_000
                    },
                    "vehicle_state":{"odometer":1000.0}
                }),
            ),
            VehicleData::for_test(
                9,
                json!({
                    "drive_state":{
                        "shift_state":"D",
                        "speed":20,
                        "latitude":47.51,
                        "longitude":19.01,
                        "timestamp":last_position
                    },
                    "vehicle_state":{"odometer":1000.1}
                }),
            ),
        ],
        failures: vec![],
    };
    persist_collection(&store, &driving, now).expect("persist drive");
    materialise_lifecycle_for_collection(&store, &driving, now).expect("open drive");

    let offline = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "offline");
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime")
        .block_on(persist_discovery_events(
            &store,
            &CursorKey::from_bytes([4; 32]),
            &[offline],
        ))
        .expect("persist offline discovery");

    let vehicle_id = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("stored UUID");
    let history = store.materialised_history(vehicle_id).expect("history");
    assert_eq!(history.drives.len(), 1);
    assert_eq!(history.positions.len(), 2);
    let observations = store
        .current_observations_for_vehicle(vehicle_id)
        .expect("current observations");
    assert!(
        observations
            .iter()
            .any(|observation| { observation.payload["record_type"] == "owner_api_discovery_v1" })
    );
}

fn safe_idle_snapshot() -> VehicleData {
    VehicleData::for_test(
        1,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"power":0},
            "charge_state":{"charging_state":"Complete"},
            "climate_state":{"is_preconditioning":false,"climate_keeper_mode":"off"},
            "vehicle_state":{
                "is_user_present":false,
                "sentry_mode":false,
                "locked":true,
                "df":0,"pf":0,"dr":0,"pr":0,"ft":0,"rt":0
            }
        }),
    )
}

#[test]
fn safe_idle_vehicle_enters_and_leaves_suspended_cadence() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);
    scheduler.stream_healthy(vehicle_id, now);

    assert!(
        scheduler
            .vehicle_succeeded(vehicle_id, PollPhase::Online, true, now)
            .is_none()
    );
    let suspended = scheduler
        .vehicle_succeeded(
            vehicle_id,
            PollPhase::Online,
            true,
            now + Duration::from_secs(15 * 60),
        )
        .expect("suspended transition");
    assert_eq!(suspended.state, "suspended");
    assert_eq!(
        scheduler
            .vehicles
            .get(&vehicle_id)
            .expect("scheduled vehicle")
            .next_poll,
        now + Duration::from_secs(45 * 60)
    );

    let resumed = scheduler
        .vehicle_succeeded(
            vehicle_id,
            PollPhase::Online,
            false,
            now + Duration::from_secs(45 * 60),
        )
        .expect("online transition");
    assert_eq!(resumed.state, "online");
}

#[test]
fn stream_disabled_vehicle_polls_immediately_after_waking_then_uses_drive_cadence() {
    let now = Instant::now();
    let mut asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
    asleep.settings.use_streaming_api = false;
    let vehicle_id = asleep.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![asleep], now);
    assert!(scheduler.due_vehicles(now).is_empty());

    let woke_at = now + Duration::from_secs(30);
    let mut online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    online.settings.use_streaming_api = false;
    scheduler.accept_discovery(vec![online], woke_at);
    assert_eq!(scheduler.due_vehicles(woke_at), vec![vehicle_id]);

    scheduler.vehicle_succeeded(vehicle_id, PollPhase::Driving, false, woke_at);
    assert_eq!(
        scheduler.vehicles[&vehicle_id].next_poll,
        woke_at + test_cadence().driving
    );
}

#[test]
fn stream_health_switches_between_streaming_and_fallback_sleep_cadence() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);

    assert!(!scheduler.vehicles[&vehicle_id].stream_healthy);
    scheduler.stream_healthy(vehicle_id, now);
    scheduler.pre_online_power(vehicle_id, Some(1), now);
    scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, true, now);
    assert_eq!(
        scheduler.vehicles[&vehicle_id].next_poll,
        now + Duration::from_secs(75)
    );

    let streaming_idle = now + Duration::from_secs(3 * 60);
    let suspended = scheduler
        .vehicle_succeeded(vehicle_id, PollPhase::Online, true, streaming_idle)
        .expect("healthy stream uses TeslaMate idle threshold");
    assert_eq!(suspended.state, "suspended");
    assert_eq!(
        scheduler.vehicles[&vehicle_id].next_poll,
        streaming_idle + Duration::from_secs(30 * 60)
    );

    let fallback_at = streaming_idle + Duration::from_secs(1);
    scheduler.stream_unhealthy(vehicle_id, fallback_at);
    assert!(!scheduler.vehicles[&vehicle_id].stream_healthy);
    assert!(!scheduler.vehicles[&vehicle_id].suspended);
    assert!(scheduler.due_vehicles(fallback_at).contains(&vehicle_id));
    assert!(!scheduler.requires_live_stream_power_gate(vehicle_id));
    scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, false, fallback_at);
    let fallback_idle = fallback_at + Duration::from_secs(15 * 60);
    let suspended = scheduler
        .vehicle_succeeded(vehicle_id, PollPhase::Online, true, fallback_idle)
        .expect("unhealthy stream uses owner polling sleep threshold");
    assert_eq!(suspended.state, "suspended");
    assert_eq!(
        scheduler.vehicles[&vehicle_id].next_poll,
        fallback_idle + Duration::from_secs(21 * 60)
    );

    let recovered_at = fallback_idle + Duration::from_secs(1);
    scheduler.stream_healthy(vehicle_id, recovered_at);
    assert!(scheduler.vehicles[&vehicle_id].stream_healthy);
    assert!(scheduler.due_vehicles(recovered_at).contains(&vehicle_id));
    scheduler.vehicle_succeeded(vehicle_id, PollPhase::Driving, false, recovered_at);
    assert_eq!(
        scheduler.vehicles[&vehicle_id].next_poll,
        recovered_at + Duration::from_secs(15)
    );
}

#[test]
fn repeated_stream_telemetry_preserves_owner_api_retry_deadline() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);
    scheduler.stream_healthy(vehicle_id, now);

    let failed_at = now + Duration::from_secs(1);
    scheduler.vehicle_failed_for_error(
        vehicle_id,
        &CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401))),
        failed_at,
    );
    let retry_at = scheduler.vehicles[&vehicle_id].next_poll;

    scheduler.stream_healthy(vehicle_id, failed_at + Duration::from_millis(100));
    assert_eq!(scheduler.vehicles[&vehicle_id].next_poll, retry_at);
    assert!(retry_at > failed_at);
}

#[test]
fn negative_stream_power_schedules_one_charging_refresh() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);
    scheduler.pre_online_power(vehicle_id, Some(0), now);
    scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, true, now);

    let ordinary_poll = scheduler.vehicles[&vehicle_id].next_poll;
    scheduler.schedule_stream_charging_poll(
        vehicle_id,
        Some("P"),
        Some(-3),
        now + Duration::from_millis(500),
    );
    assert_eq!(scheduler.vehicles[&vehicle_id].next_poll, ordinary_poll);

    let charging_at = now + Duration::from_secs(1);
    scheduler.schedule_stream_charging_poll(vehicle_id, None, Some(-3), charging_at);
    assert_eq!(scheduler.due_vehicles(charging_at), vec![vehicle_id]);
    assert_eq!(
        scheduler.vehicles[&vehicle_id].last_phase,
        PollPhase::Charging
    );

    scheduler.vehicle_failed(vehicle_id, charging_at);
    let retry_at = scheduler.vehicles[&vehicle_id].next_poll;
    scheduler.schedule_stream_charging_poll(
        vehicle_id,
        None,
        Some(-3),
        charging_at + Duration::from_millis(100),
    );
    assert_eq!(scheduler.vehicles[&vehicle_id].next_poll, retry_at);
    assert!(retry_at > charging_at);
}

#[test]
fn suspended_parked_negative_power_schedules_charging_refresh() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);
    scheduler.pre_online_power(vehicle_id, Some(0), now);
    scheduler.stream_healthy(vehicle_id, now);
    scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, true, now);
    let suspended_at = now + Duration::from_secs(3 * 60);
    scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, true, suspended_at);
    assert!(scheduler.vehicles[&vehicle_id].suspended);

    let charging_at = suspended_at + Duration::from_secs(1);
    scheduler.schedule_stream_charging_poll(vehicle_id, Some("P"), Some(-3), charging_at);
    assert_eq!(scheduler.due_vehicles(charging_at), vec![vehicle_id]);
    assert!(!scheduler.vehicles[&vehicle_id].suspended);
    assert_eq!(
        scheduler.vehicles[&vehicle_id].last_phase,
        PollPhase::Charging
    );
}

#[test]
fn driving_cadence_uses_healthy_stream_interval() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "driving");
    let vehicle_id = vehicle.id;
    let mut cadence = test_cadence();
    cadence.driving = Duration::from_millis(2_500);
    let mut scheduler = VehicleScheduler::new(cadence, now);
    scheduler.accept_discovery(vec![vehicle], now);

    scheduler.vehicle_succeeded(vehicle_id, PollPhase::Driving, false, now);
    assert_eq!(
        scheduler.vehicles[&vehicle_id].next_poll,
        now + Duration::from_millis(2_500)
    );

    scheduler.stream_healthy(vehicle_id, now + Duration::from_secs(1));
    let healthy_at = now + Duration::from_secs(2);
    scheduler.vehicle_succeeded(vehicle_id, PollPhase::Driving, false, healthy_at);
    assert_eq!(
        scheduler.vehicles[&vehicle_id].next_poll,
        healthy_at + Duration::from_secs(15)
    );
}

#[test]
fn service_fixture_closes_drive_and_blocks_full_poll_until_exit() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);
    scheduler.enter_service_mode(vehicle_id, now);

    assert!(scheduler.due_vehicles(now).is_empty());
    assert!(scheduler.due_service_vehicles(now).is_empty());
    assert!(!scheduler.should_start_stream(vehicle_id));

    let probe_at = now + test_cadence().online;
    assert_eq!(scheduler.due_service_vehicles(probe_at), vec![vehicle_id]);
    scheduler.service_retry(vehicle_id, probe_at);
    assert!(scheduler.due_vehicles(probe_at).is_empty());

    scheduler.service_exited(vehicle_id, probe_at + Duration::from_secs(1));
    assert!(scheduler.should_start_stream(vehicle_id));
    assert!(
        scheduler
            .due_vehicles(probe_at + Duration::from_secs(1))
            .is_empty()
    );
    assert!(!scheduler.vehicles[&vehicle_id].service_mode);
}

#[test]
fn teslamate_sleep_safety_blockers_are_enforced() {
    assert!(sleep_eligible(&safe_idle_snapshot()));
    let blocked = [
        json!({
            "drive_state":{"shift_state":"P","speed":0,"power":1},
            "charge_state":{"charging_state":"Complete"},
            "climate_state":{"is_preconditioning":false},
            "vehicle_state":{"locked":true}
        }),
        json!({
            "drive_state":{"shift_state":"P","speed":0,"power":0},
            "charge_state":{"charging_state":"Complete"},
            "climate_state":{"is_preconditioning":true},
            "vehicle_state":{"locked":true}
        }),
        json!({
            "drive_state":{"shift_state":"P","speed":0,"power":0},
            "charge_state":{"charging_state":"Complete"},
            "climate_state":{"is_preconditioning":false,"climate_keeper_mode":"dog"},
            "vehicle_state":{"locked":true}
        }),
        json!({
            "drive_state":{"shift_state":"P","speed":0,"power":0},
            "charge_state":{"charging_state":"Complete"},
            "climate_state":{"is_preconditioning":false},
            "vehicle_state":{"locked":false}
        }),
        json!({
            "drive_state":{"shift_state":"P","speed":0,"power":0},
            "charge_state":{"charging_state":"Complete"},
            "climate_state":{"is_preconditioning":false},
            "vehicle_state":{"locked":true,"is_user_present":true}
        }),
        json!({
            "drive_state":{"shift_state":"P","speed":0,"power":0},
            "charge_state":{"charging_state":"Complete"},
            "climate_state":{"is_preconditioning":false},
            "vehicle_state":{"locked":true,"sentry_mode":true}
        }),
        json!({
            "drive_state":{"shift_state":"P","speed":0,"power":0},
            "charge_state":{"charging_state":"Complete"},
            "climate_state":{"is_preconditioning":false},
            "vehicle_state":{"locked":true,"df":1}
        }),
        json!({
            "drive_state":{"shift_state":"P","speed":0,"power":0},
            "charge_state":{"charging_state":"Complete"},
            "climate_state":{"is_preconditioning":false},
            "vehicle_state":{
                "locked":true,
                "software_update":{"status":"downloading","download_perc":50.0}
            }
        }),
    ];
    for fields in blocked {
        assert!(!sleep_eligible(&VehicleData::for_test(1, fields)));
    }
}

#[test]
fn car_policy_is_independent_for_scheduler_and_sleep() {
    let now = Instant::now();
    let mut disabled = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    disabled.settings.enabled = false;
    disabled.settings.use_streaming_api = false;
    let mut stream_disabled = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
    stream_disabled.settings.use_streaming_api = false;
    stream_disabled.settings.suspend_after_idle_min = 2;
    stream_disabled.settings.suspend_min = 7;
    let mut streaming = Vehicle::for_test(3, "5YJ3E1EA7KF000003", "online");
    streaming.settings.use_streaming_api = true;
    streaming.settings.suspend_after_idle_min = 2;
    streaming.settings.suspend_min = 7;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(
        vec![disabled, stream_disabled.clone(), streaming.clone()],
        now,
    );

    // Stream-disabled cars poll normally; disabled cars do not.
    assert_eq!(scheduler.due_vehicles(now), vec![stream_disabled.id]);
    // Confirmed stream power opens the gate only for streaming-enabled cars.
    scheduler.pre_online_power(streaming.id, Some(1), now);
    assert_eq!(
        scheduler.due_vehicles(now),
        vec![stream_disabled.id, streaming.id]
    );
    scheduler.vehicle_succeeded(streaming.id, PollPhase::Online, true, now);
    assert!(
        !scheduler
            .due_vehicles(now + Duration::from_secs(60))
            .contains(&VehicleId::from_test(1))
    );
    assert!(
        scheduler
            .due_vehicles(now + Duration::from_secs(60))
            .contains(&stream_disabled.id),
        "stream-disabled cars remain normally pollable"
    );
    let transition = scheduler
        .vehicle_succeeded(
            streaming.id,
            PollPhase::Online,
            true,
            now + Duration::from_secs(2 * 60),
        )
        .expect("streaming car suspends");
    assert_eq!(transition.state, "suspended");
    assert_eq!(
        scheduler.vehicles[&streaming.id].next_poll,
        now + Duration::from_secs(9 * 60)
    );

    let safe = safe_idle_snapshot();
    assert!(sleep_eligible_with_policy(&safe, false));
    assert!(!sleep_eligible_with_policy(
        &VehicleData::for_test(
            2,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":0},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":false},
                "vehicle_state":{"locked":false}
            })
        ),
        true
    ));
}

#[test]
fn live_control_settings_pause_streams_and_resume_discovery() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);
    let paused = crate::hub_pack::ProjectionCarSettings {
        enabled: false,
        ..crate::hub_pack::ProjectionCarSettings::default()
    };
    let paused_targets = vec![(uuid::Uuid::nil(), 1, paused)];

    assert_eq!(
        scheduler.apply_control_settings(&paused_targets, now + Duration::from_secs(1)),
        vec![vehicle_id]
    );
    assert!(
        scheduler
            .due_vehicles(now + Duration::from_secs(1))
            .is_empty()
    );
    assert!(!scheduler.should_start_stream(vehicle_id));

    let resumed = crate::hub_pack::ProjectionCarSettings::default();
    let resumed_targets = vec![(uuid::Uuid::nil(), 1, resumed)];
    let resumed_at = now + Duration::from_secs(2);
    assert!(
        scheduler
            .apply_control_settings(&resumed_targets, resumed_at)
            .is_empty()
    );
    assert!(scheduler.discovery_due(resumed_at));
    assert!(scheduler.vehicles[&vehicle_id].settings.enabled);
}

#[test]
fn discovery_keeps_all_configured_vehicles_and_their_settings() {
    let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let second = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
    let first_settings = crate::hub_pack::ProjectionCarSettings {
        enabled: true,
        use_streaming_api: false,
        suspend_min: 11,
        ..crate::hub_pack::ProjectionCarSettings::default()
    };
    let second_settings = crate::hub_pack::ProjectionCarSettings {
        enabled: false,
        suspend_min: 22,
        ..crate::hub_pack::ProjectionCarSettings::default()
    };
    let ignored = Vehicle::for_test(3, "5YJ3E1EA7KF000003", "online");

    let selected = filter_configured_vehicles(
        vec![first, second, ignored],
        &[
            (uuid::Uuid::nil(), 1, first_settings.clone()),
            (uuid::Uuid::nil(), 2, second_settings.clone()),
        ],
    );
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].id.get(), 1);
    assert_eq!(selected[0].settings, first_settings);
    assert_eq!(selected[1].id.get(), 2);
    assert_eq!(selected[1].settings, second_settings);
}

#[test]
fn missing_configured_vehicle_waits_for_normal_discovery_cadence() {
    let now = Instant::now();
    let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let settings = crate::hub_pack::ProjectionCarSettings::default();
    let configured = vec![
        (uuid::Uuid::new_v4(), 1, settings.clone()),
        (uuid::Uuid::new_v4(), 2, settings),
    ];
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![first], now);

    let checked_at = now + Duration::from_secs(1);
    assert!(
        scheduler
            .apply_control_settings(&configured, checked_at)
            .is_empty()
    );
    assert!(!scheduler.discovery_due(checked_at));
}

#[test]
fn stream_watermark_rejects_duplicate_and_old_frames_after_restart() {
    let temp = crate::private_tempdir().expect("temporary store");
    let vehicle_id = VehicleId::from_test(9);
    let first_timestamp = current_epoch_millis().expect("clock") - 60_000;
    let update = |timestamp_ms: i64, odometer: f64| crate::tesla_stream::StreamUpdate {
        tag: vehicle_id.to_string(),
        timestamp_ms,
        speed: Some(20),
        odometer: Some(odometer),
        soc: Some(80),
        elevation: Some(25),
        est_heading: Some(180),
        est_lat: Some(51.5),
        est_lng: Some(-0.1),
        power: Some(12),
        shift_state: Some("D".to_owned()),
        range: Some(200),
        est_range: Some(210),
        heading: Some(180),
    };

    let store = HubStore::initialize(temp.path()).expect("store");
    persist_stream_update(&store, vehicle_id, &update(first_timestamp, 100.0))
        .expect("first frame");
    persist_stream_update(&store, vehicle_id, &update(first_timestamp, 100.0))
        .expect("duplicate frame");
    persist_stream_update(&store, vehicle_id, &update(first_timestamp - 1, 99.0))
        .expect("old frame");
    drop(store);

    let store = HubStore::initialize(temp.path()).expect("restart");
    persist_stream_update(&store, vehicle_id, &update(first_timestamp + 1_000, 101.0))
        .expect("new frame");

    let registered = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle identity")
        .parse::<Uuid>()
        .expect("vehicle UUID");
    assert!(
        store
            .observations_for_vehicle(registered, crate::db::ObservationQuery::from_start(10),)
            .expect("pruned stream observations")
            .is_empty()
    );
    let observations = store
        .current_observations_for_vehicle(registered)
        .expect("current stream observation");
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].observed_at_ms, first_timestamp + 1_000);
    let lifecycle = store
        .load_lifecycle_state(registered)
        .expect("lifecycle state")
        .expect("open lifecycle state");
    let open = crate::lifecycle::OpenSessionState::decode(&lifecycle.open_session_json)
        .expect("open session");
    assert!(open.open_drive.expect("open drive").positions.is_empty());
    let provisional: i64 = store
        .open()
        .expect("database")
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows
                 WHERE vehicle_id = ?1 AND domain = 'position'",
            [registered.to_string()],
            |row| row.get(0),
        )
        .expect("provisional positions");
    assert_eq!(provisional, 2);

    let mut context = stream_context(&store, vehicle_id).expect("stream context");
    assert_eq!(
        context.last_stream_timestamp_ms,
        Some(first_timestamp + 1_000)
    );
    assert!(!context.queue_lagging);
    context.report_ingestion_health(vehicle_id, first_timestamp + 12_000, Duration::from_secs(6));
    assert_eq!(
        context.last_stream_timestamp_ms,
        Some(first_timestamp + 12_000)
    );
    assert!(context.queue_lagging);
    context.report_ingestion_health(
        vehicle_id,
        first_timestamp + 13_000,
        Duration::from_millis(500),
    );
    assert!(!context.queue_lagging);
}

#[test]
fn asleep_stream_frame_without_power_only_updates_pre_online_state() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let now = Instant::now();
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "asleep");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);
    let update = |power| crate::tesla_stream::StreamUpdate {
        tag: vehicle_id.to_string(),
        timestamp_ms: current_epoch_millis().expect("clock") - 1_000,
        speed: Some(20),
        odometer: Some(100.0),
        soc: Some(80),
        elevation: Some(25),
        est_heading: Some(180),
        est_lat: Some(51.5),
        est_lng: Some(-0.1),
        power,
        shift_state: Some("D".to_owned()),
        range: Some(200),
        est_range: Some(210),
        heading: Some(180),
    };

    assert!(
        !process_stream_telemetry(&store, &mut scheduler, vehicle_id, &update(None))
            .expect("powerless asleep frame")
    );
    assert!(matches!(
        scheduler.vehicles[&vehicle_id].pre_online,
        PreOnlineCheck::ConfirmedFake { .. }
    ));
    let raw_count: i64 = store
        .open()
        .expect("database")
        .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
            row.get(0)
        })
        .expect("raw count");
    assert_eq!(
        raw_count, 0,
        "powerless asleep frame must not create lifecycle input"
    );

    assert!(
        process_stream_telemetry(&store, &mut scheduler, vehicle_id, &update(Some(12)))
            .expect("powered asleep frame")
    );
    assert!(matches!(
        scheduler.vehicles[&vehicle_id].pre_online,
        PreOnlineCheck::ConfirmedReal
    ));
    assert!(scheduler.vehicles[&vehicle_id].next_poll <= Instant::now());
    let raw_count: i64 = store
        .open()
        .expect("database")
        .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
            row.get(0)
        })
        .expect("raw count");
    assert_eq!(raw_count, 0);
    assert!(
        !process_stream_telemetry(
            &store,
            &mut scheduler,
            VehicleId::from_test(99),
            &update(Some(12)),
        )
        .expect("removed vehicle stream frame")
    );
    assert_eq!(
        store
            .current_observations_for_vehicle(
                store
                    .open()
                    .expect("database")
                    .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .expect("vehicle")
                    .parse()
                    .expect("vehicle UUID"),
            )
            .expect("current powered stream observation")
            .len(),
        1
    );
}

#[test]
fn stream_transaction_rolls_back_every_stage_and_retry_commits_once() {
    let points = [
        StreamFaultPoint::RawInsert,
        StreamFaultPoint::LifecycleWrite,
        StreamFaultPoint::WatermarkUpdate,
        StreamFaultPoint::Commit,
    ];
    for (index, point) in points.into_iter().enumerate() {
        let temp = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let vehicle_id = VehicleId::from_test(index as u64 + 40);
        let timestamp = current_epoch_millis().expect("clock") - 60_000;
        let update = crate::tesla_stream::StreamUpdate {
            tag: vehicle_id.to_string(),
            timestamp_ms: timestamp,
            speed: Some(20),
            odometer: Some(100.0),
            soc: Some(80),
            elevation: Some(25),
            est_heading: Some(180),
            est_lat: Some(51.5),
            est_lng: Some(-0.1),
            power: Some(12),
            shift_state: Some("D".to_owned()),
            range: Some(200),
            est_range: Some(210),
            heading: Some(180),
        };
        store.inject_stream_fault(point);
        assert!(persist_stream_update(&store, vehicle_id, &update).is_err());
        let connection = store.open().expect("database");
        for table in [
            "raw_observations",
            "current_observations",
            "stream_watermarks",
            "vehicle_lifecycle_state",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("fault count");
            assert_eq!(count, 0, "fault point {point:?} left {table}");
        }
        drop(connection);

        assert!(persist_stream_update(&store, vehicle_id, &update).expect("retry"));
        assert!(!persist_stream_update(&store, vehicle_id, &update).expect("duplicate"));
        let connection = store.open().expect("database");
        let raw: i64 = connection
            .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                row.get(0)
            })
            .expect("raw count");
        assert_eq!(raw, 0);
        for table in [
            "current_observations",
            "stream_watermarks",
            "vehicle_lifecycle_state",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("committed count");
            assert_eq!(count, 1, "missing committed {table}");
        }
    }
}

#[test]
fn concurrent_same_timestamp_has_one_committed_winner_and_restart_is_idempotent() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let vehicle_id = VehicleId::from_test(90);
    let timestamp = current_epoch_millis().expect("clock") - 60_000;
    let update = crate::tesla_stream::StreamUpdate {
        tag: vehicle_id.to_string(),
        timestamp_ms: timestamp,
        speed: Some(20),
        odometer: Some(100.0),
        soc: Some(80),
        elevation: Some(25),
        est_heading: Some(180),
        est_lat: Some(51.5),
        est_lng: Some(-0.1),
        power: Some(12),
        shift_state: Some("D".to_owned()),
        range: Some(200),
        est_range: Some(210),
        heading: Some(180),
    };
    let first = store.clone();
    let second = store.clone();
    let left_update = update.clone();
    let right_update = update.clone();
    let left = std::thread::spawn(move || persist_stream_update(&first, vehicle_id, &left_update));
    let right =
        std::thread::spawn(move || persist_stream_update(&second, vehicle_id, &right_update));
    let results = [
        left.join().expect("left").expect("left result"),
        right.join().expect("right").expect("right result"),
    ];
    assert_eq!(results.iter().filter(|value| **value).count(), 1);
    assert_eq!(results.iter().filter(|value| !**value).count(), 1);
    let registered = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle")
        .parse::<Uuid>()
        .expect("uuid");
    assert!(
        store
            .observations_for_vehicle(registered, crate::db::ObservationQuery::from_start(10),)
            .expect("pruned observations")
            .is_empty()
    );
    assert_eq!(
        store
            .current_observations_for_vehicle(registered)
            .expect("current observation")
            .len(),
        1
    );
    drop(store);
    let restarted = HubStore::initialize(temp.path()).expect("restart");
    assert!(!persist_stream_update(&restarted, vehicle_id, &update).expect("restart retry"));
}

#[test]
fn per_car_api_fuse_isolated_and_resets_after_five_minutes() {
    let now = Instant::now();
    let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let second = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
    let first_id = first.id;
    let second_id = second.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![first, second], now);
    scheduler.pre_online_power(first_id, Some(1), now);
    scheduler.pre_online_power(second_id, Some(1), now);

    for offset in 0..3 {
        scheduler.vehicle_failed_for_error(
            first_id,
            &CollectorError::OwnerApi(OwnerApiError::HttpStatus(500)),
            now + Duration::from_secs(offset),
        );
    }
    let due = scheduler.due_vehicles(now + Duration::from_secs(2 * 60));
    assert!(due.contains(&second_id));
    assert!(!due.contains(&first_id));
    assert!(
        scheduler
            .due_vehicles(now + Duration::from_secs(8 * 60))
            .contains(&first_id)
    );
}

#[test]
fn stream_power_gate_race_does_not_trip_owner_api_fuse() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);
    scheduler.pre_online_power(id, Some(1), now);

    for offset in 0..API_ERROR_LIMIT {
        scheduler.vehicle_failed_for_error(
            id,
            &CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
                OwnerApiError::StreamPowerNotConfirmed,
            )),
            now + Duration::from_secs(offset as u64),
        );
    }

    assert!(scheduler.vehicle_fuses[&id].api_errors.is_empty());
    assert!(scheduler.vehicle_fuses[&id].api_blown_until.is_none());
}

#[test]
fn generic_api_errors_use_teslamate_state_retry_delays() {
    let cases = [
        ("driving", GENERIC_DRIVING_RETRY),
        ("charging", GENERIC_CHARGING_RETRY),
        ("online", GENERIC_ONLINE_RETRY),
        ("updating", GENERIC_ONLINE_RETRY),
        ("asleep", GENERIC_OTHER_RETRY),
        ("offline", GENERIC_OTHER_RETRY),
        ("start", GENERIC_OTHER_RETRY),
        ("suspended", GENERIC_OTHER_RETRY),
        ("unknown", GENERIC_OTHER_RETRY),
    ];
    for (index, (state, expected)) in cases.into_iter().enumerate() {
        let now = Instant::now() + Duration::from_secs(index as u64 * 100);
        let id = VehicleId::from_test(index as u64 + 1);
        let vehicle = Vehicle::for_test(id.get(), "5YJ3E1EA7KF000001", state);
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);
        if state == "online" {
            scheduler.vehicle_succeeded(id, PollPhase::Online, false, now);
        }
        scheduler.vehicle_failed_for_error(
            id,
            &CollectorError::OwnerApi(OwnerApiError::HttpStatus(500)),
            now,
        );
        assert_eq!(
            scheduler.vehicles[&id].next_poll,
            now + expected,
            "state={state}"
        );
    }
}

#[test]
fn special_retry_precedence_is_exact_and_per_vehicle() {
    let now = Instant::now();
    let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let second = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
    let first_id = first.id;
    let second_id = second.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![first, second], now);

    scheduler.vehicle_failed_for_error(
        first_id,
        &CollectorError::OwnerApi(OwnerApiError::RateLimited {
            retry_after_seconds: 17,
        }),
        now,
    );
    assert_eq!(
        scheduler.vehicles[&first_id].next_poll,
        now + Duration::from_secs(17)
    );

    scheduler.vehicle_failed_for_error(
        second_id,
        &CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
            crate::legacy_auth::LegacyAuthError::Transport,
        ))),
        now,
    );
    assert_eq!(
        scheduler.vehicles[&second_id].next_poll,
        now + LEGACY_REFRESH_RETRY
    );

    scheduler.vehicle_failed_for_error(
        first_id,
        &CollectorError::OwnerApi(OwnerApiError::VehicleNotFound),
        now,
    );
    assert_eq!(
        scheduler.vehicles[&first_id].next_poll,
        now + test_cadence().online
    );

    scheduler.vehicle_failed_for_error(
        second_id,
        &CollectorError::OwnerApi(OwnerApiError::VehicleInService),
        now,
    );
    assert_eq!(
        scheduler.vehicles[&second_id].next_poll,
        now + test_cadence().online
    );

    scheduler.vehicle_failed_for_error(
        first_id,
        &CollectorError::FleetApi(FleetApiError::RateLimited {
            retry_after_seconds: 23,
        }),
        now,
    );
    assert_eq!(
        scheduler.vehicles[&first_id].next_poll,
        now + Duration::from_secs(23)
    );
}

#[test]
fn rate_limit_is_exact_and_vehicle_not_found_resets_at_ten_minutes() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);
    scheduler.pre_online_power(id, Some(1), now);
    scheduler.vehicle_failed_for_error(
        id,
        &CollectorError::OwnerApi(OwnerApiError::RateLimited {
            retry_after_seconds: 17,
        }),
        now,
    );
    assert!(
        scheduler
            .due_vehicles(now + Duration::from_secs(16))
            .is_empty()
    );
    assert_eq!(
        scheduler.due_vehicles(now + Duration::from_secs(17)),
        vec![id]
    );

    for offset in 0..8 {
        scheduler.vehicle_failed_for_error(
            id,
            &CollectorError::OwnerApi(OwnerApiError::VehicleNotFound),
            now + Duration::from_secs(offset),
        );
    }
    assert!(
        scheduler
            .due_vehicles(now + Duration::from_secs(9 * 60))
            .is_empty()
    );
    assert!(scheduler.vehicle_fuse_healthy(id, now + Duration::from_secs(11 * 60)));
}
