// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T11: schema v80 adds the import seam's tables, the Hub id range and the
//! guard that refuses a store holding rows the range would misread.

use super::*;
use crate::data_recovery::{DataRecoveryError, create_data_backup, restore_data_backup, verify_data_backup};
use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX, HubIdError, is_hub_id, next_hub_id};
use crate::test_assertions::LabelledResult;

const V79: i32 = 79;
const VEHICLE: &str = "20000000-0000-4000-8000-0000000000a1";
const SEAM_TABLES: [&str; 2] = ["collection_coverage", "import_seam_links"];

fn seam_columns(connection: &Connection, table: &str) -> Vec<(String, String, bool, i64)> {
    let mut statement = connection
        .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY cid")
        .unwrap();
    statement
        .query_map([table], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn seam_table_present(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )
        .unwrap()
}

/// Turn a head catalogue back into the v79 image it was migrated from: v80
/// adds only the two tables and the version.
fn seam_downgrade_to_v79(connection: &Connection) {
    connection
        .execute_batch(
            "DROP TABLE import_seam_links;
             DROP TABLE collection_coverage;
             PRAGMA user_version = 79;",
        )
        .unwrap();
}

/// A raw v79 catalogue, made by the real creation path and downgraded, with
/// foreign keys off so catalogue rows need no vehicle or import head.
fn seam_v79_catalogue() -> (tempfile::TempDir, Connection) {
    let directory = crate::private_tempdir().unwrap();
    let connection = Connection::open(directory.path().join("hub.sqlite")).unwrap();
    migrate(&connection).unwrap();
    seam_downgrade_to_v79(&connection);
    connection.pragma_update(None, "foreign_keys", false).unwrap();
    assert_eq!(schema_version(&connection).unwrap(), V79);
    (directory, connection)
}

fn seam_materialise(connection: &Connection, table: &str, id: i64) {
    let sql = match table {
        "materialised_drives" => {
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json) VALUES (?1, ?2, 1, '{}')"
        }
        "materialised_charges" => {
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json) VALUES (?1, ?2, 1, '{}')"
        }
        "materialised_charge_samples" => {
            "INSERT INTO materialised_charge_samples(vehicle_id, sample_id, charge_id, sample_json) VALUES (?1, ?2, 1, '{}')"
        }
        "materialised_positions" => {
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json) VALUES (?1, ?2, NULL, 1, '{}')"
        }
        "materialised_states" => {
            "INSERT INTO materialised_states(vehicle_id, state_id, car_id, state_json) VALUES (?1, ?2, 1, '{}')"
        }
        "materialised_updates" => {
            "INSERT INTO materialised_updates(vehicle_id, update_id, car_id, update_json) VALUES (?1, ?2, 1, '{}')"
        }
        other => panic!("unknown materialised table {other}"),
    };
    connection.execute(sql, params![VEHICLE, id]).unwrap();
}

/// Record a row in the legacy import catalogue.
fn seam_catalogue_legacy(connection: &Connection, entity: &str, id: i64) {
    connection
        .execute(
            "INSERT INTO import_projection_rows(vehicle_id, entity, entity_id) VALUES (?1, ?2, ?3)",
            params![VEHICLE, entity, id],
        )
        .unwrap();
}

/// Record a row in the current import catalogue.
fn seam_catalogue_current(connection: &Connection, entity: &str, ordinal: i64, id: i64) {
    // The head catalogue keeps the v81 shape (M08-T30): the ordinal alone
    // names the entity.
    assert_eq!(
        entity.parse::<SourceDbProjectionStateEntity>().unwrap().ordinal(),
        u8::try_from(ordinal).unwrap()
    );
    connection
        .execute(
            "INSERT INTO import_projection_state_rows(vehicle_id, entity_ordinal,
                 entity_id, car_id, projection_sha256)
             VALUES (?1, ?2, ?3, 1, zeroblob(32))",
            params![VEHICLE, ordinal, id],
        )
        .unwrap();
}

