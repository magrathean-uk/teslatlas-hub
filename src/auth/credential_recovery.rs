// SPDX-License-Identifier: AGPL-3.0-only

//! Explicit, separately encrypted recovery of the Hub's local key material.
//!
//! Normal data backups deliberately exclude these keys. This format is
//! secret-bearing even though its payload is authenticated and encrypted.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use rustix::{
    fs::{CWD, FileType, Mode, OFlags, RenameFlags, fstat, open, renameat_with},
    process::getuid,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    db::{HubStore, StoreError},
    
    legacy_owner_credentials::{
        SourceDbCredentialError, load_existing_cursor_key_bytes,  
    },
    protocol::{
        CursorKey, LineageDelta, LineageManifestV2, OpaqueCursor, PROTOCOL_V1, ProtocolLimits,
        SyncManifest, canonical_delta_chain_digest,
    },
    source_token_envelope::{SourceDbTokenError, decrypt_legacy_owner_tokens},
};

pub const RECOVERY_ENCRYPTION_KEY_BYTES: usize = 32;
#[cfg(debug_assertions)]
thread_local! { static RESTORE_FLOOR: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) }; }
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn with_restore_floor_for_test<T>(floor: i32, work: impl FnOnce() -> T) -> T {
    struct Reset(Option<i32>);
    impl Drop for Reset { fn drop(&mut self) { RESTORE_FLOOR.with(|slot| slot.set(self.0)); } }
    let reset = Reset(RESTORE_FLOOR.with(|slot| slot.replace(Some(floor))));
    let result = work(); drop(reset); result
}
fn restore_floor() -> i32 {
    #[cfg(debug_assertions)]
    if let Some(floor) = RESTORE_FLOOR.with(std::cell::Cell::get) { return floor; }
    super::credential_migration::store_floor()
}
const FILE_MAGIC: &[u8] = b"TESLATLAS-HUB-CREDENTIAL-RECOVERY-V1\0";
const NONCE_BYTES: usize = 12;
const AUTH_TAG_BYTES: usize = 16;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const MAX_SOURCE_DB_KEY_BYTES: usize = 16 * 1024;
const MAX_PLAINTEXT_BYTES: usize = 16 + 1 + 2 + MAX_SOURCE_DB_KEY_BYTES + 32 + 32;
const MAX_EXPORT_BYTES: usize =
    FILE_MAGIC.len() + NONCE_BYTES + MAX_PLAINTEXT_BYTES + AUTH_TAG_BYTES;
const SOURCE_DB_KEY_FLAG: u8 = 1 << 0;
const CURSOR_KEY_FLAG: u8 = 1 << 1;
const FLEET_KEY_FLAG: u8 = 1 << 2;
const KNOWN_FLAGS: u8 = SOURCE_DB_KEY_FLAG | CURSOR_KEY_FLAG | FLEET_KEY_FLAG;

