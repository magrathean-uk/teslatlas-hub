// SPDX-License-Identifier: AGPL-3.0-only

//! C8 Part S-a tests (C8 section 8: C8 T04 to C8 T09, C8 T11, C8 T12, C8 T13 and
//! C8 T14): the save validation rules and their safe error texts, the
//! geofence schema step with its rewind, and import activation, through
//! `HubStore` where a store is involved. A save here is validated against the
//! record alone, so the cases that need the stored row an edit targets (C8
//! T09, C8 R7, C8 R8) run on the pure function with the stored record passed
//! in. The id allocation, the same-place rule and the import reconciliation
//! beyond activation belong to the save flow tests of `b3_c8_sb` (C8 T1 to C8
//! T3, C8 T10, C8 T41 to C8 T48); the report lists and `source_owned` of the
//! activation cases are asserted here (C8 T13, C8 T14). Synthetic data only.

use super::*;
use crate::geo::geofence_validation::{
    GeofenceInactiveReason, GeofenceRejection, validate_geofence_save,
};
use crate::geo::location::{
    DEFAULT_GEOFENCE_RADIUS_METRES, FIRST_HUB_GEOFENCE_ID, MAX_GEOFENCE_COST_PER_UNIT,
    MAX_GEOFENCE_NAME_BYTES, MAX_GEOFENCE_NAME_CHARS, MAX_GEOFENCE_RADIUS_METRES,
    MAX_GEOFENCE_SESSION_FEE, MAX_IMPORTED_GEOFENCE_ID, MAX_IMPORTED_GEOFENCE_RADIUS_METRES,
    MIN_SAVED_GEOFENCE_RADIUS_METRES,
};
use crate::hub_pack::GeofenceBillingType;

pub(super) type Fence = crate::teslamate_projection::TeslaMateGeofence;

/// A fence record with `id` 0 and every optional field omitted.
pub(super) fn fence(name: &str, latitude: f64, longitude: f64) -> Fence {
    Fence {
        id: 0,
        name: name.to_owned(),
        latitude: Some(latitude),
        longitude: Some(longitude),
        radius_m: None,
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
    }
}

/// An imported source row: `id` as read, a radius, no tariff.
pub(super) fn imported(id: i64, name: &str, latitude: f64, longitude: f64, radius_m: f64) -> Fence {
    Fence {
        id,
        radius_m: Some(radius_m),
        ..fence(name, latitude, longitude)
    }
}

pub(super) fn c8_store() -> (tempfile::TempDir, HubStore) {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    (temporary, store)
}

/// One registered vehicle; `key` makes a second vehicle of the same store.
pub(super) fn c8_vehicle(store: &HubStore, key: &str) -> Uuid {
    let source = store
        .register_source(
            &SourceDescriptor::new("tesla_owner_api", "account-test"),
            1_000,
        )
        .expect("source");
    store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, key), 1_001)
        .expect("vehicle")
        .vehicle_id
}

/// The rejection of a failed save, panicking on any other outcome.
pub(super) fn rejection(result: Result<Fence, StoreError>) -> GeofenceRejection {
    match result {
        Err(StoreError::GeofenceRejected(rejection)) => rejection,
        other => panic!("expected a geofence rejection, got {other:?}"),
    }
}

/// Create `record` on a fresh vehicle of the store and return the stored fence
/// (read back from the table, not the returned record).
fn created_on_fresh_vehicle(store: &HubStore, key: &str, record: Fence) -> Fence {
    let vehicle = c8_vehicle(store, key);
    let saved = store
        .save_geofence(vehicle, None, record, false)
        .expect("accepted save");
    let stored = store.geofences(vehicle).expect("stored fences");
    assert_eq!(stored.len(), 1, "one row stored");
    assert_eq!(stored[0], saved, "the returned record is the stored row");
    stored.into_iter().next().expect("one stored fence")
}

/// The refused save on a fresh vehicle: the rejection, with nothing stored.
fn refused_on_fresh_vehicle(store: &HubStore, key: &str, record: Fence) -> GeofenceRejection {
    let vehicle = c8_vehicle(store, key);
    let refused = rejection(store.save_geofence(vehicle, None, record, false));
    assert!(
        store.geofences(vehicle).expect("fences").is_empty(),
        "a refused save writes nothing"
    );
    refused
}

