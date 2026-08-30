// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Clone)]
struct LegacyRuntimeMock {
    unauthorized: Arc<AtomicUsize>,
    token_calls: Arc<AtomicUsize>,
    authorization: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct Coordinated401Mock {
    owner_calls: Arc<AtomicUsize>,
    token_calls: Arc<AtomicUsize>,
    owner_pairs: Arc<Mutex<Vec<String>>>,
    refresh_pairs: Arc<Mutex<Vec<String>>>,
    token_entered: Arc<tokio::sync::Notify>,
    token_release: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
struct ScriptedUnauthorizedMock {
    owner_calls: Arc<AtomicUsize>,
    token_calls: Arc<AtomicUsize>,
    owner_pairs: Arc<Mutex<Vec<String>>>,
    refresh_pairs: Arc<Mutex<Vec<String>>>,
    owner_statuses: Arc<Mutex<Vec<u16>>>,
    refresh_statuses: Arc<Mutex<Vec<u16>>>,
    owner_script: Arc<Mutex<Vec<u16>>>,
    refresh_script: Arc<Mutex<Vec<u16>>>,
}

fn label_owner_authorization(headers: &HeaderMap) -> &'static str {
    match headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
    {
        Some("Bearer old-access-secret") => "old_pair",
        _ => "unknown",
    }
}

fn label_refresh_body(body: &str) -> &'static str {
    if body.contains("old-refresh-secret") {
        "old_pair"
    } else {
        "unknown"
    }
}

async fn scripted_unauthorized_products(
    State(state): State<ScriptedUnauthorizedMock>,
    headers: HeaderMap,
) -> impl IntoResponse {
    state.owner_calls.fetch_add(1, Ordering::SeqCst);
    state
        .owner_pairs
        .lock()
        .unwrap()
        .push(label_owner_authorization(&headers).to_owned());
    let status = state.owner_script.lock().unwrap().remove(0);
    state.owner_statuses.lock().unwrap().push(status);
    (
        StatusCode::from_u16(status).unwrap(),
        "scripted owner response",
    )
}

async fn scripted_unauthorized_token(
    State(state): State<ScriptedUnauthorizedMock>,
    body: String,
) -> impl IntoResponse {
    state.token_calls.fetch_add(1, Ordering::SeqCst);
    state
        .refresh_pairs
        .lock()
        .unwrap()
        .push(label_refresh_body(&body).to_owned());
    let status = state.refresh_script.lock().unwrap().remove(0);
    state.refresh_statuses.lock().unwrap().push(status);
    (
        StatusCode::from_u16(status).unwrap(),
        "scripted token response",
    )
}

async fn coordinated_401_products(
    State(state): State<Coordinated401Mock>,
    headers: HeaderMap,
) -> impl IntoResponse {
    state.owner_calls.fetch_add(1, Ordering::SeqCst);
    let pair = match headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
    {
        Some("Bearer old-access-secret") => "old_pair",
        _ => "unknown",
    };
    state.owner_pairs.lock().unwrap().push(pair.to_owned());
    (StatusCode::UNAUTHORIZED, "unauthorized")
}

async fn coordinated_blocked_token(State(state): State<Coordinated401Mock>) -> impl IntoResponse {
    state.token_calls.fetch_add(1, Ordering::SeqCst);
    let release = state.token_release.notified();
    tokio::pin!(release);
    release.as_mut().enable();
    state.token_entered.notify_waiters();
    release.await;
    (
        StatusCode::OK,
        json!({
            "access_token": "rotated-access",
            "refresh_token": "rotated-refresh",
            "token_type": "Bearer",
            "expires_in": 1_000_000_000u64,
            "created_at": 1_800_000_000i64,
        })
        .to_string(),
    )
}

async fn coordinated_failed_token(
    State(state): State<Coordinated401Mock>,
    body: String,
) -> impl IntoResponse {
    state.token_calls.fetch_add(1, Ordering::SeqCst);
    let pair = if body.contains("old-refresh-secret") {
        "old_pair"
    } else {
        "unknown"
    };
    state.refresh_pairs.lock().unwrap().push(pair.to_owned());
    (StatusCode::SERVICE_UNAVAILABLE, "refresh failed")
}

