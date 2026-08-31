// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::BTreeMap,
    fs,
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    os::unix::fs::{PermissionsExt, symlink},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use axum::{body::Body, http::Request};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use http_body_util::BodyExt;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use tower::ServiceExt;
use uuid::Uuid;

use super::*;
use crate::{
    config::{HubConfig, TlsListenerConfig},
    db::{
        HubStore, ObservationInput, SUPERVISED_COLLECTOR_LEASE_MS, SourceDescriptor,
        SupervisedCollectorState, VehicleDescriptor,
    },
    hub_pack::{ProjectionCarSettings, ProjectionDrive},
    protocol::{
        CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V2, HUB_PROJECTION_SCHEMA_V3, LineageDelta,
        LineageManifestV2, MirrorTable, OpaqueCursor, PackCompression, PackFormat, ProtocolVersion,
        SequenceRange, Sha256Digest, SyncManifest, TRANSPORT_SCHEMA_V1, TransferMode,
        TransportPack, canonical_delta_chain_digest,
    },
    teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history},
    teslamate_projection::{TeslaMateCar, TeslaMateDrive, TeslaMateHistory},
    transport::{
        TransportOperation, TransportPackRequest, TransportPackWriter, TransportRow, TransportValue,
    },
    updates_delivery::{
        publish_updates_schema_22, sign_updates_schema_22_manifest, sign_updates_schema_22_noop,
        updates_pack_request, write_updates_schema_22_pack,
    },
};

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

fn private_test_directory(path: &std::path::Path) {
    fs::create_dir(path).expect("create private test directory");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("protect private test directory");
}

fn admitted_server_fixture(
    temporary: &tempfile::TempDir,
) -> (
    Arc<crate::hub_user_process::AdmittedUserHub>,
    std::path::PathBuf,
) {
    let store = temporary.path().join("data");
    private_test_directory(&store);
    let admitted =
        crate::hub_user_process::AdmittedUserHub::for_test(&store).expect("admit test Hub root");
    (admitted, store)
}

fn local_tls_server_config(
    temporary: &tempfile::TempDir,
    data_dir: std::path::PathBuf,
    bind: SocketAddr,
) -> HubConfig {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .expect("TLS identity");
    let certificate_path = temporary.path().join("certificate.pem");
    let private_key_path = temporary.path().join("private-key.pem");
    fs::write(&certificate_path, cert.pem()).expect("write certificate");
    fs::write(&private_key_path, signing_key.serialize_pem()).expect("write private key");
    fs::set_permissions(&private_key_path, fs::Permissions::from_mode(0o600))
        .expect("protect private key");
    HubConfig {
        data_dir,
        bind,
        tls: Some(TlsListenerConfig {
            certificate_path,
            private_key_path,
            public_url: format!("https://{bind}"),
        }),
        collector: Default::default(),
        geocoder: Default::default(),
        teslamate: Default::default(),
        terrain: Default::default(),
    }
}

fn local_plain_server_config(data_dir: std::path::PathBuf, bind: SocketAddr) -> HubConfig {
    HubConfig {
        data_dir,
        bind,
        tls: None,
        collector: Default::default(),
        geocoder: Default::default(),
        teslamate: Default::default(),
        terrain: Default::default(),
    }
}

async fn wait_for_tcp_listener(bind: SocketAddr) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if TcpStream::connect_timeout(&bind, Duration::from_millis(25)).is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("TLS listener starts before outer Serve cancellation");
}

