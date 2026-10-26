// SPDX-License-Identifier: AGPL-3.0-only

//! Synthetic preceding-version fixtures; no external catalogue data.

use super::*;
use rusqlite::types::ValueRef;
use std::collections::BTreeMap;

pub(crate) const SOURCE: &str = "10000000-0000-4000-8000-000000000001";
const CUSTOM_SOURCE: &str = "10000000-0000-4000-8000-000000000002";
const CONTROL_SOURCE: &str = "10000000-0000-4000-8000-000000000003";
pub(crate) const VEHICLE: &str = "20000000-0000-4000-8000-000000000001";
const SNAPSHOT: &str = "30000000-0000-4000-8000-000000000001";
pub(crate) type Rows = BTreeMap<String, Vec<Vec<Value>>>;

pub(crate) fn preceding(connection: &Connection) {
    migrate_to_previous_import_names(connection).unwrap();
    assert_eq!(
        schema_version(connection).unwrap(),
        import_source_names_step::VERSION - 1
    );
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    for (id, kind, key) in [
        (
            SOURCE,
            import_source_names_step::OLD_KIND,
            import_source_names_step::OLD_DEFAULT_KEY,
        ),
        (
            CUSTOM_SOURCE,
            import_source_names_step::OLD_KIND,
            "retained-custom-key",
        ),
        (CONTROL_SOURCE, "owner_api", "unaffected-control"),
    ] {
        connection
            .execute(
                "INSERT INTO sources VALUES (?1,?2,3,1000)",
                params![id, kind],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_identities VALUES (?1,?2,?3)",
                params![id, kind, key],
            )
            .unwrap();
    }
    for index in 1..=65 {
        let vehicle = format!("20000000-0000-4000-8000-{index:012}");
        connection.execute("INSERT INTO vehicles(vehicle_id,source_id,source_vehicle_key,vin,display_name,created_at_ms,last_seen_at_ms) VALUES (?1,?2,?3,NULL,'Synthetic car',1000,2000)", params![vehicle,SOURCE,format!("car-{index}")]).unwrap();
        // Alternate whitespace that shrinks and exponent notation that grows
        // through the real writer serializer. Three batches exercise keysets.
        let payload = if index % 2 == 0 {
            format!(
                "{{\"record_type\":\"{}\",\"measurements\":[{}],\"position_id\":{index}}}",
                import_source_names_step::OLD_POSITION_TYPE,
                vec!["1e5"; 1200].join(",")
            )
        } else {
            serde_json::to_string_pretty(&serde_json::json!({"record_type":import_source_names_step::OLD_POSITION_TYPE,"position_id":index,"padding":vec!["retained";300]})).unwrap()
        };
        insert_current(
            connection,
            &vehicle,
            SOURCE,
            import_source_names_step::OLD_POSITION_TYPE,
            index,
            &payload,
        )
        .unwrap();
    }
    let control = "20000000-0000-4000-8000-000000000099";
    connection.execute("INSERT INTO vehicles(vehicle_id,source_id,source_vehicle_key,created_at_ms,last_seen_at_ms) VALUES (?1,?2,'control',1000,2000)", params![control,CONTROL_SOURCE]).unwrap();
    insert_current(
        connection,
        control,
        CONTROL_SOURCE,
        "owner_api_vehicle_data_v1",
        100,
        "{\"record_type\":\"owner_api_vehicle_data_v1\",\"sentinel\":17}",
    )
    .unwrap();
    insert_current(
        connection,
        VEHICLE,
        SOURCE,
        "tesla_stream_update_v1",
        101,
        "{\"record_type\":\"tesla_stream_update_v1\",\"sentinel\":23}",
    )
    .unwrap();
    connection
        .execute(
            "INSERT INTO sync_bases VALUES (?1,?2,7,?3,?4)",
            params![VEHICLE, SNAPSHOT, "a".repeat(64), b"[]".as_slice()],
        )
        .unwrap();
    for index in [0, 2] {
        connection
            .execute(
                &format!(
                    "INSERT INTO {} VALUES (?1,?2,?3,42,7)",
                    import_source_names_step::OLD_TABLES[index]
                ),
                params![VEHICLE, SOURCE, SNAPSHOT],
            )
            .unwrap();
    }
    connection
        .execute(
            &format!(
                "INSERT INTO {} VALUES (?1,'drive',11)",
                import_source_names_step::OLD_TABLES[1]
            ),
            [VEHICLE],
        )
        .unwrap();
    connection
        .execute(
            &format!(
                "INSERT INTO {} VALUES (?1,'drive',1,11,42,?2)",
                import_source_names_step::OLD_TABLES[3]
            ),
            params![VEHICLE, vec![0x31u8; 32]],
        )
        .unwrap();
    connection
        .execute(
            &format!(
                "INSERT INTO {} VALUES (?1,?2,7,'logical_projection_v1',?3,?4)",
                import_source_names_step::OLD_TABLES[4]
            ),
            params![VEHICLE, SNAPSHOT, vec![0x41u8; 32], vec![0x51u8; 32]],
        )
        .unwrap();
    connection
        .execute(
            &format!(
                "INSERT INTO {} VALUES (1,?1,?2,2000000000,1000000000,?3)",
                import_source_names_step::OLD_TABLES[5]
            ),
            params![
                vec![1u8, 2, 3],
                vec![4u8, 5, 6],
                "40000000-0000-4000-8000-000000000001"
            ],
        )
        .unwrap();
    connection.execute("INSERT INTO vehicle_identity_aliases(alias_kind,alias_value,vehicle_id,source_id,source_vehicle_key) SELECT 'source_key',source_id || ':' || source_vehicle_key,vehicle_id,source_id,source_vehicle_key FROM vehicles",[]).unwrap();
    seed_generic_manifest(connection);
    let historical = format!(
        "{{\"record_type\":\"{}\",\"historical\":true}}",
        import_source_names_step::OLD_POSITION_TYPE
    );
    connection.execute("INSERT INTO raw_observations(observation_id,source_id,vehicle_id,observed_at_ms,received_at_ms,payload_sha256,payload_json) VALUES (999,?1,?2,1000,2000,?3,?4)",params![SOURCE,VEHICLE,Sha256Digest::of_bytes(historical.as_bytes()).as_bytes().as_slice(),historical]).unwrap();
}

