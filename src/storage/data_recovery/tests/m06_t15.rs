// SPDX-License-Identifier: AGPL-3.0-only

// M06-T15: repairs of REV-CX-M06-T10 findings 1 to 4, REV-CX-M06-T11
// findings 1 to 6 and REV2-CX-M06-T08 finding 6. Each test names its
// finding. Tests that drive a seam added by this task (a hook stage, the
// free-space seam or the delayed terminal mode change) fail on 543c512 by
// their assertions, because the seam is absent there and the guarded
// behaviour with it; the notes say which. Synthetic data in temporary
// directories only; no Keychain, no person, no window.

use std::{
    num::NonZeroUsize,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::Value;

use super::*;
use crate::{
    data_recovery::{
        EncryptedBackupReport, EncryptedBackupRequest,
        archive_crypto::KdfParams,
        backup_admission::{VolumeTrust, test_seam},
        create_encrypted_backup,
        encrypted_backup::{HookAction, clear_hook, set_hook, write_test_archive},
        restore_encrypted_backup, space_seam, verify_encrypted_backup,
    },
    hub_pack::{ProjectionPackRequestV2_2, ProjectionPackWriter},
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
const INVALID_PUBLICATION: &str = "invalid_publication";
const ENCRYPTED_STAGING_PREFIX: &str = ".teslatlas-encrypted-restore-";
const SHARED_STAGING_PREFIX: &str = ".teslatlas-data-recovery-";

struct Hub {
    temporary: tempfile::TempDir,
    store: HubStore,
    destination: PathBuf,
}

fn hub(trust: VolumeTrust) -> Hub {
    clear_hook();
    space_seam::clear();
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

fn request(destination: &Path, keep: Option<usize>) -> EncryptedBackupRequest<'_> {
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

    /// A private folder for restore targets.
    fn restores(&self) -> PathBuf {
        let parent = self.temporary.path().join("restores");
        if !parent.exists() {
            fs::create_dir(&parent).expect("restore parent");
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("private");
        }
        parent
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

fn names_with_prefix(directory: &Path, prefix: &str) -> Vec<String> {
    entries(directory)
        .into_iter()
        .filter(|name| name.starts_with(prefix))
        .collect()
}

/// Every regular file under `root`, never following a link.
fn regular_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(listing) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                found.push(entry.path());
            }
        }
    }
    found
}

fn assert_invalid_publication(result: Result<EncryptedBackupReport, DataRecoveryError>) -> String {
    match result {
        Err(DataRecoveryError::InvalidBackup(message)) => {
            assert!(
                message.starts_with(INVALID_PUBLICATION),
                "an invalid publication names itself: {message}"
            );
            message
        }
        Err(other) => panic!("expected an invalid publication, got {other}"),
        Ok(report) => panic!(
            "a replaced publication was reported as a success: {}",
            report.path.display()
        ),
    }
}

fn generations(directory: &Path) -> Vec<PathBuf> {
    entries(directory)
        .into_iter()
        .filter(|name| name.starts_with("teslatlas-hub-backup-") && name.ends_with(".tlbk"))
        .map(|name| directory.join(name))
        .collect()
}

/// The peak resident set of a finished child, from `/usr/bin/time`.
fn peak_resident_bytes(report: &str) -> Option<u64> {
    for line in report.lines() {
        let line = line.trim();
        if cfg!(target_os = "macos") {
            if let Some(value) = line.strip_suffix("maximum resident set size") {
                return value.trim().parse().ok();
            }
        } else if let Some(value) = line.strip_prefix("Maximum resident set size (kbytes):") {
            return value.trim().parse::<u64>().ok().map(|kib| kib * 1024);
        }
    }
    None
}

/// The path of a test of this module, as the harness names it.
fn this_module_test(name: &str) -> String {
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{module}::{name}")
}

// REV-CX-M06-T10 finding 1 ---------------------------------------------------

const SELECTION_CHILD: &str = "TESLATLAS_M06_T15_SELECTION_CHILD";
/// Retained catalogue history: lineage manifest rows the selection skips,
/// each padded (JSON whitespace) to one MiB.
const HISTORY_ROWS: usize = 96;
const HISTORY_ROW_BYTES: usize = 1024 * 1024;
/// Far below the history's 96 MiB, far above one row and the page cache.
const SELECTION_GROWTH_LIMIT_BYTES: u64 = 40 * 1024 * 1024;

