// SPDX-License-Identifier: AGPL-3.0-only

use serde_json::json;

use super::*;

fn sample(id: i64, at_ms: i64, vehicle_data: Value) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: at_ms,
        vehicle_state: "online".to_owned(),
        payload: json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "vehicle_data": vehicle_data,
        }),
    }
}

#[test]
fn imported_group_watermarks_block_cached_openers_and_preserve_fresh_authority() {
    let at = 1_700_000_000_000;
    let mut state = OpenSessionState::new();
    state.imported_drive_watermark_ms = Some(at);
    state.imported_charge_watermark_ms = Some(at);
    let mixed = sample(
        1,
        at + 10_000,
        json!({
            "drive_state": {"timestamp": at + 10_000, "shift_state":"P", "latitude":51.0,"longitude":0.0},
            "vehicle_state": {"timestamp":at + 10_000},
            "charge_state": {"timestamp":at,"charging_state":"Charging","charger_power":7.0}
        }),
    );
    let stale = apply_sample(state, 1, &mixed).expect("mixed replay accepted");
    assert!(stale.state.open_charge.is_none());
    assert!(stale.delta.open_charge_samples.is_empty());
    assert_eq!(stale.state.phase, VehiclePhase::Online);
    let mut fresh_charge = mixed.clone();
    fresh_charge.observation_id = 2;
    fresh_charge.payload["vehicle_data"]["charge_state"]["timestamp"] = json!(at + 10_001);
    let fresh = apply_sample(stale.state, 1, &fresh_charge).expect("fresh charge accepted");
    let open = fresh
        .state
        .open_charge
        .as_ref()
        .expect("fresh charge opened");
    assert_eq!(open.start_date_ms, at + 10_001);
    let restored = OpenSessionState::decode(&fresh.state.encode().unwrap()).unwrap();
    assert_eq!(restored.open_charge, fresh.state.open_charge);
    let mixed_terminal = sample(
        3,
        at + 20_000,
        json!({
            "drive_state":{"timestamp":at + 20_000,"shift_state":"P"},
            "vehicle_state":{"timestamp":at + 20_000},
            "charge_state":{"timestamp":at,"charging_state":"Complete","charge_energy_added":100.0}
        }),
    );
    let retained = apply_sample(restored, 1, &mixed_terminal).expect("stale terminal replay");
    assert!(
        retained.state.open_charge.is_some(),
        "cached terminal cannot close fresh charge"
    );
    assert!(retained.delta.charges.is_empty());

    let mut drive_state = OpenSessionState::new();
    drive_state.imported_drive_watermark_ms = Some(at);
    let cached_drive = sample(
        1,
        at + 10_000,
        json!({
            "drive_state":{"timestamp":at,"shift_state":"D","speed":30,"latitude":51.0,"longitude":0.0},
            "vehicle_state":{"timestamp":at + 10_000},
            "charge_state":{"timestamp":at + 10_000,"charging_state":"Disconnected"}
        }),
    );
    let stale = apply_sample(drive_state, 1, &cached_drive).unwrap();
    assert!(stale.state.open_drive.is_none());
    let mut fresh_drive = cached_drive;
    fresh_drive.observation_id = 2;
    fresh_drive.payload["vehicle_data"]["drive_state"]["timestamp"] = json!(at + 10_001);
    let fresh = apply_sample(stale.state, 1, &fresh_drive).unwrap();
    assert_eq!(
        fresh.state.open_drive.as_ref().unwrap().start_date_ms,
        at + 10_001
    );
}

