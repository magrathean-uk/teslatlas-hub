// SPDX-License-Identifier: AGPL-3.0-only

//! The effective credential status a reader without a manager derives (B2
//! clarification 3, C5 R25, R29, R38, R44a, R52a; the "read-only effective
//! auth" section).
//!
//! The stored `collector_auth_state` row is what the credential unit last wrote.
//! It does not freeze usability: a `sign_in_required` row is written while the
//! access token is usable, and the token stops being usable with no write if the
//! unit is idle or gone. Status, doctor and service readiness therefore read the
//! row through the current pair, with the unit's own predicates:
//!
//! - a sign-in or sign-out stop applies only while its stopped generation is the
//!   generation of the stored pair; an external credential write lifts it, but
//!   only in this view (R44a, the stored row is not touched);
//! - a refusal applies only to the mode it was set for (R44 c);
//! - `sign_in_required` is effectively `signed_out`, with the same reason and
//!   generation, once the host clock reaches the pair's `usable_until`
//!   (strictly `now < usable_until` while usable, R29);
//! - `retrying` and `persistence_pending` stop collecting once the token is no
//!   longer usable, and an unknown bound never makes them collect;
//! - a pair that decrypts but fails the token grammar (R46) is `signed_out` with
//!   `stored_pair_invalid`, as the unit that loads it would stop;
//! - an invalid host clock (R38) never converts a stop: the stored word stands,
//!   but a state that collects only while the token is usable does not collect.
//!
//! Nothing here writes, builds a manager, takes the refresh singleton, sends a
//! request or hands a token out. It reads the existing current or previous key
//! generation without settling an interrupted replacement. A real key, cipher or
//! store fault is an error and is never turned into a guess.

use std::path::Path;

use thiserror::Error;

use super::{
    SignOutReason,
    state::{AuthStatus, AuthStop, FuseSnapshot, StoppedMode, clock_is_valid},
    stored_usable_until,
};
use crate::db::{CollectorAuthRow, HubStore, StoreError};

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
    Key(#[from] crate::teslamate_credentials::TeslaMateCredentialError),
    #[error("cipher fault")]
    Cipher(#[from] crate::teslamate_token::TeslaMateTokenError),
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

/// The effective status for the loaded `mode` at `now_ms`, or `None` before the
/// unit has ever written one. Only the four states that depend on the stored pair
/// read it (`sign_in_required`, `signed_out`, `retrying`, `persistence_pending`),
/// and a refusal compares its mode.
pub(crate) fn effective_auth_row(
    store: &HubStore,
    data_dir: &Path,
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
            let Some(tokens) = store.load_teslamate_legacy_tokens()? else {
                // No pair: nothing can match or expire, the stored word stands.
                return unchanged(row);
            };
            let key = crate::teslamate_credentials::select_stored_legacy_key_read_only(
                data_dir, &tokens,
            )?;
            let plaintext = crate::teslamate_token::decrypt_legacy_owner_tokens(
                key.as_bytes(),
                tokens.access(),
                tokens.refresh(),
            )?;
            let generation =
                crate::teslamate_token::legacy_refresh_credential_generation(&plaintext);
            let usable_until = stored_usable_until(
                plaintext.access_token(),
                tokens.expires_at(),
                tokens.next_refresh_at(),
            );
            if !super::stored_pair_passes_grammar(
                plaintext.access_token(),
                plaintext.refresh_token(),
                mode,
            ) {
                // R46: a pair that fails the token grammar cannot be used under
                // any configuration, and the unit that loads it stops signed out
                // (`stored_pair_invalid`) whatever the row said. Decrypting is not
                // enough to be usable.
                return unchanged(
                    AuthStatus::stopped(
                        AuthStop::SignedOut(SignOutReason::StoredPairInvalid),
                        generation,
                        mode,
                        false,
                        FuseSnapshot::default(),
                        status.clock_skew,
                    )
                    .to_row(),
                );
            }
            if matches!(row.state.as_str(), "sign_in_required" | "signed_out")
                && status.stopped_generation != Some(generation)
            {
                // R44a: an external write with another generation lifts a
                // credential stop. Only the view changes.
                return unchanged(
                    AuthStatus::ready(FuseSnapshot::default(), status.clock_skew).to_row(),
                );
            }
            let now = now_ms.div_euclid(1_000);
            if !clock_is_valid(now) {
                // R38: with a wrong clock usability cannot be judged, so nothing
                // converts to a sign-out; but nothing collects either, as the
                // unit's own restore decides (usable only with a valid clock).
                return unchanged(status.with_usability(false).to_row());
            }
            let usable = usable_until.is_none_or(|until| now < until);
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
