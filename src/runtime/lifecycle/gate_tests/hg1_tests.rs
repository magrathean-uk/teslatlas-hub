// SPDX-License-Identifier: AGPL-3.0-only

//! HG1 spec tests (C2 section 17.1, C2 R74; C2 T70 and C2 T71): the gate fold equals the
//! Hub's own functions, keeps at most eight type texts, and the shared phase
//! set gives C2 R34's set from the one place that computes it. Synthetic data
//! only.

use super::fold::K_GATE_TYPE_TEXTS;
use super::support::*;
use super::*;

/// Assert two figures are the same bits (bit for bit, as the spec states).
fn same_bits(got: Option<f64>, want: Option<f64>, label: &str) {
    assert_eq!(got.map(f64::to_bits), want.map(f64::to_bits), "{label}");
}

/// The samples of C2 T9, C2 T11, C2 T12, C2 T17 (both variants), C2 T17c (both variants), C2 T18
/// and C2 T21, labelled.
fn t70_cases() -> Vec<(&'static str, Vec<ProjectionChargeSample>)> {
    let t9 = vec![
        b(0).a7().c(0.00).s(),
        b(10).a7().c(1.08).s(),
        b(20).a7().c(2.16).s(),
        b(30).a7().c(3.24).s(),
    ];
    let t11 = vec![
        b(0).a7().c(0.00).s(),
        b(10).a7().c(1.08).s(),
        b(20).a7().c(2.16).s(),
        b(180).power(0.0).volts(240.0).amps(0.0).c(20.00).s(),
    ];
    let t12 = vec![
        b(0).a7().c(0.40).s(),
        b(10).a7().c(1.48).s(),
        b(20).a7().c(2.56).s(),
    ];
    let counters = [0.00, 0.22, 0.44, 0.66, 0.88, 1.10, 1.32];
    let t17_flag = counters
        .iter()
        .enumerate()
        .map(|(k, c)| b(10 * k as i64).power(1.0).present(false).c(*c).s())
        .collect::<Vec<_>>();
    let t17_electrical = counters
        .iter()
        .enumerate()
        .map(|(k, c)| {
            b(10 * k as i64)
                .power(1.0)
                .volts(120.0)
                .amps(12.0)
                .phases(1)
                .c(*c)
                .s()
        })
        .collect::<Vec<_>>();
    let t17c = vec![
        b(0).power(40.0).c(0.00).s(),
        b(2).power(30.0).c(1.25).s(),
        b(4).power(20.0).c(2.20).s(),
    ];
    let t17c_ac = t17c
        .iter()
        .cloned()
        .map(|mut sample| {
            sample.fast_charger_present = Some(false);
            sample
        })
        .collect::<Vec<_>>();
    let t18 = vec![
        b(0).dc(150.0)
            .present(true)
            .kind("Supercharger")
            .c(0.00)
            .s(),
        b(2).dc(120.0)
            .present(true)
            .kind("Supercharger")
            .c(5.00)
            .s(),
        b(4).dc(90.0).present(true).kind("Supercharger").c(9.00).s(),
    ];
    let t21 = vec![
        b_ms(1, T0).a7().c(0.00).s(),
        b_ms(2, T0).a7().c(0.00).s(),
        b(10).a7().c(1.08).s(),
        b(20).a7().c(2.16).s(),
    ];
    vec![
        ("T9", t9),
        ("T11", t11),
        ("T12", t12),
        ("T17 flag", t17_flag),
        ("T17 electrical", t17_electrical),
        ("T17c", t17c),
        ("T17c AC", t17c_ac),
        ("T18", t18),
        ("T21", t21),
    ]
}

/// C2 T50's samples at `T0 + 10 min x k`, ids `k + 1`.
fn t50() -> Vec<ProjectionChargeSample> {
    vec![
        fig(0, 50, 200.00, 0.00),
        fig(1, 60, 240.00, 6.00),
        fig(2, 70, 280.00, 12.00),
        fig(3, 80, 320.00, 18.00),
        fig(4, 90, 360.00, 24.00),
        fig(5, 95, 380.00, 27.00),
    ]
}

