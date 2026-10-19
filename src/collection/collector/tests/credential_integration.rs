// SPDX-License-Identifier: AGPL-3.0-only

// C5 part P4 spec tests at the collector boundary: the stream hand-off (test 40
// and the observer and persistence cases of tests 28 and 24 to 32), the
// refusal-check refresh (R49, tests 61 and 63), the wait and the per-request check
// (R39, test 90), the stop and the refusal as the scheduler sees them (R43, R47;
// C3 CT72 (b), (c) and (e)), the owner's retry (R44, C3 R65) and the classifier
// rows of the unit's own errors (C5 X1). Every test uses an in-process fake over
// loopback; nothing leaves the machine. Synthetic data only.

/// What the fake answers.
#[derive(Clone, Default)]
struct AuthFake {
    /// Every access token is refused with 401.
    reject_all: Arc<AtomicBool>,
    /// Access tokens refused with 401.
    rejected: Arc<Mutex<HashSet<String>>>,
    /// The answer of every list, when it is not one of the above.
    list_status: Arc<AtomicUsize>,
    /// The bearer token of every Owner API request, in order.
    owner_bearers: Arc<Mutex<Vec<String>>>,
    tokens: Arc<AtomicUsize>,
    /// How long a token request is held before it answers.
    token_delay: Arc<AtomicUsize>,
    /// While set, an Owner API request is held unanswered once it has been counted,
    /// so a read the collector made stays in flight.
    owner_hold: Arc<AtomicBool>,
    /// The vehicle actions (wake-ups) the fake has been sent.
    actions: Arc<AtomicUsize>,
}

impl AuthFake {
    fn action_requests(&self) -> usize {
        self.actions.load(Ordering::SeqCst)
    }

    fn owner_requests(&self) -> usize {
        self.owner_bearers.lock().unwrap().len()
    }

    fn token_requests(&self) -> usize {
        self.tokens.load(Ordering::SeqCst)
    }
}

async fn fake_products(
    State(fake): State<AuthFake>,
    headers: HeaderMap,
) -> axum::response::Response {
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    fake.owner_bearers.lock().unwrap().push(bearer.clone());
    while fake.owner_hold.load(Ordering::SeqCst) {
        sleep(Duration::from_millis(10)).await;
    }
    let status = fake.list_status.load(Ordering::SeqCst);
    if status != 0 {
        return (StatusCode::from_u16(status as u16).unwrap(), "{}").into_response();
    }
    if fake.reject_all.load(Ordering::SeqCst) || fake.rejected.lock().unwrap().contains(&bearer) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    (StatusCode::OK, r#"{"response":[],"count":0}"#).into_response()
}

/// A vehicle action: counted, and answered as a wake-up is.
async fn fake_wake(State(fake): State<AuthFake>) -> axum::response::Response {
    fake.actions.fetch_add(1, Ordering::SeqCst);
    (StatusCode::OK, r#"{"response":{"state":"online"}}"#).into_response()
}

async fn fake_token(State(fake): State<AuthFake>) -> axum::response::Response {
    let n = fake.tokens.fetch_add(1, Ordering::SeqCst) + 1;
    let delay = fake.token_delay.load(Ordering::SeqCst);
    if delay > 0 {
        sleep(Duration::from_millis(delay as u64)).await;
    }
    (
        StatusCode::OK,
        json!({
            "access_token": format!("AT{n}"),
            "refresh_token": format!("refresh-{n}"),
            "token_type": "Bearer",
            "expires_in": 28_800,
        })
        .to_string(),
    )
        .into_response()
}

/// The fake on a loopback port, the Owner API client for it, and the issuer URL
/// the unit refreshes at.
async fn auth_fake() -> (AuthFake, url::Url, OwnerApi, tokio::task::JoinHandle<()>) {
    crate::crypto::install_default_provider();
    let fake = AuthFake::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = url::Url::parse(&format!("http://{address}/")).unwrap();
    let router = Router::new()
        .route("/api/1/products", get(fake_products))
        .route("/api/1/vehicles/{id}/wake_up", post(fake_wake))
        .route("/oauth2/v3/token", post(fake_token))
        .with_state(fake.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let client = OwnerApi::for_fake_http(base.clone(), Duration::from_secs(2)).unwrap();
    (fake, base, client, server)
}

/// A collection credential over a pair held in memory whose schedule is not due.
fn auth_for(base: &url::Url, observer: bool) -> CollectionAuth {
    let (expires, next) = not_due_schedule();
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        base.join("oauth2/v3").unwrap(),
        "stored-access",
        "stored-refresh",
    )
    .with_test_schedule(expires, next);
    let manager = if observer {
        LegacyAuthManager::for_test_observer(auth)
    } else {
        LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())))
    };
    CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: !observer,
        region: StreamRegion::Global,
    }
}

async fn fuse_count(auth: &CollectionAuth) -> u8 {
    let CollectionAuth::Legacy { fuse, .. } = auth;
    fuse.lock().await.consecutive()
}

// ---- Test 40: the stream hand-off ---------------------------------------------------------------

#[tokio::test]
async fn t40_a_stream_rejection_is_confirmed_by_one_read_and_a_2xx_disproves_it() {
    let (fake, base, client, server) = auth_fake().await;
    // The unit's clock is the test's, so the minute that spaces two confirmation reads
    // can pass without the test waiting for it.
    let env = Arc::new(SkippingEnv::default());
    let (expires, next) = not_due_schedule();
    let pair = crate::legacy_auth::LegacyAuth::for_test(
        base.join("oauth2/v3").unwrap(),
        "stored-access",
        "stored-refresh",
    )
    .with_test_schedule(expires, next);
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test_with_env(
            pair,
            Arc::new(|_, _| Ok(())),
            Arc::clone(&env) as Arc<dyn crate::legacy_auth::attempt::AuthEnv>,
        ))),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    };
    refresh_after_stream_authentication_rejection(
        &client,
        &auth,
        StreamAuthenticationTransition::Rejected,
    )
    .await
    .expect("handled by the unit");
    // One confirmation read, carrying the pair in use; nothing counted, no refresh.
    assert_eq!(fake.owner_requests(), 1);
    assert_eq!(fake.owner_bearers.lock().unwrap()[0], "stored-access");
    assert_eq!(fake.token_requests(), 0);
    assert_eq!(fuse_count(&auth).await, 0);
    // A second `Rejected` while the pair is the same is ignored: no read, even when
    // the minute that spaces two reads has passed.
    env.skipped.fetch_add(61, Ordering::SeqCst);
    refresh_after_stream_authentication_rejection(
        &client,
        &auth,
        StreamAuthenticationTransition::Rejected,
    )
    .await
    .expect("ignored");
    assert_eq!(fake.owner_requests(), 1);
    // An `Authenticated` transition has no effect on the fuse.
    refresh_after_stream_authentication_rejection(
        &client,
        &auth,
        StreamAuthenticationTransition::Authenticated,
    )
    .await
    .expect("nothing");
    assert_eq!(fake.owner_requests(), 1);
    assert_eq!(fuse_count(&auth).await, 0);
    // The collector's counter counts the confirmation read as one `state` request.
    let CollectionAuth::Legacy { fuse, .. } = &auth;
    assert_eq!(fuse.lock().await.take_confirmation_count(), 1);
    server.abort();
}

/// An authentication environment whose clock only a test moves: it reads the second
/// it was made at, plus what the test adds, and a wait takes no time.
struct FixedAuthEnv {
    now: std::sync::atomic::AtomicI64,
}

impl FixedAuthEnv {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            now: std::sync::atomic::AtomicI64::new(epoch_seconds_of(SystemTime::now())),
        })
    }

    fn advance(&self, seconds: i64) {
        self.now.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl crate::legacy_auth::attempt::AuthEnv for FixedAuthEnv {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }

    fn sleep(&self, _seconds: i64) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    fn spread_unit(&self) -> f64 {
        0.0
    }
}

