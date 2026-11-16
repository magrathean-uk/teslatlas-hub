// SPDX-License-Identifier: AGPL-3.0-only

// M06-T11: REV-M06-T06 findings 7, 1 and 4. A backup reports success, and the
// schedule records a last good generation, only once the complete published
// file has authenticated as the object the run wrote; ACL inspection is bound
// to the held object; an interrupted or refused restore leaves no plaintext
// staging behind. Synthetic data only, in temporary directories.

use std::{num::NonZeroUsize, sync::Arc};

use serde_json::Value;

use super::*;
use crate::{
    data_recovery::{
        EncryptedBackupReport, EncryptedBackupRequest,
        archive_crypto::KdfParams,
        backup_admission::{VolumeTrust, test_seam},
        create_encrypted_backup,
        encrypted_backup::{HookAction, clear_hook, set_hook},
        restore_encrypted_backup, verify_encrypted_backup,
    },
    runtime::{
        backup_schedule::{BackupJob, BackupRunner, BackupScheduler, BackupSettings, refusal_from},
        operator::admission::OperatorRefusal,
    },
};

const PASSWORD: &str = "correct horse battery staple";
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};
/// The refusal code and the error prefix of an invalid publication, written
/// out so the test states the contract rather than borrowing it.
const PUBLICATION_INVALID_CODE: &str = "backup_publication_invalid";
const INVALID_PUBLICATION: &str = "invalid_publication";

struct Hub {
    temporary: tempfile::TempDir,
    store: HubStore,
    destination: PathBuf,
}

fn hub(trust: VolumeTrust) -> Hub {
    clear_hook();
    test_seam::force(Some(trust));
    test_seam::on_acl_listing(None::<fn(&str)>);
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let destination = temporary.path().join("destination");
    fs::create_dir(&destination).expect("destination");
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).expect("private");
    Hub {
        temporary,
        store,
        destination,
    }
}

fn request<'a>(destination: &'a Path, keep: Option<usize>) -> EncryptedBackupRequest<'a> {
    let mut request = EncryptedBackupRequest::new(destination, PASSWORD);
    request.kdf = TEST_KDF;
    request.chunk_bytes = 4096;
    request.keep = keep.and_then(NonZeroUsize::new);
    request
}

impl Hub {
    fn backup(&self, keep: Option<usize>) -> Result<EncryptedBackupReport, DataRecoveryError> {
        create_encrypted_backup(&self.store, &request(&self.destination, keep))
    }
}

fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(3));
}

fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("listing")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn assert_invalid_publication(result: Result<EncryptedBackupReport, DataRecoveryError>) {
    match result {
        Err(DataRecoveryError::InvalidBackup(message)) => assert!(
            message.starts_with(INVALID_PUBLICATION),
            "an invalid publication names itself: {message}"
        ),
        Err(other) => panic!("expected an invalid publication, got {other}"),
        Ok(report) => panic!(
            "a publication that does not authenticate was reported as a success: {}",
            report.path.display()
        ),
    }
}

/// Truncate the closed partial in place before it is published: the run's
/// own object, no longer a complete generation.
fn truncate_before_publish() {
    set_hook(|stage, path| {
        if stage == "before_publish" {
            let file = fs::OpenOptions::new().write(true).open(path).unwrap();
            let length = file.metadata().unwrap().len();
            file.set_len(length / 2).unwrap();
        }
        HookAction::Continue
    });
}

/// Replace the closed partial with `stand_in`'s bytes before publication.
fn substitute_before_publish(stand_in: PathBuf, bytes: Vec<u8>) {
    set_hook(move |stage, path| {
        if stage == "before_publish" {
            fs::write(&stand_in, &bytes).unwrap();
            fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o600)).unwrap();
            fs::rename(&stand_in, path).unwrap();
        }
        HookAction::Continue
    });
}

// Finding 7 ------------------------------------------------------------------

/// Without rotation the published file used not to be read back at all.
#[test]
fn data_recovery_m06_t11_a_damaged_publication_is_an_error_not_a_success() {
    let hub = hub(VolumeTrust::Posix);
    let first = hub.backup(None).expect("first generation");
    tick();

    truncate_before_publish();
    let damaged = hub.backup(None);
    clear_hook();
    assert_invalid_publication(damaged);
    verify_encrypted_backup(&first.path, PASSWORD).expect("the last good generation stays");

    // With rotation, the same damage is an invalid publication too, not a
    // pruning warning, and nothing older goes.
    tick();
    truncate_before_publish();
    let rotating = hub.backup(Some(1));
    clear_hook();
    assert_invalid_publication(rotating);
    verify_encrypted_backup(&first.path, PASSWORD).expect("nothing older is pruned");

    // A clean run still succeeds and prunes the genuine older generation;
    // the damaged files neither count nor go.
    tick();
    let clean = hub.backup(Some(1)).expect("clean run");
    assert_eq!(clean.rotated_generations, 1, "{:?}", clean.rotation_error);
    assert!(!first.path.exists(), "the genuine older generation was rotated");
    verify_encrypted_backup(&clean.path, PASSWORD).expect("newest verifies");
}