async fn wait_for_tcp_rebind(bind: SocketAddr) -> TcpListener {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match TcpListener::bind(bind) {
                Ok(listener) => return listener,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("TLS listener is released rather than detached")
}

#[tokio::test]
async fn native_tls_validation_rejects_malformed_and_mismatched_identity() {
    let temporary = crate::private_tempdir().expect("temporary TLS identity");
    let certificate_path = temporary.path().join("certificate.pem");
    let private_key_path = temporary.path().join("private-key.pem");
    let tls = TlsListenerConfig {
        certificate_path: certificate_path.clone(),
        private_key_path: private_key_path.clone(),
        public_url: "https://hub.example.test:8443".to_owned(),
    };

    fs::write(&certificate_path, b"not a certificate\n").expect("write malformed cert");
    fs::write(&private_key_path, b"not a private key\n").expect("write malformed key");
    fs::set_permissions(&private_key_path, fs::Permissions::from_mode(0o600))
        .expect("protect malformed key");
    rustls_config_from_identity(&tls)
        .await
        .expect_err("malformed PEM must fail native validation");

    let CertifiedKey {
        cert: first_certificate,
        signing_key: _,
    } = generate_simple_self_signed(vec!["hub.example.test".to_owned()]).expect("first identity");
    let CertifiedKey {
        cert: _,
        signing_key: second_key,
    } = generate_simple_self_signed(vec!["hub.example.test".to_owned()]).expect("second identity");
    fs::write(&certificate_path, first_certificate.pem()).expect("write certificate");
    fs::write(&private_key_path, second_key.serialize_pem()).expect("write mismatched key");
    fs::set_permissions(&private_key_path, fs::Permissions::from_mode(0o600))
        .expect("protect mismatched key");
    rustls_config_from_identity(&tls)
        .await
        .expect_err("mismatched certificate and key must fail native validation");
}

#[tokio::test]
async fn native_tls_validation_rejects_symlink_insecure_mode_and_oversize() {
    let temporary = crate::private_tempdir().expect("temporary TLS identity");
    let certificate_path = temporary.path().join("certificate.pem");
    let private_key_path = temporary.path().join("private-key.pem");
    let tls = TlsListenerConfig {
        certificate_path: certificate_path.clone(),
        private_key_path: private_key_path.clone(),
        public_url: "https://hub.example.test:8443".to_owned(),
    };

    let certificate_target = temporary.path().join("certificate-target.pem");
    fs::write(&certificate_target, b"certificate").expect("certificate target");
    symlink(&certificate_target, &certificate_path).expect("certificate symlink");
    fs::write(&private_key_path, b"key").expect("private key");
    fs::set_permissions(&private_key_path, fs::Permissions::from_mode(0o600)).expect("protect key");
    rustls_config_from_identity(&tls)
        .await
        .expect_err("Serve must reject a certificate symlink");

    fs::remove_file(&certificate_path).expect("remove symlink");
    fs::write(
        &certificate_path,
        vec![b'x'; MAX_TLS_CERTIFICATE_CHAIN_BYTES + 1],
    )
    .expect("oversize certificate");
    rustls_config_from_identity(&tls)
        .await
        .expect_err("Serve must reject an oversize certificate");

    fs::write(&certificate_path, b"certificate").expect("bounded certificate");
    fs::set_permissions(&private_key_path, fs::Permissions::from_mode(0o640))
        .expect("weaken key mode");
    rustls_config_from_identity(&tls)
        .await
        .expect_err("Serve must reject a group-readable private key");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn admitted_tls_server_creates_data_dir_cursor_key_when_not_supplied() {
    let temporary = crate::private_tempdir().expect("temporary admitted TLS server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let config = local_tls_server_config(&temporary, store_path, bind);
    let cursor_key_path = crate::teslamate_credentials::cursor_key_path(&config.data_dir);
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let server_task = tokio::spawn(async move {
        serve_for_admitted_user(
            store,
            &config,
            Sha256Digest::of_bytes(b"admitted TLS cursor fallback test"),
            admission,
            None,
            std::future::pending(),
        )
        .await
    });

    wait_for_tcp_listener(bind).await;
    assert_eq!(
        fs::read(&cursor_key_path)
            .expect("data-directory cursor key")
            .len(),
        32
    );
    assert_eq!(
        fs::metadata(&cursor_key_path)
            .expect("cursor key metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    server_task.abort();
    let cancellation = server_task
        .await
        .expect_err("outer Serve task is cancelled");
    assert!(cancellation.is_cancelled());
    let rebound = wait_for_tcp_rebind(bind).await;
    drop(rebound);
}

#[tokio::test]
async fn admitted_plain_server_reuses_the_persisted_schema_22_cursor_key() {
    let temporary = crate::private_tempdir().expect("temporary admitted plain server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let config = local_plain_server_config(store_path, bind);
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .expect("persisted cursor key");
    let (vehicle_id, _) = inject_schema_22_catalogue(&store, &cursor_key);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        serve_for_admitted_user(
            store,
            &config,
            Sha256Digest::of_bytes(b"plain persisted cursor test"),
            admission,
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });

    wait_for_tcp_listener(bind).await;
    let response = reqwest::Client::new()
        .get(format!(
            "http://{bind}/v1/vehicles/{vehicle_id}/sync/manifest"
        ))
        .header(SUPPORTED_SCHEMAS_HEADER, "2.2")
        .send()
        .await
        .expect("manifest request");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key(MANIFEST_SIGNATURE_HEADER));

    shutdown_tx.send(()).expect("signal server shutdown");
    server_task
        .await
        .expect("server task")
        .expect("plain server shutdown");
}

#[tokio::test]
async fn tls_server_cancellation_releases_owned_listener_for_rebind() {
    let temporary = crate::private_tempdir().expect("temporary TLS server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let config = local_tls_server_config(&temporary, store_path, bind);
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let server_task = tokio::spawn(async move {
        serve_with_cursor_key(
            store,
            &config,
            Sha256Digest::of_bytes(b"TLS cancellation listener test"),
            Some(CursorKey::from_bytes([93; 32])),
            Some(admission),
            std::future::pending(),
        )
        .await
    });

    wait_for_tcp_listener(bind).await;
    assert!(
        !server_task.is_finished(),
        "outer Serve task remains active until its supervisor cancels it"
    );

    server_task.abort();
    let cancellation = server_task
        .await
        .expect_err("outer Serve task is cancelled");
    assert!(cancellation.is_cancelled());

    let rebound = wait_for_tcp_rebind(bind).await;
    drop(rebound);
}

#[tokio::test]
async fn tls_server_supervisor_shutdown_gracefully_awaits_listener_stop() {
    let temporary = crate::private_tempdir().expect("temporary TLS server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let config = local_tls_server_config(&temporary, store_path, bind);
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        serve_with_cursor_key(
            store,
            &config,
            Sha256Digest::of_bytes(b"TLS graceful shutdown listener test"),
            Some(CursorKey::from_bytes([94; 32])),
            Some(admission),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });

    wait_for_tcp_listener(bind).await;
    shutdown_tx.send(()).expect("signal server shutdown");
    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("graceful shutdown remains bounded")
        .expect("outer Serve task does not panic")
        .expect("TLS listener stops cleanly after graceful shutdown");
    let rebound = wait_for_tcp_rebind(bind).await;
    drop(rebound);
}

#[tokio::test]
async fn plain_server_cancellation_releases_owned_listener_with_active_connection() {
    let temporary = crate::private_tempdir().expect("temporary plain server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let config = local_plain_server_config(store_path, bind);
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let server_task = tokio::spawn(async move {
        serve_with_cursor_key(
            store,
            &config,
            Sha256Digest::of_bytes(b"plain cancellation listener test"),
            None,
            Some(admission),
            std::future::pending(),
        )
        .await
    });

    wait_for_tcp_listener(bind).await;
    let active_connection =
        TcpStream::connect_timeout(&bind, Duration::from_millis(250)).expect("open request");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !server_task.is_finished(),
        "outer Serve task remains active with a live plaintext connection"
    );

    server_task.abort();
    let cancellation = server_task
        .await
        .expect_err("outer Serve task is cancelled");
    assert!(cancellation.is_cancelled());
    let rebound = wait_for_tcp_rebind(bind).await;
    drop(rebound);
    drop(active_connection);
}

#[tokio::test]
async fn plain_server_supervisor_shutdown_gracefully_awaits_listener_stop() {
    let temporary = crate::private_tempdir().expect("temporary plain server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let config = local_plain_server_config(store_path, bind);
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        serve_with_cursor_key(
            store,
            &config,
            Sha256Digest::of_bytes(b"plain graceful shutdown listener test"),
            None,
            Some(admission),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });

    wait_for_tcp_listener(bind).await;
    shutdown_tx.send(()).expect("signal server shutdown");
    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("graceful shutdown remains bounded")
        .expect("outer Serve task does not panic")
        .expect("plaintext listener stops cleanly after graceful shutdown");
    let rebound = wait_for_tcp_rebind(bind).await;
    drop(rebound);
}

#[tokio::test]
async fn native_readiness_binds_the_loaded_config_contract_digest() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let digest = Sha256Digest::of_bytes(b"loaded native config contract");
    let response = router_with_access(store, false, false, false, None, None, Some(digest))
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("readiness response");
    assert_eq!(
        response
            .headers()
            .get(NATIVE_CONFIG_DIGEST_HEADER)
            .expect("native config digest")
            .to_str()
            .expect("digest header text"),
        digest.to_string()
    );
}

fn seed_v2_lineage(
    store: &HubStore,
    cursor_key: &CursorKey,
) -> (Uuid, Sha256Digest, std::path::PathBuf) {
    let source = store
        .register_source(&SourceDescriptor::new("server_test", "v2"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-v2"),
            1_001,
        )
        .expect("vehicle");
    store
        .upsert_car_settings(vehicle.vehicle_id, 7, &ProjectionCarSettings::default())
        .expect("settings");
    let installation_id = store.installation_id().expect("installation");
    let snapshot_id = Uuid::new_v4();
    let digest = Sha256Digest::of_bytes(b"server-v2-base-pack");
    let pack = TransportPack {
        pack_id: Uuid::new_v4(),
        snapshot_id,
        ordinal: 0,
        schema: HUB_PROJECTION_SCHEMA_V2,
        format: PackFormat::HubProjectionSqlite,
        compression: PackCompression::Zstd,
        relative_path: TransportPack::canonical_relative_path(digest),
        sha256: digest,
        compressed_bytes: 19,
        uncompressed_bytes: 100,
        row_count: 1,
        sequence: SequenceRange {
            from_exclusive: 7,
            to_inclusive: 7,
        },
        tables: vec![MirrorTable::Car],
    };
    let pack_path = store
        .packs_dir()
        .join("sha256")
        .join(format!("{digest}.sqlite.zst"));
    fs::create_dir_all(pack_path.parent().expect("pack directory")).expect("pack directory");
    fs::write(&pack_path, b"server-v2-base-pack").expect("pack");
    let cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id,
            account_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: 1,
            sequence: 7,
        },
    )
    .expect("cursor");
    let base_manifest = SyncManifest {
        protocol: ProtocolVersion { major: 1, minor: 0 },
        schema: HUB_PROJECTION_SCHEMA_V2,
        installation_id,
        account_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: 1,
        snapshot_id,
        mode: TransferMode::FullSnapshot,
        base_sequence: 7,
        head_sequence: 7,
        chunk_count: 1,
        total_compressed_bytes: pack.compressed_bytes,
        total_uncompressed_bytes: pack.uncompressed_bytes,
        total_rows: pack.row_count,
        chunks: vec![pack.clone()],
        terminal_cursor: cursor.clone(),
    };
    base_manifest.validate().expect("base manifest");
    let connection = store.open().expect("database");
    connection
        .execute(
            "INSERT INTO sync_bases(
                vehicle_id, snapshot_id, base_sequence, base_digest, packs_json
             ) VALUES (?1, ?2, 7, ?3, ?4)",
            rusqlite::params![
                vehicle.vehicle_id.to_string(),
                snapshot_id.to_string(),
                digest.to_string(),
                serde_json::to_vec(&vec![pack.clone()]).expect("base packs")
            ],
        )
        .expect("base catalog");
    // A schema-2.1 lineage is valid only when the immutable source/car
    // binding that created its base is present. The server test builds a
    // minimal catalogue directly, so it must seed the same durable fact
    // that production base finalization records atomically.
    connection
        .execute(
            "INSERT INTO v2_base_bindings(
                vehicle_id, snapshot_id, installation_id, account_id,
                generation, selected_car_id
             ) VALUES (?1, ?2, ?3, ?4, 1, 7)",
            rusqlite::params![
                vehicle.vehicle_id.to_string(),
                snapshot_id.to_string(),
                installation_id.to_string(),
                source.source_id.to_string(),
            ],
        )
        .expect("immutable base binding");
    connection
        .execute(
            "INSERT INTO sync_manifests(
                snapshot_id, vehicle_id, head_sequence, manifest_json
             ) VALUES (?1, ?2, 7, ?3)",
            rusqlite::params![
                snapshot_id.to_string(),
                vehicle.vehicle_id.to_string(),
                serde_json::to_vec(&base_manifest).expect("manifest JSON")
            ],
        )
        .expect("manifest catalog");
    connection
        .execute(
            "INSERT INTO sync_packs(
                sha256, snapshot_id, ordinal, relative_path,
                compressed_bytes, uncompressed_bytes
             ) VALUES (?1, ?2, 0, ?3, ?4, ?5)",
            rusqlite::params![
                digest.to_string(),
                snapshot_id.to_string(),
                pack.relative_path,
                pack.compressed_bytes as i64,
                pack.uncompressed_bytes as i64,
            ],
        )
        .expect("base pack catalog");
    connection
        .execute(
            "INSERT INTO sync_heads(
                vehicle_id, base_snapshot_id, head_sequence, head_digest,
                terminal_cursor
             ) VALUES (?1, ?2, 7, ?3, ?4)",
            rusqlite::params![
                vehicle.vehicle_id.to_string(),
                snapshot_id.to_string(),
                digest.to_string(),
                serde_json::to_string(&cursor).expect("cursor JSON")
            ],
        )
        .expect("head catalog");
    (vehicle.vehicle_id, digest, pack_path)
}

