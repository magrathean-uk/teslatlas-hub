// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

#[path = "refresh_tests.rs"]
mod refresh_tests;

#[test]
fn owner_tokens_are_redacted_and_zeroizable_on_drop() {
    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}

    let access = "owner-access-secret";
    let refresh = "owner-refresh-secret";
    let mut tokens = OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned()).unwrap();
    let debug = format!("{tokens:?}");
    assert!(!debug.contains(access));
    assert!(!debug.contains(refresh));
    assert_zeroize_on_drop::<OwnerTokens>();

    tokens.zeroize();
    assert!(tokens.access_token().bytes().all(|byte| byte == 0));
    assert!(tokens.refresh_token().bytes().all(|byte| byte == 0));
}

#[test]
fn owner_token_files_accept_one_line_ending_and_enforce_semantic_bounds() {
    let access = [vec![b'a'; MAX_TOKEN_BYTES], b"\r\n".to_vec()].concat();
    let refresh = [b"refresh".as_slice(), b"\n".as_slice()].concat();
    let tokens = OwnerTokens::from_file_bytes(Zeroizing::new(access), Zeroizing::new(refresh))
        .expect("bounded token files");
    assert_eq!(tokens.access_token().len(), MAX_TOKEN_BYTES);
    assert_eq!(tokens.refresh_token(), "refresh");

    assert!(matches!(
        OwnerTokens::from_file_bytes(
            Zeroizing::new(vec![b'a'; MAX_TOKEN_BYTES + 1]),
            Zeroizing::new(b"refresh".to_vec()),
        ),
        Err(CredentialError::TokenTooLarge)
    ));
    assert!(matches!(
        OwnerTokens::from_file_bytes(
            Zeroizing::new(vec![0xff]),
            Zeroizing::new(b"refresh".to_vec()),
        ),
        Err(CredentialError::InvalidTokenBytes)
    ));
}

#[tokio::test]
async fn observer_never_posts_a_refresh_token() {
    let data = crate::private_tempdir().expect("data directory");
    let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
    crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key").expect("private key");
    let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
    let tokens =
        OwnerTokens::from_secret_parts("observer-access".to_owned(), "observer-refresh".to_owned())
            .expect("observer tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
            .expect("encrypt observer tokens");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(
                access,
                refresh,
                2_000_000_000,
                1_900_000_000,
            )
            .expect("schedule"),
        )
        .expect("store observer tokens");

    let fake =
        crate::fake_tesla::FakeTeslaSource::spawn_canonical(crate::fake_tesla::AdvanceMode::Manual)
            .await
            .expect("fake Tesla");
    let mut manager = LegacyAuthManager::from_hub_teslamate_store_observer_with_issuer(
        store,
        data.path(),
        fake.oauth_issuer_url(),
    )
    .expect("load observer");

    crate::crypto::install_default_provider();
    manager
        .refresh_if_due(&Client::new(), SystemTime::now())
        .await
        .expect("observer skips scheduled refresh");
    assert!(matches!(
        manager.refresh_now(&Client::new(), SystemTime::now()).await,
        Err(LegacyAuthManagerError::ObserverRefreshDisabled)
    ));
    assert_eq!(fake.token_refresh_request_count(), 0);
    fake.shutdown().await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn replaced_runtime_admission_blocks_refresh_before_token_transport() {
    let root = crate::private_tempdir().expect("test root");
    let data_dir = root.path().join("data");
    std::fs::create_dir(&data_dir).expect("data directory");
    std::fs::set_permissions(
        &data_dir,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("private data directory");
    let admission = crate::hub_user_process::AdmittedUserHub::for_test(&data_dir)
        .expect("admit data directory");
    let fake =
        crate::fake_tesla::FakeTeslaSource::spawn_canonical(crate::fake_tesla::AdvanceMode::Manual)
            .await
            .expect("fake Tesla");
    let auth = LegacyAuth::for_test(
        fake.oauth_issuer_url(),
        "admitted-access",
        "admitted-refresh",
    );
    let mut manager = LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())))
        .with_runtime_admission(admission);

    std::fs::rename(&data_dir, root.path().join("replaced-data"))
        .expect("replace admitted directory");
    std::fs::create_dir(&data_dir).expect("replacement directory");

    crate::crypto::install_default_provider();
    assert!(matches!(
        manager.refresh_now(&Client::new(), SystemTime::now()).await,
        Err(LegacyAuthManagerError::Credential(
            CredentialError::SensitiveAccessUnavailable
        ))
    ));
    assert_eq!(fake.token_refresh_request_count(), 0);
    fake.shutdown().await;
}

