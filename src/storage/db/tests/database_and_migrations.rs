// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn catalogue_path_admission_rejects_substitution_and_weakened_modes() {
    for fault in [
        "root_link",
        "catalogue_link",
        "root_mode",
        "catalogue_mode",
        "-wal",
        "-shm",
        "-journal",
    ] {
        let temporary = crate::private_tempdir().expect("temporary parent");
        let data_dir = temporary.path().join("hub");
        let store = HubStore::initialize(&data_dir).expect("initialise store");
        store.readiness_check().expect("healthy before substitution");
        match fault {
            "root_link" => {
                let target = temporary.path().join("original-root");
                fs::rename(&data_dir, &target).expect("move root");
                std::os::unix::fs::symlink(target, &data_dir).expect("substitute root link");
            }
            "catalogue_link" => {
                let target = temporary.path().join("original.sqlite");
                fs::rename(store.database_path(), &target).expect("move catalogue");
                std::os::unix::fs::symlink(target, store.database_path())
                    .expect("substitute catalogue link");
            }
            "root_mode" => {
                fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o755))
                    .expect("weaken root");
            }
            "catalogue_mode" => {
                fs::set_permissions(store.database_path(), fs::Permissions::from_mode(0o644))
                    .expect("weaken catalogue");
            }
            suffix => {
                let target = temporary.path().join("outside-sidecar");
                fs::write(&target, []).expect("synthetic sidecar target");
                let sidecar = data_dir.join(format!("hub.sqlite{suffix}"));
                if sidecar.exists() {
                    fs::remove_file(&sidecar).expect("remove original sidecar");
                }
                std::os::unix::fs::symlink(target, sidecar).expect("substitute sidecar link");
            }
        }
        let rejected = [
            ("existing handle readiness", store.readiness_check().is_err()),
            (
                "service readiness",
                store.service_readiness_at(false, 1_000).is_err(),
            ),
            ("read-only open", HubStore::open_read_only(&data_dir).is_err()),
            ("control open", HubStore::open_existing(&data_dir).is_err()),
            (
                "immutable open",
                HubStore::open_immutable_read_only(&data_dir).is_err(),
            ),
            (
                "schema preflight",
                HubStore::preflight_standalone_schema_read_only(&data_dir).is_err(),
            ),
            ("existing writer", store.open().is_err()),
        ];
        for (reader, rejected) in rejected {
            assert!(rejected, "{reader} accepted {fault}");
        }
    }
}

#[test]
fn catalogue_path_admission_readers_preserve_supported_live_file_modes() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("initialise store");
    let connection = store.open().expect("keep live WAL open");
    connection
        .execute(
            "INSERT INTO hub_metadata(key, value) VALUES ('synthetic_probe', 'synthetic')",
            [],
        )
        .expect("seed live WAL");
    let paths = [
        store.database_path().to_path_buf(),
        temporary.path().join("hub.sqlite-wal"),
        temporary.path().join("hub.sqlite-shm"),
    ];
    for path in &paths {
        fs::set_permissions(path, fs::Permissions::from_mode(0o640)).expect("supported mode");
    }
    let reader = HubStore::open_read_only(temporary.path()).expect("live reader");
    assert_eq!(reader.installation_id().unwrap(), store.installation_id().unwrap());
    HubStore::open_existing(temporary.path()).expect("control reader");
    for path in &paths {
        assert_eq!(fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777, 0o640);
    }
}

