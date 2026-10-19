// SPDX-License-Identifier: AGPL-3.0-only

//! C2-A gate-level spec tests (C2 section 18.1, C2 R76 (g) to (i); C2 T79, C2 T83 (d) and
//! C2 T84): the gate fold counts the dips tolerated, the restart reads, the
//! recovered restarts and the restart reads above their physical bound or
//! without power to bound them; the report gives G4's (a) to (e), the
//! over-count left and the final declines. Synthetic data only. Sessions are
//! those of car 1; a session's start time orders it.

use super::fold::ChargeGateFacts;
use super::report::{ChargeGateReport, ProcessGateFacts, charge_gate_report};
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

/// The facts of one session whose samples are `spacing_ms` apart, each with
/// the counter given and, when `power` is `Some`, that usable power.
fn counted(spacing_ms: i64, power: Option<f64>, counters: &[f64]) -> ChargeGateFacts {
    let samples = counters
        .iter()
        .enumerate()
        .map(|(k, counter)| {
            let sample = b_ms(k as i64 + 1, T0 + k as i64 * spacing_ms).c(*counter);
            match power {
                Some(power) => sample.power(power).s(),
                None => sample.s(),
            }
        })
        .collect::<Vec<_>>();
    fold_all(&samples)
}

/// Session `id` of ten usable counters one minute apart at `P 7`, one day
/// apart from its neighbours: `c = 0.10 x j`, with the counter at `index`
/// replaced by `replaced` when given.
fn counter_session(id: i64, replaced: Option<(usize, f64)>) -> ProcessGateFacts {
    let start = T0 + id * DAY;
    let samples = (0..10)
        .map(|j| {
            let counter = match replaced {
                Some((index, value)) if index == j => value,
                _ => 0.10 * j as f64,
            };
            b_ms(j as i64 + 1, start + j as i64 * MIN)
                .power(7.0)
                .c(counter)
                .s()
        })
        .collect::<Vec<_>>();
    process(id, start, &samples)
}

/// Twenty such sessions, session 7 with its fifth counter replaced.
fn twenty_sessions(fifth: Option<f64>) -> Vec<ProcessGateFacts> {
    (1..=20)
        .map(|id| counter_session(id, fifth.filter(|_| id == 7).map(|value| (4, value))))
        .collect()
}

/// Session 1 ends at `T0` with counter `p_last` and session 2 follows with the
/// two samples given, all at `P 8`.
fn boundary_pair(p_last: f64, q: [(i64, f64); 2]) -> ChargeGateReport {
    let p_samples = [
        b_ms(1, T0 - 20_000).power(8.0).c(p_last - 0.02).s(),
        b_ms(2, T0).power(8.0).c(p_last).s(),
    ];
    let q_samples = q.map(|(at_ms, counter)| b_ms(1, at_ms).power(8.0).c(counter).s());
    report_of(&[
        process(1, T0 - 20_000, &p_samples),
        process(2, q[0].0, &q_samples),
    ])
}

