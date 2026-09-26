// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs::File, io::BufReader};

use rusqlite::Connection;
use tempfile::tempdir;

use super::*;
use crate::{
    hub_pack::{
        ProjectionPreferredRangeV2_2, ProjectionUnitOfLengthV2_2, ProjectionUnitOfPressureV2_2,
        ProjectionUnitOfTemperatureV2_2,
    },
    storage::db::HubStore,
    teslamate_stage::TeslaMateStageLimits,
};

fn stage_limits() -> TeslaMateStageLimits {
    TeslaMateStageLimits {
        max_rows: 32,
        max_stage_bytes: 2 * 1024 * 1024,
        minimum_free_bytes: 0,
    }
}

fn binding() -> ProjectionBinding {
    ProjectionBinding {
        installation_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        account_id: Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
        vehicle_id: Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap(),
        generation: 1,
        selected_car_id: 1,
    }
}

fn snapshot_id() -> Uuid {
    Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap()
}

fn sequence() -> SequenceRange {
    SequenceRange {
        from_exclusive: 7,
        to_inclusive: 7,
    }
}

fn seed_roots(stage: &mut TeslaMateStage) {
    let settings = TeslaMateSettingsPhysicalV2_2 {
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
    };
    let car_settings = TeslaMateCarSettingsPhysicalV2_2 {
        id: 9,
        suspend_min: 21,
        suspend_after_idle_min: 15,
        req_not_unlocked: false,
        free_supercharging: false,
        use_streaming_api: true,
        enabled: true,
        lfp_battery: false,
    };
    let car = TeslaMateCarPhysicalV2_2 {
        id: 1,
        eid: 100,
        vid: 200,
        vin: Some("physical-vin".into()),
        name: Some("physical-car".into()),
        model: Some("3".into()),
        efficiency: Some(0.153),
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        display_priority: 1,
        inserted_at_pg_us: 0,
        updated_at_pg_us: 0,
        settings_id: car_settings.id,
    };
    stage
        .insert(TeslaMateStageTable::GlobalSettings, settings.id, &settings)
        .expect("settings root");
    stage
        .insert(
            TeslaMateStageTable::CarSettings,
            car_settings.id,
            &car_settings,
        )
        .expect("car settings root");
    stage
        .insert(TeslaMateStageTable::Cars, i64::from(car.id), &car)
        .expect("car root");
}

fn seed_updates(stage: &mut TeslaMateStage, ids: &[i32]) {
    for id in ids {
        let update = TeslaMateUpdatePhysicalV2_2 {
            id: *id,
            car_id: 1,
            start_date_pg_us: i64::from(*id) * 1_000_000,
            end_date_pg_us: None,
            version: Some(format!("2026.{id}")),
        };
        stage
            .insert(TeslaMateStageTable::Updates, i64::from(*id), &update)
            .expect("physical update");
    }
}

fn chunk_limits() -> TeslaMatePhysicalFragmentLimits {
    TeslaMatePhysicalFragmentLimits {
        max_rows_per_chunk: 4,
        max_projected_json_bytes: 64 * 1024,
    }
}

fn decode_pack(chunk: &BuiltProjectionPack, destination: &std::path::Path) {
    let input = File::open(&chunk.path).expect("compressed pack");
    let mut decoder = zstd::stream::read::Decoder::new(BufReader::new(input)).expect("decoder");
    let mut output = File::create(destination).expect("decoded pack");
    std::io::copy(&mut decoder, &mut output).expect("decode pack");
}

#[test]
fn physical_writer_requires_an_explicit_sealed_physical_stage() {
    let temporary = tempdir().expect("temp dir");
    let writer = ProjectionPackWriter::new(temporary.path().join("packs"));
    let cursor_key = CursorKey::from_bytes([7; 32]);

    let physical = TeslaMateStage::create_physical_v3(
        temporary.path().join("physical-imports"),
        stage_limits(),
    )
    .expect("physical stage");
    let error = write_staged_physical_updates_snapshot_v3(
        &physical,
        &writer,
        binding(),
        snapshot_id(),
        sequence(),
        &cursor_key,
    )
    .expect_err("open stage");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::StageNotSealed
    ));

    let mut compatibility =
        TeslaMateStage::create(temporary.path().join("compat-imports"), stage_limits())
            .expect("compatibility stage");
    compatibility.seal().expect("seal compatibility stage");
    let error = write_staged_physical_updates_snapshot_v3(
        &compatibility,
        &writer,
        binding(),
        snapshot_id(),
        sequence(),
        &cursor_key,
    )
    .expect_err("compatibility stage");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::WrongStageFormat
    ));
}

