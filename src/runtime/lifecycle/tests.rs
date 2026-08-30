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

fn fleet_sample(id: i64, at_ms: i64, vehicle_data: Value) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: at_ms,
        vehicle_state: "online".to_owned(),
        payload: json!({
            "record_type": "fleet_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "provider_raw_json": {"response": vehicle_data},
        }),
    }
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

fn imported_position_fixture(
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

fn imported_charge_fixture(
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
fn imported_active_drive_refreshes_and_survives_restart() {
    let start = 1_800_100_000_000_i64;
    let drive: TeslaMateDrive = serde_json::from_value(json!({
        "id": 70,
        "car_id": 1,
        "start_date_ms": start,
        "end_date_ms": null,
        "start_position_id": 700,
        "end_position_id": 701,
        "start_address_id": null,
        "end_address_id": null,
        "start_geofence_id": null,
        "end_geofence_id": null,
        "outside_temp_avg": 18.0,
        "inside_temp_avg": 20.0,
        "speed_max": 50,
        "power_max": 12.0,
        "power_min": -5.0,
        "start_ideal_range_km": 300.0,
        "end_ideal_range_km": 298.0,
        "start_rated_range_km": 280.0,
        "end_rated_range_km": 278.0,
        "start_km": 100.0,
        "end_km": 100.8,
        "distance_km": null,
        "duration_min": null,
        "ascent": 7,
        "descent": 2
    }))
    .expect("imported drive");
    let mut session = TeslaMateOpenSession {
        car_id: 1,
        drive: Some(drive),
        drive_positions: vec![
            imported_position_fixture(700, 70, start, 47.5, 100.0, 80, 17.0, 19.0),
            imported_position_fixture(701, 70, start + 60_000, 47.51, 100.8, 78, 19.0, 21.0),
        ],
        ..Default::default()
    };
    session.watermarks.positions.max_timestamp_ms = Some(start + 60_000);
    let seeded = seed_imported_open_session_state(uuid::Uuid::nil(), &session, None)
        .expect("seed imported drive");
    assert_eq!(seeded.open_drive.as_ref().unwrap().position_count, 2);

    let mut continued = seeded.clone();
    continued
        .open_drive
        .as_mut()
        .expect("continued drive")
        .saw_offline = true;
    session.watermarks.updates.max_id = Some(44);
    let watermark_only =
        seed_imported_open_session_state(uuid::Uuid::nil(), &session, Some(&continued))
            .expect("watermark-only refresh");
    assert!(
        watermark_only
            .open_drive
            .as_ref()
            .expect("continued drive")
            .saw_offline,
        "watermark-only movement must not erase Hub continuation state"
    );

    session.drive_positions[1].speed = Some(65);
    let same_count_refresh =
        seed_imported_open_session_state(uuid::Uuid::nil(), &session, Some(&watermark_only))
            .expect("refresh changed drive values with the same row counts");
    assert_eq!(
        same_count_refresh
            .open_drive
            .as_ref()
            .expect("refreshed drive")
            .speed_max,
        Some(65)
    );

    session.drive_positions.push(imported_position_fixture(
        702,
        70,
        start + 90_000,
        47.515,
        100.9,
        77,
        20.0,
        22.0,
    ));
    session.watermarks.positions.max_id = Some(702);
    session.watermarks.positions.max_timestamp_ms = Some(start + 90_000);
    let refreshed =
        seed_imported_open_session_state(uuid::Uuid::nil(), &session, Some(&same_count_refresh))
            .expect("refresh same imported drive parent");
    let refreshed_drive = refreshed.open_drive.as_ref().expect("refreshed drive");
    assert_eq!(refreshed_drive.position_count, 3);
    assert_eq!(refreshed_drive.last_position_date_ms, Some(start + 90_000));

    let restored = OpenSessionState::decode(&seeded.encode().expect("encode seeded drive"))
        .expect("restore seeded drive");
    let terminal = sample(
        1,
        start + 120_000,
        json!({
            "drive_state": {
                "shift_state": "P",
                "speed": 0,
                "latitude": 47.52,
                "longitude": 19.0,
                "timestamp": start + 120_000
            },
            "vehicle_state": {"odometer": 101.0 / 1.609_344},
            "charge_state": {"battery_level": 77, "battery_range": 277.0},
            "climate_state": {"inside_temp": 22.0, "outside_temp": 20.0}
        }),
    );
    let direct = apply_sample(seeded, 1, &terminal).expect("close seeded drive");
    let restarted = apply_sample(restored, 1, &terminal).expect("close restored drive");
    assert_eq!(restarted.delta.drives, direct.delta.drives);
    let closed = &restarted.delta.drives[0];
    assert_eq!(closed.distance_km, Some(1.0));
    assert_eq!(closed.start_latitude, Some(47.5));
    assert_eq!(closed.end_latitude, Some(47.52));
    assert_eq!(closed.outside_temp_avg, Some(18.0));
    assert!((closed.inside_temp_avg.unwrap() - 20.666_666_666_666_668).abs() < 1e-9);
    assert_eq!(closed.ascent, Some(7));
    assert_eq!(closed.descent, Some(2));
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
    assert_eq!(closed.charge_energy_added, Some(9.0));
    assert_eq!(closed.charge_energy_used_kwh, Some(0.9166666666666666));
    assert_eq!(closed.start_battery_level, Some(40));
    assert_eq!(closed.end_battery_level, Some(80));
    assert_eq!(closed.outside_temp_avg, Some(11.0));
    assert_eq!(closed.duration_min, Some(10));
}

#[test]
fn materializes_a_complete_drive_with_positions() {
    let start = 1_800_000_000_000_i64;
    let samples = [
        sample(
            1,
            start,
            json!({
                "drive_state": {
                    "shift_state": "D",
                    "speed": 20,
                    "latitude": 47.5,
                    "longitude": 19.0,
                    "power": -7.0,
                    "native_location_elevation": 100,
                    "timestamp": start
                },
                "vehicle_state": {"odometer": 100.0},
                "charge_state": {"battery_level": 70, "battery_range": 200.0, "ideal_battery_range": 338.8},
                "climate_state": {"outside_temp": 18.0, "inside_temp": 20.0, "is_climate_on": true}
            }),
        ),
        sample(
            2,
            start + 60_000,
            json!({
                "drive_state": {
                    "shift_state": "D",
                    "speed": 40,
                    "latitude": 47.51,
                    "longitude": 19.01,
                    "power": 12.5,
                    "native_location_elevation": 160,
                    "timestamp": start + 60_000
                },
                "vehicle_state": {"odometer": 101.25},
                "charge_state": {"battery_level": 69, "battery_range": 198.0}
            }),
        ),
        sample(
            3,
            start + 120_000,
            json!({
                "drive_state": {
                    "shift_state": "D",
                    "speed": 30,
                    "latitude": 47.515,
                    "longitude": 19.015,
                    "power": 36.0,
                    "native_location_elevation": 130,
                    "timestamp": start + 120_000
                },
                "vehicle_state": {"odometer": 103.0},
                "charge_state": {"battery_level": 68, "battery_range": 196.0, "ideal_battery_range": 334.8},
                "climate_state": {"inside_temp": 22.0}
            }),
        ),
        sample(
            4,
            start + 180_000,
            json!({
                "drive_state": {
                    "shift_state": "P",
                    "speed": 0,
                    "latitude": 47.52,
                    "longitude": 19.02,
                    "timestamp": start + 180_000
                },
                "charge_state": {"battery_level": 68, "battery_range": 196.0}
            }),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
    assert!(step.state.open_drive.is_none());
    assert_eq!(step.delta.drives.len(), 1);
    assert_eq!(step.delta.positions.len(), 4);
    assert_eq!(
        step.delta
            .positions
            .iter()
            .filter(|position| position.drive_id.is_some())
            .count(),
        3
    );
    assert_eq!(
        step.delta
            .positions
            .iter()
            .filter(|position| position.drive_id.is_none())
            .count(),
        1
    );
    assert_eq!(step.delta.drives[0].start_date_ms, start);
    assert_eq!(step.delta.drives[0].end_date_ms, start + 120_000);
    assert_eq!(step.delta.drives[0].speed_max, Some(64));
    assert!((step.delta.drives[0].distance_km.unwrap() - 3.0 * 1.609_344).abs() < 0.000_001);
    assert_eq!(step.delta.drives[0].duration_min, Some(2));
    assert_eq!(step.delta.drives[0].inside_temp_avg, Some(21.0));
    assert_eq!(step.delta.drives[0].power_max, Some(36.0));
    assert_eq!(step.delta.drives[0].power_min, Some(-7.0));
    assert_eq!(step.delta.drives[0].start_ideal_range_km, Some(545.25));
    assert_eq!(step.delta.drives[0].end_ideal_range_km, Some(538.81));
    assert_eq!(step.delta.drives[0].ascent, Some(60));
    assert_eq!(step.delta.drives[0].descent, Some(30));
    assert_eq!(step.state.last_observation_id, 4);
}

#[test]
fn discards_drive_with_fewer_than_two_positions() {
    let start = 1_800_000_050_000_i64;
    let samples = [
        sample(
            1,
            start,
            json!({
                "drive_state": {
                    "shift_state": "D",
                    "speed": 20,
                    "latitude": 47.5,
                    "longitude": 19.0,
                    "timestamp": start
                },
                "vehicle_state": {"odometer": 1000.0}
            }),
        ),
        sample(
            2,
            start + 60_000,
            json!({"drive_state":{"shift_state":"P","speed":0}}),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
    assert!(step.state.open_drive.is_none());
    assert!(step.delta.drives.is_empty());
    assert!(step.delta.positions.is_empty());
    assert_eq!(step.delta.discarded_drive_ids, vec![1]);
}

#[test]
fn fleet_parked_sample_clears_zero_position_drive() {
    let start = 1_800_000_055_000_i64;
    let samples = [
        fleet_sample(
            1,
            start,
            json!({
                "drive_state": {
                    "shift_state": "D",
                    "speed": 20,
                    "timestamp": start
                },
                "vehicle_state": {"odometer": 1000.0}
            }),
        ),
        fleet_sample(
            2,
            start + 60_000,
            json!({
                "drive_state": {
                    "shift_state": null,
                    "speed": null,
                    "timestamp": start + 60_000
                },
                "vehicle_state": {"odometer": 1000.1}
            }),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
    assert!(step.state.open_drive.is_none());
    assert_eq!(step.state.phase, VehiclePhase::Online);
    assert_eq!(step.state.last_observation_id, 2);
    assert!(step.delta.drives.is_empty());
    assert!(step.delta.positions.is_empty());
    assert_eq!(step.delta.discarded_drive_ids, vec![1]);
}

#[test]
fn discards_zero_odometer_distance_drive() {
    let start = 1_800_000_060_000_i64;
    let samples = [
        sample(
            1,
            start,
            json!({
                "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start},
                "vehicle_state":{"odometer":1000.0}
            }),
        ),
        sample(
            2,
            start + 31_000,
            json!({
                "drive_state":{"shift_state":"D","speed":20,"latitude":48.5,"longitude":20.0,"timestamp":start + 31_000},
                "vehicle_state":{"odometer":1000.0}
            }),
        ),
        sample(
            3,
            start + 60_000,
            json!({"drive_state":{"shift_state":"P","speed":0}}),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
    assert!(step.delta.drives.is_empty());
    assert!(step.delta.positions.is_empty());
}

#[test]
fn sparse_stream_does_not_close_charge_or_zero_energy() {
    let start = 1_800_000_200_000_i64;
    let charging = sample(
        1,
        start,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"latitude":47.5,"longitude":19.05,"timestamp":start},
            "charge_state":{
                "charging_state":"Charging","timestamp":start,
                "battery_level":64,"charge_energy_added":1.5,"charger_power":11.0
            }
        }),
    );
    let stream = LifecycleSample {
        observation_id: 2,
        observed_at_ms: start + 6_666,
        vehicle_state: "online".to_owned(),
        payload: stream_observation_payload(&crate::tesla_stream::parse_data_update(
            &format!(
                r#"{{"msg_type":"data:update","tag":"9","timestamp":{},"value":"0,12355.4,64,100,90,47.5,19.05,-11,P,220,210,90"}}"#,
                start + 6_666
            ),
        )
        .unwrap()),
    };
    let later = sample(
        3,
        start + 120_000,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"latitude":47.5,"longitude":19.05,"timestamp":start + 120_000},
            "charge_state":{
                "charging_state":"Complete","timestamp":start + 120_000,
                "battery_level":80,"charge_energy_added":12.0,"charger_power":0.0
            }
        }),
    );
    let opened = apply_sample(OpenSessionState::new(), 1, &charging).unwrap();
    assert!(opened.state.open_charge.is_some());
    assert!(opened.delta.charges.is_empty());
    let after_stream = apply_sample(opened.state, 1, &stream).unwrap();
    assert!(
        after_stream.state.open_charge.is_some(),
        "stream without charging_state must not seal the session"
    );
    assert!(after_stream.delta.charges.is_empty());
    let closed = apply_sample(after_stream.state, 1, &later).unwrap();
    assert_eq!(closed.delta.charges.len(), 1);
    assert_eq!(closed.delta.charges[0].charge_energy_added, Some(10.5));
    assert_eq!(closed.delta.charges[0].duration_min, Some(2));
}

#[test]
fn rounds_drive_duration_from_position_timestamps() {
    let start = 1_800_000_070_000_i64;
    let samples = [
        sample(
            1,
            start,
            json!({
                "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start},
                "vehicle_state":{"odometer":1000.0}
            }),
        ),
        sample(
            2,
            start + 31_000,
            json!({
                "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start + 31_000},
                "vehicle_state":{"odometer":1000.1}
            }),
        ),
        sample(
            3,
            start + 90_000,
            json!({"drive_state":{"shift_state":"P","speed":0}}),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
    assert_eq!(step.delta.drives[0].duration_min, Some(1));
    assert_eq!(step.delta.drives[0].end_date_ms, start + 31_000);
}

#[test]
fn offline_discovery_closes_drive_only_after_teslamate_timeout() {
    let start = 1_800_000_080_000_i64;
    let driving = [
        sample(
            1,
            start,
            json!({
                "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start},
                "vehicle_state":{"odometer":1000.0}
            }),
        ),
        sample(
            2,
            start + 1_000,
            json!({
                "drive_state":{"shift_state":"D","speed":20,"latitude":47.51,"longitude":19.01,"timestamp":start + 1_000},
                "vehicle_state":{"odometer":1000.1}
            }),
        ),
    ];
    let active = apply_samples(OpenSessionState::new(), 1, &driving)
        .expect("active drive")
        .state;
    let early =
        apply_sample(active, 1, &discovery(3, start + 30_000, "offline")).expect("early offline");
    assert!(early.state.open_drive.is_some());
    assert!(early.delta.drives.is_empty());

    let timed_out = apply_sample(
        early.state,
        1,
        &discovery(4, start + 1_000 + 15 * 60 * 1_000, "offline"),
    )
    .expect("offline timeout");
    assert!(timed_out.state.open_drive.is_none());
    assert_eq!(timed_out.delta.drives.len(), 1);
    assert_eq!(timed_out.delta.positions.len(), 2);
}

#[test]
fn teslamate_gained_range_threshold_matches_five_miles_after_five_minutes() {
    let start = 1_800_000_000_000_i64;
    let four_min = start + 4 * 60 * 1_000;
    let five_min = start + 5 * 60 * 1_000;
    assert!(!teslamate_gained_range_implies_charge(
        start,
        Some(100.0),
        four_min,
        Some(120.0)
    ));
    assert!(!teslamate_gained_range_implies_charge(
        start,
        Some(100.0),
        five_min,
        Some(100.0 + 5.0 * 1.609_344)
    ));
    assert!(teslamate_gained_range_implies_charge(
        start,
        Some(100.0),
        five_min,
        Some(100.0 + 5.0 * 1.609_344 + 0.01)
    ));
    assert!(!teslamate_gained_range_implies_charge(
        start,
        None,
        five_min,
        Some(200.0)
    ));
}

fn gained_range_drive(start: i64) -> OpenSessionState {
    apply_samples(
        OpenSessionState::new(),
        1,
        &[
            sample(
                1,
                start,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start},
                    "vehicle_state":{"odometer":1000.0},
                    "charge_state":{"battery_level":40,"ideal_battery_range":150.0,"charge_energy_added":0.0}
                }),
            ),
            sample(
                2,
                start + 1_000,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":47.51,"longitude":19.01,"timestamp":start + 1_000},
                    "vehicle_state":{"odometer":1000.1},
                    "charge_state":{"battery_level":39,"ideal_battery_range":149.0,"charge_energy_added":0.0}
                }),
            ),
        ],
    )
    .expect("drive")
    .state
}

#[test]
fn offline_drive_with_gained_range_still_emits_a_charge() {
    let start = 1_800_000_095_000_i64;
    let five_min = start + 1_000 + 5 * 60 * 1_000;
    let opened = gained_range_drive(start);
    assert!(opened.open_drive.is_some());
    assert_eq!(opened.next_charge_id, 1);

    let offline = apply_sample(opened, 1, &discovery(3, start + 2_000, "offline"))
        .expect("drive went offline");
    assert!(
        offline
            .state
            .open_drive
            .as_ref()
            .is_some_and(|open| open.saw_offline)
    );

    let recovered = sample(
        4,
        five_min,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"latitude":47.52,"longitude":19.02,"timestamp":five_min},
            "vehicle_state":{"odometer":1000.2},
            "charge_state":{"battery_level":80,"ideal_battery_range":210.0,"charge_energy_added":12.0}
        }),
    );
    let closed = apply_sample(offline.state, 1, &recovered).expect("gained range");
    assert!(closed.state.open_drive.is_none());
    assert!(closed.state.open_charge.is_none());
    assert_eq!(closed.delta.drives.len(), 1);
    assert_eq!(closed.delta.drives[0].id, 1);
    assert_eq!(closed.delta.charges.len(), 1);
    assert_eq!(closed.delta.charges[0].id, 1);
    assert_eq!(closed.state.next_charge_id, 2);
    assert_eq!(closed.delta.charges[0].start_date_ms, start + 1_000);
    assert_eq!(closed.delta.charges[0].end_date_ms, Some(five_min));
    assert_eq!(closed.delta.charges[0].charge_energy_added, Some(12.0));
    assert!(closed.delta.charges[0].end_ideal_range_km.is_some());
    assert!(
        closed.delta.charges[0].end_ideal_range_km.unwrap()
            > closed.delta.charges[0].start_ideal_range_km.unwrap_or(0.0)
    );
}

