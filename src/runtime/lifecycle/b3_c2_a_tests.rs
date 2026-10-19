// SPDX-License-Identifier: AGPL-3.0-only

//! C2-A spec tests (C2 section 18.1, C2 R76 and its tests C2 T78 and C2 T83 (a) to (c)):
//! the one restart test for Tesla's counter, the counter fold that keeps the
//! maximum through a dip, the signed interval that a dip contributes to the
//! ratio, and the energy used from the net change after a final decline.
//! Synthetic data only. `T0` is 1_760_000_000_000 ms. The gate-level tests
//! (C2 T79, C2 T83 (d), C2 T84) are in `gate_tests/c2_a_tests.rs` and the store-level
//! test (C2 T49 (d)) in `src/storage/db/tests/b3_c2_a.rs`.
//!
//! Every fold case runs the slice fold (`charge_energy_from_samples`) and the
//! one-sample observer (`observe_charge_sample`) and requires the same totals.
//! Energies are compared after `round2` (the totals already are), sums with an
//! absolute tolerance of 1e-9.

use super::*;

const T0: i64 = 1_760_000_000_000;
const MIN: i64 = 60_000;
const TOLERANCE: f64 = 1e-9;

/// `(P, V, I, r)` of one electrical reading.
type Electrical = (f64, f64, f64, i64);

/// The spec's AC sample `A7`: 7 kW at 240 V, 30 A, one phase (C2 R35 row 1: 7.2 kW).
const A7: Electrical = (7.0, 240.0, 30.0, 1);

/// `{P 8, V 230, I 12, r 3}`: 8.28 kW by C2 R35 row 1.
const AC8: Electrical = (8.0, 230.0, 12.0, 3);

