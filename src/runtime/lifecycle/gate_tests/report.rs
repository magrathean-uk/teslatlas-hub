// SPDX-License-Identifier: AGPL-3.0-only

//! The pure gate report (C2 R75 (c), C2 section 11): one car's session facts in,
//! each gate's selection, statistics and pass out. Test builds only.
//!
//! Semantics (C2 sections 11 and 17 with C2 R76): G4 counts the recovered
//! restarts, the boundary glitches (a first counter that is a restart against
//! the earlier session's last), the restart reads above their physical bound
//! and those that cannot be bounded, and reports the dips tolerated, the
//! over-count left and the final declines; G6 (c) tests the session type
//! against the operator, and the owner's answer is asked once per distinct
//! session type value. A pass is `None` while a minimum is not met or a needed
//! owner answer is missing, and G2's is `None` too while its median part is
//! open (no known warm session) and the floor holds. C2-B and C2-C amend G6
//! further.

use std::collections::HashMap;

use serde_json::{Value, json};
use time::OffsetDateTime;

use super::fold::{ChargeGateFacts, K_GATE_COUNTER_STEP_KWH, RestartRead};
use super::*;

const MS_PER_DAY: i64 = 86_400_000;
/// G1 minimum: samples and sessions (C2 section 11.1).
const G1_MIN_SAMPLES: u64 = 200;
const G1_MIN_SESSIONS: usize = 5;
/// G1 pass: at least this share of single-element samples, in per cent.
const G1_PASS_PERCENT: u64 = 95;
/// G2 minimum sessions, and the share that may fall below the floor, in per cent.
const G2_MIN_SESSIONS: usize = 10;
const G2_MAX_BELOW_FLOOR_PERCENT: usize = 10;
/// G2 median band over sessions at or above this mean outside temperature.
const G2_WARM_C: f64 = 10.0;
const G2_BAND: (f64, f64) = (1.00, 1.25);
/// G3 selection and band.
const G3_MIN_SAMPLES: u64 = 10;
const G3_MIN_ADDED_KWH: f64 = 10.0;
const G3_MIN_SESSIONS: usize = 3;
const G3_BAND: (f64, f64) = (0.97, 1.03);
/// G4 minimum: sessions with at least this many usable counters.
const G4_MIN_SESSIONS: usize = 20;
const G4_MIN_COUNTERS: u64 = 10;
/// The over-count that is reported: the energy added exceeds the session's
/// largest usable counter by more than this, in kWh (C2 R76 (g)).
const G4_OVERCOUNT_KWH: f64 = 0.5;
/// G5 windows (C2 section 11.1 G5).
const G5_SPREAD_FIGURES: usize = 20;
const G5_MIN_FIGURES: usize = 10;
const G5_MIN_SPAN_DAYS: i64 = 28;
const G5_TEMPERATURE_DAYS: i64 = 365;
const G5_MAX_SPREAD: f64 = 0.10;
const G5_COLD_C: f64 = 5.0;
const G5_WARM_C: f64 = 15.0;
const G5_TEMPERATURE_TOLERANCE: f64 = 0.10;

/// The operator of a DC site, as the owner answers it (C2 R75 (c), C2 O2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SiteOperator {
    Tesla,
    Other,
}

/// One session's facts with its identity (C2 R75 (b) 5).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProcessGateFacts {
    pub car_id: i64,
    pub process_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub facts: ChargeGateFacts,
}

/// The class of a session from Tesla's recorded power and voltage only
/// (C2 section 11.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionClass {
    DcSelected,
    AcSelected,
    Neither,
}

/// The three classes gate G6 needs at least one session of (C2 section 11.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SiteClass {
    TeslaDcSite,
    OtherDcSite,
    Ac,
}

