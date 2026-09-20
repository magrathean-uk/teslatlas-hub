// SPDX-License-Identifier: AGPL-3.0-only

#[path = "interop/seed.rs"]
mod seed;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use serde_json::Value;
use teslatlas_hub::{
    db::{HubStore, VehicleDescriptor},
    server::paired_router,
};
use tower::ServiceExt;

#[test]
fn standard_fixtures_publish_matching_schema_22_pairs_and_keep_query_rows() {
    for (name, viewer_r1, expected_drives) in [("b1", false, 5_i64), ("viewer-r1", true, 51_i64)] {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join(name);
        let prepared = if viewer_r1 {
            seed::prepare_viewer_r1(&root, 18443).unwrap()
        } else {
            seed::prepare(&root, 18443).unwrap()
        };
        let store = HubStore::initialize(root.join("hub")).unwrap();
        let key =
            teslatlas_hub::teslamate_credentials::load_or_create_cursor_key(&root.join("hub"))
                .unwrap();

        for vehicle_id in prepared.vehicle_ids {
            let (manifest_bytes, noop_bytes) =
                teslatlas_hub::updates_delivery::schema_22_signed_artifacts(
                    &store, vehicle_id, &key,
                )
                .unwrap();
            let manifest: teslatlas_hub::protocol::SyncManifest =
                serde_json::from_slice(&manifest_bytes).unwrap();
            let noop: teslatlas_hub::updates_delivery::SignedNoOpState =
                serde_json::from_slice(&noop_bytes).unwrap();
            assert_eq!(
                manifest.schema,
                teslatlas_hub::protocol::HUB_PROJECTION_SCHEMA_V3
            );
            assert_eq!(noop.projection_schema, "2.2");
            assert_eq!(noop.vehicle_id, vehicle_id);
            assert_eq!(noop.snapshot_id, manifest.snapshot_id);
            assert_eq!(noop.head_sequence, manifest.head_sequence);
            assert_eq!(noop.pack_sha256, manifest.chunks[0].sha256.to_string());
            assert!(
                store
                    .pack_for_digest(manifest.chunks[0].sha256)
                    .unwrap()
                    .is_some()
            );
        }

        let connection = store.open().unwrap();
        let drive_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM materialised_drives", [], |row| {
                row.get(0)
            })
            .unwrap();
        let charge_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM materialised_charges", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(drive_count, expected_drives, "{name} query drives");
        assert_eq!(charge_count, 1, "{name} query charges");
    }
}

#[cfg(feature = "interop-fixture")]
#[test]
fn dynamic_fixture_exposure_repairs_registered_but_unpublished_vehicle() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("partial-dynamic-vehicle");
    let prepared = seed::prepare(&root, 18443).unwrap();
    let store = HubStore::initialize(root.join("hub")).unwrap();
    let vehicle_id = uuid::Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
    let mut descriptor =
        VehicleDescriptor::new(prepared.source_id, "11").with_tesla_identity(Some(11), None);
    descriptor.vin = Some("5YJ3E1EA7KF000003".into());
    descriptor.display_name = Some("Interop dynamic third".into());
    store
        .register_vehicle_with_id(&descriptor, 1_788_566_400_000, vehicle_id)
        .unwrap();
    assert!(store.manifest_for_vehicle(vehicle_id).unwrap().is_none());
    drop(store);

    let repaired = seed::expose_dynamic_vehicle(&root).unwrap();
    assert_eq!(repaired.status, "repaired");
    assert_eq!(repaired.vehicle_id, vehicle_id);
    let reopened = HubStore::initialize(root.join("hub")).unwrap();
    let key =
        teslatlas_hub::teslamate_credentials::load_or_create_cursor_key(&root.join("hub")).unwrap();
    teslatlas_hub::updates_delivery::schema_22_signed_artifacts(&reopened, vehicle_id, &key)
        .unwrap();
    assert_eq!(reopened.published_vehicles().unwrap().len(), 3);
    assert_eq!(
        seed::expose_dynamic_vehicle(&root).unwrap().status,
        "already-exposed"
    );
}