#[test]
fn catalogue_path_admission_leaves_an_absent_root_to_each_openers_own_error() {
    let temporary = crate::private_tempdir().expect("temporary parent");
    let data_dir = temporary.path().join("missing-hub");
    assert!(matches!(
        HubStore::open_immutable_read_only(&data_dir),
        Err(StoreError::InspectCatalogue(error)) if error.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(matches!(HubStore::open_existing(&data_dir), Err(StoreError::Open(_))));
    assert!(!data_dir.exists(), "no opener creates an absent root");
}

#[test]
fn writable_catalogue_admission_rejects_foreign_and_future_databases_without_mutation() {
    for (application_id, version) in [(0, 0), (123456, 0), (APPLICATION_ID, SCHEMA_VERSION + 1)] {
        let temporary = crate::private_tempdir().expect("temporary database");
        let path = temporary.path().join("hub.sqlite");
        let connection = Connection::open(&path).expect("foreign database");
        connection
            .execute_batch(&format!(
                "CREATE TABLE owner_data(value TEXT); INSERT INTO owner_data VALUES ('preserved');
                 PRAGMA application_id = {application_id}; PRAGMA user_version = {version};"
            ))
            .expect("seed foreign identity");
        drop(connection);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::read(&path).unwrap();

        let error = HubStore::initialize(temporary.path()).expect_err("reject foreign catalogue");
        if application_id == APPLICATION_ID {
            assert!(matches!(error, StoreError::UnsupportedSchema(found) if found == version));
        } else {
            assert!(
                matches!(error, StoreError::InvalidApplicationId(found) if found == application_id)
            );
        }
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "original SQLite bytes remain unchanged"
        );
        assert!(!temporary.path().join("hub.sqlite-wal").exists());
        assert!(!temporary.path().join("hub.sqlite-shm").exists());
    }
}

#[test]
fn rejected_0640_catalogue_and_live_wal_sidecars_keep_their_permissions() {
    for (application_id, version) in [(123456, 0), (APPLICATION_ID, SCHEMA_VERSION + 1)] {
        let temporary = crate::private_tempdir().unwrap();
        let path = temporary.path().join("hub.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "PRAGMA journal_mode=WAL; CREATE TABLE owner_data(value TEXT);
             INSERT INTO owner_data VALUES ('preserved');
             PRAGMA application_id={application_id}; PRAGMA user_version={version};"
            ))
            .unwrap();
        // Keep the source handle alive so the real WAL and SHM survive; the
        // attempted Hub admission must not repair another catalogue's modes.
        let paths = [
            path.clone(),
            temporary.path().join("hub.sqlite-wal"),
            temporary.path().join("hub.sqlite-shm"),
        ];
        for file in &paths {
            fs::set_permissions(file, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let database_before = fs::read(&path).unwrap();
        let wal_before = fs::read(&paths[1]).unwrap();
        assert!(HubStore::initialize(temporary.path()).is_err());
        assert_eq!(fs::read(&path).unwrap(), database_before);
        assert_eq!(fs::read(&paths[1]).unwrap(), wal_before);
        for file in &paths {
            assert_eq!(
                fs::symlink_metadata(file).unwrap().permissions().mode() & 0o777,
                0o640,
                "rejected file mode preserved: {}",
                file.display()
            );
        }
        drop(connection);
    }
}

#[test]
fn routine_writable_open_does_not_recreate_a_missing_catalogue() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    fs::remove_file(store.database_path()).unwrap();
    assert!(matches!(
        store.open(),
        Err(StoreError::InspectSharedSqlite(_))
    ));
    assert!(!store.database_path().exists());
}

#[test]
fn lifecycle_cursor_query_uses_the_per_vehicle_id_index() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {OBSERVATIONS_AFTER_ID_SQL}"))
        .expect("query plan");
    let details = statement
        .query_map(params![Uuid::new_v4().to_string(), 0_i64, 10_i64], |row| {
            row.get::<_, String>(3)
        })
        .expect("plan rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("plan details");
    let plan = details.join("\n");
    assert!(
        plan.contains("raw_observations_vehicle_cursor"),
        "lifecycle cursor must use the per-vehicle cursor index: {plan}"
    );
    assert!(
        !plan.contains("TEMP B-TREE") && !plan.contains("raw_observations_vehicle_observed"),
        "lifecycle cursor must not sort or use the timestamp index: {plan}"
    );
}

#[test]
fn public_drive_query_uses_its_vehicle_time_cursor_index_without_sorting() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {PUBLIC_DRIVES_PAGE_SQL}"))
        .expect("query plan");
    let details = statement
        .query_map(
            params![
                Uuid::new_v4().to_string(),
                0_i64,
                10_i64,
                10_i64,
                10_i64,
                10_i64
            ],
            |row| row.get::<_, String>(3),
        )
        .expect("plan rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("plan details");
    let plan = details.join("\n");
    assert!(
        plan.contains("materialised_drives_public_query"),
        "public drive query must use its vehicle/time/cursor index: {plan}"
    );
    assert!(
        !plan.contains("TEMP B-TREE"),
        "public drive query must not sort an entire vehicle history: {plan}"
    );
}

fn tree_contents(root: &Path) -> Vec<(PathBuf, u32, Option<(u64, String)>)> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut Vec<(PathBuf, u32, Option<(u64, String)>)>,
    ) {
        for entry in fs::read_dir(directory).expect("read snapshot directory") {
            let entry = entry.expect("read snapshot entry");
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("snapshot metadata");
            let relative = path
                .strip_prefix(root)
                .expect("snapshot path below root")
                .to_path_buf();
            if metadata.is_dir() {
                entries.push((relative, metadata.permissions().mode(), None));
                visit(root, &path, entries);
            } else {
                let bytes = fs::read(&path).expect("snapshot file");
                entries.push((
                    relative,
                    metadata.permissions().mode(),
                    Some((metadata.len(), hex::encode(Sha256::digest(bytes)))),
                ));
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

// M08-T17: a store last written by the previously bundled SQLite (3.53.2) opens
// with the present one, passes SQLite's integrity check before and after the
// Hub's admitted open, and keeps its rows.
#[test]
fn a_store_written_by_the_previous_bundled_sqlite_opens_and_checks() {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/catalogue-floor.sqlite");
    let bytes = fs::read(source).expect("fixture");
    let writer = u32::from_be_bytes(bytes[96..100].try_into().unwrap());
    assert_eq!(writer, 3_053_002, "fixture last written by SQLite 3.53.2");
    let temporary = crate::private_tempdir().expect("temporary directory");
    let path = temporary.path().join("hub.sqlite");
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let integrity = |connection: &Connection| -> String {
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("integrity check")
    };
    let expected_id: String = {
        let connection =
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(integrity(&connection), "ok");
        connection
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = 'installation_id'",
                [],
                |row| row.get(0),
            )
            .expect("installation id row")
    };

    let store = HubStore::initialize(temporary.path()).expect("admitted open and migration");
    store.quick_check().expect("quick check");
    store.catalogue_check().expect("catalogue check");
    let connection = store.open().expect("reopen store");
    assert_eq!(integrity(&connection), "ok");
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    assert_eq!(store.sqlite_version().unwrap(), BUNDLED_SQLITE_VERSION);
    assert_eq!(store.installation_id().unwrap().to_string(), expected_id);
}

#[test]
fn initializes_a_checked_wal_database() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    store.quick_check().expect("database passes quick check");
    assert!(store.database_path().exists());
    assert!(store.packs_dir().is_dir());

    let connection = store.open().expect("reopen store");
    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal mode");
    assert_eq!(journal, "wal");
    let application_id: i32 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .expect("application id");
    assert_eq!(application_id, APPLICATION_ID);
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    assert_eq!(
        store.sqlite_version().expect("SQLite version"),
        BUNDLED_SQLITE_VERSION
    );
    assert!(!store.installation_id().expect("installation ID").is_nil());
    let inventory = store
        .catalogue_inventory()
        .expect("fresh catalogue inventory");
    assert_eq!(inventory.schema_version, SCHEMA_VERSION);
    assert_eq!(inventory.journal_mode, "wal");
    assert!(inventory.foreign_keys_enabled);
    assert_eq!(inventory.synchronous, 2);
    assert_eq!(inventory.vehicles, 0);
    assert_eq!(inventory.raw_observations, 0);
    assert_eq!(inventory.quarantined_sessions, 0);
    assert_eq!(inventory.referenced_packs, 0);
    assert_eq!(inventory.owner_api_credential_rows, 0);
    assert_eq!(inventory.fleet_api_credential_rows, 0);
    assert_eq!(
        inventory.installation_id,
        store.installation_id().expect("id")
    );
    let before = store
        .credential_pair_presence(CredentialSlot::OwnerApi)
        .expect("legacy tokens")
        .is_some();
    let fleet_before = store
        .credential_pair_presence(CredentialSlot::FleetApi)
        .expect("Fleet tokens")
        .is_some();
    let _ = store
        .catalogue_inventory()
        .expect("inventory is repeatable");
    assert_eq!(
        store
            .credential_pair_presence(CredentialSlot::OwnerApi)
            .expect("legacy tokens after inventory")
            .is_some(),
        before
    );
    assert_eq!(
        store
            .credential_pair_presence(CredentialSlot::FleetApi)
            .expect("Fleet tokens after inventory")
            .is_some(),
        fleet_before
    );

    let runtime = store.runtime_inventory().expect("runtime inventory");
    assert_eq!(runtime.journal_mode, "wal");
    assert_eq!(runtime.vehicles, 0);
    assert_eq!(runtime.raw_observations, 0);
    assert_eq!(runtime.quarantined_sessions, 0);
    assert_eq!(runtime.referenced_packs, 0);
    assert_eq!(runtime.owner_api_credential_rows, 0);
    assert_eq!(runtime.fleet_api_credential_rows, 0);
}

#[test]
fn directory_parent_persistence_store_ancestors() {
    let temporary = crate::private_tempdir().expect("temporary parent");
    let data_dir = temporary.path().join("first/second/hub");
    let parents = [
        temporary.path().to_path_buf(),
        temporary.path().join("first"),
        temporary.path().join("first/second"),
    ];
    let before = parents
        .each_ref()
        .map(|path| crate::platform::file_attempts::directory_syncs_at(path));

    let store = HubStore::initialize(&data_dir).expect("initialise nested store");
    store.quick_check().expect("healthy catalogue");
    for (parent, before) in parents.iter().zip(before) {
        assert!(
            crate::platform::file_attempts::directory_syncs_at(parent) > before,
            "every new ancestor of the store needs its containing directory synced"
        );
    }
    let synced = parents
        .each_ref()
        .map(|path| crate::platform::file_attempts::directory_syncs_at(path));
    drop(store);
    HubStore::initialize(&data_dir).expect("reopen nested store");
    assert_eq!(
        parents
            .each_ref()
            .map(|path| crate::platform::file_attempts::directory_syncs_at(path)),
        synced
    );
}

#[test]
fn directory_parent_persistence_admitted_root() {
    // Production admits the data directory (taking the lifetime lock, which
    // creates the root) before the store is initialised.
    let temporary = crate::private_tempdir().expect("temporary parent");
    let data_dir = temporary.path().join("first/second/hub");
    let parents = [
        temporary.path().to_path_buf(),
        temporary.path().join("first"),
        temporary.path().join("first/second"),
    ];
    let before = parents
        .each_ref()
        .map(|path| crate::platform::file_attempts::directory_syncs_at(path));

    let admitted = crate::hub_user_process::AdmittedUserHub::admit(&data_dir)
        .expect("admit a missing nested root");
    for (parent, before) in parents.iter().zip(before) {
        assert!(
            crate::platform::file_attempts::directory_syncs_at(parent) > before,
            "the lifetime lock persists every new ancestor of the data directory"
        );
    }
    let store = HubStore::initialize(&data_dir).expect("initialise the admitted root");
    store.quick_check().expect("healthy catalogue");
    drop(store);
    drop(admitted);
}

#[test]
fn inventory_counts_physical_pack_and_staging_bytes_once() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let content = store.packs_dir().join("sha256");
    let staging = store.packs_dir().join(".staging");
    fs::create_dir_all(&content).expect("content directory");
    fs::create_dir_all(&staging).expect("staging directory");
    let orphan = content.join("orphan.sqlite.zst");
    fs::write(&orphan, b"abc").expect("orphan pack");
    fs::hard_link(
        &orphan,
        store.packs_dir().join("legacy-hardlink.sqlite.zst"),
    )
    .expect("hard link fixture");
    fs::write(staging.join("projection.tmp"), b"12345").expect("staging file");

    let inventory = store.catalogue_inventory().expect("inventory");
    assert_eq!(inventory.referenced_packs, 0);
    assert_eq!(inventory.referenced_pack_bytes, 0);
    assert_eq!(inventory.physical_pack_files, 2);
    assert_eq!(inventory.physical_pack_bytes, 8);
}

