// SPDX-License-Identifier: AGPL-3.0-only

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
            params![Uuid::new_v4().to_string(), 0_i64, 10_i64, 10_i64, 10_i64, 10_i64],
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

fn remove_v50_current_observation_schema(connection: &Connection) {
    remove_v55_fleet_schema(connection);
    connection
        .execute_batch(
            "DROP TABLE current_observations;
                 DROP TABLE raw_observation_prune_guard;
                 DROP TRIGGER raw_observations_append_only_delete;
                 CREATE TRIGGER raw_observations_append_only_delete
                 BEFORE DELETE ON raw_observations
                 FOR EACH ROW
                 BEGIN
                     SELECT RAISE(ABORT, 'raw observations are append-only');
                 END;",
        )
        .expect("remove v50 current-observation schema");
}

fn remove_v55_fleet_schema(connection: &Connection) {
    connection
        .execute_batch(
            "DROP TABLE fleet_refresh_input_fences;
                 DROP INDEX fleet_refresh_receipt_output_generation;
                 DROP TABLE fleet_refresh_receipt_bindings;
                 DROP TABLE fleet_tokens;",
        )
        .expect("remove v55 Fleet schema");
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
    assert_eq!(inventory.teslamate_legacy_token_rows, 0);
    assert_eq!(inventory.fleet_token_rows, 0);
    assert_eq!(
        inventory.installation_id,
        store.installation_id().expect("id")
    );
    let before = store
        .load_teslamate_legacy_tokens()
        .expect("legacy tokens")
        .is_some();
    let fleet_before = store.load_fleet_tokens().expect("Fleet tokens").is_some();
    let _ = store
        .catalogue_inventory()
        .expect("inventory is repeatable");
    assert_eq!(
        store
            .load_teslamate_legacy_tokens()
            .expect("legacy tokens after inventory")
            .is_some(),
        before
    );
    assert_eq!(
        store
            .load_fleet_tokens()
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
    assert_eq!(runtime.teslamate_legacy_token_rows, 0);
    assert_eq!(runtime.fleet_token_rows, 0);
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

#[test]
fn teslamate_token_import_loads_and_reopens_as_one_row() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let imported = TeslaMateLegacyTokenStore::imported(
        b"access-ciphertext".to_vec(),
        b"refresh-ciphertext".to_vec(),
    )
    .expect("imported pair is valid");

    store
        .replace_teslamate_legacy_tokens(&imported)
        .expect("imported pair stores");
    let reopened = HubStore::initialize(temporary.path()).expect("store reopens");
    let loaded = reopened
        .load_teslamate_legacy_tokens()
        .expect("pair loads")
        .expect("pair exists");

    assert_eq!(loaded.access(), imported.access());
    assert_eq!(loaded.refresh(), imported.refresh());
    assert_eq!(loaded.expires_at(), 0);
    assert_eq!(loaded.next_refresh_at(), 0);
    let connection = reopened.open().expect("database opens");
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM teslamate_legacy_tokens", [], |row| {
            row.get(0)
        })
        .expect("row count");
    assert_eq!(rows, 1);
}

#[test]
fn teslamate_legacy_token_store_zeroizes_ciphertext() {
    use zeroize::Zeroize;

    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    assert_zeroize_on_drop::<TeslaMateLegacyTokenStore>();

    let mut stored = TeslaMateLegacyTokenStore::refreshed(
        b"access-secret-canary".to_vec(),
        b"refresh-secret-canary".to_vec(),
        2,
        1,
    )
    .expect("stored pair");
    stored.zeroize();
    assert!(stored.access().iter().all(|byte| *byte == 0));
    assert!(stored.refresh().iter().all(|byte| *byte == 0));
    assert_eq!(stored.expires_at(), 0);
    assert_eq!(stored.next_refresh_at(), 0);
}

