// SPDX-License-Identifier: AGPL-3.0-only

//! Retention, stale-partial cleanup and removal of encrypted-backup
//! generations (M06-T02).
//!
//! The rule behind every step: when in doubt, keep every existing generation
//! and report; never delete.
//!
//! - Every operation is relative to the destination descriptor admitted for
//!   the run, never to a path looked up again.
//! - On every destination, before the run reports anything, the complete
//!   published file is authenticated (every chunk, every member digest, the
//!   trailer and the end of the stream) and established as the object the
//!   run wrote ([`confirm_publication`]). Otherwise the run fails with an
//!   invalid publication: an error of its own, never a success with a
//!   rotation warning, so no caller records it as the last good generation.
//! - The confirmed generation stays open and counts towards retention.
//!   Before every removal it and every other kept generation must still be
//!   unchanged and still under its own name; otherwise nothing more is
//!   removed, and the run fails when the new generation itself changed.
//! - An older generation counts, or is removed, only after full
//!   authentication. A damaged one is neither counted nor removed.
//! - A removal first claims the candidate by an atomic no-replace rename into
//!   a protected directory inside the destination (the Hub's own, mode
//!   `0700`, no ACL), then authenticates what it claimed in full, through the
//!   descriptor, as the generation that was ranked; only then is that name
//!   unlinked. Anything else is renamed back. On a volume whose ownership is
//!   not enforced no directory can be protected, so nothing is deleted there.
//! - A partial file is removed only when the live catalogue records it as an
//!   abandoned run of this Hub into this destination, by generation, device
//!   and inode (trusted state no destination writer can forge). A complete
//!   generation found under a partial name is kept, and published under its
//!   own name when it was this Hub's abandoned run.

use std::{
    fs::{self, File},
    io::{self, Read},
    os::{
        fd::OwnedFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Path,
    sync::atomic::AtomicBool,
};

use rusqlite::params;
use rustix::{
    fs::{AtFlags, Dir, Mode, OFlags, RenameFlags, fsync, mkdirat, openat, renameat_with, unlinkat},
    io::Errno,
};
use uuid::Uuid;

use sha2::{Digest, Sha256};

use super::{
    ARCHIVE_PREFIX, ARCHIVE_SUFFIX, BackupManifest, DataRecoveryError, HubStore, PARTIAL_PREFIX,
    PARTIAL_SUFFIX, STREAM_BUFFER_BYTES, backup_admission::{self, AdmittedDirectory, VolumeTrust},
    canonical_archive_name, cancelled, current_epoch_ms, hook, invalid, io_error,
    open_archive_file, pin_error, stream_members,
};

/// The protected directory a removal claims its candidate into.
pub(super) const CLAIMS_DIRECTORY: &str = ".teslatlas-hub-removals";

/// Backup-owned live state: one row per run that has created its partial file
/// and not yet published or discarded it. The schema migration creates the
/// table, like the pin table; the seal empties it in every copied or restored
/// catalogue (`clear_pack_pins`), so no row is ever exported.
const RUN_TABLE: &str = "backup_runs";

/// Whether `connection`'s catalogue has the run table (a catalogue the
/// current schema migration has not reached has none).
pub(super) fn run_table_present(connection: &rusqlite::Connection) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [RUN_TABLE],
        |row| row.get(0),
    )
}

/// Reported when the volume cannot protect a removal.
const UNPROTECTED: &str = "older generations were kept: this destination does not enforce \
                           ownership (a network share or a volume that ignores ownership), so \
                           a removal cannot be protected";

