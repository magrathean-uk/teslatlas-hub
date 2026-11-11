// SPDX-License-Identifier: AGPL-3.0-only

// M06-T07: catalogues above the in-memory cap go through an encrypted
// temporary copy, and the pack pin table belongs to the schema. Synthetic data
// only, in temporary stores; no network and no collector run. The canaries are
// random strings that only the live catalogue holds in plaintext.

use std::{
    cell::{Cell, RefCell},
    io::Read,
    rc::Rc,
    time::{Duration, SystemTime},
};

use rusqlite::params;

use super::*;
use crate::data_recovery::{
    EncryptedBackupReport, EncryptedBackupRequest,
    archive_crypto::KdfParams,
    backup_admission::test_seam,
    create_encrypted_backup,
    encrypted_backup::{
        HUB_PEAK_MEMORY_BYTES, HookAction, MAX_CATALOGUE_IMAGE_BYTES, SCRATCH_CACHE_KIB,
        SCRATCH_THRESHOLD_BYTES, SERVICE_RESERVE_BYTES, STREAM_BUFFER_BYTES, archive_plaintext,
        clear_hook, force_scratch_above, set_hook,
    },
    encrypted_scratch::{EncryptedScratch, test_support},
    restore_encrypted_backup, verify_encrypted_backup,
};

const PASSWORD: &str = "correct horse battery staple";
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";
/// The large run's catalogue: comfortably above the in-memory cap.
const LARGE_CATALOGUE_TARGET_BYTES: u64 = MAX_CATALOGUE_IMAGE_BYTES + 48 * 1024 * 1024;
/// The large run's memory limit: below a single copy of its catalogue, so a
/// run that held the catalogue in memory (the in-memory path holds it twice)
/// cannot pass, while the Hub's 1 GiB peak holds with room to spare.
const LARGE_RUN_MEMORY_LIMIT_BYTES: u64 = MAX_CATALOGUE_IMAGE_BYTES;
const CHILD_ENV: &str = "TESLATLAS_HUB_M06_T07_LARGE_RUN_REPORT";

struct Hub {
    temporary: tempfile::TempDir,
    store: HubStore,
    destination: PathBuf,
}

fn hub() -> Hub {
    clear_hook();
    test_seam::force(None);
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

impl Hub {
    fn request(&self) -> EncryptedBackupRequest<'_> {
        let mut request = EncryptedBackupRequest::new(&self.destination, PASSWORD);
        request.kdf = TEST_KDF;
        request.chunk_bytes = 4096;
        request
    }

    fn data(&self) -> PathBuf {
        self.store.database_path().parent().unwrap().to_path_buf()
    }

    /// The live catalogue, a canary and the live WAL in one file image.
    fn checkpoint(&self) {
        self.store
            .open()
            .unwrap()
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .expect("checkpoint");
    }
}

fn pin_rows(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT COUNT(*) FROM backup_pack_pins", [], |row| row.get(0))
        .expect("pin rows")
}

fn open_catalogue(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("catalogue")
}

/// Which of `needles` the file at `path` holds, read in bounded pieces (the
/// large run must not read a whole catalogue into memory).
fn file_holds(path: &Path, needles: &[&[u8]]) -> Vec<usize> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let overlap = needles.iter().map(|needle| needle.len()).max().unwrap_or(1) - 1;
    let mut window = vec![0_u8; overlap + 1024 * 1024];
    let mut carried = 0;
    let mut found = vec![false; needles.len()];
    loop {
        let read = match file.read(&mut window[carried..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let filled = carried + read;
        for (index, needle) in needles.iter().enumerate() {
            if !found[index] && window[..filled].windows(needle.len()).any(|bytes| bytes == *needle)
            {
                found[index] = true;
            }
        }
        carried = overlap.min(filled);
        window.copy_within(filled - carried..filled, 0);
    }
    found
        .iter()
        .enumerate()
        .filter_map(|(index, hit)| hit.then_some(index))
        .collect()
}

/// Regular files under `root` changed since `since` that hold `needle`,
/// except those `skip` names. Directories on another device (a mounted
/// volume's decrypted view) are not on this disk and are not walked.
fn files_holding(
    root: &Path,
    needle: &[u8],
    since: SystemTime,
    skip: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let device = fs::metadata(root).map(|metadata| metadata.dev()).ok();
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
            if Some(metadata.dev()) != device {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.is_file()
                || skip(&path)
                || metadata.modified().is_ok_and(|modified| modified < since)
            {
                continue;
            }
            if !file_holds(&path, &[needle]).is_empty() {
                found.push(path);
            }
        }
    }
    found
}

