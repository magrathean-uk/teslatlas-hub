// SPDX-License-Identifier: AGPL-3.0-only

//! Bounded Hub data-backup generation, immutable verification, and restore.
//!
//! A backup generation contains the Hub catalogue (including encrypted token
//! rows, but excluding pairing challenges and paired-device bearer authority),
//! every immutable pack referenced by that catalogue, and each current
//! schema-2.2 manifest's signed no-op body. Decryption and signing keys are
//! deliberately excluded. TLS/configuration/service state is also omitted, so
//! restore leaves the service stopped and collector authority absent. Restore
//! generates a fresh cursor key and re-signs the restored sync state under it
//! (M06-T09), so no key from the old host is needed and no old cursor verifies.
//!
//! Member count (M06-T10, M06-T15). No format refuses a backup for its
//! number of members. The encrypted bundle (v5) streams its members, each
//! record carrying its own path, size and digest, and its manifest declares
//! only the count and bytes, so neither backup nor restore holds the member
//! list. A new plaintext directory backup is written in the listed format
//! (v4) while its members fit [`LISTED_FORMAT_MAX_MEMBERS`], so earlier
//! builds can read it, and above that in the streamed directory format (v5):
//! its manifest names only the catalogue member and declares the count and
//! bytes, packs are named by their digest and no-ops are checked against the
//! catalogue, so every check walks the tree. A listed manifest is held in
//! memory, so reading one stays bounded by the bound it was written under.
//! The schema 2.2 no-op selection reads the catalogue one manifest row at a
//! time and keeps one identity per vehicle, never a manifest.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use rustix::fs::{Mode, OFlags, open, statvfs};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    BUILD_VERSION,
    db::{APPLICATION_ID, HubStore, SCHEMA_VERSION, StoreError},
    protocol::{
        HUB_PROJECTION_SCHEMA_V3, LINEAGE_PROTOCOL_V2, LineageDelta, LineageManifestV2,
        PROTOCOL_NAME, PROTOCOL_V1, ProtocolVersion, SyncManifest, TransferMode,
    },
    updates_delivery::{SignedNoOpState, validate_schema_22_pair},
};

const BACKUP_KIND: &str = "teslatlas-hub-data-backup-v4"; // rename:keep
/// The streamed encrypted bundle (M06-T10): its manifest lists no member.
const STREAMED_BACKUP_KIND: &str = "teslatlas-hub-data-backup-v5"; // rename:keep
/// The streamed plaintext directory (M06-T15): its manifest names only the
/// catalogue member and declares the member count and bytes.
const STREAMED_DIRECTORY_KIND: &str = "teslatlas-hub-data-backup-directory-v5"; // rename:keep
const STREAMED_DIRECTORY_COMPLETION_KIND: &str = "teslatlas-hub-data-backup-complete-v5"; // rename:keep
const STREAMED_DIRECTORY_MANIFEST_NAME: &str = "backup-v5.json";
const COMPLETION_KIND: &str = "teslatlas-hub-data-backup-complete-v4"; // rename:keep
const BACKUP_SCOPE: &str = "hub_data_without_pairing_or_keys";
const MANIFEST_NAME: &str = "backup-v4.json";
const LEGACY_BACKUP_KIND: &str = "teslatlas-hub-data-backup-v3"; // rename:keep
const LEGACY_COMPLETION_KIND: &str = "teslatlas-hub-data-backup-complete-v3"; // rename:keep
const LEGACY_BACKUP_SCOPE: &str = "hub_data_and_pairing_without_keys";
const LEGACY_MANIFEST_NAME: &str = "backup-v3.json";
const COMPLETION_NAME: &str = "BACKUP_COMPLETE";
const STATUS_DATA_RESTORED: &str = "data_restored";
/// Carried by a successful restore report only (C3 R7, R25, R27): the restore
/// returns the Fleet counts and reservations of the backup and forces no cap.
const FLEET_REQUEST_COUNTS_NOTICE: &str = "Fleet request counts and reservations come from the backup. A restore from earlier in the same UTC calendar month can undercount requests already billed and allow the Hub's local monthly cap to be exceeded. No cap has been forced. Tesla's billing remains authoritative.";
const DATA_DIRECTORY: &str = "data";
const CATALOGUE_MEMBER: &str = "data/hub.sqlite";
const PACK_DIRECTORY: &str = "data/packs/sha256";
const SCHEMA_22_NOOP_DIRECTORY: &str = "data/packs/noop";
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const MAX_SCHEMA_22_NOOP_BYTES: u64 = 16 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_COMPLETION_BYTES: u64 = 4 * 1024;
/// The bound of a format whose manifest lists every member: that list is
/// held in memory whole, so it is bounded. The streamed bundle has no list
/// and no member bound.
const LISTED_FORMAT_MAX_MEMBERS: usize = 4_096;
const COPY_BUFFER_BYTES: usize = 1024 * 1024;
// Admission reserves only metadata/write-amplification headroom. It does not
// allocate a second copy: the staged backup/restore remains one payload copy.
const COPY_CAPACITY_HEADROOM_DIVISOR: u64 = 20;
const COPY_CAPACITY_FIXED_HEADROOM_BYTES: u64 = 8 * 1024 * 1024;
/// Per member, beyond its bytes and its partly used last block: the inode
/// and directory entry the filesystem keeps for it (M06-T15).
const MEMBER_METADATA_BYTES: u64 = 4 * 1024;
const MIN_RESTORABLE_SCHEMA_VERSION: i32 = crate::db::MIN_STORE_VERSION;
/// The first schema whose catalogue always holds the per-device vehicle grant
/// tables (and, with them, every pairing table). A backup of an older schema
/// may lack the grant tables and the rotation grace table; a backup that
/// declares this schema or a later one must hold all of them. The historical
/// optional-table branches retire with the older backup formats in G2-4 M3.
const FIRST_SCHEMA_WITH_GRANT_TABLES: i32 = 68;
const EXCLUDED_HOST_STATE: [&str; 5] = [
    "collector_decryption_key",
    "cursor_signing_key",
    "tls_identity",
    "hub_configuration",
    "service_state",
];

#[derive(Debug, Error)]
pub enum DataRecoveryError {
    #[error("data recovery I/O failed while {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid Hub data backup: {0}")]
    InvalidBackup(String),
    #[error("cannot encode Hub data-backup metadata: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("cannot decode Hub data-backup metadata: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("Hub data-backup catalogue validation failed: {0}")]
    Store(#[from] StoreError),
    #[error("Hub data-backup SQLite identity query failed: {0}")]
    CatalogueIdentity(#[source] rusqlite::Error),
    #[error("Hub data-backup credential validation failed: {0}")]
    Credential(#[from] crate::legacy_owner_credentials::SourceDbCredentialError),
    #[error("could not inspect free space at {path}: {source}")]
    FilesystemSpace {
        path: PathBuf,
        #[source]
        source: rustix::io::Errno,
    },
    #[error("the backup password is wrong, or the archive header is damaged")]
    WrongPassword,
    #[error("the backup archive is damaged: {0}")]
    CorruptArchive(String),
    #[error("the backup was cancelled")]
    Cancelled,
    #[error("cannot read the recovery password: {0}")]
    PasswordInput(&'static str),
    #[error(
        "plaintext_directory_refused: {path} is an older plaintext directory backup, which is not authenticated; name that format with --plaintext-directory to use it"
    )]
    PlaintextDirectoryRefused { path: PathBuf },
    #[error("the backup destination is unavailable at {path}: {source}")]
    DestinationUnavailable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the backup destination is full at {path}")]
    DestinationFull { path: PathBuf },
    #[error("Hub data-recovery staging capacity calculation overflowed")]
    CapacityOverflow,
    #[error(
        "insufficient free space at {path}: recovery staging needs {required} bytes, only {available} bytes available"
    )]
    InsufficientFreeSpace {
        path: PathBuf,
        required: u64,
        available: u64,
    },
    /// An encrypted backup whose scratch need exceeds the configured budget
    /// (M06-T21), refused before any scratch is attached or anything copied.
    #[error(
        "scratch_budget_exceeded: the encrypted backup of a {catalogue}-byte catalogue needs {required} bytes of scratch, above the scratch budget of {budget} bytes; nothing was attached or copied"
    )]
    ScratchBudgetExceeded {
        catalogue: u64,
        required: u64,
        budget: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BackupProtocol {
    name: String,
    snapshot: ProtocolVersion,
    lineage: ProtocolVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BackupMember {
    path: String,
    size: u64,
    mode: u32,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BackupManifest {
    kind: String,
    generation: Uuid,
    created_at_ms: i64,
    build: String,
    hub_schema: i32,
    protocol: BackupProtocol,
    installation_id: Uuid,
    scope: String,
    excluded_host_state: Vec<String>,
    /// Every member, in a listed format; empty in a streamed bundle.
    members: Vec<BackupMember>,
    /// A streamed bundle's declared member count; absent in a listed format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    member_count: Option<u64>,
    /// A streamed bundle's declared payload bytes; absent in a listed format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payload_bytes: Option<u64>,
}

/// What a streamed read of a bundle established about its members, without
/// keeping them: the catalogue member and the pack and no-op counts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamedMembers {
    catalogue: BackupMember,
    packs: u64,
    noops: u64,
    kept: u64,
}

/// The members a restore checks its staged tree against.
#[derive(Debug, Clone, Copy)]
enum RestoredMembers<'a> {
    /// A listed format: its manifest's whole (bounded) member list.
    Listed(&'a [BackupMember]),
    /// A streamed bundle: only what its stream established.
    Streamed(&'a StreamedMembers),
}

impl RestoredMembers<'_> {
    fn has_noops(self) -> bool {
        match self {
            Self::Listed(members) => members
                .iter()
                .any(|member| schema_22_noop_member_filename(&member.path).is_some()),
            Self::Streamed(streamed) => streamed.noops > 0,
        }
    }

    fn has_kept(self) -> bool {
        match self {
            Self::Listed(members) => members.iter().any(|member| kept_member_filename(&member.path).is_some()),
            Self::Streamed(streamed) => streamed.kept > 0,
        }
    }
}

/// The kind of one member, by its canonical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberKind {
    Catalogue,
    Noop,
    Pack,
    Kept,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CompletionMarker {
    kind: String,
    generation: Uuid,
    manifest_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackupFormat {
    Current,
    LegacyV3,
    /// The streamed encrypted bundle (M06-T10); never a directory.
    Streamed,
    /// The streamed plaintext directory (M06-T15); never a bundle.
    StreamedDirectory,
}

impl BackupFormat {
    fn manifest_name(self) -> &'static str {
        match self {
            Self::Current | Self::Streamed => MANIFEST_NAME,
            Self::LegacyV3 => LEGACY_MANIFEST_NAME,
            Self::StreamedDirectory => STREAMED_DIRECTORY_MANIFEST_NAME,
        }
    }

    fn completion_kind(self) -> &'static str {
        match self {
            Self::Current | Self::Streamed => COMPLETION_KIND,
            Self::LegacyV3 => LEGACY_COMPLETION_KIND,
            Self::StreamedDirectory => STREAMED_DIRECTORY_COMPLETION_KIND,
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Self::Current | Self::Streamed | Self::StreamedDirectory => BACKUP_SCOPE,
            Self::LegacyV3 => LEGACY_BACKUP_SCOPE,
        }
    }
}

/// Machine-readable receipt whose limitations are part of every successful
/// command result. `clean_host_ready` is intentionally always false.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataRecoveryReport {
    pub status: &'static str,
    pub path: PathBuf,
    pub generation: Uuid,
    pub installation_id: Uuid,
    pub member_count: usize,
    pub total_bytes: u64,
    pub scope: &'static str,
    pub clean_host_ready: bool,
    pub collector_authority: &'static str,
    pub excluded_host_state: Vec<&'static str>,
    /// Present only on a successful restore (C3 R7, R25, R27): the Fleet request
    /// counts and reservations come from the backup, and a restore from earlier
    /// in the same UTC month can undercount requests already billed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fleet_request_counts_notice: Option<&'static str>,
}

/// The staging directory of a plaintext backup or a directory-format
/// restore. It is held under an exclusive lock from creation until it is
/// published or dropped (M06-T15), so a staging directory nobody holds locked
/// was left by an interrupted run, whichever build made it, and an encrypted
/// restore's sweep can tell it from an active one.
struct StagingDirectory {
    path: PathBuf,
    published: bool,
    _lock: Option<File>,
}

