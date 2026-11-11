// SPDX-License-Identifier: AGPL-3.0-only

use zeroize::Zeroizing;
use crate::{CredentialVaultEntry, credentials::{CredentialError, OwnerTokens}, db::{HubStore, ReplacePolicy}};
use super::super::{credential_envelope::CredentialSlot, credential_vault::{CredentialEntry, CredentialStoreError}};
use super::messages::CredentialCommandError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerEntryKind { Direct, Provider, SourceToken }

impl OwnerEntryKind {
    fn origin(self) -> &'static str {
        match self { Self::Direct => "direct_entry", Self::Provider => "provider_entry", Self::SourceToken => "source_token" }
    }
}

pub struct PreparedOwnerEntry {
    entry: CredentialEntry,
    canonical: CanonicalOwnerTokens,
    kind: OwnerEntryKind,
    replace_source: bool,
}
impl PreparedOwnerEntry {
    pub fn with_explicit_replacement(mut self) -> Self { self.replace_source = true; self }
}

impl std::fmt::Debug for PreparedOwnerEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PreparedOwnerEntry").field("origin", &self.kind.origin()).field("tokens", &self.canonical).finish()
    }
}

pub struct CanonicalOwnerTokens { access: Zeroizing<String>, refresh: Zeroizing<String> }

impl std::fmt::Debug for CanonicalOwnerTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("CanonicalOwnerTokens").field("access_len", &self.access.len()).field("refresh_len", &self.refresh.len()).finish()
    }
}

impl CanonicalOwnerTokens {
    pub fn into_owner_tokens(mut self) -> Result<OwnerTokens, CredentialError> {
        OwnerTokens::from_secret_parts(std::mem::take(&mut *self.access), std::mem::take(&mut *self.refresh))
    }
}

fn canonical_copy(text: &str) -> Zeroizing<String> {
    let text = text.trim_matches(|value| matches!(value, ' ' | '\t' | '\r' | '\n'));
    let mut copy = Zeroizing::new(String::with_capacity(text.len()));
    copy.push_str(text);
    copy
}

pub fn prepare_owner_entry(
    kind: OwnerEntryKind,
    access: Zeroizing<String>,
    refresh: Option<Zeroizing<String>>,
    replace_source: bool,
) -> Result<PreparedOwnerEntry, CredentialCommandError> {
    let canonical = CanonicalOwnerTokens {
        access: canonical_copy(&access),
        refresh: refresh.as_ref().map_or_else(Zeroizing::default, |value| canonical_copy(value)),
    };
    let entry = match kind {
        OwnerEntryKind::Direct => CredentialEntry::direct_entry(access, refresh.unwrap_or_default()),
        OwnerEntryKind::Provider => CredentialEntry::provider_entry(access, refresh),
        OwnerEntryKind::SourceToken => CredentialEntry::source_token(access, refresh.unwrap_or_default()),
    }?;
    Ok(PreparedOwnerEntry { entry, canonical, kind, replace_source })
}

