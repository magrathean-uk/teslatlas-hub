// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{
    config::CollectorProvider,
    credentials::OwnerTokens,
    db::{HubStore, TeslaMateLegacyTokenStore},
    fleet_api::FleetRegion,
    fleet_credentials::{FleetSetupCredentials, persist_fleet_setup_credentials},
    hub_pack::ProjectionCarSettings,
    protocol::CursorKey,
    teslamate_credentials::{load_or_create_cursor_key, replace_key_and_tokens},
    teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history},
    teslamate_projection::{TeslaMateCar, TeslaMateHistory},
    teslamate_token::encrypt_legacy_owner_tokens,
};

fn seed_selected_car(data_dir: &Path) -> HubStore {
    let store = HubStore::initialize(data_dir).expect("store");
    let history = TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: 70,
            vid: Some(71),
            vin: Some("5YJTEST0000000001".to_owned()),
            name: Some("Install fixture".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: ProjectionCarSettings::default(),
        }],
        drives: Vec::new(),
        positions: Vec::new(),
        charging_processes: Vec::new(),
        charges: Vec::new(),
        addresses: Vec::new(),
        geofences: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
    };
    publish_history(
        &store,
        &CursorKey::from_bytes([7; 32]),
        &TeslaMateImportRequest {
            source_key: "install-fixture".to_owned(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_000,
        },
        &history,
    )
    .expect("imported car");
    store
}

fn seed_ready_hub(data_dir: &Path) {
    let store = seed_selected_car(data_dir);
    let tokens =
        OwnerTokens::from_secret_parts("access".to_owned(), "refresh".to_owned()).expect("tokens");
    let key = b"install-fixture-key";
    let (access, refresh) = encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt");
    let stored = TeslaMateLegacyTokenStore::imported(access, refresh).expect("stored tokens");
    replace_key_and_tokens(data_dir, &store, key, &stored).expect("credentials");
}

fn seed_fleet_ready_hub(data_dir: &Path) {
    let store = seed_selected_car(data_dir);
    load_or_create_cursor_key(data_dir).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iXX0.sig".to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        3_600,
    )
    .expect("Fleet credentials");
    persist_fleet_setup_credentials(&store, data_dir, &credentials, SystemTime::now())
        .expect("persist Fleet credentials");
}

#[test]
fn provider_preflight_accepts_only_the_selected_usable_credentials() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    seed_fleet_ready_hub(temporary.path());

    preflight_hub_for_provider(temporary.path(), CollectorProvider::Fleet)
        .expect("Fleet preflight");
    preflight_hub(temporary.path()).expect("generic provider preflight");
    assert!(preflight_hub_for_provider(temporary.path(), CollectorProvider::Legacy).is_err());
}

fn assert_no_install_artifacts(data: &Path, home: &Path) {
    assert!(!data.join("bin").exists(), "preflight wrote a binary");
    assert!(
        !home
            .join("Library")
            .join("LaunchAgents")
            .join(PLIST_NAME)
            .exists(),
        "preflight wrote a plist"
    );
}

#[test]
fn launchctl_query_failure_restores_prepared_replacement_files() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let binary = temporary.path().join("teslatlas-hub");
    let plist = temporary.path().join("com.teslatlas.hub.plist");
    let previous_binary = temporary.path().join(".teslatlas-hub.previous");
    let previous_plist = temporary.path().join(".com.teslatlas.hub.plist.previous");
    fs::write(&binary, b"new binary").expect("new binary");
    fs::write(&plist, b"new plist").expect("new plist");
    fs::write(&previous_binary, b"old binary").expect("old binary");
    fs::write(&previous_plist, b"old plist").expect("old plist");
    let paths = InstallPaths {
        binary: binary.clone(),
        plist: plist.clone(),
        previous_binary: Some(previous_binary.clone()),
        previous_plist: Some(previous_plist.clone()),
    };
    let mut calls = 0;
    let mut runner = |_: &[&std::ffi::OsStr]| {
        calls += 1;
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "launchctl query denied",
        ))
    };

    let error = launch_with_runner(&paths, "gui/501", "gui/501/com.teslatlas.hub", &mut runner)
        .expect_err("query failure must abort install");
    assert!(
        error
            .to_string()
            .contains("prepared install files restored")
    );
    assert_eq!(calls, 1);
    assert_eq!(fs::read(&binary).expect("restored binary"), b"old binary");
    assert_eq!(fs::read(&plist).expect("restored plist"), b"old plist");
    assert!(!previous_binary.exists());
    assert!(!previous_plist.exists());
}

