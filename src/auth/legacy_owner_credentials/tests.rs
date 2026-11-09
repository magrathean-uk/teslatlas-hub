// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs, os::unix::fs::PermissionsExt, process::Command, sync::mpsc, thread, time::Duration};

use super::*;



#[test]
fn replaces_and_loads_exact_key_with_private_permissions() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency");
    }

#[test]
fn key_and_ciphertext_replacement_recovers_both_crash_sides() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential table dependency");
    }

#[test]
fn rejects_wrong_current_key_without_a_previous_generation() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency");
    }

#[test]
fn read_only_legacy_validation_never_settles_key_generation() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency");
    }

#[test]
fn read_only_legacy_validation_rejects_mismatched_generation() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency");
    }

#[test]
fn ambiguous_refresh_rejects_same_plaintext_under_new_random_envelopes() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential table dependency");
    }

#[test]
fn sign_out_removes_tokens_and_both_key_generations() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential table dependency");
    }

#[test]
fn sign_out_after_ambiguous_refresh_allows_fresh_credentials_without_reusing_input() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential table dependency");
    }

#[test]
fn rejects_symlinked_key_file() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency");
    }

#[test]
fn rejects_empty_and_oversized_key_bytes() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency");
    }

#[test]
fn key_reader_rejects_oversized_and_replaced_files_after_open() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let secrets = temporary.path().join("secrets");
    fs::create_dir(&secrets).expect("secrets directory");
    fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
        .expect("protect secrets directory");
    let path = ({ let unavailable_value: std::path::PathBuf = panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency"); unavailable_value });
    fs::write(&path, vec![7_u8; (16 * 1024) + 1]).expect("oversized key");
    fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE)).expect("key mode");
    assert!(matches!(
        ({ let unavailable_value: Result<zeroize::Zeroizing<Vec<u8>>, crate::legacy_owner_credentials::SourceDbCredentialError> = panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency"); unavailable_value }),
        Err(SourceDbCredentialError::KeyTooLarge)
    ));

    fs::write(&path, b"original").expect("key");
    fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE)).expect("key mode");
    let replacement = secrets.join("replacement");
    fs::write(&replacement, b"replacement").expect("replacement key");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(KEY_FILE_MODE))
        .expect("replacement mode");
    assert!(matches!(
        read_checked_secret_file_after_open(&path, (16 * 1024), false, || {
            fs::rename(&replacement, &path).expect("replace key")
        }),
        Err(SourceDbCredentialError::KeyIdentityChanged)
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
            let rejected = match read_checked_secret_file(&path, 16 * 1024, cursor) {
                Err(SourceDbCredentialError::UnsafeCursorKeyFile) if cursor => true,
                Err(SourceDbCredentialError::UnsafeKeyFile) if !cursor => true,
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
        Err(SourceDbCredentialError::InvalidCursorKeyLength)
    ));

    fs::write(&path, [0_u8; CURSOR_KEY_BYTES + 1]).expect("oversized cursor key");
    fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE))
        .expect("protect oversized cursor key");
    assert!(matches!(
        load_or_create_cursor_key(temporary.path()),
        Err(SourceDbCredentialError::InvalidCursorKeyLength)
    ));

    fs::write(&path, [0_u8; CURSOR_KEY_BYTES]).expect("cursor key");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("weaken cursor key mode");
    assert!(matches!(
        load_or_create_cursor_key(temporary.path()),
        Err(SourceDbCredentialError::UnsafeCursorKeyFile)
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
        Err(SourceDbCredentialError::CursorKeyIdentityChanged)
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
        Err(SourceDbCredentialError::UnsafeCursorKeyFile)
    ));
}

// ---- B2 fix round, P3b fix 4 (R-P3bF4-4, R-P3bF4-7): the guarded settlement and its counter -----------------

