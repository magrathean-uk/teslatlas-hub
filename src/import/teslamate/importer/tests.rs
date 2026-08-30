// SPDX-License-Identifier: AGPL-3.0-only

use std::{error::Error, fs, fs::File, path::Path};

use super::*;
use crate::{
    credentials::TeslaMatePostgresPassword,
    db::{HubStore, ObservationInput, SourceDescriptor, StoreError, VehicleDescriptor},
    hub_pack::ProjectionPackOwnership,
    protocol::{CursorKey, ProtocolLimits},
    teslamate::ReadOnlySource,
    teslamate_fragments::TeslaMateFragmentLimits,
    teslamate_projection::{
        TeslaMateCar, TeslaMateGeofence, TeslaMateHistory, TeslaMatePosition, TeslaMateUpdate,
    },
    teslamate_projection_state::TeslaMateProjectionState,
    teslamate_stage::{TeslaMateStage, TeslaMateStageLimits, TeslaMateStageTable},
};

#[derive(Debug)]
struct EmptyPriorProjectionState;

impl crate::teslamate_projection_state::PriorProjectionStateLookup for EmptyPriorProjectionState {
    fn digest(
        &mut self,
        _entity: TeslaMateProjectionStateEntity,
        _id: i64,
    ) -> Result<Option<Sha256Digest>, Box<dyn Error + Send + Sync>> {
        Ok(None)
    }

    fn page_after(
        &mut self,
        _after: Option<crate::teslamate_projection_state::TeslaMateProjectionStateCursor>,
        _limit: u32,
    ) -> Result<
        crate::teslamate_projection_state::TeslaMateProjectionStateDigestPage,
        Box<dyn Error + Send + Sync>,
    > {
        Ok(
            crate::teslamate_projection_state::TeslaMateProjectionStateDigestPage {
                rows: Vec::new(),
                next_after: None,
            },
        )
    }
}

fn history() -> TeslaMateHistory {
    TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: 88,
            vid: Some(99),
            vin: Some("5YJTESTVIN1234567".into()),
            name: Some("Road car".into()),
            model: Some("Model 3".into()),
            trim_badging: Some("74d".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            efficiency_wh_per_km: Some(0.145),
            settings: Default::default(),
        }],
        drives: vec![],
        positions: vec![],
        charging_processes: vec![],
        charges: vec![],
        addresses: vec![],
        geofences: vec![],
        states: vec![],
        updates: vec![],
    }
}

fn identity_registry_image(store: &HubStore) -> [String; 4] {
    let connection = store.open().expect("identity registry");
    [
        connection
            .query_row(
                "SELECT COALESCE(json_group_array(json_array(
                        source_id, source_kind, generation, created_at_ms)), '[]')
                       FROM (SELECT * FROM sources ORDER BY source_id)",
                [],
                |row| row.get(0),
            )
            .expect("sources image"),
        connection
            .query_row(
                "SELECT COALESCE(json_group_array(json_array(
                        source_id, source_kind, source_key)), '[]')
                       FROM (SELECT * FROM source_identities ORDER BY source_id)",
                [],
                |row| row.get(0),
            )
            .expect("source identities image"),
        connection
            .query_row(
                "SELECT COALESCE(json_group_array(json_array(
                        vehicle_id, source_id, source_vehicle_key, vin, display_name,
                        created_at_ms, last_seen_at_ms)), '[]')
                       FROM (SELECT * FROM vehicles ORDER BY vehicle_id)",
                [],
                |row| row.get(0),
            )
            .expect("vehicles image"),
        connection
            .query_row(
                "SELECT COALESCE(json_group_array(json_array(
                        alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)), '[]')
                       FROM (SELECT * FROM vehicle_identity_aliases
                             ORDER BY alias_kind, alias_value)",
                [],
                |row| row.get(0),
            )
            .expect("aliases image"),
    ]
}

fn teslamate_identity_hint(source_id: Uuid, source_vehicle_key: &str) -> VehicleDescriptor {
    VehicleDescriptor {
        source_id,
        source_vehicle_key: source_vehicle_key.into(),
        vin: Some("STABLEVIN".into()),
        display_name: Some("stable".into()),
        tesla_eid: Some(88),
        tesla_vid: Some(99),
    }
}

fn update_rows_from_pack(
    inspection_directory: &Path,
    pack_path: &Path,
    name: &str,
) -> Vec<(i64, i64, i64, i64, String)> {
    let inspection_path = inspection_directory.join(format!("{name}.sqlite"));
    fs::write(
        &inspection_path,
        zstd::stream::decode_all(File::open(pack_path).expect("open pack")).expect("decode pack"),
    )
    .expect("write inspection database");
    let connection = rusqlite::Connection::open(inspection_path).expect("open inspection");
    connection
        .prepare(
            "SELECT id, car_id, start_date_ms, end_date_ms, version
                 FROM updates ORDER BY id",
        )
        .expect("schema-2.1 updates table exists")
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .expect("query updates")
        .collect::<Result<Vec<_>, _>>()
        .expect("read update rows")
}

#[test]
fn direct_successor_state_emits_typed_sparse_car_delta() {
    let temporary = crate::private_tempdir().unwrap();
    let mut projected = project_vehicle(&history(), 1).unwrap();
    let car = projected.snapshot.cars.remove(0);
    let state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 64 * 1024,
            max_changed_payload_bytes: 64 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .unwrap();
    let mut capture =
        TeslaMateProjectionStateCapture::for_successor(state, Box::new(EmptyPriorProjectionState));
    capture.record_car(&car).unwrap();
    capture.seal().unwrap();
    let binding = ProjectionBinding {
        installation_id: Uuid::from_u128(1),
        account_id: Uuid::from_u128(2),
        vehicle_id: Uuid::from_u128(3),
        generation: 0,
        selected_car_id: 1,
    };
    let mut batches = Vec::new();
    direct_delta_rows_from_capture(&mut capture, &binding, &car, 1, |batch| {
        batches.push(batch);
        Ok(())
    })
    .unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].cars, vec![car]);
    assert!(batches[0].drives.is_empty());
    assert!(batches[0].tombstones.is_empty());
}

#[test]
fn direct_successor_state_emits_typed_update_delta() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let mut source = history();
    source.updates.push(TeslaMateUpdate {
        id: 71,
        car_id: 1,
        start_date_ms: 1_700_000_000_000,
        end_date_ms: Some(1_700_000_060_000),
        version: Some("2026.44.1".into()),
    });
    let projected = project_vehicle(&source, 1).expect("project source update");
    let state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 64 * 1024,
            max_changed_payload_bytes: 64 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    let mut capture =
        TeslaMateProjectionStateCapture::for_successor(state, Box::new(EmptyPriorProjectionState));
    record_projected_direct_state(&mut capture, &projected, 1);
    capture.seal().expect("seal update capture");
    let binding = ProjectionBinding {
        installation_id: Uuid::from_u128(11),
        account_id: Uuid::from_u128(12),
        vehicle_id: Uuid::from_u128(13),
        generation: 1,
        selected_car_id: 1,
    };
    let car = projected.snapshot.cars[0].clone();
    let mut batches = Vec::new();
    direct_delta_rows_from_capture(&mut capture, &binding, &car, 1, |batch| {
        batches.push(batch);
        Ok(())
    })
    .expect("emit sparse update delta");

    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].updates, projected.updates);
    assert!(batches[0].tombstones.is_empty());
}

#[test]
fn direct_delta_batch_limit_refuses_another_batch_before_visiting_it() {
    let mut visited = 0_usize;
    let mut emitted = 0_usize;
    emit_direct_delta_batch(&mut emitted, 1, DirectDeltaRows::default(), &mut |_| {
        visited += 1;
        Ok(())
    })
    .expect("the single available batch is visited");
    let error = emit_direct_delta_batch(&mut emitted, 1, DirectDeltaRows::default(), &mut |_| {
        visited += 1;
        Ok(())
    })
    .expect_err("a protocol-full import must stop before another batch is written");
    assert!(matches!(
        error,
        TeslaMateImportError::DirectDeltaBatchLimitExceeded { maximum: 1 }
    ));
    assert_eq!(visited, 1);
}

#[test]
fn discard_unpublished_chunks_keeps_catalogued_and_reused_content() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let projected = project_vehicle(&history(), 1).expect("project source history");
    let binding = ProjectionBinding {
        installation_id: Uuid::from_u128(1),
        account_id: Uuid::from_u128(2),
        vehicle_id: Uuid::from_u128(3),
        generation: 1,
        selected_car_id: 1,
    };
    let request = ProjectionPackRequest {
        pack_id: Uuid::from_u128(4),
        snapshot_id: Uuid::from_u128(5),
        ordinal: 0,
        binding,
        sequence: SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        snapshot: &projected.snapshot,
    };
    let writer = ProjectionPackWriter::new(store.packs_dir());
    let created = writer
        .write_full_snapshot(&request)
        .expect("first producer writes pack");
    assert_eq!(created.ownership(), ProjectionPackOwnership::Created);
    let manifest = request
        .signed_manifest(&created, &CursorKey::from_bytes([46; 32]))
        .expect("sign first producer pack");
    store
        .publish_manifest(&manifest)
        .expect("catalogue first producer pack");
    let path = created.path.clone();
    let publication_gate = store.try_acquire_publication_gate().expect("gate");

    discard_unpublished_chunks(&store, &publication_gate, std::slice::from_ref(&created))
        .expect("catalogue check protects a created, now-published pack");
    assert!(path.is_file(), "catalogued created pack remains present");

    let reused = writer
        .write_full_snapshot(&request)
        .expect("second producer reuses catalogued content");
    assert_eq!(reused.ownership(), ProjectionPackOwnership::ReusedExisting);
    discard_unpublished_chunks(&store, &publication_gate, std::slice::from_ref(&reused))
        .expect("reused descriptors are never unlinked by importer cleanup");
    assert!(path.is_file(), "catalogued reused pack remains present");
    store
        .catalogue_check()
        .expect("catalogue remains valid after cleanup checks");

    let fresh_request = ProjectionPackRequest {
        pack_id: Uuid::from_u128(6),
        snapshot_id: Uuid::from_u128(7),
        ordinal: 0,
        binding: request.binding.clone(),
        sequence: request.sequence,
        snapshot: &projected.snapshot,
    };
    let fresh = writer
        .write_full_snapshot(&fresh_request)
        .expect("fresh unpublished producer pack");
    assert_eq!(fresh.ownership(), ProjectionPackOwnership::Created);
    let fresh_path = fresh.path.clone();
    discard_unpublished_chunks(&store, &publication_gate, std::slice::from_ref(&fresh))
        .expect("created unpublished pack is discarded");
    assert!(
        !fresh_path.exists(),
        "importer cleanup still removes a fresh unreferenced candidate"
    );
}