/// Imported rows of every seam table, recorded in one catalogue or the other,
/// up to the last id below the Hub range.
fn seam_seed_imported_rows(connection: &Connection) {
    seam_materialise(connection, "materialised_drives", 1);
    seam_catalogue_legacy(connection, "drive", 1);
    seam_materialise(connection, "materialised_drives", HUB_ID_BASE - 1);
    seam_catalogue_current(connection, "drive", 1, HUB_ID_BASE - 1);
    seam_materialise(connection, "materialised_charges", 2);
    seam_catalogue_current(connection, "charge", 3, 2);
    seam_materialise(connection, "materialised_charge_samples", 3);
    seam_catalogue_legacy(connection, "charge_sample", 3);
    seam_materialise(connection, "materialised_positions", 4);
    seam_catalogue_current(connection, "position", 2, 4);
    seam_materialise(connection, "materialised_states", 5);
    seam_catalogue_legacy(connection, "state", 5);
    seam_materialise(connection, "materialised_updates", 6);
    seam_catalogue_current(connection, "update", 6, 6);
}

fn seam_refusal(error: StoreError) -> (&'static str, u64) {
    match error {
        StoreError::PreSeamCollectedRows { table, count } => (table, count),
        other => panic!("expected the v80 seam refusal, found {other:?}"),
    }
}

/// A registered store downgraded to v79 and closed; the caller reopens it.
fn seam_v79_store(seed: impl FnOnce(&Connection, &str)) -> tempfile::TempDir {
    let temporary = crate::private_tempdir().unwrap();
    {
        let store = HubStore::initialize(temporary.path()).unwrap();
        let source = store
            .register_source(&SourceDescriptor::new("owner_api", "import-seam-v80"), 1_000)
            .unwrap();
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "80").with_tesla_identity(Some(80), None),
                1_000,
            )
            .unwrap();
        let connection = store.open().unwrap();
        seed(&connection, &vehicle.vehicle_id.to_string());
        seam_downgrade_to_v79(&connection);
    }
    temporary
}

fn seam_raw(directory: &Path) -> Connection {
    Connection::open(directory.join("hub.sqlite")).unwrap()
}

#[test]
fn import_seam_hub_id_range_bounds() {
    assert_eq!(HUB_ID_BASE, 1 << 30);
    assert_eq!(HUB_ID_MAX, i64::from(i32::MAX));
    assert!(!is_hub_id(HUB_ID_BASE - 1) && is_hub_id(HUB_ID_BASE) && is_hub_id(HUB_ID_MAX));
    assert!(!is_hub_id(HUB_ID_MAX + 1) && !is_hub_id((1 << 50) + 1) && !is_hub_id(0));
    assert_eq!(next_hub_id(0), Ok(HUB_ID_BASE), "a fresh cursor starts at the base");
    assert_eq!(next_hub_id(-5), Ok(HUB_ID_BASE));
    assert_eq!(next_hub_id(41_000), Ok(HUB_ID_BASE), "an imported cursor is raised");
    assert_eq!(next_hub_id(HUB_ID_BASE + 9), Ok(HUB_ID_BASE + 9));
    assert_eq!(next_hub_id(HUB_ID_MAX), Ok(HUB_ID_MAX), "the last id is handed out");
    assert_eq!(
        next_hub_id(HUB_ID_MAX + 1),
        Err(HubIdError::Exhausted { next: HUB_ID_MAX + 1 })
    );
    assert!(next_hub_id(i64::MAX).is_err());
}