/// Interval figures of the sessions a gate integrates (C2 section 11.0, Timing).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IntervalReport {
    pub intervals: u64,
    pub mean_ms: Option<f64>,
    pub long_intervals: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct G1Report {
    pub sessions: usize,
    pub samples: u64,
    pub unique: u64,
    pub empty: u64,
    pub multi: u64,
    pub unique_share: Option<f64>,
    pub empty_share: Option<f64>,
    pub multi_share: Option<f64>,
    pub lowest_session_share: Option<f64>,
    pub label_differs_share: Option<f64>,
    /// AC-selected sessions left out for a DC-named sample.
    pub excluded_dc_name_sessions: Vec<i64>,
    pub pass: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct G2Report {
    pub sessions: usize,
    pub below_floor: Vec<i64>,
    pub median: Option<f64>,
    pub warm_sessions: usize,
    /// The median over the selected sessions whose known mean outside
    /// temperature is at least 10 degrees C; `None` when there is none, which
    /// leaves the median part of the pass open.
    pub warm_median: Option<f64>,
    /// AC-selected sessions whose kind ends `None`: counted, not selected.
    pub kind_unknown_sessions: usize,
    pub intervals: IntervalReport,
    /// C2 R19's indication: `Some(false)` AC-selected sessions, and those among
    /// them with no energy used while `A_r` is below its minimum.
    pub r19_candidates: usize,
    pub r19_used_none: usize,
    /// `None` below the minimum, and while the floor holds but `warm_median`
    /// is `None`; `Some(true)` needs a known warm median within the band.
    pub pass: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct G3Report {
    pub sessions: usize,
    pub median: Option<f64>,
    pub by_kind: Vec<(FastChargerKind, usize, f64)>,
    pub intervals: IntervalReport,
    /// C2 R42's DC row uses the counter only: fewer than the minimum sessions, or
    /// a median outside the band.
    pub counter_only_branch: bool,
    pub pass: Option<bool>,
}

/// The final declines of C2 R76 (i): sessions whose last usable counter lies
/// below their maximum, `m - l` over them, and their share of all energy added.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeclineReport {
    /// Sessions with `m - l > 0`.
    pub sessions: usize,
    pub median: Option<f64>,
    pub p95: Option<f64>,
    pub maximum: Option<f64>,
    pub total_kwh: f64,
    /// `total_kwh` over the total `added_kwh` of all the car's sessions; above
    /// 0.01 it reopens C2 R76 (i).
    pub decline_share: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct G4Report {
    pub sessions_with_counters: usize,
    /// Every decrease of a usable counter that recovers at the next reading,
    /// dips included.
    pub recovered_decreases: u64,
    pub tolerated_dips: u64,
    pub restart_reads: u64,
    /// G4 (a): restart reads that recover at the next reading.
    pub recovered_restarts: u64,
    pub recovered_restart_sessions: Vec<i64>,
    /// G4 (d): restart reads whose counter exceeds its physical bound.
    pub restart_reads_above_bound: u64,
    pub above_bound_sessions: Vec<i64>,
    /// G4 (e): restart reads with no usable power to bound them.
    pub restart_reads_unbounded: u64,
    pub unbounded_sessions: Vec<i64>,
    /// Every kept restart read with its session, in session start order
    /// (at most `K_GATE_RESTART_ROWS` per session).
    pub restart_rows: Vec<(i64, RestartRead)>,
    /// Restart reads of all sessions beyond the kept rows.
    pub restart_rows_overflow: u64,
    /// The remaining over-count, reported and not gating: sessions whose
    /// energy added exceeds the largest usable counter by more than 0.5 kWh,
    /// and that excess summed.
    pub overcounted_sessions: usize,
    pub overcounted_session_ids: Vec<i64>,
    pub overcount_kwh: f64,
    pub declines: DeclineReport,
    pub consecutive_pairs: usize,
    /// Pairs whose later first sample is within `K_INT_MAX_MS` of the earlier
    /// last sample.
    pub pairs_within_bound: usize,
    /// (earlier, later) process ids.
    pub boundary_glitches: Vec<(i64, i64)>,
    pub undecided: Vec<(i64, i64)>,
    pub pass: Option<bool>,
}

/// Median and count of one calendar quarter (UTC) of figures.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QuarterFigures {
    pub year: i32,
    pub quarter: u8,
    pub median: f64,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct G5Report {
    pub figures: usize,
    pub spread_figures: usize,
    pub spread_span_days: Option<f64>,
    pub q1: Option<f64>,
    pub median: Option<f64>,
    pub q3: Option<f64>,
    pub iqr_over_median: Option<f64>,
    pub cold_figures: usize,
    pub warm_figures: usize,
    pub cold_median: Option<f64>,
    pub warm_median: Option<f64>,
    /// A temperature group has fewer than the minimum figures.
    pub temperature_open: bool,
    pub quarters: Vec<QuarterFigures>,
    pub pass: Option<bool>,
}

/// One distinct trimmed type text with its C2 R31 kind and its numbers of samples
/// and sessions (G6 (a)).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CensusEntry {
    pub value: String,
    pub kind: FastChargerKind,
    pub samples: u64,
    pub sessions: usize,
}

/// Sessions of one class with the same C2 R31 kind and C2 R32 session type (G6 (b)).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClassSessionType {
    pub class: SessionClass,
    pub is_dc: Option<bool>,
    pub session_type: Option<String>,
    pub sessions: usize,
}

/// One session that stands for a pair of session type value and brand class
/// (C2 R78 (e)) and needs the owner's answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OwnerExample {
    pub process_id: i64,
    pub session_type: Option<String>,
    /// `None` when the brand names no operator, or names both.
    pub brand_class: Option<SiteOperator>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct G6Report {
    pub census: Vec<CensusEntry>,
    pub unrecognised: Vec<String>,
    pub class_session_types: Vec<ClassSessionType>,
    /// AC-selected sessions failing (b).
    pub ac_failures: Vec<i64>,
    /// DC-selected sessions failing (c), among them a Tesla-site session typed
    /// Tesla AC whatever its brand says (C2 R78 (e)).
    pub dc_failures: Vec<i64>,
    pub tesla_sites: usize,
    /// Tesla-site sessions whose session type is None and that C2 R41 does not
    /// free (counted, not failing; C2 R78 (e)).
    pub tesla_sites_without_type: usize,
    /// Tesla-site sessions whose brand carries none, as an owner answer named
    /// the site, and that C2 R41 does not free (counted, not failing; C2 R78
    /// (e)). A session typed `Supercharger` is freed by clause (i) and is not
    /// counted.
    pub tesla_sites_without_brand: usize,
    pub tesla_sites_typed_supercharger: usize,
    /// Tesla-site sessions free-eligible by C2 R41, clause (i) or (ii),
    /// evaluated as if `free_supercharging` were true.
    pub tesla_sites_free_eligible: usize,
    /// The first session of each distinct pair of session type value and brand
    /// class (Tesla, other or none) that needs an owner answer and has none
    /// (C2 R78 (e)).
    pub needs_owner: Vec<i64>,
    /// The same examples with the pair each one stands for, in the same order.
    pub needs_owner_pairs: Vec<OwnerExample>,
    pub a_pass: bool,
    pub b_pass: bool,
    pub c_pass: Option<bool>,
    pub pass: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChargeGateReport {
    pub car_id: i64,
    pub lfp_battery: Option<bool>,
    pub sessions: usize,
    /// Every session of the car with its class, in start order.
    pub classes: Vec<(i64, SessionClass)>,
    pub g1: G1Report,
    pub g2: G2Report,
    pub g3: G3Report,
    pub g4: G4Report,
    pub g5: G5Report,
    pub g6: G6Report,
    /// Sessions whose type texts overflowed (C2 R74).
    pub overflow_sessions: Vec<i64>,
    pub classes_not_observed: Vec<SiteClass>,
}

/// The class of a session (C2 section 11.0): DC-selected when some usable power
/// is above `K_AC_MAX_KW`; AC-selected when not, with at least one powered
/// sample and every powered sample at a known per-phase voltage.
fn session_class(facts: &ChargeGateFacts) -> SessionClass {
    if facts.max_power_kw.is_some_and(|power| power > K_AC_MAX_KW) {
        SessionClass::DcSelected
    } else if facts.powered_samples >= 1 && facts.powered_off_ac_voltage == 0 {
        SessionClass::AcSelected
    } else {
        SessionClass::Neither
    }
}

/// The median of figures, the mean of the two middle values for an even count,
/// rounded to 0.1 (C2 R64's statistic, without its minimum count).
fn median_of(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    let median = if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    };
    Some(round_to_precision(median, 1))
}