#[test]
fn c8_t04_to_t09_validation() {
    let (_temporary, store) = c8_store();
    let mut key = 0_u32;
    let mut next_key = || {
        key += 1;
        format!("t04-t09-{key}")
    };

    // C8 T04, C8 R6: the radius, in the order finite, whole, minimum, maximum.
    // Creates at distinct centres.
    for (index, (radius, expected)) in [
        (9.0, Err(GeofenceRejection::RadiusBelowMinimum)),
        (0.0, Err(GeofenceRejection::RadiusBelowMinimum)),
        (-5.0, Err(GeofenceRejection::RadiusBelowMinimum)),
        (10.0, Ok(10.0)),
        (30_000.0, Ok(30_000.0)),
        (30_001.0, Err(GeofenceRejection::RadiusAboveMaximum)),
        (32_767.0, Err(GeofenceRejection::RadiusAboveMaximum)),
        // Above the maximum and fractional: whole metres is checked first.
        (30_000.5, Err(GeofenceRejection::RadiusNotWholeMetres)),
        (25.5, Err(GeofenceRejection::RadiusNotWholeMetres)),
        (0.5, Err(GeofenceRejection::RadiusNotWholeMetres)),
        (f64::NAN, Err(GeofenceRejection::RadiusNotFinite)),
        (f64::INFINITY, Err(GeofenceRejection::RadiusNotFinite)),
        (f64::NEG_INFINITY, Err(GeofenceRejection::RadiusNotFinite)),
    ]
    .into_iter()
    .enumerate()
    {
        let record = Fence {
            radius_m: Some(radius),
            ..fence("R", 10.0 + index as f64 * 0.01, 20.0)
        };
        match expected {
            Ok(stored) => {
                let fence = created_on_fresh_vehicle(&store, &next_key(), record);
                assert_eq!(fence.radius_m, Some(stored), "radius {radius}");
            }
            Err(rejected) => {
                assert_eq!(
                    refused_on_fresh_vehicle(&store, &next_key(), record),
                    rejected,
                    "radius {radius}"
                );
            }
        }
    }
    // A radius not given is the default on a create.
    let defaulted = created_on_fresh_vehicle(&store, &next_key(), fence("Home", 10.0, 20.0));
    assert_eq!(defaulted.radius_m, Some(DEFAULT_GEOFENCE_RADIUS_METRES));
    assert_eq!(DEFAULT_GEOFENCE_RADIUS_METRES, 50.0);

    // C8 T05, C8 R5: the centre, checked on the value as given.
    for (latitude, longitude) in [(90.0, 0.0), (-90.0, 0.0)] {
        let stored = created_on_fresh_vehicle(&store, &next_key(), fence("P", latitude, longitude));
        assert_eq!(stored.latitude, Some(latitude));
    }
    for (record, expected) in [
        (
            fence("C", 90.000001, 0.0),
            GeofenceRejection::LatitudeInvalid,
        ),
        (
            fence("C", -90.000001, 0.0),
            GeofenceRejection::LatitudeInvalid,
        ),
        // Out of range as given, although six-decimal rounding would bring
        // them to the bound.
        (
            fence("C", 90.0000004, 0.0),
            GeofenceRejection::LatitudeInvalid,
        ),
        (
            fence("C", -90.0000004, 0.0),
            GeofenceRejection::LatitudeInvalid,
        ),
        (
            fence("C", f64::NAN, 0.0),
            GeofenceRejection::LatitudeInvalid,
        ),
        (
            fence("C", f64::INFINITY, 0.0),
            GeofenceRejection::LatitudeInvalid,
        ),
        (
            Fence {
                latitude: None,
                ..fence("C", 0.0, 0.0)
            },
            GeofenceRejection::LatitudeInvalid,
        ),
        (
            fence("C", 0.0, 180.000001),
            GeofenceRejection::LongitudeInvalid,
        ),
        (
            fence("C", 0.0, -180.000001),
            GeofenceRejection::LongitudeInvalid,
        ),
        (
            fence("C", 0.0, 180.0000004),
            GeofenceRejection::LongitudeInvalid,
        ),
        (
            fence("C", 0.0, -180.0000004),
            GeofenceRejection::LongitudeInvalid,
        ),
        (
            fence("C", 0.0, f64::NAN),
            GeofenceRejection::LongitudeInvalid,
        ),
        (
            Fence {
                longitude: None,
                ..fence("C", 0.0, 0.0)
            },
            GeofenceRejection::LongitudeInvalid,
        ),
    ] {
        assert_eq!(
            refused_on_fresh_vehicle(&store, &next_key(), record),
            expected
        );
    }
    // Longitude 180 and -180 are both stored as given, on separate vehicles.
    for longitude in [180.0, -180.0] {
        let stored = created_on_fresh_vehicle(&store, &next_key(), fence("D", 0.0, longitude));
        assert_eq!(stored.longitude, Some(longitude));
    }

    // C8 T06, C8 R5: rounding half away from zero to six decimals, zero
    // normalised.
    let rounded = created_on_fresh_vehicle(
        &store,
        &next_key(),
        fence("Round", 51.50735123, -0.12775849),
    );
    assert_eq!(
        (rounded.latitude, rounded.longitude),
        (Some(51.507351), Some(-0.127758))
    );
    let zero = created_on_fresh_vehicle(&store, &next_key(), fence("Zero", -0.0000001, 20.0));
    let latitude = zero.latitude.expect("latitude");
    assert_eq!(latitude, 0.0);
    assert!(latitude.is_sign_positive(), "-0.0 never reaches storage");
    let tied = created_on_fresh_vehicle(&store, &next_key(), fence("Tie", 10.00000049, 19.9999995));
    assert_eq!((tied.latitude, tied.longitude), (Some(10.0), Some(20.0)));
    // Rounding is idempotent on its own output: a stored fence passed back as
    // the edit of itself resolves to the stored values.
    for stored in [&rounded, &zero, &tied] {
        let again = validate_geofence_save(stored, Some(stored), false).expect("stored values");
        assert_eq!(
            (
                again.name.as_str(),
                Some(again.latitude),
                Some(again.longitude)
            ),
            (stored.name.as_str(), stored.latitude, stored.longitude)
        );
    }

    // C8 T07, C8 R4: the name.
    let trimmed = created_on_fresh_vehicle(&store, &next_key(), fence("  Home  ", 10.0, 20.0));
    assert_eq!(trimmed.name, "Home");
    let separators = created_on_fresh_vehicle(
        &store,
        &next_key(),
        fence("\u{2028}Home\u{2029}", 10.0, 20.0),
    );
    assert_eq!(separators.name, "Home", "edge White_Space is trimmed");
    for empty in ["", "   ", "\u{3000}"] {
        assert_eq!(
            refused_on_fresh_vehicle(&store, &next_key(), fence(empty, 10.0, 20.0)),
            GeofenceRejection::NameEmpty,
            "{empty:?}"
        );
    }
    let two_bytes = "\u{e9}".repeat(MAX_GEOFENCE_NAME_CHARS);
    let stored = created_on_fresh_vehicle(&store, &next_key(), fence(&two_bytes, 10.0, 20.0));
    assert_eq!(stored.name.len(), 510);
    let three_bytes = "\u{754c}".repeat(MAX_GEOFENCE_NAME_CHARS);
    let stored = created_on_fresh_vehicle(&store, &next_key(), fence(&three_bytes, 10.0, 20.0));
    assert_eq!(stored.name.len(), 765, "wider than the previous byte limit");
    assert!(stored.name.len() <= MAX_GEOFENCE_NAME_BYTES);
    assert_eq!(
        refused_on_fresh_vehicle(
            &store,
            &next_key(),
            fence(&"a".repeat(MAX_GEOFENCE_NAME_CHARS + 1), 10.0, 20.0)
        ),
        GeofenceRejection::NameTooLong
    );
    // Too long and holding a refused character: the length is checked first.
    let long_with_control = format!(
        "{}\n{}",
        "a".repeat(MAX_GEOFENCE_NAME_CHARS / 2 + 1),
        "a".repeat(MAX_GEOFENCE_NAME_CHARS / 2)
    );
    assert_eq!(
        long_with_control.chars().count(),
        MAX_GEOFENCE_NAME_CHARS + 1
    );
    assert_eq!(
        refused_on_fresh_vehicle(&store, &next_key(), fence(&long_with_control, 10.0, 20.0)),
        GeofenceRejection::NameTooLong
    );
    for refused in [
        "Ho\nme",
        "Ho\u{0}me",
        "Ho\u{7f}me",
        "Ho\u{85}me",
        "Ho\u{2028}me",
        "Ho\u{2029}me",
        "Ho\u{202E}me",
        "Ho\u{2066}me",
        "Ho\u{200B}me",
        "Ho\u{FEFF}me",
        "Ho\tme",
        "Ho\u{202A}me",
        "Ho\u{2069}me",
    ] {
        assert_eq!(
            refused_on_fresh_vehicle(&store, &next_key(), fence(refused, 10.0, 20.0)),
            GeofenceRejection::NameControlCharacter,
            "{refused:?}"
        );
    }
    for allowed in ["Ho\u{200C}me", "Ho\u{200D}me", "Ho\u{200F}me"] {
        let stored = created_on_fresh_vehicle(&store, &next_key(), fence(allowed, 10.0, 20.0));
        assert_eq!(stored.name, allowed, "stored as given");
    }

    // C8 T08, C8 R8: the tariff on creates.
    let priced =
        |cost: Option<f64>, fee: Option<f64>, billing: Option<GeofenceBillingType>| Fence {
            cost_per_unit: cost,
            session_fee: fee,
            billing_type: billing,
            ..fence("T", 10.0, 20.0)
        };
    let stored = created_on_fresh_vehicle(&store, &next_key(), priced(Some(0.30), None, None));
    assert_eq!(
        (stored.billing_type, stored.cost_per_unit),
        (Some(GeofenceBillingType::PerKwh), Some(0.3))
    );
    let stored = created_on_fresh_vehicle(&store, &next_key(), priced(Some(0.123456), None, None));
    assert_eq!(stored.cost_per_unit, Some(0.1235));
    let stored = created_on_fresh_vehicle(&store, &next_key(), priced(Some(-0.00004), None, None));
    assert_eq!(stored.cost_per_unit, Some(0.0));
    assert!(stored.cost_per_unit.expect("cost").is_sign_positive());
    let stored =
        created_on_fresh_vehicle(&store, &next_key(), priced(Some(99999.99994), None, None));
    assert_eq!(stored.cost_per_unit, Some(99999.9999));
    for cost in [-0.05, 100_000.0, f64::NAN, f64::INFINITY] {
        assert_eq!(
            refused_on_fresh_vehicle(&store, &next_key(), priced(Some(cost), None, None)),
            GeofenceRejection::CostPerUnitInvalid,
            "{cost}"
        );
    }
    assert_eq!(
        refused_on_fresh_vehicle(
            &store,
            &next_key(),
            priced(None, None, Some(GeofenceBillingType::PerMinute))
        ),
        GeofenceRejection::BillingTypeWithoutCost
    );
    let stored = created_on_fresh_vehicle(&store, &next_key(), priced(None, Some(2.346), None));
    assert_eq!(
        (stored.billing_type, stored.session_fee),
        (None, Some(2.35))
    );
    let stored = created_on_fresh_vehicle(&store, &next_key(), priced(None, Some(-0.001), None));
    assert_eq!(stored.session_fee, Some(0.0));
    assert!(stored.session_fee.expect("fee").is_sign_positive());
    for fee in [-1.0, 1_000_000_000_000.0, f64::NAN] {
        assert_eq!(
            refused_on_fresh_vehicle(&store, &next_key(), priced(None, Some(fee), None)),
            GeofenceRejection::SessionFeeInvalid,
            "{fee}"
        );
    }
    let stored = created_on_fresh_vehicle(&store, &next_key(), priced(None, Some(5.0), None));
    assert_eq!((stored.billing_type, stored.session_fee), (None, Some(5.0)));
    let stored = created_on_fresh_vehicle(
        &store,
        &next_key(),
        priced(Some(0.5), Some(1.0), Some(GeofenceBillingType::PerMinute)),
    );
    assert_eq!(
        (
            stored.billing_type,
            stored.cost_per_unit,
            stored.session_fee
        ),
        (Some(GeofenceBillingType::PerMinute), Some(0.5), Some(1.0))
    );

    // C8 R1: the fields are checked in the order name, centre, radius, tariff,
    // and the first failure is the one returned.
    let mut everything_wrong = Fence {
        radius_m: Some(f64::NAN),
        billing_type: Some(GeofenceBillingType::PerMinute),
        cost_per_unit: Some(-1.0),
        session_fee: Some(-1.0),
        ..fence("", 95.0, 200.0)
    };
    let steps: [(fn(&mut Fence), GeofenceRejection); 6] = [
        (
            |f| f.name = "Order".to_owned(),
            GeofenceRejection::NameEmpty,
        ),
        (
            |f| f.latitude = Some(10.0),
            GeofenceRejection::LatitudeInvalid,
        ),
        (
            |f| f.longitude = Some(20.0),
            GeofenceRejection::LongitudeInvalid,
        ),
        (
            |f| f.radius_m = Some(50.0),
            GeofenceRejection::RadiusNotFinite,
        ),
        (
            |f| f.cost_per_unit = Some(0.5),
            GeofenceRejection::CostPerUnitInvalid,
        ),
        (
            |f| f.session_fee = Some(1.0),
            GeofenceRejection::SessionFeeInvalid,
        ),
    ];
    // Each step fixes the field that failed, so the next failure shows.
    for (fix, expected) in steps {
        assert_eq!(
            validate_geofence_save(&everything_wrong, None, false).expect_err("still invalid"),
            expected
        );
        fix(&mut everything_wrong);
    }
    assert!(validate_geofence_save(&everything_wrong, None, false).is_ok());
    // A billing type without any cost is the last tariff failure, after the
    // fee.
    let no_cost = Fence {
        cost_per_unit: None,
        ..everything_wrong.clone()
    };
    assert_eq!(
        validate_geofence_save(&no_cost, None, false).expect_err("no cost"),
        GeofenceRejection::BillingTypeWithoutCost
    );
    let fee_first = Fence {
        session_fee: Some(-1.0),
        ..no_cost
    };
    assert_eq!(
        validate_geofence_save(&fee_first, None, false).expect_err("fee"),
        GeofenceRejection::SessionFeeInvalid
    );

    // C8 T09, C8 R7, C8 R8, C8 table 5.2: an edit keeps the stored radius and
    // tariff when the save omits them; `clear_tariff` empties the base first.
    let gate = Fence {
        radius_m: Some(5.0),
        billing_type: Some(GeofenceBillingType::PerMinute),
        cost_per_unit: Some(0.5),
        session_fee: Some(1.0),
        ..imported(7, "Gate", 10.0, 20.0, 5.0)
    };
    let edit = |input: &Fence, stored: &Fence, clear: bool| {
        validate_geofence_save(input, Some(stored), clear)
    };
    let with_cost = Fence {
        cost_per_unit: Some(0.6),
        ..fence("Gate A", 10.0, 20.0)
    };
    let first = edit(&with_cost, &gate, false).expect("edit with a cost");
    assert_eq!(
        (
            first.radius_m,
            first.billing_type,
            first.cost_per_unit,
            first.session_fee
        ),
        (
            5.0,
            Some(GeofenceBillingType::PerMinute),
            Some(0.6),
            Some(1.0)
        ),
        "the stored radius, type and fee are kept; the radius is not checked"
    );
    let stored_after_first = Fence {
        id: 7,
        name: first.name.clone(),
        latitude: Some(first.latitude),
        longitude: Some(first.longitude),
        radius_m: Some(first.radius_m),
        billing_type: first.billing_type,
        cost_per_unit: first.cost_per_unit,
        session_fee: first.session_fee,
    };
    let again = edit(&fence("Gate A", 10.0, 20.0), &stored_after_first, false).expect("again");
    assert_eq!(
        again, first,
        "an edit that gives nothing new resolves to the stored values"
    );
    let cleared = edit(
        &Fence {
            cost_per_unit: Some(0.7),
            ..fence("Gate A", 10.0, 20.0)
        },
        &stored_after_first,
        true,
    )
    .expect("clear with a cost");
    assert_eq!(
        (
            cleared.billing_type,
            cleared.cost_per_unit,
            cleared.session_fee
        ),
        (Some(GeofenceBillingType::PerKwh), Some(0.7), None),
        "the tariff is cleared before the supplied options apply"
    );
    let wiped = edit(&fence("Gate A", 10.0, 20.0), &stored_after_first, true).expect("clear");
    assert_eq!(
        (wiped.billing_type, wiped.cost_per_unit, wiped.session_fee),
        (None, None, None)
    );
    assert_eq!(wiped.radius_m, 5.0, "clearing the tariff keeps the radius");
    assert_eq!(
        edit(
            &Fence {
                radius_m: Some(5.0),
                ..fence("Gate A", 10.0, 20.0)
            },
            &stored_after_first,
            false
        )
        .expect_err("a given radius is checked"),
        GeofenceRejection::RadiusBelowMinimum
    );
    // A stored billing type without a cost (imported fences have no NULL
    // type): kept, re-sending it changes nothing, another type is refused.
    let depot = imported(8, "Depot", 12.0, 20.0, 50.0);
    let depot = Fence {
        billing_type: Some(GeofenceBillingType::PerKwh),
        ..depot
    };
    let kept = edit(&fence("Depot", 12.0, 20.0), &depot, false).expect("kept");
    assert_eq!(
        (kept.radius_m, kept.billing_type, kept.cost_per_unit),
        (50.0, Some(GeofenceBillingType::PerKwh), None)
    );
    let resent = edit(
        &Fence {
            radius_m: Some(50.0),
            billing_type: Some(GeofenceBillingType::PerKwh),
            ..fence("Depot", 12.0, 20.0)
        },
        &depot,
        false,
    )
    .expect("the stored values re-sent");
    assert_eq!(resent, kept);
    assert_eq!(
        edit(
            &Fence {
                billing_type: Some(GeofenceBillingType::PerMinute),
                ..fence("Depot", 12.0, 20.0)
            },
            &depot,
            false
        )
        .expect_err("another type needs a cost"),
        GeofenceRejection::BillingTypeWithoutCost
    );
    let priced_depot = edit(
        &Fence {
            cost_per_unit: Some(0.4),
            ..fence("Depot", 12.0, 20.0)
        },
        &depot,
        false,
    )
    .expect("a cost keeps the stored type");
    assert_eq!(
        (priced_depot.billing_type, priced_depot.cost_per_unit),
        (Some(GeofenceBillingType::PerKwh), Some(0.4))
    );
    // A billing type given with no cost, where the resolved cost is the kept
    // stored cost: the type is given (C8 table 5.2), and with the tariff
    // cleared first there is no cost left, so it is refused.
    let retyped = Fence {
        billing_type: Some(GeofenceBillingType::PerKwh),
        ..fence("Gate", 10.0, 20.0)
    };
    let retyped_edit = edit(&retyped, &gate, false).expect("the kept cost admits the type");
    assert_eq!(
        (
            retyped_edit.radius_m,
            retyped_edit.billing_type,
            retyped_edit.cost_per_unit,
            retyped_edit.session_fee
        ),
        (5.0, Some(GeofenceBillingType::PerKwh), Some(0.5), Some(1.0))
    );
    assert_eq!(
        edit(&retyped, &gate, true).expect_err("no cost once the tariff is cleared"),
        GeofenceRejection::BillingTypeWithoutCost
    );
    // A radius given on a store edit is checked at the store seam too.
    let vehicle = c8_vehicle(&store, "t09-store");
    store
        .upsert_geofences(vehicle, &[imported(7, "Gate", 10.0, 20.0, 5.0)])
        .expect("import");
    assert_eq!(
        rejection(store.save_geofence(
            vehicle,
            Some(7),
            Fence {
                radius_m: Some(5.0),
                ..fence("Gate A", 10.0, 20.0)
            },
            false
        )),
        GeofenceRejection::RadiusBelowMinimum
    );
    assert_eq!(
        store.geofences(vehicle).expect("fences")[0].name,
        "Gate",
        "a refused edit writes nothing"
    );
}

