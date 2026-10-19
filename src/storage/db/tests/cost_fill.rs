// SPDX-License-Identifier: AGPL-3.0-only

// Fill and manual entry through the store (C2-P4): the retroactive fill of
// R49 to R51 and the manual cost of R52 to R55, with the logs of R7 and R59
// and the vehicle scoping of R8. Spec tests T35, T35b to T35e, T36, T37 to T43
// (their store half; the CLI half is in `charge_cost_cli_tests.rs`), T44, T45
// and T46. Synthetic data only. The rows are written the way a close or an
// import leaves them (`cost_row`, `cost_sample` of `cost_close.rs`), and the
// lifecycle sequences are the spec's `Ob` and `A7`.

/// One tariff-less location inside fence 3 only: 55 m north of its centre
/// (10.0, 20.0, radius 150 m), outside the small fence 4 of the tests.
const FILL_IN_FENCE_3: (f64, f64) = (10.0005, 20.0);

/// A location inside fence 3 and inside the smaller fence 4 (centre
/// 10.0010, 20.0, radius 20 m): 111 m from fence 3's centre.
const FILL_IN_FENCE_3_AND_4: (f64, f64) = (10.0010, 20.0);

/// A location outside every fence of the tests.
const FILL_OUTSIDE: (f64, f64) = (11.0, 21.0);

/// A stored row at `place`, as a close leaves it (no label, copy or cost).
fn fill_row(
    id: i64,
    place: (f64, f64),
    used: Option<f64>,
    added: Option<f64>,
) -> crate::hub_pack::ProjectionCharge {
    let mut row = cost_row(id, used, added, Some(false), None);
    row.start_latitude = Some(place.0);
    row.start_longitude = Some(place.1);
    row
}

/// Insert a stored row of any vehicle.
fn fill_put_row(store: &HubStore, vehicle_id: Uuid, row: &crate::hub_pack::ProjectionCharge) {
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, ?2, 1, ?3)",
            params![
                vehicle_id.to_string(),
                row.id,
                serde_json::to_string(row).expect("json")
            ],
        )
        .expect("insert row");
}

/// Insert the stored samples of a row: `(offset from the row's start in ms,
/// counter)` pairs. Sample ids are `charge_id * 100 + position`, unique per
/// vehicle.
fn fill_put_samples(
    store: &HubStore,
    vehicle_id: Uuid,
    charge_id: i64,
    samples: &[(i64, Option<f64>)],
) {
    let connection = store.open().expect("connection");
    for (position, (offset_ms, counter)) in samples.iter().enumerate() {
        let sample = cost_sample(
            charge_id * 100 + position as i64,
            charge_id,
            *offset_ms,
            *counter,
        );
        connection
            .execute(
                "INSERT INTO materialised_charge_samples(
                    vehicle_id, sample_id, charge_id, sample_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    vehicle_id.to_string(),
                    sample.id,
                    charge_id,
                    serde_json::to_string(&sample).expect("json")
                ],
            )
            .expect("insert sample");
    }
}

/// Insert a fence as stored, with any tariff or none, and any radius.
fn fill_put_fence(
    store: &HubStore,
    vehicle_id: Uuid,
    id: i64,
    centre: (f64, f64),
    radius_m: f64,
    tariff: Option<(crate::hub_pack::GeofenceBillingType, f64, Option<f64>)>,
) {
    store
        .upsert_geofences(
            vehicle_id,
            &[crate::teslamate_projection::TeslaMateGeofence {
                id,
                name: format!("Fence {id}"),
                latitude: Some(centre.0),
                longitude: Some(centre.1),
                radius_m: Some(radius_m),
                billing_type: tariff.map(|(billing, _, _)| billing),
                cost_per_unit: tariff.map(|(_, rate, _)| rate),
                session_fee: tariff.and_then(|(_, _, fee)| fee),
            }],
        )
        .expect("fence");
}

fn fill_read(store: &HubStore, vehicle_id: Uuid, id: i64) -> crate::hub_pack::ProjectionCharge {
    store
        .materialised_charge_for_vehicle(vehicle_id, id)
        .expect("read")
        .unwrap_or_else(|| panic!("row {id}"))
}

fn fill_mutations(store: &HubStore, vehicle_id: Uuid, id: i64) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations
             WHERE vehicle_id = ?1 AND entity = 'charge' AND entity_id = ?2",
            params![vehicle_id.to_string(), id],
            |row| row.get(0),
        )
        .expect("count")
}

/// `export_outbox.dirty_revision` of the vehicle, 0 when it has no row.
fn fill_export_revision(store: &HubStore, vehicle_id: Uuid) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT dirty_revision FROM export_outbox WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .expect("outbox")
        .unwrap_or(0)
}

fn fill_copy(
    row: &crate::hub_pack::ProjectionCharge,
) -> (
    Option<crate::hub_pack::GeofenceBillingType>,
    Option<f64>,
    Option<f64>,
) {
    (row.billing_type, row.cost_per_unit, row.session_fee)
}

fn fill_store() -> (tempfile::TempDir, HubStore, SourceRecord, VehicleRecord) {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    (directory, store, source, vehicle)
}

