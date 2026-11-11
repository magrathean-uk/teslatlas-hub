// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use tokio::net::TcpListener;

use super::*;

const TEST_SCOPED_ACCESS: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iLCJ2ZWhpY2xlX2NtZHMiLCJ2ZWhpY2xlX2NoYXJnaW5nX2NtZHMiXX0.sig";

#[test]
fn fleet_scope_claims_are_bounded_and_collection_scopes_are_required() {
    let summary = fleet_scope_summary(TEST_SCOPED_ACCESS).expect("scope summary");
    assert!(summary.vehicle_device_data);
    assert!(summary.vehicle_location);
    assert!(summary.vehicle_commands);
    assert!(summary.vehicle_charging_commands);

    let missing_location = FleetSetupCredentials::new(
        "e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig".to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("syntactically valid credentials");
    assert!(matches!(
        missing_location.require_collection_scopes(),
        Err(FleetCredentialError::MissingCollectionScopes)
    ));
    assert!(matches!(
        fleet_scope_summary("not-a-jwt"),
        Err(FleetCredentialError::InvalidAccessTokenClaims)
    ));
}

#[test]
fn fleet_setup_round_trips_encrypted_and_redacted() {
    let (root, store) = due_fleet_store();
    let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store.clone(), root.path());
    let plain = vault.load(CredentialSlot::FleetApi).expect("proved load").expect("pair");
    assert!(plain.access.as_str() == "fleet-old-access" && plain.refresh.as_str() == "fleet-old-refresh", "pair round trip");
    assert!(plain.client_id.as_deref() == Some("fleet-client") && plain.region.as_deref() == Some("eu"), "client metadata");
    assert!(!format!("{plain:?}").contains(plain.access.as_str()) && !format!("{plain:?}").contains(plain.refresh.as_str()), "redacted pair");
    let bytes = fs::read(root.path().join("hub.sqlite")).expect("catalogue");
    for value in [plain.access.as_bytes(),plain.refresh.as_bytes()] { assert!(!bytes.windows(value.len()).any(|part| part == value), "no plaintext catalogue token"); }
}

#[test]
fn stored_fleet_credentials_require_collection_scopes() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::legacy_owner_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig".to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("syntactically valid credentials");
    crate::auth::credential_vault::fixtures::seed_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist");

    assert!(matches!(
        crate::auth::credential_vault::fixtures::validate_fixture_fleet_credentials(&store, temporary.path()),
        Err(FleetCredentialError::MissingCollectionScopes)
    ));
}

#[test]
fn operational_validation_rejects_a_refresh_that_begins_after_its_first_check() {
    let root = crate::private_tempdir().expect("root");
    let store = HubStore::initialize(root.path()).expect("store");
    let credentials = FleetSetupCredentials::new(TEST_SCOPED_ACCESS.into(), "fleet-refresh".into(), "fleet-client".into(), FleetRegion::EuropeMiddleEastAndAfrica, 28800).expect("credentials");
    crate::auth::credential_vault::fixtures::seed_fleet_setup_credentials(&store,root.path(),&credentials,UNIX_EPOCH + std::time::Duration::from_secs(1000)).expect("seed");
    let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store.clone(), root.path());
    let generation = vault.stored_generation(CredentialSlot::FleetApi).expect("generation").expect("pair");
    validate_stored_fleet_credentials_after(&vault, || {}).expect("valid control");
    let result = validate_stored_fleet_credentials_after(&vault, || { vault.begin_refresh(CredentialSlot::FleetApi,generation).expect("begin in window"); });
    assert!(matches!(result,Err(FleetCredentialError::RotationOutcomeUnknown)), "operational recheck refuses");
    assert!(inspect_stored_fleet_credentials(&store, root.path()).expect("inspection").unresolved_refresh, "unresolved inspection");
}

#[test]
fn schema_55_cursor_encrypted_row_upgrades_and_scrubs_live_sqlite_traces() {
    let (root, access, refresh) = crate::auth::credential_migration::g24_m2_m::seed_admitted_cursor_fleet();
    let lane = HubStore::open_immutable_read_only(root.path()).expect("immutable lane");
    assert!(lane.credential_format_state().expect("format").expect("state").fleet_api_state == crate::db::CredentialSlotFormatState::Pending, "preflight is pending");
    lane.verify_immutable_snapshot_unchanged().expect("preflight unchanged"); drop(lane);
    let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(HubStore::initialize(root.path()).expect("store"),root.path());
    let plain = vault.load(CredentialSlot::FleetApi).expect("converted load").expect("pair");
    assert!(plain.access.as_str() == access && plain.refresh.as_str() == refresh, "cursor-derived predecessor converted");
    assert!(root.path().join("secrets").join("credential-keys").exists(), "new keyring");
    let state = HubStore::initialize(root.path()).expect("status").credential_format_state().expect("format").expect("state");
    assert!(state.completed_at_ms.is_some() && !state.purge_required && state.wal_scrubbed, "conversion purged and scrubbed");
}