/// Open `path` as a real directory, without following a link, take its
/// exclusive lock without waiting, and confirm the path still names it.
fn lock_directory(path: &Path) -> Option<(File, fs::Metadata)> {
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    rustix::fs::flock(&fd, rustix::fs::FlockOperation::NonBlockingLockExclusive).ok()?;
    let directory = File::from(fd);
    let held = directory.metadata().ok()?;
    let named = fs::symlink_metadata(path).ok()?;
    (named.dev() == held.dev() && named.ino() == held.ino()).then_some((directory, held))
}

impl StagingDirectory {
    fn create(parent: &Path) -> Result<Self, DataRecoveryError> {
        for _ in 0..16 {
            let path = parent.join(format!(
                ".teslatlas-data-recovery-{}.staging",
                Uuid::new_v4()
            ));
            let mut builder = fs::DirBuilder::new();
            builder.mode(PRIVATE_DIRECTORY_MODE);
            match builder.create(&path) {
                Ok(()) => {
                    // A concurrent sweep that locked it first removes it.
                    let Some((lock, _)) = lock_directory(&path) else {
                        continue;
                    };
                    set_mode(
                        &path,
                        PRIVATE_DIRECTORY_MODE,
                        "protecting staging directory",
                    )?;
                    return Ok(Self {
                        path,
                        published: false,
                        _lock: Some(lock),
                    });
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(io_error("creating staging directory", &path, source));
                }
            }
        }
        Err(invalid(
            "could not allocate a unique sibling staging directory",
        ))
    }

    fn publish(mut self, destination: &Path) -> Result<(), DataRecoveryError> {
        sync_directory(&self.path)?;
        no_replace_rename(&self.path, destination)?;
        self.published = true;
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("published destination has no parent"))?;
        sync_directory(parent)
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Create and atomically publish one private data-backup generation. No
/// member count refuses it: above the listed bound it is written in the
/// streamed directory format (M06-T15).
pub fn create_data_backup(
    store: &HubStore,
    destination: &Path,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    create_data_backup_admitted(store, destination, None)
}

/// A test-only admission: refuse a backup above `member_limit` members
/// before anything is staged.
#[cfg(test)]
fn create_data_backup_with_member_limit(
    store: &HubStore,
    destination: &Path,
    member_limit: usize,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    create_data_backup_admitted(store, destination, Some(member_limit))
}

fn create_data_backup_admitted(
    store: &HubStore,
    destination: &Path,
    member_limit: Option<usize>,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    let (destination, parent) = prepare_new_destination(destination)?;
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
    if parent.starts_with(&live_data) {
        return Err(invalid(
            "data-backup destination must not be inside the live Hub data directory",
        ));
    }

    let source_installation_id = store.installation_id()?;
    store.catalogue_check()?;
    let backup_snapshot = store.begin_backup_snapshot()?;
    check_backup_admission(&backup_snapshot)?;
    let (copy_bytes, members) = backup_snapshot.copy_admission()?;
    if member_limit.is_some_and(|limit| members > limit) {
        return Err(invalid("backup contains too many members"));
    }
    remove_unpublished_leftovers(&parent)?;
    admit_member_capacity(&parent, copy_bytes, members as u64)?;
    let staging = StagingDirectory::create(&parent)?;
    let payload = staging.path.join(DATA_DIRECTORY);
    let sealed = backup_snapshot
        .copy_to(&payload)
        .map_err(DataRecoveryError::from)
        .and_then(|()| seal_backup_payload(&payload));
    if let Err(error) = sealed {
        #[cfg(test)]
        if crate::auth::credential_migration::seam::take_crash() {
            // A real crash ends the process, which releases the staging lock
            // (M06-T15) and leaves the directory; the simulated one does both.
            let mut staging = staging;
            drop(staging._lock.take());
            std::mem::forget(staging);
            return Err(error);
        }
        remove_unpublished_payload(&payload);
        return Err(error);
    }
    protect_payload_tree(&payload)?;

    let copied_store = HubStore::open_immutable_read_only(&payload)?;
    copied_store.catalogue_check()?;
    let copied_installation_id = immutable_database_identity(&payload)?;
    if copied_installation_id != source_installation_id {
        return Err(invalid(
            "copied catalogue installation ID does not match the live source",
        ));
    }
    copied_store.verify_immutable_snapshot_unchanged()?;

    if streamed_directory::count_payload_members(&payload)? > LISTED_FORMAT_MAX_MEMBERS as u64 {
        return streamed_directory::publish_streamed_directory(staging, &destination, &payload, source_installation_id);
    }
    let members = inventory_payload(&payload)?;
    let manifest = BackupManifest {
        kind: BACKUP_KIND.to_owned(),
        generation: Uuid::new_v4(),
        created_at_ms: current_epoch_ms()?,
        build: BUILD_VERSION.to_owned(),
        hub_schema: SCHEMA_VERSION,
        protocol: expected_protocol(),
        installation_id: source_installation_id,
        scope: BACKUP_SCOPE.to_owned(),
        excluded_host_state: excluded_host_state_strings(),
        members,
        member_count: None,
        payload_bytes: None,
    };
    validate_manifest(&manifest)?;

    sync_payload(&payload, &manifest.members)?;
    let manifest_bytes = canonical_json(&manifest)?;
    write_private_file(&staging.path.join(MANIFEST_NAME), &manifest_bytes)?;
    let marker = CompletionMarker {
        kind: BackupFormat::Current.completion_kind().to_owned(),
        generation: manifest.generation,
        manifest_sha256: sha256_bytes_hex(&manifest_bytes),
    };
    let marker_bytes = canonical_json(&marker)?;
    write_private_file(&staging.path.join(COMPLETION_NAME), &marker_bytes)?;
    let (sealed_manifest, sealed_manifest_bytes, sealed_marker) =
        read_backup_envelope(&staging.path)?;
    if sealed_manifest != manifest
        || sealed_manifest_bytes != manifest_bytes
        || sealed_marker != marker
    {
        return Err(invalid(
            "published data-backup envelope differs from the staged metadata",
        ));
    }
    validate_backup_tree(
        &staging.path,
        &sealed_manifest,
        &sealed_manifest_bytes,
        &sealed_marker,
    )?;
    sync_directory(&staging.path)?;

    let report = report("data_backup_created", &destination, &manifest)?;
    staging.publish(&destination)?;
    Ok(report)
}

/// The format of a backup offered to `verify-backup` or `restore-data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupSourceFormat {
    /// One authenticated, encrypted bundle file (the default).
    Encrypted,
    /// The older plaintext directory generation, named explicitly.
    PlaintextDirectory,
}

/// Admits a recovery source before any password is read or anything is
/// written. An encrypted bundle is the default; a directory is the older
/// plaintext format, whose completion marker is an unkeyed digest, so it is
/// refused unless the operator names that format (`plaintext_directory`).
/// Naming it for a source that is not a directory is refused too, so the flag
/// can never downgrade an encrypted bundle.
pub fn admit_recovery_source(
    source: &Path,
    plaintext_directory: bool,
) -> Result<BackupSourceFormat, DataRecoveryError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| io_error("inspecting backup source", source, error))?;
    match (metadata.file_type().is_dir(), plaintext_directory) {
        (true, true) => Ok(BackupSourceFormat::PlaintextDirectory),
        (true, false) => Err(DataRecoveryError::PlaintextDirectoryRefused {
            path: source.to_path_buf(),
        }),
        (false, true) => Err(invalid(
            "--plaintext-directory names an older directory backup, and this source is not a directory",
        )),
        (false, false) => Ok(BackupSourceFormat::Encrypted),
    }
}

/// Verify a completed backup without opening any writable Hub connection or
/// calling `HubStore::initialize`.
pub fn verify_data_backup(source: &Path) -> Result<DataRecoveryReport, DataRecoveryError> {
    let source = resolve_existing_private_directory(source, "data-backup source")?;
    let (manifest, manifest_bytes, marker) = read_backup_envelope(&source)?;
    validate_backup_tree(&source, &manifest, &manifest_bytes, &marker)?;

    let data = source.join(DATA_DIRECTORY);
    let installation_id = immutable_database_identity_at_schema(&data, manifest.hub_schema)?;
    if installation_id != manifest.installation_id {
        return Err(invalid(
            "manifest installation ID does not match the immutable catalogue",
        ));
    }
    if manifest.hub_schema == SCHEMA_VERSION {
        let store = HubStore::open_immutable_read_only(&data)?;
        store.catalogue_check()?;
        store.verify_immutable_snapshot_unchanged()?;
    }
    report("data_backup_verified", &source, &manifest)
}

/// Restore Hub data into a new private directory. Collector keys, TLS,
/// configuration, and service state remain absent, so the restored collector
/// cannot start until credentials are explicitly recovered or replaced.
pub fn restore_data_backup(
    source: &Path,
    destination: &Path,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    let source = resolve_existing_private_directory(source, "data-backup source")?;
    let (destination, parent) = prepare_new_destination(destination)?;
    if parent.starts_with(&source) {
        return Err(invalid(
            "restore destination must not be inside the source backup generation",
        ));
    }

    let (manifest, manifest_bytes, marker) = read_backup_envelope(&source)?;
    validate_backup_tree(&source, &manifest, &manifest_bytes, &marker)?;
    let source_data = source.join(DATA_DIRECTORY);
    if immutable_database_identity_at_schema(&source_data, manifest.hub_schema)?
        != manifest.installation_id
    {
        return Err(invalid(
            "manifest installation ID does not match the immutable source catalogue",
        ));
    }
    if manifest.hub_schema == SCHEMA_VERSION {
        let source_store = HubStore::open_immutable_read_only(&source_data)?;
        source_store.catalogue_check()?;
        source_store.verify_immutable_snapshot_unchanged()?;
    }

    let (member_count, payload_bytes) = manifest_totals(&manifest)?;
    admit_member_capacity(&parent, payload_bytes, member_count as u64)?;
    let staging = StagingDirectory::create(&parent)?;
    create_private_directory(&staging.path.join("packs"))?;
    create_private_directory(&staging.path.join("packs").join("sha256"))?;
    let streamed = if backup_format(&manifest)? == BackupFormat::StreamedDirectory {
        Some(streamed_directory::copy_streamed_directory(&source, &staging.path, &manifest)?)
    } else {
        if manifest
            .members
            .iter()
            .any(|member| schema_22_noop_member_filename(&member.path).is_some())
        {
            create_private_directory(&staging.path.join("packs").join("noop"))?;
        }
        if RestoredMembers::Listed(&manifest.members).has_kept() {
            create_private_directory(&staging.path.join("packs").join("physical-kept"))?;
        }
        for member in &manifest.members {
            let relative = member
                .path
                .strip_prefix("data/")
                .ok_or_else(|| invalid("backup member is outside the data payload"))?;
            let from = source.join(&member.path);
            let to = staging.path.join(relative);
            copy_verified_member(&from, &to, member)?;
        }
        None
    };

    let members = match &streamed {
        Some(streamed) => RestoredMembers::Streamed(streamed),
        None => RestoredMembers::Listed(&manifest.members),
    };
    validate_restored_data_tree(&staging.path, members, false)?;
    validate_backup_tree(&source, &manifest, &manifest_bytes, &marker)?;
    if immutable_database_identity_at_schema(&source_data, manifest.hub_schema)?
        != manifest.installation_id
    {
        return Err(invalid(
            "source catalogue installation ID changed during restore",
        ));
    }

    finalize_restore(staging, &destination, &manifest, members)
}

/// A staged restore tree: where it is reached, and how it is published.
trait StagedTree {
    /// The path every check and SQLite connection uses for the staged tree.
    fn root(&self) -> &Path;
    /// Publish the staged tree as `destination`, never replacing anything.
    fn publish_as(self, destination: &Path) -> Result<(), DataRecoveryError>;
    /// The path [`Self::root`] still leads to the staged tree.
    fn check_bound(&self) -> Result<(), DataRecoveryError> {
        Ok(())
    }
}

impl StagedTree for StagingDirectory {
    fn root(&self) -> &Path {
        &self.path
    }

    fn publish_as(self, destination: &Path) -> Result<(), DataRecoveryError> {
        self.publish(destination)
    }
}

