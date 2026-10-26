// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

#[test]
fn import_guard_refusal_preserves_every_catalogue_row() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    store
        .register_import_source("home-source", 1000, false)
        .unwrap();
    let before = import_names_test_support::schema_snapshot(&store.open().unwrap());
    assert!(matches!(
        store.register_import_source("unmatched-private-key", 2000, false),
        Err(StoreError::UnmatchedImportSource { .. })
    ));
    assert_eq!(
        import_names_test_support::schema_snapshot(&store.open().unwrap()),
        before
    );
}

#[test]
fn concurrent_first_import_registration_has_one_winner() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let threads = ["home-source", "garage-source"].map(|key| {
        let store = store.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.register_import_source(key, 1000, false)
        })
    });
    barrier.wait();
    let results = threads.map(|thread| thread.join().unwrap());
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::UnmatchedImportSource { .. })))
            .count(),
        1
    );
    assert_eq!(
        stored_import_source_keys(&store.open().unwrap())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn read_only_import_preflight_preserves_current_store_and_absent_root() {
    let temporary = crate::private_tempdir().unwrap();
    let absent = temporary.path().join("absent");
    HubStore::preflight_import_source_key(&absent, "default", false).unwrap();
    assert!(!absent.exists());
    for materialised_schema in [false, true] {
        let fresh = temporary.path().join(if materialised_schema {
            "empty-schema"
        } else {
            "empty-file"
        });
        fs::create_dir(&fresh).unwrap();
        fs::set_permissions(&fresh, fs::Permissions::from_mode(0o700)).unwrap();
        let path = fresh.join("hub.sqlite");
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        if materialised_schema {
            Connection::open(&path)
                .unwrap()
                .execute_batch("CREATE TABLE disposable(id INTEGER); DROP TABLE disposable;")
                .unwrap();
        }
        let before = fs::read(&path).unwrap();
        HubStore::preflight_import_source_key(&fresh, "default", false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!fresh.join("packs").exists());
    }
    let store = HubStore::initialize(temporary.path()).unwrap();
    store
        .register_import_source("home-source", 1000, false)
        .unwrap();
    store.checkpoint_catalogue_for_immutable_read().unwrap();
    let before = fs::read(store.database_path()).unwrap();
    assert!(matches!(
        HubStore::preflight_import_source_key(temporary.path(), "other-source", false),
        Err(StoreError::UnmatchedImportSource { .. })
    ));
    HubStore::preflight_import_source_key(temporary.path(), "home-source", false).unwrap();
    HubStore::preflight_import_source_key(temporary.path(), "other-source", true).unwrap();
    assert_eq!(fs::read(store.database_path()).unwrap(), before);
}

#[test]
fn read_only_import_preflight_maps_historical_default_without_migrating() {
    let temporary = crate::private_tempdir().unwrap();
    let bytes = zstd::stream::decode_all(
        include_bytes!("../../../../fixtures/n1/v77-catalogue.sqlite.zst").as_slice(),
    )
    .unwrap();
    let database_path = temporary.path().join("hub.sqlite");
    fs::write(&database_path, &bytes).unwrap();
    fs::set_permissions(&database_path, fs::Permissions::from_mode(0o600)).unwrap();
    HubStore::preflight_import_source_key(temporary.path(), "default", false).unwrap();
    HubStore::preflight_import_source_key(temporary.path(), "retained-custom-key", false).unwrap();
    assert!(matches!(
        HubStore::preflight_import_source_key(temporary.path(), "other-source", false),
        Err(StoreError::UnmatchedImportSource { .. })
    ));
    HubStore::preflight_import_source_key(temporary.path(), "other-source", true).unwrap();
    assert_eq!(fs::read(&database_path).unwrap(), bytes);
    assert!(!temporary.path().join("packs").exists());
    let connection =
        Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        schema_version(&connection).unwrap(),
        crate::db::import_source_names_step::VERSION - 1
    );
}
