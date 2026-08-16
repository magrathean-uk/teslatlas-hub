//! Mac-local TLS end-to-end: claim → vehicle → signed manifest → pack stream
//! with Range resume. Drives the shipped server and pack contracts without
//! systemd, Docker, or a Debian VM.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use axum_server::tls_rustls::RustlsConfig;
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
    protocol::{CursorKey, SequenceRange},
    server::paired_router,
};
use tokio::sync::oneshot;
use uuid::Uuid;

#[tokio::test]
async fn claim_manifest_and_range_resume_over_real_tls() {
    let root = tempfile::tempdir().expect("temp root");
    let store = HubStore::initialize(root.path()).expect("store");
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
        "secret": invitation.secret(),
        "device_name": "Mac Simulator E2E"
    })
    .to_string();
    let claim = read_json(
        client
            .post(format!(
                "{base}/v1/pairings/{}/claim",
                invitation.pairing_id
            ))
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
        .post(format!(
            "{base}/v1/pairings/{}/claim",
            invitation.pairing_id
        ))
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

    let _ = shutdown_tx.send(());
    let _ = server.await;
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
    let mut descriptor = VehicleDescriptor::new(source.source_id, "9");
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
            source_eid: None,
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
    let built = ProjectionPackWriter::new(store.packs_dir()).write_full_snapshot(&request)?;
    let manifest = request.signed_manifest(&built, cursor_key)?;
    store.publish_manifest(&manifest)?;
    Ok(Published {
        vehicle_id: vehicle.vehicle_id,
    })
}

fn write_self_signed_localhost_cert(dir: &Path) -> (PathBuf, PathBuf) {
    let cert = dir.join("leaf.pem");
    let key = dir.join("leaf-key.pem");
    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-sha256",
            "-days",
            "1",
            "-nodes",
            "-keyout",
            key.to_str().expect("key path"),
            "-out",
            cert.to_str().expect("cert path"),
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost,IP:127.0.0.1",
        ])
        .status()
        .expect("openssl available on Mac");
    assert!(status.success(), "openssl failed to mint local TLS leaf");
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
        // Self-signed local leaf for Mac e2e; iOS pins by SHA-256 instead.
        .danger_accept_invalid_certs(true)
        .build()
        .expect("client")
}
