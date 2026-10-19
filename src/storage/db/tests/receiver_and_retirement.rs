// SPDX-License-Identifier: AGPL-3.0-only

// Receiver state (schema 73) and the retirement of the delivery consumer's
// tables (schema 74). Every store here is a temporary synthetic one.

const RR_INSTALLATION: &str = "synthetic-installation";
const RR_EDGE_TABLES: [&str; 6] = [
    "edge_lineages",
    "edge_applications",
    "edge_sequence_dispositions",
    "edge_accumulator_states",
    "edge_pending_publications",
    "edge_consumer_diagnostics",
];

fn rr_hex(seed: u8) -> String {
    format!("{seed:02x}").repeat(32)
}

/// The rows of one table as strings, sorted, so two snapshots compare exactly.
fn rr_rows(connection: &Connection, table: &str) -> Vec<Vec<String>> {
    use rusqlite::types::ValueRef;
    let mut statement = connection
        .prepare(&format!("SELECT * FROM \"{table}\""))
        .expect("snapshot statement");
    let columns = statement.column_count();
    let mut rows: Vec<Vec<String>> = statement
        .query_map([], |row| {
            (0..columns)
                .map(|index| {
                    Ok(match row.get_ref(index)? {
                        ValueRef::Null => "NULL".to_owned(),
                        ValueRef::Integer(value) => format!("i:{value}"),
                        ValueRef::Real(value) => format!("r:{value:?}"),
                        ValueRef::Text(value) => format!("t:{}", String::from_utf8_lossy(value)),
                        ValueRef::Blob(value) => format!("b:{}", hex::encode(value)),
                    })
                })
                .collect()
        })
        .expect("snapshot rows")
        .collect::<Result<_, _>>()
        .expect("snapshot row");
    rows.sort();
    rows
}

