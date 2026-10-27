// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use std::collections::BTreeMap;

const FLOOR_STORE: &[u8] = include_bytes!("../../../../fixtures/g2-4/m1/store-f.sqlite.zst");
const PRE_FLOOR_STORE: &[u8] =
    include_bytes!("../../../../fixtures/g2-4/m1/store-f-minus-one.sqlite.zst");
const VEHICLES: [&str; 3] = [
    "20000000-0000-4000-8000-000000000001",
    "20000000-0000-4000-8000-000000000002",
    "20000000-0000-4000-8000-000000000003",
];
type Schema = Vec<(String, String, String, Option<String>)>;
type Rows = BTreeMap<String, Vec<Vec<String>>>;

pub(super) fn copied_store(bytes: &[u8]) -> tempfile::TempDir {
    let directory = crate::private_tempdir().unwrap();
    let path = directory.path().join("hub.sqlite");
    fs::write(&path, zstd::stream::decode_all(bytes).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    directory
}

fn connection(directory: &tempfile::TempDir) -> Connection {
    Connection::open(directory.path().join("hub.sqlite")).unwrap()
}

fn schema(connection: &Connection) -> Schema {
    connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY rowid")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn rows(connection: &Connection) -> Rows {
    let tables = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{table}\""))
                .unwrap();
            let columns = statement.column_count();
            let mut values = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| {
                            row.get::<_, rusqlite::types::Value>(column)
                                .map(|value| format!("{value:?}"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            values.sort();
            (table, values)
        })
        .collect()
}

pub(super) fn schema_snapshot(connection: &Connection) -> (i32, Schema, Rows) {
    (
        schema_version(connection).unwrap(),
        schema(connection),
        rows(connection),
    )
}

fn previous_position_type() -> String {
    let directory = copied_store(PRE_FLOOR_STORE);
    let connection = connection(&directory);
    let sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='current_observations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    sql.split('\'')
        .find(|part| part.ends_with("_position_v1"))
        .unwrap()
        .to_owned()
}

// G2-4 M1-T1
#[test]
fn m1_t1_create_schema_equals_stepwise_fixture() {
    let test = "m1_t1_create_schema_equals_stepwise_fixture";
    let directory = crate::private_tempdir().unwrap();
    let connection = connection(&directory);
    migrate(&connection).labelled_ok(test, "baseline creation succeeds");
    let expected: Schema =
        serde_json::from_str(include_str!("../../../../fixtures/g2-4/m1/schema-f.json")).unwrap();
    let fixture_directory = copied_store(FLOOR_STORE);
    let fixture = self::connection(&fixture_directory);
    assert!(
        schema(&connection) == expected && schema(&fixture) == expected,
        "assertion failed: {test}: raw schema in creation order equals the shipped stepwise fixture"
    );
    let application_id: i32 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .unwrap();
    assert!(
        application_id == APPLICATION_ID,
        "assertion failed: {test}: creation writes the Hub application id"
    );
    assert!(
        schema_version(&connection).unwrap() == MIN_STORE_VERSION,
        "assertion failed: {test}: creation writes the floor version"
    );
}

// G2-4 M1-T2
#[test]
fn m1_t2_below_floor_refused_without_writes() {
    let test = "m1_t2_below_floor_refused_without_writes";
    for open in 0..3 {
        let directory = copied_store(PRE_FLOOR_STORE);
        let retained_tree = || {
            tree_contents(directory.path())
                .into_iter()
                .filter(|(path, _, _)| {
                    !matches!(
                        path.file_name().and_then(|name| name.to_str()),
                        Some("hub.sqlite-wal" | "hub.sqlite-shm")
                    )
                })
                .collect::<Vec<_>>()
        };
        let before = retained_tree();
        let catalogue_before = fs::read(directory.path().join("hub.sqlite")).unwrap();
        let error = match open {
            0 => HubStore::initialize(directory.path()).map(|_| ()),
            1 => HubStore::preflight_standalone_schema_read_only(directory.path()),
            _ => HubStore::preflight_import_source_key(directory.path(), "default", false),
        }
        .labelled_err(test, "a below-floor store is refused");
        let message = error.to_string();
        assert!(
            message.contains(&(MIN_STORE_VERSION - 1).to_string())
                && message.contains(&MIN_STORE_VERSION.to_string())
                && message.contains("re-import from the source"),
            "assertion failed: {test}: refusal names both versions and the re-import path; {message}"
        );
        assert!(
            fs::read(directory.path().join("hub.sqlite")).unwrap() == catalogue_before,
            "assertion failed: {test}: catalogue bytes are unchanged"
        );
        assert!(
            retained_tree() == before,
            "assertion failed: {test}: no retained file or store directory is changed"
        );
    }
}

