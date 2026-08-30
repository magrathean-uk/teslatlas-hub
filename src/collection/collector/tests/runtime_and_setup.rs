// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn fleet_provider_http_errors_keep_http_classification() {
    let error = FleetApiError::ProviderHttpStatus {
        status: 400,
        error: "invalid_configuration".to_owned(),
        description: None,
    };
    assert_eq!(
        fleet_action_completion(Some(&error)),
        OutboundRequestCompletion {
            outcome: OutboundRequestOutcome::HttpError,
            http_status: Some(400),
            retry_after_seconds: None,
        }
    );
    assert_eq!(
        fleet_failure_as_owner_error(&CollectorError::FleetApi(error)),
        OwnerApiError::HttpStatus(400)
    );
}

#[test]
fn owner_collection_failure_preserves_power_gate_error_classification() {
    assert_eq!(
        owner_failure_for_collector_error(CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
            OwnerApiError::StreamPowerNotConfirmed
        ),)),
        OwnerApiError::StreamPowerNotConfirmed
    );
    assert_eq!(
        owner_failure_for_collector_error(CollectorError::OwnerApiAuth(
            OwnerApiAuthError::NotSignedIn,
        )),
        OwnerApiError::LegacyAuth
    );
}

#[test]
fn fleet_provider_not_found_uses_not_found_schedule() {
    let now = Instant::now();
    let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![vehicle], now);

    scheduler.vehicle_failed_for_error(
        vehicle_id,
        &CollectorError::FleetApi(FleetApiError::ProviderHttpStatus {
            status: 404,
            error: "vehicle_not_found".to_owned(),
            description: None,
        }),
        now,
    );

    assert_eq!(scheduler.vehicle_fuses[&vehicle_id].not_found.len(), 1);
}

#[test]
fn lifecycle_id_seed_uses_covering_import_indexes_once() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let connection = store.open().expect("connection");
    for (sql, second) in [
        (
            LEGACY_IMPORT_MAX_ID_SQL,
            rusqlite::types::Value::Text("position".to_owned()),
        ),
        (
            CURRENT_IMPORT_MAX_ID_SQL,
            rusqlite::types::Value::Integer(2),
        ),
    ] {
        let mut statement = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("query plan");
        let details = statement
            .query_map(
                rusqlite::params![Uuid::new_v4().to_string(), second],
                |row| row.get::<_, String>(3),
            )
            .expect("plan rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("plan details")
            .join("\n");
        assert!(
            details.contains("SEARCH")
                && !details.contains("SCAN")
                && !details.contains("TEMP B-TREE"),
            "ID seed must seek the covering import key: {details}"
        );
    }

    let mut state = OpenSessionState::new();
    assert!(!state.id_cursors_seeded);
    seed_lifecycle_ids_from_materialised(&store, Uuid::new_v4(), &mut state).expect("indexed seed");
    assert!(state.id_cursors_seeded);
    assert!(
        OpenSessionState::decode(&state.encode().expect("state encode"))
            .expect("state decode")
            .id_cursors_seeded
    );
}

#[test]
fn fleet_proxy_root_certificate_is_descriptor_pinned_and_private() {
    let temporary = tempfile::tempdir().expect("temporary certificate root");
    let certificate = temporary.path().join("proxy-ca.pem");
    fs::write(&certificate, b"trusted-ca").expect("write certificate");
    fs::set_permissions(&certificate, fs::Permissions::from_mode(0o600))
        .expect("protect certificate");
    assert_eq!(
        read_fleet_proxy_root_certificate(&certificate, 128).expect("read safe certificate"),
        b"trusted-ca"
    );

    let link = temporary.path().join("proxy-ca-link.pem");
    symlink(&certificate, &link).expect("certificate symlink");
    assert!(matches!(
        read_fleet_proxy_root_certificate(&link, 128),
        Err(FleetApiConfigError::InvalidRootCertificate)
    ));

    fs::set_permissions(&certificate, fs::Permissions::from_mode(0o622))
        .expect("make certificate writable");
    assert!(matches!(
        read_fleet_proxy_root_certificate(&certificate, 128),
        Err(FleetApiConfigError::InvalidRootCertificate)
    ));
    fs::set_permissions(&certificate, fs::Permissions::from_mode(0o600))
        .expect("restore certificate mode");

    assert!(matches!(
        read_fleet_proxy_root_certificate(&certificate, 4),
        Err(FleetApiConfigError::InvalidRootCertificate)
    ));

    let replacement = temporary.path().join("replacement.pem");
    fs::write(&replacement, b"other-ca").expect("write replacement");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))
        .expect("protect replacement");
    assert!(matches!(
        read_fleet_proxy_root_certificate_after_open(&certificate, 128, || {
            fs::rename(&replacement, &certificate).expect("replace admitted certificate");
        }),
        Err(FleetApiConfigError::InvalidRootCertificate)
    ));
}