#[test]
fn c8_t11_safe_error_text() {
    let (_temporary, store) = c8_store();
    // Four separate creates, each otherwise valid, with a latitude, a name, a
    // radius and a cost that are refused.
    let latitude = Fence {
        latitude: Some(95.123456),
        ..fence("Place", 0.0, 20.0)
    };
    let name = fence("Secret\nPlace", 10.0, 20.0);
    let radius = Fence {
        radius_m: Some(9.0),
        ..fence("Place", 10.0, 20.0)
    };
    let cost = Fence {
        cost_per_unit: Some(123_456.0),
        ..fence("Place", 10.0, 20.0)
    };
    let cases = [
        (
            latitude,
            GeofenceRejection::LatitudeInvalid,
            "geofence latitude must be a finite number from -90 to 90 degrees",
        ),
        (
            name,
            GeofenceRejection::NameControlCharacter,
            "geofence name contains a control or invisible formatting character",
        ),
        (
            radius,
            GeofenceRejection::RadiusBelowMinimum,
            "geofence radius must be at least 10 metres",
        ),
        (
            cost,
            GeofenceRejection::CostPerUnitInvalid,
            "geofence cost per unit must be a finite number from 0 to 99999.9999",
        ),
    ];
    for (index, (record, expected, text)) in cases.into_iter().enumerate() {
        let vehicle = c8_vehicle(&store, &format!("t11-{index}"));
        let error = store
            .save_geofence(vehicle, None, record, false)
            .expect_err("refused");
        assert!(
            matches!(error, StoreError::GeofenceRejected(rejected) if rejected == expected),
            "{error:?}"
        );
        let shown = error.to_string();
        assert_eq!(shown, text);
        for value in ["95.123456", "Secret", "9.0", "123456"] {
            assert!(!shown.contains(value), "{shown:?} carries {value:?}");
        }
    }
    // Every rejection displays exactly its stable text, through the error
    // type a caller sees, and no text holds an input value.
    let texts = [
        (
            GeofenceRejection::NameEmpty,
            "geofence name is empty after trimming",
        ),
        (
            GeofenceRejection::NameTooLong,
            "geofence name is longer than 255 characters",
        ),
        (
            GeofenceRejection::NameControlCharacter,
            "geofence name contains a control or invisible formatting character",
        ),
        (
            GeofenceRejection::LatitudeInvalid,
            "geofence latitude must be a finite number from -90 to 90 degrees",
        ),
        (
            GeofenceRejection::LongitudeInvalid,
            "geofence longitude must be a finite number from -180 to 180 degrees",
        ),
        (
            GeofenceRejection::RadiusNotFinite,
            "geofence radius must be a finite number of metres",
        ),
        (
            GeofenceRejection::RadiusNotWholeMetres,
            "geofence radius must be a whole number of metres",
        ),
        (
            GeofenceRejection::RadiusBelowMinimum,
            "geofence radius must be at least 10 metres",
        ),
        (
            GeofenceRejection::RadiusAboveMaximum,
            "geofence radius must be at most 30000 metres",
        ),
        (
            GeofenceRejection::BillingTypeWithoutCost,
            "geofence billing type needs a cost per unit",
        ),
        (
            GeofenceRejection::CostPerUnitInvalid,
            "geofence cost per unit must be a finite number from 0 to 99999.9999",
        ),
        (
            GeofenceRejection::SessionFeeInvalid,
            "geofence session fee must be a finite number from 0 to 999999999999.99",
        ),
        (
            GeofenceRejection::DuplicatePlace {
                existing_id: 2_147_483_649,
            },
            "geofence 2147483649 already has this centre and radius",
        ),
        (
            GeofenceRejection::IdsExhausted,
            "no geofence id is left for this vehicle",
        ),
    ];
    for (rejected, text) in texts {
        assert_eq!(rejected.to_string(), text);
        assert_eq!(StoreError::GeofenceRejected(rejected).to_string(), text);
        let source: &dyn std::error::Error = &rejected;
        assert_eq!(source.to_string(), text);
    }
    // The texts quote the stable bounds: the constants are the one definition.
    assert!(
        GeofenceRejection::RadiusBelowMinimum
            .to_string()
            .contains(&format!("{MIN_SAVED_GEOFENCE_RADIUS_METRES}").replace(".0", ""))
    );
    assert!(
        GeofenceRejection::RadiusAboveMaximum
            .to_string()
            .contains(&format!("{MAX_GEOFENCE_RADIUS_METRES}").replace(".0", ""))
    );
    assert!(
        GeofenceRejection::NameTooLong
            .to_string()
            .contains(&MAX_GEOFENCE_NAME_CHARS.to_string())
    );
    assert!(
        GeofenceRejection::CostPerUnitInvalid
            .to_string()
            .contains(&MAX_GEOFENCE_COST_PER_UNIT.to_string())
    );
    assert!(
        GeofenceRejection::SessionFeeInvalid
            .to_string()
            .contains(&MAX_GEOFENCE_SESSION_FEE.to_string())
    );
}

