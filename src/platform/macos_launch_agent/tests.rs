// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn standalone_preflight_enforces_the_store_floor_without_writes() {
    let test = "standalone_preflight_enforces_the_store_floor_without_writes";
    let temporary = crate::private_tempdir().unwrap();
    let data_dir = temporary.path().join("state");
    let config = source_run_config(temporary.path(), &data_dir);
    let store = crate::db::HubStore::initialize(&data_dir).unwrap();
    preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).unwrap();
    let connection = store.open().unwrap();
    connection
        .pragma_update(None, "user_version", crate::db::MIN_STORE_VERSION - 1)
        .unwrap();
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(connection);
    let before = fs::read(config.database_path()).unwrap();
    assert!(
        matches!(
            crate::db::HubStore::preflight_standalone_schema_read_only(&data_dir),
            Err(crate::db::StoreError::UnsupportedSchema(77))
        ),
        "assertion failed: {test}: standalone schema admission reports UnsupportedSchema(77)"
    );
    assert!(
        matches!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)), Err(error) if error.to_string().contains("hub schema version 77")),
        "assertion failed: {test}: serve preflight preserves the below-floor schema refusal"
    );
    assert!(
        matches!(
            crate::db::HubStore::initialize(&data_dir),
            Err(crate::db::StoreError::UnsupportedSchema(77))
        ),
        "assertion failed: {test}: writable opening reports UnsupportedSchema(77)"
    );
    assert!(
        fs::read(config.database_path()).unwrap() == before,
        "assertion failed: {test}: floor refusal leaves the catalogue bytes unchanged"
    );
}

use super::*;
use crate::{
    config::{CollectorProvider, HubConfig, TlsListenerConfig},
    db::{HubStore, },
    fleet_api::FleetRegion,
    fleet_credentials::FleetSetupCredentials,
    hub_pack::ProjectionCarSettings,
    legacy_owner_credentials::{load_or_create_cursor_key, },
    protocol::CursorKey,
    source_db_import::{SourceDbImportRequest, SourceDbImportScope, publish_history},
    source_db_projection::{SourceDbCar, SourceDbHistory},
    
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{os::unix::fs::PermissionsExt, time::SystemTime};

fn seed_selected_car(data_dir: &Path) -> HubStore {
    let store = HubStore::initialize(data_dir).expect("store");
    let history = SourceDbHistory {
        cars: vec![SourceDbCar {
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
        &SourceDbImportRequest {
            new_import_source: false,
            source_key: "install-fixture".to_owned(),
            scope: SourceDbImportScope::Selected(1),
            imported_at_ms: 1_000,
        },
        &history,
    )
    .expect("imported car");
    store
}

fn seed_ready_hub(data_dir: &Path) {
    let store = seed_selected_car(data_dir);
    load_or_create_cursor_key(data_dir).expect("cursor key");
    crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(&store, data_dir, "access-0001", "refresh-0001", None).expect("Owner entry");
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
    crate::auth::credential_vault::fixtures::seed_fleet_setup_credentials(&store, data_dir, &credentials, SystemTime::now())
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
        import_source: Default::default(),
        terrain: Default::default(),
        http: Default::default(),
        pairing: Default::default(),
        backup: Default::default(),
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
    // The retired Edge mode is refused by name.
    assert!(development_serve_mode(Some(OsStr::new("1")), Some(OsStr::new("edge"))).is_err());
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
    config.collector.interval_seconds = 60;
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).is_err());
    config.collector.interval_seconds = 0;
    config.bind = "0.0.0.0:21444".parse().expect("public bind");
    assert!(preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone)).is_err());
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

/// The Fleet-only hub of the install fixtures, with an identified unresolved
/// refresh: a started token receipt bound to the stored pair, with its input fence.
fn seed_fleet_unresolved_hub(data_dir: &Path) -> (HubStore, uuid::Uuid) {
    // Since the Fleet admission (M09-T08) only the Hub's own grant is a
    // usable Fleet pair: the fixture signs in through the grant path.
    let store = seed_selected_car(data_dir);
    load_or_create_cursor_key(data_dir).expect("cursor key");
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(m09_t19_fleet_sign_in(&store, data_dir));
    let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store.clone(), data_dir);
    let slot = crate::auth::credential_envelope::CredentialSlot::FleetApi;
    let generation = vault.stored_generation(slot).expect("generation read").expect("Fleet pair");
    vault.begin_refresh(slot, generation).expect("identified unresolved refresh");
    drop(vault);
    (store, generation)
}

/// Everything a preflight must leave alone: the stored pair, the receipts and
/// fences of the refresh, the collector rows and the data directory's files.
fn preflight_snapshot(store: &HubStore, data_dir: &Path) -> String {
    let connection = store.open().expect("catalogue");
    let receipts: Vec<(i64, String)> = connection
        .prepare("SELECT id, outcome FROM outbound_request_receipts ORDER BY id")
        .expect("receipts")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("receipt rows")
        .collect::<Result<_, _>>()
        .expect("receipt values");
    let fences: i64 = connection.query_row("SELECT (SELECT count(*) FROM credential_refresh_fences) + (SELECT count(*) FROM credential_legacy_fences)", [], |row| row.get(0)).expect("fences");
    let pair: Vec<(Vec<u8>, Vec<u8>, String)> = connection.prepare("SELECT access_envelope,refresh_envelope,generation FROM credential_pairs WHERE slot='fleet_api'").expect("pair query")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).expect("pair rows").collect::<Result<_,_>>().expect("pair values");
    let mut files: Vec<String> = fs::read_dir(data_dir)
        .expect("data directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with("hub.sqlite"))
        .collect();
    files.sort();
    format!(
        "{receipts:?}|{fences}|{pair:?}|{:?}|{files:?}",
        store.load_collector_state_rows().expect("rows").account
    )
}

#[test]
fn fleet_unresolved_refresh_passes_production_and_gui_start_preflight() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data_dir = temporary.path().join("state");
    let (store, _generation) = seed_fleet_unresolved_hub(&data_dir);
    let mut config = source_run_config(temporary.path(), &data_dir);
    config.collector.provider = CollectorProvider::Fleet;
    config.collector.interval_seconds = 30;
    // The pair, key, cipher, generation, claims and scopes are sound, and the
    // refresh is the identified unresolved one; the operational predicate still
    // fences it.
    assert!(
        crate::fleet_credentials::inspect_stored_fleet_credentials(&store, &data_dir)
            .expect("an independently valid pair")
            .unresolved_refresh
    );
    assert!(matches!(
        crate::auth::credential_vault::fixtures::validate_fixture_fleet_credentials(&store, &data_dir),
        Err(crate::fleet_credentials::FleetCredentialError::RotationOutcomeUnknown)
    ));
    let before = preflight_snapshot(&store, &data_dir);
    // The provider preflight, the generic one, the production configuration
    // preflight, the Serve preflight and the GUI Start path (which reaches the
    // configuration preflight through the app's preflight path) all pass, read
    // only, with no fallback to Legacy credentials.
    preflight_hub_for_provider(&data_dir, CollectorProvider::Fleet)
        .expect("the provider preflight admits the reconnect stop");
    preflight_hub(&data_dir).expect("the generic preflight admits it");
    preflight_hub_for_config(&config).expect("the production configuration preflight");
    preflight_hub_for_serve(&config, None).expect("the production Serve preflight");
    assert!(
        preflight_hub_for_provider(&data_dir, CollectorProvider::Legacy).is_err(),
        "there is no Legacy fallback"
    );
    assert_eq!(
        preflight_snapshot(&store, &data_dir),
        before,
        "nothing was written"
    );
    assert!(
        store.unresolved_fleet_refresh_status().expect("receipts"),
        "nothing was settled"
    );
}

type PreflightFault = fn(&Path, &HubStore, uuid::Uuid);

/// Seed an unresolved Fleet refresh, apply one independent fault and require every
/// preflight entry to refuse without writing.
fn assert_fleet_preflight_rejects(label: &str, fault: PreflightFault) {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data_dir = temporary.path().join("state");
    let (store, generation) = seed_fleet_unresolved_hub(&data_dir);
    fault(&data_dir, &store, generation);
    let mut config = source_run_config(temporary.path(), &data_dir);
    config.collector.provider = CollectorProvider::Fleet;
    config.collector.interval_seconds = 30;
    let before = preflight_snapshot(&store, &data_dir);
    assert!(
        preflight_hub_for_provider(&data_dir, CollectorProvider::Fleet).is_err(),
        "{label}"
    );
    assert!(preflight_hub_for_config(&config).is_err(), "{label}");
    assert!(preflight_hub_for_serve(&config, None).is_err(), "{label}");
    assert!(preflight_hub(&data_dir).is_err(), "{label}");
    assert_eq!(
        preflight_snapshot(&store, &data_dir),
        before,
        "{label}: no write"
    );
}

#[test]
fn fleet_unresolved_preflight_rejects_independent_credential_failures() {
    let faults: [(&str, PreflightFault); 3] = [
        ("missing key", |directory, _, _| {
            fs::remove_file(directory.join("secrets").join("credential-keys"))
                .expect("remove key");
        }),
        ("bad ciphertext", |_, store, _| {store.open().expect("catalogue").execute(
                    "UPDATE credential_pairs SET access_envelope=zeroblob(length(access_envelope)) WHERE slot='fleet_api'",
                    [],
                )
                .expect("damage ciphertext");
        }),
        ("generation mismatch", |_, store, generation| {store.open().expect("catalogue").execute(
                    "UPDATE credential_pairs SET generation = ?1 WHERE slot='fleet_api'",
                    rusqlite::params![uuid::Uuid::from_u128(generation.as_u128() ^ 1).to_string()],
                )
                .expect("mismatch");
        }),
    ];
    for (label, fault) in faults {
        assert_fleet_preflight_rejects(label, fault);
    }
    // A generation mismatch with no unresolved receipt is the same error and fails
    // too: the unknown-outcome error is never accepted blindly.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data_dir = temporary.path().join("state");
    seed_fleet_ready_hub(&data_dir);
    let store = HubStore::initialize(&data_dir).expect("store");
    let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store.clone(), &data_dir);
    let generation = vault.stored_generation(crate::auth::credential_envelope::CredentialSlot::FleetApi).expect("tokens").expect("generation");
    drop(vault);
    store.open().expect("catalogue").execute(
            "UPDATE credential_pairs SET generation = ?1 WHERE slot='fleet_api'",
            rusqlite::params![uuid::Uuid::from_u128(generation.as_u128() ^ 1).to_string()],
        )
        .expect("mismatch");
    assert!(!store.unresolved_fleet_refresh_status().expect("receipts"));
    let before = preflight_snapshot(&store, &data_dir);
    assert!(preflight_hub_for_provider(&data_dir, CollectorProvider::Fleet).is_err());
    assert!(preflight_hub(&data_dir).is_err());
    assert_eq!(preflight_snapshot(&store, &data_dir), before);
}