/// A quantile by linear interpolation at `position` of the ascending values,
/// counted from 0 (G5).
fn interpolated(sorted: &[f64], position: f64) -> Option<f64> {
    let last = sorted.len().checked_sub(1)?;
    let lower = position.floor().clamp(0.0, last as f64) as usize;
    let upper = position.ceil().clamp(0.0, last as f64) as usize;
    let fraction = position - lower as f64;
    Some(sorted[lower] + fraction * (sorted[upper] - sorted[lower]))
}

fn interval_report(sessions: &[&ProcessGateFacts]) -> IntervalReport {
    let intervals: u64 = sessions.iter().map(|s| s.facts.intervals).sum();
    let sum: i64 = sessions.iter().map(|s| s.facts.interval_ms_sum).sum();
    IntervalReport {
        intervals,
        mean_ms: (intervals > 0).then(|| sum as f64 / intervals as f64),
        long_intervals: sessions.iter().map(|s| s.facts.long_intervals).sum(),
    }
}

fn share(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

fn g1(sessions: &[(&ProcessGateFacts, SessionClass)]) -> G1Report {
    let selected = sessions
        .iter()
        .filter(|(s, class)| *class == SessionClass::AcSelected && s.facts.dc_name_samples == 0)
        .map(|(s, _)| *s)
        .filter(|s| s.facts.phase_eligible > 0)
        .collect::<Vec<_>>();
    let excluded = sessions
        .iter()
        .filter(|(s, class)| *class == SessionClass::AcSelected && s.facts.dc_name_samples > 0)
        .map(|(s, _)| s.process_id)
        .collect();
    let samples: u64 = selected.iter().map(|s| s.facts.phase_eligible).sum();
    let unique: u64 = selected.iter().map(|s| s.facts.phase_unique).sum();
    let empty: u64 = selected.iter().map(|s| s.facts.phase_empty).sum();
    let multi: u64 = selected.iter().map(|s| s.facts.phase_multi).sum();
    let differs: u64 = selected.iter().map(|s| s.facts.phase_label_differs).sum();
    let lowest = selected
        .iter()
        .filter_map(|s| share(s.facts.phase_unique, s.facts.phase_eligible))
        .min_by(f64::total_cmp);
    let enough = samples >= G1_MIN_SAMPLES && selected.len() >= G1_MIN_SESSIONS;
    G1Report {
        sessions: selected.len(),
        samples,
        unique,
        empty,
        multi,
        unique_share: share(unique, samples),
        empty_share: share(empty, samples),
        multi_share: share(multi, samples),
        lowest_session_share: lowest,
        label_differs_share: share(differs, unique),
        excluded_dc_name_sessions: excluded,
        pass: enough.then_some(unique * 100 >= samples * G1_PASS_PERCENT),
    }
}

fn g2(sessions: &[(&ProcessGateFacts, SessionClass)]) -> G2Report {
    let ac = sessions
        .iter()
        .filter(|(_, class)| *class == SessionClass::AcSelected)
        .map(|(s, _)| *s)
        .collect::<Vec<_>>();
    let kind_unknown = ac.iter().filter(|s| s.facts.is_dc.is_none()).count();
    let candidates = ac
        .iter()
        .filter(|s| s.facts.is_dc == Some(false))
        .copied()
        .collect::<Vec<_>>();
    let r19_used_none = candidates
        .iter()
        .filter(|s| s.facts.energy.used_kwh.is_none() && s.facts.ratio_added_kwh < K_RATIO_MIN_KWH)
        .count();
    let selected = candidates
        .iter()
        .filter(|s| s.facts.ratio_added_kwh >= K_RATIO_MIN_KWH)
        .copied()
        .collect::<Vec<_>>();
    let ratio = |s: &ProcessGateFacts| s.facts.ratio_used_kwh / s.facts.ratio_added_kwh;
    let below_floor = selected
        .iter()
        .filter(|s| ratio(s) < K_AC_FLOOR)
        .map(|s| s.process_id)
        .collect::<Vec<_>>();
    let ratios = selected.iter().map(|s| ratio(s)).collect::<Vec<_>>();
    let warm = selected
        .iter()
        .filter(|s| {
            s.facts
                .outside_temp_mean_c
                .is_some_and(|temp| temp >= G2_WARM_C)
        })
        .map(|s| ratio(s))
        .collect::<Vec<_>>();
    let warm_median = plain_median(&warm);
    let enough = selected.len() >= G2_MIN_SESSIONS;
    let floor_ok = below_floor.len() * 100 <= selected.len() * G2_MAX_BELOW_FLOOR_PERCENT;
    // The median part (C2 section 11.1 G2): in band or out of it, and open
    // (`None`) while no selected session has a known mean outside temperature
    // of at least 10 degrees C. An open part never passes the gate: the pass
    // is `Some(true)` only with a known, in-band warm median. A floor that
    // fails decides the gate whatever the median part is (three-valued AND:
    // false with open is false, true with open is open).
    let band_ok = warm_median.map(|median| (G2_BAND.0..=G2_BAND.1).contains(&median));
    let pass = match (enough, floor_ok, band_ok) {
        (false, _, _) => None,
        (true, false, _) => Some(false),
        (true, true, band) => band,
    };
    G2Report {
        sessions: selected.len(),
        below_floor,
        median: plain_median(&ratios),
        warm_sessions: warm.len(),
        warm_median,
        kind_unknown_sessions: kind_unknown,
        intervals: interval_report(&selected),
        r19_candidates: candidates.len(),
        r19_used_none,
        pass,
    }
}

/// The unrounded median, the mean of the two middle values for an even count.
fn plain_median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    })
}

