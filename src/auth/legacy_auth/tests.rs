// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Router,
    extract::State,
    http::{
        HeaderMap, StatusCode,
        header::{ACCEPT, ACCEPT_LANGUAGE, USER_AGENT},
    },
    response::IntoResponse,
    routing::{get, post},
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    time::timeout,
};

use super::*;
use crate::owner_api::{OwnerApi, OwnerApiAuthError, OwnerApiError};

fn access_token(issuer: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::json!({"iss": issuer}).to_string());
    format!("{header}.{payload}.signature")
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

#[test]
fn derives_safe_global_and_china_issuer_regions() {
    let global =
        LegacyAuth::from_access_token(access_token("https://auth.tesla.com/oauth2/v3"), "refresh")
            .unwrap();
    assert_eq!(global.region(), StreamRegion::Global);
    let china =
        LegacyAuth::from_access_token(access_token("https://auth.tesla.cn/oauth2/v3/"), "refresh")
            .unwrap();
    assert_eq!(china.region(), StreamRegion::China);
    assert!(
        LegacyAuth::from_access_token(access_token("https://evil.example/oauth2/v3"), "refresh",)
            .is_err()
    );
    assert!(
        LegacyAuth::from_access_token(
            access_token("https://auth.tesla.com:8443/oauth2/v3"),
            "refresh",
        )
        .is_err()
    );
    let default_port = LegacyAuth::from_access_token(
        access_token("https://auth.tesla.com:443/oauth2/v3"),
        "refresh",
    )
    .unwrap();
    assert_eq!(default_port.issuer, global.issuer);
}

#[test]
fn derives_canonical_issuers_for_teslamate_opaque_access_tokens() {
    for token in ["qts-access-token", "eu-access-token", "qts-"] {
        let auth = LegacyAuth::from_access_token(token, "refresh").unwrap();
        assert_eq!(auth.region(), StreamRegion::Global);
    }
    let china = LegacyAuth::from_access_token("cn-access-token", "refresh").unwrap();
    assert_eq!(china.region(), StreamRegion::Global);

    for token in ["eu-\nsecret", "cn-\0secret"] {
        assert_eq!(
            LegacyAuth::from_access_token(token, "refresh").unwrap_err(),
            LegacyAuthError::InvalidAccessToken
        );
    }

    let fallback = LegacyAuth::from_access_token("legacy-opaque-token", "refresh").unwrap();
    assert_eq!(fallback.region(), StreamRegion::Global);
    assert_eq!(
        fallback.issuer.as_str(),
        "https://auth.tesla.com/oauth2/v3/"
    );
}

#[derive(Clone, Default)]
struct MockState {
    bodies: Arc<Mutex<Vec<String>>>,
    request_headers: Arc<Mutex<Vec<(String, String, String)>>>,
    token_response: Arc<Mutex<(StatusCode, String)>>,
    unauthorized_count: Arc<Mutex<usize>>,
    token_request_count: Arc<AtomicUsize>,
    first_token_request_started: Arc<Notify>,
    block_first_token_request: Option<Arc<Notify>>,
    token_redirects_remaining: Arc<Mutex<usize>>,
    token_redirect_location: Arc<Mutex<String>>,
    redirect_capture_requests: Arc<AtomicUsize>,
    redirect_capture_body_bytes: Arc<AtomicUsize>,
    redirect_capture_authorization: Arc<AtomicUsize>,
}

async fn token_handler(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let attempt = state.token_request_count.fetch_add(1, Ordering::SeqCst);
    state.bodies.lock().unwrap().push(body);
    state.request_headers.lock().unwrap().push((
        headers
            .get(USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        headers
            .get(ACCEPT)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        headers
            .get(ACCEPT_LANGUAGE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    ));
    if attempt == 0
        && let Some(blocker) = &state.block_first_token_request
    {
        state.first_token_request_started.notify_one();
        blocker.notified().await;
    }
    let redirect = {
        let mut remaining = state.token_redirects_remaining.lock().unwrap();
        if *remaining == 0 {
            None
        } else {
            *remaining -= 1;
            Some(state.token_redirect_location.lock().unwrap().clone())
        }
    };
    if let Some(location) = redirect {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(axum::http::header::LOCATION, location)],
            "redirect",
        )
            .into_response();
    }
    state.token_response.lock().unwrap().clone().into_response()
}