// M02-T02: a source-token import is refused before parsing and stores nothing.
#[test]
fn source_db_token_import_source_credential_disabled_stores_no_row() {
    use crate::auth::{credential_sign_in as entry, credential_vault::fixtures};
    let root = crate::private_tempdir().expect("entry root");
    let store = HubStore::initialize(root.path()).expect("store");
    let vault = fixtures::open_fixture_vault(store.clone(), root.path());
    let refused = entry::prepare_owner_entry(entry::OwnerEntryKind::SourceToken, zeroize::Zeroizing::new(" \taccess-0001\r\n".to_owned()), Some(zeroize::Zeroizing::new("refresh-0001\n".to_owned())), false).unwrap_err();
    assert_eq!(refused.code(), "source_credential_disabled");
    assert!(vault.load(crate::auth::credential_envelope::CredentialSlot::OwnerApi).expect("load").is_none());
    assert_eq!(store.open().expect("observer").query_row::<i64, _, _>("SELECT count(*) FROM credential_pairs", [], |row| row.get(0)).expect("pair count"), 0);
}

#[test]
fn source_db_own_login_store_seals_ciphertext() {
    use crate::auth::credential_vault::fixtures;
    let root = crate::private_tempdir().expect("entry root");
    fixtures::seed_legacy_supplied_pair(root.path(), "access-0001", "refresh-0001", None);
    let store = HubStore::initialize(root.path()).expect("store");
    let connection = store.open().expect("observer");
    let (access, refresh): (Vec<u8>, Vec<u8>) = connection.query_row("SELECT access_envelope,refresh_envelope FROM credential_pairs WHERE slot='owner_api'", [], |row| Ok((row.get(0)?,row.get(1)?))).expect("sealed pair");
    for bytes in [&access, &refresh] {
        assert!(!bytes.windows(b"access-0001".len()).any(|part| part == b"access-0001"));
        assert!(!bytes.windows(b"refresh-0001".len()).any(|part| part == b"refresh-0001"));
    }
}