/// The bound `W` of the radius check in the predecessor shape of `geofences`,
/// read from the table's own definition.
pub(super) fn predecessor_radius_bound(connection: &Connection) -> f64 {
    let definition: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'geofences'",
            [],
            |row| row.get(0),
        )
        .expect("table definition");
    let (_, rest) = definition
        .split_once("radius_m <= ")
        .expect("a radius upper bound");
    rest.chars()
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect::<String>()
        .parse()
        .expect("a number")
}

/// Every column of every fence row as SQLite's `quote()` text, by id.
fn quoted_fence_rows(connection: &Connection) -> Vec<Vec<String>> {
    let mut statement = connection
        .prepare(
            "SELECT quote(vehicle_id), quote(source_geofence_id), quote(name), quote(latitude),
                    quote(longitude), quote(radius_m), quote(billing_type),
                    quote(cost_per_unit), quote(session_fee)
               FROM geofences ORDER BY vehicle_id, source_geofence_id",
        )
        .expect("prepare");
    statement
        .query_map([], |row| (0..9).map(|index| row.get(index)).collect())
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

pub(super) fn table_exists(connection: &Connection, name: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![name],
            |row| row.get(0),
        )
        .expect("table check")
}

pub(super) fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count")
}

#[test]
fn c8_t12_upgrade() {
    assert!(
        REWIND_V75_SQL.starts_with(REWIND_V76_SQL),
        "assertion failed: c8_t12_upgrade: Part I and older rewinds include the complete geofence undo"
    );
    let predecessor = GEOFENCE_STEP_SCHEMA_VERSION - 1;
    let temporary = crate::private_tempdir().expect("temporary store");
    let initialized = HubStore::initialize(temporary.path());
    assert!(
        initialized.is_ok(),
        "assertion failed: c8_t12_upgrade: a fresh store must migrate through the geofence predecessor: {:?}",
        initialized.as_ref().err()
    );
    let store = initialized.expect("asserted successful migration");
    let vehicle = c8_vehicle(&store, "t12");
    let connection = store.open().expect("connection");
    // The store at the step before the geofence step: its one rewind constant
    // (every older constant starts with it), then the version of the step
    // before.
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V76_SQL} PRAGMA user_version = {predecessor}; COMMIT;"
        ))
        .expect("rewind");
    assert_eq!(schema_version(&connection).unwrap(), predecessor);
    assert!(!table_exists(&connection, "geofence_id_marks"));
    assert!(!table_exists(&connection, "deleted_imported_geofences"));
    let owned_columns: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('geofences') WHERE name = 'source_owned'",
            [],
            |row| row.get(0),
        )
        .expect("columns");
    assert_eq!(owned_columns, 0, "no ownership column before the step");
    // The predecessor checks are back: they refuse what the step widens.
    let bound = predecessor_radius_bound(&connection);
    assert!(bound >= 100.0, "the predecessor bound holds a 100 m fence");
    assert!(bound < MAX_IMPORTED_GEOFENCE_RADIUS_METRES);
    let insert = |id: i64, name: &str, radius: f64, fee: f64| {
        connection.execute(
            "INSERT INTO geofences(vehicle_id, source_geofence_id, name, latitude, longitude,
                radius_m, billing_type, cost_per_unit, session_fee)
             VALUES (?1, ?2, ?3, 10.0, 20.0, ?4, 'per_minute', 0.1, ?5)",
            params![vehicle.to_string(), id, name, radius, fee],
        )
    };
    assert!(insert(90, "x", bound + 1.0, 0.0).is_err(), "radius bound");
    assert!(
        insert(91, &"a".repeat(257), 50.0, 0.0).is_err(),
        "name bound"
    );
    assert!(insert(92, "x", 50.0, -1.0).is_err(), "session fee check");
    // One fence with a 256-character ASCII name, the largest radius and a zero
    // session fee, and one with a multi-byte name and fractional values.
    insert(1, &"a".repeat(256), bound, 0.0).expect("name of 256 characters");
    connection
        .execute(
            "INSERT INTO geofences(vehicle_id, source_geofence_id, name, latitude, longitude,
                radius_m, billing_type, cost_per_unit, session_fee)
             VALUES (?1, 2, ?2, 51.507351, -0.127758, 100.5, 'per_kwh', 0.1235, 2.35)",
            params![vehicle.to_string(), "\u{754c}".repeat(85)],
        )
        .expect("second fence");
    let before = quoted_fence_rows(&connection);
    assert_eq!(before.len(), 2);
    let sync_before = (
        count(&connection, "sync_mutations"),
        count(&connection, "export_outbox"),
    );
    drop(connection);
    drop(store);
    // Standalone Serve migrates this shape: the preflight accepts it.
    let preflight = HubStore::preflight_standalone_schema_read_only(temporary.path());
    assert!(
        preflight.is_ok(),
        "assertion failed: c8_t12_upgrade: standalone preflight accepts the geofence predecessor: {preflight:?}"
    );

    let migrated = HubStore::initialize(temporary.path()).expect("migrated");
    let connection = migrated.open().expect("connection");
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    const { assert!(SCHEMA_VERSION >= GEOFENCE_STEP_SCHEMA_VERSION) };
    // Every old column is byte-equivalent; the new state is empty or default.
    assert_eq!(quoted_fence_rows(&connection), before);
    let owned: Vec<i64> = connection
        .prepare("SELECT source_owned FROM geofences ORDER BY source_geofence_id")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(owned, [0, 0], "no row is claimed as source-owned");
    for table in ["geofence_id_marks", "deleted_imported_geofences"] {
        assert!(table_exists(&connection, table), "{table}");
        assert_eq!(count(&connection, table), 0, "{table} starts empty");
    }
    let index: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
              WHERE type = 'index' AND name = 'geofences_vehicle_location'",
            [],
            |row| row.get(0),
        )
        .expect("index");
    assert_eq!(index, 1, "the location index is recreated");
    let violations: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .expect("foreign keys");
    assert_eq!(violations, 0);
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity");
    assert_eq!(integrity, "ok");
    // The upgrade relabels nothing and records nothing (C8 R33).
    assert_eq!(
        (
            count(&connection, "sync_mutations"),
            count(&connection, "export_outbox")
        ),
        sync_before
    );
    drop(connection);

    // The widened checks hold what an import copies: a 255-character name of
    // three-byte characters, the largest imported radius and a negative fee.
    let wide = Fence {
        billing_type: Some(GeofenceBillingType::PerKwh),
        session_fee: Some(-2.5),
        ..imported(
            3,
            &"\u{754c}".repeat(MAX_GEOFENCE_NAME_CHARS),
            10.0,
            20.0,
            MAX_IMPORTED_GEOFENCE_RADIUS_METRES,
        )
    };
    migrated
        .upsert_geofences(vehicle, std::slice::from_ref(&wide))
        .expect("import");
    let stored = migrated.geofences(vehicle).expect("fences");
    assert_eq!(stored.last(), Some(&wide), "stored exactly");
}

