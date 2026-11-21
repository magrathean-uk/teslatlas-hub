// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{
    db::{SourceDescriptor, VehicleDescriptor},
    protocol::{
        CursorClaims, CursorKey, LineageBase, LineageCapability, LineageDelta, LineageManifestV2,
        MirrorTable, OpaqueCursor, PackCompression, PackFormat, SchemaVersion, SequenceRange,
        Sha256Digest, TransportPack, canonical_delta_chain_digest,
    },
};

#[test]
fn private_backup_reader_rejects_a_fifo_without_waiting_for_a_writer() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("backup-v4.json");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("run mkfifo")
            .success()
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .expect("FIFO permissions");

    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        sender
            .send(read_bounded_private_file(&path, MAX_MANIFEST_BYTES).is_err())
            .expect("send FIFO result");
    });
    assert!(
        receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("backup FIFO admission must not block")
    );
}

#[derive(Debug, PartialEq, Eq)]
struct SnapshotEntry {
    path: String,
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    sha256: Option<String>,
}

fn tree_snapshot(root: &Path) -> Vec<SnapshotEntry> {
    fn visit(root: &Path, current: &Path, output: &mut Vec<SnapshotEntry>) {
        for entry in fs::read_dir(current).expect("read snapshot directory") {
            let entry = entry.expect("snapshot entry");
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("snapshot metadata");
            let relative = path
                .strip_prefix(root)
                .expect("snapshot prefix")
                .to_string_lossy()
                .into_owned();
            if metadata.file_type().is_dir() {
                output.push(SnapshotEntry {
                    path: relative,
                    mode: permission_mode(&metadata),
                    modified_seconds: metadata.mtime(),
                    modified_nanoseconds: metadata.mtime_nsec(),
                    sha256: None,
                });
                visit(root, &path, output);
            } else {
                output.push(SnapshotEntry {
                    path: relative,
                    mode: permission_mode(&metadata),
                    modified_seconds: metadata.mtime(),
                    modified_nanoseconds: metadata.mtime_nsec(),
                    sha256: Some(sha256_file_hex(&path).expect("snapshot digest")),
                });
            }
        }
    }
    let root_metadata = fs::symlink_metadata(root).expect("snapshot root metadata");
    let mut output = vec![SnapshotEntry {
        path: ".".to_owned(),
        mode: permission_mode(&root_metadata),
        modified_seconds: root_metadata.mtime(),
        modified_nanoseconds: root_metadata.mtime_nsec(),
        sha256: None,
    }];
    visit(root, root, &mut output);
    output.sort_by(|left, right| left.path.cmp(&right.path));
    output
}

fn create_fixture() -> (tempfile::TempDir, HubStore) {
    let temporary = tempfile::tempdir().expect("fixture parent");
    let data = temporary.path().join("source-data");
    let store = HubStore::initialize(&data).expect("source store");
    store
        .create_pairing("Recovery fixture", 1_000, 10_000)
        .expect("pairing database row");
    (temporary, store)
}

fn publish_lineage_fixture(store: &HubStore, cursor_key: &CursorKey) -> LineageManifestV2 {
    let source = store
        .register_source(
            &SourceDescriptor::new("tesla_owner_api", "recovery-account"),
            1_000,
        )
        .expect("recovery account source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "recovery-vehicle"),
            1_001,
        )
        .expect("recovery vehicle");
    let installation_id = store.installation_id().expect("installation identity");
    let base_snapshot_id = Uuid::new_v4();
    let make_pack = |ordinal: u32, sequence: SequenceRange, bytes: &[u8]| -> TransportPack {
        let digest = Sha256Digest::of_bytes(bytes);
        TransportPack {
            pack_id: Uuid::new_v4(),
            snapshot_id: base_snapshot_id,
            ordinal,
            schema: SchemaVersion { major: 1, minor: 0 },
            format: PackFormat::SqliteTransport,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(digest),
            sha256: digest,
            compressed_bytes: u64::try_from(bytes.len()).expect("pack size"),
            uncompressed_bytes: 100,
            row_count: 1,
            sequence,
            tables: vec![MirrorTable::Vehicle],
        }
    };
    let base_bytes = b"recovery-base-pack";
    let delta_bytes = b"recovery-delta-pack";
    let base_pack = make_pack(
        0,
        SequenceRange {
            from_exclusive: 10,
            to_inclusive: 10,
        },
        base_bytes,
    );
    let delta_pack = make_pack(
        1,
        SequenceRange {
            from_exclusive: 10,
            to_inclusive: 11,
        },
        delta_bytes,
    );
    let base_digest = Sha256Digest::of_bytes(b"recovery-base-chain");
    let head_digest = canonical_delta_chain_digest(base_digest, delta_pack.sha256);
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: SchemaVersion { major: 1, minor: 0 },
            installation_id,
            account_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: 1,
            sequence: 11,
        },
    )
    .expect("lineage cursor");
    let lineage = LineageManifestV2 {
        protocol: LINEAGE_PROTOCOL_V2,
        capability: LineageCapability::ImmutableBaseOrderedDeltas,
        schema: SchemaVersion { major: 1, minor: 0 },
        installation_id,
        account_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: 1,
        base: LineageBase {
            snapshot_id: base_snapshot_id,
            sequence: 10,
            digest: base_digest,
            packs: vec![base_pack.clone()],
        },
        deltas: vec![LineageDelta {
            from_sequence: 10,
            to_sequence: 11,
            parent_chain_digest: base_digest,
            chain_digest: head_digest,
            pack_digest: delta_pack.sha256,
            pack: delta_pack.clone(),
        }],
        head_sequence: 11,
        head_digest,
        terminal_cursor,
    };
    let pack_directory = store.packs_dir().join("sha256");
    fs::create_dir_all(&pack_directory).expect("lineage pack directory");
    for (pack, bytes) in [
        (&base_pack, base_bytes.as_slice()),
        (&delta_pack, delta_bytes),
    ] {
        fs::write(
            pack_directory.join(format!("{}.sqlite.zst", pack.sha256)),
            bytes,
        )
        .expect("lineage pack");
    }
    store
        .commit_lineage_catalog(&lineage)
        .expect("lineage catalogue");
    store
        .open()
        .expect("lineage binding catalogue")
        .execute(
            "INSERT INTO v2_base_bindings(
                vehicle_id, snapshot_id, installation_id, account_id,
                generation, selected_car_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                vehicle.vehicle_id.to_string(),
                base_snapshot_id.to_string(),
                installation_id.to_string(),
                source.source_id.to_string(),
                1_i64,
                1_i64,
            ],
        )
        .expect("immutable lineage binding");
    lineage
}

/// The restored tree's `secrets` holds only the cursor key restore generated
/// (M06-T09): a fresh 32-byte key, never `old`, and no credential key.
fn assert_only_a_fresh_cursor_key(restored: &Path, old: Option<&[u8]>) -> CursorKey {
    let names: Vec<String> = fs::read_dir(restored.join("secrets"))
        .expect("restored secrets")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["hub-cursor.key"], "secrets holds only the new cursor key");
    let bytes = fs::read(crate::legacy_owner_credentials::cursor_key_path(restored)).unwrap();
    if let Some(old) = old {
        assert_ne!(bytes.as_slice(), old, "restore regenerates the cursor key");
    }
    CursorKey::from_bytes(bytes.as_slice().try_into().expect("32-byte key"))
}

/// `cursor` re-issued under `new` with the claims `old` signed.
fn reissued(cursor: &OpaqueCursor, old: &CursorKey, new: &CursorKey) -> OpaqueCursor {
    OpaqueCursor::issue(new, cursor.verify(old).expect("old cursor")).expect("re-issued")
}

fn pairing_authority_counts(data: &Path) -> (i64, i64) {
    let connection = open_immutable_catalogue(data).expect("pairing authority catalogue");
    let challenges = connection
        .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
            row.get(0)
        })
        .expect("pairing challenge count");
    let devices = connection
        .query_row("SELECT COUNT(*) FROM paired_devices", [], |row| row.get(0))
        .expect("paired device count");
    (challenges, devices)
}

fn catalogue_has_table(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )
        .expect("table existence")
}

/// Rows of the v68 pairing tables (grants, challenge vehicles) and the rotation
/// grace table; zero for a table the catalogue's schema does not have.
fn grant_authority_rows(data: &Path) -> (i64, i64, i64) {
    let connection = open_immutable_catalogue(data).expect("grant authority catalogue");
    let count = |table: &str| -> i64 {
        if !catalogue_has_table(&connection, table) {
            return 0;
        }
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("authority row count")
    };
    (
        count("device_vehicle_grants"),
        count("pairing_challenge_vehicles"),
        count("paired_device_token_grace"),
    )
}