#[test]
fn source_db_own_login_replacement_is_atomic_and_refresh_schedule_is_strict() {
    use crate::auth::credential_vault::fixtures;
    let root = crate::private_tempdir().expect("entry root");
    let store = HubStore::initialize(root.path()).expect("store");
    let vault = fixtures::open_fixture_vault(store.clone(), root.path());
    fixtures::replace_legacy_supplied_pair(&store, root.path(), "access-0001", "refresh-0001", None).expect("first pair");
    let before = vault.load(crate::auth::credential_envelope::CredentialSlot::OwnerApi).expect("load").expect("pair");
    let observer = store.open().expect("observer");
    observer.execute_batch("CREATE TRIGGER fixture_pair_refusal BEFORE INSERT ON credential_pairs BEGIN SELECT RAISE(ABORT,'fixture pair fault'); END;").expect("fault trigger");
    assert!(fixtures::replace_legacy_supplied_pair(&store, root.path(), "access-0002", "refresh-0002", None).is_err());
    let after = vault.load(crate::auth::credential_envelope::CredentialSlot::OwnerApi).expect("load after refusal").expect("retained pair");
    assert_eq!((after.generation,after.access.as_str(),after.refresh.as_str(),after.expires_at,after.next_refresh_at), (before.generation,"access-0001","refresh-0001",0,0));
    observer.execute_batch("DROP TRIGGER fixture_pair_refusal").expect("end fault");
    fixtures::replace_legacy_supplied_pair(&store, root.path(), "access-0002", "refresh-0002", None).expect("explicit replacement");
    let after = vault.load(crate::auth::credential_envelope::CredentialSlot::OwnerApi).expect("replacement").expect("one pair");
    assert_eq!((after.access.as_str(),after.refresh.as_str(),after.expires_at,after.next_refresh_at), ("access-0002","refresh-0002",0,0));
    assert_ne!(after.generation,before.generation);
}

#[cfg(unix)]
#[test]
fn initialize_rejects_weakened_existing_data_or_packs_directory() {
    let weakened_data = crate::private_tempdir().expect("weakened data root");
    fs::set_permissions(weakened_data.path(), fs::Permissions::from_mode(0o755))
        .expect("weaken data mode");
    assert!(matches!(
        HubStore::initialize(weakened_data.path()),
        Err(StoreError::UnsafeDataDir(_))
    ));

    let weakened_packs = crate::private_tempdir().expect("weakened packs root");
    let store = HubStore::initialize(weakened_packs.path()).expect("initial store");
    let packs = store.packs_dir().to_path_buf();
    drop(store);
    fs::set_permissions(&packs, fs::Permissions::from_mode(0o755)).expect("weaken packs mode");
    assert!(matches!(
        HubStore::initialize(weakened_packs.path()),
        Err(StoreError::UnsafePacksDir(_))
    ));
}