/// The shared tail of every restore: migrate when the catalogue is older, seal
/// out host authority, re-verify the staged tree and publish it without
/// replacement. The staged tree already holds every verified member.
fn finalize_restore<S: StagedTree>(
    staging: S,
    destination: &Path,
    manifest: &BackupManifest,
    members: RestoredMembers<'_>,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    let root = staging.root().to_path_buf();
    staging.check_bound()?;
    let migrated = manifest.hub_schema != SCHEMA_VERSION;
    if migrated {
        let migrated_store = HubStore::initialize(&root)?;
        migrated_store.catalogue_check()?;
        if migrated_store.installation_id()? != manifest.installation_id {
            return Err(invalid(
                "migrated catalogue installation ID does not match the backup manifest",
            ));
        }
        drop(migrated_store);
        remove_migration_host_state(&root, members.has_noops())?;
    }
    // Legacy v3 generations can contain live pairing challenges and bearer
    // digests. Sanitize every restored catalogue after its exact source copy
    // has been verified and before the staging directory can be published.
    seal_restored_payload(&root)?;
    // Sealed and, if need be, migrated: the staged catalogue is current.
    validate_pairing_tables(&root, SCHEMA_VERSION, true)?;

    let restored_store = HubStore::open_immutable_read_only(&root)?;
    restored_store.catalogue_check()?;
    if immutable_database_identity(&root)? != manifest.installation_id {
        return Err(invalid(
            "restored catalogue installation ID does not match the backup manifest",
        ));
    }
    restored_store.verify_immutable_snapshot_unchanged()?;
    validate_restored_data_tree(&root, members, true)?;
    drop(restored_store);
    // The bundle carries no cursor key. With the tree verified against it,
    // write a fresh key into the staged tree and re-sign every stored cursor
    // the old key signed (M06-T09): the restored Hub starts its API without
    // the old host's key export, and the rename below publishes the key and
    // the re-signed data together or not at all.
    cursor_reissue::regenerate_cursor_key(&root)?;
    sync_restored_data(&root, members)?;
    staging.check_bound()?;

    let report = report(STATUS_DATA_RESTORED, destination, manifest)?;
    staging.publish_as(destination)?;
    Ok(report)
}

fn expected_protocol() -> BackupProtocol {
    BackupProtocol {
        name: PROTOCOL_NAME.to_owned(),
        snapshot: PROTOCOL_V1,
        lineage: LINEAGE_PROTOCOL_V2,
    }
}

fn excluded_host_state_strings() -> Vec<String> {
    EXCLUDED_HOST_STATE
        .iter()
        .map(|value| (*value).to_owned())
        .collect()
}

fn report(
    status: &'static str,
    path: &Path,
    manifest: &BackupManifest,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    let (member_count, total_bytes) = manifest_totals(manifest)?;
    Ok(DataRecoveryReport {
        status,
        path: path.to_path_buf(),
        generation: manifest.generation,
        installation_id: manifest.installation_id,
        member_count,
        total_bytes,
        scope: backup_format(manifest)?.scope(),
        clean_host_ready: false,
        collector_authority: "absent",
        excluded_host_state: EXCLUDED_HOST_STATE.to_vec(),
        fleet_request_counts_notice: (status == STATUS_DATA_RESTORED)
            .then_some(FLEET_REQUEST_COUNTS_NOTICE),
    })
}

/// The member count and payload bytes of a validated manifest: declared by a
/// streamed bundle (its stream is checked against them), summed from the list
/// of a listed format.
fn manifest_totals(manifest: &BackupManifest) -> Result<(usize, u64), DataRecoveryError> {
    if let (Some(count), Some(bytes)) = (manifest.member_count, manifest.payload_bytes) {
        let count = usize::try_from(count).map_err(|_| DataRecoveryError::CapacityOverflow)?;
        return Ok((count, bytes));
    }
    let bytes = manifest.members.iter().try_fold(0_u64, |total, member| {
        total
            .checked_add(member.size)
            .ok_or(DataRecoveryError::CapacityOverflow)
    })?;
    Ok((manifest.members.len(), bytes))
}

fn checked_capacity_add(total: u64, additional: u64) -> Result<u64, DataRecoveryError> {
    total
        .checked_add(additional)
        .ok_or(DataRecoveryError::CapacityOverflow)
}

fn staging_required_bytes(copy_bytes: u64) -> Result<u64, DataRecoveryError> {
    let rounded_headroom = copy_bytes
        .checked_add(COPY_CAPACITY_HEADROOM_DIVISOR - 1)
        .ok_or(DataRecoveryError::CapacityOverflow)?
        / COPY_CAPACITY_HEADROOM_DIVISOR;
    checked_capacity_add(
        checked_capacity_add(copy_bytes, rounded_headroom)?,
        COPY_CAPACITY_FIXED_HEADROOM_BYTES,
    )
}

fn admit_staging_capacity(parent: &Path, copy_bytes: u64) -> Result<(), DataRecoveryError> {
    let required = staging_required_bytes(copy_bytes)?;
    let available = available_bytes(parent)?;
    admit_known_capacity(parent, required, available)
}

/// The volume's allocation unit at `path`: every file takes whole units.
fn allocation_block_bytes(path: &Path) -> Result<u64, DataRecoveryError> {
    let stats = statvfs(path).map_err(|source| DataRecoveryError::FilesystemSpace {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(stats.f_frsize.clamp(512, 1024 * 1024))
}

/// What `members` files of `payload_bytes` in all can take on a volume that
/// allocates `block` bytes at a time: the bytes, a partly used last block
/// and the metadata of each member (REV-CX-M06-T10 finding 2).
fn member_storage_bytes(
    payload_bytes: u64,
    members: u64,
    block: u64,
) -> Result<u64, DataRecoveryError> {
    let per_member = checked_capacity_add(block, MEMBER_METADATA_BYTES)?;
    let overhead = members
        .checked_mul(per_member)
        .ok_or(DataRecoveryError::CapacityOverflow)?;
    checked_capacity_add(payload_bytes, overhead)
}

/// Admit staging `members` files of `payload_bytes` in all under `parent`.
fn admit_member_capacity(
    parent: &Path,
    payload_bytes: u64,
    members: u64,
) -> Result<(), DataRecoveryError> {
    let block = allocation_block_bytes(parent)?;
    admit_staging_capacity(parent, member_storage_bytes(payload_bytes, members, block)?)
}

/// A test seam for the free space a volume reports, and for a volume that
/// fills part-way through a run.
#[cfg(test)]
pub(crate) mod space_seam {
    use std::{
        cell::{Cell, RefCell},
        io,
        path::Path,
    };

    type Answer = Box<dyn FnMut(&Path) -> Option<u64>>;

    thread_local! {
        static AVAILABLE: RefCell<Option<Answer>> = const { RefCell::new(None) };
        static WRITABLE: Cell<Option<u64>> = const { Cell::new(None) };
        static FILLED: Cell<bool> = const { Cell::new(false) };
    }

    /// Answer free-space questions on this thread with `answer` (`None`
    /// from it, or no seam, asks the volume).
    pub(crate) fn set(answer: impl FnMut(&Path) -> Option<u64> + 'static) {
        AVAILABLE.with(|slot| *slot.borrow_mut() = Some(Box::new(answer)));
    }

    /// Clears both seams on this thread.
    pub(crate) fn clear() {
        AVAILABLE.with(|slot| *slot.borrow_mut() = None);
        fill_after(None);
    }

    pub(super) fn available(path: &Path) -> Option<u64> {
        AVAILABLE.with(|slot| slot.borrow_mut().as_mut().and_then(|answer| answer(path)))
    }

    /// From now on this thread's backup, restore and metadata writes take
    /// `bytes` more in all, as a volume with that much room left (`None`
    /// lifts the limit): a write that crosses it is short, every later one
    /// fails with ENOSPC. Free-space answers are not changed, so a run is
    /// admitted and then finds its volume full part-way.
    pub(crate) fn fill_after(bytes: Option<u64>) {
        WRITABLE.with(|slot| slot.set(bytes));
        FILLED.with(|filled| filled.set(false));
    }

    /// Whether a write found the volume full (cut short or refused) since
    /// [`fill_after`].
    pub(crate) fn filled() -> bool {
        FILLED.with(Cell::get)
    }

    /// How many of `wanted` bytes the volume takes now; ENOSPC when none.
    pub(super) fn writable(wanted: usize) -> io::Result<usize> {
        WRITABLE.with(|slot| {
            let Some(room) = slot.get() else {
                return Ok(wanted);
            };
            let taken = usize::try_from(room).map_or(wanted, |room| room.min(wanted));
            if taken < wanted {
                FILLED.with(|filled| filled.set(true));
                if taken == 0 {
                    return Err(rustix::io::Errno::NOSPC.into());
                }
            }
            slot.set(Some(room - taken as u64));
            Ok(taken)
        })
    }
}

/// A file that backups and restores write members and metadata through. In
/// tests its writes pass the space seam, which can fill the volume part-way
/// (one short write, then ENOSPC); otherwise it is the file itself.
struct VolumeFile(File);

impl Write for VolumeFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        #[cfg(test)]
        let buffer = &buffer[..space_seam::writable(buffer.len())?];
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn available_bytes(path: &Path) -> Result<u64, DataRecoveryError> {
    #[cfg(test)]
    if let Some(available) = space_seam::available(path) {
        return Ok(available);
    }
    let stats = statvfs(path).map_err(|source| DataRecoveryError::FilesystemSpace {
        path: path.to_path_buf(),
        source,
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(DataRecoveryError::CapacityOverflow)
}

/// The free space of the held directory `directory` (M06-T21): the volume
/// of the folder admitted, whatever its path names now.
fn held_available_bytes(
    directory: &backup_admission::AdmittedDirectory,
) -> Result<u64, DataRecoveryError> {
    #[cfg(test)]
    if let Some(available) = space_seam::available(&directory.path) {
        return Ok(available);
    }
    let stats = rustix::fs::fstatvfs(&directory.fd).map_err(|source| {
        DataRecoveryError::FilesystemSpace {
            path: directory.path.clone(),
            source,
        }
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(DataRecoveryError::CapacityOverflow)
}

fn admit_known_capacity(
    parent: &Path,
    required: u64,
    available: u64,
) -> Result<(), DataRecoveryError> {
    if available < required {
        return Err(DataRecoveryError::InsufficientFreeSpace {
            path: parent.to_path_buf(),
            required,
            available,
        });
    }
    Ok(())
}

fn prepare_new_destination(destination: &Path) -> Result<(PathBuf, PathBuf), DataRecoveryError> {
    let file_name = destination
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("destination must name a new directory"))?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|source| io_error("inspecting destination parent", parent, source))?;
    if !parent_metadata.file_type().is_dir() {
        return Err(invalid("destination parent must be a real directory"));
    }
    let parent = parent
        .canonicalize()
        .map_err(|source| io_error("resolving destination parent", parent, source))?;
    let destination = parent.join(file_name);
    require_absent(&destination)?;
    Ok((destination, parent))
}

fn require_absent(path: &Path) -> Result<(), DataRecoveryError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(invalid(format!(
            "destination already exists: {}",
            path.display()
        ))),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspecting destination", path, source)),
    }
}

fn resolve_existing_private_directory(
    path: &Path,
    description: &'static str,
) -> Result<PathBuf, DataRecoveryError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspecting data-backup directory", path, source))?;
    if !metadata.file_type().is_dir() {
        return Err(invalid(format!("{description} must be a real directory")));
    }
    if permission_mode(&metadata) != PRIVATE_DIRECTORY_MODE {
        return Err(invalid(format!("{description} must have mode 0700")));
    }
    path.canonicalize()
        .map_err(|source| io_error("resolving data-backup directory", path, source))
}

fn protect_payload_tree(payload: &Path) -> Result<(), DataRecoveryError> {
    let packs = payload.join("packs");
    let sha_directory = packs.join("sha256");
    let noop_directory = packs.join("noop");
    let expected_noops = current_schema_22_noop_pairs(payload)?;
    require_real_directory(payload, "backup data payload")?;
    require_real_directory(&packs, "backup packs directory")?;
    require_real_directory(&sha_directory, "backup SHA-256 pack directory")?;
    set_mode(
        payload,
        PRIVATE_DIRECTORY_MODE,
        "protecting backup data payload",
    )?;
    set_mode(
        &packs,
        PRIVATE_DIRECTORY_MODE,
        "protecting backup packs directory",
    )?;
    set_mode(
        &sha_directory,
        PRIVATE_DIRECTORY_MODE,
        "protecting backup SHA-256 pack directory",
    )?;
    let has_kept = !expected_kept_files(payload)?.is_empty();
    if expected_noops.is_empty() {
        require_packs_entries(payload, false, has_kept, "backup packs directory")?;
    } else {
        require_real_directory(&noop_directory, "backup schema 2.2 no-op directory")?;
        set_mode(
            &noop_directory,
            PRIVATE_DIRECTORY_MODE,
            "protecting backup schema 2.2 no-op directory",
        )?;
        require_packs_entries(payload, true, has_kept, "backup packs directory")?;
        for name in expected_noops.keys() {
            let path = noop_directory.join(name);
            let metadata = require_regular_file(&path, "backup schema 2.2 no-op")?;
            if metadata.nlink() != 1 {
                return Err(invalid(format!(
                    "backup schema 2.2 no-op has multiple links: {name}"
                )));
            }
            set_mode(
                &path,
                PRIVATE_FILE_MODE,
                "protecting backup schema 2.2 no-op",
            )?;
        }
        validate_schema_22_noop_directory(payload, &expected_noops)?;
    }
    protect_kept_directory(payload)?;

    let catalogue = payload.join("hub.sqlite");
    require_regular_file(&catalogue, "backup catalogue")?;
    set_mode(&catalogue, PRIVATE_FILE_MODE, "protecting backup catalogue")?;
    // One entry at a time: the pack directory has no entry bound.
    for entry in fs::read_dir(&sha_directory)
        .map_err(|source| io_error("reading backup pack directory", &sha_directory, source))?
    {
        let entry = entry
            .map_err(|source| io_error("reading backup pack directory entry", &sha_directory, source))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("backup pack filename is not UTF-8"))?;
        if !canonical_pack_filename(&name) {
            return Err(invalid(format!("unexpected backup pack member: {name}")));
        }
        let path = entry.path();
        require_regular_file(&path, "backup pack")?;
        set_mode(&path, PRIVATE_FILE_MODE, "protecting backup pack")?;
    }
    Ok(())
}

