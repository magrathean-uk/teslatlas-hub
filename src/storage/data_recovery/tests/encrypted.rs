// SPDX-License-Identifier: AGPL-3.0-only

// M06-T01: the service-owned encrypted backup operation and its failure
// handling. Synthetic data only, in temporary stores; no network and no
// collector run. Every failure case of the milestone's first Definition of
// Done item has a test here, and each one checks that the last good
// generation is still present and still verifies.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};

use super::*;
use crate::data_recovery::{
    EncryptedBackupReport, EncryptedBackupRequest, create_encrypted_backup,
    encrypted_backup::{HookAction, clear_hook, set_hook},
    generate_recovery_password, restore_encrypted_backup, verify_encrypted_backup,
};

const PASSWORD: &str = "correct horse battery staple";

struct Rig {
    temporary: tempfile::TempDir,
    store: HubStore,
    destination: PathBuf,
    scratch: PathBuf,
}

/// A store holding one pack and one signed no-op body, a destination folder
/// and a scratch folder outside it.
fn rig() -> Rig {
    clear_hook();
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let destination = temporary.path().join("destination");
    let scratch = temporary.path().join("scratch");
    fs::create_dir(&destination).expect("destination");
    fs::create_dir(&scratch).expect("scratch");
    Rig {
        temporary,
        store,
        destination,
        scratch,
    }
}

impl Rig {
    fn request(&self) -> EncryptedBackupRequest<'_> {
        let mut request = EncryptedBackupRequest::new(&self.destination, PASSWORD);
        request.kdf_iterations = 2;
        request.chunk_bytes = 4096;
        request.scratch_parent = Some(self.scratch.clone());
        request
    }

    fn backup(&self) -> EncryptedBackupReport {
        create_encrypted_backup(&self.store, &self.request()).expect("encrypted backup")
    }

    /// Every entry of the destination, by name.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.destination)
            .expect("destination listing")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn assert_only_good(&self, generations: &[&EncryptedBackupReport]) {
        let mut expected: Vec<String> = generations
            .iter()
            .map(|report| {
                report
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        expected.sort();
        assert_eq!(self.entries(), expected, "only good generations remain");
        for report in generations {
            verify_encrypted_backup(&report.path, PASSWORD).expect("good generation verifies");
        }
        assert!(
            fs::read_dir(&self.scratch).unwrap().next().is_none(),
            "no scratch is left behind"
        );
    }
}

fn tick() {
    // Generation names carry the millisecond, so consecutive runs sort.
    std::thread::sleep(std::time::Duration::from_millis(3));
}

fn the_pack(store: &HubStore) -> PathBuf {
    fs::read_dir(store.packs_dir().join("sha256"))
        .expect("pack directory")
        .map(|entry| entry.expect("pack entry").path())
        .next()
        .expect("a pack")
}

#[test]
fn pbkdf2_matches_the_published_vectors() {
    use crate::data_recovery::archive_crypto::pbkdf2_hmac_sha256_32;
    // RFC 7914 section 11 (first 32 bytes) and a key longer than the HMAC block.
    assert_eq!(
        hex::encode(*pbkdf2_hmac_sha256_32(b"passwd", b"salt", 1)),
        "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
    );
    assert_eq!(
        hex::encode(*pbkdf2_hmac_sha256_32(b"Password", b"NaCl", 80_000)),
        "4ddcd8f60b98be21830cee5ef22701f9641a4418d04c0414aeff08876b34ab56"
    );
    assert_eq!(
        hex::encode(*pbkdf2_hmac_sha256_32(&[b'p'; 100], b"salt", 3)),
        "f598272d35e2ca276ac07694cf01636c4d643ad3075956477cfdd83eda46d9f6"
    );
}

#[test]
fn recovery_password_is_160_bits_in_dashed_groups() {
    let first = generate_recovery_password().expect("password");
    let second = generate_recovery_password().expect("password");
    assert_ne!(first, second);
    let groups: Vec<&str> = first.split('-').collect();
    assert_eq!(groups.len(), 8);
    assert!(groups.iter().all(|group| {
        group.len() == 4
            && group
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || (b'2'..=b'7').contains(&byte))
    }));
}