#[test]
fn proven_prior_full_snapshot_failure_cleans_candidate_under_gate() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let projected = project_vehicle(&history(), 1).expect("project source history");
    let request = ProjectionPackRequest {
        pack_id: Uuid::from_u128(81),
        snapshot_id: Uuid::from_u128(82),
        ordinal: 0,
        binding: ProjectionBinding {
            installation_id: Uuid::from_u128(83),
            account_id: Uuid::from_u128(84),
            vehicle_id: Uuid::from_u128(85),
            generation: 1,
            selected_car_id: 1,
        },
        sequence: SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        snapshot: &projected.snapshot,
    };
    let built = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot(&request)
        .expect("fresh candidate");
    let path = built.path.clone();
    let mut staged = StagedProjectionPacks::new(
        vec![built],
        ProjectionReport::default(),
        Sha256Digest::of_bytes(b"prior-candidate"),
        Vec::new(),
    );
    let gate = store.try_acquire_publication_gate().expect("gate");
    let error =
        crate::db::StoreError::CatalogueDurability(std::io::Error::other("test pre-commit result"));
    reconcile_failed_full_snapshot_candidate(&store, &gate, &mut staged, &error)
        .expect("proven prior cleanup");
    assert!(!path.exists(), "proven-prior candidate is removed");
    drop(staged);
    assert!(!path.exists(), "drop cannot recreate the candidate");
}

fn open_live_tail(
    position_ids: &[i64],
    charge_sample_ids: &[i64],
    state_id: i64,
) -> TeslaMateOpenSession {
    let drive = serde_json::from_value(serde_json::json!({
        "id": 70,
        "car_id": 1,
        "start_date_ms": 1_700_000_000_000_i64,
    }))
    .expect("open drive");
    let charge = serde_json::from_value(serde_json::json!({
        "id": 80,
        "car_id": 1,
        "start_date_ms": 1_700_000_000_000_i64,
    }))
    .expect("open charge");
    let drive_positions = position_ids
        .iter()
        .map(|id| {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "car_id": 1,
                "drive_id": 70,
                "date_ms": id * 1_000,
                "latitude": 51.0,
                "longitude": -0.1,
            }))
            .expect("open drive position")
        })
        .collect();
    let charge_samples = charge_sample_ids
        .iter()
        .map(|id| {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "charging_process_id": 80,
                "date_ms": id * 1_000,
            }))
            .expect("open charge sample")
        })
        .collect();
    let mut session = TeslaMateOpenSession {
        car_id: 1,
        drive: Some(drive),
        drive_positions,
        charge: Some(charge),
        charge_samples,
        state: Some(
            serde_json::from_value(serde_json::json!({
                "id": state_id,
                "car_id": 1,
                "state": "online",
                "start_date_ms": state_id * 1_000,
            }))
            .expect("open state"),
        ),
        ..Default::default()
    };
    session.watermarks = observed_open_watermarks(&session);
    session
}

#[test]
fn unchanged_direct_fingerprint_promotes_reconciled_live_tail_and_cleans_candidate_across_restart()
{
    const BASE_TIME_MS: i64 = 1_700_000_000_000;
    const TAIL_TIME_MS: i64 = BASE_TIME_MS + 1_000;

    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let request = TeslaMateImportRequest {
        source_key: "unchanged-direct-live-tail".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: BASE_TIME_MS,
    };
    let completed_history = history();
    let cursor_key = CursorKey::from_bytes([63; 32]);
    let base = publish_history(&store, &cursor_key, &request, &completed_history)
        .expect("publish completed-history base");
    let source = store
        .register_source(
            &SourceDescriptor::new("teslamate", request.source_key.clone()),
            BASE_TIME_MS,
        )
        .expect("registered source");
    assert_eq!(source.source_id, base.source_id);
    let base_manifest = store
        .manifest_for_vehicle(base.vehicle_id)
        .expect("base manifest lookup")
        .expect("base manifest");
    let direct_fingerprint = direct_snapshot_fingerprint(
        &source_history_fingerprint(&completed_history, 1).expect("completed-history fingerprint"),
        &completed_history.geofences,
    )
    .expect("direct completed-history fingerprint");
    // A direct base stores the direct-source fingerprint; seed that exact
    // persisted shape while keeping completed history unchanged below.
    store
        .record_snapshot_fingerprint(&base_manifest, direct_fingerprint)
        .expect("record direct base fingerprint");

    let first_tail = open_live_tail(&[101], &[201], 301);
    store
        .seed_imported_open_session(
            source.source_id,
            base.vehicle_id,
            1,
            &first_tail,
            BASE_TIME_MS,
        )
        .expect("seed first open tail");
    let run_id = store
        .begin_import_generation(source.source_id, base.vehicle_id, 1, TAIL_TIME_MS)
        .expect("begin unchanged generation");
    let second_tail = open_live_tail(&[102], &[202], 302);
    let reconciled = reconcile_open_session_cutover(&first_tail, &second_tail)
        .expect("reconcile bounded tails")
        .session;
    store
        .stage_import_generation_session(run_id, &reconciled)
        .expect("stage reconciled tail");

    let binding = ProjectionBinding {
        installation_id: store.installation_id().expect("installation id"),
        account_id: source.source_id,
        vehicle_id: base.vehicle_id,
        generation: source.generation,
        selected_car_id: 1,
    };
    let projection = project_vehicle(&completed_history, 1).expect("completed projection");
    let candidate = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot(&ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding,
            sequence: SequenceRange {
                from_exclusive: base.sequence,
                to_inclusive: base.sequence,
            },
            snapshot: &projection.snapshot,
        })
        .expect("write unreferenced direct candidate");
    let candidate_path = candidate.path.clone();
    let mut direct = StagedProjectionPacks::new(
        vec![candidate],
        projection.report,
        direct_fingerprint,
        Vec::new(),
    );
    assert!(
        store
            .source_fingerprint_matches(base.vehicle_id, direct_fingerprint)
            .expect("unchanged direct fingerprint matches")
    );

    promote_unchanged_direct_import(
        &store,
        &store.try_acquire_publication_gate().expect("gate"),
        run_id,
        source.source_id,
        base.vehicle_id,
        1,
        TAIL_TIME_MS,
        &mut direct,
    )
    .expect("promote unchanged direct tail without publication");
    drop(direct);
    assert!(
        !candidate_path.exists(),
        "unchanged direct capture removes its unreferenced candidate pack"
    );
    let lineage = store
        .lineage_manifest_for_vehicle(base.vehicle_id)
        .expect("unchanged lineage lookup")
        .expect("immutable base remains");
    assert_eq!(lineage.base.snapshot_id, base.snapshot_id);
    assert!(
        lineage.deltas.is_empty(),
        "unchanged history publishes no delta"
    );
    assert_eq!(
        store.v2_head(base.vehicle_id).expect("v2 head"),
        Some((
            base.snapshot_id,
            i64::try_from(base.sequence).expect("base sequence fits i64"),
            lineage.head_digest,
        )),
        "unchanged history publishes no replacement base"
    );
    assert_eq!(
        store
            .load_imported_open_session(source.source_id, base.vehicle_id)
            .expect("load promoted live tail"),
        Some(reconciled.clone()),
        "the changed drive, charge, and state tail is atomically promoted"
    );
    let generation_count: i64 = store
        .open()
        .expect("open catalogue")
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("generation count");
    assert_eq!(generation_count, 0, "successful promotion consumes staging");

    drop(store);
    let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
    assert_eq!(
        restarted
            .load_imported_open_session(source.source_id, base.vehicle_id)
            .expect("load restarted live tail"),
        Some(reconciled.clone()),
        "the no-publication tail survives restart"
    );
    let repeat_run = restarted
        .begin_import_generation(source.source_id, base.vehicle_id, 1, TAIL_TIME_MS + 1)
        .expect("begin repeat unchanged generation");
    restarted
        .stage_import_generation_session(repeat_run, &reconciled)
        .expect("stage idempotent tail");
    let mut no_candidate = StagedProjectionPacks::new(
        Vec::new(),
        ProjectionReport::default(),
        direct_fingerprint,
        Vec::new(),
    );
    promote_unchanged_direct_import(
        &restarted,
        &restarted.try_acquire_publication_gate().expect("gate"),
        repeat_run,
        source.source_id,
        base.vehicle_id,
        1,
        TAIL_TIME_MS,
        &mut no_candidate,
    )
    .expect("repeat unchanged tail promotion");
    assert_eq!(
        restarted
            .load_imported_open_session(source.source_id, base.vehicle_id)
            .expect("load idempotent tail"),
        Some(reconciled),
        "repeating the same unchanged tail does not duplicate or lose it"
    );
    let restarted_lineage = restarted
        .lineage_manifest_for_vehicle(base.vehicle_id)
        .expect("restarted lineage lookup")
        .expect("restarted immutable base");
    assert_eq!(restarted_lineage.base.snapshot_id, base.snapshot_id);
    assert!(
        restarted_lineage.deltas.is_empty(),
        "idempotent unchanged tail promotion publishes no delta"
    );
}

fn completed_drive(id: i64) -> crate::teslamate_projection::TeslaMateDrive {
    crate::teslamate_projection::TeslaMateDrive {
        id,
        car_id: 1,
        start_date_ms: 2_000,
        end_date_ms: Some(3_000),
        outside_temp_avg: None,
        speed_max: Some(50),
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_rated_range_km: Some(300.0),
        end_rated_range_km: Some(280.0),
        start_km: Some(10.0),
        end_km: Some(20.0),
        distance_km: Some(10.0),
        duration_min: Some(1),
        start_address_id: None,
        end_address_id: None,
        start_geofence_id: None,
        end_geofence_id: None,
        start_position_id: None,
        end_position_id: None,
        ascent: None,
        descent: None,
        inside_temp_avg: None,
    }
}

fn direct_state_test_limits() -> TeslaMateProjectionStateLimits {
    TeslaMateProjectionStateLimits {
        max_rows: 16,
        max_state_bytes: 64 * 1024,
        max_changed_payload_bytes: 64 * 1024,
        minimum_free_bytes: 0,
    }
}

fn record_projected_direct_state(
    capture: &mut TeslaMateProjectionStateCapture,
    projected: &TeslaMateProjection,
    selected_car_id: i64,
) {
    for car in &projected.snapshot.cars {
        capture.record_car(car).expect("capture car");
    }
    for drive in &projected.snapshot.drives {
        capture.record_drive(drive).expect("capture drive");
    }
    for position in &projected.snapshot.positions {
        capture.record_position(position).expect("capture position");
    }
    for charge in &projected.snapshot.charges {
        capture.record_charge(charge).expect("capture charge");
    }
    for sample in &projected.snapshot.charge_samples {
        capture
            .record_charge_sample(selected_car_id, sample)
            .expect("capture charge sample");
    }
    for state in &projected.states {
        capture.record_state(state).expect("capture state");
    }
    for update in &projected.updates {
        capture.record_update(update).expect("capture update");
    }
}

fn begin_direct_state_generation(
    store: &HubStore,
    binding: &ProjectionBinding,
    created_at_ms: i64,
) -> Uuid {
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            binding.vehicle_id,
            binding.selected_car_id,
            created_at_ms,
        )
        .expect("begin direct-state generation");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("stage direct-state session");
    run_id
}

