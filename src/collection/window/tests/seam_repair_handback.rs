// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T20: a session still open when coverage is handed back is closed at
//! its last evidence through the lifecycle, once, on every handback path the
//! real controller and import admission take: window expiry, operator close
//! and the sealing of a window found open in a restored store.

use std::path::Path;

use rusqlite::{Connection, params};

use super::import_seam_coverage::{
    controller_at, coverage_row, data_dir, home, raw, restore_elsewhere, store_with_vehicles,
};
use super::support::*;
use crate::collection::window::{CloseReason, Phase};
use crate::db::hub_ids::HUB_ID_BASE;
use crate::db::{HubStore, LifecycleCommit};
use crate::runtime::operator::admission::admit_import;

const SECOND: i64 = 1000;
const SESSION: i64 = HUB_ID_BASE + 1;

/// What the collector leaves when the window ends mid-drive or mid-charge:
/// the session in the lifecycle's open state, its ten children (one each ten
/// seconds from `first_ms`) in the open rows. Returns the last child's instant.
fn collect_open_session(store: &HubStore, vehicle: &str, drive: bool, first_ms: i64) -> i64 {
    let vehicle_id: uuid::Uuid = vehicle.parse().unwrap();
    let mut state = crate::lifecycle::OpenSessionState::new();
    let instants: Vec<i64> = (0..10).map(|step| first_ms + step * 10 * SECOND).collect();
    let last_ms = *instants.last().unwrap();
    let mut delta = crate::lifecycle::LifecycleDelta::default();
    if drive {
        let mut open: crate::lifecycle::OpenDrive = serde_json::from_value(serde_json::json!({
            "id": SESSION, "car_id": 1, "start_date_ms": first_ms,
            "outside_temp_sum": 0.0, "outside_temp_count": 0,
            "first_odometer": 1000.0, "last_odometer": 1004.0
        }))
        .unwrap();
        open.position_count = 10;
        open.last_position_date_ms = Some(last_ms);
        open.last_folded_at_ms = Some(last_ms);
        open.last_motion_at_ms = Some(last_ms);
        state.open_drive = Some(open);
        state.phase = crate::lifecycle::VehiclePhase::Driving;
        delta.open_drive_positions = instants
            .iter()
            .enumerate()
            .map(|(index, at)| {
                serde_json::from_value(serde_json::json!({
                    "id": HUB_ID_BASE + 1 + index as i64, "drive_id": SESSION, "car_id": 1,
                    "date_ms": at, "latitude": 47.5, "longitude": 19.0
                }))
                .unwrap()
            })
            .collect();
    } else {
        let mut open: crate::lifecycle::OpenCharge = serde_json::from_value(serde_json::json!({
            "id": SESSION, "car_id": 1, "start_date_ms": first_ms,
            "start_battery_level": 40, "outside_temp_sum": 0.0, "outside_temp_count": 0
        }))
        .unwrap();
        open.sample_count = 10;
        open.last_sample_timestamp_ms = Some(last_ms);
        state.open_charge = Some(open);
        state.phase = crate::lifecycle::VehiclePhase::Charging;
        delta.open_charge_samples = instants
            .iter()
            .enumerate()
            .map(|(index, at)| {
                serde_json::from_value(serde_json::json!({
                    "id": HUB_ID_BASE + 1 + index as i64, "charge_process_id": SESSION,
                    "timestamp_ms": at, "battery_level": 50
                }))
                .unwrap()
            })
            .collect();
    }
    let encoded = state.encode().unwrap();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id,
            car_id: 1,
            open_session_json: &encoded,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: first_ms,
            delta: &delta,
        })
        .unwrap();
    last_ms
}

/// The vehicle's closed Hub session (id, start, end), the parent of each
/// stored child, its open rows, and the lifecycle's observation cursor (the
/// applied `handback` event is pruned like any applied observation; the
/// cursor moving from 1 to 2 shows it was applied once).
#[derive(Debug, PartialEq, Eq)]
struct Closed {
    sessions: Vec<(i64, i64, i64)>,
    children: Vec<Option<i64>>,
    open_rows: i64,
    cursor: i64,
}

