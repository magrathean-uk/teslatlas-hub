// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use axum::http::Request;
use tower::ServiceExt;

fn app() -> (tempfile::TempDir, Router) {
    let root = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(root.path().join("data")).unwrap();
    let key = CursorKey::from_bytes([41; 32]);
    let app = router_with_access_telemetry_and_http(
        store,
        false,
        true,
        true,
        Some(ManifestSigning::from_cursor_key(&key)),
        Some(key),
        None,
        None,
        CorsPolicy {
            http: crate::config::HttpConfig {
                allowed_origins: vec!["https://viewer.example".into()],
            },
            same_origin: None,
        },
    );
    (root, app)
}

#[tokio::test]
async fn cors_public_preflight_is_explicit_and_auth_failures_remain_readable() {
    let (_root, app) = app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/vehicles")
                .header("Origin", "https://viewer.example")
                .header("Access-Control-Request-Method", "GET")
                .header(
                    "Access-Control-Request-Headers",
                    "authorization, if-none-match",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "https://viewer.example"
    );
    assert_eq!(response.headers()["access-control-allow-methods"], "GET");
    assert!(
        response
            .headers()
            .get("access-control-allow-credentials")
            .is_none()
    );
    assert!(
        response.headers()["vary"]
            .to_str()
            .unwrap()
            .contains("Origin")
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/vehicles")
                .header("Origin", "https://viewer.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "https://viewer.example"
    );
    assert!(
        response.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap()
            .contains("ETag")
    );
}

#[tokio::test]
async fn cors_rejects_unlisted_origins_methods_and_headers_before_handlers() {
    let (_root, app) = app();
    for (origin, method, headers) in [
        ("https://evil.example", "GET", "authorization"),
        ("null", "GET", "authorization"),
        ("https://viewer.example", "DELETE", "authorization"),
        ("https://viewer.example", "POST", "authorization"),
        ("https://viewer.example", "GET", "cookie"),
        (
            "https://viewer.example",
            "GET",
            "authorization,,content-type",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/v1/vehicles")
                    .header("Origin", origin)
                    .header("Access-Control-Request-Method", method)
                    .header("Access-Control-Request-Headers", headers)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{origin}, {method}, {headers}"
        );
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/pairings/11111111-1111-4111-8111-111111111111/claim")
                .header("Origin", "https://evil.example")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"secret":"synthetic","device_name":"test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cors_internal_ingress_is_never_exposed_and_native_requests_still_work() {
    let (_root, app) = app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/internal/fleet-telemetry")
                .header("Origin", "https://viewer.example")
                .header("Access-Control-Request-Method", "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/.well-known/teslatlas-hub")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    assert!(
        response.headers()["vary"]
            .to_str()
            .unwrap()
            .contains("Origin")
    );
}

#[tokio::test]
async fn cors_default_denies_cross_origin_and_duplicate_origin_headers() {
    let root = crate::private_tempdir().unwrap();
    let closed = router(HubStore::initialize(root.path().join("data")).unwrap());
    let response = closed
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("Origin", "https://viewer.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let (_root, app) = app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("Origin", "https://viewer.example")
                .header("Origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cors_pairing_and_sync_headers_are_scoped_to_their_routes() {
    let (_root, app) = app();
    for (route, method, headers, expected) in [
        (
            "/v1/device/rotate",
            "POST",
            "authorization, content-type",
            StatusCode::NO_CONTENT,
        ),
        (
            "/v1/pairings/test/claim",
            "POST",
            "content-type",
            StatusCode::NO_CONTENT,
        ),
        (
            "/v1/packs/sha256/test",
            "GET",
            "range, if-range",
            StatusCode::NO_CONTENT,
        ),
        (
            "/v1/vehicles/test/sync/manifest",
            "GET",
            "x-teslatlas-supported-schemas, x-teslatlas-sync-capability",
            StatusCode::NO_CONTENT,
        ),
        (
            "/v1/vehicles/test/sync/noop",
            "GET",
            "x-teslatlas-supported-schemas, x-teslatlas-sync-capability",
            StatusCode::NO_CONTENT,
        ),
        (
            "/v1/vehicles/test/sync/manifest",
            "GET",
            "range",
            StatusCode::FORBIDDEN,
        ),
        (
            "/v1/vehicles/test/sync/manifest",
            "GET",
            "if-range",
            StatusCode::FORBIDDEN,
        ),
        (
            "/v1/vehicles/test/sync/noop",
            "GET",
            "range",
            StatusCode::FORBIDDEN,
        ),
        (
            "/v1/vehicles/test/sync/noop",
            "GET",
            "if-range",
            StatusCode::FORBIDDEN,
        ),
        ("/v1/vehicles", "GET", "range", StatusCode::FORBIDDEN),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri(route)
                    .header("Origin", "https://viewer.example")
                    .header("Access-Control-Request-Method", method)
                    .header("Access-Control-Request-Headers", headers)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{route}");
    }
}

#[tokio::test]
async fn cors_decorates_public_timeouts_without_exposing_internal_timeouts() {
    async fn delayed() -> StatusCode {
        tokio::time::sleep(Duration::from_millis(100)).await;
        StatusCode::OK
    }

    let cors = CorsPolicy {
        http: crate::config::HttpConfig {
            allowed_origins: vec!["https://viewer.example".into()],
        },
        same_origin: None,
    };
    let public =
        Router::new()
            .route("/healthz", get(delayed))
            .layer(axum::middleware::from_fn_with_state(
                cors.clone(),
                browser_cors::apply,
            ));
    let internal = Router::new().route("/v1/internal/fleet-telemetry", post(delayed));
    let app = apply_http_resource_limits_with_cors(
        public.merge(internal),
        1,
        Duration::from_millis(10),
        cors,
    );

    let public_timeout = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("Origin", "https://viewer.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(public_timeout.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        public_timeout.headers()["access-control-allow-origin"],
        "https://viewer.example"
    );
    assert!(
        public_timeout.headers()["vary"]
            .to_str()
            .unwrap()
            .split(',')
            .any(|value| value.trim() == "Origin")
    );

    let internal_timeout = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/internal/fleet-telemetry")
                .header("Origin", "https://viewer.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(internal_timeout.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        internal_timeout
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    assert!(internal_timeout.headers().get("vary").is_none());
}

#[tokio::test]
async fn cors_native_tls_listener_uses_configuration_and_accepts_its_own_origin() {
    use std::os::unix::fs::PermissionsExt;
    crate::crypto::install_default_provider();
    let root = crate::private_tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bind = listener.local_addr().unwrap();
    drop(listener);
    let identity = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let cert = root.path().join("cert.pem");
    let key = root.path().join("key.pem");
    std::fs::write(&cert, identity.cert.pem()).unwrap();
    std::fs::write(&key, identity.signing_key.serialize_pem()).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let text = format!(
        "data_dir = '{}'\nbind = '{bind}'\n[tls]\ncertificate_path = '{}'\nprivate_key_path = '{}'\npublic_url = 'https://{bind}/'\n[collector]\ninterval_seconds = 0\n[http]\nallowed_origins = ['https://viewer.example']\n",
        root.path().join("data").display(),
        cert.display(),
        key.display()
    );
    let (config, digest) = HubConfig::from_exact_bytes(text.as_bytes()).unwrap();
    let store = HubStore::initialize(&config.data_dir).unwrap();
    let admission = crate::hub_user_process::AdmittedUserHub::for_test(&config.data_dir).unwrap();
    let task = tokio::spawn(async move {
        serve_with_cursor_key(
            store,
            &config,
            digest,
            Some(CursorKey::from_bytes([11; 32])),
            Some(admission),
            std::future::pending(),
        )
        .await
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(
            reqwest::Certificate::from_pem(identity.cert.pem().as_bytes()).unwrap(),
        )
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let url = format!("https://{bind}/healthz");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            assert!(!task.is_finished(), "native server exited before readiness");
            if client.get(&url).send().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let response = client
        .request(reqwest::Method::OPTIONS, &url)
        .header("Origin", "https://viewer.example")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "https://viewer.example"
    );
    let response = client
        .get(&url)
        .header("Origin", format!("https://{bind}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = client
        .get(&url)
        .header("Origin", "https://unlisted.example")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
