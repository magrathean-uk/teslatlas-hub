// External disposable public-API discriminator; coordinator owns compilation/run.
// Refuses an existing root. No raw SQL, listener, wire replacement, or live data.
// Dependencies: teslatlas-hub, axum, http-body-util, serde_json, tokio, tower, uuid.
// The direct PostgreSQL importer is traced separately, not executed by this probe.
use crate::{
    db::{HubStore, LifecycleCommit, SourceDescriptor, VehicleDescriptor},
    hub_pack::{
        ProjectionCar, ProjectionCarSettings, ProjectionPackRequest, ProjectionPackWriter,
        ProjectionSnapshot,
    },
    lifecycle::{LifecycleDelta, OpenSessionState},
    protocol::{CursorKey, SequenceRange, Sha256Digest},
    server::paired_router,
    teslamate_projection::{
        DriveRelations, TeslaMateAddress, TeslaMateDrive, TeslaMateGeofence, project_drive,
    },
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;
use uuid::Uuid;

const BUDGET: usize = 1_048_576;

#[test]
fn empty_and_unservable_single_drive_pages_are_bounded() {
    use super::super::bounded_public_drive_page;
    use crate::api::public_query::{DriveQuery, PublicDrive};
    let key = CursorKey::from_bytes([61; 32]);
    let vehicle = Uuid::new_v4();
    let query = DriveQuery {
        from_ms: 0,
        to_ms: i64::MAX,
        limit: 500,
        after: None,
    };
    let empty = bounded_public_drive_page(Vec::new(), false, query, &key, vehicle)
        .unwrap()
        .unwrap();
    assert!(empty.items.is_empty());
    assert!(empty.next_cursor.is_none());
    let drive = serde_json::from_value::<crate::hub_pack::ProjectionDrive>(json!({
        "id":1,"car_id":1,"start_date_ms":1,"end_date_ms":2,"start_address":"x".repeat(BUDGET)
    }))
    .unwrap();
    assert!(
        bounded_public_drive_page(
            vec![PublicDrive::from_projection(vehicle, drive)],
            false,
            query,
            &key,
            vehicle
        )
        .unwrap()
        .is_none()
    );
}

// A repeated single label makes a synthetic pack exceed the real expansion
// ratio admission before HTTP. Vary valid character-width labels instead of
// changing the production limit; preserve their exact serialized byte widths.
fn source_width_text(chars: usize, seed: u64, unicode: bool) -> String {
    let mut state = seed;
    (0..chars)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let digit = ((state >> 32) % 62) as u8;
            if unicode {
                char::from_u32(0x100 + u32::from(digit)).unwrap()
            } else {
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"[digit as usize]
                    as char
            }
        })
        .collect()
}

fn car(id: i64) -> ProjectionCar {
    ProjectionCar {
        id,
        name: "Synthetic response budget probe".into(),
        model: "Model 3".into(),
        vin: None,
        source_eid: None,
        source_vid: None,
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        firmware_version: None,
        efficiency_wh_per_km: None,
        settings: ProjectionCarSettings::default(),
    }
}

async fn get(
    app: &Router,
    uri: String,
    bearer: &str,
    etag: Option<&str>,
) -> (StatusCode, Option<String>, Vec<u8>) {
    let mut request = Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    if let Some(etag) = etag {
        request = request.header(header::IF_NONE_MATCH, etag);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).expect("request"))
        .await
        .expect("in-memory handler");
    let status = response.status();
    let etag = response
        .headers()
        .get(header::ETAG)
        .map(|v| v.to_str().expect("ASCII ETag").to_owned());
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .expect("cache header"),
        "no-store"
    );
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, etag, body)
}

