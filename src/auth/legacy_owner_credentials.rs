// SPDX-License-Identifier: AGPL-3.0-only

//! Minimal on-disk ownership for the exact source database `ENCRYPTION_KEY` bytes.

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

use crate::platform::file_attempts::note as attempt;

const SECRETS_DIRECTORY_MODE: u32 = 0o700;
const KEY_FILE_MODE: u32 = 0o600;
const CURSOR_KEY_BYTES: usize = 32;













pub fn cursor_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets/hub-cursor.key")
}

/// Load the durable Hub cursor-signing key, creating it exactly once when the
/// Hub data directory is first initialized.
pub fn load_or_create_cursor_key(
    data_dir: &Path,
) -> Result<crate::protocol::CursorKey, SourceDbCredentialError> {
    let secrets_dir = data_dir.join("secrets");
    ensure_secrets_directory(&secrets_dir)?;
    let destination = cursor_key_path(data_dir);
    match fs::symlink_metadata(&destination) {
        Ok(_) => load_cursor_key_file(&destination),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_cursor_key_once(&secrets_dir, &destination)
        }
        Err(error) => Err(SourceDbCredentialError::InspectCursorKey(error)),
    }
}









































fn create_cursor_key_once(
    secrets_dir: &Path,
    destination: &Path,
) -> Result<crate::protocol::CursorKey, SourceDbCredentialError> {
    let temporary = secrets_dir.join(format!(".hub-cursor-{}.tmp", Uuid::new_v4()));
    let mut bytes = [0_u8; CURSOR_KEY_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| SourceDbCredentialError::EntropyUnavailable)?;
    let created = (|| {
        attempt(&temporary);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(KEY_FILE_MODE)
            .open(&temporary)
            .map_err(SourceDbCredentialError::CreateTemporaryCursorKey)?;
        file.set_permissions(fs::Permissions::from_mode(KEY_FILE_MODE))
            .map_err(SourceDbCredentialError::ProtectTemporaryCursorKey)?;
        attempt(&temporary);
        file.write_all(&bytes)
            .map_err(SourceDbCredentialError::WriteTemporaryCursorKey)?;
        attempt(&temporary);
        file.sync_all()
            .map_err(SourceDbCredentialError::SyncTemporaryCursorKey)?;
        attempt(destination);
        match fs::hard_link(&temporary, destination) {
            Ok(()) => {
                attempt(&temporary);
                fs::remove_file(&temporary)
                    .map_err(SourceDbCredentialError::RemoveTemporaryCursorKey)?;
                sync_directory(secrets_dir)?;
                Ok(crate::protocol::CursorKey::from_bytes(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt(&temporary);
                let _ = fs::remove_file(&temporary);
                load_cursor_key_file(destination)
            }
            Err(error) => Err(SourceDbCredentialError::PublishCursorKey(error)),
        }
    })();
    if created.is_err() {
        attempt(&temporary);
        let _ = fs::remove_file(&temporary);
    }
    created
}

fn load_cursor_key_file(
    path: &Path,
) -> Result<crate::protocol::CursorKey, SourceDbCredentialError> {
    let bytes = read_checked_cursor_key_file(path)?;
    let bytes: [u8; CURSOR_KEY_BYTES] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| SourceDbCredentialError::InvalidCursorKeyLength)?;
    Ok(crate::protocol::CursorKey::from_bytes(bytes))
}

fn ensure_secrets_directory(path: &Path) -> Result<(), SourceDbCredentialError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_secrets_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(SECRETS_DIRECTORY_MODE);
            attempt(path);
            match builder.create(path) {
                Ok(()) => validate_secrets_directory(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    validate_secrets_directory(path)
                }
                Err(error) => Err(SourceDbCredentialError::CreateSecretsDirectory(error)),
            }
        }
        Err(error) => Err(SourceDbCredentialError::InspectSecretsDirectory(error)),
    }
}

fn validate_secrets_directory(path: &Path) -> Result<(), SourceDbCredentialError> {
    let metadata =
        fs::symlink_metadata(path).map_err(SourceDbCredentialError::InspectSecretsDirectory)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o777 != SECRETS_DIRECTORY_MODE
    {
        return Err(SourceDbCredentialError::UnsafeSecretsDirectory);
    }
    Ok(())
}







fn read_checked_cursor_key_file(
    path: &Path,
) -> Result<Zeroizing<Vec<u8>>, SourceDbCredentialError> {
    read_checked_secret_file(path, CURSOR_KEY_BYTES, true)
}

