//! Teslatlas Sync Protocol v1.
//!
//! The manifest is deliberately small and JSON serialisable.  History never
//! travels as a JSON array: every chunk is an immutable, zstd-compressed
//! SQLite transport database.  The client must validate a manifest before it
//! starts downloading, then verify every pack before it is attached to the
//! local mirror database.

use std::{
    collections::HashSet,
    fmt,
    io::{self, Read},
    str::FromStr,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

/// The protocol identifier exposed in discovery documents and manifests.
pub const PROTOCOL_NAME: &str = "teslatlas-sync";
pub const PROTOCOL_V1: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };
/// Additive lineage envelope. This is separate from `SyncManifest` so v1
/// manifest bytes and schema remain unchanged forever.
pub const LINEAGE_PROTOCOL_V2: ProtocolVersion = ProtocolVersion { major: 2, minor: 0 };

/// The first stable layout for a SQLite transport pack.
pub const TRANSPORT_SCHEMA_V1: SchemaVersion = SchemaVersion { major: 1, minor: 0 };

/// The first typed Hub projection layout.  Unlike the generic transport
/// layout, this has fixed `cars`, `drives`, `positions`, `charges`, and
/// `charge_samples` tables for the Teslatlas core importer.
pub const HUB_PROJECTION_SCHEMA_V1: SchemaVersion = SchemaVersion { major: 2, minor: 0 };

/// Additive typed Hub projection layout with state history.
pub const HUB_PROJECTION_SCHEMA_V2: SchemaVersion = SchemaVersion { major: 2, minor: 1 };

/// SQLite `application_id` for a Teslatlas Sync Protocol v1 transport pack.
///
/// This is distinct from the Hub database application id.  A pack is an
/// interchange file, not a copy of either server or client state.
pub const SQLITE_TRANSPORT_APPLICATION_ID: u32 = 0x5453_5031; // TSP1

/// SQLite `application_id` for a typed Hub projection pack.
pub const SQLITE_HUB_PROJECTION_APPLICATION_ID: u32 = 0x5448_5031; // THP1

const SQLITE_HEADER_BYTES: usize = 100;
const SQLITE_HEADER_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const CURSOR_PREFIX: &str = "tsp1";
const CURSOR_MAGIC: &[u8; 4] = b"TSPC";
const CURSOR_FORMAT_VERSION: u8 = 1;
const CURSOR_PAYLOAD_BYTES: usize = 77;
const CURSOR_TAG_BYTES: usize = 32;
const MANIFEST_SIGNING_SEED_DOMAIN: &[u8] = b"teslatlas-hub/manifest-ed25519-signing-seed/v1";

/// Version of the manifest and cursor envelope, not the Hub binary version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const fn is_supported(self) -> bool {
        self.major == PROTOCOL_V1.major && self.minor == PROTOCOL_V1.minor
    }
}

/// Version of the pack layout.  A client must reject an unknown layout before
/// opening or attaching its SQLite file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaVersion {
    pub major: u16,
    pub minor: u16,
}

impl SchemaVersion {
    pub const fn is_supported(self) -> bool {
        (self.major == TRANSPORT_SCHEMA_V1.major && self.minor == TRANSPORT_SCHEMA_V1.minor)
            || (self.major == HUB_PROJECTION_SCHEMA_V1.major
                && (self.minor == HUB_PROJECTION_SCHEMA_V1.minor
                    || self.minor == HUB_PROJECTION_SCHEMA_V2.minor))
    }

    /// Stored in the SQLite `user_version` field of every transport pack.
    pub const fn sqlite_user_version(self) -> u32 {
        ((self.major as u32) << 16) | self.minor as u32
    }

    const fn sqlite_application_id(self) -> Option<u32> {
        if self.major == TRANSPORT_SCHEMA_V1.major && self.minor == TRANSPORT_SCHEMA_V1.minor {
            Some(SQLITE_TRANSPORT_APPLICATION_ID)
        } else if self.major == HUB_PROJECTION_SCHEMA_V1.major
            && (self.minor == HUB_PROJECTION_SCHEMA_V1.minor
                || self.minor == HUB_PROJECTION_SCHEMA_V2.minor)
        {
            Some(SQLITE_HUB_PROJECTION_APPLICATION_ID)
        } else {
            None
        }
    }
}

/// A lowercase, fixed-width SHA-256 value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(bytes);
        Self(digest.finalize().into())
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|byte| *byte == 0)
    }

    pub fn matches(&self, bytes: &[u8]) -> bool {
        constant_time_eq(self.as_bytes(), Sha256Digest::of_bytes(bytes).as_bytes())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Sha256Digest")
            .field(&self.to_string())
            .finish()
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl FromStr for Sha256Digest {
    type Err = ProtocolError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ProtocolError::InvalidDigest);
        }
        // Canonical spelling matters because the digest is part of the pack
        // URL and its ETag.  It prevents cache aliases for the same object.
        if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Err(ProtocolError::NonCanonicalDigest);
        }
        let mut bytes = [0_u8; 32];
        hex::decode_to_slice(value, &mut bytes).map_err(|_| ProtocolError::InvalidDigest)?;
        Ok(Self(bytes))
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

/// Full snapshots use a new staged mirror.  Deltas apply only after the
/// preceding cursor has been committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferMode {
    FullSnapshot,
    Incremental,
}

/// Wire format of every immutable chunk in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackFormat {
    SqliteTransport,
    HubProjectionSqlite,
}

/// Compression is intrinsic to the object, never an HTTP content encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackCompression {
    Zstd,
}

/// Source-owned tables represented by a transport pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MirrorTable {
    Vehicle,
    Car,
    Drive,
    ChargingProcess,
    Position,
    Charge,
    ChargeSample,
    State,
    Update,
    Tombstone,
}

impl MirrorTable {
    const fn snapshot_stage(self) -> u8 {
        match self {
            Self::Vehicle | Self::Car => 0,
            // A typed `Charge` is a charging-process parent.  It must be
            // activated before its `ChargeSample` children, just as a drive
            // must precede its positions.  This order is part of the on-disk
            // import contract, not merely an encoder preference.
            Self::Drive | Self::ChargingProcess | Self::Charge => 1,
            Self::Position | Self::ChargeSample | Self::State | Self::Update => 2,
            Self::Tombstone => 3,
        }
    }

    const fn is_generic_transport(self) -> bool {
        !matches!(self, Self::Car | Self::ChargeSample)
    }

    const fn is_hub_projection(self) -> bool {
        matches!(
            self,
            Self::Car
                | Self::Drive
                | Self::Position
                | Self::Charge
                | Self::ChargeSample
                | Self::State
                | Self::Update
                | Self::Tombstone
        )
    }
}

