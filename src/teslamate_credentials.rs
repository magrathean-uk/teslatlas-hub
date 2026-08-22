//! Minimal on-disk ownership for the exact TeslaMate `ENCRYPTION_KEY` bytes.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use rustix::{
    fs::{FileType, Mode, OFlags, fcntl_getfl, fcntl_setfl, fstat, open},
    io::Errno,
    process::getuid,
};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

const SECRETS_DIRECTORY_MODE: u32 = 0o700;
const KEY_FILE_MODE: u32 = 0o600;
const MAX_KEY_BYTES: usize = 16 * 1024;
const CURSOR_KEY_BYTES: usize = 32;
const PREVIOUS_KEY_FILE_NAME: &str = ".teslamate-encryption.previous.key";

/// Generate a new local key for a user-supplied legacy token pair.
pub fn random_encryption_key() -> Zeroizing<Vec<u8>> {
    let mut key = Zeroizing::new(vec![0_u8; 32]);
    getrandom::fill(key.as_mut_slice()).expect("system entropy");
    key
}

/// Exact, non-text-normalized TeslaMate encryption-key bytes.
pub struct TeslaMateEncryptionKey(Zeroizing<Vec<u8>>);

impl TeslaMateEncryptionKey {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl std::fmt::Debug for TeslaMateEncryptionKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TeslaMateEncryptionKey([redacted])")
    }
}

pub fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets/teslamate-encryption.key")
}

fn previous_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets").join(PREVIOUS_KEY_FILE_NAME)
}

pub fn cursor_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets/hub-cursor.key")
}

/// Load the durable Hub cursor-signing key, creating it exactly once when the
/// Hub data directory is first initialized.
pub fn load_or_create_cursor_key(
    data_dir: &Path,
) -> Result<crate::protocol::CursorKey, TeslaMateCredentialError> {
    let secrets_dir = data_dir.join("secrets");
    ensure_secrets_directory(&secrets_dir)?;
    let destination = cursor_key_path(data_dir);
    match fs::symlink_metadata(&destination) {
        Ok(_) => load_cursor_key_file(&destination),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_cursor_key_once(&secrets_dir, &destination)
        }
        Err(error) => Err(TeslaMateCredentialError::InspectCursorKey(error)),
    }
}