pub fn prepare_owner_tokens(kind: OwnerEntryKind, tokens: OwnerTokens, replace_source: bool) -> Result<PreparedOwnerEntry, CredentialCommandError> {
    let mut access = Zeroizing::new(String::with_capacity(tokens.access_token().len()));
    access.push_str(tokens.access_token());
    let mut refresh = Zeroizing::new(String::with_capacity(tokens.refresh_token().len()));
    refresh.push_str(tokens.refresh_token());
    prepare_owner_entry(kind, access, Some(refresh), replace_source)
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn spend_refresh_for_test(entry: &CredentialVaultEntry, fleet: bool) -> Result<(), CredentialCommandError> {
    let slot = if fleet { CredentialSlot::FleetApi } else { CredentialSlot::OwnerApi };
    let generation = entry.vault().stored_generation(slot)?.ok_or_else(|| CredentialCommandError::new("credential_store_unavailable"))?;
    entry.vault().begin_refresh(slot, generation)?;
    Ok(())
}

/// R77's import check runs before the caller opens any source ciphertext.
pub fn check_token_import_continuity(store: &HubStore) -> Result<(), CredentialCommandError> {
    if store.credential_opaque_fence_count().map_err(CredentialStoreError::from)? > 0 {
        return Err(CredentialStoreError::FreshSignInRequired.into());
    }
    Ok(())
}

/// One bridge, one barrier, then the optional provider-switch clear.
pub fn persist_owner_entry(
    entry: &CredentialVaultEntry,
    prepared: PreparedOwnerEntry,
    clear_fleet: bool,
) -> Result<CanonicalOwnerTokens, CredentialCommandError> {
    persist_owner_entry_with_notice(entry, prepared, clear_fleet, &mut std::io::stderr().lock())
}

pub fn persist_owner_entry_with_notice(
    entry: &CredentialVaultEntry,
    prepared: PreparedOwnerEntry,
    clear_fleet: bool,
    notice: &mut impl std::io::Write,
) -> Result<CanonicalOwnerTokens, CredentialCommandError> {
    let PreparedOwnerEntry { entry: supplied, canonical, kind, replace_source } = prepared;
    let policy = if kind == OwnerEntryKind::SourceToken && !replace_source { ReplacePolicy::OnlyIfEmpty } else { ReplacePolicy::ReplaceAny };
    if kind == OwnerEntryKind::SourceToken && replace_source {
        writeln!(notice, "{}", super::REPLACE_IMPORT_WARNING).map_err(|_| CredentialCommandError::new("credential_store_unavailable"))?;
    }
    entry.vault().store_supplied_entry(supplied, policy)?;
    tracing::info!(slot = "owner_api", origin = kind.origin(), refresh_owner = if kind == OwnerEntryKind::SourceToken { "external" } else { "hub" }, provenance = "supplied", "credential write");
    if clear_fleet {
        if let Err(error) = entry.vault().clear_credential_pair(CredentialSlot::FleetApi) {
            let mut error = CredentialCommandError::from(error);
            error.pair_stored = true;
            error.operation = Some("clear_fleet_api");
            tracing::error!(operation = "clear_fleet_api", outcome = "new_pair_retained", code = error.code(), "credential provider switch");
            return Err(error);
        }
    }
    Ok(canonical)
}

pub struct PreparedFleetEntry { entry: CredentialEntry }

impl std::fmt::Debug for PreparedFleetEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PreparedFleetEntry").field("entry", &self.entry).finish()
    }
}

pub fn prepare_fleet_entry(
    access: Zeroizing<String>, refresh: Zeroizing<String>, client_id: String,
    region: crate::fleet_api::FleetRegion, expires_in: u64,
) -> Result<PreparedFleetEntry, CredentialCommandError> {
    Ok(PreparedFleetEntry { entry: CredentialEntry::fleet_supplied(access, refresh, client_id, region, expires_in)? })
}

pub fn persist_fleet_entry(
    entry: &CredentialVaultEntry, prepared: PreparedFleetEntry, clear_owner: bool,
) -> Result<(), CredentialCommandError> {
    entry.vault().store_supplied_entry(prepared.entry, ReplacePolicy::ReplaceAny)?;
    tracing::info!(slot = "fleet_api", origin = "fleet_oauth", refresh_owner = "hub", provenance = "supplied", "credential write");
    if clear_owner {
        if let Err(error) = entry.vault().clear_credential_pair(CredentialSlot::OwnerApi) {
            let mut error = CredentialCommandError::from(error);
            error.pair_stored = true;
            error.operation = Some("clear_owner_api");
            tracing::error!(operation = "clear_owner_api", outcome = "new_pair_retained", code = error.code(), "credential provider switch");
            return Err(error);
        }
    }
    Ok(())
}

pub fn clear_owner_after_fleet(entry: &CredentialVaultEntry) -> Result<(), CredentialCommandError> {
    entry.vault().clear_credential_pair(CredentialSlot::OwnerApi).map_err(|error| {
        let mut error = CredentialCommandError::from(error);
        error.pair_stored = true;
        error.operation = Some("clear_owner_api");
        error
    })
}