/// A half-open sync interval expressed as `(from_exclusive, to_inclusive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceRange {
    pub from_exclusive: u64,
    pub to_inclusive: u64,
}

impl SequenceRange {
    pub const fn is_ordered(self) -> bool {
        self.from_exclusive <= self.to_inclusive
    }
}

/// One immutable, content-addressed SQLite database compressed with zstd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportPack {
    pub pack_id: Uuid,
    pub snapshot_id: Uuid,
    pub ordinal: u32,
    pub schema: SchemaVersion,
    pub format: PackFormat,
    pub compression: PackCompression,
    /// Same-origin, canonical, content-addressed path.  The client must not
    /// accept an absolute URL here.
    pub relative_path: String,
    pub sha256: Sha256Digest,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub row_count: u64,
    pub sequence: SequenceRange,
    pub tables: Vec<MirrorTable>,
}

impl TransportPack {
    pub fn canonical_relative_path(digest: Sha256Digest) -> String {
        format!("/v1/packs/sha256/{digest}.sqlite.zst")
    }

    /// The strong ETag to emit when serving this object.
    pub fn etag(&self) -> String {
        format!("\"{}\"", self.sha256)
    }

    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        require_non_nil_uuid("pack_id", self.pack_id)?;
        require_non_nil_uuid("snapshot_id", self.snapshot_id)?;
        if !self.schema.is_supported() {
            return Err(ProtocolError::UnsupportedSchema(self.schema));
        }
        match self.format {
            PackFormat::SqliteTransport if self.schema != TRANSPORT_SCHEMA_V1 => {
                return Err(ProtocolError::FormatSchemaMismatch);
            }
            PackFormat::HubProjectionSqlite
                if self.schema != HUB_PROJECTION_SCHEMA_V1
                    && self.schema != HUB_PROJECTION_SCHEMA_V2 =>
            {
                return Err(ProtocolError::FormatSchemaMismatch);
            }
            _ => {}
        }
        if self.sha256.is_zero() {
            return Err(ProtocolError::ZeroDigest);
        }
        if self.relative_path != Self::canonical_relative_path(self.sha256) {
            return Err(ProtocolError::NonCanonicalPackPath);
        }
        if self.relative_path.len() > limits.max_relative_path_bytes
            || !self.relative_path.is_ascii()
            || self.relative_path.contains('?')
            || self.relative_path.contains('#')
            || self.relative_path.contains('\\')
        {
            return Err(ProtocolError::UnsafePackPath);
        }
        if self.compressed_bytes == 0 || self.compressed_bytes > limits.max_compressed_pack_bytes {
            return Err(ProtocolError::CompressedSizeOutOfBounds(
                self.compressed_bytes,
            ));
        }
        if self.uncompressed_bytes < SQLITE_HEADER_BYTES as u64
            || self.uncompressed_bytes > limits.max_uncompressed_pack_bytes
        {
            return Err(ProtocolError::UncompressedSizeOutOfBounds(
                self.uncompressed_bytes,
            ));
        }
        if self.uncompressed_bytes
            > self
                .compressed_bytes
                .saturating_mul(limits.max_expansion_ratio as u64)
        {
            return Err(ProtocolError::ExpansionRatioExceeded);
        }
        if self.row_count > limits.max_rows_per_pack {
            return Err(ProtocolError::RowCountOutOfBounds(self.row_count));
        }
        if !self.sequence.is_ordered() {
            return Err(ProtocolError::InvalidSequenceRange);
        }
        if self.tables.is_empty() || self.tables.len() > limits.max_tables_per_pack {
            return Err(ProtocolError::InvalidTableCount);
        }
        let mut unique_tables = HashSet::with_capacity(self.tables.len());
        if !self.tables.iter().all(|table| unique_tables.insert(*table)) {
            return Err(ProtocolError::DuplicateTable);
        }
        let table_set_matches_format = match self.format {
            PackFormat::SqliteTransport => {
                self.tables.iter().all(|table| table.is_generic_transport())
            }
            PackFormat::HubProjectionSqlite => {
                self.tables.iter().all(|table| table.is_hub_projection())
            }
        };
        if !table_set_matches_format {
            return Err(ProtocolError::PackTablesDoNotMatchFormat);
        }
        Ok(())
    }

    /// Verifies raw pack bytes before they are persisted or attached.  This is
    /// streaming: decompression has a fixed 64 KiB working buffer and rejects
    /// a compressed or expanded object beyond its declared limit.
    pub fn verify_reader<R: Read>(
        &self,
        reader: R,
        limits: ProtocolLimits,
    ) -> Result<VerifiedTransportPack, ProtocolError> {
        self.validate(limits)?;

        let mut source = HashingLimitedReader::new(reader, self.compressed_bytes);
        let mut decoder = zstd::stream::read::Decoder::new(&mut source)
            .map_err(|_| ProtocolError::PackDecompression)?;
        let mut header = [0_u8; SQLITE_HEADER_BYTES];
        let mut header_len = 0_usize;
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];

        loop {
            let read = decoder
                .read(&mut buffer)
                .map_err(|_| ProtocolError::PackDecompression)?;
            if read == 0 {
                break;
            }
            let read_u64 = u64::try_from(read).map_err(|_| ProtocolError::PackTooLarge)?;
            total = total
                .checked_add(read_u64)
                .ok_or(ProtocolError::PackTooLarge)?;
            if total > self.uncompressed_bytes || total > limits.max_uncompressed_pack_bytes {
                return Err(ProtocolError::PackTooLarge);
            }
            if header_len < SQLITE_HEADER_BYTES {
                let take = (SQLITE_HEADER_BYTES - header_len).min(read);
                header[header_len..header_len + take].copy_from_slice(&buffer[..take]);
                header_len += take;
            }
        }
        drop(decoder);
        source.drain_to_eof().map_err(map_source_read_error)?;

        if source.bytes_read() != self.compressed_bytes {
            return Err(ProtocolError::CompressedSizeMismatch {
                expected: self.compressed_bytes,
                actual: source.bytes_read(),
            });
        }
        let actual_digest = source.finish();
        if !constant_time_eq(self.sha256.as_bytes(), actual_digest.as_bytes()) {
            return Err(ProtocolError::PackHashMismatch);
        }
        if total != self.uncompressed_bytes {
            return Err(ProtocolError::UncompressedSizeMismatch {
                expected: self.uncompressed_bytes,
                actual: total,
            });
        }
        validate_sqlite_header(&header, self.schema, total)?;

        Ok(VerifiedTransportPack {
            pack_id: self.pack_id,
            sha256: self.sha256,
            compressed_bytes: self.compressed_bytes,
            uncompressed_bytes: total,
        })
    }
}

/// Result suitable for writing to the staged-import checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedTransportPack {
    pub pack_id: Uuid,
    pub sha256: Sha256Digest,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
}