fn seal_staged_catalogue(payload: &Path) -> Result<(), DataRecoveryError> {
    // The wall clock is sampled once: it is the creation time of the stop gap
    // that the sealing writes (C3 R8). The gap itself starts at the retained
    // lease's heartbeat, whatever this time is.
    seal_staged_catalogue_at(payload, current_epoch_ms()?)
}

/// Remove host-local authority from a copied or restored catalogue, on its own
/// connection (a staged file, or the encrypted backup's in-memory copy).
fn seal_host_authority(
    connection: &mut Connection,
    sealed_at_ms: i64,
) -> Result<(), DataRecoveryError> {
    // Collector leases and pairing credentials are host-local authority, never
    // recoverable data. Remove them before a copied catalogue is inventoried or
    // a restored catalogue is published. Installation/account/vehicle identity
    // and immutable sync lineage remain untouched.
    // The grant tables go first and explicitly: the foreign keys would cascade
    // from their parents, but the pairing authority must not depend on
    // `foreign_keys` being on (the pragma is a no-op inside a transaction, so
    // it precedes BEGIN).
    // A retained lease is evidence of a silence, so it becomes a stop gap in
    // the same transaction that removes it (C3 R8): the gap and the removal of
    // every host authority commit together or not at all, and the copy's first
    // collector start closes the gap instead of recording a second downtime.
    // `secure_delete` zeroes the removed bearer digests and sealed successors
    // in place; the vacuum that follows every seal drops the free pages.
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    transaction
        .execute_batch(
            "DELETE FROM device_vehicle_grants;
             DELETE FROM pairing_challenge_vehicles;
             DELETE FROM paired_device_token_grace;
             DELETE FROM pairing_challenges;
             DELETE FROM paired_devices;",
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    HubStore::seal_collector_lease_gap_in_transaction(&transaction, sealed_at_ms)?;
    transaction
        .execute("DELETE FROM supervised_collector_lease", [])
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    // A running encrypted backup's pack pins and recorded runs are live-host
    // state too; both tables are part of the schema (M06-T07) and stay, empty.
    encrypted_backup::clear_pack_pins(&transaction)
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    transaction
        .commit()
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    Ok(())
}

/// M06-T02: rebuild a sealed catalogue so that no free page or unused cell
/// space keeps bytes of a row deleted earlier, in the live store or by the
/// seal (an old refresh-token envelope, a bearer digest, a sealed successor).
/// The backup API and a file copy carry free pages verbatim; `VACUUM` writes
/// every page afresh and leaves no freelist.
fn vacuum_sealed_catalogue(connection: &Connection) -> Result<(), DataRecoveryError> {
    connection
        .execute_batch("VACUUM;")
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    let free_pages: i64 = connection
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if free_pages != 0 {
        return Err(invalid("the sealed catalogue still holds free pages"));
    }
    Ok(())
}

/// M06-T07: the vacuum of a staged catalogue (a directory backup or a
/// restore) writes the rebuilt file beside it with `VACUUM INTO` and then
/// replaces it. An in-place `VACUUM` would build the whole catalogue a second
/// time in a temporary database, in memory or in the system's temporary
/// folder, outside the staging directory; `VACUUM INTO` needs neither, so a
/// catalogue of any size stays within the Hub's memory peak and its plaintext
/// stays where the staged data already is.
fn rebuild_staged_catalogue(
    catalogue: &Path,
    connection: Connection,
) -> Result<Connection, DataRecoveryError> {
    let mut rebuilt_name = catalogue.as_os_str().to_os_string();
    rebuilt_name.push(".rebuilt");
    let rebuilt = PathBuf::from(rebuilt_name);
    // VACUUM INTO accepts an empty file: created private, never replaced.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(&rebuilt)
        .map_err(|source| io_error("creating the rebuilt staged catalogue", &rebuilt, source))?;
    let target = rebuilt
        .to_str()
        .ok_or_else(|| invalid("staged backup catalogue path is not UTF-8"))?;
    let result = connection
        .execute("VACUUM INTO ?1", [target])
        .map_err(DataRecoveryError::CatalogueIdentity);
    drop(connection);
    let replace = result.and_then(|_| {
        // The staged file's own side files went with its last connection; one
        // left behind would pair with the rebuilt file, so it stops the seal.
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut transient = catalogue.as_os_str().to_os_string();
            transient.push(suffix);
            if fs::symlink_metadata(PathBuf::from(transient)).is_ok() {
                return Err(invalid(
                    "staged backup catalogue kept SQLite side files before its rebuild",
                ));
            }
        }
        fs::rename(&rebuilt, catalogue)
            .map_err(|source| io_error("replacing the staged catalogue", catalogue, source))
    });
    if let Err(error) = replace {
        let _ = fs::remove_file(&rebuilt);
        return Err(error);
    }
    let connection = Connection::open_with_flags(
        catalogue,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(DataRecoveryError::CatalogueIdentity)?;
    let free_pages: i64 = connection
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if free_pages != 0 {
        return Err(invalid("the sealed catalogue still holds free pages"));
    }
    Ok(connection)
}

fn seal_staged_catalogue_at(payload: &Path, sealed_at_ms: i64) -> Result<(), DataRecoveryError> {
    let catalogue = payload.join("hub.sqlite");
    require_regular_file(&catalogue, "staged backup catalogue")?;
    let mut connection = Connection::open_with_flags(
        &catalogue,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(DataRecoveryError::CatalogueIdentity)?;
    seal_host_authority(&mut connection, sealed_at_ms)?;
    let connection = rebuild_staged_catalogue(&catalogue, connection)?;
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if journal_mode != "delete" {
        return Err(invalid(
            "staged backup catalogue could not be sealed out of WAL mode",
        ));
    }
    connection
        .execute_batch("PRAGMA synchronous = FULL;")
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    drop(connection);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut transient = catalogue.as_os_str().to_os_string();
        transient.push(suffix);
        let transient = PathBuf::from(transient);
        match fs::symlink_metadata(&transient) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(invalid(format!(
                    "staged backup catalogue retained transient SQLite state: {}",
                    transient.display()
                )));
            }
            Err(source) => {
                return Err(io_error(
                    "inspecting staged SQLite transient state",
                    &transient,
                    source,
                ));
            }
        }
    }
    Ok(())
}

fn inventory_payload(payload: &Path) -> Result<Vec<BackupMember>, DataRecoveryError> {
    let expected_noops = current_schema_22_noop_pairs(payload)?;
    validate_schema_22_noop_directory(payload, &expected_noops)?;
    let mut members = vec![inventory_file(
        &payload.join("hub.sqlite"),
        CATALOGUE_MEMBER.to_owned(),
    )?];
    for entry in read_directory(&payload.join("packs").join("sha256"))? {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("backup pack filename is not UTF-8"))?;
        if !canonical_pack_filename(&name) {
            return Err(invalid(format!("unexpected backup pack member: {name}")));
        }
        members.push(inventory_file(
            &entry.path(),
            format!("{PACK_DIRECTORY}/{name}"),
        )?);
        if members.len() > LISTED_FORMAT_MAX_MEMBERS {
            return Err(invalid("backup contains too many members"));
        }
    }
    for name in expected_noops.keys() {
        members.push(inventory_file(
            &payload.join("packs").join("noop").join(name),
            format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}"),
        )?);
        if members.len() > LISTED_FORMAT_MAX_MEMBERS {
            return Err(invalid("backup contains too many members"));
        }
    }
    members.extend(inventory_kept_members(payload)?);
    if members.len() > LISTED_FORMAT_MAX_MEMBERS {
        return Err(invalid("backup contains too many members"));
    }
    members.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(members)
}

fn inventory_file(path: &Path, member_path: String) -> Result<BackupMember, DataRecoveryError> {
    let metadata = require_regular_file(path, "backup member")?;
    Ok(BackupMember {
        path: member_path,
        size: metadata.len(),
        mode: permission_mode(&metadata),
        sha256: sha256_file_hex(path)?,
    })
}

fn backup_format(manifest: &BackupManifest) -> Result<BackupFormat, DataRecoveryError> {
    match (manifest.kind.as_str(), manifest.scope.as_str()) {
        (BACKUP_KIND, BACKUP_SCOPE) => Ok(BackupFormat::Current),
        (STREAMED_BACKUP_KIND, BACKUP_SCOPE) => Ok(BackupFormat::Streamed),
        (STREAMED_DIRECTORY_KIND, BACKUP_SCOPE) => Ok(BackupFormat::StreamedDirectory),
        (LEGACY_BACKUP_KIND, LEGACY_BACKUP_SCOPE) => Ok(BackupFormat::LegacyV3),
        _ => Err(invalid("unsupported data-backup kind or scope")),
    }
}

fn validate_manifest(manifest: &BackupManifest) -> Result<(), DataRecoveryError> {
    let format = backup_format(manifest)?;
    if manifest.generation.is_nil() {
        return Err(invalid("data-backup generation is nil"));
    }
    if manifest.created_at_ms < 0 {
        return Err(invalid("data-backup timestamp is negative"));
    }
    if manifest.build.is_empty()
        || manifest.build.len() > 128
        || !manifest
            .build
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".+-_".contains(&byte))
    {
        return Err(invalid("data-backup build identifier is invalid"));
    }
    if !(MIN_RESTORABLE_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&manifest.hub_schema) {
        let guidance = if manifest.hub_schema < MIN_RESTORABLE_SCHEMA_VERSION {
            "; re-import from the source into a new store"
        } else {
            ""
        };
        return Err(invalid(format!(
            "data-backup schema {} is outside the supported restore range {} through {}{}",
            manifest.hub_schema, MIN_RESTORABLE_SCHEMA_VERSION, SCHEMA_VERSION, guidance
        )));
    }
    if manifest.protocol != expected_protocol() {
        return Err(invalid("data-backup protocol declaration is unsupported"));
    }
    if manifest.installation_id.is_nil() {
        return Err(invalid("data-backup installation ID is nil"));
    }
    if manifest.excluded_host_state != excluded_host_state_strings() {
        return Err(invalid(
            "data-backup excluded-host-state declaration changed",
        ));
    }
    if format == BackupFormat::Streamed {
        // The stream carries the members; the manifest declares their count
        // and bytes, which the reader checks before it trusts the trailer.
        if !manifest.members.is_empty() {
            return Err(invalid("a streamed data-backup manifest lists no members"));
        }
        if manifest.member_count.is_none_or(|count| count == 0) || manifest.payload_bytes.is_none()
        {
            return Err(invalid(
                "a streamed data-backup manifest must declare its member count and bytes",
            ));
        }
        return Ok(());
    }
    if format == BackupFormat::StreamedDirectory {
        // The tree carries the members; the manifest names the catalogue
        // member (its digest binds the catalogue) and declares the totals.
        let [catalogue] = manifest.members.as_slice() else {
            return Err(invalid(
                "a streamed directory manifest names exactly its catalogue member",
            ));
        };
        if validate_member(catalogue)? != MemberKind::Catalogue {
            return Err(invalid(
                "a streamed directory manifest names exactly its catalogue member",
            ));
        }
        if manifest.member_count.is_none_or(|count| count == 0)
            || manifest.payload_bytes.is_none_or(|bytes| bytes < catalogue.size)
        {
            return Err(invalid(
                "a streamed directory manifest must declare its member count and bytes",
            ));
        }
        return Ok(());
    }
    if manifest.member_count.is_some() || manifest.payload_bytes.is_some() {
        return Err(invalid(
            "a listed data-backup manifest must not declare streamed totals",
        ));
    }
    if manifest.members.is_empty() || manifest.members.len() > LISTED_FORMAT_MAX_MEMBERS {
        return Err(invalid(
            "data-backup member count is outside the supported bound",
        ));
    }
    if manifest
        .members
        .windows(2)
        .any(|pair| pair[0].path >= pair[1].path)
    {
        return Err(invalid(
            "data-backup members are not uniquely sorted by canonical path",
        ));
    }

    let mut catalogue_count = 0_usize;
    let mut total_bytes = 0_u64;
    for member in &manifest.members {
        total_bytes = total_bytes
            .checked_add(member.size)
            .ok_or_else(|| invalid("data-backup member byte total overflowed"))?;
        if validate_member(member)? == MemberKind::Catalogue {
            catalogue_count += 1;
        }
    }
    if catalogue_count != 1 {
        return Err(invalid(
            "data-backup must contain exactly one Hub catalogue",
        ));
    }
    Ok(())
}