#[tokio::test]
async fn stream_task_completion_outcomes_are_typed_and_secret_safe() {
    let normal = classify_stream_task_result(
        tokio::spawn(async { Ok::<_, crate::tesla_stream::StreamSupervisorError>(()) }).await,
    );
    let supervisor = classify_stream_task_result(
        tokio::spawn(async {
            Err::<(), _>(crate::tesla_stream::StreamSupervisorError::EventQueueFull)
        })
        .await,
    );
    let panic = classify_stream_task_result(
        tokio::spawn(async { panic!("access-secret refresh-secret") }).await,
    );
    let cancelled_task = tokio::spawn(async {
        std::future::pending::<Result<(), crate::tesla_stream::StreamSupervisorError>>().await
    });
    cancelled_task.abort();
    let cancelled = classify_stream_task_result(cancelled_task.await);

    assert!(matches!(
        &normal,
        CollectorError::StreamTask(StreamTaskOutcome::CompletedNormally)
    ));
    assert!(matches!(
        &supervisor,
        CollectorError::StreamTask(StreamTaskOutcome::Supervisor(
            crate::tesla_stream::StreamSupervisorError::EventQueueFull
        ))
    ));
    assert!(matches!(
        &panic,
        CollectorError::StreamTask(StreamTaskOutcome::Panicked)
    ));
    assert!(matches!(
        &cancelled,
        CollectorError::StreamTask(StreamTaskOutcome::Cancelled)
    ));
    for error in [&normal, &supervisor, &panic, &cancelled] {
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
    }

    let tokens =
        OwnerTokens::from_secret_parts("access-secret".to_owned(), "refresh-secret".to_owned())
            .expect("bounded bearer pair");
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        url::Url::parse("https://auth.tesla.com/oauth2/v3/token").unwrap(),
        "access-secret",
        "refresh-secret",
    );
    let manager = LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
    for rendered in [format!("{tokens:?}"), format!("{manager:?}")] {
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
    }
}

#[tokio::test]
async fn completed_stream_task_drains_final_events_and_is_reaped_once() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let vehicle = Vehicle::for_test(29, "5YJ3E1EA7KF000029", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
    scheduler.accept_discovery(vec![vehicle], Instant::now());
    let (events, receiver) = mpsc::channel(2);
    events
        .send(StreamEvent::Telemetry {
            update: Box::new(crate::tesla_stream::StreamUpdate {
                tag: vehicle_id.to_string(),
                timestamp_ms: current_epoch_millis().expect("clock") - 1_000,
                speed: Some(20),
                odometer: Some(100.0),
                soc: Some(80),
                elevation: Some(25),
                est_heading: Some(180),
                est_lat: Some(51.5),
                est_lng: Some(-0.1),
                power: Some(12),
                shift_state: Some("D".to_owned()),
                range: Some(200),
                est_range: Some(210),
                heading: Some(180),
            }),
            queued_at: Instant::now(),
        })
        .await
        .expect("final telemetry");
    events
        .send(StreamEvent::AuthRejected)
        .await
        .expect("final auth transition");
    drop(events);
    let (shutdown, _stop) = oneshot::channel();
    let task = tokio::spawn(async {
        Err::<(), _>(crate::tesla_stream::StreamSupervisorError::EventQueueFull)
    });
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: Some(shutdown),
        task: Some(task),
    }];
    while !streams[0]
        .task
        .as_ref()
        .is_some_and(JoinHandle::is_finished)
    {
        tokio::task::yield_now().await;
    }

    let mut projection_car_ids = HashMap::new();
    let result = drain_stream_events_with_cache(
        &store,
        &mut scheduler,
        &mut streams,
        &mut projection_car_ids,
    )
    .await
    .expect("final events must drain before task failure");
    assert_eq!(result.transition, StreamAuthenticationTransition::Rejected);
    assert!(matches!(
        result.terminal_error,
        Some(CollectorError::StreamTask(StreamTaskOutcome::Supervisor(
            crate::tesla_stream::StreamSupervisorError::EventQueueFull
        )))
    ));
    assert!(streams[0].task.is_none(), "completed task was not consumed");
    assert!(!scheduler.vehicles[&vehicle_id].stream_healthy);
    let registered = projection_car_ids[&vehicle_id].registered_vehicle_id;
    let observations = store
        .current_observations_for_vehicle(registered)
        .expect("final telemetry observation");
    assert!(
        observations
            .iter()
            .any(|observation| { observation.payload["record_type"] == "tesla_stream_update_v1" })
    );

    stop_and_clear_manual_probe_streams(&mut streams).await;
    assert!(streams.is_empty());
}

#[tokio::test]
async fn resident_control_socket_is_private_bounded_and_service_owned() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let socket = ResidentControlSocket::bind(temporary.path()).expect("control socket");
    let socket_path = temporary.path().join(RESIDENT_CONTROL_SOCKET_NAME);
    let metadata = std::fs::symlink_metadata(&socket_path).expect("socket metadata");
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.mode() & 0o777, 0o600);

    let client = OwnerApi::for_fake_http(
        url::Url::parse("http://127.0.0.1:9/").expect("loopback URL"),
        Duration::from_secs(1),
    )
    .expect("bounded Owner client");
    let manager = Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
        LegacyAuth::for_test(
            url::Url::parse("https://auth.tesla.com/oauth2/v3/token").expect("issuer URL"),
            "resident-access",
            "resident-refresh",
        ),
        Arc::new(|_, _| Ok(())),
    )));
    let fuse = Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default()));
    let refresh = Arc::new(LegacyRefreshCoordinator::default());
    let mut task = tokio::spawn(socket.serve(store, client, manager, fuse, refresh));

    let error = request_resident_vehicle_action(
        temporary.path(),
        Uuid::new_v4(),
        LegacyVehicleAction::Wake,
    )
    .await
    .expect_err("unconfigured vehicle rejected locally");
    assert!(matches!(error, ResidentVehicleActionError::VehicleMissing));

    task.abort();
    let _ = (&mut task).await;
    assert!(!socket_path.exists());
}

