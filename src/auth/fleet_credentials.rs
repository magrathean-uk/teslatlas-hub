// SPDX-License-Identifier: AGPL-3.0-only

//! Encrypted persistence and one resident refresh owner for Fleet OAuth.

use crate::auth::credential_vault::{CredentialVault, CredentialStoreError};
use crate::auth::credential_envelope::{CredentialSlot, RefreshOwner};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
#[cfg(test)]
use std::{fs, os::unix::fs::PermissionsExt};
use std::{
    path::Path,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    db::{
         HubStore, OutboundRequestCompletion, OutboundRequestOutcome, StoreError,
    },
    fleet_api::{
        FleetAccessToken, FleetApiConfigError, FleetApiError, FleetAuthApi, FleetClientId,
        FleetRefreshToken, FleetRegion,
    },
    legacy_owner_credentials::SourceDbCredentialError,
    source_token_envelope::SourceDbTokenError,
};
#[cfg(test)]
use crate::legacy_owner_credentials::load_existing_cursor_key_bytes;

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

/// R58's public entry bridge. Source credentials are disabled (M02-T02): a
/// raw Fleet token pair is never stored, so this refuses before the vault is
/// touched or a token is copied.
pub fn persist_fleet_setup_credentials(
    entry: &crate::CredentialVaultEntry, _data_dir: &Path,
    credentials: &FleetSetupCredentials, now: SystemTime,
) -> Result<(), FleetCredentialError> {
    let _ = (entry, credentials, now);
    Err(CredentialStoreError::SourceCredentialDisabled(CredentialSlot::FleetApi).into())
}

impl FleetSetupCredentials {
    /// Fixture seeding only: the supplied row an older Hub could hold.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn credential_entry(&self, now: SystemTime) -> Result<crate::auth::credential_vault::CredentialEntry, FleetCredentialError> {
        let schedule = refresh_schedule(now, self.expires_in_seconds)?;
        let mut entry = crate::auth::credential_vault::CredentialEntry::fleet_supplied(
            Zeroizing::new(self.access_token.as_str().to_owned()), Zeroizing::new(self.refresh_token.as_str().to_owned()),
            self.client_id.clone(), self.region, self.expires_in_seconds)?;
        entry.set_schedule(schedule.expires_at, schedule.next_refresh_at);
        Ok(entry)
    }
}

/// Car discovery after a Fleet sign-in (M09-T09): run by `setup-fleet` right
/// after its exchange stored the Hub's own Fleet pair, inside the same Open
/// window and with the same vault. Order, so that a refusal opens no
/// connection and writes nothing:
/// 1. local checks: the configured provider is `fleet` and the selection is
///    usable;
/// 2. only the vault's own Fleet pair is used (origin `fleet_oauth`, refresh
///    owner the Hub, with its client id and region), read once, never
///    refreshed here;
/// 3. the window's egress gate is asked for Fleet (`window_closed` unless a
///    window is Open); the list asks again for its own permit before its send;
/// 4. one vehicle list over the Fleet API (counted in the request ledger),
///    no wake, no vehicle-data read and no command; the chosen cars are then
///    recorded.
///
/// Every failure is one of `setup`'s fixed `discovery_*` codes (or
/// `window_closed`); no token, VIN or name appears in an error or a log.
/// Returns how many cars are configured.
pub async fn discover_fleet_cars(
    entry: &crate::CredentialVaultEntry,
    store: &HubStore,
    config: &crate::config::HubConfig,
    admission: &crate::hub_user_process::AdmittedUserHub,
    selection: crate::collection::collector::OwnerCarSelection,
) -> Result<usize, crate::auth::credential_sign_in::CredentialCommandError> {
    use crate::auth::credential_sign_in::CredentialCommandError;
    use crate::collection::collector::{OwnerCarSelection, setup_fleet_vehicle, setup_fleet_vehicles};
    use crate::fleet_api::egress::{self, PermitKind, Provider};
    if config.collector.provider != crate::config::CollectorProvider::Fleet {
        return Err(CredentialCommandError::new("discovery_settings_invalid"));
    }
    if matches!(selection, OwnerCarSelection::Vehicle(id) if id <= 0) {
        return Err(CredentialCommandError::new("discovery_selection_invalid"));
    }
    let credentials = own_fleet_pair(entry)?;
    // After every local check and before the list: a closed window refuses
    // here with no connection.
    drop(egress::ask(credentials.region.base_url(), Provider::Fleet, PermitKind::Request)
        .map_err(|_| CredentialCommandError::new(crate::auth::credential_sign_in::WINDOW_CLOSED))?);
    let configured = match selection {
        OwnerCarSelection::All => setup_fleet_vehicles(store, config, &credentials, admission).await.map(|report| report.vehicles.len()),
        OwnerCarSelection::Vehicle(id) => setup_fleet_vehicle(store, config, &credentials, admission, Some(id)).await.map(|_| 1),
        OwnerCarSelection::Implicit => setup_fleet_vehicle(store, config, &credentials, admission, None).await.map(|_| 1),
    };
    let base = credentials.region.base_url();
    drop(credentials);
    match configured {
        Ok(vehicles_configured) => {
            tracing::info!(vehicles_configured, "Fleet setup configured its cars");
            Ok(vehicles_configured)
        }
        Err(error) => {
            let code = fleet_discovery_code(&error, base);
            tracing::warn!(code, "Fleet setup discovery failed");
            Err(CredentialCommandError::new(code))
        }
    }
}

