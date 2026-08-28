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
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    db::{HubStore, StoreError},
    fleet_credentials::load_existing_fleet_key_bytes,
    protocol::{
        CursorKey, LineageDelta, LineageManifestV2, OpaqueCursor, PROTOCOL_V1, ProtocolLimits,
        SyncManifest, canonical_delta_chain_digest,
    },
    teslamate_credentials::{
        TeslaMateCredentialError, load_existing_cursor_key_bytes, load_key, load_key_for_tokens,
    },
    teslamate_token::{TeslaMateTokenError, decrypt_legacy_owner_tokens},
};

pub const RECOVERY_ENCRYPTION_KEY_BYTES: usize = 32;
const FILE_MAGIC: &[u8] = b"TESLATLAS-HUB-CREDENTIAL-RECOVERY-V1\0";
const NONCE_BYTES: usize = 12;
const AUTH_TAG_BYTES: usize = 16;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const MAX_TESLAMATE_KEY_BYTES: usize = 16 * 1024;
const MAX_PLAINTEXT_BYTES: usize = 16 + 1 + 2 + MAX_TESLAMATE_KEY_BYTES + 32 + 32;
const MAX_EXPORT_BYTES: usize =
    FILE_MAGIC.len() + NONCE_BYTES + MAX_PLAINTEXT_BYTES + AUTH_TAG_BYTES;
const TESLAMATE_KEY_FLAG: u8 = 1 << 0;
const CURSOR_KEY_FLAG: u8 = 1 << 1;
const FLEET_KEY_FLAG: u8 = 1 << 2;
const KNOWN_FLAGS: u8 = TESLAMATE_KEY_FLAG | CURSOR_KEY_FLAG | FLEET_KEY_FLAG;