#[test]
fn stateful_direct_capture_publishes_base_skips_unchanged_then_publishes_changed_successor_across_restart()
 {
    const BASE_TIME_MS: i64 = 1_700_000_000_000;
    const UNCHANGED_TIME_MS: i64 = BASE_TIME_MS + 1;
    const CHANGED_TIME_MS: i64 = BASE_TIME_MS + 2;

    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let source = store
        .register_source(
            &SourceDescriptor::new("teslamate", "direct-stateful-regression"),
            BASE_TIME_MS,
        )
        .expect("fixture source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "direct-stateful-regression-car"),
            BASE_TIME_MS,
        )
        .expect("fixture vehicle");
    let binding = ProjectionBinding {
        installation_id: store.installation_id().expect("installation id"),
        account_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: source.generation,
        selected_car_id: 1,
    };
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("direct-state test publication gate");
    let cursor_key = CursorKey::from_bytes([37; 32]);
    let base_history = history();
    let base_projection =
        project_vehicle(&base_history, binding.selected_car_id).expect("base projection");
    let base_run = begin_direct_state_generation(&store, &binding, BASE_TIME_MS);
    let mut base_capture = direct_projection_state_capture(
        &store,
        &publication_gate,
        base_run,
        binding.vehicle_id,
        binding.account_id,
        binding.selected_car_id,
        false,
        direct_state_test_limits(),
    )
    .expect("initial direct state capture");
    record_projected_direct_state(&mut base_capture, &base_projection, binding.selected_car_id);
    assert_eq!(
        base_capture.mode(),
        crate::teslamate_projection_state::TeslaMateProjectionStateCaptureMode::InitialBase
    );
    base_capture.seal().expect("seal base state capture");
    let base_state = base_capture.into_state();
    let base_sequence = store
        .reserve_next_full_snapshot_sequence(&publication_gate, binding.vehicle_id)
        .expect("reserve base sequence");
    let base_snapshot_id = Uuid::new_v4();
    let base_request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: base_snapshot_id,
        ordinal: 0,
        binding: binding.clone(),
        sequence: SequenceRange {
            from_exclusive: base_sequence,
            to_inclusive: base_sequence,
        },
        snapshot: &base_projection.snapshot,
    };
    let base_pack = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&base_request, &base_projection.states, &[])
        .expect("write direct base pack");
    assert_eq!(
        base_pack.ownership(),
        ProjectionPackOwnership::Created,
        "the initial direct-import candidate owns its fresh pack until finalization"
    );
    let base_manifest = base_request
        .signed_manifest_with_states_and_updates(
            &base_pack,
            &base_projection.states,
            &[],
            &cursor_key,
        )
        .expect("sign direct base manifest");
    let base_fingerprint = direct_snapshot_fingerprint(
        &source_history_fingerprint(&base_history, binding.selected_car_id)
            .expect("base source fingerprint"),
        &base_history.geofences,
    )
    .expect("base direct fingerprint");
    let base_pack_path = base_pack.path.clone();
    let base_candidate = StagedProjectionPacks::new(
        vec![base_pack],
        ProjectionReport::default(),
        base_fingerprint,
        Vec::new(),
    );
    // Both injected failures happen after the SQLite transaction owns the
    // candidate. The durable receipt must reconcile the commit to success,
    // and dropping a still-armed candidate must never unlink its pack.
    let _commit_fault = crate::durability_fault::inject(
        crate::durability_fault::DurabilityFaultPoint::CatalogueAfterCommit,
    );
    store.inject_projection_state_detach_fault();
    store
        .finalize_import_generation_with_projection_state(
            base_run,
            binding.account_id,
            binding.vehicle_id,
            binding.selected_car_id,
            BASE_TIME_MS,
            &base_manifest,
            base_fingerprint,
            &base_history.geofences,
            &binding,
            &base_state,
            false,
        )
        .expect("committed direct base survives post-commit detach failure");
    drop(base_candidate);
    assert!(
        base_pack_path.is_file(),
        "candidate cleanup cannot delete the newly catalogued created pack"
    );
    store
        .catalogue_check()
        .expect("committed direct base remains readable after candidate cleanup");

    let unchanged_run = begin_direct_state_generation(&store, &binding, UNCHANGED_TIME_MS);
    let mut unchanged_capture = direct_projection_state_capture(
        &store,
        &publication_gate,
        unchanged_run,
        binding.vehicle_id,
        binding.account_id,
        binding.selected_car_id,
        true,
        direct_state_test_limits(),
    )
    .expect("durable prior state enables unchanged direct capture");
    record_projected_direct_state(
        &mut unchanged_capture,
        &base_projection,
        binding.selected_car_id,
    );
    assert_eq!(
        unchanged_capture.mode(),
        crate::teslamate_projection_state::TeslaMateProjectionStateCaptureMode::Successor
    );
    let unchanged_stats = unchanged_capture
        .seal()
        .expect("seal unchanged direct capture");
    assert_eq!(unchanged_stats.changed_row_count, 0);
    // Production guards on the persisted source fingerprint before it
    // invokes `direct_delta_rows_from_capture`: an empty capture would
    // otherwise intentionally emit a metadata-only car delta.
    assert!(
        store
            .source_fingerprint_matches(binding.vehicle_id, base_fingerprint)
            .expect("unchanged fingerprint lookup"),
        "the unchanged direct run must take the fingerprint guard"
    );
    store
        .abort_import_generation(unchanged_run)
        .expect("fingerprint-guarded generation is discarded");
    let unchanged_lineage = store
        .lineage_manifest_for_vehicle(binding.vehicle_id)
        .expect("unchanged lineage lookup")
        .expect("base lineage remains published");
    assert!(
        unchanged_lineage.deltas.is_empty(),
        "the fingerprint guard must not publish a successor"
    );
    drop(unchanged_capture);

    let mut changed_history = history();
    changed_history.drives.push(completed_drive(99));
    let changed_projection =
        project_vehicle(&changed_history, binding.selected_car_id).expect("changed projection");
    let changed_fingerprint = direct_snapshot_fingerprint(
        &source_history_fingerprint(&changed_history, binding.selected_car_id)
            .expect("changed source fingerprint"),
        &changed_history.geofences,
    )
    .expect("changed direct fingerprint");
    assert_ne!(changed_fingerprint, base_fingerprint);
    assert!(
        !store
            .source_fingerprint_matches(binding.vehicle_id, changed_fingerprint)
            .expect("changed fingerprint lookup"),
        "changed source history must not take the unchanged guard"
    );
    let successor_run = begin_direct_state_generation(&store, &binding, CHANGED_TIME_MS);
    let mut changed_capture = direct_projection_state_capture(
        &store,
        &publication_gate,
        successor_run,
        binding.vehicle_id,
        binding.account_id,
        binding.selected_car_id,
        true,
        direct_state_test_limits(),
    )
    .expect("durable prior state enables changed direct capture");
    record_projected_direct_state(
        &mut changed_capture,
        &changed_projection,
        binding.selected_car_id,
    );
    let changed_stats = changed_capture.seal().expect("seal changed direct capture");
    assert_eq!(changed_stats.changed_row_count, 1);

    let base_lineage = store
        .lineage_manifest_for_vehicle(binding.vehicle_id)
        .expect("base lineage lookup")
        .expect("base lineage");
    let mut deltas = Vec::new();
    let mut next_ordinal = store
        .next_v2_pack_ordinal(base_snapshot_id)
        .expect("next successor ordinal");
    let mut parent_digest = base_lineage.head_digest;
    let mut prior_sequence = base_lineage.head_sequence;
    let selected_car = changed_projection
        .snapshot
        .cars
        .first()
        .expect("changed projection car");
    direct_delta_rows_from_capture(&mut changed_capture, &binding, selected_car, 1, |batch| {
        let to_sequence = store
            .reserve_next_full_snapshot_sequence(&publication_gate, binding.vehicle_id)
            .map_err(TeslaMateImportError::from)?;
        let delta = batch.into_delta(
            binding.clone(),
            SequenceRange {
                from_exclusive: prior_sequence,
                to_inclusive: to_sequence,
            },
            parent_digest,
        );
        let built = ProjectionPackWriter::new(store.packs_dir()).write_delta(
            &ProjectionDeltaPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: base_snapshot_id,
                ordinal: next_ordinal,
                delta: &delta,
            },
        )?;
        let chain_digest = canonical_delta_chain_digest(parent_digest, built.metadata.sha256);
        deltas.push(LineageDelta {
            from_sequence: prior_sequence,
            to_sequence,
            parent_chain_digest: parent_digest,
            chain_digest,
            pack_digest: built.metadata.sha256,
            pack: built.metadata,
        });
        prior_sequence = to_sequence;
        parent_digest = chain_digest;
        next_ordinal = next_ordinal
            .checked_add(1)
            .ok_or(crate::db::StoreError::PackOrdinalTooLarge)?;
        Ok(())
    })
    .expect("emit changed direct sparse delta");
    assert_eq!(deltas.len(), 1, "one changed drive fits one typed delta");
    let successor_state = changed_capture.into_state();
    let terminal_cursor = OpaqueCursor::issue(
        &cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: prior_sequence,
        },
    )
    .expect("successor terminal cursor");
    store
        .finalize_import_generation_delta_successors_with_projection_state(
            successor_run,
            binding.account_id,
            binding.vehicle_id,
            binding.selected_car_id,
            CHANGED_TIME_MS,
            &deltas,
            &cursor_key,
            &terminal_cursor,
            changed_fingerprint,
            &changed_history.geofences,
            &successor_state,
            false,
        )
        .expect("atomically publish direct sparse successor and state");
    assert!(
        store
            .source_fingerprint_matches(binding.vehicle_id, changed_fingerprint)
            .expect("published changed fingerprint lookup")
    );

    let lineage = store
        .lineage_manifest_for_vehicle(binding.vehicle_id)
        .expect("changed lineage lookup")
        .expect("changed lineage");
    lineage
        .validate()
        .expect("published direct lineage validates");
    assert_eq!(lineage.base.snapshot_id, base_snapshot_id);
    assert_eq!(lineage.deltas.len(), 1);
    assert_eq!(lineage.head_sequence, prior_sequence);
    let delta_pack = store
        .pack_for_digest(lineage.deltas[0].pack.sha256)
        .expect("typed delta pack lookup")
        .expect("typed delta pack");
    lineage.deltas[0]
        .pack
        .verify_reader(
            File::open(&delta_pack.path).expect("open typed delta pack"),
            ProtocolLimits::default(),
        )
        .expect("typed delta pack validates");
    let inspection_path = temporary.path().join("stateful-direct-successor.sqlite");
    fs::write(
        &inspection_path,
        zstd::stream::decode_all(File::open(delta_pack.path).expect("decode typed delta pack"))
            .expect("decode typed delta pack"),
    )
    .expect("write typed delta inspection database");
    let inspection =
        rusqlite::Connection::open(&inspection_path).expect("open typed delta inspection database");
    let mode: String = inspection
        .query_row(
            "SELECT value FROM hub_pack_metadata WHERE key = 'mode'",
            [],
            |row| row.get(0),
        )
        .expect("typed delta mode");
    let changed_drive: i64 = inspection
        .query_row("SELECT id FROM drives", [], |row| row.get(0))
        .expect("changed direct drive in typed delta");
    assert_eq!(mode, "typed_delta");
    assert_eq!(changed_drive, 99);
    drop(inspection);

    let state = store
        .teslamate_import_projection_state_lookup(
            binding.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("published successor state lookup");
    assert_eq!(state.header().base_snapshot_id, base_snapshot_id);
    assert_eq!(state.header().head_sequence, lineage.head_sequence);
    drop(state);
    let connection = store.open().expect("Hub catalogue");
    let base_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sync_bases WHERE vehicle_id = ?1",
            rusqlite::params![binding.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("base count");
    let generation_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("generation count");
    assert_eq!(
        base_count, 1,
        "the successor must retain one immutable base"
    );
    assert_eq!(
        generation_count, 0,
        "base, guarded unchanged, and successor generations must be consumed"
    );
    drop(connection);
    drop(successor_state);
    drop(base_state);
    drop(publication_gate);
    drop(store);

    let restarted = HubStore::initialize(temporary.path()).expect("reopen Hub store");
    let restarted_lineage = restarted
        .lineage_manifest_for_vehicle(binding.vehicle_id)
        .expect("restarted lineage lookup")
        .expect("restarted lineage");
    restarted_lineage
        .validate()
        .expect("restarted direct lineage validates");
    assert_eq!(restarted_lineage.base.snapshot_id, base_snapshot_id);
    assert_eq!(restarted_lineage.deltas.len(), 1);
    assert_eq!(restarted_lineage.head_sequence, prior_sequence);
    let mut restarted_state = restarted
        .teslamate_import_projection_state_lookup(
            binding.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("restarted successor state lookup");
    assert_eq!(
        restarted_state.header().base_snapshot_id,
        restarted_lineage.base.snapshot_id
    );
    assert_eq!(restarted_state.header().head_sequence, prior_sequence);
    assert!(
        crate::teslamate_projection_state::PriorProjectionStateLookup::digest(
            &mut restarted_state,
            TeslaMateProjectionStateEntity::Drive,
            99,
        )
        .expect("restarted changed-drive digest lookup")
        .is_some(),
        "restarted durable state must retain the changed drive digest"
    );
}

#[test]
fn publishes_stable_vehicle_full_snapshots_with_rising_markers() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let request = TeslaMateImportRequest {
        source_key: "home-teslamate".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let mut source = history();
    source
        .states
        .push(crate::teslamate_projection::TeslaMateState {
            id: 20,
            car_id: 1,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: None,
        });
    let cursor_key = CursorKey::from_bytes([7; 32]);
    let first = publish_history(&store, &cursor_key, &request, &source).unwrap();
    let second = publish_history(&store, &cursor_key, &request, &source).unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(second.sequence, first.sequence);
    assert!(second.skipped);
    assert_eq!(first.vehicle_id, second.vehicle_id);
    assert_eq!(first.projected_rows, 2);
    let manifest = store
        .manifest_for_vehicle(first.vehicle_id)
        .unwrap()
        .expect("latest manifest");
    assert_eq!(manifest.head_sequence, 1);
    assert_eq!(manifest.schema, crate::hub_pack::HUB_PROJECTION_SCHEMA_V2);
    assert_eq!(
        manifest.chunks[0].format,
        crate::protocol::PackFormat::HubProjectionSqlite
    );
}

#[test]
fn selected_command_post_commit_finalizer_is_explicit_and_retry_safe() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let cursor_key = CursorKey::from_bytes([57; 32]);
    let request = TeslaMateImportRequest {
        source_key: format!("selected-command-{}", Uuid::new_v4()),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let legacy = publish_history(&store, &cursor_key, &request, &history())
        .expect("legacy schema-2.1 import commits");
    let binding = store
        .v2_projection_binding(legacy.vehicle_id)
        .expect("legacy binding");
    let source = DirectUpdatesSourceV2_2 {
        postgres_snapshot_sha256: crate::updates_logical::hex_sha256(Uuid::new_v4().as_bytes()),
        schema: crate::teslamate_reader::TeslaMateSchemaInfo {
            observed_migration_version: crate::teslamate_schema::MAX_VALIDATED_MIGRATION,
            observed_migration_count: crate::teslamate_schema::TESLAMATE_V4_MIGRATION_COUNT,
            minimum_supported_migration_version: crate::teslamate_schema::MIN_SUPPORTED_MIGRATION,
            maximum_validated_migration_version: crate::teslamate_schema::MAX_VALIDATED_MIGRATION,
            pinned_source_revision: crate::teslamate_schema::TESLAMATE_V4_SOURCE_REVISION,
            pinned_migration_set_sha256: crate::teslamate_schema::TESLAMATE_V4_MIGRATION_SET_SHA256,
            fingerprint: crate::updates_logical::hex_sha256(b"selected-command-schema"),
        },
        global_settings: crate::teslamate_projection::TeslaMateSettingsPhysicalV2_2 {
            id: 1,
            unit_of_length: crate::hub_pack::ProjectionUnitOfLengthV2_2::Kilometers,
            unit_of_temperature: crate::hub_pack::ProjectionUnitOfTemperatureV2_2::Celsius,
            unit_of_pressure: crate::hub_pack::ProjectionUnitOfPressureV2_2::Bar,
            preferred_range: crate::hub_pack::ProjectionPreferredRangeV2_2::Rated,
            base_url: None,
            grafana_url: None,
            language: "en".into(),
            theme_mode: "system".into(),
            inserted_at_pg_us: 0,
            updated_at_pg_us: 0,
        },
        car: crate::teslamate_projection::TeslaMateCarPhysicalV2_2 {
            id: 1,
            eid: 88,
            vid: 99,
            vin: Some("5YJTESTVIN1234567".into()),
            name: Some("Road car".into()),
            model: Some("Model 3".into()),
            efficiency: Some(0.145),
            trim_badging: Some("74d".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            display_priority: 0,
            inserted_at_pg_us: 0,
            updated_at_pg_us: 0,
            settings_id: 1,
        },
        car_settings: crate::teslamate_projection::TeslaMateCarSettingsPhysicalV2_2 {
            id: 1,
            suspend_min: 21,
            suspend_after_idle_min: 15,
            req_not_unlocked: false,
            free_supercharging: false,
            use_streaming_api: true,
            enabled: true,
            lfp_battery: false,
        },
        updates: Vec::new(),
    };
    let registered_car = history().cars.remove(0);
    validate_exported_vehicle_identity(&registered_car, &source)
        .expect("same exported VIN/EID/VID tuple");
    let mut changed_eid = source.clone();
    changed_eid.car.eid += 1;
    let mut changed_vid = source.clone();
    changed_vid.car.vid += 1;
    let mut changed_vin = source.clone();
    changed_vin.car.vin = Some("DIFFERENTVIN".into());
    for changed in [&changed_eid, &changed_vid, &changed_vin] {
        assert!(matches!(
            validate_exported_vehicle_identity(&registered_car, changed),
            Err(TeslaMateImportError::SourceVehicleIdentityChangedDuringCapture)
        ));
    }
    let legacy_manifest = store
        .manifest_for_vehicle(legacy.vehicle_id)
        .expect("legacy lookup")
        .expect("legacy manifest");
    let unadmitted_gate = store
        .try_acquire_publication_gate()
        .expect("unadmitted schema head gate");
    let expected_schema_22_head =
        production_updates_head(&store, legacy.vehicle_id).expect("schema-2.2 head");
    let error = publish_production_updates_schema_22_with_gate(
        &store,
        &cursor_key,
        &binding,
        source.clone(),
        &unadmitted_gate,
        &expected_schema_22_head,
        None,
    )
    .expect_err("other-schema head needs an exact admitted legacy commit");
    drop(unadmitted_gate);
    assert!(error.message.contains("unadmitted other-schema head"));
    assert_eq!(
        store
            .manifest_for_vehicle(legacy.vehicle_id)
            .expect("manifest after unadmitted rejection")
            .expect("legacy manifest retained"),
        legacy_manifest
    );
    let mut wrong_binding = binding.clone();
    wrong_binding.selected_car_id = 2;
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("post-commit publication gate");
    let error = finish_selected_schema_22_publication(
        &store,
        &cursor_key,
        CapturedTeslaMateImport {
            report: legacy.clone(),
            binding: wrong_binding,
            updates_v2_2: source.clone(),
            legacy_tokens: None,
            atomic_schema_22: None,
            publication_gate,
        },
    )
    .expect_err("post-commit publication failure is explicit");
    assert!(matches!(
        error,
        TeslaMateImportError::Schema22PostCommit {
            vehicle_id,
            legacy_snapshot_id,
            ..
        } if vehicle_id == legacy.vehicle_id && legacy_snapshot_id == legacy.snapshot_id
    ));
    assert_eq!(
        store
            .manifest_for_vehicle(legacy.vehicle_id)
            .expect("manifest after failure")
            .expect("legacy manifest retained"),
        legacy_manifest
    );

    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("retry publication gate");
    let completed = finish_selected_schema_22_publication(
        &store,
        &cursor_key,
        CapturedTeslaMateImport {
            report: legacy.clone(),
            binding: binding.clone(),
            updates_v2_2: source.clone(),
            legacy_tokens: None,
            atomic_schema_22: None,
            publication_gate,
        },
    )
    .expect("retry publishes the schema-2.2 pair");
    assert_eq!(completed.import, legacy);
    assert!(completed.updates_schema_22.sequence > completed.import.sequence);
    assert_eq!(
        completed.updates_schema_22.source_witness.source_row_count,
        0
    );
    let current = store
        .manifest_for_vehicle(completed.import.vehicle_id)
        .expect("schema-2.2 lookup")
        .expect("schema-2.2 manifest");
    assert_eq!(current.schema, crate::protocol::HUB_PROJECTION_SCHEMA_V3);
    assert_eq!(current.generation, binding.generation);
    assert_eq!(current.snapshot_id, completed.updates_schema_22.snapshot_id);
    assert_eq!(current.head_sequence, completed.updates_schema_22.sequence);
    crate::updates_delivery::schema_22_signed_artifacts(&store, current.vehicle_id, &cursor_key)
        .expect("canonical signed manifest/no-op pair");

    let delta_temporary = crate::private_tempdir().expect("delta Hub store");
    let delta_store = HubStore::initialize(delta_temporary.path()).expect("delta store");
    let delta_request = TeslaMateImportRequest {
        source_key: format!("selected-delta-{}", Uuid::new_v4()),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_100,
    };
    let base = publish_history(&delta_store, &cursor_key, &delta_request, &history())
        .expect("schema-2.1 base");
    let mut changed_history = history();
    changed_history.updates.push(TeslaMateUpdate {
        id: 71,
        car_id: 1,
        start_date_ms: 1_700_000_000_000,
        end_date_ms: Some(1_700_000_060_000),
        version: Some("2026.44.1".into()),
    });
    let delta = publish_history(
        &delta_store,
        &cursor_key,
        &TeslaMateImportRequest {
            imported_at_ms: 1_700_000_000_101,
            ..delta_request
        },
        &changed_history,
    )
    .expect("schema-2.1 typed delta");
    assert_eq!(delta.snapshot_id, base.snapshot_id);
    assert!(delta.sequence > base.sequence);
    let immutable_base = delta_store
        .manifest_for_vehicle(delta.vehicle_id)
        .expect("base manifest lookup")
        .expect("immutable base manifest");
    assert!(immutable_base.head_sequence < delta.sequence);
    let delta_binding = delta_store
        .v2_projection_binding(delta.vehicle_id)
        .expect("delta binding");
    let mut delta_updates = source;
    delta_updates.postgres_snapshot_sha256 =
        crate::updates_logical::hex_sha256(Uuid::new_v4().as_bytes());
    delta_updates
        .updates
        .push(crate::teslamate_projection::TeslaMateUpdatePhysicalV2_2 {
            id: 71,
            car_id: 1,
            start_date_pg_us: 0,
            end_date_pg_us: Some(1),
            version: Some("2026.44.1".into()),
        });
    let publication_gate = delta_store
        .try_acquire_publication_gate()
        .expect("delta bootstrap gate");
    let upgraded = finish_selected_schema_22_publication(
        &delta_store,
        &cursor_key,
        CapturedTeslaMateImport {
            report: delta.clone(),
            binding: delta_binding.clone(),
            updates_v2_2: delta_updates,
            legacy_tokens: None,
            atomic_schema_22: None,
            publication_gate,
        },
    )
    .expect("current V2 delta head admits schema-2.2 bootstrap");
    assert!(upgraded.updates_schema_22.sequence > delta.sequence);
    assert_eq!(
        upgraded.updates_schema_22.source_witness.selected_car_id,
        delta_binding.selected_car_id
    );
    assert_eq!(
        upgraded.updates_schema_22.source_witness.head_sequence,
        upgraded.updates_schema_22.sequence
    );
}

#[test]
fn rejected_exported_identity_restores_source_vehicle_and_alias_registry() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let empty_registry = identity_registry_image(&store);
    let source_descriptor =
        SourceDescriptor::new("teslamate", format!("identity-{}", Uuid::new_v4()));
    let (source, source_created) = store
        .register_teslamate_import_source(&source_descriptor, 1_000)
        .expect("provisional source");
    assert!(source_created);
    let vehicle_id = Uuid::new_v5(&source.source_id, b"eid:88");
    let identity_hint = teslamate_identity_hint(source.source_id, "eid:88");
    let (vehicle, checkpoint) = store
        .provision_teslamate_import_identity(
            &source,
            source_created,
            &identity_hint,
            1_000,
            vehicle_id,
        )
        .expect("provisional vehicle");
    assert_eq!(vehicle.vin, None);
    assert_eq!(vehicle.display_name, None);
    assert_eq!(identity_registry_image(&store)[3], "[]");
    assert!(
        store
            .published_vehicles()
            .expect("published vehicles")
            .is_empty()
    );
    let run_id = store
        .begin_import_generation(source.source_id, vehicle.vehicle_id, 1, 1_000)
        .expect("provisional generation");
    store
        .abort_import_generation(run_id)
        .expect("abort mismatched generation");
    store
        .rollback_teslamate_identity_registration(&checkpoint)
        .expect("rollback mismatched identity");
    assert_eq!(identity_registry_image(&store), empty_registry);
}

#[test]
fn crash_residue_is_alias_free_nonpublished_and_reused_after_reopen() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let source_descriptor = SourceDescriptor::new("teslamate", format!("crash-{}", Uuid::new_v4()));
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let (source_before_crash, source_created) = store
        .register_teslamate_import_source(&source_descriptor, 2_000)
        .expect("source commit before crash");
    assert!(source_created);
    drop(store);

    let reopened = HubStore::initialize(temporary.path()).expect("reopen after source commit");
    let (source, source_created) = reopened
        .register_teslamate_import_source(&source_descriptor, 3_000)
        .expect("reuse source residue");
    assert_eq!(source, source_before_crash);
    assert!(!source_created);
    let vehicle_id = Uuid::new_v5(&source.source_id, b"eid:88");
    let identity_hint = teslamate_identity_hint(source.source_id, "eid:88");
    let (vehicle_before_crash, _) = reopened
        .provision_teslamate_import_identity(&source, false, &identity_hint, 3_000, vehicle_id)
        .expect("vehicle commit before crash");
    drop(reopened);

    let retried = HubStore::initialize(temporary.path()).expect("reopen after vehicle commit");
    let (source, source_created) = retried
        .register_teslamate_import_source(&source_descriptor, 4_000)
        .expect("reuse source on retry");
    let (vehicle, checkpoint) = retried
        .provision_teslamate_import_identity(
            &source,
            source_created,
            &identity_hint,
            4_000,
            vehicle_id,
        )
        .expect("reuse vehicle on retry");
    assert_eq!(vehicle, vehicle_before_crash);
    assert_eq!(identity_registry_image(&retried)[3], "[]");
    assert!(
        retried
            .published_vehicles()
            .expect("published vehicles")
            .is_empty()
    );
    retried
        .rollback_teslamate_identity_registration(&checkpoint)
        .expect("reused identity has nothing provisional to delete");
    assert_eq!(
        retried
            .register_teslamate_import_source(&source_descriptor, 5_000)
            .expect("source remains reusable")
            .0,
        source
    );
}

#[test]
fn rollback_preserves_interleaved_collector_alias_and_observation() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let source_descriptor =
        SourceDescriptor::new("teslamate", format!("interleaved-{}", Uuid::new_v4()));
    let (source, source_created) = store
        .register_teslamate_import_source(&source_descriptor, 2_000)
        .expect("provisional source");
    let vehicle_id = Uuid::new_v5(&source.source_id, b"eid:88");
    let identity_hint = teslamate_identity_hint(source.source_id, "eid:88");
    let (_, checkpoint) = store
        .provision_teslamate_import_identity(
            &source,
            source_created,
            &identity_hint,
            2_000,
            vehicle_id,
        )
        .expect("provisional vehicle");
    let collector_descriptor = VehicleDescriptor {
        source_id: source.source_id,
        source_vehicle_key: "eid:88".into(),
        vin: Some("STABLEVIN".into()),
        display_name: Some("stable".into()),
        tesla_eid: Some(88),
        tesla_vid: Some(99),
    };
    let collector_vehicle = store
        .register_vehicle_with_id(&collector_descriptor, 3_000, vehicle_id)
        .expect("collector registers proven identity");
    store
        .append_observation(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: collector_vehicle.vehicle_id,
                observed_at_ms: 3_000,
                payload: serde_json::json!({"state": "online"}),
            },
            3_001,
        )
        .expect("collector observation");
    let collector_registry = identity_registry_image(&store);
    assert!(matches!(
        store.rollback_teslamate_identity_registration(&checkpoint),
        Err(StoreError::VehicleIdentityConflict)
    ));
    assert_eq!(identity_registry_image(&store), collector_registry);
    let observation_count: i64 = store
        .open()
        .expect("observation registry")
        .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
            row.get(0)
        })
        .expect("observation count");
    assert_eq!(observation_count, 1);
    assert!(
        store
            .published_vehicles()
            .expect("published vehicles")
            .is_empty()
    );
}