#[test]
fn imported_stale_charge_public_store_replay_and_complete_reopen() {
    use crate::db::{HubStore, ObservationInput, SourceDescriptor, VehicleDescriptor};
    let root = crate::private_tempdir().unwrap();
    let at = 1_700_000_000_000;
    let store = HubStore::initialize(root.path()).unwrap();
    let source = store
        .register_source(
            &SourceDescriptor::new("teslamate_import", "freshness-test"),
            at,
        )
        .unwrap();
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "10"), at)
        .unwrap();
    let mut session = TeslaMateOpenSession {
        car_id: 10,
        ..Default::default()
    };
    session.watermarks.charging_processes.max_timestamp_ms = Some(at);
    session.watermarks.charges.max_timestamp_ms = Some(at);
    store
        .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &session, at)
        .unwrap();
    let mut input = ObservationInput {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        observed_at_ms: at + 10_000,
        payload: json!({
            "record_type":"owner_api_vehicle_data_v1", "source_vehicle_state":"online",
            "vehicle_data":{
                "drive_state":{"timestamp":at + 10_000,"shift_state":"P","latitude":0.0,"longitude":0.0},
                "vehicle_state":{"timestamp":at + 10_000},
                "charge_state":{"timestamp":at,"charging_state":"Charging","battery_level":80,"charger_power":7.0}
            }
        }),
    };
    store
        .accept_owner_observation_and_lifecycle(&input, input.observed_at_ms, 10)
        .unwrap();
    drop(store);
    let store = HubStore::open_existing(root.path()).unwrap();
    let record = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .unwrap()
        .unwrap();
    assert!(
        OpenSessionState::decode(&record.open_session_json)
            .unwrap()
            .open_charge
            .is_none()
    );
    input.observed_at_ms += 1_000;
    input.payload["vehicle_data"]["charge_state"]["timestamp"] = json!(input.observed_at_ms);
    store
        .accept_owner_observation_and_lifecycle(&input, input.observed_at_ms, 10)
        .unwrap();
    let record = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .unwrap()
        .unwrap();
    let state = OpenSessionState::decode(&record.open_session_json).unwrap();
    let charge_id = state.open_charge.unwrap().id;
    drop(store);
    let store = HubStore::open_existing(root.path()).unwrap();
    let samples = store
        .open_charge_samples(vehicle.vehicle_id, charge_id)
        .unwrap();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].timestamp_ms, input.observed_at_ms);
    input.observed_at_ms += 1_000;
    input.payload["vehicle_data"]["charge_state"]["timestamp"] = json!(at);
    input.payload["vehicle_data"]["charge_state"]["charging_state"] = json!("Complete");
    store
        .accept_owner_observation_and_lifecycle(&input, input.observed_at_ms, 10)
        .unwrap();
    drop(store);
    let store = HubStore::open_existing(root.path()).unwrap();
    let record = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        OpenSessionState::decode(&record.open_session_json)
            .unwrap()
            .open_charge
            .unwrap()
            .id,
        charge_id
    );
    assert_eq!(
        store
            .open_charge_samples(vehicle.vehicle_id, charge_id)
            .unwrap()
            .len(),
        1
    );
}

fn discovery(id: i64, at_ms: i64, state: &str) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: at_ms,
        vehicle_state: state.to_owned(),
        payload: json!({
            "record_type": "owner_api_discovery_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": state,
        }),
    }
}

pub(super) fn imported_position_fixture(
    id: i64,
    drive_id: i64,
    at_ms: i64,
    latitude: f64,
    odometer: f64,
    battery_level: i64,
    outside_temp: f64,
    inside_temp: f64,
) -> TeslaMatePosition {
    serde_json::from_value(json!({
        "id": id,
        "car_id": 1,
        "drive_id": drive_id,
        "date_ms": at_ms,
        "latitude": latitude,
        "longitude": 19.0,
        "elevation": 100 + id,
        "speed": 30,
        "power": id as f64,
        "odometer": odometer,
        "ideal_battery_range_km": 300.0 - id as f64,
        "est_battery_range_km": null,
        "rated_battery_range_km": 280.0 - id as f64,
        "battery_level": battery_level,
        "usable_battery_level": battery_level,
        "fan_status": null,
        "driver_temp_setting": null,
        "passenger_temp_setting": null,
        "is_climate_on": false,
        "is_rear_defroster_on": false,
        "is_front_defroster_on": false,
        "outside_temp": outside_temp,
        "inside_temp": inside_temp,
        "battery_heater": false,
        "battery_heater_on": false,
        "battery_heater_no_power": false,
        "tpms_pressure_fl": null,
        "tpms_pressure_fr": null,
        "tpms_pressure_rl": null,
        "tpms_pressure_rr": null
    }))
    .expect("imported position fixture")
}

