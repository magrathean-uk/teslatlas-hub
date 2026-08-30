// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

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

    assert!(matches!(
        stale_manager
            .refresh_now(&Client::new(), SystemTime::now())
            .await,
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::SensitiveRotationOutcomeUnknown
        ))
    ));
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
        if guarded_checks.fetch_add(1, Ordering::SeqCst) < 2 {
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
    assert_eq!(checks.load(Ordering::SeqCst), 3);
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
    assert!(matches!(
        manager.refresh_now(&Client::new(), SystemTime::now()).await,
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::SensitiveRotationOutcomeUnknown
        ))
    ));
    response_task.await.expect("response task");
    assert!(matches!(
        manager.refresh_now(&Client::new(), SystemTime::now()).await,
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::SensitiveRotationOutcomeUnknown
        ))
    ));
    assert!(matches!(
        LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            store.clone(),
            data.path(),
            issuer.clone(),
        ),
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::SensitiveRotationOutcomeUnknown
        ))
    ));

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

#[test]
fn refreshed_pair_persistence_failure_is_terminal() {
    for error in [
        LegacyAuthManagerError::Auth(LegacyAuthError::Persistence),
        LegacyAuthManagerError::Auth(LegacyAuthError::SensitivePersistenceUnavailable),
    ] {
        assert!(error.is_sensitive_access_failure());
    }
}