#[test]
fn teslamate_import_reuses_existing_cross_source_vehicle_identity() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let owner_source = store
        .register_source(
            &SourceDescriptor::new("owner_api", format!("owner-{}", Uuid::new_v4())),
            1_000,
        )
        .expect("owner source");
    let owner_descriptor = VehicleDescriptor {
        source_id: owner_source.source_id,
        source_vehicle_key: "owner-88".into(),
        vin: Some("STABLEVIN".into()),
        display_name: Some("owner car".into()),
        tesla_eid: Some(88),
        tesla_vid: Some(99),
    };
    let owner_vehicle = store
        .register_vehicle(&owner_descriptor, 1_001)
        .expect("owner vehicle");

    let teslamate_descriptor =
        SourceDescriptor::new("teslamate", format!("history-{}", Uuid::new_v4()));
    let (teslamate_source, source_created) = store
        .register_teslamate_import_source(&teslamate_descriptor, 2_000)
        .expect("TeslaMate source");
    let identity_hint = teslamate_identity_hint(teslamate_source.source_id, "eid:88");
    let deterministic_vehicle_id = Uuid::new_v5(&teslamate_source.source_id, b"eid:88");
    let (provisioned, _) = store
        .provision_teslamate_import_identity(
            &teslamate_source,
            source_created,
            &identity_hint,
            2_000,
            deterministic_vehicle_id,
        )
        .expect("reuse owner identity before capture");
    assert_eq!(provisioned.vehicle_id, owner_vehicle.vehicle_id);

    let verified = store
        .register_vehicle_with_id(&identity_hint, 2_001, deterministic_vehicle_id)
        .expect("attach TeslaMate identity after frozen proof");
    assert_eq!(verified.vehicle_id, owner_vehicle.vehicle_id);
    let vehicle_count: i64 = store
        .open()
        .expect("vehicle registry")
        .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
        .expect("vehicle count");
    assert_eq!(vehicle_count, 1);
}