/// The Hub's own Fleet pair from this vault, for one list: origin
/// `fleet_oauth` from the Hub's own sign-in, with the Hub as refresher, its
/// client id and region. Any other pair, or none, is refused.
fn own_fleet_pair(entry: &crate::CredentialVaultEntry) -> Result<FleetSetupCredentials, crate::auth::credential_sign_in::CredentialCommandError> {
    use crate::auth::credential_envelope::CredentialOrigin;
    let unavailable = || crate::auth::credential_sign_in::CredentialCommandError::new("discovery_pair_unavailable");
    let pair = entry.vault().load(CredentialSlot::FleetApi).map_err(|_| unavailable())?.ok_or_else(unavailable)?;
    if pair.origin != Some(CredentialOrigin::FleetOauth) || pair.refresh_owner != RefreshOwner::Hub { return Err(unavailable()); }
    let region = FleetRegion::from_storage_code(pair.region.as_deref().unwrap_or_default()).map_err(|_| unavailable())?;
    let now_s = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    // The lifetime only bounds the value; the list never refreshes.
    let lifetime = u64::try_from(pair.expires_at).unwrap_or(0).saturating_sub(now_s)
        .clamp(MIN_FLEET_TOKEN_LIFETIME_SECONDS, MAX_FLEET_TOKEN_LIFETIME_SECONDS);
    FleetSetupCredentials::new(pair.access.as_str().to_owned(), pair.refresh.as_str().to_owned(),
        pair.client_id.clone().unwrap_or_default(), region, lifetime).map_err(|_| unavailable())
}

/// The fixed code of a Fleet discovery failure. A transport failure seen
/// while the gate now refuses is the window closing under the list.
fn fleet_discovery_code(error: &crate::collection::collector::CollectorError, base: &str) -> &'static str {
    use crate::collection::collector::CollectorError;
    use crate::fleet_api::egress::{self, PermitKind, Provider};
    match error {
        CollectorError::NativeSetupVehicleNotFound(_) => "discovery_vehicle_not_listed",
        CollectorError::NativeSetupVehicleSelectionRequired { .. } => "discovery_selection_required",
        CollectorError::NativeSetupVehicleIdInvalid => "discovery_selection_invalid",
        CollectorError::NativeSetupNoVehicles => "discovery_no_vehicles",
        CollectorError::NativeSetupVehicleConflict { .. } | CollectorError::FleetSetupInventoryMismatch => "discovery_vehicle_conflict",
        CollectorError::NativeSetupStoreMismatch | CollectorError::NativeSetupFleetProviderRequired => "discovery_settings_invalid",
        CollectorError::FleetApi(error) => match error {
            FleetApiError::RequestNotSent | FleetApiError::Transport | FleetApiError::RequestTimeout
                if egress::ask(base, Provider::Fleet, PermitKind::Request).is_err() => crate::auth::credential_sign_in::WINDOW_CLOSED,
            FleetApiError::RequestNotSent | FleetApiError::Transport | FleetApiError::RequestTimeout | FleetApiError::ResponseRead => "discovery_unavailable",
            FleetApiError::RateLimited { .. } => "discovery_rate_limited",
            FleetApiError::HttpStatus(_) | FleetApiError::ProviderHttpStatus { .. } => "discovery_refused",
            FleetApiError::InvalidEndpoint => "discovery_settings_invalid",
            _ => "discovery_answer_invalid",
        },
        _ => "discovery_failed",
    }
}

