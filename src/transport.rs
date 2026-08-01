//! Streaming writer for immutable Teslatlas SQLite transport packs.
//!
//! A transport pack is deliberately a small SQLite database rather than a
//! serialised Hub database.  It contains only the projected source rows that
//! an iOS mirror needs.  Rows are inserted one at a time, the SQLite file is
//! compressed directly to a temporary object, and that object is hard-linked
//! into its content-addressed location only after protocol verification.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::protocol::{
    MirrorTable, PackCompression, PackFormat, ProtocolError, ProtocolLimits,
    SQLITE_TRANSPORT_APPLICATION_ID, SchemaVersion, SequenceRange, Sha256Digest,
    TRANSPORT_SCHEMA_V1, TransferMode, TransportPack, VerifiedTransportPack,
};

const MAX_ENTITY_KEY_BYTES: usize = 512;
const MAX_ROW_PAYLOAD_BYTES: usize = 1024 * 1024;
const COMPRESSION_LEVEL: i32 = 3;

/// An operation in the source-owned mirror projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportOperation {
    Upsert,
    Delete,
}

impl TransportOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
        }
    }
}

/// A typed scalar used by the generic `sync_rows` pack table.
///
/// The map around these values is sorted (`BTreeMap`), so row payload JSON is
/// canonical for a given set of values.  A pack writer never needs to retain a
/// history-sized serialisation buffer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum TransportValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    Text(String),
}

/// One generic but typed source record.  Its payload stays in one bounded JSON
/// cell in the pack; entity-specific projection tables can be added in a later
/// schema version without changing the transfer lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportRow {
    pub table: MirrorTable,
    pub entity_key: String,
    pub source_sequence: u64,
    pub operation: TransportOperation,
    pub values: BTreeMap<String, TransportValue>,
}

/// Input needed to build one ordered transport object.
#[derive(Debug, Clone)]
pub struct TransportPackRequest<'a> {
    pub pack_id: Uuid,
    pub snapshot_id: Uuid,
    pub ordinal: u32,
    pub schema: SchemaVersion,
    pub mode: TransferMode,
    pub sequence: SequenceRange,
    pub tables: &'a [MirrorTable],
    pub rows: &'a [TransportRow],
}

/// Location and validated metadata for a completed immutable object.
#[derive(Debug, Clone)]
pub struct BuiltTransportPack {
    pub metadata: TransportPack,
    pub path: PathBuf,
    pub verified: VerifiedTransportPack,
}

/// Writes packs below `<packs_dir>/sha256/`.
#[derive(Debug, Clone)]
pub struct TransportPackWriter {
    packs_dir: PathBuf,
    limits: ProtocolLimits,
}

impl TransportPackWriter {
    pub fn new(packs_dir: impl Into<PathBuf>) -> Self {
        Self {
            packs_dir: packs_dir.into(),
            limits: ProtocolLimits::default(),
        }
    }

    pub fn with_limits(packs_dir: impl Into<PathBuf>, limits: ProtocolLimits) -> Self {
        Self {
            packs_dir: packs_dir.into(),
            limits,
        }
    }

    pub fn packs_dir(&self) -> &Path {
        &self.packs_dir
    }

    pub fn content_path(&self, digest: Sha256Digest) -> PathBuf {
        self.packs_dir
            .join("sha256")
            .join(format!("{digest}.sqlite.zst"))
    }

