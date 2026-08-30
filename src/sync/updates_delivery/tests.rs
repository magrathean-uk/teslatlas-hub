// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{
    db::{SourceDescriptor, VehicleDescriptor},
    teslamate_projection::{
        TeslaMateCarPhysicalV2_2, TeslaMateCarSettingsPhysicalV2_2, TeslaMateSettingsPhysicalV2_2,
    },
    teslamate_reader::TeslaMateSchemaInfo,
    teslamate_schema::{MAX_VALIDATED_MIGRATION, TESLAMATE_V4_MIGRATION_SET_SHA256},
    updates_logical::decode_updates_logical_stream,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn production_source(car_id: i16) -> DirectUpdatesSourceV2_2 {
    DirectUpdatesSourceV2_2 {
        postgres_snapshot_sha256: hex_sha256(Uuid::new_v4().as_bytes()),
        schema: TeslaMateSchemaInfo {
            observed_migration_version: MAX_VALIDATED_MIGRATION,
            observed_migration_count: TESLAMATE_V4_MIGRATION_COUNT,
            minimum_supported_migration_version: MAX_VALIDATED_MIGRATION,
            maximum_validated_migration_version: MAX_VALIDATED_MIGRATION,
            pinned_source_revision: TESLAMATE_V4_SOURCE_REVISION,
            pinned_migration_set_sha256: TESLAMATE_V4_MIGRATION_SET_SHA256,
            fingerprint: hex_sha256(b"production-repeatable-read-schema-fingerprint"),
        },
        global_settings: TeslaMateSettingsPhysicalV2_2 {
            id: 1,
            unit_of_length: ProjectionUnitOfLengthV2_2::Kilometers,
            unit_of_temperature: ProjectionUnitOfTemperatureV2_2::Celsius,
            unit_of_pressure: ProjectionUnitOfPressureV2_2::Bar,
            preferred_range: ProjectionPreferredRangeV2_2::Rated,
            base_url: None,
            grafana_url: None,
            language: "en".into(),
            theme_mode: "system".into(),
            inserted_at_pg_us: 0,
            updated_at_pg_us: 0,
        },
        car: TeslaMateCarPhysicalV2_2 {
            id: car_id,
            eid: 100,
            vid: 200,
            vin: Some("TESTVIN".into()),
            name: Some("Selected".into()),
            model: None,
            efficiency: None,
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            display_priority: 0,
            inserted_at_pg_us: 0,
            updated_at_pg_us: 0,
            settings_id: 3,
        },
        car_settings: TeslaMateCarSettingsPhysicalV2_2 {
            id: 3,
            suspend_min: 21,
            suspend_after_idle_min: 15,
            req_not_unlocked: false,
            free_supercharging: false,
            use_streaming_api: true,
            enabled: true,
            lfp_battery: false,
        },
        updates: vec![
            TeslaMateUpdatePhysicalV2_2 {
                id: -4,
                car_id,
                start_date_pg_us: -1,
                end_date_pg_us: Some(0),
                version: None,
            },
            TeslaMateUpdatePhysicalV2_2 {
                id: 9,
                car_id,
                start_date_pg_us: 1,
                end_date_pg_us: None,
                version: Some("  βeta 🚗  ".into()),
            },
        ],
    }
}

