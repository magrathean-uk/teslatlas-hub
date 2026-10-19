// SPDX-License-Identifier: AGPL-3.0-only

// C1 B2 amendment B (R88): bounded memory when a session closes. Spec tests
// ST100 to ST102 (ST103 is the collector's count, in its own tests). Synthetic
// data only, at most 3,001 child rows, never a live or imported database.

const R88_T0: i64 = 1_760_000_000_000;

fn r88_position(id: i64, drive_id: Option<i64>, date_ms: i64) -> crate::hub_pack::ProjectionPosition {
    serde_json::from_value(serde_json::json!({
        "id": id, "drive_id": drive_id, "car_id": 1, "date_ms": date_ms,
        "latitude": 10.0 + 0.0001 * (id % 1000) as f64, "longitude": 20.0
    }))
    .expect("position row")
}

fn r88_sample(id: i64, charge_id: i64, timestamp_ms: i64) -> crate::hub_pack::ProjectionChargeSample {
    serde_json::from_value(serde_json::json!({
        "id": id, "charge_process_id": charge_id, "timestamp_ms": timestamp_ms,
        "battery_level": 50, "charge_energy_added_kwh": 0.1 * (id % 1000) as f64,
        "charger_power_kw": 7.0
    }))
    .expect("sample row")
}

fn r88_drive_row(id: i64, start_ms: i64, end_ms: i64) -> crate::hub_pack::ProjectionDrive {
    serde_json::from_value(serde_json::json!({
        "id": id, "car_id": 1, "start_date_ms": start_ms, "end_date_ms": end_ms
    }))
    .expect("drive row")
}

fn r88_charge_row(id: i64, start_ms: i64, end_ms: i64) -> crate::hub_pack::ProjectionCharge {
    serde_json::from_value(serde_json::json!({
        "id": id, "car_id": 1, "start_date_ms": start_ms, "end_date_ms": end_ms
    }))
    .expect("charge row")
}

/// Everything a close wrote, in a form that does not depend on the vehicle id.
#[derive(Debug, PartialEq)]
struct CloseTables {
    positions: Vec<(i64, Option<i64>, String)>,
    drives: Vec<(i64, String)>,
    samples: Vec<(i64, i64, String)>,
    /// Entity, entity id, operation and payload, in sequence order.
    mutations: Vec<(String, i64, String, String)>,
}

fn read_close_tables(store: &HubStore, vehicle: &VehicleRecord) -> CloseTables {
    let connection = store.open().expect("connection");
    let key = vehicle.vehicle_id.to_string();
    let positions = connection
        .prepare(
            "SELECT position_id, drive_id, position_json FROM materialised_positions
             WHERE vehicle_id = ?1 ORDER BY position_id",
        )
        .unwrap()
        .query_map(params![key], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let drives = connection
        .prepare(
            "SELECT drive_id, drive_json FROM materialised_drives
             WHERE vehicle_id = ?1 ORDER BY drive_id",
        )
        .unwrap()
        .query_map(params![key], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let samples = connection
        .prepare(
            "SELECT sample_id, charge_id, sample_json FROM materialised_charge_samples
             WHERE vehicle_id = ?1 ORDER BY sample_id",
        )
        .unwrap()
        .query_map(params![key], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mutations = connection
        .prepare(
            "SELECT entity, entity_id, operation, payload_json FROM sync_mutations
             WHERE vehicle_id = ?1 ORDER BY revision",
        )
        .unwrap()
        .query_map(params![key], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    CloseTables { positions, drives, samples, mutations }
}

fn r88_open_rows_left(store: &HubStore, vehicle: &VehicleRecord) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("open rows")
}

struct CloseRun {
    tables: CloseTables,
    counts: (usize, usize),
    stats: ClosePageStats,
    open_rows_left: i64,
}

fn r88_open_drive_state() -> Vec<u8> {
    let mut state = crate::lifecycle::OpenSessionState::new();
    state.open_drive = Some(
        serde_json::from_value(serde_json::json!({
            "id": 1, "car_id": 1, "start_date_ms": R88_T0,
            "outside_temp_sum": 0.0, "outside_temp_count": 0
        }))
        .expect("open drive"),
    );
    state.encode().expect("state")
}

fn r88_open_charge_state() -> Vec<u8> {
    let mut state = crate::lifecycle::OpenSessionState::new();
    state.open_charge = Some(
        serde_json::from_value(serde_json::json!({
            "id": 7, "car_id": 1, "start_date_ms": R88_T0,
            "outside_temp_sum": 0.0, "outside_temp_count": 0
        }))
        .expect("open charge"),
    );
    state.encode().expect("state")
}

/// Commit 1 stores the children of an open drive; commit 2 closes it (or
/// discards it) with the given in-delta rows and the given page size.
fn r88_close_drive(
    children: &[crate::hub_pack::ProjectionPosition],
    in_delta: Vec<crate::hub_pack::ProjectionPosition>,
    end_ms: i64,
    discard: bool,
    page_rows: usize,
) -> CloseRun {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let open_state = r88_open_drive_state();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &open_state,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 1_000,
            delta: &crate::lifecycle::LifecycleDelta {
                open_drive_positions: children.to_vec(),
                ..Default::default()
            },
        })
        .expect("commit 1");
    assert_eq!(r88_open_rows_left(&store, &vehicle), children.len() as i64);
    let closed_state = crate::lifecycle::OpenSessionState::new().encode().expect("state");
    let mut delta = crate::lifecycle::LifecycleDelta {
        positions: in_delta,
        ..Default::default()
    };
    if discard {
        delta.discarded_drive_ids = vec![1];
    } else {
        delta.drives = vec![r88_drive_row(1, R88_T0, end_ms)];
    }
    let (positions, samples, stats) = store
        .commit_lifecycle_delta_with_page_rows(
            &LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &closed_state,
                last_observation_id: 2,
                quarantined: false,
                updated_at_ms: 2_000,
                delta: &delta,
            },
            page_rows,
        )
        .expect("commit 2");
    CloseRun {
        tables: read_close_tables(&store, &vehicle),
        counts: (positions, samples),
        stats,
        open_rows_left: r88_open_rows_left(&store, &vehicle),
    }
}

