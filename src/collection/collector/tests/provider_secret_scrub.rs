// SPDX-License-Identifier: AGPL-3.0-only

// Bearer reflection (csf_2a9d) through the collector: a provider response that
// echoes a live credential into an allowlisted field must not reach the
// stored observation, the current state, a stored car name or a log.

const SCRUB_LEGACY_ACCESS: &str = "old-access-secret";
const SCRUB_LEGACY_REFRESH: &str = "old-refresh-secret";
const SCRUB_FLEET_ACCESS: &str = "scrub-fleet-access-token";
const SCRUB_FLEET_REFRESH: &str = "scrub-fleet-refresh-token";
const SCRUB_FLEET_NEXT_ACCESS: &str = "scrub-fleet-next-access-token";
const SCRUB_FLEET_NEXT_REFRESH: &str = "scrub-fleet-next-refresh-token";
const SCRUB_VIN: &str = "5YJ3E1EA7KF000001";
/// What the collector stores for a vehicle with no provider display name.
const SCRUB_ABSENT_NAME: &str = "Tesla";

fn scrub_reflecting_vehicle_data(vehicle_id: u64, access: &str, refresh: &str) -> String {
    json!({"response": {
        "id": vehicle_id,
        "vehicle_id": vehicle_id,
        "vin": SCRUB_VIN,
        "state": "online",
        "display_name": access,
        "drive_state": {"shift_state": "P", "timestamp": 1_800_000_000_000_i64},
        "charge_state": {"battery_level": 80, "charging_state": "Charging"},
        "vehicle_state": {
            "car_version": format!("build {refresh} end"),
            "vehicle_name": format!("Bearer {access}"),
            "odometer": 1234.5,
            "software_update": {"status": "available", "version": format!(" {refresh}\n")}
        },
        "vehicle_config": {"car_type": "model3"}
    }})
    .to_string()
}

fn scrub_expected_provider_raw_json(vehicle_id: u64) -> serde_json::Value {
    json!({"response": {
        "id": vehicle_id,
        "vehicle_id": vehicle_id,
        "vin": SCRUB_VIN,
        "state": "online",
        "display_name": null,
        "drive_state": {"shift_state": "P", "timestamp": 1_800_000_000_000_i64},
        "charge_state": {"battery_level": 80, "charging_state": "Charging"},
        "vehicle_state": {
            "car_version": null,
            "vehicle_name": null,
            "odometer": 1234.5,
            "software_update": {"status": "available", "version": null}
        },
        "vehicle_config": {"car_type": "model3"}
    }})
}

/// The raw SQLite bytes (database and write-ahead log) contain `needle`.
fn scrub_database_bytes_contain(store: &HubStore, needle: &str) -> bool {
    let database = store.database_path().to_path_buf();
    let mut wal = database.clone().into_os_string();
    wal.push("-wal");
    [database, std::path::PathBuf::from(wal)]
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .any(|bytes| {
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        })
}

struct ScrubPersisted {
    _temp: tempfile::TempDir,
    store: HubStore,
    vehicle_id: Uuid,
}

fn scrub_persist(
    store: HubStore,
    temp: tempfile::TempDir,
    provider: CollectorProvider,
    vehicles: Vec<Vehicle>,
    snapshot: VehicleData,
) -> ScrubPersisted {
    persist_collection_atomic_for_provider(
        &store,
        &ManualCollection {
            vehicles,
            snapshots: vec![snapshot],
            failures: vec![],
        },
        1_800_000_000_001,
        provider,
    )
    .expect("collection persists");
    let vehicle_id = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("vehicle UUID");
    ScrubPersisted {
        _temp: temp,
        store,
        vehicle_id,
    }
}