#[test]
fn crash_residue_converges_with_later_cross_source_collector_identity() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let teslamate_descriptor =
        SourceDescriptor::new("teslamate", format!("crash-merge-{}", Uuid::new_v4()));
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let (teslamate_source, source_created) = store
        .register_teslamate_import_source(&teslamate_descriptor, 1_000)
        .expect("TeslaMate source");
    let identity_hint = teslamate_identity_hint(teslamate_source.source_id, "eid:88");
    let provisional_id = Uuid::new_v5(&teslamate_source.source_id, b"eid:88");
    let (provisional, _) = store
        .provision_teslamate_import_identity(
            &teslamate_source,
            source_created,
            &identity_hint,
            1_000,
            provisional_id,
        )
        .expect("alias-free vehicle before crash");
    assert_eq!(provisional.vehicle_id, provisional_id);
    drop(store);

    let restarted = HubStore::initialize(temporary.path()).expect("restart Hub store");
    let owner_source = restarted
        .register_source(
            &SourceDescriptor::new("owner_api", format!("owner-{}", Uuid::new_v4())),
            2_000,
        )
        .expect("owner source");
    let owner_descriptor = VehicleDescriptor {
        source_id: owner_source.source_id,
        source_vehicle_key: "owner-88".into(),
        vin: Some("STABLEVIN".into()),
        display_name: Some("owner car".into()),
        tesla_eid: Some(88),
        tesla_vid: Some(99),
    };
    let owner_vehicle = restarted
        .register_vehicle(&owner_descriptor, 2_001)
        .expect("collector identity after crash");
    restarted
        .append_observation(
            &ObservationInput {
                source_id: owner_source.source_id,
                vehicle_id: owner_vehicle.vehicle_id,
                observed_at_ms: 2_002,
                payload: serde_json::json!({"state": "online"}),
            },
            2_003,
        )
        .expect("collector observation");

    let (teslamate_source, source_created) = restarted
        .register_teslamate_import_source(&teslamate_descriptor, 3_000)
        .expect("reuse TeslaMate source");
    assert!(!source_created);
    let identity_hint = teslamate_identity_hint(teslamate_source.source_id, "eid:88");
    let (converged, _) = restarted
        .provision_teslamate_import_identity(
            &teslamate_source,
            source_created,
            &identity_hint,
            3_000,
            provisional_id,
        )
        .expect("converge crash residue with collector identity");
    assert_eq!(converged.vehicle_id, owner_vehicle.vehicle_id);
    let connection = restarted.open().expect("converged registry");
    let vehicle_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
        .expect("vehicle count");
    let observation_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
            row.get(0)
        })
        .expect("observation count");
    assert_eq!(vehicle_count, 1);
    assert_eq!(observation_count, 1);
}

