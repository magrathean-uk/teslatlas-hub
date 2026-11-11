// SPDX-License-Identifier: AGPL-3.0-only

use serde::Serialize;
use crate::CredentialVaultEntry;
use super::super::credential_envelope::CredentialSlot;
use super::messages::CredentialCommandError;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignOutResult {
    pub owner_api: Option<&'static str>,
    pub fleet_api: Option<&'static str>,
    pub rows_remain: bool,
    pub scrub_pending: bool,
}

impl SignOutResult {
    pub fn succeeded(&self) -> bool {
        self.owner_api.is_none() && self.fleet_api.is_none() && !self.rows_remain
    }
}

/// Both clears use this process's single vault, each with its own barrier.
/// A failed first clear never suppresses the attempt to clear the second slot.
pub fn sign_out(entry: &CredentialVaultEntry) -> SignOutResult {
    let vault = entry.vault();
    let mut committed = false;
    let mut clear = |slot| {
        let before = vault.credential_pair_presence(slot).ok().flatten().is_some();
        let result = vault.clear_credential_pair(slot);
        let after = vault.credential_pair_presence(slot);
        if before && matches!(after, Ok(None)) { committed = true; }
        result.err().map(|error| error.code())
    };
    let owner_api = clear(CredentialSlot::OwnerApi);
    let fleet_api = clear(CredentialSlot::FleetApi);
    let rows_remain = [CredentialSlot::OwnerApi, CredentialSlot::FleetApi].into_iter()
        .any(|slot| !matches!(vault.credential_pair_presence(slot), Ok(None)));
    // A blocked predecessor slot can commit its discard without ever having a
    // current pair. Its successful clear still owes Phase B, even when neither
    // presence snapshot contained a current row.
    let aftercare_pending = (owner_api.is_none() || fleet_api.is_none())
        && vault.finish_clear_aftercare().is_err();
    let scrub_pending = aftercare_pending || (committed && !matches!(vault.post_clear_scrub(), Ok(true)));
    SignOutResult { owner_api, fleet_api, rows_remain, scrub_pending }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RotationResult {
    pub status: &'static str,
    pub rows: u64,
    pub key_age_days: u64,
}

pub fn rotate_credential_key(entry: &CredentialVaultEntry) -> Result<RotationResult, CredentialCommandError> {
    let mut rows = 0;
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        rows += u64::from(entry.vault().credential_pair_presence(slot)?.is_some());
    }
    entry.vault().rotate()?;
    Ok(RotationResult { status: "rotated", rows, key_age_days: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::credential_vault::{CredentialEntry, fixtures::open_fixture_vault}, db::{HubStore, ReplacePolicy}};
    use zeroize::Zeroizing;

    // Both slots hold the Hub's own pairs: supplied entry refuses since M02-T02.
    fn fixture() -> (tempfile::TempDir, HubStore, CredentialVaultEntry) {
        let root = crate::private_tempdir().expect("fixture directory");
        let store = HubStore::initialize(root.path()).expect("store");
        let vault = open_fixture_vault(store.clone(), root.path());
        let entry = CredentialVaultEntry::for_test(vault);
        let owner = CredentialEntry::own_login_fixture(Zeroizing::new("access-0001".into()), Zeroizing::new("refresh-0001".into())).expect("Owner entry");
        entry.vault().store_supplied_entry(owner, ReplacePolicy::ReplaceAny).expect("Owner barrier");
        let fleet = CredentialEntry::fleet_supplied(Zeroizing::new("access-0002".into()), Zeroizing::new("refresh-0002".into()),
            "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet entry");
        entry.vault().store_supplied_entry(fleet, ReplacePolicy::ReplaceAny).expect("Fleet barrier");
        (root, store, entry)
    }

    #[test]
    fn sign_out_clears_both_slots_and_retains_keyring_and_attempts() {
        let (root, store, entry) = fixture();
        let generation = entry.vault().stored_generation(CredentialSlot::OwnerApi).expect("generation").expect("pair");
        entry.vault().begin_refresh(CredentialSlot::OwnerApi, generation).expect("counted attempt");
        let connection = store.open().expect("observer");
        let counts = || connection.query_row("SELECT (SELECT count(*) FROM credential_refresh_attempts), (SELECT count(*) FROM credential_refresh_fences), (SELECT count(*) FROM credential_legacy_fences)", [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))).expect("retained counts");
        let before = counts();
        let keys = std::fs::read(root.path().join("secrets").join("credential-keys")).expect("keyring");
        let result = sign_out(&entry);
        assert!(result.succeeded(), "{result:?}");
        assert!(!result.scrub_pending, "bounded scrub completed");
        for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
            assert!(entry.vault().load(slot).expect("read").is_none(), "slot cleared");
        }
        assert_eq!(counts(), before);
        assert!(std::fs::read(root.path().join("secrets").join("credential-keys")).expect("retained keyring") == keys, "keyring retained byte for byte");
    }

    #[test]
    fn rotation_reports_rows_and_preserves_both_pairs() {
        let (_root, _store, entry) = fixture();
        let result = rotate_credential_key(&entry).expect("rotation barrier");
        assert_eq!(serde_json::to_value(result).expect("result"), serde_json::json!({"status":"rotated","rows":2,"keyAgeDays":0}));
        let plain = entry.vault().load(CredentialSlot::OwnerApi).expect("read").expect("pair");
        assert_eq!(plain.access.as_str(), "access-0001");
        // M02-T02: the Fleet row is carried through rotation but refused at load.
        assert_eq!(entry.vault().load(CredentialSlot::FleetApi).expect_err("Fleet refused").code(), "source_credential_disabled");
    }
}