fn fill_import_watermark(
    store: &HubStore,
    vehicle: &VehicleRecord,
    label: &str,
    max_id: i64,
) -> SourceRecord {
    let import = store
        .register_source(&SourceDescriptor::new("teslamate", label), 1_000)
        .expect("import source");
    insert_watermark(store, &import, vehicle, "charging_processes", Some(max_id));
    import
}

#[test]
fn t35_the_fill_prices_only_unknown_hub_costs_in_the_asked_fence() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_import_watermark(&store, &vehicle, "import-t35", 10);
    fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
    // Fence 4 sits inside fence 3 and has no tariff.
    fill_put_fence(&store, v, 4, (10.0010, 20.0), 20.0, None);

    // A: the one that is filled.
    fill_put_row(
        &store,
        v,
        &fill_row(11, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
    );
    fill_put_samples(
        &store,
        v,
        11,
        &[
            (0, Some(0.00)),
            (5 * COST_MIN, Some(4.50)),
            (10 * COST_MIN, Some(9.00)),
        ],
    );
    // B: a known cost.
    let mut priced = fill_row(12, FILL_IN_FENCE_3, Some(10.00), Some(9.00));
    priced.cost = Some(5.00);
    fill_put_row(&store, v, &priced);
    // C: imported (id at or below the watermark).
    fill_put_row(
        &store,
        v,
        &fill_row(5, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
    );
    // D: outside every fence.
    fill_put_row(
        &store,
        v,
        &fill_row(13, FILL_OUTSIDE, Some(10.00), Some(9.00)),
    );
    // E: inside fence 3 and inside the smaller, tariff-less fence 4.
    fill_put_row(
        &store,
        v,
        &fill_row(14, FILL_IN_FENCE_3_AND_4, Some(10.00), Some(9.00)),
    );
    // F: no energy to price.
    fill_put_row(&store, v, &fill_row(15, FILL_IN_FENCE_3, None, Some(9.00)));
    let before_others = [12, 5, 13, 14, 15].map(|id| fill_read(&store, v, id));

    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        1
    );
    let filled = fill_read(&store, v, 11);
    assert_eq!(filled.cost, Some(3.00));
    assert_eq!(fill_copy(&filled), (Some(kwh()), Some(0.30), None));
    assert_eq!(filled.geofence, None, "the label is C8's, not the fill's");
    for (id, before) in [12, 5, 13, 14, 15].into_iter().zip(before_others) {
        assert_eq!(fill_read(&store, v, id), before, "row {id} must not change");
        assert_eq!(fill_mutations(&store, v, id), 0, "row {id}");
    }
    assert_eq!(fill_mutations(&store, v, 11), 1);
    assert_eq!(fill_export_revision(&store, v), 1);

    // A second identical call changes nothing and writes nothing.
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("again"),
        0
    );
    assert_eq!(fill_mutations(&store, v, 11), 1);
    assert_eq!(fill_export_revision(&store, v), 1);
}

/// Insert rows `1..=count` outside every fence (so the fill passes over them),
/// in one transaction.
fn fill_put_filler_rows(store: &HubStore, vehicle_id: Uuid, count: i64) {
    let mut connection = store.open().expect("connection");
    let transaction = connection.transaction().expect("begin");
    for id in 1..=count {
        let row = fill_row(id, FILL_OUTSIDE, Some(10.00), Some(9.00));
        transaction
            .execute(
                "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
                 VALUES (?1, ?2, 1, ?3)",
                params![
                    vehicle_id.to_string(),
                    id,
                    serde_json::to_string(&row).expect("json")
                ],
            )
            .expect("insert filler row");
    }
    transaction.commit().expect("commit");
}

/// Replace a filler row by a fillable one inside fence 3, with samples.
fn fill_make_fillable(store: &HubStore, vehicle_id: Uuid, id: i64) {
    let row = fill_row(id, FILL_IN_FENCE_3, Some(10.00), Some(9.00));
    store
        .open()
        .expect("connection")
        .execute(
            "UPDATE materialised_charges SET charge_json = ?3
             WHERE vehicle_id = ?1 AND charge_id = ?2",
            params![
                vehicle_id.to_string(),
                id,
                serde_json::to_string(&row).expect("json")
            ],
        )
        .expect("make fillable");
    fill_put_samples(
        store,
        vehicle_id,
        id,
        &[
            (0, Some(0.00)),
            (5 * COST_MIN, Some(4.50)),
            (10 * COST_MIN, Some(9.00)),
        ],
    );
}

#[test]
fn r50_the_fill_walks_every_page_of_a_long_history() {
    // `materialised_charge_page` reads 256 rows at a time, so ids 1..=300 span
    // two pages. A fill that stops after the first page, or that restarts
    // from the wrong id, misses the rows after row 256.
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
    fill_put_filler_rows(&store, v, 300);
    // The only fillable row lies on the second page.
    fill_make_fillable(&store, v, 300);
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        1
    );
    assert_eq!(fill_read(&store, v, 300).cost, Some(3.00));
    assert_eq!(fill_mutations(&store, v, 300), 1);
    // The fillers were passed over, not rewritten.
    for id in [1, 255, 256, 257, 299] {
        assert_eq!(fill_read(&store, v, id).cost, None, "row {id}");
        assert_eq!(fill_mutations(&store, v, id), 0, "row {id}");
    }
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("again"),
        0
    );
}

