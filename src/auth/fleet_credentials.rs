// SPDX-License-Identifier: AGPL-3.0-only

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

    pub(crate) fn refresh_token(&self) -> Result<FleetRefreshToken, FleetCredentialError> {
        FleetRefreshToken::new(self.refresh_token.as_str()).map_err(Into::into)
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
/// changing their stored generation. This is the operational predicate: any
/// unresolved refresh, identified or not, fences the stored input and the
/// credential cannot collect. Production and GUI preflight use the diagnostic
/// admission of [`inspect_stored_fleet_credentials`] instead, which lets an
/// identified unresolved refresh start as a running reconnect stop.
pub fn validate_stored_fleet_credentials(
    store: &HubStore,
    data_dir: &Path,
) -> Result<(), FleetCredentialError> {
    validate_stored_fleet_credentials_after(store, data_dir, || {})
}

/// [`validate_stored_fleet_credentials`] with a seam between its first
/// unresolved-refresh check and the inspection, where a refresh can begin
/// concurrently. The inspection's own finding counts: a refresh it reports as
/// unresolved fences the input, so the operational answer is never success.
fn validate_stored_fleet_credentials_after(
    store: &HubStore,
    data_dir: &Path,
    after_first_check: impl FnOnce(),
) -> Result<(), FleetCredentialError> {
    if store.has_unresolved_fleet_refresh()? {
        return Err(FleetCredentialError::RotationOutcomeUnknown);
    }
    after_first_check();
    if inspect_stored_fleet_credentials(store, data_dir)?.unresolved_refresh {
        return Err(FleetCredentialError::RotationOutcomeUnknown);
    }
    Ok(())
}

/// What a read-only inspection of the stored Fleet credential found once its
/// independent integrity held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FleetCredentialInspection {
    /// An identified refresh whose outcome is unknown: a started token receipt
    /// bound to this pair's generation, with its input fence (C5 R47). The
    /// stored input is fenced, so the credential cannot collect, but the pair,
    /// key, ciphertext, generation, claims and scopes are all sound and the owner
    /// can reconnect.
    pub unresolved_refresh: bool,
}