#[test]
fn installed_service_start_uses_bounded_launchctl_sequence() {
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

    let plist = Path::new("/private/tmp/com.tesatlas.hub.plist");
    let domain = "gui/501";
    let service = "gui/501/com.tesatlas.hub";

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
}

#[test]
fn admission_waits_for_lifetime_lock_release_after_service_unload() {
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
fn admission_lock_wait_is_bounded() {
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
            "gui/501/com.tesatlas.hub = {\n\tstate = running\n\tpid = 4312\n}"
        ),
        Some(4312)
    );
    assert_eq!(
        launchctl_print_running_pid(
            "gui/501/com.tesatlas.hub = {\n\tstate = exited\n\tlast exit code = 1\n}"
        ),
        None
    );
    assert_eq!(
        launchctl_print_running_pid(
            "gui/501/com.tesatlas.hub = {\n\tstate = running\n\tpid = 0\n}"
        ),
        None
    );
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
fn readiness_requires_application_pid_configuration_and_ready_status() {
    let digest = crate::protocol::Sha256Digest::from_bytes([38; 32]);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-teslatlas-native-process-id", "42".parse().unwrap());
    headers.insert(
        crate::server::NATIVE_CONFIG_DIGEST_HEADER,
        digest.to_string().parse().unwrap(),
    );
    assert!(application_readiness_matches(
        reqwest::StatusCode::OK,
        &headers,
        br#"{"status":"ready"}"#,
        42,
        digest
    ));
    for (status, pid, expected_digest, body) in [
        (
            reqwest::StatusCode::OK,
            43,
            digest,
            br#"{"status":"ready"}"#.as_slice(),
        ),
        (
            reqwest::StatusCode::OK,
            42,
            crate::protocol::Sha256Digest::from_bytes([39; 32]),
            br#"{"status":"ready"}"#.as_slice(),
        ),
        (
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            42,
            digest,
            br#"{"status":"ready"}"#.as_slice(),
        ),
        (
            reqwest::StatusCode::OK,
            42,
            digest,
            br#"{"status":"not_ready"}"#.as_slice(),
        ),
        (
            reqwest::StatusCode::OK,
            42,
            digest,
            br#"{"status":"ok"}"#.as_slice(),
        ),
        (
            reqwest::StatusCode::OK,
            42,
            digest,
            br#"{"status":"ready","status":"ready"}"#.as_slice(),
        ),
    ] {
        assert!(!application_readiness_matches(
            status,
            &headers,
            body,
            pid,
            expected_digest
        ));
    }
    headers.remove(crate::server::NATIVE_CONFIG_DIGEST_HEADER);
    assert!(!application_readiness_matches(
        reqwest::StatusCode::OK,
        &headers,
        br#"{"status":"ready"}"#,
        42,
        digest
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(target_os = "macos")]
async fn readiness_transport_rejects_unrelated_listener_and_checks_configured_tls_identity() {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().unwrap();
    let identity = rcgen::generate_simple_self_signed(vec!["readiness.hub.test".into()]).unwrap();
    let certificate = temporary.path().join("server.pem");
    fs::write(&certificate, identity.cert.pem()).unwrap();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        identity.cert.pem().into_bytes(),
        identity.signing_key.serialize_pem().into_bytes(),
    )
    .await
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let previous_config_path = temporary.path().join("previous.toml");
    let config_text = format!(
        "data_dir = '{}'\nbind = '{}'\n[collector]\ninterval_seconds = 0\n[tls]\ncertificate_path = '{}'\nprivate_key_path = '{}'\npublic_url = 'https://readiness.hub.test:{}/'\n",
        temporary.path().join("hub").display(),
        address,
        certificate.display(),
        temporary.path().join("unused-key.pem").display(),
        address.port()
    );
    fs::write(&previous_config_path, &config_text).unwrap();
    fs::set_permissions(&previous_config_path, fs::Permissions::from_mode(0o600)).unwrap();
    let (_, digest) = HubConfig::load_with_digest(&previous_config_path).unwrap();
    let response_digest = digest.to_string();
    let app = axum::Router::new().route(
        "/readyz",
        axum::routing::get(move || {
            let digest = response_digest.clone();
            async move {
                (
                    [
                        ("x-teslatlas-native-process-id", "42".to_owned()),
                        (crate::server::NATIVE_CONFIG_DIGEST_HEADER, digest),
                    ],
                    axum::Json(serde_json::json!({"status":"ready"})),
                )
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, tls)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap()
    });
    let mut config = source_run_config(temporary.path(), &temporary.path().join("hub"));
    config.bind = address;
    config.tls = Some(crate::config::TlsListenerConfig {
        certificate_path: certificate.clone(),
        private_key_path: temporary.path().join("unused-key.pem"),
        public_url: format!("https://readiness.hub.test:{}/", address.port()),
    });
    assert!(configured_application_is_ready(&config, digest, 42).unwrap());
    assert!(!configured_application_is_ready(&config, digest, 43).unwrap());
    assert!(
        !configured_application_is_ready(
            &config,
            crate::protocol::Sha256Digest::from_bytes([39; 32]),
            42
        )
        .unwrap()
    );
    config.tls.as_mut().unwrap().public_url = format!("https://wrong.hub.test:{}/", address.port());
    assert!(!configured_application_is_ready(&config, digest, 42).unwrap());

    server.abort();
    let _ = server.await;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    config.bind = listener.local_addr().unwrap();
    config.tls = None;
    // A reachable listener that does not serve the Hub endpoint was enough
    // for the previous implementation; it must not commit readiness now.
    assert!(!configured_application_is_ready(&config, digest, 42).unwrap());
}

#[test]
fn service_stop_fails_after_bounded_unload_poll() {
    let mut responses = std::iter::repeat_n(true, 2 + SERVICE_UNLOAD_ATTEMPTS);
    let mut calls = Vec::new();
    let mut runner = |arguments: &[&OsStr]| {
        calls.push(arguments[0].to_string_lossy().into_owned());
        responses
            .next()
            .ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };
    let error = stop_installed_with_runner("gui/501/com.tesatlas.hub", &mut runner).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("still loaded after stop"));
    assert_eq!(calls[0], "disable");
    assert_eq!(calls[1], "bootout");
    assert_eq!(calls.len(), 2 + SERVICE_UNLOAD_ATTEMPTS);
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
fn empty_hub_preflight_is_refused() {
    let temporary = crate::private_tempdir().unwrap();
    let data = temporary.path().join("data");
    HubStore::initialize(&data).expect("empty store");
    assert!(preflight_hub(&data).is_err());
}

#[test]
fn selected_car_without_token_preflight_is_refused() {
    let temporary = crate::private_tempdir().unwrap();
    let data = temporary.path().join("data");
    let _store = seed_selected_car(&data);
    assert!(preflight_hub(&data).is_err());
}

#[path = "log_compaction_tests.rs"]
mod log_compaction_tests;

#[test]
fn stop_disables_then_boots_out_and_waits_for_unload() {
    for (responses, expected) in [
        (
            vec![true, true, true, false],
            vec!["disable", "bootout", "print", "print"],
        ),
        (
            vec![true, false, false],
            vec!["disable", "bootout", "print"],
        ),
    ] {
        let mut responses = responses.into_iter();
        let mut calls = Vec::new();
        stop_installed_with_runner("gui/501/com.tesatlas.hub", &mut |arguments| {
            calls.push(
                arguments
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            );
            Ok(responses.next().expect("bounded sequence"))
        })
        .unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|call| call[0].as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            calls
                .iter()
                .all(|call| call.last().unwrap() == "gui/501/com.tesatlas.hub")
        );
        assert!(responses.next().is_none());
    }
}

#[test]
fn stop_does_not_boot_out_when_disable_fails() {
    for refused in [false, true] {
        let mut calls = Vec::new();
        let error = stop_installed_with_runner("synthetic", &mut |arguments| {
            calls.push(arguments[0].to_string_lossy().into_owned());
            if refused {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "disable refused",
                ))
            } else {
                Ok(false)
            }
        })
        .unwrap_err();
        assert_eq!(calls, ["disable"]);
        if refused {
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        } else {
            assert_eq!(error.to_string(), "launchctl failed");
        }
    }
}

#[test]
fn label_and_service_target_are_fixed() {
    assert_eq!(LABEL, "com.tesatlas.hub");
    let domain = format!("gui/{}", rustix::process::geteuid().as_raw());
    assert_eq!(
        service_identifiers(),
        (domain.clone(), format!("{domain}/com.tesatlas.hub"))
    );
}