/// Where Tesla fetches a registered domain's public key.
pub const FLEET_PUBLIC_KEY_WELL_KNOWN_PATH: &str = "/.well-known/appspecific/com.tesla.3p.public-key.pem";
/// One bound per partner send (token, registration, key read), well inside
/// the operator channel's wait.
const PARTNER_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A refused Fleet partner verb (M09-T18): a fixed code and fixed owner text,
/// never a provider body, the domain's answer or the client secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FleetPartnerRefusal {
    pub code: &'static str,
    pub message: &'static str,
}

impl FleetPartnerRefusal {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    fn settings_invalid() -> Self {
        Self::new("fleet_partner_settings_invalid", "setup-fleet register reads one JSON object with exactly clientId, clientSecret, domain (a host name such as example.com) and region (na, eu or cn)")
    }

    fn key(error: crate::fleet_api::partner::keys::FleetKeyError) -> Self {
        use crate::fleet_api::partner::keys::FleetKeyError;
        match error {
            FleetKeyError::Busy => Self::new("fleet_partner_key_busy", "another request is using the Hub's Fleet key; try again"),
            FleetKeyError::BackupExclusion => Self::new("fleet_partner_key_backup_exclusion", "the Hub's secrets folder could not be verified as excluded from backups; try again after correcting backup exclusion"),
            FleetKeyError::Storage => Self::new("fleet_partner_key_storage", "the Hub's Fleet key cannot be created or read in its secrets folder"),
            FleetKeyError::Unsafe => Self::new("fleet_partner_key_unsafe", "the Hub's Fleet key file is not a private file of this user; it was left unchanged"),
            FleetKeyError::Invalid => Self::new("fleet_partner_key_invalid", "the Hub's Fleet key file is not a P-256 key; it was left unchanged"),
            FleetKeyError::Generation => Self::new("fleet_partner_key_generation", "the Hub could not create its Fleet key"),
        }
    }

    fn partner(error: crate::fleet_api::partner::PartnerError) -> Self {
        use crate::fleet_api::partner::PartnerError;
        match error {
            PartnerError::InvalidInput => Self::settings_invalid(),
            PartnerError::WindowClosed => Self::new(crate::auth::credential_sign_in::WINDOW_CLOSED, "Fleet partner registration needs an open collection window"),
            PartnerError::ProviderParked => Self::new("provider_parked", "Fleet partner registration is not offered on this Hub"),
            PartnerError::Timeout => Self::new("fleet_partner_timeout", "Tesla did not answer in time; nothing is retried, try again"),
            PartnerError::Transport => Self::new("fleet_partner_transport", "Tesla could not be reached; check the network and try again"),
            PartnerError::Response1xx => Self::new("fleet_partner_response_1xx", "Tesla's HTTP 1xx answer was not understood"),
            PartnerError::Response2xx => Self::new("fleet_partner_response_2xx", "Tesla's HTTP 2xx answer was not understood"),
            PartnerError::Response3xx => Self::new("fleet_partner_response_3xx", "Tesla's HTTP 3xx answer was not understood"),
            PartnerError::Response4xx => Self::new("fleet_partner_response_4xx", "Tesla's HTTP 4xx answer was not understood"),
            PartnerError::Response5xx => Self::new("fleet_partner_response_5xx", "Tesla's HTTP 5xx answer was not understood"),
            PartnerError::InvalidResponse => Self::new("fleet_partner_invalid_response", "Tesla's answer was not understood"),
            PartnerError::Authentication => Self::new("fleet_partner_auth_failed", "Tesla refused the client id or client secret of your developer application"),
            PartnerError::PublicKeyUnreachable => Self::new("fleet_partner_public_key_unreachable", "Tesla cannot fetch the public key file at https://<your domain>/.well-known/appspecific/com.tesla.3p.public-key.pem; publish this Hub's public key there (setup-fleet public-key) and try again"),
            PartnerError::PublicKeyMismatch => Self::new("fleet_partner_public_key_mismatch", "the public key Tesla holds for your domain is not this Hub's key; publish this Hub's public key (setup-fleet public-key) at /.well-known/appspecific/com.tesla.3p.public-key.pem and try again"),
            PartnerError::OverLimit => Self::new("fleet_partner_over_limit", "Tesla refused the request for its rate or billing limit; try again later"),
            PartnerError::Registration => Self::new("fleet_partner_registration_failed", "Tesla refused to register the domain"),
        }
    }
}

impl std::fmt::Display for FleetPartnerRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FleetPartnerRefusal {}

/// `setup-fleet register`'s standard input: exactly these four keys. The
/// strings are wiped on drop; the secret moves into a wiping buffer.
#[derive(serde::Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PartnerInput {
    client_id: String,
    client_secret: String,
    domain: String,
    region: String,
}