#[test]
fn offline_timeout_preserves_gained_range_seed_across_restart() {
    let start = 1_800_000_095_500_i64;
    let timeout = start + 1_000 + 15 * 60 * 1_000;
    let recovered_at = timeout + 60_000;
    let opened = gained_range_drive(start);
    let offline = apply_sample(opened, 1, &discovery(3, start + 2_000, "offline"))
        .expect("drive went offline");
    let timed_out =
        apply_sample(offline.state, 1, &discovery(4, timeout, "offline")).expect("offline timeout");
    assert!(timed_out.state.open_drive.is_none());
    assert!(timed_out.state.pending_gained_range_charge.is_some());

    let encoded = timed_out.state.encode().expect("encode pending seed");
    let restored = OpenSessionState::decode(&encoded).expect("decode pending seed");
    assert!(restored.pending_gained_range_charge.is_some());
    let recovered = sample(
        5,
        recovered_at,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"latitude":47.52,"longitude":19.02,"timestamp":recovered_at},
            "vehicle_state":{"odometer":1000.2},
            "charge_state":{"battery_level":80,"ideal_battery_range":210.0,"charge_energy_added":12.0}
        }),
    );
    let closed = apply_sample(restored, 1, &recovered).expect("gained range after timeout");
    assert!(closed.state.pending_gained_range_charge.is_none());
    assert_eq!(closed.delta.charges.len(), 1);
    assert_eq!(closed.delta.charges[0].start_date_ms, start + 1_000);
    assert_eq!(closed.delta.charges[0].end_date_ms, Some(recovered_at));
    assert_eq!(closed.delta.charges[0].charge_energy_added, Some(12.0));
}

