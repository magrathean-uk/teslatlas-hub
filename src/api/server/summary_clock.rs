// SPDX-License-Identifier: AGPL-3.0-only

//! The clock the current summary reads as `now_ms` (C6 R28 (c)). It is the
//! system clock unless a fixture or test pins it. A pin is a development and
//! test seam: only [`paired_router_at`] and the Fixture-mode Serve path set one.
//! The module also holds the Serve entries that keep the system clock
//! ([`serve_for_admitted_user`] and the test wrapper), over the pinned
//! `serve_for_admitted_user_at` and `serve_with_cursor_key_at` in the parent.

use super::*;

/// File name of the fixture's summary-clock marker inside the Hub data
/// directory: one decimal epoch-millisecond time, written by the fixture seed
/// and read only by a Fixture-mode source run (`fixture_summary_now_ms`, macOS).
/// Defined here, in an always-compiled module, so the seed builds on every
/// target; the reader is macOS only.
pub const FIXTURE_SUMMARY_CLOCK_FILE: &str = "fixture-summary-clock-ms";

impl AppState {
    /// Pin the clock the current summary reads. `None`, zero and a negative
    /// time keep the system clock; a positive time is epoch milliseconds.
    pub(super) fn with_summary_now_ms(self, summary_now_ms: Option<i64>) -> Self {
        self.summary_test_now_ms.store(
            summary_now_ms.filter(|ms| *ms > 0).unwrap_or(0),
            std::sync::atomic::Ordering::SeqCst,
        );
        self
    }

    /// The evaluation time of a current summary, epoch milliseconds: the
    /// pinned clock when set, otherwise the system clock.
    pub(super) fn summary_now_ms(&self) -> Result<i64, std::time::SystemTimeError> {
        let pinned = self
            .summary_test_now_ms
            .load(std::sync::atomic::Ordering::SeqCst);
        if pinned != 0 {
            return Ok(pinned);
        }
        current_epoch_ms()
    }
}

/// [`paired_router`] with the clock of the current summary pinned to
/// `summary_now_ms` (epoch milliseconds); `None`, zero or a negative time keeps
/// the system clock.
///
/// A fixture and test pin: the interop fixture seeds observations at fixed
/// past times, and C6 R28 (c) measures class T freshness from
/// `max(observed_at_ms, now_ms)`, so a fixed clock keeps the pinned values
/// served. Only `current_vehicle` reads it. Production never calls this with
/// `Some`; the configured Hub builds its router from the system clock unless a
/// Fixture-mode source run supplies the fixture's own marker.
pub fn paired_router_at(
    store: HubStore,
    cursor_key: &CursorKey,
    summary_now_ms: Option<i64>,
) -> Router {
    router_for_state(
        AppState::new(
            store,
            false,
            true,
            true,
            Some(ManifestSigning::from_cursor_key(cursor_key)),
            Some(cursor_key.clone()),
            None,
            None,
        )
        .with_summary_now_ms(summary_now_ms),
        CorsPolicy::default(),
    )
}

/// Serve from the one admitted Hub process on the system clock; see
/// [`serve_for_admitted_user_at`] for the pinned fixture form.
#[cfg(unix)]
#[doc(hidden)]
pub async fn serve_for_admitted_user<F>(
    store: HubStore,
    config: &HubConfig,
    native_config_digest: Sha256Digest,
    admission: std::sync::Arc<crate::hub_user_process::AdmittedUserHub>,
    cursor_key: Option<CursorKey>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    serve_for_admitted_user_at(
        store,
        config,
        native_config_digest,
        admission,
        cursor_key,
        None,
        shutdown,
    )
    .await
}

/// The Serve entry the server tests call: the system clock, no admission
/// beyond what the caller passes.
#[cfg(all(test, unix))]
pub(super) async fn serve_with_cursor_key<F>(
    store: HubStore,
    config: &HubConfig,
    native_config_digest: Sha256Digest,
    cursor_key: Option<CursorKey>,
    admission: Option<ServerAdmission>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    serve_with_cursor_key_at(
        store,
        config,
        native_config_digest,
        cursor_key,
        admission,
        None,
        shutdown,
    )
    .await
}