#[test]
fn r50_a_fill_prices_the_rows_on_both_sides_of_a_page_boundary() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
    fill_put_filler_rows(&store, v, 300);
    // The last row of the first page, the first of the second, and the last.
    for id in [256, 257, 300] {
        fill_make_fillable(&store, v, id);
    }
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        3
    );
    for id in [256, 257, 300] {
        assert_eq!(fill_read(&store, v, id).cost, Some(3.00), "row {id}");
        assert_eq!(fill_mutations(&store, v, id), 1, "row {id}");
    }
}

#[test]
fn t35_the_fill_refuses_bad_arguments_before_writing_anything() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
    fill_put_row(
        &store,
        v,
        &fill_row(11, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
    );
    assert!(matches!(
        store.recalculate_missing_charge_costs(v, 99),
        Err(StoreError::UnknownGeofence(99))
    ));
    for id in [0, -1] {
        assert!(matches!(
            store.recalculate_missing_charge_costs(v, id),
            Err(StoreError::InvalidGeofence)
        ));
    }
    assert!(matches!(
        store.recalculate_missing_charge_costs(Uuid::nil(), 3),
        Err(StoreError::NilVehicleId)
    ));
    // The errors come in the order of R50: a nil vehicle first, then the id.
    assert!(matches!(
        store.recalculate_missing_charge_costs(Uuid::nil(), 0),
        Err(StoreError::NilVehicleId)
    ));
    assert_eq!(fill_read(&store, v, 11).cost, None);
    assert_eq!(fill_mutations(&store, v, 11), 0);
    assert_eq!(fill_export_revision(&store, v), 0);
}

#[test]
fn r40_a_fence_without_a_complete_valid_tariff_fills_nothing() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    // Fee only, type without a price, and a negative price.
    store
        .upsert_geofences(
            v,
            &[
                crate::teslamate_projection::TeslaMateGeofence {
                    id: 5,
                    name: "Fee only".to_owned(),
                    latitude: Some(10.0),
                    longitude: Some(20.0),
                    radius_m: Some(150.0),
                    billing_type: None,
                    cost_per_unit: None,
                    session_fee: Some(2.00),
                },
                crate::teslamate_projection::TeslaMateGeofence {
                    id: 6,
                    name: "Negative".to_owned(),
                    latitude: Some(12.0),
                    longitude: Some(20.0),
                    radius_m: Some(150.0),
                    billing_type: Some(kwh()),
                    cost_per_unit: Some(-0.10),
                    session_fee: None,
                },
            ],
        )
        .expect("fences");
    fill_put_row(
        &store,
        v,
        &fill_row(1, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
    );
    fill_put_row(
        &store,
        v,
        &fill_row(2, (12.0005, 20.0), Some(10.00), Some(9.00)),
    );
    assert_eq!(
        store
            .recalculate_missing_charge_costs(v, 5)
            .expect("fee only"),
        0
    );
    assert_eq!(
        store
            .recalculate_missing_charge_costs(v, 6)
            .expect("negative"),
        0
    );
    assert_eq!(fill_read(&store, v, 1).cost, None);
    assert_eq!(fill_read(&store, v, 2).cost, None);
    assert_eq!(fill_export_revision(&store, v), 0);
}

#[test]
fn t35b_a_migrated_vehicle_keeps_imported_rows_and_fills_its_live_ones() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let import = store
        .register_source(&SourceDescriptor::new("teslamate", "import-m"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(import.source_id, "vehicle-m"),
            1_001,
        )
        .expect("vehicle");
    let v = vehicle.vehicle_id;
    insert_watermark(&store, &import, &vehicle, "charging_processes", Some(40));
    fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
    for id in [39, 41, 42] {
        fill_put_row(
            &store,
            v,
            &fill_row(id, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
        );
    }
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        2
    );
    for id in [41, 42] {
        let row = fill_read(&store, v, id);
        assert_eq!(row.cost, Some(3.00), "row {id}");
        assert_eq!(fill_copy(&row), (Some(kwh()), Some(0.30), None));
    }
    assert_eq!(fill_read(&store, v, 39).cost, None);

    // Without the watermark only the id test is gone: the same rows all read
    // as Hub rows, so nothing but the watermark marks an imported row.
    store
        .open()
        .expect("connection")
        .execute(
            "DELETE FROM lifecycle_source_watermarks WHERE vehicle_id = ?1",
            params![v.to_string()],
        )
        .expect("remove the watermark");
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("again"),
        1
    );
    assert_eq!(fill_read(&store, v, 39).cost, Some(3.00));
}