#[test]
fn production_capture_publishes_dynamic_exact_pair_and_reuses_exact_bytes() {
    let temp = crate::private_tempdir().expect("store root");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([43; 32]);
    let registered_source = store
        .register_source(
            &SourceDescriptor::new("teslamate", format!("test-source-{}", Uuid::new_v4())),
            1_000,
        )
        .expect("source");
    let registered_vehicle = store
        .register_vehicle(
            &VehicleDescriptor {
                source_id: registered_source.source_id,
                source_vehicle_key: format!("test-car-{}", Uuid::new_v4()),
                vin: Some("TESTVIN".into()),
                display_name: Some("Selected".into()),
                tesla_eid: Some(100),
                tesla_vid: Some(200),
            },
            1_000,
        )
        .expect("vehicle");
    let binding = ProjectionBinding {
        installation_id: store.installation_id().expect("installation"),
        account_id: registered_source.source_id,
        vehicle_id: registered_vehicle.vehicle_id,
        generation: registered_source.generation,
        selected_car_id: 7,
    };
    let mut source = production_source(7);
    source.updates.reverse();

    let first = publish_production_updates_schema_22(&store, &cursor_key, &binding, source.clone())
        .expect("publish production pair");
    assert!(!first.reused_current_snapshot);
    assert_eq!(first.source_logical_sha256, first.hub_logical_sha256);
    assert_eq!(first.source_summary, first.hub_summary);
    assert_eq!(first.source_summary.row_count, 2);
    assert_eq!(first.source_summary.open_row_count, 1);
    assert_eq!(first.source_summary.null_version_row_count, 1);
    assert_eq!(
        first.source_witness.postgres_snapshot_sha256,
        source.postgres_snapshot_sha256
    );
    assert_eq!(
        first.source_witness.source_logical_sha256,
        first.source_witness.hub_logical_sha256
    );
    assert_eq!(first.source_witness.head_sequence, first.sequence);
    let (manifest_bytes, noop_bytes) =
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key).expect("signed pair");
    assert_eq!(hex_sha256(&manifest_bytes), first.manifest_sha256);
    assert_eq!(hex_sha256(&noop_bytes), first.noop_sha256);
    let noop_json: serde_json::Value = serde_json::from_slice(&noop_bytes).expect("no-op JSON");
    let source_witness_json = noop_json
        .get("sourceWitness")
        .expect("camelCase production source witness");
    assert_eq!(source_witness_json["sourceRowCount"], 2);
    assert_eq!(source_witness_json["hubRowCount"], 2);
    assert!(source_witness_json.get("postgresSnapshotSha256").is_some());
    assert!(noop_json.get("source_witness").is_none());

    let mut retry_source = source.clone();
    retry_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
    let second = publish_production_updates_schema_22(&store, &cursor_key, &binding, retry_source)
        .expect("idempotent production pair from a fresh exported snapshot");
    assert!(second.reused_current_snapshot);
    assert_eq!(second.snapshot_id, first.snapshot_id);
    assert_eq!(second.sequence, first.sequence);
    assert_eq!(second.pack_sha256, first.pack_sha256);
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key).expect("same pair"),
        (manifest_bytes, noop_bytes)
    );
    let pair_before_schema_rejections =
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("pair before rejection");
    let mut wrong_migration_count = source.clone();
    wrong_migration_count.schema.observed_migration_count = TESLAMATE_V4_MIGRATION_COUNT - 1;
    let error =
        publish_production_updates_schema_22(&store, &cursor_key, &binding, wrong_migration_count)
            .expect_err("contradictory migration count must fail closed");
    assert!(error.message.contains("contradicts"));
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("pair survives migration-count rejection"),
        pair_before_schema_rejections
    );
    let mut wrong_migration_version = source.clone();
    wrong_migration_version.schema.observed_migration_version -= 1;
    let error = publish_production_updates_schema_22(
        &store,
        &cursor_key,
        &binding,
        wrong_migration_version,
    )
    .expect_err("contradictory migration high-water must fail closed");
    assert!(error.message.contains("contradicts"));
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("pair survives migration-version rejection"),
        pair_before_schema_rejections
    );
    let stale_a_head = production_updates_head(&store, binding.vehicle_id).expect("capture A head");

    let mut changed_source = source.clone();
    changed_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
    changed_source.updates[1].version = Some("new exact version".into());
    let changed =
        publish_production_updates_schema_22(&store, &cursor_key, &binding, changed_source)
            .expect("changed source publishes a successor full snapshot");
    assert!(!changed.reused_current_snapshot);
    assert_ne!(changed.snapshot_id, first.snapshot_id);
    assert!(changed.sequence > first.sequence);
    assert_ne!(changed.source_logical_sha256, first.source_logical_sha256);
    let changed_manifest = store
        .manifest_for_vehicle(binding.vehicle_id)
        .expect("changed manifest lookup")
        .expect("changed manifest");
    assert_eq!(changed_manifest.generation, binding.generation);
    assert_eq!(changed_manifest.snapshot_id, changed.snapshot_id);
    assert_eq!(changed_manifest.head_sequence, changed.sequence);
    let changed_pair =
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key).expect("newer B pair");
    let stale_gate = store
        .try_acquire_publication_gate()
        .expect("stale A finalizer gate");
    let stale_error = publish_production_updates_schema_22_with_gate(
        &store,
        &cursor_key,
        &binding,
        source.clone(),
        &stale_gate,
        &stale_a_head,
        None,
    )
    .expect_err("A captured before newer B must not publish after B");
    drop(stale_gate);
    assert!(stale_error.message.contains("head changed"));
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("newer B pair survives stale A"),
        changed_pair
    );

    let mut empty_source = source.clone();
    empty_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
    empty_source.updates.clear();
    let empty =
        publish_production_updates_schema_22(&store, &cursor_key, &binding, empty_source.clone())
            .expect("zero-row snapshot publishes a signed watermark");
    assert!(!empty.reused_current_snapshot);
    assert!(empty.sequence > changed.sequence);
    assert_eq!(empty.source_witness.source_row_count, 0);
    assert_eq!(empty.source_witness.hub_row_count, 0);
    assert_eq!(empty.source_witness.source_open_row_count, 0);
    assert_eq!(empty.source_witness.source_null_version_row_count, 0);
    assert_eq!(empty.source_witness.source_start_min_pg_us, None);
    assert_eq!(empty.source_witness.source_end_max_pg_us, None);
    let empty_pair = schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
        .expect("zero-row signed pair");
    empty_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
    let empty_replay =
        publish_production_updates_schema_22(&store, &cursor_key, &binding, empty_source)
            .expect("zero-row replay is exact-byte idempotent");
    assert!(empty_replay.reused_current_snapshot);
    assert_eq!(empty_replay.snapshot_id, empty.snapshot_id);
    assert_eq!(empty_replay.sequence, empty.sequence);
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("same zero-row pair"),
        empty_pair
    );

    let current = store
        .manifest_for_vehicle(binding.vehicle_id)
        .expect("current manifest")
        .expect("published manifest");
    let mut wrong_binding = binding.clone();
    wrong_binding.selected_car_id = 8;
    let error =
        publish_production_updates_schema_22(&store, &cursor_key, &wrong_binding, source.clone())
            .expect_err("mismatched selected car must fail closed");
    assert!(error.message.contains("selected-car binding"));
    assert_eq!(
        store
            .manifest_for_vehicle(binding.vehicle_id)
            .expect("manifest after rejection")
            .expect("manifest retained"),
        current
    );

    let mut rebound_binding = binding.clone();
    rebound_binding.selected_car_id = 8;
    let mut rebound_source = source.clone();
    rebound_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
    rebound_source.car.id = 8;
    for row in &mut rebound_source.updates {
        row.car_id = 8;
    }
    let error =
        publish_production_updates_schema_22(&store, &cursor_key, &rebound_binding, rebound_source)
            .expect_err("same-generation successor cannot change the stored selected car");
    assert!(error.message.contains("stored selected-car witness"));
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("pair survives selected-car rebound"),
        empty_pair
    );

    let mut wrong_generation = binding.clone();
    wrong_generation.generation += 1;
    let error =
        publish_production_updates_schema_22(&store, &cursor_key, &wrong_generation, source)
            .expect_err("successor generation drift must fail closed");
    assert!(error.message.contains("identity or generation"));
    assert_eq!(
        store
            .manifest_for_vehicle(binding.vehicle_id)
            .expect("manifest after generation rejection")
            .expect("manifest retained"),
        current
    );

    let mut tampered_noop: SignedNoOpState =
        serde_json::from_slice(&empty_pair.1).expect("typed zero-row no-op");
    tampered_noop
        .source_witness
        .as_mut()
        .expect("production witness")
        .source_row_count = 1;
    let error = publish_updates_schema_22(&store, &current, &tampered_noop)
        .expect_err("tampered source watermark must fail closed");
    assert!(error.message.contains("source witness"));
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("pair survives witness tamper"),
        empty_pair
    );
}