#[test]
fn signout_deletes_fleet_key_and_preserves_cursor_authority() {
    let (root,store) = due_fleet_store();
    let cursor = load_existing_cursor_key_bytes(root.path()).expect("cursor").expect("cursor present");
    let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store,root.path());
    let keyring = fs::read(root.path().join("secrets").join("credential-keys")).expect("keyring");
    vault.clear_credential_pair(CredentialSlot::FleetApi).expect("sign out");
    assert!(vault.load(CredentialSlot::FleetApi).expect("empty read").is_none(), "pair removed");
    assert!(fs::read(root.path().join("secrets").join("credential-keys")).expect("retained keyring") == keyring, "C10.R35 keyring retained");
    assert!(load_existing_cursor_key_bytes(root.path()).expect("retained cursor").expect("cursor").as_slice() == cursor.as_slice(), "cursor unchanged");
}

#[tokio::test]
async fn due_refresh_rotates_encrypted_generation_receipt_and_restart_state() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::legacy_owner_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "fleet-old-access".to_owned(),
        "fleet-old-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        60,
    )
    .expect("setup credentials");
    crate::auth::credential_vault::fixtures::seed_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    let initial = fleet_snapshot(&store)
        .expect("initial Fleet row")
        .expect("initial Fleet credentials");
    let input_generation = initial
        .4
        .expect("initial credential generation");
    let initial_access_ciphertext = initial.0.as_slice().to_vec();
    let initial_refresh_ciphertext = initial.1.as_slice().to_vec();

    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake auth listener");
    let address = listener.local_addr().expect("fake auth address");
    let router = Router::new().route(
        "/oauth2/v3/token",
        post(move |headers: HeaderMap, body: Bytes| {
            let recorded = Arc::clone(&recorded);
            async move {
                let valid = headers.get("content-type").is_some_and(|value| {
                    value.as_bytes() == b"application/x-www-form-urlencoded"
                }) && body.as_ref()
                    == b"grant_type=refresh_token&client_id=fleet-client&refresh_token=fleet-old-refresh";
                recorded.lock().expect("request ledger").push(valid);
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"access_token":"fleet-next-access","refresh_token":"fleet-next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
                )
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("fake auth server");
    });
    let endpoint =
        url::Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
    let api = FleetAuthApi::for_fake_http(endpoint, std::time::Duration::from_secs(2))
        .expect("fake Fleet auth client");
    let mut manager =
        FleetAuthManager::from_store(store.clone(), temporary.path()).expect("Fleet manager");

    manager
        .refresh_if_due(&api, UNIX_EPOCH + std::time::Duration::from_secs(2_000))
        .await
        .expect("due refresh succeeds");
    assert_eq!(*requests.lock().expect("request ledger"), vec![true]);
    assert!(manager.access_token().expose() == "fleet-next-access");

    let rotated = fleet_snapshot(&store)
        .expect("rotated Fleet row")
        .expect("rotated Fleet credentials");
    let output_generation = rotated
        .4
        .expect("rotated credential generation");
    assert_ne!(output_generation, input_generation);
    assert_ne!(rotated.0.as_slice(), initial_access_ciphertext.as_slice());
    assert_ne!(rotated.1.as_slice(), initial_refresh_ciphertext.as_slice());
    let successor = manager.vault.load(CredentialSlot::FleetApi).expect("proved successor").expect("pair");
    assert!(successor.access.as_str() == "fleet-next-access" && successor.refresh.as_str() == "fleet-next-refresh", "successor opens through vault");

    let receipt =(store
        .open()
        .expect("receipt catalogue")
        ).query_row(
            "SELECT r.transport, r.operation, r.safety_class, r.precondition,
                    r.outcome, r.http_status, r.completed_at_ms IS NOT NULL,
                    b.input_generation, b.output_generation
               FROM outbound_request_receipts AS r
               JOIN credential_refresh_attempts AS b ON b.receipt_id = r.id
              ORDER BY r.id DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<u16>>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .expect("durable refresh receipt");
    assert_eq!(receipt.0, "fleet_api");
    assert_eq!(receipt.1, "token_refresh");
    assert_eq!(receipt.2, "non_wake_endpoint");
    assert_eq!(receipt.3, "not_required");
    assert_eq!(receipt.4, "success");
    assert_eq!(receipt.5, Some(200));
    assert!(receipt.6);
    assert_eq!(receipt.7, input_generation.to_string());
    assert_eq!(
        receipt.8.as_deref(),
        Some(output_generation.to_string().as_str())
    );
    assert!(
        !store.unresolved_fleet_refresh_status()
            .expect("resolved refresh")
    );

    drop(manager);
    drop(store);
    let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
    assert!(
        !restarted.unresolved_fleet_refresh_status()
            .expect("restart refresh state")
    );
    let restarted_manager =
        FleetAuthManager::from_store(restarted, temporary.path()).expect("restart loads successor");
    assert!(restarted_manager.access_token().expose() == "fleet-next-access");
    assert_eq!(restarted_manager.credential_generation, output_generation);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn post_send_refresh_error_remains_fenced_after_restart() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::legacy_owner_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "fleet-old-access".to_owned(),
        "fleet-old-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        60,
    )
    .expect("setup credentials");
    crate::auth::credential_vault::fixtures::seed_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    let input_generation = fleet_snapshot(&store)
        .expect("Fleet row")
        .expect("Fleet credentials")
        .4
        .expect("credential generation");

    let requests = Arc::new(Mutex::new(0_usize));
    let recorded = Arc::clone(&requests);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake auth listener");
    let address = listener.local_addr().expect("fake auth address");
    let router = Router::new().route(
        "/oauth2/v3/token",
        post(move || {
            let recorded = Arc::clone(&recorded);
            async move {
                let mut requests = recorded.lock().expect("request ledger");
                *requests += 1;
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [("content-type", "application/json")],
                    r#"{"error":"temporarily_unavailable"}"#,
                )
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("fake auth server");
    });
    let endpoint =
        url::Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
    let api = FleetAuthApi::for_fake_http(endpoint, std::time::Duration::from_secs(2))
        .expect("fake Fleet auth client");
    let mut manager =
        FleetAuthManager::from_store(store.clone(), temporary.path()).expect("Fleet manager");

    assert!(matches!(
        manager.refresh_now(&api, SystemTime::now()).await,
        Err(FleetCredentialError::Api(
            FleetApiError::ProviderHttpStatus { status: 500, .. }
        ))
    ));
    assert!(
        store.unresolved_fleet_refresh_status()
            .expect("fenced failure")
    );
    assert_eq!(
        fleet_snapshot(&store)
            .expect("Fleet row")
            .expect("Fleet credentials")
            .4,
        Some(input_generation)
    );

    drop(manager);
    drop(store);
    let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
    assert!(restarted.unresolved_fleet_refresh_status().unwrap());
    assert!(matches!(
        FleetAuthManager::from_store(restarted.clone(), temporary.path()),
        Err(FleetCredentialError::RotationOutcomeUnknown)
    ));
    assert_eq!(*requests.lock().expect("request ledger"), 1);
    let receipt = restarted
        .open()
        .unwrap()
        .query_row(
            "SELECT outcome, completed_at_ms IS NULL
               FROM outbound_request_receipts
              WHERE transport = 'fleet_api' AND operation = 'token_refresh'
              ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )
        .unwrap();
    assert_eq!(receipt, ("started".to_owned(), true));

    server.abort();
    let _ = server.await;
}

#[test]
fn only_definitively_unconsumed_refresh_failures_are_retryable() {
    assert_eq!(
        retryable_refresh_completion(&FleetApiError::RequestNotSent),
        Some(OutboundRequestCompletion {
            outcome: OutboundRequestOutcome::TransportError,
            http_status: None,
            retry_after_seconds: None,
        })
    );
    for ambiguous in [
        FleetApiError::RequestTimeout,
        FleetApiError::Transport,
        FleetApiError::HttpStatus(401),
        FleetApiError::HttpStatus(500),
        FleetApiError::ProviderHttpStatus {
            status: 500,
            error: "temporarily_unavailable".to_owned(),
            description: None,
        },
        FleetApiError::RateLimited {
            retry_after_seconds: 17,
        },
        FleetApiError::ResponseTooLarge,
        FleetApiError::ResponseRead,
        FleetApiError::InvalidResponse,
    ] {
        assert!(retryable_refresh_completion(&ambiguous).is_none());
    }
}

#[tokio::test]
async fn invalidated_admission_blocks_due_refresh_before_transport() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::legacy_owner_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "fleet-admission-access".to_owned(),
        "fleet-admission-refresh".to_owned(),
        "fleet-admission-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        60,
    )
    .expect("setup credentials");
    crate::auth::credential_vault::fixtures::seed_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist credentials");
    let admission =
        crate::auth::credential_vault::fixtures::fixture_admission(&store, temporary.path());
    let mut manager = {
    let runtime_store = store.clone();
    let runtime_path = temporary.path();
    let runtime_admission = admission;
    let runtime_loss = None;
    crate::auth::credential_vault::CredentialVault::open_for_runtime(
        runtime_store.clone(), std::sync::Arc::clone(&runtime_admission), runtime_path, runtime_loss.as_ref(),
    )
    .map_err(crate::fleet_credentials::FleetCredentialError::from)
    .and_then(|vault| FleetAuthManager::from_store_for_admitted_user(vault, runtime_path, runtime_admission, runtime_loss))
}
    .expect("admitted Fleet manager");
    manager.mark_refresh_due();

    let requests = Arc::new(Mutex::new(0_usize));
    let recorded = Arc::clone(&requests);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake auth listener");
    let address = listener.local_addr().expect("fake auth address");
    let router = Router::new().route(
        "/oauth2/v3/token",
        post(move || {
            let recorded = Arc::clone(&recorded);
            async move {
                *recorded.lock().expect("request ledger") += 1;
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"access_token":"next","refresh_token":"next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
                )
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("fake auth server");
    });
    let endpoint =
        url::Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
    let api = FleetAuthApi::for_fake_http(endpoint, std::time::Duration::from_secs(2))
        .expect("fake auth client");

    let lock_path = temporary
        .path()
        .join(crate::user_lifetime_lock::LOCK_FILE_NAME);
    fs::remove_file(&lock_path).expect("remove admitted lock path");
    fs::write(&lock_path, b"").expect("replace admitted lock path");
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
        .expect("replacement lock mode");

    assert!(matches!(
        manager.refresh_if_due(&api, SystemTime::now()).await,
        Err(FleetCredentialError::SensitiveAccessUnavailable)
    ));
    assert_eq!(*requests.lock().expect("request ledger"), 0);
    assert!(
        !store.unresolved_fleet_refresh_status()
            .expect("no refresh began")
    );

    server.abort();
    let _ = server.await;
}