#[tokio::test]
async fn resident_fleet_control_uses_selected_vehicle_shared_bearer_proxy_and_audit() {
    const SELECTED_EID: i64 = 70;
    const SELECTED_VIN: &str = "5YJ3E1EA7KF000001";
    const ACCESS_TOKEN: &str = "resident-fleet-access";

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let selected = select_native_setup_vehicle(
        vec![
            Vehicle::for_test(SELECTED_EID as u64, SELECTED_VIN, "online"),
            Vehicle::for_test(90, "5YJ3E1EA7KF000002", "online"),
        ],
        Some(SELECTED_EID),
    )
    .expect("explicit Fleet vehicle selection");
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    finish_collection_for_provider(
        &store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![selected],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await
    .expect("persist selected Fleet vehicle");
    let (hub_vehicle_id, eid, _) = store
        .configured_tesla_vehicles()
        .expect("configured vehicles")
        .into_iter()
        .next()
        .expect("selected vehicle");
    assert_eq!(eid, SELECTED_EID);

    let credentials = FleetSetupCredentials::new(
        ACCESS_TOKEN.to_owned(),
        "resident-fleet-refresh".to_owned(),
        "resident-fleet-client".to_owned(),
        crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("Fleet credentials");
    crate::fleet_credentials::persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        SystemTime::now(),
    )
    .expect("persist encrypted Fleet credentials");
    let manager = Arc::new(tokio::sync::Mutex::new(
        FleetAuthManager::from_store(store.clone(), temporary.path())
            .expect("resident Fleet credential manager"),
    ));

    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let proxy_store = store.clone();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake proxy listener");
    let address = listener.local_addr().expect("fake proxy address");
    let router = Router::new().route(
        &format!("/api/1/vehicles/{SELECTED_VIN}/command/door_lock"),
        post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let recorded = Arc::clone(&recorded);
            let proxy_store = proxy_store.clone();
            async move {
                let audit_started = proxy_store
                    .open()
                    .expect("proxy-side audit catalogue")
                    .query_row(
                        "SELECT EXISTS(
                                SELECT 1 FROM outbound_request_receipts
                                 WHERE transport = 'fleet_api'
                                   AND operation = 'vehicle_command'
                                   AND outcome = 'started'
                            )",
                        [],
                        |row| row.get::<_, bool>(0),
                    )
                    .expect("pre-egress audit receipt");
                recorded.lock().expect("proxy ledger").push((
                    audit_started,
                    headers
                        .get("authorization")
                        .is_some_and(|value| value.as_bytes() == b"Bearer resident-fleet-access"),
                    headers
                        .get("content-type")
                        .is_some_and(|value| value.as_bytes() == b"application/json"),
                    body.to_vec(),
                ));
                axum::Json(json!({"response": {"result": true, "reason": ""}}))
            }
        }),
    );
    let proxy_server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("fake proxy server");
    });
    let proxy_base =
        FleetCommandProxyBase::parse_loopback_http_for_test(&format!("http://{address}/"))
            .expect("loopback proxy URL");
    let proxy = FleetCommandProxy::for_fake_http(proxy_base, Duration::from_secs(2))
        .expect("fake command proxy");
    let api = FleetApi::new(
        crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
        Duration::from_secs(2),
    )
    .expect("Fleet API client");
    let auth_api = FleetAuthApi::new(
        crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
        Duration::from_secs(2),
    )
    .expect("Fleet auth client");
    let socket = ResidentControlSocket::bind(temporary.path()).expect("resident socket");
    let mut resident =
        tokio::spawn(socket.serve_fleet(store.clone(), api, auth_api, Some(proxy), manager));

    let report = request_resident_vehicle_action(
        temporary.path(),
        hub_vehicle_id,
        LegacyVehicleAction::Lock,
    )
    .await
    .expect("resident Fleet command");
    assert_eq!(report.provider, CollectorProvider::Fleet);
    assert_eq!(report.hub_vehicle_id, hub_vehicle_id);
    assert_eq!(report.tesla_eid, SELECTED_EID);
    assert!(matches!(report.action, LegacyVehicleAction::Lock));
    assert_eq!(report.result.state, None);
    assert_eq!(
        *requests.lock().expect("proxy ledger"),
        vec![(true, true, true, b"{}".to_vec())]
    );

    let receipt = store
        .open()
        .expect("receipt catalogue")
        .query_row(
            "SELECT transport, operation, safety_class, precondition, outcome,
                        http_status, completed_at_ms IS NOT NULL
                   FROM outbound_request_receipts WHERE id = ?1",
            [report.audit_receipt_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<u16>>(5)?,
                    row.get::<_, bool>(6)?,
                ))
            },
        )
        .expect("terminal outbound receipt");
    assert_eq!(
        receipt,
        (
            "fleet_api".to_owned(),
            "vehicle_command".to_owned(),
            "explicit_vehicle_command".to_owned(),
            "not_required".to_owned(),
            "success".to_owned(),
            Some(200),
            true,
        )
    );

    resident.abort();
    let _ = (&mut resident).await;
    proxy_server.abort();
    let _ = proxy_server.await;
}

