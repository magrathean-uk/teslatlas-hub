// SPDX-License-Identifier: AGPL-3.0-only

//! C2-B lifecycle-level spec tests (C2 section 18.1, C2 R77 and R78; C2 T80 and
//! C2 T81 (a) to (e)): the per-kWh cost of a DC session prices the counter only,
//! the maker's own name `Tesla` is no information as a charger type, and a
//! free-Supercharging session is one typed `Supercharger` or a DC session of
//! another DC type whose samples carry the brand `Tesla` and no other brand. The
//! store-level tests (C2 T80's manual-entry case and C2 T81 (f)) are in
//! `src/storage/db/tests/b3_c2_b.rs` and the gate-level test (C2 T82) in
//! `gate_tests/c2_b_tests.rs`. Synthetic data only. `T0` is 1_760_000_000_000 ms.

use super::*;

const T0: i64 = 1_760_000_000_000;

/// A closed charge row with the given energies and session kind and type; every
/// other field is empty. It runs 95.5 minutes from `T0`.
fn row(
    used: Option<f64>,
    added: Option<f64>,
    is_dc: Option<bool>,
    fast_charger_type: Option<&str>,
) -> ProjectionCharge {
    ProjectionCharge {
        id: 1,
        car_id: 1,
        start_date_ms: T0,
        end_date_ms: Some(T0 + 5_730_000),
        charge_energy_added: added,
        charge_energy_used_kwh: used,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        cost: None,
        fast_charger_type: fast_charger_type.map(str::to_owned),
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
        start_latitude: None,
        start_longitude: None,
        start_battery_level: None,
        end_battery_level: None,
        duration_min: Some(95),
        address: None,
        location_name: None,
        geofence: None,
        is_dc,
        charge_rate_km_per_hour: None,
        max_charger_power_kw: None,
        outside_temp_avg: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
    }
}

fn per_kwh(rate: f64) -> Option<ChargeTariff> {
    Some(ChargeTariff {
        billing_type: GeofenceBillingType::PerKwh,
        cost_per_unit: Some(rate),
        session_fee: None,
    })
}

fn context(free: bool, brand_is_tesla: bool) -> ChargeCostContext {
    ChargeCostContext {
        free_supercharging: free,
        brand_is_tesla,
        span_observed: true,
    }
}

/// One sample with the given brand and type; every other field is empty.
fn sample(id: i64, brand: Option<&str>, kind: Option<&str>) -> ProjectionChargeSample {
    ProjectionChargeSample {
        id,
        charge_process_id: 1,
        timestamp_ms: T0 + id * 60_000,
        battery_level: None,
        usable_battery_level: None,
        charge_energy_added_kwh: None,
        charger_power_kw: None,
        charger_voltage: None,
        charger_actual_current: None,
        charger_pilot_current: None,
        charger_phases: None,
        ideal_range_km: None,
        rated_range_km: None,
        outside_temp_c: None,
        battery_heater_on: None,
        battery_heater: None,
        battery_heater_no_power: None,
        not_enough_power_to_heat: None,
        fast_charger_present: None,
        fast_charger_brand: brand.map(str::to_owned),
        fast_charger_type: kind.map(str::to_owned),
        charge_cable: None,
    }
}

fn branded(brands: &[Option<&str>]) -> Vec<ProjectionChargeSample> {
    brands
        .iter()
        .enumerate()
        .map(|(index, brand)| sample(index as i64 + 1, *brand, None))
        .collect()
}