/// Bounds negotiated by the app release.  A server may emit less, never more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolLimits {
    pub max_chunks: usize,
    pub max_relative_path_bytes: usize,
    pub max_compressed_pack_bytes: u64,
    pub max_uncompressed_pack_bytes: u64,
    pub max_expansion_ratio: u32,
    pub max_rows_per_pack: u64,
    pub max_tables_per_pack: usize,
}

impl Default for ProtocolLimits {
    fn default() -> Self {
        Self {
            // This is a safety ceiling, not a packing target.  The encoder
            // should normally use 8-16 MiB compressed packs.
            max_chunks: 512,
            max_relative_path_bytes: 256,
            max_compressed_pack_bytes: 64 * 1024 * 1024,
            max_uncompressed_pack_bytes: 256 * 1024 * 1024,
            max_expansion_ratio: 64,
            max_rows_per_pack: 1_000_000,
            max_tables_per_pack: 8,
        }
    }
}

/// Complete description of either a staged snapshot or an incremental range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncManifest {
    pub protocol: ProtocolVersion,
    pub schema: SchemaVersion,
    pub installation_id: Uuid,
    pub account_id: Uuid,
    pub vehicle_id: Uuid,
    pub generation: u64,
    pub snapshot_id: Uuid,
    pub mode: TransferMode,
    pub base_sequence: u64,
    pub head_sequence: u64,
    pub chunk_count: u32,
    pub total_compressed_bytes: u64,
    pub total_uncompressed_bytes: u64,
    pub total_rows: u64,
    pub chunks: Vec<TransportPack>,
    pub terminal_cursor: OpaqueCursor,
}

/// Capability discriminator for the additive base-plus-deltas protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageCapability {
    ImmutableBaseOrderedDeltas,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineageBase {
    pub snapshot_id: Uuid,
    pub sequence: u64,
    pub digest: Sha256Digest,
    pub packs: Vec<TransportPack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineageDelta {
    pub from_sequence: u64,
    pub to_sequence: u64,
    pub parent_chain_digest: Sha256Digest,
    pub chain_digest: Sha256Digest,
    pub pack_digest: Sha256Digest,
    pub pack: TransportPack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineageManifestV2 {
    pub protocol: ProtocolVersion,
    pub capability: LineageCapability,
    pub schema: SchemaVersion,
    pub installation_id: Uuid,
    pub account_id: Uuid,
    pub vehicle_id: Uuid,
    pub generation: u64,
    pub base: LineageBase,
    pub deltas: Vec<LineageDelta>,
    pub head_sequence: u64,
    pub head_digest: Sha256Digest,
    pub terminal_cursor: OpaqueCursor,
}

impl LineageManifestV2 {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol != LINEAGE_PROTOCOL_V2 {
            return Err(ProtocolError::UnsupportedLineageProtocol(self.protocol));
        }
        if self.capability != LineageCapability::ImmutableBaseOrderedDeltas {
            return Err(ProtocolError::UnsupportedLineageCapability);
        }
        if !self.schema.is_supported() {
            return Err(ProtocolError::UnsupportedSchema(self.schema));
        }
        require_non_nil_uuid("installation_id", self.installation_id)?;
        require_non_nil_uuid("account_id", self.account_id)?;
        require_non_nil_uuid("vehicle_id", self.vehicle_id)?;
        require_non_nil_uuid("base snapshot_id", self.base.snapshot_id)?;
        if self.generation == 0 || self.base.packs.is_empty() {
            return Err(ProtocolError::LineageBaseRequired);
        }
        if self.base.digest.is_zero() {
            return Err(ProtocolError::LineageDigestRequired);
        }
        for (ordinal, pack) in self.base.packs.iter().enumerate() {
            pack.validate(ProtocolLimits::default())?;
            if pack.ordinal != ordinal as u32
                || pack.snapshot_id != self.base.snapshot_id
                || pack.sequence != (SequenceRange {
                    from_exclusive: self.base.sequence,
                    to_inclusive: self.base.sequence,
                })
            {
                return Err(ProtocolError::LineageBasePackMismatch);
            }
        }
        let mut expected_from = self.base.sequence;
        let mut parent = self.base.digest;
        let mut seen_packs = HashSet::new();
        for delta in &self.deltas {
            if delta.from_sequence != expected_from || delta.to_sequence <= delta.from_sequence {
                return Err(ProtocolError::LineageSequenceGap);
            }
            if delta.parent_chain_digest != parent
                || delta.pack_digest != delta.pack.sha256
                || !seen_packs.insert(delta.pack.pack_id)
            {
                return Err(ProtocolError::LineageChainMismatch);
            }
            delta.pack.validate(ProtocolLimits::default())?;
            if delta.pack.sequence != (SequenceRange {
                from_exclusive: delta.from_sequence,
                to_inclusive: delta.to_sequence,
            }) {
                return Err(ProtocolError::LineageDeltaPackMismatch);
            }
            expected_from = delta.to_sequence;
            parent = delta.chain_digest;
        }
        if self.head_sequence != expected_from || self.head_digest != parent {
            return Err(ProtocolError::LineageHeadMismatch);
        }
        self.terminal_cursor.validate_shape()
    }
}

impl SyncManifest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.validate_with_limits(ProtocolLimits::default())
    }

    pub fn validate_with_limits(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        if !self.protocol.is_supported() {
            return Err(ProtocolError::UnsupportedProtocol(self.protocol));
        }
        if !self.schema.is_supported() {
            return Err(ProtocolError::UnsupportedSchema(self.schema));
        }
        require_non_nil_uuid("installation_id", self.installation_id)?;
        require_non_nil_uuid("account_id", self.account_id)?;
        require_non_nil_uuid("vehicle_id", self.vehicle_id)?;
        require_non_nil_uuid("snapshot_id", self.snapshot_id)?;
        if self.generation == 0 {
            return Err(ProtocolError::InvalidGeneration);
        }
        if self.base_sequence > self.head_sequence {
            return Err(ProtocolError::InvalidManifestSequence);
        }
        if self.chunks.len() > limits.max_chunks
            || usize::try_from(self.chunk_count).ok() != Some(self.chunks.len())
        {
            return Err(ProtocolError::InvalidChunkCount {
                declared: self.chunk_count,
                actual: self.chunks.len(),
            });
        }
        if matches!(self.mode, TransferMode::Incremental)
            && self.head_sequence > self.base_sequence
            && self.chunks.is_empty()
        {
            return Err(ProtocolError::MissingDeltaChunks);
        }

        let mut ids = HashSet::with_capacity(self.chunks.len());
        let mut compressed_total = 0_u64;
        let mut uncompressed_total = 0_u64;
        let mut row_total = 0_u64;
        let mut previous_snapshot_stage = 0_u8;

        for (index, chunk) in self.chunks.iter().enumerate() {
            if chunk.ordinal != index as u32 {
                return Err(ProtocolError::NonContiguousChunkOrder);
            }
            if chunk.snapshot_id != self.snapshot_id {
                return Err(ProtocolError::ChunkSnapshotMismatch);
            }
            if chunk.schema != self.schema {
                return Err(ProtocolError::ChunkSchemaMismatch);
            }
            if !ids.insert(chunk.pack_id) {
                return Err(ProtocolError::DuplicatePackId);
            }
            chunk.validate(limits)?;
            compressed_total = checked_add(compressed_total, chunk.compressed_bytes)?;
            uncompressed_total = checked_add(uncompressed_total, chunk.uncompressed_bytes)?;
            row_total = checked_add(row_total, chunk.row_count)?;

            match self.mode {
                TransferMode::FullSnapshot => {
                    if chunk.sequence
                        != (SequenceRange {
                            from_exclusive: self.base_sequence,
                            to_inclusive: self.head_sequence,
                        })
                    {
                        return Err(ProtocolError::SnapshotSequenceMismatch);
                    }
                    let stage = chunk
                        .tables
                        .iter()
                        .map(|table| table.snapshot_stage())
                        .min()
                        .expect("validated non-empty tables");
                    if stage < previous_snapshot_stage {
                        return Err(ProtocolError::SnapshotDependencyOrder);
                    }
                    previous_snapshot_stage = stage;
                }
                TransferMode::Incremental => {
                    let expected_from = if index == 0 {
                        self.base_sequence
                    } else {
                        self.chunks[index - 1].sequence.to_inclusive
                    };
                    if chunk.sequence.from_exclusive != expected_from
                        || chunk.sequence.to_inclusive <= chunk.sequence.from_exclusive
                    {
                        return Err(ProtocolError::DeltaSequenceGap);
                    }
                }
            }
        }

        if matches!(self.mode, TransferMode::Incremental)
            && !self.chunks.is_empty()
            && self
                .chunks
                .last()
                .expect("checked non-empty")
                .sequence
                .to_inclusive
                != self.head_sequence
        {
            return Err(ProtocolError::DeltaSequenceGap);
        }
        if compressed_total != self.total_compressed_bytes
            || uncompressed_total != self.total_uncompressed_bytes
            || self.total_rows == 0
            || self.total_rows > row_total
        {
            return Err(ProtocolError::ManifestTotalsMismatch);
        }
        self.terminal_cursor.validate_shape()?;
        Ok(())
    }

    /// Verifies that the signed cursor is exactly the checkpoint described by
    /// this manifest.  Call it after `validate` and before persisting a cursor.
    pub fn validate_terminal_cursor(&self, key: &CursorKey) -> Result<(), ProtocolError> {
        let claims = self.terminal_cursor.verify(key)?;
        if claims.protocol != self.protocol
            || claims.schema != self.schema
            || claims.installation_id != self.installation_id
            || claims.account_id != self.account_id
            || claims.vehicle_id != self.vehicle_id
            || claims.generation != self.generation
            || claims.sequence != self.head_sequence
        {
            return Err(ProtocolError::CursorDoesNotMatchManifest);
        }
        Ok(())
    }
}

