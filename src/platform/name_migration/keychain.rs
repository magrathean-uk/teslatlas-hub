// SPDX-License-Identifier: AGPL-3.0-only

//! Keychain items under their current service names (DEC-160, DEC-156 rules).
//!
//! An item is looked up under the current service first. Only when it is
//! missing there is it looked up under its earlier service (and, for the
//! backup password, its earlier account, which is a path under the moved
//! folder). A value found there is written under the current name and read
//! back; only an exact read-back retires the earlier item, and retiring is
//! best effort. The rules the App's rename review set:
//! - a healthy load never fails: the found value is returned even when the
//!   copy, the read-back or the retirement fails, and the copy is retried on
//!   the next read;
//! - a Keychain that cannot be read now (locked, or a prompt would be needed)
//!   defers the step: the read reports what it reports today, nothing latches
//!   and nothing is deleted;
//! - the current item is authoritative: once it exists, earlier items are
//!   retired whatever bytes they hold, and a save retires them too, so no
//!   stale secret stays behind.

use std::sync::Arc;

use zeroize::Zeroizing;

use crate::collection::window::{AnchorStore, Refusal};
use crate::runtime::backup_schedule::keychain::{KeychainFailure, PasswordKeychain};

/// The backup password store: the current item first, then the earlier ones.
pub struct MigratingPasswordKeychain {
    current: Arc<dyn PasswordKeychain>,
    /// The store under the earlier service name.
    earlier_service: Arc<dyn PasswordKeychain>,
}

impl MigratingPasswordKeychain {
    pub fn new(current: Arc<dyn PasswordKeychain>, earlier_service: Arc<dyn PasswordKeychain>) -> Self {
        Self { current, earlier_service }
    }

    /// Where an earlier copy of `account` may be: the earlier service under
    /// the same account, then under the account's earlier spelling (a path
    /// in the moved folder), then the current service under that account.
    fn earlier_items(&self, account: &str) -> Vec<(&dyn PasswordKeychain, String)> {
        let mut items: Vec<(&dyn PasswordKeychain, String)> = vec![(self.earlier_service.as_ref(), account.to_owned())];
        if let Some(earlier_account) = super::spelling::earlier(account) {
            items.push((self.earlier_service.as_ref(), earlier_account.clone()));
            items.push((self.current.as_ref(), earlier_account));
        }
        items
    }

    fn retire_earlier(&self, account: &str) {
        for (store, earlier_account) in self.earlier_items(account) {
            let _ = store.delete(&earlier_account);
        }
    }
}

impl PasswordKeychain for MigratingPasswordKeychain {
    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, KeychainFailure> {
        match self.current.read(account) {
            Ok(value) => {
                self.retire_earlier(account);
                return Ok(value);
            }
            Err(KeychainFailure::Missing) => {}
            Err(failure) => return Err(failure),
        }
        let mut deferred = None;
        for (store, earlier_account) in self.earlier_items(account) {
            match store.read(&earlier_account) {
                Ok(value) => {
                    let copied = self.current.write(account, &value).is_ok()
                        && self.current.read(account).is_ok_and(|saved| *saved == *value);
                    if copied {
                        let _ = store.delete(&earlier_account);
                    }
                    return Ok(value);
                }
                Err(KeychainFailure::Missing) => {}
                Err(failure) => deferred = deferred.or(Some(failure)),
            }
        }
        Err(deferred.unwrap_or(KeychainFailure::Missing))
    }

    fn write(&self, account: &str, password: &[u8]) -> Result<(), KeychainFailure> {
        self.current.write(account, password)?;
        if self.current.read(account).is_ok_and(|saved| *saved == *password) {
            self.retire_earlier(account);
        }
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), KeychainFailure> {
        self.retire_earlier(account);
        self.current.delete(account)
    }
}

/// A window anchor read through its earlier service when the current one has
/// none; the same rules as the password item. Anchors are high-water marks, so
/// an earlier value is copied, never merged, and only into an empty slot.
pub fn read_anchor(current: &dyn AnchorStore, earlier: &dyn AnchorStore, name: &str) -> Result<Option<String>, Refusal> {
    if let Some(value) = current.read(name)? {
        let _ = earlier.retire(name);
        return Ok(Some(value));
    }
    let Some(value) = earlier.read(name)? else { return Ok(None) };
    if current.write(name, &value).is_ok() && current.read(name).is_ok_and(|saved| saved.as_deref() == Some(value.as_str())) {
        let _ = earlier.retire(name);
    }
    Ok(Some(value))
}

