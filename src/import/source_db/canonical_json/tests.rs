// SPDX-License-Identifier: AGPL-3.0-only

use std::collections::BTreeMap;

use serde::Serialize;

use super::{encode_sorted_object, encode_sorted_object_string};
use crate::hub_pack::{ProjectionFixedNumericV2_2, ProjectionFloat64BitsV2_2};
use crate::import::source_db::projection::SourceDbPositionPhysicalV2_2;
use crate::import::source_db::projection_state::{
    SourceDbProjectionStateEntity, canonical_payload_and_digest,
};

fn two_pass<T: Serialize>(value: &T) -> Vec<u8> {
    let value = serde_json::to_value(value).expect("to_value");
    serde_json::to_vec(&value).expect("to_vec")
}

fn assert_identical<T: Serialize>(value: &T) {
    let single = encode_sorted_object(value).expect("plain struct uses the single pass");
    assert_eq!(single, two_pass(value), "single-pass bytes differ");
    let string = encode_sorted_object_string(value).expect("string form");
    assert_eq!(string.as_bytes(), single.as_slice());
}

pub(crate) fn position(filled: bool) -> SourceDbPositionPhysicalV2_2 {
    let numeric = |value: i64| {
        if filled {
            Some(ProjectionFixedNumericV2_2::Finite(value))
        } else {
            None
        }
    };
    SourceDbPositionPhysicalV2_2 {
        id: i32::MAX,
        car_id: i16::MIN,
        drive_id: filled.then_some(i32::MIN),
        date_pg_us: i64::MIN,
        latitude_e6: ProjectionFixedNumericV2_2::NaN,
        longitude_e6: ProjectionFixedNumericV2_2::Finite(-999_999_999),
        elevation: filled.then_some(i16::MIN),
        speed: filled.then_some(i16::MAX),
        power: filled.then_some(-1),
        odometer: filled.then_some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042)),
        ideal_battery_range_km_e2: numeric(999_999),
        est_battery_range_km_e2: numeric(-999_999),
        rated_battery_range_km_e2: filled.then_some(ProjectionFixedNumericV2_2::NaN),
        battery_level: filled.then_some(0),
        usable_battery_level: filled.then_some(100),
        battery_heater: filled.then_some(false),
        battery_heater_on: filled.then_some(true),
        battery_heater_no_power: None,
        outside_temp_e1: numeric(-9_999),
        inside_temp_e1: numeric(9_999),
        fan_status: filled.then_some(i32::MIN),
        driver_temp_setting_e1: numeric(215),
        passenger_temp_setting_e1: numeric(0),
        is_climate_on: filled.then_some(true),
        is_rear_defroster_on: filled.then_some(false),
        is_front_defroster_on: None,
        tpms_pressure_fl_e1: numeric(29),
        tpms_pressure_fr_e1: numeric(30),
        tpms_pressure_rl_e1: numeric(31),
        tpms_pressure_rr_e1: numeric(32),
    }
}

#[test]
fn source_db_canonical_json_physical_position_bytes_match_two_pass() {
    assert_identical(&position(true));
    assert_identical(&position(false));
}

#[derive(Serialize)]
struct Inner {
    zeta: u8,
    alpha: Option<&'static str>,
}

#[derive(Serialize)]
struct Mixed {
    zulu: f64,
    #[serde(rename = "quote\"key")]
    quoted: &'static str,
    alpha: Inner,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<u8>,
    list: Vec<Inner>,
    map: BTreeMap<String, i64>,
    nan: f64,
    big: u64,
    text: String,
    #[serde(rename = "Upper")]
    upper: bool,
    _underscore: (),
}

#[derive(Serialize)]
struct Wrapper(Mixed);

fn mixed() -> Mixed {
    Mixed {
        zulu: 0.1 + 0.2,
        quoted: "line\nbreak \u{1}",
        alpha: Inner {
            zeta: 1,
            alpha: Some("é"),
        },
        skipped: None,
        list: vec![Inner {
            zeta: 2,
            alpha: None,
        }],
        map: BTreeMap::from([("b".to_owned(), 2), ("a".to_owned(), 1)]),
        nan: f64::NAN,
        big: u64::MAX,
        text: "plain".to_owned(),
        upper: true,
        _underscore: (),
    }
}

#[test]
fn source_db_canonical_json_nested_escaped_and_skipped_fields_match_two_pass() {
    assert_identical(&mixed());
    assert_identical(&Wrapper(mixed()));
    assert_identical(&Inner {
        zeta: 0,
        alpha: None,
    });
}

#[derive(Serialize)]
struct Empty {}

#[derive(Serialize)]
struct Flattened {
    zed: u8,
    #[serde(flatten)]
    inner: Inner,
}

#[derive(Serialize)]
struct Duplicate {
    #[serde(rename = "same")]
    first: u8,
    #[serde(rename = "same")]
    second: u8,
}

#[derive(Serialize)]
struct Unit;

#[derive(Serialize)]
enum Variant {
    Struct { b: u8, a: u8 },
}

#[test]
fn source_db_canonical_json_other_shapes_fall_back_to_two_pass() {
    assert_identical(&Empty {});
    assert!(
        encode_sorted_object(&Flattened {
            zed: 1,
            inner: Inner {
                zeta: 2,
                alpha: None
            }
        })
        .is_none()
    );
    assert!(encode_sorted_object(&Duplicate { first: 1, second: 2 }).is_none());
    assert!(encode_sorted_object(&BTreeMap::from([("a", 1)])).is_none());
    assert!(encode_sorted_object(&Unit).is_none());
    assert!(encode_sorted_object(&1_i64).is_none());
    assert!(encode_sorted_object(&"text").is_none());
    assert!(encode_sorted_object(&Some(Inner { zeta: 1, alpha: None })).is_none());
    assert!(encode_sorted_object(&Variant::Struct { b: 1, a: 2 }).is_none());
    let raw = serde_json::value::RawValue::from_string("{\"b\":1,\"a\":2}".to_owned())
        .expect("raw value");
    assert!(encode_sorted_object(&raw).is_none());
}

#[test]
fn source_db_canonical_json_projection_state_payload_and_digest_match_two_pass() {
    for (id, row) in [(7, position(true)), (8, position(false))] {
        let single =
            canonical_payload_and_digest(SourceDbProjectionStateEntity::Position, id, 1, &row)
                .expect("single pass");
        // A `serde_json::Value` object serialises as a map, so it always takes
        // the two-pass reference path inside the same function.
        let reference = canonical_payload_and_digest(
            SourceDbProjectionStateEntity::Position,
            id,
            1,
            &serde_json::to_value(&row).expect("value"),
        )
        .expect("two pass");
        assert_eq!(single, reference);
        assert_eq!(single.0, two_pass(&row));
    }
}
