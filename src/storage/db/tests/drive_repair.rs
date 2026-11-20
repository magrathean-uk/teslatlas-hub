// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T26: drive repair records, migration and store-side eligibility.

use super::*;
use crate::import::broken_drive_repair::{
    BrokenDriveCandidateIndex, GapEvidence, RepairInput, RepairSample, StateCoverage,
};

const T0: i64 = 1_700_000_000_000;
const NOW: i64 = T0 + 100 * 3_600_000;
const SOURCE: Uuid = Uuid::from_u128(0x5eed);
const VEHICLE: Uuid = Uuid::from_u128(0xca5);

/// A raw connection for seeding imported rows without their full parent
/// chain (sources, heads and bases), which these checks do not read.
fn raw(store: &HubStore) -> Connection {
    let connection = Connection::open(&store.database_path).expect("raw catalogue connection");
    connection.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    connection
}

fn sample(id: i64, at: i64, odometer: f64) -> RepairSample {
    RepairSample {
        position_id: id,
        date_ms: at,
        odometer_km: Some(odometer),
        speed_kmh: Some(30),
    }
}

/// `fragments` structural fragments one minute apart from `start`, then a
/// completed destination; ids begin at `first_id`.
fn run(first_id: i64, start: i64, fragments: i64) -> Vec<RepairInput> {
    let mut rows = (0..fragments)
        .map(|i| {
            let s = sample((first_id + i) * 10, start + i * 60_000, 100.0 + i as f64 * 0.5);
            RepairInput {
                drive_id: first_id + i,
                car_id: 1,
                start_ms: start + i * 60_000,
                end_ms: None,
                distance_km: None,
                duration_min: None,
                sample_count: 1,
                first_sample: Some(s),
                last_sample: Some(s),
                previous_drive_id: Some(first_id + i - 1),
                previous_unclosed: i > 0,
                next_drive_id: Some(first_id + i + 1),
                gap_to_next: Some(GapEvidence {
                    charge_between: false,
                    state: StateCoverage::Online,
                }),
            }
        })
        .collect::<Vec<_>>();
    let id = first_id + fragments;
    let at = start + fragments * 60_000;
    let first = sample(id * 10, at, 100.0 + fragments as f64 * 0.5);
    rows.push(RepairInput {
        drive_id: id,
        car_id: 1,
        start_ms: at,
        end_ms: Some(at + 1_800_000),
        distance_km: Some(9.0),
        duration_min: Some(30),
        sample_count: 2,
        first_sample: Some(first),
        last_sample: Some(sample(id * 10 + 1, at + 1_800_000, first.odometer_km.unwrap() + 9.0)),
        previous_drive_id: Some(id - 1),
        previous_unclosed: true,
        next_drive_id: None,
        gap_to_next: None,
    });
    rows
}

/// Materialise the imported rows the index names: destination drives with
/// their samples, detached fragment samples, and their catalogue entries.
fn seed_imported(store: &HubStore, rows: &[RepairInput]) {
    let connection = raw(store);
    let vehicle = VEHICLE.to_string();
    for row in rows {
        let sample = row.first_sample.unwrap();
        if row.end_ms.is_some() {
            connection
                .execute(
                    "INSERT OR IGNORE INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
                     VALUES (?1, ?2, 1, '{}')",
                    params![vehicle, row.drive_id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT OR IGNORE INTO import_projection_rows(vehicle_id, entity, entity_id)
                     VALUES (?1, 'drive', ?2)",
                    params![vehicle, row.drive_id],
                )
                .unwrap();
        }
        let parent = row.end_ms.map(|_| row.drive_id);
        for position in [Some(sample), row.end_ms.and(row.last_sample)].into_iter().flatten() {
            connection
                .execute(
                    "INSERT OR IGNORE INTO materialised_positions(vehicle_id, position_id, drive_id,
                         car_id, position_json) VALUES (?1, ?2, ?3, 1, '{}')",
                    params![vehicle, position.position_id, parent],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT OR IGNORE INTO import_projection_rows(vehicle_id, entity, entity_id)
                     VALUES (?1, 'position', ?2)",
                    params![vehicle, position.position_id],
                )
                .unwrap();
        }
    }
}