#[test]
fn service_readiness_wait_is_bounded() {
    let mut calls = 0;
    let error = wait_for_service_ready_with_probe(&mut || {
        calls += 1;
        if calls > SERVICE_READY_ATTEMPTS {
            return Err(io::Error::other("probe exceeded readiness bound"));
        }
        Ok(false)
    })
    .unwrap_err();
    assert_eq!(calls, SERVICE_READY_ATTEMPTS);
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn real_launchctl_is_refused_in_unit_tests() {
    let target = format!(
        "gui/{}/com.tesatlas.absent-test",
        rustix::process::geteuid().as_raw()
    );
    let error = real_launchctl(&[OsStr::new("print"), OsStr::new(&target)]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(error.to_string(), REAL_LAUNCHCTL_REFUSAL);
}

#[test]
fn real_service_running_pid_is_refused_in_unit_tests() {
    let target = format!(
        "gui/{}/com.tesatlas.absent-test",
        rustix::process::geteuid().as_raw()
    );
    let error = real_service_running_pid(&target).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(error.to_string(), REAL_LAUNCHCTL_REFUSAL);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(target_os = "macos")]
async fn installed_plist_identity_selects_the_served_snapshot() {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let config_text = format!(
        "data_dir = '{}'\nbind = '{}'\n[collector]\ninterval_seconds = 0\n",
        temporary.path().join("hub").display(),
        address
    );
    let served_path = temporary.path().join("served.toml");
    let other_path = temporary.path().join("other.toml");
    for (path, text) in [
        (&served_path, config_text.clone()),
        (&other_path, format!("{config_text}# another snapshot\n")),
    ] {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let (served, digest) = HubConfig::load_with_digest(&served_path).unwrap();
    let (other, other_digest) = HubConfig::load_with_digest(&other_path).unwrap();
    assert_eq!(served.bind, other.bind);
    assert_ne!(digest, other_digest);
    let plist = temporary.path().join("installed.plist");
    // Independent four-argument XML, matching the per-user app writer's shape.
    let config_path = served_path
        .to_str()
        .unwrap()
        .replace('&', "&amp;")
        .replace('<', "&lt;");
    fs::write(&plist, format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>ProgramArguments</key><array><string>/Applications/Tesatlas Hub.app/Contents/Resources/tesatlas-hub</string><string>--config</string><string>{config_path}</string><string>serve</string></array></dict></plist>")).unwrap();
    let (installed, installed_digest) = configured_identity_from_plist(&plist).unwrap();
    assert_eq!(installed_digest, digest);
    assert_eq!(installed.data_dir, served.data_dir);
    assert_eq!(installed.bind, served.bind);
    let app = axum::Router::new().route(
        "/readyz",
        axum::routing::get(move || async move {
            (
                [
                    ("x-teslatlas-native-process-id", "42".to_owned()),
                    (
                        crate::server::NATIVE_CONFIG_DIGEST_HEADER,
                        digest.to_string(),
                    ),
                ],
                axum::Json(serde_json::json!({"status":"ready"})),
            )
        }),
    );
    let handle = axum_server::Handle::new();
    let shutdown = handle.clone();
    let server = tokio::spawn(async move {
        axum_server::from_tcp(listener)
            .unwrap()
            .handle(handle)
            .serve(app.into_make_service())
            .await
            .unwrap();
    });
    let installed_ready = configured_application_is_ready(&installed, installed_digest, 42);
    let other_ready = configured_application_is_ready(&other, other_digest, 42);
    shutdown.graceful_shutdown(None);
    server.await.unwrap();
    assert!(installed_ready.unwrap());
    assert!(!other_ready.unwrap());
}

/// The configuration the Mac app writes on a fresh Mac: loopback bind, no TLS,
/// every collector path off (M02-T21).
fn idle_start_config(data_dir: &Path) -> HubConfig {
    HubConfig {
        data_dir: data_dir.to_owned(),
        bind: "127.0.0.1:8080".parse().expect("loopback bind"),
        tls: None,
        collector: crate::config::CollectorConfig {
            interval_seconds: 0,
            ..Default::default()
        },
        geocoder: Default::default(),
        import_source: Default::default(),
        terrain: Default::default(),
        http: Default::default(),
        pairing: Default::default(),
        backup: Default::default(),
    }
}

/// DEC-115: a data folder that was never configured passes the production
/// serve admission idle, before and after `serve` creates its empty store,
/// while the full admission it bypasses still refuses that same state.
#[test]
fn idle_start_admits_a_never_configured_data_folder() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let config = idle_start_config(&data);
    assert!(hub_is_never_configured(&config).expect("nothing to read"));
    preflight_hub_for_serve(&config, None).expect("idle start before any store");
    assert!(!data.join("hub.sqlite").exists(), "the check creates no store");

    HubStore::initialize(&data).expect("the empty store serve creates");
    assert!(hub_is_never_configured(&config).expect("empty store"));
    preflight_hub_for_serve(&config, None).expect("idle start on the empty store");
    assert!(preflight_hub_for_config(&config).is_err(), "the full admission is unchanged");
}

/// DEC-115: anything but never configured keeps today's admission, so a
/// configured but broken Hub, or one outside the idle definition, is refused.
#[test]
fn idle_start_refuses_a_configured_but_broken_hub() {
    let refused = |config: &HubConfig, label: &str| {
        assert!(!hub_is_never_configured(config).unwrap_or(false), "{label}: not idle");
        assert!(preflight_hub_for_serve(config, None).is_err(), "{label}: refused");
    };

    // A collector path on with nothing configured.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    HubStore::initialize(&data).expect("empty store");
    let mut config = idle_start_config(&data);
    config.collector.interval_seconds = 30;
    refused(&config, "cadence without car or credential");

    // A non-loopback bind with nothing configured.
    let mut config = idle_start_config(&data);
    config.bind = "192.168.1.10:8080".parse().expect("LAN bind");
    refused(&config, "LAN bind");

    // A configured car with every collector path off.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let _store = seed_selected_car(&data);
    refused(&idle_start_config(&data), "car with the collector off");

    // A stored credential with every collector path off.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = HubStore::initialize(&data).expect("empty store");
    load_or_create_cursor_key(&data).expect("cursor key");
    crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(&store, &data, "idle-access-0001", "idle-refresh-0001", None)
        .expect("Owner entry");
    refused(&idle_start_config(&data), "credential with the collector off");

    // A store that cannot be read is an error, never idle.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    fs::create_dir_all(&data).expect("data folder");
    fs::write(data.join("hub.sqlite"), b"not a database").expect("damaged store");
    let config = idle_start_config(&data);
    assert!(hub_is_never_configured(&config).is_err(), "damaged store");
    assert!(preflight_hub_for_serve(&config, None).is_err(), "damaged store refused");
}

/// The rename hand-over reads which program the loaded job runs from
/// `launchctl print` (layout as printed by macOS: one tab, then `program = `).
#[test]
fn the_loaded_jobs_program_is_read_from_its_own_line_only() {
    let installed = Path::new("/Applications/Tesatlas Hub.app/Contents/Resources/tesatlas-hub");
    let print = "gui/501/com.tesatlas.hub = {\n\tactive count = 1\n\tstate = running\n\n\tprogram = /Applications/Tesatlas Hub.app/Contents/Resources/tesatlas-hub\n\targuments = {\n\t\t/Applications/Tesatlas Hub.app/Contents/Resources/tesatlas-hub\n\t}\n}\n";
    assert_eq!(launchctl_print_program(print).as_deref(), Some(installed));
    assert_eq!(loaded_job(Some(print), installed), LoadedJob::Installed);
    assert_eq!(loaded_job(None, installed), LoadedJob::Absent);
    let other = print.replace("program = /Applications/Tesatlas Hub.app", "program = /Users/o/Applications/Tesatlas Hub.app");
    assert_eq!(loaded_job(Some(other.as_str()), installed), LoadedJob::Stale, "another bundle's binary");
    // No own line (a nested or relative one does not count): not shown to be
    // the installed program, so the definition is loaded again.
    let nested = "gui/501/com.tesatlas.hub = {\n\tendpoints = {\n\t\tprogram = /Applications/Tesatlas Hub.app/Contents/Resources/tesatlas-hub\n\t}\n}\n";
    assert_eq!(loaded_job(Some(nested), installed), LoadedJob::Stale);
    assert_eq!(loaded_job(Some("\tprogram = tesatlas-hub\n"), Path::new("tesatlas-hub")), LoadedJob::Stale);
}

#[test]
fn the_installed_files_program_and_configuration_are_both_absolute() {
    let temporary = crate::private_tempdir().unwrap();
    let plist = temporary.path().join("installed.plist");
    let write = |program: &str| {
        fs::write(&plist, format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>ProgramArguments</key><array><string>{program}</string><string>--config</string><string>/h/Tesatlas Hub/config.toml</string><string>serve</string></array></dict></plist>")).unwrap();
    };
    write("/Applications/Tesatlas Hub.app/Contents/Resources/tesatlas-hub");
    assert_eq!(
        installed_arguments_from_plist(&plist).unwrap(),
        (
            PathBuf::from("/Applications/Tesatlas Hub.app/Contents/Resources/tesatlas-hub"),
            PathBuf::from("/h/Tesatlas Hub/config.toml")
        )
    );
    write("tesatlas-hub");
    assert!(installed_arguments_from_plist(&plist).is_err(), "a relative program is refused");
}

#[test]
fn a_stale_loaded_definition_is_replaced_without_disabling_the_service() {
    let plist = Path::new("/private/tmp/com.tesatlas.hub.plist");
    let mut calls: Vec<String> = Vec::new();
    // bootout, print (still loaded), print (gone), bootstrap, print (loaded).
    let mut answers = std::collections::VecDeque::from([true, true, false, true, true]);
    let mut runner = |arguments: &[&OsStr]| {
        calls.push(arguments.iter().map(|argument| argument.to_string_lossy()).collect::<Vec<_>>().join(" "));
        answers.pop_front().ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };
    reload_installed_with_runner(plist, "gui/501", "gui/501/com.tesatlas.hub", &mut runner).expect("reload");
    assert_eq!(
        calls,
        [
            "bootout gui/501/com.tesatlas.hub",
            "print gui/501/com.tesatlas.hub",
            "print gui/501/com.tesatlas.hub",
            "bootstrap gui/501 /private/tmp/com.tesatlas.hub.plist",
            "print gui/501/com.tesatlas.hub",
        ]
    );

    // A bootstrap that fails is reported, never followed by another request.
    let mut calls = 0;
    let mut answers = std::collections::VecDeque::from([true, false, false]);
    let mut failing = |_: &[&OsStr]| {
        calls += 1;
        answers.pop_front().ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };
    let refused = reload_installed_with_runner(plist, "gui/501", "gui/501/com.tesatlas.hub", &mut failing).unwrap_err();
    assert_eq!(calls, 3);
    // Unloaded and not loaded again: the owner is told so, and what Try Again does.
    assert_eq!(refused.to_string(), format!("{RELOAD_LEFT_UNLOADED} (launchctl failed)"));
    assert!(refused.to_string().starts_with("the Hub service is not running now"));
    assert!(refused.to_string().contains("Try Again starts it"));
}

#[test]
fn a_job_that_is_not_loaded_is_loaded_without_a_restart() {
    let plist = Path::new("/private/tmp/com.tesatlas.hub.plist");
    let mut calls: Vec<String> = Vec::new();
    // print (absent), bootstrap, print (loaded).
    let mut answers = std::collections::VecDeque::from([false, true, true]);
    let mut runner = |arguments: &[&OsStr]| {
        calls.push(arguments.iter().map(|argument| argument.to_string_lossy()).collect::<Vec<_>>().join(" "));
        answers.pop_front().ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };
    load_installed_with_runner(plist, "gui/501", "gui/501/com.tesatlas.hub", &mut runner).expect("load");
    assert_eq!(
        calls,
        [
            "print gui/501/com.tesatlas.hub",
            "bootstrap gui/501 /private/tmp/com.tesatlas.hub.plist",
            "print gui/501/com.tesatlas.hub",
        ]
    );
    assert!(calls.iter().all(|call| !call.starts_with("kickstart")), "RunAtLoad starts it; no throttled restart");

    // A bootstrap that is refused is reported, with no further request.
    let mut calls = 0;
    let mut answers = std::collections::VecDeque::from([false, false]);
    let mut failing = |_: &[&OsStr]| {
        calls += 1;
        answers.pop_front().ok_or_else(|| io::Error::other("unexpected launchctl call"))
    };
    assert!(load_installed_with_runner(plist, "gui/501", "gui/501/com.tesatlas.hub", &mut failing).is_err());
    assert_eq!(calls, 2);
}

fn takeover_pair(refresh: &str) -> crate::credentials::OwnerTokens {
    crate::credentials::OwnerTokens::from_secret_parts("synthetic-access-0001".to_owned(), refresh.to_owned()).expect("synthetic pair")
}

/// Writes the unit's status row: `state` with `stopped` as its stopped generation.
fn write_auth_stop(store: &HubStore, state: &str, stopped: Option<uuid::Uuid>) {
    let connection = store.open().expect("store");
    connection.execute("DELETE FROM collector_auth_state", []).expect("clear");
    let reason = if state == "api_refused" { "client" } else { "refresh_outcome_unknown" };
    connection
        .execute(
            "INSERT INTO collector_auth_state(singleton_id, state, reason, collecting, retry_at, stopped_generation, stopped_mode, fuse_consecutive, fuse_generation, fuse_last_rejection_at, fuse_pause_until, clock_skew, updated_at_ms) VALUES (1, ?1, ?2, 0, NULL, ?3, ?4, 0, NULL, NULL, NULL, 0, 1)",
            rusqlite::params![state, reason, stopped.map(|generation| generation.to_string()), (state == "api_refused").then_some("legacy")],
        )
        .expect("status row");
}

fn auth_state(store: &HubStore) -> (String, Option<uuid::Uuid>) {
    let row = store.load_collector_auth_row().expect("row").expect("present");
    (row.state, row.stopped_generation)
}

/// M09-T16 (B4): a store whose earlier refresh left a `signed_out` stop, then signed out
/// (cleared) and took over a different unspent pair, passes production start admission.
/// The stale stop is not rewritten by the admission: the unit lifts it when it starts.
#[test]
fn takeover_after_sign_out_passes_production_start_admission() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = seed_selected_car(&data);
    load_or_create_cursor_key(&data).expect("cursor key");
    let mut config = idle_start_config(&data);
    config.collector.interval_seconds = 300;
    let vault = crate::auth::credential_vault::fixtures::legacy_vault(&store, &data);
    let first = vault.store_source_takeover(&takeover_pair("refresh-first")).expect("first").stored_generation().expect("stored");
    write_auth_stop(&store, "signed_out", Some(first));
    let refused = preflight_hub_for_provider(&data, CollectorProvider::Legacy).expect_err("the stop names the stored pair");
    assert!(refused.to_string().contains("credential sign-in required"), "{refused}");
    assert!(preflight_hub_for_serve(&config, None).is_err(), "production serve refused too");

    vault.clear_credential_pair(crate::auth::credential_envelope::CredentialSlot::OwnerApi).expect("sign out");
    let second = vault.store_source_takeover(&takeover_pair("refresh-second")).expect("second").stored_generation().expect("stored");
    assert_ne!(second, first);
    drop(vault);
    preflight_hub_for_provider(&data, CollectorProvider::Legacy).expect("a pair not descended from the stopped one is admitted");
    preflight_hub_for_config(&config).expect("the production configuration preflight");
    preflight_hub_for_serve(&config, None).expect("the production serve admission");
    assert_eq!(auth_state(&store), ("signed_out".to_owned(), Some(first)), "the admission writes nothing");
}

/// M09-T16 (B4): the stop still holds for the pair it names, for a successor the Hub's own
/// refresh derived from it and with a refresh unresolved; an
/// unrelated refusal is neither consulted nor changed.
#[test]
fn sign_out_stop_still_holds_unless_the_pair_was_replaced() {
    let refused = |data: &Path, label: &str| {
        let error = preflight_hub_for_provider(data, CollectorProvider::Legacy).expect_err(label);
        assert!(error.to_string().contains("credential sign-in required"), "{label}: {error}");
    };
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = seed_selected_car(&data);
    load_or_create_cursor_key(&data).expect("cursor key");
    let vault = crate::auth::credential_vault::fixtures::legacy_vault(&store, &data);
    let first = vault.store_source_takeover(&takeover_pair("refresh-first")).expect("first").stored_generation().expect("stored");
    // An unresolved refresh of the stored pair keeps any stop in force.
    write_auth_stop(&store, "signed_out", Some(uuid::Uuid::new_v4()));
    preflight_hub_for_provider(&data, CollectorProvider::Legacy).expect("a stop of an unrelated pair");
    let slot = crate::auth::credential_envelope::CredentialSlot::OwnerApi;
    let receipt = vault.begin_refresh(slot, first).expect("begin");
    refused(&data, "refresh unresolved");
    // The Hub's own refresh completes: the successor descends from the stopped pair.
    let output = uuid::Uuid::new_v4();
    vault.persist_refreshed(slot, receipt, first, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000).expect("successor");
    write_auth_stop(&store, "signed_out", Some(first));
    refused(&data, "successor of the stopped pair");
    write_auth_stop(&store, "signed_out", Some(output));
    refused(&data, "the stopped pair itself");
    // An API refusal is not this check's: admitted, and left as it was.
    write_auth_stop(&store, "api_refused", None);
    preflight_hub_for_provider(&data, CollectorProvider::Legacy).expect("api_refused is not a sign-out");
    assert_eq!(auth_state(&store), ("api_refused".to_owned(), None), "the refusal is untouched");
    drop(vault);
}

/// M09-T16 (B2): a configured Legacy Hub with imported history and no Owner login is
/// admitted for production serve awaiting the owner's sign-in (collector off); once the
/// Hub's own login is stored, the full admission passes and the collector can start.
#[test]
fn awaiting_sign_in_admits_imported_history_without_a_login() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = seed_selected_car(&data);
    load_or_create_cursor_key(&data).expect("cursor key");
    drop(crate::auth::credential_vault::fixtures::legacy_vault(&store, &data));
    let mut config = idle_start_config(&data);
    config.collector.interval_seconds = 300;
    assert!(!hub_is_never_configured(&config).expect("readable"), "not a new Hub");
    assert!(hub_awaits_owner_sign_in(&config).expect("readable"));
    assert!(preflight_hub_for_config(&config).is_err(), "the full admission is unchanged");
    preflight_hub_for_serve(&config, None).expect("served awaiting the sign-in");

    // The owner signs in: the Hub's own login is stored.
    crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(&store, &data, "access-0001", "refresh-0001", None).expect("own login");
    assert!(!hub_awaits_owner_sign_in(&config).expect("readable"), "no longer awaiting");
    preflight_hub_for_config(&config).expect("the full admission passes");
    let status = crate::auth::credential_sign_in::credential_status(&store, &data, 0).expect("status");
    assert!(status.legacy_credentials.present && status.legacy_credentials.store_state == "ok", "the collector's start check holds");
    assert_eq!(status.legacy_credentials.refresh_progress, Some(Some("none_sent")));
}

/// M09-T16 (B2): every other state keeps today's admission.
#[test]
fn awaiting_sign_in_keeps_every_other_refusal() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = seed_selected_car(&data);
    load_or_create_cursor_key(&data).expect("cursor key");
    drop(crate::auth::credential_vault::fixtures::legacy_vault(&store, &data));
    let collecting = || {
        let mut config = idle_start_config(&data);
        config.collector.interval_seconds = 300;
        config
    };
    let refused = |config: &HubConfig, label: &str| {
        assert!(!hub_awaits_owner_sign_in(config).expect("readable"), "{label}: not awaiting");
        assert!(preflight_hub_for_serve(config, None).is_err(), "{label}: refused");
    };
    refused(&idle_start_config(&data), "history-only configuration (collector off)");
    let mut config = collecting();
    config.collector.legacy_auth.enabled = false;
    refused(&config, "Legacy refresh disabled");
    let mut config = collecting();
    config.collector.provider = CollectorProvider::Fleet;
    // A Fleet Hub is not the Legacy state's: it has its own (M09-T19 ruling 3).
    assert!(!hub_awaits_owner_sign_in(&config).expect("readable"), "Fleet provider: not the Legacy state");
    assert_eq!(hub_awaits_fleet_setup(&config).expect("readable"), Some(FleetSetupPending::SignIn), "Fleet provider: the Fleet state");

    // No configured car.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let empty = temporary.path().join("data");
    HubStore::initialize(&empty).expect("empty store");
    let mut config = idle_start_config(&empty);
    config.collector.interval_seconds = 300;
    refused(&config, "no configured car");
    drop(store);
}

/// M09-T16 (N1): an absent key store stays admitted (the sign-in creates it); one that
/// exists but cannot be used is refused at admission, by the serve and the preflight
/// route alike, with the refusal the sign-in's vault would give.
#[test]
fn awaiting_sign_in_refuses_an_existing_unusable_key_store() {
    use std::os::unix::fs::PermissionsExt as _;
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = seed_selected_car(&data);
    load_or_create_cursor_key(&data).expect("cursor key");
    drop(crate::auth::credential_vault::fixtures::legacy_vault(&store, &data));
    let mut config = idle_start_config(&data);
    config.collector.interval_seconds = 300;
    let keyring = data.join("secrets").join(crate::auth::credential_keys::KEYRING_NAME);
    if keyring.exists() {
        fs::remove_file(&keyring).expect("no pair is stored, so the key store may go");
    }
    assert!(hub_awaits_owner_sign_in(&config).expect("an absent key store"), "absent: admitted");
    preflight_hub_for_serve(&config, None).expect("absent: served awaiting the sign-in");

    for (label, mode) in [("malformed", 0o600), ("unreadable", 0o000)] {
        fs::write(&keyring, [0_u8; 10]).expect("key file");
        fs::set_permissions(&keyring, fs::Permissions::from_mode(mode)).expect("key file mode");
        let vault = crate::auth::credential_vault::fixtures::open_admitted_fixture_vault(
            store.clone(),
            &data,
            crate::auth::credential_vault::fixtures::fixture_admission(&store, &data),
        );
        let Err(vault_error) = vault else { panic!("{label}: the vault refuses the key store") };
        let expected = format!("{}: ", vault_error.code());
        assert!(expected.starts_with("keyring_"), "{label}: {expected}");
        let error = hub_awaits_owner_sign_in(&config).expect_err(label);
        assert!(error.to_string().starts_with(&expected), "{label}: {error} (the vault: {expected})");
        let error = preflight_hub_for_serve(&config, None).expect_err(label);
        assert!(error.to_string().starts_with(&expected), "{label}: {error}");
        fs::set_permissions(&keyring, fs::Permissions::from_mode(0o600)).expect("restore mode");
        fs::remove_file(&keyring).expect("reset");
    }
    drop(store);
}

/// Signs the Hub in to the Fleet API through the real grant path: begin, then
/// the redeem against a loopback token double, inside an Open window. The
/// login is the Hub's own (`fleet_oauth`), as the collector loads it.
async fn m09_t19_fleet_sign_in(store: &HubStore, data: &Path) {
    use std::sync::Arc;
    crate::crypto::install_default_provider();
    const ACCESS: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iXX0.sig";
    let router = axum::Router::new().route("/oauth2/v3/token", axum::routing::post(|| async {
        serde_json::json!({"access_token": ACCESS, "refresh_token": "m09-t19-refresh", "expires_in": 28_800, "token_type": "Bearer"}).to_string()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let token = format!("http://{}/oauth2/v3/token", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move { axum::serve(listener, router).await.expect("token double") });
    let window = crate::fleet_api::egress::test_support::TestWindow::open(600);
    let _route = window.route(&token);
    let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store.clone(), data);
    let entry = crate::CredentialVaultEntry::for_test(Arc::clone(&vault));
    let begin = serde_json::json!({"clientId": "m09-t19-client", "region": "eu", "redirectUri": "https://owner.example/fleet/callback"}).to_string();
    let binding = crate::auth::credential_sign_in::begin_fleet_sign_in_with_stub_for_test(&entry, begin.as_bytes(), &token).expect("begin");
    let redeem = serde_json::json!({"code": format!("m09-t19-{}", uuid::Uuid::new_v4()), "redirectUri": "https://owner.example/fleet/callback",
        "clientId": "m09-t19-client", "clientSecret": "m09-t19-secret", "region": "eu", "binding": binding.id().to_string()}).to_string();
    let mut held = Some(binding);
    crate::auth::credential_sign_in::exchange_fleet_code_with_stub_for_test(&entry, &mut held, redeem.as_bytes(), &token)
        .await
        .expect("the Hub's own Fleet login");
    drop(entry);
    drop(vault);
    server.abort();
}

/// M09-T19 ruling 3: an unfinished Fleet set-up is admitted for production
/// serve, collector off, at each step (no login; a login and no car; a
/// login, a car and interval 0), and a signed-out Fleet Hub likewise; the
/// login, a car and a cadence pass the full admission; the state survives a
/// restart because it is read from the store each time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fleet_setup_admits_each_unfinished_step() {
    let fleet = |data: &Path, interval: u64| {
        let mut config = idle_start_config(data);
        config.collector.provider = CollectorProvider::Fleet;
        config.collector.interval_seconds = interval;
        config
    };
    let admitted = |config: &HubConfig, expected: FleetSetupPending, label: &str| {
        assert_eq!(hub_awaits_fleet_setup(config).expect("readable"), Some(expected), "{label}");
        assert!(!hub_awaits_owner_sign_in(config).expect("readable"), "{label}: not the Legacy state");
        preflight_hub_for_serve(config, None).unwrap_or_else(|error| panic!("{label}: served: {error}"));
        assert!(preflight_hub_for_config(config).is_err(), "{label}: the full admission is unchanged");
    };

    // A set-up that stopped after the sign-in: the login, no car.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = HubStore::initialize(&data).expect("store");
    load_or_create_cursor_key(&data).expect("cursor key");
    drop(crate::auth::credential_vault::fixtures::legacy_vault(&store, &data));
    crate::storage::setup_marker::record(&data, store.installation_id().expect("installation")).expect("marker");
    assert!(!hub_is_never_configured(&fleet(&data, 0)).expect("readable"), "set up before");
    admitted(&fleet(&data, 0), FleetSetupPending::SignIn, "no login");
    assert_eq!(FleetSetupPending::SignIn.state(), "awaiting_sign_in");
    m09_t19_fleet_sign_in(&store, &data).await;
    admitted(&fleet(&data, 0), FleetSetupPending::Vehicles, "a login and no car");
    admitted(&fleet(&data, 300), FleetSetupPending::Vehicles, "a login, no car, a cadence");
    assert_eq!(FleetSetupPending::Vehicles.state(), "awaiting_vehicles");
    drop(store);

    // A set-up that found its car: the consent step remains.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = seed_selected_car(&data);
    load_or_create_cursor_key(&data).expect("cursor key");
    m09_t19_fleet_sign_in(&store, &data).await;
    admitted(&fleet(&data, 0), FleetSetupPending::Activation, "a login, a car, interval 0");
    assert_eq!(FleetSetupPending::Activation.state(), "awaiting_activation");
    // Activated: the full admission.
    let active = fleet(&data, 300);
    assert_eq!(hub_awaits_fleet_setup(&active).expect("readable"), None);
    preflight_hub_for_config(&active).expect("the full admission passes");
    preflight_hub_for_serve(&active, None).expect("served collecting");
    // Signed out: the login is cleared; the car and cadence stay.
    let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store.clone(), &data);
    vault.clear_credential_pair(crate::auth::credential_envelope::CredentialSlot::FleetApi).expect("signed out");
    drop(vault);
    admitted(&active, FleetSetupPending::SignIn, "signed out");
    // Not a Fleet set-up: a Legacy provider keeps the Legacy rules.
    let mut legacy = fleet(&data, 300);
    legacy.collector.provider = CollectorProvider::Legacy;
    assert_eq!(hub_awaits_fleet_setup(&legacy).expect("readable"), None);
    drop(store);
}

// M08-T22: the production start's admission reads a number of rows that grows
// with the number of cars, not with the history or the packs.

/// A configured collecting Legacy Hub with imported history and no Owner login.
fn seed_awaiting_hub(data: &Path) -> HubConfig {
    let store = seed_selected_car(data);
    load_or_create_cursor_key(data).expect("cursor key");
    drop(crate::auth::credential_vault::fixtures::legacy_vault(&store, data));
    drop(store);
    let mut config = idle_start_config(data);
    config.collector.interval_seconds = 300;
    config
}

/// Makes every stored pack unreadable: any pack open or hash fails.
fn make_packs_unreadable(data: &Path) -> Vec<PathBuf> {
    let packs: Vec<PathBuf> = fs::read_dir(data.join("packs").join("sha256"))
        .expect("pack folder")
        .map(|entry| entry.expect("pack entry").path())
        .filter(|path| path.is_file())
        .collect();
    assert!(!packs.is_empty(), "the imported car references packs");
    for pack in &packs {
        fs::set_permissions(pack, fs::Permissions::from_mode(0o000)).expect("pack mode");
    }
    packs
}

/// Overwrites the root page of a table no admission reads, so the whole-store
/// `PRAGMA quick_check` fails while every read of the admission still answers.
fn break_unread_table(data: &Path, table: &str) {
    use std::os::unix::fs::FileExt as _;
    let path = data.join("hub.sqlite");
    let connection = rusqlite::Connection::open(&path).expect("store");
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").expect("checkpoint");
    let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0)).expect("page size");
    let root: i64 = connection
        .query_row("SELECT rootpage FROM sqlite_master WHERE type = 'table' AND name = ?1", [table], |row| row.get(0))
        .expect("root page");
    drop(connection);
    let (page_size, root) = (u64::try_from(page_size).expect("page size"), u64::try_from(root).expect("root page"));
    assert!(root > 1, "not the schema page");
    let file = fs::OpenOptions::new().write(true).open(&path).expect("store file");
    file.write_all_at(&vec![0xA5_u8; usize::try_from(page_size).expect("page size")], (root - 1) * page_size).expect("broken page");
}

