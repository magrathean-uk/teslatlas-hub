// SPDX-License-Identifier: AGPL-3.0-only

//! Durable, local-only SQLite staging for a source database migration.
//!
//! The PostgreSQL reader will write decoded rows here one page at a time. This
//! deliberately does not project or import any history: it is the bounded,
//! sealed hand-off between the source reader and a later pack writer. A stage
//! can be read only after sealing, so an interrupted capture can never look
//! like a complete source snapshot.

use std::{
    ffi::{OsStr, OsString},
    fs,
    os::fd::{AsFd, AsRawFd, OwnedFd},
    path::{Component, Path, PathBuf},
};

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};
use rustix::{
    fs::{
        AtFlags, FileType, Mode, OFlags, fchmod, fstat, fsync, mkdirat, open, openat, statvfs,
        unlinkat,
    },
    process::{getegid, geteuid},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::protocol::Sha256Digest;

const STAGING_DIRECTORY: &str = ".staging";
const STAGE_FILE_EXTENSION: &str = "sqlite";
const META_STATE: &str = "state";
const META_ROW_COUNT: &str = "row_count";
const META_PAYLOAD_BYTES: &str = "payload_bytes";
const META_MAX_ROWS: &str = "max_rows";
const META_MAX_STAGE_BYTES: &str = "max_stage_bytes";
const META_MINIMUM_FREE_BYTES: &str = "minimum_free_bytes";
const META_FORMAT: &str = "format";
const MIN_STAGE_BYTES: u64 = 64 * 1024;
const MAX_PAGE_SIZE: u32 = 10_000;
const MAX_ENCODING_WORKERS: usize = 8;
const DEFAULT_MINIMUM_FREE_BYTES: u64 = 512 * 1024 * 1024;
const CHARGE_SAMPLES_PAGE_SQL: &str = "SELECT source_id, row_json
     FROM stage_rows INDEXED BY stage_charge_samples_by_process
     WHERE table_name = 'charges'
       AND json_extract(row_json, '$.charging_process_id') = ?1
       AND source_id > ?2
     ORDER BY source_id ASC
     LIMIT ?3";

/// Fixed source-table names accepted by the source database stage. Callers cannot
/// interpolate relation names into a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceDbStageTable {
    GlobalSettings,
    CarSettings,
    Cars,
    Drives,
    Positions,
    ChargingProcesses,
    Charges,
    Addresses,
    Geofences,
    States,
    Updates,
}

impl SourceDbStageTable {
    pub const PHYSICAL_V3_ALL: [Self; 11] = [
        Self::GlobalSettings,
        Self::CarSettings,
        Self::Cars,
        Self::Drives,
        Self::Positions,
        Self::ChargingProcesses,
        Self::Charges,
        Self::Addresses,
        Self::Geofences,
        Self::States,
        Self::Updates,
    ];

    pub const ALL: [Self; 9] = [
        Self::Cars,
        Self::Drives,
        Self::Positions,
        Self::ChargingProcesses,
        Self::Charges,
        Self::Addresses,
        Self::Geofences,
        Self::States,
        Self::Updates,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GlobalSettings => "global_settings",
            Self::CarSettings => "car_settings",
            Self::Cars => "cars",
            Self::Drives => "drives",
            Self::Positions => "positions",
            Self::ChargingProcesses => "charging_processes",
            Self::Charges => "charges",
            Self::Addresses => "addresses",
            Self::Geofences => "geofences",
            Self::States => "states",
            Self::Updates => "updates",
        }
    }
}

/// Persisted row contract for one private source stage. Compatibility stages
/// retain the existing projected JSON shapes. Physical V3 stages are a
/// separate, explicit boundary and may never be consumed by the schema-2.1
/// fragment writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDbStageFormat {
    CompatibilityV2,
    PhysicalV3,
}

impl SourceDbStageFormat {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CompatibilityV2 => "compatibility-v2",
            Self::PhysicalV3 => "physical-v3",
        }
    }

    fn parse(value: &str) -> Result<Self, SourceDbStageError> {
        match value {
            "compatibility-v2" => Ok(Self::CompatibilityV2),
            "physical-v3" => Ok(Self::PhysicalV3),
            other => Err(SourceDbStageError::InvalidPersistedFormat(other.to_owned())),
        }
    }

    const fn accepts(self, table: SourceDbStageTable) -> bool {
        match self {
            Self::CompatibilityV2 => !matches!(
                table,
                SourceDbStageTable::GlobalSettings | SourceDbStageTable::CarSettings
            ),
            Self::PhysicalV3 => true,
        }
    }
}

/// Hard bounds for one source capture. `max_stage_bytes` is an upper bound on
/// the SQLite database allocation as well as the cumulative encoded JSON
/// payload, preventing a temporary import from consuming arbitrary host disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceDbStageLimits {
    pub max_rows: u64,
    pub max_stage_bytes: u64,
    /// Free space left untouched after reserving this stage's declared cap.
    /// This keeps a Pi or small VPS from filling its root volume mid-capture.
    pub minimum_free_bytes: u64,
}

impl Default for SourceDbStageLimits {
    fn default() -> Self {
        Self {
            max_rows: 5_000_000,
            max_stage_bytes: 4 * 1024 * 1024 * 1024,
            minimum_free_bytes: DEFAULT_MINIMUM_FREE_BYTES,
        }
    }
}