#[tokio::test]
async fn fleet_observer_revalidates_admission_before_discovery_egress() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "observer-fleet-access".to_owned(),
        "observer-fleet-refresh".to_owned(),
        "observer-fleet-client".to_owned(),
        crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("Fleet credentials");
    crate::fleet_credentials::persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        SystemTime::now(),
    )
    .expect("persist Fleet credentials");
    let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
        .expect("admit observer");
    let manager = Arc::new(tokio::sync::Mutex::new(
        FleetAuthManager::from_store_for_admitted_user(store, temporary.path(), admission)
            .expect("observer manager"),
    ));

    let requests = Arc::new(Mutex::new(0_usize));
    let recorded = Arc::clone(&requests);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake Fleet listener");
    let address = listener.local_addr().expect("fake Fleet address");
    let router = Router::new().route(
        "/api/1/vehicles",
        get(move || {
            let recorded = Arc::clone(&recorded);
            async move {
                *recorded.lock().expect("request ledger") += 1;
                axum::Json(json!({"response": [], "count": 0}))
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("fake Fleet server");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake Fleet URL");
    let api =
        FleetApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("fake Fleet client");
    let auth_api = FleetAuthApi::for_fake_http(
        base.join("oauth2/v3/token").expect("fake auth URL"),
        Duration::from_secs(2),
    )
    .expect("fake auth client");

    let lock_path = temporary
        .path()
        .join(crate::user_lifetime_lock::LOCK_FILE_NAME);
    std::fs::remove_file(&lock_path).expect("remove admitted lock path");
    std::fs::write(&lock_path, b"").expect("replace admitted lock path");
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))
        .expect("replacement lock mode");

    assert!(matches!(
        fleet_list_vehicles_with_auth(&api, &auth_api, &manager, false).await,
        Err(CollectorError::FleetCredential(
            FleetCredentialError::SensitiveAccessUnavailable
        ))
    ));
    assert_eq!(*requests.lock().expect("request ledger"), 0);

    server.abort();
    let _ = server.await;
}

#[test]
fn fleet_push_refresh_retries_only_a_proven_unsent_request() {
    assert_eq!(
        fleet_refresh_retry_delay(&FleetCredentialError::Api(FleetApiError::RequestNotSent)),
        Some(FLEET_REFRESH_REQUEST_NOT_SENT_RETRY)
    );
    for error in [
        FleetApiError::RequestTimeout,
        FleetApiError::Transport,
        FleetApiError::HttpStatus(500),
        FleetApiError::InvalidResponse,
    ] {
        assert_eq!(
            fleet_refresh_retry_delay(&FleetCredentialError::Api(error)),
            None,
            "ambiguous Fleet refresh failure must remain terminal"
        );
    }
    assert_eq!(
        fleet_refresh_retry_delay(&FleetCredentialError::RotationOutcomeUnknown),
        None
    );
}

#[test]
fn native_setup_requires_an_explicit_choice_for_multiple_vehicles() {
    let first = Vehicle::for_test(7, "5YJ3E1EA7KF000001", "asleep");
    let second = Vehicle::for_test(9, "5YJ3E1EA7KF000002", "online");
    assert!(matches!(
        select_native_setup_vehicle(vec![first.clone(), second.clone()], None),
        Err(CollectorError::NativeSetupVehicleSelectionRequired { discovered: 2 })
    ));
    assert_eq!(
        select_native_setup_vehicle(vec![first, second], Some(9))
            .expect("selected vehicle")
            .id
            .get(),
        9
    );
}

#[tokio::test]
async fn fleet_setup_requires_every_configured_vehicle_by_eid_or_vin() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    finish_collection_for_provider(
        &store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![
                Vehicle::for_test(70, "5YJ3E1EA7KF000001", "online"),
                Vehicle::for_test(90, "5YJ3E1EA7KF000002", "online"),
            ],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await
    .expect("configured Fleet vehicles");

    let complete_inventory = vec![
        Vehicle::for_test(70, "5YJ3E1EA7KF000099", "online"),
        Vehicle::for_test(999, "5YJ3E1EA7KF000002", "online"),
    ];
    ensure_fleet_inventory_contains_configured(&store, &complete_inventory)
        .expect("EID or VIN matches every configured vehicle");

    assert!(matches!(
        ensure_fleet_inventory_contains_configured(&store, &complete_inventory[..1]),
        Err(CollectorError::FleetSetupInventoryMismatch)
    ));
}

#[tokio::test]
async fn native_fleet_telemetry_commits_and_restores_through_fleet_projection() {
    const VIN: &str = "5YJ3E1EA7KF000001";
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    finish_collection_for_provider(
        &store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![Vehicle::for_test(70, VIN, "online")],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await
    .expect("configured Fleet vehicle");
    assert!(
        fleet_telemetry_seed_for_vin(&store, VIN)
            .expect("empty seed lookup")
            .is_none()
    );

    let mut accumulator =
        crate::fleet_telemetry::FleetTelemetryAccumulator::empty(VIN).expect("accumulator");
    let t0 = current_epoch_millis().expect("clock") - 1_000;
    let snapshot = accumulator
        .apply_json(
            &serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "vin": VIN,
                "txid": "tx-drive-1",
                "tx_type": "vehicle_data",
                "received_at_ms": t0 + 100,
                "timestamp_ms": t0,
                "payload": {
                    "vin": VIN,
                    "createdAt": "2027-01-15T08:00:00Z",
                    "data": {
                        "Location": {"locationValue": {"latitude": 51.5, "longitude": -0.12}},
                        "VehicleSpeed": {"doubleValue": 48.0},
                        "Gear": {"stringValue": "drive"},
                        "Power": {"doubleValue": 12.0},
                        "BatteryLevel": {"doubleValue": 80.0},
                        "Soc": {"doubleValue": 79.0}
                    }
                }
            }))
            .expect("telemetry JSON"),
        )
        .expect("telemetry snapshot");
    let report = persist_fleet_telemetry_snapshot(&store, &cursor_key, &snapshot)
        .await
        .expect("telemetry commit");
    assert_eq!(report.observations_inserted, 1);

    let restored = fleet_telemetry_seed_for_vin(&store, VIN)
        .expect("restored seed")
        .expect("Fleet state");
    assert_eq!(restored["charge_state"]["battery_level"], 80);
    assert_eq!(restored["drive_state"]["shift_state"], "D");

    let duplicate = persist_fleet_telemetry_snapshot(&store, &cursor_key, &snapshot)
        .await
        .expect("duplicate telemetry commit");
    assert_eq!(duplicate.observations_inserted, 0);
    assert_eq!(duplicate.observations_already_present, 1);
}