/// The credential unit of a store-backed bed, as the collector builds it, on the
/// clock `env` (its own clock: only the test moves it), wrapped for the confirmation
/// calls.
fn confirmation_auth(bed: &StopBed, env: &Arc<FixedAuthEnv>) -> CollectionAuth {
    bed.store_pair("stored-access", "stored-refresh");
    let manager = LegacyAuthManager::from_hub_teslamate_store_for_test(
        bed.store.clone(),
        bed.temporary.path(),
        &crate::legacy_auth::AuthEndpointConfig {
            mode: crate::legacy_auth::AuthMode::Tesla,
            region: Some(StreamRegion::Global),
            token_url: None,
            allow_loopback_http: true,
        },
        Some(bed.base.join("oauth2/v3/token").expect("token endpoint")),
        false,
        Arc::clone(env) as Arc<dyn crate::legacy_auth::attempt::AuthEnv>,
    )
    .expect("the unit starts");
    let region = manager.region();
    CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region,
    }
}

fn spawn_a_confirmation_caller(
    client: &OwnerApi,
    auth: &CollectionAuth,
) -> JoinHandle<Result<(), CollectorError>> {
    let (client, auth) = (client.clone(), auth.clone());
    tokio::spawn(async move {
        refresh_after_stream_authentication_rejection(
            &client,
            &auth,
            StreamAuthenticationTransition::Rejected,
        )
        .await
    })
}