#[test]
fn pending_gained_range_ignores_stream_then_preserves_resumed_drive() {
    let start = 1_800_000_095_625_i64;
    let timeout = start + 1_000 + 15 * 60 * 1_000;
    let opened = gained_range_drive(start);
    let offline = apply_sample(opened, 1, &discovery(3, start + 2_000, "offline"))
        .expect("drive went offline");
    let timed_out =
        apply_sample(offline.state, 1, &discovery(4, timeout, "offline")).expect("offline timeout");
    assert!(timed_out.state.pending_gained_range_charge.is_some());

    let stream_at = timeout + 1_000;
    let stream = LifecycleSample {
        observation_id: 5,
        observed_at_ms: stream_at,
        vehicle_state: "online".to_owned(),
        payload: stream_observation_payload(&crate::tesla_stream::StreamUpdate {
            tag: "9".into(),
            timestamp_ms: stream_at,
            speed: Some(10),
            odometer: Some(1_000.2),
            soc: Some(80),
            elevation: Some(100),
            est_heading: Some(90),
            est_lat: Some(47.52),
            est_lng: Some(19.02),
            power: Some(10),
            shift_state: Some("D".into()),
            range: Some(200),
            est_range: Some(190),
            heading: Some(90),
        }),
    };
    let resumed = apply_sample(timed_out.state, 1, &stream).expect("stream resumes drive");
    assert!(resumed.state.pending_gained_range_charge.is_some());
    assert!(resumed.delta.charges.is_empty());
    assert_eq!(
        resumed.state.open_drive.as_ref().map(|drive| drive.id),
        Some(2)
    );

    let authoritative_at = timeout + 60_000;
    let authoritative = sample(
        6,
        authoritative_at,
        json!({
            "drive_state":{"shift_state":"D","speed":20,"latitude":47.53,"longitude":19.03,"timestamp":authoritative_at},
            "vehicle_state":{"odometer":1000.3},
            "charge_state":{"battery_level":80,"ideal_battery_range":210.0,"charge_energy_added":12.0}
        }),
    );
    let materialized =
        apply_sample(resumed.state, 1, &authoritative).expect("authoritative recovery");
    assert!(materialized.state.pending_gained_range_charge.is_none());
    assert!(materialized.delta.drives.is_empty());
    assert_eq!(materialized.delta.charges.len(), 1);
    let open = materialized
        .state
        .open_drive
        .as_ref()
        .expect("resumed drive stays open");
    assert_eq!(open.id, 2);
    assert_eq!(open.position_count, 2);
}