// G2-4 M1-T3
#[test]
fn m1_t3_floor_fixture_rows_survive_to_head() {
    let test = "m1_t3_floor_fixture_rows_survive_to_head";
    let directory = copied_store(FLOOR_STORE);
    let connection = connection(&directory);
    let before = rows(&connection);
    let expected: BTreeMap<String, usize> = serde_json::from_str(include_str!(
        "../../../../fixtures/g2-4/m1/table-counts.json"
    ))
    .unwrap();
    assert!(
        expected.len() == 82
            && expected
                .iter()
                .all(|(table, count)| *count > 0 && before[table].len() == *count),
        "assertion failed: {test}: every application table is populated"
    );
    migrate(&connection).labelled_ok(test, "the floor fixture reaches HEAD");
    assert!(
        schema_version(&connection).unwrap() == SCHEMA_VERSION,
        "assertion failed: {test}: the fixture reaches HEAD"
    );
    assert!(
        rows(&connection) == before,
        "assertion failed: {test}: all row counts and values are preserved"
    );
    let violations: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        violations == 0,
        "assertion failed: {test}: foreign keys remain valid"
    );
}

fn classifications(connection: &mut Connection) -> Vec<(i64, bool, bool, ImportBoundary, bool)> {
    VEHICLES
        .iter()
        .map(|vehicle| {
            let id = vehicle.parse().unwrap();
            let bound = charge_import_bound(connection, id).unwrap();
            let terrain = terrain_watermark_boundary(connection, id).unwrap();
            let transaction = connection.transaction().unwrap();
            let evidence = vehicle_has_import_evidence(&transaction, vehicle, None).unwrap();
            (
                bound.max_imported_charge_id,
                bound.is_imported(7),
                bound.is_imported(8),
                terrain,
                evidence,
            )
        })
        .collect()
}

// G2-4 M1-T4
#[test]
fn m1_t4_import_readers_equal_stepwise_store() {
    let test = "m1_t4_import_readers_equal_stepwise_store";
    let baseline_directory = crate::private_tempdir().unwrap();
    let mut baseline = connection(&baseline_directory);
    migrate(&baseline).unwrap();
    baseline
        .execute_batch(include_str!("../../../../fixtures/g2-4/m1/rows.sql"))
        .unwrap();
    let stepwise_directory = copied_store(FLOOR_STORE);
    let mut stepwise = connection(&stepwise_directory);
    assert!(
        rows(&baseline) == rows(&stepwise),
        "assertion failed: {test}: identical fixture rows enter both schemas"
    );
    let expected = vec![
        (0, false, false, ImportBoundary::default(), false),
        (
            7,
            true,
            false,
            ImportBoundary {
                max_position_id: 19,
                max_date_ms: 1_760_000_000_000,
            },
            true,
        ),
        (
            7,
            true,
            false,
            ImportBoundary {
                max_position_id: 19,
                max_date_ms: 1_760_000_000_000,
            },
            true,
        ),
    ];
    let actual = classifications(&mut baseline);
    assert!(
        actual == expected,
        "assertion failed: {test}: live imported and migrated readers have their expected boundaries; {actual:?}"
    );
    assert!(
        actual == classifications(&mut stepwise),
        "assertion failed: {test}: C2 C9 and C6 readers agree across the baseline"
    );
    for connection in [&baseline, &stepwise] {
        let blob: Vec<u8> = connection
            .query_row(
                "SELECT open_session_json FROM vehicle_lifecycle_state WHERE vehicle_id=?1",
                [VEHICLES[2]],
                |row| row.get(0),
            )
            .unwrap();
        let state = crate::lifecycle::OpenSessionState::decode(&blob).unwrap();
        assert!(
            state
                .open_charge
                .as_ref()
                .is_some_and(|charge| charge.id == 7)
                && state.last_observation_id == 1,
            "assertion failed: {test}: an imported open charge was continued live"
        );
    }
}