#[test]
fn c2_t79_amended_restarts() {
    let facts = |counters: &[f64]| counted(20_000, Some(8.0), counters);
    // (a) The measured recovered dip is tolerated: no restart.
    let dip = facts(&[50.80, 50.86, 50.84, 50.88]);
    assert_eq!(dip.recovered_decreases, 1);
    assert_eq!(dip.tolerated_dips, 1);
    assert_eq!(dip.restart_reads, 0);
    assert_eq!(dip.recovered_restarts, 0);
    // A restart read that recovers is both a recovered decrease and a recovered
    // restart.
    let restart = facts(&[10.00, 10.50, 0.40, 10.60, 11.00]);
    assert_eq!(restart.recovered_decreases, 1);
    assert_eq!(restart.tolerated_dips, 0);
    assert_eq!(restart.restart_reads, 1);
    assert_eq!(restart.recovered_restarts, 1);
    // Successive dips that each stay within the tolerance are two dips, however
    // deep, and the climb back recovers the last of them only.
    let successive = facts(&[10.00, 9.97, 9.94, 12.00]);
    assert_eq!(successive.recovered_decreases, 1);
    assert_eq!(successive.tolerated_dips, 2);
    assert_eq!(successive.restart_reads, 0);
    assert_eq!(successive.recovered_restarts, 0);
    // Two restart reads, the later recovered: the pending value of a restart
    // read is replaced by the next one.
    let two = facts(&[10.00, 5.00, 3.00, 12.00]);
    assert_eq!(two.tolerated_dips, 0);
    assert_eq!(two.restart_reads, 2);
    assert_eq!(two.recovered_restarts, 1);
    // The boundary of the test itself: a fall of exactly 0.05 is a dip and one
    // of 0.06 a restart.
    assert_eq!(facts(&[10.00, 9.95]).tolerated_dips, 1);
    assert_eq!(facts(&[10.00, 9.95]).restart_reads, 0);
    assert_eq!(facts(&[10.00, 9.94]).restart_reads, 1);
    assert_eq!(facts(&[10.00, 9.94]).tolerated_dips, 0);
    // Only usable counters are compared: an unusable one between changes
    // nothing, and a rise is neither.
    let mut gap = vec![
        b_ms(1, T0).power(8.0).c(10.00).s(),
        b_ms(2, T0 + 20_000).power(8.0).s(),
        b_ms(3, T0 + 40_000).power(8.0).c(9.97).s(),
    ];
    assert_eq!(fold_all(&gap).tolerated_dips, 1);
    gap[2].charge_energy_added_kwh = Some(10.20);
    let rise = fold_all(&gap);
    assert_eq!((rise.tolerated_dips, rise.restart_reads), (0, 0));

    // (b) The measured boundary pair: session P's last usable counter is 50.86;
    // session Q reads 50.84 at +121 s and 50.88 at +126 s. The first counter is
    // a dip against 50.86, so the pair is neither a glitch nor undecided.
    let pair = boundary_pair(50.86, [(T0 + 121_000, 50.84), (T0 + 126_000, 50.88)]);
    assert_eq!(pair.g4.pairs_within_bound, 1);
    assert!(pair.g4.boundary_glitches.is_empty());
    assert!(pair.g4.undecided.is_empty());
    // Control: a first counter 0.06 below is a restart against 50.86, the next
    // one reaches it again, and the rise 0.10 exceeds (8 + 1) x 5,000 /
    // 3,600,000 + 0.01 = 0.0225: a boundary glitch.
    let glitch = boundary_pair(50.86, [(T0 + 121_000, 50.80), (T0 + 126_000, 50.90)]);
    assert_eq!(glitch.g4.boundary_glitches, vec![(1, 2)]);
    // A first counter exactly 0.05 below stays a dip.
    let edge = boundary_pair(50.86, [(T0 + 121_000, 50.81), (T0 + 126_000, 50.90)]);
    assert!(edge.g4.boundary_glitches.is_empty());

    // (c) Twenty sessions of ten usable counters, one of them with its fifth
    // counter 0.28 (a dip of 0.02 below 0.30 that recovers at 0.50): G4's (a),
    // (b) and (c) are zero and the pass is true.
    let clean = report_of(&twenty_sessions(Some(0.28))).g4;
    assert_eq!(clean.sessions_with_counters, 20);
    assert_eq!(clean.recovered_restarts, 0);
    assert!(clean.recovered_restart_sessions.is_empty());
    assert!(clean.boundary_glitches.is_empty());
    assert!(clean.undecided.is_empty());
    assert_eq!(clean.tolerated_dips, 1);
    assert_eq!(clean.recovered_decreases, 1);
    assert_eq!(clean.pass, Some(true));
    // With that fifth counter 0.20 (a restart read that recovers): (a) is 1, the
    // session is listed, and the pass is false.
    let restart = report_of(&twenty_sessions(Some(0.20))).g4;
    assert_eq!(restart.recovered_restarts, 1);
    assert_eq!(restart.recovered_restart_sessions, vec![7]);
    assert_eq!(restart.restart_reads, 1);
    assert_eq!(restart.pass, Some(false));
    // The report's totals come from the sessions' facts, not one session's.
    let two_dirty = {
        let mut processes = twenty_sessions(Some(0.20));
        processes[11] = counter_session(12, Some((4, 0.20)));
        report_of(&processes).g4
    };
    assert_eq!(two_dirty.recovered_restarts, 2);
    assert_eq!(two_dirty.recovered_restart_sessions, vec![7, 12]);
}

