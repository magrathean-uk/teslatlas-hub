//! Runtime-only access to systemd credentials.
//!
//! A Hub owner token is decrypted by systemd for the service, then exposed as
//! a short-lived regular file below `CREDENTIALS_DIRECTORY`. This module never
//! reads a token from configuration, argv, an environment value, or the Hub
//! database. It also deliberately does not log token parse errors with content.

use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::protocol::CursorKey;
use thiserror::Error;

pub const OWNER_TOKEN_CREDENTIAL: &str = "owner-token";
pub const CURSOR_KEY_CREDENTIAL: &str = "cursor-key";
pub const TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL: &str = "teslamate-postgres-password";
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const CURSOR_KEY_BYTES: usize = 32;
const MAX_POSTGRES_PASSWORD_BYTES: usize = 4 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct OwnerToken(String);

impl OwnerToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The migration source password supplied by systemd for one read-only
/// PostgreSQL session. It has the same no-log/no-config boundary as the owner
/// token, but a distinct type makes accidental API-token use a compile error.
#[derive(Clone, PartialEq, Eq)]
pub struct TeslaMatePostgresPassword(String);

impl TeslaMatePostgresPassword {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for TeslaMatePostgresPassword {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TeslaMatePostgresPassword([redacted])")
    }
}

impl std::fmt::Debug for OwnerToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OwnerToken([redacted])")
    }
}

#[derive(Debug, Clone)]
pub struct CredentialDirectory {
    path: PathBuf,
}

impl CredentialDirectory {
    pub fn from_systemd_environment() -> Result<Option<Self>, CredentialError> {
        let Some(path) = env::var_os("CREDENTIALS_DIRECTORY") else {
            return Ok(None);
        };
        if path.is_empty() {
            return Err(CredentialError::EmptyDirectory);
        }
        Ok(Some(Self {
            path: PathBuf::from(path),
        }))
    }

    pub fn required_from_systemd_environment() -> Result<Self, CredentialError> {
        Self::from_systemd_environment()?.ok_or(CredentialError::MissingDirectory)
    }

    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Load the service-provided owner token only when collection is started.
    /// A missing file is an incomplete service setup, not an anonymous token.
    pub fn owner_token(&self) -> Result<OwnerToken, CredentialError> {
        let bytes = self
            .read_private_credential(OWNER_TOKEN_CREDENTIAL, MAX_TOKEN_BYTES)
            .map_err(|error| match error {
                CredentialError::CredentialTooLarge => CredentialError::TokenTooLarge,
                error => error,
            })?;
        if bytes.is_empty() {
            return Err(CredentialError::EmptyToken);
        }
        if bytes.contains(&0) || bytes.contains(&b'\r') || bytes.contains(&b'\n') {
            return Err(CredentialError::InvalidTokenBytes);
        }
        let value = String::from_utf8(bytes).map_err(|_| CredentialError::InvalidTokenEncoding)?;
        Ok(OwnerToken(value))
    }

    /// Load the 32-byte signing key supplied to both Hub services by systemd.
    ///
    /// This key is intentionally binary. It is never parsed as text, put in
    /// configuration, or rendered through `Debug`.
    pub fn cursor_key(&self) -> Result<CursorKey, CredentialError> {
        let bytes = self
            .read_private_credential(CURSOR_KEY_CREDENTIAL, CURSOR_KEY_BYTES)
            .map_err(|error| match error {
                CredentialError::CredentialTooLarge => CredentialError::InvalidCursorKeyLength,
                error => error,
            })?;
        let bytes: [u8; CURSOR_KEY_BYTES] = bytes
            .try_into()
            .map_err(|_| CredentialError::InvalidCursorKeyLength)?;
        Ok(CursorKey::from_bytes(bytes))
    }

