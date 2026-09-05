// SPDX-License-Identifier: AGPL-3.0-only

//! Loopback TLS end-to-end: claim → vehicle → signed manifest → pack stream
//! with Range resume. Runs both the router and the real native Hub executable
//! without systemd, Docker, provider credentials, or a live vehicle.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use axum_server::tls_rustls::RustlsConfig;
use base64::Engine;
use ed25519_dalek::Verifier;
use reqwest::{
    Client,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, RANGE},
    redirect::Policy,
};
use sha2::{Digest, Sha256};
use teslatlas_hub::{
    db::{HubStore, SourceDescriptor, VehicleDescriptor},
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionDrive, ProjectionPackRequest,
        ProjectionPackWriter, ProjectionPosition, ProjectionSnapshot,
    },
    protocol::{CursorKey, SequenceRange, Sha256Digest},
    server::paired_router,
};
use tokio::sync::oneshot;
use uuid::Uuid;

#[tokio::test]
async fn claim_manifest_and_range_resume_over_real_tls() {
    let root = tempfile::tempdir().expect("temp root");
    let store = HubStore::initialize(root.path().join("hub")).expect("store");
    let cursor_key = CursorKey::from_bytes([0xA5; 32]);
    let published = publish_typed_snapshot(&store, &cursor_key).expect("seed pack");

    // Pair before moving the store into the server task.
    let invitation = store
        .create_pairing("simulator-e2e", 1_000, i64::MAX)
        .expect("pairing");

    let (cert_path, key_path) = write_self_signed_localhost_cert(root.path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = spawn_tls_server(
        store,
        cursor_key,
        cert_path.clone(),
        key_path,
        listener,
        shutdown_rx,
    )
    .await;

    let client = pinned_https_client(&cert_path);
    let base = format!("https://127.0.0.1:{}", addr.port());
    wait_until_ready(&client, &base).await;

    assert_pairing_and_pack_transfer(
        &client,
        &base,
        &published,
        invitation.pairing_id,
        invitation.secret(),
    )
    .await;
    let _ = shutdown_tx.send(());
    let _ = server.await;
}

async fn assert_pairing_and_pack_transfer(
    client: &Client,
    base: &str,
    published: &Published,
    pairing_id: Uuid,
    pairing_secret: &str,
) {
    for path in [
        "/v1/vehicles".to_owned(),
        format!("/v1/vehicles/{}/sync/manifest", published.vehicle_id),
    ] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .expect("unauthenticated request")
                .status(),
            reqwest::StatusCode::UNAUTHORIZED,
        );
    }

    // Capabilities publish the manifest verifying key.
    let capabilities = read_json(
        client
            .get(format!("{base}/.well-known/teslatlas-hub"))
            .send()
            .await
            .expect("capabilities")
            .error_for_status()
            .expect("capabilities status"),
    )
    .await;
    assert_eq!(capabilities["protocol"], "teslatlas-sync");
    assert!(
        capabilities["manifestPublicKey"]
            .as_str()
            .expect("key")
            .len()
            == 64
    );

    // One-use claim.
    let claim_body = serde_json::json!({
        "secret": pairing_secret,
        "device_name": "Mac Simulator E2E"
    })
    .to_string();
    let claim = read_json(
        client
            .post(format!("{base}/v1/pairings/{}/claim", pairing_id))
            .header(CONTENT_TYPE, "application/json")
            .body(claim_body.clone())
            .send()
            .await
            .expect("claim send")
            .error_for_status()
            .expect("claim status"),
    )
    .await;
    let token = claim["access_token"].as_str().expect("token").to_owned();
    assert_eq!(token.len(), 64);

    // Replay of the same secret must fail closed.
    let replay = client
        .post(format!("{base}/v1/pairings/{}/claim", pairing_id))
        .header(CONTENT_TYPE, "application/json")
        .body(claim_body)
        .send()
        .await
        .expect("replay send");
    assert_eq!(replay.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Vehicle list exposes the published vehicle only.
    let vehicles = read_json(
        client
            .get(format!("{base}/v1/vehicles"))
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("vehicles")
            .error_for_status()
            .expect("vehicles status"),
    )
    .await;
    let listed = vehicles["vehicles"].as_array().expect("array");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0]["vehicle_id"].as_str().expect("id"),
        published.vehicle_id.to_string()
    );

    // Signed manifest for the selected vehicle.
    let manifest_response = client
        .get(format!(
            "{base}/v1/vehicles/{}/sync/manifest",
            published.vehicle_id
        ))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(ACCEPT, "application/json")
        .send()
        .await
        .expect("manifest")
        .error_for_status()
        .expect("manifest status");
    let signature = manifest_response
        .headers()
        .get("x-teslatlas-manifest-signature")
        .expect("signature header")
        .to_str()
        .expect("signature ascii")
        .to_owned();
    assert!(!signature.is_empty());
    let raw_manifest = manifest_response.bytes().await.expect("manifest body");
    let public_key: [u8; 32] = hex::decode(
        capabilities["manifestPublicKey"]
            .as_str()
            .expect("public key"),
    )
    .expect("hex public key")
    .try_into()
    .expect("32-byte public key");
    let signature = base64::engine::general_purpose::STANDARD
        .decode(&signature)
        .expect("base64 signature");
    ed25519_dalek::VerifyingKey::from_bytes(&public_key)
        .expect("verifying key")
        .verify(
            &raw_manifest,
            &ed25519_dalek::Signature::from_slice(&signature).expect("signature"),
        )
        .expect("manifest signature authenticates downloaded bytes");
    let manifest: serde_json::Value = serde_json::from_slice(&raw_manifest).expect("manifest json");
    assert_eq!(
        manifest["vehicle_id"].as_str().expect("vehicle"),
        published.vehicle_id.to_string()
    );
    assert_eq!(manifest["chunk_count"].as_u64().expect("chunks"), 1);
    let pack_sha = manifest["chunks"][0]["sha256"]
        .as_str()
        .expect("pack sha")
        .to_owned();
    let pack_bytes = manifest["chunks"][0]["compressed_bytes"]
        .as_u64()
        .expect("pack size");
    assert!(pack_bytes > 100);

    // Full pack download.
    let pack_url = format!("{base}/v1/packs/sha256/{pack_sha}.sqlite.zst");
    assert_eq!(
        client
            .get(&pack_url)
            .send()
            .await
            .expect("unauthenticated pack request")
            .status(),
        reqwest::StatusCode::UNAUTHORIZED,
    );
    let full = client
        .get(&pack_url)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .expect("full pack")
        .error_for_status()
        .expect("full pack status")
        .bytes()
        .await
        .expect("full pack body");
    assert_eq!(full.len() as u64, pack_bytes);
    assert_eq!(hex::encode(Sha256::digest(&full)), pack_sha);

    // Simulated interrupted transfer: keep a prefix, Range-resume the tail.
    let cut = (pack_bytes / 3).max(1).min(pack_bytes - 1);
    let prefix = &full[..cut as usize];
    let partial = client
        .get(&pack_url)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(RANGE, format!("bytes={cut}-"))
        .send()
        .await
        .expect("range pack");
    assert_eq!(partial.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    let content_range = partial
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .expect("content-range")
        .to_str()
        .expect("range ascii")
        .to_owned();
    assert!(
        content_range.starts_with(&format!("bytes {cut}-")),
        "unexpected content-range {content_range}"
    );
    let tail = partial.bytes().await.expect("range body");
    assert_eq!(tail.len() as u64, pack_bytes - cut);

    let mut resumed = Vec::with_capacity(pack_bytes as usize);
    resumed.extend_from_slice(prefix);
    resumed.extend_from_slice(&tail);
    assert_eq!(resumed.len() as u64, pack_bytes);
    assert_eq!(hex::encode(Sha256::digest(&resumed)), pack_sha);
    assert_eq!(resumed, full.as_ref());

    // Wrong-vehicle manifest path is absent.
    let wrong = client
        .get(format!(
            "{base}/v1/vehicles/{}/sync/manifest",
            Uuid::new_v4()
        ))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .expect("wrong vehicle");
    assert_eq!(wrong.status(), reqwest::StatusCode::NOT_FOUND);
}

