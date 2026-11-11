// SPDX-License-Identifier: AGPL-3.0-only

use std::{path::Path, sync::Arc, time::SystemTime};
use crate::{db::HubStore, hub_user_process::AdmittedUserHub};
use super::super::credential_vault::{CredentialVault, VaultOptions};
#[cfg(debug_assertions)]
use super::super::credential_keys::{BackupExclusion, CredentialKeyError, ExclusionState};
use crate::{credentials::LegacyAuthManager, legacy_auth::{AuthEndpointConfig, AuthMode}};

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn seed_owner_pair_for_test(data: &Path, access: &str, refresh: &str, schedule: Option<(i64, i64)>) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let scope = super::SignInScope::open_for_test(data, true)?;
    let mut entry = super::super::credential_vault::CredentialEntry::own_login_fixture(zeroize::Zeroizing::new(access.to_owned()), zeroize::Zeroizing::new(refresh.to_owned())).map_err(super::CredentialCommandError::from)?;
    let (expiry, next) = schedule.unwrap_or((0, 0));
    entry.set_schedule(expiry, next);
    let generation = scope.entry().vault().store_supplied_entry(entry, crate::db::ReplacePolicy::ReplaceAny).map_err(super::CredentialCommandError::from)?;
    scope.release()?;
    Ok(generation)
}

/// Seeds the supplied Fleet row an older Hub could hold; the vault refuses it
/// at load, so it exercises only the refusal.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn seed_fleet_pair_for_test(data: &Path, credentials: &crate::fleet_credentials::FleetSetupCredentials) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let scope = super::SignInScope::open_for_test(data, true)?;
    scope.entry().vault().store_supplied_entry(credentials.credential_entry(SystemTime::now())?, crate::db::ReplacePolicy::ReplaceAny).map_err(super::CredentialCommandError::from)?;
    let generation = scope.entry().vault().stored_generation(super::super::credential_envelope::CredentialSlot::FleetApi).map_err(super::CredentialCommandError::from)?
        .ok_or_else(|| std::io::Error::other("fixture_pair_absent"))?;
    scope.release()?;
    Ok(generation)
}

