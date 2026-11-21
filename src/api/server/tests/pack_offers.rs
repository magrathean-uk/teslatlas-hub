// SPDX-License-Identifier: AGPL-3.0-only

//! Issues #98 and #109 through the paired router: a pack named in a signed
//! response stays fetchable by that device after the head rotates, while
//! discovery names only the current head.

use super::*;

struct Paired {
    device_id: Uuid,
    bearer: String,
}

fn pair(store: &HubStore, name: &str, now: i64) -> Paired {
    let invitation = store.create_pairing(name, now - 1, i64::MAX).expect("invite");
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), name, now)
        .expect("paired access");
    Paired {
        device_id: access.device_id,
        bearer: access.access_token.as_bearer().to_owned(),
    }
}

fn get(uri: &str, bearer: &str, range: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }
    request.body(Body::empty()).unwrap()
}

fn pack_uri(digest: Sha256Digest) -> String {
    format!("/v1/packs/sha256/{digest}.sqlite.zst")
}

async fn body(response: Response) -> Vec<u8> {
    response.into_body().collect().await.unwrap().to_bytes().to_vec()
}

fn rotate(
    store: &HubStore,
    temp: &std::path::Path,
    key: &CursorKey,
    prior: &crate::db::PendingPhysicalV3Admission,
    name: &str,
    at: i64,
) -> crate::db::PendingPhysicalV3Admission {
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (_, candidate) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.join(name), store, key, 1, "offers98", prior.head_sequence + 1);
    let next = store
        .rotate_pending_physical_v3_admission_at(&gate, candidate, at)
        .expect("rotation");
    store
        .activate_pending_physical_v3_rotation_at(&gate, prior.vehicle_id, &prior.receipt_id, at)
        .expect("public successor");
    next
}

