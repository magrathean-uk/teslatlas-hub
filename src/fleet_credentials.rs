//! Encrypted persistence and one resident refresh owner for Fleet OAuth.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rustix::{
    fs::{CWD, FileType, Mode, OFlags, RenameFlags, fstat, open, renameat_with},
    io::Errno,
    process::getuid,
};
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    credentials::OwnerTokens,
    db::{
        FleetTokenStore, HubStore, OutboundRequestCompletion, OutboundRequestOutcome, StoreError,
    },
    fleet_api::{
        FleetAccessToken, FleetApiConfigError, FleetApiError, FleetAuthApi, FleetClientId,
        FleetRefreshToken, FleetRegion,
    },
    protocol::CursorKey,
    teslamate_credentials::{TeslaMateCredentialError, load_existing_cursor_key_bytes},
    teslamate_token::{
        TeslaMateTokenError, decrypt_legacy_owner_tokens, encrypt_legacy_owner_tokens,
    },
};

const MIN_FLEET_TOKEN_LIFETIME_SECONDS: u64 = 60;
const MAX_FLEET_TOKEN_LIFETIME_SECONDS: u64 = 365 * 24 * 60 * 60;
const MAX_REFRESH_LEAD_SECONDS: u64 = 30 * 60;
const FLEET_KEY_BYTES: usize = 32;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const FLEET_KEY_FILE_NAME: &str = "fleet-credentials.key";
const FLEET_KEY_PENDING_FILE_NAME: &str = ".fleet-credentials.pending.key";
const FLEET_KEY_MIGRATION_MARKER: &str = ".fleet-credentials-key-migration";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetScopeSummary {
    pub vehicle_device_data: bool,
    pub vehicle_location: bool,
    pub vehicle_commands: bool,
    pub vehicle_charging_commands: bool,
}

impl FleetScopeSummary {
    const fn collection_ready(self) -> bool {
        self.vehicle_device_data && self.vehicle_location
    }
}

pub fn fleet_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets").join(FLEET_KEY_FILE_NAME)
}

fn migration_marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets").join(FLEET_KEY_MIGRATION_MARKER)
}

fn pending_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets").join(FLEET_KEY_PENDING_FILE_NAME)
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct FleetSetupCredentials {
    access_token: Zeroizing<String>,
    refresh_token: Zeroizing<String>,
    client_id: String,
    #[zeroize(skip)]
    region: FleetRegion,
    #[zeroize(skip)]
    expires_in_seconds: u64,
}

impl FleetSetupCredentials {
    pub fn new(
        access_token: String,
        refresh_token: String,
        client_id: String,
        region: FleetRegion,
        expires_in_seconds: u64,
    ) -> Result<Self, FleetCredentialError> {
        FleetAccessToken::new(access_token.clone())?;
        FleetRefreshToken::new(refresh_token.clone())?;
        FleetClientId::parse(&client_id)?;
        if !(MIN_FLEET_TOKEN_LIFETIME_SECONDS..=MAX_FLEET_TOKEN_LIFETIME_SECONDS)
            .contains(&expires_in_seconds)
        {
            return Err(FleetCredentialError::InvalidLifetime);
        }
        Ok(Self {
            access_token: Zeroizing::new(access_token),
            refresh_token: Zeroizing::new(refresh_token),
            client_id,
            region,
            expires_in_seconds,
        })
    }

    pub(crate) fn access_token(&self) -> Result<FleetAccessToken, FleetCredentialError> {
        FleetAccessToken::new(self.access_token.as_str()).map_err(Into::into)
    }

    pub(crate) const fn region(&self) -> FleetRegion {
        self.region
    }

    pub fn require_collection_scopes(&self) -> Result<(), FleetCredentialError> {
        let summary = fleet_scope_summary(&self.access_token)?;
        if !summary.collection_ready() {
            return Err(FleetCredentialError::MissingCollectionScopes);
        }
        Ok(())
    }
}

impl std::fmt::Debug for FleetSetupCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FleetSetupCredentials")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("client_id", &"[redacted]")
            .field("region", &self.region)
            .field("expires_in_seconds", &self.expires_in_seconds)
            .finish()
    }
}

pub fn persist_fleet_setup_credentials(
    store: &HubStore,
    data_dir: &Path,
    credentials: &FleetSetupCredentials,
    now: SystemTime,
) -> Result<(), FleetCredentialError> {
    let existing = store.load_fleet_tokens()?;
    if checked_path_exists(&migration_marker_path(data_dir), true)? {
        return Err(FleetCredentialError::MigrationRequired);
    }
    let replacing_legacy = existing.is_some() && load_existing_fleet_key_bytes(data_dir)?.is_none();
    if replacing_legacy {
        create_migration_marker(data_dir)?;
    }
    let key = load_or_create_fleet_key(data_dir)?;
    let schedule = refresh_schedule(now, credentials.expires_in_seconds)?;
    let stored = encrypt_store(
        &key,
        &credentials.access_token,
        &credentials.refresh_token,
        &credentials.client_id,
        credentials.region,
        schedule,
    )?;
    let persisted = if existing.is_some() {
        store.replace_fleet_tokens_and_scrub(&stored)
    } else {
        store.replace_fleet_tokens(&stored)
    };
    if let Err(error) = persisted {
        return Err(error.into());
    }
    if replacing_legacy {
        finish_key_migration(data_dir)?;
    }
    Ok(())
}

/// Validate persisted Fleet credentials without claiming refresh ownership or
/// changing their stored generation. Used by read-only service preflight.
pub fn validate_stored_fleet_credentials(
    store: &HubStore,
    data_dir: &Path,
) -> Result<(), FleetCredentialError> {
    if store.has_unresolved_fleet_refresh()? {
        return Err(FleetCredentialError::RotationOutcomeUnknown);
    }
    let stored = store
        .load_fleet_tokens()?
        .ok_or(FleetCredentialError::Missing)?;
    if checked_path_exists(&migration_marker_path(data_dir), true)? {
        return Err(FleetCredentialError::MigrationRequired);
    }
    let (encryption_key, migration_required) = match load_existing_fleet_key_bytes(data_dir)? {
        Some(key) => (key, false),
        None => {
            let cursor = load_existing_cursor_key_bytes(data_dir)?
                .ok_or(FleetCredentialError::KeyMissing)?;
            let cursor: [u8; FLEET_KEY_BYTES] = cursor
                .as_slice()
                .try_into()
                .map_err(|_| FleetCredentialError::KeyMissing)?;
            (
                Zeroizing::new(
                    CursorKey::from_bytes(cursor)
                        .fleet_credential_encryption_key()
                        .to_vec(),
                ),
                true,
            )
        }
    };
    let plaintext =
        decrypt_legacy_owner_tokens(&encryption_key, stored.access(), stored.refresh())?;
    let credential_generation =
        crate::teslamate_token::legacy_refresh_credential_generation(&plaintext);
    if stored
        .credential_generation()
        .is_some_and(|stored| stored != credential_generation)
    {
        return Err(FleetCredentialError::RotationOutcomeUnknown);
    }
    if migration_required {
        return Err(FleetCredentialError::MigrationRequired);
    }
    let access_token = FleetAccessToken::new(plaintext.access_token().to_owned())?;
    if !fleet_scope_summary(access_token.expose())?.collection_ready() {
        return Err(FleetCredentialError::MissingCollectionScopes);
    }
    FleetRefreshToken::new(plaintext.refresh_token().to_owned())?;
    FleetClientId::parse(stored.client_id())?;
    FleetRegion::from_storage_code(stored.region())?;
    Ok(())
}

