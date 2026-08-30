// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::sync::{Arc, Mutex};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, backup::Backup,
    params,
};
use rustix::{
    fs::{
        AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags, fchmod, flock, fstat,
        mkdirat, open, openat, renameat_with, statat, unlinkat,
    },
    io::Errno,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    hub_pack::{
        MAX_TEXT_BYTES, ProjectionBinding, ProjectionCar, ProjectionCarSettings,
        ProjectionCarSettingsPatch, ProjectionDelta, ProjectionDeltaEntity, ProjectionDrive,
        ProjectionPackError, ProjectionPackRequest, ProjectionPackWriter, ProjectionPosition,
        ProjectionSnapshot, ProjectionTombstone, cleanup_stale_pack_staging,
    },
    protocol::{
        CursorKey, HUB_PROJECTION_SCHEMA_V2, HUB_PROJECTION_SCHEMA_V3, LINEAGE_PROTOCOL_V2,
        LineageBase, LineageCapability, LineageDelta, LineageManifestV2, OpaqueCursor,
        ProtocolLimits, SQLITE_HUB_PROJECTION_APPLICATION_ID, SchemaSupport, SequenceRange,
        Sha256Digest, SyncManifest, TransportPack, canonical_delta_chain_digest,
    },
    teslamate_projection::TeslaMateOpenSession,
    teslamate_projection_state::{
        MAX_PAGE_SIZE, PriorProjectionStateLookup, TeslaMateProjectionState,
        TeslaMateProjectionStateCursor, TeslaMateProjectionStateDigestPage,
        TeslaMateProjectionStateDigestRow, TeslaMateProjectionStateEntity,
        TeslaMateProjectionStateError, TeslaMateProjectionStateLimits,
        TeslaMateProjectionStateTransfer, recover_stale_import_generation_spools,
    },
    teslamate_token::MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES,
};

pub const APPLICATION_ID: i32 = 0x5441_4855; // TAHU
pub const SCHEMA_VERSION: i32 = 56;
pub const BUNDLED_SQLITE_VERSION: &str = "3.53.2";
/// Paired-device bearers are renewable, but never permanent.
pub const PAIRED_DEVICE_TOKEN_LIFETIME_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// A supervised collector renews this durable lease from an independent task.
/// The interval is deliberately much shorter than the lease so a short SQLite
/// writer stall cannot make a healthy collector flap, while a killed process
/// still becomes unready within a bounded period.
pub(crate) const SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const SUPERVISED_COLLECTOR_LEASE_MS: i64 = 30_000;

/// Hard upper bound for one persisted source response. A collector must split
/// high-volume telemetry into individual observations rather than retaining an
/// unbounded response in memory or in the Hub database.
pub const MAX_RAW_OBSERVATION_BYTES: usize = 256 * 1024;

/// The read API is deliberately capped so callers cannot accidentally turn a
/// history query into an all-memory transfer.
pub const MAX_OBSERVATION_QUERY_LIMIT: u32 = 10_000;
const OBSERVATIONS_AFTER_ID_SQL: &str = "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms, \
            payload_sha256, payload_json \
     FROM raw_observations \
     WHERE vehicle_id = ?1 AND observation_id > ?2 \
     ORDER BY observation_id ASC LIMIT ?3";
