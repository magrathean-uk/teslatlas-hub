//! Durable, local-only SQLite staging for a TeslaMate migration.
//!
//! The PostgreSQL reader will write decoded rows here one page at a time. This
//! deliberately does not project or import any history: it is the bounded,
//! sealed hand-off between the source reader and a later pack writer. A stage
//! can be read only after sealing, so an interrupted capture can never look
//! like a complete source snapshot.

use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};
use rustix::fs::statvfs;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

const STAGING_DIRECTORY: &str = ".staging";
const STAGE_FILE_EXTENSION: &str = "sqlite";
const META_STATE: &str = "state";
const META_ROW_COUNT: &str = "row_count";
const META_PAYLOAD_BYTES: &str = "payload_bytes";
const META_MAX_ROWS: &str = "max_rows";
const META_MAX_STAGE_BYTES: &str = "max_stage_bytes";
const META_MINIMUM_FREE_BYTES: &str = "minimum_free_bytes";
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

/// Fixed source-table names accepted by the TeslaMate stage. Callers cannot
/// interpolate relation names into a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TeslaMateStageTable {
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

impl TeslaMateStageTable {
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

/// Hard bounds for one source capture. `max_stage_bytes` is an upper bound on
/// the SQLite database allocation as well as the cumulative encoded JSON
/// payload, preventing a temporary import from consuming arbitrary host disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeslaMateStageLimits {
    pub max_rows: u64,
    pub max_stage_bytes: u64,
    /// Free space left untouched after reserving this stage's declared cap.
    /// This keeps a Pi or small VPS from filling its root volume mid-capture.
    pub minimum_free_bytes: u64,
}

impl Default for TeslaMateStageLimits {
    fn default() -> Self {
        Self {
            max_rows: 5_000_000,
            max_stage_bytes: 4 * 1024 * 1024 * 1024,
            minimum_free_bytes: DEFAULT_MINIMUM_FREE_BYTES,
        }
    }
}

impl TeslaMateStageLimits {
    pub fn validate(self) -> Result<(), TeslaMateStageError> {
        if self.max_rows == 0 {
            return Err(TeslaMateStageError::InvalidMaximumRows);
        }
        if self.max_stage_bytes < MIN_STAGE_BYTES {
            return Err(TeslaMateStageError::InvalidMaximumStageBytes {
                minimum: MIN_STAGE_BYTES,
            });
        }
        let pages = self.max_stage_bytes / 4096;
        if self.max_stage_bytes > u64::try_from(i64::MAX).expect("i64 max fits u64")
            || pages == 0
            || pages > u64::try_from(i64::MAX).expect("i64 max fits u64")
        {
            return Err(TeslaMateStageError::InvalidMaximumStageBytes {
                minimum: MIN_STAGE_BYTES,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeslaMateStageState {
    Open,
    Sealed,
}

impl TeslaMateStageState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Sealed => "sealed",
        }
    }

    fn parse(value: &str) -> Result<Self, TeslaMateStageError> {
        match value {
            "open" => Ok(Self::Open),
            "sealed" => Ok(Self::Sealed),
            other => Err(TeslaMateStageError::InvalidPersistedState(other.to_owned())),
        }
    }
}

/// Persisted accounting for a stage. It is intentionally small; callers page
/// rows from SQLite rather than materialising a `TeslaMateHistory` in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeslaMateStageStats {
    pub state: TeslaMateStageState,
    pub row_count: u64,
    pub payload_bytes: u64,
    pub limits: TeslaMateStageLimits,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeslaMateStageRow<T> {
    pub source_id: i64,
    pub value: T,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeslaMateStagePage<T> {
    pub rows: Vec<TeslaMateStageRow<T>>,
    pub next_after_id: Option<i64>,
}

/// A writable open capture or a read-only reopened sealed capture.
pub struct TeslaMateStage {
    path: PathBuf,
    connection: Connection,
    writable: bool,
}

impl std::fmt::Debug for TeslaMateStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeslaMateStage")
            .field("path", &self.path)
            .field("writable", &self.writable)
            .finish_non_exhaustive()
    }
}

impl TeslaMateStage {
    /// Start a new capture under `<imports_dir>/.staging`. Both directories
    /// are private and the database file is mode 0600 on Unix hosts.
    pub fn create(
        imports_dir: impl AsRef<Path>,
        limits: TeslaMateStageLimits,
    ) -> Result<Self, TeslaMateStageError> {
        limits.validate()?;
        let imports_dir = imports_dir.as_ref();
        ensure_private_directory(imports_dir)?;
        let staging_dir = imports_dir.join(STAGING_DIRECTORY);
        ensure_private_directory(&staging_dir)?;
        let required_free_bytes = limits
            .max_stage_bytes
            .checked_add(limits.minimum_free_bytes)
            .ok_or(TeslaMateStageError::StageCapacityOverflow)?;
        let available_free_bytes = available_bytes(&staging_dir)?;
        if available_free_bytes < required_free_bytes {
            return Err(TeslaMateStageError::InsufficientFreeSpace {
                required: required_free_bytes,
                available: available_free_bytes,
            });
        }

        let path = staging_dir.join(format!("{}.{}", Uuid::new_v4(), STAGE_FILE_EXTENSION));
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        ensure_private_file(&path)?;
        configure_writable_connection(&connection, limits)?;
        initialise_schema(&connection, limits)?;

        Ok(Self {
            path,
            connection,
            writable: true,
        })
    }