/// The checked registration input, holding the client secret until the
/// registration consumes it.
pub struct FleetPartnerSettings {
    region: FleetRegion,
    domain: String,
    credentials: crate::fleet_api::partner::PartnerCredentials,
}

impl std::fmt::Debug for FleetPartnerSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FleetPartnerSettings([redacted])")
    }
}

/// Reads `setup-fleet register`'s input, as the operator command checks it
/// before forwarding and the service reads it again. A key naming a token,
/// in any case, is refused `source_credential_disabled` from the keys alone,
/// before any value is read; any other shape, an unknown region, an invalid
/// client id, secret or domain is `fleet_partner_settings_invalid`.
pub fn read_fleet_partner_input(line: &[u8]) -> Result<FleetPartnerSettings, FleetPartnerRefusal> {
    use crate::auth::credential_sign_in::{SOURCE_CREDENTIAL_DISABLED_TEXT, refuse_token_keys};
    use crate::fleet_api::partner::{PartnerCredentials, validate_domain};
    refuse_token_keys(line).map_err(|error| FleetPartnerRefusal::new(error.code(), SOURCE_CREDENTIAL_DISABLED_TEXT))?;
    let mut input: PartnerInput = serde_json::from_slice(line).map_err(|_| FleetPartnerRefusal::settings_invalid())?;
    let region = FleetRegion::from_storage_code(&input.region).map_err(|_| FleetPartnerRefusal::settings_invalid())?;
    let client_id = FleetClientId::parse(&input.client_id).map_err(|_| FleetPartnerRefusal::settings_invalid())?;
    let domain = validate_domain(&input.domain).map_err(|_| FleetPartnerRefusal::settings_invalid())?;
    let secret = Zeroizing::new(std::mem::take(&mut input.client_secret));
    let credentials = PartnerCredentials::new(client_id, secret).map_err(|_| FleetPartnerRefusal::settings_invalid())?;
    Ok(FleetPartnerSettings { region, domain, credentials })
}

/// `setup-fleet public-key` inside the service: the Hub's Fleet public key
/// as PEM, the key pair made on first use under `data_dir/secrets` (which
/// backups exclude). The private key is never read out. Nothing is sent.
pub fn fleet_public_key_pem(data_dir: &Path) -> Result<String, FleetPartnerRefusal> {
    crate::fleet_api::partner::keys::ensure_command_key(data_dir)
        .map(|key| key.public_pem().to_owned())
        .map_err(FleetPartnerRefusal::key)
}

/// `setup-fleet register` inside the service (design T-B3). Order, so that a
/// refusal sends nothing it need not:
/// 1. the input (token-named keys refused from the keys alone);
/// 2. the Hub's key pair, made on first use;
/// 3. the partner token (client credentials) and the registration, each
///    asking the window's egress gate (`window_closed` outside a window);
/// 4. the key Tesla then holds for the domain must be the Hub's.
///
/// A domain already registered is success. The client secret is wiped when
/// the token request ends and is never stored.
pub async fn register_fleet_partner(data_dir: &Path, line: &[u8]) -> Result<serde_json::Value, FleetPartnerRefusal> {
    register_fleet_partner_with(data_dir, line, |region| {
        crate::fleet_api::partner::PartnerApi::new(region, PARTNER_SEND_TIMEOUT)
    })
    .await
}

async fn register_fleet_partner_with(
    data_dir: &Path,
    line: &[u8],
    client: impl FnOnce(FleetRegion) -> Result<crate::fleet_api::partner::PartnerApi, crate::fleet_api::partner::PartnerError>,
) -> Result<serde_json::Value, FleetPartnerRefusal> {
    use crate::fleet_api::partner::Registration;
    let settings = read_fleet_partner_input(line)?;
    let key = crate::fleet_api::partner::keys::ensure_command_key(data_dir).map_err(FleetPartnerRefusal::key)?;
    let FleetPartnerSettings { region, domain, credentials } = settings;
    let api = client(region).map_err(FleetPartnerRefusal::partner)?;
    let registration = api.register(&domain, credentials, &key).await.map_err(|error| {
        let refusal = FleetPartnerRefusal::partner(error);
        tracing::warn!(code = refusal.code, "Fleet partner registration refused");
        refusal
    })?;
    tracing::info!("Fleet partner domain registered");
    Ok(serde_json::json!({
        "status": "partner_registered",
        "registration": match registration {
            Registration::Registered => "registered",
            Registration::AlreadyRegistered => "already_registered",
        },
        "domain": domain,
        "region": region.storage_code(),
    }))
}