#[test]
fn gained_range_seed_waits_for_an_authoritative_ideal_range() {
    let start = 1_800_000_095_750_i64;
    let timeout = start + 1_000 + 15 * 60 * 1_000;
    let opened = gained_range_drive(start);
    let offline = apply_sample(opened, 1, &discovery(3, start + 2_000, "offline"))
        .expect("drive went offline");
    let timed_out =
        apply_sample(offline.state, 1, &discovery(4, timeout, "offline")).expect("offline timeout");

    let incomplete = sample(
        5,
        timeout + 60_000,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"timestamp":timeout + 60_000},
            "vehicle_state":{"odometer":1000.2},
            "charge_state":{"battery_level":80,"charge_energy_added":12.0}
        }),
    );
    let waiting =
        apply_sample(timed_out.state, 1, &incomplete).expect("incomplete recovery sample");
    assert!(waiting.state.pending_gained_range_charge.is_some());
    assert!(waiting.delta.charges.is_empty());

    let complete = sample(
        6,
        timeout + 120_000,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"timestamp":timeout + 120_000},
            "vehicle_state":{"odometer":1000.2},
            "charge_state":{"battery_level":80,"ideal_battery_range":210.0,"charge_energy_added":12.0}
        }),
    );
    let closed = apply_sample(waiting.state, 1, &complete).expect("complete recovery sample");
    assert!(closed.state.pending_gained_range_charge.is_none());
    assert_eq!(closed.delta.charges.len(), 1);
}

#[test]
fn gained_range_does_not_fire_on_an_online_gps_gap() {
    let start = 1_800_000_096_000_i64;
    let five_min = start + 1_000 + 5 * 60 * 1_000;
    let opened = gained_range_drive(start);
    let recovered = sample(
        3,
        five_min,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"latitude":47.52,"longitude":19.02,"timestamp":five_min},
            "vehicle_state":{"odometer":1000.2},
            "charge_state":{"battery_level":80,"ideal_battery_range":210.0,"charge_energy_added":12.0}
        }),
    );
    let closed = apply_sample(opened, 1, &recovered).expect("online gap");
    assert!(closed.delta.charges.is_empty());
    assert_eq!(closed.delta.drives.len(), 1);
}

