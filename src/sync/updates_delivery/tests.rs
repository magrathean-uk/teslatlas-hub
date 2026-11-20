// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{
    db::{SourceDescriptor, VehicleDescriptor},
    source_db_projection::{
        SourceDbCarPhysicalV2_2, SourceDbCarSettingsPhysicalV2_2, SourceDbSettingsPhysicalV2_2,
    },
    source_db_reader::SourceDbSchemaInfo,
    source_db_schema::{
        MAX_VALIDATED_MIGRATION, SOURCE_DB_V4_1_1_SOURCE_REVISION, SOURCE_DB_V4_MIGRATION_COUNT,
        SOURCE_DB_V4_MIGRATION_SET_SHA256, V4_1_1_REVIEWED_SOURCE_SET, V4_2_REVIEWED_SOURCE_SET,
        V4_4_REVIEWED_SOURCE_SET,
    },
    updates_logical::decode_updates_logical_stream,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn production_source(car_id: i16) -> DirectUpdatesSourceV2_2 {
    DirectUpdatesSourceV2_2 {
        postgres_snapshot_sha256: hex_sha256(Uuid::new_v4().as_bytes()),
        schema: SourceDbSchemaInfo {
            observed_migration_version: MAX_VALIDATED_MIGRATION,
            observed_migration_count: SOURCE_DB_V4_MIGRATION_COUNT,
            minimum_supported_migration_version: MAX_VALIDATED_MIGRATION,
            maximum_validated_migration_version: MAX_VALIDATED_MIGRATION,
            pinned_source_revision: SOURCE_DB_V4_SOURCE_REVISION,
            pinned_migration_set_sha256: SOURCE_DB_V4_MIGRATION_SET_SHA256,
            fingerprint: hex_sha256(b"production-repeatable-read-schema-fingerprint"),
        },
        global_settings: SourceDbSettingsPhysicalV2_2 {
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
        car: SourceDbCarPhysicalV2_2 {
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
        car_settings: SourceDbCarSettingsPhysicalV2_2 {
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
            SourceDbUpdatePhysicalV2_2 {
                id: -4,
                car_id,
                start_date_pg_us: -1,
                end_date_pg_us: Some(0),
                version: None,
            },
            SourceDbUpdatePhysicalV2_2 {
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
            &SourceDescriptor::new(
                crate::db::IMPORT_SOURCE_KIND,
                format!("test-source-{}", Uuid::new_v4()),
            ),
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
    wrong_migration_count.schema.observed_migration_count = SOURCE_DB_V4_MIGRATION_COUNT - 1;
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
    let mut legacy_noop: SignedNoOpState =
        serde_json::from_slice(&empty_pair.1).expect("typed legacy no-op");
    {
        let witness = legacy_noop
            .source_witness
            .as_mut()
            .expect("production witness");
        witness.source_revision = SOURCE_DB_V4_1_1_SOURCE_REVISION.into();
        witness.pinned_migration_set_sha256 =
            V4_1_1_REVIEWED_SOURCE_SET.migration_set_sha256.into();
        witness.observed_migration_count = 105;
        witness.observed_migration_version = V4_1_1_REVIEWED_SOURCE_SET.newest_migration;
    }
    let legacy_witness = legacy_noop
        .source_witness
        .clone()
        .expect("legacy production witness");
    validate_production_source_witness(&legacy_noop, &legacy_witness)
        .expect("legacy beta witness remains readable under its own tuple");
    let mut mixed_witness = legacy_witness.clone();
    mixed_witness.pinned_migration_set_sha256 = SOURCE_DB_V4_MIGRATION_SET_SHA256.into();
    assert!(
        validate_production_source_witness(&legacy_noop, &mixed_witness).is_err(),
        "a legacy revision with the current migration set is not a reviewed tuple"
    );
    let mut unknown_witness = legacy_witness;
    unknown_witness.source_revision = "0000000000000000000000000000000000000000".into();
    assert!(validate_production_source_witness(&legacy_noop, &unknown_witness).is_err());
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
fn relative_cargo_target_directory_is_rejected() {
    let error = canonical_cargo_target_dir(Some(std::ffi::OsString::from("relative-target")))
        .expect_err("relative CARGO_TARGET_DIR must fail closed");
    assert!(error.message.contains("absolute path"));
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

fn registered_binding(store: &HubStore, selected_car_id: i64) -> ProjectionBinding {
    let registered_source = store
        .register_source(
            &SourceDescriptor::new(
                crate::db::IMPORT_SOURCE_KIND,
                format!("test-source-{}", Uuid::new_v4()),
            ),
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
    ProjectionBinding {
        installation_id: store.installation_id().expect("installation"),
        account_id: registered_source.source_id,
        vehicle_id: registered_vehicle.vehicle_id,
        generation: registered_source.generation,
        selected_car_id,
    }
}

fn v4_2_production_source(car_id: i16) -> DirectUpdatesSourceV2_2 {
    earlier_set_production_source(car_id, &V4_2_REVIEWED_SOURCE_SET)
}

fn earlier_set_production_source(
    car_id: i16,
    set: &'static ReviewedSourceSet,
) -> DirectUpdatesSourceV2_2 {
    let mut source = production_source(car_id);
    source.schema.observed_migration_version = set.newest_migration;
    source.schema.observed_migration_count = set.migration_count;
    source.schema.minimum_supported_migration_version = set.newest_migration;
    source.schema.maximum_validated_migration_version = set.newest_migration;
    source.schema.pinned_source_revision = set.source_revision;
    source.schema.pinned_migration_set_sha256 = set.migration_set_sha256;
    source
}

/// A store published under the v4.4.0 set still verifies after the v4.5.0
/// pin, a v4.4.0 capture is refused for new publications, and a current
/// capture publishes a successor over it.
#[test]
fn store_published_under_the_v4_4_set_verifies_and_publishes_a_current_successor() {
    let temp = crate::private_tempdir().expect("store root");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([46; 32]);
    let binding = registered_binding(&store, 7);

    let earlier = {
        let gate = store.try_acquire_publication_gate().expect("gate");
        let head = production_updates_head(&store, binding.vehicle_id).expect("head");
        publish_production_updates_schema_22_admitting(
            &store,
            &cursor_key,
            &binding,
            earlier_set_production_source(7, &V4_4_REVIEWED_SOURCE_SET),
            &gate,
            &head,
            None,
            &V4_4_REVIEWED_SOURCE_SET,
        )
        .expect("the earlier release publishes under the v4.4 set")
    };
    assert_eq!(earlier.source_witness.observed_migration_count, 107);
    let earlier_pair = schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
        .expect("the v4.4 pair still verifies after the upgrade");

    let error = publish_production_updates_schema_22(
        &store,
        &cursor_key,
        &binding,
        earlier_set_production_source(7, &V4_4_REVIEWED_SOURCE_SET),
    )
    .expect_err("a v4.4 capture is no longer admitted for new publications");
    assert!(
        error
            .message
            .contains("superseded reviewed source set 4.4.0")
    );
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("pair survives the refused capture"),
        earlier_pair
    );

    let successor =
        publish_production_updates_schema_22(&store, &cursor_key, &binding, production_source(7))
            .expect("a current capture publishes a successor over the v4.4 store");
    assert!(successor.sequence > earlier.sequence);
    assert_eq!(
        successor.source_witness.source_revision,
        SOURCE_DB_V4_SOURCE_REVISION
    );
    assert_eq!(successor.source_witness.observed_migration_count, 108);
    schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
        .expect("the successor pair verifies");
}

/// Review REV-M03-T08 finding 1: a store published by the earlier release
/// under the 20260808090000 set must still verify after the upgrade to the
/// 20261014120000 set and must accept a successor from a current capture.
#[test]
fn store_published_under_the_v4_2_set_verifies_and_publishes_a_current_successor() {
    let temp = crate::private_tempdir().expect("store root");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([44; 32]);
    let binding = registered_binding(&store, 7);

    let earlier = {
        let gate = store.try_acquire_publication_gate().expect("gate");
        let head = production_updates_head(&store, binding.vehicle_id).expect("head");
        publish_production_updates_schema_22_admitting(
            &store,
            &cursor_key,
            &binding,
            v4_2_production_source(7),
            &gate,
            &head,
            None,
            &V4_2_REVIEWED_SOURCE_SET,
        )
        .expect("the earlier release publishes under the v4.2 set")
    };
    assert_eq!(
        earlier.source_witness.source_revision,
        V4_2_REVIEWED_SOURCE_SET.source_revision
    );
    assert_eq!(earlier.source_witness.observed_migration_count, 105);
    let earlier_pair = schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
        .expect("the v4.2 pair still verifies after the upgrade");

    let error = publish_production_updates_schema_22(
        &store,
        &cursor_key,
        &binding,
        v4_2_production_source(7),
    )
    .expect_err("a v4.2 capture is no longer admitted for new publications");
    assert!(
        error
            .message
            .contains("superseded reviewed source set 4.2.0")
    );
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("pair survives the refused capture"),
        earlier_pair
    );

    let successor =
        publish_production_updates_schema_22(&store, &cursor_key, &binding, production_source(7))
            .expect("a current capture publishes a successor over the v4.2 store");
    assert!(!successor.reused_current_snapshot);
    assert_ne!(successor.snapshot_id, earlier.snapshot_id);
    assert!(successor.sequence > earlier.sequence);
    assert_eq!(
        successor.source_witness.source_revision,
        SOURCE_DB_V4_SOURCE_REVISION
    );
    assert_eq!(
        successor.source_witness.observed_migration_count,
        u64::try_from(SOURCE_DB_V4_MIGRATION_COUNT).expect("count")
    );
    schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
        .expect("the successor pair verifies");
}

/// A capture or witness naming an unknown revision is refused even when its
/// migration facts copy a reviewed set.
#[test]
fn unknown_source_revision_is_refused_for_captures_and_witnesses() {
    let temp = crate::private_tempdir().expect("store root");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([45; 32]);
    let binding = registered_binding(&store, 7);
    let mut unknown = production_source(7);
    unknown.schema.pinned_source_revision = "0000000000000000000000000000000000000000";
    let error = publish_production_updates_schema_22(&store, &cursor_key, &binding, unknown)
        .expect_err("unknown revision must fail closed");
    assert!(error.message.contains("contradicts"));
    let mut mixed = v4_2_production_source(7);
    mixed.schema.pinned_migration_set_sha256 = SOURCE_DB_V4_MIGRATION_SET_SHA256;
    let error = publish_production_updates_schema_22(&store, &cursor_key, &binding, mixed)
        .expect_err("a v4.2 revision with the current hash is not a reviewed tuple");
    assert!(error.message.contains("contradicts"));
    assert!(
        store
            .manifest_for_vehicle(binding.vehicle_id)
            .expect("manifest lookup")
            .is_none()
    );
}

/// HUB-UPDATES-LOGICAL-NOOP: an unchanged capture is answered from its logical
/// content and full source binding without building a pack, so a new SQLite or
/// compression library, which would encode the same rows into other bytes,
/// writes no replacement pack. A change of any captured root still publishes.
#[cfg(unix)]
#[test]
fn unchanged_capture_is_answered_logically_without_writing_a_pack() {
    let temp = crate::private_tempdir().expect("store root");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([47; 32]);
    let binding = registered_binding(&store, 7);
    let source = production_source(7);
    let first = publish_production_updates_schema_22(&store, &cursor_key, &binding, source.clone())
        .expect("first publication");
    assert!(!first.reused_current_snapshot);
    let pair = schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
        .expect("published pair");

    // Building a pack always stages it in the packs folder's private staging
    // folder first; holding that folder read-only makes any rebuild fail, so
    // success proves the answer is logical.
    let packs = store.packs_dir();
    let staging = packs.join(".staging");
    assert!(staging.is_dir(), "the first publication staged its pack");
    let held = [staging, packs.join("sha256"), packs.to_path_buf()];
    let listing = |path: &std::path::Path| {
        let mut names: Vec<_> = fs::read_dir(path)
            .expect("list packs")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        names.sort();
        names
    };
    let before: Vec<_> = held.iter().map(|path| listing(path)).collect();
    fs::set_permissions(&held[0], fs::Permissions::from_mode(0o500)).expect("hold staging");
    let mut retry = source.clone();
    retry.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
    let replay = publish_production_updates_schema_22(&store, &cursor_key, &binding, retry);
    fs::set_permissions(&held[0], fs::Permissions::from_mode(0o700)).expect("release staging");
    let replay = replay.expect("unchanged capture is answered without a pack write");
    assert!(replay.reused_current_snapshot);
    assert_eq!(replay.state, "schema_2_2_already_current");
    assert_eq!(replay.snapshot_id, first.snapshot_id);
    assert_eq!(replay.pack_sha256, first.pack_sha256);
    assert_eq!(replay.noop_sha256, first.noop_sha256);
    assert_eq!(held.iter().map(|path| listing(path)).collect::<Vec<_>>(), before);
    assert_eq!(
        schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key).expect("same pair"),
        pair
    );

    // The binding covers every captured root, not only the updates rows.
    let mut settings_changed = source;
    settings_changed.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
    settings_changed.car_settings.suspend_min += 1;
    let successor =
        publish_production_updates_schema_22(&store, &cursor_key, &binding, settings_changed)
            .expect("changed car settings publish a successor");
    assert!(!successor.reused_current_snapshot);
    assert!(successor.sequence > first.sequence);
    assert_eq!(successor.source_logical_sha256, first.source_logical_sha256);
}

// Issue #100: a schema-2.2 read pins the selected manifest/no-op pair until it
// owns the sidecar bytes, and successor cleanup waits for that pin.

struct Schema22Race {
    _temp: tempfile::TempDir,
    store: HubStore,
    cursor_key: CursorKey,
    binding: ProjectionBinding,
    snapshot_a: Uuid,
    pair_a: (Vec<u8>, Vec<u8>),
}

fn schema_22_race_store() -> Schema22Race {
    let temp = crate::private_tempdir().expect("store root");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([45; 32]);
    let binding = registered_binding(&store, 7);
    let published = publish_successor(&store, &cursor_key, &binding, "A");
    let pair_a = schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key).expect("A");
    Schema22Race {
        _temp: temp,
        store,
        cursor_key,
        binding,
        snapshot_a: published.snapshot_id,
        pair_a,
    }
}

fn publish_successor(
    store: &HubStore,
    cursor_key: &CursorKey,
    binding: &ProjectionBinding,
    version: &str,
) -> ProductionUpdatesPublication {
    let mut source = production_source(7);
    source.updates[1].version = Some(version.into());
    publish_production_updates_schema_22(store, cursor_key, binding, source)
        .expect("publish successor")
}

fn noop_directory(store: &HubStore) -> PathBuf {
    store
        .database_path()
        .parent()
        .expect("data root")
        .join("packs/noop")
}

fn sidecar_path(store: &HubStore, vehicle_id: Uuid, snapshot_id: Uuid) -> PathBuf {
    noop_directory(store).join(format!("{vehicle_id}.{snapshot_id}.json"))
}

/// Whether another open of the no-op directory can take the exclusive pin now.
fn exclusive_pin_is_free(store: &HubStore) -> bool {
    use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
    let fd = open(
        noop_directory(store).as_path(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .expect("open no-op directory");
    match flock(&fd, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            flock(&fd, FlockOperation::Unlock).expect("unlock");
            true
        }
        Err(rustix::io::Errno::WOULDBLOCK) => false,
        Err(error) => panic!("probe the artefact pin: {error}"),
    }
}

/// Run a read on its own thread that pauses after selecting its manifest,
/// reporting the selected snapshot and waiting for `resume`.
fn paused_reader<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    race: &'scope Schema22Race,
    selected: std::sync::mpsc::Sender<Uuid>,
    resume: std::sync::mpsc::Receiver<()>,
) -> std::thread::ScopedJoinHandle<'scope, Result<(Vec<u8>, Vec<u8>), UpdatesDeliveryError>> {
    scope.spawn(move || {
        set_schema_22_read_after_selection_hook(Some(Box::new(move |manifest| {
            selected.send(manifest.snapshot_id).expect("report selection");
            resume.recv().expect("resume");
            Ok(())
        })));
        let result = schema_22_signed_artifacts(&race.store, race.binding.vehicle_id, &race.cursor_key);
        set_schema_22_read_after_selection_hook(None);
        result
    })
}

/// Start C's publication on its own thread, reporting when its cleanup is
/// about to wait for the exclusive pin.
fn successor_cleanup<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    race: &'scope Schema22Race,
    waiting: std::sync::mpsc::Sender<()>,
) -> std::thread::ScopedJoinHandle<'scope, ProductionUpdatesPublication> {
    scope.spawn(move || {
        crate::db::set_schema_22_cleanup_before_exclusive_pin_hook(Some(Box::new(move || {
            let _ = waiting.send(());
        })));
        let published = publish_successor(&race.store, &race.cursor_key, &race.binding, "C");
        crate::db::set_schema_22_cleanup_before_exclusive_pin_hook(None);
        published
    })
}

#[test]
fn design_later_100_paused_reader_keeps_selected_pair_across_successor_cleanup() {
    let race = schema_22_race_store();
    let path_a = sidecar_path(&race.store, race.binding.vehicle_id, race.snapshot_a);
    let (published_c, read) = std::thread::scope(|scope| {
        let (selected_tx, selected_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let reader = paused_reader(scope, &race, selected_tx, resume_rx);
        assert_eq!(selected_rx.recv().expect("selection"), race.snapshot_a);

        // B keeps A as the prior current pair, so it needs no unlink phase.
        publish_successor(&race.store, &race.cursor_key, &race.binding, "B");
        assert!(path_a.exists(), "B's cleanup keeps the prior current A");

        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let cleanup = successor_cleanup(scope, &race, waiting_tx);
        waiting_rx.recv().expect("C cleanup reaches its unlink phase");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!cleanup.is_finished(), "C cleanup waits for the reader pin");
        assert!(path_a.exists(), "A survives while its reader holds the pin");

        resume_tx.send(()).expect("resume reader");
        let read = reader.join().expect("reader thread");
        (cleanup.join().expect("cleanup thread"), read)
    });
    assert_eq!(read.expect("A's pair"), race.pair_a);
    // The returned bytes are owned: A's sidecar no longer exists once C
    // finishes, and the current pair is C's.
    assert!(!path_a.exists(), "C's cleanup removed A after the pin dropped");
    let current = schema_22_signed_artifacts(&race.store, race.binding.vehicle_id, &race.cursor_key)
        .expect("C pair");
    let manifest: SyncManifest = serde_json::from_slice(&current.0).expect("manifest");
    assert_eq!(manifest.snapshot_id, published_c.snapshot_id);
    assert!(exclusive_pin_is_free(&race.store));
}

#[test]
fn design_later_100_concurrent_readers_share_the_pin_and_a_cancelled_read_releases_it() {
    let race = schema_22_race_store();
    std::thread::scope(|scope| {
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        let (second_tx, second_rx) = std::sync::mpsc::channel();
        let (resume_first_tx, resume_first_rx) = std::sync::mpsc::channel();
        let (resume_second_tx, resume_second_rx) = std::sync::mpsc::channel();
        let first = paused_reader(scope, &race, first_tx, resume_first_rx);
        let second = paused_reader(scope, &race, second_tx, resume_second_rx);
        // Both readers hold the shared pin at once; only cleanup is excluded.
        assert_eq!(first_rx.recv().expect("first selection"), race.snapshot_a);
        assert_eq!(second_rx.recv().expect("second selection"), race.snapshot_a);
        assert!(!exclusive_pin_is_free(&race.store));
        resume_first_tx.send(()).expect("resume first");
        resume_second_tx.send(()).expect("resume second");
        assert_eq!(first.join().expect("first").expect("first pair"), race.pair_a);
        assert_eq!(second.join().expect("second").expect("second pair"), race.pair_a);
    });
    assert!(exclusive_pin_is_free(&race.store));

    // A read abandoned after selection leaves no pin behind.
    let cancelled = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                set_schema_22_read_after_selection_hook(Some(Box::new(|_| {
                    Err(reject("read cancelled after selection"))
                })));
                let result = schema_22_signed_artifacts(
                    &race.store,
                    race.binding.vehicle_id,
                    &race.cursor_key,
                );
                set_schema_22_read_after_selection_hook(None);
                result
            })
            .join()
            .expect("cancelled reader")
    });
    assert!(cancelled.expect_err("cancelled").message.contains("cancelled"));
    assert!(exclusive_pin_is_free(&race.store));
    publish_successor(&race.store, &race.cursor_key, &race.binding, "B");
    publish_successor(&race.store, &race.cursor_key, &race.binding, "C");
    assert!(!sidecar_path(&race.store, race.binding.vehicle_id, race.snapshot_a).exists());
}

