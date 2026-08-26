//! Bounded Hub data-backup generation, immutable verification, and restore.
//!
//! A backup generation contains the Hub catalogue (including encrypted token
//! rows, but excluding pairing challenges and paired-device bearer authority),
//! every immutable pack referenced by that catalogue, and each current
//! schema-2.2 manifest's signed no-op body. Decryption and signing keys are
//! deliberately excluded. TLS/configuration/service state is also omitted, so
//! restore leaves the service stopped and collector authority absent.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};
use rustix::fs::statvfs;
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

const BACKUP_KIND: &str = "teslatlas-hub-data-backup-v4";
const COMPLETION_KIND: &str = "teslatlas-hub-data-backup-complete-v4";
const BACKUP_SCOPE: &str = "hub_data_without_pairing_or_keys";
const MANIFEST_NAME: &str = "backup-v4.json";
const LEGACY_BACKUP_KIND: &str = "teslatlas-hub-data-backup-v3";
const LEGACY_COMPLETION_KIND: &str = "teslatlas-hub-data-backup-complete-v3";
const LEGACY_BACKUP_SCOPE: &str = "hub_data_and_pairing_without_keys";
const LEGACY_MANIFEST_NAME: &str = "backup-v3.json";
const COMPLETION_NAME: &str = "BACKUP_COMPLETE";
const DATA_DIRECTORY: &str = "data";
const CATALOGUE_MEMBER: &str = "data/hub.sqlite";
const PACK_DIRECTORY: &str = "data/packs/sha256";
const SCHEMA_22_NOOP_DIRECTORY: &str = "data/packs/noop";
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const MAX_SCHEMA_22_NOOP_BYTES: u64 = 16 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_COMPLETION_BYTES: u64 = 4 * 1024;
const MAX_BACKUP_MEMBERS: usize = 4_096;
const COPY_BUFFER_BYTES: usize = 1024 * 1024;
// Admission reserves only metadata/write-amplification headroom. It does not
// allocate a second copy: the staged backup/restore remains one payload copy.
const COPY_CAPACITY_HEADROOM_DIVISOR: u64 = 20;
const COPY_CAPACITY_FIXED_HEADROOM_BYTES: u64 = 8 * 1024 * 1024;
const MIN_RESTORABLE_SCHEMA_VERSION: i32 = 52;
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
    Credential(#[from] crate::teslamate_credentials::TeslaMateCredentialError),
    #[error("could not inspect free space at {path}: {source}")]
    FilesystemSpace {
        path: PathBuf,
        #[source]
        source: rustix::io::Errno,
    },
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
    members: Vec<BackupMember>,
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
}

impl BackupFormat {
    fn manifest_name(self) -> &'static str {
        match self {
            Self::Current => MANIFEST_NAME,
            Self::LegacyV3 => LEGACY_MANIFEST_NAME,
        }
    }

    fn completion_kind(self) -> &'static str {
        match self {
            Self::Current => COMPLETION_KIND,
            Self::LegacyV3 => LEGACY_COMPLETION_KIND,
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Self::Current => BACKUP_SCOPE,
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
}