async fn coordinated_success_token(State(state): State<Coordinated401Mock>) -> impl IntoResponse {
    state.token_calls.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        json!({
            "access_token": "rotated-access",
            "refresh_token": "rotated-refresh",
            "token_type": "Bearer",
            "expires_in": 1_000_000_000u64,
            "created_at": 1_800_000_000i64,
        })
        .to_string(),
    )
}

fn coordinated_legacy_auth(
    issuer: url::Url,
    persisted: Arc<Mutex<(String, String)>>,
) -> CollectionAuth {
    let persisted_for_callback = Arc::clone(&persisted);
    let auth =
        crate::legacy_auth::LegacyAuth::for_test(issuer, "old-access-secret", "old-refresh-secret")
            .with_test_schedule(2_000_000_000, 1_900_000_000);
    CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
            auth,
            Arc::new(move |access, refresh| {
                *persisted_for_callback.lock().expect("durable pair lock") =
                    (access.to_owned(), refresh.to_owned());
                Ok(())
            }),
        ))),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    }
}

fn coordinated_test_legacy_auth(issuer: url::Url) -> CollectionAuth {
    let auth =
        crate::legacy_auth::LegacyAuth::for_test(issuer, "old-access-secret", "old-refresh-secret")
            .with_test_schedule(2_000_000_000, 1_900_000_000);
    let manager = LegacyAuthManager::for_test_with_active_pair(auth).expect("active test pair");
    CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    }
}

