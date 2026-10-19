// SPDX-License-Identifier: AGPL-3.0-only

//! HG2 spec tests (C2 section 17.1, C2 R75 (c); C2 T72 to C2 T76): the pure gate
//! report under its baseline semantics. Synthetic data only. Sessions are those
//! of car 1; a session's start time orders it.

use super::report::{
    ChargeGateReport, ProcessGateFacts, QuarterFigures, SessionClass, SiteClass, SiteOperator,
    charge_gate_report,
};
use super::support::*;
use super::*;

const TOLERANCE: f64 = 1e-9;

fn report_of(processes: &[ProcessGateFacts]) -> ChargeGateReport {
    charge_gate_report(processes, 1, None, &[])
}

fn close(got: Option<f64>, want: f64, label: &str) {
    let got = got.unwrap_or_else(|| panic!("{label}: None"));
    assert!(
        (got - want).abs() < TOLERANCE,
        "{label}: got {got}, want {want}"
    );
}

fn class_of(report: &ChargeGateReport, process_id: i64) -> SessionClass {
    report
        .classes
        .iter()
        .find(|(id, _)| *id == process_id)
        .map(|(_, class)| *class)
        .unwrap_or_else(|| panic!("process {process_id} missing"))
}

/// `count` AC samples a minute apart at 230 V and 16 A: the first `multi` of
/// them read 9 kW (a two-element phase set), the rest 11 kW (one element).
fn phase_session(process_id: i64, count: i64, multi: i64) -> ProcessGateFacts {
    let samples = (0..count)
        .map(|k| {
            let power = if k < multi { 9.0 } else { 11.0 };
            b(k).power(power).volts(230.0).amps(16.0).phases(3).s()
        })
        .collect::<Vec<_>>();
    process(process_id, T0 + process_id * DAY, &samples)
}

#[test]
fn c2_t72_classes() {
    let a_samples = [b(0).a7().s(), b(10).power(0.0).volts(240.0).amps(0.0).s()];
    let b_samples = [b(0).dc(120.0).s(), b(1).dc(90.0).s()];
    let c_samples = [b(0).dc(40.0).s(), b(1).dc(30.0).s()];
    let d_samples = [
        b(0).power(0.0).volts(230.0).amps(0.0).s(),
        b(1).power(0.0).volts(230.0).amps(0.0).s(),
    ];
    let e_sample = b(0).power(57.0).volts(300.0).amps(63.0).phases(3).s();
    let f_samples = [
        e_sample.clone(),
        b(1).power(58.0).volts(300.0).amps(63.0).phases(3).s(),
    ];
    let combo_on_ac = [b(0).a7().kind("Combo").s(), a_samples[1].clone()];
    let processes = [
        process(1, T0 + DAY, &a_samples),
        process(2, T0 + 2 * DAY, &b_samples),
        process(3, T0 + 3 * DAY, &c_samples),
        process(4, T0 + 4 * DAY, &d_samples),
        process(5, T0 + 5 * DAY, &[e_sample]),
        process(6, T0 + 6 * DAY, &f_samples),
        process(7, T0 + 7 * DAY, &combo_on_ac),
    ];
    let report = report_of(&processes);
    assert_eq!(report.sessions, 7);
    assert_eq!(class_of(&report, 1), SessionClass::AcSelected, "A");
    assert_eq!(class_of(&report, 2), SessionClass::DcSelected, "B");
    assert_eq!(
        class_of(&report, 3),
        SessionClass::Neither,
        "C: powered off the AC voltage range"
    );
    assert_eq!(
        class_of(&report, 4),
        SessionClass::Neither,
        "D: never powered"
    );
    assert_eq!(
        class_of(&report, 5),
        SessionClass::AcSelected,
        "E: 57 kW is AC"
    );
    assert_eq!(
        class_of(&report, 6),
        SessionClass::DcSelected,
        "F: 58 kW is DC"
    );
    assert_eq!(
        class_of(&report, 7),
        SessionClass::AcSelected,
        "Combo on AC"
    );
    // The Combo type on an AC-selected session: left out of G1, failing G6 (b).
    assert_eq!(report.g1.excluded_dc_name_sessions, vec![7]);
    assert_eq!(report.g6.ac_failures, vec![7]);
    assert!(!report.g6.b_pass);
    assert_eq!(
        report.g6.pass, None,
        "DC sessions without a brand need the owner"
    );
    // Another car's sessions are not this car's.
    let mut other = process(8, T0 + 8 * DAY, &a_samples);
    other.car_id = 2;
    let mixed = [processes[0].clone(), other];
    assert_eq!(charge_gate_report(&mixed, 1, None, &[]).sessions, 1);
    assert_eq!(
        charge_gate_report(&mixed, 2, Some(true), &[]).lfp_battery,
        Some(true)
    );
    // G1's minimum (200 samples from five sessions) and its 95 % line, on the
    // pooled share of single-element samples.
    let five = |extra_multi: i64| {
        (1..=5)
            .map(|id| phase_session(id, 40, 2 + i64::from(id == 1) * extra_multi))
            .collect::<Vec<_>>()
    };
    let at_line = report_of(&five(0)).g1;
    assert_eq!(
        (at_line.samples, at_line.unique, at_line.multi),
        (200, 190, 10)
    );
    assert_eq!(at_line.pass, Some(true), "190 of 200 is exactly 95 %");
    close(at_line.unique_share, 0.95, "share");
    close(at_line.multi_share, 0.05, "multi share");
    close(at_line.lowest_session_share, 38.0 / 40.0, "lowest");
    assert_eq!(
        report_of(&five(1)).g1.pass,
        Some(false),
        "189 of 200 is below it"
    );
    let few = five(0).into_iter().take(4).collect::<Vec<_>>();
    assert_eq!(
        report_of(&few).g1.pass,
        None,
        "four sessions are below the minimum"
    );
    let short = (1..=5)
        .map(|id| phase_session(id, 39, 0))
        .collect::<Vec<_>>();
    assert_eq!(
        report_of(&short).g1.pass,
        None,
        "195 samples are below the minimum"
    );
}