/// A file's identity on its volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    pub(super) fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// Everything a change to a file's content, size, links or attributes moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    identity: FileIdentity,
    len: u64,
    links: u64,
    mode: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            identity: FileIdentity::of(metadata),
            len: metadata.len(),
            links: metadata.nlink(),
            mode: metadata.mode(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

/// A fully authenticated generation, held open under its name.
pub(super) struct Held {
    file: File,
    name: String,
    stamp: Stamp,
}

impl Held {
    /// Unchanged since it authenticated, and still the file under its name.
    fn intact(&self, directory: &OwnedFd) -> bool {
        let Ok(now) = self.file.metadata() else {
            return false;
        };
        if Stamp::of(&now) != self.stamp {
            return false;
        }
        open_entry(directory, &self.name)
            .and_then(|file| file.metadata())
            .is_ok_and(|named| FileIdentity::of(&named) == self.stamp.identity)
    }
}

/// Open a directory entry without following a link and without blocking.
fn open_entry(directory: &OwnedFd, name: &str) -> io::Result<File> {
    openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

/// Read a whole archive through `file`: the header, every chunk, every
/// member digest, the trailer and the end of the stream. Returns its manifest
/// and the manifest's exact bytes. `cancel` is observed between members and
/// between chunks; the test seam `stage` runs before each member.
fn authenticate_whole(
    file: File,
    path: &Path,
    password: &str,
    cancel: Option<&AtomicBool>,
    stage: &'static str,
) -> Result<(BackupManifest, Vec<u8>), DataRecoveryError> {
    let mut opened = open_archive_file(file, path, password)?;
    stream_members(&mut opened, cancel, |_| {
        hook(stage, path)?;
        Ok(None)
    })?;
    Ok((opened.manifest, opened.manifest_bytes))
}

/// The length and SHA-256 of the raw bytes now under `name` in `directory`,
/// read whole through a fresh open of the name.
fn raw_digest_of_name(
    directory: &OwnedFd,
    path: &Path,
    name: &str,
    cancel: Option<&AtomicBool>,
) -> Result<(u64, [u8; 32]), DataRecoveryError> {
    let mut file = open_entry(directory, name)
        .map_err(|source| io_error("opening backup archive", path, source))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    let mut length = 0_u64;
    loop {
        cancelled(cancel)?;
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(io_error("reading backup archive", path, source)),
        };
        digest.update(&buffer[..read]);
        length += read as u64;
    }
    Ok((length, digest.finalize().into()))
}

/// Open `name` in `directory`, admit it for `trust`, authenticate it in full
/// and keep it open. It must not change while it is read.
fn hold_authenticated(
    directory: &OwnedFd,
    directory_path: &Path,
    name: &str,
    trust: VolumeTrust,
    password: &str,
    cancel: Option<&AtomicBool>,
    stage: &'static str,
) -> Result<(Held, BackupManifest, Vec<u8>), DataRecoveryError> {
    let path = directory_path.join(name);
    let file =
        open_entry(directory, name).map_err(|source| io_error("opening backup archive", &path, source))?;
    let before = backup_admission::admit_archive_file(&file, &path, trust)?;
    let reader = file
        .try_clone()
        .map_err(|source| io_error("opening backup archive", &path, source))?;
    let (manifest, manifest_bytes) = authenticate_whole(reader, &path, password, cancel, stage)?;
    let after = file
        .metadata()
        .map_err(|source| io_error("inspecting backup archive", &path, source))?;
    // Where ownership is not enforced, times and numbers can be synthesised
    // or refreshed by the server, so only the length is compared there; the
    // content is bound by the caller (see `confirm_publication`).
    let changed = match trust {
        VolumeTrust::Posix => Stamp::of(&after) != Stamp::of(&before),
        _ => after.len() != before.len(),
    };
    if changed {
        return Err(invalid(format!(
            "{} changed while it was authenticated",
            path.display()
        )));
    }
    Ok((
        Held {
            file,
            name: name.to_owned(),
            stamp: Stamp::of(&after),
        },
        manifest,
        manifest_bytes,
    ))
}

/// The protected directory, held open.
struct Claims {
    fd: OwnedFd,
    path: std::path::PathBuf,
}

impl Claims {
    /// Open the protected directory, making it when absent, and admit it.
    fn open(destination: &AdmittedDirectory) -> Result<Self, DataRecoveryError> {
        let path = destination.path.join(CLAIMS_DIRECTORY);
        match mkdirat(&destination.fd, CLAIMS_DIRECTORY, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => {
                return Err(io_error("creating the removal directory", &path, error.into()));
            }
        }
        Self::existing(destination)?
            .ok_or_else(|| invalid("the removal directory vanished as it was made"))
    }

    /// The protected directory when it exists, admitted.
    fn existing(destination: &AdmittedDirectory) -> Result<Option<Self>, DataRecoveryError> {
        let path = destination.path.join(CLAIMS_DIRECTORY);
        let fd = match openat(
            &destination.fd,
            CLAIMS_DIRECTORY,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => {
                return Err(io_error("opening the removal directory", &path, error.into()));
            }
        };
        backup_admission::admit_protected_directory(&fd, &path)?;
        Ok(Some(Self { fd, path }))
    }

    /// Move `name` from the destination into this directory, atomically and
    /// without replacing anything.
    fn claim(&self, destination: &AdmittedDirectory, name: &str) -> io::Result<()> {
        renameat_with(&destination.fd, name, &self.fd, name, RenameFlags::NOREPLACE)
            .map_err(io::Error::from)
    }

    /// Return a claimed entry under `to` in the destination, without
    /// replacing anything. When that fails it stays here and the next run
    /// tries again.
    fn put_back(&self, destination: &AdmittedDirectory, name: &str, to: &str) -> bool {
        renameat_with(&self.fd, name, &destination.fd, to, RenameFlags::NOREPLACE).is_ok()
    }

    /// Return whatever an interrupted run left claimed.
    fn return_leftovers(&self, destination: &AdmittedDirectory) -> Vec<String> {
        let mut stuck = Vec::new();
        let Ok(entries) = Dir::read_from(&self.fd) else {
            return vec![CLAIMS_DIRECTORY.to_owned()];
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            if !self.put_back(destination, name, name) {
                stuck.push(name.to_owned());
            }
        }
        stuck
    }

    fn sync(&self) {
        let _ = fsync(&self.fd);
    }
}

/// Remove the protected directory when it is empty.
fn retire_claims(destination: &AdmittedDirectory, claims: Option<Claims>) {
    if let Some(claims) = claims {
        claims.sync();
        drop(claims);
        let _ = unlinkat(&destination.fd, CLAIMS_DIRECTORY, AtFlags::REMOVEDIR);
    }
}

/// Create the run's partial file, exclusively, in the held destination.
pub(super) fn create_partial(
    destination: &AdmittedDirectory,
    name: &str,
) -> Result<(File, FileIdentity), DataRecoveryError> {
    let path = destination.path.join(name);
    let unavailable = |source: io::Error| DataRecoveryError::DestinationUnavailable {
        path: path.clone(),
        source,
    };
    let file = openat(
        &destination.fd,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
    .map_err(|error| unavailable(error.into()))?;
    let metadata = file.metadata().map_err(unavailable)?;
    Ok((file, FileIdentity::of(&metadata)))
}

/// Publish the partial under its final name, never replacing an entry.
pub(super) fn publish(
    destination: &AdmittedDirectory,
    partial: &str,
    final_name: &str,
) -> Result<(), DataRecoveryError> {
    renameat_with(
        &destination.fd,
        partial,
        &destination.fd,
        final_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        io_error(
            "atomically publishing a backup generation without replacement",
            destination.path.join(final_name),
            error.into(),
        )
    })?;
    fsync(&destination.fd).map_err(|error| {
        io_error(
            "syncing the backup destination",
            &destination.path,
            error.into(),
        )
    })
}

/// The prefix of the error that refuses a publication: the file under the
/// generation's final name is not, complete and authenticated, the object
/// the run wrote.
pub(super) const INVALID_PUBLICATION: &str = "invalid_publication";

fn invalid_publication(path: &Path, reason: impl std::fmt::Display) -> DataRecoveryError {
    invalid_publication_after(path, reason, 0)
}

/// An invalid publication found after `removed` older generations went: the
/// report says exactly how many.
fn invalid_publication_after(
    path: &Path,
    reason: impl std::fmt::Display,
    removed: usize,
) -> DataRecoveryError {
    let older = match removed {
        0 => "nothing older was removed".to_owned(),
        1 => "1 older generation had already been removed".to_owned(),
        count => format!("{count} older generations had already been removed"),
    };
    invalid(format!(
        "{INVALID_PUBLICATION}: {} {reason}; the run did not produce a good generation and \
         {older}",
        path.display()
    ))
}

/// What the run wrote, to be found again under the generation's final name.
pub(super) struct Written<'a> {
    pub(super) identity: FileIdentity,
    pub(super) manifest_bytes: &'a [u8],
    pub(super) archive_bytes: u64,
    /// SHA-256 of every byte the run wrote to the file.
    pub(super) archive_sha256: [u8; 32],
}