/// The live catalogue and its side files, which hold the canary by design.
fn live_catalogue_files(data: PathBuf) -> impl Fn(&Path) -> bool {
    move |path: &Path| {
        path.parent() == Some(data.as_path())
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hub.sqlite"))
    }
}

/// A volume is attached at `mount` when it is a different device from its
/// parent folder.
fn attached(mount: &Path) -> bool {
    match (fs::symlink_metadata(mount), mount.parent().map(fs::metadata)) {
        (Ok(volume), Some(Ok(parent))) => volume.dev() != parent.dev(),
        _ => false,
    }
}

#[test]
fn backup_large_catalogue_scratch_phase_fits_the_hub_peak() {
    // The scratch path holds three page caches (copy, seal, rebuilt file),
    // the no-op bodies and the stream buffers, whatever the catalogue's size.
    let scratch_phase = 3 * SCRATCH_CACHE_KIB * 1024
        + MAX_BACKUP_MEMBERS as u64 * MAX_SCHEMA_22_NOOP_BYTES
        + 2 * STREAM_BUFFER_BYTES as u64;
    assert!(SERVICE_RESERVE_BYTES + scratch_phase + 16 * 1024 * 1024 <= HUB_PEAK_MEMORY_BYTES);
    // The volume is prepared below the cap, so growth before the gate still
    // fits the in-memory path's refusal margin.
    assert!(SCRATCH_THRESHOLD_BYTES < MAX_CATALOGUE_IMAGE_BYTES);
    assert!(LARGE_CATALOGUE_TARGET_BYTES > MAX_CATALOGUE_IMAGE_BYTES);
    assert!(LARGE_RUN_MEMORY_LIMIT_BYTES < LARGE_CATALOGUE_TARGET_BYTES);
}

#[test]
fn backup_large_catalogue_pin_table_comes_from_the_schema_migration() {
    let (temporary, store) = create_fixture();
    let data = store.database_path().parent().unwrap().to_path_buf();
    // A fresh store has the table before any backup has run.
    assert!(catalogue_has_table(&store.open().unwrap(), "backup_pack_pins"));
    assert_eq!(pin_rows(&store.open().unwrap()), 0);
    // A store written before M06-T07, whose backups never ran, gains it at the
    // next open.
    store
        .open()
        .unwrap()
        .execute_batch("DROP TABLE backup_pack_pins;")
        .unwrap();
    drop(store);
    let reopened = HubStore::initialize(&data).expect("reopen migrates");
    let connection = reopened.open().unwrap();
    assert!(catalogue_has_table(&connection, "backup_pack_pins"));
    // A table an M06-T01 backup created on first use keeps its rows: a live
    // run's pins survive an open.
    connection
        .execute(
            "INSERT INTO backup_pack_pins(pin_id, sha256, expires_at_ms) VALUES (?1, ?2, ?3)",
            params![Uuid::new_v4().to_string(), "a".repeat(64), i64::MAX],
        )
        .unwrap();
    drop(connection);
    let again = HubStore::initialize(&data).expect("reopen");
    assert_eq!(pin_rows(&again.open().unwrap()), 1);
    drop(temporary);
}