fn served_rows(store: &HubStore) -> Vec<(String, i64, Option<i64>, String)> {
    let connection = raw(store);
    let mut statement = connection
        .prepare(
            "SELECT 'drive', drive_id, NULL, drive_json FROM materialised_drives
             UNION ALL SELECT 'position', position_id, drive_id, position_json FROM materialised_positions
             ORDER BY 1, 2",
        )
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn index(rows: Vec<RepairInput>) -> BrokenDriveCandidateIndex {
    BrokenDriveCandidateIndex { car_id: 1, rows }
}

fn ids(rows: &[RepairInput]) -> Vec<i64> {
    rows.iter().map(|row| row.drive_id).collect()
}

#[test]
fn broken_drive_repair_plan_store_migrates_to_v83_and_survives_reopen() {
    let (directory, store) = try_test_store().unwrap();
    let connection = raw(&store);
    assert_eq!(SCHEMA_VERSION, DRIVE_REPAIR_SCHEMA_VERSION);
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    let strict = |connection: &Connection| -> i64 {
        DRIVE_REPAIR_TABLES
            .iter()
            .map(|table| {
                connection
                    .query_row(
                        "SELECT COALESCE((SELECT strict FROM pragma_table_list WHERE name = ?1), 0)",
                        [table],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap()
            })
            .sum()
    };
    assert_eq!(strict(&connection), 7, "all repair tables exist and are STRICT");
    let rows = run(10, T0, 3);
    store
        .record_drive_repair_index(SOURCE, VEHICLE, &index(rows.clone()), NOW)
        .unwrap();
    drop(store);

    let store = HubStore::initialize(directory.path()).unwrap();
    assert_eq!(store.drive_repair_summary(VEHICLE).unwrap().index_rows, 4);
    assert_eq!(ids(&store.drive_repair_index_page(VEHICLE, None, 100).unwrap()), ids(&rows));

    // A v80 store, and one an earlier rung already stamped v81 or v82
    // without any repair table, gains the empty tables on its next writable
    // open and reaches v83.
    let mut store = store;
    for version in [80, 81, 82] {
        let connection = raw(&store);
        for table in DRIVE_REPAIR_TABLES {
            connection.execute_batch(&format!("DROP TABLE {table};")).unwrap();
        }
        connection.pragma_update(None, "user_version", version).unwrap();
        drop(connection);
        drop(store);
        store = HubStore::initialize(directory.path()).unwrap();
        let connection = raw(&store);
        assert_eq!(schema_version(&connection).unwrap(), 83, "from v{version}");
        assert_eq!(strict(&connection), 7, "from v{version}");
        assert_eq!(store.drive_repair_summary(VEHICLE).unwrap().index_rows, 0);
        store
            .record_drive_repair_index(SOURCE, VEHICLE, &index(rows.clone()), NOW)
            .unwrap();
        assert!(store.plan_drive_repairs(VEHICLE, NOW).unwrap().index_present, "from v{version}");
    }
}

#[test]
fn broken_drive_repair_plan_store_read_only_opens_a_v80_store() {
    // An updated Hub holds a v80 store until its first writable open; the
    // read-only lanes open it and change nothing.
    let (directory, store) = try_test_store().unwrap();
    let connection = raw(&store);
    for table in DRIVE_REPAIR_TABLES {
        connection.execute_batch(&format!("DROP TABLE {table};")).unwrap();
    }
    connection.pragma_update(None, "user_version", 80).unwrap();
    drop(store);
    drop(HubStore::open_read_only(directory.path()).expect("read-only lane opens v80"));
    assert_eq!(schema_version(&connection).unwrap(), 80, "version unchanged");
    drop(HubStore::initialize(directory.path()).unwrap());
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
}

#[test]
fn broken_drive_repair_plan_store_needs_attestation_and_changes_no_served_rows() {
    let (_directory, store) = try_test_store().unwrap();
    let rows = run(10, T0, 3);
    seed_imported(&store, &rows);
    store
        .record_drive_repair_index(SOURCE, VEHICLE, &index(rows.clone()), NOW)
        .unwrap();
    let before = served_rows(&store);

    let unknown = store.plan_drive_repairs(VEHICLE, NOW).unwrap();
    assert!(unknown.planned_keys.is_empty(), "unknown edit history is protected");
    assert_eq!(unknown.counts.refused.get("unknown_edit_history"), Some(&3));

    assert_eq!(
        store
            .attest_unedited_drive_repair_candidates(VEHICLE, &ids(&rows), NOW)
            .unwrap(),
        4
    );
    let report = store.plan_drive_repairs(VEHICLE, NOW).unwrap();
    assert_eq!(report.planned_keys.len(), 1);
    assert_eq!(report.counts.planned_fragments, 3);
    assert_eq!(report.pages, 1);
    let again = store.plan_drive_repairs(VEHICLE, NOW).unwrap();
    assert_eq!(again.planned_keys, report.planned_keys, "a replay plans the same identity");
    let summary = store.drive_repair_summary(VEHICLE).unwrap();
    assert_eq!((summary.planned, summary.attested), (1, 4));
    assert_eq!(served_rows(&store), before, "planning changes no served row");

    // Undo suppresses the identity until an explicit retry.
    let key = report.planned_keys[0];
    assert!(store.undo_drive_repair_component(VEHICLE, &key, NOW).unwrap());
    let undone = store.plan_drive_repairs(VEHICLE, NOW).unwrap();
    assert!(undone.planned_keys.is_empty());
    assert_eq!(undone.counts.refused.get("suppressed"), Some(&3));
    assert!(store.retry_drive_repair_component(VEHICLE, &key).unwrap());
    assert_eq!(store.plan_drive_repairs(VEHICLE, NOW).unwrap().planned_keys, vec![key]);
}

#[test]
fn broken_drive_repair_plan_store_refuses_foreign_or_protected_members() {
    let rows = run(10, T0, 3);
    let vehicle = VEHICLE.to_string();
    let cases: [(&str, &str); 6] = [
        ("INSERT INTO import_seam_links(vehicle_id, entity, loser_id, winner_id, decided_seq)
          VALUES (?1, 'drive', 13, 13, 0)", "not_import_owned"),
        ("INSERT INTO import_seam_links(vehicle_id, entity, loser_id, winner_id, decided_seq)
          VALUES (?1, 'drive', 11, 1073741900, 0)", "not_import_owned"),
        ("DELETE FROM import_projection_rows WHERE vehicle_id = ?1 AND entity = 'position' AND entity_id = 120",
         "not_import_owned"),
        ("INSERT INTO lifecycle_open_rows(source_id, source_table, source_row_id, vehicle_id, car_id,
              domain, parent_source_row_id, row_json)
          VALUES ('00000000-0000-0000-0000-00000000000a', 'drives', 12, ?1, 1, 'drive', NULL, '{}')",
         "not_import_owned"),
        ("UPDATE materialised_positions SET position_id = 1073741999
          WHERE vehicle_id = ?1 AND position_id = 131", "not_import_owned"),
        ("INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json,
              vehicle_ends_json, sealed)
          VALUES (1, 'grant', 1700000100000, 1700000200000, '[]', NULL, 'handback')
          -- ?1", "collection_coverage"),
    ];
    for (sql, expected) in cases {
        let (_directory, store) = try_test_store().unwrap();
        seed_imported(&store, &rows);
        store
            .record_drive_repair_index(SOURCE, VEHICLE, &index(rows.clone()), NOW)
            .unwrap();
        store
            .attest_unedited_drive_repair_candidates(VEHICLE, &ids(&rows), NOW)
            .unwrap();
        let connection = raw(&store);
        if sql.contains("?1") && !sql.contains("-- ?1") {
            connection.execute(sql, params![vehicle]).unwrap();
        } else {
            connection.execute_batch(sql).unwrap();
        }
        let report = store.plan_drive_repairs(VEHICLE, NOW).unwrap();
        assert!(report.planned_keys.is_empty(), "{expected}: {sql}");
        assert_eq!(report.counts.refused.get(expected), Some(&3), "{sql}: {:?}", report.counts);
    }

    // A human edit protects the component and supersedes its plan; an
    // attestation never overwrites it.
    let (_directory, store) = try_test_store().unwrap();
    seed_imported(&store, &rows);
    store
        .record_drive_repair_index(SOURCE, VEHICLE, &index(rows.clone()), NOW)
        .unwrap();
    store
        .attest_unedited_drive_repair_candidates(VEHICLE, &ids(&rows), NOW)
        .unwrap();
    assert_eq!(store.plan_drive_repairs(VEHICLE, NOW).unwrap().planned_keys.len(), 1);
    store
        .record_drive_edit_protection(VEHICLE, DriveRepairEntity::Drive, 13, DriveProtectionReason::Edit, NOW)
        .unwrap();
    assert_eq!(store.attest_unedited_drive_repair_candidates(VEHICLE, &[13], NOW).unwrap(), 0);
    let summary = store.drive_repair_summary(VEHICLE).unwrap();
    assert_eq!((summary.planned, summary.superseded, summary.protected), (0, 1, 1));
    let report = store.plan_drive_repairs(VEHICLE, NOW).unwrap();
    assert_eq!(report.counts.refused.get("protected"), Some(&3));
}

#[test]
fn broken_drive_repair_plan_store_reindex_invalidates_attestations() {
    let (_directory, store) = try_test_store().unwrap();
    let rows = run(10, T0, 3);
    seed_imported(&store, &rows);
    store
        .record_drive_repair_index(SOURCE, VEHICLE, &index(rows.clone()), NOW)
        .unwrap();
    store
        .attest_unedited_drive_repair_candidates(VEHICLE, &ids(&rows), NOW)
        .unwrap();
    assert_eq!(store.plan_drive_repairs(VEHICLE, NOW).unwrap().planned_keys.len(), 1);

    // An unchanged capture keeps everything.
    let unchanged = store
        .record_drive_repair_index(SOURCE, VEHICLE, &index(rows.clone()), NOW)
        .unwrap();
    assert_eq!((unchanged.unchanged, unchanged.changed, unchanged.removed), (4, 0, 0));
    assert_eq!(store.drive_repair_summary(VEHICLE).unwrap().planned, 1);

    // The source closed fragment 11 and deleted fragment 12.
    let mut changed = rows.clone();
    changed[0].end_ms = Some(T0 + 30_000);
    changed[0].distance_km = Some(0.2);
    changed.remove(1);
    let report = store
        .record_drive_repair_index(SOURCE, VEHICLE, &index(changed.clone()), NOW)
        .unwrap();
    assert_eq!((report.changed, report.removed, report.unchanged), (1, 1, 2));
    assert_eq!(report.superseded_components, 1);
    let summary = store.drive_repair_summary(VEHICLE).unwrap();
    assert_eq!((summary.planned, summary.superseded, summary.attested, summary.index_rows), (0, 1, 2, 3));
    assert!(
        store.plan_drive_repairs(VEHICLE, NOW).unwrap().planned_keys.is_empty(),
        "the remaining run is re-evaluated, never duplicated"
    );
}

#[test]
fn broken_drive_repair_plan_store_pages_runs_across_boundaries() {
    let (_directory, store) = try_test_store().unwrap();
    // 40 runs of five fragments and a destination: 240 rows over three
    // pages, with runs crossing every page boundary.
    let mut rows = Vec::new();
    for group in 0..40_i64 {
        let mut group_rows = run(1_000 + group * 10, T0 + group * 10_800_000, 5);
        group_rows[0].previous_unclosed = false;
        rows.extend(group_rows);
    }
    seed_imported(&store, &rows);
    store
        .record_drive_repair_index(SOURCE, VEHICLE, &index(rows.clone()), NOW + 200 * 3_600_000)
        .unwrap();
    store
        .attest_unedited_drive_repair_candidates(VEHICLE, &ids(&rows), NOW)
        .unwrap();
    let page = store.drive_repair_index_page(VEHICLE, None, 1_000).unwrap();
    assert_eq!(page.len(), 100, "pages never exceed the bound");
    let report = store.plan_drive_repairs(VEHICLE, NOW + 200 * 3_600_000).unwrap();
    assert_eq!(report.pages, 3);
    assert_eq!(report.planned_keys.len(), 40);
    assert_eq!(report.counts.candidates, 200);
    assert_eq!(report.counts.planned_fragments, 200);
}
