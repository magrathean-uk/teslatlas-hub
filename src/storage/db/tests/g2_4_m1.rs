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
    let floor_creation = Connection::open_in_memory().unwrap();
    // The frozen predecessor includes the six credential tables that the
    // current creation DDL intentionally omits. Rebuild its complete schema.
    for (_, name, _, sql) in &expected {
        if name != "sqlite_sequence" {
            if let Some(sql) = sql {
                floor_creation.execute_batch(sql).unwrap();
            }
        }
    }
    assert!(
        schema(&floor_creation) == expected && schema(&fixture) == expected,
        "assertion failed: {test}: raw schema in creation order equals the shipped stepwise fixture"
    );
    assert!(
        schema_version(&fixture).unwrap() == MIN_STORE_VERSION,
        "assertion failed: {test}: the shipped stepwise fixture retains the floor version"
    );
    let application_id: i32 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .unwrap();
    assert!(
        application_id == APPLICATION_ID,
        "assertion failed: {test}: creation writes the Hub application id"
    );
    assert!(
        schema_version(&connection).unwrap() == SCHEMA_VERSION,
        "assertion failed: {test}: C10 creation writes the current head version"
    );
    // M1-T12 compares complete heads from an empty predecessor. A populated
    // predecessor intentionally retains its old credential tables for Phase B.
    floor_creation
        .pragma_update(None, "application_id", APPLICATION_ID)
        .unwrap();
    floor_creation
        .pragma_update(None, "user_version", MIN_STORE_VERSION)
        .unwrap();
    migrate(&floor_creation).labelled_ok(test, "the empty floor reaches C10 head");
    assert!(
        schema_version(&floor_creation).unwrap() == SCHEMA_VERSION
            && schema(&floor_creation) == schema(&connection),
        "assertion failed: {test}: fresh and upgraded empty-floor C10 head schemas agree in creation order"
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
    assert!(
        schema_version(&connection).unwrap() == MIN_STORE_VERSION,
        "assertion failed: {test}: the populated predecessor starts at the floor"
    );
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
    let after = rows(&connection);
    assert!(
        before
            .iter()
            .all(|(table, values)| after.get(table) == Some(values)),
        "assertion failed: {test}: all row counts and values are preserved"
    );
    let added_tables: Vec<&str> = after
        .keys()
        .filter(|table| !before.contains_key(*table))
        .map(String::as_str)
        .collect();
    assert!(
        added_tables
            == vec![
                "backup_pack_pins",
                "backup_runs",
                "credential_format_state",
                "credential_key_usage",
                "credential_legacy_fences",
                "credential_pairs",
                "credential_refresh_attempts",
                "credential_refresh_fences",
                "credential_sign_ins",
            ]
            && added_tables.iter().all(|table| {
                after[*table].len() == usize::from(*table == "credential_format_state")
            }),
        "assertion failed: {test}: only the seven C10 tables and the two empty backup live-state tables (M06-T07, M06-T02) are added, with one state row and no converted credentials"
    );
    let state: (
        i64,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT format_version,owner_api_state,fleet_api_state,fences_pending,old_tables_dropped,purge_required,wal_scrubbed,old_files_removed,converted_at_ms,completed_at_ms,durability FROM credential_format_state WHERE singleton_id=1",
            [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?)),
        )
        .unwrap();
    assert_eq!(
        state,
        (
            1,
            "pending".to_owned(),
            "pending".to_owned(),
            1,
            0,
            1,
            0,
            0,
            None,
            None,
            None,
        ),
        "assertion failed: {test}: Phase A leaves populated predecessor credentials pending without Phase B or durability claims"
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
    let floor_schema: Schema =
        serde_json::from_str(include_str!("../../../../fixtures/g2-4/m1/schema-f.json")).unwrap();
    for (_, name, _, sql) in &floor_schema {
        if name != "sqlite_sequence" {
            if let Some(sql) = sql {
                baseline.execute_batch(sql).unwrap();
            }
        }
    }
    assert!(
        schema(&baseline) == floor_schema,
        "assertion failed: {test}: the reconstructed predecessor matches the frozen full floor schema"
    );
    baseline
        .pragma_update(None, "application_id", APPLICATION_ID)
        .unwrap();
    baseline
        .pragma_update(None, "user_version", MIN_STORE_VERSION)
        .unwrap();
    baseline
        .execute_batch(include_str!("../../../../fixtures/g2-4/m1/rows.sql"))
        .unwrap();
    let stepwise_directory = copied_store(FLOOR_STORE);
    let mut stepwise = connection(&stepwise_directory);
    assert!(
        schema_version(&baseline).unwrap() == MIN_STORE_VERSION
            && schema_version(&stepwise).unwrap() == MIN_STORE_VERSION,
        "assertion failed: {test}: identical readers first exercise the retained floor schemas"
    );
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
    let retained = rows(&baseline);
    migrate(&baseline).labelled_ok(test, "the seeded baseline floor reaches C10 head");
    migrate(&stepwise).labelled_ok(test, "the shipped stepwise floor reaches C10 head");
    assert!(
        schema_version(&baseline).unwrap() == SCHEMA_VERSION
            && schema_version(&stepwise).unwrap() == SCHEMA_VERSION
            && schema(&baseline) == schema(&stepwise),
        "assertion failed: {test}: both populated C10 head schemas agree"
    );
    for upgraded in [&baseline, &stepwise] {
        let upgraded_rows = rows(upgraded);
        assert!(
            retained
                .iter()
                .all(|(table, values)| upgraded_rows.get(table) == Some(values)),
            "assertion failed: {test}: every seeded floor row and value survives the actual C10 rung"
        );
    }
    assert!(
        classifications(&mut baseline) == expected && classifications(&mut stepwise) == expected,
        "assertion failed: {test}: C2 C9 and C6 readers preserve their exact floor boundaries at C10 head"
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
    let directory = copied_store(FLOOR_STORE);
    let connection = connection(&directory);
    let version = schema_version(&connection).unwrap();
    assert!(
        version == MIN_STORE_VERSION,
        "assertion failed: {test}: the retained baseline fixture starts at the floor"
    );
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
    let head_directory = crate::private_tempdir().unwrap();
    let head = self::connection(&head_directory);
    migrate(&head).unwrap();
    let head_version = schema_version(&head).unwrap();
    assert!(
        head_version == SCHEMA_VERSION,
        "assertion failed: {test}: actual C10 creation reaches the current head"
    );
    let head_before = schema_snapshot(&head);
    let error = admit_store_version(head_version, MIN_STORE_VERSION)
        .labelled_err(test, "the former floor ceiling refuses the C10 head");
    assert!(
        matches!(error,StoreError::UnsupportedSchema(found) if found==SCHEMA_VERSION),
        "assertion failed: {test}: the older floor reader reports the C10 head version"
    );
    assert!(
        schema_snapshot(&head) == head_before,
        "assertion failed: {test}: refusing the C10 head changes no rows or schema"
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