pub(crate) fn insert_current(
    connection: &Connection,
    vehicle: &str,
    source: &str,
    kind: &str,
    id: i64,
    payload: &str,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO current_observations VALUES (?1,?2,?3,?4,1000,2000,?5,?6)",
        params![
            vehicle,
            kind,
            id,
            source,
            Sha256Digest::of_bytes(payload.as_bytes())
                .as_bytes()
                .as_slice(),
            payload
        ],
    )
}

fn cell(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => serde_json::json!(["null"]),
        ValueRef::Integer(v) => serde_json::json!(["integer", v]),
        ValueRef::Real(v) => serde_json::json!(["real", v.to_bits().to_string()]),
        ValueRef::Text(v) => serde_json::json!(["text", std::str::from_utf8(v).unwrap()]),
        ValueRef::Blob(v) => serde_json::json!(["blob", v]),
    }
}

pub(crate) fn snapshot(connection: &Connection) -> Rows {
    let names = connection.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    let mut result = Rows::new();
    for name in names {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
            .unwrap();
        let count = statement.column_count();
        let mut rows = statement
            .query_map([], |r| {
                Ok((0..count)
                    .map(|i| cell(r.get_ref(i).unwrap()))
                    .collect::<Vec<_>>())
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort_by_key(|row| serde_json::to_string(row).unwrap());
        result.insert(name, rows);
    }
    result
}

pub(crate) type SchemaSnapshot = (i32, Vec<(String, String, Option<String>)>, Rows);

pub(crate) fn schema_snapshot(connection: &Connection) -> SchemaSnapshot {
    let schema = connection
        .prepare("SELECT type,name,sql FROM sqlite_schema ORDER BY type,name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (
        schema_version(connection).unwrap(),
        schema,
        snapshot(connection),
    )
}

pub(crate) fn expected(mut before: Rows) -> Rows {
    // Independent expected transformations use explicit column positions from
    // the preceding fixture's schema, never the production migration helpers.
    for (old, new) in import_source_names_step::OLD_TABLES.iter().zip([
        "import_projection_heads",
        "import_projection_rows",
        "import_projection_state_heads",
        "import_projection_state_rows",
        "import_projection_state_bridges",
        "legacy_owner_tokens",
    ]) {
        let rows = before.remove(*old).unwrap();
        before.insert(new.to_owned(), rows);
    }
    for row in before.get_mut("sources").unwrap() {
        if row[1][1] == import_source_names_step::OLD_KIND {
            row[1] = serde_json::json!(["text", "source_db"]);
        }
    }
    for row in before.get_mut("source_identities").unwrap() {
        if row[1][1] == import_source_names_step::OLD_KIND {
            row[1] = serde_json::json!(["text", "source_db"]);
            if row[2][1] == import_source_names_step::OLD_DEFAULT_KEY {
                row[2] = serde_json::json!(["text", "default"]);
            }
        }
    }
    for row in before.get_mut("current_observations").unwrap() {
        if row[1][1] == import_source_names_step::OLD_POSITION_TYPE {
            row[1] = serde_json::json!(["text", "source_db_position_v1"]);
            let mut payload: Value = serde_json::from_str(row[7][1].as_str().unwrap()).unwrap();
            payload["record_type"] = Value::String("source_db_position_v1".into());
            let bytes = serde_json::to_vec(&payload).unwrap();
            row[6] =
                serde_json::json!(["blob", Sha256Digest::of_bytes(&bytes).as_bytes().to_vec()]);
            row[7] = serde_json::json!(["text", String::from_utf8(bytes).unwrap()]);
        }
    }
    for rows in before.values_mut() {
        rows.sort_by_key(|row| serde_json::to_string(row).unwrap());
    }
    before
}

pub(crate) fn assert_expected(connection: &Connection, expected: &Rows) {
    let actual = snapshot(connection);
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (name, rows) in expected {
        assert_eq!(actual[name].len(), rows.len(), "row count: {name}");
        assert_eq!(&actual[name], rows, "complete ordered rows: {name}");
        assert_eq!(
            Sha256Digest::of_bytes(&serde_json::to_vec(&actual[name]).unwrap()),
            Sha256Digest::of_bytes(&serde_json::to_vec(rows).unwrap()),
            "ordered row hash: {name}"
        );
    }
    assert_eq!(
        schema_version(connection).unwrap(),
        import_source_names_step::VERSION
    );
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !connection
            .prepare("PRAGMA foreign_key_check(current_observations)")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let sql = connection
        .query_row(
            "SELECT group_concat(sql,'\n') FROM sqlite_schema",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    for old in import_source_names_step::OLD_TABLES {
        assert!(!sql.contains(old), "old schema reference");
    }
    assert!(!sql.contains(import_source_names_step::OLD_POSITION_TYPE));
    assert!(sql.contains("source_db_position_v1"));
    assert!(
        actual["sources"]
            .iter()
            .filter(|r| r[0][1] == SOURCE || r[0][1] == CUSTOM_SOURCE)
            .all(|r| r[1][1] == "source_db"),
        "affected source kinds must switch"
    );
}

// Synthetic generic-transport metadata keeps the populated projection heads
// valid when ordinary restore runs the historical lineage recovery path.
fn seed_generic_manifest(connection: &Connection) {
    use crate::protocol::{
        CursorClaims, MirrorTable, PackCompression, PackFormat, ProtocolVersion, SchemaVersion,
        SequenceRange, TransferMode, TransportPack,
    };
    let installation = Uuid::parse_str("50000000-0000-4000-8000-000000000001").unwrap();
    let account = Uuid::parse_str(SOURCE).unwrap();
    let vehicle = Uuid::parse_str(VEHICLE).unwrap();
    let snapshot = Uuid::parse_str(SNAPSHOT).unwrap();
    let schema = SchemaVersion { major: 1, minor: 0 };
    let protocol = ProtocolVersion { major: 1, minor: 0 };
    let digest = Sha256Digest::of_bytes(&[7u8; 100]);
    let cursor = OpaqueCursor::issue(
        &CursorKey::from_bytes([7; 32]),
        CursorClaims {
            protocol,
            schema,
            installation_id: installation,
            account_id: account,
            vehicle_id: vehicle,
            generation: 1,
            sequence: 7,
        },
    )
    .unwrap();
    let pack = TransportPack {
        pack_id: Uuid::parse_str("60000000-0000-4000-8000-000000000001").unwrap(),
        snapshot_id: snapshot,
        ordinal: 0,
        schema,
        format: PackFormat::SqliteTransport,
        compression: PackCompression::Zstd,
        relative_path: TransportPack::canonical_relative_path(digest),
        sha256: digest,
        compressed_bytes: 100,
        uncompressed_bytes: 100,
        row_count: 1,
        sequence: SequenceRange {
            from_exclusive: 7,
            to_inclusive: 7,
        },
        tables: vec![MirrorTable::Vehicle],
    };
    let manifest = SyncManifest {
        protocol,
        schema,
        installation_id: installation,
        account_id: account,
        vehicle_id: vehicle,
        generation: 1,
        snapshot_id: snapshot,
        mode: TransferMode::FullSnapshot,
        base_sequence: 7,
        head_sequence: 7,
        chunk_count: 1,
        total_compressed_bytes: 100,
        total_uncompressed_bytes: 100,
        total_rows: 1,
        chunks: vec![pack],
        terminal_cursor: cursor,
    };
    manifest.validate().unwrap();
    connection
        .execute(
            "INSERT INTO sync_manifests VALUES (?1,?2,7,?3)",
            params![SNAPSHOT, VEHICLE, serde_json::to_vec(&manifest).unwrap()],
        )
        .unwrap();
}

/// Existing older-step tests first build a current store, populate it through
/// current APIs, then rewind its schema. Finish that fixture construction by
/// restoring the preceding import names before the real migration is tested.
/// The v78 import-name rewind also rewrites payload type digests, so it is a Rust
/// fixture hook rather than SQL-only REWIND_V78_SQL. Call this hook after older
/// SQL rewinds and before migration. Fresh fixtures use `preceding` directly.
pub(crate) fn prepare_historical_import_names(connection: &Connection) {
    if schema_version(connection).unwrap() >= import_source_names_step::VERSION {
        return;
    }
    let has_neutral: bool = connection
        .query_row(
            concat!(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND ",
                "(name IN ('import_projection_heads','import_projection_rows',",
                "'import_projection_state_heads','import_projection_state_rows',",
                "'import_projection_state_bridges','legacy_owner_tokens') OR ",
                "(name='current_observations' AND instr(sql,'source_db_position_v1')>0)))",
            ),
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !has_neutral {
        return;
    }
    for (old, new) in import_source_names_step::OLD_TABLES
        .iter()
        .zip(import_source_names_step::NEW_TABLES)
    {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [new],
                |r| r.get(0),
            )
            .unwrap();
        if exists {
            connection
                .execute_batch(&format!("ALTER TABLE {new} RENAME TO {old}"))
                .unwrap();
        }
    }
    let sql: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name='current_observations'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    if let Some(sql) = sql.filter(|s| {
        s.contains(crate::collection::source_db_current::SOURCE_DB_POSITION_RECORD_TYPE)
    }) {
        let body = sql.find('(').unwrap();
        let old_sql = format!(
            "CREATE TABLE current_observations_before_import_names{}",
            &sql[body..]
        )
        .replace(
            crate::collection::source_db_current::SOURCE_DB_POSITION_RECORD_TYPE,
            import_source_names_step::OLD_POSITION_TYPE,
        );
        connection.execute_batch(&old_sql).unwrap();
        connection.execute("INSERT INTO current_observations_before_import_names SELECT vehicle_id,CASE record_type WHEN ?1 THEN ?2 ELSE record_type END,observation_id,source_id,observed_at_ms,received_at_ms,payload_sha256,payload_json FROM current_observations",params![crate::collection::source_db_current::SOURCE_DB_POSITION_RECORD_TYPE,import_source_names_step::OLD_POSITION_TYPE]).unwrap();
        connection.execute_batch("DROP TABLE current_observations; ALTER TABLE current_observations_before_import_names RENAME TO current_observations;").unwrap();
        let rows = connection
            .prepare(
                "SELECT vehicle_id,payload_json FROM current_observations WHERE record_type=?1",
            )
            .unwrap()
            .query_map([import_source_names_step::OLD_POSITION_TYPE], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for (vehicle, payload) in rows {
            let mut value: Value = serde_json::from_str(&payload).unwrap();
            value["record_type"] =
                Value::String(import_source_names_step::OLD_POSITION_TYPE.into());
            let bytes = serde_json::to_vec(&value).unwrap();
            connection.execute("UPDATE current_observations SET payload_json=?1,payload_sha256=?2 WHERE vehicle_id=?3 AND record_type=?4",params![String::from_utf8(bytes.clone()).unwrap(),Sha256Digest::of_bytes(&bytes).as_bytes().as_slice(),vehicle,import_source_names_step::OLD_POSITION_TYPE]).unwrap();
        }
    }
    connection.execute("UPDATE source_identities SET source_kind=?1,source_key=CASE source_key WHEN 'default' THEN ?1 ELSE source_key END WHERE source_kind='source_db'",[import_source_names_step::OLD_KIND]).unwrap();
    connection
        .execute(
            "UPDATE sources SET source_kind=?1 WHERE source_kind='source_db'",
            [import_source_names_step::OLD_KIND],
        )
        .unwrap();
}
