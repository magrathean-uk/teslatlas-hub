// SPDX-License-Identifier: AGPL-3.0-only

//! C2-B gate-level spec test (C2 section 18.1, C2 R78 (e) and (f); C2 T82): gate G6
//! under the brand rule the close and the fill ship. The owner's answer is one
//! per distinct pair of session type value and brand class, so one example of a
//! pair must not decide sessions the brand already tells apart; a Tesla-site
//! session is free-eligible by C2 R41 clause (i) or (ii), evaluated as if
//! `free_supercharging` were true. Synthetic data only. The sessions are those
//! of car 1; a session's start time orders it. The lifecycle-level (C2 T80,
//! C2 T81 (a) to (e)) and store-level (C2 T81 (f)) tests are in
//! `b3_c2_b_tests.rs` and `src/storage/db/tests/b3_c2_b.rs`.

use super::report::{ChargeGateReport, ProcessGateFacts, SiteOperator, charge_gate_report};
use super::support::*;
use super::*;

fn report_of(processes: &[ProcessGateFacts], answers: &[(i64, SiteOperator)]) -> ChargeGateReport {
    charge_gate_report(processes, 1, None, answers)
}

/// S1: three samples 2 min apart `{P 120, V 2, I 0, fast_charger_present true,
/// fast_charger_type "Supercharger", fast_charger_brand "Tesla"}`.
fn s1() -> Vec<ProjectionChargeSample> {
    dc_session("Supercharger", "Tesla")
}

/// S6: the same with type `Combo` and brand `Tesla`; S7: S6's samples with
/// every brand `<invalid>`.
fn s6() -> Vec<ProjectionChargeSample> {
    dc_session("Combo", "Tesla")
}

fn s7() -> Vec<ProjectionChargeSample> {
    dc_session("Combo", "<invalid>")
}

fn dc_session(kind: &str, brand: &str) -> Vec<ProjectionChargeSample> {
    (0..3)
        .map(|k| b(2 * k).dc(120.0).present(true).kind(kind).brand(brand).s())
        .collect()
}

/// S3: three `A7` samples with `fast_charger_present true` and type
/// `ACSingleWireCAN`; S4: three `A7` samples with `fast_charger_present false`
/// and no type.
fn s3() -> Vec<ProjectionChargeSample> {
    (0..3)
        .map(|k| b(2 * k).a7().present(true).kind("ACSingleWireCAN").s())
        .collect()
}

fn s4() -> Vec<ProjectionChargeSample> {
    (0..3).map(|k| b(2 * k).a7().present(false).s()).collect()
}

/// S8: S4's samples with the second sample's type `Tesla`.
fn s8() -> Vec<ProjectionChargeSample> {
    let mut samples = s4();
    samples[1].fast_charger_type = Some("Tesla".to_owned());
    samples
}

/// Session `id` of car 1, a day apart from its neighbours.
fn session(id: i64, samples: &[ProjectionChargeSample]) -> ProcessGateFacts {
    process(id, T0 + id * DAY, samples)
}

/// S1, S3, S4, S6 and S7 as process ids 1, 3, 4, 6 and 7.
fn t82_b() -> Vec<ProcessGateFacts> {
    vec![
        session(1, &s1()),
        session(3, &s3()),
        session(4, &s4()),
        session(6, &s6()),
        session(7, &s7()),
    ]
}

