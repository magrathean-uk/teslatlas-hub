// SPDX-License-Identifier: AGPL-3.0-only

// Spec test CT33 of C3 part K4: the supervised Owner loop, run end to end against
// the in-process fake Tesla source through its canonical scenario with a restart,
// never wakes the car. The fake audits every request it receives; the test reads
// that audit, so the proof is the fake's, not the collector's. Synthetic data only;
// the fake binds loopback and no test touches the network.

/// A selected imported car for the fake source's fixture vehicle, with its stream
/// switched off in its settings (the journey proves the REST path; the stream's own
/// gate is C4's).
fn seed_journey_import(store: &HubStore) {
    use crate::teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history};
    use crate::teslamate_projection::{TeslaMateCar, TeslaMateHistory};

    let imported_at_ms = current_epoch_millis().expect("clock") - 60_000;
    let history = TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: crate::fake_tesla::FIXTURE_EID as i64,
            vid: Some(crate::fake_tesla::FIXTURE_VID as i64),
            vin: Some(crate::fake_tesla::FIXTURE_VIN.to_owned()),
            name: Some("Journey fixture".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: Some("74d".to_owned()),
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: crate::hub_pack::ProjectionCarSettings {
                enabled: true,
                use_streaming_api: false,
                ..Default::default()
            },
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
        store,
        &CursorKey::from_bytes([0xD3; 32]),
        &TeslaMateImportRequest {
            source_key: "journey-fixture".to_owned(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms,
        },
        &history,
    )
    .expect("seed selected imported car");
}

/// One run of the supervised Owner loop against the fake, ready to be shut down.
struct JourneyRun {
    task: JoinHandle<Result<(), CollectorError>>,
    shutdown: oneshot::Sender<()>,
}

impl JourneyRun {
    async fn start(
        store: &HubStore,
        data_dir: &std::path::Path,
        fake: &crate::fake_tesla::FakeTeslaSource,
        cursor_byte: u8,
    ) -> Self {
        // Observer mode: the Hub is not the refresh owner, so no token request is
        // made and the fake's audit holds only the collector's reads.
        let manager = LegacyAuthManager::from_hub_teslamate_store_observer_with_issuer(
            store.clone(),
            data_dir,
            fake.oauth_issuer_url(),
        )
        .expect("load the stored legacy pair");
        let region = manager.region();
        let auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: false,
            region,
        };
        let client = crate::owner_api::OwnerApi::for_fake_http(
            fake.http_base_url().clone(),
            Duration::from_secs(2),
        )
        .expect("owner client");
        let config = supervised_restart_test_config(data_dir);
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task_store = store.clone();
        let stream_endpoint = fake.stream_endpoint().to_owned();
        let mut task = tokio::spawn(async move {
            run_supervised_with_access(
                &task_store,
                &config,
                supervised_restart_test_cadence(),
                client,
                auth,
                stream_endpoint,
                CursorKey::from_bytes([cursor_byte; 32]),
                Some(ready_tx),
                RunWriteGuard::for_run(None),
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        });
        let _cursor =
            wait_for_supervised_signal_or_abort("journey collector readiness", &mut task, ready_rx)
                .await;
        Self { task, shutdown }
    }

    async fn stop(mut self) {
        self.shutdown.send(()).expect("collector shutdown");
        join_supervised_restart_task("journey collector join", &mut self.task)
            .await
            .expect("journey collector result");
    }
}

