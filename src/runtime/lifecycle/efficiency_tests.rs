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

// ------------------------------------------------ C2 B2 amendment B (R71)

/// The R62 steps 1 to 5 written out over a whole ordered slice, as the
/// definition states them: the oracle the fold is compared with.
fn reference_figure(samples: &[ProjectionChargeSample]) -> Option<f64> {
    let mut ordered = samples.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|sample| (sample.timestamp_ms, sample.id));
    let in_window = |sample: &ProjectionChargeSample| {
        sample.battery_level.is_some_and(|level| level <= 90)
            && sample.rated_range_km.is_some_and(f64::is_finite)
            && sample
                .charge_energy_added_kwh
                .is_some_and(|counter| counter.is_finite() && counter >= 0.0)
    };
    let first = ordered.iter().position(|sample| in_window(sample))?;
    let last = ordered.iter().rposition(|sample| in_window(sample))?;
    if first == last {
        return None;
    }
    let span = &ordered[first..=last];
    if span
        .iter()
        .any(|sample| sample.battery_heater_on == Some(true) || sample.battery_heater == Some(true))
    {
        return None;
    }
    let counters = span
        .iter()
        .filter(|sample| in_window(sample))
        .map(|sample| sample.charge_energy_added_kwh.unwrap())
        .collect::<Vec<_>>();
    if counters.windows(2).any(|pair| pair[1] < pair[0]) {
        return None;
    }
    let (a, b) = (ordered[first], ordered[last]);
    let level_span = b.battery_level? - a.battery_level?;
    let energy_added = b.charge_energy_added_kwh? - a.charge_energy_added_kwh?;
    let range_gained = b.rated_range_km? - a.rated_range_km?;
    if level_span < 10 || energy_added <= 0.0 || range_gained <= 0.0 {
        return None;
    }
    let figure = (1000.0 * energy_added) / range_gained;
    (figure.is_finite() && figure > 0.0).then_some(figure)
}

/// Feed the fold one sample at a time in sample order and finish.
fn folded(samples: &[ProjectionChargeSample]) -> Option<f64> {
    let mut ordered = samples.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|sample| (sample.timestamp_ms, sample.id));
    let mut fold = EfficiencyFold::new();
    for sample in ordered {
        fold.observe(sample);
    }
    fold.finish()
}

/// `sample` at `T0` plus `minutes` with the given id.
fn placed(mut sample: ProjectionChargeSample, id: i64, minutes: i64) -> ProjectionChargeSample {
    sample.id = id;
    sample.timestamp_ms = T0 + minutes * 60_000;
    sample
}

/// T50's six samples at `T0 + 10 min * k` with ids `k + 1`.
fn t66_base() -> Vec<ProjectionChargeSample> {
    t50()
        .into_iter()
        .enumerate()
        .map(|(k, sample)| placed(sample, k as i64 + 1, 10 * k as i64))
        .collect()
}

/// A case checks the fold against the stated value, bit for bit against the
/// slice function given the samples in reverse order, and against the oracle.
fn t66_case(samples: &[ProjectionChargeSample], expected: Option<f64>, label: &str) {
    assert_eq!(folded(samples), expected, "{label}");
    let mut reversed = samples.to_vec();
    reversed.reverse();
    assert_eq!(
        session_efficiency_wh_per_km(&reversed).map(f64::to_bits),
        folded(samples).map(f64::to_bits),
        "{label}: the slice function"
    );
    assert_eq!(
        reference_figure(samples).map(f64::to_bits),
        folded(samples).map(f64::to_bits),
        "{label}: the definition"
    );
}

#[test]
fn t66_a_the_inputs_of_t50_to_t55_give_their_stated_values() {
    // T50.
    t66_case(&t66_base(), Some(150.0), "T50");
    // T51: a span under ten points.
    let t51 = vec![
        placed(fig(1, 80, 300.00, 0.00), 1, 0),
        placed(fig(2, 89, 336.00, 5.40), 2, 10),
    ];
    t66_case(&t51, None, "T51");
    // T52: a sample above the window is ignored.
    let mut t52 = t66_base();
    t52.push(placed(fig(7, 99, 300.00, 40.00), 7, 60));
    t66_case(&t52, Some(150.0), "T52");
    // T53: heater at level 70 gives None; only at level 95, 150.0.
    let mut t53 = t66_base();
    t53[2].battery_heater_on = Some(true);
    t66_case(&t53, None, "T53 heater at 70");
    let mut t53b = t66_base();
    t53b[5].battery_heater_on = Some(true);
    t66_case(&t53b, Some(150.0), "T53 heater at 95");
    // T54: a counter decrease between window samples.
    let mut t54 = t66_base();
    t54[3].charge_energy_added_kwh = Some(5.00);
    t66_case(&t54, None, "T54");
    // T55: no rated range.
    let mut t55 = t66_base();
    for sample in &mut t55 {
        sample.rated_range_km = None;
    }
    t66_case(&t55, None, "T55");
}