/// M08-T22: a production start decides the awaiting-sign-in admission without the
/// whole-store check and without opening a pack. Unreadable packs and a damaged page
/// in a table it never reads leave the production admission unchanged, while the
/// whole-store check fails on the same store.
#[test]
fn production_start_admission_reads_neither_the_whole_store_nor_a_pack() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let config = seed_awaiting_hub(&data);
    assert!(hub_awaits_owner_sign_in(&config).expect("healthy store"), "awaiting on the healthy store");

    let packs = make_packs_unreadable(&data);
    let store = HubStore::open_read_only(&data).expect("store");
    assert!(store.catalogue_check().is_err(), "the whole-store check opens the packs");
    assert!(hub_awaits_owner_sign_in(&config).expect("bounded read"), "awaiting");
    preflight_hub_for_serve(&config, None).expect("served awaiting the sign-in without opening a pack");

    break_unread_table(&data, "raw_observations");
    assert!(store.quick_check().is_err(), "the whole-store check sees the damaged page");
    assert!(hub_awaits_owner_sign_in(&config).expect("bounded read"), "awaiting");
    preflight_hub_for_serve(&config, None).expect("served awaiting the sign-in without the whole-store check");
    drop(store);
    for pack in &packs {
        fs::set_permissions(pack, fs::Permissions::from_mode(0o600)).expect("restore mode");
    }
}

