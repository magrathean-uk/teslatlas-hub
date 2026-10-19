// SPDX-License-Identifier: AGPL-3.0-only

// Shared across test binaries; each uses a different subset.
#[allow(dead_code, unused_imports)]
#[path = "interop/seed.rs"]
mod seed;

use axum::{body::Body, http::Request};
#[cfg(feature = "interop-fixture")]
use base64::{Engine as _, engine::general_purpose::STANDARD};
#[cfg(feature = "interop-fixture")]
use ed25519_dalek::{Signature, VerifyingKey};
use http_body_util::BodyExt;
use serde_json::Value;
#[cfg(feature = "interop-fixture")]
use teslatlas_hub::db::VehicleDescriptor;
#[cfg(feature = "interop-fixture")]
use teslatlas_hub::protocol::Sha256Digest;
use teslatlas_hub::{db::HubStore, server::paired_router};
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
#[tokio::test]
async fn physical_v3_fixture_rotates_and_completes_the_retained_rebase_journey() {
    const SOURCE_ID: uuid::Uuid = uuid::Uuid::from_u128(0x51300000000040008000000000000513);
    const PROFILE_HEADER: &str = "x-teslatlas-sync-profile";
    const SCHEMAS_HEADER: &str = "x-teslatlas-supported-schemas";
    const PROFILE_ID: &str = "hub-sync-v1@1.3.0";

    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("physical-v3-two-head");
    let prepared = seed::prepare_with_scenario_and_source_id(
        &root,
        21_445,
        seed::FixtureScenario::PhysicalV3Public513,
        SOURCE_ID,
    )
    .unwrap();
    let initial = prepared.physical_v3_admission.as_ref().unwrap();
    assert_eq!(initial.chunk_count, 513);
    assert!(!initial.collector_enabled);
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
                    serde_json::json!({
                        "secret": invitation["secret"],
                        "device_name": "physical-v3-two-head-fixture"
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

    let signing_keys = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/sync/signing-keys",
                    initial.vehicle_id
                ))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(signing_keys.status(), 200);
    let signing_keys: Value =
        serde_json::from_slice(&signing_keys.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    assert_eq!(signing_keys["profile_id"], PROFILE_ID);
    assert_eq!(signing_keys["vehicle_id"], initial.vehicle_id.to_string());
    let public_key = signing_keys["keys"][0]["public_key"].as_str().unwrap();

    let bootstrap = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{}/sync/manifest", initial.vehicle_id))
                .header("authorization", format!("Bearer {token}"))
                .header(PROFILE_HEADER, PROFILE_ID)
                .header(SCHEMAS_HEADER, "2.1,2.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bootstrap.status(), 200);
    let bootstrap: Value =
        serde_json::from_slice(&bootstrap.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(bootstrap["receipt_id"], initial.receipt_id);
    assert_eq!(bootstrap["sequence"], initial.head_sequence);
    assert_eq!(bootstrap["chunks"].as_array().unwrap().len(), 513);
    assert_hub_sync_fixture_signature(&bootstrap, public_key);
    let retained_first_digest = bootstrap["chunks"][0]["pack"]["sha256"]
        .as_str()
        .unwrap()
        .to_owned();

    let rotation = seed::rotate_physical_v3_public_admission(&root).unwrap();
    assert_eq!(rotation.retained_receipt_id, initial.receipt_id);
    assert_eq!(rotation.retained_head_sequence, initial.head_sequence);
    assert_eq!(rotation.active_chunk_count, 513);
    assert!(rotation.active_head_sequence > initial.head_sequence);
    assert!(!rotation.collector_enabled);

    let changes_request = |receipt_id: &str, sequence: u64| {
        Request::builder()
            .method("POST")
            .uri(format!(
                "/v1/vehicles/{}/sync/changes-since",
                initial.vehicle_id
            ))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "base_receipt_id": receipt_id,
                    "base_manifest_schema": "2.2",
                    "from_sequence": sequence,
                    "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                })
                .to_string(),
            ))
            .unwrap()
    };
    let rebase = app
        .clone()
        .oneshot(changes_request(
            &rotation.retained_receipt_id,
            rotation.retained_head_sequence,
        ))
        .await
        .unwrap();
    assert_eq!(rebase.status(), 409);
    let rebase: Value =
        serde_json::from_slice(&rebase.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(rebase["kind"], "rebase_required");
    assert_eq!(rebase["requested_base_receipt_id"], initial.receipt_id);
    assert_eq!(
        rebase["replacement"]["receipt_id"],
        rotation.active_receipt_id
    );
    assert_eq!(
        rebase["replacement"]["sequence"],
        rotation.active_head_sequence
    );
    let chunks = rebase["replacement"]["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 513);
    assert_eq!(
        chunks.first().unwrap()["pack"]["sha256"],
        rotation.active_first_chunk_sha256
    );
    assert_eq!(
        chunks.last().unwrap()["pack"]["sha256"],
        rotation.active_last_chunk_sha256
    );
    assert_eq!(
        rebase["retry_request"]["base_receipt_id"],
        rotation.active_receipt_id
    );
    assert_hub_sync_fixture_signature(&rebase, public_key);

    for digest in [
        &rotation.active_first_chunk_sha256,
        &rotation.active_last_chunk_sha256,
    ] {
        let response = app
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
        assert_eq!(response.status(), 200);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(Sha256Digest::of_bytes(&body).to_string(), digest.as_str());
    }
    let retained_pack = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/packs/sha256/{retained_first_digest}.sqlite.zst"
                ))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(retained_pack.status(), 404);

    let noop = app
        .oneshot(changes_request(
            &rotation.active_receipt_id,
            rotation.active_head_sequence,
        ))
        .await
        .unwrap();
    assert_eq!(noop.status(), 200);
    let noop: Value =
        serde_json::from_slice(&noop.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(noop["kind"], "no_op");
    assert_eq!(noop["base_receipt_id"], rotation.active_receipt_id);
    assert_eq!(noop["sequence"], rotation.active_head_sequence);
    assert_hub_sync_fixture_signature(&noop, public_key);
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

    let invitation: Value =
        serde_json::from_slice(&std::fs::read(&prepared.invitation_path).unwrap()).unwrap();
    let app = paired_router(store.clone(), &key);
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
    // The device is paired while the vehicle is active, so it is granted it:
    // the grant is what keeps the retired vehicle's packs authorized below.
    assert_eq!(
        get(&app, "/v1/vehicles", token).await["vehicles"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

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
    // A device paired while the vehicle is retired is never granted it, so
    // the retired vehicle's pack is not authorized to that device.
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let late_invitation = restarted
        .create_pairing("late device", now_ms - 1, now_ms + 60_000)
        .unwrap();
    let late_access = restarted
        .claim_pairing(
            late_invitation.pairing_id,
            late_invitation.secret(),
            "late-device-test",
            now_ms,
        )
        .unwrap();
    let late_pack = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{digest}.sqlite.zst"))
                .header(
                    "authorization",
                    format!("Bearer {}", late_access.access_token.as_bearer()),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(late_pack.status(), 404);

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
    assert!(macos_launch_agent::development_serve_mode(Some(OsStr::new("true")), None).is_err());
    assert!(macos_launch_agent::preflight_hub_for_serve(&config, None).is_err());
    macos_launch_agent::preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Fixture))
        .expect("standard fixture development Serve");
    macos_launch_agent::preflight_hub_for_serve(&config, Some(DevelopmentServeMode::Standalone))
        .expect("collector-disabled fixture is also a valid standalone source-run");

    let mut exposed = config.clone();
    exposed.bind = "0.0.0.0:21444".parse().unwrap();
    assert!(
        macos_launch_agent::preflight_hub_for_serve(&exposed, Some(DevelopmentServeMode::Fixture))
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
fn fixture_can_bind_a_selected_source_identity() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("source-bound");
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
fn fixture_binds_a_selected_primary_vehicle_to_its_source() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("selected-primary-vehicle");
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

fn ids(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

#[cfg(feature = "interop-fixture")]
fn assert_hub_sync_fixture_signature(value: &Value, public_key: &str) {
    let signature = value["signature"].as_object().unwrap();
    let mut payload = value.clone();
    payload.as_object_mut().unwrap().remove("signature");
    let canonical = serde_jcs::to_vec(&payload).unwrap();
    assert_eq!(
        signature["signed_payload_sha256"],
        Sha256Digest::of_bytes(&canonical).to_string()
    );
    let public_key: [u8; 32] = STANDARD.decode(public_key).unwrap().try_into().unwrap();
    let verifying_key = VerifyingKey::from_bytes(&public_key).unwrap();
    let signature_bytes = STANDARD
        .decode(signature["signature"].as_str().unwrap())
        .unwrap();
    let signature = Signature::from_slice(&signature_bytes).unwrap();
    verifying_key.verify_strict(&canonical, &signature).unwrap();
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
