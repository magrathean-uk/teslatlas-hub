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
    fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
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

fn reseal_backup_as_legacy_v3_with_pairing(backup: &Path, bearer: &str) {
    let catalogue = backup.join(CATALOGUE_MEMBER);
    let connection = Connection::open_with_flags(
        &catalogue,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("legacy catalogue fixture");
    connection
        .execute(
            "INSERT INTO pairing_challenges
             (pairing_id, label, secret_sha256, created_at_ms, expires_at_ms)
             VALUES (?1, 'legacy invitation', ?2, 1000, 10000)",
            rusqlite::params![Uuid::new_v4().to_string(), vec![17_u8; 32]],
        )
        .expect("legacy pairing challenge");
    connection
        .execute(
            "INSERT INTO paired_devices
             (device_id, display_name, token_sha256, created_at_ms, expires_at_ms,
              revoked_at_ms, last_authenticated_at_ms)
             VALUES (?1, 'legacy device', ?2, 2000, 20000, NULL, 2001)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                Sha256::digest(bearer.as_bytes()).to_vec()
            ],
        )
        .expect("legacy paired device");
    drop(connection);

    let current_manifest_path = backup.join(MANIFEST_NAME);
    let legacy_manifest_path = backup.join(LEGACY_MANIFEST_NAME);
    let mut manifest: BackupManifest =
        parse_canonical_json(&fs::read(&current_manifest_path).expect("current manifest bytes"))
            .expect("current manifest");
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
    fs::rename(&current_manifest_path, &legacy_manifest_path).expect("legacy manifest filename");
    let manifest_bytes = canonical_json(&manifest).expect("legacy manifest bytes");
    fs::write(&legacy_manifest_path, &manifest_bytes).expect("legacy manifest");
    set_mode(
        &legacy_manifest_path,
        PRIVATE_FILE_MODE,
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
    set_mode(&marker_path, PRIVATE_FILE_MODE, "legacy marker mode").unwrap();
}

fn reseal_backup_as_schema(backup: &Path, schema: i32) -> BackupManifest {
    let catalogue = backup.join(CATALOGUE_MEMBER);
    if (52..SCHEMA_VERSION).contains(&schema) {
        let connection = Connection::open_with_flags(
            &catalogue,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open historical catalogue fixture");
        downgrade_catalogue_fixture(&connection, schema);
        drop(connection);
    }

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
        PRIVATE_FILE_MODE,
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
    set_mode(&marker_path, PRIVATE_FILE_MODE, "historical marker mode").unwrap();
    manifest
}

fn downgrade_catalogue_fixture(connection: &Connection, schema: i32) {
    assert!((52..=54).contains(&schema));
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             DROP TABLE fleet_refresh_input_fences;
             DROP TABLE fleet_refresh_receipt_bindings;
             CREATE TABLE fleet_tokens_v54 (
                singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                access BLOB NOT NULL CHECK(length(access) BETWEEN 1 AND 16424),
                refresh BLOB NOT NULL CHECK(length(refresh) BETWEEN 1 AND 16424),
                client_id TEXT NOT NULL
                    CHECK(length(CAST(client_id AS BLOB)) BETWEEN 1 AND 255),
                region TEXT NOT NULL CHECK(region IN ('na', 'eu', 'cn')),
                expires_at INTEGER NOT NULL CHECK(expires_at > 0),
                next_refresh_at INTEGER NOT NULL
                    CHECK(next_refresh_at > 0 AND next_refresh_at < expires_at)
             ) STRICT;
             INSERT INTO fleet_tokens_v54(
                singleton_id, access, refresh, client_id, region,
                expires_at, next_refresh_at
             )
             SELECT singleton_id, access, refresh, client_id, region,
                    expires_at, next_refresh_at
               FROM fleet_tokens;
             DROP TABLE fleet_tokens;
             ALTER TABLE fleet_tokens_v54 RENAME TO fleet_tokens;
             CREATE TABLE current_observations_v54 (
                vehicle_id TEXT NOT NULL
                    REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                record_type TEXT NOT NULL CHECK(record_type IN (
                    'owner_api_discovery_v1',
                    'owner_api_vehicle_data_v1',
                    'tesla_stream_update_v1'
                )),
                observation_id INTEGER NOT NULL CHECK(observation_id > 0),
                source_id TEXT NOT NULL
                    REFERENCES sources(source_id) ON DELETE RESTRICT,
                observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
                received_at_ms INTEGER NOT NULL CHECK(received_at_ms >= 0),
                payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32),
                payload_json TEXT NOT NULL CHECK(json_valid(payload_json))
                    CHECK(length(CAST(payload_json AS BLOB)) <= 262144),
                PRIMARY KEY(vehicle_id, record_type)
             ) STRICT, WITHOUT ROWID;
             INSERT INTO current_observations_v54(
                vehicle_id, record_type, observation_id, source_id,
                observed_at_ms, received_at_ms, payload_sha256, payload_json
             )
             SELECT vehicle_id, record_type, observation_id, source_id,
                    observed_at_ms, received_at_ms, payload_sha256, payload_json
               FROM current_observations;
             DROP TABLE current_observations;
             ALTER TABLE current_observations_v54 RENAME TO current_observations;
             PRAGMA user_version = 54;
             COMMIT;",
        )
        .expect("recreate schema 54 fixture");
    if schema == 54 {
        return;
    }

    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE outbound_request_receipts_v53 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                completed_at_ms INTEGER,
                duration_ms INTEGER,
                vehicle_tesla_id INTEGER CHECK(vehicle_tesla_id > 0),
                transport TEXT NOT NULL CHECK(transport IN (
                    'owner_api', 'stream', 'legacy_auth'
                )),
                operation TEXT NOT NULL CHECK(operation IN (
                    'products', 'vehicle_probe', 'vehicle_data', 'token_refresh',
                    'stream_connect', 'stream_subscribe', 'stream_unsubscribe'
                )),
                safety_class TEXT NOT NULL CHECK(safety_class IN (
                    'non_wake_endpoint', 'conditional_read', 'direct_wake_command'
                )),
                precondition TEXT NOT NULL CHECK(precondition IN (
                    'not_required', 'stream_power_confirmed'
                )),
                outcome TEXT NOT NULL CHECK(outcome IN (
                    'started', 'success', 'http_error', 'timeout',
                    'transport_error', 'authentication_rejected',
                    'protocol_error', 'response_too_large', 'cancelled'
                )),
                http_status INTEGER CHECK(http_status BETWEEN 100 AND 599),
                retry_after_seconds INTEGER CHECK(retry_after_seconds >= 0),
                CHECK(
                    (outcome = 'started' AND completed_at_ms IS NULL
                     AND duration_ms IS NULL AND http_status IS NULL
                     AND retry_after_seconds IS NULL)
                    OR
                    (outcome <> 'started' AND completed_at_ms IS NOT NULL
                     AND duration_ms IS NOT NULL
                     AND completed_at_ms >= started_at_ms
                     AND duration_ms >= 0)
                )
             ) STRICT;
             INSERT INTO outbound_request_receipts_v53(
                id, correlation_id, started_at_ms, completed_at_ms,
                duration_ms, vehicle_tesla_id, transport, operation,
                safety_class, precondition, outcome, http_status,
                retry_after_seconds
             )
             SELECT id, correlation_id, started_at_ms, completed_at_ms,
                    duration_ms, vehicle_tesla_id, transport, operation,
                    safety_class, precondition, outcome, http_status,
                    retry_after_seconds
               FROM outbound_request_receipts;
             CREATE TABLE legacy_refresh_receipt_bindings_v53 (
                receipt_id INTEGER PRIMARY KEY NOT NULL
                    REFERENCES outbound_request_receipts_v53(id) ON DELETE CASCADE,
                attempt_id TEXT NOT NULL UNIQUE CHECK(length(attempt_id) = 36),
                input_credential_generation TEXT NOT NULL
                    CHECK(length(input_credential_generation) = 36),
                output_credential_generation TEXT
                    CHECK(output_credential_generation IS NULL
                          OR length(output_credential_generation) = 36),
                CHECK(output_credential_generation IS NULL
                      OR output_credential_generation <> input_credential_generation)
             ) STRICT;
             INSERT INTO legacy_refresh_receipt_bindings_v53(
                receipt_id, attempt_id, input_credential_generation,
                output_credential_generation
             )
             SELECT receipt_id, attempt_id, input_credential_generation,
                    output_credential_generation
               FROM legacy_refresh_receipt_bindings;
             DROP TABLE legacy_refresh_receipt_bindings;
             DROP TABLE outbound_request_receipts;
             ALTER TABLE outbound_request_receipts_v53
                RENAME TO outbound_request_receipts;
             ALTER TABLE legacy_refresh_receipt_bindings_v53
                RENAME TO legacy_refresh_receipt_bindings;
             CREATE INDEX outbound_request_receipts_proof
                ON outbound_request_receipts(
                    correlation_id, id, safety_class, outcome
                );
             CREATE INDEX outbound_request_receipts_retention
                ON outbound_request_receipts(outcome, completed_at_ms, id);
             CREATE UNIQUE INDEX legacy_refresh_receipt_output_generation
                ON legacy_refresh_receipt_bindings(output_credential_generation)
                WHERE output_credential_generation IS NOT NULL;
             DROP TABLE fleet_tokens;
             PRAGMA user_version = 53;
             COMMIT;",
        )
        .expect("recreate schema 53 fixture");
    if schema == 53 {
        return;
    }

    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE lifecycle_open_rows_v52 (
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
             INSERT INTO lifecycle_open_rows_v52(
                source_id, source_table, source_row_id, vehicle_id, car_id,
                domain, parent_source_row_id, row_json
             )
             SELECT source_id, source_table, source_row_id, vehicle_id, car_id,
                    domain, parent_source_row_id, row_json
               FROM lifecycle_open_rows;
             DROP TABLE lifecycle_open_rows;
             ALTER TABLE lifecycle_open_rows_v52 RENAME TO lifecycle_open_rows;
             CREATE INDEX lifecycle_open_rows_vehicle_domain
                ON lifecycle_open_rows(vehicle_id, domain, source_row_id);
             PRAGMA user_version = 52;
             COMMIT;",
        )
        .expect("recreate schema 52 fixture");
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

