use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::Deserialize;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::credentials::CredentialStore;
use crate::spool::SpoolConfig;

const MAX_CONFIG_BYTES: u64 = 1_048_576;
const MAX_SPOOL_BYTES: u64 = 8 * 1_024 * 1_024 * 1_024;
const MAX_SPOOL_RECORDS: usize = 1_000_000;
const MAX_RETENTION_SECONDS: u64 = 30 * 24 * 60 * 60;
const MAX_BATCH_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_BATCH_RECORDS: usize = 1_024;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeConfig {
    pub version: u16,
    pub state_directory: PathBuf,
    pub receiver_bind: SocketAddr,
    pub hub_bind: SocketAddr,
    pub receiver_bearer_path: PathBuf,
    pub spool_key_path: PathBuf,
    pub credential_store_path: PathBuf,
    pub hub_server_certificate_path: PathBuf,
    pub hub_server_private_key_path: PathBuf,
    pub hub_client_ca_path: PathBuf,
    pub spool: SpoolLimits,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoolLimits {
    pub max_bytes: u64,
    pub max_records: usize,
    pub retention_seconds: u64,
    pub batch_max_bytes: usize,
    pub batch_max_records: usize,
}

impl EdgeConfig {
    pub fn from_toml(bytes: &[u8]) -> Result<Self, ConfigError> {
        if bytes.len() > usize::try_from(MAX_CONFIG_BYTES).unwrap_or(usize::MAX) {
            return Err(ConfigError::InvalidToml);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ConfigError::InvalidToml)?;
        let config: Self = toml::from_str(text).map_err(|_| ConfigError::InvalidToml)?;
        config.validate_shape()?;
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.is_absolute() || path_contains_symlink(path)? {
            return Err(ConfigError::InvalidPath);
        }
        let metadata = fs::metadata(path).map_err(|_| ConfigError::MissingFile)?;
        if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::InvalidPath);
        }
        let bytes = fs::read(path).map_err(|_| ConfigError::Io)?;
        Self::from_toml(&bytes)
    }

    pub fn validate_runtime_files(&self) -> Result<(), ConfigError> {
        for path in [
            &self.receiver_bearer_path,
            &self.spool_key_path,
            &self.credential_store_path,
            &self.hub_server_private_key_path,
        ] {
            validate_regular_file(path, true)?;
        }
        for path in [&self.hub_server_certificate_path, &self.hub_client_ca_path] {
            validate_regular_file(path, false)?;
        }
        Ok(())
    }

    pub fn spool_config(&self) -> Result<SpoolConfig, ConfigError> {
        let retention_ms = self
            .spool
            .retention_seconds
            .checked_mul(1_000)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or(ConfigError::InvalidLimit)?;
        Ok(SpoolConfig {
            directory: self.state_directory.join("spool"),
            max_bytes: self.spool.max_bytes,
            max_records: self.spool.max_records,
            retention_ms,
            batch_max_bytes: self.spool.batch_max_bytes,
            batch_max_records: self.spool.batch_max_records,
        })
    }

    fn validate_shape(&self) -> Result<(), ConfigError> {
        if self.version != 1 {
            return Err(ConfigError::UnsupportedVersion);
        }
        for path in [
            &self.state_directory,
            &self.receiver_bearer_path,
            &self.spool_key_path,
            &self.credential_store_path,
            &self.hub_server_certificate_path,
            &self.hub_server_private_key_path,
            &self.hub_client_ca_path,
        ] {
            if !path.is_absolute() {
                return Err(ConfigError::InvalidPath);
            }
        }
        if !self.receiver_bind.ip().is_loopback()
            || self.hub_bind.ip().is_loopback()
            || self.receiver_bind.port() == 0
            || self.hub_bind.port() == 0
            || self.receiver_bind.port() == self.hub_bind.port()
        {
            return Err(ConfigError::UnsafeBind);
        }
        let limits = &self.spool;
        if !(1_048_576..=MAX_SPOOL_BYTES).contains(&limits.max_bytes)
            || !(1..=MAX_SPOOL_RECORDS).contains(&limits.max_records)
            || !(60..=MAX_RETENTION_SECONDS).contains(&limits.retention_seconds)
            || !(1..=MAX_BATCH_BYTES).contains(&limits.batch_max_bytes)
            || !(1..=MAX_BATCH_RECORDS).contains(&limits.batch_max_records)
            || u64::try_from(limits.batch_max_bytes).unwrap_or(u64::MAX) > limits.max_bytes
            || limits.batch_max_records > limits.max_records
        {
            return Err(ConfigError::InvalidLimit);
        }
        Ok(())
    }
}

