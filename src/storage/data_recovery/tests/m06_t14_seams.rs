// SPDX-License-Identifier: AGPL-3.0-only

// M06-T14: the REV-CX-M06-T08 regressions that need this repair's own
// interfaces, so they cannot be built at 07c87b9: the owner-only destination
// the shipped `backup` command asks for (finding 4, through the request's
// `private_destination`), an ancestor swapped just before the destination is
// opened (finding 2: the old race window between canonicalisation and the
// open had no hook to reach it) and a terminal restoration that fails
// (finding 6, through a seam in the PTY child). Synthetic data only.

use super::{
    m06_t14::{PTY_SCENARIO, Pty, finish_with_next_line, pty_child_report, wait_for_prompt},
    *,
};
use crate::data_recovery::{
    EncryptedBackupRequest,
    archive_crypto::KdfParams,
    backup_admission::{VolumeTrust, test_seam},
    create_encrypted_backup,
    encrypted_backup::clear_hook,
    read_recovery_password,
};

const PASSWORD: &str = "correct horse battery staple";
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};

fn request<'a>(destination: &'a Path, private: bool) -> EncryptedBackupRequest<'a> {
    let mut request = EncryptedBackupRequest::new(destination, PASSWORD);
    request.kdf = TEST_KDF;
    request.private_destination = private;
    request
}

fn folder(parent: &Path, name: &str, mode: u32) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir(&path).expect("folder");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode");
    path
}

fn posix_fixture() -> (tempfile::TempDir, HubStore) {
    clear_hook();
    test_seam::force(Some(VolumeTrust::Posix));
    test_seam::on_acl_listing(None::<fn(&str)>);
    test_seam::on_destination(None::<fn(&str)>);
    create_fixture()
}

/// Finding 4: the command's destination must be owner-only where the volume
/// enforces ownership: readable, listable or searchable foreign access by
/// mode is refused, and nothing is written.
#[test]
fn data_recovery_f4_private_destination_refuses_foreign_access_by_mode() {
    let (temporary, store) = posix_fixture();
    for mode in [0o755, 0o750, 0o705, 0o701, 0o710] {
        let destination = folder(temporary.path(), &format!("mode-{mode:o}"), mode);
        let refused = create_encrypted_backup(&store, &request(&destination, true));
        let error = refused.expect_err("foreign access refuses a private destination");
        assert!(matches!(error, DataRecoveryError::InvalidBackup(_)), "{mode:o}: {error}");
        assert!(error.to_string().contains("private folder"), "{error}");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0, "{mode:o}: nothing written");
    }
    let private = folder(temporary.path(), "private", 0o700);
    create_encrypted_backup(&store, &request(&private, true)).expect("an owner-only folder");
    // The schedule's rule is unchanged: no foreign write (App folders are 0755).
    let scheduled = folder(temporary.path(), "scheduled", 0o755);
    create_encrypted_backup(&store, &request(&scheduled, false)).expect("the schedule's rule");
    test_seam::force(None);
}

/// Finding 4: an ACL entry that grants another account any access refuses a
/// private destination whose mode is 0700; a deny entry grants nothing.
#[cfg(target_os = "macos")]
#[test]
fn data_recovery_f4_private_destination_refuses_acl_grants() {
    use crate::data_recovery::backup_admission::acl_listing_grants_access;
    let (temporary, store) = posix_fixture();
    for (name, entry) in [
        ("list", "group:everyone allow list,search"),
        ("read", "group:everyone allow readattr,readextattr,readsecurity"),
    ] {
        let destination = folder(temporary.path(), name, 0o700);
        add_acl(&destination, entry);
        let error = create_encrypted_backup(&store, &request(&destination, true))
            .expect_err("an ACL grant refuses a private destination");
        assert!(matches!(error, DataRecoveryError::InvalidBackup(_)), "{name}: {error}");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0, "{name}: nothing written");
        // A read-only grant is still admitted by the schedule's rule.
        create_encrypted_backup(&store, &request(&destination, false)).expect("no write granted");
    }
    let denied = folder(temporary.path(), "denied", 0o700);
    add_acl(&denied, "group:everyone deny delete");
    create_encrypted_backup(&store, &request(&denied, true)).expect("a deny entry admits");

    assert!(acl_listing_grants_access(
        "drwx------  2 hub  staff  64 Nov 14 06:00 d\n 0: group:everyone allow search\n"
    ));
    assert!(acl_listing_grants_access(
        "drwx------  2 hub  staff  64 Nov 14 06:00 d\n 0: user:other inherited allow list\n"
    ));
    assert!(!acl_listing_grants_access(
        "drwx------  2 hub  staff  64 Nov 14 06:00 d\n 0: group:everyone deny delete\n"
    ));
    assert!(!acl_listing_grants_access("drwx------  2 hub  staff  64 Nov 14 06:00 d\n"));
    test_seam::force(None);
}