#[tokio::test]
async fn design_later_98_offered_full_and_changed_set_packs_survive_two_rotations() {
    let temp = crate::private_tempdir().expect("private test root");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let key = CursorKey::from_bytes([0x98; 32]);
    let now = current_epoch_ms().expect("clock");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (binding, first) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("a"), &store, &key, 1, "offers98", 1);
    let first = store.stage_pending_physical_v3_admission(&gate, first).expect("A");
    let (_, second) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("b"), &store, &key, 3, "offers98", 2);
    let preview =
        crate::db::physical_v3_admission_from_manifest(&second.manifest, binding.selected_car_id)
            .expect("B identity");
    let delta = crate::import::source_db::physical_delta_pack::prepare_changed_set(
        &first,
        &preview,
        &binding,
        &key,
        Sha256Digest::of_bytes(b"offers98 target"),
        store.packs_dir(),
        0,
    )
    .expect("changed set");
    let second = store
        .rotate_pending_physical_v3_admission_with_delta_at(&gate, second, Some(delta), now)
        .expect("B");
    store
        .activate_pending_physical_v3_rotation_at(&gate, binding.vehicle_id, &first.receipt_id, now)
        .expect("B public");
    drop(gate);
    let phone = pair(&store, "offered phone", now);
    let other = pair(&store, "other phone", now);
    let app = paired_router(store.clone(), &key);
    let manifest_uri = format!("/v1/vehicles/{}/sync/manifest", binding.vehicle_id);
    let manifest = |bearer: &str| {
        Request::builder()
            .uri(&manifest_uri)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(SYNC_PROFILE_HEADER, hub_sync_v1::HUB_SYNC_PROFILE_14)
            .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
            .body(Body::empty())
            .unwrap()
    };

    // The phone is offered B's full manifest and the A-to-B changed set.
    let offered = app.clone().oneshot(manifest(&phone.bearer)).await.unwrap();
    assert_eq!(offered.status(), StatusCode::OK);
    let offered: serde_json::Value = serde_json::from_slice(&body(offered).await).unwrap();
    assert_eq!(offered["receipt_id"], second.receipt_id);
    let signing = ManifestSigning::from_cursor_key(&key);
    let first_sha =
        Sha256Digest::of_bytes(&signing.signed_physical_manifest_document(&first).unwrap());
    let changes = Request::builder()
        .method("POST")
        .uri(format!("/v1/vehicles/{}/sync/changes-since", binding.vehicle_id))
        .header(header::AUTHORIZATION, format!("Bearer {}", phone.bearer))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "source": {"installation_id":binding.installation_id,"account_id":binding.account_id,
                    "vehicle_id":binding.vehicle_id,"generation":binding.generation,
                    "selected_car_id":binding.selected_car_id},
                "base_receipt_id":first.receipt_id,"base_manifest_id":first.snapshot_id.to_string(),
                "base_manifest_sha256":first_sha.to_string(),"base_manifest_schema":"2.2",
                "from_sequence":first.head_sequence,
                "schema_version_range":{"minimum":"2.2","maximum":"2.2"},
                "accepted_changed_set_formats":[hub_sync_v1::PHYSICAL_DELTA_FORMAT],
            }))
            .unwrap(),
        ))
        .unwrap();
    let changed = app.clone().oneshot(changes).await.unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    let changed = response_json(changed).await;
    assert_eq!(changed["kind"], "physical_changed_set");
    let delta_digest: Sha256Digest =
        changed["chunks"][0]["pack"]["sha256"].as_str().unwrap().parse().unwrap();
    let full_digest = second.manifest.chunks.last().unwrap().sha256;

    let delta_bytes =
        fs::read(store.packs_dir().join("sha256").join(format!("{delta_digest}.sqlite.zst")))
            .expect("changed-set bytes");
    let mut current = second.clone();
    for (rotation, name) in [(1, "c"), (2, "d")] {
        current = rotate(&store, temp.path(), &key, &current, name, now + rotation);
        // Discovery names only the current head.
        let discovered = app.clone().oneshot(manifest(&phone.bearer)).await.unwrap();
        assert_eq!(discovered.status(), StatusCode::OK);
        let discovered: serde_json::Value =
            serde_json::from_slice(&body(discovered).await).unwrap();
        assert_eq!(discovered["receipt_id"], current.receipt_id, "rotation {rotation}");
        let current_pack = app
            .clone()
            .oneshot(get(&pack_uri(current.manifest.chunks[0].sha256), &other.bearer, None))
            .await
            .unwrap();
        assert_eq!(current_pack.status(), StatusCode::OK);
        // The offered full chunk and changed-set pack stay fetchable, verified.
        let full =
            app.clone().oneshot(get(&pack_uri(full_digest), &phone.bearer, None)).await.unwrap();
        assert_eq!(full.status(), StatusCode::OK, "rotation {rotation}");
        assert_eq!(full.headers()[header::CONTENT_TYPE], "application/vnd.teslatlas.sync-pack");
        assert_eq!(Sha256Digest::of_bytes(&body(full).await), full_digest);
        let ranged = app
            .clone()
            .oneshot(get(&pack_uri(delta_digest), &phone.bearer, Some("bytes=3-17")))
            .await
            .unwrap();
        assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT, "rotation {rotation}");
        assert_eq!(
            ranged.headers()[header::CONTENT_TYPE],
            "application/vnd.teslatlas.physical-delta+sqlite+zstd;version=1"
        );
        assert_eq!(body(ranged).await, delta_bytes[3..=17]);
    }
    // Another device, and a digest never offered, get the unknown-digest 404.
    for (digest, bearer) in [
        (full_digest, other.bearer.as_str()),
        (delta_digest, other.bearer.as_str()),
        (Sha256Digest::of_bytes(b"never offered"), phone.bearer.as_str()),
    ]
    .into_iter()
    .chain(
        // A chunk of head A the phone was never offered (none if B repeats it).
        first
            .manifest
            .chunks
            .iter()
            .map(|chunk| chunk.sha256)
            .filter(|digest| second.manifest.chunks.iter().all(|chunk| chunk.sha256 != *digest))
            .map(|digest| (digest, phone.bearer.as_str())),
    ) {
        let refused = app.clone().oneshot(get(&pack_uri(digest), bearer, None)).await.unwrap();
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);
    }
    // Expiry ends the lease; fetching renewed nothing.
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE device_pack_offers SET offered_at_ms = 0, expires_at_ms = 1
              WHERE device_id = ?1 AND sha256 = ?2",
            rusqlite::params![phone.device_id.to_string(), full_digest.to_string()],
        )
        .unwrap();
    let expired = app.clone().oneshot(get(&pack_uri(full_digest), &phone.bearer, None)).await.unwrap();
    assert_eq!(expired.status(), StatusCode::NOT_FOUND);
    // Revocation wins at once.
    store.revoke_device(phone.device_id).expect("revoke");
    let revoked = app.clone().oneshot(get(&pack_uri(delta_digest), &phone.bearer, None)).await.unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
}

