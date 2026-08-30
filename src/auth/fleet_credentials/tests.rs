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
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        TEST_SCOPED_ACCESS.to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("setup credentials");
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist");

    let stored = store.load_fleet_tokens().expect("store").expect("row");
    assert!(
        !stored
            .access()
            .windows(TEST_SCOPED_ACCESS.len())
            .any(|part| part == TEST_SCOPED_ACCESS.as_bytes())
    );
    assert!(
        !stored
            .refresh()
            .windows(13)
            .any(|part| part == b"fleet-refresh")
    );
    validate_stored_fleet_credentials(&store, temporary.path()).expect("read-only validation");
    let manager = FleetAuthManager::from_store(store, temporary.path()).expect("manager");
    assert_eq!(manager.access_token().expose(), TEST_SCOPED_ACCESS);
    assert_eq!(manager.region(), FleetRegion::EuropeMiddleEastAndAfrica);
    let rendered = format!("{credentials:?} {manager:?} {stored:?}");
    assert!(!rendered.contains(TEST_SCOPED_ACCESS));
    assert!(!rendered.contains("fleet-refresh"));
    assert!(!rendered.contains("fleet-client"));
}

#[test]
fn stored_fleet_credentials_require_collection_scopes() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig".to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("syntactically valid credentials");
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist");

    assert!(matches!(
        validate_stored_fleet_credentials(&store, temporary.path()),
        Err(FleetCredentialError::MissingCollectionScopes)
    ));
}

#[test]
fn schema_55_cursor_encrypted_row_upgrades_and_scrubs_live_sqlite_traces() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let cursor = load_existing_cursor_key_bytes(temporary.path())
        .expect("cursor read")
        .expect("cursor bytes");
    let cursor: [u8; FLEET_KEY_BYTES] = cursor.as_slice().try_into().expect("cursor key length");
    let legacy_key = Zeroizing::new(
        CursorKey::from_bytes(cursor)
            .fleet_credential_encryption_key()
            .to_vec(),
    );
    let plaintext = OwnerTokens::from_secret_parts(
        "legacy-fleet-access".to_owned(),
        "legacy-fleet-refresh".to_owned(),
    )
    .expect("legacy plaintext");
    let generation = crate::teslamate_token::legacy_refresh_credential_generation(&plaintext);
    let (access, refresh) =
        encrypt_legacy_owner_tokens(&legacy_key, &plaintext).expect("legacy encryption");
    let old_access = access.clone();
    let old_refresh = refresh.clone();
    let stored = FleetTokenStore::new(
        access,
        refresh,
        "legacy-fleet-client".to_owned(),
        "eu".to_owned(),
        2_000_000_000,
        1_900_000_000,
        Some(generation),
    )
    .expect("schema-55 Fleet row");
    store
        .replace_fleet_tokens(&stored)
        .expect("legacy Fleet row persists");

    let catalogue_before = fs::read(temporary.path().join("hub.sqlite")).expect("catalogue");
    let immutable =
        HubStore::open_immutable_read_only(temporary.path()).expect("immutable preflight store");
    assert!(matches!(
        validate_stored_fleet_credentials(&immutable, temporary.path()),
        Err(FleetCredentialError::MigrationRequired)
    ));
    immutable
        .verify_immutable_snapshot_unchanged()
        .expect("preflight stayed byte stable");
    assert_eq!(
        fs::read(temporary.path().join("hub.sqlite")).expect("catalogue after preflight"),
        catalogue_before
    );
    assert!(!fleet_key_path(temporary.path()).exists());
    let still_legacy = store
        .load_fleet_tokens()
        .expect("legacy row remains")
        .expect("legacy credentials remain");
    assert_eq!(still_legacy.access(), old_access.as_slice());
    assert_eq!(still_legacy.refresh(), old_refresh.as_slice());
    assert!(matches!(
        FleetAuthManager::from_store(store.clone(), temporary.path()),
        Err(FleetCredentialError::MigrationRequired)
    ));

    assert!(
        migrate_legacy_fleet_credentials(&store, temporary.path()).expect("bootstrap migration")
    );
    let manager =
        FleetAuthManager::from_store(store.clone(), temporary.path()).expect("migrated row loads");
    assert_eq!(manager.access_token().expose(), "legacy-fleet-access");
    let dedicated = load_existing_fleet_key_bytes(temporary.path())
        .expect("dedicated key read")
        .expect("dedicated key exists");
    assert_ne!(dedicated.as_slice(), legacy_key.as_slice());
    assert_eq!(
        fs::metadata(fleet_key_path(temporary.path()))
            .expect("Fleet key metadata")
            .permissions()
            .mode()
            & 0o777,
        PRIVATE_FILE_MODE
    );
    let upgraded = store
        .load_fleet_tokens()
        .expect("upgraded row")
        .expect("upgraded credentials");
    let decrypted = decrypt_legacy_owner_tokens(&dedicated, upgraded.access(), upgraded.refresh())
        .expect("dedicated key decrypts upgraded row");
    assert_eq!(decrypted.access_token(), "legacy-fleet-access");
    assert!(
        decrypt_legacy_owner_tokens(&legacy_key, upgraded.access(), upgraded.refresh()).is_err()
    );
    assert!(!migration_marker_path(temporary.path()).exists());

    for path in [
        temporary.path().join("hub.sqlite"),
        temporary.path().join("hub.sqlite-wal"),
    ] {
        if let Ok(bytes) = fs::read(path) {
            assert!(
                !bytes
                    .windows(old_access.len())
                    .any(|part| part == old_access)
            );
            assert!(
                !bytes
                    .windows(old_refresh.len())
                    .any(|part| part == old_refresh)
            );
        }
    }
}

