use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
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
        ProjectionSnapshot, ProjectionTombstone,
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
pub const SCHEMA_VERSION: i32 = 49;
pub const BUNDLED_SQLITE_VERSION: &str = "3.53.2";

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

/// One encrypted TeslaMate legacy OAuth pair plus its refresh schedule.
/// Schedule values are epoch seconds. Ciphertext must not be logged or formatted.
pub struct TeslaMateLegacyTokenStore {
    access: Vec<u8>,
    refresh: Vec<u8>,
    expires_at: i64,
    next_refresh_at: i64,
}

impl TeslaMateLegacyTokenStore {
    /// Imported TeslaMate ciphertext has no Hub-owned refresh schedule yet.
    pub fn imported(access: Vec<u8>, refresh: Vec<u8>) -> Result<Self, StoreError> {
        Self::new(access, refresh, 0, 0)
    }

    /// A refreshed pair must have a positive, ordered refresh schedule.
    pub fn refreshed(
        access: Vec<u8>,
        refresh: Vec<u8>,
        expires_at: i64,
        next_refresh_at: i64,
    ) -> Result<Self, StoreError> {
        let value = Self::new(access, refresh, expires_at, next_refresh_at)?;
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
}

impl std::fmt::Debug for TeslaMateLegacyTokenStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeslaMateLegacyTokenStore")
            .field("access", &"[redacted]")
            .field("refresh", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
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

fn stat_mode(raw_mode: u16) -> u32 {
    u32::from(Mode::from_raw_mode(raw_mode).as_raw_mode()) & 0o7777
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

struct SharedSchema22NoOpDirectory {
    file: File,
    gid: u32,
    path: PathBuf,
}

impl HubStore {
    pub fn initialize(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref();
        let data_dir_was_absent = !data_dir.exists();
        fs::create_dir_all(data_dir).map_err(StoreError::CreateDataDir)?;
        // Existing roots are admitted as-is; only newly-created roots receive
        // the private one-user mode here.
        if data_dir_was_absent {
            fs::set_permissions(
                data_dir,
                fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
            )
            .map_err(StoreError::ProtectDataDir)?;
        }
        let packs_dir = data_dir.join("packs");
        let packs_dir_was_absent = !packs_dir.exists();
        fs::create_dir_all(&packs_dir).map_err(StoreError::CreatePacksDir)?;
        if packs_dir_was_absent {
            fs::set_permissions(
                &packs_dir,
                fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
            )
            .map_err(StoreError::ProtectPacksDir)?;
        }

        let store = Self {
            database_path: data_dir.join("hub.sqlite"),
            packs_dir,
            private_import_spool_dir: private_import_spool_root(data_dir),
            publication_lock_path: publication_lock_path(data_dir),
            immutable_snapshot: None,
            #[cfg(test)]
            stream_fault: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            projection_state_detach_fault: Arc::new(Mutex::new(false)),
        };
        store.ensure_schema_22_noop_directory()?;
        ensure_shared_sqlite_catalogue_file(&store.database_path)?;
        let mut connection = store.open()?;
        migrate(&connection)?;
        ensure_installation_id(&connection)?;
        store.recover_legacy_v2_base_bindings(&mut connection)?;
        // Never discard a staged import while another process owns its
        // publication workflow. A busy gate makes startup retryable instead.
        let publication_gate = store.try_acquire_publication_gate()?;
        store.recover_stale_import_projection_state_spools(&publication_gate, &connection)?;
        cleanup_abandoned_import_generations(&connection)?;
        Ok(store)
    }

    pub fn open(&self) -> Result<Connection, StoreError> {
        admit_or_repair_shared_sqlite_sidecars(&self.database_path)?;
        let connection = Connection::open_with_flags(
            &self.database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::Open)?;
        configure(&connection)?;
        admit_or_repair_shared_sqlite_sidecars(&self.database_path)?;
        Ok(connection)
    }

    /// Schema 36 introduced immutable V2 projection bindings, but historical
    /// catalogues could already contain a V2 base when that table was created.
    /// Recover only from the stored base manifest and its content-addressed
    /// ordinal-zero car pack. Mutable source aliases, vehicle ownership and
    /// materialised lifecycle rows are deliberately outside this proof.
    fn recover_legacy_v2_base_bindings(
        &self,
        connection: &mut Connection,
    ) -> Result<(), StoreError> {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let candidates = {
            let mut statement = transaction
                .prepare(
                    "SELECT base.vehicle_id, base.snapshot_id, base.base_sequence,
                            base.base_digest, base.packs_json,
                            manifest.vehicle_id, manifest.head_sequence, manifest.manifest_json,
                            head.base_snapshot_id, head.head_sequence,
                            head.head_digest, head.terminal_cursor
                       FROM sync_bases AS base
                       LEFT JOIN sync_manifests AS manifest
                         ON manifest.snapshot_id = base.snapshot_id
                       LEFT JOIN sync_heads AS head
                         ON head.vehicle_id = base.vehicle_id
                       LEFT JOIN v2_base_bindings AS binding
                         ON binding.vehicle_id = base.vehicle_id
                      WHERE binding.vehicle_id IS NULL
                      ORDER BY base.vehicle_id",
                )
                .map_err(StoreError::LineageCatalog)?;
            statement
                .query_map([], |row| {
                    Ok(LegacyV2BaseBindingCandidate {
                        vehicle_id: row.get(0)?,
                        snapshot_id: row.get(1)?,
                        base_sequence: row.get(2)?,
                        base_digest: row.get(3)?,
                        packs_json: row.get(4)?,
                        manifest_vehicle_id: row.get(5)?,
                        manifest_head_sequence: row.get(6)?,
                        manifest_json: row.get(7)?,
                        head_snapshot_id: row.get(8)?,
                        head_sequence: row.get(9)?,
                        head_digest: row.get(10)?,
                        terminal_cursor: row.get(11)?,
                    })
                })
                .map_err(StoreError::LineageCatalog)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::LineageCatalog)?
        };

        let installation_id: String = transaction
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![INSTALLATION_ID_KEY],
                |row| row.get(0),
            )
            .map_err(StoreError::InstallationIdentity)?;
        let installation_id = installation_id
            .parse::<Uuid>()
            .ok()
            .filter(|value| !value.is_nil())
            .ok_or(StoreError::InvalidStoredUuid("installation identity"))?;

        for candidate in candidates {
            let manifest_json = candidate
                .manifest_json
                .as_deref()
                .ok_or(StoreError::LineageCatalogConflict)?;
            let Some(base) = legacy_v2_base_description(manifest_json)? else {
                // Generic V1 lineage bases do not carry a projection binding.
                continue;
            };
            if base.installation_id != installation_id
                || candidate.vehicle_id != base.vehicle_id.to_string()
                || candidate.snapshot_id != base.snapshot_id.to_string()
                || candidate.manifest_vehicle_id.as_deref() != Some(candidate.vehicle_id.as_str())
                || u64::try_from(candidate.base_sequence).ok() != Some(base.base_sequence)
                || candidate.manifest_head_sequence != Some(candidate.base_sequence)
                || candidate.base_digest != base.base_digest.to_string()
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            let stored_packs: Vec<TransportPack> = serde_json::from_slice(&candidate.packs_json)
                .map_err(StoreError::DeserializeManifest)?;
            if stored_packs != base.packs || stored_packs.is_empty() {
                return Err(StoreError::LineageCatalogConflict);
            }
            let head_sequence = candidate
                .head_sequence
                .and_then(|value| u64::try_from(value).ok())
                .ok_or(StoreError::LineageCatalogConflict)?;
            let head_digest = candidate
                .head_digest
                .as_deref()
                .ok_or(StoreError::LineageCatalogConflict)?
                .parse::<Sha256Digest>()
                .map_err(|_| StoreError::LineageCatalogConflict)?;
            let terminal_cursor = candidate
                .terminal_cursor
                .as_deref()
                .ok_or(StoreError::LineageCatalogConflict)?;
            let terminal_cursor: OpaqueCursor =
                serde_json::from_str(terminal_cursor).map_err(StoreError::DeserializeManifest)?;
            terminal_cursor
                .validate_shape()
                .map_err(StoreError::Manifest)?;
            if candidate.head_snapshot_id.as_deref() != Some(candidate.snapshot_id.as_str())
                || head_sequence < base.base_sequence
                || (head_sequence == base.base_sequence && head_digest != base.base_digest)
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            for pack in &base.packs {
                let catalogued: Option<(String, i64, String, i64, i64)> = transaction
                    .query_row(
                        "SELECT snapshot_id, ordinal, relative_path,
                                compressed_bytes, uncompressed_bytes
                           FROM sync_packs WHERE sha256 = ?1",
                        params![pack.sha256.to_string()],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(StoreError::LineageCatalog)?;
                let expected = (
                    pack.snapshot_id.to_string(),
                    i64::from(pack.ordinal),
                    pack.relative_path.clone(),
                    i64::try_from(pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                    i64::try_from(pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                );
                if catalogued.as_ref() != Some(&expected) {
                    return Err(StoreError::LineageCatalogConflict);
                }
            }

            let selected_car_id = self.inspect_legacy_v2_base_car_pack(
                base.packs
                    .first()
                    .ok_or(StoreError::LineageCatalogConflict)?,
                &base,
            )?;
            let binding = ProjectionBinding {
                installation_id: base.installation_id,
                account_id: base.account_id,
                vehicle_id: base.vehicle_id,
                generation: base.generation,
                selected_car_id,
            };
            record_immutable_v2_base_binding_values_in_transaction(
                &transaction,
                base.vehicle_id,
                base.snapshot_id,
                &binding,
            )?;
        }
        transaction.commit().map_err(StoreError::LineageCatalog)
    }

    /// Open an existing live Hub catalogue without creating directories,
    /// migrating schema, or issuing mutating SQL. SQLite may still create WAL
    /// coordination files while observing a concurrently active catalogue.
    pub fn open_read_only(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_read_only_with_mode(data_dir.as_ref(), false)
    }

    /// Open a byte-stable immutable snapshot for operator diagnosis. This
    /// refuses a pending WAL and must be followed by
    /// [`Self::verify_immutable_snapshot_unchanged`] before reporting success.
    pub fn open_immutable_read_only(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_read_only_with_mode(data_dir.as_ref(), true)
    }

    fn open_read_only_with_mode(data_dir: &Path, immutable: bool) -> Result<Self, StoreError> {
        let database_path = data_dir.join("hub.sqlite");
        let immutable_snapshot = if immutable {
            Some(immutable_catalogue_fingerprint(&database_path)?)
        } else {
            None
        };
        let store = Self {
            database_path,
            packs_dir: data_dir.join("packs"),
            private_import_spool_dir: private_import_spool_root(data_dir),
            publication_lock_path: publication_lock_path(data_dir),
            immutable_snapshot,
            #[cfg(test)]
            stream_fault: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            projection_state_detach_fault: Arc::new(Mutex::new(false)),
        };
        let connection = store.open_read_only_connection()?;
        let application_id: i32 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        if application_id != APPLICATION_ID {
            return Err(StoreError::InvalidApplicationId(application_id));
        }
        let version = schema_version(&connection)?;
        if version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema(version));
        }
        Ok(store)
    }

    fn open_read_only_connection(&self) -> Result<Connection, StoreError> {
        let connection = if self.immutable_snapshot.is_some() {
            let canonical = self
                .database_path
                .canonicalize()
                .map_err(StoreError::ResolveCataloguePath)?;
            let mut uri = url::Url::from_file_path(&canonical)
                .map_err(|()| StoreError::InvalidCataloguePath)?;
            uri.set_query(Some("immutable=1&mode=ro"));
            Connection::open_with_flags(
                uri.as_str(),
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(StoreError::Open)?
        } else {
            Connection::open_with_flags(
                &self.database_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(StoreError::Open)?
        };
        configure_read_only(&connection)?;
        Ok(connection)
    }

    /// Prove that the immutable catalogue diagnosed by `doctor` was not stale
    /// or raced by a writer while its checks were running.
    pub fn verify_immutable_snapshot_unchanged(&self) -> Result<(), StoreError> {
        let expected = self
            .immutable_snapshot
            .as_ref()
            .ok_or(StoreError::ImmutableSnapshotRequired)?;
        let actual = immutable_catalogue_fingerprint(&self.database_path)?;
        if &actual == expected {
            Ok(())
        } else {
            Err(StoreError::CatalogueChangedDuringImmutableCheck)
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Atomically replace the sole persisted TeslaMate token pair.
    pub fn replace_teslamate_legacy_tokens(
        &self,
        tokens: &TeslaMateLegacyTokenStore,
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "INSERT INTO teslamate_legacy_tokens(
                    singleton_id, access, refresh, expires_at, next_refresh_at
                 ) VALUES (1, ?1, ?2, ?3, ?4)
                 ON CONFLICT(singleton_id) DO UPDATE SET
                    access = excluded.access,
                    refresh = excluded.refresh,
                    expires_at = excluded.expires_at,
                    next_refresh_at = excluded.next_refresh_at",
                params![
                    tokens.access(),
                    tokens.refresh(),
                    tokens.expires_at(),
                    tokens.next_refresh_at(),
                ],
            )
            .map_err(StoreError::TeslaMateTokenStore)?;
        transaction
            .commit()
            .map_err(StoreError::TeslaMateTokenStore)
    }

    /// Load the sole persisted TeslaMate token pair without decrypting it.
    pub fn load_teslamate_legacy_tokens(
        &self,
    ) -> Result<Option<TeslaMateLegacyTokenStore>, StoreError> {
        let connection = self.open()?;
        let row: Option<(Vec<u8>, Vec<u8>, i64, i64)> = connection
            .query_row(
                "SELECT access, refresh, expires_at, next_refresh_at
                 FROM teslamate_legacy_tokens WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::TeslaMateTokenStore)?;
        row.map(|(access, refresh, expires_at, next_refresh_at)| {
            TeslaMateLegacyTokenStore::new(access, refresh, expires_at, next_refresh_at)
        })
        .transpose()
    }

    /// Serialize complete local publication workflows across every Hub process
    /// sharing this data directory. Callers must keep the returned guard alive
    /// from before sequence reservation until catalogue, lifecycle, and pack
    /// ownership work has completed.
    pub(crate) async fn acquire_publication_gate(&self) -> Result<PublicationGate, StoreError> {
        let file = self.open_publication_gate()?;
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(PublicationGate { _file: file }),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(PUBLICATION_GATE_RETRY).await;
                }
                Err(error) => return Err(StoreError::LockPublicationGate(error.into())),
            }
        }
    }

    /// Attempt to acquire the publication gate without ever waiting. This is
    /// used only by synchronous library seams; async production workflows use
    /// `acquire_publication_gate` so contention yields to Tokio rather than
    /// blocking a worker thread.
    pub(crate) fn try_acquire_publication_gate(&self) -> Result<PublicationGate, StoreError> {
        let file = self.open_publication_gate()?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(PublicationGate { _file: file }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Err(StoreError::PublicationGateBusy)
            }
            Err(error) => Err(StoreError::LockPublicationGate(error.into())),
        }
    }

    fn open_publication_gate(&self) -> Result<File, StoreError> {
        let expected_gid = shared_sqlite_group_id(&self.database_path)?;
        let path = &self.publication_lock_path;
        let parent_path = path
            .parent()
            .ok_or_else(|| StoreError::UnsafePublicationGate(path.clone()))?;
        let name = path
            .file_name()
            .ok_or_else(|| StoreError::UnsafePublicationGate(path.clone()))?;
        let parent_fd = open(
            parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| StoreError::OpenPublicationGate(error.into()))?;
        let parent = File::from(parent_fd);
        let parent_metadata =
            fstat(&parent).map_err(|error| StoreError::OpenPublicationGate(error.into()))?;
        if !FileType::from_raw_mode(parent_metadata.st_mode).is_dir()
            || parent_metadata.st_gid != expected_gid
        {
            return Err(StoreError::UnsafePublicationGate(parent_path.to_path_buf()));
        }

        let mut created = false;
        let gate_fd = loop {
            match openat(
                &parent,
                name,
                OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => break fd,
                Err(Errno::NOENT) => match openat(
                    &parent,
                    name,
                    OFlags::RDWR
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(SHARED_DATA_FILE_MODE as u16),
                ) {
                    Ok(fd) => {
                        created = true;
                        break fd;
                    }
                    Err(Errno::EXIST) => continue,
                    Err(error) => return Err(StoreError::OpenPublicationGate(error.into())),
                },
                Err(error) => return Err(StoreError::OpenPublicationGate(error.into())),
            }
        };
        let file = File::from(gate_fd);
        if created {
            fchmod(&file, Mode::from_raw_mode(SHARED_DATA_FILE_MODE as u16))
                .map_err(|error| StoreError::ProtectPublicationGate(error.into()))?;
            file.sync_all()
                .map_err(StoreError::ProtectPublicationGate)?;
            parent
                .sync_all()
                .map_err(StoreError::ProtectPublicationGate)?;
        }
        let metadata =
            fstat(&file).map_err(|error| StoreError::OpenPublicationGate(error.into()))?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file()
            || metadata.st_gid != expected_gid
            || Mode::from_raw_mode(metadata.st_mode).as_raw_mode() as u32 & 0o777
                != SHARED_DATA_FILE_MODE
        {
            return Err(StoreError::UnsafePublicationGate(path.clone()));
        }
        Ok(file)
    }

    pub fn upsert_car_settings(
        &self,
        vehicle_id: Uuid,
        car_id: i64,
        settings: &ProjectionCarSettings,
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let before = load_car_settings_row(&transaction, vehicle_id)?;
        transaction
            .execute(
                "INSERT INTO car_settings(
                    vehicle_id, car_id, enabled, use_streaming_api,
                    suspend_after_idle_min, suspend_min, req_not_unlocked,
                    free_supercharging, lfp_battery, suspend_min_resolved
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    car_id=excluded.car_id, enabled=excluded.enabled,
                    use_streaming_api=excluded.use_streaming_api,
                    suspend_after_idle_min=excluded.suspend_after_idle_min,
                    suspend_min=CASE WHEN car_settings.suspend_min_resolved != 0
                        THEN car_settings.suspend_min ELSE excluded.suspend_min END,
                    suspend_min_resolved=MAX(car_settings.suspend_min_resolved,
                        excluded.suspend_min_resolved),
                    req_not_unlocked=excluded.req_not_unlocked,
                    free_supercharging=excluded.free_supercharging,
                    lfp_battery=excluded.lfp_battery",
                params![
                    vehicle_id.to_string(),
                    car_id,
                    settings.enabled,
                    settings.use_streaming_api,
                    settings.suspend_after_idle_min,
                    settings.suspend_min,
                    settings.req_not_unlocked,
                    settings.free_supercharging,
                    settings.lfp_battery,
                    settings.suspend_min_resolved,
                ],
            )
            .map_err(StoreError::Query)?;
        let (effective_car_id, effective_settings) =
            load_car_settings_row(&transaction, vehicle_id)?
                .ok_or_else(|| StoreError::Query(rusqlite::Error::QueryReturnedNoRows))?;
        let changed = before.as_ref() != Some(&(effective_car_id, effective_settings.clone()));
        if !changed {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(());
        }
        let current_car: Option<String> = transaction
            .query_row(
                "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if let Some(current_car) = current_car {
            let mut car: ProjectionCar =
                serde_json::from_str(&current_car).map_err(StoreError::DeserializeLifecycleRow)?;
            car.settings = effective_settings.clone();
            let car_json =
                serde_json::to_string(&car).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_cars SET car_json = ?1 WHERE vehicle_id = ?2",
                    params![car_json, vehicle_id.to_string()],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        let payload = serde_json::to_string(&effective_settings)
            .map_err(StoreError::SerializeLifecycleRow)?;
        record_sync_mutation_in_transaction(
            &transaction,
            vehicle_id,
            "car_setting",
            effective_car_id,
            effective_car_id,
            "upsert",
            &payload,
        )?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(())
    }

    /// Materialise the first car record without replacing an existing
    /// authoritative record. Later lifecycle metadata patches update it.
    pub fn persist_materialised_car_if_absent(
        &self,
        vehicle_id: Uuid,
        car: &crate::hub_pack::ProjectionCar,
    ) -> Result<(), StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let car_json = serde_json::to_string(car).map_err(StoreError::SerializeLifecycleRow)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let inserted = transaction
            .execute(
                "INSERT INTO materialised_cars(vehicle_id, car_id, car_json)
                 VALUES (?1, ?2, ?3) ON CONFLICT(vehicle_id) DO NOTHING",
                params![vehicle_id.to_string(), car.id, car_json],
            )
            .map_err(StoreError::LifecycleWrite)?;
        if inserted != 0 {
            record_sync_mutation_in_transaction(
                &transaction,
                vehicle_id,
                "car",
                car.id,
                car.id,
                "upsert",
                &car_json,
            )?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    pub fn load_car_settings(&self, vehicle_id: Uuid) -> Result<ProjectionCarSettings, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT enabled, use_streaming_api, suspend_after_idle_min, suspend_min,
                        req_not_unlocked, free_supercharging, lfp_battery,
                        suspend_min_resolved
                 FROM car_settings WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    Ok(ProjectionCarSettings {
                        enabled: row.get::<_, i64>(0)? != 0,
                        use_streaming_api: row.get::<_, i64>(1)? != 0,
                        suspend_after_idle_min: row.get(2)?,
                        suspend_min: row.get(3)?,
                        suspend_min_resolved: row.get::<_, i64>(7)? != 0,
                        req_not_unlocked: row.get::<_, i64>(4)? != 0,
                        free_supercharging: row.get::<_, i64>(5)? != 0,
                        lfp_battery: row.get::<_, i64>(6)? != 0,
                    })
                },
            )
            .optional()
            .map(|settings| settings.unwrap_or_default())
            .map_err(StoreError::Query)
    }

    pub fn resolve_car_suspend_min(
        &self,
        vehicle_id: Uuid,
        model: Option<&str>,
        trim_badging: Option<&str>,
        marketing_name: Option<&str>,
    ) -> Result<bool, StoreError> {
        let Some(suspend_min) =
            crate::hub_pack::teslamate_suspend_min_default(model, trim_badging, marketing_name)
        else {
            return Ok(false);
        };
        let connection = self.open()?;
        let changed = connection
            .execute(
                "UPDATE car_settings
                 SET suspend_min = ?1, suspend_min_resolved = 1
                 WHERE vehicle_id = ?2 AND suspend_min_resolved = 0",
                params![suspend_min, vehicle_id.to_string()],
            )
            .map_err(StoreError::Query)?;
        Ok(changed != 0)
    }

    /// Create one consistent SQLite catalogue backup through SQLite's online
    /// backup API. The destination must be a new Hub-owned file; packs are
    /// intentionally handled by a separate immutable-object backup step.
    pub fn backup_catalogue_to(&self, destination: &Path) -> Result<(), StoreError> {
        if destination == self.database_path {
            return Err(StoreError::BackupDestinationIsLiveDatabase);
        }
        if destination.exists() {
            return Err(StoreError::BackupDestinationExists(
                destination.to_path_buf(),
            ));
        }
        let source = self.open()?;
        let mut backup_destination = Connection::open(destination).map_err(StoreError::Open)?;
        let result = Backup::new(&source, &mut backup_destination)
            .and_then(|backup| backup.run_to_completion(128, Duration::ZERO, None));
        drop(backup_destination);
        match result {
            Ok(()) => {
                // This is a newly created catalogue, not an upgrade repair.
                // Give a later split-UID HubStore the same group-writable
                // catalogue shape it requires for the live tree.
                fs::set_permissions(
                    destination,
                    fs::Permissions::from_mode(SHARED_SQLITE_FILE_MODE),
                )
                .map_err(StoreError::ProtectSharedSqlite)?;
                File::open(destination)
                    .and_then(|file| file.sync_all())
                    .map_err(StoreError::ProtectSharedSqlite)?;
                Ok(())
            }
            Err(error) => {
                let _ = fs::remove_file(destination);
                Err(StoreError::Backup(error))
            }
        }
    }

    /// Create a complete Hub-owned restore directory. The catalogue is copied
    /// first through SQLite's online backup API; immutable packs are then
    /// copied from the exact referenced set in that copied catalogue.
    pub fn backup_to(&self, destination: &Path) -> Result<(), StoreError> {
        if destination.exists() {
            return Err(StoreError::BackupDestinationExists(
                destination.to_path_buf(),
            ));
        }
        // Snapshot-keyed no-op files are outside SQLite. Hold the same gate
        // as pair publication so the copied catalogue and copied sidecars
        // describe one servable point in time.
        let publication_gate = self.try_acquire_publication_gate()?;
        fs::create_dir(destination).map_err(StoreError::CreateBackupDirectory)?;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
        )
        .map_err(StoreError::CreateBackupDirectory)?;
        let result = self.backup_to_created_directory(destination, &publication_gate);
        if result.is_err() {
            let _ = fs::remove_dir_all(destination);
        }
        result
    }

    fn backup_to_created_directory(
        &self,
        destination: &Path,
        publication_gate: &PublicationGate,
    ) -> Result<(), StoreError> {
        let catalogue = destination.join("hub.sqlite");
        self.backup_catalogue_to(&catalogue)?;
        let copied_catalogue = Connection::open_with_flags(
            &catalogue,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::Open)?;
        let rows = referenced_pack_rows_at(&copied_catalogue, retired_lineage_clock_ms()?)?;
        let packs = destination.join("packs").join("sha256");
        fs::create_dir_all(&packs).map_err(StoreError::CreateBackupDirectory)?;
        fs::set_permissions(
            destination.join("packs"),
            fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
        )
        .and_then(|()| {
            fs::set_permissions(
                &packs,
                fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
            )
        })
        .map_err(StoreError::CreateBackupDirectory)?;
        for (sha256, relative_path, expected_bytes) in rows {
            let expected_bytes =
                u64::try_from(expected_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
            if !is_sha256_hex(&sha256)
                || relative_path != format!("/v1/packs/sha256/{sha256}.sqlite.zst")
            {
                return Err(StoreError::UnsafeStoredPackPath);
            }
            let filename = format!("{sha256}.sqlite.zst");
            let source = self.packs_dir.join("sha256").join(&filename);
            let backup = packs.join(&filename);
            let copied =
                fs::copy(&source, &backup).map_err(|source_error| StoreError::CopyBackupPack {
                    source_path: source.clone(),
                    destination: backup.clone(),
                    source_error,
                })?;
            if copied != expected_bytes {
                return Err(StoreError::BackupPackSizeMismatch {
                    path: source,
                    expected: expected_bytes,
                    actual: copied,
                });
            }
            if sha256_file_hex(&backup)? != sha256 {
                return Err(StoreError::BackupPackDigestMismatch { path: backup });
            }
        }
        self.backup_current_schema_22_noops(
            destination,
            &catalogue,
            &copied_catalogue,
            publication_gate,
        )?;
        Ok(())
    }

    fn backup_current_schema_22_noops(
        &self,
        destination: &Path,
        catalogue: &Path,
        copied_catalogue: &Connection,
        publication_gate: &PublicationGate,
    ) -> Result<(), StoreError> {
        let manifest_rows = copied_catalogue
            .prepare(
                "SELECT manifest_json FROM sync_manifests
                 WHERE json_extract(manifest_json, '$.mode') = 'full_snapshot'
                 ORDER BY vehicle_id, head_sequence DESC, snapshot_id DESC",
            )
            .map_err(StoreError::Query)?
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        let backup_store = Self {
            database_path: catalogue.to_path_buf(),
            packs_dir: destination.join("packs"),
            private_import_spool_dir: private_import_spool_root(destination),
            publication_lock_path: publication_lock_path(destination),
            immutable_snapshot: None,
            #[cfg(test)]
            stream_fault: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            projection_state_detach_fault: Arc::new(Mutex::new(false)),
        };
        let mut visited = HashSet::new();
        for payload in manifest_rows {
            let manifest = decode_manifest(payload)?;
            if !visited.insert(manifest.vehicle_id) || manifest.schema != HUB_PROJECTION_SCHEMA_V3 {
                continue;
            }
            let bytes = self
                .schema_22_noop_for_snapshot(manifest.vehicle_id, manifest.snapshot_id)?
                .ok_or(StoreError::Schema22NoOpNotFound)?;
            let noop: crate::updates_delivery::SignedNoOpState = serde_json::from_slice(&bytes)
                .map_err(|error| StoreError::InvalidSchema22Pair(error.to_string()))?;
            let canonical = serde_json::to_vec(&noop)
                .map_err(|error| StoreError::InvalidSchema22Pair(error.to_string()))?;
            if canonical != bytes {
                return Err(StoreError::InvalidSchema22Pair(
                    "no-op is not canonical typed JSON".into(),
                ));
            }
            crate::updates_delivery::validate_schema_22_pair(&manifest, &noop)
                .map_err(|error| StoreError::InvalidSchema22Pair(error.message))?;
            backup_store.publish_schema_22_noop(publication_gate, &noop)?;
        }
        Ok(())
    }

    pub fn packs_dir(&self) -> &Path {
        &self.packs_dir
    }

    /// Construct a production direct-import state spool only while the caller
    /// holds the publication gate and the exact generation is still staging.
    /// This is deliberately narrower than the generic projection-state
    /// constructor used by isolated tests and non-generation seams.
    pub(crate) fn create_import_projection_state(
        &self,
        _publication_gate: &PublicationGate,
        run_id: Uuid,
        limits: TeslaMateProjectionStateLimits,
        maximum_changed_row_payload_bytes: u64,
    ) -> Result<TeslaMateProjectionState, StoreError> {
        if run_id.is_nil() {
            return Err(StoreError::InvalidImportGeneration);
        }
        let connection = self.open()?;
        let staging: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM import_generations
                     WHERE run_id = ?1 AND status = 'staging'
                )",
                params![run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::ImportGeneration)?;
        if !staging {
            return Err(StoreError::ImportGenerationNotFound);
        }
        let data_root = self
            .database_path
            .parent()
            .expect("Hub database path always has a data directory");
        let expected_spool_identity = private_import_spool_identity(data_root)?;
        if rustix::process::geteuid().as_raw() != expected_spool_identity.uid {
            return Err(StoreError::UnsafeImportSpool(
                self.private_import_spool_dir.clone(),
            ));
        }
        ensure_private_import_spool_directory(
            &self.private_import_spool_dir,
            expected_spool_identity,
        )?;
        TeslaMateProjectionState::create_for_import_generation(
            &self.private_import_spool_dir,
            run_id,
            limits,
            maximum_changed_row_payload_bytes,
        )
        .map_err(StoreError::TeslaMateProjectionState)
    }

    fn recover_stale_import_projection_state_spools(
        &self,
        _publication_gate: &PublicationGate,
        connection: &Connection,
    ) -> Result<(), StoreError> {
        // `initialize` holds the same process-wide gate held through every
        // production capture. A v1 run can therefore be reclaimed only here,
        // after the namespace has been fully validated by the state module.
        // A collector-only process must not create the Hub-private import
        // spool during ordinary startup. It is created atomically by the Hub
        // import identity only when a direct import is actually admitted.
        let data_root = self
            .database_path
            .parent()
            .expect("Hub database path always has a data directory");
        let expected_spool_identity = private_import_spool_identity(data_root)?;
        let spool_metadata = match fs::symlink_metadata(&self.private_import_spool_dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StoreError::InspectImportSpool(error)),
            Ok(metadata) => metadata,
        };
        validate_private_import_spool_directory(
            &self.private_import_spool_dir,
            &spool_metadata,
            expected_spool_identity,
        )?;
        match fs::read_dir(&self.private_import_spool_dir) {
            // The collector deliberately cannot traverse the Hub-only spool.
            // It must never reclaim another identity's interrupted import;
            // the next Hub/API startup performs the owned recovery instead.
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    && rustix::process::geteuid().as_raw() != expected_spool_identity.uid =>
            {
                return Ok(());
            }
            Err(error) => return Err(StoreError::InspectImportSpool(error)),
            Ok(_) => {}
        }
        for run_id in recover_stale_import_generation_spools(&self.private_import_spool_dir)? {
            connection
                .execute(
                    "DELETE FROM import_generations
                      WHERE run_id = ?1 AND status = 'staging'",
                    params![run_id.to_string()],
                )
                .map_err(StoreError::ImportGeneration)?;
        }
        Ok(())
    }

    /// Private, disposable local capture area. TeslaMate source snapshots are
    /// never written into the Hub catalogue database.
    pub fn imports_dir(&self) -> PathBuf {
        self.database_path
            .parent()
            .expect("Hub database path always has a data directory")
            .join("imports")
    }

    /// Atomically claim the singleton supervised-collector lease. A live
    /// predecessor cannot be displaced; an expired predecessor can be
    /// replaced without deleting its crash evidence first.
    pub(crate) fn acquire_supervised_collector_lease(
        &self,
        now_ms: i64,
    ) -> Result<SupervisedCollectorLease, StoreError> {
        validate_timestamp("collector lease acquisition", now_ms)?;
        let lease_until_ms = supervised_collector_lease_deadline(now_ms)?;
        let lease = SupervisedCollectorLease {
            instance_id: Uuid::new_v4(),
        };
        let connection = self.open()?;
        let changed = connection
            .execute(
                "INSERT INTO supervised_collector_lease(
                    singleton_id, instance_id, state, started_at_ms,
                    heartbeat_at_ms, lease_until_ms
                 ) VALUES (1, ?1, 'active', ?2, ?2, ?3)
                 ON CONFLICT(singleton_id) DO UPDATE SET
                    instance_id = excluded.instance_id,
                    state = excluded.state,
                    started_at_ms = excluded.started_at_ms,
                    heartbeat_at_ms = excluded.heartbeat_at_ms,
                    lease_until_ms = excluded.lease_until_ms
                 WHERE supervised_collector_lease.lease_until_ms <= excluded.heartbeat_at_ms",
                params![lease.instance_id.to_string(), now_ms, lease_until_ms],
            )
            .map_err(StoreError::SupervisedCollectorLeaseWrite)?;
        if changed == 1 {
            Ok(lease)
        } else {
            Err(StoreError::SupervisedCollectorLeaseHeld)
        }
    }

    /// Renew only the exact lease owned by this process. The macOS data-dir
    /// lock is the process singleton, so a delayed local heartbeat may revive
    /// its expired readiness record unless another instance has replaced it.
    pub(crate) fn heartbeat_supervised_collector_lease(
        &self,
        lease: SupervisedCollectorLease,
        state: SupervisedCollectorState,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("collector heartbeat", now_ms)?;
        let lease_until_ms = supervised_collector_lease_deadline(now_ms)?;
        let connection = self.open()?;
        let changed = connection
            .execute(
                "UPDATE supervised_collector_lease
                    SET state = ?1, heartbeat_at_ms = ?2, lease_until_ms = ?3
                 WHERE singleton_id = 1
                    AND instance_id = ?4",
                params![
                    state.as_str(),
                    now_ms,
                    lease_until_ms,
                    lease.instance_id.to_string()
                ],
            )
            .map_err(StoreError::SupervisedCollectorLeaseWrite)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::SupervisedCollectorLeaseLost)
        }
    }

    /// Remove only this process's lease on an orderly exit. A stale process
    /// cannot clear the replacement that acquired the singleton after expiry.
    pub(crate) fn release_supervised_collector_lease(
        &self,
        lease: SupervisedCollectorLease,
    ) -> Result<(), StoreError> {
        let connection = self.open()?;
        connection
            .execute(
                "DELETE FROM supervised_collector_lease
                  WHERE singleton_id = 1 AND instance_id = ?1",
                params![lease.instance_id.to_string()],
            )
            .map_err(StoreError::SupervisedCollectorLeaseWrite)?;
        Ok(())
    }

    pub fn quick_check(&self) -> Result<(), StoreError> {
        let connection = self.open_read_only_connection()?;
        let result: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        if result == "ok" {
            Ok(())
        } else {
            Err(StoreError::Integrity(result))
        }
    }

    /// Fast service readiness for `/readyz`.
    ///
    /// Opens the catalogue, probes that core tables respond, and refuses when
    /// lifecycle state is quarantined. Deliberately does **not** run
    /// `PRAGMA quick_check` — that full-table scan blocks the TLS accept path
    /// for multi-GB post-import databases (10M+ positions) for many minutes and
    /// makes the Hub appear dead to readiness probes. Operators use
    /// [`Self::catalogue_check`] / [`Self::quick_check`] for integrity gates.
    pub fn readiness_check(&self) -> Result<(), StoreError> {
        let connection = self.open_read_only_connection()?;
        // Cheap openability probe (fails closed on corrupt headers / missing schema).
        let _: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let _: i64 = connection
            .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let quarantined: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM vehicle_lifecycle_state WHERE quarantined != 0",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let quarantined =
            usize::try_from(quarantined).map_err(|_| StoreError::InvalidStoredCount)?;
        if quarantined == 0 {
            Ok(())
        } else {
            Err(StoreError::QuarantinedLifecycle(quarantined))
        }
    }

    /// Fast, redacted service readiness used by `/readyz`. This deliberately
    /// stops at catalogue/manifest validation and file metadata. Same-size
    /// content corruption remains a `doctor` / [`Self::catalogue_check`] gate;
    /// hashing a multi-gigabyte published corpus on every HTTP probe would
    /// itself make the service unavailable.
    pub fn service_readiness_at(
        &self,
        supervised_collector_required: bool,
        now_ms: i64,
    ) -> Result<(), ReadinessFailure> {
        match self.readiness_check() {
            Ok(()) => {}
            Err(StoreError::QuarantinedLifecycle(_)) => {
                return Err(ReadinessFailure {
                    code: ReadinessReasonCode::LifecycleQuarantined,
                });
            }
            Err(_) => {
                return Err(ReadinessFailure {
                    code: ReadinessReasonCode::CatalogueUnavailable,
                });
            }
        }
        self.verify_active_published_content_metadata()
            .map_err(|_| ReadinessFailure {
                code: ReadinessReasonCode::PublishedContentUnservable,
            })?;
        if supervised_collector_required {
            self.verify_supervised_collector_readiness_at(now_ms)?;
        }
        Ok(())
    }

    fn verify_supervised_collector_readiness_at(
        &self,
        now_ms: i64,
    ) -> Result<(), ReadinessFailure> {
        if now_ms < 0 {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CatalogueUnavailable,
            });
        }
        let connection = self
            .open_read_only_connection()
            .map_err(|_| ReadinessFailure {
                code: ReadinessReasonCode::CatalogueUnavailable,
            })?;
        let row: Option<(String, String, i64, i64, i64)> = connection
            .query_row(
                "SELECT instance_id, state, started_at_ms,
                        heartbeat_at_ms, lease_until_ms
                   FROM supervised_collector_lease WHERE singleton_id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| ReadinessFailure {
                code: ReadinessReasonCode::CatalogueUnavailable,
            })?;
        let Some((instance_id, state, started_at_ms, heartbeat_at_ms, lease_until_ms)) = row else {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CollectorAbsent,
            });
        };
        if Uuid::parse_str(&instance_id)
            .ok()
            .is_none_or(|value| value.is_nil())
            || started_at_ms < 0
            || heartbeat_at_ms < started_at_ms
            || lease_until_ms <= heartbeat_at_ms
            || !matches!(state.as_str(), "active" | "auth_terminal")
        {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CatalogueUnavailable,
            });
        }
        if lease_until_ms <= now_ms {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CollectorStale,
            });
        }
        if state == SupervisedCollectorState::AuthenticationTerminal.as_str() {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CollectorAuthTerminal,
            });
        }
        Ok(())
    }

    fn verify_active_published_content_metadata(&self) -> Result<(), StoreError> {
        type PackCatalogueEntry = (String, i64, String, i64, i64);

        let connection = self.open_read_only_connection()?;
        let pack_rows = connection
            .prepare(
                "SELECT sha256, snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                   FROM sync_packs ORDER BY sha256",
            )
            .map_err(StoreError::Query)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        let mut catalogue = HashMap::<String, PackCatalogueEntry>::with_capacity(pack_rows.len());
        for (sha256, snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes) in
            pack_rows
        {
            let digest = sha256
                .parse::<Sha256Digest>()
                .map_err(|_| StoreError::LineageCatalogConflict)?;
            if digest.to_string() != sha256
                || relative_path != TransportPack::canonical_relative_path(digest)
                || ordinal < 0
                || compressed_bytes <= 0
                || uncompressed_bytes <= 0
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            let expected_bytes =
                u64::try_from(compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
            let path = self
                .packs_dir
                .join("sha256")
                .join(format!("{sha256}.sqlite.zst"));
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| StoreError::InspectCatalogPack {
                    path: path.clone(),
                    source,
                })?;
            if !metadata.file_type().is_file() {
                return Err(StoreError::CatalogPackNotRegular { path });
            }
            if metadata.len() != expected_bytes {
                return Err(StoreError::CatalogPackSizeMismatch {
                    path,
                    expected: expected_bytes,
                    actual: metadata.len(),
                });
            }
            let entry = (
                snapshot_id,
                ordinal,
                relative_path,
                compressed_bytes,
                uncompressed_bytes,
            );
            if catalogue.insert(sha256, entry).is_some() {
                return Err(StoreError::LineageCatalogConflict);
            }
        }

        let manifest_rows = connection
            .prepare(
                "SELECT snapshot_id, vehicle_id, head_sequence, manifest_json
                   FROM sync_manifests ORDER BY vehicle_id, snapshot_id",
            )
            .map_err(StoreError::Query)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        for (snapshot_id, vehicle_id, head_sequence, payload) in manifest_rows {
            let manifest = decode_manifest(payload)?;
            if manifest.snapshot_id.to_string() != snapshot_id
                || manifest.vehicle_id.to_string() != vehicle_id
                || i64::try_from(manifest.head_sequence).ok() != Some(head_sequence)
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            for pack in &manifest.chunks {
                verify_transport_pack_catalogue_binding(&catalogue, pack)?;
            }
        }

        let lineage_vehicle_ids = connection
            .prepare("SELECT vehicle_id FROM sync_bases ORDER BY vehicle_id")
            .map_err(StoreError::Query)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        drop(connection);
        for vehicle_id in lineage_vehicle_ids {
            let vehicle_id = Uuid::parse_str(&vehicle_id)
                .map_err(|_| StoreError::InvalidStoredUuid("lineage vehicle"))?;
            let lineage = self
                .lineage_manifest_for_vehicle_with_verification(
                    vehicle_id,
                    LineagePackVerification::MetadataOnly,
                )?
                .ok_or(StoreError::LineageCatalogConflict)?;
            for pack in lineage
                .base
                .packs
                .iter()
                .chain(lineage.deltas.iter().map(|delta| &delta.pack))
            {
                verify_transport_pack_catalogue_binding(&catalogue, pack)?;
            }
        }
        Ok(())
    }

    /// Perform the operator-facing integrity gate. Unlike the fast readiness
    /// path, this runs full `PRAGMA quick_check` and hashes every currently
    /// referenced immutable pack.
    pub fn catalogue_check(&self) -> Result<(), StoreError> {
        self.quick_check()?;
        self.readiness_check()?;
        self.verify_referenced_packs()
    }

    fn verify_referenced_packs(&self) -> Result<(), StoreError> {
        self.verify_referenced_packs_at(retired_lineage_clock_ms()?)
    }

    fn verify_referenced_packs_at(&self, now_ms: i64) -> Result<(), StoreError> {
        if now_ms < 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let connection = self.open_read_only_connection()?;
        let rows = referenced_pack_rows_at(&connection, now_ms)?;

        for (sha256, relative_path, compressed_bytes) in rows {
            let compressed_bytes =
                u64::try_from(compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
            if !is_sha256_hex(&sha256)
                || relative_path != format!("/v1/packs/sha256/{sha256}.sqlite.zst")
            {
                return Err(StoreError::UnsafeStoredPackPath);
            }
            let path = self
                .packs_dir
                .join("sha256")
                .join(format!("{sha256}.sqlite.zst"));
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| StoreError::InspectCatalogPack {
                    path: path.clone(),
                    source,
                })?;
            if !metadata.file_type().is_file() {
                return Err(StoreError::CatalogPackNotRegular { path });
            }
            if metadata.len() != compressed_bytes {
                return Err(StoreError::CatalogPackSizeMismatch {
                    path,
                    expected: compressed_bytes,
                    actual: metadata.len(),
                });
            }
            if sha256_file_hex(&path)? != sha256 {
                return Err(StoreError::CatalogPackDigestMismatch { path });
            }
        }
        Ok(())
    }

    pub fn sqlite_version(&self) -> Result<String, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row("SELECT sqlite_version()", [], |row| row.get(0))
            .map_err(StoreError::Query)
    }

    /// Stable random identity of this Hub installation. It never comes from a
    /// remote source and survives package upgrades and restarts.
    pub fn installation_id(&self) -> Result<Uuid, StoreError> {
        let connection = self.open()?;
        ensure_installation_id(&connection)
    }

    pub fn publish_manifest(&self, manifest: &SyncManifest) -> Result<(), StoreError> {
        if manifest.schema == HUB_PROJECTION_SCHEMA_V3 {
            return Err(StoreError::Schema22PairPublicationRequired(
                manifest.vehicle_id,
            ));
        }
        self.publish_manifest_catalogue(manifest)
    }

    fn publish_manifest_catalogue(&self, manifest: &SyncManifest) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        // A schema-2.1 full base is not self-describing enough to recover the
        // selected source car later.  The generic manifest entry point has no
        // immutable ProjectionBinding, so it must never create (or extend) a
        // V2 lineage.  Binding-aware import finalizers pass the exact binding
        // into the transactional helper instead.
        publish_manifest_in_transaction(&transaction, manifest, None)?;
        transaction.commit().map_err(StoreError::PublishManifest)
    }

    pub(crate) fn publish_schema_22_manifest(
        &self,
        _publication_gate: &PublicationGate,
        manifest: &SyncManifest,
    ) -> Result<(), StoreError> {
        if manifest.schema != HUB_PROJECTION_SCHEMA_V3 {
            return Err(StoreError::Schema22PairPublicationRequired(
                manifest.vehicle_id,
            ));
        }
        let noop_bytes = self
            .schema_22_noop_for_snapshot(manifest.vehicle_id, manifest.snapshot_id)?
            .ok_or(StoreError::Schema22NoOpNotFound)?;
        let noop: crate::updates_delivery::SignedNoOpState = serde_json::from_slice(&noop_bytes)
            .map_err(|error| StoreError::InvalidSchema22Pair(error.to_string()))?;
        let canonical = serde_json::to_vec(&noop)
            .map_err(|error| StoreError::InvalidSchema22Pair(error.to_string()))?;
        if canonical != noop_bytes {
            return Err(StoreError::InvalidSchema22Pair(
                "no-op is not canonical typed JSON".into(),
            ));
        }
        crate::updates_delivery::validate_schema_22_pair(manifest, &noop)
            .map_err(|error| StoreError::InvalidSchema22Pair(error.message))?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let candidate_manifest =
            serde_json::to_vec(manifest).map_err(StoreError::SerializeManifest)?;
        let existing = transaction
            .query_row(
                "SELECT manifest_json FROM sync_manifests WHERE snapshot_id = ?1",
                params![manifest.snapshot_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if let Some(stored_manifest) = existing {
            if stored_manifest != candidate_manifest {
                return Err(StoreError::Schema22SnapshotConflict {
                    vehicle_id: manifest.vehicle_id,
                    snapshot_id: manifest.snapshot_id,
                });
            }
            decode_manifest(stored_manifest)?;
        }
        publish_manifest_in_transaction(&transaction, manifest, None)?;
        transaction.commit().map_err(StoreError::PublishManifest)
    }

    fn ensure_schema_22_noop_directory(&self) -> Result<(), StoreError> {
        self.open_schema_22_noop_directory(true).map(|_| ())
    }

    fn open_schema_22_noop_directory(
        &self,
        create: bool,
    ) -> Result<SharedSchema22NoOpDirectory, StoreError> {
        let data_root = self
            .database_path
            .parent()
            .ok_or_else(|| StoreError::UnsafeSchema22NoOpPath(self.database_path.clone()))?;
        let data_root_fd = open(
            data_root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        let data_root_stat =
            fstat(&data_root_fd).map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        if !FileType::from_raw_mode(data_root_stat.st_mode).is_dir() {
            return Err(StoreError::UnsafeSchema22NoOpPath(data_root.to_path_buf()));
        }
        let packs_fd = openat(
            &data_root_fd,
            "packs",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        let mut packs_stat =
            fstat(&packs_fd).map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        let packs_path = data_root.join("packs");
        // Projection writers and historical backup roots can create this
        // non-secret directory with the platform's ordinary 0755 default
        // before HubStore opens it. Only its owner may tighten that one known
        // default to the shared setgid contract; peer-owned or novel modes
        // remain package-admission failures.
        if FileType::from_raw_mode(packs_stat.st_mode).is_dir()
            && packs_stat.st_uid == rustix::process::geteuid().as_raw()
            && packs_stat.st_gid == data_root_stat.st_gid
            && stat_mode(packs_stat.st_mode) == 0o755
        {
            fchmod(
                &packs_fd,
                Mode::from_raw_mode(SHARED_DATA_DIRECTORY_MODE as u16),
            )
            .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            File::from(
                packs_fd
                    .try_clone()
                    .map_err(StoreError::AccessSchema22NoOp)?,
            )
            .sync_all()
            .map_err(StoreError::AccessSchema22NoOp)?;
            packs_stat =
                fstat(&packs_fd).map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        }
        if !FileType::from_raw_mode(packs_stat.st_mode).is_dir()
            || packs_stat.st_gid != data_root_stat.st_gid
            || stat_mode(packs_stat.st_mode) != SHARED_DATA_DIRECTORY_MODE
        {
            return Err(StoreError::UnsafeSchema22NoOpPath(packs_path));
        }

        let created = if create {
            match mkdirat(
                &packs_fd,
                "noop",
                Mode::from_raw_mode(SHARED_SCHEMA_22_NOOP_DIRECTORY_MODE as u16),
            ) {
                Ok(()) => true,
                Err(Errno::EXIST) => false,
                Err(error) => return Err(StoreError::AccessSchema22NoOp(error.into())),
            }
        } else {
            false
        };
        let noop_path = data_root.join("packs/noop");
        let noop_fd = match openat(
            &packs_fd,
            "noop",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) if !create => {
                return Err(StoreError::Schema22NoOpNotFound);
            }
            Err(error) => return Err(StoreError::AccessSchema22NoOp(error.into())),
        };
        if created {
            fchmod(
                &noop_fd,
                Mode::from_raw_mode(SHARED_SCHEMA_22_NOOP_DIRECTORY_MODE as u16),
            )
            .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            File::from(
                noop_fd
                    .try_clone()
                    .map_err(StoreError::AccessSchema22NoOp)?,
            )
            .sync_all()
            .map_err(StoreError::AccessSchema22NoOp)?;
            File::from(
                packs_fd
                    .try_clone()
                    .map_err(StoreError::AccessSchema22NoOp)?,
            )
            .sync_all()
            .map_err(StoreError::AccessSchema22NoOp)?;
        }
        let noop_stat =
            fstat(&noop_fd).map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        if !FileType::from_raw_mode(noop_stat.st_mode).is_dir()
            || noop_stat.st_gid != packs_stat.st_gid
            || stat_mode(noop_stat.st_mode) != SHARED_SCHEMA_22_NOOP_DIRECTORY_MODE
        {
            return Err(StoreError::UnsafeSchema22NoOpPath(noop_path.clone()));
        }
        Ok(SharedSchema22NoOpDirectory {
            file: File::from(noop_fd),
            gid: noop_stat.st_gid,
            path: noop_path,
        })
    }

    pub(crate) fn prepare_schema_22_noop_publication(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        keep_snapshot_id: Option<Uuid>,
    ) -> Result<(), StoreError> {
        let directory = self.open_schema_22_noop_directory(true)?;
        let prefix = format!("{vehicle_id}.");
        let temporary_prefix = format!(".{vehicle_id}.");
        let mut removed = false;
        let entries = Dir::read_from(&directory.file)
            .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        for entry in entries {
            let entry = entry.map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            let Ok(name) = entry.file_name().to_str() else {
                return Err(StoreError::UnsafeSchema22NoOpPath(directory.path.clone()));
            };
            if matches!(name, "." | "..")
                || (!name.starts_with(&prefix) && !name.starts_with(&temporary_prefix))
            {
                continue;
            }
            let path = directory.path.join(name);
            let stat = statat(&directory.file, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            let final_snapshot = parse_schema_22_noop_filename(name, vehicle_id);
            let valid_final = final_snapshot.is_some()
                && validate_schema_22_noop_file_stat(
                    &stat,
                    directory.gid,
                    SHARED_SCHEMA_22_NOOP_FILE_MODE,
                );
            let valid_temporary = is_schema_22_noop_temporary_filename(name, vehicle_id)
                && (validate_schema_22_noop_file_stat(
                    &stat,
                    directory.gid,
                    PRIVATE_SCHEMA_22_NOOP_STAGING_MODE,
                ) || validate_schema_22_noop_file_stat(
                    &stat,
                    directory.gid,
                    SHARED_SCHEMA_22_NOOP_FILE_MODE,
                ));
            if !valid_final && !valid_temporary {
                return Err(StoreError::UnsafeSchema22NoOpPath(path));
            }
            if final_snapshot == keep_snapshot_id {
                continue;
            }
            unlinkat(&directory.file, name, AtFlags::empty())
                .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            removed = true;
        }
        if removed {
            directory
                .file
                .sync_all()
                .map_err(StoreError::AccessSchema22NoOp)?;
        }
        Ok(())
    }

    /// Persist a schema-2.2 no-op under its immutable snapshot identity. The
    /// publication gate keeps same-vehicle cleanup and installation ordered.
    pub(crate) fn publish_schema_22_noop(
        &self,
        _publication_gate: &PublicationGate,
        noop: &crate::updates_delivery::SignedNoOpState,
    ) -> Result<(), StoreError> {
        let directory = self.open_schema_22_noop_directory(true)?;
        let final_name = schema_22_noop_filename(noop.vehicle_id, noop.snapshot_id);
        let path = directory.path.join(&final_name);
        let payload = serde_json::to_vec(noop).map_err(StoreError::SerializeManifest)?;
        if u64::try_from(payload.len()).unwrap_or(u64::MAX) > MAX_SCHEMA_22_NOOP_BYTES {
            return Err(StoreError::UnsafeSchema22NoOpPath(path));
        }
        if let Some(existing) =
            self.schema_22_noop_bytes_in_directory(&directory, final_name.as_str())?
        {
            return if existing == payload {
                Ok(())
            } else {
                Err(StoreError::Schema22SnapshotConflict {
                    vehicle_id: noop.vehicle_id,
                    snapshot_id: noop.snapshot_id,
                })
            };
        }
        let temporary_name = format!(
            ".{}.{}.{}.tmp",
            noop.vehicle_id,
            noop.snapshot_id,
            Uuid::new_v4()
        );
        let temporary_path = directory.path.join(&temporary_name);
        let result = (|| {
            let fd = openat(
                &directory.file,
                temporary_name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(PRIVATE_SCHEMA_22_NOOP_STAGING_MODE as u16),
            )
            .map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            let mut file = File::from(fd);
            fchmod(
                &file,
                Mode::from_raw_mode(PRIVATE_SCHEMA_22_NOOP_STAGING_MODE as u16),
            )
            .map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            let private_stat =
                fstat(&file).map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            if private_stat.st_uid != rustix::process::geteuid().as_raw()
                || !validate_schema_22_noop_file_stat(
                    &private_stat,
                    directory.gid,
                    PRIVATE_SCHEMA_22_NOOP_STAGING_MODE,
                )
            {
                return Err(StoreError::UnsafeSchema22NoOpPath(temporary_path.clone()));
            }
            file.write_all(&payload)
                .and_then(|()| file.sync_all())
                .map_err(StoreError::WriteSchema22NoOp)?;
            fchmod(
                &file,
                Mode::from_raw_mode(SHARED_SCHEMA_22_NOOP_FILE_MODE as u16),
            )
            .map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            let shared_stat =
                fstat(&file).map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            if !validate_schema_22_noop_file_stat(
                &shared_stat,
                directory.gid,
                SHARED_SCHEMA_22_NOOP_FILE_MODE,
            ) {
                return Err(StoreError::UnsafeSchema22NoOpPath(temporary_path.clone()));
            }
            drop(file);
            match renameat_with(
                &directory.file,
                temporary_name.as_str(),
                &directory.file,
                final_name.as_str(),
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => directory
                    .file
                    .sync_all()
                    .map_err(StoreError::WriteSchema22NoOp),
                Err(Errno::EXIST) => {
                    unlinkat(&directory.file, temporary_name.as_str(), AtFlags::empty())
                        .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
                    let existing = self
                        .schema_22_noop_bytes_in_directory(&directory, final_name.as_str())?
                        .ok_or(StoreError::Schema22NoOpNotFound)?;
                    if existing == payload {
                        Ok(())
                    } else {
                        Err(StoreError::Schema22SnapshotConflict {
                            vehicle_id: noop.vehicle_id,
                            snapshot_id: noop.snapshot_id,
                        })
                    }
                }
                Err(error) => Err(StoreError::WriteSchema22NoOp(error.into())),
            }
        })();
        if result.is_err() {
            let _ = unlinkat(&directory.file, temporary_name.as_str(), AtFlags::empty());
        }
        result
    }

    pub fn schema_22_noop_for_snapshot(
        &self,
        vehicle_id: Uuid,
        snapshot_id: Uuid,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let directory = match self.open_schema_22_noop_directory(false) {
            Ok(directory) => directory,
            Err(StoreError::Schema22NoOpNotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let name = schema_22_noop_filename(vehicle_id, snapshot_id);
        self.schema_22_noop_bytes_in_directory(&directory, name.as_str())
    }

    fn schema_22_noop_bytes_in_directory(
        &self,
        directory: &SharedSchema22NoOpDirectory,
        name: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let path = directory.path.join(name);
        let fd = match openat(
            &directory.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(StoreError::ReadSchema22NoOp(error.into())),
        };
        let mut file = File::from(fd);
        let stat = fstat(&file).map_err(|error| StoreError::ReadSchema22NoOp(error.into()))?;
        if !validate_schema_22_noop_file_stat(&stat, directory.gid, SHARED_SCHEMA_22_NOOP_FILE_MODE)
        {
            return Err(StoreError::UnsafeSchema22NoOpPath(path));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_SCHEMA_22_NOOP_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(StoreError::ReadSchema22NoOp)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SCHEMA_22_NOOP_BYTES {
            return Err(StoreError::UnsafeSchema22NoOpPath(path));
        }
        Ok(Some(bytes))
    }

    pub fn claim_export_outbox(
        &self,
        now_ms: i64,
    ) -> Result<Option<ExportOutboxClaim>, StoreError> {
        validate_timestamp("export outbox now_ms", now_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let row: Option<(String, i64, i64)> = transaction
            .query_row(
                "SELECT vehicle_id, dirty_revision, attempts
                 FROM export_outbox
                 WHERE next_attempt_ms <= ?1 AND claimed_until_ms <= ?1
                 ORDER BY next_attempt_ms, vehicle_id LIMIT 1",
                params![now_ms],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((vehicle, revision, attempts)) = row else {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(None);
        };
        let vehicle_id = Uuid::parse_str(&vehicle)
            .map_err(|_| StoreError::InvalidStoredUuid("export vehicle"))?;
        let lease_until_ms = now_ms.saturating_add(60_000);
        transaction
            .execute(
                "UPDATE export_outbox
                 SET attempts = attempts + 1, claimed_until_ms = ?1
                 WHERE vehicle_id = ?2 AND dirty_revision = ?3",
                params![lease_until_ms, vehicle, revision],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(Some(ExportOutboxClaim {
            vehicle_id,
            dirty_revision: revision,
            attempts: attempts + 1,
            claimed_at_ms: now_ms,
            lease_until_ms,
        }))
    }

    pub fn complete_export_outbox(&self, claim: &ExportOutboxClaim) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let deleted = transaction
            .execute(
                "DELETE FROM export_outbox
                 WHERE vehicle_id = ?1 AND dirty_revision = ?2
                   AND claimed_until_ms = ?3",
                params![
                    claim.vehicle_id.to_string(),
                    claim.dirty_revision,
                    claim.lease_until_ms
                ],
            )
            .map_err(StoreError::Query)?;
        if deleted == 0 {
            // A mutation can advance the revision while this claim is being
            // published. Keep that newer work, but place it behind already
            // eligible vehicles so one busy vehicle cannot monopolise replay.
            transaction
                .execute(
                    "UPDATE export_outbox
                     SET claimed_until_ms = 0, attempts = 0,
                         next_attempt_ms = MAX(next_attempt_ms, ?1), last_error = NULL
                     WHERE vehicle_id = ?2 AND dirty_revision > ?3
                       AND claimed_until_ms = ?4",
                    params![
                        claim.claimed_at_ms.saturating_add(1),
                        claim.vehicle_id.to_string(),
                        claim.dirty_revision,
                        claim.lease_until_ms
                    ],
                )
                .map_err(StoreError::Query)?;
        }
        transaction.commit().map_err(StoreError::Query)
    }

    pub fn fail_export_outbox(
        &self,
        claim: &ExportOutboxClaim,
        error: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("export outbox failure now_ms", now_ms)?;
        let delay = 1_i64
            .checked_shl(claim.attempts.min(16) as u32)
            .unwrap_or(60 * 60)
            .min(60 * 60)
            .saturating_mul(1_000);
        let safe_error = error
            .chars()
            .filter(|character| !character.is_control())
            .take(256)
            .collect::<String>();
        let safe_error = if safe_error.contains("://") {
            "publication_failed".to_owned()
        } else {
            safe_error
        };
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let updated = transaction
            .execute(
                "UPDATE export_outbox
                 SET claimed_until_ms = 0, next_attempt_ms = ?1, last_error = ?2
                 WHERE vehicle_id = ?3 AND dirty_revision = ?4
                   AND claimed_until_ms = ?5",
                params![
                    now_ms.saturating_add(delay),
                    safe_error,
                    claim.vehicle_id.to_string(),
                    claim.dirty_revision,
                    claim.lease_until_ms
                ],
            )
            .map_err(StoreError::Query)?;
        if updated == 0 {
            transaction
                .execute(
                    "UPDATE export_outbox
                     SET claimed_until_ms = 0, attempts = 0,
                         next_attempt_ms = MAX(next_attempt_ms, ?1), last_error = NULL
                     WHERE vehicle_id = ?2 AND dirty_revision > ?3
                       AND claimed_until_ms = ?4",
                    params![
                        claim.claimed_at_ms.saturating_add(1),
                        claim.vehicle_id.to_string(),
                        claim.dirty_revision,
                        claim.lease_until_ms
                    ],
                )
                .map_err(StoreError::Query)?;
        }
        transaction.commit().map_err(StoreError::Query)
    }

    pub fn release_export_outbox(&self, claim: &ExportOutboxClaim) -> Result<(), StoreError> {
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE export_outbox
                 SET claimed_until_ms = 0,
                     next_attempt_ms = CASE WHEN dirty_revision > ?1
                         THEN MAX(next_attempt_ms, ?2) ELSE next_attempt_ms END
                 WHERE vehicle_id = ?3 AND dirty_revision >= ?1
                   AND claimed_until_ms = ?4",
                params![
                    claim.dirty_revision,
                    claim.claimed_at_ms.saturating_add(1),
                    claim.vehicle_id.to_string(),
                    claim.lease_until_ms
                ],
            )
            .map_err(StoreError::Query)?;
        Ok(())
    }

    pub fn vehicle_has_v2_base(&self, vehicle_id: Uuid) -> Result<bool, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_bases WHERE vehicle_id = ?1)",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    pub fn has_unpublished_sync_mutations(&self, vehicle_id: Uuid) -> Result<bool, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sync_mutations
                    WHERE vehicle_id = ?1 AND published = 0
                )",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    pub fn claim_sync_mutations(
        &self,
        vehicle_id: Uuid,
        now_ms: i64,
        maximum: usize,
    ) -> Result<Option<SyncMutationClaim>, StoreError> {
        if vehicle_id.is_nil() || maximum == 0 {
            return Ok(None);
        }
        validate_timestamp("sync mutation claim now_ms", now_ms)?;
        let maximum = maximum.min(10_000);
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_key = vehicle_id.to_string();
        let first: Option<(i64, i64)> = transaction
            .query_row(
                "SELECT revision, claimed_until_ms
                 FROM sync_mutations
                 WHERE vehicle_id = ?1 AND published = 0
                 ORDER BY revision LIMIT 1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((first_revision, claimed_until_ms)) = first else {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(None);
        };
        if claimed_until_ms > now_ms {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(None);
        }
        let mut statement = transaction
            .prepare(
                "SELECT revision, entity, entity_id, car_id, operation, payload_json,
                        claimed_until_ms
                 FROM sync_mutations
                 WHERE vehicle_id = ?1 AND published = 0 AND revision >= ?2
                 ORDER BY revision LIMIT ?3",
            )
            .map_err(StoreError::Query)?;
        let mut rows = statement
            .query(params![
                vehicle_key.as_str(),
                first_revision,
                maximum as i64
            ])
            .map_err(StoreError::Query)?;
        let mut mutations = Vec::with_capacity(maximum);
        while let Some(row) = rows.next().map_err(StoreError::Query)? {
            let claimed_until: i64 = row.get(6).map_err(StoreError::Query)?;
            if claimed_until > now_ms {
                break;
            }
            mutations.push(SyncMutation {
                vehicle_id,
                revision: row.get(0).map_err(StoreError::Query)?,
                entity: row.get(1).map_err(StoreError::Query)?,
                entity_id: row.get(2).map_err(StoreError::Query)?,
                car_id: row.get(3).map_err(StoreError::Query)?,
                operation: row.get(4).map_err(StoreError::Query)?,
                payload_json: row.get(5).map_err(StoreError::Query)?,
            });
        }
        drop(rows);
        drop(statement);
        if mutations.is_empty() {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(None);
        }
        let first_revision = mutations[0].revision;
        let last_revision = mutations.last().expect("non-empty mutations").revision;
        let lease_until_ms = now_ms.saturating_add(60_000);
        transaction
            .execute(
                "UPDATE sync_mutations
                 SET claimed_until_ms = ?1
                 WHERE vehicle_id = ?2 AND published = 0
                   AND revision BETWEEN ?3 AND ?4",
                params![lease_until_ms, vehicle_key, first_revision, last_revision],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(Some(SyncMutationClaim {
            vehicle_id,
            from_revision: first_revision,
            to_revision: last_revision,
            mutations,
        }))
    }

    pub fn release_sync_mutations(&self, claim: &SyncMutationClaim) -> Result<(), StoreError> {
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE sync_mutations SET claimed_until_ms = 0
                 WHERE vehicle_id = ?1 AND published = 0
                   AND revision BETWEEN ?2 AND ?3",
                params![
                    claim.vehicle_id.to_string(),
                    claim.from_revision,
                    claim.to_revision
                ],
            )
            .map_err(StoreError::Query)?;
        Ok(())
    }

    pub fn v2_head(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<(Uuid, i64, Sha256Digest)>, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT base_snapshot_id, head_sequence, head_digest
                 FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    let snapshot_id: String = row.get(0)?;
                    let digest: String = row.get(2)?;
                    Ok((
                        Uuid::parse_str(&snapshot_id).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        row.get(1)?,
                        digest.parse::<Sha256Digest>().map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    pub(crate) fn next_v2_pack_ordinal(&self, snapshot_id: Uuid) -> Result<u32, StoreError> {
        let connection = self.open()?;
        let maximum: Option<i64> = connection
            .query_row(
                "SELECT MAX(ordinal) FROM sync_packs WHERE snapshot_id = ?1",
                params![snapshot_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let next = maximum
            .unwrap_or(-1)
            .checked_add(1)
            .ok_or(StoreError::PackOrdinalTooLarge)?;
        u32::try_from(next).map_err(|_| StoreError::PackOrdinalTooLarge)
    }

    pub fn v2_projection_binding(&self, vehicle_id: Uuid) -> Result<ProjectionBinding, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let immutable: Option<(String, String, String, i64, i64)> = connection
            .query_row(
                "SELECT binding.snapshot_id, binding.installation_id,
                        binding.account_id, binding.generation, binding.selected_car_id
                   FROM sync_bases AS base
                   JOIN v2_base_bindings AS binding
                     ON binding.vehicle_id = base.vehicle_id
                    AND binding.snapshot_id = base.snapshot_id
                  WHERE base.vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some((snapshot_id, installation_id, account_id, generation, selected_car_id)) =
            immutable
        {
            let base_snapshot: Uuid = snapshot_id
                .parse()
                .map_err(|_| StoreError::LineageCatalogConflict)?;
            if base_snapshot.is_nil() || selected_car_id <= 0 {
                return Err(StoreError::LineageCatalogConflict);
            }
            return Ok(ProjectionBinding {
                installation_id: installation_id
                    .parse()
                    .map_err(|_| StoreError::LineageCatalogConflict)?,
                account_id: account_id
                    .parse()
                    .map_err(|_| StoreError::LineageCatalogConflict)?,
                vehicle_id,
                generation: u64::try_from(generation)
                    .ok()
                    .filter(|generation| *generation > 0)
                    .ok_or(StoreError::LineageCatalogConflict)?,
                selected_car_id,
            });
        }
        let has_v2_base: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_bases WHERE vehicle_id = ?1)",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if has_v2_base {
            // A base created before immutable binding persistence cannot be
            // reconstructed from mutable source ownership, aliases, or
            // settings.  Refuse a successor rather than retargeting it.
            return Err(StoreError::ImmutableBaseBindingMissing(vehicle_id));
        }

        let installation_id = self.installation_id()?;
        let (source_id, generation, source_key, selected_car_id, materialised_car_id): (
            String,
            i64,
            String,
            Option<i64>,
            Option<i64>,
        ) = connection
            .query_row(
                "SELECT v.source_id, s.generation, v.source_vehicle_key,
                            (SELECT car_id FROM car_settings WHERE vehicle_id = v.vehicle_id),
                            (SELECT car_id FROM materialised_cars WHERE vehicle_id = v.vehicle_id)
                     FROM vehicles v JOIN sources s ON s.source_id = v.source_id
                     WHERE v.vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(StoreError::Query)?;
        let account_id = Uuid::parse_str(&source_id).map_err(|_| StoreError::InvalidSourceId)?;
        let generation =
            u64::try_from(generation).map_err(|_| StoreError::InvalidStoredSequence)?;
        let selected_car_id = selected_car_id
            .or(materialised_car_id)
            .or_else(|| {
                source_key
                    .strip_prefix("eid:")
                    .and_then(|value| value.parse().ok())
            })
            .or_else(|| {
                source_key
                    .strip_prefix("vid:")
                    .and_then(|value| value.parse().ok())
            })
            .or_else(|| source_key.parse().ok())
            .ok_or(StoreError::InvalidLifecycleCarId)?;
        Ok(ProjectionBinding {
            installation_id,
            account_id,
            vehicle_id,
            generation,
            selected_car_id,
        })
    }

    /// The single TeslaMate-imported car eligible for Owner API collection.
    /// A V2 binding and Tesla EID are durable import facts; do not infer this
    /// from mutable discovery data.
    pub fn selected_imported_tesla_eid(
        &self,
    ) -> Result<Option<(i64, ProjectionCarSettings)>, StoreError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT eid.alias_value,
                        settings.enabled, settings.use_streaming_api,
                        settings.suspend_after_idle_min, settings.suspend_min,
                        settings.req_not_unlocked, settings.free_supercharging,
                        settings.lfp_battery, settings.suspend_min_resolved,
                        car.car_json
                   FROM v2_base_bindings AS binding
                   JOIN vehicle_identity_aliases AS eid
                     ON eid.vehicle_id = binding.vehicle_id
                    AND eid.alias_kind = 'tesla_eid'
                   LEFT JOIN car_settings AS settings
                     ON settings.vehicle_id = binding.vehicle_id
                   LEFT JOIN materialised_cars AS car
                     ON car.vehicle_id = binding.vehicle_id",
            )
            .map_err(StoreError::Query)?;
        let mut rows = statement.query([]).map_err(StoreError::Query)?;
        let Some(row) = rows.next().map_err(StoreError::Query)? else {
            return Ok(None);
        };
        let values = (
            row.get::<_, String>(0).map_err(StoreError::Query)?,
            row.get::<_, Option<i64>>(1).map_err(StoreError::Query)?,
            row.get::<_, Option<i64>>(2).map_err(StoreError::Query)?,
            row.get::<_, Option<i64>>(3).map_err(StoreError::Query)?,
            row.get::<_, Option<i64>>(4).map_err(StoreError::Query)?,
            row.get::<_, Option<i64>>(5).map_err(StoreError::Query)?,
            row.get::<_, Option<i64>>(6).map_err(StoreError::Query)?,
            row.get::<_, Option<i64>>(7).map_err(StoreError::Query)?,
            row.get::<_, Option<i64>>(8).map_err(StoreError::Query)?,
            row.get::<_, Option<String>>(9).map_err(StoreError::Query)?,
        );
        if rows.next().map_err(StoreError::Query)?.is_some() {
            return Err(StoreError::LineageCatalogConflict);
        }
        let eid = values
            .0
            .parse::<i64>()
            .ok()
            .filter(|eid| *eid > 0)
            .ok_or(StoreError::LineageCatalogConflict)?;
        let settings = match values.1 {
            None => values
                .9
                .map(|car| {
                    serde_json::from_str::<ProjectionCar>(&car)
                        .map(|car| car.settings)
                        .map_err(StoreError::DeserializeLifecycleRow)
                })
                .transpose()?
                .unwrap_or_default(),
            Some(enabled) => ProjectionCarSettings {
                enabled: enabled != 0,
                use_streaming_api: values.2.unwrap_or_default() != 0,
                suspend_after_idle_min: values.3.unwrap_or_default(),
                suspend_min: values.4.unwrap_or_default(),
                req_not_unlocked: values.5.unwrap_or_default() != 0,
                free_supercharging: values.6.unwrap_or_default() != 0,
                lfp_battery: values.7.unwrap_or_default() != 0,
                suspend_min_resolved: values.8.unwrap_or_default() != 0,
            },
        };
        Ok(Some((eid, settings)))
    }

    pub fn v2_lineage_pack_count(&self, vehicle_id: Uuid) -> Result<usize, StoreError> {
        let lineage = self
            .lineage_manifest_for_vehicle(vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        lineage
            .base
            .packs
            .len()
            .checked_add(lineage.deltas.len())
            .ok_or(StoreError::LineageCapacityExhausted)
    }

    /// Build an exact collector-owned suffix plan from durable provenance.
    ///
    /// Imported successors deliberately create no `sync_live_delta_spans`
    /// row. Therefore this walks backwards from the current head and stops at
    /// the first non-collector delta instead of ever guessing its contents.
    pub fn plan_live_delta_compaction(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<LiveDeltaCompactionPlan>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let vehicle_key = vehicle_id.to_string();
        let (snapshot_id, head_sequence, head_digest): (String, i64, String) = connection
            .query_row(
                "SELECT heads.base_snapshot_id, heads.head_sequence, heads.head_digest
                 FROM sync_heads AS heads WHERE heads.vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let base_snapshot_id =
            Uuid::parse_str(&snapshot_id).map_err(|_| StoreError::LineageCatalogConflict)?;
        let head_sequence =
            u64::try_from(head_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        let head_digest = head_digest
            .parse::<Sha256Digest>()
            .map_err(|_| StoreError::LineageCatalogConflict)?;

        let mut statement = connection
            .prepare(
                "SELECT deltas.from_sequence, deltas.to_sequence,
                        deltas.parent_chain_digest, deltas.chain_digest,
                        deltas.pack_digest, deltas.pack_json,
                        spans.from_revision, spans.to_revision
                 FROM sync_live_delta_spans AS spans
                 JOIN sync_deltas AS deltas
                   ON deltas.vehicle_id = spans.vehicle_id
                  AND deltas.from_sequence = spans.from_sequence
                  AND deltas.to_sequence = spans.to_sequence
                 WHERE spans.vehicle_id = ?1
                 ORDER BY deltas.to_sequence DESC",
            )
            .map_err(StoreError::LineageCatalog)?;
        let rows = statement
            .query_map(params![vehicle_key.as_str()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(StoreError::LineageCatalog)?;
        let mut expected_to = head_sequence;
        let mut expected_digest = head_digest;
        let mut reverse_spans = Vec::new();
        for row in rows {
            let (
                from_sequence,
                to_sequence,
                parent_chain_digest,
                chain_digest,
                pack_digest,
                pack_json,
                from_revision,
                to_revision,
            ) = row.map_err(StoreError::LineageCatalog)?;
            let from_sequence =
                u64::try_from(from_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
            let to_sequence =
                u64::try_from(to_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
            if reverse_spans.is_empty()
                && (to_sequence != expected_to || chain_digest != expected_digest.to_string())
            {
                return Ok(None);
            }
            if to_sequence != expected_to || chain_digest != expected_digest.to_string() {
                break;
            }
            let delta: LineageDelta =
                serde_json::from_slice(&pack_json).map_err(StoreError::DeserializeManifest)?;
            if delta.from_sequence != from_sequence
                || delta.to_sequence != to_sequence
                || delta.parent_chain_digest.to_string() != parent_chain_digest
                || delta.chain_digest.to_string() != chain_digest
                || delta.pack_digest.to_string() != pack_digest
                || delta.pack.snapshot_id != base_snapshot_id
                || to_sequence - from_sequence
                    != u64::try_from(to_revision - from_revision + 1)
                        .map_err(|_| StoreError::LineageCatalogConflict)?
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            self.verify_lineage_pack(&delta.pack)?;
            expected_to = from_sequence;
            expected_digest = delta.parent_chain_digest;
            reverse_spans.push(LiveDeltaCompactionSpan {
                delta,
                from_revision,
                to_revision,
            });
        }
        drop(statement);
        if reverse_spans.len() < 2 {
            return Ok(None);
        }
        reverse_spans.reverse();
        if reverse_spans.windows(2).any(|window| {
            window[0].delta.to_sequence != window[1].delta.from_sequence
                || window[0].delta.chain_digest != window[1].delta.parent_chain_digest
                || window[0].to_revision.checked_add(1) != Some(window[1].from_revision)
                || window[0].delta.pack.ordinal.checked_add(1) != Some(window[1].delta.pack.ordinal)
        }) {
            return Err(StoreError::LineageCatalogConflict);
        }
        let first = reverse_spans
            .first()
            .expect("two spans prove a first compaction span");
        let last = reverse_spans
            .last()
            .expect("two spans prove a final compaction span");
        let from_revision = first.from_revision;
        let to_revision = last.to_revision;
        let expected_revision_count = to_revision
            .checked_sub(from_revision)
            .and_then(|count| count.checked_add(1))
            .ok_or(StoreError::LineageCatalogConflict)?;
        let (actual_count, minimum_revision, maximum_revision, published_count): (
            i64,
            Option<i64>,
            Option<i64>,
            i64,
        ) = connection
            .query_row(
                "SELECT COUNT(*), MIN(revision), MAX(revision),
                        COALESCE(SUM(CASE WHEN published = 1 THEN 1 ELSE 0 END), 0)
                 FROM sync_mutations
                 WHERE vehicle_id = ?1 AND revision BETWEEN ?2 AND ?3",
                params![vehicle_key.as_str(), from_revision, to_revision],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(StoreError::LineageCatalog)?;
        if actual_count != expected_revision_count
            || published_count != expected_revision_count
            || minimum_revision != Some(from_revision)
            || maximum_revision != Some(to_revision)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut mutation_statement = connection
            .prepare(
                "SELECT mutation.revision, mutation.entity, mutation.entity_id,
                        mutation.car_id, mutation.operation, mutation.payload_json
                 FROM sync_mutations AS mutation
                 WHERE mutation.vehicle_id = ?1
                   AND mutation.revision BETWEEN ?2 AND ?3
                   AND NOT EXISTS (
                       SELECT 1 FROM sync_mutations AS newer
                       WHERE newer.vehicle_id = mutation.vehicle_id
                         AND newer.revision BETWEEN ?2 AND ?3
                         AND newer.entity = mutation.entity
                         AND newer.entity_id = mutation.entity_id
                         AND newer.revision > mutation.revision
                   )
                 ORDER BY mutation.revision, mutation.entity, mutation.entity_id",
            )
            .map_err(StoreError::LineageCatalog)?;
        let mutations = mutation_statement
            .query_map(
                params![vehicle_key.as_str(), from_revision, to_revision],
                |row| {
                    Ok(SyncMutation {
                        vehicle_id,
                        revision: row.get(0)?,
                        entity: row.get(1)?,
                        entity_id: row.get(2)?,
                        car_id: row.get(3)?,
                        operation: row.get(4)?,
                        payload_json: row.get(5)?,
                    })
                },
            )
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if mutations.is_empty() {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(Some(LiveDeltaCompactionPlan {
            vehicle_id,
            base_snapshot_id,
            anchor_sequence: first.delta.from_sequence,
            anchor_digest: first.delta.parent_chain_digest,
            head_sequence: last.delta.to_sequence,
            head_digest: last.delta.chain_digest,
            first_ordinal: first.delta.pack.ordinal,
            from_revision,
            to_revision,
            mutations,
            replaced_spans: reverse_spans,
        }))
    }

    /// Rebuild a compacted collector suffix from the journal payloads that
    /// were committed atomically with the materialised rows. This never reads
    /// newer mutable state, so concurrent collection cannot leak a future row
    /// into an earlier lineage sequence.
    pub fn projection_delta_for_compaction(
        &self,
        plan: &LiveDeltaCompactionPlan,
        binding: ProjectionBinding,
    ) -> Result<ProjectionDelta, StoreError> {
        if plan.vehicle_id != binding.vehicle_id
            || plan.anchor_sequence >= plan.head_sequence
            || plan.from_revision <= 0
            || plan.to_revision < plan.from_revision
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut final_mutations = HashMap::<(String, i64), SyncMutation>::new();
        for mutation in &plan.mutations {
            if mutation.vehicle_id != plan.vehicle_id
                || mutation.revision < plan.from_revision
                || mutation.revision > plan.to_revision
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            let key = (mutation.entity.clone(), mutation.entity_id);
            match final_mutations.entry(key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(mutation.clone());
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if mutation.revision > entry.get().revision =>
                {
                    entry.insert(mutation.clone());
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
        let mut ordered = final_mutations.into_values().collect::<Vec<_>>();
        ordered.sort_by_key(|mutation| {
            (
                mutation.revision,
                mutation.entity.clone(),
                mutation.entity_id,
            )
        });
        let car_upserts = ordered
            .iter()
            .filter(|mutation| mutation.entity == "car" && mutation.operation == "upsert")
            .map(|mutation| (mutation.entity_id, mutation.revision))
            .collect::<HashMap<_, _>>();
        let mut settings = HashMap::<i64, (i64, ProjectionCarSettings)>::new();
        for mutation in &ordered {
            if mutation.entity == "car_setting" && mutation.operation == "upsert" {
                settings.insert(
                    mutation.entity_id,
                    (
                        mutation.revision,
                        serde_json::from_str(&mutation.payload_json)
                            .map_err(StoreError::DeserializeLifecycleRow)?,
                    ),
                );
            }
        }
        let mut delta = ProjectionDelta {
            binding,
            sequence: SequenceRange {
                from_exclusive: plan.anchor_sequence,
                to_inclusive: plan.head_sequence,
            },
            parent_digest: plan.anchor_digest,
            cars: Vec::new(),
            car_settings: Vec::new(),
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
            tombstones: Vec::new(),
        };
        for mutation in ordered {
            let entity = parse_sync_entity(&mutation.entity).ok_or_else(|| {
                StoreError::SyncMutation(format!("unknown entity {}", mutation.entity))
            })?;
            if mutation.operation == "tombstone" {
                delta.tombstones.push(ProjectionTombstone {
                    entity,
                    id: mutation.entity_id,
                    car_id: mutation.car_id,
                });
                continue;
            }
            if mutation.operation != "upsert" {
                return Err(StoreError::SyncMutation(
                    "invalid mutation operation".into(),
                ));
            }
            match entity {
                ProjectionDeltaEntity::Car => {
                    let mut car: ProjectionCar = serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?;
                    if let Some((_, settings)) = settings
                        .get(&mutation.entity_id)
                        .filter(|(revision, _)| *revision > mutation.revision)
                    {
                        car.settings = settings.clone();
                    }
                    delta.cars.push(car);
                }
                ProjectionDeltaEntity::CarSetting => {
                    if !car_upserts.contains_key(&mutation.entity_id) {
                        delta.car_settings.push(ProjectionCarSettingsPatch {
                            car_id: mutation.entity_id,
                            settings: serde_json::from_str(&mutation.payload_json)
                                .map_err(StoreError::DeserializeLifecycleRow)?,
                        });
                    }
                }
                ProjectionDeltaEntity::Drive => delta.drives.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::Position => delta.positions.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::Charge => delta.charges.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::ChargeSample => delta.charge_samples.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::State => delta.states.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::Update => delta.updates.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::Geofence | ProjectionDeltaEntity::Address => {
                    return Err(StoreError::SyncMutation(
                        "entity has no typed projection row".into(),
                    ));
                }
            }
        }
        if delta.is_empty() {
            return Err(StoreError::LineageCompactionUnavailable);
        }
        Ok(delta)
    }

    pub fn projection_delta_for_mutations(
        &self,
        claim: &SyncMutationClaim,
        binding: ProjectionBinding,
        sequence: SequenceRange,
        parent_digest: Sha256Digest,
    ) -> Result<ProjectionDelta, StoreError> {
        let mut final_mutations = HashMap::<(String, i64), SyncMutation>::new();
        for mutation in &claim.mutations {
            final_mutations.insert(
                (mutation.entity.clone(), mutation.entity_id),
                mutation.clone(),
            );
        }
        let mut ordered = final_mutations.into_values().collect::<Vec<_>>();
        ordered.sort_by_key(|mutation| {
            (
                mutation.revision,
                mutation.entity.clone(),
                mutation.entity_id,
            )
        });
        let has_car_upsert = ordered
            .iter()
            .any(|mutation| mutation.entity == "car" && mutation.operation == "upsert");
        let connection = self.open()?;
        let mut delta = ProjectionDelta {
            binding,
            sequence,
            parent_digest,
            cars: Vec::new(),
            car_settings: Vec::new(),
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
            tombstones: Vec::new(),
        };
        for mutation in ordered {
            let entity = parse_sync_entity(&mutation.entity).ok_or_else(|| {
                StoreError::SyncMutation(format!("unknown entity {}", mutation.entity))
            })?;
            if mutation.operation == "tombstone" {
                delta.tombstones.push(ProjectionTombstone {
                    entity,
                    id: mutation.entity_id,
                    car_id: mutation.car_id,
                });
                continue;
            }
            if mutation.operation != "upsert" {
                return Err(StoreError::SyncMutation(
                    "invalid mutation operation".into(),
                ));
            }
            match entity {
                ProjectionDeltaEntity::Car => {
                    delta.cars.push(load_projection_json(
                        &connection,
                        "materialised_cars",
                        "car_json",
                        "car_id",
                        &mutation,
                    )?);
                }
                ProjectionDeltaEntity::CarSetting => {
                    if has_car_upsert {
                        continue;
                    }
                    let car: ProjectionCar = load_projection_json(
                        &connection,
                        "materialised_cars",
                        "car_json",
                        "car_id",
                        &mutation,
                    )?;
                    delta.car_settings.push(ProjectionCarSettingsPatch {
                        car_id: mutation.entity_id,
                        settings: car.settings,
                    });
                }
                ProjectionDeltaEntity::Drive => delta.drives.push(load_projection_json(
                    &connection,
                    "materialised_drives",
                    "drive_json",
                    "drive_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::Position => delta.positions.push(load_projection_json(
                    &connection,
                    "materialised_positions",
                    "position_json",
                    "position_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::Charge => delta.charges.push(load_projection_json(
                    &connection,
                    "materialised_charges",
                    "charge_json",
                    "charge_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::ChargeSample => {
                    delta.charge_samples.push(load_projection_json(
                        &connection,
                        "materialised_charge_samples",
                        "sample_json",
                        "sample_id",
                        &mutation,
                    )?);
                }
                ProjectionDeltaEntity::State => delta.states.push(load_projection_json(
                    &connection,
                    "materialised_states",
                    "state_json",
                    "state_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::Update => delta.updates.push(load_projection_json(
                    &connection,
                    "materialised_updates",
                    "update_json",
                    "update_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::Geofence | ProjectionDeltaEntity::Address => {
                    return Err(StoreError::SyncMutation(
                        "entity has no typed projection row".into(),
                    ));
                }
            }
        }
        Ok(delta)
    }

    pub fn commit_v2_delta_claim(
        &self,
        claim: &SyncMutationClaim,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
    ) -> Result<(), StoreError> {
        self.commit_v2_delta_claim_with_limits(
            claim,
            delta,
            cursor_key,
            terminal_cursor,
            ProtocolLimits::default(),
        )
    }

    fn commit_v2_delta_claim_with_limits(
        &self,
        claim: &SyncMutationClaim,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        limits: ProtocolLimits,
    ) -> Result<(), StoreError> {
        if claim.vehicle_id.is_nil()
            || claim.mutations.is_empty()
            || claim.from_revision <= 0
            || claim.to_revision < claim.from_revision
            || claim.to_revision - claim.from_revision + 1
                != i64::try_from(claim.mutations.len()).map_err(|_| StoreError::SequenceTooLarge)?
            || claim
                .mutations
                .windows(2)
                .any(|window| window[1].revision != window[0].revision + 1)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        delta.pack.validate(limits).map_err(StoreError::Manifest)?;
        if delta.from_sequence >= delta.to_sequence
            || delta.pack_digest != delta.pack.sha256
            || delta.pack.schema != HUB_PROJECTION_SCHEMA_V2
            || delta.chain_digest
                != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack.sha256)
            || delta.pack.sequence
                != (SequenceRange {
                    from_exclusive: delta.from_sequence,
                    to_inclusive: delta.to_sequence,
                })
            || delta.to_sequence - delta.from_sequence
                != u64::try_from(claim.mutations.len()).map_err(|_| StoreError::SequenceTooLarge)?
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let binding = self.v2_projection_binding(claim.vehicle_id)?;
        self.verify_import_delta_pack(delta, &binding)?;
        let cursor_claims = terminal_cursor
            .verify(cursor_key)
            .map_err(StoreError::Manifest)?;
        if cursor_claims.protocol != crate::protocol::PROTOCOL_V1
            || cursor_claims.schema != HUB_PROJECTION_SCHEMA_V2
            || cursor_claims.installation_id != binding.installation_id
            || cursor_claims.account_id != binding.account_id
            || cursor_claims.vehicle_id != binding.vehicle_id
            || cursor_claims.generation != binding.generation
            || cursor_claims.sequence != delta.to_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut candidate_lineage = self
            .lineage_manifest_for_vehicle(claim.vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let idempotent_replay = candidate_lineage.head_sequence == delta.to_sequence
            && candidate_lineage.head_digest == delta.chain_digest
            && candidate_lineage.deltas.last() == Some(delta);
        if !idempotent_replay {
            if candidate_lineage.head_sequence != delta.from_sequence
                || candidate_lineage.head_digest != delta.parent_chain_digest
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            candidate_lineage.deltas.push(delta.clone());
            candidate_lineage.head_sequence = delta.to_sequence;
            candidate_lineage.head_digest = delta.chain_digest;
            candidate_lineage.terminal_cursor = terminal_cursor.clone();
            candidate_lineage
                .validate_with_limits(limits)
                .map_err(|error| match error {
                    crate::protocol::ProtocolError::LineageAggregateLimitExceeded => {
                        StoreError::LineageCapacityExhausted
                    }
                    other => StoreError::Manifest(other),
                })?;
        }
        let terminal_cursor_json =
            serde_json::to_string(terminal_cursor).map_err(StoreError::SerializeManifest)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_key = claim.vehicle_id.to_string();
        let current: Option<(i64, String)> = transaction
            .query_row(
                "SELECT head_sequence, head_digest FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((head_sequence, head_digest)) = current else {
            return Err(StoreError::LineageCatalogConflict);
        };
        if delta.pack.snapshot_id
            != transaction
                .query_row(
                    "SELECT snapshot_id FROM sync_bases WHERE vehicle_id = ?1",
                    params![vehicle_key.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?
                .and_then(|snapshot| Uuid::parse_str(&snapshot).ok())
                .ok_or(StoreError::LineageCatalogConflict)?
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let existing_delta: Option<(String, String)> = transaction
            .query_row(
                "SELECT chain_digest, pack_digest FROM sync_deltas
                 WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3",
                params![
                    vehicle_key.as_str(),
                    i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if head_sequence
            == i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?
            && head_digest == delta.chain_digest.to_string()
            && existing_delta.as_ref().is_some_and(|(chain, pack)| {
                chain == &delta.chain_digest.to_string() && pack == &delta.pack_digest.to_string()
            })
        {
            insert_live_delta_span_in_transaction(&transaction, claim, delta)?;
            transaction
                .execute(
                    "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0
                     WHERE vehicle_id = ?1 AND revision BETWEEN ?2 AND ?3",
                    params![vehicle_key, claim.from_revision, claim.to_revision],
                )
                .map_err(StoreError::LineageCatalog)?;
            transaction.commit().map_err(StoreError::LineageCatalog)?;
            return Ok(());
        }
        if head_sequence
            != i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?
            || head_digest != delta.parent_chain_digest.to_string()
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        if let Some((chain_digest, pack_digest)) = existing_delta.as_ref()
            && (chain_digest != &delta.chain_digest.to_string()
                || pack_digest != &delta.pack_digest.to_string())
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
        let updated = transaction
            .execute(
                "INSERT INTO sync_deltas(
                    vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                    chain_digest, pack_digest, pack_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(vehicle_id, from_sequence, to_sequence) DO NOTHING",
                params![
                    vehicle_key.as_str(),
                    i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.parent_chain_digest.to_string(),
                    delta.chain_digest.to_string(),
                    delta.pack_digest.to_string(),
                    pack_json,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if updated != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        insert_live_delta_span_in_transaction(&transaction, claim, delta)?;
        Self::register_lineage_pack_snapshot(
            &transaction,
            &vehicle_key,
            &delta.pack,
            delta.to_sequence,
            &pack_json,
        )?;
        let existing_pack: Option<(String, i64, String, i64, i64)> = transaction
            .query_row(
                "SELECT snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                 FROM sync_packs WHERE sha256 = ?1",
                params![delta.pack.sha256.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some((snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)) =
            existing_pack
        {
            if snapshot_id != delta.pack.snapshot_id.to_string()
                || ordinal != i64::from(delta.pack.ordinal)
                || relative_path != delta.pack.relative_path
                || compressed_bytes
                    != i64::try_from(delta.pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?
                || uncompressed_bytes
                    != i64::try_from(delta.pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        } else {
            let occupied: Option<String> = transaction
                .query_row(
                    "SELECT sha256 FROM sync_packs
                     WHERE snapshot_id = ?1 AND ordinal = ?2",
                    params![
                        delta.pack.snapshot_id.to_string(),
                        i64::from(delta.pack.ordinal)
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if occupied.is_some() {
                return Err(StoreError::LineageCatalogConflict);
            }
            transaction
                .execute(
                    "INSERT INTO sync_packs(
                        sha256, snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        delta.pack.sha256.to_string(),
                        delta.pack.snapshot_id.to_string(),
                        i64::from(delta.pack.ordinal),
                        delta.pack.relative_path,
                        i64::try_from(delta.pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                        i64::try_from(delta.pack.uncompressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }
        let updated = transaction
            .execute(
                "UPDATE sync_heads SET head_sequence = ?1, head_digest = ?2,
                        terminal_cursor = ?3
                 WHERE vehicle_id = ?4 AND head_sequence = ?5
                   AND head_digest = ?6",
                params![
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.chain_digest.to_string(),
                    terminal_cursor_json,
                    vehicle_key.as_str(),
                    head_sequence,
                    head_digest,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if updated != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        transaction
            .execute(
                "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0
                 WHERE vehicle_id = ?1 AND published = 0
                   AND revision BETWEEN ?2 AND ?3",
                params![vehicle_key, claim.from_revision, claim.to_revision],
            )
            .map_err(StoreError::LineageCatalog)?;
        transaction.commit().map_err(StoreError::LineageCatalog)
    }

    /// Atomically replace a contiguous collector-owned suffix with one
    /// journal-derived delta. The immutable base and any import-owned prefix
    /// remain byte-for-byte unchanged. The caller writes and verifies the new
    /// content-addressed pack first; a failed transaction therefore leaves at
    /// most an unreferenced object that normal repair can remove.
    pub fn commit_live_delta_compaction(
        &self,
        plan: &LiveDeltaCompactionPlan,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
    ) -> Result<(), StoreError> {
        self.commit_live_delta_compaction_at(
            plan,
            delta,
            cursor_key,
            terminal_cursor,
            retired_lineage_clock_ms()?,
        )
    }

    fn commit_live_delta_compaction_at(
        &self,
        plan: &LiveDeltaCompactionPlan,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        retired_at_ms: i64,
    ) -> Result<(), StoreError> {
        let expires_at_ms = retired_at_ms
            .checked_add(RETIRED_LINEAGE_PACK_RETENTION_MS)
            .ok_or(StoreError::RetiredLineageClockOverflow)?;
        let revision_span = plan
            .to_revision
            .checked_sub(plan.from_revision)
            .and_then(|count| count.checked_add(1))
            .and_then(|count| u64::try_from(count).ok())
            .ok_or(StoreError::LineageCatalogConflict)?;
        if retired_at_ms < 0
            || plan.vehicle_id.is_nil()
            || plan.replaced_spans.len() < 2
            || plan.anchor_sequence >= plan.head_sequence
            || plan.head_sequence - plan.anchor_sequence != revision_span
            || delta.from_sequence != plan.anchor_sequence
            || delta.to_sequence != plan.head_sequence
            || delta.parent_chain_digest != plan.anchor_digest
            || delta.pack.snapshot_id != plan.base_snapshot_id
            || delta.pack.ordinal != plan.first_ordinal
            || delta.pack_digest != delta.pack.sha256
            || delta.chain_digest
                != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack_digest)
            || delta.pack.sequence
                != (SequenceRange {
                    from_exclusive: plan.anchor_sequence,
                    to_inclusive: plan.head_sequence,
                })
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        delta
            .pack
            .validate(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        let binding = self.v2_projection_binding(plan.vehicle_id)?;
        self.verify_import_delta_pack(delta, &binding)?;
        let cursor_claims = terminal_cursor
            .verify(cursor_key)
            .map_err(StoreError::Manifest)?;
        if cursor_claims.protocol != crate::protocol::PROTOCOL_V1
            || cursor_claims.schema != HUB_PROJECTION_SCHEMA_V2
            || cursor_claims.installation_id != binding.installation_id
            || cursor_claims.account_id != binding.account_id
            || cursor_claims.vehicle_id != binding.vehicle_id
            || cursor_claims.generation != binding.generation
            || cursor_claims.sequence != plan.head_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }

        let mut candidate = self
            .lineage_manifest_for_vehicle(plan.vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        if candidate.base.snapshot_id != plan.base_snapshot_id
            || candidate.head_sequence != plan.head_sequence
            || candidate.head_digest != plan.head_digest
            || candidate.deltas.len() < plan.replaced_spans.len()
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let prefix_len = candidate.deltas.len() - plan.replaced_spans.len();
        if candidate.deltas[prefix_len..]
            .iter()
            .zip(&plan.replaced_spans)
            .any(|(stored, planned)| stored != &planned.delta)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        candidate
            .validate_with_limits(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        let retired_head_digest = candidate.head_digest;
        let retired_manifest_json =
            serde_json::to_vec(&candidate).map_err(StoreError::SerializeManifest)?;
        candidate.deltas.truncate(prefix_len);
        candidate.deltas.push(delta.clone());
        candidate.head_digest = delta.chain_digest;
        candidate.terminal_cursor = terminal_cursor.clone();
        candidate
            .validate_with_limits(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;

        let terminal_cursor_json =
            serde_json::to_string(terminal_cursor).map_err(StoreError::SerializeManifest)?;
        let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
        let vehicle_key = plan.vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current: Option<(String, i64, String)> = transaction
            .query_row(
                "SELECT base_snapshot_id, head_sequence, head_digest
                 FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if current
            != Some((
                plan.base_snapshot_id.to_string(),
                i64::try_from(plan.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                plan.head_digest.to_string(),
            ))
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        for span in &plan.replaced_spans {
            let stored: Option<(i64, i64, String, String)> = transaction
                .query_row(
                    "SELECT spans.from_revision, spans.to_revision,
                            spans.pack_digest, deltas.chain_digest
                     FROM sync_live_delta_spans AS spans
                     JOIN sync_deltas AS deltas
                       ON deltas.vehicle_id = spans.vehicle_id
                      AND deltas.from_sequence = spans.from_sequence
                      AND deltas.to_sequence = spans.to_sequence
                     WHERE spans.vehicle_id = ?1
                       AND spans.from_sequence = ?2 AND spans.to_sequence = ?3",
                    params![
                        vehicle_key.as_str(),
                        i64::try_from(span.delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(span.delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if stored
                != Some((
                    span.from_revision,
                    span.to_revision,
                    span.delta.pack_digest.to_string(),
                    span.delta.chain_digest.to_string(),
                ))
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        let retired_cleanup_cutoff =
            retired_at_ms.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS);
        transaction
            .execute(
                "DELETE FROM sync_retired_lineages WHERE expires_at_ms <= ?1",
                params![retired_cleanup_cutoff],
            )
            .map_err(StoreError::LineageCatalog)?;
        transaction
            .execute(
                "INSERT INTO sync_retired_lineages(
                    vehicle_id, head_digest, manifest_json,
                    retired_at_ms, expires_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    vehicle_key.as_str(),
                    retired_head_digest.to_string(),
                    retired_manifest_json,
                    retired_at_ms,
                    expires_at_ms,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        for span in &plan.replaced_spans {
            transaction
                .execute(
                    "INSERT INTO sync_retired_lineage_packs(
                        vehicle_id, head_digest, pack_digest,
                        relative_path, compressed_bytes
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        vehicle_key.as_str(),
                        retired_head_digest.to_string(),
                        span.delta.pack_digest.to_string(),
                        span.delta.pack.relative_path,
                        i64::try_from(span.delta.pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }
        for span in &plan.replaced_spans {
            let deleted_span = transaction
                .execute(
                    "DELETE FROM sync_live_delta_spans
                     WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3
                       AND from_revision = ?4 AND to_revision = ?5 AND pack_digest = ?6",
                    params![
                        vehicle_key.as_str(),
                        i64::try_from(span.delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(span.delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        span.from_revision,
                        span.to_revision,
                        span.delta.pack_digest.to_string(),
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            if deleted_span != 1 {
                return Err(StoreError::LineageCatalogConflict);
            }
            let deleted = transaction
                .execute(
                    "DELETE FROM sync_deltas
                     WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3
                       AND chain_digest = ?4 AND pack_digest = ?5",
                    params![
                        vehicle_key.as_str(),
                        i64::try_from(span.delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(span.delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        span.delta.chain_digest.to_string(),
                        span.delta.pack_digest.to_string(),
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            if deleted != 1 {
                return Err(StoreError::LineageCatalogConflict);
            }
            let deleted_pack = transaction
                .execute(
                    "DELETE FROM sync_packs WHERE sha256 = ?1 AND snapshot_id = ?2",
                    params![
                        span.delta.pack_digest.to_string(),
                        plan.base_snapshot_id.to_string(),
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            if deleted_pack != 1 {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        let inserted = transaction
            .execute(
                "INSERT INTO sync_deltas(
                    vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                    chain_digest, pack_digest, pack_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    vehicle_key.as_str(),
                    i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.parent_chain_digest.to_string(),
                    delta.chain_digest.to_string(),
                    delta.pack_digest.to_string(),
                    pack_json,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if inserted != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        Self::register_lineage_pack_snapshot(
            &transaction,
            &vehicle_key,
            &delta.pack,
            delta.to_sequence,
            &pack_json,
        )?;
        transaction
            .execute(
                "INSERT INTO sync_packs(
                    sha256, snapshot_id, ordinal, relative_path,
                    compressed_bytes, uncompressed_bytes
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    delta.pack.sha256.to_string(),
                    delta.pack.snapshot_id.to_string(),
                    i64::from(delta.pack.ordinal),
                    delta.pack.relative_path,
                    i64::try_from(delta.pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                    i64::try_from(delta.pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        let compacted_claim = SyncMutationClaim {
            vehicle_id: plan.vehicle_id,
            from_revision: plan.from_revision,
            to_revision: plan.to_revision,
            mutations: plan.mutations.clone(),
        };
        insert_live_delta_span_in_transaction(&transaction, &compacted_claim, delta)?;
        let updated = transaction
            .execute(
                "UPDATE sync_heads SET head_digest = ?1, terminal_cursor = ?2
                 WHERE vehicle_id = ?3 AND base_snapshot_id = ?4
                   AND head_sequence = ?5 AND head_digest = ?6",
                params![
                    delta.chain_digest.to_string(),
                    terminal_cursor_json,
                    vehicle_key,
                    plan.base_snapshot_id.to_string(),
                    i64::try_from(plan.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    plan.head_digest.to_string(),
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if updated != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        transaction.commit().map_err(StoreError::LineageCatalog)
    }

    /// Publish a client-valid import successor under the immutable V2 base.
    ///
    /// The pack must already be a typed delta bound to the base snapshot ID and
    /// the half-open sequence `(from_exclusive, to_inclusive]`. Full-snapshot
    /// packs with a new snapshot identity are refused.
    pub fn finalize_import_delta_successor(
        &self,
        vehicle_id: Uuid,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<(), StoreError> {
        self.finalize_import_delta_successor_with_inventory(
            vehicle_id,
            delta,
            cursor_key,
            terminal_cursor,
            fingerprint,
            geofences,
            None,
            None,
        )
    }

    /// As [`Self::finalize_import_delta_successor`], but atomically advances
    /// the source-owned TeslaMate history inventory with the lineage head.
    pub fn finalize_teslamate_import_delta_successor(
        &self,
        vehicle_id: Uuid,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        inventory: &TeslaMateImportProjectionInventory,
    ) -> Result<(), StoreError> {
        self.finalize_import_delta_successor_with_inventory(
            vehicle_id,
            delta,
            cursor_key,
            terminal_cursor,
            fingerprint,
            geofences,
            Some(inventory),
            None,
        )
    }

    /// As [`Self::finalize_teslamate_import_delta_successor`], but also
    /// atomically replaces the digest-only current projection state. This is
    /// the required completion path for a changed-history successor.
    pub fn finalize_teslamate_import_delta_successor_with_projection_state(
        &self,
        vehicle_id: Uuid,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        inventory: &TeslaMateImportProjectionInventory,
        projection_state: &TeslaMateProjectionState,
    ) -> Result<(), StoreError> {
        self.finalize_import_delta_successor_with_inventory(
            vehicle_id,
            delta,
            cursor_key,
            terminal_cursor,
            fingerprint,
            geofences,
            Some(inventory),
            Some(projection_state),
        )
    }

    /// Atomically publish every bounded direct-import successor produced from
    /// one sealed PostgreSQL snapshot. A source rewrite may exceed one pack,
    /// but clients must never observe only a prefix of its ordered deltas.
    pub fn finalize_import_generation_delta_successors_with_projection_state(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        deltas: &[LineageDelta],
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        projection_state: &TeslaMateProjectionState,
    ) -> Result<(), StoreError> {
        if run_id.is_nil() || source_id.is_nil() || vehicle_id.is_nil() || car_id <= 0 {
            return Err(StoreError::InvalidImportGeneration);
        }
        let Some(first_delta) = deltas.first() else {
            return Err(StoreError::LineageCatalogConflict);
        };
        let final_delta = deltas
            .last()
            .expect("first delta proves the successor batch is non-empty");
        let binding = self.v2_projection_binding(vehicle_id)?;
        // A direct import may only advance the immutable source/car binding
        // that created the base. Never let a caller reuse the vehicle UUID
        // with another source or selected TeslaMate car while replacing its
        // durable digest state and lifecycle session.
        if source_id != binding.account_id || car_id != binding.selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        let existing = self
            .lineage_manifest_for_vehicle(vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let mut prior_to = None;
        let mut prior_chain = None;
        let mut prior_ordinal = None;
        for delta in deltas {
            validate_import_delta_successor_shape(delta)?;
            self.verify_import_delta_pack(delta, &binding)?;
            if delta.pack.snapshot_id != first_delta.pack.snapshot_id
                || prior_to.is_some_and(|value| delta.from_sequence != value)
                || prior_chain.is_some_and(|value| delta.parent_chain_digest != value)
                || prior_ordinal.is_some_and(|value| delta.pack.ordinal <= value)
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            prior_to = Some(delta.to_sequence);
            prior_chain = Some(delta.chain_digest);
            prior_ordinal = Some(delta.pack.ordinal);
        }
        let cursor_claims = terminal_cursor
            .verify(cursor_key)
            .map_err(StoreError::Manifest)?;
        if cursor_claims.protocol != crate::protocol::PROTOCOL_V1
            || cursor_claims.schema != HUB_PROJECTION_SCHEMA_V2
            || cursor_claims.installation_id != binding.installation_id
            || cursor_claims.account_id != binding.account_id
            || cursor_claims.vehicle_id != binding.vehicle_id
            || cursor_claims.generation != binding.generation
            || cursor_claims.sequence != final_delta.to_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        // Validate the exact post-commit lineage before writing any catalogue
        // rows. This enforces the aggregate row/byte/pack ceilings across the
        // existing immutable base plus this whole bounded batch, rather than
        // merely checking every delta in isolation.
        let mut candidate_lineage = existing;
        candidate_lineage.deltas.extend_from_slice(deltas);
        candidate_lineage.head_sequence = final_delta.to_sequence;
        candidate_lineage.head_digest = final_delta.chain_digest;
        candidate_lineage.terminal_cursor = terminal_cursor.clone();
        candidate_lineage
            .validate_with_limits(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        let transfer = projection_state
            .sealed_transfer_for_import_generation(run_id, binding.selected_car_id)?;
        let terminal_cursor_json =
            serde_json::to_string(terminal_cursor).map_err(StoreError::SerializeManifest)?;
        let vehicle_key = vehicle_id.to_string();
        let mut connection = self.open()?;
        attach_teslamate_projection_state_transfer(&connection, &transfer)?;
        let result = (|| -> Result<(), StoreError> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) =
                transaction
                    .query_row(
                        "SELECT sessions.session_json, generations.base_last_observation_id,
                        generations.base_updated_at_ms
                 FROM import_generation_sessions AS sessions
                 JOIN import_generations AS generations USING(run_id)
                 WHERE generations.run_id = ?1 AND generations.source_id = ?2
                   AND generations.vehicle_id = ?3 AND generations.car_id = ?4
                   AND generations.status = 'staging'",
                        params![
                            run_id.to_string(),
                            source_id.to_string(),
                            vehicle_key.as_str(),
                            car_id
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(StoreError::ImportGeneration)?
                    .ok_or(StoreError::ImportGenerationNotFound)?;
            let session =
                serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
            let base_snapshot: String = transaction
                .query_row(
                    "SELECT snapshot_id FROM sync_bases WHERE vehicle_id = ?1",
                    params![vehicle_key.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?
                .ok_or(StoreError::LineageCatalogConflict)?;
            if base_snapshot != first_delta.pack.snapshot_id.to_string() {
                return Err(StoreError::LineageCatalogConflict);
            }
            let (initial_head_sequence, initial_head_digest): (i64, String) = transaction
                .query_row(
                    "SELECT head_sequence, head_digest FROM sync_heads WHERE vehicle_id = ?1",
                    params![vehicle_key.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?
                .ok_or(StoreError::LineageCatalogConflict)?;
            if initial_head_sequence
                != i64::try_from(first_delta.from_sequence)
                    .map_err(|_| StoreError::SequenceTooLarge)?
                || initial_head_digest != first_delta.parent_chain_digest.to_string()
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            // Delta ordinals identify the physical pack order inside the immutable
            // base snapshot.  A strictly increasing ordinal alone permits a
            // caller to skip an unused ordinal; require the exact next catalogue
            // slot instead, while the IMMEDIATE transaction holds the write lock.
            let maximum_ordinal: Option<i64> = transaction
                .query_row(
                    "SELECT MAX(ordinal) FROM sync_packs WHERE snapshot_id = ?1",
                    params![base_snapshot.as_str()],
                    |row| row.get(0),
                )
                .map_err(StoreError::LineageCatalog)?;
            let mut expected_ordinal = maximum_ordinal
                .unwrap_or(-1)
                .checked_add(1)
                .and_then(|ordinal| u32::try_from(ordinal).ok())
                .ok_or(StoreError::LineageCatalogConflict)?;
            let mut expected_sequence = first_delta.from_sequence;
            let mut expected_digest = first_delta.parent_chain_digest;
            for delta in deltas {
                if delta.pack.snapshot_id.to_string() != base_snapshot
                    || delta.from_sequence != expected_sequence
                    || delta.parent_chain_digest != expected_digest
                    || delta.pack.ordinal != expected_ordinal
                {
                    return Err(StoreError::LineageCatalogConflict);
                }
                insert_import_delta_in_transaction(&transaction, &vehicle_key, delta)?;
                expected_sequence = delta.to_sequence;
                expected_digest = delta.chain_digest;
                expected_ordinal = expected_ordinal
                    .checked_add(1)
                    .ok_or(StoreError::LineageCatalogConflict)?;
            }
            let updated = transaction
                .execute(
                    "UPDATE sync_heads SET head_sequence = ?1, head_digest = ?2,
                        terminal_cursor = ?3
                 WHERE vehicle_id = ?4 AND head_sequence = ?5
                   AND head_digest = ?6",
                    params![
                        i64::try_from(final_delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        final_delta.chain_digest.to_string(),
                        terminal_cursor_json,
                        vehicle_key.as_str(),
                        initial_head_sequence,
                        initial_head_digest,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            if updated != 1 {
                return Err(StoreError::LineageCatalogConflict);
            }
            replace_teslamate_import_projection_state_from_attached_in_transaction(
                &transaction,
                vehicle_id,
                binding.account_id,
                final_delta.pack.snapshot_id,
                final_delta.to_sequence,
                binding.selected_car_id,
                &transfer,
                false,
            )?;
            replace_teslamate_import_projection_inventory_from_attached_in_transaction(
                &transaction,
                vehicle_id,
                binding.account_id,
                final_delta.pack.snapshot_id,
                final_delta.to_sequence,
                binding.selected_car_id,
                &transfer,
                false,
            )?;
            promote_imported_open_session_in_transaction(
                &transaction,
                source_id,
                vehicle_id,
                car_id,
                &session,
                updated_at_ms,
                Some((base_last_observation_id, base_updated_at_ms)),
            )?;
            upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
            transaction
                .execute(
                    "INSERT INTO snapshot_fingerprints(
                    vehicle_id, fingerprint_sha256, snapshot_id, head_sequence
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    fingerprint_sha256 = excluded.fingerprint_sha256,
                    snapshot_id = excluded.snapshot_id,
                    head_sequence = excluded.head_sequence",
                    params![
                        vehicle_key.as_str(),
                        fingerprint.as_bytes().as_slice(),
                        final_delta.pack.snapshot_id.to_string(),
                        i64::try_from(final_delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                    ],
                )
                .map_err(StoreError::PublishManifest)?;
            if transaction
                .execute(
                    "DELETE FROM import_generations WHERE run_id = ?1 AND status = 'staging'",
                    params![run_id.to_string()],
                )
                .map_err(StoreError::ImportGeneration)?
                != 1
            {
                return Err(StoreError::ImportGenerationNotFound);
            }
            transaction.commit().map_err(StoreError::ImportGeneration)
        })();
        finish_teslamate_projection_state_transfer(
            result,
            detach_teslamate_projection_state_transfer(self, &connection),
        )
    }

    fn finalize_import_delta_successor_with_inventory(
        &self,
        vehicle_id: Uuid,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        inventory: Option<&TeslaMateImportProjectionInventory>,
        projection_state: Option<&TeslaMateProjectionState>,
    ) -> Result<(), StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        delta
            .pack
            .validate(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        if delta.pack.snapshot_id.is_nil()
            || delta.from_sequence >= delta.to_sequence
            || delta.pack_digest != delta.pack.sha256
            || delta.pack.schema != HUB_PROJECTION_SCHEMA_V2
            || delta.pack.format != crate::protocol::PackFormat::HubProjectionSqlite
            || delta.pack.compression != crate::protocol::PackCompression::Zstd
            || delta.pack.sequence
                != (SequenceRange {
                    from_exclusive: delta.from_sequence,
                    to_inclusive: delta.to_sequence,
                })
            || delta.chain_digest
                != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack.sha256)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let binding = self.v2_projection_binding(vehicle_id)?;
        self.verify_import_delta_pack(delta, &binding)?;
        if let Some(inventory) = inventory {
            validate_teslamate_import_delta_inventory(delta, &binding, inventory)?;
        }
        if projection_state.is_some() && inventory.is_none() {
            return Err(StoreError::LineageCatalogConflict);
        }
        let cursor_claims = terminal_cursor
            .verify(cursor_key)
            .map_err(StoreError::Manifest)?;
        if cursor_claims.protocol != crate::protocol::PROTOCOL_V1
            || cursor_claims.schema != HUB_PROJECTION_SCHEMA_V2
            || cursor_claims.installation_id != binding.installation_id
            || cursor_claims.account_id != binding.account_id
            || cursor_claims.vehicle_id != binding.vehicle_id
            || cursor_claims.generation != binding.generation
            || cursor_claims.sequence != delta.to_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let terminal_cursor_json =
            serde_json::to_string(terminal_cursor).map_err(StoreError::SerializeManifest)?;
        let vehicle_key = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let base_snapshot: String = transaction
            .query_row(
                "SELECT snapshot_id FROM sync_bases WHERE vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        if base_snapshot != delta.pack.snapshot_id.to_string() {
            return Err(StoreError::LineageCatalogConflict);
        }
        let current: Option<(i64, String)> = transaction
            .query_row(
                "SELECT head_sequence, head_digest FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((head_sequence, head_digest)) = current else {
            return Err(StoreError::LineageCatalogConflict);
        };
        if head_sequence
            != i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?
            || head_digest != delta.parent_chain_digest.to_string()
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
        let inserted = transaction
            .execute(
                "INSERT INTO sync_deltas(
                    vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                    chain_digest, pack_digest, pack_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(vehicle_id, from_sequence, to_sequence) DO NOTHING",
                params![
                    vehicle_key.as_str(),
                    i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.parent_chain_digest.to_string(),
                    delta.chain_digest.to_string(),
                    delta.pack_digest.to_string(),
                    pack_json,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if inserted != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        Self::register_lineage_pack_snapshot(
            &transaction,
            &vehicle_key,
            &delta.pack,
            delta.to_sequence,
            &pack_json,
        )?;
        let existing_pack: Option<(String, i64, String, i64, i64)> = transaction
            .query_row(
                "SELECT snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                 FROM sync_packs WHERE sha256 = ?1",
                params![delta.pack.sha256.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some((snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)) =
            existing_pack
        {
            if snapshot_id != delta.pack.snapshot_id.to_string()
                || ordinal != i64::from(delta.pack.ordinal)
                || relative_path != delta.pack.relative_path
                || compressed_bytes
                    != i64::try_from(delta.pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?
                || uncompressed_bytes
                    != i64::try_from(delta.pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        } else {
            let occupied: Option<String> = transaction
                .query_row(
                    "SELECT sha256 FROM sync_packs
                     WHERE snapshot_id = ?1 AND ordinal = ?2",
                    params![
                        delta.pack.snapshot_id.to_string(),
                        i64::from(delta.pack.ordinal)
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if occupied.is_some() {
                return Err(StoreError::LineageCatalogConflict);
            }
            transaction
                .execute(
                    "INSERT INTO sync_packs(
                        sha256, snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        delta.pack.sha256.to_string(),
                        delta.pack.snapshot_id.to_string(),
                        i64::from(delta.pack.ordinal),
                        delta.pack.relative_path,
                        i64::try_from(delta.pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                        i64::try_from(delta.pack.uncompressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }
        let updated = transaction
            .execute(
                "UPDATE sync_heads SET head_sequence = ?1, head_digest = ?2,
                        terminal_cursor = ?3
                 WHERE vehicle_id = ?4 AND head_sequence = ?5
                   AND head_digest = ?6",
                params![
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.chain_digest.to_string(),
                    terminal_cursor_json,
                    vehicle_key.as_str(),
                    head_sequence,
                    head_digest,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if updated != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        if let Some(inventory) = inventory {
            replace_teslamate_import_inventory_in_transaction(
                &transaction,
                vehicle_id,
                delta.pack.snapshot_id,
                delta.to_sequence,
                inventory,
                false,
            )?;
        }
        if let Some(projection_state) = projection_state {
            replace_teslamate_import_projection_state_in_transaction(
                &transaction,
                vehicle_id,
                binding.account_id,
                delta.pack.snapshot_id,
                delta.to_sequence,
                binding.selected_car_id,
                projection_state,
                false,
            )?;
        }
        upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
        transaction
            .execute(
                "INSERT INTO snapshot_fingerprints(
                    vehicle_id, fingerprint_sha256, snapshot_id, head_sequence
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    fingerprint_sha256 = excluded.fingerprint_sha256,
                    snapshot_id = excluded.snapshot_id,
                    head_sequence = excluded.head_sequence",
                params![
                    vehicle_key.as_str(),
                    fingerprint.as_bytes().as_slice(),
                    delta.pack.snapshot_id.to_string(),
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                ],
            )
            .map_err(StoreError::PublishManifest)?;
        transaction.commit().map_err(StoreError::LineageCatalog)
    }

    /// Load the exact source-owned history rows from the most recent
    /// successful TeslaMate import.  Missing or mismatched provenance is a
    /// hard failure: callers must not guess deletes from mutable history.
    pub fn teslamate_import_projection_inventory(
        &self,
        vehicle_id: Uuid,
        source_id: Uuid,
        selected_car_id: i64,
    ) -> Result<TeslaMateImportProjectionInventory, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if source_id.is_nil() || selected_car_id <= 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let connection = self.open()?;
        let header: Option<(String, String, i64)> = connection
            .query_row(
                "SELECT source_id, base_snapshot_id, selected_car_id
                   FROM teslamate_import_projection_heads WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((stored_source_id, base_snapshot_id, stored_selected_car_id)) = header else {
            return Err(StoreError::TeslaMateImportInventoryMissing(vehicle_id));
        };
        if stored_source_id != source_id.to_string()
            || stored_selected_car_id != selected_car_id
            || !connection
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM sync_bases
                         WHERE vehicle_id = ?1 AND snapshot_id = ?2
                    )",
                    params![vehicle_id.to_string(), base_snapshot_id],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(StoreError::LineageCatalog)?
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut statement = connection
            .prepare(
                "SELECT entity, entity_id
                   FROM teslamate_import_projection_rows
                  WHERE vehicle_id = ?1
                  ORDER BY entity, entity_id",
            )
            .map_err(StoreError::LineageCatalog)?;
        let rows = statement
            .query_map(params![vehicle_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(StoreError::LineageCatalog)?;
        let mut inventory_rows = Vec::new();
        for row in rows {
            let (entity, id) = row.map_err(StoreError::LineageCatalog)?;
            inventory_rows.push(ProjectionTombstone {
                entity: teslamate_inventory_entity(&entity)
                    .ok_or(StoreError::LineageCatalogConflict)?,
                id,
                car_id: selected_car_id,
            });
        }
        validate_teslamate_import_inventory_rows(selected_car_id, &inventory_rows)?;
        Ok(TeslaMateImportProjectionInventory {
            source_id,
            selected_car_id,
            rows: inventory_rows,
        })
    }

    /// Open a bounded digest lookup for a verified prior TeslaMate import.
    /// A legacy deletion inventory is not a substitute: this deliberately
    /// fails if the separate durable digest state was never persisted.
    pub fn teslamate_import_projection_state_lookup(
        &self,
        vehicle_id: Uuid,
        source_id: Uuid,
        selected_car_id: i64,
    ) -> Result<TeslaMateImportProjectionStateLookup, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if source_id.is_nil() || selected_car_id <= 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let connection = self.open_read_only_connection()?;
        // Keep every digest lookup/page on one SQLite read snapshot. If a
        // publisher advances the lineage concurrently, the later atomic
        // finalizer revalidates its head and refuses this stale capture.
        connection
            .execute_batch("BEGIN")
            .map_err(StoreError::Begin)?;
        let header: Option<(String, String, i64, i64)> = connection
            .query_row(
                "SELECT source_id, base_snapshot_id, selected_car_id, head_sequence
                   FROM teslamate_import_projection_state_heads
                  WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((
            stored_source_id,
            stored_base_snapshot_id,
            stored_selected_car_id,
            head_sequence,
        )) = header
        else {
            return Err(StoreError::TeslaMateImportProjectionStateMissing(
                vehicle_id,
            ));
        };
        let stored_source_id = Uuid::parse_str(&stored_source_id)
            .map_err(|_| StoreError::InvalidStoredUuid("projection-state source id"))?;
        let base_snapshot_id = Uuid::parse_str(&stored_base_snapshot_id)
            .map_err(|_| StoreError::InvalidStoredUuid("projection-state base snapshot id"))?;
        let head_sequence =
            u64::try_from(head_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        let header = TeslaMateImportProjectionStateHeader {
            source_id: stored_source_id,
            base_snapshot_id,
            selected_car_id: stored_selected_car_id,
            head_sequence,
        };
        if header.source_id != source_id || header.selected_car_id != selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        let current: Option<(String, i64)> = connection
            .query_row(
                "SELECT base.snapshot_id, head.head_sequence
                   FROM sync_bases AS base
                   JOIN sync_heads AS head
                     ON head.vehicle_id = base.vehicle_id
                    AND head.base_snapshot_id = base.snapshot_id
                  WHERE base.vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((current_base_snapshot_id, current_head_sequence)) = current else {
            return Err(StoreError::LineageCatalogConflict);
        };
        if current_base_snapshot_id != header.base_snapshot_id.to_string()
            || current_head_sequence
                != i64::try_from(header.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let binding: Option<(String, String, i64)> = connection
            .query_row(
                "SELECT snapshot_id, account_id, selected_car_id
                   FROM v2_base_bindings
                  WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((binding_base_snapshot_id, binding_account_id, binding_selected_car_id)) = binding
        else {
            return Err(StoreError::ImmutableBaseBindingMissing(vehicle_id));
        };
        if binding_base_snapshot_id != header.base_snapshot_id.to_string()
            || binding_account_id != header.source_id.to_string()
            || binding_selected_car_id != header.selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(TeslaMateImportProjectionStateLookup {
            connection,
            vehicle_id,
            header,
            digest_caches: Vec::new(),
            #[cfg(test)]
            digest_cache_loads: 0,
        })
    }

    /// Read one ordered bounded page of a verified prior state without
    /// exposing its SQLite connection to the caller.
    pub fn teslamate_import_projection_state_page(
        &self,
        vehicle_id: Uuid,
        source_id: Uuid,
        selected_car_id: i64,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateDigestPage, StoreError> {
        let mut lookup =
            self.teslamate_import_projection_state_lookup(vehicle_id, source_id, selected_car_id)?;
        lookup.page_after_store(after, limit)
    }

    /// Whether this V2 vehicle already has a durable projection-state head.
    /// The direct importer uses this narrow predicate only to decide whether
    /// an old inventory-only base may attempt the one-time bridge; full
    /// binding and head validation remains in the lookup/finalizer paths.
    pub(crate) fn teslamate_import_projection_state_exists(
        &self,
        vehicle_id: Uuid,
    ) -> Result<bool, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        self.open()?
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM teslamate_import_projection_state_heads
                     WHERE vehicle_id = ?1
                )",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)
    }

    /// True only for the exact legacy direct-import shape that can be safely
    /// upgraded in place. Anything less precise must use an owner-approved
    /// rebase rather than guessing a sparse successor.
    pub(crate) fn legacy_teslamate_direct_bridge_is_eligible(
        &self,
        vehicle_id: Uuid,
        source_id: Uuid,
        selected_car_id: i64,
    ) -> Result<bool, StoreError> {
        let connection = self.open()?;
        Ok(
            legacy_direct_bridge_candidate(&connection, vehicle_id, source_id, selected_car_id)?
                .is_some(),
        )
    }

    /// Atomically attach a sealed digest state to one verified legacy direct
    /// base and replace its retired physical fingerprint with the current
    /// fragment-independent logical fingerprint. This never publishes a pack,
    /// delta, or sequence. Any failed compatibility check rolls all writes
    /// back and reports that a rebase is required.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bridge_legacy_teslamate_direct_import(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        selected_car_id: i64,
        legacy_fingerprint: Sha256Digest,
        logical_fingerprint: Sha256Digest,
        projection_state: &TeslaMateProjectionState,
    ) -> Result<TeslaMateLegacyDirectBridgeResult, StoreError> {
        if run_id.is_nil() || source_id.is_nil() || vehicle_id.is_nil() || selected_car_id <= 0 {
            return Err(StoreError::InvalidImportGeneration);
        }
        let transfer = projection_state
            .sealed_transfer_for_import_generation(run_id, selected_car_id)
            .map_err(|error| {
                legacy_direct_bridge_state_error(
                    vehicle_id,
                    StoreError::TeslaMateProjectionState(error),
                )
            })?;
        let mut connection = self.open()?;
        attach_teslamate_projection_state_transfer(&connection, &transfer)?;
        let result = (|| -> Result<TeslaMateLegacyDirectBridgeResult, StoreError> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            let generation_is_staging: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                    SELECT 1 FROM import_generations
                     WHERE run_id = ?1 AND source_id = ?2 AND vehicle_id = ?3
                       AND car_id = ?4 AND status = 'staging'
                )",
                    params![
                        run_id.to_string(),
                        source_id.to_string(),
                        vehicle_id.to_string(),
                        selected_car_id,
                    ],
                    |row| row.get(0),
                )
                .map_err(StoreError::ImportGeneration)?;
            if !generation_is_staging {
                return Err(StoreError::ImportGenerationNotFound);
            }
            let candidate = legacy_direct_bridge_candidate(
                &transaction,
                vehicle_id,
                source_id,
                selected_car_id,
            )?
            .ok_or(StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id))?;
            if candidate.legacy_fingerprint != legacy_fingerprint {
                return Err(StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id));
            }
            replace_teslamate_import_projection_state_from_attached_in_transaction(
                &transaction,
                vehicle_id,
                source_id,
                candidate.snapshot_id,
                candidate.head_sequence,
                selected_car_id,
                &transfer,
                true,
            )
            .map_err(|error| legacy_direct_bridge_state_error(vehicle_id, error))?;
            if !legacy_projection_inventory_matches_state_in_transaction(&transaction, vehicle_id)?
            {
                return Err(StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id));
            }
            if transaction
                .execute(
                    "UPDATE snapshot_fingerprints
                    SET fingerprint_sha256 = ?1
                  WHERE vehicle_id = ?2
                    AND fingerprint_sha256 = ?3
                    AND snapshot_id = ?4
                    AND head_sequence = ?5",
                    params![
                        logical_fingerprint.as_bytes().as_slice(),
                        vehicle_id.to_string(),
                        legacy_fingerprint.as_bytes().as_slice(),
                        candidate.snapshot_id.to_string(),
                        i64::try_from(candidate.head_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                    ],
                )
                .map_err(StoreError::PublishManifest)?
                != 1
            {
                return Err(StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id));
            }
            transaction
                .execute(
                    "INSERT INTO teslamate_import_projection_state_bridges(
                    vehicle_id, base_snapshot_id, head_sequence, algorithm,
                    legacy_fingerprint_sha256, logical_fingerprint_sha256
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        vehicle_id.to_string(),
                        candidate.snapshot_id.to_string(),
                        i64::try_from(candidate.head_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        TESLAMATE_LEGACY_DIRECT_BRIDGE_ALGORITHM,
                        legacy_fingerprint.as_bytes().as_slice(),
                        logical_fingerprint.as_bytes().as_slice(),
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            if transaction
                .execute(
                    "DELETE FROM import_generations WHERE run_id = ?1 AND status = 'staging'",
                    params![run_id.to_string()],
                )
                .map_err(StoreError::ImportGeneration)?
                != 1
            {
                return Err(StoreError::ImportGenerationNotFound);
            }
            transaction.commit().map_err(StoreError::ImportGeneration)?;
            Ok(TeslaMateLegacyDirectBridgeResult {
                snapshot_id: candidate.snapshot_id,
                head_sequence: candidate.head_sequence,
                total_rows: candidate.total_rows,
            })
        })();
        finish_teslamate_projection_state_transfer(
            result,
            detach_teslamate_projection_state_transfer(self, &connection),
        )
    }

    /// True when the vehicle's current source fingerprint equals `fingerprint`.
    /// Unlike [`Self::manifest_for_snapshot_fingerprint`], this does not require
    /// a legacy full-snapshot `sync_manifests` row, so import deltas can skip.
    pub fn source_fingerprint_matches(
        &self,
        vehicle_id: Uuid,
        fingerprint: Sha256Digest,
    ) -> Result<bool, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM snapshot_fingerprints
                     WHERE vehicle_id = ?1 AND fingerprint_sha256 = ?2
                )",
                params![vehicle_id.to_string(), fingerprint.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    pub fn manifest_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<SyncManifest>, StoreError> {
        let connection = self.open()?;
        let payload = connection
            .query_row(
                "SELECT manifest_json FROM sync_manifests \
                 WHERE vehicle_id = ?1
                   AND json_extract(manifest_json, '$.mode') = 'full_snapshot'
                 ORDER BY head_sequence DESC LIMIT 1",
                params![vehicle_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        payload.map(decode_manifest).transpose()
    }

    /// Load the exact manifest atomically associated with a source snapshot
    /// fingerprint. Legacy unbound fingerprints deliberately return `None`.
    pub fn manifest_for_snapshot_fingerprint(
        &self,
        vehicle_id: Uuid,
        fingerprint: Sha256Digest,
    ) -> Result<Option<SyncManifest>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let payload = connection
            .query_row(
                "SELECT manifests.manifest_json
                 FROM snapshot_fingerprints AS fingerprints
                 JOIN sync_manifests AS manifests
                   ON manifests.snapshot_id = fingerprints.snapshot_id
                  AND manifests.vehicle_id = fingerprints.vehicle_id
                  AND manifests.head_sequence = fingerprints.head_sequence
                 WHERE fingerprints.vehicle_id = ?1
                   AND fingerprints.fingerprint_sha256 = ?2",
                params![vehicle_id.to_string(), fingerprint.as_bytes().as_slice()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        payload.map(decode_manifest).transpose()
    }

    pub fn snapshot_fingerprint_is_current(
        &self,
        vehicle_id: Uuid,
        fingerprint: Sha256Digest,
    ) -> Result<bool, StoreError> {
        Ok(self
            .manifest_for_snapshot_fingerprint(vehicle_id, fingerprint)?
            .is_some())
    }

    /// Whether any historical manifest catalogue entry references this pack.
    /// Import cleanup uses this rather than only the current manifest because
    /// older snapshots remain valid recovery and sync inputs.
    pub(crate) fn pack_sha256_is_catalogued(&self, sha256: &str) -> Result<bool, StoreError> {
        self.open()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_packs WHERE sha256 = ?1)",
                params![sha256],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    pub fn record_snapshot_fingerprint(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
    ) -> Result<(), StoreError> {
        if manifest.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
        transaction.commit().map_err(StoreError::PublishManifest)
    }

    /// Reserve a full-snapshot marker while the caller owns the publication
    /// gate. Other modules cannot reserve a marker without this token.
    pub(crate) fn reserve_next_full_snapshot_sequence(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
    ) -> Result<u64, StoreError> {
        self.next_full_snapshot_sequence_while_gated(vehicle_id)
    }

    /// Durably reserve the next full-snapshot marker while owning the
    /// publication gate for this single reservation.
    ///
    /// This compatibility seam keeps callers from bypassing publication
    /// serialization without exposing the gate token itself. Workflows that
    /// already hold the gate should use `reserve_next_full_snapshot_sequence`.
    pub fn next_full_snapshot_sequence(&self, vehicle_id: Uuid) -> Result<u64, StoreError> {
        let publication_gate = self.try_acquire_publication_gate()?;
        self.reserve_next_full_snapshot_sequence(&publication_gate, vehicle_id)
    }

    /// Durably reserve the next full-snapshot marker for one Hub vehicle.
    ///
    /// Reservation happens before pack construction, so a failed unpublished
    /// build can leave a harmless gap. It cannot reuse a marker already handed
    /// to another process, keeping successful publications totally ordered.
    fn next_full_snapshot_sequence_while_gated(&self, vehicle_id: Uuid) -> Result<u64, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        // The counter records reservations, while the catalogue can also be
        // advanced by a live publisher. The caller holds the publication gate.
        let next_counter: Option<i64> = transaction
            .query_row(
                "SELECT next_sequence FROM vehicle_snapshot_sequences WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let catalog_head: Option<i64> = transaction
            .query_row(
                "SELECT MAX(head_sequence) FROM sync_manifests WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let reserved = catalog_head
            .unwrap_or(0)
            .max(next_counter.unwrap_or(1).saturating_sub(1))
            .checked_add(1)
            .ok_or(StoreError::SequenceExhausted)?;
        let next_sequence = reserved
            .checked_add(1)
            .ok_or(StoreError::SequenceExhausted)?;
        transaction
            .execute(
                "INSERT INTO vehicle_snapshot_sequences (vehicle_id, next_sequence)
                 VALUES (?1, ?2)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    next_sequence = MAX(vehicle_snapshot_sequences.next_sequence, excluded.next_sequence)",
                params![vehicle_id.to_string(), next_sequence],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        u64::try_from(reserved)
            .ok()
            .filter(|sequence| *sequence >= 1)
            .ok_or(StoreError::SequenceExhausted)
    }

    /// Make the pack catalogue, imported lifecycle recovery state, geofences,
    /// fingerprint, and staging cleanup visible in one SQLite transaction.
    /// Callers retain immutable pack chunks before this transaction starts.
    pub fn finalize_import_generation(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<(), StoreError> {
        self.finalize_import_generation_with_metadata(
            run_id,
            source_id,
            vehicle_id,
            car_id,
            updated_at_ms,
            manifest,
            fingerprint,
            geofences,
            None,
        )
    }

    /// Finalize an imported V2 base while retaining the exact immutable
    /// projection binding that wrote it.
    pub fn finalize_import_generation_with_binding(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
    ) -> Result<(), StoreError> {
        self.finalize_import_generation_with_metadata(
            run_id,
            source_id,
            vehicle_id,
            car_id,
            updated_at_ms,
            manifest,
            fingerprint,
            geofences,
            Some(binding),
        )
    }

    /// Finalize a direct TeslaMate V2 base together with the sealed,
    /// digest-only state and deletion inventory needed for a later sparse
    /// successor. A verified read-only SQLite attachment transfers the state
    /// and inventory set-wise inside the same transaction; no million-row
    /// inventory is materialised in memory.
    pub fn finalize_import_generation_with_projection_state(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
        projection_state: &TeslaMateProjectionState,
    ) -> Result<(), StoreError> {
        if run_id.is_nil()
            || source_id.is_nil()
            || vehicle_id.is_nil()
            || car_id <= 0
            || manifest.vehicle_id != vehicle_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        validate_immutable_v2_base_binding(manifest, binding)?;
        // The staging generation feeds the imported lifecycle materialisation,
        // while the V2 binding owns the pack/state scope.  They must describe
        // the exact same source/car; otherwise a valid selected-car base could
        // be published next to lifecycle rows attributed to another car.
        if source_id != binding.account_id || car_id != binding.selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        let transfer = projection_state
            .sealed_transfer_for_import_generation(run_id, binding.selected_car_id)?;
        let mut connection = self.open()?;
        attach_teslamate_projection_state_transfer(&connection, &transfer)?;
        let result = (|| -> Result<(), StoreError> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) =
                transaction
                    .query_row(
                        "SELECT sessions.session_json, generations.base_last_observation_id,
                            generations.base_updated_at_ms
                     FROM import_generation_sessions AS sessions
                     JOIN import_generations AS generations USING(run_id)
                     WHERE generations.run_id = ?1 AND generations.source_id = ?2
                       AND generations.vehicle_id = ?3 AND generations.car_id = ?4
                       AND generations.status = 'staging'",
                        params![
                            run_id.to_string(),
                            source_id.to_string(),
                            vehicle_id.to_string(),
                            car_id
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(StoreError::ImportGeneration)?
                    .ok_or(StoreError::ImportGenerationNotFound)?;
            let session =
                serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
            publish_manifest_in_transaction(&transaction, manifest, Some(binding))?;
            promote_imported_open_session_in_transaction(
                &transaction,
                source_id,
                vehicle_id,
                car_id,
                &session,
                updated_at_ms,
                Some((base_last_observation_id, base_updated_at_ms)),
            )?;
            replace_teslamate_import_projection_state_from_attached_in_transaction(
                &transaction,
                vehicle_id,
                binding.account_id,
                manifest.snapshot_id,
                manifest.head_sequence,
                binding.selected_car_id,
                &transfer,
                true,
            )?;
            replace_teslamate_import_projection_inventory_from_attached_in_transaction(
                &transaction,
                vehicle_id,
                binding.account_id,
                manifest.snapshot_id,
                manifest.head_sequence,
                binding.selected_car_id,
                &transfer,
                true,
            )?;
            upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
            record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
            if transaction
                .execute(
                    "DELETE FROM import_generations WHERE run_id = ?1 AND status = 'staging'",
                    params![run_id.to_string()],
                )
                .map_err(StoreError::ImportGeneration)?
                != 1
            {
                return Err(StoreError::ImportGenerationNotFound);
            }
            transaction.commit().map_err(StoreError::ImportGeneration)
        })();
        finish_teslamate_projection_state_transfer(
            result,
            detach_teslamate_projection_state_transfer(self, &connection),
        )
    }

    fn finalize_import_generation_with_metadata(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: Option<&ProjectionBinding>,
    ) -> Result<(), StoreError> {
        if run_id.is_nil()
            || source_id.is_nil()
            || vehicle_id.is_nil()
            || car_id <= 0
            || manifest.vehicle_id != vehicle_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        if manifest.schema == HUB_PROJECTION_SCHEMA_V2 && binding.is_none() {
            return Err(StoreError::ImmutableBaseBindingMissing(vehicle_id));
        }
        if let Some(binding) = binding {
            validate_immutable_v2_base_binding(manifest, binding)?;
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) =
            transaction
                .query_row(
                    "SELECT sessions.session_json, generations.base_last_observation_id,
                        generations.base_updated_at_ms
                 FROM import_generation_sessions AS sessions
                 JOIN import_generations AS generations USING(run_id)
                 WHERE generations.run_id = ?1 AND generations.source_id = ?2
                   AND generations.vehicle_id = ?3 AND generations.car_id = ?4
                   AND generations.status = 'staging'",
                    params![
                        run_id.to_string(),
                        source_id.to_string(),
                        vehicle_id.to_string(),
                        car_id
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(StoreError::ImportGeneration)?
                .ok_or(StoreError::ImportGenerationNotFound)?;
        let session =
            serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
        publish_manifest_in_transaction(&transaction, manifest, binding)?;
        promote_imported_open_session_in_transaction(
            &transaction,
            source_id,
            vehicle_id,
            car_id,
            &session,
            updated_at_ms,
            Some((base_last_observation_id, base_updated_at_ms)),
        )?;
        upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
        record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
        if transaction
            .execute(
                "DELETE FROM import_generations WHERE run_id = ?1 AND status = 'staging'",
                params![run_id.to_string()],
            )
            .map_err(StoreError::ImportGeneration)?
            != 1
        {
            return Err(StoreError::ImportGenerationNotFound);
        }
        transaction.commit().map_err(StoreError::ImportGeneration)
    }

    /// Atomically catalogue a sealed import history snapshot and its source
    /// fingerprint. Callers retain immutable pack chunks before this call.
    pub fn finalize_import_snapshot(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<(), StoreError> {
        self.finalize_import_snapshot_with_metadata(
            manifest,
            fingerprint,
            geofences,
            None,
            None,
            None,
        )
    }

    /// As [`Self::finalize_import_snapshot`], but records the exact V2 base
    /// binding that was used to write the immutable pack.  All production V2
    /// publishers must use this entry point; a later delta must never infer
    /// account, generation, or selected car from mutable local state.
    pub fn finalize_import_snapshot_with_binding(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
    ) -> Result<(), StoreError> {
        self.finalize_import_snapshot_with_metadata(
            manifest,
            fingerprint,
            geofences,
            Some(binding),
            None,
            None,
        )
    }

    /// Atomically catalogue a TeslaMate V2 base together with the exact
    /// source-owned projection inventory.  The inventory is what permits a
    /// future source rewrite to emit precise tombstones rather than silently
    /// retaining removed rows on a client.
    pub fn finalize_teslamate_import_snapshot(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
        inventory: &TeslaMateImportProjectionInventory,
    ) -> Result<(), StoreError> {
        self.finalize_import_snapshot_with_metadata(
            manifest,
            fingerprint,
            geofences,
            Some(binding),
            Some(inventory),
            None,
        )
    }

    /// Atomically catalogue a TeslaMate V2 base with both legacy deletion
    /// inventory and the digest-only state needed by a later sparse successor.
    pub fn finalize_teslamate_import_snapshot_with_projection_state(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
        inventory: &TeslaMateImportProjectionInventory,
        projection_state: &TeslaMateProjectionState,
    ) -> Result<(), StoreError> {
        self.finalize_import_snapshot_with_metadata(
            manifest,
            fingerprint,
            geofences,
            Some(binding),
            Some(inventory),
            Some(projection_state),
        )
    }

    fn finalize_import_snapshot_with_metadata(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: Option<&ProjectionBinding>,
        inventory: Option<&TeslaMateImportProjectionInventory>,
        projection_state: Option<&TeslaMateProjectionState>,
    ) -> Result<(), StoreError> {
        if manifest.schema == HUB_PROJECTION_SCHEMA_V2 && binding.is_none() {
            return Err(StoreError::ImmutableBaseBindingMissing(manifest.vehicle_id));
        }
        if let Some(binding) = binding {
            validate_immutable_v2_base_binding(manifest, binding)?;
        }
        if let Some(inventory) = inventory {
            let binding = binding.ok_or(StoreError::LineageCatalogConflict)?;
            validate_teslamate_import_inventory(manifest, binding, inventory)?;
        }
        if projection_state.is_some() && inventory.is_none() {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        publish_manifest_in_transaction(&transaction, manifest, binding)?;
        if let Some(inventory) = inventory {
            replace_teslamate_import_inventory_in_transaction(
                &transaction,
                manifest.vehicle_id,
                manifest.snapshot_id,
                manifest.head_sequence,
                inventory,
                true,
            )?;
        }
        if let Some(projection_state) = projection_state {
            let binding = binding.ok_or(StoreError::LineageCatalogConflict)?;
            replace_teslamate_import_projection_state_in_transaction(
                &transaction,
                manifest.vehicle_id,
                binding.account_id,
                manifest.snapshot_id,
                manifest.head_sequence,
                binding.selected_car_id,
                projection_state,
                true,
            )?;
        }
        upsert_geofences_in_transaction(&transaction, manifest.vehicle_id, geofences)?;
        record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
        transaction.commit().map_err(StoreError::PublishManifest)
    }

    /// Start an inactive import generation. Nothing in this generation is
    /// visible to lifecycle reads or published manifests.
    pub fn begin_import_generation(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        created_at_ms: i64,
    ) -> Result<Uuid, StoreError> {
        if source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        require_positive_db(car_id, "import car_id")?;
        validate_timestamp("import generation created_at_ms", created_at_ms)?;
        let run_id = Uuid::new_v4();
        let connection = self.open()?;
        let (base_last_observation_id, base_updated_at_ms): (i64, i64) = connection
            .query_row(
                "SELECT last_observation_id, updated_at_ms
                 FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::ImportGeneration)?
            .unwrap_or((0, 0));
        connection
            .execute(
                "INSERT INTO import_generations(
                    run_id, source_id, vehicle_id, car_id, status, created_at_ms,
                    base_last_observation_id, base_updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, 'staging', ?5, ?6, ?7)",
                params![
                    run_id.to_string(),
                    source_id.to_string(),
                    vehicle_id.to_string(),
                    car_id,
                    created_at_ms,
                    base_last_observation_id,
                    base_updated_at_ms
                ],
            )
            .map_err(StoreError::ImportGeneration)?;
        Ok(run_id)
    }

    /// Replace the inactive generation's open-session image. This is safe to
    /// call after each bounded source read; active lifecycle state is untouched.
    pub fn stage_import_generation_session(
        &self,
        run_id: Uuid,
        session: &TeslaMateOpenSession,
    ) -> Result<(), StoreError> {
        if run_id.is_nil() {
            return Err(StoreError::InvalidImportGeneration);
        }
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        let encoded = serde_json::to_string(session).map_err(StoreError::SerializeLifecycleRow)?;
        let connection = self.open()?;
        let updated = connection
            .execute(
                "INSERT INTO import_generation_sessions(run_id, session_json)
                 SELECT ?1, ?2 WHERE EXISTS(
                    SELECT 1 FROM import_generations
                    WHERE run_id = ?1 AND status = 'staging'
                 )
                 ON CONFLICT(run_id) DO UPDATE SET session_json = excluded.session_json",
                params![run_id.to_string(), encoded],
            )
            .map_err(StoreError::ImportGeneration)?;
        if updated == 0 {
            return Err(StoreError::ImportGenerationNotFound);
        }
        Ok(())
    }

    /// Atomically promote the already validated inactive session into the
    /// existing lifecycle tables and consume its staging generation. The
    /// caller invokes this after either pack publication or an unchanged
    /// completed-history fingerprint match.
    pub fn promote_import_generation(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) =
            transaction
                .query_row(
                    "SELECT sessions.session_json, generations.base_last_observation_id,
                        generations.base_updated_at_ms
                 FROM import_generation_sessions AS sessions
                 JOIN import_generations AS generations USING(run_id)
                 WHERE run_id = ?1 AND source_id = ?2 AND vehicle_id = ?3
                   AND car_id = ?4 AND status = 'staging'",
                    params![
                        run_id.to_string(),
                        source_id.to_string(),
                        vehicle_id.to_string(),
                        car_id
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(StoreError::ImportGeneration)?
                .ok_or(StoreError::ImportGenerationNotFound)?;
        let session: TeslaMateOpenSession =
            serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
        let report = promote_imported_open_session_in_transaction(
            &transaction,
            source_id,
            vehicle_id,
            car_id,
            &session,
            updated_at_ms,
            Some((base_last_observation_id, base_updated_at_ms)),
        )?;
        if transaction
            .execute(
                "DELETE FROM import_generations WHERE run_id = ?1 AND status = 'staging'",
                params![run_id.to_string()],
            )
            .map_err(StoreError::ImportGeneration)?
            != 1
        {
            return Err(StoreError::ImportGenerationNotFound);
        }
        transaction.commit().map_err(StoreError::ImportGeneration)?;
        Ok(report)
    }

    pub fn abort_import_generation(&self, run_id: Uuid) -> Result<(), StoreError> {
        if run_id.is_nil() {
            return Ok(());
        }
        let connection = self.open()?;
        connection
            .execute(
                "DELETE FROM import_generations WHERE run_id = ?1",
                params![run_id.to_string()],
            )
            .map_err(StoreError::ImportGeneration)?;
        Ok(())
    }

    /// Commit a validated lineage only after every referenced immutable pack
    /// is present, size-correct, and hash-correct. The DB transaction never
    /// becomes visible before that verification completes.
    pub fn commit_lineage_catalog(&self, lineage: &LineageManifestV2) -> Result<(), StoreError> {
        // The generic lineage API does not carry ProjectionBinding.  It is
        // retained for schema-1 lineage scenarios only; schema-2.1 bases must
        // use a binding-aware finalizer so the persisted base cannot later be
        // retargeted from mutable local state.
        if lineage.schema == HUB_PROJECTION_SCHEMA_V2 {
            return Err(StoreError::ImmutableBaseBindingMissing(lineage.vehicle_id));
        }
        lineage.validate().map_err(StoreError::Manifest)?;
        let mut packs = lineage.base.packs.clone();
        packs.extend(lineage.deltas.iter().map(|delta| delta.pack.clone()));
        for pack in &packs {
            self.verify_lineage_pack(pack)?;
        }

        let vehicle_id = lineage.vehicle_id.to_string();
        let base_json =
            serde_json::to_vec(&lineage.base.packs).map_err(StoreError::SerializeManifest)?;
        let cursor = serde_json::to_string(&lineage.terminal_cursor)
            .map_err(StoreError::SerializeManifest)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;

        let existing_base: Option<(String, i64, String, Vec<u8>)> = transaction
            .query_row(
                "SELECT snapshot_id, base_sequence, base_digest, packs_json
                 FROM sync_bases WHERE vehicle_id = ?1",
                params![vehicle_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some((snapshot_id, sequence, digest, stored_packs)) = existing_base {
            if snapshot_id != lineage.base.snapshot_id.to_string()
                || u64::try_from(sequence).ok() != Some(lineage.base.sequence)
                || digest != lineage.base.digest.to_string()
                || stored_packs != base_json
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        } else {
            transaction
                .execute(
                    "INSERT INTO sync_bases
                     (vehicle_id, snapshot_id, base_sequence, base_digest, packs_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        vehicle_id.as_str(),
                        lineage.base.snapshot_id.to_string(),
                        i64::try_from(lineage.base.sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        lineage.base.digest.to_string(),
                        base_json,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }

        for delta in &lineage.deltas {
            let existing: Option<(String, String)> = transaction
                .query_row(
                    "SELECT chain_digest, pack_digest FROM sync_deltas
                     WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3",
                    params![
                        vehicle_id.as_str(),
                        i64::try_from(delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if let Some((chain_digest, pack_digest)) = existing {
                if chain_digest != delta.chain_digest.to_string()
                    || pack_digest != delta.pack_digest.to_string()
                {
                    return Err(StoreError::LineageCatalogConflict);
                }
                continue;
            }
            let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
            transaction
                .execute(
                    "INSERT INTO sync_deltas
                     (vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                      chain_digest, pack_digest, pack_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        vehicle_id.as_str(),
                        i64::try_from(delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        delta.parent_chain_digest.to_string(),
                        delta.chain_digest.to_string(),
                        delta.pack_digest.to_string(),
                        pack_json,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }

        for pack in &packs {
            Self::register_lineage_pack_snapshot(
                &transaction,
                &vehicle_id,
                pack,
                lineage
                    .deltas
                    .iter()
                    .find(|delta| delta.pack.sha256 == pack.sha256)
                    .map_or(lineage.base.sequence, |delta| delta.to_sequence),
                &serde_json::to_vec(lineage).map_err(StoreError::SerializeManifest)?,
            )?;
            let existing_pack: Option<(String, i64, String, i64, i64)> = transaction
                .query_row(
                    "SELECT snapshot_id, ordinal, relative_path,
                            compressed_bytes, uncompressed_bytes
                     FROM sync_packs WHERE sha256 = ?1",
                    params![pack.sha256.to_string()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if let Some((
                snapshot_id,
                ordinal,
                relative_path,
                compressed_bytes,
                uncompressed_bytes,
            )) = existing_pack
            {
                if snapshot_id != pack.snapshot_id.to_string()
                    || ordinal != i64::from(pack.ordinal)
                    || relative_path != pack.relative_path
                    || compressed_bytes
                        != i64::try_from(pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?
                    || uncompressed_bytes
                        != i64::try_from(pack.uncompressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?
                {
                    return Err(StoreError::LineageCatalogConflict);
                }
                continue;
            }
            let occupied: Option<String> = transaction
                .query_row(
                    "SELECT sha256 FROM sync_packs
                     WHERE snapshot_id = ?1 AND ordinal = ?2",
                    params![pack.snapshot_id.to_string(), i64::from(pack.ordinal)],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if occupied.is_some() {
                return Err(StoreError::LineageCatalogConflict);
            }
            transaction
                .execute(
                    "INSERT INTO sync_packs(
                        sha256, snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        pack.sha256.to_string(),
                        pack.snapshot_id.to_string(),
                        i64::from(pack.ordinal),
                        pack.relative_path,
                        i64::try_from(pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                        i64::try_from(pack.uncompressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }

        let existing_head: Option<(i64, String)> = transaction
            .query_row(
                "SELECT head_sequence, head_digest FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some((sequence, digest)) = existing_head {
            let sequence =
                u64::try_from(sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
            if sequence > lineage.head_sequence
                || (sequence == lineage.head_sequence && digest != lineage.head_digest.to_string())
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            if sequence < lineage.head_sequence {
                transaction
                    .execute(
                        "UPDATE sync_heads
                         SET head_sequence = ?1, head_digest = ?2, terminal_cursor = ?3
                         WHERE vehicle_id = ?4 AND head_sequence = ?5 AND head_digest = ?6",
                        params![
                            i64::try_from(lineage.head_sequence)
                                .map_err(|_| StoreError::SequenceTooLarge)?,
                            lineage.head_digest.to_string(),
                            cursor,
                            vehicle_id.as_str(),
                            i64::try_from(sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                            digest,
                        ],
                    )
                    .map_err(StoreError::LineageCatalog)?;
            }
        } else {
            transaction
                .execute(
                    "INSERT INTO sync_heads
                     (vehicle_id, base_snapshot_id, head_sequence, head_digest, terminal_cursor)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        vehicle_id.as_str(),
                        lineage.base.snapshot_id.to_string(),
                        i64::try_from(lineage.head_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        lineage.head_digest.to_string(),
                        cursor,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }
        transaction.commit().map_err(StoreError::LineageCatalog)
    }

    fn register_lineage_pack_snapshot(
        transaction: &Transaction<'_>,
        vehicle_id: &str,
        pack: &TransportPack,
        head_sequence: u64,
        manifest_json: &[u8],
    ) -> Result<(), StoreError> {
        let snapshot_id = pack.snapshot_id.to_string();
        let existing: Option<String> = transaction
            .query_row(
                "SELECT vehicle_id FROM sync_manifests WHERE snapshot_id = ?1",
                params![snapshot_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some(existing_vehicle_id) = existing {
            if existing_vehicle_id != vehicle_id {
                return Err(StoreError::LineageCatalogConflict);
            }
            return Ok(());
        }
        transaction
            .execute(
                "INSERT INTO sync_manifests
                 (snapshot_id, vehicle_id, head_sequence, manifest_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    snapshot_id,
                    vehicle_id,
                    i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    manifest_json,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        Ok(())
    }

    fn inspect_legacy_v2_base_car_pack(
        &self,
        pack: &TransportPack,
        base: &LegacyV2BaseDescription,
    ) -> Result<i64, StoreError> {
        if pack.ordinal != 0
            || pack.snapshot_id != base.snapshot_id
            || pack.schema != HUB_PROJECTION_SCHEMA_V2
            || pack.format != crate::protocol::PackFormat::HubProjectionSqlite
            || pack.sha256 != base.base_digest
            || pack.sequence
                != (SequenceRange {
                    from_exclusive: base.base_sequence,
                    to_inclusive: base.base_sequence,
                })
            || !pack.tables.contains(&crate::protocol::MirrorTable::Car)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        pack.validate(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;

        let path = self
            .packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        let mut file = File::open(&path).map_err(StoreError::OpenLineagePack)?;
        let metadata = file.metadata().map_err(StoreError::OpenLineagePack)?;
        if !metadata.is_file() || metadata.len() != pack.compressed_bytes {
            return Err(StoreError::LineagePackNotReady);
        }
        pack.verify_reader(&mut file, ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        file.seek(SeekFrom::Start(0))
            .map_err(StoreError::OpenLineagePack)?;
        let decoder =
            zstd::stream::read::Decoder::new(file).map_err(StoreError::DecodeLineagePack)?;
        let maximum = pack
            .uncompressed_bytes
            .checked_add(1)
            .ok_or(StoreError::LineageCatalogConflict)?;
        let inspection = LineagePackInspection {
            path: self.packs_dir.join(format!(
                ".legacy-binding-inspection-{}.sqlite",
                Uuid::new_v4()
            )),
        };
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&inspection.path)
            .map_err(|source| StoreError::CreateLineagePackInspection {
                path: inspection.path.clone(),
                source,
            })?;
        let decoded = std::io::copy(&mut decoder.take(maximum), &mut output)
            .map_err(StoreError::DecodeLineagePack)?;
        if decoded != pack.uncompressed_bytes {
            return Err(StoreError::LineageCatalogConflict);
        }
        output
            .sync_all()
            .map_err(StoreError::SyncLineagePackInspection)?;
        drop(output);

        let connection = Connection::open_with_flags(
            &inspection.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::LineageCatalog)?;
        connection
            .execute_batch("PRAGMA trusted_schema = OFF;")
            .map_err(StoreError::LineageCatalog)?;
        let application_id: i64 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?;
        let user_version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?;
        let quick_check: Vec<String> = connection
            .prepare("PRAGMA quick_check")
            .map_err(StoreError::LineageCatalog)?
            .query_map([], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<_, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if application_id != i64::from(SQLITE_HUB_PROJECTION_APPLICATION_ID)
            || user_version != i64::from(HUB_PROJECTION_SCHEMA_V2.sqlite_user_version())
            || quick_check.as_slice() != ["ok"]
        {
            return Err(StoreError::LineageCatalogConflict);
        }

        let pack_metadata = {
            let mut statement = connection
                .prepare("SELECT key, value FROM hub_pack_metadata")
                .map_err(StoreError::LineageCatalog)?;
            let mut rows = statement.query([]).map_err(StoreError::LineageCatalog)?;
            let mut values = HashMap::new();
            while let Some(row) = rows.next().map_err(StoreError::LineageCatalog)? {
                let key: String = row.get(0).map_err(StoreError::LineageCatalog)?;
                let value: String = row.get(1).map_err(StoreError::LineageCatalog)?;
                if values.insert(key, value).is_some() {
                    return Err(StoreError::LineageCatalogConflict);
                }
            }
            values
        };
        if pack_metadata.len() != FULL_SNAPSHOT_METADATA_KEYS.len()
            || FULL_SNAPSHOT_METADATA_KEYS
                .iter()
                .any(|key| !pack_metadata.contains_key(*key))
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let selected_car_id = pack_metadata
            .get("selected_car_id")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .ok_or(StoreError::LineageCatalogConflict)?;
        let expected = [
            ("protocol", "teslatlas-sync".to_owned()),
            ("pack_format", "hub_projection_sqlite".to_owned()),
            ("schema_major", HUB_PROJECTION_SCHEMA_V2.major.to_string()),
            ("schema_minor", HUB_PROJECTION_SCHEMA_V2.minor.to_string()),
            ("pack_id", pack.pack_id.to_string()),
            ("snapshot_id", base.snapshot_id.to_string()),
            ("ordinal", pack.ordinal.to_string()),
            ("mode", "full_snapshot".to_owned()),
            ("installation_id", base.installation_id.to_string()),
            ("account_id", base.account_id.to_string()),
            ("vehicle_id", base.vehicle_id.to_string()),
            ("generation", base.generation.to_string()),
            ("selected_car_id", selected_car_id.to_string()),
            ("base_sequence", base.base_sequence.to_string()),
            ("head_sequence", base.base_sequence.to_string()),
            ("row_count", pack.row_count.to_string()),
        ];
        if expected
            .iter()
            .any(|(key, expected)| pack_metadata.get(*key) != Some(expected))
        {
            return Err(StoreError::LineageCatalogConflict);
        }

        let car_ids = connection
            .prepare("SELECT id FROM cars ORDER BY id")
            .map_err(StoreError::LineageCatalog)?
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if car_ids.as_slice() != [selected_car_id] {
            return Err(StoreError::LineageCatalogConflict);
        }
        for (table, column) in [
            ("car_settings", "car_id"),
            ("drives", "car_id"),
            ("charges", "car_id"),
            ("positions", "car_id"),
            ("states", "car_id"),
            ("updates", "car_id"),
        ] {
            let out_of_scope: bool = connection
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column} != ?1)"),
                    params![selected_car_id],
                    |row| row.get(0),
                )
                .map_err(StoreError::LineageCatalog)?;
            if out_of_scope {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(selected_car_id)
    }

    fn verify_lineage_pack_for_mode(
        &self,
        pack: &TransportPack,
        verification: LineagePackVerification,
    ) -> Result<(), StoreError> {
        self.verify_lineage_pack_metadata(pack)?;
        if verification == LineagePackVerification::MetadataOnly {
            return Ok(());
        }
        let path = self
            .packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        if sha256_file_hex(&path)? != pack.sha256.to_string() {
            return Err(StoreError::LineagePackDigestMismatch);
        }
        Ok(())
    }

    fn verify_lineage_pack(&self, pack: &TransportPack) -> Result<(), StoreError> {
        self.verify_lineage_pack_for_mode(pack, LineagePackVerification::FullDigest)
    }

    fn verify_lineage_pack_metadata(&self, pack: &TransportPack) -> Result<(), StoreError> {
        if pack.relative_path != TransportPack::canonical_relative_path(pack.sha256) {
            return Err(StoreError::LineagePackNotReady);
        }
        let path = self
            .packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        let metadata = fs::symlink_metadata(&path).map_err(|_| StoreError::LineagePackNotReady)?;
        if !metadata.file_type().is_file() || metadata.len() != pack.compressed_bytes {
            return Err(StoreError::LineagePackNotReady);
        }
        Ok(())
    }

    fn verify_typed_delta_schema(connection: &Connection) -> Result<(), StoreError> {
        let objects = connection
            .prepare(
                "SELECT type, name, tbl_name, sql FROM sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            )
            .map_err(StoreError::LineageCatalog)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if objects.len() != TYPED_DELTA_TABLES.len()
            || objects.iter().any(|(kind, name, table, sql)| {
                kind != "table"
                    || table != name
                    || !TYPED_DELTA_TABLES.contains(&name.as_str())
                    || sql.is_none()
            })
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut contract = Sha256::new();
        for (kind, name, table, sql) in &objects {
            for part in [kind.as_bytes(), name.as_bytes(), table.as_bytes()] {
                contract.update(part);
                contract.update([0]);
            }
            contract.update(sql.as_deref().expect("checked schema SQL").as_bytes());
            contract.update([0, b'\n']);
        }
        let contract = hex::encode(contract.finalize());
        if contract != TYPED_DELTA_SCHEMA_CONTRACT_SHA256 {
            return Err(StoreError::LineageCatalogConflict);
        }

        let unexpected_internal: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                     WHERE name LIKE 'sqlite_%'
                       AND name NOT IN (
                           'sqlite_stat1', 'sqlite_stat4',
                           'sqlite_autoindex_hub_pack_metadata_1'
                       )
                )",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if unexpected_internal {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(())
    }

    fn verify_typed_delta_real_values(
        connection: &Connection,
        table: &str,
        column: &str,
        nonnegative: bool,
    ) -> Result<(), StoreError> {
        let mut statement = connection
            .prepare(&format!(
                "SELECT {column} FROM {table} WHERE {column} IS NOT NULL"
            ))
            .map_err(StoreError::LineageCatalog)?;
        let values = statement
            .query_map([], |row| row.get::<_, f64>(0))
            .map_err(StoreError::LineageCatalog)?;
        for value in values {
            let value = value.map_err(StoreError::LineageCatalog)?;
            if !value.is_finite() || (nonnegative && value < 0.0) {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(())
    }

    fn verify_typed_delta_soc_values(
        connection: &Connection,
        table: &str,
        column: &str,
    ) -> Result<(), StoreError> {
        let mut statement = connection
            .prepare(&format!(
                "SELECT {column} FROM {table} WHERE {column} IS NOT NULL"
            ))
            .map_err(StoreError::LineageCatalog)?;
        let values = statement
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(StoreError::LineageCatalog)?;
        for value in values {
            if !(0..=100).contains(&value.map_err(StoreError::LineageCatalog)?) {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(())
    }

    fn verify_typed_delta_text_values(
        connection: &Connection,
        table: &str,
        column: &str,
        required: bool,
    ) -> Result<(), StoreError> {
        let mut statement = connection
            .prepare(&format!("SELECT {column} FROM {table}"))
            .map_err(StoreError::LineageCatalog)?;
        let values = statement
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .map_err(StoreError::LineageCatalog)?;
        for value in values {
            let value = value.map_err(StoreError::LineageCatalog)?;
            let Some(value) = value else {
                if required {
                    return Err(StoreError::LineageCatalogConflict);
                }
                continue;
            };
            if value.len() > MAX_TEXT_BYTES
                || value.as_bytes().contains(&0)
                || (required && value.is_empty())
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(())
    }

    fn verify_typed_delta_coordinate_pairs(
        connection: &Connection,
        table: &str,
        latitude_column: &str,
        longitude_column: &str,
    ) -> Result<(), StoreError> {
        let mut statement = connection
            .prepare(&format!(
                "SELECT {latitude_column}, {longitude_column} FROM {table}"
            ))
            .map_err(StoreError::LineageCatalog)?;
        let coordinates = statement
            .query_map([], |row| {
                Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?))
            })
            .map_err(StoreError::LineageCatalog)?;
        for coordinate in coordinates {
            match coordinate.map_err(StoreError::LineageCatalog)? {
                (None, None) => {}
                (Some(latitude), Some(longitude))
                    if latitude.is_finite()
                        && longitude.is_finite()
                        && (-90.0..=90.0).contains(&latitude)
                        && (-180.0..=180.0).contains(&longitude)
                        && (latitude != 0.0 || longitude != 0.0) => {}
                _ => return Err(StoreError::LineageCatalogConflict),
            }
        }
        Ok(())
    }

    fn verify_typed_delta_row_semantics(
        connection: &Connection,
        selected_car_id: i64,
    ) -> Result<(), StoreError> {
        let malformed: bool = connection
            .query_row(
                "SELECT
                    EXISTS(SELECT 1 FROM cars
                           WHERE id <= 0 OR length(name) = 0 OR length(model) = 0)
                    OR EXISTS(SELECT 1 FROM cars AS car
                                LEFT JOIN car_settings AS settings
                                  ON settings.car_id = car.id
                                WHERE settings.car_id IS NULL)
                    OR EXISTS(SELECT 1 FROM car_settings
                                WHERE car_id <= 0
                                   OR enabled NOT IN (0, 1)
                                   OR use_streaming_api NOT IN (0, 1)
                                   OR suspend_after_idle_min <= 0
                                   OR suspend_min <= 0
                                   OR suspend_min_resolved NOT IN (0, 1)
                                   OR req_not_unlocked NOT IN (0, 1)
                                   OR free_supercharging NOT IN (0, 1)
                                   OR lfp_battery NOT IN (0, 1))
                    OR EXISTS(SELECT 1 FROM drives
                                WHERE id <= 0 OR start_date_ms <= 0
                                   OR end_date_ms < start_date_ms
                                   OR distance_km < 0)
                    OR EXISTS(SELECT 1 FROM charges
                                WHERE id <= 0 OR start_date_ms <= 0
                                   OR end_date_ms < start_date_ms
                                   OR charge_energy_added < 0)
                    OR EXISTS(SELECT 1 FROM positions
                                WHERE id <= 0 OR date_ms <= 0
                                   OR drive_id <= 0
                                   OR latitude NOT BETWEEN -90.0 AND 90.0
                                   OR longitude NOT BETWEEN -180.0 AND 180.0
                                   OR (latitude = 0.0 AND longitude = 0.0)
                                   OR odometer < 0)
                    OR EXISTS(SELECT 1 FROM charge_samples
                                WHERE id <= 0 OR charge_process_id <= 0
                                   OR timestamp_ms <= 0)
                    OR EXISTS(SELECT 1 FROM states
                                WHERE id <= 0 OR start_date_ms <= 0
                                   OR end_date_ms < start_date_ms
                                   OR state NOT IN ('online', 'offline', 'asleep'))
                    OR EXISTS(SELECT 1 FROM updates
                                WHERE id <= 0 OR start_date_ms <= 0
                                   OR end_date_ms < start_date_ms OR length(version) = 0)
                    OR EXISTS(SELECT 1 FROM tombstones
                                WHERE entity NOT IN (
                                    'drive', 'position', 'charge', 'charge_sample',
                                    'state', 'update'
                                )
                                   OR entity_id <= 0 OR car_id <= 0)",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if malformed {
            return Err(StoreError::LineageCatalogConflict);
        }
        let conflicting_tombstone: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM tombstones AS tombstone
                     WHERE (tombstone.entity = 'car'
                            AND tombstone.entity_id IN (SELECT id FROM cars))
                        OR (tombstone.entity = 'car_setting'
                            AND tombstone.entity_id IN (SELECT car_id FROM car_settings))
                        OR (tombstone.entity = 'drive'
                            AND tombstone.entity_id IN (SELECT id FROM drives))
                        OR (tombstone.entity = 'position'
                            AND tombstone.entity_id IN (SELECT id FROM positions))
                        OR (tombstone.entity = 'charge'
                            AND tombstone.entity_id IN (SELECT id FROM charges))
                        OR (tombstone.entity = 'charge_sample'
                            AND tombstone.entity_id IN (SELECT id FROM charge_samples))
                        OR (tombstone.entity = 'state'
                            AND tombstone.entity_id IN (SELECT id FROM states))
                        OR (tombstone.entity = 'update'
                            AND tombstone.entity_id IN (SELECT id FROM updates))
                )",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if conflicting_tombstone {
            return Err(StoreError::LineageCatalogConflict);
        }
        for (table, column, nonnegative) in [
            ("cars", "efficiency_wh_per_km", true),
            ("drives", "distance_km", true),
            ("drives", "efficiency", false),
            ("drives", "power_max", false),
            ("drives", "power_min", false),
            ("charges", "charge_energy_added", true),
            ("charges", "cost", false),
            ("positions", "power", false),
            ("positions", "odometer", true),
            ("positions", "ideal_battery_range_km", true),
            ("charge_samples", "charge_energy_added_kwh", true),
        ] {
            Self::verify_typed_delta_real_values(connection, table, column, nonnegative)?;
        }
        for (table, latitude, longitude) in [
            ("drives", "start_latitude", "start_longitude"),
            ("drives", "end_latitude", "end_longitude"),
            ("charges", "start_latitude", "start_longitude"),
            ("positions", "latitude", "longitude"),
        ] {
            Self::verify_typed_delta_coordinate_pairs(connection, table, latitude, longitude)?;
        }
        for (table, column) in [
            ("drives", "start_soc"),
            ("drives", "end_soc"),
            ("charges", "start_battery_level"),
            ("charges", "end_battery_level"),
            ("positions", "battery_level"),
            ("positions", "usable_battery_level"),
            ("charge_samples", "battery_level"),
            ("charge_samples", "usable_battery_level"),
        ] {
            Self::verify_typed_delta_soc_values(connection, table, column)?;
        }
        for (table, column, required) in [
            ("cars", "name", true),
            ("cars", "model", true),
            ("cars", "vin", false),
            ("cars", "firmware_version", false),
            ("drives", "start_address", false),
            ("drives", "end_address", false),
            ("drives", "start_geofence", false),
            ("drives", "end_geofence", false),
            ("charges", "address", false),
            ("charges", "location_name", false),
            ("charges", "geofence", false),
            ("updates", "version", true),
        ] {
            Self::verify_typed_delta_text_values(connection, table, column, required)?;
        }
        let multiple_open_states: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT car_id FROM states
                     WHERE end_date_ms IS NULL
                     GROUP BY car_id
                    HAVING COUNT(*) > 1
                )",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if multiple_open_states {
            return Err(StoreError::LineageCatalogConflict);
        }
        for (table, column) in [
            ("cars", "id"),
            ("car_settings", "car_id"),
            ("drives", "car_id"),
            ("charges", "car_id"),
            ("positions", "car_id"),
            ("states", "car_id"),
            ("updates", "car_id"),
            ("tombstones", "car_id"),
        ] {
            let out_of_scope: bool = connection
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column} != ?1)"),
                    params![selected_car_id],
                    |row| row.get(0),
                )
                .map_err(StoreError::LineageCatalog)?;
            if out_of_scope {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(())
    }

    /// Confirm that a changed-history successor is the sparse, externally
    /// based delta produced by [`ProjectionPackWriter::write_delta`]. The
    /// transport manifest is caller input, so its fields cannot stand in for
    /// the immutable SQLite object's own metadata.
    fn verify_import_delta_pack(
        &self,
        delta: &LineageDelta,
        binding: &ProjectionBinding,
    ) -> Result<(), StoreError> {
        let path = self
            .packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", delta.pack.sha256));
        // Verify and decode the exact same opened descriptor. Reopening the
        // content-addressed path after verification would leave a same-user
        // replacement window between the digest check and SQLite inspection.
        let mut file = File::open(&path).map_err(StoreError::OpenLineagePack)?;
        delta
            .pack
            .verify_reader(&mut file, ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        file.seek(SeekFrom::Start(0))
            .map_err(StoreError::OpenLineagePack)?;
        let decoder =
            zstd::stream::read::Decoder::new(file).map_err(StoreError::DecodeLineagePack)?;
        let maximum = delta
            .pack
            .uncompressed_bytes
            .checked_add(1)
            .ok_or(StoreError::LineageCatalogConflict)?;
        let inspection = LineagePackInspection {
            path: self
                .packs_dir
                .join(format!(".lineage-inspection-{}.sqlite", Uuid::new_v4())),
        };
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&inspection.path)
            .map_err(|source| StoreError::CreateLineagePackInspection {
                path: inspection.path.clone(),
                source,
            })?;
        let decoded = std::io::copy(&mut decoder.take(maximum), &mut output)
            .map_err(StoreError::DecodeLineagePack)?;
        if decoded != delta.pack.uncompressed_bytes {
            return Err(StoreError::LineageCatalogConflict);
        }
        output
            .sync_all()
            .map_err(StoreError::SyncLineagePackInspection)?;
        drop(output);

        let connection = Connection::open_with_flags(
            &inspection.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::LineageCatalog)?;
        connection
            .execute_batch("PRAGMA trusted_schema = OFF;")
            .map_err(StoreError::LineageCatalog)?;
        let application_id: i64 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?;
        let user_version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?;
        if application_id != i64::from(SQLITE_HUB_PROJECTION_APPLICATION_ID)
            || user_version != i64::from(HUB_PROJECTION_SCHEMA_V2.sqlite_user_version())
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let quick_check: Vec<String> = connection
            .prepare("PRAGMA quick_check")
            .map_err(StoreError::LineageCatalog)?
            .query_map([], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<_, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if quick_check.as_slice() != ["ok"] {
            return Err(StoreError::LineageCatalogConflict);
        }
        Self::verify_typed_delta_schema(&connection)?;

        let metadata = {
            let mut statement = connection
                .prepare("SELECT key, value FROM hub_pack_metadata")
                .map_err(StoreError::LineageCatalog)?;
            let mut rows = statement.query([]).map_err(StoreError::LineageCatalog)?;
            let mut metadata = HashMap::new();
            while let Some(row) = rows.next().map_err(StoreError::LineageCatalog)? {
                let key: String = row.get(0).map_err(StoreError::LineageCatalog)?;
                let value: String = row.get(1).map_err(StoreError::LineageCatalog)?;
                if metadata.insert(key, value).is_some() {
                    return Err(StoreError::LineageCatalogConflict);
                }
            }
            metadata
        };
        if metadata.len() != TYPED_DELTA_METADATA_KEYS.len()
            || TYPED_DELTA_METADATA_KEYS
                .iter()
                .any(|key| !metadata.contains_key(*key))
            || metadata
                .iter()
                .any(|(key, value)| key.len() > 64 || value.len() > 16 * 1024)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let expected_metadata = [
            ("protocol", "teslatlas-sync".to_owned()),
            ("pack_format", "hub_projection_sqlite".to_owned()),
            ("schema_major", HUB_PROJECTION_SCHEMA_V2.major.to_string()),
            ("schema_minor", HUB_PROJECTION_SCHEMA_V2.minor.to_string()),
            ("delta_schema_version", "1".to_owned()),
            ("pack_id", delta.pack.pack_id.to_string()),
            ("snapshot_id", delta.pack.snapshot_id.to_string()),
            ("ordinal", delta.pack.ordinal.to_string()),
            ("mode", "typed_delta".to_owned()),
            ("installation_id", binding.installation_id.to_string()),
            ("account_id", binding.account_id.to_string()),
            ("vehicle_id", binding.vehicle_id.to_string()),
            ("generation", binding.generation.to_string()),
            ("from_sequence", delta.from_sequence.to_string()),
            ("to_sequence", delta.to_sequence.to_string()),
            ("parent_digest", delta.parent_chain_digest.to_string()),
            ("external_base", "true".to_owned()),
        ];
        if expected_metadata
            .iter()
            .any(|(key, expected)| metadata.get(*key) != Some(expected))
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let selected_car_id = metadata
            .get("selected_car_id")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .ok_or(StoreError::LineageCatalogConflict)?;
        if selected_car_id != binding.selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }

        let table_count = |table: &str| -> Result<u64, StoreError> {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(StoreError::LineageCatalog)
                .and_then(|count| {
                    u64::try_from(count).map_err(|_| StoreError::LineageCatalogConflict)
                })
        };
        let cars = table_count("cars")?;
        let car_settings = table_count("car_settings")?;
        let drives = table_count("drives")?;
        let charges = table_count("charges")?;
        let positions = table_count("positions")?;
        let charge_samples = table_count("charge_samples")?;
        let states = table_count("states")?;
        let updates = table_count("updates")?;
        let tombstones = table_count("tombstones")?;
        // Every `ProjectionCar` materialises its embedded settings in the
        // companion table, so `car_settings` is the writer's logical count
        // for both car rows and explicit settings-only patches.
        let row_count = [
            car_settings,
            drives,
            charges,
            positions,
            charge_samples,
            states,
            updates,
            tombstones,
        ]
        .into_iter()
        .try_fold(0_u64, |total, count| {
            total
                .checked_add(count)
                .ok_or(StoreError::LineageCatalogConflict)
        })?;
        if row_count == 0
            || row_count != delta.pack.row_count
            || metadata.get("row_count") != Some(&row_count.to_string())
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        Self::verify_typed_delta_row_semantics(&connection, selected_car_id)?;
        let mut populated = HashSet::new();
        if cars != 0 || car_settings != 0 {
            populated.insert(crate::protocol::MirrorTable::Car);
        }
        for (count, table) in [
            (drives, crate::protocol::MirrorTable::Drive),
            (charges, crate::protocol::MirrorTable::Charge),
            (positions, crate::protocol::MirrorTable::Position),
            (charge_samples, crate::protocol::MirrorTable::ChargeSample),
            (states, crate::protocol::MirrorTable::State),
            (updates, crate::protocol::MirrorTable::Update),
            (tombstones, crate::protocol::MirrorTable::Tombstone),
        ] {
            if count != 0 {
                populated.insert(table);
            }
        }
        if populated != delta.pack.tables.iter().copied().collect() {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(())
    }

    pub fn pack_for_digest(&self, digest: Sha256Digest) -> Result<Option<StoredPack>, StoreError> {
        let connection = self.open_read_only_connection()?;
        if let Some(pack) = self.active_pack_for_digest(&connection, digest)? {
            return Ok(Some(pack));
        }
        self.retired_pack_for_digest_at(&connection, digest, retired_lineage_clock_ms()?)
    }

    fn active_pack_for_digest(
        &self,
        connection: &Connection,
        digest: Sha256Digest,
    ) -> Result<Option<StoredPack>, StoreError> {
        let entry = connection
            .query_row(
                "SELECT manifests.manifest_json, packs.relative_path, packs.compressed_bytes
                   FROM sync_packs AS packs
                   JOIN sync_manifests AS manifests
                     ON manifests.snapshot_id = packs.snapshot_id
                  WHERE packs.sha256 = ?1",
                params![digest.to_string()],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((manifest, relative_path, compressed_bytes)) = entry else {
            return Ok(None);
        };
        validate_catalogued_pack_manifest(&manifest)?;
        self.stored_pack_from_catalogue(digest, &relative_path, compressed_bytes)
            .map(Some)
    }

    fn retired_pack_for_digest_at(
        &self,
        connection: &Connection,
        digest: Sha256Digest,
        now_ms: i64,
    ) -> Result<Option<StoredPack>, StoreError> {
        if now_ms < 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let row: Option<(String, String, Vec<u8>, String, i64)> = connection
            .query_row(
                "SELECT lineage.vehicle_id, lineage.head_digest,
                        lineage.manifest_json, packs.relative_path,
                        packs.compressed_bytes
                 FROM sync_retired_lineage_packs AS packs
                 JOIN sync_retired_lineages AS lineage
                   ON lineage.vehicle_id = packs.vehicle_id
                  AND lineage.head_digest = packs.head_digest
                 WHERE packs.pack_digest = ?1 AND lineage.expires_at_ms > ?2
                 ORDER BY lineage.expires_at_ms DESC,
                          lineage.vehicle_id, lineage.head_digest
                 LIMIT 1",
                params![digest.to_string(), now_ms],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((vehicle_id, head_digest, manifest_json, relative_path, compressed_bytes)) = row
        else {
            return Ok(None);
        };
        validate_retired_lineage_pack_binding(
            &vehicle_id,
            &head_digest,
            &manifest_json,
            &digest.to_string(),
            &relative_path,
            compressed_bytes,
        )?;
        self.stored_pack_from_catalogue(digest, &relative_path, compressed_bytes)
            .map(Some)
    }

    fn stored_pack_from_catalogue(
        &self,
        digest: Sha256Digest,
        relative_path: &str,
        compressed_bytes: i64,
    ) -> Result<StoredPack, StoreError> {
        let compressed_bytes =
            u64::try_from(compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
        if relative_path != TransportPack::canonical_relative_path(digest) {
            return Err(StoreError::UnsafeStoredPackPath);
        }
        Ok(StoredPack {
            digest,
            compressed_bytes,
            path: self
                .packs_dir
                .join("sha256")
                .join(format!("{digest}.sqlite.zst")),
        })
    }

    pub fn lineage_manifest_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<LineageManifestV2>, StoreError> {
        self.lineage_manifest_for_vehicle_with_verification(
            vehicle_id,
            LineagePackVerification::FullDigest,
        )
    }

    fn lineage_manifest_for_vehicle_with_verification(
        &self,
        vehicle_id: Uuid,
        verification: LineagePackVerification,
    ) -> Result<Option<LineageManifestV2>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let base_row: Option<(String, i64, String, Vec<u8>)> = connection
            .query_row(
                "SELECT snapshot_id, base_sequence, base_digest, packs_json
                 FROM sync_bases WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((snapshot_id, base_sequence, base_digest, packs_json)) = base_row else {
            return Ok(None);
        };
        let base_sequence =
            u64::try_from(base_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        let base_snapshot_id = Uuid::parse_str(&snapshot_id)
            .map_err(|_| StoreError::InvalidStoredUuid("lineage base snapshot"))?;
        let base_digest = base_digest
            .parse::<Sha256Digest>()
            .map_err(|_| StoreError::LineageCatalogConflict)?;
        let base_packs: Vec<TransportPack> =
            serde_json::from_slice(&packs_json).map_err(StoreError::DeserializeManifest)?;
        if base_packs.is_empty() {
            return Err(StoreError::LineageCatalogConflict);
        }
        for pack in &base_packs {
            self.verify_lineage_pack_for_mode(pack, verification)?;
        }

        let mut deltas = Vec::new();
        let mut statement = connection
            .prepare(
                "SELECT from_sequence, to_sequence, parent_chain_digest,
                        chain_digest, pack_digest, pack_json
                 FROM sync_deltas WHERE vehicle_id = ?1
                 ORDER BY from_sequence, to_sequence",
            )
            .map_err(StoreError::LineageCatalog)?;
        let rows = statement
            .query_map(params![vehicle_id.to_string()], |row| {
                let from_sequence: i64 = row.get(0)?;
                let to_sequence: i64 = row.get(1)?;
                let parent_chain_digest: String = row.get(2)?;
                let chain_digest: String = row.get(3)?;
                let pack_digest: String = row.get(4)?;
                let pack_json: Vec<u8> = row.get(5)?;
                Ok((
                    from_sequence,
                    to_sequence,
                    parent_chain_digest,
                    chain_digest,
                    pack_digest,
                    pack_json,
                ))
            })
            .map_err(StoreError::LineageCatalog)?;
        for row in rows {
            let (from_sequence, to_sequence, parent_chain_digest, chain_digest, pack_digest, json) =
                row.map_err(StoreError::LineageCatalog)?;
            let delta: LineageDelta =
                serde_json::from_slice(&json).map_err(StoreError::DeserializeManifest)?;
            if delta.from_sequence != u64::try_from(from_sequence).unwrap_or(u64::MAX)
                || delta.to_sequence != u64::try_from(to_sequence).unwrap_or(u64::MAX)
                || delta.parent_chain_digest.to_string() != parent_chain_digest
                || delta.chain_digest.to_string() != chain_digest
                || delta.pack_digest.to_string() != pack_digest
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            self.verify_lineage_pack_for_mode(&delta.pack, verification)?;
            deltas.push(delta);
        }
        drop(statement);

        let (head_base_snapshot, head_sequence, head_digest, terminal_cursor): (
            String,
            i64,
            String,
            String,
        ) = connection
            .query_row(
                "SELECT base_snapshot_id, head_sequence, head_digest, terminal_cursor
                     FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        if head_base_snapshot != snapshot_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        let terminal_cursor: OpaqueCursor =
            serde_json::from_str(&terminal_cursor).map_err(StoreError::DeserializeManifest)?;
        let binding = self.v2_projection_binding(vehicle_id)?;
        let lineage = LineageManifestV2 {
            protocol: LINEAGE_PROTOCOL_V2,
            capability: LineageCapability::ImmutableBaseOrderedDeltas,
            schema: base_packs[0].schema,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id,
            generation: binding.generation,
            base: LineageBase {
                snapshot_id: base_snapshot_id,
                sequence: base_sequence,
                digest: base_digest,
                packs: base_packs,
            },
            deltas,
            head_sequence: u64::try_from(head_sequence)
                .map_err(|_| StoreError::InvalidStoredSequence)?,
            head_digest: head_digest
                .parse::<Sha256Digest>()
                .map_err(|_| StoreError::LineageCatalogConflict)?,
            terminal_cursor,
        };
        lineage.validate().map_err(StoreError::Manifest)?;
        Ok(Some(lineage))
    }

    /// Prepare one single-use pairing challenge without writing it. The CLI
    /// uses this phase to finish its QR/JSON presentation before activation.
    pub fn prepare_pairing(
        &self,
        label: &str,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<PairingInvitation, StoreError> {
        validate_identity("pairing label", label, MAX_PAIRING_LABEL_BYTES)?;
        validate_timestamp("pairing created_at_ms", created_at_ms)?;
        if expires_at_ms <= created_at_ms {
            return Err(StoreError::InvalidPairingExpiry);
        }

        let pairing_id = Uuid::new_v4();
        let secret = PairingSecret::generate();
        Ok(PairingInvitation {
            pairing_id,
            secret,
            created_at_ms,
            expires_at_ms,
        })
    }

    /// Persist a fully prepared invitation immediately before its local
    /// presentation. Only the secret digest crosses this boundary.
    pub fn persist_pairing(
        &self,
        label: &str,
        invitation: &PairingInvitation,
    ) -> Result<(), StoreError> {
        validate_identity("pairing label", label, MAX_PAIRING_LABEL_BYTES)?;
        validate_timestamp("pairing created_at_ms", invitation.created_at_ms)?;
        validate_timestamp("pairing expires_at_ms", invitation.expires_at_ms)?;
        if invitation.expires_at_ms <= invitation.created_at_ms {
            return Err(StoreError::InvalidPairingExpiry);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "INSERT INTO pairing_challenges \
                 (pairing_id, label, secret_sha256, created_at_ms, expires_at_ms) \
                VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    invitation.pairing_id.to_string(),
                    label,
                    invitation.secret.digest().as_slice(),
                    invitation.created_at_ms,
                    invitation.expires_at_ms,
                ],
            )
            .map_err(StoreError::CreatePairing)?;
        transaction.commit().map_err(StoreError::CreatePairing)?;
        Ok(())
    }

    /// Create and immediately persist an invitation for non-interactive
    /// callers that do not have a presentation boundary.
    pub fn create_pairing(
        &self,
        label: &str,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<PairingInvitation, StoreError> {
        let invitation = self.prepare_pairing(label, created_at_ms, expires_at_ms)?;
        self.persist_pairing(label, &invitation)?;
        Ok(invitation)
    }

    /// Revoke one invitation. Deleting a missing row is deliberately success,
    /// making cleanup safe to retry after an uncertain terminal write.
    pub fn revoke_pairing(&self, pairing_id: Uuid) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "DELETE FROM pairing_challenges WHERE pairing_id = ?1",
                params![pairing_id.to_string()],
            )
            .map_err(StoreError::RevokePairing)?;
        transaction.commit().map_err(StoreError::RevokePairing)?;
        Ok(())
    }

    /// Consume one valid pairing challenge and return the device bearer token.
    /// A failed or expired claim deliberately has one opaque outcome; callers
    /// cannot learn whether a challenge existed, expired, or had a bad secret.
    pub fn claim_pairing(
        &self,
        pairing_id: Uuid,
        secret: &str,
        device_name: &str,
        claimed_at_ms: i64,
    ) -> Result<PairedDeviceAccess, StoreError> {
        validate_identity("paired device name", device_name, MAX_DEVICE_NAME_BYTES)?;
        validate_timestamp("pairing claimed_at_ms", claimed_at_ms)?;
        let Some(secret_digest) = PairingSecret::digest_from_wire(secret) else {
            return Err(StoreError::PairingRejected);
        };

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let challenge: Option<(Vec<u8>, i64)> = transaction
            .query_row(
                "SELECT secret_sha256, expires_at_ms FROM pairing_challenges WHERE pairing_id = ?1",
                params![pairing_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::ClaimPairing)?;
        let Some((stored_digest, expires_at_ms)) = challenge else {
            return Err(StoreError::PairingRejected);
        };
        let valid_digest: [u8; PAIRING_SECRET_BYTES] = stored_digest
            .try_into()
            .map_err(|_| StoreError::PairingRejected)?;
        if claimed_at_ms >= expires_at_ms || !constant_time_equal(&valid_digest, &secret_digest) {
            return Err(StoreError::PairingRejected);
        }

        let device_id = Uuid::new_v4();
        let access_token = DeviceAccessToken::generate();
        transaction
            .execute(
                "INSERT INTO paired_devices \
                 (device_id, display_name, token_sha256, created_at_ms, last_authenticated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![
                    device_id.to_string(),
                    device_name,
                    access_token.digest().as_slice(),
                    claimed_at_ms,
                ],
            )
            .map_err(StoreError::ClaimPairing)?;
        // Delete rather than mark claimed: raw pairing material and its digest
        // have no value once a device token exists.
        transaction
            .execute(
                "DELETE FROM pairing_challenges WHERE pairing_id = ?1",
                params![pairing_id.to_string()],
            )
            .map_err(StoreError::ClaimPairing)?;
        transaction.commit().map_err(StoreError::ClaimPairing)?;
        Ok(PairedDeviceAccess {
            device_id,
            access_token,
        })
    }

    /// Authenticate an already-paired device without logging or retaining the
    /// presented bearer value. The caller can use the returned public device
    /// identity for authorization decisions.
    pub fn authenticate_device(
        &self,
        access_token: &str,
    ) -> Result<Option<PairedDeviceRecord>, StoreError> {
        let Some(token_digest) = DeviceAccessToken::digest_from_wire(access_token) else {
            return Ok(None);
        };
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT device_id, display_name, created_at_ms, last_authenticated_at_ms \
                 FROM paired_devices WHERE token_sha256 = ?1",
                params![token_digest.as_slice()],
                paired_device_from_row,
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Return the vehicles this Hub has published. Pairing currently grants a
    /// device access to this one owner-controlled Hub, not to arbitrary source
    /// databases or credentials.
    pub fn published_vehicles(&self) -> Result<Vec<PublishedVehicle>, StoreError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT vehicle_id, display_name FROM vehicles \
                 WHERE EXISTS (SELECT 1 FROM sync_manifests \
                               WHERE sync_manifests.vehicle_id = vehicles.vehicle_id) \
                 ORDER BY last_seen_at_ms DESC, vehicle_id ASC",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map([], |row| {
                let value: String = row.get(0)?;
                let vehicle_id = Uuid::parse_str(&value).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(PublishedVehicle {
                    vehicle_id,
                    display_name: row.get(1)?,
                })
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    /// Return the stable Hub identity for a collector source, creating it the
    /// first time the caller presents this non-secret identity pair.
    ///
    /// `source_key` is an opaque stable identifier such as an account or
    /// migration installation id. It must never be a bearer token, URL with a
    /// password, or other secret.
    pub(crate) fn provision_teslamate_import_identity(
        &self,
        source: &SourceRecord,
        source_created: bool,
        identity_hint: &VehicleDescriptor,
        registered_at_ms: i64,
        expected_vehicle_id: Uuid,
    ) -> Result<(VehicleRecord, TeslaMateIdentityRegistrationCheckpoint), StoreError> {
        identity_hint.validate()?;
        if identity_hint.source_id != source.source_id {
            return Err(StoreError::VehicleIdentityConflict);
        }
        validate_timestamp("vehicle registered_at_ms", registered_at_ms)?;
        if expected_vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_source_exists(&transaction, source.source_id)?;
        let source_vehicle = find_vehicle(
            &transaction,
            source.source_id,
            &identity_hint.source_vehicle_key,
        )?;
        let identity_vehicle = find_identity_vehicle(&transaction, identity_hint)?
            .map(|vehicle_id| find_vehicle_by_id(&transaction, vehicle_id))
            .transpose()?
            .flatten();
        let (vehicle, vehicle_created) = match (source_vehicle, identity_vehicle) {
            (Some(source_vehicle), Some(identity_vehicle))
                if source_vehicle.vehicle_id == identity_vehicle.vehicle_id =>
            {
                (source_vehicle, false)
            }
            (Some(source_vehicle), Some(identity_vehicle)) => {
                // A prior crash may leave this TeslaMate-owned row before the
                // exported snapshot proves VIN/EID/VID. It has no aliases and
                // is never published. If a collector has since registered the
                // real identity, remove only the exact untouched placeholder
                // and bind this import to the collector-owned vehicle.
                let has_aliases: bool = transaction
                    .query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM vehicle_identity_aliases WHERE vehicle_id = ?1)",
                        params![source_vehicle.vehicle_id.to_string()],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::Query)?;
                if source_vehicle.vin.is_some()
                    || source_vehicle.display_name.is_some()
                    || has_aliases
                {
                    return Err(StoreError::VehicleIdentityConflict);
                }
                let deleted = transaction
                    .execute(
                        "DELETE FROM vehicles
                          WHERE vehicle_id = ?1 AND source_id = ?2 AND source_vehicle_key = ?3
                            AND vin IS NULL AND display_name IS NULL
                            AND created_at_ms = ?4 AND last_seen_at_ms = ?5",
                        params![
                            source_vehicle.vehicle_id.to_string(),
                            source_vehicle.source_id.to_string(),
                            source_vehicle.source_vehicle_key,
                            source_vehicle.created_at_ms,
                            source_vehicle.last_seen_at_ms,
                        ],
                    )
                    .map_err(StoreError::RegisterVehicle)?;
                if deleted != 1 {
                    return Err(StoreError::VehicleIdentityConflict);
                }
                (identity_vehicle, false)
            }
            (Some(source_vehicle), None) => {
                if source_vehicle.vehicle_id != expected_vehicle_id {
                    return Err(StoreError::VehicleIdentityMismatch {
                        expected: expected_vehicle_id,
                        actual: source_vehicle.vehicle_id,
                    });
                }
                (source_vehicle, false)
            }
            (None, Some(identity_vehicle)) => (identity_vehicle, false),
            (None, None) => {
                transaction
                    .execute(
                        "INSERT INTO vehicles
                            (vehicle_id, source_id, source_vehicle_key, vin, display_name,
                             created_at_ms, last_seen_at_ms)
                         VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?4)",
                        params![
                            expected_vehicle_id.to_string(),
                            source.source_id.to_string(),
                            identity_hint.source_vehicle_key,
                            registered_at_ms,
                        ],
                    )
                    .map_err(StoreError::RegisterVehicle)?;
                (
                    VehicleRecord {
                        vehicle_id: expected_vehicle_id,
                        source_id: source.source_id,
                        source_vehicle_key: identity_hint.source_vehicle_key.clone(),
                        vin: None,
                        display_name: None,
                        created_at_ms: registered_at_ms,
                        last_seen_at_ms: registered_at_ms,
                    },
                    true,
                )
            }
        };
        transaction.commit().map_err(StoreError::RegisterVehicle)?;
        let checkpoint = TeslaMateIdentityRegistrationCheckpoint {
            source: source.clone(),
            source_created,
            vehicle: vehicle.clone(),
            vehicle_created,
        };
        Ok((vehicle, checkpoint))
    }

    pub(crate) fn rollback_teslamate_identity_registration(
        &self,
        checkpoint: &TeslaMateIdentityRegistrationCheckpoint,
    ) -> Result<(), StoreError> {
        if checkpoint.vehicle.vehicle_id.is_nil() || checkpoint.source.source_id.is_nil() {
            return Err(StoreError::InvalidVehicleIdentity);
        }
        if !checkpoint.vehicle_created && !checkpoint.source_created {
            return Ok(());
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if checkpoint.vehicle_created {
            let current = find_vehicle_by_id(&transaction, checkpoint.vehicle.vehicle_id)?;
            let has_aliases: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM vehicle_identity_aliases WHERE vehicle_id = ?1)",
                    params![checkpoint.vehicle.vehicle_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(StoreError::Query)?;
            if current.as_ref() != Some(&checkpoint.vehicle) || has_aliases {
                return Err(StoreError::VehicleIdentityConflict);
            }
            let deleted = transaction
                .execute(
                    "DELETE FROM vehicles WHERE vehicle_id = ?1",
                    params![checkpoint.vehicle.vehicle_id.to_string()],
                )
                .map_err(StoreError::RegisterVehicle)?;
            if deleted != 1 {
                return Err(StoreError::VehicleIdentityConflict);
            }
        }
        if checkpoint.source_created {
            let descriptor = SourceDescriptor::new(
                checkpoint.source.kind.clone(),
                checkpoint.source.key.clone(),
            );
            if find_source(&transaction, &descriptor)?.as_ref() != Some(&checkpoint.source) {
                return Err(StoreError::VehicleIdentityConflict);
            }
            let deleted_identity = transaction
                .execute(
                    "DELETE FROM source_identities
                      WHERE source_id = ?1 AND source_kind = ?2 AND source_key = ?3",
                    params![
                        checkpoint.source.source_id.to_string(),
                        checkpoint.source.kind,
                        checkpoint.source.key,
                    ],
                )
                .map_err(StoreError::RegisterSource)?;
            if deleted_identity != 1 {
                return Err(StoreError::InvalidSourceId);
            }
            let deleted = transaction
                .execute(
                    "DELETE FROM sources
                      WHERE source_id = ?1 AND source_kind = ?2 AND generation = ?3
                        AND created_at_ms = ?4",
                    params![
                        checkpoint.source.source_id.to_string(),
                        checkpoint.source.kind,
                        i64::try_from(checkpoint.source.generation)
                            .map_err(|_| StoreError::InvalidStoredGeneration)?,
                        checkpoint.source.created_at_ms,
                    ],
                )
                .map_err(StoreError::RegisterSource)?;
            if deleted != 1 {
                return Err(StoreError::InvalidSourceId);
            }
        }
        transaction.commit().map_err(StoreError::RegisterVehicle)
    }

    pub(crate) fn register_teslamate_import_source(
        &self,
        descriptor: &SourceDescriptor,
        created_at_ms: i64,
    ) -> Result<(SourceRecord, bool), StoreError> {
        self.register_source_with_creation_state(descriptor, created_at_ms)
    }

    pub fn register_source(
        &self,
        descriptor: &SourceDescriptor,
        created_at_ms: i64,
    ) -> Result<SourceRecord, StoreError> {
        self.register_source_with_creation_state(descriptor, created_at_ms)
            .map(|(source, _)| source)
    }

    fn register_source_with_creation_state(
        &self,
        descriptor: &SourceDescriptor,
        created_at_ms: i64,
    ) -> Result<(SourceRecord, bool), StoreError> {
        descriptor.validate()?;
        validate_timestamp("source created_at_ms", created_at_ms)?;

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if let Some(source) = find_source(&transaction, descriptor)? {
            transaction.commit().map_err(StoreError::RegisterSource)?;
            return Ok((source, false));
        }

        let source_id = Uuid::new_v4();
        transaction
            .execute(
                "INSERT INTO sources (source_id, source_kind, generation, created_at_ms) \
                 VALUES (?1, ?2, 1, ?3)",
                params![source_id.to_string(), descriptor.kind, created_at_ms,],
            )
            .map_err(StoreError::RegisterSource)?;
        transaction
            .execute(
                "INSERT INTO source_identities (source_id, source_kind, source_key) \
                 VALUES (?1, ?2, ?3)",
                params![source_id.to_string(), descriptor.kind, descriptor.key],
            )
            .map_err(StoreError::RegisterSource)?;
        transaction.commit().map_err(StoreError::RegisterSource)?;

        Ok((
            SourceRecord {
                source_id,
                kind: descriptor.kind.clone(),
                key: descriptor.key.clone(),
                generation: 1,
                created_at_ms,
            },
            true,
        ))
    }

    /// Return the stable Hub vehicle identity for one source-owned vehicle.
    /// Re-registering the same source key only refreshes non-identity display
    /// metadata; it can never create a second local vehicle id.
    pub fn register_vehicle(
        &self,
        descriptor: &VehicleDescriptor,
        registered_at_ms: i64,
    ) -> Result<VehicleRecord, StoreError> {
        self.register_vehicle_internal(descriptor, registered_at_ms, None)
    }

    /// Register one source-owned vehicle with an expected stable UUID. This
    /// is for non-Fleet sources such as TeslaMate, where the source identity
    /// and VIN/EID deterministically define the app-facing vehicle identity.
    pub fn register_vehicle_with_id(
        &self,
        descriptor: &VehicleDescriptor,
        registered_at_ms: i64,
        vehicle_id: Uuid,
    ) -> Result<VehicleRecord, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        self.register_vehicle_internal(descriptor, registered_at_ms, Some(vehicle_id))
    }

    fn register_vehicle_internal(
        &self,
        descriptor: &VehicleDescriptor,
        registered_at_ms: i64,
        expected_vehicle_id: Option<Uuid>,
    ) -> Result<VehicleRecord, StoreError> {
        descriptor.validate()?;
        validate_timestamp("vehicle registered_at_ms", registered_at_ms)?;

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_source_exists(&transaction, descriptor.source_id)?;

        let source_vehicle = find_vehicle(
            &transaction,
            descriptor.source_id,
            &descriptor.source_vehicle_key,
        )?;
        let had_source_vehicle = source_vehicle.is_some();
        let identity_vehicle = find_identity_vehicle(&transaction, descriptor)?;
        let identity_record = match identity_vehicle {
            Some(vehicle_id) => find_vehicle_by_id(&transaction, vehicle_id)?,
            None => None,
        };
        if source_vehicle.is_some()
            && identity_vehicle.is_some()
            && source_vehicle.as_ref().map(|v| v.vehicle_id) != identity_vehicle
        {
            return Err(StoreError::VehicleIdentityConflict);
        }
        if let Some(mut vehicle) = source_vehicle.or(identity_record) {
            if let Some(vin) = &descriptor.vin
                && let Some(existing) = vehicle.vin.as_ref()
                && existing != vin
            {
                return Err(StoreError::VehicleIdentityConflict);
            }
            if had_source_vehicle
                && let Some(expected) = expected_vehicle_id
                && expected != vehicle.vehicle_id
            {
                return Err(StoreError::VehicleIdentityMismatch {
                    expected,
                    actual: vehicle.vehicle_id,
                });
            }
            transaction
                .execute(
                    "UPDATE vehicles \
                     SET vin = COALESCE(?1, vin), \
                         display_name = COALESCE(?2, display_name), \
                         last_seen_at_ms = MAX(last_seen_at_ms, ?3) \
                     WHERE vehicle_id = ?4",
                    params![
                        descriptor.vin,
                        descriptor.display_name,
                        registered_at_ms,
                        vehicle.vehicle_id.to_string(),
                    ],
                )
                .map_err(StoreError::RegisterVehicle)?;
            register_vehicle_aliases(&transaction, vehicle.vehicle_id, descriptor)?;
            vehicle.source_id = descriptor.source_id;
            vehicle.source_vehicle_key = descriptor.source_vehicle_key.clone();
            vehicle.vin = descriptor.vin.clone().or(vehicle.vin);
            vehicle.display_name = descriptor.display_name.clone().or(vehicle.display_name);
            vehicle.last_seen_at_ms = vehicle.last_seen_at_ms.max(registered_at_ms);
            transaction.commit().map_err(StoreError::RegisterVehicle)?;
            return Ok(vehicle);
        }

        let vehicle_id = expected_vehicle_id.unwrap_or_else(Uuid::new_v4);
        transaction
            .execute(
                "INSERT INTO vehicles \
                 (vehicle_id, source_id, source_vehicle_key, vin, display_name, created_at_ms, last_seen_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    vehicle_id.to_string(),
                    descriptor.source_id.to_string(),
                    descriptor.source_vehicle_key,
                    descriptor.vin,
                    descriptor.display_name,
                    registered_at_ms,
                ],
            )
            .map_err(StoreError::RegisterVehicle)?;
        let vehicle = VehicleRecord {
            vehicle_id,
            source_id: descriptor.source_id,
            source_vehicle_key: descriptor.source_vehicle_key.clone(),
            vin: descriptor.vin.clone(),
            display_name: descriptor.display_name.clone(),
            created_at_ms: registered_at_ms,
            last_seen_at_ms: registered_at_ms,
        };
        register_vehicle_aliases(&transaction, vehicle.vehicle_id, descriptor)?;
        transaction.commit().map_err(StoreError::RegisterVehicle)?;

        Ok(vehicle)
    }

    pub fn cached_address(
        &self,
        point: crate::location::Wgs84Point,
    ) -> Result<Option<AddressCacheRecord>, StoreError> {
        let key = address_lookup_key(point);
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT a.osm_type, a.osm_id, a.display_name, a.name,
                        a.latitude, a.longitude, a.house_number, a.road,
                        a.neighbourhood, a.city, a.county, a.postcode,
                        a.state, a.state_district, a.country, a.raw_json,
                        l.latitude, l.longitude, l.looked_up_at_ms
                 FROM address_lookup_cache l
                 JOIN address_cache a
                   ON a.osm_type = l.osm_type AND a.osm_id = l.osm_id
                 WHERE l.lookup_key = ?1",
                params![key],
                |row| {
                    Ok(AddressCacheRecord {
                        osm_type: row.get(0)?,
                        osm_id: row.get(1)?,
                        display_name: row.get(2)?,
                        name: row.get(3)?,
                        latitude: row.get(4)?,
                        longitude: row.get(5)?,
                        house_number: row.get(6)?,
                        road: row.get(7)?,
                        neighbourhood: row.get(8)?,
                        city: row.get(9)?,
                        county: row.get(10)?,
                        postcode: row.get(11)?,
                        state: row.get(12)?,
                        state_district: row.get(13)?,
                        country: row.get(14)?,
                        raw_json: row.get(15)?,
                        lookup_latitude: row.get(16)?,
                        lookup_longitude: row.get(17)?,
                        looked_up_at_ms: row.get(18)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    pub fn source_vehicle_key(&self, vehicle_id: Uuid) -> Result<Option<String>, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT COALESCE(
                    (SELECT source_vehicle_key FROM vehicle_identity_aliases a
                     JOIN sources s ON s.source_id = a.source_id
                     WHERE a.vehicle_id = ?1 AND s.source_kind = 'owner_api_compat'
                     ORDER BY a.alias_kind = 'tesla_eid' DESC LIMIT 1),
                    (SELECT source_vehicle_key FROM vehicles WHERE vehicle_id = ?1)
                )",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Capture the latest durable raw observation for one source car without
    /// reading or returning its payload. The source-car mapping is accepted
    /// only when it resolves to exactly one Hub vehicle.
    pub fn observation_watermark(
        &self,
        source_car_id: i64,
    ) -> Result<ObservationWatermark, ObservationVerificationError> {
        let target = self.resolve_observation_target(source_car_id)?;
        let connection = self.open_read_only_connection()?;
        let latest = latest_observation_metadata(&connection, target.vehicle_id, None)?;
        Ok(ObservationWatermark {
            source_car_id,
            source_id: target.source_id,
            vehicle_id: target.vehicle_id,
            observation_id: latest
                .as_ref()
                .map_or(0, |observation| observation.observation_id),
            observed_at_ms: latest
                .as_ref()
                .map(|observation| observation.observed_at_ms),
            received_at_ms: latest
                .as_ref()
                .map(|observation| observation.received_at_ms),
        })
    }

    /// Verify that at least one raw observation for the selected source car
    /// has a strictly greater durable observation id than the supplied
    /// watermark. Only metadata is read and returned.
    pub fn verify_observation_after(
        &self,
        source_car_id: i64,
        after_observation_id: i64,
    ) -> Result<ObservationVerification, ObservationVerificationError> {
        if after_observation_id < 0 {
            return Err(ObservationVerificationError::InvalidWatermark);
        }
        let target = self.resolve_observation_target(source_car_id)?;
        let connection = self.open_read_only_connection()?;
        let latest = latest_observation_metadata(
            &connection,
            target.vehicle_id,
            Some(after_observation_id),
        )?;
        Ok(ObservationVerification {
            source_car_id,
            source_id: target.source_id,
            vehicle_id: target.vehicle_id,
            after_observation_id,
            latest_observation_id: latest
                .as_ref()
                .map(|observation| observation.observation_id),
            latest_observed_at_ms: latest
                .as_ref()
                .map(|observation| observation.observed_at_ms),
            latest_received_at_ms: latest
                .as_ref()
                .map(|observation| observation.received_at_ms),
        })
    }

    /// Capture the highest durable outbound-request receipt id. A caller must
    /// capture this before starting a proof window, then pass it to
    /// `verify_no_wake_after` after the collection attempt has finished.
    pub fn outbound_request_watermark(&self) -> Result<OutboundRequestWatermark, StoreError> {
        let connection = self.open_read_only_connection()?;
        let receipt_id = connection
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM outbound_request_receipts",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        Ok(OutboundRequestWatermark { receipt_id })
    }

    /// Persist an outbound-request attempt before the caller performs network
    /// I/O. This API deliberately accepts only typed classifications and
    /// numeric metadata: URLs, headers, tokens, bodies, response payloads, and
    /// arbitrary error strings cannot be written to the request ledger.
    pub fn begin_outbound_request(
        &self,
        request: &OutboundRequestStart,
    ) -> Result<OutboundRequestReceiptId, StoreError> {
        request.validate()?;
        let started_at_ms = outbound_request_clock_ms()?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_outbound_request_capacity(&transaction)?;
        transaction
            .execute(
                "INSERT INTO outbound_request_receipts(
                    correlation_id, started_at_ms, vehicle_tesla_id, transport,
                    operation, safety_class, precondition, outcome
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'started')",
                params![
                    request.correlation_id.to_string(),
                    started_at_ms,
                    request.vehicle_tesla_id,
                    request.transport.as_str(),
                    request.operation.as_str(),
                    request.safety_class.as_str(),
                    request.precondition.as_str(),
                ],
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        let receipt_id = transaction.last_insert_rowid();
        transaction
            .commit()
            .map_err(StoreError::OutboundRequestReceipt)?;
        Ok(OutboundRequestReceiptId(receipt_id))
    }

    /// Complete a previously durable request attempt in a separate SQLite
    /// transaction. Every retry must use a new `begin_outbound_request` call;
    /// this method never overwrites an earlier terminal receipt.
    pub fn complete_outbound_request(
        &self,
        receipt_id: OutboundRequestReceiptId,
        completion: &OutboundRequestCompletion,
    ) -> Result<(), StoreError> {
        completion.validate()?;
        if receipt_id.0 <= 0 {
            return Err(StoreError::InvalidOutboundRequestReceiptId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let started_at_ms: Option<i64> = transaction
            .query_row(
                "SELECT started_at_ms FROM outbound_request_receipts
                 WHERE id = ?1 AND outcome = 'started'",
                params![receipt_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::OutboundRequestReceipt)?;
        let started_at_ms = started_at_ms.ok_or(StoreError::OutboundRequestReceiptNotStarted)?;
        // Store-generated time governs terminal receipt age and duration. This
        // prevents a caller-controlled clock from expiring a receipt early or
        // holding retention indefinitely. Clamp a backwards wall-clock step to
        // the durable start timestamp rather than creating an invalid row.
        let completed_at_ms = outbound_request_clock_ms()?.max(started_at_ms);
        let duration_ms = completed_at_ms - started_at_ms;
        transaction
            .execute(
                "UPDATE outbound_request_receipts
                 SET completed_at_ms = ?2, duration_ms = ?3, outcome = ?4,
                     http_status = ?5, retry_after_seconds = ?6
                 WHERE id = ?1 AND outcome = 'started'",
                params![
                    receipt_id.0,
                    completed_at_ms,
                    duration_ms,
                    completion.outcome.as_str(),
                    completion.http_status,
                    completion
                        .retry_after_seconds
                        .map(i64::try_from)
                        .transpose()
                        .map_err(|_| StoreError::InvalidOutboundRequestRetryAfter)?
                ],
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        // Retention cleanup only ever removes terminal rows older than the
        // store-clock 30-day cutoff. It never deletes in-window or unresolved
        // receipts merely to meet the capacity bound.
        prune_expired_outbound_request_receipts(&transaction)?;
        transaction
            .commit()
            .map_err(StoreError::OutboundRequestReceipt)
    }

    // Legacy refresh receipt journaling was removed. Hub refresh persistence is
    // now one atomic replacement of the encrypted TeslaMate token pair.
    /// attempt. A process crash or task abort deliberately leaves this row
    /// unresolved. Normal code paths terminalize it explicitly, distinguishing
    /// an orderly unsubscribe from cancellation, transport loss, or failure.
    pub fn begin_stream_session(
        &self,
        correlation_id: Uuid,
        vehicle_tesla_id: i64,
    ) -> Result<StreamSessionReceiptId, StoreError> {
        if correlation_id.is_nil() {
            return Err(StoreError::NilOutboundRequestCorrelationId);
        }
        if vehicle_tesla_id <= 0 {
            return Err(StoreError::InvalidOutboundRequestVehicleId);
        }
        let started_at_ms = outbound_request_clock_ms()?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_stream_session_capacity(&transaction)?;
        transaction
            .execute(
                "INSERT INTO stream_session_receipts(
                    correlation_id, vehicle_tesla_id, started_at_ms, outcome
                 ) VALUES (?1, ?2, ?3, 'started')",
                params![correlation_id.to_string(), vehicle_tesla_id, started_at_ms],
            )
            .map_err(StoreError::StreamSessionReceipt)?;
        let receipt_id = transaction.last_insert_rowid();
        transaction
            .commit()
            .map_err(StoreError::StreamSessionReceipt)?;
        Ok(StreamSessionReceiptId(receipt_id))
    }

    /// Complete a session only after its explicit unsubscribe control request
    /// has itself completed successfully under the same correlation and car.
    pub fn complete_stream_session_orderly(
        &self,
        session_id: StreamSessionReceiptId,
        unsubscribe_receipt_id: OutboundRequestReceiptId,
    ) -> Result<(), StoreError> {
        if session_id.0 <= 0 || unsubscribe_receipt_id.0 <= 0 {
            return Err(StoreError::InvalidStreamSessionReceiptId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let session: Option<(i64, String, i64)> = transaction
            .query_row(
                "SELECT started_at_ms, correlation_id, vehicle_tesla_id
                 FROM stream_session_receipts WHERE id = ?1 AND outcome = 'started'",
                params![session_id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::StreamSessionReceipt)?;
        let (started_at_ms, correlation_id, vehicle_tesla_id) =
            session.ok_or(StoreError::StreamSessionReceiptNotStarted)?;
        // A receipt from an earlier supervisor attempt under the same
        // correlation/car is not evidence that this session shut down
        // cleanly. The control request must both start and finish after this
        // exact session began; any later session, including one that already
        // completed, makes this session non-terminal. Callers therefore fail
        // closed rather than attaching an unsubscribe to the wrong attempt.
        let unsubscribe_ok: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM outbound_request_receipts
                 WHERE id = ?1 AND correlation_id = ?2 AND vehicle_tesla_id = ?3
                   AND transport = 'stream' AND operation = 'stream_unsubscribe'
                   AND outcome = 'success'
                   AND started_at_ms >= ?4 AND completed_at_ms >= ?4
                   AND NOT EXISTS (
                       SELECT 1 FROM stream_session_receipts AS newer
                       WHERE newer.correlation_id = ?2
                         AND newer.vehicle_tesla_id = ?3
                         AND newer.id <> ?5
                         AND (newer.started_at_ms > ?4
                              OR (newer.started_at_ms = ?4 AND newer.id > ?5))
                   )",
                params![
                    unsubscribe_receipt_id.0,
                    correlation_id,
                    vehicle_tesla_id,
                    started_at_ms,
                    session_id.0,
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::StreamSessionReceipt)?;
        if unsubscribe_ok.is_none() {
            return Err(StoreError::StreamSessionUnsubscribeNotCompleted);
        }
        let completed_at_ms = outbound_request_clock_ms()?.max(started_at_ms);
        transaction
            .execute(
                "UPDATE stream_session_receipts
                 SET completed_at_ms = ?2, duration_ms = ?3, outcome = 'orderly_shutdown',
                     unsubscribe_receipt_id = ?4
                 WHERE id = ?1 AND outcome = 'started'",
                params![
                    session_id.0,
                    completed_at_ms,
                    completed_at_ms - started_at_ms,
                    unsubscribe_receipt_id.0,
                ],
            )
            .map_err(StoreError::StreamSessionReceipt)?;
        prune_expired_stream_session_receipts(&transaction)?;
        transaction
            .commit()
            .map_err(StoreError::StreamSessionReceipt)
    }

    /// Resolve a supervisor lifetime that ended without an active subscribed
    /// socket to unsubscribe. This is not an orderly-unsubscribe receipt and
    /// cannot be confused with one: the terminal outcome is explicit and the
    /// unsubscribe reference must remain NULL. A process crash still leaves
    /// `started`, preserving the crash evidence used by no-wake verification.
    pub fn complete_stream_session_terminal(
        &self,
        session_id: StreamSessionReceiptId,
        outcome: StreamSessionTerminalOutcome,
    ) -> Result<(), StoreError> {
        if session_id.0 <= 0 {
            return Err(StoreError::InvalidStreamSessionReceiptId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let started_at_ms: Option<i64> = transaction
            .query_row(
                "SELECT started_at_ms FROM stream_session_receipts
                 WHERE id = ?1 AND outcome = 'started'",
                params![session_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::StreamSessionReceipt)?;
        let started_at_ms = started_at_ms.ok_or(StoreError::StreamSessionReceiptNotStarted)?;
        let completed_at_ms = outbound_request_clock_ms()?.max(started_at_ms);
        let updated = transaction
            .execute(
                "UPDATE stream_session_receipts
                 SET completed_at_ms = ?2, duration_ms = ?3, outcome = ?4
                 WHERE id = ?1 AND outcome = 'started'",
                params![
                    session_id.0,
                    completed_at_ms,
                    completed_at_ms - started_at_ms,
                    outcome.as_str(),
                ],
            )
            .map_err(StoreError::StreamSessionReceipt)?;
        if updated != 1 {
            return Err(StoreError::StreamSessionReceiptNotStarted);
        }
        prune_expired_stream_session_receipts(&transaction)?;
        transaction
            .commit()
            .map_err(StoreError::StreamSessionReceipt)
    }

    /// Return bounded, redacted receipt metadata for one correlation after a
    /// captured watermark. This is intentionally the only public receipt read
    /// API; it cannot return a request URL, headers, bodies, or error text
    /// because none are persisted.
    pub fn outbound_request_receipts_after(
        &self,
        after_receipt_id: i64,
        correlation_id: Uuid,
        limit: u32,
    ) -> Result<Vec<OutboundRequestReceipt>, StoreError> {
        if after_receipt_id < 0 {
            return Err(StoreError::InvalidOutboundRequestWatermark);
        }
        if limit == 0 || limit > MAX_OUTBOUND_REQUEST_QUERY_LIMIT {
            return Err(StoreError::InvalidOutboundRequestQueryLimit {
                actual: limit,
                maximum: MAX_OUTBOUND_REQUEST_QUERY_LIMIT,
            });
        }
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, correlation_id, started_at_ms, completed_at_ms, duration_ms,
                        vehicle_tesla_id, transport, operation, safety_class,
                        precondition, outcome, http_status, retry_after_seconds
                 FROM outbound_request_receipts
                 WHERE id > ?1 AND correlation_id = ?2
                 ORDER BY id ASC LIMIT ?3",
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        let rows = statement
            .query_map(
                params![
                    after_receipt_id,
                    correlation_id.to_string(),
                    i64::from(limit)
                ],
                receipt_from_row,
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        rows.map(|row| row.map_err(StoreError::OutboundRequestReceipt))
            .collect()
    }

    /// Verify a bounded, correlation-scoped no-wake audit window. Empty audit
    /// windows are intentionally not proof: until network clients emit receipt
    /// rows, a verifier must fail closed rather than treating absence of data as
    /// evidence of safe collection.
    pub fn verify_no_wake_after(
        &self,
        after_receipt_id: i64,
        correlation_id: Uuid,
        observation: Option<(i64, i64)>,
    ) -> Result<NoWakeVerification, NoWakeVerificationError> {
        if after_receipt_id < 0 {
            return Err(NoWakeVerificationError::InvalidAuditWatermark);
        }
        let connection = self.open_read_only_connection()?;
        let (matching_receipts, unresolved_receipts, direct_wake_receipts, conditional_without_power_receipts) = connection
            .query_row(
                "SELECT
                    COUNT(*),
                    COALESCE(SUM(CASE WHEN outcome = 'started' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN safety_class = 'direct_wake_command' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN operation = 'vehicle_data'
                                  AND (precondition <> 'stream_power_confirmed'
                                       OR safety_class <> 'conditional_read')
                             THEN 1 ELSE 0 END), 0)
                 FROM outbound_request_receipts
                 WHERE id > ?1 AND correlation_id = ?2",
                params![after_receipt_id, correlation_id.to_string()],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                )),
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        let unresolved_stream_sessions: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM stream_session_receipts
                 WHERE correlation_id = ?1 AND outcome = 'started'",
                params![correlation_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::StreamSessionReceipt)?;
        let observation = match observation {
            Some((source_car_id, watermark)) => {
                Some(self.verify_observation_after(source_car_id, watermark)?)
            }
            None => None,
        };
        Ok(NoWakeVerification {
            after_receipt_id,
            correlation_id,
            matching_receipts,
            unresolved_receipts,
            unresolved_stream_sessions,
            direct_wake_receipts,
            conditional_without_power_receipts,
            observation,
        })
    }

    fn resolve_observation_target(
        &self,
        source_car_id: i64,
    ) -> Result<ObservationTarget, ObservationVerificationError> {
        require_positive_db(source_car_id, "source car id")
            .map_err(|_| ObservationVerificationError::InvalidSourceCarId)?;
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT DISTINCT vehicles.vehicle_id, vehicles.source_id
                 FROM vehicles
                 WHERE vehicles.vehicle_id IN (
                    SELECT vehicle_id FROM materialised_cars WHERE car_id = ?1
                    UNION
                    SELECT vehicle_id FROM vehicle_lifecycle_state WHERE car_id = ?1
                    UNION
                    SELECT vehicle_id FROM car_settings WHERE car_id = ?1
                 )
                 ORDER BY vehicles.vehicle_id",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![source_car_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(StoreError::Query)?;
        let mut targets = Vec::new();
        for row in rows {
            let (vehicle_id, source_id) = row.map_err(StoreError::Query)?;
            targets.push(ObservationTarget {
                vehicle_id: parse_stored_uuid("observation vehicle", &vehicle_id)?,
                source_id: parse_stored_uuid("observation source", &source_id)?,
            });
        }
        match targets.as_slice() {
            [] => Err(ObservationVerificationError::NoVehicleMapping),
            [target] => Ok(*target),
            _ => Err(ObservationVerificationError::AmbiguousVehicleMapping),
        }
    }

    pub fn put_address_cache(&self, record: &AddressCacheRecord) -> Result<(), StoreError> {
        validate_address_cache_record(record)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "INSERT INTO address_cache(
                    osm_type, osm_id, display_name, name, latitude, longitude,
                    house_number, road, neighbourhood, city, county, postcode,
                    state, state_district, country, raw_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(osm_type, osm_id) DO UPDATE SET
                    display_name = excluded.display_name,
                    name = excluded.name,
                    latitude = excluded.latitude,
                    longitude = excluded.longitude,
                    house_number = excluded.house_number,
                    road = excluded.road,
                    neighbourhood = excluded.neighbourhood,
                    city = excluded.city,
                    county = excluded.county,
                    postcode = excluded.postcode,
                    state = excluded.state,
                    state_district = excluded.state_district,
                    country = excluded.country,
                    raw_json = excluded.raw_json",
                params![
                    record.osm_type,
                    record.osm_id,
                    record.display_name,
                    record.name,
                    record.latitude,
                    record.longitude,
                    record.house_number,
                    record.road,
                    record.neighbourhood,
                    record.city,
                    record.county,
                    record.postcode,
                    record.state,
                    record.state_district,
                    record.country,
                    record.raw_json,
                ],
            )
            .map_err(StoreError::AddressCacheWrite)?;
        transaction
            .execute(
                "INSERT INTO address_lookup_cache(
                    lookup_key, latitude, longitude, osm_type, osm_id, looked_up_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(lookup_key) DO UPDATE SET
                    latitude = excluded.latitude,
                    longitude = excluded.longitude,
                    osm_type = excluded.osm_type,
                    osm_id = excluded.osm_id,
                    looked_up_at_ms = excluded.looked_up_at_ms",
                params![
                    address_lookup_key(crate::location::Wgs84Point {
                        latitude: record.lookup_latitude,
                        longitude: record.lookup_longitude,
                    }),
                    record.lookup_latitude,
                    record.lookup_longitude,
                    record.osm_type,
                    record.osm_id,
                    record.looked_up_at_ms,
                ],
            )
            .map_err(StoreError::AddressCacheWrite)?;
        transaction.commit().map_err(StoreError::AddressCacheWrite)
    }

    pub fn claim_address_enrichment_job(
        &self,
        now_ms: i64,
    ) -> Result<Option<AddressEnrichmentJob>, StoreError> {
        validate_timestamp("address job now_ms", now_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let job = {
            let mut statement = transaction
                .prepare(
                    "SELECT job_key, vehicle_id, target_type, target_id, field,
                            latitude, longitude, attempts
                     FROM address_enrichment_jobs
                     WHERE (status IN ('pending', 'retry') AND next_attempt_ms <= ?1)
                        OR (status = 'running' AND lease_until_ms <= ?1)
                     ORDER BY next_attempt_ms ASC, job_key ASC LIMIT 1",
                )
                .map_err(StoreError::Query)?;
            statement
                .query_row(params![now_ms], |row| {
                    let vehicle_id: String = row.get(1)?;
                    Ok(AddressEnrichmentJob {
                        job_key: row.get(0)?,
                        vehicle_id: Uuid::parse_str(&vehicle_id).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                1,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        target_type: row.get(2)?,
                        target_id: row.get(3)?,
                        field: row.get(4)?,
                        latitude: row.get(5)?,
                        longitude: row.get(6)?,
                        attempts: row
                            .get::<_, i64>(7)?
                            .try_into()
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, i64::MAX))?,
                    })
                })
                .optional()
                .map_err(StoreError::Query)?
        };
        if let Some(job) = &job {
            transaction
                .execute(
                    "UPDATE address_enrichment_jobs
                     SET status = 'running', attempts = attempts + 1,
                         lease_until_ms = ?1
                     WHERE job_key = ?2",
                    params![now_ms.saturating_add(5 * 60 * 1000), job.job_key],
                )
                .map_err(StoreError::AddressEnrichmentWrite)?;
        }
        transaction
            .commit()
            .map_err(StoreError::AddressEnrichmentWrite)?;
        Ok(job.map(|mut job| {
            job.attempts = job.attempts.saturating_add(1);
            job
        }))
    }

    pub fn complete_address_enrichment(
        &self,
        job: &AddressEnrichmentJob,
        address: Option<&str>,
        now_ms: i64,
    ) -> Result<AddressEnrichmentCompletion, StoreError> {
        validate_timestamp("address completion now_ms", now_ms)?;
        if let Some(address) = address
            && (address.trim().is_empty()
                || address.len() > MAX_DISPLAY_NAME_BYTES
                || address.chars().any(char::is_control))
        {
            return Err(StoreError::InvalidAddressEnrichment);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let mut changed = false;
        if let Some(address) = address {
            let (table, json_column, id_column) = match job.target_type.as_str() {
                "drive" => ("materialised_drives", "drive_json", "drive_id"),
                "charge" => ("materialised_charges", "charge_json", "charge_id"),
                _ => return Err(StoreError::InvalidAddressEnrichment),
            };
            let select = format!(
                "SELECT {json_column}, car_id FROM {table} WHERE vehicle_id = ?1 AND {id_column} = ?2"
            );
            let current: Option<(String, i64)> = transaction
                .query_row(
                    &select,
                    params![job.vehicle_id.to_string(), job.target_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(StoreError::Query)?;
            if let Some((current, car_id)) = current {
                let mut value: Value =
                    serde_json::from_str(&current).map_err(StoreError::DeserializeLifecycleRow)?;
                let object = value
                    .as_object_mut()
                    .ok_or(StoreError::InvalidAddressEnrichment)?;
                if object.get(&job.field).and_then(Value::as_str).is_none() {
                    object.insert(job.field.clone(), Value::String(address.trim().to_owned()));
                    let updated =
                        serde_json::to_string(&value).map_err(StoreError::SerializeLifecycleRow)?;
                    let update = format!(
                        "UPDATE {table} SET {json_column} = ?1 WHERE vehicle_id = ?2 AND {id_column} = ?3"
                    );
                    transaction
                        .execute(
                            &update,
                            params![updated, job.vehicle_id.to_string(), job.target_id],
                        )
                        .map_err(StoreError::AddressEnrichmentWrite)?;
                    let entity = if job.target_type == "drive" {
                        "drive"
                    } else {
                        "charge"
                    };
                    record_sync_mutation_in_transaction(
                        &transaction,
                        job.vehicle_id,
                        entity,
                        job.target_id,
                        car_id,
                        "upsert",
                        &updated,
                    )?;
                    changed = true;
                }
            }
        }
        transaction
            .execute(
                "UPDATE address_enrichment_jobs
                 SET status = 'complete', completed_at_ms = ?1, lease_until_ms = 0,
                     last_error = NULL
                 WHERE job_key = ?2",
                params![now_ms, job.job_key],
            )
            .map_err(StoreError::AddressEnrichmentWrite)?;
        if changed {
            mark_export_dirty_in_transaction(&transaction, job.vehicle_id)?;
        }
        transaction
            .commit()
            .map_err(StoreError::AddressEnrichmentWrite)?;
        Ok(AddressEnrichmentCompletion {
            vehicle_id: job.vehicle_id,
            changed,
        })
    }

    pub fn retry_address_enrichment(
        &self,
        job: &AddressEnrichmentJob,
        error: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("address retry now_ms", now_ms)?;
        let delay_seconds = 5_u64
            .saturating_mul(1_u64 << job.attempts.min(14))
            .min(24 * 60 * 60);
        let delay_ms = i64::try_from(delay_seconds.saturating_mul(1000)).unwrap_or(i64::MAX);
        let bounded_error = error
            .chars()
            .filter(|character| !character.is_control())
            .take(256)
            .collect::<String>();
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE address_enrichment_jobs
                 SET status = 'retry', next_attempt_ms = ?1, lease_until_ms = 0,
                     last_error = ?2
                 WHERE job_key = ?3",
                params![now_ms.saturating_add(delay_ms), bounded_error, job.job_key],
            )
            .map_err(StoreError::AddressEnrichmentWrite)?;
        Ok(())
    }

    /// Append exactly one bounded raw telemetry snapshot. The stored hash is
    /// calculated from the canonical JSON bytes that are written to SQLite.
    /// A collector retry for the same source, vehicle, observation time, and
    /// payload returns the original row without creating a duplicate.
    pub fn append_observation(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
    ) -> Result<AppendObservation, StoreError> {
        input.validate()?;
        validate_timestamp("observation received_at_ms", received_at_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let result = append_observation_in_transaction(&transaction, input, received_at_ms);
        if result.is_ok() {
            transaction
                .commit()
                .map_err(StoreError::AppendObservation)?;
        }
        result
    }

    pub(crate) fn accept_stream_observation_and_lifecycle(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
        car_id: i64,
    ) -> Result<StreamObservationResult, StoreError> {
        input.validate()?;
        validate_timestamp("observation received_at_ms", received_at_ms)?;
        if car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if !stream_timestamp_is_newer(&transaction, input.vehicle_id, input.observed_at_ms)? {
            transaction.commit().map_err(StoreError::LifecycleWrite)?;
            return Ok(StreamObservationResult::IgnoredDuplicate);
        }
        self.maybe_stream_fault(StreamFaultPoint::RawInsert)?;
        let appended = append_observation_in_transaction(&transaction, input, received_at_ms)?;

        self.maybe_stream_fault(StreamFaultPoint::LifecycleWrite)?;
        let existing = load_lifecycle_state_in_transaction(&transaction, input.vehicle_id)?;
        let mut state = match existing.as_ref() {
            Some(record) => crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                .unwrap_or_else(|_| {
                    let mut clean = crate::lifecycle::OpenSessionState::new();
                    clean.last_observation_id = record.last_observation_id;
                    clean
                }),
            None => crate::lifecycle::OpenSessionState::new(),
        };
        // Do not rehydrate full open child collections on every observation.
        // Aggregates in open_session_json plus lifecycle_open_rows are enough
        // for incremental extend; commit reloads children only when a session
        // closes.
        let observations = observations_after_id_in_transaction(
            &transaction,
            input.vehicle_id,
            state.last_observation_id,
            MAX_OBSERVATION_QUERY_LIMIT,
        )?;
        let mut delta = crate::lifecycle::LifecycleDelta::default();
        let mut quarantined = existing.as_ref().is_some_and(|record| record.quarantined);
        for observation in observations {
            let sample = crate::lifecycle::LifecycleSample {
                observation_id: observation.observation_id,
                observed_at_ms: observation.observed_at_ms,
                vehicle_state: observation_vehicle_state(&observation.payload),
                payload: observation.payload,
            };
            let step = crate::lifecycle::apply_sample(state, car_id, &sample)
                .map_err(StoreError::LifecycleProjection)?;
            state = step.state;
            quarantined |= step.quarantined;
            delta.drives.extend(step.delta.drives);
            delta.positions.extend(step.delta.positions);
            delta.charges.extend(step.delta.charges);
            delta.charge_samples.extend(step.delta.charge_samples);
            delta.states.extend(step.delta.states);
            delta.updates.extend(step.delta.updates);
            delta
                .charge_start_coordinates
                .extend(step.delta.charge_start_coordinates);
            delta
                .open_drive_positions
                .extend(step.delta.open_drive_positions);
            delta
                .open_charge_samples
                .extend(step.delta.open_charge_samples);
        }
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let encoded = state
            .encode()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        Self::commit_lifecycle_delta_in_transaction(
            &transaction,
            &LifecycleCommit {
                vehicle_id: input.vehicle_id,
                car_id,
                open_session_json: &encoded,
                last_observation_id: state.last_observation_id,
                quarantined,
                updated_at_ms: received_at_ms,
                delta: &delta,
            },
        )?;
        self.maybe_stream_fault(StreamFaultPoint::WatermarkUpdate)?;
        accept_stream_timestamp_in_transaction(
            &transaction,
            input.vehicle_id,
            input.observed_at_ms,
        )?;
        self.maybe_stream_fault(StreamFaultPoint::Commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(StreamObservationResult::Committed {
            observation_id: appended.observation.observation_id,
        })
    }

    pub fn accept_owner_observation_and_lifecycle(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
        car_id: i64,
    ) -> Result<OwnerObservationResult, StoreError> {
        input.validate()?;
        validate_timestamp("observation received_at_ms", received_at_ms)?;
        if car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        self.maybe_stream_fault(StreamFaultPoint::RawInsert)?;
        let appended = append_observation_in_transaction(&transaction, input, received_at_ms)?;
        self.maybe_stream_fault(StreamFaultPoint::LifecycleWrite)?;
        let existing = load_lifecycle_state_in_transaction(&transaction, input.vehicle_id)?;
        let mut state = match existing.as_ref() {
            Some(record) => crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                .unwrap_or_else(|_| {
                    let mut clean = crate::lifecycle::OpenSessionState::new();
                    clean.last_observation_id = record.last_observation_id;
                    clean
                }),
            None => crate::lifecycle::OpenSessionState::new(),
        };
        // Incremental path: no full open-child rehydrate per observation.
        let observations = observations_after_id_in_transaction(
            &transaction,
            input.vehicle_id,
            state.last_observation_id,
            MAX_OBSERVATION_QUERY_LIMIT,
        )?;
        let mut delta = crate::lifecycle::LifecycleDelta::default();
        let mut quarantined = existing.as_ref().is_some_and(|record| record.quarantined);
        for observation in observations {
            let sample = crate::lifecycle::LifecycleSample {
                observation_id: observation.observation_id,
                observed_at_ms: observation.observed_at_ms,
                vehicle_state: observation_vehicle_state(&observation.payload),
                payload: observation.payload,
            };
            let step = crate::lifecycle::apply_sample(state, car_id, &sample)
                .map_err(StoreError::LifecycleProjection)?;
            state = step.state;
            quarantined |= step.quarantined;
            delta.drives.extend(step.delta.drives);
            delta.positions.extend(step.delta.positions);
            delta.charges.extend(step.delta.charges);
            delta.charge_samples.extend(step.delta.charge_samples);
            delta.states.extend(step.delta.states);
            delta.updates.extend(step.delta.updates);
            delta
                .charge_start_coordinates
                .extend(step.delta.charge_start_coordinates);
            delta
                .open_drive_positions
                .extend(step.delta.open_drive_positions);
            delta
                .open_charge_samples
                .extend(step.delta.open_charge_samples);
        }
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let encoded = state
            .encode()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        Self::commit_lifecycle_delta_in_transaction(
            &transaction,
            &LifecycleCommit {
                vehicle_id: input.vehicle_id,
                car_id,
                open_session_json: &encoded,
                last_observation_id: state.last_observation_id,
                quarantined,
                updated_at_ms: received_at_ms,
                delta: &delta,
            },
        )?;
        self.maybe_stream_fault(StreamFaultPoint::Commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(OwnerObservationResult {
            append: appended,
            drives_closed: delta.drives.len(),
            charges_closed: delta.charges.len(),
            positions_materialised: delta.positions.len(),
            charge_samples_materialised: delta.charge_samples.len(),
            lifecycle_quarantined: quarantined,
        })
    }

    /// Advance the durable watermark for stream telemetry. This is deliberately
    /// separate from Owner API observations: each source has its own ordering
    /// contract, and a stream frame must never block an Owner API response.
    pub fn accept_stream_timestamp(
        &self,
        vehicle_id: Uuid,
        timestamp_ms: i64,
    ) -> Result<bool, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        validate_timestamp("stream timestamp", timestamp_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let accepted =
            accept_stream_timestamp_in_transaction(&transaction, vehicle_id, timestamp_ms)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(accepted)
    }

    /// Read a bounded, time-ordered raw observation page for a single stable
    /// Hub vehicle identity.
    pub fn observations_for_vehicle(
        &self,
        vehicle_id: Uuid,
        query: ObservationQuery,
    ) -> Result<Vec<ObservationRecord>, StoreError> {
        query.validate()?;
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms, \
                        payload_sha256, payload_json \
                 FROM raw_observations \
                 WHERE vehicle_id = ?1 \
                   AND (?2 IS NULL OR observed_at_ms >= ?2) \
                   AND (?3 IS NULL OR observed_at_ms < ?3) \
                 ORDER BY observed_at_ms ASC, observation_id ASC \
                 LIMIT ?4",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    query.from_observed_at_ms,
                    query.until_observed_at_ms,
                    i64::from(query.limit),
                ],
                observation_from_row,
            )
            .map_err(StoreError::Query)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    /// Read observations in durable insertion order after a lifecycle cursor.
    /// A lifecycle cursor is an observation ID, not a source timestamp.
    pub fn observations_after_id_for_vehicle(
        &self,
        vehicle_id: Uuid,
        after_observation_id: i64,
        limit: u32,
    ) -> Result<Vec<ObservationRecord>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if after_observation_id < 0 {
            return Err(StoreError::InvalidLifecycleCursor);
        }
        if !(1..=MAX_OBSERVATION_QUERY_LIMIT).contains(&limit) {
            return Err(StoreError::InvalidObservationQueryLimit {
                actual: limit,
                maximum: MAX_OBSERVATION_QUERY_LIMIT,
            });
        }
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms, \
                        payload_sha256, payload_json \
                 FROM raw_observations \
                 WHERE vehicle_id = ?1 AND observation_id > ?2 \
                 ORDER BY observation_id ASC LIMIT ?3",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    after_observation_id,
                    i64::from(limit)
                ],
                observation_from_row,
            )
            .map_err(StoreError::Query)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    /// Load durable open-session state for crash-safe lifecycle recovery.
    pub fn load_lifecycle_state(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<LifecycleStateRecord>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT vehicle_id, car_id, last_observation_id, open_session_json, \
                        quarantined, updated_at_ms \
                 FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    let value: String = row.get(0)?;
                    let vehicle_id = Uuid::parse_str(&value).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok(LifecycleStateRecord {
                        vehicle_id,
                        car_id: row.get(1)?,
                        last_observation_id: row.get(2)?,
                        open_session_json: row.get(3)?,
                        quarantined: row.get::<_, i64>(4)? != 0,
                        updated_at_ms: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Rehydrate provisional drive/charge children without placing them in
    /// the bounded lifecycle JSON document.
    /// Durable open drive positions for one parent (incremental lifecycle path).
    pub fn open_drive_positions(
        &self,
        vehicle_id: Uuid,
        drive_id: i64,
    ) -> Result<Vec<crate::hub_pack::ProjectionPosition>, StoreError> {
        let connection = self.open()?;
        let vehicle = vehicle_id.to_string();
        let mut statement = connection
            .prepare(
                "SELECT row_json FROM lifecycle_open_rows
                 WHERE vehicle_id = ?1 AND domain = 'position'
                   AND parent_source_row_id = ?2
                 ORDER BY source_row_id",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![vehicle, drive_id], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?;
        let mut positions = Vec::new();
        for row in rows {
            let json = row.map_err(StoreError::Query)?;
            positions
                .push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
        }
        Ok(positions)
    }

    /// Durable open charge samples for one parent (incremental lifecycle path).
    pub fn open_charge_samples(
        &self,
        vehicle_id: Uuid,
        charge_id: i64,
    ) -> Result<Vec<crate::hub_pack::ProjectionChargeSample>, StoreError> {
        let connection = self.open()?;
        let vehicle = vehicle_id.to_string();
        let mut statement = connection
            .prepare(
                "SELECT row_json FROM lifecycle_open_rows
                 WHERE vehicle_id = ?1 AND domain = 'charge_sample'
                   AND parent_source_row_id = ?2
                 ORDER BY source_row_id",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![vehicle, charge_id], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?;
        let mut samples = Vec::new();
        for row in rows {
            let json = row.map_err(StoreError::Query)?;
            samples.push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
        }
        Ok(samples)
    }

    pub fn restore_lifecycle_open_children(
        &self,
        vehicle_id: Uuid,
        state: &mut crate::lifecycle::OpenSessionState,
    ) -> Result<(), StoreError> {
        let connection = self.open()?;
        let vehicle = vehicle_id.to_string();
        let mut statement = connection
            .prepare(
                "SELECT domain, parent_source_row_id, row_json
                 FROM lifecycle_open_rows WHERE vehicle_id = ?1
                 ORDER BY source_row_id",
            )
            .map_err(StoreError::Query)?;
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let rows = statement
            .query_map(params![vehicle], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(StoreError::Query)?;
        for row in rows {
            let (domain, parent_id, json) = row.map_err(StoreError::Query)?;
            match domain.as_str() {
                "position" => {
                    let position: crate::hub_pack::ProjectionPosition =
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
                    if state
                        .open_drive
                        .as_ref()
                        .is_some_and(|open| Some(open.id) == parent_id)
                    {
                        state
                            .open_drive
                            .as_mut()
                            .expect("open drive")
                            .positions
                            .push(position);
                    }
                }
                "charge_sample" => {
                    let sample: crate::hub_pack::ProjectionChargeSample =
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
                    if state
                        .open_charge
                        .as_ref()
                        .is_some_and(|open| Some(open.id) == parent_id)
                    {
                        state
                            .open_charge
                            .as_mut()
                            .expect("open charge")
                            .samples
                            .push(sample);
                    }
                }
                _ => {}
            }
        }
        if let Some(open) = state.open_drive.as_mut() {
            if let Some(first) = open.positions.first() {
                open.start_latitude = Some(first.latitude);
                open.start_longitude = Some(first.longitude);
                open.start_soc = first.battery_level;
                open.start_rated_range_km = first.rated_battery_range_km;
            }
            open.outside_temp_sum = 0.0;
            open.outside_temp_count = 0;
            open.speed_max = None;
            for position in &open.positions {
                if let Some(value) = position.outside_temp {
                    open.outside_temp_sum += value;
                    open.outside_temp_count = open.outside_temp_count.saturating_add(1);
                }
                open.speed_max = match (open.speed_max, position.speed) {
                    (Some(current), Some(next)) => Some(current.max(next)),
                    (None, value) => value,
                    (current, None) => current,
                };
            }
        }
        Ok(())
    }

    /// Atomically retain an imported open-session snapshot outside the bounded
    /// lifecycle blob. Repeating the same source snapshot is a no-op.
    pub fn seed_imported_open_session(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        session: &TeslaMateOpenSession,
        updated_at_ms: i64,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        self.seed_imported_open_session_checked(
            source_id,
            vehicle_id,
            car_id,
            session,
            updated_at_ms,
            None,
        )
    }

    fn seed_imported_open_session_checked(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        session: &TeslaMateOpenSession,
        updated_at_ms: i64,
        expected: Option<(i64, i64)>,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        if source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }
        validate_timestamp("open session updated_at_ms", updated_at_ms)?;
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;

        let previous = self.load_lifecycle_state(vehicle_id)?;
        let previous_state = previous
            .as_ref()
            .map(|record| {
                crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                    .map_err(|_| StoreError::InvalidLifecycleSession)
            })
            .transpose()?;
        let previous_open = self.load_imported_open_session(source_id, vehicle_id)?;
        let seeded = crate::lifecycle::seed_imported_open_session_state(
            source_id,
            session,
            previous_state.as_ref(),
        )
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
        let same_seed = previous_state
            .as_ref()
            .and_then(|state| state.imported_open.as_ref())
            .is_some_and(|refs| {
                refs.source_id == source_id.to_string()
                    && refs.drive_source_row_id == session.drive.as_ref().map(|row| row.id)
                    && refs.charge_source_row_id == session.charge.as_ref().map(|row| row.id)
                    && refs.state_source_row_id == session.state.as_ref().map(|row| row.id)
                    && refs.standalone_position_count == session.standalone_positions.len() as u64
            })
            && previous_open.as_ref().is_some_and(|old| old == session);
        if same_seed {
            return Ok(OpenSessionSeedReport {
                no_op: true,
                ..OpenSessionSeedReport::default()
            });
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if let Some((expected_last_observation_id, expected_updated_at_ms)) = expected {
            let actual: Option<(i64, i64)> = transaction
                .query_row(
                    "SELECT last_observation_id, updated_at_ms
                     FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                    params![vehicle_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(StoreError::LifecycleWrite)?;
            if actual != Some((expected_last_observation_id, expected_updated_at_ms)) {
                return Err(StoreError::ImportGenerationConflict);
            }
        }
        ensure_source_exists(&transaction, source_id)?;
        ensure_vehicle_source(&transaction, vehicle_id, source_id)?;

        let source = source_id.to_string();
        let vehicle = vehicle_id.to_string();
        transaction
            .execute(
                "DELETE FROM lifecycle_open_rows
                 WHERE source_id = ?1 AND vehicle_id = ?2",
                params![source, vehicle],
            )
            .map_err(StoreError::LifecycleWrite)?;
        transaction
            .execute(
                "DELETE FROM lifecycle_source_watermarks
                 WHERE source_id = ?1 AND vehicle_id = ?2",
                params![source, vehicle],
            )
            .map_err(StoreError::LifecycleWrite)?;
        let mut inserted = 0;
        if let Some(row) = &session.drive {
            inserted += insert_open_row(
                &transaction,
                &source,
                "drives",
                row.id,
                &vehicle,
                car_id,
                "drive",
                None,
                row,
            )?;
        }
        for row in &session.drive_positions {
            inserted += insert_open_row(
                &transaction,
                &source,
                "positions",
                row.id,
                &vehicle,
                car_id,
                "position",
                row.drive_id,
                row,
            )?;
        }
        if let Some(row) = &session.charge {
            inserted += insert_open_row(
                &transaction,
                &source,
                "charging_processes",
                row.id,
                &vehicle,
                car_id,
                "charge",
                None,
                row,
            )?;
        }
        for row in &session.charge_samples {
            inserted += insert_open_row(
                &transaction,
                &source,
                "charges",
                row.id,
                &vehicle,
                car_id,
                "charge_sample",
                Some(row.charging_process_id),
                row,
            )?;
        }
        if let Some(row) = &session.state {
            inserted += insert_open_row(
                &transaction,
                &source,
                "states",
                row.id,
                &vehicle,
                car_id,
                "state",
                None,
                row,
            )?;
        }
        for row in &session.standalone_positions {
            inserted += insert_open_row(
                &transaction,
                &source,
                "positions",
                row.id,
                &vehicle,
                car_id,
                "standalone_position",
                None,
                row,
            )?;
        }

        let mut standalone_positions_inserted = 0;
        for row in &session.standalone_positions {
            let position = crate::lifecycle::imported_position(row);
            let json =
                serde_json::to_string(&position).map_err(StoreError::SerializeLifecycleRow)?;
            standalone_positions_inserted += transaction
                .execute(
                    "INSERT INTO materialised_positions(
                        vehicle_id, position_id, drive_id, car_id, position_json,
                        speed, power, est_battery_range_km, fan_status,
                        driver_temp_setting, passenger_temp_setting,
                        is_climate_on, is_rear_defroster_on, is_front_defroster_on,
                        battery_heater, battery_heater_on, battery_heater_no_power,
                        tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr
                     ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                               ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                     ON CONFLICT(vehicle_id, position_id) DO NOTHING",
                    params![
                        vehicle,
                        position.id,
                        car_id,
                        json,
                        position.speed,
                        position.power,
                        position.est_battery_range_km,
                        position.fan_status,
                        position.driver_temp_setting,
                        position.passenger_temp_setting,
                        position.is_climate_on.map(i64::from),
                        position.is_rear_defroster_on.map(i64::from),
                        position.is_front_defroster_on.map(i64::from),
                        position.battery_heater.map(i64::from),
                        position.battery_heater_on.map(i64::from),
                        position.battery_heater_no_power.map(i64::from),
                        position.tpms_pressure_fl,
                        position.tpms_pressure_fr,
                        position.tpms_pressure_rl,
                        position.tpms_pressure_rr,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }

        let watermarks = [
            ("drives", session.watermarks.drives),
            ("positions", session.watermarks.positions),
            ("charging_processes", session.watermarks.charging_processes),
            ("charges", session.watermarks.charges),
            ("states", session.watermarks.states),
            ("updates", session.watermarks.updates),
        ];
        for (domain, watermark) in watermarks {
            transaction
                .execute(
                    "INSERT INTO lifecycle_source_watermarks(
                        source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(source_id, vehicle_id, domain) DO UPDATE SET
                        max_source_row_id = MAX(max_source_row_id, excluded.max_source_row_id),
                        max_timestamp_ms = MAX(max_timestamp_ms, excluded.max_timestamp_ms)",
                    params![
                        source,
                        vehicle,
                        domain,
                        watermark.max_id,
                        watermark.max_timestamp_ms
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        let json = seeded
            .encode()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        transaction
            .execute(
                "INSERT INTO vehicle_lifecycle_state(
                    vehicle_id, car_id, last_observation_id, open_session_json,
                    quarantined, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, 0, ?5)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    car_id = excluded.car_id,
                    open_session_json = excluded.open_session_json,
                    updated_at_ms = MAX(updated_at_ms, excluded.updated_at_ms)",
                params![
                    vehicle,
                    car_id,
                    previous
                        .as_ref()
                        .map_or(0, |record| record.last_observation_id),
                    json,
                    updated_at_ms,
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        mark_export_dirty_in_transaction(&transaction, vehicle_id)?;

        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(OpenSessionSeedReport {
            provisional_rows_inserted: inserted,
            standalone_positions_inserted,
            watermarks_written: watermarks.len(),
            no_op: false,
        })
    }

    /// Reconstruct the full imported open-session view after a restart.
    pub fn load_imported_open_session(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
    ) -> Result<Option<TeslaMateOpenSession>, StoreError> {
        if source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT domain, row_json FROM lifecycle_open_rows
                 WHERE source_id = ?1 AND vehicle_id = ?2
                 ORDER BY source_table, source_row_id",
            )
            .map_err(StoreError::Query)?;
        let mut session = TeslaMateOpenSession::default();
        let mut found = false;
        let rows = statement
            .query_map(
                params![source_id.to_string(), vehicle_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(StoreError::Query)?;
        for row in rows {
            let (domain, json) = row.map_err(StoreError::Query)?;
            found = true;
            match domain.as_str() {
                "drive" => {
                    session.drive = Some(
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                    )
                }
                "position" => session.drive_positions.push(
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                "charge" => {
                    session.charge = Some(
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                    )
                }
                "charge_sample" => session.charge_samples.push(
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                "state" => {
                    session.state = Some(
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                    )
                }
                "standalone_position" => session.standalone_positions.push(
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                _ => return Err(StoreError::InvalidLifecycleSession),
            }
        }
        let mut watermark_statement = connection
            .prepare(
                "SELECT domain, max_source_row_id, max_timestamp_ms
                 FROM lifecycle_source_watermarks
                 WHERE source_id = ?1 AND vehicle_id = ?2",
            )
            .map_err(StoreError::Query)?;
        let watermarks = watermark_statement
            .query_map(
                params![source_id.to_string(), vehicle_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        crate::teslamate_projection::TeslaMateSourceWatermark {
                            max_id: row.get(1)?,
                            max_timestamp_ms: row.get(2)?,
                        },
                    ))
                },
            )
            .map_err(StoreError::Query)?;
        for watermark in watermarks {
            let (domain, value) = watermark.map_err(StoreError::Query)?;
            match domain.as_str() {
                "drives" => session.watermarks.drives = value,
                "positions" => session.watermarks.positions = value,
                "charging_processes" => session.watermarks.charging_processes = value,
                "charges" => session.watermarks.charges = value,
                "states" => session.watermarks.states = value,
                "updates" => session.watermarks.updates = value,
                _ => return Err(StoreError::InvalidLifecycleSession),
            }
        }
        if !found {
            return Ok(None);
        }
        session.car_id = session
            .drive
            .as_ref()
            .map(|row| row.car_id)
            .or_else(|| session.charge.as_ref().map(|row| row.car_id))
            .or_else(|| session.state.as_ref().map(|row| row.car_id))
            .or_else(|| session.drive_positions.first().map(|row| row.car_id))
            .unwrap_or_default();
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        Ok(Some(session))
    }

    /// Preserve imported geofence labels and geometry as a durable, append-only
    /// catalog. Invalid geometry is skipped so unrelated history can proceed.
    pub fn upsert_geofences(
        &self,
        vehicle_id: Uuid,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<usize, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let mut inserted = 0;
        for geofence in geofences {
            let Some((latitude, longitude, radius_m)) = geofence.valid_geometry() else {
                continue;
            };
            if geofence.name.trim().is_empty() || geofence.name.len() > 256 {
                continue;
            }
            inserted += transaction
                .execute(
                    "INSERT INTO geofences(
                        vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                        billing_type, cost_per_unit, session_fee
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(vehicle_id, source_geofence_id) DO NOTHING",
                    params![
                        vehicle_id.to_string(),
                        geofence.id,
                        geofence.name.trim(),
                        latitude,
                        longitude,
                        radius_m,
                        geofence
                            .billing_type
                            .map(crate::hub_pack::GeofenceBillingType::as_str),
                        geofence.cost_per_unit,
                        geofence.session_fee,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "UPDATE geofences SET
                        name = ?3, latitude = ?4, longitude = ?5, radius_m = ?6,
                        billing_type = COALESCE(?7, billing_type),
                        cost_per_unit = COALESCE(?8, cost_per_unit),
                        session_fee = COALESCE(?9, session_fee)
                     WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
                    params![
                        vehicle_id.to_string(),
                        geofence.id,
                        geofence.name.trim(),
                        latitude,
                        longitude,
                        radius_m,
                        geofence
                            .billing_type
                            .map(crate::hub_pack::GeofenceBillingType::as_str),
                        geofence.cost_per_unit,
                        geofence.session_fee,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(inserted)
    }

    /// Persist open-session state and append newly completed history rows.
    pub fn commit_lifecycle_delta(&self, commit: &LifecycleCommit<'_>) -> Result<(), StoreError> {
        if commit.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if commit.car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }
        if commit.last_observation_id < 0 {
            return Err(StoreError::InvalidLifecycleCursor);
        }
        validate_timestamp("lifecycle updated_at_ms", commit.updated_at_ms)?;
        if commit.open_session_json.len() < 2 || commit.open_session_json.len() > 65_536 {
            return Err(StoreError::InvalidLifecycleSession);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        Self::commit_lifecycle_delta_in_transaction(&transaction, commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    fn maybe_stream_fault(&self, point: StreamFaultPoint) -> Result<(), StoreError> {
        #[cfg(test)]
        {
            let mut fault = self.stream_fault.lock().expect("stream fault lock");
            if fault.as_ref().is_some_and(|value| *value == point) {
                *fault = None;
                return Err(StoreError::InjectedStreamFault(point.label()));
            }
        }
        #[cfg(not(test))]
        let _ = point;
        Ok(())
    }

    #[cfg(test)]
    pub fn inject_stream_fault(&self, point: StreamFaultPoint) {
        *self.stream_fault.lock().expect("stream fault lock") = Some(point);
    }

    #[cfg(test)]
    pub(crate) fn inject_projection_state_detach_fault(&self) {
        *self
            .projection_state_detach_fault
            .lock()
            .expect("projection-state detach fault lock") = true;
    }

    fn commit_lifecycle_delta_in_transaction(
        transaction: &Transaction<'_>,
        commit: &LifecycleCommit<'_>,
    ) -> Result<(), StoreError> {
        let mut delta = commit.delta.clone();
        let session = crate::lifecycle::OpenSessionState::decode(commit.open_session_json)
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        let lifecycle_source_id: String = transaction
            .query_row(
                "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
                params![commit.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LifecycleWrite)?;
        let vehicle_key = commit.vehicle_id.to_string();
        for position in &delta.open_drive_positions {
            insert_open_row(
                transaction,
                &lifecycle_source_id,
                "positions",
                position.id,
                &vehicle_key,
                commit.car_id,
                "position",
                position.drive_id,
                position,
            )?;
        }
        for sample in &delta.open_charge_samples {
            insert_open_row(
                transaction,
                &lifecycle_source_id,
                "charges",
                sample.id,
                &vehicle_key,
                commit.car_id,
                "charge_sample",
                Some(sample.charge_process_id),
                sample,
            )?;
        }
        // When a drive/charge closes without a full in-memory child buffer
        // (incremental path), pull durable open children once for materialization.
        for drive in &delta.drives {
            let open_positions =
                load_open_positions_for_parent(transaction, &vehicle_key, drive.id)?;
            for position in open_positions {
                if !delta
                    .positions
                    .iter()
                    .any(|existing| existing.id == position.id)
                {
                    delta.positions.push(position);
                }
            }
        }
        for charge in &delta.charges {
            let open_samples =
                load_open_charge_samples_for_parent(transaction, &vehicle_key, charge.id)?;
            for sample in open_samples {
                if !delta
                    .charge_samples
                    .iter()
                    .any(|existing| existing.id == sample.id)
                {
                    delta.charge_samples.push(sample);
                }
            }
        }
        let fences = load_geofence_fences(transaction, commit.vehicle_id)?;
        crate::lifecycle::apply_geofence_labels(&mut delta, &fences);
        let free_supercharging = transaction
            .query_row(
                "SELECT free_supercharging FROM car_settings
                 WHERE vehicle_id = ?1 AND car_id = ?2",
                params![commit.vehicle_id.to_string(), commit.car_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(StoreError::LifecycleWrite)?
            .unwrap_or(0)
            != 0;

        if let Some(patch) = session.car_metadata.as_ref() {
            let existing_json: Option<String> = transaction
                .query_row(
                    "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                    params![commit.vehicle_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::LifecycleWrite)?;
            let existing = existing_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(StoreError::DeserializeLifecycleRow)?;
            let (fallback_name, fallback_vin): (Option<String>, Option<String>) = transaction
                .query_row(
                    "SELECT display_name, vin FROM vehicles WHERE vehicle_id = ?1",
                    params![commit.vehicle_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(StoreError::LifecycleWrite)?;
            let car = patch.into_car(
                commit.car_id,
                existing.as_ref(),
                fallback_name,
                fallback_vin,
            );
            let car_json =
                serde_json::to_string(&car).map_err(StoreError::SerializeLifecycleRow)?;
            let car_name = car.name.clone();
            let car_vin = car.vin.clone();
            transaction
                .execute(
                    "UPDATE vehicles SET display_name = COALESCE(?1, display_name), \
                         vin = COALESCE(?2, vin), last_seen_at_ms = MAX(last_seen_at_ms, ?3) \
                     WHERE vehicle_id = ?4",
                    params![
                        car_name,
                        car_vin,
                        commit.updated_at_ms,
                        commit.vehicle_id.to_string()
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            if existing.as_ref() != Some(&car) {
                transaction
                    .execute(
                        "INSERT INTO materialised_cars(vehicle_id, car_id, car_json) \
                         VALUES (?1, ?2, ?3) \
                         ON CONFLICT(vehicle_id) DO UPDATE SET \
                             car_id = excluded.car_id, car_json = excluded.car_json",
                        params![commit.vehicle_id.to_string(), car.id, car_json],
                    )
                    .map_err(StoreError::LifecycleWrite)?;
                record_sync_mutation_in_transaction(
                    transaction,
                    commit.vehicle_id,
                    "car",
                    car.id,
                    commit.car_id,
                    "upsert",
                    &car_json,
                )?;
            }
        }

        transaction
            .execute(
                "INSERT INTO vehicle_lifecycle_state(
                    vehicle_id, car_id, last_observation_id, open_session_json,
                    quarantined, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    car_id = excluded.car_id,
                    last_observation_id = excluded.last_observation_id,
                    open_session_json = excluded.open_session_json,
                    quarantined = excluded.quarantined,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    commit.vehicle_id.to_string(),
                    commit.car_id,
                    commit.last_observation_id,
                    commit.open_session_json,
                    i64::from(commit.quarantined),
                    commit.updated_at_ms,
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        mark_export_dirty_in_transaction(transaction, commit.vehicle_id)?;

        for drive in &delta.drives {
            let drive_json =
                serde_json::to_string(drive).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_drives(
                        vehicle_id, drive_id, car_id, drive_json,
                        inside_temp_avg, power_max, power_min,
                        start_ideal_range_km, end_ideal_range_km, ascent, descent
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                    ON CONFLICT(vehicle_id, drive_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        drive_json = excluded.drive_json,
                        inside_temp_avg = excluded.inside_temp_avg,
                        power_max = excluded.power_max,
                        power_min = excluded.power_min,
                        start_ideal_range_km = excluded.start_ideal_range_km,
                        end_ideal_range_km = excluded.end_ideal_range_km,
                        ascent = excluded.ascent,
                        descent = excluded.descent",
                    params![
                        commit.vehicle_id.to_string(),
                        drive.id,
                        commit.car_id,
                        drive_json,
                        drive.inside_temp_avg,
                        drive.power_max,
                        drive.power_min,
                        drive.start_ideal_range_km,
                        drive.end_ideal_range_km,
                        drive.ascent,
                        drive.descent
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "drive",
                drive.id,
                commit.car_id,
                "upsert",
                &drive_json,
            )?;
        }
        for position in &delta.positions {
            let position_json =
                serde_json::to_string(position).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_positions(
                        vehicle_id, position_id, drive_id, car_id, position_json,
                        speed, power, est_battery_range_km, fan_status,
                        driver_temp_setting, passenger_temp_setting,
                        is_climate_on, is_rear_defroster_on, is_front_defroster_on,
                        battery_heater, battery_heater_on, battery_heater_no_power,
                        tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                               ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                    params![
                        commit.vehicle_id.to_string(),
                        position.id,
                        position.drive_id,
                        commit.car_id,
                        position_json,
                        position.speed,
                        position.power,
                        position.est_battery_range_km,
                        position.fan_status,
                        position.driver_temp_setting,
                        position.passenger_temp_setting,
                        position.is_climate_on.map(i64::from),
                        position.is_rear_defroster_on.map(i64::from),
                        position.is_front_defroster_on.map(i64::from),
                        position.battery_heater.map(i64::from),
                        position.battery_heater_on.map(i64::from),
                        position.battery_heater_no_power.map(i64::from),
                        position.tpms_pressure_fl,
                        position.tpms_pressure_fr,
                        position.tpms_pressure_rl,
                        position.tpms_pressure_rr,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "position",
                position.id,
                commit.car_id,
                "upsert",
                &position_json,
            )?;
        }
        for charge in &delta.charges {
            let mut charge = charge.clone();
            let start_fence = delta
                .charge_start_coordinates
                .iter()
                .find(|(id, _, _)| *id == charge.id)
                .and_then(|(_, latitude, longitude)| {
                    crate::lifecycle::match_geofence(*latitude, *longitude, &fences)
                });
            if charge.geofence.is_none() {
                charge.geofence = start_fence.map(|fence| fence.name.clone());
            }
            if charge.billing_type.is_none() {
                charge.billing_type = start_fence.and_then(|fence| fence.billing_type);
            }
            if charge.cost_per_unit.is_none() {
                charge.cost_per_unit = start_fence.and_then(|fence| fence.cost_per_unit);
            }
            if charge.session_fee.is_none() {
                charge.session_fee = start_fence.and_then(|fence| fence.session_fee);
            }
            if charge.cost.is_none() {
                charge.cost = crate::lifecycle::calculate_charge_cost(
                    charge.fast_charger_type.as_deref(),
                    free_supercharging,
                    charge.charge_energy_added,
                    charge.charge_energy_used_kwh,
                    charge.duration_min,
                    start_fence.and_then(|fence| {
                        fence
                            .billing_type
                            .map(|billing_type| crate::lifecycle::ChargeTariff {
                                billing_type,
                                cost_per_unit: fence.cost_per_unit,
                                session_fee: fence.session_fee,
                            })
                    }),
                );
            }
            let charge_json =
                serde_json::to_string(&charge).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_charges(
                        vehicle_id, charge_id, car_id, charge_json
                    ) VALUES (?1, ?2, ?3, ?4)
                    ON CONFLICT(vehicle_id, charge_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        charge_json = excluded.charge_json",
                    params![
                        commit.vehicle_id.to_string(),
                        charge.id,
                        commit.car_id,
                        charge_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "charge",
                charge.id,
                commit.car_id,
                "upsert",
                &charge_json,
            )?;
        }
        for sample in &delta.charge_samples {
            let sample_json =
                serde_json::to_string(sample).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_charge_samples(
                        vehicle_id, sample_id, charge_id, sample_json
                    ) VALUES (?1, ?2, ?3, ?4)
                    ON CONFLICT(vehicle_id, sample_id) DO UPDATE SET
                        charge_id = excluded.charge_id,
                        sample_json = excluded.sample_json",
                    params![
                        commit.vehicle_id.to_string(),
                        sample.id,
                        sample.charge_process_id,
                        sample_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "charge_sample",
                sample.id,
                commit.car_id,
                "upsert",
                &sample_json,
            )?;
        }

        for drive in &delta.drives {
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'position'
                       AND parent_source_row_id = ?2",
                    params![vehicle_key, drive.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'drive' AND source_row_id = ?2",
                    params![vehicle_key, drive.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for charge in &delta.charges {
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'charge_sample'
                       AND parent_source_row_id = ?2",
                    params![vehicle_key, charge.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'charge' AND source_row_id = ?2",
                    params![vehicle_key, charge.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for state in &delta.states {
            if state.end_date_ms.is_some() {
                transaction
                    .execute(
                        "DELETE FROM lifecycle_open_rows
                         WHERE vehicle_id = ?1 AND domain = 'state' AND source_row_id = ?2",
                        params![vehicle_key, state.id],
                    )
                    .map_err(StoreError::LifecycleWrite)?;
            }
        }
        for state in &delta.states {
            let state_json =
                serde_json::to_string(state).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_states(
                        vehicle_id, state_id, car_id, state_json
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(vehicle_id, state_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        state_json = excluded.state_json",
                    params![
                        commit.vehicle_id.to_string(),
                        state.id,
                        commit.car_id,
                        state_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "state",
                state.id,
                commit.car_id,
                "upsert",
                &state_json,
            )?;
        }
        for update in &delta.updates {
            let update_json =
                serde_json::to_string(update).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_updates(
                        vehicle_id, update_id, car_id, update_json
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(vehicle_id, update_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        update_json = excluded.update_json",
                    params![
                        commit.vehicle_id.to_string(),
                        update.id,
                        commit.car_id,
                        update_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "update",
                update.id,
                commit.car_id,
                "upsert",
                &update_json,
            )?;
        }

        enqueue_address_jobs(transaction, commit.vehicle_id, &delta)?;

        Ok(())
    }

    /// Load completed history used when publishing a phone snapshot.
    pub fn materialised_history(
        &self,
        vehicle_id: Uuid,
    ) -> Result<MaterialisedHistory, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let vehicle_key = vehicle_id.to_string();

        let drives = load_json_rows(
            &connection,
            "SELECT drive_json FROM materialised_drives WHERE vehicle_id = ?1 ORDER BY drive_id ASC",
            &vehicle_key,
        )?;
        let positions = load_json_rows(
            &connection,
            "SELECT position_json FROM materialised_positions WHERE vehicle_id = ?1 ORDER BY position_id ASC",
            &vehicle_key,
        )?;
        let charges = load_json_rows(
            &connection,
            "SELECT charge_json FROM materialised_charges WHERE vehicle_id = ?1 ORDER BY charge_id ASC",
            &vehicle_key,
        )?;
        let charge_samples = load_json_rows(
            &connection,
            "SELECT sample_json FROM materialised_charge_samples WHERE vehicle_id = ?1 ORDER BY sample_id ASC",
            &vehicle_key,
        )?;
        let states = load_json_rows(
            &connection,
            "SELECT state_json FROM materialised_states WHERE vehicle_id = ?1 ORDER BY state_id ASC",
            &vehicle_key,
        )?;
        let updates = load_json_rows(
            &connection,
            "SELECT update_json FROM materialised_updates WHERE vehicle_id = ?1 ORDER BY update_id ASC",
            &vehicle_key,
        )?;
        let car = connection
            .query_row(
                "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                params![vehicle_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(StoreError::DeserializeLifecycleRow)?;
        Ok(MaterialisedHistory {
            car,
            drives,
            positions,
            charges,
            charge_samples,
            states,
            updates,
        })
    }

    pub fn terrain_candidates(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<TerrainCandidate>, StoreError> {
        let limit = i64::from(limit.min(1_000));
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT p.vehicle_id, p.position_json
                 FROM materialised_positions p
                 JOIN materialised_drives d
                   ON d.vehicle_id = p.vehicle_id AND d.drive_id = p.drive_id
                 LEFT JOIN terrain_elevation_provenance e
                   ON e.vehicle_id = p.vehicle_id AND e.position_id = p.position_id
                 LEFT JOIN terrain_enrichment_state c
                   ON c.vehicle_id = p.vehicle_id
                 WHERE json_extract(p.position_json, '$.elevation') IS NULL
                   AND (e.status IS NULL OR
                        (e.status = 'failed' AND COALESCE(e.retry_after_ms, 0) <= ?1))
                   AND (p.position_id > COALESCE(c.cursor_position_id, 0)
                        OR e.status = 'failed')
                   AND NOT EXISTS (
                       SELECT 1 FROM materialised_positions streamed
                       WHERE streamed.vehicle_id = p.vehicle_id
                         AND streamed.drive_id = p.drive_id
                         AND json_extract(streamed.position_json, '$.odometer') IS NOT NULL
                         AND json_extract(
                               streamed.position_json,
                               '$.ideal_battery_range_km'
                             ) IS NULL
                   )
                 ORDER BY p.vehicle_id ASC, p.position_id ASC
                 LIMIT ?2",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![now_ms, limit], |row| {
                let vehicle_id: String = row.get(0)?;
                let position_json: String = row.get(1)?;
                Ok((vehicle_id, position_json))
            })
            .map_err(StoreError::Query)?;
        rows.map(|row| {
            let (vehicle_id, position_json) = row.map_err(StoreError::Query)?;
            let vehicle_id =
                Uuid::parse_str(&vehicle_id).map_err(|_| StoreError::InvalidVehicleId)?;
            let position = serde_json::from_str(&position_json)
                .map_err(StoreError::DeserializeLifecycleRow)?;
            Ok(TerrainCandidate {
                vehicle_id,
                position,
            })
        })
        .collect()
    }

    pub fn record_terrain_failure(
        &self,
        candidate: &TerrainCandidate,
        error_code: &str,
        retry_after_ms: i64,
        attempted_at_ms: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        upsert_terrain_provenance(
            &transaction,
            candidate,
            None,
            None,
            None,
            None,
            "failed",
            Some(error_code),
            retry_after_ms,
            attempted_at_ms,
        )?;
        advance_terrain_cursor(&transaction, candidate, attempted_at_ms)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)
    }

    pub fn apply_terrain_result(
        &self,
        candidate: &TerrainCandidate,
        elevation_m: Option<i16>,
        tile_name: &str,
        tile_hash: &str,
        dataset_source: &str,
        dataset_version: &str,
        attempted_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_key = candidate.vehicle_id.to_string();
        let current_json: String = transaction
            .query_row(
                "SELECT position_json FROM materialised_positions
                 WHERE vehicle_id = ?1 AND position_id = ?2",
                params![vehicle_key, candidate.position.id],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let mut position: ProjectionPosition =
            serde_json::from_str(&current_json).map_err(StoreError::DeserializeLifecycleRow)?;
        let changed = position.elevation.is_none() && elevation_m.is_some();
        if changed {
            position.elevation = elevation_m.map(i64::from);
            let position_json =
                serde_json::to_string(&position).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_positions SET position_json = ?3
                     WHERE vehicle_id = ?1 AND position_id = ?2",
                    params![vehicle_key, position.id, position_json],
                )
                .map_err(StoreError::LifecycleWrite)?;
            if let Some(drive_id) = position.drive_id {
                recompute_terrain_drive(&transaction, &vehicle_key, drive_id)?;
                let drive_json: String = transaction
                    .query_row(
                        "SELECT drive_json FROM materialised_drives
                         WHERE vehicle_id = ?1 AND drive_id = ?2",
                        params![vehicle_key, drive_id],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::LifecycleWrite)?;
                record_sync_mutation_in_transaction(
                    &transaction,
                    candidate.vehicle_id,
                    "drive",
                    drive_id,
                    position.car_id,
                    "upsert",
                    &drive_json,
                )?;
            }
            record_sync_mutation_in_transaction(
                &transaction,
                candidate.vehicle_id,
                "position",
                position.id,
                position.car_id,
                "upsert",
                &position_json,
            )?;
        }
        upsert_terrain_provenance(
            &transaction,
            candidate,
            Some(tile_name),
            Some(tile_hash),
            Some(dataset_source),
            Some(dataset_version),
            if elevation_m.is_some() {
                "success"
            } else {
                "void"
            },
            None,
            0,
            attempted_at_ms,
        )?;
        advance_terrain_cursor(&transaction, candidate, attempted_at_ms)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(changed)
    }

    pub fn publish_terrain_revision(
        &self,
        vehicle_id: Uuid,
        cursor_key: &CursorKey,
        minimum_free_bytes: u64,
    ) -> Result<bool, StoreError> {
        if self.vehicle_has_v2_base(vehicle_id)? {
            // The durable terrain mutations are already in the live sync
            // journal. Once an immutable base exists, the normal export
            // outbox must publish those mutations as sparse deltas; creating
            // another full snapshot here would replace neither the base nor
            // its head and could incorrectly acknowledge the journal.
            return Ok(false);
        }
        let history = self.materialised_history(vehicle_id)?;
        let Some(car) = history.car.clone() else {
            return Err(StoreError::TerrainCarMissing(vehicle_id));
        };
        let connection = self.open()?;
        let (source_id, generation): (String, i64) = connection
            .query_row(
                "SELECT v.source_id, s.generation
                 FROM vehicles v JOIN sources s ON s.source_id = v.source_id
                 WHERE v.vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(StoreError::Query)?;
        let account_id = Uuid::parse_str(&source_id).map_err(|_| StoreError::InvalidSourceId)?;
        let generation =
            u64::try_from(generation).map_err(|_| StoreError::InvalidStoredSequence)?;
        let snapshot = ProjectionSnapshot {
            cars: vec![car],
            drives: history.drives,
            positions: history.positions,
            charges: history.charges,
            charge_samples: history.charge_samples,
        };
        let fingerprint = Sha256Digest::from_bytes(
            Sha256::digest(
                serde_json::to_vec(&(&snapshot, &history.states, &history.updates))
                    .map_err(StoreError::SerializeLifecycleRow)?,
            )
            .into(),
        );
        if self.snapshot_fingerprint_is_current(vehicle_id, fingerprint)? {
            return Ok(false);
        }
        // The collector invokes terrain publication under the same outer
        // publication gate as its outbox and lifecycle writes.
        let sequence = self.next_full_snapshot_sequence_while_gated(vehicle_id)?;
        let binding = ProjectionBinding {
            installation_id: self.installation_id()?,
            account_id,
            vehicle_id,
            generation,
            selected_car_id: snapshot.cars[0].id,
        };
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: binding.clone(),
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &snapshot,
        };
        let writer =
            ProjectionPackWriter::new(self.packs_dir()).with_minimum_free_bytes(minimum_free_bytes);
        let built = writer
            .write_full_snapshot_with_states_and_updates(
                &request,
                &history.states,
                &history.updates,
            )
            .map_err(StoreError::TerrainPack)?;
        let manifest = request
            .signed_manifest_with_states_and_updates(
                &built,
                &history.states,
                &history.updates,
                cursor_key,
            )
            .map_err(StoreError::TerrainPack)?;
        self.finalize_import_snapshot_with_binding(&manifest, fingerprint, &[], &binding)?;
        Ok(true)
    }

    /// Check database integrity, report quarantined lifecycle state, and remove
    /// orphaned transport packs that are not referenced in the manifest catalog.
    ///
    /// A quarantine is evidence of a semantic projection failure. Clearing it
    /// without reconstructing from the immutable journal would make a damaged
    /// cursor appear healthy, so this safe repair deliberately preserves it.
    pub fn repair(&self) -> Result<RepairReport, StoreError> {
        self.repair_at(retired_lineage_clock_ms()?)
    }

    fn repair_at(&self, now_ms: i64) -> Result<RepairReport, StoreError> {
        if now_ms < 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let _publication_gate = self.try_acquire_publication_gate()?;
        let connection = self.open()?;
        let retired_cleanup_cutoff = now_ms.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS);
        connection
            .execute(
                "DELETE FROM sync_retired_lineages WHERE expires_at_ms <= ?1",
                params![retired_cleanup_cutoff],
            )
            .map_err(StoreError::LineageCatalog)?;
        self.verify_referenced_packs_at(now_ms)?;
        let quarantined_sessions_preserved: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM vehicle_lifecycle_state WHERE quarantined != 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Query)?;
        let quarantined_sessions_preserved = usize::try_from(quarantined_sessions_preserved)
            .map_err(|_| StoreError::InvalidStoredCount)?;

        let mut catalog_shas = std::collections::HashSet::new();
        for (sha, _, _) in referenced_pack_rows_at(&connection, retired_cleanup_cutoff)? {
            catalog_shas.insert(sha);
        }

        let mut orphaned_packs_removed = 0;
        let mut freed_bytes = 0;
        for packs_dir in [
            self.packs_dir().to_path_buf(),
            self.packs_dir().join("sha256"),
        ] {
            if let Ok(entries) = std::fs::read_dir(packs_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let is_orphaned = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|name| name.strip_suffix(".sqlite.zst"))
                        .is_some_and(|sha| !catalog_shas.contains(sha));
                    if is_orphaned {
                        if let Ok(metadata) = entry.metadata() {
                            freed_bytes += metadata.len();
                        }
                        if std::fs::remove_file(&path).is_ok() {
                            orphaned_packs_removed += 1;
                        }
                    }
                }
            }
        }

        Ok(RepairReport {
            status: "ok".to_owned(),
            sqlite_integrity: "ok".to_owned(),
            quarantined_sessions_preserved,
            orphaned_packs_removed,
            freed_bytes,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct RepairReport {
    pub status: String,
    pub sqlite_integrity: String,
    pub quarantined_sessions_preserved: usize,
    pub orphaned_packs_removed: usize,
    pub freed_bytes: u64,
}

/// Reject a candidate import successor before it can participate in an
/// atomic batch. The caller still performs full typed-SQLite inspection and
/// immutable-binding verification; this helper keeps the wire-level lineage
/// invariants identical to the single-successor path.
fn validate_import_delta_successor_shape(delta: &LineageDelta) -> Result<(), StoreError> {
    delta
        .pack
        .validate(ProtocolLimits::default())
        .map_err(StoreError::Manifest)?;
    if delta.pack.snapshot_id.is_nil()
        || delta.from_sequence >= delta.to_sequence
        || delta.pack_digest != delta.pack.sha256
        || delta.pack.schema != HUB_PROJECTION_SCHEMA_V2
        || delta.pack.format != crate::protocol::PackFormat::HubProjectionSqlite
        || delta.pack.compression != crate::protocol::PackCompression::Zstd
        || delta.pack.sequence
            != (SequenceRange {
                from_exclusive: delta.from_sequence,
                to_inclusive: delta.to_sequence,
            })
        || delta.chain_digest
            != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack.sha256)
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

/// Insert exactly one already-verified typed delta without advancing the
/// lineage head. The enclosing batch finalizer advances the head only after
/// every pack, source-state replacement, lifecycle promotion, and fingerprint
/// update has succeeded in the same transaction.
fn insert_import_delta_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    delta: &LineageDelta,
) -> Result<(), StoreError> {
    let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
    let inserted = transaction
        .execute(
            "INSERT INTO sync_deltas(
                vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                chain_digest, pack_digest, pack_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(vehicle_id, from_sequence, to_sequence) DO NOTHING",
            params![
                vehicle_key,
                i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                delta.parent_chain_digest.to_string(),
                delta.chain_digest.to_string(),
                delta.pack_digest.to_string(),
                pack_json,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    if inserted != 1 {
        return Err(StoreError::LineageCatalogConflict);
    }
    HubStore::register_lineage_pack_snapshot(
        transaction,
        vehicle_key,
        &delta.pack,
        delta.to_sequence,
        &pack_json,
    )?;

    let existing_pack: Option<(String, i64, String, i64, i64)> = transaction
        .query_row(
            "SELECT snapshot_id, ordinal, relative_path,
                    compressed_bytes, uncompressed_bytes
             FROM sync_packs WHERE sha256 = ?1",
            params![delta.pack.sha256.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)) =
        existing_pack
    {
        if snapshot_id != delta.pack.snapshot_id.to_string()
            || ordinal != i64::from(delta.pack.ordinal)
            || relative_path != delta.pack.relative_path
            || compressed_bytes
                != i64::try_from(delta.pack.compressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?
            || uncompressed_bytes
                != i64::try_from(delta.pack.uncompressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        return Err(StoreError::LineageCatalogConflict);
    }
    let occupied: Option<String> = transaction
        .query_row(
            "SELECT sha256 FROM sync_packs
             WHERE snapshot_id = ?1 AND ordinal = ?2",
            params![
                delta.pack.snapshot_id.to_string(),
                i64::from(delta.pack.ordinal)
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if occupied.is_some() {
        return Err(StoreError::LineageCatalogConflict);
    }
    transaction
        .execute(
            "INSERT INTO sync_packs(
                sha256, snapshot_id, ordinal, relative_path,
                compressed_bytes, uncompressed_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                delta.pack.sha256.to_string(),
                delta.pack.snapshot_id.to_string(),
                i64::from(delta.pack.ordinal),
                delta.pack.relative_path,
                i64::try_from(delta.pack.compressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?,
                i64::try_from(delta.pack.uncompressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(())
}

fn load_json_rows<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    vehicle_id: &str,
) -> Result<Vec<T>, StoreError> {
    let mut statement = connection.prepare(sql).map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id], |row| row.get::<_, String>(0))
        .map_err(StoreError::Query)?;
    let mut values = Vec::new();
    for row in rows {
        let json = row.map_err(StoreError::Query)?;
        values.push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
    }
    Ok(values)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleStateRecord {
    pub vehicle_id: Uuid,
    pub car_id: i64,
    pub last_observation_id: i64,
    pub open_session_json: Vec<u8>,
    pub quarantined: bool,
    pub updated_at_ms: i64,
}

/// One transactional lifecycle write: open-session snapshot plus completed rows.
#[derive(Debug, Clone)]
pub struct LifecycleCommit<'a> {
    pub vehicle_id: Uuid,
    pub car_id: i64,
    pub open_session_json: &'a [u8],
    pub last_observation_id: i64,
    pub quarantined: bool,
    pub updated_at_ms: i64,
    pub delta: &'a crate::lifecycle::LifecycleDelta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOutboxClaim {
    pub vehicle_id: Uuid,
    pub dirty_revision: i64,
    pub attempts: i64,
    pub claimed_at_ms: i64,
    pub lease_until_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncMutation {
    pub vehicle_id: Uuid,
    pub revision: i64,
    pub entity: String,
    pub entity_id: i64,
    pub car_id: i64,
    pub operation: String,
    pub payload_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncMutationClaim {
    pub vehicle_id: Uuid,
    pub from_revision: i64,
    pub to_revision: i64,
    pub mutations: Vec<SyncMutation>,
}

/// Exact, read-only input for replacing a contiguous collector-owned delta
/// suffix. Import successors are intentionally never represented here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveDeltaCompactionSpan {
    pub delta: LineageDelta,
    pub from_revision: i64,
    pub to_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveDeltaCompactionPlan {
    pub vehicle_id: Uuid,
    pub base_snapshot_id: Uuid,
    pub anchor_sequence: u64,
    pub anchor_digest: Sha256Digest,
    pub head_sequence: u64,
    pub head_digest: Sha256Digest,
    pub first_ordinal: u32,
    pub from_revision: i64,
    pub to_revision: i64,
    pub mutations: Vec<SyncMutation>,
    pub replaced_spans: Vec<LiveDeltaCompactionSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenSessionSeedReport {
    pub provisional_rows_inserted: usize,
    pub standalone_positions_inserted: usize,
    pub watermarks_written: usize,
    pub no_op: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MaterialisedHistory {
    pub car: Option<crate::hub_pack::ProjectionCar>,
    pub drives: Vec<crate::hub_pack::ProjectionDrive>,
    pub positions: Vec<crate::hub_pack::ProjectionPosition>,
    pub charges: Vec<crate::hub_pack::ProjectionCharge>,
    pub charge_samples: Vec<crate::hub_pack::ProjectionChargeSample>,
    pub states: Vec<crate::hub_pack::ProjectionState>,
    pub updates: Vec<crate::hub_pack::ProjectionUpdate>,
}

/// Non-secret source identity presented by an independent collector. The Hub
/// persists a generated UUID for this pair so restarts never change identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDescriptor {
    pub kind: String,
    pub key: String,
}

impl SourceDescriptor {
    pub fn new(kind: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            key: key.into(),
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        validate_identity("source kind", &self.kind, MAX_SOURCE_KIND_BYTES)?;
        if !self.kind.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        }) {
            return Err(StoreError::InvalidSourceKind);
        }
        validate_identity("source key", &self.key, MAX_SOURCE_KEY_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub source_id: Uuid,
    pub kind: String,
    pub key: String,
    pub generation: u64,
    pub created_at_ms: i64,
}

/// Source-owned stable vehicle identity and optional mutable display fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleDescriptor {
    pub source_id: Uuid,
    pub source_vehicle_key: String,
    pub vin: Option<String>,
    pub display_name: Option<String>,
    pub tesla_eid: Option<i64>,
    pub tesla_vid: Option<i64>,
}

impl VehicleDescriptor {
    pub fn new(source_id: Uuid, source_vehicle_key: impl Into<String>) -> Self {
        Self {
            source_id,
            source_vehicle_key: source_vehicle_key.into(),
            vin: None,
            display_name: None,
            tesla_eid: None,
            tesla_vid: None,
        }
    }

    pub fn with_tesla_identity(mut self, eid: Option<i64>, vid: Option<i64>) -> Self {
        self.tesla_eid = eid;
        self.tesla_vid = vid;
        self
    }

    fn validate(&self) -> Result<(), StoreError> {
        if self.source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        validate_identity(
            "source vehicle key",
            &self.source_vehicle_key,
            MAX_VEHICLE_KEY_BYTES,
        )?;
        if let Some(vin) = &self.vin {
            validate_identity("vehicle VIN", vin, MAX_VIN_BYTES)?;
        }
        if let Some(display_name) = &self.display_name {
            validate_identity("vehicle display name", display_name, MAX_DISPLAY_NAME_BYTES)?;
        }
        if self.tesla_eid.is_some_and(|value| value <= 0)
            || self.tesla_vid.is_some_and(|value| value <= 0)
        {
            return Err(StoreError::InvalidVehicleIdentity);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleRecord {
    pub vehicle_id: Uuid,
    pub source_id: Uuid,
    pub source_vehicle_key: String,
    pub vin: Option<String>,
    pub display_name: Option<String>,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
}

/// Exact alias-free rows used until the first exported TeslaMate snapshot
/// proves the pre-read VIN/EID/VID tuple. Ordinary rejection removes unchanged
/// provisional rows; crash residue remains non-published and is reused by the
/// same deterministic source/vehicle registration on retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeslaMateIdentityRegistrationCheckpoint {
    source: SourceRecord,
    source_created: bool,
    vehicle: VehicleRecord,
    vehicle_created: bool,
}

/// One collector-provided raw source response. The Hub accepts JSON objects
/// only; a response batch belongs as independent observations, not an array.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservationInput {
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub observed_at_ms: i64,
    pub payload: Value,
}

impl ObservationInput {
    fn validate(&self) -> Result<(), StoreError> {
        if self.source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if self.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        validate_timestamp("observed_at_ms", self.observed_at_ms)?;
        if !self.payload.is_object() {
            return Err(StoreError::ObservationMustBeObject);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservationRecord {
    pub observation_id: i64,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub observed_at_ms: i64,
    pub received_at_ms: i64,
    pub payload_sha256: Sha256Digest,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservationTarget {
    vehicle_id: Uuid,
    source_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationWatermark {
    pub source_car_id: i64,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub observation_id: i64,
    pub observed_at_ms: Option<i64>,
    pub received_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationVerification {
    pub source_car_id: i64,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub after_observation_id: i64,
    pub latest_observation_id: Option<i64>,
    pub latest_observed_at_ms: Option<i64>,
    pub latest_received_at_ms: Option<i64>,
}

impl ObservationVerification {
    pub fn verified(&self) -> bool {
        self.latest_observation_id.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutboundRequestReceiptId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamSessionReceiptId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSessionTerminalOutcome {
    CancelledBeforeSubscription,
    TransportEnded,
    Failed,
}

impl StreamSessionTerminalOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CancelledBeforeSubscription => "cancelled_before_subscription",
            Self::TransportEnded => "transport_ended",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundRequestWatermark {
    pub receipt_id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestTransport {
    OwnerApi,
    Stream,
    LegacyAuth,
}

impl OutboundRequestTransport {
    const fn as_str(self) -> &'static str {
        match self {
            Self::OwnerApi => "owner_api",
            Self::Stream => "stream",
            Self::LegacyAuth => "legacy_auth",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "owner_api" => Some(Self::OwnerApi),
            "stream" => Some(Self::Stream),
            "legacy_auth" => Some(Self::LegacyAuth),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestOperation {
    Products,
    VehicleProbe,
    VehicleData,
    TokenRefresh,
    StreamConnect,
    StreamSubscribe,
    StreamUnsubscribe,
}

impl OutboundRequestOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Products => "products",
            Self::VehicleProbe => "vehicle_probe",
            Self::VehicleData => "vehicle_data",
            Self::TokenRefresh => "token_refresh",
            Self::StreamConnect => "stream_connect",
            Self::StreamSubscribe => "stream_subscribe",
            Self::StreamUnsubscribe => "stream_unsubscribe",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "products" => Some(Self::Products),
            "vehicle_probe" => Some(Self::VehicleProbe),
            "vehicle_data" => Some(Self::VehicleData),
            "token_refresh" => Some(Self::TokenRefresh),
            "stream_connect" => Some(Self::StreamConnect),
            "stream_subscribe" => Some(Self::StreamSubscribe),
            "stream_unsubscribe" => Some(Self::StreamUnsubscribe),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestSafetyClass {
    NonWakeEndpoint,
    ConditionalRead,
    DirectWakeCommand,
}

impl OutboundRequestSafetyClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NonWakeEndpoint => "non_wake_endpoint",
            Self::ConditionalRead => "conditional_read",
            Self::DirectWakeCommand => "direct_wake_command",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "non_wake_endpoint" => Some(Self::NonWakeEndpoint),
            "conditional_read" => Some(Self::ConditionalRead),
            "direct_wake_command" => Some(Self::DirectWakeCommand),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestPrecondition {
    NotRequired,
    StreamPowerConfirmed,
}

impl OutboundRequestPrecondition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::StreamPowerConfirmed => "stream_power_confirmed",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "not_required" => Some(Self::NotRequired),
            "stream_power_confirmed" => Some(Self::StreamPowerConfirmed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestOutcome {
    Success,
    HttpError,
    Timeout,
    TransportError,
    AuthenticationRejected,
    ProtocolError,
    ResponseTooLarge,
    Cancelled,
}

impl OutboundRequestOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::HttpError => "http_error",
            Self::Timeout => "timeout",
            Self::TransportError => "transport_error",
            Self::AuthenticationRejected => "authentication_rejected",
            Self::ProtocolError => "protocol_error",
            Self::ResponseTooLarge => "response_too_large",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "success" => Some(Self::Success),
            "http_error" => Some(Self::HttpError),
            "timeout" => Some(Self::Timeout),
            "transport_error" => Some(Self::TransportError),
            "authentication_rejected" => Some(Self::AuthenticationRejected),
            "protocol_error" => Some(Self::ProtocolError),
            "response_too_large" => Some(Self::ResponseTooLarge),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Typed metadata committed before network I/O. There is deliberately no URL,
/// header, token, request body, response body, or arbitrary error-text field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequestStart {
    pub correlation_id: Uuid,
    pub vehicle_tesla_id: Option<i64>,
    pub transport: OutboundRequestTransport,
    pub operation: OutboundRequestOperation,
    pub safety_class: OutboundRequestSafetyClass,
    pub precondition: OutboundRequestPrecondition,
}

impl OutboundRequestStart {
    fn validate(&self) -> Result<(), StoreError> {
        if self.correlation_id.is_nil() {
            return Err(StoreError::NilOutboundRequestCorrelationId);
        }
        if self.vehicle_tesla_id.is_some_and(|id| id <= 0) {
            return Err(StoreError::InvalidOutboundRequestVehicleId);
        }
        if self.operation == OutboundRequestOperation::VehicleData
            && (self.safety_class != OutboundRequestSafetyClass::ConditionalRead
                || self.precondition != OutboundRequestPrecondition::StreamPowerConfirmed)
        {
            return Err(StoreError::InvalidVehicleDataAuditPrecondition);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequestCompletion {
    pub outcome: OutboundRequestOutcome,
    pub http_status: Option<u16>,
    pub retry_after_seconds: Option<u64>,
}

impl OutboundRequestCompletion {
    fn validate(&self) -> Result<(), StoreError> {
        if self
            .http_status
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(StoreError::InvalidOutboundRequestHttpStatus);
        }
        if self
            .retry_after_seconds
            .is_some_and(|seconds| i64::try_from(seconds).is_err())
        {
            return Err(StoreError::InvalidOutboundRequestRetryAfter);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequestReceipt {
    pub id: OutboundRequestReceiptId,
    pub correlation_id: Uuid,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub vehicle_tesla_id: Option<i64>,
    pub transport: OutboundRequestTransport,
    pub operation: OutboundRequestOperation,
    pub safety_class: OutboundRequestSafetyClass,
    pub precondition: OutboundRequestPrecondition,
    pub outcome: Option<OutboundRequestOutcome>,
    pub http_status: Option<u16>,
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoWakeVerification {
    pub after_receipt_id: i64,
    pub correlation_id: Uuid,
    pub matching_receipts: i64,
    pub unresolved_receipts: i64,
    pub unresolved_stream_sessions: i64,
    pub direct_wake_receipts: i64,
    pub conditional_without_power_receipts: i64,
    pub observation: Option<ObservationVerification>,
}

impl NoWakeVerification {
    /// An empty audit window is not proof: absence of integration data fails closed.
    pub fn audit_verified(&self) -> bool {
        self.matching_receipts > 0
            && self.unresolved_receipts == 0
            && self.unresolved_stream_sessions == 0
            && self.direct_wake_receipts == 0
            && self.conditional_without_power_receipts == 0
    }
    pub fn verified(&self) -> bool {
        self.audit_verified()
            && self
                .observation
                .as_ref()
                .is_none_or(ObservationVerification::verified)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppendObservation {
    pub observation: ObservationRecord,
    pub inserted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnerObservationResult {
    pub append: AppendObservation,
    pub drives_closed: usize,
    pub charges_closed: usize,
    pub positions_materialised: usize,
    pub charge_samples_materialised: usize,
    pub lifecycle_quarantined: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationQuery {
    pub from_observed_at_ms: Option<i64>,
    pub until_observed_at_ms: Option<i64>,
    pub limit: u32,
}

impl ObservationQuery {
    pub const fn from_start(limit: u32) -> Self {
        Self {
            from_observed_at_ms: None,
            until_observed_at_ms: None,
            limit,
        }
    }

    fn validate(self) -> Result<(), StoreError> {
        if self.limit == 0 || self.limit > MAX_OBSERVATION_QUERY_LIMIT {
            return Err(StoreError::InvalidObservationQueryLimit {
                actual: self.limit,
                maximum: MAX_OBSERVATION_QUERY_LIMIT,
            });
        }
        if let Some(timestamp) = self.from_observed_at_ms {
            validate_timestamp("observation query lower bound", timestamp)?;
        }
        if let Some(timestamp) = self.until_observed_at_ms {
            validate_timestamp("observation query upper bound", timestamp)?;
        }
        if let (Some(from), Some(until)) = (self.from_observed_at_ms, self.until_observed_at_ms)
            && from >= until
        {
            return Err(StoreError::InvalidObservationQueryRange);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StoredPack {
    pub digest: Sha256Digest,
    pub compressed_bytes: u64,
    pub path: PathBuf,
}

/// Wire validity is not sufficient to publish a pack. Schema 2.2 deliberately
/// remains protocol-recognized while its Hub writer, catalogue, and receiver
/// are incomplete, so all catalogue entry points share this fail-closed gate.
fn validate_manifest_for_catalogue(manifest: &SyncManifest) -> Result<(), StoreError> {
    manifest.validate().map_err(StoreError::Manifest)?;
    validate_schema_for_catalogue(manifest.schema)?;
    Ok(())
}

fn validate_schema_for_catalogue(schema: crate::protocol::SchemaVersion) -> Result<(), StoreError> {
    match schema.support() {
        Some(
            SchemaSupport::GenericTransport
            | SchemaSupport::TypedHubProjection
            | SchemaSupport::FullSnapshotOnlyHubProjection,
        ) => Ok(()),
        None => Err(StoreError::SchemaPublicationUnavailable(schema)),
    }
}

/// A catalogue row may describe the legacy single-manifest shape, the
/// additive lineage envelope, or one persisted lineage successor.  All three
/// are immutable evidence for an active pack, so pack serving must validate
/// the actual envelope rather than assuming the legacy shape.
fn validate_catalogued_pack_manifest(payload: &[u8]) -> Result<(), StoreError> {
    match serde_json::from_slice::<SyncManifest>(payload) {
        Ok(manifest) => validate_manifest_for_catalogue(&manifest),
        Err(sync_error) => match serde_json::from_slice::<LineageManifestV2>(payload) {
            Ok(lineage) => {
                lineage
                    .validate_with_limits(ProtocolLimits::default())
                    .map_err(StoreError::Manifest)?;
                validate_schema_for_catalogue(lineage.schema)
            }
            Err(_) => {
                let delta: LineageDelta = serde_json::from_slice(payload)
                    .map_err(|_| StoreError::DeserializeManifest(sync_error))?;
                delta
                    .pack
                    .validate(ProtocolLimits::default())
                    .map_err(StoreError::Manifest)?;
                if delta.from_sequence >= delta.to_sequence
                    || delta.pack_digest != delta.pack.sha256
                    || delta.pack.sequence
                        != (SequenceRange {
                            from_exclusive: delta.from_sequence,
                            to_inclusive: delta.to_sequence,
                        })
                    || delta.chain_digest
                        != canonical_delta_chain_digest(
                            delta.parent_chain_digest,
                            delta.pack.sha256,
                        )
                {
                    return Err(StoreError::LineageCatalogConflict);
                }
                validate_schema_for_catalogue(delta.pack.schema)
            }
        },
    }
}

fn decode_manifest(payload: Vec<u8>) -> Result<SyncManifest, StoreError> {
    let manifest: SyncManifest =
        serde_json::from_slice(&payload).map_err(StoreError::DeserializeManifest)?;
    validate_manifest_for_catalogue(&manifest)?;
    Ok(manifest)
}

fn verify_transport_pack_catalogue_binding(
    catalogue: &HashMap<String, (String, i64, String, i64, i64)>,
    pack: &TransportPack,
) -> Result<(), StoreError> {
    let expected = (
        pack.snapshot_id.to_string(),
        i64::from(pack.ordinal),
        pack.relative_path.clone(),
        i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
        i64::try_from(pack.uncompressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
    );
    if catalogue.get(&pack.sha256.to_string()) == Some(&expected) {
        Ok(())
    } else {
        Err(StoreError::LineageCatalogConflict)
    }
}

fn validate_identity(
    field: &'static str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), StoreError> {
    if value.is_empty() {
        return Err(StoreError::EmptyIdentity(field));
    }
    if value.len() > maximum_bytes {
        return Err(StoreError::IdentityTooLong {
            field,
            actual: value.len(),
            maximum: maximum_bytes,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(StoreError::IdentityControlCharacter(field));
    }
    Ok(())
}

fn validate_timestamp(field: &'static str, timestamp_ms: i64) -> Result<(), StoreError> {
    if timestamp_ms < 0 {
        return Err(StoreError::NegativeTimestamp(field));
    }
    Ok(())
}

fn supervised_collector_lease_deadline(now_ms: i64) -> Result<i64, StoreError> {
    now_ms
        .checked_add(SUPERVISED_COLLECTOR_LEASE_MS)
        .ok_or(StoreError::SupervisedCollectorClockOverflow)
}

fn find_source(
    transaction: &rusqlite::Transaction<'_>,
    descriptor: &SourceDescriptor,
) -> Result<Option<SourceRecord>, StoreError> {
    let row = transaction
        .query_row(
            "SELECT sources.source_id, sources.source_kind, source_identities.source_key, \
                    sources.generation, sources.created_at_ms \
             FROM sources \
             JOIN source_identities USING (source_id) \
             WHERE source_identities.source_kind = ?1 AND source_identities.source_key = ?2",
            params![descriptor.kind, descriptor.key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::Query)?;
    row.map(source_from_columns).transpose()
}

fn source_from_columns(
    columns: (String, String, String, i64, i64),
) -> Result<SourceRecord, StoreError> {
    let (source_id, kind, key, generation, created_at_ms) = columns;
    Ok(SourceRecord {
        source_id: parse_stored_uuid("source_id", &source_id)?,
        kind,
        key,
        generation: u64::try_from(generation).map_err(|_| StoreError::InvalidStoredGeneration)?,
        created_at_ms,
    })
}

fn ensure_source_exists(
    transaction: &rusqlite::Transaction<'_>,
    source_id: Uuid,
) -> Result<(), StoreError> {
    let found = transaction
        .query_row(
            "SELECT 1 FROM sources WHERE source_id = ?1",
            params![source_id.to_string()],
            |_| Ok(()),
        )
        .optional()
        .map_err(StoreError::Query)?;
    found.ok_or(StoreError::UnknownSource(source_id))
}

fn find_vehicle(
    transaction: &rusqlite::Transaction<'_>,
    source_id: Uuid,
    source_vehicle_key: &str,
) -> Result<Option<VehicleRecord>, StoreError> {
    let row = transaction
        .query_row(
            "SELECT vehicle_id, source_id, source_vehicle_key, vin, display_name, \
                    created_at_ms, last_seen_at_ms \
             FROM vehicles \
             WHERE source_id = ?1 AND source_vehicle_key = ?2",
            params![source_id.to_string(), source_vehicle_key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::Query)?;
    row.map(vehicle_from_columns).transpose()
}

fn find_vehicle_by_id(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<Option<VehicleRecord>, StoreError> {
    let row = transaction
        .query_row(
            "SELECT vehicle_id, source_id, source_vehicle_key, vin, display_name,
                    created_at_ms, last_seen_at_ms
             FROM vehicles WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::Query)?;
    row.map(vehicle_from_columns).transpose()
}

fn find_identity_vehicle(
    transaction: &rusqlite::Transaction<'_>,
    descriptor: &VehicleDescriptor,
) -> Result<Option<Uuid>, StoreError> {
    let mut strong = Vec::new();
    let mut secondary = Vec::new();
    let mut statement = transaction
        .prepare("SELECT alias_kind, vehicle_id FROM vehicle_identity_aliases WHERE alias_kind IN ('tesla_eid', 'tesla_vid', 'vin') AND alias_value = ?1")
        .map_err(StoreError::Query)?;
    let mut find = |kind: &str, value: String| -> Result<(), StoreError> {
        let mut rows = statement.query(params![value]).map_err(StoreError::Query)?;
        while let Some(row) = rows.next().map_err(StoreError::Query)? {
            let found_kind: String = row.get(0).map_err(StoreError::Query)?;
            let id = parse_stored_uuid(
                "vehicle_id",
                &row.get::<_, String>(1).map_err(StoreError::Query)?,
            )?;
            if found_kind == kind && !strong.contains(&id) && !secondary.contains(&id) {
                if kind == "tesla_vid" {
                    secondary.push(id);
                } else {
                    strong.push(id);
                }
            }
        }
        Ok(())
    };
    if let Some(eid) = descriptor.tesla_eid {
        find("tesla_eid", eid.to_string())?;
    }
    if let Some(vin) = &descriptor.vin {
        find("vin", vin.clone())?;
    }
    if let Some(vid) = descriptor.tesla_vid {
        find("tesla_vid", vid.to_string())?;
    }
    if strong.len() > 1 || (!strong.is_empty() && secondary.iter().any(|id| !strong.contains(id))) {
        return Err(StoreError::VehicleIdentityConflict);
    }
    if strong.len() == 1 {
        return Ok(strong.into_iter().next());
    }
    if descriptor.tesla_eid.is_some() || descriptor.vin.is_some() {
        return Ok(None);
    }
    if secondary.len() > 1 {
        return Err(StoreError::VehicleIdentityConflict);
    }
    Ok(secondary.into_iter().next())
}

fn register_vehicle_aliases(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    descriptor: &VehicleDescriptor,
) -> Result<(), StoreError> {
    let mut aliases = vec![(
        "source_key",
        format!("{}:{}", descriptor.source_id, descriptor.source_vehicle_key),
    )];
    if let Some(eid) = descriptor.tesla_eid {
        aliases.push(("tesla_eid", eid.to_string()));
    }
    if let Some(vin) = &descriptor.vin {
        aliases.push(("vin", vin.clone()));
    }
    if let Some(vid) = descriptor.tesla_vid {
        aliases.push(("tesla_vid", vid.to_string()));
    }
    for (kind, value) in aliases {
        let conflict: Option<String> = transaction
            .query_row(
                "SELECT vehicle_id FROM vehicle_identity_aliases WHERE alias_kind = ?1 AND alias_value = ?2",
                params![kind, value], |row| row.get(0),
            ).optional().map_err(StoreError::Query)?;
        if let Some(existing) = conflict
            && existing != vehicle_id.to_string()
        {
            if kind == "tesla_vid" && (descriptor.tesla_eid.is_some() || descriptor.vin.is_some()) {
                continue;
            }
            return Err(StoreError::VehicleIdentityConflict);
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO vehicle_identity_aliases
             (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
             VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    kind,
                    value,
                    vehicle_id.to_string(),
                    descriptor.source_id.to_string(),
                    descriptor.source_vehicle_key
                ],
            )
            .map_err(StoreError::RegisterVehicle)?;
    }
    Ok(())
}

fn vehicle_from_columns(
    columns: (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
        i64,
    ),
) -> Result<VehicleRecord, StoreError> {
    let (
        vehicle_id,
        source_id,
        source_vehicle_key,
        vin,
        display_name,
        created_at_ms,
        last_seen_at_ms,
    ) = columns;
    Ok(VehicleRecord {
        vehicle_id: parse_stored_uuid("vehicle_id", &vehicle_id)?,
        source_id: parse_stored_uuid("source_id", &source_id)?,
        source_vehicle_key,
        vin,
        display_name,
        created_at_ms,
        last_seen_at_ms,
    })
}

fn ensure_vehicle_belongs_to_source(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
) -> Result<(), StoreError> {
    let belongs: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM vehicle_identity_aliases WHERE vehicle_id = ?1 AND source_id = ?2",
            params![vehicle_id.to_string(), source_id.to_string()],
            |_| Ok(1),
        )
        .optional()
        .map_err(StoreError::Query)?;
    if belongs.is_none() {
        let exists: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM vehicles WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |_| Ok(1),
            )
            .optional()
            .map_err(StoreError::Query)?;
        return if exists.is_some() {
            Err(StoreError::VehicleSourceMismatch {
                vehicle_id,
                source_id,
            })
        } else {
            Err(StoreError::UnknownVehicle(vehicle_id))
        };
    }
    Ok(())
}

fn find_observation(
    transaction: &rusqlite::Transaction<'_>,
    source_id: Uuid,
    vehicle_id: Uuid,
    observed_at_ms: i64,
    payload_sha256: Sha256Digest,
) -> Result<Option<ObservationRecord>, StoreError> {
    transaction
        .query_row(
            "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms, \
                    payload_sha256, payload_json \
             FROM raw_observations \
             WHERE source_id = ?1 AND vehicle_id = ?2 AND observed_at_ms = ?3 \
               AND payload_sha256 = ?4",
            params![
                source_id.to_string(),
                vehicle_id.to_string(),
                observed_at_ms,
                payload_sha256.as_bytes().as_slice(),
            ],
            observation_from_row,
        )
        .optional()
        .map_err(StoreError::Query)
}

fn observation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObservationRecord> {
    use rusqlite::types::Type;

    let source_id: String = row.get(1)?;
    let vehicle_id: String = row.get(2)?;
    let payload_sha256: Vec<u8> = row.get(5)?;
    let payload_json: String = row.get(6)?;
    let source_id = Uuid::parse_str(&source_id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(error))
    })?;
    let vehicle_id = Uuid::parse_str(&vehicle_id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
    })?;
    let digest: [u8; 32] = payload_sha256.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            Type::Blob,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stored SHA-256 digest does not have 32 bytes",
            )),
        )
    })?;
    let payload = serde_json::from_str(&payload_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, Type::Text, Box::new(error))
    })?;
    Ok(ObservationRecord {
        observation_id: row.get(0)?,
        source_id,
        vehicle_id,
        observed_at_ms: row.get(3)?,
        received_at_ms: row.get(4)?,
        payload_sha256: Sha256Digest::from_bytes(digest),
        payload,
    })
}

fn paired_device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PairedDeviceRecord> {
    use rusqlite::types::Type;

    let device_id: String = row.get(0)?;
    let device_id = Uuid::parse_str(&device_id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error))
    })?;
    Ok(PairedDeviceRecord {
        device_id,
        display_name: row.get(1)?,
        created_at_ms: row.get(2)?,
        last_authenticated_at_ms: row.get(3)?,
    })
}

fn random_secret_wire() -> String {
    let mut bytes = Zeroizing::new([0_u8; PAIRING_SECRET_BYTES]);
    getrandom::getrandom(&mut *bytes).expect("operating system entropy for pairing credential");
    hex::encode(bytes.as_slice())
}

fn sha256_bytes(value: &[u8]) -> [u8; PAIRING_SECRET_BYTES] {
    Sha256::digest(value).into()
}

fn digest_valid_wire_secret(value: &str) -> Option<[u8; PAIRING_SECRET_BYTES]> {
    if value.len() != PAIRING_SECRET_BYTES * 2
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    // Length plus the ASCII-hex predicate fully validates the wire shape.
    // Avoid decoding a second credential-equivalent byte buffer just to
    // validate text that is hashed exactly as received.
    Some(sha256_bytes(value.as_bytes()))
}

fn constant_time_equal(
    left: &[u8; PAIRING_SECRET_BYTES],
    right: &[u8; PAIRING_SECRET_BYTES],
) -> bool {
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn parse_stored_uuid(field: &'static str, value: &str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::InvalidStoredUuid(field))
}

fn ensure_installation_id(connection: &Connection) -> Result<Uuid, StoreError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(StoreError::Begin)?;
    let existing: Option<String> = transaction
        .query_row(
            "SELECT value FROM hub_metadata WHERE key = ?1",
            params![INSTALLATION_ID_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::InstallationIdentity)?;
    let value = match existing {
        Some(value) => value,
        None => {
            let value = Uuid::new_v4().to_string();
            transaction
                .execute(
                    "INSERT INTO hub_metadata (key, value) VALUES (?1, ?2)",
                    params![INSTALLATION_ID_KEY, value],
                )
                .map_err(StoreError::InstallationIdentity)?;
            value
        }
    };
    transaction
        .commit()
        .map_err(StoreError::InstallationIdentity)?;
    parse_stored_uuid("installation_id", &value)
}

fn configure(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = FULL;
            PRAGMA foreign_keys = ON;
            PRAGMA trusted_schema = OFF;
            PRAGMA busy_timeout = 5000;
            PRAGMA application_id = 1413564501;
            ",
        )
        .map_err(StoreError::Configure)
}

fn configure_read_only(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute_batch(
            "
            PRAGMA query_only = ON;
            PRAGMA foreign_keys = ON;
            PRAGMA trusted_schema = OFF;
            PRAGMA busy_timeout = 5000;
            ",
        )
        .map_err(StoreError::Configure)
}

#[derive(Debug, Clone, Copy)]
struct ObservationMetadata {
    observation_id: i64,
    observed_at_ms: i64,
    received_at_ms: i64,
}

fn latest_observation_metadata(
    connection: &Connection,
    vehicle_id: Uuid,
    after_observation_id: Option<i64>,
) -> Result<Option<ObservationMetadata>, StoreError> {
    connection
        .query_row(
            "SELECT observation_id, observed_at_ms, received_at_ms
             FROM raw_observations
             WHERE vehicle_id = ?1
               AND (?2 IS NULL OR observation_id > ?2)
             ORDER BY observation_id DESC LIMIT 1",
            params![vehicle_id.to_string(), after_observation_id],
            |row| {
                Ok(ObservationMetadata {
                    observation_id: row.get(0)?,
                    observed_at_ms: row.get(1)?,
                    received_at_ms: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::Query)
}

fn migrate(connection: &Connection) -> Result<(), StoreError> {
    let mut version = schema_version(connection)?;
    if version == 0 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_ledger (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    sequence INTEGER NOT NULL CHECK (sequence >= 1),
                    entity_kind TEXT NOT NULL,
                    entity_key TEXT NOT NULL,
                    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
                    committed_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (source_id, sequence, entity_kind, entity_key)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_ledger_source_sequence
                    ON sync_ledger(source_id, sequence);
                PRAGMA user_version = 1;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 1;
    }

    if version == 1 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sync_manifests (
                    snapshot_id TEXT PRIMARY KEY NOT NULL,
                    vehicle_id TEXT NOT NULL,
                    head_sequence INTEGER NOT NULL CHECK (head_sequence >= 0),
                    manifest_json BLOB NOT NULL
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_manifests_vehicle_head
                    ON sync_manifests(vehicle_id, head_sequence DESC);
                CREATE TABLE IF NOT EXISTS sync_packs (
                    sha256 TEXT PRIMARY KEY NOT NULL,
                    snapshot_id TEXT NOT NULL REFERENCES sync_manifests(snapshot_id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK (compressed_bytes > 0),
                    uncompressed_bytes INTEGER NOT NULL CHECK (uncompressed_bytes >= 100),
                    UNIQUE(snapshot_id, ordinal)
                ) STRICT;
                PRAGMA user_version = 2;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 2;
    }

    if version == 2 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                -- The pre-v3 source table stays intact: it already anchors
                -- sync sequence history. This companion table gives collectors
                -- a stable, non-secret external identity without rewriting it.
                CREATE TABLE IF NOT EXISTS source_identities (
                    source_id TEXT PRIMARY KEY NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_kind TEXT NOT NULL,
                    source_key TEXT NOT NULL,
                    UNIQUE(source_kind, source_key),
                    CHECK(length(CAST(source_kind AS BLOB)) BETWEEN 1 AND 64),
                    CHECK(length(CAST(source_key AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS vehicles (
                    vehicle_id TEXT PRIMARY KEY NOT NULL,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_vehicle_key TEXT NOT NULL,
                    vin TEXT,
                    display_name TEXT,
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    last_seen_at_ms INTEGER NOT NULL CHECK(last_seen_at_ms >= created_at_ms),
                    UNIQUE(source_id, source_vehicle_key),
                    CHECK(length(CAST(source_vehicle_key AS BLOB)) BETWEEN 1 AND 256),
                    CHECK(vin IS NULL OR length(CAST(vin AS BLOB)) BETWEEN 1 AND 32),
                    CHECK(display_name IS NULL OR length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS vehicles_source_id
                    ON vehicles(source_id);
                CREATE TABLE IF NOT EXISTS raw_observations (
                    observation_id INTEGER PRIMARY KEY,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
                    received_at_ms INTEGER NOT NULL CHECK(received_at_ms >= 0),
                    payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32),
                    payload_json TEXT NOT NULL CHECK(json_valid(payload_json))
                        CHECK(length(CAST(payload_json AS BLOB)) <= 262144),
                    UNIQUE(source_id, vehicle_id, observed_at_ms, payload_sha256)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS raw_observations_vehicle_observed
                    ON raw_observations(vehicle_id, observed_at_ms, observation_id);
                CREATE TRIGGER IF NOT EXISTS raw_observations_match_vehicle_source
                BEFORE INSERT ON raw_observations
                FOR EACH ROW
                WHEN (SELECT source_id FROM vehicles WHERE vehicle_id = NEW.vehicle_id)
                     != NEW.source_id
                BEGIN
                    SELECT RAISE(ABORT, 'raw observation source and vehicle mismatch');
                END;
                CREATE TRIGGER IF NOT EXISTS raw_observations_append_only_update
                BEFORE UPDATE ON raw_observations
                FOR EACH ROW
                BEGIN
                    SELECT RAISE(ABORT, 'raw observations are append-only');
                END;
                CREATE TRIGGER IF NOT EXISTS raw_observations_append_only_delete
                BEFORE DELETE ON raw_observations
                FOR EACH ROW
                BEGIN
                    SELECT RAISE(ABORT, 'raw observations are append-only');
                END;
                PRAGMA user_version = 3;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 3;
    }

    if version == 3 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS pairing_challenges (
                    pairing_id TEXT PRIMARY KEY NOT NULL,
                    label TEXT NOT NULL,
                    secret_sha256 BLOB NOT NULL CHECK(length(secret_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > created_at_ms),
                    CHECK(length(CAST(label AS BLOB)) BETWEEN 1 AND 128)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS pairing_challenges_expiry
                    ON pairing_challenges(expires_at_ms);
                CREATE TABLE IF NOT EXISTS paired_devices (
                    device_id TEXT PRIMARY KEY NOT NULL,
                    display_name TEXT NOT NULL,
                    token_sha256 BLOB NOT NULL UNIQUE CHECK(length(token_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    last_authenticated_at_ms INTEGER,
                    CHECK(last_authenticated_at_ms IS NULL OR last_authenticated_at_ms >= created_at_ms),
                    CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 128)
                ) STRICT;
                PRAGMA user_version = 4;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 4;
    }

    if version == 4 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS vehicle_lifecycle_state (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    last_observation_id INTEGER NOT NULL CHECK(last_observation_id >= 0),
                    open_session_json BLOB NOT NULL
                        CHECK(length(open_session_json) BETWEEN 2 AND 65536),
                    quarantined INTEGER NOT NULL DEFAULT 0 CHECK(quarantined IN (0, 1)),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS materialised_drives (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    drive_json TEXT NOT NULL CHECK(json_valid(drive_json)),
                    PRIMARY KEY (vehicle_id, drive_id)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS materialised_positions (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    position_json TEXT NOT NULL CHECK(json_valid(position_json)),
                    PRIMARY KEY (vehicle_id, position_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS materialised_positions_drive
                    ON materialised_positions(vehicle_id, drive_id);
                CREATE TABLE IF NOT EXISTS materialised_charges (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    charge_id INTEGER NOT NULL CHECK(charge_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    charge_json TEXT NOT NULL CHECK(json_valid(charge_json)),
                    PRIMARY KEY (vehicle_id, charge_id)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS materialised_charge_samples (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    sample_id INTEGER NOT NULL CHECK(sample_id > 0),
                    charge_id INTEGER NOT NULL CHECK(charge_id > 0),
                    sample_json TEXT NOT NULL CHECK(json_valid(sample_json)),
                    PRIMARY KEY (vehicle_id, sample_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS materialised_charge_samples_charge
                    ON materialised_charge_samples(vehicle_id, charge_id);
                PRAGMA user_version = 5;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 5;
    }

    if version == 5 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS snapshot_fingerprints (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    fingerprint_sha256 BLOB NOT NULL CHECK(length(fingerprint_sha256) = 32)
                ) STRICT;
                PRAGMA user_version = 6;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 6;
    }

    if version == 6 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS vehicle_snapshot_sequences (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    next_sequence INTEGER NOT NULL CHECK(next_sequence >= 2)
                ) STRICT;
                PRAGMA user_version = 7;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 7;
    }

    if version == 7 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_positions RENAME TO materialised_positions_v7;
                CREATE TABLE materialised_positions (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER CHECK(drive_id IS NULL OR drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    position_json TEXT NOT NULL CHECK(json_valid(position_json)),
                    PRIMARY KEY (vehicle_id, position_id)
                ) STRICT;
                INSERT INTO materialised_positions(
                    vehicle_id, position_id, drive_id, car_id, position_json
                )
                SELECT vehicle_id, position_id, drive_id, car_id, position_json
                FROM materialised_positions_v7;
                DROP TABLE materialised_positions_v7;
                CREATE INDEX materialised_positions_drive
                    ON materialised_positions(vehicle_id, drive_id);
                PRAGMA user_version = 8;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 8;
    }

    if version == 8 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE materialised_states (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    state_id INTEGER NOT NULL CHECK(state_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    state_json TEXT NOT NULL CHECK(json_valid(state_json)),
                    PRIMARY KEY (vehicle_id, state_id)
                ) STRICT;
                PRAGMA user_version = 9;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 9;
    }

    if version == 9 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE materialised_updates (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    update_id INTEGER NOT NULL CHECK(update_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    update_json TEXT NOT NULL CHECK(json_valid(update_json)),
                    PRIMARY KEY (vehicle_id, update_id)
                ) STRICT;
                PRAGMA user_version = 10;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 10;
    }

    if version == 10 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS materialised_cars (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    car_json TEXT NOT NULL CHECK(json_valid(car_json))
                ) STRICT;
                PRAGMA user_version = 11;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 11;
    }

    if version == 11 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS geofences (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    source_geofence_id INTEGER NOT NULL CHECK(source_geofence_id > 0),
                    name TEXT NOT NULL CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 256),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    radius_m REAL NOT NULL CHECK(radius_m > 0.0 AND radius_m <= 5000.0),
                    PRIMARY KEY(vehicle_id, source_geofence_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS geofences_vehicle_location
                    ON geofences(vehicle_id, latitude, longitude);
                PRAGMA user_version = 12;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 12;
    }

    if version == 12 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS address_cache (
                    osm_type TEXT NOT NULL CHECK(length(CAST(osm_type AS BLOB)) BETWEEN 1 AND 32),
                    osm_id INTEGER NOT NULL CHECK(osm_id > 0),
                    display_name TEXT NOT NULL
                        CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256),
                    name TEXT CHECK(name IS NULL OR length(CAST(name AS BLOB)) <= 256),
                    PRIMARY KEY(osm_type, osm_id)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS address_lookup_cache (
                    lookup_key TEXT PRIMARY KEY NOT NULL
                        CHECK(length(CAST(lookup_key AS BLOB)) BETWEEN 1 AND 64),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    osm_type TEXT NOT NULL,
                    osm_id INTEGER NOT NULL,
                    looked_up_at_ms INTEGER NOT NULL CHECK(looked_up_at_ms >= 0),
                    FOREIGN KEY(osm_type, osm_id)
                        REFERENCES address_cache(osm_type, osm_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS address_lookup_cache_identity
                    ON address_lookup_cache(osm_type, osm_id);
                PRAGMA user_version = 13;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 13;
    }

    if version == 13 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS address_enrichment_jobs (
                    job_key TEXT PRIMARY KEY NOT NULL
                        CHECK(length(CAST(job_key AS BLOB)) BETWEEN 1 AND 256),
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    target_type TEXT NOT NULL CHECK(target_type IN ('drive', 'charge')),
                    target_id INTEGER NOT NULL CHECK(target_id > 0),
                    field TEXT NOT NULL CHECK(field IN ('start_address', 'end_address', 'address')),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    status TEXT NOT NULL CHECK(status IN ('pending', 'running', 'retry', 'complete')),
                    attempts INTEGER NOT NULL CHECK(attempts >= 0),
                    next_attempt_ms INTEGER NOT NULL CHECK(next_attempt_ms >= 0),
                    lease_until_ms INTEGER NOT NULL CHECK(lease_until_ms >= 0),
                    completed_at_ms INTEGER,
                    last_error TEXT
                ) STRICT;
                CREATE UNIQUE INDEX IF NOT EXISTS address_enrichment_target
                    ON address_enrichment_jobs(vehicle_id, target_type, target_id, field);
                PRAGMA user_version = 14;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 14;
    }

    if version == 14 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_positions ADD COLUMN battery_heater INTEGER
                    CHECK (battery_heater IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN battery_heater_on INTEGER
                    CHECK (battery_heater_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN battery_heater_no_power INTEGER
                    CHECK (battery_heater_no_power IN (0, 1));
                PRAGMA user_version = 15;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 15;
    }

    if version == 15 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_positions ADD COLUMN speed INTEGER;
                ALTER TABLE materialised_positions ADD COLUMN power REAL;
                ALTER TABLE materialised_positions ADD COLUMN est_battery_range_km REAL;
                ALTER TABLE materialised_positions ADD COLUMN fan_status INTEGER;
                ALTER TABLE materialised_positions ADD COLUMN driver_temp_setting REAL;
                ALTER TABLE materialised_positions ADD COLUMN passenger_temp_setting REAL;
                ALTER TABLE materialised_positions ADD COLUMN is_climate_on INTEGER
                    CHECK (is_climate_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN is_rear_defroster_on INTEGER
                    CHECK (is_rear_defroster_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN is_front_defroster_on INTEGER
                    CHECK (is_front_defroster_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_fl REAL;
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_fr REAL;
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_rl REAL;
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_rr REAL;
                PRAGMA user_version = 16;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 16;
    }

    if version == 16 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_drives ADD COLUMN inside_temp_avg REAL;
                ALTER TABLE materialised_drives ADD COLUMN power_max REAL;
                ALTER TABLE materialised_drives ADD COLUMN power_min REAL;
                ALTER TABLE materialised_drives ADD COLUMN start_ideal_range_km REAL;
                ALTER TABLE materialised_drives ADD COLUMN end_ideal_range_km REAL;
                ALTER TABLE materialised_drives ADD COLUMN ascent INTEGER;
                ALTER TABLE materialised_drives ADD COLUMN descent INTEGER;
                PRAGMA user_version = 17;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 17;
    }

    if version == 17 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE car_settings (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
                    use_streaming_api INTEGER NOT NULL CHECK(use_streaming_api IN (0, 1)),
                    suspend_after_idle_min INTEGER NOT NULL CHECK(suspend_after_idle_min > 0),
                    suspend_min INTEGER NOT NULL CHECK(suspend_min > 0),
                    req_not_unlocked INTEGER NOT NULL CHECK(req_not_unlocked IN (0, 1)),
                    free_supercharging INTEGER NOT NULL CHECK(free_supercharging IN (0, 1)),
                    lfp_battery INTEGER NOT NULL CHECK(lfp_battery IN (0, 1))
                ) STRICT;
                PRAGMA user_version = 18;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 18;
    }

    if version == 18 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE geofences ADD COLUMN billing_type TEXT
                    CHECK(billing_type IS NULL OR billing_type IN ('per_kwh', 'per_minute'));
                ALTER TABLE geofences ADD COLUMN cost_per_unit REAL;
                ALTER TABLE geofences ADD COLUMN session_fee REAL
                    CHECK(session_fee IS NULL OR session_fee >= 0.0);
                PRAGMA user_version = 19;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 19;
    }

    if version == 19 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS stream_watermarks (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    last_timestamp_ms INTEGER NOT NULL CHECK(last_timestamp_ms >= 0)
                ) STRICT;
                PRAGMA user_version = 20;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 20;
    }

    if version == 20 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS lifecycle_open_rows (
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
                CREATE INDEX IF NOT EXISTS lifecycle_open_rows_vehicle_domain
                    ON lifecycle_open_rows(vehicle_id, domain, source_row_id);
                CREATE TABLE IF NOT EXISTS lifecycle_source_watermarks (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    domain TEXT NOT NULL,
                    max_source_row_id INTEGER,
                    max_timestamp_ms INTEGER,
                    PRIMARY KEY(source_id, vehicle_id, domain)
                ) STRICT;
                PRAGMA user_version = 21;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 21;
    }

    if version == 21 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE car_settings ADD COLUMN suspend_min_resolved INTEGER NOT NULL DEFAULT 1
                    CHECK(suspend_min_resolved IN (0, 1));
                PRAGMA user_version = 22;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 22;
    }

    if version == 22 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS terrain_enrichment_state (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    cursor_position_id INTEGER NOT NULL DEFAULT 0
                        CHECK(cursor_position_id >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS terrain_elevation_provenance (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    elevation_m INTEGER,
                    tile_name TEXT,
                    tile_hash TEXT,
                    dataset_source TEXT,
                    dataset_version TEXT,
                    status TEXT NOT NULL CHECK(status IN ('success', 'void', 'failed')),
                    error_code TEXT,
                    attempts INTEGER NOT NULL CHECK(attempts >= 1),
                    attempted_at_ms INTEGER NOT NULL CHECK(attempted_at_ms >= 0),
                    retry_after_ms INTEGER NOT NULL CHECK(retry_after_ms >= 0),
                    PRIMARY KEY(vehicle_id, position_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS terrain_provenance_retry
                    ON terrain_elevation_provenance(status, retry_after_ms);
                PRAGMA user_version = 23;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 23;
    }

    if version == 23 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS vehicle_identity_aliases (
                    alias_kind TEXT NOT NULL CHECK(alias_kind IN ('source_key', 'tesla_eid', 'tesla_vid', 'vin')),
                    alias_value TEXT NOT NULL,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_vehicle_key TEXT NOT NULL,
                    PRIMARY KEY(alias_kind, alias_value),
                    CHECK(length(CAST(alias_value AS BLOB)) BETWEEN 1 AND 256),
                    CHECK(length(CAST(source_vehicle_key AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS vehicle_identity_aliases_vehicle
                    ON vehicle_identity_aliases(vehicle_id);
                DROP TRIGGER IF EXISTS raw_observations_match_vehicle_source;
                CREATE TRIGGER raw_observations_match_vehicle_source
                BEFORE INSERT ON raw_observations
                FOR EACH ROW
                WHEN NOT EXISTS (
                    SELECT 1 FROM vehicle_identity_aliases
                    WHERE vehicle_id = NEW.vehicle_id AND source_id = NEW.source_id
                )
                BEGIN
                    SELECT RAISE(ABORT, 'raw observation source and vehicle mismatch');
                END;
                INSERT OR IGNORE INTO vehicle_identity_aliases
                    (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
                SELECT 'vin', v.vin, v.vehicle_id, v.source_id, v.source_vehicle_key
                FROM vehicles v WHERE v.vin IS NOT NULL AND length(v.vin) > 0;
                INSERT OR IGNORE INTO vehicle_identity_aliases
                    (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
                SELECT 'source_key', v.source_id || ':' || v.source_vehicle_key,
                       v.vehicle_id, v.source_id, v.source_vehicle_key
                FROM vehicles v;
                INSERT OR IGNORE INTO vehicle_identity_aliases
                    (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
                SELECT 'tesla_eid', substr(v.source_vehicle_key, 5), v.vehicle_id,
                       v.source_id, v.source_vehicle_key
                FROM vehicles v
                WHERE v.source_vehicle_key GLOB 'eid:[0-9]*'
                  AND length(substr(v.source_vehicle_key, 5)) > 0;
                PRAGMA user_version = 24;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 24;
    }

    if version == 24 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sync_bases (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    snapshot_id TEXT NOT NULL UNIQUE,
                    base_sequence INTEGER NOT NULL CHECK(base_sequence >= 0),
                    base_digest TEXT NOT NULL CHECK(length(base_digest) = 64),
                    packs_json BLOB NOT NULL
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_deltas (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    from_sequence INTEGER NOT NULL CHECK(from_sequence >= 0),
                    to_sequence INTEGER NOT NULL CHECK(to_sequence > from_sequence),
                    parent_chain_digest TEXT NOT NULL CHECK(length(parent_chain_digest) = 64),
                    chain_digest TEXT NOT NULL CHECK(length(chain_digest) = 64),
                    pack_digest TEXT NOT NULL CHECK(length(pack_digest) = 64),
                    pack_json BLOB NOT NULL,
                    PRIMARY KEY(vehicle_id, from_sequence, to_sequence),
                    UNIQUE(vehicle_id, chain_digest)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_heads (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    base_snapshot_id TEXT NOT NULL REFERENCES sync_bases(snapshot_id) ON DELETE RESTRICT,
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0),
                    head_digest TEXT NOT NULL CHECK(length(head_digest) = 64),
                    terminal_cursor TEXT NOT NULL
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_deltas_vehicle_sequence
                    ON sync_deltas(vehicle_id, from_sequence, to_sequence);
                PRAGMA user_version = 25;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 25;
    }

    if version == 25 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS import_generations (
                    run_id TEXT PRIMARY KEY NOT NULL,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    status TEXT NOT NULL CHECK(status IN ('staging', 'promoting')),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS import_generations_vehicle
                    ON import_generations(vehicle_id, status);
                CREATE TABLE IF NOT EXISTS import_generation_sessions (
                    run_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES import_generations(run_id) ON DELETE CASCADE,
                    session_json TEXT NOT NULL CHECK(json_valid(session_json))
                ) STRICT;
                PRAGMA user_version = 26;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 26;
    }

    if version == 26 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE import_generations ADD COLUMN base_last_observation_id
                    INTEGER NOT NULL DEFAULT 0 CHECK(base_last_observation_id >= 0);
                ALTER TABLE import_generations ADD COLUMN base_updated_at_ms
                    INTEGER NOT NULL DEFAULT 0 CHECK(base_updated_at_ms >= 0);
                PRAGMA user_version = 27;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 27;
    }

    if version == 27 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS export_outbox (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    dirty_revision INTEGER NOT NULL CHECK(dirty_revision > 0),
                    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
                    next_attempt_ms INTEGER NOT NULL DEFAULT 0 CHECK(next_attempt_ms >= 0),
                    claimed_until_ms INTEGER NOT NULL DEFAULT 0 CHECK(claimed_until_ms >= 0),
                    last_error TEXT
                ) STRICT;
                PRAGMA user_version = 28;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 28;
    }

    if version == 28 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sync_mutation_sequences (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    next_revision INTEGER NOT NULL CHECK(next_revision > 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_mutations (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    revision INTEGER NOT NULL CHECK(revision > 0),
                    entity TEXT NOT NULL CHECK(entity IN
                        ('car', 'car_setting', 'geofence', 'address', 'drive',
                         'position', 'charge', 'charge_sample', 'state', 'update')),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    operation TEXT NOT NULL CHECK(operation IN ('upsert', 'tombstone')),
                    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
                    published INTEGER NOT NULL DEFAULT 0 CHECK(published IN (0, 1)),
                    claimed_until_ms INTEGER NOT NULL DEFAULT 0 CHECK(claimed_until_ms >= 0),
                    PRIMARY KEY(vehicle_id, revision)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_mutations_pending
                    ON sync_mutations(vehicle_id, published, revision, claimed_until_ms);
                PRAGMA user_version = 29;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 29;
    }

    if version == 29 {
        // Version 29 is retained for migration ordering. New databases do
        // not create the removed MQTT tables.
        connection
            .execute_batch("PRAGMA user_version = 30;")
            .map_err(StoreError::Migrate)?;
        version = 30;
    }

    if version == 30 {
        // Existing rows have no trustworthy manifest identity. Preserve the
        // hash, but leave these nullable columns unset so it cannot skip a
        // later import by accidentally matching an arbitrary manifest.
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE snapshot_fingerprints ADD COLUMN snapshot_id TEXT;
                ALTER TABLE snapshot_fingerprints ADD COLUMN head_sequence INTEGER;
                CREATE INDEX IF NOT EXISTS snapshot_fingerprints_manifest
                    ON snapshot_fingerprints(snapshot_id, head_sequence);
                PRAGMA user_version = 31;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 31;
    }

    if version == 31 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS outbound_request_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    vehicle_tesla_id INTEGER CHECK(vehicle_tesla_id > 0),
                    transport TEXT NOT NULL CHECK(transport IN ('owner_api', 'stream', 'legacy_auth')),
                    operation TEXT NOT NULL CHECK(operation IN ('products', 'vehicle_probe', 'vehicle_data', 'token_refresh', 'stream_connect', 'stream_subscribe', 'stream_unsubscribe')),
                    safety_class TEXT NOT NULL CHECK(safety_class IN ('non_wake_endpoint', 'conditional_read', 'direct_wake_command')),
                    precondition TEXT NOT NULL CHECK(precondition IN ('not_required', 'stream_power_confirmed')),
                    outcome TEXT NOT NULL CHECK(outcome IN ('started', 'success', 'http_error', 'timeout', 'transport_error', 'authentication_rejected', 'protocol_error', 'response_too_large', 'cancelled')),
                    http_status INTEGER CHECK(http_status BETWEEN 100 AND 599),
                    CHECK((outcome = 'started' AND completed_at_ms IS NULL AND duration_ms IS NULL AND http_status IS NULL) OR (outcome <> 'started' AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL AND completed_at_ms >= started_at_ms AND duration_ms >= 0))
                ) STRICT;
                CREATE INDEX IF NOT EXISTS outbound_request_receipts_proof ON outbound_request_receipts(correlation_id, id, safety_class, outcome);
                CREATE INDEX IF NOT EXISTS outbound_request_receipts_retention ON outbound_request_receipts(outcome, completed_at_ms, id);
                PRAGMA user_version = 32;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 32;
    }

    if version == 32 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS stream_session_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    vehicle_tesla_id INTEGER NOT NULL CHECK(vehicle_tesla_id > 0),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    outcome TEXT NOT NULL CHECK(outcome IN ('started', 'orderly_shutdown')),
                    unsubscribe_receipt_id INTEGER,
                    CHECK((outcome = 'started' AND completed_at_ms IS NULL AND duration_ms IS NULL AND unsubscribe_receipt_id IS NULL) OR (outcome = 'orderly_shutdown' AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL AND completed_at_ms >= started_at_ms AND duration_ms >= 0 AND unsubscribe_receipt_id IS NOT NULL))
                ) STRICT;
                CREATE INDEX IF NOT EXISTS stream_session_receipts_proof
                    ON stream_session_receipts(correlation_id, outcome, id);
                CREATE INDEX IF NOT EXISTS stream_session_receipts_retention
                    ON stream_session_receipts(outcome, completed_at_ms, id);
                PRAGMA user_version = 33;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 33;
    }

    if version == 33 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE outbound_request_receipts
                    ADD COLUMN retry_after_seconds INTEGER
                    CHECK(retry_after_seconds >= 0);
                PRAGMA user_version = 34;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 34;
    }

    if version == 34 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                PRAGMA user_version = 35;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 35;
    }

    if version == 35 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- The base transport metadata does not expose its selected
                -- source-car identifier through SyncManifest.  Persist the
                -- exact binding used to create each new V2 base so later
                -- deltas never reconstruct it from mutable source aliases.
                CREATE TABLE IF NOT EXISTS v2_base_bindings (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    snapshot_id TEXT NOT NULL UNIQUE
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    installation_id TEXT NOT NULL CHECK(length(installation_id) = 36),
                    account_id TEXT NOT NULL CHECK(length(account_id) = 36),
                    generation INTEGER NOT NULL CHECK(generation >= 1),
                    selected_car_id INTEGER NOT NULL CHECK(selected_car_id > 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_heads (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    base_snapshot_id TEXT NOT NULL UNIQUE
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    selected_car_id INTEGER NOT NULL CHECK(selected_car_id > 0),
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_rows (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state'
                    )),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    PRIMARY KEY(vehicle_id, entity, entity_id)
                ) STRICT;
                PRAGMA user_version = 36;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 36;
    }

    if version == 36 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- This is deliberately separate from the legacy deletion
                -- inventory: it includes `car` and records only canonical
                -- digests, never projection payload JSON.
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_state_heads (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    base_snapshot_id TEXT NOT NULL
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    selected_car_id INTEGER NOT NULL CHECK(selected_car_id > 0),
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_state_rows (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_state_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'car', 'drive', 'position', 'charge', 'charge_sample', 'state'
                    )),
                    entity_ordinal INTEGER NOT NULL CHECK(entity_ordinal BETWEEN 0 AND 5),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    projection_sha256 BLOB NOT NULL CHECK(length(projection_sha256) = 32),
                    CHECK(
                        (entity = 'car' AND entity_ordinal = 0) OR
                        (entity = 'drive' AND entity_ordinal = 1) OR
                        (entity = 'position' AND entity_ordinal = 2) OR
                        (entity = 'charge' AND entity_ordinal = 3) OR
                        (entity = 'charge_sample' AND entity_ordinal = 4) OR
                        (entity = 'state' AND entity_ordinal = 5)
                    ),
                    PRIMARY KEY(vehicle_id, entity_ordinal, entity_id),
                    UNIQUE(vehicle_id, entity, entity_id)
                ) STRICT, WITHOUT ROWID;
                PRAGMA user_version = 37;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 37;
    }

    if version == 37 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- A durable, one-time audit marker for the only supported
                -- migration from the retired fragment-dependent direct
                -- fingerprint to the fragment-independent logical one.
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_state_bridges (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    base_snapshot_id TEXT NOT NULL
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0),
                    algorithm TEXT NOT NULL CHECK(algorithm = 'logical_projection_v1'),
                    legacy_fingerprint_sha256 BLOB NOT NULL
                        CHECK(length(legacy_fingerprint_sha256) = 32),
                    logical_fingerprint_sha256 BLOB NOT NULL
                        CHECK(length(logical_fingerprint_sha256) = 32)
                ) STRICT;
                PRAGMA user_version = 38;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 38;
    }

    if version == 38 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- Firmware-update history is now part of the TeslaMate V2
                -- projection inventory and digest state. Rebuild the two
                -- constrained WITHOUT ROWID tables so existing bases retain
                -- their exact rows while new `update` facts use ordinal 6.
                CREATE TABLE teslamate_import_projection_rows_v39 (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state', 'update'
                    )),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    PRIMARY KEY(vehicle_id, entity, entity_id)
                ) STRICT;
                INSERT INTO teslamate_import_projection_rows_v39(
                    vehicle_id, entity, entity_id
                )
                SELECT vehicle_id, entity, entity_id
                  FROM teslamate_import_projection_rows;
                DROP TABLE teslamate_import_projection_rows;
                ALTER TABLE teslamate_import_projection_rows_v39
                    RENAME TO teslamate_import_projection_rows;

                CREATE TABLE teslamate_import_projection_state_rows_v39 (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_state_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'car', 'drive', 'position', 'charge', 'charge_sample', 'state', 'update'
                    )),
                    entity_ordinal INTEGER NOT NULL CHECK(entity_ordinal BETWEEN 0 AND 6),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    projection_sha256 BLOB NOT NULL CHECK(length(projection_sha256) = 32),
                    CHECK(
                        (entity = 'car' AND entity_ordinal = 0) OR
                        (entity = 'drive' AND entity_ordinal = 1) OR
                        (entity = 'position' AND entity_ordinal = 2) OR
                        (entity = 'charge' AND entity_ordinal = 3) OR
                        (entity = 'charge_sample' AND entity_ordinal = 4) OR
                        (entity = 'state' AND entity_ordinal = 5) OR
                        (entity = 'update' AND entity_ordinal = 6)
                    ),
                    PRIMARY KEY(vehicle_id, entity_ordinal, entity_id),
                    UNIQUE(vehicle_id, entity, entity_id)
                ) STRICT, WITHOUT ROWID;
                INSERT INTO teslamate_import_projection_state_rows_v39(
                    vehicle_id, entity, entity_ordinal, entity_id, car_id, projection_sha256
                )
                SELECT vehicle_id, entity, entity_ordinal, entity_id, car_id, projection_sha256
                  FROM teslamate_import_projection_state_rows;
                DROP TABLE teslamate_import_projection_state_rows;
                ALTER TABLE teslamate_import_projection_state_rows_v39
                    RENAME TO teslamate_import_projection_state_rows;
                PRAGMA user_version = 39;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 39;
    }

    if version == 39 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- Only collector-published deltas have durable sync-mutation
                -- provenance. Import successors deliberately remain outside
                -- this table so compaction can never guess how to rebuild
                -- source-owned history.
                CREATE TABLE IF NOT EXISTS sync_live_delta_spans (
                    vehicle_id TEXT NOT NULL,
                    from_sequence INTEGER NOT NULL CHECK(from_sequence >= 0),
                    to_sequence INTEGER NOT NULL CHECK(to_sequence > from_sequence),
                    from_revision INTEGER NOT NULL CHECK(from_revision > 0),
                    to_revision INTEGER NOT NULL CHECK(to_revision >= from_revision),
                    pack_digest TEXT NOT NULL CHECK(length(pack_digest) = 64),
                    PRIMARY KEY(vehicle_id, from_sequence, to_sequence),
                    FOREIGN KEY(vehicle_id, from_sequence, to_sequence)
                        REFERENCES sync_deltas(vehicle_id, from_sequence, to_sequence)
                        ON DELETE CASCADE,
                    CHECK(to_sequence - from_sequence = to_revision - from_revision + 1)
                ) STRICT;
                CREATE UNIQUE INDEX IF NOT EXISTS sync_live_delta_spans_revision_range
                    ON sync_live_delta_spans(vehicle_id, from_revision, to_revision);
                CREATE INDEX IF NOT EXISTS sync_mutations_compaction_latest
                    ON sync_mutations(vehicle_id, entity, entity_id, revision DESC);
                PRAGMA user_version = 40;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 40;
    }

    if version == 40 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                DROP INDEX IF EXISTS stream_session_receipts_proof;
                DROP INDEX IF EXISTS stream_session_receipts_retention;
                ALTER TABLE stream_session_receipts RENAME TO stream_session_receipts_v40;
                CREATE TABLE stream_session_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    vehicle_tesla_id INTEGER NOT NULL CHECK(vehicle_tesla_id > 0),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    outcome TEXT NOT NULL CHECK(outcome IN (
                        'started', 'orderly_shutdown',
                        'cancelled_before_subscription', 'transport_ended', 'failed'
                    )),
                    unsubscribe_receipt_id INTEGER,
                    CHECK(
                        (outcome = 'started'
                         AND completed_at_ms IS NULL AND duration_ms IS NULL
                         AND unsubscribe_receipt_id IS NULL)
                        OR (outcome = 'orderly_shutdown'
                            AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL
                            AND completed_at_ms >= started_at_ms AND duration_ms >= 0
                            AND unsubscribe_receipt_id IS NOT NULL)
                        OR (outcome IN (
                                'cancelled_before_subscription', 'transport_ended', 'failed'
                            )
                            AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL
                            AND completed_at_ms >= started_at_ms AND duration_ms >= 0
                            AND unsubscribe_receipt_id IS NULL)
                    )
                ) STRICT;
                INSERT INTO stream_session_receipts(
                    id, correlation_id, vehicle_tesla_id, started_at_ms,
                    completed_at_ms, duration_ms, outcome, unsubscribe_receipt_id
                )
                SELECT id, correlation_id, vehicle_tesla_id, started_at_ms,
                       completed_at_ms, duration_ms, outcome, unsubscribe_receipt_id
                  FROM stream_session_receipts_v40;
                DROP TABLE stream_session_receipts_v40;
                CREATE INDEX stream_session_receipts_proof
                    ON stream_session_receipts(correlation_id, outcome, id);
                CREATE INDEX stream_session_receipts_retention
                    ON stream_session_receipts(outcome, completed_at_ms, id);
                PRAGMA user_version = 41;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 41;
    }

    if version == 41 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- A client may already hold the signed lineage that was
                -- current immediately before live-delta compaction. Retain
                -- only the replaced objects, bound to that exact validated
                -- manifest and a finite authorization window. Arbitrary
                -- orphan files never gain an authorization row.
                CREATE TABLE sync_retired_lineages (
                    vehicle_id TEXT NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    head_digest TEXT NOT NULL CHECK(length(head_digest) = 64),
                    manifest_json BLOB NOT NULL CHECK(length(manifest_json) > 0),
                    retired_at_ms INTEGER NOT NULL CHECK(retired_at_ms >= 0),
                    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > retired_at_ms),
                    PRIMARY KEY(vehicle_id, head_digest)
                ) STRICT;
                CREATE TABLE sync_retired_lineage_packs (
                    vehicle_id TEXT NOT NULL,
                    head_digest TEXT NOT NULL,
                    pack_digest TEXT NOT NULL CHECK(length(pack_digest) = 64),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK(compressed_bytes > 0),
                    PRIMARY KEY(vehicle_id, head_digest, pack_digest),
                    FOREIGN KEY(vehicle_id, head_digest)
                        REFERENCES sync_retired_lineages(vehicle_id, head_digest)
                        ON DELETE CASCADE
                ) STRICT;
                CREATE INDEX sync_retired_lineage_packs_authorization
                    ON sync_retired_lineage_packs(pack_digest, vehicle_id, head_digest);
                CREATE INDEX sync_retired_lineages_expiry
                    ON sync_retired_lineages(expires_at_ms, vehicle_id, head_digest);
                PRAGMA user_version = 42;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 42;
    }

    if version == 42 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- Cross-process operational truth for the opt-in supervised
                -- collector. A random instance ID fences stale processes;
                -- state is a closed, redacted vocabulary rather than an
                -- arbitrary error string.
                CREATE TABLE IF NOT EXISTS supervised_collector_lease (
                    singleton_id INTEGER PRIMARY KEY NOT NULL
                        CHECK(singleton_id = 1),
                    instance_id TEXT NOT NULL CHECK(length(instance_id) = 36),
                    state TEXT NOT NULL CHECK(state IN ('active', 'auth_terminal')),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    heartbeat_at_ms INTEGER NOT NULL
                        CHECK(heartbeat_at_ms >= started_at_ms),
                    lease_until_ms INTEGER NOT NULL
                        CHECK(lease_until_ms > heartbeat_at_ms)
                ) STRICT;
                PRAGMA user_version = 43;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 43;
    }

    if version == 43 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- Bind each refresh request to the exact encrypted-journal
                -- attempt and credential generation that authorized it. The
                -- parent receipt remains redacted request metadata; this
                -- child has no token material or endpoint data.
                CREATE TABLE legacy_refresh_receipt_bindings (
                    receipt_id INTEGER PRIMARY KEY NOT NULL
                        REFERENCES outbound_request_receipts(id) ON DELETE CASCADE,
                    attempt_id TEXT NOT NULL UNIQUE CHECK(length(attempt_id) = 36),
                    input_credential_generation TEXT NOT NULL
                        CHECK(length(input_credential_generation) = 36),
                    output_credential_generation TEXT
                        CHECK(output_credential_generation IS NULL
                              OR length(output_credential_generation) = 36),
                    CHECK(output_credential_generation IS NULL
                          OR output_credential_generation <> input_credential_generation)
                ) STRICT;
                CREATE UNIQUE INDEX legacy_refresh_receipt_output_generation
                    ON legacy_refresh_receipt_bindings(output_credential_generation)
                    WHERE output_credential_generation IS NOT NULL;
                PRAGMA user_version = 44;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 44;
    }

    if version == 44 {
        connection
            .execute_batch(&format!(
                "
                BEGIN IMMEDIATE;
                -- Historical pre-parity builds used this compact table as a
                -- permanent refresh-input fence. Keep it for schema and
                -- downgrade compatibility; parity builds never add rows or
                -- consult it when deciding whether TeslaMate would retry.
                CREATE TABLE legacy_refresh_input_fences (
                    input_credential_generation TEXT PRIMARY KEY COLLATE NOCASE
                        CHECK(length(input_credential_generation) = 36)
                ) STRICT, WITHOUT ROWID;
                INSERT INTO legacy_refresh_input_fences(input_credential_generation)
                    SELECT lower(input_credential_generation)
                      FROM legacy_refresh_receipt_bindings
                     GROUP BY lower(input_credential_generation);
                CREATE TABLE legacy_refresh_input_fence_migration_guard (
                    fence_count INTEGER NOT NULL CHECK(fence_count <= {0})
                ) STRICT;
                INSERT INTO legacy_refresh_input_fence_migration_guard(fence_count)
                    SELECT COUNT(*) FROM legacy_refresh_input_fences;
                DROP TABLE legacy_refresh_input_fence_migration_guard;
                PRAGMA user_version = 45;
                COMMIT;
                ",
                MAX_LEGACY_REFRESH_INPUT_FENCES
            ))
            .map_err(StoreError::Migrate)?;
        version = 45;
    }

    if version == 45 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                DROP TABLE IF EXISTS mqtt_delivery_state;
                DROP TABLE IF EXISTS mqtt_summary_revisions;
                PRAGMA user_version = 46;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 46;
    }

    if version == 46 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS teslamate_legacy_tokens (
                    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                    access BLOB NOT NULL CHECK(length(access) > 0),
                    refresh BLOB NOT NULL CHECK(length(refresh) > 0),
                    expires_at INTEGER NOT NULL CHECK(expires_at >= 0),
                    next_refresh_at INTEGER NOT NULL CHECK(next_refresh_at >= 0),
                    CHECK(
                        (expires_at = 0 AND next_refresh_at = 0)
                        OR (expires_at > next_refresh_at AND next_refresh_at > 0)
                    )
                ) STRICT;
                PRAGMA user_version = 47;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 47;
    }

    if version == 47 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                DROP TABLE IF EXISTS migration_request_intents;
                DROP TABLE IF EXISTS migration_wake_leases;
                PRAGMA user_version = 48;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 48;
    }

    if version == 48 {
        migrate_address_cache_metadata(connection)?;
        version = 49;
    }

    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(StoreError::UnsupportedSchema(version))
    }
}

fn migrate_address_cache_metadata(connection: &Connection) -> Result<(), StoreError> {
    const COLUMNS: [(&str, &str); 12] = [
        (
            "latitude",
            "ALTER TABLE address_cache ADD COLUMN latitude REAL CHECK(latitude IS NULL OR (latitude >= -90.0 AND latitude <= 90.0));",
        ),
        (
            "longitude",
            "ALTER TABLE address_cache ADD COLUMN longitude REAL CHECK(longitude IS NULL OR (longitude >= -180.0 AND longitude <= 180.0));",
        ),
        (
            "house_number",
            "ALTER TABLE address_cache ADD COLUMN house_number TEXT;",
        ),
        ("road", "ALTER TABLE address_cache ADD COLUMN road TEXT;"),
        (
            "neighbourhood",
            "ALTER TABLE address_cache ADD COLUMN neighbourhood TEXT;",
        ),
        ("city", "ALTER TABLE address_cache ADD COLUMN city TEXT;"),
        (
            "county",
            "ALTER TABLE address_cache ADD COLUMN county TEXT;",
        ),
        (
            "postcode",
            "ALTER TABLE address_cache ADD COLUMN postcode TEXT;",
        ),
        ("state", "ALTER TABLE address_cache ADD COLUMN state TEXT;"),
        (
            "state_district",
            "ALTER TABLE address_cache ADD COLUMN state_district TEXT;",
        ),
        (
            "country",
            "ALTER TABLE address_cache ADD COLUMN country TEXT;",
        ),
        (
            "raw_json",
            "ALTER TABLE address_cache ADD COLUMN raw_json TEXT CHECK(raw_json IS NULL OR json_valid(raw_json));",
        ),
    ];

    let mut migration = String::from("BEGIN IMMEDIATE;\n");
    for (column, statement) in COLUMNS {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pragma_table_info('address_cache') WHERE name = ?1
                )",
                [column],
                |row| row.get(0),
            )
            .map_err(StoreError::Migrate)?;
        if !exists {
            migration.push_str(statement);
            migration.push('\n');
        }
    }
    migration.push_str("PRAGMA user_version = 49;\nCOMMIT;");
    connection
        .execute_batch(&migration)
        .map_err(StoreError::Migrate)
}

fn schema_version(connection: &Connection) -> Result<i32, StoreError> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(StoreError::Query)
}

fn referenced_pack_rows_at(
    connection: &Connection,
    retired_expiry_cutoff_ms: i64,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    let mut rows = {
        let mut statement = connection
            .prepare("SELECT sha256, relative_path, compressed_bytes FROM sync_packs")
            .map_err(StoreError::Query)?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
    };
    let retired_rows = {
        let mut statement = connection
            .prepare(
                "SELECT lineage.vehicle_id, lineage.head_digest, lineage.manifest_json,
                        packs.pack_digest, packs.relative_path, packs.compressed_bytes
                   FROM sync_retired_lineage_packs AS packs
                   JOIN sync_retired_lineages AS lineage
                     ON lineage.vehicle_id = packs.vehicle_id
                    AND lineage.head_digest = packs.head_digest
                  WHERE lineage.expires_at_ms > ?1
                  ORDER BY packs.pack_digest, lineage.vehicle_id, lineage.head_digest",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map(params![retired_expiry_cutoff_ms], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
    };
    for (vehicle_id, head_digest, manifest_json, pack_digest, relative_path, compressed_bytes) in
        retired_rows
    {
        validate_retired_lineage_pack_binding(
            &vehicle_id,
            &head_digest,
            &manifest_json,
            &pack_digest,
            &relative_path,
            compressed_bytes,
        )?;
        rows.push((pack_digest, relative_path, compressed_bytes));
    }
    rows.sort_unstable();
    let mut deduplicated: Vec<(String, String, i64)> = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(existing) = deduplicated.last()
            && existing.0 == row.0
        {
            if existing != &row {
                return Err(StoreError::LineageCatalogConflict);
            }
            continue;
        }
        deduplicated.push(row);
    }
    Ok(deduplicated)
}

fn validate_retired_lineage_pack_binding(
    vehicle_id: &str,
    head_digest: &str,
    manifest_json: &[u8],
    pack_digest: &str,
    relative_path: &str,
    compressed_bytes: i64,
) -> Result<(), StoreError> {
    let digest = pack_digest
        .parse::<Sha256Digest>()
        .map_err(|_| StoreError::LineageCatalogConflict)?;
    let manifest: LineageManifestV2 =
        serde_json::from_slice(manifest_json).map_err(StoreError::DeserializeManifest)?;
    manifest
        .validate_with_limits(ProtocolLimits::default())
        .map_err(StoreError::Manifest)?;
    let descriptor = manifest
        .base
        .packs
        .iter()
        .chain(manifest.deltas.iter().map(|delta| &delta.pack))
        .find(|pack| pack.sha256 == digest)
        .ok_or(StoreError::LineageCatalogConflict)?;
    if vehicle_id != manifest.vehicle_id.to_string()
        || head_digest != manifest.head_digest.to_string()
        || relative_path != descriptor.relative_path
        || compressed_bytes
            != i64::try_from(descriptor.compressed_bytes)
                .map_err(|_| StoreError::PackSizeTooLarge)?
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

fn outbound_request_clock_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::OutboundRequestClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::OutboundRequestClockOverflow)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn retired_lineage_clock_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::RetiredLineageClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::RetiredLineageClockOverflow)
}

fn prune_expired_outbound_request_receipts(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    let cutoff_ms = outbound_request_clock_ms()?.saturating_sub(OUTBOUND_REQUEST_RETENTION_MS);
    transaction
        .execute(
            "DELETE FROM outbound_request_receipts
              WHERE outcome <> 'started'
                AND completed_at_ms < ?1",
            params![cutoff_ms],
        )
        .map_err(StoreError::OutboundRequestReceipt)?;
    Ok(())
}

fn ensure_outbound_request_capacity(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    prune_expired_outbound_request_receipts(transaction)?;
    let count = outbound_request_capacity_consumers(transaction)?;
    if count >= MAX_OUTBOUND_REQUEST_RECEIPTS {
        return Err(StoreError::OutboundRequestAuditCapacityExhausted);
    }
    Ok(())
}

/// Every receipt consumes the same bounded audit budget.
fn outbound_request_capacity_consumers(transaction: &Transaction<'_>) -> Result<i64, StoreError> {
    transaction
        .query_row(
            "SELECT COUNT(*) FROM outbound_request_receipts",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::OutboundRequestReceipt)
}

fn prune_expired_stream_session_receipts(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    let cutoff_ms = outbound_request_clock_ms()?.saturating_sub(OUTBOUND_REQUEST_RETENTION_MS);
    transaction
        .execute(
            "DELETE FROM stream_session_receipts
             WHERE outcome <> 'started' AND completed_at_ms < ?1",
            params![cutoff_ms],
        )
        .map_err(StoreError::StreamSessionReceipt)?;
    Ok(())
}

fn ensure_stream_session_capacity(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    prune_expired_stream_session_receipts(transaction)?;
    let count: i64 = transaction
        .query_row("SELECT COUNT(*) FROM stream_session_receipts", [], |row| {
            row.get(0)
        })
        .map_err(StoreError::StreamSessionReceipt)?;
    if count >= MAX_OUTBOUND_REQUEST_RECEIPTS {
        return Err(StoreError::StreamSessionAuditCapacityExhausted);
    }
    Ok(())
}

fn invalid_outbound_request_receipt_value(index: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid outbound request receipt",
        )),
    )
}

fn receipt_from_row(row: &rusqlite::Row<'_>) -> Result<OutboundRequestReceipt, rusqlite::Error> {
    let correlation: String = row.get(1)?;
    let correlation_id =
        Uuid::parse_str(&correlation).map_err(|_| invalid_outbound_request_receipt_value(1))?;
    let transport: String = row.get(6)?;
    let operation: String = row.get(7)?;
    let safety_class: String = row.get(8)?;
    let precondition: String = row.get(9)?;
    let outcome: String = row.get(10)?;
    let http_status = row
        .get::<_, Option<i64>>(11)?
        .map(|value| u16::try_from(value).map_err(|_| invalid_outbound_request_receipt_value(11)))
        .transpose()?;
    let retry_after_seconds = row
        .get::<_, Option<i64>>(12)?
        .map(|value| u64::try_from(value).map_err(|_| invalid_outbound_request_receipt_value(12)))
        .transpose()?;
    Ok(OutboundRequestReceipt {
        id: OutboundRequestReceiptId(row.get(0)?),
        correlation_id,
        started_at_ms: row.get(2)?,
        completed_at_ms: row.get(3)?,
        duration_ms: row.get(4)?,
        vehicle_tesla_id: row.get(5)?,
        transport: OutboundRequestTransport::parse(&transport)
            .ok_or_else(|| invalid_outbound_request_receipt_value(6))?,
        operation: OutboundRequestOperation::parse(&operation)
            .ok_or_else(|| invalid_outbound_request_receipt_value(7))?,
        safety_class: OutboundRequestSafetyClass::parse(&safety_class)
            .ok_or_else(|| invalid_outbound_request_receipt_value(8))?,
        precondition: OutboundRequestPrecondition::parse(&precondition)
            .ok_or_else(|| invalid_outbound_request_receipt_value(9))?,
        outcome: if outcome == "started" {
            None
        } else {
            Some(
                OutboundRequestOutcome::parse(&outcome)
                    .ok_or_else(|| invalid_outbound_request_receipt_value(10))?,
            )
        },
        http_status,
        retry_after_seconds,
    })
}

fn cleanup_abandoned_import_generations(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute("DELETE FROM import_generations", [])
        .map_err(StoreError::ImportGeneration)?;
    Ok(())
}

fn require_positive_db(value: i64, field: &'static str) -> Result<(), StoreError> {
    if value <= 0 {
        Err(StoreError::InvalidLifecycleCarId)
    } else {
        let _ = field;
        Ok(())
    }
}

fn append_observation_in_transaction(
    transaction: &Transaction<'_>,
    input: &ObservationInput,
    received_at_ms: i64,
) -> Result<AppendObservation, StoreError> {
    input.validate()?;
    validate_timestamp("observation received_at_ms", received_at_ms)?;
    let payload_json =
        serde_json::to_vec(&input.payload).map_err(StoreError::SerializeObservation)?;
    if payload_json.len() > MAX_RAW_OBSERVATION_BYTES {
        return Err(StoreError::ObservationTooLarge {
            actual: payload_json.len(),
            maximum: MAX_RAW_OBSERVATION_BYTES,
        });
    }
    let payload_sha256 = Sha256Digest::of_bytes(&payload_json);
    let payload_json = String::from_utf8(payload_json).expect("serde_json is UTF-8");
    ensure_vehicle_belongs_to_source(transaction, input.vehicle_id, input.source_id)?;
    let inserted = transaction
        .execute(
            "INSERT INTO raw_observations
             (source_id, vehicle_id, observed_at_ms, received_at_ms, payload_sha256, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(source_id, vehicle_id, observed_at_ms, payload_sha256) DO NOTHING",
            params![
                input.source_id.to_string(),
                input.vehicle_id.to_string(),
                input.observed_at_ms,
                received_at_ms,
                payload_sha256.as_bytes().as_slice(),
                payload_json,
            ],
        )
        .map_err(StoreError::AppendObservation)?
        == 1;
    let observation = find_observation(
        transaction,
        input.source_id,
        input.vehicle_id,
        input.observed_at_ms,
        payload_sha256,
    )?
    .ok_or(StoreError::ObservationMissingAfterInsert)?;
    Ok(AppendObservation {
        observation,
        inserted,
    })
}

fn stream_timestamp_is_newer(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    timestamp_ms: i64,
) -> Result<bool, StoreError> {
    let previous: Option<i64> = transaction
        .query_row(
            "SELECT last_timestamp_ms FROM stream_watermarks WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    Ok(previous.is_none_or(|value| timestamp_ms > value))
}

fn accept_stream_timestamp_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    timestamp_ms: i64,
) -> Result<bool, StoreError> {
    validate_timestamp("stream timestamp", timestamp_ms)?;
    Ok(transaction
        .execute(
            "INSERT INTO stream_watermarks(vehicle_id, last_timestamp_ms)
             VALUES (?1, ?2)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                 last_timestamp_ms = excluded.last_timestamp_ms
             WHERE excluded.last_timestamp_ms > stream_watermarks.last_timestamp_ms",
            params![vehicle_id.to_string(), timestamp_ms],
        )
        .map_err(StoreError::Query)?
        == 1)
}

fn load_lifecycle_state_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<Option<LifecycleStateRecord>, StoreError> {
    transaction
        .query_row(
            "SELECT vehicle_id, car_id, last_observation_id, open_session_json,
                    quarantined, updated_at_ms
             FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| {
                let vehicle_id = row
                    .get::<_, String>(0)?
                    .parse()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok(LifecycleStateRecord {
                    vehicle_id,
                    car_id: row.get(1)?,
                    last_observation_id: row.get(2)?,
                    open_session_json: row.get(3)?,
                    quarantined: row.get::<_, i64>(4)? != 0,
                    updated_at_ms: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::Query)
}

fn observations_after_id_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    after_observation_id: i64,
    limit: u32,
) -> Result<Vec<ObservationRecord>, StoreError> {
    if after_observation_id < 0 {
        return Err(StoreError::InvalidLifecycleCursor);
    }
    if !(1..=MAX_OBSERVATION_QUERY_LIMIT).contains(&limit) {
        return Err(StoreError::InvalidObservationQueryLimit {
            actual: limit,
            maximum: MAX_OBSERVATION_QUERY_LIMIT,
        });
    }
    let mut statement = transaction
        .prepare(
            "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms,
                    payload_sha256, payload_json
             FROM raw_observations
             WHERE vehicle_id = ?1 AND observation_id > ?2
             ORDER BY observation_id ASC LIMIT ?3",
        )
        .map_err(StoreError::Query)?;
    statement
        .query_map(
            params![
                vehicle_id.to_string(),
                after_observation_id,
                i64::from(limit)
            ],
            observation_from_row,
        )
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)
}

fn load_open_positions_for_parent(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    drive_id: i64,
) -> Result<Vec<crate::hub_pack::ProjectionPosition>, StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT row_json FROM lifecycle_open_rows
             WHERE vehicle_id = ?1 AND domain = 'position'
               AND parent_source_row_id = ?2
             ORDER BY source_row_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_key, drive_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(StoreError::Query)?;
    let mut positions = Vec::new();
    for row in rows {
        let json = row.map_err(StoreError::Query)?;
        positions.push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
    }
    Ok(positions)
}

fn load_open_charge_samples_for_parent(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    charge_id: i64,
) -> Result<Vec<crate::hub_pack::ProjectionChargeSample>, StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT row_json FROM lifecycle_open_rows
             WHERE vehicle_id = ?1 AND domain = 'charge_sample'
               AND parent_source_row_id = ?2
             ORDER BY source_row_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_key, charge_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(StoreError::Query)?;
    let mut samples = Vec::new();
    for row in rows {
        let json = row.map_err(StoreError::Query)?;
        samples.push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
    }
    Ok(samples)
}

// Full open-child rehydrate was removed from the hot observation path.
// Close materialization loads children once via load_open_*_for_parent.

fn observation_vehicle_state(payload: &Value) -> String {
    payload
        .get("source_vehicle_state")
        .and_then(Value::as_str)
        .filter(|state| {
            !state.is_empty() && state.len() <= 64 && !state.chars().any(char::is_control)
        })
        .unwrap_or("unknown")
        .to_owned()
}

fn ensure_vehicle_source(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
) -> Result<(), StoreError> {
    let actual: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM vehicle_identity_aliases
             WHERE vehicle_id = ?1 AND source_id = ?2",
            params![vehicle_id.to_string(), source_id.to_string()],
            |_| Ok(1),
        )
        .optional()
        .map_err(StoreError::LifecycleWrite)?;
    let Some(actual) = actual else {
        return Err(StoreError::UnknownVehicle(vehicle_id));
    };
    let _ = actual;
    Ok(())
}

fn insert_open_row<T: Serialize>(
    transaction: &Transaction<'_>,
    source_id: &str,
    source_table: &str,
    source_row_id: i64,
    vehicle_id: &str,
    car_id: i64,
    domain: &str,
    parent_source_row_id: Option<i64>,
    row: &T,
) -> Result<usize, StoreError> {
    let row_json = serde_json::to_string(row).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "INSERT INTO lifecycle_open_rows(
                source_id, source_table, source_row_id, vehicle_id, car_id,
                domain, parent_source_row_id, row_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(source_id, source_table, source_row_id) DO NOTHING",
            params![
                source_id,
                source_table,
                source_row_id,
                vehicle_id,
                car_id,
                domain,
                parent_source_row_id,
                row_json,
            ],
        )
        .map_err(StoreError::LifecycleWrite)
}

fn mark_export_dirty_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO export_outbox(
                vehicle_id, dirty_revision, attempts, next_attempt_ms,
                claimed_until_ms, last_error
             ) VALUES (?1, 1, 0, 0, 0, NULL)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                dirty_revision = export_outbox.dirty_revision + 1,
                -- Keep an active lease fenced to its current publisher. The
                -- terminal transition will release the newer revision without
                -- deleting it; an expired lease remains immediately claimable.
                attempts = CASE WHEN export_outbox.claimed_until_ms > 0
                    THEN export_outbox.attempts ELSE 0 END,
                next_attempt_ms = CASE WHEN export_outbox.claimed_until_ms > 0
                    THEN export_outbox.next_attempt_ms ELSE 0 END,
                last_error = NULL",
            params![vehicle_id.to_string()],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn load_car_settings_row(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<Option<(i64, ProjectionCarSettings)>, StoreError> {
    transaction
        .query_row(
            "SELECT car_id, enabled, use_streaming_api, suspend_after_idle_min,
                    suspend_min, req_not_unlocked, free_supercharging,
                    lfp_battery, suspend_min_resolved
             FROM car_settings WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    ProjectionCarSettings {
                        enabled: row.get::<_, i64>(1)? != 0,
                        use_streaming_api: row.get::<_, i64>(2)? != 0,
                        suspend_after_idle_min: row.get(3)?,
                        suspend_min: row.get(4)?,
                        req_not_unlocked: row.get::<_, i64>(5)? != 0,
                        free_supercharging: row.get::<_, i64>(6)? != 0,
                        lfp_battery: row.get::<_, i64>(7)? != 0,
                        suspend_min_resolved: row.get::<_, i64>(8)? != 0,
                    },
                ))
            },
        )
        .optional()
        .map_err(StoreError::Query)
}

fn record_sync_mutation_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    entity: &str,
    entity_id: i64,
    car_id: i64,
    operation: &str,
    payload_json: &str,
) -> Result<(), StoreError> {
    let next_revision: i64 = transaction
        .query_row(
            "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision)
             VALUES (?1, 2)
             ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = next_revision + 1
             RETURNING next_revision - 1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "INSERT INTO sync_mutations(
                vehicle_id, revision, entity, entity_id, car_id,
                operation, payload_json, published, claimed_until_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0)",
            params![
                vehicle_id.to_string(),
                next_revision,
                entity,
                entity_id,
                car_id,
                operation,
                payload_json,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(())
}

fn parse_sync_entity(value: &str) -> Option<ProjectionDeltaEntity> {
    match value {
        "car" => Some(ProjectionDeltaEntity::Car),
        "car_setting" => Some(ProjectionDeltaEntity::CarSetting),
        "geofence" => Some(ProjectionDeltaEntity::Geofence),
        "address" => Some(ProjectionDeltaEntity::Address),
        "drive" => Some(ProjectionDeltaEntity::Drive),
        "position" => Some(ProjectionDeltaEntity::Position),
        "charge" => Some(ProjectionDeltaEntity::Charge),
        "charge_sample" => Some(ProjectionDeltaEntity::ChargeSample),
        "state" => Some(ProjectionDeltaEntity::State),
        "update" => Some(ProjectionDeltaEntity::Update),
        _ => None,
    }
}

fn load_projection_json<T: DeserializeOwned>(
    connection: &Connection,
    table: &str,
    column: &str,
    id_column: &str,
    mutation: &SyncMutation,
) -> Result<T, StoreError> {
    let sql = format!("SELECT {column} FROM {table} WHERE vehicle_id = ?1 AND {id_column} = ?2");
    let json: Option<String> = connection
        .query_row(
            &sql,
            params![mutation.vehicle_id.to_string(), mutation.entity_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    let json = json.ok_or_else(|| {
        StoreError::SyncMutation(format!(
            "missing materialised {} {}",
            mutation.entity, mutation.entity_id
        ))
    })?;
    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)
}

fn insert_live_delta_span_in_transaction(
    transaction: &Transaction<'_>,
    claim: &SyncMutationClaim,
    delta: &LineageDelta,
) -> Result<(), StoreError> {
    let vehicle_key = claim.vehicle_id.to_string();
    transaction
        .execute(
            "INSERT INTO sync_live_delta_spans(
                vehicle_id, from_sequence, to_sequence,
                from_revision, to_revision, pack_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(vehicle_id, from_sequence, to_sequence) DO NOTHING",
            params![
                vehicle_key.as_str(),
                i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                claim.from_revision,
                claim.to_revision,
                delta.pack_digest.to_string(),
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    let stored: (i64, i64, String) = transaction
        .query_row(
            "SELECT from_revision, to_revision, pack_digest
             FROM sync_live_delta_spans
             WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3",
            params![
                vehicle_key,
                i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(StoreError::LineageCatalog)?;
    if stored
        != (
            claim.from_revision,
            claim.to_revision,
            delta.pack_digest.to_string(),
        )
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

fn address_lookup_key(point: crate::location::Wgs84Point) -> String {
    format!("{:.6}:{:.6}", point.latitude, point.longitude)
}

fn advance_terrain_cursor(
    transaction: &Transaction<'_>,
    candidate: &TerrainCandidate,
    attempted_at_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO terrain_enrichment_state(
                vehicle_id, cursor_position_id, updated_at_ms
             ) VALUES (?1, ?2, ?3)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                cursor_position_id = MAX(cursor_position_id, excluded.cursor_position_id),
                updated_at_ms = excluded.updated_at_ms",
            params![
                candidate.vehicle_id.to_string(),
                candidate.position.id,
                attempted_at_ms,
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn upsert_terrain_provenance(
    transaction: &Transaction<'_>,
    candidate: &TerrainCandidate,
    tile_name: Option<&str>,
    tile_hash: Option<&str>,
    dataset_source: Option<&str>,
    dataset_version: Option<&str>,
    status: &str,
    error_code: Option<&str>,
    retry_after_ms: i64,
    attempted_at_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO terrain_elevation_provenance(
                vehicle_id, position_id, drive_id, latitude, longitude,
                elevation_m, tile_name, tile_hash, dataset_source, dataset_version,
                status, error_code, attempts, attempted_at_ms, retry_after_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                       COALESCE((SELECT attempts FROM terrain_elevation_provenance
                                 WHERE vehicle_id = ?1 AND position_id = ?2), 0) + 1,
                       ?13, ?14)
             ON CONFLICT(vehicle_id, position_id) DO UPDATE SET
                drive_id = excluded.drive_id,
                latitude = excluded.latitude,
                longitude = excluded.longitude,
                elevation_m = excluded.elevation_m,
                tile_name = excluded.tile_name,
                tile_hash = excluded.tile_hash,
                dataset_source = excluded.dataset_source,
                dataset_version = excluded.dataset_version,
                status = excluded.status,
                error_code = excluded.error_code,
                attempts = terrain_elevation_provenance.attempts + 1,
                attempted_at_ms = excluded.attempted_at_ms,
                retry_after_ms = excluded.retry_after_ms",
            params![
                candidate.vehicle_id.to_string(),
                candidate.position.id,
                candidate.position.drive_id,
                candidate.position.latitude,
                candidate.position.longitude,
                candidate.position.elevation,
                tile_name,
                tile_hash,
                dataset_source,
                dataset_version,
                status,
                error_code,
                attempted_at_ms,
                retry_after_ms,
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn recompute_terrain_drive(
    transaction: &Transaction<'_>,
    vehicle_id: &str,
    drive_id: i64,
) -> Result<(), StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT position_json FROM materialised_positions
             WHERE vehicle_id = ?1 AND drive_id = ?2
             ORDER BY position_id ASC",
        )
        .map_err(StoreError::LifecycleWrite)?;
    let rows = statement
        .query_map(params![vehicle_id, drive_id], |row| row.get::<_, String>(0))
        .map_err(StoreError::LifecycleWrite)?;
    let positions: Vec<ProjectionPosition> = rows
        .map(|row| {
            let json = row.map_err(StoreError::LifecycleWrite)?;
            serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)
        })
        .collect::<Result<_, _>>()?;
    let (ascent, descent) = terrain_elevation_totals(&positions);
    let drive_json: String = transaction
        .query_row(
            "SELECT drive_json FROM materialised_drives
             WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_id, drive_id],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    let mut drive: ProjectionDrive =
        serde_json::from_str(&drive_json).map_err(StoreError::DeserializeLifecycleRow)?;
    drive.ascent = Some(ascent);
    drive.descent = Some(descent);
    let drive_json = serde_json::to_string(&drive).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "UPDATE materialised_drives SET drive_json = ?3, ascent = ?4, descent = ?5
             WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_id, drive_id, drive_json, ascent, descent],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn terrain_elevation_totals(positions: &[ProjectionPosition]) -> (i64, i64) {
    let mut previous = None;
    let mut ascent = 0_i64;
    let mut descent = 0_i64;
    for elevation in positions.iter().filter_map(|position| position.elevation) {
        if let Some(previous_elevation) = previous {
            let delta = elevation - previous_elevation;
            if delta > 0 {
                ascent = ascent.saturating_add(delta);
            } else if delta < 0 {
                descent = descent.saturating_add(delta.unsigned_abs() as i64);
            }
        }
        previous = Some(elevation);
    }
    (
        if ascent >= 32_768 { 0 } else { ascent },
        if descent >= 32_768 { 0 } else { descent },
    )
}

fn validate_address_cache_record(record: &AddressCacheRecord) -> Result<(), StoreError> {
    if record.osm_type.is_empty()
        || record.osm_type.len() > 32
        || record.osm_type.chars().any(char::is_control)
        || record.osm_id <= 0
        || record.display_name.trim().is_empty()
        || record.display_name.len() > MAX_DISPLAY_NAME_BYTES
        || invalid_address_text(record.name.as_deref())
        || invalid_address_text(record.house_number.as_deref())
        || invalid_address_text(record.road.as_deref())
        || invalid_address_text(record.neighbourhood.as_deref())
        || invalid_address_text(record.city.as_deref())
        || invalid_address_text(record.county.as_deref())
        || invalid_address_text(record.postcode.as_deref())
        || invalid_address_text(record.state.as_deref())
        || invalid_address_text(record.state_district.as_deref())
        || invalid_address_text(record.country.as_deref())
        || record
            .latitude
            .is_some_and(|latitude| !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude))
        || record.longitude.is_some_and(|longitude| {
            !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude)
        })
        || record.raw_json.as_deref().is_some_and(|raw| {
            raw.len() > MAX_ADDRESS_RAW_JSON_BYTES || serde_json::from_str::<Value>(raw).is_err()
        })
        || !record.lookup_latitude.is_finite()
        || !(-90.0..=90.0).contains(&record.lookup_latitude)
        || !record.lookup_longitude.is_finite()
        || !(-180.0..=180.0).contains(&record.lookup_longitude)
        || record.looked_up_at_ms < 0
    {
        return Err(StoreError::InvalidAddressCache);
    }
    Ok(())
}

fn invalid_address_text(value: Option<&str>) -> bool {
    value.is_some_and(|text| {
        text.len() > MAX_DISPLAY_NAME_BYTES || text.chars().any(char::is_control)
    })
}

fn load_geofence_fences(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<Vec<crate::lifecycle::GeofenceFence>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT name, latitude, longitude, radius_m, billing_type,
                    cost_per_unit, session_fee
             FROM geofences WHERE vehicle_id = ?1 ORDER BY source_geofence_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id.to_string()], |row| {
            Ok(crate::lifecycle::GeofenceFence {
                name: row.get(0)?,
                latitude: row.get(1)?,
                longitude: row.get(2)?,
                radius_m: row.get(3)?,
                billing_type: row
                    .get::<_, Option<String>>(4)?
                    .map(|value| match value.as_str() {
                        "per_kwh" => crate::hub_pack::GeofenceBillingType::PerKwh,
                        "per_minute" => crate::hub_pack::GeofenceBillingType::PerMinute,
                        _ => crate::hub_pack::GeofenceBillingType::PerKwh,
                    }),
                cost_per_unit: row.get(5)?,
                session_fee: row.get(6)?,
            })
        })
        .map_err(StoreError::Query)?;
    rows.map(|row| row.map_err(StoreError::Query)).collect()
}

fn enqueue_address_jobs(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    delta: &crate::lifecycle::LifecycleDelta,
) -> Result<(), StoreError> {
    for drive in &delta.drives {
        let endpoints = [
            (
                "start_address",
                drive.start_latitude,
                drive.start_longitude,
                drive.start_address.is_some(),
            ),
            (
                "end_address",
                drive.end_latitude,
                drive.end_longitude,
                drive.end_address.is_some(),
            ),
        ];
        for (field, latitude, longitude, already_labeled) in endpoints {
            if already_labeled {
                continue;
            }
            let (Some(latitude), Some(longitude)) = (latitude, longitude) else {
                continue;
            };
            if latitude.is_finite()
                && longitude.is_finite()
                && (-90.0..=90.0).contains(&latitude)
                && (-180.0..=180.0).contains(&longitude)
            {
                insert_address_job(
                    transaction,
                    vehicle_id,
                    "drive",
                    drive.id,
                    field,
                    latitude,
                    longitude,
                )?;
            }
        }
    }
    for charge in &delta.charges {
        if charge.address.is_some() {
            continue;
        }
        let Some((_, latitude, longitude)) = delta
            .charge_start_coordinates
            .iter()
            .find(|(id, _, _)| *id == charge.id)
        else {
            continue;
        };
        if latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(latitude)
            && (-180.0..=180.0).contains(longitude)
        {
            insert_address_job(
                transaction,
                vehicle_id,
                "charge",
                charge.id,
                "address",
                *latitude,
                *longitude,
            )?;
        }
    }
    Ok(())
}

fn insert_address_job(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    target_type: &str,
    target_id: i64,
    field: &str,
    latitude: f64,
    longitude: f64,
) -> Result<(), StoreError> {
    let job_key = format!("{vehicle_id}:{target_type}:{target_id}:{field}");
    transaction
        .execute(
            "INSERT INTO address_enrichment_jobs(
                job_key, vehicle_id, target_type, target_id, field,
                latitude, longitude, status, attempts, next_attempt_ms,
                lease_until_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', 0, 0, 0)
             ON CONFLICT(vehicle_id, target_type, target_id, field) DO NOTHING",
            params![
                job_key,
                vehicle_id.to_string(),
                target_type,
                target_id,
                field,
                latitude,
                longitude
            ],
        )
        .map_err(StoreError::AddressEnrichmentWrite)?;
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_file_hex(path: &Path) -> Result<String, StoreError> {
    let mut file = fs::File::open(path).map_err(StoreError::OpenBackupPack)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(StoreError::ReadBackupPack)?;
        if read == 0 {
            return Ok(hex::encode(digest.finalize()));
        }
        digest.update(&buffer[..read]);
    }
}

fn immutable_catalogue_fingerprint(
    database_path: &Path,
) -> Result<ImmutableCatalogueFingerprint, StoreError> {
    let metadata = fs::symlink_metadata(database_path).map_err(StoreError::InspectCatalogue)?;
    if !metadata.file_type().is_file() {
        return Err(StoreError::InvalidCataloguePath);
    }

    let mut wal_name = database_path.as_os_str().to_os_string();
    wal_name.push("-wal");
    let wal_path = PathBuf::from(wal_name);
    match fs::symlink_metadata(&wal_path) {
        Ok(wal) if wal.len() != 0 => return Err(StoreError::PendingCatalogueWal),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(StoreError::InspectCatalogue(error)),
    }

    let mut file = File::open(database_path).map_err(StoreError::ReadCatalogue)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(StoreError::ReadCatalogue)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(ImmutableCatalogueFingerprint {
        bytes: metadata.len(),
        sha256: hex::encode(digest.finalize()),
    })
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("cannot create data directory: {0}")]
    CreateDataDir(std::io::Error),
    #[error("cannot create packs directory: {0}")]
    CreatePacksDir(std::io::Error),
    #[error("cannot protect data directory: {0}")]
    ProtectDataDir(std::io::Error),
    #[error("cannot protect packs directory: {0}")]
    ProtectPacksDir(std::io::Error),
    #[error("cannot create shared Hub SQLite file: {0}")]
    CreateSharedSqlite(std::io::Error),
    #[error("cannot inspect shared Hub SQLite file: {0}")]
    InspectSharedSqlite(std::io::Error),
    #[error("cannot protect shared Hub SQLite file: {0}")]
    ProtectSharedSqlite(std::io::Error),
    #[error("shared Hub SQLite file has unsafe type or mode: {0}")]
    UnsafeSharedSqlite(PathBuf),
    #[error("cannot create Hub-private import spool: {0}")]
    CreateImportSpool(std::io::Error),
    #[error("cannot inspect Hub-private import spool: {0}")]
    InspectImportSpool(std::io::Error),
    #[error("Hub-private import spool has unsafe type or mode: {0}")]
    UnsafeImportSpool(PathBuf),
    #[error("cannot open hub database: {0}")]
    Open(rusqlite::Error),
    #[error("cannot inspect hub catalogue: {0}")]
    InspectCatalogue(std::io::Error),
    #[error("cannot read hub catalogue: {0}")]
    ReadCatalogue(std::io::Error),
    #[error("cannot resolve hub catalogue path: {0}")]
    ResolveCataloguePath(std::io::Error),
    #[error("hub catalogue path cannot be represented as a SQLite file URI")]
    InvalidCataloguePath,
    #[error("hub catalogue has a pending WAL and is not an immutable snapshot")]
    PendingCatalogueWal,
    #[error("immutable catalogue snapshot mode is required")]
    ImmutableSnapshotRequired,
    #[error("hub catalogue changed during the immutable diagnostic check")]
    CatalogueChangedDuringImmutableCheck,
    #[error("cannot configure hub database: {0}")]
    Configure(rusqlite::Error),
    #[error("TeslaMate token pair is empty")]
    TeslaMateTokenPairEmpty,
    #[error("TeslaMate token ciphertext exceeds the fixed size limit")]
    TeslaMateTokenCiphertextTooLarge,
    #[error("TeslaMate token refresh schedule is invalid")]
    InvalidTeslaMateTokenSchedule,
    #[error("cannot access TeslaMate token store: {0}")]
    TeslaMateTokenStore(rusqlite::Error),
    #[error("invalid address cache record")]
    InvalidAddressCache,
    #[error("cannot write address cache: {0}")]
    AddressCacheWrite(rusqlite::Error),
    #[error("invalid address enrichment result")]
    InvalidAddressEnrichment,
    #[error("cannot write address enrichment job: {0}")]
    AddressEnrichmentWrite(rusqlite::Error),
    #[error("cannot create Hub SQLite backup: {0}")]
    Backup(rusqlite::Error),
    #[error("Hub SQLite backup destination already exists: {0}")]
    BackupDestinationExists(PathBuf),
    #[error("Hub SQLite backup destination must not be the live catalogue")]
    BackupDestinationIsLiveDatabase,
    #[error("cannot create Hub backup directory: {0}")]
    CreateBackupDirectory(std::io::Error),
    #[error("cannot copy Hub backup pack from {source_path} to {destination}: {source_error}")]
    CopyBackupPack {
        source_path: PathBuf,
        destination: PathBuf,
        source_error: std::io::Error,
    },
    #[error("Hub backup pack {path} is {actual} bytes; expected {expected}")]
    BackupPackSizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("cannot open Hub backup pack: {0}")]
    OpenBackupPack(std::io::Error),
    #[error("cannot read Hub backup pack: {0}")]
    ReadBackupPack(std::io::Error),
    #[error("Hub backup pack digest mismatches its catalogue: {path}")]
    BackupPackDigestMismatch { path: PathBuf },
    #[error("cannot migrate hub database: {0}")]
    Migrate(rusqlite::Error),
    #[error("database query failed: {0}")]
    Query(rusqlite::Error),
    #[error("cannot begin local transaction: {0}")]
    Begin(rusqlite::Error),
    #[error("cannot write supervised collector lease: {0}")]
    SupervisedCollectorLeaseWrite(rusqlite::Error),
    #[error("another supervised collector owns the live lease")]
    SupervisedCollectorLeaseHeld,
    #[error("supervised collector lease was lost or expired")]
    SupervisedCollectorLeaseLost,
    #[error("supervised collector lease clock overflowed")]
    SupervisedCollectorClockOverflow,
    #[error("cannot open Hub publication gate: {0}")]
    OpenPublicationGate(std::io::Error),
    #[error("cannot protect Hub publication gate: {0}")]
    ProtectPublicationGate(std::io::Error),
    #[error("Hub publication gate metadata is unsafe: {0}")]
    UnsafePublicationGate(PathBuf),
    #[error("cannot acquire Hub publication gate: {0}")]
    LockPublicationGate(std::io::Error),
    #[error("Hub publication gate is busy")]
    PublicationGateBusy,
    #[error("cannot publish sync manifest: {0}")]
    PublishManifest(rusqlite::Error),
    #[error("cannot associate a snapshot fingerprint with uncatalogued manifest {0}")]
    FingerprintManifestMissing(Uuid),
    #[error(
        "changed-history import must publish a typed delta bound to the immutable base snapshot"
    )]
    ImportDeltaRequiresBaseBinding,
    #[error("invalid stored vehicle identity")]
    InvalidVehicleId,
    #[error("invalid vehicle identity value")]
    InvalidVehicleIdentity,
    #[error("vehicle identity conflicts across sources")]
    VehicleIdentityConflict,
    #[error("invalid stored source identity")]
    InvalidSourceId,
    #[error("terrain materialised car is missing for {0}")]
    TerrainCarMissing(Uuid),
    #[error("cannot publish terrain projection pack: {0}")]
    TerrainPack(ProjectionPackError),
    #[error("cannot register source: {0}")]
    RegisterSource(rusqlite::Error),
    #[error("cannot register vehicle: {0}")]
    RegisterVehicle(rusqlite::Error),
    #[error("cannot create pairing invitation: {0}")]
    CreatePairing(rusqlite::Error),
    #[error("cannot revoke pairing invitation: {0}")]
    RevokePairing(rusqlite::Error),
    #[error("cannot claim pairing invitation: {0}")]
    ClaimPairing(rusqlite::Error),
    #[error("cannot append raw observation: {0}")]
    AppendObservation(rusqlite::Error),
    #[error("cannot initialise Hub installation identity: {0}")]
    InstallationIdentity(rusqlite::Error),
    #[error("cannot serialize sync manifest: {0}")]
    SerializeManifest(serde_json::Error),
    #[error("cannot deserialize sync manifest: {0}")]
    DeserializeManifest(serde_json::Error),
    #[error(
        "schema {0:?} is recognized but cannot be catalogued or served until its pack, catalogue, and receiver implementation exists"
    )]
    SchemaPublicationUnavailable(crate::protocol::SchemaVersion),
    #[error("cannot write schema 2.2 no-op: {0}")]
    WriteSchema22NoOp(std::io::Error),
    #[error("cannot read schema 2.2 no-op: {0}")]
    ReadSchema22NoOp(std::io::Error),
    #[error("cannot access schema 2.2 no-op storage: {0}")]
    AccessSchema22NoOp(std::io::Error),
    #[error("schema 2.2 no-op storage has unsafe type, ownership, mode, or name: {0}")]
    UnsafeSchema22NoOpPath(PathBuf),
    #[error("schema 2.2 no-op directory is absent")]
    Schema22NoOpNotFound,
    #[error("schema 2.2 manifest for vehicle {0} requires paired publication")]
    Schema22PairPublicationRequired(Uuid),
    #[error("schema 2.2 manifest/no-op pair is invalid: {0}")]
    InvalidSchema22Pair(String),
    #[error("schema 2.2 snapshot {snapshot_id} for vehicle {vehicle_id} is immutable")]
    Schema22SnapshotConflict { vehicle_id: Uuid, snapshot_id: Uuid },
    #[error("cannot access import generation: {0}")]
    ImportGeneration(rusqlite::Error),
    #[error("import generation is invalid")]
    InvalidImportGeneration,
    #[error("import generation was not found or is not staging")]
    ImportGenerationNotFound,
    #[error("import generation promotion became unsettled by newer live state")]
    ImportGenerationConflict,
    #[error("cannot access lineage catalogue: {0}")]
    LineageCatalog(rusqlite::Error),
    #[error("lineage catalogue conflicts with an existing sequence")]
    LineageCatalogConflict,
    #[error("V2 lineage has no safely compactable collector delta suffix")]
    LineageCompactionUnavailable,
    #[error("V2 lineage cannot accept another pack within the client protocol limits")]
    LineageCapacityExhausted,
    #[error("cannot read the store clock for retired-lineage pack retention: {0}")]
    RetiredLineageClock(std::time::SystemTimeError),
    #[error("retired-lineage pack retention clock does not fit epoch milliseconds")]
    RetiredLineageClockOverflow,
    #[error(
        "immutable V2 base binding is missing for {0}; refusing to reconstruct it from mutable source state"
    )]
    ImmutableBaseBindingMissing(Uuid),
    #[error(
        "TeslaMate imported-history inventory is missing for {0}; refusing a changed import without exact deletion provenance"
    )]
    TeslaMateImportInventoryMissing(Uuid),
    #[error(
        "TeslaMate durable projection digest state is missing for {0}; legacy inventory cannot prove changed-row payloads"
    )]
    TeslaMateImportProjectionStateMissing(Uuid),
    #[error(
        "TeslaMate legacy direct-import base for {0} cannot be proved unchanged; rebase_required"
    )]
    TeslaMateLegacyDirectRebaseRequired(Uuid),
    #[error("TeslaMate projection-state capture failed: {0}")]
    TeslaMateProjectionState(#[from] TeslaMateProjectionStateError),
    #[error("invalid sync mutation: {0}")]
    SyncMutation(String),
    #[error("lineage pack is not verified and ready")]
    LineagePackNotReady,
    #[error("lineage pack digest does not match its content")]
    LineagePackDigestMismatch,
    #[error("cannot open stored lineage pack: {0}")]
    OpenLineagePack(std::io::Error),
    #[error("cannot decode stored lineage pack: {0}")]
    DecodeLineagePack(std::io::Error),
    #[error("cannot create transient lineage pack inspection {path}: {source}")]
    CreateLineagePackInspection {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot flush transient lineage pack inspection: {0}")]
    SyncLineagePackInspection(std::io::Error),
    #[error("cannot serialize raw observation: {0}")]
    SerializeObservation(serde_json::Error),
    #[error("invalid sync manifest: {0}")]
    Manifest(crate::protocol::ProtocolError),
    #[error("sync sequence does not fit SQLite signed integer")]
    SequenceTooLarge,
    #[error("sync sequence is exhausted")]
    SequenceExhausted,
    #[error("stored sync sequence is invalid")]
    InvalidStoredSequence,
    #[error(
        "manifest sequence {attempted} is stale; current sequence is {current} for {vehicle_id}"
    )]
    StaleManifest {
        vehicle_id: Uuid,
        attempted: u64,
        current: u64,
    },
    #[error("pack size does not fit SQLite signed integer")]
    PackSizeTooLarge,
    #[error("lineage pack ordinal does not fit the protocol")]
    PackOrdinalTooLarge,
    #[error("stored pack path is not canonical")]
    UnsafeStoredPackPath,
    #[error("cannot inspect catalogue pack {path}: {source}")]
    InspectCatalogPack {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("catalogue pack is not a regular file: {path}")]
    CatalogPackNotRegular { path: PathBuf },
    #[error("catalogue pack {path} is {actual} bytes; expected {expected}")]
    CatalogPackSizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("catalogue pack digest mismatches its record: {path}")]
    CatalogPackDigestMismatch { path: PathBuf },
    #[error("{0} must not be empty")]
    EmptyIdentity(&'static str),
    #[error("{field} is {actual} bytes; maximum is {maximum}")]
    IdentityTooLong {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("{0} must not contain control characters")]
    IdentityControlCharacter(&'static str),
    #[error("source kind must be lowercase ASCII letters, digits, hyphens, or underscores")]
    InvalidSourceKind,
    #[error("{0} must be an epoch timestamp in milliseconds")]
    NegativeTimestamp(&'static str),
    #[error("pairing invitation expiry must be later than its creation time")]
    InvalidPairingExpiry,
    #[error("pairing invitation was rejected")]
    PairingRejected,
    #[error("source id must not be nil")]
    NilSourceId,
    #[error("vehicle id must not be nil")]
    NilVehicleId,
    #[error("raw observation payload must be a JSON object")]
    ObservationMustBeObject,
    #[error("raw observation is {actual} bytes; maximum is {maximum}")]
    ObservationTooLarge { actual: usize, maximum: usize },
    #[error("raw observation is missing after a successful insert")]
    ObservationMissingAfterInsert,
    #[error("raw observation query limit {actual} must be between 1 and {maximum}")]
    InvalidObservationQueryLimit { actual: u32, maximum: u32 },
    #[error("raw observation query time range is empty or reversed")]
    InvalidObservationQueryRange,
    #[error("unknown source {0}")]
    UnknownSource(Uuid),
    #[error("unknown vehicle {0}")]
    UnknownVehicle(Uuid),
    #[error("vehicle {vehicle_id} does not belong to source {source_id}")]
    VehicleSourceMismatch { vehicle_id: Uuid, source_id: Uuid },
    #[error("stored vehicle identity {actual} differs from expected identity {expected}")]
    VehicleIdentityMismatch { expected: Uuid, actual: Uuid },
    #[error("stored {0} is not a valid UUID")]
    InvalidStoredUuid(&'static str),
    #[error("stored source generation is invalid")]
    InvalidStoredGeneration,
    #[error("stored count is invalid")]
    InvalidStoredCount,
    #[error("{0} lifecycle session(s) require reconstruction")]
    QuarantinedLifecycle(usize),
    #[error("unsupported hub schema version {0}")]
    UnsupportedSchema(i32),
    #[error("unexpected hub SQLite application id {0}")]
    InvalidApplicationId(i32),
    #[error("database integrity check failed: {0}")]
    Integrity(String),
    #[error("lifecycle car id must be positive")]
    InvalidLifecycleCarId,
    #[error("lifecycle observation cursor is invalid")]
    InvalidLifecycleCursor,
    #[error("lifecycle open-session payload is invalid")]
    InvalidLifecycleSession,
    #[error("cannot write lifecycle history: {0}")]
    LifecycleWrite(rusqlite::Error),
    #[error("cannot project lifecycle: {0}")]
    LifecycleProjection(crate::lifecycle::LifecycleError),
    #[error("injected stream fault at {0}")]
    InjectedStreamFault(&'static str),
    #[cfg(test)]
    #[error("injected projection-state detach fault")]
    InjectedProjectionStateDetachFault,
    #[error("cannot serialize lifecycle history row: {0}")]
    SerializeLifecycleRow(serde_json::Error),
    #[error("cannot deserialize lifecycle history row: {0}")]
    DeserializeLifecycleRow(serde_json::Error),
    #[error("cannot access outbound request receipt: {0}")]
    OutboundRequestReceipt(rusqlite::Error),
    #[error("outbound request receipt id must be positive")]
    InvalidOutboundRequestReceiptId,
    #[error("outbound request receipt is missing or already terminal")]
    OutboundRequestReceiptNotStarted,
    #[error("outbound request correlation id must not be nil")]
    NilOutboundRequestCorrelationId,
    #[error("outbound request vehicle id must be positive")]
    InvalidOutboundRequestVehicleId,
    #[error("vehicle_data audit records require conditional_read and stream_power_confirmed")]
    InvalidVehicleDataAuditPrecondition,
    #[error("cannot read the store clock for outbound request auditing: {0}")]
    OutboundRequestClock(std::time::SystemTimeError),
    #[error("outbound request audit clock does not fit epoch milliseconds")]
    OutboundRequestClockOverflow,
    #[error("outbound request HTTP status must be between 100 and 599")]
    InvalidOutboundRequestHttpStatus,
    #[error("outbound request Retry-After does not fit a signed epoch-safe integer")]
    InvalidOutboundRequestRetryAfter,
    #[error("outbound request watermark must be non-negative")]
    InvalidOutboundRequestWatermark,
    #[error("outbound request query limit {actual} must be between 1 and {maximum}")]
    InvalidOutboundRequestQueryLimit { actual: u32, maximum: u32 },
    #[error("outbound request audit has no room without deleting an unresolved receipt")]
    OutboundRequestAuditCapacityExhausted,
    #[error("cannot access stream session receipt: {0}")]
    StreamSessionReceipt(rusqlite::Error),
    #[error("stream session receipt id must be positive")]
    InvalidStreamSessionReceiptId,
    #[error("stream session receipt is missing or already terminal")]
    StreamSessionReceiptNotStarted,
    #[error("stream session requires a successful matching unsubscribe receipt")]
    StreamSessionUnsubscribeNotCompleted,
    #[error("stream session audit has no room without deleting an unresolved session")]
    StreamSessionAuditCapacityExhausted,
}

#[derive(Debug, Error)]
pub enum ObservationVerificationError {
    #[error("source car id must be positive")]
    InvalidSourceCarId,
    #[error("watermark must be non-negative")]
    InvalidWatermark,
    #[error("no vehicle mapping for source car")]
    NoVehicleMapping,
    #[error("ambiguous vehicle mapping for source car")]
    AmbiguousVehicleMapping,
    #[error("observation query failed: {0}")]
    Store(#[from] StoreError),
}

impl ObservationVerificationError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSourceCarId => "invalid_source_car_id",
            Self::InvalidWatermark => "invalid_watermark",
            Self::NoVehicleMapping => "no_vehicle_mapping",
            Self::AmbiguousVehicleMapping => "ambiguous_vehicle_mapping",
            Self::Store(_) => "database_error",
        }
    }
}

#[derive(Debug, Error)]
pub enum NoWakeVerificationError {
    #[error("audit watermark must be non-negative")]
    InvalidAuditWatermark,
    #[error("no-wake audit query failed: {0}")]
    Store(#[from] StoreError),
    #[error("observation verification failed: {0}")]
    Observation(#[from] ObservationVerificationError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V3, LINEAGE_PROTOCOL_V2, LineageBase,
        LineageCapability, LineageDelta, LineageManifestV2, MirrorTable, OpaqueCursor,
        PackCompression, PackFormat, ProtocolError, ProtocolVersion, SchemaVersion, SequenceRange,
        TransferMode,
    };

    fn tree_contents(root: &Path) -> Vec<(PathBuf, u32, Option<(u64, String)>)> {
        fn visit(
            root: &Path,
            directory: &Path,
            entries: &mut Vec<(PathBuf, u32, Option<(u64, String)>)>,
        ) {
            for entry in fs::read_dir(directory).expect("read snapshot directory") {
                let entry = entry.expect("read snapshot entry");
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).expect("snapshot metadata");
                let relative = path
                    .strip_prefix(root)
                    .expect("snapshot path below root")
                    .to_path_buf();
                if metadata.is_dir() {
                    entries.push((relative, metadata.permissions().mode(), None));
                    visit(root, &path, entries);
                } else {
                    let bytes = fs::read(&path).expect("snapshot file");
                    entries.push((
                        relative,
                        metadata.permissions().mode(),
                        Some((metadata.len(), hex::encode(Sha256::digest(bytes)))),
                    ));
                }
            }
        }

        let mut entries = Vec::new();
        visit(root, root, &mut entries);
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    #[test]
    fn initializes_a_checked_wal_database() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        store.quick_check().expect("database passes quick check");
        assert!(store.database_path().exists());
        assert!(store.packs_dir().is_dir());

        let connection = store.open().expect("reopen store");
        let journal: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode");
        assert_eq!(journal, "wal");
        let application_id: i32 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .expect("application id");
        assert_eq!(application_id, APPLICATION_ID);
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
        assert_eq!(
            store.sqlite_version().expect("SQLite version"),
            BUNDLED_SQLITE_VERSION
        );
        assert!(!store.installation_id().expect("installation ID").is_nil());
    }

    #[test]
    fn teslamate_token_import_loads_and_reopens_as_one_row() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let imported = TeslaMateLegacyTokenStore::imported(
            b"access-ciphertext".to_vec(),
            b"refresh-ciphertext".to_vec(),
        )
        .expect("imported pair is valid");

        store
            .replace_teslamate_legacy_tokens(&imported)
            .expect("imported pair stores");
        let reopened = HubStore::initialize(temporary.path()).expect("store reopens");
        let loaded = reopened
            .load_teslamate_legacy_tokens()
            .expect("pair loads")
            .expect("pair exists");

        assert_eq!(loaded.access(), imported.access());
        assert_eq!(loaded.refresh(), imported.refresh());
        assert_eq!(loaded.expires_at(), 0);
        assert_eq!(loaded.next_refresh_at(), 0);
        let connection = reopened.open().expect("database opens");
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM teslamate_legacy_tokens", [], |row| {
                row.get(0)
            })
            .expect("row count");
        assert_eq!(rows, 1);
    }

    #[test]
    fn teslamate_token_replacement_is_atomic_and_refresh_schedule_is_strict() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let imported = TeslaMateLegacyTokenStore::imported(
            b"old-access-ciphertext".to_vec(),
            b"old-refresh-ciphertext".to_vec(),
        )
        .expect("imported pair is valid");
        store
            .replace_teslamate_legacy_tokens(&imported)
            .expect("imported pair stores");

        assert!(matches!(
            TeslaMateLegacyTokenStore::refreshed(
                b"new-access-ciphertext".to_vec(),
                b"new-refresh-ciphertext".to_vec(),
                1_000,
                1_000,
            ),
            Err(StoreError::InvalidTeslaMateTokenSchedule)
        ));

        let refreshed = TeslaMateLegacyTokenStore::refreshed(
            b"new-access-ciphertext".to_vec(),
            b"new-refresh-ciphertext".to_vec(),
            2_000,
            1_000,
        )
        .expect("refreshed schedule is valid");
        store
            .replace_teslamate_legacy_tokens(&refreshed)
            .expect("replacement commits");
        let loaded = store
            .load_teslamate_legacy_tokens()
            .expect("pair loads")
            .expect("pair exists");
        assert_eq!(loaded.access(), refreshed.access());
        assert_eq!(loaded.refresh(), refreshed.refresh());
        assert_eq!(loaded.expires_at(), 2_000);
        assert_eq!(loaded.next_refresh_at(), 1_000);
    }

    #[cfg(unix)]
    #[test]
    fn private_sqlite_catalogue_is_0600_and_only_tightens_0640() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let path = store.database_path();
        let expected_gid = fs::symlink_metadata(temporary.path())
            .expect("data-root metadata")
            .gid();

        let metadata = fs::symlink_metadata(path).expect("catalogue metadata");
        assert_eq!(metadata.gid(), expected_gid);
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            SHARED_SQLITE_FILE_MODE
        );

        // Tighten SQLite's common 0640 creation mode, but reject any unrelated
        // historical mode rather than silently changing an unknown file.
        fs::set_permissions(path, fs::Permissions::from_mode(0o640))
            .expect("simulate own interrupted SQLite mode");
        ensure_shared_sqlite_catalogue_file(path).expect("repair own 0640 catalogue");
        assert_eq!(
            fs::symlink_metadata(path)
                .expect("repaired catalogue metadata")
                .permissions()
                .mode()
                & 0o777,
            SHARED_SQLITE_FILE_MODE
        );

        fs::set_permissions(path, fs::Permissions::from_mode(0o660))
            .expect("simulate incompatible old catalogue mode");
        assert!(matches!(
            ensure_shared_sqlite_catalogue_file(path),
            Err(StoreError::UnsafeSharedSqlite(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn schema_22_noop_directory_is_shared_setgid_and_rejects_mode_or_symlink_substitution() {
        let wrong_mode = tempfile::tempdir().expect("wrong-mode store");
        let store = HubStore::initialize(wrong_mode.path()).expect("store initializes");
        let noop = store.packs_dir().join("noop");
        let expected_gid = fs::symlink_metadata(store.packs_dir())
            .expect("packs metadata")
            .gid();
        let metadata = fs::symlink_metadata(&noop).expect("no-op metadata");
        assert_eq!(metadata.gid(), expected_gid);
        assert_eq!(
            metadata.permissions().mode() & 0o7777,
            SHARED_SCHEMA_22_NOOP_DIRECTORY_MODE
        );
        drop(store);
        fs::set_permissions(&noop, fs::Permissions::from_mode(0o770))
            .expect("weaken setgid contract");
        assert!(matches!(
            HubStore::initialize(wrong_mode.path()),
            Err(StoreError::UnsafeSchema22NoOpPath(_))
        ));

        let symlinked = tempfile::tempdir().expect("symlink store");
        let store = HubStore::initialize(symlinked.path()).expect("store initializes");
        let noop = store.packs_dir().join("noop");
        drop(store);
        fs::remove_dir(&noop).expect("remove empty no-op directory");
        let outside = symlinked.path().join("outside");
        fs::create_dir(&outside).expect("outside directory");
        std::os::unix::fs::symlink(&outside, &noop).expect("substitute no-op symlink");
        assert!(matches!(
            HubStore::initialize(symlinked.path()),
            Err(StoreError::AccessSchema22NoOp(_)) | Err(StoreError::UnsafeSchema22NoOpPath(_))
        ));
    }

    #[test]
    fn import_spool_and_publication_gate_are_private_children() {
        let root = Path::new("/tmp/teslatlas-user-hub");
        assert_eq!(
            private_import_spool_root(root),
            root.join(PRIVATE_IMPORT_SPOOL_DIRECTORY_NAME)
        );
        assert_eq!(publication_lock_path(root), root.join(".publication.lock"));
    }

    #[cfg(unix)]
    #[test]
    fn publication_gate_rejects_an_incompatible_existing_lock_inode() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        fs::set_permissions(
            &store.publication_lock_path,
            fs::Permissions::from_mode(0o640),
        )
        .expect("weaken private lock mode");

        assert!(matches!(
            store.try_acquire_publication_gate(),
            Err(StoreError::UnsafePublicationGate(_))
        ));
    }

    #[test]
    fn supervised_collector_lease_fences_kill_stale_and_recovery_transitions() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        assert_eq!(
            store
                .service_readiness_at(true, 1_000)
                .expect_err("required collector is initially absent")
                .code,
            ReadinessReasonCode::CollectorAbsent
        );

        let first = store
            .acquire_supervised_collector_lease(1_000)
            .expect("first collector lease");
        let competing_process =
            HubStore::initialize(temporary.path()).expect("second process store handle");
        store
            .service_readiness_at(true, 1_001)
            .expect("live collector is ready without any stream sessions");
        assert!(matches!(
            competing_process.acquire_supervised_collector_lease(1_001),
            Err(StoreError::SupervisedCollectorLeaseHeld)
        ));

        store
            .heartbeat_supervised_collector_lease(
                first,
                SupervisedCollectorState::AuthenticationTerminal,
                2_000,
            )
            .expect("terminal auth heartbeat");
        assert_eq!(
            store
                .service_readiness_at(true, 2_001)
                .expect_err("terminal auth fails readiness")
                .code,
            ReadinessReasonCode::CollectorAuthTerminal
        );
        store
            .heartbeat_supervised_collector_lease(first, SupervisedCollectorState::Active, 3_000)
            .expect("authenticated recovery heartbeat");
        store
            .service_readiness_at(true, 3_001)
            .expect("authenticated recovery restores readiness");

        let expired_at = 3_000 + SUPERVISED_COLLECTOR_LEASE_MS;
        assert_eq!(
            store
                .service_readiness_at(true, expired_at)
                .expect_err("killed collector becomes stale at lease boundary")
                .code,
            ReadinessReasonCode::CollectorStale
        );
        store
            .heartbeat_supervised_collector_lease(
                first,
                SupervisedCollectorState::Active,
                expired_at + 1,
            )
            .expect("delayed owner revives its readiness record");
        assert!(matches!(
            competing_process.acquire_supervised_collector_lease(expired_at + 1),
            Err(StoreError::SupervisedCollectorLeaseHeld)
        ));
        let replacement_at = expired_at + 1 + SUPERVISED_COLLECTOR_LEASE_MS;
        let replacement = competing_process
            .acquire_supervised_collector_lease(replacement_at)
            .expect("replacement takes over stale lease");
        assert!(matches!(
            store.heartbeat_supervised_collector_lease(
                first,
                SupervisedCollectorState::Active,
                replacement_at + 1,
            ),
            Err(StoreError::SupervisedCollectorLeaseLost)
        ));
        store
            .release_supervised_collector_lease(first)
            .expect("stale release is harmless");
        competing_process
            .service_readiness_at(true, replacement_at + 1)
            .expect("stale release cannot clear replacement");
        competing_process
            .release_supervised_collector_lease(replacement)
            .expect("replacement releases exactly its lease");
        assert_eq!(
            store
                .service_readiness_at(true, replacement_at + 1)
                .expect_err("orderly stop is immediately absent")
                .code,
            ReadinessReasonCode::CollectorAbsent
        );
    }

    #[test]
    fn fast_readiness_checks_pack_servability_without_hashing_content() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let manifest = test_manifest();
        let pack = &manifest.chunks[0];
        let path = store
            .packs_dir()
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        fs::create_dir_all(path.parent().expect("pack parent")).expect("pack parent");
        fs::write(&path, vec![7_u8; 100]).expect("pack file");
        store
            .publish_manifest(&manifest)
            .expect("published manifest");
        store
            .service_readiness_at(false, 1_000)
            .expect("published regular pack is cheaply servable");

        fs::remove_file(&path).expect("remove pack");
        assert_eq!(
            store
                .service_readiness_at(false, 1_000)
                .expect_err("missing published pack")
                .code,
            ReadinessReasonCode::PublishedContentUnservable
        );
        fs::write(&path, vec![7_u8; 99]).expect("truncated pack");
        assert_eq!(
            store
                .service_readiness_at(false, 1_000)
                .expect_err("truncated published pack")
                .code,
            ReadinessReasonCode::PublishedContentUnservable
        );
        fs::remove_file(&path).expect("remove truncated pack");
        fs::create_dir(&path).expect("non-regular pack path");
        assert_eq!(
            store
                .service_readiness_at(false, 1_000)
                .expect_err("non-regular published pack")
                .code,
            ReadinessReasonCode::PublishedContentUnservable
        );
        fs::remove_dir(&path).expect("remove non-regular pack path");

        // The fast probe intentionally does not claim same-size integrity.
        fs::write(&path, vec![8_u8; 100]).expect("same-size corrupt pack");
        store
            .service_readiness_at(false, 1_000)
            .expect("same-size content belongs to the full doctor gate");
        assert!(matches!(
            store.catalogue_check(),
            Err(StoreError::CatalogPackDigestMismatch { .. })
        ));
    }

    #[test]
    fn upgrades_v42_with_supervised_collector_lease_schema() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("current store");
        let connection = store.open().expect("current catalogue");
        connection
            .execute_batch(
                "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 PRAGMA user_version = 42;",
            )
            .expect("recreate historical v42 boundary");
        drop(connection);

        let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v42 store");
        let connection = upgraded.open().expect("upgraded catalogue");
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
        let schema: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'supervised_collector_lease'",
                [],
                |row| row.get(0),
            )
            .expect("collector lease schema");
        assert!(schema.contains("auth_terminal"));
        assert!(schema.contains("singleton_id = 1"));
    }

    #[test]
    fn upgrades_v43_with_legacy_refresh_receipt_binding_schema() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("current store");
        let connection = store.open().expect("current catalogue");
        connection
            .execute_batch(
                "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 PRAGMA user_version = 43;",
            )
            .expect("recreate historical v43 boundary");
        drop(connection);

        let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v43 store");
        let connection = upgraded.open().expect("upgraded catalogue");
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
        let schema: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'legacy_refresh_receipt_bindings'",
                [],
                |row| row.get(0),
            )
            .expect("refresh receipt binding schema");
        assert!(schema.contains("output_credential_generation"));
        assert!(schema.contains("ON DELETE CASCADE"));
    }

    #[test]
    fn read_only_catalogue_check_does_not_change_the_store_tree() {
        let temporary = tempfile::tempdir().expect("temporary database");
        HubStore::initialize(temporary.path()).expect("store initializes");
        let before = tree_contents(temporary.path());
        let store = HubStore::open_immutable_read_only(temporary.path()).expect("immutable store");
        store.catalogue_check().expect("read-only catalogue check");
        assert_eq!(
            store.sqlite_version().expect("read-only SQLite version"),
            BUNDLED_SQLITE_VERSION
        );
        store
            .verify_immutable_snapshot_unchanged()
            .expect("immutable snapshot remains unchanged");
        drop(store);
        assert_eq!(tree_contents(temporary.path()), before);
    }

    #[test]
    fn read_only_open_rejects_a_stale_schema_without_migrating_it() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let connection = store.open().expect("writable test connection");
        connection
            .execute_batch("PRAGMA user_version = 40;")
            .expect("mark test catalogue stale");
        drop(connection);
        let before = tree_contents(temporary.path());

        assert!(matches!(
            HubStore::open_immutable_read_only(temporary.path()),
            Err(StoreError::UnsupportedSchema(40))
        ));
        assert_eq!(tree_contents(temporary.path()), before);
    }

    #[test]
    fn online_catalogue_backup_restores_through_normal_store_checks() {
        let source_directory = tempfile::tempdir().expect("source directory");
        let store = HubStore::initialize(source_directory.path()).expect("source store");
        let installation_id = store.installation_id().expect("source installation");
        let restore_directory = tempfile::tempdir().expect("restore directory");
        let backup_path = restore_directory.path().join("hub.sqlite");

        store
            .backup_catalogue_to(&backup_path)
            .expect("online backup");
        assert!(backup_path.is_file());
        let restored = HubStore::initialize(restore_directory.path()).expect("restored store");
        restored.quick_check().expect("restored integrity");
        assert_eq!(restored.installation_id().unwrap(), installation_id);
        assert!(matches!(
            store.backup_catalogue_to(&backup_path),
            Err(StoreError::BackupDestinationExists(_))
        ));
    }

    #[test]
    fn complete_backup_copies_catalogue_referenced_pack_set() {
        let source_directory = tempfile::tempdir().expect("source directory");
        let store = HubStore::initialize(source_directory.path()).expect("source store");
        let manifest = test_manifest();
        let pack = &manifest.chunks[0];
        let source_pack = store
            .packs_dir()
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        fs::create_dir_all(source_pack.parent().expect("pack parent")).expect("pack parent");
        fs::write(&source_pack, vec![7_u8; 100]).expect("source pack");
        store.publish_manifest(&manifest).expect("catalogue pack");

        let backup_parent = tempfile::tempdir().expect("backup parent");
        let backup_root = backup_parent.path().join("backup");
        store.backup_to(&backup_root).expect("complete backup");
        let restored = HubStore::initialize(&backup_root).expect("restored store");
        restored.quick_check().expect("restored integrity");
        let restored_pack = restored
            .pack_for_digest(pack.sha256)
            .expect("restored catalogue")
            .expect("restored pack");
        assert_eq!(fs::read(restored_pack.path).unwrap(), vec![7_u8; 100]);
    }

    #[test]
    fn corrupt_referenced_pack_refuses_and_cleans_backup_root() {
        let source_directory = tempfile::tempdir().expect("source directory");
        let store = HubStore::initialize(source_directory.path()).expect("source store");
        let manifest = test_manifest();
        let source_pack = store
            .packs_dir()
            .join("sha256")
            .join(format!("{}.sqlite.zst", manifest.chunks[0].sha256));
        fs::create_dir_all(source_pack.parent().expect("pack parent")).expect("pack parent");
        fs::write(&source_pack, vec![7_u8; 100]).expect("source pack");
        store.publish_manifest(&manifest).expect("catalogue pack");
        fs::write(&source_pack, vec![8_u8; 100]).expect("corrupt pack");

        let backup_parent = tempfile::tempdir().expect("backup parent");
        let backup_root = backup_parent.path().join("corrupt-backup");
        assert!(matches!(
            store.backup_to(&backup_root),
            Err(StoreError::BackupPackDigestMismatch { .. })
        ));
        assert!(!backup_root.exists());
    }

    #[test]
    fn publishes_and_loads_a_canonical_manifest_catalog() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let manifest = test_manifest();

        store.publish_manifest(&manifest).expect("publish manifest");
        let loaded = store
            .manifest_for_vehicle(manifest.vehicle_id)
            .expect("load manifest")
            .expect("manifest exists");
        assert_eq!(loaded, manifest);

        let pack = store
            .pack_for_digest(manifest.chunks[0].sha256)
            .expect("load pack")
            .expect("pack exists");
        assert_eq!(pack.compressed_bytes, manifest.chunks[0].compressed_bytes);
        assert!(pack.path.starts_with(store.packs_dir()));
    }

    #[test]
    fn schema_22_manifest_is_catalogued_as_full_snapshot() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let manifest = schema_22_test_manifest();
        let digest = manifest.chunks[0].sha256;
        assert!(matches!(
            store.publish_manifest(&manifest),
            Err(StoreError::Schema22PairPublicationRequired(vehicle_id))
                if vehicle_id == manifest.vehicle_id
        ));
        let noop = crate::updates_delivery::SignedNoOpState {
            schema: "teslatlas-hub-schema-22-noop-v1".into(),
            projection_schema: "2.2".into(),
            installation_id: manifest.installation_id,
            account_id: manifest.account_id,
            vehicle_id: manifest.vehicle_id,
            generation: manifest.generation,
            snapshot_id: manifest.snapshot_id,
            head_sequence: manifest.head_sequence,
            pack_sha256: digest.to_string(),
            terminal_cursor: manifest.terminal_cursor.clone(),
            source_witness: None,
        };
        let gate = store
            .try_acquire_publication_gate()
            .expect("schema 2.2 publication gate");
        store
            .publish_schema_22_noop(&gate, &noop)
            .expect("schema 2.2 no-op is published first");
        let mut mismatched = manifest.clone();
        mismatched.generation += 1;
        assert!(matches!(
            store.publish_schema_22_manifest(&gate, &mismatched),
            Err(StoreError::InvalidSchema22Pair(_))
        ));
        assert!(
            store
                .manifest_for_vehicle(manifest.vehicle_id)
                .expect("rejected pair lookup")
                .is_none()
        );
        store
            .publish_schema_22_manifest(&gate, &manifest)
            .expect("schema 2.2 full snapshot is catalogued");
        let loaded = store
            .manifest_for_vehicle(manifest.vehicle_id)
            .expect("catalogue lookup")
            .expect("schema 2.2 manifest");
        assert_eq!(loaded.schema, HUB_PROJECTION_SCHEMA_V3);
        assert_eq!(loaded.snapshot_id, manifest.snapshot_id);
        assert_eq!(loaded.chunks[0].sha256, digest);
    }

    #[test]
    fn source_and_vehicle_ids_are_stable_across_re_registration() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let descriptor = SourceDescriptor::new("tesla_owner_api", "account-opaque-id");
        let source = store
            .register_source(&descriptor, 1_000)
            .expect("source registers");
        let same_source = store
            .register_source(&descriptor, 2_000)
            .expect("source re-registers");
        assert_eq!(source, same_source);
        assert_eq!(source.created_at_ms, 1_000);

        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor {
                    source_id: source.source_id,
                    source_vehicle_key: "vehicle-fleet-id".into(),
                    vin: Some("5YJTESTVIN1234567".into()),
                    display_name: Some("Road car".into()),
                    tesla_eid: None,
                    tesla_vid: None,
                },
                3_000,
            )
            .expect("vehicle registers");
        let same_vehicle = store
            .register_vehicle(
                &VehicleDescriptor {
                    source_id: source.source_id,
                    source_vehicle_key: "vehicle-fleet-id".into(),
                    vin: None,
                    display_name: Some("Renamed road car".into()),
                    tesla_eid: None,
                    tesla_vid: None,
                },
                4_000,
            )
            .expect("vehicle re-registers");
        assert_eq!(same_vehicle.vehicle_id, vehicle.vehicle_id);
        assert_eq!(same_vehicle.created_at_ms, 3_000);
        assert_eq!(same_vehicle.last_seen_at_ms, 4_000);
        assert_eq!(same_vehicle.vin.as_deref(), Some("5YJTESTVIN1234567"));
        assert_eq!(
            same_vehicle.display_name.as_deref(),
            Some("Renamed road car")
        );
    }

    #[test]
    fn accepts_a_deterministic_vehicle_id_and_allocates_snapshot_markers() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let source = store
            .register_source(&SourceDescriptor::new("teslamate", "test-source"), 1_000)
            .expect("source registers");
        let expected_vehicle_id = Uuid::from_u128(7);
        let descriptor = VehicleDescriptor {
            source_id: source.source_id,
            source_vehicle_key: "vin:5YJTESTVIN1234567".into(),
            vin: Some("5YJTESTVIN1234567".into()),
            display_name: Some("Road car".into()),
            tesla_eid: None,
            tesla_vid: None,
        };
        let vehicle = store
            .register_vehicle_with_id(&descriptor, 2_000, expected_vehicle_id)
            .expect("vehicle registers");
        assert_eq!(vehicle.vehicle_id, expected_vehicle_id);
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        assert_eq!(
            store
                .reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)
                .expect("first marker"),
            1
        );
        assert_eq!(
            store
                .reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)
                .expect("second marker"),
            2
        );

        let conflicting = store
            .register_vehicle_with_id(&descriptor, 3_000, Uuid::from_u128(8))
            .expect_err("different stable identity must fail");
        assert!(matches!(
            conflicting,
            StoreError::VehicleIdentityMismatch { .. }
        ));
    }

    #[test]
    fn pairing_preparation_is_inert_and_revocation_is_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let invitation = store
            .prepare_pairing("iPhone", 1_000, 61_000)
            .expect("pairing prepares");
        let count = || {
            store
                .open()
                .expect("open database")
                .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("challenge count")
        };

        assert_eq!(count(), 0);
        store
            .persist_pairing("iPhone", &invitation)
            .expect("pairing persists");
        assert_eq!(count(), 1);
        store
            .revoke_pairing(invitation.pairing_id)
            .expect("first revocation");
        store
            .revoke_pairing(invitation.pairing_id)
            .expect("idempotent revocation");
        assert_eq!(count(), 0);
    }

    #[test]
    fn pairing_secrets_are_redacted_and_zeroizable_on_drop() {
        use zeroize::Zeroize;

        fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}

        let mut pairing = PairingSecret("pairing-secret".to_owned());
        let mut access = DeviceAccessToken("device-access-secret".to_owned());
        assert!(!format!("{access:?}").contains(access.as_bearer()));
        assert_zeroize_on_drop::<PairingSecret>();
        assert_zeroize_on_drop::<DeviceAccessToken>();
        pairing.zeroize();
        access.zeroize();
        assert!(pairing.as_wire().bytes().all(|byte| byte == 0));
        assert!(access.as_bearer().bytes().all(|byte| byte == 0));
    }

    #[test]
    fn pairing_is_single_use_and_persists_only_token_hashes() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let invitation = store
            .create_pairing("iPhone", 1_000, 61_000)
            .expect("pairing creates");
        assert!(format!("{invitation:?}").contains("[redacted]"));

        let access = store
            .claim_pairing(
                invitation.pairing_id,
                invitation.secret(),
                "Bolyki iPhone",
                2_000,
            )
            .expect("claim succeeds");
        assert_eq!(
            format!("{:?}", access.access_token),
            "DeviceAccessToken([redacted])"
        );
        let authenticated = store
            .authenticate_device(access.access_token.as_bearer())
            .expect("device lookup")
            .expect("device exists");
        assert_eq!(authenticated.device_id, access.device_id);
        assert_eq!(authenticated.display_name, "Bolyki iPhone");
        assert!(
            store
                .claim_pairing(
                    invitation.pairing_id,
                    invitation.secret(),
                    "Second phone",
                    3_000,
                )
                .is_err()
        );

        let connection = store.open().expect("open database");
        let challenge_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
                row.get(0)
            })
            .expect("challenge count");
        assert_eq!(challenge_count, 0);
        let stored_token_hash: Vec<u8> = connection
            .query_row("SELECT token_sha256 FROM paired_devices", [], |row| {
                row.get(0)
            })
            .expect("token digest");
        assert_ne!(
            stored_token_hash,
            access.access_token.as_bearer().as_bytes()
        );
    }

    #[test]
    fn pairing_claims_fail_closed_when_expired_or_malformed() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let invitation = store
            .create_pairing("iPad", 1_000, 2_000)
            .expect("pairing creates");
        assert!(matches!(
            store.claim_pairing(invitation.pairing_id, "not-a-token", "iPad", 1_500),
            Err(StoreError::PairingRejected)
        ));
        assert!(matches!(
            store.claim_pairing(invitation.pairing_id, invitation.secret(), "iPad", 2_000),
            Err(StoreError::PairingRejected)
        ));
        assert!(
            store
                .authenticate_device("not-a-token")
                .expect("malformed token lookup")
                .is_none()
        );
    }

    #[test]
    fn appends_canonical_json_once_and_retries_idempotently() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let (source, vehicle) = test_registered_vehicle(&store);
        let input = ObservationInput {
            source_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            observed_at_ms: 10_000,
            payload: serde_json::json!({"speed": 0, "battery_level": 80}),
        };

        let first = store
            .append_observation(&input, 10_010)
            .expect("first observation");
        let retry = store
            .append_observation(&input, 99_999)
            .expect("idempotent retry");
        assert!(first.inserted);
        assert!(!retry.inserted);
        assert_eq!(retry.observation, first.observation);
        assert_eq!(first.observation.received_at_ms, 10_010);
        let canonical = serde_json::to_vec(&input.payload).expect("JSON serializes");
        assert_eq!(
            first.observation.payload_sha256,
            Sha256Digest::of_bytes(&canonical)
        );

        let connection = store.open().expect("open database");
        assert!(
            connection
                .execute(
                    "UPDATE raw_observations SET payload_json = '{}' WHERE observation_id = ?1",
                    params![first.observation.observation_id],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM raw_observations WHERE observation_id = ?1",
                    params![first.observation.observation_id],
                )
                .is_err()
        );

        let observations = store
            .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(10))
            .expect("read observations");
        assert_eq!(observations, vec![first.observation]);
    }

    #[test]
    fn observations_are_time_ordered_and_query_is_bounded() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let (source, vehicle) = test_registered_vehicle(&store);
        for (observed_at_ms, value) in [(3_000, 3), (1_000, 1), (2_000, 2)] {
            store
                .append_observation(
                    &ObservationInput {
                        source_id: source.source_id,
                        vehicle_id: vehicle.vehicle_id,
                        observed_at_ms,
                        payload: serde_json::json!({"value": value}),
                    },
                    observed_at_ms + 1,
                )
                .expect("append observation");
        }
        let first_two = store
            .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(2))
            .expect("bounded page");
        assert_eq!(
            first_two
                .iter()
                .map(|row| row.observed_at_ms)
                .collect::<Vec<_>>(),
            vec![1_000, 2_000]
        );
        let filtered = store
            .observations_for_vehicle(
                vehicle.vehicle_id,
                ObservationQuery {
                    from_observed_at_ms: Some(2_000),
                    until_observed_at_ms: Some(3_000),
                    limit: 10,
                },
            )
            .expect("time query");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].observed_at_ms, 2_000);

        let error = store
            .observations_for_vehicle(
                vehicle.vehicle_id,
                ObservationQuery::from_start(MAX_OBSERVATION_QUERY_LIMIT + 1),
            )
            .expect_err("over-large query rejected");
        assert!(matches!(
            error,
            StoreError::InvalidObservationQueryLimit { .. }
        ));
    }

    #[test]
    fn rejects_wrong_source_non_object_and_oversized_observations() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let (source, vehicle) = test_registered_vehicle(&store);
        let other_source = store
            .register_source(
                &SourceDescriptor::new("teslamate_import", "migration-a"),
                1_001,
            )
            .expect("second source");
        let mismatch = store
            .append_observation(
                &ObservationInput {
                    source_id: other_source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 2_000,
                    payload: serde_json::json!({"status": "online"}),
                },
                2_001,
            )
            .expect_err("vehicle cannot be written by another source");
        assert!(matches!(mismatch, StoreError::VehicleSourceMismatch { .. }));

        let non_object = store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 2_000,
                    payload: serde_json::json!(["a response batch is not one observation"]),
                },
                2_001,
            )
            .expect_err("array rejected");
        assert!(matches!(non_object, StoreError::ObservationMustBeObject));

        let oversized = store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 2_000,
                    payload: serde_json::json!({"blob": "x".repeat(MAX_RAW_OBSERVATION_BYTES)}),
                },
                2_001,
            )
            .expect_err("oversized response rejected before database mutation");
        assert!(matches!(oversized, StoreError::ObservationTooLarge { .. }));
        assert!(
            store
                .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(10))
                .expect("read observation history")
                .is_empty()
        );
    }

    fn import_delta_test_car(car_id: i64) -> ProjectionCar {
        ProjectionCar {
            id: car_id,
            name: "Import delta fixture".into(),
            model: "Model 3".into(),
            vin: None,
            source_eid: None,
            source_vid: None,
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            firmware_version: None,
            efficiency_wh_per_km: None,
            settings: ProjectionCarSettings::default(),
        }
    }

    fn import_delta_test_cursor_key() -> CursorKey {
        CursorKey::from_bytes([61; 32])
    }

    fn import_delta_test_cursor(binding: &ProjectionBinding, sequence: u64) -> OpaqueCursor {
        OpaqueCursor::issue(
            &import_delta_test_cursor_key(),
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding.installation_id,
                account_id: binding.account_id,
                vehicle_id: binding.vehicle_id,
                generation: binding.generation,
                sequence,
            },
        )
        .expect("fixture cursor")
    }

    fn v2_base_manifest(store: &HubStore) -> (VehicleRecord, ProjectionBinding, SyncManifest) {
        let source = store
            .register_source(
                &SourceDescriptor::new("teslamate_import", "delta-fixture"),
                1_000,
            )
            .expect("fixture source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "10").with_tesla_identity(Some(70), None),
                1_001,
            )
            .expect("fixture vehicle");
        let binding = store
            .v2_projection_binding(vehicle.vehicle_id)
            .expect("fixture binding");
        let snapshot = ProjectionSnapshot {
            cars: vec![import_delta_test_car(binding.selected_car_id)],
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
        };
        let base_sequence = 1;
        let base_snapshot_id = Uuid::new_v4();
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: base_snapshot_id,
            ordinal: 0,
            binding: binding.clone(),
            sequence: SequenceRange {
                from_exclusive: base_sequence,
                to_inclusive: base_sequence,
            },
            snapshot: &snapshot,
        };
        let pack = ProjectionPackWriter::new(store.packs_dir())
            .write_full_snapshot_with_states_and_updates(&request, &[], &[])
            .expect("fixture base pack");
        let manifest = request
            .signed_manifest_with_states_and_updates(
                &pack,
                &[],
                &[],
                &import_delta_test_cursor_key(),
            )
            .expect("fixture base manifest");
        (vehicle, binding, manifest)
    }

    fn imported_v2_base(store: &HubStore) -> (VehicleRecord, ProjectionBinding, LineageManifestV2) {
        let (vehicle, binding, manifest) = v2_base_manifest(store);
        store
            .finalize_import_snapshot_with_binding(
                &manifest,
                Sha256Digest::of_bytes(b"import-delta-fixture-base"),
                &[],
                &binding,
            )
            .expect("fixture base catalogue");
        let lineage = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("fixture lineage lookup")
            .expect("fixture lineage catalogue");
        (vehicle, binding, lineage)
    }

    #[test]
    fn imported_selection_returns_one_durable_eid_and_settings() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, _) = imported_v2_base(&store);
        let settings = ProjectionCarSettings {
            use_streaming_api: false,
            suspend_min: 9,
            ..ProjectionCarSettings::default()
        };
        store
            .upsert_car_settings(vehicle.vehicle_id, binding.selected_car_id, &settings)
            .expect("settings");

        assert_eq!(
            store.selected_imported_tesla_eid().expect("selection"),
            Some((70, settings))
        );
    }

    #[test]
    fn imported_selection_uses_materialised_settings_before_defaults() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, _) = imported_v2_base(&store);
        let settings = ProjectionCarSettings {
            use_streaming_api: false,
            suspend_after_idle_min: 19,
            ..ProjectionCarSettings::default()
        };
        let car = ProjectionCar {
            id: binding.selected_car_id,
            settings: settings.clone(),
            ..import_delta_test_car(binding.selected_car_id)
        };
        store
            .persist_materialised_car_if_absent(vehicle.vehicle_id, &car)
            .expect("imported car");

        assert_eq!(
            store.selected_imported_tesla_eid().expect("selection"),
            Some((70, settings))
        );
    }

    fn test_projection_state(root: &Path, car: &ProjectionCar) -> TeslaMateProjectionState {
        let state = TeslaMateProjectionState::create(
            root,
            crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("projection state");
        let mut capture =
            crate::teslamate_projection_state::TeslaMateProjectionStateCapture::for_initial_base(
                state,
            );
        capture.record_car(car).expect("capture car");
        capture.seal().expect("seal projection state");
        capture.into_state()
    }

    fn projection_state_with_digest_rows(
        root: &Path,
        selected_car_id: i64,
        rows: &[(TeslaMateProjectionStateEntity, i64)],
    ) -> TeslaMateProjectionState {
        let maximum_rows = u64::try_from(rows.len())
            .expect("test row count fits u64")
            .checked_add(1)
            .expect("test row count has room for car");
        let mut state = TeslaMateProjectionState::create(
            root,
            crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                max_rows: maximum_rows,
                max_state_bytes: 4 * 1024 * 1024,
                max_changed_payload_bytes: 4 * 1024 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("projection state");
        state
            .record(
                TeslaMateProjectionStateEntity::Car,
                selected_car_id,
                selected_car_id,
                &serde_json::json!({"id": selected_car_id, "entity": "car"}),
            )
            .expect("capture car");
        for (entity, id) in rows {
            state
                .record(
                    *entity,
                    *id,
                    selected_car_id,
                    &serde_json::json!({"id": id, "entity": entity.as_str()}),
                )
                .expect("capture digest row");
        }
        state.seal().expect("seal projection state");
        state
    }

    /// Direct-import finalizers deliberately reject the generic test spool.
    /// Keep that generic helper above for the non-generation seams, and make
    /// generation tests opt in to the same gated constructor as production.
    fn create_direct_import_projection_state(
        store: &HubStore,
        run_id: Uuid,
        maximum_rows: u64,
    ) -> TeslaMateProjectionState {
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("direct projection-state publication gate");
        let state = store
            .create_import_projection_state(
                &publication_gate,
                run_id,
                crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                    max_rows: maximum_rows,
                    max_state_bytes: 4 * 1024 * 1024,
                    max_changed_payload_bytes: 4 * 1024 * 1024,
                    minimum_free_bytes: 0,
                },
                crate::teslamate_projection_state::DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES,
            )
            .expect("run-bound direct projection state");
        drop(publication_gate);
        state
    }

    fn direct_projection_state_with_digest_rows(
        store: &HubStore,
        run_id: Uuid,
        selected_car_id: i64,
        rows: &[(TeslaMateProjectionStateEntity, i64)],
    ) -> TeslaMateProjectionState {
        let maximum_rows = u64::try_from(rows.len())
            .expect("test row count fits u64")
            .checked_add(1)
            .expect("test row count has room for car");
        let mut state = create_direct_import_projection_state(store, run_id, maximum_rows);
        state
            .record(
                TeslaMateProjectionStateEntity::Car,
                selected_car_id,
                selected_car_id,
                &serde_json::json!({"id": selected_car_id, "entity": "car"}),
            )
            .expect("capture direct car");
        for (entity, id) in rows {
            state
                .record(
                    *entity,
                    *id,
                    selected_car_id,
                    &serde_json::json!({"id": id, "entity": entity.as_str()}),
                )
                .expect("capture direct digest row");
        }
        state.seal().expect("seal direct projection state");
        state
    }

    fn direct_test_projection_state(
        store: &HubStore,
        run_id: Uuid,
        car: &ProjectionCar,
    ) -> TeslaMateProjectionState {
        let state = create_direct_import_projection_state(store, run_id, 10);
        let mut capture =
            crate::teslamate_projection_state::TeslaMateProjectionStateCapture::for_initial_base(
                state,
            );
        capture.record_car(car).expect("capture direct car");
        capture.seal().expect("seal direct projection state");
        capture.into_state()
    }

    fn begin_projection_state_recovery_generation(store: &HubStore) -> (Uuid, ProjectionBinding) {
        let (vehicle, binding, _) = v2_base_manifest(store);
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
            )
            .expect("staging projection-state generation");
        (run_id, binding)
    }

    #[cfg(unix)]
    fn set_test_private_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .expect("set private test path mode");
    }

    #[cfg(unix)]
    fn write_owned_test_v1_run(store: &HubStore, run_id: Uuid) -> (PathBuf, PathBuf) {
        if !store.private_import_spool_dir.exists() {
            fs::create_dir(&store.private_import_spool_dir).expect("create private import spool");
        }
        set_test_private_mode(&store.private_import_spool_dir, 0o700);
        let staging = store.private_import_spool_dir.join(".projection-state");
        let namespace = staging.join("v1");
        let run_directory = namespace.join(run_id.to_string());
        for directory in [&staging, &namespace, &run_directory] {
            if !directory.exists() {
                fs::create_dir(directory).expect("create owned v1 test directory");
            }
            set_test_private_mode(directory, 0o700);
        }
        let owner_marker = serde_json::json!({
            "schema": 1,
            "kind": "teslatlas-hub/teslamate-projection-state/v1",
            "runId": run_id.to_string(),
        });
        let owner_path = run_directory.join("owner.json");
        fs::write(
            &owner_path,
            serde_json::to_vec(&owner_marker).expect("encode owned v1 marker"),
        )
        .expect("write owned v1 marker");
        set_test_private_mode(&owner_path, 0o600);
        let spool_path = run_directory.join(format!("{}.sqlite", Uuid::new_v4()));
        fs::write(&spool_path, b"deliberately-not-a-sqlite-database")
            .expect("write owned v1 spool bytes");
        set_test_private_mode(&spool_path, 0o600);
        (run_directory, spool_path)
    }

    fn staging_generation_exists(store: &HubStore, run_id: Uuid) -> bool {
        store
            .open()
            .expect("open staging catalogue")
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM import_generations WHERE run_id = ?1 AND status = 'staging')",
                params![run_id.to_string()],
                |row| row.get(0),
            )
            .expect("read staging generation")
    }

    #[test]
    fn recovery_reclaims_a_valid_owned_v1_spool_and_its_staging_generation() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (run_id, binding) = begin_projection_state_recovery_generation(&store);
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        let mut state = store
            .create_import_projection_state(
                &publication_gate,
                run_id,
                crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                    max_rows: 10,
                    max_state_bytes: 128 * 1024,
                    max_changed_payload_bytes: 128 * 1024,
                    minimum_free_bytes: 0,
                },
                crate::teslamate_projection_state::DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES,
            )
            .expect("run-bound state");
        state
            .record_car(&import_delta_test_car(binding.selected_car_id))
            .expect("capture car");
        state.seal().expect("seal state");
        let spool_path = state.path_for_test().to_path_buf();
        let run_directory = spool_path
            .parent()
            .expect("spool run directory")
            .to_path_buf();
        state
            .abandon_for_recovery_test()
            .expect("simulate interrupted import");
        // Recovery owns file-system cleanup, not SQLite content validation.
        // A crash can leave a partially-written database, but the owned v1
        // marker still makes this exact staging run reclaimable.
        fs::write(
            &spool_path,
            b"not a SQLite database after interrupted write",
        )
        .expect("corrupt stale spool bytes");
        #[cfg(unix)]
        set_test_private_mode(&spool_path, 0o600);

        let connection = store.open().expect("catalogue connection");
        store
            .recover_stale_import_projection_state_spools(&publication_gate, &connection)
            .expect("recover owned stale run");
        assert!(
            !spool_path.exists() && !run_directory.exists(),
            "recovery removes exactly the proven owned v1 run"
        );
        assert!(
            !staging_generation_exists(&store, run_id),
            "recovery removes the matching staging row before broad abandoned-generation cleanup"
        );
    }

    #[test]
    fn startup_recovery_refuses_to_scan_while_a_live_publication_gate_is_held() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (run_id, binding) = begin_projection_state_recovery_generation(&store);
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("live publication gate");
        let state = store
            .create_import_projection_state(
                &publication_gate,
                run_id,
                crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                    max_rows: 10,
                    max_state_bytes: 128 * 1024,
                    max_changed_payload_bytes: 128 * 1024,
                    minimum_free_bytes: 0,
                },
                crate::teslamate_projection_state::DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES,
            )
            .expect("live run-bound state");
        let spool_path = state.path_for_test().to_path_buf();

        assert!(matches!(
            HubStore::initialize(temporary.path()),
            Err(StoreError::PublicationGateBusy)
        ));
        assert!(
            spool_path.exists(),
            "busy startup must not scan live spools"
        );
        assert!(
            staging_generation_exists(&store, run_id),
            "busy startup must not clear the live staging row"
        );
        drop(state);
        drop(publication_gate);
        let _ = binding;
    }

    #[test]
    fn startup_recovery_preserves_a_flat_legacy_projection_state_spool() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let state = TeslaMateProjectionState::create(
            store.packs_dir(),
            crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("flat legacy spool");
        let legacy_spool_path = state.path_for_test().to_path_buf();
        state
            .abandon_for_recovery_test()
            .expect("leave legacy spool in place");
        drop(store);

        let _reopened = HubStore::initialize(temporary.path()).expect("restart succeeds");
        assert!(
            legacy_spool_path.exists(),
            "v1 recovery never guesses ownership of a flat legacy spool"
        );
    }

    #[cfg(unix)]
    #[test]
    fn startup_recovery_fails_closed_for_unsafe_v1_runs_without_deleting_any_sibling() {
        for unsafe_shape in ["owner", "unexpected", "mode", "symlink"] {
            let temporary = tempfile::tempdir().expect("temporary store");
            let store = HubStore::initialize(temporary.path()).expect("store");
            let (bad_run_id, _) = begin_projection_state_recovery_generation(&store);
            let valid_run_id = Uuid::new_v4();
            let (valid_directory, valid_spool) = write_owned_test_v1_run(&store, valid_run_id);
            let (bad_directory, bad_spool) = write_owned_test_v1_run(&store, bad_run_id);
            let sentinel = temporary.path().join(format!("{unsafe_shape}-sentinel"));

            match unsafe_shape {
                "owner" => {
                    let owner = bad_directory.join("owner.json");
                    fs::write(&owner, b"{\"schema\":999}").expect("malform owner marker");
                    set_test_private_mode(&owner, 0o600);
                }
                "unexpected" => {
                    let child = bad_directory.join("unrelated.txt");
                    fs::write(&child, b"must not be reclaimed").expect("write unexpected child");
                    set_test_private_mode(&child, 0o600);
                }
                "mode" => set_test_private_mode(&bad_directory, 0o755),
                "symlink" => {
                    fs::write(&sentinel, b"outside the spool namespace")
                        .expect("write external sentinel");
                    fs::remove_file(&bad_spool).expect("remove ordinary spool for symlink test");
                    std::os::unix::fs::symlink(&sentinel, &bad_spool)
                        .expect("place symlink in owned-looking run");
                }
                _ => unreachable!("enumerated unsafe shape"),
            }

            assert!(matches!(
                HubStore::initialize(temporary.path()),
                Err(StoreError::TeslaMateProjectionState(_))
            ));
            assert!(
                valid_directory.exists() && valid_spool.exists(),
                "{unsafe_shape}: preflight failure must preserve a valid sibling"
            );
            assert!(
                bad_directory.exists(),
                "{unsafe_shape}: unsafe run itself must be left intact for inspection"
            );
            assert!(
                staging_generation_exists(&store, bad_run_id),
                "{unsafe_shape}: broad staging cleanup must not run after recovery rejects"
            );
            if unsafe_shape == "symlink" {
                assert_eq!(
                    fs::read(&sentinel).expect("external sentinel survives"),
                    b"outside the spool namespace"
                );
            }
        }
    }

    #[test]
    fn run_bound_transfer_rejects_wrong_run_marker_mutation_and_attempt_substitution() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (run_id, binding) = begin_projection_state_recovery_generation(&store);
        let state = direct_projection_state_with_digest_rows(
            &store,
            run_id,
            binding.selected_car_id,
            &[(TeslaMateProjectionStateEntity::Position, 10)],
        );
        assert!(matches!(
            state.sealed_transfer_for_import_generation(Uuid::new_v4(), binding.selected_car_id),
            Err(TeslaMateProjectionStateError::ImportGenerationRunMismatch { .. })
        ));

        let transfer = state
            .sealed_transfer_for_import_generation(run_id, binding.selected_car_id)
            .expect("run-bound transfer descriptor");
        let replacement = direct_projection_state_with_digest_rows(
            &store,
            run_id,
            binding.selected_car_id,
            &[(TeslaMateProjectionStateEntity::Position, 99)],
        );
        assert_ne!(
            state.path_for_test(),
            replacement.path_for_test(),
            "a retry always owns a different attempt path"
        );
        fs::rename(replacement.path_for_test(), transfer.path())
            .expect("substitute another attempt at descriptor path");
        let connection = store.open().expect("catalogue connection");
        assert!(matches!(
            attach_teslamate_projection_state_transfer(&connection, &transfer),
            Err(StoreError::TeslaMateProjectionState(
                TeslaMateProjectionStateError::TransferDigestMismatch
            ))
        ));
        drop(connection);

        let marker = state
            .path_for_test()
            .parent()
            .expect("run directory")
            .join("owner.json");
        fs::write(&marker, b"{\"schema\":1}").expect("alter owner marker");
        #[cfg(unix)]
        set_test_private_mode(&marker, 0o600);
        assert!(matches!(
            state.sealed_transfer_for_import_generation(run_id, binding.selected_car_id),
            Err(TeslaMateProjectionStateError::InvalidOwnerMarker(_))
        ));
    }

    #[test]
    fn retry_drop_removes_only_its_exact_attempt_and_keeps_a_live_sibling() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (run_id, binding) = begin_projection_state_recovery_generation(&store);
        let first = direct_projection_state_with_digest_rows(
            &store,
            run_id,
            binding.selected_car_id,
            &[(TeslaMateProjectionStateEntity::Position, 10)],
        );
        let first_path = first.path_for_test().to_path_buf();
        let run_directory = first_path.parent().expect("run directory").to_path_buf();
        let second = direct_projection_state_with_digest_rows(
            &store,
            run_id,
            binding.selected_car_id,
            &[(TeslaMateProjectionStateEntity::Position, 11)],
        );
        let second_path = second.path_for_test().to_path_buf();
        drop(first);
        assert!(
            !first_path.exists() && second_path.exists() && run_directory.exists(),
            "dropping a failed retry attempt cannot remove its replacement"
        );
        drop(second);
        assert!(
            !run_directory.exists(),
            "the final normal drop removes its now-empty owned run"
        );
    }

    #[test]
    fn startup_recovery_reclaims_a_completed_run_orphaned_before_state_drop() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
            )
            .expect("staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &TeslaMateOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("stage direct-import session");
        let state = direct_test_projection_state(
            &store,
            run_id,
            &import_delta_test_car(binding.selected_car_id),
        );
        let spool_path = state.path_for_test().to_path_buf();
        let run_directory = spool_path.parent().expect("run directory").to_path_buf();
        store
            .finalize_import_generation_with_projection_state(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
                &manifest,
                Sha256Digest::of_bytes(b"completed-run-orphan"),
                &[],
                &binding,
                &state,
            )
            .expect("complete direct base finalization");
        state
            .abandon_for_recovery_test()
            .expect("simulate termination after commit before state drop");
        drop(store);

        let reopened = HubStore::initialize(temporary.path()).expect("restart reclaims orphan");
        assert!(
            !spool_path.exists() && !run_directory.exists(),
            "a committed generation leaves no owned v1 spool after restart"
        );
        assert!(
            reopened
                .lineage_manifest_for_vehicle(vehicle.vehicle_id)
                .expect("published lineage survives orphan cleanup")
                .is_some(),
            "startup reclamation never removes the completed catalogue result"
        );
    }

    fn persist_projection_state_rows(
        store: &HubStore,
        root: &Path,
        rows: &[(TeslaMateProjectionStateEntity, i64)],
    ) -> (VehicleRecord, ProjectionBinding) {
        let (vehicle, binding, manifest) = v2_base_manifest(store);
        let inventory = TeslaMateImportProjectionInventory {
            source_id: binding.account_id,
            selected_car_id: binding.selected_car_id,
            rows: Vec::new(),
        };
        let state = projection_state_with_digest_rows(root, binding.selected_car_id, rows);
        store
            .finalize_teslamate_import_snapshot_with_projection_state(
                &manifest,
                Sha256Digest::of_bytes(b"digest-cache-fixture"),
                &[],
                &binding,
                &inventory,
                &state,
            )
            .expect("persist fixture projection state");
        (vehicle, binding)
    }

    fn unchanged_direct_successor_projection_state(
        store: &HubStore,
        run_id: Uuid,
        vehicle_id: Uuid,
        binding: &ProjectionBinding,
        car: &ProjectionCar,
    ) -> TeslaMateProjectionState {
        let prior = store
            .teslamate_import_projection_state_lookup(
                vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("verified direct prior state");
        let state = create_direct_import_projection_state(store, run_id, 10);
        let mut capture =
            crate::teslamate_projection_state::TeslaMateProjectionStateCapture::for_successor(
                state,
                Box::new(prior),
            );
        capture
            .record_car(car)
            .expect("capture unchanged direct car");
        capture.seal().expect("seal direct successor state");
        capture.into_state()
    }

    #[test]
    fn projection_state_digest_cache_reuses_one_verified_range() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding) = persist_projection_state_rows(
            &store,
            temporary.path(),
            &[
                (TeslaMateProjectionStateEntity::Position, 10),
                (TeslaMateProjectionStateEntity::Position, 20),
                (TeslaMateProjectionStateEntity::Position, 30),
            ],
        );
        let mut lookup = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("verified lookup");

        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 10)
                .expect("first cached digest")
                .is_some()
        );
        assert_eq!(lookup.digest_cache_loads, 1);
        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 20)
                .expect("second cached digest")
                .is_some()
        );
        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 20)
                .expect("repeated cached digest")
                .is_some()
        );
        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 30)
                .expect("third cached digest")
                .is_some()
        );
        assert_eq!(
            lookup.digest_cache_loads, 1,
            "one entity/id range must satisfy later in-range lookups"
        );
    }

    #[test]
    fn projection_state_digest_cache_preserves_gaps_and_exhausted_absence() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding) = persist_projection_state_rows(
            &store,
            temporary.path(),
            &[
                (TeslaMateProjectionStateEntity::Position, 10),
                (TeslaMateProjectionStateEntity::Position, 20),
            ],
        );
        let mut lookup = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("verified lookup");

        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 10)
                .expect("cached digest")
                .is_some()
        );
        assert_eq!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 15)
                .expect("gap lookup"),
            None,
            "a cached range may not treat a gap as unchanged"
        );
        assert_eq!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 21)
                .expect("exhausted tail lookup"),
            None,
            "an exhausted range must preserve a missing tail row"
        );
        assert_eq!(
            lookup.digest_cache_loads, 1,
            "gaps and an exhausted tail are exact cached absences"
        );
        assert_eq!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 9)
                .expect("backward missing lookup"),
            None,
        );
        assert_eq!(
            lookup.digest_cache_loads, 2,
            "an earlier ID is outside the cached lower bound and must reload"
        );
    }

    #[test]
    fn projection_state_digest_cache_reloads_for_entity_changes_and_backtracking() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding) = persist_projection_state_rows(
            &store,
            temporary.path(),
            &[
                (TeslaMateProjectionStateEntity::Drive, 1),
                (TeslaMateProjectionStateEntity::Position, 10),
                (TeslaMateProjectionStateEntity::Position, 20),
            ],
        );
        let mut lookup = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("verified lookup");

        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 10)
                .expect("position digest")
                .is_some()
        );
        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Drive, 1)
                .expect("drive digest")
                .is_some()
        );
        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 20)
                .expect("position digest after entity change")
                .is_some()
        );
        assert_eq!(
            lookup.digest_cache_loads, 2,
            "a different entity may not invalidate or reuse the position range"
        );
        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 9)
                .expect("backtracked position digest")
                .is_none()
        );
        assert_eq!(
            lookup.digest_cache_loads, 3,
            "an earlier ID must replace only its entity range, preserving exact absence"
        );
    }

    #[test]
    fn projection_state_digest_cache_is_bounded_and_leaves_tombstone_paging_exact() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let cache_limit = i64::try_from(TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS)
            .expect("cache limit fits i64");
        let mut rows = Vec::with_capacity(TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS + 2);
        rows.push((TeslaMateProjectionStateEntity::Drive, 1));
        rows.extend((1..=cache_limit + 1).map(|id| (TeslaMateProjectionStateEntity::Position, id)));
        let (vehicle, binding) = persist_projection_state_rows(&store, temporary.path(), &rows);
        let mut lookup = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("verified lookup");

        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, 1)
                .expect("first bounded digest")
                .is_some()
        );
        let cache = lookup
            .digest_caches
            .iter()
            .find(|cache| cache.entity == TeslaMateProjectionStateEntity::Position)
            .expect("position cache loaded");
        assert_eq!(
            cache.rows.len(),
            TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS
        );
        assert!(!cache.exhausted, "a full cache page cannot claim a tail");
        assert!(
            lookup
                .digest_store(TeslaMateProjectionStateEntity::Position, cache_limit + 1)
                .expect("next range digest")
                .is_some()
        );
        assert_eq!(lookup.digest_cache_loads, 2);
        assert!(
            lookup.digest_caches.iter().all(
                |cache| cache.rows.len() <= TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS
            )
        );
        assert!(lookup.digest_caches.len() <= TeslaMateProjectionStateEntity::ALL.len());

        let first = lookup
            .page_after_store(None, 2)
            .expect("first tombstone page");
        assert_eq!(first.rows.len(), 2);
        assert_eq!(first.rows[0].entity, TeslaMateProjectionStateEntity::Car);
        assert_eq!(first.rows[1].entity, TeslaMateProjectionStateEntity::Drive);
        let second = lookup
            .page_after_store(first.next_after, 2)
            .expect("second tombstone page");
        assert_eq!(second.rows.len(), 2);
        assert!(
            second
                .rows
                .iter()
                .all(|row| row.entity == TeslaMateProjectionStateEntity::Position)
        );
    }

    #[test]
    fn projection_state_digest_cache_rejects_mismatched_durable_rows() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding) = persist_projection_state_rows(
            &store,
            temporary.path(),
            &[
                (TeslaMateProjectionStateEntity::Position, 77),
                (TeslaMateProjectionStateEntity::Position, 78),
            ],
        );
        let connection = store.open().expect("fixture catalogue");
        connection
            .execute(
                "UPDATE teslamate_import_projection_state_rows
                    SET car_id = ?1
                  WHERE vehicle_id = ?2 AND entity_ordinal = ?3 AND entity_id = ?4",
                params![
                    binding.selected_car_id + 1,
                    vehicle.vehicle_id.to_string(),
                    i64::from(TeslaMateProjectionStateEntity::Position.ordinal()),
                    77_i64,
                ],
            )
            .expect("inject wrong car row");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .expect("allow malformed test row");
        connection
            .execute(
                "UPDATE teslamate_import_projection_state_rows
                    SET entity = 'drive'
                  WHERE vehicle_id = ?1 AND entity_ordinal = ?2 AND entity_id = ?3",
                params![
                    vehicle.vehicle_id.to_string(),
                    i64::from(TeslaMateProjectionStateEntity::Position.ordinal()),
                    78_i64,
                ],
            )
            .expect("inject mismatched entity row");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = OFF")
            .expect("restore constraints");
        drop(connection);

        let mut lookup = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("open lookup over corrupted rows");
        for id in [77_i64, 78_i64] {
            assert!(matches!(
                lookup.digest_store(TeslaMateProjectionStateEntity::Position, id),
                Err(StoreError::LineageCatalogConflict)
            ));
        }
    }

    #[test]
    fn digest_projection_state_is_atomic_with_base_and_successor_heads() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let car = import_delta_test_car(binding.selected_car_id);
        let inventory = TeslaMateImportProjectionInventory {
            source_id: binding.account_id,
            selected_car_id: binding.selected_car_id,
            rows: Vec::new(),
        };
        let state = test_projection_state(temporary.path(), &car);
        store
            .finalize_teslamate_import_snapshot_with_projection_state(
                &manifest,
                Sha256Digest::of_bytes(b"projection-state-base"),
                &[],
                &binding,
                &inventory,
                &state,
            )
            .expect("catalogue base and state atomically");

        let mut lookup = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("verified projection-state lookup");
        assert_eq!(lookup.header().base_snapshot_id, manifest.snapshot_id);
        assert_eq!(lookup.header().head_sequence, manifest.head_sequence);
        assert!(
            lookup
                .digest(TeslaMateProjectionStateEntity::Car, binding.selected_car_id)
                .expect("lookup car digest")
                .is_some()
        );
        let page = lookup.page_after(None, 10).expect("bounded state page");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].entity, TeslaMateProjectionStateEntity::Car);
        drop(lookup);

        let connection = store.open().expect("state catalogue");
        let (rows, digest_bytes): (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(length(projection_sha256)), 0)
                   FROM teslamate_import_projection_state_rows",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("digest-only rows");
        assert_eq!((rows, digest_bytes), (1, 32));
        drop(connection);

        let base = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("base lineage")
            .expect("base lineage exists");
        let delta = imported_typed_delta(&store, &binding, &base);
        let prior = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("prior state");
        let successor_state = TeslaMateProjectionState::create(
            temporary.path(),
            crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("successor state");
        let mut successor =
            crate::teslamate_projection_state::TeslaMateProjectionStateCapture::for_successor(
                successor_state,
                Box::new(prior),
            );
        assert_eq!(
            successor.record_car(&car).expect("capture unchanged car"),
            crate::teslamate_projection_state::TeslaMateProjectionStateChange::Unchanged
        );
        successor.seal().expect("seal successor state");
        let successor_state = successor.into_state();
        store
            .finalize_teslamate_import_delta_successor_with_projection_state(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
                Sha256Digest::of_bytes(b"projection-state-successor"),
                &[],
                &inventory,
                &successor_state,
            )
            .expect("catalogue successor and replacement state atomically");
        let lookup = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("updated state lookup");
        assert_eq!(lookup.header().head_sequence, delta.to_sequence);
    }

    #[test]
    fn direct_import_successor_batch_is_atomic_and_advances_every_durable_head() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let car = import_delta_test_car(binding.selected_car_id);
        let inventory = TeslaMateImportProjectionInventory {
            source_id: binding.account_id,
            selected_car_id: binding.selected_car_id,
            rows: Vec::new(),
        };
        let base_state = test_projection_state(temporary.path(), &car);
        store
            .finalize_teslamate_import_snapshot_with_projection_state(
                &manifest,
                Sha256Digest::of_bytes(b"direct-batch-base"),
                &[],
                &binding,
                &inventory,
                &base_state,
            )
            .expect("catalogue base");
        let base = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("base lineage")
            .expect("base lineage exists");
        let first = imported_typed_delta(&store, &binding, &base);
        let second = imported_typed_delta_after(
            &store,
            &binding,
            base.base.snapshot_id,
            first.to_sequence,
            first.chain_digest,
            first.pack.ordinal + 1,
        );
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
            )
            .expect("staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &TeslaMateOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("stage direct-import tail");
        let successor_state = unchanged_direct_successor_projection_state(
            &store,
            run_id,
            vehicle.vehicle_id,
            &binding,
            &car,
        );

        store
            .finalize_import_generation_delta_successors_with_projection_state(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
                &[first.clone(), second.clone()],
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, second.to_sequence),
                Sha256Digest::of_bytes(b"direct-batch-successor"),
                &[],
                &successor_state,
            )
            .expect("atomically publish the complete direct-import batch");

        let lineage = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage")
            .expect("lineage exists");
        assert_eq!(lineage.deltas, vec![first, second.clone()]);
        assert_eq!(lineage.head_sequence, second.to_sequence);
        let state = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("replacement digest state");
        assert_eq!(state.header().head_sequence, second.to_sequence);
        let connection = store.open().expect("catalogue");
        let generations: i64 = connection
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("generation count");
        assert_eq!(generations, 0, "successful batch consumes its generation");
    }

    #[test]
    fn direct_import_successor_batch_rejects_out_of_scope_state_without_advancing_heads() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let car = import_delta_test_car(binding.selected_car_id);
        let inventory = TeslaMateImportProjectionInventory {
            source_id: binding.account_id,
            selected_car_id: binding.selected_car_id,
            rows: Vec::new(),
        };
        let base_state = test_projection_state(temporary.path(), &car);
        store
            .finalize_teslamate_import_snapshot_with_projection_state(
                &manifest,
                Sha256Digest::of_bytes(b"direct-batch-rollback-base"),
                &[],
                &binding,
                &inventory,
                &base_state,
            )
            .expect("catalogue base");
        let base = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("base lineage")
            .expect("base lineage exists");
        let first = imported_typed_delta(&store, &binding, &base);
        let second = imported_typed_delta_after(
            &store,
            &binding,
            base.base.snapshot_id,
            first.to_sequence,
            first.chain_digest,
            first.pack.ordinal + 1,
        );
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
            )
            .expect("staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &TeslaMateOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("stage direct-import tail");
        // A run-bound descriptor validates its selected-car scope before the
        // destination transaction begins, so a foreign-car spool cannot even
        // reach delta insertion.
        let wrong_car_state = direct_test_projection_state(
            &store,
            run_id,
            &import_delta_test_car(binding.selected_car_id + 1),
        );

        assert!(matches!(
            store.finalize_import_generation_delta_successors_with_projection_state(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
                &[first, second.clone()],
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, second.to_sequence),
                Sha256Digest::of_bytes(b"direct-batch-rollback"),
                &[],
                &wrong_car_state,
            ),
            Err(StoreError::TeslaMateProjectionState(
                TeslaMateProjectionStateError::TransferRowContractMismatch
            ))
        ));
        assert_eq!(
            store
                .lineage_manifest_for_vehicle(vehicle.vehicle_id)
                .expect("lineage after rollback"),
            Some(base),
            "a bad second-stage state must not expose a prefix of the delta batch"
        );
        let state = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("base digest state survives");
        assert_eq!(state.header().head_sequence, manifest.head_sequence);
        let connection = store.open().expect("catalogue");
        let generations: i64 = connection
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("generation count");
        assert_eq!(generations, 1, "rollback retains the staging generation");
    }

    #[test]
    fn direct_import_successor_batch_refuses_a_gap_in_pack_ordinals() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let car = import_delta_test_car(binding.selected_car_id);
        let inventory = TeslaMateImportProjectionInventory {
            source_id: binding.account_id,
            selected_car_id: binding.selected_car_id,
            rows: Vec::new(),
        };
        let base_state = test_projection_state(temporary.path(), &car);
        store
            .finalize_teslamate_import_snapshot_with_projection_state(
                &manifest,
                Sha256Digest::of_bytes(b"direct-batch-ordinal-base"),
                &[],
                &binding,
                &inventory,
                &base_state,
            )
            .expect("catalogue base");
        let base = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("base lineage")
            .expect("base lineage exists");
        let gapped = imported_typed_delta_after(
            &store,
            &binding,
            base.base.snapshot_id,
            base.head_sequence,
            base.head_digest,
            // The base owns ordinal zero, so a successor must start at one.
            2,
        );
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
            )
            .expect("staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &TeslaMateOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("stage direct-import tail");
        let successor_state = unchanged_direct_successor_projection_state(
            &store,
            run_id,
            vehicle.vehicle_id,
            &binding,
            &car,
        );

        assert!(matches!(
            store.finalize_import_generation_delta_successors_with_projection_state(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
                std::slice::from_ref(&gapped),
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, gapped.to_sequence),
                Sha256Digest::of_bytes(b"direct-batch-ordinal-gap"),
                &[],
                &successor_state,
            ),
            Err(StoreError::LineageCatalogConflict)
        ));
        assert_eq!(
            store
                .lineage_manifest_for_vehicle(vehicle.vehicle_id)
                .expect("lineage after ordinal rejection"),
            Some(base),
            "an ordinal gap must not advance the lineage head"
        );
        let connection = store.open().expect("catalogue");
        let generations: i64 = connection
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("generation count");
        assert_eq!(generations, 1, "ordinal rejection retains the generation");
    }

    #[test]
    fn legacy_inventory_without_digest_state_fails_closed_distinctly() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let inventory = TeslaMateImportProjectionInventory {
            source_id: binding.account_id,
            selected_car_id: binding.selected_car_id,
            rows: Vec::new(),
        };
        store
            .finalize_teslamate_import_snapshot(
                &manifest,
                Sha256Digest::of_bytes(b"legacy-inventory-only"),
                &[],
                &binding,
                &inventory,
            )
            .expect("legacy inventory base");
        assert!(matches!(
            store.teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            ),
            Err(StoreError::TeslaMateImportProjectionStateMissing(id)) if id == vehicle.vehicle_id
        ));
    }

    fn legacy_direct_bridge_fixture(
        store: &HubStore,
        legacy_fingerprint: Sha256Digest,
    ) -> (VehicleRecord, ProjectionBinding, SyncManifest) {
        let (vehicle, binding, manifest) = v2_base_manifest(store);
        let inventory = TeslaMateImportProjectionInventory {
            source_id: binding.account_id,
            selected_car_id: binding.selected_car_id,
            rows: vec![ProjectionTombstone {
                entity: ProjectionDeltaEntity::Position,
                id: 10,
                car_id: binding.selected_car_id,
            }],
        };
        store
            .finalize_teslamate_import_snapshot(
                &manifest,
                legacy_fingerprint,
                &[],
                &binding,
                &inventory,
            )
            .expect("legacy inventory-only base");
        (vehicle, binding, manifest)
    }

    fn legacy_direct_bridge_generation(
        store: &HubStore,
        vehicle: &VehicleRecord,
        binding: &ProjectionBinding,
    ) -> Uuid {
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
            )
            .expect("bridge staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &TeslaMateOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("bridge staging session");
        run_id
    }

    #[test]
    fn legacy_direct_bridge_attaches_state_without_pack_delta_or_sequence_and_logical_rerun_skips()
    {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
        let logical_fingerprint = Sha256Digest::of_bytes(b"logical-direct-projection");
        let (vehicle, binding, manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
        assert!(
            store
                .legacy_teslamate_direct_bridge_is_eligible(
                    vehicle.vehicle_id,
                    binding.account_id,
                    binding.selected_car_id,
                )
                .expect("legacy base is bridge eligible")
        );
        let lineage_before = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("legacy lineage")
            .expect("legacy base lineage");
        let sequence_before: Option<i64> = store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT next_sequence FROM vehicle_snapshot_sequences WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .expect("sequence before bridge");
        let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
        let state = direct_projection_state_with_digest_rows(
            &store,
            run_id,
            binding.selected_car_id,
            &[(TeslaMateProjectionStateEntity::Position, 10)],
        );

        let bridged = store
            .bridge_legacy_teslamate_direct_import(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                legacy_fingerprint,
                logical_fingerprint,
                &state,
            )
            .expect("unchanged legacy base bridges atomically");
        assert_eq!(bridged.snapshot_id, manifest.snapshot_id);
        assert_eq!(bridged.head_sequence, manifest.head_sequence);
        assert_eq!(bridged.total_rows, manifest.total_rows);
        assert!(
            store
                .source_fingerprint_matches(vehicle.vehicle_id, logical_fingerprint)
                .expect("logical fingerprint is now current"),
            "the next logical direct capture must take the normal skip guard"
        );
        assert!(
            !store
                .source_fingerprint_matches(vehicle.vehicle_id, legacy_fingerprint)
                .expect("retired physical fingerprint is replaced")
        );
        assert!(
            store
                .teslamate_import_projection_state_exists(vehicle.vehicle_id)
                .expect("state head exists")
        );
        let state_lookup = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("bridged durable state lookup");
        assert_eq!(state_lookup.header().base_snapshot_id, manifest.snapshot_id);
        assert_eq!(state_lookup.header().head_sequence, manifest.head_sequence);
        drop(state_lookup);
        assert!(
            !store
                .legacy_teslamate_direct_bridge_is_eligible(
                    vehicle.vehicle_id,
                    binding.account_id,
                    binding.selected_car_id,
                )
                .expect("bridge is one-time"),
            "the persisted state/marker prevents a second bridge"
        );
        assert_eq!(
            store
                .lineage_manifest_for_vehicle(vehicle.vehicle_id)
                .expect("lineage after bridge"),
            Some(lineage_before),
            "bridge must retain the exact immutable base/head"
        );
        let connection = store.open().expect("catalogue after bridge");
        for (table, expected) in [
            ("sync_bases", 1),
            ("sync_deltas", 0),
            ("sync_packs", 1),
            ("teslamate_import_projection_state_bridges", 1),
            ("import_generations", 0),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(count, expected, "bridge must not add {table}");
        }
        let sequence_after: Option<i64> = connection
            .query_row(
                "SELECT next_sequence FROM vehicle_snapshot_sequences WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .expect("sequence after bridge");
        assert_eq!(
            sequence_after, sequence_before,
            "bridge must not reserve a sequence"
        );
    }

    #[test]
    fn legacy_direct_bridge_rejects_changed_physical_fingerprint_without_mutation() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
        let (vehicle, binding, manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
        let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
        let state = direct_projection_state_with_digest_rows(
            &store,
            run_id,
            binding.selected_car_id,
            &[(TeslaMateProjectionStateEntity::Position, 10)],
        );
        let changed_physical = Sha256Digest::of_bytes(b"changed-direct-physical");

        assert!(matches!(
            store.bridge_legacy_teslamate_direct_import(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                changed_physical,
                Sha256Digest::of_bytes(b"logical-direct-projection"),
                &state,
            ),
            Err(StoreError::TeslaMateLegacyDirectRebaseRequired(id)) if id == vehicle.vehicle_id
        ));
        let connection = store.open().expect("catalogue after rejection");
        for (table, expected) in [
            ("teslamate_import_projection_state_heads", 0),
            ("teslamate_import_projection_state_rows", 0),
            ("teslamate_import_projection_state_bridges", 0),
            ("sync_deltas", 0),
            ("sync_packs", 1),
            ("import_generations", 1),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(count, expected, "mismatch must not alter {table}");
        }
        assert!(
            store
                .source_fingerprint_matches(vehicle.vehicle_id, legacy_fingerprint)
                .expect("legacy fingerprint remains current")
        );
        assert_eq!(
            store
                .manifest_for_vehicle(vehicle.vehicle_id)
                .expect("base manifest remains"),
            Some(manifest)
        );
    }

    #[test]
    fn legacy_direct_bridge_rolls_back_state_when_inventory_semantics_mismatch() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
        let (vehicle, binding, _manifest) =
            legacy_direct_bridge_fixture(&store, legacy_fingerprint);
        let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
        let mismatched_state = direct_projection_state_with_digest_rows(
            &store,
            run_id,
            binding.selected_car_id,
            &[
                (TeslaMateProjectionStateEntity::Position, 10),
                (TeslaMateProjectionStateEntity::Position, 11),
            ],
        );

        assert!(matches!(
            store.bridge_legacy_teslamate_direct_import(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                legacy_fingerprint,
                Sha256Digest::of_bytes(b"logical-direct-projection"),
                &mismatched_state,
            ),
            Err(StoreError::TeslaMateLegacyDirectRebaseRequired(id)) if id == vehicle.vehicle_id
        ));
        let connection = store.open().expect("catalogue after rollback");
        for (table, expected) in [
            ("teslamate_import_projection_state_heads", 0),
            ("teslamate_import_projection_state_rows", 0),
            ("teslamate_import_projection_state_bridges", 0),
            ("snapshot_fingerprints", 1),
            ("import_generations", 1),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(count, expected, "failed bridge must roll back {table}");
        }
        assert!(
            store
                .source_fingerprint_matches(vehicle.vehicle_id, legacy_fingerprint)
                .expect("fingerprint rollback")
        );
    }

    #[test]
    fn legacy_direct_bridge_rejects_a_carless_sealed_state_without_installing_a_head() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
        let (vehicle, binding, _manifest) =
            legacy_direct_bridge_fixture(&store, legacy_fingerprint);
        let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
        let mut carless_state = create_direct_import_projection_state(&store, run_id, 10);
        carless_state
            .record(
                TeslaMateProjectionStateEntity::Position,
                10,
                binding.selected_car_id,
                &serde_json::json!({"id": 10}),
            )
            .expect("record matching non-car row");
        carless_state.seal().expect("seal carless state");

        assert!(matches!(
            store.bridge_legacy_teslamate_direct_import(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                legacy_fingerprint,
                Sha256Digest::of_bytes(b"logical-direct-projection"),
                &carless_state,
            ),
            Err(StoreError::TeslaMateLegacyDirectRebaseRequired(id)) if id == vehicle.vehicle_id
        ));
        let connection = store.open().expect("catalogue after rejection");
        for table in [
            "teslamate_import_projection_state_heads",
            "teslamate_import_projection_state_rows",
            "teslamate_import_projection_state_bridges",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(count, 0, "carless bridge must not write {table}");
        }
        let generations: i64 = connection
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("staging generation count");
        assert_eq!(generations, 1, "failed bridge remains retryable");
    }

    #[test]
    fn unsealed_state_refuses_base_finalization_without_partial_catalogue() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let inventory = TeslaMateImportProjectionInventory {
            source_id: binding.account_id,
            selected_car_id: binding.selected_car_id,
            rows: Vec::new(),
        };
        let mut unsealed = TeslaMateProjectionState::create(
            temporary.path(),
            crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("unsealed state");
        unsealed
            .record_car(&import_delta_test_car(binding.selected_car_id))
            .expect("capture car");
        assert!(matches!(
            store.finalize_teslamate_import_snapshot_with_projection_state(
                &manifest,
                Sha256Digest::of_bytes(b"unsealed-projection-state"),
                &[],
                &binding,
                &inventory,
                &unsealed,
            ),
            Err(StoreError::TeslaMateProjectionState(
                TeslaMateProjectionStateError::StateNotSealed
            ))
        ));
        assert!(
            store
                .lineage_manifest_for_vehicle(vehicle.vehicle_id)
                .expect("lineage lookup")
                .is_none()
        );
        let connection = store.open().expect("catalogue");
        for table in [
            "sync_bases",
            "sync_heads",
            "teslamate_import_projection_state_heads",
            "teslamate_import_projection_state_rows",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(count, 0, "failed finalizer must not write {table}");
        }
    }

    #[test]
    fn direct_base_set_transfer_preserves_state_and_legacy_inventory_semantics() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
            )
            .expect("staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &TeslaMateOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("staging session");
        let rows = [
            (TeslaMateProjectionStateEntity::Drive, 11),
            (TeslaMateProjectionStateEntity::Position, 12),
            (TeslaMateProjectionStateEntity::Charge, 13),
            (TeslaMateProjectionStateEntity::ChargeSample, 14),
            (TeslaMateProjectionStateEntity::State, 15),
        ];
        let state = direct_projection_state_with_digest_rows(
            &store,
            run_id,
            binding.selected_car_id,
            &rows,
        );
        let expected_state = state
            .page(None, MAX_PAGE_SIZE)
            .expect("sealed state page")
            .rows;

        store
            .finalize_import_generation_with_projection_state(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
                &manifest,
                Sha256Digest::of_bytes(b"set-transfer-semantic-parity"),
                &[],
                &binding,
                &state,
            )
            .expect("set-based direct base finalization");

        let mut lookup = store
            .teslamate_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("durable state lookup");
        let actual_state = lookup
            .page_after_store(None, MAX_PAGE_SIZE)
            .expect("durable state page")
            .rows;
        assert_eq!(
            actual_state, expected_state,
            "set transfer preserves every digest row"
        );
        drop(lookup);
        let inventory = store
            .teslamate_import_projection_inventory(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("legacy inventory");
        assert_eq!(
            inventory
                .rows
                .iter()
                .map(|row| (row.entity, row.id, row.car_id))
                .collect::<Vec<_>>(),
            vec![
                (ProjectionDeltaEntity::Charge, 13, binding.selected_car_id),
                (
                    ProjectionDeltaEntity::ChargeSample,
                    14,
                    binding.selected_car_id
                ),
                (ProjectionDeltaEntity::Drive, 11, binding.selected_car_id),
                (ProjectionDeltaEntity::Position, 12, binding.selected_car_id),
                (ProjectionDeltaEntity::State, 15, binding.selected_car_id),
            ],
            "legacy inventory remains the non-car projection-state view"
        );
        let connection = store.open().expect("catalogue after transfer");
        let remaining_generations: i64 = connection
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("generation count");
        assert_eq!(
            remaining_generations, 0,
            "commit removes the staged generation"
        );
    }

    #[test]
    fn direct_base_rejects_carless_sealed_state_before_catalogue_mutation() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
            )
            .expect("staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &TeslaMateOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("staging session");
        let mut state = create_direct_import_projection_state(&store, run_id, 10);
        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                12,
                binding.selected_car_id,
                &serde_json::json!({"id": 12}),
            )
            .expect("record carless state row");
        state.seal().expect("seal carless state");

        assert!(matches!(
            store.finalize_import_generation_with_projection_state(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
                &manifest,
                Sha256Digest::of_bytes(b"carless-state"),
                &[],
                &binding,
                &state,
            ),
            Err(StoreError::TeslaMateProjectionState(
                TeslaMateProjectionStateError::TransferCarContractMismatch
            ))
        ));
        let connection = store.open().expect("catalogue after rejection");
        for table in [
            "sync_bases",
            "sync_heads",
            "teslamate_import_projection_state_heads",
            "teslamate_import_projection_state_rows",
            "teslamate_import_projection_heads",
            "teslamate_import_projection_rows",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(count, 0, "carless state must not write {table}");
        }
        let generations: i64 = connection
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("staging generation count");
        assert_eq!(generations, 1, "rejected generation remains retryable");
    }

    #[test]
    fn sealed_transfer_rejects_a_same_shape_spool_substitution() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, _) = v2_base_manifest(&store);
        let state = projection_state_with_digest_rows(
            temporary.path(),
            binding.selected_car_id,
            &[(TeslaMateProjectionStateEntity::Position, 12)],
        );
        let transfer = state
            .sealed_transfer(binding.selected_car_id)
            .expect("sealed transfer descriptor");
        let replacement = projection_state_with_digest_rows(
            temporary.path(),
            binding.selected_car_id,
            &[(TeslaMateProjectionStateEntity::Position, 99)],
        );
        std::fs::rename(replacement.path_for_test(), transfer.path())
            .expect("replace descriptor path with same-shape foreign spool");

        let connection = store.open().expect("catalogue connection");
        assert!(matches!(
            attach_teslamate_projection_state_transfer(&connection, &transfer),
            Err(StoreError::TeslaMateProjectionState(
                TeslaMateProjectionStateError::TransferDigestMismatch
            ))
        ));
        let state_heads: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM teslamate_import_projection_state_heads WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("catalogue remains untouched");
        assert_eq!(state_heads, 0);
    }

    #[test]
    fn sealed_transfer_attachment_is_read_only() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (_, binding, _) = v2_base_manifest(&store);
        let state = projection_state_with_digest_rows(
            temporary.path(),
            binding.selected_car_id,
            &[(TeslaMateProjectionStateEntity::Position, 12)],
        );
        let transfer = state
            .sealed_transfer(binding.selected_car_id)
            .expect("sealed transfer descriptor");
        let connection = store.open().expect("catalogue connection");
        attach_teslamate_projection_state_transfer(&connection, &transfer)
            .expect("attach sealed spool read-only");
        assert!(
            connection
                .execute(
                    "DELETE FROM teslamate_projection_state_spool.current_rows",
                    [],
                )
                .is_err(),
            "SQLite mode=ro attachment must reject source mutation"
        );
        detach_teslamate_projection_state_transfer(&store, &connection)
            .expect("detach read-only sealed spool");
        assert_eq!(
            state
                .page(None, MAX_PAGE_SIZE)
                .expect("source state remains readable")
                .rows
                .len(),
            2,
            "failed attachment write must leave the source spool intact"
        );
    }

    #[test]
    fn direct_import_base_refuses_a_staging_car_outside_its_v2_binding() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let wrong_car_id = binding.selected_car_id + 1;
        let run_id = store
            .begin_import_generation(binding.account_id, vehicle.vehicle_id, wrong_car_id, 2_000)
            .expect("staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &TeslaMateOpenSession {
                    car_id: wrong_car_id,
                    ..Default::default()
                },
            )
            .expect("stage direct-import tail");
        let state = direct_test_projection_state(
            &store,
            run_id,
            &import_delta_test_car(binding.selected_car_id),
        );

        assert!(matches!(
            store.finalize_import_generation_with_projection_state(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                wrong_car_id,
                2_000,
                &manifest,
                Sha256Digest::of_bytes(b"wrong-staging-car"),
                &[],
                &binding,
                &state,
            ),
            Err(StoreError::LineageCatalogConflict)
        ));
        assert!(
            store
                .lineage_manifest_for_vehicle(vehicle.vehicle_id)
                .expect("lineage lookup")
                .is_none(),
            "a mismatched staging car must not publish the base"
        );
        let connection = store.open().expect("catalogue");
        let generations: i64 = connection
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("generation count");
        assert_eq!(generations, 1, "rejected generation remains retryable");
    }

    #[test]
    fn v2_base_requires_immutable_binding_at_generic_publication_boundaries() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        let base_digest = manifest.chunks[0].sha256;
        let lineage = LineageManifestV2 {
            protocol: LINEAGE_PROTOCOL_V2,
            capability: LineageCapability::ImmutableBaseOrderedDeltas,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: manifest.installation_id,
            account_id: manifest.account_id,
            vehicle_id: manifest.vehicle_id,
            generation: manifest.generation,
            base: LineageBase {
                snapshot_id: manifest.snapshot_id,
                sequence: manifest.base_sequence,
                digest: base_digest,
                packs: manifest.chunks.clone(),
            },
            deltas: Vec::new(),
            head_sequence: manifest.head_sequence,
            head_digest: base_digest,
            terminal_cursor: manifest.terminal_cursor.clone(),
        };
        lineage.validate().expect("valid V2 base lineage");

        assert!(matches!(
            store.publish_manifest(&manifest),
            Err(StoreError::ImmutableBaseBindingMissing(vehicle_id)) if vehicle_id == vehicle.vehicle_id
        ));
        assert!(matches!(
            store.commit_lineage_catalog(&lineage),
            Err(StoreError::ImmutableBaseBindingMissing(vehicle_id)) if vehicle_id == vehicle.vehicle_id
        ));
        let connection = store.open().expect("open rejected catalogue");
        for table in [
            "sync_manifests",
            "sync_packs",
            "sync_bases",
            "sync_heads",
            "v2_base_bindings",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("rejected catalogue count");
            assert_eq!(count, 0, "generic V2 publication must not write {table}");
        }
        drop(connection);

        store
            .finalize_import_snapshot_with_binding(
                &manifest,
                Sha256Digest::of_bytes(b"generic-v2-binding-regression"),
                &[],
                &binding,
            )
            .expect("binding-aware V2 finalizer");
        assert_eq!(
            store
                .v2_projection_binding(vehicle.vehicle_id)
                .expect("persisted V2 binding"),
            binding
        );
    }

    #[test]
    fn schema_35_upgrade_recovers_v2_binding_from_immutable_base_not_mutable_source() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        store
            .finalize_import_snapshot_with_binding(
                &manifest,
                Sha256Digest::of_bytes(b"legacy-binding-upgrade"),
                &[],
                &binding,
            )
            .expect("catalogue historical V2 base");

        let connection = store.open().expect("historical catalogue");
        connection
            .execute(
                "DELETE FROM v2_base_bindings WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
            )
            .expect("remove post-v35 binding");
        connection
            .execute(
                "UPDATE sources SET generation = 9 WHERE source_id = ?1",
                params![binding.account_id.to_string()],
            )
            .expect("mutate current source generation");
        connection
            .execute(
                "UPDATE vehicles SET source_vehicle_key = '999999' WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
            )
            .expect("mutate current source vehicle key");
        connection
            .execute_batch(
                "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 DROP TABLE sync_retired_lineage_packs;
                 DROP TABLE sync_retired_lineages;
                 DROP TABLE teslamate_import_projection_rows;
                 DROP TABLE teslamate_import_projection_heads;
                 DROP TABLE v2_base_bindings;
                 PRAGMA user_version = 35;",
            )
            .expect("restore historical schema boundary");
        drop(connection);

        let upgraded = HubStore::initialize(temporary.path()).expect("recover immutable binding");
        assert_eq!(
            upgraded
                .v2_projection_binding(vehicle.vehicle_id)
                .expect("recovered V2 binding"),
            binding,
            "upgrade must use the stored base identity and packed source car"
        );
        assert!(
            upgraded
                .lineage_manifest_for_vehicle(vehicle.vehicle_id)
                .expect("upgraded lineage lookup")
                .is_some()
        );
    }

    #[test]
    fn legacy_v2_binding_recovery_fails_closed_on_manifest_pack_identity_conflict() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, manifest) = v2_base_manifest(&store);
        store
            .finalize_import_snapshot_with_binding(
                &manifest,
                Sha256Digest::of_bytes(b"legacy-binding-conflict"),
                &[],
                &binding,
            )
            .expect("catalogue historical V2 base");

        let mut conflicting_manifest = manifest.clone();
        conflicting_manifest.account_id = Uuid::new_v4();
        let conflicting_json = serde_json::to_vec(&conflicting_manifest).expect("manifest JSON");
        let connection = store.open().expect("historical catalogue");
        connection
            .execute(
                "DELETE FROM v2_base_bindings WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
            )
            .expect("remove binding");
        connection
            .execute(
                "UPDATE sync_manifests SET manifest_json = ?1 WHERE snapshot_id = ?2",
                params![conflicting_json, manifest.snapshot_id.to_string()],
            )
            .expect("inject manifest conflict");
        drop(connection);

        assert!(matches!(
            HubStore::initialize(temporary.path()),
            Err(StoreError::LineageCatalogConflict)
        ));
        let connection = store.open().expect("catalogue after failed recovery");
        let bindings: i64 = connection
            .query_row("SELECT COUNT(*) FROM v2_base_bindings", [], |row| {
                row.get(0)
            })
            .expect("binding count");
        assert_eq!(bindings, 0, "failed recovery must roll back atomically");
    }

    fn imported_typed_delta(
        store: &HubStore,
        binding: &ProjectionBinding,
        base: &LineageManifestV2,
    ) -> LineageDelta {
        let sequence = SequenceRange {
            from_exclusive: base.head_sequence,
            to_inclusive: base.head_sequence + 1,
        };
        let payload = ProjectionDelta {
            binding: binding.clone(),
            sequence,
            parent_digest: base.head_digest,
            cars: vec![import_delta_test_car(binding.selected_car_id)],
            car_settings: Vec::new(),
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
            tombstones: Vec::new(),
        };
        let pack = ProjectionPackWriter::new(store.packs_dir())
            .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: base.base.snapshot_id,
                ordinal: store
                    .next_v2_pack_ordinal(base.base.snapshot_id)
                    .expect("fixture delta ordinal"),
                delta: &payload,
            })
            .expect("fixture typed delta");
        let chain_digest = canonical_delta_chain_digest(base.head_digest, pack.metadata.sha256);
        LineageDelta {
            from_sequence: sequence.from_exclusive,
            to_sequence: sequence.to_inclusive,
            parent_chain_digest: base.head_digest,
            chain_digest,
            pack_digest: pack.metadata.sha256,
            pack: pack.metadata,
        }
    }

    fn imported_typed_delta_after(
        store: &HubStore,
        binding: &ProjectionBinding,
        snapshot_id: Uuid,
        from_sequence: u64,
        parent_chain_digest: Sha256Digest,
        ordinal: u32,
    ) -> LineageDelta {
        let sequence = SequenceRange {
            from_exclusive: from_sequence,
            to_inclusive: from_sequence + 1,
        };
        let payload = ProjectionDelta {
            binding: binding.clone(),
            sequence,
            parent_digest: parent_chain_digest,
            cars: vec![import_delta_test_car(binding.selected_car_id)],
            car_settings: Vec::new(),
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
            tombstones: Vec::new(),
        };
        let pack = ProjectionPackWriter::new(store.packs_dir())
            .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id,
                ordinal,
                delta: &payload,
            })
            .expect("fixture typed delta");
        let chain_digest = canonical_delta_chain_digest(parent_chain_digest, pack.metadata.sha256);
        LineageDelta {
            from_sequence: sequence.from_exclusive,
            to_sequence: sequence.to_inclusive,
            parent_chain_digest,
            chain_digest,
            pack_digest: pack.metadata.sha256,
            pack: pack.metadata,
        }
    }

    fn rewrite_import_delta_pack_for_test(
        store: &HubStore,
        delta: &mut LineageDelta,
        mutate: impl FnOnce(&Connection),
    ) {
        let original = store
            .packs_dir()
            .join("sha256")
            .join(format!("{}.sqlite.zst", delta.pack.sha256));
        let inspection = store
            .packs_dir()
            .join(format!(".import-delta-test-{}.sqlite", Uuid::new_v4()));
        fs::write(
            &inspection,
            zstd::stream::decode_all(File::open(original).expect("open typed delta"))
                .expect("decode typed delta"),
        )
        .expect("write typed delta inspection");
        let connection = Connection::open(&inspection).expect("open typed delta inspection");
        mutate(&connection);
        drop(connection);
        let raw = fs::read(&inspection).expect("read rewritten typed delta");
        fs::remove_file(&inspection).expect("remove typed delta inspection");
        let compressed =
            zstd::stream::encode_all(raw.as_slice(), 0).expect("recompress typed delta");
        let sha256 = Sha256Digest::of_bytes(&compressed);
        fs::write(
            store
                .packs_dir()
                .join("sha256")
                .join(format!("{}.sqlite.zst", sha256)),
            &compressed,
        )
        .expect("write rewritten typed delta");
        delta.pack.sha256 = sha256;
        delta.pack.relative_path = TransportPack::canonical_relative_path(sha256);
        delta.pack.compressed_bytes = u64::try_from(compressed.len()).expect("compressed bytes");
        delta.pack.uncompressed_bytes = u64::try_from(raw.len()).expect("uncompressed bytes");
        delta.pack_digest = sha256;
        delta.chain_digest = canonical_delta_chain_digest(delta.parent_chain_digest, sha256);
    }

    fn assert_import_delta_catalogue_unchanged(
        store: &HubStore,
        vehicle_id: Uuid,
        before: &LineageManifestV2,
    ) {
        assert_eq!(
            store
                .lineage_manifest_for_vehicle(vehicle_id)
                .expect("unchanged lineage lookup"),
            Some(before.clone()),
            "rejected import delta must not alter the public lineage"
        );
        let connection = store.open().expect("catalogue");
        for (table, expected) in [
            ("sync_bases", 1),
            ("sync_deltas", 0),
            ("sync_packs", 1),
            // The binding-aware base finalizer atomically records its source
            // fingerprint. A rejected successor must leave that one base
            // fingerprint untouched rather than adding a successor row.
            ("snapshot_fingerprints", 1),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(
                count, expected,
                "rejected import delta must not change {table}"
            );
        }
    }

    fn claimed_collector_delta(
        store: &HubStore,
        vehicle_id: Uuid,
        binding: &ProjectionBinding,
    ) -> (SyncMutationClaim, LineageDelta) {
        let (base_snapshot_id, head_sequence, parent_digest) = store
            .v2_head(vehicle_id)
            .expect("fixture head lookup")
            .expect("fixture V2 head");
        let from_sequence = u64::try_from(head_sequence).expect("non-negative fixture sequence");
        store
            .persist_materialised_car_if_absent(
                vehicle_id,
                &import_delta_test_car(binding.selected_car_id),
            )
            .expect("record a collector-shaped car mutation");
        let claim = store
            .claim_sync_mutations(vehicle_id, 2_000, 100)
            .expect("claim collector mutation")
            .expect("one collector mutation pending");
        let to_sequence = from_sequence
            .checked_add(u64::try_from(claim.mutations.len()).expect("claim length"))
            .expect("fixture sequence range");
        let payload = store
            .projection_delta_for_mutations(
                &claim,
                binding.clone(),
                SequenceRange {
                    from_exclusive: from_sequence,
                    to_inclusive: to_sequence,
                },
                parent_digest,
            )
            .expect("project claimed mutation");
        let pack = ProjectionPackWriter::new(store.packs_dir())
            .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: base_snapshot_id,
                ordinal: store
                    .next_v2_pack_ordinal(base_snapshot_id)
                    .expect("fixture delta ordinal"),
                delta: &payload,
            })
            .expect("write collector-shaped delta");
        let chain_digest = canonical_delta_chain_digest(parent_digest, pack.metadata.sha256);
        let delta = LineageDelta {
            from_sequence,
            to_sequence,
            parent_chain_digest: parent_digest,
            chain_digest,
            pack_digest: pack.metadata.sha256,
            pack: pack.metadata,
        };
        (claim, delta)
    }

    fn sync_claim_publication_state(
        store: &HubStore,
        claim: &SyncMutationClaim,
    ) -> Vec<(i64, i64, i64)> {
        let connection = store.open().expect("claim state database");
        connection
            .prepare(
                "SELECT revision, published, claimed_until_ms
                 FROM sync_mutations
                 WHERE vehicle_id = ?1 AND revision BETWEEN ?2 AND ?3
                 ORDER BY revision",
            )
            .expect("claim state query")
            .query_map(
                params![
                    claim.vehicle_id.to_string(),
                    claim.from_revision,
                    claim.to_revision
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("claim state rows")
            .map(|row| row.expect("claim state row"))
            .collect()
    }

    #[test]
    fn v2_delta_claim_rejects_invalid_inputs_without_publishing_then_is_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, _) = imported_v2_base(&store);
        let (claim, delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
        let head_before = store
            .v2_head(vehicle.vehicle_id)
            .expect("head before rejection");
        let claim_before = sync_claim_publication_state(&store, &claim);
        assert!(
            claim_before.iter().all(
                |(_, published, claimed_until_ms)| *published == 0 && *claimed_until_ms > 2_000
            ),
            "fixture must start with only leased, unpublished mutations"
        );
        let assert_rejected = || {
            assert_eq!(
                store
                    .v2_head(vehicle.vehicle_id)
                    .expect("head after rejection"),
                head_before,
                "rejected input must not advance the V2 head"
            );
            assert_eq!(
                sync_claim_publication_state(&store, &claim),
                claim_before,
                "rejected input must not publish or release the claimed mutations"
            );
        };
        let valid_cursor = import_delta_test_cursor(&binding, delta.to_sequence);

        let invalid_signature = OpaqueCursor::issue(
            &CursorKey::from_bytes([62; 32]),
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding.installation_id,
                account_id: binding.account_id,
                vehicle_id: binding.vehicle_id,
                generation: binding.generation,
                sequence: delta.to_sequence,
            },
        )
        .expect("well-formed cursor with a wrong HMAC");
        assert!(matches!(
            store.commit_v2_delta_claim(
                &claim,
                &delta,
                &import_delta_test_cursor_key(),
                &invalid_signature,
            ),
            Err(StoreError::Manifest(ProtocolError::InvalidCursorSignature))
        ));
        assert_rejected();

        let wrong_claims = OpaqueCursor::issue(
            &import_delta_test_cursor_key(),
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding.installation_id,
                account_id: Uuid::new_v4(),
                vehicle_id: binding.vehicle_id,
                generation: binding.generation,
                sequence: delta.to_sequence,
            },
        )
        .expect("validly signed cursor with wrong claims");
        assert!(matches!(
            store.commit_v2_delta_claim(
                &claim,
                &delta,
                &import_delta_test_cursor_key(),
                &wrong_claims,
            ),
            Err(StoreError::LineageCatalogConflict)
        ));
        assert_rejected();

        let mut noncanonical_chain = delta.clone();
        noncanonical_chain.chain_digest = Sha256Digest::of_bytes(b"noncanonical collector claim");
        assert!(matches!(
            store.commit_v2_delta_claim(
                &claim,
                &noncanonical_chain,
                &import_delta_test_cursor_key(),
                &valid_cursor,
            ),
            Err(StoreError::LineageCatalogConflict)
        ));
        assert_rejected();

        let mut malformed_pack = delta.clone();
        rewrite_import_delta_pack_for_test(&store, &mut malformed_pack, |connection| {
            connection
                .execute_batch(
                    "CREATE TRIGGER unexpected_after_car_insert
                     AFTER INSERT ON cars BEGIN SELECT 1; END;",
                )
                .expect("make the typed delta schema malformed");
        });
        assert!(matches!(
            store.commit_v2_delta_claim(
                &claim,
                &malformed_pack,
                &import_delta_test_cursor_key(),
                &valid_cursor,
            ),
            Err(StoreError::LineageCatalogConflict)
        ));
        assert_rejected();

        let mut wrong_binding_pack = delta.clone();
        rewrite_import_delta_pack_for_test(&store, &mut wrong_binding_pack, |connection| {
            connection
                .execute(
                    "UPDATE hub_pack_metadata SET value = ?1 WHERE key = 'account_id'",
                    params![Uuid::new_v4().to_string()],
                )
                .expect("retarget typed delta metadata");
        });
        assert!(matches!(
            store.commit_v2_delta_claim(
                &claim,
                &wrong_binding_pack,
                &import_delta_test_cursor_key(),
                &valid_cursor,
            ),
            Err(StoreError::LineageCatalogConflict)
        ));
        assert_rejected();

        store
            .commit_v2_delta_claim(
                &claim,
                &delta,
                &import_delta_test_cursor_key(),
                &valid_cursor,
            )
            .expect("valid collector-shaped delta");
        let head_after = store
            .v2_head(vehicle.vehicle_id)
            .expect("head after success");
        assert_ne!(head_after, head_before, "valid delta advances the V2 head");
        assert_eq!(
            sync_claim_publication_state(&store, &claim),
            claim_before
                .iter()
                .map(|(revision, _, _)| (*revision, 1, 0))
                .collect::<Vec<_>>(),
            "valid delta marks every claimed mutation published"
        );

        store
            .commit_v2_delta_claim(
                &claim,
                &delta,
                &import_delta_test_cursor_key(),
                &valid_cursor,
            )
            .expect("idempotent collector delta replay");
        assert_eq!(
            store
                .v2_head(vehicle.vehicle_id)
                .expect("head after replay"),
            head_after,
            "idempotent replay leaves the V2 head unchanged"
        );
        assert_eq!(
            sync_claim_publication_state(&store, &claim),
            claim_before
                .iter()
                .map(|(revision, _, _)| (*revision, 1, 0))
                .collect::<Vec<_>>(),
            "idempotent replay keeps every claimed mutation published"
        );
    }

    #[test]
    fn live_delta_compaction_preserves_the_base_and_rebuilds_from_durable_journal_payloads() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);

        let (car_claim, car_delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
        store
            .commit_v2_delta_claim(
                &car_claim,
                &car_delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, car_delta.to_sequence),
            )
            .expect("publish first live delta");
        let disabled = ProjectionCarSettings {
            enabled: false,
            ..ProjectionCarSettings::default()
        };
        store
            .upsert_car_settings(vehicle.vehicle_id, binding.selected_car_id, &disabled)
            .expect("record second live mutation");
        let (settings_claim, settings_delta) =
            claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
        store
            .commit_v2_delta_claim(
                &settings_claim,
                &settings_delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, settings_delta.to_sequence),
            )
            .expect("publish second live delta");

        let before = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage before compaction")
            .expect("published lineage");
        assert_eq!(before.base, base.base, "immutable base must not drift");
        assert_eq!(
            before.deltas,
            vec![car_delta.clone(), settings_delta.clone()]
        );
        let old_paths = before
            .deltas
            .iter()
            .map(|delta| {
                store
                    .packs_dir()
                    .join("sha256")
                    .join(format!("{}.sqlite.zst", delta.pack_digest))
            })
            .collect::<Vec<_>>();

        let plan = store
            .plan_live_delta_compaction(vehicle.vehicle_id)
            .expect("compaction plan")
            .expect("two live deltas form a compactable suffix");
        assert_eq!(plan.replaced_spans.len(), 2);
        let payload = store
            .projection_delta_for_compaction(&plan, binding.clone())
            .expect("journal-derived compacted payload");
        assert_eq!(payload.cars.len(), 1);
        assert!(payload.car_settings.is_empty());
        assert!(!payload.cars[0].settings.enabled);
        let built = ProjectionPackWriter::new(store.packs_dir())
            .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: plan.base_snapshot_id,
                ordinal: plan.first_ordinal,
                delta: &payload,
            })
            .expect("write compacted pack");
        let compacted = LineageDelta {
            from_sequence: plan.anchor_sequence,
            to_sequence: plan.head_sequence,
            parent_chain_digest: plan.anchor_digest,
            chain_digest: canonical_delta_chain_digest(plan.anchor_digest, built.metadata.sha256),
            pack_digest: built.metadata.sha256,
            pack: built.metadata,
        };

        // A newer mutable value can arrive while the immutable candidate is
        // being written. The compacted payload remains bound to its journal
        // window; the later revision stays unpublished for the next delta.
        let enabled = ProjectionCarSettings {
            enabled: true,
            ..ProjectionCarSettings::default()
        };
        store
            .upsert_car_settings(vehicle.vehicle_id, binding.selected_car_id, &enabled)
            .expect("record mutation after compaction plan");
        let retired_at_ms = retired_lineage_clock_ms().expect("retention clock");
        store
            .commit_live_delta_compaction_at(
                &plan,
                &compacted,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, compacted.to_sequence),
                retired_at_ms,
            )
            .expect("atomically swap compacted suffix");

        let after = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage after compaction")
            .expect("published lineage");
        after.validate().expect("compacted lineage validates");
        assert_eq!(after.base, before.base);
        assert_eq!(after.head_sequence, before.head_sequence);
        assert_ne!(after.head_digest, before.head_digest);
        assert_eq!(after.deltas, vec![compacted.clone()]);
        let retention_connection = store.open().expect("retention catalogue");
        let (retired_manifest_json, expires_at_ms): (Vec<u8>, i64) = retention_connection
            .query_row(
                "SELECT manifest_json, expires_at_ms
                 FROM sync_retired_lineages
                 WHERE vehicle_id = ?1 AND head_digest = ?2",
                params![
                    vehicle.vehicle_id.to_string(),
                    before.head_digest.to_string()
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("retired prior lineage");
        assert_eq!(
            serde_json::from_slice::<LineageManifestV2>(&retired_manifest_json)
                .expect("retired manifest JSON"),
            before,
            "retention authorization must be bound to the exact prior lineage"
        );
        assert_eq!(
            expires_at_ms,
            retired_at_ms + RETIRED_LINEAGE_PACK_RETENTION_MS
        );
        for delta in &before.deltas {
            assert!(
                store
                    .retired_pack_for_digest_at(
                        &retention_connection,
                        delta.pack_digest,
                        retired_at_ms + 1,
                    )
                    .expect("retired pack authorization")
                    .is_some(),
                "each replaced pack remains authorized through its prior lineage"
            );
        }
        drop(retention_connection);
        assert!(
            old_paths.iter().all(|path| path.is_file()),
            "old immutable objects remain available through bounded retention"
        );
        assert!(
            store
                .plan_live_delta_compaction(vehicle.vehicle_id)
                .expect("post-compaction plan")
                .is_none(),
            "one compacted live span cannot gain another pack"
        );

        let (newer_claim, newer_delta) =
            claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
        assert_eq!(newer_claim.mutations.len(), 1);
        store
            .commit_v2_delta_claim(
                &newer_claim,
                &newer_delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, newer_delta.to_sequence),
            )
            .expect("publish mutation that arrived during compaction");
        drop(store);

        let reopened = HubStore::initialize(temporary.path()).expect("restart compacted store");
        let restarted = reopened
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage after restart")
            .expect("published lineage after restart");
        restarted.validate().expect("restart lineage validates");
        assert_eq!(restarted.base, base.base);
        assert_eq!(
            restarted.deltas,
            vec![compacted.clone(), newer_delta.clone()]
        );
        for delta in &before.deltas {
            assert!(
                reopened
                    .pack_for_digest(delta.pack_digest)
                    .expect("retired pack lookup after restart")
                    .is_some(),
                "restart must retain prior-lineage authorization"
            );
        }

        let backup_root = temporary.path().join("retained-lineage-backup");
        reopened
            .backup_to(&backup_root)
            .expect("backup includes unexpired retired packs");
        let restored = HubStore::initialize(&backup_root).expect("restore retained backup");
        for delta in &before.deltas {
            assert!(
                restored
                    .pack_for_digest(delta.pack_digest)
                    .expect("restored retired pack lookup")
                    .is_some(),
                "restore must preserve unexpired prior-lineage downloads"
            );
        }
        drop(restored);

        let retention_connection = reopened.open().expect("retention after restart");
        for delta in &before.deltas {
            assert!(
                reopened
                    .retired_pack_for_digest_at(
                        &retention_connection,
                        delta.pack_digest,
                        expires_at_ms,
                    )
                    .expect("retired pack at exact expiry")
                    .is_none(),
                "authorization expires at the declared boundary"
            );
        }
        drop(retention_connection);
        reopened
            .repair_at(expires_at_ms + 1)
            .expect("repair inside physical-delete grace");
        assert!(
            old_paths.iter().all(|path| path.is_file()),
            "physical grace protects a just-authorized in-flight open"
        );
        reopened
            .repair_at(expires_at_ms + RETIRED_LINEAGE_PACK_DELETE_GRACE_MS)
            .expect("repair after retired-pack grace");
        assert!(
            old_paths.iter().all(|path| !path.exists()),
            "expired retired objects are eventually removed"
        );
        assert_eq!(
            reopened
                .lineage_manifest_for_vehicle(vehicle.vehicle_id)
                .expect("current lineage after retired cleanup")
                .expect("current lineage remains published"),
            restarted
        );
    }

    #[test]
    fn live_delta_compaction_coalesces_cross_table_settings_and_tombstones_by_revision() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, _) = imported_v2_base(&store);
        let plan = |mutations| LiveDeltaCompactionPlan {
            vehicle_id: vehicle.vehicle_id,
            base_snapshot_id: Uuid::new_v4(),
            anchor_sequence: 1,
            anchor_digest: Sha256Digest::of_bytes(b"coalescing anchor"),
            head_sequence: 3,
            head_digest: Sha256Digest::of_bytes(b"coalescing old head"),
            first_ordinal: 1,
            from_revision: 1,
            to_revision: 2,
            mutations,
            replaced_spans: Vec::new(),
        };
        let mutation =
            |revision: i64, entity: &str, entity_id: i64, operation: &str, payload_json: String| {
                SyncMutation {
                    vehicle_id: vehicle.vehicle_id,
                    revision,
                    entity: entity.into(),
                    entity_id,
                    car_id: binding.selected_car_id,
                    operation: operation.into(),
                    payload_json,
                }
            };

        let disabled = ProjectionCarSettings {
            enabled: false,
            ..ProjectionCarSettings::default()
        };
        let car = import_delta_test_car(binding.selected_car_id);
        let older_setting_newer_car = plan(vec![
            mutation(
                2,
                "car",
                binding.selected_car_id,
                "upsert",
                serde_json::to_string(&car).expect("serialize newer car"),
            ),
            mutation(
                1,
                "car_setting",
                binding.selected_car_id,
                "upsert",
                serde_json::to_string(&disabled).expect("serialize older settings"),
            ),
        ]);
        let payload = store
            .projection_delta_for_compaction(&older_setting_newer_car, binding.clone())
            .expect("newer full car wins over older settings patch");
        assert_eq!(payload.cars, vec![car.clone()]);
        assert!(payload.car_settings.is_empty());

        let older_car_newer_setting = plan(vec![
            mutation(
                2,
                "car_setting",
                binding.selected_car_id,
                "upsert",
                serde_json::to_string(&disabled).expect("serialize newer settings"),
            ),
            mutation(
                1,
                "car",
                binding.selected_car_id,
                "upsert",
                serde_json::to_string(&car).expect("serialize older car"),
            ),
        ]);
        let payload = store
            .projection_delta_for_compaction(&older_car_newer_setting, binding.clone())
            .expect("newer settings patch is folded into older full car");
        assert_eq!(payload.cars.len(), 1);
        assert!(!payload.cars[0].settings.enabled);
        assert!(payload.car_settings.is_empty());

        let state = crate::hub_pack::ProjectionState {
            id: 77,
            car_id: binding.selected_car_id,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: Some(2_000),
        };
        let newer_tombstone = plan(vec![
            mutation(2, "state", state.id, "tombstone", "{}".into()),
            mutation(
                1,
                "state",
                state.id,
                "upsert",
                serde_json::to_string(&state).expect("serialize older state"),
            ),
        ]);
        let payload = store
            .projection_delta_for_compaction(&newer_tombstone, binding.clone())
            .expect("newer tombstone wins");
        assert!(payload.states.is_empty());
        assert_eq!(
            payload.tombstones,
            vec![ProjectionTombstone {
                entity: ProjectionDeltaEntity::State,
                id: state.id,
                car_id: binding.selected_car_id,
            }]
        );

        let newer_upsert = plan(vec![
            mutation(
                2,
                "state",
                state.id,
                "upsert",
                serde_json::to_string(&state).expect("serialize newer state"),
            ),
            mutation(1, "state", state.id, "tombstone", "{}".into()),
        ]);
        let payload = store
            .projection_delta_for_compaction(&newer_upsert, binding)
            .expect("newer upsert wins");
        assert_eq!(payload.states, vec![state]);
        assert!(payload.tombstones.is_empty());
    }

    #[test]
    fn live_delta_admission_refuses_an_unservable_next_pack_when_no_compaction_can_gain_space() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, _) = imported_v2_base(&store);
        let two_pack_limit = ProtocolLimits {
            max_chunks: 2,
            ..ProtocolLimits::default()
        };

        let (first_claim, first_delta) =
            claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
        store
            .commit_v2_delta_claim_with_limits(
                &first_claim,
                &first_delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, first_delta.to_sequence),
                two_pack_limit,
            )
            .expect("fill the reduced release bound exactly");
        let prior = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("prior lineage")
            .expect("published lineage");
        prior
            .validate_with_limits(two_pack_limit)
            .expect("prior manifest remains servable at the bound");
        assert!(
            store
                .plan_live_delta_compaction(vehicle.vehicle_id)
                .expect("compaction availability")
                .is_none(),
            "one live delta cannot be replaced by fewer packs"
        );

        store
            .upsert_car_settings(
                vehicle.vehicle_id,
                binding.selected_car_id,
                &ProjectionCarSettings {
                    enabled: false,
                    ..ProjectionCarSettings::default()
                },
            )
            .expect("new mutation at capacity");
        let (second_claim, second_delta) =
            claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
        let claim_before = sync_claim_publication_state(&store, &second_claim);
        assert!(matches!(
            store.commit_v2_delta_claim_with_limits(
                &second_claim,
                &second_delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, second_delta.to_sequence),
                two_pack_limit,
            ),
            Err(StoreError::LineageCapacityExhausted)
        ));
        assert_eq!(
            store
                .lineage_manifest_for_vehicle(vehicle.vehicle_id)
                .expect("lineage after refused admission")
                .expect("prior lineage still published"),
            prior
        );
        assert_eq!(
            sync_claim_publication_state(&store, &second_claim),
            claim_before
        );
    }

    #[test]
    fn settings_only_sync_delta_is_emitted_and_retained() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, _) = imported_v2_base(&store);

        let (car_claim, car_delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
        store
            .commit_v2_delta_claim(
                &car_claim,
                &car_delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, car_delta.to_sequence),
            )
            .expect("publish the materialised-car precursor");

        let settings = ProjectionCarSettings {
            enabled: false,
            ..ProjectionCarSettings::default()
        };
        store
            .upsert_car_settings(vehicle.vehicle_id, binding.selected_car_id, &settings)
            .expect("record standalone settings mutation");
        let claim = store
            .claim_sync_mutations(vehicle.vehicle_id, 3_000, 100)
            .expect("claim settings mutation")
            .expect("settings mutation pending");
        assert_eq!(claim.mutations.len(), 1);
        assert_eq!(claim.mutations[0].entity, "car_setting");

        let (base_snapshot_id, head_sequence, parent_digest) = store
            .v2_head(vehicle.vehicle_id)
            .expect("V2 head")
            .expect("published V2 base");
        let from_sequence = u64::try_from(head_sequence).expect("non-negative sequence");
        let to_sequence = from_sequence
            .checked_add(u64::try_from(claim.mutations.len()).expect("claim length"))
            .expect("sequence range");
        let payload = store
            .projection_delta_for_mutations(
                &claim,
                binding.clone(),
                SequenceRange {
                    from_exclusive: from_sequence,
                    to_inclusive: to_sequence,
                },
                parent_digest,
            )
            .expect("project standalone settings mutation");
        assert!(payload.cars.is_empty());
        assert_eq!(
            payload.car_settings,
            vec![ProjectionCarSettingsPatch {
                car_id: binding.selected_car_id,
                settings: settings.clone(),
            }]
        );
        assert!(
            !payload.is_empty(),
            "a settings-only patch must not take the typed-delta no-op path"
        );

        let built = ProjectionPackWriter::new(store.packs_dir())
            .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: base_snapshot_id,
                ordinal: store
                    .next_v2_pack_ordinal(base_snapshot_id)
                    .expect("settings delta ordinal"),
                delta: &payload,
            })
            .expect("write settings-only typed delta");
        assert_eq!(built.metadata.row_count, 1);
        assert_eq!(built.metadata.tables, vec![MirrorTable::Car]);
        let inspection_path = temporary.path().join("settings-only-delta.sqlite");
        fs::write(
            &inspection_path,
            zstd::stream::decode_all(File::open(&built.path).expect("open settings delta"))
                .expect("decode settings delta"),
        )
        .expect("write settings delta inspection");
        let inspection = Connection::open(inspection_path).expect("open settings delta inspection");
        let cars: i64 = inspection
            .query_row("SELECT COUNT(*) FROM cars", [], |row| row.get(0))
            .expect("count delta cars");
        let car_settings: i64 = inspection
            .query_row("SELECT COUNT(*) FROM car_settings", [], |row| row.get(0))
            .expect("count delta settings patches");
        assert_eq!((cars, car_settings), (0, 1));

        let delta = LineageDelta {
            from_sequence,
            to_sequence,
            parent_chain_digest: parent_digest,
            chain_digest: canonical_delta_chain_digest(parent_digest, built.metadata.sha256),
            pack_digest: built.metadata.sha256,
            pack: built.metadata,
        };
        store
            .commit_v2_delta_claim(
                &claim,
                &delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
            )
            .expect("retain settings-only typed delta");
        let lineage = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("published lineage")
            .expect("lineage exists");
        lineage.validate().expect("settings-only lineage validates");
        assert_eq!(lineage.deltas, vec![car_delta, delta]);
    }

    #[test]
    fn import_delta_finalizer_rejects_full_base_relabel_without_catalogue_mutation() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let mut forged_pack = base.base.packs[0].clone();
        forged_pack.sequence = SequenceRange {
            from_exclusive: base.head_sequence,
            to_inclusive: base.head_sequence + 1,
        };
        let forged = LineageDelta {
            from_sequence: forged_pack.sequence.from_exclusive,
            to_sequence: forged_pack.sequence.to_inclusive,
            parent_chain_digest: base.head_digest,
            chain_digest: canonical_delta_chain_digest(base.head_digest, forged_pack.sha256),
            pack_digest: forged_pack.sha256,
            pack: forged_pack,
        };

        let error = store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &forged,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, forged.to_sequence),
                Sha256Digest::of_bytes(b"forged-base-relabel"),
                &[],
            )
            .expect_err("a full base cannot be relabelled as a typed delta");
        assert!(matches!(error, StoreError::LineageCatalogConflict));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
    }

    #[test]
    fn import_delta_finalizer_rejects_wrong_chain_then_accepts_the_written_typed_delta() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let delta = imported_typed_delta(&store, &binding, &base);
        let mut wrong_chain = delta.clone();
        wrong_chain.chain_digest = Sha256Digest::of_bytes(b"wrong import delta chain");

        let error = store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &wrong_chain,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, wrong_chain.to_sequence),
                Sha256Digest::of_bytes(b"wrong-chain"),
                &[],
            )
            .expect_err("a caller-supplied noncanonical chain must be rejected");
        assert!(matches!(error, StoreError::LineageCatalogConflict));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);

        store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
                Sha256Digest::of_bytes(b"valid-typed-delta"),
                &[],
            )
            .expect("the writer-produced typed delta is accepted");
        let lineage = store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("published lineage")
            .expect("lineage exists");
        lineage.validate().expect("published lineage validates");
        assert_eq!(lineage.deltas, vec![delta]);
    }

    #[test]
    fn import_delta_writer_rejects_geofence_and_address_tombstones() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let sequence = SequenceRange {
            from_exclusive: base.head_sequence,
            to_inclusive: base.head_sequence + 1,
        };
        for entity in [
            ProjectionDeltaEntity::Geofence,
            ProjectionDeltaEntity::Address,
        ] {
            let payload = ProjectionDelta {
                binding: binding.clone(),
                sequence,
                parent_digest: base.head_digest,
                cars: Vec::new(),
                car_settings: Vec::new(),
                drives: Vec::new(),
                positions: Vec::new(),
                charges: Vec::new(),
                charge_samples: Vec::new(),
                states: Vec::new(),
                updates: Vec::new(),
                tombstones: vec![ProjectionTombstone {
                    entity,
                    id: 90,
                    car_id: binding.selected_car_id,
                }],
            };
            let error = ProjectionPackWriter::new(store.packs_dir())
                .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
                    pack_id: Uuid::new_v4(),
                    snapshot_id: base.base.snapshot_id,
                    ordinal: store
                        .next_v2_pack_ordinal(base.base.snapshot_id)
                        .expect("fixture delta ordinal"),
                    delta: &payload,
                })
                .expect_err("writer rejects unsupported source-owned tombstone entities");
            assert!(matches!(
                error,
                ProjectionPackError::Invalid(message)
                    if message.contains("unsupported source-owned delta tombstone entity")
            ));
            assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
        }
    }

    #[test]
    fn import_delta_finalizer_requires_the_exact_signed_terminal_cursor_claims() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let delta = imported_typed_delta(&store, &binding, &base);

        let wrong_generation = OpaqueCursor::issue(
            &import_delta_test_cursor_key(),
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding.installation_id,
                account_id: binding.account_id,
                vehicle_id: binding.vehicle_id,
                generation: binding.generation + 1,
                sequence: delta.to_sequence,
            },
        )
        .expect("valid cursor for a different generation");
        let error = store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &wrong_generation,
                Sha256Digest::of_bytes(b"wrong-cursor-claims"),
                &[],
            )
            .expect_err("a cursor with mismatched claims must not publish a delta");
        assert!(matches!(error, StoreError::LineageCatalogConflict));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);

        let wrong_key = CursorKey::from_bytes([62; 32]);
        let invalid_signature = OpaqueCursor::issue(
            &wrong_key,
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding.installation_id,
                account_id: binding.account_id,
                vehicle_id: binding.vehicle_id,
                generation: binding.generation,
                sequence: delta.to_sequence,
            },
        )
        .expect("well-formed cursor signed with another key");
        let error = store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &invalid_signature,
                Sha256Digest::of_bytes(b"invalid-cursor-signature"),
                &[],
            )
            .expect_err("a cursor signed with another key must not publish a delta");
        assert!(matches!(
            error,
            StoreError::Manifest(ProtocolError::InvalidCursorSignature)
        ));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
    }

    #[test]
    fn import_delta_finalizer_accepts_a_car_and_completed_drive() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let sequence = SequenceRange {
            from_exclusive: base.head_sequence,
            to_inclusive: base.head_sequence + 1,
        };
        let payload = ProjectionDelta {
            binding: binding.clone(),
            sequence,
            parent_digest: base.head_digest,
            cars: vec![import_delta_test_car(binding.selected_car_id)],
            car_settings: Vec::new(),
            drives: vec![ProjectionDrive {
                id: 99,
                car_id: binding.selected_car_id,
                optimized_at_ms: None,
                start_date_ms: 2_000,
                end_date_ms: 3_000,
                distance_km: Some(10.0),
                duration_min: Some(1),
                efficiency: None,
                outside_temp_avg: None,
                inside_temp_avg: None,
                speed_max: Some(50),
                power_max: None,
                power_min: None,
                start_ideal_range_km: None,
                end_ideal_range_km: None,
                start_address: None,
                end_address: None,
                start_geofence: None,
                end_geofence: None,
                start_latitude: None,
                start_longitude: None,
                end_latitude: None,
                end_longitude: None,
                start_soc: None,
                end_soc: None,
                start_rated_range_km: Some(300.0),
                end_rated_range_km: Some(280.0),
                ascent: None,
                descent: None,
            }],
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
            tombstones: Vec::new(),
        };
        let pack = ProjectionPackWriter::new(store.packs_dir())
            .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: base.base.snapshot_id,
                ordinal: store
                    .next_v2_pack_ordinal(base.base.snapshot_id)
                    .expect("fixture delta ordinal"),
                delta: &payload,
            })
            .expect("fixture typed delta");
        let delta = LineageDelta {
            from_sequence: sequence.from_exclusive,
            to_sequence: sequence.to_inclusive,
            parent_chain_digest: base.head_digest,
            chain_digest: canonical_delta_chain_digest(base.head_digest, pack.metadata.sha256),
            pack_digest: pack.metadata.sha256,
            pack: pack.metadata,
        };

        store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
                Sha256Digest::of_bytes(b"car-and-drive"),
                &[],
            )
            .expect("writer-produced completed drive delta is accepted");
    }

    #[test]
    fn import_delta_finalizer_rejects_a_typed_delta_for_another_selected_car() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let mut delta = imported_typed_delta(&store, &binding, &base);
        let forged_car_id = binding.selected_car_id + 1;
        rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
            // Model a malicious external SQLite pack. The resulting rows are
            // internally consistent, but its selected-car identity differs
            // from the immutable catalogue binding.
            connection
                .execute_batch("PRAGMA foreign_keys = OFF;")
                .expect("allow synthetic retargeting");
            connection
                .execute("UPDATE cars SET id = ?1", params![forged_car_id])
                .expect("retarget typed delta car");
            connection
                .execute(
                    "UPDATE car_settings SET car_id = ?1",
                    params![forged_car_id],
                )
                .expect("retarget typed delta settings");
            connection
                .execute(
                    "UPDATE hub_pack_metadata SET value = ?1 WHERE key = 'selected_car_id'",
                    params![forged_car_id.to_string()],
                )
                .expect("rebind typed delta metadata");
        });

        let error = store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
                Sha256Digest::of_bytes(b"wrong-selected-car"),
                &[],
            )
            .expect_err("a typed delta for another selected car must be rejected");
        assert!(matches!(error, StoreError::LineageCatalogConflict));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
    }

    #[test]
    fn import_delta_finalizer_rejects_matching_metadata_with_an_extra_schema_object() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let mut delta = imported_typed_delta(&store, &binding, &base);
        rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
            connection
                .execute_batch(
                    "CREATE TRIGGER unexpected_after_car_insert
                     AFTER INSERT ON cars BEGIN SELECT 1; END;",
                )
                .expect("add unexpected trigger");
        });

        let error = store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
                Sha256Digest::of_bytes(b"unexpected-schema-object"),
                &[],
            )
            .expect_err("metadata cannot bless an unexpected SQLite program");
        assert!(matches!(error, StoreError::LineageCatalogConflict));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
    }

    #[test]
    fn import_delta_finalizer_rejects_forged_unsupported_tombstone_entities() {
        for entity in ["not-an-entity", "car", "car_setting", "geofence", "address"] {
            let temporary = tempfile::tempdir().expect("temporary store");
            let store = HubStore::initialize(temporary.path()).expect("store");
            let (vehicle, binding, base) = imported_v2_base(&store);
            let mut delta = imported_typed_delta(&store, &binding, &base);
            rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
                connection
                    .execute(
                        "INSERT INTO tombstones(entity, entity_id, car_id) VALUES (?1, 1, 10)",
                        params![entity],
                    )
                    .expect("insert unsupported tombstone");
                connection
                    .execute(
                        "UPDATE hub_pack_metadata SET value = '2' WHERE key = 'row_count'",
                        [],
                    )
                    .expect("update declared row count");
            });
            delta.pack.row_count = 2;
            delta.pack.tables.push(MirrorTable::Tombstone);

            let error = store
                .finalize_import_delta_successor(
                    vehicle.vehicle_id,
                    &delta,
                    &import_delta_test_cursor_key(),
                    &import_delta_test_cursor(&binding, delta.to_sequence),
                    Sha256Digest::of_bytes(entity.as_bytes()),
                    &[],
                )
                .expect_err("typed delta semantics must be valid before publication");
            assert!(matches!(error, StoreError::LineageCatalogConflict));
            assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
        }
    }

    #[test]
    fn import_delta_finalizer_rejects_forged_upsert_tombstone_overlap() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let mut delta = imported_typed_delta(&store, &binding, &base);
        rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
            connection
                .execute_batch(
                    "INSERT INTO positions(
                        id, drive_id, car_id, date_ms, latitude, longitude
                     ) VALUES (99, NULL, 10, 2_000, 51.5, -0.1);
                     INSERT INTO tombstones(entity, entity_id, car_id)
                        VALUES ('position', 99, 10);",
                )
                .expect("forge a validly shaped position/tombstone overlap");
            connection
                .execute(
                    "UPDATE hub_pack_metadata SET value = '3' WHERE key = 'row_count'",
                    [],
                )
                .expect("update declared row count");
        });
        delta.pack.row_count = 3;
        delta
            .pack
            .tables
            .extend([MirrorTable::Position, MirrorTable::Tombstone]);

        let error = store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
                Sha256Digest::of_bytes(b"forged-upsert-tombstone-overlap"),
                &[],
            )
            .expect_err("a forged typed row cannot be both upserted and tombstoned");
        assert!(matches!(error, StoreError::LineageCatalogConflict));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
    }

    #[test]
    fn import_delta_finalizer_requires_the_companion_settings_for_each_car() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let mut delta = imported_typed_delta(&store, &binding, &base);
        rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
            connection
                .execute("DELETE FROM car_settings WHERE car_id = 10", [])
                .expect("remove required car settings");
            connection
                .execute(
                    "INSERT INTO tombstones(entity, entity_id, car_id) VALUES ('drive', 77, 10)",
                    [],
                )
                .expect("add otherwise valid logical row");
        });
        // The forged metadata matches the old row-count arithmetic: without
        // the companion row it can still claim one tombstone-backed logical
        // row, so the car/settings invariant must reject it explicitly.
        delta.pack.row_count = 1;
        delta.pack.tables.push(MirrorTable::Tombstone);

        let error = store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
                Sha256Digest::of_bytes(b"missing-car-settings"),
                &[],
            )
            .expect_err("a car row must carry its companion settings row");
        assert!(matches!(error, StoreError::LineageCatalogConflict));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
    }

    #[test]
    fn import_delta_finalizer_rejects_row_semantics_the_writer_would_refuse() {
        fn partial_coordinate(connection: &Connection) {
            connection
                .execute(
                    "INSERT INTO drives(
                        id, car_id, start_date_ms, end_date_ms, start_latitude, start_longitude
                     ) VALUES (99, 10, 2_000, 3_000, 51.5, NULL)",
                    [],
                )
                .expect("insert partial coordinates");
        }

        fn non_finite_real(connection: &Connection) {
            connection
                .execute(
                    "UPDATE cars SET efficiency_wh_per_km = 1e999 WHERE id = 10",
                    [],
                )
                .expect("write an infinite REAL");
        }

        fn invalid_soc(connection: &Connection) {
            connection
                .execute(
                    "INSERT INTO positions(
                        id, drive_id, car_id, date_ms, latitude, longitude, battery_level
                     ) VALUES (100, NULL, 10, 2_000, 51.5, -0.1, 101)",
                    [],
                )
                .expect("insert out-of-range SOC");
        }

        fn negative_range(connection: &Connection) {
            connection
                .execute(
                    "INSERT INTO positions(
                        id, drive_id, car_id, date_ms, latitude, longitude, ideal_battery_range_km
                     ) VALUES (101, NULL, 10, 2_000, 51.5, -0.1, -1.0)",
                    [],
                )
                .expect("insert negative range");
        }

        fn nul_text(connection: &Connection) {
            connection
                .execute(
                    "UPDATE cars SET name = ?1 WHERE id = 10",
                    params!["safe\0name"],
                )
                .expect("write NUL-containing text");
        }

        fn overlong_text(connection: &Connection) {
            connection
                .execute(
                    "UPDATE cars SET model = ?1 WHERE id = 10",
                    params!["x".repeat(16 * 1024 + 1)],
                )
                .expect("write overlong text");
        }

        fn multiple_open_states(connection: &Connection) {
            connection
                .execute_batch(
                    "INSERT INTO states(id, car_id, state, start_date_ms, end_date_ms)
                        VALUES (200, 10, 'online', 2_000, NULL);
                     INSERT INTO states(id, car_id, state, start_date_ms, end_date_ms)
                        VALUES (201, 10, 'asleep', 3_000, NULL);",
                )
                .expect("insert incompatible open states");
        }

        let assert_rejected =
            |label: &str, added_rows: u64, table: Option<MirrorTable>, mutate: fn(&Connection)| {
                let temporary = tempfile::tempdir().expect("temporary store");
                let store = HubStore::initialize(temporary.path()).expect("store");
                let (vehicle, binding, base) = imported_v2_base(&store);
                let mut delta = imported_typed_delta(&store, &binding, &base);
                let row_count = delta
                    .pack
                    .row_count
                    .checked_add(added_rows)
                    .expect("fixture row count");
                rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
                    mutate(connection);
                    connection
                        .execute(
                            "UPDATE hub_pack_metadata SET value = ?1 WHERE key = 'row_count'",
                            params![row_count.to_string()],
                        )
                        .expect("update declared row count");
                });
                delta.pack.row_count = row_count;
                if let Some(table) = table {
                    delta.pack.tables.push(table);
                }

                let error = store
                    .finalize_import_delta_successor(
                        vehicle.vehicle_id,
                        &delta,
                        &import_delta_test_cursor_key(),
                        &import_delta_test_cursor(&binding, delta.to_sequence),
                        Sha256Digest::of_bytes(label.as_bytes()),
                        &[],
                    )
                    .expect_err(label);
                assert!(
                    matches!(error, StoreError::LineageCatalogConflict),
                    "{label}"
                );
                assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
            };

        assert_rejected(
            "partial optional coordinate pair",
            1,
            Some(MirrorTable::Drive),
            partial_coordinate,
        );
        assert_rejected("non-finite numeric value", 0, None, non_finite_real);
        assert_rejected(
            "out-of-range SOC",
            1,
            Some(MirrorTable::Position),
            invalid_soc,
        );
        assert_rejected(
            "negative battery range",
            1,
            Some(MirrorTable::Position),
            negative_range,
        );
        assert_rejected("NUL-containing text", 0, None, nul_text);
        assert_rejected("overlong text", 0, None, overlong_text);
        assert_rejected(
            "more than one open state",
            2,
            Some(MirrorTable::State),
            multiple_open_states,
        );
    }

    #[test]
    fn lineage_catalog_requires_verified_packs_and_is_restart_safe() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let base_snapshot_id = Uuid::new_v4();
        let make_pack = |snapshot_id: Uuid, ordinal: u32, sequence: SequenceRange, bytes: &[u8]| {
            let digest = Sha256Digest::of_bytes(bytes);
            TransportPack {
                pack_id: Uuid::new_v4(),
                snapshot_id,
                ordinal,
                schema: SchemaVersion { major: 1, minor: 0 },
                format: PackFormat::SqliteTransport,
                compression: PackCompression::Zstd,
                relative_path: TransportPack::canonical_relative_path(digest),
                sha256: digest,
                compressed_bytes: bytes.len() as u64,
                uncompressed_bytes: 100,
                row_count: 1,
                sequence,
                tables: vec![MirrorTable::Vehicle],
            }
        };
        let base_pack = make_pack(
            base_snapshot_id,
            0,
            SequenceRange {
                from_exclusive: 10,
                to_inclusive: 10,
            },
            b"base-pack",
        );
        let delta_pack = make_pack(
            base_snapshot_id,
            1,
            SequenceRange {
                from_exclusive: 10,
                to_inclusive: 11,
            },
            b"delta-pack",
        );
        let base_digest = Sha256Digest::of_bytes(b"base-chain");
        let chain_digest = canonical_delta_chain_digest(base_digest, delta_pack.sha256);
        let cursor = OpaqueCursor::issue(
            &CursorKey::from_bytes([7; 32]),
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: SchemaVersion { major: 1, minor: 0 },
                installation_id: store.installation_id().expect("installation"),
                account_id: Uuid::new_v4(),
                vehicle_id: vehicle.vehicle_id,
                generation: 1,
                sequence: 11,
            },
        )
        .expect("cursor");
        let lineage = LineageManifestV2 {
            protocol: LINEAGE_PROTOCOL_V2,
            capability: LineageCapability::ImmutableBaseOrderedDeltas,
            schema: SchemaVersion { major: 1, minor: 0 },
            installation_id: store.installation_id().expect("installation"),
            account_id: Uuid::new_v4(),
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
                chain_digest,
                pack_digest: delta_pack.sha256,
                pack: delta_pack.clone(),
            }],
            head_sequence: 11,
            head_digest: chain_digest,
            terminal_cursor: cursor,
        };
        assert!(matches!(
            store.commit_lineage_catalog(&lineage),
            Err(StoreError::LineagePackNotReady)
        ));
        let connection = store.open().expect("open after rejected publication");
        for table in ["sync_bases", "sync_deltas", "sync_heads", "sync_packs"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("rejected publication count");
            assert_eq!(count, 0, "rejected publication must not activate {table}");
        }
        drop(connection);
        let pack_dir = store.packs_dir().join("sha256");
        fs::create_dir_all(&pack_dir).expect("pack directory");
        for pack in [&base_pack, &delta_pack] {
            fs::write(
                pack_dir.join(format!("{}.sqlite.zst", pack.sha256)),
                if pack.pack_id == base_pack.pack_id {
                    b"base-pack".as_slice()
                } else {
                    b"delta-pack".as_slice()
                },
            )
            .expect("pack");
        }
        store
            .commit_lineage_catalog(&lineage)
            .expect("catalog commit");
        store
            .commit_lineage_catalog(&lineage)
            .expect("same commit is idempotent");
        let reopened = HubStore::initialize(temp.path()).expect("reopen");
        let count: i64 = reopened
            .open()
            .expect("open")
            .query_row("SELECT COUNT(*) FROM sync_deltas", [], |row| row.get(0))
            .expect("delta count");
        assert_eq!(count, 1);

        let mut conflict = lineage.clone();
        conflict.deltas[0].chain_digest = Sha256Digest::of_bytes(b"conflict-chain");
        conflict.head_digest = conflict.deltas[0].chain_digest;
        let head_before_conflict = reopened
            .v2_head(vehicle.vehicle_id)
            .expect("head before conflicting replay");
        assert!(matches!(
            reopened.commit_lineage_catalog(&conflict),
            Err(StoreError::Manifest(
                crate::protocol::ProtocolError::LineageChainMismatch
            ))
        ));
        assert_eq!(
            reopened
                .v2_head(vehicle.vehicle_id)
                .expect("head after conflicting replay"),
            head_before_conflict
        );
    }

    #[test]
    fn import_generation_staging_survives_active_state_and_promotes_once() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (source, vehicle) = test_registered_vehicle(&store);
        let active = crate::teslamate_projection::TeslaMateOpenSession {
            car_id: 10,
            state: Some(crate::teslamate_projection::TeslaMateState {
                id: 1,
                car_id: 10,
                state: "online".into(),
                start_date_ms: 1_000,
                end_date_ms: None,
            }),
            ..Default::default()
        };
        store
            .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &active, 1_000)
            .expect("active seed");

        let run = store
            .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 2_000)
            .expect("generation");
        store
            .stage_import_generation_session(run, &active)
            .expect("stage");
        let staged_count: i64 = store
            .open()
            .expect("open")
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("staged count");
        assert_eq!(staged_count, 1);
        assert_eq!(
            store
                .load_imported_open_session(source.source_id, vehicle.vehicle_id)
                .expect("active load"),
            Some(active.clone())
        );

        let reopened = HubStore::initialize(temp.path()).expect("restart cleanup");
        let cleaned_count: i64 = reopened
            .open()
            .expect("open after restart")
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .expect("cleaned count");
        assert_eq!(cleaned_count, 0);
        assert_eq!(
            reopened
                .load_imported_open_session(source.source_id, vehicle.vehicle_id)
                .expect("active survives restart"),
            Some(active.clone())
        );

        let successful = reopened
            .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 3_000)
            .expect("second generation");
        let mut promoted = active.clone();
        promoted.watermarks.positions.max_id = Some(12);
        reopened
            .stage_import_generation_session(successful, &promoted)
            .expect("stage second generation");
        reopened
            .promote_import_generation(successful, source.source_id, vehicle.vehicle_id, 10, 3_000)
            .expect("promote generation");
        assert_eq!(
            reopened
                .load_imported_open_session(source.source_id, vehicle.vehicle_id)
                .expect("promoted load"),
            Some(promoted)
        );
    }

    #[test]
    fn finalize_import_generation_promotes_fresh_vehicle_from_zero_cursor() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (source, vehicle) = test_registered_vehicle(&store);
        let session = crate::teslamate_projection::TeslaMateOpenSession {
            car_id: 10,
            state: Some(crate::teslamate_projection::TeslaMateState {
                id: 1,
                car_id: 10,
                state: "online".into(),
                start_date_ms: 1_000,
                end_date_ms: None,
            }),
            ..Default::default()
        };
        let run = store
            .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 1_000)
            .expect("generation");
        store
            .stage_import_generation_session(run, &session)
            .expect("stage");
        let mut manifest = test_manifest();
        manifest.vehicle_id = vehicle.vehicle_id;

        store
            .finalize_import_generation(
                run,
                source.source_id,
                vehicle.vehicle_id,
                10,
                1_000,
                &manifest,
                Sha256Digest::of_bytes(b"fresh import generation"),
                &[],
            )
            .expect("finalize fresh generation");

        assert_eq!(
            store
                .load_imported_open_session(source.source_id, vehicle.vehicle_id)
                .expect("load promoted session"),
            Some(session)
        );
    }

    #[test]
    fn import_generation_promotion_rejects_newer_live_cursor_without_reopening_state() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("test", "race"), 1_000)
            .expect("source");
        let vehicle = store
            .register_vehicle(&VehicleDescriptor::new(source.source_id, "race-car"), 1_000)
            .expect("vehicle");
        let active = crate::teslamate_projection::TeslaMateOpenSession {
            car_id: 10,
            state: Some(crate::teslamate_projection::TeslaMateState {
                id: 1,
                car_id: 10,
                state: "online".into(),
                start_date_ms: 1_000,
                end_date_ms: None,
            }),
            ..Default::default()
        };
        store
            .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &active, 1_000)
            .expect("active seed");
        let run = store
            .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 2_000)
            .expect("generation");
        store
            .stage_import_generation_session(run, &active)
            .expect("stage");
        store
            .open()
            .expect("open")
            .execute(
                "UPDATE vehicle_lifecycle_state
                 SET last_observation_id = 9, updated_at_ms = 9_000
                 WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
            )
            .expect("simulate live close");
        let error = store
            .promote_import_generation(run, source.source_id, vehicle.vehicle_id, 10, 2_000)
            .expect_err("newer live cursor must settle import");
        assert!(matches!(error, StoreError::ImportGenerationConflict));
        let state = store
            .load_lifecycle_state(vehicle.vehicle_id)
            .expect("state")
            .expect("live state remains");
        assert_eq!(state.last_observation_id, 9);
        assert_eq!(state.updated_at_ms, 9_000);
    }

    #[test]
    fn export_outbox_coalesces_retries_survives_restart_and_respects_v2_base() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("test", "outbox"), 1_000)
            .expect("source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "outbox-car"),
                1_000,
            )
            .expect("vehicle");
        let session = crate::teslamate_projection::TeslaMateOpenSession {
            car_id: 10,
            ..Default::default()
        };
        store
            .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &session, 1_000)
            .expect("dirty seed");
        let claim = store
            .claim_export_outbox(1_000)
            .expect("claim")
            .expect("outbox row");
        store
            .fail_export_outbox(&claim, "https://secret.invalid/token", 1_000)
            .expect("retry");
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("restart");
        let error: String = reopened
            .open()
            .expect("database")
            .query_row(
                "SELECT last_error FROM export_outbox WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("error");
        assert_eq!(error, "publication_failed");
        let second = reopened
            .claim_export_outbox(4_000)
            .expect("retry claim")
            .expect("retry row");
        assert!(second.attempts >= 2);
        reopened.complete_export_outbox(&second).expect("complete");
        assert!(
            reopened
                .claim_export_outbox(4_001)
                .expect("completed outbox query")
                .is_none(),
            "a completed revision must remain quiescent"
        );

        drop(reopened);
        let reopened = HubStore::initialize(temp.path()).expect("restart after completion");
        assert!(
            reopened
                .claim_export_outbox(5_000)
                .expect("completed outbox after restart")
                .is_none(),
            "a completed revision must not reappear after restart"
        );

        let base_id = Uuid::new_v4();
        reopened
            .open()
            .expect("database")
            .execute(
                "INSERT INTO sync_bases(vehicle_id, snapshot_id, base_sequence, base_digest, packs_json)
                 VALUES (?1, ?2, 1, ?3, ?4)",
                params![
                    vehicle.vehicle_id.to_string(),
                    base_id.to_string(),
                    "0".repeat(64),
                    b"[]".as_slice()
                ],
            )
            .expect("base");
        assert!(
            reopened
                .vehicle_has_v2_base(vehicle.vehicle_id)
                .expect("base check")
        );
    }

    #[test]
    fn export_outbox_completion_preserves_a_newer_revision_created_during_the_lease() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        mark_export_dirty_for_test(&store, vehicle.vehicle_id);

        let first = store
            .claim_export_outbox(1_000)
            .expect("first claim")
            .expect("first revision");
        assert_eq!(first.dirty_revision, 1);

        mark_export_dirty_for_test(&store, vehicle.vehicle_id);
        store
            .complete_export_outbox(&first)
            .expect("complete stale claim");

        let second = store
            .claim_export_outbox(1_001)
            .expect("newer claim")
            .expect("newer revision remains pending");
        assert_eq!(second.vehicle_id, vehicle.vehicle_id);
        assert_eq!(second.dirty_revision, 2);
        store
            .complete_export_outbox(&second)
            .expect("complete newer revision");
        assert!(
            store
                .claim_export_outbox(1_002)
                .expect("quiescent outbox")
                .is_none()
        );
    }

    #[test]
    fn export_outbox_completion_advances_fairly_across_vehicles() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("test", "outbox-fairness"), 1_000)
            .expect("source");
        let first_vehicle_id =
            Uuid::parse_str("00000000-0000-4000-8000-000000000001").expect("first test vehicle ID");
        let second_vehicle_id = Uuid::parse_str("00000000-0000-4000-8000-000000000002")
            .expect("second test vehicle ID");
        store
            .register_vehicle_with_id(
                &VehicleDescriptor::new(source.source_id, "outbox-car-1"),
                1_000,
                first_vehicle_id,
            )
            .expect("first vehicle");
        store
            .register_vehicle_with_id(
                &VehicleDescriptor::new(source.source_id, "outbox-car-2"),
                1_000,
                second_vehicle_id,
            )
            .expect("second vehicle");
        mark_export_dirty_for_test(&store, first_vehicle_id);
        mark_export_dirty_for_test(&store, second_vehicle_id);

        let first = store
            .claim_export_outbox(1_000)
            .expect("first claim")
            .expect("first vehicle pending");
        assert_eq!(first.vehicle_id, first_vehicle_id);
        mark_export_dirty_for_test(&store, first_vehicle_id);
        store
            .complete_export_outbox(&first)
            .expect("complete first vehicle");

        let second = store
            .claim_export_outbox(1_001)
            .expect("second claim")
            .expect("second vehicle pending");
        assert_eq!(second.vehicle_id, second_vehicle_id);
        store
            .complete_export_outbox(&second)
            .expect("complete second vehicle");

        let newer_first = store
            .claim_export_outbox(1_001)
            .expect("newer first claim")
            .expect("newer first revision remains pending");
        assert_eq!(newer_first.vehicle_id, first_vehicle_id);
        assert_eq!(newer_first.dirty_revision, 2);
        store
            .complete_export_outbox(&newer_first)
            .expect("complete newer first revision");
        assert!(
            store
                .claim_export_outbox(1_002)
                .expect("all vehicles complete")
                .is_none()
        );
    }

    #[test]
    fn export_outbox_restart_reclaims_an_expired_lease_and_fences_stale_completion() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        mark_export_dirty_for_test(&store, vehicle.vehicle_id);
        let abandoned = store
            .claim_export_outbox(1_000)
            .expect("initial claim")
            .expect("pending revision");
        drop(store);

        let reopened = HubStore::initialize(temp.path()).expect("restart with active lease");
        assert!(
            reopened
                .claim_export_outbox(abandoned.lease_until_ms - 1)
                .expect("leased query")
                .is_none(),
            "restart must preserve a live lease"
        );
        let reclaimed = reopened
            .claim_export_outbox(abandoned.lease_until_ms)
            .expect("expired lease query")
            .expect("expired revision is reclaimable");
        assert_eq!(reclaimed.dirty_revision, abandoned.dirty_revision);
        assert!(reclaimed.attempts > abandoned.attempts);

        reopened
            .complete_export_outbox(&abandoned)
            .expect("stale completion is harmless");
        assert!(
            reopened
                .claim_export_outbox(abandoned.lease_until_ms)
                .expect("new lease remains fenced")
                .is_none(),
            "a stale publisher must not consume or release the new lease"
        );
        reopened
            .complete_export_outbox(&reclaimed)
            .expect("active completion");
        assert!(
            reopened
                .claim_export_outbox(reclaimed.lease_until_ms)
                .expect("completed revision")
                .is_none()
        );
    }

    #[test]
    fn stream_session_terminal_completion_is_explicit_and_idempotence_fenced() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let correlation_id = Uuid::new_v4();
        let session = store
            .begin_stream_session(correlation_id, 123)
            .expect("begin stream session");
        store
            .complete_stream_session_terminal(
                session,
                StreamSessionTerminalOutcome::CancelledBeforeSubscription,
            )
            .expect("terminalize normal pre-subscription cancellation");
        assert!(matches!(
            store.complete_stream_session_terminal(session, StreamSessionTerminalOutcome::Failed,),
            Err(StoreError::StreamSessionReceiptNotStarted)
        ));
        let connection = store.open().expect("stream receipt catalogue");
        let (outcome, unsubscribe, completed): (String, Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT outcome, unsubscribe_receipt_id, completed_at_ms
                   FROM stream_session_receipts WHERE id = ?1",
                params![session.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("terminal receipt");
        assert_eq!(outcome, "cancelled_before_subscription");
        assert_eq!(unsubscribe, None);
        assert!(completed.is_some());
    }

    #[test]
    fn upgrades_v39_with_durable_live_delta_compaction_provenance() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("current store");
        let connection = store.open().expect("current catalogue");
        connection
            .execute_batch(
                "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 DROP TABLE sync_retired_lineage_packs;
                 DROP TABLE sync_retired_lineages;
                 DROP TABLE sync_live_delta_spans;
                 DROP INDEX sync_mutations_compaction_latest;
                 PRAGMA user_version = 39;",
            )
            .expect("recreate historical v39 boundary");
        drop(connection);

        let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v39 store");
        let connection = upgraded.open().expect("upgraded catalogue");
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
        for object in [
            "sync_live_delta_spans",
            "sync_live_delta_spans_revision_range",
            "sync_mutations_compaction_latest",
        ] {
            let found: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE name = ?1 AND type IN ('table', 'index')",
                    params![object],
                    |row| row.get(0),
                )
                .expect("compaction schema object query");
            assert_eq!(found, 1, "missing migrated object {object}");
        }
    }

    #[test]
    fn upgrades_v41_with_bounded_prior_lineage_pack_authorization() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("current store");
        let connection = store.open().expect("current catalogue");
        connection
            .execute_batch(
                "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 DROP TABLE sync_retired_lineage_packs;
                 DROP TABLE sync_retired_lineages;
                 PRAGMA user_version = 41;",
            )
            .expect("recreate historical v41 boundary");
        drop(connection);

        let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v41 store");
        let connection = upgraded.open().expect("upgraded catalogue");
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
        for object in [
            "sync_retired_lineages",
            "sync_retired_lineage_packs",
            "sync_retired_lineage_packs_authorization",
            "sync_retired_lineages_expiry",
        ] {
            let found: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE name = ?1 AND type IN ('table', 'index')",
                    params![object],
                    |row| row.get(0),
                )
                .expect("retired-lineage schema object query");
            assert_eq!(found, 1, "missing migrated object {object}");
        }
    }

    #[test]
    fn upgrades_v40_stream_receipts_without_reclassifying_crash_evidence() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("current store");
        let correlation_id = Uuid::new_v4();
        let connection = store.open().expect("current catalogue");
        connection
            .execute_batch(
                "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 DROP TABLE sync_retired_lineage_packs;
                 DROP TABLE sync_retired_lineages;
                 DROP INDEX stream_session_receipts_proof;
                 DROP INDEX stream_session_receipts_retention;
                 DROP TABLE stream_session_receipts;
                 CREATE TABLE stream_session_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    vehicle_tesla_id INTEGER NOT NULL CHECK(vehicle_tesla_id > 0),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    outcome TEXT NOT NULL CHECK(outcome IN ('started', 'orderly_shutdown')),
                    unsubscribe_receipt_id INTEGER,
                    CHECK((outcome = 'started' AND completed_at_ms IS NULL
                           AND duration_ms IS NULL AND unsubscribe_receipt_id IS NULL)
                          OR (outcome = 'orderly_shutdown' AND completed_at_ms IS NOT NULL
                              AND duration_ms IS NOT NULL
                              AND completed_at_ms >= started_at_ms AND duration_ms >= 0
                              AND unsubscribe_receipt_id IS NOT NULL))
                 ) STRICT;
                 CREATE INDEX stream_session_receipts_proof
                    ON stream_session_receipts(correlation_id, outcome, id);
                 CREATE INDEX stream_session_receipts_retention
                    ON stream_session_receipts(outcome, completed_at_ms, id);
                 PRAGMA user_version = 40;",
            )
            .expect("recreate v40 stream receipt schema");
        connection
            .execute(
                "INSERT INTO stream_session_receipts(
                    id, correlation_id, vehicle_tesla_id, started_at_ms, outcome
                 ) VALUES (1, ?1, 123, 1000, 'started')",
                params![correlation_id.to_string()],
            )
            .expect("historical unresolved receipt");
        connection
            .execute(
                "INSERT INTO stream_session_receipts(
                    id, correlation_id, vehicle_tesla_id, started_at_ms,
                    completed_at_ms, duration_ms, outcome, unsubscribe_receipt_id
                 ) VALUES (2, ?1, 123, 1000, 1100, 100, 'orderly_shutdown', 77)",
                params![correlation_id.to_string()],
            )
            .expect("historical orderly receipt");
        drop(connection);

        let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v40 store");
        let connection = upgraded.open().expect("upgraded catalogue");
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
        let outcomes = connection
            .prepare("SELECT outcome FROM stream_session_receipts ORDER BY id")
            .expect("receipt query")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("receipt rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("receipt outcomes");
        assert_eq!(outcomes, ["started", "orderly_shutdown"]);
        drop(connection);
        upgraded
            .complete_stream_session_terminal(
                StreamSessionReceiptId(1),
                StreamSessionTerminalOutcome::TransportEnded,
            )
            .expect("resolve retained crash receipt explicitly");
    }

    #[test]
    fn upgrades_a_v2_database_without_losing_existing_tables() {
        let temp = tempfile::tempdir().expect("temp directory");
        let database_path = temp.path().join("hub.sqlite");
        let legacy_source_id = Uuid::new_v4();
        let connection = Connection::open(&database_path).expect("open v2 database");
        connection
            .execute_batch(
                "
                CREATE TABLE sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE sync_manifests (
                    snapshot_id TEXT PRIMARY KEY NOT NULL,
                    vehicle_id TEXT NOT NULL,
                    head_sequence INTEGER NOT NULL CHECK (head_sequence >= 0),
                    manifest_json BLOB NOT NULL
                ) STRICT;
                CREATE TABLE sync_packs (
                    sha256 TEXT PRIMARY KEY NOT NULL,
                    snapshot_id TEXT NOT NULL REFERENCES sync_manifests(snapshot_id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK (compressed_bytes > 0),
                    uncompressed_bytes INTEGER NOT NULL CHECK (uncompressed_bytes >= 100),
                    UNIQUE(snapshot_id, ordinal)
                ) STRICT;
                PRAGMA user_version = 2;
                ",
            )
            .expect("make v2 schema");
        connection
            .execute(
                "INSERT INTO sources (source_id, source_kind, generation, created_at_ms) \
                 VALUES (?1, 'legacy', 1, 1)",
                params![legacy_source_id.to_string()],
            )
            .expect("legacy source");
        drop(connection);
        // Schema migration is exercised only after the caller has established
        // the split-UID catalogue contract. A legacy 0644 catalogue remains
        // an explicit fail-closed admission case.
        fs::set_permissions(
            &database_path,
            fs::Permissions::from_mode(SHARED_SQLITE_FILE_MODE),
        )
        .expect("protect v2 catalogue for split identities");

        let store = HubStore::initialize(temp.path()).expect("migrate v2 store");
        let migrated = store.open().expect("open migrated store");
        assert_eq!(
            schema_version(&migrated).expect("schema version"),
            SCHEMA_VERSION
        );
        let legacy_count: i64 = migrated
            .query_row("SELECT COUNT(*) FROM sources", [], |row| row.get(0))
            .expect("legacy source preserved");
        assert_eq!(legacy_count, 1);
        let raw_table_count: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'raw_observations'",
                [],
                |row| row.get(0),
            )
            .expect("raw table exists");
        assert_eq!(raw_table_count, 1);
    }

    #[test]
    fn upgrades_a_v1_database_through_v2_and_v3() {
        let temp = tempfile::tempdir().expect("temp directory");
        let database_path = temp.path().join("hub.sqlite");
        let connection = Connection::open(&database_path).expect("open v1 database");
        connection
            .execute_batch(
                "
                CREATE TABLE hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                CREATE TABLE sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE sync_ledger (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    sequence INTEGER NOT NULL CHECK (sequence >= 1),
                    entity_kind TEXT NOT NULL,
                    entity_key TEXT NOT NULL,
                    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
                    committed_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (source_id, sequence, entity_kind, entity_key)
                ) STRICT;
                PRAGMA user_version = 1;
                ",
            )
            .expect("make v1 schema");
        drop(connection);
        // See the v2 migration test: old catalogue permissions are not
        // upgraded in place by a service process.
        fs::set_permissions(
            &database_path,
            fs::Permissions::from_mode(SHARED_SQLITE_FILE_MODE),
        )
        .expect("protect v1 catalogue for split identities");

        let store = HubStore::initialize(temp.path()).expect("migrate v1 store");
        let migrated = store.open().expect("open migrated store");
        assert_eq!(
            schema_version(&migrated).expect("schema version"),
            SCHEMA_VERSION
        );
        for table in [
            "sync_manifests",
            "sync_packs",
            "source_identities",
            "vehicles",
            "raw_observations",
        ] {
            let found: i64 = migrated
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("migrated table query");
            assert_eq!(found, 1, "missing table {table}");
        }
    }

    #[test]
    fn upgrades_v36_catalogue_to_a_separate_digest_only_projection_state() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("current store");
        let connection = store.open().expect("current catalogue");
        connection
            .execute_batch(
                "
                DROP TABLE legacy_refresh_input_fences;
                DROP INDEX legacy_refresh_receipt_output_generation;
                DROP TABLE legacy_refresh_receipt_bindings;
                DROP TABLE supervised_collector_lease;
                DROP TABLE sync_retired_lineage_packs;
                DROP TABLE sync_retired_lineages;
                DROP TABLE teslamate_import_projection_state_rows;
                DROP TABLE teslamate_import_projection_state_heads;
                PRAGMA user_version = 36;
                ",
            )
            .expect("recreate historical v36 boundary");
        drop(connection);

        let upgraded = HubStore::initialize(temporary.path()).expect("upgrade from v36");
        let connection = upgraded.open().expect("upgraded catalogue");
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
        for table in [
            "teslamate_import_projection_state_heads",
            "teslamate_import_projection_state_rows",
        ] {
            let found: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("state table exists");
            assert_eq!(found, 1, "missing {table}");
        }
        let payload_column_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('teslamate_import_projection_state_rows')
                  WHERE name = 'payload_json'",
                [],
                |row| row.get(0),
            )
            .expect("state schema");
        assert_eq!(
            payload_column_count, 0,
            "Hub state must retain digests only"
        );
    }

    #[test]
    fn upgrades_v38_teslamate_projection_catalogues_for_update_history() {
        let temporary = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("current store");
        let connection = store.open().expect("current catalogue");
        connection
            .execute_batch(
                "
                DROP TABLE legacy_refresh_input_fences;
                DROP INDEX legacy_refresh_receipt_output_generation;
                DROP TABLE legacy_refresh_receipt_bindings;
                DROP TABLE supervised_collector_lease;
                DROP TABLE sync_retired_lineage_packs;
                DROP TABLE sync_retired_lineages;
                DROP TABLE teslamate_import_projection_rows;
                DROP TABLE teslamate_import_projection_state_rows;
                CREATE TABLE teslamate_import_projection_rows (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state'
                    )),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    PRIMARY KEY(vehicle_id, entity, entity_id)
                ) STRICT;
                CREATE TABLE teslamate_import_projection_state_rows (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_state_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'car', 'drive', 'position', 'charge', 'charge_sample', 'state'
                    )),
                    entity_ordinal INTEGER NOT NULL CHECK(entity_ordinal BETWEEN 0 AND 5),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    projection_sha256 BLOB NOT NULL CHECK(length(projection_sha256) = 32),
                    CHECK(
                        (entity = 'car' AND entity_ordinal = 0) OR
                        (entity = 'drive' AND entity_ordinal = 1) OR
                        (entity = 'position' AND entity_ordinal = 2) OR
                        (entity = 'charge' AND entity_ordinal = 3) OR
                        (entity = 'charge_sample' AND entity_ordinal = 4) OR
                        (entity = 'state' AND entity_ordinal = 5)
                    ),
                    PRIMARY KEY(vehicle_id, entity_ordinal, entity_id),
                    UNIQUE(vehicle_id, entity, entity_id)
                ) STRICT, WITHOUT ROWID;
                PRAGMA user_version = 38;
                ",
            )
            .expect("recreate historical v38 boundary");
        drop(connection);

        let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v38 store");
        let connection = upgraded.open().expect("upgraded catalogue");
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
        for table in [
            "teslamate_import_projection_rows",
            "teslamate_import_projection_state_rows",
        ] {
            let sql: String = connection
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("upgraded table SQL");
            assert!(sql.contains("'update'"), "{table} accepts update rows");
        }
        let state_sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'teslamate_import_projection_state_rows'",
                [],
                |row| row.get(0),
            )
            .expect("upgraded state table SQL");
        assert!(
            state_sql.contains("BETWEEN 0 AND 6"),
            "durable update state has ordinal 6"
        );
    }

    fn mark_export_dirty_for_test(store: &HubStore, vehicle_id: Uuid) {
        let mut connection = store.open().expect("database");
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("outbox transaction");
        mark_export_dirty_in_transaction(&transaction, vehicle_id).expect("mark export dirty");
        transaction.commit().expect("commit outbox mutation");
    }

    fn test_registered_vehicle(store: &HubStore) -> (SourceRecord, VehicleRecord) {
        let source = store
            .register_source(
                &SourceDescriptor::new("tesla_owner_api", "account-test"),
                1_000,
            )
            .expect("source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "vehicle-test"),
                1_001,
            )
            .expect("vehicle");
        (source, vehicle)
    }

    fn test_manifest() -> SyncManifest {
        let installation_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let vehicle_id = Uuid::new_v4();
        let digest = Sha256Digest::of_bytes(&[7_u8; 100]);
        let cursor = OpaqueCursor::issue(
            &CursorKey::from_bytes([7; 32]),
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: SchemaVersion { major: 1, minor: 0 },
                installation_id,
                account_id,
                vehicle_id,
                generation: 1,
                sequence: 9,
            },
        )
        .expect("cursor");
        let pack = TransportPack {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            schema: SchemaVersion { major: 1, minor: 0 },
            format: PackFormat::SqliteTransport,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(digest),
            sha256: digest,
            compressed_bytes: 100,
            uncompressed_bytes: 100,
            row_count: 1,
            sequence: SequenceRange {
                from_exclusive: 9,
                to_inclusive: 9,
            },
            tables: vec![MirrorTable::Vehicle],
        };
        SyncManifest {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: SchemaVersion { major: 1, minor: 0 },
            installation_id,
            account_id,
            vehicle_id,
            generation: 1,
            snapshot_id: pack.snapshot_id,
            mode: TransferMode::FullSnapshot,
            base_sequence: 9,
            head_sequence: 9,
            chunk_count: 1,
            total_compressed_bytes: pack.compressed_bytes,
            total_uncompressed_bytes: pack.uncompressed_bytes,
            total_rows: pack.row_count,
            chunks: vec![pack],
            terminal_cursor: cursor,
        }
    }

    fn schema_22_test_manifest() -> SyncManifest {
        let mut manifest = test_manifest();
        manifest.schema = HUB_PROJECTION_SCHEMA_V3;
        manifest.chunks[0].schema = HUB_PROJECTION_SCHEMA_V3;
        manifest.chunks[0].format = PackFormat::HubProjectionSqlite;
        manifest.chunks[0].tables = vec![MirrorTable::Car];
        manifest.terminal_cursor = OpaqueCursor::issue(
            &CursorKey::from_bytes([7; 32]),
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: HUB_PROJECTION_SCHEMA_V3,
                installation_id: manifest.installation_id,
                account_id: manifest.account_id,
                vehicle_id: manifest.vehicle_id,
                generation: manifest.generation,
                sequence: manifest.head_sequence,
            },
        )
        .expect("schema 2.2 cursor");
        manifest
            .validate()
            .expect("schema 2.2 remains protocol-valid");
        manifest
    }

    #[test]
    fn imported_home_work_geofences_match_live_endpoints_after_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let imported = vec![
            crate::teslamate_projection::TeslaMateGeofence {
                id: 10,
                name: "Home".into(),
                latitude: Some(51.0000),
                longitude: Some(-0.1000),
                radius_m: Some(150.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
                cost_per_unit: Some(0.30),
                session_fee: Some(2.0),
            },
            crate::teslamate_projection::TeslaMateGeofence {
                id: 11,
                name: "Work".into(),
                latitude: Some(51.0010),
                longitude: Some(-0.1010),
                radius_m: Some(150.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerMinute),
                cost_per_unit: Some(0.10),
                session_fee: Some(1.0),
            },
        ];
        assert_eq!(
            store
                .upsert_geofences(vehicle.vehicle_id, &imported)
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .upsert_geofences(vehicle.vehicle_id, &imported)
                .unwrap(),
            0
        );

        let session = crate::lifecycle::OpenSessionState::new();
        let encoded = session.encode().expect("encode session");
        let drive = crate::hub_pack::ProjectionDrive {
            id: 1,
            car_id: 1,
            optimized_at_ms: None,
            start_date_ms: 1_000,
            end_date_ms: 2_000,
            distance_km: Some(1.0),
            duration_min: Some(1),
            efficiency: None,
            outside_temp_avg: None,
            inside_temp_avg: None,
            speed_max: Some(20),
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_address: None,
            end_address: None,
            start_geofence: None,
            end_geofence: None,
            start_latitude: Some(51.0001),
            start_longitude: Some(-0.1001),
            end_latitude: Some(51.0011),
            end_longitude: Some(-0.1011),
            start_soc: Some(80),
            end_soc: Some(79),
            start_rated_range_km: None,
            end_rated_range_km: None,
            ascent: None,
            descent: None,
        };
        let charge = crate::hub_pack::ProjectionCharge {
            id: 2,
            car_id: 1,
            start_date_ms: 3_000,
            end_date_ms: Some(4_000),
            charge_energy_added: Some(1.0),
            charge_energy_used_kwh: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            cost: None,
            fast_charger_type: None,
            billing_type: None,
            cost_per_unit: None,
            session_fee: None,
            start_latitude: None,
            start_longitude: None,
            start_battery_level: Some(50),
            end_battery_level: Some(51),
            duration_min: Some(1),
            address: None,
            location_name: None,
            geofence: None,
            is_dc: Some(false),
            charge_rate_km_per_hour: None,
            max_charger_power_kw: Some(7.0),
            outside_temp_avg: None,
            start_rated_range_km: None,
            end_rated_range_km: None,
        };
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 1,
                quarantined: false,
                updated_at_ms: 4_000,
                delta: &crate::lifecycle::LifecycleDelta {
                    drives: vec![drive],
                    charges: vec![charge],
                    charge_start_coordinates: vec![(2, 51.0001, -0.1001)],
                    ..Default::default()
                },
            })
            .expect("live endpoint materialisation");

        let reopened = HubStore::initialize(temp.path()).expect("restart store");
        let connection = reopened.open().expect("open queue");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM address_enrichment_jobs WHERE vehicle_id = ?1",
                    params![vehicle.vehicle_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );
        drop(connection);
        let first_job = reopened
            .claim_address_enrichment_job(5_000)
            .unwrap()
            .expect("pending start job");
        assert_eq!(first_job.target_type, "charge");
        assert_eq!(first_job.field, "address");
        reopened
            .complete_address_enrichment(&first_job, Some("Delayed response address"), 6_000)
            .unwrap();
        let retry_job = reopened
            .claim_address_enrichment_job(5_000)
            .unwrap()
            .expect("pending end job");
        reopened
            .retry_address_enrichment(&retry_job, "temporary transport", 5_000)
            .unwrap();
        let remaining_job = reopened
            .claim_address_enrichment_job(5_000)
            .unwrap()
            .expect("remaining endpoint job");
        reopened
            .complete_address_enrichment(&remaining_job, None, 6_000)
            .unwrap();
        drop(reopened);
        let resumed = HubStore::initialize(temp.path()).expect("resume store");
        assert!(
            resumed
                .claim_address_enrichment_job(14_999)
                .unwrap()
                .is_none()
        );
        assert!(
            resumed
                .claim_address_enrichment_job(15_000)
                .unwrap()
                .is_some()
        );
        let history = resumed
            .materialised_history(vehicle.vehicle_id)
            .expect("history");
        assert_eq!(
            history.charges[0].address.as_deref(),
            Some("Delayed response address")
        );
        let stored_charge = resumed
            .open()
            .unwrap()
            .query_row(
                "SELECT charge_json FROM materialised_charges WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert!(!stored_charge.contains("osm_type"));
        assert_eq!(history.drives[0].start_geofence.as_deref(), Some("Home"));
        assert_eq!(history.drives[0].end_geofence.as_deref(), Some("Work"));
        assert_eq!(history.charges[0].geofence.as_deref(), Some("Home"));
        assert_eq!(
            history.charges[0].billing_type,
            Some(crate::hub_pack::GeofenceBillingType::PerKwh)
        );
        assert_eq!(history.charges[0].cost_per_unit, Some(0.30));
        assert_eq!(history.charges[0].session_fee, Some(2.0));
        assert_eq!(history.charges[0].cost, Some(2.3));
    }

    #[test]
    fn lifecycle_state_intervals_upsert_and_survive_store_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let state = crate::lifecycle::OpenSessionState::new();
        let encoded = state.encode().expect("encode session");
        let first = crate::hub_pack::ProjectionState {
            id: 1,
            car_id: 1,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: None,
        };
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 1,
                quarantined: false,
                updated_at_ms: 1_000,
                delta: &crate::lifecycle::LifecycleDelta {
                    states: vec![first.clone()],
                    ..Default::default()
                },
            })
            .expect("write open state");

        let closed = crate::hub_pack::ProjectionState {
            end_date_ms: Some(2_000),
            ..first
        };
        let next = crate::hub_pack::ProjectionState {
            id: 2,
            car_id: 1,
            state: "asleep".into(),
            start_date_ms: 2_000,
            end_date_ms: None,
        };
        let update = crate::hub_pack::ProjectionUpdate {
            id: 1,
            car_id: 1,
            start_date_ms: 1_500,
            end_date_ms: 2_500,
            version: "2026.2".into(),
        };
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 2,
                quarantined: false,
                updated_at_ms: 2_000,
                delta: &crate::lifecycle::LifecycleDelta {
                    states: vec![closed, next],
                    updates: vec![update],
                    ..Default::default()
                },
            })
            .expect("close and open state");

        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("restart store");
        let history = reopened
            .materialised_history(vehicle.vehicle_id)
            .expect("state history");
        assert_eq!(history.states.len(), 2);
        assert_eq!(history.states[0].state, "online");
        assert_eq!(history.states[0].end_date_ms, Some(2_000));
        assert_eq!(history.states[1].state, "asleep");
        assert_eq!(history.states[1].end_date_ms, None);
        assert_eq!(history.updates.len(), 1);
        assert_eq!(history.updates[0].version, "2026.2");
    }

    #[test]
    fn lifecycle_car_metadata_is_durable_and_preserves_imported_efficiency() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let imported = crate::hub_pack::ProjectionCar {
            id: 1,
            name: "Imported car".into(),
            model: "3".into(),
            vin: Some("5YJIMPORTED123456".into()),
            source_eid: Some(88),
            source_vid: Some(99),
            trim_badging: Some("74D".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            firmware_version: Some("2026.0".into()),
            efficiency_wh_per_km: Some(145.0),
            settings: Default::default(),
        };
        store
            .open()
            .expect("open")
            .execute(
                "INSERT INTO materialised_cars(vehicle_id, car_id, car_json) VALUES (?1, ?2, ?3)",
                params![
                    vehicle.vehicle_id.to_string(),
                    imported.id,
                    serde_json::to_string(&imported).expect("serialize imported car")
                ],
            )
            .expect("seed imported car");

        let mut state = crate::lifecycle::OpenSessionState::new();
        state.last_observation_id = 1;
        state.car_metadata = Some(crate::hub_pack::ProjectionCarPatch {
            name: Some("Road car".into()),
            model: Some("3".into()),
            vin: Some("5YJNEWVIN1234567".into()),
            trim_badging: Some("74D".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            firmware_version: Some("2026.1".into()),
        });
        let encoded = state.encode().expect("encode metadata state");
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 1,
                quarantined: false,
                updated_at_ms: 2_000,
                delta: &crate::lifecycle::LifecycleDelta::default(),
            })
            .expect("commit metadata");

        let car_mutations_before: i64 = store
            .open()
            .expect("mutation database")
            .query_row(
                "SELECT COUNT(*) FROM sync_mutations
                 WHERE vehicle_id = ?1 AND entity = 'car'",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("car mutation count");
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 1,
                quarantined: false,
                updated_at_ms: 2_001,
                delta: &crate::lifecycle::LifecycleDelta::default(),
            })
            .expect("repeat identical metadata");
        let car_mutations_after: i64 = store
            .open()
            .expect("repeat mutation database")
            .query_row(
                "SELECT COUNT(*) FROM sync_mutations
                 WHERE vehicle_id = ?1 AND entity = 'car'",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("repeat car mutation count");
        assert_eq!(
            car_mutations_after, car_mutations_before,
            "identical lifecycle metadata must not advance the sync journal"
        );

        let history = store
            .materialised_history(vehicle.vehicle_id)
            .expect("load metadata");
        let car = history.car.expect("materialised car");
        assert_eq!(car.name, "Road car");
        assert_eq!(car.model, "3");
        assert_eq!(car.vin.as_deref(), Some("5YJNEWVIN1234567"));
        assert_eq!(car.trim_badging.as_deref(), Some("74D"));
        assert_eq!(car.marketing_name.as_deref(), Some("LR AWD"));
        assert_eq!(car.exterior_color.as_deref(), Some("Pearl White"));
        assert_eq!(car.wheel_type.as_deref(), Some("Apollo"));
        assert_eq!(car.spoiler_type.as_deref(), Some("None"));
        assert_eq!(car.firmware_version.as_deref(), Some("2026.1"));
        assert_eq!(car.efficiency_wh_per_km, Some(145.0));

        state.last_observation_id = 2;
        state.car_metadata = Some(crate::hub_pack::ProjectionCarPatch {
            firmware_version: Some("2026.2".into()),
            ..Default::default()
        });
        let encoded = state.encode().expect("encode partial metadata state");
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 2,
                quarantined: false,
                updated_at_ms: 3_000,
                delta: &crate::lifecycle::LifecycleDelta::default(),
            })
            .expect("commit partial metadata");
        let car = store
            .materialised_history(vehicle.vehicle_id)
            .expect("reload metadata")
            .car
            .expect("materialised car after partial update");
        assert_eq!(car.name, "Road car");
        assert_eq!(car.vin.as_deref(), Some("5YJNEWVIN1234567"));
        assert_eq!(car.firmware_version.as_deref(), Some("2026.2"));
        assert_eq!(car.efficiency_wh_per_km, Some(145.0));
    }

    #[test]
    fn repair_preserves_quarantined_sessions_and_removes_orphaned_packs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);

        let connection = store.open().expect("open");
        connection
            .execute(
                "INSERT INTO vehicle_lifecycle_state(
                    vehicle_id, car_id, last_observation_id, open_session_json, quarantined, updated_at_ms
                 ) VALUES (?1, 1, 1, x'7b7d', 1, 1000)",
                params![vehicle.vehicle_id.to_string()],
            )
            .expect("insert quarantined");
        drop(connection);

        let orphaned_pack = store
            .packs_dir()
            .join("0000000000000000000000000000000000000000000000000000000000000000.sqlite.zst");
        std::fs::write(&orphaned_pack, b"orphaned bytes").expect("write pack");

        let report = store.repair().expect("repair");
        assert_eq!(report.status, "ok");
        assert_eq!(report.sqlite_integrity, "ok");
        assert!(matches!(
            store.readiness_check(),
            Err(StoreError::QuarantinedLifecycle(1))
        ));
        assert_eq!(report.quarantined_sessions_preserved, 1);
        assert_eq!(report.orphaned_packs_removed, 1);
        assert_eq!(report.freed_bytes, 14);
        assert!(!orphaned_pack.exists());

        let connection = store.open().expect("open");
        let quarantined: i64 = connection
            .query_row(
                "SELECT quarantined FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("query quarantined");
        assert_eq!(quarantined, 1);
    }

    #[test]
    fn car_settings_are_idempotent_and_survive_reopen() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let settings = ProjectionCarSettings {
            enabled: false,
            use_streaming_api: false,
            suspend_after_idle_min: 4,
            suspend_min: 9,
            suspend_min_resolved: true,
            req_not_unlocked: true,
            free_supercharging: true,
            lfp_battery: true,
        };
        store
            .upsert_car_settings(vehicle.vehicle_id, 1, &settings)
            .expect("first settings write");
        store
            .upsert_car_settings(vehicle.vehicle_id, 1, &settings)
            .expect("idempotent settings write");
        assert_eq!(
            store.load_car_settings(vehicle.vehicle_id).unwrap(),
            settings
        );
        let settings_mutations: i64 = store
            .open()
            .expect("mutation database")
            .query_row(
                "SELECT COUNT(*) FROM sync_mutations
                 WHERE vehicle_id = ?1 AND entity = 'car_setting'",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("settings mutation count");
        assert_eq!(
            settings_mutations, 1,
            "an identical settings write must not advance the sync journal"
        );
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("reopen");
        assert_eq!(
            reopened.load_car_settings(vehicle.vehicle_id).unwrap(),
            settings
        );
    }

    #[test]
    fn unresolved_live_default_resolves_once_and_explicit_value_wins() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let live = ProjectionCarSettings::new_live();
        store
            .upsert_car_settings(vehicle.vehicle_id, 1, &live)
            .expect("live settings");
        assert!(
            store
                .resolve_car_suspend_min(vehicle.vehicle_id, Some("3"), Some("74D"), None)
                .expect("resolve model 3")
        );
        let resolved = store.load_car_settings(vehicle.vehicle_id).unwrap();
        assert_eq!(resolved.suspend_min, 12);
        assert!(resolved.suspend_min_resolved);
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("restart");
        assert!(
            !reopened
                .resolve_car_suspend_min(vehicle.vehicle_id, Some("Y"), None, None)
                .expect("metadata must not rewrite")
        );
        assert_eq!(
            reopened
                .load_car_settings(vehicle.vehicle_id)
                .unwrap()
                .suspend_min,
            12
        );

        let explicit_source = reopened
            .register_source(
                &SourceDescriptor::new("tesla_owner_api", "explicit-test"),
                2_000,
            )
            .expect("explicit source");
        let explicit_vehicle = reopened
            .register_vehicle(
                &VehicleDescriptor::new(explicit_source.source_id, "explicit-vehicle"),
                2_001,
            )
            .expect("explicit vehicle");
        let explicit = ProjectionCarSettings {
            suspend_min: 7,
            suspend_min_resolved: true,
            ..ProjectionCarSettings::default()
        };
        reopened
            .upsert_car_settings(explicit_vehicle.vehicle_id, 1, &explicit)
            .expect("explicit settings");
        assert_eq!(
            reopened
                .load_car_settings(explicit_vehicle.vehicle_id)
                .unwrap()
                .suspend_min,
            7
        );
        assert!(
            !reopened
                .resolve_car_suspend_min(explicit_vehicle.vehicle_id, Some("3"), None, None)
                .expect("explicit value must stay authoritative")
        );
    }

    #[test]
    fn stream_watermark_is_strictly_increasing_and_survives_reopen() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);

        assert!(
            store
                .accept_stream_timestamp(vehicle.vehicle_id, 1_000)
                .expect("first watermark")
        );
        assert!(
            !store
                .accept_stream_timestamp(vehicle.vehicle_id, 1_000)
                .expect("duplicate watermark")
        );
        assert!(
            !store
                .accept_stream_timestamp(vehicle.vehicle_id, 999)
                .expect("older watermark")
        );
        assert!(
            store
                .accept_stream_timestamp(vehicle.vehicle_id, 1_001)
                .expect("newer watermark")
        );

        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("reopen");
        assert!(
            !reopened
                .accept_stream_timestamp(vehicle.vehicle_id, 1_000)
                .expect("old frame after restart")
        );
        assert!(
            reopened
                .accept_stream_timestamp(vehicle.vehicle_id, 1_002)
                .expect("new frame after restart")
        );
    }

    #[test]
    fn verify_no_wake_applies_the_captured_receipt_watermark() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let correlation_id = Uuid::new_v4();

        let old = store
            .begin_outbound_request(&OutboundRequestStart {
                correlation_id,
                vehicle_tesla_id: Some(505),
                transport: OutboundRequestTransport::OwnerApi,
                operation: OutboundRequestOperation::VehicleProbe,
                safety_class: OutboundRequestSafetyClass::DirectWakeCommand,
                precondition: OutboundRequestPrecondition::NotRequired,
            })
            .expect("old receipt");
        store
            .complete_outbound_request(
                old,
                &OutboundRequestCompletion {
                    outcome: OutboundRequestOutcome::Success,
                    http_status: None,
                    retry_after_seconds: None,
                },
            )
            .expect("complete old receipt");
        let watermark = store
            .outbound_request_watermark()
            .expect("watermark")
            .receipt_id;

        let current = store
            .begin_outbound_request(&OutboundRequestStart {
                correlation_id,
                vehicle_tesla_id: Some(505),
                transport: OutboundRequestTransport::OwnerApi,
                operation: OutboundRequestOperation::Products,
                safety_class: OutboundRequestSafetyClass::NonWakeEndpoint,
                precondition: OutboundRequestPrecondition::NotRequired,
            })
            .expect("current receipt");
        store
            .complete_outbound_request(
                current,
                &OutboundRequestCompletion {
                    outcome: OutboundRequestOutcome::Success,
                    http_status: None,
                    retry_after_seconds: None,
                },
            )
            .expect("complete current receipt");

        let verification = store
            .verify_no_wake_after(watermark, correlation_id, None)
            .expect("verify watermark window");
        assert_eq!(verification.matching_receipts, 1);
        assert_eq!(verification.direct_wake_receipts, 0);
        assert_eq!(verification.unresolved_receipts, 0);
        assert!(verification.verified());
    }

    #[test]
    fn sync_mutations_are_durable_monotonic_and_coalescible() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let car = crate::hub_pack::ProjectionCar {
            id: 1,
            name: "Test car".into(),
            model: "3".into(),
            vin: None,
            source_eid: None,
            source_vid: None,
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            firmware_version: None,
            efficiency_wh_per_km: None,
            settings: ProjectionCarSettings::default(),
        };
        store
            .persist_materialised_car_if_absent(vehicle.vehicle_id, &car)
            .expect("car");
        store
            .upsert_car_settings(vehicle.vehicle_id, 1, &ProjectionCarSettings::default())
            .expect("settings one");
        store
            .upsert_car_settings(
                vehicle.vehicle_id,
                1,
                &ProjectionCarSettings {
                    enabled: false,
                    ..ProjectionCarSettings::default()
                },
            )
            .expect("settings two");

        let connection = store.open().expect("open");
        let revisions: Vec<i64> = connection
            .prepare(
                "SELECT revision FROM sync_mutations
                 WHERE vehicle_id = ?1 ORDER BY revision",
            )
            .expect("journal query")
            .query_map(params![vehicle.vehicle_id.to_string()], |row| row.get(0))
            .expect("journal rows")
            .map(|row| row.expect("revision"))
            .collect();
        assert_eq!(revisions, vec![1, 2, 3]);
        drop(connection);

        let claim = store
            .claim_sync_mutations(vehicle.vehicle_id, 2_000, 100)
            .expect("claim")
            .expect("pending mutations");
        assert_eq!((claim.from_revision, claim.to_revision), (1, 3));
        let delta = store
            .projection_delta_for_mutations(
                &claim,
                store
                    .v2_projection_binding(vehicle.vehicle_id)
                    .expect("binding"),
                SequenceRange {
                    from_exclusive: 0,
                    to_inclusive: 3,
                },
                Sha256Digest::of_bytes(b"parent"),
            )
            .expect("typed delta");
        assert_eq!(delta.cars.len(), 1);
        assert_eq!(delta.car_settings.len(), 0);
        assert_eq!(delta.cars.len() + delta.car_settings.len(), 1);
        store.release_sync_mutations(&claim).expect("release");
    }
}
#[cfg(test)]
mod terrain_background_tests {
    use super::*;
    use crate::{
        hub_pack::{ProjectionDrive, ProjectionPosition},
        lifecycle::{LifecycleDelta, OpenSessionState},
        protocol::CursorKey,
    };

    fn position(id: i64, elevation: Option<i64>) -> ProjectionPosition {
        ProjectionPosition {
            id,
            drive_id: Some(7),
            car_id: 7,
            date_ms: id * 1_000,
            latitude: 51.0,
            longitude: -0.1,
            speed: Some(20),
            power: None,
            battery_level: Some(80),
            usable_battery_level: None,
            elevation,
            odometer: None,
            ideal_battery_range_km: None,
            est_battery_range_km: None,
            rated_battery_range_km: None,
            fan_status: None,
            driver_temp_setting: None,
            passenger_temp_setting: None,
            is_climate_on: None,
            is_rear_defroster_on: None,
            is_front_defroster_on: None,
            inside_temp: None,
            outside_temp: None,
            battery_heater: None,
            battery_heater_on: None,
            battery_heater_no_power: None,
            tpms_pressure_fl: None,
            tpms_pressure_fr: None,
            tpms_pressure_rl: None,
            tpms_pressure_rr: None,
        }
    }

    #[test]
    fn terrain_enrichment_is_restart_safe_authoritative_and_republishes_revision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("terrain_test", "one"), 1_000)
            .expect("source");
        let mut descriptor = VehicleDescriptor::new(source.source_id, "7");
        descriptor.display_name = Some("Terrain car".into());
        let vehicle = store.register_vehicle(&descriptor, 1_000).expect("vehicle");
        let drive = ProjectionDrive {
            id: 7,
            car_id: 7,
            optimized_at_ms: None,
            start_date_ms: 1_000,
            end_date_ms: 3_000,
            distance_km: Some(1.0),
            duration_min: Some(1),
            efficiency: None,
            outside_temp_avg: None,
            inside_temp_avg: None,
            speed_max: Some(20),
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_address: None,
            end_address: None,
            start_geofence: None,
            end_geofence: None,
            start_latitude: Some(51.0),
            start_longitude: Some(-0.1),
            end_latitude: Some(51.0),
            end_longitude: Some(-0.1),
            start_soc: Some(80),
            end_soc: Some(79),
            start_rated_range_km: None,
            end_rated_range_km: None,
            ascent: None,
            descent: None,
        };
        let mut open = OpenSessionState::new();
        open.car_metadata = Some(crate::hub_pack::ProjectionCarPatch {
            name: Some("Terrain car".into()),
            model: Some("3".into()),
            ..Default::default()
        });
        let encoded = open.encode().expect("open state");
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 7,
                open_session_json: &encoded,
                last_observation_id: 3,
                quarantined: false,
                updated_at_ms: 3_000,
                delta: &LifecycleDelta {
                    drives: vec![drive],
                    positions: vec![position(1, None), position(2, None), position(3, None)],
                    ..Default::default()
                },
            })
            .expect("lifecycle commit");

        let candidates = store.terrain_candidates(4_000, 1_000).expect("candidates");
        assert_eq!(candidates.len(), 3);
        for (candidate, elevation) in candidates.into_iter().zip([100_i16, 110, 90]) {
            assert!(
                store
                    .apply_terrain_result(
                        &candidate,
                        Some(elevation),
                        "N51W001",
                        "aabb",
                        "cache",
                        "srtm-0.8.0-hgt",
                        4_000,
                    )
                    .expect("terrain result")
            );
        }
        let history = store
            .materialised_history(vehicle.vehicle_id)
            .expect("history");
        assert_eq!(history.drives[0].ascent, Some(10));
        assert_eq!(history.drives[0].descent, Some(20));
        assert_eq!(
            history
                .positions
                .iter()
                .map(|p| p.elevation)
                .collect::<Vec<_>>(),
            vec![Some(100), Some(110), Some(90)]
        );
        assert!(
            store
                .terrain_candidates(4_000, 1_000)
                .expect("drained")
                .is_empty()
        );

        let authoritative = TerrainCandidate {
            vehicle_id: vehicle.vehicle_id,
            position: position(1, Some(999)),
        };
        assert!(
            !store
                .apply_terrain_result(
                    &authoritative,
                    Some(1),
                    "N51W001",
                    "different",
                    "cache",
                    "srtm-0.8.0-hgt",
                    5_000,
                )
                .expect("authoritative result")
        );
        assert_eq!(
            store
                .materialised_history(vehicle.vehicle_id)
                .expect("authoritative history")
                .positions[0]
                .elevation,
            Some(100)
        );

        assert!(
            store
                .publish_terrain_revision(vehicle.vehicle_id, &CursorKey::from_bytes([9; 32]), 1)
                .expect("publish terrain revision")
        );
        assert!(
            !store
                .publish_terrain_revision(vehicle.vehicle_id, &CursorKey::from_bytes([9; 32]), 1)
                .expect("idempotent publish")
        );
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("restart");
        let connection = reopened.open().expect("open after restart");
        let provenance: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE vehicle_id = ?1 AND status = 'success'",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("provenance");
        assert_eq!(provenance, 3);
    }

    #[test]
    fn tesla_eid_unifies_sources_and_survives_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let imported = store
            .register_source(&SourceDescriptor::new("teslamate", "copy"), 1)
            .unwrap();
        let live = store
            .register_source(
                &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
                2,
            )
            .unwrap();
        let first = store
            .register_vehicle(
                &VehicleDescriptor::new(imported.source_id, "eid:700")
                    .with_tesla_identity(Some(700), Some(900)),
                1,
            )
            .unwrap();
        let second = store
            .register_vehicle(
                &VehicleDescriptor::new(live.source_id, "700").with_tesla_identity(Some(700), None),
                2,
            )
            .unwrap();
        assert_eq!(first.vehicle_id, second.vehicle_id);
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("reopen");
        let third = reopened
            .register_vehicle(
                &VehicleDescriptor::new(live.source_id, "700").with_tesla_identity(Some(700), None),
                3,
            )
            .unwrap();
        assert_eq!(first.vehicle_id, third.vehicle_id);
    }

    #[test]
    fn distinct_eid_cars_do_not_merge_on_reused_vid_and_conflicts_fail() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("teslamate", "copy"), 1)
            .unwrap();
        let one = store
            .register_vehicle(
                &VehicleDescriptor {
                    source_id: source.source_id,
                    source_vehicle_key: "eid:701".into(),
                    vin: Some("VIN-701".into()),
                    display_name: None,
                    tesla_eid: Some(701),
                    tesla_vid: Some(901),
                },
                1,
            )
            .unwrap();
        let two = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "eid:702")
                    .with_tesla_identity(Some(702), Some(901)),
                2,
            )
            .unwrap();
        assert_ne!(one.vehicle_id, two.vehicle_id);
        let vin_conflict = store.register_vehicle(
            &VehicleDescriptor {
                source_id: source.source_id,
                source_vehicle_key: "eid:703".into(),
                vin: Some("VIN-OTHER".into()),
                display_name: None,
                tesla_eid: Some(701),
                tesla_vid: None,
            },
            4,
        );
        assert!(matches!(
            vin_conflict,
            Err(StoreError::VehicleIdentityConflict)
        ));
    }
}

#[cfg(test)]
mod observation_verification_tests {
    use rusqlite::params;

    use super::*;

    fn map_car(store: &HubStore, vehicle_id: Uuid, car_id: i64) {
        store
            .open()
            .expect("open mapping database")
            .execute(
                "INSERT INTO materialised_cars(vehicle_id, car_id, car_json)
                 VALUES (?1, ?2, ?3)",
                params![vehicle_id.to_string(), car_id, "{}"],
            )
            .expect("map source car");
    }

    #[test]
    fn watermark_and_verification_use_only_durable_observation_metadata() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (source, vehicle) = {
            let source = store
                .register_source(&SourceDescriptor::new("teslamate", "cutover-test"), 1_000)
                .expect("source");
            let vehicle = store
                .register_vehicle(&VehicleDescriptor::new(source.source_id, "vin:test"), 1_001)
                .expect("vehicle");
            (source, vehicle)
        };
        map_car(&store, vehicle.vehicle_id, 17);

        store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 2_000,
                    payload: serde_json::json!({"secret_like": "payload must not be read"}),
                },
                2_001,
            )
            .expect("first observation");

        let read_only = HubStore::open_read_only(temporary.path()).expect("read-only store");
        let watermark = read_only.observation_watermark(17).expect("watermark");
        assert_eq!(watermark.observation_id, 1);
        assert_eq!(watermark.observed_at_ms, Some(2_000));
        assert_eq!(watermark.received_at_ms, Some(2_001));
        assert!(
            !read_only
                .verify_observation_after(17, watermark.observation_id)
                .expect("pre-cutover verification")
                .verified()
        );

        store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 3_000,
                    payload: serde_json::json!({"next": true}),
                },
                3_001,
            )
            .expect("new observation");
        let verification = read_only
            .verify_observation_after(17, watermark.observation_id)
            .expect("verification");
        assert!(verification.verified());
        assert_eq!(verification.latest_observation_id, Some(2));
        assert_eq!(verification.latest_observed_at_ms, Some(3_000));
        assert_eq!(verification.latest_received_at_ms, Some(3_001));
    }

    #[test]
    fn source_car_mapping_fails_closed_when_missing_or_ambiguous() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let source = store
            .register_source(
                &SourceDescriptor::new("tesla_owner_api", "mapping-test"),
                1_000,
            )
            .expect("source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "vehicle-test"),
                1_001,
            )
            .expect("vehicle");
        assert!(matches!(
            store.observation_watermark(17),
            Err(ObservationVerificationError::NoVehicleMapping)
        ));

        map_car(&store, vehicle.vehicle_id, 17);
        let other_source = store
            .register_source(&SourceDescriptor::new("teslamate", "other"), 2_000)
            .expect("other source");
        let other_vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(other_source.source_id, "vin:other"),
                2_001,
            )
            .expect("other vehicle");
        map_car(&store, other_vehicle.vehicle_id, 17);

        assert!(matches!(
            store.observation_watermark(17),
            Err(ObservationVerificationError::AmbiguousVehicleMapping)
        ));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AddressCacheRecord {
    pub osm_type: String,
    pub osm_id: i64,
    pub display_name: String,
    pub name: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub house_number: Option<String>,
    pub road: Option<String>,
    pub neighbourhood: Option<String>,
    pub city: Option<String>,
    pub county: Option<String>,
    pub postcode: Option<String>,
    pub state: Option<String>,
    pub state_district: Option<String>,
    pub country: Option<String>,
    pub raw_json: Option<String>,
    pub lookup_latitude: f64,
    pub lookup_longitude: f64,
    pub looked_up_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AddressEnrichmentJob {
    pub job_key: String,
    pub vehicle_id: Uuid,
    pub target_type: String,
    pub target_id: i64,
    pub field: String,
    pub latitude: f64,
    pub longitude: f64,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressEnrichmentCompletion {
    pub vehicle_id: Uuid,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerrainCandidate {
    pub vehicle_id: Uuid,
    pub position: ProjectionPosition,
}

/// One opaque, single-use pairing invitation. The secret is intentionally not
/// `Debug` or `Display`; it is safe only for a local terminal or a QR payload.
#[derive(PartialEq, Eq)]
pub struct PairingInvitation {
    pub pairing_id: Uuid,
    secret: PairingSecret,
    created_at_ms: i64,
    pub expires_at_ms: i64,
}

impl PairingInvitation {
    pub fn secret(&self) -> &str {
        self.secret.as_wire()
    }
}

impl std::fmt::Debug for PairingInvitation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingInvitation")
            .field("pairing_id", &self.pairing_id)
            .field("secret", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(PartialEq, Eq, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
struct PairingSecret(String);

impl PairingSecret {
    fn generate() -> Self {
        Self(random_secret_wire())
    }

    fn as_wire(&self) -> &str {
        &self.0
    }

    fn digest(&self) -> [u8; PAIRING_SECRET_BYTES] {
        sha256_bytes(self.0.as_bytes())
    }

    fn digest_from_wire(value: &str) -> Option<[u8; PAIRING_SECRET_BYTES]> {
        digest_valid_wire_secret(value)
    }
}

/// A paired device's bearer token. It is returned once at claim time and is
/// stored only as a hash in the Hub database. It is intentionally not
/// cloneable.
///
/// ```compile_fail
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<teslatlas_hub::db::DeviceAccessToken>();
/// ```
#[derive(PartialEq, Eq, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct DeviceAccessToken(String);

impl DeviceAccessToken {
    fn generate() -> Self {
        Self(random_secret_wire())
    }

    pub fn as_bearer(&self) -> &str {
        &self.0
    }

    fn digest(&self) -> [u8; ACCESS_TOKEN_BYTES] {
        sha256_bytes(self.0.as_bytes())
    }

    fn digest_from_wire(value: &str) -> Option<[u8; ACCESS_TOKEN_BYTES]> {
        digest_valid_wire_secret(value)
    }
}

impl std::fmt::Debug for DeviceAccessToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DeviceAccessToken([redacted])")
    }
}

/// The only credential-bearing result of a successful pairing claim.
#[derive(Debug, PartialEq, Eq)]
pub struct PairedDeviceAccess {
    pub device_id: Uuid,
    pub access_token: DeviceAccessToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedDeviceRecord {
    pub device_id: Uuid,
    pub display_name: String,
    pub created_at_ms: i64,
    pub last_authenticated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PublishedVehicle {
    pub vehicle_id: Uuid,
    pub display_name: Option<String>,
}

fn validate_immutable_v2_base_binding(
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
) -> Result<(), StoreError> {
    if manifest.schema != HUB_PROJECTION_SCHEMA_V2
        || manifest.mode != crate::protocol::TransferMode::FullSnapshot
        || manifest.installation_id != binding.installation_id
        || manifest.account_id != binding.account_id
        || manifest.vehicle_id != binding.vehicle_id
        || manifest.generation != binding.generation
        || binding.selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

fn legacy_v2_base_description(
    payload: &[u8],
) -> Result<Option<LegacyV2BaseDescription>, StoreError> {
    match serde_json::from_slice::<SyncManifest>(payload) {
        Ok(manifest) => {
            validate_manifest_for_catalogue(&manifest)?;
            if manifest.schema != HUB_PROJECTION_SCHEMA_V2 {
                return Ok(None);
            }
            if manifest.mode != crate::protocol::TransferMode::FullSnapshot
                || manifest.base_sequence != manifest.head_sequence
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            let base_digest = manifest
                .chunks
                .first()
                .map(|pack| pack.sha256)
                .ok_or(StoreError::LineageCatalogConflict)?;
            Ok(Some(LegacyV2BaseDescription {
                installation_id: manifest.installation_id,
                account_id: manifest.account_id,
                vehicle_id: manifest.vehicle_id,
                generation: manifest.generation,
                snapshot_id: manifest.snapshot_id,
                base_sequence: manifest.head_sequence,
                base_digest,
                packs: manifest.chunks,
            }))
        }
        Err(sync_error) => {
            let lineage = match serde_json::from_slice::<LineageManifestV2>(payload) {
                Ok(lineage) => lineage,
                Err(_) => return Err(StoreError::DeserializeManifest(sync_error)),
            };
            lineage.validate().map_err(StoreError::Manifest)?;
            if lineage.schema != HUB_PROJECTION_SCHEMA_V2 {
                return Ok(None);
            }
            if lineage.base.packs.first().map(|pack| pack.sha256) != Some(lineage.base.digest) {
                return Err(StoreError::LineageCatalogConflict);
            }
            Ok(Some(LegacyV2BaseDescription {
                installation_id: lineage.installation_id,
                account_id: lineage.account_id,
                vehicle_id: lineage.vehicle_id,
                generation: lineage.generation,
                snapshot_id: lineage.base.snapshot_id,
                base_sequence: lineage.base.sequence,
                base_digest: lineage.base.digest,
                packs: lineage.base.packs,
            }))
        }
    }
}

fn record_immutable_v2_base_binding_in_transaction(
    transaction: &Transaction<'_>,
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
) -> Result<(), StoreError> {
    validate_immutable_v2_base_binding(manifest, binding)?;
    record_immutable_v2_base_binding_values_in_transaction(
        transaction,
        manifest.vehicle_id,
        manifest.snapshot_id,
        binding,
    )
}

fn record_immutable_v2_base_binding_values_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    snapshot_id: Uuid,
    binding: &ProjectionBinding,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil()
        || snapshot_id.is_nil()
        || binding.vehicle_id != vehicle_id
        || binding.installation_id.is_nil()
        || binding.account_id.is_nil()
        || binding.generation == 0
        || binding.selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    let base_exists: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases
                 WHERE vehicle_id = ?1 AND snapshot_id = ?2
            )",
            params![vehicle_id.to_string(), snapshot_id.to_string()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_exists {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64, i64)> = transaction
        .query_row(
            "SELECT installation_id, account_id, generation, selected_car_id
               FROM v2_base_bindings WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    let expected_generation =
        i64::try_from(binding.generation).map_err(|_| StoreError::SequenceTooLarge)?;
    if let Some((installation_id, account_id, generation, selected_car_id)) = existing {
        if installation_id != binding.installation_id.to_string()
            || account_id != binding.account_id.to_string()
            || generation != expected_generation
            || selected_car_id != binding.selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        return Ok(());
    }
    transaction
        .execute(
            "INSERT INTO v2_base_bindings(
                vehicle_id, snapshot_id, installation_id, account_id,
                generation, selected_car_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                vehicle_id.to_string(),
                snapshot_id.to_string(),
                binding.installation_id.to_string(),
                binding.account_id.to_string(),
                expected_generation,
                binding.selected_car_id,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(())
}

fn teslamate_inventory_entity_name(entity: ProjectionDeltaEntity) -> Option<&'static str> {
    match entity {
        ProjectionDeltaEntity::Drive => Some("drive"),
        ProjectionDeltaEntity::Position => Some("position"),
        ProjectionDeltaEntity::Charge => Some("charge"),
        ProjectionDeltaEntity::ChargeSample => Some("charge_sample"),
        ProjectionDeltaEntity::State => Some("state"),
        ProjectionDeltaEntity::Update => Some("update"),
        ProjectionDeltaEntity::Car
        | ProjectionDeltaEntity::CarSetting
        | ProjectionDeltaEntity::Geofence
        | ProjectionDeltaEntity::Address => None,
    }
}

fn teslamate_inventory_entity(value: &str) -> Option<ProjectionDeltaEntity> {
    match value {
        "drive" => Some(ProjectionDeltaEntity::Drive),
        "position" => Some(ProjectionDeltaEntity::Position),
        "charge" => Some(ProjectionDeltaEntity::Charge),
        "charge_sample" => Some(ProjectionDeltaEntity::ChargeSample),
        "state" => Some(ProjectionDeltaEntity::State),
        "update" => Some(ProjectionDeltaEntity::Update),
        _ => None,
    }
}

fn stored_projection_state_entity(
    name: &str,
    ordinal: i64,
) -> Result<TeslaMateProjectionStateEntity, StoreError> {
    let by_name: TeslaMateProjectionStateEntity =
        name.parse().map_err(StoreError::TeslaMateProjectionState)?;
    let by_ordinal = TeslaMateProjectionStateEntity::from_ordinal(ordinal)
        .map_err(StoreError::TeslaMateProjectionState)?;
    if by_name != by_ordinal {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(by_name)
}

fn projection_state_digest_from_blob(blob: Vec<u8>) -> Result<Sha256Digest, StoreError> {
    let digest: [u8; 32] = blob
        .try_into()
        .map_err(|_: Vec<u8>| TeslaMateProjectionStateError::InvalidStoredDigest)?;
    Ok(Sha256Digest::from_bytes(digest))
}

fn validate_teslamate_import_inventory_rows(
    selected_car_id: i64,
    rows: &[ProjectionTombstone],
) -> Result<(), StoreError> {
    if selected_car_id <= 0 {
        return Err(StoreError::LineageCatalogConflict);
    }
    let mut seen = HashSet::with_capacity(rows.len());
    for row in rows {
        if row.id <= 0
            || row.car_id != selected_car_id
            || teslamate_inventory_entity_name(row.entity).is_none()
            || !seen.insert((row.entity, row.id))
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    }
    Ok(())
}

fn validate_teslamate_import_inventory(
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
    inventory: &TeslaMateImportProjectionInventory,
) -> Result<(), StoreError> {
    validate_immutable_v2_base_binding(manifest, binding)?;
    if inventory.source_id.is_nil()
        || inventory.source_id != binding.account_id
        || inventory.selected_car_id != binding.selected_car_id
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    validate_teslamate_import_inventory_rows(inventory.selected_car_id, &inventory.rows)
}

fn validate_teslamate_import_delta_inventory(
    delta: &LineageDelta,
    binding: &ProjectionBinding,
    inventory: &TeslaMateImportProjectionInventory,
) -> Result<(), StoreError> {
    if inventory.source_id.is_nil()
        || inventory.source_id != binding.account_id
        || inventory.selected_car_id != binding.selected_car_id
        || delta.pack.snapshot_id.is_nil()
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    validate_teslamate_import_inventory_rows(inventory.selected_car_id, &inventory.rows)
}

fn replace_teslamate_import_inventory_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    base_snapshot_id: Uuid,
    head_sequence: u64,
    inventory: &TeslaMateImportProjectionInventory,
    allow_create: bool,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil() || base_snapshot_id.is_nil() {
        return Err(StoreError::LineageCatalogConflict);
    }
    validate_teslamate_import_inventory_rows(inventory.selected_car_id, &inventory.rows)?;
    let head_sequence = i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let vehicle_key = vehicle_id.to_string();
    let base_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases AS base
                JOIN sync_heads AS head ON head.vehicle_id = base.vehicle_id
                 WHERE base.vehicle_id = ?1
                   AND base.snapshot_id = ?2
                   AND head.base_snapshot_id = base.snapshot_id
                   AND head.head_sequence = ?3
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64)> = transaction
        .query_row(
            "SELECT source_id, base_snapshot_id, selected_car_id
               FROM teslamate_import_projection_heads WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((source_id, snapshot_id, selected_car_id)) = existing {
        if source_id != inventory.source_id.to_string()
            || snapshot_id != base_snapshot_id.to_string()
            || selected_car_id != inventory.selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    } else if !allow_create {
        return Err(StoreError::TeslaMateImportInventoryMissing(vehicle_id));
    }
    transaction
        .execute(
            "INSERT INTO teslamate_import_projection_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(vehicle_id) DO UPDATE SET head_sequence = excluded.head_sequence",
            params![
                vehicle_key.as_str(),
                inventory.source_id.to_string(),
                base_snapshot_id.to_string(),
                inventory.selected_car_id,
                head_sequence,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM teslamate_import_projection_rows WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO teslamate_import_projection_rows(vehicle_id, entity, entity_id)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(StoreError::LineageCatalog)?;
    for row in &inventory.rows {
        statement
            .execute(params![
                vehicle_key.as_str(),
                teslamate_inventory_entity_name(row.entity)
                    .expect("validated TeslaMate inventory entity"),
                row.id,
            ])
            .map_err(StoreError::LineageCatalog)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct LegacyDirectBridgeCandidate {
    snapshot_id: Uuid,
    head_sequence: u64,
    total_rows: u64,
    legacy_fingerprint: Sha256Digest,
}

/// Identify exactly the inventory-only V2 base shape produced by the retired
/// direct importer. This deliberately proves every relationship instead of
/// treating a missing state head as evidence that an upgrade is safe.
fn legacy_direct_bridge_candidate(
    connection: &Connection,
    vehicle_id: Uuid,
    source_id: Uuid,
    selected_car_id: i64,
) -> Result<Option<LegacyDirectBridgeCandidate>, StoreError> {
    if vehicle_id.is_nil() || source_id.is_nil() || selected_car_id <= 0 {
        return Ok(None);
    }
    let row: Option<(String, i64, Vec<u8>, Vec<u8>)> = connection
        .query_row(
            "SELECT base.snapshot_id, head.head_sequence,
                    fingerprints.fingerprint_sha256, manifests.manifest_json
               FROM sync_bases AS base
               JOIN sync_heads AS head
                 ON head.vehicle_id = base.vehicle_id
                AND head.base_snapshot_id = base.snapshot_id
               JOIN v2_base_bindings AS binding
                 ON binding.vehicle_id = base.vehicle_id
                AND binding.snapshot_id = base.snapshot_id
               JOIN teslamate_import_projection_heads AS inventory
                 ON inventory.vehicle_id = base.vehicle_id
                AND inventory.base_snapshot_id = base.snapshot_id
               JOIN snapshot_fingerprints AS fingerprints
                 ON fingerprints.vehicle_id = base.vehicle_id
                AND fingerprints.snapshot_id = base.snapshot_id
                AND fingerprints.head_sequence = head.head_sequence
               JOIN sync_manifests AS manifests
                 ON manifests.vehicle_id = base.vehicle_id
                AND manifests.snapshot_id = base.snapshot_id
                AND manifests.head_sequence = head.head_sequence
              WHERE base.vehicle_id = ?1
                AND binding.account_id = ?2
                AND binding.selected_car_id = ?3
                AND inventory.source_id = ?2
                AND inventory.selected_car_id = ?3
                AND inventory.head_sequence = head.head_sequence
                AND base.base_sequence = head.head_sequence
                AND NOT EXISTS(
                    SELECT 1 FROM sync_deltas
                     WHERE vehicle_id = base.vehicle_id
                )
                AND NOT EXISTS(
                    SELECT 1 FROM teslamate_import_projection_state_heads
                     WHERE vehicle_id = base.vehicle_id
                )
                AND NOT EXISTS(
                    SELECT 1 FROM teslamate_import_projection_state_bridges
                     WHERE vehicle_id = base.vehicle_id
                )",
            params![
                vehicle_id.to_string(),
                source_id.to_string(),
                selected_car_id,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    let Some((snapshot_id, head_sequence, legacy_bytes, manifest_bytes)) = row else {
        return Ok(None);
    };
    let Ok(snapshot_id) = Uuid::parse_str(&snapshot_id) else {
        return Ok(None);
    };
    let Ok(head_sequence) = u64::try_from(head_sequence) else {
        return Ok(None);
    };
    let Ok(legacy_bytes) = <[u8; 32]>::try_from(legacy_bytes) else {
        return Ok(None);
    };
    let Ok(manifest) = decode_manifest(manifest_bytes) else {
        return Ok(None);
    };
    if manifest.vehicle_id != vehicle_id
        || manifest.snapshot_id != snapshot_id
        || manifest.head_sequence != head_sequence
        || manifest.schema != HUB_PROJECTION_SCHEMA_V2
    {
        return Ok(None);
    }
    Ok(Some(LegacyDirectBridgeCandidate {
        snapshot_id,
        head_sequence,
        total_rows: manifest.total_rows,
        legacy_fingerprint: Sha256Digest::from_bytes(legacy_bytes),
    }))
}

/// The old inventory must exactly describe every non-car row in the newly
/// captured durable state. The enclosing transaction has not committed yet,
/// so a failed comparison leaves neither replacement state nor marker behind.
fn legacy_projection_inventory_matches_state_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<bool, StoreError> {
    transaction
        .query_row(
            "SELECT
                NOT EXISTS(
                    SELECT 1
                      FROM teslamate_import_projection_rows AS inventory
                     WHERE inventory.vehicle_id = ?1
                       AND NOT EXISTS(
                           SELECT 1
                             FROM teslamate_import_projection_state_rows AS state
                            WHERE state.vehicle_id = inventory.vehicle_id
                              AND state.entity = inventory.entity
                              AND state.entity_id = inventory.entity_id
                       )
                )
                AND NOT EXISTS(
                    SELECT 1
                      FROM teslamate_import_projection_state_rows AS state
                     WHERE state.vehicle_id = ?1
                       AND state.entity <> 'car'
                       AND NOT EXISTS(
                           SELECT 1
                             FROM teslamate_import_projection_rows AS inventory
                            WHERE inventory.vehicle_id = state.vehicle_id
                              AND inventory.entity = state.entity
                              AND inventory.entity_id = state.entity_id
                       )
                )",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)
}

fn legacy_direct_bridge_state_error(vehicle_id: Uuid, error: StoreError) -> StoreError {
    match error {
        StoreError::LineageCatalogConflict
        | StoreError::ImmutableBaseBindingMissing(_)
        | StoreError::TeslaMateImportInventoryMissing(_)
        | StoreError::TeslaMateImportProjectionStateMissing(_)
        | StoreError::TeslaMateProjectionState(_) => {
            StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id)
        }
        error => error,
    }
}

/// Attach a previously verified state spool only through SQLite's read-only
/// URI mode, then authenticate the attachment once more before a catalogue
/// transaction can read it. The fixed schema name is internal and never
/// accepts a caller-controlled identifier.
fn attach_teslamate_projection_state_transfer(
    connection: &Connection,
    transfer: &TeslaMateProjectionStateTransfer,
) -> Result<(), StoreError> {
    let uri = transfer.read_only_attachment_uri()?;
    connection
        .execute(
            "ATTACH DATABASE ?1 AS teslamate_projection_state_spool",
            params![uri],
        )
        .map_err(StoreError::LineageCatalog)?;
    if let Err(error) = transfer.validate_attached(connection) {
        // Validation happens before the Hub transaction begins. An explicit
        // detach keeps this connection reusable on the normal error path;
        // dropping the connection remains a final close-on-error backstop.
        let _ = connection.execute_batch("DETACH DATABASE teslamate_projection_state_spool");
        return Err(error.into());
    }
    Ok(())
}

fn detach_teslamate_projection_state_transfer(
    store: &HubStore,
    connection: &Connection,
) -> Result<(), StoreError> {
    #[cfg(not(test))]
    let _ = store;
    #[cfg(test)]
    {
        let mut fault = store
            .projection_state_detach_fault
            .lock()
            .expect("projection-state detach fault lock");
        if *fault {
            *fault = false;
            return Err(StoreError::InjectedProjectionStateDetachFault);
        }
    }
    connection
        .execute_batch("DETACH DATABASE teslamate_projection_state_spool")
        .map_err(StoreError::LineageCatalog)
}

/// A commit cannot be rolled back after the transfer attachment is no longer
/// needed. If SQLite then declines the best-effort detach, closing this local
/// connection releases the attachment; reporting an error would make callers
/// discard packs that the committed catalogue now owns.
fn finish_teslamate_projection_state_transfer<T>(
    result: Result<T, StoreError>,
    detach: Result<(), StoreError>,
) -> Result<T, StoreError> {
    match (result, detach) {
        (Err(error), _) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
        (Ok(value), Err(error)) => {
            tracing::warn!(%error, "could not detach committed TeslaMate projection-state spool; closing connection releases attachment");
            Ok(value)
        }
    }
}

/// Set-based counterpart of
/// [`replace_teslamate_import_projection_state_in_transaction`]. The source
/// was authenticated while attached read-only, so this writes one SQLite
/// `INSERT … SELECT` rather than crossing Rust for every source fact.
fn replace_teslamate_import_projection_state_from_attached_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
    base_snapshot_id: Uuid,
    head_sequence: u64,
    selected_car_id: i64,
    transfer: &TeslaMateProjectionStateTransfer,
    allow_create: bool,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil()
        || source_id.is_nil()
        || base_snapshot_id.is_nil()
        || selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    let head_sequence = i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let vehicle_key = vehicle_id.to_string();
    let base_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases AS base
                JOIN sync_heads AS head ON head.vehicle_id = base.vehicle_id
                 WHERE base.vehicle_id = ?1
                   AND base.snapshot_id = ?2
                   AND head.base_snapshot_id = base.snapshot_id
                   AND head.head_sequence = ?3
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let binding_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM v2_base_bindings
                 WHERE vehicle_id = ?1
                   AND snapshot_id = ?2
                   AND account_id = ?3
                   AND selected_car_id = ?4
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                source_id.to_string(),
                selected_car_id
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !binding_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64)> = transaction
        .query_row(
            "SELECT source_id, base_snapshot_id, selected_car_id
               FROM teslamate_import_projection_state_heads WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((stored_source_id, stored_base_snapshot_id, stored_selected_car_id)) = existing {
        if stored_source_id != source_id.to_string()
            || stored_base_snapshot_id != base_snapshot_id.to_string()
            || stored_selected_car_id != selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    } else if !allow_create {
        return Err(StoreError::TeslaMateImportProjectionStateMissing(
            vehicle_id,
        ));
    }
    transaction
        .execute(
            "INSERT INTO teslamate_import_projection_state_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(vehicle_id) DO UPDATE SET head_sequence = excluded.head_sequence",
            params![
                vehicle_key.as_str(),
                source_id.to_string(),
                base_snapshot_id.to_string(),
                selected_car_id,
                head_sequence,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM teslamate_import_projection_state_rows WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let inserted = transaction
        .execute(
            "INSERT INTO teslamate_import_projection_state_rows(
                vehicle_id, entity, entity_ordinal, entity_id, car_id, projection_sha256
             )
             SELECT ?1,
                    CASE entity_ordinal
                        WHEN 0 THEN 'car'
                        WHEN 1 THEN 'drive'
                        WHEN 2 THEN 'position'
                        WHEN 3 THEN 'charge'
                        WHEN 4 THEN 'charge_sample'
                        WHEN 5 THEN 'state'
                        WHEN 6 THEN 'update'
                    END,
                    entity_ordinal, entity_id, car_id, projection_sha256
               FROM teslamate_projection_state_spool.current_rows
              ORDER BY entity_ordinal ASC, entity_id ASC",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    if u64::try_from(inserted).map_err(|_| StoreError::LineageCatalogConflict)?
        != transfer.stats().row_count
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

/// Rebuild the retained legacy deletion inventory from the just-inserted
/// durable state. The car row has already been required by the descriptor and
/// intentionally remains absent from this legacy table. Reading the target
/// rather than the attachment makes the two catalogue views exactly identical
/// even if an outside process replaces the spool path after attachment.
fn replace_teslamate_import_projection_inventory_from_attached_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
    base_snapshot_id: Uuid,
    head_sequence: u64,
    selected_car_id: i64,
    transfer: &TeslaMateProjectionStateTransfer,
    allow_create: bool,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil()
        || source_id.is_nil()
        || base_snapshot_id.is_nil()
        || selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    let head_sequence = i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let vehicle_key = vehicle_id.to_string();
    let base_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases AS base
                JOIN sync_heads AS head ON head.vehicle_id = base.vehicle_id
                 WHERE base.vehicle_id = ?1
                   AND base.snapshot_id = ?2
                   AND head.base_snapshot_id = base.snapshot_id
                   AND head.head_sequence = ?3
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64)> = transaction
        .query_row(
            "SELECT source_id, base_snapshot_id, selected_car_id
               FROM teslamate_import_projection_heads WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((stored_source_id, stored_base_snapshot_id, stored_selected_car_id)) = existing {
        if stored_source_id != source_id.to_string()
            || stored_base_snapshot_id != base_snapshot_id.to_string()
            || stored_selected_car_id != selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    } else if !allow_create {
        return Err(StoreError::TeslaMateImportInventoryMissing(vehicle_id));
    }
    transaction
        .execute(
            "INSERT INTO teslamate_import_projection_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(vehicle_id) DO UPDATE SET head_sequence = excluded.head_sequence",
            params![
                vehicle_key.as_str(),
                source_id.to_string(),
                base_snapshot_id.to_string(),
                selected_car_id,
                head_sequence,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM teslamate_import_projection_rows WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let inserted = transaction
        .execute(
            "INSERT INTO teslamate_import_projection_rows(vehicle_id, entity, entity_id)
             SELECT vehicle_id, entity, entity_id
               FROM teslamate_import_projection_state_rows
              WHERE vehicle_id = ?1 AND entity_ordinal BETWEEN 1 AND 6
              ORDER BY entity_ordinal ASC, entity_id ASC",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let expected = transfer
        .stats()
        .row_count
        .checked_sub(1)
        .ok_or(StoreError::LineageCatalogConflict)?;
    if u64::try_from(inserted).map_err(|_| StoreError::LineageCatalogConflict)? != expected {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

/// Replace the digest-only current projection state in the same transaction
/// that advances the immutable-base lineage head. `allow_create` is true only
/// while cataloguing an initial base; a successor must refuse missing state
/// rather than silently treating a legacy inventory as equivalent provenance.
pub(crate) fn replace_teslamate_import_projection_state_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
    base_snapshot_id: Uuid,
    head_sequence: u64,
    selected_car_id: i64,
    state: &TeslaMateProjectionState,
    allow_create: bool,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil()
        || source_id.is_nil()
        || base_snapshot_id.is_nil()
        || selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    if !state.stats().sealed {
        return Err(StoreError::TeslaMateProjectionState(
            TeslaMateProjectionStateError::StateNotSealed,
        ));
    }
    let head_sequence = i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let vehicle_key = vehicle_id.to_string();
    let base_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases AS base
                JOIN sync_heads AS head ON head.vehicle_id = base.vehicle_id
                 WHERE base.vehicle_id = ?1
                   AND base.snapshot_id = ?2
                   AND head.base_snapshot_id = base.snapshot_id
                   AND head.head_sequence = ?3
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let binding_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM v2_base_bindings
                 WHERE vehicle_id = ?1
                   AND snapshot_id = ?2
                   AND account_id = ?3
                   AND selected_car_id = ?4
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                source_id.to_string(),
                selected_car_id
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !binding_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64)> = transaction
        .query_row(
            "SELECT source_id, base_snapshot_id, selected_car_id
               FROM teslamate_import_projection_state_heads WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((stored_source_id, stored_base_snapshot_id, stored_selected_car_id)) = existing {
        if stored_source_id != source_id.to_string()
            || stored_base_snapshot_id != base_snapshot_id.to_string()
            || stored_selected_car_id != selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    } else if !allow_create {
        return Err(StoreError::TeslaMateImportProjectionStateMissing(
            vehicle_id,
        ));
    }
    transaction
        .execute(
            "INSERT INTO teslamate_import_projection_state_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(vehicle_id) DO UPDATE SET head_sequence = excluded.head_sequence",
            params![
                vehicle_key.as_str(),
                source_id.to_string(),
                base_snapshot_id.to_string(),
                selected_car_id,
                head_sequence,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM teslamate_import_projection_state_rows WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO teslamate_import_projection_state_rows(
                vehicle_id, entity, entity_ordinal, entity_id, car_id, projection_sha256
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(StoreError::LineageCatalog)?;
    let mut saw_car = false;
    let mut after = None;
    loop {
        let page = state.page(after, MAX_PAGE_SIZE)?;
        for row in &page.rows {
            if row.id <= 0
                || row.car_id != selected_car_id
                || matches!(row.entity, TeslaMateProjectionStateEntity::Car)
                    && (row.id != selected_car_id || saw_car)
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            if matches!(row.entity, TeslaMateProjectionStateEntity::Car) {
                saw_car = true;
            }
            statement
                .execute(params![
                    vehicle_key.as_str(),
                    row.entity.as_str(),
                    i64::from(row.entity.ordinal()),
                    row.id,
                    row.car_id,
                    row.digest.as_bytes().as_slice(),
                ])
                .map_err(StoreError::LineageCatalog)?;
        }
        match page.next_after {
            Some(next_after) => after = Some(next_after),
            None => break,
        }
    }
    if !saw_car {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

fn publish_manifest_in_transaction(
    transaction: &Transaction<'_>,
    manifest: &SyncManifest,
    binding: Option<&ProjectionBinding>,
) -> Result<(), StoreError> {
    validate_manifest_for_catalogue(manifest)?;
    match binding {
        Some(binding) => validate_immutable_v2_base_binding(manifest, binding)?,
        None if manifest.schema == HUB_PROJECTION_SCHEMA_V2 => {
            return Err(StoreError::ImmutableBaseBindingMissing(manifest.vehicle_id));
        }
        None => {}
    }
    // `SyncManifest` describes a full snapshot or generic V1 incremental
    // transfer. It has no typed-delta marker, so it can never safely extend
    // an immutable V2 projection base. Import successors must go through
    // `finalize_import_delta_successor`, which receives a `LineageDelta`
    // written by the typed projection-delta writer.
    if manifest.schema == HUB_PROJECTION_SCHEMA_V2
        && transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_bases WHERE vehicle_id = ?1)",
                params![manifest.vehicle_id.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(StoreError::LineageCatalog)?
    {
        return Err(StoreError::ImportDeltaRequiresBaseBinding);
    }
    let payload = serde_json::to_vec(manifest).map_err(StoreError::SerializeManifest)?;
    let snapshot_id = manifest.snapshot_id.to_string();
    let vehicle_id = manifest.vehicle_id.to_string();
    let head_sequence =
        i64::try_from(manifest.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let current = transaction.query_row(
        "SELECT snapshot_id, head_sequence FROM sync_manifests WHERE vehicle_id = ?1 ORDER BY head_sequence DESC LIMIT 1",
        params![vehicle_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    ).optional().map_err(StoreError::Query)?;
    if let Some((current_snapshot_id, current_sequence)) = current {
        let current_sequence =
            u64::try_from(current_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        if current_sequence > manifest.head_sequence
            || (current_sequence == manifest.head_sequence && current_snapshot_id != snapshot_id)
        {
            return Err(StoreError::StaleManifest {
                vehicle_id: manifest.vehicle_id,
                attempted: manifest.head_sequence,
                current: current_sequence,
            });
        }
    }
    transaction
        .execute(
            "INSERT INTO sync_manifests(snapshot_id, vehicle_id, head_sequence, manifest_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(snapshot_id) DO UPDATE SET vehicle_id = excluded.vehicle_id,
            head_sequence = excluded.head_sequence, manifest_json = excluded.manifest_json",
            params![snapshot_id, vehicle_id, head_sequence, payload],
        )
        .map_err(StoreError::PublishManifest)?;
    transaction
        .execute(
            "DELETE FROM sync_packs WHERE snapshot_id = ?1",
            params![manifest.snapshot_id.to_string()],
        )
        .map_err(StoreError::PublishManifest)?;
    for pack in &manifest.chunks {
        transaction.execute(
            "INSERT INTO sync_packs(sha256, snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![pack.sha256.to_string(), manifest.snapshot_id.to_string(), i64::from(pack.ordinal),
                pack.relative_path, i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
                i64::try_from(pack.uncompressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?],
        ).map_err(StoreError::PublishManifest)?;
    }
    if manifest.schema == HUB_PROJECTION_SCHEMA_V2 && !manifest.chunks.is_empty() {
        let pack_digest = manifest.chunks[0].sha256.to_string();
        let packs_json =
            serde_json::to_vec(&manifest.chunks).map_err(StoreError::SerializeManifest)?;
        let terminal_cursor = serde_json::to_string(&manifest.terminal_cursor)
            .map_err(StoreError::SerializeManifest)?;
        transaction
            .execute(
                "INSERT INTO sync_bases(vehicle_id, snapshot_id, base_sequence, base_digest, packs_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    manifest.vehicle_id.to_string(),
                    manifest.snapshot_id.to_string(),
                    head_sequence,
                    pack_digest,
                    packs_json
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        // This is deliberately part of the same transaction as the base and
        // head catalogue writes. A V2 base must never become visible before
        // its exact immutable source/car binding is durable.
        record_immutable_v2_base_binding_in_transaction(
            transaction,
            manifest,
            binding.ok_or(StoreError::ImmutableBaseBindingMissing(manifest.vehicle_id))?,
        )?;
        transaction
            .execute(
                "INSERT INTO sync_heads(vehicle_id, base_snapshot_id, head_sequence, head_digest, terminal_cursor)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    manifest.vehicle_id.to_string(),
                    manifest.snapshot_id.to_string(),
                    head_sequence,
                    pack_digest,
                    terminal_cursor
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        transaction
            .execute(
                "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0 WHERE vehicle_id = ?1 AND published = 0",
                params![manifest.vehicle_id.to_string()],
            )
            .map_err(StoreError::LineageCatalog)?;
    }
    Ok(())
}

fn record_snapshot_fingerprint_in_transaction(
    transaction: &Transaction<'_>,
    manifest: &SyncManifest,
    fingerprint: Sha256Digest,
) -> Result<(), StoreError> {
    validate_manifest_for_catalogue(manifest)?;
    let head_sequence =
        i64::try_from(manifest.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let associated: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_manifests
                 WHERE snapshot_id = ?1 AND vehicle_id = ?2 AND head_sequence = ?3
            )",
            params![
                manifest.snapshot_id.to_string(),
                manifest.vehicle_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::PublishManifest)?;
    if !associated {
        return Err(StoreError::FingerprintManifestMissing(manifest.snapshot_id));
    }
    transaction
        .execute(
            "INSERT INTO snapshot_fingerprints(
                vehicle_id, fingerprint_sha256, snapshot_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                fingerprint_sha256 = excluded.fingerprint_sha256,
                snapshot_id = excluded.snapshot_id,
                head_sequence = excluded.head_sequence",
            params![
                manifest.vehicle_id.to_string(),
                fingerprint.as_bytes().as_slice(),
                manifest.snapshot_id.to_string(),
                head_sequence,
            ],
        )
        .map_err(StoreError::PublishManifest)?;
    Ok(())
}

fn upsert_geofences_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    geofences: &[crate::teslamate_projection::TeslaMateGeofence],
) -> Result<usize, StoreError> {
    let mut inserted = 0;
    for geofence in geofences {
        let Some((latitude, longitude, radius_m)) = geofence.valid_geometry() else {
            continue;
        };
        if geofence.name.trim().is_empty() || geofence.name.len() > 256 {
            continue;
        }
        inserted += transaction.execute(
            "INSERT INTO geofences(vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                billing_type, cost_per_unit, session_fee) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(vehicle_id, source_geofence_id) DO NOTHING",
            params![vehicle_id.to_string(), geofence.id, geofence.name.trim(), latitude, longitude, radius_m,
                geofence.billing_type.map(crate::hub_pack::GeofenceBillingType::as_str), geofence.cost_per_unit, geofence.session_fee],
        ).map_err(StoreError::LifecycleWrite)?;
        transaction
            .execute(
                "UPDATE geofences SET name=?3, latitude=?4, longitude=?5, radius_m=?6,
                billing_type=COALESCE(?7,billing_type), cost_per_unit=COALESCE(?8,cost_per_unit),
                session_fee=COALESCE(?9,session_fee) WHERE vehicle_id=?1 AND source_geofence_id=?2",
                params![
                    vehicle_id.to_string(),
                    geofence.id,
                    geofence.name.trim(),
                    latitude,
                    longitude,
                    radius_m,
                    geofence
                        .billing_type
                        .map(crate::hub_pack::GeofenceBillingType::as_str),
                    geofence.cost_per_unit,
                    geofence.session_fee
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
    }
    Ok(inserted)
}

fn promote_imported_open_session_in_transaction(
    transaction: &Transaction<'_>,
    source_id: Uuid,
    vehicle_id: Uuid,
    car_id: i64,
    session: &TeslaMateOpenSession,
    updated_at_ms: i64,
    expected: Option<(i64, i64)>,
) -> Result<OpenSessionSeedReport, StoreError> {
    if source_id.is_nil() || vehicle_id.is_nil() || car_id <= 0 {
        return Err(StoreError::InvalidLifecycleCarId);
    }
    validate_timestamp("open session updated_at_ms", updated_at_ms)?;
    session
        .validate()
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
    let previous = load_lifecycle_state_in_transaction(transaction, vehicle_id)?;
    if let Some((last_observation_id, prior_updated_at_ms)) = expected {
        let actual = previous
            .as_ref()
            .map(|state| (state.last_observation_id, state.updated_at_ms));
        if actual != Some((last_observation_id, prior_updated_at_ms))
            && (actual.is_some() || (last_observation_id, prior_updated_at_ms) != (0, 0))
        {
            return Err(StoreError::ImportGenerationConflict);
        }
    }
    let previous_state = previous
        .as_ref()
        .map(|state| {
            crate::lifecycle::OpenSessionState::decode(&state.open_session_json)
                .map_err(|_| StoreError::InvalidLifecycleSession)
        })
        .transpose()?;
    let seeded = crate::lifecycle::seed_imported_open_session_state(
        source_id,
        session,
        previous_state.as_ref(),
    )
    .map_err(|_| StoreError::InvalidLifecycleSession)?;
    ensure_source_exists(transaction, source_id)?;
    ensure_vehicle_source(transaction, vehicle_id, source_id)?;
    let source = source_id.to_string();
    let vehicle = vehicle_id.to_string();
    transaction
        .execute(
            "DELETE FROM lifecycle_open_rows WHERE source_id=?1 AND vehicle_id=?2",
            params![source, vehicle],
        )
        .map_err(StoreError::LifecycleWrite)?;
    transaction
        .execute(
            "DELETE FROM lifecycle_source_watermarks WHERE source_id=?1 AND vehicle_id=?2",
            params![source, vehicle],
        )
        .map_err(StoreError::LifecycleWrite)?;
    let mut inserted = 0;
    if let Some(row) = &session.drive {
        inserted += insert_open_row(
            transaction,
            &source,
            "drives",
            row.id,
            &vehicle,
            car_id,
            "drive",
            None,
            row,
        )?;
    }
    for row in &session.drive_positions {
        inserted += insert_open_row(
            transaction,
            &source,
            "positions",
            row.id,
            &vehicle,
            car_id,
            "position",
            row.drive_id,
            row,
        )?;
    }
    if let Some(row) = &session.charge {
        inserted += insert_open_row(
            transaction,
            &source,
            "charging_processes",
            row.id,
            &vehicle,
            car_id,
            "charge",
            None,
            row,
        )?;
    }
    for row in &session.charge_samples {
        inserted += insert_open_row(
            transaction,
            &source,
            "charges",
            row.id,
            &vehicle,
            car_id,
            "charge_sample",
            Some(row.charging_process_id),
            row,
        )?;
    }
    if let Some(row) = &session.state {
        inserted += insert_open_row(
            transaction,
            &source,
            "states",
            row.id,
            &vehicle,
            car_id,
            "state",
            None,
            row,
        )?;
    }
    for row in &session.standalone_positions {
        inserted += insert_open_row(
            transaction,
            &source,
            "positions",
            row.id,
            &vehicle,
            car_id,
            "standalone_position",
            None,
            row,
        )?;
    }
    let mut standalone_positions_inserted = 0;
    for row in &session.standalone_positions {
        let position = crate::lifecycle::imported_position(row);
        let json = serde_json::to_string(&position).map_err(StoreError::SerializeLifecycleRow)?;
        standalone_positions_inserted += transaction.execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json,
                speed, power, est_battery_range_km, fan_status, driver_temp_setting, passenger_temp_setting,
                is_climate_on, is_rear_defroster_on, is_front_defroster_on, battery_heater, battery_heater_on,
                battery_heater_no_power, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
             ON CONFLICT(vehicle_id, position_id) DO NOTHING",
            params![vehicle, position.id, car_id, json, position.speed, position.power, position.est_battery_range_km,
                position.fan_status, position.driver_temp_setting, position.passenger_temp_setting,
                position.is_climate_on.map(i64::from), position.is_rear_defroster_on.map(i64::from),
                position.is_front_defroster_on.map(i64::from), position.battery_heater.map(i64::from),
                position.battery_heater_on.map(i64::from), position.battery_heater_no_power.map(i64::from),
                position.tpms_pressure_fl, position.tpms_pressure_fr, position.tpms_pressure_rl, position.tpms_pressure_rr],
        ).map_err(StoreError::LifecycleWrite)?;
    }
    let watermarks = [
        ("drives", session.watermarks.drives),
        ("positions", session.watermarks.positions),
        ("charging_processes", session.watermarks.charging_processes),
        ("charges", session.watermarks.charges),
        ("states", session.watermarks.states),
        ("updates", session.watermarks.updates),
    ];
    for (domain, watermark) in watermarks {
        transaction.execute(
            "INSERT INTO lifecycle_source_watermarks(source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(source_id, vehicle_id, domain) DO UPDATE SET
             max_source_row_id=MAX(max_source_row_id, excluded.max_source_row_id),
             max_timestamp_ms=MAX(max_timestamp_ms, excluded.max_timestamp_ms)",
            params![source, vehicle, domain, watermark.max_id, watermark.max_timestamp_ms],
        ).map_err(StoreError::LifecycleWrite)?;
    }
    let json = seeded
        .encode()
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
    transaction.execute(
        "INSERT INTO vehicle_lifecycle_state(vehicle_id, car_id, last_observation_id, open_session_json, quarantined, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, 0, ?5) ON CONFLICT(vehicle_id) DO UPDATE SET car_id=excluded.car_id,
         open_session_json=excluded.open_session_json, updated_at_ms=MAX(updated_at_ms, excluded.updated_at_ms)",
        params![vehicle, car_id, previous.as_ref().map_or(0, |state| state.last_observation_id), json, updated_at_ms],
    ).map_err(StoreError::LifecycleWrite)?;
    mark_export_dirty_in_transaction(transaction, vehicle_id)?;
    Ok(OpenSessionSeedReport {
        provisional_rows_inserted: inserted,
        standalone_positions_inserted,
        watermarks_written: watermarks.len(),
        no_op: false,
    })
}