pub fn initialize(config: &EdgeConfig) -> Result<(), ConfigError> {
    config.validate_shape()?;
    for path in [
        &config.receiver_bearer_path,
        &config.spool_key_path,
        &config.credential_store_path,
    ] {
        if path.exists() || path_contains_symlink(path)? {
            return Err(ConfigError::AlreadyInitialized);
        }
    }
    create_private_directory(&config.state_directory)?;
    for path in [
        &config.receiver_bearer_path,
        &config.spool_key_path,
        &config.credential_store_path,
    ] {
        let parent = path.parent().ok_or(ConfigError::InvalidPath)?;
        if !parent.exists() {
            create_private_directory(parent)?;
        }
    }

    let mut receiver_secret = [0_u8; 32];
    rand::rng().fill_bytes(&mut receiver_secret);
    let receiver_token = URL_SAFE_NO_PAD.encode(receiver_secret);
    receiver_secret.zeroize();
    write_new_private(&config.receiver_bearer_path, receiver_token.as_bytes())?;

    let mut spool_key = [0_u8; 32];
    rand::rng().fill_bytes(&mut spool_key);
    if let Err(error) = write_new_private(&config.spool_key_path, &spool_key) {
        spool_key.zeroize();
        return Err(error);
    }
    spool_key.zeroize();
    CredentialStore::open(&config.credential_store_path).map_err(|_| ConfigError::Io)?;
    Ok(())
}

pub fn rotate_receiver_token(config: &EdgeConfig) -> Result<IssuedReceiverToken, ConfigError> {
    let destination = &config.receiver_bearer_path;
    if path_contains_symlink(destination)? || !destination.is_file() {
        return Err(ConfigError::MissingFile);
    }
    let parent = destination.parent().ok_or(ConfigError::InvalidPath)?;
    let temporary = parent.join(format!(".receiver-token.{}.tmp", Uuid::new_v4()));
    let mut secret = [0_u8; 32];
    rand::rng().fill_bytes(&mut secret);
    let token = URL_SAFE_NO_PAD.encode(secret);
    secret.zeroize();
    write_new_private(&temporary, token.as_bytes())?;
    if let Err(_error) = fs::rename(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        return Err(ConfigError::Io);
    }
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ConfigError::Io)?;
    Ok(IssuedReceiverToken { token })
}

#[derive(Debug, Zeroize, ZeroizeOnDrop)]
pub struct IssuedReceiverToken {
    token: String,
}

impl IssuedReceiverToken {
    pub fn token(&self) -> &str {
        &self.token
    }
}

fn create_private_directory(path: &Path) -> Result<(), ConfigError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ConfigError::InvalidPath);
            }
            #[cfg(unix)]
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(ConfigError::UnsafePermissions);
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ConfigError::Io),
    }
    fs::create_dir_all(path).map_err(|_| ConfigError::Io)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| ConfigError::Io)?;
    Ok(())
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut output = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            ConfigError::AlreadyInitialized
        } else {
            ConfigError::Io
        }
    })?;
    output.write_all(bytes).map_err(|_| ConfigError::Io)?;
    output.sync_all().map_err(|_| ConfigError::Io)?;
    drop(output);
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ConfigError::Io)?;
    }
    Ok(())
}

fn validate_regular_file(path: &Path, private: bool) -> Result<(), ConfigError> {
    if path_contains_symlink(path)? {
        return Err(ConfigError::InvalidPath);
    }
    let metadata = fs::metadata(path).map_err(|_| ConfigError::MissingFile)?;
    if !metadata.is_file() {
        return Err(ConfigError::InvalidPath);
    }
    #[cfg(unix)]
    if private && metadata.permissions().mode() & 0o077 != 0 {
        return Err(ConfigError::UnsafePermissions);
    }
    Ok(())
}

fn path_contains_symlink(path: &Path) -> Result<bool, ConfigError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(ConfigError::Io),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error("configuration TOML is invalid")]
    InvalidToml,
    #[error("configuration version is unsupported")]
    UnsupportedVersion,
    #[error("configuration path is unsafe")]
    InvalidPath,
    #[error("configuration bind address is unsafe")]
    UnsafeBind,
    #[error("configuration limit is invalid")]
    InvalidLimit,
    #[error("required runtime file is missing")]
    MissingFile,
    #[error("secret file permissions are unsafe")]
    UnsafePermissions,
    #[error("Edge state is already initialized")]
    AlreadyInitialized,
    #[error("configuration input or output failed")]
    Io,
}