#[tokio::test]
async fn hub_teslamate_store_refreshes_and_reopens_with_the_successor() {
    let data = crate::private_tempdir().expect("data directory");
    let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
    crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key").expect("private key");
    let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
    let initial =
        OwnerTokens::from_secret_parts("initial-access".to_owned(), "initial-refresh".to_owned())
            .expect("initial tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("encrypt initial tokens");
    let stored = crate::db::TeslaMateLegacyTokenStore::refreshed(
        access,
        refresh,
        2_000_000_000,
        1_900_000_000,
    )
    .expect("initial schedule");
    store
        .replace_teslamate_legacy_tokens(&stored)
        .expect("store initial pair");

    let fake =
        crate::fake_tesla::FakeTeslaSource::spawn_canonical(crate::fake_tesla::AdvanceMode::Manual)
            .await
            .expect("fake Tesla");
    let mut manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        data.path(),
        fake.oauth_issuer_url(),
    )
    .expect("load initial pair");
    let mut stale_manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        data.path(),
        fake.oauth_issuer_url(),
    )
    .expect("load second initial authority");
    crate::crypto::install_default_provider();
    manager
        .refresh_now(&Client::new(), SystemTime::now())
        .await
        .expect("refresh and persist successor");
    assert_eq!(fake.token_refresh_request_count(), 1);
    let expected_expires = manager.auth.expires_at();
    let expected_next_refresh = manager.next_refresh_at();

    let stored = store
        .load_teslamate_legacy_tokens()
        .expect("load stored pair")
        .expect("stored pair");
    let decrypted = crate::teslamate_token::decrypt_legacy_owner_tokens(
        key.as_bytes(),
        stored.access(),
        stored.refresh(),
    )
    .expect("decrypt successor");
    assert_eq!(
        decrypted.access_token(),
        crate::fake_tesla::FAKE_REFRESHED_ACCESS_TOKEN
    );
    assert_eq!(
        decrypted.refresh_token(),
        crate::fake_tesla::FAKE_REFRESHED_REFRESH_TOKEN
    );
    assert_eq!(stored.expires_at(), expected_expires);
    assert_eq!(stored.next_refresh_at(), expected_next_refresh);

    // C5 R44a, R62: the second manager finds the row changed by the first one's
    // refresh. That is a pair the unit itself wrote, so it loads it and sends
    // nothing; it is not a sign-out.
    stale_manager
        .refresh_now(&Client::new(), SystemTime::now())
        .await
        .expect("the stale authority adopts the successor");
    assert_eq!(
        stale_manager.access_token(),
        crate::fake_tesla::FAKE_REFRESHED_ACCESS_TOKEN
    );
    assert_eq!(stale_manager.generation(), manager.generation());
    assert_eq!(fake.token_refresh_request_count(), 1);
    assert!(
        !store
            .has_unresolved_legacy_refresh()
            .expect("stale authority creates no receipt")
    );
    assert_eq!(
        store
            .load_teslamate_legacy_tokens()
            .expect("load successor after stale attempt")
            .expect("successor remains")
            .credential_generation(),
        stored.credential_generation()
    );

    let reopened = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store,
        data.path(),
        fake.oauth_issuer_url(),
    )
    .expect("reopen successor");
    assert_eq!(
        reopened.access_token(),
        crate::fake_tesla::FAKE_REFRESHED_ACCESS_TOKEN
    );
    assert_eq!(
        reopened.refresh_token(),
        crate::fake_tesla::FAKE_REFRESHED_REFRESH_TOKEN
    );
    assert_eq!(reopened.next_refresh_at(), expected_next_refresh);
}