pub(crate) async fn test_unauthorized_six_restart_facade(
    owner_script: &[u16],
    refresh_script: &[u16],
) -> Result<LegacyUnauthorizedFacadeObservation, String> {
    if owner_script.len() != 6 || refresh_script.len() != 6 {
        return Err("unauthorized fixture requires six owner and token statuses".to_owned());
    }
    if owner_script
        .iter()
        .any(|status| StatusCode::from_u16(*status).is_err())
        || refresh_script
            .iter()
            .any(|status| StatusCode::from_u16(*status).is_err())
    {
        return Err("unauthorized fixture contains invalid HTTP status".to_owned());
    }
    let state = ScriptedUnauthorizedMock {
        owner_calls: Arc::new(AtomicUsize::new(0)),
        token_calls: Arc::new(AtomicUsize::new(0)),
        owner_pairs: Arc::new(Mutex::new(Vec::new())),
        refresh_pairs: Arc::new(Mutex::new(Vec::new())),
        owner_statuses: Arc::new(Mutex::new(Vec::new())),
        refresh_statuses: Arc::new(Mutex::new(Vec::new())),
        owner_script: Arc::new(Mutex::new(owner_script.to_vec())),
        refresh_script: Arc::new(Mutex::new(refresh_script.to_vec())),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = url::Url::parse(&format!("http://{address}/")).unwrap();
    let issuer = base.join("oauth2/v3/").unwrap();
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/api/1/products", get(scripted_unauthorized_products))
                .route("/oauth2/v3/token", post(scripted_unauthorized_token))
                .with_state(server_state),
        )
        .await
        .unwrap();
    });
    let auth = coordinated_test_legacy_auth(issuer);
    let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();
    for _ in 0..5 {
        let error = match list_vehicles_for_auth(&client, &auth).await {
            Err(error) => error,
            Ok(_) => {
                shutdown_legacy_refresh(&auth).await;
                server.abort();
                return Err("scripted owner request unexpectedly succeeded".to_owned());
            }
        };
        if !is_wrapped_legacy_unauthorized(&error) {
            shutdown_legacy_refresh(&auth).await;
            server.abort();
            return Err(format!(
                "scripted owner status did not yield wrapped 401: {error}"
            ));
        }
    }
    // The production coordinator runs refreshes asynchronously.  Drain the
    // fifth actual forced refresh before the sixth 401 melts the fuse.
    if let Err(error) = wait_for_legacy_refresh_before_owner(&auth).await {
        shutdown_legacy_refresh(&auth).await;
        server.abort();
        return Err(format!(
            "scripted refresh unexpectedly became terminal: {error}"
        ));
    }
    let sixth = match list_vehicles_for_auth(&client, &auth).await {
        Err(error) => error,
        Ok(_) => {
            shutdown_legacy_refresh(&auth).await;
            server.abort();
            return Err("sixth scripted owner request unexpectedly succeeded".to_owned());
        }
    };
    if !matches!(
        sixth,
        CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
    ) {
        shutdown_legacy_refresh(&auth).await;
        server.abort();
        return Err("sixth owner request did not melt the production fuse".to_owned());
    }
    let CollectionAuth::Legacy { manager, fuse, .. } = &auth;
    let fuse_blown = fuse.try_lock().unwrap().is_blown();
    if !fuse_blown {
        shutdown_legacy_refresh(&auth).await;
        server.abort();
        return Err("production fuse was not blown".to_owned());
    }
    let manager = manager.lock().await;
    let durable_pair = if manager.access_token() == "old-access-secret" {
        "old_pair".to_owned()
    } else {
        "unknown".to_owned()
    };
    let durable_matches = manager
        .test_pair_matches("old-access-secret", "old-refresh-secret")
        .map_err(|error| format!("managed pair read failed: {error}"))?;
    if !durable_matches {
        shutdown_legacy_refresh(&auth).await;
        server.abort();
        return Err("managed pair changed predecessor pair".to_owned());
    }
    drop(manager);

    let tls_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("TLS bind failed: {error}"))?;
    let tls_address = tls_listener
        .local_addr()
        .map_err(|error| format!("TLS address failed: {error}"))?;
    crate::crypto::install_default_provider();
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["auth.tesla.com".to_owned()])
            .map_err(|error| format!("TLS certificate failed: {error}"))?;
    let certificate_pem = cert.pem();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        certificate_pem.as_bytes().to_vec(),
        signing_key.serialize_pem().into_bytes(),
    )
    .await
    .map_err(|error| format!("TLS config failed: {error}"))?;
    let tls_state = state.clone();
    let tls_server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(tls_listener.into_std().expect("std TLS listener"), tls)
            .expect("TLS server")
            .serve(
                Router::new()
                    .route("/oauth2/v3/token", post(scripted_unauthorized_token))
                    .with_state(tls_state)
                    .into_make_service(),
            )
            .await
            .expect("TLS serve");
    });
    let certificate = Certificate::from_pem(certificate_pem.as_bytes())
        .map_err(|error| format!("TLS root failed: {error}"))?;
    let startup_client = Client::builder()
        .https_only(true)
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(2))
        .add_root_certificate(certificate)
        .resolve("auth.tesla.com", tls_address)
        .build()
        .map_err(|error| format!("TLS client failed: {error}"))?;
    let mut restarted = crate::legacy_auth::LegacyAuth::from_persisted_state(
        "old-access-secret",
        "old-refresh-secret",
        0,
        0,
    )
    .unwrap();
    let restart_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let restart_result = restarted
        .refresh_if_due_persisted(&startup_client, restart_epoch, |_, _, _, _| Ok(()))
        .await;
    if restart_result != Err(crate::legacy_auth::LegacyAuthError::HttpStatus(400)) {
        shutdown_legacy_refresh(&auth).await;
        server.abort();
        tls_server.abort();
        return Err("imported startup did not use sixth scripted HTTP 400".to_owned());
    }
    let restart_retry_ms = restarted
        .retry_at()
        .map(|retry_at| (retry_at - 1_700_000_000) * 1_000)
        .ok_or_else(|| "imported startup did not schedule retry".to_owned())?;
    let pre_restart_retry_ms = i64::try_from(crate::legacy_auth::REFRESH_RETRY_DELAY.as_millis())
        .map_err(|_| "production retry delay exceeds i64".to_owned())?;
    shutdown_legacy_refresh(&auth).await;
    server.abort();
    tls_server.abort();
    let owner_requests = state.owner_calls.load(Ordering::SeqCst);
    let refresh_requests = state.token_calls.load(Ordering::SeqCst);
    let owner_pairs = state.owner_pairs.lock().unwrap().clone();
    let refresh_pairs = state.refresh_pairs.lock().unwrap().clone();
    let owner_statuses = state.owner_statuses.lock().unwrap().clone();
    let refresh_statuses = state.refresh_statuses.lock().unwrap().clone();
    if !state.owner_script.lock().unwrap().is_empty()
        || !state.refresh_script.lock().unwrap().is_empty()
    {
        return Err("scripted owner or token responses were not exhausted".to_owned());
    }
    Ok(LegacyUnauthorizedFacadeObservation {
        owner_retries: owner_requests.saturating_sub(owner_statuses.len()),
        owner_requests,
        refresh_requests,
        owner_pairs,
        refresh_pairs,
        owner_statuses,
        refresh_statuses,
        durable_pair,
        logical_resident_pair: if fuse_blown { "none" } else { "unknown" }.to_owned(),
        attempts_before_signout: refresh_requests.saturating_sub(1),
        fuse_melts: state.owner_calls.load(Ordering::SeqCst),
        fuse_blown,
        pre_restart_retry_ms,
        restart_retry_ms,
    })
}