/// Inspect persisted Fleet credentials for diagnostic admission, without
/// claiming refresh ownership, writing, or settling anything.
///
/// The pair, its key, its ciphertext, its generation, its access-token claims
/// and its collection scopes are validated independently of any unresolved
/// refresh. Only after they hold can an unresolved refresh be reported, and only
/// an identified one (see [`FleetCredentialInspection`]). A generation mismatch
/// is a failure in its own right: `RotationOutcomeUnknown` is never accepted
/// blindly, because the same error also means a mismatched pair, and an
/// unresolved receipt that does not identify this pair is refused as well.
pub fn inspect_stored_fleet_credentials(
    store: &HubStore,
    data_dir: &Path,
) -> Result<FleetCredentialInspection, FleetCredentialError> {
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
    let unresolved_refresh = store.identified_unresolved_fleet_refresh()?;
    if !unresolved_refresh && store.has_unresolved_fleet_refresh()? {
        return Err(FleetCredentialError::RotationOutcomeUnknown);
    }
    Ok(FleetCredentialInspection { unresolved_refresh })
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
    /// Startup found an identified unresolved refresh (C5 R47, C3 R47, B2
    /// clarification 3): the stored pair is sound but its refresh input is fenced,
    /// so this manager hands out no token, refreshes nothing and every use ends in
    /// the unknown-outcome error the loops turn into the reconnect stop. It holds
    /// for the life of the manager: the owner's external credential write and a
    /// restart are the recovery, never a replay of the fenced input.
    input_fenced: bool,
    /// The token exchange that has started and not yet been applied (C3 R8, Codex
    /// R06). It runs in a task of its own, so dropping the caller (a shutdown that
    /// drops the collection future) does not cancel it: if Tesla rotated the refresh
    /// token before the drop, the successor is still written. The handle stays here,
    /// so the next caller, or a graceful shutdown, awaits it and applies its outcome.
    in_flight: Option<tokio::task::JoinHandle<FleetExchange>>,
    #[cfg(unix)]
    admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
    /// The run's irreversible loss fence (B2 fix round, P3b fix 1, R-P3bF-1), shared
    /// by the supervisor: once it answers `true` this manager hands out nothing and
    /// its exchange jobs persist nothing, whatever the admission says afterwards.
    loss: Option<crate::credentials::LossObserver>,
    /// Test only: counts the entries of the exchange jobs' encryption step, the
    /// one that follows the response (setup encryption is not counted).
    #[cfg(test)]
    encryption_entries: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

/// What a detached token exchange hands back.
enum FleetExchange {
    /// Tesla issued a pair and it is stored under `output_generation`.
    Refreshed {
        access_token: FleetAccessToken,
        refresh_token: FleetRefreshToken,
        schedule: FleetRefreshSchedule,
        output_generation: Uuid,
    },
    /// The exchange failed. `terminal` fences the manager: the refresh token may have
    /// been consumed, so nothing further is sent with it.
    Failed {
        error: FleetCredentialError,
        terminal: bool,
    },
}

/// Everything a token exchange needs, owned, so it can outlive its caller.
struct FleetExchangeJob {
    store: HubStore,
    api: FleetAuthApi,
    encryption_key: Zeroizing<Vec<u8>>,
    client_id: FleetClientId,
    refresh_token: FleetRefreshToken,
    region: FleetRegion,
    generation: Uuid,
    receipt_id: crate::db::OutboundRequestReceiptId,
    now: SystemTime,
    /// The manager's retained admission (C3 R47, CL-15): the job outlives its
    /// caller, so it checks the admission itself before it encrypts and writes.
    #[cfg(unix)]
    admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
    /// The manager's loss fence: the job outlives the supervisor's observation of the
    /// loss, so it consults the fence itself before every store mutation.
    loss: Option<crate::credentials::LossObserver>,
    #[cfg(test)]
    encryption_entries: Option<Arc<std::sync::atomic::AtomicUsize>>,
    #[cfg(test)]
    scripted: Option<ScriptedExchange>,
    /// Test only: a failure the request ends in once its hold is released (the
    /// request never went out, a refusal, ...), in place of the network.
    #[cfg(test)]
    scripted_failure: Option<ScriptedFailure>,
}

/// Test only: the answer a job takes in place of the network, and the hold it
/// waits out first. The rest of the job, the validation, the admission check, the
/// encryption and the guarded write, is the real one.
#[cfg(test)]
pub(crate) struct ScriptedExchange {
    pub(crate) refreshed: crate::fleet_api::FleetRefreshedTokens,
    pub(crate) hold: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
}

/// Test only: the failure a job's request ends in, and the hold it waits out first.
#[cfg(test)]
pub(crate) struct ScriptedFailure {
    pub(crate) error: FleetApiError,
    pub(crate) hold: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
}

impl FleetExchangeJob {
    /// The answer to the token request.
    async fn request(&mut self) -> Result<crate::fleet_api::FleetRefreshedTokens, FleetApiError> {
        #[cfg(test)]
        if let Some(scripted) = self.scripted.take() {
            scripted.hold.await;
            return Ok(scripted.refreshed);
        }
        #[cfg(test)]
        if let Some(scripted) = self.scripted_failure.take() {
            scripted.hold.await;
            return Err(scripted.error);
        }
        self.api.refresh(&self.client_id, &self.refresh_token).await
    }

    /// The retained admission, checked at the last point before the pair is
    /// encrypted and written. Nothing is awaited between this check and the end of
    /// the job, so a denial that arrived while the request was out is met here (a
    /// boundary check, not protection against a denial during the synchronous work).
    fn assert_sensitive_access(&self) -> Result<(), FleetCredentialError> {
        if self.loss.as_ref().is_some_and(|lost| lost()) {
            return Err(FleetCredentialError::SensitiveAccessUnavailable);
        }
        #[cfg(unix)]
        if let Some(admission) = &self.admission {
            admission
                .assert_sensitive_access()
                .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        }
        Ok(())
    }

    /// The encrypted row of a pair. A test counts the entries of this step.
    fn encrypt(
        &self,
        refreshed: &crate::fleet_api::FleetRefreshedTokens,
        schedule: FleetRefreshSchedule,
    ) -> Result<FleetTokenStore, FleetCredentialError> {
        #[cfg(test)]
        if let Some(entries) = &self.encryption_entries {
            entries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        encrypt_store(
            &self.encryption_key,
            refreshed.access_token.expose(),
            refreshed.refresh_token.expose(),
            self.client_id.as_str(),
            self.region,
            schedule,
        )
    }

    /// The request, the schedule, the encrypted pair and its write: the part of a
    /// refresh that must finish once the request has gone out.
    async fn run(mut self) -> FleetExchange {
        let failed = |error, terminal| FleetExchange::Failed { error, terminal };
        let refreshed = match self.request().await {
            Ok(refreshed) => refreshed,
            Err(error) => {
                if let Some(completion) = retryable_refresh_completion(&error) {
                    // Completing the receipt is a store write: after the loss it is not
                    // made, and the attempt stays unresolved for the next start (R-P3bF-1).
                    if let Err(denied) = self.assert_sensitive_access() {
                        return failed(denied, true);
                    }
                    if self
                        .store
                        .complete_retryable_fleet_refresh_failure(
                            self.receipt_id,
                            self.generation,
                            &completion,
                        )
                        .is_err()
                    {
                        return failed(FleetCredentialError::RotationOutcomeUnknown, true);
                    }
                    return failed(FleetCredentialError::Api(error), false);
                }
                return failed(FleetCredentialError::Api(error), true);
            }
        };
        let schedule = match refresh_schedule(self.now, refreshed.expires_in_seconds) {
            Ok(schedule) => schedule,
            Err(error) => return failed(error, true),
        };
        // CL-15: a denial of the retained admission skips the encryption and the
        // write and drops the successor. The receipt, its binding and the input
        // fence stay as they are, because the request went out and the outcome is
        // unknown: the failure is terminal and sensitive.
        if let Err(error) = self.assert_sensitive_access() {
            return failed(error, true);
        }
        let stored = match self.encrypt(&refreshed, schedule) {
            Ok(stored) => stored,
            Err(error) => return failed(error, true),
        };
        let Some(output_generation) = stored.credential_generation() else {
            return failed(FleetCredentialError::RotationOutcomeUnknown, true);
        };
        if let Err(error) = self.store.complete_fleet_refresh(
            self.receipt_id,
            self.generation,
            output_generation,
            &stored,
        ) {
            return failed(FleetCredentialError::Store(error), true);
        }
        FleetExchange::Refreshed {
            access_token: refreshed.access_token,
            refresh_token: refreshed.refresh_token,
            schedule,
            output_generation,
        }
    }
}

impl FleetAuthManager {
    pub(crate) fn mark_refresh_due(&mut self) {
        self.next_refresh_at = 0;
    }

    /// The generation of the stored pair: it changes exactly when a refresh
    /// installs a new pair. It is an identifier, never a secret, so the
    /// collector can tell whether a tick's request refreshed the token (C3 R47).
    pub(crate) const fn credential_generation(&self) -> Uuid {
        self.credential_generation
    }

    fn from_store_inner(
        store: HubStore,
        data_dir: &Path,
        loss: Option<crate::credentials::LossObserver>,
    ) -> Result<Self, FleetCredentialError> {
        Self::build(store, data_dir, false, loss)
    }

    /// Build the manager. With `fenced` the unresolved refresh the inspection
    /// identified is accepted, nothing is bound or written, and the manager is
    /// fenced (see `input_fenced`); otherwise any unresolved refresh refuses.
    ///
    /// `loss` is the run's guard (B2 fix round, P3b fix 3, R-P3bF3-1): it is consulted
    /// immediately before the generation bind, with no await between, and the manager
    /// keeps it as its loss observer from then on.
    fn build(
        store: HubStore,
        data_dir: &Path,
        fenced: bool,
        loss: Option<crate::credentials::LossObserver>,
    ) -> Result<Self, FleetCredentialError> {
        if !fenced && store.has_unresolved_fleet_refresh()? {
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
        if !fenced {
            if loss.as_ref().is_some_and(|lost| lost()) {
                return Err(FleetCredentialError::SensitiveAccessUnavailable);
            }
            store.bind_fleet_credential_generation(&stored, credential_generation)?;
        }
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
            input_fenced: fenced,
            in_flight: None,
            #[cfg(unix)]
            admission: None,
            loss,
            #[cfg(test)]
            encryption_entries: None,
        })
    }

    /// The manager of the supervised collector's start (C3 R47, B2 clarification
    /// 3): an identified unresolved refresh, found by the read-only inspection
    /// after the pair, key, ciphertext, generation, claims and scopes proved sound,
    /// yields a fenced manager, so the run starts with an active lease and the
    /// reconnect attention and sends nothing. Every other case, including an
    /// unresolved refresh the inspection does not identify and every independent
    /// failure, goes the ordinary way and keeps its ordinary error. The low-level
    /// constructors still refuse an unresolved refresh.
    #[cfg(unix)]
    pub(crate) fn from_store_for_supervised_start(
        store: HubStore,
        data_dir: &Path,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
        loss: Option<crate::credentials::LossObserver>,
    ) -> Result<Self, FleetCredentialError> {
        admission
            .assert_sensitive_access()
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        admission
            .assert_store_path(data_dir)
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        let identified = matches!(
            inspect_stored_fleet_credentials(&store, data_dir),
            Ok(FleetCredentialInspection {
                unresolved_refresh: true
            })
        );
        if !identified {
            return Self::from_store_for_admitted_user(store, data_dir, admission, loss);
        }
        let mut manager = Self::build(store, data_dir, true, loss)?;
        manager.admission = Some(admission);
        Ok(manager)
    }

    /// Hand the run's irreversible loss fence to the manager and its detached
    /// exchange jobs (B2 fix round, P3b fix 1, R-P3bF-1): once the supervisor has
    /// observed the loss of the retained admission, no token is handed out and no job
    /// encrypts, writes or completes a receipt, even if the admission passes again.
    pub(crate) fn share_loss_fence(&mut self, observer: crate::credentials::LossObserver) {
        self.loss = Some(observer);
    }

    /// Whether this manager was built over an identified unresolved refresh and
    /// is fenced for good.
    pub(crate) const fn is_input_fenced(&self) -> bool {
        self.input_fenced
    }

    #[cfg(unix)]
    pub(crate) fn from_store_for_admitted_user(
        store: HubStore,
        data_dir: &Path,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
        loss: Option<crate::credentials::LossObserver>,
    ) -> Result<Self, FleetCredentialError> {
        admission
            .assert_sensitive_access()
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        admission
            .assert_store_path(data_dir)
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        let mut manager = Self::from_store_inner(store, data_dir, loss)?;
        manager.admission = Some(admission);
        Ok(manager)
    }

    #[cfg(test)]
    pub(crate) fn from_store(
        store: HubStore,
        data_dir: &Path,
    ) -> Result<Self, FleetCredentialError> {
        Self::from_store_inner(store, data_dir, None)
    }

    #[cfg(test)]
    pub(crate) fn access_token(&self) -> &FleetAccessToken {
        &self.access_token
    }

    fn assert_sensitive_access(&self) -> Result<(), FleetCredentialError> {
        if self.loss.as_ref().is_some_and(|lost| lost()) {
            return Err(FleetCredentialError::SensitiveAccessUnavailable);
        }
        #[cfg(unix)]
        if let Some(admission) = &self.admission {
            admission
                .assert_sensitive_access()
                .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        }
        Ok(())
    }

    /// A fenced manager (see `input_fenced`) hands out nothing and sends nothing.
    fn assert_input_unfenced(&self) -> Result<(), FleetCredentialError> {
        if self.input_fenced {
            return Err(FleetCredentialError::RotationOutcomeUnknown);
        }
        Ok(())
    }

    /// Revalidate retained user admission at the last bearer-use boundary.
    pub(crate) fn access_token_for_sensitive_use(
        &self,
    ) -> Result<&FleetAccessToken, FleetCredentialError> {
        self.assert_sensitive_access()?;
        self.assert_input_unfenced()?;
        Ok(&self.access_token)
    }

    /// The live refresh token, behind the same admission check as the access
    /// token. It exists only so a provider response that echoes it can be
    /// scrubbed before storage; it is never sent, logged or formatted.
    pub(crate) fn refresh_token_for_sensitive_use(
        &self,
    ) -> Result<&FleetRefreshToken, FleetCredentialError> {
        self.assert_sensitive_access()?;
        self.assert_input_unfenced()?;
        Ok(&self.refresh_token)
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
        self.assert_input_unfenced()?;
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
        self.assert_input_unfenced()?;
        if self.in_flight.is_none() {
            let job = self.begin_exchange(api, now)?;
            // Detached: the exchange finishes, and persists its successor, even when
            // this call is dropped before it returns.
            self.in_flight = Some(tokio::spawn(job.run()));
        }
        self.finish_exchange().await
    }

    /// Reserve the single-use input and build the job that sends and stores.
    fn begin_exchange(
        &mut self,
        api: &FleetAuthApi,
        now: SystemTime,
    ) -> Result<FleetExchangeJob, FleetCredentialError> {
        // The reservation is a store mutation: the run's fence and the retained
        // admission are consulted immediately before it (B2 fix round, P3b fix 2,
        // R-P3bF2-1, Opus N2).
        self.assert_sensitive_access()?;
        let receipt_id = self
            .store
            .begin_fleet_refresh(self.credential_generation)
            .map_err(|error| {
                self.refresh_terminal = true;
                FleetCredentialError::Store(error)
            })?;
        // A denial that arrives between the reservation and the job leaves the receipt
        // as it is: the cancellation would be a second receipt write after the loss,
        // and the started receipt, its binding and its fence are what the next start
        // reads (R-P3bF2-1; the request was not sent, and no later write is made).
        self.assert_sensitive_access()?;
        Ok(FleetExchangeJob {
            store: self.store.clone(),
            api: api.clone(),
            encryption_key: Zeroizing::new(self.encryption_key.to_vec()),
            client_id: self.client_id.clone(),
            refresh_token: FleetRefreshToken::new(self.refresh_token.expose().to_owned()).map_err(
                |_| {
                    self.refresh_terminal = true;
                    FleetCredentialError::RotationOutcomeUnknown
                },
            )?,
            region: self.region,
            generation: self.credential_generation,
            receipt_id,
            now,
            #[cfg(unix)]
            admission: self.admission.clone(),
            loss: self.loss.clone(),
            #[cfg(test)]
            encryption_entries: self.encryption_entries.clone(),
            #[cfg(test)]
            scripted: None,
            #[cfg(test)]
            scripted_failure: None,
        })
    }

    /// Test only: start a detached exchange that takes `scripted` in place of the
    /// network, through the same reservation and job as `refresh_now`.
    #[cfg(test)]
    pub(crate) fn start_scripted_exchange(
        &mut self,
        api: &FleetAuthApi,
        now: SystemTime,
        scripted: ScriptedExchange,
    ) -> Result<(), FleetCredentialError> {
        self.assert_sensitive_access()?;
        self.assert_input_unfenced()?;
        let mut job = self.begin_exchange(api, now)?;
        job.scripted = Some(scripted);
        self.in_flight = Some(tokio::spawn(job.run()));
        Ok(())
    }

    /// Test only: start a detached exchange whose request ends in `scripted`'s
    /// failure once its hold ends, through the same reservation and job as
    /// `refresh_now`.
    #[cfg(test)]
    pub(crate) fn start_scripted_failure(
        &mut self,
        api: &FleetAuthApi,
        now: SystemTime,
        scripted: ScriptedFailure,
    ) -> Result<(), FleetCredentialError> {
        self.assert_sensitive_access()?;
        self.assert_input_unfenced()?;
        let mut job = self.begin_exchange(api, now)?;
        job.scripted_failure = Some(scripted);
        self.in_flight = Some(tokio::spawn(job.run()));
        Ok(())
    }

    /// Test only: count the entries of the encryption step of every exchange job
    /// this manager starts from now on.
    #[cfg(test)]
    pub(crate) fn observe_exchange_encryption(&mut self) -> Arc<std::sync::atomic::AtomicUsize> {
        let entries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        self.encryption_entries = Some(Arc::clone(&entries));
        entries
    }

    /// Test only: abort the detached exchange and wait for its task to end.
    #[cfg(test)]
    pub(crate) async fn abort_exchange_for_test(&mut self) {
        if let Some(handle) = self.in_flight.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    /// Awaits the exchange that has started, if any, and applies its outcome. A
    /// graceful shutdown calls it so that a refresh Tesla has answered is stored
    /// before the clients are dropped. Cancel-safe: the handle stays in the manager
    /// until the exchange has been applied.
    pub(crate) async fn finish_exchange(&mut self) -> Result<(), FleetCredentialError> {
        let Some(handle) = self.in_flight.as_mut() else {
            return Ok(());
        };
        let outcome = handle.await;
        self.in_flight = None;
        match outcome {
            Ok(FleetExchange::Refreshed {
                access_token,
                refresh_token,
                schedule,
                output_generation,
            }) => {
                self.access_token = access_token;
                self.refresh_token = refresh_token;
                self.expires_at = schedule.expires_at;
                self.next_refresh_at = schedule.next_refresh_at;
                self.credential_generation = output_generation;
                Ok(())
            }
            Ok(FleetExchange::Failed { error, terminal }) => {
                self.refresh_terminal |= terminal;
                Err(error)
            }
            Err(_task_failed) => {
                // The task panicked or was aborted: whether the token was consumed
                // is unknown, so the manager is fenced.
                self.refresh_terminal = true;
                Err(FleetCredentialError::RotationOutcomeUnknown)
            }
        }
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
        crate::platform::file_attempts::note(&temporary);
        crate::platform::file_attempts::note(&destination);
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
        crate::platform::file_attempts::note(&temporary);
        crate::platform::file_attempts::note(&destination);
        match renameat_with(CWD, &temporary, CWD, &destination, RenameFlags::NOREPLACE) {
            Ok(()) => {
                sync_directory(&secrets)?;
                Ok(key)
            }
            Err(Errno::EXIST) => {
                crate::platform::file_attempts::note(&temporary);
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
        crate::platform::file_attempts::note(&temporary);
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
            crate::platform::file_attempts::note(path);
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
    crate::platform::file_attempts::note(&marker);
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
    crate::platform::file_attempts::note(&marker);
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
            crate::platform::file_attempts::note(&path);
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
    crate::platform::file_attempts::note(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|source| fleet_key_io("creating Fleet credential key", path, source))?;
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .map_err(|source| fleet_key_io("protecting Fleet credential key", path, source))?;
    crate::platform::file_attempts::note(path);
    file.write_all(bytes)
        .map_err(|source| fleet_key_io("writing Fleet credential key", path, source))?;
    crate::platform::file_attempts::note(path);
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
    crate::platform::file_attempts::note(path);
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
    /// Whether this is the denial of the retained runtime admission itself (C3 R47,
    /// B2 clarification 3). A credential that is missing, lacks its scopes or claims,
    /// cannot be decrypted or has an unknown refresh outcome is a credential
    /// condition with the admission intact, and is not this.
    pub(crate) const fn is_admission_loss(&self) -> bool {
        matches!(self, Self::SensitiveAccessUnavailable)
    }
}

#[cfg(test)]
#[path = "fleet_credentials/tests.rs"]
mod tests;