#[test]
fn teslamate_token_replacement_is_atomic_and_refresh_schedule_is_strict() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let imported = TeslaMateLegacyTokenStore::imported(
        b"old-access-ciphertext".to_vec(),
        b"old-refresh-ciphertext".to_vec(),
    )
    .expect("imported pair is valid");
    store
        .replace_teslamate_legacy_tokens(&imported)
        .expect("imported pair stores");

    assert!(matches!(
        TeslaMateLegacyTokenStore::refreshed(
            b"new-access-ciphertext".to_vec(),
            b"new-refresh-ciphertext".to_vec(),
            1_000,
            1_000,
        ),
        Err(StoreError::InvalidTeslaMateTokenSchedule)
    ));

    let refreshed = TeslaMateLegacyTokenStore::refreshed(
        b"new-access-ciphertext".to_vec(),
        b"new-refresh-ciphertext".to_vec(),
        2_000,
        1_000,
    )
    .expect("refreshed schedule is valid");
    store
        .replace_teslamate_legacy_tokens(&refreshed)
        .expect("replacement commits");
    let loaded = store
        .load_teslamate_legacy_tokens()
        .expect("pair loads")
        .expect("pair exists");
    assert_eq!(loaded.access(), refreshed.access());
    assert_eq!(loaded.refresh(), refreshed.refresh());
    assert_eq!(loaded.expires_at(), 2_000);
    assert_eq!(loaded.next_refresh_at(), 1_000);
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
    let root = Path::new("/tmp/teslatlas-user-hub");
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
    assert_eq!(first_status.lease_until_ms, 1_000 + SUPERVISED_COLLECTOR_LEASE_MS);
    let status_json = serde_json::to_value(&first_status).expect("serialize collector lease status");
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
fn upgrades_v42_with_supervised_collector_lease_schema() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let connection = store.open().expect("current catalogue");
    remove_v50_current_observation_schema(&connection);
    connection
        .execute_batch(
            "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 PRAGMA user_version = 42;",
        )
        .expect("recreate historical v42 boundary");
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v42 store");
    let connection = upgraded.open().expect("upgraded catalogue");
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    let schema: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'supervised_collector_lease'",
            [],
            |row| row.get(0),
        )
        .expect("collector lease schema");
    assert!(schema.contains("auth_terminal"));
    assert!(schema.contains("singleton_id = 1"));
}

#[test]
fn upgrades_v43_with_legacy_refresh_receipt_binding_schema() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let connection = store.open().expect("current catalogue");
    remove_v50_current_observation_schema(&connection);
    connection
        .execute_batch(
            "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 PRAGMA user_version = 43;",
        )
        .expect("recreate historical v43 boundary");
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v43 store");
    let connection = upgraded.open().expect("upgraded catalogue");
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    let schema: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'legacy_refresh_receipt_bindings'",
            [],
            |row| row.get(0),
        )
        .expect("refresh receipt binding schema");
    assert!(schema.contains("output_credential_generation"));
    assert!(schema.contains("ON DELETE CASCADE"));
}

