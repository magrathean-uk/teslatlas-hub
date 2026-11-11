// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

pub(super) fn run_local_readers_are_bounded() {
    case_variant_predecessors();
    post_drop_and_below_rung();
    receipt_join_and_optional_output();
    schema_and_value_refusals();
    floor_presence_64_masks();
    at_rung_presence_64_masks_for_each_drop_flag();
    sanitised_and_mapped_history();
    fence_tables_100000_each();
    bindings_total_100000();
}

fn case_variant_predecessors() {
    for all_names in [false, true] {
        let f = migration_fixture(false, true, false);
        drop_c10_for_floor(&f);
        for (index, table) in PREDECESSOR_TABLES.iter().enumerate() {
            if all_names || index == 1 {
                f.conn.test_connection().execute_batch(&format!("ALTER TABLE {table} RENAME TO synthetic_case_stage; ALTER TABLE synthetic_case_stage RENAME TO {};", table.to_ascii_uppercase())).unwrap();
            }
        }
        let before = snapshot(&f);
        let result = credential_schema_rung(f.conn.test_connection(), false);
        assert!(
            matches!(result, Err(StoreError::CredentialPairInvalid)),
            "C73-CASE-01 occupied noncanonical schema names fail closed before the rung: all_names={all_names}"
        );
        let rows: i64 = f
            .conn
            .test_connection()
            .query_row("SELECT count(*) FROM FLEET_TOKENS", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            rows, 1,
            "C73-CASE-02 refusal never drops the occupied Fleet predecessor: all_names={all_names}"
        );
        assert_eq!(
            snapshot(&f),
            before,
            "C73-CASE-03 refusal preserves every table and row: all_names={all_names}"
        );
        let version: i64 = f
            .conn
            .test_connection()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version, 78,
            "C73-CASE-04 refusal never advances the schema version: all_names={all_names}"
        );
    }
    let mut f = migration_fixture(false, true, false);
    drop_c10_for_floor(&f);
    credential_schema_rung(f.conn.test_connection(), false)
        .expect("C73-CASE-CANONICAL-OK canonical occupied predecessors remain supported");
    let state = f.conn.credential_format_state().unwrap();
    assert_eq!(
        state.fleet_api_state,
        CredentialSlotFormatState::Pending,
        "C73-CASE-05 canonical occupied Fleet stays pending until conversion"
    );
    assert!(
        state.converted_at_ms.is_none() && !state.old_tables_dropped,
        "C73-CASE-06 an occupied accepted predecessor cannot be marked converted or dropped"
    );
}

fn post_drop_and_below_rung() {
    let mut f = fixture_with_memory_temp_store();
    let before = snapshot(&f);
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        assert!(
            f.conn
                .read_pre_upgrade_refresh_bindings(slot)
                .unwrap()
                .is_empty(),
            "READER-POSTDROP-02 dropped bindings are empty without missing-table SELECT"
        );
        assert!(
            f.conn
                .read_pre_upgrade_refresh_fences(slot)
                .unwrap()
                .is_empty(),
            "READER-POSTDROP-03 dropped fences are empty without missing-table SELECT"
        );
    }
    assert_eq!(
        snapshot(&f),
        before,
        "READER-POSTDROP-04 readers never mutate a completed source"
    );
    drop_c10_for_floor(&f);
    let before = snapshot(&f);
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        assert!(
            matches!(
                f.conn.read_pre_upgrade_refresh_bindings(slot),
                Err(StoreError::UnsupportedSchema(78))
            ),
            "READER-VERSION-02 below-rung binding reader refuses before credential query"
        );
        assert!(
            matches!(
                f.conn.read_pre_upgrade_refresh_fences(slot),
                Err(StoreError::UnsupportedSchema(78))
            ),
            "READER-VERSION-03 below-rung fence reader refuses before credential query"
        );
    }
    assert_eq!(
        snapshot(&f),
        before,
        "READER-VERSION-04 version refusal has no source write"
    );
}

