// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(any(test, debug_assertions))]
use std::cell::RefCell;
use std::{ops::{Deref, DerefMut}, sync::Arc};
use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, Clone, Copy)]
pub struct SecretDropEvent {
    pub kind: &'static str,
    pub before: usize,
    pub after: usize,
    pub zeroised: bool,
}
pub(crate) type SecretDropObserver = Arc<dyn Fn(SecretDropEvent) + Send + Sync>;
// DEC-065 rule 2: the observation bridge exists only in debug and test builds;
// a release build has no observer slot, so nothing can be installed.
#[cfg(any(test, debug_assertions))]
thread_local! {
    static OBSERVER: RefCell<Option<SecretDropObserver>> = const { RefCell::new(None) };
}
#[cfg(any(test, debug_assertions))]
pub(crate) fn current_secret_observer() -> Option<SecretDropObserver> { OBSERVER.with(|slot| slot.borrow().clone()) }
#[cfg(not(any(test, debug_assertions)))]
pub(crate) fn current_secret_observer() -> Option<SecretDropObserver> { None }
pub(crate) fn note_secret_drop(observer: &Option<SecretDropObserver>, kind: &'static str, before: usize, after: usize, zeroised: bool) {
    if let Some(observer) = observer {
        let event = SecretDropEvent { kind, before, after, zeroised };
        let _outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(event)));
    }
}

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn observe_sign_in_secret_drops<T>(observer: impl Fn(SecretDropEvent) + Send + Sync + 'static, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<SecretDropObserver>);
    impl Drop for Restore { fn drop(&mut self) { OBSERVER.with(|slot| *slot.borrow_mut() = self.0.take()); } }
    let restore = Restore(OBSERVER.with(|slot| slot.replace(Some(Arc::new(observer)))));
    let result = work();
    drop(restore);
    result
}

/// Release builds compile no observer: the work runs and the observer is dropped unused.
#[cfg(not(any(test, debug_assertions)))]
#[doc(hidden)]
pub fn observe_sign_in_secret_drops<T>(observer: impl Fn(SecretDropEvent) + Send + Sync + 'static, work: impl FnOnce() -> T) -> T {
    drop(observer);
    work()
}

/// A wiping text allocation whose drop proof follows it across threads.
pub struct SecretText {
    value: Zeroizing<String>,
    kind: &'static str,
    observer: Option<SecretDropObserver>,
}
impl SecretText {
    pub(crate) fn new(kind: &'static str, capacity: usize) -> Self {
        Self { value: Zeroizing::new(String::with_capacity(capacity)), kind, observer: current_secret_observer() }
    }
    #[cfg(any(test, feature = "journey-doubles"))]
    pub(crate) fn copy(kind: &'static str, text: &str) -> Self {
        let mut value = Self::new(kind, text.len()); value.value.push_str(text); value
    }
    pub(crate) fn buffer_mut(&mut self) -> &mut String { &mut self.value }
    pub fn as_str(&self) -> &str { &self.value }
}
impl Deref for SecretText { type Target = str; fn deref(&self) -> &str { &self.value } }
impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("SecretText").field("length", &self.value.len()).finish() }
}
impl Drop for SecretText {
    fn drop(&mut self) {
        let before = self.value.len(); self.value.zeroize();
        note_secret_drop(&self.observer, self.kind, before, self.value.len(), self.value.is_empty());
    }
}

pub struct SecretBuffer {
    value: Zeroizing<Vec<u8>>,
    kind: &'static str,
    observer: Option<SecretDropObserver>,
}
impl SecretBuffer {
    pub(crate) fn new(kind: &'static str, capacity: usize) -> Self {
        Self { value: Zeroizing::new(Vec::with_capacity(capacity)), kind, observer: current_secret_observer() }
    }
    pub(crate) fn copy(kind: &'static str, bytes: &[u8]) -> Self {
        let mut value = Self::new(kind, bytes.len()); value.value.extend_from_slice(bytes); value
    }

    /// Makes room for `additional` more bytes without leaving a copy behind:
    /// when the allocation must grow, the bytes move into a new allocation of
    /// the full size and the old one is wiped before it is freed (M09-T19
    /// ruling 8). A push within the reserved room never reallocates.
    pub(crate) fn reserve_wiping(&mut self, additional: usize) {
        let needed = self.value.len().saturating_add(additional);
        if needed <= self.value.capacity() { return; }
        let mut grown = Zeroizing::new(Vec::with_capacity(needed.max(self.value.capacity().saturating_mul(2))));
        grown.extend_from_slice(&self.value);
        // The old allocation is wiped in full by its own drop.
        drop(std::mem::replace(&mut self.value, grown));
    }
}
impl Deref for SecretBuffer { type Target = Vec<u8>; fn deref(&self) -> &Vec<u8> { &self.value } }
impl DerefMut for SecretBuffer { fn deref_mut(&mut self) -> &mut Vec<u8> { &mut self.value } }
impl AsRef<[u8]> for SecretBuffer { fn as_ref(&self) -> &[u8] { &self.value } }
impl std::fmt::Debug for SecretBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("SecretBuffer").field("length", &self.value.len()).finish() }
}
impl Drop for SecretBuffer {
    fn drop(&mut self) {
        let before = self.value.len(); self.value.zeroize();
        note_secret_drop(&self.observer, self.kind, before, self.value.len(), self.value.is_empty());
    }
}
