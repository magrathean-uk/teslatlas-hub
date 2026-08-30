// SPDX-License-Identifier: AGPL-3.0-only

fn test_cadence() -> CollectorCadence {
    CollectorCadence {
        driving: Duration::from_secs(5),
        charging: Duration::from_secs(10),
        online: Duration::from_secs(75),
        sleeping: Duration::from_secs(30),
        offline_drive_timeout: Duration::from_secs(15 * 60),
        idle_suspend_after: Duration::from_secs(15 * 60),
        suspended: Duration::from_secs(21 * 60),
        updating: Duration::from_secs(15),
        stream_health_timeout: Duration::from_secs(30),
        maximum_backoff: Duration::from_secs(900),
    }
}

fn supervised_restart_test_cadence() -> CollectorCadence {
    CollectorCadence {
        // Keep a run quiescent after its initial proof transaction. This
        // makes the competing-lease assertion distinguish its work from
        // a legitimate next scheduled poll.
        driving: Duration::from_secs(1),
        charging: Duration::from_secs(1),
        online: Duration::from_secs(1),
        sleeping: Duration::from_secs(1),
        offline_drive_timeout: Duration::from_secs(1),
        idle_suspend_after: Duration::from_secs(1),
        suspended: Duration::from_secs(1),
        updating: Duration::from_secs(1),
        // The fake sends its finite eight-frame burst then waits for the
        // collector's orderly unsubscribe. Keep this above initial setup
        // so a silence reconnect cannot race that proof.
        stream_health_timeout: Duration::from_secs(10),
        maximum_backoff: Duration::from_secs(1),
    }
}

fn supervised_restart_test_config(data_dir: &std::path::Path) -> HubConfig {
    HubConfig {
        data_dir: data_dir.to_path_buf(),
        bind: "127.0.0.1:39191".parse().expect("loopback bind"),
        tls: None,
        collector: crate::config::CollectorConfig::default(),
        geocoder: crate::config::GeocoderConfig {
            enabled: false,
            ..crate::config::GeocoderConfig::default()
        },
        teslamate: crate::config::TeslaMateConfig::default(),
        terrain: TerrainConfig {
            cache_dir: Some(data_dir.join("terrain-cache")),
            ..TerrainConfig::default()
        },
    }
}

fn seed_supervised_restart_import(store: &HubStore) {
    use crate::teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history};
    use crate::teslamate_projection::{TeslaMateCar, TeslaMateHistory};

    let imported_at_ms = current_epoch_millis().expect("clock") - 60_000;
    let history = TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: crate::fake_tesla::FIXTURE_EID as i64,
            vid: Some(crate::fake_tesla::FIXTURE_VID as i64),
            vin: Some(crate::fake_tesla::FIXTURE_VIN.to_owned()),
            name: Some("Restart fixture".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: Some("74d".to_owned()),
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: crate::hub_pack::ProjectionCarSettings {
                enabled: true,
                use_streaming_api: true,
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
        &CursorKey::from_bytes([0xD1; 32]),
        &TeslaMateImportRequest {
            source_key: "supervised-restart-fixture".to_owned(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms,
        },
        &history,
    )
    .expect("seed selected imported car");
}

async fn join_supervised_restart_task(
    label: &str,
    task: &mut JoinHandle<Result<(), CollectorError>>,
) -> Result<(), CollectorError> {
    match timeout(Duration::from_secs(5), &mut *task).await {
        Ok(result) => result.expect("supervised collector task join"),
        Err(_) => {
            task.abort();
            let _ = task.await;
            panic!("{label} timeout");
        }
    }
}

async fn wait_for_supervised_signal_or_abort<T>(
    label: &str,
    task: &mut JoinHandle<Result<(), CollectorError>>,
    signal: oneshot::Receiver<T>,
) -> T {
    match timeout(Duration::from_secs(5), signal).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => {
            task.abort();
            let _ = task.await;
            panic!("{label} dropped");
        }
        Err(_) => {
            task.abort();
            let _ = task.await;
            panic!("{label} timeout");
        }
    }
}