/// How many audited requests of `suffix` (`products` or `vehicle_data`) the fake
/// received while it stood at `step`.
fn audited_at(
    fake: &crate::fake_tesla::FakeTeslaSource,
    suffix: &str,
    step: crate::fake_tesla::ScenarioStep,
) -> usize {
    fake.audited_requests()
        .iter()
        .filter(|request| request.path.ends_with(suffix) && request.scenario_step == step)
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct33_the_canonical_scenario_with_a_restart_never_wakes_the_car() {
    use crate::fake_tesla::{AdvanceMode, FakeTeslaSource, ScenarioStep};

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_journey_import(&store);
    crate::teslamate_credentials::replace_key(temporary.path(), b"journey-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial =
        OwnerTokens::from_secret_parts("journey-access".to_owned(), "journey-refresh".to_owned())
            .expect("initial pair");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("Cloak initial pair");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, 2, 1)
                .expect("due refresh schedule"),
        )
        .expect("store encrypted pair");

    // The scenario is stepped by the test, in the order of the canonical one:
    // asleep, offline, online idle, drive, parked, charge, asleep again (with a
    // restart of the collector), software update, unchanged.
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    fake.set_base_ts_ms(current_epoch_millis().expect("clock") - 900_000);
    assert_eq!(fake.current_step(), ScenarioStep::AsleepDiscovery);

    let run = JourneyRun::start(&store, temporary.path(), &fake, 0xC3).await;
    // Asleep and offline: the lists are read, the car is not.
    wait_for_supervised_restart_condition("asleep lists", || {
        audited_at(&fake, "products", ScenarioStep::AsleepDiscovery) >= 2
    })
    .await;
    fake.advance();
    assert_eq!(fake.current_step(), ScenarioStep::OfflineDiscovery);
    wait_for_supervised_restart_condition("offline lists", || {
        audited_at(&fake, "products", ScenarioStep::OfflineDiscovery) >= 2
    })
    .await;
    // The car wakes (the list says online): one wake read, then the cadence.
    fake.advance();
    assert_eq!(fake.current_step(), ScenarioStep::OnlineIdle);
    wait_for_supervised_restart_condition("online idle read", || {
        audited_at(&fake, "vehicle_data", ScenarioStep::OnlineIdle) >= 1
    })
    .await;
    fake.advance();
    assert_eq!(fake.current_step(), ScenarioStep::DrivePositions);
    wait_for_supervised_restart_condition("drive reads", || {
        audited_at(&fake, "vehicle_data", ScenarioStep::DrivePositions) >= 2
    })
    .await;
    fake.advance();
    assert_eq!(fake.current_step(), ScenarioStep::Parked);
    wait_for_supervised_restart_condition("parked read", || {
        audited_at(&fake, "vehicle_data", ScenarioStep::Parked) >= 1
    })
    .await;
    fake.advance();
    assert_eq!(fake.current_step(), ScenarioStep::ChargeSamples);
    wait_for_supervised_restart_condition("charge reads", || {
        audited_at(&fake, "vehicle_data", ScenarioStep::ChargeSamples) >= 1
    })
    .await;

    // The collector restarts while the car goes to sleep: a restarted collector
    // reads a listed-asleep car no more than a running one does.
    run.stop().await;
    fake.set_step(ScenarioStep::AsleepRestart);
    let run = JourneyRun::start(&store, temporary.path(), &fake, 0xC4).await;
    wait_for_supervised_restart_condition("asleep lists after the restart", || {
        audited_at(&fake, "products", ScenarioStep::AsleepRestart) >= 2
    })
    .await;
    fake.advance();
    assert_eq!(fake.current_step(), ScenarioStep::OnlineSoftwareUpdate);
    wait_for_supervised_restart_condition("software update lists", || {
        audited_at(&fake, "products", ScenarioStep::OnlineSoftwareUpdate) >= 2
    })
    .await;
    fake.advance();
    assert_eq!(fake.current_step(), ScenarioStep::UnchangedNoOp);
    wait_for_supervised_restart_condition("unchanged lists", || {
        audited_at(&fake, "products", ScenarioStep::UnchangedNoOp) >= 1
    })
    .await;
    run.stop().await;

    // The fake's own audit: no wake or command request, no rejected request (a read
    // of an asleep or offline car is rejected by the fake), and no `vehicle_data`
    // read in a step in which the car was asleep or offline.
    fake.assert_no_wake_or_command()
        .expect("no wake or command was sent");
    for step in [
        ScenarioStep::AsleepDiscovery,
        ScenarioStep::OfflineDiscovery,
        ScenarioStep::AsleepRestart,
    ] {
        assert_eq!(
            audited_at(&fake, "vehicle_data", step),
            0,
            "a read in {step:?}"
        );
        assert!(
            audited_at(&fake, "products", step) >= 2,
            "the lists in {step:?} were read"
        );
    }
    // The proof is not vacuous: the car was read in the steps where it was awake.
    for step in [ScenarioStep::OnlineIdle, ScenarioStep::DrivePositions] {
        assert!(audited_at(&fake, "vehicle_data", step) >= 1, "{step:?}");
    }
    fake.shutdown().await;
}

/// R40 through the supervised loop: a car that drives, then falls asleep (so nothing
/// more is read or stored for it), has its open drive closed by a Hub `no_data` event
/// once `T_gap` has passed without evidence (1 s in the journey's cadence). The loop
/// plans the event at every iteration whatever the car's phase.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_loop_closes_an_open_drive_when_the_car_falls_silent() {
    use crate::fake_tesla::{AdvanceMode, FakeTeslaSource, ScenarioStep};

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_journey_import(&store);
    crate::teslamate_credentials::replace_key(temporary.path(), b"journey-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial =
        OwnerTokens::from_secret_parts("journey-access".to_owned(), "journey-refresh".to_owned())
            .expect("initial pair");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("Cloak initial pair");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, 2, 1)
                .expect("due refresh schedule"),
        )
        .expect("store encrypted pair");
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    fake.set_base_ts_ms(current_epoch_millis().expect("clock") - 900_000);
    fake.set_step(ScenarioStep::DrivePositions);
    let hub = store
        .configured_tesla_vehicles()
        .expect("configured")
        .first()
        .map(|(hub, _, _)| *hub)
        .expect("the selected car");

    let run = JourneyRun::start(&store, temporary.path(), &fake, 0xC5).await;
    wait_for_supervised_restart_condition("drive reads", || {
        audited_at(&fake, "vehicle_data", ScenarioStep::DrivePositions) >= 2
            && store
                .collector_lifecycle_snapshot(hub)
                .is_ok_and(|snapshot| snapshot.drive_open)
    })
    .await;
    let reads_while_driving = audited_at(&fake, "vehicle_data", ScenarioStep::DrivePositions);
    // The car goes to sleep: the list says asleep, nothing more is read for it.
    fake.set_step(ScenarioStep::AsleepRestart);
    wait_for_supervised_restart_condition("the drive is closed by no_data", || {
        store
            .collector_lifecycle_snapshot(hub)
            .is_ok_and(|snapshot| !snapshot.drive_open)
    })
    .await;
    run.stop().await;
    assert_eq!(
        audited_at(&fake, "vehicle_data", ScenarioStep::AsleepRestart),
        0,
        "no read of the asleep car"
    );
    assert!(reads_while_driving >= 2);
    fake.assert_no_wake_or_command().expect("no wake");
    fake.shutdown().await;
}