/// A 100-byte prepared pack object named by a genuinely signed month receipt
/// of `head`, as publication would leave it.
fn prepared_month(
    store: &HubStore,
    key: &CursorKey,
    head: &crate::db::PendingPhysicalV3Admission,
    now: i64,
    fill: u8,
) -> (String, Sha256Digest) {
    let bytes = vec![fill; 100];
    let digest = Sha256Digest::of_bytes(&bytes);
    fs::write(store.packs_dir().join("sha256").join(format!("{digest}.sqlite.zst")), &bytes)
        .expect("prepared object");
    let (artifact_id, _) = crate::sync::prepared_map::insert_signed_month_fixture(
        store,
        key,
        head,
        now,
        &digest.to_string(),
    );
    (artifact_id, digest)
}

#[tokio::test]
async fn design_later_109_issued_prepared_pack_stays_fetchable_across_rotations() {
    let temp = crate::private_tempdir().expect("private test root");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let key = CursorKey::from_bytes([0x09; 32]);
    let now = current_epoch_ms().expect("clock");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (binding, first) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("a"), &store, &key, 2, "offers98", 1);
    let first = store.stage_pending_physical_v3_admission(&gate, first).expect("A");
    drop(gate);
    let (first_id, first_digest) = prepared_month(&store, &key, &first, now, 0x71);
    let phone = pair(&store, "issued phone", now);
    let other = pair(&store, "other phone", now);
    let app = paired_router(store.clone(), &key);
    let receipt_uri = |id: &str| {
        format!("/v1/vehicles/{}/sync/prepared-artefacts/{id}", binding.vehicle_id)
    };
    let issued = app.clone().oneshot(get(&receipt_uri(&first_id), &phone.bearer, None)).await.unwrap();
    assert_eq!(issued.status(), StatusCode::OK);
    let issued: serde_json::Value = serde_json::from_slice(&body(issued).await).unwrap();
    assert_eq!(issued["pack"]["sha256"], first_digest.to_string());
    let expiry = || -> i64 {
        store
            .open()
            .unwrap()
            .query_row(
                "SELECT expires_at_ms FROM device_pack_offers WHERE device_id = ?1 AND sha256 = ?2",
                rusqlite::params![phone.device_id.to_string(), first_digest.to_string()],
                |row| row.get(0),
            )
            .unwrap()
    };
    let leased_until = expiry();

    let mut current = first.clone();
    for (rotation, name) in [(1, "b"), (2, "c")] {
        current = rotate(&store, temp.path(), &key, &current, name, now + rotation);
        // Discovery advances: the old receipt is not served, a new one names
        // the current head.
        let old = app.clone().oneshot(get(&receipt_uri(&first_id), &phone.bearer, None)).await.unwrap();
        assert_eq!(old.status(), StatusCode::NOT_FOUND, "rotation {rotation}");
        let (current_id, _) = prepared_month(&store, &key, &current, now, 0x71 + rotation as u8);
        let fresh =
            app.clone().oneshot(get(&receipt_uri(&current_id), &other.bearer, None)).await.unwrap();
        assert_eq!(fresh.status(), StatusCode::OK);
        let fresh: serde_json::Value = serde_json::from_slice(&body(fresh).await).unwrap();
        assert_eq!(fresh["source"]["input_receipt_id"], current.receipt_id);
        // The issued pack is still fetchable by the issued device only.
        let pack =
            app.clone().oneshot(get(&pack_uri(first_digest), &phone.bearer, None)).await.unwrap();
        assert_eq!(pack.status(), StatusCode::OK, "rotation {rotation}");
        assert_eq!(Sha256Digest::of_bytes(&body(pack).await), first_digest);
        let ranged = app
            .clone()
            .oneshot(get(&pack_uri(first_digest), &phone.bearer, Some("bytes=10-19")))
            .await
            .unwrap();
        assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(body(ranged).await, vec![0x71; 10]);
        let refused =
            app.clone().oneshot(get(&pack_uri(first_digest), &other.bearer, None)).await.unwrap();
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);
    }
    // Fetching renewed nothing; an unissued digest has no grace.
    assert_eq!(expiry(), leased_until);
    let unissued = app
        .clone()
        .oneshot(get(&pack_uri(Sha256Digest::of_bytes(b"unissued")), &phone.bearer, None))
        .await
        .unwrap();
    assert_eq!(unissued.status(), StatusCode::NOT_FOUND);
    // A removed grant refuses at once, and comes back with the grant.
    let connection = store.open().unwrap();
    connection
        .execute(
            "DELETE FROM device_vehicle_grants WHERE device_id = ?1",
            rusqlite::params![phone.device_id.to_string()],
        )
        .unwrap();
    let ungranted =
        app.clone().oneshot(get(&pack_uri(first_digest), &phone.bearer, None)).await.unwrap();
    assert_eq!(ungranted.status(), StatusCode::NOT_FOUND);
    connection
        .execute(
            "INSERT INTO device_vehicle_grants(device_id, vehicle_id, granted_at_ms)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![phone.device_id.to_string(), binding.vehicle_id.to_string(), now],
        )
        .unwrap();
    let regranted =
        app.clone().oneshot(get(&pack_uri(first_digest), &phone.bearer, None)).await.unwrap();
    assert_eq!(regranted.status(), StatusCode::OK);
    // Expiry ends it.
    connection
        .execute(
            "UPDATE device_pack_offers SET offered_at_ms = 0, expires_at_ms = 1
              WHERE device_id = ?1",
            rusqlite::params![phone.device_id.to_string()],
        )
        .unwrap();
    let expired =
        app.clone().oneshot(get(&pack_uri(first_digest), &phone.bearer, None)).await.unwrap();
    assert_eq!(expired.status(), StatusCode::NOT_FOUND);
    // Revocation wins.
    store.revoke_device(phone.device_id).expect("revoke");
    let revoked =
        app.clone().oneshot(get(&pack_uri(first_digest), &phone.bearer, None)).await.unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
}

