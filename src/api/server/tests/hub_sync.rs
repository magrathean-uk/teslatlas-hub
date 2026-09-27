// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

fn assert_hub_sync_signature(value: &serde_json::Value, cursor_key: &CursorKey) {
    let signature = value["signature"].as_object().expect("signature object");
    let mut payload = value.clone();
    payload
        .as_object_mut()
        .expect("signed document")
        .remove("signature");
    let canonical = serde_jcs::to_vec(&payload).expect("canonical signed payload");
    assert_eq!(
        signature["signed_payload_sha256"],
        Sha256Digest::of_bytes(&canonical).to_string()
    );
    let expected = ManifestSigning::from_cursor_key(cursor_key);
    assert_eq!(signature["key_id"], expected.key_id());
    let verifying_key_bytes: [u8; 32] = hex::decode(expected.verifying_key_hex())
        .expect("verifying key hex")
        .try_into()
        .expect("32-byte verifying key");
    let verifying_key = VerifyingKey::from_bytes(&verifying_key_bytes).expect("verifying key");
    let signature_bytes = STANDARD
        .decode(signature["signature"].as_str().expect("signature base64"))
        .expect("decode signature");
    let signature = Signature::from_slice(&signature_bytes).expect("64-byte signature");
    verifying_key
        .verify_strict(&canonical, &signature)
        .expect("hub-sync JCS signature");
}

fn assert_hub_sync_signature_rejects(value: &serde_json::Value, cursor_key: &CursorKey) {
    let signature = value["signature"].as_object().expect("signature object");
    let mut payload = value.clone();
    payload
        .as_object_mut()
        .expect("signed document")
        .remove("signature");
    let canonical = serde_jcs::to_vec(&payload).expect("canonical signed payload");
    let signing = ManifestSigning::from_cursor_key(cursor_key);
    let verifying_key_bytes: [u8; 32] = hex::decode(signing.verifying_key_hex())
        .expect("verifying key hex")
        .try_into()
        .expect("32-byte verifying key");
    let verifying_key = VerifyingKey::from_bytes(&verifying_key_bytes).expect("verifying key");
    let signature_bytes = STANDARD
        .decode(signature["signature"].as_str().expect("signature base64"))
        .expect("decode signature");
    let signature = Signature::from_slice(&signature_bytes).expect("64-byte signature");
    assert!(verifying_key.verify_strict(&canonical, &signature).is_err());
}

fn hub_sync_protocol_fixture(name: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../teslatlas-protocol/profiles/hub-sync-v1/1.3.0/examples")
        .join(name);
    serde_json::from_slice(&fs::read(path).expect("read hub-sync Protocol fixture"))
        .expect("parse hub-sync Protocol fixture")
}

#[tokio::test]
async fn hub_sync_bootstrap_manifest_is_signed_and_resumes_with_one_changed_pack() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([88; 32]);
    let (vehicle_id, base_digest, _) = seed_v2_lineage(&store, &cursor_key);
    let delta = append_v2_delta_fixture(
        &store,
        &cursor_key,
        vehicle_id,
        b"bootstrap-compatible-delta-pack",
    );
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage lookup")
        .expect("published lineage");
    let base_pack = &lineage.base.packs[0];
    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("hub sync bootstrap", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let bearer = access.access_token.as_bearer().to_owned();
    let app = paired_router(store, &cursor_key);

    let unauthenticated = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/vehicles/not-a-uuid/sync/manifest")
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .header(SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unauthenticated bootstrap response");
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert!(
        unauthenticated
            .into_body()
            .collect()
            .await
            .expect("unauthenticated bootstrap body")
            .to_bytes()
            .is_empty()
    );

    for name in [
        "bootstrap-profile-selector-invalid.json",
        "bootstrap-profile-selector-missing-schema.json",
        "bootstrap-profile-selector-invalid-schema.json",
        "bootstrap-profile-selector-duplicate.json",
        "bootstrap-profile-selector-duplicate-schema.json",
    ] {
        let fixture = hub_sync_protocol_fixture(name);
        let mut request = Request::builder()
            .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        for header in fixture["request"]["headers"]
            .as_array()
            .expect("selector fixture headers")
        {
            request = request.header(
                header["name"].as_str().expect("selector header name"),
                header["value"].as_str().expect("selector header value"),
            );
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .expect("invalid selector response");
        assert_eq!(
            response.status().as_u16(),
            fixture["expected_response"]["status"]
                .as_u64()
                .and_then(|status| u16::try_from(status).ok())
                .expect("selector fixture status"),
            "{name}"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            fixture["expected_response"]["headers"]["cache_control"]
                .as_str()
                .expect("selector cache-control"),
            "{name}"
        );
        assert!(
            response.headers().get(MANIFEST_SIGNATURE_HEADER).is_none(),
            "{name}"
        );
        assert!(
            response
                .into_body()
                .collect()
                .await
                .expect("invalid selector body")
                .to_bytes()
                .is_empty(),
            "{name}"
        );
    }

    let legacy = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("legacy manifest response");
    assert_eq!(legacy.status(), StatusCode::OK);
    let legacy = response_json(legacy).await;
    assert!(legacy.get("protocol").is_some());
    assert!(legacy.get("receipt_id").is_none());

    let bootstrap = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .header(SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("hub-sync bootstrap response");
    assert_eq!(bootstrap.status(), StatusCode::OK);
    assert!(bootstrap.headers().get(MANIFEST_SIGNATURE_HEADER).is_none());
    let bootstrap = response_json(bootstrap).await;
    assert_eq!(
        bootstrap["manifest_id"],
        lineage.base.snapshot_id.to_string()
    );
    assert_eq!(bootstrap["receipt_id"], base_digest.to_string());
    assert_eq!(bootstrap["vehicle_id"], vehicle_id.to_string());
    assert_eq!(bootstrap["kind"], "snapshot");
    assert_eq!(bootstrap["schema_version"], "2.1");
    assert_eq!(bootstrap["sequence"], lineage.base.sequence);
    assert_eq!(
        bootstrap["pack"]["object_name"],
        format!("{}.sqlite.zst", base_pack.sha256)
    );
    assert_eq!(bootstrap["pack"]["sha256"], base_pack.sha256.to_string());
    assert_eq!(
        bootstrap["pack"]["compressed_bytes"],
        base_pack.compressed_bytes
    );
    assert_hub_sync_signature(&bootstrap, &cursor_key);
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../teslatlas-protocol/profiles/hub-sync-v1/1.3.0/examples/schema-2-1-single-pack-manifest.json"
    )))
    .expect("schema 2.1 Protocol fixture");
    let mut expected = fixture["manifest"].clone();
    expected["manifest_id"] = bootstrap["manifest_id"].clone();
    expected["receipt_id"] = bootstrap["receipt_id"].clone();
    expected["vehicle_id"] = bootstrap["vehicle_id"].clone();
    expected["sequence"] = bootstrap["sequence"].clone();
    expected["pack"] = bootstrap["pack"].clone();
    expected["signature"] = bootstrap["signature"].clone();
    assert_eq!(bootstrap, expected);

    let changed_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/changes-since"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "base_receipt_id": bootstrap["receipt_id"],
                        "base_manifest_schema": bootstrap["schema_version"],
                        "from_sequence": bootstrap["sequence"],
                        "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                    }))
                    .expect("changes-since request"),
                ))
                .unwrap(),
        )
        .await
        .expect("changed-set response");
    assert_eq!(changed_response.status(), StatusCode::OK);
    let changed = response_json(changed_response).await;
    assert_eq!(changed["receipt_id"], delta.chain_digest.to_string());
    assert_eq!(changed["to_sequence"], delta.to_sequence);
    assert_eq!(changed["pack"]["sha256"], delta.pack.sha256.to_string());
    assert_hub_sync_signature(&changed, &cursor_key);

    let noop = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/changes-since"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "base_receipt_id": changed["receipt_id"],
                        "base_manifest_schema": changed["manifest_schema"],
                        "from_sequence": changed["to_sequence"],
                        "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                    }))
                    .expect("no-op request"),
                ))
                .unwrap(),
        )
        .await
        .expect("no-op response");
    assert_eq!(noop.status(), StatusCode::OK);
    let noop = response_json(noop).await;
    assert_eq!(noop["kind"], "no_op");
    assert_eq!(noop["base_receipt_id"], delta.chain_digest.to_string());
    assert_eq!(noop["sequence"], delta.to_sequence);
    assert_hub_sync_signature(&noop, &cursor_key);
}