/// Efficiency cases in sample order with their stated figure (C2 T50, C2 T52 to C2 T55
/// and C2 T66 (b) to (g)).
fn efficiency_cases() -> Vec<(&'static str, Vec<ProjectionChargeSample>, Option<f64>)> {
    let with = |edit: &dyn Fn(&mut Vec<ProjectionChargeSample>)| {
        let mut samples = t50();
        edit(&mut samples);
        samples
    };
    let ids_by_time = |mut samples: Vec<ProjectionChargeSample>| {
        samples.sort_by_key(|sample| (sample.timestamp_ms, sample.id));
        samples
    };
    vec![
        ("T50", t50(), Some(150.0)),
        (
            "T52",
            with(&|s| s.push(fig(6, 99, 300.00, 40.00))),
            Some(150.0),
        ),
        (
            "T53 heater at 70",
            with(&|s| s[2].battery_heater_on = Some(true)),
            None,
        ),
        (
            "T53 heater at 95",
            with(&|s| s[5].battery_heater_on = Some(true)),
            Some(150.0),
        ),
        (
            "T54",
            with(&|s| s[3].charge_energy_added_kwh = Some(5.00)),
            None,
        ),
        (
            "T55",
            with(&|s| {
                for sample in s.iter_mut() {
                    sample.ideal_range_km = sample.rated_range_km.take();
                }
            }),
            None,
        ),
        (
            "T66 (b) heater before A",
            ids_by_time(with(&|s| {
                let mut early = b_ms(100, T0 - 10 * MIN).level(45).c(0.00).s();
                early.battery_heater_on = Some(true);
                s.push(early);
            })),
            Some(150.0),
        ),
        (
            "T66 (c) heater between A and B on a non-window sample",
            ids_by_time(with(&|s| {
                let mut between = b_ms(101, T0 + 25 * MIN).level(75).c(15.00).s();
                between.battery_heater = Some(true);
                s.push(between);
            })),
            None,
        ),
        (
            "T66 (d) heater on A",
            with(&|s| s[0].battery_heater = Some(true)),
            None,
        ),
        (
            "T66 (d) heater on B",
            with(&|s| s[4].battery_heater = Some(true)),
            None,
        ),
        (
            "T66 (d) heater after B",
            with(&|s| s[5].battery_heater = Some(true)),
            Some(150.0),
        ),
        (
            "T66 (e) equal times, level 70 first",
            {
                // Level 60 and level 70 both at t0 + 10 min: level 70 has id 2.
                let mut samples = t50();
                samples[1].timestamp_ms = T0 + 10 * MIN;
                samples[2].timestamp_ms = T0 + 10 * MIN;
                samples[1].id = 3;
                samples[2].id = 2;
                ids_by_time(samples)
            },
            None,
        ),
        (
            "T66 (e) equal times, level 60 first",
            {
                let mut samples = t50();
                samples[2].timestamp_ms = T0 + 10 * MIN;
                ids_by_time(samples)
            },
            Some(150.0),
        ),
        ("T66 (f) one window sample", t50().split_off(4), None),
        (
            "T66 (g) only window samples are compared",
            ids_by_time(with(&|s| {
                s.push(b_ms(102, T0 + 35 * MIN).level(85).c(1.00).s());
            })),
            Some(150.0),
        ),
    ]
}

#[test]
fn c2_t70_fold_equals_hub() {
    // (a) The energy and the scratch state equal the Hub's functions, bit for bit.
    for (label, samples) in t70_cases() {
        let facts = fold_all(&samples);
        assert_eq!(
            facts.energy,
            charge_energy_from_samples(&samples, None),
            "{label}: energy"
        );
        let mut scratch = OpenCharge::empty(0, 0, 0);
        for sample in &samples {
            observe_charge_sample(&mut scratch, sample);
        }
        assert_eq!(facts.is_dc, scratch.is_dc, "{label}: is_dc");
        assert_eq!(
            facts.session_type, scratch.fast_charger_type,
            "{label}: type"
        );
        same_bits(facts.used_powered_kwh, scratch.energy_used_kwh, label);
        assert_eq!(
            facts.ratio_used_kwh.to_bits(),
            scratch.ratio_used_kwh.to_bits(),
            "{label}: U_r"
        );
        assert_eq!(
            facts.ratio_added_kwh.to_bits(),
            scratch.ratio_added_kwh.to_bits(),
            "{label}: A_r"
        );
        assert_eq!(
            facts.fully_powered, scratch.fully_powered,
            "{label}: powered"
        );
    }
    // The stated totals pin that the fold is not comparing a function to itself.
    let cases = t70_cases();
    let facts = |label: &str| {
        let (_, samples) = cases.iter().find(|(name, _)| *name == label).unwrap();
        fold_all(samples)
    };
    assert_eq!(facts("T9").energy.added_kwh, Some(3.24));
    assert_eq!(facts("T9").energy.used_kwh, Some(3.60));
    assert_eq!(facts("T18").energy.used_kwh, Some(8.00));
    assert_eq!(facts("T18").is_dc, Some(true));
    // (b) C2 T18's and C2 T11's timing and counter facts.
    let t18 = facts("T18");
    assert_eq!(t18.samples, 3);
    assert_eq!(t18.intervals, 2);
    assert_eq!(t18.interval_ms_sum, 240_000);
    assert_eq!(t18.long_intervals, 0);
    assert_eq!(t18.max_power_kw, Some(150.0));
    assert_eq!(t18.powered_samples, 3);
    assert_eq!(t18.powered_off_ac_voltage, 3);
    assert_eq!(t18.dc_name_samples, 3);
    assert_eq!(t18.type_values, vec![("Supercharger".to_owned(), 3)]);
    assert_eq!(t18.usable_counters, 3);
    assert_eq!(t18.recovered_decreases, 0);
    assert_eq!(t18.first_sample_ms, Some(T0));
    assert_eq!(t18.last_sample_ms, Some(T0 + 4 * MIN));
    let t11 = facts("T11");
    assert_eq!(t11.intervals, 3);
    assert_eq!(t11.interval_ms_sum, 10_800_000);
    assert_eq!(t11.long_intervals, 1);
    // Zero-length intervals are not counted (C2 R17): C2 T21 has two, not three.
    assert_eq!(facts("T21").intervals, 2);
    // The counter points carry the sample's usable power.
    let first = t18.first_counter.expect("first counter");
    assert_eq!(first.counter_kwh, 0.00);
    assert_eq!(first.power_kw, Some(150.0));
    assert_eq!(t18.second_counter.expect("second").counter_kwh, 5.00);
    assert_eq!(t18.last_counter.expect("last").counter_kwh, 9.00);
    // Temperatures: the f64 sum of the known values over their count.
    let temps = [b(0).temp(10.5).s(), b(1).s(), b(2).temp(-1.5).s()];
    assert_eq!(
        fold_all(&temps).outside_temp_mean_c,
        Some((10.5 + -1.5) / 2.0)
    );
    assert_eq!(fold_all(&[b(0).s()]).outside_temp_mean_c, None);
    // (c) The efficiency figure equals the Hub's, bit for bit, and the stated
    // value, with the samples fed in sample order and the function given them
    // in reverse.
    for (label, samples, want) in efficiency_cases() {
        let mut reversed = samples.clone();
        reversed.reverse();
        let facts = fold_all(&samples);
        same_bits(facts.efficiency_wh_per_km, want, label);
        same_bits(
            facts.efficiency_wh_per_km,
            session_efficiency_wh_per_km(&reversed),
            label,
        );
    }
}

