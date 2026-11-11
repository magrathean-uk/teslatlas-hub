// SPDX-License-Identifier: AGPL-3.0-only

//! Read-only credential status from the admitted process's recorded state,
//! the pair's clear schedule and its envelope headers (C10 R68, C5-X8.7).
//! This surface never opens a key, decrypts, compares a generation, or lifts
//! a recorded stop because of an external credential write.

use std::path::Path;

use thiserror::Error;

use super::{
    state::{AuthStatus, FuseSnapshot, StoppedMode, clock_is_valid},
};
use crate::db::{CollectorAuthRow, HubStore, StoreError};

/// Clear schedule and receipt facts only. This is not an integrity witness.
pub(crate) struct OwnerDiagnosticSummary {
    pub unresolved_refresh: bool,
    pub schedule_usable: bool,
}

pub(crate) fn owner_diagnostic_summary(store: &HubStore, now_ms: i64) -> Result<OwnerDiagnosticSummary, StoreError> {
    if store.credential_format_state()?.is_none() {
        return Ok(OwnerDiagnosticSummary { unresolved_refresh: false, schedule_usable: false });
    }
    let presence = store.credential_pair_presence(crate::auth::credential_envelope::CredentialSlot::OwnerApi)?;
    let schedule_usable = presence.as_ref().is_some_and(|row| row.header_check.is_ok()
        && clock_is_valid(now_ms.div_euclid(1_000))
        && (row.expires_at == 0 || now_ms.div_euclid(1_000) < row.expires_at));
    let connection = rusqlite::Connection::open_with_flags(store.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(StoreError::Open)?;
    let unresolved_refresh = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts AS attempt JOIN outbound_request_receipts AS receipt ON receipt.id=attempt.receipt_id WHERE attempt.slot='owner_api' AND receipt.outcome='started')",
        [], |row| row.get(0),
    ).map_err(StoreError::Query)?;
    Ok(OwnerDiagnosticSummary { unresolved_refresh, schedule_usable })
}

/// The status a reader should show, in the row's own shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EffectiveAuth {
    pub row: CollectorAuthRow,
    /// The epoch second at which a still usable `sign_in_required` stops being
    /// usable, when the pair gives a bound. A reader that caches its answer must
    /// not serve it from this second on.
    pub usable_until: Option<i64>,
}

