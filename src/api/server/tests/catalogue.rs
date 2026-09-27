// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

#[tokio::test]
async fn serves_catalogued_manifest_and_immutable_pack_stream() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let installation_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();
    let vehicle_id = Uuid::new_v4();
    seed_active_vehicle_identity(&store, account_id, vehicle_id);
    let snapshot_id = Uuid::new_v4();
    let rows = vec![TransportRow {
        table: MirrorTable::Position,
        entity_key: "position:1".to_owned(),
        source_sequence: 5,
        operation: TransportOperation::Upsert,
        values: BTreeMap::from([
            ("latitude".to_owned(), TransportValue::Real(51.5072)),
            ("longitude".to_owned(), TransportValue::Real(-0.1276)),
        ]),
    }];
    let tables = [MirrorTable::Position];
    let built = TransportPackWriter::new(store.packs_dir())
        .write_pack(&TransportPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id,
            ordinal: 0,
            schema: TRANSPORT_SCHEMA_V1,
            mode: TransferMode::FullSnapshot,
            sequence: SequenceRange {
                from_exclusive: 5,
                to_inclusive: 5,
            },
            tables: &tables,
            rows: &rows,
        })
        .expect("build transport pack");
    let cursor_key = CursorKey::from_bytes([9; 32]);
    let cursor = OpaqueCursor::issue(
        &cursor_key,
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: TRANSPORT_SCHEMA_V1,
            installation_id,
            account_id,
            vehicle_id,
            generation: 1,
            sequence: 5,
        },
    )
    .expect("cursor");
    let manifest = SyncManifest {
        protocol: ProtocolVersion { major: 1, minor: 0 },
        schema: TRANSPORT_SCHEMA_V1,
        installation_id,
        account_id,
        vehicle_id,
        generation: 1,
        snapshot_id,
        mode: TransferMode::FullSnapshot,
        base_sequence: 5,
        head_sequence: 5,
        chunk_count: 1,
        total_compressed_bytes: built.metadata.compressed_bytes,
        total_uncompressed_bytes: built.metadata.uncompressed_bytes,
        total_rows: built.metadata.row_count,
        chunks: vec![built.metadata.clone()],
        terminal_cursor: cursor,
    };
    store.publish_manifest(&manifest).expect("publish manifest");
    let expected_pack = fs::read(&built.path).expect("pack bytes");
    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("manifest test", now_ms - 1, i64::MAX)
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
    let verifying_key_hex = ManifestSigning::from_cursor_key(&cursor_key).verifying_key_hex();
    let app = paired_router(store, &cursor_key);

    let manifest_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("manifest response");
    assert_eq!(manifest_response.status(), StatusCode::OK);
    assert_eq!(
        manifest_response
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let signature_header = manifest_response
        .headers()
        .get(MANIFEST_SIGNATURE_HEADER)
        .expect("manifest signature")
        .to_str()
        .expect("ASCII signature")
        .to_owned();
    let raw_manifest = manifest_response
        .into_body()
        .collect()
        .await
        .expect("manifest body")
        .to_bytes();
    assert_eq!(
        serde_json::from_slice::<SyncManifest>(&raw_manifest).expect("manifest JSON"),
        manifest
    );
    let verifying_key_bytes: [u8; 32] = hex::decode(verifying_key_hex)
        .expect("verifying key hex")
        .try_into()
        .expect("32-byte verifying key");
    let verifying_key = VerifyingKey::from_bytes(&verifying_key_bytes).expect("verifying key");
    let signature = Signature::from_slice(
        &STANDARD
            .decode(signature_header)
            .expect("base64 manifest signature"),
    )
    .expect("64-byte manifest signature");
    verifying_key
        .verify_strict(&raw_manifest, &signature)
        .expect("exact raw manifest verifies");
    let mut mutated_manifest = raw_manifest.to_vec();
    mutated_manifest[0] ^= 1;
    assert!(
        verifying_key
            .verify_strict(&mutated_manifest, &signature)
            .is_err()
    );

    let pack_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/packs/sha256/{}.sqlite.zst",
                    built.metadata.sha256
                ))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("pack response");
    assert_eq!(pack_response.status(), StatusCode::OK);
    assert_eq!(
        pack_response.headers().get(header::CACHE_CONTROL).unwrap(),
        "private, max-age=31536000, immutable"
    );
    assert_eq!(
        pack_response.headers().get(header::ETAG).unwrap(),
        &built.metadata.etag()
    );
    assert_eq!(
        pack_response.headers().get(header::CONTENT_LENGTH).unwrap(),
        built.metadata.compressed_bytes.to_string().as_str()
    );
    let delivered = pack_response
        .into_body()
        .collect()
        .await
        .expect("streamed body")
        .to_bytes();
    assert_eq!(delivered.as_ref(), expected_pack.as_slice());

    let partial = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/packs/sha256/{}.sqlite.zst",
                    built.metadata.sha256
                ))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::RANGE, "bytes=1-8")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("partial pack response");
    assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        partial.headers().get(header::ACCEPT_RANGES).unwrap(),
        "bytes"
    );
    assert_eq!(
        partial.headers().get(header::CONTENT_RANGE).unwrap(),
        format!("bytes 1-8/{}", expected_pack.len()).as_str()
    );
    let partial_bytes = partial
        .into_body()
        .collect()
        .await
        .expect("partial body")
        .to_bytes();
    assert_eq!(partial_bytes.as_ref(), &expected_pack[1..=8]);

    let unsatisfiable = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/packs/sha256/{}.sqlite.zst",
                    built.metadata.sha256
                ))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::RANGE, "bytes=999999-")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("range response");
    assert_eq!(unsatisfiable.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        unsatisfiable.headers().get(header::CONTENT_RANGE).unwrap(),
        format!("bytes */{}", expected_pack.len()).as_str()
    );
}