#[test]
fn data_recovery_encrypted_round_trip_restores_the_data() {
    let rig = rig();
    let report = rig.backup();
    assert_eq!(report.status, "encrypted_backup_created");
    assert_eq!(report.rotated_generations, 0);
    assert!(report.rotation_error.is_none());
    assert_eq!(report.installation_id, rig.store.installation_id().unwrap());
    let name = report
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(name.starts_with("teslatlas-hub-backup-") && name.ends_with(".tlbk"));
    assert_eq!(rig.entries(), vec![name]);
    let archive = fs::metadata(&report.path).unwrap();
    assert_eq!(permission_mode(&archive), 0o600);
    assert_eq!(archive.len(), report.archive_bytes);
    assert!(report.archive_bytes > report.payload_bytes);

    let verified = verify_encrypted_backup(&report.path, PASSWORD).expect("verify");
    assert_eq!(verified.generation, report.generation);
    assert_eq!(verified.member_count, report.member_count);

    let restored = rig.temporary.path().join("restored");
    let restored_report =
        restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore");
    assert_eq!(restored_report.status, STATUS_DATA_RESTORED);
    assert_eq!(
        restored_report.installation_id,
        rig.store.installation_id().unwrap()
    );
    // The pack and the no-op body come back byte for byte.
    let pack = the_pack(&rig.store);
    assert_eq!(
        fs::read(
            restored
                .join("packs/sha256")
                .join(pack.file_name().unwrap())
        )
        .unwrap(),
        fs::read(&pack).unwrap()
    );
    let noop_name = fs::read_dir(restored.join("packs/noop"))
        .unwrap()
        .next()
        .expect("restored no-op")
        .unwrap()
        .file_name();
    assert!(restored.join("packs/noop").join(noop_name).is_file());
    // Pairing authority is not carried (sealing removes it).
    assert_eq!(pairing_authority_counts(&restored), (0, 0));
    // The restore leaves the source backup alone.
    verify_encrypted_backup(&report.path, PASSWORD).expect("verify again");
}

#[test]
fn backup_no_plaintext_staging() {
    let rig = rig();
    let marker_pack = fs::read(the_pack(&rig.store)).expect("pack bytes");
    let sqlite_magic = b"SQLite format 3\0".to_vec();
    let hook_magic = sqlite_magic.clone();
    let destination = rig.destination.clone();
    let observed = Rc::new(Cell::new(0_u32));
    let leaks = Rc::new(RefCell::new(Vec::<String>::new()));
    let (seen, leaked) = (observed.clone(), leaks.clone());
    let pack_probe = marker_pack[..marker_pack.len().min(32)].to_vec();
    set_hook(move |stage, _| {
        if stage == "write" {
            seen.set(seen.get() + 1);
            for entry in fs::read_dir(&destination).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                if !(name.starts_with(".teslatlas-hub-backup-") && name.ends_with(".partial")) {
                    leaked.borrow_mut().push(format!("unexpected entry {name}"));
                    continue;
                }
                let bytes = fs::read(entry.path()).unwrap();
                for (label, needle) in [("sqlite header", &hook_magic), ("pack", &pack_probe)] {
                    if bytes
                        .windows(needle.len())
                        .any(|window| window == needle.as_slice())
                    {
                        leaked.borrow_mut().push(format!("{label} found in {name}"));
                    }
                }
            }
        }
        HookAction::Continue
    });
    let report = rig.backup();
    clear_hook();
    assert!(observed.get() > 4, "the stream wrote in many steps");
    assert!(
        leaks.borrow().is_empty(),
        "plaintext staged: {:?}",
        leaks.borrow()
    );

    let archive = fs::read(&report.path).expect("archive bytes");
    for needle in [
        sqlite_magic.as_slice(),
        &marker_pack[..marker_pack.len().min(32)],
    ] {
        assert!(
            !archive.windows(needle.len()).any(|window| window == needle),
            "the archive holds plaintext"
        );
    }
    assert_eq!(rig.entries().len(), 1);
    assert!(
        fs::read_dir(&rig.scratch).unwrap().next().is_none(),
        "the scratch catalogue copy is removed"
    );
}

