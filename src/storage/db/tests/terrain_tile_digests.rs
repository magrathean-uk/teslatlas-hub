// SPDX-License-Identifier: AGPL-3.0-only

const TILE_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn digest_rows(connection: &Connection) -> Vec<(String, String, i64)> {
    connection
        .prepare(
            "SELECT tile_name, sha256, recorded_at_ms FROM terrain_tile_digests ORDER BY tile_name",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn an_unrecorded_tile_has_no_digest_and_a_recorded_one_reads_back() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    assert_eq!(store.terrain_tile_digest("N51W001").unwrap(), None);

    store
        .record_terrain_tile_digest("N51W001", TILE_DIGEST, 1_234)
        .unwrap();

    assert_eq!(
        store.terrain_tile_digest("N51W001").unwrap(),
        Some(TerrainTileDigest {
            sha256: TILE_DIGEST.to_owned(),
            recorded_at_ms: 1_234
        })
    );
    assert_eq!(
        store.terrain_tile_digest("N52W001").unwrap(),
        None,
        "the digest belongs to its tile only"
    );
}

#[test]
fn recording_a_digest_again_replaces_it() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let replacement = "f".repeat(64);

    store
        .record_terrain_tile_digest("S33E151", TILE_DIGEST, 10)
        .unwrap();
    store
        .record_terrain_tile_digest("S33E151", &replacement, 20)
        .unwrap();

    assert_eq!(
        digest_rows(&store.open().unwrap()),
        vec![("S33E151".to_owned(), replacement, 20)]
    );
}

#[test]
fn a_digest_survives_reopening_the_store() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    store
        .record_terrain_tile_digest("N00E000", TILE_DIGEST, 5)
        .unwrap();
    drop(store);

    let reopened = HubStore::initialize(temporary.path()).unwrap();

    assert_eq!(
        reopened
            .terrain_tile_digest("N00E000")
            .unwrap()
            .map(|digest| digest.sha256),
        Some(TILE_DIGEST.to_owned())
    );
}

#[test]
fn malformed_tile_names_and_digests_are_refused_before_and_by_the_table() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let uppercase = TILE_DIGEST.to_uppercase();
    let short = &TILE_DIGEST[..63];
    let long = format!("{TILE_DIGEST}0");
    let non_hex = format!("{}g", &TILE_DIGEST[..63]);

    for (name, digest, at) in [
        ("n51w001", TILE_DIGEST, 1),
        ("N51W01", TILE_DIGEST, 1),
        ("N91W001", TILE_DIGEST, 1),
        ("../N51W001", TILE_DIGEST, 1),
        ("N51W001", uppercase.as_str(), 1),
        ("N51W001", short, 1),
        ("N51W001", long.as_str(), 1),
        ("N51W001", non_hex.as_str(), 1),
        ("N51W001", TILE_DIGEST, -1),
    ] {
        assert!(
            matches!(
                store.record_terrain_tile_digest(name, digest, at),
                Err(StoreError::InvalidTerrainTileDigest)
            ),
            "{name} {digest} {at}"
        );
    }
    assert!(digest_rows(&store.open().unwrap()).is_empty());

    // The table enforces the same shapes for anything that bypasses the API.
    let connection = store.open().unwrap();
    for (name, digest) in [
        ("n51w001", TILE_DIGEST),
        ("N51W001", uppercase.as_str()),
        ("N51W001", short),
        ("N51W001", non_hex.as_str()),
    ] {
        assert!(
            connection
                .execute(
                    "INSERT INTO terrain_tile_digests(tile_name, sha256, recorded_at_ms)
                     VALUES (?1, ?2, 1)",
                    params![name, digest],
                )
                .is_err(),
            "{name} {digest}"
        );
    }
    assert!(
        connection
            .execute(
                "INSERT INTO terrain_tile_digests(tile_name, sha256, recorded_at_ms)
                 VALUES ('N51W001', ?1, -1)",
                params![TILE_DIGEST],
            )
            .is_err()
    );
}

#[test]
fn a_v68_store_migrates_to_an_empty_tile_digest_table_without_backfill() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let connection = store.open().unwrap();
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V69_SQL} PRAGMA user_version = 68; COMMIT;"
        ))
        .unwrap();
    let table_exists = |connection: &Connection| -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema
                               WHERE type = 'table' AND name = 'terrain_tile_digests')",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert!(
        !table_exists(&connection),
        "a v68 catalogue has no digest table"
    );
    drop(connection);
    drop(store);

    let migrated = HubStore::initialize(temporary.path()).unwrap();
    let connection = migrated.open().unwrap();

    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    assert!(table_exists(&connection));
    assert!(
        digest_rows(&connection).is_empty(),
        "tiles cached before the step are unproven and fetched again"
    );
    let strict: bool = connection
        .query_row(
            "SELECT strict FROM pragma_table_list('terrain_tile_digests')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(strict);
}