#[test]
fn t35c_an_import_for_another_vehicle_does_not_mark_the_live_rows_imported() {
    let (_directory, store, _source, live) = fill_store();
    let l = live.vehicle_id;
    // The same car imported as another vehicle of the import source kind, with
    // a watermark of 50 on that vehicle only.
    let import = store
        .register_source(&SourceDescriptor::new("teslamate", "import-c"), 1_000)
        .expect("import source");
    let imported = store
        .register_vehicle(
            &VehicleDescriptor::new(import.source_id, "vehicle-i"),
            1_001,
        )
        .expect("imported vehicle");
    insert_watermark(&store, &import, &imported, "charging_processes", Some(50));
    fill_put_fence(&store, l, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
    fill_put_fence(
        &store,
        imported.vehicle_id,
        3,
        (10.0, 20.0),
        150.0,
        Some((kwh(), 0.30, None)),
    );
    for id in 1..=3 {
        fill_put_row(
            &store,
            l,
            &fill_row(id, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
        );
        fill_put_row(
            &store,
            imported.vehicle_id,
            &fill_row(id, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
        );
    }
    assert_eq!(
        store.recalculate_missing_charge_costs(l, 3).expect("fill"),
        3
    );
    for id in 1..=3 {
        assert_eq!(fill_read(&store, imported.vehicle_id, id).cost, None);
    }

    // Variant: a cutover watermark of 2 on the live vehicle makes rows 1 and
    // 2 imported and row 3 a Hub row. Rows 1 to 3 are priced already, so use
    // fresh unpriced rows 11 to 13 under a watermark of 12.
    let (_second_directory, second_store, _second_source, second_live) = fill_store();
    let s = second_live.vehicle_id;
    fill_import_watermark(&second_store, &second_live, "import-cutover", 12);
    fill_put_fence(
        &second_store,
        s,
        3,
        (10.0, 20.0),
        150.0,
        Some((kwh(), 0.30, None)),
    );
    for id in 11..=13 {
        fill_put_row(
            &second_store,
            s,
            &fill_row(id, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
        );
    }
    assert_eq!(
        second_store
            .recalculate_missing_charge_costs(s, 3)
            .expect("fill"),
        1
    );
    assert_eq!(fill_read(&second_store, s, 13).cost, Some(3.00));
    assert_eq!(fill_read(&second_store, s, 11).cost, None);
    assert_eq!(fill_read(&second_store, s, 12).cost, None);
}

#[test]
fn t35d_a_continuing_row_is_filled_with_the_fee_waived() {
    let mut harness = CostHarness::new();
    // Fence 3 has no tariff when both rows close, so both costs stay unknown.
    harness.imported_fence(3, 10.0, None, None, None);
    harness.feed_all(&first_plug_in_row());
    harness.feed_all(&a7_run(
        20,
        COST_T0 + 3 * COST_HOUR,
        [7.50, 8.58, 9.66],
        "Complete",
    ));
    let rows = harness.charges();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].cost, rows[1].cost), (None, None));
    let (first_id, second_id) = (rows[0].id, rows[1].id);

    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    let filled = harness
        .store
        .recalculate_missing_charge_costs(harness.vehicle.vehicle_id, 3)
        .expect("fill");
    assert_eq!(filled, 2);
    // The second row continues the first row's plug-in: the fee is waived and
    // the cost is what the close would have given (T49 (a)).
    let second = harness.charge(second_id);
    assert_eq!(
        fill_copy(&second),
        (Some(kwh()), Some(0.30), Some(0.00)),
        "continuing row"
    );
    assert_eq!(second.cost, Some(0.82));
    // The first row pays the fee: 8.00 used * 0.30 + 1.00.
    let first = harness.charge(first_id);
    assert_eq!(fill_copy(&first), (Some(kwh()), Some(0.30), Some(1.00)));
    assert_eq!(
        first.cost,
        first
            .charge_energy_used_kwh
            .map(|used| { ((used * 0.30 + 1.00) * 100.0).round() / 100.0 })
    );
}

#[test]
fn t35e_a_live_charge_after_a_seeded_cutover_is_filled_as_a_hub_row() {
    let start = COST_T0 - COST_HOUR;
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "import-t35e"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-import"),
            1_001,
        )
        .expect("vehicle");
    let v = vehicle.vehicle_id;
    // Fence 3 has no tariff while the charges close.
    fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, None);
    let mut session = imported_open_session(start);
    session.watermarks.charging_processes.max_id = Some(50);
    store
        .seed_imported_open_session(source.source_id, v, 1, &session, start)
        .expect("seed");
    // Live observations close charge 7, then open and close a new charge.
    // Charge 7's live samples carry A7 power with counters that rise slower
    // than the integral, so its used energy is a known figure (the imported
    // samples give a ratio below one on their own, which R20 would withhold):
    // without this, a fill that ignored the import bound would still find
    // nothing to price on row 7.
    let mut live = vec![
        cost_at(
            cost_ob(1, start + 7 * COST_MIN, "Charging", cost_a7(Some(2.20))),
            10.0005,
            20.0,
        ),
        cost_ob(2, start + 9 * COST_MIN, "Complete", cost_a7(Some(2.40))),
        cost_ob(
            3,
            start + 9 * COST_MIN + COST_HOUR,
            "Disconnected",
            serde_json::json!({}),
        ),
    ];
    let second_start = start + 9 * COST_MIN + 2 * COST_HOUR;
    live.push(cost_at(
        cost_ob(4, second_start, "Charging", cost_a7(Some(0.00))),
        10.0005,
        20.0,
    ));
    live.push(cost_ob(
        5,
        second_start + 10 * COST_MIN,
        "Charging",
        cost_a7(Some(1.08)),
    ));
    live.push(cost_ob(
        6,
        second_start + 20 * COST_MIN,
        "Complete",
        cost_a7(Some(2.16)),
    ));
    feed_owner_samples(&store, &source, &vehicle, &live);
    let history = store.materialised_history(v).expect("history");
    let ids = history.charges.iter().map(|row| row.id).collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![7, 51],
        "the live charge must be allocated above the import watermark (C1 R10 with amendment A5)"
    );
    let live_charge = fill_read(&store, v, 51);
    assert_eq!(live_charge.charge_energy_added, Some(2.16));
    assert_eq!(live_charge.charge_energy_used_kwh, Some(2.40));
    assert_eq!(live_charge.cost, None);

    // Give the cutover row a start inside fence 3, so that only the import
    // bound keeps the fill away from it. Its used energy is known, so the
    // tariff below would price it (0.30 per kWh) were it not an imported row.
    let mut cutover = fill_read(&store, v, 7);
    let cutover_used = cutover
        .charge_energy_used_kwh
        .expect("the cutover row has a known used energy");
    assert!(cutover_used > 0.0);
    assert_eq!(cutover.cost, None);
    cutover.start_latitude = Some(FILL_IN_FENCE_3.0);
    cutover.start_longitude = Some(FILL_IN_FENCE_3.1);
    store
        .open()
        .expect("connection")
        .execute(
            "UPDATE materialised_charges SET charge_json = ?3
             WHERE vehicle_id = ?1 AND charge_id = ?2",
            params![
                v.to_string(),
                7,
                serde_json::to_string(&cutover).expect("json")
            ],
        )
        .expect("place the cutover row");
    store
        .save_geofence(
            v,
            Some(3),
            crate::teslamate_projection::TeslaMateGeofence {
                id: 0,
                name: "F3".to_owned(),
                latitude: Some(10.0),
                longitude: Some(20.0),
                radius_m: Some(150.0),
                billing_type: Some(kwh()),
                cost_per_unit: Some(0.30),
                session_fee: None,
            },
        )
        .expect("save fence");
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        1
    );
    let priced = fill_read(&store, v, 51);
    assert_eq!(priced.cost, Some(0.72));
    assert_eq!(fill_copy(&priced), (Some(kwh()), Some(0.30), None));
    assert_eq!(fill_read(&store, v, 7).cost, None);
}

