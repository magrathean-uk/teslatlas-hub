// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

const VIN: &str = "5YJ3E1EA7KF000001";
const T0: i64 = 1_800_000_000_000;

fn transaction(timestamp_ms: i64, data: Value) -> Vec<u8> {
    transaction_with_versions(timestamp_ms, data, Some("1.3.0"), None)
}

fn transaction_with_versions(
    timestamp_ms: i64,
    data: Value,
    client_version: Option<&str>,
    firmware_version: Option<&str>,
) -> Vec<u8> {
    let mut envelope = json!({
        "version": 1,
        "vin": VIN,
        "txid": format!("tx-{timestamp_ms}"),
        "tx_type": "vehicle_data",
        "received_at_ms": timestamp_ms + 100,
        "timestamp_ms": timestamp_ms,
        "payload": {
            "vin": VIN,
            "createdAt": "2027-01-15T08:00:00Z",
            "data": data
        }
    });
    if let Some(version) = client_version {
        envelope["device_client_version"] = json!(version);
    }
    if let Some(version) = firmware_version {
        envelope["firmware_version"] = json!(version);
    }
    serde_json::to_vec(&envelope).unwrap()
}

fn connectivity(timestamp_ms: i64, status: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "version": 1,
        "vin": VIN,
        "txid": format!("connection-{timestamp_ms}"),
        "tx_type": "connectivity",
        "received_at_ms": timestamp_ms + 100,
        "timestamp_ms": timestamp_ms,
        "payload": {
            "connectionId": "connection-1",
            "status": status,
            "createdAt": "2027-01-15T08:00:00Z"
        }
    }))
    .unwrap()
}

#[test]
fn rejects_unbounded_unknown_or_mismatched_envelopes() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    assert_eq!(
        accumulator.apply_json(&vec![b' '; MAX_FLEET_TELEMETRY_INPUT_BYTES + 1]),
        Err(FleetTelemetryError::InputTooLarge)
    );
    let mut envelope: Value = serde_json::from_slice(&transaction(T0, json!({}))).unwrap();
    envelope["extra"] = json!(true);
    assert_eq!(
        accumulator.apply_json(&serde_json::to_vec(&envelope).unwrap()),
        Err(FleetTelemetryError::InvalidJson)
    );
    envelope.as_object_mut().unwrap().remove("extra");
    envelope["vin"] = json!("5YJ3E1EA7KF000002");
    assert_eq!(
        accumulator.apply_json(&serde_json::to_vec(&envelope).unwrap()),
        Err(FleetTelemetryError::VinMismatch)
    );
}