fn g3(sessions: &[(&ProcessGateFacts, SessionClass)]) -> G3Report {
    let selected = sessions
        .iter()
        .filter(|(s, class)| {
            *class == SessionClass::DcSelected
                && s.facts.samples >= G3_MIN_SAMPLES
                && s.facts.fully_powered
                && s.facts.ratio_added_kwh >= G3_MIN_ADDED_KWH
        })
        .map(|(s, _)| *s)
        .collect::<Vec<_>>();
    let ratio = |s: &&ProcessGateFacts| s.facts.ratio_used_kwh / s.facts.ratio_added_kwh;
    let ratios = selected.iter().map(ratio).collect::<Vec<_>>();
    let median = plain_median(&ratios);
    let mut by_kind = Vec::new();
    for kind in [
        FastChargerKind::Supercharger,
        FastChargerKind::OtherDc,
        FastChargerKind::TeslaAc,
        FastChargerKind::Unrecognised,
        FastChargerKind::NoInformation,
    ] {
        let kind_ratios = selected
            .iter()
            .filter(|s| fast_charger_kind(s.facts.session_type.as_deref()) == kind)
            .map(ratio)
            .collect::<Vec<_>>();
        if let Some(kind_median) = plain_median(&kind_ratios) {
            by_kind.push((kind, kind_ratios.len(), kind_median));
        }
    }
    let in_band = median.is_some_and(|median| (G3_BAND.0..=G3_BAND.1).contains(&median));
    let enough = selected.len() >= G3_MIN_SESSIONS;
    G3Report {
        sessions: selected.len(),
        median,
        by_kind,
        intervals: interval_report(&selected),
        counter_only_branch: !(enough && in_band),
        pass: enough.then_some(in_band),
    }
}

/// The ids of the sessions whose facts satisfy `has`, in start order.
fn sessions_with(
    sessions: &[(&ProcessGateFacts, SessionClass)],
    has: impl Fn(&ChargeGateFacts) -> bool,
) -> Vec<i64> {
    sessions
        .iter()
        .filter(|(session, _)| has(&session.facts))
        .map(|(session, _)| session.process_id)
        .collect()
}

/// C2 R76 (i)'s statistics over the sessions with `m - l > 0` (G5's interpolation
/// rule), and their total over the total energy added of every session.
fn declines(sessions: &[(&ProcessGateFacts, SessionClass)]) -> DeclineReport {
    let mut figures = sessions
        .iter()
        .map(|(session, _)| session.facts.final_decline_kwh)
        .filter(|decline| *decline > 0.0)
        .collect::<Vec<_>>();
    figures.sort_by(f64::total_cmp);
    let total = figures.iter().fold(0.0_f64, |sum, figure| sum + figure);
    let added = sessions
        .iter()
        .filter_map(|(session, _)| session.facts.energy.added_kwh)
        .fold(0.0_f64, |sum, added| sum + added);
    let at = |quantile: f64| interpolated(&figures, (figures.len() as f64 - 1.0) * quantile);
    DeclineReport {
        sessions: figures.len(),
        median: at(0.5),
        p95: at(0.95),
        maximum: figures.last().copied(),
        total_kwh: total,
        decline_share: (added > 0.0).then(|| total / added),
    }
}

/// G4 (C2 section 11.1 with C2 R76 (g)): (a) restart reads that recover, (b) boundary
/// glitches, (c) undecided boundary pairs, (d) restart reads above their
/// physical bound, (e) restart reads with no power to bound them. Passes when
/// (a) to (e) are all zero, with at least 20 sessions of at least 10 usable
/// counters.
fn g4(sessions: &[(&ProcessGateFacts, SessionClass)]) -> G4Report {
    let with_counters = sessions
        .iter()
        .filter(|(s, _)| s.facts.usable_counters >= G4_MIN_COUNTERS)
        .count();
    let total = |count: fn(&ChargeGateFacts) -> u64| -> u64 {
        sessions.iter().map(|(s, _)| count(&s.facts)).sum()
    };
    let recovered = total(|facts| facts.recovered_decreases);
    let recovered_restarts = total(|facts| facts.recovered_restarts);
    let above_bound = total(|facts| facts.restart_reads_above_bound);
    let unbounded = total(|facts| facts.restart_reads_unbounded);
    let mut within_bound = 0;
    let mut glitches = Vec::new();
    let mut undecided = Vec::new();
    for pair in sessions.windows(2) {
        let (earlier, later) = (pair[0].0, pair[1].0);
        let (Some(earlier_last), Some(later_first)) =
            (earlier.facts.last_sample_ms, later.facts.first_sample_ms)
        else {
            continue;
        };
        if later_first.saturating_sub(earlier_last) > K_INT_MAX_MS {
            continue;
        }
        within_bound += 1;
        let (Some(last), Some(first), Some(second)) = (
            earlier.facts.last_counter,
            later.facts.first_counter,
            later.facts.second_counter,
        ) else {
            continue;
        };
        // (b): the later session's first usable counter is a restart against
        // the earlier session's last, and the next one reaches it again.
        if !(counter_restarted(last.counter_kwh, first.counter_kwh)
            && second.counter_kwh >= last.counter_kwh)
        {
            continue;
        }
        let ids = (earlier.process_id, later.process_id);
        match (first.power_kw, second.power_kw) {
            (None, None) => undecided.push(ids),
            (a, b) => {
                let power = a.into_iter().chain(b).fold(f64::MIN, f64::max);
                let dt_ms = second.timestamp_ms.saturating_sub(first.timestamp_ms);
                let bound = (power + K_PHASE_ABS_KW) * (dt_ms as f64) / MS_PER_HOUR
                    + K_GATE_COUNTER_STEP_KWH;
                if second.counter_kwh - first.counter_kwh > bound {
                    glitches.push(ids);
                }
            }
        }
    }
    let overcount = |facts: &ChargeGateFacts| -> Option<f64> {
        let excess = facts.energy.added_kwh? - facts.largest_counter_kwh?;
        (excess > G4_OVERCOUNT_KWH).then_some(excess)
    };
    let overcounted_session_ids = sessions_with(sessions, |facts| overcount(facts).is_some());
    let overcount_kwh = sessions
        .iter()
        .filter_map(|(session, _)| overcount(&session.facts))
        .fold(0.0_f64, |sum, excess| sum + excess);
    let enough = with_counters >= G4_MIN_SESSIONS;
    G4Report {
        sessions_with_counters: with_counters,
        recovered_decreases: recovered,
        tolerated_dips: total(|facts| facts.tolerated_dips),
        restart_reads: total(|facts| facts.restart_reads),
        recovered_restarts,
        recovered_restart_sessions: sessions_with(sessions, |facts| facts.recovered_restarts > 0),
        restart_reads_above_bound: above_bound,
        above_bound_sessions: sessions_with(sessions, |facts| facts.restart_reads_above_bound > 0),
        restart_reads_unbounded: unbounded,
        unbounded_sessions: sessions_with(sessions, |facts| facts.restart_reads_unbounded > 0),
        restart_rows: sessions
            .iter()
            .flat_map(|(session, _)| {
                session
                    .facts
                    .restart_rows
                    .iter()
                    .map(|read| (session.process_id, *read))
            })
            .collect(),
        restart_rows_overflow: total(|facts| facts.restart_rows_overflow),
        overcounted_sessions: overcounted_session_ids.len(),
        overcounted_session_ids,
        overcount_kwh,
        declines: declines(sessions),
        consecutive_pairs: sessions.len().saturating_sub(1),
        pairs_within_bound: within_bound,
        pass: enough.then_some(
            recovered_restarts == 0
                && glitches.is_empty()
                && undecided.is_empty()
                && above_bound == 0
                && unbounded == 0,
        ),
        boundary_glitches: glitches,
        undecided,
    }
}