async fn wait_for_supervised_restart_condition(label: &str, mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !condition() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label} timeout"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imported_legacy_pair_refreshes_then_collects_one_car_and_survives_reopen() {
    use crate::{
        credentials::{LegacyAuthManager, OwnerTokens},
        fake_tesla::{AdvanceMode, FAKE_REFRESHED_ACCESS_TOKEN, FakeTeslaSource},
        owner_api::OwnerApi,
    };

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);

    crate::teslamate_credentials::replace_key(temporary.path(), b"chain-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial =
        OwnerTokens::from_secret_parts("initial-access".to_owned(), "initial-refresh".to_owned())
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
    fake.set_step(crate::fake_tesla::ScenarioStep::UnchangedNoOp);
    fake.set_base_ts_ms(current_epoch_millis().expect("clock") - 900_000);
    let manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        temporary.path(),
        fake.oauth_issuer_url(),
    )
    .expect("load encrypted legacy pair");
    let region = manager.region();
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region,
    };
    let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
        .expect("owner client");
    let config = supervised_restart_test_config(temporary.path());
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let (finished, resume) = seam.arm_paused_collection_completion().await;
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task_store = store.clone();
    let task_config = config.clone();
    let stream_endpoint = fake.stream_endpoint().to_owned();
    let mut task = tokio::spawn(async move {
        SUPERVISED_COLLECTOR_TEST_SEAM
            .scope(seam, async move {
                run_supervised_with_access(
                    &task_store,
                    &task_config,
                    supervised_restart_test_cadence(),
                    client,
                    auth,
                    stream_endpoint,
                    CursorKey::from_bytes([0xC1; 32]),
                    Some(ready_tx),
                    None,
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
            })
            .await
    });
    let _cursor =
        wait_for_supervised_signal_or_abort("collector readiness", &mut task, ready_rx).await;
    let _report =
        wait_for_supervised_signal_or_abort("first collection", &mut task, finished).await;
    assert_eq!(
        fake.token_refresh_request_count(),
        1,
        "one startup OAuth refresh"
    );
    assert!(fake.audited_requests().iter().any(|request| {
        request.path
            == format!(
                "/api/1/vehicles/{}/vehicle_data",
                crate::fake_tesla::FIXTURE_EID
            )
    }));

    resume.send(()).expect("resume stream drain");
    wait_for_supervised_restart_condition("numeric stream persistence", || {
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT COUNT(*) FROM current_observations
                     WHERE record_type = 'tesla_stream_update_v1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("stream count")
            > 0
    })
    .await;
    shutdown_tx.send(()).expect("collector shutdown");
    join_supervised_restart_task("collector join", &mut task)
        .await
        .expect("collector result");
    wait_for_supervised_restart_condition("stream socket shutdown", || {
        fake.stream_session_stats().active_sessions == 0
    })
    .await;
    assert!(
        fake.audited_stream_events()
            .iter()
            .any(|event| { event.event == crate::fake_tesla::StreamAuditEvent::Unsubscribe })
    );

    let stored = store
        .load_teslamate_legacy_tokens()
        .expect("stored pair")
        .expect("stored pair exists");
    let successor = crate::teslamate_token::decrypt_legacy_owner_tokens(
        key.as_bytes(),
        stored.access(),
        stored.refresh(),
    )
    .expect("decrypt Cloak successor");
    assert_eq!(successor.access_token(), FAKE_REFRESHED_ACCESS_TOKEN);
    assert!(stored.next_refresh_at() > 0 && stored.next_refresh_at() < stored.expires_at());

    drop(store);
    let reopened_store = HubStore::initialize(temporary.path()).expect("reopen Hub");
    let vehicle_count: i64 = reopened_store
        .open()
        .expect("reopened catalogue")
        .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
        .expect("one selected vehicle");
    assert_eq!(vehicle_count, 1);
    let reopened = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        reopened_store,
        temporary.path(),
        fake.oauth_issuer_url(),
    )
    .expect("reopen latest pair");
    assert_eq!(reopened.access_token(), FAKE_REFRESHED_ACCESS_TOKEN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_collects_and_reconnects_without_refreshing() {
    use crate::{
        credentials::{LegacyAuthManager, OwnerTokens},
        fake_tesla::{AdvanceMode, FakeTeslaSource},
        owner_api::OwnerApi,
    };

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    let setup_auth = LegacyAuth::for_test(
        fake.oauth_issuer_url(),
        "observer-access",
        "observer-refresh",
    );
    let setup_client =
        OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
            .expect("setup Owner client");
    setup_native_vehicle_with_client(&store, temporary.path(), &setup_client, &setup_auth, None)
        .await
        .expect("native setup");
    crate::teslamate_credentials::replace_key(temporary.path(), b"observer-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial =
        OwnerTokens::from_secret_parts("observer-access".to_owned(), "observer-refresh".to_owned())
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

    fake.set_step(crate::fake_tesla::ScenarioStep::UnchangedNoOp);
    fake.set_base_ts_ms(current_epoch_millis().expect("clock") - 900_000);
    let manager = LegacyAuthManager::from_hub_teslamate_store_observer_with_issuer(
        store.clone(),
        temporary.path(),
        fake.oauth_issuer_url(),
    )
    .expect("load observer pair");
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: false,
        region: StreamRegion::Global,
    };
    let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
        .expect("Owner client");
    let config = supervised_restart_test_config(temporary.path());
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let (finished, resume) = seam.arm_paused_collection_completion().await;
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task_store = store.clone();
    let task_config = config.clone();
    let stream_endpoint = fake.stream_endpoint().to_owned();
    let mut task = tokio::spawn(async move {
        SUPERVISED_COLLECTOR_TEST_SEAM
            .scope(seam, async move {
                run_supervised_with_access(
                    &task_store,
                    &task_config,
                    supervised_restart_test_cadence(),
                    client,
                    auth,
                    stream_endpoint,
                    CursorKey::from_bytes([0xC2; 32]),
                    Some(ready_tx),
                    None,
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
            })
            .await
    });
    let _cursor =
        wait_for_supervised_signal_or_abort("observer readiness", &mut task, ready_rx).await;
    let _report =
        wait_for_supervised_signal_or_abort("observer first collection", &mut task, finished).await;
    assert_eq!(fake.token_refresh_request_count(), 0);
    assert!(fake.audited_requests().iter().any(|request| {
        request.path
            == format!(
                "/api/1/vehicles/{}/vehicle_data",
                crate::fake_tesla::FIXTURE_EID
            )
    }));

    resume.send(()).expect("resume observer stream drain");
    wait_for_supervised_restart_condition("observer stream persistence", || {
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT COUNT(*) FROM current_observations
                     WHERE record_type = 'tesla_stream_update_v1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("stream count")
            > 0
    })
    .await;
    fake.set_stream_available(false);
    wait_for_supervised_restart_condition("observer stream reconnect attempt", || {
        fake.stream_session_stats().connection_attempts >= 2
    })
    .await;
    fake.set_stream_available(true);
    assert_eq!(fake.token_refresh_request_count(), 0);
    shutdown_tx.send(()).expect("observer shutdown");
    join_supervised_restart_task("observer collector join", &mut task)
        .await
        .expect("observer collector result");
    assert_eq!(fake.token_refresh_request_count(), 0);
}

#[tokio::test]
async fn observer_stream_rejection_does_not_enqueue_refresh() {
    use crate::fake_tesla::{AdvanceMode, FakeTeslaSource};

    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        fake.oauth_issuer_url(),
        "observer-access",
        "observer-refresh",
    )
    .with_test_schedule(2_000_000_000, 1_900_000_000);
    let collection_auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
            auth,
            Arc::new(|_, _| Ok(())),
        ))),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: false,
        region: StreamRegion::Global,
    };
    let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
        .expect("Owner client");
    assert!(matches!(
        refresh_after_stream_authentication_rejection(
            &client,
            &collection_auth,
            StreamAuthenticationTransition::Rejected,
        )
        .await,
        Err(CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
            OwnerApiError::HttpStatus(401)
        )))
    ));
    sleep(Duration::from_millis(25)).await;
    assert_eq!(fake.token_refresh_request_count(), 0);
    shutdown_legacy_refresh(&collection_auth).await;
}

