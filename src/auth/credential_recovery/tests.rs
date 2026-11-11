// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{
    data_recovery::{create_data_backup, restore_data_backup},
    
    legacy_owner_credentials::{
        load_or_create_cursor_key,  
    },
    protocol::{CursorClaims, PROTOCOL_V1, SyncManifest, TRANSPORT_SCHEMA_V1, TransferMode},
    
};

#[test]
fn encrypted_export_round_trips_after_data_only_restore() {
    let temporary = crate::private_tempdir().expect("temporary root");
    let source_data = temporary.path().join("source-data");
    let source = HubStore::initialize(&source_data).expect("source store");
    crate::auth::credential_vault::fixtures::seed_legacy_supplied_pair(&source_data, "access-0001", "refresh-0001", None);
    load_or_create_cursor_key(&source_data).expect("source cursor");
    let source_cursor = load_existing_cursor_key_bytes(&source_data).expect("cursor read").expect("cursor");
    let data_backup = temporary.path().join("data-backup");
    create_data_backup(&source, &data_backup).expect("data backup");
    let restored_data = temporary.path().join("restored-data");
    restore_data_backup(&data_backup, &restored_data).expect("data restore");
    let restored = HubStore::initialize(&restored_data).expect("restored store");
    let recovery_key = [7; RECOVERY_ENCRYPTION_KEY_BYTES];
    let export = temporary.path().join("credentials.tthcr");
    let report = export_credentials(&source, &source_data, &export, &recovery_key).expect("credential export");
    assert!(report.secret_bearing && report.cursor_key_included);
    assert!(!report.pre_upgrade_owner_api_key_included && !report.pre_upgrade_fleet_api_key_included);
    let bytes = fs::read(&export).expect("encrypted bytes");
    assert!(!bytes.windows(source_cursor.len()).any(|part| part == source_cursor.as_slice()));
    restore_credentials(&restored, &restored_data, &export, &recovery_key).expect("credential restore");
    assert_eq!(load_existing_cursor_key_bytes(&restored_data).expect("cursor read").expect("cursor").as_slice(), source_cursor.as_slice());
    for slot in [crate::auth::credential_envelope::CredentialSlot::OwnerApi, crate::auth::credential_envelope::CredentialSlot::FleetApi] {
        assert!(restored.credential_pair_presence(slot).expect("presence").is_none());
    }
}

#[test]
fn wrong_key_tamper_and_existing_secrets_are_rejected() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let data = temporary.path().join("data");
    let store = HubStore::initialize(&data).expect("store");
    load_or_create_cursor_key(&data).expect("cursor");
    let export = temporary.path().join("credentials.tthcr");
    let recovery_key = [9_u8; RECOVERY_ENCRYPTION_KEY_BYTES];
    export_credentials(&store, &data, &export, &recovery_key).expect("export");
    assert!(matches!(
        restore_credentials(
            &store,
            &data,
            &export,
            &[8_u8; RECOVERY_ENCRYPTION_KEY_BYTES]
        ),
        Err(CredentialRecoveryError::AuthenticationFailed)
    ));

    let mut tampered = fs::read(&export).expect("export bytes");
    *tampered.last_mut().expect("ciphertext byte") ^= 1;
    let tampered_path = temporary.path().join("tampered.tthcr");
    fs::write(&tampered_path, tampered).expect("tampered file");
    fs::set_permissions(&tampered_path, fs::Permissions::from_mode(0o600)).expect("tampered mode");
    assert!(matches!(
        restore_credentials(&store, &data, &tampered_path, &recovery_key),
        Err(CredentialRecoveryError::AuthenticationFailed)
    ));
    let cursor = data.join("secrets").join("hub-cursor.key");
    let before = fs::read(&cursor).expect("existing member");
    restore_credentials(&store, &data, &export, &recovery_key).expect("identical members retained");
    assert_eq!(fs::read(&cursor).expect("retained member"), before);
    fs::write(&cursor, [99; 32]).expect("conflicting member");
    assert!(matches!(restore_credentials(&store, &data, &export, &recovery_key), Err(CredentialRecoveryError::SecretsAlreadyExist)));
    assert!(matches!(
        export_credentials(&store, &data, &export, &recovery_key),
        Err(CredentialRecoveryError::DestinationExists)
    ));
}

#[test]
fn wrong_cursor_key_is_rejected_against_manifests_before_secrets_publication() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let data = temporary.path().join("data");
    let store = HubStore::initialize(&data).expect("store");
    let catalogue_key = CursorKey::from_bytes([21; 32]);
    let manifest = empty_manifest(&store, &catalogue_key);
    store
        .publish_manifest(&manifest)
        .expect("manifest catalogue");
    assert!(
        ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = store.credential_pair_presence(crate::auth::credential_envelope::CredentialSlot::OwnerApi).map(|presence| presence.map(|_| (Vec::<u8>::new(), Vec::<u8>::new(), 0_i64, 0_i64, None::<uuid::Uuid>, String::new(), String::new()))); unavailable_value })
            .expect("token query")
            .is_none()
    );
    assert!(
        ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = store.credential_pair_presence(crate::auth::credential_envelope::CredentialSlot::FleetApi).map(|presence| presence.map(|_| (Vec::<u8>::new(), Vec::<u8>::new(), 0_i64, 0_i64, None::<uuid::Uuid>, String::new(), String::new()))); unavailable_value })
            .expect("Fleet token query")
            .is_none()
    );

    let recovery_key = [22; RECOVERY_ENCRYPTION_KEY_BYTES];
    let export = temporary.path().join("wrong-cursor.tthcr");
    write_cursor_only_export(
        &export,
        store.installation_id().expect("installation ID"),
        [23; 32],
        &recovery_key,
    );

    assert!(matches!(
        restore_credentials(&store, &data, &export, &recovery_key),
        Err(CredentialRecoveryError::CatalogueMismatch)
    ));
    assert!(!data.join("secrets").exists());
}