fn seed_receipt(db: &Connection, id: i64, slot: CredentialSlot, started: i64, terminal: bool) {
    let transport = if slot == CredentialSlot::OwnerApi {
        "legacy_auth"
    } else {
        "fleet_api"
    };
    db.execute(
        "INSERT INTO outbound_request_receipts(id,correlation_id,started_at_ms,completed_at_ms,
         duration_ms,vehicle_tesla_id,transport,operation,safety_class,precondition,outcome,
         http_status,retry_after_seconds) VALUES(?1,?2,?3,?4,?5,NULL,?6,'token_refresh',
         'non_wake_endpoint','not_required',?7,NULL,NULL)",
        params![
            id,
            generation(id as u128 + 10_000).to_string(),
            started,
            terminal.then_some(started + 1),
            terminal.then_some(1),
            transport,
            if terminal { "success" } else { "started" }
        ],
    )
    .unwrap();
}

fn seed_binding(db: &Connection, id: i64, slot: CredentialSlot, output: Option<Uuid>) {
    let table = if slot == CredentialSlot::OwnerApi {
        "legacy_refresh_receipt_bindings"
    } else {
        "fleet_refresh_receipt_bindings"
    };
    db.execute(
        &format!(
            "INSERT INTO {table}(receipt_id,attempt_id,input_credential_generation,
          output_credential_generation) VALUES(?1,?2,?3,?4)"
        ),
        params![
            id,
            generation(id as u128 + 20_000).to_string(),
            generation(101).to_string(),
            output.map(|value| value.to_string())
        ],
    )
    .unwrap();
}