#[test]
fn import_seam_schema_v80_creates_both_tables() {
    let test = "import_seam_schema_v80_creates_both_tables";
    assert_eq!(SEAM_SCHEMA_VERSION, 80);
    let expected_links = vec![
        ("vehicle_id".to_owned(), "TEXT".to_owned(), true, 1),
        ("entity".to_owned(), "TEXT".to_owned(), true, 2),
        ("loser_id".to_owned(), "INTEGER".to_owned(), true, 3),
        ("winner_id".to_owned(), "INTEGER".to_owned(), true, 0),
        ("decided_seq".to_owned(), "INTEGER".to_owned(), true, 0),
    ];
    let expected_coverage = vec![
        ("seq".to_owned(), "INTEGER".to_owned(), false, 1),
        ("grant_digest".to_owned(), "TEXT".to_owned(), true, 0),
        ("opened_at_ms".to_owned(), "INTEGER".to_owned(), true, 0),
        ("closed_at_ms".to_owned(), "INTEGER".to_owned(), false, 0),
        ("gaps_json".to_owned(), "TEXT".to_owned(), true, 0),
        ("vehicle_ends_json".to_owned(), "TEXT".to_owned(), false, 0),
        ("sealed".to_owned(), "TEXT".to_owned(), true, 0),
    ];
    let strict = |connection: &Connection, table: &str| -> bool {
        connection
            .query_row(
                "SELECT strict FROM pragma_table_list WHERE name = ?1",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    };
    let check = |connection: &Connection, label: &str| {
        assert_eq!(schema_version(connection).unwrap(), SCHEMA_VERSION, "{test}: {label} reaches v80");
        assert_eq!(seam_columns(connection, "import_seam_links"), expected_links, "{test}: {label} links");
        assert_eq!(
            seam_columns(connection, "collection_coverage"),
            expected_coverage,
            "{test}: {label} coverage"
        );
        assert!(SEAM_TABLES.iter().all(|table| strict(connection, table)), "{test}: {label} STRICT");
    };

    // A new store is created through every rung.
    let (_temporary, store) = try_test_store().unwrap();
    check(&store.open().unwrap(), "a new store");

    // A populated v79 store keeps every imported row, up to the last id below
    // the range, and gains both empty tables.
    let (_directory, connection) = seam_v79_catalogue();
    seam_seed_imported_rows(&connection);
    let (_, _, before) = super::g2_4_m1::schema_snapshot(&connection);
    migrate(&connection).labelled_ok(test, "a v79 store with imported rows migrates");
    check(&connection, "an upgraded v79 store");
    let (_, _, after) = super::g2_4_m1::schema_snapshot(&connection);
    assert!(
        before.iter().all(|(table, rows)| after.get(table) == Some(rows)),
        "{test}: every v79 row is unchanged"
    );
    let added: Vec<&String> = after.keys().filter(|table| !before.contains_key(*table)).collect();
    assert_eq!(added, SEAM_TABLES.iter().collect::<Vec<_>>(), "{test}: only the two tables are added");
    assert!(SEAM_TABLES.iter().all(|table| after[*table].is_empty()), "{test}: both start empty");
    let links_check = connection.execute(
        "INSERT INTO import_seam_links(vehicle_id, entity, loser_id, winner_id, decided_seq)
         VALUES (?1, 'car', 1, 2, 1)",
        [VEHICLE],
    );
    assert!(links_check.is_err(), "{test}: links name only the six seam entities");
    connection
        .execute(
            "INSERT INTO import_seam_links(vehicle_id, entity, loser_id, winner_id, decided_seq)
             VALUES (?1, 'drive', 7, ?2, 1)",
            params![VEHICLE, HUB_ID_BASE],
        )
        .labelled_ok(test, "a seam link is stored");
    connection
        .execute(
            "INSERT INTO collection_coverage(grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
             VALUES ('digest', 10, 20, '[]', 'handback')",
            [],
        )
        .labelled_ok(test, "a coverage row is stored");
}

#[test]
fn import_seam_schema_v80_refuses_translated_rows() {
    let test = "import_seam_schema_v80_refuses_translated_rows";
    // Rows an earlier build moved to 2^50 + id are refused even when the
    // catalogue lists them: the id alone is past the base.
    for (table, entity, ordinal, id) in [
        ("materialised_drives", "drive", 1, (1_i64 << 50) + 7),
        ("materialised_positions", "position", 2, (1_i64 << 50) + 1),
        ("materialised_charges", "charge", 3, HUB_ID_BASE),
        ("materialised_updates", "update", 6, HUB_ID_MAX),
    ] {
        let (_directory, connection) = seam_v79_catalogue();
        seam_seed_imported_rows(&connection);
        seam_materialise(&connection, table, id);
        seam_catalogue_legacy(&connection, entity, id);
        seam_catalogue_current(&connection, entity, ordinal, id);
        let before = super::g2_4_m1::schema_snapshot(&connection);
        for attempt in 0..2 {
            let error = migrate(&connection).labelled_err(test, "a translated row is refused");
            assert_eq!(seam_refusal(error), (table, 1), "{test}: {table} attempt {attempt}");
            assert_eq!(
                super::g2_4_m1::schema_snapshot(&connection),
                before,
                "{test}: {table}: the refused store keeps its version, schema and rows"
            );
        }
        assert!(SEAM_TABLES.iter().all(|seam| !seam_table_present(&connection, seam)));
    }
}

#[test]
fn import_seam_schema_v80_refuses_pre_seam_hub_rows_unchanged() {
    let test = "import_seam_schema_v80_refuses_pre_seam_hub_rows_unchanged";
    // A v79 store whose rows the Hub collected before the seam (in neither
    // catalogue) is refused on the ordinary writable open, every time.
    let temporary = seam_v79_store(|connection, vehicle| {
        for id in [11, 12] {
            connection
                .execute(
                    "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
                     VALUES (?1, ?2, 80, '{}')",
                    params![vehicle, id],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO materialised_states(vehicle_id, state_id, car_id, state_json)
                 VALUES (?1, 3, 80, '{}')",
                [vehicle],
            )
            .unwrap();
    });
    let before = super::g2_4_m1::schema_snapshot(&seam_raw(temporary.path()));
    assert_eq!(before.0, V79);
    for attempt in 0..2 {
        let error = HubStore::initialize(temporary.path())
            .map(|_| ())
            .labelled_err(test, "pre-seam Hub rows are refused");
        assert!(error.to_string().contains("materialised_charges"), "{test}: the message names the table");
        assert_eq!(seam_refusal(error), ("materialised_charges", 2), "{test}: attempt {attempt}");
        assert_eq!(
            super::g2_4_m1::schema_snapshot(&seam_raw(temporary.path())),
            before,
            "{test}: the refused store is unchanged"
        );
    }
    // The refusal is restartable: once the rows are gone the same store opens.
    seam_raw(temporary.path())
        .execute_batch("DELETE FROM materialised_charges; DELETE FROM materialised_states;")
        .unwrap();
    let store = HubStore::initialize(temporary.path()).labelled_ok(test, "a cleared store opens");
    let connection = store.open().unwrap();
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    assert!(SEAM_TABLES.iter().all(|seam| seam_table_present(&connection, seam)));
    drop(store);

    // A floor (v78) store holding Hub rows is refused before the credential
    // rung, so not even that rung's change is made.
    let floor = super::g2_4_m1::copied_store(include_bytes!(
        "../../../../fixtures/g2-4/m1/store-f.sqlite.zst"
    ));
    let floor_before = super::g2_4_m1::schema_snapshot(&seam_raw(floor.path()));
    assert_eq!(floor_before.0, MIN_STORE_VERSION);
    let error = HubStore::initialize(floor.path())
        .map(|_| ())
        .labelled_err(test, "a floor store with Hub rows is refused");
    assert_eq!(seam_refusal(error), ("materialised_charges", 1));
    assert_eq!(
        super::g2_4_m1::schema_snapshot(&seam_raw(floor.path())),
        floor_before,
        "{test}: the refused floor store keeps version 78 and every row"
    );
}

/// Re-declare the backup's catalogue as `schema` over its bytes as they are
/// now: the manifest's schema and catalogue member, then the completion
/// marker's manifest digest. Both files keep their canonical field order.
fn seam_reseal_backup(backup: &Path, from_schema: i32, schema: i32, old_digest: &str, old_size: u64) {
    let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let catalogue = backup.join("data/hub.sqlite");
    let bytes = fs::read(&catalogue).unwrap();
    let new_digest = hex(&Sha256::digest(&bytes));
    let manifest_path = backup.join("backup-v4.json");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let old_manifest_digest = hex(&Sha256::digest(manifest.as_bytes()));
    let from = format!("\"hubSchema\":{from_schema}");
    assert_eq!(manifest.matches(&from).count(), 1);
    let manifest = manifest.replacen(&from, &format!("\"hubSchema\":{schema}"), 1);
    let start = manifest.find("\"path\":\"data/hub.sqlite\"").unwrap();
    let open = manifest[..start].rfind('{').unwrap();
    let close = start + manifest[start..].find('}').unwrap();
    let member = &manifest[open..=close];
    let old_size = format!("\"size\":{old_size}");
    let old_sha = format!("\"sha256\":\"{old_digest}\"");
    assert!(member.contains(&old_size) && member.contains(&old_sha), "catalogue member: {member}");
    let member = member
        .replacen(&old_size, &format!("\"size\":{}", bytes.len()), 1)
        .replacen(&old_sha, &format!("\"sha256\":\"{new_digest}\""), 1);
    let manifest = format!("{}{}{}", &manifest[..open], member, &manifest[close + 1..]);
    fs::write(&manifest_path, &manifest).unwrap();
    let marker_path = backup.join("BACKUP_COMPLETE");
    let marker = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(marker.matches(&old_manifest_digest).count(), 1);
    let marker = marker.replacen(&old_manifest_digest, &hex(&Sha256::digest(manifest.as_bytes())), 1);
    fs::write(&marker_path, marker).unwrap();
}

/// Turn a v80 backup generation into the v79 generation an earlier build
/// would have written, optionally adding rows to its catalogue first.
fn seam_backup_as_v79(backup: &Path, extra: &str) {
    let catalogue = backup.join("data/hub.sqlite");
    let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let old_bytes = fs::read(&catalogue).unwrap();
    let old_digest = hex(&Sha256::digest(&old_bytes));
    {
        let connection = Connection::open(&catalogue).unwrap();
        connection.execute_batch(extra).unwrap();
        seam_downgrade_to_v79(&connection);
    }
    for sidecar in ["data/hub.sqlite-wal", "data/hub.sqlite-shm", "data/hub.sqlite-journal"] {
        assert!(!backup.join(sidecar).exists(), "no SQLite sidecar is left in the generation");
    }
    seam_reseal_backup(backup, SCHEMA_VERSION, V79, &old_digest, old_bytes.len() as u64);
}

#[test]
fn import_seam_schema_v80_restores_a_v79_backup() {
    let test = "import_seam_schema_v80_restores_a_v79_backup";
    let temporary = crate::private_tempdir().unwrap();
    let data = temporary.path().join("live");
    let store = HubStore::initialize(&data).unwrap();
    let source = store
        .register_source(&SourceDescriptor::new("owner_api", "import-seam-backup"), 1_000)
        .unwrap();
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "81").with_tesla_identity(Some(81), None),
            1_000,
        )
        .unwrap();
    let vehicle_id = vehicle.vehicle_id.to_string();
    store
        .open()
        .unwrap()
        .execute(
            "INSERT INTO collection_coverage(grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
             VALUES ('grant', 100, 200, '[]', 'handback')",
            [],
        )
        .unwrap();
    let restored_rows = |path: &Path| -> (i32, bool, i64) {
        let connection = seam_raw(path);
        (
            schema_version(&connection).unwrap(),
            SEAM_TABLES.iter().all(|table| seam_table_present(&connection, table)),
            connection
                .query_row("SELECT COUNT(*) FROM collection_coverage", [], |row| row.get(0))
                .unwrap(),
        )
    };

    // A v80 store backs up, verifies and restores with its coverage row.
    let current = temporary.path().join("backup-v80");
    create_data_backup(&store, &current).labelled_ok(test, "a v80 store backs up");
    verify_data_backup(&current).labelled_ok(test, "the v80 generation verifies");
    let restored = temporary.path().join("restored-v80");
    restore_data_backup(&current, &restored).labelled_ok(test, "the v80 generation restores");
    assert_eq!(restored_rows(&restored), (SCHEMA_VERSION, true, 1), "{test}: v80 restore");

    // A v79 generation verifies at 79 and restores through the v80 rung.
    let older = temporary.path().join("backup-v79");
    create_data_backup(&store, &older).unwrap();
    seam_backup_as_v79(&older, "");
    verify_data_backup(&older).labelled_ok(test, "the v79 generation verifies");
    let restored = temporary.path().join("restored-v79");
    let report = restore_data_backup(&older, &restored).labelled_ok(test, "the v79 generation restores");
    assert_eq!(report.status, "data_restored");
    assert_eq!(restored_rows(&restored), (SCHEMA_VERSION, true, 0), "{test}: v79 restore");
    HubStore::initialize(&restored).labelled_ok(test, "the restored store reopens");

    // A v79 generation holding pre-seam Hub rows is refused and publishes nothing.
    let refused = temporary.path().join("backup-v79-hub-rows");
    create_data_backup(&store, &refused).unwrap();
    seam_backup_as_v79(
        &refused,
        &format!(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES ('{vehicle_id}', 4, 81, '{{}}');"
        ),
    );
    verify_data_backup(&refused).labelled_ok(test, "the generation itself is intact");
    let destination = temporary.path().join("restored-refused");
    let error = restore_data_backup(&refused, &destination).labelled_err(test, "Hub rows refuse the restore");
    assert!(
        matches!(
            error,
            DataRecoveryError::Store(StoreError::PreSeamCollectedRows {
                table: "materialised_drives",
                count: 1
            })
        ),
        "{test}: {error:?}"
    );
    assert!(!destination.exists(), "{test}: a refused restore publishes no destination");
}