#[test]
fn restores_full_state_and_maps_drive_charge_climate_vehicle_and_config() {
    let existing = json!({
        "drive_state": {
            "native_location_elevation": 42,
            "elevation": 43
        },
        "vehicle_state": {"locked": true, "timestamp": T0 - 1_000},
        "charge_state": {"battery_level": 70, "timestamp": T0 - 1_000}
    });
    let mut accumulator = FleetTelemetryAccumulator::restore(VIN, &existing).unwrap();
    let snapshot = accumulator
        .apply_json(&transaction(
            T0,
            json!({
                "Location": {"locationValue": {"latitude": 51.5, "longitude": -0.12}},
                "VehicleSpeed": {"doubleValue": 62.137119},
                "GpsHeading": {"doubleValue": 123.5},
                "Gear": {"shiftStateValue": "ShiftStateD"},
                "BatteryLevel": {"doubleValue": 81.0},
                "Soc": {"doubleValue": 79.0},
                "RatedRange": {"doubleValue": 198.838781},
                "EstBatteryRange": {"doubleValue": 190.0},
                "IdealBatteryRange": {"doubleValue": 210.0},
                "PackVoltage": {"doubleValue": 400.0},
                "PackCurrent": {"doubleValue": -50.0},
                "DetailedChargeState": {"detailedChargeStateValue": "DetailedChargeStateCharging"},
                "InsideTemp": {"doubleValue": 21.5},
                "HvacPower": {"hvacPowerValue": "HvacPowerStateOn"},
                "Odometer": {"doubleValue": 1000.0},
                "DoorState": {"doorValue": {"driverFront": true, "driverRear": false}},
                "FdWindow": {"windowStateValue": "WindowStateOpened"},
                "TpmsPressureFl": {"doubleValue": 2.9},
                "Version": {"stringValue": "2027.2.1 abc"},
                "CarType": {"carTypeValue": "CarTypeModel3"},
                "SoftwareUpdateDownloadPercentComplete": {"intValue": 20}
            }),
        ))
        .unwrap();
    let owner = snapshot.owner_data.as_object().unwrap();
    assert_eq!(owner["created_at"], T0);
    assert_eq!(owner["drive_state"]["shift_state"], "D");
    assert!(
        !owner["drive_state"]
            .as_object()
            .unwrap()
            .contains_key("native_location_elevation")
    );
    assert!(
        !owner["drive_state"]
            .as_object()
            .unwrap()
            .contains_key("elevation")
    );
    assert!((owner["drive_state"]["speed"].as_f64().unwrap() - 62.137_119).abs() < 0.001);
    assert_eq!(owner["drive_state"]["heading"], 123.5);
    assert_eq!(owner["drive_state"]["power"], 20.0);
    assert_eq!(owner["charge_state"]["battery_level"], 81);
    assert_eq!(owner["charge_state"]["usable_battery_level"], 79);
    assert!((owner["charge_state"]["battery_range"].as_f64().unwrap() - 198.838_781).abs() < 0.001);
    assert_eq!(owner["charge_state"]["est_battery_range"], 190.0);
    assert_eq!(owner["charge_state"]["ideal_battery_range"], 210.0);
    assert_eq!(owner["climate_state"]["is_climate_on"], true);
    assert!((owner["vehicle_state"]["odometer"].as_f64().unwrap() - 1000.0).abs() < 0.001);
    assert_eq!(owner["vehicle_state"]["df"], 1);
    assert_eq!(owner["vehicle_state"]["dr"], 0);
    assert_eq!(owner["vehicle_state"]["fd_window"], 1);
    assert_eq!(
        owner["vehicle_state"]["software_update"]["download_perc"],
        20
    );
    assert_eq!(owner["vehicle_config"]["car_type"], "model3");
}

#[test]
fn rounds_fractional_battery_percentages_without_rejecting_transaction() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    let snapshot = accumulator
        .apply_json(&transaction(
            T0,
            json!({
                "BatteryLevel": {"doubleValue": 80.6},
                "Soc": {"doubleValue": "79.4"}
            }),
        ))
        .unwrap();
    assert_eq!(snapshot.owner_data["charge_state"]["battery_level"], 81);
    assert_eq!(
        snapshot.owner_data["charge_state"]["usable_battery_level"],
        79
    );
}

#[test]
fn resolves_door_layout_from_version_field_and_owner_state() {
    let door_data = json!({
        "DoorState": {"doorValue": {
            "driverFront": true,
            "driverRear": true,
            "passengerFront": false,
            "passengerRear": true,
            "trunkFront": true,
            "trunkRear": false
        }}
    });
    let same_transaction_data = json!({
        "DoorState": door_data["DoorState"].clone(),
        "Version": {"stringValue": "2024.40.0"}
    });
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    let snapshot = accumulator
        .apply_json(&transaction_with_versions(
            T0,
            same_transaction_data,
            None,
            None,
        ))
        .unwrap();
    assert_eq!(snapshot.owner_data["vehicle_state"]["df"], 1);
    assert_eq!(snapshot.owner_data["vehicle_state"]["dr"], 0);
    assert_eq!(snapshot.owner_data["vehicle_state"]["pf"], 1);
    assert_eq!(snapshot.owner_data["vehicle_state"]["pr"], 1);
    assert_eq!(snapshot.owner_data["vehicle_state"]["ft"], 1);
    assert_eq!(snapshot.owner_data["vehicle_state"]["rt"], 0);

    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    accumulator
        .apply_json(&transaction(
            T0,
            json!({"Version": {"stringValue": "2024.40.0"}}),
        ))
        .unwrap();
    let snapshot = accumulator
        .apply_json(&transaction_with_versions(
            T0 + 1_000,
            door_data.clone(),
            None,
            None,
        ))
        .unwrap();
    assert_eq!(snapshot.owner_data["vehicle_state"]["dr"], 0);
    assert_eq!(snapshot.owner_data["vehicle_state"]["pf"], 1);

    let existing = json!({"vehicle_state": {"dr": 1, "pf": 1}});
    let mut accumulator = FleetTelemetryAccumulator::restore(VIN, &existing).unwrap();
    let snapshot = accumulator
        .apply_json(&transaction_with_versions(T0, door_data, None, None))
        .unwrap();
    let vehicle_state = snapshot.owner_data["vehicle_state"].as_object().unwrap();
    assert_eq!(vehicle_state["df"], 1);
    assert!(!vehicle_state.contains_key("dr"));
    assert!(!vehicle_state.contains_key("pf"));
    assert_eq!(vehicle_state["pr"], 1);
    assert_eq!(vehicle_state["ft"], 1);
    assert_eq!(vehicle_state["rt"], 0);
}