#[test]
fn upgrades_v50_paired_bearers_with_finite_lifetime_and_revocation_columns() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let connection = store.open().expect("current catalogue");
    remove_v55_fleet_schema(&connection);
    connection
        .execute_batch(
            "DROP TABLE paired_devices;
                 CREATE TABLE paired_devices (
                    device_id TEXT PRIMARY KEY NOT NULL,
                    display_name TEXT NOT NULL,
                    token_sha256 BLOB NOT NULL UNIQUE CHECK(length(token_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    last_authenticated_at_ms INTEGER,
                    CHECK(last_authenticated_at_ms IS NULL
                          OR last_authenticated_at_ms >= created_at_ms),
                    CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 128)
                 ) STRICT;
                 PRAGMA user_version = 50;",
        )
        .expect("recreate historical v50 paired-device boundary");
    let device_id = Uuid::new_v4();
    connection
        .execute(
            "INSERT INTO paired_devices(
                    device_id, display_name, token_sha256,
                    created_at_ms, last_authenticated_at_ms
                 ) VALUES (?1, 'legacy phone', ?2, 1000, 2000)",
            params![device_id.to_string(), vec![7_u8; 32]],
        )
        .expect("historical paired device");
    let future_device_id = Uuid::new_v4();
    connection
        .execute(
            "INSERT INTO paired_devices(
                    device_id, display_name, token_sha256,
                    created_at_ms, last_authenticated_at_ms
                 ) VALUES (?1, 'future legacy phone', ?2, ?3, NULL)",
            params![future_device_id.to_string(), vec![8_u8; 32], i64::MAX],
        )
        .expect("future-dated historical paired device");
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v50 store");
    let connection = upgraded.open().expect("upgraded catalogue");
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    let (created, expires, revoked, last_authenticated): (i64, i64, Option<i64>, Option<i64>) =
        connection
            .query_row(
                "SELECT created_at_ms, expires_at_ms, revoked_at_ms,
                            last_authenticated_at_ms
                       FROM paired_devices WHERE device_id = ?1",
                params![device_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("migrated paired device");
    assert_eq!(created, 1000);
    assert!(expires > created);
    assert_eq!(revoked, None);
    assert_eq!(last_authenticated, Some(2000));
    let (future_created, future_expires): (i64, i64) = connection
        .query_row(
            "SELECT created_at_ms, expires_at_ms
                   FROM paired_devices WHERE device_id = ?1",
            params![future_device_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("future-dated paired device migrates");
    assert_eq!(future_created, i64::MAX - 1);
    assert_eq!(future_expires, i64::MAX);
}

#[test]
fn upgrades_v51_token_row_then_binds_generation_after_authenticated_decryption() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let key_bytes = b"v51 exact TeslaMate key";
    crate::teslamate_credentials::replace_key(temporary.path(), key_bytes).expect("private key");
    let plaintext = crate::credentials::OwnerTokens::from_secret_parts(
        "v51-access".to_owned(),
        "v51-refresh".to_owned(),
    )
    .expect("plaintext pair");
    let expected_generation =
        crate::teslamate_token::legacy_refresh_credential_generation(&plaintext);
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key_bytes, &plaintext)
            .expect("encrypt v51 pair");
    let connection = store.open().expect("current catalogue");
    remove_v55_fleet_schema(&connection);
    connection
        .execute_batch(
            "DROP TABLE teslamate_legacy_tokens;
                 CREATE TABLE teslamate_legacy_tokens (
                    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                    access BLOB NOT NULL CHECK(length(access) > 0),
                    refresh BLOB NOT NULL CHECK(length(refresh) > 0),
                    expires_at INTEGER NOT NULL CHECK(expires_at >= 0),
                    next_refresh_at INTEGER NOT NULL CHECK(next_refresh_at >= 0),
                    CHECK(expires_at > next_refresh_at AND next_refresh_at > 0)
                 ) STRICT;
                 PRAGMA user_version = 51;",
        )
        .expect("recreate historical v51 token table");
    connection
        .execute(
            "INSERT INTO teslamate_legacy_tokens(
                    singleton_id, access, refresh, expires_at, next_refresh_at
                 ) VALUES (1, ?1, ?2, 2000000000, 1900000000)",
            params![access, refresh],
        )
        .expect("historical pair");
    drop(connection);
    drop(store);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v51 store");
    assert_eq!(
        upgraded
            .load_teslamate_legacy_tokens()
            .expect("upgraded pair loads")
            .expect("upgraded pair")
            .credential_generation(),
        None
    );
    let issuer = url::Url::parse("http://127.0.0.1/").expect("test issuer");
    let manager = crate::credentials::LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        upgraded.clone(),
        temporary.path(),
        issuer.clone(),
    )
    .expect("authenticated decrypt binds generation");
    assert_eq!(manager.access_token(), "v51-access");
    assert_eq!(
        upgraded
            .load_teslamate_legacy_tokens()
            .expect("bound pair loads")
            .expect("bound pair")
            .credential_generation(),
        Some(expected_generation)
    );
    drop(manager);
    crate::credentials::LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        upgraded,
        temporary.path(),
        issuer,
    )
    .expect("bound generation reopens");
}

#[test]
fn v51_noncanonical_credential_generation_column_is_not_stamped_v52() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let connection = store.open().expect("current catalogue");
    remove_v55_fleet_schema(&connection);
    connection
        .execute_batch(
            "DROP TABLE teslamate_legacy_tokens;
                 CREATE TABLE teslamate_legacy_tokens (
                    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                    access BLOB NOT NULL,
                    refresh BLOB NOT NULL,
                    expires_at INTEGER NOT NULL,
                    next_refresh_at INTEGER NOT NULL,
                    credential_generation INTEGER
                 ) STRICT;
                 INSERT INTO teslamate_legacy_tokens(
                    singleton_id, access, refresh, expires_at, next_refresh_at,
                    credential_generation
                 ) VALUES (1, x'01', x'02', 2000, 1750, 7);
                 PRAGMA user_version = 51;",
        )
        .expect("recreate noncanonical v51 table");
    drop(connection);
    drop(store);

    assert!(matches!(
        HubStore::initialize(temporary.path()),
        Err(StoreError::Migrate(_))
    ));
    let connection = Connection::open(temporary.path().join("hub.sqlite"))
        .expect("inspect rolled-back catalogue");
    assert_eq!(schema_version(&connection).expect("schema version"), 51);
}

