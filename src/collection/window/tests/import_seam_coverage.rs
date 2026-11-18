// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T13: the collection coverage is kept in the store beside the window
//! ledger, so a backup, a restore and a new host carry the import cutoff. Every
//! test drives the real controller and the real store.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, params};

use super::support::*;
use crate::collection::window::{CloseReason, Phase, WindowController, WindowDeps};
use crate::data_recovery::{create_data_backup, restore_data_backup};
use crate::db::hub_ids::HUB_ID_BASE;
use crate::db::{HubStore, SourceDescriptor, VehicleDescriptor};
use crate::import::source_db::seam::{HubCoverage, SeamError};
use crate::runtime::operator::admission::admit_import;

const SECOND: i64 = 1000;

pub(super) fn data_dir(rig: &Rig) -> PathBuf {
    rig.tmp.path().join("data")
}

pub(super) fn home(rig: &Rig) -> PathBuf {
    rig.tmp.path().join("home")
}

/// A store in the rig's data directory with two registered vehicles.
pub(super) fn store_with_vehicles(rig: &Rig) -> (HubStore, String, String) {
    // The controller made the data directory on the way to its window
    // directory; the store wants it private to the user.
    std::fs::create_dir_all(data_dir(rig)).unwrap();
    std::fs::set_permissions(data_dir(rig), std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = HubStore::initialize(data_dir(rig)).unwrap();
    let source = store
        .register_source(&SourceDescriptor::new("owner_api", "import-seam-coverage"), 1_000)
        .unwrap();
    let register = |tesla_id: i64| {
        store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, tesla_id.to_string())
                    .with_tesla_identity(Some(tesla_id), None),
                1_000,
            )
            .unwrap()
            .vehicle_id
            .to_string()
    };
    let first = register(91);
    let second = register(92);
    (store, first, second)
}

fn coverage_of(connection: &Connection, vehicle: &str) -> Vec<(i64, i64)> {
    HubCoverage::for_store(connection, vehicle)
        .unwrap()
        .intervals()
        .to_vec()
}

pub(super) fn coverage_row(connection: &Connection, seq: i64) -> (String, Option<i64>, String) {
    connection
        .query_row(
            "SELECT sealed, closed_at_ms, gaps_json FROM collection_coverage WHERE seq = ?1",
            [seq],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

/// Opens a window now, lets `seconds` pass, closes it and hands it back.
fn run_window(rig: &Rig, hub: &WindowController, start_in: u64, seconds: u64) {
    if start_in > 0 {
        hub.open(&rig.grant(start_in as i64, 600)).unwrap();
        rig.clock.advance(start_in);
        assert_eq!(hub.tick().unwrap(), Phase::Open);
    } else {
        assert_eq!(hub.open(&rig.grant(0, 600)).unwrap().phase, Phase::Open);
    }
    rig.clock.advance(seconds);
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap().expect("a handback");
}

fn add_hub_position(connection: &Connection, vehicle: &str, offset: i64, date_ms: i64) {
    connection
        .execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
             VALUES (?1, ?2, NULL, 1, ?3)",
            params![vehicle, HUB_ID_BASE + offset, format!("{{\"date_ms\": {date_ms}}}")],
        )
        .unwrap();
}

/// Backs the store up and restores it into a data directory of its own.
pub(super) fn restore_elsewhere(rig: &Rig, store: &HubStore, name: &str) -> PathBuf {
    let backup = rig.tmp.path().join(format!("backup-{name}"));
    create_data_backup(store, &backup).unwrap();
    let restored = rig.tmp.path().join(format!("restored-{name}"));
    restore_data_backup(&backup, &restored).unwrap();
    restored
}

pub(super) fn raw(directory: &Path) -> Connection {
    Connection::open(directory.join("hub.sqlite")).unwrap()
}

/// A controller on a window directory of its own (a new host).
pub(super) fn controller_at(rig: &Rig, data: &Path) -> WindowController {
    let (hub, _) = WindowController::start(
        crate::collection::window::WindowConfig {
            dir: data.join("window"),
            claim_path: data.join("claim.lock"),
            host_id: HOST.to_owned(),
        },
        WindowDeps {
            clock: rig.clock.clone(),
            anchor: rig.anchor.clone(),
            probe: rig.probe.clone(),
            gate: crate::collection::window::EgressGate::new(rig.clock.clone()),
            handback: std::sync::Arc::new(crate::collection::window::NoHighWater),
        },
    );
    hub
}

#[test]
fn import_seam_restore_keeps_the_cutoff() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let (store, vehicle, _) = store_with_vehicles(&rig);
    let connection = store.open().unwrap();

    // Open: one row, open, and the cutoff is not recorded yet.
    assert_eq!(hub.open(&rig.grant(0, 600)).unwrap().phase, Phase::Open);
    let (sealed, closed, _) = coverage_row(&connection, 1);
    assert_eq!((sealed.as_str(), closed), ("open", None));
    assert_eq!(
        HubCoverage::for_store(&connection, &vehicle).unwrap_err(),
        SeamError::WindowNotHandedBack
    );

    // Handback: the same row closes at the handback.
    rig.clock.advance(300);
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap().unwrap();
    let (sealed, closed, _) = coverage_row(&connection, 1);
    assert_eq!(sealed, "handback");
    assert_eq!(closed, Some((T0 + 300) * SECOND));
    assert_eq!(coverage_of(&connection, &vehicle), vec![(T0 * SECOND, (T0 + 300) * SECOND)]);

    // A second window adds its own interval; the first is untouched.
    run_window(&rig, &hub, 1600, 200);
    let second_start = (T0 + 300 + 1600) * SECOND;
    assert_eq!(
        coverage_of(&connection, &vehicle),
        vec![
            (T0 * SECOND, (T0 + 300) * SECOND),
            (second_start, second_start + 200 * SECOND)
        ]
    );
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM collection_coverage", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2);

    // A restore of the backup keeps the cutoff exactly.
    let restored = restore_elsewhere(&rig, &store, "same-host");
    assert_eq!(
        coverage_of(&raw(&restored), &vehicle),
        coverage_of(&connection, &vehicle)
    );
}