#[tokio::test]
async fn hub_sync_physical_bootstrap_serves_513_chunks_and_continues_with_noop() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let cursor_key = CursorKey::from_bytes([93; 32]);
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let (binding, candidate) =
        crate::import::teslamate::physical_fragments::tests::public_admission_candidate_fixture(
            &temp.path().join("primary"),
            &store,
            &cursor_key,
            513,
        );
    let admission = store
        .stage_pending_physical_v3_admission(&gate, candidate)
        .expect("primary physical admission");
    let (other_binding, other_candidate) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source(
            &temp.path().join("secondary"),
            &store,
            &cursor_key,
            1,
            "physical-v3-other-vehicle",
        );
    store
        .stage_pending_physical_v3_admission(&gate, other_candidate)
        .expect("secondary physical admission");
    drop(gate);

    assert_eq!(admission.chunk_count, 513);
    assert!(
        store
            .manifest_for_vehicle(binding.vehicle_id)
            .expect("generic manifest lookup")
            .is_none()
    );
    for pack in &admission.manifest.chunks {
        assert!((1..=16 * 1024 * 1024).contains(&pack.compressed_bytes));
        assert!(
            store
                .pack_for_digest(pack.sha256)
                .expect("generic pack lookup")
                .is_none()
        );
    }

    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("physical 1.3 bootstrap", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let bearer = access.access_token.as_bearer().to_owned();
    let app = paired_router(store.clone(), &cursor_key);

    let legacy = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{}/sync/manifest", binding.vehicle_id))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("legacy bootstrap response");
    assert_eq!(legacy.status(), StatusCode::NOT_FOUND);

    let bootstrap = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{}/sync/manifest", binding.vehicle_id))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .header(SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("physical bootstrap response");
    assert_eq!(bootstrap.status(), StatusCode::OK);
    let bootstrap_raw = bootstrap
        .into_body()
        .collect()
        .await
        .expect("physical bootstrap body")
        .to_bytes();
    assert!(bootstrap_raw.len() <= MAX_SYNC_CONTROL_RESPONSE_BYTES);
    let bootstrap: serde_json::Value =
        serde_json::from_slice(&bootstrap_raw).expect("physical bootstrap JSON");
    assert_eq!(bootstrap["manifest_id"], admission.snapshot_id.to_string());
    assert_eq!(bootstrap["receipt_id"], admission.receipt_id);
    assert_eq!(bootstrap["vehicle_id"], binding.vehicle_id.to_string());
    assert_eq!(bootstrap["kind"], "snapshot");
    assert_eq!(bootstrap["schema_version"], "2.2");
    assert_eq!(bootstrap["sequence"], admission.head_sequence);
    let chunks = bootstrap["chunks"].as_array().expect("physical chunks");
    assert_eq!(chunks.len(), 513);
    for (index, (chunk, pack)) in chunks.iter().zip(&admission.manifest.chunks).enumerate() {
        assert_eq!(chunk["chunk_index"], index as u64);
        assert_eq!(chunk["pack"]["sha256"], pack.sha256.to_string());
        assert_eq!(chunk["pack"]["compressed_bytes"], pack.compressed_bytes);
        assert_eq!(
            chunk["pack"]["object_name"],
            format!("{}.sqlite.zst", pack.sha256)
        );
    }
    let fixture = hub_sync_protocol_fixture("schema-2-2-multi-chunk-manifest.json");
    assert_eq!(
        bootstrap
            .as_object()
            .expect("bootstrap object")
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        fixture["manifest"]
            .as_object()
            .expect("fixture manifest")
            .keys()
            .collect::<std::collections::BTreeSet<_>>()
    );
    assert_hub_sync_signature(&bootstrap, &cursor_key);
    assert_hub_sync_signature_rejects(&bootstrap, &CursorKey::from_bytes([94; 32]));

    let first = &admission.manifest.chunks[0];
    let first_name = format!("{}.sqlite.zst", first.sha256);
    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{first_name}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unauthorized physical pack response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert!(
        unauthorized
            .into_body()
            .collect()
            .await
            .expect("unauthorized physical pack body")
            .to_bytes()
            .is_empty()
    );

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{first_name}"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("physical pack response");
    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(
        first_response
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .expect("physical pack ETag"),
        format!("\"{}\"", first.sha256)
    );
    assert_eq!(
        first_response
            .into_body()
            .collect()
            .await
            .expect("physical pack body")
            .to_bytes()
            .as_ref(),
        fs::read(store.packs_dir().join("sha256").join(&first_name))
            .expect("physical pack bytes")
            .as_slice()
    );

    let last = admission.manifest.chunks.last().expect("last chunk");
    let last_name = format!("{}.sqlite.zst", last.sha256);
    let last_bytes = fs::read(store.packs_dir().join("sha256").join(&last_name))
        .expect("last physical pack bytes");
    let ranged = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{last_name}"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::RANGE, "bytes=5-31")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("physical range response");
    assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        ranged
            .into_body()
            .collect()
            .await
            .expect("physical range body")
            .to_bytes()
            .as_ref(),
        &last_bytes[5..=31]
    );

    let changes_request = |vehicle_id, receipt_id: &str| {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/vehicles/{vehicle_id}/sync/changes-since"))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "base_receipt_id": receipt_id,
                    "base_manifest_schema": "2.2",
                    "from_sequence": admission.head_sequence,
                    "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                }))
                .expect("changes-since request"),
            ))
            .unwrap()
    };
    let noop = app
        .clone()
        .oneshot(changes_request(binding.vehicle_id, &admission.receipt_id))
        .await
        .expect("physical no-op response");
    assert_eq!(noop.status(), StatusCode::OK);
    let noop = response_json(noop).await;
    let fixture = hub_sync_protocol_fixture("sync-noop-signed.json");
    let mut expected = fixture["receipt"].clone();
    expected["vehicle_id"] = noop["vehicle_id"].clone();
    expected["base_receipt_id"] = noop["base_receipt_id"].clone();
    expected["sequence"] = noop["sequence"].clone();
    expected["signature"] = noop["signature"].clone();
    assert_eq!(noop, expected);
    assert_hub_sync_signature(&noop, &cursor_key);

    for (vehicle_id, receipt) in [
        (binding.vehicle_id, "pv3_unknown_receipt"),
        (other_binding.vehicle_id, admission.receipt_id.as_str()),
    ] {
        let unknown = app
            .clone()
            .oneshot(changes_request(vehicle_id, receipt))
            .await
            .expect("unknown physical receipt response");
        assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(unknown.headers().get(MANIFEST_SIGNATURE_HEADER).is_none());
        let unknown = response_json(unknown).await;
        assert_eq!(
            unknown,
            hub_sync_protocol_fixture("changes-since-error-unknown-base-receipt.json")["response"]
                ["body"]
        );
    }

    let unsupported_schema = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/v1/vehicles/{}/sync/changes-since",
                    binding.vehicle_id
                ))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "base_receipt_id": admission.receipt_id,
                        "base_manifest_schema": "2.3",
                        "from_sequence": admission.head_sequence,
                        "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                    }))
                    .expect("unsupported base schema request"),
                ))
                .unwrap(),
        )
        .await
        .expect("unsupported base schema response");
    assert_eq!(
        unsupported_schema.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        response_json(unsupported_schema).await,
        serde_json::json!({
            "code": "invalid_request",
            "message": "Request body does not match the changes-since schema."
        })
    );
}