fn reseal_backup_as_legacy_v3_with_pairing(backup: &Path, bearer: &str) {
    let catalogue = backup.join(CATALOGUE_MEMBER);
    let connection = Connection::open_with_flags(
        &catalogue,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("legacy catalogue fixture");
    let pairing_id = Uuid::new_v4().to_string();
    let device_id = Uuid::new_v4().to_string();
    connection
        .execute(
            "INSERT INTO pairing_challenges
             (pairing_id, label, secret_sha256, created_at_ms, expires_at_ms)
             VALUES (?1, 'legacy invitation', ?2, 1000, 10000)",
            rusqlite::params![pairing_id, vec![17_u8; 32]],
        )
        .expect("legacy pairing challenge");
    connection
        .execute(
            "INSERT INTO paired_devices
             (device_id, display_name, token_sha256, created_at_ms, expires_at_ms,
              revoked_at_ms, last_authenticated_at_ms)
             VALUES (?1, 'legacy device', ?2, 2000, 20000, NULL, 2001)",
            rusqlite::params![device_id, Sha256::digest(bearer.as_bytes()).to_vec()],
        )
        .expect("legacy paired device");
    // Schema 68 and later also carry the device's grants, the challenge's
    // vehicles and a rotation grace row; give each one row per fixture
    // vehicle. An older catalogue has none of these tables.
    if catalogue_has_table(&connection, "device_vehicle_grants") {
        connection
            .execute(
                "INSERT INTO device_vehicle_grants (device_id, vehicle_id, granted_at_ms)
                 SELECT ?1, vehicle_id, 2000 FROM vehicles",
                [&device_id],
            )
            .expect("legacy device grants");
        connection
            .execute(
                "INSERT INTO pairing_challenge_vehicles (pairing_id, vehicle_id)
                 SELECT ?1, vehicle_id FROM vehicles",
                [&pairing_id],
            )
            .expect("legacy challenge vehicles");
        connection
            .execute(
                "INSERT INTO paired_device_token_grace (device_id, token_sha256, valid_until_ms)
                 VALUES (?1, ?2, 99999)",
                rusqlite::params![device_id, vec![29_u8; 32]],
            )
            .expect("legacy rotation grace");
        // Rows whose parent is already gone (planted with foreign keys off):
        // no cascade from `paired_devices`, `pairing_challenges` or `vehicles`
        // can reach them, so only the restore's own explicit deletes do.
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 INSERT INTO device_vehicle_grants (device_id, vehicle_id, granted_at_ms)
                     VALUES ('orphan-device', 'orphan-vehicle', 1);
                 INSERT INTO pairing_challenge_vehicles (pairing_id, vehicle_id)
                     VALUES ('orphan-pairing', 'orphan-vehicle');
                 INSERT INTO paired_device_token_grace (device_id, token_sha256, valid_until_ms)
                     VALUES ('orphan-device', zeroblob(32), 1);
                 PRAGMA foreign_keys = ON;",
            )
            .expect("orphaned grant authority");
    }
    drop(connection);
    reseal_backup_as_legacy_v3(backup);
}

/// Seal the backup's manifest and marker as the legacy v3 format over the
/// catalogue as it is now; a manifest already sealed as legacy is sealed again.
/// The declared schema stays as the manifest has it.
fn reseal_backup_as_legacy_v3(backup: &Path) {
    let catalogue = backup.join(CATALOGUE_MEMBER);
    let current_manifest_path = backup.join(MANIFEST_NAME);
    let legacy_manifest_path = backup.join(LEGACY_MANIFEST_NAME);
    let source_manifest_path = if current_manifest_path.exists() {
        current_manifest_path.clone()
    } else {
        legacy_manifest_path.clone()
    };
    let mut manifest: BackupManifest =
        parse_canonical_json(&fs::read(&source_manifest_path).expect("manifest bytes"))
            .expect("manifest");
    manifest.kind = LEGACY_BACKUP_KIND.to_owned();
    manifest.scope = LEGACY_BACKUP_SCOPE.to_owned();
    let catalogue_member = manifest
        .members
        .iter_mut()
        .find(|member| member.path == CATALOGUE_MEMBER)
        .expect("legacy catalogue member");
    catalogue_member.size = fs::metadata(&catalogue)
        .expect("legacy catalogue metadata")
        .len();
    catalogue_member.sha256 = sha256_file_hex(&catalogue).expect("legacy catalogue digest");
    if source_manifest_path == current_manifest_path {
        fs::rename(&current_manifest_path, &legacy_manifest_path)
            .expect("legacy manifest filename");
    }
    let manifest_bytes = canonical_json(&manifest).expect("legacy manifest bytes");
    fs::write(&legacy_manifest_path, &manifest_bytes).expect("legacy manifest");
    set_mode(
        &legacy_manifest_path,
        0o600,
        "legacy manifest mode",
    )
    .unwrap();

    let marker = CompletionMarker {
        kind: LEGACY_COMPLETION_KIND.to_owned(),
        generation: manifest.generation,
        manifest_sha256: sha256_bytes_hex(&manifest_bytes),
    };
    let marker_path = backup.join(COMPLETION_NAME);
    fs::write(&marker_path, canonical_json(&marker).unwrap()).expect("legacy marker");
    set_mode(&marker_path, 0o600, "legacy marker mode").unwrap();
}

/// Re-seal the manifest, its catalogue member and the completion marker over
/// the backup's catalogue as it is now, declaring `schema`.
fn reseal_backup_manifest(backup: &Path, schema: i32) -> BackupManifest {
    let catalogue = backup.join(CATALOGUE_MEMBER);
    let manifest_path = backup.join(MANIFEST_NAME);
    let mut manifest: BackupManifest =
        parse_canonical_json(&fs::read(&manifest_path).expect("manifest bytes"))
            .expect("typed manifest");
    manifest.hub_schema = schema;
    let catalogue_member = manifest
        .members
        .iter_mut()
        .find(|member| member.path == CATALOGUE_MEMBER)
        .expect("catalogue member");
    catalogue_member.size = fs::metadata(&catalogue).expect("catalogue metadata").len();
    catalogue_member.sha256 = sha256_file_hex(&catalogue).expect("catalogue digest");
    let manifest_bytes = canonical_json(&manifest).expect("historical manifest");
    fs::write(&manifest_path, &manifest_bytes).expect("write historical manifest");
    set_mode(
        &manifest_path,
        0o600,
        "historical manifest mode",
    )
    .unwrap();

    let marker = CompletionMarker {
        kind: COMPLETION_KIND.to_owned(),
        generation: manifest.generation,
        manifest_sha256: sha256_bytes_hex(&manifest_bytes),
    };
    let marker_path = backup.join(COMPLETION_NAME);
    fs::write(&marker_path, canonical_json(&marker).unwrap()).expect("write historical marker");
    set_mode(&marker_path, 0o600, "historical marker mode").unwrap();
    manifest
}

#[test]
fn staging_capacity_admission_rejects_simulated_low_space() {
    let copy_bytes = 101_u64;
    let required = staging_required_bytes(copy_bytes).expect("required staged bytes");
    assert_eq!(
        required,
        copy_bytes
            + (copy_bytes / COPY_CAPACITY_HEADROOM_DIVISOR + 1)
            + COPY_CAPACITY_FIXED_HEADROOM_BYTES
    );
    let parent = Path::new("/simulated-capacity-parent");
    assert!(matches!(
        admit_known_capacity(parent, required, required - 1),
        Err(DataRecoveryError::InsufficientFreeSpace {
            path,
            required: actual_required,
            available,
        }) if path == parent && actual_required == required && available == required - 1
    ));
    admit_known_capacity(parent, required, required).expect("exact capacity accepted");
}

#[test]
fn staging_capacity_calculation_rejects_overflow() {
    assert!(matches!(
        staging_required_bytes(u64::MAX),
        Err(DataRecoveryError::CapacityOverflow)
    ));
}

#[test]
fn backup_capacity_excludes_uncatalogued_pack_and_symlink_files() {
    let (temporary, store) = create_fixture();
    let before = store.backup_copy_bytes().expect("catalogued copy bytes");
    let sha = store.packs_dir().join("sha256");
    fs::create_dir_all(&sha).expect("pack directory");
    fs::write(sha.join("orphan.sqlite.zst"), vec![0_u8; 4096]).expect("write orphan pack");
    let outside = temporary.path().join("outside-pack");
    fs::write(&outside, b"must not be counted through symlink").expect("write outside");
    std::os::unix::fs::symlink(&outside, sha.join("linked.sqlite.zst"))
        .expect("create capacity symlink");

    assert_eq!(
        store
            .backup_copy_bytes()
            .expect("unchanged exact copy bytes"),
        before
    );
}

#[test]
fn backup_member_admission_rejects_before_creating_staging_payload() {
    let (temporary, store) = create_fixture();
    let key = CursorKey::from_bytes([9; 32]);
    publish_lineage_fixture(&store, &key);
    store
        .catalogue_check()
        .expect("valid catalogue exceeds injected archive cap");
    let members = store
        .begin_backup_snapshot()
        .expect("snapshot")
        .copy_admission()
        .expect("admission")
        .1;
    assert_eq!(members, 3);
    let mut before: Vec<_> = fs::read_dir(temporary.path())
        .expect("parent")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    before.sort();
    let destination = temporary.path().join("too-many-members");
    let error =
        create_data_backup_with_member_limit(&store, &destination, 2).expect_err("member limit");
    assert!(error.to_string().contains("too many members"));
    assert!(!destination.exists());
    let mut after: Vec<_> = fs::read_dir(temporary.path())
        .expect("parent")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    after.sort();
    assert_eq!(
        before, after,
        "rejection precedes staged catalogue and pack copies"
    );
    create_data_backup_with_member_limit(&store, &destination, 3).expect("exact boundary backup");
    assert_eq!(
        inventory_payload(&destination.join(DATA_DIRECTORY))
            .expect("inventory")
            .len(),
        members
    );
}