    /// Build, verify, and atomically publish one content-addressed pack.
    ///
    /// The source slice is the caller's bounded batch.  The writer stores one
    /// row at a time and streams compression from disk, so it never creates a
    /// second in-memory image of the SQLite database.
    pub fn write_pack(
        &self,
        request: &TransportPackRequest<'_>,
    ) -> Result<BuiltTransportPack, TransportError> {
        self.validate_request(request)?;
        let staging_dir = self.packs_dir.join(".staging");
        let content_dir = self.packs_dir.join("sha256");
        fs::create_dir_all(&staging_dir).map_err(|source| TransportError::CreateDirectory {
            path: staging_dir.clone(),
            source,
        })?;
        fs::create_dir_all(&content_dir).map_err(|source| TransportError::CreateDirectory {
            path: content_dir.clone(),
            source,
        })?;

        let sqlite_temp = TemporaryPath::create(&staging_dir, "sqlite")?;
        self.write_sqlite_pack(sqlite_temp.path(), request)?;
        let uncompressed_bytes = fs::metadata(sqlite_temp.path())
            .map_err(|source| TransportError::Metadata {
                path: sqlite_temp.path().to_path_buf(),
                source,
            })?
            .len();

        let compressed_temp = TemporaryPath::create(&staging_dir, "zst")?;
        let (sha256, compressed_bytes) = compress_file(sqlite_temp.path(), compressed_temp.path())?;
        let metadata = TransportPack {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            schema: request.schema,
            format: PackFormat::SqliteTransport,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(sha256),
            sha256,
            compressed_bytes,
            uncompressed_bytes,
            row_count: request.rows.len() as u64,
            sequence: request.sequence,
            tables: request.tables.to_vec(),
        };
        metadata.validate(self.limits)?;

        let verified = verify_file(&metadata, compressed_temp.path(), self.limits)?;
        let final_path = self.content_path(sha256);
        publish_immutable(compressed_temp.path(), &final_path, &metadata, self.limits)?;

        Ok(BuiltTransportPack {
            metadata,
            path: final_path,
            verified,
        })
    }

    fn validate_request(&self, request: &TransportPackRequest<'_>) -> Result<(), TransportError> {
        if request.pack_id.is_nil() || request.snapshot_id.is_nil() {
            return Err(TransportError::NilIdentifier);
        }
        if request.schema != TRANSPORT_SCHEMA_V1 {
            return Err(TransportError::UnsupportedSchema(request.schema));
        }
        if !request.sequence.is_ordered() {
            return Err(TransportError::InvalidSequenceRange);
        }
        if matches!(request.mode, TransferMode::Incremental)
            && request.sequence.to_inclusive <= request.sequence.from_exclusive
        {
            return Err(TransportError::NonProgressingDelta);
        }
        if request.tables.is_empty() || request.tables.len() > self.limits.max_tables_per_pack {
            return Err(TransportError::InvalidTableCount);
        }
        let declared = request.tables.iter().copied().collect::<HashSet<_>>();
        if declared.len() != request.tables.len() {
            return Err(TransportError::DuplicateTable);
        }
        if u64::try_from(request.rows.len()).map_err(|_| TransportError::TooManyRows)?
            > self.limits.max_rows_per_pack
        {
            return Err(TransportError::TooManyRows);
        }
        let mut previous_sequence = 0_u64;
        for (index, row) in request.rows.iter().enumerate() {
            if !declared.contains(&row.table) {
                return Err(TransportError::UndeclaredRowTable);
            }
            if row.entity_key.is_empty()
                || row.entity_key.len() > MAX_ENTITY_KEY_BYTES
                || row.entity_key.as_bytes().contains(&0)
            {
                return Err(TransportError::InvalidEntityKey);
            }
            if matches!(row.operation, TransportOperation::Delete) && !row.values.is_empty() {
                return Err(TransportError::DeleteHasValues);
            }
            if matches!(row.operation, TransportOperation::Upsert) && row.values.is_empty() {
                return Err(TransportError::EmptyUpsert);
            }
            if row
                .values
                .iter()
                .any(|(key, _)| key.is_empty() || key.as_bytes().contains(&0))
            {
                return Err(TransportError::InvalidValueKey);
            }
            if row
                .values
                .values()
                .any(|value| matches!(value, TransportValue::Real(number) if !number.is_finite()))
            {
                return Err(TransportError::NonFiniteReal);
            }
            if row.source_sequence > i64::MAX as u64 {
                return Err(TransportError::SequenceOutOfSqliteRange);
            }
            match request.mode {
                TransferMode::FullSnapshot => {
                    if row.source_sequence > request.sequence.to_inclusive {
                        return Err(TransportError::RowOutsideSequenceRange);
                    }
                }
                TransferMode::Incremental => {
                    if row.source_sequence <= request.sequence.from_exclusive
                        || row.source_sequence > request.sequence.to_inclusive
                    {
                        return Err(TransportError::RowOutsideSequenceRange);
                    }
                    if index != 0 && row.source_sequence < previous_sequence {
                        return Err(TransportError::RowsNotSequenceOrdered);
                    }
                }
            }
            previous_sequence = row.source_sequence;
            let payload = encode_payload(row)?;
            if payload
                .as_ref()
                .is_some_and(|value| value.len() > MAX_ROW_PAYLOAD_BYTES)
            {
                return Err(TransportError::RowPayloadTooLarge);
            }
        }
        Ok(())
    }

