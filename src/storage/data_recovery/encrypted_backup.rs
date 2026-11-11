// SPDX-License-Identifier: AGPL-3.0-only

//! The service-owned encrypted backup operation (M06-T01).
//!
//! One run produces one archive file (one generation) in a user-chosen
//! destination folder, encrypted with a key derived from a user-kept recovery
//! password (see [`super::archive_crypto`] for the container).
//!
//! Order of a run, per the Backups decision (2026-10-08):
//!
//! 1. Derive the archive key (Argon2id), before any store lock.
//! 2. Under the publication gate: copy the catalogue with SQLite's online
//!    backup API into a private in-memory database, read the signed no-op
//!    bodies, list the packs and pin every one of them with a pin row in the
//!    live catalogue. Then release the gate.
//! 3. Seal the in-memory copy (credential rows, pairing and collector
//!    authority, pin rows) and stream one encrypted archive straight into a
//!    partial file in the destination: manifest, catalogue, packs, no-ops.
//! 4. Publish by a no-replace rename, release the pins, rotate.
//!
//! No plaintext is ever staged on disk. The catalogue copy is a `:memory:`
//! database with in-memory journal and temporary storage, so it has no file
//! and no side file; once sealed it is serialised and the database dropped; the no-op bodies stay in memory; packs are read from
//! the live store, where the pins keep GC from removing them until the run
//! ends. Compaction never deletes a pack file (it only retires catalogue
//! references, which GC later acts on), so the pin protects a pack against
//! both. A pin row carries a lease: a run killed with its pins in place stops
//! protecting its packs when the lease runs out, and the next run clears it.
//!
//! Memory. The Hub's peak is 1 GiB. The key derivation (at most
//! [`super::archive_crypto::MAX_KDF_MEMORY_KIB`]) finishes and frees its memory before the copy
//! starts; the catalogue copy is capped at [`MAX_CATALOGUE_IMAGE_BYTES`] and
//! counted twice (the copy and its serialised image overlap briefly); the
//! rest is fixed buffers. See
//! `memory_budget_fits_the_hub_peak` in the tests.
//!
//! Failure rule. A generation becomes visible only by a no-replace rename of a
//! fully written, synced partial file, so every failure before the rename
//! leaves the destination exactly as it was: the last good generation stays.
//! Rotation runs only after publication and never touches the new generation;
//! a rotation failure is reported beside a successful backup.
//!
//! Authenticity (M06-T02). Each generation's manifest is the first record of
//! its archive, so it is authenticated by the password-derived key with the
//! rest of the stream; it names the installation, the generation and its
//! creation time, and the file name must be exactly the one those imply.
//! Verify and restore refuse a file whose name does not match its
//! authenticated generation. Retention, stale-partial cleanup and every
//! removal follow [`retention`]: a generation counts or is removed only after
//! full authentication, the new one only as the object this run wrote, a
//! removal claims its candidate into a protected directory first, a partial is
//! removed only when the live catalogue records it as an abandoned run, and on
//! a volume that does not enforce ownership nothing is deleted. On a volume
//! that enforces ownership, removable or not, the folder and the archives must
//! also pass POSIX and ACL admission ([`super::backup_admission`]).
//!
//! Secret exclusion (M06-T02). The seal removes credential rows, the
//! credential unit's status row, pairing and collector authority, then
//! vacuums the copy, so free pages copied from the live catalogue cannot
//! carry an old refresh-token envelope or bearer digest into the archive. Key
//! files (credential keys, the Fleet virtual-key private key, TLS identity,
//! cursor key) are never members.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    num::NonZeroUsize,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::{
    BACKUP_KIND, BACKUP_SCOPE, BUILD_VERSION, BackupManifest, BackupMember, CATALOGUE_MEMBER,
    DataRecoveryError, DataRecoveryReport, HubStore, MAX_BACKUP_MEMBERS, MAX_MANIFEST_BYTES,
    MAX_SCHEMA_22_NOOP_BYTES, PACK_DIRECTORY, PRIVATE_FILE_MODE, SCHEMA_22_NOOP_DIRECTORY,
    SCHEMA_VERSION, Sha256, StagingDirectory, StoreError, admit_staging_capacity,
    backup_admission::{self, AdmittedDirectory},
    archive_crypto::{
        ArchiveReader, ArchiveWriter, DEFAULT_CHUNK_BYTES, DEFAULT_KDF, KdfParams, MAX_CHUNK_BYTES,
        MAX_KDF_MEMORY_KIB, SealingKey, TAG_BYTES, map_write_error,
    },
    canonical_json, canonical_pack_filename, catalogue_identity, check_backup_admission,
    create_private_directory, current_epoch_ms, current_schema_22_noop_pairs_in,
    excluded_host_state_strings, expected_protocol, finalize_restore,
    immutable_database_identity_at_schema, invalid, io_error, manifest_copy_bytes,
    open_regular_nonblocking, parse_canonical_json, prepare_new_destination, read_directory,
    report, seal_backup_credentials, seal_backup_credentials_checked, seal_host_authority,
    vacuum_sealed_catalogue, validate_manifest, validate_restored_data_tree,
    validate_schema_22_noop_bytes,
};
use rusqlite::{
    Connection, MAIN_DB, TransactionBehavior,
    backup::{Backup, StepResult},
    params,
    serialize::Data,
};
use serde::Serialize;
use sha2::Digest;
use uuid::Uuid;