fn quarter_of(start_ms: i64) -> (i32, u8) {
    let stamp = OffsetDateTime::from_unix_timestamp(start_ms.div_euclid(1000))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    (stamp.year(), (u8::from(stamp.month()) - 1) / 3 + 1)
}

fn g5(sessions: &[(&ProcessGateFacts, SessionClass)]) -> G5Report {
    // (start, figure, mean outside temperature), in start order.
    let figures = sessions
        .iter()
        .filter(|(s, class)| *class == SessionClass::AcSelected && s.facts.is_dc == Some(false))
        .filter_map(|(s, _)| {
            Some((
                s.start_ms,
                s.facts.efficiency_wh_per_km?,
                s.facts.outside_temp_mean_c,
            ))
        })
        .collect::<Vec<_>>();
    let mut quarter_map: Vec<((i32, u8), Vec<f64>)> = Vec::new();
    for (start, figure, _) in &figures {
        let key = quarter_of(*start);
        match quarter_map.iter_mut().find(|(k, _)| *k == key) {
            Some((_, values)) => values.push(*figure),
            None => quarter_map.push((key, vec![*figure])),
        }
    }
    quarter_map.sort_by_key(|(key, _)| *key);
    let quarters = quarter_map
        .iter()
        .filter_map(|((year, quarter), values)| {
            Some(QuarterFigures {
                year: *year,
                quarter: *quarter,
                median: median_of(values)?,
                count: values.len(),
            })
        })
        .collect::<Vec<_>>();
    let mut report = G5Report {
        figures: figures.len(),
        spread_figures: 0,
        spread_span_days: None,
        q1: None,
        median: None,
        q3: None,
        iqr_over_median: None,
        cold_figures: 0,
        warm_figures: 0,
        cold_median: None,
        warm_median: None,
        temperature_open: true,
        quarters,
        pass: None,
    };
    let Some(newest) = figures.last().map(|(start, _, _)| *start) else {
        return report;
    };
    // The spread window: the newest figures, extended to older ones until the
    // newest and oldest starts are at least 28 days apart.
    let mut begin = figures.len().saturating_sub(G5_SPREAD_FIGURES);
    while begin > 0 && newest - figures[begin].0 < G5_MIN_SPAN_DAYS * MS_PER_DAY {
        begin -= 1;
    }
    let window = &figures[begin..];
    let span_ms = newest - window[0].0;
    let mut sorted = window
        .iter()
        .map(|(_, figure, _)| *figure)
        .collect::<Vec<_>>();
    sorted.sort_by(f64::total_cmp);
    let count = sorted.len() as f64;
    report.spread_figures = window.len();
    report.spread_span_days = Some(span_ms as f64 / MS_PER_DAY as f64);
    report.q1 = interpolated(&sorted, (count - 1.0) / 4.0);
    report.q3 = interpolated(&sorted, 3.0 * (count - 1.0) / 4.0);
    report.median = median_of(&sorted);
    report.iqr_over_median = report
        .q1
        .zip(report.q3)
        .zip(report.median)
        .map(|((q1, q3), median)| (q3 - q1) / median);
    // The temperature window: starts within 365 days before the newest.
    let recent = figures
        .iter()
        .filter(|(start, _, _)| newest - *start <= G5_TEMPERATURE_DAYS * MS_PER_DAY);
    let cold = recent
        .clone()
        .filter(|(_, _, temp)| temp.is_some_and(|temp| temp < G5_COLD_C))
        .map(|(_, figure, _)| *figure)
        .collect::<Vec<_>>();
    let warm = recent
        .filter(|(_, _, temp)| temp.is_some_and(|temp| temp > G5_WARM_C))
        .map(|(_, figure, _)| *figure)
        .collect::<Vec<_>>();
    report.cold_figures = cold.len();
    report.warm_figures = warm.len();
    report.temperature_open = cold.len() < K_EFF_MIN_FIGURES || warm.len() < K_EFF_MIN_FIGURES;
    if !report.temperature_open {
        report.cold_median = median_of(&cold);
        report.warm_median = median_of(&warm);
    }
    let temperature_ok = match (report.cold_median, report.warm_median) {
        (Some(a), Some(b)) => (a - b).abs() <= G5_TEMPERATURE_TOLERANCE * a.min(b),
        _ => true,
    };
    let enough = window.len() >= G5_MIN_FIGURES && span_ms >= G5_MIN_SPAN_DAYS * MS_PER_DAY;
    report.pass = enough.then(|| {
        report
            .iqr_over_median
            .is_some_and(|spread| spread <= G5_MAX_SPREAD)
            && temperature_ok
    });
    report
}

