// SPDX-License-Identifier: AGPL-3.0-only

//! C6 Part S2: the status command's vehicle fields speak the summary's
//! vocabulary and take the location name from the summary's coordinate pair
//! (C6 ST-S32, R29, R41). Synthetic data only.

use std::{fs, os::unix::fs::PermissionsExt};

use serde_json::{Value, json};
use teslatlas_hub::db::{HubStore, ObservationInput, SourceDescriptor, VehicleDescriptor};
use uuid::Uuid;

use crate::{CurrentVehicleStatusFields, current_vehicle_status_fields};

const T0: i64 = 1_760_000_000_000;
/// The default silence tolerance, C1's `DEFAULT_OFFLINE_DRIVE_TIMEOUT`.
const T: i64 = 600_000;

/// An Owner-wire vehicle-data observation stating `groups`, each group stamped
/// with the observation time.
fn vehicle_data(groups: Value) -> Value {
    let mut data = groups;
    for (_, members) in data.as_object_mut().expect("groups") {
        members["timestamp"] = json!(T0);
    }
    json!({
        "record_type": "owner_api_vehicle_data_v1",
        "source_vehicle_state": "online",
        "vehicle_data": data
    })
}

fn discovery(state: &str) -> Value {
    json!({
        "record_type": "owner_api_discovery_v1",
        "source_vehicle_state": state
    })
}

#[test]
fn c6_st_s32_status() {
    let temporary = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("private data directory");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("tesla_owner_api", "status-s2-test"),
            1_000,
        )
        .expect("source");
    let register = |key: &str| {
        store
            .register_vehicle(&VehicleDescriptor::new(source.source_id, key), 1_001)
            .expect("vehicle")
            .vehicle_id
    };
    let accept = |vehicle_id: Uuid, car_id: i64, observed_at_ms: i64, payload: Value| {
        store
            .accept_owner_observation_and_lifecycle(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id,
                    observed_at_ms,
                    payload,
                },
                observed_at_ms + 1,
                car_id,
            )
            .expect("observation and lifecycle");
    };

    // C6 ST-S2's vehicle: a drive in progress.
    let driving = register("driving");
    accept(
        driving,
        1,
        T0,
        vehicle_data(json!({
            "drive_state": {
                "latitude": 10.2, "longitude": 20.8, "speed": 30, "shift_state": "D"
            },
            "charge_state": {"battery_level": 79}
        })),
    );
    // C6 ST-S7's vehicle: it was seen online, then asleep two hours later.
    let asleep = register("asleep");
    accept(
        asleep,
        2,
        T0,
        vehicle_data(json!({
            "drive_state": {"latitude": 10.2, "longitude": 20.8},
            "charge_state": {"battery_level": 80}
        })),
    );
    accept(asleep, 2, T0 + 7_200_000, discovery("asleep"));
    // C6 ST-S1's vehicle: parked inside a stored fence "Home" of radius 100 m.
    let parked = register("parked");
    store
        .upsert_geofences(
            parked,
            &[teslatlas_hub::teslamate_projection::TeslaMateGeofence {
                id: 1,
                name: "Home".into(),
                latitude: Some(10.123457),
                longitude: Some(20.765432),
                radius_m: Some(100.0),
                billing_type: Some(teslatlas_hub::hub_pack::GeofenceBillingType::PerKwh),
                cost_per_unit: None,
                session_fee: None,
            }],
        )
        .expect("geofence");
    accept(
        parked,
        3,
        T0,
        vehicle_data(json!({
            "drive_state": {"latitude": 10.1234567, "longitude": 20.7654321},
            "charge_state": {"battery_level": 80}
        })),
    );
    // C6 ST-S12's vehicle: no position at all.
    let positionless = register("positionless");
    accept(
        positionless,
        4,
        T0,
        vehicle_data(json!({"charge_state": {"battery_level": 60}})),
    );
    let never_seen = register("never-seen");

    let read_only = HubStore::open_read_only(temporary.path()).expect("read-only store");
    let status = |vehicle_id: Uuid, now_ms: i64| {
        current_vehicle_status_fields(&read_only, vehicle_id, now_ms, T).expect("status")
    };
    assert_eq!(
        status(driving, T0 + 1_000),
        CurrentVehicleStatusFields {
            activity_state: Some("driving".into()),
            battery_level: Some(79),
            location_name: None,
            telemetry_status: "cached",
        }
    );
    // A sleeping car keeps its persistent battery level and is named by the
    // summary's vocabulary; its old position names no fence here.
    assert_eq!(
        status(asleep, T0 + 7_200_000),
        CurrentVehicleStatusFields {
            activity_state: Some("asleep".into()),
            battery_level: Some(80),
            location_name: None,
            telemetry_status: "cached",
        }
    );
    assert_eq!(
        status(parked, T0 + 1_000),
        CurrentVehicleStatusFields {
            activity_state: Some("online".into()),
            battery_level: Some(80),
            location_name: Some("Home".into()),
            telemetry_status: "cached",
        }
    );
    assert_eq!(
        status(positionless, T0 + 1_000),
        CurrentVehicleStatusFields {
            activity_state: Some("online".into()),
            battery_level: Some(60),
            location_name: None,
            telemetry_status: "cached",
        }
    );
    assert_eq!(
        status(never_seen, T0 + 1_000),
        CurrentVehicleStatusFields {
            activity_state: None,
            battery_level: None,
            location_name: None,
            telemetry_status: "unknown",
        }
    );
    // The vocabulary is the summary's: never `unknown`, `suspended` or
    // `unavailable`.
    for vehicle_id in [driving, asleep, parked, positionless, never_seen] {
        let state = status(vehicle_id, T0 + 1_000).activity_state;
        assert!(
            !matches!(
                state.as_deref(),
                Some("unknown" | "suspended" | "unavailable")
            ),
            "status vocabulary: {state:?}"
        );
    }
}
