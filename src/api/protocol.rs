// SPDX-License-Identifier: AGPL-3.0-only

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

/// Full-fidelity Hub projection identity.  Schema 2.2 is deliberately
/// complete-snapshot-only until its pack writer, cataloguing, and receiver
/// migrations are all available.  It must never be smuggled through the
/// existing typed delta/lineage protocol.
pub const HUB_PROJECTION_SCHEMA_V3: SchemaVersion = SchemaVersion { major: 2, minor: 2 };

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
const FLEET_CREDENTIAL_ENCRYPTION_DOMAIN: &[u8] = b"teslatlas-hub/fleet-credential-encryption/v1";
const PUBLIC_QUERY_CURSOR_DOMAIN: &[u8] = b"teslatlas-hub/public-query-cursor/v1\0";

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

/// The protocol-level delivery contract for a recognized SQLite schema.
///
/// Keep this separate from the numeric version.  A recognized schema is not
/// automatically safe in every existing transport envelope: 2.1 is the
/// typed projection accepted by full and delta routes, whereas 2.2 is a new
/// full-fidelity identity that is full-snapshot-only during its staged rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemaSupport {
    GenericTransport,
    TypedHubProjection,
    FullSnapshotOnlyHubProjection,
}

impl SchemaVersion {
    /// Returns the exact protocol support contract for this schema.  Unknown
    /// versions intentionally have no contract and must fail closed.
    pub const fn support(self) -> Option<SchemaSupport> {
        if self.major == TRANSPORT_SCHEMA_V1.major && self.minor == TRANSPORT_SCHEMA_V1.minor {
            Some(SchemaSupport::GenericTransport)
        } else if self.major == HUB_PROJECTION_SCHEMA_V1.major
            && (self.minor == HUB_PROJECTION_SCHEMA_V1.minor
                || self.minor == HUB_PROJECTION_SCHEMA_V2.minor)
        {
            Some(SchemaSupport::TypedHubProjection)
        } else if self.major == HUB_PROJECTION_SCHEMA_V3.major
            && self.minor == HUB_PROJECTION_SCHEMA_V3.minor
        {
            Some(SchemaSupport::FullSnapshotOnlyHubProjection)
        } else {
            None
        }
    }

    pub const fn is_supported(self) -> bool {
        self.support().is_some()
    }

    pub const fn supports_incremental(self) -> bool {
        matches!(
            self.support(),
            Some(SchemaSupport::GenericTransport | SchemaSupport::TypedHubProjection)
        )
    }

    pub const fn supports_lineage_v2(self) -> bool {
        self.supports_incremental()
    }

    const fn is_hub_projection(self) -> bool {
        matches!(
            self.support(),
            Some(SchemaSupport::TypedHubProjection | SchemaSupport::FullSnapshotOnlyHubProjection)
        )
    }

    /// Stored in the SQLite `user_version` field of every transport pack.
    pub const fn sqlite_user_version(self) -> u32 {
        ((self.major as u32) << 16) | self.minor as u32
    }