/// A session of ten usable counters (P 7) a minute apart, starting `id` days
/// after `T0`; `counters` are the ten values.
fn counter_session(id: i64, counters: &[f64; 10]) -> ProcessGateFacts {
    let start = T0 + id * DAY;
    let samples = counters
        .iter()
        .enumerate()
        .map(|(k, c)| {
            b_ms(k as i64 + 1, start + k as i64 * MIN)
                .power(7.0)
                .c(*c)
                .s()
        })
        .collect::<Vec<_>>();
    process(id, start, &samples)
}

/// `count` clean sessions of ten usable counters, one day apart.
fn clean_sessions(count: i64) -> Vec<ProcessGateFacts> {
    let counters = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
    (1..=count)
        .map(|id| counter_session(id, &counters))
        .collect()
}

/// Session P ends at `T0` with counter 20.00; session Q's two samples follow.
fn boundary(
    p_last: f64,
    q: [(i64, f64, Option<f64>); 2],
) -> (Vec<ProcessGateFacts>, ChargeGateReport) {
    let p_samples = [
        b_ms(1, T0 - 2 * MIN).power(11.0).c(p_last - 0.01).s(),
        b_ms(2, T0).power(11.0).c(p_last).s(),
    ];
    let q_samples = q.map(|(at_ms, counter, power)| {
        let mut sample = b_ms(1, at_ms).c(counter).s();
        sample.charger_power_kw = power;
        sample
    });
    let processes = vec![
        process(1, T0 - 2 * MIN, &p_samples),
        process(2, q[0].0, &q_samples),
    ];
    let report = report_of(&processes);
    (processes, report)
}