#[tokio::test]
async fn hub_sync_physical_rotation_stays_fail_closed_until_rebase_is_available() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let cursor_key = CursorKey::from_bytes([94; 32]);
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let (binding, first) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("first"),
            &store,
            &cursor_key,
            1,
            "physical-v3-admission",
            1,
        );
    let prior = store
        .stage_pending_physical_v3_admission(&gate, first)
        .expect("first physical admission");
    let (_, second) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("second"),
            &store,
            &cursor_key,
            1,
            "physical-v3-admission",
            2,
        );
    let current = store
        .rotate_pending_physical_v3_admission_at(&gate, second, 1_000)
        .expect("rotated physical admission");
    drop(gate);

    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("blocked physical rotation", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let bearer = access.access_token.as_bearer().to_owned();
    let app = paired_router(store, &cursor_key);
    let bootstrap = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{}/sync/manifest", binding.vehicle_id))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .header(SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("blocked bootstrap response");
    assert_eq!(bootstrap.status(), StatusCode::SERVICE_UNAVAILABLE);
    for pack in prior
        .manifest
        .chunks
        .iter()
        .chain(current.manifest.chunks.iter())
    {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&pack.relative_path)
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("blocked pack response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn hub_sync_physical_retained_prior_rebases_to_the_promoted_successor() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let cursor_key = CursorKey::from_bytes([96; 32]);
    let retained_at_ms = current_epoch_ms().expect("retention clock");

    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let (binding, first) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("primary-first"),
            &store,
            &cursor_key,
            1,
            "physical-v3-rebase-primary",
            1,
        );
    let prior = store
        .stage_pending_physical_v3_admission(&gate, first)
        .expect("first physical admission");
    let (_, second) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("primary-second"),
            &store,
            &cursor_key,
            513,
            "physical-v3-rebase-primary",
            2,
        );
    let current = store
        .rotate_pending_physical_v3_admission_at(&gate, second, retained_at_ms)
        .expect("rotated physical admission");
    assert!(matches!(
        store.activate_pending_physical_v3_rotation_at(
            &gate,
            binding.vehicle_id,
            "pv3_unknown_retained_receipt",
            retained_at_ms,
        ),
        Err(crate::storage::db::StoreError::PhysicalV3AdmissionConflict)
    ));
    let activated = store
        .activate_pending_physical_v3_rotation_at(
            &gate,
            binding.vehicle_id,
            &prior.receipt_id,
            retained_at_ms,
        )
        .expect("activate rotated physical admission");
    assert_eq!(activated, current);

    let (other_binding, other_first) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("other-first"),
            &store,
            &cursor_key,
            1,
            "physical-v3-rebase-other",
            1,
        );
    let other_prior = store
        .stage_pending_physical_v3_admission(&gate, other_first)
        .expect("other first admission");
    let (_, other_second) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("other-second"),
            &store,
            &cursor_key,
            1,
            "physical-v3-rebase-other",
            2,
        );
    store
        .rotate_pending_physical_v3_admission_at(&gate, other_second, retained_at_ms)
        .expect("other rotation");
    store
        .activate_pending_physical_v3_rotation_at(
            &gate,
            other_binding.vehicle_id,
            &other_prior.receipt_id,
            retained_at_ms,
        )
        .expect("activate other rotation");

    let (expired_binding, expired_first) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("expired-first"),
            &store,
            &cursor_key,
            1,
            "physical-v3-rebase-expired",
            1,
        );
    let expired_prior = store
        .stage_pending_physical_v3_admission(&gate, expired_first)
        .expect("expired first admission");
    let (_, expired_second) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("expired-second"),
            &store,
            &cursor_key,
            1,
            "physical-v3-rebase-expired",
            2,
        );
    store
        .rotate_pending_physical_v3_admission_at(&gate, expired_second, 1)
        .expect("expired rotation");
    store
        .activate_pending_physical_v3_rotation_at(
            &gate,
            expired_binding.vehicle_id,
            &expired_prior.receipt_id,
            1,
        )
        .expect("activate before retained expiry");
    drop(gate);

    let invitation = store
        .create_pairing("physical rebase", retained_at_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            retained_at_ms,
        )
        .expect("paired access");
    let bearer = access.access_token.as_bearer().to_owned();
    let app = paired_router(store.clone(), &cursor_key);
    let request = |vehicle_id: Uuid, receipt_id: &str, sequence: u64| {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/vehicles/{vehicle_id}/sync/changes-since"))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "base_receipt_id": receipt_id,
                    "base_manifest_schema": "2.2",
                    "from_sequence": sequence,
                    "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                }))
                .expect("changes-since request"),
            ))
            .unwrap()
    };

    let rebase = app
        .clone()
        .oneshot(request(
            binding.vehicle_id,
            &prior.receipt_id,
            prior.head_sequence,
        ))
        .await
        .expect("physical rebase response");
    assert_eq!(rebase.status(), StatusCode::CONFLICT);
    let rebase_raw = rebase
        .into_body()
        .collect()
        .await
        .expect("physical rebase body")
        .to_bytes();
    assert!(rebase_raw.len() <= MAX_SYNC_CONTROL_RESPONSE_BYTES);
    let rebase: serde_json::Value =
        serde_json::from_slice(&rebase_raw).expect("physical rebase JSON");
    let fixture = hub_sync_protocol_fixture("changes-since-rebase-after-compaction.json");
    assert_eq!(
        rebase
            .as_object()
            .expect("rebase object")
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        fixture["response"]
            .as_object()
            .expect("fixture rebase object")
            .keys()
            .collect::<std::collections::BTreeSet<_>>()
    );
    assert_eq!(
        rebase["replacement"]
            .as_object()
            .expect("replacement object")
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        fixture["response"]["replacement"]
            .as_object()
            .expect("fixture replacement object")
            .keys()
            .collect::<std::collections::BTreeSet<_>>()
    );
    assert_eq!(
        rebase["retry_request"]
            .as_object()
            .expect("retry object")
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        fixture["response"]["retry_request"]
            .as_object()
            .expect("fixture retry object")
            .keys()
            .collect::<std::collections::BTreeSet<_>>()
    );
    assert_eq!(rebase["kind"], "rebase_required");
    assert_eq!(rebase["reason"], "compacted");
    assert_eq!(rebase["vehicle_id"], binding.vehicle_id.to_string());
    assert_eq!(rebase["requested_base_receipt_id"], prior.receipt_id);
    assert_eq!(rebase["requested_base_manifest_schema"], "2.2");
    assert_eq!(rebase["requested_from_sequence"], prior.head_sequence);
    assert_eq!(
        rebase["replacement"]["manifest_id"],
        current.snapshot_id.to_string()
    );
    assert_eq!(rebase["replacement"]["receipt_id"], current.receipt_id);
    assert_eq!(rebase["replacement"]["sequence"], current.head_sequence);
    assert_eq!(rebase["replacement"]["manifest_schema"], "2.2");
    let chunks = rebase["replacement"]["chunks"]
        .as_array()
        .expect("replacement chunks");
    assert_eq!(chunks.len(), 513);
    for (index, (chunk, pack)) in chunks.iter().zip(&current.manifest.chunks).enumerate() {
        assert_eq!(chunk["chunk_index"], index as u64);
        assert_eq!(chunk["pack"]["sha256"], pack.sha256.to_string());
        assert_eq!(chunk["pack"]["compressed_bytes"], pack.compressed_bytes);
        assert_eq!(
            chunk["pack"]["object_name"],
            format!("{}.sqlite.zst", pack.sha256)
        );
    }
    assert_eq!(
        rebase["retry_request"],
        serde_json::json!({
            "base_receipt_id": current.receipt_id,
            "base_manifest_schema": "2.2",
            "from_sequence": current.head_sequence,
            "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
        })
    );
    assert_hub_sync_signature(&rebase, &cursor_key);
    assert_hub_sync_signature_rejects(&rebase, &CursorKey::from_bytes([97; 32]));

    let noop = app
        .clone()
        .oneshot(request(
            binding.vehicle_id,
            &current.receipt_id,
            current.head_sequence,
        ))
        .await
        .expect("active no-op response");
    assert_eq!(noop.status(), StatusCode::OK);
    let noop = response_json(noop).await;
    assert_eq!(noop["kind"], "no_op");
    assert_eq!(noop["vehicle_id"], binding.vehicle_id.to_string());
    assert_eq!(noop["base_receipt_id"], current.receipt_id);
    assert_eq!(noop["base_manifest_schema"], "2.2");
    assert_eq!(noop["sequence"], current.head_sequence);
    assert_eq!(noop["manifest_schema"], "2.2");
    assert_hub_sync_signature(&noop, &cursor_key);

    for (vehicle_id, receipt_id, sequence) in [
        (
            binding.vehicle_id,
            prior.receipt_id.as_str(),
            prior.head_sequence + 1,
        ),
        (
            binding.vehicle_id,
            "pv3_unknown_receipt",
            prior.head_sequence,
        ),
        (
            binding.vehicle_id,
            other_prior.receipt_id.as_str(),
            other_prior.head_sequence,
        ),
        (
            expired_binding.vehicle_id,
            expired_prior.receipt_id.as_str(),
            expired_prior.head_sequence,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(request(vehicle_id, receipt_id, sequence))
            .await
            .expect("unknown retained receipt response");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(response.headers().get(MANIFEST_SIGNATURE_HEADER).is_none());
        assert_eq!(
            response_json(response).await,
            hub_sync_protocol_fixture("changes-since-error-unknown-base-receipt.json")["response"]
                ["body"]
        );
    }

    for pack in [
        &current.manifest.chunks[0],
        current.manifest.chunks.last().expect("last current chunk"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&pack.relative_path)
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("active successor pack response");
        assert_eq!(response.status(), StatusCode::OK);
    }
    let retained_pack = &prior.manifest.chunks[0];
    let response = app
        .oneshot(
            Request::builder()
                .uri(&retained_pack.relative_path)
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("retained prior pack response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn hub_sync_physical_routes_fail_closed_for_retired_rebound_or_tampered_pack() {
    use std::io::Write as _;

    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let cursor_key = CursorKey::from_bytes([95; 32]);
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let (binding, candidate) =
        crate::import::teslamate::physical_fragments::tests::public_admission_candidate_fixture(
            &temp.path().join("candidate"),
            &store,
            &cursor_key,
            2,
        );
    let admission = store
        .stage_pending_physical_v3_admission(&gate, candidate)
        .expect("physical admission");
    drop(gate);
    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("physical fail closed", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let bearer = access.access_token.as_bearer().to_owned();
    let request = || {
        Request::builder()
            .uri(format!("/v1/vehicles/{}/sync/manifest", binding.vehicle_id))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
            .header(SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID)
            .body(Body::empty())
            .unwrap()
    };
    let app = paired_router(store.clone(), &cursor_key);
    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("initial bootstrap")
            .status(),
        StatusCode::OK
    );

    assert!(
        store
            .retire_vehicle(binding.vehicle_id, now_ms + 1)
            .expect("retire vehicle")
    );
    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("retired bootstrap")
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(
        store
            .reactivate_vehicle(binding.vehicle_id)
            .expect("reactivate vehicle")
    );

    let connection = store.open().expect("catalogue");
    connection
        .execute(
            "UPDATE sources SET generation = generation + 1
              WHERE source_id = (
                    SELECT source_id FROM vehicles WHERE vehicle_id = ?1
              )",
            [binding.vehicle_id.to_string()],
        )
        .expect("advance source generation");
    drop(connection);
    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("rebound bootstrap")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let pack = &admission.manifest.chunks[0];
    let pack_uri = format!("/v1/packs/sha256/{}.sqlite.zst", pack.sha256);
    assert_eq!(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri(&pack_uri)
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .expect("rebound pack")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    let connection = store.open().expect("catalogue");
    connection
        .execute(
            "UPDATE sources SET generation = generation - 1
              WHERE source_id = (
                    SELECT source_id FROM vehicles WHERE vehicle_id = ?1
              )",
            [binding.vehicle_id.to_string()],
        )
        .expect("restore source generation");
    drop(connection);
    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("restored bootstrap")
            .status(),
        StatusCode::OK
    );

    let pack_path = store
        .packs_dir()
        .join("sha256")
        .join(format!("{}.sqlite.zst", pack.sha256));
    let pack_metadata = fs::metadata(&pack_path).expect("pack metadata");
    let original_size = pack_metadata.len();
    let original_permissions = pack_metadata.permissions();
    fs::set_permissions(&pack_path, fs::Permissions::from_mode(0o000))
        .expect("make pack unreadable sentinel");
    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("metadata-only bootstrap")
            .status(),
        StatusCode::OK
    );
    let noop = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/v1/vehicles/{}/sync/changes-since",
                    binding.vehicle_id
                ))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "base_receipt_id": admission.receipt_id,
                        "base_manifest_schema": "2.2",
                        "from_sequence": admission.head_sequence,
                        "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                    }))
                    .expect("metadata-only no-op request"),
                ))
                .unwrap(),
        )
        .await
        .expect("metadata-only no-op response");
    assert_eq!(noop.status(), StatusCode::OK);
    assert_eq!(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri(&pack_uri)
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .expect("unreadable pack response")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    fs::set_permissions(&pack_path, original_permissions).expect("restore pack permissions");

    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(&pack_path)
        .expect("pack file");
    file.write_all(&[0]).expect("same-length pack tamper");
    drop(file);
    assert_eq!(
        fs::metadata(&pack_path).expect("tampered metadata").len(),
        original_size
    );
    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("tampered bootstrap")
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.oneshot(
            Request::builder()
                .uri(pack_uri)
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap()
        )
        .await
        .expect("tampered pack")
        .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn hub_sync_physical_routes_do_not_serve_a_non_public_marker() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let cursor_key = CursorKey::from_bytes([96; 32]);
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let (binding, candidate) = crate::import::teslamate::physical_fragments::tests::
        public_admission_candidate_fixture_for_source_and_sequence(
            &temp.path().join("candidate"),
            &store,
            &cursor_key,
            1,
            "physical-v3-non-public-sequence",
            0,
        );
    let admission = store
        .stage_pending_physical_v3_admission(&gate, candidate)
        .expect("private physical marker");
    drop(gate);
    assert_eq!(admission.head_sequence, 0);
    assert!(!crate::db::physical_v3_admission_is_public(&admission));
    assert!(
        store
            .pending_physical_v3_pack_for_digest(admission.manifest.chunks[0].sha256)
            .expect("non-public pack lookup")
            .is_none()
    );

    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("non-public physical marker", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let bearer = access.access_token.as_bearer().to_owned();
    let app = paired_router(store, &cursor_key);

    let bootstrap = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{}/sync/manifest", binding.vehicle_id))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .header(SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("non-public bootstrap response");
    assert_eq!(bootstrap.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        bootstrap.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert!(
        bootstrap
            .into_body()
            .collect()
            .await
            .expect("non-public bootstrap body")
            .to_bytes()
            .is_empty()
    );

    let pack = &admission.manifest.chunks[0];
    let pack_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{}.sqlite.zst", pack.sha256))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("non-public pack response");
    assert_eq!(pack_response.status(), StatusCode::NOT_FOUND);

    let changes = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/v1/vehicles/{}/sync/changes-since",
                    binding.vehicle_id
                ))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "base_receipt_id": admission.receipt_id,
                        "base_manifest_schema": "2.2",
                        "from_sequence": 0,
                        "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                    }))
                    .expect("non-public changes request"),
                ))
                .unwrap(),
        )
        .await
        .expect("non-public changes response");
    assert_eq!(changes.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        changes.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
}