#[test]
fn backup_member_admission_deduplicates_current_and_retained_packs_with_noop() {
    let (temporary, store) = create_fixture();
    let key = CursorKey::from_bytes([9; 32]);
    let current = publish_lineage_fixture(&store, &key);
    let mut retained = current.clone();
    let retained_bytes = b"recovery-retained-only-pack";
    let retained_digest = Sha256Digest::of_bytes(retained_bytes);
    retained.deltas[0].pack.pack_id = Uuid::new_v4();
    retained.deltas[0].pack.sha256 = retained_digest;
    retained.deltas[0].pack.relative_path = TransportPack::canonical_relative_path(retained_digest);
    retained.deltas[0].pack.compressed_bytes = retained_bytes.len() as u64;
    retained.deltas[0].pack_digest = retained_digest;
    retained.deltas[0].chain_digest =
        canonical_delta_chain_digest(retained.base.digest, retained_digest);
    retained.head_digest = retained.deltas[0].chain_digest;
    retained.validate().expect("valid retained fixture lineage");
    fs::write(
        store
            .packs_dir()
            .join("sha256")
            .join(format!("{retained_digest}.sqlite.zst")),
        retained_bytes,
    )
    .expect("retained-only pack");
    let now_ms = current_epoch_ms().expect("retention clock");
    let connection = store.open().expect("retention catalogue");
    connection
        .execute(
            "INSERT INTO sync_retired_lineages(
                vehicle_id, head_digest, manifest_json, retired_at_ms, expires_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                retained.vehicle_id.to_string(),
                retained.head_digest.to_string(),
                serde_json::to_vec(&retained).expect("retained manifest"),
                now_ms,
                now_ms + 24 * 60 * 60 * 1_000,
            ],
        )
        .expect("retain prior lineage");
    for pack in [&retained.base.packs[0], &retained.deltas[0].pack] {
        connection
            .execute(
                "INSERT INTO sync_retired_lineage_packs(
                    vehicle_id, head_digest, pack_digest, relative_path, compressed_bytes
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    retained.vehicle_id.to_string(),
                    retained.head_digest.to_string(),
                    pack.sha256.to_string(),
                    pack.relative_path,
                    pack.compressed_bytes as i64,
                ],
            )
            .expect("retained pack binding");
    }
    drop(connection);
    let (physical, noop) = publish_schema_22_fixture(&store);
    store.catalogue_check().expect("valid mixed backup source");
    let members = store
        .begin_backup_snapshot()
        .expect("mixed backup snapshot")
        .copy_admission()
        .expect("mixed member admission")
        .1;
    assert_eq!(
        members, 6,
        "one catalogue, four unique packs, and one current no-op"
    );

    let parent_metadata = fs::metadata(temporary.path()).expect("backup parent metadata");
    let parent_modified = (parent_metadata.mtime(), parent_metadata.mtime_nsec());
    let mut parent_entries: Vec<_> = fs::read_dir(temporary.path())
        .expect("backup parent")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    parent_entries.sort();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let destination = temporary.path().join("mixed-backup");
    let error = create_data_backup_with_member_limit(&store, &destination, members - 1)
        .expect_err("retained pack and no-op exceed injected cap");
    assert!(error.to_string().contains("too many members"));
    assert!(!destination.exists());
    let after_metadata = fs::metadata(temporary.path()).expect("unchanged backup parent");
    assert_eq!(
        (after_metadata.mtime(), after_metadata.mtime_nsec()),
        parent_modified,
        "early rejection must not create and then remove a staging directory"
    );
    let mut after_entries: Vec<_> = fs::read_dir(temporary.path())
        .expect("backup parent after rejection")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    after_entries.sort();
    assert_eq!(after_entries, parent_entries);

    let report = create_data_backup_with_member_limit(&store, &destination, members)
        .expect("deduplicated exact boundary backup");
    assert_eq!(report.member_count, members);
    let actual_paths: BTreeSet<_> = inventory_payload(&destination.join(DATA_DIRECTORY))
        .expect("mixed copied inventory")
        .into_iter()
        .map(|member| member.path)
        .collect();
    let noop_path = format!(
        "{SCHEMA_22_NOOP_DIRECTORY}/{}.{}.json",
        physical.vehicle_id, physical.snapshot_id
    );
    let mut expected_paths = BTreeSet::from([CATALOGUE_MEMBER.to_owned(), noop_path.clone()]);
    for digest in [
        current.base.packs[0].sha256,
        current.deltas[0].pack_digest,
        retained_digest,
        physical.chunks[0].sha256,
    ] {
        expected_paths.insert(format!("{PACK_DIRECTORY}/{digest}.sqlite.zst"));
    }
    assert_eq!(actual_paths, expected_paths);
    assert_eq!(
        fs::read(destination.join(noop_path)).expect("copied current no-op"),
        serde_json::to_vec(&noop).expect("exact no-op bytes")
    );
    verify_data_backup(&destination).expect("mixed retained/current backup verifies");
}

#[test]
fn backup_member_admission_counts_physical_noop_and_ignores_orphans() {
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let members = store
        .begin_backup_snapshot()
        .expect("snapshot")
        .copy_admission()
        .expect("admission")
        .1;
    assert_eq!(members, 3, "catalogue, physical pack, and current no-op");
    fs::write(
        store.packs_dir().join("sha256").join("orphan.sqlite.zst"),
        b"orphan",
    )
    .expect("orphan");
    assert_eq!(
        store
            .begin_backup_snapshot()
            .expect("snapshot")
            .copy_admission()
            .expect("admission")
            .1,
        members
    );
    let destination = temporary.path().join("physical-backup");
    create_data_backup_with_member_limit(&store, &destination, members).expect("physical backup");
    assert_eq!(
        inventory_payload(&destination.join(DATA_DIRECTORY))
            .expect("inventory")
            .len(),
        members
    );
}

fn publish_schema_22_fixture(store: &HubStore) -> (SyncManifest, SignedNoOpState) {
    let data = store.database_path().parent().expect("fixture data root");
    let cursor_key = crate::legacy_owner_credentials::load_or_create_cursor_key(data)
        .expect("fixture cursor key");
    let (built, snapshot) =
        crate::updates_delivery::write_updates_schema_22_pack(store.packs_dir(), Vec::new())
            .expect("write schema 2.2 backup fixture pack");
    let request = crate::updates_delivery::updates_pack_request(&snapshot);
    let manifest =
        crate::updates_delivery::sign_updates_schema_22_manifest(&request, &built, &cursor_key)
            .expect("sign schema 2.2 backup fixture manifest");
    let noop = crate::updates_delivery::sign_updates_schema_22_noop(
        &request.binding,
        request.snapshot_id,
        request.sequence.to_inclusive,
        &built.metadata.sha256.to_string(),
        &cursor_key,
    )
    .expect("sign schema 2.2 backup fixture no-op");
    crate::updates_delivery::publish_updates_schema_22(store, &manifest, &noop)
        .expect("publish schema 2.2 backup fixture pair");
    (manifest, noop)
}