/// After publication and before the run reports anything, on every
/// destination: open the final name through the held destination, admit it,
/// read it whole (header, every chunk, every member digest, the trailer and
/// the end of the stream) and establish that it is the object this run
/// wrote. Everywhere that means the exact manifest bytes the run wrote (its
/// generation is unique to the run and sealed under the password-derived
/// key) and the exact length; on a volume that enforces ownership also the
/// same device and inode, unchanged while it was read. A volume that does
/// not enforce ownership can synthesise device and inode numbers and change
/// them on a rename, so there the authenticated content is the identity, as
/// decided: such destinations rely on the authenticated encryption.
///
/// On such a volume the name is bound to the content after the read
/// instead (REV-CX-M06-T11 finding 1): once the authenticated read is done,
/// the final name is opened again and its whole raw content must have the
/// length and SHA-256 of what the run wrote. A file replaced, or changed in
/// place, after the reader opened it is therefore refused, whatever its
/// length or numbers.
///
/// Returns the generation held open, for rotation. A file that does not
/// authenticate, or is not the run's, is an invalid publication; an I/O
/// failure while reading stays an I/O failure; a cancelled read is
/// cancelled. None is a success.
pub(super) fn confirm_publication(
    destination: &AdmittedDirectory,
    final_name: &str,
    written: &Written<'_>,
    password: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Held, DataRecoveryError> {
    let path = destination.path.join(final_name);
    let (held, _, manifest_bytes) = match hold_authenticated(
        &destination.fd,
        &destination.path,
        final_name,
        destination.trust,
        password,
        cancel,
        "confirm_member",
    ) {
        Ok(found) => found,
        Err(
            error @ (DataRecoveryError::Io { .. }
            | DataRecoveryError::DestinationUnavailable { .. }
            | DataRecoveryError::Cancelled),
        ) => {
            return Err(error);
        }
        Err(error) => {
            return Err(invalid_publication(
                &path,
                format_args!("did not authenticate in full: {error}"),
            ));
        }
    };
    if manifest_bytes != written.manifest_bytes {
        return Err(invalid_publication(&path, "is not the generation this run wrote"));
    }
    if held.stamp.len != written.archive_bytes {
        return Err(invalid_publication(&path, "does not have the length this run wrote"));
    }
    if destination.trust == VolumeTrust::Posix {
        if held.stamp.identity != written.identity {
            return Err(invalid_publication(&path, "is not the object this run wrote"));
        }
        return Ok(held);
    }
    hook("confirm_rebind", &path)?;
    let (length, digest) = raw_digest_of_name(&destination.fd, &path, final_name, cancel)?;
    if length != written.archive_bytes || digest != written.archive_sha256 {
        return Err(invalid_publication(
            &path,
            "is not, byte for byte, the file this run wrote (it changed after it was opened)",
        ));
    }
    Ok(held)
}

/// The last step before success: on a volume that enforces ownership the
/// confirmed generation must still be unchanged and under its own name.
/// `removed` older generations went before this check; the error says so.
pub(super) fn confirm_still_published(
    destination: &AdmittedDirectory,
    newest: &Held,
    removed: usize,
) -> Result<(), DataRecoveryError> {
    if destination.trust == VolumeTrust::Posix && !newest.intact(&destination.fd) {
        return Err(invalid_publication_after(
            &destination.path.join(&newest.name),
            "changed after it was authenticated",
            removed,
        ));
    }
    Ok(())
}

/// Record the run's partial as live state before anything is written to it.
pub(super) fn record_run(
    store: &HubStore,
    generation: Uuid,
    destination: &AdmittedDirectory,
    partial: FileIdentity,
) -> Result<(), DataRecoveryError> {
    let connection = store.open()?;
    connection
        .execute(
            "INSERT INTO backup_runs(generation, destination, file_device, file_inode,
                 started_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                generation.to_string(),
                destination.path.as_os_str().as_bytes(),
                partial.device.to_string(),
                partial.inode.to_string(),
                current_epoch_ms()?,
            ],
        )
        .map_err(pin_error)?;
    Ok(())
}