#[test]
fn design_later_100_pinned_read_still_fails_closed_and_releases_the_pin() {
    let race = schema_22_race_store();
    let vehicle_id = race.binding.vehicle_id;
    let path_a = sidecar_path(&race.store, vehicle_id, race.snapshot_a);

    let wrong_key = CursorKey::from_bytes([46; 32]);
    schema_22_signed_artifacts(&race.store, vehicle_id, &wrong_key)
        .expect_err("wrong signing key fails closed");
    assert!(exclusive_pin_is_free(&race.store));

    let original = fs::read(&path_a).expect("A sidecar");
    let value: serde_json::Value = serde_json::from_slice(&original).expect("A JSON");
    fs::write(&path_a, serde_json::to_vec_pretty(&value).expect("pretty")).expect("rewrite");
    let error = schema_22_signed_artifacts(&race.store, vehicle_id, &race.cursor_key)
        .expect_err("non-canonical sidecar fails closed");
    assert!(error.message.contains("canonical"));
    assert!(exclusive_pin_is_free(&race.store));
    fs::write(&path_a, &original).expect("restore");

    let aside = race._temp.path().join("aside.json");
    fs::rename(&path_a, &aside).expect("move A aside");
    std::os::unix::fs::symlink(&aside, &path_a).expect("symlink");
    schema_22_signed_artifacts(&race.store, vehicle_id, &race.cursor_key)
        .expect_err("symlinked sidecar is refused");
    assert!(exclusive_pin_is_free(&race.store));
    fs::remove_file(&path_a).expect("remove symlink");
    fs::rename(&aside, &path_a).expect("restore A");

    fs::remove_file(&path_a).expect("remove A");
    let error = schema_22_signed_artifacts(&race.store, vehicle_id, &race.cursor_key)
        .expect_err("absent sidecar fails closed");
    assert!(error.message.contains("not catalogued"));
    assert!(exclusive_pin_is_free(&race.store));
}

