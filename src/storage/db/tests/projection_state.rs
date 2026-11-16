// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn digest_projection_state_is_atomic_with_base_and_successor_heads() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = import_delta_test_car(binding.selected_car_id);
    let inventory = SourceDbImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let state = test_projection_state(temporary.path(), &car);
    store
        .finalize_source_db_import_snapshot_with_projection_state(
            &manifest,
            Sha256Digest::of_bytes(b"projection-state-base"),
            &[],
            &binding,
            &inventory,
            &state,
        )
        .expect("catalogue base and state atomically");

    let mut lookup = store
        .source_db_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("verified projection-state lookup");
    assert_eq!(lookup.header().base_snapshot_id, manifest.snapshot_id);
    assert_eq!(lookup.header().head_sequence, manifest.head_sequence);
    assert!(
        lookup
            .digest(SourceDbProjectionStateEntity::Car, binding.selected_car_id)
            .expect("lookup car digest")
            .is_some()
    );
    let page = lookup.page_after(None, 10).expect("bounded state page");
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].entity, SourceDbProjectionStateEntity::Car);
    drop(lookup);

    let connection = store.open().expect("state catalogue");
    let (rows, digest_bytes): (i64, i64) = connection
        .query_row(
            concat!(
                "SELECT COUNT(*), COALESCE(MAX(length(projection_sha256)), 0)
                   FROM ",
                crate::db::import_table_names::import_projection_state_rows_table!()
            ),
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
        .source_db_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("prior state");
    let successor_state = SourceDbProjectionState::create(
        temporary.path(),
        crate::import_projection_state::SourceDbProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("successor state");
    let mut successor =
        crate::import_projection_state::SourceDbProjectionStateCapture::for_successor(
            successor_state,
            Box::new(prior),
        );
    assert_eq!(
        successor.record_car(&car).expect("capture unchanged car"),
        crate::import_projection_state::SourceDbProjectionStateChange::Unchanged
    );
    successor.seal().expect("seal successor state");
    let successor_state = successor.into_state();
    store
        .finalize_source_db_import_delta_successor_with_projection_state(
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
        .source_db_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("updated state lookup");
    assert_eq!(lookup.header().head_sequence, delta.to_sequence);
}

fn direct_materialised_base(
    store: &HubStore,
    drives: &[ProjectionDrive],
) -> (
    VehicleRecord,
    ProjectionBinding,
    ProjectionCar,
    LineageManifestV2,
) {
    direct_materialised_base_with_firmware(store, drives, None)
}

/// As `direct_materialised_base`, with the installed version the import left
/// on the car row (the newest imported update's version).
fn direct_materialised_base_with_firmware(
    store: &HubStore,
    drives: &[ProjectionDrive],
    firmware_version: Option<&str>,
) -> (
    VehicleRecord,
    ProjectionBinding,
    ProjectionCar,
    LineageManifestV2,
) {
    let (vehicle, binding, manifest) = v2_base_manifest(store);
    let mut car = import_delta_test_car(binding.selected_car_id);
    car.firmware_version = firmware_version.map(str::to_owned);
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
        )
        .expect("staging direct base");
    store
        .stage_import_generation_session(
            run_id,
            &SourceDbOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("stage direct base session");
    let state = create_direct_import_projection_state(store, run_id, 32);
    let mut capture =
        crate::import_projection_state::SourceDbProjectionStateCapture::for_initial_base(state);
    capture.record_car(&car).expect("capture base car");
    for drive in drives {
        capture.record_drive(drive).expect("capture base drive");
    }
    capture.seal().expect("seal direct base state");
    let state = capture.into_state();
    store
        .finalize_import_generation_with_projection_state_and_materialisation(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
            &manifest,
            Sha256Digest::of_bytes(b"direct-materialised-base"),
            &[],
            &binding,
            &state,
            &car,
            drives,
            false,
        )
        .expect("publish direct materialised base");
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("direct base lineage")
        .expect("direct base exists");
    (vehicle, binding, car, lineage)
}

#[test]
fn unchanged_direct_import_self_heals_missing_materialisation() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let drive = import_materialised_test_drive(7, 10, 4.0);
    let (vehicle, binding, car, _) = direct_materialised_base(&store, std::slice::from_ref(&drive));
    store
        .open()
        .expect("catalogue")
        .execute_batch("DELETE FROM materialised_cars; DELETE FROM materialised_drives;")
        .expect("simulate pre-repair missing read model");
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            3_000,
        )
        .expect("unchanged staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &SourceDbOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("unchanged staged session");
    store
        .promote_import_generation_with_materialisation(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            3_000,
            &car,
            std::slice::from_ref(&drive),
        )
        .expect("unchanged direct self-heal");
    assert_eq!(
        store
            .materialised_car_for_vehicle(vehicle.vehicle_id)
            .expect("car after self-heal"),
        Some(car)
    );
    assert_eq!(
        store
            .materialised_drive_for_vehicle(vehicle.vehicle_id, drive.id)
            .expect("drive after self-heal"),
        Some(drive)
    );
}

#[test]
fn direct_materialisation_validation_rolls_back_initial_publication() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = import_delta_test_car(binding.selected_car_id);
    let drive = import_materialised_test_drive(7, binding.selected_car_id, 4.0);
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
            &SourceDbOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("staged session");
    // The materialisation claims a drive which is absent from the sealed
    // projection state. The whole publication must remain retryable.
    let state = direct_test_projection_state(&store, run_id, &car);
    assert!(matches!(
        store.finalize_import_generation_with_projection_state_and_materialisation(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
            &manifest,
            Sha256Digest::of_bytes(b"invalid-materialisation"),
            &[],
            &binding,
            &state,
            &car,
            std::slice::from_ref(&drive),
            false,
        ),
        Err(StoreError::LineageCatalogConflict)
    ));
    assert!(
        store
            .manifest_for_vehicle(vehicle.vehicle_id)
            .expect("manifest after rollback")
            .is_none()
    );
    assert!(
        store
            .materialised_car_for_vehicle(vehicle.vehicle_id)
            .expect("car after rollback")
            .is_none()
    );
    assert!(
        store
            .materialised_drive_for_vehicle(vehicle.vehicle_id, drive.id)
            .expect("drive after rollback")
            .is_none()
    );
    assert!(
        staging_generation_exists(&store, run_id),
        "failed publication retains its staging generation"
    );
}

