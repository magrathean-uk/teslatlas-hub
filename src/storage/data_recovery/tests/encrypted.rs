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
    time::SystemTime,
};

use super::*;
use crate::{
    data_recovery::{
        EncryptedBackupReport, EncryptedBackupRequest,
        archive_crypto::{
            DEFAULT_KDF, HEADER_BYTES, KdfParams, MAX_CHUNK_BYTES, MAX_KDF_MEMORY_KIB, SALT_OFFSET,
            TAG_BYTES, argon2id, derive_keys_for_test,
        },
        create_encrypted_backup,
        encrypted_backup::{
            HUB_PEAK_MEMORY_BYTES, HookAction, MAX_CATALOGUE_IMAGE_BYTES, SERVICE_RESERVE_BYTES,
            STREAM_BUFFER_BYTES, clear_hook, copy_catalogue_into_memory, set_hook,
        },
        generate_recovery_password, live_backup_pack_pins, restore_encrypted_backup,
        verify_encrypted_backup,
    },
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionCarSettings, ProjectionDeltaPackRequest,
        ProjectionPackRequest, ProjectionPackWriter, ProjectionSnapshot,
    },
    protocol::HUB_PROJECTION_SCHEMA_V2,
};

const PASSWORD: &str = "correct horse battery staple";
/// Argon2id at test cost; the format and its vectors use the real cost.
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};

struct Rig {
    temporary: tempfile::TempDir,
    store: HubStore,
    destination: PathBuf,
}

/// A store holding one pack and one signed no-op body, and a destination
/// folder.
fn rig() -> Rig {
    clear_hook();
    // M06-T02: these runs remove partials and rotate, which happens only on a
    // volume that enforces ownership; the scratch volume may not.
    crate::data_recovery::backup_admission::test_seam::force(Some(
        crate::data_recovery::backup_admission::VolumeTrust::Posix,
    ));
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let destination = temporary.path().join("destination");
    fs::create_dir(&destination).expect("destination");
    Rig {
        temporary,
        store,
        destination,
    }
}

