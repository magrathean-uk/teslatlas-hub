// SPDX-License-Identifier: AGPL-3.0-only

use std::path::Path;
use serde::Serialize;
use crate::{
    auth::{credential_envelope::{CredentialSlot, EnvelopeError}, credential_keys, credential_vault::CredentialStoreError},
    db::{CredentialDurabilityState, CredentialMigrationBlockReason, CredentialPairPresence, CredentialSecretsExclusion, CredentialSlotFormatState, HubStore, StoreError},
    platform::private_directory::{self, PrivateDirectoryError},
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SlotStatus {
    pub present: bool,
    pub expires_at: Option<i64>,
    pub next_refresh_at: Option<i64>,
    pub origin: Option<&'static str>,
    pub refresh_owner: Option<&'static str>,
    pub store_state: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BlockedReasons {
    pub owner_api: Option<&'static str>,
    pub fleet_api: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStoreStatus {
    pub format_state: &'static str,
    pub blocked_reason: BlockedReasons,
    pub cleanup_needed_bytes: Option<u64>,
    pub keyring: &'static str,
    pub key_age_days: Option<u64>,
    pub exposure_suspected: bool,
    pub secrets_backup_excluded: Option<bool>,
    pub durability: Option<&'static str>,
    pub legacy_fences: u64,
    pub opaque_legacy_fences: u64,
    pub token_entry_requires_sign_in: bool,
    pub backups_waiting: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    pub legacy_credentials: SlotStatus,
    pub fleet_credentials: SlotStatus,
    pub credential_store: CredentialStoreStatus,
}

/// A blocked slot holds pre-upgrade source material, refused without being
/// read since source credentials are disabled (M02-T02), whatever reason an
/// earlier version recorded.
fn block_code(reason: CredentialMigrationBlockReason) -> &'static str {
    match reason {
        CredentialMigrationBlockReason::OldKeyUnavailable | CredentialMigrationBlockReason::OldPairInvalid => "source_credential_disabled",
    }
}

fn header_code(error: &EnvelopeError) -> &'static str {
    match error {
        EnvelopeError::UnknownKey => "unknown_key",
        EnvelopeError::BindingMismatch(_) => "binding_mismatch",
        EnvelopeError::UnknownVersion(_) | EnvelopeError::UnknownAlgorithm(_) => "unknown_format",
        EnvelopeError::OverCap => "over_cap",
        EnvelopeError::TokenTooLarge => "token_too_large",
        EnvelopeError::EntropyUnavailable => "credential_store_unavailable",
        EnvelopeError::Truncated | EnvelopeError::BadMagic | EnvelopeError::Corrupt => "corrupt",
    }
}

fn slot_status(
    presence: Option<CredentialPairPresence>,
    pending: bool,
    blocked: bool,
) -> SlotStatus {
    let store_state = if blocked { "source_credential_disabled" } else if pending { "migration_pending" } else {
        presence.as_ref().and_then(|row| row.header_check.as_ref().err()).map_or("ok", header_code)
    };
    // Source credentials are disabled: a pair the Hub did not get from its own
    // login is reported by its refusal code, so nothing treats it as usable.
    let store_state = match presence.as_ref() {
        Some(row) if store_state == "ok" => crate::auth::credential_vault::stored_origin_refusal(row.slot, row.origin, row.refresh_owner).unwrap_or("ok"),
        _ => store_state,
    };
    SlotStatus {
        present: presence.is_some(),
        expires_at: presence.as_ref().map(|row| row.expires_at),
        next_refresh_at: presence.as_ref().map(|row| row.next_refresh_at),
        origin: presence.as_ref().and_then(|row| row.origin.map(|origin| origin.as_str())),
        refresh_owner: presence.as_ref().map(|row| row.refresh_owner.as_str()),
        store_state,
    }
}

fn keyring_status(data_dir: &Path, rows_present: bool, now_ms: i64) -> (&'static str, Option<u64>, bool) {
    let absent = || if rows_present { ("keyring_missing", None, false) } else { ("ok", None, false) };
    let directory = match private_directory::open_validated_read_only(&data_dir.join("secrets")) {
        Ok(Some(directory)) => directory,
        Ok(None) => return absent(),
        Err(PrivateDirectoryError::Io { .. }) => return ("keyring_unreadable", None, false),
        Err(_) => return ("keyring_unsafe", None, false),
    };
    match credential_keys::check_keyring(&directory) {
        Ok(Some(check)) => {
            let now_s = now_ms.max(0).div_euclid(1000) as u64;
            ("ok", Some(now_s.saturating_sub(check.current.created_at_s) / 86_400), check.exposure_suspected)
        }
        Ok(None) => absent(),
        Err(error) => (CredentialStoreError::from(error).code(), None, false),
    }
}

/// R68: clear presence, the format row and the non-mutating keyring checks.
/// No admission or credential connection is requested by this projection.
pub fn credential_status(store: &HubStore, data_dir: &Path, now_ms: i64) -> Result<CredentialStatus, StoreError> {
    let format = store.credential_format_state()?;
    let owner = if format.is_some() { store.credential_pair_presence(CredentialSlot::OwnerApi)? } else { None };
    let fleet = if format.is_some() { store.credential_pair_presence(CredentialSlot::FleetApi)? } else { None };
    let (keyring, key_age_days, exposure_suspected) = keyring_status(data_dir, owner.is_some() || fleet.is_some(), now_ms);
    let Some(format) = format else {
        return Ok(CredentialStatus {
            legacy_credentials: slot_status(None, true, false),
            fleet_credentials: slot_status(None, true, false),
            credential_store: CredentialStoreStatus {
                format_state: "pending",
                blocked_reason: BlockedReasons { owner_api: None, fleet_api: None },
                cleanup_needed_bytes: None, keyring, key_age_days, exposure_suspected,
                secrets_backup_excluded: None, durability: None, legacy_fences: 0,
                opaque_legacy_fences: 0, token_entry_requires_sign_in: false, backups_waiting: true,
            },
        });
    };
    let owner_pending = format.owner_api_state == CredentialSlotFormatState::Pending;
    let fleet_pending = format.fleet_api_state == CredentialSlotFormatState::Pending;
    let blocked = format.owner_api_blocked.is_some() || format.fleet_api_blocked.is_some();
    let format_state = if blocked { "blocked" }
        else if owner_pending || fleet_pending || format.fences_pending { "pending" }
        else if format.completed_at_ms.is_none() || format.purge_required || !format.wal_scrubbed
            || !format.old_files_removed || !format.old_tables_dropped { "cleanup_pending" }
        else { "complete" };
    let opaque = store.credential_opaque_fence_count()?;
    let secrets_backup_excluded = if cfg!(target_os = "macos") {
        match format.secrets_exclusion {
            Some(CredentialSecretsExclusion::Verified) => Some(true),
            Some(CredentialSecretsExclusion::Failed) => Some(false),
            Some(CredentialSecretsExclusion::NotApplicable) | None => None,
        }
    } else { None };
    Ok(CredentialStatus {
        legacy_credentials: slot_status(owner, owner_pending, format.owner_api_blocked.is_some()),
        fleet_credentials: slot_status(fleet, fleet_pending, format.fleet_api_blocked.is_some()),
        credential_store: CredentialStoreStatus {
            format_state,
            blocked_reason: BlockedReasons {
                owner_api: format.owner_api_blocked.map(block_code),
                fleet_api: format.fleet_api_blocked.map(block_code),
            },
            cleanup_needed_bytes: format.purge_needed_bytes.map(|bytes| bytes.max(0) as u64),
            keyring, key_age_days, secrets_backup_excluded,
            // R16: the live check, or an exposure the vault repaired and recorded.
            exposure_suspected: exposure_suspected || format.keyring_exposure,
            durability: format.durability.map(|state| match state {
                CredentialDurabilityState::Ok => "ok", CredentialDurabilityState::Unavailable => "unavailable",
            }),
            legacy_fences: format.legacy_fences.max(0) as u64,
            opaque_legacy_fences: opaque,
            token_entry_requires_sign_in: opaque > 0,
            backups_waiting: format_state != "complete",
        },
    })
}

/// The recorded credential failures that make the configured service unhealthy.
pub fn credential_store_unhealthy(status: &CredentialStatus, owner_required: bool, fleet_required: bool) -> bool {
    status.credential_store.keyring != "ok"
        || status.credential_store.durability == Some("unavailable")
        || status.credential_store.secrets_backup_excluded == Some(false)
        || (owner_required && status.legacy_credentials.store_state != "ok")
        || (fleet_required && status.fleet_credentials.store_state != "ok")
}
