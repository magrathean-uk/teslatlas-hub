// SPDX-License-Identifier: AGPL-3.0-only
use super::*;
use crate::{
    current_state::build_current_vehicle_summary,
    db::{SourceDescriptor, VehicleDescriptor},
};
use uuid::Uuid;

fn sample() -> TeslaMateCurrentPosition {
    TeslaMateCurrentPosition {
        car_id: 1,
        eid: 42,
        vin: Some("fixture-vin".into()),
        position_id: 7,
        observed_at_ms: 1_000_000,
        source_now_ms: 1_010_000,
        battery_level: Some(73),
        ideal_range_km: Some(301.25),
        est_range_km: Some(271.75),
        rated_range_km: Some(284.5),
        odometer_km: Some(102_400.75),
    }
}
#[test]
fn teslamate_current_preserves_source_timestamp_native_km_and_sparse_projection() {
    let temp = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temp.path()).unwrap();
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "fixture"), 1)
        .unwrap();
    let mut descriptor = VehicleDescriptor::new(source.source_id, "eid:42");
    descriptor.vin = Some("fixture-vin".into());
    let vehicle = store.register_vehicle(&descriptor, 2).unwrap();
    let payload = observation(&sample(), 1_010_000).unwrap();
    let input = ObservationInput {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        observed_at_ms: sample().observed_at_ms,
        payload,
    };
    assert!(store.record_teslamate_current(&input, 1_010_000).unwrap());
    let first = store
        .current_observations_for_vehicle(vehicle.vehicle_id)
        .unwrap();
    assert!(!store.record_teslamate_current(&input, 1_040_000).unwrap());
    assert_eq!(
        first,
        store
            .current_observations_for_vehicle(vehicle.vehicle_id)
            .unwrap()
    );
    assert!(
        store
            .observations_for_vehicle(
                vehicle.vehicle_id,
                crate::db::ObservationQuery::from_start(10)
            )
            .unwrap()
            .is_empty()
    );
    let mut older_owner = input.clone();
    older_owner.observed_at_ms -= 1;
    older_owner.payload = serde_json::json!({"record_type":"owner_api_vehicle_data_v1",
        "response":{"drive_state":{"latitude":51.0,"longitude":0.1},
        "charge_state":{"battery_level":50,"battery_range":200.0},"vehicle_state":{"odometer":100.0}}});
    store.append_observation(&older_owner, 1_010_000).unwrap();
    let observations = store
        .current_observations_for_vehicle(vehicle.vehicle_id)
        .unwrap();
    let summary =
        build_current_vehicle_summary(vehicle.vehicle_id, &observations, None, None, None);
    assert_eq!(summary.observed_at_ms, Some(1_000_000));
    assert_eq!(summary.battery_level, Some(73));
    assert_eq!(summary.ideal_battery_range_km, Some(301.25));
    assert_eq!(summary.est_battery_range_km, Some(271.75));
    assert_eq!(summary.rated_battery_range_km, Some(284.5));
    assert_eq!(summary.odometer, Some(102_400.75));
    assert_eq!(summary.latitude, None);
    assert_eq!(summary.longitude, None);
    assert_eq!(summary.charging_state, None);
    assert_eq!(summary.since, None);
    let mut old = input.clone();
    old.observed_at_ms -= 10;
    assert!(!store.record_teslamate_current(&old, 1_050_000).unwrap());
    assert_eq!(
        store
            .current_observations_for_vehicle(vehicle.vehicle_id)
            .unwrap(),
        observations
    );
}

#[test]
fn teslamate_current_rejects_null_stale_future_skew_and_invalid_numbers() {
    let valid = sample();
    assert!(observation(&valid, 1_010_000).is_some());
    assert!(observation(&valid, 1_400_001).is_none());
    assert!(observation(&valid, 999_999).is_none());
    let mut invalid = sample();
    invalid.source_now_ms = 999_999;
    assert!(observation(&invalid, 1_010_000).is_none());
    invalid.source_now_ms = i64::MIN;
    assert!(observation(&invalid, 1_010_000).is_none());
    for battery in [None, Some(-1), Some(101)] {
        let mut invalid = sample();
        invalid.battery_level = battery;
        assert!(
            observation(&invalid, 1_010_000)
                .unwrap()
                .get("battery_level")
                .is_none()
        );
    }
    for value in [
        None,
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(-1.0),
        Some(2001.0),
    ] {
        let mut invalid = sample();
        invalid.ideal_range_km = value;
        assert!(
            observation(&invalid, 1_010_000)
                .unwrap()
                .get("ideal_battery_range_km")
                .is_none()
        );
    }
    let mut invalid = sample();
    invalid.odometer_km = None;
    assert!(
        observation(&invalid, 1_010_000)
            .unwrap()
            .get("odometer_km")
            .is_none()
    );
}