#[test]
fn data_recovery_wrong_password_is_refused_without_writes() {
    let rig = rig();
    let report = rig.backup();
    let before = fs::read(&report.path).unwrap();
    let restored = rig.temporary.path().join("restored");
    assert!(matches!(
        verify_encrypted_backup(&report.path, "not the right password"),
        Err(DataRecoveryError::WrongPassword)
    ));
    assert!(matches!(
        restore_encrypted_backup(&report.path, "not the right password", &restored),
        Err(DataRecoveryError::WrongPassword)
    ));
    assert!(!restored.exists());
    assert_eq!(
        fs::read_dir(rig.temporary.path()).unwrap().count(),
        3,
        "no staging directory is left beside the destination"
    );
    assert_eq!(fs::read(&report.path).unwrap(), before);
    rig.assert_only_good(&[&report]);
}

#[test]
fn data_recovery_weak_password_and_unsafe_locations_are_refused() {
    let rig = rig();
    let mut request = rig.request();
    request.password = "short";
    assert!(create_encrypted_backup(&rig.store, &request).is_err());
    assert!(rig.entries().is_empty());

    let inside = rig.store.database_path().parent().unwrap().to_path_buf();
    let request = EncryptedBackupRequest::new(&inside, PASSWORD);
    assert!(create_encrypted_backup(&rig.store, &request).is_err());

    let mut request = rig.request();
    request.scratch_parent = Some(rig.destination.clone());
    assert!(create_encrypted_backup(&rig.store, &request).is_err());
    assert!(rig.entries().is_empty());
}

#[test]
fn data_recovery_corruption_is_detected_and_nothing_is_published() {
    let rig = rig();
    let report = rig.backup();
    let good = fs::read(&report.path).unwrap();
    let restored = rig.temporary.path().join("restored");
    let mutate = |name: &str, bytes: Vec<u8>| {
        let path = rig.temporary.path().join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    };
    let mut flipped = good.clone();
    let middle = flipped.len() / 2;
    flipped[middle] ^= 0x01;
    let mut extended = good.clone();
    extended.extend_from_slice(b"extra");
    let mut reordered = good.clone();
    // Swap the first two whole chunks after the header.
    let chunk = 4096 + 16;
    let (first, second) = (80, 80 + chunk);
    let swapped: Vec<u8> = reordered[second..second + chunk].to_vec();
    let original: Vec<u8> = reordered[first..first + chunk].to_vec();
    reordered[first..first + chunk].copy_from_slice(&swapped);
    reordered[second..second + chunk].copy_from_slice(&original);
    let cases = [
        ("flipped", flipped),
        ("truncated_mid_chunk", good[..good.len() - 7].to_vec()),
        ("truncated_at_a_chunk", good[..80 + chunk * 3].to_vec()),
        ("header_only", good[..80].to_vec()),
        ("extended", extended),
        ("reordered", reordered),
        ("empty", Vec::new()),
        (
            "not_an_archive",
            b"this is not a teslatlas archive at all, only text".repeat(4),
        ),
    ];
    for (name, bytes) in cases {
        let path = mutate(name, bytes);
        assert!(
            matches!(
                verify_encrypted_backup(&path, PASSWORD),
                Err(DataRecoveryError::CorruptArchive(_))
            ),
            "{name} must be reported as a damaged archive"
        );
        assert!(
            restore_encrypted_backup(&path, PASSWORD, &restored).is_err(),
            "{name} must not restore"
        );
        assert!(!restored.exists(), "{name} published nothing");
        assert!(
            fs::read_dir(rig.temporary.path())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("staging")),
            "{name} left no staging directory"
        );
    }
    // A damaged header fails its key check; a damaged parameter is refused.
    let mut header_flip = good.clone();
    header_flip[30] ^= 0x01; // inside the salt
    let path = mutate("salt_flipped", header_flip);
    assert!(matches!(
        verify_encrypted_backup(&path, PASSWORD),
        Err(DataRecoveryError::WrongPassword)
    ));
    let mut version = good;
    version[8] = 9;
    let path = mutate("version", version);
    assert!(matches!(
        verify_encrypted_backup(&path, PASSWORD),
        Err(DataRecoveryError::CorruptArchive(_))
    ));
    rig.assert_only_good(&[&report]);
}