/// The files of `secrets/` with inode, modification time and length.
fn secrets_image(data_dir: &Path) -> Vec<String> {
    use std::os::unix::fs::MetadataExt as _;
    let mut image: Vec<String> = fs::read_dir(data_dir.join("secrets"))
        .expect("secrets")
        .flatten()
        .map(|entry| {
            let metadata = entry.metadata().expect("metadata");
            format!(
                "{}:{}:{}.{}:{}",
                entry.file_name().to_string_lossy(),
                metadata.ino(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.len()
            )
        })
        .collect();
    image.sort();
    image
}



#[test]
fn p3bf4_a_refused_settlement_leaves_both_key_files_as_they_were() {
    for committed_new in [true, false] {
        let (temporary, tokens, _) = ({ let unavailable_value: (tempfile::TempDir, (Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), &'static [u8]) = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value });
        let image = secrets_image(temporary.path());
        let attempts = crate::platform::file_attempts::count_under(temporary.path());
        assert!(matches!(
            ({ let load_key_for_tokens_with: fn(&std::path::Path, &(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), &dyn Fn() -> bool) -> Result<zeroize::Zeroizing<Vec<u8>>, crate::legacy_owner_credentials::SourceDbCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); load_key_for_tokens_with(temporary.path(), &tokens, &|| false) }),
            Err(SourceDbCredentialError::SettlementRefused)
        ));
        assert_eq!(
            secrets_image(temporary.path()),
            image,
            "committed {committed_new}"
        );
        assert_eq!(
            crate::platform::file_attempts::count_under(temporary.path()),
            attempts,
            "no file mutation was attempted (committed {committed_new})"
        );
    }
}

#[test]
fn p3bf4_an_admitted_settlement_consults_in_front_of_each_mutation_and_settles() {
    for (committed_new, consultations) in [(true, 1_usize), (false, 2)] {
        let (temporary, tokens, key) = ({ let unavailable_value: (tempfile::TempDir, (Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), &'static [u8]) = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value });
        let consulted = std::cell::Cell::new(0_usize);
        let loaded = ({ let load_key_for_tokens_with: fn(&std::path::Path, &(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), &dyn Fn() -> bool) -> Result<zeroize::Zeroizing<Vec<u8>>, crate::legacy_owner_credentials::SourceDbCredentialError> = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); load_key_for_tokens_with(temporary.path(), &tokens, &|| {
            consulted.set(consulted.get() + 1);
            true
        }) })
        .expect("the admitted settlement");
        assert_eq!(loaded.as_slice(), key);
        assert_eq!(consulted.get(), consultations, "committed {committed_new}");
        assert!(!({ let unavailable_value: std::path::PathBuf = panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency"); unavailable_value }).exists());
        assert_eq!(({ let unavailable_value: Result<zeroize::Zeroizing<Vec<u8>>, crate::legacy_owner_credentials::SourceDbCredentialError> = panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency"); unavailable_value }).expect("current").as_slice(), key);
    }
}

#[test]
fn p3bf4_a_restore_refused_between_its_two_mutations_is_recovered_by_the_next_start() {
        panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency");
    }

#[test]
fn p3bf5_the_credential_file_counter_counts_creation_settlement_and_directory_sync() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let count = || crate::platform::file_attempts::count_under(temporary.path());
    assert_eq!(count(), 0);
    load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let created = count();
    // The secrets directory, the temporary file's create, write and sync, the hard link
    // of the final name, the removal of the temporary name and the sync of the directory.
    assert_eq!(created, 7);
    // An idempotent load writes nothing and counts nothing.
    load_or_create_cursor_key(temporary.path()).expect("cursor key again");
    assert_eq!(count(), created);
    // The settlement of an interrupted replacement removes the previous generation and
    // syncs the directory (the transient write is counted by the terrain control,
    // `p3bf4_the_stamp_counts_a_transient_file_write_the_final_files_do_not_show`).
    let (settled, tokens, _) = ({ let unavailable_value: (tempfile::TempDir, (Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), &'static [u8]) = panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency"); unavailable_value });
    let before = crate::platform::file_attempts::count_under(settled.path());
    ({ let unavailable_value: Result<zeroize::Zeroizing<Vec<u8>>, crate::legacy_owner_credentials::SourceDbCredentialError> = panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency"); unavailable_value }).expect("settled");
    assert_eq!(
        crate::platform::file_attempts::count_under(settled.path()),
        before + 2,
        "the removal of the previous generation and the sync of the directory are counted"
    );
}