fn rr_schema(connection: &Connection) -> Vec<(String, String, Option<String>)> {
    connection
        .prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn rr_now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn rr_user_version(connection: &Connection) -> i32 {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

fn rr_table_exists(connection: &Connection, name: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [name],
            |row| row.get(0),
        )
        .unwrap()
}

/// A separate read-only connection, as the rollback proof requires.
fn rr_read_only(dir: &std::path::Path) -> Connection {
    Connection::open_with_flags(
        dir.join("hub.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("read-only connection")
}

/// A store rewound to schema 73 with the retired objects restored, plus the
/// seeded synthetic rows: two lineages (one with an accumulator state), three
/// gap rows, one record disposition and a pending publication per `pending`.
fn rr_store_at_73(pending: &[&str]) -> (tempfile::TempDir, HubStore, Uuid, Uuid) {
    let (temporary, store) = cs_store();
    let with_state = cs_vehicle(&store, "rr-with-state");
    let without_state = cs_vehicle(&store, "rr-without-state");
    let connection = store.open().expect("connection");
    let source: String = connection
        .query_row(
            "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
            [with_state.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    // Sparse observations and an unpublished sync mutation the retirement must
    // leave exactly as they are (the tests compare them before and after).
    for (vehicle, observed_at_ms) in [(with_state, 1_000_i64), (without_state, 2_000)] {
        store
            .append_observation(
                &ObservationInput {
                    source_id: source.parse().unwrap(),
                    vehicle_id: vehicle,
                    observed_at_ms,
                    payload: serde_json::json!({
                        "record_type": "fleet_api_vehicle_data_v1",
                        "provider_feed": "fleet_telemetry",
                        "provider_raw_json": {"response": {"charge_state": {"battery_level": 55}}},
                    }),
                },
                observed_at_ms + 1,
            )
            .expect("seed observation");
    }
    connection
        .execute(
            "INSERT INTO sync_mutations(vehicle_id, revision, entity, entity_id, car_id,
                operation, payload_json, published, claimed_until_ms)
             VALUES (?1, 900, 'car_setting', 1, 1, 'upsert', '{\"seed\":true}', 0, 0)",
            [with_state.to_string()],
        )
        .expect("seed unpublished mutation");
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V74_SQL} PRAGMA user_version = 73; COMMIT;"
        ))
        .expect("rewind to 73");
    for (lineage, vehicle, car_id) in [("l1", with_state, 1_i64), ("l2", without_state, 2_i64)] {
        connection
            .execute(
                "INSERT INTO edge_lineages(installation_id, lineage, source_id, vehicle_id, vin,
                    car_id, first_spool_seq, ack_frontier, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, '5YJ3E1EA7KF000001', ?5, 1, 3, 10, 20)",
                rusqlite::params![RR_INSTALLATION, lineage, source, vehicle.to_string(), car_id],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO edge_accumulator_states(installation_id, lineage, vehicle_id,
                state_version, state_json, through_spool_seq, updated_at_ms)
             VALUES (?1, 'l1', ?2, 1, x'7b7d', 3, 30)",
            rusqlite::params![RR_INSTALLATION, with_state.to_string()],
        )
        .unwrap();
    for (lineage, seq, seed, reason, occurred) in [
        ("l1", 1_i64, 0xa1_u8, "lost_envelope", 100_i64),
        ("l1", 2, 0xa2, "lost_stable_record_id", 200),
        ("l2", 1, 0xa3, "lost_envelope", 300),
    ] {
        connection
            .execute(
                "INSERT INTO edge_sequence_dispositions(installation_id, lineage, spool_seq,
                    item_kind, item_id, legacy_record_id, payload_sha256, category, reason,
                    occurred_at_ms, evidence_sha256, committed_at_ms)
                 VALUES (?1, ?2, ?3, 'gap', ?4, NULL, NULL, 'durable_gap', ?5, ?6, ?7, 400)",
                rusqlite::params![
                    RR_INSTALLATION,
                    lineage,
                    seq,
                    rr_hex(seed),
                    reason,
                    occurred,
                    rr_hex(seed + 0x10)
                ],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO edge_sequence_dispositions(installation_id, lineage, spool_seq,
                item_kind, item_id, legacy_record_id, payload_sha256, category, reason,
                occurred_at_ms, evidence_sha256, committed_at_ms)
             VALUES (?1, 'l1', 3, 'record', ?2, ?3, ?4, 'projected_telemetry', 'ok',
                NULL, NULL, 500)",
            rusqlite::params![RR_INSTALLATION, rr_hex(0xb1), rr_hex(0xb2), rr_hex(0xb3)],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO edge_consumer_diagnostics(installation_id, lineage, state, detail,
                updated_at_ms)
             VALUES (?1, 'l1', 'connected', 'seeded', 40)",
            rusqlite::params![RR_INSTALLATION],
        )
        .unwrap();
    for (index, status) in pending.iter().enumerate() {
        let record = rr_hex(0xc0 + u8::try_from(index).unwrap());
        connection
            .execute(
                "INSERT INTO edge_applications(installation_id, lineage, stable_record_id,
                    payload_sha256, disposition, first_spool_seq, observation_id, applied_at_ms)
                 VALUES (?1, 'l1', ?2, ?3, 'projected_telemetry', ?4, NULL, 600)",
                rusqlite::params![RR_INSTALLATION, record, rr_hex(0xd0), 10 + index as i64],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO edge_pending_publications(installation_id, lineage, stable_record_id,
                    vehicle_id, status, attempts, next_attempt_ms, last_error, created_at_ms,
                    completed_at_ms)
                 VALUES (?1, 'l1', ?2, ?3, ?4, 0, 0, NULL, 700, NULL)",
                rusqlite::params![RR_INSTALLATION, record, with_state.to_string(), status],
            )
            .unwrap();
    }
    drop(connection);
    (temporary, store, with_state, without_state)
}

#[test]
fn accumulator_state_table_enforces_version_bounds_and_cascade() {
    let (temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "rr-state");
    let connection = store.open().unwrap();
    let insert = |version: i64, length: usize| {
        connection.execute(
            "INSERT INTO fleet_telemetry_accumulator_states(
                vehicle_id, state_version, state_json, updated_at_ms)
             VALUES (?1, ?2, ?3, 1)",
            rusqlite::params![vehicle.to_string(), version, vec![b'x'; length]],
        )
    };
    assert!(insert(1, 10).is_err(), "version one is refused");
    assert!(insert(3, 10).is_err(), "version three is refused");
    assert!(insert(2, 1).is_err(), "one byte is too short");
    assert!(insert(2, 262_145).is_err(), "262145 bytes is too long");
    assert_eq!(insert(2, 262_144).unwrap(), 1);
    connection
        .execute("DELETE FROM fleet_telemetry_accumulator_states", [])
        .unwrap();
    assert_eq!(insert(2, 2).unwrap(), 1);
    // The store accessors round-trip the bytes and only upsert version two.
    assert_eq!(store.load_receiver_state(vehicle).unwrap(), Some(vec![b'x'; 2]));
    let mut other = store.open().unwrap();
    let transaction = other.transaction().unwrap();
    HubStore::put_receiver_state_in_transaction(&transaction, vehicle, b"{\"a\":1}", 9).unwrap();
    transaction.commit().unwrap();
    assert_eq!(
        store.load_receiver_state(vehicle).unwrap(),
        Some(b"{\"a\":1}".to_vec())
    );
    // Deleting the vehicle removes its state.
    connection
        .execute("DELETE FROM vehicles WHERE vehicle_id = ?1", [vehicle.to_string()])
        .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM fleet_telemetry_accumulator_states",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(temporary);
}