    /// Load the PostgreSQL password used only by the TeslaMate history
    /// migrator. A password in configuration, a source URL, or argv is never
    /// accepted.
    pub fn teslamate_postgres_password(
        &self,
    ) -> Result<TeslaMatePostgresPassword, CredentialError> {
        let bytes = self
            .read_private_credential(
                TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL,
                MAX_POSTGRES_PASSWORD_BYTES,
            )
            .map_err(|error| match error {
                CredentialError::CredentialTooLarge => CredentialError::PostgresPasswordTooLarge,
                error => error,
            })?;
        if bytes.is_empty() {
            return Err(CredentialError::EmptyPostgresPassword);
        }
        if bytes.contains(&0) || bytes.contains(&b'\r') || bytes.contains(&b'\n') {
            return Err(CredentialError::InvalidPostgresPasswordBytes);
        }
        let value = String::from_utf8(bytes)
            .map_err(|_| CredentialError::InvalidPostgresPasswordEncoding)?;
        Ok(TeslaMatePostgresPassword(value))
    }

    fn read_private_credential(
        &self,
        credential_name: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, CredentialError> {
        let path = self.path.join(credential_name);
        let entry_metadata =
            fs::symlink_metadata(&path).map_err(|source| CredentialError::Read {
                path: path.clone(),
                source,
            })?;
        if !entry_metadata.file_type().is_file() {
            return Err(CredentialError::NotRegularFile);
        }
        ensure_private_mode(&entry_metadata, &path)?;

        let mut file = fs::File::open(&path).map_err(|source| CredentialError::Read {
            path: path.clone(),
            source,
        })?;
        let opened_metadata = file.metadata().map_err(|source| CredentialError::Read {
            path: path.clone(),
            source,
        })?;
        if !opened_metadata.file_type().is_file() {
            return Err(CredentialError::NotRegularFile);
        }
        ensure_private_mode(&opened_metadata, &path)?;
        ensure_same_file(&entry_metadata, &opened_metadata)?;

        let mut bytes = Vec::with_capacity(max_bytes.saturating_add(1));
        file.by_ref()
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| CredentialError::Read { path, source })?;
        if bytes.len() > max_bytes {
            return Err(CredentialError::CredentialTooLarge);
        }
        Ok(bytes)
    }
}

