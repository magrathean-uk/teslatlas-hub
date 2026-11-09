// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use std::collections::BTreeMap;

type Rows = BTreeMap<String, Vec<Vec<String>>>;
type Schema = Vec<(String, String, String, Option<String>)>;

fn rows(connection: &Connection) -> Rows {
    let tables = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
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

fn schema(connection: &Connection) -> Schema {
    connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn state(connection: &Connection) -> Vec<String> {
    rows(connection).remove("credential_format_state").unwrap()[0].clone()
}

#[test]
fn floor_store_preserves_populated_tables() {
    let directory = crate::private_tempdir().unwrap();
    let path = directory.path().join("hub.sqlite");
    let fixture = include_bytes!("../../../../fixtures/g2-4/m1/store-f.sqlite.zst");
    fs::write(&path, zstd::stream::decode_all(fixture.as_slice()).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    assert_eq!(schema_version(&connection).unwrap(), MIN_STORE_VERSION);
    let before = rows(&connection);
    let expected: BTreeMap<String, usize> = serde_json::from_str(include_str!(
        "../../../../fixtures/g2-4/m1/table-counts.json"
    ))
    .unwrap();
    assert_eq!(
        expected.len(),
        82,
        "M1-T3: all retained application tables have witnesses"
    );
    for (table, count) in &expected {
        assert!(
            *count > 0,
            "M1-T3: {table} has a populated synthetic witness"
        );
        assert_eq!(
            before[table].len(),
            *count,
            "M1-T3: {table} count before upgrade"
        );
    }
    migrate(&connection).unwrap();
    assert_eq!(
        schema_version(&connection).unwrap(),
        SCHEMA_VERSION,
        "M1-T3: actual C10 suffix raises the version"
    );
    let after = rows(&connection);
    for (table, values) in &before {
        assert_eq!(
            after.get(table),
            Some(values),
            "M1-T3: every retained value, count and identity in {table} survives Phase A"
        );
    }
    let violations: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0, "M1-T3: retained foreign keys remain valid");
    let actual: (String, String, i64, i64, i64, i64, i64) = connection
        .query_row("SELECT owner_api_state,fleet_api_state,fences_pending,old_tables_dropped,purge_required,wal_scrubbed,old_files_removed FROM credential_format_state WHERE singleton_id=1", [], |row| {
            Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))
        }).unwrap();
    assert_eq!(
        actual,
        ("pending".into(), "pending".into(), 1, 0, 1, 0, 0),
        "M1-T3: populated predecessor remains Phase-B pending without consuming old rows"
    );
    let phase_a_state = state(&connection);
    let phase_a_schema = schema(&connection);
    migrate(&connection).unwrap();
    assert_eq!(
        rows(&connection),
        after,
        "M1-T3: reopening the populated predecessor preserves every table value"
    );
    assert_eq!(
        state(&connection),
        phase_a_state,
        "M1-T3: pending conversion state and timestamps are never reinitialised"
    );
    assert_eq!(
        schema(&connection),
        phase_a_schema,
        "M1-T3: populated predecessor suffix runs exactly once"
    );
}