/// Check one member on its own: private mode, a SHA-256 digest and a
/// canonical path (the catalogue, a bounded schema 2.2 no-op, or a pack named
/// by its digest). A listed manifest checks each entry with it; a streamed
/// bundle checks each record as it arrives.
fn validate_member(member: &BackupMember) -> Result<MemberKind, DataRecoveryError> {
    if member.mode != PRIVATE_FILE_MODE {
        return Err(invalid(format!(
            "data-backup member {} is not private mode 0600",
            member.path
        )));
    }
    validate_sha256(&member.sha256)?;
    if member.path == CATALOGUE_MEMBER {
        return Ok(MemberKind::Catalogue);
    }
    if schema_22_noop_member_filename(&member.path).is_some() {
        if member.size == 0 || member.size > MAX_SCHEMA_22_NOOP_BYTES {
            return Err(invalid(format!(
                "schema 2.2 no-op member size is outside its bound: {}",
                member.path
            )));
        }
        return Ok(MemberKind::Noop);
    }
    if kept_member_filename(&member.path).is_some() {
        return Ok(MemberKind::Kept);
    }
    let filename = member
        .path
        .strip_prefix(&format!("{PACK_DIRECTORY}/"))
        .ok_or_else(|| invalid(format!("unsafe data-backup member path: {}", member.path)))?;
    if filename.contains('/') || !canonical_pack_filename(filename) {
        return Err(invalid(format!(
            "unsafe data-backup pack path: {}",
            member.path
        )));
    }
    let path_digest = filename
        .strip_suffix(".sqlite.zst")
        .ok_or_else(|| invalid("backup pack suffix is invalid"))?;
    if path_digest != member.sha256 {
        return Err(invalid(format!(
            "backup pack path digest does not match member digest: {}",
            member.path
        )));
    }
    Ok(MemberKind::Pack)
}

fn read_backup_envelope(
    source: &Path,
) -> Result<(BackupManifest, Vec<u8>, CompletionMarker), DataRecoveryError> {
    let present = |name: &str| match fs::symlink_metadata(source.join(name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(
            "inspecting data-backup manifest",
            source.join(name),
            error,
        )),
    };
    let manifest_name = if present(STREAMED_DIRECTORY_MANIFEST_NAME)? {
        STREAMED_DIRECTORY_MANIFEST_NAME
    } else if present(MANIFEST_NAME)? {
        MANIFEST_NAME
    } else {
        LEGACY_MANIFEST_NAME
    };
    require_directory_entries(
        source,
        [manifest_name, COMPLETION_NAME, DATA_DIRECTORY],
        "data-backup root",
    )?;
    let manifest_path = source.join(manifest_name);
    let manifest_bytes = read_bounded_private_file(&manifest_path, MAX_MANIFEST_BYTES)?;
    let manifest: BackupManifest = parse_canonical_json(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    let format = backup_format(&manifest)?;
    if format == BackupFormat::Streamed {
        return Err(invalid(
            "a streamed manifest belongs in an encrypted bundle, not a directory backup",
        ));
    }
    if manifest_name != format.manifest_name() {
        return Err(invalid(
            "data-backup manifest filename does not match its format",
        ));
    }

    let marker_path = source.join(COMPLETION_NAME);
    let marker_bytes = read_bounded_private_file(&marker_path, MAX_COMPLETION_BYTES)?;
    let marker: CompletionMarker = parse_canonical_json(&marker_bytes)?;
    if marker.kind != format.completion_kind()
        || marker.generation != manifest.generation
        || marker.manifest_sha256 != sha256_bytes_hex(&manifest_bytes)
    {
        return Err(invalid(
            "data-backup completion marker does not bind the exact manifest",
        ));
    }
    validate_sha256(&marker.manifest_sha256)?;
    Ok((manifest, manifest_bytes, marker))
}

fn validate_backup_tree(
    root: &Path,
    manifest: &BackupManifest,
    manifest_bytes: &[u8],
    marker: &CompletionMarker,
) -> Result<(), DataRecoveryError> {
    validate_manifest(manifest)?;
    let format = backup_format(manifest)?;
    if format == BackupFormat::Streamed {
        return Err(invalid(
            "a streamed manifest belongs in an encrypted bundle, not a directory backup",
        ));
    }
    if marker.kind != format.completion_kind()
        || marker.generation != manifest.generation
        || marker.manifest_sha256 != sha256_bytes_hex(manifest_bytes)
    {
        return Err(invalid(
            "completion marker does not match data-backup manifest",
        ));
    }
    require_directory_entries(
        root,
        [format.manifest_name(), COMPLETION_NAME, DATA_DIRECTORY],
        "data-backup root",
    )?;
    let data = root.join(DATA_DIRECTORY);
    require_directory_entries(&data, ["hub.sqlite", "packs"], "data-backup payload")?;
    // Both formats must hold every pairing table their declared schema has,
    // or a restore would fail when it empties them. Only a Current backup must
    // also hold no authority: a legacy v3 generation may carry live pairing
    // rows, which the restore revokes before it publishes anything.
    validate_pairing_tables(
        &data,
        manifest.hub_schema,
        matches!(format, BackupFormat::Current | BackupFormat::StreamedDirectory),
    )?;
    if format == BackupFormat::StreamedDirectory {
        return streamed_directory::validate_streamed_directory_payload(&data, manifest);
    }
    let expected_noops = current_schema_22_noop_pairs(&data)?;
    let expected_kept = expected_kept_files(&data)?;
    require_packs_entries(
        &data,
        !expected_noops.is_empty(),
        !expected_kept.is_empty(),
        "data-backup packs directory",
    )?;
    validate_listed_kept_members(&data, &manifest.members, &expected_kept)?;

    let expected_packs: BTreeSet<String> = manifest
        .members
        .iter()
        .filter_map(|member| {
            member
                .path
                .strip_prefix(&format!("{PACK_DIRECTORY}/"))
                .map(str::to_owned)
        })
        .collect();
    require_directory_entry_set(
        &data.join("packs").join("sha256"),
        &expected_packs,
        "data-backup SHA-256 pack directory",
    )?;
    let declared_noops: BTreeSet<String> = manifest
        .members
        .iter()
        .filter_map(|member| schema_22_noop_member_filename(&member.path).map(str::to_owned))
        .collect();
    let expected_noop_names: BTreeSet<String> = expected_noops.keys().cloned().collect();
    if declared_noops != expected_noop_names {
        return Err(invalid(
            "data-backup schema 2.2 no-op members do not match the current catalogue",
        ));
    }
    validate_schema_22_noop_directory(&data, &expected_noops)?;
    for member in &manifest.members {
        let path = root.join(&member.path);
        let metadata = require_regular_file(&path, "data-backup member")?;
        if permission_mode(&metadata) != member.mode {
            return Err(invalid(format!(
                "data-backup member mode changed: {}",
                member.path
            )));
        }
        if metadata.len() != member.size {
            return Err(invalid(format!(
                "data-backup member size changed: {}",
                member.path
            )));
        }
        if sha256_file_hex(&path)? != member.sha256 {
            return Err(invalid(format!(
                "data-backup member digest changed: {}",
                member.path
            )));
        }
    }
    Ok(())
}

fn validate_restored_data_tree(
    root: &Path,
    members: RestoredMembers<'_>,
    catalogue_migrated: bool,
) -> Result<(), DataRecoveryError> {
    require_directory_entries(root, ["hub.sqlite", "packs"], "restored data root")?;
    let expected_noops = current_schema_22_noop_pairs(root)?;
    let expected_kept = expected_kept_files(root)?;
    require_packs_entries(
        root,
        !expected_noops.is_empty(),
        !expected_kept.is_empty(),
        "restored packs directory",
    )?;
    let members = match members {
        RestoredMembers::Listed(members) => members,
        RestoredMembers::Streamed(streamed) => {
            if expected_kept.len() as u64 != streamed.kept {
                return Err(invalid(
                    "restored kept archive members do not match the catalogue",
                ));
            }
            validate_kept_directory(root, &expected_kept)?;
            return validate_streamed_data_tree(
                root,
                streamed,
                &expected_noops,
                catalogue_migrated,
            );
        }
    };
    validate_listed_kept_members(root, members, &expected_kept)?;
    let expected_packs: BTreeSet<String> = members
        .iter()
        .filter_map(|member| {
            member
                .path
                .strip_prefix(&format!("{PACK_DIRECTORY}/"))
                .map(str::to_owned)
        })
        .collect();
    require_directory_entry_set(
        &root.join("packs").join("sha256"),
        &expected_packs,
        "restored SHA-256 pack directory",
    )?;
    let declared_noops: BTreeSet<String> = members
        .iter()
        .filter_map(|member| schema_22_noop_member_filename(&member.path).map(str::to_owned))
        .collect();
    let expected_noop_names: BTreeSet<String> = expected_noops.keys().cloned().collect();
    if declared_noops != expected_noop_names {
        return Err(invalid(
            "restored schema 2.2 no-op members do not match the current catalogue",
        ));
    }
    validate_schema_22_noop_directory(root, &expected_noops)?;
    for member in members {
        let relative = member
            .path
            .strip_prefix("data/")
            .ok_or_else(|| invalid("restored member is outside the data scope"))?;
        let path = root.join(relative);
        let metadata = require_regular_file(&path, "restored data member")?;
        let content_changed = member.path == CATALOGUE_MEMBER && catalogue_migrated;
        if permission_mode(&metadata) != member.mode
            || (!content_changed
                && (metadata.len() != member.size || sha256_file_hex(&path)? != member.sha256))
        {
            return Err(invalid(format!(
                "restored data member differs from its manifest: {}",
                member.path
            )));
        }
    }
    Ok(())
}

/// The staged tree of a streamed bundle, checked without its member list
/// (M06-T10): the pack directory is read one entry at a time, each pack must
/// be a private regular file whose content digest is its name, and their
/// number must be the streamed one; the no-ops must be exactly the current
/// catalogue's, as many as streamed; the catalogue must match its streamed
/// size and digest unless the restore has since changed it.
fn validate_streamed_data_tree(
    root: &Path,
    streamed: &StreamedMembers,
    expected_noops: &BTreeMap<String, Uuid>,
    catalogue_migrated: bool,
) -> Result<(), DataRecoveryError> {
    if expected_noops.len() as u64 != streamed.noops {
        return Err(invalid(
            "restored schema 2.2 no-op members do not match the current catalogue",
        ));
    }
    validate_schema_22_noop_directory(root, expected_noops)?;
    let packs = root.join("packs").join("sha256");
    let metadata = require_real_directory(&packs, "restored SHA-256 pack directory")?;
    if permission_mode(&metadata) != PRIVATE_DIRECTORY_MODE {
        return Err(invalid(
            "restored SHA-256 pack directory must have mode 0700",
        ));
    }
    let entries = fs::read_dir(&packs)
        .map_err(|source| io_error("reading restored pack directory", &packs, source))?;
    let mut count = 0_u64;
    for entry in entries {
        let entry =
            entry.map_err(|source| io_error("reading restored pack entry", &packs, source))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("restored pack directory contains a non-UTF-8 name"))?;
        if is_ignorable_finder_file(&entry.path(), &name) {
            continue;
        }
        let digest = name
            .strip_suffix(".sqlite.zst")
            .filter(|_| canonical_pack_filename(&name))
            .ok_or_else(|| invalid(format!("unexpected restored pack: {name}")))?;
        let path = entry.path();
        let metadata = require_regular_file(&path, "restored pack")?;
        if permission_mode(&metadata) != PRIVATE_FILE_MODE || sha256_file_hex(&path)? != digest {
            return Err(invalid(format!(
                "restored data member differs from its bundle: {PACK_DIRECTORY}/{name}"
            )));
        }
        count += 1;
    }
    if count != streamed.packs {
        return Err(invalid(format!(
            "restored pack directory holds {count} packs, the bundle streamed {}",
            streamed.packs
        )));
    }
    let catalogue = root.join("hub.sqlite");
    let metadata = require_regular_file(&catalogue, "restored data member")?;
    if permission_mode(&metadata) != PRIVATE_FILE_MODE
        || (!catalogue_migrated
            && (metadata.len() != streamed.catalogue.size
                || sha256_file_hex(&catalogue)? != streamed.catalogue.sha256))
    {
        return Err(invalid(format!(
            "restored data member differs from its bundle: {CATALOGUE_MEMBER}"
        )));
    }
    Ok(())
}