fn st100_children() -> Vec<crate::hub_pack::ProjectionPosition> {
    (101..=113)
        .map(|id| r88_position(id, Some(1), R88_T0 + 10_000 * (id - 101)))
        .collect()
}

fn st100_in_delta() -> Vec<crate::hub_pack::ProjectionPosition> {
    vec![
        r88_position(120, None, R88_T0 + 130_000),
        // The same row as stored child 107.
        r88_position(107, Some(1), R88_T0 + 10_000 * 6),
    ]
}

fn st100_mutation_entities(run: &CloseRun) -> Vec<(String, i64)> {
    run.tables
        .mutations
        .iter()
        .map(|(entity, id, _, _)| (entity.clone(), *id))
        .collect()
}

#[test]
fn st100_a_closing_drive_reads_its_children_in_pages_with_the_same_rows_as_one_read() {
    let end = R88_T0 + 100_000;
    let small = r88_close_drive(&st100_children(), st100_in_delta(), end, false, 4);
    let large = r88_close_drive(&st100_children(), st100_in_delta(), end, false, 13);
    for run in [&small, &large] {
        assert_eq!(run.counts, (14, 0));
        assert_eq!(run.open_rows_left, 0);
        let placed = run
            .tables
            .positions
            .iter()
            .map(|(id, drive, _)| (*id, *drive))
            .collect::<Vec<_>>();
        let mut expected = (101..=111).map(|id| (id, Some(1))).collect::<Vec<_>>();
        expected.extend([(112, None), (113, None), (120, None)]);
        assert_eq!(placed, expected, "107 appears once, as the delta's row");
        // The drive's mutation, then the delta rows 120 and 107, then the
        // stored children in id order without 107.
        let mut order = vec![("drive".to_owned(), 1), ("position".to_owned(), 120), ("position".to_owned(), 107)];
        order.extend((101..=106).chain(108..=113).map(|id| ("position".to_owned(), id)));
        assert_eq!(st100_mutation_entities(run), order);
    }
    assert_eq!(small.tables, large.tables, "rows and mutations do not depend on the page size");
    assert_eq!(small.stats, ClosePageStats { pages: 4, largest_page: 4 });
    assert_eq!(large.stats, ClosePageStats { pages: 1, largest_page: 13 });
}

#[test]
fn st100_variant_a_discarded_drive_places_every_child_and_delta_row_outside_it() {
    let end = R88_T0 + 100_000;
    let small = r88_close_drive(&st100_children(), st100_in_delta(), end, true, 4);
    let large = r88_close_drive(&st100_children(), st100_in_delta(), end, true, 13);
    for run in [&small, &large] {
        assert_eq!(run.counts, (14, 0));
        assert_eq!(run.open_rows_left, 0);
        assert!(run.tables.drives.is_empty());
        assert!(run.tables.positions.iter().all(|(_, drive, _)| drive.is_none()));
        assert_eq!(run.tables.positions.len(), 14);
        let mut order = vec![("position".to_owned(), 120), ("position".to_owned(), 107)];
        order.extend((101..=106).chain(108..=113).map(|id| ("position".to_owned(), id)));
        assert_eq!(st100_mutation_entities(run), order);
    }
    assert_eq!(small.tables, large.tables);
    assert_eq!(small.stats, ClosePageStats { pages: 4, largest_page: 4 });
    assert_eq!(large.stats, ClosePageStats { pages: 1, largest_page: 13 });
}