/// Test helper for constructing an already-persisted key fixture. Production
/// replacements must use `replace_key_and_tokens`.
#[cfg(test)]
pub(crate) fn replace_key(data_dir: &Path, key: &[u8]) -> Result<(), TeslaMateCredentialError> {
    validate_key(key)?;
    let secrets_dir = data_dir.join("secrets");
    ensure_secrets_directory(&secrets_dir)?;
    let destination = key_path(data_dir);
    validate_existing_key_file(&destination)?;

    let temporary = secrets_dir.join(format!(".teslamate-encryption-{}.tmp", Uuid::new_v4()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(KEY_FILE_MODE)
            .open(&temporary)
            .map_err(TeslaMateCredentialError::CreateTemporaryKey)?;
        file.set_permissions(fs::Permissions::from_mode(KEY_FILE_MODE))
            .map_err(TeslaMateCredentialError::ProtectTemporaryKey)?;
        file.write_all(key)
            .map_err(TeslaMateCredentialError::WriteTemporaryKey)?;
        file.sync_all()
            .map_err(TeslaMateCredentialError::SyncTemporaryKey)?;
        fs::rename(&temporary, &destination).map_err(TeslaMateCredentialError::ReplaceKey)?;
        sync_directory(&secrets_dir)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

/// Replace the key file and its ciphertext pair as one recoverable operation.
///
/// SQLite and the filesystem cannot share one transaction. The old key is
/// therefore retained until the SQLite replacement commits. On interruption,
/// startup tests both private key generations against the committed pair and
/// deterministically keeps the matching generation.
pub fn replace_key_and_tokens(
    data_dir: &Path,
    store: &crate::db::HubStore,
    key: &[u8],
    tokens: &crate::db::TeslaMateLegacyTokenStore,
) -> Result<(), TeslaMateCredentialImportError> {
    recover_pending_key_replacement(data_dir, store)?;
    let plaintext =
        crate::teslamate_token::decrypt_legacy_owner_tokens(key, tokens.access(), tokens.refresh())
            .map_err(TeslaMateCredentialImportError::TokenCipher)?;
    let generation = crate::teslamate_token::legacy_refresh_credential_generation(&plaintext);
    let tokens = tokens
        .with_credential_generation(generation)
        .map_err(TeslaMateCredentialImportError::TokenStore)?;
    let replacement = begin_key_replacement(data_dir, key)?;
    if let Err(store_error) = store.replace_teslamate_legacy_tokens(&tokens) {
        return match replacement.rollback() {
            Ok(()) => Err(TeslaMateCredentialImportError::TokenStore(store_error)),
            Err(rollback) => Err(TeslaMateCredentialImportError::Rollback {
                store: store_error.to_string(),
                rollback,
            }),
        };
    }
    replacement.commit()?;
    Ok(())
}

/// Remove TeslaMate collector authority after its process has been stopped.
///
/// The token row is deleted first. A leftover key by itself grants no access,
/// while deleting the key first could leave a usable ciphertext pair with an
/// already-loaded key generation after an interrupted operation.
pub fn remove_key_and_tokens(
    data_dir: &Path,
    store: &crate::db::HubStore,
) -> Result<(), TeslaMateCredentialRemovalError> {
    store
        .clear_teslamate_legacy_tokens()
        .map_err(TeslaMateCredentialRemovalError::TokenStore)?;

    let secrets_dir = data_dir.join("secrets");
    let current = key_path(data_dir);
    let previous = previous_key_path(data_dir);
    let current_exists = path_exists(&current)?;
    let previous_exists = path_exists(&previous)?;
    if current_exists {
        remove_checked_key_file(&current)?;
    }
    if previous_exists {
        remove_checked_key_file(&previous)?;
    }
    if current_exists || previous_exists {
        sync_directory(&secrets_dir)?;
    }
    Ok(())
}

/// Load the key generation that decrypts the committed ciphertext pair.
/// This also settles an interrupted cross-filesystem replacement.
pub fn load_key_for_tokens(
    data_dir: &Path,
    tokens: &crate::db::TeslaMateLegacyTokenStore,
) -> Result<TeslaMateEncryptionKey, TeslaMateCredentialError> {
    let previous = previous_key_path(data_dir);
    let previous_exists = path_exists(&previous)?;
    match load_key(data_dir) {
        Ok(current) if key_matches_tokens(&current, tokens) => {
            if previous_exists {
                remove_checked_key_file(&previous)?;
                sync_directory(&data_dir.join("secrets"))?;
            }
            Ok(current)
        }
        Ok(_) | Err(_) if previous_exists => {
            let prior = load_key_file(&previous)?;
            if !key_matches_tokens(&prior, tokens) {
                return Err(TeslaMateCredentialError::NoMatchingKeyGeneration);
            }
            restore_previous_key(data_dir)?;
            load_key(data_dir)
        }
        Ok(current) => Ok(current),
        Err(error) => Err(error),
    }
}

fn recover_pending_key_replacement(
    data_dir: &Path,
    store: &crate::db::HubStore,
) -> Result<(), TeslaMateCredentialImportError> {
    if !path_exists(&previous_key_path(data_dir))? {
        return Ok(());
    }
    let stored = store
        .load_teslamate_legacy_tokens()
        .map_err(TeslaMateCredentialImportError::TokenStore)?
        .ok_or(TeslaMateCredentialError::PendingKeyReplacementWithoutTokens)?;
    load_key_for_tokens(data_dir, &stored)?;
    Ok(())
}

fn key_matches_tokens(
    key: &TeslaMateEncryptionKey,
    tokens: &crate::db::TeslaMateLegacyTokenStore,
) -> bool {
    crate::teslamate_token::decrypt_legacy_owner_tokens(
        key.as_bytes(),
        tokens.access(),
        tokens.refresh(),
    )
    .is_ok()
}

struct KeyReplacement {
    data_dir: PathBuf,
    had_previous: bool,
}

impl KeyReplacement {
    fn commit(self) -> Result<(), TeslaMateCredentialError> {
        if self.had_previous {
            remove_checked_key_file(&previous_key_path(&self.data_dir))?;
            sync_directory(&self.data_dir.join("secrets"))?;
        }
        Ok(())
    }

    fn rollback(self) -> Result<(), TeslaMateCredentialError> {
        let current = key_path(&self.data_dir);
        if path_exists(&current)? {
            remove_checked_key_file(&current)?;
        }
        if self.had_previous {
            fs::rename(previous_key_path(&self.data_dir), &current)
                .map_err(TeslaMateCredentialError::RestoreKey)?;
        }
        sync_directory(&self.data_dir.join("secrets"))
    }
}

fn begin_key_replacement(
    data_dir: &Path,
    key: &[u8],
) -> Result<KeyReplacement, TeslaMateCredentialError> {
    validate_key(key)?;
    let secrets_dir = data_dir.join("secrets");
    ensure_secrets_directory(&secrets_dir)?;
    let current = key_path(data_dir);
    let previous = previous_key_path(data_dir);
    if path_exists(&previous)? {
        return Err(TeslaMateCredentialError::PendingKeyReplacement);
    }
    let had_previous = path_exists(&current)?;
    if had_previous {
        validate_key_file(&current)?;
    }

    let temporary = secrets_dir.join(format!(".teslamate-encryption-{}.tmp", Uuid::new_v4()));
    write_private_key_file(&temporary, key)?;
    if had_previous {
        if let Err(error) = fs::rename(&current, &previous) {
            let _ = fs::remove_file(&temporary);
            return Err(TeslaMateCredentialError::ReplaceKey(error));
        }
        sync_directory(&secrets_dir)?;
    }
    if let Err(error) = fs::rename(&temporary, &current) {
        if had_previous {
            let _ = fs::rename(&previous, &current);
            let _ = sync_directory(&secrets_dir);
        }
        let _ = fs::remove_file(&temporary);
        return Err(TeslaMateCredentialError::ReplaceKey(error));
    }
    sync_directory(&secrets_dir)?;
    Ok(KeyReplacement {
        data_dir: data_dir.to_path_buf(),
        had_previous,
    })
}

fn write_private_key_file(path: &Path, key: &[u8]) -> Result<(), TeslaMateCredentialError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(KEY_FILE_MODE)
        .open(path)
        .map_err(TeslaMateCredentialError::CreateTemporaryKey)?;
    file.set_permissions(fs::Permissions::from_mode(KEY_FILE_MODE))
        .map_err(TeslaMateCredentialError::ProtectTemporaryKey)?;
    file.write_all(key)
        .map_err(TeslaMateCredentialError::WriteTemporaryKey)?;
    file.sync_all()
        .map_err(TeslaMateCredentialError::SyncTemporaryKey)
}

fn path_exists(path: &Path) -> Result<bool, TeslaMateCredentialError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(TeslaMateCredentialError::InspectKey(error)),
    }
}