async fn redirect_capture_handler(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    state
        .redirect_capture_requests
        .fetch_add(1, Ordering::SeqCst);
    if !body.is_empty() {
        state
            .redirect_capture_body_bytes
            .fetch_add(body.len(), Ordering::SeqCst);
    }
    if headers.get("authorization").is_some() {
        state
            .redirect_capture_authorization
            .fetch_add(1, Ordering::SeqCst);
    }
    (StatusCode::INTERNAL_SERVER_ERROR, "redirect capture")
}

async fn products_handler(State(state): State<MockState>) -> impl IntoResponse {
    let mut count = state.unauthorized_count.lock().unwrap();
    if *count > 0 {
        *count -= 1;
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    (StatusCode::OK, r#"{"response":[],"count":0}"#).into_response()
}

async fn mock_server(state: MockState) -> (Url, tokio::task::JoinHandle<()>) {
    crate::crypto::install_default_provider();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/oauth2/v3/token", post(token_handler))
                .route(
                    "/oauth2/v3/redirect-capture",
                    post(redirect_capture_handler),
                )
                .route("/api/1/products", get(products_handler))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    (
        Url::parse(&format!("http://{address}/oauth2/v3/")).unwrap(),
        task,
    )
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

fn valid_response(access_token: &str, refresh_token: &str) -> String {
    serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": "Bearer",
        "expires_in": 1000,
        "created_at": 1_700_000_000,
    })
    .to_string()
}

#[tokio::test]
async fn posts_exact_teslamate_json_and_schedules_at_seventy_five_percent() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            valid_response("new-access", "new-refresh"),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let mut auth = crate::credentials::LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
    auth.refresh_now(
        &Client::new(),
        UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    )
    .await
    .unwrap();
    assert_eq!(auth.access_token(), "new-access");
    assert_eq!(auth.refresh_token(), "new-refresh");
    assert_eq!(auth.next_refresh_at(), 1_700_000_750);
    let bodies = state.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&bodies[0]).unwrap(),
        serde_json::json!({
            "grant_type": "refresh_token",
            "scope": "openid email offline_access",
            "client_id": "ownerapi",
            "refresh_token": "old-refresh"
        })
    );
    assert_eq!(
        state.request_headers.lock().unwrap().as_slice(),
        &[(
            TESLAMATE_USER_AGENT.to_owned(),
            TESLAMATE_ACCEPT.to_owned(),
            TESLAMATE_ACCEPT_LANGUAGE.to_owned(),
        )]
    );
}

