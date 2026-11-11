// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::runtime::lifecycle::{charge_display_aggregates, charge_session_kind};

fn sample(id: i64, time: i64, text: Option<&str>, power: Option<f64>) -> SourceDbCharge {
    let mut row = super::tests::history().charges.remove(0);
    row.id = id;
    row.date_ms = time;
    row.fast_charger_type = text.map(str::to_owned);
    row.charger_power_kw = power;
    row.fast_charger_present = None;
    row.charger_voltage = None;
    row.charger_actual_current = None;
    row.charger_phases = None;
    row
}

fn process() -> SourceDbChargingProcess {
    let mut row = super::tests::history().charging_processes.remove(0);
    row.position_id = None;
    row.address_id = None;
    row.geofence_id = None;
    row.start_date_ms = 100;
    row.end_date_ms = Some(200);
    row
}

fn facts(samples: &[SourceDbCharge]) -> ChargeProjectionFacts {
    ChargeProjectionFacts::from_samples(&samples.iter().collect::<Vec<_>>())
}

fn projected(samples: &[SourceDbCharge]) -> ProjectionCharge {
    project_charge(&process(), 1, None, None, None, &facts(samples)).unwrap()
}

#[test]
fn ind12_power_does_not_gate_type_and_peak_is_largest_finite() {
    for power in [Some(0.0), None, Some(-3.0), Some(f64::NAN)] {
        let row = projected(&[sample(1, 100, Some(" Combo "), power)]);
        assert_eq!(row.fast_charger_type.as_deref(), Some("Combo"));
        assert_eq!(row.max_charger_power_kw, power.filter(|p| p.is_finite() && *p >= 0.0));
    }
    let readings = [None, Some(-2.0), Some(f64::NEG_INFINITY), Some(f64::INFINITY), Some(f64::NAN)];
    let mut samples: Vec<_> = readings.into_iter().enumerate()
        .map(|(i, power)| sample(i as i64 + 1, 100, None, power)).collect();
    assert_eq!(projected(&samples).max_charger_power_kw, None);
    for (id, power) in [(10, 7.0), (11, 23.5), (12, 11.0), (13, 0.0)] {
        samples.push(sample(id, 100, None, Some(power)));
    }
    assert_eq!(projected(&samples).max_charger_power_kw, Some(23.5));
    assert_eq!(projected(&[]).max_charger_power_kw, None);
}

#[test]
fn ind12_no_information_types_give_null() {
    let texts = ["", "<invalid>", "Unknown", "SNA", "Other", "Tesla"];
    let mut samples = vec![sample(1, 1, None, Some(7.0))];
    for text in texts {
        for text in [text.to_owned(), format!("FastCharger{text}")] {
            for text in [text.to_ascii_uppercase(), text.to_ascii_lowercase()] {
                samples.push(sample(samples.len() as i64 + 1, 2, Some(&format!(" {text} ")), Some(7.0)));
            }
        }
    }
    assert_eq!(projected(&samples).fast_charger_type, None);
    samples.push(sample(100, 3, Some("NewConnector"), None));
    assert_eq!(projected(&samples).fast_charger_type.as_deref(), Some("NewConnector"));
}

#[test]
fn ind12_earliest_informative_type_wins() {
    let early = sample(90, 100, Some(" FutureConnector "), None);
    let late = sample(10, 200, Some("Combo"), Some(50.0));
    for samples in [vec![early.clone(), late.clone()], vec![late, early.clone()]] {
        assert_eq!(projected(&samples).fast_charger_type.as_deref(), Some("FutureConnector"));
    }
    let tied = sample(80, 100, Some(" CHAdeMO "), Some(0.0));
    for samples in [vec![early.clone(), tied.clone()], vec![tied, early]] {
        assert_eq!(projected(&samples).fast_charger_type.as_deref(), Some("CHAdeMO"));
    }
}

fn every_permutation(rows: &mut [SourceDbCharge], at: usize, check: &mut impl FnMut(&[SourceDbCharge])) {
    if at == rows.len() {
        check(rows);
        return;
    }
    for i in at..rows.len() {
        rows.swap(at, i);
        every_permutation(rows, at + 1, check);
        rows.swap(at, i);
    }
}