/// A fence row whose vehicle does not exist is a foreign key violation in the
/// rebuilt table: the step fails as a whole and the store stays as it was.
#[test]
fn c8_t12_foreign_key_check_rolls_the_step_back() {
    let predecessor = GEOFENCE_STEP_SCHEMA_VERSION - 1;
    let (temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t12-fk");
    let connection = store.open().expect("connection");
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V76_SQL} PRAGMA user_version = {predecessor}; COMMIT;"
        ))
        .expect("rewind");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("foreign keys off");
    connection
        .execute(
            "INSERT INTO geofences(vehicle_id, source_geofence_id, name, latitude, longitude, radius_m)
             VALUES (?1, 1, 'Orphan', 10.0, 20.0, 50.0)",
            params![Uuid::new_v4().to_string()],
        )
        .expect("orphan row");
    connection
        .execute(
            "INSERT INTO geofences(vehicle_id, source_geofence_id, name, latitude, longitude, radius_m)
             VALUES (?1, 2, 'Kept', 10.0, 20.0, 50.0)",
            params![vehicle.to_string()],
        )
        .expect("row");
    let before = quoted_fence_rows(&connection);
    drop(connection);
    drop(store);

    let failed = HubStore::initialize(temporary.path());
    assert!(
        matches!(failed, Err(StoreError::Migrate(_))),
        "got {:?}",
        failed.err()
    );
    let connection = Connection::open(temporary.path().join("hub.sqlite")).expect("open");
    assert_eq!(schema_version(&connection).unwrap(), predecessor);
    assert_eq!(quoted_fence_rows(&connection), before);
    assert!(!table_exists(&connection, "geofence_id_marks"));
    assert!(!table_exists(&connection, "deleted_imported_geofences"));
    assert!(!table_exists(&connection, "geofences_rebuilt"));

    // With foreign keys off on the migrating connection the copy cannot refuse
    // the orphan itself, so the step's own foreign key check is what does.
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("foreign keys off");
    let refused = migrate(&connection);
    assert!(
        matches!(refused, Err(StoreError::Migrate(_))),
        "got {refused:?}"
    );
    assert_eq!(schema_version(&connection).unwrap(), predecessor);
    assert_eq!(quoted_fence_rows(&connection), before);
    assert!(!table_exists(&connection, "geofence_id_marks"));
    assert!(!table_exists(&connection, "geofences_rebuilt"));
}