fn inject_schema_22_catalogue(store: &HubStore, cursor_key: &CursorKey) -> (Uuid, Sha256Digest) {
    let installation_id = store.installation_id().expect("installation");
    let account_id = Uuid::new_v4();
    let vehicle_id = Uuid::new_v4();
    let snapshot_id = Uuid::new_v4();
    let pack_bytes = b"schema-22-not-published";
    let digest = Sha256Digest::of_bytes(pack_bytes);
    let pack = TransportPack {
        pack_id: Uuid::new_v4(),
        snapshot_id,
        ordinal: 0,
        schema: HUB_PROJECTION_SCHEMA_V3,
        format: PackFormat::HubProjectionSqlite,
        compression: PackCompression::Zstd,
        relative_path: TransportPack::canonical_relative_path(digest),
        sha256: digest,
        compressed_bytes: u64::try_from(pack_bytes.len()).expect("pack size"),
        uncompressed_bytes: 100,
        row_count: 1,
        sequence: SequenceRange {
            from_exclusive: 7,
            to_inclusive: 7,
        },
        tables: vec![MirrorTable::Car],
    };
    let cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V3,
            installation_id,
            account_id,
            vehicle_id,
            generation: 1,
            sequence: 7,
        },
    )
    .expect("schema 2.2 cursor");
    let manifest = SyncManifest {
        protocol: ProtocolVersion { major: 1, minor: 0 },
        schema: HUB_PROJECTION_SCHEMA_V3,
        installation_id,
        account_id,
        vehicle_id,
        generation: 1,
        snapshot_id,
        mode: TransferMode::FullSnapshot,
        base_sequence: 7,
        head_sequence: 7,
        chunk_count: 1,
        total_compressed_bytes: pack.compressed_bytes,
        total_uncompressed_bytes: pack.uncompressed_bytes,
        total_rows: pack.row_count,
        chunks: vec![pack.clone()],
        terminal_cursor: cursor,
    };
    manifest
        .validate()
        .expect("schema 2.2 remains protocol-valid");
    let path = store
        .packs_dir()
        .join("sha256")
        .join(format!("{digest}.sqlite.zst"));
    fs::create_dir_all(path.parent().expect("pack directory")).expect("pack directory");
    fs::write(path, pack_bytes).expect("pack");
    let connection = store.open().expect("catalogue");
    connection
        .execute(
            "INSERT INTO sync_manifests(
                snapshot_id, vehicle_id, head_sequence, manifest_json
             ) VALUES (?1, ?2, 7, ?3)",
            rusqlite::params![
                snapshot_id.to_string(),
                vehicle_id.to_string(),
                serde_json::to_vec(&manifest).expect("manifest JSON"),
            ],
        )
        .expect("schema 2.2 manifest fixture");
    connection
        .execute(
            "INSERT INTO sync_packs(
                sha256, snapshot_id, ordinal, relative_path,
                compressed_bytes, uncompressed_bytes
             ) VALUES (?1, ?2, 0, ?3, ?4, ?5)",
            rusqlite::params![
                digest.to_string(),
                snapshot_id.to_string(),
                pack.relative_path,
                i64::try_from(pack.compressed_bytes).expect("pack size"),
                i64::try_from(pack.uncompressed_bytes).expect("pack size"),
            ],
        )
        .expect("schema 2.2 pack fixture");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("schema 2.2 publication gate");
    store
        .publish_schema_22_noop(
            &publication_gate,
            &crate::updates_delivery::SignedNoOpState {
                schema: "teslatlas-hub-schema-22-noop-v1".into(),
                projection_schema: "2.2".into(),
                installation_id,
                account_id,
                vehicle_id,
                generation: 1,
                snapshot_id,
                head_sequence: 7,
                pack_sha256: digest.to_string(),
                terminal_cursor: manifest.terminal_cursor,
                source_witness: None,
            },
        )
        .expect("schema 2.2 no-op fixture");
    (vehicle_id, digest)
}