/// Codex R06: a refresh whose request has gone out is finished even when its caller
/// is dropped (a shutdown drops the collection future). Tesla answers after the
/// caller has gone; the successor is still written, the receipt resolved, and a
/// graceful shutdown that waits for the exchange applies it to the manager.
#[tokio::test]
async fn a_started_refresh_is_stored_when_its_caller_is_dropped() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::legacy_owner_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "fleet-old-access".to_owned(),
        "fleet-old-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        60,
    )
    .expect("setup credentials");
    crate::auth::credential_vault::fixtures::seed_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    let input_generation = fleet_snapshot(&store)
        .expect("Fleet row")
        .expect("Fleet credentials")
        .4
        .expect("credential generation");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake auth listener");
    let address = listener.local_addr().expect("fake auth address");
    let router = Router::new().route(
        "/oauth2/v3/token",
        post(|| async {
            // Tesla rotates the token and answers only after the caller has gone.
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                r#"{"access_token":"fleet-next-access","refresh_token":"fleet-next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
            )
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("fake auth server");
    });
    let endpoint =
        url::Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
    let api = FleetAuthApi::for_fake_http(endpoint, std::time::Duration::from_secs(5))
        .expect("fake Fleet auth client");
    let mut manager =
        FleetAuthManager::from_store(store.clone(), temporary.path()).expect("Fleet manager");

    // The caller gives up after 50 ms, as a dropped collection future does.
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            manager.refresh_now(&api, SystemTime::now()),
        )
        .await
        .is_err()
    );
    assert!(
        store.unresolved_fleet_refresh_status().expect("receipt"),
        "the attempt is still open while Tesla has not answered"
    );

    // A graceful shutdown waits for the exchange and applies its outcome.
    manager
        .finish_exchange()
        .await
        .expect("the exchange finishes");
    assert!(manager.access_token().expose() == "fleet-next-access");
    assert!(
        !store.unresolved_fleet_refresh_status().expect("resolved"),
        "the successor is written and the attempt closed"
    );
    let stored = fleet_snapshot(&store)
        .expect("Fleet row")
        .expect("Fleet credentials");
    assert_ne!(stored.4, Some(input_generation));
    assert_eq!(
        stored.4,
        Some(manager.credential_generation())
    );
    // With nothing in flight the call returns at once and changes nothing.
    manager.finish_exchange().await.expect("nothing in flight");

    server.abort();
    let _ = server.await;
}