#[test]
fn reopened_production_pack_root_mismatch_fails_closed() {
    let temporary = crate::private_tempdir().expect("pack directory");
    let source = production_source(7);
    let source_stream =
        encode_updates_logical_stream(&source.updates).expect("source logical stream");
    let source_capture = production_updates_capture_proof(&source);
    let snapshot = production_updates_snapshot(&source);
    let mut binding = pinned_updates_binding();
    binding.selected_car_id = 7;
    let request = ProjectionPackRequestV2_2 {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding,
        sequence: SequenceRange {
            from_exclusive: 1,
            to_inclusive: 1,
        },
        snapshot: &snapshot,
    };
    let built = ProjectionPackWriter::new(temporary.path())
        .write_full_snapshot_2_2(&request)
        .expect("write production-shaped pack");
    verify_reopened_production_capture(
        &built.metadata,
        &built.path,
        &source_stream,
        &source_capture,
    )
    .expect("all independently reopened roots match");

    let mut mismatched_source = source_capture;
    mismatched_source
        .global_settings
        .language
        .push_str("-changed");
    let error = verify_reopened_production_capture(
        &built.metadata,
        &built.path,
        &source_stream,
        &mismatched_source,
    )
    .expect_err("source-only root claim must be rejected");
    assert!(error.message.contains("roots or updates"));
}