#[tokio::test]
async fn unauthorized_facade_uses_scripted_responses_not_constants() {
    let result = test_unauthorized_six_restart_facade(&[401; 6], &[400; 6])
        .await
        .expect("fixture script must drive production facade");
    assert_eq!(result.owner_statuses, vec![401; 6]);
    assert_eq!(result.refresh_statuses, vec![400; 6]);
    assert_eq!(result.pre_restart_retry_ms, 300_000);
    assert_eq!(result.restart_retry_ms, 450_000);
    let result = test_unauthorized_six_restart_facade(&[500; 6], &[400; 6]).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn legacy_401_returns_before_blocked_refresh_and_next_owner_waits_for_ticket() {
    let state = Coordinated401Mock {
        owner_calls: Arc::new(AtomicUsize::new(0)),
        token_calls: Arc::new(AtomicUsize::new(0)),
        owner_pairs: Arc::new(Mutex::new(Vec::new())),
        refresh_pairs: Arc::new(Mutex::new(Vec::new())),
        token_entered: Arc::new(tokio::sync::Notify::new()),
        token_release: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = url::Url::parse(&format!("http://{address}/")).unwrap();
    let issuer = base.join("oauth2/v3/").unwrap();
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/api/1/products", get(coordinated_401_products))
                .route("/oauth2/v3/token", post(coordinated_blocked_token))
                .with_state(server_state),
        )
        .await
        .unwrap();
    });
    let durable = Arc::new(Mutex::new((
        "old-access".to_owned(),
        "old-refresh".to_owned(),
    )));
    let auth = coordinated_legacy_auth(issuer, durable);
    let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();

    let first = timeout(
        Duration::from_secs(1),
        list_vehicles_for_auth(&client, &auth),
    )
    .await
    .expect("first owner response must not wait for refresh");
    assert!(is_wrapped_legacy_unauthorized(first.as_ref().unwrap_err()));
    timeout(Duration::from_secs(1), async {
        while state.token_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("single refresh must start asynchronously");
    assert_eq!(state.owner_calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);

    let second = list_vehicles_for_auth(&client, &auth);
    tokio::pin!(second);
    assert!(
        timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err()
    );
    assert_eq!(state.owner_calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);
    state.token_release.notify_one();
    let second = timeout(Duration::from_secs(1), &mut second)
        .await
        .expect("second logical request must resume after ticket");
    assert!(is_wrapped_legacy_unauthorized(&second.unwrap_err()));
    assert_eq!(state.owner_calls.load(Ordering::SeqCst), 2);

    timeout(Duration::from_secs(1), async {
        while state.token_calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second accepted refresh starts");
    let shutdown = shutdown_legacy_refresh(&auth);
    tokio::pin!(shutdown);
    assert!(
        timeout(Duration::from_millis(50), &mut shutdown)
            .await
            .is_err(),
        "normal shutdown must await an accepted refresh"
    );
    state.token_release.notify_one();
    timeout(Duration::from_secs(1), &mut shutdown)
        .await
        .expect("normal shutdown drains accepted refresh");
    assert_eq!(state.token_calls.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn one_shot_legacy_drain_waits_for_queued_refresh_before_shutdown() {
    let state = Coordinated401Mock {
        owner_calls: Arc::new(AtomicUsize::new(0)),
        token_calls: Arc::new(AtomicUsize::new(0)),
        owner_pairs: Arc::new(Mutex::new(Vec::new())),
        refresh_pairs: Arc::new(Mutex::new(Vec::new())),
        token_entered: Arc::new(tokio::sync::Notify::new()),
        token_release: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = url::Url::parse(&format!("http://{address}/")).unwrap();
    let issuer = base.join("oauth2/v3/").unwrap();
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/api/1/products", get(coordinated_401_products))
                .route("/oauth2/v3/token", post(coordinated_blocked_token))
                .with_state(server_state),
        )
        .await
        .unwrap();
    });
    let durable = Arc::new(Mutex::new((
        "old-access".to_owned(),
        "old-refresh".to_owned(),
    )));
    let auth = coordinated_legacy_auth(issuer, durable);
    let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();
    let result = list_vehicles_for_auth(&client, &auth).await;
    let error = result.unwrap_err();
    assert!(is_wrapped_legacy_unauthorized(&error));
    timeout(Duration::from_secs(1), async {
        while state.token_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let drain = drain_and_shutdown_legacy_refresh(&auth);
    tokio::pin!(drain);
    assert!(
        timeout(Duration::from_millis(50), &mut drain)
            .await
            .is_err()
    );
    state.token_release.notify_one();
    timeout(Duration::from_secs(1), &mut drain)
        .await
        .expect("one-shot shutdown drains its queued refresh")
        .expect("non-sensitive refresh failure remains retryable");
    assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn legacy_six_401_failures_keep_old_pair_and_blow_fuse_without_owner_retry() {
    let state = Coordinated401Mock {
        owner_calls: Arc::new(AtomicUsize::new(0)),
        token_calls: Arc::new(AtomicUsize::new(0)),
        owner_pairs: Arc::new(Mutex::new(Vec::new())),
        refresh_pairs: Arc::new(Mutex::new(Vec::new())),
        token_entered: Arc::new(tokio::sync::Notify::new()),
        token_release: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = url::Url::parse(&format!("http://{address}/")).unwrap();
    let issuer = base.join("oauth2/v3/").unwrap();
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/api/1/products", get(coordinated_401_products))
                .route("/oauth2/v3/token", post(coordinated_failed_token))
                .with_state(server_state),
        )
        .await
        .unwrap();
    });
    let auth = coordinated_test_legacy_auth(issuer);
    let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();

    for _ in 0..5 {
        let result = list_vehicles_for_auth(&client, &auth).await;
        let error = result.unwrap_err();
        assert!(is_wrapped_legacy_unauthorized(&error));
        assert!(!is_terminal_auth_failure(&error));
    }
    let sixth = list_vehicles_for_auth(&client, &auth).await.unwrap_err();
    assert!(matches!(
        sixth,
        CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
    ));
    assert!(is_terminal_auth_failure(&sixth));
    assert_eq!(state.owner_calls.load(Ordering::SeqCst), 6);
    assert_eq!(state.token_calls.load(Ordering::SeqCst), 5);
    assert!(matches!(
        &auth,
        CollectionAuth::Legacy { fuse, .. } if fuse.try_lock().unwrap().is_blown()
    ));
    let CollectionAuth::Legacy { manager, .. } = &auth;
    let manager = manager.lock().await;
    assert_eq!(manager.access_token(), "old-access-secret");
    assert_eq!(manager.refresh_token(), "old-refresh-secret");
    assert!(
        manager
            .test_pair_matches("old-access-secret", "old-refresh-secret")
            .expect("managed predecessor pair")
    );
    drop(manager);
    shutdown_legacy_refresh(&auth).await;
    server.abort();
}

#[tokio::test]
async fn successful_coordinator_refresh_resets_the_legacy_401_fuse() {
    let state = Coordinated401Mock {
        owner_calls: Arc::new(AtomicUsize::new(0)),
        token_calls: Arc::new(AtomicUsize::new(0)),
        owner_pairs: Arc::new(Mutex::new(Vec::new())),
        refresh_pairs: Arc::new(Mutex::new(Vec::new())),
        token_entered: Arc::new(tokio::sync::Notify::new()),
        token_release: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = url::Url::parse(&format!("http://{address}/")).unwrap();
    let issuer = base.join("oauth2/v3/").unwrap();
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/api/1/products", get(coordinated_401_products))
                .route("/oauth2/v3/token", post(coordinated_success_token))
                .with_state(server_state),
        )
        .await
        .unwrap();
    });
    let auth = coordinated_test_legacy_auth(issuer);
    let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();
    for _ in 0..6 {
        let error = list_vehicles_for_auth(&client, &auth).await.unwrap_err();
        assert!(is_wrapped_legacy_unauthorized(&error));
    }
    assert_eq!(state.owner_calls.load(Ordering::SeqCst), 6);
    assert!(state.token_calls.load(Ordering::SeqCst) >= 5);
    assert!(matches!(
        &auth,
        CollectionAuth::Legacy { fuse, .. } if !fuse.try_lock().unwrap().is_blown()
    ));
    shutdown_legacy_refresh(&auth).await;
    server.abort();
}

async fn legacy_products(
    State(state): State<LegacyRuntimeMock>,
    headers: HeaderMap,
) -> impl IntoResponse {
    state.authorization.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    );
    let was_unauthorized =
        state
            .unauthorized
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                count.checked_sub(1)
            });
    if was_unauthorized.is_ok() {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    (
            StatusCode::OK,
            r#"{"response":[{"vehicle_id":9,"id":9,"vin":"5YJ3E1EA7KF000001","state":"online"}],"count":1}"#,
        )
            .into_response()
}

