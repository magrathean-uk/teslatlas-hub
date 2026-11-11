// SPDX-License-Identifier: AGPL-3.0-only

//! Retention, stale-partial cleanup and removal of encrypted-backup
//! generations (M06-T02).
//!
//! The rule behind every step: when in doubt, keep every existing generation
//! and report; never delete.
//!
//! - Every operation is relative to the destination descriptor admitted for
//!   the run, never to a path looked up again.
//! - The new generation counts towards retention only once the complete
//!   published file has been authenticated (every chunk, every member digest
//!   and the trailer) and is the very object the run wrote (same device and
//!   inode, same manifest bytes). It stays open, and before every removal it
//!   and every other kept generation must still be unchanged and still under
//!   its own name; otherwise nothing more is removed.
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
    io,
    os::{
        fd::OwnedFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Path,
};

use rusqlite::params;
use rustix::{
    fs::{AtFlags, Dir, Mode, OFlags, RenameFlags, fsync, mkdirat, openat, renameat_with, unlinkat},
    io::Errno,
};
use uuid::Uuid;

use super::{
    ARCHIVE_PREFIX, ARCHIVE_SUFFIX, BackupManifest, DataRecoveryError, HubStore, PARTIAL_PREFIX,
    PARTIAL_SUFFIX, backup_admission::{self, AdmittedDirectory, VolumeTrust},
    canonical_archive_name, current_epoch_ms, hook, invalid, io_error, open_archive_file,
    pin_error, stream_members,
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
/// and the manifest's exact bytes.
fn authenticate_whole(
    file: File,
    path: &Path,
    password: &str,
) -> Result<(BackupManifest, Vec<u8>), DataRecoveryError> {
    let mut opened = open_archive_file(file, path, password)?;
    stream_members(&mut opened, |_| Ok(None))?;
    Ok((opened.manifest, opened.manifest_bytes))
}

/// Open `name` in `directory`, admit it for `trust`, authenticate it in full
/// and keep it open. It must not change while it is read.
fn hold_authenticated(
    directory: &OwnedFd,
    directory_path: &Path,
    name: &str,
    trust: VolumeTrust,
    password: &str,
) -> Result<(Held, BackupManifest, Vec<u8>), DataRecoveryError> {
    let path = directory_path.join(name);
    let file =
        open_entry(directory, name).map_err(|source| io_error("opening backup archive", &path, source))?;
    let before = backup_admission::admit_archive_file(&file, &path, trust)?;
    let reader = file
        .try_clone()
        .map_err(|source| io_error("opening backup archive", &path, source))?;
    let (manifest, manifest_bytes) = authenticate_whole(reader, &path, password)?;
    let after = file
        .metadata()
        .map_err(|source| io_error("inspecting backup archive", &path, source))?;
    if Stamp::of(&after) != Stamp::of(&before) {
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
    match authenticate_whole(file, &path, password) {
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

/// After publication: confirm the new generation, then remove this Hub's
/// generations beyond `keep` (the new one counts as one), oldest first.
/// Returns how many went and the first failure or doubt, if any.
pub(super) fn rotate_after_publish(
    destination: &AdmittedDirectory,
    keep: usize,
    newest_name: &str,
    written: FileIdentity,
    manifest_bytes: &[u8],
    password: &str,
    installation_id: Uuid,
) -> (usize, Option<String>) {
    if destination.trust != VolumeTrust::Posix {
        return (0, Some(UNPROTECTED.to_owned()));
    }
    let newest = match hold_authenticated(
        &destination.fd,
        &destination.path,
        newest_name,
        destination.trust,
        password,
    ) {
        Ok((held, _, bytes)) if held.stamp.identity == written && bytes == manifest_bytes => held,
        Ok(_) => {
            return (
                0,
                Some(
                    "older generations were kept: the published file is not the generation \
                     this run wrote"
                        .to_owned(),
                ),
            );
        }
        Err(error) => {
            return (
                0,
                Some(format!(
                    "older generations were kept: the new generation could not be \
                     authenticated after publication: {error}"
                )),
            );
        }
    };
    if let Err(error) = hook("rotate_confirmed", &destination.path.join(newest_name)) {
        return (0, Some(error.to_string()));
    }
    rotate(destination, keep, newest, password, installation_id)
}

fn rotate(
    destination: &AdmittedDirectory,
    keep: usize,
    newest: Held,
    password: &str,
    installation_id: Uuid,
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

    let mut kept = vec![newest];
    let mut removed = 0;
    let mut failure: Option<String> = None;
    for candidate in candidates {
        if kept.len() < keep {
            match hold_authenticated(
                &destination.fd,
                &destination.path,
                &candidate.name,
                destination.trust,
                password,
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
        if !kept.iter().all(|held| held.intact(&destination.fd)) {
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
        match remove_candidate(destination, held_claims, &candidate, password, installation_id) {
            Ok(()) => removed += 1,
            Err(reason) => note(reason),
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

/// Claim the candidate into the protected directory, authenticate what was
/// claimed in full as the generation that was ranked, and only then unlink
/// it. Anything else goes back under its name and is reported.
fn remove_candidate(
    destination: &AdmittedDirectory,
    claims: &Claims,
    candidate: &Candidate,
    password: &str,
    installation_id: Uuid,
) -> Result<(), String> {
    let name = candidate.name.as_str();
    claims
        .claim(destination, name)
        .map_err(|error| format!("{name} could not be claimed for removal and was kept: {error}"))?;
    let verdict = hold_authenticated(&claims.fd, &claims.path, name, destination.trust, password)
        .map_err(|error| error.to_string())
        .and_then(|(held, manifest, _)| {
            let same = held.stamp.identity == candidate.identity
                && manifest.generation == candidate.generation
                && manifest.installation_id == installation_id
                && canonical_archive_name(&manifest).ok().as_deref() == Some(name);
            if same {
                Ok(held)
            } else {
                Err("the claimed file is not the generation that was ranked".to_owned())
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