impl SourceDbStageLimits {
    pub fn validate(self) -> Result<(), SourceDbStageError> {
        if self.max_rows == 0 {
            return Err(SourceDbStageError::InvalidMaximumRows);
        }
        if self.max_stage_bytes < MIN_STAGE_BYTES {
            return Err(SourceDbStageError::InvalidMaximumStageBytes {
                minimum: MIN_STAGE_BYTES,
            });
        }
        let pages = self.max_stage_bytes / 4096;
        if self.max_stage_bytes > u64::try_from(i64::MAX).expect("i64 max fits u64")
            || pages == 0
            || pages > u64::try_from(i64::MAX).expect("i64 max fits u64")
        {
            return Err(SourceDbStageError::InvalidMaximumStageBytes {
                minimum: MIN_STAGE_BYTES,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDbStageState {
    Open,
    Sealed,
}

impl SourceDbStageState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Sealed => "sealed",
        }
    }

    fn parse(value: &str) -> Result<Self, SourceDbStageError> {
        match value {
            "open" => Ok(Self::Open),
            "sealed" => Ok(Self::Sealed),
            other => Err(SourceDbStageError::InvalidPersistedState(other.to_owned())),
        }
    }
}

/// Persisted accounting for a stage. It is intentionally small; callers page
/// rows from SQLite rather than materialising a `SourceDbHistory` in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceDbStageStats {
    pub state: SourceDbStageState,
    pub row_count: u64,
    pub payload_bytes: u64,
    pub limits: SourceDbStageLimits,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceDbStageRow<T> {
    pub source_id: i64,
    pub value: T,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceDbStagePage<T> {
    pub rows: Vec<SourceDbStageRow<T>>,
    pub next_after_id: Option<i64>,
}

/// A writable open capture or a read-only reopened sealed capture.
pub struct SourceDbStage {
    path: PathBuf,
    connection: Connection,
    writable: bool,
    format: SourceDbStageFormat,
    file_identity: StageFileIdentity,
    directory: PrivateDirectory,
    file_name: OsString,
    file_descriptor: OwnedFd,
}

/// A capture owned by an in-flight import. Failed or cancelled imports must
/// remove their private source rows; publication takes ownership explicitly.
#[derive(Debug)]
pub(crate) struct OwnedSourceDbStage(Option<SourceDbStage>);

impl OwnedSourceDbStage {
    pub(crate) fn new(stage: SourceDbStage) -> Self {
        Self(Some(stage))
    }

    pub(crate) fn stage_mut(&mut self) -> &mut SourceDbStage {
        self.0.as_mut().expect("owned stage is present")
    }

    pub(crate) fn take(mut self) -> SourceDbStage {
        self.0.take().expect("owned stage is present")
    }
}

impl Drop for OwnedSourceDbStage {
    fn drop(&mut self) {
        if let Some(stage) = self.0.take()
            && let Err(error) = stage.discard()
        {
            tracing::error!(%error, "could not discard unpublished physical source stage");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StageFileIdentity {
    device: u64,
    inode: u64,
}

struct PrivateDirectory {
    descriptor: OwnedFd,
    identity: StageFileIdentity,
    path: PathBuf,
}

struct PrivateStagePath {
    directory: PrivateDirectory,
    file_name: OsString,
    path: PathBuf,
}

impl std::fmt::Debug for SourceDbStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceDbStage")
            .field("path", &self.path)
            .field("writable", &self.writable)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl SourceDbStage {
    /// Start a new capture under `<imports_dir>/.staging`. Both directories
    /// are private and the database file is mode 0600 on Unix hosts.
    pub fn create(
        imports_dir: impl AsRef<Path>,
        limits: SourceDbStageLimits,
    ) -> Result<Self, SourceDbStageError> {
        Self::create_with_format(imports_dir, limits, SourceDbStageFormat::CompatibilityV2)
    }

    /// Start a source-physical stage for schema-2.2 pack production. This is
    /// intentionally distinct from [`Self::create`], whose persisted rows
    /// remain the compatibility projection used by schema 2.1.
    pub fn create_physical_v3(
        imports_dir: impl AsRef<Path>,
        limits: SourceDbStageLimits,
    ) -> Result<Self, SourceDbStageError> {
        Self::create_with_format(imports_dir, limits, SourceDbStageFormat::PhysicalV3)
    }

    fn create_with_format(
        imports_dir: impl AsRef<Path>,
        limits: SourceDbStageLimits,
        format: SourceDbStageFormat,
    ) -> Result<Self, SourceDbStageError> {
        limits.validate()?;
        let imports_dir = imports_dir.as_ref();
        let imports_dir = ensure_private_directory(imports_dir)?;
        let staging_dir = ensure_private_child_directory(&imports_dir, STAGING_DIRECTORY)?;
        let required_free_bytes = limits
            .max_stage_bytes
            .checked_add(limits.minimum_free_bytes)
            .ok_or(SourceDbStageError::StageCapacityOverflow)?;
        let available_free_bytes = available_bytes(&staging_dir.path)?;
        if available_free_bytes < required_free_bytes {
            return Err(SourceDbStageError::InsufficientFreeSpace {
                required: required_free_bytes,
                available: available_free_bytes,
            });
        }

        let file_name = OsString::from(format!("{}.{}", Uuid::new_v4(), STAGE_FILE_EXTENSION));
        let path = staging_dir.path.join(&file_name);
        let (file_descriptor, file_identity) =
            create_private_stage_file(&staging_dir, &file_name, &path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        verify_stage_path_identity(&staging_dir, &file_name, &path, file_identity)?;
        configure_writable_connection(&connection, limits)?;
        initialise_schema(&connection, limits, format)?;
        verify_stage_path_identity(&staging_dir, &file_name, &path, file_identity)?;
        Ok(Self {
            path,
            connection,
            writable: true,
            format,
            file_identity,
            directory: staging_dir,
            file_name,
            file_descriptor,
        })
    }

    /// Reopen a completed snapshot. Open captures are deliberately rejected:
    /// callers must resume/rebuild them with a writer, not treat them as a
    /// source-consistent history.
    pub fn open_sealed(path: impl AsRef<Path>) -> Result<Self, SourceDbStageError> {
        Self::open_sealed_with_hook(path.as_ref(), || {})
    }

    fn open_sealed_with_hook(
        path: &Path,
        before_sqlite_open: impl FnOnce(),
    ) -> Result<Self, SourceDbStageError> {
        let stage_path = ensure_private_stage_path(path)?;
        let path = stage_path.path.clone();
        let (file_descriptor, file_identity) = open_private_stage_file(&stage_path, false)?;
        before_sqlite_open();
        let connection = open_read_only_sqlite_from_descriptor(&file_descriptor)?;
        verify_stage_path_identity(
            &stage_path.directory,
            &stage_path.file_name,
            &path,
            file_identity,
        )?;
        let format = read_stage_format(&connection)?;
        let stage = Self {
            path,
            connection,
            writable: false,
            format,
            file_identity,
            directory: stage_path.directory,
            file_name: stage_path.file_name,
            file_descriptor,
        };
        let stats = stage.stats()?;
        if stats.state != SourceDbStageState::Sealed {
            return Err(SourceDbStageError::StageNotSealed);
        }
        stage.verify_integrity()?;
        stage.verify_accounting(stats)?;
        verify_stage_path_identity(
            &stage.directory,
            &stage.file_name,
            &stage.path,
            file_identity,
        )?;
        Ok(stage)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn verify_path_identity(&self) -> Result<(), SourceDbStageError> {
        let descriptor_identity = validate_private_descriptor(
            self.file_descriptor.as_fd(),
            &self.path,
            FileType::RegularFile,
            0o600,
        )?;
        if descriptor_identity != self.file_identity {
            return Err(SourceDbStageError::StagePathIdentityChanged(
                self.path.clone(),
            ));
        }
        verify_stage_path_identity(
            &self.directory,
            &self.file_name,
            &self.path,
            self.file_identity,
        )
    }

    /// Remove exactly this private staging file. Open captures are never
    /// resumable after a source-session failure; callers use this to discard
    /// their partial snapshot rather than risking mixed PostgreSQL views.
    pub fn discard(self) -> Result<(), SourceDbStageError> {
        self.discard_with_hook(|| {})
    }

    fn discard_with_hook(self, before_unlink: impl FnOnce()) -> Result<(), SourceDbStageError> {
        let Self {
            path,
            connection,
            writable: _,
            format: _,
            file_identity,
            directory,
            file_name,
            file_descriptor,
        } = self;
        drop(connection);
        let descriptor_identity = validate_private_descriptor(
            file_descriptor.as_fd(),
            &path,
            FileType::RegularFile,
            0o600,
        )?;
        if descriptor_identity != file_identity {
            return Err(SourceDbStageError::StagePathIdentityChanged(path));
        }
        before_unlink();
        verify_stage_path_identity(&directory, &file_name, &path, file_identity)?;
        let (unlink_guard, unlink_identity) =
            open_private_stage_file_at(&directory, &file_name, &path, false)?;
        if unlink_identity != file_identity {
            return Err(SourceDbStageError::StagePathIdentityChanged(path));
        }
        crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::StageDiscardUnlink,
        )
        .map_err(SourceDbStageError::Durability)?;
        unlinkat(&directory.descriptor, &file_name, AtFlags::empty()).map_err(|source| {
            SourceDbStageError::RemoveStage {
                path: path.clone(),
                source: std::io::Error::from(source),
            }
        })?;
        drop(unlink_guard);
        crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::StageDiscardDirectoryFsync,
        )
        .map_err(SourceDbStageError::Durability)?;
        fsync(&directory.descriptor).map_err(|source| SourceDbStageError::SyncStageDirectory {
            path: directory.path,
            source,
        })?;
        Ok(())
    }

    pub fn stats(&self) -> Result<SourceDbStageStats, SourceDbStageError> {
        self.verify_path_identity()?;
        let state = SourceDbStageState::parse(&read_meta(&self.connection, META_STATE)?)?;
        let row_count = parse_meta_u64(&self.connection, META_ROW_COUNT)?;
        let payload_bytes = parse_meta_u64(&self.connection, META_PAYLOAD_BYTES)?;
        let limits = SourceDbStageLimits {
            max_rows: parse_meta_u64(&self.connection, META_MAX_ROWS)?,
            max_stage_bytes: parse_meta_u64(&self.connection, META_MAX_STAGE_BYTES)?,
            minimum_free_bytes: parse_meta_u64(&self.connection, META_MINIMUM_FREE_BYTES)?,
        };
        limits.validate()?;
        if row_count > limits.max_rows || payload_bytes > limits.max_stage_bytes {
            return Err(SourceDbStageError::PersistedBoundsExceeded);
        }
        Ok(SourceDbStageStats {
            state,
            row_count,
            payload_bytes,
            limits,
        })
    }

    /// Hash the logical contents of one sealed stage independently of its
    /// SQLite page layout. Every PhysicalV3 table contributes its name, row
    /// count, ordered source IDs, and exact persisted JSON bytes, including
    /// empty tables. Callers bind this digest to the installation/source/
    /// vehicle identity before deriving a stable publication snapshot ID.
    pub(crate) fn sealed_content_digest(&self) -> Result<Sha256Digest, SourceDbStageError> {
        let stats = self.stats()?;
        if stats.state != SourceDbStageState::Sealed {
            return Err(SourceDbStageError::StageNotSealed);
        }
        self.verify_integrity()?;
        self.verify_accounting(stats)?;

        let mut digest = Sha256::new();
        digest.update(crate::runtime::legacy_names::STAGE_LOGICAL_CONTENT_V1.as_bytes());
        digest.update(self.format.as_str().as_bytes());
        digest.update(stats.row_count.to_be_bytes());
        digest.update(stats.payload_bytes.to_be_bytes());
        for table in SourceDbStageTable::PHYSICAL_V3_ALL {
            let table_name = table.as_str().as_bytes();
            digest.update(
                u32::try_from(table_name.len())
                    .expect("fixed table name length fits u32")
                    .to_be_bytes(),
            );
            digest.update(table_name);
            let row_count: i64 = self.connection.query_row(
                "SELECT COUNT(*) FROM stage_rows WHERE table_name = ?1",
                [table.as_str()],
                |row| row.get(0),
            )?;
            digest.update(
                u64::try_from(row_count)
                    .map_err(|_| SourceDbStageError::InvalidStoredAccounting)?
                    .to_be_bytes(),
            );
            let mut statement = self.connection.prepare(
                "SELECT source_id, row_json FROM stage_rows
                  WHERE table_name = ?1 ORDER BY source_id ASC",
            )?;
            let rows = statement.query_map([table.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (source_id, json) = row?;
                digest.update(source_id.to_be_bytes());
                digest.update(
                    u64::try_from(json.len())
                        .map_err(|_| SourceDbStageError::InvalidStoredAccounting)?
                        .to_be_bytes(),
                );
                digest.update(json.as_bytes());
            }
        }
        Ok(Sha256Digest::from_bytes(digest.finalize().into()))
    }

    /// Return the persisted row contract. Stages created before this marker
    /// existed remain compatibility stages, so an upgrade cannot reinterpret
    /// their projected JSON as physical source rows.
    pub fn format(&self) -> Result<SourceDbStageFormat, SourceDbStageError> {
        self.verify_path_identity()?;
        Ok(self.format)
    }

    /// Insert exactly one decoded source row. The value must serialize to a
    /// JSON object, avoiding accidental scalar/blob staging contracts.
    pub fn insert<T: Serialize>(
        &mut self,
        table: SourceDbStageTable,
        source_id: i64,
        value: &T,
    ) -> Result<(), SourceDbStageError> {
        self.insert_page(table, [(source_id, value)])
    }

    /// Commit one decoded PostgreSQL page. This is the capture hot path: a
    /// caller holds at most one source page and this method makes all of that
    /// page durable in one local SQLite transaction. Empty pages are harmless
    /// and do not create a transaction.
    pub fn insert_page<T, I>(
        &mut self,
        table: SourceDbStageTable,
        input: I,
    ) -> Result<(), SourceDbStageError>
    where
        T: Serialize,
        I: IntoIterator<Item = (i64, T)>,
    {
        if !self.writable {
            return Err(SourceDbStageError::StageReadOnly);
        }
        self.require_open()?;
        let mut encoded = Vec::new();
        for (source_id, value) in input {
            if source_id <= 0 {
                return Err(SourceDbStageError::InvalidSourceId);
            }
            if encoded.len() >= usize::try_from(MAX_PAGE_SIZE).expect("u32 fits usize") {
                return Err(SourceDbStageError::CapturePageTooLarge {
                    maximum: MAX_PAGE_SIZE,
                });
            }
            let json = encode_row(&value)?;
            let encoded_bytes = u64::try_from(json.len()).expect("usize always fits u64");
            let encoded_bytes_sql =
                i64::try_from(encoded_bytes).expect("validated byte bound fits i64");
            encoded.push((source_id, json, encoded_bytes, encoded_bytes_sql));
        }
        self.insert_encoded_page(table, encoded)
    }

    /// Parallel variant for the PostgreSQL capture path. Encoding is done in
    /// bounded workers; SQLite insertion remains one ordered transaction.
    pub(crate) fn insert_page_parallel<T, I>(
        &mut self,
        table: SourceDbStageTable,
        input: I,
    ) -> Result<(), SourceDbStageError>
    where
        T: Serialize + Sync,
        I: IntoIterator<Item = (i64, T)>,
    {
        if !self.writable {
            return Err(SourceDbStageError::StageReadOnly);
        }
        self.require_open()?;
        let mut rows = Vec::new();
        for (source_id, value) in input {
            if source_id <= 0 {
                return Err(SourceDbStageError::InvalidSourceId);
            }
            if rows.len() >= usize::try_from(MAX_PAGE_SIZE).expect("u32 fits usize") {
                return Err(SourceDbStageError::CapturePageTooLarge {
                    maximum: MAX_PAGE_SIZE,
                });
            }
            rows.push((source_id, value));
        }
        if rows.is_empty() {
            return Ok(());
        }
        let encoded = encode_rows_parallel(rows, stage_encoding_worker_count())?;
        self.insert_encoded_page(table, encoded)
    }

    /// Commit rows already decoded and JSON-encoded by a PostgreSQL reader
    /// lane. Only this SQLite commit path runs on the coordinator; source
    /// decoding and serialization stay parallel and bounded upstream.
    pub(crate) fn insert_encoded_json_page<I>(
        &mut self,
        table: SourceDbStageTable,
        input: I,
    ) -> Result<(), SourceDbStageError>
    where
        I: IntoIterator<Item = (i64, String)>,
    {
        if !self.writable {
            return Err(SourceDbStageError::StageReadOnly);
        }
        self.require_open()?;
        let mut encoded = Vec::new();
        for (source_id, json) in input {
            if source_id <= 0 {
                return Err(SourceDbStageError::InvalidSourceId);
            }
            if encoded.len() >= usize::try_from(MAX_PAGE_SIZE).expect("u32 fits usize") {
                return Err(SourceDbStageError::CapturePageTooLarge {
                    maximum: MAX_PAGE_SIZE,
                });
            }
            let encoded_bytes = u64::try_from(json.len()).expect("usize always fits u64");
            let encoded_bytes_sql =
                i64::try_from(encoded_bytes).expect("validated byte bound fits i64");
            encoded.push((source_id, json, encoded_bytes, encoded_bytes_sql));
        }
        self.insert_encoded_page(table, encoded)
    }

    fn insert_encoded_page(
        &mut self,
        table: SourceDbStageTable,
        encoded: Vec<(i64, String, u64, i64)>,
    ) -> Result<(), SourceDbStageError> {
        if encoded.is_empty() {
            return Ok(());
        }
        let format = self.format()?;
        if !format.accepts(table) {
            return Err(SourceDbStageError::TableFormatMismatch {
                format: format.as_str(),
                table: table.as_str(),
            });
        }
        let stats = self.stats()?;
        let page_rows = u64::try_from(encoded.len()).expect("usize always fits u64");
        let next_rows =
            stats
                .row_count
                .checked_add(page_rows)
                .ok_or(SourceDbStageError::RowLimitExceeded {
                    maximum: stats.limits.max_rows,
                })?;
        if next_rows > stats.limits.max_rows {
            return Err(SourceDbStageError::RowLimitExceeded {
                maximum: stats.limits.max_rows,
            });
        }
        let page_payload = encoded.iter().try_fold(0_u64, |total, (_, _, bytes, _)| {
            total
                .checked_add(*bytes)
                .ok_or(SourceDbStageError::PayloadByteLimitExceeded {
                    maximum: stats.limits.max_stage_bytes,
                })
        })?;
        let next_payload = stats.payload_bytes.checked_add(page_payload).ok_or(
            SourceDbStageError::PayloadByteLimitExceeded {
                maximum: stats.limits.max_stage_bytes,
            },
        )?;
        if next_payload > stats.limits.max_stage_bytes {
            return Err(SourceDbStageError::PayloadByteLimitExceeded {
                maximum: stats.limits.max_stage_bytes,
            });
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare_cached(
            "INSERT INTO stage_rows(table_name, source_id, row_json, encoded_bytes) \
             VALUES(?1, ?2, ?3, ?4)",
        )?;
        for (source_id, json, _, encoded_bytes_sql) in encoded {
            match statement.execute(params![table.as_str(), source_id, json, encoded_bytes_sql]) {
                Ok(_) => {}
                Err(error) if is_unique_violation(&error) => {
                    return Err(SourceDbStageError::DuplicateSourceId {
                        table: table.as_str(),
                        source_id,
                    });
                }
                Err(error) => {
                    return Err(map_write_error(error, stats.limits.max_stage_bytes));
                }
            }
        }
        drop(statement);
        write_meta(&transaction, META_ROW_COUNT, next_rows)?;
        write_meta(&transaction, META_PAYLOAD_BYTES, next_payload)?;
        crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::StagePageCommit,
        )
        .map_err(SourceDbStageError::Durability)?;
        transaction
            .commit()
            .map_err(|error| map_write_error(error, stats.limits.max_stage_bytes))?;
        Ok(())
    }

    /// Seal an open snapshot after a full SQLite integrity check. Sealed data
    /// cannot be mutated through this API, and the reopened reader is SQLite
    /// read-only.
    pub fn seal(&mut self) -> Result<SourceDbStageStats, SourceDbStageError> {
        if !self.writable {
            return Err(SourceDbStageError::StageReadOnly);
        }
        self.require_open()?;
        self.verify_accounting(self.stats()?)?;
        // Run the full integrity check once, after the sealed-state metadata
        // is committed. A pre-write check duplicated the same full scan and
        // added minutes to large migrations without changing the final
        // validity guarantee.
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        write_meta(
            &transaction,
            META_STATE,
            SourceDbStageState::Sealed.as_str(),
        )?;
        crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::StageSealCommit,
        )
        .map_err(SourceDbStageError::Durability)?;
        transaction.commit()?;
        self.verify_integrity()?;
        self.stats()
    }

    /// Run SQLite's complete integrity check. This is intentionally not only a
    /// quick check because a sealed migration may be retained for a long time.
    pub fn verify_integrity(&self) -> Result<(), SourceDbStageError> {
        let mut statement = self.connection.prepare("PRAGMA integrity_check")?;
        let values = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if values.len() == 1 && values.first().is_some_and(|value| value == "ok") {
            return Ok(());
        }
        Err(SourceDbStageError::IntegrityCheckFailed(values.join("; ")))
    }

    fn verify_accounting(&self, expected: SourceDbStageStats) -> Result<(), SourceDbStageError> {
        let (row_count, payload_bytes): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(encoded_bytes), 0) FROM stage_rows",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let actual_rows =
            u64::try_from(row_count).map_err(|_| SourceDbStageError::InvalidStoredAccounting)?;
        let actual_payload = u64::try_from(payload_bytes)
            .map_err(|_| SourceDbStageError::InvalidStoredAccounting)?;
        if actual_rows != expected.row_count || actual_payload != expected.payload_bytes {
            return Err(SourceDbStageError::PersistedAccountingMismatch {
                expected_rows: expected.row_count,
                actual_rows,
                expected_payload_bytes: expected.payload_bytes,
                actual_payload_bytes: actual_payload,
            });
        }
        if actual_rows > expected.limits.max_rows
            || actual_payload > expected.limits.max_stage_bytes
        {
            return Err(SourceDbStageError::PersistedBoundsExceeded);
        }
        Ok(())
    }

    /// Retrieve a bounded typed page from a sealed stage using a fixed keyset
    /// query. It never constructs a whole source history in memory.
    pub fn page<T: DeserializeOwned>(
        &self,
        table: SourceDbStageTable,
        after_id: i64,
        limit: u32,
    ) -> Result<SourceDbStagePage<T>, SourceDbStageError> {
        if after_id < 0 {
            return Err(SourceDbStageError::InvalidPageCursor);
        }
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(SourceDbStageError::InvalidPageSize);
        }
        if self.stats()?.state != SourceDbStageState::Sealed {
            return Err(SourceDbStageError::StageNotSealed);
        }
        let query_limit = i64::from(limit) + 1;
        let mut statement = self.connection.prepare(
            "SELECT source_id, row_json
             FROM stage_rows
             WHERE table_name = ?1 AND source_id > ?2
             ORDER BY source_id ASC
             LIMIT ?3",
        )?;
        let mut output = statement
            .query_map(params![table.as_str(), after_id, query_limit], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (source_id, json) = row?;
                let value = serde_json::from_str(&json).map_err(|source| {
                    SourceDbStageError::StoredRowDecode {
                        table: table.as_str(),
                        source_id,
                        source,
                    }
                })?;
                Ok(SourceDbStageRow { source_id, value })
            })
            .collect::<Result<Vec<_>, SourceDbStageError>>()?;
        let next_after_id = if output.len() > usize::try_from(limit).expect("u32 fits usize") {
            output.pop();
            output.last().map(|row| row.source_id)
        } else {
            None
        };
        Ok(SourceDbStagePage {
            rows: output,
            next_after_id,
        })
    }

    /// Fetch one fixed-table source row from a sealed capture. It is used by
    /// the fragment writer for parent and endpoint relations, so it never
    /// needs to materialize another whole stage table.
    pub fn get<T: DeserializeOwned>(
        &self,
        table: SourceDbStageTable,
        source_id: i64,
    ) -> Result<Option<T>, SourceDbStageError> {
        if source_id <= 0 {
            return Err(SourceDbStageError::InvalidSourceId);
        }
        if self.stats()?.state != SourceDbStageState::Sealed {
            return Err(SourceDbStageError::StageNotSealed);
        }
        let json = self
            .connection
            .query_row(
                "SELECT row_json FROM stage_rows WHERE table_name = ?1 AND source_id = ?2",
                params![table.as_str(), source_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|json| {
            serde_json::from_str(&json).map_err(|source| SourceDbStageError::StoredRowDecode {
                table: table.as_str(),
                source_id,
                source,
            })
        })
        .transpose()
    }

    /// Page charge samples for exactly one staged charging process. The field
    /// name and table are fixed here, so raw source data can never influence a
    /// SQL identifier or JSON path.
    pub fn charge_samples_for_process<T: DeserializeOwned>(
        &self,
        charging_process_id: i64,
        after_id: i64,
        limit: u32,
    ) -> Result<SourceDbStagePage<T>, SourceDbStageError> {
        if charging_process_id <= 0 {
            return Err(SourceDbStageError::InvalidSourceId);
        }
        self.page_charge_samples(charging_process_id, after_id, limit)
    }

    fn page_charge_samples<T: DeserializeOwned>(
        &self,
        value: i64,
        after_id: i64,
        limit: u32,
    ) -> Result<SourceDbStagePage<T>, SourceDbStageError> {
        if after_id < 0 {
            return Err(SourceDbStageError::InvalidPageCursor);
        }
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(SourceDbStageError::InvalidPageSize);
        }
        if self.stats()?.state != SourceDbStageState::Sealed {
            return Err(SourceDbStageError::StageNotSealed);
        }
        let query_limit = i64::from(limit) + 1;
        let mut statement = self.connection.prepare(CHARGE_SAMPLES_PAGE_SQL)?;
        let mut output = statement
            .query_map(params![value, after_id, query_limit], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (source_id, json) = row?;
                let value = serde_json::from_str(&json).map_err(|source| {
                    SourceDbStageError::StoredRowDecode {
                        table: SourceDbStageTable::Charges.as_str(),
                        source_id,
                        source,
                    }
                })?;
                Ok(SourceDbStageRow { source_id, value })
            })
            .collect::<Result<Vec<_>, SourceDbStageError>>()?;
        let next_after_id = if output.len() > usize::try_from(limit).expect("u32 fits usize") {
            output.pop();
            output.last().map(|row| row.source_id)
        } else {
            None
        };
        Ok(SourceDbStagePage {
            rows: output,
            next_after_id,
        })
    }

    fn require_open(&self) -> Result<(), SourceDbStageError> {
        match self.stats()?.state {
            SourceDbStageState::Open => Ok(()),
            SourceDbStageState::Sealed => Err(SourceDbStageError::StageSealed),
        }
    }
}

fn configure_writable_connection(
    connection: &Connection,
    limits: SourceDbStageLimits,
) -> Result<(), SourceDbStageError> {
    connection.execute_batch(
        "PRAGMA journal_mode=DELETE;
         PRAGMA synchronous=FULL;
         PRAGMA foreign_keys=ON;",
    )?;
    // A known page size turns the persisted page cap into a strict upper bound
    // on the main stage database allocation.
    connection.execute_batch("PRAGMA page_size=4096")?;
    let page_limit = i64::try_from(limits.max_stage_bytes / 4096).expect("validated page limit");
    connection.pragma_update(None, "max_page_count", page_limit)?;
    Ok(())
}

fn open_read_only_sqlite_from_descriptor(
    descriptor: &OwnedFd,
) -> Result<Connection, rusqlite::Error> {
    // Sealed stages are immutable, so SQLite needs no journal beside /dev/fd.
    // Opening this URI duplicates the already admitted descriptor; replacing
    // the stage pathname cannot redirect SQLite to a different inode.
    let uri = format!(
        "file:/dev/fd/{}?mode=ro&immutable=1",
        descriptor.as_raw_fd()
    );
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
}

fn initialise_schema(
    connection: &Connection,
    limits: SourceDbStageLimits,
    format: SourceDbStageFormat,
) -> Result<(), SourceDbStageError> {
    connection.execute_batch(
        "CREATE TABLE stage_meta(
             key TEXT PRIMARY KEY NOT NULL,
             value TEXT NOT NULL
         ) STRICT;
         CREATE TABLE stage_rows(
             table_name TEXT NOT NULL,
             source_id INTEGER NOT NULL CHECK(source_id > 0),
             row_json TEXT NOT NULL CHECK(json_valid(row_json)),
             encoded_bytes INTEGER NOT NULL CHECK(encoded_bytes >= 0),
             PRIMARY KEY(table_name, source_id),
             CHECK(table_name IN (
                 'global_settings', 'car_settings', 'cars', 'drives',
                 'positions', 'charging_processes', 'charges',
                 'addresses', 'geofences', 'states', 'updates'
             ))
         ) STRICT, WITHOUT ROWID;",
    )?;
    connection.execute_batch(
        "CREATE INDEX stage_charge_samples_by_process
         ON stage_rows(json_extract(row_json, '$.charging_process_id'), source_id)
         WHERE table_name = 'charges';",
    )?;
    let transaction = connection.unchecked_transaction()?;
    for (key, value) in [
        (META_STATE, SourceDbStageState::Open.as_str().to_owned()),
        (META_ROW_COUNT, "0".to_owned()),
        (META_PAYLOAD_BYTES, "0".to_owned()),
        (META_FORMAT, format.as_str().to_owned()),
        (META_MAX_ROWS, limits.max_rows.to_string()),
        (META_MAX_STAGE_BYTES, limits.max_stage_bytes.to_string()),
        (
            META_MINIMUM_FREE_BYTES,
            limits.minimum_free_bytes.to_string(),
        ),
    ] {
        write_meta(&transaction, key, value)?;
    }
    crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::StageSchemaCommit,
    )
    .map_err(SourceDbStageError::Durability)?;
    transaction.commit()?;