/// Import `rows` through the transaction-level upsert that the import
/// finalisation calls, in one IMMEDIATE transaction; its report.
pub(super) fn import_in_transaction(
    store: &HubStore,
    vehicle: Uuid,
    rows: &[Fence],
) -> GeofenceUpsertReport {
    let mut connection = store.open().expect("connection");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin");
    let report = upsert_geofences_in_transaction(&transaction, vehicle, rows).expect("upsert");
    transaction.commit().expect("commit");
    report
}

/// The `source_owned` column of one stored fence.
pub(super) fn source_owned_of(connection: &Connection, vehicle: Uuid, id: i64) -> i64 {
    connection
        .query_row(
            "SELECT source_owned FROM geofences
             WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
            params![vehicle.to_string(), id],
            |row| row.get(0),
        )
        .expect("stored fence")
}

/// The stored fence rows of one vehicle as `quote()` text, without the vehicle
/// column, so two vehicles compare equal when they hold the same fences.
pub(super) fn fence_snapshot(connection: &Connection, vehicle: Uuid) -> Vec<Vec<String>> {
    let key = format!("'{vehicle}'");
    quoted_fence_rows(connection)
        .into_iter()
        .filter(|row| row[0] == key)
        .map(|row| row[1..].to_vec())
        .collect()
}

/// Import `rows` on a fresh vehicle through `HubStore::upsert_geofences` and on
/// another through the transaction-level upsert; both must report the same and
/// store the same values. Returns the report and the stored rows.
fn imported_by_both_paths(
    store: &HubStore,
    key: &str,
    rows: &[Fence],
) -> (GeofenceUpsertReport, Vec<Vec<String>>) {
    let by_store = c8_vehicle(store, &format!("{key}-store"));
    let by_transaction = c8_vehicle(store, &format!("{key}-transaction"));
    let report = store.upsert_geofences(by_store, rows).expect("import");
    assert_eq!(
        import_in_transaction(store, by_transaction, rows),
        report,
        "{key}: report"
    );
    let connection = store.open().expect("connection");
    let stored = fence_snapshot(&connection, by_store);
    assert_eq!(
        fence_snapshot(&connection, by_transaction),
        stored,
        "{key}: stored rows"
    );
    (report, stored)
}

/// The activated rows of C8 T13, by id as the test names them.
fn t13_rows() -> Vec<Fence> {
    let tariff_nan = Fence {
        billing_type: Some(GeofenceBillingType::PerKwh),
        cost_per_unit: Some(f64::NAN),
        session_fee: Some(f64::NAN),
        ..imported(8, "H", 10.0002, 20.0, 50.0)
    };
    let negative_fee = Fence {
        billing_type: Some(GeofenceBillingType::PerKwh),
        cost_per_unit: None,
        session_fee: Some(-2.5),
        ..imported(6, "F", 10.0, 20.0, 32_767.0)
    };
    vec![
        imported(1, "A", 10.0, 20.0, 0.0),
        imported(2, "B", 10.0, 20.0, -3.0),
        Fence {
            latitude: None,
            ..imported(3, "C", 10.0, 20.0, 50.0)
        },
        imported(4, "D", 95.0, 20.0, 50.0),
        imported(5, "", 10.0, 20.0, 50.0),
        negative_fee,
        imported(7, "G\tate", 10.0001, 20.0, 3.0),
        tariff_nan,
        imported(9, "I", 10.0, 20.0, f64::NAN),
        imported(10, " \u{3000} ", 10.0, 20.0, 50.0),
        imported(0, "Z", 10.0, 20.0, 50.0),
        imported(-1, "Y", 10.0, 20.0, 50.0),
        imported(2_147_483_648, "X", 10.0, 20.0, 50.0),
    ]
}

/// The import rows grouped by the stable code of their reason, ids ascending
/// (the order an import processes them in).
fn inactive_by_code(rows: &[Fence]) -> std::collections::BTreeMap<&'static str, Vec<i64>> {
    let mut sorted: Vec<&Fence> = rows.iter().collect();
    sorted.sort_by_key(|row| row.id);
    let mut grouped = std::collections::BTreeMap::<&'static str, Vec<i64>>::new();
    for row in sorted {
        if let Err(reason) = row.import_activation() {
            grouped.entry(reason.code()).or_default().push(row.id);
        }
    }
    grouped
}