#[tokio::test]
async fn c5t07_concurrent_confirmation_callers_wait_without_a_second_read() {
    // The production wrapper holds the shared fuse and the manager through the whole
    // confirmation (C5 R41a, B2 clarification 3): a second caller queues behind them
    // and waits, so exactly one confirmation request is in flight. When the first
    // finishes (a 2xx disproves the rejection) the second finds the pair cleared and
    // reads nothing: no duplicate confirmation, and the usual result applies. The
    // unit is the collector's own, over a temporary store.
    let bed = StopBed::new().await;
    let env = FixedAuthEnv::new();
    let auth = confirmation_auth(&bed, &env);
    bed.fake.owner_hold.store(true, Ordering::SeqCst);
    let first = spawn_a_confirmation_caller(&bed.client, &auth);
    timeout(Duration::from_secs(5), async {
        while bed.fake.owner_requests() < 1 {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first confirmation read is in flight");
    let second = spawn_a_confirmation_caller(&bed.client, &auth);
    sleep(Duration::from_millis(300)).await;
    assert!(!first.is_finished(), "the first confirmation is held");
    assert!(!second.is_finished(), "the second caller waits for the fuse and the manager");
    assert_eq!(bed.fake.owner_requests(), 1, "one request in flight");
    bed.fake.owner_hold.store(false, Ordering::SeqCst);
    first.await.expect("join").expect("the first confirmation");
    second.await.expect("join").expect("the second caller");
    assert_eq!(bed.fake.owner_requests(), 1, "no duplicate confirmation");
    assert_eq!(bed.fake.token_requests(), 0);
    assert_eq!(fuse_count(&auth).await, 0, "the 2xx disproved the rejection");
    // The collector counts the one confirmation read once.
    let outcomes = take_confirmation_outcomes(&auth).await;
    assert_eq!(outcomes.reads, 1);
}

#[tokio::test]
async fn c5t07_a_queued_confirmation_after_an_inconclusive_answer_keeps_the_sixty_second_spacing() {
    // The first confirmation is answered 429, which neither proves nor disproves the
    // rejection. A second caller queued behind it acquires the locks when the first
    // has finished, and the ordinary rules apply: the unit's own clock decides, and a
    // further read is not sent before sixty seconds have passed since the first.
    let bed = StopBed::new().await;
    let env = FixedAuthEnv::new();
    let auth = confirmation_auth(&bed, &env);
    bed.fake.owner_hold.store(true, Ordering::SeqCst);
    bed.fake.list_status.store(429, Ordering::SeqCst);
    let first = spawn_a_confirmation_caller(&bed.client, &auth);
    timeout(Duration::from_secs(5), async {
        while bed.fake.owner_requests() < 1 {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first confirmation read is in flight");
    let second = spawn_a_confirmation_caller(&bed.client, &auth);
    sleep(Duration::from_millis(300)).await;
    assert!(!second.is_finished(), "the second caller waits");
    assert_eq!(bed.fake.owner_requests(), 1, "still one request in flight");
    bed.fake.owner_hold.store(false, Ordering::SeqCst);
    first.await.expect("join").expect("the first confirmation");
    second.await.expect("join").expect("the second caller");
    assert_eq!(
        bed.fake.owner_requests(),
        1,
        "the queued caller sent no second request: the spacing applies after the release"
    );
    // A rejection 59 seconds after the first read is within the spacing: none sent.
    env.advance(59);
    spawn_a_confirmation_caller(&bed.client, &auth)
        .await
        .expect("join")
        .expect("handled");
    assert_eq!(bed.fake.owner_requests(), 1, "59 s: no read");
    // At sixty seconds exactly one more is sent.
    env.advance(1);
    spawn_a_confirmation_caller(&bed.client, &auth)
        .await
        .expect("join")
        .expect("handled");
    assert_eq!(bed.fake.owner_requests(), 2, "60 s: exactly one more read");
    assert_eq!(bed.fake.token_requests(), 0);
}

#[tokio::test]
async fn t40_a_stream_rejection_the_account_confirms_is_one_counted_rejection_and_one_refresh() {
    let (fake, base, client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    fake.reject_all.store(true, Ordering::SeqCst);
    refresh_after_stream_authentication_rejection(
        &client,
        &auth,
        StreamAuthenticationTransition::Rejected,
    )
    .await
    .expect("handled by the unit");
    assert_eq!(fake.owner_requests(), 1, "the confirmation read");
    assert_eq!(fake.token_requests(), 1, "the forced refresh");
    assert_eq!(fuse_count(&auth).await, 1);
    // The rejection does not end the run and sets no lease state by itself.
    wait_for_legacy_refresh_before_owner(&auth)
        .await
        .expect("no fence");
    server.abort();
}

#[tokio::test]
async fn t28_an_observer_s_stream_rejection_is_a_wait_and_never_a_refresh() {
    let (fake, base, client, server) = auth_fake().await;
    let auth = auth_for(&base, true);
    fake.reject_all.store(true, Ordering::SeqCst);
    refresh_after_stream_authentication_rejection(
        &client,
        &auth,
        StreamAuthenticationTransition::Rejected,
    )
    .await
    .expect("an observer's rejection is a wait, not an error of the run");
    assert_eq!(fake.token_requests(), 0);
    let CollectionAuth::Legacy { manager, .. } = &auth;
    assert_eq!(
        manager.lock().await.status_now().state,
        AuthState::ObserverWaiting
    );
    // The collector is told to wait, and sends nothing.
    assert!(matches!(
        legacy_auth_wait(&auth, SystemTime::now()),
        AuthWait::For(wait) if wait == Duration::from_secs(60)
    ));
    let error = legacy_auth_gate(&auth).expect("a request would not be sent");
    assert!(matches!(
        legacy_auth_error_of(&error),
        Some(LegacyAuthError::AuthUnavailable { .. })
    ));
    server.abort();
}

/// R24: a refresh whose answer cannot be written is kept and written again; it is
/// not a loss of the credential. The previous pair stays in use while it is usable,
/// no second token request is sent, and the account is not stopped.
#[tokio::test]
async fn a_failed_write_of_a_refreshed_pair_is_retried_and_stops_nothing() {
    let (fake, base, client, server) = auth_fake().await;
    let (expires, next) = not_due_schedule();
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        base.join("oauth2/v3").unwrap(),
        "stored-access",
        "stored-refresh",
    )
    .with_test_schedule(expires, next);
    let collection_auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
            auth,
            Arc::new(|_, _| Err(CredentialError::LegacyTokenStateWrite)),
        ))),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    };
    fake.reject_all.store(true, Ordering::SeqCst);
    refresh_after_stream_authentication_rejection(
        &client,
        &collection_auth,
        StreamAuthenticationTransition::Rejected,
    )
    .await
    .expect("the write is retried, not fatal");
    wait_for_legacy_refresh_before_owner(&collection_auth)
        .await
        .expect("a pending write is not a sensitive failure");
    assert_eq!(fake.token_requests(), 1);
    let CollectionAuth::Legacy { manager, .. } = &collection_auth;
    assert_eq!(
        manager.lock().await.status_now().state,
        AuthState::PersistencePending
    );
    // The previous pair is still handed out while it is usable.
    assert!(matches!(
        legacy_auth_wait(&collection_auth, SystemTime::now()),
        AuthWait::Ready
    ));
    shutdown_legacy_refresh(&collection_auth)
        .await
        .expect("the shutdown keeps its admission");
    server.abort();
}

// ---- R49: the refusal check -----------------------------------------------------------------------

#[tokio::test]
async fn t61_the_refusal_check_asks_the_unit_for_one_refresh_and_an_observer_for_none() {
    let (fake, base, client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    // The refused list carried the stored pair: the check refreshes once.
    fake.list_status.store(403, Ordering::SeqCst);
    let CollectionAuth::Legacy { manager, fuse, .. } = &auth;
    {
        let mut fuse = fuse.lock().await;
        let mut manager = manager.lock().await;
        let _ = client
            .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
            .await;
    }
    assert!(matches!(
        refusal_check_refresh_for_auth(&client, &auth).await,
        RefusalCheck::Refreshed
    ));
    assert_eq!(fake.token_requests(), 1);
    // A second ask for the same refused list needs no exchange: the pair is newer.
    assert!(matches!(
        refusal_check_refresh_for_auth(&client, &auth).await,
        RefusalCheck::Refreshed
    ));
    assert_eq!(fake.token_requests(), 1);

    // An observer refreshes nothing and changes nothing (test 63).
    let observer = auth_for(&base, true);
    let before = {
        let CollectionAuth::Legacy { manager, .. } = &observer;
        manager.lock().await.status_now()
    };
    assert!(matches!(
        refusal_check_refresh_for_auth(&client, &observer).await,
        RefusalCheck::ObserverRefreshDisabled
    ));
    let CollectionAuth::Legacy { manager, .. } = &observer;
    assert_eq!(manager.lock().await.status_now(), before);
    assert_eq!(fake.token_requests(), 1);
    server.abort();
}

// ---- R39: the wait and the per-request check -----------------------------------------------------

#[tokio::test]
async fn t90_the_collector_asks_before_it_counts_and_a_busy_unit_is_never_cancelled() {
    let (fake, base, client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    assert_eq!(legacy_auth_wait(&auth, SystemTime::now()), AuthWait::Ready);
    assert!(legacy_auth_gate(&auth).is_none());
    // A request that holds the unit (here waiting for a held token exchange): the
    // next asker finds it busy, and the request is not cancelled by being asked
    // about.
    fake.token_delay.store(300, Ordering::SeqCst);
    fake.rejected
        .lock()
        .unwrap()
        .insert("stored-access".to_owned());
    let request = tokio::spawn({
        let client = client.clone();
        let auth = auth.clone();
        async move { list_vehicles_for_auth(&client, &auth).await }
    });
    while fake.token_requests() == 0 {
        tokio::task::yield_now().await;
    }
    assert_eq!(legacy_auth_wait(&auth, SystemTime::now()), AuthWait::Ready);
    request
        .await
        .expect("task")
        .expect("the request completes after the exchange");
    // A stop is `Stopped`, and the check returns the unit's own error.
    let CollectionAuth::Legacy { manager, .. } = &auth;
    manager
        .lock()
        .await
        .stop_account(crate::legacy_auth::SignOutReason::AccessTokenRejected);
    assert_eq!(
        legacy_auth_wait(&auth, SystemTime::now()),
        AuthWait::Stopped
    );
    let error = legacy_auth_gate(&auth).expect("stopped");
    assert!(is_credential_stop(&error), "{error:?}");
    assert!(
        is_terminal_auth_failure(&error),
        "a signed-out account is the lease's terminal state"
    );
    assert!(!is_sensitive_access_failure(&error));
    server.abort();
}

// ---- R43, R47: the stop and the refusal as the scheduler and the lease see them -------------------

#[tokio::test]
async fn ct72_b_a_stop_the_unit_reached_is_the_account_s_stop_and_its_lift_collects_again() {
    let (_fake, base, _client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    let mut core = Core::owner();
    let (state, receiver) = watch::channel(SupervisedCollectorState::Active);
    let now = core.at(0);
    assert!(!apply_credential_state(
        &auth,
        &mut core.scheduler,
        &state,
        now
    ));
    assert_eq!(*receiver.borrow(), SupervisedCollectorState::Active);
    // The unit signs the account out outside any scheduled request.
    let CollectionAuth::Legacy { manager, fuse, .. } = &auth;
    manager
        .lock()
        .await
        .stop_account(crate::legacy_auth::SignOutReason::AccessTokenRejected);
    let now = core.at(1);
    assert!(apply_credential_state(
        &auth,
        &mut core.scheduler,
        &state,
        now
    ));
    assert_eq!(core.scheduler.account.state, AccountState::CredentialStop);
    assert_eq!(
        *receiver.borrow(),
        SupervisedCollectorState::AuthenticationTerminal
    );
    // The owner's "retry collection" reaches the unit before the scheduler applies it.
    manager.lock().await.sync_fuse(&mut *fuse.lock().await);
    let retry_at = core.at(2);
    apply_owner_actions_with_credentials(
        &mut core.scheduler,
        &auth,
        &[OwnerActionRow {
            id: 1,
            kind: OwnerActionKind::RetryCollection,
            vehicle: None,
        }],
        retry_at,
    )
    .await;
    let now = core.at(3);
    assert!(!apply_credential_state(
        &auth,
        &mut core.scheduler,
        &state,
        now
    ));
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
    assert_eq!(*receiver.borrow(), SupervisedCollectorState::Active);
    server.abort();
}

#[tokio::test]
async fn ct72_e_a_refusal_the_unit_set_becomes_the_accounts_refusal_by_mode_and_reason() {
    let (_fake, base, _client, server) = auth_fake().await;
    for (reason, expected) in [
        (
            crate::legacy_auth::RefusalReason::OwnerApi,
            "owner_api_refused",
        ),
        (
            crate::legacy_auth::RefusalReason::Client,
            "owner_api_client_refused",
        ),
    ] {
        let auth = auth_for(&base, false);
        let mut core = Core::owner();
        let (state, _receiver) = watch::channel(SupervisedCollectorState::Active);
        let CollectionAuth::Legacy { manager, .. } = &auth;
        manager
            .lock()
            .await
            .apply_collector_start(&CollectorStartView {
                reset_refusal: false,
                account: Some(("legacy", "owner_api_refused", expected)),
            });
        let _ = reason;
        let now = core.at(0);
        // A refusal is the scheduler's own state, not a credential stop.
        assert!(!apply_credential_state(
            &auth,
            &mut core.scheduler,
            &state,
            now
        ));
        assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
        assert_eq!(core.scheduler.account.state_reason, expected);
    }
    server.abort();
}

#[test]
fn the_unit_s_errors_have_their_rows_in_the_classifier() {
    use crate::legacy_auth::{LegacyAuthError as E, RefusalReason, SignOutReason};
    let wrapped = |error: E| {
        CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(error)))
    };
    let list = RequestKind::List;
    let read = RequestKind::Read;
    let legacy = crate::config::CollectorMode::Legacy;
    let provider = crate::config::CollectorMode::Provider;
    assert_eq!(
        classify(
            &wrapped(E::AuthUnavailable { retry_at: Some(5) }),
            read,
            legacy
        ),
        ErrorClass::AuthWait
    );
    assert_eq!(
        classify(&wrapped(E::VehicleAccessRejected), read, legacy),
        ErrorClass::ClientError
    );
    assert_eq!(
        classify(
            &wrapped(E::OwnerApiRefused {
                reason: RefusalReason::OwnerApi
            }),
            list,
            legacy
        ),
        ErrorClass::OwnerApiRefusal
    );
    assert_eq!(
        classify(
            &wrapped(E::OwnerApiRefused {
                reason: RefusalReason::Client
            }),
            list,
            provider
        ),
        ErrorClass::ProviderRefusal
    );
    for terminal in [
        E::SignedOut {
            reason: SignOutReason::RefreshOutcomeUnknown,
        },
        E::AccessTokenRejected,
        E::ProviderTokenRejected,
        E::ProviderTokenExpired,
        E::RegionConflict,
        E::TokenUrlInvalid,
    ] {
        assert_eq!(
            classify(&wrapped(terminal), read, legacy),
            ErrorClass::CredentialStop
        );
    }
    // Neither a wait on a retry nor a refused client is a stop. The lost authority
    // is not an account state either (C3 R42, R47, B2 clarification): the loops
    // normalise it to `SensitiveAccessUnavailable` and end the run before the
    // scheduler classifies anything, so it carries no stop here.
    assert_eq!(
        classify(&wrapped(E::RefreshTokenRejected), read, legacy),
        ErrorClass::Auth
    );
    assert_eq!(
        classify(&wrapped(E::RefreshNotRenewed), read, legacy),
        ErrorClass::Auth
    );
    // A failed write of a successor is a wait for the write (C5 R45 row 5, R24), never
    // a stop and never the lost authority: it takes the ordinary authentication
    // back-off (C5T-03).
    for error in [E::SensitivePersistenceUnavailable, E::Persistence] {
        assert_eq!(
            classify(&wrapped(error), read, legacy),
            ErrorClass::Auth
        );
    }
    // The same through the other wrappers.
    assert_eq!(
        classify(
            &CollectorError::LegacyAuth(E::AuthUnavailable { retry_at: None }),
            read,
            legacy
        ),
        ErrorClass::AuthWait
    );
}

#[test]
fn the_unit_s_waits_and_refusals_enter_the_scheduler_by_their_rows_with_the_units_own_time() {
    use crate::legacy_auth::{LegacyAuthError as E, RefusalReason};
    let mut core = ep_online_core(1);
    let id = core_id(1);
    let wall_seconds = core.scheduler.wall_or_last().div_euclid(1_000);
    let now = core.at(0);
    // CT72 (c): `AuthUnavailable` until t + 90 s on a read: the account waits for
    // that time, the car's failure count is untouched and the read stays due.
    let error = CollectorError::LegacyAuth(E::AuthUnavailable {
        retry_at: Some(wall_seconds + 90),
    });
    core.scheduler.vehicle_failed_for_error(id, &error, now);
    assert_eq!(
        core.scheduler.account.wait,
        Some((now + Duration::from_secs(90), WaitReason::AuthWait))
    );
    assert_eq!(core.scheduler.vehicles[&id].consecutive_failures, 0);
    // A time that is not known is the unit's own look-again time: a minute (R39).
    let mut core = ep_online_core(1);
    let now = core.at(0);
    let error = CollectorError::LegacyAuth(E::AuthUnavailable { retry_at: None });
    core.scheduler.vehicle_failed_for_error(id, &error, now);
    assert_eq!(
        core.scheduler.account.wait,
        Some((now + Duration::from_secs(60), WaitReason::AuthWait))
    );
    // An observer's rejection is such a wait and ends no run (R28, R39).
    let mut core = Core::owner();
    let now = core.at(0);
    let delay = core.scheduler.discovery_failed_for_error(&error, now);
    assert_eq!(delay, Duration::from_secs(60));
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
    // CT72 (d): the unit's own refusal, by mode and reason.
    let mut core = Core::owner();
    let now = core.at(0);
    core.scheduler.discovery_failed_for_error(
        &CollectorError::LegacyAuth(E::OwnerApiRefused {
            reason: RefusalReason::Client,
        }),
        now,
    );
    assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
    assert_eq!(
        core.scheduler.account.state_reason,
        "owner_api_client_refused"
    );
    // CT72 (a): one car's refused access is that car's failure only.
    let mut core = ep_online_core(1);
    let now = core.at(0);
    core.scheduler.vehicle_failed_for_error(
        id,
        &CollectorError::LegacyAuth(E::VehicleAccessRejected),
        now,
    );
    assert_eq!(core.scheduler.vehicles[&id].consecutive_failures, 1);
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
}

// ---- The loop maintains the unit; the owner's reads are counted ----------------------------------

#[tokio::test]
async fn the_loop_asks_a_unit_with_nothing_to_hand_out_to_refresh_and_counts_confirmation_reads() {
    let (fake, base, client, server) = auth_fake().await;
    // A pair whose token has expired and whose refresh is due: nothing can be handed
    // out, so the loop's maintenance asks for the refresh.
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        base.join("oauth2/v3").unwrap(),
        "stored-access",
        "stored-refresh",
    );
    // The forced refresh below follows the first within a minute, so the minimum
    // interval of C5 R19 holds it back; the environment skips that wait.
    let manager = LegacyAuthManager::for_test_with_env(
        auth,
        Arc::new(|_, _| Ok(())),
        Arc::new(SkippingEnv::default()),
    );
    let collection_auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region: StreamRegion::Global,
    };
    {
        // An imported pair that has failed once: the retry is pending and the
        // token past usability, which is a wait.
        let CollectionAuth::Legacy { manager, .. } = &collection_auth;
        let mut manager = manager.lock().await;
        manager.auth_for_test().usable_until = Some(0);
        manager.auth_for_test().startup_refresh_pending = true;
    }
    assert!(matches!(
        legacy_auth_wait(&collection_auth, SystemTime::now()),
        AuthWait::For(_)
    ));
    maintain_legacy_credential(&client, &collection_auth).await;
    assert_eq!(fake.token_requests(), 1);
    assert_eq!(
        legacy_auth_wait(&collection_auth, SystemTime::now()),
        AuthWait::Ready
    );
    // A confirmation read is counted as one `state` request (C3 R24, R25).
    let CollectionAuth::Legacy { fuse, .. } = &collection_auth;
    fake.reject_all.store(true, Ordering::SeqCst);
    refresh_after_stream_authentication_rejection(
        &client,
        &collection_auth,
        StreamAuthenticationTransition::Rejected,
    )
    .await
    .expect("handled");
    let ledger = Core::owner().scheduler.request_ledger();
    let state_sent =
        |ledger: &RequestLedger| ledger.view().requests[RequestCategory::State.index()].1;
    let before = state_sent(&ledger);
    let outcomes = take_confirmation_outcomes(&collection_auth).await;
    count_confirmation_reads(outcomes.reads, &ledger);
    assert_eq!(state_sent(&ledger), before + 1);
    assert_eq!(fuse.lock().await.take_confirmation_count(), 0);
    server.abort();
}

// ---- The loop: a stop the unit holds, and the owner's retry --------------------------------------

/// C5 R43, R44 (b), R47 and C3 R65, in the real loop: the account is signed out
/// (`access_token_rejected`) when the collector starts, so the loop sends nothing, the
/// lease is `auth_terminal`, readiness says so and the stop's gap is open. The owner's
/// "retry collection" reaches the unit before the scheduler applies it, the stop is
/// lifted, one list is sent and the lease is active again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t42_a_signed_out_account_sends_nothing_and_the_owners_retry_resumes_it() {
    use crate::legacy_auth::{
        AuthEndpointConfig, AuthMode, SignOutReason,
        state::{AuthStatus, AuthStop, FuseSnapshot, StoppedMode},
    };

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);
    crate::teslamate_credentials::replace_key(temporary.path(), b"chain-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial =
        OwnerTokens::from_secret_parts("loop-access".to_owned(), "loop-refresh".to_owned())
            .expect("initial pair");
    let generation = crate::teslamate_token::legacy_refresh_credential_generation(&initial);
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("Cloak pair");
    let (expires, next) = not_due_schedule();
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, expires, next)
                .expect("a schedule that is not due"),
        )
        .expect("store pair");
    // The unit's own row from the previous run: signed out for this very pair.
    let now = epoch_seconds_of(SystemTime::now());
    let signed_out = AuthStatus::stopped(
        AuthStop::SignedOut(SignOutReason::AccessTokenRejected),
        generation,
        StoppedMode::Legacy,
        false,
        FuseSnapshot {
            consecutive: 3,
            generation: Some(generation),
            last_rejection_at: Some(now),
            pause_until: None,
            rejected_access: None,
        },
        false,
    );
    store
        .write_collector_auth_row(&signed_out.to_row(), now * 1_000)
        .expect("stored stop");

    let (fake, base, client, server) = auth_fake().await;
    let manager = LegacyAuthManager::from_hub_teslamate_store_for_test(
        store.clone(),
        temporary.path(),
        &AuthEndpointConfig {
            mode: AuthMode::Tesla,
            region: Some(StreamRegion::Global),
            token_url: None,
            allow_loopback_http: true,
        },
        Some(base.join("oauth2/v3/token").expect("token endpoint")),
        false,
        Arc::new(SkippingEnv::default()),
    )
    .expect("the stop is restored, not refused");
    let region = manager.region();
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region,
    };
    let config = supervised_restart_test_config(temporary.path());
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task_store = store.clone();
    let mut task = tokio::spawn(async move {
        run_supervised_with_access(
            &task_store,
            &config,
            supervised_restart_test_cadence(),
            client,
            auth,
            "ws://127.0.0.1:9/streaming/".to_owned(),
            CursorKey::from_bytes([0xC5; 32]),
            Some(ready_tx),
            RunWriteGuard::for_run(None),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });
    let _cursor =
        wait_for_supervised_signal_or_abort("collector readiness", &mut task, ready_rx).await;
    let lease_state = |expected: &'static str| {
        let store = store.clone();
        move || {
            store
                .open()
                .expect("catalogue")
                .query_row("SELECT state FROM supervised_collector_lease", [], |row| {
                    row.get::<_, String>(0)
                })
                .map(|state| state == expected)
                .unwrap_or(false)
        }
    };
    // R43: the lease is `auth_terminal` while the account is signed out, and nothing
    // is sent.
    k6_wait_for(
        &mut task,
        "the terminal lease",
        lease_state("auth_terminal"),
    )
    .await;
    sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        fake.owner_requests(),
        0,
        "a signed-out account sends nothing"
    );
    assert_eq!(fake.token_requests(), 0);
    assert_eq!(
        store.collector_auth_readiness().expect("readiness"),
        Some(crate::db::CollectorAuthReadiness::SignedOut)
    );
    // R47: the stop opens the scheduler's `auth_terminal` gap.
    k6_wait_for(&mut task, "the gap", || {
        store
            .collection_gaps(10)
            .map(|gaps| {
                gaps.iter()
                    .any(|gap| gap.kind == "auth_terminal" && gap.ended_at_ms.is_none())
            })
            .unwrap_or(false)
    })
    .await;
    // The owner's retry: the unit lifts, the scheduler collects again, one list goes.
    store
        .insert_collector_owner_action(
            "retry_collection",
            None,
            current_epoch_millis().expect("clock"),
        )
        .expect("retry row");
    k6_wait_for(&mut task, "the list after the retry", || {
        fake.owner_requests() >= 1
    })
    .await;
    k6_wait_for(&mut task, "an active lease", lease_state("active")).await;
    assert_eq!(fake.token_requests(), 0, "the retry rotates nothing");
    assert_eq!(
        store
            .load_collector_auth_row()
            .expect("row")
            .expect("a row")
            .state,
        "ready"
    );
    shutdown_tx.send(()).expect("shutdown");
    join_supervised_restart_task("collector join", &mut task)
        .await
        .expect("collector result");
    server.abort();
}

