// SPDX-License-Identifier: AGPL-3.0-only

//! FIX-UPGRADE-RO: the start-up checks that run before the migrations admit a
//! set-up Hub at any supported older schema.

use super::*;
use crate::storage::setup_marker::{self, SetupMarkerError};

fn head_store() -> (tempfile::TempDir, Uuid) {
    let directory = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(directory.path()).unwrap();
    let id = store.installation_id().unwrap();
    drop(store);
    (directory, id)
}

fn catalogue_connection(directory: &tempfile::TempDir) -> Connection {
    let connection = Connection::open(directory.path().join("hub.sqlite")).unwrap();
    connection.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    connection
}

/// A head catalogue stamped as the older `version`, as the existing rung tests
/// make one: v79 lacks the two import seam tables, and every version below the
/// head lacks the drive repair tables.
fn rewind_head_catalogue(directory: &tempfile::TempDir, version: i32) {
    let connection = catalogue_connection(directory);
    for table in DRIVE_REPAIR_TABLES {
        connection.execute_batch(&format!("DROP TABLE {table};")).unwrap();
    }
    if version < 80 {
        connection
            .execute_batch("DROP TABLE import_seam_links; DROP TABLE collection_coverage;")
            .unwrap();
    }
    connection.pragma_update(None, "user_version", version).unwrap();
}

fn stored_version(directory: &tempfile::TempDir) -> i32 {
    schema_version(&catalogue_connection(directory)).unwrap()
}

/// The marker check passes the older catalogue untouched, then the start's own
/// open brings it to the head with its installation id unchanged.
fn assert_start_reaches_head(directory: &tempfile::TempDir, id: Uuid, from: i32) {
    setup_marker::record(directory.path(), id).expect("marker");
    assert_eq!(setup_marker::check_catalogue(directory.path()), Ok(()), "from v{from}");
    assert_eq!(stored_version(directory), from, "the check changes nothing, from v{from}");
    let store = HubStore::initialize(directory.path()).unwrap_or_else(|error| panic!("start from v{from}: {error:?}"));
    assert_eq!(store.installation_id().unwrap(), id, "from v{from}");
    drop(store);
    assert_eq!(stored_version(directory), SCHEMA_VERSION, "from v{from}");
    assert_eq!(setup_marker::check_catalogue(directory.path()), Ok(()), "at the head");
}

#[test]
fn fresh_install_journey_marker_check_admits_every_older_rung_and_the_start_migrates() {
    for version in [79, 80, 81, 82] {
        let (directory, id) = head_store();
        rewind_head_catalogue(&directory, version);
        assert_start_reaches_head(&directory, id, version);
    }
}

#[test]
fn fresh_install_journey_marker_check_admits_the_floor_store_and_the_start_migrates() {
    let directory = g2_4_m1::copied_store(include_bytes!("../../../../fixtures/g2-4/m1/store-f.sqlite.zst"));
    let connection = catalogue_connection(&directory);
    assert_eq!(schema_version(&connection).unwrap(), MIN_STORE_VERSION);
    assert!(g2_4_m1::catalogue_floor_witnesses(&connection) > 0);
    let id = HubStore::open_migration_candidate_read_only(directory.path())
        .and_then(|store| store.installation_id())
        .expect("the floor store's installation id");
    drop(connection);
    assert_start_reaches_head(&directory, id, MIN_STORE_VERSION);
}

#[test]
fn fresh_install_journey_marker_check_without_a_marker_leaves_an_older_unconfigured_catalogue_to_the_start() {
    let (directory, _) = head_store();
    rewind_head_catalogue(&directory, 82);
    assert_eq!(setup_marker::check_catalogue(directory.path()), Ok(()));
    assert_eq!(setup_marker::read(directory.path()), Ok(None), "no marker is written");
    assert_eq!(stored_version(&directory), 82);
}

#[test]
fn fresh_install_journey_marker_check_still_refuses_what_it_refused_on_an_older_schema() {
    // A wrong installation id.
    let (directory, _) = head_store();
    rewind_head_catalogue(&directory, 82);
    setup_marker::record(directory.path(), Uuid::new_v4()).expect("another marker");
    assert_eq!(setup_marker::check_catalogue(directory.path()), Err(SetupMarkerError::CatalogueReplaced));

    // A newer schema, and a foreign application id.
    let (directory, id) = head_store();
    setup_marker::record(directory.path(), id).expect("marker");
    let connection = catalogue_connection(&directory);
    connection.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
    assert_eq!(setup_marker::check_catalogue(directory.path()), Err(SetupMarkerError::CatalogueUnreadable));
    connection.pragma_update(None, "user_version", SCHEMA_VERSION).unwrap();
    connection.pragma_update(None, "application_id", 1).unwrap();
    assert_eq!(setup_marker::check_catalogue(directory.path()), Err(SetupMarkerError::CatalogueUnreadable));
    connection.pragma_update(None, "application_id", APPLICATION_ID).unwrap();
    // Below the floor.
    connection.pragma_update(None, "user_version", MIN_STORE_VERSION - 1).unwrap();
    assert_eq!(setup_marker::check_catalogue(directory.path()), Err(SetupMarkerError::CatalogueUnreadable));
    drop(connection);

    // An empty file, then a missing one.
    let (directory, id) = head_store();
    setup_marker::record(directory.path(), id).expect("marker");
    let path = directory.path().join("hub.sqlite");
    fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(0).unwrap();
    assert_eq!(setup_marker::check_catalogue(directory.path()), Err(SetupMarkerError::CatalogueUnreadable));
    for name in ["hub.sqlite", "hub.sqlite-wal", "hub.sqlite-shm"] {
        let _ = fs::remove_file(directory.path().join(name));
    }
    assert_eq!(setup_marker::check_catalogue(directory.path()), Err(SetupMarkerError::CatalogueMissing));
    assert!(!path.exists(), "nothing created");
}

#[test]
fn store_opening_migration_candidate_open_admits_the_supported_range_and_keeps_the_live_set() {
    let (directory, id) = head_store();
    rewind_head_catalogue(&directory, 82);
    let candidate = HubStore::open_migration_candidate_read_only(directory.path()).expect("v82 candidate");
    assert_eq!(candidate.installation_id().unwrap(), id);
    drop(candidate);
    // The other read-only routes keep their explicit set.
    assert!(matches!(HubStore::open_read_only(directory.path()), Err(StoreError::UnsupportedSchema(82))));
    assert!(matches!(HubStore::open_existing(directory.path()), Err(StoreError::UnsupportedSchema(82))));
    assert_eq!(stored_version(&directory), 82, "nothing was changed");

    let connection = catalogue_connection(&directory);
    connection.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
    assert!(matches!(
        HubStore::open_migration_candidate_read_only(directory.path()),
        Err(StoreError::UnsupportedSchema(version)) if version == SCHEMA_VERSION + 1
    ));
    connection.pragma_update(None, "user_version", SCHEMA_VERSION).unwrap();
    connection.pragma_update(None, "application_id", 1).unwrap();
    assert!(matches!(
        HubStore::open_migration_candidate_read_only(directory.path()),
        Err(StoreError::InvalidApplicationId(1))
    ));
}