#[test]
fn ind12_any_order_equals_ordered_c2_fold() {
    let mut seed = 0x12c2_90ab_5678_1234_u64;
    let mut draw = |bound: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 32) as usize) % bound
    };
    let texts = [None, Some("Unknown"), Some("FastChargerTesla"), Some("Combo"), Some(" ACSingleWireCAN "), Some("NewType")];
    let powers = [None, Some(-1.0), Some(0.0), Some(1.0), Some(7.0), Some(58.0), Some(f64::NAN), Some(f64::INFINITY)];
    for sequence in 0..48 {
        let mut samples: Vec<_> = (0..draw(6)).map(|i| {
            let mut row = sample(i as i64 + 1, 50 + draw(4) as i64 * 75, texts[draw(texts.len())], powers[draw(powers.len())]);
            row.fast_charger_present = [None, Some(true), Some(false)][draw(3)];
            row.charger_phases = [None, Some(1), Some(3)][draw(3)];
            row.charger_voltage = [None, Some(230.0), Some(400.0)][draw(3)];
            row.charger_actual_current = [None, Some(16.0), Some(32.0)][draw(3)];
            row
        }).collect();
        let mut ordered: Vec<_> = samples.iter().map(project_charge_sample).collect();
        ordered.sort_by_key(|row| (row.timestamp_ms, row.id));
        let kind = charge_session_kind(&ordered);
        let peak = charge_display_aggregates(&ordered).max_charger_power_kw;
        let canonical = facts(&samples);
        every_permutation(&mut samples, 0, &mut |permutation| {
            let actual = facts(permutation);
            assert_eq!(actual, canonical, "sequence {sequence}");
            let row = projected(permutation);
            assert_eq!(row.is_dc, kind.is_dc, "sequence {sequence}");
            assert_eq!(row.fast_charger_type, kind.fast_charger_type, "sequence {sequence}");
            assert_eq!(row.max_charger_power_kw, peak, "sequence {sequence}");
            assert_eq!(row.start_date_ms, ordered.first().map_or(100, |row| 100.min(row.timestamp_ms)));
            assert_eq!(row.end_date_ms, Some(ordered.last().map_or(200, |row| 200.max(row.timestamp_ms))));
        });
    }
}

#[test]
fn ind12_three_callers_agree_on_reversed_ids() {
    use crate::source_db_direct::observe_charge_for_process;
    use crate::source_db_fragments::project_charge_from_stage;
    use crate::source_db_stage::{SourceDbStage, SourceDbStageLimits, SourceDbStageTable};

    let process = process();
    let samples = vec![sample(10, 250, Some("Combo"), Some(70.0)), sample(90, 50, Some(" FutureConnector "), None)];
    let mut source = super::tests::history();
    source.charging_processes = vec![process.clone()];
    source.charges = samples.clone();
    let memory = project_vehicle(&source, 1).unwrap().snapshot.charges.remove(0);
    let temporary = crate::private_tempdir().unwrap();
    let mut stage = SourceDbStage::create(temporary.path().join("stage"), SourceDbStageLimits {
        max_rows: 20, max_stage_bytes: 1024 * 1024, minimum_free_bytes: 0,
    }).unwrap();
    for row in &samples {
        stage.insert(SourceDbStageTable::Charges, row.id, row).unwrap();
    }
    stage.seal().unwrap();
    let (staged, staged_facts) = project_charge_from_stage(&stage, &process, 1).unwrap();
    let mut direct_facts = HashMap::new();
    for row in &samples {
        observe_charge_for_process(row, &mut direct_facts);
    }
    assert_eq!(direct_facts.len(), 1);
    assert_eq!(direct_facts[&process.id], staged_facts);
    let direct = project_charge(&process, 1, None, None, None, &direct_facts[&process.id]).unwrap();
    assert_eq!(memory, staged);
    assert_eq!(memory, direct);
    assert_eq!(memory.fast_charger_type.as_deref(), Some("FutureConnector"));
    assert_eq!(memory.is_dc, Some(true));
    assert_eq!(memory.max_charger_power_kw, Some(70.0));
    assert_eq!((memory.start_date_ms, memory.end_date_ms), (50, Some(250)));

}

