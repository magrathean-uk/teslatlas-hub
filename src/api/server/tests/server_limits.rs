// SPDX-License-Identifier: AGPL-3.0-only

// Server tests kept in their own file so `tests.rs` stays under the line limit. This
// file is `include!`d into the `tests` module, so test paths and the helpers it
// shares with its siblings are unchanged.

#[tokio::test]
async fn physical_pack_verification_buffers_immutable_bytes_from_same_open_descriptor() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let path = temporary.path().join("pack.sqlite.zst");
    let held_path = temporary.path().join("held.sqlite.zst");
    let original = b"opened physical pack bytes";
    fs::write(&path, original).expect("physical pack");
    let mut opened = open_admitted_physical_pack(&path).expect("open pack");
    fs::rename(&path, &held_path).expect("rename held pack");
    fs::write(&path, b"replacement physical pack").expect("replacement pack");

    let verified = read_verified_physical_pack(
        &mut opened,
        u64::try_from(original.len()).expect("bounded pack"),
        Sha256Digest::of_bytes(original),
    )
    .await
    .expect("verify held pack");
    fs::write(&held_path, vec![b'x'; original.len()]).expect("mutate held inode after verify");
    assert_eq!(verified.as_ref(), original);

    let symlink = temporary.path().join("pack-link.sqlite.zst");
    std::os::unix::fs::symlink(&held_path, &symlink).expect("physical pack symlink");
    assert!(open_admitted_physical_pack(&symlink).is_err());
}

#[test]
fn physical_public_admission_gate_enforces_profile_wire_bounds() {
    const MAX_I_JSON_INTEGER: u64 = 9_007_199_254_740_991;
    const MAX_PROFILE_PACK_BYTES: u64 = 16 * 1024 * 1024;

    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = HubStore::initialize(temporary.path().join("store")).expect("store");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let (_, candidate) = crate::sync::physical_fragments::tests::public_admission_candidate_fixture(
        &temporary.path().join("candidate"),
        &store,
        &CursorKey::from_bytes([97; 32]),
        1,
    );
    let admission = store
        .stage_pending_physical_v3_admission(&gate, candidate)
        .expect("physical admission");
    assert!(crate::db::physical_v3_admission_is_public(&admission));

    let mut boundary = admission.clone();
    boundary.head_sequence = MAX_I_JSON_INTEGER;
    boundary.manifest.chunks[0].compressed_bytes = MAX_PROFILE_PACK_BYTES;
    assert!(crate::db::physical_v3_admission_is_public(&boundary));

    let mut invalid = boundary.clone();
    invalid.head_sequence = MAX_I_JSON_INTEGER + 1;
    assert!(!crate::db::physical_v3_admission_is_public(&invalid));
    invalid = boundary.clone();
    invalid.manifest.chunks[0].compressed_bytes = MAX_PROFILE_PACK_BYTES + 1;
    assert!(!crate::db::physical_v3_admission_is_public(&invalid));
    invalid = boundary.clone();
    invalid.manifest.chunks[0].compressed_bytes = 0;
    assert!(!crate::db::physical_v3_admission_is_public(&invalid));

    let pack = boundary.manifest.chunks[0].clone();
    boundary.manifest.chunks.resize(1_771, pack.clone());
    assert!(crate::db::physical_v3_admission_is_public(&boundary));
    boundary.manifest.chunks.push(pack);
    assert!(!crate::db::physical_v3_admission_is_public(&boundary));
}

#[test]
fn tls_identity_reader_rejects_a_fifo_without_waiting_for_a_writer() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("identity.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("run mkfifo")
            .success()
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("FIFO mode");

    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        sender
            .send(read_tls_identity_file(&path, 1024, true).is_err())
            .expect("send FIFO result");
    });
    assert!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("FIFO identity admission must not block")
    );
    worker.join().expect("FIFO identity worker");
}