/// A destination without ownership relies on the authenticated encryption:
/// the published file is authenticated there too, and a substitute is an
/// invalid publication, whether garbage or another genuine generation.
#[test]
fn data_recovery_m06_t11_a_substituted_publication_on_an_encryption_only_destination_is_refused() {
    let hub = hub(VolumeTrust::EncryptionOnly);
    let first = hub.backup(None).expect("first generation");
    tick();

    substitute_before_publish(hub.temporary.path().join("stand-in"), vec![0x42_u8; 8192]);
    let garbage = hub.backup(Some(1));
    clear_hook();
    assert_invalid_publication(garbage);
    tick();

    // A replay of this Hub's own older generation authenticates, but it is
    // not the object this run wrote.
    let replay = fs::read(&first.path).unwrap();
    substitute_before_publish(hub.temporary.path().join("stand-in"), replay);
    let replayed = hub.backup(None);
    clear_hook();
    assert_invalid_publication(replayed);
    verify_encrypted_backup(&first.path, PASSWORD).expect("the last good generation stays");
    tick();

    // A clean run on such a volume succeeds; it keeps every older generation
    // and says so, as decided.
    let clean = hub.backup(Some(1)).expect("clean run");
    assert_eq!(clean.rotated_generations, 0);
    assert!(clean.rotation_error.is_some(), "the kept generations are reported");
    verify_encrypted_backup(&clean.path, PASSWORD).expect("newest verifies");
    verify_encrypted_backup(&first.path, PASSWORD).expect("older generation kept");
}

/// The schedule's runner, as the service's, with the test password cost.
struct TestRunner {
    store: HubStore,
}

impl BackupRunner for TestRunner {
    fn run(&self, job: &BackupJob) -> Result<Value, OperatorRefusal> {
        let mut request = request(&job.destination, job.keep.map(NonZeroUsize::get));
        request.cancel = Some(Arc::clone(&job.cancel));
        let report =
            create_encrypted_backup(&self.store, &request).map_err(|error| refusal_from(&error, None))?;
        Ok(serde_json::to_value(&report).unwrap())
    }
}

#[test]
fn data_recovery_m06_t11_backup_schedule_records_no_last_good_for_an_invalid_publication() {
    let hub = hub(VolumeTrust::Posix);
    let settings = BackupSettings {
        enabled: false,
        destination: Some(hub.destination.clone()),
        password: Some(crate::runtime::backup_schedule::PasswordSource::File(hub.temporary.path().join("unused-password"))),
        interval_ms: 3_600_000,
        keep: NonZeroUsize::new(2),
        retry_ms: 60_000,
        startup_delay_ms: 0,
    };
    let schedule = BackupScheduler::start(
        settings,
        Arc::new(TestRunner {
            store: hub.store.clone(),
        }),
        hub.temporary.path().join("schedule-state.json"),
        Box::new(|| false),
    )
    .expect("schedule");

    truncate_before_publish();
    let refused = schedule.run_now(None, None);
    clear_hook();
    let refusal = refused.expect_err("an invalid publication is not a success");
    assert_eq!(refusal.code, PUBLICATION_INVALID_CODE, "{}", refusal.message);
    let state = schedule.last();
    assert_eq!(state.last_success_ms, None, "no last good time");
    assert_eq!(state.last_generation, None, "no last good generation");
    assert_eq!(state.last_failure_code.as_deref(), Some(PUBLICATION_INVALID_CODE));

    tick();
    let report = schedule.run_now(None, None).expect("clean run");
    let state = schedule.last();
    assert!(state.last_success_ms.is_some());
    assert_eq!(
        state.last_generation.as_deref(),
        report.get("generation").and_then(Value::as_str)
    );
    schedule.stop();
}

// Finding 1 ------------------------------------------------------------------

