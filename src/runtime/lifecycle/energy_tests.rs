// SPDX-License-Identifier: AGPL-3.0-only

//! Part C2-P2 spec tests (C2 specification section 10, T1 to T21, with T3,
//! T12b, T17b and T17c) and the rules of R2a, R10 to R27 that no numbered test
//! pins: counter validity, restart intervals, the finish rows, old blobs,
//! seeding by fold and the display aggregates. Synthetic data only. `T0` is
//! 1_760_000_000_000 ms.
//!
//! `b(id, minutes)` builds one `ProjectionChargeSample` whose unspecified
//! fields are `None` (the spec's `S(t, {...})`); `.c`, `.pvir` and the other
//! chained setters fill fields. `a7` is the spec's AC sample `A7`: 7 kW
//! reported at 240 V and 30 A on one phase, which R35 row 1 reads as 7.2 kW.
//! Fold tests run both the slice fold and the one-sample-at-a-time observer
//! and require the same totals. Energies and costs are compared after `round2`
//! (the totals already are), intermediate powers with an absolute tolerance of
//! 1e-9. Lifecycle tests use `ob`, the spec's `Ob(...)`: an owner
//! `vehicle_data` whose odometer is 621.37 miles (the car is unmoved, C1 R65).

use serde_json::{Map, json};

use super::charge_tests::{HOUR, MIN};
use super::*;

const T0: i64 = 1_760_000_000_000;
const TOLERANCE: f64 = 1e-9;

/// A sample builder over `ProjectionChargeSample`.
#[derive(Clone)]
struct B(ProjectionChargeSample);

fn b_ms(id: i64, at_ms: i64) -> B {
    B(ProjectionChargeSample {
        id,
        charge_process_id: 1,
        timestamp_ms: at_ms,
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
        fast_charger_brand: None,
        fast_charger_type: None,
        charge_cable: None,
    })
}

/// A sample `minutes` after `t0`; the id is `minutes + 1`, unique in a series
/// whose minutes differ.
fn b(minutes: i64) -> B {
    b_ms(minutes + 1, T0 + minutes * MIN)
}

impl B {
    fn c(mut self, kwh: f64) -> Self {
        self.0.charge_energy_added_kwh = Some(kwh);
        self
    }
    fn power(mut self, kw: f64) -> Self {
        self.0.charger_power_kw = Some(kw);
        self
    }
    fn volts(mut self, volts: f64) -> Self {
        self.0.charger_voltage = Some(volts);
        self
    }
    fn amps(mut self, amps: f64) -> Self {
        self.0.charger_actual_current = Some(amps);
        self
    }
    fn phases(mut self, phases: i64) -> Self {
        self.0.charger_phases = Some(phases);
        self
    }
    fn present(mut self, present: bool) -> Self {
        self.0.fast_charger_present = Some(present);
        self
    }
    fn kind(mut self, text: &str) -> Self {
        self.0.fast_charger_type = Some(text.to_owned());
        self
    }
    /// The AC sample `A7`: 7 kW at 240 V, 30 A, one phase.
    fn a7(self) -> Self {
        self.power(7.0).volts(240.0).amps(30.0).phases(1)
    }
    fn s(self) -> ProjectionChargeSample {
        self.0
    }
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

/// Assert the slice fold and the incremental observer both give the stated
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
    let incremental = charge_energy_totals(&open);
    assert_eq!(incremental, slice, "{label}: incremental equals slice");
    open
}

// ------------------------------------------------------------- energy added

#[test]
fn t1_basic_counter() {
    let samples = [
        b(0).c(0.00).s(),
        b(2).c(2.50).s(),
        b(4).c(5.00).s(),
        b(6).c(7.50).s(),
    ];
    // No power at all: the integral is unknown, so the energy used is None.
    check(&samples, None, Some(7.50), None, "T1");
}

#[test]
fn t2_counter_already_running_without_a_snapshot() {
    let samples = [b(0).c(0.40).s(), b(2).c(2.90).s()];
    let open = check(&samples, None, Some(2.90), None, "T2");
    // With no baseline and no earlier value the baseline is zero (R11).
    assert_eq!(open.first_energy_added, Some(0.0));
    assert_eq!(open.max_energy_added, Some(2.90));
    assert_eq!(open.last_energy_added, Some(2.90));
}

#[test]
fn t4_top_up_in_the_same_plug_in() {
    let samples = [b(0).c(20.50).s(), b(2).c(21.00).s()];
    check(&samples, Some(20.00), Some(1.00), None, "T4");
}

#[test]
fn t5_a_lower_first_counter_than_the_preset_lowers_the_baseline() {
    let samples = [b(0).c(0.30).s(), b(2).c(1.30).s()];
    let open = check(&samples, Some(20.00), Some(1.30), None, "T5");
    assert_eq!(open.first_energy_added, Some(0.0));
}

#[test]
fn t6_an_imported_restart_adds_the_finished_segment() {
    let samples = [
        b(0).c(0.00).s(),
        b(1).c(5.00).s(),
        b(2).c(10.00).s(),
        b(3).c(0.30).s(),
        b(4).c(4.00).s(),
    ];
    let open = check(&samples, None, Some(14.00), None, "T6");
    assert_eq!(open.energy_added_carry_kwh, 10.0);
    assert_eq!(open.first_energy_added, Some(0.0));
    assert_eq!(open.max_energy_added, Some(4.0));
    assert_eq!(open.last_energy_added, Some(4.0));
}

#[test]
fn t7_an_unusable_counter_inside_changes_nothing() {
    let samples = [b(0).c(1.00).s(), b(1).s(), b(2).c(3.00).s()];
    let open = check(&samples, None, Some(3.00), None, "T7");
    assert_eq!(open.sample_count, 3);
}