    let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let max_pages = i64::try_from(limits.max_stage_bytes / 4096).expect("validated page limit");
    if page_count > max_pages {
        return Err(SourceDbStageError::StageLimitTooSmall {
            minimum: u64::try_from(page_count).expect("page count non-negative") * 4096,
        });
    }
    Ok(())
}

fn stage_encoding_worker_count() -> usize {
    let available = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    stage_encoding_worker_count_for(available)
}

fn stage_encoding_worker_count_for(available: usize) -> usize {
    available.clamp(1, MAX_ENCODING_WORKERS)
}

fn encode_rows_parallel<T: Serialize + Sync>(
    rows: Vec<(i64, T)>,
    requested_workers: usize,
) -> Result<Vec<(i64, String, u64, i64)>, SourceDbStageError> {
    let worker_count = requested_workers.clamp(1, rows.len());
    let chunk_size = rows.len().div_ceil(worker_count);
    std::thread::scope(|scope| {
        let handles = rows
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|(source_id, value)| encode_row(value).map(|json| (*source_id, json)))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let mut encoded = Vec::with_capacity(rows.len());
        for handle in handles {
            let worker_rows = handle
                .join()
                .map_err(|_| SourceDbStageError::EncodingWorkerPanicked)?;
            for row in worker_rows {
                let (source_id, json) = row?;
                let encoded_bytes = u64::try_from(json.len()).expect("usize always fits u64");
                let encoded_bytes_sql =
                    i64::try_from(encoded_bytes).expect("validated byte bound fits i64");
                encoded.push((source_id, json, encoded_bytes, encoded_bytes_sql));
            }
        }
        Ok(encoded)
    })
}