#[test]
fn c2_t82_pair_grouped_answers() {
    // The brand flags are the production fold's: S6 reads Tesla, S7 carries none.
    let processes = t82_b();
    let (six, seven) = (&processes[3].facts, &processes[4].facts);
    assert!(
        six.tesla_brand && !six.other_brand,
        "S6's brand reads Tesla"
    );
    assert!(!seven.tesla_brand && !seven.other_brand, "S7 carries none");
    assert_eq!(six.session_type.as_deref(), Some("Combo"));
    assert_eq!(seven.session_type.as_deref(), Some("Combo"));

    // (a) S1, S3, S4 and S8: the census holds `Tesla` (1 sample, 1 session, no
    // information); S8 ends `Some(false)` with no session type; pass true.
    let (p8, s8_samples) = (8, s8());
    let census_processes = vec![
        session(1, &s1()),
        session(3, &s3()),
        session(4, &s4()),
        session(p8, &s8_samples),
    ];
    let report = report_of(&census_processes, &[]);
    let tesla = report
        .g6
        .census
        .iter()
        .find(|entry| entry.value == "Tesla")
        .expect("the census holds the text Tesla");
    assert_eq!(
        (tesla.samples, tesla.sessions, tesla.kind),
        (1, 1, FastChargerKind::NoInformation)
    );
    assert_eq!(census_processes[3].facts.is_dc, Some(false));
    assert_eq!(census_processes[3].facts.session_type, None);
    assert!(report.g6.unrecognised.is_empty());
    assert!(report.g6.ac_failures.is_empty());
    assert_eq!(report.g6.pass, Some(true));

    // (b) S1, S3, S4, S6 and S7: one example per pair (`Combo`, Tesla) and
    // (`Combo`, none); pass None until both are answered.
    let report = report_of(&processes, &[]);
    assert_eq!(report.g6.needs_owner, vec![6, 7]);
    assert_eq!(report.g6.pass, None);
    // An answer decides its own pair only: both sessions are typed `Combo`.
    let only_six = report_of(&processes, &[(6, SiteOperator::Tesla)]);
    assert_eq!(only_six.g6.needs_owner, vec![7]);
    assert_eq!(only_six.g6.pass, None);
    let only_seven = report_of(&processes, &[(7, SiteOperator::Other)]);
    assert_eq!(only_seven.g6.needs_owner, vec![6]);
    assert_eq!(only_seven.g6.pass, None);
    // (S6, tesla) and (S7, other): pass true.
    let answered = report_of(
        &processes,
        &[(6, SiteOperator::Tesla), (7, SiteOperator::Other)],
    );
    assert!(answered.g6.needs_owner.is_empty());
    assert_eq!(answered.g6.pass, Some(true));
    assert_eq!(answered.g6.tesla_sites, 2, "S1 and S6");
    assert_eq!(answered.g6.tesla_sites_free_eligible, 2);
    assert!(answered.g6.dc_failures.is_empty());
    // (S6, tesla) and (S7, tesla): pass true, S7 reported as a Tesla-site
    // session that C2 R41 does not free (its brand carries none).
    let both_tesla = report_of(
        &processes,
        &[(6, SiteOperator::Tesla), (7, SiteOperator::Tesla)],
    );
    assert_eq!(both_tesla.g6.pass, Some(true));
    assert_eq!(both_tesla.g6.tesla_sites, 3, "S1, S6 and S7");
    assert_eq!(
        both_tesla.g6.tesla_sites_free_eligible, 2,
        "S7 is not freed"
    );
    assert_eq!(both_tesla.g6.tesla_sites_without_brand, 1);
    assert_eq!(both_tesla.g6.tesla_sites_without_type, 0);
    assert!(both_tesla.g6.dc_failures.is_empty());
    assert_eq!(both_tesla.to_json()["g6"]["tesla_sites_without_brand"], 1);
    // (S6, other) with either answer for S7: pass false, S6 listed under G6 (c)
    // (another operator's site, free-eligible by clause (ii)).
    for seven_answer in [SiteOperator::Tesla, SiteOperator::Other] {
        let report = report_of(&processes, &[(6, SiteOperator::Other), (7, seven_answer)]);
        assert_eq!(report.g6.pass, Some(false));
        assert_eq!(report.g6.dc_failures, vec![6]);
        assert!(report.g6.needs_owner.is_empty());
    }

    // (c) S6', with S6's samples and starting after S6, takes S6's answer (same
    // pair) and is not listed in `needs_owner`; S7 still needs its own.
    let mut with_repeat = t82_b();
    with_repeat.push(session(9, &s6()));
    let report = report_of(&with_repeat, &[]);
    assert_eq!(report.g6.needs_owner, vec![6, 7], "S6' is not listed");
    let report = report_of(&with_repeat, &[(6, SiteOperator::Tesla)]);
    assert_eq!(report.g6.needs_owner, vec![7]);
    let report = report_of(
        &with_repeat,
        &[(6, SiteOperator::Tesla), (7, SiteOperator::Other)],
    );
    assert!(report.g6.needs_owner.is_empty());
    assert_eq!(report.g6.tesla_sites, 3, "S1, S6 and S6'");
    assert_eq!(report.g6.pass, Some(true));
}