#[cfg(unix)]
fn ensure_private_mode(metadata: &fs::Metadata, _path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::MetadataExt;

    if metadata.mode() & 0o077 != 0 {
        return Err(CredentialError::InsecurePermissions);
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_mode(_metadata: &fs::Metadata, _path: &Path) -> Result<(), CredentialError> {
    Ok(())
}

#[cfg(unix)]
fn ensure_same_file(
    entry_metadata: &fs::Metadata,
    opened_metadata: &fs::Metadata,
) -> Result<(), CredentialError> {
    use std::os::unix::fs::MetadataExt;

    if entry_metadata.dev() != opened_metadata.dev()
        || entry_metadata.ino() != opened_metadata.ino()
    {
        return Err(CredentialError::CredentialChanged);
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_same_file(
    _entry_metadata: &fs::Metadata,
    _opened_metadata: &fs::Metadata,
) -> Result<(), CredentialError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("CREDENTIALS_DIRECTORY is required for this command")]
    MissingDirectory,
    #[error("CREDENTIALS_DIRECTORY is empty")]
    EmptyDirectory,
    #[error("cannot read service credential: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("service credential is not a regular file")]
    NotRegularFile,
    #[error("service credential permissions are not private")]
    InsecurePermissions,
    #[error("service credential changed while it was opened")]
    CredentialChanged,
    #[error("service credential exceeds its size limit")]
    CredentialTooLarge,
    #[error("owner token is empty")]
    EmptyToken,
    #[error("owner token exceeds the size limit")]
    TokenTooLarge,
    #[error("owner token contains unsupported control bytes")]
    InvalidTokenBytes,
    #[error("owner token is not UTF-8")]
    InvalidTokenEncoding,
    #[error("cursor signing key must be exactly 32 bytes")]
    InvalidCursorKeyLength,
    #[error("TeslaMate PostgreSQL password is empty")]
    EmptyPostgresPassword,
    #[error("TeslaMate PostgreSQL password exceeds the size limit")]
    PostgresPasswordTooLarge,
    #[error("TeslaMate PostgreSQL password contains unsupported control bytes")]
    InvalidPostgresPasswordBytes,
    #[error("TeslaMate PostgreSQL password is not UTF-8")]
    InvalidPostgresPasswordEncoding,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn reads_a_private_regular_owner_token_without_exposing_debug_value() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let token_path = temp.path().join(OWNER_TOKEN_CREDENTIAL);
        fs::write(&token_path, "test-owner-token").expect("token file");
        set_private_mode(&token_path);

        let token = CredentialDirectory::from_path(temp.path())
            .owner_token()
            .expect("valid token");
        assert_eq!(token.as_str(), "test-owner-token");
        assert_eq!(format!("{token:?}"), "OwnerToken([redacted])");
    }

    #[test]
    fn rejects_control_bytes_in_credentials() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let token_path = temp.path().join(OWNER_TOKEN_CREDENTIAL);
        fs::write(&token_path, "test-owner-token\n").expect("token file");
        set_private_mode(&token_path);
        let error = CredentialDirectory::from_path(temp.path())
            .owner_token()
            .expect_err("newline must be rejected");
        assert!(matches!(error, CredentialError::InvalidTokenBytes));
    }

    #[test]
    fn reads_an_exact_binary_cursor_key_without_exposing_debug_value() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let key_path = temp.path().join(CURSOR_KEY_CREDENTIAL);
        fs::write(
            &key_path,
            (0_u8..CURSOR_KEY_BYTES as u8).collect::<Vec<_>>(),
        )
        .expect("cursor key file");
        set_private_mode(&key_path);

        let key = CredentialDirectory::from_path(temp.path())
            .cursor_key()
            .expect("valid binary key");
        assert_eq!(format!("{key:?}"), "CursorKey([REDACTED])");
    }

    #[test]
    fn rejects_cursor_keys_that_are_not_exactly_32_bytes() {
        for length in [31, 33] {
            let temp = tempfile::tempdir().expect("temporary credential directory");
            let key_path = temp.path().join(CURSOR_KEY_CREDENTIAL);
            fs::write(&key_path, vec![7_u8; length]).expect("cursor key file");
            set_private_mode(&key_path);

            let error = CredentialDirectory::from_path(temp.path())
                .cursor_key()
                .expect_err("cursor key must have exact length");
            assert!(matches!(error, CredentialError::InvalidCursorKeyLength));
        }
    }

    #[test]
    fn reads_a_private_postgres_password_without_debug_exposure() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let password_path = temp.path().join(TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL);
        fs::write(&password_path, "test-postgres-password").expect("password file");
        set_private_mode(&password_path);

        let password = CredentialDirectory::from_path(temp.path())
            .teslamate_postgres_password()
            .expect("valid password");
        assert_eq!(password.as_str(), "test-postgres-password");
        assert_eq!(
            format!("{password:?}"),
            "TeslaMatePostgresPassword([redacted])"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_world_readable_credentials() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let token_path = temp.path().join(OWNER_TOKEN_CREDENTIAL);
        fs::write(&token_path, "test-owner-token").expect("token file");
        set_world_readable_mode(&token_path);
        let error = CredentialDirectory::from_path(temp.path())
            .owner_token()
            .expect_err("world-readable credential must be rejected");
        assert!(matches!(error, CredentialError::InsecurePermissions));
    }

    #[cfg(unix)]
    fn set_private_mode(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private mode");
    }

    #[cfg(not(unix))]
    fn set_private_mode(_path: &Path) {}

    #[cfg(unix)]
    fn set_world_readable_mode(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).expect("world-readable mode");
    }
}