struct StagingDirectory {
    path: PathBuf,
    published: bool,
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
                    set_mode(
                        &path,
                        PRIVATE_DIRECTORY_MODE,
                        "protecting staging directory",
                    )?;
                    return Ok(Self {
                        path,
                        published: false,
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

/// Create and atomically publish one private data-backup generation.
pub fn create_data_backup(
    store: &HubStore,
    destination: &Path,
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
    admit_staging_capacity(&parent, backup_snapshot.copy_bytes()?)?;
    let staging = StagingDirectory::create(&parent)?;
    let payload = staging.path.join(DATA_DIRECTORY);
    backup_snapshot.copy_to(&payload)?;
    seal_staged_catalogue(&payload)?;
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

    admit_staging_capacity(&parent, manifest_copy_bytes(&manifest)?)?;
    let staging = StagingDirectory::create(&parent)?;
    create_private_directory(&staging.path.join("packs"))?;
    create_private_directory(&staging.path.join("packs").join("sha256"))?;
    if manifest
        .members
        .iter()
        .any(|member| schema_22_noop_member_filename(&member.path).is_some())
    {
        create_private_directory(&staging.path.join("packs").join("noop"))?;
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

    validate_restored_data_tree(&staging.path, &manifest.members, false)?;
    validate_backup_tree(&source, &manifest, &manifest_bytes, &marker)?;
    if immutable_database_identity_at_schema(&source_data, manifest.hub_schema)?
        != manifest.installation_id
    {
        return Err(invalid(
            "source catalogue installation ID changed during restore",
        ));
    }

    let migrated = manifest.hub_schema != SCHEMA_VERSION;
    if migrated {
        let migrated_store = HubStore::initialize(&staging.path)?;
        migrated_store.catalogue_check()?;
        if migrated_store.installation_id()? != manifest.installation_id {
            return Err(invalid(
                "migrated catalogue installation ID does not match the backup manifest",
            ));
        }
        drop(migrated_store);
        remove_migration_host_state(&staging.path, &manifest.members)?;
    }
    // Legacy v3 generations can contain live pairing challenges and bearer
    // digests. Sanitize every restored catalogue after its exact source copy
    // has been verified and before the staging directory can be published.
    seal_staged_catalogue(&staging.path)?;
    validate_pairing_authority_absent(&staging.path)?;

    let restored_store = HubStore::open_immutable_read_only(&staging.path)?;
    restored_store.catalogue_check()?;
    if immutable_database_identity(&staging.path)? != manifest.installation_id {
        return Err(invalid(
            "restored catalogue installation ID does not match the backup manifest",
        ));
    }
    restored_store.verify_immutable_snapshot_unchanged()?;
    validate_restored_data_tree(&staging.path, &manifest.members, true)?;
    sync_restored_data(&staging.path, &manifest.members)?;

    let report = report("data_restored", &destination, &manifest)?;
    staging.publish(&destination)?;
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
    let total_bytes = manifest.members.iter().try_fold(0_u64, |total, member| {
        total
            .checked_add(member.size)
            .ok_or_else(|| invalid("backup member byte total overflowed"))
    })?;
    Ok(DataRecoveryReport {
        status,
        path: path.to_path_buf(),
        generation: manifest.generation,
        installation_id: manifest.installation_id,
        member_count: manifest.members.len(),
        total_bytes,
        scope: backup_format(manifest)?.scope(),
        clean_host_ready: false,
        collector_authority: "absent",
        excluded_host_state: EXCLUDED_HOST_STATE.to_vec(),
    })
}

/// Calculate the bytes a completed backup can copy from a validated manifest.
/// Manifest members are checked before this is called, so the sum is bounded,
/// canonical, and cannot name symlinked data outside the backup root.
fn manifest_copy_bytes(manifest: &BackupManifest) -> Result<u64, DataRecoveryError> {
    manifest.members.iter().try_fold(0_u64, |total, member| {
        total
            .checked_add(member.size)
            .ok_or(DataRecoveryError::CapacityOverflow)
    })
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

fn available_bytes(path: &Path) -> Result<u64, DataRecoveryError> {
    let stats = statvfs(path).map_err(|source| DataRecoveryError::FilesystemSpace {
        path: path.to_path_buf(),
        source,
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
    if expected_noops.is_empty() {
        require_directory_entries(&packs, ["sha256"], "backup packs directory")?;
    } else {
        require_real_directory(&noop_directory, "backup schema 2.2 no-op directory")?;
        set_mode(
            &noop_directory,
            PRIVATE_DIRECTORY_MODE,
            "protecting backup schema 2.2 no-op directory",
        )?;
        require_directory_entries(&packs, ["noop", "sha256"], "backup packs directory")?;
        require_directory_entry_set(
            &noop_directory,
            &expected_noops.keys().cloned().collect(),
            "backup schema 2.2 no-op directory",
        )?;
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

    let catalogue = payload.join("hub.sqlite");
    require_regular_file(&catalogue, "backup catalogue")?;
    set_mode(&catalogue, PRIVATE_FILE_MODE, "protecting backup catalogue")?;
    for entry in read_directory(&sha_directory)? {
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
    let catalogue = payload.join("hub.sqlite");
    require_regular_file(&catalogue, "staged backup catalogue")?;
    let connection = Connection::open_with_flags(
        &catalogue,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(DataRecoveryError::CatalogueIdentity)?;
    // Collector leases and pairing credentials are host-local authority, never
    // recoverable data. Remove them before a copied catalogue is inventoried or
    // a restored catalogue is published. Installation/account/vehicle identity
    // and immutable sync lineage remain untouched.
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             DELETE FROM pairing_challenges;
             DELETE FROM paired_devices;
             DELETE FROM supervised_collector_lease;
             COMMIT;",
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
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
        if members.len() > MAX_BACKUP_MEMBERS {
            return Err(invalid("backup contains too many members"));
        }
    }
    for name in expected_noops.keys() {
        members.push(inventory_file(
            &payload.join("packs").join("noop").join(name),
            format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}"),
        )?);
        if members.len() > MAX_BACKUP_MEMBERS {
            return Err(invalid("backup contains too many members"));
        }
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
        (LEGACY_BACKUP_KIND, LEGACY_BACKUP_SCOPE) => Ok(BackupFormat::LegacyV3),
        _ => Err(invalid("unsupported data-backup kind or scope")),
    }
}

fn validate_manifest(manifest: &BackupManifest) -> Result<(), DataRecoveryError> {
    let _ = backup_format(manifest)?;
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
        return Err(invalid(format!(
            "data-backup schema {} is outside the supported restore range {} through {}",
            manifest.hub_schema, MIN_RESTORABLE_SCHEMA_VERSION, SCHEMA_VERSION
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
    if manifest.members.is_empty() || manifest.members.len() > MAX_BACKUP_MEMBERS {
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
        if member.mode != PRIVATE_FILE_MODE {
            return Err(invalid(format!(
                "data-backup member {} is not private mode 0600",
                member.path
            )));
        }
        validate_sha256(&member.sha256)?;
        if member.path == CATALOGUE_MEMBER {
            catalogue_count += 1;
            continue;
        }
        if schema_22_noop_member_filename(&member.path).is_some() {
            if member.size == 0 || member.size > MAX_SCHEMA_22_NOOP_BYTES {
                return Err(invalid(format!(
                    "schema 2.2 no-op member size is outside its bound: {}",
                    member.path
                )));
            }
            continue;
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
    }
    if catalogue_count != 1 {
        return Err(invalid(
            "data-backup must contain exactly one Hub catalogue",
        ));
    }
    let _ = total_bytes;
    Ok(())
}

fn read_backup_envelope(
    source: &Path,
) -> Result<(BackupManifest, Vec<u8>, CompletionMarker), DataRecoveryError> {
    let manifest_name = match fs::symlink_metadata(source.join(MANIFEST_NAME)) {
        Ok(_) => MANIFEST_NAME,
        Err(error) if error.kind() == io::ErrorKind::NotFound => LEGACY_MANIFEST_NAME,
        Err(error) => {
            return Err(io_error(
                "inspecting data-backup manifest",
                source.join(MANIFEST_NAME),
                error,
            ));
        }
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
    if format == BackupFormat::Current {
        validate_pairing_authority_absent(&data)?;
    }
    let expected_noops = current_schema_22_noop_pairs(&data)?;
    if expected_noops.is_empty() {
        require_directory_entries(
            &data.join("packs"),
            ["sha256"],
            "data-backup packs directory",
        )?;
    } else {
        require_directory_entries(
            &data.join("packs"),
            ["noop", "sha256"],
            "data-backup packs directory",
        )?;
    }

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
    members: &[BackupMember],
    catalogue_migrated: bool,
) -> Result<(), DataRecoveryError> {
    require_directory_entries(root, ["hub.sqlite", "packs"], "restored data root")?;
    let expected_noops = current_schema_22_noop_pairs(root)?;
    if expected_noops.is_empty() {
        require_directory_entries(&root.join("packs"), ["sha256"], "restored packs directory")?;
    } else {
        require_directory_entries(
            &root.join("packs"),
            ["noop", "sha256"],
            "restored packs directory",
        )?;
    }
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

fn remove_migration_host_state(
    root: &Path,
    members: &[BackupMember],
) -> Result<(), DataRecoveryError> {
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

    if !members
        .iter()
        .any(|member| schema_22_noop_member_filename(&member.path).is_some())
    {
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
    let mut input = File::open(source)
        .map_err(|source_error| io_error("opening source backup member", source, source_error))?;
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
    let mut output = BufWriter::with_capacity(COPY_BUFFER_BYTES, output);
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
        digest.update(&buffer[..read]);
        output.write_all(&buffer[..read]).map_err(|source_error| {
            io_error("writing restored data member", destination, source_error)
        })?;
    }
    output.flush().map_err(|source_error| {
        io_error("flushing restored data member", destination, source_error)
    })?;
    let output = output.into_inner().map_err(|error| {
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
    sync_directory(&payload.join("packs"))?;
    sync_directory(payload)
}

fn sync_restored_data(root: &Path, members: &[BackupMember]) -> Result<(), DataRecoveryError> {
    for member in members {
        let relative = member
            .path
            .strip_prefix("data/")
            .ok_or_else(|| invalid("restored member is outside the data scope"))?;
        sync_file(&root.join(relative))?;
    }
    sync_directory(&root.join("packs").join("sha256"))?;
    if members
        .iter()
        .any(|member| schema_22_noop_member_filename(&member.path).is_some())
    {
        sync_directory(&root.join("packs").join("noop"))?;
    }
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
    let connection = open_immutable_catalogue(data)?;
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

fn validate_pairing_authority_absent(data: &Path) -> Result<(), DataRecoveryError> {
    let connection = open_immutable_catalogue(data)?;
    let (challenge_count, device_count): (i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM pairing_challenges),
                (SELECT COUNT(*) FROM paired_devices)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if challenge_count != 0 || device_count != 0 {
        return Err(invalid(
            "current data-backup catalogue retains pairing authority",
        ));
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
    let file = File::open(path)
        .map_err(|source| io_error("opening data-backup metadata", path, source))?;
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
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|source| io_error("creating data-backup metadata", path, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error("writing data-backup metadata", path, source))?;
    file.sync_all()
        .map_err(|source| io_error("syncing data-backup metadata", path, source))?;
    set_mode(path, PRIVATE_FILE_MODE, "protecting data-backup metadata")
}

fn sha256_file_hex(path: &Path) -> Result<String, DataRecoveryError> {
    let before = require_regular_file(path, "data-backup member being hashed")?;
    let file = File::open(path)
        .map_err(|source| io_error("opening data-backup member for hashing", path, source))?;
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

fn current_schema_22_noop_pairs(
    data: &Path,
) -> Result<BTreeMap<String, SyncManifest>, DataRecoveryError> {
    let connection = open_immutable_catalogue(data)?;
    let rows = connection
        .prepare(
            "SELECT snapshot_id, vehicle_id, head_sequence, manifest_json
             FROM sync_manifests",
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(DataRecoveryError::CatalogueIdentity)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    let mut current = BTreeMap::<Uuid, SyncManifest>::new();
    for (snapshot_id, vehicle_id, head_sequence, payload) in rows {
        let manifest: SyncManifest = match serde_json::from_slice(&payload) {
            Ok(manifest) => manifest,
            Err(_)
                if serde_json::from_slice::<LineageManifestV2>(&payload).is_ok()
                    || serde_json::from_slice::<LineageDelta>(&payload).is_ok() =>
            {
                continue;
            }
            Err(error) => return Err(DataRecoveryError::Decode(error)),
        };
        manifest
            .validate()
            .map_err(|error| invalid(format!("invalid manifest in backup catalogue: {error}")))?;
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
        match current.get(&manifest.vehicle_id) {
            Some(existing) if existing.head_sequence > manifest.head_sequence => {}
            Some(existing)
                if existing.head_sequence == manifest.head_sequence
                    && existing.snapshot_id != manifest.snapshot_id =>
            {
                return Err(invalid(
                    "backup catalogue has conflicting current full snapshots",
                ));
            }
            Some(existing) if existing.head_sequence == manifest.head_sequence => {}
            _ => {
                current.insert(manifest.vehicle_id, manifest);
            }
        }
    }
    let mut pairs = BTreeMap::new();
    for manifest in current.into_values() {
        if manifest.schema != HUB_PROJECTION_SCHEMA_V3 {
            continue;
        }
        let name = format!("{}.{}.json", manifest.vehicle_id, manifest.snapshot_id);
        if pairs.insert(name, manifest).is_some() {
            return Err(invalid("duplicate schema 2.2 no-op backup identity"));
        }
    }
    Ok(pairs)
}

fn validate_schema_22_noop_directory(
    data: &Path,
    expected: &BTreeMap<String, SyncManifest>,
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
    let expected_names = expected.keys().cloned().collect();
    require_directory_entry_set(&directory, &expected_names, "schema 2.2 no-op directory")?;
    for (name, manifest) in expected {
        let path = directory.join(name);
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
        let noop: SignedNoOpState =
            serde_json::from_slice(&bytes).map_err(DataRecoveryError::Decode)?;
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
        })?;
    }
    Ok(())
}

fn require_directory_entries<const N: usize>(
    path: &Path,
    expected: [&str; N],
    description: &'static str,
) -> Result<(), DataRecoveryError> {
    let expected = expected
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    require_directory_entry_set(path, &expected, description)
}

fn require_directory_entry_set(
    path: &Path,
    expected: &BTreeSet<String>,
    description: &'static str,
) -> Result<(), DataRecoveryError> {
    let metadata = require_real_directory(path, description)?;
    if permission_mode(&metadata) != PRIVATE_DIRECTORY_MODE {
        return Err(invalid(format!("{description} must have mode 0700")));
    }
    let mut actual = BTreeSet::new();
    for entry in read_directory(path)? {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid(format!("{description} contains a non-UTF-8 name")))?;
        if !actual.insert(name) {
            return Err(invalid(format!("{description} contains a duplicate name")));
        }
    }
    if &actual != expected {
        return Err(invalid(format!(
            "{description} has unknown or missing members (expected {expected:?}, actual {actual:?})"
        )));
    }
    Ok(())
}

fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, DataRecoveryError> {
    let directory = fs::read_dir(path)
        .map_err(|source| io_error("reading data-backup directory", path, source))?;
    let mut entries = Vec::new();
    for entry in directory {
        entries.push(
            entry
                .map_err(|source| io_error("reading data-backup directory entry", path, source))?,
        );
        if entries.len() > MAX_BACKUP_MEMBERS + 16 {
            return Err(invalid(
                "data-backup directory entry count exceeds its bound",
            ));
        }
    }
    Ok(entries)
}

fn require_real_directory(
    path: &Path,
    description: &'static str,
) -> Result<fs::Metadata, DataRecoveryError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspecting data-backup directory", path, source))?;
    if !metadata.file_type().is_dir() {
        return Err(invalid(format!("{description} is not a real directory")));
    }
    Ok(metadata)
}

fn require_regular_file(
    path: &Path,
    description: &'static str,
) -> Result<fs::Metadata, DataRecoveryError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspecting data-backup file", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(invalid(format!("{description} is not a regular file")));
    }
    Ok(metadata)
}

fn create_private_directory(path: &Path) -> Result<(), DataRecoveryError> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(PRIVATE_DIRECTORY_MODE);
    builder
        .create(path)
        .map_err(|source| io_error("creating restored data directory", path, source))?;
    set_mode(
        path,
        PRIVATE_DIRECTORY_MODE,
        "protecting restored data directory",
    )
}

fn set_mode(path: &Path, mode: u32, operation: &'static str) -> Result<(), DataRecoveryError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source| io_error(operation, path, source))
}

fn permission_mode(metadata: &fs::Metadata) -> u32 {
    metadata.permissions().mode() & 0o7777
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

fn sync_file(path: &Path) -> Result<(), DataRecoveryError> {
    File::open(path)
        .map_err(|source| io_error("opening data-backup member for sync", path, source))?
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{SourceDescriptor, VehicleDescriptor},
        protocol::{
            CursorClaims, CursorKey, LineageBase, LineageCapability, LineageDelta,
            LineageManifestV2, MirrorTable, OpaqueCursor, PackCompression, PackFormat,
            SchemaVersion, SequenceRange, Sha256Digest, TransportPack,
            canonical_delta_chain_digest,
        },
    };

    #[derive(Debug, PartialEq, Eq)]
    struct SnapshotEntry {
        path: String,
        mode: u32,
        modified_seconds: i64,
        modified_nanoseconds: i64,
        sha256: Option<String>,
    }

    fn tree_snapshot(root: &Path) -> Vec<SnapshotEntry> {
        fn visit(root: &Path, current: &Path, output: &mut Vec<SnapshotEntry>) {
            for entry in fs::read_dir(current).expect("read snapshot directory") {
                let entry = entry.expect("snapshot entry");
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).expect("snapshot metadata");
                let relative = path
                    .strip_prefix(root)
                    .expect("snapshot prefix")
                    .to_string_lossy()
                    .into_owned();
                if metadata.file_type().is_dir() {
                    output.push(SnapshotEntry {
                        path: relative,
                        mode: permission_mode(&metadata),
                        modified_seconds: metadata.mtime(),
                        modified_nanoseconds: metadata.mtime_nsec(),
                        sha256: None,
                    });
                    visit(root, &path, output);
                } else {
                    output.push(SnapshotEntry {
                        path: relative,
                        mode: permission_mode(&metadata),
                        modified_seconds: metadata.mtime(),
                        modified_nanoseconds: metadata.mtime_nsec(),
                        sha256: Some(sha256_file_hex(&path).expect("snapshot digest")),
                    });
                }
            }
        }
        let root_metadata = fs::symlink_metadata(root).expect("snapshot root metadata");
        let mut output = vec![SnapshotEntry {
            path: ".".to_owned(),
            mode: permission_mode(&root_metadata),
            modified_seconds: root_metadata.mtime(),
            modified_nanoseconds: root_metadata.mtime_nsec(),
            sha256: None,
        }];
        visit(root, root, &mut output);
        output.sort_by(|left, right| left.path.cmp(&right.path));
        output
    }

    fn create_fixture() -> (tempfile::TempDir, HubStore) {
        let temporary = tempfile::tempdir().expect("fixture parent");
        let data = temporary.path().join("source-data");
        let store = HubStore::initialize(&data).expect("source store");
        store
            .create_pairing("Recovery fixture", 1_000, 10_000)
            .expect("pairing database row");
        (temporary, store)
    }

    fn publish_lineage_fixture(store: &HubStore, cursor_key: &CursorKey) -> LineageManifestV2 {
        let source = store
            .register_source(
                &SourceDescriptor::new("tesla_owner_api", "recovery-account"),
                1_000,
            )
            .expect("recovery account source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "recovery-vehicle"),
                1_001,
            )
            .expect("recovery vehicle");
        let installation_id = store.installation_id().expect("installation identity");
        let base_snapshot_id = Uuid::new_v4();
        let make_pack = |ordinal: u32, sequence: SequenceRange, bytes: &[u8]| -> TransportPack {
            let digest = Sha256Digest::of_bytes(bytes);
            TransportPack {
                pack_id: Uuid::new_v4(),
                snapshot_id: base_snapshot_id,
                ordinal,
                schema: SchemaVersion { major: 1, minor: 0 },
                format: PackFormat::SqliteTransport,
                compression: PackCompression::Zstd,
                relative_path: TransportPack::canonical_relative_path(digest),
                sha256: digest,
                compressed_bytes: u64::try_from(bytes.len()).expect("pack size"),
                uncompressed_bytes: 100,
                row_count: 1,
                sequence,
                tables: vec![MirrorTable::Vehicle],
            }
        };
        let base_bytes = b"recovery-base-pack";
        let delta_bytes = b"recovery-delta-pack";
        let base_pack = make_pack(
            0,
            SequenceRange {
                from_exclusive: 10,
                to_inclusive: 10,
            },
            base_bytes,
        );
        let delta_pack = make_pack(
            1,
            SequenceRange {
                from_exclusive: 10,
                to_inclusive: 11,
            },
            delta_bytes,
        );
        let base_digest = Sha256Digest::of_bytes(b"recovery-base-chain");
        let head_digest = canonical_delta_chain_digest(base_digest, delta_pack.sha256);
        let terminal_cursor = OpaqueCursor::issue(
            cursor_key,
            CursorClaims {
                protocol: PROTOCOL_V1,
                schema: SchemaVersion { major: 1, minor: 0 },
                installation_id,
                account_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                generation: 1,
                sequence: 11,
            },
        )
        .expect("lineage cursor");
        let lineage = LineageManifestV2 {
            protocol: LINEAGE_PROTOCOL_V2,
            capability: LineageCapability::ImmutableBaseOrderedDeltas,
            schema: SchemaVersion { major: 1, minor: 0 },
            installation_id,
            account_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: 1,
            base: LineageBase {
                snapshot_id: base_snapshot_id,
                sequence: 10,
                digest: base_digest,
                packs: vec![base_pack.clone()],
            },
            deltas: vec![LineageDelta {
                from_sequence: 10,
                to_sequence: 11,
                parent_chain_digest: base_digest,
                chain_digest: head_digest,
                pack_digest: delta_pack.sha256,
                pack: delta_pack.clone(),
            }],
            head_sequence: 11,
            head_digest,
            terminal_cursor,
        };
        let pack_directory = store.packs_dir().join("sha256");
        fs::create_dir_all(&pack_directory).expect("lineage pack directory");
        for (pack, bytes) in [
            (&base_pack, base_bytes.as_slice()),
            (&delta_pack, delta_bytes),
        ] {
            fs::write(
                pack_directory.join(format!("{}.sqlite.zst", pack.sha256)),
                bytes,
            )
            .expect("lineage pack");
        }
        store
            .commit_lineage_catalog(&lineage)
            .expect("lineage catalogue");
        store
            .open()
            .expect("lineage binding catalogue")
            .execute(
                "INSERT INTO v2_base_bindings(
                    vehicle_id, snapshot_id, installation_id, account_id,
                    generation, selected_car_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    vehicle.vehicle_id.to_string(),
                    base_snapshot_id.to_string(),
                    installation_id.to_string(),
                    source.source_id.to_string(),
                    1_i64,
                    1_i64,
                ],
            )
            .expect("immutable lineage binding");
        lineage
    }

    fn pairing_authority_counts(data: &Path) -> (i64, i64) {
        let connection = open_immutable_catalogue(data).expect("pairing authority catalogue");
        let challenges = connection
            .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
                row.get(0)
            })
            .expect("pairing challenge count");
        let devices = connection
            .query_row("SELECT COUNT(*) FROM paired_devices", [], |row| row.get(0))
            .expect("paired device count");
        (challenges, devices)
    }

    fn reseal_backup_as_legacy_v3_with_pairing(backup: &Path, bearer: &str) {
        let catalogue = backup.join(CATALOGUE_MEMBER);
        let connection = Connection::open_with_flags(
            &catalogue,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("legacy catalogue fixture");
        connection
            .execute(
                "INSERT INTO pairing_challenges
                 (pairing_id, label, secret_sha256, created_at_ms, expires_at_ms)
                 VALUES (?1, 'legacy invitation', ?2, 1000, 10000)",
                rusqlite::params![Uuid::new_v4().to_string(), vec![17_u8; 32]],
            )
            .expect("legacy pairing challenge");
        connection
            .execute(
                "INSERT INTO paired_devices
                 (device_id, display_name, token_sha256, created_at_ms, expires_at_ms,
                  revoked_at_ms, last_authenticated_at_ms)
                 VALUES (?1, 'legacy device', ?2, 2000, 20000, NULL, 2001)",
                rusqlite::params![
                    Uuid::new_v4().to_string(),
                    Sha256::digest(bearer.as_bytes()).to_vec()
                ],
            )
            .expect("legacy paired device");
        drop(connection);

        let current_manifest_path = backup.join(MANIFEST_NAME);
        let legacy_manifest_path = backup.join(LEGACY_MANIFEST_NAME);
        let mut manifest: BackupManifest = parse_canonical_json(
            &fs::read(&current_manifest_path).expect("current manifest bytes"),
        )
        .expect("current manifest");
        manifest.kind = LEGACY_BACKUP_KIND.to_owned();
        manifest.scope = LEGACY_BACKUP_SCOPE.to_owned();
        let catalogue_member = manifest
            .members
            .iter_mut()
            .find(|member| member.path == CATALOGUE_MEMBER)
            .expect("legacy catalogue member");
        catalogue_member.size = fs::metadata(&catalogue)
            .expect("legacy catalogue metadata")
            .len();
        catalogue_member.sha256 = sha256_file_hex(&catalogue).expect("legacy catalogue digest");
        fs::rename(&current_manifest_path, &legacy_manifest_path)
            .expect("legacy manifest filename");
        let manifest_bytes = canonical_json(&manifest).expect("legacy manifest bytes");
        fs::write(&legacy_manifest_path, &manifest_bytes).expect("legacy manifest");
        set_mode(
            &legacy_manifest_path,
            PRIVATE_FILE_MODE,
            "legacy manifest mode",
        )
        .unwrap();

        let marker = CompletionMarker {
            kind: LEGACY_COMPLETION_KIND.to_owned(),
            generation: manifest.generation,
            manifest_sha256: sha256_bytes_hex(&manifest_bytes),
        };
        let marker_path = backup.join(COMPLETION_NAME);
        fs::write(&marker_path, canonical_json(&marker).unwrap()).expect("legacy marker");
        set_mode(&marker_path, PRIVATE_FILE_MODE, "legacy marker mode").unwrap();
    }

    fn reseal_backup_as_schema(backup: &Path, schema: i32) -> BackupManifest {
        let catalogue = backup.join(CATALOGUE_MEMBER);
        if (52..SCHEMA_VERSION).contains(&schema) {
            let connection = Connection::open_with_flags(
                &catalogue,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .expect("open historical catalogue fixture");
            downgrade_catalogue_fixture(&connection, schema);
            drop(connection);
        }

        let manifest_path = backup.join(MANIFEST_NAME);
        let mut manifest: BackupManifest =
            parse_canonical_json(&fs::read(&manifest_path).expect("manifest bytes"))
                .expect("typed manifest");
        manifest.hub_schema = schema;
        let catalogue_member = manifest
            .members
            .iter_mut()
            .find(|member| member.path == CATALOGUE_MEMBER)
            .expect("catalogue member");
        catalogue_member.size = fs::metadata(&catalogue).expect("catalogue metadata").len();
        catalogue_member.sha256 = sha256_file_hex(&catalogue).expect("catalogue digest");
        let manifest_bytes = canonical_json(&manifest).expect("historical manifest");
        fs::write(&manifest_path, &manifest_bytes).expect("write historical manifest");
        set_mode(
            &manifest_path,
            PRIVATE_FILE_MODE,
            "historical manifest mode",
        )
        .unwrap();

        let marker = CompletionMarker {
            kind: COMPLETION_KIND.to_owned(),
            generation: manifest.generation,
            manifest_sha256: sha256_bytes_hex(&manifest_bytes),
        };
        let marker_path = backup.join(COMPLETION_NAME);
        fs::write(&marker_path, canonical_json(&marker).unwrap()).expect("write historical marker");
        set_mode(&marker_path, PRIVATE_FILE_MODE, "historical marker mode").unwrap();
        manifest
    }

    fn downgrade_catalogue_fixture(connection: &Connection, schema: i32) {
        assert!((52..=54).contains(&schema));
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 DROP TABLE fleet_refresh_input_fences;
                 DROP TABLE fleet_refresh_receipt_bindings;
                 CREATE TABLE fleet_tokens_v54 (
                    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                    access BLOB NOT NULL CHECK(length(access) BETWEEN 1 AND 16424),
                    refresh BLOB NOT NULL CHECK(length(refresh) BETWEEN 1 AND 16424),
                    client_id TEXT NOT NULL
                        CHECK(length(CAST(client_id AS BLOB)) BETWEEN 1 AND 255),
                    region TEXT NOT NULL CHECK(region IN ('na', 'eu', 'cn')),
                    expires_at INTEGER NOT NULL CHECK(expires_at > 0),
                    next_refresh_at INTEGER NOT NULL
                        CHECK(next_refresh_at > 0 AND next_refresh_at < expires_at)
                 ) STRICT;
                 INSERT INTO fleet_tokens_v54(
                    singleton_id, access, refresh, client_id, region,
                    expires_at, next_refresh_at
                 )
                 SELECT singleton_id, access, refresh, client_id, region,
                        expires_at, next_refresh_at
                   FROM fleet_tokens;
                 DROP TABLE fleet_tokens;
                 ALTER TABLE fleet_tokens_v54 RENAME TO fleet_tokens;
                 CREATE TABLE current_observations_v54 (
                    vehicle_id TEXT NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    record_type TEXT NOT NULL CHECK(record_type IN (
                        'owner_api_discovery_v1',
                        'owner_api_vehicle_data_v1',
                        'tesla_stream_update_v1'
                    )),
                    observation_id INTEGER NOT NULL CHECK(observation_id > 0),
                    source_id TEXT NOT NULL
                        REFERENCES sources(source_id) ON DELETE RESTRICT,
                    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
                    received_at_ms INTEGER NOT NULL CHECK(received_at_ms >= 0),
                    payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32),
                    payload_json TEXT NOT NULL CHECK(json_valid(payload_json))
                        CHECK(length(CAST(payload_json AS BLOB)) <= 262144),
                    PRIMARY KEY(vehicle_id, record_type)
                 ) STRICT, WITHOUT ROWID;
                 INSERT INTO current_observations_v54(
                    vehicle_id, record_type, observation_id, source_id,
                    observed_at_ms, received_at_ms, payload_sha256, payload_json
                 )
                 SELECT vehicle_id, record_type, observation_id, source_id,
                        observed_at_ms, received_at_ms, payload_sha256, payload_json
                   FROM current_observations;
                 DROP TABLE current_observations;
                 ALTER TABLE current_observations_v54 RENAME TO current_observations;
                 PRAGMA user_version = 54;
                 COMMIT;",
            )
            .expect("recreate schema 54 fixture");
        if schema == 54 {
            return;
        }

        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE outbound_request_receipts_v53 (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    vehicle_tesla_id INTEGER CHECK(vehicle_tesla_id > 0),
                    transport TEXT NOT NULL CHECK(transport IN (
                        'owner_api', 'stream', 'legacy_auth'
                    )),
                    operation TEXT NOT NULL CHECK(operation IN (
                        'products', 'vehicle_probe', 'vehicle_data', 'token_refresh',
                        'stream_connect', 'stream_subscribe', 'stream_unsubscribe'
                    )),
                    safety_class TEXT NOT NULL CHECK(safety_class IN (
                        'non_wake_endpoint', 'conditional_read', 'direct_wake_command'
                    )),
                    precondition TEXT NOT NULL CHECK(precondition IN (
                        'not_required', 'stream_power_confirmed'
                    )),
                    outcome TEXT NOT NULL CHECK(outcome IN (
                        'started', 'success', 'http_error', 'timeout',
                        'transport_error', 'authentication_rejected',
                        'protocol_error', 'response_too_large', 'cancelled'
                    )),
                    http_status INTEGER CHECK(http_status BETWEEN 100 AND 599),
                    retry_after_seconds INTEGER CHECK(retry_after_seconds >= 0),
                    CHECK(
                        (outcome = 'started' AND completed_at_ms IS NULL
                         AND duration_ms IS NULL AND http_status IS NULL
                         AND retry_after_seconds IS NULL)
                        OR
                        (outcome <> 'started' AND completed_at_ms IS NOT NULL
                         AND duration_ms IS NOT NULL
                         AND completed_at_ms >= started_at_ms
                         AND duration_ms >= 0)
                    )
                 ) STRICT;
                 INSERT INTO outbound_request_receipts_v53(
                    id, correlation_id, started_at_ms, completed_at_ms,
                    duration_ms, vehicle_tesla_id, transport, operation,
                    safety_class, precondition, outcome, http_status,
                    retry_after_seconds
                 )
                 SELECT id, correlation_id, started_at_ms, completed_at_ms,
                        duration_ms, vehicle_tesla_id, transport, operation,
                        safety_class, precondition, outcome, http_status,
                        retry_after_seconds
                   FROM outbound_request_receipts;
                 CREATE TABLE legacy_refresh_receipt_bindings_v53 (
                    receipt_id INTEGER PRIMARY KEY NOT NULL
                        REFERENCES outbound_request_receipts_v53(id) ON DELETE CASCADE,
                    attempt_id TEXT NOT NULL UNIQUE CHECK(length(attempt_id) = 36),
                    input_credential_generation TEXT NOT NULL
                        CHECK(length(input_credential_generation) = 36),
                    output_credential_generation TEXT
                        CHECK(output_credential_generation IS NULL
                              OR length(output_credential_generation) = 36),
                    CHECK(output_credential_generation IS NULL
                          OR output_credential_generation <> input_credential_generation)
                 ) STRICT;
                 INSERT INTO legacy_refresh_receipt_bindings_v53(
                    receipt_id, attempt_id, input_credential_generation,
                    output_credential_generation
                 )
                 SELECT receipt_id, attempt_id, input_credential_generation,
                        output_credential_generation
                   FROM legacy_refresh_receipt_bindings;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE outbound_request_receipts;
                 ALTER TABLE outbound_request_receipts_v53
                    RENAME TO outbound_request_receipts;
                 ALTER TABLE legacy_refresh_receipt_bindings_v53
                    RENAME TO legacy_refresh_receipt_bindings;
                 CREATE INDEX outbound_request_receipts_proof
                    ON outbound_request_receipts(
                        correlation_id, id, safety_class, outcome
                    );
                 CREATE INDEX outbound_request_receipts_retention
                    ON outbound_request_receipts(outcome, completed_at_ms, id);
                 CREATE UNIQUE INDEX legacy_refresh_receipt_output_generation
                    ON legacy_refresh_receipt_bindings(output_credential_generation)
                    WHERE output_credential_generation IS NOT NULL;
                 DROP TABLE fleet_tokens;
                 PRAGMA user_version = 53;
                 COMMIT;",
            )
            .expect("recreate schema 53 fixture");
        if schema == 53 {
            return;
        }

        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE lifecycle_open_rows_v52 (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    source_table TEXT NOT NULL,
                    source_row_id INTEGER NOT NULL CHECK(source_row_id > 0),
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    domain TEXT NOT NULL CHECK(domain IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state',
                        'standalone_position'
                    )),
                    parent_source_row_id INTEGER,
                    row_json TEXT NOT NULL CHECK(json_valid(row_json)),
                    PRIMARY KEY(source_id, source_table, source_row_id)
                 ) STRICT;
                 INSERT INTO lifecycle_open_rows_v52(
                    source_id, source_table, source_row_id, vehicle_id, car_id,
                    domain, parent_source_row_id, row_json
                 )
                 SELECT source_id, source_table, source_row_id, vehicle_id, car_id,
                        domain, parent_source_row_id, row_json
                   FROM lifecycle_open_rows;
                 DROP TABLE lifecycle_open_rows;
                 ALTER TABLE lifecycle_open_rows_v52 RENAME TO lifecycle_open_rows;
                 CREATE INDEX lifecycle_open_rows_vehicle_domain
                    ON lifecycle_open_rows(vehicle_id, domain, source_row_id);
                 PRAGMA user_version = 52;
                 COMMIT;",
            )
            .expect("recreate schema 52 fixture");
    }

    #[test]
    fn staging_capacity_admission_rejects_simulated_low_space() {
        let copy_bytes = 101_u64;
        let required = staging_required_bytes(copy_bytes).expect("required staged bytes");
        assert_eq!(
            required,
            copy_bytes
                + (copy_bytes / COPY_CAPACITY_HEADROOM_DIVISOR + 1)
                + COPY_CAPACITY_FIXED_HEADROOM_BYTES
        );
        let parent = Path::new("/simulated-capacity-parent");
        assert!(matches!(
            admit_known_capacity(parent, required, required - 1),
            Err(DataRecoveryError::InsufficientFreeSpace {
                path,
                required: actual_required,
                available,
            }) if path == parent && actual_required == required && available == required - 1
        ));
        admit_known_capacity(parent, required, required).expect("exact capacity accepted");
    }

    #[test]
    fn staging_capacity_calculation_rejects_overflow() {
        assert!(matches!(
            staging_required_bytes(u64::MAX),
            Err(DataRecoveryError::CapacityOverflow)
        ));
    }

    #[test]
    fn backup_capacity_excludes_uncatalogued_pack_and_symlink_files() {
        let (temporary, store) = create_fixture();
        let before = store.backup_copy_bytes().expect("catalogued copy bytes");
        let sha = store.packs_dir().join("sha256");
        fs::create_dir_all(&sha).expect("pack directory");
        fs::write(sha.join("orphan.sqlite.zst"), vec![0_u8; 4096]).expect("write orphan pack");
        let outside = temporary.path().join("outside-pack");
        fs::write(&outside, b"must not be counted through symlink").expect("write outside");
        std::os::unix::fs::symlink(&outside, sha.join("linked.sqlite.zst"))
            .expect("create capacity symlink");

        assert_eq!(
            store
                .backup_copy_bytes()
                .expect("unchanged exact copy bytes"),
            before
        );
    }

    fn publish_schema_22_fixture(store: &HubStore) -> (SyncManifest, SignedNoOpState) {
        let data = store.database_path().parent().expect("fixture data root");
        let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(data)
            .expect("fixture cursor key");
        let (built, snapshot) =
            crate::updates_delivery::write_updates_schema_22_pack(store.packs_dir(), Vec::new())
                .expect("write schema 2.2 backup fixture pack");
        let request = crate::updates_delivery::updates_pack_request(&snapshot);
        let manifest =
            crate::updates_delivery::sign_updates_schema_22_manifest(&request, &built, &cursor_key)
                .expect("sign schema 2.2 backup fixture manifest");
        let noop = crate::updates_delivery::sign_updates_schema_22_noop(
            &request.binding,
            request.snapshot_id,
            request.sequence.to_inclusive,
            &built.metadata.sha256.to_string(),
            &cursor_key,
        )
        .expect("sign schema 2.2 backup fixture no-op");
        crate::updates_delivery::publish_updates_schema_22(store, &manifest, &noop)
            .expect("publish schema 2.2 backup fixture pair");
        (manifest, noop)
    }

    #[test]
    fn data_backup_revokes_pairing_authority_and_preserves_identity_and_lineage() {
        let (temporary, store) = create_fixture();
        let source_data = store.database_path().parent().expect("source data root");
        let device_pairing = store
            .create_pairing("Recovery paired device", 2_000, 20_000)
            .expect("device pairing");
        let paired_access = store
            .claim_pairing(
                device_pairing.pairing_id,
                device_pairing.secret(),
                "Recovery device",
                3_000,
            )
            .expect("active paired device");
        let paired_bearer = paired_access.access_token.as_bearer().to_owned();
        assert!(
            store
                .authenticate_device_at(&paired_bearer, 3_001)
                .expect("source bearer authentication")
                .is_some()
        );
        assert_eq!(pairing_authority_counts(source_data), (1, 1));
        let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(source_data)
            .expect("source cursor key");
        let source_lineage = publish_lineage_fixture(&store, &cursor_key);
        let source_cursor = fs::read(crate::teslamate_credentials::cursor_key_path(source_data))
            .expect("source cursor bytes");
        let owner_tokens = crate::credentials::OwnerTokens::from_secret_parts(
            "backup-access".to_owned(),
            "backup-refresh".to_owned(),
        )
        .expect("owner tokens");
        let encryption_key = b"backup exact TeslaMate key";
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(encryption_key, &owner_tokens)
                .expect("encrypt owner tokens");
        let stored_tokens =
            crate::db::TeslaMateLegacyTokenStore::imported(access, refresh).expect("stored tokens");
        crate::teslamate_credentials::replace_key_and_tokens(
            source_data,
            &store,
            encryption_key,
            &stored_tokens,
        )
        .expect("source credentials");
        let installation_id = store.installation_id().expect("source installation");
        store
            .acquire_supervised_collector_lease(1_000)
            .expect("active source collector lease");
        let source_authority_rows: i64 = store
            .open()
            .expect("source catalogue")
            .query_row(
                "SELECT COUNT(*) FROM supervised_collector_lease",
                [],
                |row| row.get(0),
            )
            .expect("source authority rows");
        assert_eq!(source_authority_rows, 1);
        let backup = temporary.path().join("backup-generation");
        let created = create_data_backup(&store, &backup).expect("create data backup");
        assert_eq!(created.scope, BACKUP_SCOPE);
        assert!(!created.clean_host_ready);
        assert_eq!(created.collector_authority, "absent");
        assert_eq!(permission_mode(&fs::metadata(&backup).unwrap()), 0o700);
        assert!(!backup.join("data/secrets").exists());
        assert_eq!(
            pairing_authority_counts(&backup.join(DATA_DIRECTORY)),
            (0, 0),
            "new backups must exclude invitations and paired-device bearers"
        );
        let backup_store =
            HubStore::open_immutable_read_only(backup.join(DATA_DIRECTORY)).expect("backup store");
        assert_eq!(backup_store.installation_id().unwrap(), installation_id);
        assert_eq!(
            backup_store
                .source_vehicle_key(source_lineage.vehicle_id)
                .expect("backup vehicle identity"),
            Some("recovery-vehicle".to_owned())
        );
        assert_eq!(
            backup_store
                .lineage_manifest_for_vehicle(source_lineage.vehicle_id)
                .expect("backup lineage"),
            Some(source_lineage.clone()),
            "backup sanitation must preserve account-bound vehicle sync lineage"
        );

        let before = tree_snapshot(&backup);
        let verified = verify_data_backup(&backup).expect("verify data backup");
        assert_eq!(verified.generation, created.generation);
        assert_eq!(tree_snapshot(&backup), before);
        let backup_authority_rows: i64 = open_immutable_catalogue(&backup.join(DATA_DIRECTORY))
            .expect("backup catalogue")
            .query_row(
                "SELECT COUNT(*) FROM supervised_collector_lease",
                [],
                |row| row.get(0),
            )
            .expect("backup authority rows");
        assert_eq!(backup_authority_rows, 0);

        let restored = temporary.path().join("restored-data");
        let restored_report =
            restore_data_backup(&backup, &restored).expect("restore bounded data");
        assert_eq!(restored_report.installation_id, installation_id);
        assert!(!restored_report.clean_host_ready);
        assert_eq!(tree_snapshot(&backup), before);
        assert!(!restored.join("secrets").exists());
        assert!(!restored.join("tls").exists());

        let restored_store = HubStore::open_immutable_read_only(&restored).expect("restored store");
        restored_store
            .catalogue_check()
            .expect("restored catalogue");
        assert_eq!(
            immutable_database_identity(&restored).unwrap(),
            installation_id
        );
        assert_eq!(pairing_authority_counts(&restored), (0, 0));
        assert!(
            restored_store
                .authenticate_device_at(&paired_bearer, 3_001)
                .expect("restored bearer authentication")
                .is_none(),
            "a pre-backup paired-device bearer must not authenticate after restore"
        );
        assert_eq!(
            restored_store
                .source_vehicle_key(source_lineage.vehicle_id)
                .expect("restored vehicle identity"),
            Some("recovery-vehicle".to_owned())
        );
        assert_eq!(
            restored_store
                .lineage_manifest_for_vehicle(source_lineage.vehicle_id)
                .expect("restored lineage"),
            Some(source_lineage),
            "restore sanitation must preserve account-bound vehicle sync lineage"
        );
        let connection = open_immutable_catalogue(&restored).expect("immutable authority query");
        let restored_authority_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM supervised_collector_lease",
                [],
                |row| row.get(0),
            )
            .expect("restored authority rows");
        assert_eq!(restored_authority_rows, 0);
        let restored_tokens = restored_store
            .load_teslamate_legacy_tokens()
            .expect("restored token row")
            .expect("restored credentials");
        assert!(
            crate::teslamate_credentials::load_key_for_tokens(&restored, &restored_tokens).is_err(),
            "default data restore must not recover the credential key"
        );
        assert_eq!(
            fs::read(crate::teslamate_credentials::cursor_key_path(source_data))
                .expect("source cursor key after backup"),
            source_cursor,
            "backup must not mutate source key material"
        );
    }

    #[test]
    fn legacy_v3_backup_is_accepted_but_pairing_authority_is_revoked_before_publication() {
        let (temporary, store) = create_fixture();
        let device_pairing = store
            .create_pairing("Legacy paired device", 2_000, 20_000)
            .expect("legacy device pairing");
        let paired_access = store
            .claim_pairing(
                device_pairing.pairing_id,
                device_pairing.secret(),
                "Legacy device",
                3_000,
            )
            .expect("legacy paired device");
        let paired_bearer = paired_access.access_token.as_bearer().to_owned();
        let backup = temporary.path().join("legacy-v3-backup");
        create_data_backup(&store, &backup).expect("create sanitized fixture base");
        reseal_backup_as_legacy_v3_with_pairing(&backup, &paired_bearer);
        assert_eq!(
            pairing_authority_counts(&backup.join(DATA_DIRECTORY)),
            (1, 1)
        );
        let source_before = tree_snapshot(&backup);

        let verified = verify_data_backup(&backup).expect("accept legacy v3 backup");
        assert_eq!(verified.scope, LEGACY_BACKUP_SCOPE);
        assert_eq!(tree_snapshot(&backup), source_before);

        let restored = temporary.path().join("legacy-v3-restored");
        restore_data_backup(&backup, &restored).expect("restore legacy v3 backup");
        assert_eq!(tree_snapshot(&backup), source_before);
        assert_eq!(pairing_authority_counts(&restored), (0, 0));
        let restored_store =
            HubStore::open_immutable_read_only(&restored).expect("legacy restored immutable store");
        assert!(
            restored_store
                .authenticate_device_at(&paired_bearer, 3_001)
                .expect("legacy restored bearer authentication")
                .is_none(),
            "legacy bearer authority must not survive restore publication"
        );
    }

    #[test]
    fn historical_restore_migrates_schema_52_through_54_without_touching_source_or_pack_members() {
        for schema in [52, 53, 54] {
            let (temporary, store) = create_fixture();
            publish_schema_22_fixture(&store);
            let installation_id = store.installation_id().expect("installation ID");
            let backup = temporary.path().join(format!("schema-{schema}-backup"));
            create_data_backup(&store, &backup).expect("create current backup fixture");
            let manifest = reseal_backup_as_schema(&backup, schema);
            verify_data_backup(&backup).expect("verify historical backup");
            let source_before = tree_snapshot(&backup);

            let restored = temporary.path().join(format!("schema-{schema}-restored"));
            restore_data_backup(&backup, &restored).expect("restore historical backup");

            assert_eq!(tree_snapshot(&backup), source_before);
            assert_eq!(
                immutable_database_identity(&restored).expect("current restored identity"),
                installation_id
            );
            let restored_store =
                HubStore::open_immutable_read_only(&restored).expect("current restored store");
            restored_store
                .catalogue_check()
                .expect("current restored catalogue");
            restored_store
                .verify_immutable_snapshot_unchanged()
                .expect("restored catalogue remains immutable");

            for member in manifest
                .members
                .iter()
                .filter(|member| member.path != CATALOGUE_MEMBER)
            {
                let relative = member.path.strip_prefix("data/").unwrap();
                let path = restored.join(relative);
                let metadata = fs::metadata(&path).expect("restored member metadata");
                assert_eq!(permission_mode(&metadata), member.mode);
                assert_eq!(metadata.len(), member.size);
                assert_eq!(sha256_file_hex(&path).unwrap(), member.sha256);
            }
        }
    }

    #[test]
    fn historical_restore_rejects_schema_outside_the_current_range() {
        for schema in [MIN_RESTORABLE_SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
            let (temporary, store) = create_fixture();
            let backup = temporary.path().join(format!("schema-{schema}-backup"));
            create_data_backup(&store, &backup).expect("create backup fixture");
            reseal_backup_as_schema(&backup, schema);
            let source_before = tree_snapshot(&backup);
            let restored = temporary.path().join(format!("schema-{schema}-restored"));

            assert!(matches!(
                restore_data_backup(&backup, &restored),
                Err(DataRecoveryError::InvalidBackup(message))
                    if message.contains("outside the supported restore range")
            ));
            assert!(!restored.exists());
            assert_eq!(tree_snapshot(&backup), source_before);
        }
    }

    #[test]
    fn schema_22_pair_round_trips_with_exact_private_paths_modes_and_digests() {
        let (temporary, store) = create_fixture();
        let (manifest, noop) = publish_schema_22_fixture(&store);
        let name = format!("{}.{}.json", manifest.vehicle_id, manifest.snapshot_id);
        let member_path = format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}");
        let expected_bytes = serde_json::to_vec(&noop).expect("canonical no-op bytes");
        let backup = temporary.path().join("schema-22-backup");
        let created = create_data_backup(&store, &backup).expect("create schema 2.2 backup");
        let backup_noop = backup.join(&member_path);
        assert_eq!(
            fs::read(&backup_noop).expect("backup no-op"),
            expected_bytes
        );
        assert_eq!(
            permission_mode(&fs::metadata(backup_noop.parent().unwrap()).unwrap()),
            PRIVATE_DIRECTORY_MODE
        );
        let backup_noop_metadata = fs::metadata(&backup_noop).expect("backup no-op metadata");
        assert_eq!(permission_mode(&backup_noop_metadata), PRIVATE_FILE_MODE);
        assert_eq!(backup_noop_metadata.nlink(), 1);

        let backup_manifest: BackupManifest = parse_canonical_json(
            &fs::read(backup.join(MANIFEST_NAME)).expect("backup manifest bytes"),
        )
        .expect("backup manifest");
        let member = backup_manifest
            .members
            .iter()
            .find(|member| member.path == member_path)
            .expect("manifest no-op member");
        assert_eq!(member.mode, PRIVATE_FILE_MODE);
        assert_eq!(member.size, u64::try_from(expected_bytes.len()).unwrap());
        assert_eq!(member.sha256, sha256_bytes_hex(&expected_bytes));
        assert_eq!(created.member_count, backup_manifest.members.len());
        verify_data_backup(&backup).expect("verify schema 2.2 backup");

        let hardlink = temporary.path().join("schema-22-hardlink");
        fs::hard_link(&backup_noop, &hardlink).expect("create no-op hardlink attack");
        assert!(verify_data_backup(&backup).is_err());
        fs::remove_file(hardlink).expect("remove no-op hardlink attack");
        verify_data_backup(&backup).expect("verify after removing hardlink");

        let restored = temporary.path().join("schema-22-restored");
        restore_data_backup(&backup, &restored).expect("restore schema 2.2 backup");
        let restored_noop = restored.join("packs").join("noop").join(name);
        assert_eq!(
            fs::read(&restored_noop).expect("restored no-op"),
            expected_bytes
        );
        assert_eq!(
            permission_mode(&fs::metadata(restored_noop.parent().unwrap()).unwrap()),
            PRIVATE_DIRECTORY_MODE
        );
        let restored_metadata = fs::metadata(restored_noop).expect("restored no-op metadata");
        assert_eq!(permission_mode(&restored_metadata), PRIVATE_FILE_MODE);
        assert_eq!(restored_metadata.nlink(), 1);
    }

    #[test]
    fn schema_22_verifier_rejects_a_resealed_but_mismatched_noop_pair() {
        let (temporary, store) = create_fixture();
        let (manifest, _) = publish_schema_22_fixture(&store);
        let name = format!("{}.{}.json", manifest.vehicle_id, manifest.snapshot_id);
        let member_path = format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}");
        let backup = temporary.path().join("schema-22-tampered-backup");
        create_data_backup(&store, &backup).expect("create schema 2.2 backup");

        let noop_path = backup.join(&member_path);
        let mut noop: SignedNoOpState =
            serde_json::from_slice(&fs::read(&noop_path).expect("no-op bytes"))
                .expect("typed no-op");
        noop.generation += 1;
        let mismatched_bytes = serde_json::to_vec(&noop).expect("mismatched no-op bytes");
        fs::write(&noop_path, &mismatched_bytes).expect("write mismatched no-op");
        set_mode(&noop_path, PRIVATE_FILE_MODE, "test no-op mode").unwrap();

        let manifest_path = backup.join(MANIFEST_NAME);
        let mut backup_manifest: BackupManifest =
            parse_canonical_json(&fs::read(&manifest_path).unwrap()).unwrap();
        let member = backup_manifest
            .members
            .iter_mut()
            .find(|member| member.path == member_path)
            .expect("manifest no-op member");
        member.size = u64::try_from(mismatched_bytes.len()).unwrap();
        member.sha256 = sha256_bytes_hex(&mismatched_bytes);
        let manifest_bytes = canonical_json(&backup_manifest).expect("resealed manifest");
        fs::write(&manifest_path, &manifest_bytes).expect("write resealed manifest");
        set_mode(&manifest_path, PRIVATE_FILE_MODE, "test manifest mode").unwrap();

        let marker_path = backup.join(COMPLETION_NAME);
        let marker = CompletionMarker {
            kind: COMPLETION_KIND.to_owned(),
            generation: backup_manifest.generation,
            manifest_sha256: sha256_bytes_hex(&manifest_bytes),
        };
        fs::write(&marker_path, canonical_json(&marker).unwrap()).expect("write resealed marker");
        set_mode(&marker_path, PRIVATE_FILE_MODE, "test marker mode").unwrap();

        assert!(verify_data_backup(&backup).is_err());
    }

    #[test]
    fn verifier_rejects_tampering_unknown_members_missing_members_and_symlinks() {
        let cases = ["tampered", "unknown", "missing", "symlink"];
        for case in cases {
            let (temporary, store) = create_fixture();
            let backup = temporary.path().join("backup-generation");
            create_data_backup(&store, &backup).expect("create data backup");
            match case {
                "tampered" => {
                    let catalogue = backup.join(CATALOGUE_MEMBER);
                    let mut bytes = fs::read(&catalogue).expect("catalogue bytes");
                    bytes[0] ^= 1;
                    fs::write(&catalogue, bytes).expect("tamper catalogue");
                    set_mode(&catalogue, 0o600, "test mode").unwrap();
                }
                "unknown" => {
                    let unknown = backup.join("unexpected");
                    fs::write(&unknown, b"unexpected").expect("unknown member");
                    set_mode(&unknown, 0o600, "test mode").unwrap();
                }
                "missing" => fs::remove_file(backup.join(COMPLETION_NAME)).expect("remove marker"),
                "symlink" => {
                    let marker = backup.join(COMPLETION_NAME);
                    fs::remove_file(&marker).expect("remove marker");
                    std::os::unix::fs::symlink(MANIFEST_NAME, marker).expect("marker symlink");
                }
                _ => unreachable!(),
            }
            assert!(
                verify_data_backup(&backup).is_err(),
                "{case} backup must be rejected"
            );
        }
    }

    #[test]
    fn restore_refuses_existing_destination_and_leaves_it_untouched() {
        let (temporary, store) = create_fixture();
        let backup = temporary.path().join("backup-generation");
        create_data_backup(&store, &backup).expect("create data backup");
        let destination = temporary.path().join("existing");
        fs::create_dir(&destination).expect("existing destination");
        fs::write(destination.join("owner-file"), b"preserve").expect("owner file");

        assert!(restore_data_backup(&backup, &destination).is_err());
        assert_eq!(
            fs::read(destination.join("owner-file")).expect("preserved owner file"),
            b"preserve"
        );
    }

    #[test]
    fn noncanonical_or_scope_expanding_manifest_is_rejected() {
        let (temporary, store) = create_fixture();
        let backup = temporary.path().join("backup-generation");
        create_data_backup(&store, &backup).expect("create data backup");
        let manifest_path = backup.join(MANIFEST_NAME);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["scope"] = serde_json::Value::String("full_disaster_recovery".to_owned());
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("expanded manifest"),
        )
        .expect("write expanded manifest");
        set_mode(&manifest_path, 0o600, "test mode").unwrap();

        assert!(verify_data_backup(&backup).is_err());
    }
}