#[derive(Debug, Error)]
pub enum CredentialRecoveryError {
    #[error(transparent)]
    Vault(super::credential_sign_in::CredentialCommandError),
    #[error("credential-recovery encryption key must be exactly 32 bytes")]
    InvalidRecoveryKey,
    #[error("credential-recovery export contains no recoverable key material")]
    NoKeyMaterial,
    #[error("credential-recovery file already exists or has an unsafe type")]
    DestinationExists,
    #[error("credential-recovery source must be an owned regular file with mode 0600")]
    UnsafeSource,
    #[error("credential-recovery payload is invalid: {0}")]
    InvalidPayload(&'static str),
    #[error("credential-recovery payload authentication failed")]
    AuthenticationFailed,
    #[error("credential-recovery installation does not match this Hub data")]
    InstallationMismatch,
    #[error("credential-recovery keys do not match the restored catalogue")]
    CatalogueMismatch,
    #[error("operating-system entropy is unavailable for credential recovery")]
    EntropyUnavailable,
    #[error("credential-recovery restore refuses to replace an existing secrets directory")]
    SecretsAlreadyExist,
    #[error("credential-recovery I/O failed while {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Credential(#[from] SourceDbCredentialError),
    #[error(transparent)]
    Token(#[from] SourceDbTokenError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRecoveryReport {
    pub status: String,
    pub path: PathBuf,
    pub installation_id: Uuid,
    pub secret_bearing: bool,
    pub encryption: String,
    pub cursor_key_included: bool,
    pub pre_upgrade_owner_api_key_included: bool,
    pub pre_upgrade_fleet_api_key_included: bool,
}

struct DecodedPayload {
    installation_id: Uuid,
    source_db_key: Option<Zeroizing<Vec<u8>>>,
    cursor_key: Option<Zeroizing<Vec<u8>>>,
    fleet_key: Option<Zeroizing<Vec<u8>>>,
}

/// Export only local decryption/signing keys into a separately encrypted file.
pub fn export_credentials(
    store: &HubStore,
    data_dir: &Path,
    destination: &Path,
    recovery_key: &[u8],
) -> Result<CredentialRecoveryReport, CredentialRecoveryError> {
    let cipher = recovery_cipher(recovery_key)?;
    let installation_id = store.installation_id()?;
    let stored_tokens = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = Ok(None); unavailable_value })?;
    let source_db_key = match stored_tokens.as_ref() {
        Some(tokens) => {
            let key = ({ let unavailable_value: Result<zeroize::Zeroizing<Vec<u8>>, crate::legacy_owner_credentials::SourceDbCredentialError> = Ok(zeroize::Zeroizing::new(Vec::new())); unavailable_value })?;
            drop(decrypt_legacy_owner_tokens(
                key.as_slice(),
                tokens.0.as_slice(),
                tokens.1.as_slice(),
            )?);
            Some(key)
        }
        None => None,
    };
    let cursor_key = load_existing_cursor_key_bytes(data_dir)?;
    validate_cursor_key_catalogue(store, cursor_key.as_deref().map(Vec::as_slice))?;
    let fleet_tokens = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = Ok(None); unavailable_value })?;
    let fleet_key = if let Some(tokens) = fleet_tokens.as_ref() {
        match ({ let unavailable_value: Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::fleet_credentials::FleetCredentialError> = Ok(None); unavailable_value })
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?
        {
            Some(key) => {
                drop(decrypt_legacy_owner_tokens(
                    &key,
                    tokens.0.as_slice(),
                    tokens.1.as_slice(),
                )?);
                Some(key)
            }
            None => {
                let cursor = cursor_key
                    .as_ref()
                    .ok_or(CredentialRecoveryError::CatalogueMismatch)?;
                let legacy = fleet_encryption_key(cursor)?;
                drop(decrypt_legacy_owner_tokens(
                    legacy.as_ref(),
                    tokens.0.as_slice(),
                    tokens.1.as_slice(),
                )?);
                None
            }
        }
    } else {
        None
    };
    if source_db_key.is_none() && cursor_key.is_none() && fleet_key.is_none() {
        return Err(CredentialRecoveryError::NoKeyMaterial);
    }

    let mut plaintext = encode_payload(
        installation_id,
        source_db_key.as_ref().map(|key| key.as_slice()),
        cursor_key.as_deref().map(Vec::as_slice),
        fleet_key.as_deref().map(Vec::as_slice),
    )?;
    let mut nonce_bytes = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut nonce_bytes).map_err(|_| CredentialRecoveryError::EntropyUnavailable)?;
    let ciphertext = cipher
        .encrypt(
            &Nonce::from(nonce_bytes),
            Payload {
                msg: plaintext.as_slice(),
                aad: FILE_MAGIC,
            },
        )
        .map_err(|_| CredentialRecoveryError::AuthenticationFailed)?;
    plaintext.zeroize();

    let mut envelope = Zeroizing::new(Vec::with_capacity(
        FILE_MAGIC.len() + NONCE_BYTES + ciphertext.len(),
    ));
    envelope.extend_from_slice(FILE_MAGIC);
    envelope.extend_from_slice(&nonce_bytes);
    envelope.extend_from_slice(&ciphertext);
    publish_private_file(destination, &envelope)?;

    Ok(report(
        "exported",
        destination,
        installation_id,
        source_db_key.is_some(),
        cursor_key.is_some(),
        fleet_key.is_some(),
    ))
}

/// Restore keys into a data-only restore. Existing key material is never
/// replaced, and the exact installation identity must match.
pub fn restore_credentials(
    store: &HubStore,
    data_dir: &Path,
    source: &Path,
    recovery_key: &[u8],
) -> Result<CredentialRecoveryReport, CredentialRecoveryError> {
    {
        let cipher = recovery_cipher(recovery_key)?;
        let envelope = read_private_file(source)?;
        let payload = decode_envelope(&cipher, &envelope)?;
        if payload.installation_id != store.installation_id()? { return Err(CredentialRecoveryError::InstallationMismatch); }
        validate_cursor_key_catalogue(store, payload.cursor_key.as_deref().map(Vec::as_slice))?;
    }
    #[cfg(test)]
    let vault = super::credential_vault::fixtures::open_fixture_vault(store.clone(), data_dir);
    #[cfg(not(test))]
    let admission = crate::hub_user_process::AdmittedUserHub::admit(data_dir)
        .map_err(|_| CredentialRecoveryError::Vault(super::credential_sign_in::CredentialCommandError::new("sensitive_access_unavailable")))?;
    #[cfg(not(test))]
    let vault = super::credential_vault::CredentialVault::open(store.clone(), std::sync::Arc::clone(&admission), data_dir, Default::default())
        .map_err(|error| CredentialRecoveryError::Vault(error.into()))?;
    vault.restore_credentials(source, recovery_key)
}

/// Restore through the command's existing admission and sole vault.
pub fn restore_credentials_from_entry(
    entry: &crate::CredentialVaultEntry,
    source: &Path,
    recovery_key: &[u8],
) -> Result<CredentialRecoveryReport, CredentialRecoveryError> {
    entry.vault().restore_credentials(source, recovery_key)
}

fn report(
    status: &'static str,
    path: &Path,
    installation_id: Uuid,
    pre_upgrade_owner_api_key_included: bool,
    cursor_key_included: bool,
    pre_upgrade_fleet_api_key_included: bool,
) -> CredentialRecoveryReport {
    CredentialRecoveryReport {
        status: status.to_owned(),
        path: path.to_path_buf(),
        installation_id,
        secret_bearing: true,
        encryption: "AES-256-GCM".to_owned(),
        cursor_key_included,
        pre_upgrade_owner_api_key_included,
        pre_upgrade_fleet_api_key_included,
    }
}