#[test]
fn retirement_copies_gaps_and_drops_edge_objects() {
    let (temporary, store, with_state, _) = rr_store_at_73(&["pending"]);
    let before = store.open().unwrap();
    // All six retired tables hold rows, so the drops remove real data.
    for table in RR_EDGE_TABLES {
        assert!(!rr_rows(&before, table).is_empty(), "{table} is seeded");
    }
    let observations_before = rr_rows(&before, "raw_observations");
    assert_eq!(observations_before.len(), 2, "sparse observations are seeded");
    let mutations_before = rr_rows(&before, "sync_mutations");
    assert_eq!(mutations_before.len(), 1, "an unpublished mutation is seeded");
    let dispositions: Vec<Vec<String>> = rr_rows(&before, "edge_sequence_dispositions");
    assert_eq!(dispositions.len(), 4);
    drop(before);
    drop(store);

    let clock_before = rr_now_ms();
    let store = HubStore::initialize(temporary.path()).expect("migrated");
    let clock_after = rr_now_ms();
    let connection = store.open().unwrap();
    assert_eq!(rr_user_version(&connection), SCHEMA_VERSION);
    let copy_times: Vec<i64> = connection
        .prepare("SELECT copied_at_ms FROM retired_delivery_gaps")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(copy_times.len(), 3);
    for copied_at_ms in copy_times {
        assert!(
            (clock_before..=clock_after).contains(&copied_at_ms),
            "copied_at_ms {copied_at_ms} lies within the migration's clock bounds"
        );
    }
    let gaps = rr_rows(&connection, "retired_delivery_gaps");
    assert_eq!(gaps.len(), 3, "only the three gap rows are copied");
    let mut statement = connection
        .prepare(
            "SELECT installation_id, lineage, spool_seq, item_id, reason, occurred_at_ms,
                    evidence_sha256, committed_at_ms, vehicle_id
               FROM retired_delivery_gaps ORDER BY lineage, spool_seq",
        )
        .unwrap();
    let rows: Vec<(String, String, i64, String, String, i64, String, i64, Option<String>)> = statement
        .query_map([], |row| {
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
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let expected = [
        ("l1", 1, 0xa1_u8, "lost_envelope", 100, Some(with_state.to_string())),
        ("l1", 2, 0xa2, "lost_stable_record_id", 200, Some(with_state.to_string())),
        ("l2", 1, 0xa3, "lost_envelope", 300, None),
    ];
    for (row, (lineage, seq, seed, reason, occurred, vehicle)) in rows.iter().zip(expected) {
        assert_eq!(row.0, RR_INSTALLATION);
        assert_eq!(row.1, lineage);
        assert_eq!(row.2, seq);
        assert_eq!(row.3, rr_hex(seed));
        assert_eq!(row.4, reason);
        assert_eq!(row.5, occurred);
        assert_eq!(row.6, rr_hex(seed + 0x10));
        assert_eq!(row.7, 400);
        assert_eq!(row.8, vehicle, "the lineage's accumulator state assigns the vehicle");
    }
    drop(statement);
    for table in RR_EDGE_TABLES {
        assert!(!rr_table_exists(&connection, table), "{table} is dropped");
    }
    for object in [
        "edge_lineages_require_car_id",
        "edge_lineages_immutable_car_id",
        "edge_pending_publication_due",
        "edge_sequence_item_identity",
    ] {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
                [object],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!present, "{object} is dropped");
    }
    assert_eq!(rr_rows(&connection, "raw_observations"), observations_before);
    assert_eq!(rr_rows(&connection, "sync_mutations"), mutations_before);
    // The pending publication marked its vehicle for derived-work replay.
    assert_eq!(
        rr_rows(&connection, "export_outbox")
            .iter()
            .map(|row| row[0].clone())
            .collect::<Vec<_>>(),
        vec![format!("t:{with_state}")]
    );
}

#[test]
fn retirement_creates_an_empty_gap_table_without_gaps() {
    let (temporary, store) = cs_store();
    drop(store);
    // A catalogue at 73 that never held a disposition table.
    let store = HubStore::initialize(temporary.path()).unwrap();
    let connection = store.open().unwrap();
    connection
        .execute_batch(
            "BEGIN IMMEDIATE; DROP TABLE retired_delivery_gaps; PRAGMA user_version = 73; COMMIT;",
        )
        .unwrap();
    assert!(!rr_table_exists(&connection, "edge_sequence_dispositions"));
    drop(connection);
    drop(store);
    let store = HubStore::initialize(temporary.path()).unwrap();
    let connection = store.open().unwrap();
    assert_eq!(rr_user_version(&connection), SCHEMA_VERSION);
    assert!(rr_rows(&connection, "retired_delivery_gaps").is_empty());
}

#[test]
fn retirement_marks_pending_vehicles_in_the_export_outbox() {
    let (temporary, store, with_state, _) = rr_store_at_73(&["pending", "complete"]);
    drop(store);
    let store = HubStore::initialize(temporary.path()).unwrap();
    let connection = store.open().unwrap();
    let marked: Vec<String> = connection
        .prepare("SELECT vehicle_id FROM export_outbox ORDER BY vehicle_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(marked, vec![with_state.to_string()]);
    drop(connection);
    drop(store);

    // A publication that is complete marks nothing.
    let (temporary, store, _, _) = rr_store_at_73(&["complete"]);
    drop(store);
    let store = HubStore::initialize(temporary.path()).unwrap();
    let connection = store.open().unwrap();
    assert!(rr_rows(&connection, "export_outbox").is_empty());
}

#[test]
fn retirement_failure_rolls_back_every_change() {
    for fault in [
        RetirementFault::GapCopy,
        RetirementFault::OutboxMarking,
        RetirementFault::TableDrops,
    ] {
        let (temporary, store, _, _) = rr_store_at_73(&["pending"]);
        drop(store);
        let read = rr_read_only(temporary.path());
        let schema_before = rr_schema(&read);
        let tracked = [
            "edge_lineages",
            "edge_applications",
            "edge_sequence_dispositions",
            "edge_accumulator_states",
            "edge_pending_publications",
            "edge_consumer_diagnostics",
            "export_outbox",
            "raw_observations",
            "sync_mutations",
        ];
        let rows_before: Vec<_> = tracked.iter().map(|table| rr_rows(&read, table)).collect();
        for (table, rows) in tracked.iter().zip(&rows_before) {
            // The outbox is empty at 73 by design: the step is what fills it.
            assert_eq!(rows.is_empty(), *table == "export_outbox", "{table} seed");
        }
        assert_eq!(rr_user_version(&read), 73);
        drop(read);

        set_retirement_fault(Some(fault));
        let failed = HubStore::initialize(temporary.path());
        set_retirement_fault(None);
        assert!(
            matches!(failed, Err(StoreError::Migrate(_))),
            "{fault:?} must fail the open"
        );

        let read = rr_read_only(temporary.path());
        assert_eq!(rr_user_version(&read), 73, "{fault:?}: version unchanged");
        assert_eq!(rr_schema(&read), schema_before, "{fault:?}: schema unchanged");
        assert!(!rr_table_exists(&read, "retired_delivery_gaps"), "{fault:?}");
        for table in RR_EDGE_TABLES {
            assert!(rr_table_exists(&read, table), "{fault:?}: {table}");
        }
        let rows_after: Vec<_> = tracked.iter().map(|table| rr_rows(&read, table)).collect();
        assert_eq!(rows_after, rows_before, "{fault:?}: rows unchanged");
        assert!(rr_rows(&read, "export_outbox").is_empty(), "{fault:?}");
        drop(read);

        // With the fault cleared the same store migrates.
        let store = HubStore::initialize(temporary.path()).expect("retry succeeds");
        let connection = store.open().unwrap();
        assert_eq!(rr_user_version(&connection), SCHEMA_VERSION);
        assert_eq!(rr_rows(&connection, "retired_delivery_gaps").len(), 3);
    }
}

/// R5(b): whatever fails inside the v74 step except the clock is a migration
/// error, with the whole step rolled back.
#[test]
fn retirement_marking_failures_are_migration_errors_and_roll_back() {
    type Break = fn(&Connection);
    let breaks: [(&str, Break); 2] = [
        ("the outbox write is refused", |connection| {
            connection
                .execute_batch(
                    "CREATE TRIGGER rr_refuse_outbox BEFORE INSERT ON export_outbox
                     BEGIN SELECT RAISE(ABORT, 'refused'); END;",
                )
                .unwrap();
        }),
        ("a pending publication names no valid vehicle", |connection| {
            connection.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
            connection
                .execute(
                    "UPDATE edge_pending_publications SET vehicle_id = 'not-a-uuid'
                      WHERE status != 'complete'",
                    [],
                )
                .unwrap();
        }),
    ];
    for (name, break_it) in breaks {
        let (temporary, store, _, _) = rr_store_at_73(&["pending"]);
        drop(store);
        let writable = Connection::open(temporary.path().join("hub.sqlite")).unwrap();
        break_it(&writable);
        drop(writable);

        let failed = HubStore::initialize(temporary.path());
        assert!(
            matches!(failed, Err(StoreError::Migrate(_))),
            "{name}: expected a migration error, got {:?}",
            failed.err()
        );
        let read = rr_read_only(temporary.path());
        assert_eq!(rr_user_version(&read), 73, "{name}: version unchanged");
        assert!(!rr_table_exists(&read, "retired_delivery_gaps"), "{name}");
        for table in RR_EDGE_TABLES {
            assert!(rr_table_exists(&read, table), "{name}: {table}");
        }
        assert!(rr_rows(&read, "export_outbox").is_empty(), "{name}");
    }
}

#[test]
fn rewind_constants_restore_the_previous_shape() {
    let (fresh_dir, fresh) = cs_store();
    let fresh_schema = rr_schema(&fresh.open().unwrap());
    drop(fresh);
    drop(fresh_dir);
    for (rewind, version) in [(REWIND_V74_SQL, 73), (REWIND_V73_SQL, 72)] {
        let (temporary, store) = cs_store();
        let connection = store.open().unwrap();
        connection
            .execute_batch(&format!(
                "BEGIN IMMEDIATE; {rewind} PRAGMA user_version = {version}; COMMIT;"
            ))
            .unwrap();
        assert!(rr_table_exists(&connection, "edge_lineages"), "v{version}");
        assert!(!rr_table_exists(&connection, "retired_delivery_gaps"));
        assert_eq!(
            rr_table_exists(&connection, "fleet_telemetry_accumulator_states"),
            version == 73
        );
        drop(connection);
        drop(store);
        let migrated = HubStore::initialize(temporary.path()).unwrap();
        let connection = migrated.open().unwrap();
        assert_eq!(rr_user_version(&connection), SCHEMA_VERSION);
        let strip = |schema: Vec<(String, String, Option<String>)>| {
            schema
                .into_iter()
                .map(|(kind, name, sql)| (kind, name, sql.map(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" "))))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            strip(rr_schema(&connection)),
            strip(fresh_schema.clone()),
            "rewinding to v{version} migrates to the fresh schema"
        );
    }
}

/// S1: the version each step writes inside its own SQL, pinned apart from the
/// version number the migration chain tracks in Rust. The receiver step alone
/// leaves a store at 73 and the retirement step alone moves it to 74, so a
/// wrong hand renumber of either literal fails here.
#[test]
fn each_step_writes_its_own_schema_version() {
    let (_temporary, store) = cs_store();
    let connection = store.open().unwrap();
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V73_SQL} PRAGMA user_version = 72; COMMIT;"
        ))
        .unwrap();
    assert_eq!(rr_user_version(&connection), 72);
    assert!(!rr_table_exists(
        &connection,
        "fleet_telemetry_accumulator_states"
    ));

    migrate_receiver_state_table(&connection).unwrap();
    assert_eq!(rr_user_version(&connection), 73, "the receiver step writes 73");
    assert!(rr_table_exists(
        &connection,
        "fleet_telemetry_accumulator_states"
    ));
    assert!(!rr_table_exists(&connection, "retired_delivery_gaps"));
    for table in RR_EDGE_TABLES {
        assert!(rr_table_exists(&connection, table), "{table} survives 73");
    }

    migrate_retire_delivery_tables(&connection).unwrap();
    assert_eq!(rr_user_version(&connection), 74, "the retirement step writes 74");
    assert_eq!(SCHEMA_VERSION, 74);
    assert!(rr_table_exists(&connection, "retired_delivery_gaps"));
    for table in RR_EDGE_TABLES {
        assert!(!rr_table_exists(&connection, table), "{table} dropped at 74");
    }
}