#[tokio::test]
async fn fleet_vin_match_rotates_eid_without_losing_car_settings() {
    const VIN: &str = "5YJ3E1EA7KF000001";
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    let mut original = Vehicle::for_test(70, VIN, "online");
    original.settings.enabled = false;
    original.settings.suspend_after_idle_min = 123;
    original.settings.suspend_min = 456;
    original.settings.req_not_unlocked = false;
    finish_collection_for_provider(
        &store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![original],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await
    .expect("initial Fleet vehicle");

    let existing = store
        .configured_tesla_vehicles()
        .expect("configured vehicle");
    let mut rotated = Vehicle::for_test(999, VIN, "online");
    rotated.settings = configured_settings_for_discovered_vehicle(&store, &existing, &rotated)
        .expect("unambiguous VIN match")
        .expect("existing settings");
    rotated.settings.use_streaming_api = false;
    finish_collection_for_provider(
        &store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![rotated],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await
    .expect("rotated Fleet vehicle");

    let configured = store
        .configured_tesla_vehicles()
        .expect("canonical configured vehicle");
    assert_eq!(configured.len(), 1);
    assert_eq!(configured[0].1, 999);
    assert!(!configured[0].2.enabled);
    assert!(!configured[0].2.use_streaming_api);
    assert_eq!(configured[0].2.suspend_after_idle_min, 123);
    assert_eq!(configured[0].2.suspend_min, 456);
    assert!(!configured[0].2.req_not_unlocked);
    assert_eq!(
        store
            .configured_tesla_vehicle_identity(configured[0].0)
            .expect("canonical identity"),
        Some((999, Some(VIN.to_owned())))
    );
    assert!(matches!(
        configured_fleet_vehicle_for_vin(&store, VIN),
        Err(CollectorError::SelectedVehicleMissing)
    ));
}

#[tokio::test]
async fn native_setup_discovers_and_publishes_one_vehicle_without_wake() {
    use crate::fake_tesla::{AdvanceMode, FIXTURE_EID, FakeTeslaSource};

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    let auth = LegacyAuth::for_test(
        fake.oauth_issuer_url(),
        "native-setup-access",
        "native-setup-refresh",
    );
    let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
        .expect("loopback Owner client");

    let report = setup_native_vehicle_with_client(&store, temporary.path(), &client, &auth, None)
        .await
        .expect("native setup");

    assert_eq!(report.selected_vehicle_id, FIXTURE_EID as i64);
    assert_eq!(report.snapshots_published, 1);
    assert_eq!(
        store.selected_tesla_eid().expect("selection").unwrap().0,
        FIXTURE_EID as i64
    );
    assert_eq!(store.published_vehicles().expect("vehicles").len(), 1);
    let requests = fake.audited_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/api/1/products");
    assert!(!requests[0].rejected);

    #[cfg(target_os = "macos")]
    {
        let tokens = OwnerTokens::from_secret_parts(
            "native-setup-access".to_owned(),
            "native-setup-refresh".to_owned(),
        )
        .expect("setup tokens");
        let key = b"native-setup-key";
        let (access, refresh) = crate::teslamate_token::encrypt_legacy_owner_tokens(key, &tokens)
            .expect("encrypted setup tokens");
        let stored = crate::db::TeslaMateLegacyTokenStore::imported(access, refresh)
            .expect("stored setup tokens");
        crate::teslamate_credentials::replace_key_and_tokens(
            temporary.path(),
            &store,
            key,
            &stored,
        )
        .expect("persist setup tokens");
        crate::macos_launch_agent::preflight_hub(temporary.path())
            .expect("native setup is service-ready");
    }
}

#[tokio::test]
async fn native_setup_can_configure_every_discovered_vehicle_in_one_request() {
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = Router::new().route(
            "/api/1/products",
            get(move || {
                let counted = Arc::clone(&counted);
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "response": [
                            {"vehicle_id": 71, "id": 70, "vin": "5YJ3E1EA7KF000001", "state": "asleep", "display_name": "One"},
                            {"vehicle_id": 91, "id": 90, "vin": "5YJ3E1EA7KF000002", "state": "online", "display_name": "Two"}
                        ],
                        "count": 2
                    }))
                }
            }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake server");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
    let client =
        OwnerApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("Owner client");
    let auth = LegacyAuth::for_test(base, "setup-access", "setup-refresh");
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");

    let report = setup_native_vehicles_with_client(&store, temporary.path(), &client, &auth)
        .await
        .expect("multi-vehicle setup");

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(report.vehicles.len(), 2);
    assert_eq!(store.configured_tesla_vehicles().expect("cars").len(), 2);
    assert_eq!(store.published_vehicles().expect("published").len(), 2);
    server.abort();
    let _ = server.await;
}