fn remove_migration_host_state(root: &Path, has_noops: bool) -> Result<(), DataRecoveryError> {
    let publication_lock = root.join(".publication.lock");
    let metadata = require_regular_file(&publication_lock, "migration publication lock")?;
    if permission_mode(&metadata) != PRIVATE_FILE_MODE || metadata.nlink() != 1 {
        return Err(invalid("migration publication lock has unsafe metadata"));
    }
    fs::remove_file(&publication_lock).map_err(|source| {
        io_error(
            "removing migration-only publication lock",
            &publication_lock,
            source,
        )
    })?;

    if !has_noops {
        let noop = root.join("packs").join("noop");
        require_directory_entries(&noop, [], "migration-created no-op directory")?;
        fs::remove_dir(&noop).map_err(|source| {
            io_error("removing migration-created no-op directory", &noop, source)
        })?;
    }
    Ok(())
}

fn copy_verified_member(
    source: &Path,
    destination: &Path,
    member: &BackupMember,
) -> Result<(), DataRecoveryError> {
    let source_link_metadata = require_regular_file(source, "source backup member")?;
    let mut input = open_regular_nonblocking(source, "opening source backup member")?;
    let source_file_metadata = input.metadata().map_err(|source_error| {
        io_error("inspecting opened source member", source, source_error)
    })?;
    if source_link_metadata.dev() != source_file_metadata.dev()
        || source_link_metadata.ino() != source_file_metadata.ino()
        || source_file_metadata.len() != member.size
        || permission_mode(&source_file_metadata) != member.mode
    {
        return Err(invalid(format!(
            "source backup member changed while it was opened: {}",
            member.path
        )));
    }

    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(destination)
        .map_err(|source_error| {
            io_error("creating restored data member", destination, source_error)
        })?;
    let mut output = BufWriter::with_capacity(COPY_BUFFER_BYTES, VolumeFile(output));
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = input.read(&mut buffer).map_err(|source_error| {
            io_error("reading source backup member", source, source_error)
        })?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read).map_err(|_| invalid("copy byte count overflowed"))?)
            .ok_or_else(|| invalid("copy byte count overflowed"))?;
        if copied > member.size {
            return Err(invalid(format!(
                "source backup member grew beyond its manifest size: {}",
                member.path
            )));
        }
        digest.update(&buffer[..read]);
        output.write_all(&buffer[..read]).map_err(|source_error| {
            io_error("writing restored data member", destination, source_error)
        })?;
    }
    output.flush().map_err(|source_error| {
        io_error("flushing restored data member", destination, source_error)
    })?;
    let VolumeFile(output) = output.into_inner().map_err(|error| {
        io_error(
            "finishing restored data member",
            destination,
            error.into_error(),
        )
    })?;
    output.sync_all().map_err(|source_error| {
        io_error("syncing restored data member", destination, source_error)
    })?;
    set_mode(
        destination,
        PRIVATE_FILE_MODE,
        "protecting restored data member",
    )?;
    if copied != member.size || hex::encode(digest.finalize()) != member.sha256 {
        return Err(invalid(format!(
            "copied data member does not match its manifest: {}",
            member.path
        )));
    }
    Ok(())
}

fn sync_payload(payload: &Path, members: &[BackupMember]) -> Result<(), DataRecoveryError> {
    for member in members {
        let relative = member
            .path
            .strip_prefix("data/")
            .ok_or_else(|| invalid("backup payload member is outside the data scope"))?;
        sync_file(&payload.join(relative))?;
    }
    sync_directory(&payload.join("packs").join("sha256"))?;
    if members
        .iter()
        .any(|member| schema_22_noop_member_filename(&member.path).is_some())
    {
        sync_directory(&payload.join("packs").join("noop"))?;
    }
    sync_kept_directory(payload, RestoredMembers::Listed(members).has_kept())?;
    sync_directory(&payload.join("packs"))?;
    sync_directory(payload)
}

fn sync_restored_data(root: &Path, members: RestoredMembers<'_>) -> Result<(), DataRecoveryError> {
    match members {
        RestoredMembers::Listed(members) => {
            for member in members {
                let relative = member
                    .path
                    .strip_prefix("data/")
                    .ok_or_else(|| invalid("restored member is outside the data scope"))?;
                sync_file(&root.join(relative))?;
            }
        }
        RestoredMembers::Streamed(_) => {
            // Each pack and no-op was synced as it was written and nothing
            // has changed it since; the catalogue was sealed and re-signed.
            sync_file(&root.join("hub.sqlite"))?;
        }
    }
    sync_directory(&root.join("packs").join("sha256"))?;
    if members.has_noops() {
        sync_directory(&root.join("packs").join("noop"))?;
    }
    sync_kept_directory(root, members.has_kept())?;
    sync_directory(&root.join("packs"))?;
    sync_directory(root)
}

fn immutable_database_identity(data: &Path) -> Result<Uuid, DataRecoveryError> {
    immutable_database_identity_at_schema(data, SCHEMA_VERSION)
}

fn immutable_database_identity_at_schema(
    data: &Path,
    expected_schema: i32,
) -> Result<Uuid, DataRecoveryError> {
    catalogue_identity(&open_immutable_catalogue(data)?, expected_schema)
}

/// Application ID, declared schema, quick-check and installation ID of a
/// catalogue on any connection (an immutable file or an in-memory copy).
fn catalogue_identity(
    connection: &Connection,
    expected_schema: i32,
) -> Result<Uuid, DataRecoveryError> {
    let application_id: i32 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if application_id != APPLICATION_ID {
        return Err(invalid(format!(
            "immutable catalogue application ID {application_id} does not match {APPLICATION_ID}",
        )));
    }
    let schema: i32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if schema != expected_schema {
        return Err(invalid(format!(
            "immutable catalogue schema {schema} does not match declared schema {expected_schema}",
        )));
    }
    let quick_check = connection
        .prepare("PRAGMA quick_check")
        .map_err(DataRecoveryError::CatalogueIdentity)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(DataRecoveryError::CatalogueIdentity)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if quick_check != ["ok"] {
        return Err(invalid("immutable catalogue quick-check failed"));
    }
    let installation_id: String = connection
        .query_row(
            "SELECT value FROM hub_metadata WHERE key = 'installation_id'",
            [],
            |row| row.get(0),
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    Uuid::parse_str(&installation_id)
        .ok()
        .filter(|value| !value.is_nil())
        .ok_or_else(|| invalid("immutable catalogue installation ID is invalid"))
}

fn open_immutable_catalogue(data: &Path) -> Result<Connection, DataRecoveryError> {
    let database = data.join("hub.sqlite");
    let canonical = database
        .canonicalize()
        .map_err(|source| io_error("resolving immutable backup catalogue", &database, source))?;
    let mut uri = url::Url::from_file_path(&canonical)
        .map_err(|()| invalid("immutable backup catalogue path is not a file URI"))?;
    uri.set_query(Some("immutable=1&mode=ro"));
    Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(DataRecoveryError::CatalogueIdentity)
}

/// Pairing authority is host-local and never survives a backup or a restore:
/// no challenge, device, rotation grace or vehicle grant. The two base tables
/// exist in every schema a backup can have (from
/// `MIN_RESTORABLE_SCHEMA_VERSION` on). The rotation grace table and the two
/// grant tables are optional only in a backup that declares a schema older
/// than [`FIRST_SCHEMA_WITH_GRANT_TABLES`]: a catalogue that declares that
/// schema or a later one without one of them is rejected here, at verification,
/// and not left to fail when the restore tries to empty it. This holds for the
/// current and the legacy v3 backup format alike. With `require_empty` every
/// table that is present must also hold no row (a Current backup, and the
/// staged catalogue after sealing); a legacy v3 backup is the one that may
/// still carry rows, which the restore deletes.
fn validate_pairing_tables(
    data: &Path,
    declared_schema: i32,
    require_empty: bool,
) -> Result<(), DataRecoveryError> {
    let connection = open_immutable_catalogue(data)?;
    for (table, in_every_schema) in [
        ("pairing_challenges", true),
        ("paired_devices", true),
        ("paired_device_token_grace", false),
        ("device_vehicle_grants", false),
        ("pairing_challenge_vehicles", false),
    ] {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(DataRecoveryError::CatalogueIdentity)?;
        if !present {
            if in_every_schema || declared_schema >= FIRST_SCHEMA_WITH_GRANT_TABLES {
                return Err(invalid(format!(
                    "data-backup catalogue declares schema {declared_schema} but lacks the {table} table"
                )));
            }
            continue;
        }
        if !require_empty {
            continue;
        }
        // The names above are this function's own constants, never input.
        let rows: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(DataRecoveryError::CatalogueIdentity)?;
        if rows != 0 {
            return Err(invalid(
                "current data-backup catalogue retains pairing authority",
            ));
        }
    }
    Ok(())
}

fn current_epoch_ms() -> Result<i64, DataRecoveryError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("system clock is before the Unix epoch"))?
        .as_millis();
    i64::try_from(millis).map_err(|_| invalid("system clock does not fit the backup timestamp"))
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, DataRecoveryError> {
    let mut bytes = serde_json::to_vec(value).map_err(DataRecoveryError::Encode)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn parse_canonical_json<T>(bytes: &[u8]) -> Result<T, DataRecoveryError>
where
    T: DeserializeOwned + Serialize,
{
    let value: T = serde_json::from_slice(bytes).map_err(DataRecoveryError::Decode)?;
    if canonical_json(&value)? != bytes {
        return Err(invalid("data-backup metadata is not canonical JSON"));
    }
    Ok(value)
}

fn read_bounded_private_file(path: &Path, maximum: u64) -> Result<Vec<u8>, DataRecoveryError> {
    let metadata = require_regular_file(path, "data-backup metadata")?;
    if permission_mode(&metadata) != PRIVATE_FILE_MODE {
        return Err(invalid(format!(
            "data-backup metadata is not private mode 0600: {}",
            path.display()
        )));
    }
    if metadata.len() == 0 || metadata.len() > maximum {
        return Err(invalid(format!(
            "data-backup metadata size is outside its bound: {}",
            path.display()
        )));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| invalid("data-backup metadata size does not fit memory"))?;
    let mut bytes = Vec::with_capacity(capacity);
    let file = open_regular_nonblocking(path, "opening data-backup metadata")?;
    let opened_metadata = file
        .metadata()
        .map_err(|source| io_error("inspecting opened data-backup metadata", path, source))?;
    if !same_regular_file(&metadata, &opened_metadata) {
        return Err(invalid("data-backup metadata changed while it was opened"));
    }
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("reading data-backup metadata", path, source))?;
    if u64::try_from(bytes.len()).ok() != Some(metadata.len()) {
        return Err(invalid("data-backup metadata changed while it was read"));
    }
    let after = require_regular_file(path, "data-backup metadata")?;
    if !same_regular_file(&metadata, &after) {
        return Err(invalid("data-backup metadata changed while it was read"));
    }
    Ok(bytes)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), DataRecoveryError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|source| io_error("creating data-backup metadata", path, source))?;
    let mut file = VolumeFile(file);
    file.write_all(bytes)
        .map_err(|source| io_error("writing data-backup metadata", path, source))?;
    file.0
        .sync_all()
        .map_err(|source| io_error("syncing data-backup metadata", path, source))?;
    set_mode(path, PRIVATE_FILE_MODE, "protecting data-backup metadata")
}

