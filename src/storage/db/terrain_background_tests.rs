// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{
    hub_pack::ProjectionPosition,
    lifecycle::{LifecycleDelta, OpenSessionState},
};

fn position(id: i64, elevation: Option<i64>) -> ProjectionPosition {
    ProjectionPosition {
        id,
        drive_id: Some(7),
        car_id: 7,
        date_ms: id * 1_000,
        latitude: 51.0,
        longitude: -0.1,
        speed: Some(20),
        power: None,
        battery_level: Some(80),
        usable_battery_level: None,
        elevation,
        odometer: None,
        ideal_battery_range_km: None,
        est_battery_range_km: None,
        rated_battery_range_km: None,
        fan_status: None,
        driver_temp_setting: None,
        passenger_temp_setting: None,
        is_climate_on: None,
        is_rear_defroster_on: None,
        is_front_defroster_on: None,
        inside_temp: None,
        outside_temp: None,
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        tpms_pressure_fl: None,
        tpms_pressure_fr: None,
        tpms_pressure_rl: None,
        tpms_pressure_rr: None,
    }
}

#[test]
fn lifecycle_position_replay_after_import_is_idempotent() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("replay_test", "one"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "7"), 1_000)
        .expect("vehicle");
    let encoded = OpenSessionState::new().encode().expect("open state");
    let first = position(42, None);
    let first_delta = LifecycleDelta {
        positions: vec![first],
        ..Default::default()
    };
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 7,
            open_session_json: &encoded,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 1_000,
            delta: &first_delta,
        })
        .expect("initial materialisation");

    let mut replay = position(42, None);
    replay.speed = Some(30);
    let replay_delta = LifecycleDelta {
        positions: vec![replay],
        ..Default::default()
    };
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 7,
            open_session_json: &encoded,
            last_observation_id: 2,
            quarantined: false,
            updated_at_ms: 2_000,
            delta: &replay_delta,
        })
        .expect("replayed imported position");

    let connection = store.open().expect("open");
    let (count, speed): (i64, Option<i64>) = connection
        .query_row(
            "SELECT COUNT(*), MAX(speed) FROM materialised_positions
             WHERE vehicle_id = ?1 AND position_id = 42",
            params![vehicle.vehicle_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("replayed row");
    assert_eq!(count, 1);
    assert_eq!(speed, Some(30));
}

#[test]
fn tesla_eid_unifies_sources_and_survives_restart() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let imported = store
        .register_source(&SourceDescriptor::new("teslamate", "copy"), 1)
        .unwrap();
    let live = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            2,
        )
        .unwrap();
    let first = store
        .register_vehicle(
            &VehicleDescriptor::new(imported.source_id, "eid:700")
                .with_tesla_identity(Some(700), Some(900)),
            1,
        )
        .unwrap();
    let second = store
        .register_vehicle(
            &VehicleDescriptor::new(live.source_id, "700").with_tesla_identity(Some(700), None),
            2,
        )
        .unwrap();
    assert_eq!(first.vehicle_id, second.vehicle_id);
    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("reopen");
    let third = reopened
        .register_vehicle(
            &VehicleDescriptor::new(live.source_id, "700").with_tesla_identity(Some(700), None),
            3,
        )
        .unwrap();
    assert_eq!(first.vehicle_id, third.vehicle_id);
}

#[test]
fn distinct_eid_cars_do_not_merge_on_reused_vid_and_conflicts_fail() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "copy"), 1)
        .unwrap();
    let one = store
        .register_vehicle(
            &VehicleDescriptor {
                source_id: source.source_id,
                source_vehicle_key: "eid:701".into(),
                vin: Some("VIN-701".into()),
                display_name: None,
                tesla_eid: Some(701),
                tesla_vid: Some(901),
            },
            1,
        )
        .unwrap();
    let two = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "eid:702")
                .with_tesla_identity(Some(702), Some(901)),
            2,
        )
        .unwrap();
    assert_ne!(one.vehicle_id, two.vehicle_id);
    let vin_conflict = store.register_vehicle(
        &VehicleDescriptor {
            source_id: source.source_id,
            source_vehicle_key: "eid:703".into(),
            vin: Some("VIN-OTHER".into()),
            display_name: None,
            tesla_eid: Some(701),
            tesla_vid: None,
        },
        4,
    );
    assert!(matches!(
        vin_conflict,
        Err(StoreError::VehicleIdentityConflict)
    ));
}