pub fn stored_fleet_scope_summary(
    store: &HubStore,
    data_dir: &Path,
) -> Result<Option<FleetScopeSummary>, FleetCredentialError> {
    let Some(stored) = store.load_fleet_tokens()? else {
        return Ok(None);
    };
    let encryption_key =
        load_existing_fleet_key_bytes(data_dir)?.ok_or(FleetCredentialError::MigrationRequired)?;
    let plaintext =
        decrypt_legacy_owner_tokens(&encryption_key, stored.access(), stored.refresh())?;
    fleet_scope_summary(plaintext.access_token()).map(Some)
}

fn fleet_scope_summary(access_token: &str) -> Result<FleetScopeSummary, FleetCredentialError> {
    let mut parts = access_token.split('.');
    let Some(header) = parts.next() else {
        return Err(FleetCredentialError::InvalidAccessTokenClaims);
    };
    let Some(payload) = parts.next() else {
        return Err(FleetCredentialError::InvalidAccessTokenClaims);
    };
    let Some(signature) = parts.next() else {
        return Err(FleetCredentialError::InvalidAccessTokenClaims);
    };
    if header.is_empty() || payload.is_empty() || signature.is_empty() || parts.next().is_some() {
        return Err(FleetCredentialError::InvalidAccessTokenClaims);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| FleetCredentialError::InvalidAccessTokenClaims)?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| FleetCredentialError::InvalidAccessTokenClaims)?;
    let scopes = claims
        .get("scp")
        .and_then(serde_json::Value::as_array)
        .ok_or(FleetCredentialError::InvalidAccessTokenClaims)?;
    if scopes
        .iter()
        .any(|scope| scope.as_str().is_none_or(str::is_empty))
    {
        return Err(FleetCredentialError::InvalidAccessTokenClaims);
    }
    let has = |required: &str| scopes.iter().any(|scope| scope.as_str() == Some(required));
    Ok(FleetScopeSummary {
        vehicle_device_data: has("vehicle_device_data"),
        vehicle_location: has("vehicle_location"),
        vehicle_commands: has("vehicle_cmds"),
        vehicle_charging_commands: has("vehicle_charging_cmds"),
    })
}

pub(crate) struct FleetAuthManager {
    store: HubStore,
    encryption_key: Zeroizing<Vec<u8>>,
    access_token: FleetAccessToken,
    refresh_token: FleetRefreshToken,
    client_id: FleetClientId,
    region: FleetRegion,
    expires_at: i64,
    next_refresh_at: i64,
    credential_generation: Uuid,
    refresh_terminal: bool,
    #[cfg(unix)]
    admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
}

impl FleetAuthManager {
    pub(crate) fn mark_refresh_due(&mut self) {
        self.next_refresh_at = 0;
    }

    fn from_store_inner(store: HubStore, data_dir: &Path) -> Result<Self, FleetCredentialError> {
        if store.has_unresolved_fleet_refresh()? {
            return Err(FleetCredentialError::RotationOutcomeUnknown);
        }
        let stored = store
            .load_fleet_tokens()?
            .ok_or(FleetCredentialError::Missing)?;
        if checked_path_exists(&migration_marker_path(data_dir), true)? {
            return Err(FleetCredentialError::MigrationRequired);
        }
        let encryption_key = load_existing_fleet_key_bytes(data_dir)?
            .ok_or(FleetCredentialError::MigrationRequired)?;
        let plaintext =
            decrypt_legacy_owner_tokens(&encryption_key, stored.access(), stored.refresh())?;
        let credential_generation =
            crate::teslamate_token::legacy_refresh_credential_generation(&plaintext);
        store.bind_fleet_credential_generation(&stored, credential_generation)?;
        Ok(Self {
            store,
            encryption_key,
            access_token: FleetAccessToken::new(plaintext.access_token().to_owned())?,
            refresh_token: FleetRefreshToken::new(plaintext.refresh_token().to_owned())?,
            client_id: FleetClientId::parse(stored.client_id())?,
            region: FleetRegion::from_storage_code(stored.region())?,
            expires_at: stored.expires_at(),
            next_refresh_at: stored.next_refresh_at(),
            credential_generation,
            refresh_terminal: false,
            #[cfg(unix)]
            admission: None,
        })
    }

    #[cfg(unix)]
    pub(crate) fn from_store_for_admitted_user(
        store: HubStore,
        data_dir: &Path,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ) -> Result<Self, FleetCredentialError> {
        admission
            .assert_sensitive_access()
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        admission
            .assert_store_path(data_dir)
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        let mut manager = Self::from_store_inner(store, data_dir)?;
        manager.admission = Some(admission);
        Ok(manager)
    }

    #[cfg(test)]
    pub(crate) fn from_store(
        store: HubStore,
        data_dir: &Path,
    ) -> Result<Self, FleetCredentialError> {
        Self::from_store_inner(store, data_dir)
    }

    #[cfg(test)]
    pub(crate) fn access_token(&self) -> &FleetAccessToken {
        &self.access_token
    }

    fn assert_sensitive_access(&self) -> Result<(), FleetCredentialError> {
        #[cfg(unix)]
        if let Some(admission) = &self.admission {
            admission
                .assert_sensitive_access()
                .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        }
        Ok(())
    }

    /// Revalidate retained user admission at the last bearer-use boundary.
    pub(crate) fn access_token_for_sensitive_use(
        &self,
    ) -> Result<&FleetAccessToken, FleetCredentialError> {
        self.assert_sensitive_access()?;
        Ok(&self.access_token)
    }