fn recovery_cipher(recovery_key: &[u8]) -> Result<Aes256Gcm, CredentialRecoveryError> {
    if recovery_key.len() != RECOVERY_ENCRYPTION_KEY_BYTES {
        return Err(CredentialRecoveryError::InvalidRecoveryKey);
    }
    Aes256Gcm::new_from_slice(recovery_key).map_err(|_| CredentialRecoveryError::InvalidRecoveryKey)
}

fn fleet_encryption_key(cursor_key: &[u8]) -> Result<Zeroizing<[u8; 32]>, CredentialRecoveryError> {
    let cursor_key: [u8; 32] = cursor_key
        .try_into()
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
    Ok(Zeroizing::new(
        CursorKey::from_bytes(cursor_key).fleet_credential_encryption_key(),
    ))
}

fn validate_cursor_key_catalogue(
    store: &HubStore,
    candidate: Option<&[u8]>,
) -> Result<(), CredentialRecoveryError> {
    let connection = store.open()?;
    let mut manifest_statement = connection
        .prepare("SELECT manifest_json FROM sync_manifests ORDER BY snapshot_id")
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
    let mut manifest_rows = manifest_statement
        .query([])
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
    let mut manifests = Vec::new();
    while let Some(row) = manifest_rows
        .next()
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?
    {
        let encoded: Vec<u8> = row
            .get(0)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        manifests.push(encoded);
    }
    drop(manifest_rows);
    drop(manifest_statement);

    let head_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM sync_heads", [], |row| row.get(0))
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
    if manifests.is_empty() && head_count == 0 {
        return Ok(());
    }
    let candidate: [u8; 32] = candidate
        .ok_or(CredentialRecoveryError::CatalogueMismatch)?
        .try_into()
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
    let cursor_key = CursorKey::from_bytes(candidate);
    let installation_id = store.installation_id()?;
    for manifest in &manifests {
        validate_catalogued_cursor_record(manifest, &cursor_key, installation_id)?;
    }

    let mut head_statement = connection
        .prepare(
            "SELECT heads.vehicle_id, heads.base_snapshot_id, heads.head_sequence,
                    heads.terminal_cursor, manifests.manifest_json
               FROM sync_heads AS heads
               LEFT JOIN sync_manifests AS manifests
                 ON manifests.snapshot_id = heads.base_snapshot_id
              ORDER BY heads.vehicle_id",
        )
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
    let mut head_rows = head_statement
        .query([])
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
    while let Some(row) = head_rows
        .next()
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?
    {
        let vehicle_id: String = row
            .get(0)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        let base_snapshot_id: String = row
            .get(1)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        let head_sequence: i64 = row
            .get(2)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        let terminal_cursor: String = row
            .get(3)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        let base_manifest: Vec<u8> = row
            .get(4)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        let vehicle_id = vehicle_id
            .parse::<Uuid>()
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        let base_snapshot_id = base_snapshot_id
            .parse::<Uuid>()
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        let head_sequence =
            u64::try_from(head_sequence).map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        let terminal_cursor: OpaqueCursor = serde_json::from_str(&terminal_cursor)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        let claims = terminal_cursor
            .verify(&cursor_key)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        if let Ok(base_manifest) = serde_json::from_slice::<SyncManifest>(&base_manifest) {
            if base_manifest.snapshot_id != base_snapshot_id
                || base_manifest.vehicle_id != vehicle_id
                || base_manifest.installation_id != installation_id
                || claims.protocol != base_manifest.protocol
                || claims.schema != base_manifest.schema
                || claims.installation_id != base_manifest.installation_id
                || claims.account_id != base_manifest.account_id
                || claims.vehicle_id != vehicle_id
                || claims.generation != base_manifest.generation
                || claims.sequence != head_sequence
            {
                return Err(CredentialRecoveryError::CatalogueMismatch);
            }
        } else if let Ok(lineage) = serde_json::from_slice::<LineageManifestV2>(&base_manifest) {
            if lineage.base.snapshot_id != base_snapshot_id
                || lineage.vehicle_id != vehicle_id
                || lineage.installation_id != installation_id
                || claims.protocol != PROTOCOL_V1
                || claims.schema != lineage.schema
                || claims.installation_id != lineage.installation_id
                || claims.account_id != lineage.account_id
                || claims.vehicle_id != vehicle_id
                || claims.generation != lineage.generation
                || claims.sequence != head_sequence
            {
                return Err(CredentialRecoveryError::CatalogueMismatch);
            }
        } else {
            return Err(CredentialRecoveryError::CatalogueMismatch);
        }
    }
    Ok(())
}

