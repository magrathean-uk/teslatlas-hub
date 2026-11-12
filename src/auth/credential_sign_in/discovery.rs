// SPDX-License-Identifier: AGPL-3.0-only

//! Car discovery with the Hub's own login (M02-T13; T01 design 1.1 and 4).
//!
//! `setup` runs this right after its exchange has stored the Hub's own pair,
//! inside the same Open window and with the same vault, before the collector
//! is started again. Order, so that a refusal opens no connection and writes
//! nothing:
//! 1. local checks: the Owner provider (Fleet stays parked) and the selection;
//! 2. the vault's own pair is loaded: only an owner pair of origin
//!    `hub_login` whose refresh the Hub owns is used, never another pair;
//! 3. the window's egress gate is asked (`window_closed` unless a window is
//!    Open), after every local check;
//! 4. one products list over the Owner API, which asks the gate again for a
//!    permit of its own immediately before its send, is read to the Owner
//!    API's bound and parsed strictly;
//! 5. the selection is applied to that list and only then are the chosen
//!    cars recorded, so the collector's start condition holds.
//!
//! A failure leaves the stored pair as it is and configures no car. Every
//! failure is a fixed `discovery_*` code (or `window_closed`,
//! `provider_parked`); no token, VIN or name appears in an error or a log.

use crate::{
    CredentialVaultEntry,
    auth::credential_envelope::{CredentialOrigin, CredentialSlot, RefreshOwner},
    collection::collector::{CollectorError, OwnerCarSelection, configure_owner_cars, owner_discovery_client},
    config::{CollectorProvider, HubConfig},
    db::HubStore,
    fleet_api::egress::{self, PermitKind, Provider},
    legacy_auth::{AuthEndpointConfig, AuthMode, LegacyAuth},
    owner_api::{OwnerApi, OwnerApiError},
};
use super::CredentialCommandError;

/// The listed cars hold no car of the requested id.
pub const DISCOVERY_VEHICLE_NOT_LISTED: &str = "discovery_vehicle_not_listed";
/// The account lists several cars and neither `--vehicle-id` nor
/// `--all-vehicles` chose among them.
pub const DISCOVERY_SELECTION_REQUIRED: &str = "discovery_selection_required";
/// `--vehicle-id` was not a positive id, or both selections were given.
pub const DISCOVERY_SELECTION_INVALID: &str = "discovery_selection_invalid";
/// The account lists no car.
pub const DISCOVERY_NO_VEHICLES: &str = "discovery_no_vehicles";
/// This Hub already holds another car than the one requested.
pub const DISCOVERY_VEHICLE_CONFLICT: &str = "discovery_vehicle_conflict";
/// The list answer was malformed, inconsistent or too large.
pub const DISCOVERY_ANSWER_INVALID: &str = "discovery_answer_invalid";
/// Tesla refused the list (an HTTP error status).
pub const DISCOVERY_REFUSED: &str = "discovery_refused";
/// Tesla asked the Hub to slow down.
pub const DISCOVERY_RATE_LIMITED: &str = "discovery_rate_limited";
/// The list could not be sent or was not answered in time.
pub const DISCOVERY_UNAVAILABLE: &str = "discovery_unavailable";
/// No usable own pair is stored.
pub const DISCOVERY_PAIR_UNAVAILABLE: &str = "discovery_pair_unavailable";
/// The Owner API settings could not be used.
pub const DISCOVERY_SETTINGS_INVALID: &str = "discovery_settings_invalid";
/// Any other local failure (the car records could not be written).
pub const DISCOVERY_FAILED: &str = "discovery_failed";

/// How many cars discovery configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerDiscovery {
    pub vehicles_configured: usize,
}

/// The selection from `setup`'s flags; both flags together are refused.
pub fn owner_car_selection(vehicle_id: Option<i64>, all_vehicles: bool) -> Result<OwnerCarSelection, CredentialCommandError> {
    match (vehicle_id, all_vehicles) {
        (Some(_), true) => Err(CredentialCommandError::new(DISCOVERY_SELECTION_INVALID)),
        (Some(id), false) if id > 0 => Ok(OwnerCarSelection::Vehicle(id)),
        (Some(_), false) => Err(CredentialCommandError::new(DISCOVERY_SELECTION_INVALID)),
        (None, true) => Ok(OwnerCarSelection::All),
        (None, false) => Ok(OwnerCarSelection::Implicit),
    }
}

/// Lists the account's cars with the vault's own pair and records the
/// selected ones. See the module notes for the order of checks.
pub async fn discover_owner_cars(entry: &CredentialVaultEntry, store: &HubStore, config: &HubConfig, selection: OwnerCarSelection) -> Result<OwnerDiscovery, CredentialCommandError> {
    let base = config.collector.owner_api_base_url.clone().ok_or_else(|| CredentialCommandError::new(DISCOVERY_SETTINGS_INVALID))?;
    // The Hub's own pair is a Tesla login: it is only ever sent to Tesla's
    // Owner API. A provider host (Provider mode, parked) is refused here.
    if crate::config::collector_mode(&config.collector) != crate::config::CollectorMode::Legacy {
        return Err(CredentialCommandError::new(DISCOVERY_SETTINGS_INVALID));
    }
    discover_with(entry, store, config, selection, &base, owner_discovery_client).await
}