    pub(crate) const fn region(&self) -> FleetRegion {
        self.region
    }

    pub(crate) async fn refresh_if_due(
        &mut self,
        api: &FleetAuthApi,
        now: SystemTime,
    ) -> Result<(), FleetCredentialError> {
        self.assert_sensitive_access()?;
        let now_seconds = epoch_seconds(now)?;
        if now_seconds < self.next_refresh_at {
            return Ok(());
        }
        self.refresh_now(api, now).await
    }

    pub(crate) async fn refresh_now(
        &mut self,
        api: &FleetAuthApi,
        now: SystemTime,
    ) -> Result<(), FleetCredentialError> {
        if self.refresh_terminal {
            return Err(FleetCredentialError::RotationOutcomeUnknown);
        }
        self.assert_sensitive_access()?;
        let receipt_id = self
            .store
            .begin_fleet_refresh(self.credential_generation)
            .map_err(|error| {
                self.refresh_terminal = true;
                FleetCredentialError::Store(error)
            })?;
        if self.assert_sensitive_access().is_err() {
            if self
                .store
                .cancel_unsent_fleet_refresh(receipt_id, self.credential_generation)
                .is_err()
            {
                self.refresh_terminal = true;
                return Err(FleetCredentialError::RotationOutcomeUnknown);
            }
            return Err(FleetCredentialError::SensitiveAccessUnavailable);
        }
        let refreshed = match api.refresh(&self.client_id, &self.refresh_token).await {
            Ok(refreshed) => refreshed,
            Err(error) => {
                if let Some(completion) = retryable_refresh_completion(&error) {
                    if self
                        .store
                        .complete_retryable_fleet_refresh_failure(
                            receipt_id,
                            self.credential_generation,
                            &completion,
                        )
                        .is_err()
                    {
                        self.refresh_terminal = true;
                        return Err(FleetCredentialError::RotationOutcomeUnknown);
                    }
                    return Err(FleetCredentialError::Api(error));
                }
                self.refresh_terminal = true;
                return Err(FleetCredentialError::Api(error));
            }
        };
        let schedule = refresh_schedule(now, refreshed.expires_in_seconds).inspect_err(|_| {
            self.refresh_terminal = true;
        })?;
        let stored = encrypt_store(
            &self.encryption_key,
            refreshed.access_token.expose(),
            refreshed.refresh_token.expose(),
            self.client_id.as_str(),
            self.region,
            schedule,
        )
        .inspect_err(|_| {
            self.refresh_terminal = true;
        })?;
        let output_generation = stored.credential_generation().ok_or_else(|| {
            self.refresh_terminal = true;
            FleetCredentialError::RotationOutcomeUnknown
        })?;
        self.store
            .complete_fleet_refresh(
                receipt_id,
                self.credential_generation,
                output_generation,
                &stored,
            )
            .map_err(|error| {
                self.refresh_terminal = true;
                FleetCredentialError::Store(error)
            })?;
        self.access_token = refreshed.access_token;
        self.refresh_token = refreshed.refresh_token;
        self.expires_at = schedule.expires_at;
        self.next_refresh_at = schedule.next_refresh_at;
        self.credential_generation = output_generation;
        Ok(())
    }
}

fn retryable_refresh_completion(error: &FleetApiError) -> Option<OutboundRequestCompletion> {
    // Any HTTP response proves the request was sent, not that a single-use
    // refresh token remained unconsumed. Preserve the fence in that case.
    if !matches!(error, FleetApiError::RequestNotSent) {
        return None;
    }
    Some(OutboundRequestCompletion {
        outcome: OutboundRequestOutcome::TransportError,
        http_status: None,
        retry_after_seconds: None,
    })
}

impl std::fmt::Debug for FleetAuthManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FleetAuthManager")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("client_id", &"[redacted]")
            .field("region", &self.region)
            .field("expires_at", &self.expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
            .finish()
    }
}

#[derive(Clone, Copy)]
struct FleetRefreshSchedule {
    expires_at: i64,
    next_refresh_at: i64,
}

fn refresh_schedule(
    now: SystemTime,
    expires_in_seconds: u64,
) -> Result<FleetRefreshSchedule, FleetCredentialError> {
    if !(MIN_FLEET_TOKEN_LIFETIME_SECONDS..=MAX_FLEET_TOKEN_LIFETIME_SECONDS)
        .contains(&expires_in_seconds)
    {
        return Err(FleetCredentialError::InvalidLifetime);
    }
    let now = epoch_seconds(now)?;
    let lifetime =
        i64::try_from(expires_in_seconds).map_err(|_| FleetCredentialError::InvalidLifetime)?;
    let lead = i64::try_from((expires_in_seconds / 4).clamp(1, MAX_REFRESH_LEAD_SECONDS))
        .map_err(|_| FleetCredentialError::InvalidLifetime)?;
    let expires_at = now
        .checked_add(lifetime)
        .ok_or(FleetCredentialError::ClockOverflow)?;
    let next_refresh_at = expires_at
        .checked_sub(lead)
        .ok_or(FleetCredentialError::ClockOverflow)?;
    Ok(FleetRefreshSchedule {
        expires_at,
        next_refresh_at,
    })
}

fn epoch_seconds(now: SystemTime) -> Result<i64, FleetCredentialError> {
    i64::try_from(
        now.duration_since(UNIX_EPOCH)
            .map_err(FleetCredentialError::Clock)?
            .as_secs(),
    )
    .map_err(|_| FleetCredentialError::ClockOverflow)
}

pub(crate) fn load_or_migrate_fleet_key_for_tokens(
    store: &HubStore,
    data_dir: &Path,
    stored: &FleetTokenStore,
) -> Result<Zeroizing<Vec<u8>>, FleetCredentialError> {
    let marker_exists = checked_path_exists(&migration_marker_path(data_dir), true)?;
    let current = load_existing_fleet_key_bytes(data_dir)?;
    if !marker_exists {
        if let Some(current) = current {
            return Ok(current);
        }
        let plaintext = decrypt_with_legacy_cursor_key(data_dir, stored)?;
        create_migration_marker(data_dir)?;
        let key = load_or_create_fleet_key(data_dir)?;
        migrate_fleet_ciphertext(store, stored, &plaintext, &key)?;
        finish_key_migration(data_dir)?;
        return Ok(key);
    }

    let key = current.unwrap_or(load_or_create_fleet_key(data_dir)?);
    let plaintext = decrypt_legacy_owner_tokens(&key, stored.access(), stored.refresh())
        .or_else(|_| decrypt_with_legacy_cursor_key(data_dir, stored))?;
    migrate_fleet_ciphertext(store, stored, &plaintext, &key)?;
    finish_key_migration(data_dir)?;
    Ok(key)
}

