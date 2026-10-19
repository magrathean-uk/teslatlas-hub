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
    let (_, candidate) =
        crate::import::teslamate::physical_fragments::tests::public_admission_candidate_fixture(
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