#[test]
fn c2_t73_baseline_glitches() {
    // Within a session: a decrease whose next usable counter is at least the
    // value before it is recovered.
    let counted = |counters: &[Option<f64>]| {
        let samples = counters
            .iter()
            .enumerate()
            .map(|(k, counter)| {
                let mut sample = b(k as i64).power(7.0).s();
                sample.charge_energy_added_kwh = *counter;
                sample
            })
            .collect::<Vec<_>>();
        fold_all(&samples).recovered_decreases
    };
    let some = |values: &[f64]| values.iter().map(|v| Some(*v)).collect::<Vec<_>>();
    assert_eq!(counted(&some(&[10.00, 10.50, 0.40, 10.60, 11.00])), 1);
    assert_eq!(counted(&some(&[10.00, 0.30, 4.00])), 0);
    assert_eq!(counted(&some(&[10.00, 5.00, 3.00, 12.00])), 1);
    assert_eq!(counted(&[Some(10.00), None, Some(0.40), Some(10.60)]), 1);
    // The report adds them up over the car's sessions.
    let recovered = [
        &[10.00, 10.50, 0.40, 10.60, 11.00][..],
        &[10.00, 5.00, 3.00, 12.00][..],
    ];
    let processes = recovered
        .iter()
        .enumerate()
        .map(|(index, counters)| {
            let samples = counters
                .iter()
                .enumerate()
                .map(|(k, c)| b(k as i64).power(7.0).c(*c).s())
                .collect::<Vec<_>>();
            process(index as i64 + 1, T0 + (index as i64 + 1) * DAY, &samples)
        })
        .collect::<Vec<_>>();
    assert_eq!(report_of(&processes).g4.recovered_decreases, 2);
    // Across sessions: P's last counter is 20.00 at t; Q follows at t + 5 min.
    let five = T0 + 5 * MIN;
    let six = T0 + 6 * MIN;
    // (a) The rise 20.00 exceeds (11 + 1) x 60,000 / 3,600,000 + 0.01 = 0.21.
    let (_, report) = boundary(20.00, [(five, 0.10, Some(11.0)), (six, 20.10, Some(11.0))]);
    assert_eq!(report.g4.boundary_glitches, vec![(1, 2)]);
    assert!(report.g4.undecided.is_empty());
    assert_eq!(
        (report.g4.consecutive_pairs, report.g4.pairs_within_bound),
        (1, 1)
    );
    // (b) The later session never reaches the earlier counter: not a glitch.
    let (_, report) = boundary(20.00, [(five, 0.00, Some(11.0)), (six, 0.19, Some(11.0))]);
    assert!(report.g4.boundary_glitches.is_empty() && report.g4.undecided.is_empty());
    // (c) Reaches 0.15, but the rise 0.19 is within 0.21.
    let (_, report) = boundary(0.15, [(five, 0.00, Some(11.0)), (six, 0.19, Some(11.0))]);
    assert!(report.g4.boundary_glitches.is_empty() && report.g4.undecided.is_empty());
    // (d) As (a) with no power on either sample: undecided.
    let (_, report) = boundary(20.00, [(five, 0.10, None), (six, 20.10, None)]);
    assert!(report.g4.boundary_glitches.is_empty());
    assert_eq!(report.g4.undecided, vec![(1, 2)]);
    // One usable power is enough to decide, and the larger of the two is used.
    let (_, report) = boundary(20.00, [(five, 0.10, None), (six, 20.10, Some(11.0))]);
    assert_eq!(report.g4.boundary_glitches, vec![(1, 2)]);
    // (e) Q's first sample 600,001 ms after P's last: the pair is not examined;
    // at exactly 600,000 ms it is.
    let late = T0 + 600_001;
    let (_, report) = boundary(
        20.00,
        [(late, 0.10, Some(11.0)), (late + MIN, 20.10, Some(11.0))],
    );
    assert!(report.g4.boundary_glitches.is_empty() && report.g4.undecided.is_empty());
    assert_eq!(
        (report.g4.consecutive_pairs, report.g4.pairs_within_bound),
        (1, 0)
    );
    let edge = T0 + 600_000;
    let (_, report) = boundary(
        20.00,
        [(edge, 0.10, Some(11.0)), (edge + MIN, 20.10, Some(11.0))],
    );
    assert_eq!(report.g4.boundary_glitches, vec![(1, 2)]);
    // (f) Fewer than 20 sessions of at least 10 usable counters: no pass even
    // when every count is zero; twenty clean ones pass; one recovery fails it.
    let nineteen = report_of(&clean_sessions(19)).g4;
    assert_eq!(nineteen.pass, None);
    assert_eq!(nineteen.recovered_decreases, 0);
    let twenty = report_of(&clean_sessions(20)).g4;
    assert_eq!(
        (twenty.sessions_with_counters, twenty.pass),
        (20, Some(true))
    );
    let mut dirty = clean_sessions(20);
    dirty[3] = counter_session(
        4,
        &[10.0, 10.5, 0.4, 10.6, 11.0, 11.5, 12.0, 12.5, 13.0, 13.5],
    );
    let dirty = report_of(&dirty).g4;
    assert_eq!((dirty.recovered_decreases, dirty.pass), (1, Some(false)));
}

/// C2 T9's samples with a mean outside temperature.
fn t9(temp: f64) -> Vec<ProjectionChargeSample> {
    t9_with(&[0.00, 1.08, 2.16, 3.24], Some(temp))
}

/// C2 T9's samples with the counters and the outside temperature given
/// (`None`: no temperature on any sample).
fn t9_with(counters: &[f64; 4], temp: Option<f64>) -> Vec<ProjectionChargeSample> {
    counters
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let sample = b(10 * k as i64).a7().c(*c);
            let sample = match temp {
                Some(temp) => sample.temp(temp),
                None => sample,
            };
            sample.s()
        })
        .collect()
}