#[test]
fn default_schema_negotiation_never_advertises_schema_22() {
    let headers = HeaderMap::new();
    assert_eq!(
        negotiate_hub_projection_schema(&headers, HUB_PROJECTION_SCHEMA_V3),
        Err(SchemaNegotiationError::NoCompatibleSchema)
    );
    assert_eq!(
        negotiate_hub_projection_schema(&headers, HUB_PROJECTION_SCHEMA_V2),
        Ok(HUB_PROJECTION_SCHEMA_V2)
    );
}

#[tokio::test]
async fn exposes_health_and_capabilities() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let installation_id = store.installation_id().expect("installation identity");
    let unsigned = router(store.clone());
    let app = public_query_router(store, 46);

    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("health response");
    assert_eq!(health.status(), StatusCode::OK);

    let capabilities = app
        .oneshot(
            Request::builder()
                .uri("/.well-known/teslatlas-hub")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("capabilities response");
    let bytes = capabilities
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&bytes).expect("capabilities JSON");
    assert_eq!(payload["pack_format"], "sqlite-zstd");
    assert_eq!(payload["sourceUrl"], crate::corresponding_source_url());
    assert_eq!(payload["hub_id"], installation_id.to_string());
    assert_eq!(payload["api_versions"], serde_json::json!(["1.0"]));
    assert_eq!(
        payload["capabilities"],
        serde_json::json!([
            "query.vehicles",
            "query.current",
            "query.drives",
            "sync.packs"
        ])
    );
    assert!(payload["manifestPublicKey"].as_str().is_some());

    let unsigned_capabilities = unsigned
        .oneshot(
            Request::builder()
                .uri("/.well-known/teslatlas-hub")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unsigned capabilities response");
    let unsigned_payload = response_json(unsigned_capabilities).await;
    assert_eq!(
        unsigned_payload["capabilities"],
        serde_json::json!(["query.vehicles", "query.current"])
    );
}