/// Each example of `needs_owner` with the pair it stands for (C2 R78 (e)): the
/// amended receipt asks the owner one question per pair, so the pair travels
/// with the example. Another session typed `Supercharger` whose brand carries
/// none is free-eligible by C2 R41 clause (i) whatever the owner answers, so it
/// is not an exception to "every Tesla-site session is free-eligible".
#[test]
fn c2_t82_owner_pairs_and_free_eligible_exceptions() {
    use super::report::OwnerExample;
    let mut processes = t82_b();
    processes.push(session(10, &dc_session("Supercharger", "<invalid>")));
    let report = report_of(&processes, &[]);
    assert_eq!(report.g6.needs_owner, vec![6, 7, 10]);
    let example = |process_id: i64, kind: &str, class: Option<SiteOperator>| OwnerExample {
        process_id,
        session_type: Some(kind.to_owned()),
        brand_class: class,
    };
    assert_eq!(
        report.g6.needs_owner_pairs,
        vec![
            example(6, "Combo", Some(SiteOperator::Tesla)),
            example(7, "Combo", None),
            example(10, "Supercharger", None),
        ]
    );
    let json = report.to_json();
    assert_eq!(
        json["g6"]["needs_owner_pairs"],
        serde_json::json!([
            {"process_id": 6, "session_type": "Combo", "brand_class": "tesla"},
            {"process_id": 7, "session_type": "Combo", "brand_class": "none"},
            {"process_id": 10, "session_type": "Supercharger", "brand_class": "none"},
        ])
    );
    assert_eq!(json["g6"]["needs_owner"], serde_json::json!([6, 7, 10]));
    // An answered pair leaves the list, and its example with it.
    let report = report_of(&processes, &[(7, SiteOperator::Other)]);
    assert_eq!(
        report
            .g6
            .needs_owner_pairs
            .iter()
            .map(|example| example.process_id)
            .collect::<Vec<_>>(),
        report.g6.needs_owner
    );
    assert_eq!(report.g6.needs_owner, vec![6, 10]);

    // S10 answered Tesla beside S6 Tesla and S7 other: a Tesla-site session
    // typed Supercharger is free-eligible by clause (i), so it is neither an
    // exception nor a failure.
    let answered = report_of(
        &processes,
        &[
            (6, SiteOperator::Tesla),
            (7, SiteOperator::Other),
            (10, SiteOperator::Tesla),
        ],
    );
    assert_eq!(answered.g6.pass, Some(true));
    assert_eq!(answered.g6.tesla_sites, 3, "S1, S6 and S10");
    assert_eq!(answered.g6.tesla_sites_free_eligible, 3);
    assert_eq!(answered.g6.tesla_sites_typed_supercharger, 2, "S1 and S10");
    assert_eq!(
        answered.g6.tesla_sites_without_brand, 0,
        "S10's brand carries none and clause (i) frees it"
    );
    assert_eq!(answered.g6.tesla_sites_without_type, 0);
    assert!(answered.g6.dc_failures.is_empty());
    // The same session typed `Combo` is the exception C2 R78 (e) counts.
    let combo = vec![
        session(1, &s1()),
        session(10, &dc_session("Combo", "<invalid>")),
    ];
    let report = report_of(&combo, &[(10, SiteOperator::Tesla)]);
    assert_eq!(report.g6.tesla_sites, 2);
    assert_eq!(report.g6.tesla_sites_free_eligible, 1);
    assert_eq!(report.g6.tesla_sites_without_brand, 1);
}

