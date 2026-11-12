// SPDX-License-Identifier: AGPL-3.0-only

//! Reads the unattended backup password from the Mac Keychain (M06-T12).
//!
//! On the Mac the app stores the recovery password as a generic password in
//! the owner's login keychain: service [`KEYCHAIN_SERVICE`], account the
//! `backup.password_file` path the configuration names (so each Hub
//! configuration has its own item; no file is written there), or
//! [`DEFAULT_ACCOUNT`] when `backup.password_source = "keychain"` names no
//! path (M06-T13). The item's
//! access list trusts exactly the app and its bundled service. The service is
//! the per-user LaunchAgent, so it runs while the owner is logged in and the
//! login keychain is open to it.
//!
//! The service reads with the Keychain's user interface disallowed: it never
//! shows a prompt. A missing item, a refused read (a locked keychain, or a
//! service binary whose code no longer matches the access list, as after an
//! unsigned rebuild or an update) or an unreadable value refuses the run with
//! [`BACKUP_PASSWORD_UNAVAILABLE`] and a message that tells the owner to save
//! the recovery password again in the app. There is no fallback to a file on
//! the Mac. The password is never logged, printed or passed on.

use std::path::Path;

use zeroize::Zeroizing;

use super::password::PASSWORD_FILE_MAX_BYTES;
use crate::runtime::operator::admission::OperatorRefusal;

/// The Keychain item's service attribute; the app writes the same string.
pub const KEYCHAIN_SERVICE: &str = "uk.magrathean.teslatlas.hub.backup";

/// The item's account when the configuration names the Keychain source and
/// no `backup.password_file` (the app always names the path it writes).
pub const DEFAULT_ACCOUNT: &str = "unattended-backup";

/// The run is refused because the service cannot read the password item.
pub const BACKUP_PASSWORD_UNAVAILABLE: &str = "backup_password_unavailable";

/// Why the Keychain gave no password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeychainFailure {
    /// No item for this Hub.
    Missing,
    /// The item exists but this process may not read it without a prompt: the
    /// keychain is locked, or the access list does not trust this binary.
    Refused,
    /// Any other Keychain error, or a value that is not a usable password.
    Unreadable,
}

/// Where the service reads the password item. The service's is
/// [`system_keychain`]; tests supply a store of their own or a disposable
/// keychain, never the login keychain.
pub trait PasswordKeychain: Send + Sync {
    fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, KeychainFailure>;
}

/// Reads the password item for `account` (the configured path): UTF-8 of at
/// most 1 KiB, as a password file holds.
pub fn read_keychain_password(
    keychain: &dyn PasswordKeychain,
    account: &str,
) -> Result<Zeroizing<String>, OperatorRefusal> {
    let bytes = keychain.read(account).map_err(|failure| unavailable(failure, account))?;
    if bytes.is_empty() || bytes.len() > PASSWORD_FILE_MAX_BYTES {
        return Err(unavailable(KeychainFailure::Unreadable, account));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| unavailable(KeychainFailure::Unreadable, account))?;
    Ok(Zeroizing::new(text.to_owned()))
}

/// The refusal the owner sees in the app (its last failure). It names no
/// password and no Keychain internals beyond what the owner can act on.
pub fn unavailable(failure: KeychainFailure, account: &str) -> OperatorRefusal {
    let message = match failure {
        KeychainFailure::Missing
            if Path::new(account).is_absolute() && Path::new(account).exists() =>
        {
            "the backup password is still in a file waiting to move into the Keychain; open Teslatlas Hub to move it, or save the recovery password again in Service details, Backups"
        }
        KeychainFailure::Missing => {
            "the backup password is not in the Keychain; save the recovery password again in Teslatlas Hub (Service details, Backups)"
        }
        KeychainFailure::Refused => {
            "the service may not read the backup password in the Keychain (the keychain is locked, or the Hub was updated or rebuilt since the password was saved); save the recovery password again in Teslatlas Hub (Service details, Backups)"
        }
        KeychainFailure::Unreadable => {
            "the backup password in the Keychain cannot be read; save the recovery password again in Teslatlas Hub (Service details, Backups)"
        }
    };
    OperatorRefusal::new(BACKUP_PASSWORD_UNAVAILABLE, message)
}

/// The service's Keychain: the owner's default keychain search list.
pub fn system_keychain() -> std::sync::Arc<dyn PasswordKeychain> {
    std::sync::Arc::new(SystemKeychain::search_list())
}

#[cfg(target_os = "macos")]
pub use mac::SystemKeychain;

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::Mutex;

    use security_framework::os::macos::{keychain::SecKeychain, passwords::find_generic_password};
    use zeroize::Zeroizing;

    use super::{KEYCHAIN_SERVICE, KeychainFailure, PasswordKeychain};

    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
    const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
    const ERR_SEC_AUTH_FAILED: i32 = -25293;
    const ERR_SEC_NO_SUCH_KEYCHAIN: i32 = -25294;

    /// Turning the Keychain's user interface off is process-wide and its
    /// guard turns it back on, so reads take turns.
    static NO_PROMPT: Mutex<()> = Mutex::new(());

    /// Reads generic passwords through the Security framework.
    pub struct SystemKeychain {
        /// None searches the owner's default list (the login keychain).
        keychains: Option<Vec<SecKeychain>>,
    }

    impl SystemKeychain {
        pub fn search_list() -> Self {
            Self { keychains: None }
        }

        /// Searches only `keychain` (tests use a disposable one).
        pub fn only(keychain: SecKeychain) -> Self {
            Self { keychains: Some(vec![keychain]) }
        }
    }

    pub(crate) fn classify(code: i32) -> KeychainFailure {
        match code {
            ERR_SEC_ITEM_NOT_FOUND | ERR_SEC_NO_SUCH_KEYCHAIN => KeychainFailure::Missing,
            ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_AUTH_FAILED => KeychainFailure::Refused,
            _ => KeychainFailure::Unreadable,
        }
    }

    impl PasswordKeychain for SystemKeychain {
        fn read(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, KeychainFailure> {
            let _turn = NO_PROMPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let _no_prompt =
                SecKeychain::disable_user_interaction().map_err(|_| KeychainFailure::Unreadable)?;
            match find_generic_password(self.keychains.as_deref(), KEYCHAIN_SERVICE, account) {
                Ok((password, _item)) => Ok(Zeroizing::new(password.to_vec())),
                Err(error) => Err(classify(error.code())),
            }
        }
    }
}

#[cfg(all(target_os = "macos", test))]
pub(crate) use mac::classify;

/// Elsewhere there is no Keychain: the password comes from its file.
#[cfg(not(target_os = "macos"))]
pub struct SystemKeychain;

#[cfg(not(target_os = "macos"))]
impl SystemKeychain {
    pub fn search_list() -> Self {
        Self
    }
}

#[cfg(not(target_os = "macos"))]
impl PasswordKeychain for SystemKeychain {
    fn read(&self, _account: &str) -> Result<Zeroizing<Vec<u8>>, KeychainFailure> {
        Err(KeychainFailure::Unreadable)
    }
}