#[cfg(feature = "interop-fixture")]
#[tokio::test]
async fn dynamic_fixture_vehicle_retires_and_returns_with_stable_signed_state() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("dynamic-vehicle");
    let prepared = seed::prepare(&root, 18443).unwrap();
    let exposed = seed::expose_dynamic_vehicle(&root).unwrap();
    assert_eq!(exposed.status, "exposed");
    assert_eq!(
        exposed.vehicle_id.to_string(),
        "33333333-3333-4333-8333-333333333333"
    );
    let key =
        teslatlas_hub::teslamate_credentials::load_or_create_cursor_key(&root.join("hub")).unwrap();
    let store = HubStore::initialize(root.join("hub")).unwrap();
    assert_eq!(store.published_vehicles().unwrap().len(), 3);
    let signed_before = teslatlas_hub::updates_delivery::schema_22_signed_artifacts(
        &store,
        exposed.vehicle_id,
        &key,
    )
    .unwrap();
    let manifest: teslatlas_hub::protocol::SyncManifest =
        serde_json::from_slice(&signed_before.0).unwrap();
    let digest = manifest.chunks[0].sha256;

    let retired = seed::retire_dynamic_vehicle(&root).unwrap();
    assert_eq!(retired.status, "retired");
    assert_eq!(
        seed::retire_dynamic_vehicle(&root).unwrap().status,
        "already-retired"
    );
    let restarted = HubStore::initialize(root.join("hub")).unwrap();
    assert!(!restarted.vehicle_is_active(exposed.vehicle_id).unwrap());
    assert_eq!(restarted.published_vehicles().unwrap().len(), 2);
    assert_eq!(
        teslatlas_hub::updates_delivery::schema_22_signed_artifacts(
            &restarted,
            exposed.vehicle_id,
            &key,
        )
        .unwrap(),
        signed_before
    );

    let invitation: Value =
        serde_json::from_slice(&std::fs::read(&prepared.invitation_path).unwrap()).unwrap();
    let app = paired_router(restarted, &key);
    let claim = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/v1/pairings/{}/claim",
                    invitation["pairing_id"].as_str().unwrap()
                ))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "secret": invitation["secret"],
                        "device_name": "dynamic-fixture-test"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(claim.status(), 200);
    let claim: Value =
        serde_json::from_slice(&claim.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let token = claim["access_token"].as_str().unwrap();
    assert_eq!(
        get(&app, "/v1/vehicles", token).await["vehicles"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for route in [
        format!("/v1/vehicles/{}/current", exposed.vehicle_id),
        format!("/v1/vehicles/{}/drives", exposed.vehicle_id),
        format!("/v1/vehicles/{}/sync/manifest", exposed.vehicle_id),
        format!("/v1/vehicles/{}/sync/noop", exposed.vehicle_id),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(route)
                    .header("authorization", format!("Bearer {token}"))
                    .header("x-teslatlas-supported-schemas", "2.2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
    }
    let pack = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{digest}.sqlite.zst"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        pack.status(),
        200,
        "digest-addressed pack remains authorized"
    );

    let restored = seed::restore_dynamic_vehicle(&root).unwrap();
    assert_eq!(restored.status, "restored");
    assert_eq!(restored.vehicle_id, exposed.vehicle_id);
    let restarted = HubStore::initialize(root.join("hub")).unwrap();
    assert!(restarted.vehicle_is_active(exposed.vehicle_id).unwrap());
    assert_eq!(restarted.published_vehicles().unwrap().len(), 3);
    assert_eq!(
        teslatlas_hub::updates_delivery::schema_22_signed_artifacts(
            &restarted,
            exposed.vehicle_id,
            &key,
        )
        .unwrap(),
        signed_before
    );
    assert_eq!(
        seed::expose_dynamic_vehicle(&root).unwrap().status,
        "already-exposed"
    );
    for route in [
        format!("/v1/vehicles/{}/current", exposed.vehicle_id),
        format!("/v1/vehicles/{}/drives", exposed.vehicle_id),
        format!("/v1/vehicles/{}/sync/manifest", exposed.vehicle_id),
        format!("/v1/vehicles/{}/sync/noop", exposed.vehicle_id),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(route)
                    .header("authorization", format!("Bearer {token}"))
                    .header("x-teslatlas-supported-schemas", "2.2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn standard_fixture_is_admitted_for_fixture_source_run_not_production() {
    use std::ffi::OsStr;
    use teslatlas_hub::{
        config::HubConfig,
        macos_launch_agent::{self, DevelopmentServeMode},
    };

    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("development-serve");
    let prepared = seed::prepare(&root, 21444).unwrap();
    let config = HubConfig::load(&prepared.config_path).unwrap();

    assert_eq!(
        macos_launch_agent::development_serve_mode(None, None).unwrap(),
        None
    );
    assert!(
        macos_launch_agent::development_serve_mode(Some(OsStr::new("true")), None).is_err()
    );
    assert!(macos_launch_agent::preflight_hub_for_serve(&config, None).is_err());
    macos_launch_agent::preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Fixture))
        .expect("standard fixture development Serve");
    macos_launch_agent::preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone))
        .expect("collector-disabled fixture is also a valid standalone source-run");

    let mut exposed = config.clone();
    exposed.bind = "0.0.0.0:21444".parse().unwrap();
    assert!(
        macos_launch_agent::preflight_hub_for_serve(
            &exposed,
            Some(DevelopmentServeMode::Fixture)
        )
        .is_err()
    );

    let mut collecting = config.clone();
    collecting.collector.interval_seconds = 1;
    assert!(
        macos_launch_agent::preflight_hub_for_serve(
            &collecting,
            Some(DevelopmentServeMode::Fixture)
        )
        .is_err()
    );

    let mut plaintext = config.clone();
    plaintext.tls = None;
    assert!(
        macos_launch_agent::preflight_hub_for_serve(
            &plaintext,
            Some(DevelopmentServeMode::Fixture)
        )
        .is_err()
    );

    let empty_data = parent.path().join("empty-store");
    drop(HubStore::initialize(&empty_data).unwrap());
    let mut empty = config;
    empty.data_dir = empty_data;
    assert!(
        macos_launch_agent::preflight_hub_for_serve(&empty, Some(DevelopmentServeMode::Fixture))
            .is_err()
    );
    macos_launch_agent::preflight_hub_for_serve(&empty, Some(DevelopmentServeMode::Standalone))
        .expect("empty standalone source-run");
}