#[test]
fn data_recovery_interruption_keeps_the_last_good_generation() {
    let rig = rig();
    let first = rig.backup();
    tick();

    // A process killed mid-write leaves its partial file behind.
    let writes = Rc::new(Cell::new(0_u32));
    let counter = writes.clone();
    set_hook(move |stage, _| {
        if stage == "write" {
            counter.set(counter.get() + 1);
            if counter.get() == 6 {
                return HookAction::Crash;
            }
        }
        HookAction::Continue
    });
    assert!(create_encrypted_backup(&rig.store, &rig.request()).is_err());
    clear_hook();
    let leftovers: Vec<String> = rig
        .entries()
        .into_iter()
        .filter(|name| name.ends_with(".partial"))
        .collect();
    assert_eq!(leftovers.len(), 1, "the crash left one partial file");
    verify_encrypted_backup(&first.path, PASSWORD).expect("last good generation intact");

    // The next run clears the stale partial and publishes.
    tick();
    let second = rig.backup();
    rig.assert_only_good(&[&first, &second]);

    // A failing write (broken pipe) is cleaned up at once.
    tick();
    set_hook(|stage, _| {
        if stage == "write" {
            HookAction::Fail(io::ErrorKind::BrokenPipe)
        } else {
            HookAction::Continue
        }
    });
    assert!(matches!(
        create_encrypted_backup(&rig.store, &rig.request()),
        Err(DataRecoveryError::Io { .. })
    ));
    clear_hook();
    rig.assert_only_good(&[&first, &second]);
}

#[test]
fn data_recovery_unavailable_destination_is_refused_before_any_write() {
    let rig = rig();
    let good = rig.backup();
    let missing = rig.temporary.path().join("no-such-folder");
    let request = EncryptedBackupRequest::new(&missing, PASSWORD);
    assert!(matches!(
        create_encrypted_backup(&rig.store, &request),
        Err(DataRecoveryError::DestinationUnavailable { .. })
    ));
    let not_a_folder = rig.temporary.path().join("a-file");
    fs::write(&not_a_folder, b"x").unwrap();
    let request = EncryptedBackupRequest::new(&not_a_folder, PASSWORD);
    assert!(matches!(
        create_encrypted_backup(&rig.store, &request),
        Err(DataRecoveryError::DestinationUnavailable { .. })
    ));
    // A folder the Hub cannot write to (a removed or read-only volume).
    let read_only = rig.temporary.path().join("read-only");
    fs::create_dir(&read_only).unwrap();
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o500)).unwrap();
    let mut request = rig.request();
    request.destination = &read_only;
    let result = create_encrypted_backup(&rig.store, &request);
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        result,
        Err(DataRecoveryError::DestinationUnavailable { .. })
    ));
    assert!(fs::read_dir(&read_only).unwrap().next().is_none());
    // The destination vanishing mid-run fails the run and keeps what was good.
    tick();
    set_hook(|stage, _| {
        if stage == "write" {
            HookAction::Fail(io::ErrorKind::NotFound)
        } else {
            HookAction::Continue
        }
    });
    assert!(create_encrypted_backup(&rig.store, &rig.request()).is_err());
    clear_hook();
    rig.assert_only_good(&[&good]);
    // The publication gate was released by every refusal.
    drop(
        rig.store
            .try_acquire_publication_gate()
            .expect("gate is free"),
    );
}