impl Rig {
    fn request(&self) -> EncryptedBackupRequest<'_> {
        let mut request = EncryptedBackupRequest::new(&self.destination, PASSWORD);
        request.kdf = TEST_KDF;
        request.chunk_bytes = 4096;
        request
    }

    fn backup(&self) -> EncryptedBackupReport {
        create_encrypted_backup(&self.store, &self.request()).expect("encrypted backup")
    }

    /// A second handle on the same data directory, for hooks.
    fn other_handle(&self) -> HubStore {
        HubStore::initialize(self.store.database_path().parent().unwrap()).expect("second handle")
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

fn pack_path(store: &HubStore, digest: impl std::fmt::Display) -> PathBuf {
    store
        .packs_dir()
        .join("sha256")
        .join(format!("{digest}.sqlite.zst"))
}

/// Pin rows in the live catalogue, live or expired.
fn pin_rows(store: &HubStore) -> i64 {
    let connection = store.open().expect("pin catalogue");
    if !catalogue_has_table(&connection, "backup_pack_pins") {
        return 0;
    }
    connection
        .query_row("SELECT COUNT(*) FROM backup_pack_pins", [], |row| {
            row.get(0)
        })
        .expect("pin rows")
}

fn now_ms() -> i64 {
    current_epoch_ms().expect("clock")
}

#[test]
fn memory_budget_fits_the_hub_peak() {
    // The key derivation finishes and frees its memory before the catalogue
    // copy exists; the noop bodies, the catalogue copy and the stream buffers
    // coexist. The rest of the Hub keeps its reserve beside either phase.
    let kdf_phase = u64::from(MAX_KDF_MEMORY_KIB) * 1024;
    let noop_bodies = MAX_BACKUP_MEMBERS as u64 * MAX_SCHEMA_22_NOOP_BYTES;
    let stream_phase = 2 * MAX_CATALOGUE_IMAGE_BYTES
        + noop_bodies
        + STREAM_BUFFER_BYTES as u64
        + (MAX_CHUNK_BYTES + TAG_BYTES) as u64;
    assert!(SERVICE_RESERVE_BYTES + kdf_phase.max(stream_phase) <= HUB_PEAK_MEMORY_BYTES);
    // A restore holds the derivation, then one chunk and the stream buffer.
    assert!(
        SERVICE_RESERVE_BYTES + kdf_phase + 2 * MAX_CHUNK_BYTES as u64 <= HUB_PEAK_MEMORY_BYTES
    );
    // The default cost is RFC 9106's second recommended option and within it.
    assert_eq!(
        DEFAULT_KDF,
        KdfParams {
            memory_kib: 64 * 1024,
            passes: 3,
            lanes: 4
        }
    );
}

#[test]
fn argon2id_matches_rfc_9106_and_independent_vectors() {
    let hex32 = |password: &[u8], salt: &[u8], secret: &[u8], data: &[u8], params| {
        let mut out = [0_u8; 32];
        argon2id(password, salt, secret, data, params, &mut out).expect("argon2id");
        hex::encode(out)
    };
    // RFC 9106 section 5.3 (Argon2id, version 0x13), which OpenSSL 3.5 also
    // reproduces.
    assert_eq!(
        hex32(
            &[1; 32],
            &[2; 16],
            &[3; 8],
            &[4; 12],
            KdfParams {
                memory_kib: 32,
                passes: 3,
                lanes: 4
            }
        ),
        "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"
    );
    // Computed independently with `openssl kdf ... ARGON2ID` (OpenSSL 3.5.9
    // and 4.1.0): the archive's own shape (no secret, no associated data).
    let salt: Vec<u8> = (0..16).collect();
    assert_eq!(
        hex32(
            PASSWORD.as_bytes(),
            &salt,
            &[],
            &[],
            KdfParams {
                memory_kib: 1024,
                passes: 3,
                lanes: 4
            }
        ),
        "00a25649dddfe222ccc5e274c6e01824376754449f71182d45084a020f3b5e19"
    );
    // The production cost itself.
    assert_eq!(
        hex32(PASSWORD.as_bytes(), &salt, &[], &[], DEFAULT_KDF),
        "853b272a44db1421c02962669a55eb0994f3cab385ed1c4c79253eee19bab49e"
    );
}

#[test]
fn archive_key_schedule_matches_an_independent_vector() {
    // A fixed header (m = 1024 KiB, t = 3, p = 4, 4096-byte chunks, salt
    // 00..0f, nonce prefix a0..a6). The expected keys come from OpenSSL:
    // Argon2id for the master key, then HMAC-SHA-256 over the labels.
    let fixed = hex::decode(
        "544c41544c424b310202010000000400000000030000000400001000000000\
         00000102030405060708090a0b0c0d0e0fa0a1a2a3a4a5a600",
    )
    .unwrap();
    assert_eq!(fixed.len() + 32, HEADER_BYTES);
    let (encryption, check) = derive_keys_for_test(
        PASSWORD,
        &fixed,
        KdfParams {
            memory_kib: 1024,
            passes: 3,
            lanes: 4,
        },
    );
    assert_eq!(
        hex::encode(encryption),
        "4e1e55b411c98ec9f7acdfeaeec59c7d0e41e595f1d8fbebce4f6418dff9c4c8"
    );
    assert_eq!(
        hex::encode(check),
        "4a88cda43677a853a01aa99ad2b352d1e698c98ea3cb4bcdee47a01efb19de64"
    );
}

#[test]
fn data_recovery_archive_header_names_argon2id_and_its_cost() {
    let rig = rig();
    let report = rig.backup();
    let archive = fs::read(&report.path).unwrap();
    assert_eq!(&archive[..8], b"TLATLBK1");
    assert_eq!((archive[8], archive[9], archive[10]), (2, 2, 1));
    let field = |at: usize| u32::from_be_bytes(archive[at..at + 4].try_into().unwrap());
    assert_eq!(
        (field(12), field(16), field(20), field(24)),
        (TEST_KDF.memory_kib, TEST_KDF.passes, TEST_KDF.lanes, 4096)
    );
    // A header asking for more memory than the budget allows is refused
    // before any derivation.
    let mut greedy = archive.clone();
    greedy[12..16].copy_from_slice(&(MAX_KDF_MEMORY_KIB + 1).to_be_bytes());
    let path = rig.temporary.path().join("greedy");
    fs::write(&path, greedy).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        verify_encrypted_backup(&path, PASSWORD),
        Err(DataRecoveryError::CorruptArchive(_))
    ));
    // The default request uses the RFC 9106 cost.
    assert_eq!(
        EncryptedBackupRequest::new(&rig.destination, PASSWORD).kdf,
        DEFAULT_KDF
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

/// Every regular file under `root` (links not followed) modified since
/// `since` that holds `needle`, except those `skip` names.
fn files_holding(
    root: &Path,
    needle: &[u8],
    since: SystemTime,
    skip: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.is_file()
                || skip(&path)
                || metadata.len() > 64 * 1024 * 1024
                || metadata.modified().is_ok_and(|modified| modified < since)
            {
                continue;
            }
            if fs::read(&path)
                .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
            {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn backup_no_plaintext_staging() {
    let since = SystemTime::now() - std::time::Duration::from_secs(2);
    let rig = rig();
    // A canary that only the live catalogue holds in plaintext.
    let canary = format!("m06-plaintext-canary-{}", Uuid::new_v4());
    rig.store
        .register_source(&SourceDescriptor::new("tesla_owner_api", &canary), 1_000)
        .expect("canary source");
    let live_data = rig.store.database_path().parent().unwrap().to_path_buf();
    let live_root = live_data.clone();
    let live_sqlite = move |path: &Path| {
        path.parent() == Some(live_root.as_path())
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hub.sqlite"))
    };
    // The scan sees plaintext where it is: the live catalogue.
    assert!(
        !files_holding(&live_data, canary.as_bytes(), since, &|_| false).is_empty(),
        "the scan finds the canary in the live catalogue"
    );
    let pack_probe = {
        let bytes = fs::read(the_pack(&rig.store)).expect("pack bytes");
        bytes[..bytes.len().min(32)].to_vec()
    };
    let sqlite_magic = b"SQLite format 3\0".to_vec();
    let temporary_root = std::env::temp_dir();
    let rig_root = rig.temporary.path().to_path_buf();
    let destination = rig.destination.clone();
    let scans = Rc::new(Cell::new(0_u32));
    let writes = Rc::new(Cell::new(0_u32));
    let leaks = Rc::new(RefCell::new(Vec::<String>::new()));
    let (scanned, written, leaked) = (scans.clone(), writes.clone(), leaks.clone());
    let needle = canary.clone();
    set_hook(move |stage, _| {
        // The destination holds only the encrypted partial, at every write.
        if stage == "write" {
            written.set(written.get() + 1);
            for entry in fs::read_dir(&destination).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                if !(name.starts_with(".teslatlas-hub-backup-") && name.ends_with(".partial")) {
                    leaked.borrow_mut().push(format!("unexpected entry {name}"));
                    continue;
                }
                let bytes = fs::read(entry.path()).unwrap();
                for (label, probe) in [
                    ("sqlite header", sqlite_magic.as_slice()),
                    ("pack", pack_probe.as_slice()),
                    ("canary", needle.as_bytes()),
                ] {
                    if bytes.windows(probe.len()).any(|window| window == probe) {
                        leaked.borrow_mut().push(format!("{label} found in {name}"));
                    }
                }
            }
            return HookAction::Continue;
        }
        // While the copy exists (under the gate, and at every record while
        // it streams): no file anywhere under the temporary root, the store
        // and the destination included, holds the canary, except the live
        // catalogue; and no SQLite side file or temporary exists beside it.
        if matches!(stage, "catalogue_copied" | "record" | "before_publish") {
            scanned.set(scanned.get() + 1);
            for path in files_holding(&temporary_root, needle.as_bytes(), since, &live_sqlite) {
                leaked
                    .borrow_mut()
                    .push(format!("canary in {} at {stage}", path.display()));
            }
            let mut pending = vec![rig_root.clone()];
            while let Some(directory) = pending.pop() {
                for entry in fs::read_dir(&directory).into_iter().flatten().flatten() {
                    let path = entry.path();
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        pending.push(path);
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let side_file = ["-journal", "-wal", "-shm"]
                        .iter()
                        .any(|suffix| name.ends_with(suffix))
                        || name.starts_with("etilqs_")
                        || name.ends_with(".sqlite");
                    if side_file && !live_sqlite(&path) {
                        leaked
                            .borrow_mut()
                            .push(format!("SQLite file {} at {stage}", path.display()));
                    }
                }
            }
        }
        HookAction::Continue
    });
    let report = rig.backup();
    clear_hook();
    assert!(writes.get() > 4, "the stream wrote in many steps");
    assert!(
        scans.get() > 4,
        "scratch was inspected while the run held the copy"
    );
    assert!(
        leaks.borrow().is_empty(),
        "plaintext staged: {:?}",
        leaks.borrow()
    );

    let archive = fs::read(&report.path).expect("archive bytes");
    for needle in [b"SQLite format 3\0".as_slice(), canary.as_bytes()] {
        assert!(
            !archive.windows(needle.len()).any(|window| window == needle),
            "the archive holds plaintext"
        );
    }
    assert_eq!(rig.entries().len(), 1);
    // The canary was in the backup, encrypted: the restore brings it back.
    let restored = rig.temporary.path().join("restored");
    restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore");
    assert!(!files_holding(&restored, canary.as_bytes(), since, &|_| false).is_empty());
}

#[test]
fn backup_no_plaintext_staging_catalogue_copy_is_memory_only() {
    let rig = rig();
    let memory = copy_catalogue_into_memory(&rig.store).expect("in-memory copy");
    // No file backs the copy, so no journal, WAL or temporary file can.
    let main_file: String = memory
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name = 'main'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(main_file, "");
    let temp_store: i64 = memory
        .query_row("PRAGMA temp_store", [], |row| row.get(0))
        .unwrap();
    assert_eq!(temp_store, 2, "temporary tables and indices stay in memory");
    let journal: String = memory
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "memory");
    // It is a full, consistent copy of the live catalogue.
    assert_eq!(
        catalogue_identity(&memory, SCHEMA_VERSION).expect("copy identity"),
        rig.store.installation_id().unwrap()
    );
    let live: i64 = rig
        .store
        .open()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM sync_manifests", [], |row| row.get(0))
        .unwrap();
    let copied: i64 = memory
        .query_row("SELECT COUNT(*) FROM sync_manifests", [], |row| row.get(0))
        .unwrap();
    assert_eq!(live, copied);
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
        2,
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

    // A key derivation cost outside the format's range is refused.
    let mut request = rig.request();
    request.kdf.memory_kib = MAX_KDF_MEMORY_KIB + 1;
    assert!(create_encrypted_backup(&rig.store, &request).is_err());
    assert!(rig.entries().is_empty());
    assert_eq!(pin_rows(&rig.store), 0, "no refusal leaves a pin");
}

#[test]
fn data_recovery_corruption_is_detected_and_nothing_is_published() {
    let rig = rig();
    let report = rig.backup();
    let good = fs::read(&report.path).unwrap();
    let restored = rig.temporary.path().join("restored");
    // Each damaged copy keeps the generation's own file name (M06-T02 binds
    // the name to the authenticated manifest), in a folder of its own.
    let mutate = |name: &str, bytes: Vec<u8>| {
        let folder = rig.temporary.path().join(format!("case-{name}"));
        fs::create_dir(&folder).unwrap();
        let path = folder.join(report.path.file_name().unwrap());
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
    let (first, second) = (HEADER_BYTES, HEADER_BYTES + chunk);
    let swapped: Vec<u8> = reordered[second..second + chunk].to_vec();
    let original: Vec<u8> = reordered[first..first + chunk].to_vec();
    reordered[first..first + chunk].copy_from_slice(&swapped);
    reordered[second..second + chunk].copy_from_slice(&original);
    let cases = [
        ("flipped", flipped),
        ("truncated_mid_chunk", good[..good.len() - 7].to_vec()),
        (
            "truncated_at_a_chunk",
            good[..HEADER_BYTES + chunk * 3].to_vec(),
        ),
        ("header_only", good[..HEADER_BYTES].to_vec()),
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
    header_flip[SALT_OFFSET + 2] ^= 0x01; // inside the salt
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

fn compaction_cursor_key() -> CursorKey {
    CursorKey::from_bytes([61; 32])
}

fn compaction_cursor(binding: &ProjectionBinding, sequence: u64) -> OpaqueCursor {
    OpaqueCursor::issue(
        &compaction_cursor_key(),
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence,
        },
    )
    .expect("compaction fixture cursor")
}

fn compaction_car(car_id: i64) -> ProjectionCar {
    ProjectionCar {
        id: car_id,
        name: "Compaction fixture".into(),
        model: "Model 3".into(),
        vin: None,
        source_eid: None,
        source_vid: None,
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        source_model_text: None,
        firmware_version: None,
        efficiency_wh_per_km: None,
        settings: ProjectionCarSettings::default(),
    }
}

/// Publish one collector delta for the pending mutations of `vehicle_id`.
fn publish_collector_delta(store: &HubStore, vehicle_id: Uuid, binding: &ProjectionBinding) {
    let (base_snapshot_id, head_sequence, parent_digest) = store
        .v2_head(vehicle_id)
        .expect("fixture head lookup")
        .expect("fixture V2 head");
    let from_sequence = u64::try_from(head_sequence).expect("fixture sequence");
    let claim = store
        .claim_sync_mutations(vehicle_id, 2_000, 100)
        .expect("claim collector mutation")
        .expect("a collector mutation is pending");
    let to_sequence = from_sequence + u64::try_from(claim.mutations.len()).unwrap();
    let payload = store
        .projection_delta_for_mutations(
            &claim,
            binding.clone(),
            SequenceRange {
                from_exclusive: from_sequence,
                to_inclusive: to_sequence,
            },
            parent_digest,
        )
        .expect("project claimed mutation");
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_delta(&ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: base_snapshot_id,
            ordinal: store
                .next_v2_pack_ordinal(base_snapshot_id)
                .expect("fixture delta ordinal"),
            delta: &payload,
        })
        .expect("write collector delta");
    let delta = LineageDelta {
        from_sequence,
        to_sequence,
        parent_chain_digest: parent_digest,
        chain_digest: canonical_delta_chain_digest(parent_digest, pack.metadata.sha256),
        pack_digest: pack.metadata.sha256,
        pack: pack.metadata,
    };
    store
        .commit_v2_delta_claim(
            &claim,
            &delta,
            &compaction_cursor_key(),
            &compaction_cursor(binding, delta.to_sequence),
        )
        .expect("publish collector delta");
}

/// An imported V2 base with two collector deltas: a suffix the real live
/// compaction can replace.
fn compactable_lineage(store: &HubStore) -> (Uuid, ProjectionBinding) {
    let source = store
        .register_source(
            &SourceDescriptor::new("source_db_import", "m06-compaction"),
            1_000,
        )
        .expect("fixture source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "10").with_tesla_identity(Some(70), None),
            1_001,
        )
        .expect("fixture vehicle");
    let binding = store
        .v2_projection_binding(vehicle.vehicle_id)
        .expect("fixture binding");
    let snapshot = ProjectionSnapshot {
        cars: vec![compaction_car(binding.selected_car_id)],
        drives: Vec::new(),
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
    };
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: binding.clone(),
        sequence: SequenceRange {
            from_exclusive: 1,
            to_inclusive: 1,
        },
        snapshot: &snapshot,
    };
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])
        .expect("fixture base pack");
    let manifest = request
        .signed_manifest_with_states_and_updates(&pack, &[], &[], &compaction_cursor_key())
        .expect("fixture base manifest");
    store
        .finalize_import_snapshot_with_binding(
            &manifest,
            Sha256Digest::of_bytes(b"m06-compaction-base"),
            &[],
            &binding,
        )
        .expect("fixture base catalogue");
    store
        .persist_materialised_car_if_absent(
            vehicle.vehicle_id,
            &compaction_car(binding.selected_car_id),
        )
        .expect("collector car mutation");
    publish_collector_delta(store, vehicle.vehicle_id, &binding);
    store
        .upsert_car_settings(
            vehicle.vehicle_id,
            binding.selected_car_id,
            &ProjectionCarSettings {
                enabled: false,
                ..ProjectionCarSettings::default()
            },
        )
        .expect("collector setting mutation");
    publish_collector_delta(store, vehicle.vehicle_id, &binding);
    (vehicle.vehicle_id, binding)
}