#[test]
fn data_backup_revokes_pairing_authority_and_preserves_identity_and_lineage() {
    let (temporary, store) = create_fixture();
    let source_data = store.database_path().parent().expect("source data root");
    let device_pairing = store
        .create_pairing("Recovery paired device", 2_000, 20_000)
        .expect("device pairing");
    let paired_access = store
        .claim_pairing(
            device_pairing.pairing_id,
            device_pairing.secret(),
            "Recovery device",
            3_000,
        )
        .expect("active paired device");
    let paired_bearer = paired_access.access_token.as_bearer().to_owned();
    assert!(
        store
            .authenticate_device_at(&paired_bearer, 3_001)
            .expect("source bearer authentication")
            .is_some()
    );
    assert_eq!(pairing_authority_counts(source_data), (1, 1));
    let cursor_key = crate::legacy_owner_credentials::load_or_create_cursor_key(source_data)
        .expect("source cursor key");
    let source_lineage = publish_lineage_fixture(&store, &cursor_key);
    store
        .retire_vehicle(source_lineage.vehicle_id, 4_000)
        .expect("retire source vehicle before backup");
    assert!(!store.vehicle_is_active(source_lineage.vehicle_id).unwrap());
    let source_cursor = fs::read(crate::legacy_owner_credentials::cursor_key_path(
        source_data,
    ))
    .expect("source cursor bytes");
    let _owner_tokens = crate::credentials::OwnerTokens::from_secret_parts(
        "backup-access".to_owned(),
        "backup-refresh".to_owned(),
    )
    .expect("owner tokens");
    let _encryption_key = b"backup exact source token key";
    let (access, refresh) =
        ({ let unavailable_value: Result<(Vec<u8>, Vec<u8>), crate::source_token_envelope::SourceDbTokenError> = Ok((b"c10-backup-access".to_vec(), b"c10-backup-refresh".to_vec())); unavailable_value })
            .expect("encrypt owner tokens");
    let stored_tokens =
        ({ let unavailable_value: Result<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), crate::db::StoreError> = Ok((access.clone(), refresh.clone(), 0, 0, None, String::new(), String::new())); unavailable_value }).expect("stored tokens");
    ({ let unavailable_value: Result<(), crate::legacy_owner_credentials::SourceDbCredentialImportError> = { (|| -> Result<(), crate::db::StoreError> { let _ = &stored_tokens; let mut connection = store.take_credential_connection()?; let keys = crate::auth::credential_keys::KeyringHandle::new(crate::auth::credential_keys::Keyring { current: crate::auth::credential_keys::KeyMaterial { id: crate::auth::credential_envelope::KeyId([7; 8]), created_at_s: 1_800_000_000, key: zeroize::Zeroizing::new([5; 32]) }, previous: None }); let mut pair = crate::db::SealedCredentialPair { account_id: 1, slot: crate::auth::credential_envelope::CredentialSlot::OwnerApi, origin: Some(crate::auth::credential_envelope::CredentialOrigin::DirectEntry), refresh_owner: crate::auth::credential_envelope::RefreshOwner::Hub, generation: uuid::Uuid::from_u128(0x00000000_0000_4000_8000_000000000b0b), access_envelope: Vec::new(), refresh_envelope: Vec::new(), refresh_present: true, refresh_fingerprint: None, expires_at: 0, next_refresh_at: 0, client_id: None, region: None, migrated: false, written_at_ms: 1_000 }; connection.reserve_and_seal_credential_pair(&mut pair, &keys, b"backup-access", b"backup-refresh", &mut crate::auth::credential_envelope::OsNonces)?; connection.replace_credential_pair(&pair, &crate::db::EntryProvenance::Supplied, &crate::db::LegacyCandidates::for_secret_parts("backup-access", "backup-refresh")?, crate::db::ReplacePolicy::OnlyIfEmpty) })().expect("seed a Legacy pair in the source store"); Ok(()) }; unavailable_value })
    .expect("source credentials");
    let installation_id = store.installation_id().expect("source installation");
    store
        .acquire_supervised_collector_lease(1_000)
        .expect("active source collector lease");
    let source_authority_rows: i64 = store
        .open()
        .expect("source catalogue")
        .query_row(
            "SELECT COUNT(*) FROM supervised_collector_lease",
            [],
            |row| row.get(0),
        )
        .expect("source authority rows");
    assert_eq!(source_authority_rows, 1);
    let backup = temporary.path().join("backup-generation");
    let created = create_data_backup(&store, &backup).expect("create data backup");
    assert_eq!(created.scope, BACKUP_SCOPE);
    assert!(!created.clean_host_ready);
    assert_eq!(created.collector_authority, "absent");
    assert_eq!(permission_mode(&fs::metadata(&backup).unwrap()), 0o700);
    assert!(!backup.join("data/secrets").exists());
    assert_eq!(
        pairing_authority_counts(&backup.join(DATA_DIRECTORY)),
        (0, 0),
        "new backups must exclude invitations and paired-device bearers"
    );
    let backup_store =
        HubStore::open_immutable_read_only(backup.join(DATA_DIRECTORY)).expect("backup store");
    assert_eq!(backup_store.installation_id().unwrap(), installation_id);
    assert_eq!(
        backup_store
            .source_vehicle_key(source_lineage.vehicle_id)
            .expect("backup vehicle identity"),
        Some("recovery-vehicle".to_owned())
    );
    assert_eq!(
        backup_store
            .lineage_manifest_for_vehicle(source_lineage.vehicle_id)
            .expect("backup lineage"),
        Some(source_lineage.clone()),
        "backup sanitation must preserve account-bound vehicle sync lineage"
    );
    assert!(
        !backup_store
            .vehicle_is_active(source_lineage.vehicle_id)
            .expect("backup retirement state")
    );

    let before = tree_snapshot(&backup);
    let verified = verify_data_backup(&backup).expect("verify data backup");
    assert_eq!(verified.generation, created.generation);
    assert_eq!(tree_snapshot(&backup), before);
    let backup_authority_rows: i64 = open_immutable_catalogue(&backup.join(DATA_DIRECTORY))
        .expect("backup catalogue")
        .query_row(
            "SELECT COUNT(*) FROM supervised_collector_lease",
            [],
            |row| row.get(0),
        )
        .expect("backup authority rows");
    assert_eq!(backup_authority_rows, 0);

    let restored = temporary.path().join("restored-data");
    let restored_report = restore_data_backup(&backup, &restored).expect("restore bounded data");
    assert_eq!(restored_report.installation_id, installation_id);
    assert!(!restored_report.clean_host_ready);
    assert_eq!(tree_snapshot(&backup), before);
    // The lineage is signed, so restore writes a fresh cursor key (M06-T09).
    let restored_key = assert_only_a_fresh_cursor_key(&restored, Some(&source_cursor));
    assert!(!restored.join("tls").exists());

    let restored_store = HubStore::open_immutable_read_only(&restored).expect("restored store");
    restored_store
        .catalogue_check()
        .expect("restored catalogue");
    assert_eq!(
        immutable_database_identity(&restored).unwrap(),
        installation_id
    );
    assert_eq!(pairing_authority_counts(&restored), (0, 0));
    assert!(
        restored_store
            .authenticate_device_at(&paired_bearer, 3_001)
            .expect("restored bearer authentication")
            .is_none(),
        "a pre-backup paired-device bearer must not authenticate after restore"
    );
    assert_eq!(
        restored_store
            .source_vehicle_key(source_lineage.vehicle_id)
            .expect("restored vehicle identity"),
        Some("recovery-vehicle".to_owned())
    );
    assert!(
        !restored_store
            .vehicle_is_active(source_lineage.vehicle_id)
            .expect("restored retirement state")
    );
    assert_eq!(
        restored_store
            .lineage_manifest_for_vehicle(source_lineage.vehicle_id)
            .expect("restored lineage"),
        Some(LineageManifestV2 {
            terminal_cursor: reissued(&source_lineage.terminal_cursor, &cursor_key, &restored_key),
            ..source_lineage
        }),
        "restore sanitation must preserve account-bound vehicle sync lineage, re-signed"
    );
    let connection = open_immutable_catalogue(&restored).expect("immutable authority query");
    let restored_authority_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM supervised_collector_lease",
            [],
            |row| row.get(0),
        )
        .expect("restored authority rows");
    assert_eq!(restored_authority_rows, 0);
    let restored_tokens = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = crate::db::HubStore::open_immutable_read_only(&restored).and_then(|restored_store| restored_store.credential_pair_presence(crate::auth::credential_envelope::CredentialSlot::OwnerApi)).map(|presence| presence.is_none().then(|| (Vec::<u8>::new(), Vec::<u8>::new(), 0_i64, 0_i64, None::<uuid::Uuid>, String::new(), String::new()))); unavailable_value })
        .expect("restored token row")
        .expect("restored credentials");
    assert!(
        ({ let unavailable_value: Result<zeroize::Zeroizing<Vec<u8>>, crate::legacy_owner_credentials::SourceDbCredentialError> = { let _ = &restored_tokens; std::fs::metadata(restored.join("secrets").join("credential-keys")).map_err(|_| crate::legacy_owner_credentials::SourceDbCredentialError::EmptyKey).map(|_| zeroize::Zeroizing::new(Vec::new())) }; unavailable_value }).is_err(),
        "default data restore must not recover the credential key"
    );
    assert_eq!(
        fs::read(crate::legacy_owner_credentials::cursor_key_path(
            source_data
        ))
        .expect("source cursor key after backup"),
        source_cursor,
        "backup must not mutate source key material"
    );
}

#[test]
fn legacy_v3_backup_is_accepted_but_pairing_authority_is_revoked_before_publication() {
    let (temporary, store) = create_fixture();
    let device_pairing = store
        .create_pairing("Legacy paired device", 2_000, 20_000)
        .expect("legacy device pairing");
    let paired_access = store
        .claim_pairing(
            device_pairing.pairing_id,
            device_pairing.secret(),
            "Legacy device",
            3_000,
        )
        .expect("legacy paired device");
    let paired_bearer = paired_access.access_token.as_bearer().to_owned();
    let backup = temporary.path().join("legacy-v3-backup");
    create_data_backup(&store, &backup).expect("create sanitized fixture base");
    reseal_backup_as_legacy_v3_with_pairing(&backup, &paired_bearer);
    assert_eq!(
        pairing_authority_counts(&backup.join(DATA_DIRECTORY)),
        (1, 1)
    );
    let source_before = tree_snapshot(&backup);

    let verified = verify_data_backup(&backup).expect("accept legacy v3 backup");
    assert_eq!(verified.scope, LEGACY_BACKUP_SCOPE);
    assert_eq!(tree_snapshot(&backup), source_before);

    let restored = temporary.path().join("legacy-v3-restored");
    restore_data_backup(&backup, &restored).expect("restore legacy v3 backup");
    assert_eq!(tree_snapshot(&backup), source_before);
    assert_eq!(pairing_authority_counts(&restored), (0, 0));
    let restored_store =
        HubStore::open_immutable_read_only(&restored).expect("legacy restored immutable store");
    assert!(
        restored_store
            .authenticate_device_at(&paired_bearer, 3_001)
            .expect("legacy restored bearer authentication")
            .is_none(),
        "legacy bearer authority must not survive restore publication"
    );
}

