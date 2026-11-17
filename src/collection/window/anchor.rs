// SPDX-License-Identifier: AGPL-3.0-only

//! Small values pinned outside the data directory and every backup (login
//! keychain): the issuer's ledger head pin (ruling R2) and each Hub's
//! consumption anchor (ruling R3). The values are counters and digests, never
//! secrets.

use std::{collections::HashMap, process::Command, sync::Mutex};

use super::refusal::Refusal;

pub trait AnchorStore: Send + Sync {
    fn read(&self, name: &str) -> Result<Option<String>, Refusal>;
    fn write(&self, name: &str, value: &str) -> Result<(), Refusal>;
    /// Removes the value; only the rename step retires an earlier item with
    /// it (DEC-160), best effort. A store that cannot remove keeps the value.
    fn retire(&self, _name: &str) -> Result<(), Refusal> {
        Ok(())
    }
}

/// In-memory store for tests and injected use.
#[derive(Debug, Default)]
pub struct MemoryAnchor {
    values: Mutex<HashMap<String, String>>,
    fail_writes: Mutex<bool>,
    fail_reads: Mutex<bool>,
}

impl MemoryAnchor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes later writes fail, to test failed pinning.
    pub fn set_fail_writes(&self, fail: bool) {
        *self.fail_writes.lock().expect("anchor lock") = fail;
    }

    /// Makes later reads fail, to test an unreadable anchor.
    pub fn set_fail_reads(&self, fail: bool) {
        *self.fail_reads.lock().expect("anchor lock") = fail;
    }

    /// Forgets a value, to test a missing anchor.
    pub fn remove(&self, name: &str) {
        self.values.lock().expect("anchor lock").remove(name);
    }
}

impl AnchorStore for MemoryAnchor {
    fn read(&self, name: &str) -> Result<Option<String>, Refusal> {
        if *self.fail_reads.lock().expect("anchor lock") {
            return Err(Refusal::AnchorUnavailable);
        }
        Ok(self.values.lock().expect("anchor lock").get(name).cloned())
    }

    fn write(&self, name: &str, value: &str) -> Result<(), Refusal> {
        if *self.fail_writes.lock().expect("anchor lock") {
            return Err(Refusal::AnchorUnavailable);
        }
        self.values
            .lock()
            .expect("anchor lock")
            .insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    fn retire(&self, name: &str) -> Result<(), Refusal> {
        self.remove(name);
        Ok(())
    }
}

/// The login keychain through `/usr/bin/security`.
#[derive(Debug, Clone)]
pub struct KeychainAnchor {
    service: String,
}

const SECURITY: &str = "/usr/bin/security";
const SECURITY_ITEM_NOT_FOUND: i32 = 44;

impl KeychainAnchor {
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_owned(),
        }
    }

    pub(crate) fn read_args(&self, name: &str) -> Vec<String> {
        [
            "find-generic-password",
            "-s",
            &self.service,
            "-a",
            name,
            "-w",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    pub(crate) fn write_args(&self, name: &str, value: &str) -> Vec<String> {
        [
            "add-generic-password",
            "-U",
            "-s",
            &self.service,
            "-a",
            name,
            "-w",
            value,
        ]
        .map(str::to_owned)
        .to_vec()
    }
}

impl KeychainAnchor {
    /// Runs `security`. A rename step (DEC-160) passes its unlock check: the
    /// child starts only when the keychain is unlocked just before, and the
    /// step stops waiting for it at a deadline (the child is left to end by
    /// itself, never killed), so it never holds a load or a write up.
    fn security(&self, args: Vec<String>, rename_step: Option<&dyn Fn() -> bool>) -> Result<std::process::Output, Refusal> {
        let mut command = Command::new(SECURITY);
        command.args(args);
        let output = match rename_step {
            Some(unlocked) => crate::platform::name_migration::keychain::bounded_output(&mut command, unlocked),
            None => command.output(),
        };
        output.map_err(|_| Refusal::AnchorUnavailable)
    }

    fn read_own(&self, name: &str, rename_step: Option<&dyn Fn() -> bool>) -> Result<Option<String>, Refusal> {
        let output = self.security(self.read_args(name), rename_step)?;
        match output.status.code() {
            Some(0) => Ok(Some(
                String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            )),
            Some(SECURITY_ITEM_NOT_FOUND) => Ok(None),
            _ => Err(Refusal::AnchorUnavailable),
        }
    }

    fn write_own(&self, name: &str, value: &str) -> Result<(), Refusal> {
        if self.security(self.write_args(name, value), None)?.status.success() {
            Ok(())
        } else {
            Err(Refusal::AnchorUnavailable)
        }
    }

    /// The same item under the service's earlier spelling, after the swap
    /// (DEC-160); `None` before it.
    fn earlier(&self) -> Option<Self> {
        crate::platform::name_migration::keychain::earlier_service(&self.service).map(|service| Self { service })
    }

    /// Runs `step` with this service's item as a rename step: a no-prompt
    /// existence check, then an unlock check immediately before any bounded
    /// `security` read, and retirement with the Keychain's user interface off
    /// (DEC-160).
    fn rename_step<T>(&self, step: impl FnOnce(&dyn AnchorStore) -> T) -> T {
        use crate::platform::name_migration::keychain::{AnchorStep, SystemNoPromptItems};
        let read_value = |name: &str, unlocked: &dyn Fn() -> bool| self.read_own(name, Some(unlocked));
        step(&AnchorStep { service: &self.service, items: &SystemNoPromptItems, read_value: &read_value })
    }
}

/// One service name's own item, as the Hub has always read and written it.
struct OwnItem<'a>(&'a KeychainAnchor);

impl AnchorStore for OwnItem<'_> {
    fn read(&self, name: &str) -> Result<Option<String>, Refusal> {
        self.0.read_own(name, None)
    }
    fn write(&self, name: &str, value: &str) -> Result<(), Refusal> {
        self.0.write_own(name, value)
    }
}

/// Reads and writes the current service's item; after the swap an item still
/// under the earlier service is read in its place and retired after the next
/// write (DEC-160).
impl AnchorStore for KeychainAnchor {
    fn read(&self, name: &str) -> Result<Option<String>, Refusal> {
        match self.earlier() {
            Some(earlier) => earlier.rename_step(|earlier_step| {
                crate::platform::name_migration::keychain::read_anchor(&OwnItem(self), earlier_step, name)
            }),
            None => self.read_own(name, None),
        }
    }

    fn write(&self, name: &str, value: &str) -> Result<(), Refusal> {
        match self.earlier() {
            Some(earlier) => self.rename_step(|current_step| {
                earlier.rename_step(|earlier_step| {
                    crate::platform::name_migration::keychain::write_anchor(&OwnItem(self), current_step, earlier_step, name, value)
                })
            }),
            None => self.write_own(name, value),
        }
    }
}
