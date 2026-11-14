// SPDX-License-Identifier: AGPL-3.0-only

// M06-T16: restore re-keys the physical sync state too. A catalogue holding
// only physical signed state gets its key at restore; the pending and
// retained physical admissions are re-signed with their claims and pack bytes
// kept; the stored 1.4 delta transition is invalidated, so its adjacent
// retained base gets a signed restart instead of a 503; and the current
// head's prepared month is re-issued, so its receipt and pack are served
// straight after re-pairing. Synthetic data in temporary stores; no network
// and no real credential.

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::{
    data_recovery::{
        EncryptedBackupRequest,
        archive_crypto::KdfParams,
        backup_admission::{VolumeTrust, test_seam},
        create_encrypted_backup,
        encrypted_backup::clear_hook,
        restore_encrypted_backup,
    },
    db::PendingPhysicalV3Admission,
    manifest_signing::ManifestSigning,
    server::{SUPPORTED_SCHEMAS_HEADER, SYNC_PROFILE_HEADER},
};

const PASSWORD: &str = "correct horse battery staple";
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};
const PROFILE_14: &str = "hub-sync-v1@1.4.0";
const DELTA_FORMAT: &str = "teslatlas-physical-v3-delta-v1";

struct PhysicalHub {
    temporary: tempfile::TempDir,
    store: HubStore,
    data: PathBuf,
    key: CursorKey,
    key_bytes: Vec<u8>,
    retained: PendingPhysicalV3Admission,
    current: PendingPhysicalV3Admission,
    month: (String, i64, i64),
}