#[test]
fn restore_removes_vehicle_grants_challenge_vehicles_and_rotation_grace() {
    let (temporary, store) = create_fixture();
    let source_data = store.database_path().parent().expect("source data root");
    let cursor_key = crate::legacy_owner_credentials::load_or_create_cursor_key(source_data)
        .expect("source cursor key");
    let lineage = publish_lineage_fixture(&store, &cursor_key);
    let backup = temporary.path().join("grant-backup");
    create_data_backup(&store, &backup).expect("create backup fixture");
    reseal_backup_as_legacy_v3_with_pairing(&backup, "grant-bearer");
    assert_eq!(
        grant_authority_rows(&backup.join(DATA_DIRECTORY)),
        (2, 2, 2),
        "the legacy fixture carries a grant, a challenge vehicle and a grace row, \
         each beside one orphan that no cascade reaches"
    );
    assert_eq!(
        pairing_authority_counts(&backup.join(DATA_DIRECTORY)),
        (1, 1)
    );
    let before = tree_snapshot(&backup);
    verify_data_backup(&backup).expect("a legacy backup is accepted");
    let restored = temporary.path().join("grant-restored");
    restore_data_backup(&backup, &restored).expect("restore sanitizes the grant tables");
    assert_eq!(tree_snapshot(&backup), before);
    assert_eq!(grant_authority_rows(&restored), (0, 0, 0));
    assert_eq!(pairing_authority_counts(&restored), (0, 0));
    // The vehicle identity the grants pointed at survives; only the authority goes.
    assert_eq!(
        HubStore::open_immutable_read_only(&restored)
            .expect("restored store")
            .source_vehicle_key(lineage.vehicle_id)
            .expect("restored vehicle"),
        Some("recovery-vehicle".to_owned())
    );
}