/// The real live-delta compaction, as the collector runs it. Returns the
/// replaced delta packs and the compacted one.
fn compact(
    store: &HubStore,
    vehicle_id: Uuid,
    binding: &ProjectionBinding,
) -> (Vec<PathBuf>, PathBuf) {
    let before = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage before compaction")
        .expect("published lineage");
    let plan = store
        .plan_live_delta_compaction(vehicle_id)
        .expect("compaction plan")
        .expect("two live deltas form a compactable suffix");
    let payload = store
        .projection_delta_for_compaction(&plan, binding.clone())
        .expect("compacted payload");
    let built = ProjectionPackWriter::new(store.packs_dir())
        .write_delta(&ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: plan.base_snapshot_id,
            ordinal: plan.first_ordinal,
            delta: &payload,
        })
        .expect("write compacted pack");
    let compacted = LineageDelta {
        from_sequence: plan.anchor_sequence,
        to_sequence: plan.head_sequence,
        parent_chain_digest: plan.anchor_digest,
        chain_digest: canonical_delta_chain_digest(plan.anchor_digest, built.metadata.sha256),
        pack_digest: built.metadata.sha256,
        pack: built.metadata,
    };
    store
        .commit_live_delta_compaction(
            &plan,
            &compacted,
            &compaction_cursor_key(),
            &compaction_cursor(binding, compacted.to_sequence),
        )
        .expect("compaction commits");
    let replaced = before
        .deltas
        .iter()
        .map(|delta| pack_path(store, delta.pack_digest))
        .collect();
    (replaced, pack_path(store, compacted.pack_digest))
}

