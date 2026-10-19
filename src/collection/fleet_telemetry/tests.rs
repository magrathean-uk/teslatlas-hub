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

/// The same bounded MAX+1 synthetic input, for the two restore entry points
/// (mutant M11b and M11c of the fix round 2 record).
#[test]
fn restore_entry_points_refuse_input_above_the_byte_bound() {
    let oversized = vec![b' '; MAX_FLEET_TELEMETRY_INPUT_BYTES + 1];
    assert_eq!(
        FleetTelemetryAccumulator::restore_complete(&oversized).err(),
        Some(FleetTelemetryError::InputTooLarge)
    );
    let padding = "a".repeat(MAX_FLEET_TELEMETRY_INPUT_BYTES);
    assert_eq!(
        FleetTelemetryAccumulator::restore(VIN, &json!({"padding": padding})).err(),
        Some(FleetTelemetryError::InputTooLarge)
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
    assert_eq!(
        parse_envelope(&transaction(T0, json!({}))).unwrap().vin,
        VIN
    );
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
    assert_eq!(fields.len(), 49);
    assert!(fields.len() <= MAX_FLEET_TELEMETRY_FIELDS);
}

fn envelope_with(tx_type: &str, timestamp_ms: i64, received_at_ms: i64, payload: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "version": 1,
        "vin": VIN,
        "txid": format!("tx-{tx_type}-{timestamp_ms}"),
        "tx_type": tx_type,
        "received_at_ms": received_at_ms,
        "timestamp_ms": timestamp_ms,
        "payload": payload
    }))
    .unwrap()
}

fn connectivity_with(timestamp_ms: i64, status: &str, connection_id: Option<&str>) -> Vec<u8> {
    let mut payload = json!({"status": status, "createdAt": "2027-01-15T08:00:00Z"});
    if let Some(id) = connection_id {
        payload["connectionId"] = json!(id);
    }
    envelope_with("connectivity", timestamp_ms, timestamp_ms + 100, payload)
}

fn soc(percent: i64) -> Value {
    json!({"Soc": {"intValue": percent.to_string()}})
}

#[test]
fn strict_vin_refuses_lowercase_and_excluded_letters() {
    for vin in [
        "5yj3e1ea7kf000001",
        "5YJ3E1EA7KF00000I",
        "5YJ3E1EA7KF00000O",
        "5YJ3E1EA7KF00000Q",
    ] {
        let mut envelope: Value = serde_json::from_slice(&transaction(T0, json!({}))).unwrap();
        envelope["vin"] = json!(vin);
        assert_eq!(
            parse_envelope(&serde_json::to_vec(&envelope).unwrap()).unwrap_err(),
            FleetTelemetryError::InvalidVin,
            "{vin}"
        );
    }
    assert!(FleetTelemetryAccumulator::empty("5yj3e1ea7kf000001").is_err());
    assert!(
        FleetTelemetryAccumulator::restore_complete_for_vin(b"{}", "5yj3e1ea7kf000001").is_err()
    );
    let mut envelope: Value = serde_json::from_slice(&transaction(T0, json!({}))).unwrap();
    envelope["payload"]["vin"] = json!("5yj3e1ea7kf000001");
    assert_eq!(
        parse_envelope(&serde_json::to_vec(&envelope).unwrap()).unwrap_err(),
        FleetTelemetryError::InvalidVin
    );
}

#[test]
fn envelope_key_set_is_exact() {
    let base: Value = serde_json::from_slice(&transaction(T0, json!({}))).unwrap();
    for (key, value) in [("deviceClientVersion", "1.0"), ("firmwareVersion", "1.0")] {
        // Without the snake-case spelling: with both present, a repeated field
        // would be refused for that reason alone and hide an accepted alias.
        let mut envelope = base.clone();
        let fields = envelope.as_object_mut().unwrap();
        fields.remove("device_client_version");
        fields.remove("firmware_version");
        envelope[key] = json!(value);
        assert_eq!(
            parse_envelope(&serde_json::to_vec(&envelope).unwrap()).unwrap_err(),
            FleetTelemetryError::InvalidJson,
            "{key}"
        );
    }
    for key in ["device_client_version", "firmware_version"] {
        let mut envelope = base.clone();
        envelope[key] = Value::Null;
        assert_eq!(
            parse_envelope(&serde_json::to_vec(&envelope).unwrap()).unwrap_err(),
            FleetTelemetryError::InvalidJson,
            "{key}"
        );
        let mut envelope = base.clone();
        envelope[key] = json!("2027.2.1");
        assert!(parse_envelope(&serde_json::to_vec(&envelope).unwrap()).is_ok());
    }
    let mut envelope = base.clone();
    envelope["payload"] = json!([1, 2, 3]);
    assert_eq!(
        parse_envelope(&serde_json::to_vec(&envelope).unwrap()).unwrap_err(),
        FleetTelemetryError::InvalidJson
    );
    let mut envelope = base;
    envelope["version"] = json!(2);
    assert_eq!(
        parse_envelope(&serde_json::to_vec(&envelope).unwrap()).unwrap_err(),
        FleetTelemetryError::UnsupportedVersion
    );
}