/// The run's partial is published or gone: its row goes. A row that cannot
/// be removed only keeps a partial that no longer exists on record.
pub(super) fn forget_run(store: &HubStore, generation: Uuid) {
    if let Ok(connection) = store.open() {
        let _ = connection.execute(
            "DELETE FROM backup_runs WHERE generation = ?1",
            [generation.to_string()],
        );
    }
}

/// The recorded runs into `destination`: generation and partial identity.
fn recorded_runs(
    store: &HubStore,
    destination: &AdmittedDirectory,
) -> Result<Vec<(Uuid, FileIdentity)>, DataRecoveryError> {
    let connection = store.open()?;
    if !run_table_present(&connection).map_err(pin_error)? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT generation, file_device, file_inode FROM backup_runs
             WHERE destination = ?1",
        )
        .map_err(pin_error)?;
    let rows = statement
        .query_map([destination.path.as_os_str().as_bytes()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(pin_error)?;
    let mut runs = Vec::new();
    for row in rows {
        let (generation, device, inode) = row.map_err(pin_error)?;
        // A row this module did not write is not trusted for anything.
        if let (Ok(generation), Ok(device), Ok(inode)) = (
            Uuid::parse_str(&generation),
            device.parse::<u64>(),
            inode.parse::<u64>(),
        ) {
            runs.push((generation, FileIdentity { device, inode }));
        }
    }
    Ok(runs)
}

fn partial_name(generation: Uuid) -> String {
    format!("{PARTIAL_PREFIX}{generation}{PARTIAL_SUFFIX}")
}

/// Discard the partial of a run that failed now: only the object this run
/// created, claimed into the protected directory first. Where no directory
/// can be protected, the partial and its row stay.
pub(super) fn discard_failed_partial(
    store: &HubStore,
    destination: &AdmittedDirectory,
    generation: Uuid,
    written: FileIdentity,
) {
    if destination.trust != VolumeTrust::Posix {
        return;
    }
    let Ok(claims) = Claims::open(destination) else {
        return;
    };
    let name = partial_name(generation);
    if remove_own_partial(destination, &claims, &name, written) {
        forget_run(store, generation);
    }
    retire_claims(destination, Some(claims));
}

/// Claim `name`, and unlink it only when it is the run's own object.
fn remove_own_partial(
    destination: &AdmittedDirectory,
    claims: &Claims,
    name: &str,
    written: FileIdentity,
) -> bool {
    if claims.claim(destination, name).is_err() {
        return false;
    }
    let ours = open_entry(&claims.fd, name)
        .and_then(|file| file.metadata())
        .is_ok_and(|metadata| metadata.file_type().is_file() && FileIdentity::of(&metadata) == written);
    if ours && unlinkat(&claims.fd, name, AtFlags::empty()).is_ok() {
        return true;
    }
    claims.put_back(destination, name, name);
    false
}

/// Before a run: clear the partials of this Hub's abandoned runs into this
/// destination. Only a partial the live catalogue records, by generation,
/// device and inode, is touched; a file under a partial name that no row
/// names (a renamed generation, another Hub's or a stranger's file) stays. A
/// recorded partial that turns out to be a complete generation of this Hub is
/// published under its own name instead of being removed; one that cannot be
/// read with this password stays.
pub(super) fn clear_abandoned_runs(
    store: &HubStore,
    destination: &AdmittedDirectory,
    password: &str,
    installation_id: Uuid,
) -> Result<(), DataRecoveryError> {
    let runs = recorded_runs(store, destination)?;
    if runs.is_empty() {
        return Ok(());
    }
    let mut claims: Option<Claims> = None;
    for (generation, recorded) in runs {
        let name = partial_name(generation);
        let current = match open_entry(&destination.fd, &name) {
            Ok(file) => file.metadata(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                forget_run(store, generation);
                continue;
            }
            Err(error) => Err(error),
        };
        match current {
            Ok(metadata)
                if metadata.file_type().is_file() && FileIdentity::of(&metadata) == recorded => {}
            Ok(_) => {
                // The run's object is gone; what has its name is not ours.
                forget_run(store, generation);
                continue;
            }
            Err(_) => continue,
        }
        if destination.trust != VolumeTrust::Posix {
            continue;
        }
        if claims.is_none() {
            match Claims::open(destination) {
                Ok(opened) => claims = Some(opened),
                Err(_) => return Ok(()),
            }
        }
        let Some(held) = claims.as_ref() else {
            return Ok(());
        };
        if let Err(error) = hook("partial_claim", &destination.path.join(&name)) {
            retire_claims(destination, claims);
            return Err(error);
        }
        if held.claim(destination, &name).is_err() {
            continue;
        }
        settle_claimed_partial(
            store,
            destination,
            held,
            &name,
            generation,
            recorded,
            password,
            installation_id,
        );
    }
    retire_claims(destination, claims);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn settle_claimed_partial(
    store: &HubStore,
    destination: &AdmittedDirectory,
    claims: &Claims,
    name: &str,
    generation: Uuid,
    recorded: FileIdentity,
    password: &str,
    installation_id: Uuid,
) {
    let path = claims.path.join(name);
    let Ok(file) = open_entry(&claims.fd, name) else {
        claims.put_back(destination, name, name);
        return;
    };
    let is_ours = file
        .metadata()
        .is_ok_and(|metadata| metadata.file_type().is_file() && FileIdentity::of(&metadata) == recorded);
    if !is_ours {
        claims.put_back(destination, name, name);
        forget_run(store, generation);
        return;
    }
    match authenticate_whole(file, &path, password, None, "partial_member") {
        Ok((manifest, _)) => {
            // Complete: preserved, and published when it is this run's.
            let published = manifest.installation_id == installation_id
                && manifest.generation == generation
                && canonical_archive_name(&manifest)
                    .is_ok_and(|canonical| claims.put_back(destination, name, &canonical));
            if !published {
                claims.put_back(destination, name, name);
            }
            forget_run(store, generation);
        }
        Err(DataRecoveryError::CorruptArchive(_)) => {
            // This run's own object, cut short: never a generation.
            if unlinkat(&claims.fd, name, AtFlags::empty()).is_ok() {
                forget_run(store, generation);
            } else {
                claims.put_back(destination, name, name);
            }
        }
        Err(_) => {
            // Another password, or unreadable now: keep it.
            claims.put_back(destination, name, name);
        }
    }
}

/// One of this Hub's older generations, ranked by its authenticated manifest.
struct Candidate {
    created_at_ms: i64,
    generation: Uuid,
    name: String,
    identity: FileIdentity,
}

/// After [`confirm_publication`]: remove this Hub's generations beyond `keep`
/// (the confirmed new one counts as one), oldest first. Returns how many went
/// and the first failure or doubt, if any; the new generation is good either
/// way.
pub(super) fn rotate_after_publish(
    destination: &AdmittedDirectory,
    keep: usize,
    newest: &Held,
    password: &str,
    installation_id: Uuid,
    cancel: Option<&AtomicBool>,
) -> (usize, Option<String>) {
    if destination.trust != VolumeTrust::Posix {
        return (0, Some(UNPROTECTED.to_owned()));
    }
    if let Err(error) = hook("rotate_confirmed", &destination.path.join(&newest.name)) {
        return (0, Some(error.to_string()));
    }
    rotate(destination, keep, newest, password, installation_id, cancel)
}

fn rotate(
    destination: &AdmittedDirectory,
    keep: usize,
    newest: &Held,
    password: &str,
    installation_id: Uuid,
    cancel: Option<&AtomicBool>,
) -> (usize, Option<String>) {
    let mut doubt: Option<String> = None;
    let mut note = |message: String| {
        doubt.get_or_insert(message);
    };
    // Whatever an interrupted removal left claimed goes back first.
    let mut claims = match Claims::existing(destination) {
        Ok(Some(existing)) => {
            let stuck = existing.return_leftovers(destination);
            if !stuck.is_empty() {
                note(format!(
                    "{} could not be returned from {CLAIMS_DIRECTORY}",
                    stuck.join(", ")
                ));
            }
            Some(existing)
        }
        Ok(None) => None,
        Err(error) => {
            return (
                0,
                Some(format!("older generations were kept: {error}")),
            );
        }
    };

    let mut candidates = match ranked_candidates(destination, &newest.name, password, installation_id)
    {
        Ok(found) => found,
        Err(error) => {
            retire_claims(destination, claims);
            return (0, Some(format!("could not list the destination: {error}")));
        }
    };
    candidates.sort_by(|left, right| {
        (right.created_at_ms, right.generation).cmp(&(left.created_at_ms, left.generation))
    });

    let mut kept: Vec<Held> = Vec::new();
    let mut removed = 0;
    let mut failure: Option<String> = None;
    for candidate in candidates {
        if cancelled(cancel).is_err() {
            note("rotation was cancelled; no further generation was removed".to_owned());
            break;
        }
        if kept.len() + 1 < keep {
            match hold_authenticated(
                &destination.fd,
                &destination.path,
                &candidate.name,
                destination.trust,
                password,
                cancel,
                "rotation_member",
            ) {
                Ok((held, manifest, _))
                    if held.stamp.identity == candidate.identity
                        && manifest.generation == candidate.generation
                        && manifest.installation_id == installation_id =>
                {
                    kept.push(held);
                }
                _ => note(format!(
                    "{} did not authenticate in full; it was kept and not counted",
                    candidate.name
                )),
            }
            continue;
        }
        if !(newest.intact(&destination.fd) && kept.iter().all(|held| held.intact(&destination.fd))) {
            note(
                "a kept generation changed during rotation; no further generation was removed"
                    .to_owned(),
            );
            break;
        }
        let path = destination.path.join(&candidate.name);
        if let Err(error) = hook("rotate_remove", &path) {
            failure = Some(error.to_string());
            break;
        }
        if claims.is_none() {
            match Claims::open(destination) {
                Ok(opened) => claims = Some(opened),
                Err(error) => {
                    note(format!("older generations were kept: {error}"));
                    break;
                }
            }
        }
        let Some(held_claims) = claims.as_ref() else {
            break;
        };
        let guard = KeptGenerations {
            directory: &destination.fd,
            newest,
            kept: &kept,
        };
        match remove_candidate(
            destination,
            held_claims,
            &candidate,
            password,
            installation_id,
            &guard,
            cancel,
        ) {
            Ok(()) => removed += 1,
            Err(reason) => {
                note(reason);
                // A kept generation in doubt stops every further removal.
                if !guard.intact() {
                    break;
                }
            }
        }
    }
    let _ = fsync(&destination.fd);
    retire_claims(destination, claims);
    (removed, failure.or(doubt))
}

/// This Hub's generations other than the newest, by authenticated manifest:
/// admitted, the manifest naming this installation, the name exactly the one
/// the manifest implies.
fn ranked_candidates(
    destination: &AdmittedDirectory,
    newest: &str,
    password: &str,
    installation_id: Uuid,
) -> io::Result<Vec<Candidate>> {
    let mut found = Vec::new();
    for entry in Dir::read_from(&destination.fd)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if !(name.starts_with(ARCHIVE_PREFIX) && name.ends_with(ARCHIVE_SUFFIX)) || name == newest {
            continue;
        }
        let path = destination.path.join(name);
        let Ok(file) = open_entry(&destination.fd, name) else {
            continue;
        };
        let Ok(metadata) = backup_admission::admit_archive_file(&file, &path, destination.trust)
        else {
            continue;
        };
        let Ok(opened) = open_archive_file(file, &path, password) else {
            continue;
        };
        let manifest = opened.manifest;
        if manifest.installation_id != installation_id
            || canonical_archive_name(&manifest).ok().as_deref() != Some(name)
        {
            continue;
        }
        found.push(Candidate {
            created_at_ms: manifest.created_at_ms,
            generation: manifest.generation,
            name: name.to_owned(),
            identity: FileIdentity::of(&metadata),
        });
    }
    Ok(found)
}

/// The generations a removal must leave good: the confirmed newest and the
/// kept older ones.
struct KeptGenerations<'a> {
    directory: &'a OwnedFd,
    newest: &'a Held,
    kept: &'a [Held],
}