#[test]
fn reopened_pack_rejects_size_bomb_and_row_cap_plus_one() {
    let temporary = crate::private_tempdir().expect("pack directory");
    let source = production_source(7);
    let snapshot = production_updates_snapshot(&source);
    let mut binding = pinned_updates_binding();
    binding.selected_car_id = 7;
    let request = ProjectionPackRequestV2_2 {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding,
        sequence: SequenceRange {
            from_exclusive: 1,
            to_inclusive: 1,
        },
        snapshot: &snapshot,
    };
    let built = ProjectionPackWriter::new(temporary.path())
        .write_full_snapshot_2_2(&request)
        .expect("write production-shaped pack");

    let mut wrong_compressed_size = built.metadata.clone();
    wrong_compressed_size.compressed_bytes += 1;
    assert!(
        private_sqlite_tempfile_from_pack(&wrong_compressed_size, &built.path).is_err(),
        "advertised compressed size mismatch must fail before allocation"
    );

    let mut row_capped = built.metadata.clone();
    row_capped.row_count = 1;
    let error = read_hub_updates_from_pack(&row_capped, &built.path)
        .expect_err("two updates exceed an advertised one-row cap");
    assert!(error.message.contains("row bound"));

    let expanded = (0..4_097)
        .map(|index| u8::try_from(index % 251).expect("bounded byte"))
        .collect::<Vec<_>>();
    let bomb = zstd::stream::encode_all(Cursor::new(&expanded), 1).expect("compress bomb");
    let bomb_path = temporary.path().join("bounded-bomb.zst");
    fs::write(&bomb_path, &bomb).expect("write bomb fixture");
    let mut bomb_metadata = built.metadata;
    bomb_metadata.compressed_bytes = u64::try_from(bomb.len()).expect("bomb size");
    bomb_metadata.uncompressed_bytes = 4_096;
    bomb_metadata.sha256 = crate::protocol::Sha256Digest::of_bytes(&bomb);
    assert!(
        private_sqlite_tempfile_from_pack(&bomb_metadata, &bomb_path).is_err(),
        "expanded cap-plus-one input must fail closed"
    );
}

#[test]
fn production_witness_rejects_overlapping_null_and_empty_denominators() {
    assert!(!validate_witness_summary(
        1,
        1,
        0,
        1,
        1,
        Some(10),
        Some(10),
        Some(20),
        Some(20),
    ));
}