#[derive(Debug, Error)]
pub enum CredentialRecoveryError {
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
    Credential(#[from] TeslaMateCredentialError),
    #[error(transparent)]
    Token(#[from] TeslaMateTokenError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRecoveryReport {
    pub status: &'static str,
    pub path: PathBuf,
    pub installation_id: Uuid,
    pub secret_bearing: bool,
    pub encryption: &'static str,
    pub teslamate_key_included: bool,
    pub cursor_key_included: bool,
    pub fleet_key_included: bool,
}

struct DecodedPayload {
    installation_id: Uuid,
    teslamate_key: Option<Zeroizing<Vec<u8>>>,
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
    let stored_tokens = store.load_teslamate_legacy_tokens()?;
    let teslamate_key = match stored_tokens.as_ref() {
        Some(tokens) => {
            let key = load_key(data_dir)?;
            drop(decrypt_legacy_owner_tokens(
                key.as_bytes(),
                tokens.access(),
                tokens.refresh(),
            )?);
            Some(key)
        }
        None => None,
    };
    let cursor_key = load_existing_cursor_key_bytes(data_dir)?;
    validate_cursor_key_catalogue(store, cursor_key.as_deref().map(Vec::as_slice))?;
    let fleet_tokens = store.load_fleet_tokens()?;
    let fleet_key = if let Some(tokens) = fleet_tokens.as_ref() {
        match load_existing_fleet_key_bytes(data_dir)
            .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?
        {
            Some(key) => {
                drop(decrypt_legacy_owner_tokens(
                    &key,
                    tokens.access(),
                    tokens.refresh(),
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
                    tokens.access(),
                    tokens.refresh(),
                )?);
                None
            }
        }
    } else {
        None
    };
    if teslamate_key.is_none() && cursor_key.is_none() && fleet_key.is_none() {
        return Err(CredentialRecoveryError::NoKeyMaterial);
    }

    let mut plaintext = encode_payload(
        installation_id,
        teslamate_key.as_ref().map(|key| key.as_bytes()),
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
        teslamate_key.is_some(),
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
    let cipher = recovery_cipher(recovery_key)?;
    let envelope = read_private_file(source)?;
    let payload = decode_envelope(&cipher, &envelope)?;
    if payload.installation_id != store.installation_id()? {
        return Err(CredentialRecoveryError::InstallationMismatch);
    }
    validate_cursor_key_catalogue(store, payload.cursor_key.as_deref().map(Vec::as_slice))?;

    let stored_tokens = store.load_teslamate_legacy_tokens()?;
    let fleet_tokens = store.load_fleet_tokens()?;
    if stored_tokens.is_some() != payload.teslamate_key.is_some() {
        return Err(CredentialRecoveryError::CatalogueMismatch);
    }
    if let (Some(tokens), Some(key)) = (stored_tokens.as_ref(), payload.teslamate_key.as_ref()) {
        drop(decrypt_legacy_owner_tokens(
            key,
            tokens.access(),
            tokens.refresh(),
        )?);
    }
    if fleet_tokens.is_none() && payload.fleet_key.is_some() {
        return Err(CredentialRecoveryError::CatalogueMismatch);
    }
    if let Some(tokens) = fleet_tokens.as_ref() {
        match (payload.fleet_key.as_ref(), payload.cursor_key.as_ref()) {
            (Some(key), _) => drop(decrypt_legacy_owner_tokens(
                key,
                tokens.access(),
                tokens.refresh(),
            )?),
            (None, Some(cursor)) => {
                let legacy = fleet_encryption_key(cursor)?;
                drop(decrypt_legacy_owner_tokens(
                    legacy.as_ref(),
                    tokens.access(),
                    tokens.refresh(),
                )?);
            }
            (None, None) => return Err(CredentialRecoveryError::CatalogueMismatch),
        }
    }
    if payload.teslamate_key.is_none()
        && payload.cursor_key.is_none()
        && payload.fleet_key.is_none()
    {
        return Err(CredentialRecoveryError::NoKeyMaterial);
    }

    publish_secrets_directory(data_dir, &payload)?;
    if let Some(tokens) = stored_tokens.as_ref() {
        drop(load_key_for_tokens(data_dir, tokens)?);
    }
    let restored_cursor = load_existing_cursor_key_bytes(data_dir)?;
    if restored_cursor.as_deref() != payload.cursor_key.as_deref() {
        return Err(CredentialRecoveryError::CatalogueMismatch);
    }
    let restored_fleet_key = load_existing_fleet_key_bytes(data_dir)
        .map_err(|_| CredentialRecoveryError::CatalogueMismatch)?;
    if restored_fleet_key.as_deref() != payload.fleet_key.as_deref() {
        return Err(CredentialRecoveryError::CatalogueMismatch);
    }
    if let Some(tokens) = fleet_tokens.as_ref() {
        if let Some(key) = restored_fleet_key.as_ref() {
            drop(decrypt_legacy_owner_tokens(
                key,
                tokens.access(),
                tokens.refresh(),
            )?);
        } else {
            let cursor = restored_cursor
                .as_ref()
                .ok_or(CredentialRecoveryError::CatalogueMismatch)?;
            let legacy = fleet_encryption_key(cursor)?;
            drop(decrypt_legacy_owner_tokens(
                legacy.as_ref(),
                tokens.access(),
                tokens.refresh(),
            )?);
        }
    }

    Ok(report(
        "restored",
        source,
        payload.installation_id,
        payload.teslamate_key.is_some(),
        payload.cursor_key.is_some(),
        payload.fleet_key.is_some(),
    ))
}

fn report(
    status: &'static str,
    path: &Path,
    installation_id: Uuid,
    teslamate_key_included: bool,
    cursor_key_included: bool,
    fleet_key_included: bool,
) -> CredentialRecoveryReport {
    CredentialRecoveryReport {
        status,
        path: path.to_path_buf(),
        installation_id,
        secret_bearing: true,
        encryption: "AES-256-GCM",
        teslamate_key_included,
        cursor_key_included,
        fleet_key_included,
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
    teslamate_key: Option<&[u8]>,
    cursor_key: Option<&[u8]>,
    fleet_key: Option<&[u8]>,
) -> Result<Zeroizing<Vec<u8>>, CredentialRecoveryError> {
    let mut flags = 0_u8;
    if teslamate_key.is_some() {
        flags |= TESLAMATE_KEY_FLAG;
    }
    if cursor_key.is_some() {
        flags |= CURSOR_KEY_FLAG;
    }
    if fleet_key.is_some() {
        flags |= FLEET_KEY_FLAG;
    }
    let teslamate_key = teslamate_key.unwrap_or_default();
    if teslamate_key.len() > MAX_TESLAMATE_KEY_BYTES {
        return Err(CredentialRecoveryError::InvalidPayload(
            "TeslaMate key is too large",
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
    let key_length = u16::try_from(teslamate_key.len())
        .map_err(|_| CredentialRecoveryError::InvalidPayload("TeslaMate key length is invalid"))?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_PLAINTEXT_BYTES));
    bytes.extend_from_slice(installation_id.as_bytes());
    bytes.push(flags);
    bytes.extend_from_slice(&key_length.to_be_bytes());
    bytes.extend_from_slice(teslamate_key);
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
        || key_length > MAX_TESLAMATE_KEY_BYTES
        || (flags & TESLAMATE_KEY_FLAG == 0) != (key_length == 0)
    {
        return Err(CredentialRecoveryError::InvalidPayload(
            "key catalogue is invalid",
        ));
    }
    let teslamate_end = 19 + key_length;
    let cursor_end = teslamate_end + if flags & CURSOR_KEY_FLAG != 0 { 32 } else { 0 };
    let teslamate_key = (key_length > 0).then(|| Zeroizing::new(bytes[19..teslamate_end].to_vec()));
    let cursor_key = (flags & CURSOR_KEY_FLAG != 0)
        .then(|| Zeroizing::new(bytes[teslamate_end..cursor_end].to_vec()));
    let fleet_key =
        (flags & FLEET_KEY_FLAG != 0).then(|| Zeroizing::new(bytes[cursor_end..].to_vec()));
    Ok(DecodedPayload {
        installation_id,
        teslamate_key,
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
        if let Some(key) = payload.teslamate_key.as_ref() {
            write_private_file(&staging.join("teslamate-encryption.key"), key)?;
        }
        if let Some(key) = payload.cursor_key.as_ref() {
            write_private_file(&staging.join("hub-cursor.key"), key)?;
        }
        if let Some(key) = payload.fleet_key.as_ref() {
            write_private_file(&staging.join("fleet-credentials.key"), key)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        credentials::OwnerTokens,
        data_recovery::{create_data_backup, restore_data_backup},
        db::TeslaMateLegacyTokenStore,
        protocol::{CursorClaims, PROTOCOL_V1, SyncManifest, TRANSPORT_SCHEMA_V1, TransferMode},
        teslamate_credentials::{
            load_or_create_cursor_key, random_encryption_key, replace_key_and_tokens,
        },
        teslamate_token::encrypt_legacy_owner_tokens,
    };

    #[test]
    fn encrypted_export_round_trips_after_data_only_restore() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let source_data = temporary.path().join("source-data");
        let source = HubStore::initialize(&source_data).expect("source store");
        let tokens = OwnerTokens::from_secret_parts(
            "credential-recovery-access".to_owned(),
            "credential-recovery-refresh".to_owned(),
        )
        .expect("tokens");
        let teslamate_key = random_encryption_key().expect("random TeslaMate key");
        let (access, refresh) =
            encrypt_legacy_owner_tokens(&teslamate_key, &tokens).expect("ciphertext");
        let stored = TeslaMateLegacyTokenStore::imported(access, refresh).expect("stored tokens");
        replace_key_and_tokens(&source_data, &source, &teslamate_key, &stored)
            .expect("source credentials");
        load_or_create_cursor_key(&source_data).expect("source cursor");
        let source_cursor = load_existing_cursor_key_bytes(&source_data)
            .expect("source cursor read")
            .expect("source cursor bytes");

        let data_backup = temporary.path().join("data-backup");
        create_data_backup(&source, &data_backup).expect("data backup");
        let restored_data = temporary.path().join("restored-data");
        restore_data_backup(&data_backup, &restored_data).expect("data restore");
        let restored = HubStore::initialize(&restored_data).expect("restored store");

        let recovery_key = [7_u8; RECOVERY_ENCRYPTION_KEY_BYTES];
        let export = temporary.path().join("credentials.tthcr");
        let report = export_credentials(&source, &source_data, &export, &recovery_key)
            .expect("credential export");
        assert!(report.secret_bearing);
        let bytes = fs::read(&export).expect("encrypted bytes");
        assert!(
            !bytes
                .windows(teslamate_key.len())
                .any(|part| part == teslamate_key.as_slice())
        );
        assert!(
            !bytes
                .windows(32)
                .any(|part| part == source_cursor.as_slice())
        );

        restore_credentials(&restored, &restored_data, &export, &recovery_key)
            .expect("credential restore");
        let restored_tokens = restored
            .load_teslamate_legacy_tokens()
            .expect("token query")
            .expect("stored token row");
        let restored_key =
            load_key_for_tokens(&restored_data, &restored_tokens).expect("restored TeslaMate key");
        assert_eq!(restored_key.as_bytes(), teslamate_key.as_slice());
        assert_eq!(
            load_existing_cursor_key_bytes(&restored_data)
                .expect("cursor read")
                .expect("cursor key")
                .as_slice(),
            source_cursor.as_slice()
        );
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
        fs::set_permissions(&tampered_path, fs::Permissions::from_mode(0o600))
            .expect("tampered mode");
        assert!(matches!(
            restore_credentials(&store, &data, &tampered_path, &recovery_key),
            Err(CredentialRecoveryError::AuthenticationFailed)
        ));
        assert!(matches!(
            restore_credentials(&store, &data, &export, &recovery_key),
            Err(CredentialRecoveryError::SecretsAlreadyExist)
        ));
        assert!(matches!(
            export_credentials(&store, &data, &export, &recovery_key),
            Err(CredentialRecoveryError::DestinationExists)
        ));
    }

    #[test]
    fn fleet_only_credentials_round_trip_with_the_dedicated_key() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let source_data = temporary.path().join("fleet-source");
        let source = HubStore::initialize(&source_data).expect("source store");
        load_or_create_cursor_key(&source_data).expect("source cursor");
        let credentials = crate::fleet_credentials::FleetSetupCredentials::new(
            "fleet-recovery-access".to_owned(),
            "fleet-recovery-refresh".to_owned(),
            "fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("Fleet credentials");
        crate::fleet_credentials::persist_fleet_setup_credentials(
            &source,
            &source_data,
            &credentials,
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000),
        )
        .expect("Fleet row persists");
        let source_fleet_key = load_existing_fleet_key_bytes(&source_data)
            .expect("source Fleet key read")
            .expect("source Fleet key");

        let data_backup = temporary.path().join("fleet-data-backup");
        create_data_backup(&source, &data_backup).expect("Fleet data backup");
        let restored_data = temporary.path().join("fleet-restored");
        restore_data_backup(&data_backup, &restored_data).expect("Fleet data restore");
        let restored = HubStore::initialize(&restored_data).expect("restored store");
        assert!(
            load_existing_fleet_key_bytes(&restored_data)
                .expect("data-only restore Fleet key check")
                .is_none()
        );

        let recovery_key = [11_u8; RECOVERY_ENCRYPTION_KEY_BYTES];
        let export = temporary.path().join("fleet-credentials.tthcr");
        let report = export_credentials(&source, &source_data, &export, &recovery_key)
            .expect("Fleet credential export");
        assert!(report.fleet_key_included);
        restore_credentials(&restored, &restored_data, &export, &recovery_key)
            .expect("Fleet credential restore");

        let restored_fleet_key = load_existing_fleet_key_bytes(&restored_data)
            .expect("restored Fleet key read")
            .expect("restored Fleet key");
        assert_eq!(restored_fleet_key.as_slice(), source_fleet_key.as_slice());
        let restored_fleet = restored
            .load_fleet_tokens()
            .expect("Fleet row loads")
            .expect("Fleet row remains");
        let decrypted = decrypt_legacy_owner_tokens(
            &restored_fleet_key,
            restored_fleet.access(),
            restored_fleet.refresh(),
        )
        .expect("Fleet row decrypts");
        assert_eq!(decrypted.access_token(), "fleet-recovery-access");
        assert_eq!(decrypted.refresh_token(), "fleet-recovery-refresh");
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
            store
                .load_teslamate_legacy_tokens()
                .expect("token query")
                .is_none()
        );
        assert!(
            store
                .load_fleet_tokens()
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

        restore_credentials(&store, &data, &export, &recovery_key)
            .expect("empty catalogue restore");
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
}