/// The child the parent measures: a catalogue with one current schema 2.2
/// snapshot and 96 MiB of retained history; `select` also runs the no-op
/// selection, `baseline` stops before it.
#[test]
fn data_recovery_m06_t15_child_noop_selection() {
    let Ok(mode) = std::env::var(SELECTION_CHILD) else {
        return;
    };
    let (_temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let lineage = publish_lineage_fixture(&store, &CursorKey::from_bytes([9; 32]));
    let mut padded = serde_json::to_vec(&lineage).expect("lineage manifest");
    padded.resize(HISTORY_ROW_BYTES, b' ');
    {
        let connection = Connection::open(store.database_path()).expect("live catalogue");
        for sequence in 0..HISTORY_ROWS {
            connection
                .execute(
                    "INSERT INTO sync_manifests(snapshot_id, vehicle_id, head_sequence, manifest_json)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        Uuid::new_v4().to_string(),
                        lineage.vehicle_id.to_string(),
                        sequence as i64,
                        padded.as_slice()
                    ],
                )
                .expect("history row");
        }
        connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .expect("checkpoint");
    }
    drop(padded);
    let data = store.database_path().parent().expect("data").to_path_buf();
    if mode == "select" {
        let pairs = current_schema_22_noop_pairs(&data).expect("the selection");
        assert_eq!(pairs.len(), 1, "one current schema 2.2 snapshot");
    }
    println!("M06T15_SELECTION_CHILD {mode}");
}

fn selection_child_peak(mode: &str) -> u64 {
    let flag = if cfg!(target_os = "macos") { "-l" } else { "-v" };
    let output = Command::new("/usr/bin/time")
        .arg(flag)
        .arg(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            &this_module_test("data_recovery_m06_t15_child_noop_selection"),
            "--test-threads=1",
            "--nocapture",
        ])
        .env(SELECTION_CHILD, mode)
        .output()
        .expect("child run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(&format!("M06T15_SELECTION_CHILD {mode}")),
        "child {mode} failed:\n{stdout}\n{stderr}"
    );
    peak_resident_bytes(&stderr).expect("peak resident set")
}

/// Finding 1: the schema 2.2 no-op selection that backup and restore run
/// over the catalogue holds no retained history: the old selection
/// collected every manifest row (here 96 MiB) before choosing.
#[test]
fn data_recovery_m06_t15_noop_selection_holds_no_retained_history() {
    let baseline = selection_child_peak("baseline");
    let selecting = selection_child_peak("select");
    assert!(
        selecting < baseline + SELECTION_GROWTH_LIMIT_BYTES,
        "the selection grew the peak from {baseline} to {selecting} bytes over \
         {HISTORY_ROWS} retained rows of {HISTORY_ROW_BYTES} bytes"
    );
}

// REV-CX-M06-T10 finding 4 ---------------------------------------------------

/// Write `count` small packs into the live store and reference each from the
/// catalogue (`sync_packs`), as monthly packs of `snapshot_id`.
fn reference_synthetic_packs(store: &HubStore, snapshot_id: Uuid, count: usize) {
    let directory = store.packs_dir().join("sha256");
    fs::create_dir_all(&directory).expect("pack directory");
    let connection = Connection::open(store.database_path()).expect("live catalogue");
    let transaction = connection.unchecked_transaction().expect("transaction");
    for index in 0..count {
        let bytes = format!("m06-t15 referenced pack {index}").into_bytes();
        let digest = sha256_bytes_hex(&bytes);
        let path = directory.join(format!("{digest}.sqlite.zst"));
        fs::write(&path, &bytes).expect("synthetic pack");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("pack mode");
        transaction
            .execute(
                "INSERT INTO sync_packs(sha256, snapshot_id, ordinal, relative_path,
                     compressed_bytes, uncompressed_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, 100)",
                rusqlite::params![
                    digest,
                    snapshot_id.to_string(),
                    index as i64 + 1,
                    format!("/v1/packs/sha256/{digest}.sqlite.zst"),
                    bytes.len() as i64
                ],
            )
            .expect("pack reference");
    }
    transaction.commit().expect("references");
}