#[test]
fn t36_a_per_minute_row_is_filled_only_when_its_span_is_observed() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_fence(
        &store,
        v,
        3,
        (10.0, 20.0),
        150.0,
        Some((per_minute(), 0.25, None)),
    );
    let end_ms = 95 * COST_MIN + COST_MIN / 2;
    let mut observed = fill_row(21, FILL_IN_FENCE_3, None, Some(30.00));
    observed.end_date_ms = Some(COST_T0 + end_ms);
    fill_put_row(&store, v, &observed);
    let mut offsets = (0..=19)
        .map(|step| (step * 5 * COST_MIN, None))
        .collect::<Vec<_>>();
    offsets.push((end_ms, None));
    fill_put_samples(&store, v, 21, &offsets);
    // One sample only, two hours after the start: the start is a bound.
    let mut bounded = fill_row(22, FILL_IN_FENCE_3, None, Some(30.00));
    bounded.end_date_ms = Some(COST_T0 + 2 * COST_HOUR);
    fill_put_row(&store, v, &bounded);
    fill_put_samples(&store, v, 22, &[(2 * COST_HOUR, None)]);

    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        1
    );
    // 95.5 minutes * 0.25 = 23.875, half away from zero.
    let row = fill_read(&store, v, 21);
    assert_eq!(row.cost, Some(23.88));
    assert_eq!(fill_copy(&row), (Some(per_minute()), Some(0.25), None));
    assert_eq!(fill_read(&store, v, 22).cost, None);
}

#[test]
fn r50_the_fill_uses_the_setting_of_the_rows_own_car() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
    let supercharger = |id: i64| {
        let mut row = cost_row(id, None, Some(9.00), Some(true), Some("Supercharger"));
        row.start_latitude = Some(FILL_IN_FENCE_3.0);
        row.start_longitude = Some(FILL_IN_FENCE_3.1);
        row
    };
    fill_put_row(&store, v, &supercharger(1));
    // No setting: a DC row with a counter is priced at the fence's rate.
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        1
    );
    assert_eq!(fill_read(&store, v, 1).cost, Some(2.70));
    // The setting is on for car 1: the next Supercharger row is free.
    store
        .upsert_car_settings(
            v,
            1,
            &crate::hub_pack::ProjectionCarSettings {
                free_supercharging: true,
                ..Default::default()
            },
        )
        .expect("settings");
    fill_put_row(&store, v, &supercharger(2));
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        1
    );
    assert_eq!(fill_read(&store, v, 2).cost, Some(0.00));
    // A row already priced is never touched by the later setting (R47).
    assert_eq!(fill_read(&store, v, 1).cost, Some(2.70));
}

/// A stored row of vehicle `v` that the manual entry tests use: AC, used
/// 20.00, a tariff copy with a fee.
fn manual_row(id: i64) -> crate::hub_pack::ProjectionCharge {
    let mut row = cost_row(id, Some(20.00), Some(18.00), Some(false), None);
    row.billing_type = Some(kwh());
    row.cost_per_unit = Some(0.30);
    row.session_fee = Some(1.00);
    row
}