#[test]
fn ind12_dc_evidence_anywhere_wins() {
    let mut ac = sample(1, 100, None, Some(7.0));
    ac.fast_charger_present = Some(false);
    let quiet = sample(2, 150, None, None);
    assert_eq!(projected(&[quiet.clone()]).is_dc, None);
    assert_eq!(projected(&[ac.clone(), quiet.clone()]).is_dc, Some(false));
    for dc in [sample(3, 200, Some("Supercharger"), None), sample(3, 200, None, Some(58.0)), {
        let mut row = sample(3, 200, None, None);
        row.fast_charger_present = Some(true);
        row
    }] {
        for samples in [vec![ac.clone(), quiet.clone(), dc.clone()], vec![dc, quiet.clone(), ac.clone()]] {
            assert_eq!(projected(&samples).is_dc, Some(true));
        }
    }
    assert_eq!(projected(&[sample(1, 100, Some("ACSingleWireCAN"), None)]).is_dc, Some(false));
    for power in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(projected(&[sample(1, 100, None, Some(power))]).is_dc, None);
    }
}

#[test]
fn ind12_null_process_columns_stay_null() {
    let mut process = process();
    let samples = [sample(1, 50, Some("Combo"), Some(7.0)), sample(2, 250, None, Some(4.0))];
    let facts = facts(&samples);
    process.charge_energy_added = None;
    process.end_battery_level = None;
    process.start_rated_range_km = None;
    process.end_rated_range_km = None;
    let row = project_charge(&process, 1, None, None, None, &facts).unwrap();
    assert_eq!(row.charge_energy_added, None);
    assert_eq!(row.end_battery_level, None);
    assert_eq!(row.start_rated_range_km, None);
    assert_eq!(row.end_rated_range_km, None);
    process.charge_energy_added = Some(12.5);
    process.end_battery_level = Some(42);
    process.start_rated_range_km = Some(120.0);
    process.end_rated_range_km = Some(180.0);
    let row = project_charge(&process, 1, None, None, None, &facts).unwrap();
    assert_eq!(row.charge_energy_added, Some(12.5));
    assert_eq!(row.end_battery_level, Some(42));
    assert_eq!(row.start_rated_range_km, Some(120.0));
    assert_eq!(row.end_rated_range_km, Some(180.0));
    let empty = project_charge(&process, 1, None, None, None, &ChargeProjectionFacts::default()).unwrap();
    assert_eq!((empty.start_date_ms, empty.end_date_ms), (100, Some(200)));
    process.end_date_ms = None;
    assert_eq!(project_charge(&process, 1, None, None, None, &facts).unwrap().end_date_ms, None);
}

#[test]
fn ind12_closed_import_kind_equals_c2_t90() {
    let mut source = super::tests::history();
    source.charging_processes = vec![process()];
    source.charges = vec![sample(10, 200, Some("Combo"), Some(100.0)), sample(90, 100, Some(" ACSingleWireCAN "), None)];
    let mut ordered: Vec<_> = source.charges.iter().map(project_charge_sample).collect();
    ordered.sort_by_key(|row| (row.timestamp_ms, row.id));
    let expected = charge_session_kind(&ordered);
    let imported = project_vehicle(&source, 1).unwrap();
    let served = &imported.snapshot.charges[0];
    assert_eq!(served.is_dc, expected.is_dc);
    assert_eq!(served.fast_charger_type, expected.fast_charger_type);
    assert_eq!(served.is_dc, Some(true));
    assert_eq!(served.fast_charger_type.as_deref(), Some("ACSingleWireCAN"));
}

#[test]
fn ind12_efficiency_unchanged() {
    for (input, expected) in [(None, None), (Some(0.25), Some(250.0)), (Some(160.0), Some(160.0)), (Some(0.0), Some(0.0)), (Some(1.0), Some(1.0))] {
        assert_eq!(normalise_efficiency(input), Ok(expected));
    }
    for value in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(normalise_efficiency(Some(value)), Err(SourceDbProjectionError::InvalidValue { field: "car.efficiency" }));
    }
}