/// Request-ledger reads are metadata-only and bounded independently from raw
/// observation reads so proof commands cannot accidentally load an unbounded
/// audit history into memory.
pub const MAX_OUTBOUND_REQUEST_QUERY_LIMIT: u32 = 10_000;
/// Completed request receipts are eligible for normal retention cleanup after
/// this period. Unresolved `started` receipts are never deleted automatically.
pub const OUTBOUND_REQUEST_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
/// Replaced content-addressed delta objects remain authorized for clients that
/// already hold the immediately prior signed lineage. The active manifest is
/// always no-store, so one day covers interrupted LAN transfers without
/// turning retired objects into an unbounded permanent catalogue.
pub const RETIRED_LINEAGE_PACK_RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;
/// Physical cleanup trails authorization expiry so a request authorized just
/// before the boundary can still open and stream its immutable file while a
/// concurrent operator repair runs.
const RETIRED_LINEAGE_PACK_DELETE_GRACE_MS: i64 = 60 * 60 * 1_000;
/// The ledger rejects a new request before network I/O when completed-receipt
/// cleanup cannot make room below this bound without deleting an unresolved row.
pub const MAX_OUTBOUND_REQUEST_RECEIPTS: i64 = 100_000;
/// Permanent refresh-input fences have no TTL because forgetting one could
/// authorize reuse of a consumed single-use token. Bound the compact ledger at
/// the same conservative scale as request auditing and fail closed before any
/// network dispatch once operator intervention is required.
pub const MAX_LEGACY_REFRESH_INPUT_FENCES: i64 = MAX_OUTBOUND_REQUEST_RECEIPTS;
const MAX_SOURCE_KIND_BYTES: usize = 64;
const MAX_SOURCE_KEY_BYTES: usize = 256;
const MAX_VEHICLE_KEY_BYTES: usize = 256;
const MAX_VIN_BYTES: usize = 32;
const MAX_DISPLAY_NAME_BYTES: usize = 256;
const MAX_ADDRESS_RAW_JSON_BYTES: usize = 64 * 1024;
const MAX_PAIRING_LABEL_BYTES: usize = 128;
const MAX_DEVICE_NAME_BYTES: usize = 128;
const PAIRING_SECRET_BYTES: usize = 32;
const ACCESS_TOKEN_BYTES: usize = 32;
const INSTALLATION_ID_KEY: &str = "installation_id";
const CATALOGUE_COMMIT_RECEIPT_KEY: &str = "catalogue_commit_receipt_v1";
const PUBLICATION_GATE_RETRY: Duration = Duration::from_millis(50);
// One user-owned Hub process owns the entire local tree.
const SHARED_DATA_DIRECTORY_MODE: u32 = 0o700;
const SHARED_DATA_FILE_MODE: u32 = 0o600;
const SHARED_SQLITE_FILE_MODE: u32 = 0o600;
const SHARED_SCHEMA_22_NOOP_DIRECTORY_MODE: u32 = 0o700;
const SHARED_SCHEMA_22_NOOP_FILE_MODE: u32 = 0o600;
const PRIVATE_SCHEMA_22_NOOP_STAGING_MODE: u32 = 0o600;
const MAX_SCHEMA_22_NOOP_BYTES: u64 = 16 * 1024;
const PRIVATE_IMPORT_SPOOL_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_IMPORT_SPOOL_DIRECTORY_NAME: &str = "import-spool";
const TESLAMATE_LEGACY_DIRECT_BRIDGE_ALGORITHM: &str = "logical_projection_v1";
const FULL_SNAPSHOT_METADATA_KEYS: [&str; 16] = [
    "protocol",
    "pack_format",
    "schema_major",
    "schema_minor",
    "pack_id",
    "snapshot_id",
    "ordinal",
    "mode",
    "installation_id",
    "account_id",
    "vehicle_id",
    "generation",
    "selected_car_id",
    "base_sequence",
    "head_sequence",
    "row_count",
];
const TYPED_DELTA_METADATA_KEYS: [&str; 19] = [
    "protocol",
    "pack_format",
    "schema_major",
    "schema_minor",
    "delta_schema_version",
    "pack_id",
    "snapshot_id",
    "ordinal",
    "mode",
    "installation_id",
    "account_id",
    "vehicle_id",
    "generation",
    "selected_car_id",
    "from_sequence",
    "to_sequence",
    "parent_digest",
    "external_base",
    "row_count",
];
const TYPED_DELTA_TABLES: [&str; 10] = [
    "hub_pack_metadata",
    "cars",
    "car_settings",
    "drives",
    "charges",
    "positions",
    "charge_samples",
    "states",
    "updates",
    "tombstones",
];
// Hash of the sorted, NUL-delimited `(type, name, tbl_name, sql)` rows for
// every non-internal object written by `ProjectionPackWriter::write_delta`.
// A change here is a wire-format change: add a new schema version rather than
// silently accepting a different SQLite program under the same 2.1 contract.
const TYPED_DELTA_SCHEMA_CONTRACT_SHA256: &str =
    "5a0d41c43c7a64faa14d3e4f3ea037cf9cf3bf441d28df3bb56580bbc7a8b227";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamObservationResult {
    Committed { observation_id: i64 },
    IgnoredDuplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestCommitState {
    Exact,
    Absent,
    Conflicting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogueCommitReceiptState {
    Exact,
    Prior,
    Conflicting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFaultPoint {
    RawInsert,
    LifecycleWrite,
    WatermarkUpdate,
    Commit,
}

#[cfg(test)]
impl StreamFaultPoint {
    const fn label(self) -> &'static str {
        match self {
            Self::RawInsert => "raw_insert",
            Self::LifecycleWrite => "lifecycle_write",
            Self::WatermarkUpdate => "watermark_update",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HubStore {
    database_path: PathBuf,
    packs_dir: PathBuf,
    private_import_spool_dir: PathBuf,
    publication_lock_path: PathBuf,
    immutable_snapshot: Option<ImmutableCatalogueFingerprint>,
    #[cfg(test)]
    stream_fault: Arc<Mutex<Option<StreamFaultPoint>>>,
    #[cfg(test)]
    projection_state_detach_fault: Arc<Mutex<bool>>,
}

/// One collector-owned SQLite handle for high-rate stream writes. Each
/// observation still commits independently; only repeated connection setup,
/// schema reads, and PRAGMA work leave the hot path.
pub(crate) struct StreamObservationWriter {
    store: HubStore,
    connection: Connection,
}

impl StreamObservationWriter {
    pub(crate) fn accept(
        &mut self,
        input: &ObservationInput,
        received_at_ms: i64,
        car_id: i64,
    ) -> Result<StreamObservationResult, StoreError> {
        self.store.accept_stream_observation_and_lifecycle_on(
            &mut self.connection,
            input,
            received_at_ms,
            car_id,
        )
    }
}

/// One encrypted TeslaMate legacy OAuth pair plus its refresh schedule.
/// Schedule values are epoch seconds. Ciphertext must not be logged or formatted.
#[derive(zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct TeslaMateLegacyTokenStore {
    access: Vec<u8>,
    refresh: Vec<u8>,
    expires_at: i64,
    next_refresh_at: i64,
    #[zeroize(skip)]
    credential_generation: Option<Uuid>,
}

impl TeslaMateLegacyTokenStore {
    /// Imported TeslaMate ciphertext has no Hub-owned refresh schedule yet.
    pub fn imported(access: Vec<u8>, refresh: Vec<u8>) -> Result<Self, StoreError> {
        Self::new(access, refresh, 0, 0, None)
    }

    /// A refreshed pair must have a positive, ordered refresh schedule.
    pub fn refreshed(
        access: Vec<u8>,
        refresh: Vec<u8>,
        expires_at: i64,
        next_refresh_at: i64,
    ) -> Result<Self, StoreError> {
        let value = Self::new(access, refresh, expires_at, next_refresh_at, None)?;
        if value.expires_at == 0 {
            return Err(StoreError::InvalidTeslaMateTokenSchedule);
        }
        Ok(value)
    }

    fn new(
        access: Vec<u8>,
        refresh: Vec<u8>,
        expires_at: i64,
        next_refresh_at: i64,
        credential_generation: Option<Uuid>,
    ) -> Result<Self, StoreError> {
        if access.is_empty() || refresh.is_empty() {
            return Err(StoreError::TeslaMateTokenPairEmpty);
        }
        if access.len() > MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES
            || refresh.len() > MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES
        {
            return Err(StoreError::TeslaMateTokenCiphertextTooLarge);
        }
        let imported = expires_at == 0 && next_refresh_at == 0;
        let scheduled = expires_at > next_refresh_at && next_refresh_at > 0;
        if !imported && !scheduled {
            return Err(StoreError::InvalidTeslaMateTokenSchedule);
        }
        Ok(Self {
            access,
            refresh,
            expires_at,
            next_refresh_at,
            credential_generation,
        })
    }

    pub fn access(&self) -> &[u8] {
        &self.access
    }

    pub fn refresh(&self) -> &[u8] {
        &self.refresh
    }

    /// Epoch seconds when the access token expires; zero only before first refresh.
    pub const fn expires_at(&self) -> i64 {
        self.expires_at
    }

    /// Epoch seconds when the next refresh is due; zero only before first refresh.
    pub const fn next_refresh_at(&self) -> i64 {
        self.next_refresh_at
    }

    pub(crate) const fn credential_generation(&self) -> Option<Uuid> {
        self.credential_generation
    }

    pub(crate) fn with_credential_generation(
        &self,
        credential_generation: Uuid,
    ) -> Result<Self, StoreError> {
        if credential_generation.is_nil() {
            return Err(StoreError::InvalidLegacyRefreshGeneration);
        }
        Self::new(
            self.access.clone(),
            self.refresh.clone(),
            self.expires_at,
            self.next_refresh_at,
            Some(credential_generation),
        )
    }
}

impl std::fmt::Debug for TeslaMateLegacyTokenStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeslaMateLegacyTokenStore")
            .field("access", &"[redacted]")
            .field("refresh", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
            .field(
                "credential_generation",
                &self.credential_generation.map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// Encrypted official Fleet OAuth pair and its public refresh metadata.
#[derive(zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct FleetTokenStore {
    access: Vec<u8>,
    refresh: Vec<u8>,
    client_id: String,
    region: String,
    expires_at: i64,
    next_refresh_at: i64,
    #[zeroize(skip)]
    credential_generation: Option<Uuid>,
}

impl FleetTokenStore {
    pub fn new(
        access: Vec<u8>,
        refresh: Vec<u8>,
        client_id: String,
        region: String,
        expires_at: i64,
        next_refresh_at: i64,
        credential_generation: Option<Uuid>,
    ) -> Result<Self, StoreError> {
        if access.is_empty()
            || refresh.is_empty()
            || access.len() > MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES
            || refresh.len() > MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES
            || client_id.is_empty()
            || client_id.len() > 255
            || client_id.chars().any(char::is_control)
            || !matches!(region.as_str(), "na" | "eu" | "cn")
            || next_refresh_at <= 0
            || expires_at <= next_refresh_at
            || credential_generation.is_some_and(|generation| generation.is_nil())
        {
            return Err(StoreError::InvalidFleetTokenStore);
        }
        Ok(Self {
            access,
            refresh,
            client_id,
            region,
            expires_at,
            next_refresh_at,
            credential_generation,
        })
    }

    pub fn access(&self) -> &[u8] {
        &self.access
    }
    pub fn refresh(&self) -> &[u8] {
        &self.refresh
    }
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
    pub fn region(&self) -> &str {
        &self.region
    }
    pub const fn expires_at(&self) -> i64 {
        self.expires_at
    }
    pub const fn next_refresh_at(&self) -> i64 {
        self.next_refresh_at
    }
    pub(crate) const fn credential_generation(&self) -> Option<Uuid> {
        self.credential_generation
    }
}

impl std::fmt::Debug for FleetTokenStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FleetTokenStore")
            .field("access", &"[redacted]")
            .field("refresh", &"[redacted]")
            .field("client_id", &"[redacted]")
            .field("region", &self.region)
            .field("expires_at", &self.expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
            .field(
                "credential_generation",
                &self.credential_generation.map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
struct PrivateImportSpoolIdentity {
    uid: u32,
    gid: u32,
}

/// Create the private direct-import spool inside the user-owned Hub tree.
/// The 0700 mode is supplied to `mkdir(2)` rather than repaired afterwards.
fn ensure_private_import_spool_directory(
    path: &Path,
    expected: PrivateImportSpoolIdentity,
) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_import_spool_directory(path, &metadata, expected),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(PRIVATE_IMPORT_SPOOL_DIRECTORY_MODE);
            match builder.create(path) {
                Ok(()) => {
                    let metadata =
                        fs::symlink_metadata(path).map_err(StoreError::InspectImportSpool)?;
                    validate_private_import_spool_directory(path, &metadata, expected)
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata =
                        fs::symlink_metadata(path).map_err(StoreError::InspectImportSpool)?;
                    validate_private_import_spool_directory(path, &metadata, expected)
                }
                Err(error) => Err(StoreError::CreateImportSpool(error)),
            }
        }
        Err(error) => Err(StoreError::InspectImportSpool(error)),
    }
}

fn validate_private_import_spool_directory(
    path: &Path,
    metadata: &fs::Metadata,
    expected: PrivateImportSpoolIdentity,
) -> Result<(), StoreError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != expected.uid
        || metadata.gid() != expected.gid
        || metadata.permissions().mode() & 0o777 != PRIVATE_IMPORT_SPOOL_DIRECTORY_MODE
    {
        return Err(StoreError::UnsafeImportSpool(path.to_path_buf()));
    }
    Ok(())
}

fn private_import_spool_identity(
    data_root: &Path,
) -> Result<PrivateImportSpoolIdentity, StoreError> {
    let metadata = fs::symlink_metadata(data_root).map_err(StoreError::InspectImportSpool)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError::UnsafeImportSpool(data_root.to_path_buf()));
    }
    Ok(PrivateImportSpoolIdentity {
        uid: metadata.uid(),
        gid: metadata.gid(),
    })
}

fn private_import_spool_root(data_dir: &Path) -> PathBuf {
    data_dir.join(PRIVATE_IMPORT_SPOOL_DIRECTORY_NAME)
}

fn publication_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(".publication.lock")
}