#[test]
fn c2_t70_type_text_cap() {
    // (d) Nine samples with the types V1 to V9: eight kept, overflow one.
    let nine = (1..=9)
        .map(|k| b(k).kind(&format!("V{k}")).s())
        .collect::<Vec<_>>();
    let facts = fold_all(&nine);
    assert_eq!(facts.type_values.len(), K_GATE_TYPE_TEXTS);
    assert_eq!(facts.type_values.len(), 8);
    let kept = (1..=8).map(|k| (format!("V{k}"), 1)).collect::<Vec<_>>();
    assert_eq!(facts.type_values, kept);
    assert_eq!(facts.type_values_overflow, 1);
    // Texts are trimmed, an empty text is no value, a kept text counts every
    // sample, and each sample of a text that was not kept overflows.
    let more = nine
        .iter()
        .cloned()
        .chain([
            b(10).kind(" V1 ").s(),
            b(11).kind("   ").s(),
            b(12).kind("V9").s(),
            b(13).kind("V9").s(),
        ])
        .collect::<Vec<_>>();
    let facts = fold_all(&more);
    assert_eq!(facts.type_values[0], ("V1".to_owned(), 2));
    assert_eq!(facts.type_values.len(), 8);
    assert_eq!(facts.type_values_overflow, 3);
    // The session type itself is the Hub's C2 R32 result: the first informative text.
    assert_eq!(facts.session_type.as_deref(), Some("V1"));
}