/// C2 R78 (e): "A Tesla-site session typed Tesla AC still fails, as before." The
/// two exceptions (a session type of None, a brand that carries none) are not
/// open to a Tesla AC type: a DC-selected session whose folded type is
/// `ACSingleWireCAN` or `MCSingleWireCAN` is listed in `dc_failures` and fails
/// G6 beside an eligible S1, whether its brand reads `Tesla` or carries none
/// (the owner then names the operator) and is counted under neither exception.
#[test]
fn c2_t82_tesla_ac_type_fails_beside_the_no_brand_exception() {
    for kind in ["ACSingleWireCAN", "MCSingleWireCAN"] {
        for brand in ["<invalid>", "Tesla"] {
            let ac_typed = session(10, &dc_session(kind, brand));
            assert_eq!(
                ac_typed.facts.session_type.as_deref(),
                Some(kind),
                "the fold keeps the Tesla AC type as the session type"
            );
            assert_eq!(ac_typed.facts.is_dc, Some(true));
            let processes = vec![session(1, &s1()), ac_typed];
            let report = report_of(&processes, &[(10, SiteOperator::Tesla)]);
            let g6 = &report.g6;
            assert!(g6.needs_owner.is_empty(), "{kind} {brand}: answered");
            assert_eq!(g6.tesla_sites, 2, "{kind} {brand}: S1 and the session");
            assert_eq!(g6.tesla_sites_free_eligible, 1, "{kind} {brand}: S1 only");
            assert_eq!(g6.dc_failures, vec![10], "{kind} {brand}");
            assert_eq!(g6.c_pass, Some(false), "{kind} {brand}");
            assert_eq!(g6.pass, Some(false), "{kind} {brand}");
            assert_eq!(
                g6.tesla_sites_without_brand, 0,
                "{kind} {brand}: a failure, not the no-brand exception"
            );
            assert_eq!(g6.tesla_sites_without_type, 0, "{kind} {brand}");
            assert_eq!(
                report.to_json()["g6"]["dc_failures"],
                serde_json::json!([10])
            );
        }
    }
    // The exception itself is intact: a `Combo` session whose brand carries none
    // is counted, not failed, so the gate passes beside S1.
    let combo = vec![
        session(1, &s1()),
        session(10, &dc_session("Combo", "<invalid>")),
    ];
    let report = report_of(&combo, &[(10, SiteOperator::Tesla)]);
    assert!(report.g6.dc_failures.is_empty());
    assert_eq!(report.g6.tesla_sites_without_brand, 1);
    assert_eq!(report.g6.pass, Some(true));
}

/// The counter, in kWh, of each sample of a session whose samples are `step_ms`
/// apart from `T0`, with the usable power of `power`.
fn counted_session(
    id: i64,
    step_ms: i64,
    power: Option<f64>,
    counters: &[f64],
) -> ProcessGateFacts {
    let start = T0 + id * DAY;
    let samples = counters
        .iter()
        .enumerate()
        .map(|(k, counter)| {
            let sample = b_ms(k as i64 + 1, start + k as i64 * step_ms).c(*counter);
            match power {
                Some(power) => sample.power(power).s(),
                None => sample.s(),
            }
        })
        .collect::<Vec<_>>();
    process(id, start, &samples)
}