#[test]
fn pinned_fixture_logical_stream_matches_frozen_digest() {
    let fixture = parse_pinned_updates_fixture().expect("parse pinned fixture");
    let stream = encode_updates_logical_stream(&fixture.rows).expect("encode");
    assert_eq!(stream.sha256, PINNED_CANONICAL_SHA256);
    assert_eq!(stream.bytes.len(), PINNED_CANONICAL_BYTES);
    assert_eq!(stream.summary.row_count, 6);
    assert_eq!(stream.summary.completed_row_count, 5);
    assert_eq!(stream.summary.open_row_count, 1);
    assert_eq!(stream.summary.null_version_row_count, 2);
    assert_eq!(stream.summary.empty_version_row_count, 1);
    assert_eq!(stream.summary.start_min_pg_us, Some(i64::MIN));
    assert_eq!(stream.summary.start_max_pg_us, Some(978_307_199_999_999));
    assert_eq!(stream.summary.end_min_pg_us, Some(-1));
    assert_eq!(stream.summary.end_max_pg_us, Some(i64::MAX));
    let decoded = decode_updates_logical_stream(&stream.bytes).expect("decode");
    assert_eq!(decoded.rows, stream.rows);
    assert!(decoded.rows.iter().any(|row| row.id == i32::MIN));
    assert!(decoded.rows.iter().any(|row| row.id == i32::MAX));
    assert!(
        decoded
            .rows
            .iter()
            .any(|row| row.version.as_deref() == Some(""))
    );
    assert!(
        decoded
            .rows
            .iter()
            .any(|row| row.version.is_none() && row.end_date_pg_us.is_none())
    );
    assert!(
        decoded
            .rows
            .iter()
            .any(|row| row.version.as_deref() == Some("  βeta 🚗  "))
    );
}

#[test]
fn copy_parser_preserves_null_empty_escapes_commas_and_quotes() {
    let fields = copy_fields(b"\\N,\"\",\\\\N,\"a,b\",\"a\"\"b\"").unwrap();
    assert_eq!(
        fields,
        vec![
            None,
            Some(Vec::new()),
            Some(b"\\N".to_vec()),
            Some(b"a,b".to_vec()),
            Some(b"a\"b".to_vec())
        ]
    );
}

#[test]
fn decompressed_sqlite_uses_private_raii_temporary_storage() {
    let (directory, file_path) = private_sqlite_tempfile(b"not a database").expect("temporary");
    let directory_path = directory.path.clone();
    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(&directory_path)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&file_path)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(directory);
    assert!(!file_path.exists());
    assert!(!directory_path.exists());
}

#[test]
fn timestamp_parser_matches_pinned_fixture_microseconds() {
    assert_eq!(parse_pg_timestamp_us(b"-infinity").unwrap(), i64::MIN);
    assert_eq!(parse_pg_timestamp_us(b"infinity").unwrap(), i64::MAX);
    assert_eq!(
        parse_pg_timestamp_us(b"1999-12-31 23:59:59.999999").unwrap(),
        -1
    );
    assert_eq!(
        parse_pg_timestamp_us(b"2026-01-01 00:00:00.123456").unwrap(),
        820_540_800_123_456
    );
    assert_eq!(
        parse_pg_timestamp_us(b"2030-12-31 23:59:59.999999").unwrap(),
        978_307_199_999_999
    );
}

#[test]
fn timestamp_parser_rejects_invalid_calendar_dates_and_times() {
    assert!(parse_pg_timestamp_us(b"2026-02-29 00:00:00").is_err());
    assert!(parse_pg_timestamp_us(b"2026-04-31 00:00:00").is_err());
    assert!(parse_pg_timestamp_us(b"1900-02-29 00:00:00").is_err());
    assert!(parse_pg_timestamp_us(b"2024-02-29 23:59:59.999999").is_ok());
    assert!(parse_pg_timestamp_us(b"2000-02-29 00:00:00").is_ok());
    assert!(parse_pg_timestamp_us(b"2026-01-01 24:00:00").is_err());
    assert!(parse_pg_timestamp_us(b"2026-01-01 00:60:00").is_err());
    assert!(parse_pg_timestamp_us(b"2026-01-01 00:00:60").is_err());
}
