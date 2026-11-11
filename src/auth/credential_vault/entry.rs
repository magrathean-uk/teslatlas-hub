// SPDX-License-Identifier: AGPL-3.0-only
use super::*;
pub struct CredentialVaultEntry { vault: Arc<CredentialVault> }
pub struct CredentialVaultEntryError { error: CredentialStoreError }
impl CredentialVaultEntry {
    pub(in crate::auth) fn vault(&self) -> &CredentialVault { &self.vault }
    #[cfg(test)]
    pub(crate) fn for_test(vault: Arc<CredentialVault>) -> Self { Self { vault } }
}
impl CredentialVaultEntryError {
    pub fn code(&self) -> &'static str {
        self.error.code()
    }
}
pub fn open_credential_vault_for_entry(store: HubStore, admission: Arc<AdmittedUserHub>, data_dir: &Path) -> Result<CredentialVaultEntry, CredentialVaultEntryError> {
    CredentialVault::open(store, admission, data_dir, VaultOptions::default()).map(|vault| CredentialVaultEntry { vault }).map_err(|error| CredentialVaultEntryError { error })
}