/// A window anchor write: the current item, then best-effort retirement.
pub fn write_anchor(current: &dyn AnchorStore, earlier: &dyn AnchorStore, name: &str, value: &str) -> Result<(), Refusal> {
    current.write(name, value)?;
    if current.read(name).is_ok_and(|saved| saved.as_deref() == Some(value)) {
        let _ = earlier.retire(name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex};

    use super::*;
    use crate::collection::window::MemoryAnchor;

    /// An injected password store; failures can be set per operation.
    #[derive(Default)]
    struct Store {
        items: Mutex<HashMap<String, Vec<u8>>>,
        read_failure: Mutex<Option<KeychainFailure>>,
        write_fails: Mutex<bool>,
        delete_fails: Mutex<bool>,
        read_back_differs: Mutex<bool>,
        reads: Mutex<usize>,
    }

    impl Store {
        fn with(account: &str, value: &[u8]) -> Arc<Self> {
            let store = Self::default();
            store.items.lock().unwrap().insert(account.to_owned(), value.to_vec());
            Arc::new(store)
        }
        fn get(&self, account: &str) -> Option<Vec<u8>> {
            self.items.lock().unwrap().get(account).cloned()
        }
    }

    impl PasswordKeychain for Store {
        fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, KeychainFailure> {
            *self.reads.lock().unwrap() += 1;
            if let Some(failure) = *self.read_failure.lock().unwrap() {
                return Err(failure);
            }
            let mut value = self.get(account).ok_or(KeychainFailure::Missing)?;
            if *self.read_back_differs.lock().unwrap() && *self.reads.lock().unwrap() > 1 {
                value.push(b'!');
            }
            Ok(Zeroizing::new(value))
        }
        fn write(&self, account: &str, password: &[u8]) -> Result<(), KeychainFailure> {
            if *self.write_fails.lock().unwrap() {
                return Err(KeychainFailure::Refused);
            }
            self.items.lock().unwrap().insert(account.to_owned(), password.to_vec());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), KeychainFailure> {
            if *self.delete_fails.lock().unwrap() {
                return Err(KeychainFailure::Unreadable);
            }
            self.items.lock().unwrap().remove(account);
            Ok(())
        }
    }

    const ACCOUNT: &str = "/Users/owner/Library/Application Support/Tesatlas Hub/backup-password";
    const EARLIER_ACCOUNT: &str = "/Users/owner/Library/Application Support/Teslatlas Hub/backup-password"; // rename:keep
    const PASSWORD: &[u8] = b"correct horse battery";

    fn keychain(current: &Arc<Store>, earlier: &Arc<Store>) -> MigratingPasswordKeychain {
        MigratingPasswordKeychain::new(current.clone(), earlier.clone())
    }

    #[test]
    fn an_earlier_item_is_copied_read_back_and_then_retired() {
        let current = Arc::new(Store::default());
        let earlier = Store::with(EARLIER_ACCOUNT, PASSWORD);
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), PASSWORD);
        assert_eq!(current.get(ACCOUNT).as_deref(), Some(PASSWORD));
        assert_eq!(earlier.get(EARLIER_ACCOUNT), None);
        // A second launch reads the current item and finds nothing earlier.
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), PASSWORD);
    }

    #[test]
    fn the_same_account_under_the_earlier_service_is_found_too() {
        let current = Arc::new(Store::default());
        let earlier = Store::with("unattended-backup", PASSWORD);
        assert_eq!(&*keychain(&current, &earlier).read("unattended-backup").unwrap(), PASSWORD);
        assert_eq!(current.get("unattended-backup").as_deref(), Some(PASSWORD));
        assert_eq!(earlier.get("unattended-backup"), None);
    }

    #[test]
    fn a_failed_copy_still_returns_the_value_and_keeps_the_earlier_item() {
        let current = Arc::new(Store::default());
        *current.write_fails.lock().unwrap() = true;
        let earlier = Store::with(EARLIER_ACCOUNT, PASSWORD);
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), PASSWORD);
        assert_eq!(earlier.get(EARLIER_ACCOUNT).as_deref(), Some(PASSWORD));
        // Interrupted: the next launch, with the Keychain writable again, finishes.
        *current.write_fails.lock().unwrap() = false;
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), PASSWORD);
        assert_eq!(earlier.get(EARLIER_ACCOUNT), None);
    }

    #[test]
    fn a_read_back_that_differs_never_retires_the_earlier_item() {
        let current = Arc::new(Store::default());
        *current.read_back_differs.lock().unwrap() = true;
        let earlier = Store::with(EARLIER_ACCOUNT, PASSWORD);
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), PASSWORD);
        assert_eq!(earlier.get(EARLIER_ACCOUNT).as_deref(), Some(PASSWORD));
    }

    #[test]
    fn a_failed_retirement_never_fails_a_healthy_load() {
        let current = Arc::new(Store::default());
        let earlier = Store::with(EARLIER_ACCOUNT, PASSWORD);
        *earlier.delete_fails.lock().unwrap() = true;
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), PASSWORD);
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), PASSWORD);
        *earlier.delete_fails.lock().unwrap() = false;
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), PASSWORD);
        assert_eq!(earlier.get(EARLIER_ACCOUNT), None, "retried on a later healthy read");
    }

    #[test]
    fn a_locked_keychain_defers_without_deleting_anything() {
        let current = Arc::new(Store::default());
        let earlier = Store::with(EARLIER_ACCOUNT, PASSWORD);
        *earlier.read_failure.lock().unwrap() = Some(KeychainFailure::Refused);
        assert_eq!(keychain(&current, &earlier).read(ACCOUNT).err(), Some(KeychainFailure::Refused));
        assert_eq!(current.get(ACCOUNT), None);
        *earlier.read_failure.lock().unwrap() = None;
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), PASSWORD);
        let current_locked = Arc::new(Store::default());
        *current_locked.read_failure.lock().unwrap() = Some(KeychainFailure::Refused);
        let earlier = Store::with(EARLIER_ACCOUNT, PASSWORD);
        assert_eq!(keychain(&current_locked, &earlier).read(ACCOUNT).err(), Some(KeychainFailure::Refused));
        assert_eq!(earlier.get(EARLIER_ACCOUNT).as_deref(), Some(PASSWORD));
    }

    #[test]
    fn when_both_exist_the_current_item_wins_and_the_earlier_one_is_retired() {
        let current = Store::with(ACCOUNT, b"the newer password");
        let earlier = Store::with(EARLIER_ACCOUNT, PASSWORD);
        assert_eq!(&*keychain(&current, &earlier).read(ACCOUNT).unwrap(), b"the newer password");
        assert_eq!(earlier.get(EARLIER_ACCOUNT), None);
    }

    #[test]
    fn a_save_retires_the_earlier_item_and_nothing_found_is_missing() {
        let current = Arc::new(Store::default());
        let earlier = Store::with(EARLIER_ACCOUNT, PASSWORD);
        keychain(&current, &earlier).write(ACCOUNT, b"a replacement password").unwrap();
        assert_eq!(earlier.get(EARLIER_ACCOUNT), None);
        let empty = Arc::new(Store::default());
        assert_eq!(keychain(&empty, &Arc::new(Store::default())).read(ACCOUNT).err(), Some(KeychainFailure::Missing));
    }

    #[test]
    fn a_window_anchor_moves_with_read_back_and_both_exist_keeps_the_current() {
        let current = MemoryAnchor::new();
        let earlier = MemoryAnchor::new();
        earlier.write("consumed", "41").unwrap();
        assert_eq!(read_anchor(&current, &earlier, "consumed").unwrap().as_deref(), Some("41"));
        assert_eq!(current.read("consumed").unwrap().as_deref(), Some("41"));
        assert_eq!(earlier.read("consumed").unwrap(), None);
        assert_eq!(read_anchor(&current, &earlier, "consumed").unwrap().as_deref(), Some("41"));
        earlier.write("consumed", "7").unwrap();
        assert_eq!(read_anchor(&current, &earlier, "consumed").unwrap().as_deref(), Some("41"));
        assert_eq!(earlier.read("consumed").unwrap(), None);
    }

    #[test]
    fn a_window_anchor_copy_that_fails_keeps_the_earlier_value_and_returns_it() {
        let current = MemoryAnchor::new();
        current.set_fail_writes(true);
        let earlier = MemoryAnchor::new();
        earlier.write("pin", "abc").unwrap();
        assert_eq!(read_anchor(&current, &earlier, "pin").unwrap().as_deref(), Some("abc"));
        assert_eq!(earlier.read("pin").unwrap().as_deref(), Some("abc"));
        current.set_fail_writes(false);
        write_anchor(&current, &earlier, "pin", "abd").unwrap();
        assert_eq!(earlier.read("pin").unwrap(), None);
    }
}
