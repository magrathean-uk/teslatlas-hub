// SPDX-License-Identifier: AGPL-3.0-only

//! Part C2-P3 spec tests for the cost function (C2 specification section 10:
//! T30, T30b, T30c) and the rules of R40 to R44a and R55 that no numbered test
//! pins: tariff validity and copy, the observed span, the continuation check,
//! the energy basis per session kind, the range of a computed cost and the
//! zero edge values of R58. The store-level tests (T31 to T34, T48, T49, T62,
//! T63) are in `src/storage/db/tests/cost_close.rs`. Synthetic data only.
//! `T0` is 1_760_000_000_000 ms.

use super::*;

const T0: i64 = 1_760_000_000_000;
const MIN_MS: i64 = 60_000;

/// A closed charge row with the given energies and session kind; every other
/// field is empty. It starts at `T0` and ends 95.5 minutes later (5,730,000
/// ms), the spec's per-minute example.
fn row(used: Option<f64>, added: Option<f64>, is_dc: Option<bool>) -> ProjectionCharge {
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
        fast_charger_type: None,
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

fn ac(used: Option<f64>, added: Option<f64>) -> ProjectionCharge {
    row(used, added, Some(false))
}

fn per_kwh(rate: Option<f64>, fee: Option<f64>) -> Option<ChargeTariff> {
    Some(ChargeTariff {
        billing_type: GeofenceBillingType::PerKwh,
        cost_per_unit: rate,
        session_fee: fee,
    })
}

fn per_minute(rate: Option<f64>, fee: Option<f64>) -> Option<ChargeTariff> {
    Some(ChargeTariff {
        billing_type: GeofenceBillingType::PerMinute,
        cost_per_unit: rate,
        session_fee: fee,
    })
}

fn context(free: bool, span_observed: bool) -> ChargeCostContext {
    ChargeCostContext {
        free_supercharging: free,
        brand_is_tesla: false,
        span_observed,
    }
}

fn kind(mut charge: ProjectionCharge, text: &str) -> ProjectionCharge {
    charge.fast_charger_type = Some(text.to_owned());
    charge
}

/// One row of T30: the row, its tariff, the context and the expected cost.
type CostCase = (
    &'static str,
    ProjectionCharge,
    Option<ChargeTariff>,
    ChargeCostContext,
    Option<f64>,
);

#[test]
fn t30_the_cost_table() {
    let mut open_ended = row(None, None, Some(false));
    open_ended.end_date_ms = None;
    let cases: Vec<CostCase> = vec![
        (
            "used 22.22 with fee",
            ac(Some(22.22), Some(20.00)),
            per_kwh(Some(0.30), Some(1.00)),
            context(false, true),
            Some(7.67),
        ),
        (
            "used 22.22 without fee",
            ac(Some(22.22), Some(20.00)),
            per_kwh(Some(0.30), None),
            context(false, true),
            Some(6.67),
        ),
        (
            "AC without an inlet energy is unknown, never the battery energy",
            ac(None, Some(20.00)),
            per_kwh(Some(0.30), None),
            context(false, true),
            None,
        ),
        (
            "DC prices the counter",
            row(Some(8.00), Some(9.00), Some(true)),
            per_kwh(Some(0.50), None),
            context(false, true),
            Some(4.50),
        ),
        (
            "DC without a counter has no per-kWh cost: the integral does not stand in (C2 R77)",
            row(Some(8.00), None, Some(true)),
            per_kwh(Some(0.50), None),
            context(false, true),
            None,
        ),
        (
            "unknown kind takes the AC rule",
            row(Some(10.00), Some(9.00), None),
            per_kwh(Some(0.30), None),
            context(false, true),
            Some(3.00),
        ),
        (
            "per minute is 95.5 minutes",
            ac(Some(10.00), Some(9.00)),
            per_minute(Some(0.25), None),
            context(false, true),
            Some(23.88),
        ),
        (
            "per minute needs an observed span",
            ac(Some(10.00), Some(9.00)),
            per_minute(Some(0.25), None),
            context(false, false),
            None,
        ),
        (
            "per minute needs an end",
            open_ended,
            per_minute(Some(0.25), None),
            context(false, true),
            None,
        ),
        (
            "free Supercharger, tariff with a fee",
            kind(row(None, Some(9.00), Some(true)), "Supercharger"),
            per_kwh(Some(0.50), Some(2.00)),
            context(true, true),
            Some(0.00),
        ),
        (
            "free Supercharger, prefixed name, no tariff",
            kind(row(None, Some(9.00), Some(true)), "FastChargerSupercharger"),
            None,
            context(true, true),
            Some(0.00),
        ),
        (
            "a Combo site is not a Supercharger",
            kind(row(None, Some(9.00), Some(true)), "Combo"),
            per_kwh(Some(0.50), None),
            context(true, true),
            Some(4.50),
        ),
        (
            "Supercharger without the setting and without a tariff",
            kind(row(None, Some(9.00), Some(true)), "Supercharger"),
            None,
            context(false, true),
            None,
        ),
        (
            "rate missing",
            ac(Some(10.00), None),
            per_kwh(None, Some(1.00)),
            context(false, true),
            None,
        ),
        (
            "negative rate",
            ac(Some(10.00), None),
            per_kwh(Some(-0.05), None),
            context(false, true),
            None,
        ),
        (
            "negative fee",
            ac(Some(10.00), None),
            per_kwh(Some(0.30), Some(-1.00)),
            context(false, true),
            None,
        ),
        (
            "zero energy costs zero",
            ac(Some(0.00), None),
            per_kwh(Some(0.30), None),
            context(false, true),
            Some(0.00),
        ),
        (
            "zero energy costs the fee",
            ac(Some(0.00), None),
            per_kwh(Some(0.30), Some(1.50)),
            context(false, true),
            Some(1.50),
        ),
        (
            "waived copy",
            ac(Some(10.00), None),
            per_kwh(Some(0.30), Some(0.00)),
            context(false, true),
            Some(3.00),
        ),
    ];
    for (label, charge, tariff, context, want) in cases {
        let got = calculate_charge_cost(&charge, tariff, context);
        assert_eq!(got, want, "{label}");
    }
}

#[test]
fn r44_a_non_finite_rate_fee_or_energy_gives_no_cost() {
    let charge = ac(Some(10.00), None);
    for (label, tariff) in [
        ("NaN rate", per_kwh(Some(f64::NAN), None)),
        ("infinite rate", per_kwh(Some(f64::INFINITY), None)),
        ("NaN fee", per_kwh(Some(0.30), Some(f64::NAN))),
        ("infinite fee", per_kwh(Some(0.30), Some(f64::INFINITY))),
    ] {
        assert_eq!(
            calculate_charge_cost(&charge, tariff, context(false, true)),
            None,
            "{label}"
        );
    }
    for (label, used) in [
        ("NaN energy", f64::NAN),
        ("infinite energy", f64::INFINITY),
        ("negative energy", -1.0),
    ] {
        assert_eq!(
            calculate_charge_cost(
                &ac(Some(used), None),
                per_kwh(Some(0.30), Some(1.00)),
                context(false, true)
            ),
            None,
            "{label}"
        );
    }
}

#[test]
fn r44_the_basis_follows_the_kind_and_never_mixes_quantities() {
    // AC and unknown kind never fall back to the counter.
    for is_dc in [Some(false), None] {
        let charge = row(None, Some(9.00), is_dc);
        assert_eq!(
            calculate_charge_cost(&charge, per_kwh(Some(0.30), None), context(false, true)),
            None,
            "{is_dc:?}"
        );
    }
    // DC with neither energy is unknown.
    assert_eq!(
        calculate_charge_cost(
            &row(None, None, Some(true)),
            per_kwh(Some(0.30), None),
            context(false, true)
        ),
        None
    );
    // The per-minute basis ignores the energies and the kind.
    let dc = row(None, None, Some(true));
    assert_eq!(
        calculate_charge_cost(
            &dc,
            per_minute(Some(0.25), Some(1.00)),
            context(false, true)
        ),
        Some(24.88)
    );
}

#[test]
fn r43_the_per_minute_basis_is_the_unrounded_stored_span() {
    // 95.5 minutes, not the stored duration of 95: 95.5 * 0.25 = 23.875.
    let charge = ac(None, None);
    assert_eq!(charge.duration_min, Some(95));
    assert_eq!(
        calculate_charge_cost(&charge, per_minute(Some(0.25), None), context(false, true)),
        Some(23.88)
    );
    // An end before the start is a fault, so no cost.
    let mut backwards = ac(None, None);
    backwards.end_date_ms = Some(T0 - 1);
    assert_eq!(
        calculate_charge_cost(
            &backwards,
            per_minute(Some(0.25), None),
            context(false, true)
        ),
        None
    );
}

#[test]
fn r58_a_zero_minute_session_costs_the_fee() {
    let mut charge = ac(Some(0.00), Some(0.00));
    charge.end_date_ms = Some(T0);
    assert_eq!(
        calculate_charge_cost(
            &charge,
            per_minute(Some(0.25), Some(2.00)),
            context(false, true)
        ),
        Some(2.00)
    );
    assert_eq!(
        calculate_charge_cost(&charge, per_minute(Some(0.25), None), context(false, true)),
        Some(0.00)
    );
}

#[test]
fn r4_the_cost_is_rounded_once_and_negative_zero_is_zero() {
    // 0.125 * 0.3 = 0.0375: one rounding at the end gives 0.04.
    assert_eq!(
        calculate_charge_cost(
            &ac(Some(0.125), None),
            per_kwh(Some(0.30), None),
            context(false, true)
        ),
        Some(0.04)
    );
    // A zero result carries no sign.
    let zero = calculate_charge_cost(
        &ac(Some(0.00), None),
        per_kwh(Some(0.30), None),
        context(false, true),
    )
    .expect("zero cost");
    assert!(zero == 0.0 && zero.is_sign_positive());
}

#[test]
fn r55_a_computed_cost_outside_the_pack_range_is_none() {
    // The pack's `cost_e2` holds at most 999,999,999,999.99.
    let rate = per_kwh(Some(1.0), None);
    assert_eq!(
        calculate_charge_cost(
            &ac(Some(999_999_999_999.99), None),
            rate,
            context(false, true)
        ),
        Some(999_999_999_999.99)
    );
    assert_eq!(
        calculate_charge_cost(
            &ac(Some(1_000_000_000_000.0), None),
            rate,
            context(false, true)
        ),
        None
    );
    assert_eq!(
        calculate_charge_cost(&ac(Some(1e300), None), rate, context(false, true)),
        None
    );
    assert!(cost_fits_pack_scale(-999_999_999_999.99));
    assert!(!cost_fits_pack_scale(-1_000_000_000_000.0));
    assert!(!cost_fits_pack_scale(f64::NAN));
    assert!(!cost_fits_pack_scale(f64::INFINITY));
    assert!(cost_fits_pack_scale(0.0));
}

fn counter_sample(id: i64, offset_ms: i64, counter: Option<f64>) -> ProjectionChargeSample {
    ProjectionChargeSample {
        id,
        charge_process_id: 1,
        timestamp_ms: T0 + offset_ms,
        battery_level: None,
        usable_battery_level: None,
        charge_energy_added_kwh: counter,
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
        fast_charger_brand: None,
        fast_charger_type: None,
        charge_cable: None,
    }
}

fn counters(values: &[f64]) -> Vec<ProjectionChargeSample> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| counter_sample(index as i64 + 1, index as i64 * MIN_MS, Some(*value)))
        .collect()
}