fn receipt_join_and_optional_output() {
    let mut f = migration_fixture(true, true, true);
    seed_receipt(
        f.conn.test_connection(),
        10,
        CredentialSlot::OwnerApi,
        T0_MS - 10,
        false,
    );
    seed_receipt(
        f.conn.test_connection(),
        20,
        CredentialSlot::FleetApi,
        T0_MS - 20,
        true,
    );
    seed_binding(
        f.conn.test_connection(),
        10,
        CredentialSlot::OwnerApi,
        Some(generation(303)),
    );
    seed_binding(f.conn.test_connection(), 20, CredentialSlot::FleetApi, None);
    let owner = f
        .conn
        .read_pre_upgrade_refresh_bindings(CredentialSlot::OwnerApi)
        .expect("READER-BINDING-OK valid joined Owner history is readable before field assertions");
    assert_eq!(
        owner.len(),
        1,
        "READER-BINDING-01 every Owner binding is returned"
    );
    let row = &owner[0];
    assert_eq!(
        row.receipt_id.0, 10,
        "READER-BINDING-02 receipt join uses the actual receipt id"
    );
    assert_eq!(
        row.attempt_id,
        generation(20_010),
        "READER-BINDING-03 attempt does not become correlation id"
    );
    assert_eq!(
        row.input_generation,
        generation(101),
        "READER-BINDING-04 input generation preserved"
    );
    assert_eq!(
        row.output_generation,
        Some(generation(303)),
        "READER-BINDING-05 optional output preserved"
    );
    assert_eq!(
        row.started_at_ms,
        T0_MS - 10,
        "READER-BINDING-06 receipt start timestamp preserved"
    );
    assert!(
        matches!(row.receipt_state, PreUpgradeReceiptState::Started),
        "READER-BINDING-07 Started is distinct from every terminal outcome"
    );
    let owner_debug = format!("{row:?}").to_ascii_lowercase();
    assert!(
        !owner_debug.contains(&generation(101).to_string())
            && !owner_debug.contains(&generation(303).to_string()),
        "READER-CUSTODY-03 binding Debug redacts both input generation and a present output generation"
    );
    let fleet = f
        .conn
        .read_pre_upgrade_refresh_bindings(CredentialSlot::FleetApi)
        .unwrap();
    assert_eq!(
        fleet.len(),
        1,
        "READER-BINDING-08 every Fleet binding is returned"
    );
    assert!(
        matches!(
            fleet[0].receipt_state,
            PreUpgradeReceiptState::Completed(OutboundRequestOutcome::Success)
        ),
        "READER-BINDING-09 successful receipt maps to Completed(Success)"
    );
    assert!(
        fleet[0].output_generation.is_none(),
        "READER-BINDING-10 terminal success may retain absent output generation"
    );
    assert_eq!(
        fleet[0].started_at_ms,
        T0_MS - 20,
        "READER-BINDING-11 Fleet earliest-time input is preserved"
    );
    for (text, expected) in [
        ("success", OutboundRequestOutcome::Success),
        ("http_error", OutboundRequestOutcome::HttpError),
        ("timeout", OutboundRequestOutcome::Timeout),
        ("transport_error", OutboundRequestOutcome::TransportError),
        (
            "authentication_rejected",
            OutboundRequestOutcome::AuthenticationRejected,
        ),
        ("protocol_error", OutboundRequestOutcome::ProtocolError),
        (
            "response_too_large",
            OutboundRequestOutcome::ResponseTooLarge,
        ),
        ("cancelled", OutboundRequestOutcome::Cancelled),
    ] {
        f.conn
            .test_connection()
            .execute(
                "UPDATE outbound_request_receipts SET outcome=?1 WHERE id=20",
                [text],
            )
            .unwrap();
        let rows = f
            .conn
            .read_pre_upgrade_refresh_bindings(CredentialSlot::FleetApi)
            .unwrap();
        assert!(
            matches!(&rows[0].receipt_state, PreUpgradeReceiptState::Completed(actual) if *actual == expected),
            "READER-BINDING-12 every terminal outcome is preserved distinctly: {text}"
        );
        assert!(
            rows[0].output_generation.is_none(),
            "READER-BINDING-13 terminal outcomes never manufacture an absent output: {text}"
        );
    }
    let debug = format!("{:?}", fleet[0]);
    assert!(
        !debug.contains(&generation(101).to_string()),
        "READER-CUSTODY-02 binding Debug redacts generations"
    );

    let before = snapshot(&f);
    f.conn
        .test_connection()
        .execute(
            "UPDATE outbound_request_receipts SET transport='owner_api' WHERE id=10",
            [],
        )
        .unwrap();
    assert_readers_invalid(&mut f, "READER-JOIN-01 wrong slot transport");
    f.conn
        .test_connection()
        .execute(
            "UPDATE outbound_request_receipts SET transport='legacy_auth' WHERE id=10",
            [],
        )
        .unwrap();
    assert_eq!(
        snapshot(&f),
        before,
        "READER-JOIN-02 read refusal made no incidental history repair"
    );
    f.conn.test_connection().execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM outbound_request_receipts WHERE id=10; PRAGMA foreign_keys=ON;").unwrap();
    assert_readers_invalid(
        &mut f,
        "READER-JOIN-03 orphan is visible, never hidden by an inner join",
    );

    let mut f = migration_fixture(false, false, true);
    seed_receipt(
        f.conn.test_connection(),
        30,
        CredentialSlot::OwnerApi,
        T0_MS,
        false,
    );
    seed_binding(f.conn.test_connection(), 30, CredentialSlot::OwnerApi, None);
    seed_binding(f.conn.test_connection(), 30, CredentialSlot::FleetApi, None);
    assert_readers_invalid(
        &mut f,
        "READER-JOIN-04 receipt_collision_refused across predecessor stores",
    );
}