mod retention;

const ARCHIVE_PREFIX: &str = "teslatlas-hub-backup-";
const ARCHIVE_SUFFIX: &str = ".tlbk";
const PARTIAL_PREFIX: &str = ".teslatlas-hub-backup-";
const PARTIAL_SUFFIX: &str = ".partial";
const MANIFEST_RECORD: &str = "manifest.json";
const RECORD_FILE: u8 = 1;
const RECORD_END: u8 = 0xFE;
const MAX_RECORD_PATH_BYTES: usize = 1024;
pub(super) const STREAM_BUFFER_BYTES: usize = 1024 * 1024;

/// The Hub's whole-process peak (Backups decision, 2026-10-08).
pub(super) const HUB_PEAK_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;
/// What the rest of the Hub may hold while a backup runs.
pub(super) const SERVICE_RESERVE_BYTES: u64 = 384 * 1024 * 1024;
/// The largest catalogue an encrypted backup copies. The copy and its
/// serialised image coexist briefly, so the budget counts it twice; the vacuum
/// of the sealed copy (its rebuilt image beside the copy) fits the same two.
pub(super) const MAX_CATALOGUE_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
// The memory budget holds at compile time (the tests spell it out): the key
// derivation phase, then the copy, no-op bodies and stream buffers, each beside
// the service reserve.
const _: () = assert!(
    SERVICE_RESERVE_BYTES + MAX_KDF_MEMORY_KIB as u64 * 1024 <= HUB_PEAK_MEMORY_BYTES
        && SERVICE_RESERVE_BYTES
            + 2 * MAX_CATALOGUE_IMAGE_BYTES
            + MAX_BACKUP_MEMBERS as u64 * MAX_SCHEMA_22_NOOP_BYTES
            + STREAM_BUFFER_BYTES as u64
            + (MAX_CHUNK_BYTES + TAG_BYTES) as u64
            <= HUB_PEAK_MEMORY_BYTES
);
const BACKUP_STEP_PAGES: i32 = 256;
const BACKUP_BUSY_RETRIES: u32 = 2_000;
const SQLITE_HEADER_BYTES: usize = 100;

/// The pin rows of running encrypted backups. Backup-owned live state: it is
/// created on first use, GC reads it when present, and the seal drops it from
/// every copied or restored catalogue.
const PACK_PIN_TABLE: &str = "backup_pack_pins";
const CREATE_PACK_PIN_TABLE: &str = "CREATE TABLE IF NOT EXISTS backup_pack_pins (
    pin_id TEXT NOT NULL CHECK(length(pin_id) = 36),
    sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > 0),
    PRIMARY KEY(pin_id, sha256)
) STRICT, WITHOUT ROWID;";
pub(super) const DROP_PACK_PIN_TABLE: &str =
    "DROP TABLE IF EXISTS backup_pack_pins; DROP TABLE IF EXISTS backup_runs;";
/// A pin's lease; a running backup renews it once half has passed.
pub(super) const PACK_PIN_LEASE_MS: i64 = 6 * 60 * 60 * 1000;

#[cfg(test)]
pub(super) use test_hooks::{Action as HookAction, clear_hook, set_hook};

/// Everything one backup run needs. The password is never logged or stored.
pub struct EncryptedBackupRequest<'a> {
    /// The user-selected destination folder; it must already exist.
    pub destination: &'a Path,
    /// The user-kept recovery password (at least 12 characters).
    pub password: &'a str,
    /// Generations to keep after a successful run; `None` keeps every one.
    pub keep: Option<NonZeroUsize>,
    /// Set by the caller to stop the run; checked per chunk and per member.
    pub cancel: Option<Arc<AtomicBool>>,
    pub(crate) kdf: KdfParams,
    pub(crate) chunk_bytes: usize,
}