fn encode_row<T: Serialize>(value: &T) -> Result<String, SourceDbStageError> {
    let value = serde_json::to_value(value).map_err(SourceDbStageError::SerializeRow)?;
    if !matches!(value, Value::Object(_)) {
        return Err(SourceDbStageError::RowMustBeJsonObject);
    }
    serde_json::to_string(&value).map_err(SourceDbStageError::SerializeRow)
}

fn read_meta(connection: &Connection, key: &'static str) -> Result<String, SourceDbStageError> {
    connection
        .query_row(
            "SELECT value FROM stage_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(SourceDbStageError::MissingMetadata(key))
}

fn read_stage_format(connection: &Connection) -> Result<SourceDbStageFormat, SourceDbStageError> {
    let value = connection
        .query_row(
            "SELECT value FROM stage_meta WHERE key = ?1",
            params![META_FORMAT],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    value
        .as_deref()
        .map(SourceDbStageFormat::parse)
        .transpose()
        .map(|value| value.unwrap_or(SourceDbStageFormat::CompatibilityV2))
}

fn parse_meta_u64(connection: &Connection, key: &'static str) -> Result<u64, SourceDbStageError> {
    read_meta(connection, key)?
        .parse()
        .map_err(|_| SourceDbStageError::InvalidMetadata(key))
}

fn write_meta(
    transaction: &rusqlite::Transaction<'_>,
    key: &'static str,
    value: impl ToString,
) -> Result<(), SourceDbStageError> {
    transaction.execute(
        "INSERT INTO stage_meta(key, value) VALUES(?2, ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![value.to_string(), key],
    )?;
    Ok(())
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _) if code.code == ErrorCode::ConstraintViolation
    )
}

