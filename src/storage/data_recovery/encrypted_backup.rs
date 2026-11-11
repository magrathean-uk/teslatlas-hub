// SPDX-License-Identifier: AGPL-3.0-only

//! The service-owned encrypted backup operation (M06-T01).
//!
//! One run produces one archive file (one generation) in a user-chosen
//! destination folder, encrypted with a key derived from a user-kept recovery
//! password (see [`super::archive_crypto`] for the container).
//!
//! Plaintext handling. The archive streams encrypted straight into a partial
//! file in the destination; nothing readable is ever written there. The one
//! plaintext artefact is the online catalogue copy that SQLite's backup API
//! produces, which needs a file: it lives in a private (0700) scratch
//! directory outside the destination, is sealed (host authority removed)
//! before it is read into the archive, and goes when the run ends. Packs and
//! the signed no-op bodies are streamed from their source files and never
//! copied.
//!
//! Failure rule. A generation becomes visible only by a no-replace rename of a
//! fully written, synced partial file, so every failure before the rename
//! leaves the destination exactly as it was: the last good generation stays.
//! Rotation runs only after publication and never touches the new generation;
//! a rotation failure is reported beside a successful backup.

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
};

use super::{
    BACKUP_KIND, BACKUP_SCOPE, BUILD_VERSION, BackupManifest, BackupMember, CATALOGUE_MEMBER,
    DataRecoveryError, DataRecoveryReport, HubStore, MAX_BACKUP_MEMBERS, MAX_MANIFEST_BYTES,
    PACK_DIRECTORY, PRIVATE_FILE_MODE, SCHEMA_22_NOOP_DIRECTORY, SCHEMA_VERSION, Sha256,
    StagingDirectory, StoreError, admit_staging_capacity,
    archive_crypto::{
        ArchiveReader, ArchiveWriter, DEFAULT_CHUNK_BYTES, DEFAULT_KDF_ITERATIONS, map_write_error,
    },
    canonical_json, canonical_pack_filename, check_backup_admission, create_private_directory,
    current_epoch_ms, current_schema_22_noop_pairs, excluded_host_state_strings, expected_protocol,
    finalize_restore, immutable_database_identity, immutable_database_identity_at_schema, invalid,
    inventory_file, io_error, manifest_copy_bytes, no_replace_rename, open_regular_nonblocking,
    parse_canonical_json, prepare_new_destination, read_directory, report, seal_backup_payload,
    set_mode, sync_directory, validate_manifest, validate_restored_data_tree,
    validate_schema_22_noop_directory, write_private_file,
};
use serde::Serialize;
use sha2::Digest;
use uuid::Uuid;

const ARCHIVE_PREFIX: &str = "teslatlas-hub-backup-";
const ARCHIVE_SUFFIX: &str = ".tlbk";
const PARTIAL_PREFIX: &str = ".teslatlas-hub-backup-";
const PARTIAL_SUFFIX: &str = ".partial";
const SCRATCH_PREFIX: &str = ".teslatlas-backup-scratch-";
const MANIFEST_RECORD: &str = "manifest.json";
const RECORD_FILE: u8 = 1;
const RECORD_END: u8 = 0xFE;
const MAX_RECORD_PATH_BYTES: usize = 1024;
const STREAM_BUFFER_BYTES: usize = 1024 * 1024;

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
    pub(crate) kdf_iterations: u32,
    pub(crate) chunk_bytes: usize,
    pub(crate) scratch_parent: Option<PathBuf>,
}