#[test]
fn event_clock_allows_five_minutes_and_refuses_one_more_millisecond() {
    let received = T0 + 1_000;
    let on_edge = envelope_with("alerts", received + 300_000, received, json!({"name": "x"}));
    assert_eq!(
        parse_envelope(&on_edge).unwrap().timestamp_ms,
        received + 300_000
    );
    let beyond = envelope_with("alerts", received + 300_001, received, json!({"name": "x"}));
    assert_eq!(
        parse_envelope(&beyond).unwrap_err(),
        FleetTelemetryError::InvalidTimestamp
    );
}

#[test]
fn tx_type_selects_the_payload_shape() {
    let alert = envelope_with(
        "Alerts",
        T0,
        T0 + 1,
        json!({"data": {"Soc": 1}, "name": "n"}),
    );
    let parsed = parse_envelope(&alert).unwrap();
    assert_eq!(parsed.kind, ReceiverKind::Event);
    assert_eq!(parsed.tx_type, "alerts");
    assert_eq!(parsed.event_payload.unwrap()["name"], "n");
    let malformed = envelope_with("V", T0, T0 + 1, json!({"vin": VIN}));
    assert_eq!(
        parse_envelope(&malformed).unwrap_err(),
        FleetTelemetryError::InvalidJson
    );
    let unsupported = envelope_with("telemetry", T0, T0 + 1, json!({"data": {}}));
    assert_eq!(
        parse_envelope(&unsupported).unwrap_err(),
        FleetTelemetryError::InvalidTransactionIdentity
    );
    let connectivity_as_vehicle = envelope_with("connectivity", T0, T0 + 1, json!({"data": {}}));
    assert_eq!(
        parse_envelope(&connectivity_as_vehicle).unwrap_err(),
        FleetTelemetryError::InvalidJson
    );
}

#[test]
fn events_never_touch_the_accumulator() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    accumulator.apply_json(&transaction(T0, soc(80))).unwrap();
    let before = accumulator.encode_complete().unwrap();
    let alert = envelope_with("alerts", T0 + 10, T0 + 20, json!({"name": "n"}));
    assert_eq!(
        accumulator.apply_json(&alert),
        Err(FleetTelemetryError::InvalidTransactionIdentity)
    );
    assert_eq!(accumulator.encode_complete().unwrap(), before);
}

#[test]
fn connectivity_without_connection_id_or_created_at_is_accepted() {
    let bare = envelope_with("connectivity", T0, T0 + 1, json!({"status": "CONNECTED"}));
    assert_eq!(
        parse_envelope(&bare).unwrap().kind,
        ReceiverKind::Connectivity
    );
    for payload in [
        json!({"status": "CONNECTED", "connectionId": ""}),
        json!({"status": "CONNECTED", "connectionId": null}),
        json!({"status": "CONNECTED", "createdAt": null}),
        json!({"status": "CONNECTED", "CreatedAt": null}),
        json!({"status": "CONNECTED", "createdAt": "yesterday"}),
        json!({"status": "WANDERING"}),
        json!({"connectionId": "c"}),
    ] {
        assert!(
            parse_envelope(&envelope_with("connectivity", T0, T0 + 1, payload.clone())).is_err(),
            "{payload}"
        );
    }
}