/// Sessions with the given process ids, a day apart from `T0`, each folded from
/// `samples`.
fn sessions_of(
    ids: std::ops::RangeInclusive<i64>,
    samples: &[ProjectionChargeSample],
) -> Vec<ProcessGateFacts> {
    ids.map(|id| process(id, T0 + id * DAY, samples)).collect()
}

/// Session L: seven samples 10 min apart `{P 1, V 240, I 3, r 1, present false}`
/// with c 0.00 to 1.32 in steps of 0.22 (C2 R35 row 2: 0.72 kW).
fn session_l(temp: f64) -> Vec<ProjectionChargeSample> {
    (0..7)
        .map(|k| {
            b(10 * k)
                .power(1.0)
                .volts(240.0)
                .amps(3.0)
                .phases(1)
                .present(false)
                .c(0.22 * k as f64)
                .temp(temp)
                .s()
        })
        .collect()
}

fn t9_sessions(count: i64) -> Vec<ProcessGateFacts> {
    (1..=count)
        .map(|id| process(id, T0 + id * DAY, &t9(12.0)))
        .collect()
}

#[test]
fn c2_t74_ratio_gates() {
    // (a) G2: nine C2 T9 sessions (ratio 3.60 / 3.24 = 1.111) and L (ratio 0.545).
    let l = process(10, T0 + 10 * DAY, &session_l(12.0));
    assert_eq!(l.facts.is_dc, Some(false));
    close(Some(l.facts.ratio_used_kwh), 0.72, "L U_r");
    close(Some(l.facts.ratio_added_kwh), 1.32, "L A_r");
    let mut processes = t9_sessions(9);
    processes.push(l.clone());
    let report = report_of(&processes);
    assert_eq!(report.g2.sessions, 10);
    assert_eq!(
        report.g2.below_floor,
        vec![10],
        "1 of 10 is within the 10 % allowance"
    );
    close(report.g2.median, 3.60 / 3.24, "median");
    assert_eq!(report.g2.pass, Some(true));
    assert_eq!(report.g2.intervals.intervals, 9 * 3 + 6);
    assert_eq!(report.g2.intervals.long_intervals, 0);
    close(report.g2.intervals.mean_ms, 600_000.0, "mean interval");
    // A second L-shaped session: 2 of 11 below the floor, both listed.
    processes.push(process(11, T0 + 11 * DAY, &session_l(12.0)));
    let report = report_of(&processes);
    assert_eq!(report.g2.below_floor, vec![10, 11]);
    assert_eq!(report.g2.pass, Some(false));
    // The nine C2 T9 sessions alone are below the minimum.
    assert_eq!(report_of(&t9_sessions(9)).g2.pass, None);
    // Cold sessions stay out of the median (heating while charging is real
    // draw), and a session whose kind ends None is counted, not selected.
    let mut cold = t9_sessions(9);
    cold.push(process(10, T0 + 10 * DAY, &session_l(3.0)));
    // Voltage in range, but a power that two phase counts explain: AC-selected
    // with no kind evidence.
    let unknown = (0..7)
        .map(|k| {
            b(10 * k)
                .power(4.0)
                .volts(230.0)
                .amps(6.0)
                .c(0.22 * k as f64)
                .s()
        })
        .collect::<Vec<_>>();
    cold.push(process(12, T0 + 12 * DAY, &unknown));
    let report = report_of(&cold);
    assert_eq!(report.g2.warm_sessions, 9);
    assert_eq!(report.g2.kind_unknown_sessions, 1, "counted, not selected");
    assert_eq!(report.g2.sessions, 10);
    close(report.g2.warm_median, 3.60 / 3.24, "warm median of nine");
    assert_eq!(report.g2.pass, Some(true));
    // The median part of the pass is open while no selected session has a known
    // mean outside temperature of at least 10 degrees C, and an open part never
    // passes the gate (C2 section 11.1 G2: "the median part stays open"; a gate
    // that stays open is completed by Hub-recorded sessions, C2 section 11,
    // Minimums). Ten cold sessions (ratio 1.111, none below the floor) have no
    // warm median, so the pass is open.
    let report = report_of(&sessions_of(1..=10, &t9(3.0)));
    assert_eq!((report.g2.sessions, report.g2.warm_sessions), (10, 0));
    assert!(report.g2.below_floor.is_empty());
    close(report.g2.median, 3.60 / 3.24, "cold median");
    assert_eq!(report.g2.warm_median, None);
    assert_eq!(
        report.g2.pass, None,
        "ten cold sessions leave the median part open, not passed"
    );
    // Ten sessions with no temperature on any sample have no mean either.
    let unknown_temp = t9_with(&[0.00, 1.08, 2.16, 3.24], None);
    let report = report_of(&sessions_of(1..=10, &unknown_temp));
    assert_eq!((report.g2.sessions, report.g2.warm_sessions), (10, 0));
    assert!(report.g2.below_floor.is_empty());
    assert_eq!(report.g2.warm_median, None);
    assert_eq!(
        report.g2.pass, None,
        "ten sessions of unknown temperature leave the median part open"
    );
    // One warm session among nine cold ones gives the median part its value:
    // 1.111 is in the band, so the gate passes.
    let mut one_warm = sessions_of(1..=9, &t9(3.0));
    one_warm.push(process(10, T0 + 10 * DAY, &t9(12.0)));
    let report = report_of(&one_warm);
    assert_eq!((report.g2.sessions, report.g2.warm_sessions), (10, 1));
    close(report.g2.warm_median, 3.60 / 3.24, "single warm median");
    assert_eq!(report.g2.pass, Some(true));
    // A known warm median outside the band fails the gate (1.333 above 1.25).
    let steep = t9_with(&[0.00, 0.90, 1.80, 2.70], Some(12.0));
    let report = report_of(&sessions_of(1..=10, &steep));
    assert!(report.g2.below_floor.is_empty());
    close(report.g2.warm_median, 3.60 / 2.70, "steep warm median");
    assert_eq!(report.g2.pass, Some(false));
    // A failed floor decides the gate whatever the open median part is: eight
    // cold C2 T9 sessions and two cold L sessions, 2 of 10 below the floor.
    let mut floor_failed = sessions_of(1..=8, &t9(3.0));
    floor_failed.push(process(9, T0 + 9 * DAY, &session_l(3.0)));
    floor_failed.push(process(10, T0 + 10 * DAY, &session_l(3.0)));
    let report = report_of(&floor_failed);
    assert_eq!(report.g2.below_floor, vec![9, 10]);
    assert_eq!(report.g2.warm_median, None);
    assert_eq!(report.g2.pass, Some(false));
    // G3: three sessions of eleven samples a minute apart at 120 kW.
    let dc_session = |id: i64, step: f64| {
        let samples = (0..=10)
            .map(|k| b(k).dc(120.0).kind("Supercharger").c(step * k as f64).s())
            .collect::<Vec<_>>();
        process(id, T0 + id * DAY, &samples)
    };
    let three = [
        dc_session(1, 2.00),
        dc_session(2, 2.00),
        dc_session(3, 2.00),
    ];
    let report = report_of(&three);
    close(Some(three[0].facts.ratio_used_kwh), 20.0, "U_r");
    close(Some(three[0].facts.ratio_added_kwh), 20.0, "A_r");
    assert_eq!(report.g3.sessions, 3);
    close(report.g3.median, 1.0, "median");
    assert_eq!(report.g3.pass, Some(true));
    assert!(!report.g3.counter_only_branch);
    assert_eq!(
        report
            .g3
            .by_kind
            .iter()
            .map(|(kind, n, _)| (*kind, *n))
            .collect::<Vec<_>>(),
        vec![(FastChargerKind::Supercharger, 3)]
    );
    assert_eq!(report.g3.intervals.intervals, 30);
    // With c = 1.80 x k the median is 1.111 and the gate fails.
    let slow = [
        dc_session(1, 1.80),
        dc_session(2, 1.80),
        dc_session(3, 1.80),
    ];
    let report = report_of(&slow);
    close(report.g3.median, 20.0 / 18.0, "median");
    assert_eq!(report.g3.pass, Some(false));
    assert!(report.g3.counter_only_branch);
    // Two sessions: no pass, and the counter-only branch applies.
    let report = report_of(&three[..2]);
    assert_eq!(report.g3.pass, None);
    assert!(report.g3.counter_only_branch);
    // A session needs ten samples and 10 kWh counted to be selected.
    let short = (0..=8)
        .map(|k| b(k).dc(120.0).kind("Supercharger").c(2.5 * k as f64).s())
        .collect::<Vec<_>>();
    assert_eq!(report_of(&[process(1, T0, &short)]).g3.sessions, 0);
}