#[test]
fn r10_negative_and_non_finite_counters_are_unusable() {
    let mut negative = b(1).s();
    negative.charge_energy_added_kwh = Some(-0.01);
    let mut infinite = b(2).s();
    infinite.charge_energy_added_kwh = Some(f64::INFINITY);
    let mut not_a_number = b(3).s();
    not_a_number.charge_energy_added_kwh = Some(f64::NAN);
    let samples = [
        b(0).c(2.00).s(),
        negative,
        infinite,
        not_a_number,
        b(4).c(3.00).s(),
    ];
    let open = check(&samples, None, Some(3.00), None, "R10");
    // The unusable values did not restart the counter.
    assert_eq!(open.energy_added_carry_kwh, 0.0);
    assert_eq!(open.last_energy_added, Some(3.00));
    // After an unusable counter the next interval is not counted (R18).
    let mut open = fold(&[b(0).a7().c(0.00).s(), b(10).a7().c(1.08).s()], None);
    let counted = open.ratio_added_kwh;
    observe_charge_sample(&mut open, &b(20).a7().s());
    assert_eq!(open.prev_energy_added_kwh, None);
    observe_charge_sample(&mut open, &b(30).a7().c(3.24).s());
    assert_eq!(open.ratio_added_kwh, counted, "the interval after None");
    observe_charge_sample(&mut open, &b(40).a7().c(4.32).s());
    close(
        Some(open.ratio_added_kwh),
        Some(counted + 1.08),
        "counted again",
    );
}

// -------------------------------------------------------------- energy used

#[test]
fn t8_no_counter_gives_the_integral_alone() {
    let samples = [b(0).a7().s(), b(10).a7().s(), b(20).a7().s()];
    let open = check(&samples, None, None, Some(2.40), "T8");
    assert_eq!(open.is_dc, Some(false));
}

#[test]
fn t9_a_direct_integral_with_the_counter() {
    let samples = [
        b(0).a7().c(0.00).s(),
        b(10).a7().c(1.08).s(),
        b(20).a7().c(2.16).s(),
        b(30).a7().c(3.24).s(),
    ];
    let open = check(&samples, None, Some(3.24), Some(3.60), "T9");
    assert!(open.fully_powered);
    assert_eq!(open.pre_sample_kwh, Some(0.0));
    close(open.energy_used_kwh, Some(3.60), "U_p");
}

#[test]
fn t10_the_interval_bound_is_inclusive() {
    let last_over = T0 + 20 * MIN + 600_001;
    let over = [
        b(0).a7().c(0.00).s(),
        b(10).a7().c(1.08).s(),
        b(20).a7().c(2.16).s(),
        b_ms(99, last_over).a7().c(3.24).s(),
    ];
    let open = check(&over, None, Some(3.24), Some(3.60), "T10 over");
    assert!(!open.fully_powered);
    // The over-long interval adds nothing: U_p is the two powered intervals.
    close(open.energy_used_kwh, Some(2.4), "T10 U_p");
    assert_eq!(K_INT_MAX_MS, 600_000);
    // C2 section 7, reason 3: at the default configuration the silence the
    // collector hands the lifecycle fold as `T_gap` is `K_INT_MAX_MS`, so the
    // intervals the energy fold leaves unintegrated are the silences C1 R65
    // judges.
    let default_gap = Duration::from_secs(
        crate::config::CollectorConfig::default().offline_drive_timeout_seconds,
    );
    assert_eq!(default_gap, DEFAULT_OFFLINE_DRIVE_TIMEOUT);
    assert_eq!(default_gap.as_millis() as i64, K_INT_MAX_MS);
    let exact = [
        b(0).a7().c(0.00).s(),
        b(10).a7().c(1.08).s(),
        b(20).a7().c(2.16).s(),
        b_ms(99, T0 + 20 * MIN + 600_000).a7().c(3.24).s(),
    ];
    let open = fold(&exact, None);
    assert!(open.fully_powered, "exactly K_INT_MAX_MS stays powered");
}

#[test]
fn t11_an_offline_gap_uses_the_counter_and_the_ratio() {
    let samples = [
        b(0).a7().c(0.00).s(),
        b(10).a7().c(1.08).s(),
        b(20).a7().c(2.16).s(),
        b(180).power(0.0).volts(240.0).amps(0.0).c(20.00).s(),
    ];
    let open = check(&samples, None, Some(20.00), Some(22.22), "T11");
    assert!(!open.fully_powered);
    close(Some(open.ratio_used_kwh), Some(2.4), "U_r");
    close(Some(open.ratio_added_kwh), Some(2.16), "A_r");
}

#[test]
fn t12_energy_before_the_first_sample_is_scaled_by_the_ratio() {
    let samples = [
        b(0).a7().c(0.40).s(),
        b(10).a7().c(1.48).s(),
        b(20).a7().c(2.56).s(),
    ];
    let open = check(&samples, None, Some(2.56), Some(2.84), "T12");
    close(open.pre_sample_kwh, Some(0.40), "pre-sample");
}

#[test]
fn t12b_a_small_energy_before_the_first_sample_counts_at_ratio_one() {
    let small = [
        b(0).a7().c(0.03).s(),
        b(10).a7().c(1.11).s(),
        b(20).a7().c(2.19).s(),
    ];
    let open = check(&small, None, Some(2.19), Some(2.43), "T12b row 3");
    close(open.pre_sample_kwh, Some(0.03), "pre-sample");
    let larger = [
        b(0).a7().c(0.05).s(),
        b(10).a7().c(1.13).s(),
        b(20).a7().c(2.21).s(),
    ];
    check(&larger, None, Some(2.21), Some(2.46), "T12b row 4");
    // The bound is inclusive: 0.04 kWh before the first sample is row 3. Here
    // the two rows differ: U_p = U_r = 2.40 and A_r = 1.60, so row 3 gives
    // 2.40 + 0.04 = 2.44 while row 4 would give 1.64 * 2.40 / 1.60 = 2.46.
    let edge = [
        b(0).a7().c(0.04).s(),
        b(10).a7().c(0.84).s(),
        b(20).a7().c(1.64).s(),
    ];
    let open = check(&edge, None, Some(1.64), Some(2.44), "T12b edge");
    close(open.pre_sample_kwh, Some(0.04), "edge pre-sample");
    // One counter step above the bound is row 4 on the same intervals:
    // 1.61 * 2.40 / 1.56 = 2.48 where row 3 would give 2.45.
    let above = [
        b(0).a7().c(0.05).s(),
        b(10).a7().c(0.85).s(),
        b(20).a7().c(1.61).s(),
    ];
    check(&above, None, Some(1.61), Some(2.48), "T12b above the edge");
}