#[test]
fn direct_successor_reconciles_imported_drives_and_preserves_live_only_rows() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let first = import_materialised_test_drive(7, 10, 4.0);
    let removed = import_materialised_test_drive(8, 10, 5.0);
    let (vehicle, binding, mut car, base) =
        direct_materialised_base(&store, &[first.clone(), removed.clone()]);
    let live_only = import_materialised_test_drive(99, binding.selected_car_id, 9.0);
    let live_json = serde_json::to_string(&live_only).expect("serialize live-only drive");
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                vehicle.vehicle_id.to_string(),
                live_only.id,
                live_only.car_id,
                live_json
            ],
        )
        .expect("seed live-only drive");

    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            3_000,
        )
        .expect("successor staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &SourceDbOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("successor staged session");
    let prior = store
        .source_db_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("prior imported state");
    let state = create_direct_import_projection_state(&store, run_id, 32);
    let mut capture = crate::import_projection_state::SourceDbProjectionStateCapture::for_successor(
        state,
        Box::new(prior),
    );
    car.firmware_version = Some("2026.20".into());
    let updated = import_materialised_test_drive(7, binding.selected_car_id, 6.0);
    capture.record_car(&car).expect("capture updated car");
    capture
        .record_drive(&updated)
        .expect("capture updated retained drive");
    capture.seal().expect("seal successor state");
    let state = capture.into_state();
    let delta = imported_typed_delta(&store, &binding, &base);
    store
        .finalize_import_generation_delta_successors_with_projection_state_and_materialisation(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            3_000,
            std::slice::from_ref(&delta),
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
            Sha256Digest::of_bytes(b"direct-materialised-successor"),
            &[],
            &state,
            &car,
            std::slice::from_ref(&updated),
        )
        .expect("publish materialised successor");
    assert_eq!(
        store
            .materialised_drive_for_vehicle(vehicle.vehicle_id, updated.id)
            .expect("updated drive"),
        Some(updated)
    );
    assert!(
        store
            .materialised_drive_for_vehicle(vehicle.vehicle_id, removed.id)
            .expect("removed imported drive")
            .is_none()
    );
    assert_eq!(
        store
            .materialised_drive_for_vehicle(vehicle.vehicle_id, live_only.id)
            .expect("live-only drive"),
        Some(live_only)
    );
    assert_eq!(
        store
            .materialised_car_for_vehicle(vehicle.vehicle_id)
            .expect("updated car"),
        Some(car)
    );
}

#[test]
fn sparse_direct_successor_writes_only_changed_projection_and_drive_rows() {
    for count in [8_i64, 24] {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let mut drives: Vec<_> = (0..count)
            .map(|index| import_materialised_test_drive(7 + index, 10, 4.0 + index as f64))
            .collect();
        let (vehicle, binding, car, base) = direct_materialised_base(&store, &drives);
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                3_000,
            )
            .expect("successor generation");
        store
            .stage_import_generation_session(
                run_id,
                &SourceDbOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("session");
        let prior = store
            .source_db_import_projection_state_lookup(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("prior state");
        let state = create_direct_import_projection_state(&store, run_id, 32);
        let mut capture =
            crate::import_projection_state::SourceDbProjectionStateCapture::for_successor(
                state,
                Box::new(prior),
            );
        capture.record_car(&car).expect("car");
        drives[0] = import_materialised_test_drive(7, binding.selected_car_id, 6.0);
        let mut changed = 0;
        for drive in &drives {
            let change = capture.record_drive(drive).expect("capture drive");
            changed += usize::from(
                change != crate::import_projection_state::SourceDbProjectionStateChange::Unchanged,
            );
        }
        assert_eq!(changed, 1);
        capture.seal().expect("seal");
        let state = capture.into_state();
        let connection = store.open().expect("measurement connection");
        connection.execute(
            "UPDATE materialised_drives SET inside_temp_avg=12345 WHERE vehicle_id=?1 AND drive_id=?2",
            params![vehicle.vehicle_id.to_string(), drives[1].id],
        ).expect("stale derived-only column with unchanged canonical JSON");
        connection
            .execute_batch(concat!(
                "CREATE TABLE remediation_write_counts(kind TEXT PRIMARY KEY, n INTEGER NOT NULL);
             INSERT INTO remediation_write_counts VALUES ('projection', 0), ('drive', 0);
             CREATE TRIGGER remediation_projection_delete AFTER DELETE ON ",
                crate::db::import_table_names::import_projection_state_rows_table!(),
                "
               BEGIN UPDATE remediation_write_counts SET n=n+1 WHERE kind='projection'; END;
             CREATE TRIGGER remediation_projection_insert AFTER INSERT ON ",
                crate::db::import_table_names::import_projection_state_rows_table!(),
                "
               BEGIN UPDATE remediation_write_counts SET n=n+1 WHERE kind='projection'; END;
             CREATE TRIGGER remediation_projection_update AFTER UPDATE ON ",
                crate::db::import_table_names::import_projection_state_rows_table!(),
                "
               BEGIN UPDATE remediation_write_counts SET n=n+1 WHERE kind='projection'; END;
             CREATE TRIGGER remediation_drive_update AFTER UPDATE ON materialised_drives
               BEGIN UPDATE remediation_write_counts SET n=n+1 WHERE kind='drive'; END;
             CREATE TRIGGER remediation_drive_insert AFTER INSERT ON materialised_drives
               BEGIN UPDATE remediation_write_counts SET n=n+1 WHERE kind='drive'; END;
             CREATE TRIGGER remediation_drive_delete AFTER DELETE ON materialised_drives
               BEGIN UPDATE remediation_write_counts SET n=n+1 WHERE kind='drive'; END;
             PRAGMA wal_checkpoint(TRUNCATE);"
            ))
            .expect("bounded fixture write counters");
        let delta = imported_typed_delta(&store, &binding, &base);
        let started = std::time::Instant::now();
        store
            .finalize_import_generation_delta_successors_with_projection_state_and_materialisation(
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                3_000,
                std::slice::from_ref(&delta),
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
                Sha256Digest::of_bytes(b"sparse-write-budget"),
                &[],
                &state,
                &car,
                &drives,
            )
            .expect("sparse atomic finalization");
        let elapsed_us = started.elapsed().as_micros();
        let writes = |kind: &str| -> i64 {
            connection
                .query_row(
                    "SELECT n FROM remediation_write_counts WHERE kind=?1",
                    params![kind],
                    |row| row.get(0),
                )
                .expect("write count")
        };
        assert_eq!(
            writes("projection"),
            1,
            "unchanged authenticated rows are retained"
        );
        assert_eq!(
            writes("drive"),
            2,
            "one changed drive and one derived-only repair"
        );
        let repaired: Option<f64> = connection.query_row(
            "SELECT inside_temp_avg FROM materialised_drives WHERE vehicle_id=?1 AND drive_id=?2",
            params![vehicle.vehicle_id.to_string(), drives[1].id], |row| row.get(0),
        ).expect("repaired derived column");
        assert_eq!(repaired, drives[1].inside_temp_avg);
        for drive in &drives {
            let actual = store
                .materialised_drive_for_vehicle(vehicle.vehicle_id, drive.id)
                .expect("drive query")
                .expect("drive");
            assert_eq!(
                serde_json::to_value(actual).expect("actual"),
                serde_json::to_value(drive).expect("expected")
            );
        }
        let wal = PathBuf::from(format!("{}-wal", store.database_path().display()));
        let wal_bytes = fs::metadata(wal)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        println!(
            "{}",
            serde_json::json!({"check":"sparse_finalization_write_budget", "history_drives":count, "changed_drives":changed, "projection_writes":writes("projection"), "drive_writes":writes("drive"), "finalization_us":elapsed_us, "wal_bytes":wal_bytes, "scope":"synthetic small history; instrumentation included; full authentication/validation retained"})
        );
    }
}

// M03-T15: the import seam applied to drives and positions in the store
// (design M03-SEAM-R3, section 4). Every test publishes through the real
// import publication path; Hub rows and coverage are written as the
// collector and the window controller leave them.

const SEAM_T: i64 = 1_800_000_000_000;
const SEAM_MINUTE: i64 = 60_000;

/// A store with a published base of no drives.
fn seam_store() -> (
    tempfile::TempDir,
    HubStore,
    VehicleRecord,
    ProjectionBinding,
    ProjectionCar,
) {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, car, _) = direct_materialised_base(&store, &[]);
    (temporary, store, vehicle, binding, car)
}