/// End the retention of every retired lineage, so GC may remove its packs.
fn expire_retired_lineages(store: &HubStore) {
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE sync_retired_lineages SET retired_at_ms = 0, expires_at_ms = 1",
            [],
        )
        .expect("expire retired lineages");
}

struct CompactionRig {
    rig: Rig,
    vehicle_id: Uuid,
    binding: ProjectionBinding,
}

fn compaction_rig() -> CompactionRig {
    let rig = rig();
    let (vehicle_id, binding) = compactable_lineage(&rig.store);
    CompactionRig {
        rig,
        vehicle_id,
        binding,
    }
}

#[test]
fn data_recovery_gate_covers_only_the_catalogue_copy_and_pins_cover_the_packs() {
    let fixture = compaction_rig();
    let rig = &fixture.rig;
    let packs: Vec<String> = read_directory(&rig.store.packs_dir().join("sha256"))
        .unwrap()
        .into_iter()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    let other = rig.other_handle();
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let lines = log.clone();
    set_hook(move |stage, _| {
        if matches!(stage, "catalogue_copied" | "record" | "before_publish") {
            let gate = match other.try_acquire_publication_gate() {
                Err(StoreError::PublicationGateBusy) => "busy".to_owned(),
                Err(error) => format!("error {error}"),
                Ok(_) => "free".to_owned(),
            };
            let pins = live_backup_pack_pins(&other.open().unwrap(), now_ms()).unwrap();
            lines
                .borrow_mut()
                .push(format!("{stage} gate {gate} pins {}", pins.len()));
        }
        HookAction::Continue
    });
    let report = rig.backup();
    clear_hook();
    let log = log.borrow();
    let pinned = packs.len();
    assert_eq!(log[0], format!("catalogue_copied gate busy pins {pinned}"));
    assert!(log.len() > 3);
    assert!(
        log[1..]
            .iter()
            .all(|line| line.ends_with(&format!("gate free pins {pinned}"))),
        "the gate is released after the copy and the pins held to the end: {log:?}"
    );
    assert_eq!(pin_rows(&rig.store), 0, "the pins go with the run");
    rig.assert_only_good(&[&report]);
}