#[tokio::test]
async fn schema_22_catalogue_serves_only_when_client_advertises_22() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([92; 32]);
    let (vehicle_id, digest) = inject_schema_22_catalogue(&store, &cursor_key);

    let loaded = store
        .manifest_for_vehicle(vehicle_id)
        .expect("catalogue lookup")
        .expect("schema 2.2 manifest");
    assert_eq!(loaded.schema, HUB_PROJECTION_SCHEMA_V3);
    assert!(
        store
            .pack_for_digest(digest)
            .expect("pack lookup")
            .is_some()
    );

    let app = router_with_access(
        store,
        false,
        false,
        false,
        Some(ManifestSigning::from_cursor_key(&cursor_key)),
        Some(cursor_key.clone()),
        None,
    );
    let refused = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("manifest response");
    assert_eq!(refused.status(), StatusCode::NOT_ACCEPTABLE);

    let served = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("schema 2.2 manifest");
    assert_eq!(served.status(), StatusCode::OK);
    let served_bytes = served.into_body().collect().await.expect("body").to_bytes();
    let served_manifest: SyncManifest =
        serde_json::from_slice(&served_bytes).expect("signed schema 2.2 manifest");
    assert_eq!(served_manifest.schema, HUB_PROJECTION_SCHEMA_V3);
    assert_eq!(served_manifest.vehicle_id, vehicle_id);

    let pack = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/packs/sha256/{digest}.sqlite.zst"))
                .header(SUPPORTED_SCHEMAS_HEADER, "2.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("pack response");
    assert_eq!(pack.status(), StatusCode::OK);
}

#[tokio::test]
async fn readiness_reports_redacted_collector_absent_stale_terminal_and_recovery_codes() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("server_test", "offline-readiness"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "offline-vehicle"),
            1_001,
        )
        .expect("vehicle");
    store
        .append_observation(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: 1_002,
                payload: serde_json::json!({"source_vehicle_state": "offline"}),
            },
            1_002,
        )
        .expect("offline observation");
    let disabled_vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "disabled-vehicle"),
            1_003,
        )
        .expect("disabled vehicle");
    store
        .append_observation(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: disabled_vehicle.vehicle_id,
                observed_at_ms: 1_004,
                payload: serde_json::json!({
                    "source_vehicle_state": "online",
                    "settings": {"enabled": false}
                }),
            },
            1_004,
        )
        .expect("disabled observation");
    let app = router_with_access(store.clone(), true, false, false, None, None, None);

    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("health response");
    assert_eq!(health.status(), StatusCode::OK);

    let absent = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("absent readiness");
    assert_eq!(absent.status(), StatusCode::SERVICE_UNAVAILABLE);
    let absent: serde_json::Value = serde_json::from_slice(
        &absent
            .into_body()
            .collect()
            .await
            .expect("absent body")
            .to_bytes(),
    )
    .expect("absent JSON");
    assert_eq!(
        absent,
        serde_json::json!({
            "status": "not_ready",
            "reason": "collector_absent"
        })
    );

    let now_ms = current_epoch_ms().expect("clock");
    let stale = store
        .acquire_supervised_collector_lease(now_ms - SUPERVISED_COLLECTOR_LEASE_MS - 1)
        .expect("stale crash lease");
    tokio::time::sleep(READINESS_CACHE_TTL + Duration::from_millis(10)).await;
    let stale_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("stale readiness");
    assert_eq!(stale_response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let stale_payload: serde_json::Value = serde_json::from_slice(
        &stale_response
            .into_body()
            .collect()
            .await
            .expect("stale body")
            .to_bytes(),
    )
    .expect("stale JSON");
    assert_eq!(stale_payload["reason"], "collector_stale");

    let replacement = store
        .acquire_supervised_collector_lease(now_ms)
        .expect("stale lease replacement");
    store
        .release_supervised_collector_lease(stale)
        .expect("stale owner cannot clear replacement");
    tokio::time::sleep(READINESS_CACHE_TTL + Duration::from_millis(10)).await;
    let recovered = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("recovered readiness");
    assert_eq!(recovered.status(), StatusCode::OK);

    store
        .heartbeat_supervised_collector_lease(
            replacement,
            SupervisedCollectorState::AuthenticationTerminal,
            now_ms + 1,
        )
        .expect("terminal auth heartbeat");
    tokio::time::sleep(READINESS_CACHE_TTL + Duration::from_millis(10)).await;
    let terminal = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("terminal readiness");
    assert_eq!(terminal.status(), StatusCode::SERVICE_UNAVAILABLE);
    let terminal_payload: serde_json::Value = serde_json::from_slice(
        &terminal
            .into_body()
            .collect()
            .await
            .expect("terminal body")
            .to_bytes(),
    )
    .expect("terminal JSON");
    assert_eq!(
        terminal_payload,
        serde_json::json!({
            "status": "not_ready",
            "reason": "collector_auth_terminal"
        })
    );

    // Normal offline/disabled vehicles and the absence of stream sessions
    // do not fail readiness once the required collector is live and
    // authenticated.
    store
        .heartbeat_supervised_collector_lease(
            replacement,
            SupervisedCollectorState::Active,
            now_ms + 2,
        )
        .expect("authenticated recovery");
    tokio::time::sleep(READINESS_CACHE_TTL + Duration::from_millis(10)).await;
    let recovered = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("offline vehicle readiness");
    assert_eq!(recovered.status(), StatusCode::OK);
    let stream_sessions: i64 = store
        .open()
        .expect("catalogue")
        .query_row("SELECT COUNT(*) FROM stream_session_receipts", [], |row| {
            row.get(0)
        })
        .expect("stream session count");
    assert_eq!(stream_sessions, 0);
}