#[test]
fn state_version_two_round_trips_and_version_one_is_refused() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    accumulator
        .apply_json(&connectivity_with(T0, "CONNECTED", Some("c1")))
        .unwrap();
    accumulator
        .apply_json(&transaction(
            T0 + 10,
            json!({
                "ACChargingPower": {"doubleValue": 7.0},
                "DCChargingPower": {"doubleValue": 0.0},
                "PackVoltage": {"doubleValue": 400.0}
            }),
        ))
        .unwrap();
    accumulator
        .apply_json(&connectivity_with(T0 + 20, "DISCONNECTED", None))
        .unwrap();
    let bytes = accumulator.encode_complete().unwrap();
    let restored = FleetTelemetryAccumulator::restore_complete(&bytes).unwrap();
    assert_eq!(restored.encode_complete().unwrap(), bytes);
    assert_eq!(restored.connection_boundary_ms, Some(T0 + 20));
    assert_eq!(restored.connection_id, None);

    let mut state: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(state["version"], 2);
    state["ac_power"] = json!({"value": 3.5, "timestamp_ms": T0 + 5});
    state["dc_power"] = json!({"value": 1.5, "timestamp_ms": T0 + 6});
    let with_power =
        FleetTelemetryAccumulator::restore_complete(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(with_power.ac_power.unwrap().value, 3.5);
    assert_eq!(with_power.dc_power.unwrap().timestamp_ms, T0 + 6);
    for version in [1, 3] {
        let mut stale = state.clone();
        stale["version"] = json!(version);
        assert_eq!(
            FleetTelemetryAccumulator::restore_complete(&serde_json::to_vec(&stale).unwrap())
                .unwrap_err(),
            FleetTelemetryError::InvalidExistingState
        );
    }
    let mut early = state.clone();
    early["connection_boundary_ms"] = json!(EARLIEST_TIMESTAMP_MS - 1);
    assert!(
        FleetTelemetryAccumulator::restore_complete(&serde_json::to_vec(&early).unwrap()).is_err()
    );
    let mut early_power = state;
    early_power["ac_power"] = json!({"value": 1.0, "timestamp_ms": 5});
    assert!(
        FleetTelemetryAccumulator::restore_complete(&serde_json::to_vec(&early_power).unwrap())
            .is_err()
    );
}

#[test]
fn connection_end_clears_groups_and_keeps_watermarks() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    accumulator
        .apply_json(&connectivity_with(T0, "CONNECTED", Some("c1")))
        .unwrap();
    accumulator
        .apply_json(&transaction(
            T0 + 10,
            json!({
                "Soc": {"intValue": "80"},
                "Odometer": {"doubleValue": 100.0},
                "PackVoltage": {"doubleValue": 400.0},
                "PackCurrent": {"doubleValue": -10.0}
            }),
        ))
        .unwrap();
    // Same ID and an online record without one end nothing.
    for record in [
        connectivity_with(T0 + 11, "CONNECTED", Some("c1")),
        connectivity_with(T0 + 12, "CONNECTED", None),
    ] {
        let snapshot = accumulator.apply_json(&record).unwrap();
        assert!(snapshot.owner_data["charge_state"].is_object());
    }
    assert_eq!(accumulator.connection_boundary_ms, Some(T0));
    // A regressed offline record cannot clear newer state.
    let snapshot = accumulator
        .apply_json(&connectivity_with(T0 + 5, "DISCONNECTED", None))
        .unwrap();
    assert_eq!(snapshot.regressed_fields, vec!["Connectivity"]);
    assert!(snapshot.owner_data["charge_state"].is_object());
    // A new ID ends the connection.
    let snapshot = accumulator
        .apply_json(&connectivity_with(T0 + 20, "CONNECTED", Some("c2")))
        .unwrap();
    for group in [
        "drive_state",
        "charge_state",
        "climate_state",
        "vehicle_state",
        "vehicle_config",
    ] {
        assert!(snapshot.owner_data.get(group).is_none(), "{group}");
    }
    assert_eq!(snapshot.owner_data["state"], "online");
    assert!(accumulator.pack_voltage.is_none() && accumulator.pack_current.is_none());
    assert_eq!(accumulator.field_watermarks["soc"], T0 + 10);
    assert_eq!(accumulator.connection_id.as_deref(), Some("c2"));
    assert_eq!(accumulator.connection_boundary_ms, Some(T0 + 20));
    // Offline ends it too and forgets the ID when none is supplied.
    accumulator
        .apply_json(&connectivity_with(T0 + 30, "DISCONNECTED", None))
        .unwrap();
    assert_eq!(accumulator.connection_id, None);
    assert_eq!(accumulator.connection_boundary_ms, Some(T0 + 30));
    // Vehicle records neither set the ID nor move the boundary.
    accumulator
        .apply_json(&transaction(T0 + 40, soc(70)))
        .unwrap();
    assert_eq!(accumulator.connection_boundary_ms, Some(T0 + 30));
}

