#![forbid(unsafe_code)]

use std::fs;

use tempfile::TempDir;
use teslatlas_edge::credentials::{CredentialError, CredentialStore};

const T0: i64 = 1_800_000_000_000;

#[test]
fn enrolment_returns_secret_once_and_stores_only_digest() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("hub-credentials.json");
    let store = CredentialStore::open(&path).unwrap();

    let issued = store.enrol("home-hub", T0, 60_000).unwrap();
    assert!(issued.token().starts_with("tte1."));
    assert!(store.verify(issued.token(), T0 + 1).unwrap());
    let stored = fs::read_to_string(&path).unwrap();
    assert!(!stored.contains(issued.token()));
    assert!(!stored.contains(issued.secret_component()));
    assert!(stored.contains("secret_digest"));

    let reopened = CredentialStore::open(&path).unwrap();
    assert!(reopened.verify(issued.token(), T0 + 2).unwrap());
    assert!(
        reopened
            .list()
            .unwrap()
            .iter()
            .all(|record| record.token().is_none())
    );
}

#[test]
fn rotation_supports_zero_or_bounded_overlap() {
    let temp = TempDir::new().unwrap();
    let store = CredentialStore::open(temp.path().join("credentials.json")).unwrap();
    let old = store.enrol("home-hub", T0, 120_000).unwrap();
    let replacement = store
        .rotate(old.credential_id(), T0 + 1_000, 5_000, 120_000)
        .unwrap();

    assert!(store.verify(old.token(), T0 + 5_999).unwrap());
    assert!(!store.verify(old.token(), T0 + 6_000).unwrap());
    assert!(store.verify(replacement.token(), T0 + 6_000).unwrap());

    let immediate = store
        .rotate(replacement.credential_id(), T0 + 7_000, 0, 120_000)
        .unwrap();
    assert!(!store.verify(replacement.token(), T0 + 7_000).unwrap());
    assert!(store.verify(immediate.token(), T0 + 7_000).unwrap());
}

#[test]
fn revocation_and_expiry_fail_closed() {
    let temp = TempDir::new().unwrap();
    let store = CredentialStore::open(temp.path().join("credentials.json")).unwrap();
    let revoked = store.enrol("revoked-hub", T0, 60_000).unwrap();
    store.revoke(revoked.credential_id(), T0 + 100).unwrap();
    assert!(!store.verify(revoked.token(), T0 + 100).unwrap());

    let expiring = store.enrol("expiring-hub", T0, 1_000).unwrap();
    assert!(store.verify(expiring.token(), T0 + 999).unwrap());
    assert!(!store.verify(expiring.token(), T0 + 1_000).unwrap());
    assert_eq!(
        store.rotate(revoked.credential_id(), T0 + 200, 0, 1_000),
        Err(CredentialError::InactiveCredential)
    );
}

#[cfg(unix)]
#[test]
fn credential_store_does_not_change_existing_parent_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    let parent = temp.path().join("existing-parent");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    CredentialStore::open(parent.join("credentials.json")).unwrap();
    assert_eq!(
        std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn duplicate_json_keys_in_credential_state_fail_closed() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("credentials.json");
    fs::write(
        &path,
        b"{\"version\":1,\"credentials\":[],\"credentials\":[]}",
    )
    .unwrap();
    assert!(matches!(
        CredentialStore::open(path),
        Err(CredentialError::InvalidStore)
    ));
}

#[test]
fn credential_process_worker() {
    let Ok(root) = std::env::var("TESLATLAS_TEST_CREDENTIAL_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let operation = std::env::var("TESLATLAS_TEST_CREDENTIAL_OPERATION").unwrap();
    fs::write(root.join(format!("{operation}.ready")), b"ready").unwrap();
    let store = CredentialStore::open(root.join("credentials.json")).unwrap();
    if operation == "revoke" {
        store
            .revoke(
                &std::env::var("TESLATLAS_TEST_CREDENTIAL_ID").unwrap(),
                T0 + 1,
            )
            .unwrap();
    } else {
        store.enrol("other-hub", T0 + 1, 60_000).unwrap();
    }
    fs::write(root.join(format!("{operation}.done")), b"done").unwrap();
}

#[test]
fn independent_processes_wait_for_stable_lock_and_preserve_revocation() {
    use fs4::FileExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let temp = TempDir::new().unwrap();
    let store = CredentialStore::open(temp.path().join("credentials.json")).unwrap();
    let issued = store.enrol("revoked-hub", T0, 60_000).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(temp.path().join(".credentials.json.lock"))
        .unwrap();
    FileExt::lock(&lock).unwrap();
    let mut children = ["revoke", "enrol"].map(|operation| {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "credential_process_worker", "--nocapture"])
            .env("TESLATLAS_TEST_CREDENTIAL_ROOT", temp.path())
            .env("TESLATLAS_TEST_CREDENTIAL_OPERATION", operation)
            .env("TESLATLAS_TEST_CREDENTIAL_ID", issued.credential_id())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(temp.path().join("revoke.ready").exists() && temp.path().join("enrol.ready").exists()) {
        assert!(
            Instant::now() < deadline,
            "credential workers failed to start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    for child in &mut children {
        assert!(
            child.try_wait().unwrap().is_none(),
            "mutation bypassed cross-process lock"
        );
    }
    FileExt::unlock(&lock).unwrap();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(store.list().unwrap().len(), 2);
    assert!(!store.verify(issued.token(), T0 + 2).unwrap());
    let reopened = CredentialStore::open(temp.path().join("credentials.json")).unwrap();
    assert_eq!(reopened.list().unwrap().len(), 2);
    assert!(!reopened.verify(issued.token(), T0 + 2).unwrap());
}

#[cfg(unix)]
#[test]
fn credential_lock_rejects_symlinks_and_shared_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("credentials.json");
    let target = temp.path().join("unrelated");
    fs::write(&target, b"preserved").unwrap();
    let lock = temp.path().join(".credentials.json.lock");
    symlink(&target, &lock).unwrap();
    assert!(matches!(
        CredentialStore::open(&path),
        Err(CredentialError::InvalidPath)
    ));
    assert_eq!(fs::read(&target).unwrap(), b"preserved");
    fs::remove_file(&lock).unwrap();
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(matches!(
        CredentialStore::open(&path),
        Err(CredentialError::InvalidPath)
    ));
    assert!(!path.exists());
}