    fn write_sqlite_pack(
        &self,
        path: &Path,
        request: &TransportPackRequest<'_>,
    ) -> Result<(), TransportError> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(TransportError::OpenSqlite)?;
        let max_pages = self.limits.max_uncompressed_pack_bytes / 4_096;
        if max_pages == 0 {
            return Err(TransportError::UncompressedLimitTooSmall);
        }
        connection
            .pragma_update(None, "page_size", 4_096_i64)
            .map_err(TransportError::ConfigureSqlite)?;
        connection
            .pragma_update(
                None,
                "max_page_count",
                i64::try_from(max_pages).unwrap_or(i64::MAX),
            )
            .map_err(TransportError::ConfigureSqlite)?;
        connection
            .execute_batch(
                "
                PRAGMA journal_mode = OFF;
                PRAGMA synchronous = OFF;
                PRAGMA foreign_keys = ON;
                PRAGMA trusted_schema = OFF;
                PRAGMA temp_store = FILE;
                BEGIN IMMEDIATE;
                CREATE TABLE pack_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                CREATE TABLE sync_rows (
                    row_ordinal INTEGER PRIMARY KEY NOT NULL,
                    source_sequence INTEGER NOT NULL CHECK (source_sequence >= 0),
                    table_name TEXT NOT NULL,
                    entity_key TEXT NOT NULL,
                    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
                    payload_json TEXT,
                    CHECK (
                        (operation = 'upsert' AND payload_json IS NOT NULL)
                        OR (operation = 'delete' AND payload_json IS NULL)
                    ),
                    UNIQUE (source_sequence, table_name, entity_key)
                ) STRICT;
                CREATE INDEX sync_rows_sequence ON sync_rows(source_sequence, row_ordinal);
                COMMIT;
                ",
            )
            .map_err(TransportError::CreateSchema)?;

