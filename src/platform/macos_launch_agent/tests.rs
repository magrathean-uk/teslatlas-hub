// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn standalone_preflight_allows_only_known_v66_forward_upgrade_without_writes() {
    use crate::db::{HubStore, ObservationInput, SourceDescriptor, VehicleDescriptor};
    let temporary = crate::private_tempdir().unwrap();
    let data_dir = temporary.path().join("state");
    let config = source_run_config(temporary.path(), &data_dir);
    let store = HubStore::initialize(&data_dir).unwrap();
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "preflight-fixture"), 1)
        .unwrap();
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "eid:42"), 2)
        .unwrap();
    store.append_observation(&ObservationInput { source_id: source.source_id, vehicle_id: vehicle.vehicle_id,
        observed_at_ms: 3, payload: serde_json::json!({"record_type":"owner_api_vehicle_data_v1","fixture":true}) },4).unwrap();
    let before_records = store
        .current_observations_for_vehicle(vehicle.vehicle_id)
        .unwrap();
    let connection = store.open().unwrap();
    let schema: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='current_observations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let schema66 = schema
        .replacen("current_observations", "current_v66", 1)
        .replace(", 'teslamate_position_v1'", "");
    assert!(!schema66.contains("teslamate_position_v1"));
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {schema66};
        INSERT INTO current_v66 SELECT * FROM current_observations;
        DROP TABLE current_observations; ALTER TABLE current_v66 RENAME TO current_observations;
        PRAGMA user_version=66; COMMIT; PRAGMA wal_checkpoint(TRUNCATE);"
        ))
        .unwrap();
    drop(connection);
    let before_bytes = fs::read(config.database_path()).unwrap();
    assert!(matches!(
        HubStore::open_read_only(&data_dir),
        Err(crate::db::StoreError::UnsupportedSchema(66))
    ));
    preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).unwrap();
    assert_eq!(fs::read(config.database_path()).unwrap(), before_bytes);
    let connection = rusqlite::Connection::open(config.database_path()).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
            .unwrap(),
        66
    );
    drop(connection);
    // The same initialization used by ordinary Serve owns the actual migration.
    let migrated = HubStore::initialize(&data_dir).unwrap();
    assert_eq!(
        migrated
            .current_observations_for_vehicle(vehicle.vehicle_id)
            .unwrap(),
        before_records
    );
    let connection = migrated.open().unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
            .unwrap(),
        67
    );
    preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).unwrap();
    for unsupported in [65_i32, 68] {
        connection
            .pragma_update(None, "user_version", unsupported)
            .unwrap();
        assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).is_err());
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .unwrap(),
            unsupported
        );
    }
    connection.pragma_update(None, "user_version", 66).unwrap();
    connection.pragma_update(None, "application_id", 0).unwrap();
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).is_err());
}

use super::*;
use crate::{
    config::{CollectorProvider, EdgeCollectorConfig, HubConfig, TlsListenerConfig},
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
use rcgen::{CertifiedKey, generate_simple_self_signed};

#[test]
fn launch_agent_plist_routes_logs_to_the_private_data_directory() {
    let plist = render_plist(
        Path::new("/private/tmp/hub"),
        Path::new("/private/tmp/config.toml"),
        Path::new("/private/tmp/private & data"),
    )
    .expect("render plist");
    assert!(plist.contains("<key>StandardOutPath</key>\n  <string>/private/tmp/private &amp; data/hub.stdout.log</string>"));
    assert!(plist.contains("<key>StandardErrorPath</key>\n  <string>/private/tmp/private &amp; data/hub.stderr.log</string>"));
    assert!(!plist.contains("@STDOUT@"));
    assert!(!plist.contains("@STDERR@"));
}

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

fn source_run_config(temporary: &Path, data_dir: &Path) -> HubConfig {
    HubStore::initialize(data_dir).expect("source-run store");
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).expect("TLS identity");
    let certificate_path = temporary.join("source-run-certificate.pem");
    let private_key_path = temporary.join("source-run-private-key.pem");
    fs::write(&certificate_path, cert.pem()).expect("write certificate");
    fs::write(&private_key_path, signing_key.serialize_pem()).expect("write private key");
    fs::set_permissions(&certificate_path, fs::Permissions::from_mode(0o600))
        .expect("protect certificate");
    fs::set_permissions(&private_key_path, fs::Permissions::from_mode(0o600))
        .expect("protect private key");
    let collector = crate::config::CollectorConfig {
        interval_seconds: 0,
        ..Default::default()
    };
    HubConfig {
        data_dir: data_dir.to_owned(),
        bind: "127.0.0.1:21444".parse().expect("loopback bind"),
        tls: Some(TlsListenerConfig {
            certificate_path,
            private_key_path,
            public_url: "https://127.0.0.1:21444/".to_owned(),
        }),
        collector,
        geocoder: Default::default(),
        teslamate: Default::default(),
        terrain: Default::default(),
        http: Default::default(),
    }
}

