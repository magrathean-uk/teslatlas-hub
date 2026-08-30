// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs, os::unix::fs::PermissionsExt, process::Command, sync::mpsc, thread, time::Duration};

use super::*;

fn encrypted_store(
    key: &[u8],
    access: &str,
    refresh: &str,
) -> crate::db::TeslaMateLegacyTokenStore {
    let tokens =
        crate::credentials::OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned())
            .expect("test tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt pair");
    let generation = crate::teslamate_token::legacy_refresh_credential_generation(&tokens);
    crate::db::TeslaMateLegacyTokenStore::imported(access, refresh)
        .expect("stored pair")
        .with_credential_generation(generation)
        .expect("credential generation")
}

#[test]
fn replaces_and_loads_exact_key_with_private_permissions() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    replace_key(temporary.path(), b"first-key").expect("first key writes");
    replace_key(temporary.path(), b"second-key").expect("second key replaces first");

    let loaded = load_key(temporary.path()).expect("key loads");
    assert_eq!(loaded.as_bytes(), b"second-key");
    let secrets = temporary.path().join("secrets");
    assert_eq!(
        fs::symlink_metadata(&secrets)
            .expect("secrets metadata")
            .permissions()
            .mode()
            & 0o777,
        SECRETS_DIRECTORY_MODE
    );
    assert_eq!(
        fs::symlink_metadata(key_path(temporary.path()))
            .expect("key metadata")
            .permissions()
            .mode()
            & 0o777,
        KEY_FILE_MODE
    );
}

#[test]
fn key_and_ciphertext_replacement_recovers_both_crash_sides() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = crate::db::HubStore::initialize(temporary.path()).expect("store");
    let old_key = b"old exact TeslaMate key";
    let new_key = b"new exact TeslaMate key";
    let old_store = encrypted_store(old_key, "old-access", "old-refresh");
    let new_store = encrypted_store(new_key, "new-access", "new-refresh");

    replace_key_and_tokens(temporary.path(), &store, old_key, &old_store).expect("initial pair");

    // Crash before the SQLite commit: the durable old pair selects and
    // restores the previous key generation.
    drop(begin_key_replacement(temporary.path(), new_key).expect("stage new key"));
    let recovered = load_key_for_tokens(temporary.path(), &old_store).expect("recover old key");
    assert_eq!(recovered.as_bytes(), old_key);
    assert!(!previous_key_path(temporary.path()).exists());

    // Crash after the SQLite commit: the durable new pair selects the new
    // key and only discards the retained old generation.
    drop(begin_key_replacement(temporary.path(), new_key).expect("stage new key"));
    store
        .replace_teslamate_legacy_tokens(&new_store)
        .expect("commit new pair");
    let recovered = load_key_for_tokens(temporary.path(), &new_store).expect("keep new key");
    assert_eq!(recovered.as_bytes(), new_key);
    assert!(!previous_key_path(temporary.path()).exists());
}

#[test]
fn rejects_wrong_current_key_without_a_previous_generation() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let tokens = encrypted_store(b"original TeslaMate key", "access", "refresh");
    replace_key(temporary.path(), b"wrong TeslaMate key").expect("write wrong key");

    assert!(!previous_key_path(temporary.path()).exists());
    assert!(matches!(
        load_key_for_tokens(temporary.path(), &tokens),
        Err(TeslaMateCredentialError::NoMatchingKeyGeneration)
    ));
}

#[test]
fn read_only_legacy_validation_never_settles_key_generation() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let old_key = b"old exact TeslaMate key";
    let tokens = encrypted_store(old_key, "access", "refresh");
    replace_key(temporary.path(), old_key).expect("write old key");
    drop(begin_key_replacement(temporary.path(), b"new wrong key").expect("stage key"));

    validate_stored_legacy_credentials_read_only(temporary.path(), &tokens)
        .expect("previous generation decrypts");
    assert!(previous_key_path(temporary.path()).exists());
    assert_eq!(
        load_key(temporary.path()).expect("current key").as_bytes(),
        b"new wrong key"
    );

    fs::remove_file(previous_key_path(temporary.path())).expect("remove previous fixture");
    assert!(matches!(
        validate_stored_legacy_credentials_read_only(temporary.path(), &tokens),
        Err(TeslaMateCredentialError::NoMatchingKeyGeneration)
    ));
}

#[test]
fn read_only_legacy_validation_rejects_mismatched_generation() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let key = b"exact TeslaMate key";
    let tokens = encrypted_store(key, "access", "refresh")
        .with_credential_generation(Uuid::new_v4())
        .expect("mismatched generation fixture");
    replace_key(temporary.path(), key).expect("write key");

    assert!(matches!(
        validate_stored_legacy_credentials_read_only(temporary.path(), &tokens),
        Err(TeslaMateCredentialError::NoMatchingKeyGeneration)
    ));
}