/// Claims embedded inside a Hub-issued cursor.  This type is server-side; the
/// JSON manifest always contains only [`OpaqueCursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorClaims {
    pub protocol: ProtocolVersion,
    pub schema: SchemaVersion,
    pub installation_id: Uuid,
    pub account_id: Uuid,
    pub vehicle_id: Uuid,
    pub generation: u64,
    pub sequence: u64,
}

impl CursorClaims {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if !self.protocol.is_supported() {
            return Err(ProtocolError::UnsupportedProtocol(self.protocol));
        }
        if !self.schema.is_supported() {
            return Err(ProtocolError::UnsupportedSchema(self.schema));
        }
        require_non_nil_uuid("cursor installation_id", self.installation_id)?;
        require_non_nil_uuid("cursor account_id", self.account_id)?;
        require_non_nil_uuid("cursor vehicle_id", self.vehicle_id)?;
        if self.generation == 0 {
            return Err(ProtocolError::InvalidGeneration);
        }
        Ok(())
    }
}

/// Signing material for cursor integrity.  Keep it in a systemd credential or
/// another protected secret store; never in Hub configuration or logs.
#[derive(Clone)]
pub struct CursorKey([u8; 32]);

impl CursorKey {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Derive a purpose-specific Ed25519 seed without exposing or directly
    /// reusing the cursor HMAC key.
    pub(crate) fn manifest_signing_seed(&self) -> [u8; 32] {
        hmac_sha256(&self.0, MANIFEST_SIGNING_SEED_DOMAIN)
    }
}

impl fmt::Debug for CursorKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CursorKey([REDACTED])")
    }
}

/// A signed, versioned token.  Clients treat it as an opaque string; it is not
/// a bearer credential and must not be logged.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OpaqueCursor(String);

impl OpaqueCursor {
    pub fn issue(key: &CursorKey, claims: CursorClaims) -> Result<Self, ProtocolError> {
        claims.validate()?;
        let payload = encode_cursor_claims(claims);
        let tag = hmac_sha256(&key.0, &payload);
        Ok(Self(format!(
            "{CURSOR_PREFIX}.{}.{}",
            hex::encode(payload),
            hex::encode(tag)
        )))
    }

    pub fn verify(&self, key: &CursorKey) -> Result<CursorClaims, ProtocolError> {
        let (payload, tag) = self.decode()?;
        let expected = hmac_sha256(&key.0, &payload);
        if !constant_time_eq(&expected, &tag) {
            return Err(ProtocolError::InvalidCursorSignature);
        }
        decode_cursor_claims(&payload)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate_shape(&self) -> Result<(), ProtocolError> {
        self.decode().map(|_| ())
    }

    fn decode(
        &self,
    ) -> Result<([u8; CURSOR_PAYLOAD_BYTES], [u8; CURSOR_TAG_BYTES]), ProtocolError> {
        let mut parts = self.0.split('.');
        let Some(prefix) = parts.next() else {
            return Err(ProtocolError::MalformedCursor);
        };
        let Some(payload_hex) = parts.next() else {
            return Err(ProtocolError::MalformedCursor);
        };
        let Some(tag_hex) = parts.next() else {
            return Err(ProtocolError::MalformedCursor);
        };
        if prefix != CURSOR_PREFIX
            || parts.next().is_some()
            || payload_hex.len() != CURSOR_PAYLOAD_BYTES * 2
            || tag_hex.len() != CURSOR_TAG_BYTES * 2
            || payload_hex
                .bytes()
                .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
            || tag_hex
                .bytes()
                .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
        {
            return Err(ProtocolError::MalformedCursor);
        }
        let mut payload = [0_u8; CURSOR_PAYLOAD_BYTES];
        let mut tag = [0_u8; CURSOR_TAG_BYTES];
        hex::decode_to_slice(payload_hex, &mut payload)
            .map_err(|_| ProtocolError::MalformedCursor)?;
        hex::decode_to_slice(tag_hex, &mut tag).map_err(|_| ProtocolError::MalformedCursor)?;
        Ok((payload, tag))
    }
}

impl fmt::Debug for OpaqueCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueCursor([REDACTED])")
    }
}