fn scrub_stored_vehicle_name(persisted: &ScrubPersisted) -> Option<String> {
    persisted
        .store
        .open()
        .expect("database")
        .query_row("SELECT display_name FROM vehicles", [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .expect("stored vehicle name")
}

/// Everything a reader of the Hub could be shown for the vehicle: the stored
/// observation, the current summary built from it and the raw database.
fn scrub_assert_nothing_stored_or_served(
    persisted: &ScrubPersisted,
    record_type: &str,
    vehicle_id_in_json: u64,
    secrets: &[&str],
) {
    let observations = persisted
        .store
        .current_observations_for_vehicle(persisted.vehicle_id)
        .expect("current observations");
    let observation = observations
        .iter()
        .find(|observation| observation.payload["record_type"] == record_type)
        .expect("provider vehicle-data observation");
    assert_eq!(
        observation.payload["provider_raw_json"],
        scrub_expected_provider_raw_json(vehicle_id_in_json),
        "reflected fields are null and every other field is intact"
    );
    let stored = serde_json::to_string(&observation.payload).expect("payload JSON");
    let summary = crate::current_state::build_current_vehicle_summary(
        persisted.vehicle_id,
        &observations,
        None,
        None,
        None,
    );
    assert_eq!(summary.battery_level, Some(80));
    assert_eq!(summary.charging_state.as_deref(), Some("Charging"));
    assert_eq!(summary.display_name, None);
    assert_eq!(summary.version, None);
    assert_eq!(summary.update_version, None);
    assert_eq!(summary.update_status.as_deref(), Some("available"));
    let served = serde_json::to_string(&summary).expect("summary JSON");
    for secret in secrets {
        assert!(
            !stored.contains(secret),
            "credential bytes in the stored observation"
        );
        assert!(
            !served.contains(secret),
            "credential bytes in the current summary"
        );
        assert!(
            !scrub_database_bytes_contain(&persisted.store, secret),
            "credential bytes in the database file"
        );
    }
}

#[tokio::test]
async fn legacy_collection_stores_and_serves_no_reflected_credential() {
    let log = crate::owner_api::provider_secrets::capture::LogCapture::start();
    let products = format!(
        r#"{{"response":[{{"vehicle_id":9,"id":9,"vin":"{SCRUB_VIN}","state":"online","display_name":"Bearer {SCRUB_LEGACY_ACCESS}"}}],"count":1}}"#
    );
    let data = scrub_reflecting_vehicle_data(9, SCRUB_LEGACY_ACCESS, SCRUB_LEGACY_REFRESH);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = Router::new()
        .route("/api/1/products", get(move || async move { products }))
        .route(
            "/api/1/vehicles/{vehicle_id}/vehicle_data",
            get(move || async move { data }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Owner");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
    let client = OwnerApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("client");
    let auth = coordinated_test_legacy_auth(base);

    let vehicles = list_vehicles_for_auth(&client, &auth)
        .await
        .expect("discovery");
    let snapshot = vehicle_data_for_auth(&client, &auth, VehicleId::from_test(9), None)
        .await
        .expect("vehicle data");
    shutdown_legacy_refresh(&auth).await;
    server.abort();

    assert_eq!(vehicles.len(), 1);
    assert_eq!(vehicles[0].display_name, None);
    assert_eq!(vehicles[0].vin, SCRUB_VIN);
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let persisted = scrub_persist(store, temp, CollectorProvider::Legacy, vehicles, snapshot);

    assert_eq!(
        scrub_stored_vehicle_name(&persisted).as_deref(),
        Some(SCRUB_ABSENT_NAME),
        "a dropped provider name is stored as an absent name"
    );
    scrub_assert_nothing_stored_or_served(
        &persisted,
        "owner_api_vehicle_data_v1",
        9,
        &[SCRUB_LEGACY_ACCESS, SCRUB_LEGACY_REFRESH],
    );
    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 2, "{log}");
    for secret in [SCRUB_LEGACY_ACCESS, SCRUB_LEGACY_REFRESH] {
        assert!(!log.contains(secret), "credential bytes in the log");
    }
}

#[test]
fn unscrubbed_reflection_would_be_stored_so_the_database_scan_is_meaningful() {
    // Positive control for the scans above: with no live secret in scope the
    // same response is stored verbatim and the raw scan finds the bytes.
    let raw: serde_json::Value = serde_json::from_str(&scrub_reflecting_vehicle_data(
        9,
        SCRUB_LEGACY_ACCESS,
        SCRUB_LEGACY_REFRESH,
    ))
    .expect("fixture JSON");
    let snapshot =
        VehicleData::from_provider_raw_json(VehicleId::from_test(9), raw, &ProviderSecrets::none())
            .expect("response parses");
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let persisted = scrub_persist(
        store,
        temp,
        CollectorProvider::Legacy,
        vec![Vehicle::for_test(9, SCRUB_VIN, "online")],
        snapshot,
    );

    for secret in [SCRUB_LEGACY_ACCESS, SCRUB_LEGACY_REFRESH] {
        assert!(scrub_database_bytes_contain(&persisted.store, secret));
    }
    // The vehicle-data display_name also becomes the stored car name, which
    // is why the scrub must run before persistence.
    assert_eq!(
        scrub_stored_vehicle_name(&persisted).as_deref(),
        Some(SCRUB_LEGACY_ACCESS)
    );
}

async fn scrub_fleet_fixture(
    temp: &std::path::Path,
    store: &HubStore,
) -> Arc<tokio::sync::Mutex<FleetAuthManager>> {
    let credentials = FleetSetupCredentials::new(
        SCRUB_FLEET_ACCESS.to_owned(),
        SCRUB_FLEET_REFRESH.to_owned(),
        "scrub-fleet-client".to_owned(),
        crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("Fleet credentials");
    crate::fleet_credentials::persist_fleet_setup_credentials(
        store,
        temp,
        &credentials,
        SystemTime::now(),
    )
    .expect("persist Fleet credentials");
    Arc::new(tokio::sync::Mutex::new(
        FleetAuthManager::from_store(store.clone(), temp).expect("Fleet manager"),
    ))
}

#[tokio::test]
async fn fleet_collection_stores_and_serves_no_reflected_credential() {
    let log = crate::owner_api::provider_secrets::capture::LogCapture::start();
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let manager = scrub_fleet_fixture(temp.path(), &store).await;
    let vehicles_body = format!(
        r#"{{"response":[{{"id":70,"vehicle_id":71,"vin":"{SCRUB_VIN}","state":"online","display_name":"{SCRUB_FLEET_ACCESS}"}}],"count":1}}"#
    );
    let data = scrub_reflecting_vehicle_data(70, SCRUB_FLEET_ACCESS, SCRUB_FLEET_REFRESH);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = Router::new()
        .route("/api/1/vehicles", get(move || async move { vehicles_body }))
        .route(
            "/api/1/vehicles/{vin}/vehicle_data",
            get(move || async move { data }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Fleet");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
    let api = FleetApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("Fleet client");
    let auth_api = FleetAuthApi::for_fake_http(
        base.join("oauth2/v3/token").expect("token URL"),
        Duration::from_secs(2),
    )
    .expect("Fleet auth client");

    let vehicles = fleet_list_vehicles_with_auth(&api, &auth_api, &manager, false)
        .await
        .expect("discovery");
    let snapshot = fleet_vehicle_data_with_auth(&api, &auth_api, &manager, &vehicles[0], false)
        .await
        .expect("vehicle data");
    server.abort();

    assert_eq!(vehicles.len(), 1);
    assert_eq!(vehicles[0].display_name, None);
    let persisted = scrub_persist(store, temp, CollectorProvider::Fleet, vehicles, snapshot);

    assert_eq!(
        scrub_stored_vehicle_name(&persisted).as_deref(),
        Some(SCRUB_ABSENT_NAME),
        "a dropped provider name is stored as an absent name"
    );
    // The refresh token reaches the scrub from the terrain wiring: it is
    // echoed only inside longer text and a padded value.
    scrub_assert_nothing_stored_or_served(
        &persisted,
        "fleet_api_vehicle_data_v1",
        70,
        &[SCRUB_FLEET_ACCESS, SCRUB_FLEET_REFRESH],
    );
    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 2, "{log}");
    for secret in [SCRUB_FLEET_ACCESS, SCRUB_FLEET_REFRESH] {
        assert!(!log.contains(secret), "credential bytes in the log");
    }
}

#[tokio::test]
async fn fleet_retry_after_refresh_scrubs_against_the_refreshed_pair() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let manager = scrub_fleet_fixture(temp.path(), &store).await;
    // The first vehicle-data request is rejected; the refreshed pair is then
    // echoed by the retry, and the old pair is no longer live.
    let data = scrub_reflecting_vehicle_data(70, SCRUB_FLEET_NEXT_ACCESS, SCRUB_FLEET_NEXT_REFRESH);
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    let token_body = json!({
        "access_token": SCRUB_FLEET_NEXT_ACCESS,
        "refresh_token": SCRUB_FLEET_NEXT_REFRESH,
        "expires_in": 28_800,
        "token_type": "Bearer"
    })
    .to_string();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = Router::new()
        .route(
            "/api/1/vehicles/{vin}/vehicle_data",
            get(move || {
                let counted = Arc::clone(&counted);
                let data = data.clone();
                async move {
                    if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                        (StatusCode::UNAUTHORIZED, "{}".to_owned()).into_response()
                    } else {
                        (StatusCode::OK, data).into_response()
                    }
                }
            }),
        )
        .route("/oauth2/v3/token", post(move || async move { token_body }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Fleet");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
    let api = FleetApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("Fleet client");
    let auth_api = FleetAuthApi::for_fake_http(
        base.join("oauth2/v3/token").expect("token URL"),
        Duration::from_secs(2),
    )
    .expect("Fleet auth client");

    let snapshot = fleet_vehicle_data_with_auth(
        &api,
        &auth_api,
        &manager,
        &Vehicle::for_test(70, SCRUB_VIN, "online"),
        true,
    )
    .await
    .expect("retried vehicle data");
    server.abort();

    assert_eq!(requests.load(Ordering::SeqCst), 2);
    let persisted = scrub_persist(
        store,
        temp,
        CollectorProvider::Fleet,
        vec![Vehicle::for_test(70, SCRUB_VIN, "online")],
        snapshot,
    );
    scrub_assert_nothing_stored_or_served(
        &persisted,
        "fleet_api_vehicle_data_v1",
        70,
        &[SCRUB_FLEET_NEXT_ACCESS, SCRUB_FLEET_NEXT_REFRESH],
    );
}

// A short credential that carries a space of its own (validation keeps it as
// given) is echoed with that space, so trimming the echo is not enough to
// recognise it.
const SCRUB_PADDED_ACCESS: &str = "pad-access ";
const SCRUB_PADDED_REFRESH: &str = " pad-refresh";
const SCRUB_PADDED_VERSION: &str = "2026.20.1 abc123";

fn scrub_padded_vehicle_data(vehicle_id: u64) -> String {
    json!({"response": {
        "id": vehicle_id,
        "vehicle_id": vehicle_id,
        "vin": SCRUB_VIN,
        "state": "online",
        "display_name": SCRUB_PADDED_ACCESS,
        "drive_state": {"shift_state": "P", "timestamp": 1_800_000_000_000_i64},
        "charge_state": {"battery_level": 80, "charging_state": "Charging"},
        "vehicle_state": {
            "car_version": SCRUB_PADDED_VERSION,
            "vehicle_name": format!("Bearer {SCRUB_PADDED_ACCESS}"),
            "odometer": 1234.5,
            "software_update": {"status": "available", "version": SCRUB_PADDED_REFRESH}
        },
        "vehicle_config": {"car_type": "model3"}
    }})
    .to_string()
}

fn scrub_padded_expected_provider_raw_json(vehicle_id: u64) -> serde_json::Value {
    // The three reflected fields are null; the car version is not a reflection.
    let mut expected = scrub_expected_provider_raw_json(vehicle_id);
    expected["response"]["vehicle_state"]["car_version"] = json!(SCRUB_PADDED_VERSION);
    expected
}

fn scrub_padded_legacy_auth(issuer: url::Url) -> CollectionAuth {
    let auth =
        crate::legacy_auth::LegacyAuth::for_test(issuer, SCRUB_PADDED_ACCESS, SCRUB_PADDED_REFRESH)
            .with_test_schedule(2_000_000_000, 1_900_000_000);
    let manager = LegacyAuthManager::for_test_with_active_pair(auth).expect("active test pair");
    CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    }
}

#[tokio::test]
async fn legacy_collection_stores_no_credential_that_carries_a_space_of_its_own() {
    let log = crate::owner_api::provider_secrets::capture::LogCapture::start();
    let products = format!(
        r#"{{"response":[{{"vehicle_id":9,"id":9,"vin":"{SCRUB_VIN}","state":"online","display_name":"{SCRUB_PADDED_ACCESS}"}}],"count":1}}"#
    );
    let data = scrub_padded_vehicle_data(9);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = Router::new()
        .route("/api/1/products", get(move || async move { products }))
        .route(
            "/api/1/vehicles/{vehicle_id}/vehicle_data",
            get(move || async move { data }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Owner");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
    let client = OwnerApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("client");
    let auth = scrub_padded_legacy_auth(base);

    let vehicles = list_vehicles_for_auth(&client, &auth)
        .await
        .expect("discovery");
    let snapshot = vehicle_data_for_auth(&client, &auth, VehicleId::from_test(9), None)
        .await
        .expect("vehicle data");
    shutdown_legacy_refresh(&auth).await;
    server.abort();

    assert_eq!(vehicles.len(), 1);
    assert_eq!(vehicles[0].display_name, None);
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let persisted = scrub_persist(store, temp, CollectorProvider::Legacy, vehicles, snapshot);

    assert_eq!(
        scrub_stored_vehicle_name(&persisted).as_deref(),
        Some(SCRUB_ABSENT_NAME),
        "a dropped provider name is stored as an absent name"
    );
    let observations = persisted
        .store
        .current_observations_for_vehicle(persisted.vehicle_id)
        .expect("current observations");
    let observation = observations
        .iter()
        .find(|observation| observation.payload["record_type"] == "owner_api_vehicle_data_v1")
        .expect("provider vehicle-data observation");
    assert_eq!(
        observation.payload["provider_raw_json"],
        scrub_padded_expected_provider_raw_json(9)
    );
    let stored = serde_json::to_string(&observation.payload).expect("payload JSON");
    for secret in [SCRUB_PADDED_ACCESS, SCRUB_PADDED_REFRESH] {
        assert!(
            !stored.contains(secret),
            "credential bytes in the stored observation"
        );
        assert!(
            !scrub_database_bytes_contain(&persisted.store, secret),
            "credential bytes in the database file"
        );
    }
    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 2, "{log}");
    for secret in [SCRUB_PADDED_ACCESS, SCRUB_PADDED_REFRESH] {
        assert!(!log.contains(secret), "credential bytes in the log");
    }
}

#[test]
fn unscrubbed_padded_reflection_would_be_stored_so_the_padded_scan_is_meaningful() {
    let raw: serde_json::Value =
        serde_json::from_str(&scrub_padded_vehicle_data(9)).expect("fixture JSON");
    let snapshot =
        VehicleData::from_provider_raw_json(VehicleId::from_test(9), raw, &ProviderSecrets::none())
            .expect("response parses");
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let persisted = scrub_persist(
        store,
        temp,
        CollectorProvider::Legacy,
        vec![Vehicle::for_test(9, SCRUB_VIN, "online")],
        snapshot,
    );

    for secret in [SCRUB_PADDED_ACCESS, SCRUB_PADDED_REFRESH] {
        assert!(scrub_database_bytes_contain(&persisted.store, secret));
    }
    // The store keeps the name without its padding, which is why the scrub
    // cannot rely on the stored form to find the token again.
    assert_eq!(
        scrub_stored_vehicle_name(&persisted).as_deref(),
        Some(SCRUB_PADDED_ACCESS.trim())
    );
}

// A provider `state` is stored as the source vehicle state and served as the
// current state, so a credential echoed into it, on either vehicle list or in
// the Legacy state probe, becomes the Hub's own unknown state before anything
// is persisted.

const SCRUB_UNKNOWN_STATE: &str = "unknown";
const SCRUB_VIN_2: &str = "5YJ3E1EA7KF000002";
const SCRUB_VIN_3: &str = "5YJ3E1EA7KF000003";

fn scrub_discovered_vehicle(store: &HubStore, source_vehicle_key: &str) -> Uuid {
    store
        .open()
        .expect("database")
        .query_row(
            "SELECT vehicle_id FROM vehicles WHERE source_vehicle_key = ?1",
            [source_vehicle_key],
            |row| row.get::<_, String>(0),
        )
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("vehicle UUID")
}

/// Everything a reader of the Hub could be shown for a discovered vehicle: the
/// stored discovery observation (its source vehicle state is `expected`), the
/// current summary built from it (its state is `expected_current`, which is the
/// vehicle's open lifecycle state when one is current) and the raw database.
fn scrub_assert_discovered_state(
    store: &HubStore,
    source_vehicle_key: &str,
    provider: CollectorProvider,
    expected: &str,
    expected_current: &str,
    secrets: &[&str],
) {
    let vehicle_id = scrub_discovered_vehicle(store, source_vehicle_key);
    let observations = store
        .current_observations_for_vehicle(vehicle_id)
        .expect("current observations");
    let record_type = provider_discovery_record_type(provider);
    let discovery = observations
        .iter()
        .find(|observation| observation.payload["record_type"] == record_type)
        .expect("discovery observation");
    assert_eq!(
        discovery.payload["source_vehicle_state"], expected,
        "stored source vehicle state of {source_vehicle_key}"
    );
    let lifecycle = store.load_lifecycle_state(vehicle_id).expect("lifecycle");
    let summary = crate::current_state::build_current_vehicle_summary(
        vehicle_id,
        &observations,
        None,
        lifecycle.as_ref(),
        None,
    );
    assert_eq!(
        summary.state.as_deref(),
        Some(expected_current),
        "current state of {source_vehicle_key}"
    );
    let stored = serde_json::to_string(&discovery.payload).expect("payload JSON");
    let served = serde_json::to_string(&summary).expect("summary JSON");
    for secret in secrets {
        assert!(
            !stored.contains(secret),
            "credential bytes in the stored observation"
        );
        assert!(
            !served.contains(secret),
            "credential bytes in the current summary"
        );
        assert!(
            !scrub_database_bytes_contain(store, secret),
            "credential bytes in the database file"
        );
    }
}

fn scrub_state_products(access: &str, refresh: &str) -> String {
    json!({"response": [
        {"vehicle_id": 9, "id": 9, "vin": SCRUB_VIN, "state": access, "display_name": "Athena"},
        {"vehicle_id": 10, "id": 10, "vin": SCRUB_VIN_2, "state": format!("x-{refresh}-y")},
        {"vehicle_id": 11, "id": 11, "vin": SCRUB_VIN_3, "state": "asleep", "display_name": "Apollo"}
    ], "count": 3})
    .to_string()
}

#[tokio::test]
async fn legacy_list_state_that_reflects_a_credential_is_stored_and_served_as_unknown() {
    let log = crate::owner_api::provider_secrets::capture::LogCapture::start();
    let products = scrub_state_products(SCRUB_LEGACY_ACCESS, SCRUB_LEGACY_REFRESH);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = Router::new().route("/api/1/products", get(move || async move { products }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Owner");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
    let client = OwnerApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("client");
    let auth = coordinated_test_legacy_auth(base);

    let vehicles = list_vehicles_for_auth(&client, &auth)
        .await
        .expect("discovery");
    shutdown_legacy_refresh(&auth).await;
    server.abort();

    assert_eq!(
        vehicles
            .iter()
            .map(|vehicle| vehicle.state.as_str())
            .collect::<Vec<_>>(),
        vec![SCRUB_UNKNOWN_STATE, SCRUB_UNKNOWN_STATE, "asleep"]
    );
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    persist_unchanged_discoveries(&store, &vehicles, CollectorProvider::Legacy)
        .await
        .expect("discoveries persist");

    let secrets = [SCRUB_LEGACY_ACCESS, SCRUB_LEGACY_REFRESH];
    for (key, expected) in [
        ("9", SCRUB_UNKNOWN_STATE),
        ("10", SCRUB_UNKNOWN_STATE),
        ("11", "asleep"),
    ] {
        scrub_assert_discovered_state(
            &store,
            key,
            CollectorProvider::Legacy,
            expected,
            expected,
            &secrets,
        );
    }
    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(log.contains("response[].state"), "{log}");
    for secret in secrets {
        assert!(!log.contains(secret), "credential bytes in the log");
    }
}

#[tokio::test]
async fn fleet_list_state_that_reflects_a_credential_is_stored_and_served_as_unknown() {
    let log = crate::owner_api::provider_secrets::capture::LogCapture::start();
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let manager = scrub_fleet_fixture(temp.path(), &store).await;
    let vehicles_body = json!({"response": [
        {"id": 70, "vehicle_id": 71, "vin": SCRUB_VIN, "state": SCRUB_FLEET_ACCESS, "display_name": "Athena"},
        {"id": 80, "vehicle_id": 81, "vin": SCRUB_VIN_2, "state": format!("x-{SCRUB_FLEET_REFRESH}-y")},
        {"id": 90, "vehicle_id": 91, "vin": SCRUB_VIN_3, "state": "asleep", "display_name": "Apollo"}
    ], "count": 3})
    .to_string();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = Router::new().route("/api/1/vehicles", get(move || async move { vehicles_body }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Fleet");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
    let api = FleetApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("Fleet client");
    let auth_api = FleetAuthApi::for_fake_http(
        base.join("oauth2/v3/token").expect("token URL"),
        Duration::from_secs(2),
    )
    .expect("Fleet auth client");

    let vehicles = fleet_list_vehicles_with_auth(&api, &auth_api, &manager, false)
        .await
        .expect("discovery");
    server.abort();

    assert_eq!(
        vehicles
            .iter()
            .map(|vehicle| vehicle.state.as_str())
            .collect::<Vec<_>>(),
        vec![SCRUB_UNKNOWN_STATE, SCRUB_UNKNOWN_STATE, "asleep"]
    );
    persist_unchanged_discoveries(&store, &vehicles, CollectorProvider::Fleet)
        .await
        .expect("discoveries persist");

    let secrets = [SCRUB_FLEET_ACCESS, SCRUB_FLEET_REFRESH];
    for (key, expected) in [
        ("70", SCRUB_UNKNOWN_STATE),
        ("80", SCRUB_UNKNOWN_STATE),
        ("90", "asleep"),
    ] {
        scrub_assert_discovered_state(
            &store,
            key,
            CollectorProvider::Fleet,
            expected,
            expected,
            &secrets,
        );
    }
    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(log.contains("response[].state"), "{log}");
    for secret in secrets {
        assert!(!log.contains(secret), "credential bytes in the log");
    }
}

#[tokio::test]
async fn legacy_state_probe_that_reflects_a_credential_is_stored_and_served_as_unknown() {
    let log = crate::owner_api::provider_secrets::capture::LogCapture::start();
    let probe = json!({"response": {"id": 9, "state": SCRUB_LEGACY_REFRESH}}).to_string();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = Router::new().route(
        "/api/1/vehicles/{vehicle_id}",
        get(move || async move { probe }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Owner");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
    let client = OwnerApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("client");
    let auth = coordinated_test_legacy_auth(base);
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([4; 32]);

    // The vehicle was discovered asleep and persisted; the offline-state probe
    // then asks the provider for its state, as the supervised loop does.
    let now = Instant::now();
    let asleep = Vehicle::for_test(9, SCRUB_VIN, "asleep");
    let id = asleep.id;
    let mut scheduler = VehicleScheduler::new(test_cadence(), now);
    scheduler.accept_discovery(vec![asleep.clone()], now);
    persist_discovery_events(&store, &cursor_key, &[asleep])
        .await
        .expect("asleep discovery persists");
    scrub_assert_discovered_state(&store, "9", CollectorProvider::Legacy, "asleep", "asleep", &[]);

    let state = vehicle_state_for_auth(&client, &auth, id)
        .await
        .expect("state probe");
    shutdown_legacy_refresh(&auth).await;
    server.abort();
    assert_eq!(state, SCRUB_UNKNOWN_STATE);
    let events = scheduler.accept_vehicle_state(id, state, now);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].state, SCRUB_UNKNOWN_STATE);
    persist_discovery_events(&store, &cursor_key, &events)
        .await
        .expect("probed state persists");

    // The stored observation carries the Hub's unknown state. The vehicle's open
    // lifecycle state stays what it was: an unknown state is no information, so
    // the current summary still says asleep, and the credential is nowhere.
    scrub_assert_discovered_state(
        &store,
        "9",
        CollectorProvider::Legacy,
        SCRUB_UNKNOWN_STATE,
        "asleep",
        &[SCRUB_LEGACY_ACCESS, SCRUB_LEGACY_REFRESH],
    );
    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(log.contains("response.state"), "{log}");
    for secret in [SCRUB_LEGACY_ACCESS, SCRUB_LEGACY_REFRESH] {
        assert!(!log.contains(secret), "credential bytes in the log");
    }
}

#[tokio::test]
async fn unscrubbed_state_reflection_would_be_stored_so_the_state_scan_is_meaningful() {
    // Positive control for the scans above: a provider state that is a live
    // credential and reaches the store unscrubbed is persisted, served and
    // found in the raw database.
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let vehicle = Vehicle::for_test(9, SCRUB_VIN, SCRUB_LEGACY_ACCESS);
    persist_unchanged_discoveries(&store, &[vehicle], CollectorProvider::Legacy)
        .await
        .expect("discovery persists");

    let vehicle_id = scrub_discovered_vehicle(&store, "9");
    let observations = store
        .current_observations_for_vehicle(vehicle_id)
        .expect("current observations");
    let discovery = observations
        .iter()
        .find(|observation| {
            observation.payload["record_type"]
                == provider_discovery_record_type(CollectorProvider::Legacy)
        })
        .expect("discovery observation");
    assert_eq!(
        discovery.payload["source_vehicle_state"],
        SCRUB_LEGACY_ACCESS
    );
    let summary = crate::current_state::build_current_vehicle_summary(
        vehicle_id,
        &observations,
        None,
        None,
        None,
    );
    assert_eq!(summary.state.as_deref(), Some(SCRUB_LEGACY_ACCESS));
    assert!(scrub_database_bytes_contain(&store, SCRUB_LEGACY_ACCESS));
}