fn seam_drive(id: i64, car_id: i64, start_ms: i64, end_ms: i64) -> ProjectionDrive {
    let mut drive = import_materialised_test_drive(id, car_id, 3.0);
    drive.start_date_ms = start_ms;
    drive.end_date_ms = end_ms;
    drive
}

fn seam_position(
    id: i64,
    car_id: i64,
    date_ms: i64,
) -> crate::source_db_projection::SourceDbPosition {
    serde_json::from_value(serde_json::json!({
        "id": id, "car_id": car_id, "drive_id": null, "date_ms": date_ms,
        "latitude": 47.5, "longitude": 19.0
    }))
    .expect("source position")
}

/// A handed-back window `[opened_ms, closed_ms)` in the store's coverage.
fn seam_window(store: &HubStore, seq: i64, opened_ms: i64, closed_ms: Option<i64>) {
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
             VALUES (?1, ?2, ?3, ?4, '[]', ?5)",
            params![
                seq,
                format!("grant-{seq}"),
                opened_ms,
                closed_ms,
                if closed_ms.is_some() { COVERAGE_SEALED_HANDBACK } else { COVERAGE_SEALED_OPEN }
            ],
        )
        .expect("coverage window");
}

/// A drive the Hub collected (or extended), as the collector stores it.
fn seam_hub_drive(store: &HubStore, vehicle: &VehicleRecord, drive: &ProjectionDrive) {
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                vehicle.vehicle_id.to_string(),
                drive.id,
                drive.car_id,
                serde_json::to_string(drive).expect("drive json")
            ],
        )
        .expect("Hub drive");
}

/// A position the Hub collected.
fn seam_hub_position(
    store: &HubStore,
    vehicle: &VehicleRecord,
    car_id: i64,
    id: i64,
    drive_id: Option<i64>,
    date_ms: i64,
) {
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                vehicle.vehicle_id.to_string(),
                id,
                drive_id,
                car_id,
                serde_json::json!({"id": id, "car_id": car_id, "drive_id": drive_id, "date_ms": date_ms})
                    .to_string()
            ],
        )
        .expect("Hub position");
}

/// Publishes one import successor of `drives` (ascending ids) and the open
/// session's standalone positions through the real publication path.
fn seam_publish(
    store: &HubStore,
    vehicle: &VehicleRecord,
    binding: &ProjectionBinding,
    car: &ProjectionCar,
    drives: &[ProjectionDrive],
    standalone_positions: Vec<crate::source_db_projection::SourceDbPosition>,
) -> Result<(), StoreError> {
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            3_000,
        )
        .expect("successor generation");
    store
        .stage_import_generation_session(
            run_id,
            &SourceDbOpenSession {
                car_id: binding.selected_car_id,
                standalone_positions,
                ..Default::default()
            },
        )
        .expect("successor session");
    let prior = store
        .source_db_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("prior state");
    let state = create_direct_import_projection_state(store, run_id, 64);
    let mut capture = crate::import_projection_state::SourceDbProjectionStateCapture::for_successor(
        state,
        Box::new(prior),
    );
    capture.record_car(car).expect("capture car");
    for drive in drives {
        capture.record_drive(drive).expect("capture drive");
    }
    capture.seal().expect("seal successor state");
    let state = capture.into_state();
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("lineage")
        .expect("published lineage");
    let delta = imported_typed_delta(store, binding, &lineage);
    store.finalize_import_generation_delta_successors_with_projection_state_and_materialisation(
        run_id,
        binding.account_id,
        vehicle.vehicle_id,
        binding.selected_car_id,
        3_000,
        std::slice::from_ref(&delta),
        &import_delta_test_cursor_key(),
        &import_delta_test_cursor(binding, delta.to_sequence),
        Sha256Digest::of_bytes(format!("seam-{run_id}").as_bytes()),
        &[],
        &state,
        car,
        drives,
    )
}

/// The stored drives: id, start and end.
fn seam_stored_drives(store: &HubStore, vehicle: &VehicleRecord) -> Vec<(i64, i64, i64)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT drive_id,
                    CAST(json_extract(drive_json, '$.start_date_ms') AS INTEGER),
                    CAST(json_extract(drive_json, '$.end_date_ms') AS INTEGER)
               FROM materialised_drives WHERE vehicle_id = ?1 ORDER BY drive_id",
        )
        .expect("drives");
    statement
        .query_map(params![vehicle.vehicle_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("drive rows")
        .collect::<Result<_, _>>()
        .expect("drive rows")
}

/// The stored positions: id, drive id (column and JSON agree) and instant.
fn seam_stored_positions(
    store: &HubStore,
    vehicle: &VehicleRecord,
) -> Vec<(i64, Option<i64>, i64)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT position_id, drive_id,
                    CAST(json_extract(position_json, '$.drive_id') AS INTEGER),
                    CAST(json_extract(position_json, '$.date_ms') AS INTEGER)
               FROM materialised_positions WHERE vehicle_id = ?1 ORDER BY position_id",
        )
        .expect("positions");
    statement
        .query_map(params![vehicle.vehicle_id.to_string()], |row| {
            let column: Option<i64> = row.get(1)?;
            let json: Option<i64> = row.get(2)?;
            assert_eq!(column, json, "the position's drive column and JSON agree");
            Ok((row.get(0)?, column, row.get(3)?))
        })
        .expect("position rows")
        .collect::<Result<_, _>>()
        .expect("position rows")
}