/// Finding 4: a new plaintext directory backup above the old 4,096-member
/// ceiling is created (in the streamed directory format), verified and
/// restored; a damaged pack in it is still refused.
#[test]
fn data_recovery_m06_t15_plaintext_backup_above_the_listed_bound_verifies_and_restores() {
    let (temporary, store) = create_fixture();
    let (manifest, _) = publish_schema_22_fixture(&store);
    reference_synthetic_packs(&store, manifest.snapshot_id, 4_200);
    let backup = temporary.path().join("plaintext");
    let created = create_data_backup(&store, &backup).expect("no member bound");
    assert!(created.member_count > LISTED_FORMAT_MAX_MEMBERS, "{}", created.member_count);
    assert!(backup.join("backup-v5.json").exists(), "the streamed directory format");
    assert!(!backup.join("backup-v4.json").exists());

    let verified = verify_data_backup(&backup).expect("verify above the ceiling");
    assert_eq!(verified.member_count, created.member_count);
    assert_eq!(verified.total_bytes, created.total_bytes);

    let restored_path = temporary.path().join("restored");
    let restored = restore_data_backup(&backup, &restored_path).expect("restore above the ceiling");
    assert_eq!(restored.member_count, created.member_count);
    assert_eq!(
        fs::read_dir(restored_path.join("packs").join("sha256")).unwrap().count(),
        created.member_count - 2,
        "every pack is restored (the members less the catalogue and the no-op)"
    );
    let sample = b"m06-t15 referenced pack 4199".to_vec();
    let name = format!("{}.sqlite.zst", sha256_bytes_hex(&sample));
    assert_eq!(fs::read(restored_path.join("packs").join("sha256").join(&name)).unwrap(), sample);

    let damaged = backup.join("data").join("packs").join("sha256").join(&name);
    fs::write(&damaged, b"m06-t15 referenced pack 41990").unwrap();
    fs::set_permissions(&damaged, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(verify_data_backup(&backup).is_err(), "a damaged pack is refused");
}

// REV-CX-M06-T10 finding 3 ---------------------------------------------------

/// Publish one current schema 2.2 snapshot and its no-op for each of `count`
/// new vehicles, through the shipped publication path.
fn publish_schema_22_vehicles(store: &HubStore, count: usize) {
    let data = store.database_path().parent().expect("fixture data root");
    let cursor_key = crate::legacy_owner_credentials::load_or_create_cursor_key(data)
        .expect("fixture cursor key");
    let snapshot = crate::updates_delivery::updates_snapshot_v2_2(Vec::new());
    for _ in 0..count {
        let mut binding = crate::updates_delivery::pinned_updates_binding();
        binding.vehicle_id = Uuid::new_v4();
        let request = ProjectionPackRequestV2_2 {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding,
            sequence: SequenceRange {
                from_exclusive: 0,
                to_inclusive: 0,
            },
            snapshot: &snapshot,
        };
        let built = ProjectionPackWriter::new(store.packs_dir())
            .write_full_snapshot_2_2(&request)
            .expect("vehicle pack");
        let manifest =
            crate::updates_delivery::sign_updates_schema_22_manifest(&request, &built, &cursor_key)
                .expect("vehicle manifest");
        let noop = crate::updates_delivery::sign_updates_schema_22_noop(
            &request.binding,
            request.snapshot_id,
            0,
            &built.metadata.sha256.to_string(),
            &cursor_key,
        )
        .expect("vehicle no-op");
        crate::updates_delivery::publish_updates_schema_22(store, &manifest, &noop)
            .expect("vehicle publication");
    }
}

/// Finding 3: a bundle the producer accepts with more no-op members than
/// the old listed directory ceiling (4,112 entries) restores.
#[test]
fn data_recovery_m06_t15_more_noops_than_the_old_directory_ceiling_restore() {
    let hub = hub(VolumeTrust::Posix);
    publish_schema_22_vehicles(&hub.store, 4_112);
    let created = hub.backup(None).expect("the producer accepts the no-ops");
    verify_encrypted_backup(&created.path, PASSWORD).expect("verify");
    let restored_path = hub.restores().join("restored");
    restore_encrypted_backup(&created.path, PASSWORD, &restored_path)
        .expect("restore accepts what the producer accepts");
    assert_eq!(
        fs::read_dir(restored_path.join("packs").join("noop")).unwrap().count(),
        4_113,
        "every vehicle's no-op is restored"
    );
}

// REV-CX-M06-T10 finding 2 ---------------------------------------------------

/// A hand-made v5 bundle of `packs` tiny packs whose last record is damaged
/// (its path names another digest), with its payload bytes.
fn tiny_member_bundle(directory: &Path, packs: usize) -> (PathBuf, u64) {
    let mut members: Vec<(String, Vec<u8>)> = (0..packs)
        .map(|index| {
            let bytes = format!("t{index}").into_bytes();
            (format!("{PACK_DIRECTORY}/{}.sqlite.zst", sha256_bytes_hex(&bytes)), bytes)
        })
        .collect();
    members.sort();
    let last = members.last_mut().expect("a pack");
    last.0 = format!("{PACK_DIRECTORY}/{}.sqlite.zst", "f".repeat(64));
    members.insert(0, (CATALOGUE_MEMBER.to_owned(), b"not a catalogue".to_vec()));
    let payload_bytes: u64 = members.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    let manifest = BackupManifest {
        kind: STREAMED_BACKUP_KIND.to_owned(),
        generation: Uuid::new_v4(),
        created_at_ms: 1_700_000_000_000,
        build: BUILD_VERSION.to_owned(),
        hub_schema: SCHEMA_VERSION,
        protocol: expected_protocol(),
        installation_id: Uuid::new_v4(),
        scope: BACKUP_SCOPE.to_owned(),
        excluded_host_state: excluded_host_state_strings(),
        members: Vec::new(),
        member_count: Some(members.len() as u64),
        payload_bytes: Some(payload_bytes),
    };
    let manifest_bytes = canonical_json(&manifest).expect("manifest");
    let records: Vec<(&str, &[u8])> = members
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    let archive = write_test_archive(directory, PASSWORD, TEST_KDF, &manifest_bytes, &records, true)
        .expect("bundle");
    (archive, payload_bytes)
}

/// Finding 2: restore admission counts each member's allocation and
/// metadata: many tiny members, in a bundle damaged near its end, are
/// refused for space before anything is written; and the reserve kept while
/// streaming stops a restore whose space runs out midway.
#[test]
fn data_recovery_m06_t15_restore_admission_counts_member_overhead() {
    let hub = hub(VolumeTrust::Posix);
    let bundles = hub.temporary.path().join("bundles");
    fs::create_dir(&bundles).unwrap();
    fs::set_permissions(&bundles, fs::Permissions::from_mode(0o700)).unwrap();
    let (archive, payload_bytes) = tiny_member_bundle(&bundles, 2_000);
    let parent = hub.restores();

    // Enough for the payload and the old headroom, not for 2,001 files.
    let enough_for_bytes = staging_required_bytes(payload_bytes).unwrap() + 1024 * 1024;
    space_seam::set(move |_| Some(enough_for_bytes));
    let refused = restore_encrypted_backup(&archive, PASSWORD, &parent.join("low"));
    space_seam::clear();
    assert!(
        matches!(refused, Err(DataRecoveryError::InsufficientFreeSpace { .. })),
        "{refused:?}"
    );
    assert!(entries(&parent).is_empty(), "nothing was written: {:?}", entries(&parent));

    // Admitted, then the space goes while members are written.
    let mut calls = 0_u32;
    space_seam::set(move |_| {
        calls += 1;
        Some(if calls < 50 { u64::MAX / 4 } else { 1024 * 1024 })
    });
    let stopped = restore_encrypted_backup(&archive, PASSWORD, &parent.join("midway"));
    space_seam::clear();
    assert!(
        matches!(stopped, Err(DataRecoveryError::InsufficientFreeSpace { .. })),
        "the reserve stops the stream before its damaged end: {stopped:?}"
    );
    assert!(entries(&parent).is_empty(), "the staging went: {:?}", entries(&parent));
}

/// Write `count` small unreferenced packs into the live store.
fn write_live_packs(store: &HubStore, count: usize) {
    let directory = store.packs_dir().join("sha256");
    fs::create_dir_all(&directory).expect("pack directory");
    for index in 0..count {
        let bytes = format!("m06-t15 live pack {index}").into_bytes();
        let path = directory.join(format!("{}.sqlite.zst", sha256_bytes_hex(&bytes)));
        fs::write(&path, &bytes).expect("pack");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("pack mode");
    }
}

fn pin_rows(store: &HubStore) -> i64 {
    store
        .open()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM backup_pack_pins", [], |row| row.get(0))
        .unwrap()
}

/// Finding 2: backup admission counts the archive's record headers and
/// authentication tags, and the pin rows on the live data volume, before
/// the partial file or any pin row is written.
#[test]
fn data_recovery_m06_t15_backup_admission_counts_archive_and_pin_overhead() {
    let hub = hub(VolumeTrust::Posix);
    write_live_packs(&hub.store, 2_000);
    let first = hub.backup(None).expect("a run with space");
    let before = entries(&hub.destination);
    tick();

    // The destination has the payload and the old headroom, not the
    // archive's per-member records and tags.
    let destination = hub.destination.canonicalize().unwrap();
    let enough_for_payload = staging_required_bytes(first.payload_bytes).unwrap() + 64 * 1024;
    space_seam::set(move |path| {
        (path.canonicalize().ok().as_deref() == Some(destination.as_path()))
            .then_some(enough_for_payload)
    });
    let refused = hub.backup(None);
    space_seam::clear();
    assert!(
        matches!(refused, Err(DataRecoveryError::InsufficientFreeSpace { .. })),
        "{refused:?}"
    );
    assert_eq!(entries(&hub.destination), before, "no partial was made");
    tick();

    // The live data volume has no room for the pin rows.
    let live = hub.store.database_path().parent().unwrap().canonicalize().unwrap();
    space_seam::set(move |path| {
        (path.canonicalize().ok().as_deref() == Some(live.as_path())).then_some(1024 * 1024)
    });
    let refused = hub.backup(None);
    space_seam::clear();
    assert!(
        matches!(refused, Err(DataRecoveryError::InsufficientFreeSpace { .. })),
        "{refused:?}"
    );
    assert_eq!(pin_rows(&hub.store), 0, "no pin row was written");
    assert_eq!(entries(&hub.destination), before, "nothing was written");
}

// REV-CX-M06-T11 finding 1 ---------------------------------------------------

/// Finding 1: on a destination that does not enforce ownership, a final
/// entry replaced after the read-back opened it, or changed in place with
/// the same length after its bytes were read, is an invalid publication.
#[test]
fn data_recovery_m06_t15_an_encryption_only_publication_is_bound_after_its_read() {
    let hub = hub(VolumeTrust::EncryptionOnly);
    let first = hub.backup(None).expect("first generation");
    tick();

    let stand_in = hub.temporary.path().join("stand-in");
    let mut replaced = false;
    set_hook(move |stage, path| {
        if stage == "confirm_member" && !replaced {
            replaced = true;
            let length = fs::metadata(path).unwrap().len();
            fs::write(&stand_in, vec![0x42_u8; length as usize]).unwrap();
            fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o600)).unwrap();
            fs::rename(&stand_in, path).unwrap();
        }
        HookAction::Continue
    });
    let result = hub.backup(None);
    clear_hook();
    assert_invalid_publication(result);
    tick();

    // Changed in place, same length, in bytes the read has passed.
    let mut calls = 0_u32;
    set_hook(move |stage, path| {
        if stage == "confirm_member" {
            calls += 1;
            if calls == 3 {
                use std::os::unix::fs::FileExt;
                let file = fs::OpenOptions::new().write(true).open(path).unwrap();
                file.write_at(&[0_u8; 32], 0).unwrap();
            }
        }
        HookAction::Continue
    });
    let result = hub.backup(None);
    clear_hook();
    assert_invalid_publication(result);
    verify_encrypted_backup(&first.path, PASSWORD).expect("the last good generation stays");
    let clean = hub.backup(None).expect("a clean run still succeeds there");
    verify_encrypted_backup(&clean.path, PASSWORD).expect("newest verifies");
}

