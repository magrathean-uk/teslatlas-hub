// SPDX-License-Identifier: AGPL-3.0-only

//! Keychain items under their current service names (DEC-160, DEC-156 rules).
//!
//! Before the swap nothing here is used: [`earlier_service`] is `None` and
//! every store is the plain one. After it an item is looked up under the
//! current service first, and only when it is missing there under its earlier
//! service. The rules the App's rename review set:
//! - a healthy load never fails and never waits on a new step;
//! - a Keychain that cannot be read now (locked, or a prompt would be needed)
//!   defers the step: the read reports what it reports today, nothing latches
//!   and nothing is deleted;
//! - an earlier item is retired only after the current one reads back with
//!   exactly the same bytes, and retiring is best effort;
//! - the current item is authoritative: once it exists, earlier items are
//!   retired whatever bytes they hold, so no stale secret stays behind.
//!
//! The backup password (Security framework, user interface turned off) is
//! copied on first read and read back before the earlier item is retired.
//! A window anchor goes through `/usr/bin/security`, which cannot turn the
//! user interface off, so its load path adds no change at all: a missing
//! current anchor is read from the earlier item through a bounded step, and
//! the earlier item is retired only after the next current write reads back,
//! each step stopped at [`STEP_DEADLINE`].

use std::{
    io,
    process::{Command, Output, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

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

/// The service's earlier spelling after the swap; `None` before it, so no
/// Keychain step is added to this build's reads and writes.
pub fn earlier_service(service: &str) -> Option<String> {
    super::spelling::renamed().then(|| super::spelling::earlier(service)).flatten()
}

/// The backup password store for `service`: after the swap the current item
/// first, then the earlier ones ([`MigratingPasswordKeychain`]); before it,
/// `current` itself and nothing else.
pub fn password_store(
    current: Arc<dyn PasswordKeychain>,
    service: &str,
    earlier: impl FnOnce(String) -> Arc<dyn PasswordKeychain>,
) -> Arc<dyn PasswordKeychain> {
    match earlier_service(service) {
        Some(earlier_service) => Arc::new(MigratingPasswordKeychain::new(current, earlier(earlier_service))),
        None => current,
    }
}

/// How long one of the rename's own `security` steps may take.
pub const STEP_DEADLINE: Duration = Duration::from_secs(5);

/// Runs `command` (a rename step's `security` call) within [`STEP_DEADLINE`].
pub fn bounded_output(command: &mut Command) -> io::Result<Output> {
    bounded_output_within(command, STEP_DEADLINE)
}

/// Runs `command` with no input; a child still running at `deadline` is
/// stopped and the step reports a time-out, so it never holds a caller up.
pub fn bounded_output_within(command: &mut Command, deadline: Duration) -> io::Result<Output> {
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if started.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the Keychain step did not finish in time"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A window anchor after the swap. The load path adds no Keychain change: the
/// current item is read as always and, only when it is missing, the earlier
/// item is read in its place through a bounded `earlier_step`. A time-out or
/// a refusal there is reported as unavailable (never as a missing anchor),
/// and nothing is written or removed. Anchors are high-water marks: an
/// earlier value is never merged, and the current one wins when both exist.
pub fn read_anchor(current: &dyn AnchorStore, earlier_step: &dyn AnchorStore, name: &str) -> Result<Option<String>, Refusal> {
    match current.read(name)? {
        Some(value) => Ok(Some(value)),
        None => earlier_step.read(name),
    }
}

/// A window anchor write after the swap: the current item is written as
/// always, which is the copy; only once a bounded read-back returns exactly
/// the written value is the earlier item retired (bounded, best effort). A
/// step that cannot finish keeps the earlier item for the next write.
pub fn write_anchor(
    current: &dyn AnchorStore,
    current_step: &dyn AnchorStore,
    earlier_step: &dyn AnchorStore,
    name: &str,
    value: &str,
) -> Result<(), Refusal> {
    current.write(name, value)?;
    if current_step.read(name).is_ok_and(|saved| saved.as_deref() == Some(value)) {
        let _ = earlier_step.retire(name);
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

    /// An injected anchor item: each rename step can fail, as a Keychain that
    /// is locked, needs a prompt or times out would.
    #[derive(Default)]
    struct Item {
        values: MemoryAnchor,
        fail_reads: Mutex<bool>,
        fail_retire: Mutex<bool>,
        read_back_differs: Mutex<bool>,
        changes: Mutex<usize>,
    }

    impl AnchorStore for Item {
        fn read(&self, name: &str) -> Result<Option<String>, Refusal> {
            if *self.fail_reads.lock().unwrap() {
                return Err(Refusal::AnchorUnavailable);
            }
            let value = self.values.read(name)?;
            Ok(if *self.read_back_differs.lock().unwrap() { value.map(|value| format!("{value}0")) } else { value })
        }
        fn write(&self, name: &str, value: &str) -> Result<(), Refusal> {
            *self.changes.lock().unwrap() += 1;
            self.values.write(name, value)
        }
        fn retire(&self, name: &str) -> Result<(), Refusal> {
            *self.changes.lock().unwrap() += 1;
            if *self.fail_retire.lock().unwrap() {
                return Err(Refusal::AnchorUnavailable);
            }
            self.values.remove(name);
            Ok(())
        }
    }

    fn stored(item: &Item, name: &str) -> Option<String> {
        item.values.read(name).unwrap()
    }

    #[test]
    fn a_window_anchor_read_falls_back_to_the_earlier_item_and_changes_nothing() {
        let (current, earlier) = (Item::default(), Item::default());
        earlier.values.write("consumed", "41").unwrap();
        assert_eq!(read_anchor(&current, &earlier, "consumed").unwrap().as_deref(), Some("41"));
        assert_eq!((*current.changes.lock().unwrap(), *earlier.changes.lock().unwrap()), (0, 0));
        // Both exist: the current value wins, still without any change.
        current.values.write("consumed", "42").unwrap();
        assert_eq!(read_anchor(&current, &earlier, "consumed").unwrap().as_deref(), Some("42"));
        assert_eq!(stored(&earlier, "consumed").as_deref(), Some("41"));
        assert_eq!(read_anchor(&Item::default(), &Item::default(), "consumed").unwrap(), None);
    }

    #[test]
    fn an_earlier_item_that_cannot_be_read_now_is_unavailable_never_missing() {
        let (current, earlier) = (Item::default(), Item::default());
        earlier.values.write("pin", "abc").unwrap();
        *earlier.fail_reads.lock().unwrap() = true;
        assert_eq!(read_anchor(&current, &earlier, "pin").err(), Some(Refusal::AnchorUnavailable));
        *earlier.fail_reads.lock().unwrap() = false;
        assert_eq!(read_anchor(&current, &earlier, "pin").unwrap().as_deref(), Some("abc"));
    }

    #[test]
    fn a_window_anchor_write_retires_the_earlier_item_only_after_an_exact_read_back() {
        let (current, earlier) = (Item::default(), Item::default());
        earlier.values.write("consumed", "41").unwrap();
        *current.read_back_differs.lock().unwrap() = true;
        write_anchor(&current, &current, &earlier, "consumed", "42").unwrap();
        assert_eq!(stored(&earlier, "consumed").as_deref(), Some("41"), "kept: the read-back differed");
        *current.read_back_differs.lock().unwrap() = false;
        *current.fail_reads.lock().unwrap() = true;
        write_anchor(&current, &current, &earlier, "consumed", "43").unwrap();
        assert_eq!(stored(&earlier, "consumed").as_deref(), Some("41"), "kept: the read-back did not finish");
        *current.fail_reads.lock().unwrap() = false;
        write_anchor(&current, &current, &earlier, "consumed", "44").unwrap();
        assert_eq!(stored(&earlier, "consumed"), None);
        assert_eq!(stored(&current, "consumed").as_deref(), Some("44"));
    }

    #[test]
    fn a_retirement_that_cannot_finish_is_tried_again_by_the_next_write_in_the_same_process() {
        let (current, earlier) = (Item::default(), Item::default());
        earlier.values.write("pin", "abc").unwrap();
        *earlier.fail_retire.lock().unwrap() = true;
        write_anchor(&current, &current, &earlier, "pin", "abd").unwrap();
        assert_eq!(stored(&earlier, "pin").as_deref(), Some("abc"));
        assert_eq!(read_anchor(&current, &earlier, "pin").unwrap().as_deref(), Some("abd"));
        *earlier.fail_retire.lock().unwrap() = false;
        write_anchor(&current, &current, &earlier, "pin", "abe").unwrap();
        assert_eq!(stored(&earlier, "pin"), None);
    }

    #[test]
    fn a_failed_current_write_is_reported_and_keeps_the_earlier_item() {
        let current = MemoryAnchor::new();
        current.set_fail_writes(true);
        let earlier = Item::default();
        earlier.values.write("pin", "abc").unwrap();
        assert_eq!(write_anchor(&current, &current, &earlier, "pin", "abd").err(), Some(Refusal::AnchorUnavailable));
        assert_eq!(stored(&earlier, "pin").as_deref(), Some("abc"));
    }

    #[test]
    fn a_rename_step_that_does_not_finish_is_stopped_at_its_deadline() {
        let started = std::time::Instant::now();
        let mut slow = Command::new("/bin/sleep");
        slow.arg("30");
        let error = bounded_output_within(&mut slow, Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        let mut quick = Command::new("/bin/echo");
        quick.arg("41");
        let output = bounded_output_within(&mut quick, Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"41\n");
    }

    /// REV-CX-M08-T12A finding 8: before the swap no Keychain store gains a
    /// step: the backup store is the plain one and anchors have no earlier item.
    #[test]
    fn the_current_build_adds_no_keychain_call() {
        if super::super::spelling::renamed() {
            return; // after the swap the migration tests above apply
        }
        use crate::runtime::backup_schedule::keychain::KEYCHAIN_SERVICE;
        use crate::runtime::operator::authority::{HUB_ANCHOR_SERVICE, ISSUER_ANCHOR_SERVICE};
        for service in [KEYCHAIN_SERVICE, HUB_ANCHOR_SERVICE, ISSUER_ANCHOR_SERVICE] {
            assert_eq!(earlier_service(service), None, "{service}");
        }
        let current = Store::with("account", PASSWORD);
        let store = password_store(current.clone(), KEYCHAIN_SERVICE, |_| panic!("no earlier store"));
        assert_eq!(&*store.read("account").unwrap(), PASSWORD);
        assert_eq!(store.read("missing").err(), Some(KeychainFailure::Missing));
        assert_eq!(*current.reads.lock().unwrap(), 2, "one call per read, as before");
        let _renamed = super::super::spelling::assume_renamed();
        assert_eq!(earlier_service("Tesatlas Hub Window").as_deref(), Some(HUB_ANCHOR_SERVICE));
    }
}