#[test]
fn upgrades_v52_lifecycle_open_row_key_to_include_vehicle() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let (source, vehicle) = test_registered_vehicle(&store);
    let row = crate::teslamate_projection::TeslaMateState {
        id: 1,
        car_id: 10,
        state: "online".into(),
        start_date_ms: 1_000,
        end_date_ms: None,
    };
    let row_json = serde_json::to_string(&row).expect("state JSON");
    let connection = store.open().expect("current catalogue");
    remove_v55_fleet_schema(&connection);
    connection
        .execute_batch(
            "DROP TABLE lifecycle_open_rows;
                 CREATE TABLE lifecycle_open_rows (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    source_table TEXT NOT NULL,
                    source_row_id INTEGER NOT NULL CHECK(source_row_id > 0),
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    domain TEXT NOT NULL CHECK(domain IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state',
                        'standalone_position'
                    )),
                    parent_source_row_id INTEGER,
                    row_json TEXT NOT NULL CHECK(json_valid(row_json)),
                    PRIMARY KEY(source_id, source_table, source_row_id)
                 ) STRICT;
                 CREATE INDEX lifecycle_open_rows_vehicle_domain
                    ON lifecycle_open_rows(vehicle_id, domain, source_row_id);
                 PRAGMA user_version = 52;",
        )
        .expect("recreate v52 open-row key");
    connection
        .execute(
            "INSERT INTO lifecycle_open_rows(
                    source_id, source_table, source_row_id, vehicle_id, car_id,
                    domain, parent_source_row_id, row_json
                 ) VALUES (?1, 'states', 1, ?2, 10, 'state', NULL, ?3)",
            params![
                source.source_id.to_string(),
                vehicle.vehicle_id.to_string(),
                row_json
            ],
        )
        .expect("historical open row");
    drop(connection);
    drop(store);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v52 store");
    let connection = upgraded.open().expect("upgraded catalogue");
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    let schema: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'lifecycle_open_rows'",
            [],
            |row| row.get(0),
        )
        .expect("open-row schema");
    assert!(schema.contains("PRIMARY KEY(source_id, vehicle_id, source_table, source_row_id)"));
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM lifecycle_open_rows", [], |row| {
            row.get(0)
        })
        .expect("preserved open rows");
    assert_eq!(rows, 1);
}

#[test]
fn unbound_historical_legacy_receipt_does_not_block_credential_recovery() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let connection = store.open().expect("current catalogue");
    remove_v55_fleet_schema(&connection);
    let future_start = i64::MAX - 1;
    connection
        .execute(
            "INSERT INTO outbound_request_receipts(
                    correlation_id, started_at_ms, vehicle_tesla_id, transport,
                    operation, safety_class, precondition, outcome
                 ) VALUES (?1, ?2, NULL, 'legacy_auth', 'token_refresh',
                           'non_wake_endpoint', 'not_required', 'started')",
            params![Uuid::new_v4().to_string(), future_start],
        )
        .expect("historical unbound receipt");
    let receipt_id = connection.last_insert_rowid();
    connection
        .execute_batch("PRAGMA user_version = 51;")
        .expect("historical schema boundary");
    drop(connection);
    drop(store);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v51 store");
    assert!(
        !upgraded
            .has_unresolved_legacy_refresh()
            .expect("unbound row is not refresh state")
    );
    let (outcome, completed, duration): (String, i64, i64) = upgraded
        .open()
        .expect("upgraded catalogue")
        .query_row(
            "SELECT outcome, completed_at_ms, duration_ms
                   FROM outbound_request_receipts WHERE id = ?1",
            params![receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("historical receipt terminalized");
    assert_eq!(outcome, "cancelled");
    assert_eq!(completed, future_start);
    assert_eq!(duration, 0);

    let key = b"fresh recovery key";
    let plaintext = crate::credentials::OwnerTokens::from_secret_parts(
        "fresh-access".to_owned(),
        "fresh-refresh".to_owned(),
    )
    .expect("fresh plaintext");
    let (access, refresh) = crate::teslamate_token::encrypt_legacy_owner_tokens(key, &plaintext)
        .expect("fresh ciphertext");
    let fresh = TeslaMateLegacyTokenStore::imported(access, refresh).expect("fresh pair");
    crate::teslamate_credentials::replace_key_and_tokens(temporary.path(), &upgraded, key, &fresh)
        .expect("fresh credentials recover");
    let generation = upgraded
        .load_teslamate_legacy_tokens()
        .expect("fresh credentials load")
        .expect("fresh credentials")
        .credential_generation()
        .expect("fresh generation");
    let receipt = upgraded
        .begin_legacy_refresh(generation)
        .expect("fresh generation can begin refresh");
    upgraded
        .cancel_unsent_legacy_refresh(receipt, generation)
        .expect("test cleanup");
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
    let imported = TeslaMateLegacyTokenStore::imported(
        b"access-ciphertext".to_vec(),
        b"refresh-ciphertext".to_vec(),
    )
    .expect("test credentials");
    let reader = store
        .open_read_only_connection()
        .expect("blocking reader opens");
    reader.execute_batch("BEGIN").expect("read transaction");
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
        .expect("read snapshot");
    store
        .replace_teslamate_legacy_tokens(&imported)
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
            .load_teslamate_legacy_tokens()
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
