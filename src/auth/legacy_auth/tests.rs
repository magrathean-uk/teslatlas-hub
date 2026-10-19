// SPDX-License-Identifier: AGPL-3.0-only

// The submodules below take these names from here through `use super::*`.
#[allow(unused_imports)]
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[allow(unused_imports)]
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use reqwest::Client;
#[allow(unused_imports)]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    time::timeout,
};

#[allow(unused_imports)]
use super::wire::AcceptedTokens;
#[allow(unused_imports)]
use super::*;

#[path = "authority_tests.rs"]
mod authority_tests;
#[path = "endpoint_tests.rs"]
mod endpoint_tests;
#[path = "schedule_tests.rs"]
mod schedule_tests;
#[path = "state_tests.rs"]
mod state_tests;
#[path = "wire_tests.rs"]
mod wire_tests;

/// Spec section 8 fixtures. `t0` is 2027-01-15T08:00:00Z.
pub(super) const T0: i64 = 1_800_000_000;

/// The opaque access token of the spec fixtures (OP).
pub(super) const OP: &str = "opaque-access-0001";

/// `jwt(p)`: base64url-no-pad of the fixed header, a dot, base64url-no-pad of
/// the JSON payload, a dot and a dummy signature (spec section 8).
pub(super) fn jwt(payload: serde_json::Value) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    format!(
        "{}.{}.c2ln",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(payload.to_string())
    )
}

/// JG: a global-issuer token with an `exp` eight hours after `t0`.
pub(super) fn jg() -> String {
    jwt(serde_json::json!({"iss": "https://auth.tesla.com/oauth2/v3", "exp": 1_800_028_800_i64}))
}

/// JC: a China-issuer token with the same `exp`.
pub(super) fn jc() -> String {
    jwt(serde_json::json!({"iss": "https://auth.tesla.cn/oauth2/v3", "exp": 1_800_028_800_i64}))
}

/// JX: a token from an issuer that is not pinned.
pub(super) fn jx() -> String {
    jwt(serde_json::json!({"iss": "https://issuer.example.test/oauth2/v3"}))
}

#[test]
fn legacy_auth_is_redacted_and_zeroizable_on_drop() {
    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}

    let access = "owner-access-secret";
    let refresh = "owner-refresh-secret";
    let mut auth = LegacyAuth::for_test(
        Url::parse("https://auth.tesla.com/oauth2/v3/").unwrap(),
        access,
        refresh,
    );
    let debug = format!("{auth:?}");
    assert!(!debug.contains(access));
    assert!(!debug.contains(refresh));
    assert_zeroize_on_drop::<LegacyAuth>();

    auth.zeroize();
    assert!(auth.access_token().bytes().all(|byte| byte == 0));
    assert!(auth.refresh_token().bytes().all(|byte| byte == 0));
}

async fn raw_chunked_token_server(
    chunks: Vec<Vec<u8>>,
    complete: bool,
) -> (Url, tokio::task::JoinHandle<()>) {
    crate::crypto::install_default_provider();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await;
        if socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .is_err()
        {
            return;
        }
        for chunk in chunks {
            let header = format!("{:X}\r\n", chunk.len());
            if socket.write_all(header.as_bytes()).await.is_err()
                || socket.write_all(&chunk).await.is_err()
                || socket.write_all(b"\r\n").await.is_err()
            {
                return;
            }
        }
        if complete {
            let _ = socket.write_all(b"0\r\n\r\n").await;
        } else {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    (
        Url::parse(&format!("http://{address}/oauth2/v3/")).unwrap(),
        task,
    )
}