#[test]
fn launchctl_query_failure_removes_new_install_files_without_backups() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let binary = temporary.path().join("teslatlas-hub");
    let plist = temporary.path().join("com.teslatlas.hub.plist");
    fs::write(&binary, b"new binary").expect("new binary");
    fs::write(&plist, b"new plist").expect("new plist");
    let paths = InstallPaths {
        binary: binary.clone(),
        plist: plist.clone(),
        previous_binary: None,
        previous_plist: None,
    };
    let mut runner = |_: &[&std::ffi::OsStr]| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "launchctl query denied",
        ))
    };

    launch_with_runner(&paths, "gui/501", "gui/501/com.teslatlas.hub", &mut runner)
        .expect_err("query failure must abort install");
    assert!(!binary.exists());
    assert!(!plist.exists());
}

#[test]
fn installed_service_start_stop_and_restart_use_bounded_launchctl_sequences() {
    fn runner(
        responses: Vec<bool>,
    ) -> (
        impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
        std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    ) {
        let responses = std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::VecDeque::from(responses),
        ));
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_runner = std::sync::Arc::clone(&calls);
        let responses_for_runner = std::sync::Arc::clone(&responses);
        let run = move |arguments: &[&std::ffi::OsStr]| {
            calls_for_runner.lock().expect("calls").push(
                arguments
                    .iter()
                    .map(|argument| argument.to_string_lossy().into_owned())
                    .collect(),
            );
            responses_for_runner
                .lock()
                .expect("responses")
                .pop_front()
                .ok_or_else(|| io::Error::other("unexpected launchctl call"))
        };
        (run, calls)
    }

    let plist = Path::new("/private/tmp/com.teslatlas.hub.plist");
    let domain = "gui/501";
    let service = "gui/501/com.teslatlas.hub";

    let (mut start_runner, start_calls) = runner(vec![false, true, true, true]);
    start_installed_with_runner(plist, domain, service, &mut start_runner).expect("start service");
    assert_eq!(
        start_calls
            .lock()
            .expect("start calls")
            .iter()
            .map(|call| call[0].as_str())
            .collect::<Vec<_>>(),
        ["print", "bootstrap", "kickstart", "print"]
    );

    let (mut stop_runner, stop_calls) = runner(vec![true, true, false]);
    stop_installed_with_runner(service, &mut stop_runner).expect("stop service");
    assert_eq!(
        stop_calls
            .lock()
            .expect("stop calls")
            .iter()
            .map(|call| call[0].as_str())
            .collect::<Vec<_>>(),
        ["bootout", "print", "print"]
    );

    let (mut restart_runner, restart_calls) =
        runner(vec![true, true, false, false, true, true, true]);
    restart_installed_with_runner(plist, domain, service, &mut restart_runner)
        .expect("restart service");
    assert_eq!(
        restart_calls
            .lock()
            .expect("restart calls")
            .iter()
            .map(|call| call[0].as_str())
            .collect::<Vec<_>>(),
        [
            "bootout",
            "print",
            "print",
            "print",
            "bootstrap",
            "kickstart",
            "print"
        ]
    );
}