#[test]
fn import_seam_schema_v80_credentials_open_and_read_after_migration() {
    let test = "import_seam_schema_v80_credentials_open_and_read_after_migration";
    let generation = Uuid::from_bytes([0x80; 16]);
    let temporary = crate::private_tempdir().unwrap();
    {
        let store = HubStore::initialize(temporary.path()).unwrap();
        s_test_with_credential_connection(&store, |connection| {
            connection.set_test_now_ms(1_800_000_000_000);
            let pair = s_test_synthetic_pair(
                connection,
                CredentialSlot::OwnerApi,
                generation,
                b"synthetic-v80-access",
                b"synthetic-v80-refresh",
                0,
                0,
            )
            .expect("synthetic pair");
            connection
                .replace_credential_pair(
                    &pair,
                    &EntryProvenance::Supplied,
                    &LegacyCandidates(Vec::new()),
                    ReplacePolicy::ReplaceAny,
                )
                .expect("credential write");
            connection.credential_durability_barrier().expect("barrier");
        });
        seam_downgrade_to_v79(&store.open().unwrap());
    }
    let read = |store: &HubStore| {
        (
            store.credential_format_state().unwrap().is_some(),
            store.credential_opaque_fence_count().unwrap(),
            s_test_with_credential_connection(store, |connection| {
                connection
                    .load_credential_pair(CredentialSlot::OwnerApi)
                    .unwrap()
                    .map(|pair| pair.generation)
            }),
        )
    };
    {
        // The v79 store reads its credentials before any v80 build opens it.
        let raw = seam_raw(temporary.path());
        assert_eq!(schema_version(&raw).unwrap(), V79);
        let pairs: i64 = raw.query_row("SELECT COUNT(*) FROM credential_pairs", [], |row| row.get(0)).unwrap();
        assert_eq!(pairs, 1, "{test}: the v79 store holds the sealed pair");
    }
    let store = HubStore::initialize(temporary.path()).labelled_ok(test, "the v79 store migrates");
    assert_eq!(schema_version(&store.open().unwrap()).unwrap(), SCHEMA_VERSION);
    assert_eq!(read(&store), (true, 0, Some(generation)), "{test}: credentials read at v80");
    drop(store);
    let store = HubStore::initialize(temporary.path()).labelled_ok(test, "the v80 store reopens");
    assert_eq!(read(&store), (true, 0, Some(generation)), "{test}: credentials read after reopen");
}