impl<'a> EncryptedBackupRequest<'a> {
    pub fn new(destination: &'a Path, password: &'a str) -> Self {
        Self {
            destination,
            password,
            keep: None,
            cancel: None,
            kdf: DEFAULT_KDF,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptedBackupReport {
    pub status: &'static str,
    pub path: PathBuf,
    pub generation: Uuid,
    pub installation_id: Uuid,
    pub member_count: usize,
    pub payload_bytes: u64,
    pub archive_bytes: u64,
    /// Older generations removed by rotation after this run.
    pub rotated_generations: usize,
    /// Set when rotation failed; the new generation is still good.
    pub rotation_error: Option<String>,
}

fn cancelled(cancel: Option<&AtomicBool>) -> Result<(), DataRecoveryError> {
    if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        Err(DataRecoveryError::Cancelled)
    } else {
        Ok(())
    }
}

/// A test seam: stages are "catalogue_copied" (under the gate), "write",
/// "record", "before_publish" and "rotate_remove". Production builds compile
/// this to nothing.
#[cfg(test)]
fn hook(stage: &str, path: &Path) -> Result<(), DataRecoveryError> {
    match test_hooks::call(stage, path) {
        HookAction::Continue => Ok(()),
        HookAction::Fail(kind) => Err(io_error(
            "injected failure",
            path,
            io::Error::new(kind, format!("injected at {stage}")),
        )),
        HookAction::Crash => {
            test_hooks::CRASHED.with(|flag| flag.set(true));
            Err(invalid("simulated crash"))
        }
    }
}

#[cfg(not(test))]
#[inline]
#[allow(clippy::unnecessary_wraps)]
fn hook(_stage: &str, _path: &Path) -> Result<(), DataRecoveryError> {
    Ok(())
}

/// The destination file. Every write passes the test seam, which can inject a
/// full disk or an I/O error.
struct Sink {
    file: File,
    #[cfg_attr(not(test), allow(dead_code))]
    path: PathBuf,
}

impl Write for Sink {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        #[cfg(test)]
        match test_hooks::call("write", &self.path) {
            HookAction::Continue => {}
            HookAction::Fail(kind) => return Err(io::Error::new(kind, "injected write failure")),
            HookAction::Crash => {
                test_hooks::CRASHED.with(|flag| flag.set(true));
                return Err(io::Error::other("simulated crash"));
            }
        }
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

enum Source {
    /// The sealed in-memory catalogue copy.
    Catalogue,
    /// A pack in the live store, content-addressed by its own name and pinned.
    Pack(PathBuf),
    /// A small body held in memory (a signed schema 2.2 no-op).
    Bytes(Vec<u8>),
}

struct Planned {
    member: BackupMember,
    source: Source,
}

/// Pin rows that keep GC from removing the packs a run is streaming. They are
/// written under the publication gate and released when the run ends,
/// whichever way it ends; a killed run leaves them to their lease.
pub(super) struct PackPins<'a> {
    store: &'a HubStore,
    pin_id: Uuid,
    expires_at_ms: i64,
}

fn pin_error(error: rusqlite::Error) -> DataRecoveryError {
    DataRecoveryError::Store(StoreError::Query(error))
}

impl<'a> PackPins<'a> {
    /// Pin `digests`; the caller holds the publication gate.
    fn take<'d>(
        store: &'a HubStore,
        digests: impl Iterator<Item = &'d str>,
    ) -> Result<Self, DataRecoveryError> {
        let now = current_epoch_ms()?;
        let expires_at_ms = now
            .checked_add(PACK_PIN_LEASE_MS)
            .ok_or_else(|| invalid("backup pin lease is out of range"))?;
        let pin_id = Uuid::new_v4();
        let mut connection = store.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(pin_error)?;
        transaction
            .execute_batch(CREATE_PACK_PIN_TABLE)
            .map_err(pin_error)?;
        transaction
            .execute(
                "DELETE FROM backup_pack_pins WHERE expires_at_ms <= ?1",
                params![now],
            )
            .map_err(pin_error)?;
        for digest in digests {
            transaction
                .execute(
                    "INSERT INTO backup_pack_pins(pin_id, sha256, expires_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![pin_id.to_string(), digest, expires_at_ms],
                )
                .map_err(pin_error)?;
        }
        transaction.commit().map_err(pin_error)?;
        Ok(Self {
            store,
            pin_id,
            expires_at_ms,
        })
    }

    /// Extend the lease once half of it has passed.
    fn renew_if_due(&mut self) -> Result<(), DataRecoveryError> {
        let now = current_epoch_ms()?;
        if self.expires_at_ms - now > PACK_PIN_LEASE_MS / 2 {
            return Ok(());
        }
        let expires_at_ms = now
            .checked_add(PACK_PIN_LEASE_MS)
            .ok_or_else(|| invalid("backup pin lease is out of range"))?;
        self.store
            .open()?
            .execute(
                "UPDATE backup_pack_pins SET expires_at_ms = ?2 WHERE pin_id = ?1",
                params![self.pin_id.to_string(), expires_at_ms],
            )
            .map_err(pin_error)?;
        self.expires_at_ms = expires_at_ms;
        Ok(())
    }

    fn release_rows(&self) -> Result<(), DataRecoveryError> {
        self.store
            .open()?
            .execute(
                "DELETE FROM backup_pack_pins WHERE pin_id = ?1",
                params![self.pin_id.to_string()],
            )
            .map_err(pin_error)?;
        Ok(())
    }
}

impl Drop for PackPins<'_> {
    fn drop(&mut self) {
        // A failed release leaves the rows to their lease; nothing else
        // depends on it.
        let _ = self.release_rows();
    }
}

/// The packs that live backups have pinned, for GC. A store that never ran an
/// encrypted backup has no pin table and no pins; an expired pin protects
/// nothing.
pub(crate) fn live_backup_pack_pins(
    connection: &Connection,
    now_ms: i64,
) -> Result<Vec<String>, StoreError> {
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [PACK_PIN_TABLE],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    if !present {
        return Ok(Vec::new());
    }
    connection
        .prepare("SELECT DISTINCT sha256 FROM backup_pack_pins WHERE expires_at_ms > ?1")
        .map_err(StoreError::Query)?
        .query_map(params![now_ms], |row| row.get::<_, String>(0))
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)
}