#[tokio::test]
async fn fleet_telemetry_ingress_is_token_gated_and_separately_bounded() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let token_path = temporary.path().join("telemetry-token");
    let token = "a".repeat(64);
    fs::write(&token_path, format!("{token}\n")).expect("token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("private token");
    let ingress = FleetTelemetryIngress::from_token_file(&token_path).expect("ingress");
    let app = router_with_access_and_telemetry(
        HubStore::initialize(temporary.path().join("data")).expect("store"),
        false,
        false,
        false,
        None,
        Some(CursorKey::from_bytes([7; 32])),
        None,
        Some(ingress),
    );
    let body = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": "tx-1",
        "tx_type": "vehicle_data",
        "received_at_ms": 1_800_000_000_100_i64,
        "timestamp_ms": 1_800_000_000_000_i64,
        "payload": {"data": {"Soc": {"intValue": "80"}}}
    }))
    .expect("telemetry body");

    let unauthorized_response = app
        .clone()
        .oneshot(
            Request::post("/v1/internal/fleet-telemetry")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .expect("unauthorized response");
    assert_eq!(unauthorized_response.status(), StatusCode::UNAUTHORIZED);

    let unknown_vehicle_response = app
        .clone()
        .oneshot(
            Request::post("/v1/internal/fleet-telemetry")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("unknown vehicle response");
    assert_eq!(
        unknown_vehicle_response.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let oversized_response = app
        .oneshot(
            Request::post("/v1/internal/fleet-telemetry")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(vec![b'x'; MAX_FLEET_TELEMETRY_INPUT_BYTES + 1]))
                .unwrap(),
        )
        .await
        .expect("oversized response");
    assert_eq!(oversized_response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn http_resource_limits_bound_handlers_and_wait_time() {
    #[derive(Clone)]
    struct Probe {
        active: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
    }

    async fn observed(State(probe): State<Probe>) -> StatusCode {
        let active = probe.active.fetch_add(1, Ordering::SeqCst) + 1;
        probe.maximum.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        probe.active.fetch_sub(1, Ordering::SeqCst);
        StatusCode::OK
    }

    let probe = Probe {
        active: Arc::new(AtomicUsize::new(0)),
        maximum: Arc::new(AtomicUsize::new(0)),
    };
    let app = apply_http_resource_limits(
        Router::new()
            .route("/bounded", get(observed))
            .with_state(probe.clone()),
        2,
        Duration::from_secs(1),
    );
    let requests = (0..8).map(|_| {
        app.clone().oneshot(
            Request::builder()
                .uri("/bounded")
                .body(Body::empty())
                .expect("request"),
        )
    });
    for response in futures_util::future::join_all(requests).await {
        assert_eq!(response.expect("bounded response").status(), StatusCode::OK);
    }
    assert!(probe.maximum.load(Ordering::SeqCst) <= 2);

    let timed = apply_http_resource_limits(
        Router::new().route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                StatusCode::OK
            }),
        ),
        1,
        Duration::from_millis(10),
    )
    .oneshot(
        Request::builder()
            .uri("/slow")
            .body(Body::empty())
            .expect("request"),
    )
    .await
    .expect("timeout response");
    assert_eq!(timed.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn pack_stream_stops_when_its_bearer_loses_access() {
    for transition in ["revoke", "expire", "rotate_twice"] {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let key = CursorKey::from_bytes([83; 32]);
        let (vehicle_id, old_digest, old_path) = seed_v2_lineage(&store, &key);
        let bytes: Vec<u8> = (0..512 * 1024).map(|n| (n % 251) as u8).collect();
        let digest = Sha256Digest::of_bytes(&bytes);
        let path = old_path.with_file_name(format!("{digest}.sqlite.zst"));
        fs::write(&path, &bytes).expect("synthetic pack");
        let connection = store.open().expect("catalogue");
        let raw: Vec<u8> = connection.query_row(
            "SELECT manifest_json FROM sync_manifests WHERE vehicle_id = ?1",
            [vehicle_id.to_string()], |row| row.get(0),
        ).expect("manifest");
        let mut manifest: SyncManifest = serde_json::from_slice(&raw).expect("decode manifest");
        manifest.chunks[0].sha256 = digest;
        manifest.chunks[0].relative_path = TransportPack::canonical_relative_path(digest);
        manifest.chunks[0].compressed_bytes = bytes.len() as u64;
        manifest.chunks[0].uncompressed_bytes = bytes.len() as u64;
        manifest.total_compressed_bytes = bytes.len() as u64;
        manifest.total_uncompressed_bytes = bytes.len() as u64;
        manifest.validate().expect("valid manifest");
        connection.execute(
            "UPDATE sync_manifests SET manifest_json = ?1 WHERE vehicle_id = ?2",
            rusqlite::params![serde_json::to_vec(&manifest).expect("encode manifest"), vehicle_id.to_string()],
        ).expect("update manifest");
        connection.execute(
            "UPDATE sync_packs SET sha256 = ?1, relative_path = ?2,
                    compressed_bytes = ?3, uncompressed_bytes = ?3 WHERE sha256 = ?4",
            rusqlite::params![digest.to_string(), manifest.chunks[0].relative_path,
                              bytes.len() as i64, old_digest.to_string()],
        ).expect("update pack catalogue");
        drop(connection);
        let now = current_epoch_ms().expect("clock");
        let invitation = store.create_pairing("stream fixture", now - 1, now + 60_000)
            .expect("invitation");
        let access = store.claim_pairing(invitation.pairing_id, invitation.secret(), "fixture phone", now)
            .expect("paired device");
        let app = paired_router(store.clone(), &key);
        let response = app.clone().oneshot(Request::builder()
            .uri(format!("/v1/packs/sha256/{digest}.sqlite.zst"))
            .header(header::AUTHORIZATION, format!("Bearer {}", access.access_token.as_bearer()))
            .body(Body::empty()).unwrap()).await.expect("pack response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let first = body.frame().await.expect("first frame").expect("authorised frame")
            .into_data().expect("data frame");
        assert!(!first.is_empty());
        assert!(first.len() < bytes.len());
        match transition {
            "revoke" => store.revoke_device(access.device_id).expect("owner revokes device"),
            "expire" => {
                store.open().expect("catalogue").execute(
                    "UPDATE paired_devices SET expires_at_ms = ?1 WHERE device_id = ?2",
                    rusqlite::params![now - 1, access.device_id.to_string()],
                ).expect("expire device");
            }
            "rotate_twice" => {
                let successor = store.rotate_device(access.access_token.as_bearer(), now).expect("rotate");
                store.rotate_device(successor.access_token.as_bearer(), now).expect("rotate again");
            }
            _ => unreachable!(),
        }
        assert!(store.authenticate_device(access.access_token.as_bearer()).expect("authentication").is_none());
        let next = body.frame().await;
        assert!(matches!(next, None | Some(Err(_))),
            "{transition}: the existing response still sends history after its bearer is refused");
    }
}