/// M08-T32: a production start on an existing data folder creates no directory,
/// so it makes no directory-creation flush, and the catalogue opens before the
/// listener are few: the store opening keeps one connection, and once Serve
/// holds its catalogue the admission's opens all reuse the held shared-memory
/// mapping, none truncating and rebuilding it. Without the hold every one does.
#[test]
fn production_start_admission_opens_the_catalogue_under_one_hold() {
    use crate::platform::file_attempts::directory_syncs_at;
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let config = seed_awaiting_hub(&data);
    let syncs = |path: &Path| directory_syncs_at(path);
    let (parent_syncs, data_syncs, packs_syncs) =
        (syncs(temporary.path()), syncs(&data), syncs(&data.join("packs")));
    let database = data.join("hub.sqlite");

    let lock = crate::hub_user_process::AdmittedUserHub::admit(&data).expect("lifetime lock");
    let opened = crate::db::catalogue_open_counts_at(&database);
    let store = HubStore::initialize(&data).expect("the existing store opens");
    assert_eq!(
        (syncs(temporary.path()), syncs(&data), syncs(&data.join("packs"))),
        (parent_syncs, data_syncs, packs_syncs),
        "no directory-creation flush on an existing data folder"
    );
    let initialised = store.catalogue_open_counts();
    let opening = initialised.opens_without_hold - opened.opens_without_hold;
    let hold = store.hold_catalogue().expect("Serve holds its catalogue");
    let held = store.catalogue_open_counts();
    assert_eq!(held.opens_without_hold - initialised.opens_without_hold, 1, "the hold's own open");

    let admit = |store: &HubStore| {
        store.checkpoint_catalogue_for_immutable_read().expect("checkpoint");
        assert!(!hub_is_never_configured(&config).expect("configured"));
        preflight_hub_for_serve(&config, None).expect("served awaiting the sign-in");
        crate::storage::setup_marker::record(&data, store.installation_id().expect("installation id"))
            .expect("set-up marker");
    };
    admit(&store);
    let admitted = store.catalogue_open_counts();
    let admission_opens = admitted.opens - held.opens;
    assert!(admission_opens > 0, "the admission reads the catalogue");
    assert_eq!(admitted.opens_without_hold, held.opens_without_hold, "every admission open reused the held mapping");
    assert!(opening <= 1, "the store opening rebuilds the shared memory at most once: {opening}");

    drop(hold);
    admit(&store);
    let unheld = store.catalogue_open_counts();
    assert_eq!(
        unheld.opens_without_hold - admitted.opens_without_hold,
        unheld.opens - admitted.opens,
        "without the hold each open is the shared memory's only user"
    );
    drop(lock);
}