/// C5 R28 and R39, C3 R47 in the real loop: an observer whose list is refused with a
/// 401 does not end the run. The refusal is a wait: the unit says the observer is
/// waiting for the refresh owner's next pair, nothing more is sent, and the lease stays
/// active. (Its resumption on a new pair, within the minute, is the manager's test 28
/// and the wait test below.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t28_an_observer_whose_list_is_refused_waits_and_the_run_goes_on() {
    use crate::legacy_auth::{AuthEndpointConfig, AuthMode};

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);
    crate::teslamate_credentials::replace_key(temporary.path(), b"chain-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let write_pair = |access: &str, refresh: &str| {
        let tokens =
            OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned()).expect("pair");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
                .expect("Cloak pair");
        let (expires, next) = not_due_schedule();
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, expires, next)
                    .expect("a schedule that is not due"),
            )
            .expect("store pair");
    };
    write_pair("observer-access", "observer-refresh");
    let (fake, base, client, server) = auth_fake().await;
    fake.reject_all.store(true, Ordering::SeqCst);
    let env = Arc::new(SkippingEnv::default());
    let manager = LegacyAuthManager::from_hub_teslamate_store_for_test(
        store.clone(),
        temporary.path(),
        &AuthEndpointConfig {
            mode: AuthMode::Tesla,
            region: Some(StreamRegion::Global),
            token_url: None,
            allow_loopback_http: true,
        },
        Some(base.join("oauth2/v3/token").expect("token endpoint")),
        true,
        Arc::clone(&env) as Arc<dyn crate::legacy_auth::attempt::AuthEnv>,
    )
    .expect("observer");
    let region = manager.region();
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: false,
        region,
    };
    let config = supervised_restart_test_config(temporary.path());
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task_store = store.clone();
    let mut task = tokio::spawn(async move {
        run_supervised_with_access(
            &task_store,
            &config,
            supervised_restart_test_cadence(),
            client,
            auth,
            "ws://127.0.0.1:9/streaming/".to_owned(),
            CursorKey::from_bytes([0xC6; 32]),
            Some(ready_tx),
            RunWriteGuard::for_run(None),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });
    let _cursor =
        wait_for_supervised_signal_or_abort("collector readiness", &mut task, ready_rx).await;
    // The refused list: the observer waits, the run is not ended.
    let auth_state = || {
        store
            .load_collector_auth_row()
            .ok()
            .flatten()
            .map(|row| (row.state, row.reason))
    };
    k6_wait_for(&mut task, "the observer's wait", || {
        auth_state()
            == Some((
                "observer_waiting".to_owned(),
                Some("token_rejected".to_owned()),
            ))
    })
    .await;
    let sent = fake.owner_requests();
    assert_eq!(sent, 1, "the one refused list");
    sleep(Duration::from_millis(2_500)).await;
    assert!(
        !task.is_finished(),
        "an observer's 401 does not end the run"
    );
    assert_eq!(
        fake.owner_requests(),
        sent,
        "nothing is sent while it waits"
    );
    assert_eq!(fake.token_requests(), 0, "an observer never refreshes");
    // CT74 (c) (C3 R47, B2 clarification): an observer's 401 is an `AuthWait`, not a
    // stop, so no `auth_terminal` gap opens.
    assert_eq!(open_auth_terminal_gaps(&store), 0, "no stop and no gap");
    assert_eq!(
        store.collector_auth_readiness().expect("readiness"),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
    assert_eq!(
        store
            .open()
            .expect("catalogue")
            .query_row("SELECT state FROM supervised_collector_lease", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("lease"),
        "active"
    );
    // C5T-12, CT74 (c): the loop goes on while the observer waits. An owner action
    // is still read at a settings refresh and applied (here a "retry collection",
    // which an observer's unit has nothing to lift for), so the waiting account is
    // not a stopped loop.
    store
        .insert_collector_owner_action(
            "retry_collection",
            None,
            current_epoch_millis().expect("clock"),
        )
        .expect("retry row");
    k6_wait_for(&mut task, "the owner action to be applied while waiting", || {
        store
            .pending_collector_owner_actions()
            .map(|pending| pending.is_empty())
            .unwrap_or(false)
    })
    .await;
    assert_eq!(open_auth_terminal_gaps(&store), 0, "still no stop and no gap");
    // The refresh owner's next pair (an external write) resumes collection within the
    // minute the unit re-reads the stored generation, in the loop, with the new token.
    fake.reject_all.store(false, Ordering::SeqCst);
    write_pair("observer-access-2", "observer-refresh-2");
    env.skipped.fetch_add(61, Ordering::SeqCst);
    k6_wait_for(&mut task, "the list after the new pair", || {
        fake.owner_requests() > sent
    })
    .await;
    assert_eq!(
        fake.owner_bearers.lock().unwrap().last().map(String::as_str),
        Some("observer-access-2"),
        "the next list carries the new token"
    );
    k6_wait_for(&mut task, "the ready row", || auth_state().is_some_and(|(state, _)| state == "ready"))
        .await;
    assert_eq!(fake.token_requests(), 0, "an observer still never refreshes");
    shutdown_tx.send(()).expect("shutdown");
    join_supervised_restart_task("collector join", &mut task)
        .await
        .expect("collector result");
    server.abort();
}

/// C5 R39 and C3 R47: a wait the scheduler took for the unit's time ends when the
/// unit says it can hand out a token again, so an observer that is given a new pair
/// does not sit out the rest of the minute the scheduler had been told.
#[tokio::test]
async fn the_scheduler_s_auth_wait_ends_when_the_unit_is_ready_again() {
    let (_fake, base, _client, server) = auth_fake().await;
    let auth = auth_for(&base, true);
    let mut core = ep_online_core(1);
    let (state, _receiver) = watch::channel(SupervisedCollectorState::Active);
    let now = core.at(0);
    // The observer's rejection: a wait of a minute for the unit's time.
    let CollectionAuth::Legacy { manager, .. } = &auth;
    manager.lock().await.observer_rejected();
    let error = legacy_auth_gate(&auth).expect("waiting");
    core.scheduler.discovery_failed_for_error(&error, now);
    assert!(core.scheduler.account.wait.is_some());
    assert!(!apply_credential_state(
        &auth,
        &mut core.scheduler,
        &state,
        now
    ));
    assert!(
        core.scheduler.account.wait.is_some(),
        "the unit still waits"
    );
    // The refresh owner's next pair is adopted: the unit is ready, the wait ends.
    {
        let mut manager = manager.lock().await;
        let auth = manager.auth_for_test();
        auth.observer_rejected = false;
    }
    let now = core.at(5);
    assert!(!apply_credential_state(
        &auth,
        &mut core.scheduler,
        &state,
        now
    ));
    assert!(core.scheduler.account.wait.is_none());
    assert_eq!(core.scheduler.account.list_wanted, Some(now));
    // A rate-limit wait is Tesla's: it is not the unit's to end.
    core.scheduler.rate_limited(120, now);
    let later = core.at(6);
    let _ = apply_credential_state(&auth, &mut core.scheduler, &state, later);
    assert!(matches!(
        core.scheduler.account.wait,
        Some((_, WaitReason::RateLimited))
    ));
    server.abort();
}

// ---- CT72 (c): AuthUnavailable holds the requests not yet started (B2 clarification) --------------

/// A scheduler of `cars` online cars read once, on a manual clock whose wall time is
/// the real one, so that the unit's own times (the system clock) and the scheduler's
/// agree, as they do in production.
fn ct72_online_core_on_real_time(cars: u64) -> Core {
    let t0 = Instant::now();
    let wall_ms = current_epoch_millis().expect("clock");
    let clock = ManualClock::new(t0, wall_ms);
    let mut scheduler = VehicleScheduler::new_with(
        core_cadence(),
        crate::config::CollectorMode::Legacy,
        core_cadence().fleet_policy(),
        t0,
        clock.clone(),
        FixedJitter::constant(0.0),
    );
    scheduler.accept_discovery((1..=cars).map(|id| core_vehicle(id, "online")).collect(), t0);
    assert_eq!(scheduler.due_vehicles(t0).len() as u64, cars);
    for id in 1..=cars {
        scheduler.vehicle_succeeded(core_id(id), PollPhase::Online, false, t0);
    }
    Core { scheduler, clock, t0 }
}

#[tokio::test]
async fn ct72_c_a_unit_that_says_wait_leaves_all_three_reads_unsent_uncounted_and_due() {
    // C3 R47: each request of a tick asks C5 R39 at its own start, before it is
    // counted. `legacy_auth_wait` says `For(90 s)` when a tick with three due reads
    // starts: no read is sent or counted, the three cars stay due, no failure is
    // counted, and the reads go out when the unit's time has come.
    let (fake, base, client, server) = auth_fake().await;
    let auth = auth_for(&base, false);
    let wall_seconds = epoch_seconds_of(SystemTime::now());
    {
        let CollectionAuth::Legacy { manager, .. } = &auth;
        // The unit's time is 90 s after the tick's start, which is a minute after
        // the cars' first reads on the scheduler's clock.
        manager.lock().await.auth_for_test().hold_until = Some(wall_seconds + 60 + 90);
    }
    assert!(matches!(
        legacy_auth_wait(&auth, SystemTime::now()),
        AuthWait::For(wait) if (149..=150).contains(&wait.as_secs())
    ));
    let mut core = ct72_online_core_on_real_time(3);
    let ledger = core.scheduler.request_ledger();
    let sent = |ledger: &RequestLedger| ledger.view().requests[RequestCategory::Data.index()].1;
    let t = core.at(60);
    ep_sync_clock(&core, t);
    let due = core.scheduler.due_vehicles(t);
    assert_eq!(due.len(), 3);
    for id in &due {
        let future = owner_read_future(&mut core.scheduler, *id, false, None, &client, &auth, &ledger);
        let (key, result) = future.await;
        assert_eq!(key, TickKey::Read(*id));
        let error = result.expect_err("held for the unit's wait");
        assert!(
            matches!(
                legacy_auth_error_of(&error),
                Some(LegacyAuthError::AuthUnavailable { retry_at: Some(_) })
            ),
            "{error:?}"
        );
        // The loop's handler: the account waits for the unit's time, the car stays due.
        core.scheduler.vehicle_failed_for_error(*id, &error, t);
    }
    assert_eq!(fake.owner_requests(), 0, "nothing reached Tesla");
    assert_eq!(sent(&ledger), 0, "nothing was counted");
    for id in &due {
        assert_eq!(core.scheduler.vehicles[id].consecutive_failures, 0, "no failure counted");
    }
    let wait = core.scheduler.account.wait.expect("an account wait");
    assert_eq!(wait.1, WaitReason::AuthWait);
    let remaining = wait.0.saturating_duration_since(t).as_secs();
    assert!((88..=90).contains(&remaining), "{remaining} s");
    // Nothing starts before the unit's time; the three cars are due after it.
    assert!(core.scheduler.due_vehicles(t + Duration::from_secs(60)).is_empty());
    let later = wait.0 + Duration::from_millis(1);
    ep_sync_clock(&core, later);
    assert_eq!(core.scheduler.due_vehicles(later).len(), 3);
    // The unit's time has come: the reads go out and are counted.
    {
        let CollectionAuth::Legacy { manager, .. } = &auth;
        manager.lock().await.auth_for_test().hold_until = None;
    }
    assert!(legacy_auth_gate(&auth).is_none());
    for id in &due {
        let future = owner_read_future(&mut core.scheduler, *id, false, None, &client, &auth, &ledger);
        let _ = future.await;
    }
    assert_eq!(sent(&ledger), 3, "the three reads were counted at their start");
    server.abort();
}

#[tokio::test]
async fn ct72_c_a_read_that_meets_the_wait_does_not_cancel_the_requests_in_flight() {
    // The first of three concurrent reads returns `AuthUnavailable { retry_at: t + 90 s }`
    // while the other two, already handed to the unit, are in flight: their outcomes
    // are committed in the tick, no request starts before t + 90 s (the unit's check at
    // each request's start holds the refused car's next read), and no failure is
    // counted.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let (fake, base, client, server) = auth_fake().await;
            let auth = auth_for(&base, false);
            let mut bed = TickBed::owner();
            // The unit's times are the system clock's; the bed's wall clock is moved to it.
            let real_ms = current_epoch_millis().expect("clock");
            bed.clock.step_wall_ms(real_ms - bed.wall());
            bed.list((1..=3).map(|id| core_vehicle(id, "online")).collect()).await;
            let t0 = bed.now();
            let due = bed.scheduler.due_vehicles(t0);
            assert_eq!(due.len(), 3);
            let commits = seam.tick_commits();
            bed.scheduler.begin_tick();
            let received_at_ms = bed.wall();
            // The unit says wait 90 s from now: the first read's own check meets it.
            {
                let CollectionAuth::Legacy { manager, .. } = &auth;
                manager.lock().await.auth_for_test().hold_until =
                    Some(epoch_seconds_of(SystemTime::now()) + 90);
            }
            let ledger = bed.scheduler.request_ledger();
            let mut requests: Vec<TickFuture<'_>> = Vec::new();
            let refused = due[0];
            requests.push(owner_read_future(
                &mut bed.scheduler,
                refused,
                false,
                None,
                &client,
                &auth,
                &ledger,
            ));
            for (index, id) in due.iter().copied().enumerate().skip(1) {
                let snapshot = VehicleData::for_test(
                    index as u64 + 1,
                    json!({"drive_state": {"shift_state": null, "speed": null, "power": 0,
                                           "timestamp": received_at_ms},
                           "charge_state": {"charging_state": "Disconnected"}}),
                );
                requests.push(Box::pin(async move {
                    // Handed to the unit before the wait: in flight when the first
                    // read has already been refused.
                    sleep(Duration::from_millis(50)).await;
                    (TickKey::Read(id), Ok(TickPayload::Data(snapshot)))
                }));
            }
            let mut run = TickRun {
                scheduler: &mut bed.scheduler,
                identities: &mut bed.identities,
                commit: TickCommit {
                    store: &bed.store,
                    cursor_key: &bed.cursor_key,
                    provider: bed.provider,
                    offline_drive_timeout: Duration::from_secs(600),
                },
                batch: TickBatch::default(),
                terrain_wake: &bed.terrain,
                any_wrote: false,
            };
            let flow = drive_requests(&mut run, requests, None, |run: &mut TickRun<'_>, key, result, now| {
                // The Owner loop's handler, for the two arms this case meets.
                let TickKey::Read(vehicle_id) = key else {
                    return Ok(ReadFlow::Continue);
                };
                match result {
                    Ok(TickPayload::Data(snapshot)) => {
                        let vehicle = run
                            .scheduler
                            .vehicles()
                            .into_iter()
                            .find(|vehicle| vehicle.id == vehicle_id)
                            .expect("the car");
                        run.scheduler.vehicle_succeeded(
                            vehicle_id,
                            poll_phase(&snapshot),
                            sleep_eligible_with_policy(&snapshot, false),
                            now,
                        );
                        run.batch.snapshots.push(SnapshotItem {
                            vehicle,
                            snapshot,
                            received_at_ms,
                        });
                    }
                    Err(error) => {
                        run.scheduler.vehicle_failed_for_error(vehicle_id, &error, now);
                    }
                    _ => {}
                }
                Ok(ReadFlow::Continue)
            })
            .await
            .expect("the tick");
            assert_eq!(flow, ReadFlow::Continue);
            // The two requests in flight were not cancelled: their snapshots are stored
            // in this tick's transaction.
            let stored: i64 = bed.query(
                "SELECT COUNT(*) FROM current_observations WHERE record_type LIKE '%vehicle_data%'",
            );
            assert_eq!(stored, 2);
            // (At most one commit per result: the bed's clock is a manual one, so the
            // age bound of R5 may cut the tick into its three results.)
            assert!(seam.tick_commits() - commits <= 3);
            // No failure is counted for any car, and the refused car is still due.
            for id in &due {
                assert_eq!(bed.scheduler.vehicles[id].consecutive_failures, 0);
            }
            assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
            assert!(!bed.scheduler.vehicles[&refused].in_flight, "it stays due");
            // No request starts before the unit's time: the check at the next read's
            // own start holds it, unsent and uncounted.
            let sent_before = ledger.view().requests[RequestCategory::Data.index()].1;
            let again = owner_read_future(
                &mut bed.scheduler,
                refused,
                false,
                None,
                &client,
                &auth,
                &ledger,
            )
            .await;
            assert!(again.1.is_err(), "held");
            assert_eq!(ledger.view().requests[RequestCategory::Data.index()].1, sent_before);
            assert_eq!(fake.owner_requests(), 0, "nothing reached Tesla");
            server.abort();
        })
        .await;
}

