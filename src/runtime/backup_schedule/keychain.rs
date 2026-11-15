// SPDX-License-Identifier: AGPL-3.0-only

//! Keeps the unattended backup password in the Mac Keychain (M06-T12, M06-T17).
//!
//! The item is a generic password in the owner's login keychain: service
//! [`KEYCHAIN_SERVICE`], account the `backup.password_file` path the
//! configuration names (so each Hub configuration has its own item; no file is
//! written there), or [`DEFAULT_ACCOUNT`] when `backup.password_source =
//! "keychain"` names no path (M06-T13).
//!
//! The service binary creates and owns the item (DEC-123, option A): the app
//! hands it the password on standard input (`backup-password store`) and never
//! stores it itself. Created by this binary, the item trusts this binary only,
//! and the Keychain's partition is this binary's signing team, so the
//! LaunchAgent (the same binary) reads it without a prompt, a Hub update signed
//! with the same identity keeps reading it, and the app and other programs
//! are refused. A replacement changes the item's data in place, so the old
//! password stays until the new one commits, and is read back before it counts.
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

/// A password offered to `backup-password store` is not one the recovery
/// commands can take, or the account is unusable.
pub const BACKUP_PASSWORD_INVALID: &str = "backup_password_invalid";

/// The fewest characters a stored password has (the archive's own minimum).
pub const MINIMUM_PASSWORD_CHARS: usize = 12;

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
    /// Sets the item's data in place, or adds the item when there is none:
    /// one Keychain write, so the old password stays until the new one
    /// commits. A store that cannot write says so.
    fn write(&self, _account: &str, _password: &[u8]) -> Result<(), KeychainFailure> {
        Err(KeychainFailure::Unreadable)
    }
}

/// Whether `password` is one the recovery commands can be given back and the
/// archive accepts (REV-CX-M06-T08 finding 1, REV-CX-M06-T17 CR-7): UTF-8, at
/// most 1 KiB and at least [`MINIMUM_PASSWORD_CHARS`] characters. CR, LF and
/// NUL are kept exactly: the scheduled file reader always took them, and
/// `verify-backup` and `restore-data` take them as a password file after a
/// `#pw-file` line, so a password already in use moves into the Keychain and
/// keeps working unchanged. A newly typed password is one line in the app.
pub fn password_acceptable(password: &[u8]) -> bool {
    password.len() <= PASSWORD_FILE_MAX_BYTES
        && std::str::from_utf8(password)
            .is_ok_and(|text| text.chars().count() >= MINIMUM_PASSWORD_CHARS)
}

/// Reads the password item for `account` (the configured path). A value the
/// recovery commands could not take (see [`password_acceptable`]), such as a
/// short or altered item, is refused as unavailable, never used.
pub fn read_keychain_password(
    keychain: &dyn PasswordKeychain,
    account: &str,
) -> Result<Zeroizing<String>, OperatorRefusal> {
    let bytes = keychain.read(account).map_err(|failure| unavailable(failure, account))?;
    if !password_acceptable(&bytes) {
        return Err(unavailable(KeychainFailure::Unreadable, account));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| unavailable(KeychainFailure::Unreadable, account))?;
    Ok(Zeroizing::new(text.to_owned()))
}

/// What the item holds compared with a password the app offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    /// There is no item for this account.
    Missing,
    /// The item holds exactly these bytes.
    Equal,
    /// The item holds another password.
    Different,
}

fn invalid(message: &'static str) -> OperatorRefusal {
    OperatorRefusal::new(BACKUP_PASSWORD_INVALID, message)
}

fn check_account(account: &str) -> Result<(), OperatorRefusal> {
    if account.is_empty() || account.len() > 4096 || account.contains('\0') {
        return Err(invalid("the Keychain account must be the configured password path or unattended-backup"));
    }
    Ok(())
}

/// Equality that takes the same time wherever the bytes differ.
fn same_bytes(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).fold(0_u8, |difference, (a, b)| difference | (a ^ b)) == 0
}

/// One password from standard input: at most 1 KiB and one final line end
/// (LF or CR LF), which is dropped, the rule of the scheduled password file.
/// Every other byte is the password's own, so a password that ends in CR is
/// sent without a final LF and one that ends in LF is sent with one more (the
/// app's framing). Nothing else is read and nothing is echoed.
pub fn password_from_input(mut reader: impl std::io::Read) -> Result<Zeroizing<Vec<u8>>, OperatorRefusal> {
    use std::io::Read as _;
    let mut bytes = Zeroizing::new(Vec::with_capacity(PASSWORD_FILE_MAX_BYTES + 3));
    (&mut reader)
        .take(u64::try_from(PASSWORD_FILE_MAX_BYTES + 3).expect("cap fits"))
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("standard input cannot be read"))?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.is_empty() || bytes.len() > PASSWORD_FILE_MAX_BYTES {
        return Err(invalid("send one password of at most 1 KiB on standard input"));
    }
    Ok(bytes)
}