/// S1: a store at 72 (the shape B2 left) whose retirement step fails once is
/// left at 73 with the receiver table present and every retired object intact,
/// and the next open finishes the migration. A receiver step that wrote 72
/// would leave the store at 72 with the table already there, and every retry
/// would fail on the existing table.
#[test]
fn a_failed_retirement_after_the_receiver_step_keeps_schema_73_and_retries() {
    let (temporary, store) = cs_store();
    let connection = store.open().unwrap();
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V73_SQL} PRAGMA user_version = 72; COMMIT;"
        ))
        .unwrap();
    drop(connection);
    drop(store);

    set_retirement_fault(Some(RetirementFault::GapCopy));
    let failed = HubStore::initialize(temporary.path());
    set_retirement_fault(None);
    assert!(
        matches!(failed, Err(StoreError::Migrate(_))),
        "the retirement fault must fail the open, got {:?}",
        failed.err()
    );

    let read = rr_read_only(temporary.path());
    assert_eq!(rr_user_version(&read), 73, "the receiver step committed alone");
    assert!(rr_table_exists(&read, "fleet_telemetry_accumulator_states"));
    for table in RR_EDGE_TABLES {
        assert!(rr_table_exists(&read, table), "{table} intact after the failure");
    }
    assert!(!rr_table_exists(&read, "retired_delivery_gaps"));
    drop(read);

    let store = HubStore::initialize(temporary.path()).expect("retry succeeds");
    let connection = store.open().unwrap();
    assert_eq!(rr_user_version(&connection), 74);
    assert_eq!(rr_user_version(&connection), SCHEMA_VERSION);
    assert!(rr_table_exists(&connection, "retired_delivery_gaps"));
    for table in RR_EDGE_TABLES {
        assert!(!rr_table_exists(&connection, table), "{table} dropped after the retry");
    }
}

