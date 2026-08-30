// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::teslamate_projection::{
    TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive, TeslaMatePosition,
    TeslaMateSourceWatermark, TeslaMateSourceWatermarks, TeslaMateState,
};

fn drive() -> TeslaMateDrive {
    TeslaMateDrive {
        id: 7,
        car_id: 1,
        start_date_ms: 1_000,
        end_date_ms: None,
        start_position_id: Some(1),
        end_position_id: None,
        start_address_id: None,
        end_address_id: None,
        start_geofence_id: None,
        end_geofence_id: None,
        outside_temp_avg: None,
        inside_temp_avg: None,
        speed_max: Some(20),
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
        start_km: Some(10.0),
        end_km: None,
        distance_km: None,
        duration_min: None,
        ascent: None,
        descent: None,
    }
}

fn position(id: i64, drive_id: Option<i64>) -> TeslaMatePosition {
    TeslaMatePosition {
        id,
        car_id: 1,
        drive_id,
        date_ms: id * 1_000,
        latitude: 51.0,
        longitude: -0.1,
        elevation: None,
        speed: Some(20),
        power: None,
        odometer: Some(10.0 + id as f64),
        ideal_battery_range_km: None,
        est_battery_range_km: None,
        rated_battery_range_km: None,
        battery_level: Some(80),
        usable_battery_level: None,
        fan_status: None,
        driver_temp_setting: None,
        passenger_temp_setting: None,
        is_climate_on: None,
        is_rear_defroster_on: None,
        is_front_defroster_on: None,
        outside_temp: None,
        inside_temp: None,
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        tpms_pressure_fl: None,
        tpms_pressure_fr: None,
        tpms_pressure_rl: None,
        tpms_pressure_rr: None,
    }
}

fn process() -> TeslaMateChargingProcess {
    TeslaMateChargingProcess {
        id: 8,
        car_id: 1,
        position_id: None,
        address_id: None,
        geofence_id: None,
        start_date_ms: 1_000,
        end_date_ms: None,
        charge_energy_added: Some(1.0),
        charge_energy_used_kwh: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_battery_level: Some(50),
        end_battery_level: None,
        duration_min: None,
        outside_temp_avg: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
        cost: None,
    }
}

fn sample(id: i64) -> TeslaMateCharge {
    TeslaMateCharge {
        id,
        charging_process_id: 8,
        date_ms: id * 1_000,
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        battery_level: Some(50),
        usable_battery_level: None,
        charge_energy_added_kwh: Some(id as f64),
        charger_actual_current: None,
        charger_phases: None,
        charger_pilot_current: None,
        charger_power_kw: None,
        charger_voltage: None,
        charge_cable: None,
        fast_charger_present: None,
        fast_charger_brand: None,
        fast_charger_type: None,
        ideal_range_km: None,
        rated_range_km: None,
        not_enough_power_to_heat: None,
        outside_temp_c: None,
    }
}

fn state() -> TeslaMateState {
    TeslaMateState {
        id: 20,
        car_id: 1,
        state: "online".into(),
        start_date_ms: 1_000,
        end_date_ms: None,
    }
}

fn watermarks(position: i64, charge: i64) -> TeslaMateSourceWatermarks {
    let position = TeslaMateSourceWatermark {
        max_id: Some(position),
        max_timestamp_ms: Some(position * 1_000),
    };
    let charge = TeslaMateSourceWatermark {
        max_id: Some(charge),
        max_timestamp_ms: Some(charge * 1_000),
    };
    TeslaMateSourceWatermarks {
        drives: TeslaMateSourceWatermark {
            max_id: Some(7),
            max_timestamp_ms: Some(1_000),
        },
        positions: position,
        charging_processes: TeslaMateSourceWatermark {
            max_id: Some(8),
            max_timestamp_ms: Some(1_000),
        },
        charges: charge,
        states: TeslaMateSourceWatermark {
            max_id: Some(20),
            max_timestamp_ms: Some(1_000),
        },
        updates: TeslaMateSourceWatermark::default(),
    }
}

fn open_session(
    position_ids: &[i64],
    sample_ids: &[i64],
    standalone_ids: &[i64],
) -> TeslaMateOpenSession {
    TeslaMateOpenSession {
        car_id: 1,
        drive: Some(drive()),
        drive_positions: position_ids
            .iter()
            .map(|id| position(*id, Some(7)))
            .collect(),
        charge: Some(process()),
        charge_samples: sample_ids.iter().map(|id| sample(*id)).collect(),
        state: Some(state()),
        standalone_positions: standalone_ids
            .iter()
            .map(|id| position(*id, None))
            .collect(),
        watermarks: watermarks(
            position_ids.iter().copied().max().unwrap_or_default(),
            sample_ids.iter().copied().max().unwrap_or_default(),
        ),
    }
}