// G2-4 M1-T5
#[test]
fn m1_t5_neutral_schema_checks_and_default_source() {
    let test = "m1_t5_neutral_schema_checks_and_default_source";
    let directory = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(directory.path()).unwrap();
    let old_type = previous_position_type();
    let old_kind = old_type.strip_suffix("_position_v1").unwrap();
    let connection = store.open().unwrap();
    assert!(
        schema(&connection).iter().all(|(kind, name, table, sql)| [
            kind.as_str(),
            name,
            table,
            sql.as_deref().unwrap_or("")
        ]
        .iter()
        .all(|part| !part.to_lowercase().contains(old_kind))),
        "assertion failed: {test}: every schema object and SQL text uses neutral names"
    );
    let (first, created) = store
        .register_source_db_import_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, DEFAULT_IMPORT_SOURCE_KEY),
            1000,
            false,
        )
        .unwrap();
    let (second, again) = store
        .register_source_db_import_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, "default"),
            2000,
            false,
        )
        .unwrap();
    assert!(
        DEFAULT_IMPORT_SOURCE_KEY == "default"
            && created
            && !again
            && first.source_id == second.source_id,
        "assertion failed: {test}: a default re-import reuses its source identity"
    );
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(first.source_id, "check-car"), 1000)
        .unwrap();
    let insert = |record_type: &str, id| {
        connection.execute(
            "INSERT INTO current_observations VALUES (?1,?2,?3,?4,0,0,?5,'{}')",
            params![
                vehicle.vehicle_id.to_string(),
                record_type,
                id,
                first.source_id.to_string(),
                vec![0u8; 32]
            ],
        )
    };
    insert("source_db_position_v1", 1)
        .labelled_ok(test, "the neutral current type passes its CHECK");
    assert!(
        insert(&old_type, 2).is_err(),
        "assertion failed: {test}: the preceding current type is refused"
    );
}

// G2-4 M1-T7
#[test]
fn m1_t7_name_gate_data_retires_collapsed_entries() {
    let test = "m1_t7_name_gate_data_retires_collapsed_entries";
    let allow: Value =
        serde_json::from_str(include_str!("../../../../scripts/name-policy/allow.json")).unwrap();
    let milestones: Value = serde_json::from_str(include_str!(
        "../../../../scripts/name-policy/milestones.json"
    ))
    .unwrap();
    let entries = allow["entries"].as_array().unwrap();
    assert!(
        entries.iter().all(|entry| {
            let id = entry["id"].as_str().unwrap();
            !(entry["expires"] == "G2-4-M1"
                && (id.starts_with("retained-") || id.starts_with("preceding-import-name-")))
        }),
        "assertion failed: {test}: collapsed-history entries are removed"
    );
    assert!(
        milestones["closed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "G2-4-M1"),
        "assertion failed: {test}: the collapse milestone is closed"
    );
}

// G2-4 M1-T8
#[test]
fn m1_t8_older_schema_ceiling_refuses_baseline_store() {
    let test = "m1_t8_older_schema_ceiling_refuses_baseline_store";
    let directory = crate::private_tempdir().unwrap();
    let connection = connection(&directory);
    migrate(&connection).unwrap();
    let version = schema_version(&connection).unwrap();
    let before = schema_snapshot(&connection);
    let error = admit_store_version(version, MIN_STORE_VERSION - 1)
        .labelled_err(test, "an older schema ceiling refuses the baseline store");
    assert!(
        matches!(error,StoreError::UnsupportedSchema(found) if found==MIN_STORE_VERSION),
        "assertion failed: {test}: the newer-store check reports the baseline version"
    );
    assert!(
        schema_snapshot(&connection) == before,
        "assertion failed: {test}: the older-reader refusal changes no rows"
    );
}

// G2-4 M1-T9
#[test]
#[ignore = "Legacy direct imports remain supported until their bridge is retired"]
fn m1_t9_u02_retirement_deferred_per_m0() {}

// G2-4: stored geofence bounds agree with the location constants.
#[test]
fn m1_geofence_checks_match_location_constants() {
    use crate::geo::location::{
        FIRST_HUB_GEOFENCE_ID, MAX_GEOFENCE_NAME_BYTES, MAX_IMPORTED_GEOFENCE_ID,
        MAX_IMPORTED_GEOFENCE_RADIUS_METRES,
    };
    let test = "m1_geofence_checks_match_location_constants";
    let directory = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(directory.path()).unwrap();
    let connection = store.open().unwrap();
    let table_sql = |table: &str| -> String {
        connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap()
    };
    let geofences = table_sql("geofences");
    assert!(
        geofences.contains(&format!(
            "radius_m <= {MAX_IMPORTED_GEOFENCE_RADIUS_METRES:?}"
        )),
        "assertion failed: {test}: the radius CHECK matches the imported radius constant"
    );
    assert!(
        geofences.contains(&format!("BETWEEN 1 AND {MAX_GEOFENCE_NAME_BYTES}")),
        "assertion failed: {test}: the name CHECK matches the name byte constant"
    );
    assert!(
        table_sql("geofence_id_marks").contains(&format!("last_hub_id >= {FIRST_HUB_GEOFENCE_ID}")),
        "assertion failed: {test}: the Hub id CHECK matches the first Hub id constant"
    );
    assert!(
        table_sql("deleted_imported_geofences")
            .contains(&format!("BETWEEN 1 AND {MAX_IMPORTED_GEOFENCE_ID}")),
        "assertion failed: {test}: the imported id CHECK matches the last imported id constant"
    );
}