#[test]
fn r19_row_three_holds_when_the_ratio_is_unavailable() {
    // A_r is 0.54 kWh, below K_RATIO_MIN_KWH, so row 4 cannot apply and only
    // row 3 can give a value: U_p 0.60 + e0 0.04, with E 0.58.
    let at_the_bound = [b(0).a7().c(0.04).s(), b(5).a7().c(0.58).s()];
    let open = check(
        &at_the_bound,
        None,
        Some(0.58),
        Some(0.64),
        "row 3 at e0 0.04",
    );
    assert!(open.ratio_added_kwh < K_RATIO_MIN_KWH);
    // One counter step more before the first sample leaves neither row.
    let past = [b(0).a7().c(0.05).s(), b(5).a7().c(0.59).s()];
    check(&past, None, Some(0.59), None, "row 3 past the bound");
    // Nothing before the first sample is row 3 at e0 0 (no `e0 > 0` needed).
    let none = [b(0).a7().c(0.00).s(), b(5).a7().c(0.54).s()];
    check(&none, None, Some(0.54), Some(0.60), "row 3 at e0 0");
}

#[test]
fn r19_row_four_needs_exactly_one_kwh_of_counted_energy() {
    // A_r is exactly K_RATIO_MIN_KWH: used = 1.50 * 1.20 / 1.00.
    let exact = [b(0).a7().c(0.50).s(), b(10).a7().c(1.50).s()];
    let open = check(&exact, None, Some(1.50), Some(1.80), "row 4 at A_r 1.00");
    assert_eq!(open.ratio_added_kwh, K_RATIO_MIN_KWH);
    // One counter step less is row 5.
    let under = [b(0).a7().c(0.50).s(), b(10).a7().c(1.49).s()];
    check(&under, None, Some(1.49), None, "row 4 below A_r 1.00");
}

#[test]
fn r20_the_ac_floor_lies_at_ninety_five_per_cent_of_the_energy_added() {
    // A7 for 20 minutes integrates to 2.40 kWh (row 3, nothing before the
    // first sample). The counter sets the ratio of used to added.
    let session = |middle: f64, added: f64| {
        [
            b(0).a7().c(0.00).s(),
            b(10).a7().c(middle).s(),
            b(20).a7().c(added).s(),
        ]
    };
    // 2.40 / 2.54 = 0.945: below the floor, withheld.
    check(
        &session(1.27, 2.54),
        None,
        Some(2.54),
        None,
        "R20 below the floor",
    );
    // 2.40 / 2.51 = 0.956: above the floor, published.
    check(
        &session(1.25, 2.51),
        None,
        Some(2.51),
        Some(2.40),
        "R20 above the floor",
    );
    // E exactly K_RATIO_MIN_KWH: the floor applies and 0.84 kWh is below 0.95.
    let at_one = [b(0).a7().c(0.00).s(), b(7).a7().c(1.00).s()];
    check(&at_one, None, Some(1.00), None, "R20 at E 1.00");
    // E one counter step below K_RATIO_MIN_KWH: no floor, the value stands.
    let below_one = [b(0).a7().c(0.00).s(), b(7).a7().c(0.99).s()];
    check(&below_one, None, Some(0.99), Some(0.84), "R20 below E 1.00");
}

#[test]
fn t13_an_unavailable_ratio_gives_none() {
    let samples = [
        b(0).a7().c(0.00).s(),
        b(5).a7().c(0.54).s(),
        b(180).power(0.0).c(20.00).s(),
    ];
    let open = check(&samples, None, Some(20.00), None, "T13");
    assert!(open.ratio_added_kwh < K_RATIO_MIN_KWH);
}

#[test]
fn t14_plugged_in_at_zero_kw_is_a_measured_zero() {
    let idle = |minutes| {
        b(minutes)
            .power(0.0)
            .volts(230.0)
            .amps(0.0)
            .phases(3)
            .c(0.00)
            .s()
    };
    let samples = [idle(0), idle(10), idle(20)];
    let open = check(&samples, None, Some(0.00), Some(0.00), "T14");
    assert!(open.fully_powered);
    assert_eq!(open.energy_used_kwh, Some(0.0));
}

#[test]
fn t15_a_single_after_gap_sample_has_no_energy_used() {
    let samples = [b(0).c(25.00).power(0.0).s()];
    check(&samples, Some(5.00), Some(20.00), None, "T15");
}

#[test]
fn t16_a_single_sample_with_nothing_added_is_a_zero() {
    let samples = [b(0).c(0.00).s()];
    check(&samples, None, Some(0.00), Some(0.00), "T16");
}

#[test]
fn t17_the_ac_floor_withholds_a_low_integral() {
    let ladder = |decorate: fn(B) -> B| -> Vec<ProjectionChargeSample> {
        (0..7)
            .map(|step| decorate(b(step * 10).c(step as f64 * 22.0 / 100.0)).s())
            .collect()
    };
    // P 1 kW with the charge point stated AC: 1.00 kWh against 1.32 added.
    let reported = ladder(|sample| sample.power(1.0).present(false));
    let open = check(&reported, None, Some(1.32), None, "T17");
    assert_eq!(open.is_dc, Some(false));
    // V 120, I 12, one phase: R34 is unique, so 1.44 kW and the floor holds.
    let measured = ladder(|sample| sample.power(1.0).volts(120.0).amps(12.0).phases(1));
    let open = check(&measured, None, Some(1.32), Some(1.44), "T17 measured");
    assert_eq!(open.is_dc, Some(false));
}

#[test]
fn t17b_an_unknown_kind_has_no_floor() {
    let samples: Vec<_> = (0..7)
        .map(|step| b(step * 10).c(step as f64 * 22.0 / 100.0).power(1.0).s())
        .collect();
    let open = check(&samples, None, Some(1.32), Some(1.00), "T17b");
    assert_eq!(open.is_dc, None);
}

#[test]
fn t17c_a_dc_like_taper_of_unknown_kind_is_kept() {
    let taper = [
        b(0).power(40.0).c(0.00),
        b(2).power(30.0).c(1.25),
        b(4).power(20.0).c(2.20),
    ];
    let samples = taper.clone().map(B::s);
    let open = check(&samples, None, Some(2.20), Some(2.00), "T17c");
    assert_eq!(open.is_dc, None);
    // The same samples with the charge point stated AC: the floor applies.
    let ac = taper.map(|sample| sample.present(false).s());
    let open = check(&ac, None, Some(2.20), None, "T17c AC");
    assert_eq!(open.is_dc, Some(false));
}