/// M08-T22: on a healthy store the bounded admission decides every reviewed state
/// as the whole-store form it replaced did (that form was removed in M08-T25).
#[test]
fn production_start_admission_decides_every_reviewed_state() {
    let same = |config: &HubConfig, label: &str| hub_awaits_owner_sign_in(config).map_err(|error| format!("{label}: {error}"));
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let config = seed_awaiting_hub(&data);
    assert_eq!(same(&config, "awaiting"), Ok(true));
    let mut other = config.clone();
    other.collector.legacy_auth.enabled = false;
    assert_eq!(same(&other, "Legacy refresh disabled"), Ok(false));
    let mut other = config.clone();
    other.collector.provider = CollectorProvider::Fleet;
    assert_eq!(same(&other, "Fleet provider"), Ok(false));
    assert_eq!(same(&idle_start_config(&data), "collector off"), Ok(false));

    // A quarantined car: refused by both, before any credential is read.
    let store = HubStore::open_read_only(&data).expect("store");
    let (vehicle, _, _) = store.configured_tesla_vehicles().expect("cars").into_iter().next().expect("the configured car");
    let connection = rusqlite::Connection::open(data.join("hub.sqlite")).expect("writable");
    let quarantine = |value: i64| {
        connection
            .execute(
                "INSERT INTO vehicle_lifecycle_state(vehicle_id, car_id, last_observation_id, open_session_json, quarantined, updated_at_ms)
                 VALUES (?1, 1, 0, CAST('{}' AS BLOB), ?2, 1)
                 ON CONFLICT(vehicle_id) DO UPDATE SET quarantined = excluded.quarantined",
                rusqlite::params![vehicle.to_string(), value],
            )
            .expect("lifecycle row")
    };
    quarantine(1);
    assert!(same(&config, "quarantined").is_err(), "quarantined: refused");
    quarantine(0);
    assert_eq!(same(&config, "released"), Ok(true));
    drop(connection);

    // The Hub's own login is stored.
    let store = HubStore::initialize(&data).expect("writable store");
    crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(&store, &data, "access-0001", "refresh-0001", None).expect("own login");
    assert_eq!(same(&config, "signed in"), Ok(false));

    // No configured car.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let empty = temporary.path().join("data");
    HubStore::initialize(&empty).expect("empty store");
    let mut config = idle_start_config(&empty);
    config.collector.interval_seconds = 300;
    assert_eq!(same(&config, "no configured car"), Ok(false));
}

/// M08-T22 with M09-T19 ruling 3: a production start decides the Fleet waiting
/// states on the same bounded read as the Legacy one. A Fleet Hub with imported
/// history and no Fleet login is served awaiting its sign-in with unreadable
/// packs and a damaged page in a table no admission reads, and Start's
/// `preflight` answers it the same way (M08-T25), while the activation form
/// (`setup-fleet activate`, the whole-store check) refuses the same store.
#[test]
fn production_start_fleet_admission_reads_neither_the_whole_store_nor_a_pack() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let mut config = seed_awaiting_hub(&data);
    config.collector.provider = CollectorProvider::Fleet;
    assert_eq!(hub_awaits_fleet_setup(&config).expect("healthy store"), Some(FleetSetupPending::SignIn));
    assert_eq!(
        awaits_fleet_setup(&config, AdmissionStoreCheck::Bounded).expect("bounded read"),
        Some(FleetSetupPending::SignIn),
        "the bounded form decides alike on a healthy store"
    );

    let packs = make_packs_unreadable(&data);
    assert!(hub_awaits_fleet_setup(&config).is_err(), "the activation form keeps the whole-store check");
    assert_eq!(awaits_fleet_setup(&config, AdmissionStoreCheck::Bounded).expect("bounded read"), Some(FleetSetupPending::SignIn));
    preflight_hub_for_serve(&config, None).expect("served awaiting the Fleet sign-in without opening a pack");

    break_unread_table(&data, "raw_observations");
    assert_eq!(awaits_fleet_setup(&config, AdmissionStoreCheck::Bounded).expect("bounded read"), Some(FleetSetupPending::SignIn));
    preflight_hub_for_serve(&config, None).expect("served awaiting the Fleet sign-in without the whole-store check");
    let answer = start(&config).expect("Start answers on the bounded read");
    assert_eq!(answer["state"], "awaiting_sign_in");
    assert_eq!(answer["provider"], "fleet");
    assert_eq!(answer["collectorEnabled"], false);
    for pack in &packs {
        fs::set_permissions(pack, fs::Permissions::from_mode(0o600)).expect("restore mode");
    }
}

// M08-T25: the command-line `preflight` the Mac app's Start runs, its answers
// for every state the app shows, and its reads.

/// `preflight` as the app's Start runs it: the function the command-line verb
/// runs, with its set-up marker check and its reader. Every call also shows
/// that it opened no pack and read no whole catalogue (M08-T25 repair).
fn start(config: &HubConfig) -> Result<serde_json::Value, String> {
    crate::db::read_probe::take();
    let answer = start_preflight(config).map_err(|error| error.to_string());
    assert_eq!(crate::db::read_probe::take(), (0, 0), "Start opened no pack and read no whole catalogue ({answer:?})");
    answer
}

/// Whether the `doctor` admits the selected provider's stored pair: the
/// credential decision `preflight` gives for a configured Hub.
fn doctor_admits(config: &HubConfig) -> bool {
    let store = HubStore::open_read_only(&config.data_dir).expect("store");
    let report = crate::diagnostics::inspect_hub(&store, config).expect("doctor");
    let present = match config.collector.provider {
        CollectorProvider::Legacy => report.credentials.legacy.present,
        CollectorProvider::Fleet => report.credentials.fleet.present,
    };
    present && report.checks.iter().any(|check| check.name == "selectedProviderCredentials" && check.passed)
}

/// The refusal `preflight` gives when the selected pair is absent or not admitted.
fn credential_refusal(config: &HubConfig) -> String {
    let store = HubStore::open_read_only(&config.data_dir).expect("store");
    let status = crate::auth::credential_sign_in::credential_status(&store, &config.data_dir, preflight_clock_ms().expect("clock")).expect("status");
    crate::auth::credential_sign_in::diagnostic_admission_error(&status, config.collector.provider == CollectorProvider::Fleet).to_string()
}

/// A configured collecting Legacy Hub with the Hub's own login stored.
fn seed_signed_in_hub(data: &Path) -> HubConfig {
    let config = seed_awaiting_hub(data);
    let store = HubStore::initialize(data).expect("writable store");
    crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(&store, data, "access-0001", "refresh-0001", None).expect("own login");
    config
}