/// Create the private catalogue inode before SQLite sees the path so platform
/// umasks cannot weaken the one-user 0600 contract.
fn ensure_shared_sqlite_catalogue_file(path: &Path) -> Result<(), StoreError> {
    let expected_gid = shared_sqlite_group_id(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => admit_or_repair_shared_sqlite_file(path, &metadata, expected_gid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(SHARED_SQLITE_FILE_MODE)
                .open(path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata =
                        fs::symlink_metadata(path).map_err(StoreError::InspectSharedSqlite)?;
                    return admit_or_repair_shared_sqlite_file(path, &metadata, expected_gid);
                }
                Err(error) => return Err(StoreError::CreateSharedSqlite(error)),
            };
            file.set_permissions(fs::Permissions::from_mode(SHARED_SQLITE_FILE_MODE))
                .map_err(StoreError::ProtectSharedSqlite)?;
            file.sync_all().map_err(StoreError::ProtectSharedSqlite)?;
            let metadata = fs::symlink_metadata(path).map_err(StoreError::InspectSharedSqlite)?;
            validate_shared_sqlite_file(path, &metadata, expected_gid)
        }
        Err(error) => Err(StoreError::InspectSharedSqlite(error)),
    }
}

fn shared_sqlite_group_id(database_path: &Path) -> Result<u32, StoreError> {
    let parent = database_path
        .parent()
        .ok_or_else(|| StoreError::UnsafeSharedSqlite(database_path.to_path_buf()))?;
    let metadata = fs::symlink_metadata(parent).map_err(StoreError::InspectSharedSqlite)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError::UnsafeSharedSqlite(parent.to_path_buf()));
    }
    Ok(metadata.gid())
}