#[tokio::test]
async fn fixture_has_two_vehicles_and_three_exact_drive_pages() {
    let scenario: Value = serde_json::from_str(include_str!("interop/scenario.json")).unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("isolated");
    let prepared = seed::prepare(&root, 18443).unwrap();
    let store = HubStore::initialize(root.join("hub")).unwrap();
    let key =
        teslatlas_hub::teslamate_credentials::load_or_create_cursor_key(&root.join("hub")).unwrap();
    let app = paired_router(store, &key);
    let invitation: Value =
        serde_json::from_slice(&std::fs::read(&prepared.invitation_path).unwrap()).unwrap();
    let claim = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/v1/pairings/{}/claim",
                    invitation["pairing_id"].as_str().unwrap()
                ))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"secret":invitation["secret"],"device_name":"fixture-test"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(claim.status(), 200);
    let claim: Value =
        serde_json::from_slice(&claim.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let token = claim["access_token"].as_str().unwrap();
    let vehicles = get(&app, "/v1/vehicles", token).await;
    assert_eq!(vehicles["vehicles"], scenario["vehicles"]);
    let vehicle = "11111111-1111-4111-8111-111111111111";
    let current = get(&app, &format!("/v1/vehicles/{vehicle}/current"), token).await;
    for (field, expected) in scenario["current"].as_object().unwrap() {
        assert_eq!(&current[field], expected, "initial current field {field}");
    }
    assert_eq!(current["battery_level"], 0);
    assert_eq!(current["inside_temp"], 21.5);
    assert_eq!(current["outside_temp"], Value::Null);
    let empty = get(
        &app,
        "/v1/vehicles/22222222-2222-4222-8222-222222222222/current",
        token,
    )
    .await;
    assert_eq!(empty["observed_at_ms"], Value::Null);
    let page1 = get(
        &app,
        &format!("/v1/vehicles/{vehicle}/drives?limit=2"),
        token,
    )
    .await;
    assert_eq!(ids(&page1), vec![105, 104]);
    let page2 = get(
        &app,
        &format!(
            "/v1/vehicles/{vehicle}/drives?limit=2&cursor={}",
            page1["next_cursor"].as_str().unwrap()
        ),
        token,
    )
    .await;
    assert_eq!(ids(&page2), vec![103, 102]);
    let page3 = get(
        &app,
        &format!(
            "/v1/vehicles/{vehicle}/drives?limit=2&cursor={}",
            page2["next_cursor"].as_str().unwrap()
        ),
        token,
    )
    .await;
    assert_eq!(ids(&page3), vec![101]);
    assert_eq!(page3["next_cursor"], Value::Null);
    assert_eq!(page3["items"][0]["distance_km"], Value::Null);
    for page in [&page1, &page2, &page3] {
        for drive in page["items"].as_array().unwrap() {
            for (field, expected) in scenario["drives"][drive["id"].to_string()]
                .as_object()
                .unwrap()
            {
                assert_eq!(&drive[field], expected, "drive field {field}");
            }
        }
    }
    assert_eq!(
        page1["items"][0]["start_date_ms"],
        page1["items"][1]["start_date_ms"]
    );

    std::fs::write(
        root.join("scenario.json"),
        include_str!("interop/scenario.json"),
    )
    .unwrap();
    seed::advance(&root).unwrap();
    let updated = get(&app, &format!("/v1/vehicles/{vehicle}/current"), token).await;
    for (field, expected) in scenario["later_current"].as_object().unwrap() {
        assert_eq!(&updated[field], expected, "later current field {field}");
    }
    let receipt: Value =
        serde_json::from_slice(&std::fs::read(root.join("advance.json")).unwrap()).unwrap();
    assert_eq!(receipt["charge"], scenario["charge"]);
    assert!(
        seed::advance(&root).is_err(),
        "a later update is single use"
    );
    assert!(
        seed::prepare(&root, 18443).is_err(),
        "must never overwrite an existing fixture"
    );
}