#[tokio::test]
async fn hub_sync_bootstrap_sequence_is_i_json_bounded_without_changing_legacy() {
    const MAX_I_JSON_INTEGER: u64 = 9_007_199_254_740_991;

    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([91; 32]);
    let (vehicle_id, _, _) = seed_v2_lineage(&store, &cursor_key);
    set_v2_base_sequence(&store, &cursor_key, vehicle_id, MAX_I_JSON_INTEGER);
    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("I-JSON sequence admission", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let bearer = access.access_token.as_bearer().to_owned();
    let app = paired_router(store.clone(), &cursor_key);
    let request = || {
        Request::builder()
            .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
            .header(SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID)
            .body(Body::empty())
            .unwrap()
    };

    let at_limit = app
        .clone()
        .oneshot(request())
        .await
        .expect("at-limit bootstrap response");
    assert_eq!(at_limit.status(), StatusCode::OK);
    let at_limit = response_json(at_limit).await;
    assert_eq!(at_limit["sequence"], MAX_I_JSON_INTEGER);
    assert_hub_sync_signature(&at_limit, &cursor_key);

    let unsafe_fixture = hub_sync_protocol_fixture("schema-2-1-manifest-unsafe-integer.json");
    let unsafe_sequence = unsafe_fixture["manifest"]["sequence"]
        .as_u64()
        .expect("unsafe fixture sequence");
    assert!(unsafe_sequence > MAX_I_JSON_INTEGER);
    set_v2_base_sequence(&store, &cursor_key, vehicle_id, unsafe_sequence);
    let rejected = app
        .clone()
        .oneshot(request())
        .await
        .expect("unsafe bootstrap response");
    assert_eq!(rejected.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        rejected.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert!(rejected.headers().get(MANIFEST_SIGNATURE_HEADER).is_none());
    assert!(
        rejected
            .into_body()
            .collect()
            .await
            .expect("unsafe bootstrap body")
            .to_bytes()
            .is_empty()
    );

    let legacy = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("legacy manifest response");
    assert_eq!(legacy.status(), StatusCode::OK);
    let legacy = response_json(legacy).await;
    assert!(legacy.get("protocol").is_some());
    assert!(legacy.get("receipt_id").is_none());
}

#[tokio::test]
async fn hub_sync_bootstrap_rejects_a_multi_pack_internal_base() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([89; 32]);
    let (vehicle_id, _, _) = seed_v2_lineage(&store, &cursor_key);
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage lookup")
        .expect("published lineage");
    let second_bytes = b"second-internal-base-pack";
    let second_digest = Sha256Digest::of_bytes(second_bytes);
    let mut second = lineage.base.packs[0].clone();
    second.pack_id = Uuid::new_v4();
    second.ordinal = 1;
    second.sha256 = second_digest;
    second.relative_path = TransportPack::canonical_relative_path(second_digest);
    second.compressed_bytes = u64::try_from(second_bytes.len()).expect("second pack size");
    fs::write(
        store
            .packs_dir()
            .join("sha256")
            .join(format!("{second_digest}.sqlite.zst")),
        second_bytes,
    )
    .expect("second base pack");
    let mut packs = lineage.base.packs.clone();
    packs.push(second.clone());
    let connection = store.open().expect("base catalogue");
    connection
        .execute(
            "UPDATE sync_bases SET packs_json = ?1 WHERE vehicle_id = ?2",
            rusqlite::params![
                serde_json::to_vec(&packs).expect("base packs JSON"),
                vehicle_id.to_string()
            ],
        )
        .expect("multi-pack base catalogue");
    connection
        .execute(
            "INSERT INTO sync_packs(
                sha256, snapshot_id, ordinal, relative_path,
                compressed_bytes, uncompressed_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                second.sha256.to_string(),
                second.snapshot_id.to_string(),
                i64::from(second.ordinal),
                second.relative_path,
                second.compressed_bytes as i64,
                second.uncompressed_bytes as i64,
            ],
        )
        .expect("second base pack catalogue");
    drop(connection);
    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("multi-pack admission", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let response = paired_router(store, &cursor_key)
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", access.access_token.as_bearer()),
                )
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .header(SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("multi-pack bootstrap response");
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert!(
        response
            .into_body()
            .collect()
            .await
            .expect("unavailable body")
            .to_bytes()
            .is_empty()
    );
}

#[tokio::test]
async fn hub_sync_bootstrap_rejects_an_internal_base_over_sixteen_mib() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([90; 32]);
    let (vehicle_id, old_digest, _) = seed_v2_lineage(&store, &cursor_key);
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage lookup")
        .expect("published lineage");
    let oversized_bytes = vec![b'x'; 16 * 1024 * 1024 + 1];
    let oversized_digest = Sha256Digest::of_bytes(&oversized_bytes);
    let mut pack = lineage.base.packs[0].clone();
    pack.sha256 = oversized_digest;
    pack.relative_path = TransportPack::canonical_relative_path(oversized_digest);
    pack.compressed_bytes = u64::try_from(oversized_bytes.len()).expect("oversized pack size");
    fs::write(
        store
            .packs_dir()
            .join("sha256")
            .join(format!("{oversized_digest}.sqlite.zst")),
        &oversized_bytes,
    )
    .expect("oversized base pack");
    let connection = store.open().expect("base catalogue");
    connection
        .execute(
            "UPDATE sync_bases
                SET base_digest = ?1, packs_json = ?2
              WHERE vehicle_id = ?3",
            rusqlite::params![
                oversized_digest.to_string(),
                serde_json::to_vec(&vec![pack.clone()]).expect("base packs JSON"),
                vehicle_id.to_string(),
            ],
        )
        .expect("oversized base catalogue");
    connection
        .execute(
            "UPDATE sync_heads SET head_digest = ?1 WHERE vehicle_id = ?2",
            rusqlite::params![oversized_digest.to_string(), vehicle_id.to_string()],
        )
        .expect("oversized head catalogue");
    connection
        .execute(
            "DELETE FROM sync_packs WHERE sha256 = ?1",
            rusqlite::params![old_digest.to_string()],
        )
        .expect("remove original pack catalogue");
    connection
        .execute(
            "INSERT INTO sync_packs(
                sha256, snapshot_id, ordinal, relative_path,
                compressed_bytes, uncompressed_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                pack.sha256.to_string(),
                pack.snapshot_id.to_string(),
                i64::from(pack.ordinal),
                pack.relative_path,
                pack.compressed_bytes as i64,
                pack.uncompressed_bytes as i64,
            ],
        )
        .expect("oversized pack catalogue");
    drop(connection);
    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("oversized admission", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let response = paired_router(store, &cursor_key)
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", access.access_token.as_bearer()),
                )
                .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                .header(SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oversized bootstrap response");
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert!(
        response
            .into_body()
            .collect()
            .await
            .expect("unavailable body")
            .to_bytes()
            .is_empty()
    );
}