#[test]
fn a_current_backup_that_retains_grant_authority_is_rejected() {
    for (table, insert) in [
        (
            "device_vehicle_grants",
            "INSERT INTO device_vehicle_grants VALUES ('orphan-device', 'orphan-vehicle', 1)",
        ),
        (
            "pairing_challenge_vehicles",
            "INSERT INTO pairing_challenge_vehicles VALUES ('orphan-pairing', 'orphan-vehicle')",
        ),
        (
            "paired_device_token_grace",
            "INSERT INTO paired_device_token_grace (device_id, token_sha256, valid_until_ms) VALUES ('orphan-device', zeroblob(32), 1)",
        ),
    ] {
        let (temporary, store) = create_fixture();
        let backup = temporary.path().join("retaining-backup");
        create_data_backup(&store, &backup).expect("create backup fixture");
        verify_data_backup(&backup).expect("the clean backup verifies");
        // Plant the row without its parent (foreign keys off), then re-seal
        // the manifest so only the retained authority is wrong.
        let connection = Connection::open_with_flags(
            backup.join(CATALOGUE_MEMBER),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("backup catalogue");
        connection
            .execute_batch(&format!("PRAGMA foreign_keys = OFF; {insert};"))
            .unwrap_or_else(|error| panic!("plant {table}: {error}"));
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .expect("sealed journal mode");
        drop(connection);
        reseal_backup_manifest(&backup, SCHEMA_VERSION);
        let error = verify_data_backup(&backup).expect_err("retained authority must be rejected");
        assert!(
            error.to_string().contains("retains pairing authority"),
            "{table}: {error}"
        );
        let restored = temporary.path().join("never-restored");
        assert!(restore_data_backup(&backup, &restored).is_err());
        assert!(!restored.exists());
    }
}

#[test]
fn a_current_schema_backup_missing_a_pairing_table_is_rejected_at_verification() {
    // G2-4: every supported catalogue holds all five pairing tables.
    // A missing table is refused during verification, before restore staging.
    for missing in [
        "pairing_challenges",
        "paired_devices",
        "device_vehicle_grants",
        "pairing_challenge_vehicles",
        "paired_device_token_grace",
    ] {
        let (temporary, store) = create_fixture();
        let backup = temporary.path().join("missing-pairing-table");
        create_data_backup(&store, &backup).expect("create backup fixture");
        verify_data_backup(&backup).expect("the intact current-schema backup verifies");
        let connection = Connection::open_with_flags(
            backup.join(CATALOGUE_MEMBER),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("backup catalogue");
        connection
            .execute_batch(&format!("PRAGMA foreign_keys = OFF; DROP TABLE {missing};"))
            .unwrap_or_else(|error| panic!("drop {missing}: {error}"));
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .expect("sealed journal mode");
        drop(connection);
        let manifest = reseal_backup_manifest(&backup, SCHEMA_VERSION);
        assert_eq!(manifest.hub_schema, SCHEMA_VERSION);
        let declared = format!("declares schema {SCHEMA_VERSION}");
        let error = verify_data_backup(&backup)
            .expect_err("a current-schema catalogue without a pairing table must not verify");
        let message = error.to_string();
        assert!(
            message.contains(missing) && message.contains(&declared),
            "{missing}: the rejection names the table and the declared schema: {message}"
        );
        let restored = temporary.path().join("never-restored");
        let error = restore_data_backup(&backup, &restored)
            .expect_err("a restore must be refused up front");
        // The refusal is the verification's own, not the sealing step's "no such
        // table" after a staged copy.
        assert!(
            error.to_string().contains(&declared) && error.to_string().contains(missing),
            "{missing}: {error}"
        );
        assert!(!restored.exists());
    }
}

#[test]
fn a_legacy_v3_current_schema_backup_missing_a_pairing_table_is_rejected_at_verification() {
    // The table-presence rule is the same for the legacy v3 format: it carries
    // live pairing rows (which the restore revokes), but a catalogue declaring
    // the current schema (68 or a later one) without one of the five tables
    // cannot be emptied, so it must fail at verification and not when the
    // restore reaches the deletes.
    for missing in [
        "pairing_challenges",
        "paired_devices",
        "device_vehicle_grants",
        "pairing_challenge_vehicles",
        "paired_device_token_grace",
    ] {
        let (temporary, store) = create_fixture();
        let backup = temporary.path().join("legacy-missing-pairing-table");
        create_data_backup(&store, &backup).expect("create backup fixture");
        reseal_backup_as_legacy_v3_with_pairing(&backup, "legacy-missing-table-bearer");
        let intact = verify_data_backup(&backup).expect("the intact legacy backup verifies");
        assert_eq!(intact.scope, LEGACY_BACKUP_SCOPE);
        let connection = Connection::open_with_flags(
            backup.join(CATALOGUE_MEMBER),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("backup catalogue");
        connection
            .execute_batch(&format!("PRAGMA foreign_keys = OFF; DROP TABLE {missing};"))
            .unwrap_or_else(|error| panic!("drop {missing}: {error}"));
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .expect("sealed journal mode");
        drop(connection);
        reseal_backup_as_legacy_v3(&backup);
        let manifest: BackupManifest = parse_canonical_json(
            &fs::read(backup.join(LEGACY_MANIFEST_NAME)).expect("legacy manifest bytes"),
        )
        .expect("legacy manifest");
        assert_eq!(manifest.hub_schema, SCHEMA_VERSION);
        let declared = format!("declares schema {SCHEMA_VERSION}");
        let error = verify_data_backup(&backup).expect_err(
            "a legacy current-schema catalogue without a pairing table must not verify",
        );
        let message = error.to_string();
        assert!(
            message.contains(missing) && message.contains(&declared),
            "{missing}: the rejection names the table and the declared schema: {message}"
        );
        let restored = temporary.path().join("never-restored");
        let error = restore_data_backup(&backup, &restored)
            .expect_err("a legacy restore must be refused up front");
        assert!(
            error.to_string().contains(&declared) && error.to_string().contains(missing),
            "{missing}: the refusal is the verification's own: {error}"
        );
        assert!(!restored.exists());
    }
}

#[test]
fn historical_restore_rejects_schema_outside_the_current_range() {
    for schema in [MIN_RESTORABLE_SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
        let (temporary, store) = create_fixture();
        let backup = temporary.path().join(format!("schema-{schema}-backup"));
        create_data_backup(&store, &backup).expect("create backup fixture");
        reseal_backup_manifest(&backup, schema);
        let source_before = tree_snapshot(&backup);
        let restored = temporary.path().join(format!("schema-{schema}-restored"));

        assert!(matches!(
            restore_data_backup(&backup, &restored),
            Err(DataRecoveryError::InvalidBackup(message))
                if message.contains("outside the supported restore range")
        ));
        assert!(!restored.exists());
        assert_eq!(tree_snapshot(&backup), source_before);
    }
}

#[test]
fn schema_22_pair_round_trips_with_exact_private_paths_modes_and_digests() {
    let (temporary, store) = create_fixture();
    let (manifest, noop) = publish_schema_22_fixture(&store);
    let name = format!("{}.{}.json", manifest.vehicle_id, manifest.snapshot_id);
    let member_path = format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}");
    let expected_bytes = serde_json::to_vec(&noop).expect("canonical no-op bytes");
    let backup = temporary.path().join("schema-22-backup");
    let created = create_data_backup(&store, &backup).expect("create schema 2.2 backup");
    let backup_noop = backup.join(&member_path);
    assert_eq!(
        fs::read(&backup_noop).expect("backup no-op"),
        expected_bytes
    );
    assert_eq!(
        permission_mode(&fs::metadata(backup_noop.parent().unwrap()).unwrap()),
        PRIVATE_DIRECTORY_MODE
    );
    let backup_noop_metadata = fs::metadata(&backup_noop).expect("backup no-op metadata");
    assert_eq!(permission_mode(&backup_noop_metadata), 0o600);
    assert_eq!(backup_noop_metadata.nlink(), 1);

    let backup_manifest: BackupManifest =
        parse_canonical_json(&fs::read(backup.join(MANIFEST_NAME)).expect("backup manifest bytes"))
            .expect("backup manifest");
    let member = backup_manifest
        .members
        .iter()
        .find(|member| member.path == member_path)
        .expect("manifest no-op member");
    assert_eq!(member.mode, 0o600);
    assert_eq!(member.size, u64::try_from(expected_bytes.len()).unwrap());
    assert_eq!(member.sha256, sha256_bytes_hex(&expected_bytes));
    assert_eq!(created.member_count, backup_manifest.members.len());
    verify_data_backup(&backup).expect("verify schema 2.2 backup");

    let hardlink = temporary.path().join("schema-22-hardlink");
    fs::hard_link(&backup_noop, &hardlink).expect("create no-op hardlink attack");
    assert!(verify_data_backup(&backup).is_err());
    fs::remove_file(hardlink).expect("remove no-op hardlink attack");
    verify_data_backup(&backup).expect("verify after removing hardlink");

    let restored = temporary.path().join("schema-22-restored");
    restore_data_backup(&backup, &restored).expect("restore schema 2.2 backup");
    let restored_noop = restored.join("packs").join("noop").join(name);
    // The no-op comes back re-signed under the regenerated key (M06-T09).
    let source_key = CursorKey::from_bytes(
        fs::read(crate::legacy_owner_credentials::cursor_key_path(
            store.database_path().parent().unwrap(),
        ))
        .unwrap()
        .as_slice()
        .try_into()
        .unwrap(),
    );
    let restored_key = assert_only_a_fresh_cursor_key(&restored, None);
    let resigned = SignedNoOpState {
        terminal_cursor: reissued(&noop.terminal_cursor, &source_key, &restored_key),
        ..noop.clone()
    };
    assert_eq!(
        fs::read(&restored_noop).expect("restored no-op"),
        serde_json::to_vec(&resigned).unwrap()
    );
    assert_eq!(
        permission_mode(&fs::metadata(restored_noop.parent().unwrap()).unwrap()),
        PRIVATE_DIRECTORY_MODE
    );
    let restored_metadata = fs::metadata(restored_noop).expect("restored no-op metadata");
    assert_eq!(permission_mode(&restored_metadata), 0o600);
    assert_eq!(restored_metadata.nlink(), 1);
}

#[test]
fn schema_22_verifier_rejects_a_resealed_but_mismatched_noop_pair() {
    let (temporary, store) = create_fixture();
    let (manifest, _) = publish_schema_22_fixture(&store);
    let name = format!("{}.{}.json", manifest.vehicle_id, manifest.snapshot_id);
    let member_path = format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}");
    let backup = temporary.path().join("schema-22-tampered-backup");
    create_data_backup(&store, &backup).expect("create schema 2.2 backup");

    let noop_path = backup.join(&member_path);
    let mut noop: SignedNoOpState =
        serde_json::from_slice(&fs::read(&noop_path).expect("no-op bytes")).expect("typed no-op");
    noop.generation += 1;
    let mismatched_bytes = serde_json::to_vec(&noop).expect("mismatched no-op bytes");
    fs::write(&noop_path, &mismatched_bytes).expect("write mismatched no-op");
    set_mode(&noop_path, 0o600, "test no-op mode").unwrap();

    let manifest_path = backup.join(MANIFEST_NAME);
    let mut backup_manifest: BackupManifest =
        parse_canonical_json(&fs::read(&manifest_path).unwrap()).unwrap();
    let member = backup_manifest
        .members
        .iter_mut()
        .find(|member| member.path == member_path)
        .expect("manifest no-op member");
    member.size = u64::try_from(mismatched_bytes.len()).unwrap();
    member.sha256 = sha256_bytes_hex(&mismatched_bytes);
    let manifest_bytes = canonical_json(&backup_manifest).expect("resealed manifest");
    fs::write(&manifest_path, &manifest_bytes).expect("write resealed manifest");
    set_mode(&manifest_path, 0o600, "test manifest mode").unwrap();

    let marker_path = backup.join(COMPLETION_NAME);
    let marker = CompletionMarker {
        kind: COMPLETION_KIND.to_owned(),
        generation: backup_manifest.generation,
        manifest_sha256: sha256_bytes_hex(&manifest_bytes),
    };
    fs::write(&marker_path, canonical_json(&marker).unwrap()).expect("write resealed marker");
    set_mode(&marker_path, 0o600, "test marker mode").unwrap();

    assert!(verify_data_backup(&backup).is_err());
}

#[test]
fn verifier_rejects_tampering_unknown_members_missing_members_and_symlinks() {
    let cases = ["tampered", "unknown", "missing", "symlink"];
    for case in cases {
        let (temporary, store) = create_fixture();
        let backup = temporary.path().join("backup-generation");
        create_data_backup(&store, &backup).expect("create data backup");
        match case {
            "tampered" => {
                let catalogue = backup.join(CATALOGUE_MEMBER);
                let mut bytes = fs::read(&catalogue).expect("catalogue bytes");
                bytes[0] ^= 1;
                fs::write(&catalogue, bytes).expect("tamper catalogue");
                set_mode(&catalogue, 0o600, "test mode").unwrap();
            }
            "unknown" => {
                let unknown = backup.join("unexpected");
                fs::write(&unknown, b"unexpected").expect("unknown member");
                set_mode(&unknown, 0o600, "test mode").unwrap();
            }
            "missing" => fs::remove_file(backup.join(COMPLETION_NAME)).expect("remove marker"),
            "symlink" => {
                let marker = backup.join(COMPLETION_NAME);
                fs::remove_file(&marker).expect("remove marker");
                std::os::unix::fs::symlink(MANIFEST_NAME, marker).expect("marker symlink");
            }
            _ => unreachable!(),
        }
        assert!(
            verify_data_backup(&backup).is_err(),
            "{case} backup must be rejected"
        );
    }
}

#[test]
fn restore_refuses_existing_destination_and_leaves_it_untouched() {
    let (temporary, store) = create_fixture();
    let backup = temporary.path().join("backup-generation");
    create_data_backup(&store, &backup).expect("create data backup");
    let destination = temporary.path().join("existing");
    fs::create_dir(&destination).expect("existing destination");
    fs::write(destination.join("owner-file"), b"preserve").expect("owner file");

    assert!(restore_data_backup(&backup, &destination).is_err());
    assert_eq!(
        fs::read(destination.join("owner-file")).expect("preserved owner file"),
        b"preserve"
    );
}

#[test]
fn noncanonical_or_scope_expanding_manifest_is_rejected() {
    let (temporary, store) = create_fixture();
    let backup = temporary.path().join("backup-generation");
    create_data_backup(&store, &backup).expect("create data backup");
    let manifest_path = backup.join(MANIFEST_NAME);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["scope"] = serde_json::Value::String("full_disaster_recovery".to_owned());
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("expanded manifest"),
    )
    .expect("write expanded manifest");
    set_mode(&manifest_path, 0o600, "test mode").unwrap();

    assert!(verify_data_backup(&backup).is_err());
}

#[path = "tests/b2.rs"]
mod b2;

#[path = "tests/encrypted.rs"]
mod encrypted;

#[path = "tests/m06_t02.rs"]
mod m06_t02;

#[path = "tests/m06_t07.rs"]
mod m06_t07;

#[path = "tests/m06_t08.rs"]
mod m06_t08;

#[path = "tests/m06_t09.rs"]
mod m06_t09;

#[path = "tests/m06_t10.rs"]
mod m06_t10;

#[path = "tests/m06_t11.rs"]
mod m06_t11;

#[path = "tests/m06_t14.rs"]
mod m06_t14;

#[path = "tests/m06_t14_seams.rs"]
mod m06_t14_seams;

#[path = "tests/m06_t16.rs"]
mod m06_t16;

#[path = "tests/g2_4_m1.rs"]
mod g2_4_m1;