#[test]
fn import_seam_schema_v80_reopen_is_idempotent() {
    let test = "import_seam_schema_v80_reopen_is_idempotent";
    let (_directory, connection) = seam_v79_catalogue();
    seam_seed_imported_rows(&connection);
    // A rung stopped before its commit leaves the v79 store as it was and the
    // next open completes it.
    {
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Immediate).unwrap();
        transaction.execute_batch(IMPORT_SEAM_LINKS_SCHEMA).unwrap();
        transaction.execute_batch(COLLECTION_COVERAGE_SCHEMA).unwrap();
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION).unwrap();
        // Dropped without commit: rolled back, as a stop part way would be.
    }
    assert_eq!(schema_version(&connection).unwrap(), V79);
    assert!(SEAM_TABLES.iter().all(|seam| !seam_table_present(&connection, seam)));
    migrate(&connection).labelled_ok(test, "the interrupted rung runs again");
    let first = super::g2_4_m1::schema_snapshot(&connection);
    assert_eq!(first.0, SCHEMA_VERSION);
    for attempt in 0..2 {
        migrate(&connection).labelled_ok(test, "a v80 store migrates as a no-op");
        assert_eq!(super::g2_4_m1::schema_snapshot(&connection), first, "{test}: attempt {attempt}");
    }

    // The ordinary store open is a no-op on a v80 store, too.
    let temporary = seam_v79_store(|_, _| {});
    let opened = HubStore::initialize(temporary.path()).labelled_ok(test, "first open migrates");
    drop(opened);
    let snapshot = super::g2_4_m1::schema_snapshot(&seam_raw(temporary.path()));
    assert_eq!(snapshot.0, SCHEMA_VERSION);
    for _ in 0..2 {
        drop(HubStore::initialize(temporary.path()).labelled_ok(test, "reopen"));
        assert_eq!(super::g2_4_m1::schema_snapshot(&seam_raw(temporary.path())), snapshot);
    }
}