pub(super) fn imported_charge_fixture(
    id: i64,
    process_id: i64,
    at_ms: i64,
    energy_added: f64,
    battery_level: i64,
    outside_temp: f64,
) -> TeslaMateCharge {
    serde_json::from_value(json!({
        "id": id,
        "charging_process_id": process_id,
        "date_ms": at_ms,
        "battery_heater": false,
        "battery_heater_on": false,
        "battery_heater_no_power": false,
        "battery_level": battery_level,
        "usable_battery_level": battery_level,
        "charge_energy_added_kwh": energy_added,
        "charger_actual_current": 16.0,
        "charger_phases": 3,
        "charger_pilot_current": 16.0,
        "charger_power_kw": 11.0,
        "charger_voltage": 230.0,
        "charge_cable": "IEC",
        "fast_charger_present": false,
        "fast_charger_brand": null,
        "fast_charger_type": null,
        "ideal_range_km": 150.0 + id as f64,
        "rated_range_km": 140.0 + id as f64,
        "not_enough_power_to_heat": false,
        "outside_temp_c": outside_temp
    }))
    .expect("imported charge fixture")
}

#[test]
fn imported_active_charge_refreshes_and_survives_restart() {
    let start = 1_800_200_000_000_i64;
    let process: TeslaMateChargingProcess = serde_json::from_value(json!({
        "id": 80,
        "car_id": 1,
        "position_id": null,
        "address_id": null,
        "geofence_id": null,
        "start_date_ms": start,
        "end_date_ms": null,
        "charge_energy_added": 3.0,
        "charge_energy_used_kwh": 0.9166666666666666,
        "start_ideal_range_km": 151.0,
        "end_ideal_range_km": 152.0,
        "start_battery_level": 40,
        "end_battery_level": 50,
        "duration_min": null,
        "outside_temp_avg": 11.0,
        "start_rated_range_km": 141.0,
        "end_rated_range_km": 142.0,
        "cost": null
    }))
    .expect("imported charging process");
    let mut session = TeslaMateOpenSession {
        car_id: 1,
        charge: Some(process),
        charge_samples: vec![
            imported_charge_fixture(1, 80, start, 1.0, 40, 10.0),
            imported_charge_fixture(2, 80, start + 300_000, 4.0, 50, 12.0),
        ],
        ..Default::default()
    };
    session.watermarks.charges.max_timestamp_ms = Some(start + 300_000);
    let seeded = seed_imported_open_session_state(uuid::Uuid::nil(), &session, None)
        .expect("seed imported charge");
    assert_eq!(seeded.open_charge.as_ref().unwrap().sample_count, 2);

    session.charge_samples[1].charge_energy_added_kwh = Some(6.0);
    let same_count_refresh =
        seed_imported_open_session_state(uuid::Uuid::nil(), &session, Some(&seeded))
            .expect("refresh changed charge values with the same row counts");
    assert_eq!(
        same_count_refresh
            .open_charge
            .as_ref()
            .expect("refreshed charge")
            .last_energy_added,
        Some(6.0)
    );

    session.charge_samples.push(imported_charge_fixture(
        3,
        80,
        start + 450_000,
        7.0,
        65,
        13.0,
    ));
    session.watermarks.charges.max_id = Some(3);
    session.watermarks.charges.max_timestamp_ms = Some(start + 450_000);
    let refreshed =
        seed_imported_open_session_state(uuid::Uuid::nil(), &session, Some(&same_count_refresh))
            .expect("refresh same imported charge parent");
    let refreshed_charge = refreshed.open_charge.as_ref().expect("refreshed charge");
    assert_eq!(refreshed_charge.sample_count, 3);
    assert_eq!(
        refreshed_charge.last_sample_timestamp_ms,
        Some(start + 450_000)
    );
    assert_eq!(refreshed_charge.last_energy_added, Some(7.0));

    let restored = OpenSessionState::decode(&seeded.encode().expect("encode seeded charge"))
        .expect("restore seeded charge");
    let terminal = sample(
        1,
        start + 600_000,
        json!({
            "drive_state": {"shift_state": "P", "speed": 0},
            "charge_state": {
                "charging_state": "Complete",
                "timestamp": start + 600_000,
                "battery_level": 80,
                "charge_energy_added": 10.0,
                "charger_power": 0.0,
                "battery_range": 180.0,
                "ideal_battery_range": 190.0
            }
        }),
    );
    let direct = apply_sample(seeded, 1, &terminal).expect("close seeded charge");
    let restarted = apply_sample(restored, 1, &terminal).expect("close restored charge");
    assert_eq!(restarted.delta.charges, direct.delta.charges);
    let closed = &restarted.delta.charges[0];
    // The imported counter 1.0 is no baseline (C2 R11, R14): 10.0 - 0.0.
    assert_eq!(closed.charge_energy_added, Some(10.0));
    // The fixture draws 1.38 kWh over the counted intervals for 9.0 kWh added
    // (a ratio far below one, which no AC charger can show), so R19 row 4 gives
    // 1.53 kWh and the AC floor of R20 (95 % of 10.0) withholds it.
    assert_eq!(closed.charge_energy_used_kwh, None);
    assert_eq!(closed.start_battery_level, Some(40));
    assert_eq!(closed.end_battery_level, Some(80));
    assert_eq!(closed.outside_temp_avg, Some(11.0));
    assert_eq!(closed.duration_min, Some(10));
}