#[doc(hidden)]
pub fn begin_refresh_for_test(data: &Path, fleet: bool, expected: uuid::Uuid) -> Result<(), Box<dyn std::error::Error>> {
    // A refresh begins only under the live collector lease of the store that
    // opened the vault (M02 design 5), so the fixture holds one for its begin.
    let admission = AdmittedUserHub::admit(data)?;
    let store = HubStore::initialize(data)?;
    let vault = CredentialVault::open(store.clone(), Arc::clone(&admission), data,
        VaultOptions { #[cfg(debug_assertions)] exclusion: Some(Arc::new(Excluded)), ..VaultOptions::default() }).map_err(super::CredentialCommandError::from)?;
    let slot = if fleet { super::super::credential_envelope::CredentialSlot::FleetApi } else { super::super::credential_envelope::CredentialSlot::OwnerApi };
    let actual = vault.stored_generation(slot).map_err(super::CredentialCommandError::from)?;
    if actual != Some(expected) { return Err(std::io::Error::other("fixture_generation_mismatch").into()); }
    with_refresh_lease(&store, || vault.begin_refresh(slot, expected).map(|_| ()).map_err(super::CredentialCommandError::from))?;
    drop(vault);
    drop(store);
    Arc::try_unwrap(admission).map(drop).map_err(|_| std::io::Error::other("fixture_admission_retained").into())
}

/// Hold the supervised collector lease on `store` for one refresh, as the
/// collector does, and give it back whatever the refresh returned.
fn with_refresh_lease<T, E: Into<Box<dyn std::error::Error>>>(store: &HubStore, refresh: impl FnOnce() -> Result<T, E>) -> Result<T, Box<dyn std::error::Error>> {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_millis();
    let lease = store.acquire_supervised_collector_lease(i64::try_from(now)?)?;
    let result = refresh().map_err(Into::into);
    store.release_supervised_collector_lease(lease)?;
    result
}

#[doc(hidden)]
pub fn pair_matches_for_test(data: &Path, fleet: bool, access: &str, refresh: &str) -> Result<bool, Box<dyn std::error::Error>> {
    #[cfg(debug_assertions)]
    let scope = super::SignInScope::open_for_test(data, true)?;
    #[cfg(not(debug_assertions))]
    let scope = super::SignInScope::open(data)?;
    let slot = if fleet { super::super::credential_envelope::CredentialSlot::FleetApi } else { super::super::credential_envelope::CredentialSlot::OwnerApi };
    let pair = scope.entry().vault().load(slot).map_err(super::CredentialCommandError::from)?;
    let matches = pair.as_ref().is_some_and(|pair| pair.access.as_str() == access && pair.refresh.as_str() == refresh);
    drop(pair);
    scope.release()?;
    Ok(matches)
}

#[cfg(debug_assertions)]
struct Excluded;
#[cfg(debug_assertions)]
impl BackupExclusion for Excluded {
    fn ensure_excluded(&self, _: &Path) -> Result<ExclusionState, CredentialKeyError> {
        #[cfg(target_os = "macos")]
        { Ok(ExclusionState::Verified) }
        #[cfg(not(target_os = "macos"))]
        { Ok(ExclusionState::NotApplicable) }
    }
}
struct FixtureManager {
    manager: Option<LegacyAuthManager>,
    store: Option<HubStore>,
    admission: Option<Arc<AdmittedUserHub>>,
}
impl FixtureManager {
    fn open(data: &Path, token_url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let url = url::Url::parse(token_url).map_err(|_| std::io::Error::other("fixture_endpoint_invalid"))?;
        let loopback = match url.host() { Some(url::Host::Ipv4(address)) => address.is_loopback(), Some(url::Host::Ipv6(address)) => address.is_loopback(), _ => false };
        if !loopback || url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err(std::io::Error::other("fixture_endpoint_invalid").into());
        }
        let admission = AdmittedUserHub::admit(data)?;
        let store = HubStore::initialize(data)?;
        let vault = CredentialVault::open(store.clone(), Arc::clone(&admission), data,
            VaultOptions { #[cfg(debug_assertions)] exclusion: Some(Arc::new(Excluded)), ..VaultOptions::default() }).map_err(super::CredentialCommandError::from)?;
        // Opaque synthetic tokens use C5's explicit Provider loopback profile.
        // The real manager, persistence, fences and barriers are unchanged.
        let endpoint = AuthEndpointConfig { mode: AuthMode::Provider, region: None, token_url: Some(token_url.to_owned()), allow_loopback_http: true };
        let manager = LegacyAuthManager::from_hub_source_db_store_for_admitted_user(store.clone(), data, &endpoint, Arc::clone(&admission), None, vault)?;
        Ok(Self { manager: Some(manager), store: Some(store), admission: Some(admission) })
    }
    fn release(mut self) -> Result<(), Box<dyn std::error::Error>> {
        drop(self.manager.take());
        drop(self.store.take());
        match Arc::try_unwrap(self.admission.take().expect("fixture admission")) {
            Ok(admission) => { drop(admission); Ok(()) }
            Err(_) => Err(std::io::Error::other("fixture_admission_retained").into()),
        }
    }
}

/// Seed the recorded admission with the real C5 constructor, without an
/// exchange. The endpoint is numeric loopback and construction performs no I/O
/// against it. The caller uses a fresh simulated process after entry.
#[doc(hidden)]
pub fn record_provider_admission_for_test(data: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let fixture = FixtureManager::open(data, "http://127.0.0.1:9/token")?;
    let usable = fixture.manager.as_ref().expect("manager").access_token_for_sensitive_use().is_ok();
    fixture.release()?;
    Ok(usable)
}

/// Exercise C5's real managed refresh after a Hub grant. Only observations
/// leave the fixture; neither a token nor the resident vault is returned.
#[doc(hidden)]
pub async fn refresh_owner_manager_for_test(data: &Path, token_url: &str, before: &str, after: &str) -> Result<(bool, bool), Box<dyn std::error::Error>> {
    let mut fixture = FixtureManager::open(data, token_url)?;
    let manager = fixture.manager.as_mut().expect("manager");
    let loaded = manager.access_token_for_sensitive_use()? == before;
    crate::crypto::install_default_provider();
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(60)).build()?;
    let store = fixture.store.clone().expect("fixture store");
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_millis();
    let lease = store.acquire_supervised_collector_lease(i64::try_from(now)?)?;
    let refreshed = manager.refresh_now(&client, SystemTime::now()).await;
    store.release_supervised_collector_lease(lease)?;
    drop(store);
    refreshed?;
    let adopted = manager.access_token_for_sensitive_use()? == after;
    manager.shutdown().await?;
    fixture.release()?;
    Ok((loaded, adopted))
}