/// A Hub whose only signed sync state is physical: a retained head, its
/// adjacent current successor with the stored 1.4 delta transition between
/// them, and the prepared month of each head, all published through the
/// production publication path.
async fn physical_hub() -> PhysicalHub {
    clear_hook();
    test_seam::force(Some(VolumeTrust::Posix));
    let (temporary, store) = create_fixture();
    let data = store.database_path().parent().unwrap().to_path_buf();
    let key = crate::legacy_owner_credentials::load_or_create_cursor_key(&data).expect("key");
    let key_bytes = fs::read(crate::legacy_owner_credentials::cursor_key_path(&data)).unwrap();
    let binding = crate::sync::physical_fragments::tests::registered_admission_binding(&store);
    let now = current_epoch_ms().unwrap();
    let (stage, name, from_ms, to_ms) =
        crate::sync::prepared_map::month_stage_fixture(temporary.path(), now, false);
    let retained = crate::physical_publication::publish_sealed_physical_v3_stage(
        &store,
        &key,
        binding.clone(),
        stage,
    )
    .await
    .expect("first head")
    .admission;
    let (stage, ..) = crate::sync::prepared_map::month_stage_fixture(temporary.path(), now, true);
    let current =
        crate::physical_publication::publish_sealed_physical_v3_stage(&store, &key, binding, stage)
            .await
            .expect("adjacent successor")
            .admission;
    assert_eq!(current.head_sequence, retained.head_sequence + 1);

    let catalogue = open_immutable_catalogue(&data).unwrap();
    let count = |sql: &str| -> i64 { catalogue.query_row(sql, [], |row| row.get(0)).unwrap() };
    assert_eq!(
        count(
            "SELECT (SELECT COUNT(*) FROM sync_manifests) + (SELECT COUNT(*) FROM sync_heads)
                  + (SELECT COUNT(*) FROM sync_retired_lineages)"
        ),
        0,
        "the fixture holds physical signed state only"
    );
    let transition: (String, String) = catalogue
        .query_row(
            "SELECT base_receipt_id, target_receipt_id FROM physical_v3_delta_transitions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("one stored delta transition");
    assert_eq!(
        transition,
        (retained.receipt_id.clone(), current.receipt_id.clone())
    );
    let prepared: i64 = catalogue
        .query_row(
            "SELECT COUNT(*) FROM prepared_map_months WHERE input_receipt_id = ?1",
            [&current.receipt_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(prepared, 1, "the current head has its prepared month");
    drop(catalogue);
    PhysicalHub {
        temporary,
        store,
        data,
        key,
        key_bytes,
        retained,
        current,
        month: (name, from_ms, to_ms),
    }
}

impl PhysicalHub {
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

/// (table, snapshot, receipt, manifest SHA-256, manifest JSON) of every
/// physical admission: the current head first, then the retained ones.
fn admission_rows(data: &Path) -> Vec<(String, String, String, String, Vec<u8>)> {
    let catalogue = open_immutable_catalogue(data).unwrap();
    let mut rows = Vec::new();
    for table in [
        "pending_physical_v3_admissions",
        "retained_physical_v3_admissions",
    ] {
        let mut statement = catalogue
            .prepare(&format!(
                "SELECT snapshot_id, receipt_id, manifest_sha256, manifest_json
                   FROM {table} ORDER BY head_sequence DESC"
            ))
            .unwrap();
        let found = statement
            .query_map([], |row| {
                Ok((
                    table.to_owned(),
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.extend(found);
    }
    rows
}

fn query_rows(data: &Path, sql: &str) -> Vec<String> {
    let catalogue = open_immutable_catalogue(data).unwrap();
    let mut statement = catalogue.prepare(sql).unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            let mut line = String::new();
            for index in 0..columns {
                line.push_str(&format!("{:?}|", row.get_ref(index)?));
            }
            Ok(line)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// The file hash of every snapshot pack the admissions name.
fn snapshot_pack_hashes(
    data: &Path,
    admissions: &[&PendingPhysicalV3Admission],
) -> BTreeMap<String, String> {
    admissions
        .iter()
        .flat_map(|admission| &admission.manifest.chunks)
        .map(|pack| {
            let path = data
                .join("packs")
                .join("sha256")
                .join(format!("{}.sqlite.zst", pack.sha256));
            (pack.sha256.to_string(), sha256_file_hex(&path).unwrap())
        })
        .collect()
}

/// The Ed25519 signature strings of the stored delta and prepared receipts.
fn stored_signatures(data: &Path) -> Vec<String> {
    let catalogue = open_immutable_catalogue(data).unwrap();
    let receipts: Vec<Vec<u8>> = catalogue
        .prepare(
            "SELECT receipt_json FROM physical_v3_delta_transitions
             UNION ALL SELECT receipt_json FROM prepared_map_months",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    receipts
        .iter()
        .map(|bytes| {
            let receipt: Value = serde_json::from_slice(bytes).unwrap();
            receipt["signature"]["signature"]
                .as_str()
                .expect("signed receipt")
                .to_owned()
        })
        .collect()
}

fn catalogue_image(data: &Path) -> Vec<u8> {
    let mut image = fs::read(data.join("hub.sqlite")).unwrap();
    for side in ["hub.sqlite-wal", "hub.sqlite-journal"] {
        if let Ok(bytes) = fs::read(data.join(side)) {
            image.extend(bytes);
        }
    }
    image
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// A device paired after the restore, and the router the restored Hub serves.
fn re_paired(store: &HubStore, key: &CursorKey) -> (Router, String) {
    let now = current_epoch_ms().unwrap();
    let invitation = store
        .create_pairing("Re-paired", now - 1, now + 60_000)
        .unwrap();
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "Re-paired", now)
        .expect("re-pair");
    (
        crate::server::paired_router(store.clone(), key),
        access.access_token.as_bearer().to_owned(),
    )
}

async fn request(app: &Router, bearer: &str, uri: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(SYNC_PROFILE_HEADER, PROFILE_14)
        .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2");
    let body = match body {
        Some(value) => {
            builder = builder
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    let response = app.clone().oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, bytes)
}

/// The 1.4 changes-since request of an App holding `head`, whose signed
/// manifest it identifies by the SHA-256 that `key` gives it.
fn changes_request(key: &CursorKey, head: &PendingPhysicalV3Admission) -> Value {
    let manifest = ManifestSigning::from_cursor_key(key)
        .signed_physical_manifest_document(head)
        .unwrap();
    json!({
        "source": {
            "installation_id": head.installation_id, "account_id": head.account_id,
            "vehicle_id": head.vehicle_id, "generation": head.manifest.generation,
            "selected_car_id": head.selected_car_id,
        },
        "base_receipt_id": head.receipt_id,
        "base_manifest_id": head.snapshot_id.to_string(),
        "base_manifest_sha256": Sha256Digest::of_bytes(&manifest).to_string(),
        "base_manifest_schema": "2.2",
        "from_sequence": head.head_sequence,
        "schema_version_range": {"minimum": "2.2", "maximum": "2.2"},
        "accepted_changed_set_formats": [DELTA_FORMAT],
    })
}

/// The document's Ed25519 signature verifies under the manifest-signing key
/// that `key` derives, over the JCS form of everything but the signature.
fn assert_signed_by(document: &Value, key: &CursorKey) {
    let signing = ManifestSigning::from_cursor_key(key);
    let signature = document["signature"].as_object().expect("signature object");
    assert_eq!(signature["key_id"], signing.key_id());
    let mut payload = document.clone();
    payload.as_object_mut().unwrap().remove("signature");
    let canonical = serde_jcs::to_vec(&payload).unwrap();
    assert_eq!(
        signature["signed_payload_sha256"],
        Sha256Digest::of_bytes(&canonical).to_string()
    );
    let public: [u8; 32] = hex::decode(signing.verifying_key_hex())
        .unwrap()
        .try_into()
        .unwrap();
    let bytes = STANDARD
        .decode(signature["signature"].as_str().unwrap())
        .unwrap();
    VerifyingKey::from_bytes(&public)
        .unwrap()
        .verify_strict(&canonical, &Signature::from_slice(&bytes).unwrap())
        .expect("signed by the restored key");
}

/// The identity a prepared pack's metadata names: artefact, vehicle, input
/// manifest, input receipt and input sequence.
type PreparedIdentity = (String, String, String, String, i64);

/// The rows of one prepared pack: its metadata identity, and its month and
/// tile rows as debug text.
fn prepared_pack_rows(bytes: &[u8], scratch: &Path) -> (PreparedIdentity, Vec<String>, Vec<String>) {
    let image = tempfile::NamedTempFile::new_in(scratch).unwrap();
    fs::write(image.path(), zstd::stream::decode_all(bytes).unwrap()).unwrap();
    let pack = rusqlite::Connection::open_with_flags(
        image.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let rows = |sql: &str| -> Vec<String> {
        let mut statement = pack.prepare(sql).unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                let mut line = String::new();
                for index in 0..columns {
                    line.push_str(&format!("{:?}|", row.get_ref(index)?));
                }
                Ok(line)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let identity = pack
        .query_row(
            "SELECT artifact_id, vehicle_id, input_manifest_id, input_receipt_id, input_sequence
               FROM prepared_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    (
        identity,
        rows("SELECT * FROM map_months ORDER BY month"),
        rows("SELECT * FROM map_tiles ORDER BY month, zoom, tile_x, tile_y"),
    )
}

// ---------------------------------------------------------------------------
// restore_revocation (M06-T16)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_revocation_physical_only_catalogue_is_re_keyed_with_claims_and_packs_kept() {
    let hub = physical_hub().await;
    let source_rows = admission_rows(&hub.data);
    assert_eq!(source_rows.len(), 2);
    let snapshot_packs = snapshot_pack_hashes(&hub.data, &[&hub.retained, &hub.current]);
    let pending_packs = query_rows(&hub.data, "SELECT * FROM pending_physical_v3_packs ORDER BY sha256");
    let retained_pack_rows = "SELECT ordinal, sha256, relative_path, compressed_bytes,
                                     uncompressed_bytes
                                FROM retained_physical_v3_packs ORDER BY ordinal";
    let retained_packs = query_rows(&hub.data, retained_pack_rows);
    let old_signatures = stored_signatures(&hub.data);
    assert!(old_signatures.len() >= 2, "a delta and a prepared receipt are signed");

    let restored = hub.backup_and_restore();

    // A catalogue holding only physical signed state gets its key at restore.
    let key_path = crate::legacy_owner_credentials::cursor_key_path(&restored);
    let key_bytes = fs::read(&key_path).expect("restore writes the key of physical signed state");
    assert_ne!(key_bytes, hub.key_bytes, "restore regenerates the cursor key");
    assert_eq!(fs::metadata(&key_path).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(!crate::runtime::operator::authority::credential_recovery_pending(&restored));
    let key = CursorKey::from_bytes(key_bytes.as_slice().try_into().unwrap());

    // Each admission is re-signed: its claims are kept, only the cursor string
    // changes in its manifest, and its manifest hash and receipt are derived
    // again from the re-signed manifest.
    let restored_rows = admission_rows(&restored);
    assert_eq!(restored_rows.len(), source_rows.len());
    for (before, after) in source_rows.iter().zip(&restored_rows) {
        assert_eq!((&before.0, &before.1), (&after.0, &after.1), "same table and snapshot");
        let old: SyncManifest = serde_json::from_slice(&before.4).unwrap();
        let new: SyncManifest = serde_json::from_slice(&after.4).unwrap();
        let claims = old.terminal_cursor.verify(&hub.key).expect("source cursor");
        assert_eq!(new.terminal_cursor.verify(&key).expect("re-signed cursor"), claims);
        assert!(new.terminal_cursor.verify(&hub.key).is_err());
        assert!(old.terminal_cursor.verify(&key).is_err(), "an old cursor is refused");
        let (old_cursor, new_cursor) = (
            old.terminal_cursor.as_str().as_bytes(),
            new.terminal_cursor.as_str().as_bytes(),
        );
        let at = before
            .4
            .windows(old_cursor.len())
            .position(|window| window == old_cursor)
            .unwrap();
        let mut expected = before.4.clone();
        expected[at..at + old_cursor.len()].copy_from_slice(new_cursor);
        assert_eq!(after.4, expected, "{} changed beyond its cursor", after.0);
        let identity =
            crate::db::physical_v3_admission_from_manifest(&new, hub.current.selected_car_id)
                .unwrap();
        assert_eq!(after.2, identity.receipt_id);
        assert_eq!(after.3, identity.manifest_sha256.to_string());
        assert_ne!(after.2, before.2, "the receipt names the re-signed manifest");
    }

    // The production readers accept the re-keyed rows and their pack files.
    let store = HubStore::initialize(&restored).expect("restored store");
    let vehicle = hub.current.vehicle_id;
    let current = store
        .pending_physical_v3_admission_for_vehicle(vehicle)
        .unwrap()
        .expect("current head");
    assert_eq!(current.receipt_id, restored_rows[0].2);
    assert_eq!(current.snapshot_id, hub.current.snapshot_id);
    assert_eq!(current.manifest.chunks, hub.current.manifest.chunks);
    let retained = store
        .retained_physical_v3_admission_for_receipt_at(
            vehicle,
            &restored_rows[1].2,
            current_epoch_ms().unwrap(),
            true,
        )
        .unwrap()
        .expect("retained base")
        .admission;
    assert_eq!(retained.snapshot_id, hub.retained.snapshot_id);
    assert_eq!(retained.manifest.chunks, hub.retained.manifest.chunks);
    assert_eq!(
        snapshot_pack_hashes(&restored, &[&retained, &current]),
        snapshot_packs,
        "snapshot pack bytes are kept"
    );
    assert_eq!(
        query_rows(&restored, "SELECT * FROM pending_physical_v3_packs ORDER BY sha256"),
        pending_packs
    );
    assert_eq!(query_rows(&restored, retained_pack_rows), retained_packs);
    assert_eq!(
        query_rows(&restored, "SELECT DISTINCT receipt_id FROM retained_physical_v3_packs"),
        query_rows(&restored, "SELECT receipt_id FROM retained_physical_v3_admissions"),
        "retained packs follow their renamed receipt"
    );
    assert_eq!(retained.receipt_id, restored_rows[1].2);

    // The delta transition is invalidated; only the current head's prepared
    // month remains, under its renamed receipt.
    assert!(query_rows(&restored, "SELECT * FROM physical_v3_delta_transitions").is_empty());
    assert!(query_rows(&restored, "SELECT * FROM physical_v3_delta_packs").is_empty());
    let catalogue = open_immutable_catalogue(&restored).unwrap();
    let prepared: Vec<String> = catalogue
        .prepare("SELECT input_receipt_id FROM prepared_map_months")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(prepared, vec![current.receipt_id.clone()]);
    drop(catalogue);

    // No old cursor and no old signature survives in the restored catalogue.
    let image = catalogue_image(&restored);
    for row in &source_rows {
        let manifest: SyncManifest = serde_json::from_slice(&row.4).unwrap();
        assert!(
            !contains(&image, manifest.terminal_cursor.as_str().as_bytes()),
            "an old physical cursor survived"
        );
    }
    for signature in &old_signatures {
        assert!(!contains(&image, signature.as_bytes()), "an old signature survived");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_revocation_adjacent_retained_base_gets_a_signed_restart_not_a_503() {
    let hub = physical_hub().await;
    let restored = hub.backup_and_restore();
    // The serve path loads the key restore wrote (or, before M06-T16, makes one).
    let key = crate::legacy_owner_credentials::load_or_create_cursor_key(&restored).unwrap();
    let store = HubStore::initialize(&restored).expect("restored store");
    let (app, bearer) = re_paired(&store, &key);
    let vehicle = hub.current.vehicle_id;
    let current = store
        .pending_physical_v3_control_admission_for_vehicle(vehicle)
        .unwrap()
        .expect("current head");
    let retained_receipt: String = open_immutable_catalogue(&restored)
        .unwrap()
        .query_row(
            "SELECT receipt_id FROM retained_physical_v3_admissions WHERE vehicle_id = ?1",
            [vehicle.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let retained = store
        .retained_physical_v3_admission_for_receipt_at(
            vehicle,
            &retained_receipt,
            current_epoch_ms().unwrap(),
            true,
        )
        .unwrap()
        .expect("retained base")
        .admission;
    assert_eq!(retained.head_sequence + 1, current.head_sequence, "an adjacent base");

    let (status, manifest) =
        request(&app, &bearer, &format!("/v1/vehicles/{vehicle}/sync/manifest"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        manifest,
        ManifestSigning::from_cursor_key(&key)
            .signed_physical_manifest_document(&current)
            .unwrap()
    );

    let changes = format!("/v1/vehicles/{vehicle}/sync/changes-since");
    let (status, raw) =
        request(&app, &bearer, &changes, Some(changes_request(&key, &retained))).await;
    assert_ne!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the stored adjacent transition must not answer 503 after a restore"
    );
    assert_eq!(status, StatusCode::CONFLICT);
    let restart: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(restart["kind"], "rebase_required");
    assert_eq!(restart["requested_base"]["receipt_id"], retained.receipt_id);
    assert_eq!(restart["replacement"]["receipt_id"], current.receipt_id);
    assert_eq!(restart["retry_request"]["base_receipt_id"], current.receipt_id);
    assert_signed_by(&restart, &key);

    // A checkpoint held before the restore is unknown, not unavailable.
    let (status, _) =
        request(&app, &bearer, &changes, Some(changes_request(&hub.key, &hub.retained))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The retry the restart names is answered.
    let (status, raw) =
        request(&app, &bearer, &changes, Some(changes_request(&key, &current))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<Value>(&raw).unwrap()["kind"], "no_op");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_revocation_prepared_receipt_and_pack_are_served_straight_after_re_pairing() {
    let hub = physical_hub().await;
    let (source_receipt, source_pack): (Vec<u8>, String) = open_immutable_catalogue(&hub.data)
        .unwrap()
        .query_row(
            "SELECT receipt_json, pack_sha256 FROM prepared_map_months
              WHERE input_receipt_id = ?1",
            [&hub.current.receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let source_receipt: Value = serde_json::from_slice(&source_receipt).unwrap();
    let source_pack = fs::read(
        hub.data
            .join("packs")
            .join("sha256")
            .join(format!("{source_pack}.sqlite.zst")),
    )
    .unwrap();
    let (_, source_months, source_tiles) = prepared_pack_rows(&source_pack, hub.temporary.path());
    assert!(!source_tiles.is_empty(), "the fixture month has map data");

    let restored = hub.backup_and_restore();
    let key = crate::legacy_owner_credentials::load_or_create_cursor_key(&restored).unwrap();
    let store = HubStore::initialize(&restored).expect("restored store");
    let (app, bearer) = re_paired(&store, &key);
    let vehicle = hub.current.vehicle_id;
    let current = store
        .pending_physical_v3_control_admission_for_vehicle(vehicle)
        .unwrap()
        .expect("current head");
    let (name, from_ms, to_ms) = &hub.month;
    let id = crate::sync::prepared_map::artifact_id(&current, name, *from_ms, *to_ms);

    let (status, raw) = request(
        &app,
        &bearer,
        &format!("/v1/vehicles/{vehicle}/sync/prepared-artefacts/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the prepared receipt is served without an import");
    let receipt: Value = serde_json::from_slice(&raw).unwrap();
    assert_signed_by(&receipt, &key);
    assert_eq!(receipt["artifact_id"], id);
    assert_eq!(receipt["generation"]["generation_id"], id);
    assert_eq!(receipt["source"]["input_receipt_id"], current.receipt_id);
    // Every claim but the renamed identity and the rebuilt pack is kept.
    let claims = |document: &Value| {
        let mut document = document.clone();
        let object = document.as_object_mut().unwrap();
        for member in ["artifact_id", "signature", "pack"] {
            object.remove(member);
        }
        document["generation"]["generation_id"] = Value::Null;
        document["source"]["input_receipt_id"] = Value::Null;
        document
    };
    assert_eq!(claims(&receipt), claims(&source_receipt));
    assert_eq!(receipt["pack"]["media_type"], source_receipt["pack"]["media_type"]);

    let digest = receipt["pack"]["sha256"].as_str().unwrap().to_owned();
    let (status, bytes) =
        request(&app, &bearer, &format!("/v1/packs/sha256/{digest}.sqlite.zst"), None).await;
    assert_eq!(status, StatusCode::OK, "the prepared pack is served without an import");
    assert_eq!(Sha256Digest::of_bytes(&bytes).to_string(), digest);
    assert_eq!(
        u64::try_from(bytes.len()).unwrap(),
        receipt["pack"]["compressed_bytes"].as_u64().unwrap()
    );
    // The pack carries the receipt's identity and the same month and tiles.
    let (identity, months, tiles) = prepared_pack_rows(&bytes, hub.temporary.path());
    assert_eq!(
        identity,
        (
            id.clone(),
            vehicle.to_string(),
            current.snapshot_id.to_string(),
            current.receipt_id.clone(),
            i64::try_from(current.head_sequence).unwrap(),
        )
    );
    assert_eq!(months, source_months);
    assert_eq!(tiles, source_tiles);
}