        let transaction = connection
            .unchecked_transaction()
            .map_err(TransportError::BeginTransaction)?;
        insert_metadata(&transaction, request)?;
        {
            let mut statement = transaction
                .prepare_cached(
                    "
                    INSERT INTO sync_rows (
                        row_ordinal, source_sequence, table_name, entity_key, operation, payload_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    ",
                )
                .map_err(TransportError::PrepareRow)?;
            for (index, row) in request.rows.iter().enumerate() {
                let ordinal = i64::try_from(index).map_err(|_| TransportError::TooManyRows)?;
                let sequence = i64::try_from(row.source_sequence)
                    .map_err(|_| TransportError::SequenceOutOfSqliteRange)?;
                let payload = encode_payload(row)?;
                statement
                    .execute(params![
                        ordinal,
                        sequence,
                        table_name(row.table),
                        row.entity_key,
                        row.operation.as_str(),
                        payload,
                    ])
                    .map_err(TransportError::InsertRow)?;
            }
        }
        transaction
            .commit()
            .map_err(TransportError::CommitTransaction)?;
        connection
            .pragma_update(None, "application_id", SQLITE_TRANSPORT_APPLICATION_ID)
            .map_err(TransportError::ConfigureSqlite)?;
        connection
            .pragma_update(None, "user_version", request.schema.sqlite_user_version())
            .map_err(TransportError::ConfigureSqlite)?;
        connection
            .execute_batch("PRAGMA optimize; VACUUM;")
            .map_err(TransportError::FinalizeSqlite)?;
        connection
            .pragma_update(None, "application_id", SQLITE_TRANSPORT_APPLICATION_ID)
            .map_err(TransportError::ConfigureSqlite)?;
        connection
            .pragma_update(None, "user_version", request.schema.sqlite_user_version())
            .map_err(TransportError::ConfigureSqlite)?;
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .map_err(TransportError::IntegrityCheck)
            .and_then(|result| {
                if result == "ok" {
                    Ok(())
                } else {
                    Err(TransportError::IntegrityFailure)
                }
            })?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("cannot create pack directory {path}: {source}")]
    CreateDirectory { path: PathBuf, source: io::Error },
    #[error("cannot create temporary pack file {path}: {source}")]
    CreateTemporary { path: PathBuf, source: io::Error },
    #[error("cannot inspect temporary pack file {path}: {source}")]
    Metadata { path: PathBuf, source: io::Error },
    #[error("cannot open SQLite transport pack: {0}")]
    OpenSqlite(rusqlite::Error),
    #[error("cannot create SQLite transport schema: {0}")]
    CreateSchema(rusqlite::Error),
    #[error("cannot start pack write transaction: {0}")]
    BeginTransaction(rusqlite::Error),
    #[error("cannot prepare transport row statement: {0}")]
    PrepareRow(rusqlite::Error),
    #[error("cannot insert transport row: {0}")]
    InsertRow(rusqlite::Error),
    #[error("cannot commit transport rows: {0}")]
    CommitTransaction(rusqlite::Error),
    #[error("cannot configure SQLite transport pack: {0}")]
    ConfigureSqlite(rusqlite::Error),
    #[error("cannot finalise SQLite transport pack: {0}")]
    FinalizeSqlite(rusqlite::Error),
    #[error("SQLite transport integrity check failed to run: {0}")]
    IntegrityCheck(rusqlite::Error),
    #[error("SQLite transport integrity check failed")]
    IntegrityFailure,
    #[error("cannot serialise transport row: {0}")]
    SerializeRow(serde_json::Error),
    #[error("cannot read temporary SQLite pack {path}: {source}")]
    ReadSource { path: PathBuf, source: io::Error },
    #[error("cannot create compressed pack {path}: {source}")]
    CreateCompressed { path: PathBuf, source: io::Error },
    #[error("cannot compress SQLite transport pack: {0}")]
    Compress(io::Error),
    #[error("cannot synchronise compressed pack: {0}")]
    SyncCompressed(io::Error),
    #[error("cannot open compressed pack for validation {path}: {source}")]
    OpenCompressed { path: PathBuf, source: io::Error },
    #[error("cannot publish immutable pack {path}: {source}")]
    Publish { path: PathBuf, source: io::Error },
    #[error("existing immutable pack at {0} is invalid")]
    ExistingPackInvalid(PathBuf),
    #[error("transport protocol validation failed: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("pack and snapshot identifiers must not be nil")]
    NilIdentifier,
    #[error("unsupported transport schema {0:?}")]
    UnsupportedSchema(SchemaVersion),
    #[error("transport sequence is invalid")]
    InvalidSequenceRange,
    #[error("uncompressed transport pack limit is smaller than one SQLite page")]
    UncompressedLimitTooSmall,
    #[error("incremental pack must advance its sequence")]
    NonProgressingDelta,
    #[error("pack table list is empty or exceeds its limit")]
    InvalidTableCount,
    #[error("pack table list contains duplicates")]
    DuplicateTable,
    #[error("pack exceeds the configured row bound")]
    TooManyRows,
    #[error("row table is not declared by the pack")]
    UndeclaredRowTable,
    #[error("row entity key is empty, too large, or contains a NUL")]
    InvalidEntityKey,
    #[error("row value key is empty or contains a NUL")]
    InvalidValueKey,
    #[error("transport real values must be finite")]
    NonFiniteReal,
    #[error("delete row must not carry values")]
    DeleteHasValues,
    #[error("upsert row must carry at least one value")]
    EmptyUpsert,
    #[error("row sequence falls outside its pack range")]
    RowOutsideSequenceRange,
    #[error("incremental rows must be ordered by source sequence")]
    RowsNotSequenceOrdered,
    #[error("row payload exceeds its bounded size")]
    RowPayloadTooLarge,
    #[error("source sequence exceeds SQLite signed integer range")]
    SequenceOutOfSqliteRange,
}