/// Commit 1 stores the samples of an open charge; commit 2 closes it.
fn r88_close_charge(page_rows: usize, in_delta: Vec<crate::hub_pack::ProjectionChargeSample>) -> CloseRun {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let open_state = r88_open_charge_state();
    let samples = (201..=213)
        .map(|id| {
            let mut sample = r88_sample(id, 7, R88_T0 + 60_000 * (id - 201));
            sample.charge_energy_added_kwh = Some(0.1 * (id - 200) as f64);
            sample
        })
        .collect::<Vec<_>>();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &open_state,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 1_000,
            delta: &crate::lifecycle::LifecycleDelta {
                open_charge_samples: samples,
                ..Default::default()
            },
        })
        .expect("commit 1");
    let closed_state = crate::lifecycle::OpenSessionState::new().encode().expect("state");
    let delta = crate::lifecycle::LifecycleDelta {
        charges: vec![r88_charge_row(7, R88_T0, R88_T0 + 12 * 60_000)],
        charge_samples: in_delta,
        ..Default::default()
    };
    let (positions, samples, stats) = store
        .commit_lifecycle_delta_with_page_rows(
            &LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &closed_state,
                last_observation_id: 2,
                quarantined: false,
                updated_at_ms: 2_000,
                delta: &delta,
            },
            page_rows,
        )
        .expect("commit 2");
    CloseRun {
        tables: read_close_tables(&store, &vehicle),
        counts: (positions, samples),
        stats,
        open_rows_left: r88_open_rows_left(&store, &vehicle),
    }
}

#[test]
fn st101_a_closing_charge_reads_its_samples_in_pages_with_the_same_rows_as_one_read() {
    let small = r88_close_charge(4, Vec::new());
    let large = r88_close_charge(13, Vec::new());
    for run in [&small, &large] {
        assert_eq!(run.counts, (0, 13));
        assert_eq!(run.open_rows_left, 0);
        let ids = run
            .tables
            .samples
            .iter()
            .map(|(id, charge, _)| (*id, *charge))
            .collect::<Vec<_>>();
        assert_eq!(ids, (201..=213).map(|id| (id, 7)).collect::<Vec<_>>());
        // The sample mutations follow the charge's, in id order.
        let mut order = vec![("charge".to_owned(), 7)];
        order.extend((201..=213).map(|id| ("charge_sample".to_owned(), id)));
        let seen = st100_mutation_entities(run)
            .into_iter()
            .filter(|(entity, _)| entity == "charge" || entity == "charge_sample")
            .collect::<Vec<_>>();
        assert_eq!(seen, order);
    }
    assert_eq!(small.tables, large.tables);
    assert_eq!(small.stats, ClosePageStats { pages: 4, largest_page: 4 });
    assert_eq!(large.stats, ClosePageStats { pages: 1, largest_page: 13 });
}

#[test]
fn st102_the_production_page_is_one_thousand_rows_and_gives_the_rows_of_one_read() {
    assert_eq!(crate::lifecycle::K_CLOSE_PAGE_ROWS, 1_000);
    let children = (1..=3_001)
        .map(|id| r88_position(id, Some(1), R88_T0 + 1_000 * (id - 1)))
        .collect::<Vec<_>>();
    let end = R88_T0 + 3_000_000;
    let production = r88_close_drive(&children, Vec::new(), end, false, crate::lifecycle::K_CLOSE_PAGE_ROWS);
    let whole = r88_close_drive(&children, Vec::new(), end, false, 3_001);
    assert_eq!(production.counts, (3_001, 0));
    assert_eq!(production.open_rows_left, 0);
    assert_eq!(production.stats, ClosePageStats { pages: 4, largest_page: 1_000 });
    assert_eq!(whole.stats, ClosePageStats { pages: 1, largest_page: 3_001 });
    assert_eq!(production.tables, whole.tables);
    assert!(production.tables.positions.iter().all(|(_, drive, _)| *drive == Some(1)));
}