#[test]
fn data_recovery_concurrent_compaction_and_gc_respect_the_pins() {
    let fixture = compaction_rig();
    let rig = &fixture.rig;
    let good = rig.backup();
    tick();

    // While the archive streams (gate released), the real compaction retires
    // the two live delta packs and the real GC runs after their retention
    // has ended. The pins keep both packs in place for the run.
    let other = rig.other_handle();
    let (vehicle_id, binding) = (fixture.vehicle_id, fixture.binding.clone());
    let outcome = Rc::new(RefCell::new(None::<(Vec<PathBuf>, PathBuf, usize)>));
    let seen = outcome.clone();
    set_hook(move |stage, _| {
        if stage == "record" && seen.borrow().is_none() {
            let (replaced, compacted) = compact(&other, vehicle_id, &binding);
            expire_retired_lineages(&other);
            let report = other.repair().expect("GC runs beside the backup");
            *seen.borrow_mut() = Some((replaced, compacted, report.orphaned_packs_removed));
        }
        HookAction::Continue
    });
    let during = rig.backup();
    clear_hook();
    let (replaced, compacted, removed_during) = outcome.borrow_mut().take().expect("compacted");
    assert_eq!(replaced.len(), 2);
    assert_eq!(removed_during, 0, "GC removed nothing that was pinned");
    assert!(replaced.iter().all(|path| path.is_file()));
    rig.assert_only_good(&[&good, &during]);
    assert_eq!(pin_rows(&rig.store), 0);

    // The generation holds the pre-compaction lineage it copied, whole.
    let restored = rig.temporary.path().join("restored");
    restore_encrypted_backup(&during.path, PASSWORD, &restored).expect("restore");
    for path in &replaced {
        assert!(
            restored
                .join("packs/sha256")
                .join(path.file_name().unwrap())
                .is_file()
        );
    }

    // With the run over and the pins gone, the same GC removes them; the
    // compacted pack, which the live lineage references, stays.
    let report = rig.store.repair().expect("GC after the run");
    assert_eq!(report.orphaned_packs_removed, 2);
    assert!(replaced.iter().all(|path| !path.exists()));
    assert!(compacted.is_file());
}