/// The operator a session's brands name (C2 section 11.1 G6): Tesla when some
/// sample's brand reads Tesla and none carries another brand; another operator
/// when some carries another brand and none reads Tesla; otherwise unknown.
fn brand_operator(facts: &ChargeGateFacts) -> Option<SiteOperator> {
    match (facts.tesla_brand, facts.other_brand) {
        (true, false) => Some(SiteOperator::Tesla),
        (false, true) => Some(SiteOperator::Other),
        _ => None,
    }
}

/// Whether the owner's answer decides the session's operator: the brand names
/// none, or it disagrees with the session type (C2 R32: a Tesla brand with a
/// type of kind other DC, another brand with a type of kind Supercharger).
/// One answer decides the sessions that share the pair of session type value and
/// brand class, not the type alone (C2 R78 (e)): the brand already tells apart
/// what one example must not decide.
fn needs_answer(facts: &ChargeGateFacts) -> bool {
    matches!(
        (
            brand_operator(facts),
            fast_charger_kind(facts.session_type.as_deref()),
        ),
        (None, _)
            | (Some(SiteOperator::Tesla), FastChargerKind::OtherDc)
            | (Some(SiteOperator::Other), FastChargerKind::Supercharger)
    )
}

fn g6(
    sessions: &[(&ProcessGateFacts, SessionClass)],
    operator_answers: &[(i64, SiteOperator)],
) -> (G6Report, Vec<SiteClass>) {
    // (a) The census over every session of the car, in first-seen order.
    let mut census: Vec<CensusEntry> = Vec::new();
    for (session, _) in sessions {
        for (value, samples) in &session.facts.type_values {
            match census.iter_mut().find(|entry| entry.value == *value) {
                Some(entry) => {
                    entry.samples += samples;
                    entry.sessions += 1;
                }
                None => census.push(CensusEntry {
                    value: value.clone(),
                    kind: fast_charger_kind(Some(value)),
                    samples: *samples,
                    sessions: 1,
                }),
            }
        }
    }
    let unrecognised = census
        .iter()
        .filter(|entry| entry.kind == FastChargerKind::Unrecognised)
        .map(|entry| entry.value.clone())
        .collect::<Vec<_>>();
    let overflowed = sessions
        .iter()
        .any(|(session, _)| session.facts.type_values_overflow > 0);
    let a_pass = unrecognised.is_empty() && !overflowed;
    // (b) Per class, each session's kind and type; AC sessions must stay AC.
    let mut class_session_types: Vec<ClassSessionType> = Vec::new();
    for (session, class) in sessions {
        if *class == SessionClass::Neither {
            continue;
        }
        let facts = &session.facts;
        match class_session_types.iter_mut().find(|row| {
            row.class == *class
                && row.is_dc == facts.is_dc
                && row.session_type == facts.session_type
        }) {
            Some(row) => row.sessions += 1,
            None => class_session_types.push(ClassSessionType {
                class: *class,
                is_dc: facts.is_dc,
                session_type: facts.session_type.clone(),
                sessions: 1,
            }),
        }
    }
    let ac_failures = sessions
        .iter()
        .filter(|(_, class)| *class == SessionClass::AcSelected)
        .filter(|(session, _)| {
            let facts = &session.facts;
            facts.is_dc == Some(true)
                || facts.dc_name_samples > 0
                || !matches!(
                    fast_charger_kind(facts.session_type.as_deref()),
                    FastChargerKind::TeslaAc | FastChargerKind::NoInformation
                )
        })
        .map(|(session, _)| session.process_id)
        .collect::<Vec<_>>();
    let b_pass = ac_failures.is_empty();
    // (c) The operator of each DC-selected session. One answer per distinct pair
    // of session type value and brand class (C2 R78 (e)) decides every session
    // of that pair that needs one.
    let answers = operator_answers.iter().copied().collect::<HashMap<_, _>>();
    let dc = sessions
        .iter()
        .filter(|(_, class)| *class == SessionClass::DcSelected)
        .map(|(session, _)| *session)
        .collect::<Vec<_>>();
    type Pair = (Option<String>, Option<SiteOperator>);
    let pair =
        |facts: &ChargeGateFacts| -> Pair { (facts.session_type.clone(), brand_operator(facts)) };
    let mut group_answer: Vec<(Pair, Option<SiteOperator>)> = Vec::new();
    let mut needs_owner = Vec::new();
    let mut needs_owner_pairs = Vec::new();
    for session in dc.iter().filter(|s| needs_answer(&s.facts)) {
        let key = pair(&session.facts);
        if !group_answer.iter().any(|(seen, _)| *seen == key) {
            let answer = dc
                .iter()
                .filter(|other| needs_answer(&other.facts) && pair(&other.facts) == key)
                .find_map(|other| answers.get(&other.process_id).copied());
            if answer.is_none() {
                needs_owner.push(session.process_id);
                needs_owner_pairs.push(OwnerExample {
                    process_id: session.process_id,
                    session_type: key.0.clone(),
                    brand_class: key.1,
                });
            }
            group_answer.push((key, answer));
        }
    }
    let operator = |session: &ProcessGateFacts| -> Option<SiteOperator> {
        if needs_answer(&session.facts) {
            let key = pair(&session.facts);
            group_answer
                .iter()
                .find(|(seen, _)| *seen == key)
                .and_then(|(_, answer)| *answer)
        } else {
            brand_operator(&session.facts)
        }
    };
    let mut dc_failures = Vec::new();
    let (mut tesla_sites, mut without_type, mut without_brand) = (0, 0, 0);
    let (mut typed_supercharger, mut free_eligible) = (0, 0);
    let mut other_sites = 0;
    for session in &dc {
        let facts = &session.facts;
        // C2 R41 (i) or (ii), evaluated as if `free_supercharging` were true,
        // by the rule the close and the fill ship (C2 R78 (e)).
        let eligible = free_supercharging_site(
            facts.session_type.as_deref(),
            facts.is_dc,
            brand_operator(facts) == Some(SiteOperator::Tesla),
        );
        match operator(session) {
            Some(SiteOperator::Tesla) => {
                tesla_sites += 1;
                // A Tesla AC type still fails, as before (C2 R78 (e)): it is
                // rejected before the two exceptions below, so a brand that
                // carries none cannot hide it.
                let tesla_ac =
                    fast_charger_kind(facts.session_type.as_deref()) == FastChargerKind::TeslaAc;
                let untyped = facts.session_type.is_none();
                let unbranded = brand_operator(facts).is_none();
                // Exceptions to "every Tesla-site session is free-eligible"
                // (C2 R78 (e)): a session clause (i) frees is not one.
                if untyped && !eligible && !tesla_ac {
                    without_type += 1;
                }
                if unbranded && !eligible && !tesla_ac {
                    without_brand += 1;
                }
                if is_supercharger(facts.session_type.as_deref()) {
                    typed_supercharger += 1;
                }
                if eligible {
                    free_eligible += 1;
                } else if tesla_ac || (!untyped && !unbranded) {
                    dc_failures.push(session.process_id);
                }
            }
            Some(SiteOperator::Other) => {
                other_sites += 1;
                if eligible {
                    dc_failures.push(session.process_id);
                }
            }
            None => {}
        }
    }
    // A Tesla site must be free-eligible at least once.
    let c_holds = dc_failures.is_empty() && (tesla_sites == 0 || free_eligible > 0);
    let c_pass = needs_owner.is_empty().then_some(c_holds);
    let mut not_observed = Vec::new();
    if tesla_sites == 0 {
        not_observed.push(SiteClass::TeslaDcSite);
    }
    if other_sites == 0 {
        not_observed.push(SiteClass::OtherDcSite);
    }
    if !sessions
        .iter()
        .any(|(_, class)| *class == SessionClass::AcSelected)
    {
        not_observed.push(SiteClass::Ac);
    }
    // The pass follows the owner's answer: while a listed example has no answer
    // it stays open (`None`) whatever (a) and (b) say (C2 R75 (c), O2 and R78
    // (f): "G6's rule that its pass stays open while a listed example has no
    // answer is unchanged"); `a_pass` and `b_pass` are reported beside it.
    let pass = c_pass.map(|c| a_pass && b_pass && c);
    (
        G6Report {
            census,
            unrecognised,
            class_session_types,
            ac_failures,
            dc_failures,
            tesla_sites,
            tesla_sites_without_type: without_type,
            tesla_sites_without_brand: without_brand,
            tesla_sites_typed_supercharger: typed_supercharger,
            tesla_sites_free_eligible: free_eligible,
            needs_owner,
            needs_owner_pairs,
            a_pass,
            b_pass,
            c_pass,
            pass,
        },
        not_observed,
    )
}

