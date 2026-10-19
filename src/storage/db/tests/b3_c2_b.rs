// SPDX-License-Identifier: AGPL-3.0-only

//! C2-B store-level tests (C2 section 18.1, C2 T81 (f) as revision 11 states it,
//! C2 T80's manual-entry case): the close (C2 R46) and the retroactive fill
//! (C2 R50) fold the brand evidence of every sample of a charge, whether the
//! sample sits in the stored open rows, in the closing delta or in both, and
//! price a Tesla-brand DC session of another DC type as free Supercharging
//! (C2 R78 (c)); a session that also carries another brand is paid; and a DC
//! session with no usable counter has no per-kWh cost (C2 R77). Synthetic data
//! only; the sequences are built with the spec's `Ob` and the store harness of
//! `cost_close.rs`.

use super::*;

const SAMPLE_GAP_MS: i64 = 2 * COST_MIN;

/// One DC observation of the stated brand: `{P 120, V 2, I 0,
/// fast_charger_present true, fast_charger_type "Combo"}` at counter `counter`.
fn dc_observation(
    id: i64,
    counter: f64,
    brand: Option<&str>,
    state: &str,
    coordinates: bool,
) -> crate::lifecycle::LifecycleSample {
    let mut fields = serde_json::json!({
        "charge_energy_added": counter, "charger_power": 120,
        "charger_voltage": 2, "charger_actual_current": 0,
        "fast_charger_present": true, "fast_charger_type": "Combo",
    });
    if let Some(brand) = brand {
        fields["fast_charger_brand"] = serde_json::json!(brand);
    }
    let observation = cost_ob(id, COST_T0 + (id - 1) * SAMPLE_GAP_MS, state, fields);
    if coordinates {
        cost_at(observation, 10.0005, 20.0)
    } else {
        observation
    }
}

/// Where the closing sample (counter 8.00) travels to the store.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Closing {
    /// As an open row, the way the collector commits it: every sample of the
    /// charge is then a stored open row when the close folds.
    StoredRow,
    /// In the delta's own `charge_samples` only, as a pure step's buffer is
    /// forwarded: the earlier two samples are stored rows, this one is carried.
    DeltaOnly,
    /// In both: the close must offer it once.
    StoredAndDelta,
}

/// Three DC samples 2 min apart at counters 0.00, 4.00 and 8.00 with the given
/// brands, committed one observation at a time (the last is `Complete` and
/// closes the charge). With `fence` a fence 3 around the start coordinates
/// exists, holding the given per-kWh tariff or none.
fn close_session(
    brands: [Option<&str>; 3],
    free_at_close: bool,
    fence: Option<Option<f64>>,
    closing: Closing,
) -> CostHarness {
    let mut harness = CostHarness::new();
    match fence {
        Some(Some(rate)) => harness.fence(3, "F3", 10.0, kwh(), rate, None),
        Some(None) => harness.imported_fence(3, 10.0, None, None, None),
        None => {}
    }
    if free_at_close {
        harness.set_free_supercharging(1);
    }
    let coordinates = fence.is_some();
    harness.feed(&dc_observation(1, 0.00, brands[0], "Charging", coordinates));
    harness.feed(&dc_observation(2, 4.00, brands[1], "Charging", false));
    let last = dc_observation(3, 8.00, brands[2], "Complete", false);
    let mut step =
        crate::lifecycle::apply_sample(harness.state.clone(), 1, &last).expect("closing step");
    match closing {
        Closing::StoredRow => {}
        Closing::DeltaOnly => {
            let carried = std::mem::take(&mut step.delta.open_charge_samples);
            step.delta.charge_samples.extend(carried);
        }
        Closing::StoredAndDelta => {
            let carried = step.delta.open_charge_samples.clone();
            step.delta.charge_samples.extend(carried);
        }
    }
    harness
        .commit_step(&step, 3, last.observed_at_ms)
        .expect("closing commit");
    harness.state = step.state;
    harness
}

/// The charge's materialised samples, in id order.
fn stored_samples(harness: &CostHarness) -> Vec<crate::hub_pack::ProjectionChargeSample> {
    let mut samples = harness
        .store
        .materialised_history(harness.vehicle.vehicle_id)
        .expect("history")
        .charge_samples;
    samples.sort_by_key(|sample| sample.id);
    samples
}