#[test]
fn c2_t83_decline_report() {
    // (d) Three sessions without power and without a baseline, samples 10 min
    // apart: S1 c 0.00, 5.00, 4.98, 4.96; S2 c 0.00, 5.00; S3 c 0.00, 3.00,
    // 2.97, 2.94. The per-session final decline m - l is 0.04, 0.0 and 0.06.
    let session = |id: i64, counters: &[f64]| {
        let samples = counters
            .iter()
            .enumerate()
            .map(|(k, counter)| b_ms(k as i64 + 1, T0 + id * DAY + k as i64 * 10 * MIN).c(*counter))
            .map(B::s)
            .collect::<Vec<_>>();
        process(id, T0 + id * DAY, &samples)
    };
    let s1 = session(1, &[0.00, 5.00, 4.98, 4.96]);
    let s2 = session(2, &[0.00, 5.00]);
    let s3 = session(3, &[0.00, 3.00, 2.97, 2.94]);
    close(Some(s1.facts.final_decline_kwh), 0.04, "S1 m - l");
    assert_eq!(s2.facts.final_decline_kwh, 0.0);
    close(Some(s3.facts.final_decline_kwh), 0.06, "S3 m - l");
    // The decline is below the maximum, not below the first counter: S1's last
    // counter lies below its maximum 5.00 and above its first 0.00.
    assert_eq!(s1.facts.energy.added_kwh, Some(5.00));
    assert_eq!(s1.facts.largest_counter_kwh, Some(5.00));
    let report = report_of(&[s1.clone(), s2.clone(), s3.clone()]).g4;
    assert_eq!(report.declines.sessions, 2);
    close(report.declines.median, 0.05, "median");
    close(report.declines.p95, 0.059, "95th percentile");
    close(report.declines.maximum, 0.06, "maximum");
    assert!((report.declines.total_kwh - 0.10).abs() < TOLERANCE);
    close(report.declines.decline_share, 0.10 / 13.00, "decline share");
    assert!(report.declines.decline_share.unwrap() < 0.01);
    // Without S2 the total energy added is 8.00 and the share exceeds 1 %.
    let report = report_of(&[s1, s3]).g4;
    close(report.declines.decline_share, 0.10 / 8.00, "decline share");
    assert!((report.declines.decline_share.unwrap() - 0.0125).abs() < TOLERANCE);
    assert!(report.declines.decline_share.unwrap() > 0.01);
    // No decline at all: no statistic, a share of zero, and a total of zero.
    let none = report_of(&[session(2, &[0.00, 5.00])]).g4;
    assert_eq!(none.declines.sessions, 0);
    assert_eq!(none.declines.median, None);
    assert_eq!(none.declines.maximum, None);
    assert_eq!(none.declines.total_kwh, 0.0);
    assert_eq!(none.declines.decline_share, Some(0.0));
}

/// The samples of one session starting at `base`, as (offset in ms, counter,
/// usable power).
fn series(base: i64, rows: &[(i64, f64, Option<f64>)]) -> Vec<ProjectionChargeSample> {
    rows.iter()
        .map(|(offset_ms, counter, power)| {
            let sample = b_ms(offset_ms / 1000 + 1, base + offset_ms).c(*counter);
            match power {
                Some(power) => sample.power(*power).s(),
                None => sample.s(),
            }
        })
        .collect()
}

/// Session `id` of `rows`, one day apart from its neighbours.
fn session_of(id: i64, rows: &[(i64, f64, Option<f64>)]) -> ProcessGateFacts {
    let start = T0 + id * DAY;
    process(id, start, &series(start, rows))
}