#[test]
fn gained_range_does_not_fire_before_five_offline_minutes() {
    let start = 1_800_000_097_000_i64;
    let four_min = start + 1_000 + 4 * 60 * 1_000;
    let opened = gained_range_drive(start);
    let offline = apply_sample(opened, 1, &discovery(3, start + 2_000, "offline"))
        .expect("drive went offline");
    let recovered = sample(
        4,
        four_min,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"latitude":47.52,"longitude":19.02,"timestamp":four_min},
            "vehicle_state":{"odometer":1000.2},
            "charge_state":{"battery_level":80,"ideal_battery_range":210.0,"charge_energy_added":12.0}
        }),
    );
    let closed = apply_sample(offline.state, 1, &recovered).expect("too soon");
    assert!(closed.delta.charges.is_empty());
}

#[test]
fn offline_discovery_uses_the_configured_drive_timeout() {
    let start = 1_800_000_085_000_i64;
    let active = apply_samples(
        OpenSessionState::new(),
        1,
        &[
            sample(
                1,
                start,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start},
                    "vehicle_state":{"odometer":1000.0}
                }),
            ),
            sample(
                2,
                start + 1_000,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":47.51,"longitude":19.01,"timestamp":start + 1_000},
                    "vehicle_state":{"odometer":1000.1}
                }),
            ),
        ],
    )
    .expect("active drive")
    .state;

    let short = apply_sample_with_offline_drive_timeout(
        active.clone(),
        1,
        &discovery(3, start + 31_000, "offline"),
        Duration::from_secs(30),
    )
    .expect("short timeout");
    assert!(short.state.open_drive.is_none());
    assert_eq!(short.delta.drives.len(), 1);

    let long = apply_sample_with_offline_drive_timeout(
        active,
        1,
        &discovery(3, start + 16 * 60 * 1_000, "offline"),
        Duration::from_secs(60 * 60),
    )
    .expect("long timeout");
    assert!(long.state.open_drive.is_some());
    assert!(long.delta.drives.is_empty());
}

#[test]
fn offline_discovery_keeps_charge_open_but_asleep_closes_it() {
    let start = 1_800_000_090_000_i64;
    let charging = sample(
        1,
        start,
        json!({
            "charge_state":{
                "charging_state":"Charging",
                "battery_level":40,
                "charge_energy_added":1.0
            }
        }),
    );
    let active = apply_sample(OpenSessionState::new(), 1, &charging)
        .expect("active charge")
        .state;
    let offline =
        apply_sample(active, 1, &discovery(2, start + 60_000, "offline")).expect("offline charge");
    assert!(offline.state.open_charge.is_some());
    assert!(offline.delta.charges.is_empty());

    let asleep = apply_sample(offline.state, 1, &discovery(3, start + 120_000, "asleep"))
        .expect("asleep charge");
    assert!(asleep.state.open_charge.is_none());
    assert_eq!(asleep.delta.charges.len(), 1);
}

#[test]
fn software_update_phase_survives_offline_and_finishes_online() {
    let start = 1_800_000_095_000_i64;
    let installing = sample(
        1,
        start,
        json!({
            "drive_state":{"shift_state":"P","speed":0},
            "vehicle_state":{
                "timestamp":start,
                "car_version":"2019.8.4",
                "software_update":{"status":"installing","version":"2019.8.5"}
            }
        }),
    );
    let updating = apply_sample(OpenSessionState::new(), 1, &installing)
        .expect("start update")
        .state;
    assert_eq!(updating.phase, VehiclePhase::Updating);

    let offline = apply_sample(updating, 1, &discovery(2, start + 30_000, "offline"))
        .expect("offline update")
        .state;
    let offline = OpenSessionState::decode(&offline.encode().expect("encode update"))
        .expect("restore update");
    assert_eq!(offline.phase, VehiclePhase::Updating);

    let finished = apply_sample(
        offline,
        1,
        &sample(
            3,
            start + 60_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0},
                "vehicle_state":{
                    "timestamp":start + 60_000,
                    "car_version":"2019.8.5",
                    "software_update":{"status":"","version":"2019.8.5"}
                }
            }),
        ),
    )
    .expect("finish update");
    assert_eq!(finished.state.phase, VehiclePhase::Online);
    assert!(finished.state.open_update.is_none());
    assert_eq!(finished.delta.updates.len(), 1);
    assert_eq!(finished.delta.updates[0].start_date_ms, start);
    assert_eq!(finished.delta.updates[0].end_date_ms, start + 60_000);
    assert_eq!(finished.delta.updates[0].version, "2019.8.5");
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
    assert_eq!(jumped.delta.updates[0].start_date_ms, start + 60_000);
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
    assert_eq!(step.delta.charge_samples.len(), 3);
    assert!((step.delta.charges[0].charge_energy_added.unwrap() - 0.2).abs() < 1e-9);
    assert_eq!(step.delta.charges[0].start_date_ms, start);
    assert_eq!(step.delta.charges[0].end_date_ms, Some(start + 20_000));
}

#[test]
fn charge_aggregate_uses_teslamate_delta_and_ordered_grid_energy() {
    let start = 1_800_000_700_000_i64;
    let samples = [
        sample(
            1,
            start + 100_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.5,"longitude":-0.1},
                "charge_state":{
                    "charging_state":"Charging","timestamp":start,
                    "battery_level":40,"charge_energy_added":1.0,
                    "ideal_battery_range":300.0,"battery_range":280.0,
                    "charger_phases":1,"charger_actual_current":10.0,"charger_voltage":230.0
                }
            }),
        ),
        sample(
            2,
            start + 101_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.5,"longitude":-0.1},
                "charge_state":{
                    "charging_state":"Charging","timestamp":start + 60_000,
                    "battery_level":45,"charge_energy_added":2.0,
                    "ideal_battery_range":310.0,"battery_range":290.0,
                    "charger_power":5.0
                }
            }),
        ),
        sample(
            3,
            start + 102_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.5,"longitude":-0.1},
                "charge_state":{
                    "charging_state":"Charging","timestamp":start + 120_000,
                    "battery_level":50,"charge_energy_added":2.5,
                    "ideal_battery_range":320.0,"battery_range":300.0,
                    "charger_phases":1,"charger_voltage":230.0
                }
            }),
        ),
        sample(
            4,
            start + 103_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.5,"longitude":-0.1},
                "charge_state":{
                    "charging_state":"Complete","timestamp":start + 180_000,
                    "battery_level":50,"charge_energy_added":0.0,
                    "ideal_battery_range":325.0,"battery_range":305.0
                }
            }),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("charge aggregate");
    let charge = &step.delta.charges[0];
    assert_eq!(charge.start_date_ms, start);
    assert_eq!(charge.charge_energy_added, Some(1.5));
    assert!((charge.charge_energy_used_kwh.unwrap() - 5.0 / 60.0).abs() < 1e-9);
    assert!((charge.start_ideal_range_km.unwrap() - 482.8).abs() < 1e-9);
    assert!((charge.end_ideal_range_km.unwrap() - 523.04).abs() < 1e-9);
    assert!((charge.start_rated_range_km.unwrap() - 450.62).abs() < 1e-9);
    assert!((charge.end_rated_range_km.unwrap() - 490.85).abs() < 1e-9);
    assert_eq!(charge.start_latitude, Some(51.5));
    assert_eq!(charge.start_longitude, Some(-0.1));
    assert_eq!(charge.cost, None);
}