#[test]
fn signout_deletes_fleet_key_and_preserves_cursor_authority() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let cursor_before = load_existing_cursor_key_bytes(temporary.path())
        .expect("cursor read")
        .expect("cursor bytes");
    let credentials = FleetSetupCredentials::new(
        "fleet-signout-access".to_owned(),
        "fleet-signout-refresh".to_owned(),
        "fleet-signout-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("setup credentials");
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist Fleet credentials");
    let stored = store
        .load_fleet_tokens()
        .expect("stored Fleet row")
        .expect("Fleet credentials");
    let cursor: [u8; FLEET_KEY_BYTES] = cursor_before
        .as_slice()
        .try_into()
        .expect("cursor key length");
    let cursor_derived = Zeroizing::new(
        CursorKey::from_bytes(cursor)
            .fleet_credential_encryption_key()
            .to_vec(),
    );
    assert!(
        decrypt_legacy_owner_tokens(&cursor_derived, stored.access(), stored.refresh()).is_err()
    );
    assert!(fleet_key_path(temporary.path()).exists());

    remove_fleet_key_and_tokens(temporary.path(), &store).expect("Fleet signout");

    assert!(store.load_fleet_tokens().expect("Fleet row").is_none());
    assert!(
        load_existing_fleet_key_bytes(temporary.path())
            .expect("Fleet key absence")
            .is_none()
    );
    assert_eq!(
        load_existing_cursor_key_bytes(temporary.path())
            .expect("cursor read after signout")
            .expect("cursor remains")
            .as_slice(),
        cursor_before.as_slice()
    );
}

#[tokio::test]
async fn due_refresh_rotates_encrypted_generation_receipt_and_restart_state() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "fleet-old-access".to_owned(),
        "fleet-old-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        60,
    )
    .expect("setup credentials");
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    let initial = store
        .load_fleet_tokens()
        .expect("initial Fleet row")
        .expect("initial Fleet credentials");
    let input_generation = initial
        .credential_generation()
        .expect("initial credential generation");
    let initial_access_ciphertext = initial.access().to_vec();
    let initial_refresh_ciphertext = initial.refresh().to_vec();

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

    let rotated = store
        .load_fleet_tokens()
        .expect("rotated Fleet row")
        .expect("rotated Fleet credentials");
    let output_generation = rotated
        .credential_generation()
        .expect("rotated credential generation");
    assert_ne!(output_generation, input_generation);
    assert_ne!(rotated.access(), initial_access_ciphertext.as_slice());
    assert_ne!(rotated.refresh(), initial_refresh_ciphertext.as_slice());
    let encryption_key = load_existing_fleet_key_bytes(temporary.path())
        .expect("Fleet encryption key")
        .expect("Fleet key file");
    let successor =
        decrypt_legacy_owner_tokens(&encryption_key, rotated.access(), rotated.refresh())
            .expect("decrypt rotated credentials");
    assert!(successor.access_token() == "fleet-next-access");
    assert!(successor.refresh_token() == "fleet-next-refresh");

    let receipt = store
        .open()
        .expect("receipt catalogue")
        .query_row(
            "SELECT r.transport, r.operation, r.safety_class, r.precondition,
                    r.outcome, r.http_status, r.completed_at_ms IS NOT NULL,
                    b.input_credential_generation, b.output_credential_generation
               FROM outbound_request_receipts AS r
               JOIN fleet_refresh_receipt_bindings AS b ON b.receipt_id = r.id
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
        !store
            .has_unresolved_fleet_refresh()
            .expect("resolved refresh")
    );

    drop(manager);
    drop(store);
    let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
    assert!(
        !restarted
            .has_unresolved_fleet_refresh()
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
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "fleet-old-access".to_owned(),
        "fleet-old-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        60,
    )
    .expect("setup credentials");
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    let input_generation = store
        .load_fleet_tokens()
        .expect("Fleet row")
        .expect("Fleet credentials")
        .credential_generation()
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
        store
            .has_unresolved_fleet_refresh()
            .expect("fenced failure")
    );
    assert_eq!(
        store
            .load_fleet_tokens()
            .expect("Fleet row")
            .expect("Fleet credentials")
            .credential_generation(),
        Some(input_generation)
    );

    drop(manager);
    drop(store);
    let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
    assert!(restarted.has_unresolved_fleet_refresh().unwrap());
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
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        "fleet-admission-access".to_owned(),
        "fleet-admission-refresh".to_owned(),
        "fleet-admission-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        60,
    )
    .expect("setup credentials");
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist credentials");
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let mut manager =
        FleetAuthManager::from_store_for_admitted_user(store.clone(), temporary.path(), admission)
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
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .expect("replacement lock mode");

    assert!(matches!(
        manager.refresh_if_due(&api, SystemTime::now()).await,
        Err(FleetCredentialError::SensitiveAccessUnavailable)
    ));
    assert_eq!(*requests.lock().expect("request ledger"), 0);
    assert!(
        !store
            .has_unresolved_fleet_refresh()
            .expect("no refresh began")
    );

    server.abort();
    let _ = server.await;
}