fn validate_catalogued_cursor_record(
    encoded: &[u8],
    cursor_key: &CursorKey,
    installation_id: Uuid,
) -> Result<(), CredentialRecoveryError> {
    if let Ok(manifest) = serde_json::from_slice::<SyncManifest>(encoded) {
        if manifest.installation_id != installation_id
            || manifest.validate().is_err()
            || manifest.validate_terminal_cursor(cursor_key).is_err()
        {
            return Err(CredentialRecoveryError::CatalogueMismatch);
        }
        return Ok(());
    }
    if let Ok(lineage) = serde_json::from_slice::<LineageManifestV2>(encoded) {
        let claims = lineage
            .terminal_cursor
            .verify(cursor_key)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        if lineage.installation_id != installation_id
            || lineage.validate().is_err()
            || claims.protocol != PROTOCOL_V1
            || claims.schema != lineage.schema
            || claims.installation_id != lineage.installation_id
            || claims.account_id != lineage.account_id
            || claims.vehicle_id != lineage.vehicle_id
            || claims.generation != lineage.generation
            || claims.sequence != lineage.head_sequence
        {
            return Err(CredentialRecoveryError::CatalogueMismatch);
        }
        return Ok(());
    }
    let delta: LineageDelta =
        serde_json::from_slice(encoded).map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
    if delta.pack.validate(ProtocolLimits::default()).is_err()
        || delta.from_sequence >= delta.to_sequence
        || delta.pack_digest != delta.pack.sha256
        || delta.chain_digest
            != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack_digest)
        || delta.pack.sequence.from_exclusive != delta.from_sequence
        || delta.pack.sequence.to_inclusive != delta.to_sequence
    {
        return Err(CredentialRecoveryError::CatalogueMismatch);
    }
    Ok(())
}

fn encode_payload(
    installation_id: Uuid,
    source_db_key: Option<&[u8]>,
    cursor_key: Option<&[u8]>,
    fleet_key: Option<&[u8]>,
) -> Result<Zeroizing<Vec<u8>>, CredentialRecoveryError> {
    let mut flags = 0_u8;
    let source_db_key: Option<&[u8]> = if source_db_key.is_some() { None } else { source_db_key };
    if source_db_key.is_some() {
        flags |= SOURCE_DB_KEY_FLAG;
    }
    if cursor_key.is_some() {
        flags |= CURSOR_KEY_FLAG;
    }
    let fleet_key: Option<&[u8]> = if fleet_key.is_some() { None } else { fleet_key };
    if fleet_key.is_some() {
        flags |= FLEET_KEY_FLAG;
    }
    let source_db_key = source_db_key.unwrap_or_default();
    if source_db_key.len() > MAX_SOURCE_DB_KEY_BYTES {
        return Err(CredentialRecoveryError::InvalidPayload(
            "Legacy owner token key is too large",
        ));
    }
    if cursor_key.is_some_and(|key| key.len() != 32) {
        return Err(CredentialRecoveryError::InvalidPayload(
            "cursor key length is invalid",
        ));
    }
    if fleet_key.is_some_and(|key| key.len() != 32) {
        return Err(CredentialRecoveryError::InvalidPayload(
            "Fleet key length is invalid",
        ));
    }
    let key_length = u16::try_from(source_db_key.len()).map_err(|_| {
        CredentialRecoveryError::InvalidPayload("Legacy owner token key length is invalid")
    })?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_PLAINTEXT_BYTES));
    bytes.extend_from_slice(installation_id.as_bytes());
    bytes.push(flags);
    bytes.extend_from_slice(&key_length.to_be_bytes());
    bytes.extend_from_slice(source_db_key);
    if let Some(cursor_key) = cursor_key {
        bytes.extend_from_slice(cursor_key);
    }
    if let Some(fleet_key) = fleet_key {
        bytes.extend_from_slice(fleet_key);
    }
    Ok(bytes)
}

fn decode_envelope(
    cipher: &Aes256Gcm,
    envelope: &[u8],
) -> Result<DecodedPayload, CredentialRecoveryError> {
    let minimum = FILE_MAGIC.len() + NONCE_BYTES + AUTH_TAG_BYTES + 16 + 1 + 2;
    if envelope.len() < minimum || !envelope.starts_with(FILE_MAGIC) {
        return Err(CredentialRecoveryError::InvalidPayload(
            "header or length is invalid",
        ));
    }
    let nonce_start = FILE_MAGIC.len();
    let ciphertext_start = nonce_start + NONCE_BYTES;
    let nonce = Nonce::try_from(&envelope[nonce_start..ciphertext_start])
        .map_err(|_| CredentialRecoveryError::InvalidPayload("nonce length is invalid"))?;
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &envelope[ciphertext_start..],
                aad: FILE_MAGIC,
            },
        )
        .map_err(|_| CredentialRecoveryError::AuthenticationFailed)?;
    decode_payload(Zeroizing::new(plaintext))
}