/// The drive and position mutations journalled for sync, in order.
fn seam_mutations(store: &HubStore, vehicle: &VehicleRecord) -> Vec<(String, i64, String)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT entity, entity_id, operation FROM sync_mutations
              WHERE vehicle_id = ?1 AND entity IN ('drive', 'position') ORDER BY revision",
        )
        .expect("mutations");
    statement
        .query_map(params![vehicle.vehicle_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("mutation rows")
        .collect::<Result<_, _>>()
        .expect("mutation rows")
}

/// The recorded drive decisions: loser and winner.
fn seam_links(store: &HubStore, vehicle: &VehicleRecord) -> Vec<(i64, i64)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT loser_id, winner_id FROM import_seam_links
              WHERE vehicle_id = ?1 AND entity = 'drive' ORDER BY loser_id",
        )
        .expect("links");
    statement
        .query_map(params![vehicle.vehicle_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("link rows")
        .collect::<Result<_, _>>()
        .expect("link rows")
}

fn tombstone(entity: &str, id: i64) -> (String, i64, String) {
    (entity.to_owned(), id, "tombstone".to_owned())
}

#[test]
fn import_seam_source_id_at_the_base_is_refused() {
    use crate::db::hub_ids::HUB_ID_BASE;
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let retained = import_materialised_test_drive(7, 10, 4.0);
    let (vehicle, binding, car, base) = direct_materialised_base(&store, std::slice::from_ref(&retained));
    let car_id = binding.selected_car_id;
    let below = import_materialised_test_drive(HUB_ID_BASE - 1, car_id, 5.0);
    let at_base = import_materialised_test_drive(HUB_ID_BASE, car_id, 6.0);

    let refused = seam_publish(
        &store,
        &vehicle,
        &binding,
        &car,
        &[retained.clone(), below.clone(), at_base],
        Vec::new(),
    );
    assert!(
        matches!(refused, Err(StoreError::InvalidImportGeneration)),
        "a source drive id at the base is refused: {refused:?}"
    );
    // Nothing of the refused publication is visible.
    assert_eq!(
        seam_stored_drives(&store, &vehicle)
            .iter()
            .map(|row| row.0)
            .collect::<Vec<_>>(),
        vec![retained.id]
    );
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("lineage")
        .expect("base lineage remains");
    assert_eq!(lineage.head_sequence, base.head_sequence);

    // A standalone position at the base is refused as well.
    let refused = seam_publish(
        &store,
        &vehicle,
        &binding,
        &car,
        std::slice::from_ref(&retained),
        vec![seam_position(HUB_ID_BASE, car_id, SEAM_T)],
    );
    assert!(
        matches!(refused, Err(StoreError::InvalidImportGeneration)),
        "a source position id at the base is refused: {refused:?}"
    );
    assert!(seam_stored_positions(&store, &vehicle).is_empty());

    // The last id below the base is the source's and is admitted.
    seam_publish(
        &store,
        &vehicle,
        &binding,
        &car,
        &[retained.clone(), below.clone()],
        Vec::new(),
    )
    .expect("the last source id below the base");
    assert_eq!(
        seam_stored_drives(&store, &vehicle)
            .iter()
            .map(|row| row.0)
            .collect::<Vec<_>>(),
        vec![retained.id, below.id]
    );
}

#[test]
fn import_seam_replay_after_a_clash_keeps_every_drive() {
    use crate::db::hub_ids::HUB_ID_BASE;
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + 60 * SEAM_MINUTE));
    // The Hub collected two drives in its window.
    let (first, second) = (HUB_ID_BASE + 1, HUB_ID_BASE + 2);
    seam_hub_drive(
        &store,
        &vehicle,
        &seam_drive(
            first,
            car_id,
            SEAM_T + 10 * SEAM_MINUTE,
            SEAM_T + 20 * SEAM_MINUTE,
        ),
    );
    seam_hub_drive(
        &store,
        &vehicle,
        &seam_drive(
            second,
            car_id,
            SEAM_T + 30 * SEAM_MINUTE,
            SEAM_T + 40 * SEAM_MINUTE,
        ),
    );
    for (offset, minute) in [(1, 10), (2, 15), (3, 20), (4, 30), (5, 40)] {
        let drive = if minute < 30 { first } else { second };
        seam_hub_position(
            &store,
            &vehicle,
            car_id,
            HUB_ID_BASE + offset,
            Some(drive),
            SEAM_T + minute * SEAM_MINUTE,
        );
    }
    // The source holds the first trip too (it clashes in time), and its own
    // drives before and after the window.
    let source = vec![
        seam_drive(
            40,
            car_id,
            SEAM_T - 30 * SEAM_MINUTE,
            SEAM_T - 20 * SEAM_MINUTE,
        ),
        seam_drive(
            41,
            car_id,
            SEAM_T + 10 * SEAM_MINUTE + 5_000,
            SEAM_T + 20 * SEAM_MINUTE - 5_000,
        ),
        seam_drive(
            42,
            car_id,
            SEAM_T + 90 * SEAM_MINUTE,
            SEAM_T + 100 * SEAM_MINUTE,
        ),
    ];
    let positions_before = seam_stored_positions(&store, &vehicle);

    seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect("first publication");
    let drive_ids = |store: &HubStore| -> Vec<i64> {
        seam_stored_drives(store, &vehicle)
            .iter()
            .map(|row| row.0)
            .collect()
    };
    assert_eq!(
        drive_ids(&store),
        vec![40, 42, first, second],
        "the Hub's trip is stored once"
    );
    assert_eq!(seam_links(&store, &vehicle), vec![(41, first)]);
    assert_eq!(seam_stored_positions(&store, &vehicle), positions_before);
    assert!(
        seam_mutations(&store, &vehicle).is_empty(),
        "nothing of the Hub's is removed"
    );
    let after_first = (
        seam_stored_drives(&store, &vehicle),
        seam_links(&store, &vehicle),
    );

    // The same source published again changes nothing: every drive stays.
    seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect("replay");
    assert_eq!(
        (
            seam_stored_drives(&store, &vehicle),
            seam_links(&store, &vehicle)
        ),
        after_first
    );
    assert_eq!(seam_stored_positions(&store, &vehicle), positions_before);
    assert!(
        seam_mutations(&store, &vehicle).is_empty(),
        "a replay journals nothing"
    );
}

#[test]
fn import_seam_drive_across_a_window_edge_is_stored_once() {
    use crate::db::hub_ids::HUB_ID_BASE;
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + 60 * SEAM_MINUTE));
    // The Hub began recording at window open, in the middle of a drive the
    // source recorded whole across the edge.
    let hub_drive = HUB_ID_BASE + 1;
    seam_hub_drive(
        &store,
        &vehicle,
        &seam_drive(hub_drive, car_id, SEAM_T, SEAM_T + 5 * SEAM_MINUTE),
    );
    for (offset, minute) in [(1, 1), (2, 3), (3, 5)] {
        seam_hub_position(
            &store,
            &vehicle,
            car_id,
            HUB_ID_BASE + offset,
            Some(hub_drive),
            SEAM_T + minute * SEAM_MINUTE,
        );
    }
    let crossing = seam_drive(
        50,
        car_id,
        SEAM_T - 10 * SEAM_MINUTE,
        SEAM_T + 5 * SEAM_MINUTE,
    );
    let source = vec![crossing.clone()];

    seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect("publication");
    assert_eq!(
        seam_stored_drives(&store, &vehicle),
        vec![(50, crossing.start_date_ms, crossing.end_date_ms)],
        "the crossing drive is stored once, as the source's"
    );
    // The Hub's positions inside its coverage stay, now under the winner.
    let positions: Vec<(i64, Option<i64>)> = seam_stored_positions(&store, &vehicle)
        .iter()
        .map(|row| (row.0, row.1))
        .collect();
    assert_eq!(
        positions,
        vec![
            (HUB_ID_BASE + 1, Some(50)),
            (HUB_ID_BASE + 2, Some(50)),
            (HUB_ID_BASE + 3, Some(50))
        ]
    );
    assert_eq!(seam_links(&store, &vehicle), vec![(hub_drive, 50)]);
    let journal = seam_mutations(&store, &vehicle);
    assert_eq!(
        journal
            .iter()
            .filter(|row| row.2 == "tombstone")
            .collect::<Vec<_>>(),
        vec![&tombstone("drive", hub_drive)],
        "the Hub's half goes with one tombstone and no position goes"
    );
    assert_eq!(
        journal.iter().filter(|row| row.0 == "position").count(),
        3,
        "each moved position is journalled"
    );

    // A replay finds the superseded row and its link and repeats itself.
    seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect("replay");
    assert_eq!(seam_stored_drives(&store, &vehicle).len(), 1);
    assert_eq!(seam_links(&store, &vehicle), vec![(hub_drive, 50)]);
    assert_eq!(
        seam_mutations(&store, &vehicle),
        journal,
        "a replay journals nothing"
    );
}