#[test]
fn unavailable_unknown_and_regressed_fields_do_not_fabricate_state() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    accumulator
        .apply_json(&transaction(
            T0,
            json!({"BatteryLevel": {"intValue": "80"}}),
        ))
        .unwrap();
    let report = accumulator
        .apply_json(&transaction(
            T0 + 1_000,
            json!({
                "BatteryLevel": {"invalidValue": "not_available"},
                "FutureTeslaField": {"stringValue": "value"}
            }),
        ))
        .unwrap();
    assert_eq!(report.owner_data["charge_state"]["battery_level"], 80);
    assert_eq!(report.unavailable_fields, vec!["BatteryLevel"]);
    assert_eq!(report.unknown_fields, vec!["FutureTeslaField"]);
    let report = accumulator
        .apply_json(&transaction(
            T0 + 500,
            json!({"BatteryLevel": {"intValue": "20"}}),
        ))
        .unwrap();
    assert_eq!(report.owner_data["charge_state"]["battery_level"], 80);
    assert_eq!(report.regressed_fields, vec!["BatteryLevel"]);
}

#[test]
fn hvac_power_maps_every_official_state_without_blocking_ack() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    for (offset, state, expected) in [
        (0, "HvacPowerStatePrecondition", true),
        (1_000, "HvacPowerStateOverheatProtect", true),
        (2_000, "HvacPowerStateOff", false),
    ] {
        let snapshot = accumulator
            .apply_json(&transaction(
                T0 + offset,
                json!({"HvacPower": {"hvacPowerValue": state}}),
            ))
            .unwrap();
        assert_eq!(
            snapshot.owner_data["climate_state"]["is_climate_on"],
            expected
        );
    }
    let unavailable = accumulator
        .apply_json(&transaction(
            T0 + 3_000,
            json!({"HvacPower": {"hvacPowerValue": "HvacPowerStateUnknown"}}),
        ))
        .unwrap();
    assert_eq!(unavailable.unavailable_fields, vec!["HvacPower"]);
}

#[test]
fn routes_vin_and_uses_vehicle_data_as_online_connectivity_evidence() {
    assert_eq!(vin_from_json(&transaction(T0, json!({}))).unwrap(), VIN);
    let mut accumulator = FleetTelemetryAccumulator::restore(
        VIN,
        &json!({"state": "offline", "created_at": T0 - 1_000}),
    )
    .unwrap();
    let data = accumulator
        .apply_json(&transaction(
            T0,
            json!({
                "Gear": {"shiftStateValue": "ShiftStateD"},
                "Soc": {"intValue": "80"}
            }),
        ))
        .unwrap();
    assert_eq!(data.source_vehicle_state.as_deref(), Some("online"));
    assert_eq!(data.owner_data["state"], "online");
    assert_eq!(data.owner_data["drive_state"]["shift_state"], "D");
    let connected = accumulator
        .apply_json(&connectivity(T0 + 1_000, "CONNECTED"))
        .unwrap();
    assert_eq!(connected.source_vehicle_state.as_deref(), Some("online"));
    assert_eq!(connected.owner_data["state"], "online");
}

#[test]
fn failed_transaction_is_atomic() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    let before = accumulator.owner_data();
    let invalid = transaction(
        T0,
        json!({
            "Gear": {"shiftStateValue": "ShiftStateD"},
            "Location": {"locationValue": {"latitude": 91, "longitude": 0}}
        }),
    );
    assert_eq!(
        accumulator.apply_json(&invalid),
        Err(FleetTelemetryError::InvalidCoordinates)
    );
    assert_eq!(accumulator.owner_data(), before);
}