// ---- B2 P3: a resident action meets a standing C3 refusal (C3 R48, R49) --------------------------

/// A hub with a stored Owner pair, the credential unit over it, the shared locks the
/// resident control holds, and the Owner API double with an action route.
struct ResidentRig {
    _temporary: tempfile::TempDir,
    dir: std::path::PathBuf,
    store: HubStore,
    fake: AuthFake,
    client: OwnerApi,
    server: tokio::task::JoinHandle<()>,
    manager: Arc<tokio::sync::Mutex<LegacyAuthManager>>,
    fuse: Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
    refresh: Arc<LegacyRefreshCoordinator>,
    hub_vehicle: Uuid,
    eid: i64,
    token_url: url::Url,
    mode: crate::legacy_auth::AuthMode,
}

impl ResidentRig {
    async fn new(mode: crate::legacy_auth::AuthMode) -> Self {
        let (fake, base, client, server) = auth_fake().await;
        let token_url = base.join("oauth2/v3/token").expect("token endpoint");
        let (temporary, store) = cadence_wait_hub();
        let dir = temporary.path().to_path_buf();
        ct72_owner_stores_credentials(&store, &dir, "owner-access-0", "owner-refresh-0");
        let (hub_vehicle, eid, _) = store.configured_tesla_vehicles().expect("cars")[0].clone();
        let unit = ct72_unit(&store, &dir, mode, &token_url);
        Self {
            _temporary: temporary,
            dir,
            store,
            fake,
            client,
            server,
            manager: Arc::new(tokio::sync::Mutex::new(unit)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            hub_vehicle,
            eid,
            token_url,
            mode,
        }
    }

    fn c3_mode(&self) -> &'static str {
        match self.mode {
            crate::legacy_auth::AuthMode::Provider => "provider",
            _ => "legacy",
        }
    }