fn admit_or_repair_shared_sqlite_sidecars(database_path: &Path) -> Result<(), StoreError> {
    let expected_gid = shared_sqlite_group_id(database_path)?;
    ["-wal", "-shm", "-journal"]
        .into_iter()
        .try_for_each(|suffix| {
            let path = PathBuf::from(format!("{}{}", database_path.display(), suffix));
            match fs::symlink_metadata(&path) {
                Ok(metadata) => admit_or_repair_shared_sqlite_file(&path, &metadata, expected_gid),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(StoreError::InspectSharedSqlite(error)),
            }
        })
}

fn admit_or_repair_shared_sqlite_file(
    path: &Path,
    metadata: &fs::Metadata,
    expected_gid: u32,
) -> Result<(), StoreError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.gid() != expected_gid {
        return Err(StoreError::UnsafeSharedSqlite(path.to_path_buf()));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode == SHARED_SQLITE_FILE_MODE {
        return Ok(());
    }
    if mode != 0o640 || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(StoreError::UnsafeSharedSqlite(path.to_path_buf()));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(SHARED_SQLITE_FILE_MODE))
        .map_err(StoreError::ProtectSharedSqlite)?;
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(StoreError::ProtectSharedSqlite)?;
    let metadata = fs::symlink_metadata(path).map_err(StoreError::InspectSharedSqlite)?;
    validate_shared_sqlite_file(path, &metadata, expected_gid)
}