fn publish_schema_22_fixture(store: &HubStore) -> (SyncManifest, SignedNoOpState) {
    let data = store.database_path().parent().expect("fixture data root");
    let cursor_key =
        crate::teslamate_credentials::load_or_create_cursor_key(data).expect("fixture cursor key");
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
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(source_data)
        .expect("source cursor key");
    let source_lineage = publish_lineage_fixture(&store, &cursor_key);
    let source_cursor = fs::read(crate::teslamate_credentials::cursor_key_path(source_data))
        .expect("source cursor bytes");
    let owner_tokens = crate::credentials::OwnerTokens::from_secret_parts(
        "backup-access".to_owned(),
        "backup-refresh".to_owned(),
    )
    .expect("owner tokens");
    let encryption_key = b"backup exact TeslaMate key";
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(encryption_key, &owner_tokens)
            .expect("encrypt owner tokens");
    let stored_tokens =
        crate::db::TeslaMateLegacyTokenStore::imported(access, refresh).expect("stored tokens");
    crate::teslamate_credentials::replace_key_and_tokens(
        source_data,
        &store,
        encryption_key,
        &stored_tokens,
    )
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
    assert!(!restored.join("secrets").exists());
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
    assert_eq!(
        restored_store
            .lineage_manifest_for_vehicle(source_lineage.vehicle_id)
            .expect("restored lineage"),
        Some(source_lineage),
        "restore sanitation must preserve account-bound vehicle sync lineage"
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
    let restored_tokens = restored_store
        .load_teslamate_legacy_tokens()
        .expect("restored token row")
        .expect("restored credentials");
    assert!(
        crate::teslamate_credentials::load_key_for_tokens(&restored, &restored_tokens).is_err(),
        "default data restore must not recover the credential key"
    );
    assert_eq!(
        fs::read(crate::teslamate_credentials::cursor_key_path(source_data))
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
fn historical_restore_migrates_schema_52_through_54_without_touching_source_or_pack_members() {
    for schema in [52, 53, 54] {
        let (temporary, store) = create_fixture();
        publish_schema_22_fixture(&store);
        let installation_id = store.installation_id().expect("installation ID");
        let backup = temporary.path().join(format!("schema-{schema}-backup"));
        create_data_backup(&store, &backup).expect("create current backup fixture");
        let manifest = reseal_backup_as_schema(&backup, schema);
        verify_data_backup(&backup).expect("verify historical backup");
        let source_before = tree_snapshot(&backup);

        let restored = temporary.path().join(format!("schema-{schema}-restored"));
        restore_data_backup(&backup, &restored).expect("restore historical backup");

        assert_eq!(tree_snapshot(&backup), source_before);
        assert_eq!(
            immutable_database_identity(&restored).expect("current restored identity"),
            installation_id
        );
        let restored_store =
            HubStore::open_immutable_read_only(&restored).expect("current restored store");
        restored_store
            .catalogue_check()
            .expect("current restored catalogue");
        restored_store
            .verify_immutable_snapshot_unchanged()
            .expect("restored catalogue remains immutable");

        for member in manifest
            .members
            .iter()
            .filter(|member| member.path != CATALOGUE_MEMBER)
        {
            let relative = member.path.strip_prefix("data/").unwrap();
            let path = restored.join(relative);
            let metadata = fs::metadata(&path).expect("restored member metadata");
            assert_eq!(permission_mode(&metadata), member.mode);
            assert_eq!(metadata.len(), member.size);
            assert_eq!(sha256_file_hex(&path).unwrap(), member.sha256);
        }
    }
}

#[test]
fn historical_restore_rejects_schema_outside_the_current_range() {
    for schema in [MIN_RESTORABLE_SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
        let (temporary, store) = create_fixture();
        let backup = temporary.path().join(format!("schema-{schema}-backup"));
        create_data_backup(&store, &backup).expect("create backup fixture");
        reseal_backup_as_schema(&backup, schema);
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
    assert_eq!(permission_mode(&backup_noop_metadata), PRIVATE_FILE_MODE);
    assert_eq!(backup_noop_metadata.nlink(), 1);

    let backup_manifest: BackupManifest =
        parse_canonical_json(&fs::read(backup.join(MANIFEST_NAME)).expect("backup manifest bytes"))
            .expect("backup manifest");
    let member = backup_manifest
        .members
        .iter()
        .find(|member| member.path == member_path)
        .expect("manifest no-op member");
    assert_eq!(member.mode, PRIVATE_FILE_MODE);
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
    assert_eq!(
        fs::read(&restored_noop).expect("restored no-op"),
        expected_bytes
    );
    assert_eq!(
        permission_mode(&fs::metadata(restored_noop.parent().unwrap()).unwrap()),
        PRIVATE_DIRECTORY_MODE
    );
    let restored_metadata = fs::metadata(restored_noop).expect("restored no-op metadata");
    assert_eq!(permission_mode(&restored_metadata), PRIVATE_FILE_MODE);
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
    set_mode(&noop_path, PRIVATE_FILE_MODE, "test no-op mode").unwrap();

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
    set_mode(&manifest_path, PRIVATE_FILE_MODE, "test manifest mode").unwrap();

    let marker_path = backup.join(COMPLETION_NAME);
    let marker = CompletionMarker {
        kind: COMPLETION_KIND.to_owned(),
        generation: backup_manifest.generation,
        manifest_sha256: sha256_bytes_hex(&manifest_bytes),
    };
    fs::write(&marker_path, canonical_json(&marker).unwrap()).expect("write resealed marker");
    set_mode(&marker_path, PRIVATE_FILE_MODE, "test marker mode").unwrap();

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
