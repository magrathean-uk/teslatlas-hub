// SPDX-License-Identifier: AGPL-3.0-only
#![cfg(target_os = "macos")]

use super::super::{Cli, Command, macos_migration_start_request};
use clap::{Parser, error::ErrorKind};

#[test]
fn macos_has_no_install_or_service_verb() {
    for arguments in [
        vec!["install"],
        vec!["service", "status"],
        vec!["service", "start"],
    ] {
        let error =
            Cli::try_parse_from(std::iter::once("teslatlas-hub").chain(arguments)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }
}

#[test]
fn macos_keeps_preflight_for_the_app_start_check() {
    let cli = Cli::try_parse_from([
        "teslatlas-hub",
        "--config",
        "/private/tmp/c.toml",
        "preflight",
    ])
    .unwrap();
    assert_eq!(
        cli.config.unwrap(),
        std::path::PathBuf::from("/private/tmp/c.toml")
    );
    assert!(matches!(
        cli.command,
        Command::Preflight {
            history_only: false
        }
    ));
}

#[test]
fn macos_migration_start_points_to_the_app() {
    assert_eq!(
        macos_migration_start_request(),
        serde_json::json!({
            "serviceStartRequested": true,
            "next": "open Teslatlas Hub.app and choose Start Hub",
        })
    );
}