/// Swap an ancestor of `object` for a decoy with an ACL-free object at the
/// same path while the first ACL listing runs, and restore it afterwards.
#[cfg(target_os = "macos")]
fn swap_ancestor_during_first_listing(ancestor: PathBuf, decoy_object: PathBuf, is_directory: bool) {
    let mut calls = 0_u32;
    test_seam::on_acl_listing(Some(move |stage: &str| {
        let held = ancestor.with_extension("held");
        match stage {
            "before_listing" => {
                calls += 1;
                if calls == 1 {
                    fs::rename(&ancestor, &held).unwrap();
                    fs::create_dir_all(decoy_object.parent().unwrap()).unwrap();
                    if is_directory {
                        fs::create_dir(&decoy_object).unwrap();
                        fs::set_permissions(&decoy_object, fs::Permissions::from_mode(0o700)).unwrap();
                    } else {
                        fs::write(&decoy_object, b"decoy").unwrap();
                        fs::set_permissions(&decoy_object, fs::Permissions::from_mode(0o600)).unwrap();
                    }
                }
            }
            "after_listing" if held.exists() => {
                fs::remove_dir_all(&ancestor).unwrap();
                fs::rename(&held, &ancestor).unwrap();
            }
            _ => {}
        }
    }));
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

#[cfg(target_os = "macos")]
#[test]
fn data_recovery_m06_t11_acl_inspection_is_bound_to_the_held_object_through_an_ancestor_swap() {
    let hub = hub(VolumeTrust::Posix);

    // A destination whose ACL lets everyone add and delete entries.
    let inner = hub.temporary.path().join("outer").join("inner");
    let open_folder = inner.join("destination");
    fs::create_dir_all(&open_folder).unwrap();
    fs::set_permissions(&open_folder, fs::Permissions::from_mode(0o700)).unwrap();
    add_acl(&open_folder, "group:everyone allow list,search,add_file,delete_child");
    swap_ancestor_during_first_listing(inner.clone(), open_folder.clone(), true);
    let admitted = create_encrypted_backup(&hub.store, &request(&open_folder, None));
    test_seam::on_acl_listing(None::<fn(&str)>);
    assert!(
        matches!(admitted, Err(DataRecoveryError::InvalidBackup(_))),
        "the held folder's ACL is refused despite the swap: {admitted:?}"
    );
    assert_eq!(entries(&open_folder), Vec::<String>::new(), "nothing written");

    // An archive whose ACL lets everyone write it.
    let good = hub.backup(None).expect("good generation");
    add_acl(&good.path, "group:everyone allow write,append");
    swap_ancestor_during_first_listing(hub.destination.clone(), good.path.clone(), false);
    let verified = verify_encrypted_backup(&good.path, PASSWORD);
    test_seam::on_acl_listing(None::<fn(&str)>);
    assert!(
        verified.is_err(),
        "the held archive's ACL is refused despite the swap"
    );
}

// Finding 4 ------------------------------------------------------------------

/// Every entry under `root` that holds file bytes, recursively.
fn files_with_bytes(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() && entry.metadata().unwrap().len() > 0 {
                found.push(entry.path());
            }
        }
    }
    found
}

#[test]
fn data_recovery_m06_t11_an_interrupted_restore_leaves_no_plaintext_after_the_next_start() {
    let hub = hub(VolumeTrust::Posix);
    let good = hub.backup(None).expect("good generation");
    let parent = hub.temporary.path().join("restores");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();

    // The process dies after the first member was written in plaintext.
    let mut members = 0_u32;
    set_hook(move |stage, _| {
        if stage == "restore_member" {
            members += 1;
            if members == 2 {
                return HookAction::Crash;
            }
        }
        HookAction::Continue
    });
    let interrupted = restore_encrypted_backup(&good.path, PASSWORD, &parent.join("first"));
    clear_hook();
    assert!(interrupted.is_err());
    assert!(!parent.join("first").exists(), "nothing was adopted");
    assert!(
        !files_with_bytes(&parent).is_empty(),
        "precondition: the interruption left plaintext staging behind"
    );

    // The next start clears it, and restores.
    restore_encrypted_backup(&good.path, PASSWORD, &parent.join("second")).expect("next restore");
    assert_eq!(entries(&parent), vec!["second".to_owned()], "no staging is left");

    // A bundle that fails authentication near its end leaves nothing either.
    let mut bytes = fs::read(&good.path).unwrap();
    let near_end = bytes.len() - 5;
    bytes[near_end] ^= 0x01;
    fs::write(&good.path, &bytes).unwrap();
    let refused = restore_encrypted_backup(&good.path, PASSWORD, &parent.join("third"));
    assert!(
        matches!(refused, Err(DataRecoveryError::CorruptArchive(_))),
        "{refused:?}"
    );
    assert_eq!(entries(&parent), vec!["second".to_owned()], "no staging is left");
}
