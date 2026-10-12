// SPDX-License-Identifier: AGPL-3.0-only

//! Part C2-P5 spec tests for the session figure and the car statistic (C2
//! specification section 10: T50 to T55) and the rules of R62 to R66 that no
//! numbered test pins: the window edges, the span edge, the heater range, the
//! sample order, and the median and its rounding. The store-level tests (T56
//! to T61, T58b, T64, T65) are in `src/storage/db/tests/efficiency.rs`.
//! Synthetic data only. `T0` is 1_760_000_000_000 ms.
//!
//! `fig(level, rated, c)` builds one `ProjectionChargeSample` with a battery
//! level in per cent, rated range in km and counter in kWh; the spec writes it
//! `(level, rated, c)`. Samples are one minute apart, in the order given.

use super::*;

const T0: i64 = 1_760_000_000_000;

fn bare(id: i64) -> ProjectionChargeSample {
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
        fast_charger_brand: None,
        fast_charger_type: None,
        charge_cable: None,
    }
}

fn fig(id: i64, level: i64, rated: f64, counter: f64) -> ProjectionChargeSample {
    ProjectionChargeSample {
        battery_level: Some(level),
        rated_range_km: Some(rated),
        charge_energy_added_kwh: Some(counter),
        ..bare(id)
    }
}

/// T50's samples: 50 to 90 % in 10-point steps, 6 kWh and 40 km per step, and
/// a last sample at 95 % that is above the window.
fn t50() -> Vec<ProjectionChargeSample> {
    vec![
        fig(1, 50, 200.00, 0.00),
        fig(2, 60, 240.00, 6.00),
        fig(3, 70, 280.00, 12.00),
        fig(4, 80, 320.00, 18.00),
        fig(5, 90, 360.00, 24.00),
        fig(6, 95, 380.00, 27.00),
    ]
}

#[test]
fn t50_the_session_figure_is_battery_energy_per_rated_km() {
    // (24.00 - 0.00) kWh over (360 - 200) km, window 50 to 90 %.
    assert_eq!(session_efficiency_wh_per_km(&t50()), Some(150.0));
}

#[test]
fn t51_a_span_under_ten_points_gives_no_figure() {
    let samples = [fig(1, 80, 300.00, 0.00), fig(2, 89, 336.00, 5.40)];
    assert_eq!(session_efficiency_wh_per_km(&samples), None);
}

#[test]
fn the_span_edge_is_ten_points_inclusive() {
    // 80 to 90 % is exactly K_EFF_MIN_SPAN: 6.00 kWh over 40 km.
    let samples = [fig(1, 80, 300.00, 0.00), fig(2, 90, 340.00, 6.00)];
    assert_eq!(session_efficiency_wh_per_km(&samples), Some(150.0));
}

#[test]
fn t52_samples_above_the_window_are_ignored() {
    let mut samples = t50();
    samples.push(fig(7, 99, 300.00, 40.00));
    assert_eq!(session_efficiency_wh_per_km(&samples), Some(150.0));
}

#[test]
fn the_window_edge_is_ninety_per_cent_inclusive() {
    // The first four samples of T50 (50 to 80 %) give 150.0 on their own.
    let mut samples = t50();
    samples.truncate(4);
    // A last sample at 91 % is outside the window and ignored.
    samples.push(fig(5, 91, 370.00, 24.90));
    assert_eq!(session_efficiency_wh_per_km(&samples), Some(150.0));
    // At 90 % it is inside and ends the window: 24.00 kWh over 150 km.
    samples.pop();
    samples.push(fig(5, 90, 350.00, 24.00));
    assert_eq!(session_efficiency_wh_per_km(&samples), Some(160.0));
}

#[test]
fn t53_a_heating_battery_inside_the_window_gives_no_figure() {
    let mut samples = t50();
    samples[2].battery_heater_on = Some(true);
    assert_eq!(session_efficiency_wh_per_km(&samples), None);
    let mut heater = t50();
    heater[2].battery_heater = Some(true);
    assert_eq!(session_efficiency_wh_per_km(&heater), None);
}

#[test]
fn t53_a_heater_only_above_the_window_is_ignored() {
    let mut samples = t50();
    samples[5].battery_heater_on = Some(true);
    assert_eq!(session_efficiency_wh_per_km(&samples), Some(150.0));
}

#[test]
fn a_heater_reading_off_or_unknown_does_not_block() {
    let mut samples = t50();
    samples[1].battery_heater_on = Some(false);
    samples[2].battery_heater = Some(false);
    samples[3].battery_heater_no_power = Some(true);
    assert_eq!(session_efficiency_wh_per_km(&samples), Some(150.0));
}

#[test]
fn a_heater_on_a_sample_without_range_between_the_ends_still_counts() {
    // The range runs from the first to the last window sample, whatever the
    // sample in between reports, so a heating reading on it is inside.
    let mut samples = t50();
    samples[2].rated_range_km = None;
    samples[2].battery_heater_on = Some(true);
    assert_eq!(session_efficiency_wh_per_km(&samples), None);
}

#[test]
fn t54_a_counter_decrease_between_window_samples_gives_no_figure() {
    let mut samples = t50();
    samples[3].charge_energy_added_kwh = Some(5.00);
    assert_eq!(session_efficiency_wh_per_km(&samples), None);
}

#[test]
fn a_counter_that_stays_equal_is_not_a_decrease() {
    let mut samples = t50();
    samples[2].charge_energy_added_kwh = Some(6.00);
    assert_eq!(session_efficiency_wh_per_km(&samples), Some(150.0));
}