fn sha256_file_hex(path: &Path) -> Result<String, DataRecoveryError> {
    let before = require_regular_file(path, "data-backup member being hashed")?;
    let file = open_regular_nonblocking(path, "opening data-backup member for hashing")?;
    let opened = file
        .metadata()
        .map_err(|source| io_error("inspecting opened data-backup member", path, source))?;
    if !same_regular_file(&before, &opened) {
        return Err(invalid("data-backup member changed while it was opened"));
    }
    let mut reader = BufReader::with_capacity(COPY_BUFFER_BYTES, file);
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| io_error("hashing data-backup member", path, source))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let after = require_regular_file(path, "data-backup member being hashed")?;
    if !same_regular_file(&before, &after) {
        return Err(invalid("data-backup member changed while it was hashed"));
    }
    Ok(hex::encode(digest.finalize()))
}

fn sha256_bytes_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn validate_sha256(value: &str) -> Result<(), DataRecoveryError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("data-backup SHA-256 value is not canonical"));
    }
    Ok(())
}

fn canonical_pack_filename(value: &str) -> bool {
    let Some(digest) = value.strip_suffix(".sqlite.zst") else {
        return false;
    };
    validate_sha256(digest).is_ok()
}

fn canonical_schema_22_noop_filename(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(vehicle) = parts.next() else {
        return false;
    };
    let Some(snapshot) = parts.next() else {
        return false;
    };
    if parts.next() != Some("json") || parts.next().is_some() {
        return false;
    }
    Uuid::parse_str(vehicle).is_ok_and(|value| value.to_string() == vehicle)
        && Uuid::parse_str(snapshot).is_ok_and(|value| value.to_string() == snapshot)
}

fn schema_22_noop_member_filename(path: &str) -> Option<&str> {
    let name = path.strip_prefix(&format!("{SCHEMA_22_NOOP_DIRECTORY}/"))?;
    (!name.contains('/') && canonical_schema_22_noop_filename(name)).then_some(name)
}

/// The current schema 2.2 full snapshots of the catalogue at `data`, by
/// no-op file name (`<vehicle>.<snapshot>.json`) and snapshot id.
fn current_schema_22_noop_pairs(data: &Path) -> Result<BTreeMap<String, Uuid>, DataRecoveryError> {
    current_schema_22_noop_pairs_in(&open_immutable_catalogue(data)?)
}

/// One vehicle's current full snapshot, by identity only.
#[derive(Debug, Clone, Copy)]
struct CurrentSnapshot {
    head_sequence: u64,
    snapshot_id: Uuid,
    schema_22: bool,
}

/// Select each vehicle's current full snapshot and keep those of schema 2.2
/// (REV-CX-M06-T10 finding 1): the manifest rows are read one at a time,
/// straight from SQLite's row buffer, each parsed and dropped before the next
/// is read, and only one identity per vehicle is kept. Memory is one row and
/// one entry per vehicle, never one per retained manifest; each selected
/// manifest is read again by its snapshot id where its no-op is checked
/// ([`current_noop_manifest`]).
fn current_schema_22_noop_pairs_in(
    connection: &Connection,
) -> Result<BTreeMap<String, Uuid>, DataRecoveryError> {
    let mut statement = connection
        .prepare(
            "SELECT snapshot_id, vehicle_id, head_sequence, manifest_json
             FROM sync_manifests",
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    let mut rows = statement
        .query([])
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    let mut current = BTreeMap::<Uuid, CurrentSnapshot>::new();
    while let Some(row) = rows.next().map_err(DataRecoveryError::CatalogueIdentity)? {
        let column = |index: usize| row.get_ref(index).map_err(DataRecoveryError::CatalogueIdentity);
        let unreadable = |_| invalid("backup catalogue manifest row has an unexpected type");
        let snapshot_id = column(0)?.as_str().map_err(unreadable)?;
        let vehicle_id = column(1)?.as_str().map_err(unreadable)?;
        let head_sequence = column(2)?.as_i64().map_err(unreadable)?;
        let payload = column(3)?.as_blob().map_err(unreadable)?;
        let Some(manifest) = parse_catalogue_manifest(payload)? else {
            continue;
        };
        if manifest.snapshot_id.to_string() != snapshot_id
            || manifest.vehicle_id.to_string() != vehicle_id
            || i64::try_from(manifest.head_sequence).ok() != Some(head_sequence)
        {
            return Err(invalid(
                "backup catalogue manifest row does not match its typed identity",
            ));
        }
        if manifest.mode != TransferMode::FullSnapshot {
            continue;
        }
        let candidate = CurrentSnapshot {
            head_sequence: manifest.head_sequence,
            snapshot_id: manifest.snapshot_id,
            schema_22: manifest.schema == HUB_PROJECTION_SCHEMA_V3,
        };
        match current.get(&manifest.vehicle_id) {
            Some(existing) if existing.head_sequence > candidate.head_sequence => {}
            Some(existing)
                if existing.head_sequence == candidate.head_sequence
                    && existing.snapshot_id != candidate.snapshot_id =>
            {
                return Err(invalid(
                    "backup catalogue has conflicting current full snapshots",
                ));
            }
            Some(existing) if existing.head_sequence == candidate.head_sequence => {}
            _ => {
                current.insert(manifest.vehicle_id, candidate);
            }
        }
    }
    let mut pairs = BTreeMap::new();
    for (vehicle_id, snapshot) in current {
        if !snapshot.schema_22 {
            continue;
        }
        let name = format!("{vehicle_id}.{}.json", snapshot.snapshot_id);
        if pairs.insert(name, snapshot.snapshot_id).is_some() {
            return Err(invalid("duplicate schema 2.2 no-op backup identity"));
        }
    }
    Ok(pairs)
}

/// One catalogue manifest row: a validated sync manifest, or `None` for a
/// lineage manifest or delta (which select no no-op).
fn parse_catalogue_manifest(payload: &[u8]) -> Result<Option<SyncManifest>, DataRecoveryError> {
    let manifest: SyncManifest = match serde_json::from_slice(payload) {
        Ok(manifest) => manifest,
        Err(_)
            if serde_json::from_slice::<LineageManifestV2>(payload).is_ok()
                || serde_json::from_slice::<LineageDelta>(payload).is_ok() =>
        {
            return Ok(None);
        }
        Err(error) => return Err(DataRecoveryError::Decode(error)),
    };
    manifest
        .validate()
        .map_err(|error| invalid(format!("invalid manifest in backup catalogue: {error}")))?;
    Ok(Some(manifest))
}

/// The manifest of one selected current snapshot, read by its id.
fn current_noop_manifest(
    connection: &Connection,
    snapshot_id: Uuid,
) -> Result<SyncManifest, DataRecoveryError> {
    let payload: Vec<u8> = connection
        .query_row(
            "SELECT manifest_json FROM sync_manifests WHERE snapshot_id = ?1",
            [snapshot_id.to_string()],
            |row| row.get(0),
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    match parse_catalogue_manifest(&payload)? {
        Some(manifest) if manifest.snapshot_id == snapshot_id => Ok(manifest),
        _ => Err(invalid(
            "a current schema 2.2 snapshot is missing from the backup catalogue",
        )),
    }
}

/// The no-op directory must hold exactly one body per current schema 2.2
/// snapshot of the catalogue at `data`, each checked against its manifest.
/// The directory is read one entry at a time with no entry bound
/// (REV-CX-M06-T10 finding 3): each entry must name a current snapshot,
/// whose manifest is read by id, and the count must equal the catalogue's,
/// so the correspondence is exact without listing the directory.
fn validate_schema_22_noop_directory(
    data: &Path,
    expected: &BTreeMap<String, Uuid>,
) -> Result<(), DataRecoveryError> {
    let directory = data.join("packs").join("noop");
    if expected.is_empty() {
        return match fs::symlink_metadata(&directory) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(invalid(
                "schema 2.2 no-op directory exists without a current schema 2.2 manifest",
            )),
            Err(source) => Err(io_error(
                "inspecting schema 2.2 no-op directory",
                &directory,
                source,
            )),
        };
    }
    let metadata = require_real_directory(&directory, "schema 2.2 no-op directory")?;
    if permission_mode(&metadata) != PRIVATE_DIRECTORY_MODE {
        return Err(invalid("schema 2.2 no-op directory must have mode 0700"));
    }
    let connection = open_immutable_catalogue(data)?;
    let entries = fs::read_dir(&directory)
        .map_err(|source| io_error("reading schema 2.2 no-op directory", &directory, source))?;
    let mut seen = 0_usize;
    for entry in entries {
        let entry = entry
            .map_err(|source| io_error("reading schema 2.2 no-op directory", &directory, source))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("schema 2.2 no-op directory contains a non-UTF-8 name"))?;
        if !expected.contains_key(&name) && is_ignorable_finder_file(&entry.path(), &name) {
            continue;
        }
        let Some(snapshot_id) = expected.get(&name) else {
            return Err(invalid(format!(
                "schema 2.2 no-op directory has an unknown member: {name}"
            )));
        };
        seen += 1;
        let path = directory.join(&name);
        let metadata = require_regular_file(&path, "schema 2.2 no-op member")?;
        if metadata.nlink() != 1
            || permission_mode(&metadata) != PRIVATE_FILE_MODE
            || metadata.len() == 0
            || metadata.len() > MAX_SCHEMA_22_NOOP_BYTES
        {
            return Err(invalid(format!(
                "schema 2.2 no-op member has unsafe metadata: {name}"
            )));
        }
        let bytes = read_bounded_private_file(&path, MAX_SCHEMA_22_NOOP_BYTES)?;
        let manifest = current_noop_manifest(&connection, *snapshot_id)?;
        validate_schema_22_noop_bytes(&name, &manifest, &bytes)?;
    }
    if seen != expected.len() {
        return Err(invalid(format!(
            "schema 2.2 no-op directory holds {seen} members, the catalogue has {} current",
            expected.len()
        )));
    }
    Ok(())
}

/// One schema 2.2 no-op body: bounded, canonical typed JSON, paired with its
/// current manifest.
fn validate_schema_22_noop_bytes(
    name: &str,
    manifest: &SyncManifest,
    bytes: &[u8],
) -> Result<(), DataRecoveryError> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_SCHEMA_22_NOOP_BYTES {
        return Err(invalid(format!(
            "schema 2.2 no-op member has an unsafe size: {name}"
        )));
    }
    let noop: SignedNoOpState = serde_json::from_slice(bytes).map_err(DataRecoveryError::Decode)?;
    if serde_json::to_vec(&noop).map_err(DataRecoveryError::Encode)? != bytes {
        return Err(invalid(format!(
            "schema 2.2 no-op member is not canonical typed JSON: {name}"
        )));
    }
    validate_schema_22_pair(manifest, &noop).map_err(|error| {
        invalid(format!(
            "schema 2.2 no-op member does not match its manifest: {}",
            error.message
        ))
    })
}

fn same_regular_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.file_type().is_file()
        && right.file_type().is_file()
        && left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && permission_mode(left) == permission_mode(right)
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

fn open_regular_nonblocking(
    path: &Path,
    operation: &'static str,
) -> Result<File, DataRecoveryError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|source| io_error(operation, path, source.into()))?;
    let file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspecting opened data-backup file", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(invalid(format!(
            "opened data-backup path is not a regular file: {}",
            path.display()
        )));
    }
    Ok(file)
}

fn sync_file(path: &Path) -> Result<(), DataRecoveryError> {
    open_regular_nonblocking(path, "opening data-backup member for sync")?
        .sync_all()
        .map_err(|source| io_error("syncing data-backup member", path, source))
}

fn sync_directory(path: &Path) -> Result<(), DataRecoveryError> {
    File::open(path)
        .map_err(|source| io_error("opening data-recovery directory for sync", path, source))?
        .sync_all()
        .map_err(|source| io_error("syncing data-recovery directory", path, source))
}

fn no_replace_rename(source: &Path, destination: &Path) -> Result<(), DataRecoveryError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(|source_error| {
        io_error(
            "atomically publishing data-recovery directory without replacement",
            destination,
            source_error.into(),
        )
    })
}

fn invalid(message: impl Into<String>) -> DataRecoveryError {
    DataRecoveryError::InvalidBackup(message.into())
}