// REV-CX-M06-T11 finding 3 ---------------------------------------------------

/// Finding 3: with keep = 1, a newest generation damaged while rotation
/// reads the candidate keeps the candidate: the previous good generation
/// still verifies, and the error says nothing older was removed.
#[test]
fn data_recovery_m06_t15_retention_keeps_the_previous_generation_when_the_newest_breaks() {
    let hub = hub(VolumeTrust::Posix);
    let first = hub.backup(None).expect("first generation");
    tick();
    let destination = hub.destination.clone();
    let first_path = first.path.clone();
    set_hook(move |stage, _| {
        if stage == "rotate_remove" {
            let newest = generations(&destination)
                .into_iter()
                .find(|path| *path != first_path)
                .expect("the newest generation");
            let file = fs::OpenOptions::new().write(true).open(&newest).unwrap();
            let length = file.metadata().unwrap().len();
            file.set_len(length / 2).unwrap();
        }
        HookAction::Continue
    });
    let result = hub.backup(Some(1));
    clear_hook();
    let message = assert_invalid_publication(result);
    assert!(message.contains("nothing older was removed"), "{message}");
    verify_encrypted_backup(&first.path, PASSWORD).expect("the previous generation survives");
}

// REV-CX-M06-T11 finding 6 ---------------------------------------------------