fn sample(
    id: i64,
    at_ms: i64,
    counter: Option<f64>,
    power: Option<Electrical>,
) -> ProjectionChargeSample {
    ProjectionChargeSample {
        id,
        charge_process_id: 1,
        timestamp_ms: at_ms,
        battery_level: None,
        usable_battery_level: None,
        charge_energy_added_kwh: counter,
        charger_power_kw: power.map(|(p, _, _, _)| p),
        charger_voltage: power.map(|(_, v, _, _)| v),
        charger_actual_current: power.map(|(_, _, i, _)| i),
        charger_pilot_current: None,
        charger_phases: power.map(|(_, _, _, r)| r),
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

/// The samples `step_ms` apart from `T0` with the counters given and the same
/// electrical reading on each.
fn series(
    step_ms: i64,
    power: Option<Electrical>,
    counters: &[f64],
) -> Vec<ProjectionChargeSample> {
    counters
        .iter()
        .enumerate()
        .map(|(k, counter)| sample(k as i64 + 1, T0 + k as i64 * step_ms, Some(*counter), power))
        .collect()
}

fn close(got: Option<f64>, want: Option<f64>, label: &str) {
    match (got, want) {
        (Some(got), Some(want)) => assert!(
            (got - want).abs() < TOLERANCE,
            "{label}: got {got}, want {want}"
        ),
        (got, want) => assert_eq!(got, want, "{label}"),
    }
}

/// The incremental fold of `samples` over a default charge with `baseline`.
fn fold(samples: &[ProjectionChargeSample], baseline: Option<f64>) -> OpenCharge {
    let mut open = OpenCharge::empty(1, 1, T0);
    open.first_energy_added = baseline;
    for sample in samples {
        observe_charge_sample(&mut open, sample);
    }
    open
}

/// Assert the slice fold and the one-sample observer both give the stated
/// totals, and return the open charge for further checks.
fn check(
    samples: &[ProjectionChargeSample],
    baseline: Option<f64>,
    added: Option<f64>,
    used: Option<f64>,
    label: &str,
) -> OpenCharge {
    let slice = charge_energy_from_samples(samples, baseline);
    close(slice.added_kwh, added, &format!("{label}: slice added"));
    close(slice.used_kwh, used, &format!("{label}: slice used"));
    let open = fold(samples, baseline);
    assert_eq!(
        charge_energy_totals(&open),
        slice,
        "{label}: observer equals slice"
    );
    open
}

#[test]
fn c2_t78_counter_restart_boundaries() {
    // (a) The restart test: a fall beyond K_COUNTER_DIP_KWH + K_ENERGY_EPS_KWH.
    for (previous, current, restarted) in [
        (10.00, 10.00, false),
        (10.00, 9.98, false),
        (10.00, 9.95, false),
        (10.00, 9.94, true),
        (0.05, 0.00, false),
        (0.06, 0.00, true),
        (50.86, 50.84, false),
        (26.72, 26.70, false),
        (20.00, 0.30, true),
        (9.98, 10.00, false),
    ] {
        assert_eq!(
            counter_restarted(previous, current),
            restarted,
            "counter_restarted({previous}, {current})"
        );
    }
    // The tolerance is 0.05 kWh and the epsilon is half a counter step: two
    // stored values five counter steps apart are a dip and six apart a restart,
    // whatever their binary representation (the epsilon term removed would make
    // 10.00 against 9.95 a restart for some representations).
    assert_eq!(K_COUNTER_DIP_KWH, 0.05);
    assert_eq!(K_ENERGY_EPS_KWH, 0.005);
    for steps in 0..=5 {
        let previous = 12.34;
        let current = previous - steps as f64 * 0.01;
        assert!(!counter_restarted(previous, current), "{steps} steps");
    }
    for steps in 6..=40 {
        let previous = 12.34;
        let current = previous - steps as f64 * 0.01;
        assert!(counter_restarted(previous, current), "{steps} steps");
    }
    // A rise is never a restart, and a restart needs a finite fall.
    assert!(!counter_restarted(0.0, 1.0e6));
    assert!(!counter_restarted(f64::NAN, 1.0));
    assert!(!counter_restarted(1.0, f64::NAN));
}

#[test]
fn c2_t78_dip_and_restart_folds() {
    // (b) The measured recovered dip at the AC cadence: baseline 50.70, eight
    // samples 20 s apart at 8.28 kW, c 50.70, 50.74, 50.79, 50.83, 50.86, 50.84,
    // 50.88, 50.93. The dip is not a second session: added 0.23, no carry, every
    // interval counted. (Revision 5 gave 51.09 and left the dip's interval out.)
    let b = series(
        20_000,
        Some(AC8),
        &[50.70, 50.74, 50.79, 50.83, 50.86, 50.84, 50.88, 50.93],
    );
    let open = check(&b, Some(50.70), Some(0.23), Some(0.32), "T78 (b)");
    assert_eq!(open.energy_added_carry_kwh, 0.0);
    assert_eq!(open.max_energy_added, Some(50.93));
    assert_eq!(open.last_energy_added, Some(50.93));
    close(open.energy_used_kwh, Some(0.322), "T78 (b) U_p");
    close(Some(open.ratio_used_kwh), Some(0.322), "T78 (b) U_r");
    close(Some(open.ratio_added_kwh), Some(0.23), "T78 (b) A_r");
    assert_eq!(open.is_dc, Some(false));

    // (c) Dips that stay lower, and a dip at the last counter: c 0.00, 2.50, 5.00,
    // 4.98, 4.98, 7.50, 7.48 at 10-minute steps with no power. The energy is the
    // maximum, the last counter is the dip, and the energy used is unknown.
    // (Revision 5 gave 19.98.)
    let c = series(10 * MIN, None, &[0.00, 2.50, 5.00, 4.98, 4.98, 7.50, 7.48]);
    let open = check(&c, None, Some(7.50), None, "T78 (c)");
    assert_eq!(open.energy_added_carry_kwh, 0.0);
    assert_eq!(open.max_energy_added, Some(7.50));
    assert_eq!(open.last_energy_added, Some(7.48));

    // (d) The bound inside a fold: a fall of 0.05 is a dip, one of 0.06 a restart.
    let d = series(10 * MIN, None, &[10.00, 9.95, 12.00]);
    let open = check(&d, None, Some(12.00), None, "T78 (d) dip");
    assert_eq!(open.energy_added_carry_kwh, 0.0);
    let d = series(10 * MIN, None, &[10.00, 9.94, 12.00]);
    let open = check(&d, None, Some(22.00), None, "T78 (d) restart");
    assert_eq!(open.energy_added_carry_kwh, 10.00);

    // (e) Successive dips: each fall is 0.03 against the previous counter; against
    // the maximum the third reading would be a restart and give 22.00.
    let e = series(10 * MIN, None, &[10.00, 9.97, 9.94, 12.00]);
    let open = check(&e, None, Some(12.00), None, "T78 (e)");
    assert_eq!(open.energy_added_carry_kwh, 0.0);
    assert_eq!(open.max_energy_added, Some(12.00));

    // (f) The first counter against a preset: a dip below it keeps the preset
    // and gives no negative energy before the first sample; a restart against it
    // lowers the preset to zero (C2 R9 S6 (a)).
    let f = series(10 * MIN, None, &[19.98, 20.50, 21.00]);
    let open = check(&f, Some(20.00), Some(1.00), None, "T78 (f) dip");
    assert_eq!(open.first_energy_added, Some(20.00));
    assert_eq!(open.pre_sample_kwh, Some(0.0));
    let f = series(10 * MIN, None, &[19.94, 21.00]);
    let open = check(&f, Some(20.00), Some(21.00), None, "T78 (f) restart");
    assert_eq!(open.first_energy_added, Some(0.0));
    // A first counter equal to the preset is no fall at all.
    let f = series(10 * MIN, None, &[20.00, 20.50]);
    let open = check(&f, Some(20.00), Some(0.50), None, "T78 (f) equal");
    assert_eq!(open.pre_sample_kwh, Some(0.0));

    // (g) A counted dip interval: A7 samples 10 minutes apart with c 0.40, 1.48,
    // 1.46, 2.56, 3.64. The dip's interval is counted with its signed change, so
    // A_r is the net change over the counted intervals (3.24) and the energy used
    // is 3.64 x 4.80 / 3.24 = 5.39. Leaving the dip's interval out (U_r 3.60, A_r
    // 3.26) would give 4.02.
    let g = series(10 * MIN, Some(A7), &[0.40, 1.48, 1.46, 2.56, 3.64]);
    let open = check(&g, None, Some(3.64), Some(5.39), "T78 (g)");
    close(open.energy_used_kwh, Some(4.80), "T78 (g) U_p");
    close(Some(open.ratio_used_kwh), Some(4.80), "T78 (g) U_r");
    close(Some(open.ratio_added_kwh), Some(3.24), "T78 (g) A_r");
    assert_eq!(open.last_energy_added, Some(3.64));
    // A restart interval is still left out of the ratio (C2 R18): the energy so far
    // goes into the carry and the new counter is all new energy.
    let restart = series(10 * MIN, Some(A7), &[0.40, 1.48, 2.56, 0.30, 1.38]);
    let open = check(&restart, None, Some(3.94), Some(4.38), "restart");
    assert_eq!(open.energy_added_carry_kwh, 2.56);
    close(Some(open.ratio_added_kwh), Some(3.24), "restart A_r");
    close(Some(open.ratio_used_kwh), Some(3.60), "restart U_r");

    // The one-sample observer keeps the same state at every step: folding the
    // first n samples and finishing equals the slice of those n samples.
    for samples in [&b, &c, &d, &e, &f, &g, &restart] {
        for count in 1..=samples.len() {
            let slice = charge_energy_from_samples(&samples[..count], None);
            let open = fold(&samples[..count], None);
            assert_eq!(charge_energy_totals(&open), slice, "prefix {count}");
        }
    }
}

#[test]
fn c2_t83_net_energy_after_decline() {
    // (a) Row 4 after a final decline: A7 samples at t0, +10m, +20m, +30m and
    // +40m with c 0.40, 1.48, 2.56, 3.64, 3.60. The energy added keeps the
    // maximum 3.64, the last counter is 3.60, all four intervals are counted
    // (the last with -0.04), and the factor is the net change: 3.60 x 4.80 /
    // 3.20 = 5.40. (The factor `E` would give 5.46.)
    let a = series(10 * MIN, Some(A7), &[0.40, 1.48, 2.56, 3.64, 3.60]);
    let open = check(&a, None, Some(3.64), Some(5.40), "T83 (a)");
    assert_eq!(open.max_energy_added, Some(3.64));
    assert_eq!(open.last_energy_added, Some(3.60));
    close(open.energy_used_kwh, Some(4.80), "T83 (a) U_p");
    close(Some(open.ratio_used_kwh), Some(4.80), "T83 (a) U_r");
    close(Some(open.ratio_added_kwh), Some(3.20), "T83 (a) A_r");

    // (b) No final decline: the last counter is the maximum, `E_n = E`, and
    // C2 T78 (g) keeps its used 5.39.
    let b = series(10 * MIN, Some(A7), &[0.40, 1.48, 1.46, 2.56, 3.64]);
    let open = check(&b, None, Some(3.64), Some(5.39), "T83 (b)");
    assert_eq!(open.last_energy_added, open.max_energy_added);

    // (c) Net change not positive: baseline 20.00; P 7 at t0 and +10m with c
    // 20.00 and 21.08, then 22 samples without power at 10-minute steps with c
    // 21.03, 20.98, ..., 19.98 (each 0.05 below the previous, so every fall is a
    // dip). The kind stays unknown, added is 1.08 and A_r 1.08, but the net
    // change 19.98 - 20.00 is -0.02, so row 4 has no factor to scale and the
    // energy used is unknown. (The factor `E` would give 1.17, and without the
    // positive-net condition the value would be -0.02.)
    let powered = |id: i64, at_ms: i64, counter: f64| {
        let mut sample = sample(id, at_ms, Some(counter), None);
        sample.charger_power_kw = Some(7.0);
        sample
    };
    let mut c = vec![powered(1, T0, 20.00), powered(2, T0 + 10 * MIN, 21.08)];
    for k in 0..22 {
        let counter = f64::from(2103 - 5 * k) / 100.0;
        c.push(sample(
            i64::from(k) + 3,
            T0 + (i64::from(k) + 2) * 10 * MIN,
            Some(counter),
            None,
        ));
    }
    assert_eq!(c.last().unwrap().charge_energy_added_kwh, Some(19.98));
    let open = check(&c, Some(20.00), Some(1.08), None, "T83 (c)");
    assert_eq!(open.is_dc, None);
    assert_eq!(open.max_energy_added, Some(21.08));
    assert_eq!(open.last_energy_added, Some(19.98));
    close(Some(open.ratio_added_kwh), Some(1.08), "T83 (c) A_r");
    let net = open.energy_added_carry_kwh
        + (open.last_energy_added.unwrap() - open.first_energy_added.unwrap());
    assert!((net - -0.02).abs() < TOLERANCE, "net change {net}");

    // The same session without the final decline has a positive net change and
    // a value: the factor is the net change, here equal to the energy added, so
    // the figure is the one `E` would give (1.08 x 7 kW x 10 min / 1.08).
    let up = vec![
        powered(1, T0, 20.00),
        powered(2, T0 + 10 * MIN, 21.08),
        sample(3, T0 + 20 * MIN, Some(21.08), None),
    ];
    let open = check(&up, Some(20.00), Some(1.08), Some(1.17), "T83 (c) control");
    assert!(open.ratio_added_kwh >= 1.0);
}