/// Apply C2 section 11 to one car's sessions (C2 R75 (c)). Sessions of other cars
/// are ignored; the order is start time, then process id.
pub(crate) fn charge_gate_report(
    processes: &[ProcessGateFacts],
    car_id: i64,
    lfp_battery: Option<bool>,
    operator_answers: &[(i64, SiteOperator)],
) -> ChargeGateReport {
    let mut ordered = processes
        .iter()
        .filter(|process| process.car_id == car_id)
        .collect::<Vec<_>>();
    ordered.sort_by_key(|process| (process.start_ms, process.process_id));
    let sessions = ordered
        .iter()
        .map(|process| (*process, session_class(&process.facts)))
        .collect::<Vec<_>>();
    let (g6, classes_not_observed) = g6(&sessions, operator_answers);
    ChargeGateReport {
        car_id,
        lfp_battery,
        sessions: sessions.len(),
        classes: sessions
            .iter()
            .map(|(process, class)| (process.process_id, *class))
            .collect(),
        g1: g1(&sessions),
        g2: g2(&sessions),
        g3: g3(&sessions),
        g4: g4(&sessions),
        g5: g5(&sessions),
        g6,
        overflow_sessions: sessions
            .iter()
            .filter(|(process, _)| process.facts.type_values_overflow > 0)
            .map(|(process, _)| process.process_id)
            .collect(),
        classes_not_observed,
    }
}

fn brand_class_label(class: Option<SiteOperator>) -> &'static str {
    match class {
        Some(SiteOperator::Tesla) => "tesla",
        Some(SiteOperator::Other) => "other",
        None => "none",
    }
}

fn kind_label(kind: FastChargerKind) -> &'static str {
    match kind {
        FastChargerKind::Supercharger => "supercharger",
        FastChargerKind::OtherDc => "other_dc",
        FastChargerKind::TeslaAc => "tesla_ac",
        FastChargerKind::NoInformation => "no_information",
        FastChargerKind::Unrecognised => "unrecognised",
    }
}

fn class_label(class: SessionClass) -> &'static str {
    match class {
        SessionClass::DcSelected => "dc_selected",
        SessionClass::AcSelected => "ac_selected",
        SessionClass::Neither => "neither",
    }
}

fn site_label(class: SiteClass) -> &'static str {
    match class {
        SiteClass::TeslaDcSite => "tesla_dc_site",
        SiteClass::OtherDcSite => "other_dc_site",
        SiteClass::Ac => "ac",
    }
}

fn pairs(values: &[(i64, i64)]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|(earlier, later)| json!([earlier, later]))
            .collect(),
    )
}