#[test]
fn t18_a_dc_session_integrates_the_reported_power() {
    let dc = |minutes, power, counter| {
        b(minutes)
            .power(power)
            .present(true)
            .kind("Supercharger")
            .volts(2.0)
            .amps(0.0)
            .c(counter)
            .s()
    };
    let samples = [dc(0, 150.0, 0.00), dc(2, 120.0, 5.00), dc(4, 90.0, 9.00)];
    let open = check(&samples, None, Some(9.00), Some(8.00), "T18");
    assert_eq!(open.is_dc, Some(true));
    assert_eq!(open.fast_charger_type.as_deref(), Some("Supercharger"));
}

#[test]
fn r20_dc_has_no_floor_and_ac_keeps_a_value_at_the_floor() {
    // DC: battery-side integral 1.00 kWh against 1.32 added is legitimate.
    let dc: Vec<_> = (0..7)
        .map(|step| {
            b(step * 10)
                .power(1.0)
                .kind("Combo")
                .c(step as f64 * 22.0 / 100.0)
                .s()
        })
        .collect();
    check(&dc, None, Some(1.32), Some(1.00), "R20 DC");
    // AC at 7.2 kW whose counter says 2.16 over 20 minutes against an
    // integral of 2.40 is far above the floor and is published.
    let ac = [
        b(0).a7().c(0.00).s(),
        b(10).a7().c(1.08).s(),
        b(20).a7().c(2.16).s(),
    ];
    check(&ac, None, Some(2.16), Some(2.40), "R20 AC");
    // Under one kWh of energy added the floor does not apply (E < 1).
    let small = [
        b(0).power(2.0).present(false).c(0.00).s(),
        b(10).power(2.0).present(false).c(0.90).s(),
    ];
    let open = fold(&small, None);
    assert_eq!(open.is_dc, Some(false));
    let totals = charge_energy_totals(&open);
    close(totals.used_kwh, Some(0.33), "R20 below one kWh");
}

#[test]
fn r19_row_two_nothing_added() {
    // Gap-free, nothing reached the battery (heater draw): the integral.
    let heating = [
        b(0).power(1.0).c(0.00).s(),
        b(10).power(1.0).c(0.00).s(),
        b(20).power(1.0).c(0.00).s(),
    ];
    check(&heating, None, Some(0.00), Some(0.33), "row 2 integral");
    // A gap hides a possible draw the counter does not see: None.
    let gap = [b(0).power(1.0).c(0.00).s(), b(60).power(1.0).c(0.00).s()];
    check(&gap, None, Some(0.00), None, "row 2 gap");
    // E below half a counter step is nothing.
    let noise = [b(0).power(0.0).c(0.00).s(), b(10).power(0.0).c(0.004).s()];
    check(&noise, None, Some(0.00), Some(0.00), "row 2 noise");
}

#[test]
fn r19_row_one_needs_two_gap_free_samples() {
    check(&[b(0).a7().s()], None, None, None, "row 1 single");
    let gap = [b(0).a7().s(), b(60).a7().s()];
    check(&gap, None, None, None, "row 1 gap");
}

#[test]
fn r19_a_restart_interval_is_left_out_of_the_ratio() {
    let samples = [
        b(0).a7().c(0.40).s(),
        b(10).a7().c(1.48).s(),
        b(20).a7().c(2.56).s(),
        b(30).a7().c(0.30).s(),
        b(40).a7().c(1.38).s(),
    ];
    let open = fold(&samples, None);
    assert_eq!(open.energy_added_carry_kwh, 2.56);
    close(
        Some(open.ratio_added_kwh),
        Some(3.24),
        "A_r leaves the restart out",
    );
    close(
        Some(open.ratio_used_kwh),
        Some(3.6),
        "U_r leaves the restart out",
    );
    // E = 2.56 + 1.38 = 3.94; used = 3.94 * 3.6 / 3.24 = 4.378.
    let totals = charge_energy_totals(&open);
    close(totals.added_kwh, Some(3.94), "restart added");
    close(totals.used_kwh, Some(4.38), "restart used");
}

#[test]
fn r13_the_output_is_never_negative_and_is_rounded_once() {
    let mut open = OpenCharge::empty(1, 1, T0);
    open.first_energy_added = Some(0.0);
    open.max_energy_added = Some(1.004);
    assert_eq!(charge_energy_totals(&open).added_kwh, Some(1.0));
    open.max_energy_added = Some(1.006);
    assert_eq!(charge_energy_totals(&open).added_kwh, Some(1.01));
    // A baseline above the maximum (a hand-edited blob) cannot go negative.
    open.first_energy_added = Some(5.0);
    open.max_energy_added = Some(1.0);
    let added = charge_energy_totals(&open).added_kwh;
    assert_eq!(added, Some(0.0));
    assert!(added.unwrap().is_sign_positive(), "no -0.0 (R4)");
}

#[test]
fn r4_round2_writes_negative_zero_as_zero() {
    assert!(round2(-0.004).is_sign_positive());
    assert_eq!(round2(-0.004), 0.0);
    assert_eq!(round2(2.456), 2.46);
    assert_eq!(round2(-3.2), -3.2);
}

#[test]
fn t21_equal_timestamps_are_skipped() {
    let samples = [
        b_ms(1, T0).a7().c(0.00).s(),
        b_ms(2, T0).a7().c(0.00).s(),
        b_ms(3, T0 + 10 * MIN).a7().c(1.08).s(),
        b_ms(4, T0 + 20 * MIN).a7().c(2.16).s(),
    ];
    let open = check(&samples, None, Some(2.16), Some(2.40), "T21");
    assert!(open.fully_powered);
    assert_eq!(open.sample_count, 4);
}

#[test]
fn r26_the_slice_fold_orders_by_time_then_id() {
    let ordered = [
        b_ms(1, T0).a7().c(0.00).s(),
        b_ms(2, T0 + 10 * MIN).a7().c(1.08).s(),
        b_ms(3, T0 + 20 * MIN).a7().c(2.16).s(),
    ];
    let mut shuffled = ordered.to_vec();
    shuffled.reverse();
    assert_eq!(
        charge_energy_from_samples(&shuffled, None),
        charge_energy_from_samples(&ordered, None)
    );
}