#[test]
fn import_seam_inherited_drive_closed_earlier_by_source_keeps_hub_positions() {
    use crate::db::hub_ids::HUB_ID_BASE;
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + 120 * SEAM_MINUTE));
    // The import before the window inherited the open drive 60; the Hub went
    // on collecting it from window open and closed it an hour later.
    let inherited = seam_drive(
        60,
        car_id,
        SEAM_T - 20 * SEAM_MINUTE,
        SEAM_T + 60 * SEAM_MINUTE,
    );
    seam_hub_drive(&store, &vehicle, &inherited);
    let hub_positions: Vec<i64> = (1..=59).map(|minute| HUB_ID_BASE + minute).collect();
    for (index, id) in hub_positions.iter().enumerate() {
        seam_hub_position(
            &store,
            &vehicle,
            car_id,
            *id,
            Some(60),
            SEAM_T + (index as i64 + 1) * SEAM_MINUTE,
        );
    }
    // The source closed its copy just after the window opened.
    let source = vec![
        seam_drive(
            59,
            car_id,
            SEAM_T - 90 * SEAM_MINUTE,
            SEAM_T - 80 * SEAM_MINUTE,
        ),
        seam_drive(60, car_id, SEAM_T - 20 * SEAM_MINUTE, SEAM_T + SEAM_MINUTE),
    ];

    for round in ["publication", "replay"] {
        seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect(round);
        assert_eq!(
            seam_stored_drives(&store, &vehicle),
            vec![
                (59, source[0].start_date_ms, source[0].end_date_ms),
                (60, inherited.start_date_ms, inherited.end_date_ms)
            ],
            "{round}: the stored row the Hub saw more of stays"
        );
        let kept: Vec<(i64, Option<i64>)> = seam_stored_positions(&store, &vehicle)
            .iter()
            .map(|row| (row.0, row.1))
            .collect();
        assert_eq!(
            kept,
            hub_positions
                .iter()
                .map(|id| (*id, Some(60)))
                .collect::<Vec<_>>(),
            "{round}: every Hub position of the inherited drive stays"
        );
        assert!(
            seam_links(&store, &vehicle).is_empty(),
            "{round}: one id, no loser"
        );
        assert!(
            seam_mutations(&store, &vehicle).is_empty(),
            "{round}: nothing removed"
        );
    }
}

#[test]
fn import_seam_standalone_positions_inside_coverage_are_the_hubs() {
    use crate::db::hub_ids::HUB_ID_BASE;
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + 60 * SEAM_MINUTE));
    let hub_position = HUB_ID_BASE + 5;
    seam_hub_position(
        &store,
        &vehicle,
        car_id,
        hub_position,
        None,
        SEAM_T + 10 * SEAM_MINUTE,
    );
    let standalone = vec![
        seam_position(70, car_id, SEAM_T + 10 * SEAM_MINUTE),
        seam_position(71, car_id, SEAM_T - 10 * SEAM_MINUTE),
        seam_position(72, car_id, SEAM_T + 60 * SEAM_MINUTE),
        seam_position(73, car_id, SEAM_T),
    ];

    for round in ["publication", "replay"] {
        seam_publish(&store, &vehicle, &binding, &car, &[], standalone.clone()).expect(round);
        assert_eq!(
            seam_stored_positions(&store, &vehicle),
            vec![
                (71, None, SEAM_T - 10 * SEAM_MINUTE),
                (72, None, SEAM_T + 60 * SEAM_MINUTE),
                (hub_position, None, SEAM_T + 10 * SEAM_MINUTE),
            ],
            "{round}: inside coverage (open edge included) the Hub's; at and after the close the source's"
        );
        assert!(
            seam_mutations(&store, &vehicle).is_empty(),
            "{round}: the Hub's stays"
        );
    }
}

#[test]
fn import_seam_source_time_correction_after_a_skip() {
    use crate::db::hub_ids::HUB_ID_BASE;
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + 60 * SEAM_MINUTE));
    let hub_drive = HUB_ID_BASE + 1;
    seam_hub_drive(
        &store,
        &vehicle,
        &seam_drive(
            hub_drive,
            car_id,
            SEAM_T + 10 * SEAM_MINUTE,
            SEAM_T + 20 * SEAM_MINUTE,
        ),
    );
    let before = seam_drive(
        40,
        car_id,
        SEAM_T - 30 * SEAM_MINUTE,
        SEAM_T - 20 * SEAM_MINUTE,
    );
    let clash = seam_drive(
        41,
        car_id,
        SEAM_T + 10 * SEAM_MINUTE,
        SEAM_T + 20 * SEAM_MINUTE,
    );
    seam_publish(
        &store,
        &vehicle,
        &binding,
        &car,
        &[before.clone(), clash],
        Vec::new(),
    )
    .expect("first publication");
    assert_eq!(seam_links(&store, &vehicle), vec![(41, hub_drive)]);

    // The source corrects drive 41's times so they no longer meet the Hub's
    // drive. The recorded decision holds while its winner exists.
    let corrected = seam_drive(
        41,
        car_id,
        SEAM_T + 45 * SEAM_MINUTE,
        SEAM_T + 50 * SEAM_MINUTE,
    );
    seam_publish(
        &store,
        &vehicle,
        &binding,
        &car,
        &[before.clone(), corrected],
        Vec::new(),
    )
    .expect("publication after the correction");
    assert_eq!(
        seam_stored_drives(&store, &vehicle)
            .iter()
            .map(|row| row.0)
            .collect::<Vec<_>>(),
        vec![40, hub_drive],
        "the skipped drive stays skipped"
    );
    assert_eq!(seam_links(&store, &vehicle), vec![(41, hub_drive)]);

    // The source deletes drive 41: its link goes, and nothing else changes.
    seam_publish(&store, &vehicle, &binding, &car, &[before], Vec::new())
        .expect("publication after the deletion");
    assert!(seam_links(&store, &vehicle).is_empty());
    assert_eq!(
        seam_stored_drives(&store, &vehicle)
            .iter()
            .map(|row| row.0)
            .collect::<Vec<_>>(),
        vec![40, hub_drive]
    );
    assert!(seam_mutations(&store, &vehicle).is_empty());
}