impl ChargeGateReport {
    /// The report as JSON: process ids, counts and statistics only (section
    /// 11.0, Receipts).
    pub(crate) fn to_json(&self) -> Value {
        let intervals = |report: &IntervalReport| {
            json!({
                "intervals": report.intervals,
                "mean_ms": report.mean_ms,
                "long_intervals": report.long_intervals,
            })
        };
        json!({
            "car_id": self.car_id,
            "lfp_battery": self.lfp_battery,
            "sessions": self.sessions,
            "classes": self.classes.iter()
                .map(|(id, class)| json!([id, class_label(*class)]))
                .collect::<Vec<_>>(),
            "overflow_sessions": self.overflow_sessions,
            "classes_not_observed": self.classes_not_observed.iter()
                .map(|class| site_label(*class))
                .collect::<Vec<_>>(),
            "g1": {
                "sessions": self.g1.sessions,
                "samples": self.g1.samples,
                "unique": self.g1.unique,
                "empty": self.g1.empty,
                "multi": self.g1.multi,
                "unique_share": self.g1.unique_share,
                "empty_share": self.g1.empty_share,
                "multi_share": self.g1.multi_share,
                "lowest_session_share": self.g1.lowest_session_share,
                "label_differs_share": self.g1.label_differs_share,
                "excluded_dc_name_sessions": self.g1.excluded_dc_name_sessions,
                "pass": self.g1.pass,
            },
            "g2": {
                "sessions": self.g2.sessions,
                "below_floor": self.g2.below_floor,
                "median": self.g2.median,
                "warm_sessions": self.g2.warm_sessions,
                "warm_median": self.g2.warm_median,
                "kind_unknown_sessions": self.g2.kind_unknown_sessions,
                "intervals": intervals(&self.g2.intervals),
                "r19_candidates": self.g2.r19_candidates,
                "r19_used_none": self.g2.r19_used_none,
                "pass": self.g2.pass,
            },
            "g3": {
                "sessions": self.g3.sessions,
                "median": self.g3.median,
                "by_kind": self.g3.by_kind.iter()
                    .map(|(kind, count, median)| json!({
                        "kind": kind_label(*kind), "sessions": count, "median": median,
                    }))
                    .collect::<Vec<_>>(),
                "intervals": intervals(&self.g3.intervals),
                "counter_only_branch": self.g3.counter_only_branch,
                "pass": self.g3.pass,
            },
            "g4": {
                "sessions_with_counters": self.g4.sessions_with_counters,
                "recovered_decreases": self.g4.recovered_decreases,
                "tolerated_dips": self.g4.tolerated_dips,
                "restart_reads": self.g4.restart_reads,
                "recovered_restarts": self.g4.recovered_restarts,
                "recovered_restart_sessions": self.g4.recovered_restart_sessions,
                "restart_reads_above_bound": self.g4.restart_reads_above_bound,
                "above_bound_sessions": self.g4.above_bound_sessions,
                "restart_reads_unbounded": self.g4.restart_reads_unbounded,
                "unbounded_sessions": self.g4.unbounded_sessions,
                "restart_rows": self.g4.restart_rows.iter()
                    .map(|(process_id, read)| json!({
                        "process_id": process_id,
                        "previous_ms": read.previous.timestamp_ms,
                        "previous_counter_kwh": read.previous.counter_kwh,
                        "previous_power_kw": read.previous.power_kw,
                        "ms": read.current.timestamp_ms,
                        "counter_kwh": read.current.counter_kwh,
                        "power_kw": read.current.power_kw,
                        "p_max_kw": read.power_kw,
                        "bound_kwh": read.bound_kwh,
                        "above_bound": read.above_bound(),
                    }))
                    .collect::<Vec<_>>(),
                "restart_rows_overflow": self.g4.restart_rows_overflow,
                "overcounted_sessions": self.g4.overcounted_sessions,
                "overcounted_session_ids": self.g4.overcounted_session_ids,
                "overcount_kwh": self.g4.overcount_kwh,
                "final_declines": {
                    "sessions": self.g4.declines.sessions,
                    "median": self.g4.declines.median,
                    "p95": self.g4.declines.p95,
                    "maximum": self.g4.declines.maximum,
                    "total_kwh": self.g4.declines.total_kwh,
                    "decline_share": self.g4.declines.decline_share,
                },
                "consecutive_pairs": self.g4.consecutive_pairs,
                "pairs_within_bound": self.g4.pairs_within_bound,
                "boundary_glitches": pairs(&self.g4.boundary_glitches),
                "undecided": pairs(&self.g4.undecided),
                "pass": self.g4.pass,
            },
            "g5": {
                "figures": self.g5.figures,
                "spread_figures": self.g5.spread_figures,
                "spread_span_days": self.g5.spread_span_days,
                "q1": self.g5.q1,
                "median": self.g5.median,
                "q3": self.g5.q3,
                "iqr_over_median": self.g5.iqr_over_median,
                "cold_figures": self.g5.cold_figures,
                "warm_figures": self.g5.warm_figures,
                "cold_median": self.g5.cold_median,
                "warm_median": self.g5.warm_median,
                "temperature_open": self.g5.temperature_open,
                "quarters": self.g5.quarters.iter()
                    .map(|q| json!({
                        "year": q.year, "quarter": q.quarter, "median": q.median, "count": q.count,
                    }))
                    .collect::<Vec<_>>(),
                "pass": self.g5.pass,
            },
            "g6": {
                "census": self.g6.census.iter()
                    .map(|entry| json!({
                        "value": entry.value,
                        "kind": kind_label(entry.kind),
                        "samples": entry.samples,
                        "sessions": entry.sessions,
                    }))
                    .collect::<Vec<_>>(),
                "unrecognised": self.g6.unrecognised,
                "class_session_types": self.g6.class_session_types.iter()
                    .map(|row| json!({
                        "class": class_label(row.class),
                        "is_dc": row.is_dc,
                        "session_type": row.session_type,
                        "sessions": row.sessions,
                    }))
                    .collect::<Vec<_>>(),
                "ac_failures": self.g6.ac_failures,
                "dc_failures": self.g6.dc_failures,
                "tesla_sites": self.g6.tesla_sites,
                "tesla_sites_without_type": self.g6.tesla_sites_without_type,
                "tesla_sites_without_brand": self.g6.tesla_sites_without_brand,
                "tesla_sites_typed_supercharger": self.g6.tesla_sites_typed_supercharger,
                "tesla_sites_free_eligible": self.g6.tesla_sites_free_eligible,
                "needs_owner": self.g6.needs_owner,
                "needs_owner_pairs": self.g6.needs_owner_pairs.iter()
                    .map(|example| json!({
                        "process_id": example.process_id,
                        "session_type": example.session_type,
                        "brand_class": brand_class_label(example.brand_class),
                    }))
                    .collect::<Vec<_>>(),
                "a_pass": self.g6.a_pass,
                "b_pass": self.g6.b_pass,
                "c_pass": self.g6.c_pass,
                "pass": self.g6.pass,
            },
        })
    }
}
