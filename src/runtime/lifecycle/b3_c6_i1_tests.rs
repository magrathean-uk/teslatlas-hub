// SPDX-License-Identifier: AGPL-3.0-only

//! C6-I1 lifecycle producer tests (C6 R11, R14, R20; spec tests C6 ST-I13, ST-I15
//! and ST-I21, the `parse_sample` halves). The lifecycle producer turns a live
//! vehicle-data payload into a `ProjectionCarPatch`: the model is the Hub code
//! of `vehicle_model_code(vin, car_type)`, every descriptor text is the raw
//! value trimmed and bounded by `live_config_text`, and no variant is derived.
//! The store-backed halves are in `src/storage/db/tests/b3_c6_i1.rs`. Synthetic
//! data only.

use serde_json::{Value, json};

use super::*;
use crate::hub_pack::ProjectionCarPatch;

const V3: &str = "5YJ3E1EA0KFTEST01";
const VS: &str = "5YJSA1E20HFTEST02";
const VL: &str = "LRW3E7EK0MCTEST07";

/// An owner vehicle-data observation with the given `vehicle_config` group and
/// optional root and data-object VINs, as the collector writes one.
fn payload(root_vin: Option<&str>, data_vin: Option<&str>, config: Value) -> LifecycleSample {
    let mut data = json!({
        "drive_state": {"shift_state": "P", "speed": 0},
        "vehicle_state": {"timestamp": 1_800_000_000_000_i64, "car_version": "2026.1"},
        "vehicle_config": config,
    });
    if let Some(vin) = data_vin {
        data["vin"] = json!(vin);
    }
    let mut payload = json!({
        "record_type": "owner_api_vehicle_data_v1",
        "source_vehicle_id": "9",
        "vehicle_data": data,
    });
    if let Some(vin) = root_vin {
        payload["vin"] = json!(vin);
    }
    LifecycleSample {
        observation_id: 1,
        observed_at_ms: 1_800_000_000_000,
        vehicle_state: "online".to_owned(),
        payload,
    }
}

/// The patch the lifecycle producer builds for one payload.
fn patch_of(sample: &LifecycleSample) -> ProjectionCarPatch {
    apply_sample(OpenSessionState::new(), 1, sample)
        .expect("apply the sample")
        .state
        .car_metadata
        .expect("the online sample carries a car patch")
}

fn some(value: &str) -> Option<String> {
    Some(value.to_owned())
}

#[test]
fn c6_st_i13_parse_sample_keeps_raw_descriptors() {
    // C6 ST-I13 (producer half): the Hub code for the model, the raw configuration
    // texts trimmed with their case unchanged, and no derived marketing name.
    let patch = patch_of(&payload(
        None,
        Some(V3),
        json!({
            "car_type": "model3", "trim_badging": " 74d ",
            "exterior_color": " MidnightSilver ", "wheel_type": "Pinwheel18",
            "spoiler_type": "None"
        }),
    ));
    assert_eq!(patch.model, some("3"));
    assert_eq!(patch.vin, some(V3));
    assert_eq!(patch.trim_badging, some("74d"));
    assert_eq!(patch.marketing_name, None);
    assert_eq!(patch.exterior_color, some("MidnightSilver"));
    assert_eq!(patch.wheel_type, some("Pinwheel18"));
    assert_eq!(patch.spoiler_type, some("None"));

    // No variant is derived whatever the badge: the badge is the raw text, and
    // a mixed-case or lower-case badge is not upper-cased.
    // The vectors are the specification's (C6 ST-I10, ST-I11, ST-I14, ST-I18).
    for badge in ["74d", "74", "74D", " p100d ", "LR"] {
        let patch = patch_of(&payload(
            None,
            Some(V3),
            json!({"car_type": "model3", "trim_badging": badge}),
        ));
        assert_eq!(
            (patch.trim_badging, patch.marketing_name),
            (some(badge.trim()), None),
            "badge {badge:?}"
        );
    }

    // C6 ST-I15 and ST-I21 (producer half): the patch carries only raw text, so
    // applying it twice gives the same stored car, with the raw trim kept.
    let first = patch.into_car(1, None, None, None);
    let second = patch.into_car(1, Some(&first), None, None);
    assert_eq!(first, second);
    assert_eq!(first.trim_badging, some("74d"));
    assert_eq!(first.marketing_name, None);
    assert_eq!(first.source_model_text, None);
}

#[test]
fn c6_st_i13_parse_sample_reads_the_collector_response_shape() {
    // C6 ST-I13 on the specification's parked response P0, in the shape the
    // collector writes: the data object is `provider_raw_json.response`, its
    // VIN is the filed VIN V3 and the source vehicle state is online.
    let p0 = json!({
        "display_name": "Test Car",
        "vin": V3,
        "drive_state": {
            "shift_state": null, "speed": null, "power": 0, "latitude": 10.1234567,
            "longitude": 20.7654321, "heading": 90, "elevation": 50.4
        },
        "charge_state": {
            "battery_level": 80, "usable_battery_level": 79, "charging_state": "Disconnected",
            "conn_charge_cable": "<invalid>", "charger_power": 0
        },
        "vehicle_state": {"odometer": 1000.0, "car_version": "2024.14.3 aa1bb2", "locked": true},
        "vehicle_config": {
            "car_type": "model3", "trim_badging": "74d", "exterior_color": "MidnightSilver",
            "wheel_type": "Pinwheel18", "spoiler_type": "None", "sun_roof_installed": 0
        }
    });
    let sample = LifecycleSample {
        observation_id: 1,
        observed_at_ms: 1_800_000_000_000,
        vehicle_state: "online".to_owned(),
        payload: json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": "online",
            "provider_raw_json": {"response": p0},
        }),
    };
    let patch = patch_of(&sample);
    assert_eq!(patch.model, some("3"));
    assert_eq!(patch.vin, some(V3));
    assert_eq!(patch.trim_badging, some("74d"));
    assert_eq!(patch.marketing_name, None);
    assert_eq!(patch.exterior_color, some("MidnightSilver"));
    assert_eq!(patch.wheel_type, some("Pinwheel18"));
    assert_eq!(patch.spoiler_type, some("None"));
}