#[tokio::test]
async fn runtime_replacement_after_refresh_receipt_begin_cancels_before_token_post() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let data = crate::private_tempdir().expect("data directory");
    let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
    crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key").expect("private key");
    let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
    let initial =
        OwnerTokens::from_secret_parts("initial-access".to_owned(), "initial-refresh".to_owned())
            .expect("initial tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("encrypt initial tokens");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(
                access,
                refresh,
                2_000_000_000,
                1_900_000_000,
            )
            .expect("initial schedule"),
        )
        .expect("store initial pair");

    let fake =
        crate::fake_tesla::FakeTeslaSource::spawn_canonical(crate::fake_tesla::AdvanceMode::Manual)
            .await
            .expect("fake Tesla");
    let mut manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        data.path(),
        fake.oauth_issuer_url(),
    )
    .expect("load initial pair");
    let checks = Arc::new(AtomicUsize::new(0));
    let guarded_checks = Arc::clone(&checks);
    manager.sensitive_access = Arc::new(move || {
        // The check before the attempt passes; the one after the reservation
        // (C5 R17) does not.
        if guarded_checks.fetch_add(1, Ordering::SeqCst) < 1 {
            Ok(())
        } else {
            Err(CredentialError::SensitiveAccessUnavailable)
        }
    });

    crate::crypto::install_default_provider();
    assert!(matches!(
        manager.refresh_now(&Client::new(), SystemTime::now()).await,
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::SensitiveAccessUnavailable
        ))
    ));
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    assert_eq!(fake.token_refresh_request_count(), 0);
    assert!(
        !store
            .has_unresolved_legacy_refresh()
            .expect("cancelled receipt is terminal")
    );
    let generation = store
        .load_teslamate_legacy_tokens()
        .expect("tokens load")
        .expect("tokens remain")
        .credential_generation()
        .expect("generation remains");
    let receipt = store
        .begin_legacy_refresh(generation)
        .expect("input remains refreshable");
    store
        .cancel_unsent_legacy_refresh(receipt, generation)
        .expect("test cleanup");
    fake.shutdown().await;
}

#[tokio::test]
async fn post_send_refresh_failure_is_terminal_until_explicit_new_credentials() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let data = crate::private_tempdir().expect("data directory");
    let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
    crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key").expect("private key");
    let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
    let initial = OwnerTokens::from_secret_parts(
        "ambiguous-access".to_owned(),
        "ambiguous-refresh".to_owned(),
    )
    .expect("initial tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("encrypt initial tokens");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(
                access,
                refresh,
                2_000_000_000,
                1_900_000_000,
            )
            .expect("stored initial pair"),
        )
        .expect("persist initial pair");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback refresh listener");
    let address = listener.local_addr().expect("loopback address");
    let response_task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("refresh request");
        let mut request = vec![0_u8; 16 * 1024];
        let read = socket
            .read(&mut request)
            .await
            .expect("read refresh request");
        assert!(read > 0);
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
            )
            .await
            .expect("malformed success response");
    });
    let issuer = url::Url::parse(&format!("http://{address}/")).expect("loopback issuer");
    let mut manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        data.path(),
        issuer.clone(),
    )
    .expect("load initial pair");
    crate::crypto::install_default_provider();
    // C5 R45 row 4: a grant that cannot be used leaves the attempt unresolved
    // and the account in `sign_in_required`; no token request follows.
    assert!(matches!(
        manager.refresh_now(&Client::new(), SystemTime::now()).await,
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::InvalidResponse
        ))
    ));
    response_task.await.expect("response task");
    // C5-S2: the account is in `sign_in_required`, which stops token requests only
    // (R43). A forced refresh is told that none is possible, which is a wait for
    // the caller and not the terminal error of a sign-out the state does not hold
    // (R56): the access token is still usable here.
    let refused = manager
        .refresh_now(&Client::new(), SystemTime::now())
        .await
        .expect_err("no token request follows");
    assert!(
        matches!(
            refused,
            LegacyAuthManagerError::Auth(LegacyAuthError::RefreshNotPossible)
        ),
        "{refused:?}"
    );
    assert!(!refused.is_terminal_auth_failure());
    assert_eq!(
        manager.status_now().state,
        crate::legacy_auth::state::AuthState::SignInRequired
    );
    // A restart builds the manager in its stopped state (C5-B2, R57 step 3): it
    // sends nothing, hands out no token and says why, so the collector can start,
    // show the state and wait for the owner.
    let restarted = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        data.path(),
        issuer.clone(),
    )
    .expect("a stopped manager");
    assert!(matches!(
        restarted.access_token_for_sensitive_use(),
        Err(LegacyAuthManagerError::Auth(LegacyAuthError::SignedOut {
            reason: crate::legacy_auth::SignOutReason::RefreshOutcomeUnknown
        }))
    ));
    let status = restarted.status_now();
    assert_eq!(
        status.state,
        crate::legacy_auth::state::AuthState::SignedOut
    );
    assert!(status.state.lease_is_terminal());
    drop(restarted);

    let replacement =
        OwnerTokens::from_secret_parts("fresh-access".to_owned(), "fresh-refresh".to_owned())
            .expect("explicit replacement tokens");
    let replacement_generation =
        crate::teslamate_token::legacy_refresh_credential_generation(&replacement);
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &replacement)
            .expect("encrypt replacement tokens");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(
                access,
                refresh,
                2_100_000_000,
                2_000_000_000,
            )
            .expect("stored replacement pair")
            .with_credential_generation(replacement_generation)
            .expect("replacement generation"),
        )
        .expect("explicit replacement supersedes ambiguity");
    let recovered =
        LegacyAuthManager::from_hub_teslamate_store_with_issuer(store, data.path(), issuer)
            .expect("new credential authority starts");
    assert_eq!(recovered.access_token(), "fresh-access");
    assert_eq!(recovered.refresh_token(), "fresh-refresh");
}