#[test]
fn t66_b_to_d_a_heater_counts_from_the_first_window_sample_to_the_last() {
    // (b) A heater before A does not count.
    let mut before = t66_base();
    let mut early = placed(bare(100), 100, -10);
    early.battery_level = Some(45);
    early.charge_energy_added_kwh = Some(0.00);
    early.battery_heater_on = Some(true);
    before.push(early);
    t66_case(&before, Some(150.0), "(b) heater before A");
    // (c) A heater between A and B on a sample that is not a window sample.
    let mut between = t66_base();
    let mut middle = placed(bare(101), 101, 25);
    middle.battery_level = Some(75);
    middle.charge_energy_added_kwh = Some(15.00);
    middle.battery_heater = Some(true);
    between.push(middle);
    t66_case(&between, None, "(c) heater between");
    // (d) On A only, on B only, and on the level-95 sample only.
    let mut on_a = t66_base();
    on_a[0].battery_heater = Some(true);
    t66_case(&on_a, None, "(d) on A");
    let mut on_b = t66_base();
    on_b[4].battery_heater = Some(true);
    t66_case(&on_b, None, "(d) on B");
    let mut after = t66_base();
    after[5].battery_heater = Some(true);
    t66_case(&after, Some(150.0), "(d) after B");
}

#[test]
fn t66_e_to_g_equal_times_one_window_sample_and_only_window_samples_compared() {
    // (e) The level-60 and level-70 samples at the same time, the level-70
    // sample with id 2 and the level-60 sample with id 3: sample order puts
    // c 12.00 before c 6.00 between window samples.
    let mut swapped = t66_base();
    swapped[2] = placed(swapped[2].clone(), 2, 10);
    swapped[1] = placed(swapped[1].clone(), 3, 10);
    t66_case(&swapped, None, "(e) decrease in sample order");
    let mut ordered = t66_base();
    ordered[1] = placed(ordered[1].clone(), 2, 10);
    ordered[2] = placed(ordered[2].clone(), 3, 10);
    t66_case(&ordered, Some(150.0), "(e) ids swapped back");
    // (f) One window sample: the level-90 and level-95 samples only.
    let base = t66_base();
    t66_case(&[base[4].clone(), base[5].clone()], None, "(f)");
    // (g) A sample without rated range is not a window sample, so its lower
    // counter is not compared.
    let mut lower = t66_base();
    let mut stray = placed(bare(102), 102, 35);
    stray.battery_level = Some(85);
    stray.charge_energy_added_kwh = Some(1.00);
    lower.push(stray);
    t66_case(&lower, Some(150.0), "(g)");
}

#[test]
fn t66_h_the_fold_equals_the_definition_on_generated_sequences() {
    // A deterministic generator over short sequences with every feature that
    // can change a figure: levels around the window edges, missing readings,
    // counter steps down, heater flags and equal times.
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = move |bound: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % bound
    };
    let (mut with_figure, mut without_figure) = (0, 0);
    for round in 0..20_000 {
        let length = next(9) as usize;
        let mut samples = Vec::new();
        let mut level = 40 + next(15) as i64;
        let mut counter = 0.0_f64;
        let mut rated = 150.0_f64;
        for index in 0..length {
            let mut sample = bare(index as i64 + 1);
            sample.timestamp_ms = T0 + (index as i64 / (1 + next(2) as i64)) * 60_000;
            level += next(9) as i64 - 1;
            counter += (next(7) as f64 - 1.0) * 1.37;
            rated += (next(9) as f64 - 2.0) * 8.0;
            sample.battery_level = (next(12) != 0).then_some(level.clamp(0, 100));
            sample.rated_range_km = match next(14) {
                0 => None,
                1 => Some(f64::NAN),
                _ => Some(rated),
            };
            sample.charge_energy_added_kwh = match next(14) {
                0 => None,
                1 => Some(-0.5),
                _ => Some(counter.max(0.0)),
            };
            sample.battery_heater_on = match next(12) {
                0 => Some(true),
                1 => Some(false),
                _ => None,
            };
            sample.battery_heater = (next(25) == 0).then_some(true);
            samples.push(sample);
        }
        assert_eq!(
            folded(&samples).map(f64::to_bits),
            reference_figure(&samples).map(f64::to_bits),
            "round {round}: {samples:?}"
        );
        if folded(&samples).is_some() {
            with_figure += 1;
        } else {
            without_figure += 1;
        }
    }
    // The generator reaches both outcomes often enough to mean something.
    assert!(
        with_figure > 500 && without_figure > 500,
        "{with_figure} {without_figure}"
    );
}
