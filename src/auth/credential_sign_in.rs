// SPDX-License-Identifier: AGPL-3.0-only

//! Command-facing credential input and custody.

mod buffers;
mod custody;
mod entries;
mod messages;
mod status;
mod scope;
mod control;
mod input;
mod protocol;
mod supplied;
mod service;
mod observe;
#[cfg(any(debug_assertions, feature = "interop-fixture"))]
mod manager_fixture;
#[cfg(test)]
pub(crate) mod status_fixtures;

pub use buffers::observe_file_token_drops;
pub(crate) use buffers::{DecodedFileToken, decoded_file_token};
pub use custody::{FileCustodyError, read_credential_file, read_credential_file_with_hooks};
pub use entries::{CanonicalOwnerTokens, OwnerEntryKind, PreparedOwnerEntry, PreparedFleetEntry, persist_owner_entry, prepare_owner_entry, persist_fleet_entry, prepare_fleet_entry};
pub use messages::{CredentialCommandError, REPLACE_IMPORT_WARNING, SOURCE_CREDENTIAL_DISABLED_TEXT, credential_entry_error, credential_owner_message, source_credential_disabled};
pub use messages::{SignInPreconditionError, owner_sign_in_precondition, sign_in_input_error, sign_in_service_pause_failed, sign_in_service_restart_failed, sign_in_status_precondition, sign_in_unavailable};
pub use messages::diagnostic_admission_error;
pub use status::{BlockedReasons, CredentialStatus, CredentialStoreStatus, SlotStatus, credential_status, credential_store_unhealthy};
pub use control::{RotationResult, SignOutResult, rotate_credential_key, sign_out};
pub use input::{SIGN_IN_LINE_CAP, SignInInput, SignInInputError};
pub use scope::SignInScope;
pub use protocol::{FLEET_SIGN_IN_SCOPE, SignInSettings, SignInAttempt, pkce_verifier, pkce_challenge};
pub use supplied::{OwnerInputError, decode_owner_entry, decode_owner_files};
pub use supplied::{PreparedFleetSetup, decode_fleet_entry, persist_fleet_setup};
pub use entries::clear_owner_after_fleet;
pub use entries::prepare_owner_tokens;
pub use entries::persist_owner_entry_with_notice;
pub use observe::{SecretBuffer, SecretDropEvent, SecretText, observe_sign_in_secret_drops};
pub(crate) use observe::{SecretDropObserver, current_secret_observer, note_secret_drop};
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use entries::spend_refresh_for_test;
pub use service::{SignInCompletion, SignInServiceError, SignInWorkError, with_sign_in_scope};
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use service::with_sign_in_scope_for_test;
#[cfg(any(debug_assertions, feature = "interop-fixture"))]
#[doc(hidden)]
pub use manager_fixture::{record_provider_admission_for_test, refresh_owner_manager_for_test, begin_refresh_for_test, pair_matches_for_test};
// Pair seeding writes a pair without an exchange, so it exists only in debug
// and test builds, never in a release build (with or without interop-fixture).
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use manager_fixture::{seed_owner_pair_for_test, seed_fleet_pair_for_test};

#[cfg(test)]
#[path = "credential_sign_in/tests.rs"]
mod g24_m2_v2;