#[test]
fn import_seam_new_host_sees_the_cutoff() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let (store, vehicle, other) = store_with_vehicles(&rig);
    run_window(&rig, &hub, 0, 400);
    let expected = vec![(T0 * SECOND, (T0 + 400) * SECOND)];

    // A new host holds the restored store and no window directory at all.
    let restored = restore_elsewhere(&rig, &store, "new-host");
    assert!(!restored.join("window").exists());
    let new_host = controller_at(&rig, &restored);
    assert_eq!(new_host.status().phase, Phase::Closed);
    let connection = raw(&restored);
    assert_eq!(coverage_of(&connection, &vehicle), expected);
    assert_eq!(coverage_of(&connection, &other), expected);

    // Import admission on the new host leaves the cutoff as it is.
    let _admission = admit_import(Some(&restored), &home(&rig)).unwrap();
    assert_eq!(coverage_of(&connection, &vehicle), expected);
}

#[test]
fn import_seam_missing_store_handback_is_mirrored() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    // The window runs and hands back before the data directory has a store.
    run_window(&rig, &hub, 0, 500);
    assert!(!data_dir(&rig).join("hub.sqlite").exists());
    drop(hub);

    let (store, vehicle, _) = store_with_vehicles(&rig);
    let connection = store.open().unwrap();
    assert_eq!(coverage_of(&connection, &vehicle), Vec::<(i64, i64)>::new());

    // Import admission copies the ledger's handback into the store.
    drop(admit_import(Some(&data_dir(&rig)), &home(&rig)).unwrap());
    assert_eq!(
        coverage_of(&connection, &vehicle),
        vec![(T0 * SECOND, (T0 + 500) * SECOND)]
    );
    let (sealed, closed, _) = coverage_row(&connection, 1);
    assert_eq!((sealed.as_str(), closed), ("handback", Some((T0 + 500) * SECOND)));

    // A second admission changes nothing.
    drop(admit_import(Some(&data_dir(&rig)), &home(&rig)).unwrap());
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM collection_coverage", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn import_seam_restored_open_window_is_sealed_per_vehicle() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let (store, first, second) = store_with_vehicles(&rig);
    let connection = store.open().unwrap();

    // The backup is taken inside the window: each vehicle has Hub-range rows
    // up to its own last moment.
    assert_eq!(hub.open(&rig.grant(0, 3000)).unwrap().phase, Phase::Open);
    add_hub_position(&connection, &first, 1, (T0 + 100) * SECOND);
    add_hub_position(&connection, &first, 2, (T0 + 300) * SECOND);
    add_hub_position(&connection, &second, 1, (T0 + 700) * SECOND);
    // A source-range row after the Hub's last one does not move the seal.
    connection
        .execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
             VALUES (?1, 5, NULL, 1, ?2)",
            params![first, format!("{{\"date_ms\": {}}}", (T0 + 2000) * SECOND)],
        )
        .unwrap();
    let restored = restore_elsewhere(&rig, &store, "inside-window");

    // The restored store still shows the window open: nothing is recorded.
    let restored_connection = raw(&restored);
    assert_eq!(coverage_row(&restored_connection, 1).0, "open");

    // A host with no window open starts on it and seals the window.
    let new_host = controller_at(&rig, &restored);
    assert_eq!(new_host.status().phase, Phase::Closed);
    let (sealed, closed, _) = coverage_row(&restored_connection, 1);
    assert_eq!(sealed, "restore");
    // The seal is the instant after each vehicle's last Hub row, so the
    // half-open coverage holds that row too (settled in M03-T15).
    assert_eq!(closed, Some((T0 + 700) * SECOND + 1));
    assert_eq!(coverage_of(&restored_connection, &first), vec![(T0 * SECOND, (T0 + 300) * SECOND + 1)]);
    assert_eq!(coverage_of(&restored_connection, &second), vec![(T0 * SECOND, (T0 + 700) * SECOND + 1)]);
    assert!(HubCoverage::for_store(&restored_connection, &first).unwrap().contains((T0 + 300) * SECOND));
    let ends: String = restored_connection
        .query_row("SELECT vehicle_ends_json FROM collection_coverage WHERE seq = 1", [], |row| row.get(0))
        .unwrap();
    let ends: serde_json::Value = serde_json::from_str(&ends).unwrap();
    assert_eq!(ends[&first], (T0 + 300) * SECOND + 1);
    assert_eq!(ends[&second], (T0 + 700) * SECOND + 1);

    // The original host restarts on its own open window and does not seal it.
    drop(hub);
    let (again, outcome) = rig.start_hub();
    assert_eq!(outcome, crate::collection::window::ResumeOutcome::Resumed(Phase::Open));
    assert_eq!(coverage_row(&connection, 1).0, "open");
    drop(again);

    // Import admission also seals a restored store no service has started on.
    let other = restore_elsewhere(&rig, &store, "admission");
    drop(admit_import(Some(&other), &home(&rig)).unwrap());
    assert_eq!(coverage_row(&raw(&other), 1).0, "restore");
}