#[tokio::test]
async fn readiness_refuses_a_cheaply_unservable_published_lineage_pack() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([91; 32]);
    let (_, _, pack_path) = seed_v2_lineage(&store, &cursor_key);
    let app = router(store);

    let ready = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("ready response");
    assert_eq!(ready.status(), StatusCode::OK);

    fs::write(&pack_path, b"truncated").expect("truncate published pack");
    tokio::time::sleep(READINESS_CACHE_TTL + Duration::from_millis(10)).await;
    let unavailable = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unready response");
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    let payload: serde_json::Value = serde_json::from_slice(
        &unavailable
            .into_body()
            .collect()
            .await
            .expect("unready body")
            .to_bytes(),
    )
    .expect("unready JSON");
    assert_eq!(
        payload,
        serde_json::json!({
            "status": "not_ready",
            "reason": "published_content_unservable"
        })
    );

    let health = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("health response");
    assert_eq!(health.status(), StatusCode::OK);
}

#[tokio::test]
async fn tls_capabilities_publish_lowercase_manifest_verifying_key() {
    let temp = crate::private_tempdir().expect("temp directory");
    let cursor_key = CursorKey::from_bytes([29; 32]);
    let expected = ManifestSigning::from_cursor_key(&cursor_key).verifying_key_hex();
    let app = paired_router(
        HubStore::initialize(temp.path()).expect("store"),
        &cursor_key,
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri("/.well-known/teslatlas-hub")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("capabilities response");
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("capabilities body")
        .to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&bytes).expect("capabilities JSON");
    let published = payload["manifestPublicKey"]
        .as_str()
        .expect("manifest verifying key");
    assert_eq!(published, expected);
    assert_eq!(published.len(), 64);
    assert!(
        published
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[tokio::test]
async fn tls_router_requires_a_paired_device_and_claims_once() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let invitation = store
        .create_pairing("iPhone", 1_000, i64::MAX)
        .expect("pairing invitation");
    let cursor_key = CursorKey::from_bytes([31; 32]);
    let app = paired_router(store, &cursor_key);

    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/vehicles")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("response");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let claim_body = serde_json::json!({
        "secret": invitation.secret(),
        "device_name": "Bolyki iPhone",
    })
    .to_string();
    let claimed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/pairings/{}/claim", invitation.pairing_id))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(claim_body))
                .unwrap(),
        )
        .await
        .expect("claim response");
    assert_eq!(claimed.status(), StatusCode::OK);
    let payload = claimed
        .into_body()
        .collect()
        .await
        .expect("claim payload")
        .to_bytes();
    let claimed = serde_json::from_slice::<serde_json::Value>(&payload).expect("claim JSON");
    let access_token = claimed["access_token"]
        .as_str()
        .expect("access token")
        .to_owned();
    assert!(claimed["expires_at_ms"].as_i64().unwrap() > 0);

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/vehicles")
                .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("vehicle response");
    assert_eq!(listed.status(), StatusCode::OK);

    let rotated = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/device/rotate")
                .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("rotation response");
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated_payload = rotated
        .into_body()
        .collect()
        .await
        .expect("rotation payload")
        .to_bytes();
    let rotated_token = serde_json::from_slice::<serde_json::Value>(&rotated_payload)
        .expect("rotation JSON")["access_token"]
        .as_str()
        .expect("rotated access token")
        .to_owned();
    let old_after_rotation = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/vehicles")
                .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("old token response");
    assert_eq!(old_after_rotation.status(), StatusCode::UNAUTHORIZED);
    let new_after_rotation = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/vehicles")
                .header(header::AUTHORIZATION, format!("Bearer {rotated_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("new token response");
    assert_eq!(new_after_rotation.status(), StatusCode::OK);

    let replay = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/pairings/{}/claim", invitation.pairing_id))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "secret": invitation.secret(),
                        "device_name": "Second phone",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("replay response");
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn current_vehicle_serves_the_durable_v4_1_1_summary_without_raw_history() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api", "current-summary-test"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "9").with_tesla_identity(Some(9), None),
            1_000,
        )
        .expect("vehicle");
    store
        .accept_owner_observation_and_lifecycle(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: 2_000,
                payload: serde_json::json!({
                    "record_type": "owner_api_vehicle_data_v1",
                    "source_vehicle_id": "9",
                    "source_vehicle_state": "online",
                    "display_name": "Rusty",
                    "vin": "5YJ3E1EA7NF000001",
                    "vehicle_data": {
                        "drive_state": {"latitude": 47.5, "longitude": 19.0, "speed": 10},
                        "charge_state": {"battery_level": 80, "charging_state": "Disconnected"},
                        "vehicle_state": {
                            "timestamp": 2_000,
                            "service_mode": true,
                            "software_update": {"status": "downloading", "download_perc": 42}
                        },
                        "vehicle_config": {"car_type": "model3", "trim_badging": "50"}
                    }
                }),
            },
            2_001,
            1,
        )
        .expect("current observation");
    let app = router(store.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{}/current", vehicle.vehicle_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("current response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let current: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(current["display_name"], "Rusty");
    assert_eq!(current["battery_level"], 80);
    assert_eq!(current["download_perc"], 42);
    assert_eq!(current["service_mode"], true);
    assert_eq!(current["car"]["marketing_name"], "RWD");
    assert_eq!(
        store
            .open()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

fn public_drive_fixture(id: i64, start_date_ms: i64) -> ProjectionDrive {
    ProjectionDrive {
        id,
        car_id: 1,
        optimized_at_ms: None,
        start_date_ms,
        end_date_ms: start_date_ms + 500,
        distance_km: Some(id as f64),
        duration_min: Some(1),
        efficiency: None,
        outside_temp_avg: None,
        inside_temp_avg: None,
        speed_max: None,
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_address: None,
        end_address: None,
        start_geofence: None,
        end_geofence: None,
        start_latitude: None,
        start_longitude: None,
        end_latitude: None,
        end_longitude: None,
        start_soc: None,
        end_soc: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
        ascent: None,
        descent: None,
    }
}

fn public_query_router(store: HubStore, key_byte: u8) -> Router {
    trusted_local_router(
        store,
        false,
        Some(CursorKey::from_bytes([key_byte; 32])),
        None,
    )
}

fn seed_public_drive(store: &HubStore, vehicle_id: Uuid, drive: &ProjectionDrive) {
    store
        .open()
        .expect("open store")
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                vehicle_id.to_string(),
                drive.id,
                drive.car_id,
                serde_json::to_string(drive).expect("drive JSON")
            ],
        )
        .expect("seed materialised drive");
}