    fn c3_refusal_state(&self) -> &'static str {
        match self.mode {
            crate::legacy_auth::AuthMode::Provider => "provider_refused",
            _ => "owner_api_refused",
        }
    }

    /// The three entry points of a resident action: the executor, the fused request
    /// path, and the control socket (which serves only through the executor). Each
    /// is asked for one wake-up and its outcome is `Ok` or the error as text.
    async fn try_each_path(&self) -> [Result<(), String>; 3] {
        let executor = execute_resident_legacy_vehicle_action(
            &self.store,
            &self.client,
            &self.manager,
            &self.fuse,
            &self.refresh,
            self.hub_vehicle,
            LegacyVehicleAction::Wake,
        )
        .await
        .map(|_| ())
        .map_err(|error| format!("{error:?}"));
        let fused = {
            let mut fuse = self.fuse.lock().await;
            let mut manager = self.manager.lock().await;
            self.client
                .execute_vehicle_action_with_legacy_auth_fused(
                    &mut manager,
                    &mut fuse,
                    VehicleId::try_from_i64(self.eid).expect("vehicle id"),
                    LegacyVehicleAction::Wake,
                )
                .await
                .map(|_| ())
                .map_err(|error| format!("{error:?}"))
        };
        let socket = ResidentControlSocket::bind(&self.dir).expect("control socket");
        let serving = tokio::spawn(socket.serve(
            self.store.clone(),
            self.client.clone(),
            Arc::clone(&self.manager),
            Arc::clone(&self.fuse),
            Arc::clone(&self.refresh),
        ));
        let dispatched =
            request_resident_vehicle_action(&self.dir, self.hub_vehicle, LegacyVehicleAction::Wake)
                .await
                .map(|_| ())
                .map_err(|error| format!("{error:?}"));
        serving.abort();
        let _ = serving.await;
        [executor, fused, dispatched]
    }

    fn c5_row(&self) -> Option<crate::db::CollectorAuthRow> {
        self.store.load_collector_auth_row().expect("auth row")
    }

    fn receipts(&self) -> i64 {
        self.store
            .open()
            .expect("catalogue")
            .query_row("SELECT COUNT(*) FROM outbound_request_receipts", [], |row| row.get(0))
            .expect("receipts")
    }
}