async fn legacy_token(State(state): State<LegacyRuntimeMock>, _body: String) -> impl IntoResponse {
    state.token_calls.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        json!({
            "access_token": "rotated-access",
            "refresh_token": "rotated-refresh",
            "token_type": "Bearer",
            "expires_in": 1_000_000_000u64,
            "created_at": 1_800_000_000i64,
        })
        .to_string(),
    )
}

#[tokio::test]
async fn legacy_collector_refresh_persists_then_stream_uses_rotated_access() {
    let state = LegacyRuntimeMock {
        unauthorized: Arc::new(AtomicUsize::new(1)),
        token_calls: Arc::new(AtomicUsize::new(0)),
        authorization: Arc::new(Mutex::new(Vec::new())),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = url::Url::parse(&format!("http://{address}/")).unwrap();
    let issuer = base.join("oauth2/v3/").unwrap();
    let server_state = state.clone();
    let http_server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/oauth2/v3/token", post(legacy_token))
                .route("/api/1/products", get(legacy_products))
                .with_state(server_state),
        )
        .await
        .unwrap();
    });

    let auth =
        crate::legacy_auth::LegacyAuth::for_test(issuer, "old-access-secret", "old-refresh-secret")
            .with_test_schedule(2_000_000_000, 1_900_000_000);
    let manager = Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
        auth,
        Arc::new(|_, _| Ok(())),
    )));
    let collection_auth = CollectionAuth::Legacy {
        manager: Arc::clone(&manager),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    };
    let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();
    assert!(matches!(
        list_vehicles_for_auth(&client, &collection_auth).await,
        Err(CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
            OwnerApiError::HttpStatus(401)
        )))
    ));
    let vehicles = list_vehicles_for_auth(&client, &collection_auth)
        .await
        .unwrap();
    assert_eq!(vehicles.len(), 1);
    assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        state.authorization.lock().unwrap().as_slice(),
        &["Bearer old-access-secret", "Bearer rotated-access"]
    );
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_endpoint = format!("ws://{}/streaming/", ws_listener.local_addr().unwrap());
    let ws_server = tokio::spawn(async move {
        let (tcp, _) = ws_listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        let message = socket.next().await.unwrap().unwrap();
        let Message::Text(text) = message else {
            panic!("stream subscribe must be text")
        };
        let frame: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(frame["token"], "rotated-access");
        socket
            .send(Message::Text(
                r#"{"msg_type":"control:hello","code":200}"#.into(),
            ))
            .await
            .unwrap();
    });
    let (events, _receiver) = mpsc::channel(4);
    let supervisor = TeslaStreamSupervisor::new_legacy_auth_for_test(
        VehicleId::from_test(9),
        StreamVehicleId::from_test(9),
        manager,
        StreamRegion::Global,
        ws_endpoint,
        client.legacy_auth_http_client(),
        events,
    )
    .unwrap();
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    ws_server.await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    http_server.abort();
}