#[test]
fn r24_a_very_long_session_integrates_normally() {
    // Four days of ten-minute samples, idle: sums stay exact zeros.
    let samples: Vec<_> = (0..576)
        .map(|step| b(step * 10).power(0.0).c(1.00).s())
        .collect();
    let open = check(&samples, Some(1.00), Some(0.00), Some(0.00), "R24 idle");
    assert_eq!(open.sample_count, 576);
    // A charge held open for days with a charge at the end.
    let mut samples = vec![b(0).a7().c(0.00).s()];
    samples.push(b(10).a7().c(1.20).s());
    samples.push(b(4 * 24 * 60).a7().c(1.30).s());
    let open = fold(&samples, None);
    assert!(!open.fully_powered, "the multi-day gap is not integrated");
}

// ---------------------------------------------------- session kind and phases

#[test]
fn r31_r32_the_observer_is_the_only_writer_of_kind_and_type() {
    let samples = [
        b(0).kind("<invalid>").s(),
        b(1).kind("Supercharger").present(true).power(120.0).s(),
        b(2).kind("Combo").present(false).power(7.0).s(),
    ];
    let open = fold(&samples, None);
    assert_eq!(open.is_dc, Some(true));
    assert_eq!(open.fast_charger_type.as_deref(), Some("Supercharger"));
}

#[test]
fn r36_the_session_phase_count_follows_the_sample_not_itself() {
    let open = fold(
        &[
            b(0).volts(230.0).amps(16.0).power(11.0).phases(3).s(),
            b(10).volts(230.0).amps(6.0).power(4.0).s(),
        ],
        None,
    );
    // The second sample is a tie broken by the session count: its power is
    // 3 * 230 * 6 = 4.14 kW and the count is still the first sample's.
    close(
        open.last_sample_power_kw,
        Some(4.14),
        "tie broken by session",
    );
    assert_eq!(open.phase_count, Some(3));
    let switched = fold(
        &[
            b(0).volts(230.0).amps(16.0).power(11.0).phases(3).s(),
            b(10).volts(230.0).amps(6.0).power(1.0).phases(1).s(),
        ],
        None,
    );
    close(
        switched.last_sample_power_kw,
        Some(1.38),
        "switched to one phase",
    );
    assert_eq!(switched.phase_count, Some(1));
}

#[test]
fn s5_the_observer_writes_the_last_sample_time_and_count() {
    let mut open = OpenCharge::empty(1, 1, T0);
    observe_charge_sample(&mut open, &b(3).a7().c(1.0).s());
    assert_eq!(open.last_sample_timestamp_ms, Some(T0 + 3 * MIN));
    assert_eq!(open.sample_count, 1);
    assert_eq!(open.last_energy_added, Some(1.0));
    close(open.last_sample_power_kw, Some(7.2), "last power");
    assert_eq!(open.energy_used_kwh, Some(0.0));
}

// ------------------------------------------------------------ old blobs, R2a

#[test]
fn t20_an_old_blob_continues_from_its_stored_fields() {
    let mut open: OpenCharge = serde_json::from_value(json!({
        "id": 1, "car_id": 1, "start_date_ms": T0 - HOUR,
        "outside_temp_sum": 0.0, "outside_temp_count": 0,
        "first_energy_added": 0.0, "max_energy_added": 5.0,
        "last_energy_added": 5.0, "sample_count": 3,
        "energy_used_kwh": 5.5, "last_sample_timestamp_ms": T0,
        "last_sample_power_kw": 7.2
    }))
    .expect("old blob");
    // The new fields decode at their defaults (R25).
    assert_eq!(open.energy_added_carry_kwh, 0.0);
    assert_eq!(open.prev_energy_added_kwh, None);
    assert_eq!(open.ratio_used_kwh, 0.0);
    assert_eq!(open.ratio_added_kwh, 0.0);
    assert!(!open.fully_powered);
    assert_eq!(open.pre_sample_kwh, None);
    assert_eq!(open.phase_count, None);
    for (minutes, counter) in [(10, 6.08), (20, 7.16), (30, 8.24)] {
        observe_charge_sample(
            &mut open,
            &b_ms(minutes, T0 + minutes * MIN).a7().c(counter).s(),
        );
    }
    let totals = charge_energy_totals(&open);
    close(totals.added_kwh, Some(8.24), "T20 added");
    close(totals.used_kwh, Some(9.16), "T20 used");
    assert!(
        !open.fully_powered,
        "an upgraded session never claims a gap-free integral"
    );
    assert_eq!(open.sample_count, 6);
}

#[test]
fn r25_a_blob_with_no_new_fields_and_no_energy_decodes_and_folds() {
    let mut open: OpenCharge = serde_json::from_value(json!({
        "id": 1, "car_id": 1, "start_date_ms": T0,
        "outside_temp_sum": 0.0, "outside_temp_count": 0
    }))
    .expect("minimal blob");
    // Nothing folded yet: the first sample starts a fresh integral.
    observe_charge_sample(&mut open, &b(0).a7().c(0.00).s());
    assert!(open.fully_powered);
    assert_eq!(open.energy_used_kwh, Some(0.0));
    assert_eq!(open.pre_sample_kwh, Some(0.0));
}