#[test]
fn fixture_rejects_zero_port_without_creating_state() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("invalid");
    assert!(seed::prepare(&root, 0).is_err());
    assert!(!root.exists());
}

#[tokio::test]
async fn viewer_r1_fixture_has_exact_twenty_five_twenty_five_one_drive_pages() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("viewer-r1");
    let prepared = seed::prepare_viewer_r1(&root, 18444).unwrap();
    let store = HubStore::initialize(root.join("hub")).unwrap();
    let key =
        teslatlas_hub::teslamate_credentials::load_or_create_cursor_key(&root.join("hub")).unwrap();
    let app = paired_router(store, &key);
    let invitation: Value =
        serde_json::from_slice(&std::fs::read(&prepared.invitation_path).unwrap()).unwrap();
    let claim = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/v1/pairings/{}/claim",
                    invitation["pairing_id"].as_str().unwrap()
                ))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"secret":invitation["secret"],"device_name":"viewer-r1-test"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(claim.status(), 200);
    let claim: Value =
        serde_json::from_slice(&claim.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let token = claim["access_token"].as_str().unwrap();
    let vehicle = "11111111-1111-4111-8111-111111111111";

    let page_one = get(
        &app,
        &format!("/v1/vehicles/{vehicle}/drives?limit=25"),
        token,
    )
    .await;
    assert_eq!(ids(&page_one), (1027..=1051).rev().collect::<Vec<_>>());
    let page_two = get(
        &app,
        &format!(
            "/v1/vehicles/{vehicle}/drives?limit=25&cursor={}",
            page_one["next_cursor"].as_str().unwrap()
        ),
        token,
    )
    .await;
    assert_eq!(ids(&page_two), (1002..=1026).rev().collect::<Vec<_>>());
    let page_three = get(
        &app,
        &format!(
            "/v1/vehicles/{vehicle}/drives?limit=25&cursor={}",
            page_two["next_cursor"].as_str().unwrap()
        ),
        token,
    )
    .await;
    assert_eq!(ids(&page_three), vec![1001]);
    assert_eq!(page_three["next_cursor"], Value::Null);
}

#[test]
fn fixture_can_bind_the_selected_edge_source_identity() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("edge-bound");
    let source_id = uuid::Uuid::parse_str("04d1bc2f-0e9a-4f84-9ac5-492955dd8d5e").unwrap();

    seed::prepare_with_source_id(&root, 18480, source_id).unwrap();

    let store = HubStore::initialize(root.join("hub")).unwrap();
    let source = store
        .register_source(
            &teslatlas_hub::db::SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            1_788_566_400_000,
        )
        .unwrap();
    assert_eq!(source.source_id, source_id);
}

