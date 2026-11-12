// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T12: the Hub allocates the rows it collects from its reserved id range,
//! and an id alone says who owns a row. Store side: import seeding, the
//! seeding raise above superseded Hub ids, charge ownership for cost fill and
//! efficiency, the terrain boundary, the window high water and the importer's
//! refusal of a source id in the range. Synthetic data only.

use super::*;
use crate::collection::window::controller::HandbackSource;
use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX, is_hub_id};
use crate::import::source_db::importer::{
    SourceDbImportError, admit_history_source_ids, admit_session_source_ids,
};
use crate::lifecycle::{OpenSessionState, allocate_hub_id};
use crate::source_db_projection::{SourceDbDrive, SourceDbHistory, SourceDbOpenSession};

const SEAM_T0: i64 = 1_760_000_000_000;

/// A registered import source and a vehicle under it.
fn seam_import_vehicle(store: &HubStore, label: &str) -> (SourceRecord, VehicleRecord) {
    let source = store
        .register_source(
            &SourceDescriptor::new(crate::db::IMPORT_SOURCE_KIND, label),
            1_000,
        )
        .expect("import source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, label), 1_001)
        .expect("vehicle");
    (source, vehicle)
}

/// Insert a bare materialised row of the vehicle.
fn seam_put(store: &HubStore, vehicle_id: Uuid, table: &str, column: &str, id: i64) {
    let (extra_columns, extra_values) = match table {
        "materialised_charge_samples" => ("charge_id", "1"),
        "materialised_positions" => ("drive_id, car_id", "NULL, 1"),
        _ => ("car_id", "1"),
    };
    let json_column = match table {
        "materialised_drives" => "drive_json",
        "materialised_charges" => "charge_json",
        "materialised_charge_samples" => "sample_json",
        "materialised_positions" => "position_json",
        "materialised_states" => "state_json",
        "materialised_updates" => "update_json",
        other => panic!("unknown table {other}"),
    };
    store
        .open()
        .expect("connection")
        .execute(
            &format!(
                "INSERT INTO {table}(vehicle_id, {column}, {extra_columns}, {json_column})
                 VALUES (?1, ?2, {extra_values}, '{{}}')"
            ),
            params![vehicle_id.to_string(), id],
        )
        .expect("materialised row");
}

fn seam_state(store: &HubStore, vehicle_id: Uuid) -> OpenSessionState {
    let record = store
        .load_lifecycle_state(vehicle_id)
        .expect("lifecycle read")
        .expect("lifecycle state");
    OpenSessionState::decode(&record.open_session_json).expect("state decode")
}

fn seam_cursors(state: &OpenSessionState) -> [i64; 6] {
    [
        state.next_drive_id,
        state.next_position_id,
        state.next_charge_id,
        state.next_charge_sample_id,
        state.next_state_id,
        state.next_update_id,
    ]
}

/// An imported open session whose watermarks reach far into the source's ids
/// but stay below the range.
fn seam_imported_session(car_id: i64) -> SourceDbOpenSession {
    let mut session = SourceDbOpenSession {
        car_id,
        ..Default::default()
    };
    session.watermarks.drives.max_id = Some(41_000);
    session.watermarks.drives.max_timestamp_ms = Some(SEAM_T0);
    session.watermarks.positions.max_id = Some(11_745_317);
    session.watermarks.positions.max_timestamp_ms = Some(SEAM_T0);
    session.watermarks.charging_processes.max_id = Some(842);
    session.watermarks.charges.max_id = Some(335_316);
    session.watermarks.states.max_id = Some(5_840);
    session.watermarks.updates.max_id = Some(48);
    session
}

#[test]
fn import_seam_collector_allocates_in_the_range_after_an_import() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (source, vehicle) = seam_import_vehicle(&store, "seam-import");
    let v = vehicle.vehicle_id;
    seam_put(&store, v, "materialised_drives", "drive_id", 41_000);
    store
        .seed_imported_open_session(source.source_id, v, 10, &seam_imported_session(10), SEAM_T0)
        .expect("import seeding");
    // Import seeding leaves every cursor at the base: the watermarks are
    // source ids, below the range, and no longer raise a cursor.
    let seeded = seam_state(&store, v);
    assert_eq!(seam_cursors(&seeded), [HUB_ID_BASE; 6]);
    assert!(seeded.id_cursors_in_hub_range());

    // The Owner collection path opens a drive after the import: its id, and
    // every position it writes, is a Hub id.
    let at = SEAM_T0 + 10_000;
    let input = ObservationInput {
        source_id: source.source_id,
        vehicle_id: v,
        observed_at_ms: at,
        payload: serde_json::json!({
            "record_type": "owner_api_vehicle_data_v1", "source_vehicle_state": "online",
            "vehicle_data": {
                "drive_state": {"timestamp": at, "shift_state": "D", "speed": 30,
                                "latitude": 51.0, "longitude": 0.0},
                "vehicle_state": {"timestamp": at},
                "charge_state": {"timestamp": at, "charging_state": "Disconnected"}
            }
        }),
    };
    store
        .accept_owner_observation_and_lifecycle(&input, at, 10)
        .expect("live observation");
    let live = seam_state(&store, v);
    let drive = live.open_drive.as_ref().expect("an open live drive");
    assert_eq!(drive.id, HUB_ID_BASE, "the first live drive takes the base");
    assert_eq!(live.next_drive_id, HUB_ID_BASE + 1);
    let stray: i64 = store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM materialised_positions
             WHERE vehicle_id = ?1 AND NOT (position_id BETWEEN ?2 AND ?3)",
            params![v.to_string(), HUB_ID_BASE, HUB_ID_MAX],
            |row| row.get(0),
        )
        .expect("positions");
    assert_eq!(stray, 0, "every live position is a Hub id");
}