#[test]
fn t37_a_manual_total_replaces_the_cost_and_keeps_the_copy() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_row(&store, v, &manual_row(7));
    let row = store.set_charge_cost(v, 7, 18.4).expect("set");
    assert_eq!(row.cost, Some(18.40));
    assert_eq!(fill_copy(&row), (Some(kwh()), Some(0.30), Some(1.00)));
    assert_eq!(fill_read(&store, v, 7), row);
    assert_eq!(fill_mutations(&store, v, 7), 1);
    assert_eq!(fill_export_revision(&store, v), 1);
    // The same value again writes nothing and still returns the row.
    let again = store.set_charge_cost(v, 7, 18.4).expect("again");
    assert_eq!(again, row);
    assert_eq!(fill_mutations(&store, v, 7), 1);
    assert_eq!(fill_export_revision(&store, v), 1);
    // A different value replaces the earlier cost.
    assert_eq!(
        store.set_charge_cost(v, 7, 19.0).expect("replace").cost,
        Some(19.00)
    );
    assert_eq!(fill_mutations(&store, v, 7), 2);
}

#[test]
fn t38_a_manual_per_kwh_rate_prices_the_inlet_energy_without_the_fence_fee() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_row(&store, v, &manual_row(7));
    let row = store.set_charge_cost_rate(v, 7, 0.25, kwh()).expect("rate");
    assert_eq!(row.cost, Some(5.00));
    assert_eq!(fill_copy(&row), (Some(kwh()), Some(0.25), None));
    assert_eq!(fill_read(&store, v, 7), row);
    assert_eq!(fill_mutations(&store, v, 7), 1);
    // Entering it again is a no-op.
    assert_eq!(
        store
            .set_charge_cost_rate(v, 7, 0.25, kwh())
            .expect("again"),
        row
    );
    assert_eq!(fill_mutations(&store, v, 7), 1);
}

#[test]
fn t38_a_manual_rate_ignores_free_charging() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    store
        .upsert_car_settings(
            v,
            1,
            &crate::hub_pack::ProjectionCarSettings {
                free_supercharging: true,
                ..Default::default()
            },
        )
        .expect("settings");
    let mut row = cost_row(
        7,
        Some(20.00),
        Some(19.00),
        Some(true),
        Some("Supercharger"),
    );
    row.cost = Some(0.00);
    fill_put_row(&store, v, &row);
    // The owner's entry is the complete price: DC uses the counter (19.00).
    assert_eq!(
        store
            .set_charge_cost_rate(v, 7, 0.50, kwh())
            .expect("rate")
            .cost,
        Some(9.50)
    );
}

#[test]
fn t39_a_rate_with_no_basis_names_the_mode_and_writes_nothing() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_row(&store, v, &cost_row(7, None, Some(9.00), Some(false), None));
    let before = fill_read(&store, v, 7);
    assert!(matches!(
        store.set_charge_cost_rate(v, 7, 0.25, kwh()),
        Err(StoreError::ChargeCostBasisUnavailable {
            charge_id: 7,
            mode: "per_kwh"
        })
    ));
    // The materialised imported open process has no end.
    let mut open = cost_row(8, None, None, None, None);
    open.end_date_ms = None;
    fill_put_row(&store, v, &open);
    assert!(matches!(
        store.set_charge_cost_rate(v, 8, 0.25, per_minute()),
        Err(StoreError::ChargeCostBasisUnavailable {
            charge_id: 8,
            mode: "per_minute"
        })
    ));
    // A per-minute rate on a row whose start is only a bound has no basis.
    let mut bounded = cost_row(9, None, None, None, None);
    bounded.end_date_ms = Some(COST_T0 + 2 * COST_HOUR);
    fill_put_row(&store, v, &bounded);
    fill_put_samples(&store, v, 9, &[(2 * COST_HOUR, None)]);
    assert!(matches!(
        store.set_charge_cost_rate(v, 9, 0.25, per_minute()),
        Err(StoreError::ChargeCostBasisUnavailable {
            charge_id: 9,
            mode: "per_minute"
        })
    ));
    assert_eq!(fill_read(&store, v, 7), before);
    assert_eq!(fill_mutations(&store, v, 7), 0);
    assert_eq!(fill_mutations(&store, v, 8), 0);
    assert_eq!(fill_export_revision(&store, v), 0);
    // A manual total needs no basis, so it prices the open process.
    assert_eq!(
        store.set_charge_cost(v, 8, 4.0).expect("total").cost,
        Some(4.00)
    );
}

#[test]
fn t40_a_manual_per_minute_rate_prices_the_connected_minutes() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    let end_ms = 95 * COST_MIN + COST_MIN / 2;
    let mut row = cost_row(7, None, None, Some(false), None);
    row.end_date_ms = Some(COST_T0 + end_ms);
    fill_put_row(&store, v, &row);
    let mut offsets = (0..=19)
        .map(|step| (step * 5 * COST_MIN, None))
        .collect::<Vec<_>>();
    offsets.push((end_ms, None));
    fill_put_samples(&store, v, 7, &offsets);
    let stored = store
        .set_charge_cost_rate(v, 7, 0.25, per_minute())
        .expect("rate");
    assert_eq!(stored.cost, Some(23.88));
    assert_eq!(fill_copy(&stored), (Some(per_minute()), Some(0.25), None));
}