fn map_write_error(error: rusqlite::Error, maximum: u64) -> SourceDbStageError {
    if matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _) if code.code == ErrorCode::DiskFull
    ) {
        SourceDbStageError::DatabaseByteLimitExceeded { maximum }
    } else {
        SourceDbStageError::Sqlite(error)
    }
}

fn ensure_private_stage_path(path: &Path) -> Result<PrivateStagePath, SourceDbStageError> {
    if path.extension().and_then(|extension| extension.to_str()) != Some(STAGE_FILE_EXTENSION) {
        return Err(SourceDbStageError::InvalidStagePath);
    }
    let parent = path.parent().ok_or(SourceDbStageError::InvalidStagePath)?;
    if parent.file_name().and_then(|name| name.to_str()) != Some(STAGING_DIRECTORY) {
        return Err(SourceDbStageError::InvalidStagePath);
    }
    let imports_dir = parent
        .parent()
        .ok_or(SourceDbStageError::InvalidStagePath)?;
    let imports_dir = open_private_directory(imports_dir)?;
    let staging_dir = open_private_child_directory(&imports_dir, STAGING_DIRECTORY)?;
    let file_name = path
        .file_name()
        .ok_or(SourceDbStageError::InvalidStagePath)?
        .to_os_string();
    let path = staging_dir.path.join(&file_name);
    Ok(PrivateStagePath {
        directory: staging_dir,
        file_name,
        path,
    })
}

