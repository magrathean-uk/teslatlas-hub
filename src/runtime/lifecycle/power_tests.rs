// SPDX-License-Identifier: AGPL-3.0-only

//! Part C2-P1 spec tests (C2 specification section 10, T22 to T26) and the
//! rules of R31 to R39 that no numbered test pins: charger type
//! normalisation, the evidence boundaries, the constants' edges and the
//! purity of the sample functions. Synthetic data only. `t0` is
//! 1_760_000_000_000 ms.
//!
//! The sample literal is `S`: a `ProjectionChargeSample` whose unspecified
//! fields are `None`. The session folds below apply the pure pieces the
//! observer will call (`sample_kind_evidence` with `session_kind_after`, and
//! `session_charger_type_after`) one sample at a time, in the order R39
//! states: this sample's kind evidence first, then the power with the updated
//! kind.

use super::*;

const T0: i64 = 1_760_000_000_000;
const TOLERANCE: f64 = 1e-9;

/// A sample at `t0 + minutes` with every optional field `None`.
fn s(minutes: i64) -> ProjectionChargeSample {
    ProjectionChargeSample {
        id: minutes + 1,
        charge_process_id: 1,
        timestamp_ms: T0 + minutes * 60_000,
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

/// An electrical sample: voltage, current, reported power and phase label.
fn vipr(
    voltage: Option<f64>,
    current: Option<f64>,
    power: Option<f64>,
    phases: Option<i64>,
) -> ProjectionChargeSample {
    let mut sample = s(0);
    sample.charger_voltage = voltage;
    sample.charger_actual_current = current;
    sample.charger_power_kw = power;
    sample.charger_phases = phases;
    sample
}

fn typed(text: &str) -> ProjectionChargeSample {
    let mut sample = s(0);
    sample.fast_charger_type = Some(text.to_owned());
    sample
}

fn assert_power(
    sample: &ProjectionChargeSample,
    session_is_dc: Option<bool>,
    session_phase_count: Option<i64>,
    power_kw: Option<f64>,
    phase_count: Option<i64>,
    label: &str,
) {
    let got = sample_grid_power_kw(sample, session_is_dc, session_phase_count);
    assert_eq!(got.phase_count, phase_count, "{label}: phase count");
    match (got.power_kw, power_kw) {
        (Some(got), Some(want)) => {
            assert!((got - want).abs() < TOLERANCE, "{label}: {got} vs {want}")
        }
        (got, want) => assert_eq!(got, want, "{label}: power"),
    }
}

/// Fold a sequence of samples into the session kind (R31 table).
fn fold_kind(samples: &[ProjectionChargeSample]) -> Option<bool> {
    samples.iter().fold(None, |is_dc, sample| {
        session_kind_after(is_dc, sample_kind_evidence(sample))
    })
}

/// Fold a sequence of samples into the session charger type (R32).
fn fold_type(samples: &[ProjectionChargeSample]) -> Option<String> {
    samples.iter().fold(None, |current, sample| {
        session_charger_type_after(current, sample)
    })
}

#[test]
fn t22_power_table() {
    type Row = (
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<i64>,
        Option<bool>,
        Option<i64>,
        Option<f64>,
        Option<i64>,
    );
    let rows: [Row; 15] = [
        // V, I, P, r, session is_dc, session count, power, phase count
        (
            Some(230.0),
            Some(16.0),
            Some(11.0),
            Some(3),
            None,
            None,
            Some(11.04),
            Some(3),
        ),
        (
            Some(240.0),
            Some(32.0),
            Some(8.0),
            Some(2),
            None,
            None,
            Some(7.68),
            Some(1),
        ),
        (
            Some(120.0),
            Some(12.0),
            Some(1.0),
            Some(1),
            None,
            None,
            Some(1.44),
            Some(1),
        ),
        (
            Some(230.0),
            Some(6.0),
            Some(4.0),
            Some(3),
            None,
            None,
            Some(4.14),
            Some(3),
        ),
        (
            Some(230.0),
            Some(6.0),
            Some(4.0),
            None,
            None,
            None,
            Some(4.0),
            None,
        ),
        (
            Some(230.0),
            Some(6.0),
            Some(4.0),
            None,
            None,
            Some(3),
            Some(4.14),
            None,
        ),
        (
            Some(230.0),
            Some(16.0),
            None,
            Some(3),
            None,
            None,
            Some(11.04),
            Some(3),
        ),
        (
            Some(230.0),
            Some(16.0),
            None,
            Some(0),
            None,
            None,
            None,
            None,
        ),
        (
            Some(230.0),
            Some(16.0),
            Some(11.0),
            Some(1),
            None,
            None,
            Some(11.04),
            Some(3),
        ),
        (
            Some(230.0),
            Some(16.0),
            Some(7.0),
            Some(2),
            None,
            None,
            Some(7.36),
            Some(2),
        ),
        (
            Some(400.0),
            Some(16.0),
            Some(11.0),
            Some(3),
            None,
            None,
            Some(11.0),
            None,
        ),
        (
            Some(230.0),
            Some(90.0),
            Some(11.0),
            None,
            None,
            None,
            Some(11.0),
            None,
        ),
        (
            None,
            Some(16.0),
            Some(11.0),
            None,
            None,
            None,
            Some(11.0),
            None,
        ),
        (None, None, Some(-3.0), None, None, None, None, None),
        (
            Some(2.0),
            Some(0.0),
            Some(120.0),
            None,
            Some(true),
            None,
            Some(120.0),
            None,
        ),
    ];
    for (index, (voltage, current, power, phases, is_dc, session, want_power, want_phases)) in
        rows.into_iter().enumerate()
    {
        assert_power(
            &vipr(voltage, current, power, phases),
            is_dc,
            session,
            want_power,
            want_phases,
            &format!("T22 row {}", index + 1),
        );
    }
}

#[test]
fn t22_dc_session_reads_the_reported_power_whatever_the_electrical_inputs() {
    // R33 step 2: during DC the on-board charger's voltage and current do not
    // describe the flow, even when they look like a clean AC input.
    let sample = vipr(Some(230.0), Some(16.0), Some(11.0), Some(3));
    assert_power(
        &sample,
        Some(true),
        None,
        Some(11.0),
        None,
        "dc, plausible AC inputs",
    );
    assert_power(
        &sample,
        Some(true),
        Some(1),
        Some(11.0),
        None,
        "dc with session count",
    );
    let unusable = vipr(Some(230.0), Some(16.0), None, Some(3));
    assert_power(
        &unusable,
        Some(true),
        Some(3),
        None,
        None,
        "dc without power",
    );
    // A kind of Some(false) or None does not switch the rule off.
    assert_power(
        &sample,
        Some(false),
        None,
        Some(11.04),
        Some(3),
        "ac session",
    );
}

#[test]
fn t22_voltage_current_and_power_edges() {
    // R33 step 3 bounds are inclusive at both ends of each range (K_V_MIN,
    // K_V_MAX, K_I_MAX) and exclusive at a current of zero.
    assert_power(
        &vipr(Some(90.0), Some(10.0), Some(1.0), Some(1)),
        None,
        None,
        Some(0.9),
        Some(1),
        "V at K_V_MIN",
    );
    assert_power(
        &vipr(Some(89.9), Some(10.0), Some(1.0), Some(1)),
        None,
        None,
        Some(1.0),
        None,
        "V below K_V_MIN",
    );
    assert_power(
        &vipr(Some(305.0), Some(10.0), Some(3.0), Some(1)),
        None,
        None,
        Some(3.05),
        Some(1),
        "V at K_V_MAX",
    );
    assert_power(
        &vipr(Some(305.1), Some(10.0), Some(3.0), Some(1)),
        None,
        None,
        Some(3.0),
        None,
        "V above K_V_MAX",
    );
    assert_power(
        &vipr(Some(240.0), Some(80.0), Some(19.0), Some(1)),
        None,
        None,
        Some(19.2),
        Some(1),
        "I at K_I_MAX",
    );
    assert_power(
        &vipr(Some(240.0), Some(80.1), Some(19.0), Some(1)),
        None,
        None,
        Some(19.0),
        None,
        "I above K_I_MAX",
    );
    assert_power(
        &vipr(Some(240.0), Some(0.0), Some(0.0), Some(1)),
        None,
        None,
        Some(0.0),
        None,
        "I zero reads the reported power",
    );
    assert_power(
        &vipr(Some(240.0), Some(-1.0), Some(5.0), Some(1)),
        None,
        None,
        Some(5.0),
        None,
        "negative I",
    );
    assert_power(
        &vipr(Some(f64::NAN), Some(16.0), Some(11.0), Some(3)),
        None,
        None,
        Some(11.0),
        None,
        "NaN V is not known",
    );
    assert_power(
        &vipr(Some(230.0), Some(f64::INFINITY), Some(11.0), Some(3)),
        None,
        None,
        Some(11.0),
        None,
        "infinite I is not known",
    );
    assert_power(
        &vipr(Some(230.0), Some(16.0), Some(f64::NAN), Some(3)),
        None,
        None,
        Some(11.04),
        Some(3),
        "NaN P is not usable",
    );
    // R37: nothing known gives no power.
    assert_power(
        &vipr(None, None, None, Some(3)),
        None,
        None,
        None,
        None,
        "nothing known",
    );
    assert_power(
        &vipr(Some(230.0), None, None, Some(3)),
        None,
        None,
        None,
        None,
        "current unknown, no power",
    );
    assert_power(
        &vipr(None, Some(16.0), None, Some(3)),
        None,
        Some(3),
        None,
        None,
        "voltage unknown, no power",
    );
    // R33 step 1: a negative reported power is a fault, even with inputs that
    // would give a power; with a label the label row (R35 row 5) applies.
    assert_power(
        &vipr(Some(230.0), Some(16.0), Some(-1.0), Some(3)),
        None,
        None,
        Some(11.04),
        Some(3),
        "negative P with label",
    );
    assert_power(
        &vipr(Some(230.0), Some(16.0), Some(-1.0), None),
        None,
        None,
        None,
        None,
        "negative P without label",
    );
    assert_power(
        &vipr(None, None, Some(0.0), None),
        None,
        None,
        Some(0.0),
        None,
        "plugged in at 0 kW",
    );
    assert_power(
        &vipr(Some(240.0), Some(0.0), Some(0.0), None),
        None,
        None,
        Some(0.0),
        None,
        "I 0, P 0",
    );
}

#[test]
fn t22_consistency_set_edges_follow_the_tolerance() {
    // R34: |pred - P| <= 1.0 + 0.10 * pred. At 230 V and 6 A the predictions
    // are 1.38, 2.76 and 4.14 kW with tolerances 1.138, 1.276 and 1.414.
    // P 2: one phase (0.62) and two (0.76) agree, three (2.14) does not.
    let pair = vipr(Some(230.0), Some(6.0), Some(2.0), None);
    assert_power(
        &pair,
        None,
        None,
        Some(2.0),
        None,
        "set {1,2}, no tie-break",
    );
    assert_power(
        &pair,
        None,
        Some(2),
        Some(2.76),
        None,
        "set {1,2}, session 2",
    );
    assert_power(
        &pair,
        None,
        Some(1),
        Some(1.38),
        None,
        "set {1,2}, session 1",
    );
    assert_power(
        &pair,
        None,
        Some(3),
        Some(2.0),
        None,
        "set {1,2}, session 3 not in set",
    );
    // P 2.5 still holds one phase (1.12 <= 1.138); P 2.55 does not (1.17), so
    // the count becomes unique.
    let edge_in = vipr(Some(230.0), Some(6.0), Some(2.5), None);
    assert_power(
        &edge_in,
        None,
        None,
        Some(2.5),
        None,
        "relative edge, still ambiguous",
    );
    let edge_out = vipr(Some(230.0), Some(6.0), Some(2.55), None);
    assert_power(
        &edge_out,
        None,
        None,
        Some(2.76),
        Some(2),
        "relative edge, unique",
    );
    // P 1.0 leaves only one phase (two: 1.76 > 1.276).
    assert_power(
        &vipr(Some(230.0), Some(6.0), Some(1.0), None),
        None,
        None,
        Some(1.38),
        Some(1),
        "unique low",
    );
    // The label breaks a tie only when it is consistent, and the label
    // outranks the session count.
    let label_two = vipr(Some(230.0), Some(6.0), Some(2.0), Some(2));
    assert_power(
        &label_two,
        None,
        Some(1),
        Some(2.76),
        Some(2),
        "label beats session",
    );
    let label_three = vipr(Some(230.0), Some(6.0), Some(2.0), Some(3));
    assert_power(
        &label_three,
        None,
        Some(1),
        Some(1.38),
        None,
        "inconsistent label falls to session",
    );
    // An empty set: P far from every count gives the reported power and never
    // the label or the session.
    let far = vipr(Some(230.0), Some(16.0), Some(30.0), Some(3));
    assert_power(&far, None, None, Some(30.0), None, "empty set");
    assert_power(
        &far,
        None,
        Some(3),
        Some(30.0),
        None,
        "empty set ignores session",
    );
}

#[test]
fn t22_label_and_session_count_without_power_are_limited_to_one_to_three() {
    // R35 rows 5 to 7 with the domain of R30.
    let base = |phases| vipr(Some(230.0), Some(10.0), None, phases);
    assert_power(&base(Some(1)), None, None, Some(2.3), Some(1), "label 1");
    assert_power(&base(Some(2)), None, None, Some(4.6), Some(2), "label 2");
    assert_power(&base(Some(3)), None, None, Some(6.9), Some(3), "label 3");
    assert_power(&base(Some(4)), None, None, None, None, "label 4");
    assert_power(&base(Some(-1)), None, None, None, None, "label -1");
    assert_power(
        &base(Some(0)),
        None,
        Some(2),
        Some(4.6),
        None,
        "label 0 falls to session 2",
    );
    assert_power(
        &base(Some(1)),
        None,
        Some(3),
        Some(2.3),
        Some(1),
        "label outranks session without power",
    );
    assert_power(&base(None), None, Some(3), Some(6.9), None, "session only");
    assert_power(
        &base(None),
        None,
        Some(0),
        None,
        None,
        "session 0 is not a count",
    );
    assert_power(
        &base(None),
        None,
        Some(4),
        None,
        None,
        "session 4 is not a count",
    );
}

#[test]
fn t23_session_kind() {
    let present = |value: bool| {
        let mut sample = s(0);
        sample.fast_charger_present = Some(value);
        sample
    };
    let with_present = |value: bool, text: &str| {
        let mut sample = typed(text);
        sample.fast_charger_present = Some(value);
        sample
    };
    let power = |kw: f64| {
        let mut sample = s(0);
        sample.charger_power_kw = Some(kw);
        sample
    };
    let present_power = |value: bool, kw: f64| {
        let mut sample = power(kw);
        sample.fast_charger_present = Some(value);
        sample
    };
    let cases: Vec<(&str, Vec<ProjectionChargeSample>, Option<bool>)> = vec![
        (
            "ACSingleWireCAN",
            vec![typed("ACSingleWireCAN")],
            Some(false),
        ),
        (
            "FastChargerCombo",
            vec![typed("FastChargerCombo")],
            Some(true),
        ),
        (
            "fastchargersupercharger",
            vec![typed("fastchargersupercharger")],
            Some(true),
        ),
        ("padded combo", vec![typed(" combo ")], Some(true)),
        (
            "present true, invalid",
            vec![with_present(true, "<invalid>")],
            Some(true),
        ),
        (
            "present false, P 7",
            vec![present_power(false, 7.0)],
            Some(false),
        ),
        ("P 60", vec![power(60.0)], Some(true)),
        ("P 50, V None", vec![power(50.0)], None),
        (
            "present false P 7, then Supercharger",
            vec![present_power(false, 7.0), typed("Supercharger")],
            Some(true),
        ),
        (
            "Supercharger, then present false P 7",
            vec![typed("Supercharger"), present_power(false, 7.0)],
            Some(true),
        ),
        ("P 7, V None, present None", vec![power(7.0)], None),
        (
            "present true, MCSingleWireCAN",
            vec![with_present(true, "MCSingleWireCAN")],
            Some(false),
        ),
        (
            "present true, FASTCHARGERACSINGLEWIRECAN",
            vec![with_present(true, "FASTCHARGERACSINGLEWIRECAN")],
            Some(false),
        ),
        // Beyond the table: the other branches of R31.
        ("present true alone", vec![present(true)], Some(true)),
        (
            "present true, no-information type",
            vec![with_present(true, "Unknown")],
            Some(true),
        ),
        (
            "present true, unrecognised type",
            vec![with_present(true, "NewKind")],
            Some(true),
        ),
        ("present false alone", vec![present(false)], None),
        (
            "present false, P 0.9",
            vec![present_power(false, 0.9)],
            None,
        ),
        (
            "present false, P 1",
            vec![present_power(false, 1.0)],
            Some(false),
        ),
        (
            "present false, negative P",
            vec![present_power(false, -4.0)],
            None,
        ),
        ("CHAdeMO", vec![typed("CHAdeMO")], Some(true)),
        ("GB", vec![typed("GB")], Some(true)),
        ("SNA", vec![typed("SNA")], None),
        ("NewKind alone", vec![typed("NewKind")], None),
        (
            "Tesla AC type with a DC power",
            vec![{
                let mut sample = typed("ACSingleWireCAN");
                sample.charger_power_kw = Some(60.0);
                sample
            }],
            Some(true),
        ),
        (
            "DC type beats AC flag in one sample",
            vec![{
                let mut sample = typed("Combo");
                sample.fast_charger_present = Some(false);
                sample.charger_power_kw = Some(7.0);
                sample
            }],
            Some(true),
        ),
        (
            "DC is final over a later Tesla AC type",
            vec![typed("Combo"), typed("ACSingleWireCAN")],
            Some(true),
        ),
        (
            "AC then no evidence stays AC",
            vec![typed("ACSingleWireCAN"), s(1)],
            Some(false),
        ),
        (
            "no evidence then AC",
            vec![s(0), typed("ACSingleWireCAN")],
            Some(false),
        ),
        ("nothing", vec![s(0)], None),
        ("empty session", vec![], None),
    ];
    for (label, samples, want) in cases {
        assert_eq!(fold_kind(&samples), want, "T23 {label}");
    }
}

#[test]
fn t23_power_threshold_is_the_ac_ceiling_of_the_standards() {
    // K_AC_MAX_KW = 3 phases x 63 A x 305 V (R31, section 7).
    assert_eq!(K_AC_MAX_KW, 3.0 * 63.0 * K_V_MAX / 1000.0);
    let at = |kw: f64| {
        let mut sample = s(0);
        sample.charger_power_kw = Some(kw);
        sample
    };
    assert_eq!(
        sample_kind_evidence(&at(57.645)),
        None,
        "at the ceiling is not DC"
    );
    assert_eq!(
        sample_kind_evidence(&at(57.646)),
        Some(true),
        "above the ceiling is DC"
    );
    assert_eq!(
        sample_kind_evidence(&at(58.0)),
        Some(true),
        "whole-kW 58 is DC"
    );
    assert_eq!(sample_kind_evidence(&at(57.0)), None);
    assert_eq!(sample_kind_evidence(&at(f64::NAN)), None);
    assert_eq!(
        sample_kind_evidence(&at(f64::INFINITY)),
        None,
        "an infinite reading is not known"
    );
}

#[test]
fn t23_a_single_consistent_phase_count_is_ac_evidence() {
    // R31 AC evidence by R34: exactly one consistent count and P >= 1.
    let unique = vipr(Some(230.0), Some(16.0), Some(11.0), None);
    assert_eq!(sample_kind_evidence(&unique), Some(false));
    // Two consistent counts (V 230, I 6, P 4: {2, 3}) are not evidence.
    let ambiguous = vipr(Some(230.0), Some(6.0), Some(4.0), None);
    assert_eq!(sample_kind_evidence(&ambiguous), None);
    // No consistent count: not evidence.
    let none = vipr(Some(230.0), Some(16.0), Some(30.0), None);
    assert_eq!(sample_kind_evidence(&none), None);
    // P below 1 kW is not evidence even when one count is consistent.
    let tiny = vipr(Some(120.0), Some(5.0), Some(0.0), None);
    assert_eq!(sample_kind_evidence(&tiny), None);
    // Voltage outside the per-phase range (a DC pack voltage) is not evidence.
    let dc_voltage = vipr(Some(400.0), Some(16.0), Some(6.4), None);
    assert_eq!(sample_kind_evidence(&dc_voltage), None);
    // The kind rule does not read the phase label or the session.
    let labelled = vipr(Some(230.0), Some(6.0), Some(4.0), Some(3));
    assert_eq!(sample_kind_evidence(&labelled), None);
}

#[test]
fn t23_session_kind_table() {
    let cases = [
        (None, None, None),
        (None, Some(false), Some(false)),
        (None, Some(true), Some(true)),
        (Some(false), None, Some(false)),
        (Some(false), Some(false), Some(false)),
        (Some(false), Some(true), Some(true)),
        (Some(true), None, Some(true)),
        (Some(true), Some(false), Some(true)),
        (Some(true), Some(true), Some(true)),
    ];
    for (before, evidence, after) in cases {
        assert_eq!(
            session_kind_after(before, evidence),
            after,
            "{before:?} + {evidence:?}"
        );
    }
}

#[test]
fn t24_session_charger_type() {
    let cases: Vec<(&str, Vec<&str>, Option<&str>)> = vec![
        (
            "invalid then Supercharger",
            vec!["<invalid>", "Supercharger", "Supercharger"],
            Some("Supercharger"),
        ),
        ("invalid then SNA", vec!["<invalid>", "SNA"], None),
        (
            "Combo then Supercharger",
            vec!["Combo", "Supercharger"],
            Some("Combo"),
        ),
        (
            "Other then NewKind",
            vec!["FastChargerOther", "NewKind"],
            Some("NewKind"),
        ),
        ("empty then trimmed", vec!["", "  Combo  "], Some("Combo")),
        (
            "Unknown, Other, prefix only",
            vec!["Unknown", "Other", "FastCharger", " "],
            None,
        ),
        (
            "prefixed Unknown then CHAdeMO",
            vec!["FastChargerUnknown", "FastChargerCHAdeMO"],
            Some("FastChargerCHAdeMO"),
        ),
    ];
    for (label, texts, want) in cases {
        let samples = texts.into_iter().map(typed).collect::<Vec<_>>();
        assert_eq!(fold_type(&samples).as_deref(), want, "T24 {label}");
    }
    // A sample without a type never sets it and never clears it.
    let samples = vec![s(0), typed("Combo"), s(1)];
    assert_eq!(fold_type(&samples).as_deref(), Some("Combo"));
    assert_eq!(fold_type(&[]), None);
}

#[test]
fn t24_is_supercharger() {
    for text in [
        "Supercharger",
        "FastChargerSupercharger",
        "supercharger",
        " SUPERCHARGER ",
    ] {
        assert!(is_supercharger(Some(text)), "{text:?}");
    }
    for text in [
        "Combo",
        "SuperchargerX",
        "FastChargerFastChargerSupercharger",
        "Tesla Supercharger",
        "CHAdeMO",
        "ACSingleWireCAN",
        "<invalid>",
    ] {
        assert!(!is_supercharger(Some(text)), "{text:?}");
    }
    assert!(!is_supercharger(None));
}

#[test]
fn r31_charger_type_normalisation() {
    use FastChargerKind::*;
    let cases = [
        (None, NoInformation),
        (Some(""), NoInformation),
        (Some("   "), NoInformation),
        (Some("<invalid>"), NoInformation),
        (Some("<INVALID>"), NoInformation),
        (Some("Unknown"), NoInformation),
        (Some("unknown"), NoInformation),
        (Some("FastChargerUnknown"), NoInformation),
        (Some("SNA"), NoInformation),
        (Some("FastChargerSNA"), NoInformation),
        (Some("Other"), NoInformation),
        (Some("FastChargerOther"), NoInformation),
        (Some("FastCharger"), NoInformation),
        (Some("Supercharger"), Supercharger),
        (Some("FastChargerSupercharger"), Supercharger),
        (Some("FASTCHARGERSUPERCHARGER"), Supercharger),
        (Some("  fastchargersupercharger\n"), Supercharger),
        (Some("CHAdeMO"), OtherDc),
        (Some("chademo"), OtherDc),
        (Some("FastChargerCHAdeMO"), OtherDc),
        (Some("GB"), OtherDc),
        (Some("FastChargerGB"), OtherDc),
        (Some("Combo"), OtherDc),
        (Some("FastChargerCombo"), OtherDc),
        (Some("ACSingleWireCAN"), TeslaAc),
        (Some("FastChargerACSingleWireCAN"), TeslaAc),
        (Some("MCSingleWireCAN"), TeslaAc),
        (Some("mcsinglewirecan"), TeslaAc),
        (Some("FastChargerMCSingleWireCAN"), TeslaAc),
        // At most one prefix is stripped; anything else is unrecognised.
        (Some("FastChargerFastChargerCombo"), Unrecognised),
        (Some("FastChargerFastChargerSupercharger"), Unrecognised),
        (Some("Fast Charger Combo"), Unrecognised),
        (Some("SuperchargerX"), Unrecognised),
        (Some("GB/T"), Unrecognised),
        (Some("NewKind"), Unrecognised),
        (Some("FastChargerNewKind"), Unrecognised),
        (Some("FastCharger Combo"), Unrecognised),
        // Non-ASCII text must not panic where the prefix would split a character.
        (Some("FastChargé"), Unrecognised),
        (Some("é"), Unrecognised),
        (Some("FastCharg€r"), Unrecognised),
        (Some("FastChargerÇombo"), Unrecognised),
    ];
    for (text, want) in cases {
        assert_eq!(fast_charger_kind(text), want, "{text:?}");
    }
}

#[test]
fn t25_phase_switching() {
    // A real change during a session: the latest determination wins (R36).
    let first = sample_grid_power_kw(
        &vipr(Some(230.0), Some(16.0), Some(11.0), Some(3)),
        None,
        None,
    );
    assert!((first.power_kw.unwrap() - 11.04).abs() < TOLERANCE);
    assert_eq!(first.phase_count, Some(3));
    let session = first.phase_count.or(None);
    let second = sample_grid_power_kw(
        &vipr(Some(230.0), Some(6.0), Some(1.0), Some(1)),
        None,
        session,
    );
    assert!((second.power_kw.unwrap() - 1.38).abs() < TOLERANCE);
    assert_eq!(second.phase_count, Some(1));
    assert_eq!(second.phase_count.or(session), Some(1));
}

#[test]
fn t26_tie_broken_by_session_count() {
    let first = sample_grid_power_kw(&vipr(Some(230.0), Some(16.0), Some(11.0), None), None, None);
    assert!((first.power_kw.unwrap() - 11.04).abs() < TOLERANCE);
    assert_eq!(first.phase_count, Some(3));
    let session = first.phase_count.or(None);
    let second = sample_grid_power_kw(
        &vipr(Some(230.0), Some(6.0), Some(4.0), None),
        None,
        session,
    );
    assert!((second.power_kw.unwrap() - 4.14).abs() < TOLERANCE);
    // R36: a count taken from the session is not reported, so the session
    // count stays 3 and does not reinforce itself.
    assert_eq!(second.phase_count, None);
    assert_eq!(second.phase_count.or(session), Some(3));
    // The label outranks the session count; the result is reported (row 2).
    let third = sample_grid_power_kw(
        &vipr(Some(230.0), Some(6.0), Some(4.0), Some(2)),
        None,
        session,
    );
    assert!((third.power_kw.unwrap() - 2.0 * 230.0 * 6.0 / 1000.0).abs() < TOLERANCE);
    assert_eq!(third.phase_count, Some(2));
}

#[test]
fn r38_sample_functions_are_pure_and_leave_the_phase_label_as_reported() {
    // The car's label is stored as reported (R38); the functions only read it.
    let mut sample = vipr(Some(230.0), Some(16.0), Some(11.0), Some(1));
    sample.fast_charger_type = Some(" combo ".to_owned());
    let before = sample.clone();
    let first = sample_grid_power_kw(&sample, Some(false), Some(2));
    let second = sample_grid_power_kw(&sample, Some(false), Some(2));
    let _ = sample_kind_evidence(&sample);
    let _ = session_charger_type_after(None, &sample);
    assert_eq!(sample, before, "no function changes the sample");
    assert_eq!(first, second, "same inputs, same result (R2)");
    assert_eq!(
        sample.charger_phases,
        Some(1),
        "the label stays what the car reported"
    );
    assert_eq!(
        first.phase_count,
        Some(3),
        "the determined count differs from the label"
    );
}
