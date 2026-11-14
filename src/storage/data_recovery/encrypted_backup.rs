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
//! and no side file; once sealed it is serialised and the database dropped.
//! A catalogue above the in-memory cap is copied instead into an encrypted
//! scratch volume ([`super::encrypted_scratch`], M06-T07): SQLite works on
//! plain pages through the attached volume while the disk holds only its
//! encrypted blocks, the seal rebuilds the copy there with `VACUUM INTO`
//! (no temporary database, no sort spill) and the sealed file streams from
//! there; the volume is detached and its image removed once the catalogue
//! has streamed. The no-op bodies stay in memory; packs are read from
//! the live store, where the pins keep GC from removing them until the run
//! ends. Compaction never deletes a pack file (it only retires catalogue
//! references, which GC later acts on), so the pin protects a pack against
//! both. A pin row carries a lease: a run killed with its pins in place stops
//! protecting its packs when the lease runs out, and the next run clears it.
//!
//! Memory. The Hub's peak is 1 GiB. The key derivation (at most
//! [`super::archive_crypto::MAX_KDF_MEMORY_KIB`]) finishes and frees its memory before the copy
//! starts; the in-memory catalogue copy is capped at
//! [`MAX_CATALOGUE_IMAGE_BYTES`] and counted twice (the copy and its
//! serialised image overlap briefly); the rest is fixed buffers. A larger
//! catalogue takes the scratch path, where memory is the SQLite page caches
//! ([`SCRATCH_CACHE_KIB`] each) and the stream buffers, whatever the
//! catalogue's size. See `memory_budget_fits_the_hub_peak` and
//! `backup_large_catalogue_*` in the tests.
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
//! Member count (M06-T10). A run has no member bound and holds nothing per
//! member. Under the gate the pack directory is read one entry at a time
//! straight into the run's pin rows, counting packs and bytes; the stream then
//! pages through those rows in digest order. The manifest (the first record)
//! declares the member count and payload bytes instead of listing members, and
//! each member record carries its own path, size and digest (format v5). The
//! reader checks every record as it arrives, holding only the previous path,
//! and the totals against the declared ones before the trailer. Bundles
//! written before v5 list their members in the manifest; they still verify and
//! restore, with that list bounded as when they were written.
//!
//! Secret exclusion (M06-T02). The seal removes credential rows, the
//! credential unit's status row, pairing and collector authority, then
//! vacuums the copy, so free pages copied from the live catalogue cannot
//! carry an old refresh-token envelope or bearer digest into the archive. Key
//! files (credential keys, the Fleet virtual-key private key, TLS identity,
//! cursor key) are never members.

#[cfg(test)]
use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    num::NonZeroUsize,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::{
    BACKUP_SCOPE, BUILD_VERSION, BackupFormat, BackupManifest, BackupMember, CATALOGUE_MEMBER,
    COPY_CAPACITY_FIXED_HEADROOM_BYTES, DataRecoveryError, DataRecoveryReport, HubStore,
    MAX_MANIFEST_BYTES, MAX_SCHEMA_22_NOOP_BYTES, MemberKind, PACK_DIRECTORY,
    PRIVATE_DIRECTORY_MODE, PRIVATE_FILE_MODE, RestoredMembers, SCHEMA_22_NOOP_DIRECTORY,
    SCHEMA_VERSION, STREAMED_BACKUP_KIND, Sha256, StagedTree, StoreError, StreamedMembers,
    admit_known_capacity, admit_staging_capacity, allocation_block_bytes, available_bytes,
    backup_format,
    backup_admission::{self, AdmittedDirectory},
    encrypted_scratch::EncryptedScratch,
    archive_crypto::{
        ArchiveReader, ArchiveWriter, DEFAULT_CHUNK_BYTES, DEFAULT_KDF, HEADER_BYTES, KdfParams,
        MAX_CHUNK_BYTES, MAX_KDF_MEMORY_KIB, SealingKey, TAG_BYTES, map_write_error,
    },
    canonical_json, canonical_pack_filename, catalogue_identity, check_backup_admission,
    checked_capacity_add, current_epoch_ms, current_noop_manifest,
    current_schema_22_noop_pairs_in, excluded_host_state_strings, expected_protocol,
    finalize_restore, immutable_database_identity_at_schema, invalid, io_error,
    manifest_totals, member_storage_bytes, open_regular_nonblocking, parse_canonical_json,
    prepare_new_destination, report, schema_22_noop_member_filename, seal_backup_credentials, seal_backup_credentials_checked,
    seal_host_authority, staging_required_bytes, vacuum_sealed_catalogue, validate_manifest,
    validate_member, validate_restored_data_tree, validate_schema_22_noop_bytes, VolumeFile,
};
use rusqlite::{
    Connection, MAIN_DB, OpenFlags, TransactionBehavior,
    backup::{Backup, StepResult},
    params,
    serialize::Data,
};
use serde::Serialize;
use sha2::Digest;
use uuid::Uuid;

mod restore_staging;
mod retention;

const ARCHIVE_PREFIX: &str = "teslatlas-hub-backup-";
const ARCHIVE_SUFFIX: &str = ".tlbk";
const PARTIAL_PREFIX: &str = ".teslatlas-hub-backup-";
const PARTIAL_SUFFIX: &str = ".partial";
const MANIFEST_RECORD: &str = "manifest.json";
/// The manifest record, and every member record of a bundle before v5.
const RECORD_FILE: u8 = 1;
/// A member record of a streamed (v5) bundle: it carries its own digest.
const RECORD_MEMBER: u8 = 2;
const RECORD_END: u8 = 0xFE;
/// Pinned packs read from the live catalogue per page while streaming.
const PACK_PAGE_ROWS: i64 = 256;
const MAX_RECORD_PATH_BYTES: usize = 1024;
/// The longest member path a bundle writes (a no-op body's, 94 bytes),
/// rounded up: what a member record carries besides its bytes is the tag,
/// the path length, the path, the size and the digest.
const MAX_MEMBER_PATH_BYTES: u64 = 128;
const MEMBER_RECORD_OVERHEAD_BYTES: u64 = 1 + 2 + MAX_MEMBER_PATH_BYTES + 8 + 32;
/// The manifest size counted before the manifest exists.
const MANIFEST_ESTIMATE_BYTES: u64 = 64 * 1024;
/// What one pin row can add to the live catalogue and its WAL: the row and
/// its index entry, each written again as a WAL frame copy, with room to
/// spare (REV-CX-M06-T10 finding 2).
const PIN_ROW_BYTES: u64 = 1024;
/// Pin rows admitted at a time beyond the pre-count.
const PIN_ADMISSION_ROWS: u64 = 1024;
pub(super) const STREAM_BUFFER_BYTES: usize = 1024 * 1024;