#[test]
fn r2a_every_f64_accumulator_round_trips_bit_for_bit() {
    let values = [
        0.1 + 0.2,
        1.0 / 3.0,
        2.0 / 3.0 * 1000.0,
        9_007_199_254_740_993.0 / 3.0,
        5e-324,
        1.797_693_134_862_315_7e308,
        // Values a parser that scales a 64-bit significand by one power of
        // ten misreads (they read back as 0.4, 11.2 and 12.38019611496456),
        // so only the `float_roundtrip` feature keeps them exact.
        0.399_999_999_999_999_97,
        11.200_000_000_000_001,
        12.380_196_114_964_559,
    ];
    for value in values {
        let mut open = OpenCharge::empty(1, 1, T0);
        open.energy_used_kwh = Some(value);
        open.ratio_used_kwh = value;
        open.ratio_added_kwh = value;
        open.energy_added_carry_kwh = value;
        open.last_sample_power_kw = Some(value);
        open.pre_sample_kwh = Some(value);
        open.prev_energy_added_kwh = Some(value);
        open.first_energy_added = Some(value);
        open.max_energy_added = Some(value);
        open.last_energy_added = Some(value);
        let mut state = OpenSessionState::new();
        state.open_charge = Some(open.clone());
        let back = OpenSessionState::decode(&state.encode().expect("encode")).expect("decode");
        let back = back.open_charge.expect("open charge");
        let bits = |charge: &OpenCharge| {
            [
                charge.energy_used_kwh.unwrap().to_bits(),
                charge.ratio_used_kwh.to_bits(),
                charge.ratio_added_kwh.to_bits(),
                charge.energy_added_carry_kwh.to_bits(),
                charge.last_sample_power_kw.unwrap().to_bits(),
                charge.pre_sample_kwh.unwrap().to_bits(),
                charge.prev_energy_added_kwh.unwrap().to_bits(),
                charge.first_energy_added.unwrap().to_bits(),
                charge.max_energy_added.unwrap().to_bits(),
                charge.last_energy_added.unwrap().to_bits(),
            ]
        };
        assert_eq!(bits(&back), bits(&open), "round trip of {value:e}");
    }
}

#[test]
fn r2a_the_hub_manifest_enables_float_roundtrip_for_itself() {
    // The round-trip test above passes whenever any crate in the build turns
    // the feature on (today `serde_jcs` does, through feature unification),
    // so the dependency line itself is pinned: the lossy parser reads values
    // such as 0.39999999999999997 back as 0.4, and R2 and section 9 need every
    // persisted accumulator to survive bit for bit whatever else is linked.
    let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    let line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("serde_json"))
        .expect("the manifest names serde_json");
    assert!(
        line.contains("float_roundtrip"),
        "serde_json must enable float_roundtrip in this crate: {line}"
    );
}

// ------------------------------------------------------ display aggregates

#[test]
fn r27_the_aggregate_observer_keeps_last_max_and_mean() {
    let mut open = OpenCharge::empty(1, 1, T0);
    let parsed =
        |level: Option<i64>, ideal: Option<f64>, rated: Option<f64>, power, temp| ParsedSample {
            battery_level: level,
            ideal_range_km: ideal,
            rated_range_km: rated,
            charger_power_kw: power,
            outside_temp: temp,
            ..ParsedSample::default()
        };
    observe_charge_aggregate(
        &mut open,
        &parsed(Some(50), Some(200.0), Some(190.0), Some(7.0), Some(10.0)),
    );
    observe_charge_aggregate(
        &mut open,
        &parsed(None, None, Some(195.5), Some(11.0), Some(12.0)),
    );
    observe_charge_aggregate(
        &mut open,
        &parsed(Some(55), Some(210.0), None, Some(-1.0), None),
    );
    observe_charge_aggregate(&mut open, &parsed(None, None, None, None, Some(f64::NAN)));
    assert_eq!(open.last_battery_level, Some(55));
    assert_eq!(open.last_ideal_range_km, Some(210.0));
    assert_eq!(open.last_rated_range_km, Some(195.5));
    // A negative reading is not a charging power (R33 step 1, R27).
    assert_eq!(open.max_charger_power_kw, Some(11.0));
    assert_eq!(open.outside_temp_sum, 22.0);
    assert_eq!(open.outside_temp_count, 2);
    // Nothing known leaves everything as it was.
    let mut untouched = OpenCharge::empty(1, 1, T0);
    observe_charge_aggregate(&mut untouched, &ParsedSample::default());
    assert_eq!(untouched, OpenCharge::empty(1, 1, T0));
    // The counts saturate.
    open.outside_temp_count = u32::MAX;
    observe_charge_aggregate(&mut open, &parsed(None, None, None, None, Some(1.0)));
    assert_eq!(open.outside_temp_count, u32::MAX);
}

#[test]
fn r27_the_aggregate_observer_never_touches_the_energy_fields() {
    let mut open = OpenCharge::empty(1, 1, T0);
    open.first_energy_added = Some(20.0);
    let before = open.clone();
    let parsed = ParsedSample {
        charge_energy_added: Some(21.0),
        charger_power_kw: Some(7.0),
        ..ParsedSample::default()
    };
    observe_charge_aggregate(&mut open, &parsed);
    assert_eq!(open.first_energy_added, before.first_energy_added);
    assert_eq!(open.max_energy_added, None);
    assert_eq!(open.last_energy_added, None);
    assert_eq!(open.sample_count, 0);
    assert_eq!(open.is_dc, None);
}

// --------------------------------------------------------- seeded by fold

#[test]
fn r14_a_seeded_session_is_the_fold_of_its_imported_samples() {
    let start = T0 - HOUR;
    let process = ImportedChargeFixture::new(start);
    let seeded = process.open_charge();
    assert_eq!(seeded.sample_count, 2);
    // The imported counter 1.00 is no baseline: no earlier row holds any part
    // of it (R11 first row), so the baseline is zero.
    assert_eq!(seeded.first_energy_added, Some(0.0));
    assert_eq!(seeded.max_energy_added, Some(2.0));
    assert_eq!(seeded.last_energy_added, Some(2.0));
    assert_eq!(seeded.is_dc, Some(false));
    assert_eq!(seeded.last_sample_timestamp_ms, Some(start + 5 * MIN));
    // 230 V, 16 A on three phases is 11.04 kW at both samples.
    close(seeded.last_sample_power_kw, Some(11.04), "seeded power");
    assert!(seeded.fully_powered);
    assert_eq!(seeded.pre_sample_kwh, Some(1.0));
    // The seed equals the incremental fold of the same stored samples.
    let samples = process.projected_samples();
    let folded = fold(&samples, None);
    assert_eq!(folded.max_energy_added, seeded.max_energy_added);
    assert_eq!(folded.energy_used_kwh, seeded.energy_used_kwh);
    assert_eq!(folded.ratio_used_kwh, seeded.ratio_used_kwh);
    assert_eq!(folded.ratio_added_kwh, seeded.ratio_added_kwh);
}