#[test]
fn import_seam_publishing_between_two_windows() {
    use crate::db::hub_ids::HUB_ID_BASE;
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    let hour = 60 * SEAM_MINUTE;
    // The first window, handed back, with the Hub's drive and positions.
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + hour));
    let first = HUB_ID_BASE + 1;
    seam_hub_drive(
        &store,
        &vehicle,
        &seam_drive(
            first,
            car_id,
            SEAM_T + 10 * SEAM_MINUTE,
            SEAM_T + 20 * SEAM_MINUTE,
        ),
    );
    seam_hub_position(
        &store,
        &vehicle,
        car_id,
        HUB_ID_BASE + 1,
        Some(first),
        SEAM_T + 15 * SEAM_MINUTE,
    );
    let earlier = seam_drive(80, car_id, SEAM_T - hour, SEAM_T - hour + 10 * SEAM_MINUTE);
    let first_clash = seam_drive(
        81,
        car_id,
        SEAM_T + 10 * SEAM_MINUTE,
        SEAM_T + 20 * SEAM_MINUTE,
    );
    seam_publish(
        &store,
        &vehicle,
        &binding,
        &car,
        &[earlier.clone(), first_clash.clone()],
        Vec::new(),
    )
    .expect("publication after the first window");

    // The second window opens; while it is open there is no cutoff, so a
    // publication refuses and changes nothing.
    seam_window(&store, 2, SEAM_T + 3 * hour, None);
    let between = seam_drive(82, car_id, SEAM_T + 90 * SEAM_MINUTE, SEAM_T + 2 * hour);
    let second_clash = seam_drive(
        83,
        car_id,
        SEAM_T + 3 * hour + 10 * SEAM_MINUTE,
        SEAM_T + 3 * hour + 20 * SEAM_MINUTE,
    );
    let source = vec![earlier, first_clash, between, second_clash];
    let drives_before = seam_stored_drives(&store, &vehicle);
    assert!(matches!(
        seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()),
        Err(StoreError::ImportGenerationConflict)
    ));
    assert_eq!(seam_stored_drives(&store, &vehicle), drives_before);

    // Handed back, with the Hub's second drive.
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE collection_coverage SET closed_at_ms = ?1, sealed = ?2 WHERE seq = 2",
            params![SEAM_T + 4 * hour, COVERAGE_SEALED_HANDBACK],
        )
        .expect("handback");
    let second = HUB_ID_BASE + 2;
    seam_hub_drive(
        &store,
        &vehicle,
        &seam_drive(
            second,
            car_id,
            SEAM_T + 3 * hour + 10 * SEAM_MINUTE,
            SEAM_T + 3 * hour + 20 * SEAM_MINUTE,
        ),
    );
    seam_hub_position(
        &store,
        &vehicle,
        car_id,
        HUB_ID_BASE + 2,
        Some(second),
        SEAM_T + 3 * hour + 15 * SEAM_MINUTE,
    );
    for round in ["publication", "replay"] {
        seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect(round);
        assert_eq!(
            seam_stored_drives(&store, &vehicle)
                .iter()
                .map(|row| row.0)
                .collect::<Vec<_>>(),
            vec![80, 82, first, second],
            "{round}: each window's drive is the Hub's, the drive between them the source's"
        );
        assert_eq!(
            seam_links(&store, &vehicle),
            vec![(81, first), (83, second)]
        );
        assert_eq!(seam_stored_positions(&store, &vehicle).len(), 2);
        assert!(
            seam_mutations(&store, &vehicle).is_empty(),
            "{round}: nothing removed"
        );
    }
}

#[test]
fn import_seam_restore_then_import() {
    use crate::db::hub_ids::HUB_ID_BASE;
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    // A backup taken inside a window: the store shows the window open and
    // the Hub's rows up to its last moment.
    seam_window(&store, 1, SEAM_T, None);
    let hub_drive = HUB_ID_BASE + 1;
    let last_ms = SEAM_T + 20 * SEAM_MINUTE;
    seam_hub_drive(
        &store,
        &vehicle,
        &seam_drive(hub_drive, car_id, SEAM_T + 10 * SEAM_MINUTE, last_ms),
    );
    seam_hub_position(
        &store,
        &vehicle,
        car_id,
        HUB_ID_BASE + 1,
        Some(hub_drive),
        SEAM_T + 10 * SEAM_MINUTE,
    );
    // The Hub's last row is a position at exactly the drive's end.
    seam_hub_position(
        &store,
        &vehicle,
        car_id,
        HUB_ID_BASE + 2,
        Some(hub_drive),
        last_ms,
    );
    // The restored host has no window open, so it seals the window per
    // vehicle at its last Hub row.
    let sealed = crate::db::seal_restored_open_windows(&store.open().expect("catalogue"), None)
        .expect("seal the restored window");
    assert_eq!(sealed, 1);

    // The source resumed after the restore; it also holds the Hub's trip and
    // a position at the Hub's last instant.
    let resumed = seam_drive(
        93,
        car_id,
        SEAM_T + 30 * SEAM_MINUTE,
        SEAM_T + 40 * SEAM_MINUTE,
    );
    let source = vec![
        seam_drive(
            91,
            car_id,
            SEAM_T + 10 * SEAM_MINUTE + 5_000,
            last_ms - 5_000,
        ),
        resumed,
    ];
    let standalone = vec![
        seam_position(92, car_id, last_ms),
        seam_position(94, car_id, last_ms + 1),
    ];
    for round in ["publication", "replay"] {
        seam_publish(
            &store,
            &vehicle,
            &binding,
            &car,
            &source,
            standalone.clone(),
        )
        .expect(round);
        assert_eq!(
            seam_stored_drives(&store, &vehicle)
                .iter()
                .map(|row| row.0)
                .collect::<Vec<_>>(),
            vec![93, hub_drive],
            "{round}: the Hub's trip stands inside the sealed coverage"
        );
        assert_eq!(seam_links(&store, &vehicle), vec![(91, hub_drive)]);
        let positions: Vec<i64> = seam_stored_positions(&store, &vehicle)
            .iter()
            .map(|row| row.0)
            .collect();
        assert_eq!(
            positions,
            vec![94, HUB_ID_BASE + 1, HUB_ID_BASE + 2],
            "{round}: the Hub's position at its last instant stays; the source's at that instant does not"
        );
        assert!(
            seam_mutations(&store, &vehicle).is_empty(),
            "{round}: nothing removed"
        );
    }
}