fn validate_shared_sqlite_file(
    path: &Path,
    metadata: &fs::Metadata,
    expected_gid: u32,
) -> Result<(), StoreError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.gid() != expected_gid
        || metadata.permissions().mode() & 0o777 != SHARED_SQLITE_FILE_MODE
    {
        return Err(StoreError::UnsafeSharedSqlite(path.to_path_buf()));
    }
    Ok(())
}

fn stat_mode(raw_mode: impl Into<u64>) -> u32 {
    (raw_mode.into() as u32) & 0o7777
}

fn open_directory_path_nofollow(path: &Path) -> std::io::Result<File> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    #[cfg(target_os = "macos")]
    let absolute = {
        let mut absolute = absolute;
        for (alias, canonical) in [
            ("/tmp", "/private/tmp"),
            ("/var", "/private/var"),
            ("/etc", "/private/etc"),
        ] {
            if let Ok(suffix) = absolute.strip_prefix(alias) {
                absolute = Path::new(canonical).join(suffix);
                break;
            }
        }
        absolute
    };
    let mut descriptor = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    for component in absolute.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "directory path contains a non-normal component",
            ));
        };
        descriptor = openat(
            &descriptor,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
    }
    Ok(File::from(descriptor))
}

fn schema_22_noop_filename(vehicle_id: Uuid, snapshot_id: Uuid) -> String {
    format!("{vehicle_id}.{snapshot_id}.json")
}

fn parse_schema_22_noop_filename(name: &str, expected_vehicle_id: Uuid) -> Option<Uuid> {
    let mut parts = name.split('.');
    let vehicle = parts.next()?;
    let snapshot = parts.next()?;
    let vehicle_id = Uuid::parse_str(vehicle).ok()?;
    let snapshot_id = Uuid::parse_str(snapshot).ok()?;
    if parts.next()? != "json" || parts.next().is_some() || vehicle_id != expected_vehicle_id {
        return None;
    }
    (vehicle_id.to_string() == vehicle && snapshot_id.to_string() == snapshot)
        .then_some(snapshot_id)
}