/// One AC session per day starting at `T0 + k days`, facts set directly.
fn daily(
    figures: impl Fn(i64) -> (f64, f64),
    days: std::ops::RangeInclusive<i64>,
) -> Vec<ProcessGateFacts> {
    days.map(|k| {
        let (figure, temp) = figures(k);
        direct_ac(k + 1_000, T0 + k * DAY, Some(figure), Some(temp))
    })
    .collect()
}

fn baseline_figure(k: i64) -> (f64, f64) {
    let figure = match k {
        ..=5 => 300.0,
        6..=14 => 150.0,
        _ => 160.0,
    };
    (figure, 10.0)
}

#[test]
fn c2_t75_efficiency_windows() {
    // (a) Spread window k = 6 to 34: 29 figures over 28 days.
    let report = report_of(&daily(baseline_figure, 0..=34));
    let g5 = &report.g5;
    assert_eq!(g5.figures, 35);
    assert_eq!(
        g5.spread_figures, 29,
        "0.0 would mean the window stopped at 20"
    );
    close(g5.spread_span_days, 28.0, "span");
    close(g5.q1, 150.0, "Q1");
    close(g5.q3, 160.0, "Q3");
    close(g5.median, 160.0, "median");
    close(g5.iqr_over_median, 0.0625, "IQR / median");
    assert!(g5.temperature_open);
    assert_eq!(g5.pass, Some(true));
    let window = (6..=34).map(|k| baseline_figure(k).0).collect::<Vec<_>>();
    assert_eq!(g5.median, car_efficiency_from_figures(&window));
    assert_eq!(
        g5.quarters,
        vec![QuarterFigures {
            year: 2025,
            quarter: 4,
            median: 160.0,
            count: 35
        }]
    );
    // (b) Ten figures 100 to 190, sessions four days apart.
    let ramp = |count: i64| {
        (0..count)
            .map(|k| {
                direct_ac(
                    k + 1,
                    T0 + 4 * k * DAY,
                    Some(100.0 + 10.0 * k as f64),
                    Some(10.0),
                )
            })
            .collect::<Vec<_>>()
    };
    let report = report_of(&ramp(10));
    close(report.g5.q1, 122.5, "Q1");
    close(report.g5.q3, 167.5, "Q3");
    close(report.g5.median, 145.0, "median");
    close(report.g5.iqr_over_median, 45.0 / 145.0, "IQR / median");
    assert_eq!(report.g5.pass, Some(false));
    // The first nine only: below the minimum of ten figures.
    assert_eq!(report_of(&ramp(9)).g5.pass, None);
    // Ten figures inside four weeks: below the minimum span.
    let tight = (0..10)
        .map(|k| direct_ac(k + 1, T0 + k * DAY, Some(150.0), Some(10.0)))
        .collect::<Vec<_>>();
    assert_eq!(report_of(&tight).g5.pass, None);
    // (c) k = 15 to 17 cold (2 C) and k = 18 to 20 warm (20 C).
    let seasonal = |cold_figure: f64, cold_days: std::ops::RangeInclusive<i64>| {
        move |k: i64| {
            let (figure, temp) = baseline_figure(k);
            if cold_days.contains(&k) {
                (cold_figure, 2.0)
            } else if (18..=20).contains(&k) {
                (figure, 20.0)
            } else {
                (figure, temp)
            }
        }
    };
    let report = report_of(&daily(seasonal(180.0, 15..=17), 0..=34));
    close(report.g5.iqr_over_median, 0.0625, "spread");
    close(report.g5.cold_median, 180.0, "cold");
    close(report.g5.warm_median, 160.0, "warm");
    assert!(!report.g5.temperature_open);
    assert_eq!(report.g5.pass, Some(false), "20.0 is more than 16.0");
    let (cold, warm) = (report.g5.cold_figures, report.g5.warm_figures);
    assert_eq!((cold, warm), (3, 3));
    // A difference of exactly 10 % of the lower median is within the bound.
    let report = report_of(&daily(seasonal(176.0, 15..=17), 0..=34));
    close(report.g5.cold_median, 176.0, "cold");
    assert_eq!(report.g5.pass, Some(true), "16.0 equals the bound");
    let report = report_of(&daily(seasonal(176.1, 15..=17), 0..=34));
    assert_eq!(report.g5.pass, Some(false), "16.1 is above it");
    // Only k = 15 and 16 cold: the temperature part stays open.
    let report = report_of(&daily(seasonal(180.0, 15..=16), 0..=34));
    assert!(report.g5.temperature_open);
    assert_eq!(report.g5.pass, Some(true));
    // A session at k = -400 (2 C, 300.0) is outside both windows, reported in
    // its own quarter, and changes no other result.
    let base = report_of(&daily(seasonal(180.0, 15..=17), 0..=34));
    let mut processes = daily(seasonal(180.0, 15..=17), 0..=34);
    processes.push(direct_ac(1, T0 - 400 * DAY, Some(300.0), Some(2.0)));
    let report = report_of(&processes);
    let mut quarters = base.g5.quarters.clone();
    quarters.insert(
        0,
        QuarterFigures {
            year: 2024,
            quarter: 3,
            median: 300.0,
            count: 1,
        },
    );
    assert_eq!(report.g5.quarters, quarters);
    let mut expected = base.g5.clone();
    expected.figures += 1;
    expected.quarters = quarters;
    assert_eq!(report.g5, expected);
    // Only AC-selected sessions of kind Some(false) with a known figure count.
    let mut dc_kind = direct_ac(5, T0, Some(10.0), Some(10.0));
    dc_kind.facts.is_dc = Some(true);
    let mut no_figure = direct_ac(6, T0, None, Some(10.0));
    no_figure.facts.efficiency_wh_per_km = None;
    assert_eq!(report_of(&[dc_kind, no_figure]).g5.figures, 0);
}