fn mark_vehicle_published(store: &HubStore, vehicle_id: Uuid) {
    store
        .open()
        .expect("open store")
        .execute(
            "INSERT INTO sync_manifests(snapshot_id, vehicle_id, head_sequence, manifest_json)
             VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                vehicle_id.to_string(),
                b"{}".as_slice()
            ],
        )
        .expect("seed published vehicle");
}

async fn response_json(response: Response) -> serde_json::Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    serde_json::from_slice(&body).expect("response JSON")
}

#[tokio::test]
async fn public_drive_query_hides_unpublished_vehicle_history() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api", "unpublished-public-drive-query"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "9").with_tesla_identity(Some(9), None),
            1_000,
        )
        .expect("vehicle");
    seed_public_drive(&store, vehicle.vehicle_id, &public_drive_fixture(1, 1_000));

    let response = public_query_router(store, 47)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/drives?from_ms=1000&to_ms=2000",
                    vehicle.vehicle_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unpublished drive query response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "vehicle_not_found"
    );
}

#[tokio::test]
async fn public_drive_query_is_bounded_ordered_and_cursor_resumable() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api", "public-drive-query"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "9").with_tesla_identity(Some(9), None),
            1_000,
        )
        .expect("vehicle");
    for drive in [
        public_drive_fixture(1, 1_000),
        public_drive_fixture(2, 2_000),
        public_drive_fixture(3, 3_000),
    ] {
        seed_public_drive(&store, vehicle.vehicle_id, &drive);
    }
    mark_vehicle_published(&store, vehicle.vehicle_id);
    let wrong_key_app = public_query_router(store.clone(), 48);
    let app = public_query_router(store, 47);

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/drives?limit=2&from_ms=1000&to_ms=4000",
                    vehicle.vehicle_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("first drive page");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()[header::CACHE_CONTROL], "no-store");
    let etag = first
        .headers()
        .get(header::ETAG)
        .expect("drive page ETag")
        .clone();
    let first = response_json(first).await;
    assert_eq!(first["items"][0]["id"], 3);
    assert_eq!(first["items"][1]["id"], 2);
    assert_eq!(
        first["items"][0]["vehicle_id"],
        vehicle.vehicle_id.to_string()
    );
    let first_item = first["items"][0].as_object().expect("public drive object");
    assert!(!first_item.contains_key("car_id"));
    assert!(!first_item.contains_key("optimized_at_ms"));
    let cursor = first["next_cursor"].as_str().expect("next cursor");

    let wrong_key = wrong_key_app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/drives?limit=2&from_ms=1000&to_ms=4000&cursor={cursor}",
                    vehicle.vehicle_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("wrong cursor-key response");
    assert_eq!(wrong_key.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(wrong_key).await["error"]["code"],
        "invalid_cursor"
    );

    let not_modified = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/drives?limit=2&from_ms=1000&to_ms=4000",
                    vehicle.vehicle_id
                ))
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("conditional drive page");
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(not_modified.headers()[header::ETAG], etag);
    assert_eq!(not_modified.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        not_modified
            .into_body()
            .collect()
            .await
            .expect("304 body")
            .to_bytes()
            .is_empty()
    );

    let second = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/drives?limit=2&from_ms=1000&to_ms=4000&cursor={cursor}",
                    vehicle.vehicle_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("second drive page");
    assert_eq!(second.status(), StatusCode::OK);
    let second = response_json(second).await;
    assert_eq!(second["items"].as_array().expect("items").len(), 1);
    assert_eq!(second["items"][0]["id"], 1);
    assert!(second["next_cursor"].is_null());

    let wrong_scope = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/drives?limit=2&from_ms=999&to_ms=4000&cursor={cursor}",
                    vehicle.vehicle_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("wrong-scope cursor response");
    assert_eq!(wrong_scope.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(wrong_scope).await["error"]["code"],
        "invalid_cursor"
    );

    let mut tampered = cursor.as_bytes().to_vec();
    let last = tampered.last_mut().expect("cursor byte");
    *last = if *last == b'a' { b'b' } else { b'a' };
    let tampered = String::from_utf8(tampered).expect("ASCII cursor");
    let rejected = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{}/drives?limit=2&from_ms=1000&to_ms=4000&cursor={tampered}",
                    vehicle.vehicle_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("tampered cursor response");
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(rejected).await["error"]["code"],
        "invalid_cursor"
    );
}

#[tokio::test]
async fn public_drive_query_rejects_invalid_scope_and_preserves_device_auth() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([47; 32]);
    let app = paired_router(store.clone(), &cursor_key);
    let unknown_vehicle = Uuid::new_v4();

    let denied = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{unknown_vehicle}/drives"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unauthorized drive query");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let local = public_query_router(store, 47);
    let invalid_range = local
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/vehicles/{unknown_vehicle}/drives?from_ms=2000&to_ms=2000"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("invalid range response");
    assert_eq!(invalid_range.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(invalid_range).await["error"]["code"],
        "invalid_time_range"
    );

    let invalid_limit = local
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{unknown_vehicle}/drives?limit=501"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("invalid limit response");
    assert_eq!(invalid_limit.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(invalid_limit).await["error"]["code"],
        "invalid_limit"
    );

    let unknown = local
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{unknown_vehicle}/drives"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unknown vehicle response");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(unknown).await["error"]["code"],
        "vehicle_not_found"
    );
}

