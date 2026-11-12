// SPDX-License-Identifier: AGPL-3.0-only

// M06-T09: a Hub restored from bundle and password alone, on a fresh host,
// starts its API without the old host's key export. Restore regenerates the
// cursor key and re-signs the signed sync state; history and packs stay
// byte-identical, and every pre-restore bearer and cursor is refused.
// Synthetic data in temporary stores; no network and no real credential.

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use tower::ServiceExt;

use super::*;
use crate::data_recovery::{
    EncryptedBackupRequest,
    archive_crypto::KdfParams,
    backup_admission::{VolumeTrust, test_seam},
    create_encrypted_backup,
    encrypted_backup::clear_hook,
    restore_encrypted_backup,
};

const PASSWORD: &str = "correct horse battery staple";
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};

/// Tables whose rows are history or its immutable sync structure: restore
/// must leave every row of these byte-identical.
const HISTORY_TABLES: &[&str] = &[
    "vehicles",
    "sources",
    "source_identities",
    "materialised_cars",
    "materialised_drives",
    "materialised_charges",
    "materialised_charge_samples",
    "materialised_positions",
    "materialised_states",
    "materialised_updates",
    "raw_observations",
    "collection_gaps",
    "sync_bases",
    "sync_packs",
    "sync_deltas",
    "v2_base_bindings",
];

struct SignedHub {
    temporary: tempfile::TempDir,
    store: HubStore,
    data: PathBuf,
    key: CursorKey,
    key_bytes: Vec<u8>,
    lineage: LineageManifestV2,
    schema_22: SyncManifest,
    bearer: String,
}

/// A Hub with signed sync history: a v2 lineage (manifest and head) and a
/// current schema 2.2 snapshot with its signed no-op, plus one paired device.
fn signed_hub() -> SignedHub {
    clear_hook();
    test_seam::force(Some(VolumeTrust::Posix));
    let (temporary, store) = create_fixture();
    let data = store.database_path().parent().unwrap().to_path_buf();
    let key = crate::legacy_owner_credentials::load_or_create_cursor_key(&data).expect("key");
    let key_bytes = fs::read(crate::legacy_owner_credentials::cursor_key_path(&data)).unwrap();
    let lineage = publish_lineage_fixture(&store, &key);
    let (schema_22, _) = publish_schema_22_fixture(&store);
    let now = current_epoch_ms().unwrap();
    let invitation = store.create_pairing("Before backup", now - 1, now + 60_000).unwrap();
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "Before backup", now)
        .expect("claim");
    let bearer = access.access_token.as_bearer().to_owned();
    SignedHub {
        temporary,
        store,
        data,
        key,
        key_bytes,
        lineage,
        schema_22,
        bearer,
    }
}

impl SignedHub {
    /// Encrypted backup, then restore onto a fresh data directory.
    fn backup_and_restore(&self) -> PathBuf {
        let destination = self.temporary.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
        let mut request = EncryptedBackupRequest::new(&destination, PASSWORD);
        request.kdf = TEST_KDF;
        request.chunk_bytes = 4096;
        let report = create_encrypted_backup(&self.store, &request).expect("encrypted backup");
        let fresh_host = self.temporary.path().join("fresh-host");
        fs::create_dir(&fresh_host).unwrap();
        fs::set_permissions(&fresh_host, fs::Permissions::from_mode(0o700)).unwrap();
        let restored = fresh_host.join("hub-data");
        restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore");
        restored
    }
}