#[test]
fn import_seam_superseded_hub_id_is_never_reused() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (source, vehicle) = seam_import_vehicle(&store, "seam-superseded");
    let v = vehicle.vehicle_id;
    // The store holds Hub drives up to base + 4; the seam superseded the Hub's
    // drive base + 9 (gone from the table, kept in the links) and a Hub
    // position base + 30 that won over a source position.
    seam_put(
        &store,
        v,
        "materialised_drives",
        "drive_id",
        HUB_ID_BASE + 4,
    );
    seam_put(
        &store,
        v,
        "materialised_states",
        "state_id",
        HUB_ID_BASE + 2,
    );
    let connection = store.open().expect("connection");
    connection
        .execute(
            "INSERT INTO import_seam_links(vehicle_id, entity, loser_id, winner_id, decided_seq)
             VALUES (?1, 'drive', ?2, 40999, 1), (?1, 'position', 777, ?3, 1)",
            params![v.to_string(), HUB_ID_BASE + 9, HUB_ID_BASE + 30],
        )
        .expect("links");

    let floors = hub_range_id_floors(&connection, v).expect("floors");
    assert_eq!(floors.drives, HUB_ID_BASE + 9);
    assert_eq!(floors.positions, HUB_ID_BASE + 30);
    assert_eq!(floors.states, HUB_ID_BASE + 2);
    assert_eq!(floors.charges, 0);
    drop(connection);

    // A later import seeds the state again: the cursors rise above every Hub
    // id the store remembers, and the next drive is not base + 9.
    store
        .seed_imported_open_session(source.source_id, v, 10, &seam_imported_session(10), SEAM_T0)
        .expect("import seeding");
    let mut seeded = seam_state(&store, v);
    assert_eq!(seeded.next_drive_id, HUB_ID_BASE + 10);
    assert_eq!(seeded.next_position_id, HUB_ID_BASE + 31);
    assert_eq!(seeded.next_state_id, HUB_ID_BASE + 3);
    assert_eq!(seeded.next_charge_id, HUB_ID_BASE);
    assert_eq!(
        allocate_hub_id(&mut seeded.next_drive_id).expect("drive id"),
        HUB_ID_BASE + 10
    );
}

#[test]
fn import_seam_import_seeding_raises_restored_cursors_into_the_range() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (source, vehicle) = seam_import_vehicle(&store, "seam-restored");
    let v = vehicle.vehicle_id;
    // A restored pre-range state with its sessions closed, already seeded.
    let mut restored = OpenSessionState::new();
    restored.id_cursors_seeded = true;
    restored.next_drive_id = 41_001;
    restored.next_position_id = 900_001;
    restored.next_charge_id = 843;
    restored.next_charge_sample_id = 335_317;
    restored.next_state_id = 5_841;
    restored.next_update_id = 49;
    let mut connection = store.open().expect("connection");
    let transaction = connection.transaction().expect("begin");
    raise_id_cursors_above_watermarks(&transaction, v, &mut restored).expect("raise");
    transaction.commit().expect("commit");
    assert_eq!(seam_cursors(&restored), [HUB_ID_BASE; 6]);
    drop(connection);

    // The same through the import seeding transaction, from a stored state.
    let mut stored = OpenSessionState::new();
    stored.id_cursors_seeded = true;
    stored.next_drive_id = 77;
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO vehicle_lifecycle_state(vehicle_id, car_id, last_observation_id,
                 open_session_json, quarantined, updated_at_ms)
             VALUES (?1, 10, 0, ?2, 0, ?3)",
            params![v.to_string(), stored.encode().expect("encode"), SEAM_T0],
        )
        .expect("restored state");
    store
        .seed_imported_open_session(source.source_id, v, 10, &seam_imported_session(10), SEAM_T0)
        .expect("import seeding");
    assert_eq!(seam_cursors(&seam_state(&store, v)), [HUB_ID_BASE; 6]);
}