/// M08-T25: `preflight`'s answer and refusal for every state the app shows. The
/// credential decision for a configured Hub is the `doctor`'s.
#[test]
fn start_preflight_answers_every_state_the_app_shows() {
    let provider = |provider| serde_json::to_value(provider).expect("provider");
    let ready = serde_json::json!({"status": "ready", "version": crate::BUILD_VERSION, "provider": provider(CollectorProvider::Legacy)});

    // Never configured: idle, the store is not created.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let fresh = temporary.path().join("fresh");
    fs::create_dir(&fresh).expect("data folder");
    assert_eq!(
        start(&idle_start_config(&fresh)),
        Ok(serde_json::json!({"status": "ready", "state": "idle", "version": crate::BUILD_VERSION, "provider": provider(CollectorProvider::Legacy), "collectorEnabled": false})),
    );
    assert!(!fresh.join("hub.sqlite").exists(), "idle creates no store");

    // Imported history and no login: the answer the imported-history sign-in requires.
    let data = temporary.path().join("data");
    let config = seed_awaiting_hub(&data);
    let awaiting = serde_json::json!({"status": "ready", "state": "awaiting_sign_in", "version": crate::BUILD_VERSION, "provider": provider(CollectorProvider::Legacy), "collectorEnabled": false});
    assert_eq!(start(&config), Ok(awaiting.clone()), "awaiting the owner's sign-in");

    // No login and Legacy refresh disabled: refused with the credential status.
    let mut disabled = config.clone();
    disabled.collector.legacy_auth.enabled = false;
    assert!(!doctor_admits(&disabled));
    assert_eq!(start(&disabled), Err(credential_refusal(&disabled)), "Legacy refresh disabled");

    // A quarantined car: refused before any credential is read.
    let store = HubStore::open_read_only(&data).expect("store");
    let (vehicle, _, _) = store.configured_tesla_vehicles().expect("cars").into_iter().next().expect("the configured car");
    let connection = rusqlite::Connection::open(data.join("hub.sqlite")).expect("writable");
    let quarantine = |value: i64| {
        connection
            .execute(
                "INSERT INTO vehicle_lifecycle_state(vehicle_id, car_id, last_observation_id, open_session_json, quarantined, updated_at_ms)
                 VALUES (?1, 1, 0, CAST('{}' AS BLOB), ?2, 1)
                 ON CONFLICT(vehicle_id) DO UPDATE SET quarantined = excluded.quarantined",
                rusqlite::params![vehicle.to_string(), value],
            )
            .expect("lifecycle row")
    };
    quarantine(1);
    let unavailable = format!("Hub data is unavailable: {}", store.readiness_check().expect_err("quarantined"));
    assert_eq!(start(&config), Err(unavailable.clone()), "quarantined, awaiting");

    // The Hub's own login is stored: ready, and still refused while a car is quarantined.
    let writable = HubStore::initialize(&data).expect("writable store");
    crate::auth::credential_vault::fixtures::replace_legacy_supplied_pair(&writable, &data, "access-0001", "refresh-0001", None).expect("own login");
    assert_eq!(start(&config), Err(unavailable), "quarantined, signed in");
    quarantine(0);
    drop(connection);
    assert!(doctor_admits(&config));
    assert_eq!(start(&config), Ok(ready.clone()), "signed in");

    // Tesla refused the login: attention, not a refusal.
    write_auth_stop(&writable, "api_refused", None);
    assert!(doctor_admits(&config));
    assert_eq!(start(&config), Ok(ready.clone()), "Tesla refused the login");

    // The Hub's collection stopped on the stored login: refused, needs a sign-in.
    let vault = crate::auth::credential_vault::fixtures::legacy_vault(&writable, &data);
    let slot = crate::auth::credential_envelope::CredentialSlot::OwnerApi;
    let generation = vault.stored_generation(slot).expect("generation").expect("pair");
    write_auth_stop(&writable, "signed_out", Some(generation));
    let doctor = if doctor_admits(&config) { Ok(ready.clone()) } else { Err(credential_refusal(&config)) };
    assert_eq!(start(&config), doctor, "stopped on the stored login");

    // A refresh whose outcome is unknown: refused.
    write_auth_stop(&writable, "api_refused", None);
    vault.begin_refresh(slot, generation).expect("unresolved refresh");
    assert!(!doctor_admits(&config));
    assert_eq!(start(&config), Err(credential_refusal(&config)), "refresh outcome unknown");
    drop(vault);
    drop(writable);
    drop(store);

    // A Fleet Hub (M09-T19a merge): the Hub's own Fleet grant, a car and a
    // cadence are admitted as the doctor admits them (the Fleet line collects
    // with Fleet, M09-T19); a pair that is not the Hub's own grant is still
    // refused as main refuses a stored Fleet pair (M02-T02).
    let fleet = temporary.path().join("fleet");
    seed_fleet_ready_hub(&fleet);
    let mut config = idle_start_config(&fleet);
    config.collector.provider = CollectorProvider::Fleet;
    config.collector.interval_seconds = 30;
    assert!(doctor_admits(&config));
    let fleet_ready = serde_json::json!({"status": "ready", "version": crate::BUILD_VERSION, "provider": provider(CollectorProvider::Fleet)});
    assert_eq!(start(&config), Ok(fleet_ready), "Fleet, the Hub's own grant");
    let supplied = temporary.path().join("fleet-supplied");
    drop(seed_selected_car(&supplied));
    load_or_create_cursor_key(&supplied).expect("cursor key");
    crate::auth::credential_vault::fixtures::seed_blocked_fleet_setup_credentials(
        &supplied,
        &FleetSetupCredentials::new(
            "e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iXX0.sig".to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            3_600,
        )
        .expect("credentials"),
        SystemTime::now(),
    );
    let mut config = idle_start_config(&supplied);
    config.collector.provider = CollectorProvider::Fleet;
    config.collector.interval_seconds = 30;
    assert!(!doctor_admits(&config));
    let refusal = credential_refusal(&config);
    assert!(refusal.contains("source_credential_disabled"), "{refusal}");
    assert_eq!(start(&config), Err(refusal), "Fleet, a pair that is not the Hub's own");

    // No configured car.
    let empty = temporary.path().join("empty");
    HubStore::initialize(&empty).expect("empty store");
    let mut config = idle_start_config(&empty);
    config.collector.interval_seconds = 300;
    assert_eq!(start(&config), Err("at least one configured vehicle is required".to_owned()), "no configured car");
}

/// M08-T25: a Hub awaiting the owner's sign-in. Every pack unreadable, then the
/// root page of `raw_observations` overwritten: the whole-store check fails on
/// the store, and Start's `preflight` still answers `awaiting_sign_in`.
#[test]
fn start_preflight_awaiting_sign_in_reads_neither_the_whole_store_nor_a_pack() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let config = seed_awaiting_hub(&data);
    let awaiting = start(&config).expect("healthy store");
    assert_eq!(awaiting["state"], "awaiting_sign_in");

    let packs = make_packs_unreadable(&data);
    let store = HubStore::open_read_only(&data).expect("store");
    assert!(store.catalogue_check().is_err(), "the whole-store check opens the packs");
    let (packs_opened, whole_reads) = crate::db::read_probe::take();
    assert!(packs_opened > 0 && whole_reads > 0, "the probe counts the whole-store check's reads");
    assert_eq!(start(&config), Ok(awaiting.clone()), "no pack is opened");

    break_unread_table(&data, "raw_observations");
    assert!(store.quick_check().is_err(), "the whole-store check sees the damaged page");
    assert_eq!(start(&config), Ok(awaiting), "no whole-store check");
    drop(store);
    for pack in &packs {
        fs::set_permissions(pack, fs::Permissions::from_mode(0o600)).expect("restore mode");
    }
}

// M09-T11 Fleet cases (G-FLEET-PREFLIGHT-WITNESS closed). The non-decrypting
// preflight's scope witness is the stored pair's clear origin header: only the
// Hub's own Fleet grant is admitted, and the sign-in stores a grant only after
// its claims carry the collection scopes (M09-T19 ruling 6); the claims
// themselves are read at each collector start, not here.
#[test]
fn fleet_preflight_rejects_a_pair_without_the_recorded_scope_witness() {
    const UNSCOPED_ACCESS: &str = "e30.eyJzY3AiOlsib3BlbmlkIl19.sig";
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data_dir = temporary.path().join("state");
    let store = seed_selected_car(&data_dir);
    load_or_create_cursor_key(&data_dir).expect("cursor key");
    // An unscoped pair stored before the Hub's first own Fleet grant (a
    // supplied pair an older Hub held): it carries no scope witness.
    crate::auth::credential_vault::fixtures::seed_blocked_fleet_setup_credentials(
        &data_dir,
        &FleetSetupCredentials::new(
            UNSCOPED_ACCESS.to_owned(),
            "fleet-refresh-two".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            3_600,
        )
        .expect("credentials"),
        SystemTime::now(),
    );
    let mut config = source_run_config(temporary.path(), &data_dir);
    config.collector.provider = CollectorProvider::Fleet;
    config.collector.interval_seconds = 30;
    let before = preflight_snapshot(&store, &data_dir);
    let refused = preflight_hub_for_provider(&data_dir, CollectorProvider::Fleet)
        .expect_err("no recorded scope witness");
    assert!(refused.to_string().contains("source_credential_disabled"), "{refused}");
    assert!(preflight_hub_for_config(&config).is_err());
    assert!(preflight_hub_for_serve(&config, None).is_err());
    assert!(preflight_hub(&data_dir).is_err());
    assert_eq!(preflight_snapshot(&store, &data_dir), before, "no write");
}

/// The Fleet configuration with `interval` used by the M09-T11 B3 cases.
fn m09_t11_fleet_config(data: &Path, interval: u64) -> HubConfig {
    let mut config = idle_start_config(data);
    config.collector.provider = CollectorProvider::Fleet;
    config.collector.interval_seconds = interval;
    config
}

/// M09-T11 B3, beside the older-row case above: a Fleet grant without
/// `vehicle_location` is refused by the sign-in before anything is stored, so
/// the provider preflight and the full admission refuse, and the admission
/// reports the sign-in step (`awaiting_sign_in`), the service running with
/// the collector off. Nothing is written by the preflights.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fleet_admission_refuses_a_grant_without_the_collection_scopes() {
    const NARROWED: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig";
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = seed_selected_car(&data);
    load_or_create_cursor_key(&data).expect("cursor key");
    assert!(
        !crate::auth::credential_vault::fixtures::fleet_sign_in_answered_with(&store, &data, NARROWED).await,
        "the narrowed grant is refused"
    );
    let config = m09_t11_fleet_config(&data, 300);
    let before = preflight_snapshot(&store, &data);
    assert!(preflight_hub_for_provider(&data, CollectorProvider::Fleet).is_err(), "no login is admitted");
    assert!(preflight_hub_for_config(&config).is_err(), "the full admission refuses");
    assert_eq!(hub_awaits_fleet_setup(&config).expect("readable"), Some(FleetSetupPending::SignIn), "sign in again");
    preflight_hub_for_serve(&config, None).expect("served awaiting the sign-in, collector off");
    assert_eq!(preflight_snapshot(&store, &data), before, "no write");
}

/// M09-T11 B1 and B3: the Hub's own scoped grant whose refresh comes back
/// without `vehicle_location`. The successor is refused before the commit, so
/// the preflight reads the earlier, scoped generation with its unresolved
/// refresh and admits only the reconnect stop (the collector starts fenced
/// and sends nothing), while the operational check refuses with the unknown
/// outcome (sign in again). Nothing is written by the preflights.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fleet_preflight_after_a_narrowed_refresh_admits_only_the_reconnect_stop() {
    const NARROWED: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig";
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let store = seed_selected_car(&data);
    load_or_create_cursor_key(&data).expect("cursor key");
    m09_t19_fleet_sign_in(&store, &data).await;
    let config = m09_t11_fleet_config(&data, 300);
    preflight_hub_for_config(&config).expect("the full admission of the scoped grant");
    let probe = crate::auth::credential_vault::fixtures::refresh_fleet_pair_answered_with(&store, &data, NARROWED).await;
    assert!(
        matches!(probe.first, Err(crate::fleet_credentials::FleetCredentialError::MissingCollectionScopes)),
        "{:?}",
        probe.first
    );
    assert_eq!(probe.requests, 1);
    let before = preflight_snapshot(&store, &data);
    assert!(
        crate::fleet_credentials::inspect_stored_fleet_credentials(&store, &data)
            .expect("the earlier grant")
            .unresolved_refresh
    );
    preflight_hub_for_provider(&data, CollectorProvider::Fleet).expect("the reconnect stop is admitted");
    preflight_hub_for_config(&config).expect("the reconnect stop is admitted");
    assert!(matches!(
        crate::auth::credential_vault::fixtures::validate_fixture_fleet_credentials(&store, &data),
        Err(crate::fleet_credentials::FleetCredentialError::RotationOutcomeUnknown)
    ));
    assert_eq!(preflight_snapshot(&store, &data), before, "no write");
}