fn schema_and_value_refusals() {
    let mut f = migration_fixture(true, true, false);
    Uuid::parse_str("00000000000040008000000000000065")
        .expect("SETUP-UUID-32 the compact fixture is parseable but violates stored length 36");
    for (sql, reset, label) in [
        (
            "UPDATE legacy_owner_tokens SET refresh=X''",
            "UPDATE legacy_owner_tokens SET refresh=X'B1B2B3'",
            "empty refresh ciphertext",
        ),
        (
            "UPDATE legacy_owner_tokens SET singleton_id=2",
            "UPDATE legacy_owner_tokens SET singleton_id=1",
            "singleton must be exactly one",
        ),
        (
            "UPDATE legacy_owner_tokens SET credential_generation='00000000000040008000000000000065'",
            "UPDATE legacy_owner_tokens SET credential_generation=NULL",
            "parseable compact generation is not 36 characters",
        ),
        (
            "UPDATE legacy_owner_tokens SET access=X''",
            "UPDATE legacy_owner_tokens SET access=X'0102'",
            "empty ciphertext",
        ),
        (
            "UPDATE legacy_owner_tokens SET expires_at=1,next_refresh_at=1",
            "UPDATE legacy_owner_tokens SET expires_at=0,next_refresh_at=0",
            "invalid Owner schedule",
        ),
        (
            "UPDATE legacy_owner_tokens SET credential_generation='xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx'",
            "UPDATE legacy_owner_tokens SET credential_generation=NULL",
            "unparseable generation",
        ),
        (
            "UPDATE fleet_tokens SET expires_at=600,next_refresh_at=600",
            "UPDATE fleet_tokens SET expires_at=900,next_refresh_at=600",
            "invalid Fleet schedule",
        ),
        (
            "UPDATE fleet_tokens SET region='invalid'",
            "UPDATE fleet_tokens SET region='eu'",
            "invalid Fleet region",
        ),
        (
            "UPDATE fleet_tokens SET client_id=''",
            "UPDATE fleet_tokens SET client_id='synthetic-client'",
            "empty Fleet application identifier",
        ),
    ] {
        f.conn.test_connection().execute_batch(sql).unwrap();
        let before = snapshot(&f);
        assert_readers_invalid(&mut f, label);
        assert_eq!(
            snapshot(&f),
            before,
            "READER-VALUE-01 malformed per-value refusal is read-only: {label}"
        );
        f.conn.test_connection().execute_batch(reset).unwrap();
    }
    f.conn
        .test_connection()
        .execute("UPDATE fleet_tokens SET client_id=?1", ["é".repeat(128)])
        .unwrap();
    assert_readers_invalid(
        &mut f,
        "READER-VALUE-02 client bound counts 256 UTF-8 bytes, not 128 characters",
    );

    for (ddl, label) in [
        (
            "CREATE TABLE legacy_owner_tokens(singleton_id INTEGER PRIMARY KEY,access BLOB,refresh BLOB NOT NULL,expires_at INTEGER NOT NULL,next_refresh_at INTEGER NOT NULL,credential_generation TEXT) STRICT",
            "nullable ciphertext metadata",
        ),
        (
            "CREATE TABLE legacy_owner_tokens(singleton_id INTEGER PRIMARY KEY,access BLOB NOT NULL,refresh BLOB NOT NULL,expires_at TEXT NOT NULL,next_refresh_at INTEGER NOT NULL,credential_generation TEXT) STRICT",
            "wrong declared type",
        ),
        (
            "CREATE TABLE legacy_owner_tokens(singleton_id INTEGER PRIMARY KEY,access BLOB NOT NULL,refresh BLOB NOT NULL,expires_at INTEGER NOT NULL,next_refresh_at INTEGER NOT NULL,credential_generation TEXT,extra INTEGER) STRICT",
            "unexpected column",
        ),
        (
            "CREATE TABLE legacy_owner_tokens(singleton_id INTEGER PRIMARY KEY,access BLOB NOT NULL,refresh BLOB NOT NULL,expires_at INTEGER NOT NULL,next_refresh_at INTEGER NOT NULL,credential_generation TEXT)",
            "non-STRICT table",
        ),
        (
            "CREATE VIEW legacy_owner_tokens AS SELECT 1 AS singleton_id",
            "wrong-kind view",
        ),
    ] {
        let mut f = migration_fixture(false, false, false);
        f.conn
            .test_connection()
            .execute_batch("DROP TABLE legacy_owner_tokens")
            .unwrap();
        f.conn.test_connection().execute_batch(ddl).unwrap();
        let before = snapshot(&f);
        assert_readers_invalid(&mut f, label);
        assert_eq!(
            snapshot(&f),
            before,
            "READER-SHAPE-01 invalid metadata is never repaired: {label}"
        );
    }

    let mut f = fixture_with_memory_temp_store();
    create_predecessors(f.conn.test_connection(), 63, true);
    f.conn
        .test_connection()
        .execute_batch(
            "UPDATE credential_format_state SET owner_api_state='none',fleet_api_state='none',
         fences_pending=0,old_tables_dropped=0,purge_required=1,wal_scrubbed=0,
         old_files_removed=0,converted_at_ms=NULL,completed_at_ms=NULL",
        )
        .unwrap();
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        assert!(
            f.conn
                .read_pre_upgrade_refresh_bindings(slot)
                .unwrap()
                .is_empty(),
            "READER-SHAPE-02 equivalent DDL and implicit INTEGER PRIMARY KEY nonnull are admitted"
        );
    }
    f.conn
        .test_connection()
        .execute("DELETE FROM credential_format_state", [])
        .unwrap();
    assert_readers_invalid(
        &mut f,
        "READER-STATE-01 missing at-rung singleton is not empty input",
    );
    let mut f = migration_fixture(true, false, false);
    f.conn
        .test_connection()
        .execute("DELETE FROM legacy_owner_tokens", [])
        .unwrap();
    assert_readers_invalid(
        &mut f,
        "READER-STATE-02 Pending requires its actual predecessor row",
    );
    let mut f = migration_fixture(false, false, false);
    seed_predecessor_row(&f, CredentialSlot::OwnerApi, generation(101));
    assert_readers_invalid(
        &mut f,
        "READER-STATE-03 resolved slot cannot conceal residual token row",
    );

    for (change, label) in [
        ("format_version=2", "unsupported format version"),
        (
            "owner_api_blocked='old_key_unavailable'",
            "block reason on resolved slot",
        ),
        ("converted_at_ms=1", "conversion marker before table drop"),
        (
            "completed_at_ms=1",
            "completion marker before conversion/cleanup",
        ),
    ] {
        let mut f = migration_fixture(false, false, false);
        f.conn
            .test_connection()
            .execute_batch(&format!(
                "PRAGMA ignore_check_constraints=ON; UPDATE credential_format_state SET {change};
             PRAGMA ignore_check_constraints=OFF;"
            ))
            .unwrap();
        assert_readers_invalid(&mut f, label);
    }
}