/// Complete the forward-only schema-55 Fleet key split during bootstrap.
pub fn migrate_legacy_fleet_credentials(
    store: &HubStore,
    data_dir: &Path,
) -> Result<bool, FleetCredentialError> {
    let Some(stored) = store.load_fleet_tokens()? else {
        return Ok(false);
    };
    let migration_required = checked_path_exists(&migration_marker_path(data_dir), true)?
        || load_existing_fleet_key_bytes(data_dir)?.is_none();
    if !migration_required {
        let key =
            load_existing_fleet_key_bytes(data_dir)?.ok_or(FleetCredentialError::KeyMissing)?;
        drop(decrypt_legacy_owner_tokens(
            &key,
            stored.access(),
            stored.refresh(),
        )?);
        return Ok(false);
    }
    let key = load_or_migrate_fleet_key_for_tokens(store, data_dir, &stored)?;
    let migrated = store
        .load_fleet_tokens()?
        .ok_or(FleetCredentialError::Missing)?;
    drop(decrypt_legacy_owner_tokens(
        &key,
        migrated.access(),
        migrated.refresh(),
    )?);
    Ok(true)
}

fn decrypt_with_legacy_cursor_key(
    data_dir: &Path,
    stored: &FleetTokenStore,
) -> Result<OwnerTokens, FleetCredentialError> {
    let bytes =
        load_existing_cursor_key_bytes(data_dir)?.ok_or(FleetCredentialError::KeyMissing)?;
    let bytes: [u8; FLEET_KEY_BYTES] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| FleetCredentialError::KeyMissing)?;
    let key = Zeroizing::new(
        CursorKey::from_bytes(bytes)
            .fleet_credential_encryption_key()
            .to_vec(),
    );
    decrypt_legacy_owner_tokens(&key, stored.access(), stored.refresh()).map_err(Into::into)
}

fn migrate_fleet_ciphertext(
    store: &HubStore,
    stored: &FleetTokenStore,
    plaintext: &OwnerTokens,
    key: &[u8],
) -> Result<(), FleetCredentialError> {
    let replacement = encrypt_store(
        key,
        plaintext.access_token(),
        plaintext.refresh_token(),
        stored.client_id(),
        FleetRegion::from_storage_code(stored.region())?,
        FleetRefreshSchedule {
            expires_at: stored.expires_at(),
            next_refresh_at: stored.next_refresh_at(),
        },
    )?;
    store.replace_fleet_tokens_and_scrub(&replacement)?;
    Ok(())
}

fn load_or_create_fleet_key(data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, FleetCredentialError> {
    if let Some(key) = load_existing_fleet_key_bytes(data_dir)? {
        return Ok(key);
    }
    let secrets = ensure_secrets_directory(data_dir)?;
    let destination = fleet_key_path(data_dir);
    let temporary = pending_key_path(data_dir);
    if checked_path_exists(&temporary, false)? {
        renameat_with(CWD, &temporary, CWD, &destination, RenameFlags::NOREPLACE).map_err(
            |source| {
                fleet_key_io(
                    "resuming Fleet credential key publication",
                    &destination,
                    source.into(),
                )
            },
        )?;
        sync_directory(&secrets)?;
        return load_existing_fleet_key_bytes(data_dir)?.ok_or(FleetCredentialError::KeyMissing);
    }
    let mut key = Zeroizing::new(vec![0_u8; FLEET_KEY_BYTES]);
    getrandom::fill(key.as_mut_slice()).map_err(|_| FleetCredentialError::EntropyUnavailable)?;
    let result = (|| {
        write_private_file(&temporary, &key)?;
        match renameat_with(CWD, &temporary, CWD, &destination, RenameFlags::NOREPLACE) {
            Ok(()) => {
                sync_directory(&secrets)?;
                Ok(key)
            }
            Err(Errno::EXIST) => {
                let _ = fs::remove_file(&temporary);
                load_existing_fleet_key_bytes(data_dir)?.ok_or(FleetCredentialError::KeyMissing)
            }
            Err(source) => Err(fleet_key_io(
                "publishing Fleet credential key",
                &destination,
                source.into(),
            )),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn load_existing_fleet_key_bytes(
    data_dir: &Path,
) -> Result<Option<Zeroizing<Vec<u8>>>, FleetCredentialError> {
    let path = fleet_key_path(data_dir);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            validate_secrets_directory(&data_dir.join("secrets"))?;
            read_checked_private_file(&path, FLEET_KEY_BYTES, false).map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(fleet_key_io(
            "inspecting Fleet credential key",
            path,
            source,
        )),
    }
}

pub fn remove_fleet_key_and_tokens(
    data_dir: &Path,
    store: &HubStore,
) -> Result<(), FleetCredentialError> {
    store.clear_fleet_tokens()?;
    let secrets = data_dir.join("secrets");
    let key = fleet_key_path(data_dir);
    let pending = pending_key_path(data_dir);
    let marker = migration_marker_path(data_dir);
    let mut removed = false;
    for (path, marker_file) in [(&key, false), (&pending, false), (&marker, true)] {
        if checked_path_exists(path, marker_file)? {
            fs::remove_file(path)
                .map_err(|source| fleet_key_io("removing Fleet credential key", path, source))?;
            removed = true;
        }
    }
    if removed {
        sync_directory(&secrets)?;
    }
    Ok(())
}

fn create_migration_marker(data_dir: &Path) -> Result<(), FleetCredentialError> {
    let secrets = ensure_secrets_directory(data_dir)?;
    let marker = migration_marker_path(data_dir);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(&marker)
    {
        Ok(file) => {
            file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
                .map_err(|source| {
                    fleet_key_io("protecting Fleet migration marker", &marker, source)
                })?;
            file.sync_all().map_err(|source| {
                fleet_key_io("syncing Fleet migration marker", &marker, source)
            })?;
            sync_directory(&secrets)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            read_checked_private_file(&marker, 0, true).map(drop)
        }
        Err(source) => Err(fleet_key_io(
            "creating Fleet migration marker",
            marker,
            source,
        )),
    }
}

fn finish_key_migration(data_dir: &Path) -> Result<(), FleetCredentialError> {
    let marker = migration_marker_path(data_dir);
    read_checked_private_file(&marker, 0, true)?;
    fs::remove_file(&marker)
        .map_err(|source| fleet_key_io("removing Fleet migration marker", &marker, source))?;
    sync_directory(&data_dir.join("secrets"))
}

fn ensure_secrets_directory(data_dir: &Path) -> Result<PathBuf, FleetCredentialError> {
    let path = data_dir.join("secrets");
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(PRIVATE_DIRECTORY_MODE);
            match builder.create(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(fleet_key_io("creating secrets directory", &path, source));
                }
            }
        }
        Err(source) => return Err(fleet_key_io("inspecting secrets directory", &path, source)),
    }
    validate_secrets_directory(&path)?;
    Ok(path)
}

fn validate_secrets_directory(path: &Path) -> Result<(), FleetCredentialError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| fleet_key_io("inspecting secrets directory", path, source))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != getuid().as_raw()
        || metadata.permissions().mode() & 0o777 != PRIVATE_DIRECTORY_MODE
    {
        return Err(FleetCredentialError::UnsafeKeyMaterial);
    }
    Ok(())
}