/// The imported rows of T63: an open process and two charge rows.
struct ImportedChargeFixture {
    process: TeslaMateChargingProcess,
    samples: Vec<TeslaMateCharge>,
}

impl ImportedChargeFixture {
    fn new(start: i64) -> Self {
        let process = serde_json::from_value(json!({
            "id": 7, "car_id": 1, "position_id": null, "address_id": null,
            "geofence_id": null, "start_date_ms": start, "end_date_ms": null,
            "charge_energy_added": 99.0, "charge_energy_used_kwh": 99.0,
            "start_ideal_range_km": 151.0, "end_ideal_range_km": 152.0,
            "start_battery_level": 40, "end_battery_level": 50,
            "duration_min": null, "outside_temp_avg": 11.0,
            "start_rated_range_km": 141.0, "end_rated_range_km": 142.0,
            "cost": null
        }))
        .expect("imported charging process");
        let samples = vec![
            super::tests::imported_charge_fixture(1, 7, start, 1.0, 40, 10.0),
            super::tests::imported_charge_fixture(2, 7, start + 5 * MIN, 2.0, 50, 12.0),
        ];
        Self { process, samples }
    }

    fn open_charge(&self) -> OpenCharge {
        open_charge_from_source(&self.process, &self.samples)
    }

    fn projected_samples(&self) -> Vec<ProjectionChargeSample> {
        self.samples.iter().map(project_charge_sample).collect()
    }
}

#[test]
fn r14_the_seed_never_reads_the_imported_row_energy_columns() {
    let start = T0 - HOUR;
    let process = ImportedChargeFixture::new(start);
    // The fixture's own columns say 99.0; the seed ignores them.
    let seeded = process.open_charge();
    let totals = charge_energy_totals(&seeded);
    close(totals.added_kwh, Some(2.0), "seeded added");
}

// ----------------------------------------------------- lifecycle level

/// The spec's `Ob(...)`: an owner `vehicle_data` with a charge group whose
/// charging state is `state`, plus the listed charge fields, and a vehicle
/// group carrying the odometer (621.37 miles).
pub(crate) fn ob(id: i64, at_ms: i64, state: &str, fields: Value) -> LifecycleSample {
    let mut charge = Map::new();
    charge.insert("timestamp".to_owned(), json!(at_ms));
    charge.insert("charging_state".to_owned(), json!(state));
    if let Value::Object(extra) = fields {
        charge.extend(extra);
    }
    LifecycleSample {
        observation_id: id,
        observed_at_ms: at_ms,
        vehicle_state: "online".to_owned(),
        payload: json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": "online",
            "provider_raw_json": {"response": {
                "charge_state": Value::Object(charge),
                "vehicle_state": {"timestamp": at_ms, "odometer": 621.37},
            }},
        }),
    }
}

pub(crate) fn a7_fields(counter: Option<f64>) -> Value {
    let mut fields = json!({
        "charger_power": 7, "charger_voltage": 240,
        "charger_actual_current": 30, "charger_phases": 1,
    });
    if let Some(counter) = counter {
        fields["charge_energy_added"] = json!(counter);
    }
    fields
}

pub(crate) fn counter_only(counter: f64) -> Value {
    json!({ "charge_energy_added": counter })
}

fn run_all(state: OpenSessionState, samples: &[LifecycleSample]) -> LifecycleStep {
    apply_samples(state, 1, samples).expect("apply samples")
}

#[test]
fn t3a_a_replug_after_unplug_counts_the_whole_counter() {
    let samples = [
        ob(1, T0 - 40 * MIN, "Charging", a7_fields(Some(18.00))),
        ob(2, T0, "Complete", a7_fields(Some(20.00))),
        ob(3, T0 + 5 * MIN, "Disconnected", json!({})),
        ob(4, T0 + HOUR, "Charging", a7_fields(Some(25.00))),
        ob(5, T0 + HOUR + 10 * MIN, "Complete", a7_fields(Some(26.08))),
    ];
    let result = run_all(OpenSessionState::new(), &samples);
    assert_eq!(result.delta.charges.len(), 2);
    assert_eq!(result.delta.charges[0].charge_energy_added, Some(20.00));
    // The baseline is 0.0 (the cable was out), so the whole counter is new.
    assert_eq!(result.delta.charges[1].charge_energy_added, Some(26.08));
}

#[test]
fn t3b_a_preset_the_first_counter_is_below_is_lowered() {
    let before = [
        ob(1, T0 - 40 * MIN, "Charging", a7_fields(Some(18.00))),
        ob(2, T0 - 20 * MIN, "Complete", a7_fields(Some(20.00))),
    ];
    let first = run_all(OpenSessionState::new(), &before);
    assert_eq!(first.delta.charges[0].charge_energy_added, Some(20.00));
    // The next session opens with no counter, so the preset is 20.00.
    let opening = ob(3, T0, "Charging", a7_fields(None));
    let opened = run_all(first.state, &[opening]);
    assert_eq!(
        opened
            .state
            .open_charge
            .as_ref()
            .unwrap()
            .first_energy_added,
        Some(20.00)
    );
    let rest = [
        ob(4, T0 + 10 * MIN, "Charging", a7_fields(Some(0.30))),
        ob(5, T0 + 20 * MIN, "Complete", a7_fields(Some(1.38))),
    ];
    let result = run_all(opened.state, &rest);
    // The first counter (0.30) is below the preset, so the baseline becomes
    // 0.0 (R11), and C1 R66 does not split the session.
    assert_eq!(result.delta.charges.len(), 1);
    assert_eq!(result.delta.charges[0].charge_energy_added, Some(1.38));
}

#[test]
fn t5_a_top_up_after_complete_does_not_count_the_earlier_energy_twice() {
    let samples = [
        ob(1, T0, "Charging", counter_only(10.0)),
        ob(2, T0 + 10 * MIN, "Complete", counter_only(20.00)),
        ob(3, T0 + 15 * MIN, "Charging", counter_only(20.50)),
        ob(4, T0 + 25 * MIN, "Complete", counter_only(21.00)),
    ];
    let result = run_all(OpenSessionState::new(), &samples);
    assert_eq!(result.delta.charges.len(), 2);
    assert_eq!(result.delta.charges[0].charge_energy_added, Some(20.00));
    // The baseline is the snapshot's 20.00 (C1 R61a), so only 1.00 is new.
    assert_eq!(result.delta.charges[1].charge_energy_added, Some(1.00));
}