/// Head A of a fresh store, its chunks leased to `phone`, then rotated to B.
struct LeasedRotation {
    _temp: tempfile::TempDir,
    store: HubStore,
    binding: crate::hub_pack::ProjectionBinding,
    first: crate::db::PendingPhysicalV3Admission,
    phone: Paired,
    now: i64,
}

fn leased_rotation() -> LeasedRotation {
    let temp = crate::private_tempdir().expect("private test root");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let key = CursorKey::from_bytes([0x98; 32]);
    let now = current_epoch_ms().expect("clock");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (binding, first) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("a"), &store, &key, 1, "offers98", 1);
    let first = store.stage_pending_physical_v3_admission(&gate, first).expect("A");
    drop(gate);
    let phone = pair(&store, "leased phone", now);
    let offers = first
        .manifest
        .chunks
        .iter()
        .map(|chunk| crate::db::PackOffer {
            digest: chunk.sha256,
            kind: crate::db::PackOfferKind::Physical,
        })
        .collect::<Vec<_>>();
    store
        .record_pack_offers_at(phone.device_id, binding.vehicle_id, &first.receipt_id, &offers, now)
        .expect("A leased");
    rotate(&store, temp.path(), &key, &first, "b", now + 1);
    LeasedRotation { _temp: temp, store, binding, first, phone, now }
}

fn move_generation(store: &HubStore, vehicle_id: Uuid, by: i64) {
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE sources SET generation = generation + ?2 WHERE source_id =
               (SELECT source_id FROM vehicles WHERE vehicle_id = ?1)",
            rusqlite::params![vehicle_id.to_string(), by],
        )
        .unwrap();
}