fn is_schema_22_noop_temporary_filename(name: &str, expected_vehicle_id: Uuid) -> bool {
    let Some(body) = name.strip_prefix('.') else {
        return false;
    };
    let mut parts = body.split('.');
    let Some(vehicle) = parts.next() else {
        return false;
    };
    let Some(snapshot) = parts.next() else {
        return false;
    };
    let Some(attempt) = parts.next() else {
        return false;
    };
    let Some(suffix) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && suffix == "tmp"
        && Uuid::parse_str(vehicle).ok() == Some(expected_vehicle_id)
        && Uuid::parse_str(snapshot).is_ok_and(|value| value.to_string() == snapshot)
        && Uuid::parse_str(attempt).is_ok_and(|value| value.to_string() == attempt)
}

fn validate_schema_22_noop_file_stat(stat: &rustix::fs::Stat, gid: u32, mode: u32) -> bool {
    FileType::from_raw_mode(stat.st_mode).is_file()
        && stat.st_gid == gid
        && stat.st_nlink == 1
        && stat_mode(stat.st_mode) == mode
}

/// Opaque ownership token for the one supervised collector allowed to report
/// operational health. The random value fences a delayed or restarted process
/// from renewing or clearing a successor's lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SupervisedCollectorLease {
    instance_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SupervisedCollectorState {
    Active,
    AuthenticationTerminal,
}

impl SupervisedCollectorState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::AuthenticationTerminal => "auth_terminal",
        }
    }
}

/// Stable, redacted service-readiness reasons. These codes deliberately carry
/// no paths, source errors, vehicle identifiers, endpoints, or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessReasonCode {
    CatalogueUnavailable,
    LifecycleQuarantined,
    PublishedContentUnservable,
    CollectorAbsent,
    CollectorStale,
    CollectorAuthTerminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessFailure {
    pub code: ReadinessReasonCode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImmutableCatalogueFingerprint {
    bytes: u64,
    sha256: String,
}

/// The source-scoped completed-history rows that a TeslaMate import has
/// actually published.  This is deliberately separate from the live
/// materialisation tables: a later source rewrite may only tombstone rows for
/// which the importer has durable provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateImportProjectionInventory {
    pub source_id: Uuid,
    pub selected_car_id: i64,
    pub rows: Vec<ProjectionTombstone>,
}

/// Digest-only, source-owned projection state for the current immutable base
/// and lineage head. Unlike the legacy deletion inventory, this includes the
/// car row and is required before any changed-history import can be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateImportProjectionStateHeader {
    pub source_id: Uuid,
    pub base_snapshot_id: Uuid,
    pub selected_car_id: i64,
    pub head_sequence: u64,
}

/// Result of the one-time, unchanged-only legacy direct-import bridge. The
/// base and head are retained verbatim; the bridge writes neither a pack nor
/// a sequence reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TeslaMateLegacyDirectBridgeResult {
    pub snapshot_id: Uuid,
    pub head_sequence: u64,
    pub total_rows: u64,
}

/// A read-only, verified prior-state cursor bound to one vehicle/source/car
/// and the Hub's current immutable base/head. It intentionally owns a SQLite
/// connection so capture can use bounded lookup calls without loading a map.
pub struct TeslaMateImportProjectionStateLookup {
    connection: Connection,
    vehicle_id: Uuid,
    header: TeslaMateImportProjectionStateHeader,
    digest_caches: Vec<TeslaMateImportProjectionStateDigestCache>,
    #[cfg(test)]
    digest_cache_loads: usize,
}

// Direct source capture is grouped by entity and normally keyset-ordered
// within each group. Keep one fixed range per closed entity set, rather than a
// history-sized map: a direct successor with ten million unchanged positions
// performs a bounded number of SQLite reads while a backtrack replaces only
// that entity's range.
const TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS: usize = 1_024;

#[derive(Debug)]
struct TeslaMateImportProjectionStateDigestCache {
    entity: TeslaMateProjectionStateEntity,
    lower_bound_id: i64,
    rows: Vec<TeslaMateProjectionStateDigestRow>,
    exhausted: bool,
}

impl TeslaMateImportProjectionStateDigestCache {
    /// Returns `None` only when this range cannot answer the lookup. An inner
    /// `None` is an exact cached absence, including a gap between two stored
    /// IDs or the exhausted tail of an entity.
    fn digest(
        &self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    ) -> Option<Option<Sha256Digest>> {
        if self.entity != entity || id < self.lower_bound_id {
            return None;
        }
        let Some(last) = self.rows.last() else {
            return self.exhausted.then_some(None);
        };
        if id > last.id {
            return self.exhausted.then_some(None);
        }
        let digest = self
            .rows
            .binary_search_by_key(&id, |row| row.id)
            .ok()
            .map(|index| self.rows[index].digest);
        Some(digest)
    }
}