#[test]
fn t41_manual_entry_errors_come_in_order_and_write_nothing() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_row(&store, v, &manual_row(7));
    assert!(matches!(
        store.set_charge_cost(Uuid::nil(), 7, 1.0),
        Err(StoreError::NilVehicleId)
    ));
    assert!(matches!(
        store.set_charge_cost(v, 0, 1.0),
        Err(StoreError::InvalidChargeId)
    ));
    assert!(matches!(
        store.set_charge_cost(v, -3, 1.0),
        Err(StoreError::InvalidChargeId)
    ));
    // The value is checked before the row is looked up: an absent row with an
    // invalid value reports the value.
    assert!(matches!(
        store.set_charge_cost(v, 99, f64::NAN),
        Err(StoreError::InvalidChargeCost)
    ));
    assert!(matches!(
        store.set_charge_cost(v, 99, 1.0),
        Err(StoreError::UnknownCharge(99))
    ));
    assert!(matches!(
        store.set_charge_cost_rate(v, 99, 0.25, kwh()),
        Err(StoreError::UnknownCharge(99))
    ));
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            store.set_charge_cost(v, 7, bad),
            Err(StoreError::InvalidChargeCost)
        ));
        assert!(matches!(
            store.set_charge_cost_rate(v, 7, bad, kwh()),
            Err(StoreError::InvalidChargeCost)
        ));
    }
    for bad in [-0.05, 100_000.0, 99_999.999_96] {
        assert!(
            matches!(
                store.set_charge_cost_rate(v, 7, bad, kwh()),
                Err(StoreError::InvalidChargeCost)
            ),
            "rate {bad}"
        );
    }
    assert_eq!(fill_read(&store, v, 7), manual_row(7));
    assert_eq!(fill_mutations(&store, v, 7), 0);
    assert_eq!(fill_export_revision(&store, v), 0);
    // A rate is rounded to four decimals.
    let rounded = store
        .set_charge_cost_rate(v, 7, 0.123456, kwh())
        .expect("rounded rate");
    assert_eq!(rounded.cost_per_unit, Some(0.1235));
}

#[test]
fn t42_a_manual_total_on_an_imported_row_survives_a_later_fill() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_import_watermark(&store, &vehicle, "import-t42", 20);
    fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
    fill_put_row(
        &store,
        v,
        &fill_row(9, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
    );
    assert_eq!(
        store.set_charge_cost(v, 9, 12.00).expect("manual").cost,
        Some(12.00)
    );
    assert_eq!(
        store.recalculate_missing_charge_costs(v, 3).expect("fill"),
        0
    );
    assert_eq!(fill_read(&store, v, 9).cost, Some(12.00));
}

#[test]
fn t43_negative_and_signed_zero_totals() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_row(&store, v, &manual_row(7));
    assert_eq!(
        store.set_charge_cost(v, 7, -3.20).expect("credit").cost,
        Some(-3.20)
    );
    assert_eq!(
        store.set_charge_cost(v, 7, -0.004).expect("zero").cost,
        Some(0.0)
    );
    let stored: String = store
        .open()
        .expect("connection")
        .query_row(
            "SELECT charge_json FROM materialised_charges
             WHERE vehicle_id = ?1 AND charge_id = 7",
            params![v.to_string()],
            |row| row.get(0),
        )
        .expect("json");
    assert!(stored.contains("\"cost\":0.0"), "{stored}");
    assert!(!stored.contains("-0.0"), "{stored}");
    assert!(matches!(
        store.set_charge_cost_rate(v, 7, -0.05, kwh()),
        Err(StoreError::InvalidChargeCost)
    ));
    // A rate that rounds to zero is a free rate, stored as 0.0.
    let free = store
        .set_charge_cost_rate(v, 7, -0.00004, kwh())
        .expect("rounds to zero");
    assert_eq!((free.cost_per_unit, free.cost), (Some(0.0), Some(0.0)));
}

#[test]
fn t46_range_limits_of_totals_rates_and_computed_costs() {
    let (_directory, store, _source, vehicle) = fill_store();
    let v = vehicle.vehicle_id;
    fill_put_row(&store, v, &manual_row(7));
    for bad in [1e300, -1e300, 1e308, 1e15] {
        assert!(
            matches!(
                store.set_charge_cost(v, 7, bad),
                Err(StoreError::InvalidChargeCost)
            ),
            "total {bad}"
        );
    }
    // The largest value of the pack's scale-2 numeric is representable.
    assert_eq!(
        store
            .set_charge_cost(v, 7, 999_999_999_999.99)
            .expect("large")
            .cost,
        Some(999_999_999_999.99)
    );
    // The top of the rate range is accepted.
    let top = store
        .set_charge_cost_rate(v, 7, 99_999.999_9, kwh())
        .expect("top rate");
    assert_eq!(top.cost_per_unit, Some(99_999.999_9));
    // A valid rate over so much energy that the cost leaves the pack's range is
    // an invalid cost, not a missing basis.
    let mut huge = cost_row(8, Some(1e12), Some(1e12), Some(false), None);
    huge.cost = None;
    fill_put_row(&store, v, &huge);
    assert!(matches!(
        store.set_charge_cost_rate(v, 8, 99_999.0, kwh()),
        Err(StoreError::InvalidChargeCost)
    ));
    assert_eq!(fill_read(&store, v, 8).cost, None);
    // The computed cost of the same row is None.
    assert_eq!(
        crate::lifecycle::calculate_charge_cost(
            &huge,
            Some(crate::lifecycle::ChargeTariff {
                billing_type: kwh(),
                cost_per_unit: Some(99_999.0),
                session_fee: None,
            }),
            crate::lifecycle::ChargeCostContext {
                free_supercharging: false,
                span_observed: true,
            },
        ),
        None
    );
}