#[test]
fn available_update_cancels_open_update_without_history() {
    let start = 1_800_000_100_000_i64;
    let installing = apply_sample(
        OpenSessionState::new(),
        1,
        &sample(
            1,
            start,
            json!({
                "vehicle_state": {
                    "timestamp": start,
                    "car_version": "2026.1",
                    "software_update": {"status": "installing"}
                }
            }),
        ),
    )
    .expect("start update");
    assert!(installing.state.open_update.is_some());

    let cancelled = apply_sample(
        installing.state,
        1,
        &sample(
            2,
            start + 30_000,
            json!({
                "vehicle_state": {
                    "timestamp": start + 30_000,
                    "car_version": "2026.1",
                    "software_update": {"status": "available"}
                }
            }),
        ),
    )
    .expect("cancel update");
    assert!(cancelled.state.open_update.is_none());
    assert!(cancelled.delta.updates.is_empty());
}

#[test]
fn newer_firmware_version_is_logged_as_missed_update_once() {
    let start = 1_800_000_110_000_i64;
    let first = apply_sample(
        OpenSessionState::new(),
        1,
        &sample(
            1,
            start,
            json!({
                "vehicle_state": {"timestamp": start, "car_version": "2026.1"}
            }),
        ),
    )
    .expect("first version");
    assert!(first.delta.updates.is_empty());

    let jumped = apply_sample(
        first.state,
        1,
        &sample(
            2,
            start + 60_000,
            json!({
                "vehicle_state": {
                    "timestamp": start + 60_000,
                    "car_version": "2026.2"
                }
            }),
        ),
    )
    .expect("firmware jump");
    assert_eq!(jumped.delta.updates.len(), 1);
    // R31 item 2: the update was missed while unobserved, so its start is the
    // last time the older version was seen, a lower bound.
    assert_eq!(jumped.delta.updates[0].start_date_ms, start);
    assert_eq!(jumped.delta.updates[0].end_date_ms, start + 60_000);
    assert_eq!(jumped.delta.updates[0].version, "2026.2");

    let unchanged = apply_sample(
        jumped.state,
        1,
        &sample(
            3,
            start + 120_000,
            json!({
                "vehicle_state": {
                    "timestamp": start + 120_000,
                    "car_version": "2026.2"
                }
            }),
        ),
    )
    .expect("same firmware");
    assert!(unchanged.delta.updates.is_empty());
}