#[test]
fn import_seam_coverage_subtracts_that_vehicles_gaps() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let (store, first, second) = store_with_vehicles(&rig);
    let connection = store.open().unwrap();

    assert_eq!(hub.open(&rig.grant(0, 600)).unwrap().phase, Phase::Open);
    // A gap of the whole window, from the collector not running.
    hub.record_gap(T0 + 100, T0 + 150).unwrap();
    // A gap of the first vehicle only, from the store's own gap table.
    connection
        .execute(
            "INSERT INTO collection_gaps(vehicle_id, kind, started_at_ms, ended_at_ms, alert, created_at_ms)
             VALUES (?1, 'tesla_unreachable', ?2, ?3, 0, ?2)",
            params![first, (T0 + 200) * SECOND, (T0 + 260) * SECOND],
        )
        .unwrap();
    rig.clock.advance(400);
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap().unwrap();

    let start = T0 * SECOND;
    let end = (T0 + 400) * SECOND;
    let whole = ((T0 + 100) * SECOND, (T0 + 150) * SECOND);
    let own = ((T0 + 200) * SECOND, (T0 + 260) * SECOND);
    assert_eq!(
        coverage_of(&connection, &first),
        vec![(start, whole.0), (whole.1, own.0), (own.1, end)],
        "the first vehicle loses both gaps"
    );
    assert_eq!(
        coverage_of(&connection, &second),
        vec![(start, whole.0), (whole.1, end)],
        "the second vehicle loses only the gap of the whole window"
    );
}

/// Review finding #39: a Hub position collected between the last persisted
/// tick and a stop is inside coverage after the restart, so the next import's
/// seam keeps it (the store reader finds it; the gap starts after it).
#[test]
fn m08_t39_hub_row_after_the_last_tick_survives_the_next_import() {
    use crate::collection::window::ResumeOutcome;
    use crate::import::source_db::seam::{ChildRow, HubRows, SeamLinks, SourceRows, plan_seam};
    use crate::runtime::operator::authority::StoreHighWater;

    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let (store, vehicle, _) = store_with_vehicles(&rig);
    let connection = store.open().unwrap();
    assert_eq!(hub.open(&rig.grant(0, 3600)).unwrap().phase, Phase::Open);
    rig.clock.advance(100);
    assert_eq!(hub.tick().unwrap(), Phase::Open);
    // Collected 3.4 s after the tick, then the service stops.
    let row_ms = (T0 + 103) * SECOND + 400;
    add_hub_position(&connection, &vehicle, 1, row_ms);
    drop(hub);
    rig.clock.advance(600);
    let (hub, outcome) =
        rig.start_hub_with(std::sync::Arc::new(StoreHighWater::new(&data_dir(&rig))));
    assert_eq!(outcome, ResumeOutcome::Resumed(Phase::Open));
    rig.clock.advance(100);
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap().unwrap();

    assert_eq!(
        coverage_of(&connection, &vehicle),
        vec![(T0 * SECOND, (T0 + 104) * SECOND), ((T0 + 700) * SECOND, (T0 + 800) * SECOND)],
        "the stop is a gap, from just after the Hub's newest row"
    );
    let coverage = HubCoverage::for_store(&connection, &vehicle).unwrap();
    let hub_rows = HubRows {
        positions: vec![ChildRow { id: HUB_ID_BASE + 1, at_ms: row_ms, parent: None }],
        ..HubRows::default()
    };
    let plan = plan_seam(&coverage, &hub_rows, &SourceRows::default(), &SeamLinks::default());
    assert!(plan.positions.hub_removed.is_empty(), "the Hub row is not tombstoned");
    assert_eq!(plan.positions.hub_kept_count, 1);
}