/// Why the effective status could not be derived. Carries no cause text so that
/// no path or secret can travel with it.
#[derive(Debug, Error)]
pub(crate) enum EffectiveAuthError {
    #[error("store fault")]
    Store(#[from] StoreError),
    #[error("key fault")]
    Key(#[from] crate::legacy_owner_credentials::SourceDbCredentialError),
    #[error("cipher fault")]
    Cipher(#[from] crate::source_token_envelope::SourceDbTokenError),
}

/// The stored status of a reader that has no loaded mode (an explicitly
/// context-free store read): only what the row itself says. A `sign_in_required`
/// row that does not collect is one whose token is no longer usable, so it reads
/// as the `signed_out` it has become.
pub(crate) fn stored_auth_view(row: CollectorAuthRow) -> CollectorAuthRow {
    if row.state == "sign_in_required"
        && !row.collecting
        && let Ok(status) = AuthStatus::from_row(&row)
    {
        return status.with_sign_in_expired().to_row();
    }
    row
}

/// C5-X8 section 7: the projection inside an admitted process uses the
/// already-open vault. Non-admitted surfaces keep `effective_auth_row` below.
pub(crate) fn effective_auth_row_admitted(
    vault: &crate::auth::credential_vault::CredentialVault,
    store: &HubStore,
    mode: StoppedMode,
    now_ms: i64,
) -> Result<Option<EffectiveAuth>, EffectiveAuthError> {
    use super::{state::{AuthStop,AuthState},SignOutReason};
    let Some(row)=store.load_collector_auth_row()? else{return Ok(None)};
    let finish=|row,usable_until|Ok(Some(EffectiveAuth {row,usable_until}));
    if row.state=="api_refused" || !matches!(row.state.as_str(),"sign_in_required"|"signed_out"|"retrying"|"persistence_pending") {
        return effective_auth_row(store,Path::new(""),mode,now_ms);
    }
    let status=AuthStatus::from_row(&row)?;
    let plain=vault.load(crate::auth::credential_envelope::CredentialSlot::OwnerApi).map_err(|error|EffectiveAuthError::Store(StoreError::WriteRefused(
        if matches!(error,crate::auth::credential_vault::CredentialStoreError::SourceCredentialDisabled(_)|crate::auth::credential_vault::CredentialStoreError::UnknownOrigin(_)) {"source_credential_disabled"} else {"credential_vault_unavailable"})))?;
    let Some(plain)=plain else{return finish(row,None)};
    if !super::stored_pair_passes_grammar(&plain.access,&plain.refresh,mode) {
        return finish(AuthStatus::stopped(AuthStop::SignedOut(SignOutReason::StoredPairInvalid),plain.generation,mode,false,FuseSnapshot::default(),status.clock_skew).to_row(),None);
    }
    if status.stopped_generation.is_some_and(|generation|generation!=plain.generation) {
        return finish(AuthStatus::ready(FuseSnapshot::default(),status.clock_skew).to_row(),None);
    }
    let now=now_ms.div_euclid(1000);
    if !clock_is_valid(now){return finish(status.with_usability(false).to_row(),None);}
    let bound=super::stored_usable_until(&plain.access,plain.expires_at,plain.next_refresh_at);
    let usable=bound.is_none_or(|until|now<until);
    let status=if status.state==AuthState::SignInRequired {
        if usable{status.with_usability(true)}else{status.with_sign_in_expired()}
    }else{status.with_usability(status.collecting && usable)};
    let bound=if status.state==AuthState::SignInRequired{bound}else{None};
    finish(status.to_row(),bound)
}

/// The effective status for the loaded `mode` at `now_ms`, or `None` before the
/// unit has ever written one. Only the four states that depend on the stored pair
/// read it (`sign_in_required`, `signed_out`, `retrying`, `persistence_pending`),
/// and a refusal compares its mode.
pub(crate) fn effective_auth_row(
    store: &HubStore,
    _data_dir: &Path,
    mode: StoppedMode,
    now_ms: i64,
) -> Result<Option<EffectiveAuth>, EffectiveAuthError> {
    let Some(row) = store.load_collector_auth_row()? else {
        return Ok(None);
    };
    let unchanged = |row: CollectorAuthRow| {
        Ok(Some(EffectiveAuth {
            row,
            usable_until: None,
        }))
    };
    match row.state.as_str() {
        "api_refused" => {
            let status = AuthStatus::from_row(&row)?;
            if status.stopped_mode == Some(mode) {
                unchanged(row)
            } else {
                // A refusal does not outlive a change of mode (R44 c).
                unchanged(AuthStatus::ready(FuseSnapshot::default(), status.clock_skew).to_row())
            }
        }
        "sign_in_required" | "signed_out" | "retrying" | "persistence_pending" => {
            let status = AuthStatus::from_row(&row)?;
            // R68 and C5-X8.7: status sees clear metadata and headers only.
            // Only the admitted manager can compare generations, prove token
            // grammar, or lift a stop after an external credential write.
            let Some(presence) = store.credential_pair_presence(crate::auth::credential_envelope::CredentialSlot::OwnerApi)? else {
                return unchanged(row);
            };
            presence.header_check.map_err(|error| EffectiveAuthError::Store(StoreError::CredentialEnvelope(error)))?;
            let usable_until = (presence.expires_at > 0).then_some(presence.expires_at);
            let now = now_ms.div_euclid(1_000);
            if !clock_is_valid(now) {
                // R38: with a wrong clock usability cannot be judged, so nothing
                // converts to a sign-out; but nothing collects either, as the
                // unit's own restore decides (usable only with a valid clock).
                return unchanged(status.with_usability(false).to_row());
            }
            let usable = usable_until.map_or(status.collecting, |until| now < until);
            let status = if row.state == "sign_in_required" {
                // The stored `collecting` was judged when the row was written,
                // possibly with an invalid clock; the pair decides now.
                if usable {
                    status.with_usability(true)
                } else {
                    status.with_sign_in_expired()
                }
            } else {
                // A wait that collected while its token was usable stops
                // collecting when the bound passes; a wait that was not
                // collecting for another reason (a rejected token) is never
                // made to collect by an unknown bound.
                let collecting = status.collecting && usable;
                status.with_usability(collecting)
            };
            let still_sign_in = status.state == super::state::AuthState::SignInRequired;
            Ok(Some(EffectiveAuth {
                row: status.to_row(),
                usable_until: if still_sign_in { usable_until } else { None },
            }))
        }
        _ => unchanged(row),
    }
}

#[cfg(test)]
mod n1_src_auth_legacy_auth_effective_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for EffectiveAuthError {
        fn error_example() -> Self {
            EffectiveAuthError::Store(example())
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_effective_auth_error(error: EffectiveAuthError) {
            match &error {
                EffectiveAuthError::Store(..) => (),
                EffectiveAuthError::Key(..) => (),
                EffectiveAuthError::Cipher(..) => (),
            }
            assert_name_free(&error);
        }
        check_effective_auth_error(EffectiveAuthError::Store(example()));
        check_effective_auth_error(EffectiveAuthError::Key(example()));
        check_effective_auth_error(EffectiveAuthError::Cipher(example()));
    }
}
