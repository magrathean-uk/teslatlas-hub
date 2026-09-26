// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

#[tokio::test]
async fn plain_loopback_server_rejects_missing_bearer_and_wrong_host() {
    let temporary = crate::private_tempdir().expect("temporary plain server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let config = local_plain_server_config(store_path, bind);
    let local_token_path = config.data_dir.join("secrets/local-api-token");
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        serve_with_cursor_key(
            store,
            &config,
            Sha256Digest::of_bytes(b"plain local access test"),
            None,
            Some(admission),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });

    wait_for_tcp_listener(bind).await;
    let client = reqwest::Client::new();
    let url = format!("http://{bind}/v1/vehicles");
    let absent = client
        .get(&url)
        .send()
        .await
        .expect("request without bearer");
    assert_eq!(absent.status(), StatusCode::UNAUTHORIZED);
    let health = client
        .get(format!("http://{bind}/healthz"))
        .send()
        .await
        .expect("public health probe");
    assert_eq!(health.status(), StatusCode::OK);
    let wrong_host = client
        .get(&url)
        .header(header::HOST, "evil.example")
        .send()
        .await
        .expect("request with wrong host");
    assert_eq!(wrong_host.status(), StatusCode::BAD_REQUEST);
    let duplicate_host = client
        .get(&url)
        .header(header::HOST, bind.to_string())
        .header(header::HOST, "evil.example")
        .send()
        .await
        .expect("request with duplicate Host headers");
    assert_eq!(duplicate_host.status(), StatusCode::BAD_REQUEST);
    let token = fs::read_to_string(&local_token_path).expect("private local bearer file");
    let mode = fs::metadata(&local_token_path)
        .expect("local bearer metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let authorized = client
        .get(&url)
        .bearer_auth(token.trim_end())
        .send()
        .await
        .expect("request with local bearer");
    assert_eq!(authorized.status(), StatusCode::OK);

    shutdown_tx.send(()).expect("signal shutdown");
    server_task
        .await
        .expect("server task")
        .expect("server shuts down");
}