#[tokio::test]
async fn changes_since_serves_one_changed_pack_then_a_signed_noop() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([86; 32]);
    let (vehicle_id, base_digest, _) = seed_v2_lineage(&store, &cursor_key);
    let delta =
        append_v2_delta_fixture(&store, &cursor_key, vehicle_id, b"changes-since-delta-pack");
    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("changes since ledger", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let bearer = access.access_token.as_bearer().to_owned();
    let app = paired_router(store, &cursor_key);
    let request = |receipt: String, sequence: u64| {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/vehicles/{vehicle_id}/sync/changes-since"))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "base_receipt_id": receipt,
                    "base_manifest_schema": "2.1",
                    "from_sequence": sequence,
                    "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                }))
                .expect("request JSON"),
            ))
            .unwrap()
    };

    let changed = app
        .clone()
        .oneshot(request(base_digest.to_string(), delta.from_sequence))
        .await
        .expect("changed-set response");
    assert_eq!(changed.status(), StatusCode::OK);
    assert!(changed.headers().get(header::CACHE_CONTROL).is_none());
    let changed = response_json(changed).await;
    assert_eq!(changed["vehicle_id"], vehicle_id.to_string());
    assert_eq!(changed["base_receipt_id"], base_digest.to_string());
    assert_eq!(changed["base_manifest_schema"], "2.1");
    assert_eq!(changed["from_sequence"], delta.from_sequence);
    assert_eq!(changed["to_sequence"], delta.to_sequence);
    assert_eq!(changed["manifest_schema"], "2.1");
    assert_eq!(changed["receipt_id"], delta.chain_digest.to_string());
    assert_eq!(
        changed["changed_set_sha256"],
        delta.chain_digest.to_string()
    );
    assert_eq!(changed["pack"]["sha256"], delta.pack.sha256.to_string());
    assert_eq!(
        changed["pack"]["object_name"],
        format!("{}.sqlite.zst", delta.pack.sha256)
    );
    assert_eq!(
        changed["pack"]["compressed_bytes"],
        delta.pack.compressed_bytes
    );
    assert_hub_sync_signature(&changed, &cursor_key);

    let noop = app
        .oneshot(request(delta.chain_digest.to_string(), delta.to_sequence))
        .await
        .expect("no-op response");
    assert_eq!(noop.status(), StatusCode::OK);
    let noop = response_json(noop).await;
    assert_eq!(noop["kind"], "no_op");
    assert_eq!(noop["vehicle_id"], vehicle_id.to_string());
    assert_eq!(noop["base_receipt_id"], delta.chain_digest.to_string());
    assert_eq!(noop["base_manifest_schema"], "2.1");
    assert_eq!(noop["sequence"], delta.to_sequence);
    assert_eq!(noop["manifest_schema"], "2.1");
    assert_hub_sync_signature(&noop, &cursor_key);
}