    /// Reopen a completed snapshot. Open captures are deliberately rejected:
    /// callers must resume/rebuild them with a writer, not treat them as a
    /// source-consistent history.
    pub fn open_sealed(path: impl AsRef<Path>) -> Result<Self, TeslaMateStageError> {
        let path = path.as_ref().to_path_buf();
        ensure_private_stage_path(&path)?;
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let stage = Self {
            path,
            connection,
            writable: false,
        };
        let stats = stage.stats()?;
        if stats.state != TeslaMateStageState::Sealed {
            return Err(TeslaMateStageError::StageNotSealed);
        }
        stage.verify_integrity()?;
        stage.verify_accounting(stats)?;
        Ok(stage)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove exactly this private staging file. Open captures are never
    /// resumable after a source-session failure; callers use this to discard
    /// their partial snapshot rather than risking mixed PostgreSQL views.
    pub fn discard(self) -> Result<(), TeslaMateStageError> {
        let path = self.path.clone();
        drop(self);
        fs::remove_file(&path).map_err(|source| TeslaMateStageError::RemoveStage { path, source })
    }

    pub fn stats(&self) -> Result<TeslaMateStageStats, TeslaMateStageError> {
        let state = TeslaMateStageState::parse(&read_meta(&self.connection, META_STATE)?)?;
        let row_count = parse_meta_u64(&self.connection, META_ROW_COUNT)?;
        let payload_bytes = parse_meta_u64(&self.connection, META_PAYLOAD_BYTES)?;
        let limits = TeslaMateStageLimits {
            max_rows: parse_meta_u64(&self.connection, META_MAX_ROWS)?,
            max_stage_bytes: parse_meta_u64(&self.connection, META_MAX_STAGE_BYTES)?,
            minimum_free_bytes: parse_meta_u64(&self.connection, META_MINIMUM_FREE_BYTES)?,
        };
        limits.validate()?;
        if row_count > limits.max_rows || payload_bytes > limits.max_stage_bytes {
            return Err(TeslaMateStageError::PersistedBoundsExceeded);
        }
        Ok(TeslaMateStageStats {
            state,
            row_count,
            payload_bytes,
            limits,
        })
    }

    /// Insert exactly one decoded source row. The value must serialize to a
    /// JSON object, avoiding accidental scalar/blob staging contracts.
    pub fn insert<T: Serialize>(
        &mut self,
        table: TeslaMateStageTable,
        source_id: i64,
        value: &T,
    ) -> Result<(), TeslaMateStageError> {
        self.insert_page(table, [(source_id, value)])
    }

    /// Commit one decoded PostgreSQL page. This is the capture hot path: a
    /// caller holds at most one source page and this method makes all of that
    /// page durable in one local SQLite transaction. Empty pages are harmless
    /// and do not create a transaction.
    pub fn insert_page<T, I>(
        &mut self,
        table: TeslaMateStageTable,
        input: I,
    ) -> Result<(), TeslaMateStageError>
    where
        T: Serialize,
        I: IntoIterator<Item = (i64, T)>,
    {
        if !self.writable {
            return Err(TeslaMateStageError::StageReadOnly);
        }
        self.require_open()?;
        let mut encoded = Vec::new();
        for (source_id, value) in input {
            if source_id <= 0 {
                return Err(TeslaMateStageError::InvalidSourceId);
            }
            if encoded.len() >= usize::try_from(MAX_PAGE_SIZE).expect("u32 fits usize") {
                return Err(TeslaMateStageError::CapturePageTooLarge {
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
        table: TeslaMateStageTable,
        input: I,
    ) -> Result<(), TeslaMateStageError>
    where
        T: Serialize + Sync,
        I: IntoIterator<Item = (i64, T)>,
    {
        if !self.writable {
            return Err(TeslaMateStageError::StageReadOnly);
        }
        self.require_open()?;
        let mut rows = Vec::new();
        for (source_id, value) in input {
            if source_id <= 0 {
                return Err(TeslaMateStageError::InvalidSourceId);
            }
            if rows.len() >= usize::try_from(MAX_PAGE_SIZE).expect("u32 fits usize") {
                return Err(TeslaMateStageError::CapturePageTooLarge {
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
        table: TeslaMateStageTable,
        input: I,
    ) -> Result<(), TeslaMateStageError>
    where
        I: IntoIterator<Item = (i64, String)>,
    {
        if !self.writable {
            return Err(TeslaMateStageError::StageReadOnly);
        }
        self.require_open()?;
        let mut encoded = Vec::new();
        for (source_id, json) in input {
            if source_id <= 0 {
                return Err(TeslaMateStageError::InvalidSourceId);
            }
            if encoded.len() >= usize::try_from(MAX_PAGE_SIZE).expect("u32 fits usize") {
                return Err(TeslaMateStageError::CapturePageTooLarge {
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
        table: TeslaMateStageTable,
        encoded: Vec<(i64, String, u64, i64)>,
    ) -> Result<(), TeslaMateStageError> {
        if encoded.is_empty() {
            return Ok(());
        }
        let stats = self.stats()?;
        let page_rows = u64::try_from(encoded.len()).expect("usize always fits u64");
        let next_rows = stats.row_count.checked_add(page_rows).ok_or(
            TeslaMateStageError::RowLimitExceeded {
                maximum: stats.limits.max_rows,
            },
        )?;
        if next_rows > stats.limits.max_rows {
            return Err(TeslaMateStageError::RowLimitExceeded {
                maximum: stats.limits.max_rows,
            });
        }
        let page_payload = encoded.iter().try_fold(0_u64, |total, (_, _, bytes, _)| {
            total
                .checked_add(*bytes)
                .ok_or(TeslaMateStageError::PayloadByteLimitExceeded {
                    maximum: stats.limits.max_stage_bytes,
                })
        })?;
        let next_payload = stats.payload_bytes.checked_add(page_payload).ok_or(
            TeslaMateStageError::PayloadByteLimitExceeded {
                maximum: stats.limits.max_stage_bytes,
            },
        )?;
        if next_payload > stats.limits.max_stage_bytes {
            return Err(TeslaMateStageError::PayloadByteLimitExceeded {
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
                    return Err(TeslaMateStageError::DuplicateSourceId {
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
        transaction
            .commit()
            .map_err(|error| map_write_error(error, stats.limits.max_stage_bytes))?;
        Ok(())
    }

    /// Seal an open snapshot after a full SQLite integrity check. Sealed data
    /// cannot be mutated through this API, and the reopened reader is SQLite
    /// read-only.
    pub fn seal(&mut self) -> Result<TeslaMateStageStats, TeslaMateStageError> {
        if !self.writable {
            return Err(TeslaMateStageError::StageReadOnly);
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
            TeslaMateStageState::Sealed.as_str(),
        )?;
        transaction.commit()?;
        self.verify_integrity()?;
        self.stats()
    }

    /// Run SQLite's complete integrity check. This is intentionally not only a
    /// quick check because a sealed migration may be retained for a long time.
    pub fn verify_integrity(&self) -> Result<(), TeslaMateStageError> {
        let mut statement = self.connection.prepare("PRAGMA integrity_check")?;
        let values = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if values.len() == 1 && values.first().is_some_and(|value| value == "ok") {
            return Ok(());
        }
        Err(TeslaMateStageError::IntegrityCheckFailed(values.join("; ")))
    }

    fn verify_accounting(&self, expected: TeslaMateStageStats) -> Result<(), TeslaMateStageError> {
        let (row_count, payload_bytes): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(encoded_bytes), 0) FROM stage_rows",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let actual_rows =
            u64::try_from(row_count).map_err(|_| TeslaMateStageError::InvalidStoredAccounting)?;
        let actual_payload = u64::try_from(payload_bytes)
            .map_err(|_| TeslaMateStageError::InvalidStoredAccounting)?;
        if actual_rows != expected.row_count || actual_payload != expected.payload_bytes {
            return Err(TeslaMateStageError::PersistedAccountingMismatch {
                expected_rows: expected.row_count,
                actual_rows,
                expected_payload_bytes: expected.payload_bytes,
                actual_payload_bytes: actual_payload,
            });
        }
        if actual_rows > expected.limits.max_rows
            || actual_payload > expected.limits.max_stage_bytes
        {
            return Err(TeslaMateStageError::PersistedBoundsExceeded);
        }
        Ok(())
    }

    /// Retrieve a bounded typed page from a sealed stage using a fixed keyset
    /// query. It never constructs a whole source history in memory.
    pub fn page<T: DeserializeOwned>(
        &self,
        table: TeslaMateStageTable,
        after_id: i64,
        limit: u32,
    ) -> Result<TeslaMateStagePage<T>, TeslaMateStageError> {
        if after_id < 0 {
            return Err(TeslaMateStageError::InvalidPageCursor);
        }
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(TeslaMateStageError::InvalidPageSize);
        }
        if self.stats()?.state != TeslaMateStageState::Sealed {
            return Err(TeslaMateStageError::StageNotSealed);
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
                    TeslaMateStageError::StoredRowDecode {
                        table: table.as_str(),
                        source_id,
                        source,
                    }
                })?;
                Ok(TeslaMateStageRow { source_id, value })
            })
            .collect::<Result<Vec<_>, TeslaMateStageError>>()?;
        let next_after_id = if output.len() > usize::try_from(limit).expect("u32 fits usize") {
            output.pop();
            output.last().map(|row| row.source_id)
        } else {
            None
        };
        Ok(TeslaMateStagePage {
            rows: output,
            next_after_id,
        })
    }

    /// Fetch one fixed-table source row from a sealed capture. It is used by
    /// the fragment writer for parent and endpoint relations, so it never
    /// needs to materialize another whole stage table.
    pub fn get<T: DeserializeOwned>(
        &self,
        table: TeslaMateStageTable,
        source_id: i64,
    ) -> Result<Option<T>, TeslaMateStageError> {
        if source_id <= 0 {
            return Err(TeslaMateStageError::InvalidSourceId);
        }
        if self.stats()?.state != TeslaMateStageState::Sealed {
            return Err(TeslaMateStageError::StageNotSealed);
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
            serde_json::from_str(&json).map_err(|source| TeslaMateStageError::StoredRowDecode {
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
    ) -> Result<TeslaMateStagePage<T>, TeslaMateStageError> {
        if charging_process_id <= 0 {
            return Err(TeslaMateStageError::InvalidSourceId);
        }
        self.page_charge_samples(charging_process_id, after_id, limit)
    }

    fn page_charge_samples<T: DeserializeOwned>(
        &self,
        value: i64,
        after_id: i64,
        limit: u32,
    ) -> Result<TeslaMateStagePage<T>, TeslaMateStageError> {
        if after_id < 0 {
            return Err(TeslaMateStageError::InvalidPageCursor);
        }
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(TeslaMateStageError::InvalidPageSize);
        }
        if self.stats()?.state != TeslaMateStageState::Sealed {
            return Err(TeslaMateStageError::StageNotSealed);
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
                    TeslaMateStageError::StoredRowDecode {
                        table: TeslaMateStageTable::Charges.as_str(),
                        source_id,
                        source,
                    }
                })?;
                Ok(TeslaMateStageRow { source_id, value })
            })
            .collect::<Result<Vec<_>, TeslaMateStageError>>()?;
        let next_after_id = if output.len() > usize::try_from(limit).expect("u32 fits usize") {
            output.pop();
            output.last().map(|row| row.source_id)
        } else {
            None
        };
        Ok(TeslaMateStagePage {
            rows: output,
            next_after_id,
        })
    }

    fn require_open(&self) -> Result<(), TeslaMateStageError> {
        match self.stats()?.state {
            TeslaMateStageState::Open => Ok(()),
            TeslaMateStageState::Sealed => Err(TeslaMateStageError::StageSealed),
        }
    }
}

fn configure_writable_connection(
    connection: &Connection,
    limits: TeslaMateStageLimits,
) -> Result<(), TeslaMateStageError> {
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

fn initialise_schema(
    connection: &Connection,
    limits: TeslaMateStageLimits,
) -> Result<(), TeslaMateStageError> {
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
                 'cars', 'drives', 'positions', 'charging_processes', 'charges',
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
        (META_STATE, TeslaMateStageState::Open.as_str().to_owned()),
        (META_ROW_COUNT, "0".to_owned()),
        (META_PAYLOAD_BYTES, "0".to_owned()),
        (META_MAX_ROWS, limits.max_rows.to_string()),
        (META_MAX_STAGE_BYTES, limits.max_stage_bytes.to_string()),
        (
            META_MINIMUM_FREE_BYTES,
            limits.minimum_free_bytes.to_string(),
        ),
    ] {
        write_meta(&transaction, key, value)?;
    }
    transaction.commit()?;

    let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let max_pages = i64::try_from(limits.max_stage_bytes / 4096).expect("validated page limit");
    if page_count > max_pages {
        return Err(TeslaMateStageError::StageLimitTooSmall {
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
) -> Result<Vec<(i64, String, u64, i64)>, TeslaMateStageError> {
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
                .map_err(|_| TeslaMateStageError::EncodingWorkerPanicked)?;
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

fn encode_row<T: Serialize>(value: &T) -> Result<String, TeslaMateStageError> {
    let value = serde_json::to_value(value).map_err(TeslaMateStageError::SerializeRow)?;
    if !matches!(value, Value::Object(_)) {
        return Err(TeslaMateStageError::RowMustBeJsonObject);
    }
    serde_json::to_string(&value).map_err(TeslaMateStageError::SerializeRow)
}

fn read_meta(connection: &Connection, key: &'static str) -> Result<String, TeslaMateStageError> {
    connection
        .query_row(
            "SELECT value FROM stage_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(TeslaMateStageError::MissingMetadata(key))
}

fn parse_meta_u64(connection: &Connection, key: &'static str) -> Result<u64, TeslaMateStageError> {
    read_meta(connection, key)?
        .parse()
        .map_err(|_| TeslaMateStageError::InvalidMetadata(key))
}

fn write_meta(
    transaction: &rusqlite::Transaction<'_>,
    key: &'static str,
    value: impl ToString,
) -> Result<(), TeslaMateStageError> {
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

fn map_write_error(error: rusqlite::Error, maximum: u64) -> TeslaMateStageError {
    if matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _) if code.code == ErrorCode::DiskFull
    ) {
        TeslaMateStageError::DatabaseByteLimitExceeded { maximum }
    } else {
        TeslaMateStageError::Sqlite(error)
    }
}

fn ensure_private_stage_path(path: &Path) -> Result<(), TeslaMateStageError> {
    if path.extension().and_then(|extension| extension.to_str()) != Some(STAGE_FILE_EXTENSION) {
        return Err(TeslaMateStageError::InvalidStagePath);
    }
    let parent = path.parent().ok_or(TeslaMateStageError::InvalidStagePath)?;
    if parent.file_name().and_then(|name| name.to_str()) != Some(STAGING_DIRECTORY) {
        return Err(TeslaMateStageError::InvalidStagePath);
    }
    ensure_private_directory(parent)?;
    let imports_dir = parent
        .parent()
        .ok_or(TeslaMateStageError::InvalidStagePath)?;
    ensure_private_directory(imports_dir)?;
    ensure_private_file(path)
}

fn ensure_private_directory(path: &Path) -> Result<(), TeslaMateStageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(TeslaMateStageError::SymlinkPath(path.to_path_buf()));
            }
            if !metadata.is_dir() {
                return Err(TeslaMateStageError::ExpectedDirectory(path.to_path_buf()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|source| TeslaMateStageError::CreateDirectory {
                path: path.to_path_buf(),
                source,
            })?;
        }
        Err(source) => {
            return Err(TeslaMateStageError::InspectPath {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    set_private_directory_permissions(path)
}

fn ensure_private_file(path: &Path) -> Result<(), TeslaMateStageError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| TeslaMateStageError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.file_type().is_symlink() {
        return Err(TeslaMateStageError::SymlinkPath(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(TeslaMateStageError::ExpectedFile(path.to_path_buf()));
    }
    set_private_file_permissions(path)
}

fn available_bytes(path: &Path) -> Result<u64, TeslaMateStageError> {
    let stats = statvfs(path).map_err(|source| TeslaMateStageError::FilesystemSpace {
        path: path.to_path_buf(),
        source,
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(TeslaMateStageError::FilesystemSpaceOverflow)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), TeslaMateStageError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        TeslaMateStageError::SetPermissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), TeslaMateStageError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), TeslaMateStageError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        TeslaMateStageError::SetPermissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), TeslaMateStageError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum TeslaMateStageError {
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
    #[error("could not inspect stage path {path}: {source}")]
    InspectPath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not set private permissions on {path}: {source}")]
    SetPermissions {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not remove private stage {path}: {source}")]
    RemoveStage {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("stage path may not be a symlink: {0}")]
    SymlinkPath(PathBuf),
    #[error("expected stage directory at {0}")]
    ExpectedDirectory(PathBuf),
    #[error("expected stage file at {0}")]
    ExpectedFile(PathBuf),
    #[error("stage path must be a .sqlite file inside a private .staging directory")]
    InvalidStagePath,
    #[error("stage database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stage needs at least {minimum} bytes for its SQLite schema")]
    StageLimitTooSmall { minimum: u64 },
    #[error("stage state is invalid: {0}")]
    InvalidPersistedState(String),
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
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Barrier},
    };

    use serde::{Deserialize, Serialize, ser::SerializeStruct};
    use tempfile::tempdir;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Row {
        label: String,
        ordinal: u32,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct ChargeRow {
        charging_process_id: i64,
        label: String,
    }

    fn limits() -> TeslaMateStageLimits {
        TeslaMateStageLimits {
            max_rows: 10,
            max_stage_bytes: 512 * 1024,
            minimum_free_bytes: 0,
        }
    }

    #[test]
    fn encoding_workers_are_bounded() {
        assert_eq!(stage_encoding_worker_count_for(0), 1);
        assert_eq!(stage_encoding_worker_count_for(1), 1);
        assert_eq!(stage_encoding_worker_count_for(4), 4);
        assert_eq!(stage_encoding_worker_count_for(64), MAX_ENCODING_WORKERS);
    }

    #[derive(Clone)]
    struct BarrierRow {
        barrier: Arc<Barrier>,
        ordinal: u32,
    }

    impl Serialize for BarrierRow {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            self.barrier.wait();
            let mut row = serializer.serialize_struct("BarrierRow", 1)?;
            row.serialize_field("ordinal", &self.ordinal)?;
            row.end()
        }
    }

    #[test]
    fn parallel_encoding_uses_multiple_workers_and_keeps_input_order() {
        let barrier = Arc::new(Barrier::new(2));
        let rows = vec![
            (
                2,
                BarrierRow {
                    barrier: Arc::clone(&barrier),
                    ordinal: 2,
                },
            ),
            (
                1,
                BarrierRow {
                    barrier,
                    ordinal: 1,
                },
            ),
        ];
        let encoded = encode_rows_parallel(rows, 2).expect("parallel encoding");
        assert_eq!(
            encoded.iter().map(|row| row.0).collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    #[test]
    fn parallel_insert_preserves_deterministic_stored_order() {
        let temporary = tempdir().expect("temp dir");
        let mut stage = TeslaMateStage::create(temporary.path(), limits()).expect("stage");
        stage
            .insert_page_parallel(
                TeslaMateStageTable::Cars,
                vec![
                    (
                        3,
                        Row {
                            label: "c".into(),
                            ordinal: 3,
                        },
                    ),
                    (
                        1,
                        Row {
                            label: "a".into(),
                            ordinal: 1,
                        },
                    ),
                    (
                        2,
                        Row {
                            label: "b".into(),
                            ordinal: 2,
                        },
                    ),
                ],
            )
            .expect("insert");
        stage.seal().expect("seal");
        let page = stage
            .page::<Row>(TeslaMateStageTable::Cars, 0, 10)
            .expect("page");
        assert_eq!(
            page.rows
                .iter()
                .map(|row| row.source_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    struct PanickingRow;

    impl Serialize for PanickingRow {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            panic!("test worker panic");
        }
    }

    #[test]
    fn parallel_encoding_reports_worker_panics() {
        let error = encode_rows_parallel(vec![(1, PanickingRow)], 1).expect_err("panic error");
        assert!(matches!(error, TeslaMateStageError::EncodingWorkerPanicked));
    }

    #[test]
    fn stages_typed_rows_in_private_paths_and_pages_only_when_sealed() {
        let temporary = tempdir().expect("temp dir");
        let imports = temporary.path().join("imports");
        let mut stage = TeslaMateStage::create(&imports, limits()).expect("stage");
        let path = stage.path().to_path_buf();
        assert_eq!(
            fs::metadata(&imports)
                .expect("imports mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(imports.join(STAGING_DIRECTORY))
                .expect("staging mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("stage mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        stage
            .insert(
                TeslaMateStageTable::Positions,
                1,
                &Row {
                    label: "first".to_owned(),
                    ordinal: 1,
                },
            )
            .expect("first row");
        stage
            .insert(
                TeslaMateStageTable::Positions,
                2,
                &Row {
                    label: "second".to_owned(),
                    ordinal: 2,
                },
            )
            .expect("second row");
        stage
            .insert(
                TeslaMateStageTable::Positions,
                3,
                &Row {
                    label: "third".to_owned(),
                    ordinal: 3,
                },
            )
            .expect("third row");
        stage
            .insert(
                TeslaMateStageTable::Charges,
                11,
                &ChargeRow {
                    charging_process_id: 7,
                    label: "first sample".to_owned(),
                },
            )
            .expect("first sample");
        stage
            .insert(
                TeslaMateStageTable::Charges,
                12,
                &ChargeRow {
                    charging_process_id: 7,
                    label: "second sample".to_owned(),
                },
            )
            .expect("second sample");
        assert!(matches!(
            stage.page::<Row>(TeslaMateStageTable::Positions, 0, 2),
            Err(TeslaMateStageError::StageNotSealed)
        ));

        let stats = stage.seal().expect("sealed");
        assert_eq!(stats.state, TeslaMateStageState::Sealed);
        assert_eq!(stats.row_count, 5);
        stage.verify_integrity().expect("integrity");
        let first = stage
            .page::<Row>(TeslaMateStageTable::Positions, 0, 2)
            .expect("first page");
        assert_eq!(
            first
                .rows
                .iter()
                .map(|row| row.source_id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(first.next_after_id, Some(2));
        let second = stage
            .page::<Row>(
                TeslaMateStageTable::Positions,
                first.next_after_id.expect("cursor"),
                2,
            )
            .expect("second page");
        assert_eq!(
            second
                .rows
                .iter()
                .map(|row| row.source_id)
                .collect::<Vec<_>>(),
            [3]
        );
        assert_eq!(second.next_after_id, None);
        assert_eq!(
            stage
                .get::<ChargeRow>(TeslaMateStageTable::Charges, 11)
                .expect("sample lookup")
                .expect("sample"),
            ChargeRow {
                charging_process_id: 7,
                label: "first sample".to_owned(),
            }
        );
        let samples = stage
            .charge_samples_for_process::<ChargeRow>(7, 0, 1)
            .expect("sample page");
        assert_eq!(samples.rows.len(), 1);
        assert_eq!(samples.rows[0].source_id, 11);
        assert_eq!(samples.next_after_id, Some(11));
        assert!(matches!(
            stage.insert(
                TeslaMateStageTable::Positions,
                4,
                &Row {
                    label: "forbidden".to_owned(),
                    ordinal: 4,
                }
            ),
            Err(TeslaMateStageError::StageSealed)
        ));
        drop(stage);

        let mut reopened = TeslaMateStage::open_sealed(path).expect("reopened sealed");
        assert_eq!(reopened.stats().expect("stats").row_count, 5);
        assert!(matches!(
            reopened.insert(
                TeslaMateStageTable::Positions,
                4,
                &Row {
                    label: "forbidden".to_owned(),
                    ordinal: 4,
                }
            ),
            Err(TeslaMateStageError::StageReadOnly)
        ));
    }

    #[test]
    fn charge_sample_page_uses_process_index() {
        let temporary = tempdir().expect("temp dir");
        let mut stage = TeslaMateStage::create(temporary.path(), limits()).expect("stage");
        stage
            .insert(
                TeslaMateStageTable::Charges,
                11,
                &ChargeRow {
                    charging_process_id: 7,
                    label: "wanted".to_owned(),
                },
            )
            .expect("wanted charge");
        stage
            .insert(
                TeslaMateStageTable::Charges,
                12,
                &ChargeRow {
                    charging_process_id: 8,
                    label: "other".to_owned(),
                },
            )
            .expect("other charge");
        stage.seal().expect("sealed");

        let sql = format!("EXPLAIN QUERY PLAN {CHARGE_SAMPLES_PAGE_SQL}");
        let mut statement = stage.connection.prepare(&sql).expect("plan statement");
        let plan = statement
            .query_map(rusqlite::params![7_i64, 0_i64, 2_i64], |row| {
                row.get::<_, String>(3)
            })
            .expect("plan query")
            .collect::<Result<Vec<_>, _>>()
            .expect("plan rows");
        assert!(
            plan.iter()
                .any(|detail| detail.contains("USING INDEX stage_charge_samples_by_process")),
            "unexpected charge-sample plan: {plan:?}"
        );
    }

    #[test]
    fn rejects_bound_overrun_duplicate_ids_and_non_object_rows() {
        let temporary = tempdir().expect("temp dir");
        let mut stage = TeslaMateStage::create(
            temporary.path(),
            TeslaMateStageLimits {
                max_rows: 1,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("stage");
        let first = Row {
            label: "one".to_owned(),
            ordinal: 1,
        };
        stage
            .insert(TeslaMateStageTable::Cars, 1, &first)
            .expect("one row");
        assert!(matches!(
            stage.insert(TeslaMateStageTable::Cars, 2, &first),
            Err(TeslaMateStageError::RowLimitExceeded { maximum: 1 })
        ));

        let temporary = tempdir().expect("temp dir");
        let mut byte_limited = TeslaMateStage::create(
            temporary.path(),
            TeslaMateStageLimits {
                max_rows: 2,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("stage");
        let too_large = Row {
            label: "x".repeat(128 * 1024),
            ordinal: 1,
        };
        assert!(matches!(
            byte_limited.insert(TeslaMateStageTable::Cars, 1, &too_large),
            Err(TeslaMateStageError::PayloadByteLimitExceeded { .. })
        ));
        assert!(matches!(
            byte_limited.insert(TeslaMateStageTable::Cars, 0, &first),
            Err(TeslaMateStageError::InvalidSourceId)
        ));
        assert!(matches!(
            byte_limited.insert(TeslaMateStageTable::Cars, 1, &"scalar"),
            Err(TeslaMateStageError::RowMustBeJsonObject)
        ));
        byte_limited
            .insert(TeslaMateStageTable::Cars, 1, &first)
            .expect("row");
        assert!(matches!(
            byte_limited.insert(TeslaMateStageTable::Cars, 1, &first),
            Err(TeslaMateStageError::DuplicateSourceId { .. })
        ));
    }

    #[test]
    fn refuses_open_or_symlinked_stages_as_complete_snapshots() {
        let temporary = tempdir().expect("temp dir");
        let stage = TeslaMateStage::create(temporary.path(), limits()).expect("stage");
        let path = stage.path().to_path_buf();
        drop(stage);
        assert!(matches!(
            TeslaMateStage::open_sealed(&path),
            Err(TeslaMateStageError::StageNotSealed)
        ));

        let target = temporary.path().join("target.sqlite");
        fs::write(&target, b"not a stage").expect("target");
        let link = temporary.path().join(".staging/link.sqlite");
        std::os::unix::fs::symlink(&target, &link).expect("link");
        assert!(matches!(
            TeslaMateStage::open_sealed(link),
            Err(TeslaMateStageError::SymlinkPath(_))
        ));
    }

    #[test]
    fn discards_only_its_exact_private_stage_file() {
        let temporary = tempdir().expect("temp dir");
        let stage = TeslaMateStage::create(temporary.path(), limits()).expect("stage");
        let path = stage.path().to_path_buf();
        stage.discard().expect("discard");
        assert!(
            matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(temporary.path().join(STAGING_DIRECTORY).is_dir());
    }

    #[test]
    fn refuses_to_consume_the_host_disk_reserve() {
        let temporary = tempdir().expect("temp dir");
        let result = TeslaMateStage::create(
            temporary.path(),
            TeslaMateStageLimits {
                max_rows: 1,
                max_stage_bytes: MIN_STAGE_BYTES,
                minimum_free_bytes: u64::MAX - MIN_STAGE_BYTES,
            },
        );
        assert!(matches!(
            result,
            Err(TeslaMateStageError::InsufficientFreeSpace { .. })
        ));
    }
}
