// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs, os::unix::fs::PermissionsExt, process::Command, sync::mpsc, thread, time::Duration};

use super::*;

#[test]
fn key_reader_rejects_oversized_and_replaced_files_after_open() {
    // The checked reader the cursor key shares, on its general (non-cursor) branch.
    let temporary = crate::private_tempdir().expect("temporary directory");
    let secrets = temporary.path().join("secrets");
    fs::create_dir(&secrets).expect("secrets directory");
    fs::set_permissions(&secrets, fs::Permissions::from_mode(SECRETS_DIRECTORY_MODE))
        .expect("protect secrets directory");
    let path = secrets.join("checked-secret");
    let maximum = 16 * 1024;
    fs::write(&path, vec![7_u8; maximum + 1]).expect("oversized secret");
    fs::set_permissions(&path, fs::Permissions::from_mode(KEY_FILE_MODE)).expect("secret mode");
    assert!(matches!(
        read_checked_secret_file(&path, maximum, false),
        Err(SourceDbCredentialError::KeyTooLarge)
    ));

    fs::write(&path, vec![7_u8; maximum]).expect("secret at the cap");
    assert_eq!(
        read_checked_secret_file(&path, maximum, false)
            .expect("the cap is accepted")
            .len(),
        maximum
    );

    fs::write(&path, b"original").expect("secret");
    let replacement = secrets.join("replacement");
    fs::write(&replacement, b"replacement").expect("replacement secret");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(KEY_FILE_MODE))
        .expect("replacement mode");
    assert!(matches!(
        read_checked_secret_file_after_open(&path, maximum, false, || {
            fs::rename(&replacement, &path).expect("replace secret")
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

#[test]
fn p3bf5_the_credential_file_counter_counts_cursor_key_creation_and_directory_sync() {
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
}