#[test]
fn t45_the_fill_and_manual_entry_touch_only_the_asked_vehicle() {
    let (_directory, store, source, first) = fill_store();
    let second = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-two"),
            1_002,
        )
        .expect("second vehicle");
    let (v1, v2) = (first.vehicle_id, second.vehicle_id);
    for v in [v1, v2] {
        fill_put_fence(&store, v, 3, (10.0, 20.0), 150.0, Some((kwh(), 0.30, None)));
        for id in 11..=13 {
            fill_put_row(
                &store,
                v,
                &fill_row(id, FILL_IN_FENCE_3, Some(10.00), Some(9.00)),
            );
            fill_put_samples(
                &store,
                v,
                id,
                &[(0, Some(0.00)), (10 * COST_MIN, Some(9.00))],
            );
        }
    }
    assert_eq!(
        store.recalculate_missing_charge_costs(v1, 3).expect("fill"),
        3
    );
    for id in 11..=13 {
        assert_eq!(fill_read(&store, v1, id).cost, Some(3.00));
        assert_eq!(fill_read(&store, v2, id).cost, None);
        assert_eq!(fill_mutations(&store, v2, id), 0);
    }
    assert_eq!(fill_export_revision(&store, v2), 0);
    store
        .set_charge_cost(v2, 11, 8.0)
        .expect("manual on the second");
    assert_eq!(fill_read(&store, v2, 11).cost, Some(8.00));
    assert_eq!(fill_read(&store, v1, 11).cost, Some(3.00));
    assert_eq!(fill_mutations(&store, v1, 11), 1);
    // A charge id the vehicle does not hold is unknown even when another has it.
    let third = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-three"),
            1_003,
        )
        .expect("third vehicle");
    assert!(matches!(
        store.set_charge_cost(third.vehicle_id, 11, 1.0),
        Err(StoreError::UnknownCharge(11))
    ));
}

/// Log capture for T44: the process-wide capture shared with the collector's log
/// tests (`crate::test_log_capture`), armed for this thread. A process can install
/// one global subscriber only, and a scoped one would not be reliable here: while
/// exactly one scoped subscriber exists, a callsite that another test thread
/// reaches first is registered against that thread's own (empty) view and is then
/// cached as disabled for every thread.
fn arm_fill_log_capture() {
    crate::test_log_capture::arm();
}

/// The text this thread logged since `arm_fill_log_capture`.
fn take_fill_logs() -> String {
    crate::test_log_capture::disarm()
}

#[test]
fn t44_close_fill_and_manual_entry_log_no_name_coordinate_or_vin() {
    const NAME: &str = "Secret Place";
    const VIN: &str = "5YJSYNTHETIC00002";
    arm_fill_log_capture();
    {
        let mut harness = CostHarness::new();
        let v = harness.vehicle.vehicle_id;
        harness
            .store
            .save_geofence(
                v,
                Some(3),
                crate::teslamate_projection::TeslaMateGeofence {
                    id: 0,
                    name: NAME.to_owned(),
                    latitude: Some(10.000123),
                    longitude: Some(20.000456),
                    radius_m: Some(150.0),
                    billing_type: Some(kwh()),
                    cost_per_unit: Some(0.30),
                    session_fee: Some(1.00),
                },
            )
            .expect("fence");
        // T31's close, with a VIN in the payloads and coordinates inside the
        // fence.
        let mut sequence = gap_sequence(true);
        for sample in &mut sequence {
            sample.payload["vin"] = serde_json::json!(VIN);
        }
        sequence[0] = cost_at(sequence[0].clone(), 10.000123, 20.000456);
        harness.feed_all(&sequence);
        assert_eq!(harness.charges().len(), 1);
        // T35's fill over an unpriced row at the secret place.
        let row = fill_row(40, (10.000123, 20.000456), Some(10.00), Some(9.00));
        fill_put_row(&harness.store, v, &row);
        assert_eq!(
            harness
                .store
                .recalculate_missing_charge_costs(v, 3)
                .expect("fill"),
            1
        );
        // T37's manual entry.
        harness.store.set_charge_cost(v, 40, 18.4).expect("total");
        harness
            .store
            .set_charge_cost_rate(v, 40, 0.25, kwh())
            .expect("rate");
    }
    let captured = take_fill_logs();
    assert!(
        captured.contains("missing charge costs filled"),
        "the fill must log its outcome: {captured}"
    );
    assert!(captured.contains("manual charge cost stored"), "{captured}");
    for forbidden in [NAME, "10.000123", "20.000456", "10.0001", "20.0004", VIN] {
        assert!(
            !captured.contains(forbidden),
            "{forbidden:?} must not appear in the logs: {captured}"
        );
    }
}