/// The amended receipt reports every restart's post-fall counter against its
/// physical bound (work order section 7; coordinator ruling F6). The facts keep
/// the first `K_GATE_RESTART_ROWS` restart reads of a session with both
/// timestamps, both counters, the larger power and the bound, and count the
/// rest, so they stay of fixed size (C2 R74 (c)); G4 lists them with their
/// session and the JSON carries them.
#[test]
fn c2_t84_restart_rows_carry_the_bound_evidence() {
    use super::fold::K_GATE_RESTART_ROWS;
    const BOUND_TOLERANCE: f64 = 1e-9;
    // (a) 30.00 at t0, 29.80 at +20 s, both at `P 11`: bound (11 + 1) x 20,000 /
    // 3,600,000 + 0.01 = 0.0766667, which 29.80 exceeds.
    let a = counted_session(7, 20_000, Some(11.0), &[30.00, 29.80, 29.90]);
    assert_eq!(a.facts.restart_reads, 1);
    assert_eq!(a.facts.restart_rows.len(), 1);
    assert_eq!(a.facts.restart_rows_overflow, 0);
    let row = a.facts.restart_rows[0];
    let start = T0 + 7 * DAY;
    assert_eq!(row.previous.timestamp_ms, start);
    assert_eq!(row.previous.counter_kwh, 30.00);
    assert_eq!(row.current.timestamp_ms, start + 20_000);
    assert_eq!(row.current.counter_kwh, 29.80);
    assert_eq!(row.power_kw, Some(11.0));
    let bound = row.bound_kwh.expect("the bound is computed");
    assert!((bound - (12.0 * 20_000.0 / 3_600_000.0 + 0.01)).abs() < BOUND_TOLERANCE);
    assert!(row.above_bound());

    // (b) Within its bound: 10.00 then 0.05, whose bound from `P 11` is the same
    // 0.0766667 (the larger power of the two samples decides it).
    let b = counted_session(8, 20_000, Some(11.0), &[10.00, 0.05, 1.00]);
    assert_eq!(b.facts.restart_rows.len(), 1);
    assert!(!b.facts.restart_rows[0].above_bound());
    assert_eq!(b.facts.restart_rows[0].current.counter_kwh, 0.05);

    // (c) No usable power on either sample: no bound, not above it.
    let c = counted_session(9, 20_000, None, &[10.00, 0.05, 1.00]);
    assert_eq!(c.facts.restart_reads_unbounded, 1);
    assert_eq!(c.facts.restart_rows.len(), 1);
    assert_eq!(c.facts.restart_rows[0].power_kw, None);
    assert_eq!(c.facts.restart_rows[0].bound_kwh, None);
    assert!(!c.facts.restart_rows[0].above_bound());

    // (d) Ten restarts in one session: the first eight are kept in sample
    // order, the other two are counted.
    let counters = (0..21)
        .map(|k| if k % 2 == 0 { 10.00 } else { 5.00 })
        .collect::<Vec<_>>();
    let d = counted_session(11, 20_000, Some(8.0), &counters);
    assert_eq!(d.facts.restart_reads, 10);
    assert_eq!(K_GATE_RESTART_ROWS, 8);
    assert_eq!(d.facts.restart_rows.len(), K_GATE_RESTART_ROWS);
    assert_eq!(d.facts.restart_rows_overflow, 2);
    let d_start = T0 + 11 * DAY;
    for (k, row) in d.facts.restart_rows.iter().enumerate() {
        let at = d_start + 20_000 * (2 * k as i64 + 1);
        assert_eq!(row.current.timestamp_ms, at, "row {k} is the {k}th restart");
        assert_eq!(row.previous.timestamp_ms, at - 20_000);
        assert_eq!(
            (row.previous.counter_kwh, row.current.counter_kwh),
            (10.00, 5.00)
        );
    }

    // The report lists the kept rows with their session, in start order, and
    // sums the overflow; the JSON carries each row and the overflow.
    let report = report_of(&[a, b, c, d], &[]);
    let g4 = &report.g4;
    let ids = g4
        .restart_rows
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    assert_eq!(ids, [vec![7, 8, 9], vec![11; 8]].concat());
    assert_eq!(g4.restart_rows_overflow, 2);
    assert_eq!(g4.restart_reads, 13, "the rows and the overflow add up");
    assert_eq!(
        g4.restart_rows.len() as u64 + g4.restart_rows_overflow,
        g4.restart_reads
    );
    let json = report.to_json();
    let rows = json["g4"]["restart_rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 11);
    assert_eq!(json["g4"]["restart_rows_overflow"], 2);
    let first = &rows[0];
    assert_eq!(first["process_id"], 7);
    assert_eq!(first["previous_ms"], start);
    assert_eq!(first["previous_counter_kwh"], 30.00);
    assert_eq!(first["ms"], start + 20_000);
    assert_eq!(first["counter_kwh"], 29.80);
    assert_eq!(first["previous_power_kw"], 11.0);
    assert_eq!(first["power_kw"], 11.0);
    assert_eq!(first["p_max_kw"], 11.0);
    assert_eq!(first["above_bound"], true);
    assert!(
        (first["bound_kwh"].as_f64().expect("bound") - 0.0766666666).abs() < 1e-6,
        "{first}"
    );
    assert_eq!(rows[1]["above_bound"], false);
    assert_eq!(rows[2]["process_id"], 9);
    assert_eq!(rows[2]["bound_kwh"], serde_json::Value::Null);
    assert_eq!(rows[2]["p_max_kw"], serde_json::Value::Null);
    assert_eq!(rows[2]["above_bound"], false);
}