impl Drop for ResidentRig {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3_resident_action_respects_live_c3_refusal() {
    use crate::legacy_auth::AuthMode;
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            for mode in [AuthMode::Tesla, AuthMode::Provider] {
                let rig = ResidentRig::new(mode).await;
                p3_seed_c3(&rig.store, rig.c3_mode(), &"a".repeat(64), rig.c3_refusal_state());
                let c5_before = rig.c5_row();
                let receipts_before = rig.receipts();
                // A matching standing refusal: no action, no token request, no receipt, and
                // C5's row is untouched, on every path.
                for (path, outcome) in ["executor", "fused", "socket"]
                    .iter()
                    .zip(rig.try_each_path().await)
                {
                    assert!(outcome.is_err(), "{mode:?} {path}: {outcome:?}");
                }
                assert_eq!(rig.fake.action_requests(), 0, "{mode:?}");
                assert_eq!(rig.fake.token_requests(), 0, "{mode:?}");
                assert_eq!(rig.fake.owner_requests(), 0, "{mode:?}");
                assert_eq!(rig.c5_row(), c5_before, "{mode:?}: C5 is unchanged");
                assert_eq!(rig.receipts(), receipts_before, "{mode:?}: no audit receipt");
                // An unreadable account row rejects too (the read fails, so the check
                // cannot say there is no refusal).
                rig.store
                    .open()
                    .expect("catalogue")
                    .execute_batch("ALTER TABLE collector_account_state RENAME TO p3_hidden_account")
                    .expect("hide the table");
                for (path, outcome) in ["executor", "fused", "socket"]
                    .iter()
                    .zip(rig.try_each_path().await)
                {
                    assert!(outcome.is_err(), "{mode:?} {path} (read failure): {outcome:?}");
                }
                rig.store
                    .open()
                    .expect("catalogue")
                    .execute_batch("ALTER TABLE p3_hidden_account RENAME TO collector_account_state")
                    .expect("restore the table");
                assert_eq!(rig.fake.action_requests(), 0, "{mode:?}: read failure");
                assert_eq!(rig.c5_row(), c5_before, "{mode:?}");
                // The owner's retry (the scheduler's commit that makes the account
                // collect again) permits one action, on each path.
                rig.store
                    .open()
                    .expect("catalogue")
                    .execute("UPDATE collector_account_state SET state = 'collecting'", [])
                    .expect("retry");
                let outcomes = rig.try_each_path().await;
                assert!(outcomes.iter().all(Result::is_ok), "{mode:?}: {outcomes:?}");
                assert_eq!(rig.fake.action_requests(), 3, "{mode:?}: one per path");
            }
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3_resident_action_rechecks_after_external_pair_write() {
    use crate::legacy_auth::AuthMode;
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let rig = ResidentRig::new(AuthMode::Tesla).await;
            let config = p3_config(&rig.dir, P3Config::Legacy);
            let g0 = rig.store.owner_wire_credential_identity().expect("identity");
            // C3 holds a standing refusal for the configured Legacy mode, with the
            // fingerprint of the pair stored now; C5 is not refused.
            let legacy_fp = collector_config_fingerprint(
                crate::config::CollectorMode::Legacy,
                &crate::config::owner_api_origin_text(&config.collector),
                "wss://streaming.example.test",
                &g0,
            );
            p3_seed_c3(&rig.store, "legacy", &legacy_fp, "owner_api_refused");
            // The live unit holds a C5 stop of the pair now stored (signed out): the
            // action is blocked by it as well.
            let env = Arc::new(SkippingEnv::default());
            *rig.manager.lock().await = ct72_unit_with_env(
                &rig.store,
                &rig.dir,
                crate::legacy_auth::AuthMode::Tesla,
                &rig.token_url,
                Arc::clone(&env),
            );
            rig.manager
                .lock()
                .await
                .stop_account(crate::legacy_auth::SignOutReason::RefreshTokenRejected);
            assert_eq!(ct72_state_of(&*rig.manager.lock().await), "signed_out");
            // An external pair write lifts C5's stop; the live unit finds the write on
            // its own recheck (R44a), without being rebuilt, and is usable again. C3's
            // refusal remains, so the action is still blocked, by C3 alone.
            ct72_owner_stores_credentials(&rig.store, &rig.dir, "owner-access-1", "owner-refresh-1");
            env.sleep_for_test(61);
            assert!(rig.manager.lock().await.recheck_external_write(), "the write is found");
            assert_eq!(ct72_state_of(&*rig.manager.lock().await), "ready");
            assert_eq!(rig.c5_row().expect("row").state, "ready", "C5's stop is lifted");
            for (path, outcome) in ["executor", "fused", "socket"]
                .iter()
                .zip(rig.try_each_path().await)
            {
                assert!(outcome.is_err(), "{path}: C3's refusal still blocks: {outcome:?}");
            }
            assert_eq!(rig.fake.action_requests(), 0);
            assert_eq!(rig.fake.token_requests(), 0);
            // The ordinary start sees the changed credential identity: it resets C3's
            // refusal, tells the unit, and the action is then permitted (one).
            let started = p3_start(&rig.store, &config).expect("the ordinary start");
            assert!(started.refusal_reset);
            rig.manager.lock().await.apply_collector_start(&started.credential_view());
            let outcomes = rig.try_each_path().await;
            assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
            assert_eq!(rig.fake.action_requests(), 3);

            // The control: a refusal C3 holds for another mode is not this unit's, so
            // the action goes out.
            let other = ResidentRig::new(AuthMode::Tesla).await;
            p3_seed_c3(&other.store, "provider", &"b".repeat(64), "provider_refused");
            let outcomes = other.try_each_path().await;
            assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
            assert_eq!(other.fake.action_requests(), 3);
        })
        .await;
}