#[test]
fn source_run_mode_requires_explicit_opt_in_and_known_mode() {
    assert_eq!(development_serve_mode(None, None).unwrap(), None);
    assert_eq!(
        development_serve_mode(Some(OsStr::new("1")), None).unwrap(),
        Some(DevelopmentServeMode::Fixture)
    );
    for (raw, expected) in [
        ("fixture", DevelopmentServeMode::Fixture),
        ("standalone", DevelopmentServeMode::Standalone),
        ("edge", DevelopmentServeMode::Edge),
        ("private-lan", DevelopmentServeMode::PrivateLan),
    ] {
        assert_eq!(
            development_serve_mode(Some(OsStr::new("1")), Some(OsStr::new(raw))).unwrap(),
            Some(expected)
        );
    }
    assert!(development_serve_mode(None, Some(OsStr::new("standalone"))).is_err());
    assert!(development_serve_mode(Some(OsStr::new("true")), None).is_err());
    assert!(development_serve_mode(Some(OsStr::new("1")), Some(OsStr::new("production"))).is_err());
}

#[test]
fn private_lan_source_run_requires_exact_rfc1918_https_and_valid_collection_authority() {
    let temporary = crate::private_tempdir().expect("temporary source-run root");
    let data = temporary.path().join("history");
    let mut config = source_run_config(temporary.path(), &data);
    config.bind = "172.17.17.111:21445".parse().expect("private LAN bind");
    config.tls.as_mut().unwrap().public_url = "https://172.17.17.111:21445".to_owned();
    preflight_hub_for_serve(&config, Some(DevelopmentServeMode::PrivateLan))
        .expect("disabled collector with valid Hub state");
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).is_err());
    assert!(preflight_hub_for_serve(&config, None).is_err());

    for bind in [
        "0.0.0.0:21445",
        "172.17.17.111:0",
        "8.8.8.8:21445",
        "127.0.0.1:21445",
        "169.254.1.1:21445",
        "[::]:21445",
    ] {
        config.bind = bind.parse().expect("test bind");
        assert!(
            preflight_hub_for_serve(&config, Some(DevelopmentServeMode::PrivateLan)).is_err(),
            "{bind}"
        );
    }
    config.bind = "172.17.17.111:21445".parse().unwrap();
    for url in [
        "http://172.17.17.111:21445",
        "https://172.17.17.112:21445",
        "https://172.17.17.111:21446",
        "https://user@172.17.17.111:21445",
        "https://172.17.17.111:21445/?q=1",
        "https://172.17.17.111:21445/#fragment",
        "https://172.17.17.111:21445/other",
        "https://0xac11116f:21445",
    ] {
        config.tls.as_mut().unwrap().public_url = url.to_owned();
        assert!(
            preflight_hub_for_serve(&config, Some(DevelopmentServeMode::PrivateLan)).is_err(),
            "{url}"
        );
    }
    config.tls.as_mut().unwrap().public_url = "https://172.17.17.111:21445/".to_owned();
    config.collector.interval_seconds = 300;
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::PrivateLan)).is_err());
    config.collector.provider = CollectorProvider::Fleet;
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::PrivateLan)).is_err());

    let native_data = temporary.path().join("native");
    seed_ready_hub(&native_data);
    let mut native = source_run_config(temporary.path(), &native_data);
    native.bind = "10.24.0.7:21445".parse().unwrap();
    native.tls.as_mut().unwrap().public_url = "https://10.24.0.7:21445".to_owned();
    native.collector.interval_seconds = 300;
    preflight_hub_for_serve(&native, Some(DevelopmentServeMode::PrivateLan))
        .expect("native Legacy with normal usable credential preflight");
}

#[test]
fn standalone_source_run_accepts_empty_state_but_rejects_collectors_and_fixture_claims() {
    let temporary = crate::private_tempdir().expect("temporary source-run root");
    let data = temporary.path().join("data");
    let mut config = source_run_config(temporary.path(), &data);

    preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone))
        .expect("fresh empty standalone Hub");
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Fixture)).is_err());
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Edge)).is_err());

    config.collector.interval_seconds = 60;
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).is_err());
    config.collector.interval_seconds = 0;
    config.bind = "0.0.0.0:21444".parse().expect("public bind");
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).is_err());
}