pub(crate) fn load_existing_cursor_key_bytes(
    data_dir: &Path,
) -> Result<Option<Zeroizing<Vec<u8>>>, SourceDbCredentialError> {
    let path = cursor_key_path(data_dir);
    match fs::symlink_metadata(&path) {
        Ok(_) => read_checked_cursor_key_file(&path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SourceDbCredentialError::InspectCursorKey(error)),
    }
}

fn read_checked_secret_file(
    path: &Path,
    maximum: usize,
    cursor: bool,
) -> Result<Zeroizing<Vec<u8>>, SourceDbCredentialError> {
    read_checked_secret_file_after_open(path, maximum, cursor, || {})
}

fn read_checked_secret_file_after_open(
    path: &Path,
    maximum: usize,
    cursor: bool,
    after_open: impl FnOnce(),
) -> Result<Zeroizing<Vec<u8>>, SourceDbCredentialError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| match error {
        Errno::LOOP => {
            if cursor {
                SourceDbCredentialError::UnsafeCursorKeyFile
            } else {
                SourceDbCredentialError::UnsafeKeyFile
            }
        }
        _ => {
            if cursor {
                SourceDbCredentialError::ReadCursorKey
            } else {
                SourceDbCredentialError::ReadKey
            }
        }
    })?;
    let held = fstat(&descriptor).map_err(|_| {
        if cursor {
            SourceDbCredentialError::ReadCursorKey
        } else {
            SourceDbCredentialError::ReadKey
        }
    })?;
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || (held.st_mode as u32 & 0o777) != KEY_FILE_MODE
        || held.st_uid != getuid().as_raw()
    {
        return Err(if cursor {
            SourceDbCredentialError::UnsafeCursorKeyFile
        } else {
            SourceDbCredentialError::UnsafeKeyFile
        });
    }
    let flags = fcntl_getfl(&descriptor).map_err(|_| {
        if cursor {
            SourceDbCredentialError::ReadCursorKey
        } else {
            SourceDbCredentialError::ReadKey
        }
    })?;
    fcntl_setfl(&descriptor, flags & !OFlags::NONBLOCK).map_err(|_| {
        if cursor {
            SourceDbCredentialError::ReadCursorKey
        } else {
            SourceDbCredentialError::ReadKey
        }
    })?;

    after_open();

    let file: File = descriptor.into();
    let mut bytes = Zeroizing::new(Vec::with_capacity(maximum.min(8 * 1024)));
    file.take(u64::try_from(maximum + 1).expect("key cap fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|_| {
            if cursor {
                SourceDbCredentialError::ReadCursorKey
            } else {
                SourceDbCredentialError::ReadKey
            }
        })?;
    if bytes.len() > maximum {
        return Err(if cursor {
            SourceDbCredentialError::InvalidCursorKeyLength
        } else {
            SourceDbCredentialError::KeyTooLarge
        });
    }

    let current = fs::symlink_metadata(path).map_err(|_| {
        if cursor {
            SourceDbCredentialError::CursorKeyIdentityChanged
        } else {
            SourceDbCredentialError::KeyIdentityChanged
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
            SourceDbCredentialError::CursorKeyIdentityChanged
        } else {
            SourceDbCredentialError::KeyIdentityChanged
        });
    }
    Ok(bytes)
}



fn sync_directory(path: &Path) -> Result<(), SourceDbCredentialError> {
    attempt(path);
    File::open(path)
        .map_err(SourceDbCredentialError::OpenSecretsDirectory)?
        .sync_all()
        .map_err(SourceDbCredentialError::SyncSecretsDirectory)
}

