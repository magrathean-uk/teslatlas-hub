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