fn ensure_private_directory(path: &Path) -> Result<PrivateDirectory, SourceDbStageError> {
    private_directory(path, true)
}

fn open_private_directory(path: &Path) -> Result<PrivateDirectory, SourceDbStageError> {
    private_directory(path, false)
}

fn private_directory(path: &Path, create: bool) -> Result<PrivateDirectory, SourceDbStageError> {
    let absolute = absolute_stage_path(path)?;
    let name = absolute
        .file_name()
        .ok_or(SourceDbStageError::InvalidStagePath)?;
    let parent_path = absolute
        .parent()
        .ok_or(SourceDbStageError::InvalidStagePath)?;
    let parent = open_directory_components(parent_path)?;
    let created = if create {
        match mkdirat(&parent, name, Mode::from_raw_mode(0o700)) {
            Ok(()) => true,
            Err(rustix::io::Errno::EXIST) => false,
            Err(source) => {
                return Err(SourceDbStageError::SecureCreateDirectory {
                    path: absolute,
                    source,
                });
            }
        }
    } else {
        false
    };
    let directory = openat(
        &parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| SourceDbStageError::SecureOpen {
        path: absolute.clone(),
        source,
    })?;
    if created {
        fchmod(&directory, Mode::from_raw_mode(0o700)).map_err(|source| {
            SourceDbStageError::SecurePermissions {
                path: absolute.clone(),
                source,
            }
        })?;
    }
    finish_private_directory(directory, &absolute)
}

fn absolute_stage_path(path: &Path) -> Result<PathBuf, SourceDbStageError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|source| SourceDbStageError::InspectPath {
                path: path.to_path_buf(),
                source,
            })?
    };
    #[cfg(target_os = "macos")]
    if let Ok(suffix) = absolute.strip_prefix("/var") {
        return Ok(Path::new("/private/var").join(suffix));
    }
    #[cfg(target_os = "macos")]
    if let Ok(suffix) = absolute.strip_prefix("/tmp") {
        return Ok(Path::new("/private/tmp").join(suffix));
    }
    #[cfg(target_os = "macos")]
    if let Ok(suffix) = absolute.strip_prefix("/etc") {
        return Ok(Path::new("/private/etc").join(suffix));
    }
    Ok(absolute)
}

fn open_directory_components(path: &Path) -> Result<OwnedFd, SourceDbStageError> {
    let mut descriptor = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| SourceDbStageError::SecureOpen {
        path: PathBuf::from("/"),
        source,
    })?;
    let mut traversed = PathBuf::from("/");
    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(SourceDbStageError::InvalidStagePath);
        };
        traversed.push(name);
        descriptor = openat(
            &descriptor,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|source| SourceDbStageError::SecureOpen {
            path: traversed.clone(),
            source,
        })?;
    }
    Ok(descriptor)
}

fn ensure_private_child_directory(
    parent: &PrivateDirectory,
    name: &str,
) -> Result<PrivateDirectory, SourceDbStageError> {
    private_child_directory(parent, name, true)
}

fn open_private_child_directory(
    parent: &PrivateDirectory,
    name: &str,
) -> Result<PrivateDirectory, SourceDbStageError> {
    private_child_directory(parent, name, false)
}

fn private_child_directory(
    parent: &PrivateDirectory,
    name: &str,
    create: bool,
) -> Result<PrivateDirectory, SourceDbStageError> {
    let path = parent.path.join(name);
    let created = if create {
        match mkdirat(&parent.descriptor, name, Mode::from_raw_mode(0o700)) {
            Ok(()) => true,
            Err(rustix::io::Errno::EXIST) => false,
            Err(source) => {
                return Err(SourceDbStageError::SecureCreateDirectory { path, source });
            }
        }
    } else {
        false
    };
    if !created {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(SourceDbStageError::SymlinkPath(path));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(SourceDbStageError::ExpectedDirectory(path));
            }
            Ok(_) => {}
            Err(source) => {
                return Err(SourceDbStageError::InspectPath { path, source });
            }
        }
    }
    let directory = openat(
        &parent.descriptor,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| SourceDbStageError::SecureOpen {
        path: path.clone(),
        source,
    })?;
    if created {
        fchmod(&directory, Mode::from_raw_mode(0o700)).map_err(|source| {
            SourceDbStageError::SecurePermissions {
                path: path.clone(),
                source,
            }
        })?;
    }
    finish_private_directory(directory, &path)
}

fn finish_private_directory(
    directory: OwnedFd,
    path: &Path,
) -> Result<PrivateDirectory, SourceDbStageError> {
    let identity =
        validate_private_descriptor(directory.as_fd(), path, FileType::Directory, 0o700)?;
    Ok(PrivateDirectory {
        descriptor: directory,
        identity,
        path: path.to_path_buf(),
    })
}

fn create_private_stage_file(
    directory: &PrivateDirectory,
    file_name: &OsStr,
    path: &Path,
) -> Result<(OwnedFd, StageFileIdentity), SourceDbStageError> {
    let file = openat(
        &directory.descriptor,
        file_name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|source| SourceDbStageError::SecureOpen {
        path: path.to_path_buf(),
        source,
    })?;
    fchmod(&file, Mode::from_raw_mode(0o600)).map_err(|source| {
        SourceDbStageError::SecurePermissions {
            path: path.to_path_buf(),
            source,
        }
    })?;
    let identity = validate_private_descriptor(file.as_fd(), path, FileType::RegularFile, 0o600)?;
    verify_stage_path_identity(directory, file_name, path, identity)?;
    Ok((file, identity))
}

fn open_private_stage_file(
    stage_path: &PrivateStagePath,
    writable: bool,
) -> Result<(OwnedFd, StageFileIdentity), SourceDbStageError> {
    open_private_stage_file_at(
        &stage_path.directory,
        &stage_path.file_name,
        &stage_path.path,
        writable,
    )
}