fn decode_payload(bytes: Zeroizing<Vec<u8>>) -> Result<DecodedPayload, CredentialRecoveryError> {
    if bytes.len() < 19 || bytes.len() > MAX_PLAINTEXT_BYTES {
        return Err(CredentialRecoveryError::InvalidPayload(
            "plaintext length is invalid",
        ));
    }
    let installation_id = Uuid::from_slice(&bytes[..16])
        .map_err(|_| CredentialRecoveryError::InvalidPayload("installation ID is invalid"))?;
    if installation_id.is_nil() {
        return Err(CredentialRecoveryError::InvalidPayload(
            "installation ID is nil",
        ));
    }
    let flags = bytes[16];
    if flags == 0 || flags & !KNOWN_FLAGS != 0 {
        return Err(CredentialRecoveryError::InvalidPayload("flags are invalid"));
    }
    let key_length = usize::from(u16::from_be_bytes([bytes[17], bytes[18]]));
    let expected = 19_usize
        .checked_add(key_length)
        .and_then(|length| length.checked_add(if flags & CURSOR_KEY_FLAG != 0 { 32 } else { 0 }))
        .and_then(|length| length.checked_add(if flags & FLEET_KEY_FLAG != 0 { 32 } else { 0 }))
        .ok_or(CredentialRecoveryError::InvalidPayload(
            "payload length overflowed",
        ))?;
    if expected != bytes.len()
        || key_length > MAX_SOURCE_DB_KEY_BYTES
        || (flags & SOURCE_DB_KEY_FLAG == 0) != (key_length == 0)
    {
        return Err(CredentialRecoveryError::InvalidPayload(
            "key catalogue is invalid",
        ));
    }
    let source_db_end = 19 + key_length;
    let cursor_end = source_db_end + if flags & CURSOR_KEY_FLAG != 0 { 32 } else { 0 };
    let source_db_key = (key_length > 0).then(|| Zeroizing::new(bytes[19..source_db_end].to_vec()));
    let cursor_key = (flags & CURSOR_KEY_FLAG != 0)
        .then(|| Zeroizing::new(bytes[source_db_end..cursor_end].to_vec()));
    let fleet_key =
        (flags & FLEET_KEY_FLAG != 0).then(|| Zeroizing::new(bytes[cursor_end..].to_vec()));
    Ok(DecodedPayload {
        installation_id,
        source_db_key,
        cursor_key,
        fleet_key,
    })
}

fn publish_private_file(destination: &Path, bytes: &[u8]) -> Result<(), CredentialRecoveryError> {
    match fs::symlink_metadata(destination) {
        Ok(_) => return Err(CredentialRecoveryError::DestinationExists),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(io_error(
                "inspecting export destination",
                destination,
                source,
            ));
        }
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staging = parent.join(format!(
        ".teslatlas-credential-recovery-{}.staging",
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .open(&staging)
            .map_err(|source| io_error("creating encrypted credential export", &staging, source))?;
        file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
            .map_err(|source| {
                io_error("protecting encrypted credential export", &staging, source)
            })?;
        file.write_all(bytes)
            .map_err(|source| io_error("writing encrypted credential export", &staging, source))?;
        file.sync_all()
            .map_err(|source| io_error("syncing encrypted credential export", &staging, source))?;
        rename_no_replace(&staging, destination)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

fn read_private_file(source: &Path) -> Result<Zeroizing<Vec<u8>>, CredentialRecoveryError> {
    let descriptor = open(
        source,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::LOOP {
            CredentialRecoveryError::UnsafeSource
        } else {
            io_error("opening encrypted credential export", source, error.into())
        }
    })?;
    let metadata = fstat(&descriptor).map_err(|error| {
        io_error(
            "inspecting encrypted credential export",
            source,
            error.into(),
        )
    })?;
    if !FileType::from_raw_mode(metadata.st_mode).is_file()
        || (metadata.st_mode as u32 & 0o777) != PRIVATE_FILE_MODE
        || metadata.st_uid != getuid().as_raw()
        || metadata.st_size < 0
        || usize::try_from(metadata.st_size)
            .ok()
            .is_none_or(|size| size > MAX_EXPORT_BYTES)
    {
        return Err(CredentialRecoveryError::UnsafeSource);
    }
    let file = File::from(descriptor);
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        usize::try_from(metadata.st_size).unwrap_or_default(),
    ));
    file.take(u64::try_from(MAX_EXPORT_BYTES + 1).expect("export cap fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|source_error| {
            io_error("reading encrypted credential export", source, source_error)
        })?;
    if bytes.len() > MAX_EXPORT_BYTES {
        return Err(CredentialRecoveryError::UnsafeSource);
    }
    Ok(bytes)
}

fn publish_secrets_directory(
    data_dir: &Path,
    payload: &DecodedPayload,
) -> Result<(), CredentialRecoveryError> {
    let destination = data_dir.join("secrets");
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(CredentialRecoveryError::SecretsAlreadyExist),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(io_error(
                "inspecting secrets destination",
                &destination,
                source,
            ));
        }
    }
    let staging = data_dir.join(format!(
        ".teslatlas-credential-restore-{}.staging",
        Uuid::new_v4()
    ));
    let result = (|| {
        fs::DirBuilder::new()
            .mode(PRIVATE_DIRECTORY_MODE)
            .create(&staging)
            .map_err(|source| io_error("creating credential restore staging", &staging, source))?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).map_err(
            |source| io_error("protecting credential restore staging", &staging, source),
        )?;
        if let Some(key) = payload.source_db_key.as_ref() {
            ({ let unavailable_value: Result<(), CredentialRecoveryError> = write_private_file(&staging.join("pre-upgrade-owner-api.key"), key); unavailable_value })?;
        }
        if let Some(key) = payload.cursor_key.as_ref() {
            write_private_file(&staging.join("hub-cursor.key"), key)?;
        }
        if let Some(key) = payload.fleet_key.as_ref() {
            ({ let unavailable_value: Result<(), CredentialRecoveryError> = write_private_file(&staging.join("pre-upgrade-fleet-api.key"), key); unavailable_value })?;
        }
        sync_directory(&staging)?;
        rename_no_replace(&staging, &destination)?;
        sync_directory(data_dir)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), CredentialRecoveryError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|source| io_error("creating restored credential key", path, source))?;
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .map_err(|source| io_error("protecting restored credential key", path, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error("writing restored credential key", path, source))?;
    file.sync_all()
        .map_err(|source| io_error("syncing restored credential key", path, source))
}