#[tokio::test]
async fn persistence_failure_keeps_rotated_pair_and_retries_without_second_refresh() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            valid_response("new-access", "new-refresh"),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let mut auth = LegacyAuth::for_test(issuer.clone(), "old-access", "old-refresh");
    let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    let error = auth
        .refresh_now_persisted(&Client::new(), refresh_epoch, |_, _, _, _| {
            Err(LegacyAuthError::Persistence)
        })
        .await
        .unwrap_err();
    assert_eq!(error, LegacyAuthError::Persistence);
    assert_eq!(auth.access_token(), "new-access");
    assert_eq!(auth.refresh_token(), "new-refresh");
    assert_eq!(auth.retry_at(), Some(1_700_000_300));
    assert_eq!(state.bodies.lock().unwrap().len(), 1);

    // The convenience API has no durable sink and must not be allowed to
    // erase the pending-persistence state.
    assert_eq!(
        auth.refresh_now(&Client::new(), refresh_epoch + REFRESH_RETRY_DELAY,)
            .await
            .unwrap_err(),
        LegacyAuthError::Persistence
    );

    let persisted = Arc::new(Mutex::new(Vec::new()));
    let persisted_for_sink = Arc::clone(&persisted);
    auth.refresh_now_persisted(
        &Client::new(),
        refresh_epoch + REFRESH_RETRY_DELAY,
        move |access, refresh, expires_at, next_refresh_at| {
            persisted_for_sink.lock().unwrap().push((
                access.to_owned(),
                refresh.to_owned(),
                expires_at,
                next_refresh_at,
            ));
            Ok(())
        },
    )
    .await
    .unwrap();

    assert_eq!(state.bodies.lock().unwrap().len(), 1);
    assert_eq!(auth.retry_at(), None);
    assert!(
        !auth
            .refresh_due(refresh_epoch + REFRESH_RETRY_DELAY)
            .unwrap()
    );
    assert_eq!(
        persisted.lock().unwrap().as_slice(),
        &[(
            "new-access".to_owned(),
            "new-refresh".to_owned(),
            1_700_001_000,
            1_700_000_750,
        )]
    );
}

#[tokio::test]
async fn persistence_sink_error_is_preserved_on_initial_attempt_and_retry() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            valid_response("new-access", "new-refresh"),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    for now in [refresh_epoch, refresh_epoch + REFRESH_RETRY_DELAY] {
        assert_eq!(
            auth.refresh_now_persisted(&Client::new(), now, |_, _, _, _| {
                Err(LegacyAuthError::SensitivePersistenceUnavailable)
            })
            .await
            .expect_err("sink error must remain typed"),
            LegacyAuthError::SensitivePersistenceUnavailable
        );
    }
    assert_eq!(state.bodies.lock().unwrap().len(), 1);
    assert_eq!(auth.access_token(), "new-access");
    assert_eq!(auth.refresh_token(), "new-refresh");
}

#[tokio::test]
async fn credential_manager_retries_failed_sink_with_rotated_pair_only() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            valid_response("new-access", "new-refresh"),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let attempts_for_sink = Arc::clone(&attempts);
    let auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let mut manager = crate::credentials::LegacyAuthManager::for_test(
        auth,
        Arc::new(move |access, refresh| {
            let mut attempts = attempts_for_sink.lock().unwrap();
            attempts.push((access.to_owned(), refresh.to_owned()));
            if attempts.len() == 1 {
                Err(crate::credentials::CredentialError::LegacyTokenStateWrite)
            } else {
                Ok(())
            }
        }),
    );
    let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    assert!(matches!(
        manager.refresh_now(&Client::new(), refresh_epoch).await,
        Err(crate::credentials::LegacyAuthManagerError::Auth(
            LegacyAuthError::Persistence
        ))
    ));
    manager
        .refresh_if_due(&Client::new(), refresh_epoch + REFRESH_RETRY_DELAY)
        .await
        .unwrap();

    assert_eq!(state.bodies.lock().unwrap().len(), 1);
    assert_eq!(manager.access_token(), "new-access");
    assert_eq!(manager.refresh_token(), "new-refresh");
    assert_eq!(
        attempts.lock().unwrap().as_slice(),
        &[
            ("new-access".to_owned(), "new-refresh".to_owned()),
            ("new-access".to_owned(), "new-refresh".to_owned()),
        ]
    );
}

#[tokio::test]
async fn missing_created_at_uses_validated_local_refresh_epoch() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "token_type": "bEaReR",
                "expires_in": 1000,
            })
            .to_string(),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    auth.refresh_now(
        &Client::new(),
        UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    )
    .await
    .unwrap();
    assert_eq!(auth.expires_at(), 1_700_001_000);
    assert_eq!(auth.next_refresh_at(), 1_700_000_750);
}