fn added(kwh: Option<f64>) -> ProjectionCharge {
    ac(None, kwh)
}

#[test]
fn t30b_continuation_and_fee_waiver() {
    for (label, charge, samples, want) in [
        ("top-up", added(Some(1.00)), counters(&[20.50, 21.00]), true),
        ("one sample", added(Some(38.00)), counters(&[40.00]), true),
        (
            "restarted counter",
            added(Some(0.30)),
            counters(&[0.30]),
            false,
        ),
        (
            "whole counter",
            added(Some(20.00)),
            counters(&[0.50, 5.00, 10.00, 15.00, 20.00]),
            false,
        ),
        (
            "restart inside",
            added(Some(14.00)),
            counters(&[0.0, 5.0, 10.0, 0.30, 4.00]),
            false,
        ),
        (
            "energy unknown",
            added(None),
            counters(&[20.50, 21.00]),
            false,
        ),
    ] {
        assert_eq!(charge_continues_plug_in(&charge, &samples), want, "{label}");
    }
    // The counter moves in steps of 0.01, so one step above is a continuation
    // and equal values (float noise only) are not.
    assert!(charge_continues_plug_in(
        &added(Some(1.00)),
        &counters(&[1.01])
    ));
    assert!(!charge_continues_plug_in(
        &added(Some(1.00)),
        &counters(&[1.00])
    ));
    // Unusable counters are ignored, so only the usable maximum counts.
    let mut mixed = counters(&[20.50, 21.00]);
    mixed.push(counter_sample(9, 9 * MIN_MS, None));
    mixed.push(counter_sample(10, 10 * MIN_MS, Some(-5.0)));
    mixed.push(counter_sample(11, 11 * MIN_MS, Some(f64::NAN)));
    assert!(charge_continues_plug_in(&added(Some(1.00)), &mixed));
    assert!(!charge_continues_plug_in(
        &added(Some(1.00)),
        &[counter_sample(1, 0, None)]
    ));
    assert!(!charge_continues_plug_in(&added(Some(1.00)), &[]));
    assert!(!charge_continues_plug_in(
        &added(Some(f64::NAN)),
        &counters(&[5.0])
    ));

    let with_fee = ChargeTariff {
        billing_type: GeofenceBillingType::PerKwh,
        cost_per_unit: Some(0.30),
        session_fee: Some(1.00),
    };
    assert_eq!(
        with_fee.waive_fee(),
        ChargeTariff {
            session_fee: Some(0.00),
            ..with_fee
        }
    );
    let without_fee = ChargeTariff {
        session_fee: None,
        ..with_fee
    };
    assert_eq!(without_fee.waive_fee(), without_fee);
}