fn s1() -> Vec<ProjectionChargeSample> {
    (0..3)
        .map(|k| {
            b(2 * k)
                .dc(120.0)
                .present(true)
                .kind("Supercharger")
                .brand("Tesla")
                .s()
        })
        .collect()
}

fn s2() -> Vec<ProjectionChargeSample> {
    (0..3)
        .map(|k| {
            b(2 * k)
                .dc(100.0)
                .present(true)
                .kind("Combo")
                .brand("BrandX")
                .s()
        })
        .collect()
}

fn a7_samples(present: bool, kind: Option<&str>) -> Vec<ProjectionChargeSample> {
    (0..3)
        .map(|k| {
            let sample = b(2 * k).a7().present(present);
            match kind {
                Some(kind) => sample.kind(kind).s(),
                None => sample.s(),
            }
        })
        .collect()
}

fn strip_brand(samples: Vec<ProjectionChargeSample>) -> Vec<ProjectionChargeSample> {
    samples
        .into_iter()
        .map(|mut sample| {
            sample.fast_charger_brand = None;
            sample
        })
        .collect()
}

fn strip_type(samples: Vec<ProjectionChargeSample>) -> Vec<ProjectionChargeSample> {
    samples
        .into_iter()
        .map(|mut sample| {
            sample.fast_charger_type = None;
            sample
        })
        .collect()
}