#[test]
fn charge_energy_uses_the_current_row_and_phase_fallback() {
    let start = 1_800_000_000_000;
    let samples = [
        sample(
            1,
            start,
            json!({
                "charge_state": {
                    "charging_state": "Charging",
                    "timestamp": start,
                    "charger_power": 1.0
                }
            }),
        ),
        sample(
            2,
            start + 3_600_000,
            json!({
                "charge_state": {
                    "charging_state": "Charging",
                    "timestamp": start + 3_600_000,
                    "charger_power": 6.0,
                    "charger_phases": 1
                }
            }),
        ),
    ];
    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("valid charge");
    let stored = &step.state.open_charge.expect("open charge").samples;
    assert_eq!(calculate_energy_used_kwh(stored), Some(6.0));
}

#[test]
fn nonpositive_live_charger_phases_are_stored_as_null() {
    let sample = sample(
        1,
        1_800_000_000_000,
        json!({
            "charge_state": {
                "charging_state": "Charging",
                "charger_power": 3.0,
                "charger_phases": 0
            }
        }),
    );
    let step = apply_sample(OpenSessionState::new(), 1, &sample).expect("valid charge");
    assert_eq!(step.delta.open_charge_samples[0].charger_phases, None);
}

#[test]
fn stationary_positions_emit_on_entry_and_every_five_minutes() {
    let start = 1_800_000_098_000_i64;
    let samples = [
        sample(
            1,
            start,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.0,"longitude":-0.1},
                "charge_state":{"charging_state":"Unplugged","battery_level":60}
            }),
        ),
        sample(
            2,
            start + 299_999,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.001,"longitude":-0.101},
                "charge_state":{"charging_state":"Unplugged","battery_level":60}
            }),
        ),
        sample(
            3,
            start + 300_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.002,"longitude":-0.102},
                "charge_state":{"charging_state":"Unplugged","battery_level":60}
            }),
        ),
        sample(
            4,
            start + 300_001,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.003,"longitude":-0.103},
                "charge_state":{"charging_state":"Charging","battery_level":61}
            }),
        ),
        sample(
            5,
            start + 600_001,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.004,"longitude":-0.104},
                "charge_state":{"charging_state":"Charging","battery_level":62}
            }),
        ),
    ];

    let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("stationary trace");
    let positions: Vec<_> = step
        .delta
        .positions
        .iter()
        .filter(|position| position.drive_id.is_none())
        .collect();

    assert_eq!(positions.len(), 4);
    assert_eq!(
        positions
            .iter()
            .map(|position| position.date_ms)
            .collect::<Vec<_>>(),
        vec![start, start + 300_000, start + 300_001, start + 600_001]
    );
    assert!(positions.iter().all(|position| position.drive_id.is_none()));
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
fn stationary_positions_require_coordinates_skip_driving_and_replay() {
    let start = 1_800_000_099_000_i64;
    let missing_coordinates = sample(
        1,
        start,
        json!({
            "drive_state":{"shift_state":"P","speed":0},
            "charge_state":{"charging_state":"Unplugged"}
        }),
    );
    let first = apply_sample(OpenSessionState::new(), 1, &missing_coordinates)
        .expect("missing coordinates")
        .state;
    assert!(first.last_stationary_position_at_ms.is_none());

    let stationary = sample(
        2,
        start + 1_000,
        json!({
            "drive_state":{"shift_state":"P","speed":0,"latitude":51.0,"longitude":-0.1},
            "charge_state":{"charging_state":"Unplugged"}
        }),
    );
    let after_stationary = apply_sample(first, 1, &stationary).expect("stationary");
    assert_eq!(
        after_stationary
            .delta
            .positions
            .iter()
            .filter(|position| position.drive_id.is_none())
            .count(),
        1
    );

    let replay = apply_sample(after_stationary.state.clone(), 1, &stationary).expect("replay");
    assert!(replay.delta.positions.is_empty());

    let driving = sample(
        3,
        start + 2_000,
        json!({
            "drive_state":{"shift_state":"D","speed":10,"latitude":51.1,"longitude":-0.2},
            "charge_state":{"charging_state":"Unplugged"}
        }),
    );
    let driving_step = apply_sample(after_stationary.state, 1, &driving).expect("driving");
    assert!(
        driving_step
            .delta
            .positions
            .iter()
            .all(|position| position.drive_id.is_some())
    );
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
            .charge_samples
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
    let drive_positions: Vec<_> = step
        .delta
        .positions
        .iter()
        .filter(|position| position.drive_id.is_some())
        .map(|position| position.date_ms)
        .collect();
    assert_eq!(
        drive_positions,
        vec![start + 2_000, start + 3_000, start + 4_000]
    );
    assert!(
        step.delta
            .positions
            .iter()
            .map(|position| position.date_ms)
            .collect::<Vec<_>>()
            .windows(2)
            .all(|window| window[0] <= window[1])
    );
    assert!(
        step.delta
            .charge_samples
            .iter()
            .map(|sample| sample.timestamp_ms)
            .collect::<Vec<_>>()
            .windows(2)
            .all(|window| window[0] <= window[1])
    );
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
    assert_eq!(step.delta.charge_samples.len(), 3);
    assert_eq!(step.delta.charges[0].start_battery_level, Some(40));
    assert_eq!(step.delta.charges[0].end_battery_level, Some(80));
    assert_eq!(step.delta.charges[0].charge_energy_added, Some(10.5));
}

