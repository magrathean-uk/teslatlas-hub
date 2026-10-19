// SPDX-License-Identifier: AGPL-3.0-only

#![forbid(unsafe_code)]
// Intentional API surface: multi-arg import/finalize and complex store
// closures are preferred over artificial parameter objects for now.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod api;
pub mod auth;
pub mod collection;
pub mod geo;
pub mod import;
pub mod platform;
pub mod runtime;
pub mod storage;
pub mod sync;

#[path = "../source_identity.rs"]
mod source_identity;

// Stable compatibility exports. New code should prefer the domain paths above.
pub use api::{fleet_api, http_range, owner_api, protocol, server, transport};
pub use auth::{
    credential_recovery, credentials, crypto, fleet_credentials, legacy_auth,
    teslamate_credentials, teslamate_token,
};
#[cfg(test)]
pub use collection::fake_tesla;
pub use collection::{collector, current_state, fleet_telemetry, tesla_stream};
pub use geo::{geocoder, gpx, location, terrain, terrain_cache};
pub use import::teslamate::{
    direct as teslamate_direct, fragments as teslamate_fragments, importer as teslamate_import,
    parity as teslamate_parity, physical_fragments as teslamate_physical_fragments,
    physical_publication as teslamate_physical_publication, progress as teslamate_progress,
    projection as teslamate_projection, projection_state as teslamate_projection_state,
    reader as teslamate_reader, schema as teslamate_schema, source as teslamate,
    stage as teslamate_stage, writeback as teslamate_writeback,
};
#[cfg(unix)]
#[doc(hidden)]
pub use platform::hub_user_process;
#[cfg(target_os = "linux")]
pub use platform::linux_systemd;
#[cfg(target_os = "macos")]
pub use platform::macos_launch_agent;
pub use runtime::{config, diagnostics, lifecycle};
pub use storage::{data_recovery, db};
pub use sync::{hub_pack, updates_delivery, updates_logical};

#[cfg(unix)]
pub(crate) use platform::user_lifetime_lock;
pub(crate) use storage::durability_fault;
pub(crate) use sync::manifest_signing;

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SOURCE_URL: &str = "https://github.com/magrathean-uk/teslatlas-hub";
pub const UNBOUND_SOURCE_ERROR: &str = "this non-distributable developer build has no exact \
source identity; rebuild with TESLATLAS_HUB_SOURCE_COMMIT set to a pushed 40-hex commit";
const SOURCE_COMMIT_RAW: &str = env!("TESLATLAS_HUB_SOURCE_COMMIT");

/// Exact source commit embedded by an explicit build input.
pub fn source_commit() -> Option<&'static str> {
    source_identity::is_exact_commit(SOURCE_COMMIT_RAW).then_some(SOURCE_COMMIT_RAW)
}

fn corresponding_source_url_for(source_commit: &str) -> Option<String> {
    source_identity::is_exact_commit(source_commit)
        .then(|| format!("{SOURCE_URL}/tree/{source_commit}"))
}

/// Immutable Corresponding Source URL, absent for an unbound developer build.
pub fn corresponding_source_url() -> Option<String> {
    corresponding_source_url_for(SOURCE_COMMIT_RAW)
}

fn discovery_source_url_for(source_commit: &str) -> String {
    corresponding_source_url_for(source_commit).unwrap_or_else(|| SOURCE_URL.to_owned())
}

/// Discovery remains profile-compatible while distinguishing an unbound build.
pub fn discovery_source_url() -> String {
    discovery_source_url_for(SOURCE_COMMIT_RAW)
}

#[cfg(test)]
pub(crate) fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;

    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
}

/// One process-wide log capture for tests. Tests that read their own log lines
/// share it, because a process may install only one global `tracing` subscriber:
/// two helpers that each installed their own made whichever ran second fail, so
/// the outcome depended on test order. The subscriber is installed once, at
/// `TRACE`, and writes each event to a buffer of the thread that emitted it, so
/// every callsite stays enabled for every thread (a scoped subscriber would let a
/// callsite first reached by another thread cache as disabled) and a thread that
/// has not armed a buffer captures nothing.
#[cfg(test)]
pub(crate) mod test_log_capture {
    use std::{
        collections::HashMap,
        io::Write,
        sync::{LazyLock, Mutex, Once},
        thread::{self, ThreadId},
    };

    static BUFFERS: LazyLock<Mutex<HashMap<ThreadId, Vec<u8>>>> = LazyLock::new(Default::default);