#[test]
fn postgres_password_parsing() {
    assert_eq!(
        TeslaMatePostgresPassword::from_bytes(b"postgres")
            .unwrap()
            .as_str(),
        "postgres"
    );
    assert_eq!(
        TeslaMatePostgresPassword::from_bytes(b"postgres\n")
            .unwrap()
            .as_str(),
        "postgres"
    );
    assert!(TeslaMatePostgresPassword::from_bytes(b"bad\n\n").is_err());
}

/// C5T-03, R45 row 5, R24: a failed write of a successor is a wait for the write
/// and not a loss of the authority to use the credential. The manager never
/// reports it to a caller (the successor is held and the write retried), and the
/// two errors that name it are not the collector's sensitive-access fence, which
/// ends the run. The real loss of the admission and an outcome of a refresh that
/// is unknown while the guard fails still are.
#[test]
fn a_failed_write_of_a_successor_is_a_wait_and_not_a_lost_authority() {
    for error in [
        LegacyAuthManagerError::Auth(LegacyAuthError::Persistence),
        LegacyAuthManagerError::Auth(LegacyAuthError::SensitivePersistenceUnavailable),
    ] {
        assert!(!error.is_sensitive_access_failure(), "{error:?}");
        assert!(!error.is_terminal_auth_failure(), "{error:?}");
    }
    for error in [
        LegacyAuthManagerError::Auth(LegacyAuthError::SensitiveAccessUnavailable),
        LegacyAuthManagerError::Auth(LegacyAuthError::SensitiveRotationOutcomeUnknown),
        LegacyAuthManagerError::Credential(CredentialError::SensitiveAccessUnavailable),
    ] {
        assert!(error.is_sensitive_access_failure(), "{error:?}");
    }
}

/// A JWT-shaped access token with the given payload (C5 spec section 8 `jwt`).
fn jwt_for_test(payload: serde_json::Value) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    format!(
        "{}.{}.c2ln",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(payload.to_string())
    )
}

fn seed_pair(
    store: &crate::db::HubStore,
    data_dir: &Path,
    access: &str,
    refresh: &str,
    schedule: Option<(i64, i64)>,
) {
    crate::teslamate_credentials::replace_key(data_dir, b"test-cloak-key").expect("private key");
    let key = crate::teslamate_credentials::load_key(data_dir).expect("load private key");
    let tokens = OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned())
        .expect("stored tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
            .expect("encrypt tokens");
    let stored = match schedule {
        Some((expires_at, next_refresh_at)) => crate::db::TeslaMateLegacyTokenStore::refreshed(
            access,
            refresh,
            expires_at,
            next_refresh_at,
        ),
        None => crate::db::TeslaMateLegacyTokenStore::imported(access, refresh),
    }
    .expect("schedule");
    store
        .replace_teslamate_legacy_tokens(&stored)
        .expect("store tokens");
}