#[tokio::test]
async fn managed_stream_rejection_enqueues_legacy_refresh() {
    use crate::fake_tesla::{AdvanceMode, FakeTeslaSource};

    crate::crypto::install_default_provider();
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        fake.oauth_issuer_url(),
        "stream-access",
        "stream-refresh",
    )
    .with_test_schedule(2_000_000_000, 1_900_000_000);
    let collection_auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
            auth,
            Arc::new(|_, _| Ok(())),
        ))),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    };
    let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
        .expect("Owner client");

    refresh_after_stream_authentication_rejection(
        &client,
        &collection_auth,
        StreamAuthenticationTransition::Rejected,
    )
    .await
    .expect("managed stream rejection queues refresh");
    timeout(Duration::from_secs(1), async {
        while fake.token_refresh_request_count() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("one refresh request");
    wait_for_legacy_refresh_before_owner(&collection_auth)
        .await
        .expect("durably refreshed");
    shutdown_legacy_refresh(&collection_auth).await;
    fake.shutdown().await;
}

#[tokio::test]
async fn persisted_refresh_failure_fences_later_collection() {
    use crate::fake_tesla::{AdvanceMode, FakeTeslaSource};

    crate::crypto::install_default_provider();
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        fake.oauth_issuer_url(),
        "stream-access",
        "stream-refresh",
    )
    .with_test_schedule(2_000_000_000, 1_900_000_000);
    let collection_auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
            auth,
            Arc::new(|_, _| Err(CredentialError::LegacyTokenStateWrite)),
        ))),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    };
    let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
        .expect("Owner client");

    refresh_after_stream_authentication_rejection(
        &client,
        &collection_auth,
        StreamAuthenticationTransition::Rejected,
    )
    .await
    .expect("queue refresh");
    assert!(matches!(
        wait_for_legacy_refresh_before_owner(&collection_auth).await,
        Err(CollectorError::SensitiveAccessUnavailable)
    ));
    assert_eq!(fake.token_refresh_request_count(), 1);
    shutdown_legacy_refresh(&collection_auth).await;
    fake.shutdown().await;
}
