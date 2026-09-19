// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    os::unix::fs::PermissionsExt,
    time::Duration,
};

use super::super::serve_with_cursor_key;
use super::{
    admitted_server_fixture, local_tls_server_config, wait_for_tcp_listener, wait_for_tcp_rebind,
};
use crate::{
    config::{CollectorProvider, FleetTelemetryConfig, HubConfig},
    db::HubStore,
    protocol::{CursorKey, Sha256Digest},
};
use axum::http::StatusCode;

static FLEET_LISTENER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn local_fleet_tls_server_config(
    temporary: &tempfile::TempDir,
    data_dir: std::path::PathBuf,
    bind: SocketAddr,
) -> (HubConfig, String) {
    let mut config = local_tls_server_config(temporary, data_dir, bind);
    let token = "a".repeat(64);
    let token_path = temporary.path().join("telemetry-token");
    fs::write(&token_path, format!("{token}\n")).expect("write telemetry token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))
        .expect("protect telemetry token");
    config.collector.provider = CollectorProvider::Fleet;
    config.collector.fleet_command_proxy_url = Some("https://127.0.0.1:4443".to_owned());
    config.collector.fleet_telemetry = Some(FleetTelemetryConfig {
        hostname: "telemetry.example.test".to_owned(),
        port: 443,
        ca_certificate_path: temporary.path().join("telemetry-ca.pem"),
        ingest_token_path: token_path,
    });
    (config, token)
}

fn synthetic_fleet_telemetry_body() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": "tx-server-test",
        "tx_type": "vehicle_data",
        "received_at_ms": 1_800_000_000_100_i64,
        "timestamp_ms": 1_800_000_000_000_i64,
        "payload": {"data": {"Soc": {"intValue": "80"}}}
    })
}

#[tokio::test]
async fn fleet_tls_uses_private_loopback_ingress_and_excludes_it_from_public_router() {
    let _listener_guard = FLEET_LISTENER_TEST_LOCK.lock().await;
    let temporary = crate::private_tempdir().expect("temporary Fleet TLS server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let private_reservation = TcpListener::bind(crate::config::fleet_telemetry_ingress_bind())
        .expect("reserve Fleet ingress");
    let public_reservation =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve public port");
    let public_bind = public_reservation.local_addr().expect("public address");
    drop(public_reservation);
    let (config, token) = local_fleet_tls_server_config(&temporary, store_path, public_bind);
    let store = HubStore::initialize(&config.data_dir).expect("store");
    drop(private_reservation);
    let private_bind = crate::config::fleet_telemetry_ingress_bind();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        serve_with_cursor_key(
            store,
            &config,
            Sha256Digest::of_bytes(b"Fleet private listener test"),
            Some(CursorKey::from_bytes([61; 32])),
            Some(admission),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });

    wait_for_tcp_listener(public_bind).await;
    wait_for_tcp_listener(private_bind).await;
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("local TLS test client");
    let body = synthetic_fleet_telemetry_body();
    let public_response = client
        .post(format!("https://{public_bind}/v1/internal/fleet-telemetry"))
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("public TLS response");
    assert_eq!(
        public_response.status(),
        StatusCode::NOT_FOUND,
        "the public router must omit private Fleet ingestion even with its bearer"
    );
    let private_unauthorized = client
        .post(format!("http://{private_bind}/v1/internal/fleet-telemetry"))
        .bearer_auth("wrong-token")
        .json(&body)
        .send()
        .await
        .expect("private unauthorized response");
    assert_eq!(
        private_unauthorized.status(),
        StatusCode::UNAUTHORIZED,
        "the fixed private listener must retain bearer admission"
    );
    let private_response = client
        .post(format!("http://{private_bind}/v1/internal/fleet-telemetry"))
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("private Fleet response");
    assert_eq!(
        private_response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "the fixed private listener must reach the token-gated handler"
    );

    shutdown_tx.send(()).expect("signal dual listener shutdown");
    server_task
        .await
        .expect("dual listener task")
        .expect("dual listener shutdown");
    drop(wait_for_tcp_rebind(public_bind).await);
    drop(wait_for_tcp_rebind(private_bind).await);
}

#[tokio::test]
async fn fleet_tls_private_bind_failure_releases_public_listener() {
    let _listener_guard = FLEET_LISTENER_TEST_LOCK.lock().await;
    let temporary = crate::private_tempdir().expect("temporary Fleet bind-failure root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let private_reservation = TcpListener::bind(crate::config::fleet_telemetry_ingress_bind())
        .expect("reserve Fleet ingress");
    let public_reservation =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve public port");
    let public_bind = public_reservation.local_addr().expect("public address");
    drop(public_reservation);
    let (config, _token) = local_fleet_tls_server_config(&temporary, store_path, public_bind);
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        serve_with_cursor_key(
            store,
            &config,
            Sha256Digest::of_bytes(b"Fleet private bind failure test"),
            Some(CursorKey::from_bytes([62; 32])),
            Some(admission),
            std::future::pending(),
        ),
    )
    .await
    .expect("private bind failure is immediate")
    .expect_err("private bind failure must fail Serve");
    assert!(
        result.to_string().contains("address already in use")
            || result.to_string().contains("Address already in use"),
        "unexpected private bind failure: {result}"
    );
    drop(private_reservation);
    drop(wait_for_tcp_rebind(public_bind).await);
}

#[tokio::test]
async fn fleet_tls_server_cancellation_releases_both_listeners() {
    let _listener_guard = FLEET_LISTENER_TEST_LOCK.lock().await;
    let temporary = crate::private_tempdir().expect("temporary Fleet cancellation root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let private_reservation = TcpListener::bind(crate::config::fleet_telemetry_ingress_bind())
        .expect("reserve Fleet ingress");
    let public_reservation =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve public port");
    let public_bind = public_reservation.local_addr().expect("public address");
    drop(public_reservation);
    let (config, _token) = local_fleet_tls_server_config(&temporary, store_path, public_bind);
    let store = HubStore::initialize(&config.data_dir).expect("store");
    drop(private_reservation);
    let private_bind = crate::config::fleet_telemetry_ingress_bind();
    let server_task = tokio::spawn(async move {
        serve_with_cursor_key(
            store,
            &config,
            Sha256Digest::of_bytes(b"Fleet dual listener cancellation test"),
            Some(CursorKey::from_bytes([63; 32])),
            Some(admission),
            std::future::pending(),
        )
        .await
    });

    wait_for_tcp_listener(public_bind).await;
    wait_for_tcp_listener(private_bind).await;
    server_task.abort();
    let cancellation = server_task
        .await
        .expect_err("outer dual listener task is cancelled");
    assert!(cancellation.is_cancelled());
    drop(wait_for_tcp_rebind(public_bind).await);
    drop(wait_for_tcp_rebind(private_bind).await);
}
