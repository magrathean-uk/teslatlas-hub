// SPDX-License-Identifier: AGPL-3.0-only
#![cfg(target_os = "macos")]

use super::super::{Cli, Command, macos_migration_start_request};
use clap::error::ErrorKind;

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

/// M02-T21 (DEC-115): the service's own collector slot under a supervisor on
/// an Open window. Never configured, the Hub passes the serve admission idle
/// and the slot refuses inside the window, so nothing is collected. After the
/// sign-in's stored result (the Hub's own pair, through the test double) and
/// the configuration (an imported car and a cadence), the restart passes the
/// full admission and its slot starts the collector inside that same window.
/// The window keeps a gate of its own: the process-wide egress gate that
/// every Tesla send asks stays closed, so nothing leaves the process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_start_collection_begins_only_after_sign_in_inside_the_window() {
    use std::{os::unix::fs::PermissionsExt as _, sync::Arc};
    use teslatlas_hub::{
        collection::window::{
            CollectorSlot as _, EgressGate, IssueRequest, Issuer, ManualClock, ManualProbe,
            MemoryAnchor, NoHighWater, Phase, Supervisor, WindowClock as _, WindowConfig,
            WindowController, WindowDeps,
        },
        config::HubConfig,
        db::HubStore,
        hub_pack::ProjectionCarSettings,
        hub_user_process::AdmittedUserHub,
        legacy_owner_credentials::load_or_create_cursor_key,
        macos_launch_agent::{hub_is_never_configured, preflight_hub_for_serve},
        runtime::operator::authority::RecoveryGate,
        source_db_import::{SourceDbImportRequest, SourceDbImportScope, publish_history},
        source_db_projection::{SourceDbCar, SourceDbHistory},
    };
    use super::super::ServiceCollectorSlot;

    const HOST: &str = "host-idle-start";
    assert!(!EgressGate::process_wide().is_open(), "no test opens the process gate");
    let root = tempfile::tempdir().expect("root");
    let data = root.path().join("data");
    let scratch = root.path().join("scratch");
    for dir in [&data, &scratch] {
        std::fs::create_dir(dir).expect("dir");
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).expect("private dir");
    }
    let config = |collector: &str| {
        let path = root.path().join("config.toml");
        std::fs::write(
            &path,
            format!("data_dir = {data:?}\nbind = \"127.0.0.1:8080\"\n{collector}\n[geocoder]\nenabled = false\n\n[terrain]\nenabled = false\n"),
        )
        .expect("config");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("config mode");
        HubConfig::load(&path).expect("config loads")
    };

    // An Open window on the service's own controller, with a gate of its own.
    let clock = Arc::new(ManualClock::new(1_800_000_000));
    let anchor = Arc::new(MemoryAnchor::new());
    let issuer = Issuer::new(&scratch.join("issuer"), "host-idle-issuer", anchor.clone(), clock.clone());
    let key = scratch.join("issuer.key");
    let trust = issuer.init(&key).expect("issuer");
    let (controller, _) = WindowController::start(
        WindowConfig {
            dir: data.join("window"),
            claim_path: scratch.join("claim/host-claim.lock"),
            host_id: HOST.to_owned(),
        },
        WindowDeps {
            clock: clock.clone(),
            anchor,
            probe: Arc::new(ManualProbe::new(0.1)),
            gate: EgressGate::new(clock.clone()),
            handback: Arc::new(NoHighWater),
        },
    );
    let controller = Arc::new(controller);
    controller.trust(&trust).expect("trusted");
    let now = clock.wall();
    let grant = scratch.join("grant.json");
    issuer
        .issue(
            &key,
            &IssueRequest {
                host_id: HOST.to_owned(),
                starts_at: now,
                expires_at: now + 600,
                approver: "G".to_owned(),
                reason: "idle start test".to_owned(),
                approval_ref: "approval-idle-start".to_owned(),
            },
            &grant,
        )
        .expect("grant");
    let opened = controller.open(&std::fs::read(&grant).expect("grant bytes")).expect("window opens");
    assert_eq!(opened.phase, Phase::Open);
    let deadline = now + 600;

    // Idle: admitted, and nothing starts inside the Open window.
    let idle = config("");
    let store = HubStore::initialize(&data).expect("the store serve creates");
    assert!(hub_is_never_configured(&idle).expect("idle state"));
    preflight_hub_for_serve(&idle, None).expect("idle admission");
    {
        let admission = AdmittedUserHub::admit(&data).expect("service admission");
        let slot = ServiceCollectorSlot::new(
            tokio::runtime::Handle::current(),
            store.clone(),
            idle.clone(),
            Arc::clone(&admission),
            RecoveryGate::new(false),
        );
        let mut probe = slot.clone();
        assert_eq!(probe.start(deadline).unwrap_err(), "collector_not_ready");
        let mut supervisor = Supervisor::new(Arc::clone(&controller), slot.clone());
        assert!(!supervisor.step().collector_running, "idle collects nothing in an Open window");
        assert!(!probe.is_running());
    }

    // Sign in and configure through the doubles, with the service stopped.
    teslatlas_hub::auth::credential_sign_in::seed_owner_pair_for_test(&data, "idle-start-access-0002", "idle-start-refresh-0002", None)
        .expect("the Hub's own pair");
    assert!(teslatlas_hub::auth::credential_sign_in::record_provider_admission_for_test(&data).expect("provider admission"));
    let history = SourceDbHistory {
        cars: vec![SourceDbCar {
            id: 1,
            eid: 5_300_003,
            vid: Some(5_400_003),
            vin: Some("5YJ3E1EA7KF530003".to_owned()),
            name: Some("Idle start window".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: ProjectionCarSettings { enabled: true, use_streaming_api: false, ..Default::default() },
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
        &load_or_create_cursor_key(&data).expect("cursor key"),
        &SourceDbImportRequest {
            new_import_source: false,
            source_key: "idle-start-window".to_owned(),
            scope: SourceDbImportScope::Selected(1),
            imported_at_ms: (now - 60) * 1000,
        },
        &history,
    )
    .expect("configured car");
    let configured = config("\n[collector]\ninterval_seconds = 30\n");
    assert!(!hub_is_never_configured(&configured).expect("configured state"));
    preflight_hub_for_serve(&configured, None).expect("the restart passes the full admission");

    // The restart's slot starts the collector inside the same window.
    let admission = AdmittedUserHub::admit(&data).expect("service admission");
    let slot = ServiceCollectorSlot::new(
        tokio::runtime::Handle::current(),
        store.clone(),
        configured,
        Arc::clone(&admission),
        RecoveryGate::new(false),
    );
    let mut started = slot.clone();
    assert_eq!(started.start(deadline), Ok(()), "collection begins inside the window");
    started.stop();
    let stopping = std::time::Instant::now();
    while started.is_running() {
        assert!(stopping.elapsed() < std::time::Duration::from_secs(60), "collector stops");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    started.release_lease().expect("lease released");
    assert!(!EgressGate::process_wide().is_open(), "nothing could leave the process");
}