// ---- receiver event retention (6A S5) ----

fn rr_event(source: Uuid, vehicle: Uuid, observed_at_ms: i64, txid: &str) -> ObservationInput {
    ObservationInput {
        source_id: source,
        vehicle_id: vehicle,
        observed_at_ms,
        payload: serde_json::json!({
            "record_type": RECEIVER_EVENT_RECORD_TYPE,
            "tx_type": "alerts",
            "txid": txid,
            "envelope_received_at_ms": observed_at_ms + 5,
            "timestamp_ms": observed_at_ms,
            "payload": {"name": "synthetic"},
        }),
    }
}

fn rr_source_of(store: &HubStore, vehicle: Uuid) -> Uuid {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
            [vehicle.to_string()],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn receiver_event_rows_have_no_current_row_and_survive_the_processed_row_prune() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "rr-events");
    let source = rr_source_of(&store, vehicle);
    let mut connection = store.open().unwrap();
    let transaction = connection.transaction().unwrap();
    let event = HubStore::append_event_observation_in_transaction(
        &transaction,
        &rr_event(source, vehicle, 1_800_000_000_000, "tx-1"),
        1_800_000_000_900,
    )
    .unwrap();
    assert!(event.inserted);
    assert_eq!(event.observation.observed_at_ms, 1_800_000_000_000);
    assert_eq!(event.observation.received_at_ms, 1_800_000_000_900);
    // A processing row, as the lifecycle consumes it.
    let processed = append_observation_in_transaction(
        &transaction,
        &ObservationInput {
            source_id: source,
            vehicle_id: vehicle,
            observed_at_ms: 1_800_000_001_000,
            payload: serde_json::json!({
                "record_type": "fleet_api_vehicle_data_v1",
                "provider_raw_json": {"response": {"charge_state": {"battery_level": 55}}},
            }),
        },
        1_800_000_001_100,
    )
    .unwrap();
    // A replay of the same event inserts nothing, whatever its admission time.
    let replay = HubStore::append_event_observation_in_transaction(
        &transaction,
        &rr_event(source, vehicle, 1_800_000_000_000, "tx-1"),
        1_800_000_005_000,
    )
    .unwrap();
    assert!(!replay.inserted);
    assert_eq!(replay.observation.received_at_ms, 1_800_000_000_900);
    // Only the processing row is a current row; the event has none.
    let current: Vec<String> = transaction
        .prepare("SELECT record_type FROM current_observations WHERE vehicle_id = ?1")
        .unwrap()
        .query_map([vehicle.to_string()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(current, vec!["fleet_api_vehicle_data_v1".to_owned()]);
    // The prune after projection takes the processing row and keeps the event.
    prune_processed_observations(&transaction, vehicle, processed.observation.observation_id)
        .unwrap();
    transaction.commit().unwrap();
    let remaining: Vec<(i64, String)> = connection
        .prepare(
            "SELECT observation_id, json_extract(payload_json, '$.record_type')
               FROM raw_observations WHERE vehicle_id = ?1 ORDER BY observation_id",
        )
        .unwrap()
        .query_map([vehicle.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        remaining,
        vec![(
            event.observation.observation_id,
            RECEIVER_EVENT_RECORD_TYPE.to_owned()
        )],
        "the event outlives the prune; the processing row does not"
    );
}

#[test]
fn receiver_event_append_refuses_any_other_record_type() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "rr-events-type");
    let source = rr_source_of(&store, vehicle);
    let mut connection = store.open().unwrap();
    let transaction = connection.transaction().unwrap();
    let mut input = rr_event(source, vehicle, 1_800_000_000_000, "tx-1");
    input.payload["record_type"] = serde_json::json!("fleet_api_vehicle_data_v1");
    assert!(matches!(
        HubStore::append_event_observation_in_transaction(&transaction, &input, 1_800_000_000_900),
        Err(StoreError::InvalidEventRecord)
    ));
}

