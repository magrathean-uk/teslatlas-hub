// SPDX-License-Identifier: AGPL-3.0-only

use super::super::import_names_test_support as fixture;
use super::super::import_source_names_step as step;
use super::*;

fn previous() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    fixture::preceding(&connection);
    connection
}

#[test]
fn mt1_upgrade_renames_every_object_and_keeps_rows() {
    let connection = previous();
    let expected = fixture::expected(fixture::snapshot(&connection));
    migrate(&connection).unwrap();
    fixture::assert_expected(&connection, &expected);
}

#[test]
fn mt1b_orphan_current_row_refuses_the_step() {
    let connection = previous();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection.execute("UPDATE current_observations SET source_id='10000000-0000-4000-8000-999999999999' WHERE observation_id=1",[]).unwrap();
    let before = fixture::schema_snapshot(&connection);
    assert!(matches!(
        migrate(&connection),
        Err(StoreError::ImportSourceNameMigration(
            "the rebuilt current rows have an invalid foreign key"
        ))
    ));
    assert_eq!(fixture::schema_snapshot(&connection), before);
}

#[test]
fn mt2_fresh_store_has_no_old_names() {
    let connection = Connection::open_in_memory().unwrap();
    migrate(&connection).unwrap();
    let sql = connection
        .query_row("SELECT group_concat(sql) FROM sqlite_schema", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    for old in step::OLD_TABLES {
        assert!(!sql.contains(old));
    }
    assert!(!sql.contains(step::OLD_POSITION_TYPE));
    connection
        .execute(
            "INSERT INTO sources VALUES (?1,'source_db',1,0)",
            [fixture::SOURCE],
        )
        .unwrap();
    connection.execute("INSERT INTO vehicles(vehicle_id,source_id,source_vehicle_key,created_at_ms,last_seen_at_ms) VALUES (?1,?2,'car',0,0)",params![fixture::VEHICLE,fixture::SOURCE]).unwrap();
    assert!(
        fixture::insert_current(
            &connection,
            fixture::VEHICLE,
            fixture::SOURCE,
            "source_db_position_v1",
            1,
            "{\"record_type\":\"source_db_position_v1\"}"
        )
        .is_ok()
    );
    assert!(
        fixture::insert_current(
            &connection,
            fixture::VEHICLE,
            fixture::SOURCE,
            step::OLD_POSITION_TYPE,
            2,
            "{}"
        )
        .is_err()
    );
    assert_eq!(schema_version(&connection).unwrap(), step::VERSION);
}

#[test]
fn mt3_failed_step_leaves_the_previous_version() {
    let connection = previous();
    let before = fixture::schema_snapshot(&connection);
    let expected = fixture::expected(fixture::snapshot(&connection));
    assert!(step::migrate_with_fault(&connection, true).is_err());
    assert_eq!(fixture::schema_snapshot(&connection), before);
    migrate(&connection).unwrap();
    fixture::assert_expected(&connection, &expected);
}

fn collision(default_only: bool, new_kind: bool) {
    let connection = previous();
    if default_only {
        connection
            .execute(
                "UPDATE source_identities SET source_key='default' WHERE source_id=?1",
                [fixture::SOURCE],
            )
            .unwrap();
    } else {
        connection.execute("UPDATE source_identities SET source_key='default' WHERE source_key='retained-custom-key'",[]).unwrap();
    }
    if new_kind {
        connection.execute("UPDATE source_identities SET source_kind='source_db',source_key='independent-key' WHERE source_id=?1",[fixture::SOURCE]).unwrap();
        connection.execute("UPDATE source_identities SET source_key='retained-custom-key' WHERE source_key='default'",[]).unwrap();
    }
    let before = fixture::schema_snapshot(&connection);
    let error = migrate(&connection).unwrap_err();
    assert!(matches!(error, StoreError::ImportSourceNameMigration(_)));
    assert_eq!(fixture::schema_snapshot(&connection), before);
}

#[test]
fn mt9a_refuses_a_store_with_both_keys() {
    collision(false, false);
}
#[test]
fn mt9b_refuses_a_store_with_only_the_default_key() {
    collision(true, false);
}
#[test]
fn mt9c_refuses_any_already_neutral_kind() {
    collision(false, true);
}

#[test]
fn mt11_step_refuses_when_legacy_alter_table_is_on() {
    let connection = previous();
    connection
        .pragma_update(None, "legacy_alter_table", true)
        .unwrap();
    let before = fixture::schema_snapshot(&connection);
    assert!(matches!(
        migrate(&connection),
        Err(StoreError::ImportSourceNameMigration(
            "legacy_alter_table must be off before the import-name step"
        ))
    ));
    assert_eq!(fixture::schema_snapshot(&connection), before);
}

#[test]
fn unexpected_current_payload_rolls_back_the_step() {
    let connection = previous();
    connection
        .execute(
            "UPDATE current_observations SET payload_json='{}' WHERE observation_id=1",
            [],
        )
        .unwrap();
    let before = fixture::schema_snapshot(&connection);
    assert!(migrate(&connection).is_err());
    assert_eq!(fixture::schema_snapshot(&connection), before);
}

#[test]
fn legacy_owner_tokens_load_refresh_and_count_through_the_renamed_table() {
    let temporary = crate::private_tempdir().unwrap();
    let path = temporary.path().join("hub.sqlite");
    let connection = Connection::open(&path).unwrap();
    fixture::preceding(&connection);
    drop(connection);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let pair = store.load_legacy_owner_tokens().unwrap().unwrap();
    assert_eq!(pair.access(), [1, 2, 3]);
    assert_eq!(pair.refresh(), [4, 5, 6]);
    assert_eq!(
        store.runtime_inventory().unwrap().legacy_owner_token_rows,
        1
    );
    let updated =
        LegacyOwnerTokenStore::refreshed(vec![8, 9], vec![10, 11], 2_000_000_001, 1_000_000_001)
            .unwrap();
    store.replace_legacy_owner_tokens(&updated).unwrap();
    let loaded = store.load_legacy_owner_tokens().unwrap().unwrap();
    assert_eq!(loaded.access(), [8, 9]);
    assert_eq!(loaded.refresh(), [10, 11]);
    assert_eq!(loaded.expires_at(), 2_000_000_001);
    assert_eq!(loaded.next_refresh_at(), 1_000_000_001);
    assert_eq!(
        store.catalogue_inventory().unwrap().legacy_owner_token_rows,
        1
    );
}

#[test]
fn migrated_default_source_registration_reuses_source_and_vehicle_ids() {
    let temporary = crate::private_tempdir().unwrap();
    let path = temporary.path().join("hub.sqlite");
    let connection = Connection::open(&path).unwrap();
    fixture::preceding(&connection);
    drop(connection);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let source = store
        .register_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, DEFAULT_IMPORT_SOURCE_KEY),
            3000,
        )
        .unwrap();
    assert_eq!(source.source_id.to_string(), fixture::SOURCE);
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "car-1"), 3000)
        .unwrap();
    assert_eq!(vehicle.vehicle_id.to_string(), fixture::VEHICLE);
    assert_eq!(store.runtime_inventory().unwrap().vehicles, 66);
}