fn floor_presence_64_masks() {
    for occupied in [false, true] {
        for mask in 0u8..64 {
            let temporary = crate::private_tempdir().unwrap();
            let db = Connection::open(temporary.path().join("floor.sqlite")).unwrap();
            configure_test_memory_temp_store(&db);
            db.execute_batch(
                "PRAGMA user_version=78; PRAGMA foreign_keys=ON; BEGIN IMMEDIATE;
                 CREATE TABLE outbound_request_receipts(
                  id INTEGER PRIMARY KEY AUTOINCREMENT,correlation_id TEXT NOT NULL,
                  started_at_ms INTEGER NOT NULL,completed_at_ms INTEGER,duration_ms INTEGER,
                  vehicle_tesla_id INTEGER,transport TEXT NOT NULL,operation TEXT NOT NULL,
                  safety_class TEXT NOT NULL,precondition TEXT NOT NULL,outcome TEXT NOT NULL,
                  http_status INTEGER,retry_after_seconds INTEGER) STRICT;",
            )
            .unwrap();
            create_predecessors(&db, mask, false);
            if occupied {
                if mask & 1 != 0 {
                    db.execute_batch(
                        "INSERT INTO legacy_owner_tokens VALUES(1,X'01',X'02',0,0,NULL)",
                    )
                    .unwrap();
                }
                if mask & 2 != 0 {
                    db.execute_batch(
                        "INSERT INTO fleet_tokens VALUES(1,X'01',X'02','client','eu',900,600,NULL)",
                    )
                    .unwrap();
                }
                for index in [3, 5] {
                    if mask & (1 << index) != 0 {
                        db.execute(
                            &format!("INSERT INTO {} VALUES(?1)", PREDECESSOR_TABLES[index]),
                            [generation(101).to_string()],
                        )
                        .unwrap();
                    }
                }
                for (index, slot) in [(2, CredentialSlot::OwnerApi), (4, CredentialSlot::FleetApi)]
                {
                    if mask & (1 << index) != 0 {
                        let id = index as i64 + 10;
                        seed_receipt(&db, id, slot, T0_MS, false);
                        seed_binding(&db, id, slot, None);
                    }
                }
            }
            db.execute_batch("COMMIT").unwrap();
            let before: i64 = db
                .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
                .unwrap();
            let result = credential_schema_rung(&db, false);
            if mask == 0 || mask == 63 {
                assert!(
                    result.is_ok(),
                    "PRESENCE-FLOOR-01 all-six or all-absent floor admitted: mask={mask} occupied={occupied}"
                );
                let version: i64 = db
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(
                    version, 79,
                    "PRESENCE-FLOOR-02 admitted real rung reaches version79"
                );
                let state: (bool, bool, bool, Option<i64>) = db.query_row(
                    "SELECT purge_required,wal_scrubbed,old_files_removed,completed_at_ms FROM credential_format_state",
                    [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
                ).unwrap();
                assert_eq!(
                    state,
                    (true, false, false, None),
                    "PRESENCE-FLOOR-03 floor/interrupted-fresh version alone gets no fresh cleanup shortcut"
                );
            } else {
                assert!(
                    matches!(result, Err(StoreError::CredentialPairInvalid)),
                    "PRESENCE-FLOOR-04 every proper subset refused before C10 DDL: mask={mask} occupied={occupied}"
                );
                let after: i64 = db
                    .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(
                    after, before,
                    "PRESENCE-FLOOR-05 refused rung creates/drops no schema object"
                );
                let version: i64 = db
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(
                    version, 78,
                    "PRESENCE-FLOOR-06 refused rung leaves floor version intact"
                );
            }
        }
    }
}

fn at_rung_presence_64_masks_for_each_drop_flag() {
    for dropped in [false, true] {
        for occupied in [false, true] {
            for mask in 0u8..64 {
                let mut f = fixture_with_memory_temp_store();
                f.conn
                    .test_connection()
                    .execute_batch("BEGIN IMMEDIATE")
                    .unwrap();
                create_predecessors(f.conn.test_connection(), mask, false);
                if !dropped {
                    f.conn.test_connection().execute(
                        "UPDATE credential_format_state SET owner_api_state=?1,fleet_api_state=?1,
                         fences_pending=?2,old_tables_dropped=0,purge_required=1,wal_scrubbed=0,
                         old_files_removed=0,converted_at_ms=NULL,completed_at_ms=NULL",
                        params![if occupied { "pending" } else { "none" },occupied],
                    ).unwrap();
                }
                if occupied {
                    if mask & 1 != 0 {
                        seed_predecessor_row(&f, CredentialSlot::OwnerApi, generation(101));
                    }
                    if mask & 2 != 0 {
                        seed_predecessor_row(&f, CredentialSlot::FleetApi, generation(202));
                    }
                    for index in [3, 5] {
                        if mask & (1 << index) != 0 {
                            f.conn
                                .test_connection()
                                .execute(
                                    &format!(
                                        "INSERT INTO {} VALUES(?1)",
                                        PREDECESSOR_TABLES[index]
                                    ),
                                    [generation(101).to_string()],
                                )
                                .unwrap();
                        }
                    }
                    for (index, slot) in
                        [(2, CredentialSlot::OwnerApi), (4, CredentialSlot::FleetApi)]
                    {
                        if mask & (1 << index) != 0 {
                            let id = index as i64 + 10;
                            seed_receipt(f.conn.test_connection(), id, slot, T0_MS, false);
                            seed_binding(f.conn.test_connection(), id, slot, None);
                        }
                    }
                }
                f.conn.test_connection().execute_batch("COMMIT").unwrap();
                let before = snapshot(&f);
                let admitted = (dropped && mask == 0) || (!dropped && mask == 63);
                for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
                    if admitted {
                        assert_eq!(
                            f.conn
                                .read_pre_upgrade_refresh_bindings(slot)
                                .unwrap()
                                .len(),
                            usize::from(!dropped && occupied),
                            "PRESENCE-AT-02 valid binding table is read completely, including occupied matrix cases"
                        );
                        let fences = f.conn.read_pre_upgrade_refresh_fences(slot).unwrap();
                        assert_eq!(
                            fences.len(),
                            usize::from(!dropped && occupied),
                            "PRESENCE-AT-03 post-drop empty never queries missing tables"
                        );
                    } else {
                        assert!(
                            matches!(
                                f.conn.read_pre_upgrade_refresh_bindings(slot),
                                Err(StoreError::CredentialPairInvalid)
                            ),
                            "PRESENCE-AT-05 inconsistent full matrix rejects binding reader"
                        );
                        assert!(
                            matches!(
                                f.conn.read_pre_upgrade_refresh_fences(slot),
                                Err(StoreError::CredentialPairInvalid)
                            ),
                            "PRESENCE-AT-06 inconsistent full matrix rejects fence reader"
                        );
                    }
                }
                assert_eq!(
                    snapshot(&f),
                    before,
                    "PRESENCE-AT-07 every matrix read/refusal leaves source untouched"
                );
            }
        }
    }
}