// ---- export-outbox enumeration (6A S9) ----

#[test]
fn export_outbox_pages_are_ordered_keyset_and_bounded() {
    let (_temporary, store) = cs_store();
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            CS_W0,
        )
        .unwrap();
    let total = EXPORT_OUTBOX_VEHICLE_PAGE + 44;
    let mut expected = Vec::new();
    for index in 0..total {
        let id = Uuid::from_u128(0x1000_0000_0000_4000_8000_0000_0000_0000 + index as u128);
        store
            .register_vehicle_with_id(
                &VehicleDescriptor::new(source.source_id, format!("rr-outbox-{index}")),
                CS_W0,
                id,
            )
            .unwrap();
        expected.push(id);
    }
    let connection = store.open().unwrap();
    for id in &expected {
        connection
            .execute(
                "INSERT INTO export_outbox(vehicle_id, dirty_revision) VALUES (?1, 1)",
                [id.to_string()],
            )
            .unwrap();
    }
    let before = connection
        .query_row("SELECT COUNT(*) FROM export_outbox", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    // Whatever the caller asks, a page holds at most 256 vehicles.
    let first = store.export_outbox_vehicle_page(None, 10_000).unwrap();
    assert_eq!(first.len(), EXPORT_OUTBOX_VEHICLE_PAGE);
    assert_eq!(first, expected[..EXPORT_OUTBOX_VEHICLE_PAGE]);
    let second = store
        .export_outbox_vehicle_page(first.last().copied(), 10_000)
        .unwrap();
    assert_eq!(second, expected[EXPORT_OUTBOX_VEHICLE_PAGE..]);
    assert!(
        store
            .export_outbox_vehicle_page(second.last().copied(), 256)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.export_outbox_vehicle_page(None, 3).unwrap(),
        expected[..3]
    );
    // The enumeration claims, completes and fails nothing.
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM export_outbox", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        before
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM export_outbox
                  WHERE dirty_revision <> 1 OR attempts <> 0 OR claimed_until_ms <> 0
                     OR last_error IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

// ---- read-only identity resolution (6A S5, C-6) ----

fn rr_fleet_source() -> SourceDescriptor {
    SourceDescriptor::new("fleet_api_compat", "local_installation_v1")
}

fn rr_fleet_descriptor(vin: &str) -> VehicleDescriptor {
    let mut descriptor =
        VehicleDescriptor::new(Uuid::nil(), "70").with_tesla_identity(Some(70), None);
    descriptor.vin = Some(vin.to_owned());
    descriptor
}

/// The sources, vehicles and aliases of the store, to prove a lookup wrote none.
fn rr_identity_state(connection: &Connection) -> Vec<Vec<Vec<String>>> {
    ["sources", "source_identities", "vehicles", "vehicle_identity_aliases"]
        .iter()
        .map(|table| rr_rows(connection, table))
        .collect()
}

#[test]
fn registered_vehicle_unchanged_reads_without_registering_or_writing() {
    const VIN: &str = "5YJ3E1EA7KF000001";
    let (_temporary, store) = cs_store();
    // Nothing registered yet: nothing is unchanged.
    assert_eq!(
        store
            .registered_vehicle_unchanged(&rr_fleet_source(), &rr_fleet_descriptor(VIN))
            .unwrap(),
        None
    );
    let source = store.register_source(&rr_fleet_source(), CS_W0).unwrap();
    let mut descriptor = rr_fleet_descriptor(VIN);
    descriptor.source_id = source.source_id;
    let registered = store.register_vehicle(&descriptor, CS_W0).unwrap();

    let state_before = rr_identity_state(&store.open().unwrap());
    store.instrumentation().reset();
    let sinks_before = store.sink_attempts();
    let found = store
        .registered_vehicle_unchanged(&rr_fleet_source(), &rr_fleet_descriptor(VIN))
        .unwrap()
        .expect("registered identity is unchanged");
    assert_eq!(found.vehicle_id, registered.vehicle_id);
    assert_eq!(found.source_id, source.source_id);
    assert_eq!(found.source_generation, source.generation);
    let counts = store.instrumentation().snapshot();
    assert_eq!(counts.registration_calls(), 0, "no registration call");
    assert_eq!(counts.writable_opens, 0, "one read-only connection only");
    assert_eq!(counts.read_only_opens, 1);
    assert_eq!(store.sink_attempts(), sinks_before, "no write attempted");
    assert_eq!(rr_identity_state(&store.open().unwrap()), state_before);
}

#[test]
fn registered_vehicle_unchanged_is_none_for_every_difference_registration_would_write() {
    const VIN: &str = "5YJ3E1EA7KF000001";
    let setup = || {
        let (temporary, store) = cs_store();
        let source = store.register_source(&rr_fleet_source(), CS_W0).unwrap();
        let mut descriptor = rr_fleet_descriptor(VIN);
        descriptor.source_id = source.source_id;
        let registered = store.register_vehicle(&descriptor, CS_W0).unwrap();
        (temporary, store, registered.vehicle_id)
    };
    let unchanged = |store: &HubStore, vin: &str| {
        store
            .registered_vehicle_unchanged(&rr_fleet_source(), &rr_fleet_descriptor(vin))
            .unwrap()
    };

    // A required alias is gone: source key, Tesla EID or VIN.
    for kind in ["source_key", "tesla_eid", "vin"] {
        let (_temporary, store, _) = setup();
        assert!(unchanged(&store, VIN).is_some());
        store
            .open()
            .unwrap()
            .execute(
                "DELETE FROM vehicle_identity_aliases WHERE alias_kind = ?1",
                [kind],
            )
            .unwrap();
        assert_eq!(unchanged(&store, VIN), None, "missing {kind} alias");
    }

    // The stored VIN is absent or differs.
    for stored in [None, Some("5YJ3E1EA7KF000009")] {
        let (_temporary, store, vehicle) = setup();
        store
            .open()
            .unwrap()
            .execute(
                "UPDATE vehicles SET vin = ?1 WHERE vehicle_id = ?2",
                rusqlite::params![stored, vehicle.to_string()],
            )
            .unwrap();
        assert_eq!(unchanged(&store, VIN), None, "stored VIN {stored:?}");
    }

    // A second EID alias on the vehicle: registration would delete it.
    let (_temporary, store, vehicle) = setup();
    store
        .open()
        .unwrap()
        .execute(
            "INSERT INTO vehicle_identity_aliases(alias_kind, alias_value, vehicle_id, source_id,
                source_vehicle_key)
             SELECT 'tesla_eid', '71', vehicle_id, source_id, source_vehicle_key
               FROM vehicles WHERE vehicle_id = ?1",
            [vehicle.to_string()],
        )
        .unwrap();
    assert_eq!(unchanged(&store, VIN), None, "stale EID alias");

    // An alias that belongs to another vehicle is a conflict, never unchanged.
    let (_temporary, store, vehicle) = setup();
    let other = store
        .register_vehicle(
            &VehicleDescriptor::new(
                store.register_source(&rr_fleet_source(), CS_W0).unwrap().source_id,
                "other-key",
            ),
            CS_W0,
        )
        .unwrap();
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE vehicle_identity_aliases SET vehicle_id = ?1
              WHERE alias_kind = 'vin' AND vehicle_id = ?2",
            [other.vehicle_id.to_string(), vehicle.to_string()],
        )
        .unwrap();
    assert_eq!(unchanged(&store, VIN), None, "conflicting alias");
}