#[test]
fn fixture_binds_selected_edge_primary_vehicle_to_selected_source() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("edge-primary-vehicle");
    let source_id = uuid::Uuid::parse_str("8c867580-27df-4e80-9a5f-c73f9864f805").unwrap();
    let vehicle_id = uuid::Uuid::parse_str("024c338a-decd-4196-976e-29fe11b1810e").unwrap();

    let prepared = seed::prepare_with_scenario_source_and_primary_vehicle(
        &root,
        18490,
        seed::FixtureScenario::B1,
        source_id,
        vehicle_id,
        "5YJ3E1EA7KF000002",
        10,
    )
    .unwrap();

    assert_eq!(prepared.source_id, source_id);
    assert_eq!(prepared.vehicle_ids[0], vehicle_id);
    let store = HubStore::initialize(root.join("hub")).unwrap();
    let connection = store.open().unwrap();
    let stored: (String, String, String) = connection
        .query_row(
            "SELECT source_id, vin, source_vehicle_key FROM vehicles WHERE vehicle_id = ?1",
            [vehicle_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored.0, source_id.to_string());
    assert_eq!(stored.1, "5YJ3E1EA7KF000002");
    assert_eq!(stored.2, "10");
}

#[cfg(feature = "interop-fixture")]
#[test]
fn interop_source_registration_preserves_the_sealed_source_identity() {
    // Removing the exact-ID test entry point, or making it generate a UUID,
    // must make this fail: an empty Edge fixture could no longer honour its
    // sealed source binding without rewriting SQLite rows.
    let parent = tempfile::tempdir().unwrap();
    let store = HubStore::initialize(parent.path().join("hub")).unwrap();
    let source_id = uuid::Uuid::parse_str("7a73673e-10f3-4d85-a060-8818b2c8d4b3").unwrap();
    let descriptor =
        teslatlas_hub::db::SourceDescriptor::new("owner_api_compat", "empty_edge_binding_test");

    let registered = store
        .register_interop_source_with_id(&descriptor, 1_788_566_400_000, source_id)
        .unwrap();
    let reopened = store
        .register_source(&descriptor, 1_788_566_400_001)
        .unwrap();

    assert_eq!(registered.source_id, source_id);
    assert_eq!(reopened.source_id, source_id);
}

#[cfg(feature = "interop-fixture")]
#[test]
fn interop_source_registration_rejects_conflicting_sealed_identities() {
    // Replacing the conflict checks with a generated UUID, or allowing a
    // source-id to be rebound to another descriptor, must make this fail.
    let parent = tempfile::tempdir().unwrap();
    let store = HubStore::initialize(parent.path().join("hub")).unwrap();
    let first_id = uuid::Uuid::parse_str("95c3baa0-2cc8-49d4-a34a-3cf2adb68a76").unwrap();
    let conflicting_id = uuid::Uuid::parse_str("c9cd8b9f-89a1-4a4e-aa74-e9e25e72387a").unwrap();
    let first = teslatlas_hub::db::SourceDescriptor::new(
        "owner_api_compat",
        "empty_edge_binding_conflict_first",
    );
    let second = teslatlas_hub::db::SourceDescriptor::new(
        "owner_api_compat",
        "empty_edge_binding_conflict_second",
    );

    store
        .register_interop_source_with_id(&first, 1_788_566_400_000, first_id)
        .unwrap();

    assert!(matches!(
        store.register_interop_source_with_id(&first, 1_788_566_400_001, conflicting_id),
        Err(teslatlas_hub::db::StoreError::InvalidSourceId)
    ));
    assert!(matches!(
        store.register_interop_source_with_id(&second, 1_788_566_400_001, first_id),
        Err(teslatlas_hub::db::StoreError::InvalidSourceId)
    ));
}