#[tokio::test]
async fn provider_created_at_does_not_move_receipt_based_schedule() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "token_type": "Bearer",
                "expires_in": 1000,
                "created_at": 1,
            })
            .to_string(),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    auth.refresh_now(
        &Client::new(),
        UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    )
    .await
    .unwrap();
    assert_eq!(auth.expires_at(), 1_700_001_000);
    assert_eq!(auth.next_refresh_at(), 1_700_000_750);
}

#[test]
fn refresh_delay_matches_teslamate_rounding_at_quarter_boundaries() {
    assert_eq!(teslamate_refresh_delay_seconds(1).unwrap(), 1);
    assert_eq!(teslamate_refresh_delay_seconds(2).unwrap(), 2);
    assert_eq!(teslamate_refresh_delay_seconds(3).unwrap(), 2);
    assert_eq!(teslamate_refresh_delay_seconds(4).unwrap(), 3);
}

#[tokio::test]
async fn persisted_startup_refresh_failure_uses_450_seconds_then_300() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::INTERNAL_SERVER_ERROR,
            "retry later".to_owned(),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    auth.expires_at = 1_800_000_000;
    auth.next_refresh_at = 1_750_000_000;
    auth.startup_refresh_pending = true;
    let startup = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    assert_eq!(
        auth.refresh_if_due(&Client::new(), startup)
            .await
            .unwrap_err(),
        LegacyAuthError::HttpStatus(500)
    );
    assert_eq!(auth.retry_at(), Some(1_700_000_450));
    assert_eq!(auth.access_token(), "old-access");
    assert_eq!(auth.refresh_token(), "old-refresh");

    assert_eq!(
        auth.refresh_if_due(&Client::new(), startup + STARTUP_REFRESH_RETRY_DELAY,)
            .await
            .unwrap_err(),
        LegacyAuthError::HttpStatus(500)
    );
    assert_eq!(auth.retry_at(), Some(1_700_000_750));
    assert_eq!(state.bodies.lock().unwrap().len(), 2);
}

#[test]
fn persisted_reload_honours_wire_schedule() {
    assert_eq!(
        LegacyAuth::from_persisted_state("old-access", "old-refresh", 100, 200).unwrap_err(),
        LegacyAuthError::InvalidPersistedSchedule
    );
    let auth =
        LegacyAuth::from_persisted_state("old-access", "old-refresh", 1_800_000_000, 1_750_000_000)
            .unwrap();
    assert_eq!(auth.expires_at(), 1_800_000_000);
    assert_eq!(auth.next_refresh_at(), 1_750_000_000);
    assert!(
        !auth
            .refresh_due(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
            .unwrap()
    );
    assert!(
        auth.refresh_due(UNIX_EPOCH + Duration::from_secs(1_750_000_000))
            .unwrap()
    );

    let imported = LegacyAuth::from_persisted_state("old-access", "old-refresh", 0, 0).unwrap();
    assert!(
        imported
            .refresh_due(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
            .unwrap()
    );
}

#[tokio::test]
async fn cancelled_refresh_does_not_permanently_fence_predecessor() {
    let blocker = Arc::new(Notify::new());
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            valid_response("new-access", "new-refresh"),
        ))),
        block_first_token_request: Some(Arc::clone(&blocker)),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    let client = Client::new();
    let mut first = Box::pin(auth.refresh_now(&client, refresh_epoch));
    tokio::select! {
        () = state.first_token_request_started.notified() => {}
        result = &mut first => panic!("first refresh unexpectedly completed: {result:?}"),
    }
    drop(first);

    auth.refresh_now(&client, refresh_epoch).await.unwrap();
    blocker.notify_one();
    assert_eq!(auth.access_token(), "new-access");
    assert_eq!(auth.refresh_token(), "new-refresh");
    assert_eq!(state.token_request_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn non_bearer_token_type_is_accepted_like_teslamate() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "token_type": "MAC",
                "expires_in": 1000,
                "created_at": 1_700_000_000,
            })
            .to_string(),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    auth.refresh_now(
        &Client::new(),
        UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    )
    .await
    .unwrap();
    assert_eq!(auth.access_token(), "new-access");
    assert_eq!(auth.refresh_token(), "new-refresh");
    assert_eq!(auth.token_type(), "MAC");
}