fn remove_checked_key_file(path: &Path) -> Result<(), TeslaMateCredentialError> {
    validate_key_file(path)?;
    fs::remove_file(path).map_err(TeslaMateCredentialError::RemoveKey)
}

fn restore_previous_key(data_dir: &Path) -> Result<(), TeslaMateCredentialError> {
    let current = key_path(data_dir);
    let previous = previous_key_path(data_dir);
    validate_key_file(&previous)?;
    if path_exists(&current)? {
        remove_checked_key_file(&current)?;
    }
    fs::rename(&previous, &current).map_err(TeslaMateCredentialError::RestoreKey)?;
    sync_directory(&data_dir.join("secrets"))
}

/// Load the exact stored bytes without trimming, parsing, logging, or copying
/// them into a string.
pub(crate) fn load_key(
    data_dir: &Path,
) -> Result<TeslaMateEncryptionKey, TeslaMateCredentialError> {
    let secrets_dir = data_dir.join("secrets");
    validate_secrets_directory(&secrets_dir)?;
    let path = key_path(data_dir);
    load_key_file(&path)
}

fn load_key_file(path: &Path) -> Result<TeslaMateEncryptionKey, TeslaMateCredentialError> {
    let key = read_checked_key_file(path, MAX_KEY_BYTES)?;
    validate_key(&key)?;
    Ok(TeslaMateEncryptionKey(key))
}

fn create_cursor_key_once(
    secrets_dir: &Path,
    destination: &Path,
) -> Result<crate::protocol::CursorKey, TeslaMateCredentialError> {
    let temporary = secrets_dir.join(format!(".hub-cursor-{}.tmp", Uuid::new_v4()));
    let mut bytes = [0_u8; CURSOR_KEY_BYTES];
    getrandom::fill(&mut bytes).expect("system entropy");
    let created = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(KEY_FILE_MODE)
            .open(&temporary)
            .map_err(TeslaMateCredentialError::CreateTemporaryCursorKey)?;
        file.set_permissions(fs::Permissions::from_mode(KEY_FILE_MODE))
            .map_err(TeslaMateCredentialError::ProtectTemporaryCursorKey)?;
        file.write_all(&bytes)
            .map_err(TeslaMateCredentialError::WriteTemporaryCursorKey)?;
        file.sync_all()
            .map_err(TeslaMateCredentialError::SyncTemporaryCursorKey)?;
        match fs::hard_link(&temporary, destination) {
            Ok(()) => {
                fs::remove_file(&temporary)
                    .map_err(TeslaMateCredentialError::RemoveTemporaryCursorKey)?;
                sync_directory(secrets_dir)?;
                Ok(crate::protocol::CursorKey::from_bytes(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temporary);
                load_cursor_key_file(destination)
            }
            Err(error) => Err(TeslaMateCredentialError::PublishCursorKey(error)),
        }
    })();
    if created.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    created
}

fn load_cursor_key_file(
    path: &Path,
) -> Result<crate::protocol::CursorKey, TeslaMateCredentialError> {
    let bytes = read_checked_cursor_key_file(path)?;
    let bytes: [u8; CURSOR_KEY_BYTES] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| TeslaMateCredentialError::InvalidCursorKeyLength)?;
    Ok(crate::protocol::CursorKey::from_bytes(bytes))
}

fn ensure_secrets_directory(path: &Path) -> Result<(), TeslaMateCredentialError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_secrets_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(SECRETS_DIRECTORY_MODE);
            match builder.create(path) {
                Ok(()) => validate_secrets_directory(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    validate_secrets_directory(path)
                }
                Err(error) => Err(TeslaMateCredentialError::CreateSecretsDirectory(error)),
            }
        }
        Err(error) => Err(TeslaMateCredentialError::InspectSecretsDirectory(error)),
    }
}