/// A sealed copy and a restored catalogue keep the pin table and carry no pin
/// rows, and no bytes of rows deleted from the live catalogue, on the
/// in-memory path and on the encrypted scratch path.
#[test]
fn backup_large_catalogue_sealed_copies_carry_no_pins_or_deleted_bytes() {
    let paths: &[Option<u64>] = if cfg!(target_os = "macos") {
        &[None, Some(0)]
    } else {
        &[None]
    };
    for &scratch in paths {
        let hub = hub();
        let deleted = format!("m06-t07-deleted-canary-{}", Uuid::new_v4());
        let foreign_pin = Uuid::new_v4().to_string();
        {
            let connection = hub.store.open().unwrap();
            connection
                .execute_batch(
                    "PRAGMA secure_delete = OFF;
                     CREATE TABLE m06_t07_scrap(body BLOB NOT NULL);",
                )
                .unwrap();
            for _ in 0..16 {
                connection
                    .execute(
                        "INSERT INTO m06_t07_scrap(body) VALUES (CAST(?1 AS BLOB) || zeroblob(2000))",
                        [&deleted],
                    )
                    .unwrap();
            }
            connection.execute("DELETE FROM m06_t07_scrap", []).unwrap();
            // Another run's live pin, copied with the catalogue.
            connection
                .execute(
                    "INSERT INTO backup_pack_pins(pin_id, sha256, expires_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![foreign_pin, "b".repeat(64), i64::MAX],
                )
                .unwrap();
        }
        hub.checkpoint();
        assert!(
            !file_holds(hub.store.database_path(), &[deleted.as_bytes()]).is_empty(),
            "precondition: the live catalogue's free pages hold the deleted rows"
        );

        force_scratch_above(scratch);
        let used_scratch = Rc::new(Cell::new(false));
        let seen = used_scratch.clone();
        set_hook(move |stage, _| {
            if stage == "scratch_copied" {
                seen.set(true);
            }
            HookAction::Continue
        });
        let report = create_encrypted_backup(&hub.store, &hub.request()).expect("backup");
        clear_hook();
        assert_eq!(used_scratch.get(), scratch.is_some(), "the path under test ran");

        let plaintext = archive_plaintext(&report.path, PASSWORD).expect("archive plaintext");
        assert!(
            !plaintext
                .windows(deleted.len())
                .any(|window| window == deleted.as_bytes()),
            "deleted bytes reached the archive (scratch: {scratch:?})"
        );
        assert!(
            !plaintext
                .windows(foreign_pin.len())
                .any(|window| window == foreign_pin.as_bytes()),
            "a pin row reached the archive (scratch: {scratch:?})"
        );
        let restored = hub.temporary.path().join("restored");
        restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore");
        let catalogue = open_catalogue(&restored.join("hub.sqlite"));
        assert!(catalogue_has_table(&catalogue, "backup_pack_pins"));
        assert_eq!(pin_rows(&catalogue), 0, "the restored catalogue has no pins");
        assert!(file_holds(&restored.join("hub.sqlite"), &[deleted.as_bytes()]).is_empty());
        // The live store keeps the other run's pin; this run released its own.
        let live = hub.store.open().unwrap();
        assert_eq!(pin_rows(&live), 1);
        let foreign: i64 = live
            .query_row(
                "SELECT COUNT(*) FROM backup_pack_pins WHERE pin_id = ?1",
                [&foreign_pin],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(foreign, 1);
    }
}

/// The scratch path stages no plaintext: while the run holds the copy, no file
/// on disk under the temporary root (the store, the destination and the
/// scratch image included) holds the canary, the scratch image holds no
/// SQLite page in the clear, and the decrypted view is a separate volume.
#[cfg(target_os = "macos")]
#[test]
fn backup_no_plaintext_staging_through_the_encrypted_scratch_volume() {
    let since = SystemTime::now() - Duration::from_secs(2);
    let hub = hub();
    let canary = format!("m06-t07-scratch-canary-{}", Uuid::new_v4());
    hub.store
        .register_source(&SourceDescriptor::new("tesla_owner_api", &canary), 1_000)
        .expect("canary source");
    hub.checkpoint();
    let skip = live_catalogue_files(hub.data());
    assert!(
        !files_holding(&hub.data(), canary.as_bytes(), since, &|_| false).is_empty(),
        "the scan finds the canary in the live catalogue"
    );
    force_scratch_above(Some(0));
    let temporary_root = std::env::temp_dir();
    let destination = hub.destination.clone();
    let image = Rc::new(RefCell::new(None::<PathBuf>));
    let scans = Rc::new(Cell::new(0_u32));
    let leaks = Rc::new(RefCell::new(Vec::<String>::new()));
    let (seen_image, scanned, leaked) = (image.clone(), scans.clone(), leaks.clone());
    let needle = canary.clone();
    set_hook(move |stage, path| {
        if stage == "scratch_copied" {
            let root = path.parent().unwrap();
            let mount = test_support::mount_of(root).expect("mount record");
            if !attached(&mount) {
                leaked.borrow_mut().push("the scratch is not a mounted volume".into());
            }
            // The copy really is on the volume: its decrypted view holds it.
            if file_holds(&mount.join("catalogue.sqlite"), &[needle.as_bytes()]).is_empty() {
                leaked.borrow_mut().push("the scratch copy is not on the volume".into());
            }
            *seen_image.borrow_mut() = Some(path.to_path_buf());
        }
        if stage == "write" {
            for entry in fs::read_dir(&destination).unwrap().flatten() {
                if !file_holds(&entry.path(), &[SQLITE_MAGIC, needle.as_bytes()]).is_empty() {
                    leaked
                        .borrow_mut()
                        .push(format!("plaintext in {}", entry.path().display()));
                }
            }
            return HookAction::Continue;
        }
        if matches!(
            stage,
            "scratch_copied" | "catalogue_copied" | "catalogue_sealed" | "record" | "before_publish"
        ) {
            scanned.set(scanned.get() + 1);
            for path in files_holding(&temporary_root, needle.as_bytes(), since, &skip) {
                leaked
                    .borrow_mut()
                    .push(format!("canary in {} at {stage}", path.display()));
            }
            if let Some(image) = seen_image.borrow().as_ref() {
                if !file_holds(image, &[SQLITE_MAGIC]).is_empty() {
                    leaked
                        .borrow_mut()
                        .push(format!("a clear SQLite page in the image at {stage}"));
                }
            }
        }
        HookAction::Continue
    });
    let report = create_encrypted_backup(&hub.store, &hub.request()).expect("backup");
    clear_hook();
    let image = image.borrow().clone().expect("the run used the scratch volume");
    assert!(scans.get() > 4, "scratch was inspected while the run held the copy");
    assert!(leaks.borrow().is_empty(), "plaintext staged: {:?}", leaks.borrow());
    // The volume is detached and its image removed once the catalogue streamed.
    assert!(!image.parent().unwrap().exists(), "the scratch folder is removed");
    let restored = hub.temporary.path().join("restored");
    restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore");
    assert!(!file_holds(&restored.join("hub.sqlite"), &[canary.as_bytes()]).is_empty());
}

/// A killed run's scratch volume (attached, its lock released) is detached
/// and removed by the next run; a live run's volume is left alone.
#[cfg(target_os = "macos")]
#[test]
fn backup_large_catalogue_stale_scratch_of_a_killed_run_is_removed() {
    let live = EncryptedScratch::create(64 * 1024 * 1024).expect("live scratch");
    let killed = EncryptedScratch::create(64 * 1024 * 1024).expect("killed scratch");
    let killed_mount = killed.mount().to_path_buf();
    assert!(attached(&killed_mount), "the volume is attached");
    let killed_root = test_support::abandon(killed);
    // A file written there stays encrypted on disk.
    fs::write(live.path("probe"), b"m06-t07 probe").unwrap();
    assert!(file_holds(&live.image(), &[b"m06-t07 probe".as_slice()]).is_empty());

    let next = EncryptedScratch::create(64 * 1024 * 1024).expect("next scratch");
    assert!(!killed_root.exists(), "the killed run's scratch is removed");
    assert!(!attached(&killed_mount), "the killed run's volume is detached");
    assert!(live.root().exists() && attached(live.mount()), "a live scratch stays");
    let (live_root, live_mount) = (live.root().to_path_buf(), live.mount().to_path_buf());
    drop(next);
    drop(live);
    assert!(!live_root.exists() && !attached(&live_mount));
}

/// Back up and restore a catalogue above the in-memory cap in a process of its
/// own, under a memory limit below one copy of that catalogue, scanning the
/// disk while the run holds the copy.
#[cfg(target_os = "macos")]
#[test]
fn backup_large_catalogue_backs_up_and_restores_above_the_cap_under_a_memory_limit() {
    let work = tempfile::tempdir().expect("work folder");
    let report = work.path().join("report.txt");
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, rest)| rest);
    let output = std::process::Command::new("/usr/bin/time")
        .arg("-l")
        .arg(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            &format!("{module}::backup_large_catalogue_child_run"),
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_ENV, &report)
        .output()
        .expect("child run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "the large run failed:\n{stdout}\n{stderr}"
    );
    let peak: u64 = stderr
        .lines()
        .find(|line| line.contains("maximum resident set size"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .expect("peak resident set size");
    let fields: BTreeMap<String, String> = fs::read_to_string(&report)
        .expect("child report")
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let number = |key: &str| -> u64 { fields[key].parse().expect(key) };
    assert!(
        number("catalogue_bytes") > MAX_CATALOGUE_IMAGE_BYTES,
        "the catalogue is above the in-memory cap"
    );
    assert!(
        peak < LARGE_RUN_MEMORY_LIMIT_BYTES,
        "peak {peak} bytes is above the {LARGE_RUN_MEMORY_LIMIT_BYTES} byte limit"
    );
    assert!(number("scans") >= 6, "scratch was scanned while the run held the copy");
    assert_eq!(fields["leaks"], "", "plaintext staged");
    assert_eq!(number("restored_rows"), number("bulk_rows"));
    assert_eq!(number("restored_canary"), 1);
}

/// The large run, started by the test above in its own process.
#[test]
#[ignore = "run in its own process by backup_large_catalogue_backs_up_and_restores_above_the_cap_under_a_memory_limit"]
fn backup_large_catalogue_child_run() {
    let Some(report_path) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let since = SystemTime::now() - Duration::from_secs(2);
    let hub = hub();
    let canary = format!("m06-t07-large-canary-{}", Uuid::new_v4());
    hub.store
        .register_source(&SourceDescriptor::new("tesla_owner_api", &canary), 1_000)
        .expect("canary source");
    // Bulk rows of incompressible bytes, the canary spread through them, in
    // batches so the WAL stays small.
    let rows_per_batch = 256_i64;
    let row_bytes = 64 * 1024_i64;
    let bulk_rows = i64::try_from(LARGE_CATALOGUE_TARGET_BYTES).unwrap() / row_bytes;
    {
        let connection = hub.store.open().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE m06_t07_bulk(id INTEGER PRIMARY KEY, tag TEXT NOT NULL,
                   body BLOB NOT NULL);
                 CREATE INDEX m06_t07_bulk_tag ON m06_t07_bulk(tag);",
            )
            .unwrap();
        let mut written = 0;
        while written < bulk_rows {
            let batch = rows_per_batch.min(bulk_rows - written);
            connection
                .execute(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
                     INSERT INTO m06_t07_bulk(tag, body)
                     SELECT hex(randomblob(16)),
                            CASE WHEN i % 32 = 0 THEN CAST(?2 AS BLOB) || randomblob(?3 - 128)
                                 ELSE randomblob(?3) END
                       FROM n",
                    params![batch, canary, row_bytes],
                )
                .unwrap();
            written += batch;
        }
    }
    hub.checkpoint();
    let catalogue_bytes: i64 = hub
        .store
        .open()
        .unwrap()
        .query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let skip = live_catalogue_files(hub.data());
    let temporary_root = std::env::temp_dir();
    let destination = hub.destination.clone();
    let image = Rc::new(RefCell::new(None::<PathBuf>));
    let scans = Rc::new(Cell::new(0_u64));
    let writes = Rc::new(Cell::new(0_u64));
    let leaks = Rc::new(RefCell::new(Vec::<String>::new()));
    let (seen_image, scanned, written, leaked) =
        (image.clone(), scans.clone(), writes.clone(), leaks.clone());
    let needle = canary.clone();
    set_hook(move |stage, path| {
        if stage == "scratch_copied" {
            let root = path.parent().unwrap();
            match test_support::mount_of(root) {
                Some(mount) if attached(&mount) => {
                    if file_holds(&mount.join("catalogue.sqlite"), &[needle.as_bytes()]).is_empty()
                    {
                        leaked.borrow_mut().push("the copy is not on the volume".into());
                    }
                }
                _ => leaked.borrow_mut().push("the scratch is not a mounted volume".into()),
            }
            *seen_image.borrow_mut() = Some(path.to_path_buf());
        }
        let periodic = stage == "write" && {
            written.set(written.get() + 1);
            written.get() % 128 == 0
        };
        if periodic
            || matches!(
                stage,
                "scratch_copied"
                    | "catalogue_copied"
                    | "catalogue_sealed"
                    | "record"
                    | "before_publish"
            )
        {
            scanned.set(scanned.get() + 1);
            for path in files_holding(&temporary_root, needle.as_bytes(), since, &skip) {
                leaked
                    .borrow_mut()
                    .push(format!("canary in {} at {stage}", path.display()));
            }
            let image = seen_image.borrow().clone();
            for file in image.into_iter().chain(
                fs::read_dir(&destination)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.path()),
            ) {
                if !file_holds(&file, &[SQLITE_MAGIC]).is_empty() {
                    leaked
                        .borrow_mut()
                        .push(format!("a clear SQLite page in {} at {stage}", file.display()));
                }
            }
        }
        HookAction::Continue
    });
    let mut request = EncryptedBackupRequest::new(&hub.destination, PASSWORD);
    request.kdf = TEST_KDF;
    let report: EncryptedBackupReport =
        create_encrypted_backup(&hub.store, &request).expect("large backup");
    clear_hook();
    let used = image.borrow().clone();
    if used.is_none() {
        leaks.borrow_mut().push("the run did not use the scratch volume".into());
    }
    if used.is_some_and(|image| image.parent().unwrap().exists()) {
        leaks.borrow_mut().push("the scratch folder was left behind".into());
    }
    if pin_rows(&hub.store.open().unwrap()) != 0 {
        leaks.borrow_mut().push("the run left pins".into());
    }

    verify_encrypted_backup(&report.path, PASSWORD).expect("verify");
    let restored = hub.temporary.path().join("restored");
    restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("large restore");
    let catalogue = open_catalogue(&restored.join("hub.sqlite"));
    let restored_rows: i64 = catalogue
        .query_row("SELECT COUNT(*) FROM m06_t07_bulk", [], |row| row.get(0))
        .unwrap();
    if pin_rows(&catalogue) != 0 {
        leaks.borrow_mut().push("the restored catalogue has pins".into());
    }
    let restored_canary = file_holds(&restored.join("hub.sqlite"), &[canary.as_bytes()]).len();
    fs::write(
        report_path,
        format!(
            "catalogue_bytes={catalogue_bytes}\narchive_bytes={}\nscans={}\nwrites={}\n\
             bulk_rows={bulk_rows}\nrestored_rows={restored_rows}\n\
             restored_canary={restored_canary}\nleaks={}\n",
            report.archive_bytes,
            scans.get(),
            writes.get(),
            leaks.borrow().join("; "),
        ),
    )
    .expect("child report");
}