#[cfg(unix)]
#[test]
fn private_sqlite_catalogue_is_0600_and_only_tightens_0640() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let path = store.database_path();
    let expected_gid = fs::symlink_metadata(temporary.path())
        .expect("data-root metadata")
        .gid();

    let metadata = fs::symlink_metadata(path).expect("catalogue metadata");
    assert_eq!(metadata.gid(), expected_gid);
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        SHARED_SQLITE_FILE_MODE
    );

    // Tighten SQLite's common 0640 creation mode, but reject any unrelated
    // historical mode rather than silently changing an unknown file.
    fs::set_permissions(path, fs::Permissions::from_mode(0o640))
        .expect("simulate own interrupted SQLite mode");
    ensure_shared_sqlite_catalogue_file(path).expect("repair own 0640 catalogue");
    assert_eq!(
        fs::symlink_metadata(path)
            .expect("repaired catalogue metadata")
            .permissions()
            .mode()
            & 0o777,
        SHARED_SQLITE_FILE_MODE
    );

    fs::set_permissions(path, fs::Permissions::from_mode(0o660))
        .expect("simulate incompatible old catalogue mode");
    assert!(matches!(
        ensure_shared_sqlite_catalogue_file(path),
        Err(StoreError::UnsafeSharedSqlite(_))
    ));
}

#[cfg(unix)]
#[test]
fn schema_22_noop_directory_is_shared_setgid_and_rejects_mode_or_symlink_substitution() {
    let wrong_mode = crate::private_tempdir().expect("wrong-mode store");
    let store = HubStore::initialize(wrong_mode.path()).expect("store initializes");
    let noop = store.packs_dir().join("noop");
    let expected_gid = fs::symlink_metadata(store.packs_dir())
        .expect("packs metadata")
        .gid();
    let metadata = fs::symlink_metadata(&noop).expect("no-op metadata");
    assert_eq!(metadata.gid(), expected_gid);
    assert_eq!(
        metadata.permissions().mode() & 0o7777,
        SHARED_SCHEMA_22_NOOP_DIRECTORY_MODE
    );
    drop(store);
    fs::set_permissions(&noop, fs::Permissions::from_mode(0o770)).expect("weaken setgid contract");
    assert!(matches!(
        HubStore::initialize(wrong_mode.path()),
        Err(StoreError::UnsafeSchema22NoOpPath(_))
    ));

    let symlinked = crate::private_tempdir().expect("symlink store");
    let store = HubStore::initialize(symlinked.path()).expect("store initializes");
    let noop = store.packs_dir().join("noop");
    drop(store);
    fs::remove_dir(&noop).expect("remove empty no-op directory");
    let outside = symlinked.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    std::os::unix::fs::symlink(&outside, &noop).expect("substitute no-op symlink");
    assert!(matches!(
        HubStore::initialize(symlinked.path()),
        Err(StoreError::AccessSchema22NoOp(_)) | Err(StoreError::UnsafeSchema22NoOpPath(_))
    ));
}

#[test]
fn import_spool_and_publication_gate_are_private_children() {
    let root = Path::new("/tmp/tesatlas-user-hub");
    assert_eq!(
        private_import_spool_root(root),
        root.join(PRIVATE_IMPORT_SPOOL_DIRECTORY_NAME)
    );
    assert_eq!(publication_lock_path(root), root.join(".publication.lock"));
}

#[cfg(unix)]
#[test]
fn publication_gate_rejects_an_incompatible_existing_lock_inode() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    fs::set_permissions(
        &store.publication_lock_path,
        fs::Permissions::from_mode(0o640),
    )
    .expect("weaken private lock mode");

    assert!(matches!(
        store.try_acquire_publication_gate(),
        Err(StoreError::UnsafePublicationGate(_))
    ));
}