#[test]
fn import_seam_inherited_session_keeps_its_id_children_get_hub_ids() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (source, vehicle) = seam_import_vehicle(&store, "seam-inherited");
    let v = vehicle.vehicle_id;
    // TeslaMate stopped mid-drive: the open drive 40999 and its positions
    // carry source ids.
    let mut session = seam_imported_session(10);
    session.drive = Some(
        serde_json::from_value(serde_json::json!({
            "id": 40_999, "car_id": 10, "start_date_ms": SEAM_T0 - 60_000
        }))
        .expect("open drive"),
    );
    session.drive_positions = (0..3)
        .map(|offset| {
            serde_json::from_value(serde_json::json!({
                "id": 900_000 + offset, "car_id": 10, "drive_id": 40_999,
                "date_ms": SEAM_T0 - 60_000 + offset * 20_000,
                "latitude": 51.0, "longitude": 0.001 * offset as f64
            }))
            .expect("open position")
        })
        .collect();
    session.watermarks.positions.max_id = Some(900_002);
    store
        .seed_imported_open_session(source.source_id, v, 10, &session, SEAM_T0)
        .expect("import seeding");
    let seeded = seam_state(&store, v);
    assert_eq!(
        seeded.open_drive.as_ref().map(|drive| drive.id),
        Some(40_999)
    );
    assert_eq!(seam_cursors(&seeded), [HUB_ID_BASE; 6]);

    // Live collection continues the inherited drive: the drive keeps its
    // source id and the positions it adds take Hub ids.
    for step in 1..=2_i64 {
        let at = SEAM_T0 + step * 10_000;
        let input = ObservationInput {
            source_id: source.source_id,
            vehicle_id: v,
            observed_at_ms: at,
            payload: serde_json::json!({
                "record_type": "owner_api_vehicle_data_v1", "source_vehicle_state": "online",
                "vehicle_data": {
                    "drive_state": {"timestamp": at, "shift_state": "D", "speed": 40,
                                    "latitude": 51.0, "longitude": 0.01 * step as f64},
                    "vehicle_state": {"timestamp": at},
                    "charge_state": {"timestamp": at, "charging_state": "Disconnected"}
                }
            }),
        };
        store
            .accept_owner_observation_and_lifecycle(&input, at, 10)
            .expect("live observation");
    }
    let live = seam_state(&store, v);
    assert_eq!(live.open_drive.as_ref().map(|drive| drive.id), Some(40_999));
    assert!(
        live.next_position_id > HUB_ID_BASE,
        "the live positions of the inherited drive were allocated in the range"
    );
    let ids: Vec<i64> = {
        let connection = store.open().expect("connection");
        let mut statement = connection
            .prepare(
                "SELECT position_id FROM materialised_positions
                 WHERE vehicle_id = ?1 AND position_id >= ?2 ORDER BY position_id",
            )
            .expect("positions");
        statement
            .query_map(params![v.to_string(), 900_003_i64], |row| row.get(0))
            .expect("rows")
            .collect::<Result<_, _>>()
            .expect("ids")
    };
    assert!(
        ids.iter().all(|id| is_hub_id(*id)),
        "new positions: {ids:?}"
    );
}