fn at_ms(id: i64, offset_ms: i64) -> ProjectionChargeSample {
    counter_sample(id, offset_ms, None)
}

fn spanned(end_offset_ms: Option<i64>) -> ProjectionCharge {
    let mut charge = ac(None, None);
    charge.end_date_ms = end_offset_ms.map(|offset| T0 + offset);
    charge
}

#[test]
fn t30c_observed_span() {
    let m5 = 5 * MIN_MS;
    let m10 = 10 * MIN_MS;
    let h3 = 3 * 60 * MIN_MS;
    for (label, charge, samples, want) in [
        (
            "ordinary polling",
            spanned(Some(m10)),
            vec![at_ms(1, 0), at_ms(2, m5), at_ms(3, m10)],
            true,
        ),
        (
            "first sample one millisecond past the bound",
            spanned(Some(K_INT_MAX_MS + 1)),
            vec![at_ms(1, K_INT_MAX_MS + 1)],
            false,
        ),
        (
            "first sample exactly at the bound",
            spanned(Some(K_INT_MAX_MS)),
            vec![at_ms(1, K_INT_MAX_MS)],
            true,
        ),
        (
            "closed by the first sample after a gap",
            spanned(Some(h3)),
            vec![at_ms(1, 0), at_ms(2, m5), at_ms(3, h3)],
            false,
        ),
        (
            "a gap in the middle that later samples close",
            spanned(Some(h3 + m5)),
            vec![at_ms(1, 0), at_ms(2, h3), at_ms(3, h3 + m5)],
            true,
        ),
        (
            "one sample at its own start",
            spanned(Some(0)),
            vec![at_ms(1, 0)],
            true,
        ),
        ("no samples", spanned(Some(h3)), vec![], true),
        (
            "end past the bound after the last sample",
            spanned(Some(m5 + K_INT_MAX_MS + 1)),
            vec![at_ms(1, 0), at_ms(2, m5)],
            false,
        ),
        (
            "end exactly at the bound after the last sample",
            spanned(Some(m5 + K_INT_MAX_MS)),
            vec![at_ms(1, 0), at_ms(2, m5)],
            true,
        ),
        (
            "an open row has no end to bound",
            spanned(None),
            vec![at_ms(1, 0), at_ms(2, m5)],
            true,
        ),
    ] {
        assert_eq!(charge_span_observed(&charge, &samples), want, "{label}");
    }
}