#[test]
fn supervised_collector_lease_fences_kill_stale_and_recovery_transitions() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    assert_eq!(
        store
            .service_readiness_at(true, 1_000)
            .expect_err("required collector is initially absent")
            .code,
        ReadinessReasonCode::CollectorAbsent
    );

    let first = store
        .acquire_supervised_collector_lease(1_000)
        .expect("first collector lease");
    let first_status = store
        .supervised_collector_lease_status()
        .expect("read first collector lease status")
        .expect("first collector lease status");
    assert!(Uuid::parse_str(&first_status.instance_id).is_ok());
    assert_eq!(first_status.started_at_ms, 1_000);
    assert_eq!(first_status.heartbeat_at_ms, 1_000);
    assert_eq!(
        first_status.lease_until_ms,
        1_000 + SUPERVISED_COLLECTOR_LEASE_MS
    );
    let status_json =
        serde_json::to_value(&first_status).expect("serialize collector lease status");
    let status_object = status_json.as_object().expect("collector status object");
    let mut status_keys = status_object.keys().map(String::as_str).collect::<Vec<_>>();
    status_keys.sort_unstable();
    assert_eq!(
        status_keys,
        ["heartbeatAtMs", "instanceId", "leaseUntilMs", "startedAtMs"]
    );
    let competing_process =
        HubStore::initialize(temporary.path()).expect("second process store handle");
    store
        .service_readiness_at(true, 1_001)
        .expect("live collector is ready without any stream sessions");
    assert!(matches!(
        competing_process.acquire_supervised_collector_lease(1_001),
        Err(StoreError::SupervisedCollectorLeaseHeld)
    ));

    store
        .heartbeat_supervised_collector_lease(
            first,
            SupervisedCollectorState::AuthenticationTerminal,
            2_000,
        )
        .expect("terminal auth heartbeat");
    assert_eq!(
        store
            .service_readiness_at(true, 2_001)
            .expect_err("terminal auth fails readiness")
            .code,
        ReadinessReasonCode::CollectorAuthTerminal
    );
    store
        .heartbeat_supervised_collector_lease(first, SupervisedCollectorState::Active, 3_000)
        .expect("authenticated recovery heartbeat");
    let recovered_status = store
        .supervised_collector_lease_status()
        .expect("read recovered collector lease status")
        .expect("recovered collector lease status");
    assert_eq!(recovered_status.instance_id, first_status.instance_id);
    assert_eq!(recovered_status.started_at_ms, 1_000);
    assert_eq!(recovered_status.heartbeat_at_ms, 3_000);
    assert_eq!(
        recovered_status.lease_until_ms,
        3_000 + SUPERVISED_COLLECTOR_LEASE_MS
    );
    store
        .service_readiness_at(true, 3_001)
        .expect("authenticated recovery restores readiness");

    let expired_at = 3_000 + SUPERVISED_COLLECTOR_LEASE_MS;
    assert_eq!(
        store
            .service_readiness_at(true, expired_at)
            .expect_err("killed collector becomes stale at lease boundary")
            .code,
        ReadinessReasonCode::CollectorStale
    );
    store
        .heartbeat_supervised_collector_lease(
            first,
            SupervisedCollectorState::Active,
            expired_at + 1,
        )
        .expect("delayed owner revives its readiness record");
    assert!(matches!(
        competing_process.acquire_supervised_collector_lease(expired_at + 1),
        Err(StoreError::SupervisedCollectorLeaseHeld)
    ));
    let replacement_at = expired_at + 1 + SUPERVISED_COLLECTOR_LEASE_MS;
    let replacement = competing_process
        .acquire_supervised_collector_lease(replacement_at)
        .expect("replacement takes over stale lease");
    assert!(matches!(
        store.heartbeat_supervised_collector_lease(
            first,
            SupervisedCollectorState::Active,
            replacement_at + 1,
        ),
        Err(StoreError::SupervisedCollectorLeaseLost)
    ));
    store
        .release_supervised_collector_lease(first)
        .expect("stale release is harmless");
    competing_process
        .service_readiness_at(true, replacement_at + 1)
        .expect("stale release cannot clear replacement");
    competing_process
        .release_supervised_collector_lease(replacement)
        .expect("replacement releases exactly its lease");
    assert_eq!(
        store
            .supervised_collector_lease_status()
            .expect("read released collector lease status"),
        None
    );
    assert_eq!(
        store
            .service_readiness_at(true, replacement_at + 1)
            .expect_err("orderly stop is immediately absent")
            .code,
        ReadinessReasonCode::CollectorAbsent
    );
}

#[test]
fn a_stale_collector_lease_release_leaves_the_replacement_row_untouched() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let first = store
        .acquire_supervised_collector_lease(1_000)
        .expect("first collector lease");
    let replacement = store
        .acquire_supervised_collector_lease(1_000 + SUPERVISED_COLLECTOR_LEASE_MS)
        .expect("replacement takes over the expired lease");
    let before = store
        .supervised_collector_lease_status()
        .expect("read replacement status")
        .expect("replacement row");
    store
        .release_supervised_collector_lease(first)
        .expect("stale release is harmless");
    assert_eq!(
        store
            .supervised_collector_lease_status()
            .expect("read status after the stale release"),
        Some(before),
        "the replaced owner's release deletes nothing of the replacement"
    );
    store
        .release_supervised_collector_lease(replacement)
        .expect("replacement releases its own lease");
    assert_eq!(
        store
            .supervised_collector_lease_status()
            .expect("read released status"),
        None
    );
}

#[test]
fn fast_readiness_checks_pack_servability_without_hashing_content() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let manifest = test_manifest();
    let pack = &manifest.chunks[0];
    let path = store
        .packs_dir()
        .join("sha256")
        .join(format!("{}.sqlite.zst", pack.sha256));
    fs::create_dir_all(path.parent().expect("pack parent")).expect("pack parent");
    fs::write(&path, vec![7_u8; 100]).expect("pack file");
    store
        .publish_manifest(&manifest)
        .expect("published manifest");
    store
        .service_readiness_at(false, 1_000)
        .expect("published regular pack is cheaply servable");

    fs::remove_file(&path).expect("remove pack");
    assert_eq!(
        store
            .service_readiness_at(false, 1_000)
            .expect_err("missing published pack")
            .code,
        ReadinessReasonCode::PublishedContentUnservable
    );
    fs::write(&path, vec![7_u8; 99]).expect("truncated pack");
    assert_eq!(
        store
            .service_readiness_at(false, 1_000)
            .expect_err("truncated published pack")
            .code,
        ReadinessReasonCode::PublishedContentUnservable
    );
    fs::remove_file(&path).expect("remove truncated pack");
    fs::create_dir(&path).expect("non-regular pack path");
    assert_eq!(
        store
            .service_readiness_at(false, 1_000)
            .expect_err("non-regular published pack")
            .code,
        ReadinessReasonCode::PublishedContentUnservable
    );
    fs::remove_dir(&path).expect("remove non-regular pack path");

    // The fast probe intentionally does not claim same-size integrity.
    fs::write(&path, vec![8_u8; 100]).expect("same-size corrupt pack");
    store
        .service_readiness_at(false, 1_000)
        .expect("same-size content belongs to the full doctor gate");
    assert!(matches!(
        store.catalogue_check(),
        Err(StoreError::CatalogPackDigestMismatch { .. })
    ));
}