fn sanitised_and_mapped_history() {
    let mut f = migration_fixture(false, false, true);
    f.conn.test_connection().execute_batch(
        "UPDATE credential_format_state SET owner_api_state='discarded',fleet_api_state='discarded';",
    ).unwrap();
    f.conn
        .test_connection()
        .execute(
            "INSERT INTO legacy_refresh_input_fences VALUES(?1)",
            [generation(505).to_string()],
        )
        .unwrap();
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        assert!(
            f.conn
                .read_pre_upgrade_refresh_bindings(slot)
                .unwrap()
                .is_empty(),
            "READER-SANITISED-01 R53 discarded/no-row state is admitted before final drop"
        );
    }
    assert_eq!(
        f.conn
            .read_pre_upgrade_refresh_fences(CredentialSlot::OwnerApi)
            .unwrap(),
        [generation(505)],
        "READER-SANITISED-02 standalone history survives absent token/binding rows"
    );
    f.conn
        .test_connection()
        .execute("UPDATE credential_format_state SET fences_pending=0", [])
        .unwrap();
    assert_eq!(
        f.conn
            .read_pre_upgrade_refresh_fences(CredentialSlot::OwnerApi)
            .unwrap(),
        [generation(505)],
        "READER-SANITISED-03 mapped-but-retained history is not silently hidden"
    );
}