#[test]
fn t55_without_rated_range_there_is_no_figure_even_with_ideal_range() {
    let mut samples = t50();
    for sample in &mut samples {
        sample.ideal_range_km = sample.rated_range_km.take();
    }
    assert_eq!(session_efficiency_wh_per_km(&samples), None);
}

/// T50 with 250 km at 60 %, so that a window starting at 60 % (18.00 kWh over
/// 110 km) differs from one starting at 50 % (24.00 kWh over 160 km, 150.0).
fn t50_skewed() -> Vec<ProjectionChargeSample> {
    let mut samples = t50();
    samples[1].rated_range_km = Some(250.00);
    samples
}

#[test]
fn samples_missing_a_level_or_a_counter_are_not_window_samples() {
    assert_eq!(session_efficiency_wh_per_km(&t50_skewed()), Some(150.0));
    let from_sixty = Some((1000.0 * 18.0) / 110.0);
    let mut samples = t50_skewed();
    samples[0].battery_level = None;
    assert_eq!(session_efficiency_wh_per_km(&samples), from_sixty);
    let mut samples = t50_skewed();
    samples[0].rated_range_km = None;
    assert_eq!(session_efficiency_wh_per_km(&samples), from_sixty);
    let mut samples = t50_skewed();
    samples[0].charge_energy_added_kwh = None;
    assert_eq!(session_efficiency_wh_per_km(&samples), from_sixty);
    // A negative counter is not usable (R10) and a non-finite range is not
    // known.
    let mut samples = t50_skewed();
    samples[0].charge_energy_added_kwh = Some(-0.01);
    assert_eq!(session_efficiency_wh_per_km(&samples), from_sixty);
    let mut samples = t50_skewed();
    samples[0].rated_range_km = Some(f64::NAN);
    assert_eq!(session_efficiency_wh_per_km(&samples), from_sixty);
}

#[test]
fn fewer_than_two_window_samples_give_no_figure() {
    assert_eq!(session_efficiency_wh_per_km(&[]), None);
    assert_eq!(
        session_efficiency_wh_per_km(&[fig(1, 50, 200.00, 0.00)]),
        None
    );
    // Two samples, one above the window.
    assert_eq!(
        session_efficiency_wh_per_km(&[fig(1, 50, 200.00, 0.00), fig(2, 95, 380.00, 27.00)]),
        None
    );
}

#[test]
fn no_energy_or_no_range_gained_gives_no_figure() {
    let flat_energy = [fig(1, 50, 200.00, 5.00), fig(2, 70, 280.00, 5.00)];
    assert_eq!(session_efficiency_wh_per_km(&flat_energy), None);
    let flat_range = [fig(1, 50, 200.00, 0.00), fig(2, 70, 200.00, 12.00)];
    assert_eq!(session_efficiency_wh_per_km(&flat_range), None);
    let range_falls = [fig(1, 50, 200.00, 0.00), fig(2, 70, 190.00, 12.00)];
    assert_eq!(session_efficiency_wh_per_km(&range_falls), None);
    let level_falls = [fig(1, 70, 200.00, 0.00), fig(2, 50, 280.00, 12.00)];
    assert_eq!(session_efficiency_wh_per_km(&level_falls), None);
}

#[test]
fn the_figure_does_not_depend_on_the_order_the_samples_are_given_in() {
    let mut shuffled = t50();
    shuffled.reverse();
    assert_eq!(session_efficiency_wh_per_km(&shuffled), Some(150.0));
    // Equal timestamps order by id.
    let mut same_time = t50();
    for sample in &mut same_time {
        sample.timestamp_ms = T0;
    }
    same_time.reverse();
    assert_eq!(session_efficiency_wh_per_km(&same_time), Some(150.0));
}

#[test]
fn the_figure_is_not_rounded_and_the_input_is_not_changed() {
    let samples = [fig(1, 50, 200.00, 0.00), fig(2, 90, 360.00, 24.32)];
    let before = samples.clone();
    assert_eq!(
        session_efficiency_wh_per_km(&samples),
        Some((1000.0 * 24.32) / 160.0)
    );
    assert_eq!(samples, before);
}

#[test]
fn the_car_figure_is_the_median_of_the_session_figures() {
    assert_eq!(
        car_efficiency_from_figures(&[150.0, 152.0, 149.0]),
        Some(150.0)
    );
    // An even count is the mean of the two middle values (T56's fourth).
    assert_eq!(
        car_efficiency_from_figures(&[150.0, 152.0, 149.0, 160.0]),
        Some(151.0)
    );
    // One outlier does not move it.
    assert_eq!(
        car_efficiency_from_figures(&[150.0, 150.0, 900.0]),
        Some(150.0)
    );
    // The order of the figures does not matter.
    assert_eq!(
        car_efficiency_from_figures(&[160.0, 149.0, 152.0, 150.0]),
        Some(151.0)
    );
}

#[test]
fn fewer_than_three_figures_give_no_car_figure() {
    assert_eq!(car_efficiency_from_figures(&[]), None);
    assert_eq!(car_efficiency_from_figures(&[150.0]), None);
    assert_eq!(car_efficiency_from_figures(&[150.0, 151.0]), None);
    assert!(car_efficiency_from_figures(&[150.0, 151.0, 152.0]).is_some());
}

#[test]
fn the_car_figure_is_rounded_to_a_tenth() {
    assert_eq!(
        car_efficiency_from_figures(&[150.04, 150.04, 150.04]),
        Some(150.0)
    );
    assert_eq!(
        car_efficiency_from_figures(&[150.06, 150.06, 150.06]),
        Some(150.1)
    );
    // The median of an even count is taken before rounding.
    assert_eq!(
        car_efficiency_from_figures(&[150.0, 150.0, 150.09, 150.09]),
        Some(150.0)
    );
}