fn closed(connection: &Connection, vehicle: &str, drive: bool) -> Closed {
    let (sessions, children) = if drive {
        (
            "SELECT drive_id, CAST(json_extract(drive_json, '$.start_date_ms') AS INTEGER),
                    CAST(json_extract(drive_json, '$.end_date_ms') AS INTEGER)
               FROM materialised_drives WHERE vehicle_id = ?1 ORDER BY drive_id",
            "SELECT drive_id FROM materialised_positions WHERE vehicle_id = ?1 ORDER BY position_id",
        )
    } else {
        (
            "SELECT charge_id, CAST(json_extract(charge_json, '$.start_date_ms') AS INTEGER),
                    CAST(json_extract(charge_json, '$.end_date_ms') AS INTEGER)
               FROM materialised_charges WHERE vehicle_id = ?1 ORDER BY charge_id",
            "SELECT charge_id FROM materialised_charge_samples WHERE vehicle_id = ?1 ORDER BY sample_id",
        )
    };
    let rows = |sql: &str| -> Vec<(i64, i64, i64)> {
        let mut statement = connection.prepare(sql).unwrap();
        statement
            .query_map([vehicle], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let parents = |sql: &str| -> Vec<Option<i64>> {
        let mut statement = connection.prepare(sql).unwrap();
        statement
            .query_map([vehicle], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let count = |sql: &str| -> i64 { connection.query_row(sql, params![vehicle], |row| row.get(0)).unwrap() };
    Closed {
        sessions: rows(sessions),
        children: parents(children),
        open_rows: count("SELECT COUNT(*) FROM lifecycle_open_rows WHERE vehicle_id = ?1"),
        cursor: count("SELECT last_observation_id FROM vehicle_lifecycle_state WHERE vehicle_id = ?1"),
    }
}

fn expected(first_ms: i64, last_ms: i64) -> Closed {
    Closed {
        sessions: vec![(SESSION, first_ms, last_ms)],
        children: vec![Some(SESSION); 10],
        open_rows: 0,
        cursor: 2,
    }
}

/// The session is still open: nothing stored, its ten open rows, no event.
fn still_open() -> Closed {
    Closed {
        sessions: Vec::new(),
        children: Vec::new(),
        open_rows: 10,
        cursor: 1,
    }
}

#[test]
fn seam_repair_handback_operator_close_closes_the_open_drive() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let (store, vehicle, _) = store_with_vehicles(&rig);
    let connection = store.open().unwrap();
    assert_eq!(hub.open(&rig.grant(0, 600)).unwrap().phase, Phase::Open);
    let first_ms = (T0 + 60) * SECOND;
    let last_ms = collect_open_session(&store, &vehicle, true, first_ms);
    rig.clock.advance(300);
    // While the window is open nothing is closed.
    assert_eq!(hub.tick().unwrap(), Phase::Open);
    assert_eq!(closed(&connection, &vehicle, true), still_open());

    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap().expect("a handback");
    assert_eq!(coverage_row(&connection, 1).0, "handback");
    assert_eq!(closed(&connection, &vehicle, true), expected(first_ms, last_ms));
    // Import admission mirrors the same handback: nothing closes twice.
    drop(admit_import(Some(&data_dir(&rig)), &home(&rig)).unwrap());
    assert_eq!(closed(&connection, &vehicle, true), expected(first_ms, last_ms));
}

#[test]
fn seam_repair_handback_expiry_closes_the_open_charge() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let (store, vehicle, _) = store_with_vehicles(&rig);
    let connection = store.open().unwrap();
    assert_eq!(hub.open(&rig.grant(0, 600)).unwrap().phase, Phase::Open);
    let first_ms = (T0 + 60) * SECOND;
    let last_ms = collect_open_session(&store, &vehicle, false, first_ms);
    rig.clock.advance(601);
    assert_eq!(hub.tick().unwrap(), Phase::Closing);
    let handback = hub.finish_close().unwrap().expect("a handback");
    assert_eq!(handback.reason, CloseReason::Expired);
    assert_eq!(closed(&connection, &vehicle, false), expected(first_ms, last_ms));
    drop(admit_import(Some(&data_dir(&rig)), &home(&rig)).unwrap());
    assert_eq!(closed(&connection, &vehicle, false), expected(first_ms, last_ms));
}

#[test]
fn seam_repair_handback_restored_window_closes_once_on_either_path() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let (store, vehicle, _) = store_with_vehicles(&rig);
    // The backup is taken inside the window, mid-drive.
    assert_eq!(hub.open(&rig.grant(0, 3000)).unwrap().phase, Phase::Open);
    let first_ms = (T0 + 60) * SECOND;
    let last_ms = collect_open_session(&store, &vehicle, true, first_ms);
    let check = |directory: &Path, path: &str| {
        let connection = raw(directory);
        assert_eq!(coverage_row(&connection, 1).0, "restore", "{path}");
        assert_eq!(closed(&connection, &vehicle, true), expected(first_ms, last_ms), "{path}");
    };

    // A host with no window open starts on the restored store and seals it.
    let restored = restore_elsewhere(&rig, &store, "new-host");
    assert_eq!(closed(&raw(&restored), &vehicle, true), still_open());
    let new_host = controller_at(&rig, &restored);
    assert_eq!(new_host.status().phase, Phase::Closed);
    check(&restored, "new host");
    drop(new_host);
    let again = controller_at(&rig, &restored);
    check(&restored, "new host restarted");
    drop(again);

    // The original host restarts on its own open window: it seals nothing and
    // its drive stays open.
    drop(hub);
    let (resumed, _) = rig.start_hub();
    assert_eq!(resumed.status().phase, Phase::Open);
    let connection = store.open().unwrap();
    assert_eq!(coverage_row(&connection, 1).0, "open");
    assert_eq!(closed(&connection, &vehicle, true), still_open());
    drop(resumed);

    // Import admission seals a restored store no service has started on.
    let other = restore_elsewhere(&rig, &store, "admission");
    drop(admit_import(Some(&other), &home(&rig)).unwrap());
    check(&other, "admission");
    drop(admit_import(Some(&other), &home(&rig)).unwrap());
    check(&other, "second admission");
}

/// M03-T22 (DEC-140): an older handback whose coverage write failed is
/// retried only while no window is Pending or Open. A later window that
/// collects an active drive and charge meanwhile keeps both open, through the
/// retry and a service restart, until its own handback closes them at their
/// last evidence; the older handback then reaches the store and closes nothing.
#[test]
fn seam_repair_handback_retry_waits_for_a_later_window_to_close() {
    use crate::collection::window::controller::fail_next_coverage_handback;
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let (store, driving, charging) = store_with_vehicles(&rig);
    let connection = store.open().unwrap();

    // The first window's handback does not reach the store's coverage.
    assert_eq!(hub.open(&rig.grant(0, 600)).unwrap().phase, Phase::Open);
    rig.clock.advance(60);
    hub.close(CloseReason::OperatorClose);
    fail_next_coverage_handback();
    hub.finish_close().unwrap().expect("a handback");
    assert_eq!(coverage_row(&connection, 1).0, "open");
    assert!(hub.coverage_unreconciled());

    // A later window opens (past the issuer's gap) and collects a drive and
    // a charge.
    rig.clock.advance(1440);
    assert_eq!(hub.open(&rig.grant(0, 3000)).unwrap().phase, Phase::Open);
    let first_ms = (T0 + 1560) * SECOND;
    let drive_last_ms = collect_open_session(&store, &driving, true, first_ms);
    let charge_last_ms = collect_open_session(&store, &charging, false, first_ms);
    rig.clock.advance(300);
    assert_eq!(hub.tick().unwrap(), Phase::Open);

    // The retry waits: the older handback stays unwritten, the sessions open.
    assert!(!hub.reconcile_coverage(), "no retry while a window is open");
    assert!(hub.coverage_unreconciled());
    assert_eq!(coverage_row(&connection, 1).0, "open");
    assert_eq!(closed(&connection, &driving, true), still_open());
    assert_eq!(closed(&connection, &charging, false), still_open());

    // A restart inside the later window defers the retry the same way.
    drop(hub);
    let (hub, _) = rig.start_hub();
    assert_eq!(hub.status().phase, Phase::Open);
    assert!(hub.coverage_unreconciled());
    assert!(!hub.reconcile_coverage());
    assert_eq!(coverage_row(&connection, 1).0, "open");
    assert_eq!(closed(&connection, &driving, true), still_open());
    assert_eq!(closed(&connection, &charging, false), still_open());

    // The later window's own handback closes them at their last evidence.
    rig.clock.advance(60);
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap().expect("a handback");
    assert_eq!(coverage_row(&connection, 2).0, "handback");
    assert_eq!(closed(&connection, &driving, true), expected(first_ms, drive_last_ms));
    assert_eq!(closed(&connection, &charging, false), expected(first_ms, charge_last_ms));

    // After the close the retry writes the older handback and closes nothing.
    assert!(hub.reconcile_coverage());
    assert_eq!(coverage_row(&connection, 1).0, "handback");
    assert_eq!(closed(&connection, &driving, true), expected(first_ms, drive_last_ms));
    assert_eq!(closed(&connection, &charging, false), expected(first_ms, charge_last_ms));
}