#[test]
fn service_stop_fails_after_bounded_unload_poll() {
    let mut responses =
        std::iter::once(true).chain(std::iter::repeat_n(true, SERVICE_UNLOAD_ATTEMPTS));
    let mut runner = |_: &[&std::ffi::OsStr]| {
        responses
            .next()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };

    let error = stop_installed_with_runner("gui/501/com.teslatlas.hub", &mut runner)
        .expect_err("persistently loaded service must fail");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("still loaded after stop"));
    assert!(responses.next().is_none());
}

#[test]
fn only_known_launchctl_absence_is_nonfatal() {
    assert!(expected_launchctl_absence(
        "print",
        Some(113),
        "Could not find service"
    ));
    assert!(expected_launchctl_absence(
        "bootout",
        Some(3),
        "No such process"
    ));
    assert!(!expected_launchctl_absence(
        "print",
        Some(1),
        "permission denied"
    ));
    assert!(!expected_launchctl_absence(
        "bootout",
        Some(1),
        "permission denied"
    ));
}

#[test]
fn failed_replacement_restores_loaded_service_without_launchctl() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let binary = temporary.path().join("teslatlas-hub");
    let plist = temporary.path().join("com.teslatlas.hub.plist");
    let previous_binary = temporary.path().join(".teslatlas-hub.previous");
    let previous_plist = temporary.path().join(".com.teslatlas.hub.plist.previous");
    fs::write(&binary, b"new binary").expect("new binary");
    fs::write(&plist, b"new plist").expect("new plist");
    fs::write(&previous_binary, b"old binary").expect("old binary");
    fs::write(&previous_plist, b"old plist").expect("old plist");
    let paths = InstallPaths {
        binary: binary.clone(),
        plist: plist.clone(),
        previous_binary: Some(previous_binary.clone()),
        previous_plist: Some(previous_plist.clone()),
    };
    let mut outcomes = std::collections::VecDeque::from([
        true,  // print: old service loaded
        true,  // bootout: old service
        false, // print: old service unloaded
        false, // bootstrap: replacement fails
        true,  // rollback bootout
        false, // print: replacement service unloaded
        true,  // rollback bootstrap
        true,  // rollback kickstart
    ]);
    let mut calls = Vec::new();
    let result = {
        let mut runner = |arguments: &[&std::ffi::OsStr]| {
            calls.push(
                arguments
                    .iter()
                    .map(|argument| argument.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            );
            outcomes
                .pop_front()
                .ok_or_else(|| io::Error::other("unexpected launchctl call"))
        };
        launch_with_runner(&paths, "gui/501", "gui/501/com.teslatlas.hub", &mut runner)
    };
    assert!(
        result
            .expect_err("replacement must fail")
            .to_string()
            .contains("previous Hub service restored")
    );
    assert_eq!(fs::read(&binary).expect("restored binary"), b"old binary");
    assert_eq!(fs::read(&plist).expect("restored plist"), b"old plist");
    assert!(!previous_binary.exists());
    assert!(!previous_plist.exists());
    assert_eq!(calls[0][0], "print");
    assert_eq!(calls[0][1], "gui/501/com.teslatlas.hub");
    assert_eq!(calls[3][0], "bootstrap");
    assert_eq!(calls[3][1], "gui/501");
    assert_eq!(calls[3][2], plist.display().to_string());
    assert_eq!(calls[6][0], "bootstrap");
    assert_eq!(calls[6][1], "gui/501");
    assert_eq!(calls[6][2], plist.display().to_string());
}