#[test]
fn persists_a_collected_snapshot_and_retries_without_duplication() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let received_at_ms = 1_800_000_000_000;
    let collection = ManualCollection {
        vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({"drive_state": {"timestamp": received_at_ms - 1}}),
        )],
        failures: vec![],
    };

    let first = persist_collection(&store, &collection, received_at_ms).expect("first collection");
    let second = persist_collection(&store, &collection, received_at_ms).expect("retry collection");

    assert_eq!(first.observations_inserted, 1);
    assert_eq!(second.observations_inserted, 0);
    assert_eq!(second.observations_already_present, 1);
    let vehicle_id = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("stored UUID");
    let observations = store
        .observations_for_vehicle(vehicle_id, crate::db::ObservationQuery::from_start(1))
        .expect("stored observation");
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].observed_at_ms, received_at_ms - 1);
    assert_eq!(
        observations[0].payload["record_type"],
        "owner_api_vehicle_data_v1"
    );
    assert_eq!(observations[0].payload["source_vehicle_state"], "online");
}

#[tokio::test]
async fn disabled_geocoder_leaves_pending_jobs_untouched() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("test", "geocoder-disabled"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "vehicle"), 1_000)
        .expect("vehicle");
    store
        .open()
        .expect("database")
        .execute(
            "INSERT INTO address_enrichment_jobs(
                    job_key, vehicle_id, target_type, target_id, field,
                    latitude, longitude, status, attempts, next_attempt_ms,
                    lease_until_ms
                 ) VALUES (?1, ?2, 'drive', 1, 'start_address', 1.0, 2.0,
                           'pending', 0, 0, 0)",
            rusqlite::params!["disabled-geocoder-job", vehicle.vehicle_id.to_string()],
        )
        .expect("address job");
    let config = HubConfig {
        data_dir: temp.path().to_path_buf(),
        bind: "127.0.0.1:8080".parse().expect("bind"),
        tls: None,
        collector: crate::config::CollectorConfig::default(),
        geocoder: crate::config::GeocoderConfig {
            enabled: false,
            ..crate::config::GeocoderConfig::default()
        },
        teslamate: crate::config::TeslaMateConfig::default(),
        terrain: TerrainConfig::default(),
    };

    assert!(
        !run_address_enrichment_once(&store, &config, &CursorKey::from_bytes([7; 32]), &[], 1_000,)
            .await
            .expect("disabled enrichment")
    );
    let status = store
        .open()
        .expect("database")
        .query_row(
            "SELECT status FROM address_enrichment_jobs WHERE job_key = 'disabled-geocoder-job'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("job status");
    assert_eq!(status, "pending");
}

#[test]
fn invalid_or_future_source_times_fall_back_to_receipt_time() {
    let received_at_ms = 1_800_000_000_000;
    for timestamp in [1_i64, received_at_ms + FUTURE_TIMESTAMP_SKEW_MS + 1] {
        let snapshot = VehicleData::for_test(9, json!({"drive_state": {"timestamp": timestamp}}));
        assert_eq!(
            observation_timestamp(&snapshot, received_at_ms),
            received_at_ms
        );
    }
}