#[test]
fn suffix_steps_run_once() {
    let predecessor_directory = crate::private_tempdir().unwrap();
    let predecessor = Connection::open(predecessor_directory.path().join("hub.sqlite")).unwrap();
    let floor_sql = include_str!("../schema_floor.sql")
        .split("-- C10 credential schema")
        .next()
        .unwrap();
    predecessor.execute_batch(floor_sql).unwrap();
    predecessor
        .pragma_update(None, "application_id", APPLICATION_ID)
        .unwrap();
    predecessor
        .pragma_update(None, "user_version", MIN_STORE_VERSION)
        .unwrap();
    let fresh_directory = crate::private_tempdir().unwrap();
    let fresh = Connection::open(fresh_directory.path().join("hub.sqlite")).unwrap();
    migrate(&predecessor).unwrap();
    migrate(&fresh).unwrap();
    assert_eq!(
        schema_version(&predecessor).unwrap(),
        79,
        "F-SUFFIX: actual next-free C10 rung runs on the floor"
    );
    assert_eq!(
        schema_version(&fresh).unwrap(),
        79,
        "F-SUFFIX: actual next-free C10 rung runs on creation"
    );
    assert_eq!(
        schema(&predecessor),
        schema(&fresh),
        "M1-T12: fresh and upgraded complete schema, constraints, indexes and collations equal"
    );
    let tables: i64 = predecessor.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('credential_pairs','credential_refresh_fences','credential_legacy_fences','credential_refresh_attempts','credential_key_usage','credential_sign_ins','credential_format_state')", [], |row| row.get(0)).unwrap();
    assert_eq!(tables, 7, "F-SUFFIX: seven actual C10 tables exist");
    let predecessor_state = state(&predecessor);
    let fresh_state = state(&fresh);
    let predecessor_schema = schema(&predecessor);
    let fresh_schema = schema(&fresh);
    let predecessor_rows = rows(&predecessor);
    let fresh_rows = rows(&fresh);
    for connection in [&predecessor, &fresh] {
        migrate(connection).unwrap();
        migrate(connection).unwrap();
    }
    assert_eq!(
        schema(&predecessor),
        predecessor_schema,
        "M1-T12: predecessor suffix is never reapplied"
    );
    assert_eq!(
        schema(&fresh),
        fresh_schema,
        "M1-T12: creation suffix is never reapplied"
    );
    assert_eq!(
        rows(&predecessor),
        predecessor_rows,
        "M1-T12: second and third predecessor opens preserve every value"
    );
    assert_eq!(
        rows(&fresh),
        fresh_rows,
        "M1-T12: second and third fresh opens preserve every value"
    );
    assert_eq!(
        state(&predecessor),
        predecessor_state,
        "M1-T12: predecessor state/time witness remains exactly once"
    );
    assert_eq!(
        state(&fresh),
        fresh_state,
        "M1-T12: fresh completion/time witness remains exactly once"
    );
    let predecessor_flags: (i64,i64,i64,Option<i64>) = predecessor.query_row("SELECT purge_required,wal_scrubbed,old_files_removed,completed_at_ms FROM credential_format_state", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    let fresh_flags: (i64,i64,i64,Option<i64>) = fresh.query_row("SELECT purge_required,wal_scrubbed,old_files_removed,completed_at_ms FROM credential_format_state", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(
        predecessor_flags,
        (1, 0, 0, None),
        "M1-T12: empty predecessor is not mistaken for a fresh opening"
    );
    assert_eq!(
        (fresh_flags.0, fresh_flags.1, fresh_flags.2),
        (0, 1, 1),
        "M1-T12: only creation is pre-scrubbed and pre-removed"
    );
    assert!(
        fresh_flags.3.is_some(),
        "M1-T12: only genuine creation completes in Phase A"
    );
    let emptied_directory = crate::private_tempdir().unwrap();
    let emptied_path = emptied_directory.path().join("hub.sqlite");
    let fixture = include_bytes!("../../../../fixtures/g2-4/m1/store-f.sqlite.zst");
    fs::write(
        &emptied_path,
        zstd::stream::decode_all(fixture.as_slice()).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&emptied_path, fs::Permissions::from_mode(0o600)).unwrap();
    let emptied = Connection::open(emptied_path).unwrap();
    emptied.pragma_update(None, "foreign_keys", true).unwrap();
    let former_tables = [
        "legacy_refresh_receipt_bindings",
        "legacy_refresh_input_fences",
        "fleet_refresh_receipt_bindings",
        "fleet_refresh_input_fences",
        "legacy_owner_tokens",
        "fleet_tokens",
    ];
    for table in former_tables {
        let exists: i64 = emptied
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            exists, 1,
            "S-OR-03 actual M1 fixture has {table} before drop"
        );
        emptied
            .execute(&format!("DELETE FROM {table}"), [])
            .unwrap();
    }
    emptied.pragma_update(None, "secure_delete", false).unwrap();
    migrate(&emptied).unwrap();
    let drop_state: (
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
    ) = emptied
        .query_row(
            "SELECT owner_api_state,fleet_api_state,fences_pending,old_tables_dropped,
                purge_required,wal_scrubbed,old_files_removed,converted_at_ms,completed_at_ms
           FROM credential_format_state WHERE singleton_id=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        (
            drop_state.0.as_str(),
            drop_state.1.as_str(),
            drop_state.2,
            drop_state.3
        ),
        ("none", "none", 0, 1)
    );
    assert_eq!(
        (drop_state.4, drop_state.5, drop_state.6),
        (1, 0, 0),
        "S-OR-03 empty predecessor still needs Phase-B cleanup"
    );
    assert!(
        drop_state.7.is_some(),
        "S-OR-03 rung records conversion when no credentials or fences remain"
    );
    assert_eq!(
        drop_state.8, None,
        "S-OR-03 precursor is not marked complete before cleanup"
    );
    for table in former_tables {
        let exists: i64 = emptied
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            exists, 0,
            "S-OR-03 empty legacy table {table} is actually dropped"
        );
    }
    assert_eq!(
        schema(&emptied),
        schema(&fresh),
        "S-OR-03 real empty predecessor converges with fresh schema"
    );
    let violations: i64 = emptied
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        violations, 0,
        "S-OR-03 no dangling foreign keys after real legacy-table drops"
    );
    for (_, name, _, sql) in schema(&emptied) {
        if let Some(sql) = sql {
            for table in former_tables {
                assert!(
                    !sql.contains(table),
                    "S-OR-03 retained object {name} has no dropped-table reference"
                );
            }
        }
    }
    let secure_delete: i64 = emptied
        .pragma_query_value(None, "secure_delete", |row| row.get(0))
        .unwrap();
    assert_eq!(
        secure_delete, 0,
        "S-OR-18 rung restores opening connection secure_delete"
    );
}