#[test]
fn online_response_projects_mergeable_car_metadata() {
    let start = 1_800_000_520_000_i64;
    let response = sample(
        1,
        start,
        json!({
            "display_name":"Road car",
            "vin":"5YJTESTVIN1234567",
            "drive_state":{"shift_state":"P","speed":0},
            "vehicle_config":{
                "car_type":"model3","trim_badging":"74D",
                "exterior_color":"Pearl White","wheel_type":"Apollo","spoiler_type":"None"
            },
            "vehicle_state":{"timestamp":start,"car_version":"2026.1"}
        }),
    );

    let first = apply_sample(OpenSessionState::new(), 1, &response)
        .expect("metadata response")
        .state;
    assert_eq!(
        first.car_metadata,
        Some(crate::hub_pack::ProjectionCarPatch {
            name: Some("Road car".into()),
            model: Some("3".into()),
            vin: Some("5YJTESTVIN1234567".into()),
            trim_badging: Some("74D".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            firmware_version: Some("2026.1".into()),
        })
    );

    let restored = OpenSessionState::decode(&first.encode().expect("encode metadata"))
        .expect("decode metadata");
    let partial = sample(
        2,
        start + 1_000,
        json!({
            "display_name":"Renamed road car",
            "drive_state":{"shift_state":"P","speed":0},
            "vehicle_state":{"car_version":"2026.2"}
        }),
    );
    let updated = apply_sample(restored, 1, &partial)
        .expect("partial metadata")
        .state;
    assert_eq!(
        updated.car_metadata,
        Some(crate::hub_pack::ProjectionCarPatch {
            name: Some("Renamed road car".into()),
            model: Some("3".into()),
            vin: Some("5YJTESTVIN1234567".into()),
            trim_badging: Some("74D".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            firmware_version: Some("2026.2".into()),
        })
    );
}

#[test]
fn missing_charge_state_preserves_one_charging_session() {
    let start = 1_800_000_097_000_i64;
    let samples = [
        sample(
            1,
            start,
            json!({
                "charge_state":{
                    "charging_state":"Charging",
                    "battery_level":40,
                    "charge_energy_added":0.1
                }
            }),
        ),
        sample(
            2,
            start + 5_000,
            json!({"drive_state":{"shift_state":"P","speed":0},"charge_state":null}),
        ),
        sample(
            3,
            start + 10_000,
            json!({"drive_state":{"shift_state":"P","speed":0},"charge_state":null}),
        ),
        sample(
            4,
            start + 15_000,
            json!({
                "charge_state":{
                    "charging_state":"Charging",
                    "battery_level":41,
                    "charge_energy_added":0.3
                }
            }),
        ),
        sample(
            5,
            start + 20_000,
            json!({
                "charge_state":{
                    "charging_state":"Complete",
                    "battery_level":41,
                    "charge_energy_added":0.3
                }
            }),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("charge trace");
    assert!(step.state.open_charge.is_none());
    assert_eq!(step.delta.charges.len(), 1);
    assert_eq!(step.delta.open_charge_samples.len(), 3);
    // R61a: with no parked snapshot the counter already shown belongs to a
    // session the Hub has not recorded, so the whole counter is counted.
    assert!((step.delta.charges[0].charge_energy_added.unwrap() - 0.3).abs() < 1e-9);
    assert_eq!(step.delta.charges[0].start_date_ms, start);
    assert_eq!(step.delta.charges[0].end_date_ms, Some(start + 20_000));
}

#[test]
fn state_intervals_transition_keep_open_state_and_resume_after_restart() {
    let online = discovery(1, 1_000, "online");
    let asleep = discovery(2, 2_000, "asleep");
    let back_online = discovery(3, 3_000, "online");

    let first = apply_sample(OpenSessionState::new(), 1, &online).expect("online");
    assert_eq!(first.delta.states.len(), 1);
    assert_eq!(first.delta.states[0].state, "online");
    assert_eq!(first.delta.states[0].end_date_ms, None);

    let second = apply_sample(first.state, 1, &asleep).expect("asleep");
    assert_eq!(second.delta.states.len(), 2);
    assert_eq!(second.delta.states[0].end_date_ms, Some(2_000));
    assert_eq!(second.delta.states[1].state, "asleep");
    assert_eq!(second.delta.states[1].end_date_ms, None);

    let restored = OpenSessionState::decode(&second.state.encode().expect("encode state history"))
        .expect("decode state history");
    let third = apply_sample(restored, 1, &back_online).expect("online after restart");
    assert_eq!(third.delta.states[0].state, "asleep");
    assert_eq!(third.delta.states[0].end_date_ms, Some(3_000));
    assert_eq!(third.delta.states[1].state, "online");
    assert_eq!(third.delta.states[1].end_date_ms, None);
    assert_eq!(third.state.next_state_id, 4);
}

#[test]
fn nested_timestamps_drive_positions_and_charge_samples() {
    let state_time = 1_800_000_500_000_i64;
    let samples = [
        sample(
            1,
            state_time + 100_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.0,"longitude":-0.1,"timestamp":state_time},
                "charge_state":{"charging_state":"Unplugged","timestamp":state_time}
            }),
        ),
        sample(
            2,
            state_time + 101_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.001,"longitude":-0.101,"timestamp":state_time + 1_000},
                "charge_state":{"charging_state":"Charging","timestamp":state_time + 1_000,"charge_energy_added":1.0}
            }),
        ),
        sample(
            3,
            state_time + 102_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.002,"longitude":-0.102,"timestamp":state_time + 2_000},
                "charge_state":{"charging_state":"Complete","timestamp":state_time + 2_000,"charge_energy_added":2.0}
            }),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("timestamp trace");
    assert_eq!(
        step.delta
            .positions
            .iter()
            .map(|position| position.date_ms)
            .collect::<Vec<_>>(),
        vec![state_time, state_time + 1_000, state_time + 2_000]
    );
    assert_eq!(
        step.delta
            .open_charge_samples
            .iter()
            .map(|sample| sample.timestamp_ms)
            .collect::<Vec<_>>(),
        vec![state_time + 1_000, state_time + 2_000]
    );
}