#[tokio::test]
async fn missing_token_type_rotates_persists_and_defaults_to_bearer() {
    let response = serde_json::json!({
        "access_token": "new-access",
        "refresh_token": "new-refresh",
        "expires_in": 1000,
        "created_at": 1_700_000_000,
    })
    .to_string();
    let state = MockState {
        token_response: Arc::new(Mutex::new((StatusCode::OK, response))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let persisted = Arc::new(Mutex::new(Vec::new()));
    let saved = Arc::clone(&persisted);
    auth.refresh_now_persisted(
        &Client::new(),
        refresh_epoch,
        move |access, refresh, _, _| {
            saved
                .lock()
                .unwrap()
                .push((access.to_owned(), refresh.to_owned()));
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(auth.access_token(), "new-access");
    assert_eq!(auth.refresh_token(), "new-refresh");
    assert_eq!(auth.token_type(), "Bearer");
    assert_eq!(auth.next_refresh_at(), 1_700_000_750);
    assert_eq!(
        persisted.lock().unwrap().as_slice(),
        &[("new-access".to_owned(), "new-refresh".to_owned())]
    );
    assert_eq!(auth.retry_at(), None);
    assert_eq!(state.bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn null_token_type_rotates_persists_and_defaults_to_bearer() {
    let response = serde_json::json!({
        "access_token": "new-access",
        "refresh_token": "new-refresh",
        "token_type": null,
        "expires_in": 1000,
    })
    .to_string();
    let state = MockState {
        token_response: Arc::new(Mutex::new((StatusCode::OK, response))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    auth.refresh_now_persisted(&Client::new(), refresh_epoch, |_, _, _, _| Ok(()))
        .await
        .unwrap();
    assert_eq!(auth.access_token(), "new-access");
    assert_eq!(auth.refresh_token(), "new-refresh");
    assert_eq!(auth.token_type(), "Bearer");
    assert_eq!(auth.next_refresh_at(), 1_700_000_750);
    assert_eq!(auth.retry_at(), None);
    assert_eq!(state.bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn malformed_success_response_retains_pair_and_retries() {
    for body in ["<html>not a token</html>", "not-json"] {
        let state = MockState {
            token_response: Arc::new(Mutex::new((StatusCode::OK, body.to_owned()))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(
            auth.refresh_now(&Client::new(), refresh_epoch)
                .await
                .unwrap_err(),
            LegacyAuthError::InvalidResponse
        );
        assert_eq!(auth.access_token(), "old-access");
        assert_eq!(auth.refresh_token(), "old-refresh");
        assert_eq!(
            auth.refresh_if_due(&Client::new(), refresh_epoch + Duration::from_secs(1))
                .await
                .unwrap_err(),
            LegacyAuthError::RefreshDeferred
        );
        assert_eq!(state.bodies.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn accepts_valid_split_chunked_token_response() {
    let body = valid_response("new-access", "new-refresh");
    let split_at = body.len() / 2;
    let (issuer, _task) = raw_chunked_token_server(
        vec![
            body.as_bytes()[..split_at].to_vec(),
            body.as_bytes()[split_at..].to_vec(),
        ],
        true,
    )
    .await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");

    auth.refresh_now(
        &Client::new(),
        UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    )
    .await
    .unwrap();

    assert_eq!(auth.access_token(), "new-access");
    assert_eq!(auth.refresh_token(), "new-refresh");
}

#[tokio::test]
async fn chunked_token_response_over_cap_fails_before_eof_and_preserves_pair() {
    let (issuer, _task) = raw_chunked_token_server(
        vec![vec![b'x'; MAX_TOKEN_RESPONSE_BYTES], vec![b'y']],
        false,
    )
    .await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let receipt = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    let error = timeout(
        Duration::from_secs(1),
        auth.refresh_now(&Client::new(), receipt),
    )
    .await
    .expect("response cap must finish before the chunked response EOF")
    .unwrap_err();

    assert_eq!(error, LegacyAuthError::ResponseTooLarge);
    assert_eq!(auth.access_token(), "old-access");
    assert_eq!(auth.refresh_token(), "old-refresh");
    assert_eq!(auth.retry_at(), Some(1_700_000_300));
}

#[tokio::test]
async fn invalid_response_retains_pair_for_retry_without_redaction_leak() {
    let secret = "old-refresh-secret";
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            r#"{"access_token":"new-access","token_type":"Bearer","expires_in":1000,"created_at":1700000000}"#.to_owned(),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", secret);
    let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let error = auth
        .refresh_now(&Client::new(), refresh_epoch)
        .await
        .unwrap_err();
    assert_eq!(error, LegacyAuthError::InvalidResponse);
    assert_eq!(auth.refresh_token(), secret);
    assert_eq!(auth.retry_at(), Some(1_700_000_300));
    assert!(!format!("{auth:?}").contains(secret));
    assert!(!error.to_string().contains(secret));
    assert_eq!(
        auth.refresh_now(&Client::new(), refresh_epoch + Duration::from_secs(1))
            .await
            .unwrap_err(),
        LegacyAuthError::RefreshDeferred
    );
    assert_eq!(state.bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn non_200_response_retains_pair_and_retries() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::INTERNAL_SERVER_ERROR,
            "retry later".to_owned(),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    assert_eq!(
        auth.refresh_now(&Client::new(), refresh_epoch)
            .await
            .unwrap_err(),
        LegacyAuthError::HttpStatus(500)
    );
    assert_eq!(auth.access_token(), "old-access");
    assert_eq!(auth.refresh_token(), "old-refresh");
    assert_eq!(auth.retry_at(), Some(1_700_000_300));
    assert_eq!(
        auth.refresh_now(&Client::new(), refresh_epoch + Duration::from_secs(1))
            .await
            .unwrap_err(),
        LegacyAuthError::RefreshDeferred
    );
    assert_eq!(state.bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn token_endpoint_requires_status_exactly_200() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::CREATED,
            valid_response("new-access", "new-refresh"),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state).await;
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let receipt = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(
        auth.refresh_now(&Client::new(), receipt).await.unwrap_err(),
        LegacyAuthError::HttpStatus(201)
    );
    assert_eq!(auth.access_token(), "old-access");
    assert_eq!(auth.refresh_token(), "old-refresh");
    assert_eq!(auth.retry_at(), Some(1_700_000_300));
}

#[tokio::test]
async fn bound_audit_force_bypasses_retry_twice_but_nonforce_remains_deferred() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::SERVICE_UNAVAILABLE,
            "refresh failed".to_owned(),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let client = Client::new();
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let first = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(
        auth.refresh_persisted_with_bound_audit(&client, first, true, |_, _, _, _| Ok(()))
            .await
            .unwrap_err(),
        LegacyAuthError::HttpStatus(503)
    );
    assert_eq!(auth.retry_at(), Some(1_700_000_300));
    let second = first + Duration::from_secs(1);
    assert_eq!(
        auth.refresh_persisted_with_bound_audit(&client, second, true, |_, _, _, _| Ok(()))
            .await
            .unwrap_err(),
        LegacyAuthError::HttpStatus(503)
    );
    assert_eq!(auth.retry_at(), Some(1_700_000_301));
    assert_eq!(state.token_request_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        auth.refresh_persisted_with_bound_audit(
            &client,
            second + Duration::from_secs(1),
            false,
            |_, _, _, _| Ok(())
        )
        .await
        .unwrap_err(),
        LegacyAuthError::RefreshDeferred
    );
    assert_eq!(state.token_request_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_callback_force_retries_immediately_after_failure() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::SERVICE_UNAVAILABLE,
            "refresh failed".to_owned(),
        ))),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    let mut manager =
        crate::credentials::LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
    let first = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert!(matches!(
        manager.refresh_now(&Client::new(), first).await,
        Err(crate::credentials::LegacyAuthManagerError::Auth(
            LegacyAuthError::HttpStatus(503)
        ))
    ));
    assert!(matches!(
        manager
            .refresh_now(&Client::new(), first + Duration::from_secs(1))
            .await,
        Err(crate::credentials::LegacyAuthManagerError::Auth(
            LegacyAuthError::HttpStatus(503)
        ))
    ));
    assert_eq!(state.token_request_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn auth_client_rejects_redirect_without_replaying_refresh_body() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            valid_response("new-access", "new-refresh"),
        ))),
        token_redirects_remaining: Arc::new(Mutex::new(1)),
        token_redirect_location: Arc::new(Mutex::new("/oauth2/v3/redirect-capture".to_owned())),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let owner =
        OwnerApi::for_fake_http(issuer.join("../../").unwrap(), Duration::from_millis(50)).unwrap();
    let mut auth = LegacyAuth::for_test(issuer.clone(), "old-access", "old-refresh");
    assert_eq!(
        auth.refresh_now(
            &owner.legacy_auth_http_client(),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        )
        .await
        .unwrap_err(),
        LegacyAuthError::HttpStatus(307)
    );
    assert_eq!(state.token_request_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        state.redirect_capture_requests.load(Ordering::SeqCst),
        0,
        "redirect target must receive no request"
    );
    assert_eq!(
        state.redirect_capture_body_bytes.load(Ordering::SeqCst),
        0,
        "refresh body must not reach redirect target"
    );
    assert_eq!(
        state.redirect_capture_authorization.load(Ordering::SeqCst),
        0,
        "credential headers must not reach redirect target"
    );
}

#[tokio::test]
async fn owner_api_401_is_one_wrapped_request_without_sync_refresh_or_retry() {
    let state = MockState {
        token_response: Arc::new(Mutex::new((
            StatusCode::OK,
            valid_response("new-access", "new-refresh"),
        ))),
        unauthorized_count: Arc::new(Mutex::new(1)),
        ..MockState::default()
    };
    let (issuer, _task) = mock_server(state.clone()).await;
    let client =
        OwnerApi::for_fake_http(issuer.join("../../").unwrap(), Duration::from_secs(2)).unwrap();
    let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
    auth.expires_at = 2_000_000_000;
    auth.next_refresh_at = 1_900_000_000;
    let mut auth = crate::credentials::LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
    let error = client
        .list_vehicles_with_legacy_auth(&mut auth)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401))
    ));
    assert_eq!(*state.unauthorized_count.lock().unwrap(), 0);
    assert_eq!(state.token_request_count.load(Ordering::SeqCst), 0);
}

#[test]
fn account_unauthorized_fuse_is_shared_windowed_and_resettable() {
    let base = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut fuse = LegacyAuthFuse::default();
    for offset in 0..5 {
        fuse.record_unauthorized(base + Duration::from_secs(offset));
        assert!(!fuse.is_blown());
    }
    fuse.record_unauthorized(base + Duration::from_secs(9 * 60));
    assert!(fuse.is_blown());

    fuse.reset();
    assert!(!fuse.is_blown());
    fuse.record_unauthorized(base + Duration::from_secs(20 * 60));
    assert!(!fuse.is_blown());
    fuse.record_unauthorized(base + Duration::from_secs(20 * 60 + 601));
    assert!(!fuse.is_blown());
}