#[test]
fn import_seam_range_exhaustion_refuses() {
    // The last id of the range is handed out once; the next allocation refuses
    // and leaves the cursor where it was.
    let mut cursor = HUB_ID_MAX;
    assert_eq!(allocate_hub_id(&mut cursor), Ok(HUB_ID_MAX));
    assert_eq!(cursor, HUB_ID_MAX + 1);
    assert_eq!(
        allocate_hub_id(&mut cursor),
        Err(crate::lifecycle::LifecycleError::IdentifierExhausted)
    );
    assert_eq!(cursor, HUB_ID_MAX + 1);

    // A live drive cannot open past the range.
    let mut state = OpenSessionState::new();
    state.next_drive_id = HUB_ID_MAX + 1;
    let sample = crate::lifecycle::LifecycleSample {
        observation_id: 1,
        observed_at_ms: SEAM_T0,
        vehicle_state: "online".to_owned(),
        payload: serde_json::json!({
            "record_type": "owner_api_vehicle_data_v1", "source_vehicle_state": "online",
            "vehicle_data": {
                "drive_state": {"timestamp": SEAM_T0, "shift_state": "D", "speed": 30,
                                "latitude": 51.0, "longitude": 0.0},
                "vehicle_state": {"timestamp": SEAM_T0},
                "charge_state": {"timestamp": SEAM_T0, "charging_state": "Disconnected"}
            }
        }),
    };
    assert!(matches!(
        crate::lifecycle::apply_sample(state, 1, &sample),
        Err(crate::lifecycle::LifecycleError::IdentifierExhausted)
    ));
}

#[test]
fn import_seam_cost_fill_and_efficiency_agree() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    // A charge watermark that reaches into the Hub's range, as a later import
    // over Hub ids would record it: ownership still goes by the range.
    fill_import_watermark(&store, &vehicle, "import-seam-agree", HUB_ID_BASE + 100);
    let connection = store.open().expect("connection");
    let bound = charge_import_bound(&connection, v).expect("bound");
    drop(connection);
    for (id, imported) in [
        (37, true),
        (HUB_ID_BASE - 1, true),
        (HUB_ID_BASE, false),
        (HUB_ID_BASE + 100, false),
        (HUB_ID_MAX, false),
    ] {
        assert_eq!(bound.is_imported(id), imported, "charge {id}");
    }

    // Efficiency: three imported sessions at 200.0 and three Hub sessions at
    // 150.0; only the Hub's count.
    eff_put_car(&store, v, Some(162.5));
    for id in 37..=39 {
        eff_put(&store, v, id, 32.00);
    }
    for id in HUB_ID_BASE + 1..=HUB_ID_BASE + 3 {
        eff_put(&store, v, id, 24.00);
    }
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(150.0));

    // Cost fill: the same rule prices the Hub's row and leaves the imported one.
    fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
    let samples = [
        (0, Some(0.00)),
        (5 * COST_MIN, Some(4.50)),
        (10 * COST_MIN, Some(9.00)),
    ];
    for id in [30, HUB_ID_BASE + 4] {
        fill_put_row(
            &store,
            v,
            &fill_row(id, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
        );
        fill_put_samples(&store, v, id, &samples);
    }
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        1
    );
    assert_eq!(fill_read(&store, v, HUB_ID_BASE + 4).cost, Some(3.00));
    assert_eq!(fill_read(&store, v, 30).cost, None);
}

#[test]
fn import_seam_terrain_boundary_uses_the_range() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    // A positions watermark past the range still leaves the boundary below it.
    let import = store
        .register_source(
            &SourceDescriptor::new(crate::db::IMPORT_SOURCE_KIND, "import-seam-terrain"),
            1_000,
        )
        .expect("import source");
    insert_watermark(&store, &import, &vehicle, "positions", Some(HUB_ID_MAX + 7));
    let connection = store.open().expect("connection");
    let watermark = terrain_watermark_boundary(&connection, v).expect("watermark boundary");
    assert_eq!(watermark.max_position_id, HUB_ID_BASE - 1);
    let merged = terrain_merged_boundary(
        ImportBoundary {
            max_position_id: HUB_ID_BASE + 5,
            max_date_ms: SEAM_T0,
        },
        ImportBoundary {
            max_position_id: 10,
            max_date_ms: SEAM_T0,
        },
    );
    assert_eq!(merged.max_position_id, HUB_ID_BASE - 1);

    // Against a boundary that claims every id and date, a Hub-range position
    // still qualifies and a source position does not.
    seam_put(&store, v, "materialised_drives", "drive_id", HUB_ID_BASE);
    let everything = ImportBoundary {
        max_position_id: i64::MAX,
        max_date_ms: i64::MAX,
    };
    let mut eligibility = HashMap::new();
    let hub = r88_position(HUB_ID_BASE + 1, Some(HUB_ID_BASE), SEAM_T0);
    let source = r88_position(500, Some(HUB_ID_BASE), SEAM_T0);
    assert!(
        terrain_qualifies(
            &connection,
            v,
            Some(HUB_ID_BASE),
            &hub,
            everything,
            &mut eligibility
        )
        .expect("hub position")
    );
    assert!(
        !terrain_qualifies(
            &connection,
            v,
            Some(HUB_ID_BASE),
            &source,
            everything,
            &mut eligibility
        )
        .expect("source position")
    );
}