#[test]
fn delayed_frame_of_an_ended_connection_is_regressed() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    accumulator.apply_json(&transaction(T0, soc(80))).unwrap();
    accumulator
        .apply_json(&connectivity_with(T0 + 20, "DISCONNECTED", None))
        .unwrap();
    let restored_bytes = accumulator.encode_complete().unwrap();
    for restart in [false, true] {
        let mut accumulator = if restart {
            FleetTelemetryAccumulator::restore_complete(&restored_bytes).unwrap()
        } else {
            accumulator.clone()
        };
        let delayed = accumulator
            .apply_json(&transaction(T0 + 10, soc(75)))
            .unwrap();
        assert_eq!(delayed.regressed_fields, vec!["Soc"], "restart={restart}");
        assert!(delayed.owner_data.get("charge_state").is_none());
        let at_boundary = accumulator
            .apply_json(&transaction(T0 + 20, soc(74)))
            .unwrap();
        assert_eq!(at_boundary.regressed_fields, vec!["Soc"]);
        let fresh = accumulator
            .apply_json(&transaction(T0 + 30, soc(73)))
            .unwrap();
        assert_eq!(fresh.updated_fields, vec!["Soc"]);
        assert_eq!(fresh.owner_data["charge_state"]["usable_battery_level"], 73);
    }
}

#[test]
fn group_timestamp_is_the_newest_key_watermark() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    accumulator
        .apply_json(&transaction(T0 + 10, soc(80)))
        .unwrap();
    accumulator
        .apply_json(&transaction(
            T0 + 50,
            json!({"ChargeLimitSoc": {"intValue": "90"}}),
        ))
        .unwrap();
    // An older sample of a key with its own watermark must not move the
    // group back.
    let snapshot = accumulator
        .apply_json(&transaction(T0 + 20, soc(79)))
        .unwrap();
    assert_eq!(snapshot.updated_fields, vec!["Soc"]);
    assert_eq!(snapshot.owner_data["charge_state"]["timestamp"], T0 + 50);
    assert_eq!(snapshot.owner_data["charge_state"]["charge_limit_soc"], 90);
    // A group without keys is absent.
    accumulator
        .apply_json(&connectivity_with(T0 + 60, "DISCONNECTED", None))
        .unwrap();
    assert!(accumulator.owner_data().get("charge_state").is_none());
}

fn charge_energy(owner: &Value) -> Option<f64> {
    owner["charge_state"]["charge_energy_added"].as_f64()
}

#[test]
fn energy_counter_is_dc_charging_energy_in_only() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    let ignored = accumulator
        .apply_json(&transaction(
            T0,
            json!({"ChargeEnergyAdded": {"doubleValue": 5.0}}),
        ))
        .unwrap();
    assert_eq!(charge_energy(&ignored.owner_data), None);
    let mapped = accumulator
        .apply_json(&transaction(
            T0 + 1,
            json!({"DCChargingEnergyIn": {"doubleValue": 2.5}}),
        ))
        .unwrap();
    assert_eq!(charge_energy(&mapped.owner_data), Some(2.5));
    let both = accumulator
        .apply_json(&transaction(
            T0 + 2,
            json!({
                "ChargeEnergyAdded": {"doubleValue": 99.0},
                "DCChargingEnergyIn": {"doubleValue": 3.0}
            }),
        ))
        .unwrap();
    assert_eq!(charge_energy(&both.owner_data), Some(3.0));
    let alternating = accumulator
        .apply_json(&transaction(
            T0 + 3,
            json!({"ChargeEnergyAdded": {"doubleValue": 120.0}}),
        ))
        .unwrap();
    assert_eq!(charge_energy(&alternating.owner_data), Some(3.0));
}