#[test]
fn teslamate_current_partial_samples_omit_missing_or_invalid_fields_without_inference() {
    let mut partial = sample();
    partial.ideal_range_km = None;
    partial.est_range_km = None;
    partial.rated_range_km = None;
    partial.odometer_km = None;
    let battery_only = observation(&partial, 1_010_000).unwrap();
    assert_eq!(battery_only["battery_level"], 73);
    assert!(battery_only.get("est_battery_range_km").is_none());
    partial.battery_level = None;
    partial.odometer_km = Some(123.5);
    let odo_only = observation(&partial, 1_010_000).unwrap();
    assert_eq!(odo_only["odometer_km"], 123.5);
    assert!(odo_only.get("battery_level").is_none());
    partial.est_range_km = Some(f64::NAN);
    assert!(
        observation(&partial, 1_010_000)
            .unwrap()
            .get("est_battery_range_km")
            .is_none()
    );
    partial.odometer_km = None;
    assert!(observation(&partial, 1_010_000).is_none());
    partial.ideal_range_km = Some(250.0);
    let ideal_only = observation(&partial, 1_010_000).unwrap();
    assert!(ideal_only.get("est_battery_range_km").is_none());
    assert_eq!(ideal_only["ideal_battery_range_km"], 250.0);
}

#[test]
fn teslamate_current_resolves_only_exact_existing_source_vehicle_identity() {
    let temp = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temp.path()).unwrap();
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "fixture"), 1)
        .unwrap();
    let mut descriptor = VehicleDescriptor::new(source.source_id, "eid:42");
    descriptor.vin = Some("fixture-vin".into());
    let vehicle = store.register_vehicle(&descriptor, 2).unwrap();
    assert_eq!(
        store
            .teslamate_current_source("fixture", vehicle.vehicle_id, "eid:42", Some("fixture-vin"))
            .unwrap(),
        Some(source.source_id)
    );
    for (key, id, stable, vin) in [
        ("wrong", vehicle.vehicle_id, "eid:42", Some("fixture-vin")),
        ("fixture", Uuid::new_v4(), "eid:42", Some("fixture-vin")),
        ("fixture", vehicle.vehicle_id, "eid:43", Some("fixture-vin")),
        ("fixture", vehicle.vehicle_id, "eid:42", Some("wrong-vin")),
        ("fixture", vehicle.vehicle_id, "eid:42", None),
    ] {
        assert!(
            store
                .teslamate_current_source(key, id, stable, vin)
                .unwrap()
                .is_none()
        );
    }
    store.retire_vehicle(vehicle.vehicle_id, 3).unwrap();
    assert!(
        store
            .teslamate_current_source("fixture", vehicle.vehicle_id, "eid:42", Some("fixture-vin"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn teslamate_current_is_optional_and_connection_config_rejects_inline_secrets() {
    assert!(crate::config::TeslaMateConfig::default().current.is_none());
    let mut config = TeslaMateCurrentConfig {
        source_url: "postgresql://reader@127.0.0.1/fixture".into(),
        source_key: "fixture".into(),
        vehicle_id: Uuid::new_v4(),
        car_id: 1,
        password_file: "/private/fixture/password".into(),
    };
    config.validate().unwrap();
    config.source_url = "postgresql://reader:secret@127.0.0.1/fixture".into();
    assert!(config.validate().is_err());
    config.source_url = "postgresql://reader@127.0.0.1/fixture".into();
    config.car_id = 0;
    assert!(config.validate().is_err());
}

#[test]
fn teslamate_current_password_reader_is_private_bounded_and_rejects_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temporary = crate::private_tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let path = root.join("password");
    std::fs::write(&path, b"fixture-password\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(password(&path).is_ok());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(password(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let alias = root.join("alias");
    symlink(&path, &alias).unwrap();
    assert!(password(&alias).is_err());
    std::fs::write(&path, vec![b'x'; 16 * 1024 + 1]).unwrap();
    assert!(password(&path).is_err());
}