#[cfg(all(feature = "interop-fixture", target_os = "macos"))]
#[test]
fn empty_edge_binding_fixture_passes_macos_preflight_with_zero_edge_telemetry() {
    // If this fixture seeds a drive, charge, producer observation, or Edge
    // ledger advance, the normal preflight control path is no longer honest.
    // The one synthetic base is intentionally retained: it is the only
    // supported way to make a configured vehicle visible to macOS preflight.
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("empty-edge-binding");
    let source_id = uuid::Uuid::parse_str("0c722b24-1d6b-495f-9386-67a2bcb13b9a").unwrap();
    let vehicle_id = uuid::Uuid::parse_str("97f0ccbf-ad87-4b79-83e5-04d1589235f5").unwrap();
    let binding = seed::EmptyEdgeBinding {
        installation_id: "empty-edge-binding".into(),
        lineage: "spool-1".into(),
        source_id,
        vehicle_id,
        vin: "5YJ3E1EA7KF000003".into(),
        car_id: 42,
    };

    let prepared = seed::prepare_empty_edge_binding(&root, &binding).unwrap();
    let store = HubStore::initialize(&prepared.data_dir).unwrap();
    let configured = store.configured_tesla_vehicles().unwrap();
    assert_eq!(configured.len(), 1);
    assert_eq!(configured[0].0, vehicle_id);
    assert_eq!(configured[0].1, 42);
    assert!(configured[0].2.enabled);
    let configured_binding = teslatlas_hub::db::EdgeBinding {
        installation_id: binding.installation_id.clone(),
        lineage: binding.lineage.clone(),
        source_id,
        vehicle_id,
        vin: binding.vin.clone(),
        car_id: binding.car_id,
    };
    store.validate_edge_binding(&configured_binding).unwrap();
    let mismatched_binding = teslatlas_hub::db::EdgeBinding {
        vin: "5YJ3E1EA7KF000004".into(),
        ..configured_binding
    };
    assert!(store.validate_edge_binding(&mismatched_binding).is_err());
    assert!(
        store
            .current_observations_for_vehicle(vehicle_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.published_vehicles().unwrap().len(), 1);
    let ledger = store
        .edge_ledger_counts(&binding.installation_id, &binding.lineage)
        .unwrap();
    assert_eq!(ledger.applications, 0);
    assert_eq!(ledger.sequences, 0);
    assert_eq!(ledger.pending_publications, 0);
    assert_eq!(ledger.ack_frontier, None);

    let connection = store.open().unwrap();
    for table in [
        "raw_observations",
        "current_observations",
        "materialised_drives",
        "materialised_positions",
        "materialised_charges",
        "materialised_charge_samples",
        "edge_lineages",
        "edge_accumulator_states",
        "edge_consumer_diagnostics",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table} must remain empty");
    }
    for table in [
        "materialised_cars",
        "sync_manifests",
        "sync_bases",
        "sync_heads",
        "v2_base_bindings",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "{table} must contain only the required base");
    }

    let config = format!(
        "data_dir = {data:?}\nbind = '127.0.0.1:18488'\n[collector]\nprovider = 'fleet'\ninterval_seconds = 0\n[collector.legacy_auth]\nenabled = false\n[collector.edge]\nbase_url = 'https://192.0.2.1:21443'\nca_certificate_path = {ca:?}\nclient_certificate_path = {cert:?}\nclient_private_key_path = {key:?}\nbearer_token_path = {bearer:?}\ninstallation_id = {installation_id:?}\nlineage = {lineage:?}\nsource_id = '{source_id}'\nvehicle_id = '{vehicle_id}'\nvin = {vin:?}\ncar_id = {car_id}\n",
        data = prepared.data_dir,
        ca = root.join("edge-ca.pem"),
        cert = root.join("hub-client.pem"),
        key = root.join("hub-client-key.pem"),
        bearer = root.join("delivery-bearer"),
        installation_id = binding.installation_id,
        lineage = binding.lineage,
        source_id = source_id,
        vehicle_id = vehicle_id,
        vin = binding.vin,
        car_id = binding.car_id,
    );
    let (config, _) =
        teslatlas_hub::config::HubConfig::from_exact_bytes(config.as_bytes()).unwrap();
    teslatlas_hub::macos_launch_agent::preflight_hub_for_config(&config).unwrap();
    let report = teslatlas_hub::runtime::diagnostics::inspect_hub(&store, &config).unwrap();
    assert_eq!(report.status, "ok");
    for name in [
        "selectedProviderCredentials",
        "fleetCollectionScopes",
        "collectorReadiness",
    ] {
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == name && check.passed),
            "{name} must accept the configured Edge transport without direct Fleet credentials"
        );
    }
}

fn ids(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

async fn get(app: &axum::Router, route: &str, token: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(route)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "route {route}");
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