async fn variant(
    root: &Path,
    label: &str,
    address: String,
    fence: Option<String>,
    over_budget: bool,
) {
    // These explicit checks preserve the ordinary pinned source's character widths.
    assert!(address.chars().count() <= 512);
    assert!(!address.as_bytes().contains(&0));
    assert!(
        fence
            .as_ref()
            .is_none_or(|s| s.chars().count() <= 255 && s.len() <= 256)
    );
    let store_path = root.join(label);
    let store = HubStore::initialize(&store_path).expect("new synthetic store");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate_import", label), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "10").with_tesla_identity(Some(70), None),
            1_001,
        )
        .expect("vehicle");
    let binding = store
        .v2_projection_binding(vehicle.vehicle_id)
        .expect("binding");
    let key = CursorKey::from_bytes([63; 32]);
    let source_address = TeslaMateAddress {
        id: 1,
        display_name: Some(address),
        name: None,
    };
    let source_fence = fence.map(|name| TeslaMateGeofence {
        id: 1,
        name,
        latitude: Some(51.5),
        longitude: Some(-0.1),
        radius_m: Some(100.0),
        billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
        cost_per_unit: None,
        session_fee: None,
    });
    let mut drives = Vec::new();
    for id in 1..=500_i64 {
        let mut address = source_address.clone();
        address.id = id;
        if address.display_name.as_ref().unwrap().chars().count() == 512 {
            let unicode = address.display_name.as_ref().unwrap().len() > 512;
            address.display_name = Some(source_width_text(512, id as u64, unicode));
        }
        let mut fence = source_fence.clone();
        if let Some(fence) = fence.as_mut() {
            fence.id = id;
            fence.name = source_width_text(255, id as u64 + 10_000, false);
        }
        // Missing Option fields deserialize as None, exactly the projected nullable values.
        let source_drive: TeslaMateDrive = serde_json::from_value(json!({
            "id": id, "car_id": binding.selected_car_id,
            "start_date_ms": ((id + 1) / 2) * 1_000, "end_date_ms": ((id + 1) / 2) * 1_000 + 500,
            "start_address_id": address.id, "end_address_id": address.id,
            "start_geofence_id": fence.as_ref().map(|f| f.id),
            "end_geofence_id": fence.as_ref().map(|f| f.id),
        }))
        .expect("typed source drive");
        drives.push(
            project_drive(
                &source_drive,
                binding.selected_car_id,
                DriveRelations {
                    start_address: Some(&address),
                    end_address: Some(&address),
                    start_geofence: fence.as_ref(),
                    end_geofence: fence.as_ref(),
                    ..DriveRelations::default()
                },
            )
            .expect("source projection")
            .expect("completed drive"),
        );
    }
    let snapshot = ProjectionSnapshot {
        cars: vec![car(binding.selected_car_id)],
        drives: drives.clone(),
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
    };
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: binding.clone(),
        sequence: SequenceRange {
            from_exclusive: 1,
            to_inclusive: 1,
        },
        snapshot: &snapshot,
    };
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])
        .expect("real typed admission");
    let manifest = request
        .signed_manifest_with_states_and_updates(&pack, &[], &[], &key)
        .expect("manifest");
    store
        .finalize_import_snapshot_with_binding(
            &manifest,
            Sha256Digest::of_bytes(label.as_bytes()),
            &[],
            &binding,
        )
        .expect("base publication");
    // Snapshot publication alone does not populate the public materialised table.
    // Use its public transactional writer with the same admitted project_drive rows.
    let delta = LifecycleDelta {
        drives,
        ..LifecycleDelta::default()
    };
    let encoded = OpenSessionState::new().encode().expect("session");
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: binding.selected_car_id,
            open_session_json: &encoded,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: 1_000_000,
            delta: &delta,
        })
        .expect("public materialisation");
    assert_eq!(
        store
            .materialised_drives_page(vehicle.vehicle_id, 0, i64::MAX, None, 501)
            .expect("read")
            .len(),
        500
    );
    let store = HubStore::initialize(&store_path).expect("restart");
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("timestamp");
    let invitation = store
        .create_pairing("synthetic byte probe", now - 1, now + 60_000)
        .expect("pairing");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "synthetic client",
            now,
        )
        .expect("bearer");
    let bearer = access.access_token.as_bearer();
    let app = paired_router(store, &key);
    let uri = format!("/v1/vehicles/{}/drives?limit=500", vehicle.vehicle_id);
    let (status, etag, body) = get(&app, uri.clone(), bearer, None).await;
    assert_eq!(status, StatusCode::OK);
    let page: Value = serde_json::from_slice(&body).expect("real response JSON");
    let emitted = page["items"].as_array().expect("items").len();
    assert!(body.len() <= BUDGET);
    assert_eq!(emitted < 500, over_budget);
    assert_eq!(page["next_cursor"].is_string(), over_budget);
    println!(
        "{label}: limit=500 status={} body_bytes={} over_1MiB={} rows={emitted} restart=true",
        status.as_u16(),
        body.len(),
        body.len() > BUDGET
    );
    let (status, _, body304) = get(&app, uri, bearer, etag.as_deref()).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body304.is_empty());
    for limit in [100, 500] {
        let mut cursor: Option<String> = None;
        let mut ids = BTreeSet::new();
        let mut ordered = Vec::new();
        let mut pages = 0;
        let mut maximum_page_bytes = 0;
        loop {
            let mut uri = format!("/v1/vehicles/{}/drives?limit={limit}", vehicle.vehicle_id);
            if let Some(cursor) = cursor.as_ref() {
                uri.push_str("&cursor=");
                uri.push_str(cursor);
            }
            let (status, _, body) = get(&app, uri, bearer, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.len() <= BUDGET);
            maximum_page_bytes = maximum_page_bytes.max(body.len());
            let page: Value = serde_json::from_slice(&body).expect("100-row page");
            for item in page["items"].as_array().expect("page items") {
                ordered.push(item["id"].as_i64().expect("ordered drive ID"));
                assert!(
                    ids.insert(item["id"].as_i64().expect("drive ID")),
                    "duplicate across pages"
                );
            }
            pages += 1;
            assert!(pages <= 5, "bounded expected traversal");
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            pages,
            if limit == 100 {
                5
            } else if over_budget {
                2
            } else {
                1
            }
        );
        assert_eq!(ids, (1..=500).collect::<BTreeSet<_>>());
        assert_eq!(ordered, (1..=500).rev().collect::<Vec<_>>());
        println!(
            "{label}: limit={limit} pages={pages} maximum_body_bytes={maximum_page_bytes} all_500_once=true conditional_304_empty=true"
        );
    }
}

#[tokio::test]
async fn source_valid_admitted_drive_pages_respect_byte_budget_and_traverse_once() {
    let data = crate::private_tempdir().expect("disposable store");
    let root = data.path().join("stores");
    variant(&root, "short", "Main Street".into(), None, false).await;
    variant(
        &root,
        "ascii-source-width",
        "A".repeat(512),
        Some("G".repeat(255)),
        true,
    )
    .await;
    variant(&root, "unicode-source-width", "Å".repeat(512), None, true).await;
}
