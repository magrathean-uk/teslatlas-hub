// SPDX-License-Identifier: AGPL-3.0-only

//! Supplied-token input, refused (M02-T02).
//!
//! Source credentials are disabled. The Hub stores a sign-in only from its own
//! login, so no access or refresh token handed to it (typed, piped, read from a
//! file, decrypted from another application, or a provider's) is parsed or
//! kept. These entry points remain only as refusal shims for the command paths
//! that still name them; each returns `source_credential_disabled` before it
//! looks at the input, and no flag, setting, variable or acknowledgement
//! changes that.

use std::fmt;
use zeroize::Zeroizing;
use super::{CredentialCommandError, OwnerEntryKind, PreparedOwnerEntry, source_credential_disabled};

#[derive(Debug)]
pub enum OwnerInputError { Credential(CredentialCommandError), UnknownKey(String), DuplicateKey(String), InvalidDocument }
impl fmt::Display for OwnerInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Credential(error) => error.fmt(f),
            Self::UnknownKey(key) => write!(f, "unknown input key: {key}"),
            Self::DuplicateKey(key) => write!(f, "duplicate input key: {key}"),
            Self::InvalidDocument => f.write_str("invalid token input document"),
        }
    }
}
impl std::error::Error for OwnerInputError {}

/// Refused: token files are never read into a stored sign-in.
pub fn decode_owner_files(access: Zeroizing<Vec<u8>>, refresh: Zeroizing<Vec<u8>>, kind: OwnerEntryKind) -> Result<PreparedOwnerEntry, OwnerInputError> {
    drop((access, refresh, kind));
    Err(OwnerInputError::Credential(source_credential_disabled()))
}

/// Refused before the document is parsed: no token input is accepted.
pub fn decode_owner_entry(bytes: &[u8], kind: OwnerEntryKind, replace_source: bool) -> Result<PreparedOwnerEntry, OwnerInputError> {
    let _ = (bytes, kind, replace_source);
    Err(OwnerInputError::Credential(source_credential_disabled()))
}

/// A Fleet token set that could be stored. It cannot exist: Fleet token input
/// is refused, so this type has no value.
pub struct PreparedFleetSetup { never: std::convert::Infallible }
impl fmt::Debug for PreparedFleetSetup {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result { match self.never {} }
}
impl PreparedFleetSetup {
    pub fn into_credentials(self) -> crate::fleet_credentials::FleetSetupCredentials { match self.never {} }
}

/// Refused before the document is parsed: Fleet tokens are never supplied.
pub fn decode_fleet_entry(bytes: &[u8]) -> Result<PreparedFleetSetup, Box<dyn std::error::Error>> {
    let _ = bytes;
    Err(source_credential_disabled().into())
}

pub fn persist_fleet_setup(entry: &crate::CredentialVaultEntry, prepared: PreparedFleetSetup, clear_owner: bool) -> Result<crate::fleet_credentials::FleetSetupCredentials, CredentialCommandError> {
    let _ = (entry, clear_owner);
    match prepared.never {}
}