#[test]
fn data_recovery_full_destination_keeps_the_last_good_generation() {
    let rig = rig();
    let good = rig.backup();
    tick();
    set_hook(|stage, _| {
        if stage == "write" {
            HookAction::Fail(io::ErrorKind::StorageFull)
        } else {
            HookAction::Continue
        }
    });
    let result = create_encrypted_backup(&rig.store, &rig.request());
    clear_hook();
    assert!(matches!(
        result,
        Err(DataRecoveryError::DestinationFull { .. })
    ));
    rig.assert_only_good(&[&good]);

    // Up-front admission also refuses a destination without room.
    assert!(matches!(
        admit_known_capacity(Path::new("/destination"), 100, 99),
        Err(DataRecoveryError::InsufficientFreeSpace { .. })
    ));
}

#[test]
fn data_recovery_cancellation_keeps_the_last_good_generation() {
    let rig = rig();
    let good = rig.backup();
    tick();

    // Cancelled before the run starts: nothing is touched.
    let flag = Arc::new(AtomicBool::new(true));
    let mut request = rig.request();
    request.cancel = Some(flag);
    assert!(matches!(
        create_encrypted_backup(&rig.store, &request),
        Err(DataRecoveryError::Cancelled)
    ));
    rig.assert_only_good(&[&good]);

    // Cancelled while members stream.
    let flag = Arc::new(AtomicBool::new(false));
    let trigger = flag.clone();
    let records = Rc::new(Cell::new(0_u32));
    let counter = records.clone();
    set_hook(move |stage, _| {
        if stage == "record" {
            counter.set(counter.get() + 1);
            if counter.get() == 2 {
                trigger.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        HookAction::Continue
    });
    let mut request = rig.request();
    request.cancel = Some(flag.clone());
    let result = create_encrypted_backup(&rig.store, &request);
    clear_hook();
    assert!(matches!(result, Err(DataRecoveryError::Cancelled)));
    rig.assert_only_good(&[&good]);

    // Cancelled after the archive is complete but before it is published.
    let flag = Arc::new(AtomicBool::new(false));
    let trigger = flag.clone();
    set_hook(move |stage, _| {
        if stage == "before_publish" {
            trigger.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        HookAction::Continue
    });
    let mut request = rig.request();
    request.cancel = Some(flag);
    let result = create_encrypted_backup(&rig.store, &request);
    clear_hook();
    assert!(matches!(result, Err(DataRecoveryError::Cancelled)));
    rig.assert_only_good(&[&good]);
}

#[test]
fn data_recovery_concurrent_compaction_cannot_change_the_run_or_the_last_good_generation() {
    let rig = rig();
    let good = rig.backup();
    tick();

    // Anything that takes the publication gate (publication, compaction, GC of
    // packs) is held off for the whole run, and a second backup is refused.
    let other =
        HubStore::initialize(rig.store.database_path().parent().unwrap()).expect("second handle");
    let outcomes = Rc::new(RefCell::new(Vec::<String>::new()));
    let log = outcomes.clone();
    let second_destination = rig.destination.clone();
    set_hook(move |stage, _| {
        if stage == "record" {
            log.borrow_mut()
                .push(match other.try_acquire_publication_gate() {
                    Err(StoreError::PublicationGateBusy) => "gate busy".to_owned(),
                    Err(error) => format!("unexpected gate error {error}"),
                    Ok(_) => "gate was free".to_owned(),
                });
            let request = EncryptedBackupRequest::new(&second_destination, PASSWORD);
            log.borrow_mut()
                .push(match create_encrypted_backup(&other, &request) {
                    Err(DataRecoveryError::Store(StoreError::PublicationGateBusy)) => {
                        "second backup refused".to_owned()
                    }
                    other => format!("unexpected second backup {other:?}"),
                });
        }
        HookAction::Continue
    });
    let second = rig.backup();
    clear_hook();
    let outcomes = outcomes.borrow();
    assert!(!outcomes.is_empty());
    assert!(
        outcomes
            .iter()
            .all(|line| line == "gate busy" || line == "second backup refused"),
        "{outcomes:?}"
    );
    rig.assert_only_good(&[&good, &second]);
    fs::remove_file(&second.path).unwrap();

    // A pack removed under the run (a compaction that ignored the gate) fails
    // the run instead of producing a generation with a hole.
    let pack = the_pack(&rig.store);
    let pack_copy = fs::read(&pack).unwrap();
    let victim = pack.clone();
    set_hook(move |stage, _| {
        if stage == "record" {
            let _ = fs::remove_file(&victim);
        }
        HookAction::Continue
    });
    let result = create_encrypted_backup(&rig.store, &rig.request());
    clear_hook();
    assert!(result.is_err(), "a pack that vanished fails the run");
    rig.assert_only_good(&[&good]);
    fs::write(&pack, pack_copy).unwrap();
    fs::set_permissions(&pack, fs::Permissions::from_mode(0o600)).unwrap();

    // A pack rewritten under the run no longer matches its address.
    let victim = pack.clone();
    let done = Cell::new(false);
    set_hook(move |stage, _| {
        if stage == "record" && !done.replace(true) {
            let mut bytes = fs::read(&victim).unwrap();
            bytes[0] ^= 0xFF;
            fs::write(&victim, bytes).unwrap();
        }
        HookAction::Continue
    });
    let result = create_encrypted_backup(&rig.store, &rig.request());
    clear_hook();
    assert!(result.is_err(), "a rewritten pack fails the run");
    rig.assert_only_good(&[&good]);
}

#[test]
fn data_recovery_rotation_failure_keeps_the_new_and_the_last_good_generations() {
    let rig = rig();
    let oldest = rig.backup();
    tick();
    let middle = rig.backup();
    tick();
    // A foreign file and a lookalike directory are never rotation targets.
    fs::write(rig.destination.join("notes.txt"), b"keep me").unwrap();
    fs::write(
        rig.destination.join("teslatlas-hub-backup-foreign.txt"),
        b"keep me",
    )
    .unwrap();

    set_hook(|stage, _| {
        if stage == "rotate_remove" {
            HookAction::Fail(io::ErrorKind::PermissionDenied)
        } else {
            HookAction::Continue
        }
    });
    let mut request = rig.request();
    request.keep = std::num::NonZeroUsize::new(1);
    let report = create_encrypted_backup(&rig.store, &request).expect("backup despite rotation");
    clear_hook();
    assert_eq!(report.rotated_generations, 0);
    assert!(
        report.rotation_error.is_some(),
        "the rotation failure is reported"
    );
    for generation in [&oldest, &middle, &report] {
        verify_encrypted_backup(&generation.path, PASSWORD).expect("every generation survives");
    }

    // Without the fault, rotation keeps the newest N and nothing else goes.
    tick();
    let mut request = rig.request();
    request.keep = std::num::NonZeroUsize::new(2);
    let latest = create_encrypted_backup(&rig.store, &request).expect("rotating backup");
    assert!(latest.rotation_error.is_none());
    assert_eq!(latest.rotated_generations, 2);
    assert!(!oldest.path.exists() && !middle.path.exists());
    verify_encrypted_backup(&report.path, PASSWORD).expect("kept generation");
    verify_encrypted_backup(&latest.path, PASSWORD).expect("newest generation");
    assert_eq!(
        fs::read(rig.destination.join("notes.txt")).unwrap(),
        b"keep me"
    );
    assert!(
        rig.destination
            .join("teslatlas-hub-backup-foreign.txt")
            .exists()
    );
}