#[test]
fn st101_variant_a_stored_sample_already_in_the_delta_is_written_once_and_first() {
    // Sample 207 is both an in-delta row and a stored child.
    let in_delta = |id: i64| {
        let mut sample = r88_sample(id, 7, R88_T0 + 60_000 * (id - 201));
        sample.charge_energy_added_kwh = Some(0.1 * (id - 200) as f64);
        sample
    };
    let small = r88_close_charge(4, vec![in_delta(207)]);
    let large = r88_close_charge(13, vec![in_delta(207)]);
    for run in [&small, &large] {
        assert_eq!(run.counts, (0, 13));
        assert_eq!(run.tables.samples.len(), 13);
        let order = st100_mutation_entities(run)
            .into_iter()
            .filter(|(entity, _)| entity == "charge_sample")
            .map(|(_, id)| id)
            .collect::<Vec<_>>();
        let mut expected = vec![207];
        expected.extend((201..=213).filter(|id| *id != 207));
        assert_eq!(order, expected);
    }
    assert_eq!(small.tables, large.tables);
}

/// C2 R46 step 1 (Codex R10): the close folds the stored open samples and the delta's
/// own into the facts the cost rules read, one stored row at a time, and a stored
/// sample the delta carries again counts as the delta has it. The row's energy added
/// is 1.0 kWh over 10 kWh used at 0.30 plus a session fee of 1.00. A highest counter
/// of at least 1.0 plus the counter step proves the row continues the plug-in, which
/// waives the fee (3.00); otherwise the fee is charged (4.00). Stored sample 207
/// carries `stored_207`, every other stored sample 0.5.
fn r46_cost_after_closing_with(
    stored_207: f64,
    in_delta: Vec<crate::hub_pack::ProjectionChargeSample>,
) -> Option<f64> {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    store
        .save_geofence(
            vehicle.vehicle_id,
            Some(3),
            crate::teslamate_projection::TeslaMateGeofence {
                id: 0,
                name: "F3".to_owned(),
                latitude: Some(10.0),
                longitude: Some(20.0),
                radius_m: Some(150.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
                cost_per_unit: Some(0.30),
                session_fee: Some(1.00),
            },
        )
        .expect("fence");
    let samples = (201..=213)
        .map(|id| {
            let mut sample = r88_sample(id, 7, R88_T0 + 60_000 * (id - 201));
            sample.charge_energy_added_kwh = Some(if id == 207 { stored_207 } else { 0.5 });
            sample
        })
        .collect::<Vec<_>>();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &r88_open_charge_state(),
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 1_000,
            delta: &crate::lifecycle::LifecycleDelta {
                open_charge_samples: samples,
                ..Default::default()
            },
        })
        .expect("commit 1");
    let mut charge = r88_charge_row(7, R88_T0, R88_T0 + 12 * 60_000);
    charge.charge_energy_added = Some(1.0);
    charge.charge_energy_used_kwh = Some(10.0);
    let closed_state = crate::lifecycle::OpenSessionState::new().encode().expect("state");
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &closed_state,
            last_observation_id: 2,
            quarantined: false,
            updated_at_ms: 2_000,
            delta: &crate::lifecycle::LifecycleDelta {
                charges: vec![charge],
                charge_samples: in_delta,
                charge_start_coordinates: vec![(7, 10.0, 20.0)],
                ..Default::default()
            },
        })
        .expect("commit 2");
    let json: String = store
        .open()
        .expect("connection")
        .query_row(
            "SELECT charge_json FROM materialised_charges
             WHERE vehicle_id = ?1 AND charge_id = 7",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("closed charge");
    serde_json::from_str::<crate::hub_pack::ProjectionCharge>(&json)
        .expect("charge")
        .cost
}

#[test]
fn r46_the_close_folds_stored_and_delta_samples_by_id_with_the_delta_first() {
    let sample = |id: i64, counter: f64| {
        let mut sample = r88_sample(id, 7, R88_T0 + 60_000 * (id - 201));
        sample.charge_energy_added_kwh = Some(counter);
        sample
    };
    // Only the stored samples count: a high stored counter proves the continuation.
    assert_eq!(r46_cost_after_closing_with(5.0, Vec::new()), Some(3.00));
    assert_eq!(r46_cost_after_closing_with(0.5, Vec::new()), Some(4.00));
    // The delta carries sample 207 again with a low counter: the delta's version
    // counts, the stored one does not, so the fee is charged.
    assert_eq!(
        r46_cost_after_closing_with(5.0, vec![sample(207, 0.5)]),
        Some(4.00)
    );
    // The delta's own, new sample counts too: it proves the continuation.
    assert_eq!(
        r46_cost_after_closing_with(0.5, vec![sample(214, 5.0)]),
        Some(3.00)
    );
}