#[test]
fn pack_power_requires_both_post_restore_values_and_preserves_owner_sign() {
    let existing = json!({
        "created_at": T0,
        "drive_state": {"power": 77.0, "timestamp": T0}
    });
    let mut accumulator = FleetTelemetryAccumulator::restore(VIN, &existing).unwrap();

    let voltage_only = accumulator
        .apply_json(&transaction(
            T0 + 100,
            json!({"PackVoltage": {"doubleValue": 400.0}}),
        ))
        .unwrap();
    assert!(
        voltage_only.owner_data["drive_state"]
            .get("power")
            .is_none()
    );

    let discharge = accumulator
        .apply_json(&transaction(
            T0 + 200,
            json!({"PackCurrent": {"doubleValue": -40.0}}),
        ))
        .unwrap();
    assert_eq!(discharge.owner_data["drive_state"]["power"], 16.0);

    accumulator
        .apply_json(&transaction(
            T0 + 300,
            json!({"PackVoltage": {"invalid": true}}),
        ))
        .unwrap();
    let stale_component_rejected = accumulator
        .apply_json(&transaction(
            T0 + 400,
            json!({"PackCurrent": {"doubleValue": 10.0}}),
        ))
        .unwrap();
    assert_eq!(
        stale_component_rejected.owner_data["drive_state"].get("power"),
        None
    );

    let charge = accumulator
        .apply_json(&transaction(
            T0 + 500,
            json!({"PackVoltage": {"doubleValue": 400.0}}),
        ))
        .unwrap();
    assert_eq!(charge.owner_data["drive_state"]["power"], -4.0);

    let unknown = accumulator
        .apply_json(&transaction(
            T0 + 600,
            json!({"PackCurrent": {"futureValue": "unsupported"}}),
        ))
        .unwrap();
    assert!(unknown.owner_data["drive_state"].get("power").is_none());
    assert_eq!(unknown.unknown_fields, vec!["PackCurrent"]);
    let regressed = accumulator
        .apply_json(&transaction(
            T0 + 550,
            json!({"PackCurrent": {"doubleValue": -20.0}}),
        ))
        .unwrap();
    assert!(regressed.owner_data["drive_state"].get("power").is_none());
    assert_eq!(regressed.regressed_fields, vec!["PackCurrent"]);
}

#[test]
fn rejects_bad_coordinates_nonfinite_strings_and_duplicate_keys() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    assert_eq!(
        accumulator.apply_json(&transaction(
            T0,
            json!({"Location": {"locationValue": {"latitude": 91, "longitude": 0}}})
        )),
        Err(FleetTelemetryError::InvalidCoordinates)
    );
    assert_eq!(
        accumulator.apply_json(&transaction(
            T0,
            json!({"VehicleSpeed": {"doubleValue": "NaN"}})
        )),
        Err(FleetTelemetryError::NonFiniteNumber)
    );
    let list = json!([
        {"key":"Soc", "value":{"intValue":"50"}},
        {"key":"soc", "value":{"intValue":"51"}}
    ]);
    assert_eq!(
        accumulator.apply_json(&transaction(T0, list)),
        Err(FleetTelemetryError::InvalidFieldName)
    );
}

#[test]
fn cheap_config_is_official_fields_shape_and_bounded() {
    let config = recommended_cheap_fields_config();
    let fields = config["fields"].as_object().unwrap();
    assert_eq!(fields["Location"]["interval_seconds"], 5);
    assert_eq!(fields["GpsHeading"]["interval_seconds"], 5);
    assert_eq!(fields["PackVoltage"]["interval_seconds"], 5);
    assert_eq!(fields["PackCurrent"]["interval_seconds"], 5);
    assert_eq!(fields["EstBatteryRange"]["interval_seconds"], 30);
    assert_eq!(fields["IdealBatteryRange"]["interval_seconds"], 30);
    assert_eq!(fields["Version"]["interval_seconds"], 3600);
    assert_eq!(fields.len(), 47);
    assert!(fields.len() <= MAX_FLEET_TELEMETRY_FIELDS);
}