#[test]
fn import_seam_schema_v80_read_only_opens_a_v79_store() {
    let test = "import_seam_schema_v80_read_only_opens_a_v79_store";
    // An updated Hub holds a v79 store until its first writable open; status
    // and diagnosis lanes open it read-only and change nothing.
    let temporary = seam_v79_store(|_, _| {});
    let observer = seam_raw(temporary.path());
    let data_version = |connection: &Connection| -> i64 {
        connection.query_row("PRAGMA data_version", [], |row| row.get(0)).unwrap()
    };
    let before = data_version(&observer);
    for _ in 0..2 {
        drop(HubStore::open_read_only(temporary.path()).labelled_ok(test, "read-only lane opens v79"));
        drop(HubStore::open_existing(temporary.path()).labelled_ok(test, "control lane opens v79"));
    }
    assert_eq!(data_version(&observer), before, "{test}: read-only opens write nothing");
    assert_eq!(schema_version(&observer).unwrap(), V79, "{test}: version unchanged");
    assert!(SEAM_TABLES.iter().all(|seam| !seam_table_present(&observer, seam)), "{test}: no seam table created");

    // Outside the supported range the refusal stays the stable schema error.
    observer.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
    assert!(matches!(
        HubStore::open_read_only(temporary.path()),
        Err(StoreError::UnsupportedSchema(version)) if version == SCHEMA_VERSION + 1
    ));
    observer.pragma_update(None, "user_version", V79).unwrap();

    // The first writable open migrates it to the head.
    drop(HubStore::initialize(temporary.path()).labelled_ok(test, "writable open migrates"));
    assert_eq!(schema_version(&observer).unwrap(), SCHEMA_VERSION);
    assert!(SEAM_TABLES.iter().all(|seam| seam_table_present(&observer, seam)));
    drop(HubStore::open_read_only(temporary.path()).labelled_ok(test, "read-only lane opens v80"));
}