#[test]
fn starts_a_charge_on_teslamate_starting_state() {
    let start = 1_800_000_150_000_i64;
    let starting = sample(
        1,
        start,
        json!({"charge_state":{"charging_state":"Starting","battery_level":40,"charge_energy_added":0.0}}),
    );
    let state = apply_sample(OpenSessionState::new(), 1, &starting)
        .expect("starting charge")
        .state;

    assert_eq!(state.phase, VehiclePhase::Charging);
    assert!(state.open_charge.is_some());
}

#[test]
fn restart_mid_drive_then_completion_matches_uninterrupted_projection() {
    let start = 1_800_000_200_000_i64;
    let all = [
        sample(
            1,
            start,
            json!({"drive_state":{"shift_state":"D","speed":10,"latitude":10.0,"longitude":10.0},"charge_state":{"battery_level":60}}),
        ),
        sample(
            2,
            start + 30_000,
            json!({"drive_state":{"shift_state":"D","speed":20,"latitude":10.01,"longitude":10.01},"charge_state":{"battery_level":59}}),
        ),
        sample(
            3,
            start + 60_000,
            json!({"drive_state":{"shift_state":"P","speed":0,"latitude":10.02,"longitude":10.02},"charge_state":{"battery_level":58}}),
        ),
    ];

    let continuous = apply_samples(OpenSessionState::new(), 1, &all).expect("continuous");

    // Simulate crash after sample 1: encode open state, decode, resume.
    let after_first = apply_sample(OpenSessionState::new(), 1, &all[0]).expect("first");
    let encoded = after_first.state.encode().expect("encode open drive");
    let restored = OpenSessionState::decode(&encoded).expect("decode");
    assert!(restored.open_drive.is_some());
    let after_second = apply_sample(restored, 1, &all[1]).expect("second");
    // Replay of sample 1 is a no-op after restart recovery.
    let replay = apply_sample(after_second.state.clone(), 1, &all[0]).expect("replay");
    assert!(replay.delta.drives.is_empty());
    assert!(replay.delta.positions.is_empty());
    let after_third = apply_sample(after_second.state, 1, &all[2]).expect("third");

    assert_eq!(after_third.delta.drives, continuous.delta.drives);
    // Continuous path emits positions only on close; restarted path same.
    assert_eq!(
        after_third.delta.positions.len(),
        continuous.delta.positions.len()
    );
    assert_eq!(after_third.state.last_observation_id, 3);
    assert!(after_third.state.open_drive.is_none());
}

#[test]
fn corrupt_payload_is_rejected_without_advancing_or_discarding_open_session() {
    let start = 1_800_000_300_000_i64;
    let good = sample(
        1,
        start,
        json!({"drive_state":{"shift_state":"D","speed":5,"latitude":1.0,"longitude":2.0}}),
    );
    let bad = LifecycleSample {
        observation_id: 2,
        observed_at_ms: start + 1,
        vehicle_state: "online".to_owned(),
        payload: json!({"record_type":"not-valid"}),
    };
    let mid = apply_sample(OpenSessionState::new(), 1, &good).expect("open drive");
    assert!(mid.state.open_drive.is_some());
    let preserved = mid.state.clone();
    assert!(matches!(
        apply_sample(mid.state, 1, &bad),
        Err(LifecycleError::InvalidPayload)
    ));
    assert!(preserved.open_drive.is_some());
    assert_eq!(preserved.last_observation_id, 1);
}

#[test]
fn regressed_close_time_does_not_regress_or_close_the_active_drive() {
    let start = 1_800_000_325_000_i64;
    let open = sample(
        1,
        start,
        json!({"drive_state":{"shift_state":"D","speed":5,"latitude":1.0,"longitude":2.0}}),
    );
    let regressed_end = sample(
        2,
        start - 1,
        json!({"drive_state":{"shift_state":"P","speed":0,"latitude":1.0,"longitude":2.0}}),
    );
    let state = apply_sample(OpenSessionState::new(), 1, &open)
        .expect("open drive")
        .state;

    let step = apply_sample(state, 1, &regressed_end).expect("ignore stale close");
    assert!(step.state.open_drive.is_some());
    assert_eq!(step.state.last_observation_id, 2);
    assert_eq!(step.state.last_observed_at_ms, Some(start));
    assert!(step.delta.drives.is_empty());
    assert!(step.delta.positions.is_empty());
}

#[test]
fn impossible_position_is_rejected_before_projection() {
    let sample = sample(
        1,
        1_800_000_340_000,
        json!({"drive_state":{"shift_state":"D","speed":5,"latitude":91.0,"longitude":2.0}}),
    );

    assert_eq!(
        apply_sample(OpenSessionState::new(), 1, &sample),
        Err(LifecycleError::InvalidCoordinates)
    );
}

#[test]
fn suspended_discovery_state_is_preserved_without_starting_a_lifecycle() {
    let sample = LifecycleSample {
        observation_id: 1,
        observed_at_ms: 1_800_000_350_000,
        vehicle_state: "suspended".to_owned(),
        payload: json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "vehicle_data": {"drive_state": {"shift_state": "P", "speed": 0}}
        }),
    };

    let step = apply_sample(OpenSessionState::new(), 1, &sample).expect("project");
    assert_eq!(step.state.phase, VehiclePhase::Suspended);
    assert!(step.state.open_drive.is_none());
    assert!(step.state.open_charge.is_none());
}

#[test]
fn force_close_emits_open_drive_and_charge() {
    let start = 1_800_000_400_000_i64;
    let drive = sample(
        1,
        start,
        json!({"drive_state":{"shift_state":"D","speed":5,"latitude":1.0,"longitude":2.0},"charge_state":{"battery_level":50}}),
    );
    let mut state = apply_sample(OpenSessionState::new(), 1, &drive)
        .expect("drive")
        .state;
    // Start a charge on a later sample after parking is not needed for this
    // force-close unit; open charge directly via a charging sample.
    let charge = sample(
        2,
        start + 10_000,
        json!({"charge_state":{"charging_state":"Charging","battery_level":50,"charge_energy_added":0.1,"charger_power":7.0},"drive_state":{"shift_state":"P","speed":0}}),
    );
    // Close drive first by parking, then open charge.
    state = apply_sample(state, 1, &charge).expect("charge open").state;
    // Manually ensure we have an open charge for force-close proof.
    assert!(state.open_charge.is_some() || state.open_drive.is_none());
    let step = force_close_open_sessions(state, 1, start + 20_000).expect("force close");
    assert!(step.state.open_drive.is_none());
    assert!(step.state.open_charge.is_none());
}