/// Run against Cargo's real executable, or a native release/package artifact:
/// TESLATLAS_HUB_SMOKE_BINARY=/absolute/path/teslatlas-hub cargo test --test
/// tls_import_e2e packaged_binary_serves_seeded_snapshot -- --exact
#[tokio::test]
async fn packaged_binary_serves_seeded_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    use teslatlas_hub::{
        credentials::OwnerTokens,
        db::TeslaMateLegacyTokenStore,
        teslamate_credentials::{load_or_create_cursor_key, replace_key_and_tokens},
        teslamate_token::encrypt_legacy_owner_tokens,
    };

    let root = tempfile::tempdir().expect("isolated smoke root");
    let data_dir = root.path().join("hub");
    let store = HubStore::initialize(&data_dir).expect("smoke store");
    let cursor_key = load_or_create_cursor_key(&data_dir).expect("durable synthetic signing key");
    let published = publish_typed_snapshot(&store, &cursor_key).expect("synthetic snapshot");
    // macOS Serve preflight requires a usable encrypted pair even when the
    // collector is disabled. These literals have no authority at any provider.
    let tokens = OwnerTokens::from_file_bytes(
        zeroize::Zeroizing::new(b"smoke-test-not-a-tesla-access-token".to_vec()),
        zeroize::Zeroizing::new(b"smoke-test-not-a-tesla-refresh-token".to_vec()),
    )
    .expect("synthetic tokens");
    let key = b"smoke-test-local-encryption-key";
    let (access, refresh) =
        encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt synthetic pair");
    let stored = TeslaMateLegacyTokenStore::imported(access, refresh).expect("synthetic pair");
    replace_key_and_tokens(&data_dir, &store, key, &stored).expect("seed synthetic pair");
    let invitation = store
        .create_pairing("binary-smoke", 1_000, i64::MAX)
        .expect("pairing");
    store
        .checkpoint_catalogue_for_immutable_read()
        .expect("preflight snapshot");
    drop(store);

    let (cert, private_key) = write_self_signed_localhost_cert(root.path());
    let port_reservation =
        std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral loopback port");
    let address = port_reservation.local_addr().expect("port");
    let base = format!("https://{address}");
    // A bound, unserved loopback endpoint catches any accidental Owner API call.
    let owner_api_sink = std::net::TcpListener::bind("127.0.0.1:0").expect("Owner API sink");
    owner_api_sink
        .set_nonblocking(true)
        .expect("nonblocking sink");
    let config = root.path().join("config.toml");
    fs::write(&config, format!(
        "data_dir = {data_dir:?}\nbind = {address:?}\n\
         [tls]\ncertificate_path = {cert:?}\nprivate_key_path = {private_key:?}\npublic_url = {base:?}\n\
         [collector]\ninterval_seconds = 0\nowner_api_base_url = {owner_api:?}\n\
         [collector.legacy_auth]\nenabled = false\n\
         [terrain]\nenabled = false\n[geocoder]\nenabled = false\n",
        data_dir = data_dir.to_str().expect("UTF-8 path"),
        address = address.to_string(),
        cert = cert.to_str().expect("UTF-8 cert path"),
        private_key = private_key.to_str().expect("UTF-8 key path"),
        owner_api = format!("https://{}/", owner_api_sink.local_addr().expect("sink address")),
    )).expect("smoke config");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).expect("private config");
    let binary = std::env::var_os("TESLATLAS_HUB_SMOKE_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_teslatlas-hub")));
    let log_path = root.path().join("serve.log");
    let log = fs::File::create(&log_path).expect("process log");
    drop(port_reservation);
    let mut process = SmokeProcess(
        Command::new(&binary)
            .arg("--config")
            .arg(&config)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().expect("log clone")))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("launch smoke binary"),
    );
    let client = pinned_https_client(&cert);
    let mut healthy = false;
    for _ in 0..100 {
        if let Some(status) = process.0.try_wait().expect("child status") {
            panic!(
                "smoke binary exited {status}: {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        if client
            .get(format!("{base}/healthz"))
            .timeout(Duration::from_millis(250))
            .send()
            .await
            .is_ok_and(|reply| reply.status().is_success())
        {
            healthy = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        healthy,
        "binary never became healthy: {}",
        fs::read_to_string(&log_path).unwrap_or_default()
    );
    let ready = client
        .get(format!("{base}/readyz"))
        .send()
        .await
        .expect("readiness");
    assert_eq!(ready.status(), reqwest::StatusCode::OK);
    assert_eq!(read_json(ready).await["status"], "ready");
    assert_pairing_and_pack_transfer(
        &client,
        &base,
        &published,
        invitation.pairing_id,
        invitation.secret(),
    )
    .await;
    assert_eq!(
        owner_api_sink
            .accept()
            .expect_err("disabled collector must not contact Owner API")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );

    let pid = rustix::process::Pid::from_raw(process.0.id() as i32).expect("child pid");
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).expect("graceful SIGTERM");
    let mut stopped = false;
    for _ in 0..100 {
        if let Some(status) = process.0.try_wait().expect("shutdown status") {
            assert!(status.success(), "binary shutdown failed: {status}");
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(stopped, "binary did not stop after SIGTERM");
    assert!(
        std::net::TcpStream::connect(address).is_err(),
        "listener outlived binary"
    );
}

struct SmokeProcess(Child);

impl Drop for SmokeProcess {
    fn drop(&mut self) {
        // Test panics must never leave a resident Hub behind.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn read_json(response: reqwest::Response) -> serde_json::Value {
    let bytes = response.bytes().await.expect("response body");
    serde_json::from_slice(&bytes).expect("response json")
}

struct Published {
    vehicle_id: Uuid,
}

fn publish_typed_snapshot(
    store: &HubStore,
    cursor_key: &CursorKey,
) -> Result<Published, Box<dyn std::error::Error>> {
    let now = 1_800_000_000_000_i64;
    let source = store.register_source(
        &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
        now,
    )?;
    let mut descriptor =
        VehicleDescriptor::new(source.source_id, "9").with_tesla_identity(Some(9), None);
    descriptor.display_name = Some("E2E Model 3".to_owned());
    descriptor.vin = Some("5YJ3E1EA7KF000001".to_owned());
    let vehicle = store.register_vehicle(&descriptor, now)?;
    let installation_id = store.installation_id()?;
    let selected_car_id = 9_i64;
    let snapshot = ProjectionSnapshot {
        cars: vec![ProjectionCar {
            id: selected_car_id,
            name: "E2E Model 3".to_owned(),
            model: "model3".to_owned(),
            vin: Some("5YJ3E1EA7KF000001".to_owned()),
            source_eid: Some(9),
            source_vid: None,
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            firmware_version: Some("2026.20".to_owned()),
            efficiency_wh_per_km: None,
            settings: Default::default(),
        }],
        drives: vec![ProjectionDrive {
            id: 1,
            car_id: selected_car_id,
            optimized_at_ms: None,
            start_date_ms: now - 600_000,
            end_date_ms: now - 300_000,
            distance_km: Some(4.2),
            duration_min: Some(5),
            efficiency: None,
            outside_temp_avg: Some(18.0),
            inside_temp_avg: Some(21.0),
            speed_max: Some(72),
            power_max: Some(36.0),
            power_min: Some(-7.0),
            start_ideal_range_km: Some(338.8),
            end_ideal_range_km: Some(334.8),
            start_address: None,
            end_address: None,
            start_geofence: None,
            end_geofence: None,
            start_latitude: Some(47.50),
            start_longitude: Some(19.04),
            end_latitude: Some(47.51),
            end_longitude: Some(19.05),
            start_soc: Some(70),
            end_soc: Some(68),
            start_rated_range_km: Some(320.0),
            end_rated_range_km: Some(315.0),
            ascent: Some(60),
            descent: Some(30),
        }],
        positions: vec![
            ProjectionPosition {
                id: 1,
                drive_id: Some(1),
                car_id: selected_car_id,
                date_ms: now - 600_000,
                latitude: 47.50,
                longitude: 19.04,
                speed: Some(20),
                power: Some(12.0),
                battery_level: Some(70),
                usable_battery_level: Some(69),
                elevation: None,
                odometer: Some(12_000.0),
                ideal_battery_range_km: None,
                est_battery_range_km: None,
                rated_battery_range_km: Some(320.0),
                fan_status: None,
                driver_temp_setting: None,
                passenger_temp_setting: None,
                is_climate_on: Some(true),
                is_rear_defroster_on: None,
                is_front_defroster_on: None,
                inside_temp: Some(21.0),
                outside_temp: Some(18.0),
                battery_heater: None,
                battery_heater_on: None,
                battery_heater_no_power: None,
                tpms_pressure_fl: None,
                tpms_pressure_fr: None,
                tpms_pressure_rl: None,
                tpms_pressure_rr: None,
            },
            ProjectionPosition {
                id: 2,
                drive_id: Some(1),
                car_id: selected_car_id,
                date_ms: now - 300_000,
                latitude: 47.51,
                longitude: 19.05,
                speed: Some(0),
                power: Some(0.0),
                battery_level: Some(68),
                usable_battery_level: Some(67),
                elevation: None,
                odometer: Some(12_004.2),
                ideal_battery_range_km: None,
                est_battery_range_km: None,
                rated_battery_range_km: Some(315.0),
                fan_status: None,
                driver_temp_setting: None,
                passenger_temp_setting: None,
                is_climate_on: Some(true),
                is_rear_defroster_on: None,
                is_front_defroster_on: None,
                inside_temp: Some(21.0),
                outside_temp: Some(18.0),
                battery_heater: None,
                battery_heater_on: None,
                battery_heater_no_power: None,
                tpms_pressure_fl: None,
                tpms_pressure_fr: None,
                tpms_pressure_rl: None,
                tpms_pressure_rr: None,
            },
        ],
        charges: Vec::new(),
        charge_samples: Vec::new(),
    };
    let sequence = store.next_full_snapshot_sequence(vehicle.vehicle_id)?;
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: ProjectionBinding {
            installation_id,
            account_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: source.generation,
            selected_car_id,
        },
        sequence: SequenceRange {
            from_exclusive: sequence,
            to_inclusive: sequence,
        },
        snapshot: &snapshot,
    };
    let built = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])?;
    let manifest = request.signed_manifest_with_states_and_updates(&built, &[], &[], cursor_key)?;
    store.finalize_import_snapshot_with_binding(
        &manifest,
        Sha256Digest::from_bytes([0x5A; 32]),
        &[],
        &request.binding,
    )?;
    Ok(Published {
        vehicle_id: vehicle.vehicle_id,
    })
}

fn write_self_signed_localhost_cert(dir: &Path) -> (PathBuf, PathBuf) {
    let cert = dir.join("leaf.pem");
    let key = dir.join("leaf-key.pem");
    let identity =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .expect("local TLS identity");
    fs::write(&cert, identity.cert.pem()).expect("certificate");
    fs::write(&key, identity.signing_key.serialize_pem()).expect("private key");
    // Restrict key mode the way production expects for private material.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).expect("key mode");
    }
    (cert, key)
}