/// Each of the three samples is materialised once, with no duplicate id, and
/// the production fold over them reads `expected_tesla` as the brand evidence
/// (stated per case, not derived from the samples) and agrees with the rule
/// over the slice.
fn assert_three_samples_once(harness: &CostHarness, expected_tesla: bool) {
    let samples = stored_samples(harness);
    let ids = samples.iter().map(|sample| sample.id).collect::<Vec<_>>();
    assert_eq!(ids.len(), 3, "three samples: {ids:?}");
    let mut distinct = ids.clone();
    distinct.dedup();
    assert_eq!(distinct, ids, "no duplicate sample id");
    let counters = samples
        .iter()
        .map(|sample| sample.charge_energy_added_kwh)
        .collect::<Vec<_>>();
    assert_eq!(counters, vec![Some(0.00), Some(4.00), Some(8.00)]);
    let mut facts = crate::lifecycle::ChargeSampleFacts::default();
    for sample in &samples {
        facts.observe(sample);
    }
    assert_eq!(
        facts.brand_is_tesla(),
        expected_tesla,
        "the folded brand evidence of the case"
    );
    assert_eq!(
        facts.brand_is_tesla(),
        crate::lifecycle::charge_brand_is_tesla(&samples),
        "the folded facts equal the slice rule"
    );
}

/// With the fence's tariff set and the setting on after the close, the
/// requested missing-cost fill prices the row; a second fill changes nothing.
fn enable_and_fill(harness: &CostHarness, rate: f64) -> (u64, u64) {
    harness.set_free_supercharging(1);
    harness.fence(3, "F3", 10.0, kwh(), rate, None);
    let vehicle = harness.vehicle.vehicle_id;
    let first = harness
        .store
        .recalculate_missing_charge_costs(vehicle, 3)
        .expect("fill");
    let second = harness
        .store
        .recalculate_missing_charge_costs(vehicle, 3)
        .expect("second fill");
    (first, second)
}

#[test]
fn c2_t81_close_and_fill_brand_all_samples() {
    let tesla = Some("Tesla");
    let invalid = Some("<invalid>");
    let layouts = [
        Closing::StoredRow,
        Closing::DeltaOnly,
        Closing::StoredAndDelta,
    ];
    for closing in layouts {
        let label = format!("{closing:?}");
        // Every sample Tesla, no start coordinates, setting on: free, no copy.
        let all_tesla = close_session([tesla; 3], true, None, closing);
        let charge = all_tesla.charge(all_tesla.charges()[0].id);
        assert_eq!(charge.cost, Some(0.00), "{label}: every brand Tesla");
        assert_eq!(
            (
                charge.billing_type,
                charge.cost_per_unit,
                charge.session_fee
            ),
            (None, None, None),
            "{label}"
        );
        assert_three_samples_once(&all_tesla, true);
        // The same with every brand `<invalid>`: the operator is unknown.
        let unknown = close_session([invalid; 3], true, None, closing);
        assert_eq!(
            unknown.charges()[0].cost,
            None,
            "{label}: every brand invalid"
        );
        assert_three_samples_once(&unknown, false);

        // Case 1: Tesla only in the earliest stored sample (c 0.00); case 2:
        // only in the closing sample (c 8.00). The others carry no brand.
        for (case, brands) in [
            ("all Tesla", [tesla; 3]),
            ("case 1", [tesla, None, None]),
            ("case 2", [None, None, tesla]),
        ] {
            let at_close = close_session(brands, true, None, closing);
            assert_eq!(
                at_close.charges()[0].cost,
                Some(0.00),
                "{label} {case}: close"
            );
            assert_three_samples_once(&at_close, true);

            // The same with start coordinates in fence 3 holding no tariff and
            // the setting off at close: cost None; then the setting on and the
            // fence priced at 0.30 with no fee: the fill returns 1, writes 0.00
            // and the tariff copy; a second fill returns 0.
            let pending = close_session(brands, false, Some(None), closing);
            assert_eq!(pending.charges()[0].cost, None, "{label} {case}: pending");
            let (first, second) = enable_and_fill(&pending, 0.30);
            assert_eq!((first, second), (1, 0), "{label} {case}: fills");
            let filled = pending.charges()[0].clone();
            assert_eq!(filled.cost, Some(0.00), "{label} {case}: filled");
            assert_eq!(filled.billing_type, Some(kwh()));
            assert_eq!(filled.cost_per_unit, Some(0.30));
            assert_eq!(filled.session_fee, None);
            assert_three_samples_once(&pending, true);
        }
    }
}

