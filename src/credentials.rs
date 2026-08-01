//! Runtime-only access to systemd credentials.
//!
//! A Hub owner token is decrypted by systemd for the service, then exposed as
//! a short-lived regular file below `CREDENTIALS_DIRECTORY`. This module never
//! reads a token from configuration, argv, an environment value, or the Hub
//! database. It also deliberately does not log token parse errors with content.

use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::SystemTime,
};

use crate::{
    legacy_auth::{LegacyAuth, LegacyAuthError},
    legacy_token_state::{LegacyTokenState, LegacyTokenStateError},
};
use crate::protocol::CursorKey;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

pub const OWNER_TOKEN_CREDENTIAL: &str = "owner-token";
pub const TESLAMATE_OWNER_TOKENS_CREDENTIAL: &str = "teslamate-owner-tokens";
pub const TESLAMATE_OWNER_TOKENS_PREVIOUS_CREDENTIAL: &str = "teslamate-owner-tokens-previous";
pub const TESLAMATE_ENCRYPTION_KEY_CREDENTIAL: &str = "teslamate-encryption-key";
pub const CURSOR_KEY_CREDENTIAL: &str = "cursor-key";
pub const TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL: &str = "teslamate-postgres-password";
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_OWNER_TOKENS_BYTES: usize = (MAX_TOKEN_BYTES * 2) + 256;
const CURSOR_KEY_BYTES: usize = 32;
const MAX_POSTGRES_PASSWORD_BYTES: usize = 4 * 1024;
const MAX_TESLAMATE_ENCRYPTION_KEY_BYTES: usize = 4 * 1024;
const MAX_MQTT_CREDENTIAL_BYTES: usize = 4 * 1024;
#[cfg(target_os = "macos")]
const MAC_KEYCHAIN_HELPER_ENV: &str = "TESLATLAS_HUB_MAC_KEYCHAIN_HELPER";
#[cfg(target_os = "macos")]
const MAC_OWNER_SERVICE_ENV: &str = "TESLATLAS_HUB_MAC_OWNER_SERVICE";
#[cfg(target_os = "macos")]
const MAC_ACCOUNT_ENV: &str = "TESLATLAS_HUB_MAC_ACCOUNT";

#[derive(Clone, PartialEq, Eq)]
pub struct OwnerToken(String);

impl OwnerToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A TeslaMate legacy credential pair transferred only through one encrypted
/// systemd credential. The access token can serve existing read-only Owner
/// API requests; the refresh token remains available for a separately
/// specified refresh protocol and is never rendered or persisted by Hub.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnerTokens {
    access_token: String,
    refresh_token: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct MqttCredential(Zeroizing<String>);