#[test]
fn read_only_catalogue_check_does_not_change_the_store_tree() {
    let temporary = crate::private_tempdir().expect("temporary database");
    HubStore::initialize(temporary.path()).expect("store initializes");
    let before = tree_contents(temporary.path());
    let store = HubStore::open_immutable_read_only(temporary.path()).expect("immutable store");
    assert_eq!(
        store
            .catalogue_inventory()
            .expect("immutable inventory")
            .journal_mode,
        "wal"
    );
    store.catalogue_check().expect("read-only catalogue check");
    assert_eq!(
        store.sqlite_version().expect("read-only SQLite version"),
        BUNDLED_SQLITE_VERSION
    );
    store
        .verify_immutable_snapshot_unchanged()
        .expect("immutable snapshot remains unchanged");
    drop(store);
    assert_eq!(tree_contents(temporary.path()), before);
}

#[test]
fn short_lived_writer_checkpoint_allows_immediate_immutable_preflight() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let imported = s_test_with_credential_connection(&store, |connection| {
        connection.set_test_now_ms(AS_T0 * 1_000);
        s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            Uuid::new_v4(),
            b"synthetic-access",
            b"synthetic-refresh",
            0,
            0,
        )
    })
    .expect("test credentials");
    let reader = store
        .open_read_only_connection()
        .expect("blocking reader opens");
    reader.execute_batch("BEGIN").expect("read transaction");
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
        .expect("read snapshot");
    s_test_with_credential_connection(&store, |connection| {
        connection.replace_credential_pair(
            &imported,
            &EntryProvenance::Supplied,
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::ReplaceAny,
        )?;
        connection.credential_durability_barrier()
    })
    .expect("short-lived write");
    assert!(matches!(
        HubStore::open_immutable_read_only(temporary.path()),
        Err(StoreError::PendingCatalogueWal)
    ));
    drop(reader);

    store
        .checkpoint_catalogue_for_immutable_read()
        .expect("catalogue checkpoint");
    let immutable =
        HubStore::open_immutable_read_only(temporary.path()).expect("immutable preflight opens");
    immutable
        .verify_immutable_snapshot_unchanged()
        .expect("immutable snapshot remains stable");
    drop(immutable);

    assert!(
        store
            .published_vehicles()
            .expect("published vehicles")
            .is_empty()
    );
    assert!(
        store
            .configured_tesla_vehicles()
            .expect("configured vehicles")
            .is_empty()
    );
    assert!(
        store
            .credential_pair_presence(CredentialSlot::OwnerApi)
            .expect("legacy credentials")
            .is_some()
    );
    assert!(matches!(
        store.current_observations_for_vehicle(Uuid::new_v4()),
        Err(StoreError::UnknownVehicle(_))
    ));
    HubStore::open_immutable_read_only(temporary.path())
        .expect("read queries do not create a WAL frame");
}

#[test]
fn read_only_open_rejects_a_stale_schema_without_migrating_it() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let connection = store.open().expect("writable test connection");
    connection
        .execute_batch("PRAGMA user_version = 40;")
        .expect("mark test catalogue stale");
    drop(connection);
    let before = tree_contents(temporary.path());

    assert!(matches!(
        HubStore::open_immutable_read_only(temporary.path()),
        Err(StoreError::UnsupportedSchema(40))
    ));
    assert_eq!(tree_contents(temporary.path()), before);
}

#[test]
fn online_catalogue_backup_restores_through_normal_store_checks() {
    let source_directory = crate::private_tempdir().expect("source directory");
    let store = HubStore::initialize(source_directory.path()).expect("source store");
    let installation_id = store.installation_id().expect("source installation");
    let restore_directory = crate::private_tempdir().expect("restore directory");
    let backup_path = restore_directory.path().join("hub.sqlite");

    store
        .backup_catalogue_to(&backup_path)
        .expect("online backup");
    assert!(backup_path.is_file());
    let restored = HubStore::initialize(restore_directory.path()).expect("restored store");
    restored.quick_check().expect("restored integrity");
    assert_eq!(restored.installation_id().unwrap(), installation_id);
    assert!(matches!(
        store.backup_catalogue_to(&backup_path),
        Err(StoreError::BackupDestinationExists(_))
    ));
}

#[test]
fn complete_backup_copies_catalogue_referenced_pack_set() {
    let source_directory = crate::private_tempdir().expect("source directory");
    let store = HubStore::initialize(source_directory.path()).expect("source store");
    let manifest = test_manifest();
    let pack = &manifest.chunks[0];
    let source_pack = store
        .packs_dir()
        .join("sha256")
        .join(format!("{}.sqlite.zst", pack.sha256));
    fs::create_dir_all(source_pack.parent().expect("pack parent")).expect("pack parent");
    fs::write(&source_pack, vec![7_u8; 100]).expect("source pack");
    store.publish_manifest(&manifest).expect("catalogue pack");

    let backup_parent = crate::private_tempdir().expect("backup parent");
    let backup_root = backup_parent.path().join("backup");
    store.backup_to(&backup_root).expect("complete backup");
    let restored = HubStore::initialize(&backup_root).expect("restored store");
    restored.quick_check().expect("restored integrity");
    let restored_pack = restored
        .pack_for_digest(pack.sha256)
        .expect("restored catalogue")
        .expect("restored pack");
    assert_eq!(fs::read(restored_pack.path).unwrap(), vec![7_u8; 100]);
}