fn validate_secrets_directory(path: &Path) -> Result<(), TeslaMateCredentialError> {
    let metadata =
        fs::symlink_metadata(path).map_err(TeslaMateCredentialError::InspectSecretsDirectory)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o777 != SECRETS_DIRECTORY_MODE
    {
        return Err(TeslaMateCredentialError::UnsafeSecretsDirectory);
    }
    Ok(())
}

#[cfg(test)]
fn validate_existing_key_file(path: &Path) -> Result<(), TeslaMateCredentialError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_key_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TeslaMateCredentialError::InspectKey(error)),
    }
}

fn validate_key_file(path: &Path) -> Result<(), TeslaMateCredentialError> {
    let metadata = fs::symlink_metadata(path).map_err(TeslaMateCredentialError::InspectKey)?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || metadata.permissions().mode() & 0o777 != KEY_FILE_MODE
        || metadata.uid() != getuid().as_raw()
    {
        return Err(TeslaMateCredentialError::UnsafeKeyFile);
    }
    Ok(())
}

fn read_checked_key_file(
    path: &Path,
    maximum: usize,
) -> Result<Zeroizing<Vec<u8>>, TeslaMateCredentialError> {
    read_checked_secret_file(path, maximum, false)
}

fn read_checked_cursor_key_file(
    path: &Path,
) -> Result<Zeroizing<Vec<u8>>, TeslaMateCredentialError> {
    read_checked_secret_file(path, CURSOR_KEY_BYTES, true)
}

pub(crate) fn load_existing_cursor_key_bytes(
    data_dir: &Path,
) -> Result<Option<Zeroizing<Vec<u8>>>, TeslaMateCredentialError> {
    let path = cursor_key_path(data_dir);
    match fs::symlink_metadata(&path) {
        Ok(_) => read_checked_cursor_key_file(&path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TeslaMateCredentialError::InspectCursorKey(error)),
    }
}

fn read_checked_secret_file(
    path: &Path,
    maximum: usize,
    cursor: bool,
) -> Result<Zeroizing<Vec<u8>>, TeslaMateCredentialError> {
    read_checked_secret_file_after_open(path, maximum, cursor, || {})
}

fn read_checked_secret_file_after_open(
    path: &Path,
    maximum: usize,
    cursor: bool,
    after_open: impl FnOnce(),
) -> Result<Zeroizing<Vec<u8>>, TeslaMateCredentialError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| match error {
        Errno::LOOP => {
            if cursor {
                TeslaMateCredentialError::UnsafeCursorKeyFile
            } else {
                TeslaMateCredentialError::UnsafeKeyFile
            }
        }
        _ => {
            if cursor {
                TeslaMateCredentialError::ReadCursorKey
            } else {
                TeslaMateCredentialError::ReadKey
            }
        }
    })?;
    let held = fstat(&descriptor).map_err(|_| {
        if cursor {
            TeslaMateCredentialError::ReadCursorKey
        } else {
            TeslaMateCredentialError::ReadKey
        }
    })?;
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || (held.st_mode as u32 & 0o777) != KEY_FILE_MODE
        || held.st_uid != getuid().as_raw()
    {
        return Err(if cursor {
            TeslaMateCredentialError::UnsafeCursorKeyFile
        } else {
            TeslaMateCredentialError::UnsafeKeyFile
        });
    }
    let flags = fcntl_getfl(&descriptor).map_err(|_| {
        if cursor {
            TeslaMateCredentialError::ReadCursorKey
        } else {
            TeslaMateCredentialError::ReadKey
        }
    })?;
    fcntl_setfl(&descriptor, flags & !OFlags::NONBLOCK).map_err(|_| {
        if cursor {
            TeslaMateCredentialError::ReadCursorKey
        } else {
            TeslaMateCredentialError::ReadKey
        }
    })?;

    after_open();

    let file: File = descriptor.into();
    let mut bytes = Zeroizing::new(Vec::with_capacity(maximum.min(8 * 1024)));
    file.take(u64::try_from(maximum + 1).expect("key cap fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|_| {
            if cursor {
                TeslaMateCredentialError::ReadCursorKey
            } else {
                TeslaMateCredentialError::ReadKey
            }
        })?;
    if bytes.len() > maximum {
        return Err(if cursor {
            TeslaMateCredentialError::InvalidCursorKeyLength
        } else {
            TeslaMateCredentialError::KeyTooLarge
        });
    }

    let current = fs::symlink_metadata(path).map_err(|_| {
        if cursor {
            TeslaMateCredentialError::CursorKeyIdentityChanged
        } else {
            TeslaMateCredentialError::KeyIdentityChanged
        }
    })?;
    if current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.uid() != held.st_uid
        || current.dev() != held.st_dev as u64
        || current.ino() != held.st_ino
        || current.permissions().mode() & 0o777 != KEY_FILE_MODE
    {
        return Err(if cursor {
            TeslaMateCredentialError::CursorKeyIdentityChanged
        } else {
            TeslaMateCredentialError::KeyIdentityChanged
        });
    }
    Ok(bytes)
}