#[test]
fn c2_t71_phase_sets() {
    // (a) The tuples (V, I, P) and their sets.
    let sample = |v: Option<f64>, i: Option<f64>, p: Option<f64>| {
        let mut sample = b(0).s();
        sample.charger_voltage = v;
        sample.charger_actual_current = i;
        sample.charger_power_kw = p;
        sample
    };
    let set = |one, two, three| Some(PhaseSet { one, two, three });
    let cases = [
        (
            sample(Some(230.0), Some(16.0), Some(11.0)),
            set(false, false, true),
        ),
        (
            sample(Some(240.0), Some(32.0), Some(8.0)),
            set(true, false, false),
        ),
        (
            sample(Some(230.0), Some(6.0), Some(4.0)),
            set(false, true, true),
        ),
        (
            sample(Some(230.0), Some(16.0), Some(9.0)),
            set(false, true, true),
        ),
        (
            sample(Some(230.0), Some(16.0), Some(20.0)),
            set(false, false, false),
        ),
        (sample(Some(400.0), Some(16.0), Some(11.0)), None),
        (sample(Some(230.0), Some(90.0), Some(11.0)), None),
        (sample(Some(230.0), Some(0.0), Some(0.0)), None),
        (sample(None, Some(16.0), Some(11.0)), None),
        (sample(Some(230.0), Some(16.0), None), None),
        (sample(Some(230.0), Some(16.0), Some(-3.0)), None),
    ];
    for (index, (sample, want)) in cases.iter().enumerate() {
        assert_eq!(phase_consistency_set(sample), *want, "T71 (a) case {index}");
    }
    // The input bounds are inclusive at 90 and 305 V and at 80 A, and exclusive
    // beyond them (C2 R33 step 3); a non-finite reading is no reading.
    for (v, i, defined) in [
        (90.0, 16.0, true),
        (89.9, 16.0, false),
        (305.0, 16.0, true),
        (305.1, 16.0, false),
        (230.0, 80.0, true),
        (230.0, 80.1, false),
        (230.0, f64::NAN, false),
        (f64::INFINITY, 16.0, false),
    ] {
        let probe = sample(Some(v), Some(i), Some(11.0));
        assert_eq!(
            phase_consistency_set(&probe).is_some(),
            defined,
            "V {v} I {i}"
        );
    }
    assert_eq!(
        phase_consistency_set(&sample(Some(230.0), Some(16.0), Some(f64::NAN))),
        None
    );
    // C2 T22 passes unchanged: the power selection reads its set from the same place.
    let row = |v, i, p, r: Option<i64>, session: Option<i64>| {
        let mut probe = sample(Some(v), Some(i), Some(p));
        probe.charger_phases = r;
        sample_grid_power_kw(&probe, None, session)
    };
    let power = row(230.0, 16.0, 11.0, Some(3), None);
    assert_eq!((power.power_kw, power.phase_count), (Some(11.04), Some(3)));
    let power = row(240.0, 32.0, 8.0, Some(2), None);
    assert_eq!((power.power_kw, power.phase_count), (Some(7.68), Some(1)));
    let power = row(230.0, 6.0, 4.0, Some(3), None);
    assert_eq!((power.power_kw, power.phase_count), (Some(4.14), Some(3)));
    let power = row(230.0, 6.0, 4.0, None, Some(3));
    assert_eq!((power.power_kw, power.phase_count), (Some(4.14), None));
    let power = row(230.0, 6.0, 4.0, None, None);
    assert_eq!((power.power_kw, power.phase_count), (Some(4.0), None));
    let power = row(400.0, 16.0, 11.0, Some(3), None);
    assert_eq!((power.power_kw, power.phase_count), (Some(11.0), None));
    // C2 R31 reads its AC test from the same set: exactly one consistent count.
    let evidence = |v, i, p| sample_kind_evidence(&sample(Some(v), Some(i), Some(p)));
    assert_eq!(
        evidence(230.0, 16.0, 11.0),
        Some(false),
        "a unique set is AC"
    );
    assert_eq!(
        evidence(230.0, 6.0, 4.0),
        None,
        "two counts are no evidence"
    );
    assert_eq!(
        evidence(230.0, 16.0, 20.0),
        None,
        "an empty set is no evidence"
    );
    assert_eq!(evidence(230.0, 0.5, 0.0), None, "below one kW is no flow");
}

#[test]
fn c2_t71_gate_counts() {
    // (b) One session, samples a minute apart, (P, V, I, r).
    let with = |k: i64, p: Option<f64>, v: f64, i: f64, r: i64| {
        let mut sample = b(k).volts(v).amps(i).phases(r).s();
        sample.charger_power_kw = p;
        sample
    };
    let samples = [
        with(0, Some(11.0), 230.0, 16.0, 3),
        with(1, Some(8.0), 240.0, 32.0, 2),
        with(2, Some(9.0), 230.0, 16.0, 3),
        with(3, Some(20.0), 230.0, 16.0, 3),
        with(4, Some(4.0), 230.0, 6.0, 3),
        with(5, None, 230.0, 16.0, 3),
    ];
    let facts = fold_all(&samples);
    assert_eq!(
        facts.phase_eligible, 4,
        "the fifth is 1,380 VA, the sixth has no power"
    );
    assert_eq!(facts.phase_unique, 2);
    assert_eq!(facts.phase_multi, 1);
    assert_eq!(facts.phase_empty, 1);
    assert_eq!(
        facts.phase_label_differs, 1,
        "the second: {{1}} against r 2"
    );
    assert_eq!(facts.powered_samples, 5);
    assert_eq!(facts.powered_off_ac_voltage, 0);
    // The 3 kW edge is inclusive: 240 V x 12.5 A is exactly 3,000 VA.
    let edge = fold_all(&[b(0).power(3.0).volts(240.0).amps(12.5).s()]);
    assert_eq!(edge.phase_eligible, 1);
    let under = fold_all(&[b(0).power(3.0).volts(240.0).amps(12.4).s()]);
    assert_eq!(under.phase_eligible, 0);
}
