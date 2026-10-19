// SPDX-License-Identifier: AGPL-3.0-only

/// A refreshed-form schedule that is not due now and is not the "clock moved
/// back" case of C5 R27: it expires in five hours and is due in three.
fn not_due_schedule() -> (i64, i64) {
    let now = epoch_seconds_of(SystemTime::now());
    (now + 18_000, now + 10_800)
}

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
    token_entered: Arc<tokio::sync::Notify>,
    token_release: Arc<tokio::sync::Notify>,
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

fn coordinated_legacy_auth(
    issuer: url::Url,
    persisted: Arc<Mutex<(String, String)>>,
) -> CollectionAuth {
    let persisted_for_callback = Arc::clone(&persisted);
    let auth =
        crate::legacy_auth::LegacyAuth::for_test(issuer, "old-access-secret", "old-refresh-secret")
            .with_test_schedule(not_due_schedule().0, not_due_schedule().1);
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
            .with_test_schedule(not_due_schedule().0, not_due_schedule().1);
    let manager = LegacyAuthManager::for_test_with_active_pair(auth).expect("active test pair");
    CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    }
}

/// A 401 is handled inside the request (C5 R40, R41, R61): the request waits for
/// the one forced refresh, then is sent again once with the new pair. The server
/// answers 401 to every pair, so the second answer is a second counted rejection
/// of a new generation, which starts the confirmation pause (R41 count 2).
#[tokio::test]
async fn a_401_waits_for_the_forced_refresh_and_is_resent_once() {
    let state = Coordinated401Mock {
        owner_calls: Arc::new(AtomicUsize::new(0)),
        token_calls: Arc::new(AtomicUsize::new(0)),
        owner_pairs: Arc::new(Mutex::new(Vec::new())),
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

    let first = tokio::spawn({
        let client = client.clone();
        let auth = auth.clone();
        async move { list_vehicles_for_auth(&client, &auth).await }
    });
    // The first answer is a 401, so the request waits for the refresh.
    timeout(Duration::from_secs(1), async {
        while state.token_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the forced refresh starts");
    sleep(Duration::from_millis(50)).await;
    assert!(!first.is_finished(), "the request waits for the exchange");
    assert_eq!(state.owner_calls.load(Ordering::SeqCst), 1);
    state.token_release.notify_one();
    let error = timeout(Duration::from_secs(2), first)
        .await
        .expect("the request ends after the refresh")
        .expect("task")
        .expect_err("the server refuses the new pair as well");
    // Re-sent once (R61), then a second counted rejection pauses (R41).
    assert_eq!(state.owner_calls.load(Ordering::SeqCst), 2);
    assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);
    assert!(
        matches!(
            legacy_auth_error_of(&error),
            Some(LegacyAuthError::AuthUnavailable { retry_at: Some(_) })
        ),
        "{error:?}"
    );
    assert!(!is_credential_stop(&error));
    // Requests are held for the pause: the next one is not sent (R39, R41).
    assert!(matches!(
        legacy_auth_wait(&auth, SystemTime::now()),
        AuthWait::For(_)
    ));
    assert!(legacy_auth_gate(&auth).is_some());
    assert_eq!(state.owner_calls.load(Ordering::SeqCst), 2);
    server.abort();
}

/// Shutdown waits for an exchange in flight and outlives the caller that began
/// it (C5 R15a, R33a): dropping the request never aborts the exchange.
#[tokio::test]
async fn shutdown_waits_for_an_exchange_whose_request_was_dropped() {
    let state = Coordinated401Mock {
        owner_calls: Arc::new(AtomicUsize::new(0)),
        token_calls: Arc::new(AtomicUsize::new(0)),
        owner_pairs: Arc::new(Mutex::new(Vec::new())),
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
    // The request meets a 401, starts the forced refresh and is dropped.
    let request = tokio::spawn({
        let client = client.clone();
        let auth = auth.clone();
        async move { list_vehicles_for_auth(&client, &auth).await }
    });
    timeout(Duration::from_secs(1), async {
        while state.token_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    request.abort();
    let _ = request.await;

    let drain = drain_and_shutdown_legacy_refresh(&auth);
    tokio::pin!(drain);
    assert!(
        timeout(Duration::from_millis(50), &mut drain).await.is_err(),
        "normal shutdown awaits the exchange"
    );
    state.token_release.notify_one();
    timeout(Duration::from_secs(1), &mut drain)
        .await
        .expect("shutdown drains the exchange")
        .expect("no sensitive failure");
    assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);
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
            .with_test_schedule(not_due_schedule().0, not_due_schedule().1);
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
    // The first answer is a 401: the unit refreshes once and the request is sent
    // again with the rotated pair (C5 R40, R41, R61).
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