#[test]
fn ambiguous_refresh_rejects_same_plaintext_under_new_random_envelopes() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = crate::db::HubStore::initialize(temporary.path()).expect("store");
    let first_key = b"first exact TeslaMate key";
    let second_key = b"second exact TeslaMate key";
    let first = encrypted_store(first_key, "same-access", "same-refresh");
    replace_key_and_tokens(temporary.path(), &store, first_key, &first).expect("initial pair");
    let generation = store
        .load_teslamate_legacy_tokens()
        .expect("load initial pair")
        .expect("initial pair")
        .credential_generation()
        .expect("bound generation");
    store
        .begin_legacy_refresh(generation)
        .expect("ambiguous refresh intent");

    let reencrypted = encrypted_store(second_key, "same-access", "same-refresh");
    assert!(matches!(
        replace_key_and_tokens(temporary.path(), &store, second_key, &reencrypted,),
        Err(TeslaMateCredentialImportError::TokenStore(
            crate::db::StoreError::LegacyRefreshOutcomeUnknown
        ))
    ));
    assert_eq!(
        load_key_for_tokens(temporary.path(), &first)
            .expect("old key remains")
            .as_bytes(),
        first_key
    );
}

#[test]
fn sign_out_removes_tokens_and_both_key_generations() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = crate::db::HubStore::initialize(temporary.path()).expect("store");
    let key = b"exact TeslaMate key";
    let stored = encrypted_store(key, "access", "refresh");
    replace_key_and_tokens(temporary.path(), &store, key, &stored).expect("persist pair");
    drop(begin_key_replacement(temporary.path(), b"replacement key").expect("stage key"));

    remove_key_and_tokens(temporary.path(), &store).expect("remove authority");

    assert!(
        store
            .load_teslamate_legacy_tokens()
            .expect("read token row")
            .is_none()
    );
    assert!(!key_path(temporary.path()).exists());
    assert!(!previous_key_path(temporary.path()).exists());

    remove_key_and_tokens(temporary.path(), &store).expect("idempotent removal");
}

#[test]
fn sign_out_after_ambiguous_refresh_allows_fresh_credentials_without_reusing_input() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = crate::db::HubStore::initialize(temporary.path()).expect("store");
    let old_key = b"old exact TeslaMate key";
    let old = encrypted_store(old_key, "old-access", "old-refresh");
    replace_key_and_tokens(temporary.path(), &store, old_key, &old).expect("persist old pair");
    let old_generation = store
        .load_teslamate_legacy_tokens()
        .expect("load old pair")
        .expect("old pair")
        .credential_generation()
        .expect("old generation");
    store
        .begin_legacy_refresh(old_generation)
        .expect("ambiguous refresh starts");

    remove_key_and_tokens(temporary.path(), &store).expect("sign out");
    assert!(
        !store
            .has_unresolved_legacy_refresh()
            .expect("sign out closes ambiguous receipt")
    );

    let fresh_key = b"fresh exact TeslaMate key";
    let fresh = encrypted_store(fresh_key, "fresh-access", "fresh-refresh");
    replace_key_and_tokens(temporary.path(), &store, fresh_key, &fresh)
        .expect("fresh pair persists");
    let reopened = store
        .load_teslamate_legacy_tokens()
        .expect("fresh pair loads")
        .expect("fresh pair");
    let fresh_generation = reopened.credential_generation().expect("fresh generation");
    assert_ne!(fresh_generation, old_generation);
    assert_eq!(
        load_key_for_tokens(temporary.path(), &reopened)
            .expect("fresh key reopens")
            .as_bytes(),
        fresh_key
    );
    assert!(matches!(
        replace_key_and_tokens(temporary.path(), &store, old_key, &old),
        Err(TeslaMateCredentialImportError::TokenStore(
            crate::db::StoreError::LegacyRefreshOutcomeUnknown
        ))
    ));
    let current = store
        .load_teslamate_legacy_tokens()
        .expect("fresh pair reloads")
        .expect("fresh pair remains");
    assert_eq!(current.credential_generation(), Some(fresh_generation));
    assert_eq!(
        load_key_for_tokens(temporary.path(), &current)
            .expect("fresh key remains")
            .as_bytes(),
        fresh_key
    );
}

#[test]
fn rejects_symlinked_key_file() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let secrets = temporary.path().join("secrets");
    fs::create_dir(&secrets).expect("secrets directory");
    fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
        .expect("protect secrets directory");
    let outside = temporary.path().join("outside");
    fs::write(&outside, b"outside").expect("outside file");
    std::os::unix::fs::symlink(&outside, key_path(temporary.path())).expect("key symlink");

    assert!(matches!(
        load_key(temporary.path()),
        Err(TeslaMateCredentialError::UnsafeKeyFile)
    ));
    assert!(matches!(
        replace_key(temporary.path(), b"replacement"),
        Err(TeslaMateCredentialError::UnsafeKeyFile)
    ));
}

#[test]
fn rejects_empty_and_oversized_key_bytes() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    assert!(matches!(
        replace_key(temporary.path(), b""),
        Err(TeslaMateCredentialError::EmptyKey)
    ));
    assert!(matches!(
        replace_key(temporary.path(), &vec![0; MAX_KEY_BYTES + 1]),
        Err(TeslaMateCredentialError::KeyTooLarge)
    ));
}