fn insert_metadata(
    transaction: &rusqlite::Transaction<'_>,
    request: &TransportPackRequest<'_>,
) -> Result<(), TransportError> {
    let values = [
        ("protocol", "teslatlas-sync".to_owned()),
        ("protocol_major", "1".to_owned()),
        ("protocol_minor", "0".to_owned()),
        ("schema_major", request.schema.major.to_string()),
        ("schema_minor", request.schema.minor.to_string()),
        ("pack_id", request.pack_id.to_string()),
        ("snapshot_id", request.snapshot_id.to_string()),
        ("ordinal", request.ordinal.to_string()),
        ("mode", mode_name(request.mode).to_owned()),
        (
            "from_exclusive",
            request.sequence.from_exclusive.to_string(),
        ),
        ("to_inclusive", request.sequence.to_inclusive.to_string()),
        ("row_count", request.rows.len().to_string()),
        (
            "tables",
            request
                .tables
                .iter()
                .map(|table| table_name(*table))
                .collect::<Vec<_>>()
                .join(","),
        ),
    ];
    let mut statement = transaction
        .prepare_cached("INSERT INTO pack_metadata (key, value) VALUES (?1, ?2)")
        .map_err(TransportError::PrepareRow)?;
    for (key, value) in values {
        statement
            .execute(params![key, value])
            .map_err(TransportError::InsertRow)?;
    }
    Ok(())
}

fn encode_payload(row: &TransportRow) -> Result<Option<String>, TransportError> {
    match row.operation {
        TransportOperation::Upsert => serde_json::to_string(&row.values)
            .map(Some)
            .map_err(TransportError::SerializeRow),
        TransportOperation::Delete => Ok(None),
    }
}

fn table_name(table: MirrorTable) -> &'static str {
    match table {
        MirrorTable::Vehicle => "vehicle",
        MirrorTable::Car => "car",
        MirrorTable::Drive => "drive",
        MirrorTable::ChargingProcess => "charging_process",
        MirrorTable::Position => "position",
        MirrorTable::Charge => "charge",
        MirrorTable::ChargeSample => "charge_sample",
        MirrorTable::State => "state",
        MirrorTable::Update => "update",
        MirrorTable::Tombstone => "tombstone",
    }
}

fn mode_name(mode: TransferMode) -> &'static str {
    match mode {
        TransferMode::FullSnapshot => "full_snapshot",
        TransferMode::Incremental => "incremental",
    }
}

fn compress_file(
    source_path: &Path,
    destination_path: &Path,
) -> Result<(Sha256Digest, u64), TransportError> {
    let mut source = File::open(source_path).map_err(|source| TransportError::ReadSource {
        path: source_path.to_path_buf(),
        source,
    })?;
    let destination = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(destination_path)
        .map_err(|source| TransportError::CreateCompressed {
            path: destination_path.to_path_buf(),
            source,
        })?;
    let counting = HashingWriter::new(destination);
    let mut encoder = zstd::stream::write::Encoder::new(counting, COMPRESSION_LEVEL)
        .map_err(TransportError::Compress)?;
    io::copy(&mut source, &mut encoder).map_err(TransportError::Compress)?;
    let counting = encoder.finish().map_err(TransportError::Compress)?;
    let (file, digest, bytes) = counting.finish();
    file.sync_all().map_err(TransportError::SyncCompressed)?;
    Ok((digest, bytes))
}

fn verify_file(
    metadata: &TransportPack,
    path: &Path,
    limits: ProtocolLimits,
) -> Result<VerifiedTransportPack, TransportError> {
    let file = File::open(path).map_err(|source| TransportError::OpenCompressed {
        path: path.to_path_buf(),
        source,
    })?;
    metadata
        .verify_reader(file, limits)
        .map_err(TransportError::Protocol)
}

fn publish_immutable(
    temporary_path: &Path,
    final_path: &Path,
    metadata: &TransportPack,
    limits: ProtocolLimits,
) -> Result<(), TransportError> {
    match fs::hard_link(temporary_path, final_path) {
        Ok(()) => sync_parent_directory(final_path),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if verify_file(metadata, final_path, limits).is_ok() {
                Ok(())
            } else {
                Err(TransportError::ExistingPackInvalid(
                    final_path.to_path_buf(),
                ))
            }
        }
        Err(source) => Err(TransportError::Publish {
            path: final_path.to_path_buf(),
            source,
        }),
    }
}