#[test]
fn failed_replacement_restores_loaded_plist_without_binary_backup() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let binary = temporary.path().join("teslatlas-hub");
    let plist = temporary.path().join("com.teslatlas.hub.plist");
    let previous_plist = temporary.path().join(".com.teslatlas.hub.plist.previous");
    fs::write(&binary, b"new binary").expect("new binary");
    fs::write(&plist, b"new plist").expect("new plist");
    fs::write(&previous_plist, b"old plist").expect("old plist");
    let paths = InstallPaths {
        binary: binary.clone(),
        plist: plist.clone(),
        previous_binary: None,
        previous_plist: Some(previous_plist.clone()),
    };
    let mut outcomes = std::collections::VecDeque::from([
        true,  // print: old service loaded
        true,  // bootout: old service
        false, // print: old service unloaded
        true,  // bootstrap: replacement
        false, // kickstart: replacement fails
        true,  // rollback bootout
        false, // print: replacement service unloaded
        true,  // rollback bootstrap
        true,  // rollback kickstart
    ]);
    let mut runner = |_: &[&std::ffi::OsStr]| {
        outcomes
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };
    let error = launch_with_runner(&paths, "gui/501", "gui/501/com.teslatlas.hub", &mut runner)
        .expect_err("replacement must fail");
    assert!(error.to_string().contains("previous Hub service restored"));
    assert_eq!(fs::read(&plist).expect("restored plist"), b"old plist");
    assert_eq!(
        fs::read(&binary).expect("new binary remains"),
        b"new binary"
    );
    assert!(!previous_plist.exists());
}

#[test]
fn installs_private_binary_and_minimal_absolute_plist_without_launchctl() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("Hub Data");
    let home = temporary.path().join("home");
    fs::create_dir_all(&home).expect("home directory");
    let config = data.join("config.toml");
    let executable = temporary.path().join("source-bin");
    seed_ready_hub(&data);
    fs::write(&config, "[hub]\n").expect("config");
    fs::write(&executable, "binary bytes").expect("source binary");

    let paths =
        install_files_after_preflight(&data, &config, &home, &executable).expect("install files");
    assert_eq!(
        fs::read(&paths.binary).expect("installed binary"),
        b"binary bytes"
    );
    assert_eq!(
        fs::metadata(&paths.binary)
            .expect("binary mode")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&paths.plist)
            .expect("plist mode")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let plist = fs::read_to_string(&paths.plist).expect("plist");
    assert!(plist.contains(&format!("<string>{}</string>", paths.binary.display())));
    let canonical_config = config.canonicalize().expect("canonical config");
    assert!(plist.contains(&format!("<string>{}</string>", canonical_config.display())));
    assert!(plist.contains("<string>--config</string>"));
    assert!(plist.contains("<string>serve</string>"));
    assert!(plist.contains(
        "<key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>"
    ));
    assert!(!plist.contains("<key>KeepAlive</key>\n  <true/>"));
    assert!(!plist.contains("EnvironmentVariables"));
    assert!(!plist.contains("ResourceLimits"));
    assert!(!plist.contains("SERVICE_WRAPPER"));
    assert!(plist.contains("<key>ProcessType</key>\n  <string>Background</string>"));
    assert!(plist.contains("<key>Umask</key>\n  <integer>63</integer>"));
}

#[test]
fn empty_hub_preflight_creates_no_install_artifacts() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let home = temporary.path().join("home");
    fs::create_dir_all(&home).expect("home directory");
    let config = data.join("config.toml");
    let executable = temporary.path().join("source-bin");
    HubStore::initialize(&data).expect("empty store");
    fs::write(&config, "[hub]\n").expect("config");
    fs::write(&executable, "binary bytes").expect("source binary");

    assert!(install_files_after_preflight(&data, &config, &home, &executable).is_err());
    assert_no_install_artifacts(&data, &home);
}

#[test]
fn selected_car_without_token_creates_no_install_artifacts() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let home = temporary.path().join("home");
    fs::create_dir_all(&home).expect("home directory");
    let config = data.join("config.toml");
    let executable = temporary.path().join("source-bin");
    let _store = seed_selected_car(&data);
    fs::write(&config, "[hub]\n").expect("config");
    fs::write(&executable, "binary bytes").expect("source binary");

    assert!(install_files_after_preflight(&data, &config, &home, &executable).is_err());
    assert_no_install_artifacts(&data, &home);
}