/// The Hub's whole-process peak (Backups decision, 2026-10-08).
pub(super) const HUB_PEAK_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;
/// What the rest of the Hub may hold while a backup runs.
pub(super) const SERVICE_RESERVE_BYTES: u64 = 384 * 1024 * 1024;
/// The largest catalogue an encrypted backup copies. The copy and its
/// serialised image coexist briefly, so the budget counts it twice; the vacuum
/// of the sealed copy (its rebuilt image beside the copy) fits the same two.
pub(super) const MAX_CATALOGUE_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
/// The signed no-op bodies a run holds: one per vehicle with a current
/// schema 2.2 snapshot, so they follow the number of vehicles, not history.
/// 4,096 vehicles at the largest body size; a run above it is refused.
pub(super) const MAX_HELD_NOOP_BYTES: u64 = 4_096 * MAX_SCHEMA_22_NOOP_BYTES;
// The memory budget holds at compile time (the tests spell it out): the key
// derivation phase, then the copy, no-op bodies and stream buffers, each beside
// the service reserve.
const _: () = assert!(
    SERVICE_RESERVE_BYTES + MAX_KDF_MEMORY_KIB as u64 * 1024 <= HUB_PEAK_MEMORY_BYTES
        && SERVICE_RESERVE_BYTES
            + 2 * MAX_CATALOGUE_IMAGE_BYTES
            + MAX_HELD_NOOP_BYTES
            + STREAM_BUFFER_BYTES as u64
            + (MAX_CHUNK_BYTES + TAG_BYTES) as u64
            <= HUB_PEAK_MEMORY_BYTES
);
/// A catalogue above this size is copied into the encrypted scratch volume.
/// The volume is prepared before the publication gate, so the threshold
/// leaves room for the catalogue to grow before the copy under the gate.
pub(super) const SCRATCH_THRESHOLD_BYTES: u64 = MAX_CATALOGUE_IMAGE_BYTES / 4 * 3;
/// The page cache of each scratch-path connection (copy, seal, rebuild).
pub(super) const SCRATCH_CACHE_KIB: u64 = 8 * 1024;
const _: () = assert!(
    SERVICE_RESERVE_BYTES
        + 3 * SCRATCH_CACHE_KIB * 1024
        + MAX_HELD_NOOP_BYTES
        + 2 * STREAM_BUFFER_BYTES as u64
        + (MAX_CHUNK_BYTES + TAG_BYTES) as u64
        <= HUB_PEAK_MEMORY_BYTES
);
const SCRATCH_COPY: &str = "catalogue.sqlite";
const SCRATCH_SEALED: &str = "sealed.sqlite";
const BACKUP_STEP_PAGES: i32 = 256;
const BACKUP_BUSY_RETRIES: u32 = 2_000;
const SQLITE_HEADER_BYTES: usize = 100;

/// The pin rows of running encrypted backups. Backup-owned live state in a
/// table the schema migration creates (M06-T07); the seal empties it in every
/// copied or restored catalogue.
const PACK_PIN_TABLE: &str = "backup_pack_pins";

fn pack_pin_table_present(connection: &Connection) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [PACK_PIN_TABLE],
        |row| row.get(0),
    )
}

/// Remove every pin row and every recorded run (`backup_runs`, M06-T02) from
/// a copied or restored catalogue. Both tables are live-host state that the
/// schema migration creates: they stay, empty; a catalogue from before a
/// table has none of its rows.
pub(super) fn clear_pack_pins(connection: &Connection) -> Result<(), rusqlite::Error> {
    if pack_pin_table_present(connection)? {
        connection.execute("DELETE FROM backup_pack_pins", [])?;
    }
    if retention::run_table_present(connection)? {
        connection.execute("DELETE FROM backup_runs", [])?;
    }
    Ok(())
}
/// A pin's lease; a running backup renews it once half has passed.
pub(super) const PACK_PIN_LEASE_MS: i64 = 6 * 60 * 60 * 1000;

#[cfg(test)]
pub(super) use test_hooks::{Action as HookAction, clear_hook, force_scratch_above, set_hook};

/// The size above which a run copies through the encrypted scratch volume;
/// tests lower it to take that path with small catalogues.
fn scratch_threshold() -> u64 {
    #[cfg(test)]
    if let Some(bytes) = test_hooks::SCRATCH_ABOVE.with(std::cell::Cell::get) {
        return bytes;
    }
    SCRATCH_THRESHOLD_BYTES
}

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
    /// Require a private destination where the volume enforces ownership:
    /// no access at all for other accounts, by mode or by ACL (M06-T14). The
    /// shipped `backup` command sets it; the service's schedule keeps the
    /// rule that no other account may write there.
    pub private_destination: bool,
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
            private_destination: false,
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
    /// Set when rotation failed; the new generation is still good (it was
    /// authenticated in full as the object the run wrote before rotation).
    pub rotation_error: Option<String>,
}

fn cancelled(cancel: Option<&AtomicBool>) -> Result<(), DataRecoveryError> {
    if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        Err(DataRecoveryError::Cancelled)
    } else {
        Ok(())
    }
}

/// A test seam: stages are "scratch_copied" (the encrypted image, under the
/// gate), "catalogue_copied" (under the gate), "catalogue_sealed", "write", "record", "before_publish" and
/// "rotate_remove". Production builds compile this to nothing.
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

/// The destination file. Every write passes the test seams, which can inject a
/// full disk, an I/O error or a volume that fills part-way.
struct Sink {
    file: VolumeFile,
    #[cfg_attr(not(test), allow(dead_code))]
    path: PathBuf,
    /// SHA-256 of every byte written to the file, in order.
    digest: Sha256,
}

impl Sink {
    fn new(file: File, path: PathBuf) -> Self {
        Self {
            file: VolumeFile(file),
            path,
            digest: Sha256::new(),
        }
    }
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
        let written = self.file.write(buffer)?;
        self.digest.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// A signed schema 2.2 no-op body, read under the gate and held in memory.
struct NoopBody {
    member: BackupMember,
    bytes: Vec<u8>,
}

/// The packs a run pinned: how many and their bytes. The pin rows themselves
/// are the list; nothing per pack is held in memory.
#[derive(Debug, Clone, Copy)]
struct PackPlan {
    count: u64,
    bytes: u64,
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
    /// List the live packs and pin every one of them in one transaction; the
    /// caller holds the publication gate. The directory is read one entry at a
    /// time straight into the pin rows, counting packs and bytes, so the list
    /// is never held in memory. Packs are immutable and content-addressed:
    /// the file name is the digest, which the stream re-checks while it reads.
    ///
    /// `admitted_rows` pin rows were admitted on the live data volume before
    /// the gate; every further [`PIN_ADMISSION_ROWS`] are admitted before
    /// they are written, so the pins never fill the volume.
    fn take(store: &'a HubStore, admitted_rows: u64) -> Result<(Self, PackPlan), DataRecoveryError> {
        let mut admitted_rows = admitted_rows;
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
            .execute(
                "DELETE FROM backup_pack_pins WHERE expires_at_ms <= ?1",
                params![now],
            )
            .map_err(pin_error)?;
        let mut plan = PackPlan { count: 0, bytes: 0 };
        let pack_directory = store.packs_dir().join("sha256");
        let entries = match fs::read_dir(&pack_directory) {
            Ok(entries) => Some(entries),
            Err(source) if source.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(io_error("reading the live pack directory", &pack_directory, source));
            }
        };
        let mut insert = transaction
            .prepare(
                "INSERT INTO backup_pack_pins(pin_id, sha256, expires_at_ms)
                 VALUES (?1, ?2, ?3)",
            )
            .map_err(pin_error)?;
        for entry in entries.into_iter().flatten() {
            let entry = entry.map_err(|source| {
                io_error("reading the live pack directory", &pack_directory, source)
            })?;
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
            let digest = name.trim_end_matches(".sqlite.zst");
            if plan.count >= admitted_rows {
                admit_pin_rows(store, PIN_ADMISSION_ROWS)?;
                admitted_rows += PIN_ADMISSION_ROWS;
            }
            insert
                .execute(params![pin_id.to_string(), digest, expires_at_ms])
                .map_err(pin_error)?;
            plan.count += 1;
            plan.bytes = plan
                .bytes
                .checked_add(metadata.len())
                .ok_or(DataRecoveryError::CapacityOverflow)?;
        }
        drop(insert);
        transaction.commit().map_err(pin_error)?;
        Ok((
            Self {
                store,
                pin_id,
                expires_at_ms,
            },
            plan,
        ))
    }