fn tesla_endpoint(region: Option<crate::tesla_stream::StreamRegion>) -> AuthEndpointConfig {
    AuthEndpointConfig {
        mode: crate::legacy_auth::AuthMode::Tesla,
        region,
        token_url: None,
        allow_loopback_http: false,
    }
}

/// C5 spec test 84 and test 74 at the manager: every production constructor
/// takes the endpoint configuration, pasted whitespace is trimmed in memory
/// only, and the single-use fence is keyed by the generation of the stored
/// bytes, never of the trimmed copy.
#[test]
fn production_constructors_take_the_endpoint_and_key_the_fence_by_the_stored_bytes() {
    let data = crate::private_tempdir().expect("data directory");
    let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
    let access = jwt_for_test(serde_json::json!({
        "iss": "https://auth.tesla.cn/oauth2/v3",
        "exp": 1_800_028_800_i64,
    }));
    // Surrounding spaces survive the credential helpers (only control bytes
    // are refused there), so a pasted pair like this can be stored.
    let padded_access = format!("{access} ");
    seed_pair(
        &store,
        data.path(),
        &padded_access,
        "refresh-0 ",
        Some((1_800_028_800, 1_800_021_600)),
    );
    let raw_tokens =
        OwnerTokens::from_secret_parts(padded_access, "refresh-0 ".to_owned()).expect("raw tokens");
    let expected_generation =
        crate::teslamate_token::legacy_refresh_credential_generation(&raw_tokens);
    let trimmed_tokens =
        OwnerTokens::from_secret_parts(access.clone(), "refresh-0".to_owned()).expect("trimmed");
    assert_ne!(
        expected_generation,
        crate::teslamate_token::legacy_refresh_credential_generation(&trimmed_tokens),
        "the trimmed copy would be a different generation"
    );

    let managed = LegacyAuthManager::from_hub_teslamate_store(
        store.clone(),
        data.path(),
        &tesla_endpoint(None),
    )
    .expect("managed manager");
    assert_eq!(managed.access_token(), access);
    assert_eq!(managed.refresh_token(), "refresh-0");
    assert_eq!(managed.region(), crate::tesla_stream::StreamRegion::China);
    // The row is bound to the generation of its stored bytes at construction.
    let stored_generation = store
        .load_teslamate_legacy_tokens()
        .expect("load row")
        .expect("a row")
        .credential_generation();
    let LegacyAuthPersistence::HubTeslaMate(persistence) = &managed.persistence else {
        panic!("a store-backed manager");
    };
    let (receipt, input_generation) = persistence.begin_refresh().expect("begin the attempt");
    assert_eq!(input_generation, expected_generation);
    assert_eq!(Some(input_generation), stored_generation);
    persistence
        .cancel_refresh(receipt, input_generation)
        .expect("cancel the unsent attempt");

    let observer = LegacyAuthManager::from_hub_teslamate_store_observer(
        store,
        data.path(),
        &tesla_endpoint(None),
    )
    .expect("observer manager");
    assert_eq!(observer.refresh_token(), "refresh-0");
    assert_eq!(observer.region(), crate::tesla_stream::StreamRegion::China);
}

