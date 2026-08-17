#![forbid(unsafe_code)]
// Intentional API surface: multi-arg import/finalize and complex store
// closures are preferred over artificial parameter objects for now.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod collector;
pub mod config;
pub mod credentials;
pub mod crypto;
pub mod data_recovery;
pub mod db;
#[cfg(test)]
pub mod fake_tesla;
pub mod geocoder;
pub mod http_range;
pub mod hub_pack;
#[cfg(unix)]
#[doc(hidden)]
pub mod hub_user_process;
pub mod legacy_auth;
pub mod lifecycle;
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
pub mod transport;
pub mod updates_delivery;
pub mod updates_logical;
#[cfg(unix)]
pub(crate) mod user_lifetime_lock;

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SOURCE_URL: &str = "https://github.com/magrathean-uk/teslatlas-hub";

/// Interactive legal notice printed by `teslatlas-hub legal`.
pub fn legal_notice() -> String {
    format!(
        "Teslatlas Hub {BUILD_VERSION}\n\
         Copyright © 2026 Magrathean UK Ltd\n\
         License: AGPL-3.0-only\n\
         Teslatlas Hub — originally developed and published by Magrathean UK Ltd. Source: {SOURCE_URL}\n\
         Unofficial; not affiliated with Tesla or TeslaMate; no warranty."
    )
}

#[cfg(test)]
mod legal_notice_tests {
    use super::{legal_notice, BUILD_VERSION, SOURCE_URL};

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
            notice.contains("originally developed and published by Magrathean UK Ltd"),
            "notice must carry Company attribution: {notice}"
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