fn fence_tables_100000_each() {
    let mut f = migration_fixture(false, false, true);
    for (table, offset) in [
        ("legacy_refresh_input_fences", 0),
        ("fleet_refresh_input_fences", 100_000),
    ] {
        f.conn
            .test_connection()
            .execute_batch(&format!(
                "WITH RECURSIVE n(v) AS (VALUES(1) UNION ALL SELECT v+1 FROM n WHERE v<100000)
             INSERT INTO {table} SELECT printf('00000000-0000-4000-8000-%012x',v+{offset}) FROM n;"
            ))
            .unwrap();
    }
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        assert!(
            matches!(f.conn.read_pre_upgrade_refresh_bindings(slot), Ok(ref rows) if rows.is_empty()),
            "READER-CAPACITY-10 every binding reader admits source union above the destination cap"
        );
    }
    let owner = f
        .conn
        .read_pre_upgrade_refresh_fences(CredentialSlot::OwnerApi)
        .unwrap();
    let fleet = f
        .conn
        .read_pre_upgrade_refresh_fences(CredentialSlot::FleetApi)
        .unwrap();
    assert_eq!(
        (owner.len(), fleet.len()),
        (100_000, 100_000),
        "READER-CAPACITY-01 fence_tables_100000_each returns full independent 200000 source union"
    );
    assert!(
        owner.contains(&generation(100_000)) && fleet.contains(&generation(200_000)),
        "READER-CAPACITY-02 history_over_10000_complete includes each last source generation"
    );
    f.conn
        .test_connection()
        .execute(
            "INSERT INTO legacy_refresh_input_fences VALUES(?1)",
            [generation(200_001).to_string()],
        )
        .unwrap();
    let before = snapshot(&f);
    credential_pre_upgrade_reset_decode_visits();
    assert_readers_invalid(
        &mut f,
        "READER-CAPACITY-03 source_100001_refused with no truncated vector",
    );
    assert_eq!(
        credential_pre_upgrade_decode_visits(),
        0,
        "READER-COUNT-FIRST-01 all readers reject oversized source before any history vector decode"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "READER-CAPACITY-04 over-budget source refusal never prunes standalone history"
    );
}