#[test]
fn design_later_100_cleanup_waits_for_a_pin_held_by_another_process() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let race = schema_22_race_store();
    let path_a = sidecar_path(&race.store, race.binding.vehicle_id, race.snapshot_a);
    publish_successor(&race.store, &race.cursor_key, &race.binding, "B");

    let mut holder = Command::new("/usr/bin/perl")
        .args([
            "-e",
            r#"use Fcntl qw(:flock O_RDONLY); sysopen(my $f, $ARGV[0], O_RDONLY) or die "open: $!"; flock($f, LOCK_SH) or die "lock: $!"; $| = 1; print "locked\n"; <STDIN>;"#,
        ])
        .arg(noop_directory(&race.store))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn pin holder");
    let mut line = String::new();
    BufReader::new(holder.stdout.as_mut().expect("holder stdout"))
        .read_line(&mut line)
        .expect("holder reports");
    assert_eq!(line, "locked\n");

    let published_c = std::thread::scope(|scope| {
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let cleanup = successor_cleanup(scope, &race, waiting_tx);
        waiting_rx.recv().expect("C cleanup reaches its unlink phase");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!cleanup.is_finished(), "cleanup waits for the other process");
        assert!(path_a.exists(), "A survives while another process pins it");
        drop(holder.stdin.take());
        assert!(holder.wait().expect("holder exits").success());
        cleanup.join().expect("cleanup thread")
    });
    assert!(!path_a.exists());
    let current = schema_22_signed_artifacts(&race.store, race.binding.vehicle_id, &race.cursor_key)
        .expect("C pair");
    let manifest: SyncManifest = serde_json::from_slice(&current.0).expect("manifest");
    assert_eq!(manifest.snapshot_id, published_c.snapshot_id);
}