#[test]
fn edge_source_run_uses_normal_binding_preflight_and_rejects_standalone_mode() {
    let temporary = crate::private_tempdir().expect("temporary source-run root");
    let data = temporary.path().join("data");
    let store = seed_selected_car(&data);
    let (vehicle_id, _, _) = store.configured_tesla_vehicles().unwrap()[0].clone();
    let source_id: String = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
            rusqlite::params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("source identity");
    let mut config = source_run_config(temporary.path(), &data);
    config.collector.edge = Some(EdgeCollectorConfig {
        base_url: "https://127.0.0.1:24443/".to_owned(),
        ca_certificate_path: temporary.path().join("edge-ca.pem"),
        client_certificate_path: temporary.path().join("edge-client.pem"),
        client_private_key_path: temporary.path().join("edge-client-key.pem"),
        bearer_token_path: temporary.path().join("edge-token"),
        installation_id: "local-edge".to_owned(),
        lineage: "local-spool".to_owned(),
        source_id: source_id.parse().expect("source UUID"),
        vehicle_id,
        vin: "5YJTEST0000000001".to_owned(),
        car_id: 1,
        poll_milliseconds: 500,
        timeout_seconds: 20,
        max_backoff_seconds: 60,
    });

    preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Edge))
        .expect("explicit local Edge binding");
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).is_err());
    config.collector.edge.as_mut().unwrap().vin = "5YJTEST0000000002".to_owned();
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Edge)).is_err());
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

fn prepared_install_fixture(root: &Path, with_previous: bool) -> InstallPaths {
    let binary = root.join("teslatlas-hub");
    let plist = root.join("com.teslatlas.hub.plist");
    fs::write(&binary, b"new binary").expect("new binary");
    fs::write(&plist, b"new plist").expect("new plist");
    let previous_binary = with_previous.then(|| root.join(".teslatlas-hub.previous"));
    let previous_plist = with_previous.then(|| root.join(".com.teslatlas.hub.plist.previous"));
    if let Some(previous) = &previous_binary {
        fs::write(previous, b"old binary").expect("old binary");
    }
    if let Some(previous) = &previous_plist {
        fs::write(previous, b"old plist").expect("old plist");
    }
    InstallPaths {
        binary,
        plist,
        previous_binary,
        previous_plist,
    }
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

    let error = launch_with_runner(
        &paths,
        false,
        "gui/501",
        "gui/501/com.teslatlas.hub",
        &mut runner,
        &mut || Ok(true),
    )
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

    launch_with_runner(
        &paths,
        false,
        "gui/501",
        "gui/501/com.teslatlas.hub",
        &mut runner,
        &mut || Ok(true),
    )
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
fn running_service_is_stopped_and_settled_before_replacement() {
    let service = "gui/501/com.teslatlas.hub";
    let mut outcomes = std::collections::VecDeque::from([true, true, false]);
    let mut calls = Vec::new();
    let mut runner = |arguments: &[&std::ffi::OsStr]| {
        calls.push(arguments[0].to_string_lossy().into_owned());
        outcomes
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };

    assert!(
        stop_for_replacement_with_runner(service, &mut runner)
            .expect("running service should stop before replacement")
    );
    assert_eq!(calls, ["print", "bootout", "print"]);
    assert!(outcomes.is_empty());
}

#[test]
fn replacement_waits_for_lifetime_lock_release_after_launchd_unloads() {
    let mut attempts = 0;
    let admission = wait_for_admission_with_probe(|| {
        attempts += 1;
        if attempts < 3 {
            Err(crate::hub_user_process::UserLifetimeLockError::AlreadyRunning)
        } else {
            Ok("admitted")
        }
    })
    .expect("lock should become available");

    assert_eq!(admission, "admitted");
    assert_eq!(attempts, 3);
}

#[test]
fn replacement_lock_wait_is_bounded() {
    let mut attempts = 0;
    let error = wait_for_admission_with_probe::<()>(|| {
        attempts += 1;
        Err(crate::hub_user_process::UserLifetimeLockError::AlreadyRunning)
    })
    .expect_err("persistently held lock must time out");

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(attempts, LOCK_RELEASE_ATTEMPTS);
}

#[test]
fn launchctl_running_state_requires_a_live_pid() {
    assert_eq!(
        launchctl_print_running_pid(
            "gui/501/com.teslatlas.hub = {\n\tstate = running\n\tpid = 4312\n}"
        ),
        Some(4312)
    );
    assert_eq!(
        launchctl_print_running_pid(
            "gui/501/com.teslatlas.hub = {\n\tstate = exited\n\tlast exit code = 1\n}"
        ),
        None
    );
    assert_eq!(
        launchctl_print_running_pid(
            "gui/501/com.teslatlas.hub = {\n\tstate = running\n\tpid = 0\n}"
        ),
        None
    );
}

#[test]
fn failed_stopped_replacement_restores_previous_files() {
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
        false, // print: service remains stopped
        false, // bootout: absent
        false, // print: absent after bootout
        false, // bootstrap: replacement fails
        false, // rollback bootout: absent
        false, // rollback print: absent
    ]);
    let mut calls = Vec::new();
    let mut runner = |arguments: &[&std::ffi::OsStr]| {
        calls.push(arguments[0].to_string_lossy().into_owned());
        outcomes
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };

    let error = launch_with_runner(
        &paths,
        false,
        "gui/501",
        "gui/501/com.teslatlas.hub",
        &mut runner,
        &mut || Ok(true),
    )
    .expect_err("replacement must fail");
    assert!(
        error
            .to_string()
            .contains("prepared install files restored")
    );
    assert_eq!(fs::read(&binary).expect("restored binary"), b"old binary");
    assert_eq!(fs::read(&plist).expect("restored plist"), b"old plist");
    assert!(!previous_binary.exists());
    assert!(!previous_plist.exists());
    assert_eq!(
        calls,
        ["print", "bootout", "print", "bootstrap", "bootout", "print"]
    );
    assert!(outcomes.is_empty());
}

