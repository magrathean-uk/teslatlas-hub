// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

use std::{cell::Cell, fs, io::Write, os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink}, path::Path};
use teslatlas_hub::auth::credential_sign_in::{FileCustodyError, read_credential_file, read_credential_file_with_hooks};

fn root() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("v2-custody-").permissions(fs::Permissions::from_mode(0o700)).tempdir().expect("synthetic root")
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).expect("private input");
    file.write_all(bytes).expect("synthetic input");
}

#[test]
fn private_regular_file_accepts_an_ancestor_link_and_enforces_the_cap() {
    let root = root();
    let directory = root.path().join("directory");
    fs::create_dir(&directory).expect("input directory");
    let path = directory.join("input");
    write_private(&path, b"access-0001");
    let alias = root.path().join("alias");
    symlink(&directory, &alias).expect("ancestor link");
    let bytes = read_credential_file(&alias.join("input"), 11).expect("private input accepted");
    assert!(bytes.as_slice() == b"access-0001");
    assert!(matches!(read_credential_file(&path, 10), Err(FileCustodyError::TooLarge)));
    assert!(fs::read(&path).expect("input retained") == b"access-0001");
}

#[test]
fn unsafe_entries_are_rejected_without_following_the_final_link() {
    let root = root();
    let target = root.path().join("target");
    write_private(&target, b"access-0001");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).expect("unsafe mode");
    assert!(matches!(read_credential_file(&target, 32), Err(FileCustodyError::UnsafeFile)));
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("private mode");
    let link = root.path().join("link");
    symlink(&target, &link).expect("final link");
    assert!(matches!(read_credential_file(&link, 32), Err(FileCustodyError::UnsafeFile)));
    assert!(fs::read(&target).expect("target retained") == b"access-0001");
    assert!(matches!(read_credential_file(root.path(), 32), Err(FileCustodyError::UnsafeFile)));
}

#[test]
fn replacement_before_open_or_during_read_refuses_the_result() {
    for swap_before_open in [true, false] {
        let root = root();
        let path = root.path().join("input");
        let replacement = root.path().join("replacement");
        write_private(&path, b"access-0001");
        write_private(&replacement, b"access-0002");
        let read = Cell::new(false);
        let result = read_credential_file_with_hooks(
            &path, 32,
            || { if swap_before_open { fs::rename(&replacement, &path).expect("pre-open replacement"); } },
            || { if !swap_before_open { fs::rename(&replacement, &path).expect("post-open replacement"); } },
            || read.set(true),
        );
        assert!(matches!(result, Err(FileCustodyError::IdentityChanged)));
        assert_eq!(read.get(), !swap_before_open);
        assert!(fs::read(&path).expect("replacement retained") == b"access-0002");
    }
}