fn charger_power(owner: &Value) -> Option<f64> {
    owner["charge_state"]["charger_power"].as_f64()
}

#[test]
fn charger_power_prefers_ac_then_dc_then_generic() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    let generic = accumulator
        .apply_json(&transaction(
            T0,
            json!({"ChargerPower": {"doubleValue": 4.0}}),
        ))
        .unwrap();
    assert_eq!(charger_power(&generic.owner_data), Some(4.0));
    let dc = accumulator
        .apply_json(&transaction(
            T0 + 1,
            json!({"DCChargingPower": {"doubleValue": 50.0}}),
        ))
        .unwrap();
    assert_eq!(charger_power(&dc.owner_data), Some(50.0));
    let suppressed = accumulator
        .apply_json(&transaction(
            T0 + 2,
            json!({"ChargerPower": {"doubleValue": 9.0}}),
        ))
        .unwrap();
    assert_eq!(charger_power(&suppressed.owner_data), Some(50.0));
    let ac = accumulator
        .apply_json(&transaction(
            T0 + 3,
            json!({"ACChargingPower": {"doubleValue": 7.0}}),
        ))
        .unwrap();
    assert_eq!(charger_power(&ac.owner_data), Some(7.0));
    // Delivered AC zero yields to delivered DC; with no DC it is retained.
    let zero_ac = accumulator
        .apply_json(&transaction(
            T0 + 4,
            json!({"ACChargingPower": {"doubleValue": 0.0}}),
        ))
        .unwrap();
    assert_eq!(charger_power(&zero_ac.owner_data), Some(50.0));
    let zero_dc = accumulator
        .apply_json(&transaction(
            T0 + 5,
            json!({"DCChargingPower": {"doubleValue": 0.0}}),
        ))
        .unwrap();
    assert_eq!(charger_power(&zero_dc.owner_data), Some(0.0));
    // A new connection forgets both specific signals, so generic speaks again.
    accumulator
        .apply_json(&connectivity_with(T0 + 10, "CONNECTED", Some("c9")))
        .unwrap();
    let fresh = accumulator
        .apply_json(&transaction(
            T0 + 11,
            json!({"ChargerPower": {"doubleValue": 2.0}}),
        ))
        .unwrap();
    assert_eq!(charger_power(&fresh.owner_data), Some(2.0));
}

#[test]
fn fast_charger_fields_keep_the_enum_text() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    let snapshot = accumulator
        .apply_json(&transaction(
            T0,
            json!({
                "FastChargerPresent": {"booleanValue": true},
                "FastChargerType": {"fastChargerValue": "FastChargerSupercharger"}
            }),
        ))
        .unwrap();
    assert_eq!(
        snapshot.owner_data["charge_state"]["fast_charger_present"],
        true
    );
    assert_eq!(
        snapshot.owner_data["charge_state"]["fast_charger_type"],
        "FastChargerSupercharger"
    );
    let config = recommended_cheap_fields_config();
    for name in [
        "DCChargingEnergyIn",
        "FastChargerPresent",
        "FastChargerType",
    ] {
        assert!(config["fields"][name].is_object(), "{name}");
    }
    assert_eq!(config["fields"]["FastChargerType"]["interval_seconds"], 15);
}

#[test]
fn duplicate_payload_keys_are_refused() {
    // The typed payload structs refused a repeated key; reading the payload as a
    // value first must not let the last of two equal keys win silently.
    let vehicle = format!(
        r#"{{"version":1,"vin":"{VIN}","txid":"dup-1","tx_type":"V","received_at_ms":{r},"timestamp_ms":{T0},
            "payload":{{"vin":"{VIN}","data":{{"Soc":{{"intValue":"80"}}}},"data":{{"Soc":{{"intValue":"10"}}}}}}}}"#,
        r = T0 + 1
    );
    assert_eq!(
        parse_envelope(vehicle.as_bytes()).unwrap_err(),
        FleetTelemetryError::InvalidJson
    );
    let connectivity = format!(
        r#"{{"version":1,"vin":"{VIN}","txid":"dup-2","tx_type":"connectivity","received_at_ms":{r},"timestamp_ms":{T0},
            "payload":{{"status":"CONNECTED","status":"DISCONNECTED"}}}}"#,
        r = T0 + 1
    );
    assert_eq!(
        parse_envelope(connectivity.as_bytes()).unwrap_err(),
        FleetTelemetryError::InvalidJson
    );
    // The same payloads with distinct keys are accepted.
    let once = format!(
        r#"{{"version":1,"vin":"{VIN}","txid":"dup-3","tx_type":"V","received_at_ms":{r},"timestamp_ms":{T0},
            "payload":{{"vin":"{VIN}","data":{{"Soc":{{"intValue":"80"}}}}}}}}"#,
        r = T0 + 1
    );
    assert_eq!(
        parse_envelope(once.as_bytes()).unwrap().kind,
        ReceiverKind::Vehicle
    );
}

