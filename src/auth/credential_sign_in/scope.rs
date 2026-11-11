// SPDX-License-Identifier: AGPL-3.0-only

use std::{path::Path, sync::Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use crate::{CredentialVaultEntry, CredentialVaultEntryError, db::HubStore, hub_user_process::AdmittedUserHub};
use super::messages::{CredentialCommandError, credential_entry_error};

/// All admission-dependent resources end before the service restart.
pub struct SignInScope {
    entry: Option<CredentialVaultEntry>,
    store: Option<HubStore>,
    admission: Option<Arc<AdmittedUserHub>>,
    release_failed: Arc<AtomicBool>,
}

impl SignInScope {
    pub fn open(data_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::open_using(data_dir, crate::open_credential_vault_for_entry)
    }

    fn open_using(
        data_dir: &Path,
        open: impl FnOnce(HubStore, Arc<AdmittedUserHub>, &Path) -> Result<CredentialVaultEntry, CredentialVaultEntryError>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let admission = AdmittedUserHub::admit(data_dir)?;
        Self::from_admission_using(data_dir, admission, open, Arc::new(AtomicBool::new(false)))
    }

    pub(super) fn from_admission_using(
        data_dir: &Path,
        admission: Arc<AdmittedUserHub>,
        open: impl FnOnce(HubStore, Arc<AdmittedUserHub>, &Path) -> Result<CredentialVaultEntry, CredentialVaultEntryError>,
        release_failed: Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut scope = Self { entry: None, store: None, admission: Some(admission), release_failed };
        let store = HubStore::initialize(data_dir).map_err(|error| CredentialCommandError::from(super::super::credential_vault::CredentialStoreError::from(error)))?;
        scope.store = Some(store);
        scope.entry = Some(open(scope.store.as_ref().expect("live store").clone(), Arc::clone(scope.admission.as_ref().expect("live admission")), data_dir).map_err(credential_entry_error)?);
        Ok(scope)
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn open_for_test(data_dir: &Path, excluded: bool) -> Result<Self, Box<dyn std::error::Error>> {
        Self::open_using(data_dir, |store, admission, data_dir| CredentialVaultEntry::open_for_test(store, admission, data_dir, excluded))
    }

    pub fn entry(&self) -> &CredentialVaultEntry { self.entry.as_ref().expect("live sign-in scope") }

    pub fn release(mut self) -> Result<(), CredentialCommandError> {
        self.release_fields();
        if self.release_failed.load(Ordering::Acquire) { Err(CredentialCommandError::new("sign_in_service_pause_failed")) } else { Ok(()) }
    }

    fn release_fields(&mut self) {
        drop(self.entry.take());
        drop(self.store.take());
        if let Some(admission) = self.admission.take() {
            match Arc::try_unwrap(admission) {
                Ok(admission) => drop(admission),
                Err(_) => self.release_failed.store(true, Ordering::Release),
            }
        }
    }
}

impl Drop for SignInScope {
    fn drop(&mut self) {
        self.release_fields();
    }
}