/// Twenty clean sessions of ten usable counters (sessions 1 to 20), plus the
/// session given as 21.
fn twenty_and(extra: &[(i64, f64, Option<f64>)]) -> Vec<ProcessGateFacts> {
    let mut processes = twenty_sessions(None);
    processes.push(session_of(21, extra));
    processes
}

#[test]
fn c2_t84_bound_and_overcount() {
    // (a) One session at `P 11`: 30.00 at t0, 29.80 at +20 s, 29.90 at +6 min.
    // The fall to 29.80 is a restart read that does not recover (29.90 is below
    // 30.00), and 29.80 exceeds its bound (11 + 1) x 20,000 / 3,600,000 + 0.01 =
    // 0.0767: above the bound, so the counter cannot have restarted and climbed
    // there in the time. The energy added 59.90 counts the 30.00 twice.
    let a = [
        (0, 30.00, Some(11.0)),
        (20_000, 29.80, Some(11.0)),
        (6 * 60_000, 29.90, Some(11.0)),
    ];
    let facts = fold_all(&series(T0, &a));
    assert_eq!(facts.restart_reads, 1);
    assert_eq!(facts.recovered_restarts, 0);
    assert_eq!(facts.restart_reads_above_bound, 1);
    assert_eq!(facts.restart_reads_unbounded, 0);
    assert_eq!(facts.energy.added_kwh, Some(59.90));
    assert_eq!(facts.largest_counter_kwh, Some(30.00));
    let report = report_of(&[session_of(7, &a)]).g4;
    assert_eq!(report.overcounted_sessions, 1);
    assert_eq!(report.overcounted_session_ids, vec![7]);
    assert!((report.overcount_kwh - 29.90).abs() < TOLERANCE);
    assert_eq!(report.recovered_restarts, 0, "(a) cannot see it");
    assert_eq!(report.restart_reads_above_bound, 1, "(d) finds it");
    assert_eq!(report.above_bound_sessions, vec![7]);
    // The gate needs twenty sessions of ten usable counters: beside twenty
    // clean ones, (d) alone fails the pass; the three-counter session is not
    // one of the twenty.
    let gate = report_of(&twenty_and(&a)).g4;
    assert_eq!(gate.sessions_with_counters, 20);
    assert_eq!(
        (gate.recovered_restarts, gate.restart_reads_above_bound),
        (0, 1)
    );
    assert_eq!(gate.pass, Some(false));
    let nineteen = (1..=19)
        .map(|id| counter_session(id, None))
        .chain([session_of(20, &a)])
        .collect::<Vec<_>>();
    let gate = report_of(&nineteen).g4;
    assert_eq!(gate.sessions_with_counters, 19);
    assert_eq!(
        gate.pass, None,
        "twenty sessions of ten counters are needed"
    );

    // (b) 10.00 at t0 with `P 11`, 0.05 at +20 s with `P 0`, 1.00 at +6 min with
    // `P 11`: a restart read within its bound (0.05 against 0.0767, from the
    // larger power 11). The energy added 11.00 against the largest counter 10.00
    // is reported and does not gate.
    let b = [
        (0, 10.00, Some(11.0)),
        (20_000, 0.05, Some(0.0)),
        (6 * 60_000, 1.00, Some(11.0)),
    ];
    let facts = fold_all(&series(T0, &b));
    assert_eq!(facts.restart_reads, 1);
    assert_eq!(facts.restart_reads_above_bound, 0);
    assert_eq!(facts.restart_reads_unbounded, 0);
    assert_eq!(facts.energy.added_kwh, Some(11.00));
    let report = report_of(&[session_of(7, &b)]).g4;
    assert_eq!(report.overcounted_sessions, 1);
    assert!((report.overcount_kwh - 1.00).abs() < TOLERANCE);
    assert_eq!(report.restart_reads_above_bound, 0);
    assert_eq!(report_of(&twenty_and(&b)).g4.pass, Some(true));
    // The bound uses the larger power of the two samples: from the restart
    // read's own sample alone it would be (0 + 1) x 20,000 / 3,600,000 + 0.01 =
    // 0.0156, below 0.05. With `P 1` on the earlier sample the bound is
    // (1 + 1) x 20,000 / 3,600,000 + 0.01 = 0.0211, and the read is above it.
    let weak = [
        (0, 10.00, Some(1.0)),
        (20_000, 0.05, Some(0.0)),
        (6 * 60_000, 1.00, Some(1.0)),
    ];
    assert_eq!(fold_all(&series(T0, &weak)).restart_reads_above_bound, 1);
    // An over-count of 0.5 kWh or less is not reported: 10.00, then 0.05 and
    // 0.45 gives 10.45 added against 10.00.
    let small = [
        (0, 10.00, Some(11.0)),
        (20_000, 0.05, Some(0.0)),
        (6 * 60_000, 0.45, Some(11.0)),
    ];
    assert_eq!(
        report_of(&[session_of(7, &small)]).g4.overcounted_sessions,
        0
    );

    // (c) As (b) without `charger_power` on its first two samples: the bound
    // cannot be computed, so the read is undecided and not above the bound.
    let c = [
        (0, 10.00, None),
        (20_000, 0.05, None),
        (6 * 60_000, 1.00, Some(11.0)),
    ];
    let facts = fold_all(&series(T0, &c));
    assert_eq!(facts.restart_reads_unbounded, 1);
    assert_eq!(facts.restart_reads_above_bound, 0);
    let report = report_of(&[session_of(7, &c)]).g4;
    assert_eq!(report.restart_reads_unbounded, 1);
    assert_eq!(report.unbounded_sessions, vec![7]);
    assert_eq!(report.restart_reads_above_bound, 0);
    assert_eq!(
        report_of(&twenty_sessions(None)).g4.pass,
        Some(true),
        "the twenty clean sessions pass"
    );
    assert_eq!(
        report_of(&twenty_and(&c)).g4.pass,
        Some(false),
        "(e) alone fails the pass"
    );

    // (d) Revision 6's cases under the new counts, at 20 s and `P 8` (bound
    // 0.06 kWh).
    let bounded = |counters: &[f64]| counted(20_000, Some(8.0), counters);
    assert_eq!(
        bounded(&[50.80, 50.86, 50.84, 50.88]).restart_reads_above_bound,
        0
    );
    assert_eq!(
        bounded(&[10.00, 10.50, 0.40, 10.60, 11.00]).restart_reads_above_bound,
        1
    );
    assert_eq!(
        bounded(&[10.00, 9.97, 9.94, 12.00]).restart_reads_above_bound,
        0
    );
    assert_eq!(
        bounded(&[10.00, 5.00, 3.00, 12.00]).restart_reads_above_bound,
        2
    );
    // C2 T79 (c)'s report: (d) 0, (e) 0 and the pass true; its variant with the fifth
    // counter 0.20 also has (d) 1 (0.20 exceeds (7 + 1) x 60,000 / 3,600,000 +
    // 0.01 = 0.1433).
    let clean = report_of(&twenty_sessions(Some(0.28))).g4;
    assert_eq!(clean.restart_reads_above_bound, 0);
    assert_eq!(clean.restart_reads_unbounded, 0);
    assert_eq!(clean.pass, Some(true));
    let restart = report_of(&twenty_sessions(Some(0.20))).g4;
    assert_eq!(restart.restart_reads_above_bound, 1);
    assert_eq!(restart.above_bound_sessions, vec![7]);
    assert_eq!(restart.restart_reads_unbounded, 0);
    // The JSON carries the new facts.
    let json = report_of(&twenty_sessions(Some(0.20))).to_json();
    assert_eq!(json["g4"]["restart_reads_above_bound"], 1);
    assert_eq!(json["g4"]["recovered_restarts"], 1);
    assert_eq!(json["g4"]["final_declines"]["sessions"], 0);
}