async fn spawn_tls_server(
    store: HubStore,
    cursor_key: CursorKey,
    cert_path: PathBuf,
    key_path: PathBuf,
    listener: std::net::TcpListener,
    shutdown_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    teslatlas_hub::crypto::install_default_provider();
    let tls = RustlsConfig::from_pem_file(cert_path, key_path)
        .await
        .expect("tls config");
    let app = paired_router(store, &cursor_key);
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    let server = tokio::spawn(async move {
        let result = axum_server::from_tcp_rustls(listener, tls)
            .expect("tls server from tcp")
            .handle(handle)
            .serve(app.into_make_service())
            .await;
        if let Err(error) = result {
            panic!("TLS server failed: {error}");
        }
    });
    // Drive graceful stop when the test finishes.
    tokio::spawn(async move {
        let _ = shutdown_rx.await;
        shutdown_handle.shutdown();
    });
    // Yield so the accept loop can start on the pre-bound listener.
    for _ in 0..20 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    server
}

async fn wait_until_ready(client: &Client, base: &str) {
    let url = format!("{base}/healthz");
    let mut last = String::from("no attempt");
    for _ in 0..50 {
        match client.get(&url).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(response) => last = format!("http {}", response.status()),
            Err(error) => last = format!("error: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("TLS Hub did not become ready at {url}: {last}");
}

fn pinned_https_client(cert_path: &Path) -> Client {
    teslatlas_hub::crypto::install_default_provider();
    // For this Mac e2e proof we accept the generated local leaf. Production
    // iOS pins the SHA-256 fingerprint from the pairing URI instead.
    let pem = fs::read(cert_path).expect("read cert");
    let cert = reqwest::Certificate::from_pem(&pem).expect("parse cert");
    Client::builder()
        .use_rustls_tls()
        .https_only(true)
        .redirect(Policy::none())
        .timeout(Duration::from_secs(10))
        .add_root_certificate(cert)
        .build()
        .expect("client")
}