#[test]
fn failed_fresh_bootstrap_removes_prepared_files() {
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
    let mut outcomes = std::collections::VecDeque::from([
        false, // print: no service
        false, // bootout: absent
        false, // print: absent after bootout
        false, // bootstrap: replacement fails
        false, // rollback bootout: absent
        false, // rollback print: absent
    ]);
    let mut runner = |_: &[&std::ffi::OsStr]| {
        outcomes
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };

    let error = launch_with_runner(
        &paths,
        false,
        "gui/501",
        "gui/501/com.teslatlas.hub",
        &mut runner,
        &mut || Ok(true),
    )
    .expect_err("fresh bootstrap must fail");
    assert!(
        error
            .to_string()
            .contains("prepared install files restored")
    );
    assert!(!binary.exists());
    assert!(!plist.exists());
    assert!(outcomes.is_empty());
}

#[test]
fn replacement_without_stable_readiness_restores_running_service() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let paths = prepared_install_fixture(temporary.path(), true);
    let mut outcomes = std::collections::VecDeque::from([
        false, // print: old service was already stopped
        false, // bootout: absent
        false, // print: absent after bootout
        true,  // bootstrap replacement
        true,  // kickstart replacement
        true,  // rollback bootout replacement
        false, // print: replacement unloaded
        true,  // bootstrap previous service
        true,  // kickstart previous service
    ]);
    let mut runner = |_: &[&std::ffi::OsStr]| {
        outcomes
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };
    let mut probes = 0;
    let mut readiness = || {
        probes += 1;
        Ok(probes > SERVICE_READY_ATTEMPTS)
    };

    let error = launch_with_runner(
        &paths,
        true,
        "gui/501",
        "gui/501/com.teslatlas.hub",
        &mut runner,
        &mut readiness,
    )
    .expect_err("unstable replacement must roll back");
    assert!(error.to_string().contains("previous Hub service restored"));
    assert_eq!(fs::read(&paths.binary).expect("old binary"), b"old binary");
    assert_eq!(fs::read(&paths.plist).expect("old plist"), b"old plist");
    assert_eq!(
        probes,
        SERVICE_READY_ATTEMPTS + SERVICE_READY_STABLE_OBSERVATIONS
    );
    assert!(outcomes.is_empty());
}

#[test]
fn replacement_without_stable_readiness_restores_stopped_files() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let paths = prepared_install_fixture(temporary.path(), true);
    let mut outcomes = std::collections::VecDeque::from([
        false, // print: service remains stopped
        false, // bootout: absent
        false, // print: absent after bootout
        true,  // bootstrap replacement
        true,  // kickstart replacement
        true,  // rollback bootout replacement
        false, // print: replacement unloaded
    ]);
    let mut runner = |_: &[&std::ffi::OsStr]| {
        outcomes
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };

    let error = launch_with_runner(
        &paths,
        false,
        "gui/501",
        "gui/501/com.teslatlas.hub",
        &mut runner,
        &mut || Ok(false),
    )
    .expect_err("unstable replacement must restore stopped files");
    assert!(
        error
            .to_string()
            .contains("prepared install files restored")
    );
    assert_eq!(fs::read(&paths.binary).expect("old binary"), b"old binary");
    assert_eq!(fs::read(&paths.plist).expect("old plist"), b"old plist");
    assert!(outcomes.is_empty());
}