/// Copy the live catalogue with SQLite's online backup API into a private
/// `:memory:` database. The copy has no file, so no journal, WAL or temporary
/// side file can exist for it (temporary storage is in memory too). A source
/// beyond [`MAX_CATALOGUE_IMAGE_BYTES`] is refused before anything is copied,
/// and a source that grows past it during the copy stops the copy.
pub(super) fn copy_catalogue_into_memory(
    store: &HubStore,
) -> Result<Connection, DataRecoveryError> {
    let source = store.open()?;
    let pragma = |name: &str| -> Result<u64, DataRecoveryError> {
        let value: i64 = source
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .map_err(DataRecoveryError::CatalogueIdentity)?;
        u64::try_from(value).map_err(|_| invalid("catalogue page geometry is invalid"))
    };
    let page_size = pragma("page_size")?;
    let image_bytes = pragma("page_count")?
        .checked_mul(page_size)
        .ok_or(DataRecoveryError::CapacityOverflow)?;
    let too_large = || {
        invalid(format!(
            "the Hub catalogue ({image_bytes} bytes) is larger than the {} MiB that an \
             encrypted backup may hold in memory",
            MAX_CATALOGUE_IMAGE_BYTES / (1024 * 1024)
        ))
    };
    if image_bytes > MAX_CATALOGUE_IMAGE_BYTES {
        return Err(too_large());
    }
    let mut memory = Connection::open_in_memory().map_err(DataRecoveryError::CatalogueIdentity)?;
    memory
        .execute_batch("PRAGMA temp_store = MEMORY;")
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    {
        let backup =
            Backup::new(&source, &mut memory).map_err(DataRecoveryError::CatalogueIdentity)?;
        let mut busy = 0_u32;
        loop {
            match backup
                .step(BACKUP_STEP_PAGES)
                .map_err(DataRecoveryError::CatalogueIdentity)?
            {
                StepResult::Done => break,
                StepResult::More => busy = 0,
                _ => {
                    busy += 1;
                    if busy > BACKUP_BUSY_RETRIES {
                        return Err(invalid(
                            "the live catalogue stayed locked during the backup copy",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let pages = u64::try_from(backup.progress().pagecount).unwrap_or(u64::MAX);
            if pages.saturating_mul(page_size) > MAX_CATALOGUE_IMAGE_BYTES {
                return Err(too_large());
            }
        }
    }
    Ok(memory)
}

/// The sealed catalogue image and its archive member. The image streams with
/// its file-format version bytes set to the rollback journal (the copy of a
/// WAL catalogue carries WAL's), exactly as a sealed staged file would be.
struct SealedCatalogue {
    image: Data<'static>,
    header: [u8; SQLITE_HEADER_BYTES],
    member: BackupMember,
}

/// Seal the copy, then serialise it into one in-memory image and drop the
/// database: the two copies coexist only for the length of the serialisation.
fn seal_catalogue(
    mut connection: Connection,
    installation_id: Uuid,
) -> Result<SealedCatalogue, DataRecoveryError> {
    seal_backup_credentials_checked(&connection)?;
    seal_backup_credentials(&mut connection)?;
    seal_host_authority(&mut connection, current_epoch_ms()?)?;
    vacuum_sealed_catalogue(&connection)?;
    if catalogue_identity(&connection, SCHEMA_VERSION)? != installation_id {
        return Err(invalid(
            "copied catalogue installation ID does not match the live source",
        ));
    }
    let image: Data<'static> = match connection
        .serialize(MAIN_DB)
        .map_err(DataRecoveryError::CatalogueIdentity)?
    {
        Data::Owned(owned) => Data::Owned(owned),
        Data::Shared(_) => return Err(invalid("the in-memory catalogue copy is not private")),
    };
    drop(connection);
    if image.len() <= SQLITE_HEADER_BYTES || !matches!((image[18], image[19]), (1 | 2, 1 | 2)) {
        return Err(invalid(
            "the in-memory catalogue copy is not a database image",
        ));
    }
    let mut header = [0_u8; SQLITE_HEADER_BYTES];
    header.copy_from_slice(&image[..SQLITE_HEADER_BYTES]);
    header[18] = 1;
    header[19] = 1;
    let mut digest = Sha256::new();
    digest.update(header);
    digest.update(&image[SQLITE_HEADER_BYTES..]);
    let member = BackupMember {
        path: CATALOGUE_MEMBER.to_owned(),
        size: image.len() as u64,
        mode: PRIVATE_FILE_MODE,
        sha256: hex::encode(digest.finalize()),
    };
    Ok(SealedCatalogue {
        image,
        header,
        member,
    })
}

fn resolve_destination(destination: &Path) -> Result<PathBuf, DataRecoveryError> {
    let unavailable = |source: io::Error| DataRecoveryError::DestinationUnavailable {
        path: destination.to_path_buf(),
        source,
    };
    let metadata = fs::metadata(destination).map_err(unavailable)?;
    if !metadata.is_dir() {
        return Err(DataRecoveryError::DestinationUnavailable {
            path: destination.to_path_buf(),
            source: io::Error::new(io::ErrorKind::NotADirectory, "not a directory"),
        });
    }
    destination
        .canonicalize()
        .map_err(|source| DataRecoveryError::DestinationUnavailable {
            path: destination.to_path_buf(),
            source,
        })
}

fn generation_stamp(created_at_ms: i64) -> Result<String, DataRecoveryError> {
    let moment = time::OffsetDateTime::from_unix_timestamp(created_at_ms / 1000)
        .map_err(|_| invalid("backup timestamp is out of range"))?;
    Ok(format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}{:03}Z",
        moment.year(),
        u8::from(moment.month()),
        moment.day(),
        moment.hour(),
        moment.minute(),
        moment.second(),
        created_at_ms.rem_euclid(1000)
    ))
}

/// The file name a generation must have: the creation stamp and generation
/// of its authenticated manifest.
fn canonical_archive_name(manifest: &BackupManifest) -> Result<String, DataRecoveryError> {
    Ok(format!(
        "{ARCHIVE_PREFIX}{}-{}{ARCHIVE_SUFFIX}",
        generation_stamp(manifest.created_at_ms)?,
        manifest.generation
    ))
}

/// What the publication gate protects: the in-memory catalogue copy, the
/// no-op bodies it selects, the pack list and the pins on those packs.
struct Snapshot<'a> {
    catalogue: Connection,
    noops: Vec<Planned>,
    packs: Vec<Planned>,
    pins: PackPins<'a>,
}

fn take_snapshot(store: &HubStore) -> Result<Snapshot<'_>, DataRecoveryError> {
    let gate = store.begin_backup_snapshot()?;
    check_backup_admission(&gate)?;
    let catalogue = copy_catalogue_into_memory(store)?;
    let noops = plan_noops(store, &catalogue)?;
    let packs = plan_packs(store)?;
    let pins = PackPins::take(store, packs.iter().map(|item| item.member.sha256.as_str()))?;
    hook("catalogue_copied", store.database_path())?;
    // The gate goes as soon as the catalogue is copied and the packs pinned.
    drop(gate);
    Ok(Snapshot {
        catalogue,
        noops,
        packs,
        pins,
    })
}

/// Create one encrypted backup generation.
pub fn create_encrypted_backup(
    store: &HubStore,
    request: &EncryptedBackupRequest<'_>,
) -> Result<EncryptedBackupReport, DataRecoveryError> {
    let cancel = request.cancel.as_deref();
    cancelled(cancel)?;
    let admitted: AdmittedDirectory =
        backup_admission::admit_destination(&resolve_destination(request.destination)?)?;
    let destination = admitted.path.clone();
    let live_data = store
        .database_path()
        .parent()
        .ok_or_else(|| invalid("live Hub catalogue has no data-directory parent"))?
        .canonicalize()
        .map_err(|source| {
            io_error(
                "resolving live Hub data directory",
                store.database_path(),
                source,
            )
        })?;
    if destination.starts_with(&live_data) {
        return Err(invalid(
            "backup destination must not be inside the live Hub data directory",
        ));
    }
    // The memory-hard derivation runs, and frees its memory, before the run
    // takes the gate or copies anything.
    let key = SealingKey::derive(request.password, request.kdf, request.chunk_bytes)?;
    cancelled(cancel)?;

    let source_installation_id = store.installation_id()?;
    // Before the catalogue copy, so reading a recorded partial never runs
    // beside it.
    retention::clear_abandoned_runs(store, &admitted, request.password, source_installation_id)?;
    cancelled(cancel)?;
    store.catalogue_check()?;
    let Snapshot {
        catalogue,
        noops,
        packs,
        mut pins,
    } = take_snapshot(store)?;
    let generation = Uuid::new_v4();
    let partial_name = format!("{PARTIAL_PREFIX}{generation}{PARTIAL_SUFFIX}");
    let partial = destination.join(&partial_name);
    let mut written: Option<retention::FileIdentity> = None;
    let run = (|| {
        cancelled(cancel)?;
        let catalogue = seal_catalogue(catalogue, source_installation_id)?;
        let mut planned = vec![Planned {
            member: catalogue.member.clone(),
            source: Source::Catalogue,
        }];
        planned.extend(packs);
        planned.extend(noops);
        if planned.len() > MAX_BACKUP_MEMBERS {
            return Err(invalid("backup contains too many members"));
        }
        planned.sort_by(|left, right| left.member.path.cmp(&right.member.path));
        let payload_bytes = planned.iter().try_fold(0_u64, |total, item| {
            total
                .checked_add(item.member.size)
                .ok_or(DataRecoveryError::CapacityOverflow)
        })?;
        admit_staging_capacity(&destination, payload_bytes)?;

        let created_at_ms = current_epoch_ms()?;
        let manifest = BackupManifest {
            kind: BACKUP_KIND.to_owned(),
            generation,
            created_at_ms,
            build: BUILD_VERSION.to_owned(),
            hub_schema: SCHEMA_VERSION,
            protocol: expected_protocol(),
            installation_id: source_installation_id,
            scope: BACKUP_SCOPE.to_owned(),
            excluded_host_state: excluded_host_state_strings(),
            members: planned.iter().map(|item| item.member.clone()).collect(),
        };
        validate_manifest(&manifest)?;
        let manifest_bytes = canonical_json(&manifest)?;
        let final_name = canonical_archive_name(&manifest)?;
        let (file, identity) =
            retention::create_partial(&admitted, &partial_name)?;
        written = Some(identity);
        retention::record_run(store, generation, &admitted, identity)?;
        let archive_bytes = write_partial(
            request,
            key,
            &partial,
            file,
            &manifest_bytes,
            &planned,
            Some(catalogue),
            &mut pins,
        )?;
        hook("before_publish", &partial)?;
        cancelled(cancel)?;
        retention::publish(&admitted, &partial_name, &final_name)?;
        retention::forget_run(store, generation);
        Ok((
            final_name,
            manifest_bytes,
            manifest.members.len(),
            payload_bytes,
            archive_bytes,
        ))
    })();
    let (final_name, manifest_bytes, member_count, payload_bytes, archive_bytes) = match run {
        Ok(done) => done,
        Err(error) => {
            #[cfg(test)]
            if test_hooks::CRASHED.with(|flag| flag.replace(false)) {
                // A killed process removes nothing: its partial file and its
                // run row stay for the next run, and its pins stay until
                // their lease ends.
                std::mem::forget(pins);
                return Err(error);
            }
            if let Some(identity) = written {
                retention::discard_failed_partial(store, &admitted, generation, identity);
            }
            return Err(error);
        }
    };
    // The packs are in the published generation: the pins go now.
    drop(pins);

    let (rotated_generations, rotation_error) = match request.keep {
        Some(keep) => retention::rotate_after_publish(
            &admitted,
            keep.get(),
            &final_name,
            written.ok_or_else(|| invalid("the published generation has no identity"))?,
            &manifest_bytes,
            request.password,
            source_installation_id,
        ),
        None => (0, None),
    };
    Ok(EncryptedBackupReport {
        status: "encrypted_backup_created",
        path: destination.join(&final_name),
        generation,
        installation_id: source_installation_id,
        member_count,
        payload_bytes,
        archive_bytes,
        rotated_generations,
        rotation_error,
    })
}

/// The signed no-op body of each current schema 2.2 snapshot in the copy,
/// read while the gate still holds publication off.
fn plan_noops(store: &HubStore, catalogue: &Connection) -> Result<Vec<Planned>, DataRecoveryError> {
    let mut planned = Vec::new();
    for (name, manifest) in current_schema_22_noop_pairs_in(catalogue)? {
        let bytes = store
            .schema_22_noop_for_snapshot(manifest.vehicle_id, manifest.snapshot_id)?
            .ok_or(StoreError::Schema22NoOpNotFound)?;
        validate_schema_22_noop_bytes(&name, &manifest, &bytes)?;
        planned.push(Planned {
            member: BackupMember {
                path: format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}"),
                size: bytes.len() as u64,
                mode: PRIVATE_FILE_MODE,
                sha256: hex::encode(Sha256::digest(&bytes)),
            },
            source: Source::Bytes(bytes),
        });
        if planned.len() > MAX_BACKUP_MEMBERS {
            return Err(invalid("backup contains too many members"));
        }
    }
    Ok(planned)
}