// Ported from the retired delivery consumer's store tests (6A S6), where it ran
// through a stored lineage state: a complete state keeps both halves of a split
// pack sample and every watermark across encode and restore.
#[test]
fn receiver_complete_accumulator_state_preserves_split_pack_components_and_watermarks() {
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    let first = json!({
        "version":1,"vin":VIN,"txid":"voltage","tx_type":"V",
        "received_at_ms":1800000000100_i64,"timestamp_ms":1800000000000_i64,
        "payload":{"vin":VIN,"createdAt":"2027-01-15T08:00:00Z","data":{"PackVoltage":{"doubleValue":400.0}}}
    });
    accumulator
        .apply_json(&serde_json::to_vec(&first).unwrap())
        .unwrap();
    let encoded = accumulator.encode_complete().unwrap();
    let mut restored = FleetTelemetryAccumulator::restore_complete(&encoded).unwrap();
    let second = json!({
        "version":1,"vin":VIN,"txid":"current","tx_type":"V",
        "received_at_ms":1800000000200_i64,"timestamp_ms":1800000000100_i64,
        "payload":{"vin":VIN,"createdAt":"2027-01-15T08:00:00.100Z","data":{"PackCurrent":{"doubleValue":10.0}}}
    });
    let snapshot = restored
        .apply_json(&serde_json::to_vec(&second).unwrap())
        .unwrap();
    assert_eq!(snapshot.owner_data["drive_state"]["power"], -4.0);
    let mut regressed = first;
    regressed["txid"] = json!("old-voltage");
    let snapshot = restored
        .apply_json(&serde_json::to_vec(&regressed).unwrap())
        .unwrap();
    assert!(
        snapshot
            .regressed_fields
            .contains(&"PackVoltage".to_owned())
    );
    assert_eq!(snapshot.owner_data["drive_state"]["power"], -4.0);
}

// Ported from the retired consumer's wrong-VIN state test (6A S6): the state of
// one vehicle is usable for that VIN and for no other. The refusal is by VIN,
// not by a decode failure, so the state here is valid.
#[test]
fn receiver_complete_state_of_one_vehicle_is_refused_for_another() {
    const OTHER: &str = "5YJ3E1EA7KF000002";
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    accumulator.apply_json(&transaction(T0, soc(80))).unwrap();
    let bytes = accumulator.encode_complete().unwrap();
    FleetTelemetryAccumulator::restore_complete(&bytes).expect("the state itself is valid");
    assert!(FleetTelemetryAccumulator::restore_complete_for_vin(&bytes, VIN).is_ok());
    assert_eq!(
        FleetTelemetryAccumulator::restore_complete_for_vin(&bytes, OTHER).unwrap_err(),
        FleetTelemetryError::VinMismatch
    );
    assert_eq!(
        FleetTelemetryAccumulator::restore_complete_for_vin(&bytes, &VIN.to_lowercase())
            .unwrap_err(),
        FleetTelemetryError::InvalidVin
    );
    // A state whose recorded VIN was rewritten to another vehicle's is refused for
    // the first vehicle and accepted for the second only if every part agrees.
    let mut rewritten: Value = serde_json::from_slice(&bytes).unwrap();
    rewritten["vin"] = json!(OTHER);
    rewritten["owner_data"]["vin"] = json!(OTHER);
    let rewritten = serde_json::to_vec(&rewritten).unwrap();
    assert_eq!(
        FleetTelemetryAccumulator::restore_complete_for_vin(&rewritten, VIN).unwrap_err(),
        FleetTelemetryError::VinMismatch
    );
    assert!(FleetTelemetryAccumulator::restore_complete_for_vin(&rewritten, OTHER).is_ok());
}