#[test]
fn fresh_install_without_stable_readiness_removes_files() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let paths = prepared_install_fixture(temporary.path(), false);
    let mut outcomes = std::collections::VecDeque::from([
        false, // print: no service
        false, // bootout: absent
        false, // print: absent after bootout
        true,  // bootstrap replacement
        true,  // kickstart replacement
        true,  // rollback bootout replacement
        false, // print: replacement unloaded
    ]);
    let mut runner = |_: &[&std::ffi::OsStr]| {
        outcomes
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };

    let error = launch_with_runner(
        &paths,
        false,
        "gui/501",
        "gui/501/com.teslatlas.hub",
        &mut runner,
        &mut || Ok(false),
    )
    .expect_err("unstable fresh install must roll back");
    assert!(
        error
            .to_string()
            .contains("prepared install files restored")
    );
    assert!(!paths.binary.exists());
    assert!(!paths.plist.exists());
    assert!(outcomes.is_empty());
}

#[test]
fn stable_replacement_commits_only_after_consecutive_readiness() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let paths = prepared_install_fixture(temporary.path(), true);
    let previous_binary = paths.previous_binary.clone().expect("previous binary");
    let previous_plist = paths.previous_plist.clone().expect("previous plist");
    let mut outcomes = std::collections::VecDeque::from([
        false, // print: old service was already stopped
        false, // bootout: absent
        false, // print: absent after bootout
        true,  // bootstrap replacement
        true,  // kickstart replacement
    ]);
    let mut runner = |_: &[&std::ffi::OsStr]| {
        outcomes
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };
    let mut readiness = std::collections::VecDeque::from(
        std::iter::repeat_n(true, SERVICE_READY_STABLE_OBSERVATIONS - 1)
            .chain(std::iter::once(false))
            .chain(std::iter::repeat_n(true, SERVICE_READY_STABLE_OBSERVATIONS))
            .collect::<Vec<_>>(),
    );

    launch_with_runner(
        &paths,
        true,
        "gui/501",
        "gui/501/com.teslatlas.hub",
        &mut runner,
        &mut || {
            readiness
                .pop_front()
                .ok_or_else(|| io::Error::other("unexpected readiness probe"))
        },
    )
    .expect("stable replacement should commit");
    assert_eq!(fs::read(&paths.binary).expect("new binary"), b"new binary");
    assert_eq!(fs::read(&paths.plist).expect("new plist"), b"new plist");
    assert!(!previous_binary.exists());
    assert!(!previous_plist.exists());
    assert!(readiness.is_empty());
    assert!(outcomes.is_empty());
}

#[test]
fn readiness_rechecks_listener_when_same_pid_stays_running() {
    let mut ready_pid = None;
    let mut listener_states = std::collections::VecDeque::from(
        std::iter::repeat_n(true, SERVICE_READY_STABLE_OBSERVATIONS - 1)
            .chain(std::iter::once(false))
            .chain(std::iter::repeat_n(true, SERVICE_READY_STABLE_OBSERVATIONS))
            .collect::<Vec<_>>(),
    );
    let mut listener_probes = 0;

    wait_for_service_ready_with_probe(&mut || {
        readiness_observation(&mut ready_pid, 42, &mut || {
            listener_probes += 1;
            listener_states
                .pop_front()
                .ok_or_else(|| io::Error::other("unexpected listener probe"))
        })
    })
    .expect("listener recovery under the same process should become stable");

    assert_eq!(listener_probes, SERVICE_READY_STABLE_OBSERVATIONS * 2);
    assert!(listener_states.is_empty());
    assert_eq!(ready_pid, Some(42));
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
        false, // print: old service was already stopped for replacement
        false, // bootout: absent
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
        launch_with_runner(
            &paths,
            true,
            "gui/501",
            "gui/501/com.teslatlas.hub",
            &mut runner,
            &mut || Ok(true),
        )
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
        false, // print: old service was already stopped for replacement
        false, // bootout: absent
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
    let error = launch_with_runner(
        &paths,
        true,
        "gui/501",
        "gui/501/com.teslatlas.hub",
        &mut runner,
        &mut || Ok(true),
    )
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