fn open_private_stage_file_at(
    directory: &PrivateDirectory,
    file_name: &OsStr,
    path: &Path,
    writable: bool,
) -> Result<(OwnedFd, StageFileIdentity), SourceDbStageError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| SourceDbStageError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.file_type().is_symlink() {
        return Err(SourceDbStageError::SymlinkPath(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(SourceDbStageError::ExpectedFile(path.to_path_buf()));
    }
    let access = if writable {
        OFlags::RDWR
    } else {
        OFlags::RDONLY
    };
    let file = openat(
        &directory.descriptor,
        file_name,
        access | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|source| SourceDbStageError::SecureOpen {
        path: path.to_path_buf(),
        source,
    })?;
    let identity = validate_private_descriptor(file.as_fd(), path, FileType::RegularFile, 0o600)?;
    Ok((file, identity))
}

fn validate_private_descriptor(
    descriptor: impl AsFd,
    path: &Path,
    expected_type: FileType,
    expected_mode: u32,
) -> Result<StageFileIdentity, SourceDbStageError> {
    let stat = fstat(descriptor).map_err(|source| SourceDbStageError::SecureInspect {
        path: path.to_path_buf(),
        source,
    })?;
    let actual_type = FileType::from_raw_mode(stat.st_mode);
    if actual_type != expected_type {
        return if expected_type == FileType::Directory {
            Err(SourceDbStageError::ExpectedDirectory(path.to_path_buf()))
        } else {
            Err(SourceDbStageError::ExpectedFile(path.to_path_buf()))
        };
    }
    if expected_type == FileType::RegularFile && stat.st_nlink != 1 {
        #[allow(clippy::useless_conversion)]
        let actual = u64::from(stat.st_nlink);
        return Err(SourceDbStageError::UnexpectedLinkCount {
            path: path.to_path_buf(),
            actual,
        });
    }
    let expected_uid = geteuid().as_raw();
    let expected_gid = getegid().as_raw();
    if stat.st_uid != expected_uid || stat.st_gid != expected_gid {
        return Err(SourceDbStageError::UnexpectedOwner {
            path: path.to_path_buf(),
            expected_uid,
            expected_gid,
            actual_uid: stat.st_uid,
            actual_gid: stat.st_gid,
        });
    }
    #[allow(clippy::useless_conversion)]
    let actual_mode = u32::from(Mode::from_raw_mode(stat.st_mode).as_raw_mode());
    if actual_mode != expected_mode {
        return Err(SourceDbStageError::InsecurePermissions {
            path: path.to_path_buf(),
            expected: expected_mode,
            actual: actual_mode,
        });
    }
    Ok(StageFileIdentity {
        #[allow(clippy::useless_conversion)]
        device: u64::try_from(stat.st_dev).expect("filesystem device identifier fits u64"),
        inode: stat.st_ino,
    })
}

fn verify_stage_path_identity(
    directory: &PrivateDirectory,
    file_name: &OsStr,
    path: &Path,
    expected: StageFileIdentity,
) -> Result<(), SourceDbStageError> {
    if directory.identity
        != validate_private_descriptor(
            directory.descriptor.as_fd(),
            &directory.path,
            FileType::Directory,
            0o700,
        )?
    {
        return Err(SourceDbStageError::DirectoryIdentityChanged(
            directory.path.clone(),
        ));
    }
    let current_directory_descriptor = open_directory_components(&directory.path)?;
    let current_directory_identity = validate_private_descriptor(
        current_directory_descriptor.as_fd(),
        &directory.path,
        FileType::Directory,
        0o700,
    )?;
    if current_directory_identity != directory.identity {
        return Err(SourceDbStageError::DirectoryIdentityChanged(
            directory.path.clone(),
        ));
    }
    let current_directory = PrivateDirectory {
        descriptor: current_directory_descriptor,
        identity: current_directory_identity,
        path: directory.path.clone(),
    };
    let (file, actual) = open_private_stage_file_at(&current_directory, file_name, path, false)?;
    drop(file);
    if actual != expected {
        return Err(SourceDbStageError::StagePathIdentityChanged(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

fn available_bytes(path: &Path) -> Result<u64, SourceDbStageError> {
    let stats = statvfs(path).map_err(|source| SourceDbStageError::FilesystemSpace {
        path: path.to_path_buf(),
        source,
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(SourceDbStageError::FilesystemSpaceOverflow)
}

#[derive(Debug, Error)]
pub enum SourceDbStageError {
    #[error("stage maximum rows must be positive")]
    InvalidMaximumRows,
    #[error("stage maximum bytes must be at least {minimum}")]
    InvalidMaximumStageBytes { minimum: u64 },
    #[error("stage capacity calculation overflowed")]
    StageCapacityOverflow,
    #[error("could not inspect free space for {path}: {source}")]
    FilesystemSpace {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("filesystem free-space calculation overflowed")]
    FilesystemSpaceOverflow,
    #[error("stage needs {required} free bytes but only {available} are available")]
    InsufficientFreeSpace { required: u64, available: u64 },
    #[error("could not create private stage directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not create private stage directory {path} relative to its owner: {source}")]
    SecureCreateDirectory {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("could not inspect stage path {path}: {source}")]
    InspectPath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not securely open stage path {path}: {source}")]
    SecureOpen {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("could not inspect securely opened stage path {path}: {source}")]
    SecureInspect {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("could not set new private stage permissions on {path}: {source}")]
    SecurePermissions {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("could not remove private stage {path}: {source}")]
    RemoveStage {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not durably sync private stage directory {path}: {source}")]
    SyncStageDirectory {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("stage durability checkpoint failed: {0}")]
    Durability(#[source] std::io::Error),
    #[error("stage path may not be a symlink: {0}")]
    SymlinkPath(PathBuf),
    #[error("expected stage directory at {0}")]
    ExpectedDirectory(PathBuf),
    #[error("expected stage file at {0}")]
    ExpectedFile(PathBuf),
    #[error("stage file {path} must have exactly one hard link, not {actual}")]
    UnexpectedLinkCount { path: PathBuf, actual: u64 },
    #[error(
        "stage path {path} must be owned by uid {expected_uid} gid {expected_gid}, not uid {actual_uid} gid {actual_gid}"
    )]
    UnexpectedOwner {
        path: PathBuf,
        expected_uid: u32,
        expected_gid: u32,
        actual_uid: u32,
        actual_gid: u32,
    },
    #[error("stage path {path} must have mode {expected:o}, not {actual:o}")]
    InsecurePermissions {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    #[error("stage path identity changed while it was being opened: {0}")]
    StagePathIdentityChanged(PathBuf),
    #[error("stage directory identity changed while it was being opened: {0}")]
    DirectoryIdentityChanged(PathBuf),
    #[error("stage path must be a .sqlite file inside a private .staging directory")]
    InvalidStagePath,
    #[error("stage database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stage needs at least {minimum} bytes for its SQLite schema")]
    StageLimitTooSmall { minimum: u64 },
    #[error("stage state is invalid: {0}")]
    InvalidPersistedState(String),
    #[error("stage format is invalid: {0}")]
    InvalidPersistedFormat(String),
    #[error("stage format {format} cannot contain {table} rows")]
    TableFormatMismatch {
        format: &'static str,
        table: &'static str,
    },
    #[error("stage metadata is missing {0}")]
    MissingMetadata(&'static str),
    #[error("stage metadata is invalid for {0}")]
    InvalidMetadata(&'static str),
    #[error("persisted stage accounting exceeds its saved bounds")]
    PersistedBoundsExceeded,
    #[error("persisted stage accounting is not a non-negative SQLite integer")]
    InvalidStoredAccounting,
    #[error(
        "persisted stage accounting does not match rows (expected {expected_rows}/{expected_payload_bytes}, actual {actual_rows}/{actual_payload_bytes})"
    )]
    PersistedAccountingMismatch {
        expected_rows: u64,
        actual_rows: u64,
        expected_payload_bytes: u64,
        actual_payload_bytes: u64,
    },
    #[error("stage is not sealed")]
    StageNotSealed,
    #[error("stage is sealed and cannot accept more rows")]
    StageSealed,
    #[error("reopened sealed stage is read-only")]
    StageReadOnly,
    #[error("source row id must be positive")]
    InvalidSourceId,
    #[error("capture page may contain at most {maximum} rows")]
    CapturePageTooLarge { maximum: u32 },
    #[error("stage row limit exceeded ({maximum})")]
    RowLimitExceeded { maximum: u64 },
    #[error("stage JSON payload limit exceeded ({maximum} bytes)")]
    PayloadByteLimitExceeded { maximum: u64 },
    #[error("stage database byte limit exceeded ({maximum} bytes)")]
    DatabaseByteLimitExceeded { maximum: u64 },
    #[error("stage already contains {table} source id {source_id}")]
    DuplicateSourceId { table: &'static str, source_id: i64 },
    #[error("stage row must serialize to a JSON object")]
    RowMustBeJsonObject,
    #[error("could not serialize stage row: {0}")]
    SerializeRow(serde_json::Error),
    #[error("stage encoding worker panicked")]
    EncodingWorkerPanicked,
    #[error("could not decode stored {table} row {source_id}: {source}")]
    StoredRowDecode {
        table: &'static str,
        source_id: i64,
        source: serde_json::Error,
    },
    #[error("stage integrity check failed: {0}")]
    IntegrityCheckFailed(String),
    #[error("page cursor must be non-negative")]
    InvalidPageCursor,
    #[error("page limit must be between 1 and {MAX_PAGE_SIZE}")]
    InvalidPageSize,
}

#[cfg(test)]
#[path = "stage/tests.rs"]
mod tests;

#[cfg(test)]
mod n1_src_import_source_db_stage_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for SourceDbStageError {
        fn error_example() -> Self {
            SourceDbStageError::InvalidMaximumRows
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_source_db_stage_error(error: SourceDbStageError) {
            match &error {
                SourceDbStageError::InvalidMaximumRows => (),
                SourceDbStageError::InvalidMaximumStageBytes { .. } => (),
                SourceDbStageError::StageCapacityOverflow => (),
                SourceDbStageError::FilesystemSpace { .. } => (),
                SourceDbStageError::FilesystemSpaceOverflow => (),
                SourceDbStageError::InsufficientFreeSpace { .. } => (),
                SourceDbStageError::CreateDirectory { .. } => (),
                SourceDbStageError::SecureCreateDirectory { .. } => (),
                SourceDbStageError::InspectPath { .. } => (),
                SourceDbStageError::SecureOpen { .. } => (),
                SourceDbStageError::SecureInspect { .. } => (),
                SourceDbStageError::SecurePermissions { .. } => (),
                SourceDbStageError::RemoveStage { .. } => (),
                SourceDbStageError::SyncStageDirectory { .. } => (),
                SourceDbStageError::Durability(..) => (),
                SourceDbStageError::SymlinkPath(..) => (),
                SourceDbStageError::ExpectedDirectory(..) => (),
                SourceDbStageError::ExpectedFile(..) => (),
                SourceDbStageError::UnexpectedLinkCount { .. } => (),
                SourceDbStageError::UnexpectedOwner { .. } => (),
                SourceDbStageError::InsecurePermissions { .. } => (),
                SourceDbStageError::StagePathIdentityChanged(..) => (),
                SourceDbStageError::DirectoryIdentityChanged(..) => (),
                SourceDbStageError::InvalidStagePath => (),
                SourceDbStageError::Sqlite(..) => (),
                SourceDbStageError::StageLimitTooSmall { .. } => (),
                SourceDbStageError::InvalidPersistedState(..) => (),
                SourceDbStageError::InvalidPersistedFormat(..) => (),
                SourceDbStageError::TableFormatMismatch { .. } => (),
                SourceDbStageError::MissingMetadata(..) => (),
                SourceDbStageError::InvalidMetadata(..) => (),
                SourceDbStageError::PersistedBoundsExceeded => (),
                SourceDbStageError::InvalidStoredAccounting => (),
                SourceDbStageError::PersistedAccountingMismatch { .. } => (),
                SourceDbStageError::StageNotSealed => (),
                SourceDbStageError::StageSealed => (),
                SourceDbStageError::StageReadOnly => (),
                SourceDbStageError::InvalidSourceId => (),
                SourceDbStageError::CapturePageTooLarge { .. } => (),
                SourceDbStageError::RowLimitExceeded { .. } => (),
                SourceDbStageError::PayloadByteLimitExceeded { .. } => (),
                SourceDbStageError::DatabaseByteLimitExceeded { .. } => (),
                SourceDbStageError::DuplicateSourceId { .. } => (),
                SourceDbStageError::RowMustBeJsonObject => (),
                SourceDbStageError::SerializeRow(..) => (),
                SourceDbStageError::EncodingWorkerPanicked => (),
                SourceDbStageError::StoredRowDecode { .. } => (),
                SourceDbStageError::IntegrityCheckFailed(..) => (),
                SourceDbStageError::InvalidPageCursor => (),
                SourceDbStageError::InvalidPageSize => (),
            }
            assert_name_free(&error);
        }
        check_source_db_stage_error(SourceDbStageError::InvalidMaximumRows);
        check_source_db_stage_error(SourceDbStageError::InvalidMaximumStageBytes {
            minimum: example(),
        });
        check_source_db_stage_error(SourceDbStageError::StageCapacityOverflow);
        check_source_db_stage_error(SourceDbStageError::FilesystemSpace {
            path: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::FilesystemSpaceOverflow);
        check_source_db_stage_error(SourceDbStageError::InsufficientFreeSpace {
            required: example(),
            available: example(),
        });
        check_source_db_stage_error(SourceDbStageError::CreateDirectory {
            path: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::SecureCreateDirectory {
            path: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::InspectPath {
            path: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::SecureOpen {
            path: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::SecureInspect {
            path: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::SecurePermissions {
            path: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::RemoveStage {
            path: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::SyncStageDirectory {
            path: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::Durability(example()));
        check_source_db_stage_error(SourceDbStageError::SymlinkPath(example()));
        check_source_db_stage_error(SourceDbStageError::ExpectedDirectory(example()));
        check_source_db_stage_error(SourceDbStageError::ExpectedFile(example()));
        check_source_db_stage_error(SourceDbStageError::UnexpectedLinkCount {
            path: example(),
            actual: example(),
        });
        check_source_db_stage_error(SourceDbStageError::UnexpectedOwner {
            path: example(),
            expected_uid: example(),
            expected_gid: example(),
            actual_uid: example(),
            actual_gid: example(),
        });
        check_source_db_stage_error(SourceDbStageError::InsecurePermissions {
            path: example(),
            expected: example(),
            actual: example(),
        });
        check_source_db_stage_error(SourceDbStageError::StagePathIdentityChanged(example()));
        check_source_db_stage_error(SourceDbStageError::DirectoryIdentityChanged(example()));
        check_source_db_stage_error(SourceDbStageError::InvalidStagePath);
        check_source_db_stage_error(SourceDbStageError::Sqlite(example()));
        check_source_db_stage_error(SourceDbStageError::StageLimitTooSmall { minimum: example() });
        check_source_db_stage_error(SourceDbStageError::InvalidPersistedState(example()));
        check_source_db_stage_error(SourceDbStageError::InvalidPersistedFormat(example()));
        check_source_db_stage_error(SourceDbStageError::TableFormatMismatch {
            format: example(),
            table: example(),
        });
        check_source_db_stage_error(SourceDbStageError::MissingMetadata(example()));
        check_source_db_stage_error(SourceDbStageError::InvalidMetadata(example()));
        check_source_db_stage_error(SourceDbStageError::PersistedBoundsExceeded);
        check_source_db_stage_error(SourceDbStageError::InvalidStoredAccounting);
        check_source_db_stage_error(SourceDbStageError::PersistedAccountingMismatch {
            expected_rows: example(),
            actual_rows: example(),
            expected_payload_bytes: example(),
            actual_payload_bytes: example(),
        });
        check_source_db_stage_error(SourceDbStageError::StageNotSealed);
        check_source_db_stage_error(SourceDbStageError::StageSealed);
        check_source_db_stage_error(SourceDbStageError::StageReadOnly);
        check_source_db_stage_error(SourceDbStageError::InvalidSourceId);
        check_source_db_stage_error(SourceDbStageError::CapturePageTooLarge { maximum: example() });
        check_source_db_stage_error(SourceDbStageError::RowLimitExceeded { maximum: example() });
        check_source_db_stage_error(SourceDbStageError::PayloadByteLimitExceeded {
            maximum: example(),
        });
        check_source_db_stage_error(SourceDbStageError::DatabaseByteLimitExceeded {
            maximum: example(),
        });
        check_source_db_stage_error(SourceDbStageError::DuplicateSourceId {
            table: example(),
            source_id: example(),
        });
        check_source_db_stage_error(SourceDbStageError::RowMustBeJsonObject);
        check_source_db_stage_error(SourceDbStageError::SerializeRow(example()));
        check_source_db_stage_error(SourceDbStageError::EncodingWorkerPanicked);
        check_source_db_stage_error(SourceDbStageError::StoredRowDecode {
            table: example(),
            source_id: example(),
            source: example(),
        });
        check_source_db_stage_error(SourceDbStageError::IntegrityCheckFailed(example()));
        check_source_db_stage_error(SourceDbStageError::InvalidPageCursor);
        check_source_db_stage_error(SourceDbStageError::InvalidPageSize);
    }
}
