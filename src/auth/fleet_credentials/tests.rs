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
        panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency");
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
fn operational_validation_rejects_a_refresh_that_begins_after_its_first_check() {
    // A refresh can begin between the operational predicate's first check and
    // the inspection. The inspection then reports it unresolved, and the
    // operational answer must be the fenced one, never success.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::legacy_owner_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
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
    let generation = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
        .expect("store")
        .expect("row")
        .4
        .expect("bound generation");
    // Control: with nothing in flight the seam changes nothing.
    validate_stored_fleet_credentials_after(&store, temporary.path(), || {})
        .expect("valid without a refresh");
    assert!(!({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("receipts"));
    // The refresh begins exactly in the window.
    let result = validate_stored_fleet_credentials_after(&store, temporary.path(), || {
        ({ let unavailable_value: Result<crate::db::OutboundRequestReceiptId, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
            .expect("refresh begins");
    });
    assert!(
        matches!(result, Err(FleetCredentialError::RotationOutcomeUnknown)),
        "{result:?}"
    );
    assert!(
        inspect_stored_fleet_credentials(&store, temporary.path())
            .expect("inspection")
            .unresolved_refresh
    );
    assert!(matches!(
        validate_stored_fleet_credentials(&store, temporary.path()),
        Err(FleetCredentialError::RotationOutcomeUnknown)
    ));
}

#[test]
fn schema_55_cursor_encrypted_row_upgrades_and_scrubs_live_sqlite_traces() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    crate::legacy_owner_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    let cursor = load_existing_cursor_key_bytes(temporary.path())
        .expect("cursor read")
        .expect("cursor bytes");
    let cursor: [u8; 32] = cursor.as_slice().try_into().expect("cursor key length");
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
    let generation = crate::source_token_envelope::legacy_refresh_credential_generation(&plaintext);
    let (access, refresh) =
        ({ let unavailable_value: Result<(Vec<u8>, Vec<u8>), crate::source_token_envelope::SourceDbTokenError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("legacy encryption");
    let old_access = access.clone();
    let old_refresh = refresh.clone();
    let stored = ({ let unavailable_value: Result<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
    .expect("schema-55 Fleet row");
    ({ let unavailable_value: Result<(), crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
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
    assert!(!({ let unavailable_value: std::path::PathBuf = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).exists());
    let still_legacy = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
        .expect("legacy row remains")
        .expect("legacy credentials remain");
    assert_eq!(still_legacy.0.as_slice(), old_access.as_slice());
    assert_eq!(still_legacy.1.as_slice(), old_refresh.as_slice());
    assert!(matches!(
        FleetAuthManager::from_store(store.clone(), temporary.path()),
        Err(FleetCredentialError::MigrationRequired)
    ));

    assert!(
        ({ let unavailable_value: Result<bool, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("bootstrap migration")
    );
    let manager =
        FleetAuthManager::from_store(store.clone(), temporary.path()).expect("migrated row loads");
    assert_eq!(manager.access_token().expose(), "legacy-fleet-access");
    let dedicated = ({ let unavailable_value: Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
        .expect("dedicated key read")
        .expect("dedicated key exists");
    assert_ne!(dedicated.as_slice(), legacy_key.as_slice());
    assert_eq!(
        fs::metadata(({ let unavailable_value: std::path::PathBuf = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }))
            .expect("Fleet key metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let upgraded = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
        .expect("upgraded row")
        .expect("upgraded credentials");
    let decrypted = decrypt_legacy_owner_tokens(&dedicated, upgraded.0.as_slice(), upgraded.1.as_slice())
        .expect("dedicated key decrypts upgraded row");
    assert_eq!(decrypted.access_token(), "legacy-fleet-access");
    assert!(
        decrypt_legacy_owner_tokens(&legacy_key, upgraded.0.as_slice(), upgraded.1.as_slice()).is_err()
    );
    assert!(!({ let unavailable_value: std::path::PathBuf = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).exists());

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
    crate::legacy_owner_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
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
    let stored = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
        .expect("stored Fleet row")
        .expect("Fleet credentials");
    let cursor: [u8; 32] = cursor_before
        .as_slice()
        .try_into()
        .expect("cursor key length");
    let cursor_derived = Zeroizing::new(
        CursorKey::from_bytes(cursor)
            .fleet_credential_encryption_key()
            .to_vec(),
    );
    assert!(
        decrypt_legacy_owner_tokens(&cursor_derived, stored.0.as_slice(), stored.1.as_slice()).is_err()
    );
    assert!(({ let unavailable_value: std::path::PathBuf = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).exists());

    ({ let unavailable_value: Result<(), crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("Fleet signout");

    assert!(({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("Fleet row").is_none());
    assert!(
        ({ let unavailable_value: Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
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
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    let initial = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
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

    let rotated = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
        .expect("rotated Fleet row")
        .expect("rotated Fleet credentials");
    let output_generation = rotated
        .4
        .expect("rotated credential generation");
    assert_ne!(output_generation, input_generation);
    assert_ne!(rotated.0.as_slice(), initial_access_ciphertext.as_slice());
    assert_ne!(rotated.1.as_slice(), initial_refresh_ciphertext.as_slice());
    let encryption_key = ({ let unavailable_value: Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
        .expect("Fleet encryption key")
        .expect("Fleet key file");
    let successor =
        decrypt_legacy_owner_tokens(&encryption_key, rotated.0.as_slice(), rotated.1.as_slice())
            .expect("decrypt rotated credentials");
    assert!(successor.access_token() == "fleet-next-access");
    assert!(successor.refresh_token() == "fleet-next-refresh");

    let receipt =({ let _retained_query_context = &(store
        .open()
        .expect("receipt catalogue")
        ); let unavailable_value: &rusqlite::Connection = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).query_row(
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
        !({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
            .expect("resolved refresh")
    );

    drop(manager);
    drop(store);
    let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
    assert!(
        !({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
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
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    let input_generation = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
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
        ({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
            .expect("fenced failure")
    );
    assert_eq!(
        ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
            .expect("Fleet row")
            .expect("Fleet credentials")
            .4,
        Some(input_generation)
    );

    drop(manager);
    drop(store);
    let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
    assert!(({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).unwrap());
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
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist credentials");
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let mut manager = FleetAuthManager::from_store_for_admitted_user(
        store.clone(),
        temporary.path(),
        admission,
        None,
    )
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
        !({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value })
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
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist due credentials");
    let input_generation = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
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
        ({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("receipt"),
        "the attempt is still open while Tesla has not answered"
    );

    // A graceful shutdown waits for the exchange and applies its outcome.
    manager
        .finish_exchange()
        .await
        .expect("the exchange finishes");
    assert!(manager.access_token().expose() == "fleet-next-access");
    assert!(
        !({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("resolved"),
        "the successor is written and the attempt closed"
    );
    let stored = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
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
    persist_fleet_setup_credentials(
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
    let stored = ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("Fleet row")?;
    let attempts =({ let _retained_query_context = &(store
        .open()
        .expect("catalogue")
        ); let unavailable_value: &rusqlite::Connection = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).prepare(
            "SELECT r.id, r.outcome, r.completed_at_ms IS NULL,
                    b.input_credential_generation, b.output_credential_generation,
                    EXISTS(SELECT 1 FROM fleet_refresh_input_fences AS f
                            WHERE f.input_credential_generation = b.input_credential_generation)
               FROM outbound_request_receipts AS r
               JOIN fleet_refresh_receipt_bindings AS b ON b.receipt_id = r.id
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
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let mut manager = FleetAuthManager::from_store_for_admitted_user(
        store.clone(),
        temporary.path(),
        admission,
        None,
    )
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
    assert!(matches!(
        FleetAuthManager::from_store(store.clone(), temporary.path()),
        Err(FleetCredentialError::RotationOutcomeUnknown)
    ));
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
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let mut manager = FleetAuthManager::from_store_for_admitted_user(
        store.clone(),
        temporary.path(),
        admission,
        None,
    )
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
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let mut manager = FleetAuthManager::from_store_for_admitted_user(
            store.clone(),
            temporary.path(),
            admission,
            None,
        )
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
            assert!(({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value }).expect("receipts"));
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
            assert!(!({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value }).expect("receipts"));
        }
    }
}

#[tokio::test]
async fn nb_detached_exchange_persists_with_admission_intact() {
    let (temporary, store) = due_fleet_store();
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let mut manager = FleetAuthManager::from_store_for_admitted_user(
        store.clone(),
        temporary.path(),
        admission,
        None,
    )
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
    assert!(!({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value }).expect("receipts"));
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
    ({ let unavailable_value: Result<(), crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("sign out");
    auth.release.notify_one();
    let outcome = manager.finish_exchange().await;

    assert!(outcome.is_err(), "{outcome:?}");
    assert!(
        ({ let unavailable_value: Result<Option<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String)>, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("Fleet row").is_none(),
        "the removed credentials are not written back"
    );
    let receipts =({ let _retained_query_context = &(store
        .open()
        .expect("catalogue")
        ); let unavailable_value: &rusqlite::Connection = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).query_row(
            "SELECT r.outcome FROM outbound_request_receipts AS r
               JOIN fleet_refresh_receipt_bindings AS b ON b.receipt_id = r.id",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("the old receipt");
    assert_eq!(receipts, "cancelled");
    assert!(!({ let unavailable_value: Result<bool, crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value }).expect("receipts"));
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
    let key = ({ let unavailable_value: Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::fleet_credentials::FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
        .expect("key")
        .expect("Fleet key");
    let replacement = ({ let encrypt_store: fn(&[u8], &str, &str, &str, FleetRegion, FleetRefreshSchedule) -> Result<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), FleetCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); encrypt_store(
        &key,
        "fleet-replacement-access",
        "fleet-replacement-refresh",
        "fleet-client",
        FleetRegion::EuropeMiddleEastAndAfrica,
        FleetRefreshSchedule {
            expires_at: 9_000_000_000,
            next_refresh_at: 8_999_999_000,
        },
    ) })
    .expect("replacement row");
    ({ let unavailable_value: Result<(), crate::db::StoreError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential table dependency"); unavailable_value })
        .expect("replacement is accepted");
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
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let mut manager = FleetAuthManager::from_store_for_admitted_user(
            store.clone(),
            temporary.path(),
            admission,
            None,
        )
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