#[test]
fn corrupt_referenced_pack_refuses_and_cleans_backup_root() {
    let source_directory = crate::private_tempdir().expect("source directory");
    let store = HubStore::initialize(source_directory.path()).expect("source store");
    let manifest = test_manifest();
    let source_pack = store
        .packs_dir()
        .join("sha256")
        .join(format!("{}.sqlite.zst", manifest.chunks[0].sha256));
    fs::create_dir_all(source_pack.parent().expect("pack parent")).expect("pack parent");
    fs::write(&source_pack, vec![7_u8; 100]).expect("source pack");
    store.publish_manifest(&manifest).expect("catalogue pack");
    fs::write(&source_pack, vec![8_u8; 100]).expect("corrupt pack");

    let backup_parent = crate::private_tempdir().expect("backup parent");
    let backup_root = backup_parent.path().join("corrupt-backup");
    assert!(matches!(
        store.backup_to(&backup_root),
        Err(StoreError::BackupPackDigestMismatch { .. })
    ));
    assert!(!backup_root.exists());
}

fn design_later_51_store_with_one_pack() -> (tempfile::TempDir, HubStore, std::path::PathBuf) {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let manifest = test_manifest();
    let path = store
        .packs_dir()
        .join("sha256")
        .join(format!("{}.sqlite.zst", manifest.chunks[0].sha256));
    fs::create_dir_all(path.parent().expect("pack parent")).expect("pack parent");
    fs::write(&path, vec![7_u8; 100]).expect("pack file");
    (temporary, store, path)
}

/// Issue #51: the background scan reports a final name nothing references
/// and same-size damage to a referenced object, and changes neither.
#[test]
fn design_later_51_scan_reports_an_orphan_final_and_same_size_damage() {
    let (_temporary, store, path) = design_later_51_store_with_one_pack();
    let manifest = test_manifest();
    store.publish_manifest(&manifest).expect("published manifest");
    let clean = store.reconcile_packs(&|| false).expect("clean scan");
    assert!(clean.complete);
    assert_eq!((clean.final_names, clean.referenced_checked), (1, 1));
    assert!(clean.codes().is_empty(), "{:?}", clean.codes());

    let orphan = path.with_file_name(format!("{}.sqlite.zst", "ab".repeat(32)));
    fs::write(&orphan, b"left by an interrupted installation").expect("orphan final");
    fs::write(&path, vec![8_u8; 100]).expect("same-size damage");
    let report = store.reconcile_packs(&|| false).expect("scan");
    assert!(report.complete);
    assert_eq!(report.final_names, 2);
    assert_eq!(report.unreferenced_finals, 1);
    assert_eq!(report.digest_mismatches, 1);
    assert_eq!(
        report.codes(),
        vec!["pack_unreferenced_final", "pack_object_digest_mismatch"]
    );
    // Reported only: the orphan stays, the damaged object keeps its bytes.
    assert!(orphan.exists());
    assert_eq!(fs::read(&path).expect("damaged pack"), vec![8_u8; 100]);

    fs::remove_file(&path).expect("remove referenced pack");
    let missing = store.reconcile_packs(&|| false).expect("scan");
    assert_eq!(missing.missing_objects, 1);
    assert!(missing.codes().contains(&"pack_object_missing"));
}

/// Issue #51: a cancelled scan says it is incomplete instead of reporting
/// the store clean.
#[test]
fn design_later_51_an_interrupted_scan_reports_incomplete() {
    let (_temporary, store, path) = design_later_51_store_with_one_pack();
    let manifest = test_manifest();
    store.publish_manifest(&manifest).expect("published manifest");
    fs::write(&path, vec![8_u8; 100]).expect("same-size damage");
    for allowed in 0..3 {
        let asked = std::cell::Cell::new(0);
        let report = store
            .reconcile_packs(&|| {
                asked.set(asked.get() + 1);
                asked.get() > allowed
            })
            .expect("interrupted scan");
        assert!(!report.complete, "cancelled after {allowed} checks");
        assert_eq!(report.codes()[0], "pack_reconciliation_incomplete");
        assert_eq!(report.digest_mismatches, 0, "the damage was not reached");
    }
}

/// Issue #51: a final name whose installation commits during the scan, or
/// is still being installed under the publication gate, is not reported as
/// an orphan.
#[test]
fn design_later_51_a_concurrent_installation_is_not_an_orphan() {
    let (_temporary, store, _path) = design_later_51_store_with_one_pack();
    let manifest = test_manifest();
    // The installation holds the gate: the name stays unclassified.
    let gate = store.try_acquire_publication_gate().expect("installer's gate");
    let held = store.reconcile_packs(&|| false).expect("scan");
    assert_eq!((held.unreferenced_finals, held.unclassified_finals), (0, 1));
    assert!(!held.complete);
    assert!(held.codes().contains(&"pack_final_unclassified"));
    drop(gate);
    // The installation commits its catalogue row between the listing and
    // the recheck: the name is referenced by then.
    let committed = store
        .reconcile_packs_with(&|| false, &|| {
            store.publish_manifest(&manifest).expect("published manifest");
        })
        .expect("scan");
    assert_eq!(committed.unreferenced_finals, 0);
    assert!(committed.complete);
}