#[tokio::test]
async fn design_later_98_leased_download_stops_when_the_grant_or_generation_moves() {
    let fixture = leased_rotation();
    let store = &fixture.store;
    let digest = fixture.first.manifest.chunks[0].sha256;
    let device = fixture.phone.device_id;
    let grant = |present: bool| {
        let connection = store.open().unwrap();
        if present {
            connection
                .execute(
                    "INSERT INTO device_vehicle_grants(device_id, vehicle_id, granted_at_ms)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        device.to_string(),
                        fixture.binding.vehicle_id.to_string(),
                        fixture.now
                    ],
                )
                .unwrap();
        } else {
            connection
                .execute(
                    "DELETE FROM device_vehicle_grants WHERE device_id = ?1",
                    rusqlite::params![device.to_string()],
                )
                .unwrap();
        }
    };
    for case in ["grant", "generation"] {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let mut body = verified_pack_body(
            Bytes::from(vec![7; 512 * 1024]),
            slots.clone().acquire_owned().await.expect("slot"),
            PackDeviceSlot {
                device_id: device,
                counts: Arc::new(Mutex::new(HashMap::from([(device, 1)]))),
                authorization: Some(Arc::new(PackStreamAuthorization {
                    store: Arc::new(store.clone()),
                    bearer: zeroize::Zeroizing::new(fixture.phone.bearer.clone()),
                    lease: Some(digest),
                })),
            },
            Duration::from_secs(2),
        );
        let first = body.frame().await.expect("first chunk");
        assert!(first.is_ok(), "{case}: the leased body starts");
        match case {
            "grant" => grant(false),
            _ => move_generation(store, fixture.binding.vehicle_id, 1),
        }
        let next = body.frame().await.expect("refusal frame");
        assert!(next.is_err(), "{case}: the next chunk is refused");
        assert!(body.frame().await.is_none(), "{case}: the body ends");
        match case {
            "grant" => grant(true),
            _ => move_generation(store, fixture.binding.vehicle_id, -1),
        }
    }
}