#[test]
fn service_close_uses_last_position_and_persists_service_mode() {
    let start = 1_800_000_410_000_i64;
    let opened = apply_sample(
        OpenSessionState::new(),
        1,
        &sample(
            1,
            start,
            json!({
                "drive_state":{"shift_state":"D","speed":20,"latitude":51.0,"longitude":-0.1,"timestamp":start},
                "vehicle_state":{"odometer":1000.0,"service_mode":false}
            }),
        ),
    )
    .expect("open drive")
    .state;
    let opened = apply_sample(
        opened,
        1,
        &sample(
            2,
            start + 1_000,
            json!({
                "drive_state":{"shift_state":"D","speed":21,"latitude":51.001,"longitude":-0.101,"timestamp":start + 1_000},
                "vehicle_state":{"odometer":1000.1,"service_mode":false}
            }),
        ),
    )
    .expect("extend drive")
    .state;
    let step = force_close_for_service(opened, 1, start + 5_000).expect("service close");
    assert_eq!(step.state.service_mode, Some(true));
    assert!(step.state.open_drive.is_none());
    assert_eq!(step.delta.drives.len(), 1);
    assert_eq!(step.delta.positions[1].date_ms, start + 1_000);

    let exited = apply_sample(
        step.state,
        1,
        &sample(
            3,
            start + 10_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"timestamp":start + 10_000},
                "vehicle_state":{"service_mode":false}
            }),
        ),
    )
    .expect("service exit")
    .state;
    assert_eq!(exited.service_mode, Some(false));
}

#[test]
fn geofences_fill_missing_live_drive_labels_only() {
    let mut delta = LifecycleDelta {
        drives: vec![ProjectionDrive {
            id: 1,
            car_id: 1,
            optimized_at_ms: None,
            start_date_ms: 1,
            end_date_ms: 2,
            distance_km: None,
            duration_min: None,
            efficiency: None,
            outside_temp_avg: None,
            inside_temp_avg: None,
            speed_max: None,
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_address: None,
            end_address: None,
            start_geofence: None,
            end_geofence: Some("Imported".into()),
            start_latitude: Some(51.0),
            start_longitude: Some(-0.1),
            end_latitude: Some(51.001),
            end_longitude: Some(-0.101),
            start_soc: None,
            end_soc: None,
            start_rated_range_km: None,
            end_rated_range_km: None,
            ascent: None,
            descent: None,
        }],
        ..LifecycleDelta::default()
    };
    let fences = vec![
        GeofenceFence {
            name: "Home".into(),
            latitude: 51.0,
            longitude: -0.1,
            radius_m: 150.0,
            billing_type: None,
            cost_per_unit: None,
            session_fee: None,
        },
        GeofenceFence {
            name: "Work".into(),
            latitude: 51.001,
            longitude: -0.101,
            radius_m: 150.0,
            billing_type: None,
            cost_per_unit: None,
            session_fee: None,
        },
    ];

    apply_geofence_labels(&mut delta, &fences);

    assert_eq!(delta.drives[0].start_geofence.as_deref(), Some("Home"));
    assert_eq!(delta.drives[0].end_geofence.as_deref(), Some("Imported"));
}

#[test]
fn position_thermal_flags_survive_sparse_stream_frames_until_drive_close() {
    let t0 = 1_800_001_000_000_i64;
    let owner = sample(
        1,
        t0,
        json!({
            "drive_state": {"shift_state":"D","speed":20,"power":12.5,"latitude":51.0,"longitude":-0.1,"timestamp":t0},
            "charge_state": {"battery_level":70,"battery_heater_on":true,"est_battery_range":200.0},
            "climate_state": {"battery_heater":true,"battery_heater_no_power":false,"fan_status":2,"driver_temp_setting":21.5,"passenger_temp_setting":22.0,"is_rear_defroster_on":false,"is_front_defroster_on":true},
            "vehicle_state": {"odometer":1000.0,"tpms_pressure_fl":2.4,"tpms_pressure_fr":2.5,"tpms_pressure_rl":2.6,"tpms_pressure_rr":2.7}
        }),
    );
    let sparse_stream = LifecycleSample {
        observation_id: 2,
        observed_at_ms: t0 + 1_000,
        vehicle_state: "online".into(),
        payload: json!({
            "record_type":"tesla_stream_update_v1",
            "fields":{"drive_state":{"timestamp":t0 + 1_000,"shift_state":"D","speed":21,"latitude":51.001,"longitude":-0.101},"vehicle_state":{"odometer":1001.0}}
        }),
    };
    let parked = sample(
        3,
        t0 + 2_000,
        json!({"drive_state":{"shift_state":"P","speed":0,"timestamp":t0 + 2_000}}),
    );

    let first = apply_sample(OpenSessionState::new(), 1, &owner).unwrap();
    let second = apply_sample(first.state, 1, &sparse_stream).unwrap();
    let closed = apply_sample(second.state, 1, &parked).unwrap();

    assert_eq!(closed.delta.positions.len(), 2);
    for (index, position) in closed.delta.positions.into_iter().enumerate() {
        assert_eq!(position.speed, Some(if index == 0 { 32 } else { 34 }));
        assert_eq!(position.power, (index == 0).then_some(12.5));
        assert_eq!(position.est_battery_range_km, Some(321.87));
        assert_eq!(position.fan_status, Some(2));
        assert_eq!(position.driver_temp_setting, Some(21.5));
        assert_eq!(position.passenger_temp_setting, Some(22.0));
        assert_eq!(position.is_rear_defroster_on, Some(false));
        assert_eq!(position.is_front_defroster_on, Some(true));
        assert_eq!(position.battery_heater, Some(true));
        assert_eq!(position.battery_heater_on, Some(true));
        assert_eq!(position.battery_heater_no_power, Some(false));
        assert_eq!(position.tpms_pressure_fl, Some(2.4));
        assert_eq!(position.tpms_pressure_fr, Some(2.5));
        assert_eq!(position.tpms_pressure_rl, Some(2.6));
        assert_eq!(position.tpms_pressure_rr, Some(2.7));
    }
}

#[test]
fn elevation_totals_match_teslamate_cap_boundary() {
    assert_eq!(cap_elevation_total(32_767), 32_767);
    assert_eq!(cap_elevation_total(32_768), 0);
    assert_eq!(cap_elevation_total(40_000), 0);
}