#[test]
fn publish_history_keeps_thirty_nine_exact_updates_in_signed_base_and_changed_delta() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let request = TeslaMateImportRequest {
        source_key: "update-history-regression".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([61; 32]);
    let mut source = history();
    let expected = (0_i64..39)
        .map(|offset| {
            let id = 31_000 + offset;
            let start_date_ms = 1_800_000_000_000 + offset * 10_000;
            let end_date_ms = start_date_ms + 9_000;
            let version = format!("2026.44.{}", offset + 1);
            source.updates.push(TeslaMateUpdate {
                id,
                car_id: 1,
                start_date_ms,
                end_date_ms: Some(end_date_ms),
                version: Some(version.clone()),
            });
            (id, 1, start_date_ms, end_date_ms, version)
        })
        .collect::<Vec<_>>();

    let first = publish_history(&store, &cursor_key, &request, &source)
        .expect("publish signed schema-2.1 base");
    assert!(!first.skipped);
    assert_eq!(first.projection.projected_updates, 39);
    let base_manifest = store
        .manifest_for_vehicle(first.vehicle_id)
        .expect("base manifest query")
        .expect("base manifest");
    assert_eq!(base_manifest.schema, HUB_PROJECTION_SCHEMA_V2);
    base_manifest
        .validate_terminal_cursor(&cursor_key)
        .expect("base manifest has a valid signature");
    let base_pack = store
        .pack_for_digest(base_manifest.chunks[0].sha256)
        .expect("base pack lookup")
        .expect("base pack");
    assert_eq!(
        update_rows_from_pack(temporary.path(), &base_pack.path, "base-updates"),
        expected,
        "base pack preserves every source update ID, version, and complete date range"
    );

    let changed_offset = 20_usize;
    let changed_end_date_ms = expected[changed_offset].3 + 5_000;
    let changed_version = "2026.44.21-hotfix".to_owned();
    source.updates[changed_offset].end_date_ms = Some(changed_end_date_ms);
    source.updates[changed_offset].version = Some(changed_version.clone());
    let mut expected_changed = expected.clone();
    expected_changed[changed_offset].3 = changed_end_date_ms;
    expected_changed[changed_offset].4 = changed_version;

    let second = publish_history(&store, &cursor_key, &request, &source)
        .expect("changed update publishes a typed successor");
    assert!(!second.skipped);
    assert_eq!(second.snapshot_id, first.snapshot_id);
    assert!(second.sequence > first.sequence);
    let lineage = store
        .lineage_manifest_for_vehicle(first.vehicle_id)
        .expect("lineage lookup")
        .expect("lineage");
    lineage
        .validate()
        .expect("changed update lineage remains signed and valid");
    assert_eq!(lineage.deltas.len(), 1);
    let delta_pack = store
        .pack_for_digest(lineage.deltas[0].pack.sha256)
        .expect("delta pack lookup")
        .expect("typed delta pack");
    assert_eq!(lineage.deltas[0].pack.schema, HUB_PROJECTION_SCHEMA_V2);
    assert_eq!(
        update_rows_from_pack(temporary.path(), &delta_pack.path, "changed-updates"),
        expected_changed,
        "changed firmware history is carried by the typed delta instead of being dropped"
    );
}