impl Serialize for OpaqueCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OpaqueCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let cursor = Self(String::deserialize(deserializer)?);
        cursor.validate_shape().map_err(de::Error::custom)?;
        Ok(cursor)
    }
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("unsupported protocol version {0:?}")]
    UnsupportedProtocol(ProtocolVersion),
    #[error("unsupported transport schema {0:?}")]
    UnsupportedSchema(SchemaVersion),
    #[error("{0} must not be the nil UUID")]
    NilUuid(&'static str),
    #[error("generation must be at least one")]
    InvalidGeneration,
    #[error("manifest base sequence is after its head sequence")]
    InvalidManifestSequence,
    #[error("declared chunk count {declared} does not equal actual count {actual}")]
    InvalidChunkCount { declared: u32, actual: usize },
    #[error("an incremental range with changes must contain chunks")]
    MissingDeltaChunks,
    #[error("chunk ordinals must start at zero and be contiguous")]
    NonContiguousChunkOrder,
    #[error("a chunk belongs to a different snapshot")]
    ChunkSnapshotMismatch,
    #[error("a chunk schema differs from its manifest")]
    ChunkSchemaMismatch,
    #[error("duplicate transport pack id")]
    DuplicatePackId,
    #[error("a full snapshot chunk has a different source sequence")]
    SnapshotSequenceMismatch,
    #[error("full snapshot chunks violate table dependency order")]
    SnapshotDependencyOrder,
    #[error("incremental chunks have a gap, overlap, or non-progressing sequence")]
    DeltaSequenceGap,
    #[error("manifest totals do not equal the sum of its chunks")]
    ManifestTotalsMismatch,
    #[error("invalid SHA-256 digest")]
    InvalidDigest,
    #[error("SHA-256 digest must use canonical lowercase hexadecimal")]
    NonCanonicalDigest,
    #[error("SHA-256 digest must not be all zeroes")]
    ZeroDigest,
    #[error("transport pack path is not canonical for its digest")]
    NonCanonicalPackPath,
    #[error("transport pack path is unsafe")]
    UnsafePackPath,
    #[error("compressed pack size {0} is outside bounds")]
    CompressedSizeOutOfBounds(u64),
    #[error("uncompressed pack size {0} is outside bounds")]
    UncompressedSizeOutOfBounds(u64),
    #[error("compressed pack exceeds the allowed expansion ratio")]
    ExpansionRatioExceeded,
    #[error("pack row count {0} is outside bounds")]
    RowCountOutOfBounds(u64),
    #[error("invalid source sequence range")]
    InvalidSequenceRange,
    #[error("a pack must name one to the maximum number of tables")]
    InvalidTableCount,
    #[error("a pack names the same table more than once")]
    DuplicateTable,
    #[error("the pack format does not match its schema version")]
    FormatSchemaMismatch,
    #[error("the pack tables do not match its declared format")]
    PackTablesDoNotMatchFormat,
    #[error("unable to decode zstd transport pack")]
    PackDecompression,
    #[error("transport pack exceeded a declared or configured size bound")]
    PackTooLarge,
    #[error("compressed size mismatch: expected {expected}, found {actual}")]
    CompressedSizeMismatch { expected: u64, actual: u64 },
    #[error("transport pack SHA-256 mismatch")]
    PackHashMismatch,
    #[error("uncompressed size mismatch: expected {expected}, found {actual}")]
    UncompressedSizeMismatch { expected: u64, actual: u64 },
    #[error("transport pack is not a valid Teslatlas SQLite database")]
    InvalidSqliteHeader,
    #[error("transport pack SQLite application id is wrong")]
    InvalidSqliteApplicationId,
    #[error("transport pack SQLite user version is wrong")]
    InvalidSqliteUserVersion,
    #[error("source reader failed: {0}")]
    PackRead(#[source] io::Error),
    #[error("integer overflow while summing manifest sizes")]
    TotalsOverflow,
    #[error("cursor is malformed")]
    MalformedCursor,
    #[error("cursor signature is invalid")]
    InvalidCursorSignature,
    #[error("cursor does not match manifest checkpoint")]
    CursorDoesNotMatchManifest,
    #[error("unsupported lineage protocol version {0:?}")]
    UnsupportedLineageProtocol(ProtocolVersion),
    #[error("unsupported lineage capability")]
    UnsupportedLineageCapability,
    #[error("lineage requires a complete immutable base")]
    LineageBaseRequired,
    #[error("lineage digest is required")]
    LineageDigestRequired,
    #[error("lineage base pack does not match its base")]
    LineageBasePackMismatch,
    #[error("lineage delta sequence has a gap or overlap")]
    LineageSequenceGap,
    #[error("lineage parent or pack digest chain is invalid")]
    LineageChainMismatch,
    #[error("lineage delta pack does not match its range")]
    LineageDeltaPackMismatch,
    #[error("lineage head does not match the ordered chain")]
    LineageHeadMismatch,
}

fn checked_add(left: u64, right: u64) -> Result<u64, ProtocolError> {
    left.checked_add(right).ok_or(ProtocolError::TotalsOverflow)
}

fn require_non_nil_uuid(field: &'static str, value: Uuid) -> Result<(), ProtocolError> {
    if value.is_nil() {
        Err(ProtocolError::NilUuid(field))
    } else {
        Ok(())
    }
}

fn validate_sqlite_header(
    header: &[u8; SQLITE_HEADER_BYTES],
    schema: SchemaVersion,
    total_bytes: u64,
) -> Result<(), ProtocolError> {
    if &header[..16] != SQLITE_HEADER_MAGIC
        || !matches!(header[18], 1 | 2)
        || !matches!(header[19], 1 | 2)
        || header[21] != 64
        || header[22] != 32
        || header[23] != 32
        || be_u32(&header[44..48]) != 4
        || !matches!(be_u32(&header[56..60]), 1..=3)
    {
        return Err(ProtocolError::InvalidSqliteHeader);
    }
    let page_size = match be_u16(&header[16..18]) {
        1 => 65_536_u64,
        value if value >= 512 && value.is_power_of_two() => u64::from(value),
        _ => return Err(ProtocolError::InvalidSqliteHeader),
    };
    if total_bytes < page_size || !total_bytes.is_multiple_of(page_size) {
        return Err(ProtocolError::InvalidSqliteHeader);
    }
    if schema
        .sqlite_application_id()
        .is_none_or(|expected| be_u32(&header[68..72]) != expected)
    {
        return Err(ProtocolError::InvalidSqliteApplicationId);
    }
    if be_u32(&header[60..64]) != schema.sqlite_user_version() {
        return Err(ProtocolError::InvalidSqliteUserVersion);
    }
    Ok(())
}

fn be_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn map_source_read_error(error: io::Error) -> ProtocolError {
    if error.kind() == io::ErrorKind::InvalidData {
        ProtocolError::PackTooLarge
    } else {
        ProtocolError::PackRead(error)
    }
}

struct HashingLimitedReader<R> {
    inner: R,
    hasher: Sha256,
    bytes_read: u64,
    max_bytes: u64,
}

impl<R> HashingLimitedReader<R> {
    fn new(inner: R, max_bytes: u64) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes_read: 0,
            max_bytes,
        }
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    fn finish(self) -> Sha256Digest {
        Sha256Digest::from_bytes(self.hasher.finalize().into())
    }
}

