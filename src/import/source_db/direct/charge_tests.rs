// SPDX-License-Identifier: AGPL-3.0-only

//! The direct import's per-process charge step: the imported session's kind,
//! charger type and peak power come out the same whatever order the binary
//! copy stream delivers the samples in. Expected values are written out, not
//! recomputed through the production fold.

use std::collections::HashMap;

use super::{ProjectionCharge, observe_charge_for_process};
use crate::source_db_projection::{
    ChargeProjectionFacts, SourceDbCharge, SourceDbChargingProcess, project_charge,
};

fn process(id: i64) -> SourceDbChargingProcess {
    SourceDbChargingProcess {
        id,
        car_id: 1,
        position_id: None,
        address_id: None,
        geofence_id: None,
        start_date_ms: 1_000,
        end_date_ms: Some(9_000),
        charge_energy_added: None,
        charge_energy_used_kwh: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_battery_level: None,
        end_battery_level: None,
        duration_min: None,
        outside_temp_avg: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
        cost: None,
    }
}

fn sample(
    id: i64,
    process_id: i64,
    date_ms: i64,
    present: Option<bool>,
    kind: Option<&str>,
    power: Option<f64>,
) -> SourceDbCharge {
    SourceDbCharge {
        id,
        charging_process_id: process_id,
        date_ms,
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        battery_level: None,
        usable_battery_level: None,
        charge_energy_added_kwh: None,
        charger_actual_current: None,
        charger_phases: None,
        charger_pilot_current: None,
        charger_power_kw: power,
        charger_voltage: None,
        charge_cable: None,
        fast_charger_present: present,
        fast_charger_brand: None,
        fast_charger_type: kind.map(str::to_owned),
        ideal_range_km: None,
        rated_range_km: None,
        not_enough_power_to_heat: None,
        outside_temp_c: None,
    }
}

/// Feed `stream` through the direct import's per-process step, then project
/// the process it names.
fn direct_import(stream: &[SourceDbCharge], process_id: i64) -> ProjectionCharge {
    let mut facts = HashMap::<i64, ChargeProjectionFacts>::new();
    for row in stream {
        observe_charge_for_process(row, &mut facts);
    }
    let facts = facts.get(&process_id).expect("the process has samples");
    project_charge(&process(process_id), 1, None, None, None, facts).unwrap()
}

fn reversed(stream: &[SourceDbCharge]) -> Vec<SourceDbCharge> {
    stream.iter().rev().cloned().collect()
}

#[test]
fn direct_import_dc_session_reports_kind_type_and_peak() {
    // Arrival order is neither time nor id order.
    let stream = vec![
        sample(40, 7, 3_000, None, Some(" Combo "), Some(62.5)),
        sample(10, 7, 2_000, Some(true), None, Some(18.0)),
        sample(30, 7, 5_000, None, None, Some(41.0)),
        sample(20, 7, 4_000, None, None, None),
    ];
    for stream in [stream.clone(), reversed(&stream)] {
        let row = direct_import(&stream, 7);
        assert_eq!(row.is_dc, Some(true));
        assert_eq!(row.fast_charger_type.as_deref(), Some("Combo"));
        assert_eq!(row.max_charger_power_kw, Some(62.5));
    }
}

#[test]
fn direct_import_ac_session_is_not_dc_and_has_no_type() {
    let stream = vec![
        sample(1, 8, 2_000, Some(false), None, Some(7.4)),
        sample(2, 8, 3_000, Some(false), Some("Unknown"), Some(11.0)),
    ];
    for stream in [stream.clone(), reversed(&stream)] {
        let row = direct_import(&stream, 8);
        assert_eq!(row.is_dc, Some(false));
        assert_eq!(row.fast_charger_type, None);
        assert_eq!(row.max_charger_power_kw, Some(11.0));
    }
}

#[test]
fn direct_import_earliest_informative_type_wins_across_arrival_order() {
    let early = sample(90, 9, 2_000, None, Some("FutureConnector"), None);
    let late = sample(10, 9, 3_000, None, Some("Combo"), Some(50.0));
    for stream in [vec![early.clone(), late.clone()], vec![late, early]] {
        let row = direct_import(&stream, 9);
        assert_eq!(row.fast_charger_type.as_deref(), Some("FutureConnector"));
        assert_eq!(row.is_dc, Some(true));
        assert_eq!(row.max_charger_power_kw, Some(50.0));
    }
}

#[test]
fn direct_import_keeps_interleaved_processes_apart() {
    let stream = vec![
        sample(1, 11, 2_000, Some(true), Some("Combo"), Some(120.0)),
        sample(2, 12, 2_100, Some(false), None, Some(7.0)),
        sample(3, 11, 2_200, None, None, Some(90.0)),
        sample(4, 12, 2_300, None, None, None),
    ];
    let dc = direct_import(&stream, 11);
    assert_eq!(dc.is_dc, Some(true));
    assert_eq!(dc.fast_charger_type.as_deref(), Some("Combo"));
    assert_eq!(dc.max_charger_power_kw, Some(120.0));
    let ac = direct_import(&stream, 12);
    assert_eq!(ac.is_dc, Some(false));
    assert_eq!(ac.fast_charger_type, None);
    assert_eq!(ac.max_charger_power_kw, Some(7.0));
}

#[test]
fn direct_import_session_without_evidence_stays_null() {
    let row = direct_import(&[sample(1, 13, 2_000, None, None, None)], 13);
    assert_eq!(row.is_dc, None);
    assert_eq!(row.fast_charger_type, None);
    assert_eq!(row.max_charger_power_kw, None);
}