#[test]
fn imported_geofences_survive_publication_and_restart() {
    let data = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(data.path()).unwrap();
    let request = TeslaMateImportRequest {
        source_key: "home-teslamate".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let mut source = history();
    source.geofences = vec![
        TeslaMateGeofence {
            id: 1,
            name: "Home".into(),
            latitude: Some(51.0),
            longitude: Some(-0.1),
            radius_m: Some(150.0),
            billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
            cost_per_unit: Some(0.30),
            session_fee: Some(2.0),
        },
        TeslaMateGeofence {
            id: 2,
            name: "Work".into(),
            latitude: Some(51.001),
            longitude: Some(-0.101),
            radius_m: Some(150.0),
            billing_type: Some(crate::hub_pack::GeofenceBillingType::PerMinute),
            cost_per_unit: Some(0.10),
            session_fee: Some(1.0),
        },
    ];
    let cursor_key = CursorKey::from_bytes([8; 32]);
    let published = publish_history(&store, &cursor_key, &request, &source).unwrap();
    let count: i64 = store
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM geofences WHERE vehicle_id = ?1",
            [published.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    drop(store);

    let reopened = HubStore::initialize(data.path()).unwrap();
    let names: Vec<String> = {
        let connection = reopened.open().unwrap();
        let mut statement = connection
            .prepare("SELECT name FROM geofences WHERE vehicle_id = ?1 ORDER BY source_geofence_id")
            .unwrap();
        statement
            .query_map([published.vehicle_id.to_string()], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(names, ["Home", "Work"]);
}

#[test]
fn eid_fallback_never_uses_the_local_car_id() {
    let mut source = history();
    source.cars[0].vin = None;
    source.cars[0].eid = 9001;
    assert_eq!(
        stable_vehicle_key_for_car(&source.cars[0]).unwrap(),
        "eid:9001"
    );
}

#[test]
fn changed_history_publishes_import_delta_successor_without_second_base() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let request = TeslaMateImportRequest {
        source_key: "home-teslamate".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([7; 32]);
    let first_source = history();
    let first = publish_history(&store, &cursor_key, &request, &first_source).unwrap();
    assert!(!first.skipped);
    assert_eq!(first.sequence, 1);

    let base_lineage = store
        .lineage_manifest_for_vehicle(first.vehicle_id)
        .expect("lineage after first import")
        .expect("base lineage present");
    base_lineage
        .validate()
        .expect("first import lineage must validate");
    assert!(base_lineage.deltas.is_empty());
    assert_eq!(base_lineage.base.snapshot_id, first.snapshot_id);
    assert_eq!(base_lineage.head_sequence, first.sequence);

    // A SyncManifest can describe a complete snapshot but cannot prove
    // that its SQLite payload is a typed delta. Do not let a caller relabel
    // the base bytes with a successor range: only the dedicated typed-delta
    // finalizer may extend this lineage.
    let before_forged_successor = base_lineage.clone();
    let mut forged_successor = store
        .manifest_for_vehicle(first.vehicle_id)
        .expect("base manifest lookup")
        .expect("base manifest present");
    forged_successor.mode = crate::protocol::TransferMode::Incremental;
    forged_successor.base_sequence = first.sequence;
    forged_successor.head_sequence = first.sequence + 1;
    forged_successor.chunks[0].sequence = SequenceRange {
        from_exclusive: first.sequence,
        to_inclusive: forged_successor.head_sequence,
    };
    forged_successor.terminal_cursor = OpaqueCursor::issue(
        &cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: forged_successor.installation_id,
            account_id: forged_successor.account_id,
            vehicle_id: forged_successor.vehicle_id,
            generation: forged_successor.generation,
            sequence: forged_successor.head_sequence,
        },
    )
    .expect("forged cursor shape");
    assert!(matches!(
        store.publish_manifest(&forged_successor),
        Err(crate::db::StoreError::ImmutableBaseBindingMissing(vehicle_id))
            if vehicle_id == first.vehicle_id
    ));
    assert_eq!(
        store
            .lineage_manifest_for_vehicle(first.vehicle_id)
            .expect("lineage after rejected forged successor"),
        Some(before_forged_successor),
        "rejected full-snapshot relabel must not mutate the published lineage"
    );

    let mut changed = history();
    // Change source history fingerprint by adding a completed drive.
    changed
        .drives
        .push(crate::teslamate_projection::TeslaMateDrive {
            id: 99,
            car_id: 1,
            start_date_ms: 2_000,
            end_date_ms: Some(3_000),
            outside_temp_avg: None,
            speed_max: Some(50),
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_rated_range_km: Some(300.0),
            end_rated_range_km: Some(280.0),
            start_km: Some(10.0),
            end_km: Some(20.0),
            distance_km: Some(10.0),
            duration_min: Some(1),
            start_address_id: None,
            end_address_id: None,
            start_geofence_id: None,
            end_geofence_id: None,
            start_position_id: None,
            end_position_id: None,
            ascent: None,
            descent: None,
            inside_temp_avg: None,
        });
    let second = publish_history(&store, &cursor_key, &request, &changed)
        .expect("changed history must publish as import delta successor");
    assert!(!second.skipped, "changed fingerprint must not skip");
    assert!(second.sequence > first.sequence);
    assert_eq!(second.vehicle_id, first.vehicle_id);
    assert_eq!(
        second.snapshot_id, first.snapshot_id,
        "changed history must keep the immutable base snapshot identity"
    );

    let changed_lineage = store
        .lineage_manifest_for_vehicle(second.vehicle_id)
        .expect("lineage after changed import")
        .expect("changed lineage present");
    changed_lineage
        .validate()
        .expect("changed-history lineage must validate for public retrieval");
    assert_eq!(changed_lineage.base.snapshot_id, first.snapshot_id);
    assert_eq!(changed_lineage.deltas.len(), 1);
    assert_eq!(
        changed_lineage.deltas[0].pack.snapshot_id,
        first.snapshot_id
    );
    assert_eq!(
        changed_lineage.deltas[0].pack.sequence.from_exclusive,
        first.sequence
    );
    assert_eq!(
        changed_lineage.deltas[0].pack.sequence.to_inclusive,
        second.sequence
    );
    assert_eq!(changed_lineage.head_sequence, second.sequence);
    let delta_pack = store
        .pack_for_digest(changed_lineage.deltas[0].pack.sha256)
        .expect("delta pack lookup")
        .expect("delta pack is servable");
    changed_lineage.deltas[0]
        .pack
        .verify_reader(
            File::open(&delta_pack.path).expect("open delta pack"),
            ProtocolLimits::default(),
        )
        .expect("servable delta bytes validate against the published manifest");
    let inspection_path = temporary.path().join("changed-history-delta.sqlite");
    fs::write(
        &inspection_path,
        zstd::stream::decode_all(File::open(delta_pack.path).expect("open delta for inspection"))
            .expect("decode typed delta"),
    )
    .expect("write typed-delta inspection database");
    let inspection = rusqlite::Connection::open(inspection_path).expect("open typed delta");
    let mode: String = inspection
        .query_row(
            "SELECT value FROM hub_pack_metadata WHERE key = 'mode'",
            [],
            |row| row.get(0),
        )
        .expect("delta mode");
    let parent_digest: String = inspection
        .query_row(
            "SELECT value FROM hub_pack_metadata WHERE key = 'parent_digest'",
            [],
            |row| row.get(0),
        )
        .expect("delta parent");
    let changed_drive: i64 = inspection
        .query_row("SELECT id FROM drives", [], |row| row.get(0))
        .expect("changed drive is present in the apply payload");
    assert_eq!(mode, "typed_delta");
    assert_eq!(parent_digest, base_lineage.head_digest.to_string());
    assert_eq!(changed_drive, 99);
    assert_eq!(
        store
            .open()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sync_bases WHERE vehicle_id = ?1",
                rusqlite::params![first.vehicle_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "must not create a second immutable base"
    );

    let third = publish_history(&store, &cursor_key, &request, &changed).unwrap();
    assert!(third.skipped, "identical changed fingerprint skips");
    assert_eq!(third.sequence, second.sequence);
    assert_eq!(third.snapshot_id, first.snapshot_id);

    // Restart: reopen the same store directory and retrieve lineage again.
    let restarted = HubStore::initialize(temporary.path()).unwrap();
    let restarted_lineage = restarted
        .lineage_manifest_for_vehicle(first.vehicle_id)
        .expect("lineage after restart")
        .expect("lineage survives restart");
    restarted_lineage
        .validate()
        .expect("restarted lineage must remain valid");
    assert_eq!(restarted_lineage.base.snapshot_id, first.snapshot_id);
    assert_eq!(restarted_lineage.deltas.len(), 1);
    assert_eq!(restarted_lineage.head_sequence, second.sequence);
}

#[test]
fn changed_history_tombstones_removed_teslamate_rows_without_second_base() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let request = TeslaMateImportRequest {
        source_key: "home-teslamate".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([31; 32]);
    let mut first_history = history();
    first_history.drives.push(completed_drive(44));
    let first = publish_history(&store, &cursor_key, &request, &first_history)
        .expect("first source history publishes a base");
    let base = store
        .lineage_manifest_for_vehicle(first.vehicle_id)
        .expect("base lineage lookup")
        .expect("base lineage");

    let mut rewritten = first_history.clone();
    rewritten.drives.clear();
    let second = publish_history(&store, &cursor_key, &request, &rewritten)
        .expect("removing a published row produces a typed successor");
    assert_eq!(second.snapshot_id, first.snapshot_id);
    assert!(second.sequence > first.sequence);

    let lineage = store
        .lineage_manifest_for_vehicle(first.vehicle_id)
        .expect("rewritten lineage lookup")
        .expect("rewritten lineage");
    lineage
        .validate()
        .expect("tombstone successor remains client-valid lineage");
    assert_eq!(lineage.base.snapshot_id, first.snapshot_id);
    assert_eq!(lineage.deltas.len(), 1);
    let delta_pack = store
        .pack_for_digest(lineage.deltas[0].pack.sha256)
        .expect("delta pack lookup")
        .expect("delta pack exists");
    lineage.deltas[0]
        .pack
        .verify_reader(
            File::open(&delta_pack.path).expect("open delta pack"),
            ProtocolLimits::default(),
        )
        .expect("tombstone delta transport is valid");
    let inspection_path = temporary.path().join("removed-history-delta.sqlite");
    fs::write(
        &inspection_path,
        zstd::stream::decode_all(File::open(delta_pack.path).expect("open delta pack"))
            .expect("decode delta pack"),
    )
    .expect("write inspection pack");
    let inspection = rusqlite::Connection::open(inspection_path).expect("open inspection");
    let tombstones: Vec<(String, i64, i64)> = inspection
        .prepare("SELECT entity, entity_id, car_id FROM tombstones ORDER BY entity, entity_id")
        .expect("prepare tombstone query")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query tombstones")
        .collect::<Result<_, _>>()
        .expect("read tombstones");
    assert_eq!(tombstones, vec![("drive".into(), 44, 1)]);
    assert!(
        store
            .teslamate_import_projection_inventory(first.vehicle_id, first.source_id, 1)
            .expect("current import inventory")
            .rows
            .is_empty()
    );
    assert_eq!(
        store
            .open()
            .expect("open catalogue")
            .query_row(
                "SELECT COUNT(*) FROM sync_bases WHERE vehicle_id = ?1",
                rusqlite::params![first.vehicle_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("base count"),
        1,
        "history rewrites must extend, never replace, the immutable base"
    );
    assert_eq!(base.base.snapshot_id, lineage.base.snapshot_id);
}

#[test]
fn changed_history_without_prior_inventory_fails_before_lineage_mutation() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let request = TeslaMateImportRequest {
        source_key: "home-teslamate".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([32; 32]);
    let mut first_history = history();
    first_history.drives.push(completed_drive(45));
    let first = publish_history(&store, &cursor_key, &request, &first_history)
        .expect("first source history publishes a base");
    let before = store
        .lineage_manifest_for_vehicle(first.vehicle_id)
        .expect("base lineage lookup")
        .expect("base lineage");
    store
        .open()
        .expect("open catalogue")
        .execute(
            "DELETE FROM teslamate_import_projection_heads WHERE vehicle_id = ?1",
            rusqlite::params![first.vehicle_id.to_string()],
        )
        .expect("simulate a pre-inventory legacy base");

    let mut rewritten = first_history;
    rewritten.drives.clear();
    let error = publish_history(&store, &cursor_key, &request, &rewritten)
        .expect_err("a legacy base without exact provenance must fail closed");
    assert!(matches!(
        error,
        TeslaMateImportError::Store(crate::db::StoreError::TeslaMateImportInventoryMissing(id))
            if id == first.vehicle_id
    ));
    assert_eq!(
        store
            .lineage_manifest_for_vehicle(first.vehicle_id)
            .expect("lineage after rejection"),
        Some(before),
        "the failed rewrite must not reserve or publish a successor"
    );
}

#[test]
fn owner_compat_identity_before_teslamate_base_keeps_the_teslamate_binding() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let owner = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            1_700_000_000_000,
        )
        .expect("owner compatibility source");
    let owner_vehicle = store
        .register_vehicle(
            &VehicleDescriptor {
                source_id: owner.source_id,
                source_vehicle_key: "88".into(),
                vin: Some("5YJTESTVIN1234567".into()),
                display_name: Some("Owner compatibility car".into()),
                tesla_eid: Some(88),
                tesla_vid: None,
            },
            1_700_000_000_000,
        )
        .expect("owner compatibility vehicle");
    store
        .upsert_car_settings(
            owner_vehicle.vehicle_id,
            88,
            &crate::hub_pack::ProjectionCarSettings::default(),
        )
        .expect("owner compatibility settings");

    let request = TeslaMateImportRequest {
        source_key: "home-teslamate".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_001,
    };
    let cursor_key = CursorKey::from_bytes([33; 32]);
    let first = publish_history(&store, &cursor_key, &request, &history())
        .expect("TeslaMate base can unify with owner identity");
    assert_eq!(first.vehicle_id, owner_vehicle.vehicle_id);
    let binding = store
        .v2_projection_binding(first.vehicle_id)
        .expect("immutable base binding");
    assert_eq!(binding.account_id, first.source_id);
    assert_eq!(binding.selected_car_id, 1);
    assert_ne!(binding.account_id, owner.source_id);

    let mut changed = history();
    changed.drives.push(completed_drive(46));
    let second = publish_history(&store, &cursor_key, &request, &changed)
        .expect("changed TeslaMate history extends the cross-source base");
    let lineage = store
        .lineage_manifest_for_vehicle(first.vehicle_id)
        .expect("lineage lookup")
        .expect("lineage");
    lineage
        .validate()
        .expect("cross-source successor remains client-valid");
    assert_eq!(second.snapshot_id, first.snapshot_id);
    assert_eq!(lineage.account_id, first.source_id);
    assert_eq!(lineage.deltas.len(), 1);
}

#[test]
fn sealed_stage_publication_never_needs_an_in_memory_history() {
    let data = crate::private_tempdir().unwrap();
    let imports = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(data.path()).unwrap();
    let mut stage = TeslaMateStage::create(
        imports.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 10,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .unwrap();
    let car = history().cars.remove(0);
    stage
        .insert(TeslaMateStageTable::Cars, car.id, &car)
        .unwrap();
    stage.seal().unwrap();

    let report = publish_staged_history(
        &store,
        &CursorKey::from_bytes([9; 32]),
        &TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        },
        &stage,
    )
    .unwrap();
    let manifest = store
        .manifest_for_vehicle(report.vehicle_id)
        .unwrap()
        .expect("published staged manifest");
    assert_eq!(manifest.chunk_count, 1);
    assert_eq!(manifest.total_rows, 1);
    assert_eq!(report.projection, ProjectionReport::default());
    assert_eq!(
        store.selected_tesla_eid().expect("selected imported car"),
        Some((88, crate::hub_pack::ProjectionCarSettings::default()))
    );

    let unchanged = publish_staged_history(
        &store,
        &CursorKey::from_bytes([9; 32]),
        &TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_060_000,
        },
        &stage,
    )
    .unwrap();
    assert_eq!(unchanged.snapshot_id, report.snapshot_id);
    assert_eq!(unchanged.sequence, report.sequence);
    assert_eq!(
        store
            .manifest_for_vehicle(report.vehicle_id)
            .unwrap()
            .unwrap(),
        manifest
    );
}

#[test]
fn staged_unchanged_history_promotes_newer_open_drive_session() {
    let data = crate::private_tempdir().expect("Hub data directory");
    let imports = crate::private_tempdir().expect("staging directory");
    let store = HubStore::initialize(data.path()).expect("Hub store");
    let request = TeslaMateImportRequest {
        source_key: "staged-open-drive".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([70; 32]);
    let mut stage = TeslaMateStage::create(
        imports.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 16,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("create staged source");
    let car = history().cars.remove(0);
    stage
        .insert(TeslaMateStageTable::Cars, car.id, &car)
        .expect("stage selected car");
    stage.seal().expect("seal staged source");

    let mut first_drive = completed_drive(900);
    first_drive.end_date_ms = None;
    let first_session = TeslaMateOpenSession {
        car_id: 1,
        drive: Some(first_drive),
        ..TeslaMateOpenSession::default()
    };
    let base =
        publish_staged_history_with_session(&store, &cursor_key, &request, &stage, &first_session)
            .expect("publish staged base and active drive");
    assert!(!base.skipped);
    assert_eq!(
        store
            .load_imported_open_session(base.source_id, base.vehicle_id)
            .expect("load atomically published active drive"),
        Some(first_session)
    );

    let mut newer_drive = completed_drive(901);
    newer_drive.start_date_ms = 4_000;
    newer_drive.end_date_ms = None;
    let newer_session = TeslaMateOpenSession {
        car_id: 1,
        drive: Some(newer_drive),
        ..TeslaMateOpenSession::default()
    };
    let unchanged_request = TeslaMateImportRequest {
        imported_at_ms: request.imported_at_ms + 1,
        ..request.clone()
    };
    let unchanged = publish_staged_history_with_session(
        &store,
        &cursor_key,
        &unchanged_request,
        &stage,
        &newer_session,
    )
    .expect("promote newer active drive without a new history lineage");
    assert!(
        unchanged.skipped,
        "identical staged history takes fingerprint path"
    );
    assert_eq!(unchanged.snapshot_id, base.snapshot_id);
    assert_eq!(unchanged.sequence, base.sequence);
    assert_eq!(
        store
            .load_imported_open_session(base.source_id, base.vehicle_id)
            .expect("load promoted newer active drive"),
        Some(newer_session)
    );
}

#[test]
fn staged_v21_base_persists_inventory_for_a_changed_history_successor() {
    let data = crate::private_tempdir().expect("Hub data directory");
    let imports = crate::private_tempdir().expect("staging directory");
    let store = HubStore::initialize(data.path()).expect("Hub store");
    let request = TeslaMateImportRequest {
        source_key: "staged-successor".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([71; 32]);
    let mut stage = TeslaMateStage::create(
        imports.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 16,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("create sealed source stage");
    let car = history().cars.remove(0);
    stage
        .insert(TeslaMateStageTable::Cars, car.id, &car)
        .expect("stage selected car");
    stage.seal().expect("seal staged source");

    let base = publish_staged_history(&store, &cursor_key, &request, &stage)
        .expect("publish staged schema-2.1 base");
    assert!(
        store
            .teslamate_import_projection_inventory(base.vehicle_id, base.source_id, 1)
            .expect("staged base inventory")
            .rows
            .is_empty(),
        "the car-only staged base has no source-owned child inventory"
    );
    let prior = store
        .teslamate_import_projection_state_lookup(base.vehicle_id, base.source_id, 1)
        .expect("staged base durable state");
    assert_eq!(prior.header().base_snapshot_id, base.snapshot_id);
    assert_eq!(prior.header().head_sequence, base.sequence);
    drop(prior);

    let mut changed = history();
    changed.drives.push(completed_drive(77));
    let successor = publish_history(&store, &cursor_key, &request, &changed)
        .expect("changed history extends the staged base");
    assert_eq!(successor.snapshot_id, base.snapshot_id);
    assert!(successor.sequence > base.sequence);
    let lineage = store
        .lineage_manifest_for_vehicle(base.vehicle_id)
        .expect("staged successor lineage lookup")
        .expect("staged successor lineage");
    lineage
        .validate()
        .expect("staged successor remains a valid typed lineage");
    assert_eq!(lineage.deltas.len(), 1);
}

#[test]
fn migration_final_snapshot_after_initial_base_is_servable() {
    let data = crate::private_tempdir().expect("Hub data directory");
    let imports = crate::private_tempdir().expect("staging directory");
    let store = HubStore::initialize(data.path()).expect("Hub store");
    let request = TeslaMateImportRequest {
        source_key: "migration-final-snapshot".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([73; 32]);
    let mut stage = TeslaMateStage::create(
        imports.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 16,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("create initial migration stage");
    let car = history().cars.remove(0);
    stage
        .insert(TeslaMateStageTable::Cars, car.id, &car)
        .expect("stage initial selected car");
    stage.seal().expect("seal initial migration stage");
    let initial = publish_staged_history(&store, &cursor_key, &request, &stage)
        .expect("initial migration base");

    let mut final_stage = TeslaMateStage::create(
        imports.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 16,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("create final migration stage");
    let mut final_history = history();
    final_history.drives.push(completed_drive(173));
    let final_car = final_history.cars.remove(0);
    final_stage
        .insert(TeslaMateStageTable::Cars, final_car.id, &final_car)
        .expect("stage final selected car");
    for drive in final_history.drives {
        final_stage
            .insert(TeslaMateStageTable::Drives, drive.id, &drive)
            .expect("stage final drive");
    }
    final_stage.seal().expect("seal final migration stage");
    let mut active_drive = completed_drive(900);
    active_drive.end_date_ms = None;
    let final_session = TeslaMateOpenSession {
        car_id: 1,
        drive: Some(active_drive),
        ..TeslaMateOpenSession::default()
    };
    let final_report = publish_staged_history_with_session(
        &store,
        &cursor_key,
        &request,
        &final_stage,
        &final_session,
    )
    .expect("changed final migration snapshot");
    assert_eq!(final_report.snapshot_id, initial.snapshot_id);
    assert!(final_report.sequence > initial.sequence);
    let persisted_session = store
        .load_imported_open_session(initial.source_id, initial.vehicle_id)
        .expect("load final imported session")
        .expect("final imported session");
    assert_eq!(
        persisted_session.drive.as_ref().map(|row| row.id),
        Some(900)
    );
    assert_eq!(
        persisted_session
            .drive
            .as_ref()
            .and_then(|row| row.end_date_ms),
        None
    );
    assert!(persisted_session.drive_positions.is_empty());
    assert!(persisted_session.charge_samples.is_empty());

    let lineage = store
        .lineage_manifest_for_vehicle(initial.vehicle_id)
        .expect("servable lineage lookup")
        .expect("servable lineage");
    lineage.validate().expect("Serve-compatible lineage");
    assert_eq!(lineage.deltas.len(), 1);
    let delta = &lineage.deltas[0].pack;
    let delta_path = store
        .pack_for_digest(delta.sha256)
        .expect("servable delta lookup")
        .expect("servable delta pack")
        .path;
    delta
        .verify_reader(
            File::open(delta_path).expect("open servable delta"),
            ProtocolLimits::default(),
        )
        .expect("Serve-compatible delta bytes");
}

#[test]
fn staged_v21_base_hands_off_to_a_direct_successor_capture() {
    let data = crate::private_tempdir().expect("Hub data directory");
    let imports = crate::private_tempdir().expect("staging directory");
    let store = HubStore::initialize(data.path()).expect("Hub store");
    let request = TeslaMateImportRequest {
        source_key: "staged-direct-handoff".into(),
        scope: TeslaMateImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([72; 32]);
    let mut stage = TeslaMateStage::create(
        imports.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 16,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("create sealed source stage");
    let car = history().cars.remove(0);
    stage
        .insert(TeslaMateStageTable::Cars, car.id, &car)
        .expect("stage selected car");
    stage.seal().expect("seal staged source");
    let base = publish_staged_history(&store, &cursor_key, &request, &stage)
        .expect("publish staged schema-2.1 base");
    let binding = store
        .v2_projection_binding(base.vehicle_id)
        .expect("staged immutable binding");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("acquire direct handoff gate");
    let run_id = store
        .begin_import_generation(base.source_id, base.vehicle_id, 1, 1_700_000_060_000)
        .expect("begin direct successor generation");
    let mut capture = direct_projection_state_capture(
        &store,
        &publication_gate,
        run_id,
        base.vehicle_id,
        base.source_id,
        1,
        true,
        direct_state_test_limits(),
    )
    .expect("staged base provides the direct successor's durable prior state");
    assert_eq!(
        capture.mode(),
        crate::teslamate_projection_state::TeslaMateProjectionStateCaptureMode::Successor
    );

    let mut changed = history();
    changed.drives.push(completed_drive(78));
    let projected = project_vehicle(&changed, 1).expect("project direct successor source");
    let selected_car = projected.snapshot.cars[0].clone();
    record_projected_direct_state(&mut capture, &projected, 1);
    let state = capture.seal().expect("seal direct successor capture");
    assert_eq!(state.changed_row_count, 1);
    let mut batches = Vec::new();
    direct_delta_rows_from_capture(&mut capture, &binding, &selected_car, 1, |batch| {
        batches.push(batch);
        Ok(())
    })
    .expect("direct successor can emit a sparse typed delta after staged handoff");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].drives, projected.snapshot.drives);

    drop(capture);
    store
        .abort_import_generation(run_id)
        .expect("discard direct handoff test generation");
}

#[test]
fn staged_publication_adapts_before_the_protocol_chunk_ceiling() {
    let data = crate::private_tempdir().unwrap();
    let imports = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(data.path()).unwrap();
    let mut stage = TeslaMateStage::create(
        imports.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 2_000,
            max_stage_bytes: 4 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .unwrap();
    let car = history().cars.remove(0);
    stage
        .insert(TeslaMateStageTable::Cars, car.id, &car)
        .unwrap();
    let base_position = TeslaMatePosition {
        id: 20,
        car_id: 1,
        drive_id: None,
        date_ms: 1_700_000_030_000,
        latitude: 51.5,
        longitude: -0.1,
        elevation: None,
        speed: Some(50),
        power: Some(10.0),
        odometer: None,
        ideal_battery_range_km: None,
        est_battery_range_km: None,
        rated_battery_range_km: Some(390.0),
        battery_level: Some(78),
        usable_battery_level: Some(77),
        fan_status: None,
        driver_temp_setting: None,
        passenger_temp_setting: None,
        is_climate_on: Some(false),
        is_rear_defroster_on: None,
        is_front_defroster_on: None,
        outside_temp: Some(18.0),
        inside_temp: Some(20.0),
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        tpms_pressure_fl: None,
        tpms_pressure_fr: None,
        tpms_pressure_rl: None,
        tpms_pressure_rr: None,
    };
    for id in 20..=1060 {
        let mut position = base_position.clone();
        position.id = id;
        position.date_ms += id;
        stage
            .insert(TeslaMateStageTable::Positions, id, &position)
            .unwrap();
    }
    stage.seal().unwrap();
    let open_session = TeslaMateOpenSession {
        car_id: 1,
        ..TeslaMateOpenSession::default()
    };

    let report = publish_staged_history_with_limits(
        &store,
        &CursorKey::from_bytes([12; 32]),
        &TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        },
        &stage,
        &open_session,
        TeslaMateFragmentLimits {
            max_rows_per_fragment: 3,
            max_projected_json_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let manifest = store
        .manifest_for_vehicle(report.vehicle_id)
        .unwrap()
        .expect("published staged manifest");
    assert_eq!(report.projection.projected_positions, 1_041);
    assert!(manifest.chunk_count < 512);
    assert!(manifest.total_rows > 1_000);
}

#[tokio::test]
async fn native_complete_corpus_publishes_a_durable_manifest_when_configured() {
    let Ok(url) = std::env::var("TESLATLAS_HUB_TEST_POSTGRES_URL") else {
        return;
    };
    let source = ReadOnlySource::parse(&url).expect("credential-free source URL");
    let password =
        TeslaMatePostgresPassword::from_bytes(b"fixture-password").expect("fixture password");
    let data = crate::private_tempdir().expect("Hub data directory");
    let store = HubStore::initialize(data.path()).expect("Hub store");
    let report = import_from_postgres(
        &store,
        &source,
        &password,
        &CursorKey::from_bytes([11; 32]),
        &TeslaMateImportRequest {
            source_key: "native-corpus".into(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: 1_767_225_600_000,
        },
        TeslaMateReadLimits {
            maximum_rows: 32,
            parallel_copy_lanes: 3,
            ..TeslaMateReadLimits::default()
        },
    )
    .await
    .expect("full native publication");

    // car + drive + 3 positions + charging process + charge + state +
    // update + 2 addresses + geofence
    assert_eq!(report.projected_rows, 13);
    let manifest = store
        .manifest_for_vehicle(report.vehicle_id)
        .expect("manifest query")
        .expect("published manifest");
    assert_eq!(manifest.snapshot_id, report.snapshot_id);
    assert_eq!(manifest.total_rows, 13);
    assert!(!manifest.chunks.is_empty());
    assert_eq!(store.repair().expect("Hub repair check").status, "ok");

    let backup_parent = crate::private_tempdir().expect("backup parent");
    let backup_root = backup_parent.path().join("restored-hub");
    store.backup_to(&backup_root).expect("complete backup");
    let restored = HubStore::initialize(&backup_root).expect("restored Hub store");
    restored.quick_check().expect("restored integrity");
    let restored_manifest = restored
        .manifest_for_vehicle(report.vehicle_id)
        .expect("restored manifest query")
        .expect("restored manifest");
    assert_eq!(restored_manifest, manifest);
    for chunk in restored_manifest.chunks {
        assert!(
            restored
                .pack_for_digest(chunk.sha256)
                .expect("restored pack lookup")
                .expect("restored pack")
                .path
                .is_file()
        );
    }
}