fn validate_key(key: &[u8]) -> Result<(), TeslaMateCredentialError> {
    if key.is_empty() {
        return Err(TeslaMateCredentialError::EmptyKey);
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(TeslaMateCredentialError::KeyTooLarge);
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), TeslaMateCredentialError> {
    File::open(path)
        .map_err(TeslaMateCredentialError::OpenSecretsDirectory)?
        .sync_all()
        .map_err(TeslaMateCredentialError::SyncSecretsDirectory)
}

#[derive(Debug, Error)]
pub enum TeslaMateCredentialError {
    #[error("TeslaMate encryption key is empty")]
    EmptyKey,
    #[error("TeslaMate encryption key exceeds the fixed size limit")]
    KeyTooLarge,
    #[error("cannot create TeslaMate secrets directory: {0}")]
    CreateSecretsDirectory(std::io::Error),
    #[error("cannot inspect TeslaMate secrets directory: {0}")]
    InspectSecretsDirectory(std::io::Error),
    #[error("TeslaMate secrets directory has unsafe type or mode")]
    UnsafeSecretsDirectory,
    #[error("cannot inspect TeslaMate encryption key file: {0}")]
    InspectKey(std::io::Error),
    #[error("TeslaMate encryption key file has unsafe type or mode")]
    UnsafeKeyFile,
    #[error("cannot create temporary TeslaMate encryption key: {0}")]
    CreateTemporaryKey(std::io::Error),
    #[error("cannot protect temporary TeslaMate encryption key: {0}")]
    ProtectTemporaryKey(std::io::Error),
    #[error("cannot write temporary TeslaMate encryption key: {0}")]
    WriteTemporaryKey(std::io::Error),
    #[error("cannot sync temporary TeslaMate encryption key: {0}")]
    SyncTemporaryKey(std::io::Error),
    #[error("cannot replace TeslaMate encryption key: {0}")]
    ReplaceKey(std::io::Error),
    #[error("cannot remove superseded TeslaMate encryption key: {0}")]
    RemoveKey(std::io::Error),
    #[error("cannot restore previous TeslaMate encryption key: {0}")]
    RestoreKey(std::io::Error),
    #[error("an interrupted TeslaMate key replacement must be recovered first")]
    PendingKeyReplacement,
    #[error("an interrupted TeslaMate key replacement has no committed token pair")]
    PendingKeyReplacementWithoutTokens,
    #[error("neither private TeslaMate key generation decrypts the committed token pair")]
    NoMatchingKeyGeneration,
    #[error("cannot open TeslaMate secrets directory: {0}")]
    OpenSecretsDirectory(std::io::Error),
    #[error("cannot sync TeslaMate secrets directory: {0}")]
    SyncSecretsDirectory(std::io::Error),
    #[error("cannot read TeslaMate encryption key")]
    ReadKey,
    #[error("TeslaMate encryption key identity changed while reading")]
    KeyIdentityChanged,
    #[error("cannot inspect Hub cursor key: {0}")]
    InspectCursorKey(std::io::Error),
    #[error("Hub cursor key has unsafe type or mode")]
    UnsafeCursorKeyFile,
    #[error("Hub cursor key must be exactly 32 bytes")]
    InvalidCursorKeyLength,
    #[error("cannot create temporary Hub cursor key: {0}")]
    CreateTemporaryCursorKey(std::io::Error),
    #[error("cannot protect temporary Hub cursor key: {0}")]
    ProtectTemporaryCursorKey(std::io::Error),
    #[error("cannot write temporary Hub cursor key: {0}")]
    WriteTemporaryCursorKey(std::io::Error),
    #[error("cannot sync temporary Hub cursor key: {0}")]
    SyncTemporaryCursorKey(std::io::Error),
    #[error("cannot publish Hub cursor key: {0}")]
    PublishCursorKey(std::io::Error),
    #[error("cannot remove temporary Hub cursor key: {0}")]
    RemoveTemporaryCursorKey(std::io::Error),
    #[error("cannot read Hub cursor key")]
    ReadCursorKey,
    #[error("Hub cursor key identity changed while reading")]
    CursorKeyIdentityChanged,
}

#[derive(Debug, Error)]
pub enum TeslaMateCredentialImportError {
    #[error(transparent)]
    Credential(#[from] TeslaMateCredentialError),
    #[error("cannot store TeslaMate token pair: {0}")]
    TokenStore(#[source] crate::db::StoreError),
    #[error("cannot authenticate TeslaMate token pair: {0}")]
    TokenCipher(#[source] crate::teslamate_token::TeslaMateTokenError),
    #[error("cannot store TeslaMate token pair ({store}); key rollback also failed: {rollback}")]
    Rollback {
        store: String,
        rollback: TeslaMateCredentialError,
    },
}

#[derive(Debug, Error)]
pub enum TeslaMateCredentialRemovalError {
    #[error("cannot remove TeslaMate token pair: {0}")]
    TokenStore(#[source] crate::db::StoreError),
    #[error(transparent)]
    Credential(#[from] TeslaMateCredentialError),
}

#[cfg(test)]
mod tests {
    use std::{
        fs, os::unix::fs::PermissionsExt, process::Command, sync::mpsc, thread, time::Duration,
    };

    use super::*;

    fn encrypted_store(
        key: &[u8],
        access: &str,
        refresh: &str,
    ) -> crate::db::TeslaMateLegacyTokenStore {
        let tokens = crate::credentials::OwnerTokens::from_secret_parts(
            access.to_owned(),
            refresh.to_owned(),
        )
        .expect("test tokens");
        let (access, refresh) = crate::teslamate_token::encrypt_legacy_owner_tokens(key, &tokens)
            .expect("encrypt pair");
        let generation = crate::teslamate_token::legacy_refresh_credential_generation(&tokens);
        crate::db::TeslaMateLegacyTokenStore::imported(access, refresh)
            .expect("stored pair")
            .with_credential_generation(generation)
            .expect("credential generation")
    }

    #[test]
    fn replaces_and_loads_exact_key_with_private_permissions() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        replace_key(temporary.path(), b"first-key").expect("first key writes");
        replace_key(temporary.path(), b"second-key").expect("second key replaces first");

        let loaded = load_key(temporary.path()).expect("key loads");
        assert_eq!(loaded.as_bytes(), b"second-key");
        let secrets = temporary.path().join("secrets");
        assert_eq!(
            fs::symlink_metadata(&secrets)
                .expect("secrets metadata")
                .permissions()
                .mode()
                & 0o777,
            SECRETS_DIRECTORY_MODE
        );
        assert_eq!(
            fs::symlink_metadata(key_path(temporary.path()))
                .expect("key metadata")
                .permissions()
                .mode()
                & 0o777,
            KEY_FILE_MODE
        );
    }

    #[test]
    fn key_and_ciphertext_replacement_recovers_both_crash_sides() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let store = crate::db::HubStore::initialize(temporary.path()).expect("store");
        let old_key = b"old exact TeslaMate key";
        let new_key = b"new exact TeslaMate key";
        let old_store = encrypted_store(old_key, "old-access", "old-refresh");
        let new_store = encrypted_store(new_key, "new-access", "new-refresh");

        replace_key_and_tokens(temporary.path(), &store, old_key, &old_store)
            .expect("initial pair");

        // Crash before the SQLite commit: the durable old pair selects and
        // restores the previous key generation.
        drop(begin_key_replacement(temporary.path(), new_key).expect("stage new key"));
        let recovered = load_key_for_tokens(temporary.path(), &old_store).expect("recover old key");
        assert_eq!(recovered.as_bytes(), old_key);
        assert!(!previous_key_path(temporary.path()).exists());

        // Crash after the SQLite commit: the durable new pair selects the new
        // key and only discards the retained old generation.
        drop(begin_key_replacement(temporary.path(), new_key).expect("stage new key"));
        store
            .replace_teslamate_legacy_tokens(&new_store)
            .expect("commit new pair");
        let recovered = load_key_for_tokens(temporary.path(), &new_store).expect("keep new key");
        assert_eq!(recovered.as_bytes(), new_key);
        assert!(!previous_key_path(temporary.path()).exists());
    }

    #[test]
    fn ambiguous_refresh_rejects_same_plaintext_under_new_random_envelopes() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let store = crate::db::HubStore::initialize(temporary.path()).expect("store");
        let first_key = b"first exact TeslaMate key";
        let second_key = b"second exact TeslaMate key";
        let first = encrypted_store(first_key, "same-access", "same-refresh");
        replace_key_and_tokens(temporary.path(), &store, first_key, &first).expect("initial pair");
        let generation = store
            .load_teslamate_legacy_tokens()
            .expect("load initial pair")
            .expect("initial pair")
            .credential_generation()
            .expect("bound generation");
        store
            .begin_legacy_refresh(generation)
            .expect("ambiguous refresh intent");

        let reencrypted = encrypted_store(second_key, "same-access", "same-refresh");
        assert!(matches!(
            replace_key_and_tokens(temporary.path(), &store, second_key, &reencrypted,),
            Err(TeslaMateCredentialImportError::TokenStore(
                crate::db::StoreError::LegacyRefreshOutcomeUnknown
            ))
        ));
        assert_eq!(
            load_key_for_tokens(temporary.path(), &first)
                .expect("old key remains")
                .as_bytes(),
            first_key
        );
    }

    #[test]
    fn sign_out_removes_tokens_and_both_key_generations() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let store = crate::db::HubStore::initialize(temporary.path()).expect("store");
        let key = b"exact TeslaMate key";
        let stored = encrypted_store(key, "access", "refresh");
        replace_key_and_tokens(temporary.path(), &store, key, &stored).expect("persist pair");
        drop(begin_key_replacement(temporary.path(), b"replacement key").expect("stage key"));

        remove_key_and_tokens(temporary.path(), &store).expect("remove authority");

        assert!(
            store
                .load_teslamate_legacy_tokens()
                .expect("read token row")
                .is_none()
        );
        assert!(!key_path(temporary.path()).exists());
        assert!(!previous_key_path(temporary.path()).exists());

        remove_key_and_tokens(temporary.path(), &store).expect("idempotent removal");
    }

    #[test]
    fn sign_out_after_ambiguous_refresh_allows_fresh_credentials_without_reusing_input() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let store = crate::db::HubStore::initialize(temporary.path()).expect("store");
        let old_key = b"old exact TeslaMate key";
        let old = encrypted_store(old_key, "old-access", "old-refresh");
        replace_key_and_tokens(temporary.path(), &store, old_key, &old).expect("persist old pair");
        let old_generation = store
            .load_teslamate_legacy_tokens()
            .expect("load old pair")
            .expect("old pair")
            .credential_generation()
            .expect("old generation");
        store
            .begin_legacy_refresh(old_generation)
            .expect("ambiguous refresh starts");

        remove_key_and_tokens(temporary.path(), &store).expect("sign out");
        assert!(
            !store
                .has_unresolved_legacy_refresh()
                .expect("sign out closes ambiguous receipt")
        );

        let fresh_key = b"fresh exact TeslaMate key";
        let fresh = encrypted_store(fresh_key, "fresh-access", "fresh-refresh");
        replace_key_and_tokens(temporary.path(), &store, fresh_key, &fresh)
            .expect("fresh pair persists");
        let reopened = store
            .load_teslamate_legacy_tokens()
            .expect("fresh pair loads")
            .expect("fresh pair");
        let fresh_generation = reopened.credential_generation().expect("fresh generation");
        assert_ne!(fresh_generation, old_generation);
        assert_eq!(
            load_key_for_tokens(temporary.path(), &reopened)
                .expect("fresh key reopens")
                .as_bytes(),
            fresh_key
        );
        assert!(matches!(
            replace_key_and_tokens(temporary.path(), &store, old_key, &old),
            Err(TeslaMateCredentialImportError::TokenStore(
                crate::db::StoreError::LegacyRefreshOutcomeUnknown
            ))
        ));
        let current = store
            .load_teslamate_legacy_tokens()
            .expect("fresh pair reloads")
            .expect("fresh pair remains");
        assert_eq!(current.credential_generation(), Some(fresh_generation));
        assert_eq!(
            load_key_for_tokens(temporary.path(), &current)
                .expect("fresh key remains")
                .as_bytes(),
            fresh_key
        );
    }

    #[test]
    fn rejects_symlinked_key_file() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let secrets = temporary.path().join("secrets");
        fs::create_dir(&secrets).expect("secrets directory");
        fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
            .expect("protect secrets directory");
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"outside").expect("outside file");
        std::os::unix::fs::symlink(&outside, key_path(temporary.path())).expect("key symlink");

        assert!(matches!(
            load_key(temporary.path()),
            Err(TeslaMateCredentialError::UnsafeKeyFile)
        ));
        assert!(matches!(
            replace_key(temporary.path(), b"replacement"),
            Err(TeslaMateCredentialError::UnsafeKeyFile)
        ));
    }

    #[test]
    fn rejects_empty_and_oversized_key_bytes() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        assert!(matches!(
            replace_key(temporary.path(), b""),
            Err(TeslaMateCredentialError::EmptyKey)
        ));
        assert!(matches!(
            replace_key(temporary.path(), &vec![0; MAX_KEY_BYTES + 1]),
            Err(TeslaMateCredentialError::KeyTooLarge)
        ));
    }

    #[test]
    fn key_reader_rejects_oversized_and_replaced_files_after_open() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let secrets = temporary.path().join("secrets");
        fs::create_dir(&secrets).expect("secrets directory");
        fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
            .expect("protect secrets directory");
        let path = key_path(temporary.path());
        fs::write(&path, vec![7_u8; MAX_KEY_BYTES + 1]).expect("oversized key");
        fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE)).expect("key mode");
        assert!(matches!(
            load_key(temporary.path()),
            Err(TeslaMateCredentialError::KeyTooLarge)
        ));

        fs::write(&path, b"original").expect("key");
        fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE)).expect("key mode");
        let replacement = secrets.join("replacement");
        fs::write(&replacement, b"replacement").expect("replacement key");
        fs::set_permissions(&replacement, fs::Permissions::from_mode(KEY_FILE_MODE))
            .expect("replacement mode");
        assert!(matches!(
            read_checked_secret_file_after_open(&path, MAX_KEY_BYTES, false, || {
                fs::rename(&replacement, &path).expect("replace key")
            }),
            Err(TeslaMateCredentialError::KeyIdentityChanged)
        ));
    }

    #[test]
    fn key_readers_reject_fifos_without_waiting_for_a_writer() {
        for cursor in [false, true] {
            let temporary = crate::private_tempdir().expect("temporary directory");
            let path = temporary.path().join(if cursor {
                "cursor-key.fifo"
            } else {
                "encryption-key.fifo"
            });
            assert!(
                Command::new("mkfifo")
                    .arg(&path)
                    .status()
                    .expect("run mkfifo")
                    .success()
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE))
                .expect("FIFO mode");

            let (sender, receiver) = mpsc::channel();
            let worker = thread::spawn(move || {
                let rejected = match read_checked_secret_file(&path, MAX_KEY_BYTES, cursor) {
                    Err(TeslaMateCredentialError::UnsafeCursorKeyFile) if cursor => true,
                    Err(TeslaMateCredentialError::UnsafeKeyFile) if !cursor => true,
                    _ => false,
                };
                sender.send(rejected).expect("send FIFO result");
            });
            assert!(
                receiver
                    .recv_timeout(Duration::from_secs(1))
                    .expect("FIFO admission must not block")
            );
            worker.join().expect("FIFO admission worker");
        }
    }

    #[test]
    fn creates_and_reloads_one_private_cursor_key() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let _first = load_or_create_cursor_key(temporary.path()).expect("cursor key creates");
        let path = cursor_key_path(temporary.path());
        let first_bytes = fs::read(&path).expect("cursor key bytes");
        let _second = load_or_create_cursor_key(temporary.path()).expect("cursor key reloads");

        assert_eq!(first_bytes.len(), CURSOR_KEY_BYTES);
        assert_eq!(
            fs::read(&path).expect("cursor key bytes reopen"),
            first_bytes
        );
        assert_eq!(
            fs::symlink_metadata(&path)
                .expect("cursor key metadata")
                .permissions()
                .mode()
                & 0o777,
            KEY_FILE_MODE
        );
    }

    #[test]
    fn rejects_bad_cursor_key_length_and_mode() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let secrets = temporary.path().join("secrets");
        fs::create_dir(&secrets).expect("secrets directory");
        fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
            .expect("protect secrets directory");
        let path = cursor_key_path(temporary.path());
        fs::write(&path, [0_u8; CURSOR_KEY_BYTES - 1]).expect("short cursor key");
        fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE))
            .expect("protect short cursor key");
        assert!(matches!(
            load_or_create_cursor_key(temporary.path()),
            Err(TeslaMateCredentialError::InvalidCursorKeyLength)
        ));

        fs::write(&path, [0_u8; CURSOR_KEY_BYTES + 1]).expect("oversized cursor key");
        fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE))
            .expect("protect oversized cursor key");
        assert!(matches!(
            load_or_create_cursor_key(temporary.path()),
            Err(TeslaMateCredentialError::InvalidCursorKeyLength)
        ));

        fs::write(&path, [0_u8; CURSOR_KEY_BYTES]).expect("cursor key");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .expect("weaken cursor key mode");
        assert!(matches!(
            load_or_create_cursor_key(temporary.path()),
            Err(TeslaMateCredentialError::UnsafeCursorKeyFile)
        ));

        fs::write(&path, [0_u8; CURSOR_KEY_BYTES]).expect("cursor key");
        fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE))
            .expect("cursor key mode");
        let replacement = secrets.join("replacement-cursor");
        fs::write(&replacement, [1_u8; CURSOR_KEY_BYTES]).expect("replacement cursor key");
        fs::set_permissions(&replacement, fs::Permissions::from_mode(KEY_FILE_MODE))
            .expect("replacement cursor key mode");
        assert!(matches!(
            read_checked_secret_file_after_open(&path, CURSOR_KEY_BYTES, true, || {
                fs::rename(&replacement, &path).expect("replace cursor key")
            }),
            Err(TeslaMateCredentialError::CursorKeyIdentityChanged)
        ));
    }

    #[test]
    fn rejects_symlinked_cursor_key() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let secrets = temporary.path().join("secrets");
        fs::create_dir(&secrets).expect("secrets directory");
        fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
            .expect("protect secrets directory");
        let outside = temporary.path().join("outside");
        fs::write(&outside, [0_u8; CURSOR_KEY_BYTES]).expect("outside file");
        std::os::unix::fs::symlink(&outside, cursor_key_path(temporary.path()))
            .expect("cursor key symlink");

        assert!(matches!(
            load_or_create_cursor_key(temporary.path()),
            Err(TeslaMateCredentialError::UnsafeCursorKeyFile)
        ));
    }
}