fn table_dump(data: &Path, table: &str) -> Option<Vec<String>> {
    let connection = open_immutable_catalogue(data).expect("catalogue");
    let exists: bool = connection
        .query_row(
            "SELECT COUNT(*) = 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    if !exists {
        return None;
    }
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let columns = statement.column_count();
    let rows = statement
        .query_map([], |row| {
            let mut line = String::new();
            for index in 0..columns {
                line.push_str(&format!("{:?}|", row.get_ref(index)?));
            }
            Ok(line)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    Some(rows)
}

fn pack_hashes(data: &Path) -> BTreeMap<String, String> {
    fs::read_dir(data.join("packs").join("sha256"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                sha256_file_hex(&entry.path()).unwrap(),
            )
        })
        .collect()
}

/// Every terminal cursor the catalogue holds: manifest rows, retired
/// lineages and heads, each as (where, cursor).
fn catalogue_cursors(data: &Path) -> Vec<(String, OpaqueCursor)> {
    let connection = open_immutable_catalogue(data).expect("catalogue");
    let mut cursors = Vec::new();
    for (table, column) in [
        ("sync_manifests", "manifest_json"),
        ("sync_retired_lineages", "manifest_json"),
    ] {
        let rows: Vec<Vec<u8>> = connection
            .prepare(&format!("SELECT {column} FROM {table}"))
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for bytes in rows {
            if let Ok(manifest) = serde_json::from_slice::<SyncManifest>(&bytes) {
                cursors.push((table.to_owned(), manifest.terminal_cursor));
            } else if let Ok(lineage) = serde_json::from_slice::<LineageManifestV2>(&bytes) {
                cursors.push((table.to_owned(), lineage.terminal_cursor));
            }
        }
    }
    let heads: Vec<String> = connection
        .prepare("SELECT terminal_cursor FROM sync_heads")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for head in heads {
        cursors.push((
            "sync_heads".to_owned(),
            serde_json::from_str(&head).expect("head cursor JSON"),
        ));
    }
    cursors
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

// ---------------------------------------------------------------------------
// restore_revocation (M06-T09)
// ---------------------------------------------------------------------------

#[test]
fn restore_revocation_signed_history_restores_without_the_old_key_export() {
    let hub = signed_hub();
    let source_cursors = catalogue_cursors(&hub.data);
    assert!(
        source_cursors.len() >= 3,
        "the fixture signs a lineage manifest, its head and a schema 2.2 snapshot"
    );
    let restored = hub.backup_and_restore();

    // Start-up: the restored directory does not wait for the old host's
    // credential export (REV-M06-T06 finding 3).
    assert!(
        !crate::runtime::operator::authority::credential_recovery_pending(&restored),
        "a restored Hub with signed sync history must not need restore-recovery-credentials"
    );
    let new_key_bytes =
        fs::read(crate::legacy_owner_credentials::cursor_key_path(&restored)).expect("new key");
    assert_eq!(new_key_bytes.len(), 32);
    assert_ne!(new_key_bytes, hub.key_bytes, "restore regenerates the cursor key");
    let new_key = CursorKey::from_bytes(new_key_bytes.as_slice().try_into().unwrap());
    let key_mode = fs::metadata(crate::legacy_owner_credentials::cursor_key_path(&restored))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(key_mode, 0o600);

    // Every restored cursor is re-signed under the new key with its claims
    // unchanged; none verifies under the old key.
    let restored_cursors = catalogue_cursors(&restored);
    assert_eq!(restored_cursors.len(), source_cursors.len());
    for ((source_place, source), (restored_place, restored_cursor)) in
        source_cursors.iter().zip(&restored_cursors)
    {
        assert_eq!(source_place, restored_place);
        let claims = restored_cursor.verify(&new_key).expect("re-signed cursor");
        assert_eq!(claims, source.verify(&hub.key).expect("source cursor"));
        assert!(restored_cursor.verify(&hub.key).is_err());
        assert!(source.verify(&new_key).is_err(), "an old cursor is refused");
    }

    // The schema 2.2 pair the serve path hands out verifies under the new key.
    let restored_store = HubStore::initialize(&restored).expect("restored store");
    let (manifest_bytes, noop_bytes) = crate::updates_delivery::schema_22_signed_artifacts(
        &restored_store,
        hub.schema_22.vehicle_id,
        &new_key,
    )
    .expect("schema 2.2 artifacts under the new key");
    let served: SyncManifest = serde_json::from_slice(&manifest_bytes).unwrap();
    served.validate_terminal_cursor(&new_key).expect("served cursor");
    assert!(served.validate_terminal_cursor(&hub.key).is_err());
    let noop: SignedNoOpState = serde_json::from_slice(&noop_bytes).unwrap();
    assert_eq!(noop.terminal_cursor, served.terminal_cursor);
    assert!(
        restored_store
            .lineage_manifest_for_vehicle(hub.lineage.vehicle_id)
            .expect("lineage")
            .expect("restored lineage")
            .terminal_cursor
            .verify(&new_key)
            .is_ok()
    );

    // No old cursor string survives anywhere in the restored image.
    let mut image = fs::read(restored.join("hub.sqlite")).unwrap();
    for side in ["hub.sqlite-wal", "hub.sqlite-journal"] {
        if let Ok(bytes) = fs::read(restored.join(side)) {
            image.extend(bytes);
        }
    }
    image.extend(noop_bytes);
    for (_, old) in &source_cursors {
        assert!(!contains(&image, old.as_str().as_bytes()), "an old cursor survived");
    }
}

#[test]
fn restore_revocation_resign_keeps_history_rows_and_pack_hashes() {
    let hub = signed_hub();
    let history: Vec<_> = HISTORY_TABLES
        .iter()
        .map(|table| table_dump(&hub.data, table))
        .collect();
    let packs = pack_hashes(&hub.data);
    let source = open_immutable_catalogue(&hub.data).unwrap();
    let source_heads: Vec<(String, i64, String)> = source
        .prepare("SELECT vehicle_id, head_sequence, head_digest FROM sync_heads ORDER BY vehicle_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let source_manifests: Vec<(String, Vec<u8>)> = source
        .prepare("SELECT snapshot_id, manifest_json FROM sync_manifests ORDER BY snapshot_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(source);
    let source_cursors = catalogue_cursors(&hub.data);

    let restored = hub.backup_and_restore();
    for (table, before) in HISTORY_TABLES.iter().zip(&history) {
        assert_eq!(&table_dump(&restored, table), before, "{table} changed");
    }
    assert_eq!(pack_hashes(&restored), packs, "pack contents changed");

    let new_key = CursorKey::from_bytes(
        fs::read(crate::legacy_owner_credentials::cursor_key_path(&restored))
            .unwrap()
            .as_slice()
            .try_into()
            .unwrap(),
    );
    let restored_catalogue = open_immutable_catalogue(&restored).unwrap();
    let restored_heads: Vec<(String, i64, String)> = restored_catalogue
        .prepare("SELECT vehicle_id, head_sequence, head_digest FROM sync_heads ORDER BY vehicle_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(restored_heads, source_heads, "head sequences and digests are kept");
    let restored_manifests: Vec<(String, Vec<u8>)> = restored_catalogue
        .prepare("SELECT snapshot_id, manifest_json FROM sync_manifests ORDER BY snapshot_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(restored_manifests.len(), source_manifests.len());
    // Each manifest differs from its source only in its cursor string.
    for ((snapshot, before), (restored_snapshot, after)) in
        source_manifests.iter().zip(&restored_manifests)
    {
        assert_eq!(snapshot, restored_snapshot);
        assert_eq!(before.len(), after.len());
        let mut expected = before.clone();
        for (_, old) in &source_cursors {
            let claims = old.verify(&hub.key).unwrap();
            let fresh = OpaqueCursor::issue(&new_key, claims).unwrap();
            let old = old.as_str().as_bytes();
            if let Some(at) = expected.windows(old.len()).position(|window| window == old) {
                expected[at..at + old.len()].copy_from_slice(fresh.as_str().as_bytes());
            }
        }
        assert_eq!(after, &expected, "manifest {snapshot} changed beyond its cursor");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_revocation_restored_hub_serves_and_refuses_pre_restore_access() {
    let hub = signed_hub();
    let restored = hub.backup_and_restore();
    assert!(!crate::runtime::operator::authority::credential_recovery_pending(&restored));
    // The serve path loads the key restore wrote; it never creates another.
    let key = crate::legacy_owner_credentials::load_or_create_cursor_key(&restored).unwrap();
    let store = HubStore::initialize(&restored).expect("restored store");
    let app = crate::server::paired_router(store.clone(), &key);
    let get = |uri: &str, bearer: Option<&str>| {
        let mut request = Request::get(uri);
        if let Some(bearer) = bearer {
            request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        }
        request.body(Body::empty()).unwrap()
    };
    let health = app.clone().oneshot(get("/healthz", None)).await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let refused = app
        .clone()
        .oneshot(get("/v1/vehicles", Some(hub.bearer.as_str())))
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        StatusCode::UNAUTHORIZED,
        "a pre-restore bearer is refused"
    );
    let now = current_epoch_ms().unwrap();
    let invitation = store.create_pairing("Re-paired", now - 1, now + 60_000).unwrap();
    let fresh = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "Re-paired", now)
        .expect("re-pair");
    let accepted = app
        .clone()
        .oneshot(get("/v1/vehicles", Some(fresh.access_token.as_bearer())))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK, "a re-paired device is served");
    // An old cursor does not verify under the key the API signs with.
    assert!(hub.lineage.terminal_cursor.verify(&key).is_err());
    assert!(hub.schema_22.validate_terminal_cursor(&key).is_err());
}
