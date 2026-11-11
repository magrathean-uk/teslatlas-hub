// SPDX-License-Identifier: AGPL-3.0-only

use zeroize::Zeroizing;
use crate::{CredentialVaultEntry, credentials::{CredentialError, OwnerTokens}};
use super::super::credential_envelope::CredentialSlot;
use super::messages::{CredentialCommandError, source_credential_disabled};

/// The kinds of hand-supplied Owner API input older Hubs accepted. Every kind
/// is refused since M02-T02: source credentials are disabled and the Hub stores
/// a sign-in only from its own login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerEntryKind { Direct, Provider, SourceToken }

/// A supplied Owner API pair that could be stored. It cannot exist: supplied
/// input is refused, so this type has no value and nothing can persist one.
pub struct PreparedOwnerEntry { never: std::convert::Infallible }
impl PreparedOwnerEntry {
    pub fn with_explicit_replacement(self) -> Self { match self.never {} }
}

impl std::fmt::Debug for PreparedOwnerEntry {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { match self.never {} }
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

/// Refused: source credentials are disabled, so no supplied token (of any
/// kind, with or without an explicit replacement) is prepared for storage.
pub fn prepare_owner_entry(
    kind: OwnerEntryKind,
    access: Zeroizing<String>,
    refresh: Option<Zeroizing<String>>,
    replace_source: bool,
) -> Result<PreparedOwnerEntry, CredentialCommandError> {
    drop((access, refresh));
    let _ = (kind, replace_source);
    Err(source_credential_disabled())
}

/// Refused, as [`prepare_owner_entry`]: a decrypted or otherwise obtained
/// pair is never prepared for storage.
pub fn prepare_owner_tokens(kind: OwnerEntryKind, tokens: OwnerTokens, replace_source: bool) -> Result<PreparedOwnerEntry, CredentialCommandError> {
    drop(tokens);
    prepare_owner_entry(kind, Zeroizing::default(), None, replace_source)
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn spend_refresh_for_test(entry: &CredentialVaultEntry, fleet: bool) -> Result<(), CredentialCommandError> {
    let slot = if fleet { CredentialSlot::FleetApi } else { CredentialSlot::OwnerApi };
    let generation = entry.vault().stored_generation(slot)?.ok_or_else(|| CredentialCommandError::new("credential_store_unavailable"))?;
    entry.vault().begin_refresh(slot, generation)?;
    Ok(())
}

/// Unreachable by construction: no [`PreparedOwnerEntry`] value exists.
pub fn persist_owner_entry(
    entry: &CredentialVaultEntry,
    prepared: PreparedOwnerEntry,
    clear_fleet: bool,
) -> Result<CanonicalOwnerTokens, CredentialCommandError> {
    persist_owner_entry_with_notice(entry, prepared, clear_fleet, &mut std::io::sink())
}

pub fn persist_owner_entry_with_notice(
    entry: &CredentialVaultEntry,
    prepared: PreparedOwnerEntry,
    clear_fleet: bool,
    notice: &mut impl std::io::Write,
) -> Result<CanonicalOwnerTokens, CredentialCommandError> {
    let _ = (entry, clear_fleet, notice);
    match prepared.never {}
}

/// A supplied Fleet pair that could be stored. It cannot exist: Fleet token
/// input is refused, so this type has no value.
pub struct PreparedFleetEntry { never: std::convert::Infallible }

impl std::fmt::Debug for PreparedFleetEntry {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { match self.never {} }
}

/// Refused: Fleet tokens are never supplied to the Hub.
pub fn prepare_fleet_entry(
    access: Zeroizing<String>, refresh: Zeroizing<String>, client_id: String,
    region: crate::fleet_api::FleetRegion, expires_in: u64,
) -> Result<PreparedFleetEntry, CredentialCommandError> {
    drop((access, refresh));
    let _ = (client_id, region, expires_in);
    Err(source_credential_disabled())
}

pub fn persist_fleet_entry(
    entry: &CredentialVaultEntry, prepared: PreparedFleetEntry, clear_owner: bool,
) -> Result<(), CredentialCommandError> {
    let _ = (entry, clear_owner);
    match prepared.never {}
}

pub fn clear_owner_after_fleet(entry: &CredentialVaultEntry) -> Result<(), CredentialCommandError> {
    entry.vault().clear_credential_pair(CredentialSlot::OwnerApi).map_err(|error| {
        let mut error = CredentialCommandError::from(error);
        error.pair_stored = true;
        error.operation = Some("clear_owner_api");
        error
    })
}