fn checked_path_exists(path: &Path, marker: bool) -> Result<bool, FleetCredentialError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            if let Some(parent) = path.parent() {
                validate_secrets_directory(parent)?;
            }
            read_checked_private_file(path, if marker { 0 } else { FLEET_KEY_BYTES }, marker)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(fleet_key_io("inspecting Fleet key material", path, source)),
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), FleetCredentialError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|source| fleet_key_io("creating Fleet credential key", path, source))?;
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .map_err(|source| fleet_key_io("protecting Fleet credential key", path, source))?;
    file.write_all(bytes)
        .map_err(|source| fleet_key_io("writing Fleet credential key", path, source))?;
    file.sync_all()
        .map_err(|source| fleet_key_io("syncing Fleet credential key", path, source))
}

fn read_checked_private_file(
    path: &Path,
    exact_bytes: usize,
    marker: bool,
) -> Result<Zeroizing<Vec<u8>>, FleetCredentialError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| FleetCredentialError::UnsafeKeyMaterial)?;
    let held = fstat(&descriptor).map_err(|_| FleetCredentialError::UnsafeKeyMaterial)?;
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || (held.st_mode as u32 & 0o777) != PRIVATE_FILE_MODE
        || held.st_uid != getuid().as_raw()
    {
        return Err(FleetCredentialError::UnsafeKeyMaterial);
    }
    let file: File = descriptor.into();
    let mut bytes = Zeroizing::new(Vec::with_capacity(exact_bytes));
    file.take(u64::try_from(exact_bytes + 1).expect("Fleet key cap fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|source| fleet_key_io("reading Fleet key material", path, source))?;
    let current =
        fs::symlink_metadata(path).map_err(|_| FleetCredentialError::UnsafeKeyMaterial)?;
    if bytes.len() != exact_bytes
        || current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.uid() != held.st_uid
        || current.dev() != held.st_dev as u64
        || current.ino() != held.st_ino
        || current.permissions().mode() & 0o777 != PRIVATE_FILE_MODE
    {
        return Err(if marker {
            FleetCredentialError::UnsafeMigrationState
        } else {
            FleetCredentialError::UnsafeKeyMaterial
        });
    }
    Ok(bytes)
}

fn sync_directory(path: &Path) -> Result<(), FleetCredentialError> {
    File::open(path)
        .map_err(|source| fleet_key_io("opening secrets directory", path, source))?
        .sync_all()
        .map_err(|source| fleet_key_io("syncing secrets directory", path, source))
}

fn fleet_key_io(
    operation: &'static str,
    path: impl AsRef<Path>,
    source: std::io::Error,
) -> FleetCredentialError {
    FleetCredentialError::KeyIo {
        operation,
        path: path.as_ref().to_path_buf(),
        source,
    }
}

fn encrypt_store(
    encryption_key: &[u8],
    access_token: &str,
    refresh_token: &str,
    client_id: &str,
    region: FleetRegion,
    schedule: FleetRefreshSchedule,
) -> Result<FleetTokenStore, FleetCredentialError> {
    let plaintext =
        OwnerTokens::from_secret_parts(access_token.to_owned(), refresh_token.to_owned())?;
    let generation = crate::teslamate_token::legacy_refresh_credential_generation(&plaintext);
    let (access, refresh) = encrypt_legacy_owner_tokens(encryption_key, &plaintext)?;
    FleetTokenStore::new(
        access,
        refresh,
        client_id.to_owned(),
        region.storage_code().to_owned(),
        schedule.expires_at,
        schedule.next_refresh_at,
        Some(generation),
    )
    .map_err(Into::into)
}

#[derive(Debug, Error)]
pub enum FleetCredentialError {
    #[error("operating-system entropy is unavailable for Fleet credential creation")]
    EntropyUnavailable,
    #[error("Fleet credentials are not configured")]
    Missing,
    #[error("Fleet credential key is missing")]
    KeyMissing,
    #[error("Fleet credential key migration is required; run bootstrap")]
    MigrationRequired,
    #[error("Fleet credential key material has unsafe type, ownership, mode, or length")]
    UnsafeKeyMaterial,
    #[error("Fleet credential key migration state is unsafe")]
    UnsafeMigrationState,
    #[error("Fleet credential key I/O failed while {operation} at {path}: {source}")]
    KeyIo {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Fleet token lifetime is invalid")]
    InvalidLifetime,
    #[error("Fleet access token claims are invalid")]
    InvalidAccessTokenClaims,
    #[error("Fleet authorization is missing vehicle data or location access; reconnect Tesla")]
    MissingCollectionScopes,
    #[error("system clock is before the Unix epoch")]
    Clock(#[source] std::time::SystemTimeError),
    #[error("system clock does not fit Fleet token scheduling")]
    ClockOverflow,
    #[error("Fleet refresh outcome is ambiguous; replace credentials before retrying")]
    RotationOutcomeUnknown,
    #[error("runtime sensitive-access admission is unavailable")]
    SensitiveAccessUnavailable,
    #[error(transparent)]
    ApiConfig(#[from] FleetApiConfigError),
    #[error(transparent)]
    Api(#[from] FleetApiError),
    #[error(transparent)]
    Credential(#[from] crate::credentials::CredentialError),
    #[error(transparent)]
    Cipher(#[from] TeslaMateTokenError),
    #[error(transparent)]
    Key(#[from] TeslaMateCredentialError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl FleetCredentialError {
    pub(crate) fn is_sensitive_access_failure(&self) -> bool {
        matches!(
            self,
            Self::Missing
                | Self::KeyMissing
                | Self::MigrationRequired
                | Self::UnsafeKeyMaterial
                | Self::UnsafeMigrationState
                | Self::InvalidAccessTokenClaims
                | Self::MissingCollectionScopes
                | Self::KeyIo { .. }
                | Self::Cipher(_)
                | Self::Key(_)
                | Self::Store(_)
                | Self::RotationOutcomeUnknown
                | Self::SensitiveAccessUnavailable
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Router,
        body::Bytes,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use tokio::net::TcpListener;

    use super::*;

    const TEST_SCOPED_ACCESS: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iLCJ2ZWhpY2xlX2NtZHMiLCJ2ZWhpY2xlX2NoYXJnaW5nX2NtZHMiXX0.sig";

    #[test]
    fn fleet_scope_claims_are_bounded_and_collection_scopes_are_required() {
        let summary = fleet_scope_summary(TEST_SCOPED_ACCESS).expect("scope summary");
        assert!(summary.vehicle_device_data);
        assert!(summary.vehicle_location);
        assert!(summary.vehicle_commands);
        assert!(summary.vehicle_charging_commands);

        let missing_location = FleetSetupCredentials::new(
            "e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig".to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("syntactically valid credentials");
        assert!(matches!(
            missing_location.require_collection_scopes(),
            Err(FleetCredentialError::MissingCollectionScopes)
        ));
        assert!(matches!(
            fleet_scope_summary("not-a-jwt"),
            Err(FleetCredentialError::InvalidAccessTokenClaims)
        ));
    }

    #[test]
    fn fleet_setup_round_trips_encrypted_and_redacted() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let credentials = FleetSetupCredentials::new(
            TEST_SCOPED_ACCESS.to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("setup credentials");
        persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            UNIX_EPOCH + std::time::Duration::from_secs(1_000),
        )
        .expect("persist");

        let stored = store.load_fleet_tokens().expect("store").expect("row");
        assert!(
            !stored
                .access()
                .windows(TEST_SCOPED_ACCESS.len())
                .any(|part| part == TEST_SCOPED_ACCESS.as_bytes())
        );
        assert!(
            !stored
                .refresh()
                .windows(13)
                .any(|part| part == b"fleet-refresh")
        );
        validate_stored_fleet_credentials(&store, temporary.path()).expect("read-only validation");
        let manager = FleetAuthManager::from_store(store, temporary.path()).expect("manager");
        assert_eq!(manager.access_token().expose(), TEST_SCOPED_ACCESS);
        assert_eq!(manager.region(), FleetRegion::EuropeMiddleEastAndAfrica);
        let rendered = format!("{credentials:?} {manager:?} {stored:?}");
        assert!(!rendered.contains(TEST_SCOPED_ACCESS));
        assert!(!rendered.contains("fleet-refresh"));
        assert!(!rendered.contains("fleet-client"));
    }

    #[test]
    fn stored_fleet_credentials_require_collection_scopes() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let credentials = FleetSetupCredentials::new(
            "e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig".to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("syntactically valid credentials");
        persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            UNIX_EPOCH + std::time::Duration::from_secs(1_000),
        )
        .expect("persist");

        assert!(matches!(
            validate_stored_fleet_credentials(&store, temporary.path()),
            Err(FleetCredentialError::MissingCollectionScopes)
        ));
    }

    #[test]
    fn schema_55_cursor_encrypted_row_upgrades_and_scrubs_live_sqlite_traces() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let cursor = load_existing_cursor_key_bytes(temporary.path())
            .expect("cursor read")
            .expect("cursor bytes");
        let cursor: [u8; FLEET_KEY_BYTES] =
            cursor.as_slice().try_into().expect("cursor key length");
        let legacy_key = Zeroizing::new(
            CursorKey::from_bytes(cursor)
                .fleet_credential_encryption_key()
                .to_vec(),
        );
        let plaintext = OwnerTokens::from_secret_parts(
            "legacy-fleet-access".to_owned(),
            "legacy-fleet-refresh".to_owned(),
        )
        .expect("legacy plaintext");
        let generation = crate::teslamate_token::legacy_refresh_credential_generation(&plaintext);
        let (access, refresh) =
            encrypt_legacy_owner_tokens(&legacy_key, &plaintext).expect("legacy encryption");
        let old_access = access.clone();
        let old_refresh = refresh.clone();
        let stored = FleetTokenStore::new(
            access,
            refresh,
            "legacy-fleet-client".to_owned(),
            "eu".to_owned(),
            2_000_000_000,
            1_900_000_000,
            Some(generation),
        )
        .expect("schema-55 Fleet row");
        store
            .replace_fleet_tokens(&stored)
            .expect("legacy Fleet row persists");

        let catalogue_before = fs::read(temporary.path().join("hub.sqlite")).expect("catalogue");
        let immutable = HubStore::open_immutable_read_only(temporary.path())
            .expect("immutable preflight store");
        assert!(matches!(
            validate_stored_fleet_credentials(&immutable, temporary.path()),
            Err(FleetCredentialError::MigrationRequired)
        ));
        immutable
            .verify_immutable_snapshot_unchanged()
            .expect("preflight stayed byte stable");
        assert_eq!(
            fs::read(temporary.path().join("hub.sqlite")).expect("catalogue after preflight"),
            catalogue_before
        );
        assert!(!fleet_key_path(temporary.path()).exists());
        let still_legacy = store
            .load_fleet_tokens()
            .expect("legacy row remains")
            .expect("legacy credentials remain");
        assert_eq!(still_legacy.access(), old_access.as_slice());
        assert_eq!(still_legacy.refresh(), old_refresh.as_slice());
        assert!(matches!(
            FleetAuthManager::from_store(store.clone(), temporary.path()),
            Err(FleetCredentialError::MigrationRequired)
        ));

        assert!(
            migrate_legacy_fleet_credentials(&store, temporary.path())
                .expect("bootstrap migration")
        );
        let manager = FleetAuthManager::from_store(store.clone(), temporary.path())
            .expect("migrated row loads");
        assert_eq!(manager.access_token().expose(), "legacy-fleet-access");
        let dedicated = load_existing_fleet_key_bytes(temporary.path())
            .expect("dedicated key read")
            .expect("dedicated key exists");
        assert_ne!(dedicated.as_slice(), legacy_key.as_slice());
        assert_eq!(
            fs::metadata(fleet_key_path(temporary.path()))
                .expect("Fleet key metadata")
                .permissions()
                .mode()
                & 0o777,
            PRIVATE_FILE_MODE
        );
        let upgraded = store
            .load_fleet_tokens()
            .expect("upgraded row")
            .expect("upgraded credentials");
        let decrypted =
            decrypt_legacy_owner_tokens(&dedicated, upgraded.access(), upgraded.refresh())
                .expect("dedicated key decrypts upgraded row");
        assert_eq!(decrypted.access_token(), "legacy-fleet-access");
        assert!(
            decrypt_legacy_owner_tokens(&legacy_key, upgraded.access(), upgraded.refresh())
                .is_err()
        );
        assert!(!migration_marker_path(temporary.path()).exists());

        for path in [
            temporary.path().join("hub.sqlite"),
            temporary.path().join("hub.sqlite-wal"),
        ] {
            if let Ok(bytes) = fs::read(path) {
                assert!(
                    !bytes
                        .windows(old_access.len())
                        .any(|part| part == old_access)
                );
                assert!(
                    !bytes
                        .windows(old_refresh.len())
                        .any(|part| part == old_refresh)
                );
            }
        }
    }

    #[test]
    fn signout_deletes_fleet_key_and_preserves_cursor_authority() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let cursor_before = load_existing_cursor_key_bytes(temporary.path())
            .expect("cursor read")
            .expect("cursor bytes");
        let credentials = FleetSetupCredentials::new(
            "fleet-signout-access".to_owned(),
            "fleet-signout-refresh".to_owned(),
            "fleet-signout-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("setup credentials");
        persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            UNIX_EPOCH + std::time::Duration::from_secs(1_000),
        )
        .expect("persist Fleet credentials");
        let stored = store
            .load_fleet_tokens()
            .expect("stored Fleet row")
            .expect("Fleet credentials");
        let cursor: [u8; FLEET_KEY_BYTES] = cursor_before
            .as_slice()
            .try_into()
            .expect("cursor key length");
        let cursor_derived = Zeroizing::new(
            CursorKey::from_bytes(cursor)
                .fleet_credential_encryption_key()
                .to_vec(),
        );
        assert!(
            decrypt_legacy_owner_tokens(&cursor_derived, stored.access(), stored.refresh())
                .is_err()
        );
        assert!(fleet_key_path(temporary.path()).exists());

        remove_fleet_key_and_tokens(temporary.path(), &store).expect("Fleet signout");

        assert!(store.load_fleet_tokens().expect("Fleet row").is_none());
        assert!(
            load_existing_fleet_key_bytes(temporary.path())
                .expect("Fleet key absence")
                .is_none()
        );
        assert_eq!(
            load_existing_cursor_key_bytes(temporary.path())
                .expect("cursor read after signout")
                .expect("cursor remains")
                .as_slice(),
            cursor_before.as_slice()
        );
    }

    #[tokio::test]
    async fn due_refresh_rotates_encrypted_generation_receipt_and_restart_state() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let credentials = FleetSetupCredentials::new(
            "fleet-old-access".to_owned(),
            "fleet-old-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            60,
        )
        .expect("setup credentials");
        persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            UNIX_EPOCH + std::time::Duration::from_secs(1_000),
        )
        .expect("persist due credentials");
        let initial = store
            .load_fleet_tokens()
            .expect("initial Fleet row")
            .expect("initial Fleet credentials");
        let input_generation = initial
            .credential_generation()
            .expect("initial credential generation");
        let initial_access_ciphertext = initial.access().to_vec();
        let initial_refresh_ciphertext = initial.refresh().to_vec();

        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake auth listener");
        let address = listener.local_addr().expect("fake auth address");
        let router = Router::new().route(
            "/oauth2/v3/token",
            post(move |headers: HeaderMap, body: Bytes| {
                let recorded = Arc::clone(&recorded);
                async move {
                    let valid = headers.get("content-type").is_some_and(|value| {
                        value.as_bytes() == b"application/x-www-form-urlencoded"
                    }) && body.as_ref()
                        == b"grant_type=refresh_token&client_id=fleet-client&refresh_token=fleet-old-refresh";
                    recorded.lock().expect("request ledger").push(valid);
                    (
                        StatusCode::OK,
                        [("content-type", "application/json")],
                        r#"{"access_token":"fleet-next-access","refresh_token":"fleet-next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
                    )
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("fake auth server");
        });
        let endpoint =
            url::Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
        let api = FleetAuthApi::for_fake_http(endpoint, std::time::Duration::from_secs(2))
            .expect("fake Fleet auth client");
        let mut manager =
            FleetAuthManager::from_store(store.clone(), temporary.path()).expect("Fleet manager");

        manager
            .refresh_if_due(&api, UNIX_EPOCH + std::time::Duration::from_secs(2_000))
            .await
            .expect("due refresh succeeds");
        assert_eq!(*requests.lock().expect("request ledger"), vec![true]);
        assert!(manager.access_token().expose() == "fleet-next-access");

        let rotated = store
            .load_fleet_tokens()
            .expect("rotated Fleet row")
            .expect("rotated Fleet credentials");
        let output_generation = rotated
            .credential_generation()
            .expect("rotated credential generation");
        assert_ne!(output_generation, input_generation);
        assert_ne!(rotated.access(), initial_access_ciphertext.as_slice());
        assert_ne!(rotated.refresh(), initial_refresh_ciphertext.as_slice());
        let encryption_key = load_existing_fleet_key_bytes(temporary.path())
            .expect("Fleet encryption key")
            .expect("Fleet key file");
        let successor =
            decrypt_legacy_owner_tokens(&encryption_key, rotated.access(), rotated.refresh())
                .expect("decrypt rotated credentials");
        assert!(successor.access_token() == "fleet-next-access");
        assert!(successor.refresh_token() == "fleet-next-refresh");

        let receipt = store
            .open()
            .expect("receipt catalogue")
            .query_row(
                "SELECT r.transport, r.operation, r.safety_class, r.precondition,
                        r.outcome, r.http_status, r.completed_at_ms IS NOT NULL,
                        b.input_credential_generation, b.output_credential_generation
                   FROM outbound_request_receipts AS r
                   JOIN fleet_refresh_receipt_bindings AS b ON b.receipt_id = r.id
                  ORDER BY r.id DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<u16>>(5)?,
                        row.get::<_, bool>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .expect("durable refresh receipt");
        assert_eq!(receipt.0, "fleet_api");
        assert_eq!(receipt.1, "token_refresh");
        assert_eq!(receipt.2, "non_wake_endpoint");
        assert_eq!(receipt.3, "not_required");
        assert_eq!(receipt.4, "success");
        assert_eq!(receipt.5, Some(200));
        assert!(receipt.6);
        assert_eq!(receipt.7, input_generation.to_string());
        assert_eq!(
            receipt.8.as_deref(),
            Some(output_generation.to_string().as_str())
        );
        assert!(
            !store
                .has_unresolved_fleet_refresh()
                .expect("resolved refresh")
        );

        drop(manager);
        drop(store);
        let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
        assert!(
            !restarted
                .has_unresolved_fleet_refresh()
                .expect("restart refresh state")
        );
        let restarted_manager = FleetAuthManager::from_store(restarted, temporary.path())
            .expect("restart loads successor");
        assert!(restarted_manager.access_token().expose() == "fleet-next-access");
        assert_eq!(restarted_manager.credential_generation, output_generation);

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn post_send_refresh_error_remains_fenced_after_restart() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let credentials = FleetSetupCredentials::new(
            "fleet-old-access".to_owned(),
            "fleet-old-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            60,
        )
        .expect("setup credentials");
        persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            UNIX_EPOCH + std::time::Duration::from_secs(1_000),
        )
        .expect("persist due credentials");
        let input_generation = store
            .load_fleet_tokens()
            .expect("Fleet row")
            .expect("Fleet credentials")
            .credential_generation()
            .expect("credential generation");

        let requests = Arc::new(Mutex::new(0_usize));
        let recorded = Arc::clone(&requests);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake auth listener");
        let address = listener.local_addr().expect("fake auth address");
        let router = Router::new().route(
            "/oauth2/v3/token",
            post(move || {
                let recorded = Arc::clone(&recorded);
                async move {
                    let mut requests = recorded.lock().expect("request ledger");
                    *requests += 1;
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        [("content-type", "application/json")],
                        r#"{"error":"temporarily_unavailable"}"#,
                    )
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("fake auth server");
        });
        let endpoint =
            url::Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
        let api = FleetAuthApi::for_fake_http(endpoint, std::time::Duration::from_secs(2))
            .expect("fake Fleet auth client");
        let mut manager =
            FleetAuthManager::from_store(store.clone(), temporary.path()).expect("Fleet manager");

        assert!(matches!(
            manager.refresh_now(&api, SystemTime::now()).await,
            Err(FleetCredentialError::Api(
                FleetApiError::ProviderHttpStatus { status: 500, .. }
            ))
        ));
        assert!(
            store
                .has_unresolved_fleet_refresh()
                .expect("fenced failure")
        );
        assert_eq!(
            store
                .load_fleet_tokens()
                .expect("Fleet row")
                .expect("Fleet credentials")
                .credential_generation(),
            Some(input_generation)
        );

        drop(manager);
        drop(store);
        let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
        assert!(restarted.has_unresolved_fleet_refresh().unwrap());
        assert!(matches!(
            FleetAuthManager::from_store(restarted.clone(), temporary.path()),
            Err(FleetCredentialError::RotationOutcomeUnknown)
        ));
        assert_eq!(*requests.lock().expect("request ledger"), 1);
        let receipt = restarted
            .open()
            .unwrap()
            .query_row(
                "SELECT outcome, completed_at_ms IS NULL
                   FROM outbound_request_receipts
                  WHERE transport = 'fleet_api' AND operation = 'token_refresh'
                  ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            )
            .unwrap();
        assert_eq!(receipt, ("started".to_owned(), true));

        server.abort();
        let _ = server.await;
    }

    #[test]
    fn only_definitively_unconsumed_refresh_failures_are_retryable() {
        assert_eq!(
            retryable_refresh_completion(&FleetApiError::RequestNotSent),
            Some(OutboundRequestCompletion {
                outcome: OutboundRequestOutcome::TransportError,
                http_status: None,
                retry_after_seconds: None,
            })
        );
        for ambiguous in [
            FleetApiError::RequestTimeout,
            FleetApiError::Transport,
            FleetApiError::HttpStatus(401),
            FleetApiError::HttpStatus(500),
            FleetApiError::ProviderHttpStatus {
                status: 500,
                error: "temporarily_unavailable".to_owned(),
                description: None,
            },
            FleetApiError::RateLimited {
                retry_after_seconds: 17,
            },
            FleetApiError::ResponseTooLarge,
            FleetApiError::ResponseRead,
            FleetApiError::InvalidResponse,
        ] {
            assert!(retryable_refresh_completion(&ambiguous).is_none());
        }
    }

    #[tokio::test]
    async fn invalidated_admission_blocks_due_refresh_before_transport() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let credentials = FleetSetupCredentials::new(
            "fleet-admission-access".to_owned(),
            "fleet-admission-refresh".to_owned(),
            "fleet-admission-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            60,
        )
        .expect("setup credentials");
        persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            UNIX_EPOCH + std::time::Duration::from_secs(1_000),
        )
        .expect("persist credentials");
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let mut manager = FleetAuthManager::from_store_for_admitted_user(
            store.clone(),
            temporary.path(),
            admission,
        )
        .expect("admitted Fleet manager");
        manager.mark_refresh_due();

        let requests = Arc::new(Mutex::new(0_usize));
        let recorded = Arc::clone(&requests);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake auth listener");
        let address = listener.local_addr().expect("fake auth address");
        let router = Router::new().route(
            "/oauth2/v3/token",
            post(move || {
                let recorded = Arc::clone(&recorded);
                async move {
                    *recorded.lock().expect("request ledger") += 1;
                    (
                        StatusCode::OK,
                        [("content-type", "application/json")],
                        r#"{"access_token":"next","refresh_token":"next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
                    )
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("fake auth server");
        });
        let endpoint =
            url::Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
        let api = FleetAuthApi::for_fake_http(endpoint, std::time::Duration::from_secs(2))
            .expect("fake auth client");

        let lock_path = temporary
            .path()
            .join(crate::user_lifetime_lock::LOCK_FILE_NAME);
        fs::remove_file(&lock_path).expect("remove admitted lock path");
        fs::write(&lock_path, b"").expect("replace admitted lock path");
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
            .expect("replacement lock mode");

        assert!(matches!(
            manager.refresh_if_due(&api, SystemTime::now()).await,
            Err(FleetCredentialError::SensitiveAccessUnavailable)
        ));
        assert_eq!(*requests.lock().expect("request ledger"), 0);
        assert!(
            !store
                .has_unresolved_fleet_refresh()
                .expect("no refresh began")
        );

        server.abort();
        let _ = server.await;
    }
}