/// Saves `password` as the item for `account` and reads it back through this
/// binary's own access before it counts (`backup-password store`). The
/// password must be one the recovery commands can take. The item's data
/// changes in place: a failure before the write commits leaves the old
/// password; a read-back that differs is reported, never hidden.
pub fn store_password(
    keychain: &dyn PasswordKeychain,
    account: &str,
    password: &[u8],
) -> Result<(), OperatorRefusal> {
    check_account(account)?;
    if !password_acceptable(password) {
        return Err(invalid(
            "the recovery password needs at least 12 characters of UTF-8 text and at most 1 KiB",
        ));
    }
    keychain.write(account, password).map_err(|failure| match failure {
        KeychainFailure::Refused => OperatorRefusal::new(
            BACKUP_PASSWORD_UNAVAILABLE,
            "the Keychain refused to save the backup password (the keychain is locked, or another program owns the item); unlock the login keychain and save again",
        ),
        _ => OperatorRefusal::new(BACKUP_PASSWORD_UNAVAILABLE, "the backup password could not be saved in the Keychain"),
    })?;
    let saved = keychain.read(account).map_err(|failure| unavailable(failure, account))?;
    if !same_bytes(&saved, password) {
        return Err(OperatorRefusal::new(
            BACKUP_PASSWORD_UNAVAILABLE,
            "the Keychain gave back a different backup password after saving; save the recovery password again",
        ));
    }
    Ok(())
}

/// Compares `password` with the item for `account` through this binary's own
/// access (`backup-password compare`): the app uses it to finish or refuse
/// moving an old password file. A refused or unreadable item is a refusal,
/// and so is an item that holds the same bytes but is not a password the
/// service can use (REV-CX-M06-T17 CR-6): the app then keeps the file.
pub fn compare_password(
    keychain: &dyn PasswordKeychain,
    account: &str,
    password: &[u8],
) -> Result<Comparison, OperatorRefusal> {
    check_account(account)?;
    match keychain.read(account) {
        Ok(saved) if same_bytes(&saved, password) && password_acceptable(&saved) => Ok(Comparison::Equal),
        Ok(saved) if same_bytes(&saved, password) => Err(unavailable(KeychainFailure::Unreadable, account)),
        Ok(_) => Ok(Comparison::Different),
        Err(KeychainFailure::Missing) => Ok(Comparison::Missing),
        Err(failure) => Err(unavailable(failure, account)),
    }
}

/// Whether the item for `account` holds a password the service can use
/// (`backup-password status`): the app's schedule keeps counting a saved
/// password through ordinary edits. Missing is `Ok(false)`.
pub fn password_status(keychain: &dyn PasswordKeychain, account: &str) -> Result<bool, OperatorRefusal> {
    check_account(account)?;
    match read_keychain_password(keychain, account) {
        Ok(_) => Ok(true),
        Err(_) if keychain.read(account).err() == Some(KeychainFailure::Missing) => Ok(false),
        Err(refusal) => Err(refusal),
    }
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
            "the service may not read the backup password in the Keychain (the keychain is locked, or this Hub build is signed differently from the one that saved it); save the recovery password again in Teslatlas Hub (Service details, Backups)"
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

        /// Writes into the first keychain of the list, or the owner's default
        /// one. An item this binary may not open is never replaced or deleted
        /// (that would need a prompt): the write is refused.
        fn write(&self, account: &str, password: &[u8]) -> Result<(), KeychainFailure> {
            let _turn = NO_PROMPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let _no_prompt =
                SecKeychain::disable_user_interaction().map_err(|_| KeychainFailure::Unreadable)?;
            let keychain = match &self.keychains {
                Some(list) => list.first().cloned().ok_or(KeychainFailure::Unreadable)?,
                None => SecKeychain::default().map_err(|error| classify(error.code()))?,
            };
            match keychain.find_generic_password(KEYCHAIN_SERVICE, account) {
                Ok((_, mut item)) => item.set_password(password),
                Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => {
                    keychain.add_generic_password(KEYCHAIN_SERVICE, account, password)
                }
                Err(error) => return Err(classify(error.code())),
            }
            .map_err(|error| classify(error.code()))
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