impl std::fmt::Debug for TeslaMateImportProjectionStateLookup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeslaMateImportProjectionStateLookup")
            .field("vehicle_id", &self.vehicle_id)
            .field("header", &self.header)
            .finish_non_exhaustive()
    }
}

impl TeslaMateImportProjectionStateLookup {
    pub fn header(&self) -> &TeslaMateImportProjectionStateHeader {
        &self.header
    }

    fn digest_store(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    ) -> Result<Option<Sha256Digest>, StoreError> {
        if id <= 0 {
            return Err(StoreError::TeslaMateProjectionState(
                TeslaMateProjectionStateError::InvalidRowId,
            ));
        }
        if let Some(cache) = self
            .digest_caches
            .iter()
            .find(|cache| cache.entity == entity)
            && let Some(digest) = cache.digest(entity, id)
        {
            return Ok(digest);
        }
        self.load_digest_cache(entity, id)?;
        self.digest_caches
            .iter()
            .find(|cache| cache.entity == entity)
            .and_then(|cache| cache.digest(entity, id))
            .ok_or(StoreError::LineageCatalogConflict)
    }

    fn load_digest_cache(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        lower_bound_id: i64,
    ) -> Result<(), StoreError> {
        let query_limit = i64::try_from(TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS)
            .expect("fixed cache limit fits SQLite integer");
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT entity, entity_ordinal, entity_id, car_id, projection_sha256
                   FROM teslamate_import_projection_state_rows
                  WHERE vehicle_id = ?1
                    AND entity_ordinal = ?2
                    AND entity_id >= ?3
                  ORDER BY entity_id ASC
                  LIMIT ?4",
            )
            .map_err(StoreError::LineageCatalog)?;
        let raw_rows = statement
            .query_map(
                params![
                    self.vehicle_id.to_string(),
                    i64::from(entity.ordinal()),
                    lower_bound_id,
                    query_limit,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .map_err(StoreError::LineageCatalog)?;
        let mut rows = Vec::with_capacity(TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS);
        let mut previous_id = None;
        for raw_row in raw_rows {
            let (stored_name, stored_ordinal, stored_id, stored_car_id, digest) =
                raw_row.map_err(StoreError::LineageCatalog)?;
            if stored_id < lower_bound_id
                || previous_id.is_some_and(|previous_id| stored_id <= previous_id)
                || stored_car_id != self.header.selected_car_id
                || stored_projection_state_entity(&stored_name, stored_ordinal)? != entity
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            previous_id = Some(stored_id);
            rows.push(TeslaMateProjectionStateDigestRow {
                entity,
                id: stored_id,
                car_id: stored_car_id,
                digest: projection_state_digest_from_blob(digest)?,
            });
        }
        if rows.len() > TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS {
            return Err(StoreError::LineageCatalogConflict);
        }
        let cache = TeslaMateImportProjectionStateDigestCache {
            entity,
            lower_bound_id,
            exhausted: rows.len() < TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS,
            rows,
        };
        if let Some(existing) = self
            .digest_caches
            .iter_mut()
            .find(|existing| existing.entity == entity)
        {
            *existing = cache;
        } else {
            if self.digest_caches.len() >= TeslaMateProjectionStateEntity::ALL.len() {
                return Err(StoreError::LineageCatalogConflict);
            }
            self.digest_caches.push(cache);
        }
        #[cfg(test)]
        {
            self.digest_cache_loads += 1;
        }
        Ok(())
    }

    fn page_after_store(
        &mut self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateDigestPage, StoreError> {
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(StoreError::TeslaMateProjectionState(
                TeslaMateProjectionStateError::InvalidPageSize,
            ));
        }
        if after.is_some_and(|cursor| cursor.id <= 0) {
            return Err(StoreError::TeslaMateProjectionState(
                TeslaMateProjectionStateError::InvalidCursor,
            ));
        }
        let (after_entity, after_id) = after.map_or((-1_i64, 0_i64), |cursor| {
            (i64::from(cursor.entity.ordinal()), cursor.id)
        });
        let query_limit = i64::from(limit) + 1;
        let mut statement = self
            .connection
            .prepare(
                "SELECT entity, entity_ordinal, entity_id, car_id, projection_sha256
                   FROM teslamate_import_projection_state_rows
                  WHERE vehicle_id = ?1
                    AND (entity_ordinal > ?2
                      OR (entity_ordinal = ?2 AND entity_id > ?3))
                  ORDER BY entity_ordinal ASC, entity_id ASC
                  LIMIT ?4",
            )
            .map_err(StoreError::LineageCatalog)?;
        let raw_rows = statement
            .query_map(
                params![
                    self.vehicle_id.to_string(),
                    after_entity,
                    after_id,
                    query_limit
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .map_err(StoreError::LineageCatalog)?;
        let mut rows = Vec::new();
        for raw_row in raw_rows {
            let (name, ordinal, id, car_id, digest) =
                raw_row.map_err(StoreError::LineageCatalog)?;
            if id <= 0 || car_id != self.header.selected_car_id {
                return Err(StoreError::LineageCatalogConflict);
            }
            rows.push(TeslaMateProjectionStateDigestRow {
                entity: stored_projection_state_entity(&name, ordinal)?,
                id,
                car_id,
                digest: projection_state_digest_from_blob(digest)?,
            });
        }
        let next_after = if rows.len() > usize::try_from(limit).expect("u32 fits usize") {
            rows.pop();
            rows.last().map(|row| TeslaMateProjectionStateCursor {
                entity: row.entity,
                id: row.id,
            })
        } else {
            None
        };
        Ok(TeslaMateProjectionStateDigestPage { rows, next_after })
    }
}

impl PriorProjectionStateLookup for TeslaMateImportProjectionStateLookup {
    fn digest(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    ) -> Result<Option<Sha256Digest>, Box<dyn std::error::Error + Send + Sync>> {
        self.digest_store(entity, id)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }

    fn page_after(
        &mut self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateDigestPage, Box<dyn std::error::Error + Send + Sync>> {
        self.page_after_store(after, limit)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }
}

/// A private, short-lived SQLite materialisation used only to authenticate a
/// compressed typed delta before the catalogue can reference it.
struct LineagePackInspection {
    path: PathBuf,
}

struct LegacyV2BaseBindingCandidate {
    vehicle_id: String,
    snapshot_id: String,
    base_sequence: i64,
    base_digest: String,
    packs_json: Vec<u8>,
    manifest_vehicle_id: Option<String>,
    manifest_head_sequence: Option<i64>,
    manifest_json: Option<Vec<u8>>,
    head_snapshot_id: Option<String>,
    head_sequence: Option<i64>,
    head_digest: Option<String>,
    terminal_cursor: Option<String>,
}

struct LegacyV2BaseDescription {
    installation_id: Uuid,
    account_id: Uuid,
    vehicle_id: Uuid,
    generation: u64,
    snapshot_id: Uuid,
    base_sequence: u64,
    base_digest: Sha256Digest,
    packs: Vec<TransportPack>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineagePackVerification {
    FullDigest,
    MetadataOnly,
}

impl Drop for LineagePackInspection {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Process-wide, advisory writer gate for workflows that mutate local
/// lifecycle state and then publish a full snapshot. The file descriptor owns
/// the lock and releases it automatically when the outer workflow returns.
///
/// This is intentionally acquired only by outer publication workflows. Lower
/// level catalogue and sequence methods remain ungated so one workflow cannot
/// re-enter the lock while it is already building packs.
#[derive(Debug)]
pub(crate) struct PublicationGate {
    _file: File,
}

/// One catalogue/pack backup point held under the publication gate from
/// capacity admission through the final immutable copy.
pub(crate) struct HubBackupSnapshot<'a> {
    store: &'a HubStore,
    publication_gate: PublicationGate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackCleanupOutcome {
    Retained,
    Missing,
    Removed,
}

struct SharedSchema22NoOpDirectory {
    file: File,
    gid: u32,
    path: PathBuf,
}

include!("db/store_opening.rs");
include!("db/legacy_credentials.rs");
include!("db/fleet_credentials.rs");
include!("db/maintenance.rs");
include!("db/publication.rs");
include!("db/live_sync.rs");
include!("db/import_finalization.rs");
include!("db/lineage_verification.rs");
include!("db/identity_and_audit.rs");
include!("db/observations_and_lifecycle.rs");
include!("db/geofences_and_terrain.rs");
include!("db/catalogue_models.rs");
include!("db/domain_models.rs");
include!("db/catalogue_helpers.rs");
include!("db/migrations.rs");
include!("db/catalogue_maintenance.rs");
include!("db/observation_persistence.rs");
include!("db/projection_helpers.rs");
include!("db/errors.rs");
include!("db/access_models.rs");
include!("db/import_state.rs");
include!("db/transaction_helpers.rs");

#[cfg(test)]
#[path = "db/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "db/terrain_background_tests.rs"]
mod terrain_background_tests;

#[cfg(test)]
#[path = "db/observation_verification_tests.rs"]
mod observation_verification_tests;