/// C5 R5 at the manager (C5-B2): a configuration the token contradicts builds the
/// manager in its configuration-error state. It sends nothing and hands out no
/// token, and the collector still starts (R43), takes its lease and shows the
/// state; a construction that failed here would end the run before its
/// readiness. Nothing is stored as a stop (R5), so the next start with the
/// configuration fixed is ready.
#[test]
fn the_manager_is_built_in_a_configuration_error_for_a_conflicting_or_missing_region() {
    use crate::legacy_auth::state::AuthState;

    let data = crate::private_tempdir().expect("data directory");
    let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
    seed_pair(&store, data.path(), "opaque-access-0001", "refresh-0", None);
    let manager = LegacyAuthManager::from_hub_teslamate_store(
        store.clone(),
        data.path(),
        &tesla_endpoint(None),
    )
    .expect("a stopped manager is still a manager");
    assert_eq!(manager.status_now().state, AuthState::ConfigurationError);
    assert!(matches!(
        manager.access_token_for_sensitive_use(),
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::RegionRequired
        ))
    ));
    assert!(matches!(
        manager.wait_view(manager.clock()),
        AuthWait::Stopped
    ));
    assert_eq!(
        manager.region(),
        crate::tesla_stream::StreamRegion::Global,
        "the data hosts of a stopped manager follow the configured region, else the global one"
    );
    let row = store
        .load_collector_auth_row()
        .unwrap()
        .expect("a status row");
    assert_eq!(
        (row.state.as_str(), row.reason.as_deref()),
        ("configuration_error", Some("region_required"))
    );
    assert!(row.stopped_generation.is_none(), "not a stored stop (R5)");
    drop(manager);
    // The configuration fixed, the next start is ready with no owner action.
    let manager = LegacyAuthManager::from_hub_teslamate_store(
        store.clone(),
        data.path(),
        &tesla_endpoint(Some(crate::tesla_stream::StreamRegion::China)),
    )
    .expect("the fixed configuration");
    assert_eq!(manager.status_now().state, AuthState::Ready);
    assert_eq!(manager.region(), crate::tesla_stream::StreamRegion::China);
    assert!(manager.access_token_for_sensitive_use().is_ok());
    drop(manager);
    // A token of the global issuer and a configured China region contradict each
    // other (R5): the manager is stopped, and the data hosts it names follow the
    // configured region.
    let global_token = jwt_for_test(serde_json::json!({
        "iss": "https://auth.tesla.com/oauth2/v3",
        "exp": 1_800_028_800_i64,
    }));
    seed_pair(&store, data.path(), &global_token, "refresh-0", None);
    let manager = LegacyAuthManager::from_hub_teslamate_store(
        store,
        data.path(),
        &tesla_endpoint(Some(crate::tesla_stream::StreamRegion::China)),
    )
    .expect("a stopped manager");
    assert_eq!(manager.status_now().state, AuthState::ConfigurationError);
    assert_eq!(manager.region(), crate::tesla_stream::StreamRegion::China);
}

/// R-P3bF4-4 at the constructor: an interrupted key replacement is settled only while
/// the run's guard admits it. A guard that has observed the loss refuses the settlement,
/// and the refusal is the bare loss of the sensitive access, the error every other path
/// of the unit reports for it, and not a fault of a credential file. Both key files stay
/// as they were.
#[test]
fn a_refused_key_settlement_is_the_bare_loss_of_the_sensitive_access() {
    for replacement_committed in [true, false] {
        let data = crate::private_tempdir().expect("data directory");
        let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
        seed_pair(&store, data.path(), "opaque-access-0001", "refresh-0", None);
        if replacement_committed {
            crate::teslamate_credentials::leave_previous_key_for_test(data.path(), b"left-over")
                .expect("a leftover previous generation");
        } else {
            crate::teslamate_credentials::begin_key_replacement_for_test(data.path(), b"newer-key")
                .expect("a replacement that did not commit");
        }
        let current = crate::teslamate_credentials::key_path(data.path());
        let previous = data
            .path()
            .join("secrets")
            .join(".teslamate-encryption.previous.key");
        let seeded = (
            std::fs::read(&current).expect("the current key"),
            std::fs::read(&previous).expect("the previous generation was seeded"),
        );
        let lost: LossObserver = Arc::new(|| true);
        let outcome = LegacyAuthManager::from_hub_teslamate_store_with_policy(
            store,
            data.path(),
            LegacyAuthRefreshPolicy::Managed,
            AuthMode::Tesla,
            crate::tesla_stream::StreamRegion::Global,
            endpoint_rebuild(&tesla_endpoint(None)),
            Arc::new(SystemEnv),
            Some(lost),
        );
        assert!(
            matches!(
                outcome,
                Err(LegacyAuthManagerError::Credential(
                    CredentialError::SensitiveAccessUnavailable
                ))
            ),
            "{replacement_committed}: {outcome:?}"
        );
        let after = (
            std::fs::read(&current).expect("the current key is still there"),
            std::fs::read(&previous).expect("the previous generation is still there"),
        );
        assert!(
            after == seeded,
            "{replacement_committed}: a key file was removed or restored"
        );
    }
}