#[test]
fn c2_t80_dc_counter_basis() {
    // `calculate_charge_cost` with context not free, span observed and
    // `brand_is_tesla` false (C2 R77, T80).
    let cases: [(&str, ProjectionCharge, f64, Option<f64>); 4] = [
        (
            "DC prices the counter, not the inlet energy",
            row(Some(8.56), Some(8.00), Some(true), None),
            0.50,
            Some(4.00),
        ),
        (
            "DC with no counter has no per-kWh cost, whatever the integral says",
            row(Some(8.00), None, Some(true), None),
            0.50,
            None,
        ),
        (
            "DC with a counter and no integral prices the counter",
            row(None, Some(9.00), Some(true), None),
            0.50,
            Some(4.50),
        ),
        (
            "an unknown kind keeps the inlet energy basis",
            row(Some(10.00), Some(9.00), None, None),
            0.30,
            Some(3.00),
        ),
    ];
    for (label, charge, rate, want) in cases {
        let got = calculate_charge_cost(&charge, per_kwh(rate), context(false, false));
        assert_eq!(got, want, "{label}");
    }
    // The change is the basis only: neither a DC session typed as a Supercharger
    // site nor the Tesla brand restores the integral for pricing, and a
    // per-minute tariff still prices the connected minutes of the same row.
    let no_counter = row(Some(8.00), None, Some(true), Some("Combo"));
    for brand_is_tesla in [false, true] {
        assert_eq!(
            calculate_charge_cost(&no_counter, per_kwh(0.50), context(false, brand_is_tesla)),
            None
        );
    }
    let per_minute = Some(ChargeTariff {
        billing_type: GeofenceBillingType::PerMinute,
        cost_per_unit: Some(0.10),
        session_fee: None,
    });
    assert_eq!(
        calculate_charge_cost(&no_counter, per_minute, context(false, false)),
        Some(9.55)
    );
}