#[test]
fn missing_invalid_and_regressed_nested_timestamps_stay_monotonic() {
    let start = 1_800_000_510_000_i64;
    let samples = [
        sample(
            1,
            start,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.0,"longitude":-0.1},
                "charge_state":{"charging_state":"Charging","charge_energy_added":1.0}
            }),
        ),
        sample(
            2,
            start + 1_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.001,"longitude":-0.101,"timestamp":"invalid"},
                "charge_state":{"charging_state":"Charging","timestamp":"invalid","charge_energy_added":1.1}
            }),
        ),
        sample(
            3,
            start + 2_000,
            json!({
                "drive_state":{"shift_state":"D","speed":10,"latitude":51.002,"longitude":-0.102,"timestamp":start - 1_000},
                "charge_state":{"charging_state":"Complete","timestamp":start - 1_000,"charge_energy_added":1.2},
                "vehicle_state":{"odometer":1000.0}
            }),
        ),
        sample(
            4,
            start + 3_000,
            json!({
                "drive_state":{"shift_state":"D","speed":12,"latitude":51.003,"longitude":-0.103,"timestamp":start - 2_000},
                "charge_state":{"charging_state":"Unplugged"},
                "vehicle_state":{"odometer":1000.1}
            }),
        ),
        sample(
            5,
            start + 4_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.004,"longitude":-0.104,"timestamp":start - 3_000},
                "charge_state":{"charging_state":"Unplugged","timestamp":start - 2_000},
                "vehicle_state":{"odometer":1000.2}
            }),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("fallback trace");
    // Observation 2 has an unreadable group time, which falls back to its own
    // observation time (R5). Observations 3 to 5 carry group times at or
    // before the newest fact of their family, so their drive and gear are
    // stale (R6): no drive opens, and only the standalone positions of
    // observations 1 and 2 are stored.
    assert!(
        step.delta
            .positions
            .iter()
            .all(|position| position.drive_id.is_none())
    );
    assert_eq!(
        step.delta
            .positions
            .iter()
            .map(|position| position.date_ms)
            .collect::<Vec<_>>(),
        vec![start, start + 1_000]
    );
    // Charge samples: observation 1, observation 2 (fallback time) and
    // observation 4, whose charge group has no time of its own. The
    // regressed group times of observations 3 and 5 make them stale.
    assert_eq!(
        step.delta
            .open_charge_samples
            .iter()
            .map(|sample| sample.timestamp_ms)
            .collect::<Vec<_>>(),
        vec![start, start + 1_000, start + 3_000]
    );
    assert!(step.state.open_drive.is_none());
}