#[tokio::test]
async fn trusted_local_schema_22_uses_the_active_cursor_key() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([72; 32]);
    let (built, snapshot) =
        write_updates_schema_22_pack(store.packs_dir(), Vec::new()).expect("schema 2.2 pack");
    let request = updates_pack_request(&snapshot);
    let manifest = sign_updates_schema_22_manifest(&request, &built, &cursor_key)
        .expect("schema 2.2 manifest");
    let noop = sign_updates_schema_22_noop(
        &request.binding,
        request.snapshot_id,
        request.sequence.to_inclusive,
        &built.metadata.sha256.to_string(),
        &cursor_key,
    )
    .expect("schema 2.2 no-op");
    seed_active_vehicle_identity(
        &store,
        request.binding.account_id,
        request.binding.vehicle_id,
    );
    publish_updates_schema_22(&store, &manifest, &noop).expect("publish pair");

    let app = trusted_local_router(store, false, Some(cursor_key), None);
    for endpoint in ["manifest", "noop"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/vehicles/{}/sync/{endpoint}",
                        request.binding.vehicle_id
                    ))
                    .header(SUPPORTED_SCHEMAS_HEADER, "2.2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("trusted local schema 2.2 response");
        assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
        assert!(
            response.headers().contains_key(MANIFEST_SIGNATURE_HEADER),
            "{endpoint} is signed by the active cursor key"
        );
    }
}

#[tokio::test]
async fn schema_22_noop_without_active_key_is_permanently_unavailable() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source_id = Uuid::new_v4();
    let vehicle_id = Uuid::new_v4();
    seed_active_vehicle_identity(&store, source_id, vehicle_id);

    let response = trusted_local_router(store, false, None, None)
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/noop"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("no-op response");
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert!(response.headers().get(MANIFEST_SIGNATURE_HEADER).is_none());
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
}