#[test]
fn c8_t13_activation() {
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t13");
    let rows = t13_rows();
    let report = store.upsert_geofences(vehicle, &rows).expect("import");
    assert_eq!(rows.len(), 13, "source rows");
    // The report of C8 T13 (C8 R14): every list holds ids ascending.
    assert_eq!(report.source_rows, 13);
    assert_eq!(report.inserted, [6, 7, 8]);
    assert!(report.updated.is_empty());
    assert!(report.unchanged.is_empty());
    assert!(report.kept_hub_values.is_empty());
    assert!(report.deleted_in_hub.is_empty());
    assert_eq!(report.tariff_not_finite, [8]);
    assert!(report.absent_from_source.is_empty());
    assert_eq!(report.relabel, RelabelCounts::default());
    let expected_inactive: std::collections::BTreeMap<GeofenceInactiveReason, Vec<i64>> = [
        (
            GeofenceInactiveReason::IdOutOfRange,
            vec![-1, 0, 2_147_483_648],
        ),
        (GeofenceInactiveReason::NameEmpty, vec![5, 10]),
        (GeofenceInactiveReason::CentreMissing, vec![3]),
        (GeofenceInactiveReason::CentreOutOfRange, vec![4]),
        (GeofenceInactiveReason::RadiusOutOfRange, vec![9]),
        (GeofenceInactiveReason::RadiusNotPositive, vec![1, 2]),
    ]
    .into_iter()
    .collect();
    assert_eq!(report.inactive, expected_inactive);
    // Every source row is in exactly one list (C8 section 1.2).
    let listed = report.inserted.len()
        + report.updated.len()
        + report.unchanged.len()
        + report.kept_hub_values.len()
        + report.deleted_in_hub.len()
        + report.inactive.values().map(Vec::len).sum::<usize>();
    assert_eq!(u64::try_from(listed).expect("count"), report.source_rows);
    // Exactly ids 6, 7 and 8 are active, with the values as given: the tab,
    // the radius of 3 and the negative fee are kept, and the not-finite tariff
    // is stored NULL with its billing type.
    let stored = store.geofences(vehicle).expect("fences");
    assert_eq!(
        stored.iter().map(|row| row.id).collect::<Vec<_>>(),
        [6, 7, 8]
    );
    assert_eq!(stored[0], rows[5]);
    assert_eq!(stored[1], rows[6]);
    assert_eq!(stored[1].name, "G\tate");
    assert_eq!(stored[1].radius_m, Some(3.0));
    assert_eq!(stored[0].session_fee, Some(-2.5));
    assert_eq!(
        stored[2],
        Fence {
            id: 8,
            name: "H".to_owned(),
            latitude: Some(10.0002),
            longitude: Some(20.0),
            radius_m: Some(50.0),
            billing_type: Some(GeofenceBillingType::PerKwh),
            cost_per_unit: None,
            session_fee: None,
        }
    );
    let connection = store.open().expect("connection");
    // An imported row is written source-owned (C8 R16a).
    for id in [6, 7, 8] {
        assert_eq!(source_owned_of(&connection, vehicle, id), 1, "row {id}");
    }
    let raw_tariff: (Option<f64>, Option<f64>) = connection
        .query_row(
            "SELECT cost_per_unit, session_fee FROM geofences WHERE source_geofence_id = 8",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("row 8");
    assert_eq!(raw_tariff, (None, None), "NaN is stored NULL");
    // The transaction-level upsert the import finalisation calls stores the
    // same rows as the store-level one.
    let (both_report, transaction_rows) = imported_by_both_paths(&store, "t13", &rows);
    assert_eq!(both_report, report, "both import paths report alike");
    assert_eq!(transaction_rows, fence_snapshot(&connection, vehicle));
    // A not-finite price is stored NULL on both paths, and the type is kept.
    let unbounded = Fence {
        billing_type: Some(GeofenceBillingType::PerMinute),
        cost_per_unit: Some(f64::INFINITY),
        session_fee: Some(f64::NEG_INFINITY),
        ..imported(21, "Unbounded", 10.0, 20.0, 50.0)
    };
    let (unbounded_report, unbounded_rows) =
        imported_by_both_paths(&store, "t13-infinite", std::slice::from_ref(&unbounded));
    assert_eq!(unbounded_report.inserted, [21]);
    assert_eq!(unbounded_report.tariff_not_finite, [21]);
    assert_eq!(
        unbounded_rows[0][5..],
        ["'per_minute'", "NULL", "NULL"],
        "billing type kept, cost and fee NULL"
    );
    assert_eq!(unbounded_rows[0][1], "'Unbounded'");

    // The reasons, by stable code, ids ascending; the activated rows are the
    // rest of the slice.
    let expected: std::collections::BTreeMap<&str, Vec<i64>> = [
        ("id_out_of_range", vec![-1, 0, 2_147_483_648]),
        ("name_empty", vec![5, 10]),
        ("centre_missing", vec![3]),
        ("centre_out_of_range", vec![4]),
        ("radius_out_of_range", vec![9]),
        ("radius_not_positive", vec![1, 2]),
    ]
    .into_iter()
    .collect();
    let inactive = inactive_by_code(&rows);
    assert_eq!(inactive, expected);
    assert_eq!(
        inactive.values().map(Vec::len).sum::<usize>() + stored.len(),
        rows.len(),
        "every source row is activated or listed under one reason"
    );
    // Rows of a tariff that is not finite are the ones the report lists.
    let not_finite: Vec<i64> = rows
        .iter()
        .filter(|row| row.import_activation().is_ok())
        .filter(|row| ImportedGeofenceTariff::of(row).not_finite)
        .map(|row| row.id)
        .collect();
    assert_eq!(not_finite, report.tariff_not_finite);
    assert_eq!(
        rows[8].import_activation(),
        Err(GeofenceInactiveReason::RadiusOutOfRange)
    );
    assert_eq!(
        rows[0].import_activation(),
        Err(GeofenceInactiveReason::RadiusNotPositive)
    );
    // The stable codes.
    for (reason, code) in [
        (GeofenceInactiveReason::IdOutOfRange, "id_out_of_range"),
        (GeofenceInactiveReason::NameEmpty, "name_empty"),
        (GeofenceInactiveReason::CentreMissing, "centre_missing"),
        (
            GeofenceInactiveReason::CentreOutOfRange,
            "centre_out_of_range",
        ),
        (GeofenceInactiveReason::RadiusMissing, "radius_missing"),
        (
            GeofenceInactiveReason::RadiusOutOfRange,
            "radius_out_of_range",
        ),
        (
            GeofenceInactiveReason::RadiusNotPositive,
            "radius_not_positive",
        ),
    ] {
        assert_eq!(reason.code(), code);
    }

    // The checks run in the order id, name, centre, radius, and each boundary
    // is where C8 R13 puts it.
    let row = |id: i64,
               name: &str,
               latitude: Option<f64>,
               longitude: Option<f64>,
               radius: Option<f64>| Fence {
        id,
        name: name.to_owned(),
        latitude,
        longitude,
        radius_m: radius,
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
    };
    let all_wrong = row(0, " ", None, None, None);
    assert_eq!(
        all_wrong.import_activation(),
        Err(GeofenceInactiveReason::IdOutOfRange)
    );
    let all_wrong = row(1, " ", None, None, None);
    assert_eq!(
        all_wrong.import_activation(),
        Err(GeofenceInactiveReason::NameEmpty)
    );
    let all_wrong = row(1, "N", None, Some(500.0), None);
    assert_eq!(
        all_wrong.import_activation(),
        Err(GeofenceInactiveReason::CentreMissing)
    );
    let all_wrong = row(1, "N", Some(0.0), None, Some(50.0));
    assert_eq!(
        all_wrong.import_activation(),
        Err(GeofenceInactiveReason::CentreMissing)
    );
    let all_wrong = row(1, "N", Some(f64::NAN), Some(0.0), None);
    assert_eq!(
        all_wrong.import_activation(),
        Err(GeofenceInactiveReason::CentreOutOfRange)
    );
    let all_wrong = row(1, "N", Some(0.0), Some(180.000001), None);
    assert_eq!(
        all_wrong.import_activation(),
        Err(GeofenceInactiveReason::CentreOutOfRange)
    );
    let all_wrong = row(1, "N", Some(0.0), Some(0.0), None);
    assert_eq!(
        all_wrong.import_activation(),
        Err(GeofenceInactiveReason::RadiusMissing)
    );
    for (radius, expected) in [
        (f64::NAN, GeofenceInactiveReason::RadiusOutOfRange),
        (f64::NEG_INFINITY, GeofenceInactiveReason::RadiusOutOfRange),
        (f64::INFINITY, GeofenceInactiveReason::RadiusOutOfRange),
        (0.0, GeofenceInactiveReason::RadiusNotPositive),
        (-0.0, GeofenceInactiveReason::RadiusNotPositive),
        (-1.0, GeofenceInactiveReason::RadiusNotPositive),
        (32_767.5, GeofenceInactiveReason::RadiusOutOfRange),
    ] {
        assert_eq!(
            row(1, "N", Some(0.0), Some(0.0), Some(radius)).import_activation(),
            Err(expected),
            "{radius}"
        );
    }
    for (id, name, latitude, longitude, radius) in [
        (1, "N", 90.0, 180.0, f64::MIN_POSITIVE),
        (
            MAX_IMPORTED_GEOFENCE_ID,
            "\u{200B}",
            -90.0,
            -180.0,
            32_767.0,
        ),
        (7, "  padded  ", 10.123456789, 20.0, 2.5),
    ] {
        assert_eq!(
            row(id, name, Some(latitude), Some(longitude), Some(radius)).import_activation(),
            Ok((latitude, longitude, radius)),
            "{id}"
        );
    }
    assert_eq!(MAX_IMPORTED_GEOFENCE_ID, FIRST_HUB_GEOFENCE_ID - 1);

    // Activation does not normalise: a name with edge spaces, a centre beyond
    // six decimals, a radius below the Hub minimum, a fraction, a negative
    // price and a cost beyond four decimals are stored as read.
    let copied = Fence {
        billing_type: Some(GeofenceBillingType::PerMinute),
        cost_per_unit: Some(-0.123456),
        session_fee: Some(1.005),
        ..imported(11, "  Padded  ", 10.123456789, -0.0000001, 2.5)
    };
    assert!(copied.radius_m.expect("radius") < MIN_SAVED_GEOFENCE_RADIUS_METRES);
    let other = c8_vehicle(&store, "t13-copy");
    assert_eq!(
        store
            .upsert_geofences(other, std::slice::from_ref(&copied))
            .expect("import")
            .inserted,
        [11]
    );
    assert_eq!(
        store.geofences(other).expect("fences"),
        std::slice::from_ref(&copied)
    );
    // The transaction-level upsert writes the name as read too.
    let (copied_report, copied_rows) =
        imported_by_both_paths(&store, "t13-copy", std::slice::from_ref(&copied));
    assert_eq!(copied_report.inserted, [11]);
    assert_eq!(copied_rows[0][1], "'  Padded  '");
    assert_eq!(copied_rows, fence_snapshot(&connection, other));
}

#[test]
fn c8_t14_inactive_keeps_row() {
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t14");
    store
        .upsert_geofences(vehicle, &t13_rows())
        .expect("the C8 T13 import");
    let connection = store.open().expect("connection");
    let before = fence_snapshot(&connection, vehicle);
    assert_eq!(before.len(), 3);
    let stored_before = store.geofences(vehicle).expect("fences");
    // C8 R14: a row that is not activated is listed by its reason; the active
    // rows 7 and 8 are not in the slice and stay (C8 R16a).
    let assert_kept = |report: &GeofenceUpsertReport, code: &str, context: &str| {
        assert_eq!(report.source_rows, 1, "{context}");
        assert_eq!(
            report
                .inactive
                .iter()
                .map(|(reason, ids)| (reason.code(), ids.clone()))
                .collect::<Vec<_>>(),
            [(code, vec![6])],
            "{context}"
        );
        assert!(
            report.inserted.is_empty() && report.updated.is_empty(),
            "{context}"
        );
        assert!(
            report.unchanged.is_empty() && report.kept_hub_values.is_empty(),
            "{context}"
        );
        assert_eq!(report.absent_from_source, [7, 8], "{context}");
        assert_eq!(report.relabel, RelabelCounts::default(), "{context}");
    };
    // The same imports run through the transaction-level upsert on a second
    // vehicle; after every step both hold the same rows.
    let twin = c8_vehicle(&store, "t14-transaction");
    assert_eq!(
        import_in_transaction(&store, twin, &t13_rows()).inserted,
        [6, 7, 8]
    );
    assert_eq!(
        fence_snapshot(&connection, twin),
        fence_snapshot(&connection, vehicle)
    );
    let twin_before = fence_snapshot(&connection, twin);

    // The same id with a radius that is not positive: nothing is written.
    let replacement = imported(6, "F", 10.0, 20.0, 0.0);
    assert_eq!(
        inactive_by_code(std::slice::from_ref(&replacement)),
        [("radius_not_positive", vec![6])].into_iter().collect()
    );
    let report = store
        .upsert_geofences(vehicle, std::slice::from_ref(&replacement))
        .expect("import");
    assert_kept(&report, "radius_not_positive", "store path");
    assert_eq!(
        fence_snapshot(&connection, vehicle),
        before,
        "rows 6, 7 and 8 unchanged"
    );
    assert_eq!(store.geofences(vehicle).expect("fences"), stored_before);
    for id in [6, 7, 8] {
        assert_eq!(source_owned_of(&connection, vehicle, id), 1, "row {id}");
    }
    assert_eq!(
        import_in_transaction(&store, twin, std::slice::from_ref(&replacement)),
        report
    );
    assert_eq!(fence_snapshot(&connection, twin), twin_before);

    // Every other way a replacement can fail leaves the active row alone, and
    // a failing row does not stop a valid one in the same slice.
    let invalid = [
        (
            Fence {
                radius_m: None,
                ..replacement.clone()
            },
            "radius_missing",
        ),
        (
            Fence {
                radius_m: Some(f64::NAN),
                ..replacement.clone()
            },
            "radius_out_of_range",
        ),
        (
            Fence {
                radius_m: Some(40_000.0),
                ..replacement.clone()
            },
            "radius_out_of_range",
        ),
        (
            Fence {
                name: "   ".to_owned(),
                radius_m: Some(80.0),
                ..replacement.clone()
            },
            "name_empty",
        ),
        (
            Fence {
                latitude: None,
                radius_m: Some(80.0),
                ..replacement.clone()
            },
            "centre_missing",
        ),
        (
            Fence {
                latitude: Some(91.0),
                radius_m: Some(80.0),
                ..replacement.clone()
            },
            "centre_out_of_range",
        ),
    ];
    for (row, code) in invalid {
        assert_eq!(
            inactive_by_code(std::slice::from_ref(&row)),
            [(code, vec![6])].into_iter().collect(),
            "{code}"
        );
        let report = store
            .upsert_geofences(vehicle, std::slice::from_ref(&row))
            .expect("import");
        assert_kept(&report, code, code);
        assert_eq!(fence_snapshot(&connection, vehicle), before, "{code}");
        assert_eq!(
            import_in_transaction(&store, twin, std::slice::from_ref(&row)),
            report,
            "{code}"
        );
        assert_eq!(fence_snapshot(&connection, twin), twin_before, "{code}");
    }
    let fresh = imported(12, "Fresh", 30.0, 40.0, 60.0);
    let mixed = store
        .upsert_geofences(vehicle, &[replacement.clone(), fresh.clone()])
        .expect("import");
    assert_eq!(mixed.source_rows, 2);
    assert_eq!(mixed.inserted, [12]);
    assert_eq!(mixed.inactive.len(), 1);
    assert_eq!(mixed.absent_from_source, [7, 8]);
    assert_eq!(source_owned_of(&connection, vehicle, 12), 1);
    assert_eq!(
        import_in_transaction(&store, twin, &[replacement, fresh.clone()]),
        mixed
    );
    assert_eq!(
        fence_snapshot(&connection, twin),
        fence_snapshot(&connection, vehicle)
    );
    let after = store.geofences(vehicle).expect("fences");
    assert_eq!(after.len(), 4);
    assert_eq!(after[..3], stored_before[..]);
    assert_eq!(after[3], fresh);
}

/// The constants of C8 section 1.1 are the only definitions of their values and
/// agree with one another and with the stored bounds.
#[test]
fn c8_section_1_1_constants() {
    assert_eq!(MIN_SAVED_GEOFENCE_RADIUS_METRES, 10.0);
    assert_eq!(MAX_GEOFENCE_RADIUS_METRES, 30_000.0);
    assert_eq!(MAX_IMPORTED_GEOFENCE_RADIUS_METRES, 32_767.0);
    assert_eq!(FIRST_HUB_GEOFENCE_ID, 2_147_483_648);
    assert_eq!(MAX_GEOFENCE_NAME_CHARS, 255);
    assert_eq!(MAX_GEOFENCE_NAME_BYTES, 1_020);
    // The Hub bounds sit inside the stored bound, so every saved radius fits.
    const {
        assert!(MIN_SAVED_GEOFENCE_RADIUS_METRES <= DEFAULT_GEOFENCE_RADIUS_METRES);
        assert!(DEFAULT_GEOFENCE_RADIUS_METRES <= MAX_GEOFENCE_RADIUS_METRES);
        assert!(MAX_GEOFENCE_RADIUS_METRES <= MAX_IMPORTED_GEOFENCE_RADIUS_METRES);
    };
    // The stored checks are built from those constants.
    let (_temporary, store) = c8_store();
    let connection = store.open().expect("connection");
    let definition: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'geofences'",
            [],
            |row| row.get(0),
        )
        .expect("definition");
    assert!(definition.contains(&format!(
        "radius_m <= {MAX_IMPORTED_GEOFENCE_RADIUS_METRES:?}"
    )));
    assert!(definition.contains(&format!("BETWEEN 1 AND {MAX_GEOFENCE_NAME_BYTES}")));
    assert!(
        !definition.contains("session_fee IS NULL OR"),
        "no fee check"
    );
    // The id bounds of the two added tables are built from the constants too.
    let table_sql = |name: &str| -> String {
        connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![name],
                |row| row.get(0),
            )
            .expect("table definition")
    };
    assert!(
        table_sql("geofence_id_marks").contains(&format!("last_hub_id >= {FIRST_HUB_GEOFENCE_ID}"))
    );
    assert!(
        table_sql("deleted_imported_geofences")
            .contains(&format!("BETWEEN 1 AND {MAX_IMPORTED_GEOFENCE_ID}"))
    );
}
