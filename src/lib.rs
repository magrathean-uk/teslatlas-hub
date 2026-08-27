#![forbid(unsafe_code)]
// Intentional API surface: multi-arg import/finalize and complex store
// closures are preferred over artificial parameter objects for now.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod collector;
pub mod config;
pub mod credential_recovery;
pub mod credentials;
pub mod crypto;
pub mod current_state;
pub mod data_recovery;
pub mod db;
pub mod diagnostics;
mod durability_fault;
#[cfg(test)]
pub mod fake_tesla;
pub mod fleet_api;
pub mod fleet_credentials;
pub mod fleet_telemetry;
pub mod geocoder;
pub mod gpx;
pub mod http_range;
pub mod hub_pack;
#[cfg(unix)]
#[doc(hidden)]
pub mod hub_user_process;
pub mod legacy_auth;
pub mod lifecycle;
#[cfg(target_os = "linux")]
pub mod linux_systemd;
pub mod location;
#[cfg(target_os = "macos")]
pub mod macos_launch_agent;
mod manifest_signing;
pub mod owner_api;
pub mod protocol;
pub mod server;
pub mod terrain;
pub mod terrain_cache;
pub mod tesla_stream;
pub mod teslamate;
pub mod teslamate_credentials;
pub mod teslamate_direct;
pub mod teslamate_fragments;
pub mod teslamate_import;
pub mod teslamate_parity;
pub mod teslamate_projection;
pub mod teslamate_projection_state;
pub mod teslamate_reader;
pub mod teslamate_schema;
pub mod teslamate_stage;
pub mod teslamate_token;
pub mod teslamate_writeback;
pub mod transport;
pub mod updates_delivery;
pub mod updates_logical;
#[cfg(unix)]
pub(crate) mod user_lifetime_lock;

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SOURCE_URL: &str = "https://github.com/magrathean-uk/teslatlas-hub";

#[cfg(test)]
pub(crate) fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;

    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
}

/// Interactive legal notice printed by `teslatlas-hub legal`.
pub fn legal_notice() -> String {
    format!(
        "Teslatlas Hub {BUILD_VERSION}\n\
         Copyright © 2026 Magrathean UK Ltd\n\
         License: AGPL-3.0-only\n\
         Teslatlas Hub — originally authored by Gyorgy Bolyki and published by Magrathean UK Ltd. Source: {SOURCE_URL}\n\
         Unofficial; not affiliated with Tesla or TeslaMate; no warranty."
    )
}

#[cfg(test)]
mod legal_notice_tests {
    use super::{BUILD_VERSION, SOURCE_URL, legal_notice};

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
            notice.contains("Copyright © 2026 Magrathean UK Ltd"),
            "notice must name the company copyright: {notice}"
        );
        assert!(
            notice.contains("originally authored by Gyorgy Bolyki")
                && notice.contains("published by Magrathean UK Ltd"),
            "notice must carry the founder/company attribution: {notice}"
        );
        assert!(
            notice.contains(SOURCE_URL),
            "notice must offer the source URL: {notice}"
        );
        assert!(
            notice.contains("no warranty")
                && notice.contains("not affiliated with Tesla or TeslaMate"),
            "notice must state no-warranty and unofficial status: {notice}"
        );
    }
}