impl KeptGenerations<'_> {
    /// Every kept generation is unchanged since it authenticated and still
    /// under its own name.
    fn intact(&self) -> bool {
        self.newest.intact(self.directory) && self.kept.iter().all(|held| held.intact(self.directory))
    }
}

/// Claim the candidate into the protected directory, authenticate what was
/// claimed in full as the generation that was ranked, check once more that
/// every kept generation is still good, and only then unlink it. Anything
/// else goes back under its name and is reported. The last check runs after
/// the candidate's full read, immediately before the unlink (REV-CX-M06-T11
/// finding 3), so a kept generation damaged while the candidate was read
/// keeps the candidate.
#[allow(clippy::too_many_arguments)]
fn remove_candidate(
    destination: &AdmittedDirectory,
    claims: &Claims,
    candidate: &Candidate,
    password: &str,
    installation_id: Uuid,
    guard: &KeptGenerations<'_>,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    let name = candidate.name.as_str();
    claims
        .claim(destination, name)
        .map_err(|error| format!("{name} could not be claimed for removal and was kept: {error}"))?;
    let verdict = hold_authenticated(
        &claims.fd,
        &claims.path,
        name,
        destination.trust,
        password,
        cancel,
        "rotation_member",
    )
    .map_err(|error| error.to_string())
        .and_then(|(held, manifest, _)| {
            let same = held.stamp.identity == candidate.identity
                && manifest.generation == candidate.generation
                && manifest.installation_id == installation_id
                && canonical_archive_name(&manifest).ok().as_deref() == Some(name);
            if !same {
                Err("the claimed file is not the generation that was ranked".to_owned())
            } else if hook("rotate_before_unlink", &claims.path.join(name)).is_err()
                || cancelled(cancel).is_err()
            {
                Err("the removal was stopped".to_owned())
            } else if !guard.intact() {
                Err("a kept generation changed while it was read; no further generation was \
                     removed"
                    .to_owned())
            } else {
                Ok(held)
            }
        });
    match verdict {
        Ok(held) => {
            drop(held);
            unlinkat(&claims.fd, name, AtFlags::empty()).map_err(|error| {
                claims.put_back(destination, name, name);
                format!("{name} could not be removed and was kept: {error}")
            })
        }
        Err(reason) => {
            let returned = claims.put_back(destination, name, name);
            Err(if returned {
                format!("{name} was kept: {reason}")
            } else {
                format!("{name} was kept in {CLAIMS_DIRECTORY}: {reason}")
            })
        }
    }
}