fn fence(
    billing_type: Option<GeofenceBillingType>,
    rate: Option<f64>,
    fee: Option<f64>,
) -> GeofenceFence {
    GeofenceFence {
        source_geofence_id: 3,
        name: "F3".to_owned(),
        latitude: 10.0,
        longitude: 20.0,
        radius_m: 150.0,
        billing_type,
        cost_per_unit: rate,
        session_fee: fee,
    }
}

#[test]
fn r40_a_fence_tariff_is_copied_only_when_complete_and_valid() {
    let kwh = Some(GeofenceBillingType::PerKwh);
    let minute = Some(GeofenceBillingType::PerMinute);
    let complete = ChargeTariff::of_fence(&fence(kwh, Some(0.30), Some(1.00))).expect("complete");
    assert_eq!(
        complete,
        ChargeTariff {
            billing_type: GeofenceBillingType::PerKwh,
            cost_per_unit: Some(0.30),
            session_fee: Some(1.00),
        }
    );
    assert_eq!(
        ChargeTariff::of_fence(&fence(minute, Some(0.0), None)),
        Some(ChargeTariff {
            billing_type: GeofenceBillingType::PerMinute,
            cost_per_unit: Some(0.0),
            session_fee: None,
        })
    );
    for (label, candidate) in [
        ("fee only, no type", fence(None, None, Some(2.00))),
        ("type without a rate", fence(kwh, None, None)),
        ("rate without a type", fence(None, Some(0.30), None)),
        ("negative rate", fence(kwh, Some(-0.10), None)),
        ("negative fee", fence(kwh, Some(0.30), Some(-1.00))),
        ("NaN rate", fence(kwh, Some(f64::NAN), None)),
        ("infinite rate", fence(kwh, Some(f64::INFINITY), None)),
        ("NaN fee", fence(kwh, Some(0.30), Some(f64::NAN))),
        ("infinite fee", fence(kwh, Some(0.30), Some(f64::INFINITY))),
    ] {
        assert_eq!(ChargeTariff::of_fence(&candidate), None, "{label}");
    }
}