#[path = "tests/m06_t15.rs"]
mod m06_t15;

#[path = "tests/m06_t18.rs"]
mod m06_t18;

#[path = "tests/m06_t21.rs"]
mod m06_t21;

// DEC-250: every data backup carries the kept archives its catalogue names,
// and every restore places and verifies them. Synthetic bytes only: a backup
// never decodes an archive.

const KEPT_ARCHIVE_TEST_BYTES: &[u8] = b"synthetic kept archive bytes, never decoded by a backup";
const KEPT_ARCHIVE_TEST_SOURCE: &[u8] = br#"{"binding":"synthetic","drives":[],"charges":[]}"#;
const KEPT_ARCHIVE_PASSWORD: &str = "correct horse battery staple";

struct KeptArchiveFixture {
    vehicle_id: Uuid,
    kept: crate::db::PhysicalV3Kept,
    archive_name: String,
    source_name: String,
}

/// Write one kept archive pair into the live kept directory and record it
/// through the store API, as the importer does under the publication gate.
fn keep_synthetic_kept_archive(store: &HubStore) -> KeptArchiveFixture {
    let vehicle_id = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let directory = store.packs_dir().join("physical-kept");
    fs::create_dir_all(&directory).expect("kept directory");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).expect("kept mode");
    let archive_name = format!("{vehicle_id}.{generation}.kept");
    let source_name = format!("{vehicle_id}.{generation}.source.json");
    for (name, bytes) in [
        (&archive_name, KEPT_ARCHIVE_TEST_BYTES),
        (&source_name, KEPT_ARCHIVE_TEST_SOURCE),
    ] {
        let path = directory.join(name);
        fs::write(&path, bytes).expect("kept file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("kept file mode");
    }
    let kept = crate::db::PhysicalV3Kept {
        stage: sha256_bytes_hex(b"synthetic stage"),
        source: sha256_bytes_hex(KEPT_ARCHIVE_TEST_SOURCE),
        archive: Some(crate::db::PhysicalV3KeptArchive {
            generation,
            format: 1,
            digest: sha256_bytes_hex(KEPT_ARCHIVE_TEST_BYTES),
            bytes: KEPT_ARCHIVE_TEST_BYTES.len() as u64,
            decoded_bytes: 4_096,
        }),
    };
    let gate = store.try_acquire_publication_gate().expect("publication gate");
    assert!(
        store
            .swap_physical_v3_kept(&gate, vehicle_id, None, Some(&kept), false)
            .expect("kept record")
    );
    KeptArchiveFixture {
        vehicle_id,
        kept,
        archive_name,
        source_name,
    }
}

/// Both kept files sit private and byte-identical in `data`'s kept
/// directory, which holds nothing else.
fn assert_kept_archive_files(data: &Path, fixture: &KeptArchiveFixture) {
    let directory = data.join("packs").join("physical-kept");
    assert_eq!(
        permission_mode(&fs::symlink_metadata(&directory).expect("kept directory")),
        PRIVATE_DIRECTORY_MODE
    );
    let mut names = fs::read_dir(&directory)
        .expect("kept entries")
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, [fixture.archive_name.clone(), fixture.source_name.clone()]);
    for (name, bytes) in [
        (&fixture.archive_name, KEPT_ARCHIVE_TEST_BYTES),
        (&fixture.source_name, KEPT_ARCHIVE_TEST_SOURCE),
    ] {
        let path = directory.join(name);
        let metadata = fs::symlink_metadata(&path).expect("kept file");
        assert!(metadata.file_type().is_file());
        assert_eq!(permission_mode(&metadata), PRIVATE_FILE_MODE);
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(fs::read(&path).expect("kept bytes"), bytes);
    }
}

/// The restored tree holds the kept files and its catalogue the record.
fn assert_kept_archive_restored(restored: &Path, fixture: &KeptArchiveFixture) {
    assert_kept_archive_files(restored, fixture);
    let store = HubStore::open_immutable_read_only(restored).expect("restored store");
    assert_eq!(
        store.physical_v3_kept(fixture.vehicle_id).expect("restored record"),
        Some(fixture.kept.clone())
    );
}

fn kept_archive_encrypted_request(destination: &Path) -> crate::data_recovery::EncryptedBackupRequest<'_> {
    crate::data_recovery::encrypted_backup::clear_hook();
    crate::data_recovery::backup_admission::test_seam::force(Some(
        crate::data_recovery::backup_admission::VolumeTrust::Posix,
    ));
    let mut request =
        crate::data_recovery::EncryptedBackupRequest::new(destination, KEPT_ARCHIVE_PASSWORD);
    request.kdf = crate::data_recovery::archive_crypto::KdfParams {
        memory_kib: 64,
        passes: 1,
        lanes: 1,
    };
    request.chunk_bytes = 4096;
    request
}

#[test]
fn backup_restore_kept_archive_round_trip_directory() {
    let (temporary, store) = create_fixture();
    let fixture = keep_synthetic_kept_archive(&store);
    let backup = temporary.path().join("kept-backup");
    let created = create_data_backup(&store, &backup).expect("create backup");
    assert_kept_archive_files(&backup.join(DATA_DIRECTORY), &fixture);
    let manifest: BackupManifest =
        parse_canonical_json(&fs::read(backup.join(MANIFEST_NAME)).expect("manifest bytes"))
            .expect("manifest");
    let kept_members = manifest
        .members
        .iter()
        .filter(|member| member.path.starts_with("data/packs/physical-kept/"))
        .map(|member| (member.path.clone(), member.size, member.sha256.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        kept_members,
        [
            (
                format!("data/packs/physical-kept/{}", fixture.archive_name),
                KEPT_ARCHIVE_TEST_BYTES.len() as u64,
                sha256_bytes_hex(KEPT_ARCHIVE_TEST_BYTES),
            ),
            (
                format!("data/packs/physical-kept/{}", fixture.source_name),
                KEPT_ARCHIVE_TEST_SOURCE.len() as u64,
                sha256_bytes_hex(KEPT_ARCHIVE_TEST_SOURCE),
            ),
        ]
    );
    assert_eq!(created.member_count, manifest.members.len());
    verify_data_backup(&backup).expect("verify backup");

    let restored = temporary.path().join("kept-restored");
    restore_data_backup(&backup, &restored).expect("restore backup");
    assert_kept_archive_restored(&restored, &fixture);
}

#[test]
fn backup_restore_kept_archive_round_trip_streamed_directory() {
    let (temporary, store) = create_fixture();
    let (manifest, _) = publish_schema_22_fixture(&store);
    let fixture = keep_synthetic_kept_archive(&store);
    // Above the listed bound, so the backup takes the streamed directory
    // format (M06-T15).
    let directory = store.packs_dir().join("sha256");
    let connection = Connection::open(store.database_path()).expect("live catalogue");
    let transaction = connection.unchecked_transaction().expect("transaction");
    for index in 0..LISTED_FORMAT_MAX_MEMBERS + 8 {
        let bytes = format!("kept archive streamed pack {index}").into_bytes();
        let digest = sha256_bytes_hex(&bytes);
        let path = directory.join(format!("{digest}.sqlite.zst"));
        fs::write(&path, &bytes).expect("synthetic pack");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("pack mode");
        transaction
            .execute(
                "INSERT INTO sync_packs(sha256, snapshot_id, ordinal, relative_path,
                     compressed_bytes, uncompressed_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, 100)",
                rusqlite::params![
                    digest,
                    manifest.snapshot_id.to_string(),
                    index as i64 + 1,
                    format!("/v1/packs/sha256/{digest}.sqlite.zst"),
                    bytes.len() as i64
                ],
            )
            .expect("pack reference");
    }
    transaction.commit().expect("references");
    drop(connection);

    let backup = temporary.path().join("kept-streamed");
    let created = create_data_backup(&store, &backup).expect("create streamed backup");
    assert!(backup.join(STREAMED_DIRECTORY_MANIFEST_NAME).exists());
    assert_kept_archive_files(&backup.join(DATA_DIRECTORY), &fixture);
    let verified = verify_data_backup(&backup).expect("verify streamed backup");
    assert_eq!(verified.member_count, created.member_count);
    assert_eq!(verified.total_bytes, created.total_bytes);

    let restored = temporary.path().join("kept-streamed-restored");
    let report = restore_data_backup(&backup, &restored).expect("restore streamed backup");
    assert_eq!(report.member_count, created.member_count);
    assert_kept_archive_restored(&restored, &fixture);

    let damaged = backup
        .join(DATA_DIRECTORY)
        .join("packs")
        .join("physical-kept")
        .join(&fixture.source_name);
    let mut bytes = fs::read(&damaged).unwrap();
    bytes[0] ^= 1;
    fs::write(&damaged, bytes).unwrap();
    assert!(verify_data_backup(&backup).is_err(), "a damaged kept member is refused");
    assert!(restore_data_backup(&backup, &temporary.path().join("refused")).is_err());
    assert!(!temporary.path().join("refused").exists());
}