#[test]
fn c2_t81_type_and_brand_rules() {
    // (a) `fast_charger_kind`: the maker's name says nothing, with the same
    // normalisation as every other name; `Teslas` is a new name.
    for text in ["Tesla", " tesla ", "TESLA", "FastChargerTesla"] {
        assert_eq!(
            fast_charger_kind(Some(text)),
            FastChargerKind::NoInformation,
            "{text:?}"
        );
    }
    assert_eq!(
        fast_charger_kind(Some("Teslas")),
        FastChargerKind::Unrecognised
    );
    assert_eq!(
        fast_charger_kind(Some("Supercharger")),
        FastChargerKind::Supercharger
    );
    assert_eq!(fast_charger_kind(Some("Combo")), FastChargerKind::OtherDc);
    assert_eq!(
        fast_charger_kind(Some("MCSingleWireCAN")),
        FastChargerKind::TeslaAc
    );

    // (b) Session type (C2 R32): `Tesla` sets no type.
    let session_type = |kinds: &[&str]| -> Option<String> {
        kinds
            .iter()
            .enumerate()
            .fold(None, |current, (index, kind)| {
                session_charger_type_after(current, &sample(index as i64 + 1, None, Some(kind)))
            })
    };
    assert_eq!(
        session_type(&["MCSingleWireCAN", "Tesla"]).as_deref(),
        Some("MCSingleWireCAN")
    );
    assert_eq!(
        session_type(&["<invalid>", "Tesla", "Combo"]).as_deref(),
        Some("Combo")
    );
    assert_eq!(session_type(&["Tesla", "Tesla"]), None);

    // (c) Kind (C2 R31): `Tesla` with the charger present and 7 kW is DC
    // evidence as `<invalid>` is; on an `A7` sample with no present flag it is AC.
    let mut present = sample(1, None, Some("Tesla"));
    present.fast_charger_present = Some(true);
    present.charger_power_kw = Some(7.0);
    assert_eq!(sample_kind_evidence(&present), Some(true));
    let mut invalid = present.clone();
    invalid.fast_charger_type = Some("<invalid>".to_owned());
    assert_eq!(sample_kind_evidence(&invalid), Some(true));
    let mut a7 = sample(2, None, Some("Tesla"));
    a7.charger_power_kw = Some(7.0);
    a7.charger_voltage = Some(240.0);
    a7.charger_actual_current = Some(30.0);
    a7.charger_phases = Some(1);
    assert_eq!(sample_kind_evidence(&a7), Some(false));

    // (d) `charge_brand_is_tesla` over the samples' brands.
    let cases: [(&str, Vec<Option<&str>>, bool); 10] = [
        ("two Tesla", vec![Some("Tesla"), Some("Tesla")], true),
        (
            "invalid then Tesla",
            vec![Some("<invalid>"), Some("Tesla")],
            true,
        ),
        ("padded lower case", vec![Some(" tesla ")], true),
        ("upper case", vec![Some("TESLA")], true),
        (
            "every no-brand text with one Tesla",
            vec![Some("Unknown"), Some("SNA"), Some(""), None, Some("Tesla")],
            true,
        ),
        (
            "Tesla and another brand",
            vec![Some("Tesla"), Some("BrandX")],
            false,
        ),
        ("only invalid", vec![Some("<invalid>")], false),
        ("absent", vec![None], false),
        ("another brand only", vec![Some("BrandX")], false),
        ("no samples", vec![], false),
    ];
    for (label, brands, want) in cases {
        let samples = branded(&brands);
        assert_eq!(charge_brand_is_tesla(&samples), want, "{label}");
        // The slice wrapper is the production fold: any order, one body.
        let mut reversed = samples.clone();
        reversed.reverse();
        assert_eq!(
            ChargeSampleFacts::of(&samples),
            ChargeSampleFacts::of(&reversed)
        );
        assert_eq!(
            ChargeSampleFacts::of(&reversed).brand_is_tesla(),
            want,
            "{label}"
        );
        // An independently stated rule over the same texts.
        let texts: Vec<String> = brands
            .iter()
            .flatten()
            .map(|text| text.trim().to_ascii_lowercase())
            .collect();
        let tesla = texts.iter().any(|text| text == "tesla");
        let other = texts
            .iter()
            .any(|text| !["tesla", "", "<invalid>", "unknown", "sna"].contains(&text.as_str()));
        assert_eq!(tesla && !other, want, "{label}: reference");
    }

    // (e) `calculate_charge_cost` with `free_supercharging` true and the span
    // observed, tariff per_kwh 0.50 unless stated.
    let dc_combo = row(None, Some(9.00), Some(true), Some("Combo"));
    let table: [(
        &str,
        ProjectionCharge,
        Option<ChargeTariff>,
        ChargeCostContext,
        Option<f64>,
    ); 7] = [
        (
            "DC Combo, Tesla brand",
            dc_combo.clone(),
            per_kwh(0.50),
            context(true, true),
            Some(0.0),
        ),
        (
            "DC Combo, no Tesla brand",
            dc_combo.clone(),
            per_kwh(0.50),
            context(true, false),
            Some(4.50),
        ),
        (
            "DC Combo, setting off",
            dc_combo,
            per_kwh(0.50),
            context(false, true),
            Some(4.50),
        ),
        (
            "DC without a type is not freed",
            row(None, Some(9.00), Some(true), None),
            None,
            context(true, true),
            None,
        ),
        (
            "typed Supercharger is freed by its type alone",
            row(None, None, Some(true), Some("Supercharger")),
            None,
            context(true, false),
            Some(0.0),
        ),
        (
            "AC is never freed by a brand",
            row(Some(10.00), None, Some(false), Some("MCSingleWireCAN")),
            per_kwh(0.30),
            context(true, true),
            Some(3.00),
        ),
        (
            "an unknown kind is never freed by a brand",
            row(Some(10.00), None, None, Some("Combo")),
            per_kwh(0.50),
            context(true, true),
            Some(5.00),
        ),
    ];
    for (label, charge, tariff, context, want) in table {
        assert_eq!(
            calculate_charge_cost(&charge, tariff, context),
            want,
            "{label}"
        );
    }
    // The predicate itself: clause (i) or clause (ii), nothing else.
    let site = |kind: Option<&str>, is_dc: Option<bool>, brand: bool| {
        is_free_supercharging_site(&row(None, None, is_dc, kind), brand)
    };
    assert!(site(Some("Supercharger"), Some(true), false));
    assert!(site(Some("FastChargerSupercharger"), None, false));
    for other_dc in ["Combo", "CHAdeMO", "GB"] {
        assert!(site(Some(other_dc), Some(true), true), "{other_dc}");
        assert!(!site(Some(other_dc), Some(true), false), "{other_dc}");
        assert!(!site(Some(other_dc), Some(false), true), "{other_dc}");
        assert!(!site(Some(other_dc), None, true), "{other_dc}");
    }
    for no_clause in [None, Some("Tesla"), Some("CCS"), Some("ACSingleWireCAN")] {
        assert!(!site(no_clause, Some(true), true), "{no_clause:?}");
    }
}