#[test]
fn second_open_tail_is_merged_unsettled_restartable_and_idempotent() {
    let first = open_session(&[1, 2], &[10, 11], &[30]);
    let mut second = open_session(&[2, 3], &[11, 12], &[30, 31]);
    second.watermarks.positions.max_id = Some(999);
    second.watermarks.charges.max_id = Some(999);
    let cutover = reconcile_open_session_cutover(&first, &second).expect("cutover");
    assert!(cutover.cutover_unsettled);
    assert_eq!(cutover.session.drive_positions.len(), 3);
    assert_eq!(cutover.session.charge_samples.len(), 3);
    assert_eq!(cutover.session.standalone_positions.len(), 2);
    assert_eq!(cutover.session.watermarks.positions.max_id, Some(31));
    assert_eq!(cutover.session.watermarks.charges.max_id, Some(12));

    let data = crate::private_tempdir().expect("data");
    let store = HubStore::initialize(data.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "cutover"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "1"), 1_000)
        .expect("vehicle");
    store
        .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 1, &first, 1_000)
        .expect("first seed");
    store
        .seed_imported_open_session(
            source.source_id,
            vehicle.vehicle_id,
            1,
            &cutover.session,
            2_000,
        )
        .expect("second merge");
    let loaded = store
        .load_imported_open_session(source.source_id, vehicle.vehicle_id)
        .expect("load merged")
        .expect("merged session");
    assert_eq!(loaded.drive_positions.len(), 3);
    assert_eq!(loaded.charge_samples.len(), 3);
    assert_eq!(loaded.standalone_positions.len(), 2);
    assert!(
        store
            .seed_imported_open_session(
                source.source_id,
                vehicle.vehicle_id,
                1,
                &cutover.session,
                2_000,
            )
            .expect("duplicate merge")
            .no_op
    );

    drop(store);
    let reopened = HubStore::initialize(data.path()).expect("restart");
    let resumed = reopened
        .load_imported_open_session(source.source_id, vehicle.vehicle_id)
        .expect("load after restart")
        .expect("resumed session");
    assert_eq!(resumed.drive_positions.len(), 3);
    assert_eq!(resumed.charge_samples.len(), 3);
    assert_eq!(resumed.standalone_positions.len(), 2);

    let mut invalid = cutover.session.clone();
    invalid.drive_positions[0].car_id = 99;
    assert!(
        reopened
            .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 1, &invalid, 3_000,)
            .is_err()
    );
    let preserved = reopened
        .load_imported_open_session(source.source_id, vehicle.vehicle_id)
        .expect("load after failed merge")
        .expect("preserved session");
    assert_eq!(preserved.drive_positions.len(), 3);
    assert_eq!(preserved.charge_samples.len(), 3);
}

#[test]
fn open_to_closed_cutover_removes_provisional_parent_once() {
    let first = open_session(&[1, 2], &[10, 11], &[30]);
    let second = TeslaMateOpenSession {
        car_id: 1,
        watermarks: watermarks(3, 12),
        ..TeslaMateOpenSession::default()
    };
    let cutover = reconcile_open_session_cutover(&first, &second).expect("close cutover");
    assert!(!cutover.cutover_unsettled);
    assert!(cutover.session.drive.is_none());
    assert!(cutover.session.charge.is_none());
}

#[test]
fn unsettled_cutover_is_bounded_and_reported_for_retry() {
    let first = open_session(&[1, 2], &[10, 11], &[30]);
    let mut second = open_session(&[2, 3], &[11, 12], &[30, 31]);
    second.watermarks.positions.max_id = Some(999);
    let cutover = reconcile_open_session_cutover(&first, &second).expect("cutover");
    assert!(cutover.cutover_unsettled);
    assert_eq!(
        cutover
            .session
            .drive_positions
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        cutover
            .session
            .charge_samples
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![10, 11, 12]
    );
}

#[test]
fn direct_cutover_keeps_the_tail_from_its_history_snapshot() {
    let captured = open_session(&[1, 2], &[10, 11], &[30]);
    let observed_later = open_session(&[1, 2, 3], &[10, 11, 12], &[30, 31]);
    let cutover = reconcile_direct_snapshot_cutover(&captured, &observed_later)
        .expect("direct cutover witness");

    assert!(cutover.cutover_unsettled);
    assert_eq!(cutover.session, captured);
    assert_ne!(cutover.session, observed_later);
    assert!(matches!(
        require_settled_direct_cutover(&cutover),
        Err(TeslaMateImportError::CutoverUnsettled)
    ));

    let settled =
        reconcile_direct_snapshot_cutover(&captured, &captured).expect("settled direct cutover");
    require_settled_direct_cutover(&settled).expect("settled snapshot may publish");

    let mut completed_history_moved = captured.clone();
    completed_history_moved.watermarks.updates = TeslaMateSourceWatermark {
        max_id: Some(90),
        max_timestamp_ms: Some(4_000),
    };
    let changed_history = reconcile_direct_snapshot_cutover(&captured, &completed_history_moved)
        .expect("completed-history witness");
    assert!(changed_history.cutover_unsettled);

    let mut parent_updated = captured.clone();
    parent_updated.drive.as_mut().expect("open drive").speed_max = Some(99);
    let changed_parent =
        reconcile_direct_snapshot_cutover(&captured, &parent_updated).expect("open-parent witness");
    assert!(changed_parent.cutover_unsettled);

    let mut state_updated = captured.clone();
    state_updated.state.as_mut().expect("open state").state = "asleep".to_owned();
    assert!(
        reconcile_direct_snapshot_cutover(&captured, &state_updated)
            .expect("state-only witness")
            .cutover_unsettled
    );

    let empty = TeslaMateOpenSession {
        car_id: 1,
        ..TeslaMateOpenSession::default()
    };
    let mut short_completed_session = empty.clone();
    short_completed_session.watermarks.drives = TeslaMateSourceWatermark {
        max_id: Some(91),
        max_timestamp_ms: Some(5_000),
    };
    assert!(
        reconcile_direct_snapshot_cutover(&empty, &short_completed_session)
            .expect("short completed-session witness")
            .cutover_unsettled
    );
}