    /// This run's pinned digests after `after`, in order, one page at a
    /// time; each page is a short read of the live catalogue.
    fn page_after(&self, after: &str) -> Result<Vec<String>, DataRecoveryError> {
        let connection = self.store.open()?;
        let mut statement = connection
            .prepare(
                "SELECT sha256 FROM backup_pack_pins
                 WHERE pin_id = ?1 AND sha256 > ?2
                 ORDER BY sha256 LIMIT ?3",
            )
            .map_err(pin_error)?;
        let page = statement
            .query_map(
                params![self.pin_id.to_string(), after, PACK_PAGE_ROWS],
                |row| row.get::<_, String>(0),
            )
            .map_err(pin_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(pin_error)?;
        Ok(page)
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

/// The live catalogue's folder: where pin rows and their WAL frames land.
fn live_data_directory(store: &HubStore) -> Result<&Path, DataRecoveryError> {
    store
        .database_path()
        .parent()
        .ok_or_else(|| invalid("live Hub catalogue has no data-directory parent"))
}

/// Admit `rows` pin rows on the live data volume, keeping the fixed reserve.
fn admit_pin_rows(store: &HubStore, rows: u64) -> Result<(), DataRecoveryError> {
    let live = live_data_directory(store)?;
    let required = rows
        .checked_mul(PIN_ROW_BYTES)
        .ok_or(DataRecoveryError::CapacityOverflow)
        .and_then(|bytes| checked_capacity_add(bytes, COPY_CAPACITY_FIXED_HEADROOM_BYTES))?;
    admit_known_capacity(live, required, available_bytes(live)?)
}

/// The live packs, counted one directory entry at a time: their number and
/// bytes, before the gate, for admission.
fn count_live_packs(store: &HubStore) -> Result<(u64, u64), DataRecoveryError> {
    let directory = store.packs_dir().join("sha256");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(source) => return Err(io_error("reading the live pack directory", &directory, source)),
    };
    let (mut count, mut bytes) = (0_u64, 0_u64);
    for entry in entries {
        let entry =
            entry.map_err(|source| io_error("reading the live pack directory", &directory, source))?;
        if !entry.file_name().to_str().is_some_and(canonical_pack_filename) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        count += 1;
        bytes = checked_capacity_add(bytes, metadata.len())?;
    }
    Ok((count, bytes))
}

/// An upper bound on an archive's bytes: the container header, the manifest
/// record, each member record's header and bytes, the end tag and trailer,
/// and one authentication tag per chunk (REV-CX-M06-T10 finding 2).
fn archive_bytes_bound(
    members: u64,
    payload_bytes: u64,
    manifest_bytes: u64,
    chunk_bytes: usize,
) -> Result<u64, DataRecoveryError> {
    let overflow = || DataRecoveryError::CapacityOverflow;
    let records = members
        .checked_mul(MEMBER_RECORD_OVERHEAD_BYTES)
        .ok_or_else(overflow)?;
    let plain = (1 + 2 + MANIFEST_RECORD.len() as u64 + 8)
        .checked_add(manifest_bytes)
        .and_then(|total| total.checked_add(records))
        .and_then(|total| total.checked_add(payload_bytes))
        .and_then(|total| total.checked_add(1 + 32))
        .ok_or_else(overflow)?;
    let chunk = u64::try_from(chunk_bytes.max(1)).map_err(|_| overflow())?;
    (plain / chunk + 1)
        .checked_mul(TAG_BYTES as u64)
        .and_then(|tags| tags.checked_add(plain))
        .and_then(|total| total.checked_add(HEADER_BYTES as u64))
        .ok_or_else(overflow)
}

/// Admit an archive of at most `bound` bytes into the destination.
fn admit_archive(destination: &Path, bound: u64) -> Result<(), DataRecoveryError> {
    admit_known_capacity(
        destination,
        staging_required_bytes(bound)?,
        available_bytes(destination)?,
    )
}

/// The packs that live backups have pinned, for GC. An expired pin protects
/// nothing. A catalogue the current schema migration has not reached has no
/// pin table, and no backup can have pinned anything in it.
pub(crate) fn live_backup_pack_pins(
    connection: &Connection,
    now_ms: i64,
) -> Result<Vec<String>, StoreError> {
    if !pack_pin_table_present(connection).map_err(StoreError::Query)? {
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

/// The page size and current image size of a catalogue.
fn catalogue_geometry(source: &Connection) -> Result<(u64, u64), DataRecoveryError> {
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
    Ok((page_size, image_bytes))
}

/// The live catalogue's current size, to choose the copy before the gate.
fn live_catalogue_bytes(store: &HubStore) -> Result<u64, DataRecoveryError> {
    Ok(catalogue_geometry(&store.open()?)?.1)
}

/// Copy `source` into `destination` with the online backup API, a few pages
/// at a time; `too_large` stops a copy that grows past `limit_bytes`.
fn step_backup(
    source: &Connection,
    destination: &mut Connection,
    page_size: u64,
    limit_bytes: u64,
    too_large: impl Fn() -> DataRecoveryError,
) -> Result<(), DataRecoveryError> {
    let backup = Backup::new(source, destination).map_err(DataRecoveryError::CatalogueIdentity)?;
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
        if pages.saturating_mul(page_size) > limit_bytes {
            return Err(too_large());
        }
    }
    Ok(())
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
    let (page_size, image_bytes) = catalogue_geometry(&source)?;
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
    step_backup(
        &source,
        &mut memory,
        page_size,
        MAX_CATALOGUE_IMAGE_BYTES,
        too_large,
    )?;
    Ok(memory)
}

/// The size of the scratch volume for a catalogue of `image_bytes`: the copy
/// and its rebuilt sealed file side by side, room to grow before the gate, and
/// the copy's WAL.
fn scratch_capacity(image_bytes: u64) -> Result<u64, DataRecoveryError> {
    image_bytes
        .checked_add(image_bytes / 4)
        .and_then(|copy| copy.checked_mul(2))
        .and_then(|both| both.checked_add(256 * 1024 * 1024))
        .ok_or(DataRecoveryError::CapacityOverflow)
}

/// Open a file database on the scratch volume with bounded memory: a fixed
/// page cache and in-memory statement journals (small: the seal's deletes).
fn open_scratch_database(path: &Path) -> Result<Connection, DataRecoveryError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(DataRecoveryError::CatalogueIdentity)?;
    connection
        .execute_batch(&format!(
            "PRAGMA temp_store = MEMORY; PRAGMA cache_size = -{SCRATCH_CACHE_KIB};"
        ))
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    Ok(connection)
}

/// Copy the live catalogue with the online backup API into a file on the
/// encrypted scratch volume (M06-T07). Its journal and WAL live there too.
pub(super) fn copy_catalogue_into_scratch(
    store: &HubStore,
    scratch: &EncryptedScratch,
) -> Result<Connection, DataRecoveryError> {
    let source = store.open()?;
    let (page_size, _) = catalogue_geometry(&source)?;
    let mut copy = open_scratch_database(&scratch.path(SCRATCH_COPY))?;
    step_backup(&source, &mut copy, page_size, u64::MAX, || {
        DataRecoveryError::CapacityOverflow
    })?;
    Ok(copy)
}

/// The catalogue copy taken under the gate.
enum CatalogueCopy {
    Memory(Connection),
    /// The copy on the encrypted scratch volume, and the volume.
    Scratch(Connection, EncryptedScratch),
}

impl CatalogueCopy {
    fn connection(&self) -> &Connection {
        match self {
            Self::Memory(connection) | Self::Scratch(connection, _) => connection,
        }
    }
}

/// The sealed catalogue image and its archive member. The image streams with
/// its file-format version bytes set to the rollback journal (the copy of a
/// WAL catalogue carries WAL's), exactly as a sealed staged file would be.
struct SealedCatalogue {
    image: SealedImage,
    header: [u8; SQLITE_HEADER_BYTES],
    member: BackupMember,
}

enum SealedImage {
    /// The serialised in-memory copy.
    Memory(Data<'static>),
    /// The rebuilt sealed file on the scratch volume; the volume is detached
    /// when this is dropped, after the file is closed.
    Scratch {
        file: File,
        _scratch: EncryptedScratch,
    },
}

/// The normalised header and digest of a sealed image: `header` is its first
/// bytes, `rest` everything after them.
fn sealed_member(
    image: &[u8],
    rest: impl Read,
    size: u64,
) -> Result<([u8; SQLITE_HEADER_BYTES], BackupMember), DataRecoveryError> {
    if size <= SQLITE_HEADER_BYTES as u64
        || image.len() < SQLITE_HEADER_BYTES
        || !matches!((image[18], image[19]), (1 | 2, 1 | 2))
    {
        return Err(invalid(
            "the sealed catalogue copy is not a database image",
        ));
    }
    let mut header = [0_u8; SQLITE_HEADER_BYTES];
    header.copy_from_slice(&image[..SQLITE_HEADER_BYTES]);
    header[18] = 1;
    header[19] = 1;
    let mut digest = Sha256::new();
    digest.update(header);
    let mut rest = rest;
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    let mut total = SQLITE_HEADER_BYTES as u64;
    loop {
        let read = rest
            .read(&mut buffer)
            .map_err(|source| io_error("reading the sealed catalogue", Path::new(CATALOGUE_MEMBER), source))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        total += read as u64;
    }
    if total != size {
        return Err(invalid("the sealed catalogue copy changed while it was read"));
    }
    let member = BackupMember {
        path: CATALOGUE_MEMBER.to_owned(),
        size,
        mode: PRIVATE_FILE_MODE,
        sha256: hex::encode(digest.finalize()),
    };
    Ok((header, member))
}

/// Seal the scratch copy and rebuild it with `VACUUM INTO` beside it on the
/// scratch volume: a fresh file with no free page and no unused cell space
/// from the live catalogue, written without a temporary database or a sort
/// spill (indexes are created first and filled in order), so nothing of it
/// leaves the encrypted volume and memory stays at the page caches.
fn seal_scratch_catalogue(
    mut connection: Connection,
    scratch: EncryptedScratch,
    installation_id: Uuid,
) -> Result<SealedCatalogue, DataRecoveryError> {
    seal_backup_credentials_checked(&connection)?;
    seal_backup_credentials(&mut connection)?;
    seal_host_authority(&mut connection, current_epoch_ms()?)?;
    let sealed_path = scratch.path(SCRATCH_SEALED);
    let target = sealed_path
        .to_str()
        .ok_or_else(|| invalid("the backup scratch path is not UTF-8"))?;
    connection
        .execute("VACUUM INTO ?1", [target])
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    drop(connection);
    let sealed = open_scratch_database(&sealed_path)?;
    let free_pages: i64 = sealed
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if free_pages != 0 {
        return Err(invalid("the sealed catalogue still holds free pages"));
    }
    if catalogue_identity(&sealed, SCHEMA_VERSION)? != installation_id {
        return Err(invalid(
            "copied catalogue installation ID does not match the live source",
        ));
    }
    drop(sealed);
    let mut file = File::open(&sealed_path)
        .map_err(|source| io_error("opening the sealed catalogue", &sealed_path, source))?;
    let size = file
        .metadata()
        .map_err(|source| io_error("inspecting the sealed catalogue", &sealed_path, source))?
        .len();
    let mut first = [0_u8; SQLITE_HEADER_BYTES];
    file.read_exact(&mut first)
        .map_err(|source| io_error("reading the sealed catalogue", &sealed_path, source))?;
    let (header, member) = sealed_member(&first, &mut file, size)?;
    Ok(SealedCatalogue {
        image: SealedImage::Scratch {
            file,
            _scratch: scratch,
        },
        header,
        member,
    })
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
    let rest = image.get(SQLITE_HEADER_BYTES..).unwrap_or_default();
    let (header, member) = sealed_member(&image, rest, image.len() as u64)?;
    Ok(SealedCatalogue {
        image: SealedImage::Memory(image),
        header,
        member,
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

/// What the publication gate protects: the catalogue copy, the no-op bodies
/// it selects, and the pins on the packs (with their count and bytes).
struct Snapshot<'a> {
    catalogue: CatalogueCopy,
    noops: Vec<NoopBody>,
    packs: PackPlan,
    pins: PackPins<'a>,
}

fn take_snapshot(
    store: &HubStore,
    scratch: Option<EncryptedScratch>,
    admitted_pin_rows: u64,
) -> Result<Snapshot<'_>, DataRecoveryError> {
    let gate = store.begin_backup_snapshot()?;
    check_backup_admission(&gate)?;
    let catalogue = match scratch {
        Some(scratch) => {
            let copy = copy_catalogue_into_scratch(store, &scratch)?;
            #[cfg(test)]
            hook("scratch_copied", &scratch.image())?;
            CatalogueCopy::Scratch(copy, scratch)
        }
        None => CatalogueCopy::Memory(copy_catalogue_into_memory(store)?),
    };
    let noops = plan_noops(store, catalogue.connection())?;
    let (pins, packs) = PackPins::take(store, admitted_pin_rows)?;
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

/// Create one encrypted backup generation. Success is reported only once the
/// complete published file has authenticated as the object this run wrote,
/// on every destination; otherwise the run fails with `InvalidBackup` whose
/// message starts with `invalid_publication`, and nothing older is removed.
pub fn create_encrypted_backup(
    store: &HubStore,
    request: &EncryptedBackupRequest<'_>,
) -> Result<EncryptedBackupReport, DataRecoveryError> {
    let cancel = request.cancel.as_deref();
    cancelled(cancel)?;
    // The destination is opened as named (never through a link at its end)
    // and every check applies to the directory opened (M06-T14).
    let access = if request.private_destination {
        backup_admission::DestinationAccess::OwnerOnly
    } else {
        backup_admission::DestinationAccess::NoForeignWrite
    };
    let admitted: AdmittedDirectory = backup_admission::admit_destination(request.destination, access)?;
    let destination = admitted.path.clone();
    let live_data = store
        .database_path()
        .parent()
        .ok_or_else(|| invalid("live Hub catalogue has no data-directory parent"))?;
    backup_admission::refuse_inside(&admitted, live_data)?;
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
    // A catalogue above the in-memory cap goes through the encrypted scratch
    // volume, prepared before the gate (attaching takes a moment).
    let image_bytes = live_catalogue_bytes(store)?;
    let scratch = if image_bytes > scratch_threshold() {
        let capacity = scratch_capacity(image_bytes)?;
        admit_staging_capacity(&std::env::temp_dir(), capacity)?;
        Some(EncryptedScratch::create(capacity)?)
    } else {
        None
    };
    cancelled(cancel)?;
    // Before the gate and before any pin row or archive byte: the live
    // packs, counted one entry at a time, must fit the destination as an
    // archive (with the catalogue as it is now), and their pin rows the live
    // data volume (REV-CX-M06-T10 finding 2).
    let (live_packs, live_pack_bytes) = count_live_packs(store)?;
    admit_archive(
        &destination,
        archive_bytes_bound(
            live_packs.saturating_add(1),
            checked_capacity_add(live_pack_bytes, image_bytes)?,
            MANIFEST_ESTIMATE_BYTES,
            request.chunk_bytes,
        )?,
    )?;
    admit_pin_rows(store, live_packs)?;
    let Snapshot {
        catalogue,
        noops,
        packs,
        mut pins,
    } = take_snapshot(store, scratch, live_packs)?;
    let generation = Uuid::new_v4();
    let partial_name = format!("{PARTIAL_PREFIX}{generation}{PARTIAL_SUFFIX}");
    let partial = destination.join(&partial_name);
    let mut written: Option<retention::FileIdentity> = None;
    let run = (|| {
        cancelled(cancel)?;
        let catalogue = match catalogue {
            CatalogueCopy::Memory(connection) => {
                seal_catalogue(connection, source_installation_id)?
            }
            CatalogueCopy::Scratch(connection, scratch) => {
                seal_scratch_catalogue(connection, scratch, source_installation_id)?
            }
        };
        hook("catalogue_sealed", &destination)?;
        let noop_count = u64::try_from(noops.len()).map_err(|_| DataRecoveryError::CapacityOverflow)?;
        let member_count = packs
            .count
            .checked_add(noop_count)
            .and_then(|count| count.checked_add(1))
            .ok_or(DataRecoveryError::CapacityOverflow)?;
        let payload_bytes = noops
            .iter()
            .try_fold(catalogue.member.size, |total, noop| total.checked_add(noop.member.size))
            .and_then(|total| total.checked_add(packs.bytes))
            .ok_or(DataRecoveryError::CapacityOverflow)?;
        let created_at_ms = current_epoch_ms()?;
        let manifest = BackupManifest {
            kind: STREAMED_BACKUP_KIND.to_owned(),
            generation,
            created_at_ms,
            build: BUILD_VERSION.to_owned(),
            hub_schema: SCHEMA_VERSION,
            protocol: expected_protocol(),
            installation_id: source_installation_id,
            scope: BACKUP_SCOPE.to_owned(),
            excluded_host_state: excluded_host_state_strings(),
            members: Vec::new(),
            member_count: Some(member_count),
            payload_bytes: Some(payload_bytes),
        };
        validate_manifest(&manifest)?;
        let manifest_bytes = canonical_json(&manifest)?;
        // The whole archive, record headers and authentication tags
        // included, must fit before its partial file exists.
        admit_archive(
            &destination,
            archive_bytes_bound(
                member_count,
                payload_bytes,
                manifest_bytes.len() as u64,
                request.chunk_bytes,
            )?,
        )?;
        let final_name = canonical_archive_name(&manifest)?;
        let (file, identity) =
            retention::create_partial(&admitted, &partial_name)?;
        written = Some(identity);
        retention::record_run(store, generation, &admitted, identity)?;
        let (archive_bytes, archive_sha256) = write_partial(
            request,
            key,
            store,
            &partial,
            file,
            &manifest_bytes,
            catalogue,
            &noops,
            (member_count, payload_bytes),
            &mut pins,
        )?;
        hook("before_publish", &partial)?;
        cancelled(cancel)?;
        retention::publish(&admitted, &partial_name, &final_name)?;
        retention::forget_run(store, generation);
        Ok((
            final_name,
            manifest_bytes,
            usize::try_from(member_count).map_err(|_| DataRecoveryError::CapacityOverflow)?,
            payload_bytes,
            (archive_bytes, archive_sha256),
        ))
    })();
    let (final_name, manifest_bytes, member_count, payload_bytes, (archive_bytes, archive_sha256)) =
        match run {
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

    // Nothing is reported, and nothing older is removed, until the complete
    // published file has authenticated as the object this run wrote. The
    // read observes cancellation; a cancelled run reports no success and
    // removes nothing (REV-CX-M06-T11 finding 6).
    let newest = retention::confirm_publication(
        &admitted,
        &final_name,
        &retention::Written {
            identity: written.ok_or_else(|| invalid("the published generation has no identity"))?,
            manifest_bytes: &manifest_bytes,
            archive_bytes,
            archive_sha256,
        },
        request.password,
        cancel,
    )?;
    cancelled(cancel)?;
    let (rotated_generations, rotation_error) = match request.keep {
        Some(keep) => retention::rotate_after_publish(
            &admitted,
            keep.get(),
            &newest,
            request.password,
            source_installation_id,
            cancel,
        ),
        None => (0, None),
    };
    retention::confirm_still_published(&admitted, &newest, rotated_generations)?;
    drop(newest);
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
/// read while the gate still holds publication off, in path order. They are
/// one per vehicle; their bytes are bounded by [`MAX_HELD_NOOP_BYTES`].
fn plan_noops(store: &HubStore, catalogue: &Connection) -> Result<Vec<NoopBody>, DataRecoveryError> {
    let mut planned = Vec::new();
    let mut held = 0_u64;
    for (name, snapshot_id) in current_schema_22_noop_pairs_in(catalogue)? {
        let manifest = current_noop_manifest(catalogue, snapshot_id)?;
        let bytes = store
            .schema_22_noop_for_snapshot(manifest.vehicle_id, manifest.snapshot_id)?
            .ok_or(StoreError::Schema22NoOpNotFound)?;
        validate_schema_22_noop_bytes(&name, &manifest, &bytes)?;
        held += bytes.len() as u64;
        if held > MAX_HELD_NOOP_BYTES {
            return Err(invalid(
                "the current schema 2.2 no-op bodies exceed what a backup holds in memory",
            ));
        }
        planned.push(NoopBody {
            member: BackupMember {
                path: format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}"),
                size: bytes.len() as u64,
                mode: PRIVATE_FILE_MODE,
                sha256: hex::encode(Sha256::digest(&bytes)),
            },
            bytes,
        });
    }
    Ok(planned)
}

/// The running count and bytes of the members a stream has written.
#[derive(Default)]
struct Streamed {
    count: u64,
    bytes: u64,
}

impl Streamed {
    fn add(&mut self, member: &BackupMember) -> Result<(), DataRecoveryError> {
        self.count += 1;
        self.bytes = self
            .bytes
            .checked_add(member.size)
            .ok_or(DataRecoveryError::CapacityOverflow)?;
        Ok(())
    }
}

/// Stream the bundle in path order: the manifest, the catalogue, the no-op
/// bodies, then the pinned packs a page at a time. The written count and
/// bytes must equal the manifest's declared ones before the trailer goes.
#[allow(clippy::too_many_arguments)]
fn write_partial(
    request: &EncryptedBackupRequest<'_>,
    key: SealingKey,
    store: &HubStore,
    partial: &Path,
    file: File,
    manifest_bytes: &[u8],
    catalogue: SealedCatalogue,
    noops: &[NoopBody],
    (declared_count, declared_bytes): (u64, u64),
    pins: &mut PackPins<'_>,
) -> Result<(u64, [u8; 32]), DataRecoveryError> {
    let cancel = request.cancel.as_deref();
    let sink = Sink::new(file, partial.to_path_buf());
    let mut writer = ArchiveWriter::start(sink, partial.to_path_buf(), key)?;

    hook("record", partial)?;
    put_file_header(&mut writer, MANIFEST_RECORD, manifest_bytes.len() as u64)?;
    writer.write_plain(manifest_bytes)?;
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    let mut streamed = Streamed::default();

    cancelled(cancel)?;
    pins.renew_if_due()?;
    hook("record", partial)?;
    let SealedCatalogue {
        image,
        header,
        member: catalogue_member,
    } = catalogue;
    match image {
        SealedImage::Memory(image) => {
            stream_member(
                &mut writer,
                &catalogue_member,
                header.as_slice().chain(&image[SQLITE_HEADER_BYTES..]),
                &mut buffer,
                cancel,
            )?;
            // The copy's memory goes before the packs stream.
            drop(image);
        }
        SealedImage::Scratch { mut file, _scratch } => {
            file.seek(SeekFrom::Start(SQLITE_HEADER_BYTES as u64))
                .map_err(|source| {
                    io_error("reading the sealed catalogue", &catalogue_member.path, source)
                })?;
            stream_member(
                &mut writer,
                &catalogue_member,
                header.as_slice().chain(&mut file),
                &mut buffer,
                cancel,
            )?;
            // The scratch volume goes before the packs stream.
            drop(file);
            drop(_scratch);
        }
    }
    streamed.add(&catalogue_member)?;

    for noop in noops {
        cancelled(cancel)?;
        pins.renew_if_due()?;
        hook("record", partial)?;
        stream_member(
            &mut writer,
            &noop.member,
            noop.bytes.as_slice(),
            &mut buffer,
            cancel,
        )?;
        streamed.add(&noop.member)?;
    }

    let pack_directory = store.packs_dir().join("sha256");
    let mut after = String::new();
    loop {
        let page = pins.page_after(&after)?;
        if page.is_empty() {
            break;
        }
        for digest in page {
            cancelled(cancel)?;
            pins.renew_if_due()?;
            hook("record", partial)?;
            let name = format!("{digest}.sqlite.zst");
            let path = pack_directory.join(&name);
            let input = open_regular_nonblocking(&path, "opening backup source member")?;
            let metadata = input
                .metadata()
                .map_err(|source| io_error("inspecting backup source member", &path, source))?;
            if metadata.nlink() == 0 {
                return Err(invalid(format!(
                    "backup source member was removed before it was streamed: {PACK_DIRECTORY}/{name}"
                )));
            }
            let member = BackupMember {
                path: format!("{PACK_DIRECTORY}/{name}"),
                size: metadata.len(),
                mode: PRIVATE_FILE_MODE,
                sha256: digest,
            };
            stream_member(&mut writer, &member, input, &mut buffer, cancel)?;
            streamed.add(&member)?;
            after = member.sha256;
        }
    }
    if streamed.count != declared_count || streamed.bytes != declared_bytes {
        return Err(invalid(
            "the backup's members changed between the snapshot and the stream",
        ));
    }
    writer.write_plain(&[RECORD_END])?;
    let manifest_digest: [u8; 32] = Sha256::digest(manifest_bytes).into();
    writer.write_plain(&manifest_digest)?;
    let sink = writer.finish()?;
    sink.file
        .0
        .sync_all()
        .map_err(|source| map_write_error(partial, source))?;
    let size = sink
        .file
        .0
        .metadata()
        .map_err(|source| io_error("inspecting backup archive", partial, source))?
        .len();
    Ok((size, sink.digest.finalize().into()))
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

/// A v5 member record header: tag, path, size and the member's digest.
fn put_member_header(
    writer: &mut ArchiveWriter<Sink>,
    member: &BackupMember,
) -> Result<(), DataRecoveryError> {
    let path_length =
        u16::try_from(member.path.len()).map_err(|_| invalid("record path is too long"))?;
    let mut digest = [0_u8; 32];
    hex::decode_to_slice(&member.sha256, &mut digest)
        .map_err(|_| invalid("backup member digest is not SHA-256 hex"))?;
    writer.write_plain(&[RECORD_MEMBER])?;
    writer.write_plain(&path_length.to_be_bytes())?;
    writer.write_plain(member.path.as_bytes())?;
    writer.write_plain(&member.size.to_be_bytes())?;
    writer.write_plain(&digest)
}

fn stream_member(
    writer: &mut ArchiveWriter<Sink>,
    member: &BackupMember,
    mut input: impl Read,
    buffer: &mut [u8],
    cancel: Option<&AtomicBool>,
) -> Result<(), DataRecoveryError> {
    let source_path = Path::new(&member.path);
    put_member_header(writer, member)?;
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
    // v5 streams its members; a bundle written before it lists them (v4).
    if !matches!(
        backup_format(&manifest)?,
        BackupFormat::Streamed | BackupFormat::Current
    ) || manifest.scope != BACKUP_SCOPE
    {
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
    read_record_path_and_size(reader)
}

/// The path and size that follow a record tag.
fn read_record_path_and_size(
    reader: &mut ArchiveReader<File>,
) -> Result<(String, u64), DataRecoveryError> {
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

/// Read one member's bytes, verify its digest and hand them to `sink`.
fn read_member_body(
    reader: &mut ArchiveReader<File>,
    member: &BackupMember,
    sink: Option<File>,
    buffer: &mut [u8],
    cancel: Option<&AtomicBool>,
) -> Result<(), DataRecoveryError> {
    let mut sink = sink.map(VolumeFile);
    let mut digest = Sha256::new();
    let mut remaining = member.size;
    while remaining > 0 {
        cancelled(cancel)?;
        let take = usize::try_from(remaining).map_or(buffer.len(), |left| left.min(buffer.len()));
        reader.read_exact_plain(&mut buffer[..take])?;
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
    if let Some(VolumeFile(file)) = sink {
        file.sync_all().map_err(|source| {
            io_error(
                "syncing restored data member",
                Path::new(&member.path),
                source,
            )
        })?;
    }
    Ok(())
}

/// Read every member record, verify size and digest, and hand each member's
/// bytes to `sink_for` when it returns a file. Ends with the trailer, which
/// must bind the manifest, and then the end of the stream. Returns what the
/// stream established about the members.
fn stream_members(
    opened: &mut OpenedArchive,
    cancel: Option<&AtomicBool>,
    sink_for: impl FnMut(&BackupMember) -> Result<Option<File>, DataRecoveryError>,
) -> Result<StreamedMembers, DataRecoveryError> {
    let streamed = match backup_format(&opened.manifest)? {
        BackupFormat::Streamed => stream_streamed_members(opened, cancel, sink_for)?,
        _ => stream_listed_members(opened, cancel, sink_for)?,
    };
    let mut digest = [0_u8; 32];
    opened.reader.read_exact_plain(&mut digest)?;
    if digest != sha256_digest_bytes(&opened.manifest_bytes) {
        return Err(DataRecoveryError::CorruptArchive(
            "the archive trailer does not match its manifest".into(),
        ));
    }
    opened.reader.expect_end()?;
    Ok(streamed)
}

/// A bundle before v5: the records follow the manifest's (bounded) member
/// list one for one, then the end tag.
fn stream_listed_members(
    opened: &mut OpenedArchive,
    cancel: Option<&AtomicBool>,
    mut sink_for: impl FnMut(&BackupMember) -> Result<Option<File>, DataRecoveryError>,
) -> Result<StreamedMembers, DataRecoveryError> {
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    for member in &opened.manifest.members {
        cancelled(cancel)?;
        let (path, size) = read_file_header(&mut opened.reader)?;
        if path != member.path || size != member.size {
            return Err(DataRecoveryError::CorruptArchive(format!(
                "the archive record does not match its manifest entry: {}",
                member.path
            )));
        }
        let sink = sink_for(member)?;
        read_member_body(&mut opened.reader, member, sink, &mut buffer, cancel)?;
    }
    let mut tag = [0_u8; 1];
    opened.reader.read_exact_plain(&mut tag)?;
    if tag[0] != RECORD_END {
        return Err(DataRecoveryError::CorruptArchive(
            "the archive trailer does not match its manifest".into(),
        ));
    }
    let members = &opened.manifest.members;
    let catalogue = members
        .iter()
        .find(|member| member.path == CATALOGUE_MEMBER)
        .cloned()
        .ok_or_else(|| invalid("data-backup must contain exactly one Hub catalogue"))?;
    let noops = members
        .iter()
        .filter(|member| schema_22_noop_member_filename(&member.path).is_some())
        .count() as u64;
    Ok(StreamedMembers {
        catalogue,
        packs: members.len() as u64 - 1 - noops,
        noops,
    })
}

/// A v5 bundle: member records until the end tag, each checked as it
/// arrives, in strictly ascending path order, holding only the previous path;
/// the count and bytes must equal the manifest's declared ones.
fn stream_streamed_members(
    opened: &mut OpenedArchive,
    cancel: Option<&AtomicBool>,
    mut sink_for: impl FnMut(&BackupMember) -> Result<Option<File>, DataRecoveryError>,
) -> Result<StreamedMembers, DataRecoveryError> {
    let corrupt = |message: &str| DataRecoveryError::CorruptArchive(message.to_owned());
    let declared_count = opened
        .manifest
        .member_count
        .ok_or_else(|| corrupt("the archive manifest declares no member count"))?;
    let declared_bytes = opened
        .manifest
        .payload_bytes
        .ok_or_else(|| corrupt("the archive manifest declares no payload bytes"))?;
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    let mut previous: Option<String> = None;
    let mut catalogue: Option<BackupMember> = None;
    let (mut count, mut bytes, mut packs, mut noops) = (0_u64, 0_u64, 0_u64, 0_u64);
    loop {
        cancelled(cancel)?;
        let mut tag = [0_u8; 1];
        opened.reader.read_exact_plain(&mut tag)?;
        match tag[0] {
            RECORD_END => break,
            RECORD_MEMBER => {}
            _ => return Err(corrupt("unexpected record in the archive")),
        }
        let (path, size) = read_record_path_and_size(&mut opened.reader)?;
        let mut digest = [0_u8; 32];
        opened.reader.read_exact_plain(&mut digest)?;
        let member = BackupMember {
            path,
            size,
            mode: PRIVATE_FILE_MODE,
            sha256: hex::encode(digest),
        };
        if previous
            .as_deref()
            .is_some_and(|previous| previous >= member.path.as_str())
        {
            return Err(corrupt(
                "the archive members are not uniquely sorted by canonical path",
            ));
        }
        let kind = validate_member(&member).map_err(|error| corrupt(&error.to_string()))?;
        count += 1;
        bytes = bytes
            .checked_add(member.size)
            .ok_or_else(|| corrupt("the archive member byte total overflowed"))?;
        if count > declared_count || bytes > declared_bytes {
            return Err(corrupt(
                "the archive holds more members or bytes than its manifest declares",
            ));
        }
        match kind {
            MemberKind::Catalogue => catalogue = Some(member.clone()),
            MemberKind::Noop => noops += 1,
            MemberKind::Pack => packs += 1,
        }
        let sink = sink_for(&member)?;
        read_member_body(&mut opened.reader, &member, sink, &mut buffer, cancel)?;
        previous = Some(member.path);
    }
    if count != declared_count || bytes != declared_bytes {
        return Err(corrupt(
            "the archive holds fewer members or bytes than its manifest declares",
        ));
    }
    let catalogue =
        catalogue.ok_or_else(|| corrupt("the archive must contain exactly one Hub catalogue"))?;
    Ok(StreamedMembers {
        catalogue,
        packs,
        noops,
    })
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
    stream_members(&mut opened, None, |_| Ok(None))?;
    report("data_backup_verified", archive, &opened.manifest)
}

/// Restore an archive into a new directory. The wrong password is refused
/// before anything is written; any damage found while streaming discards the
/// staged tree, so nothing partial is ever published.
///
/// When plaintext first touches disk (M06-T11): after the header has passed
/// the password check, the first chunk has authenticated and yielded the
/// manifest (canonical, valid, its generation matching the file name),
/// capacity is admitted and the locked staging directory exists. Then the
/// first member's record (path, size, digest) is read from authenticated
/// plaintext and its file created in the staging directory; its bytes are
/// written chunk by chunk, each only after that chunk's AES-256-GCM tag has
/// verified under the nonce for its position (and the final-chunk flag). At
/// that moment the member's digest, the later chunks, the final chunk, the
/// declared totals and the trailer are not yet checked.
///
/// What guarantees nothing unauthenticated is adopted: the staged tree is
/// published (one no-replace rename onto the target) only after the whole
/// stream has authenticated, every member digest, the declared totals, the
/// trailer binding the manifest and the end of the stream, and after the tree
/// and installation checks. Any failure before that removes the staging
/// directory; an interrupted restore's staging directory, no longer locked by
/// any process, is removed when the next restore into the same parent starts.
///
/// Binding and space (M06-T15): the parent is admitted and held, the staging
/// is made and written through held descriptors and its path is checked
/// against them before every phase that uses it ([`restore_staging`]), and
/// the parent must
/// hold the members with their allocation and metadata overhead; before each
/// member the space the rest of the bundle needs, plus a reserve, must still
/// be free.
pub fn restore_encrypted_backup(
    archive: &Path,
    password: &str,
    destination: &Path,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    let (destination, parent_path) = prepare_new_destination(destination)?;
    let parent = backup_admission::admit_destination(
        &parent_path,
        backup_admission::DestinationAccess::NoForeignWrite,
    )
    .map_err(|error| match error {
        DataRecoveryError::InvalidBackup(_) => invalid(
            "the restore target's folder must be owned by the Hub's user and not writable by \
             other accounts, by its mode or by an access control list",
        ),
        other => other,
    })?;
    restore_staging::clear_interrupted_restores(&parent)?;
    let mut opened = open_archive(archive, password)?;
    let (member_count, payload_bytes) = manifest_totals(&opened.manifest)?;
    let (member_count, payload_bytes) = (member_count as u64, payload_bytes);
    let block = allocation_block_bytes(&parent.path)?;
    admit_staging_capacity(
        &parent.path,
        member_storage_bytes(payload_bytes, member_count, block)?,
    )?;
    let space = parent.path.clone();
    let mut staging = restore_staging::HeldStaging::create(parent)?;
    let root = staging.root().to_path_buf();
    let (mut created, mut created_bytes) = (0_u64, 0_u64);
    let streamed = stream_members(&mut opened, None, |member| {
        hook("restore_member", &root)?;
        // The rest of the bundle, with its overhead, plus the reserve must
        // still fit before another file is made.
        let remaining = member_storage_bytes(
            payload_bytes.saturating_sub(created_bytes),
            member_count.saturating_sub(created),
            block,
        )
        .and_then(|bytes| checked_capacity_add(bytes, COPY_CAPACITY_FIXED_HEADROOM_BYTES))?;
        admit_known_capacity(&space, remaining, available_bytes(&space)?)?;
        let file = staging.create_member(member)?;
        created += 1;
        created_bytes = created_bytes.saturating_add(member.size);
        Ok(Some(file))
    });
    #[cfg(test)]
    if streamed.is_err() && test_hooks::CRASHED.with(|flag| flag.replace(false)) {
        // A killed process removes nothing, and its lock goes with it.
        staging.leave_as_killed();
        return Err(streamed.err().expect("an error"));
    }
    let streamed = streamed?;
    // From here the tree is reached by its path: it must still be the one
    // the stream wrote through held descriptors.
    staging.check_bound()?;

    let members = match backup_format(&opened.manifest)? {
        BackupFormat::Streamed => RestoredMembers::Streamed(&streamed),
        _ => RestoredMembers::Listed(&opened.manifest.members),
    };
    validate_restored_data_tree(&root, members, false)?;
    if immutable_database_identity_at_schema(&root, opened.manifest.hub_schema)?
        != opened.manifest.installation_id
    {
        return Err(invalid(
            "manifest installation ID does not match the restored catalogue",
        ));
    }
    finalize_restore(staging, &destination, &opened.manifest, members)
}

/// The authenticated manifest of an archive, for tests.
#[cfg(test)]
pub(super) fn archive_manifest(
    archive: &Path,
    password: &str,
) -> Result<BackupManifest, DataRecoveryError> {
    let file = open_regular_nonblocking(archive, "opening backup archive")?;
    Ok(open_archive_file(file, archive, password)?.manifest)
}

/// Write an archive by hand into `directory` under the name its manifest
/// implies, for tests: `streamed` false writes the layout of bundles before
/// v5 (file records, as the manifest lists them), true writes v5 member
/// records in the order given, faults included. The trailer binds the
/// manifest either way.
#[cfg(test)]
pub(super) fn write_test_archive(
    directory: &Path,
    password: &str,
    kdf: KdfParams,
    manifest_bytes: &[u8],
    members: &[(&str, &[u8])],
    streamed: bool,
) -> Result<PathBuf, DataRecoveryError> {
    let manifest: BackupManifest = parse_canonical_json(manifest_bytes)?;
    let path = directory.join(canonical_archive_name(&manifest)?);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(&path)
        .map_err(|source| io_error("creating test archive", &path, source))?;
    let key = SealingKey::derive(password, kdf, 4096)?;
    let mut writer = ArchiveWriter::start(Sink::new(file, path.clone()), path.clone(), key)?;
    put_file_header(&mut writer, MANIFEST_RECORD, manifest_bytes.len() as u64)?;
    writer.write_plain(manifest_bytes)?;
    for (member_path, bytes) in members {
        if streamed {
            put_member_header(
                &mut writer,
                &BackupMember {
                    path: (*member_path).to_owned(),
                    size: bytes.len() as u64,
                    mode: PRIVATE_FILE_MODE,
                    sha256: hex::encode(Sha256::digest(bytes)),
                },
            )?;
        } else {
            put_file_header(&mut writer, member_path, bytes.len() as u64)?;
        }
        writer.write_plain(bytes)?;
    }
    writer.write_plain(&[RECORD_END])?;
    writer.write_plain(&sha256_digest_bytes(manifest_bytes))?;
    let sink = writer.finish()?;
    sink.file
        .0
        .sync_all()
        .map_err(|source| io_error("syncing test archive", &path, source))?;
    Ok(path)
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
        pub(super) static SCRATCH_ABOVE: Cell<Option<u64>> = const { Cell::new(None) };
    }

    /// Take the encrypted scratch path for catalogues above `bytes` on this
    /// thread (`None` restores the production threshold).
    pub fn force_scratch_above(bytes: Option<u64>) {
        SCRATCH_ABOVE.with(|threshold| threshold.set(bytes));
    }

    pub fn set_hook(hook: impl FnMut(&str, &Path) -> Action + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub fn clear_hook() {
        HOOK.with(|slot| *slot.borrow_mut() = None);
        CRASHED.with(|flag| flag.set(false));
        SCRATCH_ABOVE.with(|threshold| threshold.set(None));
    }

    pub(super) fn call(stage: &str, path: &Path) -> Action {
        HOOK.with(|slot| match slot.borrow_mut().as_mut() {
            Some(hook) => hook(stage, path),
            None => Action::Continue,
        })
    }
}
