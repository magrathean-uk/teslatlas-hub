// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(any(test, debug_assertions))]
use std::{cell::RefCell, rc::Rc};
use std::ops::{Deref, DerefMut};
use zeroize::{Zeroize, ZeroizeOnDrop};

// DEC-065 rule 2: the observation bridge exists only in debug and test builds;
// a release build has no observer slot, so nothing can be installed.
#[cfg(any(test, debug_assertions))]
type DropObserver = Rc<dyn Fn(usize, usize)>;
#[cfg(any(test, debug_assertions))]
thread_local! {
    static FILE_TOKEN_DROPS: RefCell<Option<DropObserver>> = const { RefCell::new(None) };
}

/// Observe lengths before and after wiping. The observer never receives bytes.
/// Its scope is local to this thread and is restored on every return path.
#[cfg(any(test, debug_assertions))]
pub fn observe_file_token_drops<T>(
    observer: impl Fn(usize, usize) + 'static,
    action: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<DropObserver>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FILE_TOKEN_DROPS.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let restore = Restore(FILE_TOKEN_DROPS.with(|slot| {
        slot.replace(Some(Rc::new(observer)))
    }));
    let result = action();
    drop(restore);
    result
}

/// Release builds compile no observer: the action runs and the observer is dropped unused.
#[cfg(not(any(test, debug_assertions)))]
pub fn observe_file_token_drops<T>(
    observer: impl Fn(usize, usize) + 'static,
    action: impl FnOnce() -> T,
) -> T {
    drop(observer);
    action()
}

/// Own the allocation before the first decoded byte is copied into it.
pub(crate) struct DecodedFileToken {
    value: String,
}

impl Deref for DecodedFileToken {
    type Target = String;
    fn deref(&self) -> &String { &self.value }
}

impl DerefMut for DecodedFileToken {
    fn deref_mut(&mut self) -> &mut String { &mut self.value }
}

impl Zeroize for DecodedFileToken {
    fn zeroize(&mut self) { self.value.zeroize(); }
}

impl ZeroizeOnDrop for DecodedFileToken {}

impl Drop for DecodedFileToken {
    fn drop(&mut self) {
        #[cfg(any(test, debug_assertions))]
        let length = self.value.len();
        self.zeroize();
        #[cfg(any(test, debug_assertions))]
        {
            let observer = FILE_TOKEN_DROPS.with(|slot| slot.borrow().clone());
            if let Some(observer) = observer { observer(length, self.value.len()); }
        }
    }
}

impl std::fmt::Debug for DecodedFileToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("DecodedFileToken").field("length", &self.value.len()).finish()
    }
}

pub(crate) fn decoded_file_token(text: &str) -> DecodedFileToken {
    let mut decoded = DecodedFileToken { value: String::with_capacity(text.len()) };
    decoded.value.push_str(text);
    decoded
}