#[cfg(target_os = "macos")]
fn add_acl(path: &Path, entry: &str) {
    let status = std::process::Command::new("/bin/chmod")
        .arg("+a")
        .arg(entry)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "cannot set `{entry}` on {}", path.display());
}

/// Finding 2: an ancestor of the destination swapped for a link into the live
/// data directory just before the open is caught by the containment check on
/// the directory actually opened; an ancestor swapped back after the open is
/// caught because the path no longer names the held directory. Nothing is
/// written into live data either way.
#[test]
fn data_recovery_f2_ancestor_swap_cannot_redirect_the_destination() {
    let (temporary, store) = posix_fixture();
    let live = store.database_path().parent().expect("live data").to_path_buf();
    let inside = folder(&live, "backups", 0o700);
    let outer = temporary.path().join("outer");
    fs::create_dir(&outer).expect("outer");
    let destination = folder(&outer, "backups", 0o700);
    let parked = temporary.path().join("outer-parked");

    let swap = {
        let (outer, parked, live) = (outer.clone(), parked.clone(), live.clone());
        move |stage: &str| {
            if stage == "before_open" {
                fs::rename(&outer, &parked).expect("park the real ancestor");
                std::os::unix::fs::symlink(&live, &outer).expect("ancestor link into live data");
            }
        }
    };
    test_seam::on_destination(Some(swap));
    let error = create_encrypted_backup(&store, &request(&destination, true))
        .expect_err("the opened directory is inside live data");
    test_seam::on_destination(None::<fn(&str)>);
    assert!(error.to_string().contains("live Hub data directory"), "{error}");
    assert_eq!(fs::read_dir(&inside).unwrap().count(), 0, "nothing written into live data");

    // Restore the real ancestor, then swap it for the link only after the open.
    fs::remove_file(&outer).expect("remove the link");
    fs::rename(&parked, &outer).expect("restore the real ancestor");
    let swap_after = {
        let (outer, parked, live) = (outer.clone(), parked.clone(), live.clone());
        move |stage: &str| {
            if stage == "after_open" {
                fs::rename(&outer, &parked).expect("park the real ancestor");
                std::os::unix::fs::symlink(&live, &outer).expect("ancestor link into live data");
            }
        }
    };
    test_seam::on_destination(Some(swap_after));
    let refused = create_encrypted_backup(&store, &request(&destination, true));
    test_seam::on_destination(None::<fn(&str)>);
    assert!(refused.is_err(), "the path no longer names the opened directory: {refused:?}");
    assert_eq!(fs::read_dir(&inside).unwrap().count(), 0, "nothing written into live data");
    assert_eq!(fs::read_dir(parked.join("backups")).unwrap().count(), 0, "nothing published");
    test_seam::force(None);
}

/// The child for a failing restoration: the seam makes every restoration of
/// the saved terminal settings fail. Outside the harness it does nothing.
#[test]
fn data_recovery_pty_child_with_failing_restore() {
    let Ok(_scenario) = std::env::var(PTY_SCENARIO) else {
        return;
    };
    crate::data_recovery::recovery_password::test_seam::fail_restore();
    println!("CHILD-PID {}", std::process::id());
    pty_child_report(read_recovery_password(false));
}

/// Finding 6: a restoration that fails is reported as an error naming the
/// way out, and the password read before it is not used.
#[cfg(target_os = "macos")]
#[test]
fn data_recovery_f6_pty_failed_restoration_is_reported() {
    let mut pty = Pty::start("m06_t14_seams::data_recovery_pty_child_with_failing_restore", "single");
    wait_for_prompt(&mut pty);
    pty.send(b"correct horse battery staple\n");
    let shown = finish_with_next_line(pty);
    assert!(shown.contains("OUTCOME refused"), "{shown}");
    assert!(shown.contains("terminal echo could not be restored"), "{shown}");
    assert!(shown.contains("stty sane"), "{shown}");
    assert!(!shown.contains("OUTCOME accepted"), "{shown}");
    assert!(!shown.contains("correct horse"), "nothing is echoed:\n{shown}");
}
