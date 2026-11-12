// SPDX-License-Identifier: AGPL-3.0-only

use super::super::credential_vault::{CredentialStoreError, SignInError};

pub const REPLACE_IMPORT_WARNING: &str = "Replacing the stored sign-in means that a rollback to the system the token came from will need a fresh sign-in there; after a rollback, sign in there again.";

/// The one refusal text for every source-credential path (M02-T02).
pub const SOURCE_CREDENTIAL_DISABLED_TEXT: &str = "Source credentials are disabled: the Hub never imports, stores, uses or refreshes another application's sign-in, its tokens or its key, and no option or setting changes that. Sign in with the Hub's own login (in the app, or with setup --sign-in).";

/// Fixed owner copy. Inputs that are not recognised codes are never echoed.
pub fn credential_owner_message(
    code: &str,
    blocked_reason: Option<&str>,
    cleanup_needed_bytes: Option<u64>,
    issuer_code: Option<&str>,
) -> Option<String> {
    let text = match code {
        "keyring_missing" => "The Hub's credential key is missing. Sign in again to resume collection.",
        "keyring_unreadable" => "The Hub cannot read its credential key file.",
        "keyring_unsafe" => "The Hub's credential key file is not private to this user. Fix it or sign in again.",
        "keyring_invalid" => "The Hub's credential key file is damaged. Sign in again.",
        "keyring_newer" => "The credential key file was written by a newer Hub. Update the Hub.",
        "exposure_suspected" => "The credential key file was readable by others and has been made private. Rotate the credential key.",
        "unknown_key" => "The stored sign-in was saved under a different key (another Hub or an old backup). Sign in again.",
        "corrupt" => "The stored sign-in is damaged. Sign in again.",
        "binding_mismatch" => "The stored sign-in does not belong where it is stored. Sign in again.",
        "unknown_format" => "The stored sign-in was written by a newer Hub. Update the Hub.",
        "over_cap" => "The stored sign-in is larger than the Hub accepts. Sign in again.",
        "migration_pending" => "Updating the stored sign-in format.",
        "migration_blocked" => match blocked_reason {
            Some("old_key_unavailable") => "A sign-in stored by an earlier version cannot be read. Sign in again.",
            Some("old_pair_invalid") => "A sign-in stored by an earlier version is damaged. Sign in again.",
            _ => return None,
        },
        "secrets_backup_exclusion_failed" => "The Hub could not keep its key folder out of Time Machine, so it will not save new sign-ins until it can.",
        "durability_unavailable" => "The disk holding the Hub's data cannot guarantee that sign-in changes survive a power cut. Move the Hub's data folder to a local disk.",
        "credential_purge_pending" => {
            let bytes = cleanup_needed_bytes.filter(|bytes| *bytes > 0)?;
            return Some(format!("Finishing the sign-in format update needs {} GB more free space on the data disk. Backups wait until then.", bytes.div_ceil(1_000_000_000)));
        }
        "source_credential_disabled" => SOURCE_CREDENTIAL_DISABLED_TEXT,
        "unknown_origin" => "The stored sign-in was not made by the Hub's own login, so the Hub does not use it. Sign in with the Hub's own login (in the app, or with setup --sign-in).",
        "token_entry_requires_sign_in" => "Some sign-ins this Hub used before its last update cannot be recognised in every form, so it now stores new sign-ins only from its own login. Tokens entered by hand, provider refresh tokens and token imports are refused; sign in with the Hub's login instead.",
        "fresh_sign_in_required" => "This Hub cannot confirm that this token was not used before its last update, so it stores new sign-ins only from its own login. Sign in with the Hub's login (in the app, or with setup --sign-in; setup-fleet --sign-in for the Fleet connection). With a provider, enter only the provider's access token.",
        "empty_access_token" => "The access token was empty, so nothing was stored.",
        "empty_refresh_token" => "The refresh token was empty, so nothing was stored.",
        "token_too_large" => "A token is larger than the Hub accepts, so nothing was stored.",
        "refresh_token_spent" => "This refresh token has already been used, so nothing was stored; sign in again.",
        "slot_occupied" => "The Hub already holds a sign-in, so the imported token was not stored; import again and choose to replace the stored sign-in to use it instead.",
        "invalid_origin" => "The token input named an origin, which the Hub sets itself, so nothing was stored; remove the origin and try again.",
        "sign_in_state_mismatch" => "The sign-in answer did not belong to this sign-in. Start the sign-in again.",
        "sign_in_refused" => {
            let issuer = issuer_code.filter(|code| matches!(*code, "invalid_request" | "unauthorized_client" | "access_denied" | "unsupported_response_type" | "invalid_scope" | "server_error" | "temporarily_unavailable")).unwrap_or("other");
            return Some(format!("Tesla did not complete the sign-in. ({issuer})"));
        }
        "sign_in_expired" => "The sign-in took longer than 10 minutes. Start it again.",
        "sign_in_answer_invalid" => "The sign-in answer could not be used. Start it again.",
        "sign_in_unavailable" => "The Hub could not reach Tesla's sign-in service. Check the connection and start again.",
        "sign_in_binding_invalid" => "The sign-in could not be completed. Start it again.",
        "sign_in_settings_invalid" => "The Fleet sign-in settings were missing, incomplete or too large. Nothing was changed. Check them and start again.",
        "sign_in_cancelled" => "The sign-in was cancelled before Tesla was asked for tokens. Start it again when you are ready.",
        "sign_in_service_pause_failed" => "The Hub could not pause collection for the sign-in, so the sign-in did not start. If the Hub shows as stopped, choose Start Hub, then try again.",
        "sign_in_service_restart_failed" => "The sign-in has ended, but the Hub could not confirm that collection restarted. If the Hub shows as stopped, choose Start Hub.",
        "backup_waits_for_credential_cleanup" => {
            let mut text = "The backup will start once the Hub has finished updating its stored sign-ins.".to_owned();
            if matches!(blocked_reason, Some("blocked" | "keyring_missing")) {
                text.push_str(" Sign in again.");
            } else if let Some(bytes) = cleanup_needed_bytes.filter(|bytes| *bytes > 0) {
                text.push_str(&format!(" {} GB more free space is needed on the data disk.", bytes.div_ceil(1_000_000_000)));
            }
            return Some(text);
        }
        _ => return None,
    };
    Some(text.to_owned())
}