impl<'a> EncryptedBackupRequest<'a> {
    pub fn new(destination: &'a Path, password: &'a str) -> Self {
        Self {
            destination,
            password,
            keep: None,
            cancel: None,
            kdf_iterations: DEFAULT_KDF_ITERATIONS,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            scratch_parent: None,
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

/// A test seam: stages are "write", "record", "before_publish" and
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
    /// A file in the private scratch directory (catalogue, no-op bodies).
    Scratch(PathBuf),
    /// A pack in the live store, content-addressed by its own name.
    Pack(PathBuf),
}

struct Planned {
    member: BackupMember,
    source: Source,
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

/// A backup left half-written by a killed process: the Hub's own partial name,
/// a regular file, never a link.
fn remove_stale_partials(destination: &Path) -> Result<(), DataRecoveryError> {
    let entries =
        fs::read_dir(destination).map_err(|source| DataRecoveryError::DestinationUnavailable {
            path: destination.to_path_buf(),
            source,
        })?;
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(inner) = name
            .strip_prefix(PARTIAL_PREFIX)
            .and_then(|rest| rest.strip_suffix(PARTIAL_SUFFIX))
        else {
            continue;
        };
        if Uuid::parse_str(inner).is_err() {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

/// Create one encrypted backup generation.
pub fn create_encrypted_backup(
    store: &HubStore,
    request: &EncryptedBackupRequest<'_>,
) -> Result<EncryptedBackupReport, DataRecoveryError> {
    let cancel = request.cancel.as_deref();
    cancelled(cancel)?;
    let destination = resolve_destination(request.destination)?;
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
    let scratch_parent = match &request.scratch_parent {
        Some(path) => path.clone(),
        None => std::env::temp_dir(),
    }
    .canonicalize()
    .map_err(|source| io_error("resolving backup scratch directory", &destination, source))?;
    if scratch_parent.starts_with(&destination) {
        return Err(invalid(
            "the backup scratch directory must not be inside the destination",
        ));
    }

    let source_installation_id = store.installation_id()?;
    store.catalogue_check()?;
    // The publication gate is held for the whole run: no publication, GC or
    // compaction that takes it can change the catalogue or the packs while
    // they are read. Dropping the snapshot releases it.
    let snapshot = store.begin_backup_snapshot()?;
    check_backup_admission(&snapshot)?;
    remove_stale_partials(&destination)?;
    cancelled(cancel)?;

    let scratch = tempfile::Builder::new()
        .prefix(SCRATCH_PREFIX)
        .tempdir_in(&scratch_parent)
        .map_err(|source| io_error("creating backup scratch directory", &scratch_parent, source))?;
    let data = scratch.path().join("data");
    create_private_directory(&data)?;
    let catalogue = data.join("hub.sqlite");
    store.backup_catalogue_to(&catalogue)?;
    seal_backup_payload(&data)?;
    set_mode(
        &catalogue,
        PRIVATE_FILE_MODE,
        "protecting scratch catalogue",
    )?;
    if immutable_database_identity(&data)? != source_installation_id {
        return Err(invalid(
            "copied catalogue installation ID does not match the live source",
        ));
    }
    cancelled(cancel)?;

    let planned = plan_members(store, &data, &catalogue)?;
    let payload_bytes = planned.iter().try_fold(0_u64, |total, item| {
        total
            .checked_add(item.member.size)
            .ok_or(DataRecoveryError::CapacityOverflow)
    })?;
    admit_staging_capacity(&destination, payload_bytes)?;

    let generation = Uuid::new_v4();
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

    let final_path = destination.join(format!(
        "{ARCHIVE_PREFIX}{}-{generation}{ARCHIVE_SUFFIX}",
        generation_stamp(created_at_ms)?
    ));
    let partial = destination.join(format!("{PARTIAL_PREFIX}{generation}{PARTIAL_SUFFIX}"));
    let written = write_partial(request, &partial, &manifest_bytes, &planned);
    let archive_bytes = match written {
        Ok(bytes) => bytes,
        Err(error) => {
            #[cfg(test)]
            if test_hooks::CRASHED.with(|flag| flag.replace(false)) {
                return Err(error);
            }
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
    };
    let published = (|| {
        hook("before_publish", &partial)?;
        cancelled(cancel)?;
        no_replace_rename(&partial, &final_path)?;
        sync_directory(&destination)
    })();
    if let Err(error) = published {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    drop(scratch);
    drop(snapshot);

    let (rotated_generations, rotation_error) = match request.keep {
        Some(keep) => rotate(&destination, keep.get(), &final_path),
        None => (0, None),
    };
    Ok(EncryptedBackupReport {
        status: "encrypted_backup_created",
        path: final_path,
        generation,
        installation_id: source_installation_id,
        member_count: manifest.members.len(),
        payload_bytes,
        archive_bytes,
        rotated_generations,
        rotation_error,
    })
}

fn plan_members(
    store: &HubStore,
    data: &Path,
    catalogue: &Path,
) -> Result<Vec<Planned>, DataRecoveryError> {
    let mut planned = vec![Planned {
        member: inventory_file(catalogue, CATALOGUE_MEMBER.to_owned())?,
        source: Source::Scratch(catalogue.to_path_buf()),
    }];

    // Packs are immutable and content-addressed: the file name is the digest,
    // which the stream re-checks while it reads.
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

    // The signed no-op body of each current schema 2.2 snapshot.
    let expected = current_schema_22_noop_pairs(data)?;
    if !expected.is_empty() {
        let noop_directory = data.join("packs").join("noop");
        create_private_directory(&data.join("packs"))?;
        create_private_directory(&noop_directory)?;
        for (name, manifest) in &expected {
            let bytes = store
                .schema_22_noop_for_snapshot(manifest.vehicle_id, manifest.snapshot_id)?
                .ok_or(StoreError::Schema22NoOpNotFound)?;
            let path = noop_directory.join(name);
            write_private_file(&path, &bytes)?;
            planned.push(Planned {
                member: inventory_file(&path, format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}"))?,
                source: Source::Scratch(path),
            });
            if planned.len() > MAX_BACKUP_MEMBERS {
                return Err(invalid("backup contains too many members"));
            }
        }
        validate_schema_22_noop_directory(data, &expected)?;
    }
    planned.sort_by(|left, right| left.member.path.cmp(&right.member.path));
    Ok(planned)
}

fn write_partial(
    request: &EncryptedBackupRequest<'_>,
    partial: &Path,
    manifest_bytes: &[u8],
    planned: &[Planned],
) -> Result<u64, DataRecoveryError> {
    let cancel = request.cancel.as_deref();
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(partial)
        .map_err(|source| DataRecoveryError::DestinationUnavailable {
            path: partial.to_path_buf(),
            source,
        })?;
    let sink = Sink {
        file,
        path: partial.to_path_buf(),
    };
    let mut writer = ArchiveWriter::start(
        sink,
        partial.to_path_buf(),
        request.password,
        request.kdf_iterations,
        request.chunk_bytes,
    )?;

    hook("record", partial)?;
    put_file_header(&mut writer, MANIFEST_RECORD, manifest_bytes.len() as u64)?;
    writer.write_plain(manifest_bytes)?;
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    for item in planned {
        cancelled(cancel)?;
        hook("record", partial)?;
        stream_member(&mut writer, item, &mut buffer, cancel)?;
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
    item: &Planned,
    buffer: &mut [u8],
    cancel: Option<&AtomicBool>,
) -> Result<(), DataRecoveryError> {
    let (Source::Scratch(path) | Source::Pack(path)) = &item.source;
    let member = &item.member;
    let mut input = open_regular_nonblocking(path, "opening backup source member")?;
    let metadata = input
        .metadata()
        .map_err(|source| io_error("inspecting backup source member", path, source))?;
    if metadata.len() != member.size || metadata.nlink() == 0 {
        return Err(invalid(format!(
            "backup source member changed before it was streamed: {}",
            member.path
        )));
    }
    put_file_header(writer, &member.path, member.size)?;
    let mut digest = Sha256::new();
    let mut streamed = 0_u64;
    loop {
        cancelled(cancel)?;
        let read = input
            .read(buffer)
            .map_err(|source| io_error("reading backup source member", path, source))?;
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
    if streamed != member.size || hex::encode(digest.finalize()) != member.sha256 {
        return Err(invalid(format!(
            "backup source member changed or is damaged: {}",
            member.path
        )));
    }
    Ok(())
}

/// Remove generations beyond `keep`, oldest first, only after the new one is
/// published. Returns how many went and the first failure, if any.
fn rotate(destination: &Path, keep: usize, newest: &Path) -> (usize, Option<String>) {
    let entries = match fs::read_dir(destination) {
        Ok(entries) => entries,
        Err(error) => return (0, Some(format!("could not list the destination: {error}"))),
    };
    let mut generations: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(ARCHIVE_PREFIX)
                && name.ends_with(ARCHIVE_SUFFIX)
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
        .map(|entry| entry.path())
        .collect();
    generations.sort();
    generations.reverse();
    let mut removed = 0;
    for path in generations.into_iter().skip(keep) {
        if path == newest {
            continue;
        }
        let result = hook("rotate_remove", &path).and_then(|()| {
            fs::remove_file(&path)
                .map_err(|source| io_error("removing an old generation", &path, source))
        });
        match result {
            Ok(()) => removed += 1,
            Err(error) => return (removed, Some(error.to_string())),
        }
    }
    let _ = sync_directory(destination);
    (removed, None)
}

struct OpenedArchive {
    reader: ArchiveReader<File>,
    manifest: BackupManifest,
    manifest_bytes: Vec<u8>,
}

fn open_archive(archive: &Path, password: &str) -> Result<OpenedArchive, DataRecoveryError> {
    let file = open_regular_nonblocking(archive, "opening backup archive")?;
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