#[test]
fn c2_t81_conflicting_brands() {
    let tesla = Some("Tesla");
    let other = Some("BrandX");
    for closing in [
        Closing::StoredRow,
        Closing::DeltaOnly,
        Closing::StoredAndDelta,
    ] {
        // Case 3: Tesla at c 0.00 and BrandX in the closing sample; case 4 the
        // reverse. The middle sample (c 4.00) is unbranded. Another operator's
        // brand anywhere makes the session paid, whichever sample carries it.
        for (case, brands) in [
            ("case 3", [tesla, None, other]),
            ("case 4", [other, None, tesla]),
        ] {
            let label = format!("{closing:?} {case}");
            // Tariff per_kwh 0.30, no fee, setting on at close: paid 8.00 x 0.30.
            let paid = close_session(brands, true, Some(Some(0.30)), closing);
            let charge = paid.charges()[0].clone();
            assert_eq!(charge.cost, Some(2.40), "{label}: close");
            assert_eq!(charge.billing_type, Some(kwh()), "{label}");
            assert_eq!(charge.cost_per_unit, Some(0.30), "{label}");
            assert_eq!(charge.session_fee, None, "{label}");
            assert_three_samples_once(&paid, false);

            // No tariff and the setting off at close: cost None; then the
            // setting on and the tariff set: the fill returns 1, writes the
            // paid cost and that tariff copy; a second fill returns 0.
            let pending = close_session(brands, false, Some(None), closing);
            assert_eq!(pending.charges()[0].cost, None, "{label}: pending");
            let (first, second) = enable_and_fill(&pending, 0.30);
            assert_eq!((first, second), (1, 0), "{label}: fills");
            let filled = pending.charges()[0].clone();
            assert_eq!(filled.cost, Some(2.40), "{label}: filled");
            assert_eq!(filled.billing_type, Some(kwh()), "{label}");
            assert_eq!(filled.cost_per_unit, Some(0.30), "{label}");
            assert_eq!(filled.session_fee, None, "{label}");
            assert_three_samples_once(&pending, false);
        }
    }
}

#[test]
fn c2_t80_manual_per_kwh_dc_without_counter() {
    // C2 T80: `set-charge-cost --mode per_kwh --cost 0.25` on a materialised DC
    // row with added None and used 8.00 gives `ChargeCostBasisUnavailable`, and
    // the row is unchanged (no cost, no copy, no sync mutation). The same rate
    // on a DC row with a counter prices the counter.
    let harness = CostHarness::new();
    let mut no_counter = cost_row(1, Some(8.00), None, Some(true), Some("Combo"));
    no_counter.start_date_ms = COST_T0;
    store_charge_row(&harness, &no_counter);
    let before = harness.charge(1);
    let mutations = harness.charge_mutations(1);
    let error = harness
        .store
        .set_charge_cost_rate(harness.vehicle.vehicle_id, 1, 0.25, kwh())
        .expect_err("a DC row with no counter has no per-kWh basis");
    assert!(
        matches!(
            error,
            StoreError::ChargeCostBasisUnavailable {
                charge_id: 1,
                mode: "per_kwh"
            }
        ),
        "{error:?}"
    );
    assert_eq!(harness.charge(1), before, "the row is unchanged");
    assert_eq!(harness.charge_mutations(1), mutations);

    let with_counter = cost_row(2, Some(8.56), Some(8.00), Some(true), Some("Combo"));
    store_charge_row(&harness, &with_counter);
    let priced = harness
        .store
        .set_charge_cost_rate(harness.vehicle.vehicle_id, 2, 0.50, kwh())
        .expect("a counter prices the row");
    assert_eq!(priced.cost, Some(4.00));
}