#[test]
fn export_rejects_a_cursor_key_that_does_not_match_the_manifest_catalogue() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let data = temporary.path().join("data");
    let store = HubStore::initialize(&data).expect("store");
    let catalogue_key = load_or_create_cursor_key(&data).expect("catalogue cursor key");
    let manifest = empty_manifest(&store, &catalogue_key);
    store
        .publish_manifest(&manifest)
        .expect("manifest catalogue");
    fs::write(data.join("secrets/hub-cursor.key"), [29; 32]).expect("replace fixture key");
    let export = temporary.path().join("wrong-source-cursor.tthcr");

    assert!(matches!(
        export_credentials(&store, &data, &export, &[30; RECOVERY_ENCRYPTION_KEY_BYTES]),
        Err(CredentialRecoveryError::CatalogueMismatch)
    ));
    assert!(!export.exists());
}

#[test]
fn malformed_manifest_catalogue_is_rejected_before_secrets_publication() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let data = temporary.path().join("data");
    let store = HubStore::initialize(&data).expect("store");
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO sync_manifests(snapshot_id, vehicle_id, head_sequence, manifest_json)
             VALUES (?1, ?2, 0, ?3)",
            (
                Uuid::new_v4().to_string(),
                Uuid::new_v4().to_string(),
                b"not-json".as_slice(),
            ),
        )
        .expect("malformed manifest row");
    let recovery_key = [24; RECOVERY_ENCRYPTION_KEY_BYTES];
    let export = temporary.path().join("malformed-catalogue.tthcr");
    write_cursor_only_export(
        &export,
        store.installation_id().expect("installation ID"),
        [25; 32],
        &recovery_key,
    );

    assert!(matches!(
        restore_credentials(&store, &data, &export, &recovery_key),
        Err(CredentialRecoveryError::CatalogueMismatch)
    ));
    assert!(!data.join("secrets").exists());
}

#[test]
fn cursor_key_restore_allows_an_empty_catalogue() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let data = temporary.path().join("data");
    let store = HubStore::initialize(&data).expect("store");
    let recovery_key = [26; RECOVERY_ENCRYPTION_KEY_BYTES];
    let export = temporary.path().join("empty-catalogue.tthcr");
    write_cursor_only_export(
        &export,
        store.installation_id().expect("installation ID"),
        [27; 32],
        &recovery_key,
    );

    restore_credentials(&store, &data, &export, &recovery_key).expect("empty catalogue restore");
    assert!(data.join("secrets/hub-cursor.key").is_file());
}

fn empty_manifest(store: &HubStore, cursor_key: &CursorKey) -> SyncManifest {
    let installation_id = store.installation_id().expect("installation ID");
    let account_id = Uuid::new_v4();
    let vehicle_id = Uuid::new_v4();
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: TRANSPORT_SCHEMA_V1,
            installation_id,
            account_id,
            vehicle_id,
            generation: 1,
            sequence: 0,
        },
    )
    .expect("terminal cursor");
    SyncManifest {
        protocol: PROTOCOL_V1,
        schema: TRANSPORT_SCHEMA_V1,
        installation_id,
        account_id,
        vehicle_id,
        generation: 1,
        snapshot_id: Uuid::new_v4(),
        mode: TransferMode::FullSnapshot,
        base_sequence: 0,
        head_sequence: 0,
        chunk_count: 0,
        total_compressed_bytes: 0,
        total_uncompressed_bytes: 0,
        total_rows: 0,
        chunks: Vec::new(),
        terminal_cursor,
    }
}

fn write_cursor_only_export(
    destination: &Path,
    installation_id: Uuid,
    cursor_key: [u8; 32],
    recovery_key: &[u8],
) {
    let cipher = recovery_cipher(recovery_key).expect("recovery cipher");
    let mut plaintext =
        encode_payload(installation_id, None, Some(&cursor_key), None).expect("payload");
    let nonce_bytes = [31; NONCE_BYTES];
    let ciphertext = cipher
        .encrypt(
            &Nonce::from(nonce_bytes),
            Payload {
                msg: plaintext.as_slice(),
                aad: FILE_MAGIC,
            },
        )
        .expect("encrypt payload");
    plaintext.zeroize();
    let mut envelope = Zeroizing::new(Vec::new());
    envelope.extend_from_slice(FILE_MAGIC);
    envelope.extend_from_slice(&nonce_bytes);
    envelope.extend_from_slice(&ciphertext);
    publish_private_file(destination, &envelope).expect("publish recovery export");
}

#[path = "tests/g24_m2_recovery.rs"]
pub(in crate::auth) mod g24_m2_recovery;