#[derive(Debug)]
pub struct CredentialCommandError {
    code: &'static str,
    message: Option<String>,
    pub(crate) pair_stored: bool,
    pub(crate) operation: Option<&'static str>,
}

impl CredentialCommandError {
    pub fn code(&self) -> &'static str { self.code }
    pub fn pair_stored(&self) -> bool { self.pair_stored }
    pub fn operation(&self) -> Option<&'static str> { self.operation }
    pub(crate) fn new(code: &'static str) -> Self {
        Self { code, message: credential_owner_message(code, None, None, None), pair_stored: false, operation: None }
    }
}

impl std::fmt::Display for CredentialCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.message {
            Some(message) => write!(formatter, "{}: {message}", self.code),
            None => formatter.write_str(self.code),
        }
    }
}

impl std::error::Error for CredentialCommandError {}

impl From<CredentialStoreError> for CredentialCommandError {
    fn from(error: CredentialStoreError) -> Self {
        let mut command = Self::new(error.code());
        if let CredentialStoreError::SignIn(SignInError::Refused(issuer)) = error {
            command.message = credential_owner_message(command.code, None, None, Some(issuer.code()));
        }
        command
    }
}

pub fn credential_entry_error(error: crate::CredentialVaultEntryError) -> CredentialCommandError {
    CredentialCommandError::new(error.code())
}

/// The stable refusal of every source-credential input, flag and path.
pub fn source_credential_disabled() -> CredentialCommandError { CredentialCommandError::new("source_credential_disabled") }
pub fn sign_in_unavailable() -> CredentialCommandError { CredentialStoreError::SignIn(SignInError::Unavailable).into() }
pub fn sign_in_service_pause_failed() -> CredentialCommandError { CredentialStoreError::SignIn(SignInError::ServicePauseFailed).into() }
pub fn sign_in_service_restart_failed() -> CredentialCommandError { CredentialCommandError::new("sign_in_service_restart_failed") }
pub fn sign_in_input_error(error: super::SignInInputError, settings: bool) -> CredentialCommandError {
    use super::SignInInputError;
    let error = match error {
        SignInInputError::Cancelled => SignInError::Cancelled,
        _ if settings => SignInError::SettingsInvalid,
        SignInInputError::Closed => SignInError::Cancelled,
        SignInInputError::TimedOut => SignInError::Expired,
        SignInInputError::TooLong | SignInInputError::Read => SignInError::AnswerInvalid,
    };
    CredentialStoreError::SignIn(error).into()
}