#[tokio::test]
async fn public_drive_query_fails_closed_on_catalogue_identity_divergence() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api", "public-drive-identity"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "9").with_tesla_identity(Some(9), None),
            1_000,
        )
        .expect("vehicle");
    let drive = public_drive_fixture(99, 1_000);
    store
        .open()
        .expect("open store")
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, 1, ?2, ?3)",
            rusqlite::params![
                vehicle.vehicle_id.to_string(),
                drive.car_id,
                serde_json::to_string(&drive).expect("drive JSON")
            ],
        )
        .expect("seed divergent drive identity");
    mark_vehicle_published(&store, vehicle.vehicle_id);

    let response = public_query_router(store, 47)
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{}/drives", vehicle.vehicle_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("divergent identity response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "service_unavailable"
    );
}

#[tokio::test]
async fn public_drive_query_requires_a_cursor_integrity_key() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let response = router(store)
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{}/drives", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unsigned query response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "service_unavailable"
    );
}

#[test]
fn pairing_claim_credentials_are_zeroizing_and_redacted() {
    let secret = PairingSecretInput(zeroize::Zeroizing::new("pairing-secret-canary".to_owned()));
    let bearer = PairingBearer(zeroize::Zeroizing::new("bearer-token-canary".to_owned()));
    assert!(!format!("{secret:?}").contains("pairing-secret-canary"));
    assert!(!format!("{bearer:?}").contains("bearer-token-canary"));

    let response = PairingClaimResponse {
        device_id: Uuid::nil(),
        access_token: bearer,
        expires_at_ms: 123,
    };
    let encoded = serde_json::to_vec(&response).expect("pairing response JSON");
    assert!(String::from_utf8_lossy(&encoded).contains("bearer-token-canary"));
}

#[test]
fn one_device_cannot_consume_the_global_pack_stream_pool() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let state = AppState::new(
        HubStore::initialize(temporary.path()).expect("store"),
        false,
        true,
        true,
        None,
        None,
        None,
        None,
    );
    let first_id = Uuid::new_v4();
    let first = state
        .try_acquire_pack_device_slot(first_id)
        .expect("first device slot");
    let second = state
        .try_acquire_pack_device_slot(first_id)
        .expect("second device slot");
    assert!(state.try_acquire_pack_device_slot(first_id).is_none());
    let other = state
        .try_acquire_pack_device_slot(Uuid::new_v4())
        .expect("other device remains admitted");
    drop(first);
    assert!(state.try_acquire_pack_device_slot(first_id).is_some());
    drop(second);
    drop(other);
}

#[tokio::test]
async fn stalled_pack_body_releases_global_and_device_slots() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let state = AppState::new(
        HubStore::initialize(temporary.path()).expect("store"),
        false,
        true,
        true,
        None,
        None,
        None,
        None,
    );
    let device_id = Uuid::new_v4();
    let device_slot = state
        .try_acquire_pack_device_slot(device_id)
        .expect("device slot");
    let permit = state
        .pack_stream_slots
        .clone()
        .try_acquire_owned()
        .expect("global slot");
    let (mut writer, reader) = tokio::io::duplex(512 * 1024);
    let writer_task = tokio::spawn(async move {
        tokio::io::AsyncWriteExt::write_all(&mut writer, &vec![7_u8; 512 * 1024])
            .await
            .expect("feed pack bytes");
    });
    let _unread_body = pack_stream_body(reader, permit, device_slot, Duration::from_millis(10));

    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        state.pack_stream_slots.available_permits(),
        MAX_ACTIVE_PACK_STREAMS
    );
    let recovered = state
        .try_acquire_pack_device_slot(device_id)
        .expect("stalled device slot released");
    drop(recovered);
    writer_task.abort();
}

#[tokio::test]
async fn serves_catalogued_manifest_and_immutable_pack_stream() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let installation_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();
    let vehicle_id = Uuid::new_v4();
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
    assert_eq!(wrong_key_response.status(), StatusCode::SERVICE_UNAVAILABLE);
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

#[test]
fn device_auth_store_errors_are_unavailable_not_unauthorized() {
    let device = PairedDeviceRecord {
        device_id: Uuid::nil(),
        display_name: "hub".to_owned(),
        created_at_ms: 1,
        expires_at_ms: 2,
        revoked_at_ms: None,
        last_authenticated_at_ms: None,
    };
    assert!(matches!(
        device_auth_from_store(Ok(Some(device))),
        DeviceAuthDecision::Allow(_)
    ));
    assert!(matches!(
        device_auth_from_store(Ok(None)),
        DeviceAuthDecision::Unauthorized
    ));
    assert!(matches!(
        device_auth_from_store(Err(crate::db::StoreError::Integrity(
            "database is locked".to_owned()
        ))),
        DeviceAuthDecision::Unavailable
    ));
    let missing = device_auth_response(device_auth_from_store(Ok(None))).expect_err("401");
    assert_eq!(missing, StatusCode::UNAUTHORIZED);
    assert_eq!(
        device_auth_reject(missing).status(),
        StatusCode::UNAUTHORIZED
    );
    let unavailable = device_auth_response(device_auth_from_store(Err(
        crate::db::StoreError::Integrity("database is locked".to_owned()),
    )))
    .expect_err("503");
    assert_eq!(unavailable, StatusCode::SERVICE_UNAVAILABLE);
    assert_ne!(unavailable, StatusCode::UNAUTHORIZED);
    assert_eq!(
        device_auth_reject(unavailable).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