fn bindings_total_100000() {
    let mut f = migration_fixture(false, false, true);
    f.conn
        .test_connection()
        .execute_batch(&format!(
            "WITH RECURSIVE n(v) AS (VALUES(1) UNION ALL SELECT v+1 FROM n WHERE v<100000)
         INSERT INTO outbound_request_receipts(id,correlation_id,started_at_ms,completed_at_ms,
         duration_ms,vehicle_tesla_id,transport,operation,safety_class,precondition,outcome,
         http_status,retry_after_seconds)
         SELECT v,printf('10000000-0000-4000-8000-%012x',v),{T0_MS}+v,NULL,NULL,NULL,
          CASE WHEN v<=50000 THEN 'legacy_auth' ELSE 'fleet_api' END,'token_refresh',
          'non_wake_endpoint','not_required','started',NULL,NULL FROM n;
         INSERT INTO legacy_refresh_receipt_bindings SELECT id,
          printf('20000000-0000-4000-8000-%012x',id),
          '00000000-0000-4000-8000-000000000065',NULL
          FROM outbound_request_receipts WHERE id<=50000;
         INSERT INTO fleet_refresh_receipt_bindings SELECT id,
          printf('20000000-0000-4000-8000-%012x',id),
          '00000000-0000-4000-8000-000000000065',NULL
          FROM outbound_request_receipts WHERE id>50000;"
        ))
        .unwrap();
    let owner = f
        .conn
        .read_pre_upgrade_refresh_bindings(CredentialSlot::OwnerApi)
        .unwrap();
    let fleet = f
        .conn
        .read_pre_upgrade_refresh_bindings(CredentialSlot::FleetApi)
        .unwrap();
    assert_eq!(
        (owner.len(), fleet.len()),
        (50_000, 50_000),
        "READER-CAPACITY-05 bindings_total_100000 returns both complete histories"
    );
    assert!(
        owner
            .iter()
            .any(|row| row.receipt_id.0 == 50_000 && row.started_at_ms == T0_MS + 50_000),
        "READER-CAPACITY-06 Owner history beyond ordinary query limit retains exact receipt start"
    );
    assert!(
        fleet
            .iter()
            .any(|row| row.receipt_id.0 == 100_000 && row.started_at_ms == T0_MS + 100_000),
        "READER-CAPACITY-07 Fleet history beyond ordinary query limit retains last row"
    );
    seed_receipt(
        f.conn.test_connection(),
        100_001,
        CredentialSlot::OwnerApi,
        T0_MS,
        false,
    );
    seed_binding(
        f.conn.test_connection(),
        100_001,
        CredentialSlot::OwnerApi,
        None,
    );
    credential_pre_upgrade_reset_decode_visits();
    assert_readers_invalid(
        &mut f,
        "READER-CAPACITY-08 bindings_total_100001_refused atomically",
    );
    assert_eq!(
        credential_pre_upgrade_decode_visits(),
        0,
        "READER-COUNT-FIRST-02 combined binding budget is checked before allocating a history vector"
    );
}