    #[derive(Clone, Copy)]
    struct Capture;

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(buffer) = BUFFERS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(&thread::current().id())
            {
                buffer.extend_from_slice(bytes);
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            *self
        }
    }

    /// Install the capture (once) and start an empty buffer for this thread.
    pub(crate) fn arm() {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            tracing::subscriber::set_global_default(
                tracing_subscriber::fmt()
                    .with_ansi(false)
                    .with_max_level(tracing::Level::TRACE)
                    .with_writer(Capture)
                    .finish(),
            )
            .expect("no test installs a global subscriber but this one");
        });
        BUFFERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(thread::current().id(), Vec::new());
    }

    /// What this thread has logged since `arm`.
    pub(crate) fn read() -> String {
        let buffers = BUFFERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(
            buffers
                .get(&thread::current().id())
                .map_or(&[][..], Vec::as_slice),
        )
        .into_owned()
    }

    /// Stop capturing for this thread and return what it logged.
    pub(crate) fn disarm() -> String {
        let captured = BUFFERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&thread::current().id())
            .unwrap_or_default();
        String::from_utf8_lossy(&captured).into_owned()
    }
}

/// Interactive legal notice printed by `teslatlas-hub legal`.
pub fn legal_notice() -> String {
    let corresponding_source = corresponding_source_url().unwrap_or_else(|| {
        "UNBOUND DEVELOPMENT BUILD; rebuild with TESLATLAS_HUB_SOURCE_COMMIT before distribution"
            .to_owned()
    });
    format!(
        "Teslatlas Hub {BUILD_VERSION}\n\
         Copyright © 2026 MAGRATHEAN UK LTD\n\
         License: AGPL-3.0-only\n\
         Teslatlas Hub — originally authored by György Bolyki and published by MAGRATHEAN UK LTD. Source: {SOURCE_URL}\n\
         Corresponding Source: {corresponding_source}\n\
         Unofficial; not affiliated with Tesla or TeslaMate; no warranty."
    )
}

#[cfg(test)]
mod legal_notice_tests {
    use super::{
        BUILD_VERSION, SOURCE_COMMIT_RAW, SOURCE_URL, UNBOUND_SOURCE_ERROR,
        corresponding_source_url, corresponding_source_url_for, discovery_source_url_for,
        legal_notice, source_commit, source_identity,
    };

    #[test]
    fn source_identity_is_an_exact_commit_or_explicitly_unbound() {
        assert_eq!(BUILD_VERSION, "2026.36.2");
        if let Some(commit) = source_commit() {
            assert!(source_identity::is_exact_commit(commit));
            assert_eq!(
                corresponding_source_url(),
                Some(format!("{SOURCE_URL}/tree/{commit}"))
            );
        } else {
            assert_eq!(SOURCE_COMMIT_RAW, source_identity::UNBOUND_SOURCE_COMMIT);
            assert_eq!(corresponding_source_url(), None);
        }
    }

    #[test]
    fn commit_validation_rejects_ambiguous_or_noncanonical_inputs() {
        assert!(source_identity::is_exact_commit(&"a".repeat(40)));
        for invalid in [
            "",
            "a",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "gggggggggggggggggggggggggggggggggggggggg",
            "../../aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(!source_identity::is_exact_commit(invalid), "{invalid}");
        }
    }

    #[test]
    fn canonical_discovery_source_covers_bound_and_unbound_builds() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            corresponding_source_url_for(commit),
            Some(format!("{SOURCE_URL}/tree/{commit}"))
        );
        assert_eq!(
            discovery_source_url_for(commit),
            format!("{SOURCE_URL}/tree/{commit}")
        );
        assert_eq!(corresponding_source_url_for("UNBOUND"), None);
        assert_eq!(discovery_source_url_for("UNBOUND"), SOURCE_URL);
        assert!(UNBOUND_SOURCE_ERROR.contains("non-distributable"));
    }

    #[test]
    fn legal_notice_identifies_agpl_only_and_notice_facts() {
        let notice = legal_notice();
        assert!(
            notice.starts_with(&format!("Teslatlas Hub {BUILD_VERSION}\n")),
            "notice must identify the running package version: {notice}"
        );
        assert!(
            notice.contains("License: AGPL-3.0-only"),
            "notice must name AGPL-3.0-only: {notice}"
        );
        assert!(
            !notice.contains("AGPL-3.0-or-later"),
            "notice must not offer or-later: {notice}"
        );
        assert!(
            notice.contains("Copyright © 2026 MAGRATHEAN UK LTD\n"),
            "notice must name the company copyright: {notice}"
        );
        assert!(
            notice.contains("originally authored by György Bolyki")
                && notice.contains("published by MAGRATHEAN UK LTD"),
            "notice must carry the founder/company attribution: {notice}"
        );
        assert!(
            notice.contains(SOURCE_URL),
            "notice must offer the source URL: {notice}"
        );
        match corresponding_source_url() {
            Some(url) => assert!(
                notice.contains(&url),
                "notice must offer exact Corresponding Source: {notice}"
            ),
            None => assert!(
                notice.contains("UNBOUND DEVELOPMENT BUILD"),
                "unbound notice must fail closed: {notice}"
            ),
        }
        assert!(
            notice.contains("no warranty")
                && notice.contains("not affiliated with Tesla or TeslaMate"),
            "notice must state no-warranty and unofficial status: {notice}"
        );
    }
}