#[tokio::test]
async fn paired_schema_22_restart_keeps_exact_noop_and_wrong_key_fails_closed() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store_path = temp.path().join("store");
    let store = HubStore::initialize(&store_path).expect("store");
    let cursor_key = CursorKey::from_bytes([73; 32]);
    let (built, snapshot) =
        write_updates_schema_22_pack(store.packs_dir(), Vec::new()).expect("schema 2.2 pack");
    let request = updates_pack_request(&snapshot);
    let manifest = sign_updates_schema_22_manifest(&request, &built, &cursor_key)
        .expect("schema 2.2 manifest");
    let noop = sign_updates_schema_22_noop(
        &request.binding,
        request.snapshot_id,
        request.sequence.to_inclusive,
        &built.metadata.sha256.to_string(),
        &cursor_key,
    )
    .expect("schema 2.2 no-op");
    seed_active_vehicle_identity(
        &store,
        request.binding.account_id,
        request.binding.vehicle_id,
    );
    publish_updates_schema_22(&store, &manifest, &noop).expect("publish pair");

    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("schema 2.2 no-op test", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "test client",
            now_ms,
        )
        .expect("paired access");
    let restarted = HubStore::initialize(&store_path).expect("restart store");
    let app = paired_router(restarted.clone(), &cursor_key);
    let selected = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/sync/manifest",
                    request.binding.vehicle_id
                ))
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
        .expect("selected physical bootstrap response");
    assert_eq!(selected.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        selected.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert!(
        selected
            .into_body()
            .collect()
            .await
            .expect("selected physical bootstrap body")
            .to_bytes()
            .is_empty(),
        "updates-only schema 2.2 publication must not qualify as PhysicalV3"
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/sync/noop",
                    request.binding.vehicle_id
                ))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", access.access_token.as_bearer()),
                )
                .header(SUPPORTED_SCHEMAS_HEADER, "2.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("no-op response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let signature_header = response
        .headers()
        .get(MANIFEST_SIGNATURE_HEADER)
        .expect("no-op signature")
        .to_str()
        .expect("ASCII signature")
        .to_owned();
    let raw_noop = response
        .into_body()
        .collect()
        .await
        .expect("no-op body")
        .to_bytes();
    assert_eq!(
        serde_json::from_slice::<crate::updates_delivery::SignedNoOpState>(&raw_noop)
            .expect("no-op JSON"),
        noop
    );

    let verifying_key_bytes: [u8; 32] =
        hex::decode(ManifestSigning::from_cursor_key(&cursor_key).verifying_key_hex())
            .expect("verifying key hex")
            .try_into()
            .expect("32-byte verifying key");
    let verifying_key = VerifyingKey::from_bytes(&verifying_key_bytes).expect("verifying key");
    let signature = Signature::from_slice(
        &STANDARD
            .decode(signature_header)
            .expect("base64 no-op signature"),
    )
    .expect("64-byte no-op signature");
    verifying_key
        .verify_strict(&raw_noop, &signature)
        .expect("exact raw no-op verifies");
    let mut mutated_noop = raw_noop.to_vec();
    mutated_noop[0] ^= 1;
    assert!(
        verifying_key
            .verify_strict(&mutated_noop, &signature)
            .is_err()
    );

    let wrong_key = CursorKey::from_bytes([74; 32]);
    let wrong_key_response = paired_router(restarted, &wrong_key)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/sync/noop",
                    request.binding.vehicle_id
                ))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", access.access_token.as_bearer()),
                )
                .header(SUPPORTED_SCHEMAS_HEADER, "2.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("wrong-key no-op response");
    assert_eq!(wrong_key_response.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        wrong_key_response
            .headers()
            .get(header::CACHE_CONTROL)
            .expect("wrong-key cache policy"),
        "no-store"
    );
    assert!(
        wrong_key_response
            .headers()
            .get(MANIFEST_SIGNATURE_HEADER)
            .is_none(),
        "unavailable no-op must not carry a signature"
    );
    let error_body = wrong_key_response
        .into_body()
        .collect()
        .await
        .expect("wrong-key error body")
        .to_bytes();
    assert!(
        error_body.is_empty(),
        "unavailable no-op body must be empty"
    );
}

