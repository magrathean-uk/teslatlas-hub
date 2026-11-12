// SPDX-License-Identifier: AGPL-3.0-only
use super::*;
pub struct CredentialVaultEntry { vault: Arc<CredentialVault> }
pub struct CredentialVaultEntryError { error: CredentialStoreError }
impl CredentialVaultEntry {
    pub(in crate::auth) fn vault(&self) -> &CredentialVault { &self.vault }
    /// The operator's refused-credential purge (M02-T11), on this process's one vault.
    pub(crate) fn purge_refused_credentials(&self) -> Result<RefusedCredentialReport, CredentialStoreError> {
        self.vault.purge_refused_credentials()
    }
    #[cfg(test)]
    pub(crate) fn for_test(vault: Arc<CredentialVault>) -> Self { Self { vault } }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn test_faults(probe_failure: bool, fail_barrier: Option<usize>, cut: Option<&'static str>, observer: Box<dyn FnMut(&str)>) -> impl Drop {
        struct Reset(Option<BinaryControl>);
        impl Drop for Reset {
            fn drop(&mut self) { BINARY_CONTROL.with(|slot| *slot.borrow_mut() = self.0.take()); }
        }
        Reset(BINARY_CONTROL.with(|slot| slot.replace(Some(BinaryControl { probe_failure, fail_barrier, barriers: 0, cut, observer }))))
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn test_io_observer(observer: Box<dyn FnMut(&str, &Path)>) -> impl Drop {
        private_directory::observe_for_test(observer)
    }
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn seed_migration_for_test(data: &Path, candidate: u8, fleet: bool, missing_keyring: bool) -> Result<Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
        fixtures::seed_pending_for_binary(data, candidate, fleet, missing_keyring)
    }

    /// Synthetic exclusion backend for binary tests, which link a non-test library.
    /// Ordinary entry and runtime opening never select this constructor.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn open_for_test(store: HubStore, admission: Arc<AdmittedUserHub>, data_dir: &Path, excluded: bool) -> Result<Self, CredentialVaultEntryError> {
        struct TestExclusion(bool);
        impl BackupExclusion for TestExclusion {
            fn ensure_excluded(&self, _: &Path) -> Result<ExclusionState, CredentialKeyError> {
                if self.0 {
                    test_boundary("exclusion_verified").map_err(|_| CredentialKeyError::BackupExclusionFailed)?;
                    Ok(ExclusionState::Verified)
                } else { Err(CredentialKeyError::BackupExclusionFailed) }
            }
        }
        #[allow(clippy::needless_update)] // test builds add VaultOptions fields
        CredentialVault::open(store, admission, data_dir, VaultOptions {
            exclusion: Some(Arc::new(TestExclusion(excluded))), ..VaultOptions::default()
        }).map(|vault| Self { vault }).map_err(|error| CredentialVaultEntryError { error })
    }
}

#[cfg(debug_assertions)]
struct BinaryControl {
    probe_failure: bool,
    fail_barrier: Option<usize>,
    barriers: usize,
    cut: Option<&'static str>,
    observer: Box<dyn FnMut(&str)>,
}
#[cfg(debug_assertions)]
thread_local! {
    static BINARY_CONTROL: std::cell::RefCell<Option<BinaryControl>> = const { std::cell::RefCell::new(None) };
}
#[cfg(debug_assertions)]
pub(super) fn test_probe_failure() -> bool {
    BINARY_CONTROL.with(|slot| slot.borrow().as_ref().is_some_and(|control| control.probe_failure))
}
#[cfg(debug_assertions)]
pub(super) fn test_boundary(point: &'static str) -> Result<(), CredentialStoreError> {
    BINARY_CONTROL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(control) = slot.as_mut() else { return Ok(()) };
        (control.observer)(point);
        if point == "credential_barrier" {
            control.barriers += 1;
            if control.fail_barrier == Some(control.barriers) { return Err(CredentialStoreError::DurabilityUnavailable); }
        }
        if control.cut == Some(point) { return Err(CredentialStoreError::DurabilityUnavailable); }
        Ok(())
    })
}
impl CredentialVaultEntryError {
    pub fn code(&self) -> &'static str {
        self.error.code()
    }
}
pub fn open_credential_vault_for_entry(store: HubStore, admission: Arc<AdmittedUserHub>, data_dir: &Path) -> Result<CredentialVaultEntry, CredentialVaultEntryError> {
    CredentialVault::open(store, admission, data_dir, VaultOptions::default()).map(|vault| CredentialVaultEntry { vault }).map_err(|error| CredentialVaultEntryError { error })
}