#[tokio::test]
async fn supervised_heartbeat_renews_during_idle_and_publishes_auth_recovery() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let lease = store
        .acquire_supervised_collector_lease(current_epoch_millis().expect("clock"))
        .expect("collector lease");
    let initial_heartbeat: i64 = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT heartbeat_at_ms FROM supervised_collector_lease",
            [],
            |row| row.get(0),
        )
        .expect("initial heartbeat");
    let (state, state_rx) = watch::channel(SupervisedCollectorState::Active);
    let (shutdown, stop) = oneshot::channel();
    let task = tokio::spawn(run_supervised_collector_heartbeat(
        store.clone(),
        lease,
        state_rx,
        stop,
        Duration::from_millis(20),
    ));

    // No scheduler work is running in this test. The independent ticker
    // must still advance the durable heartbeat during the idle period.
    tokio::time::sleep(Duration::from_millis(70)).await;
    let renewed_heartbeat: i64 = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT heartbeat_at_ms FROM supervised_collector_lease",
            [],
            |row| row.get(0),
        )
        .expect("renewed heartbeat");
    assert!(renewed_heartbeat > initial_heartbeat);

    state.send_replace(SupervisedCollectorState::AuthenticationTerminal);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let terminal: String = store
        .open()
        .expect("catalogue")
        .query_row("SELECT state FROM supervised_collector_lease", [], |row| {
            row.get(0)
        })
        .expect("terminal state");
    assert_eq!(terminal, "auth_terminal");

    state.send_replace(SupervisedCollectorState::Active);
    tokio::time::sleep(Duration::from_millis(20)).await;
    store
        .service_readiness_at(true, current_epoch_millis().expect("clock"))
        .expect("authenticated success clears terminal readiness");

    shutdown.send(()).expect("heartbeat shutdown");
    task.await
        .expect("heartbeat task")
        .expect("heartbeat result");
    store
        .release_supervised_collector_lease(lease)
        .expect("release lease");
}

#[tokio::test]
async fn supervised_heartbeat_survives_temporary_catalogue_write_rejection() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let lease = store
        .acquire_supervised_collector_lease(current_epoch_millis().expect("clock"))
        .expect("collector lease");
    let initial_heartbeat: i64 = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT heartbeat_at_ms FROM supervised_collector_lease",
            [],
            |row| row.get(0),
        )
        .expect("initial heartbeat");
    let blocker = store.open().expect("catalogue blocker");
    blocker
        .execute_batch(
            "CREATE TRIGGER reject_supervised_collector_heartbeat
                 BEFORE UPDATE OF heartbeat_at_ms ON supervised_collector_lease
                 BEGIN SELECT RAISE(ABORT, 'test heartbeat write rejection'); END;",
        )
        .expect("install temporary write rejection");

    let (_state, state_rx) = watch::channel(SupervisedCollectorState::Active);
    let (shutdown, stop) = oneshot::channel();
    let task = tokio::spawn(run_supervised_collector_heartbeat(
        store.clone(),
        lease,
        state_rx,
        stop,
        Duration::from_millis(20),
    ));
    tokio::time::sleep(Duration::from_millis(70)).await;
    assert!(
        !task.is_finished(),
        "temporary SQLite write failure must not stop collection"
    );

    blocker
        .execute_batch("DROP TRIGGER reject_supervised_collector_heartbeat")
        .expect("clear temporary write rejection");
    tokio::time::sleep(Duration::from_millis(70)).await;
    let renewed_heartbeat: i64 = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT heartbeat_at_ms FROM supervised_collector_lease",
            [],
            |row| row.get(0),
        )
        .expect("renewed heartbeat");
    assert!(renewed_heartbeat > initial_heartbeat);

    shutdown.send(()).expect("heartbeat shutdown");
    task.await
        .expect("heartbeat task")
        .expect("heartbeat result");
    store
        .release_supervised_collector_lease(lease)
        .expect("release lease");
}

#[test]
fn only_typed_terminal_auth_failures_trip_operational_readiness() {
    assert!(is_terminal_auth_failure(
        &CollectorError::SensitiveAccessUnavailable
    ));
    assert!(is_terminal_auth_failure(&CollectorError::OwnerApi(
        OwnerApiError::HttpStatus(401)
    )));
    assert!(!is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
        OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(LegacyAuthError::HttpStatus(
            403
        )))
    )));
    assert!(is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
        OwnerApiAuthError::NotSignedIn
    )));
    assert!(!is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
        OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401))
    )));
    assert!(is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
        OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(403))
    )));
    assert!(!is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
        OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
            LegacyAuthError::InvalidRefreshToken
        ))
    )));
    assert!(!is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
        OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
            LegacyAuthError::RotationOutcomeUnknown
        ))
    )));
    assert!(!is_terminal_auth_failure(&CollectorError::OwnerApi(
        OwnerApiError::RateLimited {
            retry_after_seconds: 60,
        }
    )));
    assert!(!is_terminal_auth_failure(&CollectorError::OwnerApi(
        OwnerApiError::Transport
    )));
}