#[tokio::test]
async fn design_later_98_lease_serves_a_pack_whose_head_rotates_during_lookup() {
    let temp = crate::private_tempdir().expect("private test root");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let key = CursorKey::from_bytes([0x98; 32]);
    let now = current_epoch_ms().expect("clock");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (binding, first) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("a"), &store, &key, 3, "offers98", 1);
    let first = store.stage_pending_physical_v3_admission(&gate, first).expect("A");
    drop(gate);
    let phone = pair(&store, "leased phone", now);
    let offers = first
        .manifest
        .chunks
        .iter()
        .map(|chunk| crate::db::PackOffer {
            digest: chunk.sha256,
            kind: crate::db::PackOfferKind::Physical,
        })
        .collect::<Vec<_>>();
    store
        .record_pack_offers_at(phone.device_id, binding.vehicle_id, &first.receipt_id, &offers, now)
        .expect("A leased");
    let app = paired_router(store.clone(), &key);
    // B replaces A after the lookup found the digest on A, before A loads.
    let (_, successor) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("b"), &store, &key, 1, "offers98", 2);
    let digest = first
        .manifest
        .chunks
        .iter()
        .map(|chunk| chunk.sha256)
        .find(|digest| successor.manifest.chunks.iter().all(|chunk| chunk.sha256 != *digest))
        .expect("a chunk of A that B does not repeat");
    let (writer, vehicle_id, prior) = (store.clone(), binding.vehicle_id, first.receipt_id.clone());
    let rotated = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = rotated.clone();
    crate::db::ROTATE_DURING_PHYSICAL_PACK_LOOKUP.with(|hook| {
        hook.set(Some(Box::new(move || {
            let gate = writer.try_acquire_publication_gate().expect("gate");
            writer
                .rotate_pending_physical_v3_admission_at(&gate, successor, now + 1)
                .expect("rotation");
            writer
                .activate_pending_physical_v3_rotation_at(&gate, vehicle_id, &prior, now + 1)
                .expect("public successor");
            seen.set(true);
        })));
    });
    let served = app.clone().oneshot(get(&pack_uri(digest), &phone.bearer, None)).await.unwrap();
    assert!(rotated.get(), "the head rotated during the lookup");
    assert_eq!(served.status(), StatusCode::OK);
    assert_eq!(Sha256Digest::of_bytes(&body(served).await), digest);
    // Without a lease the same race stays the retryable 503.
    let other = pair(&store, "other phone", now);
    let (_, third) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("c"), &store, &key, 2, "offers98", 3);
    let current = store
        .pending_physical_v3_control_admission_for_vehicle(vehicle_id)
        .expect("head B")
        .expect("B public");
    let unleased = current
        .manifest
        .chunks
        .iter()
        .map(|chunk| chunk.sha256)
        .find(|digest| third.manifest.chunks.iter().all(|chunk| chunk.sha256 != *digest))
        .expect("a chunk of B that C does not repeat");
    let writer = store.clone();
    crate::db::ROTATE_DURING_PHYSICAL_PACK_LOOKUP.with(|hook| {
        hook.set(Some(Box::new(move || {
            let gate = writer.try_acquire_publication_gate().expect("gate");
            writer
                .rotate_pending_physical_v3_admission_at(&gate, third, now + 2)
                .expect("rotation");
            writer
                .activate_pending_physical_v3_rotation_at(&gate, vehicle_id, &current.receipt_id, now + 2)
                .expect("public successor");
        })));
    });
    let refused = app.oneshot(get(&pack_uri(unleased), &other.bearer, None)).await.unwrap();
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn design_later_98_if_range_with_another_validator_gets_the_full_body() {
    let fixture = leased_rotation();
    let app = paired_router(fixture.store.clone(), &CursorKey::from_bytes([0x98; 32]));
    let digest = fixture.first.manifest.chunks[0].sha256;
    let full = fs::read(fixture.store.packs_dir().join("sha256").join(format!("{digest}.sqlite.zst")))
        .expect("leased bytes");
    let request = |if_range: &str| {
        let mut request = get(&pack_uri(digest), &fixture.phone.bearer, Some("bytes=0-9"));
        request
            .headers_mut()
            .insert(header::IF_RANGE, HeaderValue::from_str(if_range).unwrap());
        request
    };
    let other = format!("\"{}\"", Sha256Digest::of_bytes(b"another pack"));
    let weak = format!("W/\"{digest}\"");
    for validator in [other.as_str(), "Sat, 21 Nov 2026 00:00:00 GMT", weak.as_str()] {
        let response = app.clone().oneshot(request(validator)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{validator}");
        assert!(response.headers().get(header::CONTENT_RANGE).is_none());
        assert_eq!(body(response).await, full);
    }
    let matching = app.clone().oneshot(request(&format!("\"{digest}\""))).await.unwrap();
    assert_eq!(matching.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body(matching).await, full[..10]);
}

fn lease_rows(store: &HubStore, device: Uuid) -> i64 {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM device_pack_offers WHERE device_id = ?1",
            rusqlite::params![device.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn design_later_109_head_requests_create_no_lease() {
    let temp = crate::private_tempdir().expect("private test root");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let key = CursorKey::from_bytes([0x09; 32]);
    let now = current_epoch_ms().expect("clock");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (binding, first) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("a"), &store, &key, 1, "offers98", 1);
    let first = store.stage_pending_physical_v3_admission(&gate, first).expect("A");
    drop(gate);
    let (artifact_id, _) = prepared_month(&store, &key, &first, now, 0x7a);
    let phone = pair(&store, "head phone", now);
    let app = paired_router(store.clone(), &key);
    let manifest = |method: &str| {
        Request::builder()
            .method(method)
            .uri(format!("/v1/vehicles/{}/sync/manifest", binding.vehicle_id))
            .header(header::AUTHORIZATION, format!("Bearer {}", phone.bearer))
            .header(SYNC_PROFILE_HEADER, hub_sync_v1::HUB_SYNC_PROFILE_14)
            .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
            .body(Body::empty())
            .unwrap()
    };
    let receipt = |method: &str| {
        Request::builder()
            .method(method)
            .uri(format!(
                "/v1/vehicles/{}/sync/prepared-artefacts/{artifact_id}",
                binding.vehicle_id
            ))
            .header(header::AUTHORIZATION, format!("Bearer {}", phone.bearer))
            .body(Body::empty())
            .unwrap()
    };
    for request in [manifest("HEAD"), receipt("HEAD")] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(lease_rows(&store, phone.device_id), 0);
    for request in [manifest("GET"), receipt("GET")] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(
        lease_rows(&store, phone.device_id),
        i64::try_from(first.manifest.chunks.len()).unwrap() + 1
    );
}

#[tokio::test]
async fn design_later_98_changed_set_with_a_damaged_signature_is_not_leased() {
    let temp = crate::private_tempdir().expect("private test root");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let key = CursorKey::from_bytes([0x98; 32]);
    let now = current_epoch_ms().expect("clock");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (binding, first) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("a"), &store, &key, 1, "offers98", 1);
    let first = store.stage_pending_physical_v3_admission(&gate, first).expect("A");
    let (_, second) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("b"), &store, &key, 3, "offers98", 2);
    let preview =
        crate::db::physical_v3_admission_from_manifest(&second.manifest, binding.selected_car_id)
            .expect("B identity");
    let delta = crate::import::source_db::physical_delta_pack::prepare_changed_set(
        &first,
        &preview,
        &binding,
        &key,
        Sha256Digest::of_bytes(b"offers98 damaged"),
        store.packs_dir(),
        0,
    )
    .expect("changed set");
    let second = store
        .rotate_pending_physical_v3_admission_with_delta_at(&gate, second, Some(delta), now)
        .expect("B");
    store
        .activate_pending_physical_v3_rotation_at(&gate, binding.vehicle_id, &first.receipt_id, now)
        .expect("B public");
    drop(gate);
    let phone = pair(&store, "damaged phone", now);
    let app = paired_router(store.clone(), &key);
    let first_sha = Sha256Digest::of_bytes(
        &ManifestSigning::from_cursor_key(&key)
            .signed_physical_manifest_document(&first)
            .unwrap(),
    );
    let changes = || {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/vehicles/{}/sync/changes-since", binding.vehicle_id))
            .header(header::AUTHORIZATION, format!("Bearer {}", phone.bearer))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "source": {"installation_id":binding.installation_id,"account_id":binding.account_id,
                        "vehicle_id":binding.vehicle_id,"generation":binding.generation,
                        "selected_car_id":binding.selected_car_id},
                    "base_receipt_id":first.receipt_id,"base_manifest_id":first.snapshot_id.to_string(),
                    "base_manifest_sha256":first_sha.to_string(),"base_manifest_schema":"2.2",
                    "from_sequence":first.head_sequence,
                    "schema_version_range":{"minimum":"2.2","maximum":"2.2"},
                    "accepted_changed_set_formats":[hub_sync_v1::PHYSICAL_DELTA_FORMAT],
                }))
                .unwrap(),
            ))
            .unwrap()
    };
    let connection = store.open().unwrap();
    let stored: Vec<u8> = connection
        .query_row(
            "SELECT receipt_json FROM physical_v3_delta_transitions WHERE target_receipt_id = ?1",
            rusqlite::params![second.receipt_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut damaged: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    damaged["signature"]["signature"] = serde_json::json!(format!("{}==", "A".repeat(86)));
    let write = |receipt: &[u8]| {
        connection
            .execute(
                "UPDATE physical_v3_delta_transitions SET receipt_json = ?1
                  WHERE target_receipt_id = ?2",
                rusqlite::params![receipt, second.receipt_id],
            )
            .unwrap();
    };
    write(&serde_json::to_vec(&damaged).unwrap());
    let refused = app.clone().oneshot(changes()).await.unwrap();
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(lease_rows(&store, phone.device_id), 0);
    // The genuine receipt verifies and is leased.
    write(&stored);
    let served = app.clone().oneshot(changes()).await.unwrap();
    assert_eq!(served.status(), StatusCode::OK);
    assert!(lease_rows(&store, phone.device_id) > 0);
}

#[tokio::test]
async fn design_later_109_current_prepared_pack_streams_without_the_lease() {
    let temp = crate::private_tempdir().expect("private test root");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let key = CursorKey::from_bytes([0x09; 32]);
    let now = current_epoch_ms().expect("clock");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (binding, head) =
        crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("a"), &store, &key, 2, "offers98", 1);
    let head = store.stage_pending_physical_v3_admission(&gate, head).expect("A");
    drop(gate);
    let (artifact_id, digest) = prepared_month(&store, &key, &head, now, 0x51);
    let phone = pair(&store, "issued phone", now);
    let app = paired_router(store.clone(), &key);
    let receipt_uri =
        format!("/v1/vehicles/{}/sync/prepared-artefacts/{artifact_id}", binding.vehicle_id);
    let issued = app.clone().oneshot(get(&receipt_uri, &phone.bearer, None)).await.unwrap();
    assert_eq!(issued.status(), StatusCode::OK);
    assert_eq!(lease_rows(&store, phone.device_id), 1, "the issued pack is leased");
    // Both responses start while the lease lives; it ends before delivery.
    let full = app.clone().oneshot(get(&pack_uri(digest), &phone.bearer, None)).await.unwrap();
    let ranged = app
        .clone()
        .oneshot(get(&pack_uri(digest), &phone.bearer, Some("bytes=10-19")))
        .await
        .unwrap();
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE device_pack_offers SET offered_at_ms = 0, expires_at_ms = 1
              WHERE device_id = ?1",
            rusqlite::params![phone.device_id.to_string()],
        )
        .unwrap();
    // The prepared receipt is still current, so neither body depends on the
    // lease.
    assert_eq!(Sha256Digest::of_bytes(&body(full).await), digest);
    assert_eq!(body(ranged).await, vec![0x51; 10]);
}