/// The schedule's runner, as the service's, with the test password cost.
struct TestRunner {
    store: HubStore,
}

impl BackupRunner for TestRunner {
    fn run(&self, job: &BackupJob) -> Result<Value, OperatorRefusal> {
        let mut request = request(&job.destination, job.keep.map(NonZeroUsize::get));
        request.cancel = Some(Arc::clone(&job.cancel));
        let report = create_encrypted_backup(&self.store, &request)
            .map_err(|error| refusal_from(&error, None))?;
        Ok(serde_json::to_value(&report).unwrap())
    }
}

/// Finding 6: cancellation is observed while the published file is read
/// back: the run is not a success and nothing older is removed; the
/// schedule's `stop` during the read-back ends the run and frees its
/// single-run slot.
#[test]
fn data_recovery_m06_t15_cancellation_reaches_the_publication_read_back() {
    let hub = hub(VolumeTrust::Posix);
    let first = hub.backup(None).expect("first generation");
    tick();

    let flag = Arc::new(AtomicBool::new(false));
    let raised = Arc::clone(&flag);
    set_hook(move |stage, _| {
        if stage == "confirm_member" {
            raised.store(true, Ordering::SeqCst);
        }
        HookAction::Continue
    });
    let mut cancelled_request = request(&hub.destination, Some(1));
    cancelled_request.cancel = Some(flag);
    let cancelled = create_encrypted_backup(&hub.store, &cancelled_request);
    clear_hook();
    assert!(matches!(cancelled, Err(DataRecoveryError::Cancelled)), "{cancelled:?}");
    verify_encrypted_backup(&first.path, PASSWORD).expect("nothing older was removed");
    tick();

    let settings = BackupSettings {
        enabled: false,
        destination: Some(hub.destination.clone()),
        password_file: Some(hub.temporary.path().join("unused-password")),
        interval_ms: 3_600_000,
        keep: NonZeroUsize::new(1),
        retry_ms: 60_000,
        startup_delay_ms: 0,
    };
    let schedule = Arc::new(
        BackupScheduler::start(
            settings,
            Arc::new(TestRunner {
                store: hub.store.clone(),
            }),
            hub.temporary.path().join("schedule-state.json"),
            Box::new(|| false),
        )
        .expect("schedule"),
    );
    let stopping = Arc::clone(&schedule);
    let mut stopped = false;
    set_hook(move |stage, _| {
        if stage == "confirm_member" && !stopped {
            stopped = true;
            stopping.stop();
        }
        HookAction::Continue
    });
    let refused = schedule.run_now(None, None);
    clear_hook();
    assert!(refused.is_err(), "a run stopped in its read-back is not a success");
    assert_eq!(schedule.last().last_success_ms, None);
    assert_eq!(schedule.status()["running"], Value::Bool(false), "the slot is free");
    verify_encrypted_backup(&first.path, PASSWORD).expect("nothing older was removed");
}