#[tokio::test]
async fn changes_since_rebases_retained_checkpoint_and_rejects_unknown_receipt() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([87; 32]);
    let (vehicle_id, base_digest, _) = seed_v2_lineage(&store, &cursor_key);
    let base = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("base lineage lookup")
        .expect("base lineage");
    let current = append_v2_delta_fixture(
        &store,
        &cursor_key,
        vehicle_id,
        b"current-compacted-delta-pack",
    );

    let retired_bytes = b"retired-pre-compaction-delta-pack";
    let retired_digest = Sha256Digest::of_bytes(retired_bytes);
    let retired_pack = TransportPack {
        pack_id: Uuid::new_v4(),
        snapshot_id: base.base.snapshot_id,
        ordinal: 1,
        schema: HUB_PROJECTION_SCHEMA_V2,
        format: PackFormat::HubProjectionSqlite,
        compression: PackCompression::Zstd,
        relative_path: TransportPack::canonical_relative_path(retired_digest),
        sha256: retired_digest,
        compressed_bytes: u64::try_from(retired_bytes.len()).expect("retired pack size"),
        uncompressed_bytes: 100,
        row_count: 1,
        sequence: SequenceRange {
            from_exclusive: base.head_sequence,
            to_inclusive: base.head_sequence + 1,
        },
        tables: vec![MirrorTable::Car],
    };
    let retired_delta = LineageDelta {
        from_sequence: base.head_sequence,
        to_sequence: base.head_sequence + 1,
        parent_chain_digest: base.head_digest,
        chain_digest: canonical_delta_chain_digest(base.head_digest, retired_digest),
        pack_digest: retired_digest,
        pack: retired_pack,
    };
    let mut retired = base.clone();
    retired.deltas.push(retired_delta.clone());
    retired.head_sequence = retired_delta.to_sequence;
    retired.head_digest = retired_delta.chain_digest;
    let binding = store
        .v2_projection_binding(vehicle_id)
        .expect("projection binding");
    retired.terminal_cursor = OpaqueCursor::issue(
        &cursor_key,
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id,
            generation: binding.generation,
            sequence: retired.head_sequence,
        },
    )
    .expect("retired cursor");
    retired.validate().expect("retired lineage");
    let retired_at_ms = current_epoch_ms().expect("retirement clock");
    store
        .open()
        .expect("retired catalogue")
        .execute(
            "INSERT INTO sync_retired_lineages(
                vehicle_id, head_digest, manifest_json, retired_at_ms, expires_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                vehicle_id.to_string(),
                retired.head_digest.to_string(),
                serde_json::to_vec(&retired).expect("retired lineage JSON"),
                retired_at_ms,
                retired_at_ms + 60_000,
            ],
        )
        .expect("retired lineage catalogue");
    let invitation = store
        .create_pairing("changes since compaction", retired_at_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            retired_at_ms,
        )
        .expect("paired access");
    let bearer = access.access_token.as_bearer().to_owned();
    let app = paired_router(store, &cursor_key);
    let request = |receipt: &str, sequence: u64| {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/vehicles/{vehicle_id}/sync/changes-since"))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "base_receipt_id": receipt,
                    "base_manifest_schema": "2.1",
                    "from_sequence": sequence,
                    "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}
                }))
                .expect("request JSON"),
            ))
            .unwrap()
    };

    let rebase = app
        .clone()
        .oneshot(request(
            &retired_delta.chain_digest.to_string(),
            retired_delta.to_sequence,
        ))
        .await
        .expect("rebase response");
    assert_eq!(rebase.status(), StatusCode::CONFLICT);
    let rebase = response_json(rebase).await;
    assert_eq!(rebase["kind"], "rebase_required");
    assert_eq!(rebase["reason"], "compacted");
    assert_eq!(rebase["vehicle_id"], vehicle_id.to_string());
    assert_eq!(
        rebase["requested_base_receipt_id"],
        retired_delta.chain_digest.to_string()
    );
    assert_eq!(rebase["requested_from_sequence"], retired_delta.to_sequence);
    assert_eq!(rebase["replacement"]["receipt_id"], base_digest.to_string());
    assert_eq!(rebase["replacement"]["sequence"], base.base.sequence);
    assert_eq!(rebase["replacement"]["manifest_schema"], "2.1");
    assert_eq!(
        rebase["replacement"]["manifest_id"],
        base.base.snapshot_id.to_string()
    );
    assert_eq!(
        rebase["retry_request"]["base_receipt_id"],
        base_digest.to_string()
    );
    assert_eq!(rebase["retry_request"]["from_sequence"], base.base.sequence);
    assert_eq!(
        rebase["retry_request"]["schema_version_range"],
        serde_json::json!({"minimum": "2.1", "maximum": "2.2"})
    );
    assert_hub_sync_signature(&rebase, &cursor_key);

    let unknown = app
        .oneshot(request("receipt_unknown_999999", 42))
        .await
        .expect("unknown receipt response");
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(unknown.headers().get(header::CACHE_CONTROL).is_none());
    let unknown = response_json(unknown).await;
    let expected: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../teslatlas-protocol/profiles/hub-sync-v1/1.3.0/examples/changes-since-error-unknown-base-receipt.json"
    )))
    .expect("unknown-receipt fixture");
    assert_eq!(unknown, expected["response"]["body"]);

    assert_ne!(current.chain_digest, retired_delta.chain_digest);
}