#[test]
fn enlarged_writer_payload_refuses_without_partial_migration() {
    let connection = previous();
    let payload = format!(
        "{{\"record_type\":\"{}\",\"measurements\":[{}]}}",
        step::OLD_POSITION_TYPE,
        vec!["1e5"; 45_000].join(",")
    );
    assert!(payload.len() < MAX_RAW_OBSERVATION_BYTES);
    connection
        .execute(
            "UPDATE current_observations SET payload_json=?1 WHERE observation_id=1",
            [payload],
        )
        .unwrap();
    let before = fixture::schema_snapshot(&connection);
    assert!(matches!(
        migrate(&connection),
        Err(StoreError::ObservationTooLarge { .. })
    ));
    assert_eq!(fixture::schema_snapshot(&connection), before);
}

#[test]
fn current_import_names_rewind_and_upgrade_round_trip() {
    let connection = previous();
    migrate(&connection).unwrap();
    let before = fixture::snapshot(&connection);
    connection
        .pragma_update(None, "user_version", step::VERSION - 1)
        .unwrap();
    fixture::prepare_historical_import_names(&connection);
    assert_eq!(schema_version(&connection).unwrap(), step::VERSION - 1);
    migrate_historical_fixture(&connection).unwrap();
    assert_eq!(schema_version(&connection).unwrap(), step::VERSION);
    assert_eq!(fixture::snapshot(&connection), before);
}