    const fn sqlite_application_id(self) -> Option<u32> {
        match self.support() {
            Some(SchemaSupport::GenericTransport) => Some(SQLITE_TRANSPORT_APPLICATION_ID),
            Some(
                SchemaSupport::TypedHubProjection | SchemaSupport::FullSnapshotOnlyHubProjection,
            ) => Some(SQLITE_HUB_PROJECTION_APPLICATION_ID),
            None => None,
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
            PackFormat::HubProjectionSqlite if !self.schema.is_hub_projection() => {
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

/// Canonical V2 delta-chain commitment.  The parent is the immutable base
/// digest for the first delta and the prior delta commitment thereafter.
pub fn canonical_delta_chain_digest(
    parent_chain_digest: Sha256Digest,
    pack_digest: Sha256Digest,
) -> Sha256Digest {
    Sha256Digest::of_bytes(format!("delta-v2/{parent_chain_digest}:{pack_digest}").as_bytes())
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
        self.validate_with_limits(ProtocolLimits::default())
    }

    /// Validates the lineage envelope against the receiver's release bounds.
    ///
    /// Lineage has no aggregate fields on the wire, so the aggregate ceiling
    /// is derived from the same per-pack limits and maximum pack count used by
    /// [`SyncManifest`]. This keeps an authenticated but corrupt lineage from
    /// consuming unbounded validation or staging resources.
    pub fn validate_with_limits(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        if self.protocol != LINEAGE_PROTOCOL_V2 {
            return Err(ProtocolError::UnsupportedLineageProtocol(self.protocol));
        }
        if self.capability != LineageCapability::ImmutableBaseOrderedDeltas {
            return Err(ProtocolError::UnsupportedLineageCapability);
        }
        if !self.schema.is_supported() {
            return Err(ProtocolError::UnsupportedSchema(self.schema));
        }
        if !self.schema.supports_lineage_v2() {
            return Err(ProtocolError::FullSnapshotOnlySchemaInLineageV2(
                self.schema,
            ));
        }
        require_non_nil_uuid("installation_id", self.installation_id)?;
        require_non_nil_uuid("account_id", self.account_id)?;
        require_non_nil_uuid("vehicle_id", self.vehicle_id)?;
        require_non_nil_uuid("base snapshot_id", self.base.snapshot_id)?;
        if self.generation == 0 || self.base.packs.is_empty() {
            return Err(ProtocolError::LineageBaseRequired);
        }
        let total_pack_count = self
            .base
            .packs
            .len()
            .checked_add(self.deltas.len())
            .ok_or(ProtocolError::LineageAggregateLimitExceeded)?;
        if total_pack_count > limits.max_chunks {
            return Err(ProtocolError::LineageAggregateLimitExceeded);
        }
        let maximum_lineage_compressed_bytes =
            lineage_aggregate_limit(limits.max_compressed_pack_bytes, limits.max_chunks)?;
        let maximum_lineage_uncompressed_bytes =
            lineage_aggregate_limit(limits.max_uncompressed_pack_bytes, limits.max_chunks)?;
        let maximum_lineage_rows =
            lineage_aggregate_limit(limits.max_rows_per_pack, limits.max_chunks)?;
        if self.base.digest.is_zero() {
            return Err(ProtocolError::LineageDigestRequired);
        }
        let mut seen_packs = HashSet::with_capacity(total_pack_count);
        let mut compressed_total = 0_u64;
        let mut uncompressed_total = 0_u64;
        let mut row_total = 0_u64;
        for (ordinal, pack) in self.base.packs.iter().enumerate() {
            if pack.schema != self.schema || pack.snapshot_id != self.base.snapshot_id {
                return Err(ProtocolError::LineageBasePackMismatch);
            }
            if !seen_packs.insert(pack.pack_id) {
                return Err(ProtocolError::DuplicatePackId);
            }
            pack.validate(limits)?;
            if pack.ordinal != ordinal as u32
                || pack.sequence
                    != (SequenceRange {
                        from_exclusive: self.base.sequence,
                        to_inclusive: self.base.sequence,
                    })
            {
                return Err(ProtocolError::LineageBasePackMismatch);
            }
            compressed_total = checked_lineage_total(
                compressed_total,
                pack.compressed_bytes,
                maximum_lineage_compressed_bytes,
            )?;
            uncompressed_total = checked_lineage_total(
                uncompressed_total,
                pack.uncompressed_bytes,
                maximum_lineage_uncompressed_bytes,
            )?;
            row_total = checked_lineage_total(row_total, pack.row_count, maximum_lineage_rows)?;
        }
        let mut expected_from = self.base.sequence;
        let mut parent = self.base.digest;
        for delta in &self.deltas {
            if delta.from_sequence != expected_from || delta.to_sequence <= delta.from_sequence {
                return Err(ProtocolError::LineageSequenceGap);
            }
            if delta.parent_chain_digest != parent
                || delta.pack_digest != delta.pack.sha256
                || delta.chain_digest
                    != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack_digest)
            {
                return Err(ProtocolError::LineageChainMismatch);
            }
            if delta.pack.schema != self.schema
                || delta.pack.snapshot_id != self.base.snapshot_id
                || delta.pack.sequence
                    != (SequenceRange {
                        from_exclusive: delta.from_sequence,
                        to_inclusive: delta.to_sequence,
                    })
            {
                return Err(ProtocolError::LineageDeltaPackMismatch);
            }
            if !seen_packs.insert(delta.pack.pack_id) {
                return Err(ProtocolError::DuplicatePackId);
            }
            delta.pack.validate(limits)?;
            compressed_total = checked_lineage_total(
                compressed_total,
                delta.pack.compressed_bytes,
                maximum_lineage_compressed_bytes,
            )?;
            uncompressed_total = checked_lineage_total(
                uncompressed_total,
                delta.pack.uncompressed_bytes,
                maximum_lineage_uncompressed_bytes,
            )?;
            row_total =
                checked_lineage_total(row_total, delta.pack.row_count, maximum_lineage_rows)?;
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
        if matches!(self.mode, TransferMode::Incremental) && !self.schema.supports_incremental() {
            return Err(ProtocolError::FullSnapshotOnlySchemaInIncrementalManifest(
                self.schema,
            ));
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
            || self.total_rows != row_total
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

/// Signing material for cursor integrity. Keep it in a protected local secret
/// file or equivalent secret store; never in Hub configuration or logs.
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

    pub(crate) fn fleet_credential_encryption_key(&self) -> [u8; 32] {
        hmac_sha256(&self.0, FLEET_CREDENTIAL_ENCRYPTION_DOMAIN)
    }

    pub(crate) fn public_query_cursor_tag(&self, payload: &[u8]) -> [u8; 32] {
        let mut message = Vec::with_capacity(PUBLIC_QUERY_CURSOR_DOMAIN.len() + payload.len());
        message.extend_from_slice(PUBLIC_QUERY_CURSOR_DOMAIN);
        message.extend_from_slice(payload);
        hmac_sha256(&self.0, &message)
    }

    pub(crate) fn verifies_public_query_cursor_tag(&self, payload: &[u8], tag: &[u8; 32]) -> bool {
        constant_time_eq(&self.public_query_cursor_tag(payload), tag)
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
    #[error("schema {0:?} is full-snapshot-only and cannot use a v2 lineage envelope")]
    FullSnapshotOnlySchemaInLineageV2(SchemaVersion),
    #[error("schema {0:?} is full-snapshot-only and cannot use an incremental manifest")]
    FullSnapshotOnlySchemaInIncrementalManifest(SchemaVersion),
    #[error("lineage requires a complete immutable base")]
    LineageBaseRequired,
    #[error("lineage exceeds aggregate protocol limits")]
    LineageAggregateLimitExceeded,
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

fn lineage_aggregate_limit(per_pack: u64, max_packs: usize) -> Result<u64, ProtocolError> {
    per_pack
        .checked_mul(
            u64::try_from(max_packs).map_err(|_| ProtocolError::LineageAggregateLimitExceeded)?,
        )
        .ok_or(ProtocolError::LineageAggregateLimitExceeded)
}

fn checked_lineage_total(current: u64, added: u64, limit: u64) -> Result<u64, ProtocolError> {
    let total = current
        .checked_add(added)
        .ok_or(ProtocolError::LineageAggregateLimitExceeded)?;
    if total > limit {
        return Err(ProtocolError::LineageAggregateLimitExceeded);
    }
    Ok(total)
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
#[path = "protocol/tests.rs"]
mod tests;