fn sync_parent_directory(path: &Path) -> Result<(), TransportError> {
    let parent = path.parent().ok_or_else(|| TransportError::Publish {
        path: path.to_path_buf(),
        source: io::Error::other("immutable pack has no parent directory"),
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| TransportError::Publish {
            path: path.to_path_buf(),
            source,
        })
}

struct TemporaryPath {
    path: PathBuf,
}

impl TemporaryPath {
    fn create(directory: &Path, extension: &str) -> Result<Self, TransportError> {
        for _ in 0..32 {
            let path = directory.join(format!("{}.{}.tmp", Uuid::new_v4(), extension));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => {
                    drop(file);
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(TransportError::CreateTemporary { path, source }),
            }
        }
        let path = directory.join(format!("exhausted.{}.tmp", extension));
        Err(TransportError::CreateTemporary {
            path,
            source: io::Error::new(io::ErrorKind::AlreadyExists, "temporary name collision"),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        // These names are private, UUID-addressed staging files.  Cleanup is
        // scoped to the exact temporary object, never the packs directory.
        let _ = fs::remove_file(&self.path);
    }
}

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    bytes_written: u64,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes_written: 0,
        }
    }

    fn finish(self) -> (W, Sha256Digest, u64) {
        (
            self.inner,
            Sha256Digest::from_bytes(self.hasher.finalize().into()),
            self.bytes_written,
        )
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.bytes_written += u64::try_from(written).expect("usize fits into u64");
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn uuid(value: &str) -> Uuid {
        Uuid::parse_str(value).expect("test UUID")
    }

    fn request<'a>(rows: &'a [TransportRow]) -> TransportPackRequest<'a> {
        TransportPackRequest {
            pack_id: uuid("11111111-1111-4111-8111-111111111111"),
            snapshot_id: uuid("22222222-2222-4222-8222-222222222222"),
            ordinal: 3,
            schema: TRANSPORT_SCHEMA_V1,
            mode: TransferMode::Incremental,
            sequence: SequenceRange {
                from_exclusive: 100,
                to_inclusive: 103,
            },
            tables: &[
                MirrorTable::Vehicle,
                MirrorTable::Position,
                MirrorTable::Tombstone,
            ],
            rows,
        }
    }

    fn rows() -> Vec<TransportRow> {
        vec![
            TransportRow {
                table: MirrorTable::Vehicle,
                entity_key: "vehicle-1".into(),
                source_sequence: 101,
                operation: TransportOperation::Upsert,
                values: BTreeMap::from([
                    ("odometer_km".into(), TransportValue::Real(12_345.6)),
                    ("online".into(), TransportValue::Boolean(true)),
                ]),
            },
            TransportRow {
                table: MirrorTable::Position,
                entity_key: "position-101".into(),
                source_sequence: 102,
                operation: TransportOperation::Upsert,
                values: BTreeMap::from([
                    ("latitude".into(), TransportValue::Real(51.5074)),
                    ("longitude".into(), TransportValue::Real(-0.1278)),
                    (
                        "timestamp_ms".into(),
                        TransportValue::Integer(1_721_234_567_890),
                    ),
                ]),
            },
            TransportRow {
                table: MirrorTable::Tombstone,
                entity_key: "state-77".into(),
                source_sequence: 103,
                operation: TransportOperation::Delete,
                values: BTreeMap::new(),
            },
        ]
    }

    #[test]
    fn writes_and_verifies_content_addressed_sqlite_zstd_pack() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let writer = TransportPackWriter::new(temporary.path().join("packs"));
        let rows = rows();
        let built = writer.write_pack(&request(&rows)).expect("write pack");

        assert!(built.path.is_file());
        assert_eq!(
            built.path,
            writer.content_path(built.metadata.sha256),
            "pack must live at its digest path"
        );
        assert_eq!(built.metadata.row_count, rows.len() as u64);
        assert_eq!(built.verified.sha256, built.metadata.sha256);
        built
            .metadata
            .validate(ProtocolLimits::default())
            .expect("protocol metadata");
        built
            .metadata
            .verify_reader(
                File::open(&built.path).expect("pack file"),
                ProtocolLimits::default(),
            )
            .expect("protocol verifier");

        let compressed = File::open(&built.path).expect("pack file");
        let unpacked = zstd::stream::decode_all(compressed).expect("decode pack");
        let pack_path = temporary.path().join("inspect.sqlite");
        fs::write(&pack_path, unpacked).expect("write test sqlite");
        let connection = Connection::open(pack_path).expect("open sqlite");
        let application_id: u32 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .expect("application id");
        let user_version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user version");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM sync_rows", [], |row| row.get(0))
            .expect("row count");
        let mode: String = connection
            .query_row(
                "SELECT value FROM pack_metadata WHERE key = 'mode'",
                [],
                |row| row.get(0),
            )
            .expect("mode metadata");
        assert_eq!(application_id, SQLITE_TRANSPORT_APPLICATION_ID);
        assert_eq!(user_version, TRANSPORT_SCHEMA_V1.sqlite_user_version());
        assert_eq!(count, rows.len() as i64);
        assert_eq!(mode, "incremental");
    }

    #[test]
    fn rejects_invalid_rows_before_creating_a_pack() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let writer = TransportPackWriter::new(temporary.path().join("packs"));
        let mut invalid_rows = rows();
        invalid_rows[0].table = MirrorTable::Charge;
        assert!(matches!(
            writer.write_pack(&request(&invalid_rows)),
            Err(TransportError::UndeclaredRowTable)
        ));

        let mut invalid_rows = rows();
        invalid_rows[0].source_sequence = 100;
        assert!(matches!(
            writer.write_pack(&request(&invalid_rows)),
            Err(TransportError::RowOutsideSequenceRange)
        ));

        let mut invalid_rows = rows();
        invalid_rows[2]
            .values
            .insert("bad".into(), TransportValue::Null);
        assert!(matches!(
            writer.write_pack(&request(&invalid_rows)),
            Err(TransportError::DeleteHasValues)
        ));

        let mut invalid_rows = rows();
        invalid_rows[0]
            .values
            .insert("latitude".into(), TransportValue::Real(f64::NAN));
        assert!(matches!(
            writer.write_pack(&request(&invalid_rows)),
            Err(TransportError::NonFiniteReal)
        ));
        assert!(!writer.packs_dir().exists());
    }

    #[test]
    fn refuses_to_start_when_the_protocol_limit_cannot_hold_one_sqlite_page() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let limits = ProtocolLimits {
            max_uncompressed_pack_bytes: 2_048,
            ..ProtocolLimits::default()
        };
        let writer = TransportPackWriter::with_limits(temporary.path().join("packs"), limits);
        let rows = rows();
        assert!(matches!(
            writer.write_pack(&request(&rows)),
            Err(TransportError::UncompressedLimitTooSmall)
        ));
    }

    #[test]
    fn existing_content_addressed_object_is_verified_not_overwritten() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let writer = TransportPackWriter::new(temporary.path().join("packs"));
        let rows = rows();
        let first = writer.write_pack(&request(&rows)).expect("first pack");
        let original = fs::read(&first.path).expect("first bytes");
        let second = writer.write_pack(&request(&rows)).expect("second pack");
        assert_eq!(fs::read(&first.path).expect("first bytes"), original);
        assert_eq!(first.metadata.sha256, second.metadata.sha256);
    }

    #[test]
    fn writer_streaming_output_is_accepted_by_protocol_verifier() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let writer = TransportPackWriter::new(temporary.path().join("packs"));
        let rows = rows();
        let built = writer.write_pack(&request(&rows)).expect("write pack");
        let bytes = fs::read(&built.path).expect("bounded test pack");
        let verified = built
            .metadata
            .verify_reader(Cursor::new(bytes), ProtocolLimits::default())
            .expect("verified pack");
        assert_eq!(verified, built.verified);
    }
}