impl<R: Read> HashingLimitedReader<R> {
    fn drain_to_eof(&mut self) -> io::Result<()> {
        let mut buffer = [0_u8; 4096];
        while self.read(&mut buffer)? != 0 {}
        Ok(())
    }
}

impl<R: Read> Read for HashingLimitedReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.bytes_read == self.max_bytes {
            let mut probe = [0_u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "compressed pack exceeds declared size",
                )),
            };
        }
        let remaining = usize::try_from(self.max_bytes - self.bytes_read).unwrap_or(usize::MAX);
        let wanted = output.len().min(remaining);
        let read = self.inner.read(&mut output[..wanted])?;
        self.bytes_read += u64::try_from(read).expect("usize fits into u64");
        self.hasher.update(&output[..read]);
        Ok(read)
    }
}

fn encode_cursor_claims(claims: CursorClaims) -> [u8; CURSOR_PAYLOAD_BYTES] {
    let mut bytes = [0_u8; CURSOR_PAYLOAD_BYTES];
    bytes[..4].copy_from_slice(CURSOR_MAGIC);
    bytes[4] = CURSOR_FORMAT_VERSION;
    bytes[5..7].copy_from_slice(&claims.protocol.major.to_be_bytes());
    bytes[7..9].copy_from_slice(&claims.protocol.minor.to_be_bytes());
    bytes[9..11].copy_from_slice(&claims.schema.major.to_be_bytes());
    bytes[11..13].copy_from_slice(&claims.schema.minor.to_be_bytes());
    bytes[13..29].copy_from_slice(claims.installation_id.as_bytes());
    bytes[29..45].copy_from_slice(claims.account_id.as_bytes());
    bytes[45..61].copy_from_slice(claims.vehicle_id.as_bytes());
    bytes[61..69].copy_from_slice(&claims.generation.to_be_bytes());
    bytes[69..77].copy_from_slice(&claims.sequence.to_be_bytes());
    bytes
}