#[tokio::test]
async fn explicit_delta_v2_returns_validated_lineage_and_authorized_packs() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([41; 32]);
    let (vehicle_id, digest, _) = seed_v2_lineage(&store, &cursor_key);
    let app = router(store);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .header(SYNC_CAPABILITY_HEADER, DELTA_V2_CAPABILITY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("v2 response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/vnd.teslatlas.sync-lineage+json"
    );
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let body = response
        .into_body()
        .collect()
        .await
        .expect("lineage body")
        .to_bytes();
    let lineage: LineageManifestV2 = serde_json::from_slice(&body).expect("lineage JSON");
    lineage.validate().expect("validated lineage");
    assert_eq!(lineage.vehicle_id, vehicle_id);
    assert_eq!(lineage.base.digest, digest);

    let pack = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{digest}.sqlite.zst"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("pack response");
    assert_eq!(pack.status(), StatusCode::OK);

    let unauthorized_digest = Sha256Digest::of_bytes(b"not-catalogued");
    let missing = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{unauthorized_digest}.sqlite.zst"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("missing pack response");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn imported_changed_history_serves_a_valid_typed_delta() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([44; 32]);
    let request = TeslaMateImportRequest {
        source_key: "server-import".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let mut history = TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: 440,
            vid: Some(441),
            vin: Some("5YJTESTSERVER00440".into()),
            name: Some("Server route car".into()),
            model: Some("3".into()),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: Default::default(),
        }],
        drives: vec![],
        positions: vec![],
        charging_processes: vec![],
        charges: vec![],
        addresses: vec![],
        geofences: vec![],
        states: vec![],
        updates: vec![],
    };
    let first = publish_history(&store, &cursor_key, &request, &history).expect("base import");
    history.drives.push(TeslaMateDrive {
        id: 440,
        car_id: 1,
        start_date_ms: 2_000,
        end_date_ms: Some(3_000),
        outside_temp_avg: None,
        speed_max: Some(40),
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_rated_range_km: Some(300.0),
        end_rated_range_km: Some(280.0),
        start_km: Some(10.0),
        end_km: Some(20.0),
        distance_km: Some(10.0),
        duration_min: Some(1),
        start_address_id: None,
        end_address_id: None,
        start_geofence_id: None,
        end_geofence_id: None,
        start_position_id: None,
        end_position_id: None,
        ascent: None,
        descent: None,
        inside_temp_avg: None,
    });
    let second =
        publish_history(&store, &cursor_key, &request, &history).expect("typed-delta successor");
    assert_eq!(second.snapshot_id, first.snapshot_id);

    let app = router(store);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{}/sync/manifest", first.vehicle_id))
                .header(SYNC_CAPABILITY_HEADER, DELTA_V2_CAPABILITY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("lineage response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("lineage body")
        .to_bytes();
    let lineage: LineageManifestV2 = serde_json::from_slice(&body).expect("lineage JSON");
    lineage.validate().expect("client-valid lineage");
    assert_eq!(lineage.base.snapshot_id, first.snapshot_id);
    assert_eq!(lineage.head_sequence, second.sequence);
    assert_eq!(lineage.deltas.len(), 1, "one changed-history delta");
    let delta = &lineage.deltas[0];

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{}.sqlite.zst", delta.pack.sha256))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("delta pack response");
    assert_eq!(response.status(), StatusCode::OK);
    let delta_bytes = response
        .into_body()
        .collect()
        .await
        .expect("delta pack body")
        .to_bytes();
    assert_eq!(Sha256Digest::of_bytes(&delta_bytes), delta.pack.sha256);

    let inspection_path = temp.path().join("served-delta.sqlite");
    fs::write(
        &inspection_path,
        zstd::stream::decode_all(delta_bytes.as_ref()).expect("decode served delta"),
    )
    .expect("write served delta inspection database");
    let inspection = rusqlite::Connection::open(inspection_path).expect("open served delta");
    let mode: String = inspection
        .query_row(
            "SELECT value FROM hub_pack_metadata WHERE key = 'mode'",
            [],
            |row| row.get(0),
        )
        .expect("delta mode");
    let drive_id: i64 = inspection
        .query_row("SELECT id FROM drives", [], |row| row.get(0))
        .expect("changed drive");
    assert_eq!(mode, "typed_delta");
    assert_eq!(drive_id, 440);
}

#[tokio::test]
async fn restart_serves_unexpired_prior_lineage_pack_but_never_an_arbitrary_orphan() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([45; 32]);
    let (vehicle_id, _, _) = seed_v2_lineage(&store, &cursor_key);
    let binding = store
        .v2_projection_binding(vehicle_id)
        .expect("immutable binding");
    let mut prior = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage lookup")
        .expect("base lineage");
    let retired_bytes = b"server-retired-delta";
    let retired_digest = Sha256Digest::of_bytes(retired_bytes);
    let parent_digest = prior.head_digest;
    let pack = TransportPack {
        pack_id: Uuid::new_v4(),
        snapshot_id: prior.base.snapshot_id,
        ordinal: 1,
        schema: HUB_PROJECTION_SCHEMA_V2,
        format: PackFormat::HubProjectionSqlite,
        compression: PackCompression::Zstd,
        relative_path: TransportPack::canonical_relative_path(retired_digest),
        sha256: retired_digest,
        compressed_bytes: u64::try_from(retired_bytes.len()).expect("retired bytes"),
        uncompressed_bytes: 100,
        row_count: 1,
        sequence: SequenceRange {
            from_exclusive: prior.head_sequence,
            to_inclusive: prior.head_sequence + 1,
        },
        tables: vec![MirrorTable::Car],
    };
    let chain_digest = canonical_delta_chain_digest(parent_digest, retired_digest);
    prior.deltas.push(LineageDelta {
        from_sequence: prior.head_sequence,
        to_sequence: prior.head_sequence + 1,
        parent_chain_digest: parent_digest,
        chain_digest,
        pack_digest: retired_digest,
        pack: pack.clone(),
    });
    prior.head_sequence += 1;
    prior.head_digest = chain_digest;
    prior.terminal_cursor = OpaqueCursor::issue(
        &cursor_key,
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: prior.head_sequence,
        },
    )
    .expect("prior terminal cursor");
    prior.validate().expect("valid prior lineage");
    let retired_path = store
        .packs_dir()
        .join("sha256")
        .join(format!("{retired_digest}.sqlite.zst"));
    fs::write(&retired_path, retired_bytes).expect("retired pack file");

    let orphan_bytes = b"server-arbitrary-orphan";
    let orphan_digest = Sha256Digest::of_bytes(orphan_bytes);
    fs::write(
        store
            .packs_dir()
            .join("sha256")
            .join(format!("{orphan_digest}.sqlite.zst")),
        orphan_bytes,
    )
    .expect("orphan pack file");
    let retired_at_ms = current_epoch_ms().expect("retirement clock");
    let connection = store.open().expect("catalogue");
    connection
        .execute(
            "INSERT INTO sync_retired_lineages(
                vehicle_id, head_digest, manifest_json,
                retired_at_ms, expires_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                vehicle_id.to_string(),
                prior.head_digest.to_string(),
                serde_json::to_vec(&prior).expect("prior lineage JSON"),
                retired_at_ms,
                retired_at_ms + 60_000,
            ],
        )
        .expect("retired lineage");
    connection
        .execute(
            "INSERT INTO sync_retired_lineage_packs(
                vehicle_id, head_digest, pack_digest,
                relative_path, compressed_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                vehicle_id.to_string(),
                prior.head_digest.to_string(),
                retired_digest.to_string(),
                pack.relative_path,
                i64::try_from(pack.compressed_bytes).expect("retired pack size"),
            ],
        )
        .expect("retired lineage pack");
    drop(connection);
    drop(store);

    let app = router(HubStore::initialize(temp.path()).expect("restart store"));
    let retired = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{retired_digest}.sqlite.zst"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("retired pack response");
    assert_eq!(retired.status(), StatusCode::OK);
    assert_eq!(
        retired.headers().get(header::CACHE_CONTROL).unwrap(),
        "private, max-age=31536000, immutable"
    );
    assert_eq!(
        retired
            .into_body()
            .collect()
            .await
            .expect("retired pack body")
            .to_bytes()
            .as_ref(),
        retired_bytes
    );

    let orphan = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{orphan_digest}.sqlite.zst"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("orphan response");
    assert_eq!(orphan.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delta_v2_rejects_unknown_unavailable_and_corrupt_requests_without_v1_fallback() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let unknown = Uuid::new_v4();
    seed_active_vehicle_identity(&store, Uuid::new_v4(), unknown);
    let app = router(store);
    let unknown_capability = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{unknown}/sync/manifest"))
                .header(SYNC_CAPABILITY_HEADER, "future-delta")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unknown capability response");
    assert_eq!(unknown_capability.status(), StatusCode::BAD_REQUEST);

    let unavailable = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{unknown}/sync/manifest"))
                .header(SYNC_CAPABILITY_HEADER, DELTA_V2_CAPABILITY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unavailable response");
    assert_eq!(unavailable.status(), StatusCode::NOT_ACCEPTABLE);

    let temp_corrupt = crate::private_tempdir().expect("corrupt temp directory");
    let corrupt_store = HubStore::initialize(temp_corrupt.path()).expect("corrupt store");
    let cursor_key = CursorKey::from_bytes([43; 32]);
    let (vehicle_id, _, pack_path) = seed_v2_lineage(&corrupt_store, &cursor_key);
    fs::write(&pack_path, b"corrupt").expect("corrupt pack");
    let corrupt_app = router(corrupt_store);
    let corrupt = corrupt_app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .header(SYNC_CAPABILITY_HEADER, DELTA_V2_CAPABILITY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("corrupt response");
    assert_eq!(corrupt.status(), StatusCode::SERVICE_UNAVAILABLE);
}