#[test]
fn direct_import_successor_batch_is_atomic_and_advances_every_durable_head() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = import_delta_test_car(binding.selected_car_id);
    let inventory = SourceDbImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let base_state = test_projection_state(temporary.path(), &car);
    store
        .finalize_source_db_import_snapshot_with_projection_state(
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
            &SourceDbOpenSession {
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
            false,
        )
        .expect("atomically publish the complete direct-import batch");

    let lineage = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("lineage")
        .expect("lineage exists");
    assert_eq!(lineage.deltas, vec![first, second.clone()]);
    assert_eq!(lineage.head_sequence, second.to_sequence);
    let state = store
        .source_db_import_projection_state_lookup(
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
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = import_delta_test_car(binding.selected_car_id);
    let inventory = SourceDbImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let base_state = test_projection_state(temporary.path(), &car);
    store
        .finalize_source_db_import_snapshot_with_projection_state(
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
            &SourceDbOpenSession {
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
            false,
        ),
        Err(StoreError::SourceDbProjectionState(
            SourceDbProjectionStateError::TransferRowContractMismatch
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
        .source_db_import_projection_state_lookup(
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
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = import_delta_test_car(binding.selected_car_id);
    let inventory = SourceDbImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let base_state = test_projection_state(temporary.path(), &car);
    store
        .finalize_source_db_import_snapshot_with_projection_state(
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
            &SourceDbOpenSession {
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
            false,
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
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let inventory = SourceDbImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    store
        .finalize_source_db_import_snapshot(
            &manifest,
            Sha256Digest::of_bytes(b"legacy-inventory-only"),
            &[],
            &binding,
            &inventory,
        )
        .expect("legacy inventory base");
    assert!(matches!(
        store.source_db_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        ),
        Err(StoreError::SourceDbImportProjectionStateMissing(id)) if id == vehicle.vehicle_id
    ));
}

fn legacy_direct_bridge_fixture(
    store: &HubStore,
    legacy_fingerprint: Sha256Digest,
) -> (VehicleRecord, ProjectionBinding, SyncManifest) {
    let (vehicle, binding, manifest) = v2_base_manifest(store);
    let inventory = SourceDbImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: vec![ProjectionTombstone {
            entity: ProjectionDeltaEntity::Position,
            id: 10,
            car_id: binding.selected_car_id,
        }],
    };
    store
        .finalize_source_db_import_snapshot(
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
            &SourceDbOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("bridge staging session");
    run_id
}

#[test]
fn legacy_direct_bridge_attaches_state_without_pack_delta_or_sequence_and_logical_rerun_skips() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
    let logical_fingerprint = Sha256Digest::of_bytes(b"logical-direct-projection");
    let (vehicle, binding, manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
    let drive = import_materialised_test_drive(7, binding.selected_car_id, 4.0);
    store
        .open()
        .expect("legacy catalogue")
        .execute(
            concat!(
                "INSERT INTO ",
                crate::db::import_table_names::import_projection_rows_table!(),
                "(vehicle_id, entity, entity_id)
             VALUES (?1, 'drive', ?2)"
            ),
            params![vehicle.vehicle_id.to_string(), drive.id],
        )
        .expect("seed legacy imported drive ownership");
    assert!(
        store
            .legacy_source_db_direct_bridge_is_eligible(
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
    let car = import_delta_test_car(binding.selected_car_id);
    let state = create_direct_import_projection_state(&store, run_id, 10);
    let mut capture =
        crate::import_projection_state::SourceDbProjectionStateCapture::for_initial_base(state);
    capture.record_car(&car).expect("capture bridged car");
    capture.record_drive(&drive).expect("capture bridged drive");
    capture
        .record(
            SourceDbProjectionStateEntity::Position,
            10,
            binding.selected_car_id,
            &serde_json::json!({"id": 10, "entity": "position"}),
        )
        .expect("capture bridged position");
    capture.seal().expect("seal bridge state");
    let state = capture.into_state();

    let bridged = store
        .bridge_legacy_source_db_direct_import_with_materialisation(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            legacy_fingerprint,
            logical_fingerprint,
            &state,
            &car,
            std::slice::from_ref(&drive),
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
            .source_db_import_projection_state_exists(vehicle.vehicle_id)
            .expect("state head exists")
    );
    let state_lookup = store
        .source_db_import_projection_state_lookup(
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
            .legacy_source_db_direct_bridge_is_eligible(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("bridge is one-time"),
        "the persisted state/marker prevents a second bridge"
    );
    assert_eq!(
        store
            .materialised_car_for_vehicle(vehicle.vehicle_id)
            .expect("bridged materialised car"),
        Some(car)
    );
    assert_eq!(
        store
            .materialised_drive_for_vehicle(vehicle.vehicle_id, drive.id)
            .expect("bridged materialised drive"),
        Some(drive)
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
        (
            crate::db::import_table_names::import_projection_state_bridges_table!(),
            1,
        ),
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
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
    let (vehicle, binding, manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
    let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
    let state = direct_projection_state_with_digest_rows(
        &store,
        run_id,
        binding.selected_car_id,
        &[(SourceDbProjectionStateEntity::Position, 10)],
    );
    let changed_physical = Sha256Digest::of_bytes(b"changed-direct-physical");

    assert!(matches!(
        store.bridge_legacy_source_db_direct_import(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            changed_physical,
            Sha256Digest::of_bytes(b"logical-direct-projection"),
            &state,
        ),
        Err(StoreError::SourceDbLegacyDirectRebaseRequired(id)) if id == vehicle.vehicle_id
    ));
    let connection = store.open().expect("catalogue after rejection");
    for (table, expected) in [
        (
            crate::db::import_table_names::import_projection_state_heads_table!(),
            0,
        ),
        (
            crate::db::import_table_names::import_projection_state_rows_table!(),
            0,
        ),
        (
            crate::db::import_table_names::import_projection_state_bridges_table!(),
            0,
        ),
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
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
    let (vehicle, binding, _manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
    let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
    let mismatched_state = direct_projection_state_with_digest_rows(
        &store,
        run_id,
        binding.selected_car_id,
        &[
            (SourceDbProjectionStateEntity::Position, 10),
            (SourceDbProjectionStateEntity::Position, 11),
        ],
    );

    assert!(matches!(
        store.bridge_legacy_source_db_direct_import(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            legacy_fingerprint,
            Sha256Digest::of_bytes(b"logical-direct-projection"),
            &mismatched_state,
        ),
        Err(StoreError::SourceDbLegacyDirectRebaseRequired(id)) if id == vehicle.vehicle_id
    ));
    let connection = store.open().expect("catalogue after rollback");
    for (table, expected) in [
        (
            crate::db::import_table_names::import_projection_state_heads_table!(),
            0,
        ),
        (
            crate::db::import_table_names::import_projection_state_rows_table!(),
            0,
        ),
        (
            crate::db::import_table_names::import_projection_state_bridges_table!(),
            0,
        ),
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
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
    let (vehicle, binding, _manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
    let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
    let mut carless_state = create_direct_import_projection_state(&store, run_id, 10);
    carless_state
        .record(
            SourceDbProjectionStateEntity::Position,
            10,
            binding.selected_car_id,
            &serde_json::json!({"id": 10}),
        )
        .expect("record matching non-car row");
    carless_state.seal().expect("seal carless state");

    assert!(matches!(
        store.bridge_legacy_source_db_direct_import(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            legacy_fingerprint,
            Sha256Digest::of_bytes(b"logical-direct-projection"),
            &carless_state,
        ),
        Err(StoreError::SourceDbLegacyDirectRebaseRequired(id)) if id == vehicle.vehicle_id
    ));
    let connection = store.open().expect("catalogue after rejection");
    for table in [
        crate::db::import_table_names::import_projection_state_heads_table!(),
        crate::db::import_table_names::import_projection_state_rows_table!(),
        crate::db::import_table_names::import_projection_state_bridges_table!(),
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
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let inventory = SourceDbImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let mut unsealed = SourceDbProjectionState::create(
        temporary.path(),
        crate::import_projection_state::SourceDbProjectionStateLimits {
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
        store.finalize_source_db_import_snapshot_with_projection_state(
            &manifest,
            Sha256Digest::of_bytes(b"unsealed-projection-state"),
            &[],
            &binding,
            &inventory,
            &unsealed,
        ),
        Err(StoreError::SourceDbProjectionState(
            SourceDbProjectionStateError::StateNotSealed
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
        crate::db::import_table_names::import_projection_state_heads_table!(),
        crate::db::import_table_names::import_projection_state_rows_table!(),
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
    let temporary = crate::private_tempdir().expect("temporary store");
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
            &SourceDbOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("staging session");
    let rows = [
        (SourceDbProjectionStateEntity::Drive, 11),
        (SourceDbProjectionStateEntity::Position, 12),
        (SourceDbProjectionStateEntity::Charge, 13),
        (SourceDbProjectionStateEntity::ChargeSample, 14),
        (SourceDbProjectionStateEntity::State, 15),
    ];
    let state =
        direct_projection_state_with_digest_rows(&store, run_id, binding.selected_car_id, &rows);
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
            false,
        )
        .expect("set-based direct base finalization");

    let mut lookup = store
        .source_db_import_projection_state_lookup(
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
        .source_db_import_projection_inventory(
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
    for table in [
        crate::db::import_table_names::import_projection_heads_table!(),
        crate::db::import_table_names::import_projection_rows_table!(),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("direct import leaves no duplicate legacy inventory");
        assert_eq!(count, 0, "direct import does not duplicate {table}");
    }
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
    let temporary = crate::private_tempdir().expect("temporary store");
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
            &SourceDbOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("staging session");
    let mut state = create_direct_import_projection_state(&store, run_id, 10);
    state
        .record(
            SourceDbProjectionStateEntity::Position,
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
            false,
        ),
        Err(StoreError::SourceDbProjectionState(
            SourceDbProjectionStateError::TransferCarContractMismatch
        ))
    ));
    let connection = store.open().expect("catalogue after rejection");
    for table in [
        "sync_bases",
        "sync_heads",
        crate::db::import_table_names::import_projection_state_heads_table!(),
        crate::db::import_table_names::import_projection_state_rows_table!(),
        crate::db::import_table_names::import_projection_heads_table!(),
        crate::db::import_table_names::import_projection_rows_table!(),
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
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, _) = v2_base_manifest(&store);
    let state = projection_state_with_digest_rows(
        temporary.path(),
        binding.selected_car_id,
        &[(SourceDbProjectionStateEntity::Position, 12)],
    );
    let transfer = state
        .sealed_transfer(binding.selected_car_id)
        .expect("sealed transfer descriptor");
    let replacement = projection_state_with_digest_rows(
        temporary.path(),
        binding.selected_car_id,
        &[(SourceDbProjectionStateEntity::Position, 99)],
    );
    std::fs::rename(replacement.path_for_test(), transfer.path())
        .expect("replace descriptor path with same-shape foreign spool");

    let connection = store.open().expect("catalogue connection");
    assert!(matches!(
        attach_source_db_projection_state_transfer(&connection, &transfer),
        Err(StoreError::SourceDbProjectionState(
            SourceDbProjectionStateError::TransferDigestMismatch
        ))
    ));
    let state_heads: i64 = connection
        .query_row(
            concat!(
                "SELECT COUNT(*) FROM ",
                crate::db::import_table_names::import_projection_state_heads_table!(),
                " WHERE vehicle_id = ?1"
            ),
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("catalogue remains untouched");
    assert_eq!(state_heads, 0);
}

#[test]
fn sealed_transfer_attachment_is_read_only() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (_, binding, _) = v2_base_manifest(&store);
    let state = projection_state_with_digest_rows(
        temporary.path(),
        binding.selected_car_id,
        &[(SourceDbProjectionStateEntity::Position, 12)],
    );
    let transfer = state
        .sealed_transfer(binding.selected_car_id)
        .expect("sealed transfer descriptor");
    let connection = store.open().expect("catalogue connection");
    attach_source_db_projection_state_transfer(&connection, &transfer)
        .expect("attach sealed spool read-only");
    assert!(
        connection
            .execute(
                concat!(
                    "DELETE FROM ",
                    crate::db::import_table_names::import_state_spool_alias!(),
                    ".current_rows"
                ),
                [],
            )
            .is_err(),
        "SQLite mode=ro attachment must reject source mutation"
    );
    detach_source_db_projection_state_transfer(&store, &connection)
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
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let wrong_car_id = binding.selected_car_id + 1;
    let run_id = store
        .begin_import_generation(binding.account_id, vehicle.vehicle_id, wrong_car_id, 2_000)
        .expect("staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &SourceDbOpenSession {
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
            false,
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
    let temporary = crate::private_tempdir().expect("temporary store");
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
fn retained_store_recovers_v2_binding_from_immutable_base_not_mutable_source() {
    let temporary = crate::private_tempdir().expect("temporary store");
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
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("recover immutable binding");
    assert_eq!(
        upgraded
            .v2_projection_binding(vehicle.vehicle_id)
            .expect("recovered V2 binding"),
        binding,
        "upgrade must use the stored base identity and packed source car"
    );
    // The decoded copy of the base pack that the recovery inspected is transient: gone,
    // and counted (create, write, sync, removal) at the helper it goes through (B2 fix
    // round, P3b fix 5, R-P3bF5-3).
    let inspected: Vec<_> = crate::platform::file_attempts::entries_under(upgraded.packs_dir())
        .into_iter()
        .filter(|(path, _)| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with(".legacy-binding-inspection-")
            })
        })
        .collect();
    assert_eq!(inspected.len(), 1, "{inspected:?}");
    assert_eq!(inspected[0].1, 4, "{inspected:?}");
    assert!(!inspected[0].0.exists(), "the inspection file is removed");
    assert!(
        upgraded
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("upgraded lineage lookup")
            .is_some()
    );
}

#[test]
fn legacy_v2_binding_recovery_fails_closed_on_manifest_pack_identity_conflict() {
    let temporary = crate::private_tempdir().expect("temporary store");
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