/// Discovery against a loopback HTTPS stub Owner API, for the binary's
/// tests. It still asks the process-wide egress gate, so it sends nothing
/// unless a window is Open. No other host is accepted.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub async fn discover_owner_cars_with_stub_for_test(entry: &CredentialVaultEntry, store: &HubStore, config: &HubConfig, selection: OwnerCarSelection, stub: &str) -> Result<OwnerDiscovery, CredentialCommandError> {
    let url = url::Url::parse(stub).map_err(|_| CredentialCommandError::new(DISCOVERY_SETTINGS_INVALID))?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if !loopback || url.scheme() != "https" { return Err(CredentialCommandError::new(DISCOVERY_SETTINGS_INVALID)); }
    let mut config = config.clone();
    config.collector.owner_api_base_url = Some(stub.to_owned());
    discover_with(entry, store, &config, selection, stub, owner_discovery_client).await
}

/// The ordered discovery; `base` is the Owner API base the gate is asked
/// for and `client_for` builds the client for the pair's region.
pub(super) async fn discover_with(
    entry: &CredentialVaultEntry,
    store: &HubStore,
    config: &HubConfig,
    selection: OwnerCarSelection,
    base: &str,
    client_for: impl FnOnce(&HubConfig, &LegacyAuth) -> Result<OwnerApi, CollectorError>,
) -> Result<OwnerDiscovery, CredentialCommandError> {
    if config.collector.provider != CollectorProvider::Legacy {
        return Err(CredentialCommandError::new(crate::runtime::operator::admission::PROVIDER_PARKED));
    }
    if matches!(selection, OwnerCarSelection::Vehicle(id) if id <= 0) {
        return Err(CredentialCommandError::new(DISCOVERY_SELECTION_INVALID));
    }
    let auth = own_pair(entry, config)?;
    let client = client_for(config, &auth).map_err(|_| CredentialCommandError::new(DISCOVERY_SETTINGS_INVALID))?;
    // After every local check and before the list: a closed window refuses
    // here with no connection. The list asks the gate again for its own
    // permit immediately before its one send.
    drop(egress::ask(base, Provider::Legacy, PermitKind::Request).map_err(|_| CredentialCommandError::new(super::exchange::WINDOW_CLOSED))?);
    let configured = configure_owner_cars(store, config, &client, &auth, selection).await;
    drop(auth);
    match configured {
        Ok(vehicles_configured) => {
            tracing::info!(vehicles_configured, "owner setup configured its cars");
            Ok(OwnerDiscovery { vehicles_configured })
        }
        Err(error) => {
            let code = failure_code(&error, base);
            tracing::warn!(code, "owner setup discovery failed");
            Err(CredentialCommandError::new(code))
        }
    }
}

/// The Hub's own owner pair, from this vault, as a pair that reads once and
/// never refreshes. Any other pair, or none, is refused.
fn own_pair(entry: &CredentialVaultEntry, config: &HubConfig) -> Result<LegacyAuth, CredentialCommandError> {
    let unavailable = || CredentialCommandError::new(DISCOVERY_PAIR_UNAVAILABLE);
    let pair = entry.vault().load(CredentialSlot::OwnerApi).map_err(|_| unavailable())?.ok_or_else(unavailable)?;
    if pair.origin != Some(CredentialOrigin::HubLogin) || pair.refresh_owner != RefreshOwner::Hub || pair.access.is_empty() {
        return Err(unavailable());
    }
    // The own pair came from Tesla's owner token endpoint, so its issuer pin
    // (and region) is Tesla's, whatever host serves the list in a test.
    let endpoint = AuthEndpointConfig { mode: AuthMode::Tesla, region: config.collector.stream_region, token_url: None, allow_loopback_http: false };
    LegacyAuth::from_access_token(pair.access.as_str(), pair.refresh.as_str(), &endpoint)
        .map_err(|_| unavailable())
}

/// The fixed code of a discovery failure. A transport failure seen while the
/// gate now refuses is the window closing under the list.
fn failure_code(error: &CollectorError, base: &str) -> &'static str {
    match error {
        CollectorError::NativeSetupVehicleNotFound(_) => DISCOVERY_VEHICLE_NOT_LISTED,
        CollectorError::NativeSetupVehicleSelectionRequired { .. } => DISCOVERY_SELECTION_REQUIRED,
        CollectorError::NativeSetupVehicleIdInvalid => DISCOVERY_SELECTION_INVALID,
        CollectorError::NativeSetupNoVehicles => DISCOVERY_NO_VEHICLES,
        CollectorError::NativeSetupVehicleConflict { .. } => DISCOVERY_VEHICLE_CONFLICT,
        CollectorError::NativeSetupStoreMismatch => DISCOVERY_SETTINGS_INVALID,
        CollectorError::OwnerApi(error) => match error {
            OwnerApiError::Transport | OwnerApiError::RequestTimeout
                if egress::ask(base, Provider::Legacy, PermitKind::Request).is_err() => super::exchange::WINDOW_CLOSED,
            OwnerApiError::Transport | OwnerApiError::RequestTimeout | OwnerApiError::ResponseRead => DISCOVERY_UNAVAILABLE,
            OwnerApiError::RateLimited { .. } => DISCOVERY_RATE_LIMITED,
            OwnerApiError::HttpStatus(_) | OwnerApiError::Forbidden { .. } | OwnerApiError::Gone | OwnerApiError::VehicleNotFound
                | OwnerApiError::VehicleInService => DISCOVERY_REFUSED,
            OwnerApiError::LegacyAuth | OwnerApiError::CredentialAuthorityUnavailable => DISCOVERY_UNAVAILABLE,
            OwnerApiError::InvalidEndpoint => DISCOVERY_SETTINGS_INVALID,
            _ => DISCOVERY_ANSWER_INVALID,
        },
        _ => DISCOVERY_FAILED,
    }
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