/// Packs are immutable and content-addressed: the file name is the digest,
/// which the stream re-checks while it reads.
fn plan_packs(store: &HubStore) -> Result<Vec<Planned>, DataRecoveryError> {
    let mut planned = Vec::new();
    let pack_directory = store.packs_dir().join("sha256");
    match read_directory(&pack_directory) {
        Ok(entries) => {
            for entry in entries {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !canonical_pack_filename(&name) {
                    continue;
                }
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|source| io_error("inspecting live pack", &path, source))?;
                if !metadata.file_type().is_file() {
                    return Err(invalid(format!("live pack is not a regular file: {name}")));
                }
                let digest = name.trim_end_matches(".sqlite.zst").to_owned();
                planned.push(Planned {
                    member: BackupMember {
                        path: format!("{PACK_DIRECTORY}/{name}"),
                        size: metadata.len(),
                        mode: PRIVATE_FILE_MODE,
                        sha256: digest,
                    },
                    source: Source::Pack(path),
                });
                if planned.len() > MAX_BACKUP_MEMBERS {
                    return Err(invalid("backup contains too many members"));
                }
            }
        }
        Err(DataRecoveryError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(planned)
}

fn write_partial(
    request: &EncryptedBackupRequest<'_>,
    key: SealingKey,
    partial: &Path,
    file: File,
    manifest_bytes: &[u8],
    planned: &[Planned],
    mut catalogue: Option<SealedCatalogue>,
    pins: &mut PackPins<'_>,
) -> Result<u64, DataRecoveryError> {
    let cancel = request.cancel.as_deref();
    let sink = Sink {
        file,
        path: partial.to_path_buf(),
    };
    let mut writer = ArchiveWriter::start(sink, partial.to_path_buf(), key)?;

    hook("record", partial)?;
    put_file_header(&mut writer, MANIFEST_RECORD, manifest_bytes.len() as u64)?;
    writer.write_plain(manifest_bytes)?;
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    for item in planned {
        cancelled(cancel)?;
        pins.renew_if_due()?;
        hook("record", partial)?;
        match &item.source {
            Source::Catalogue => {
                let sealed = catalogue
                    .take()
                    .ok_or_else(|| invalid("the catalogue member appears twice"))?;
                stream_member(
                    &mut writer,
                    &item.member,
                    sealed
                        .header
                        .as_slice()
                        .chain(&sealed.image[SQLITE_HEADER_BYTES..]),
                    &mut buffer,
                    cancel,
                )?;
                // The copy's memory goes before the packs stream.
                drop(sealed);
            }
            Source::Bytes(bytes) => {
                stream_member(
                    &mut writer,
                    &item.member,
                    bytes.as_slice(),
                    &mut buffer,
                    cancel,
                )?;
            }
            Source::Pack(path) => {
                let input = open_regular_nonblocking(path, "opening backup source member")?;
                let metadata = input
                    .metadata()
                    .map_err(|source| io_error("inspecting backup source member", path, source))?;
                if metadata.len() != item.member.size || metadata.nlink() == 0 {
                    return Err(invalid(format!(
                        "backup source member changed before it was streamed: {}",
                        item.member.path
                    )));
                }
                stream_member(&mut writer, &item.member, input, &mut buffer, cancel)?;
            }
        }
    }
    writer.write_plain(&[RECORD_END])?;
    let manifest_digest: [u8; 32] = Sha256::digest(manifest_bytes).into();
    writer.write_plain(&manifest_digest)?;
    let sink = writer.finish()?;
    sink.file
        .sync_all()
        .map_err(|source| map_write_error(partial, source))?;
    let size = sink
        .file
        .metadata()
        .map_err(|source| io_error("inspecting backup archive", partial, source))?
        .len();
    Ok(size)
}

fn put_file_header(
    writer: &mut ArchiveWriter<Sink>,
    path: &str,
    size: u64,
) -> Result<(), DataRecoveryError> {
    let path_length = u16::try_from(path.len()).map_err(|_| invalid("record path is too long"))?;
    writer.write_plain(&[RECORD_FILE])?;
    writer.write_plain(&path_length.to_be_bytes())?;
    writer.write_plain(path.as_bytes())?;
    writer.write_plain(&size.to_be_bytes())
}

fn stream_member(
    writer: &mut ArchiveWriter<Sink>,
    member: &BackupMember,
    mut input: impl Read,
    buffer: &mut [u8],
    cancel: Option<&AtomicBool>,
) -> Result<(), DataRecoveryError> {
    let source_path = Path::new(&member.path);
    put_file_header(writer, &member.path, member.size)?;
    let mut digest = Sha256::new();
    let mut streamed = 0_u64;
    loop {
        cancelled(cancel)?;
        let read = input
            .read(buffer)
            .map_err(|source| io_error("reading backup source member", source_path, source))?;
        if read == 0 {
            break;
        }
        streamed = streamed
            .checked_add(read as u64)
            .filter(|total| *total <= member.size)
            .ok_or_else(|| {
                invalid(format!(
                    "backup source member grew while it was streamed: {}",
                    member.path
                ))
            })?;
        digest.update(&buffer[..read]);
        writer.write_plain(&buffer[..read])?;
    }
    buffer.fill(0);
    if streamed != member.size || hex::encode(digest.finalize()) != member.sha256 {
        return Err(invalid(format!(
            "backup source member changed or is damaged: {}",
            member.path
        )));
    }
    Ok(())
}

struct OpenedArchive {
    reader: ArchiveReader<File>,
    manifest: BackupManifest,
    manifest_bytes: Vec<u8>,
}

/// Open an archive for verify or restore: admitted for its volume, its
/// manifest authenticated, and its file name exactly the one its
/// authenticated generation implies, so a renamed or substituted generation
/// is never trusted or restored.
fn open_archive(archive: &Path, password: &str) -> Result<OpenedArchive, DataRecoveryError> {
    let parent = archive
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let trust = backup_admission::classify(parent)?;
    let file = open_regular_nonblocking(archive, "opening backup archive")?;
    backup_admission::admit_archive_file(&file, archive, trust)?;
    let opened = open_archive_file(file, archive, password)?;
    let named = archive.file_name().and_then(|name| name.to_str());
    if named != Some(canonical_archive_name(&opened.manifest)?.as_str()) {
        return Err(invalid(
            "the archive's file name does not match its authenticated generation; restore \
             the generation under the name the Hub gave it",
        ));
    }
    Ok(opened)
}

/// Authenticate the manifest record of an opened archive or partial file.
fn open_archive_file(
    file: File,
    archive: &Path,
    password: &str,
) -> Result<OpenedArchive, DataRecoveryError> {
    let mut reader = ArchiveReader::open(file, archive.to_path_buf(), password)?;
    let (path, size) = read_file_header(&mut reader)?;
    if path != MANIFEST_RECORD || size > MAX_MANIFEST_BYTES {
        return Err(DataRecoveryError::CorruptArchive(
            "the archive does not start with its manifest".into(),
        ));
    }
    let mut manifest_bytes = vec![0_u8; usize::try_from(size).map_err(|_| invalid("manifest"))?];
    reader.read_exact_plain(&mut manifest_bytes)?;
    let manifest: BackupManifest = parse_canonical_json(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    if manifest.kind != BACKUP_KIND || manifest.scope != BACKUP_SCOPE {
        return Err(DataRecoveryError::CorruptArchive(
            "the archive manifest is not a current Hub backup".into(),
        ));
    }
    Ok(OpenedArchive {
        reader,
        manifest,
        manifest_bytes,
    })
}

fn read_file_header(reader: &mut ArchiveReader<File>) -> Result<(String, u64), DataRecoveryError> {
    let mut tag = [0_u8; 1];
    reader.read_exact_plain(&mut tag)?;
    if tag[0] != RECORD_FILE {
        return Err(DataRecoveryError::CorruptArchive(
            "unexpected record in the archive".into(),
        ));
    }
    let mut length = [0_u8; 2];
    reader.read_exact_plain(&mut length)?;
    let length = usize::from(u16::from_be_bytes(length));
    if length == 0 || length > MAX_RECORD_PATH_BYTES {
        return Err(DataRecoveryError::CorruptArchive(
            "invalid record path length".into(),
        ));
    }
    let mut path = vec![0_u8; length];
    reader.read_exact_plain(&mut path)?;
    let path = String::from_utf8(path)
        .map_err(|_| DataRecoveryError::CorruptArchive("record path is not UTF-8".into()))?;
    let mut size = [0_u8; 8];
    reader.read_exact_plain(&mut size)?;
    Ok((path, u64::from_be_bytes(size)))
}

/// Read every member record in manifest order, verify size and digest, and
/// hand each member's bytes to `sink_for` when it returns a file. Ends with the
/// trailer, which must bind the manifest, and then the end of the stream.
fn stream_members(
    opened: &mut OpenedArchive,
    mut sink_for: impl FnMut(&BackupMember) -> Result<Option<File>, DataRecoveryError>,
) -> Result<(), DataRecoveryError> {
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    for member in &opened.manifest.members {
        let (path, size) = read_file_header(&mut opened.reader)?;
        if path != member.path || size != member.size {
            return Err(DataRecoveryError::CorruptArchive(format!(
                "the archive record does not match its manifest entry: {}",
                member.path
            )));
        }
        let mut sink = sink_for(member)?;
        let mut digest = Sha256::new();
        let mut remaining = size;
        while remaining > 0 {
            let take =
                usize::try_from(remaining).map_or(buffer.len(), |left| left.min(buffer.len()));
            opened.reader.read_exact_plain(&mut buffer[..take])?;
            digest.update(&buffer[..take]);
            if let Some(file) = sink.as_mut() {
                file.write_all(&buffer[..take]).map_err(|source| {
                    io_error(
                        "writing restored data member",
                        Path::new(&member.path),
                        source,
                    )
                })?;
            }
            remaining -= take as u64;
        }
        if hex::encode(digest.finalize()) != member.sha256 {
            return Err(DataRecoveryError::CorruptArchive(format!(
                "the archive member digest does not match its manifest: {}",
                member.path
            )));
        }
        if let Some(file) = sink {
            file.sync_all().map_err(|source| {
                io_error(
                    "syncing restored data member",
                    Path::new(&member.path),
                    source,
                )
            })?;
        }
    }
    let mut trailer = [0_u8; 33];
    opened.reader.read_exact_plain(&mut trailer)?;
    if trailer[0] != RECORD_END || trailer[1..] != sha256_digest_bytes(&opened.manifest_bytes) {
        return Err(DataRecoveryError::CorruptArchive(
            "the archive trailer does not match its manifest".into(),
        ));
    }
    opened.reader.expect_end()
}

fn sha256_digest_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Authenticate and fully read an archive without writing anything: every
/// chunk, every member digest and the trailer.
pub fn verify_encrypted_backup(
    archive: &Path,
    password: &str,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    let mut opened = open_archive(archive, password)?;
    stream_members(&mut opened, |_| Ok(None))?;
    report("data_backup_verified", archive, &opened.manifest)
}

/// Restore an archive into a new directory. The wrong password is refused
/// before anything is written; any damage found while streaming discards the
/// staged tree, so nothing partial is ever published.
pub fn restore_encrypted_backup(
    archive: &Path,
    password: &str,
    destination: &Path,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    let (destination, parent) = prepare_new_destination(destination)?;
    let mut opened = open_archive(archive, password)?;
    admit_staging_capacity(&parent, manifest_copy_bytes(&opened.manifest)?)?;
    let staging = StagingDirectory::create(&parent)?;
    create_private_directory(&staging.path.join("packs"))?;
    create_private_directory(&staging.path.join("packs").join("sha256"))?;
    if opened.manifest.members.iter().any(|member| {
        member
            .path
            .starts_with(&format!("{SCHEMA_22_NOOP_DIRECTORY}/"))
    }) {
        create_private_directory(&staging.path.join("packs").join("noop"))?;
    }
    let root = staging.path.clone();
    stream_members(&mut opened, |member| {
        let relative = member
            .path
            .strip_prefix("data/")
            .ok_or_else(|| invalid("backup member is outside the data payload"))?;
        let target = root.join(relative);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .open(&target)
            .map(Some)
            .map_err(|source| io_error("creating restored data member", &target, source))
    })?;

    validate_restored_data_tree(&staging.path, &opened.manifest.members, false)?;
    if immutable_database_identity_at_schema(&staging.path, opened.manifest.hub_schema)?
        != opened.manifest.installation_id
    {
        return Err(invalid(
            "manifest installation ID does not match the restored catalogue",
        ));
    }
    finalize_restore(staging, &destination, &opened.manifest)
}

/// The whole authenticated plaintext stream of an archive, for tests that
/// look for bytes an archive must never carry.
#[cfg(test)]
pub(super) fn archive_plaintext(archive: &Path, password: &str) -> Result<Vec<u8>, DataRecoveryError> {
    let file = open_regular_nonblocking(archive, "opening backup archive")?;
    ArchiveReader::open(file, archive.to_path_buf(), password)?.read_to_end_plain()
}

#[cfg(test)]
pub(super) mod test_hooks {
    use std::{
        cell::{Cell, RefCell},
        io,
        path::Path,
    };

    pub enum Action {
        Continue,
        Fail(io::ErrorKind),
        /// Leave everything as a killed process would.
        Crash,
    }

    type Hook = Box<dyn FnMut(&str, &Path) -> Action>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
        pub(super) static CRASHED: Cell<bool> = const { Cell::new(false) };
    }

    pub fn set_hook(hook: impl FnMut(&str, &Path) -> Action + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub fn clear_hook() {
        HOOK.with(|slot| *slot.borrow_mut() = None);
        CRASHED.with(|flag| flag.set(false));
    }

    pub(super) fn call(stage: &str, path: &Path) -> Action {
        HOOK.with(|slot| match slot.borrow_mut().as_mut() {
            Some(hook) => hook(stage, path),
            None => Action::Continue,
        })
    }
}