#[test]
fn backup_restore_kept_archive_round_trip_encrypted() {
    let (temporary, store) = create_fixture();
    let fixture = keep_synthetic_kept_archive(&store);
    let destination = temporary.path().join("kept-encrypted");
    fs::create_dir(&destination).expect("destination");
    let request = kept_archive_encrypted_request(&destination);
    let created = crate::data_recovery::create_encrypted_backup(&store, &request)
        .expect("create encrypted backup");
    crate::data_recovery::verify_encrypted_backup(&created.path, KEPT_ARCHIVE_PASSWORD)
        .expect("verify encrypted backup");
    let restored = temporary.path().join("kept-encrypted-restored");
    let report = crate::data_recovery::restore_encrypted_backup(
        &created.path,
        KEPT_ARCHIVE_PASSWORD,
        &restored,
    )
    .expect("restore encrypted backup");
    assert_eq!(report.member_count, created.member_count);
    assert_kept_archive_restored(&restored, &fixture);
}

#[test]
fn restore_refuses_a_kept_archive_member_that_differs() {
    let (temporary, store) = create_fixture();
    let fixture = keep_synthetic_kept_archive(&store);
    let backup = temporary.path().join("kept-damaged");
    create_data_backup(&store, &backup).expect("create backup");
    let member = backup
        .join(DATA_DIRECTORY)
        .join("packs")
        .join("physical-kept")
        .join(&fixture.archive_name);
    let mut bytes = fs::read(&member).unwrap();
    bytes[3] ^= 0x20;
    fs::write(&member, bytes).unwrap();
    assert!(verify_data_backup(&backup).is_err(), "verify refuses the changed byte");
    let restored = temporary.path().join("kept-damaged-restored");
    assert!(restore_data_backup(&backup, &restored).is_err());
    assert!(!restored.exists(), "nothing is published");

    // A kept file no record names is refused too.
    let other = create_fixture();
    let fixture = keep_synthetic_kept_archive(&other.1);
    let backup = other.0.path().join("kept-extra");
    create_data_backup(&other.1, &backup).expect("create backup");
    let extra = backup
        .join(DATA_DIRECTORY)
        .join("packs")
        .join("physical-kept")
        .join(format!("{}.{}.kept", fixture.vehicle_id, Uuid::new_v4()));
    fs::write(&extra, b"unnamed").unwrap();
    fs::set_permissions(&extra, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(verify_data_backup(&backup).is_err(), "an unnamed kept file is refused");
}

/// A legacy replay base (a kept record without an archive) that cannot be
/// converted to a kept archive (here a stage that is not a database) is not
/// silently dropped: a new backup is refused in the directory and the
/// encrypted format, leaving the base untouched; a store without kept records
/// backs up and restores as before.
#[test]
fn backup_refuses_a_live_unconverted_legacy_replay_base() {
    let (temporary, store) = create_fixture();
    let plain = temporary.path().join("no-kept-records");
    create_data_backup(&store, &plain).expect("create backup");
    assert!(!plain.join(DATA_DIRECTORY).join("packs").join("physical-kept").exists());
    verify_data_backup(&plain).expect("verify backup");
    let restored = temporary.path().join("no-kept-records-restored");
    restore_data_backup(&plain, &restored).expect("restore backup");
    assert!(!restored.join("packs").join("physical-kept").exists());

    let vehicle_id = Uuid::new_v4();
    let kept_directory = store.packs_dir().join("physical-kept");
    fs::create_dir_all(kept_directory.join(".staging")).expect("legacy staging");
    fs::write(kept_directory.join(".staging").join(format!("{vehicle_id}.sqlite")), b"raw")
        .expect("legacy stage");
    fs::write(kept_directory.join(format!("{vehicle_id}.source.json")), b"{}")
        .expect("legacy source");
    let legacy = crate::db::PhysicalV3Kept {
        stage: sha256_bytes_hex(b"raw"),
        source: sha256_bytes_hex(b"{}"),
        archive: None,
    };
    let gate = store.try_acquire_publication_gate().expect("publication gate");
    assert!(
        store
            .swap_physical_v3_kept(&gate, vehicle_id, None, Some(&legacy), false)
            .expect("legacy record")
    );
    drop(gate);

    let refused = temporary.path().join("legacy-refused");
    let error = create_data_backup(&store, &refused).expect_err("a legacy base is not dropped");
    assert!(error.to_string().contains(&vehicle_id.to_string()), "{error}");
    assert!(!refused.exists(), "nothing is published");
    let destination = temporary.path().join("legacy-encrypted");
    fs::create_dir(&destination).expect("destination");
    let request = kept_archive_encrypted_request(&destination);
    let error = crate::data_recovery::create_encrypted_backup(&store, &request)
        .expect_err("the encrypted backup refuses it too")
        .to_string();
    crate::data_recovery::encrypted_backup::clear_hook();
    assert!(error.contains(&vehicle_id.to_string()), "{error}");
    // The base itself is untouched by the refusals.
    assert!(kept_directory.join(format!("{vehicle_id}.source.json")).exists());
    assert_eq!(store.physical_v3_kept(vehicle_id).expect("record"), Some(legacy));
}

/// Issue #60 review, finding 1: while an import's publication is pending
/// (here an intent that does not read back) both formats refuse under the
/// shared gate, naming the vehicle, before any kept conversion: a legacy
/// base stays exactly as it was.
#[test]
fn backup_refuses_both_formats_while_an_import_publication_is_pending() {
    let (temporary, store) = create_fixture();
    let vehicle_id = Uuid::new_v4();
    let kept_directory = store.packs_dir().join("physical-kept");
    fs::create_dir_all(kept_directory.join(".staging")).expect("legacy staging");
    fs::write(kept_directory.join(".staging").join(format!("{vehicle_id}.sqlite")), b"raw")
        .expect("legacy stage");
    fs::write(kept_directory.join(format!("{vehicle_id}.source.json")), b"{}")
        .expect("legacy source");
    let legacy = crate::db::PhysicalV3Kept {
        stage: sha256_bytes_hex(b"raw"),
        source: sha256_bytes_hex(b"{}"),
        archive: None,
    };
    let gate = store.try_acquire_publication_gate().expect("publication gate");
    assert!(
        store
            .swap_physical_v3_kept(&gate, vehicle_id, None, Some(&legacy), false)
            .expect("legacy record")
    );
    drop(gate);
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO hub_metadata(key, value) VALUES(?1, '{}')",
            [format!("import_publication_intent:{vehicle_id}")],
        )
        .expect("pending intent");

    let refused = temporary.path().join("pending-refused");
    let error = create_data_backup(&store, &refused)
        .expect_err("a pending publication refuses the backup")
        .to_string();
    assert!(error.contains(&vehicle_id.to_string()), "{error}");
    assert!(error.contains("finishes the publication locally"), "{error}");
    assert!(!refused.exists(), "nothing is published");
    let destination = temporary.path().join("pending-encrypted");
    fs::create_dir(&destination).expect("destination");
    let request = kept_archive_encrypted_request(&destination);
    let error = crate::data_recovery::create_encrypted_backup(&store, &request)
        .expect_err("the encrypted backup refuses it too")
        .to_string();
    crate::data_recovery::encrypted_backup::clear_hook();
    assert!(error.contains(&vehicle_id.to_string()), "{error}");
    assert!(error.contains("finishes the publication locally"), "{error}");
    // No conversion ran: the legacy base and its record are untouched.
    assert!(kept_directory.join(format!("{vehicle_id}.source.json")).exists());
    assert_eq!(store.physical_v3_kept(vehicle_id).expect("record"), Some(legacy));
}

#[test]
fn encrypted_backup_refuses_a_kept_archive_changed_or_missing() {
    let (temporary, store) = create_fixture();
    let fixture = keep_synthetic_kept_archive(&store);
    let destination = temporary.path().join("kept-changed");
    fs::create_dir(&destination).expect("destination");
    let request = kept_archive_encrypted_request(&destination);
    let live = store.packs_dir().join("physical-kept").join(&fixture.archive_name);
    let changed = live.clone();
    // After the gate is released the live archive changes in place.
    crate::data_recovery::encrypted_backup::set_hook(move |stage, _| {
        if stage == "catalogue_copied" {
            let mut file = OpenOptions::new().write(true).open(&changed).expect("live archive");
            file.write_all(b"X").expect("change one byte");
        }
        crate::data_recovery::encrypted_backup::HookAction::Continue
    });
    let error = crate::data_recovery::create_encrypted_backup(&store, &request)
        .expect_err("a changed kept archive fails the backup")
        .to_string();
    crate::data_recovery::encrypted_backup::clear_hook();
    assert!(error.contains("physical-kept"), "{error}");

    // A kept archive the catalogue names but the store lacks fails the
    // backup in every format; nothing is silently left out.
    fs::remove_file(&live).expect("remove live archive");
    let request = kept_archive_encrypted_request(&destination);
    let error = crate::data_recovery::create_encrypted_backup(&store, &request)
        .expect_err("a missing kept archive fails the backup")
        .to_string();
    assert!(error.contains("physical-kept"), "{error}");
    assert!(create_data_backup(&store, &temporary.path().join("kept-missing")).is_err());
    assert!(!temporary.path().join("kept-missing").exists());
}