#[test]
fn key_reader_rejects_oversized_and_replaced_files_after_open() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let secrets = temporary.path().join("secrets");
    fs::create_dir(&secrets).expect("secrets directory");
    fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
        .expect("protect secrets directory");
    let path = key_path(temporary.path());
    fs::write(&path, vec![7_u8; MAX_KEY_BYTES + 1]).expect("oversized key");
    fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE)).expect("key mode");
    assert!(matches!(
        load_key(temporary.path()),
        Err(TeslaMateCredentialError::KeyTooLarge)
    ));

    fs::write(&path, b"original").expect("key");
    fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE)).expect("key mode");
    let replacement = secrets.join("replacement");
    fs::write(&replacement, b"replacement").expect("replacement key");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(KEY_FILE_MODE))
        .expect("replacement mode");
    assert!(matches!(
        read_checked_secret_file_after_open(&path, MAX_KEY_BYTES, false, || {
            fs::rename(&replacement, &path).expect("replace key")
        }),
        Err(TeslaMateCredentialError::KeyIdentityChanged)
    ));
}

#[test]
fn key_readers_reject_fifos_without_waiting_for_a_writer() {
    for cursor in [false, true] {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let path = temporary.path().join(if cursor {
            "cursor-key.fifo"
        } else {
            "encryption-key.fifo"
        });
        assert!(
            Command::new("mkfifo")
                .arg(&path)
                .status()
                .expect("run mkfifo")
                .success()
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE)).expect("FIFO mode");

        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let rejected = match read_checked_secret_file(&path, MAX_KEY_BYTES, cursor) {
                Err(TeslaMateCredentialError::UnsafeCursorKeyFile) if cursor => true,
                Err(TeslaMateCredentialError::UnsafeKeyFile) if !cursor => true,
                _ => false,
            };
            sender.send(rejected).expect("send FIFO result");
        });
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("FIFO admission must not block")
        );
        worker.join().expect("FIFO admission worker");
    }
}

#[test]
fn creates_and_reloads_one_private_cursor_key() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let _first = load_or_create_cursor_key(temporary.path()).expect("cursor key creates");
    let path = cursor_key_path(temporary.path());
    let first_bytes = fs::read(&path).expect("cursor key bytes");
    let _second = load_or_create_cursor_key(temporary.path()).expect("cursor key reloads");

    assert_eq!(first_bytes.len(), CURSOR_KEY_BYTES);
    assert_eq!(
        fs::read(&path).expect("cursor key bytes reopen"),
        first_bytes
    );
    assert_eq!(
        fs::symlink_metadata(&path)
            .expect("cursor key metadata")
            .permissions()
            .mode()
            & 0o777,
        KEY_FILE_MODE
    );
}

#[test]
fn rejects_bad_cursor_key_length_and_mode() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let secrets = temporary.path().join("secrets");
    fs::create_dir(&secrets).expect("secrets directory");
    fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
        .expect("protect secrets directory");
    let path = cursor_key_path(temporary.path());
    fs::write(&path, [0_u8; CURSOR_KEY_BYTES - 1]).expect("short cursor key");
    fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE))
        .expect("protect short cursor key");
    assert!(matches!(
        load_or_create_cursor_key(temporary.path()),
        Err(TeslaMateCredentialError::InvalidCursorKeyLength)
    ));

    fs::write(&path, [0_u8; CURSOR_KEY_BYTES + 1]).expect("oversized cursor key");
    fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE))
        .expect("protect oversized cursor key");
    assert!(matches!(
        load_or_create_cursor_key(temporary.path()),
        Err(TeslaMateCredentialError::InvalidCursorKeyLength)
    ));

    fs::write(&path, [0_u8; CURSOR_KEY_BYTES]).expect("cursor key");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("weaken cursor key mode");
    assert!(matches!(
        load_or_create_cursor_key(temporary.path()),
        Err(TeslaMateCredentialError::UnsafeCursorKeyFile)
    ));

    fs::write(&path, [0_u8; CURSOR_KEY_BYTES]).expect("cursor key");
    fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE)).expect("cursor key mode");
    let replacement = secrets.join("replacement-cursor");
    fs::write(&replacement, [1_u8; CURSOR_KEY_BYTES]).expect("replacement cursor key");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(KEY_FILE_MODE))
        .expect("replacement cursor key mode");
    assert!(matches!(
        read_checked_secret_file_after_open(&path, CURSOR_KEY_BYTES, true, || {
            fs::rename(&replacement, &path).expect("replace cursor key")
        }),
        Err(TeslaMateCredentialError::CursorKeyIdentityChanged)
    ));
}

#[test]
fn rejects_symlinked_cursor_key() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let secrets = temporary.path().join("secrets");
    fs::create_dir(&secrets).expect("secrets directory");
    fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
        .expect("protect secrets directory");
    let outside = temporary.path().join("outside");
    fs::write(&outside, [0_u8; CURSOR_KEY_BYTES]).expect("outside file");
    std::os::unix::fs::symlink(&outside, cursor_key_path(temporary.path()))
        .expect("cursor key symlink");

    assert!(matches!(
        load_or_create_cursor_key(temporary.path()),
        Err(TeslaMateCredentialError::UnsafeCursorKeyFile)
    ));
}