#[test]
fn r40_the_row_tariff_is_present_iff_the_row_names_a_billing_type() {
    let mut charge = ac(Some(10.00), None);
    assert_eq!(ChargeTariff::of_charge(&charge), None);
    // A rate or fee alone is not a tariff.
    charge.cost_per_unit = Some(0.30);
    charge.session_fee = Some(1.00);
    assert_eq!(ChargeTariff::of_charge(&charge), None);
    charge.billing_type = Some(GeofenceBillingType::PerMinute);
    assert_eq!(
        ChargeTariff::of_charge(&charge),
        Some(ChargeTariff {
            billing_type: GeofenceBillingType::PerMinute,
            cost_per_unit: Some(0.30),
            session_fee: Some(1.00),
        })
    );
    // The row copy keeps its values as stored; a missing rate is judged by R44.
    charge.cost_per_unit = None;
    let tariff = ChargeTariff::of_charge(&charge);
    assert!(tariff.is_some());
    assert_eq!(
        calculate_charge_cost(&charge, tariff, context(false, true)),
        None
    );
}

/// C2 R43a, R44a (Codex R10): the close and the fill fold a charge's samples into
/// four facts one row at a time, in the order the store returns them, with no sorted
/// copy. The facts must give what the rule gives over the sorted list, whatever the
/// order and for ties of time, so the check below states the rule again over a list
/// it sorts itself and compares it with every rotation, the reversal and a number of
/// shuffles of the same samples.
#[test]
fn r43a_r44a_the_folded_facts_equal_the_rule_over_the_sorted_samples() {
    let m5 = 5 * MIN_MS;
    let h3 = 3 * 60 * MIN_MS;
    let samples = vec![
        counter_sample(1, 0, Some(1.0)),
        counter_sample(2, m5, None),
        // The same time as sample 2: the id breaks the tie.
        counter_sample(3, m5, Some(7.5)),
        counter_sample(4, 2 * m5, Some(f64::NAN)),
        counter_sample(5, h3, Some(3.0)),
        counter_sample(6, h3 + m5, Some(-4.0)),
        counter_sample(7, h3 + 2 * m5, Some(9.25)),
    ];
    let reference = |charge: &ProjectionCharge, samples: &[ProjectionChargeSample]| {
        let mut sorted = samples.to_vec();
        sorted.sort_by_key(|sample| (sample.timestamp_ms, sample.id));
        let span = match (sorted.first(), sorted.last()) {
            (Some(first), Some(last)) => {
                let gap_before_last = sorted
                    .len()
                    .checked_sub(2)
                    .map(|index| last.timestamp_ms - sorted[index].timestamp_ms);
                !(first.timestamp_ms - charge.start_date_ms > K_INT_MAX_MS
                    || gap_before_last.is_some_and(|gap| gap > K_INT_MAX_MS)
                    || charge
                        .end_date_ms
                        .is_some_and(|end| end - last.timestamp_ms > K_INT_MAX_MS))
            }
            _ => true,
        };
        let largest = sorted
            .iter()
            .filter_map(|sample| sample.charge_energy_added_kwh)
            .filter(|counter| counter.is_finite() && *counter >= 0.0)
            .fold(None::<f64>, |best, counter| {
                Some(best.map_or(counter, |best| best.max(counter)))
            });
        let continues = charge
            .charge_energy_added
            .filter(|added| added.is_finite())
            .is_some_and(|added| largest.is_some_and(|m| m - added >= K_ENERGY_EPS_KWH));
        (span, continues)
    };
    let mut orders: Vec<Vec<ProjectionChargeSample>> = Vec::new();
    for rotation in 0..samples.len() {
        let mut rotated = samples.clone();
        rotated.rotate_left(rotation);
        orders.push(rotated);
    }
    let mut reversed = samples.clone();
    reversed.reverse();
    orders.push(reversed);
    let mut state = 0x2545_f491_u64;
    for _ in 0..40 {
        let mut shuffled = samples.clone();
        for index in (1..shuffled.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            shuffled.swap(index, (state % (index as u64 + 1)) as usize);
        }
        orders.push(shuffled);
    }
    // Prefixes too: a two-sample, a one-sample and an empty list.
    orders.push(samples[..2].to_vec());
    orders.push(samples[..1].to_vec());
    orders.push(Vec::new());
    for charge in [
        spanned(Some(h3 + 2 * m5)),
        spanned(Some(h3 + 2 * m5 + K_INT_MAX_MS + 1)),
        spanned(Some(h3 + 2 * m5 + K_INT_MAX_MS)),
        spanned(None),
        {
            let mut charge = spanned(Some(h3 + 2 * m5));
            charge.start_date_ms = T0 - K_INT_MAX_MS - 1;
            charge
        },
        {
            let mut charge = spanned(Some(h3 + 2 * m5));
            charge.charge_energy_added = Some(9.0);
            charge
        },
        {
            let mut charge = spanned(Some(h3 + 2 * m5));
            charge.charge_energy_added = Some(f64::NAN);
            charge
        },
    ] {
        for order in &orders {
            let facts = ChargeSampleFacts::of(order);
            assert_eq!(
                (
                    charge_span_observed_from(&charge, &facts),
                    charge_continues_plug_in_from(&charge, &facts)
                ),
                reference(&charge, order),
                "{} samples, end {:?}",
                order.len(),
                charge.end_date_ms
            );
        }
    }
}