/// The closed rows of every step, with their samples.
struct Closed {
    charges: Vec<ProjectionCharge>,
    samples: Vec<ProjectionChargeSample>,
}

/// Fold `samples` one by one, optionally encoding and decoding the state
/// between every observation, and return the closed rows and the final blob.
fn run_stepwise(samples: &[LifecycleSample], through_blob: bool) -> (Closed, Vec<u8>) {
    let mut state = OpenSessionState::new();
    let mut closed = Closed {
        charges: Vec::new(),
        samples: Vec::new(),
    };
    for sample in samples {
        let step = apply_sample(state, 1, sample).expect("apply sample");
        closed.charges.extend(step.delta.charges);
        closed.samples.extend(step.delta.open_charge_samples);
        state = if through_blob {
            OpenSessionState::decode(&step.state.encode().expect("encode")).expect("decode")
        } else {
            step.state
        };
    }
    (closed, state.encode().expect("final blob"))
}

fn samples_of(closed: &Closed, charge: &ProjectionCharge) -> Vec<ProjectionChargeSample> {
    closed
        .samples
        .iter()
        .filter(|sample| sample.charge_process_id == charge.id)
        .cloned()
        .collect()
}

#[test]
fn t19_the_incremental_fold_equals_the_slice_across_encode_and_decode() {
    // T11's samples through the lifecycle (C1 R65 continuity holds).
    let gap = [
        ob(1, T0, "Charging", a7_fields(Some(0.00))),
        ob(2, T0 + 10 * MIN, "Charging", a7_fields(Some(1.08))),
        ob(3, T0 + 20 * MIN, "Charging", a7_fields(Some(2.16))),
        ob(
            4,
            T0 + 3 * HOUR,
            "Complete",
            json!({"charge_energy_added": 20.00, "charger_power": 0,
                   "charger_voltage": 240, "charger_actual_current": 0}),
        ),
    ];
    // T17's second variant: seven samples at 120 V, 12 A, one phase, 1 kW.
    let measured: Vec<LifecycleSample> = (0..7)
        .map(|step| {
            let state = if step == 6 { "Complete" } else { "Charging" };
            ob(
                step + 1,
                T0 + step * 10 * MIN,
                state,
                json!({"charge_energy_added": step as f64 * 22.0 / 100.0,
                       "charger_power": 1, "charger_voltage": 120,
                       "charger_actual_current": 12, "charger_phases": 1}),
            )
        })
        .collect();
    // Six AC samples at varying voltage and current: the integral's partial
    // sums 1.0793333333333335, 1.5483333333333333 and 2.1550000000000002 are
    // values a lossy parser misreads, so the state must survive each encode
    // and decode exactly (R2a). Power is `V * I / 1000` of the one consistent
    // phase count (R34, R35 row 1).
    let varied: Vec<LifecycleSample> = [
        (230, 12, 3, 0.00),
        (230, 16, 4, 0.50),
        (236, 12, 3, 1.00),
        (233, 12, 3, 1.40),
        (236, 19, 4, 2.00),
        (240, 27, 6, 2.60),
    ]
    .into_iter()
    .enumerate()
    .map(|(step, (volts, amps, power, counter))| {
        let state = if step == 5 { "Complete" } else { "Charging" };
        ob(
            step as i64 + 1,
            T0 + step as i64 * 10 * MIN,
            state,
            json!({"charge_energy_added": counter, "charger_power": power,
                   "charger_voltage": volts, "charger_actual_current": amps,
                   "charger_phases": 1}),
        )
    })
    .collect();
    for (label, samples, added, used) in [
        ("gap", gap.to_vec(), 20.00, 22.22),
        ("measured", measured, 1.32, 1.44),
        ("lossy digits", varied, 2.60, 3.07),
    ] {
        let (direct, direct_blob) = run_stepwise(&samples, false);
        let (through, through_blob) = run_stepwise(&samples, true);
        assert_eq!(direct.charges.len(), 1, "{label}");
        let charge = &direct.charges[0];
        assert_eq!(charge.charge_energy_added, Some(added), "{label} added");
        assert_eq!(charge.charge_energy_used_kwh, Some(used), "{label} used");
        assert_eq!(direct.charges, through.charges, "{label}: rows after blobs");
        assert_eq!(direct_blob, through_blob, "{label}: final blob bytes");
        let stored = samples_of(&direct, charge);
        let totals = charge_energy_from_samples(&stored, Some(0.0));
        assert_eq!(
            totals.added_kwh, charge.charge_energy_added,
            "{label}: slice"
        );
        assert_eq!(
            totals.used_kwh, charge.charge_energy_used_kwh,
            "{label}: slice"
        );
        // One closed observation run in a single call gives the same rows.
        let single = run_all(OpenSessionState::new(), &samples);
        assert_eq!(single.delta.charges, direct.charges, "{label}: one call");
    }
}

#[test]
fn t63_a_seeded_session_continues_live_with_baseline_zero() {
    let start = T0 - HOUR;
    let process = ImportedChargeFixture::new(start);
    let session = TeslaMateOpenSession {
        car_id: 1,
        charge: Some(process.process.clone()),
        charge_samples: process.samples.clone(),
        ..Default::default()
    };
    let mut session = session;
    session.watermarks.charges.max_timestamp_ms = Some(start + 5 * MIN);
    let state = seed_imported_open_session_state(uuid::Uuid::nil(), &session, None)
        .expect("seed imported charge");
    let live = [
        ob(1, start + 7 * MIN, "Charging", counter_only(3.00)),
        ob(2, start + 9 * MIN, "Complete", counter_only(4.00)),
    ];
    let result = run_all(state, &live);
    assert_eq!(result.delta.charges.len(), 1);
    let closed = &result.delta.charges[0];
    assert_eq!(closed.id, 7);
    // The imported counter 1.00 is not a baseline: 4.00 - 0.0.
    assert_eq!(closed.charge_energy_added, Some(4.00));
    // Only the live samples travel in the delta; the imported ones stay.
    assert_eq!(result.delta.open_charge_samples.len(), 2);
}