impl MqttCredential {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Debug for MqttCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MqttCredential([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MqttCredentials {
    pub(crate) username: Option<MqttCredential>,
    pub(crate) password: Option<MqttCredential>,
}

impl MqttCredentials {
    pub fn username(&self) -> Option<&str> {
        self.username.as_ref().map(MqttCredential::as_str)
    }

    pub fn password(&self) -> Option<&str> {
        self.password.as_ref().map(MqttCredential::as_str)
    }

    #[cfg(test)]
    pub(crate) fn for_test(username: Option<&str>, password: Option<&str>) -> Self {
        Self {
            username: username.map(|value| MqttCredential(Zeroizing::new(value.to_owned()))),
            password: password.map(|value| MqttCredential(Zeroizing::new(value.to_owned()))),
        }
    }
}

impl std::fmt::Debug for MqttCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MqttCredentials")
            .field("username", &self.username.as_ref().map(|_| "[redacted]"))
            .field("password", &self.password.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl OwnerTokens {
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    pub(crate) fn from_secret_parts(
        access_token: String,
        refresh_token: String,
    ) -> Result<Self, CredentialError> {
        validate_token_component(&access_token)?;
        validate_token_component(&refresh_token)?;
        Ok(Self {
            access_token,
            refresh_token,
        })
    }

    pub fn credential_json(&self) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
        let wire = OwnerTokensWire {
            version: 1,
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
        };
        serde_json::to_vec(&wire)
            .map(Zeroizing::new)
            .map_err(|_| CredentialError::EncodeOwnerTokens)
    }

    pub fn from_credential_json(bytes: &[u8]) -> Result<Self, CredentialError> {
        if bytes.is_empty() || bytes.len() > MAX_OWNER_TOKENS_BYTES {
            return Err(CredentialError::OwnerTokensTooLarge);
        }
        let wire: OwnerTokensWire =
            serde_json::from_slice(bytes).map_err(|_| CredentialError::InvalidOwnerTokens)?;
        if wire.version != 1 {
            return Err(CredentialError::UnsupportedOwnerTokensVersion);
        }
        Self::from_secret_parts(wire.access_token, wire.refresh_token)
    }
}

#[derive(Debug, Error)]
pub enum LegacyAuthManagerError {
    #[error("legacy credential error: {0}")]
    Credential(#[from] CredentialError),
    #[error("legacy auth error: {0}")]
    Auth(#[from] LegacyAuthError),
}

pub struct LegacyAuthManager {
    auth: LegacyAuth,
    persistence: LegacyAuthPersistence,
    state_generation: Option<uuid::Uuid>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyAuthStateWire {
    version: u8,
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    next_refresh_at: i64,
}

enum LegacyAuthPersistence {
    #[cfg(any(target_os = "macos", test))]
    Callback(Arc<dyn Fn(&str, &str, i64, i64) -> Result<(), CredentialError> + Send + Sync>),
    #[cfg(target_os = "linux")]
    LinuxState(Arc<LegacyTokenState>),
}

impl std::fmt::Debug for LegacyAuthManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.auth.fmt(formatter)
    }
}

impl LegacyAuthManager {
    pub fn from_directory(directory: CredentialDirectory) -> Result<Self, LegacyAuthManagerError> {
        #[cfg(target_os = "linux")]
        {
            let state = Arc::new(directory.legacy_token_state()?);
            let mut lock = state.lock().map_err(CredentialError::from)?;
            if let Some(loaded) = lock.load().map_err(CredentialError::from)? {
                let auth = legacy_auth_from_state(&loaded.payload)?;
                return Ok(Self {
                    auth,
                    persistence: LegacyAuthPersistence::LinuxState(state),
                    state_generation: Some(loaded.generation),
                });
            }
            let tokens = directory.teslamate_owner_tokens()?;
            let auth = LegacyAuth::from_access_token(tokens.access_token(), tokens.refresh_token())
                .map_err(LegacyAuthManagerError::Auth)?;
            return Ok(Self {
                auth,
                persistence: LegacyAuthPersistence::LinuxState(state),
                state_generation: None,
            });
        }

        #[cfg(target_os = "macos")]
        {
            let mac_keychain = MacKeychainConfig::from_environment()?;
            if let Some(payload) = load_mac_legacy_auth_state(&mac_keychain)? {
                let auth = legacy_auth_from_state(&payload)?;
                return Ok(Self {
                    auth,
                    persistence: LegacyAuthPersistence::Callback(mac_legacy_persistence_sink(mac_keychain)),
                    state_generation: None,
                });
            }
            let tokens = directory.teslamate_owner_tokens()?;
            let auth = LegacyAuth::from_access_token(tokens.access_token(), tokens.refresh_token())
                .map_err(LegacyAuthManagerError::Auth)?;
            return Ok(Self {
                auth,
                persistence: LegacyAuthPersistence::Callback(mac_legacy_persistence_sink(mac_keychain)),
                state_generation: None,
            });
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Err(CredentialError::LegacyTokenStateUnavailable.into())
    }

    pub fn access_token(&self) -> &str {
        self.auth.access_token()
    }

    pub fn refresh_token(&self) -> &str {
        self.auth.refresh_token()
    }

    pub fn next_refresh_at(&self) -> i64 {
        self.auth.next_refresh_at()
    }

    pub fn region(&self) -> crate::tesla_stream::StreamRegion {
        self.auth.region()
    }

    pub async fn refresh_if_due(
        &mut self,
        client: &Client,
        now: SystemTime,
    ) -> Result<(), LegacyAuthManagerError> {
        self.refresh_with_persistence(client, now, false).await
    }

    pub async fn refresh_now(
        &mut self,
        client: &Client,
        now: SystemTime,
    ) -> Result<(), LegacyAuthManagerError> {
        self.refresh_with_persistence(client, now, true).await
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        auth: LegacyAuth,
        persist: Arc<dyn Fn(&str, &str) -> Result<(), CredentialError> + Send + Sync>,
    ) -> Self {
        Self {
            auth,
            persistence: LegacyAuthPersistence::Callback(Arc::new(
                move |access, refresh, _, _| persist(access, refresh),
            )),
            state_generation: None,
        }
    }

    async fn refresh_with_persistence(
        &mut self,
        client: &Client,
        now: SystemTime,
        force: bool,
    ) -> Result<(), LegacyAuthManagerError> {
        match &self.persistence {
            #[cfg(any(target_os = "macos", test))]
            LegacyAuthPersistence::Callback(persist) => {
                let persist = Arc::clone(persist);
                let result = if force {
                    self.auth
                        .refresh_now_persisted(client, now, move |access, refresh, expires_at, next_refresh_at| {
                            persist(access, refresh, expires_at, next_refresh_at)
                                .map_err(|_| LegacyAuthError::Persistence)
                        })
                        .await
                } else {
                    self.auth
                        .refresh_if_due_persisted(client, now, move |access, refresh, expires_at, next_refresh_at| {
                            persist(access, refresh, expires_at, next_refresh_at)
                                .map_err(|_| LegacyAuthError::Persistence)
                        })
                        .await
                };
                result.map_err(LegacyAuthManagerError::Auth)
            }
            #[cfg(target_os = "linux")]
            LegacyAuthPersistence::LinuxState(state) => {
                let mut lock = state.lock().map_err(CredentialError::from)?;
                if let Some(loaded) = lock.load().map_err(CredentialError::from)?
                    && Some(loaded.generation) != self.state_generation
                {
                    self.auth = legacy_auth_from_state(&loaded.payload)?;
                    self.state_generation = Some(loaded.generation);
                }
                let result = if force {
                    self.auth.refresh_now_persisted(client, now, |access, refresh, expires_at, next_refresh_at| {
                        let payload = encode_legacy_auth_state(access, refresh, expires_at, next_refresh_at)
                            .map_err(|_| LegacyAuthError::Persistence)?;
                        lock.persist(&payload)
                            .map(|_| ())
                            .map_err(|_| LegacyAuthError::Persistence)
                    }).await
                } else {
                    self.auth.refresh_if_due_persisted(client, now, |access, refresh, expires_at, next_refresh_at| {
                        let payload = encode_legacy_auth_state(access, refresh, expires_at, next_refresh_at)
                            .map_err(|_| LegacyAuthError::Persistence)?;
                        lock.persist(&payload)
                            .map(|_| ())
                            .map_err(|_| LegacyAuthError::Persistence)
                    }).await
                };
                if result.is_ok() {
                    self.state_generation = lock
                        .load()
                        .map_err(CredentialError::from)?
                        .map(|loaded| loaded.generation);
                }
                result.map_err(LegacyAuthManagerError::Auth)
            }
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn mac_legacy_persistence_sink(
    config: MacKeychainConfig,
) -> Arc<dyn Fn(&str, &str, i64, i64) -> Result<(), CredentialError> + Send + Sync> {
    Arc::new(move |access, refresh, expires_at, next_refresh_at| {
        persist_mac_legacy_auth_state(&config, access, refresh, expires_at, next_refresh_at)
    })
}

#[cfg(any(target_os = "macos", test))]
struct MacKeychainConfig {
    helper: PathBuf,
    service: String,
    account: String,
}

#[cfg(any(target_os = "macos", test))]
impl MacKeychainConfig {
    #[cfg(target_os = "macos")]
    fn from_environment() -> Result<Self, CredentialError> {
        let helper = env::var_os(MAC_KEYCHAIN_HELPER_ENV);
        let service = env::var(MAC_OWNER_SERVICE_ENV).ok();
        let account = env::var(MAC_ACCOUNT_ENV).ok();
        let (Some(helper), Some(service), Some(account)) = (helper, service, account) else {
            return Err(CredentialError::MacKeychainConfigurationMissing);
        };
        Self::from_parts(PathBuf::from(helper), service, account)
    }

    fn from_parts(
        helper: PathBuf,
        service: String,
        account: String,
    ) -> Result<Self, CredentialError> {
        if !helper.is_absolute() {
            return Err(CredentialError::MacKeychainHelperInvalid);
        }
        let metadata = fs::symlink_metadata(&helper)
            .map_err(|_| CredentialError::MacKeychainHelperInvalid)?;
        if !metadata.file_type().is_file() {
            return Err(CredentialError::MacKeychainHelperInvalid);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode();
            if mode & 0o111 == 0 || mode & 0o022 != 0 {
                return Err(CredentialError::MacKeychainHelperInvalid);
            }
        }
        validate_mac_keychain_argument(&service, CredentialError::MacKeychainServiceInvalid)?;
        validate_mac_keychain_argument(&account, CredentialError::MacKeychainAccountInvalid)?;
        Ok(Self {
            helper,
            service,
            account,
        })
    }
}

#[cfg(any(target_os = "macos", test))]
fn validate_mac_keychain_argument(
    value: &str,
    error: CredentialError,
) -> Result<(), CredentialError> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(|character| character.is_control())
    {
        return Err(error);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn persist_mac_teslamate_owner_tokens(
    config: &MacKeychainConfig,
    access_token: &str,
    refresh_token: &str,
) -> Result<(), CredentialError> {
    let tokens = OwnerTokens::from_secret_parts(access_token.to_owned(), refresh_token.to_owned())?;
    let payload = tokens.credential_json()?;
    let mut child = Command::new(&config.helper)
        .env_clear()
        .arg("set")
        .arg(&config.service)
        .arg(&config.account)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    child
        .stdin
        .take()
        .ok_or(CredentialError::MacKeychainHelperFailed)?
        .write_all(&payload)
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    let status = child
        .wait()
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    if !status.success() {
        return Err(CredentialError::MacKeychainHelperFailed);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn persist_mac_legacy_auth_state(
    config: &MacKeychainConfig,
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
    next_refresh_at: i64,
) -> Result<(), CredentialError> {
    let payload = encode_legacy_auth_state(access_token, refresh_token, expires_at, next_refresh_at)?;
    let mut child = Command::new(&config.helper)
        .env_clear()
        .arg("set")
        .arg(&config.service)
        .arg(&config.account)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    child
        .stdin
        .take()
        .ok_or(CredentialError::MacKeychainHelperFailed)?
        .write_all(&payload)
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    let status = child
        .wait()
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    if !status.success() {
        return Err(CredentialError::MacKeychainHelperFailed);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn load_mac_legacy_auth_state(
    config: &MacKeychainConfig,
) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
    let exists = Command::new(&config.helper)
        .env_clear()
        .arg("exists")
        .arg(&config.service)
        .arg(&config.account)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    match exists.code() {
        Some(0) => {}
        Some(1) => return Ok(None),
        _ => return Err(CredentialError::MacKeychainHelperFailed),
    }

    let mut child = Command::new(&config.helper)
        .env_clear()
        .arg("get")
        .arg(&config.service)
        .arg(&config.account)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    let mut payload = Zeroizing::new(Vec::with_capacity(MAX_OWNER_TOKENS_BYTES + 1));
    child
        .stdout
        .take()
        .ok_or(CredentialError::MacKeychainHelperFailed)?
        .take((MAX_OWNER_TOKENS_BYTES + 1) as u64)
        .read_to_end(&mut payload)
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    let status = child
        .wait()
        .map_err(|_| CredentialError::MacKeychainHelperFailed)?;
    if !status.success() || payload.is_empty() || payload.len() > MAX_OWNER_TOKENS_BYTES {
        return Err(CredentialError::MacKeychainHelperFailed);
    }
    Ok(Some(payload))
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

/// Exact source encryption-key bytes used only to authenticate and decrypt
/// TeslaMate's legacy token envelope. It is never copied to Hub storage,
/// configuration, argv, or a report.
pub struct TeslaMateEncryptionKey(Zeroizing<Vec<u8>>);

impl TeslaMateEncryptionKey {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl std::fmt::Debug for TeslaMatePostgresPassword {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TeslaMatePostgresPassword([redacted])")
    }
}

impl std::fmt::Debug for TeslaMateEncryptionKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TeslaMateEncryptionKey([redacted])")
    }
}

impl std::fmt::Debug for OwnerToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OwnerToken([redacted])")
    }
}

impl std::fmt::Debug for OwnerTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OwnerTokens([redacted])")
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

    /// Load the complete TeslaMate legacy token pair. This accepts only the
    /// versioned, exact JSON envelope so an access token can never silently be
    /// mistaken for a refreshable pair.
    pub fn teslamate_owner_tokens(&self) -> Result<OwnerTokens, CredentialError> {
        self.read_teslamate_owner_tokens_named(TESLAMATE_OWNER_TOKENS_CREDENTIAL)
    }

    pub fn mqtt_credentials(
        &self,
        username_name: Option<&str>,
        password_name: Option<&str>,
    ) -> Result<MqttCredentials, CredentialError> {
        Ok(MqttCredentials {
            username: self.read_mqtt_credential(username_name)?,
            password: self.read_mqtt_credential(password_name)?,
        })
    }

    fn read_mqtt_credential(
        &self,
        name: Option<&str>,
    ) -> Result<Option<MqttCredential>, CredentialError> {
        let Some(name) = name else { return Ok(None) };
        let bytes = self
            .read_private_credential(name, MAX_MQTT_CREDENTIAL_BYTES)
            .map_err(|error| match error {
                CredentialError::CredentialTooLarge => CredentialError::MqttCredentialTooLarge,
                error => error,
            })?;
        if bytes.is_empty() {
            return Err(CredentialError::EmptyMqttCredential);
        }
        if bytes.iter().any(|byte| byte == &0 || byte.is_ascii_control()) {
            return Err(CredentialError::InvalidMqttCredentialBytes);
        }
        let value = String::from_utf8(bytes)
            .map_err(|_| CredentialError::InvalidMqttCredentialEncoding)?;
        Ok(Some(MqttCredential(Zeroizing::new(value))))
    }

    fn read_teslamate_owner_tokens_named(
        &self,
        credential_name: &str,
    ) -> Result<OwnerTokens, CredentialError> {
        let bytes = self
            .read_private_credential(credential_name, MAX_OWNER_TOKENS_BYTES)
            .map_err(|error| match error {
                CredentialError::CredentialTooLarge => CredentialError::OwnerTokensTooLarge,
                error => error,
            })?;
        OwnerTokens::from_credential_json(&bytes)
    }

    /// Prefer the complete TeslaMate token pair when installed, while keeping
    /// existing single-token Hub installations compatible. A malformed pair
    /// is never silently bypassed.
    pub fn owner_token_for_collection(&self) -> Result<OwnerToken, CredentialError> {
        match self.teslamate_owner_tokens() {
            Ok(tokens) => Ok(OwnerToken(tokens.access_token)),
            Err(CredentialError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                self.owner_token()
            }
            Err(error) => Err(error),
        }
    }

    pub fn replace_encrypted_generation(
        current: &Path,
        ciphertext: &[u8],
    ) -> Result<(), CredentialError> {
        if ciphertext.is_empty() {
            return Err(CredentialError::CredentialTooLarge);
        }
        let parent = current.parent().ok_or_else(|| CredentialError::Read {
            path: current.to_path_buf(),
            source: std::io::Error::other("credential path has no parent"),
        })?;
        fs::create_dir_all(parent).map_err(|source| CredentialError::Read {
            path: parent.to_path_buf(),
            source,
        })?;
        #[cfg(all(unix, not(test)))]
        {
            use std::os::unix::fs::MetadataExt;
            if fs::metadata(parent)
                .map_err(|source| CredentialError::Read {
                    path: parent.to_path_buf(),
                    source,
                })?
                .uid()
                != 0
            {
                return Err(CredentialError::InsecurePermissions);
            }
        }
        let name = current.file_name().unwrap().to_string_lossy();
        let candidate = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
        let previous = parent.join(format!("{name}-previous"));
        let result = (|| -> Result<(), CredentialError> {
            #[cfg(unix)]
            use std::os::unix::fs::OpenOptionsExt;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
                .map_err(|source| CredentialError::Read {
                    path: candidate.clone(),
                    source,
                })?;
            output
                .write_all(ciphertext)
                .map_err(|source| CredentialError::Read {
                    path: candidate.clone(),
                    source,
                })?;
            output.sync_all().map_err(|source| CredentialError::Read {
                path: candidate.clone(),
                source,
            })?;
            drop(output);
            if current.exists() {
                let previous_tmp =
                    parent.join(format!(".{name}-previous.{}.tmp", uuid::Uuid::new_v4()));
                fs::copy(current, &previous_tmp).map_err(|source| CredentialError::Read {
                    path: previous_tmp.clone(),
                    source,
                })?;
                let previous_file = fs::OpenOptions::new()
                    .write(true)
                    .open(&previous_tmp)
                    .map_err(|source| CredentialError::Read {
                        path: previous_tmp.clone(),
                        source,
                    })?;
                previous_file
                    .sync_all()
                    .map_err(|source| CredentialError::Read {
                        path: previous_tmp.clone(),
                        source,
                    })?;
                drop(previous_file);
                fs::rename(&previous_tmp, &previous).map_err(|source| CredentialError::Read {
                    path: previous.clone(),
                    source,
                })?;
            }
            fs::rename(&candidate, current).map_err(|source| CredentialError::Read {
                path: current.to_path_buf(),
                source,
            })?;
            sync_directory(parent)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&candidate);
        }
        result
    }

    /// Restore rollback ciphertext under the current filename without
    /// re-encrypting it, preserving its embedded systemd credential name.
    pub fn restore_previous_generation(current: &Path) -> Result<(), CredentialError> {
        let parent = current.parent().ok_or_else(|| CredentialError::Read {
            path: current.to_path_buf(),
            source: std::io::Error::other("credential path has no parent"),
        })?;
        let name = current.file_name().unwrap().to_string_lossy();
        let previous = parent.join(format!("{name}-previous"));
        let candidate = parent.join(format!(".{name}.rollback.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<(), CredentialError> {
            #[cfg(unix)]
            use std::os::unix::fs::OpenOptionsExt;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
                .map_err(|source| CredentialError::Read {
                    path: candidate.clone(),
                    source,
                })?;
            let mut input = fs::File::open(&previous).map_err(|source| CredentialError::Read {
                path: previous.clone(),
                source,
            })?;
            std::io::copy(&mut input, &mut output).map_err(|source| CredentialError::Read {
                path: candidate.clone(),
                source,
            })?;
            output.sync_all().map_err(|source| CredentialError::Read {
                path: candidate.clone(),
                source,
            })?;
            drop(output);
            fs::rename(&candidate, current).map_err(|source| CredentialError::Read {
                path: current.to_path_buf(),
                source,
            })?;
            sync_directory(parent)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&candidate);
        }
        result
    }

    pub fn persist_host_teslamate_owner_tokens(
        access_token: &str,
        refresh_token: &str,
    ) -> Result<(), CredentialError> {
        persist_host_teslamate_owner_tokens_at(
            Path::new("/etc/teslatlas/credentials/teslamate-owner-tokens"),
            access_token,
            refresh_token,
        )
    }

    pub fn persist_linux_teslamate_owner_tokens(
        &self,
        tokens: &OwnerTokens,
    ) -> Result<(), CredentialError> {
        #[cfg(target_os = "linux")]
        {
            let state = self.legacy_token_state()?;
            let mut lock = state.lock()?;
            let payload = encode_legacy_auth_state(
                tokens.access_token(),
                tokens.refresh_token(),
                0,
                0,
            )?;
            lock.persist(&payload)?;
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = tokens;
            Err(CredentialError::LegacyTokenStateUnavailable)
        }
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

    /// Load the exact TeslaMate `ENCRYPTION_KEY` bytes from a dedicated,
    /// host-encrypted systemd credential. This is deliberately distinct from
    /// the database password and owner token pair.
    pub fn teslamate_encryption_key(&self) -> Result<TeslaMateEncryptionKey, CredentialError> {
        let bytes = self
            .read_private_credential(
                TESLAMATE_ENCRYPTION_KEY_CREDENTIAL,
                MAX_TESLAMATE_ENCRYPTION_KEY_BYTES,
            )
            .map_err(|error| match error {
                CredentialError::CredentialTooLarge => CredentialError::EncryptionKeyTooLarge,
                error => error,
            })?;
        if bytes.is_empty() {
            return Err(CredentialError::EmptyEncryptionKey);
        }
        if bytes.contains(&0) || bytes.contains(&b'\r') || bytes.contains(&b'\n') {
            return Err(CredentialError::InvalidEncryptionKeyBytes);
        }
        Ok(TeslaMateEncryptionKey(Zeroizing::new(bytes)))
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
        Read::by_ref(&mut file)
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| CredentialError::Read { path, source })?;
        if bytes.len() > max_bytes {
            return Err(CredentialError::CredentialTooLarge);
        }
        Ok(bytes)
    }

    fn legacy_token_state(&self) -> Result<LegacyTokenState, CredentialError> {
        let bytes = self
            .read_private_credential(CURSOR_KEY_CREDENTIAL, CURSOR_KEY_BYTES)
            .map_err(|error| match error {
                CredentialError::CredentialTooLarge => CredentialError::InvalidCursorKeyLength,
                error => error,
            })?;
        let bytes: [u8; CURSOR_KEY_BYTES] = bytes
            .try_into()
            .map_err(|_| CredentialError::InvalidCursorKeyLength)?;
        Ok(LegacyTokenState::new(bytes))
    }
}

fn persist_host_teslamate_owner_tokens_at(
    destination: &Path,
    access_token: &str,
    refresh_token: &str,
) -> Result<(), CredentialError> {
    let tokens = OwnerTokens::from_secret_parts(access_token.to_owned(), refresh_token.to_owned())?;
    let payload = tokens.credential_json()?;
    let mut child = Command::new("systemd-creds")
        .args([
            "encrypt",
            "--with-key=host",
            "--name=teslamate-owner-tokens",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CredentialError::CredentialEncryptionFailed)?;
    child
        .stdin
        .take()
        .ok_or(CredentialError::CredentialEncryptionFailed)?
        .write_all(&payload)
        .map_err(|_| CredentialError::CredentialEncryptionFailed)?;
    let output = child
        .wait_with_output()
        .map_err(|_| CredentialError::CredentialEncryptionFailed)?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(CredentialError::CredentialEncryptionFailed);
    }
    CredentialDirectory::replace_encrypted_generation(destination, &output.stdout)
}

fn sync_directory(path: &Path) -> Result<(), CredentialError> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| CredentialError::Read {
            path: path.to_path_buf(),
            source,
        })
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OwnerTokensWire {
    version: u8,
    access_token: String,
    refresh_token: String,
}

fn encode_legacy_auth_state(
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
    next_refresh_at: i64,
) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
    OwnerTokens::from_secret_parts(access_token.to_owned(), refresh_token.to_owned())?;
    serde_json::to_vec(&LegacyAuthStateWire {
        version: 1,
        access_token: access_token.to_owned(),
        refresh_token: refresh_token.to_owned(),
        expires_at,
        next_refresh_at,
    })
    .map(Zeroizing::new)
    .map_err(|_| CredentialError::EncodeOwnerTokens)
}

fn legacy_auth_from_state(payload: &[u8]) -> Result<LegacyAuth, LegacyAuthManagerError> {
    match serde_json::from_slice::<LegacyAuthStateWire>(payload) {
        Ok(wire) => {
            if wire.version != 1 {
                return Err(CredentialError::UnsupportedOwnerTokensVersion.into());
            }
            LegacyAuth::from_persisted_state(
                wire.access_token,
                wire.refresh_token,
                wire.expires_at,
                wire.next_refresh_at,
            )
            .map_err(LegacyAuthManagerError::Auth)
        }
        Err(_) => {
            let tokens = OwnerTokens::from_credential_json(payload)?;
            LegacyAuth::from_access_token(tokens.access_token(), tokens.refresh_token())
                .map_err(LegacyAuthManagerError::Auth)
        }
    }
}

fn validate_token_component(value: &str) -> Result<(), CredentialError> {
    if value.is_empty() {
        return Err(CredentialError::EmptyOwnerTokens);
    }
    if value.len() > MAX_TOKEN_BYTES {
        return Err(CredentialError::OwnerTokensTooLarge);
    }
    if value.chars().any(char::is_control) {
        return Err(CredentialError::InvalidOwnerTokens);
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_private_mode(metadata: &fs::Metadata, _path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::MetadataExt;

    // systemd exposes credentials as root-owned 0440 files inside a
    // service-private, read-only mount. Permit that group-read bit while
    // rejecting group writes and every form of access for other users.
    if metadata.mode() & 0o027 != 0 {
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
    #[error("TeslaMate owner token pair is empty")]
    EmptyOwnerTokens,
    #[error("TeslaMate owner token pair exceeds the size limit")]
    OwnerTokensTooLarge,
    #[error("TeslaMate owner token pair is not a supported private credential")]
    InvalidOwnerTokens,
    #[error("TeslaMate owner token pair version is unsupported")]
    UnsupportedOwnerTokensVersion,
    #[error("TeslaMate owner token pair could not be encoded")]
    EncodeOwnerTokens,
    #[error("TeslaMate owner token pair could not be encrypted")]
    CredentialEncryptionFailed,
    #[error("Hub-owned legacy token state is unavailable on this platform")]
    LegacyTokenStateUnavailable,
    #[error("Hub-owned legacy token state could not be read or persisted")]
    LegacyTokenStateWrite,
    #[error("Hub-owned legacy token state is invalid")]
    LegacyTokenState(#[from] LegacyTokenStateError),
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
    #[error("TeslaMate encryption key is empty")]
    EmptyEncryptionKey,
    #[error("TeslaMate encryption key exceeds the size limit")]
    EncryptionKeyTooLarge,
    #[error("TeslaMate encryption key contains unsupported control bytes")]
    InvalidEncryptionKeyBytes,
    #[error("MQTT credential is empty")]
    EmptyMqttCredential,
    #[error("MQTT credential exceeds the size limit")]
    MqttCredentialTooLarge,
    #[error("MQTT credential contains unsupported control bytes")]
    InvalidMqttCredentialBytes,
    #[error("MQTT credential is not UTF-8")]
    InvalidMqttCredentialEncoding,
    #[cfg(any(target_os = "macos", test))]
    #[error("macOS keychain persistence configuration is incomplete")]
    MacKeychainConfigurationMissing,
    #[cfg(any(target_os = "macos", test))]
    #[error("macOS keychain helper is invalid")]
    MacKeychainHelperInvalid,
    #[cfg(any(target_os = "macos", test))]
    #[error("macOS keychain service is invalid")]
    MacKeychainServiceInvalid,
    #[cfg(any(target_os = "macos", test))]
    #[error("macOS keychain account is invalid")]
    MacKeychainAccountInvalid,
    #[cfg(any(target_os = "macos", test))]
    #[error("macOS keychain helper failed")]
    MacKeychainHelperFailed,
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
    #[test]
    fn reads_systemd_style_group_readable_credentials() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary credential directory");
        let key_path = temp.path().join(CURSOR_KEY_CREDENTIAL);
        fs::write(&key_path, vec![7_u8; CURSOR_KEY_BYTES]).expect("cursor key file");
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o440))
            .expect("systemd credential mode");

        CredentialDirectory::from_path(temp.path())
            .cursor_key()
            .expect("systemd credential mode must be accepted");
    }

    fn pair_json(access: &str, refresh: &str) -> Vec<u8> {
        serde_json::json!({
            "version": 1,
            "access_token": access,
            "refresh_token": refresh,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn rejects_invalid_candidate_without_touching_current_generation() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let current = temp.path().join(TESLAMATE_OWNER_TOKENS_CREDENTIAL);
        fs::write(&current, b"old-encrypted").expect("current ciphertext");
        set_private_mode(&current);
        assert!(matches!(
            OwnerTokens::from_credential_json(br#"{"version":1,"access_token":""}"#),
            Err(CredentialError::InvalidOwnerTokens | CredentialError::OwnerTokensTooLarge)
        ));
        assert_eq!(fs::read(&current).unwrap(), b"old-encrypted");
    }

    #[test]
    fn atomically_replaces_current_and_keeps_previous_ciphertext() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let current = temp.path().join(TESLAMATE_OWNER_TOKENS_CREDENTIAL);
        fs::write(&current, b"old-encrypted").expect("current ciphertext");
        set_private_mode(&current);
        CredentialDirectory::replace_encrypted_generation(&current, b"new-encrypted")
            .expect("atomic replacement");
        assert_eq!(fs::read(&current).unwrap(), b"new-encrypted");
        assert_eq!(
            fs::read(temp.path().join(TESLAMATE_OWNER_TOKENS_PREVIOUS_CREDENTIAL)).unwrap(),
            b"old-encrypted"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&current).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failed_pre_rename_keeps_current_and_leaves_no_candidate() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let current = temp.path().join(TESLAMATE_OWNER_TOKENS_CREDENTIAL);
        fs::create_dir(&current).expect("blocking directory");
        let error = CredentialDirectory::replace_encrypted_generation(&current, b"candidate")
            .expect_err("directory must block replacement");
        assert!(matches!(error, CredentialError::Read { .. }));
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .to_string()
                .contains(".tmp")
        }));
    }

    #[test]
    fn runtime_uses_current_generation_only() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let current = temp.path().join(TESLAMATE_OWNER_TOKENS_CREDENTIAL);
        let previous = temp.path().join(TESLAMATE_OWNER_TOKENS_PREVIOUS_CREDENTIAL);
        fs::write(&current, b"not-json").expect("corrupt current");
        fs::write(&previous, pair_json("previous-access", "previous-refresh"))
            .expect("previous pair");
        set_private_mode(&current);
        set_private_mode(&previous);
        let error = CredentialDirectory::from_path(temp.path())
            .teslamate_owner_tokens()
            .expect_err("corrupt current generation must not use rollback material");
        assert!(matches!(error, CredentialError::InvalidOwnerTokens));
    }

    #[test]
    fn rollback_copies_previous_ciphertext_to_current_name() {
        let temp = tempfile::tempdir().expect("temporary credential directory");
        let current = temp.path().join(TESLAMATE_OWNER_TOKENS_CREDENTIAL);
        let previous = temp.path().join(TESLAMATE_OWNER_TOKENS_PREVIOUS_CREDENTIAL);
        fs::write(&current, b"new-encrypted").expect("current ciphertext");
        fs::write(&previous, b"old-encrypted").expect("previous ciphertext");
        set_private_mode(&current);
        set_private_mode(&previous);
        CredentialDirectory::restore_previous_generation(&current).expect("rollback");
        assert_eq!(fs::read(&current).unwrap(), b"old-encrypted");
        assert_eq!(fs::read(&previous).unwrap(), b"old-encrypted");
    }

    #[test]
    fn credential_pair_debug_is_redacted() {
        let tokens =
            OwnerTokens::from_credential_json(&pair_json("access-secret", "refresh-secret"))
                .expect("valid pair");
        let debug = format!("{tokens:?}");
        assert_eq!(debug, "OwnerTokens([redacted])");
        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("refresh-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn mac_keychain_sink_sends_strict_json_only_on_stdin() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary helper directory");
        let helper = temp.path().join("keychain-helper.sh");
        let argv = temp.path().join("argv");
        let environment = temp.path().join("environment");
        let stdin = temp.path().join("stdin");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n/usr/bin/env > '{}'\ncat > '{}'\n",
            argv.display(),
            environment.display(),
            stdin.display(),
        );
        fs::write(&helper, script).expect("helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).expect("executable");
        let config = MacKeychainConfig::from_parts(
            helper,
            "com.teslatlas.hub.owner-tokens".to_owned(),
            "test-account".to_owned(),
        )
        .expect("valid helper configuration");

        persist_mac_teslamate_owner_tokens(&config, "access-secret", "refresh-secret")
            .expect("helper success");
        let args = fs::read_to_string(argv).expect("argv capture");
        assert_eq!(
            args,
            "set\ncom.teslatlas.hub.owner-tokens\ntest-account\n"
        );
        assert!(!args.contains("access-secret"));
        assert!(!args.contains("refresh-secret"));
        assert!(!fs::read_to_string(environment)
            .expect("environment capture")
            .contains("secret"));
        let stored = fs::read(stdin).expect("stdin capture");
        let tokens = OwnerTokens::from_credential_json(&stored).expect("strict token JSON");
        assert_eq!(tokens.access_token(), "access-secret");
        assert_eq!(tokens.refresh_token(), "refresh-secret");
    }

    #[cfg(unix)]
    #[test]
    fn mac_keychain_helper_failure_is_retryable_and_does_not_mutate_pair() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary helper directory");
        let helper = temp.path().join("keychain-helper.sh");
        fs::write(&helper, "#!/bin/sh\ncat >/dev/null\nexit 1\n").expect("helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).expect("executable");
        let config = MacKeychainConfig::from_parts(
            helper,
            "com.teslatlas.hub.owner-tokens".to_owned(),
            "test-account".to_owned(),
        )
        .expect("valid helper configuration");

        for _ in 0..2 {
            assert!(matches!(
                persist_mac_teslamate_owner_tokens(&config, "old-access", "old-refresh"),
                Err(CredentialError::MacKeychainHelperFailed)
            ));
        }
        assert_eq!("old-access", "old-access");
        assert_eq!("old-refresh", "old-refresh");
    }

    #[cfg(unix)]
    #[test]
    fn mac_keychain_configuration_requires_absolute_private_executable() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary helper directory");
        let helper = temp.path().join("keychain-helper.sh");
        fs::write(&helper, "#!/bin/sh\nexit 0\n").expect("helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o644)).expect("mode");
        assert!(matches!(
            MacKeychainConfig::from_parts(
                PathBuf::from("relative-helper"),
                "service".to_owned(),
                "account".to_owned(),
            ),
            Err(CredentialError::MacKeychainHelperInvalid)
        ));
        assert!(matches!(
            MacKeychainConfig::from_parts(
                helper,
                "service".to_owned(),
                "account".to_owned(),
            ),
            Err(CredentialError::MacKeychainHelperInvalid)
        ));
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