#[test]
fn physical_updates_writer_rejects_unsupported_relations_before_writing_a_chunk() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage =
        TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
            .expect("physical stage");
    seed_roots(&mut stage);
    stage
        .insert(
            TeslaMateStageTable::States,
            20,
            &serde_json::json!({"id": 20}),
        )
        .expect("unsupported physical state marker");
    stage.seal().expect("seal physical stage");

    let writer = ProjectionPackWriter::new(store.packs_dir());
    let error = write_staged_physical_updates_snapshot_v3(
        &stage,
        &writer,
        binding(),
        snapshot_id(),
        sequence(),
        &CursorKey::from_bytes([10; 32]),
    )
    .expect_err("unsupported relation rows");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::UnsupportedTableRows { table: "states" }
    ));
    assert!(!store.packs_dir().join("sha256").exists());
}

#[test]
fn sealed_physical_stage_streams_verified_contiguous_v3_chunks_without_catalogue_rows() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage =
        TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
            .expect("physical stage");
    seed_roots(&mut stage);
    seed_updates(&mut stage, &[10, 11, 12]);
    stage.seal().expect("seal physical stage");

    let writer = ProjectionPackWriter::new(store.packs_dir());
    let cursor_key = CursorKey::from_bytes([8; 32]);
    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &writer,
        binding(),
        snapshot_id(),
        sequence(),
        &cursor_key,
        chunk_limits(),
    )
    .expect("physical candidate");

    assert_eq!(candidate.chunks.len(), 3);
    assert_eq!(candidate.manifest.chunks.len(), 3);
    assert_eq!(candidate.logical_source_rows, 6);
    assert_eq!(candidate.manifest.total_rows, 12);
    candidate
        .manifest
        .validate_terminal_cursor(&cursor_key)
        .expect("signed manifest cursor");
    let mut update_ids = Vec::new();
    for (ordinal, chunk) in candidate.chunks.iter().enumerate() {
        assert_eq!(chunk.metadata.ordinal, ordinal as u32);
        assert_eq!(chunk.metadata.snapshot_id, snapshot_id());
        assert_eq!(chunk.metadata.sequence, sequence());
        assert!(chunk.metadata.compressed_bytes <= HUB_SYNC_PROFILE_MAX_PACK_BYTES);
        chunk
            .metadata
            .verify_reader(
                File::open(&chunk.path).expect("pack file"),
                ProtocolLimits::default(),
            )
            .expect("verified compressed pack");

        let decoded = temporary.path().join(format!("chunk-{ordinal}.sqlite"));
        decode_pack(chunk, &decoded);
        let connection = Connection::open(decoded).expect("decoded SQLite");
        for table in ["global_settings", "cars", "car_settings"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("root count");
            assert_eq!(count, 1, "{table} root must repeat in every chunk");
        }
        let mut statement = connection
            .prepare("SELECT id FROM updates ORDER BY id")
            .expect("update query");
        update_ids.extend(
            statement
                .query_map([], |row| row.get::<_, i32>(0))
                .expect("updates")
                .collect::<Result<Vec<_>, _>>()
                .expect("update ids"),
        );
    }
    assert_eq!(update_ids, vec![10, 11, 12]);

    let connection = store.open().expect("catalogue");
    for table in ["sync_manifests", "sync_packs"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0, "writer proof must not publish {table}");
    }
}

#[test]
fn failed_later_chunk_removes_created_objects_and_never_touches_the_catalogue() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage =
        TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
            .expect("physical stage");
    seed_roots(&mut stage);
    seed_updates(&mut stage, &[10, 11]);
    stage.seal().expect("seal physical stage");

    let writer = ProjectionPackWriter::new(store.packs_dir());
    let error = write_staged_physical_updates_snapshot_v3_with_test_failure(
        &stage,
        &writer,
        binding(),
        snapshot_id(),
        sequence(),
        &CursorKey::from_bytes([9; 32]),
        chunk_limits(),
        1,
    )
    .expect_err("injected second chunk failure");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::InjectedTestFailure(1)
    ));

    let content = store.packs_dir().join("sha256");
    let remaining = std::fs::read_dir(content)
        .expect("content directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("content entries");
    assert!(remaining.is_empty());
    let connection = store.open().expect("catalogue");
    for table in ["sync_manifests", "sync_packs"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0);
    }
}
