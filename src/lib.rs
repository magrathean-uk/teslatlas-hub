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
    parity as teslamate_parity, projection as teslamate_projection,
    projection_state as teslamate_projection_state, reader as teslamate_reader,
    schema as teslamate_schema, source as teslamate, stage as teslamate_stage,
    writeback as teslamate_writeback,
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

/// Signed release tag containing the exact source for this binary version.
pub fn source_release_tag() -> String {
    format!("v{BUILD_VERSION}")
}

/// Version-bound release page listing every Corresponding Source component.
pub fn corresponding_source_url() -> String {
    let tag = source_release_tag();
    format!("{SOURCE_URL}/releases/tag/{tag}")
}

#[cfg(test)]
pub(crate) fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;

    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
}

/// Interactive legal notice printed by `teslatlas-hub legal`.
pub fn legal_notice() -> String {
    let corresponding_source = corresponding_source_url();
    format!(
        "Teslatlas Hub {BUILD_VERSION}\n\
         Copyright © 2026 György Bolyki, MAGRATHEAN UK LTD, and identified contributors, each for material they own\n\
         License: AGPL-3.0-only\n\
         Teslatlas Hub — originally authored by György Bolyki and published by MAGRATHEAN UK LTD. Source: {SOURCE_URL}\n\
         Corresponding Source for this version: {corresponding_source}\n\
         Unofficial; not affiliated with Tesla or TeslaMate; no warranty."
    )
}

#[cfg(test)]
mod legal_notice_tests {
    use super::{
        BUILD_VERSION, SOURCE_URL, corresponding_source_url, legal_notice, source_release_tag,
    };

    #[test]
    fn corresponding_source_is_bound_to_the_exact_package_version() {
        let tag = format!("v{BUILD_VERSION}");
        assert_eq!(source_release_tag(), tag);
        assert_eq!(
            corresponding_source_url(),
            format!("{SOURCE_URL}/releases/tag/{tag}")
        );
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
            notice.contains("Copyright © 2026 György Bolyki, MAGRATHEAN UK LTD"),
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
        assert!(
            notice.contains(&corresponding_source_url()),
            "notice must offer exact Corresponding Source: {notice}"
        );
        assert!(
            notice.contains("no warranty")
                && notice.contains("not affiliated with Tesla or TeslaMate"),
            "notice must state no-warranty and unofficial status: {notice}"
        );
    }
}