#[derive(Debug, Error)]
pub enum SourceDbCredentialError {
    #[error("operating-system entropy is unavailable for credential creation")]
    EntropyUnavailable,
    #[error("source token key is empty")]
    EmptyKey,
    #[error("source token key exceeds the fixed size limit")]
    KeyTooLarge,
    #[error("cannot create source database secrets directory: {0}")]
    CreateSecretsDirectory(std::io::Error),
    #[error("cannot inspect source database secrets directory: {0}")]
    InspectSecretsDirectory(std::io::Error),
    #[error("Source database secrets directory has unsafe type or mode")]
    UnsafeSecretsDirectory,
    #[error("cannot inspect source token key file: {0}")]
    InspectKey(std::io::Error),
    #[error("source token key file has unsafe type or mode")]
    UnsafeKeyFile,
    #[error("cannot create temporary source token key: {0}")]
    CreateTemporaryKey(std::io::Error),
    #[error("cannot protect temporary source token key: {0}")]
    ProtectTemporaryKey(std::io::Error),
    #[error("cannot write temporary source token key: {0}")]
    WriteTemporaryKey(std::io::Error),
    #[error("cannot sync temporary source token key: {0}")]
    SyncTemporaryKey(std::io::Error),
    #[error("cannot replace source token key: {0}")]
    ReplaceKey(std::io::Error),
    #[error("cannot remove superseded source token key: {0}")]
    RemoveKey(std::io::Error),
    #[error("cannot restore previous source token key: {0}")]
    RestoreKey(std::io::Error),
    #[error("an interrupted source database key replacement must be recovered first")]
    PendingKeyReplacement,
    #[error("an interrupted source database key replacement has no committed token pair")]
    PendingKeyReplacementWithoutTokens,
    #[error("neither private source database key generation decrypts the committed token pair")]
    NoMatchingKeyGeneration,
    #[error(
        "the run's guard refused the settlement of an interrupted source database key replacement"
    )]
    SettlementRefused,
    #[error("cannot open source database secrets directory: {0}")]
    OpenSecretsDirectory(std::io::Error),
    #[error("cannot sync source database secrets directory: {0}")]
    SyncSecretsDirectory(std::io::Error),
    #[error("cannot read source token key")]
    ReadKey,
    #[error("source token key identity changed while reading")]
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
pub enum SourceDbCredentialImportError {
    #[error(transparent)]
    Credential(#[from] SourceDbCredentialError),
    #[error("cannot store legacy owner token pair: {0}")]
    TokenStore(#[source] crate::db::StoreError),
    #[error("cannot authenticate legacy owner token pair: {0}")]
    TokenCipher(#[source] crate::source_token_envelope::SourceDbTokenError),
    #[error("cannot store legacy owner token pair ({store}); key rollback also failed: {rollback}")]
    Rollback {
        store: String,
        rollback: SourceDbCredentialError,
    },
}

#[derive(Debug, Error)]
pub enum SourceDbCredentialRemovalError {
    #[error("cannot remove legacy owner token pair: {0}")]
    TokenStore(#[source] crate::db::StoreError),
    #[error(transparent)]
    Credential(#[from] SourceDbCredentialError),
}

#[cfg(test)]
#[path = "legacy_owner_credentials/tests.rs"]
mod tests;

#[cfg(test)]
mod n1_src_auth_legacy_owner_credentials_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for SourceDbCredentialError {
        fn error_example() -> Self {
            SourceDbCredentialError::EntropyUnavailable
        }
    }

    impl ErrorExample for SourceDbCredentialImportError {
        fn error_example() -> Self {
            SourceDbCredentialImportError::Credential(example())
        }
    }

    impl ErrorExample for SourceDbCredentialRemovalError {
        fn error_example() -> Self {
            SourceDbCredentialRemovalError::TokenStore(example())
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_source_db_credential_error(error: SourceDbCredentialError) {
            match &error {
                SourceDbCredentialError::EntropyUnavailable => (),
                SourceDbCredentialError::EmptyKey => (),
                SourceDbCredentialError::KeyTooLarge => (),
                SourceDbCredentialError::CreateSecretsDirectory(..) => (),
                SourceDbCredentialError::InspectSecretsDirectory(..) => (),
                SourceDbCredentialError::UnsafeSecretsDirectory => (),
                SourceDbCredentialError::InspectKey(..) => (),
                SourceDbCredentialError::UnsafeKeyFile => (),
                SourceDbCredentialError::CreateTemporaryKey(..) => (),
                SourceDbCredentialError::ProtectTemporaryKey(..) => (),
                SourceDbCredentialError::WriteTemporaryKey(..) => (),
                SourceDbCredentialError::SyncTemporaryKey(..) => (),
                SourceDbCredentialError::ReplaceKey(..) => (),
                SourceDbCredentialError::RemoveKey(..) => (),
                SourceDbCredentialError::RestoreKey(..) => (),
                SourceDbCredentialError::PendingKeyReplacement => (),
                SourceDbCredentialError::PendingKeyReplacementWithoutTokens => (),
                SourceDbCredentialError::NoMatchingKeyGeneration => (),
                SourceDbCredentialError::SettlementRefused => (),
                SourceDbCredentialError::OpenSecretsDirectory(..) => (),
                SourceDbCredentialError::SyncSecretsDirectory(..) => (),
                SourceDbCredentialError::ReadKey => (),
                SourceDbCredentialError::KeyIdentityChanged => (),
                SourceDbCredentialError::InspectCursorKey(..) => (),
                SourceDbCredentialError::UnsafeCursorKeyFile => (),
                SourceDbCredentialError::InvalidCursorKeyLength => (),
                SourceDbCredentialError::CreateTemporaryCursorKey(..) => (),
                SourceDbCredentialError::ProtectTemporaryCursorKey(..) => (),
                SourceDbCredentialError::WriteTemporaryCursorKey(..) => (),
                SourceDbCredentialError::SyncTemporaryCursorKey(..) => (),
                SourceDbCredentialError::PublishCursorKey(..) => (),
                SourceDbCredentialError::RemoveTemporaryCursorKey(..) => (),
                SourceDbCredentialError::ReadCursorKey => (),
                SourceDbCredentialError::CursorKeyIdentityChanged => (),
            }
            assert_name_free(&error);
        }
        check_source_db_credential_error(SourceDbCredentialError::EntropyUnavailable);
        check_source_db_credential_error(SourceDbCredentialError::EmptyKey);
        check_source_db_credential_error(SourceDbCredentialError::KeyTooLarge);
        check_source_db_credential_error(
            SourceDbCredentialError::CreateSecretsDirectory(example()),
        );
        check_source_db_credential_error(SourceDbCredentialError::InspectSecretsDirectory(
            example(),
        ));
        check_source_db_credential_error(SourceDbCredentialError::UnsafeSecretsDirectory);
        check_source_db_credential_error(SourceDbCredentialError::InspectKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::UnsafeKeyFile);
        check_source_db_credential_error(SourceDbCredentialError::CreateTemporaryKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::ProtectTemporaryKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::WriteTemporaryKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::SyncTemporaryKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::ReplaceKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::RemoveKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::RestoreKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::PendingKeyReplacement);
        check_source_db_credential_error(
            SourceDbCredentialError::PendingKeyReplacementWithoutTokens,
        );
        check_source_db_credential_error(SourceDbCredentialError::NoMatchingKeyGeneration);
        check_source_db_credential_error(SourceDbCredentialError::SettlementRefused);
        check_source_db_credential_error(SourceDbCredentialError::OpenSecretsDirectory(example()));
        check_source_db_credential_error(SourceDbCredentialError::SyncSecretsDirectory(example()));
        check_source_db_credential_error(SourceDbCredentialError::ReadKey);
        check_source_db_credential_error(SourceDbCredentialError::KeyIdentityChanged);
        check_source_db_credential_error(SourceDbCredentialError::InspectCursorKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::UnsafeCursorKeyFile);
        check_source_db_credential_error(SourceDbCredentialError::InvalidCursorKeyLength);
        check_source_db_credential_error(SourceDbCredentialError::CreateTemporaryCursorKey(
            example(),
        ));
        check_source_db_credential_error(SourceDbCredentialError::ProtectTemporaryCursorKey(
            example(),
        ));
        check_source_db_credential_error(SourceDbCredentialError::WriteTemporaryCursorKey(
            example(),
        ));
        check_source_db_credential_error(
            SourceDbCredentialError::SyncTemporaryCursorKey(example()),
        );
        check_source_db_credential_error(SourceDbCredentialError::PublishCursorKey(example()));
        check_source_db_credential_error(SourceDbCredentialError::RemoveTemporaryCursorKey(
            example(),
        ));
        check_source_db_credential_error(SourceDbCredentialError::ReadCursorKey);
        check_source_db_credential_error(SourceDbCredentialError::CursorKeyIdentityChanged);
        fn check_source_db_credential_import_error(error: SourceDbCredentialImportError) {
            match &error {
                SourceDbCredentialImportError::Credential(..) => (),
                SourceDbCredentialImportError::TokenStore(..) => (),
                SourceDbCredentialImportError::TokenCipher(..) => (),
                SourceDbCredentialImportError::Rollback { .. } => (),
            }
            assert_name_free(&error);
        }
        check_source_db_credential_import_error(SourceDbCredentialImportError::Credential(
            example(),
        ));
        check_source_db_credential_import_error(SourceDbCredentialImportError::TokenStore(
            example(),
        ));
        check_source_db_credential_import_error(SourceDbCredentialImportError::TokenCipher(
            example(),
        ));
        check_source_db_credential_import_error(SourceDbCredentialImportError::Rollback {
            store: example(),
            rollback: example(),
        });
        fn check_source_db_credential_removal_error(error: SourceDbCredentialRemovalError) {
            match &error {
                SourceDbCredentialRemovalError::TokenStore(..) => (),
                SourceDbCredentialRemovalError::Credential(..) => (),
            }
            assert_name_free(&error);
        }
        check_source_db_credential_removal_error(SourceDbCredentialRemovalError::TokenStore(
            example(),
        ));
        check_source_db_credential_removal_error(SourceDbCredentialRemovalError::Credential(
            example(),
        ));
    }
}