/// M08-T25: a Hub with the Hub's own login stored, on the same damaged store:
/// Start's `preflight` still answers ready.
#[test]
fn start_preflight_signed_in_reads_neither_the_whole_store_nor_a_pack() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let config = seed_signed_in_hub(&data);
    let ready = start(&config).expect("healthy store");
    assert_eq!(ready["status"], "ready");
    assert!(ready.get("state").is_none(), "the credentials-present answer");

    let packs = make_packs_unreadable(&data);
    let store = HubStore::open_read_only(&data).expect("store");
    assert!(store.catalogue_check().is_err(), "the whole-store check opens the packs");
    let (packs_opened, whole_reads) = crate::db::read_probe::take();
    assert!(packs_opened > 0 && whole_reads > 0, "the probe counts the whole-store check's reads");
    assert_eq!(start(&config), Ok(ready.clone()), "no pack is opened");

    break_unread_table(&data, "raw_observations");
    assert!(store.quick_check().is_err(), "the whole-store check sees the damaged page");
    assert_eq!(start(&config), Ok(ready), "no whole-store check");
    drop(store);
    for pack in &packs {
        fs::set_permissions(pack, fs::Permissions::from_mode(0o600)).expect("restore mode");
    }
}

// M08-T25 repair: Start's `preflight` through the verb's own function, its
// bounded reads on every branch, and the refusals the service's start has.

/// A Hub never configured whose existing catalogue fails the whole-store check
/// (the root page of `raw_observations` overwritten): the idle answer comes
/// from bounded reads only, and the probe sees the old reads.
#[test]
fn start_preflight_idle_with_a_catalogue_reads_neither_the_whole_store_nor_a_pack() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    drop(HubStore::initialize(&data).expect("the empty store serve creates"));
    let config = idle_start_config(&data);
    let idle = start(&config).expect("idle on the healthy empty store");
    assert_eq!(idle["state"], "idle");

    break_unread_table(&data, "raw_observations");
    let store = HubStore::open_read_only(&data).expect("store");
    assert!(store.catalogue_check().is_err(), "the whole-store check sees the damaged page");
    assert!(crate::db::read_probe::take().1 > 0, "the probe counts the whole-store check");
    assert!(HubStore::open_immutable_read_only(&data).is_ok(), "the immutable snapshot opens");
    assert_eq!(crate::db::read_probe::take().1, 1, "the probe counts the immutable snapshot's hash of the whole file");
    assert_eq!(start(&config), Ok(idle), "idle without the whole-store check");
}

/// The stored credential states the first round pinned only through the
/// admission rule, now through Start: a pending and a blocked Owner slot, and
/// a key store fault with a login present. Each is refused with the doctor's
/// refusal.
#[test]
fn start_preflight_refuses_pending_blocked_and_key_store_faults() {
    let slot_state = |config: &HubConfig| {
        let store = HubStore::open_read_only(&config.data_dir).expect("store");
        crate::auth::credential_sign_in::credential_status(&store, &config.data_dir, preflight_clock_ms().expect("clock"))
            .expect("status")
            .legacy_credentials
            .store_state
    };
    let temporary = crate::private_tempdir().expect("temporary directory");

    // A pending Owner slot: the stored pair is not converted yet.
    let pending = temporary.path().join("pending");
    let config = {
        let store = seed_selected_car(&pending);
        drop(store);
        crate::CredentialVaultEntry::seed_migration_for_test(&pending, 0, false, false).expect("pending Owner slot");
        let mut config = idle_start_config(&pending);
        config.collector.interval_seconds = 300;
        config
    };
    assert!(!hub_awaits_owner_sign_in(&config).expect("readable"), "a pending slot does not await the sign-in");
    assert!(!doctor_admits(&config));
    assert_eq!(slot_state(&config), "migration_pending");
    assert_eq!(start(&config), Err(credential_refusal(&config)), "pending Owner slot");

    // The same slot blocked: its old key is unavailable.
    rusqlite::Connection::open(pending.join("hub.sqlite"))
        .expect("writable")
        .execute("UPDATE credential_format_state SET owner_api_blocked = 'old_key_unavailable' WHERE singleton_id = 1", [])
        .expect("blocked Owner slot");
    assert!(!hub_awaits_owner_sign_in(&config).expect("readable"), "a blocked slot does not await the sign-in");
    assert!(!doctor_admits(&config));
    assert_eq!(slot_state(&config), "source_credential_disabled");
    assert_eq!(start(&config), Err(credential_refusal(&config)), "blocked Owner slot");

    // The Hub's own login is stored and its key store is damaged.
    let signed_in = temporary.path().join("signed-in");
    let config = seed_signed_in_hub(&signed_in);
    assert_eq!(start(&config).expect("signed in")["status"], "ready");
    let keyring = signed_in.join("secrets").join(crate::auth::credential_keys::KEYRING_NAME);
    fs::write(&keyring, [0_u8; 10]).expect("damaged key file");
    assert!(!doctor_admits(&config));
    let store = HubStore::open_read_only(&signed_in).expect("store");
    let status = crate::auth::credential_sign_in::credential_status(&store, &signed_in, preflight_clock_ms().expect("clock")).expect("status");
    assert!(status.legacy_credentials.present && status.credential_store.keyring != "ok", "a login with a damaged key store");
    let refusal = credential_refusal(&config);
    assert!(refusal.starts_with(&format!("{}: ", status.credential_store.keyring)), "{refusal}");
    assert_eq!(start(&config), Err(refusal), "key store fault with a login present");
}

/// A clock before 1970 refuses Start in both branches with the words the
/// whole-store check gave: "Hub data is unavailable: ..." where the awaiting
/// rule reads the catalogue (any configured collecting Legacy Hub), and the
/// clock's own refusal for a Hub that reaches the credentials-present branch
/// directly (a Fleet Hub).
#[test]
fn start_preflight_refuses_a_clock_before_1970_in_both_branches() {
    struct Clock;
    impl Drop for Clock {
        fn drop(&mut self) {
            PREFLIGHT_CLOCK.with(|clock| clock.set(None));
        }
    }
    let before_1970 = std::time::UNIX_EPOCH - Duration::from_secs(1);
    let clock_refusal = crate::db::StoreError::RetiredLineageClock(
        before_1970.duration_since(std::time::UNIX_EPOCH).expect_err("before 1970"),
    )
    .to_string();
    assert!(clock_refusal.starts_with("cannot read the store clock for retired-lineage pack retention: "), "{clock_refusal}");
    let unavailable = format!("Hub data is unavailable: {clock_refusal}");
    let temporary = crate::private_tempdir().expect("temporary directory");

    let awaiting = seed_awaiting_hub(&temporary.path().join("awaiting"));
    let signed_in = seed_signed_in_hub(&temporary.path().join("signed-in"));
    let fleet_data = temporary.path().join("fleet");
    seed_fleet_ready_hub(&fleet_data);
    let mut fleet = idle_start_config(&fleet_data);
    fleet.collector.provider = CollectorProvider::Fleet;
    fleet.collector.interval_seconds = 30;
    assert_eq!(start(&awaiting).expect("awaiting")["state"], "awaiting_sign_in");
    assert_eq!(start(&signed_in).expect("signed in")["status"], "ready");

    let _clock = Clock;
    PREFLIGHT_CLOCK.with(|clock| clock.set(Some(before_1970)));
    assert_eq!(start(&awaiting), Err(unavailable.clone()), "awaiting the sign-in");
    assert_eq!(start(&signed_in), Err(unavailable), "signed in");
    assert_eq!(start(&fleet), Err(clock_refusal), "credentials present, Fleet");
}

/// The pack staging folder the service's start sweeps before its listener
/// opens: one that is not the Hub's own private folder stops that start, and
/// Start refuses it in both branches, with the old words where the old
/// `preflight` could not list it either. A folder of the Hub's own that cannot
/// be listed is made private by the start, so it passes.
#[test]
fn start_preflight_refuses_a_staging_folder_the_start_stops_on() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let awaiting = seed_awaiting_hub(&temporary.path().join("awaiting"));
    let signed_in = seed_signed_in_hub(&temporary.path().join("signed-in"));
    for config in [&awaiting, &signed_in] {
        let answer = start(config).expect("healthy");
        let staging = config.data_dir.join("packs").join(".staging");
        fs::create_dir_all(&staging).expect("staging folder");
        let mode = |mode: u32| fs::set_permissions(&staging, fs::Permissions::from_mode(mode)).expect("staging mode");

        mode(0o770);
        let unsafe_staging = crate::db::StoreError::PackStartupRepair(crate::hub_pack::ProjectionPackError::UnsafeStaging(staging.clone())).to_string();
        assert_eq!(start(config), Err(unsafe_staging), "group-writable, listable");
        assert!(HubStore::initialize(&config.data_dir).is_err(), "the service's start stops on it");

        mode(0o020);
        let refusal = start(config).expect_err("group-writable, not listable");
        assert!(refusal.starts_with(&format!("cannot inspect catalogue pack {}", staging.display())), "{refusal}");

        mode(0o000);
        assert_eq!(start(config), Ok(answer.clone()), "the Hub's own folder, not listable: the start makes it private");
        drop(HubStore::initialize(&config.data_dir).expect("the service's start repairs it"));
        assert_eq!(fs::metadata(&staging).expect("staging").permissions().mode() & 0o777, 0o700);
        assert_eq!(start(config), Ok(answer), "repaired");
    }
}

/// The import spool the service's start recovers before its listener opens:
/// one that is not private stops that start, and Start refuses it in both
/// branches with the start's own words.
#[test]
fn start_preflight_refuses_an_import_spool_the_start_stops_on() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let awaiting = seed_awaiting_hub(&temporary.path().join("awaiting"));
    let signed_in = seed_signed_in_hub(&temporary.path().join("signed-in"));
    for config in [&awaiting, &signed_in] {
        let answer = start(config).expect("healthy");
        let spool = config.data_dir.join("import-spool");
        fs::create_dir(&spool).expect("import spool");
        fs::set_permissions(&spool, fs::Permissions::from_mode(0o700)).expect("private");
        assert_eq!(start(config), Ok(answer.clone()), "a private spool");

        fs::set_permissions(&spool, fs::Permissions::from_mode(0o755)).expect("not private");
        let refusal = crate::db::StoreError::UnsafeImportSpool(spool.clone()).to_string();
        assert_eq!(start(config), Err(refusal), "a spool that is not private");
        assert!(
            matches!(HubStore::initialize(&config.data_dir), Err(crate::db::StoreError::UnsafeImportSpool(path)) if path == spool),
            "the service's start stops on it"
        );
        fs::remove_dir(&spool).expect("reset");
        assert_eq!(start(config), Ok(answer), "no spool");
    }
}