fn typed(samples: Vec<ProjectionChargeSample>, text: &str) -> Vec<ProjectionChargeSample> {
    samples
        .into_iter()
        .map(|mut sample| {
            sample.fast_charger_type = Some(text.to_owned());
            sample
        })
        .collect()
}

/// Sessions S1 to S4 (process ids 1 to 4, a day apart) plus any extra ones.
fn t76(samples: [Vec<ProjectionChargeSample>; 4]) -> Vec<ProcessGateFacts> {
    samples
        .iter()
        .enumerate()
        .map(|(index, samples)| {
            let id = index as i64 + 1;
            process(id, T0 + id * DAY, samples)
        })
        .collect()
}

fn base_t76() -> [Vec<ProjectionChargeSample>; 4] {
    [
        s1(),
        s2(),
        a7_samples(true, Some("ACSingleWireCAN")),
        a7_samples(false, None),
    ]
}

#[test]
fn c2_t76_baseline_types() {
    let processes = t76(base_t76());
    let report = report_of(&processes);
    let census = report
        .g6
        .census
        .iter()
        .map(|entry| {
            (
                entry.value.as_str(),
                entry.samples,
                entry.sessions,
                entry.kind,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        census,
        vec![
            ("Supercharger", 3, 1, FastChargerKind::Supercharger),
            ("Combo", 3, 1, FastChargerKind::OtherDc),
            ("ACSingleWireCAN", 3, 1, FastChargerKind::TeslaAc),
        ]
    );
    assert_eq!(processes[2].facts.is_dc, Some(false));
    assert_eq!(processes[3].facts.is_dc, Some(false));
    assert_eq!(report.g6.tesla_sites, 1, "S1 is a Tesla site");
    assert!(
        report.g6.dc_failures.is_empty(),
        "S2 is another operator's, typed Combo"
    );
    assert!(report.g6.needs_owner.is_empty());
    assert!(report.classes_not_observed.is_empty());
    assert_eq!(report.g6.pass, Some(true));
    assert_eq!(report.to_json()["g6"]["pass"], json!(true));
    // (a) S2's type CCS: unrecognised.
    let mut ccs = base_t76();
    ccs[1] = typed(s2(), "CCS");
    let report = report_of(&t76(ccs));
    assert_eq!(report.g6.unrecognised, vec!["CCS".to_owned()]);
    assert_eq!(report.g6.pass, Some(false));
    // (b) S3's type Supercharger: S3 ends DC, with three DC-name samples.
    let mut ac_named_dc = base_t76();
    ac_named_dc[2] = a7_samples(true, Some("Supercharger"));
    let processes = t76(ac_named_dc);
    assert_eq!(processes[2].facts.is_dc, Some(true));
    assert_eq!(processes[2].facts.dc_name_samples, 3);
    let report = report_of(&processes);
    assert_eq!(report.g6.ac_failures, vec![3]);
    assert_eq!(report.g6.pass, Some(false));
    // (c) S2's type Supercharger with brand BrandX: the owner decides.
    let mut disagree = base_t76();
    disagree[1] = typed(s2(), "Supercharger");
    let processes = t76(disagree);
    let report = report_of(&processes);
    assert_eq!(report.g6.needs_owner, vec![2]);
    assert_eq!(report.g6.pass, None);
    let answered = |operator| charge_gate_report(&processes, 1, None, &[(2, operator)]);
    let other = answered(SiteOperator::Other);
    assert_eq!(other.g6.pass, Some(false));
    assert_eq!(other.g6.dc_failures, vec![2]);
    assert!(other.g6.needs_owner.is_empty());
    assert_eq!(answered(SiteOperator::Tesla).g6.pass, Some(true));
    // (d) S1 and S2 without brands, and S5: S1's samples without a brand,
    // starting after S1. One answer per session type value decides S5 too.
    let mut unbranded = base_t76();
    unbranded[0] = strip_brand(s1());
    unbranded[1] = strip_brand(s2());
    let mut processes = t76(unbranded);
    processes.push(process(5, T0 + 5 * DAY, &strip_brand(s1())));
    let report = report_of(&processes);
    assert_eq!(report.g6.needs_owner, vec![1, 2]);
    assert_eq!(report.g6.pass, None);
    let both = [(1, SiteOperator::Tesla), (2, SiteOperator::Other)];
    let report = charge_gate_report(&processes, 1, None, &both);
    assert!(report.g6.needs_owner.is_empty());
    assert_eq!(
        report.g6.tesla_sites, 2,
        "S5 is a Tesla site by S1's answer"
    );
    assert_eq!(report.g6.pass, Some(true));
    let one = charge_gate_report(&processes, 1, None, &both[..1]);
    assert_eq!(one.g6.needs_owner, vec![2]);
    assert_eq!(one.g6.pass, None);
    // An answer that names a session which needs none decides nothing.
    let stray = charge_gate_report(&processes, 1, None, &[(3, SiteOperator::Other)]);
    assert_eq!(stray.g6.needs_owner, vec![1, 2]);
    // (e) S1 with no type: no Tesla-site session typed Supercharger.
    let mut untyped = base_t76();
    untyped[0] = strip_type(s1());
    let mut processes = t76(untyped);
    let report = report_of(&processes);
    assert!(
        report.g6.needs_owner.is_empty(),
        "the Tesla brand names the site"
    );
    assert_eq!(report.g6.pass, Some(false));
    // Adding S5 with S1's original samples: pass, half the Tesla sites untyped.
    processes.push(process(5, T0 + 5 * DAY, &s1()));
    let report = report_of(&processes);
    assert_eq!(report.g6.pass, Some(true));
    assert_eq!(report.g6.tesla_sites, 2);
    assert_eq!(report.g6.tesla_sites_without_type, 1);
    assert_eq!(report.g6.tesla_sites_typed_supercharger, 1);
    // (f) Without S3 and S4 the AC class is not observed, and the pass stands.
    let report = report_of(&processes_of_dc_only());
    assert_eq!(report.classes_not_observed, vec![SiteClass::Ac]);
    assert_eq!(report.g6.pass, Some(true));
    // A DC type text more than eight distinct values long is reported.
    let nine = (1..=9)
        .map(|k| b(k).dc(120.0).kind(&format!("V{k}")).s())
        .collect::<Vec<_>>();
    let report = report_of(&[process(1, T0, &nine)]);
    assert_eq!(report.overflow_sessions, vec![1]);
    assert!(!report.g6.a_pass);
}

fn processes_of_dc_only() -> Vec<ProcessGateFacts> {
    vec![process(1, T0 + DAY, &s1()), process(2, T0 + 2 * DAY, &s2())]
}