fn rename_no_replace(source: &Path, destination: &Path) -> Result<(), CredentialRecoveryError> {
    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(|error| {
        io_error(
            "publishing credential recovery without replacement",
            destination,
            error.into(),
        )
    })
}

fn sync_directory(path: &Path) -> Result<(), CredentialRecoveryError> {
    File::open(path)
        .map_err(|source| io_error("opening recovery directory for sync", path, source))?
        .sync_all()
        .map_err(|source| io_error("syncing recovery directory", path, source))
}

fn io_error(
    operation: &'static str,
    path: impl AsRef<Path>,
    source: std::io::Error,
) -> CredentialRecoveryError {
    CredentialRecoveryError::Io {
        operation,
        path: path.as_ref().to_path_buf(),
        source,
    }
}

/// Restore under the admitted vault's connection (R52, E58, M2P-58): Phase B
/// has already run. Matching is per slot; publication never replaces a target.
pub(crate) fn restore_credentials_with_connection(
    store: &crate::db::HubStore,
    connection: &mut crate::db::CredentialConnection,
    data_dir: &Path,
    source: &Path,
    recovery_key: &[u8],
) -> Result<CredentialRecoveryReport, CredentialRecoveryError> {
    use crate::auth::{
        credential_envelope::CredentialSlot,
        credential_migration::{LAST_PRE_C10_VERSION, boundary_continues},
    };
    use crate::db::CredentialSlotFormatState;

    /// E58 existing-directory branch: preflight every payload target (identical
    /// bytes stay untouched, different bytes refuse before any write), publish the
    /// missing ones through synced 0600 `.hub-restore-<UUID v4>.tmp` files and
    /// no-replace renames, then the secrets-directory barrier and a readback.
    fn publish_into_existing_secrets(
        secrets_path: &Path,
        payload: &DecodedPayload,
    ) -> Result<(), CredentialRecoveryError> {
        use crate::auth::credential_migration::{boundary_continues, note_event};
        use crate::platform::private_directory::{EntryState, open_validated};
        use std::os::fd::OwnedFd;

        let failed = |operation: &'static str, path: &Path| {
            io_error(
                operation,
                path,
                std::io::Error::other("directory operation failed"),
            )
        };
        let secrets = open_validated(secrets_path, false)
            .map_err(|_| failed("opening the secrets directory", secrets_path))?;
        let members: [(&str, Option<&[u8]>); 3] = [
            (
                "hub-cursor.key",
                payload.cursor_key.as_deref().map(Vec::as_slice),
            ),
            (
                "pre-upgrade-owner-api.key",
                payload.source_db_key.as_deref().map(Vec::as_slice),
            ),
            (
                "pre-upgrade-fleet-api.key",
                payload.fleet_key.as_deref().map(Vec::as_slice),
            ),
        ];
        let read_member = |name: &str,
                           descriptor: OwnedFd|
         -> Result<Zeroizing<Vec<u8>>, CredentialRecoveryError> {
            let mut bytes = Zeroizing::new(Vec::new());
            File::from(descriptor)
                .take(u64::try_from(MAX_SOURCE_DB_KEY_BYTES + 1).expect("key cap fits u64"))
                .read_to_end(&mut bytes)
                .map_err(|source| {
                    io_error(
                        "reading an existing restore target",
                        secrets_path.join(name),
                        source,
                    )
                })?;
            Ok(bytes)
        };
        let mut missing: Vec<(&str, &[u8])> = Vec::new();
        for (name, bytes) in members {
            let Some(bytes) = bytes else { continue };
            match secrets
                .inspect_entry(name)
                .map_err(|_| failed("inspecting a restore target", &secrets_path.join(name)))?
            {
                EntryState::Absent => missing.push((name, bytes)),
                EntryState::Regular(descriptor) => {
                    if read_member(name, descriptor)?.as_slice() != bytes {
                        return Err(CredentialRecoveryError::SecretsAlreadyExist);
                    }
                }
                EntryState::Unsafe(_) => return Err(CredentialRecoveryError::SecretsAlreadyExist),
            }
        }
        for (name, bytes) in missing {
            let staged = format!(".hub-restore-{}.tmp", Uuid::new_v4());
            let staged_path = secrets_path.join(&staged);
            let mut file = secrets.create_exclusive(&staged).map_err(|source| {
                io_error("creating a staged restore member", &staged_path, source)
            })?;
            file.write_all(bytes).map_err(|source| {
                io_error("writing a staged restore member", &staged_path, source)
            })?;
            file.sync_all().map_err(|source| {
                io_error("syncing a staged restore member", &staged_path, source)
            })?;
            drop(file);
            note_event(&format!("restore:staged:{name}"));
            if !boundary_continues(&format!("restore:staged_synced:{name}")) {
                return Err(failed(
                    "stopping after a staged member was synced",
                    &staged_path,
                ));
            }
            renameat_with(
                &secrets,
                staged.as_str(),
                &secrets,
                name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|error| {
                io_error(
                    "publishing a restore member without replacement",
                    secrets_path.join(name),
                    error.into(),
                )
            })?;
        }
        secrets
            .durable_sync()
            .map_err(|_| failed("syncing the secrets directory", secrets_path))?;
        note_event("restore:secrets_barrier");
        for (name, bytes) in members {
            let Some(bytes) = bytes else { continue };
            let matches = match secrets
                .inspect_entry(name)
                .map_err(|_| failed("reading back a restore member", &secrets_path.join(name)))?
            {
                EntryState::Regular(descriptor) => {
                    read_member(name, descriptor)?.as_slice() == bytes
                }
                _ => false,
            };
            if !matches {
                return Err(CredentialRecoveryError::CatalogueMismatch);
            }
        }
        Ok(())
    }

    /// Per-slot matching: where a slot's old row exists, usable member material
    /// must authenticate both of its stored values (the Fleet slot with its
    /// dedicated member or the cursor-derived key); where it does not, the
    /// member is accepted under its restore name.
    fn match_pending_slots(
        connection: &mut crate::db::CredentialConnection,
        state: &crate::db::CredentialFormatState,
        owner_key: Option<&[u8]>,
        fleet_key: Option<&[u8]>,
        cursor_key: Option<&[u8]>,
    ) -> Result<(), CredentialRecoveryError> {
        if state.owner_api_state == CredentialSlotFormatState::Pending {
            let row = connection
                .read_pre_upgrade_credential_row(CredentialSlot::OwnerApi)?
                .ok_or(StoreError::CredentialPairInvalid)?;
            let key = owner_key.ok_or(CredentialRecoveryError::CatalogueMismatch)?;
            decrypt_legacy_owner_tokens(key, &row.encrypted_access, &row.encrypted_refresh)
                .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        }
        if state.fleet_api_state == CredentialSlotFormatState::Pending {
            let row = connection
                .read_pre_upgrade_credential_row(CredentialSlot::FleetApi)?
                .ok_or(StoreError::CredentialPairInvalid)?;
            let authenticated = match (fleet_key, cursor_key) {
                (Some(key), _) => {
                    decrypt_legacy_owner_tokens(key, &row.encrypted_access, &row.encrypted_refresh)
                }
                (None, Some(cursor)) => {
                    let derived = fleet_encryption_key(cursor)?;
                    decrypt_legacy_owner_tokens(
                        derived.as_ref(),
                        &row.encrypted_access,
                        &row.encrypted_refresh,
                    )
                }
                (None, None) => return Err(CredentialRecoveryError::CatalogueMismatch),
            };
            authenticated.map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
        }
        Ok(())
    }

    /// One published member, read back through a no-follow open.
    fn read_back_member(path: &Path) -> Result<Zeroizing<Vec<u8>>, CredentialRecoveryError> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(i32::try_from(OFlags::NOFOLLOW.bits()).unwrap_or(0))
            .open(path)
            .map_err(|source| io_error("reading back a restored member", path, source))?;
        let mut bytes = Zeroizing::new(Vec::new());
        (&mut file)
            .take(u64::try_from(MAX_SOURCE_DB_KEY_BYTES + 1).expect("key cap fits u64"))
            .read_to_end(&mut bytes)
            .map_err(|source| io_error("reading back a restored member", path, source))?;
        Ok(bytes)
    }

    let cipher = recovery_cipher(recovery_key)?;
    let envelope = read_private_file(source)?;
    let payload = decode_envelope(&cipher, &envelope)?;
    if payload.installation_id != store.installation_id()? {
        return Err(CredentialRecoveryError::InstallationMismatch);
    }
    validate_cursor_key_catalogue(store, payload.cursor_key.as_deref().map(Vec::as_slice))?;
    // R50: above the recorded floor raise, pre-upgrade members are refused
    // before anything is written.
    if (payload.source_db_key.is_some() || payload.fleet_key.is_some())
        && restore_floor() > LAST_PRE_C10_VERSION
    {
        return Err(CredentialRecoveryError::InvalidPayload(
            "pre-upgrade key members are refused above the store floor",
        ));
    }
    if payload.source_db_key.is_none()
        && payload.cursor_key.is_none()
        && payload.fleet_key.is_none()
    {
        return Err(CredentialRecoveryError::NoKeyMaterial);
    }
    // Per-slot matching: where a slot's old row exists, usable member material
    // must authenticate both of its stored values; where it does not, the
    // member is accepted under its restore name.
    let state = connection.credential_format_state()?;
    match_pending_slots(
        connection,
        &state,
        payload.source_db_key.as_deref().map(Vec::as_slice),
        payload.fleet_key.as_deref().map(Vec::as_slice),
        payload.cursor_key.as_deref().map(Vec::as_slice),
    )?;

    let secrets_directory = data_dir.join("secrets");
    match fs::symlink_metadata(&secrets_directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // E58 absent-directory branch: the retained mode-0700 staging
            // directory, no-replace publication and data-directory barrier.
            publish_secrets_directory(data_dir, &payload)?;
            if !boundary_continues("restore:absent_published") {
                return Err(io_error(
                    "stopping after the absent-directory publication",
                    &secrets_directory,
                    std::io::Error::other("stopped"),
                ));
            }
            // Post-publication readback of the two pre-upgrade members: the
            // restored bytes must equal the payload and, for a pending slot,
            // authenticate its stored row again.
            let restored_owner = payload
                .source_db_key
                .as_ref()
                .map(|member| {
                    read_back_member(&secrets_directory.join("pre-upgrade-owner-api.key"))
                        .map(|restored| (restored, member))
                })
                .transpose()?;
            let restored_fleet = payload
                .fleet_key
                .as_ref()
                .map(|member| {
                    read_back_member(&secrets_directory.join("pre-upgrade-fleet-api.key"))
                        .map(|restored| (restored, member))
                })
                .transpose()?;
            for (restored, member) in restored_owner.iter().chain(restored_fleet.iter()) {
                if restored.as_slice() != member.as_slice() {
                    return Err(CredentialRecoveryError::CatalogueMismatch);
                }
            }
            match_pending_slots(
                connection,
                &state,
                restored_owner
                    .as_ref()
                    .map(|(restored, _)| restored.as_slice()),
                restored_fleet
                    .as_ref()
                    .map(|(restored, _)| restored.as_slice()),
                payload.cursor_key.as_deref().map(Vec::as_slice),
            )?;
        }
        Ok(_) => publish_into_existing_secrets(&secrets_directory, &payload)?,
        Err(source_error) => {
            return Err(io_error(
                "inspecting secrets destination",
                &secrets_directory,
                source_error,
            ));
        }
    }
    if payload.cursor_key.is_some() {
        let restored_cursor = load_existing_cursor_key_bytes(data_dir)?;
        if restored_cursor.as_deref() != payload.cursor_key.as_deref() {
            return Err(CredentialRecoveryError::CatalogueMismatch);
        }
    }
    Ok(report(
        "restored",
        source,
        payload.installation_id,
        payload.source_db_key.is_some(),
        payload.cursor_key.is_some(),
        payload.fleet_key.is_some(),
    ))
}