fn io_error(
    operation: &'static str,
    path: impl AsRef<Path>,
    source: io::Error,
) -> DataRecoveryError {
    DataRecoveryError::Io {
        operation,
        path: path.as_ref().to_path_buf(),
        source,
    }
}

mod archive_crypto;
mod backup_admission;
mod cursor_reissue;
mod directory_entries;
mod encrypted_backup;
mod encrypted_scratch;
mod kept_archive;
mod recovery_password;
mod streamed_directory;

use directory_entries::{
    create_private_directory, is_ignorable_finder_file, permission_mode, read_directory,
    require_directory_entries, require_directory_entry_set, require_real_directory,
    require_regular_file, set_mode,
};

use kept_archive::{
    expected_kept_files, inventory_kept_members, kept_member_filename, protect_kept_directory,
    require_packs_entries, sync_kept_directory, validate_kept_directory,
    validate_listed_kept_members,
};

pub use archive_crypto::generate_recovery_password;
pub use recovery_password::{
    RECOVERY_PASSWORD_MAX_BYTES, read_recovery_password, recovery_password_from,
};
pub(crate) use encrypted_backup::live_backup_pack_pins;
pub use encrypted_backup::{
    EncryptedBackupReport, EncryptedBackupRequest, EncryptedBackupSpaceNeeds,
    create_encrypted_backup, encrypted_backup_space_needs, restore_encrypted_backup,
    verify_encrypted_backup,
};

#[cfg(test)]
#[path = "data_recovery/tests.rs"]
mod tests;

/// What a data backup waits for (R53 step 1); the refusal itself is
/// `StoreError::BackupWaitsForCredentialCleanup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackupWait {
    Pending,
    Blocked,
    PurgePending { needed_bytes: Option<u64> },
}

/// The backup refusal while refused pre-upgrade credentials block it
/// (M02-T11, DEC-083): it names the command that lists and removes them,
/// because a refused slot is never replaced by itself.
pub const BACKUP_BLOCKED_BY_REFUSED_CREDENTIALS: &str = "backup_blocked_by_refused_credentials: data backups are blocked by refused credentials kept from before the upgrade; list them with `tesatlas-hub purge-refused-credentials` and remove them with `tesatlas-hub purge-refused-credentials --confirm`";

/// The refusal text for a backup that waits because a refused pre-upgrade
/// slot blocks the credential cleanup; `None` for any other outcome.
pub fn refused_credential_backup_message(
    store: &HubStore,
    error: &DataRecoveryError,
) -> Option<&'static str> {
    if !matches!(
        error,
        DataRecoveryError::Store(StoreError::BackupWaitsForCredentialCleanup)
    ) {
        return None;
    }
    let state = store.credential_format_state().ok().flatten()?;
    (state.owner_api_blocked.is_some() || state.fleet_api_blocked.is_some())
        .then_some(BACKUP_BLOCKED_BY_REFUSED_CREDENTIALS)
}

pub(crate) const CAR_CONTROL_SECRET_TABLES: &[&str] = &["credential_pairs"];
pub(crate) const PRE_UPGRADE_SECRET_TABLES: &[&str] = &["legacy_owner_tokens", "fleet_tokens"];
#[cfg_attr(not(test), expect(dead_code, reason = "used by the test build only"))]
pub(crate) const CAR_CONTROL_SECRET_FILES: &[&str] = &[
    "credential-keys",
    "fleet-command-key.pem",
    "fleet-proxy-tls-key.pem",
];

pub(crate) fn credential_backup_wait(
    admission: &crate::db::CredentialBackupAdmission,
) -> Option<BackupWait> {
    use crate::db::CredentialBackupAdmission as Admission;
    match admission {
        Admission::Clean => None,
        Admission::Pending => Some(BackupWait::Pending),
        Admission::Blocked { .. } => Some(BackupWait::Blocked),
        Admission::CleanupPending { needed_bytes } => Some(BackupWait::PurgePending {
            needed_bytes: *needed_bytes,
        }),
    }
}

/// R53 step 1: the cached admission, checked before any destination write.
fn check_backup_admission(
    snapshot: &crate::db::HubBackupSnapshot<'_>,
) -> Result<(), DataRecoveryError> {
    match credential_backup_wait(&snapshot.credential_backup_admission()) {
        None => Ok(()),
        Some(_) => Err(DataRecoveryError::Store(
            StoreError::BackupWaitsForCredentialCleanup,
        )),
    }
}

/// The existing backup's own staging name: `.teslatlas-data-recovery-<UUID v4>.staging`.
fn is_backup_staging_name(name: &str) -> bool {
    let Some(inner) = name
        .strip_prefix(".teslatlas-data-recovery-")
        .and_then(|rest| rest.strip_suffix(".staging"))
    else {
        return false;
    };
    matches!(Uuid::parse_str(inner), Ok(uuid)
        if uuid.get_version_num() == 4 && uuid.hyphenated().to_string() == inner)
}

fn remove_payload_files(payload: &Path) -> io::Result<()> {
    const PAYLOAD_SQLITE_FILES: [&str; 4] = [
        "hub.sqlite",
        "hub.sqlite-wal",
        "hub.sqlite-shm",
        "hub.sqlite-journal",
    ];
    for name in PAYLOAD_SQLITE_FILES {
        let file = payload.join(name);
        // A link is never followed and never removed here (symlink metadata).
        match fs::symlink_metadata(&file) {
            Ok(metadata) if metadata.file_type().is_symlink() => continue,
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(source),
        }
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(source),
        }
    }
    match fs::remove_dir(payload) {
        Ok(()) => Ok(()),
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(source) => Err(source),
    }
}

/// R53 step 4: before the next run writes anything, the unpublished payload
/// and SQLite side files of an earlier run, found by the backup's own staging
/// names in the destination parent (never a link, never a prefix match).
fn remove_unpublished_leftovers(parent: &Path) -> Result<(), DataRecoveryError> {
    let entries = fs::read_dir(parent)
        .map_err(|source| io_error("listing the backup destination parent", parent, source))?;
    for entry in entries {
        let entry = entry
            .map_err(|source| io_error("listing the backup destination parent", parent, source))?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !is_backup_staging_name(name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| {
            io_error("inspecting an unpublished backup leftover", &path, source)
        })?;
        if !metadata.file_type().is_dir() {
            continue;
        }
        // A staging directory held locked belongs to a running backup or
        // restore (M06-T15): it is left alone. The lock is held while this
        // one is cleared.
        let Some((_lock, _)) = lock_directory(&path) else {
            continue;
        };
        // The data child is inspected without following links: a symbolic link
        // (or anything that is not a directory) is skipped, never entered.
        let payload = path.join(DATA_DIRECTORY);
        match fs::symlink_metadata(&payload) {
            Ok(child) if child.file_type().is_dir() => {
                remove_payload_files(&payload).map_err(|source| {
                    io_error("removing an unpublished backup payload", &path, source)
                })?;
            }
            Ok(_) => continue,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(io_error(
                    "inspecting an unpublished backup payload",
                    &payload,
                    source,
                ));
            }
        }
        // The staging directory goes only when nothing but empty directories is
        // left in it; any other entry stays.
        fn remove_empty_directories(directory: &Path) -> io::Result<()> {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    remove_empty_directories(&entry.path())?;
                }
            }
            match fs::remove_dir(directory) {
                Err(source) if source.kind() != io::ErrorKind::DirectoryNotEmpty => Err(source),
                _ => Ok(()),
            }
        }
        let _ = remove_empty_directories(&path);
    }
    Ok(())
}

/// R53 step 4: a failed run removes its payload and SQLite side files.
fn remove_unpublished_payload(payload: &Path) {
    let _ = remove_payload_files(payload);
}

/// R53 step 3: the copy's own state row must be credential-clean and OLD-T1
/// and OLD-T2 absent.
fn seal_backup_credentials_checked(connection: &Connection) -> Result<(), DataRecoveryError> {
    let clean: bool = connection
        .query_row(
            "SELECT converted_at_ms IS NOT NULL AND purge_required = 0
             FROM credential_format_state",
            [],
            |row| row.get(0),
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if !clean {
        return Err(invalid("staged backup catalogue is not credential-clean"));
    }
    let old_tables = PRE_UPGRADE_SECRET_TABLES
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(",");
    let present: i64 = connection
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ({old_tables})"
            ),
            [],
            |row| row.get(0),
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if present != 0 {
        return Err(invalid(
            "staged backup catalogue still holds a pre-upgrade credential table",
        ));
    }
    Ok(())
}

/// R53 step 4: with `secure_delete` on and in one transaction, the credential
/// rows go (fences, attempts and key usage stay). M06-T02: so does the
/// credential unit's status row, whose generations are digests of the owner's
/// refresh and access tokens; without it and without a pair, a restored Hub
/// is in the state of a fresh install and needs a fresh Hub login.
fn seal_backup_credentials(connection: &mut Connection) -> Result<(), DataRecoveryError> {
    connection
        .execute_batch("PRAGMA secure_delete = ON;")
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    transaction
        .execute_batch(
            "DELETE FROM credential_pairs; DELETE FROM credential_sign_ins;
             DELETE FROM collector_auth_state;",
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    transaction
        .commit()
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    Ok(())
}

/// R53 steps 3 and 4: the payload seal. The copy's own state row must be
/// credential-clean and OLD-T1 and OLD-T2 absent; then, with `secure_delete`
/// on and in one transaction, the credential rows go (fences, attempts and key
/// usage stay) before the retained seal closes the connection and checks that
/// no side file is left.
fn seal_backup_payload(payload: &Path) -> Result<(), DataRecoveryError> {
    #[cfg(test)]
    {
        use crate::auth::credential_migration::seam::{PayloadAction, payload_stage};
        if !matches!(
            payload_stage("after_copy", payload),
            PayloadAction::Continue
        ) {
            return Err(invalid("backup stopped after the copy"));
        }
    }
    let catalogue = payload.join("hub.sqlite");
    require_regular_file(&catalogue, "staged backup catalogue")?;
    let mut connection = Connection::open_with_flags(
        &catalogue,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(DataRecoveryError::CatalogueIdentity)?;
    seal_backup_credentials_checked(&connection)?;
    #[cfg(test)]
    {
        use crate::auth::credential_migration::seam::{PayloadAction, payload_stage};
        if !matches!(
            payload_stage("seal_transaction", payload),
            PayloadAction::Continue
        ) {
            return Err(invalid("backup seal transaction failed"));
        }
    }
    seal_backup_credentials(&mut connection)?;
    drop(connection);
    seal_staged_catalogue(payload)
}

/// R53 step 5: the sanitising seal of a restored payload, before its first
/// open: credential rows (and, in a generation written before this unit, the
/// OLD-T1 and OLD-T2 rows) are deleted under `secure_delete`, every pending
/// slot becomes discarded with no reason, fences and attempts stay.
fn seal_restored_payload(payload: &Path) -> Result<(), DataRecoveryError> {
    let catalogue = payload.join("hub.sqlite");
    require_regular_file(&catalogue, "restored catalogue")?;
    let mut connection = Connection::open_with_flags(
        &catalogue,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(DataRecoveryError::CatalogueIdentity)?;
    connection
        .execute_batch("PRAGMA secure_delete = ON;")
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    let exists = |name: &str| -> Result<bool, DataRecoveryError> {
        transaction
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count == 1)
            .map_err(DataRecoveryError::CatalogueIdentity)
    };
    // M06-T02: the credential unit's status row goes too, so the restored Hub
    // starts as a fresh install that needs a fresh Hub login.
    let tables = CAR_CONTROL_SECRET_TABLES
        .iter()
        .chain(["credential_sign_ins", "collector_auth_state"].iter())
        .chain(PRE_UPGRADE_SECRET_TABLES.iter());
    for table in tables {
        if exists(table)? {
            transaction
                .execute_batch(&format!("DELETE FROM {table};"))
                .map_err(DataRecoveryError::CatalogueIdentity)?;
        }
    }
    if exists("credential_format_state")? {
        transaction
            .execute_batch(
                "UPDATE credential_format_state SET owner_api_state = 'discarded',
                   owner_api_blocked = NULL WHERE owner_api_state = 'pending';
                 UPDATE credential_format_state SET fleet_api_state = 'discarded',
                   fleet_api_blocked = NULL WHERE fleet_api_state = 'pending';",
            )
            .map_err(DataRecoveryError::CatalogueIdentity)?;
    }
    transaction
        .commit()
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    drop(connection);
    seal_staged_catalogue(payload)
}
