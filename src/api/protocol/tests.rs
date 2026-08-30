// SPDX-License-Identifier: AGPL-3.0-only

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

fn cursor_claims_for_schema(schema: SchemaVersion, sequence: u64) -> CursorClaims {
    let (installation_id, account_id, vehicle_id, _) = ids();
    CursorClaims {
        protocol: PROTOCOL_V1,
        schema,
        installation_id,
        account_id,
        vehicle_id,
        generation: 7,
        sequence,
    }
}

fn cursor_claims(sequence: u64) -> CursorClaims {
    cursor_claims_for_schema(TRANSPORT_SCHEMA_V1, sequence)
}

fn cursor_for_schema(schema: SchemaVersion, sequence: u64) -> OpaqueCursor {
    OpaqueCursor::issue(
        &CursorKey::from_bytes([7; 32]),
        cursor_claims_for_schema(schema, sequence),
    )
    .expect("cursor")
}

fn cursor(sequence: u64) -> OpaqueCursor {
    cursor_for_schema(TRANSPORT_SCHEMA_V1, sequence)
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

fn hub_projection_pack(
    schema: SchemaVersion,
    ordinal: u32,
    sequence: SequenceRange,
    tables: Vec<MirrorTable>,
) -> TransportPack {
    let mut value = pack(ordinal, sequence, tables);
    value.schema = schema;
    value.format = PackFormat::HubProjectionSqlite;
    value
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

fn hub_projection_manifest(
    schema: SchemaVersion,
    mode: TransferMode,
    chunks: Vec<TransportPack>,
) -> SyncManifest {
    let mut value = manifest(mode, chunks);
    value.schema = schema;
    value.terminal_cursor = cursor_for_schema(schema, value.head_sequence);
    value
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
fn schema_22_is_a_recognized_full_snapshot_identity() {
    assert_eq!(
        HUB_PROJECTION_SCHEMA_V2.support(),
        Some(SchemaSupport::TypedHubProjection)
    );
    assert_eq!(
        HUB_PROJECTION_SCHEMA_V3.support(),
        Some(SchemaSupport::FullSnapshotOnlyHubProjection)
    );
    assert!(HUB_PROJECTION_SCHEMA_V2.supports_incremental());
    assert!(!HUB_PROJECTION_SCHEMA_V3.supports_incremental());
    let unreviewed_successor = SchemaVersion { major: 2, minor: 3 };
    assert_eq!(unreviewed_successor.support(), None);
    assert!(!unreviewed_successor.is_supported());

    let range = SequenceRange {
        from_exclusive: 40,
        to_inclusive: 80,
    };
    let value = hub_projection_manifest(
        HUB_PROJECTION_SCHEMA_V3,
        TransferMode::FullSnapshot,
        vec![hub_projection_pack(
            HUB_PROJECTION_SCHEMA_V3,
            0,
            range,
            vec![MirrorTable::Car],
        )],
    );

    value.validate().expect("2.2 full snapshot");
    value
        .validate_terminal_cursor(&CursorKey::from_bytes([7; 32]))
        .expect("2.2 full snapshot cursor");
}

#[test]
fn schema_22_rejects_incremental_manifest_even_when_its_packs_match() {
    let value = hub_projection_manifest(
        HUB_PROJECTION_SCHEMA_V3,
        TransferMode::Incremental,
        vec![hub_projection_pack(
            HUB_PROJECTION_SCHEMA_V3,
            0,
            SequenceRange {
                from_exclusive: 40,
                to_inclusive: 41,
            },
            vec![MirrorTable::Position],
        )],
    );

    assert!(matches!(
        value.validate(),
        Err(ProtocolError::FullSnapshotOnlySchemaInIncrementalManifest(
            HUB_PROJECTION_SCHEMA_V3
        ))
    ));
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
fn accepts_zero_row_no_op_manifests_but_requires_exact_row_totals() {
    let no_op = manifest(TransferMode::Incremental, vec![]);
    no_op.validate().expect("zero-row no-op");

    let range = SequenceRange {
        from_exclusive: 40,
        to_inclusive: 80,
    };
    let mut value = manifest(
        TransferMode::FullSnapshot,
        vec![pack(0, range, vec![MirrorTable::Vehicle])],
    );
    value.total_rows = 0;
    assert!(matches!(
        value.validate(),
        Err(ProtocolError::ManifestTotalsMismatch)
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
    let tampered: OpaqueCursor =
        serde_json::from_value(serde_json::Value::String(tampered)).expect("shape remains valid");
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
    let chain_digest = canonical_delta_chain_digest(base_digest, delta_pack.sha256);
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

fn typed_projection_lineage_manifest() -> LineageManifestV2 {
    let mut value = lineage_manifest();
    value.schema = HUB_PROJECTION_SCHEMA_V2;
    value.base.packs[0].schema = HUB_PROJECTION_SCHEMA_V2;
    value.base.packs[0].format = PackFormat::HubProjectionSqlite;
    value.base.packs[0].tables = vec![MirrorTable::Car];
    value.deltas[0].pack.schema = HUB_PROJECTION_SCHEMA_V2;
    value.deltas[0].pack.format = PackFormat::HubProjectionSqlite;
    value.terminal_cursor = cursor_for_schema(HUB_PROJECTION_SCHEMA_V2, value.head_sequence);
    value
}

#[test]
fn schema_22_is_rejected_from_the_generic_v2_lineage_envelope() {
    let mut value = typed_projection_lineage_manifest();
    value.schema = HUB_PROJECTION_SCHEMA_V3;
    value.base.packs[0].schema = HUB_PROJECTION_SCHEMA_V3;
    value.deltas[0].pack.schema = HUB_PROJECTION_SCHEMA_V3;
    value.terminal_cursor = cursor_for_schema(HUB_PROJECTION_SCHEMA_V3, value.head_sequence);

    assert!(matches!(
        value.validate(),
        Err(ProtocolError::FullSnapshotOnlySchemaInLineageV2(
            HUB_PROJECTION_SCHEMA_V3
        ))
    ));
}

#[test]
fn typed_21_lineage_rejects_a_schema_22_delta() {
    let mut value = typed_projection_lineage_manifest();
    value.validate().expect("typed 2.1 lineage");
    value.deltas[0].pack.schema = HUB_PROJECTION_SCHEMA_V3;

    assert!(matches!(
        value.validate(),
        Err(ProtocolError::LineageDeltaPackMismatch)
    ));
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

    let mut value = lineage_manifest();
    value.deltas[0].chain_digest = Sha256Digest::of_bytes(b"non-canonical-chain");
    value.head_digest = value.deltas[0].chain_digest;
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
fn lineage_binds_all_packs_to_its_schema_snapshot_and_unique_identity() {
    let mut value = lineage_manifest();
    value.base.packs[0].schema = HUB_PROJECTION_SCHEMA_V1;
    assert!(matches!(
        value.validate(),
        Err(ProtocolError::LineageBasePackMismatch)
    ));

    let mut value = lineage_manifest();
    value.base.packs[0].snapshot_id = Uuid::new_v4();
    assert!(matches!(
        value.validate(),
        Err(ProtocolError::LineageBasePackMismatch)
    ));

    let mut value = lineage_manifest();
    value.deltas[0].pack.schema = HUB_PROJECTION_SCHEMA_V1;
    assert!(matches!(
        value.validate(),
        Err(ProtocolError::LineageDeltaPackMismatch)
    ));

    let mut value = lineage_manifest();
    value.deltas[0].pack.snapshot_id = Uuid::new_v4();
    assert!(matches!(
        value.validate(),
        Err(ProtocolError::LineageDeltaPackMismatch)
    ));

    let mut value = lineage_manifest();
    value.deltas[0].pack.pack_id = value.base.packs[0].pack_id;
    assert!(matches!(
        value.validate(),
        Err(ProtocolError::DuplicatePackId)
    ));
}

#[test]
fn lineage_fails_closed_for_aggregate_limits_and_limit_overflow() {
    let value = lineage_manifest();
    let single_pack_limit = ProtocolLimits {
        max_chunks: 1,
        ..ProtocolLimits::default()
    };
    assert!(matches!(
        value.validate_with_limits(single_pack_limit),
        Err(ProtocolError::LineageAggregateLimitExceeded)
    ));

    let overflowing_byte_limit = ProtocolLimits {
        max_chunks: 2,
        max_compressed_pack_bytes: u64::MAX,
        ..ProtocolLimits::default()
    };
    assert!(matches!(
        value.validate_with_limits(overflowing_byte_limit),
        Err(ProtocolError::LineageAggregateLimitExceeded)
    ));
}

#[test]
fn v2_golden_manifest_round_trips_with_client_wire_shape() {
    let bytes = include_bytes!("../../../fixtures/lineage_manifest_v2.json");
    let manifest: LineageManifestV2 =
        serde_json::from_slice(bytes).expect("golden v2 manifest parses");
    manifest.validate().expect("golden v2 manifest validates");
    let expected: serde_json::Value = serde_json::from_slice(bytes).expect("golden JSON value");
    assert_eq!(
        serde_json::to_value(&manifest).expect("serialize"),
        expected
    );
}