#[test]
fn materializes_a_complete_charge_with_samples() {
    let start = 1_800_000_100_000_i64;
    let samples = [
        sample(
            1,
            start,
            json!({
                "charge_state": {
                    "charging_state": "Charging",
                    "battery_level": 40,
                    "charge_energy_added": 1.5,
                    "charger_power": 11.0,
                    "battery_range": 120.0
                },
                "drive_state": {"shift_state": "P", "speed": 0, "latitude": 1.0, "longitude": 2.0}
            }),
        ),
        sample(
            2,
            start + 300_000,
            json!({
                "charge_state": {
                    "charging_state": "Charging",
                    "battery_level": 50,
                    "charge_energy_added": 4.0,
                    "charger_power": 11.0,
                    "battery_range": 150.0
                }
            }),
        ),
        sample(
            3,
            start + 600_000,
            json!({
                "charge_state": {
                    "charging_state": "Complete",
                    "battery_level": 80,
                    "charge_energy_added": 12.0,
                    "charger_power": 0.0,
                    "battery_range": 220.0
                }
            }),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
    assert!(step.state.open_charge.is_none());
    assert_eq!(step.delta.charges.len(), 1);
    assert_eq!(step.delta.open_charge_samples.len(), 3);
    assert_eq!(step.delta.charges[0].start_battery_level, Some(40));
    assert_eq!(step.delta.charges[0].end_battery_level, Some(80));
    // R61a: no parked snapshot, so the counter shown at the first sample is
    // counted: 12.0 - 0.0.
    assert_eq!(step.delta.charges[0].charge_energy_added, Some(12.0));
}

#[test]
fn force_close_functions_follow_the_hub_events() {
    let start = 1_800_000_400_000_i64;
    let charge = |id: i64, at_ms: i64| {
        sample(
            id,
            at_ms,
            json!({
                "charge_state": {"charging_state": "Charging", "battery_level": 50,
                                 "charge_energy_added": 0.1, "charger_power": 7.0},
            }),
        )
    };
    let state = apply_sample(OpenSessionState::new(), 1, &charge(1, start))
        .expect("charge opens")
        .state;
    assert!(state.open_charge.is_some());
    // `no_data` closes an open drive only (R15): the charge stays open.
    let kept = force_close_open_sessions(state.clone(), 1, start + 20_000).expect("no_data");
    assert!(kept.state.open_charge.is_some());
    assert!(kept.delta.charges.is_empty());
    assert_eq!(kept.state.last_observation_id, state.last_observation_id);
    // Service mode closes the charge at its last sample.
    let closed = force_close_for_service(state, 1, start + 20_000).expect("service");
    assert!(closed.state.open_charge.is_none());
    assert_eq!(closed.delta.charges.len(), 1);
    assert_eq!(closed.delta.charges[0].end_date_ms, Some(start));
    assert_eq!(closed.state.service_mode, Some(true));
}