// ---------------------------------------------------------------------------
// B2 fix round, P3b.1 (N-B, CL-15): the detached exchange and the retained
// admission. The token endpoint is a double that answers only when the test lets
// it, so the order of every step is an acknowledged one and no test waits on a
// time.
// ---------------------------------------------------------------------------

/// A Fleet token endpoint whose answer is held until `release`, and which says
/// when a request has arrived.
struct GatedAuth {
    api: FleetAuthApi,
    posts: Arc<std::sync::atomic::AtomicUsize>,
    arrived: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl GatedAuth {
    async fn start(timeout: std::time::Duration) -> Self {
        let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let arrived = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake auth listener");
        let address = listener.local_addr().expect("fake auth address");
        let (handler_posts, handler_arrived, handler_release) = (
            Arc::clone(&posts),
            Arc::clone(&arrived),
            Arc::clone(&release),
        );
        let router = Router::new().route(
            "/oauth2/v3/token",
            post(move || {
                let (posts, arrived, release) = (
                    Arc::clone(&handler_posts),
                    Arc::clone(&handler_arrived),
                    Arc::clone(&handler_release),
                );
                async move {
                    posts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    arrived.notify_one();
                    release.notified().await;
                    (
                        StatusCode::OK,
                        [("content-type", "application/json")],
                        r#"{"access_token":"fleet-next-access","refresh_token":"fleet-next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
                    )
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("fake auth server");
        });
        let endpoint =
            url::Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
        let api = FleetAuthApi::for_fake_http(endpoint, timeout).expect("fake Fleet auth client");
        Self {
            api,
            posts,
            arrived,
            release,
            server,
        }
    }

    fn posts(&self) -> usize {
        self.posts.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn stop(self) {
        self.server.abort();
        let _ = self.server.await;
    }
}

/// Fleet credentials that are due, in a private Hub directory.
fn due_fleet_store() -> (tempfile::TempDir, HubStore) {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::legacy_owner_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "fleet-old-access".to_owned(),
        "fleet-old-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        60,
    )
    .expect("setup credentials");
    crate::auth::credential_vault::fixtures::seed_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    (temporary, store)
}

/// What the store holds of the Fleet pair and of its refresh attempts, for an
/// exact before-and-after comparison.
#[derive(Debug, PartialEq, Eq)]
struct FleetLedger {
    access: Vec<u8>,
    refresh: Vec<u8>,
    generation: Option<Uuid>,
    expires_at: i64,
    next_refresh_at: i64,
    /// Receipt id, outcome, whether it is still open, its input generation, its
    /// output generation and whether the input fence is held.
    attempts: Vec<(i64, String, bool, String, Option<String>, bool)>,
}

fn fleet_ledger(store: &HubStore) -> Option<FleetLedger> {
    let stored = fleet_snapshot(&store).expect("Fleet row")?;
    let attempts =(store
        .open()
        .expect("catalogue")
        ).prepare(
            "SELECT r.id, r.outcome, r.completed_at_ms IS NULL,
                    COALESCE(b.input_generation, ''), b.output_generation,
                    EXISTS(SELECT 1 FROM credential_refresh_fences AS f
                            WHERE f.fence_id = b.fence_id)
               FROM outbound_request_receipts AS r
               LEFT JOIN credential_refresh_attempts AS b ON b.receipt_id = r.id
              WHERE r.transport='fleet_api' AND r.operation='token_refresh'
              ORDER BY r.id",
        )
        .expect("ledger query")
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .expect("ledger rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("ledger rows read");
    Some(FleetLedger {
        access: stored.0.as_slice().to_vec(),
        refresh: stored.1.as_slice().to_vec(),
        generation: stored.4,
        expires_at: stored.2,
        next_refresh_at: stored.3,
        attempts,
    })
}

/// Replace the retained lock file, which denies the admission.
fn deny_fleet_admission(directory: &Path) {
    let lock_path = directory.join(crate::user_lifetime_lock::LOCK_FILE_NAME);
    fs::remove_file(&lock_path).expect("remove admitted lock path");
    fs::write(&lock_path, b"").expect("replace admitted lock path");
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
        .expect("replacement lock mode");
}

/// Start a refresh through `manager`, return once the double has the request and
/// drop the caller, as a shutdown that drops the collection future does.
async fn start_and_drop_the_caller(manager: &mut FleetAuthManager, auth: &GatedAuth) {
    tokio::select! {
        biased;
        () = auth.arrived.notified() => {}
        outcome = manager.refresh_now(&auth.api, SystemTime::now()) => {
            panic!("the refresh ended before the request arrived: {outcome:?}");
        }
    }
}

#[tokio::test]
async fn nb_detached_exchange_refuses_persistence_after_admission_loss() {
    let (temporary, store) = due_fleet_store();
    let admission =
        crate::auth::credential_vault::fixtures::fixture_admission(&store, temporary.path());
    let revoked_admission = Arc::downgrade(&admission);
    let mut manager = {
    let runtime_store = store.clone();
    let runtime_path = temporary.path();
    let runtime_admission = admission;
    let runtime_loss = None;
    crate::auth::credential_vault::CredentialVault::open_for_runtime(
        runtime_store.clone(), std::sync::Arc::clone(&runtime_admission), runtime_path, runtime_loss.as_ref(),
    )
    .map_err(crate::fleet_credentials::FleetCredentialError::from)
    .and_then(|vault| FleetAuthManager::from_store_for_admitted_user(vault, runtime_path, runtime_admission, runtime_loss))
}
    .expect("admitted Fleet manager");
    let encryption = manager.observe_exchange_encryption();
    let auth = GatedAuth::start(std::time::Duration::from_secs(5)).await;

    start_and_drop_the_caller(&mut manager, &auth).await;
    let held = fleet_ledger(&store).expect("pair");
    assert_eq!(held.attempts.len(), 1);
    assert_eq!(held.attempts[0].1, "started");
    assert!(held.attempts[0].5, "the input fence is held");
    // The retained admission is denied while the answer is held back; the answer
    // is then released.
    deny_fleet_admission(temporary.path());
    auth.release.notify_one();
    let outcome = manager.finish_exchange().await;

    assert!(
        matches!(
            outcome,
            Err(FleetCredentialError::SensitiveAccessUnavailable)
        ),
        "{outcome:?}"
    );
    assert!(outcome.expect_err("denied").is_admission_loss());
    assert_eq!(
        encryption.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a denied job does not encrypt"
    );
    assert_eq!(
        fleet_ledger(&store).expect("pair"),
        held,
        "ciphertext, generation, schedule, receipt, binding and fence are as they were"
    );
    assert_eq!(auth.posts(), 1);
    // Nothing more is sent, now or from a new manager: the attempt is unresolved.
    assert!(matches!(
        manager.refresh_now(&auth.api, SystemTime::now()).await,
        Err(FleetCredentialError::RotationOutcomeUnknown)
    ));
    drop(manager);
    let store = crate::auth::credential_vault::fixtures::restart_after_revocation(store, temporary.path())
        .expect("restart after revoked process exits");
    assert!(revoked_admission.upgrade().is_none(), "revoked process released");
    let restarted_admission = crate::auth::credential_vault::fixtures::fixture_admission(&store, temporary.path());
    restarted_admission.assert_sensitive_access().expect("new process admission");
    assert!(matches!(
        FleetAuthManager::from_store(store.clone(), temporary.path()),
        Err(FleetCredentialError::RotationOutcomeUnknown)
    ));
    assert!(fleet_ledger(&store).expect("pair after restart") == held, "restart preserves unresolved input");
    assert_eq!(auth.posts(), 1);
    auth.stop().await;
}

/// Hand the manager a loss fence a test sets, as the supervisor's observation of the
/// loss: set, it never clears.
fn loss_fence_for(manager: &mut FleetAuthManager) -> Arc<std::sync::atomic::AtomicBool> {
    let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observer = Arc::clone(&lost);
    manager.share_loss_fence(Arc::new(move || {
        observer.load(std::sync::atomic::Ordering::SeqCst)
    }));
    lost
}

#[tokio::test]
async fn nb_detached_exchange_refuses_persistence_after_the_loss_fence_while_the_admission_passes()
{
    // R-P3bF-1: the supervisor observed the loss while the answer was held back, and
    // the admission, which passes throughout (as a restored one does), cannot lift it.
    let (temporary, store) = due_fleet_store();
    let admission =
        crate::auth::credential_vault::fixtures::fixture_admission(&store, temporary.path());
    let mut manager = {
    let runtime_store = store.clone();
    let runtime_path = temporary.path();
    let runtime_admission = admission;
    let runtime_loss = None;
    crate::auth::credential_vault::CredentialVault::open_for_runtime(
        runtime_store.clone(), std::sync::Arc::clone(&runtime_admission), runtime_path, runtime_loss.as_ref(),
    )
    .map_err(crate::fleet_credentials::FleetCredentialError::from)
    .and_then(|vault| FleetAuthManager::from_store_for_admitted_user(vault, runtime_path, runtime_admission, runtime_loss))
}
    .expect("admitted Fleet manager");
    let lost = loss_fence_for(&mut manager);
    let encryption = manager.observe_exchange_encryption();
    let auth = GatedAuth::start(std::time::Duration::from_secs(5)).await;

    start_and_drop_the_caller(&mut manager, &auth).await;
    let held = fleet_ledger(&store).expect("pair");
    lost.store(true, std::sync::atomic::Ordering::SeqCst);
    auth.release.notify_one();
    let outcome = manager.finish_exchange().await;

    assert!(
        matches!(
            outcome,
            Err(FleetCredentialError::SensitiveAccessUnavailable)
        ),
        "{outcome:?}"
    );
    assert_eq!(
        encryption.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a fenced job does not encrypt"
    );
    assert_eq!(
        fleet_ledger(&store).expect("pair"),
        held,
        "ciphertext, generation, schedule, receipt, binding and fence are as they were"
    );
    assert_eq!(auth.posts(), 1);
    // The manager hands out no token after the fence, and sends nothing.
    assert!(matches!(
        manager.access_token_for_sensitive_use(),
        Err(FleetCredentialError::SensitiveAccessUnavailable)
    ));
    assert!(
        manager
            .refresh_now(&auth.api, SystemTime::now())
            .await
            .is_err()
    );
    assert_eq!(auth.posts(), 1);
    auth.stop().await;
}

#[tokio::test]
async fn nb_detached_failure_receipt_is_not_completed_after_the_loss_fence() {
    // The request never went out (R-P3bF-1: a receipt completion is a store write). With
    // no fence the failure completes its receipt as a transport error and the input is
    // released; with the fence set while the request was out, nothing is written: the
    // receipt stays started and the failure is the sensitive one.
    for fenced in [false, true] {
        let (temporary, store) = due_fleet_store();
        let admission = crate::auth::credential_vault::fixtures::fixture_admission(&store, temporary.path());
        let mut manager = {
    let runtime_store = store.clone();
    let runtime_path = temporary.path();
    let runtime_admission = admission;
    let runtime_loss = None;
    crate::auth::credential_vault::CredentialVault::open_for_runtime(
        runtime_store.clone(), std::sync::Arc::clone(&runtime_admission), runtime_path, runtime_loss.as_ref(),
    )
    .map_err(crate::fleet_credentials::FleetCredentialError::from)
    .and_then(|vault| FleetAuthManager::from_store_for_admitted_user(vault, runtime_path, runtime_admission, runtime_loss))
}
        .expect("admitted Fleet manager");
        let lost = loss_fence_for(&mut manager);
        let api = FleetAuthApi::for_fake_http(
            url::Url::parse("http://127.0.0.1:9/oauth2/v3/token").expect("unused auth URL"),
            std::time::Duration::from_secs(1),
        )
        .expect("Fleet auth client");
        let release = Arc::new(tokio::sync::Notify::new());
        let hold = {
            let release = Arc::clone(&release);
            async move { release.notified().await }
        };
        manager
            .start_scripted_failure(
                &api,
                SystemTime::now(),
                ScriptedFailure {
                    error: FleetApiError::RequestNotSent,
                    hold: Box::pin(hold),
                },
            )
            .expect("the exchange starts");
        let held = fleet_ledger(&store).expect("pair");
        assert_eq!(held.attempts[0].1, "started");
        lost.store(fenced, std::sync::atomic::Ordering::SeqCst);
        release.notify_one();
        let outcome = manager.finish_exchange().await;
        if fenced {
            assert!(
                matches!(
                    outcome,
                    Err(FleetCredentialError::SensitiveAccessUnavailable)
                ),
                "{outcome:?}"
            );
            assert_eq!(
                fleet_ledger(&store).expect("pair"),
                held,
                "nothing was written"
            );
            assert!(store.unresolved_fleet_refresh_status().expect("receipts"));
        } else {
            assert!(
                matches!(
                    outcome,
                    Err(FleetCredentialError::Api(FleetApiError::RequestNotSent))
                ),
                "{outcome:?}"
            );
            let after = fleet_ledger(&store).expect("pair");
            assert_eq!(
                after.attempts[0].1, "transport_error",
                "the receipt is completed"
            );
            assert!(!after.attempts[0].5, "and the input is released");
            assert!(!store.unresolved_fleet_refresh_status().expect("receipts"));
        }
    }
}

#[tokio::test]
async fn nb_detached_exchange_persists_with_admission_intact() {
    let (temporary, store) = due_fleet_store();
    let admission =
        crate::auth::credential_vault::fixtures::fixture_admission(&store, temporary.path());
    let mut manager = {
    let runtime_store = store.clone();
    let runtime_path = temporary.path();
    let runtime_admission = admission;
    let runtime_loss = None;
    crate::auth::credential_vault::CredentialVault::open_for_runtime(
        runtime_store.clone(), std::sync::Arc::clone(&runtime_admission), runtime_path, runtime_loss.as_ref(),
    )
    .map_err(crate::fleet_credentials::FleetCredentialError::from)
    .and_then(|vault| FleetAuthManager::from_store_for_admitted_user(vault, runtime_path, runtime_admission, runtime_loss))
}
    .expect("admitted Fleet manager");
    let encryption = manager.observe_exchange_encryption();
    let auth = GatedAuth::start(std::time::Duration::from_secs(5)).await;
    let input = fleet_ledger(&store).expect("pair");

    start_and_drop_the_caller(&mut manager, &auth).await;
    auth.release.notify_one();
    manager
        .finish_exchange()
        .await
        .expect("the exchange finishes");

    assert_eq!(encryption.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(manager.access_token().expose() == "fleet-next-access");
    let after = fleet_ledger(&store).expect("pair");
    assert_ne!(after.generation, input.generation);
    assert_ne!(after.access, input.access);
    assert_eq!(after.generation, Some(manager.credential_generation()));
    assert_eq!(after.attempts.len(), 1);
    assert_eq!(after.attempts[0].1, "success");
    assert!(!after.attempts[0].2, "the receipt is closed");
    assert!(!store.unresolved_fleet_refresh_status().expect("receipts"));
    assert_eq!(auth.posts(), 1);
    // A second finish has nothing to apply and changes nothing.
    manager.finish_exchange().await.expect("nothing in flight");
    assert_eq!(fleet_ledger(&store).expect("pair"), after);
    auth.stop().await;
}

#[tokio::test]
async fn nb_detached_exchange_cannot_resurrect_removed_credentials() {
    let (temporary, store) = due_fleet_store();
    let mut manager =
        FleetAuthManager::from_store(store.clone(), temporary.path()).expect("Fleet manager");
    let auth = GatedAuth::start(std::time::Duration::from_secs(5)).await;

    start_and_drop_the_caller(&mut manager, &auth).await;
    // The owner signs out while the answer is held back.
    manager.vault.clear_credential_pair(CredentialSlot::FleetApi).expect("sign out");
    auth.release.notify_one();
    let outcome = manager.finish_exchange().await;

    assert!(outcome.is_err(), "{outcome:?}");
    assert!(
        fleet_snapshot(&store).expect("Fleet row").is_none(),
        "the removed credentials are not written back"
    );
    let receipts =(store
        .open()
        .expect("catalogue")
        ).query_row(
            "SELECT r.outcome FROM outbound_request_receipts AS r
               JOIN credential_refresh_attempts AS b ON b.receipt_id = r.id",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("the old receipt");
    assert_eq!(receipts, "cancelled");
    assert!(!store.unresolved_fleet_refresh_status().expect("receipts"));
    assert_eq!(auth.posts(), 1);
    auth.stop().await;
}

#[tokio::test]
async fn nb_detached_exchange_cannot_overwrite_replacement_credentials() {
    let (temporary, store) = due_fleet_store();
    let mut manager =
        FleetAuthManager::from_store(store.clone(), temporary.path()).expect("Fleet manager");
    let auth = GatedAuth::start(std::time::Duration::from_secs(5)).await;

    start_and_drop_the_caller(&mut manager, &auth).await;
    // The owner connects another pair while the answer is held back.
    let mut entry = crate::auth::credential_vault::CredentialEntry::fleet_supplied(Zeroizing::new("fleet-replacement-access".into()),Zeroizing::new("fleet-replacement-refresh".into()),"fleet-client".into(),FleetRegion::EuropeMiddleEastAndAfrica,28800).expect("replacement entry");
    entry.set_schedule(9_000_000_000,8_999_999_000);
    manager.vault.store_supplied_entry(entry,crate::db::ReplacePolicy::ReplaceAny).expect("replacement accepted");
    let replacement = fleet_snapshot(&store).expect("replacement snapshot").expect("replacement row");
    let installed = fleet_ledger(&store).expect("pair");
    auth.release.notify_one();
    let outcome = manager.finish_exchange().await;

    assert!(outcome.is_err(), "{outcome:?}");
    let after = fleet_ledger(&store).expect("pair");
    // The whole ledger is the replacement's: ciphertext, generation, both schedule
    // times and the receipts (R-P3bF-6).
    assert_eq!(
        after, installed,
        "nothing of the replacement was overwritten"
    );
    assert_eq!(after.access, replacement.0.as_slice());
    assert_eq!(after.refresh, replacement.1.as_slice());
    assert_eq!(after.generation, replacement.4);
    assert_eq!(after.generation, installed.generation);
    assert!(
        manager.access_token().expose() == "fleet-old-access",
        "the old successor is never adopted"
    );
    assert_ne!(after.generation, Some(manager.credential_generation()));
    assert!(
        after
            .attempts
            .iter()
            .all(|attempt| attempt.1 == "cancelled")
    );
    assert_eq!(auth.posts(), 1);
    auth.stop().await;
}

#[tokio::test]
async fn r06_sent_timeout_leaves_the_attempt_unresolved() {
    // The request timeout is 200 ms and the double answers only when the test says,
    // after the timeout has ended the call: the answer comes too late to count, and
    // nothing is written for it.
    let (temporary, store) = due_fleet_store();
    let mut manager =
        FleetAuthManager::from_store(store.clone(), temporary.path()).expect("Fleet manager");
    let auth = GatedAuth::start(std::time::Duration::from_millis(200)).await;
    let before = fleet_ledger(&store).expect("pair");

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        manager.refresh_now(&auth.api, SystemTime::now()),
    )
    .await
    .expect("the call ends at the request timeout, well inside two seconds");
    assert!(
        matches!(
            outcome,
            Err(FleetCredentialError::Api(FleetApiError::RequestTimeout))
        ),
        "{outcome:?}"
    );
    assert_eq!(auth.posts(), 1);
    // The answer arrives after the call has ended.
    auth.release.notify_one();
    let after = fleet_ledger(&store).expect("pair");
    assert_eq!(after.access, before.access);
    assert_eq!(after.refresh, before.refresh);
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.next_refresh_at, before.next_refresh_at);
    assert_eq!(after.attempts.len(), 1);
    assert_eq!(after.attempts[0].1, "started");
    assert!(after.attempts[0].5, "the input stays fenced");
    assert!(manager.access_token().expose() == "fleet-old-access");
    // The next refresh is refused as an unknown outcome, with no second request.
    assert!(matches!(
        manager.refresh_now(&auth.api, SystemTime::now()).await,
        Err(FleetCredentialError::RotationOutcomeUnknown)
    ));
    assert_eq!(auth.posts(), 1);
    assert_eq!(fleet_ledger(&store).expect("pair"), after);
    auth.stop().await;
}

#[tokio::test]
async fn p3bf2_the_reservation_and_a_late_denial_write_no_receipt_after_the_loss() {
    // B2 fix round, P3b fix 2, R-P3bF2-1 (Opus N2): the reservation of the single-use
    // input is a store mutation, so the loss gate stands immediately in front of it; and
    // a denial that arrives between the reservation and the job leaves the receipt as it
    // is, with no cancellation written after the loss. The gate is asked three times
    // before the request would go out: by `refresh_now`, before the reservation, and
    // after it. It starts to refuse at the call the scenario names.
    for (refuses_from, reserved) in [(2_usize, false), (3_usize, true)] {
        let (temporary, store) = due_fleet_store();
        let admission = crate::auth::credential_vault::fixtures::fixture_admission(&store, temporary.path());
        let mut manager = {
    let runtime_store = store.clone();
    let runtime_path = temporary.path();
    let runtime_admission = admission;
    let runtime_loss = None;
    crate::auth::credential_vault::CredentialVault::open_for_runtime(
        runtime_store.clone(), std::sync::Arc::clone(&runtime_admission), runtime_path, runtime_loss.as_ref(),
    )
    .map_err(crate::fleet_credentials::FleetCredentialError::from)
    .and_then(|vault| FleetAuthManager::from_store_for_admitted_user(vault, runtime_path, runtime_admission, runtime_loss))
}
        .expect("admitted Fleet manager");
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        manager.share_loss_fence(Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 >= refuses_from
        }));
        let auth = GatedAuth::start(std::time::Duration::from_secs(5)).await;
        let before = fleet_ledger(&store).expect("pair");
        assert!(before.attempts.is_empty());

        let outcome = manager.refresh_now(&auth.api, SystemTime::now()).await;

        assert!(
            matches!(
                outcome,
                Err(FleetCredentialError::SensitiveAccessUnavailable)
            ),
            "refusing from call {refuses_from}: {outcome:?}"
        );
        assert_eq!(auth.posts(), 0, "the request never went out");
        let after = fleet_ledger(&store).expect("pair");
        if reserved {
            assert_eq!(after.attempts.len(), 1, "the reservation was made");
            assert_eq!(
                after.attempts[0].1, "started",
                "and left as it is: no cancellation after the loss"
            );
            assert!(after.attempts[0].5, "the input fence is held");
        } else {
            assert_eq!(after, before, "no reservation, no cancellation, nothing");
        }
        auth.stop().await;
    }
}

type FleetSnapshot = (Vec<u8>,Vec<u8>,i64,i64,Option<Uuid>,String,String);
/// Test-owned ciphertext observation, never used to hand credentials to a manager.
fn fleet_snapshot(store: &HubStore) -> Result<Option<FleetSnapshot>,StoreError> {
    use rusqlite::OptionalExtension;
    store.open()?.query_row("SELECT access_envelope,refresh_envelope,expires_at,next_refresh_at,generation,client_id,region FROM credential_pairs WHERE account_id=1 AND slot='fleet_api'",[],|r| {
        let generation: String = r.get(4)?;
        Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,Uuid::parse_str(&generation).ok(),r.get(5)?,r.get(6)?))
    }).optional().map_err(StoreError::Query)
}