#[test]
fn c6_st_i13_parse_sample_model_follows_the_descriptor_rules() {
    let model = |root: Option<&str>, data: Option<&str>, car_type: Value| {
        patch_of(&payload(root, data, json!({"car_type": car_type}))).model
    };
    // C6 R6: a filed VIN decides over any text, a recognised text decides without
    // a VIN, and an unrecognised text passes through trimmed and verbatim.
    assert_eq!(model(None, Some(VS), json!("model3")), some("S"));
    assert_eq!(model(None, Some(V3), json!("lychee")), some("3"));
    assert_eq!(model(None, None, json!("models2")), some("S"));
    assert_eq!(model(None, None, json!(" Model Y ")), some("Y"));
    assert_eq!(model(None, None, json!("cybertruck")), some("C"));
    assert_eq!(model(None, None, json!("lychee")), some("lychee"));
    assert_eq!(model(None, None, json!("  New Thing ")), some("New Thing"));
    assert_eq!(model(None, None, json!("CarTypeUnknown")), None);
    // A not-filed WMI: the recognised text wins, the VIN is the fallback.
    assert_eq!(model(None, Some(VL), json!("modely")), some("Y"));
    assert_eq!(model(None, Some(VL), json!("lychee")), some("3"));
    // A VIN alone gives the code when the payload has no car type at all.
    let no_type = patch_of(&payload(None, Some(V3), json!({"trim_badging": "74"})));
    assert_eq!(no_type.model, some("3"));
    // No VIN and no model text: no model in the patch.
    let nothing = patch_of(&payload(None, None, json!({"trim_badging": "74"})));
    assert_eq!(nothing.model, None);
    // The root VIN is the payload VIN; the data object's is the fallback.
    assert_eq!(model(Some(VS), Some(V3), json!("model3")), some("S"));
    assert_eq!(model(Some(VS), None, json!("model3")), some("S"));
    // A model text beyond the byte bound states nothing (C6 R5 step 1).
    let long = "A".repeat(257);
    assert_eq!(model(None, None, json!(long)), None);
    assert_eq!(model(None, Some(V3), json!(long)), some("3"));
}

#[test]
fn c6_st_i11_parse_sample_applies_the_byte_bound() {
    // C6 R11: a descriptor text longer than 256 bytes after trimming is no text;
    // 256 bytes is kept; the bound counts bytes, not characters.
    let at_bound = "A".repeat(256);
    let over = "A".repeat(257);
    let two_byte_at_bound = "é".repeat(128);
    let two_byte_over = "é".repeat(129);
    let padded = format!("  {at_bound}  ");
    for field in [
        "trim_badging",
        "exterior_color",
        "wheel_type",
        "spoiler_type",
    ] {
        let read = |value: Value| {
            let mut config = json!({"car_type": "model3"});
            config[field] = value;
            let patch = patch_of(&payload(None, Some(V3), config));
            match field {
                "trim_badging" => patch.trim_badging,
                "exterior_color" => patch.exterior_color,
                "wheel_type" => patch.wheel_type,
                _ => patch.spoiler_type,
            }
        };
        assert_eq!(read(json!(at_bound)), Some(at_bound.clone()), "{field}");
        assert_eq!(
            read(json!(padded)),
            Some(at_bound.clone()),
            "{field}: trimmed first"
        );
        assert_eq!(read(json!(over)), None, "{field}");
        assert_eq!(
            read(json!(two_byte_at_bound)),
            Some(two_byte_at_bound.clone()),
            "{field}"
        );
        assert_eq!(read(json!(two_byte_over)), None, "{field}");
        assert_eq!(read(json!("   ")), None, "{field}");
        assert_eq!(read(json!(7)), None, "{field}: not a string");
        assert_eq!(read(Value::Null), None, "{field}");
    }
    // The bounded patch stays inside the blob's budget with every field at the
    // bound (C1 R12): it encodes and decodes unchanged.
    let patch = patch_of(&payload(
        None,
        Some(V3),
        json!({
            "car_type": "model3", "trim_badging": at_bound, "exterior_color": at_bound,
            "wheel_type": at_bound, "spoiler_type": at_bound
        }),
    ));
    let state = apply_sample(
        OpenSessionState::new(),
        1,
        &payload(
            None,
            Some(V3),
            json!({
                "car_type": "model3", "trim_badging": at_bound, "exterior_color": at_bound,
                "wheel_type": at_bound, "spoiler_type": at_bound
            }),
        ),
    )
    .expect("apply the sample")
    .state;
    let decoded = OpenSessionState::decode(&state.encode().expect("encode")).expect("decode");
    assert_eq!(decoded.car_metadata, Some(patch));
}