#[test]
fn data_recovery_failed_run_releases_its_pins_and_a_killed_run_leaves_them_to_their_lease() {
    // A failed run (here, the destination fails mid-stream) releases its pins:
    // GC is free to remove what compaction retired during the run.
    let fixture = compaction_rig();
    let rig = &fixture.rig;
    let good = rig.backup();
    tick();
    let other = rig.other_handle();
    let (vehicle_id, binding) = (fixture.vehicle_id, fixture.binding.clone());
    let replaced = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let seen = replaced.clone();
    set_hook(move |stage, _| match stage {
        "record" if seen.borrow().is_empty() => {
            let (retired, _) = compact(&other, vehicle_id, &binding);
            expire_retired_lineages(&other);
            *seen.borrow_mut() = retired;
            HookAction::Continue
        }
        "write" if !seen.borrow().is_empty() => HookAction::Fail(io::ErrorKind::BrokenPipe),
        _ => HookAction::Continue,
    });
    assert!(create_encrypted_backup(&rig.store, &rig.request()).is_err());
    clear_hook();
    rig.assert_only_good(&[&good]);
    assert_eq!(pin_rows(&rig.store), 0, "a failed run releases its pins");
    let replaced = replaced.borrow().clone();
    assert_eq!(rig.store.repair().unwrap().orphaned_packs_removed, 2);
    assert!(replaced.iter().all(|path| !path.exists()));

    // A killed run leaves its pins; GC honours them until the lease ends.
    let fixture = compaction_rig();
    let rig = &fixture.rig;
    let other = rig.other_handle();
    let (vehicle_id, binding) = (fixture.vehicle_id, fixture.binding.clone());
    let replaced = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let seen = replaced.clone();
    set_hook(move |stage, _| match stage {
        "record" if seen.borrow().is_empty() => {
            let (retired, _) = compact(&other, vehicle_id, &binding);
            expire_retired_lineages(&other);
            *seen.borrow_mut() = retired;
            HookAction::Continue
        }
        "write" if !seen.borrow().is_empty() => HookAction::Crash,
        _ => HookAction::Continue,
    });
    assert!(create_encrypted_backup(&rig.store, &rig.request()).is_err());
    clear_hook();
    let replaced = replaced.borrow().clone();
    assert!(
        pin_rows(&rig.store) > 0,
        "a killed run cannot release its pins"
    );
    assert_eq!(rig.store.repair().unwrap().orphaned_packs_removed, 0);
    assert!(replaced.iter().all(|path| path.is_file()));
    // Its lease ends: the pins protect nothing, and the next run clears them.
    rig.store
        .open()
        .unwrap()
        .execute("UPDATE backup_pack_pins SET expires_at_ms = 1", [])
        .unwrap();
    assert_eq!(rig.store.repair().unwrap().orphaned_packs_removed, 2);
    tick();
    let next = rig.backup();
    assert_eq!(pin_rows(&rig.store), 0);
    verify_encrypted_backup(&next.path, PASSWORD).expect("next run");
}

#[test]
fn data_recovery_a_pack_changed_under_the_run_fails_it() {
    let rig = rig();
    let good = rig.backup();
    tick();
    // A pack removed under the run (bypassing GC and its pins) fails the run
    // instead of producing a generation with a hole.
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
    assert_eq!(pin_rows(&rig.store), 0);
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