#[tokio::test]
async fn stream_auth_rejection_fences_later_owner_api_success_until_healthy_stream() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
    let vehicle_id = vehicle.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
    scheduler.accept_discovery(vec![vehicle], Instant::now());
    let (events, receiver) = mpsc::channel(4);
    let (shutdown, stop) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = stop.await;
        Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
    });
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: Some(shutdown),
        task: Some(task),
    }];
    let (state, receiver) = watch::channel(SupervisedCollectorState::Active);
    let mut stream_authentication_rejected = false;

    events
        .send(StreamEvent::AuthRejected)
        .await
        .expect("auth rejection event");
    let transition = drain_stream_events(&store, &mut scheduler, &mut streams)
        .await
        .expect("drain rejection");
    assert_eq!(transition, StreamAuthenticationTransition::Rejected);
    report_stream_authentication_transition(
        &state,
        &mut stream_authentication_rejected,
        transition,
    );

    // A products 200 response says nothing about the Streaming API
    // credential. It must not clear an earlier stream 401/403.
    report_successful_owner_api_request(&state, stream_authentication_rejected);
    assert_eq!(
        *receiver.borrow(),
        SupervisedCollectorState::AuthenticationTerminal
    );

    let telemetry = crate::tesla_stream::parse_data_update(
            r#"{"msg_type":"data:update","tag":"9","value":"1,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#,
        )
        .expect("valid authenticated stream frame");
    events
        .send(StreamEvent::Telemetry {
            update: Box::new(telemetry),
            queued_at: Instant::now(),
        })
        .await
        .expect("authenticated telemetry event");
    let transition = drain_stream_events(&store, &mut scheduler, &mut streams)
        .await
        .expect("drain healthy stream");
    assert_eq!(transition, StreamAuthenticationTransition::Authenticated);
    report_stream_authentication_transition(
        &state,
        &mut stream_authentication_rejected,
        transition,
    );
    assert!(!stream_authentication_rejected);
    assert_eq!(*receiver.borrow(), SupervisedCollectorState::Active);

    stop_and_clear_manual_probe_streams(&mut streams).await;
    assert!(streams.is_empty());
}

#[tokio::test]
async fn production_stream_queue_backpressures_and_drains_without_event_loss() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let vehicle_id = VehicleId::from_test(19);
    let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
    scheduler.accept_discovery(
        vec![Vehicle::for_test(19, "5YJ3E1EA7KF000019", "online")],
        Instant::now(),
    );
    let (sender, receiver) = mpsc::channel(STREAM_EVENT_CHANNEL_CAPACITY);
    let (shutdown, stop) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = stop.await;
        Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
    });
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: Some(shutdown),
        task: Some(task),
    }];
    let first_timestamp = current_epoch_millis().expect("clock")
        - i64::try_from((STREAM_EVENT_CHANNEL_CAPACITY + 2) * 200)
            .expect("production queue duration fits i64");
    let telemetry = |offset: usize| StreamEvent::Telemetry {
        update: Box::new(crate::tesla_stream::StreamUpdate {
            tag: vehicle_id.to_string(),
            timestamp_ms: first_timestamp
                + i64::try_from(offset * 200).expect("telemetry offset fits i64"),
            speed: Some(20),
            odometer: Some(100.0 + offset as f64 / 1_000.0),
            soc: Some(80),
            elevation: Some(25),
            est_heading: Some(180),
            est_lat: Some(51.5),
            est_lng: Some(-0.1),
            power: Some(12),
            shift_state: Some("D".to_owned()),
            range: Some(200),
            est_range: Some(210),
            heading: Some(180),
        }),
        queued_at: Instant::now(),
    };
    for offset in 0..STREAM_EVENT_CHANNEL_CAPACITY {
        sender
            .try_send(telemetry(offset))
            .expect("fill production stream queue");
    }
    let final_sender = sender.clone();
    let final_event = telemetry(STREAM_EVENT_CHANNEL_CAPACITY);
    let mut blocked_sender = tokio::spawn(async move { final_sender.send(final_event).await });
    assert!(
        timeout(Duration::from_millis(20), &mut blocked_sender)
            .await
            .is_err(),
        "the bounded producer must backpressure while the queue is full"
    );

    let mut projection_car_ids = HashMap::new();
    let mut result = drain_stream_events_with_cache(
        &store,
        &mut scheduler,
        &mut streams,
        &mut projection_car_ids,
    )
    .await
    .expect("first bounded stream drain");
    assert!(result.backlog);
    timeout(Duration::from_secs(1), blocked_sender)
        .await
        .expect("backpressured sender resumes after drain")
        .expect("sender task")
        .expect("final event delivery");
    drop(sender);

    let mut drain_turns = 1;
    while result.backlog {
        tokio::task::yield_now().await;
        result = drain_stream_events_with_cache(
            &store,
            &mut scheduler,
            &mut streams,
            &mut projection_car_ids,
        )
        .await
        .expect("prioritized backlog drain");
        assert!(result.terminal_error.is_none());
        drain_turns += 1;
    }
    assert_eq!(
        drain_turns,
        (STREAM_EVENT_CHANNEL_CAPACITY + 1).div_ceil(MAX_STREAM_EVENTS_PER_DRAIN)
    );
    let registered = projection_car_ids[&vehicle_id].registered_vehicle_id;
    let positions: i64 = store
        .open()
        .expect("database")
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows
                 WHERE vehicle_id = ?1 AND domain = 'position'",
            [registered.to_string()],
            |row| row.get(0),
        )
        .expect("stream positions");
    assert_eq!(positions, (STREAM_EVENT_CHANNEL_CAPACITY + 1) as i64);

    stop_and_clear_manual_probe_streams(&mut streams).await;
}

#[test]
fn active_streams_bound_collection_sleep_below_queue_capacity() {
    assert_eq!(collection_sleep_cap(false), CONTROL_SETTINGS_REFRESH);
    assert_eq!(collection_sleep_cap(true), STREAM_EVENT_DRAIN_INTERVAL);
    assert!(STREAM_EVENT_DRAIN_INTERVAL < Duration::from_secs(1));
    const {
        assert!(STREAM_EVENT_CHANNEL_CAPACITY > MAX_STREAM_EVENTS_PER_DRAIN);
    }
}