// REV-CX-M06-T11 findings 2, 4 and 5 ------------------------------------------

fn only_child_with_prefix(directory: &Path, prefix: &str) -> PathBuf {
    let names = names_with_prefix(directory, prefix);
    assert_eq!(names.len(), 1, "{names:?}");
    directory.join(&names[0])
}

/// A decoy with the staging tree's shape, where misdirected writes would land.
fn make_decoy_tree(root: &Path) {
    for directory in [
        root.to_path_buf(),
        root.join("data"),
        root.join("data").join("packs"),
        root.join("data").join("packs").join("sha256"),
        root.join("data").join("packs").join("noop"),
    ] {
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn assert_restored(path: &Path, expected_members: usize) {
    let store = HubStore::open_immutable_read_only(path).expect("the restored catalogue opens");
    store.catalogue_check().expect("the restored catalogue checks");
    let packs = fs::read_dir(path.join("packs").join("sha256")).unwrap().count();
    assert_eq!(packs + 2, expected_members, "every member is restored");
}

/// Finding 2: the staging container moved and replaced by a decoy during
/// extraction: every member still lands in the held tree, which is what is
/// published; the decoy receives nothing.
#[test]
fn data_recovery_m06_t15_staging_root_swap_during_extraction_cannot_redirect_writes() {
    let hub = hub(VolumeTrust::Posix);
    let good = hub.backup(None).expect("good generation");
    let parent = hub.restores();
    let swap_parent = parent.clone();
    let mut members = 0_u32;
    set_hook(move |stage, _| {
        if stage == "restore_member" {
            members += 1;
            if members == 2 {
                let container = only_child_with_prefix(&swap_parent, ENCRYPTED_STAGING_PREFIX);
                fs::rename(&container, swap_parent.join("moved-staging")).unwrap();
                make_decoy_tree(&container);
            }
        }
        HookAction::Continue
    });
    let restored = parent.join("restored");
    let result = restore_encrypted_backup(&good.path, PASSWORD, &restored);
    clear_hook();
    result.expect("the held tree is restored");
    assert_restored(&restored, good.member_count);
    let decoy = only_child_with_prefix(&parent, ENCRYPTED_STAGING_PREFIX);
    assert!(regular_files(&decoy).is_empty(), "the decoy received nothing");
    assert!(regular_files(&parent.join("moved-staging")).is_empty(), "no plaintext stays behind");
}

/// Finding 2: the restore's folder moved away (an ancestor swap) during
/// extraction: nothing is written into the decoy that took its path, the
/// restore is refused before publication, and no plaintext stays in either.
#[test]
fn data_recovery_m06_t15_ancestor_swap_during_extraction_publishes_nothing() {
    let hub = hub(VolumeTrust::Posix);
    let good = hub.backup(None).expect("good generation");
    let parent = hub.restores();
    let moved = hub.temporary.path().join("restores-moved");
    let (swap_parent, swap_moved) = (parent.clone(), moved.clone());
    let mut members = 0_u32;
    set_hook(move |stage, _| {
        if stage == "restore_member" {
            members += 1;
            if members == 2 {
                let container = only_child_with_prefix(&swap_parent, ENCRYPTED_STAGING_PREFIX);
                let name = container.file_name().unwrap().to_owned();
                fs::rename(&swap_parent, &swap_moved).unwrap();
                fs::create_dir(&swap_parent).unwrap();
                fs::set_permissions(&swap_parent, fs::Permissions::from_mode(0o700)).unwrap();
                make_decoy_tree(&swap_parent.join(name));
            }
        }
        HookAction::Continue
    });
    let result = restore_encrypted_backup(&good.path, PASSWORD, &parent.join("restored"));
    clear_hook();
    let error = result.expect_err("a moved folder is not published into");
    assert!(error.to_string().contains("moved"), "{error}");
    assert!(regular_files(&parent).is_empty(), "the decoy received nothing");
    assert!(regular_files(&moved).is_empty(), "no plaintext stays behind");
}

/// Finding 2: the container moved and replaced just before publication:
/// the held tree is published, never the decoy.
#[test]
fn data_recovery_m06_t15_staging_root_swap_before_publication_publishes_the_held_tree() {
    let hub = hub(VolumeTrust::Posix);
    let good = hub.backup(None).expect("good generation");
    let parent = hub.restores();
    let swap_parent = parent.clone();
    set_hook(move |stage, _| {
        if stage == "restore_before_publish" {
            let container = only_child_with_prefix(&swap_parent, ENCRYPTED_STAGING_PREFIX);
            fs::rename(&container, swap_parent.join("moved-staging")).unwrap();
            make_decoy_tree(&container);
            fs::write(container.join("data").join("decoy-marker"), b"decoy").unwrap();
        }
        HookAction::Continue
    });
    let restored = parent.join("restored");
    let result = restore_encrypted_backup(&good.path, PASSWORD, &restored);
    clear_hook();
    result.expect("the held tree is published");
    assert_restored(&restored, good.member_count);
    assert!(!restored.join("decoy-marker").exists(), "the decoy was not published");
}

/// An abandoned staging directory named `name`, holding plaintext.
fn leftover(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    make_decoy_tree(&path);
    fs::write(path.join("data").join("hub.sqlite"), b"abandoned plaintext").unwrap();
    path
}

/// Finding 2: the leftover swapped for another directory between the
/// sweep's inspection and its claim: the other directory is put back
/// untouched, and the leftover's plaintext is removed through its held
/// descriptor wherever it went.
#[test]
fn data_recovery_m06_t15_sweep_swap_between_inspection_and_removal_keeps_the_stranger() {
    let hub = hub(VolumeTrust::Posix);
    let good = hub.backup(None).expect("good generation");
    let parent = hub.restores();
    let name = format!("{ENCRYPTED_STAGING_PREFIX}{}.staging", Uuid::new_v4());
    let abandoned = leftover(&parent, &name);
    let elsewhere = hub.temporary.path().join("elsewhere");
    let (swap_from, swap_to) = (abandoned.clone(), elsewhere.clone());
    set_hook(move |stage, _| {
        if stage == "restore_sweep_claim" {
            fs::rename(&swap_from, &swap_to).unwrap();
            fs::create_dir(&swap_from).unwrap();
            fs::set_permissions(&swap_from, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(swap_from.join("stranger"), b"not the leftover").unwrap();
        }
        HookAction::Continue
    });
    let restored = parent.join("restored");
    let result = restore_encrypted_backup(&good.path, PASSWORD, &restored);
    clear_hook();
    result.expect("the restore proceeds");
    assert_eq!(fs::read(abandoned.join("stranger")).unwrap(), b"not the leftover");
    assert!(regular_files(&elsewhere).is_empty(), "the leftover's plaintext is gone");
    assert_restored(&restored, good.member_count);
}

/// Findings 4 and 5: a staging directory left by an encrypted restore of an
/// earlier build (the shared name) is removed; active restores, an encrypted
/// one and a directory-format one, both holding their lock, stay.
#[test]
fn data_recovery_m06_t15_earlier_leftovers_go_and_active_restores_stay() {
    let hub = hub(VolumeTrust::Posix);
    let good = hub.backup(None).expect("good generation");
    let parent = hub.restores();
    let legacy = leftover(&parent, &format!("{SHARED_STAGING_PREFIX}{}.staging", Uuid::new_v4()));
    let active_directory_restore = StagingDirectory::create(&parent).expect("active restore");
    fs::write(active_directory_restore.path.join("in-progress"), b"active").unwrap();
    let active_encrypted = leftover(
        &parent,
        &format!("{ENCRYPTED_STAGING_PREFIX}{}.staging", Uuid::new_v4()),
    );
    let held = fs::File::open(&active_encrypted).unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();

    restore_encrypted_backup(&good.path, PASSWORD, &parent.join("restored")).expect("restore");
    assert!(!legacy.exists(), "the earlier build's leftover is removed");
    assert!(active_directory_restore.path.join("in-progress").exists(), "active restore kept");
    assert!(active_encrypted.join("data").join("hub.sqlite").exists(), "active restore kept");
    drop(held);
}

/// Finding 4: a sweep that cannot list, inspect or remove stops the restore
/// before it extracts anything; the next clean start recovers.
#[test]
fn data_recovery_m06_t15_failed_cleanup_is_reported_and_then_recovered() {
    let hub = hub(VolumeTrust::Posix);
    let good = hub.backup(None).expect("good generation");
    let parent = hub.restores();
    leftover(&parent, &format!("{ENCRYPTED_STAGING_PREFIX}{}.staging", Uuid::new_v4()));
    for (attempt, stage) in ["restore_sweep_list", "restore_sweep_inspect", "restore_sweep_remove"]
        .into_iter()
        .enumerate()
    {
        set_hook(move |seen, _| {
            if seen == stage {
                HookAction::Fail(io::ErrorKind::PermissionDenied)
            } else {
                HookAction::Continue
            }
        });
        let target = parent.join(format!("refused-{attempt}"));
        let refused = restore_encrypted_backup(&good.path, PASSWORD, &target);
        clear_hook();
        assert!(
            matches!(refused, Err(DataRecoveryError::Io { .. })),
            "{stage}: a failed clean-up is reported: {refused:?}"
        );
        assert!(!target.exists(), "{stage}: nothing was restored");
        assert!(
            names_with_prefix(&parent, ENCRYPTED_STAGING_PREFIX).len() <= 1,
            "{stage}: no new staging was made"
        );
        assert!(
            !regular_files(&parent).is_empty(),
            "{stage}: the abandoned plaintext is still there, and reported"
        );
    }
    let restored = parent.join("restored");
    restore_encrypted_backup(&good.path, PASSWORD, &restored).expect("the next start recovers");
    assert_eq!(entries(&parent), vec!["restored".to_owned()], "nothing is left over");
}

// REV2-CX-M06-T08 finding 6 --------------------------------------------------

/// Runs the child, waits past the delayed mode change, then shows the
/// terminal settings and lets the next reader read one line.
const RACE_SHELL: &str = "\"$@\"; status=$?; sleep 3; printf '\\nCHILD-STATUS %s\\n' \"$status\"; \
    /bin/stty -a; printf 'NEXT-READ\\n'; IFS= read -r line; printf 'NEXT-LINE[%s]\\n' \"$line\"";

/// The child: its prompt's mode change is slow, and SIGTERM arrives while it
/// is pending. Outside the harness it does nothing.
#[test]
fn data_recovery_m06_t15_pty_child_racing_activation() {
    let Ok(_scenario) = std::env::var(super::m06_t14::PTY_SCENARIO) else {
        return;
    };
    crate::data_recovery::recovery_password::test_seam::race_activation_with_termination(
        std::time::Duration::from_millis(1_000),
    );
    super::m06_t14::pty_child_report(crate::data_recovery::read_recovery_password(false));
}

/// Carried finding 6: a termination signal while the hidden-typing mode is
/// being switched on waits for that change and then restores the terminal;
/// the change can never land after the restoration.
#[cfg(target_os = "macos")]
#[test]
fn data_recovery_m06_t15_pty_termination_during_activation_restores_the_terminal() {
    let mut pty = super::m06_t14::Pty::start_with_shell(
        "m06_t15::data_recovery_m06_t15_pty_child_racing_activation",
        "single",
        RACE_SHELL,
    );
    let Some(status) = pty.line_after("CHILD-STATUS ") else {
        pty.fail("the child did not end");
    };
    assert_eq!(status, "143", "ended by SIGTERM");
    let shown = super::m06_t14::finish_with_next_line(pty);
    super::m06_t14::assert_terminal_restored(&shown);
}