fn decode_cursor_claims(bytes: &[u8; CURSOR_PAYLOAD_BYTES]) -> Result<CursorClaims, ProtocolError> {
    if &bytes[..4] != CURSOR_MAGIC || bytes[4] != CURSOR_FORMAT_VERSION {
        return Err(ProtocolError::MalformedCursor);
    }
    let claims = CursorClaims {
        protocol: ProtocolVersion {
            major: be_u16(&bytes[5..7]),
            minor: be_u16(&bytes[7..9]),
        },
        schema: SchemaVersion {
            major: be_u16(&bytes[9..11]),
            minor: be_u16(&bytes[11..13]),
        },
        installation_id: Uuid::from_slice(&bytes[13..29])
            .map_err(|_| ProtocolError::MalformedCursor)?,
        account_id: Uuid::from_slice(&bytes[29..45]).map_err(|_| ProtocolError::MalformedCursor)?,
        vehicle_id: Uuid::from_slice(&bytes[45..61]).map_err(|_| ProtocolError::MalformedCursor)?,
        generation: u64::from_be_bytes(bytes[61..69].try_into().expect("fixed slice")),
        sequence: u64::from_be_bytes(bytes[69..77].try_into().expect("fixed slice")),
    };
    claims.validate()?;
    Ok(claims)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;
    let mut key_block = [0_u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(key_block.map(|byte| byte ^ 0x36));
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(key_block.map(|byte| byte ^ 0x5c));
    outer.update(inner_digest);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut different = 0_u8;
    for (left, right) in left.iter().zip(right) {
        different |= left ^ right;
    }
    different == 0
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn uuid(value: &str) -> Uuid {
        Uuid::parse_str(value).expect("test UUID")
    }

    fn ids() -> (Uuid, Uuid, Uuid, Uuid) {
        (
            uuid("11111111-1111-4111-8111-111111111111"),
            uuid("22222222-2222-4222-8222-222222222222"),
            uuid("33333333-3333-4333-8333-333333333333"),
            uuid("44444444-4444-4444-8444-444444444444"),
        )
    }

    fn cursor_claims(sequence: u64) -> CursorClaims {
        let (installation_id, account_id, vehicle_id, _) = ids();
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: TRANSPORT_SCHEMA_V1,
            installation_id,
            account_id,
            vehicle_id,
            generation: 7,
            sequence,
        }
    }

    fn cursor(sequence: u64) -> OpaqueCursor {
        OpaqueCursor::issue(&CursorKey::from_bytes([7; 32]), cursor_claims(sequence))
            .expect("cursor")
    }

    fn pack(ordinal: u32, sequence: SequenceRange, tables: Vec<MirrorTable>) -> TransportPack {
        let (_, _, _, snapshot_id) = ids();
        let digest = Sha256Digest::of_bytes(format!("pack-{ordinal}").as_bytes());
        TransportPack {
            pack_id: Uuid::new_v4(),
            snapshot_id,
            ordinal,
            schema: TRANSPORT_SCHEMA_V1,
            format: PackFormat::SqliteTransport,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(digest),
            sha256: digest,
            compressed_bytes: 1_024,
            uncompressed_bytes: 8_192,
            row_count: 100,
            sequence,
            tables,
        }
    }

    fn manifest(mode: TransferMode, chunks: Vec<TransportPack>) -> SyncManifest {
        let (installation_id, account_id, vehicle_id, snapshot_id) = ids();
        let base_sequence = 40;
        let head_sequence = match mode {
            TransferMode::FullSnapshot => 80,
            TransferMode::Incremental => chunks
                .last()
                .map(|chunk| chunk.sequence.to_inclusive)
                .unwrap_or(base_sequence),
        };
        SyncManifest {
            protocol: PROTOCOL_V1,
            schema: TRANSPORT_SCHEMA_V1,
            installation_id,
            account_id,
            vehicle_id,
            generation: 7,
            snapshot_id,
            mode,
            base_sequence,
            head_sequence,
            chunk_count: chunks.len() as u32,
            total_compressed_bytes: chunks.iter().map(|chunk| chunk.compressed_bytes).sum(),
            total_uncompressed_bytes: chunks.iter().map(|chunk| chunk.uncompressed_bytes).sum(),
            total_rows: chunks.iter().map(|chunk| chunk.row_count).sum(),
            chunks,
            terminal_cursor: cursor(head_sequence),
        }
    }

    #[test]
    fn valid_snapshot_is_serializable_and_validated() {
        let range = SequenceRange {
            from_exclusive: 40,
            to_inclusive: 80,
        };
        let value = manifest(
            TransferMode::FullSnapshot,
            vec![
                pack(0, range, vec![MirrorTable::Vehicle]),
                pack(
                    1,
                    range,
                    vec![MirrorTable::Drive, MirrorTable::ChargingProcess],
                ),
                pack(2, range, vec![MirrorTable::Position, MirrorTable::Charge]),
            ],
        );

        value.validate().expect("valid snapshot");
        value
            .validate_terminal_cursor(&CursorKey::from_bytes([7; 32]))
            .expect("terminal cursor");
        let json = serde_json::to_string(&value).expect("serialize manifest");
        let decoded: SyncManifest = serde_json::from_str(&json).expect("deserialize manifest");
        assert_eq!(decoded, value);
    }

    #[test]
    fn rejects_unknown_versions_before_pack_work() {
        let mut value = manifest(TransferMode::FullSnapshot, vec![]);
        value.protocol.minor = 1;
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::UnsupportedProtocol(_))
        ));
        value.protocol = PROTOCOL_V1;
        value.schema.major = 99;
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::UnsupportedSchema(_))
        ));
    }

    #[test]
    fn rejects_manifest_count_order_totals_and_unsafe_paths() {
        let range = SequenceRange {
            from_exclusive: 40,
            to_inclusive: 80,
        };
        let mut value = manifest(
            TransferMode::FullSnapshot,
            vec![pack(0, range, vec![MirrorTable::Vehicle])],
        );
        value.chunk_count = 2;
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::InvalidChunkCount { .. })
        ));

        value.chunk_count = 1;
        value.chunks[0].ordinal = 3;
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::NonContiguousChunkOrder)
        ));

        value.chunks[0].ordinal = 0;
        value.total_rows += 1;
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::ManifestTotalsMismatch)
        ));

        value.total_rows -= 1;
        value.chunks[0].relative_path = "/v1/packs/sha256/not-the-digest.sqlite.zst".into();
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::NonCanonicalPackPath)
        ));
    }

    #[test]
    fn rejects_snapshot_dependency_regression() {
        let range = SequenceRange {
            from_exclusive: 40,
            to_inclusive: 80,
        };
        let value = manifest(
            TransferMode::FullSnapshot,
            vec![
                pack(0, range, vec![MirrorTable::Position]),
                pack(1, range, vec![MirrorTable::Vehicle]),
            ],
        );
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::SnapshotDependencyOrder)
        ));
    }

    #[test]
    fn validates_incremental_contiguous_sequence_only() {
        let value = manifest(
            TransferMode::Incremental,
            vec![
                pack(
                    0,
                    SequenceRange {
                        from_exclusive: 40,
                        to_inclusive: 56,
                    },
                    vec![MirrorTable::Position],
                ),
                pack(
                    1,
                    SequenceRange {
                        from_exclusive: 56,
                        to_inclusive: 63,
                    },
                    vec![MirrorTable::Tombstone],
                ),
            ],
        );
        value.validate().expect("contiguous delta");

        let mut gap = value;
        gap.chunks[1].sequence.from_exclusive = 57;
        assert!(matches!(
            gap.validate(),
            Err(ProtocolError::DeltaSequenceGap)
        ));
    }

    #[test]
    fn rejects_pack_sizes_outside_limits() {
        let mut value = pack(
            0,
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 0,
            },
            vec![MirrorTable::Vehicle],
        );
        value.uncompressed_bytes = 256 * 1024 * 1024 + 1;
        assert!(matches!(
            value.validate(ProtocolLimits::default()),
            Err(ProtocolError::UncompressedSizeOutOfBounds(_))
        ));
        value.uncompressed_bytes = 65_537;
        value.compressed_bytes = 1;
        assert!(matches!(
            value.validate(ProtocolLimits::default()),
            Err(ProtocolError::ExpansionRatioExceeded)
        ));
    }

    fn sqlite_transport_file(schema: SchemaVersion) -> Vec<u8> {
        let mut bytes = vec![0_u8; 4_096];
        for (index, byte) in bytes[SQLITE_HEADER_BYTES..].iter_mut().enumerate() {
            // Keep the test object realistic: a normal pack must not only
            // pass because a zero-filled page compresses like a zip bomb.
            *byte = ((index * 73 + 19) % 251) as u8;
        }
        bytes[..16].copy_from_slice(SQLITE_HEADER_MAGIC);
        bytes[16..18].copy_from_slice(&4_096_u16.to_be_bytes());
        bytes[18] = 2;
        bytes[19] = 2;
        bytes[21] = 64;
        bytes[22] = 32;
        bytes[23] = 32;
        bytes[44..48].copy_from_slice(&4_u32.to_be_bytes());
        bytes[56..60].copy_from_slice(&1_u32.to_be_bytes());
        bytes[60..64].copy_from_slice(&schema.sqlite_user_version().to_be_bytes());
        bytes[68..72].copy_from_slice(&SQLITE_TRANSPORT_APPLICATION_ID.to_be_bytes());
        bytes
    }

    fn verified_pack() -> (TransportPack, Vec<u8>) {
        let uncompressed = sqlite_transport_file(TRANSPORT_SCHEMA_V1);
        let compressed = zstd::stream::encode_all(Cursor::new(&uncompressed), 1).expect("zstd");
        let digest = Sha256Digest::of_bytes(&compressed);
        let (_, _, _, snapshot_id) = ids();
        (
            TransportPack {
                pack_id: Uuid::new_v4(),
                snapshot_id,
                ordinal: 0,
                schema: TRANSPORT_SCHEMA_V1,
                format: PackFormat::SqliteTransport,
                compression: PackCompression::Zstd,
                relative_path: TransportPack::canonical_relative_path(digest),
                sha256: digest,
                compressed_bytes: compressed.len() as u64,
                uncompressed_bytes: uncompressed.len() as u64,
                row_count: 1,
                sequence: SequenceRange {
                    from_exclusive: 0,
                    to_inclusive: 0,
                },
                tables: vec![MirrorTable::Vehicle],
            },
            compressed,
        )
    }

    #[test]
    fn verifies_bounded_zstd_sqlite_transport_pack() {
        let (pack, bytes) = verified_pack();
        let verified = pack
            .verify_reader(Cursor::new(bytes), ProtocolLimits::default())
            .expect("verified pack");
        assert_eq!(verified.pack_id, pack.pack_id);
        assert_eq!(verified.uncompressed_bytes, 4_096);
        assert_eq!(pack.etag(), format!("\"{}\"", pack.sha256));
    }

    #[test]
    fn rejects_wrong_hash_extra_bytes_and_wrong_sqlite_identity() {
        let (pack, mut bytes) = verified_pack();
        bytes[0] ^= 1;
        assert!(matches!(
            pack.verify_reader(Cursor::new(bytes), ProtocolLimits::default()),
            Err(ProtocolError::PackDecompression) | Err(ProtocolError::PackHashMismatch)
        ));

        let (pack, mut bytes) = verified_pack();
        bytes.push(1);
        let error = pack
            .verify_reader(Cursor::new(bytes), ProtocolLimits::default())
            .expect_err("trailing bytes must be rejected");
        assert!(
            matches!(
                error,
                ProtocolError::PackTooLarge | ProtocolError::PackDecompression
            ),
            "{error:?}"
        );

        let (mut pack, _) = verified_pack();
        let mut uncompressed = sqlite_transport_file(TRANSPORT_SCHEMA_V1);
        uncompressed[68..72].copy_from_slice(&0_u32.to_be_bytes());
        let bytes = zstd::stream::encode_all(Cursor::new(&uncompressed), 1).expect("zstd");
        pack.sha256 = Sha256Digest::of_bytes(&bytes);
        pack.relative_path = TransportPack::canonical_relative_path(pack.sha256);
        pack.compressed_bytes = bytes.len() as u64;
        assert!(matches!(
            pack.verify_reader(Cursor::new(bytes), ProtocolLimits::default()),
            Err(ProtocolError::InvalidSqliteApplicationId)
        ));

        let (mut pack, bytes) = verified_pack();
        pack.sha256 = Sha256Digest::of_bytes(b"another object");
        pack.relative_path = TransportPack::canonical_relative_path(pack.sha256);
        assert!(matches!(
            pack.verify_reader(Cursor::new(bytes), ProtocolLimits::default()),
            Err(ProtocolError::PackHashMismatch)
        ));
    }

    #[test]
    fn cursor_is_signed_opaque_and_bound_to_the_manifest() {
        let key = CursorKey::from_bytes([7; 32]);
        let token = OpaqueCursor::issue(&key, cursor_claims(80)).expect("issue");
        assert_eq!(token.verify(&key).expect("verify"), cursor_claims(80));
        assert!(!format!("{token:?}").contains("tsp1"));

        let range = SequenceRange {
            from_exclusive: 40,
            to_inclusive: 80,
        };
        let value = manifest(
            TransferMode::FullSnapshot,
            vec![pack(0, range, vec![MirrorTable::Vehicle])],
        );
        value.validate_terminal_cursor(&key).expect("bound cursor");

        let mut tampered = token.as_str().to_owned();
        let replacement = if tampered.ends_with('0') { "1" } else { "0" };
        tampered.replace_range(tampered.len() - 1.., replacement);
        let tampered: OpaqueCursor = serde_json::from_value(serde_json::Value::String(tampered))
            .expect("shape remains valid");
        assert!(matches!(
            tampered.verify(&key),
            Err(ProtocolError::InvalidCursorSignature)
        ));
    }

    fn lineage_manifest() -> LineageManifestV2 {
        let (_, account_id, vehicle_id, snapshot_id) = ids();
        let base_pack = pack(
            0,
            SequenceRange {
                from_exclusive: 40,
                to_inclusive: 40,
            },
            vec![MirrorTable::Vehicle],
        );
        let delta_pack = pack(
            0,
            SequenceRange {
                from_exclusive: 40,
                to_inclusive: 41,
            },
            vec![MirrorTable::Position],
        );
        let base_digest = Sha256Digest::of_bytes(b"base");
        let chain_digest = Sha256Digest::of_bytes(b"delta-chain");
        LineageManifestV2 {
            protocol: LINEAGE_PROTOCOL_V2,
            capability: LineageCapability::ImmutableBaseOrderedDeltas,
            schema: TRANSPORT_SCHEMA_V1,
            installation_id: ids().0,
            account_id,
            vehicle_id,
            generation: 7,
            base: LineageBase {
                snapshot_id,
                sequence: 40,
                digest: base_digest,
                packs: vec![base_pack],
            },
            deltas: vec![LineageDelta {
                from_sequence: 40,
                to_sequence: 41,
                parent_chain_digest: base_digest,
                chain_digest,
                pack_digest: delta_pack.sha256,
                pack: delta_pack,
            }],
            head_sequence: 41,
            head_digest: chain_digest,
            terminal_cursor: cursor(41),
        }
    }

    #[test]
    fn lineage_requires_full_base_and_contiguous_digest_chain() {
        let mut value = lineage_manifest();
        value.validate().expect("valid lineage");

        value.base.packs.clear();
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::LineageBaseRequired)
        ));

        let mut value = lineage_manifest();
        value.deltas[0].from_sequence = 39;
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::LineageSequenceGap)
        ));

        let mut value = lineage_manifest();
        value.deltas[0].parent_chain_digest = Sha256Digest::of_bytes(b"wrong-parent");
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::LineageChainMismatch)
        ));
    }

    #[test]
    fn lineage_rejects_overlapping_pack_ranges_and_wrong_head() {
        let mut value = lineage_manifest();
        value.deltas[0].pack.sequence.to_inclusive = 42;
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::LineageDeltaPackMismatch)
        ));

        let mut value = lineage_manifest();
        value.head_digest = Sha256Digest::of_bytes(b"wrong-head");
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::LineageHeadMismatch)
        ));
    }

    #[test]
    fn v2_golden_manifest_round_trips_with_client_wire_shape() {
        let bytes = include_bytes!("../fixtures/lineage_manifest_v2.json");
        let manifest: LineageManifestV2 =
            serde_json::from_slice(bytes).expect("golden v2 manifest parses");
        manifest.validate().expect("golden v2 manifest validates");
        let expected: serde_json::Value =
            serde_json::from_slice(bytes).expect("golden JSON value");
        assert_eq!(serde_json::to_value(&manifest).expect("serialize"), expected);
    }
}