#[test]
fn import_seam_high_water_reads_the_range() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (_source, first) = seam_import_vehicle(&store, "seam-high-water-1");
    let (_source, second) = seam_import_vehicle(&store, "seam-high-water-2");
    // Imported rows keep source ids; Hub rows are inserted out of id order, so
    // the latest rowid is not the largest id.
    seam_put(
        &store,
        first.vehicle_id,
        "materialised_drives",
        "drive_id",
        41_000,
    );
    seam_put(
        &store,
        first.vehicle_id,
        "materialised_drives",
        "drive_id",
        HUB_ID_BASE + 7,
    );
    seam_put(
        &store,
        first.vehicle_id,
        "materialised_drives",
        "drive_id",
        HUB_ID_BASE + 2,
    );
    seam_put(
        &store,
        second.vehicle_id,
        "materialised_drives",
        "drive_id",
        HUB_ID_BASE + 9,
    );
    seam_put(
        &store,
        second.vehicle_id,
        "materialised_drives",
        "drive_id",
        3,
    );
    seam_put(
        &store,
        first.vehicle_id,
        "materialised_charges",
        "charge_id",
        HUB_ID_BASE,
    );
    seam_put(
        &store,
        first.vehicle_id,
        "materialised_positions",
        "position_id",
        11_745_317,
    );
    drop(store);

    let high_water =
        crate::runtime::operator::authority::StoreHighWater::new(directory.path()).high_water();
    assert_eq!(high_water.drives, Some(HUB_ID_BASE + 9));
    assert_eq!(high_water.charges, Some(HUB_ID_BASE));
    assert_eq!(
        high_water.positions, None,
        "imported positions are no high water"
    );
    assert!(high_water.drives.is_some_and(is_hub_id));
}

#[test]
fn import_seam_source_id_at_the_base_is_refused() {
    // The last source id below the base is admitted; the base is refused,
    // naming the table and the id, on every admission path.
    let mut session = seam_imported_session(1);
    session.watermarks.positions.max_id = Some(HUB_ID_BASE - 1);
    admit_session_source_ids(&session).expect("ids below the base are admitted");
    session.watermarks.charges.max_id = Some(HUB_ID_BASE);
    match admit_session_source_ids(&session) {
        Err(SourceDbImportError::InvalidImportGeneration { table, id }) => {
            assert_eq!((table, id), ("charges", HUB_ID_BASE));
        }
        other => panic!("the base must be refused: {other:?}"),
    }

    let drive = |id: i64| -> SourceDbDrive {
        serde_json::from_value(serde_json::json!({
            "id": id, "car_id": 1, "start_date_ms": SEAM_T0
        }))
        .expect("drive row")
    };
    let mut history = SourceDbHistory {
        cars: Vec::new(),
        drives: vec![drive(1), drive(HUB_ID_BASE - 1)],
        positions: Vec::new(),
        charging_processes: Vec::new(),
        charges: Vec::new(),
        addresses: Vec::new(),
        geofences: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
    };
    admit_history_source_ids(&history).expect("below the base");
    history.drives.push(drive(HUB_ID_BASE));
    let error = admit_history_source_ids(&history).expect_err("the base");
    assert!(matches!(
        error,
        SourceDbImportError::InvalidImportGeneration { table: "drives", id } if id == HUB_ID_BASE
    ));
    assert!(
        error
            .to_string()
            .contains(&format!("drives id {HUB_ID_BASE}"))
    );

    // The whole-history publication refuses before it writes anything.
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let count_sources = || -> i64 {
        store
            .open()
            .expect("connection")
            .query_row("SELECT COUNT(*) FROM sources", [], |row| row.get(0))
            .expect("sources")
    };
    let sources_before = count_sources();
    let request = crate::import::source_db::importer::SourceDbImportRequest {
        new_import_source: false,
        source_key: "seam-refusal".into(),
        scope: crate::import::source_db::importer::SourceDbImportScope::Selected(1),
        imported_at_ms: SEAM_T0,
    };
    let refused = crate::import::source_db::importer::publish_history(
        &store,
        &crate::protocol::CursorKey::from_bytes([12; 32]),
        &request,
        &history,
    );
    assert!(matches!(
        refused,
        Err(SourceDbImportError::InvalidImportGeneration {
            table: "drives",
            ..
        })
    ));
    assert_eq!(count_sources(), sources_before, "nothing was registered");
}