pub fn validate_stored_fleet_credentials(entry: &crate::CredentialVaultEntry, _data_dir: &Path) -> Result<(), FleetCredentialError> {
    validate_stored_fleet_credentials_after(entry.vault(), || {})
}
fn validate_stored_fleet_credentials_after(vault: &CredentialVault, after_first_check: impl FnOnce()) -> Result<(), FleetCredentialError> {
    if vault.has_unresolved_refresh(CredentialSlot::FleetApi)? { return Err(FleetCredentialError::RotationOutcomeUnknown); }
    after_first_check();
    let Some(plain) = vault.load(CredentialSlot::FleetApi)? else { return Err(FleetCredentialError::Missing); };
    FleetAccessToken::new(plain.access.as_str())?;
    FleetRefreshToken::new(plain.refresh.as_str())?;
    FleetClientId::parse(plain.client_id.as_deref().ok_or(FleetCredentialError::Missing)?)?;
    FleetRegion::from_storage_code(plain.region.as_deref().ok_or(FleetCredentialError::Missing)?)?;
    if !fleet_scope_summary(&plain.access)?.collection_ready() { return Err(FleetCredentialError::MissingCollectionScopes); }
    if vault.has_unresolved_refresh(CredentialSlot::FleetApi)? { return Err(FleetCredentialError::RotationOutcomeUnknown); }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FleetCredentialInspection { pub unresolved_refresh: bool }

/// R68: this non-admitted surface proves presence and headers, without
/// decrypting, selecting a key or accepting an unproven unresolved input.
pub fn inspect_stored_fleet_credentials(store: &HubStore, data_dir: &Path) -> Result<FleetCredentialInspection, FleetCredentialError> {
    let presence = store.credential_pair_presence(CredentialSlot::FleetApi)?.ok_or(FleetCredentialError::Missing)?;
    presence.header_check.map_err(|error| FleetCredentialError::Store(StoreError::CredentialEnvelope(error)))?;
    crate::auth::credential_vault::admit_stored_origin(presence.slot, presence.origin, presence.refresh_owner)?;
    let secrets = crate::platform::private_directory::open_validated_read_only(&data_dir.join("secrets"))
        .map_err(|_| FleetCredentialError::UnsafeKeyMaterial)?.ok_or(FleetCredentialError::KeyMissing)?;
    crate::auth::credential_keys::check_keyring(&secrets)
        .map_err(|_| FleetCredentialError::UnsafeKeyMaterial)?.ok_or(FleetCredentialError::KeyMissing)?;
    Ok(FleetCredentialInspection { unresolved_refresh: store.unresolved_fleet_refresh_status()? })
}

pub fn stored_fleet_scope_summary(entry: &crate::CredentialVaultEntry, _data_dir: &Path) -> Result<Option<FleetScopeSummary>, FleetCredentialError> {
    entry.vault().load(CredentialSlot::FleetApi)?.map(|plain| fleet_scope_summary(&plain.access)).transpose()
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
    vault: Arc<CredentialVault>,
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
    vault: Arc<CredentialVault>,
    api: FleetAuthApi,
    client_id: FleetClientId,
    refresh_token: FleetRefreshToken,
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
                    if self.vault.close_refresh(self.receipt_id, self.generation, completion.outcome, completion.http_status, completion.retry_after_seconds, true)
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
        #[cfg(test)]
        if let Some(entries) = &self.encryption_entries { entries.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }
        let output_generation = Uuid::new_v4();
        if let Err(error) = self.vault.persist_refreshed(CredentialSlot::FleetApi, self.receipt_id, self.generation, output_generation,
            refreshed.access_token.expose(), refreshed.refresh_token.expose(), schedule.expires_at, schedule.next_refresh_at) {
            return failed(FleetCredentialError::from(error), true);
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
    #[cfg(test)]
    pub(crate) fn retain_fixture_admission(&mut self)->Arc<crate::hub_user_process::AdmittedUserHub>{
        let admission=self.vault.test_admission();
        self.admission=Some(Arc::clone(&admission));
        admission
    }
    pub(crate) fn mark_refresh_due(&mut self) {
        self.next_refresh_at = 0;
    }

    /// The generation of the stored pair: it changes exactly when a refresh
    /// installs a new pair. It is an identifier, never a secret, so the
    /// collector can tell whether a tick's request refreshed the token (C3 R47).
    pub(crate) const fn credential_generation(&self) -> Uuid {
        self.credential_generation
    }
    pub(crate) fn collector_credential_identity(&self) -> Result<String, FleetCredentialError> {
        self.assert_sensitive_access()?;
        self.vault.collector_credential_identity(true).map_err(Into::into)
    }

    pub(crate) fn from_vault(
        vault: Arc<CredentialVault>, accept_identified: bool,
        loss: Option<crate::credentials::LossObserver>,
    ) -> Result<Self, FleetCredentialError> {
        if loss.as_ref().is_some_and(|lost| lost()) { return Err(FleetCredentialError::SensitiveAccessUnavailable); }
        let plain = vault.load(CredentialSlot::FleetApi)?.ok_or(FleetCredentialError::Missing)?;
        if plain.refresh_owner != RefreshOwner::Hub { return Err(FleetCredentialError::RotationOutcomeUnknown); }
        let access_token = FleetAccessToken::new(plain.access.as_str())?;
        let refresh_token = FleetRefreshToken::new(plain.refresh.as_str())?;
        let unresolved = vault.has_unresolved_refresh(CredentialSlot::FleetApi)?;
        let input_fenced = if unresolved {
            if !accept_identified || !vault.inspect_fleet_refresh_receipt()? { return Err(FleetCredentialError::RotationOutcomeUnknown); }
            true
        } else { false };
        let client_id = FleetClientId::parse(plain.client_id.as_deref().ok_or(FleetCredentialError::Missing)?)?;
        let region = FleetRegion::from_storage_code(plain.region.as_deref().ok_or(FleetCredentialError::Missing)?)?;
        Ok(Self { vault, access_token, refresh_token, client_id, region,
            expires_at: plain.expires_at, next_refresh_at: plain.next_refresh_at, credential_generation: plain.generation,
            refresh_terminal: false, input_fenced, in_flight: None,
            #[cfg(unix)] admission: None, loss, #[cfg(test)] encryption_entries: None })
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
        vault: Arc<CredentialVault>,
        data_dir: &Path,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
        loss: Option<crate::credentials::LossObserver>,
    ) -> Result<Self, FleetCredentialError> {
        if loss.as_ref().is_some_and(|lost| lost()) { return Err(FleetCredentialError::SensitiveAccessUnavailable); }
        admission
            .assert_sensitive_access()
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        admission
            .assert_store_path(data_dir)
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        let mut manager = Self::from_vault(vault, true, loss)?;
        if !fleet_scope_summary(manager.access_token.expose())?.collection_ready() { return Err(FleetCredentialError::MissingCollectionScopes); }
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
        vault: Arc<CredentialVault>,
        data_dir: &Path,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
        loss: Option<crate::credentials::LossObserver>,
    ) -> Result<Self, FleetCredentialError> {
        if loss.as_ref().is_some_and(|lost| lost()) { return Err(FleetCredentialError::SensitiveAccessUnavailable); }
        admission
            .assert_sensitive_access()
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        admission
            .assert_store_path(data_dir)
            .map_err(|_| FleetCredentialError::SensitiveAccessUnavailable)?;
        let mut manager = Self::from_vault(vault, false, loss)?;
        manager.admission = Some(admission);
        Ok(manager)
    }

    #[cfg(test)]
    pub(crate) fn from_store(
        store: HubStore,
        data_dir: &Path,
    ) -> Result<Self, FleetCredentialError> {
        let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store, data_dir);
        Self::from_vault(vault, false, None)
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
        let receipt_id = self.vault.begin_refresh(CredentialSlot::FleetApi, self.credential_generation)
            .map_err(|error| {
                if !matches!(error, CredentialStoreError::DurabilityUnavailable | CredentialStoreError::BackupExclusionFailed) { self.refresh_terminal = true; }
                FleetCredentialError::from(error)
            })?;
        // A denial that arrives between the reservation and the job leaves the receipt
        // as it is: the cancellation would be a second receipt write after the loss,
        // and the started receipt, its binding and its fence are what the next start
        // reads (R-P3bF2-1; the request was not sent, and no later write is made).
        self.assert_sensitive_access()?;
        Ok(FleetExchangeJob {
            vault: Arc::clone(&self.vault),
            api: api.clone(),
            client_id: self.client_id.clone(),
            refresh_token: FleetRefreshToken::new(self.refresh_token.expose().to_owned()).map_err(
                |_| {
                    self.refresh_terminal = true;
                    FleetCredentialError::RotationOutcomeUnknown
                },
            )?,
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

impl From<CredentialStoreError> for FleetCredentialError {
    fn from(error: CredentialStoreError) -> Self {
        match error {
            CredentialStoreError::Store(error) => Self::Store(error),
            CredentialStoreError::SensitiveAccessUnavailable | CredentialStoreError::LockPoisoned => Self::SensitiveAccessUnavailable,
            CredentialStoreError::SourceCredentialDisabled(_) | CredentialStoreError::UnknownOrigin(_) => Self::Store(StoreError::WriteRefused("source_credential_disabled")),
            _ => Self::Store(StoreError::WriteRefused("credential_vault_unavailable")),
        }
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

/// The Fleet partner verbs inside the service (M09-T18), against an
/// in-process Tesla double: no network, no Keychain.
#[cfg(test)]
mod fleet_partner_service_tests {
    use super::*;
    use crate::fleet_api::{
        in_process::{InProcessAnswer, InProcessTransport},
        partner::{PartnerApi, PartnerError},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SECRET: &str = "synthetic-partner-secret-0001";
    const TOKEN: &str = r#"{"access_token":"synthetic-partner-bearer","token_type":"Bearer","expires_in":3600}"#;

    fn input(domain: &str) -> String {
        format!(r#"{{"clientId":"synthetic-client","clientSecret":"{SECRET}","domain":"{domain}","region":"eu"}}"#)
    }

    fn answer(status: u16, body: &str) -> InProcessAnswer {
        Ok(http::Response::builder().status(status).body(body.as_bytes().to_vec()).unwrap())
    }

    /// Tesla's three answers: token, registration, then the key it holds.
    async fn run(data: &Path, line: &str, registration: (u16, String), held: String) -> (Result<serde_json::Value, FleetPartnerRefusal>, usize) {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let transport = InProcessTransport::new(move |_| {
            let result = match count.fetch_add(1, Ordering::SeqCst) {
                0 => answer(200, TOKEN),
                1 => answer(registration.0, &registration.1),
                _ => answer(200, &held),
            };
            async { result }
        });
        let result = register_fleet_partner_with(data, line.as_bytes(), |region| {
            assert_eq!(region, FleetRegion::EuropeMiddleEastAndAfrica);
            Ok(PartnerApi::stubbed_for_test(region, transport))
        })
        .await;
        (result, calls.load(Ordering::SeqCst))
    }

    fn held(hex: &str) -> String {
        format!(r#"{{"response":{{"public_key":"{hex}"}}}}"#)
    }

    fn hub_key_hex(data: &Path) -> String {
        crate::fleet_api::partner::keys::ensure_command_key(data).unwrap().public_point_hex()
    }

    fn files_hold(dir: &Path, needle: &str) -> bool {
        fs::read_dir(dir).unwrap().flatten().any(|entry| {
            let path = entry.path();
            if path.is_dir() { files_hold(&path, needle) } else {
                fs::read(&path).is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
            }
        })
    }

    #[tokio::test]
    async fn fleet_partner_register_reports_registration_and_never_keeps_the_secret() {
        let tmp = crate::private_tempdir().unwrap();
        let hex = hub_key_hex(tmp.path());
        for (registration, expected) in [
            ((200, r#"{"response":{"domain":"synthetic.example.test"}}"#.to_owned()), "registered"),
        ] {
            let (result, sends) = run(tmp.path(), &input("Synthetic.Example.Test"), registration, held(&hex.to_ascii_uppercase())).await;
            let report = result.unwrap();
            assert_eq!(report, serde_json::json!({
                "status": "partner_registered", "registration": expected,
                "domain": "synthetic.example.test", "region": "eu",
            }));
            assert_eq!(sends, 3);
            assert!(!report.to_string().contains(SECRET));
        }
        assert!(!files_hold(tmp.path(), SECRET), "the client secret is never stored");
    }

    /// The key file at the domain: missing is unreachable, another key is a
    /// mismatch; neither echoes Tesla's text.
    #[tokio::test]
    async fn fleet_partner_register_refuses_an_unreachable_or_foreign_key() {
        let tmp = crate::private_tempdir().unwrap();
        let hex = hub_key_hex(tmp.path());
        let mut foreign = hex.clone();
        foreign.replace_range(128.., if hex.ends_with('0') { "11" } else { "00" });
        let registered = || (200, r#"{"response":{"domain":"synthetic.example.test"}}"#.to_owned());
        for (registration, held_key, code, sends) in [
            ((400, r#"{"error":"public_key_not_found","error_description":"synthetic-echo"}"#.to_owned()), held(&hex), "fleet_partner_response_4xx", 2),
            (registered(), held(&foreign), "fleet_partner_public_key_mismatch", 3),
            ((409, r#"{"error":"already_registered"}"#.to_owned()), held(&foreign), "fleet_partner_response_4xx", 2),
            ((401, "synthetic-echo".to_owned()), held(&hex), "fleet_partner_response_4xx", 2),
        ] {
            let (result, count) = run(tmp.path(), &input("synthetic.example.test"), registration, held_key).await;
            let refusal = result.unwrap_err();
            assert_eq!((refusal.code, count), (code, sends));
            assert!(!refusal.to_string().contains("synthetic-echo") && !refusal.to_string().contains(SECRET));
        }
    }

    /// Exactly clientId, clientSecret, domain and region. A token-named key
    /// is refused from the keys alone, even before a malformed value; nothing
    /// is sent and no key is made for a refused input.
    #[tokio::test]
    async fn fleet_partner_register_input_is_exact_and_refuses_token_keys_first() {
        let tmp = crate::private_tempdir().unwrap();
        let valid = input("synthetic.example.test");
        let token_cases = [
            r#"{"clientId":"synthetic-client","accessToken":"x","clientSecret":"s","domain":"synthetic.example.test","region":"eu"}"#.to_owned(),
            r#"{"REFRESH_TOKEN": not json"#.to_owned(),
            valid.replace(r#""region":"eu""#, r#""region":"eu","partnerToken":"x""#),
        ];
        let invalid_cases = [
            "not json".to_owned(),
            valid.replace(r#","region":"eu""#, ""),
            valid.replace(r#""region":"eu""#, r#""region":"eu","redirectUri":"x""#),
            valid.replace(r#""region":"eu""#, r#""region":"eu","region":"eu""#),
            valid.replace(r#""region":"eu""#, r#""region":"us""#),
            valid.replace(SECRET, ""),
            valid.replace(SECRET, "synthetic secret"),
            valid.replace("synthetic.example.test", "https://synthetic.example.test/"),
            valid.replace("synthetic.example.test", "127.0.0.1"),
            valid.replace("synthetic-client", ""),
            format!("{valid} {valid}"),
        ];
        for (cases, code) in [(&token_cases[..], "source_credential_disabled"), (&invalid_cases[..], "fleet_partner_settings_invalid")] {
            for line in cases {
                let refusal = read_fleet_partner_input(line.as_bytes()).unwrap_err();
                assert_eq!(refusal.code, code, "{line}");
                assert!(!refusal.to_string().contains(SECRET));
                let (result, sends) = run(tmp.path(), line, (200, String::new()), String::new()).await;
                assert_eq!((result.unwrap_err().code, sends), (code, 0), "{line}");
            }
        }
        assert!(!tmp.path().join("secrets").exists(), "no key made for a refused input");
        assert!(!format!("{:?}", read_fleet_partner_input(valid.as_bytes()).unwrap()).contains(SECRET));
    }

    #[test]
    fn fleet_partner_public_key_is_made_once_and_holds_no_private_key() {
        let tmp = crate::private_tempdir().unwrap();
        let first = fleet_public_key_pem(tmp.path()).unwrap();
        assert!(first.starts_with("-----BEGIN PUBLIC KEY-----") && !first.contains("PRIVATE"));
        assert_eq!(fleet_public_key_pem(tmp.path()).unwrap(), first);
        let missing = tmp.path().join("absent");
        assert_eq!(fleet_public_key_pem(&missing).unwrap_err().code, "fleet_partner_key_storage");
    }

    /// Every library refusal has a fixed code; the window refusal is the
    /// service's `window_closed`.
    #[test]
    fn fleet_partner_refusals_are_fixed_codes() {
        use PartnerError::*;
        for (error, code) in [
            (WindowClosed, "window_closed"), (InvalidInput, "fleet_partner_settings_invalid"),
            (Timeout, "fleet_partner_timeout"), (Transport, "fleet_partner_transport"),
            (InvalidResponse, "fleet_partner_invalid_response"), (Authentication, "fleet_partner_auth_failed"),
            (PublicKeyUnreachable, "fleet_partner_public_key_unreachable"),
            (PublicKeyMismatch, "fleet_partner_public_key_mismatch"), (OverLimit, "fleet_partner_over_limit"),
            (Registration, "fleet_partner_registration_failed"), (ProviderParked, "provider_parked"),
        ] {
            assert_eq!(FleetPartnerRefusal::partner(error).code, code);
        }
    }
}