#[derive(Debug)]
pub struct SignInPreconditionError { pub class: &'static str, pub key: &'static str }
impl std::fmt::Display for SignInPreconditionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: This connection uses a provider. Enter only the provider's access token for this mode.", self.key)
    }
}
impl std::error::Error for SignInPreconditionError {}

pub fn owner_sign_in_precondition(mode: crate::config::CollectorMode) -> Result<(), SignInPreconditionError> {
    if mode == crate::config::CollectorMode::Provider {
        Err(SignInPreconditionError { class: "configuration_precondition", key: "provider_mode_sign_in_unavailable" })
    } else { Ok(()) }
}

pub fn sign_in_status_precondition(status: &super::CredentialStatus) -> Result<(), CredentialCommandError> {
    let code = if status.credential_store.durability == Some("unavailable") { Some("durability_unavailable") }
        else if status.credential_store.secrets_backup_excluded == Some(false) { Some("secrets_backup_exclusion_failed") }
        else if !matches!(status.credential_store.keyring, "ok" | "keyring_missing") { Some(status.credential_store.keyring) }
        else { None };
    match code { Some(code) => Err(CredentialCommandError::new(code)), None => Ok(()) }
}

pub fn diagnostic_admission_error(status: &super::CredentialStatus, fleet: bool) -> CredentialCommandError {
    let slot = if fleet { &status.fleet_credentials } else { &status.legacy_credentials };
    let code = if status.credential_store.keyring != "ok" { status.credential_store.keyring }
        else if status.credential_store.durability == Some("unavailable") { "durability_unavailable" }
        else if status.credential_store.secrets_backup_excluded == Some(false) { "secrets_backup_exclusion_failed" }
        else if slot.store_state != "ok" { slot.store_state }
        else { "credential_store_unavailable" };
    let mut error = CredentialCommandError::new(code);
    if code == "migration_blocked" {
        let reason = if fleet { status.credential_store.blocked_reason.fleet_api } else { status.credential_store.blocked_reason.owner_api };
        error.message = credential_owner_message(code, reason, status.credential_store.cleanup_needed_bytes, None);
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential_vault::SignInRefusalCode;

    #[test]
    fn issuer_refusal_preserves_only_the_fixed_issuer_code() {
        for issuer in ["invalid_request", "unauthorized_client", "access_denied", "unsupported_response_type", "invalid_scope", "server_error", "temporarily_unavailable", "unrecognised-input"] {
            let refusal = SignInRefusalCode::from_issuer_code(issuer);
            let expected = refusal.code();
            let command = CredentialCommandError::from(CredentialStoreError::SignIn(SignInError::Refused(refusal)));
            assert_eq!(command.code(), "sign_in_refused");
            assert_eq!(command.message, credential_owner_message("sign_in_refused", None, None, Some(expected)));
            if expected == "other" { assert!(!command.to_string().contains(issuer)); }
        }
    }

    #[test]
    fn cleanup_messages_use_only_a_positive_decimal_gigabyte_shortfall() {
        assert!(credential_owner_message("credential_purge_pending", None, None, None).is_none());
        assert!(credential_owner_message("credential_purge_pending", None, Some(0), None).is_none());
        let message = credential_owner_message("credential_purge_pending", None, Some(1_000_000_001), None).expect("shortfall");
        assert!(message.contains("2 GB"));
        let zero = credential_owner_message("backup_waits_for_credential_cleanup", None, Some(0), None).expect("backup wait");
        assert!(!zero.contains("GB"));
        let blocked = credential_owner_message("backup_waits_for_credential_cleanup", Some("blocked"), Some(9_000_000_000), None).expect("blocked wait");
        assert!(blocked.ends_with("Sign in again."));
        assert!(!blocked.contains("GB"));
    }
}
