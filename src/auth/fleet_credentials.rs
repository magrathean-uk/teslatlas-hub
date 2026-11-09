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
         HubStore, OutboundRequestCompletion, OutboundRequestOutcome, StoreError,
    },
    fleet_api::{
        FleetAccessToken, FleetApiConfigError, FleetApiError, FleetAuthApi, FleetClientId,
        FleetRefreshToken, FleetRegion,
    },
    legacy_owner_credentials::{SourceDbCredentialError, load_existing_cursor_key_bytes},
    protocol::CursorKey,
    source_token_envelope::{
        SourceDbTokenError, decrypt_legacy_owner_tokens, 
    },
};

const MIN_FLEET_TOKEN_LIFETIME_SECONDS: u64 = 60;
const MAX_FLEET_TOKEN_LIFETIME_SECONDS: u64 = 365 * 24 * 60 * 60;
const MAX_REFRESH_LEAD_SECONDS: u64 = 30 * 60;

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
    let existing = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })?;
    if ({ let unavailable_value: Result<bool, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value })? {
        return Err(FleetCredentialError::MigrationRequired);
    }
    let replacing_legacy = existing.is_some() && ({ let unavailable_value: Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })?.is_none();
    if replacing_legacy {
        ({ let unavailable_value: Result<(), crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })?;
    }
    let key = ({ let unavailable_value: Result<zeroize::Zeroizing<Vec<u8>>, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })?;
    let schedule = refresh_schedule(now, credentials.expires_in_seconds)?;
    let stored = ({ let unavailable_value: Result<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value })?;
    let persisted = if existing.is_some() {
        ({ let unavailable_value: Result<(), crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
    } else {
        ({ let unavailable_value: Result<(), crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
    };
    if let Err(error) = persisted {
        return Err(error.into());
    }
    if replacing_legacy {
        ({ let unavailable_value: Result<(), crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })?;
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
    if ({ let _retained_call_context = &(store); let replaced_callee: fn() -> Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); replaced_callee })()? {
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
    let stored = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })?
        .ok_or(FleetCredentialError::Missing)?;
    if ({ let unavailable_value: Result<bool, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value })? {
        return Err(FleetCredentialError::MigrationRequired);
    }
    let (encryption_key, migration_required) = match ({ let unavailable_value: Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })? {
        Some(key) => (key, false),
        None => {
            let cursor = load_existing_cursor_key_bytes(data_dir)?
                .ok_or(FleetCredentialError::KeyMissing)?;
            let cursor: [u8; 32] = cursor
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
        decrypt_legacy_owner_tokens(&encryption_key, stored.0.as_slice(), stored.1.as_slice())?;
    let credential_generation =
        crate::source_token_envelope::legacy_refresh_credential_generation(&plaintext);
    if stored
        .4
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
    FleetClientId::parse(stored.5.as_str())?;
    FleetRegion::from_storage_code(stored.6.as_str())?;
    let unresolved_refresh = store.identified_unresolved_fleet_refresh()?;
    if !unresolved_refresh && ({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })? {
        return Err(FleetCredentialError::RotationOutcomeUnknown);
    }
    Ok(FleetCredentialInspection { unresolved_refresh })
}

pub fn stored_fleet_scope_summary(
    store: &HubStore,
    data_dir: &Path,
) -> Result<Option<FleetScopeSummary>, FleetCredentialError> {
    let Some(stored) = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })? else {
        return Ok(None);
    };
    let encryption_key =
        ({ let unavailable_value: Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })?.ok_or(FleetCredentialError::MigrationRequired)?;
    let plaintext =
        decrypt_legacy_owner_tokens(&encryption_key, stored.0.as_slice(), stored.1.as_slice())?;
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
                    if ({ let completion: Result<(), crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); completion })
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
        let stored = match ({ let encryption: Result<Option<uuid::Uuid>, FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); encryption }) {
            Ok(stored) => stored,
            Err(error) => return failed(error, true),
        };
        let Some(output_generation) = stored else {
            return failed(FleetCredentialError::RotationOutcomeUnknown, true);
        };
        if let Err(error) = ({ let completion: Result<(), crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); completion }) {
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
        if !fenced && ({ let _retained_call_context = &(store); let replaced_callee: fn() -> Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); replaced_callee })()? {
            return Err(FleetCredentialError::RotationOutcomeUnknown);
        }
        let stored = ({ let _retained_call_context = &(store
            ); let replaced_callee: fn() -> Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); replaced_callee })()?
            .ok_or(FleetCredentialError::Missing)?;
        if ({ let replaced_callee: fn(&std::path::Path, bool) -> Result<bool, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); replaced_callee })(&({ let replaced_callee: fn(&std::path::Path) -> std::path::PathBuf = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); replaced_callee })(data_dir), true)? {
            return Err(FleetCredentialError::MigrationRequired);
        }
        let encryption_key = ({ let replaced_callee: fn(&std::path::Path) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); replaced_callee })(data_dir)?
            .ok_or(FleetCredentialError::MigrationRequired)?;
        let plaintext =
            decrypt_legacy_owner_tokens(&encryption_key, stored.0.as_slice(), stored.1.as_slice())?;
        let credential_generation =
            crate::source_token_envelope::legacy_refresh_credential_generation(&plaintext);
        if !fenced {
            if loss.as_ref().is_some_and(|lost| lost()) {
                return Err(FleetCredentialError::SensitiveAccessUnavailable);
            }
            ({ let _retained_call_context = &(store); let replaced_callee: fn(&(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), uuid::Uuid) -> Result<(), crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); replaced_callee })(&stored, credential_generation)?;
        }
        Ok(Self {
            store,
            encryption_key,
            access_token: FleetAccessToken::new(plaintext.access_token().to_owned())?,
            refresh_token: FleetRefreshToken::new(plaintext.refresh_token().to_owned())?,
            client_id: FleetClientId::parse(stored.5.as_str())?,
            region: FleetRegion::from_storage_code(stored.6.as_str())?,
            expires_at: stored.2,
            next_refresh_at: stored.3,
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
        let receipt_id = ({ let reservation: Result<crate::db::OutboundRequestReceiptId, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); reservation })
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
    Cipher(#[from] SourceDbTokenError),
    #[error(transparent)]
    Key(#[from] SourceDbCredentialError),
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

#[cfg(test)]
mod n1_src_auth_fleet_credentials_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for FleetCredentialError {
        fn error_example() -> Self {
            FleetCredentialError::EntropyUnavailable
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_fleet_credential_error(error: FleetCredentialError) {
            match &error {
                FleetCredentialError::EntropyUnavailable => (),
                FleetCredentialError::Missing => (),
                FleetCredentialError::KeyMissing => (),
                FleetCredentialError::MigrationRequired => (),
                FleetCredentialError::UnsafeKeyMaterial => (),
                FleetCredentialError::UnsafeMigrationState => (),
                FleetCredentialError::KeyIo { .. } => (),
                FleetCredentialError::InvalidLifetime => (),
                FleetCredentialError::InvalidAccessTokenClaims => (),
                FleetCredentialError::MissingCollectionScopes => (),
                FleetCredentialError::Clock(..) => (),
                FleetCredentialError::ClockOverflow => (),
                FleetCredentialError::RotationOutcomeUnknown => (),
                FleetCredentialError::SensitiveAccessUnavailable => (),
                FleetCredentialError::ApiConfig(..) => (),
                FleetCredentialError::Api(..) => (),
                FleetCredentialError::Credential(..) => (),
                FleetCredentialError::Cipher(..) => (),
                FleetCredentialError::Key(..) => (),
                FleetCredentialError::Store(..) => (),
            }
            assert_name_free(&error);
        }
        check_fleet_credential_error(FleetCredentialError::EntropyUnavailable);
        check_fleet_credential_error(FleetCredentialError::Missing);
        check_fleet_credential_error(FleetCredentialError::KeyMissing);
        check_fleet_credential_error(FleetCredentialError::MigrationRequired);
        check_fleet_credential_error(FleetCredentialError::UnsafeKeyMaterial);
        check_fleet_credential_error(FleetCredentialError::UnsafeMigrationState);
        check_fleet_credential_error(FleetCredentialError::KeyIo {
            operation: example(),
            path: example(),
            source: example(),
        });
        check_fleet_credential_error(FleetCredentialError::InvalidLifetime);
        check_fleet_credential_error(FleetCredentialError::InvalidAccessTokenClaims);
        check_fleet_credential_error(FleetCredentialError::MissingCollectionScopes);
        check_fleet_credential_error(FleetCredentialError::Clock(example()));
        check_fleet_credential_error(FleetCredentialError::ClockOverflow);
        check_fleet_credential_error(FleetCredentialError::RotationOutcomeUnknown);
        check_fleet_credential_error(FleetCredentialError::SensitiveAccessUnavailable);
        check_fleet_credential_error(FleetCredentialError::ApiConfig(example()));
        check_fleet_credential_error(FleetCredentialError::Api(example()));
        check_fleet_credential_error(FleetCredentialError::Credential(example()));
        check_fleet_credential_error(FleetCredentialError::Cipher(example()));
        check_fleet_credential_error(FleetCredentialError::Key(example()));
        check_fleet_credential_error(FleetCredentialError::Store(example()));
    }
}