#[cfg(test)]
#[path = "credential_recovery/tests.rs"]
pub(in crate::auth) mod tests;

#[cfg(test)]
mod n1_src_auth_credential_recovery_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for CredentialRecoveryError {
        fn error_example() -> Self {
            CredentialRecoveryError::InvalidRecoveryKey
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_credential_recovery_error(error: CredentialRecoveryError) {
            match &error {
                CredentialRecoveryError::Vault(..) => (),
                CredentialRecoveryError::InvalidRecoveryKey => (),
                CredentialRecoveryError::NoKeyMaterial => (),
                CredentialRecoveryError::DestinationExists => (),
                CredentialRecoveryError::UnsafeSource => (),
                CredentialRecoveryError::InvalidPayload(..) => (),
                CredentialRecoveryError::AuthenticationFailed => (),
                CredentialRecoveryError::InstallationMismatch => (),
                CredentialRecoveryError::CatalogueMismatch => (),
                CredentialRecoveryError::EntropyUnavailable => (),
                CredentialRecoveryError::SecretsAlreadyExist => (),
                CredentialRecoveryError::Io { .. } => (),
                CredentialRecoveryError::Store(..) => (),
                CredentialRecoveryError::Credential(..) => (),
                CredentialRecoveryError::Token(..) => (),
            }
            assert_name_free(&error);
        }
        check_credential_recovery_error(CredentialRecoveryError::InvalidRecoveryKey);
        check_credential_recovery_error(CredentialRecoveryError::Vault(super::super::credential_sign_in::CredentialCommandError::new("durability_unavailable")));
        check_credential_recovery_error(CredentialRecoveryError::NoKeyMaterial);
        check_credential_recovery_error(CredentialRecoveryError::DestinationExists);
        check_credential_recovery_error(CredentialRecoveryError::UnsafeSource);
        check_credential_recovery_error(CredentialRecoveryError::InvalidPayload(example()));
        check_credential_recovery_error(CredentialRecoveryError::AuthenticationFailed);
        check_credential_recovery_error(CredentialRecoveryError::InstallationMismatch);
        check_credential_recovery_error(CredentialRecoveryError::CatalogueMismatch);
        check_credential_recovery_error(CredentialRecoveryError::EntropyUnavailable);
        check_credential_recovery_error(CredentialRecoveryError::SecretsAlreadyExist);
        check_credential_recovery_error(CredentialRecoveryError::Io {
            operation: example(),
            path: example(),
            source: example(),
        });
        check_credential_recovery_error(CredentialRecoveryError::Store(example()));
        check_credential_recovery_error(CredentialRecoveryError::Credential(example()));
        check_credential_recovery_error(CredentialRecoveryError::Token(example()));
    }
}
