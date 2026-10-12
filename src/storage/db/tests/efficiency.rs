// SPDX-License-Identifier: AGPL-3.0-only

// Car efficiency through the store (C2-P5): the candidate scan, the median,
// the write only on change and its triggers (R63 to R67), the imported value
// kept until the Hub has figures (R68) and determinism across a restart
// (R69). Spec tests T56 to T61, T58b, T64 and T65, plus the rules they leave
// unpinned. The session figure itself (T50 to T55) is tested in
// `src/runtime/lifecycle/efficiency_tests.rs`. Synthetic data only.
//
// A session's samples run (battery level %, rated range km, counter kWh):
// (50, 200.00, 0.00), (70, 280.00, x / 2) and (90, 360.00, x), so its figure is
// `1000 * x / 160` Wh/km: x = 24.00 gives 150.0, 24.32 gives 152.0, 23.84 gives
// 149.0, 25.60 gives 160.0, 32.00 gives 200.0 and 22.40 gives 140.0.

/// A car row with the given efficiency, as an import or an earlier recompute
/// leaves it.
fn eff_car(efficiency: Option<f64>) -> crate::hub_pack::ProjectionCar {
    crate::hub_pack::ProjectionCar {
        id: 1,
        name: "Synthetic car".into(),
        model: "3".into(),
        vin: Some("5YJSYNTHETIC00001".into()),
        source_eid: Some(88),
        source_vid: Some(99),
        trim_badging: Some("74D".into()),
        marketing_name: Some("LR AWD".into()),
        exterior_color: Some("Pearl White".into()),
        wheel_type: Some("Apollo".into()),
        spoiler_type: Some("None".into()),
        firmware_version: Some("2026.0".into()),
        efficiency_wh_per_km: efficiency,
        settings: Default::default(),
    }
}

/// Insert the car row directly, as an import does: no sync mutation.
fn eff_put_car(store: &HubStore, vehicle_id: Uuid, efficiency: Option<f64>) {
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO materialised_cars(vehicle_id, car_id, car_json) VALUES (?1, 1, ?2)",
            params![
                vehicle_id.to_string(),
                serde_json::to_string(&eff_car(efficiency)).expect("car json")
            ],
        )
        .expect("insert car");
}

fn eff_car_of(store: &HubStore, vehicle_id: Uuid) -> crate::hub_pack::ProjectionCar {
    store
        .materialised_car_for_vehicle(vehicle_id)
        .expect("read car")
        .expect("car row")
}

fn eff_car_mutations(store: &HubStore, vehicle_id: Uuid) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND entity = 'car'",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("count")
}

/// One session sample at `offset_ms` from the row's start: sample id
/// `charge_id * 100 + position`, unique per vehicle.
fn eff_sample(
    charge_id: i64,
    position: i64,
    offset_ms: i64,
    level: i64,
    rated: f64,
    counter: f64,
) -> crate::hub_pack::ProjectionChargeSample {
    let mut sample = cost_sample(charge_id * 100 + position, charge_id, offset_ms, Some(counter));
    sample.battery_level = Some(level);
    sample.rated_range_km = Some(rated);
    sample
}

/// Insert a row of `car_id` and its three samples (see the file header for
/// the figure of `x`). `closed` false leaves the row open.
fn eff_put_session_for(
    connection: &Connection,
    vehicle_id: Uuid,
    car_id: i64,
    charge_id: i64,
    is_dc: Option<bool>,
    closed: bool,
    x: f64,
) {
    let mut row = cost_row(charge_id, None, None, is_dc, None);
    row.car_id = car_id;
    if !closed {
        row.end_date_ms = None;
    }
    connection
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                vehicle_id.to_string(),
                charge_id,
                car_id,
                serde_json::to_string(&row).expect("row json")
            ],
        )
        .expect("insert row");
    let samples = [
        eff_sample(charge_id, 0, 0, 50, 200.00, 0.00),
        eff_sample(charge_id, 1, 5 * COST_MIN, 70, 280.00, x / 2.0),
        eff_sample(charge_id, 2, 10 * COST_MIN, 90, 360.00, x),
    ];
    for sample in samples {
        connection
            .execute(
                "INSERT INTO materialised_charge_samples(
                    vehicle_id, sample_id, charge_id, sample_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    vehicle_id.to_string(),
                    sample.id,
                    charge_id,
                    serde_json::to_string(&sample).expect("sample json")
                ],
            )
            .expect("insert sample");
    }
}

/// A closed Hub AC session of car 1 (unless `is_dc` says otherwise).
fn eff_put(store: &HubStore, vehicle_id: Uuid, charge_id: i64, x: f64) {
    eff_put_session_for(
        &store.open().expect("connection"),
        vehicle_id,
        1,
        charge_id,
        Some(false),
        true,
        x,
    );
}

fn eff_put_kind(store: &HubStore, vehicle_id: Uuid, charge_id: i64, is_dc: Option<bool>, x: f64) {
    eff_put_session_for(
        &store.open().expect("connection"),
        vehicle_id,
        1,
        charge_id,
        is_dc,
        true,
        x,
    );
}

/// Run the recomputation in its own committed transaction.
fn eff_recompute(store: &HubStore, vehicle_id: Uuid) {
    let mut connection = store.open().expect("connection");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin");
    recompute_car_efficiency(&transaction, vehicle_id, 1).expect("recompute");
    transaction.commit().expect("commit");
}

/// Captured log text without the colour escapes the formatter adds.
fn eff_plain(logs: String) -> String {
    let mut plain = String::new();
    let mut characters = logs.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            for skipped in characters.by_ref() {
                if skipped == 'm' {
                    break;
                }
            }
        } else {
            plain.push(character);
        }
    }
    plain
}

fn eff_store() -> (tempfile::TempDir, HubStore, VehicleRecord) {
    let (directory, store, _source, vehicle) = fill_store();
    (directory, store, vehicle)
}

#[test]
fn t56_the_car_figure_is_the_median_of_the_newest_hub_ac_sessions() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    // An imported value that the Hub's own figure supersedes (R68).
    eff_put_car(&store, v, Some(162.5));
    for (id, x) in [(1, 24.00), (2, 24.32), (3, 23.84)] {
        eff_put(&store, v, id, x);
    }
    // A second vehicle with equal charge ids whose sessions give 200.0 (R8).
    let second = store
        .register_vehicle(&VehicleDescriptor::new(vehicle.source_id, "vehicle-two"), 1_002)
        .expect("second vehicle");
    let v2 = second.vehicle_id;
    eff_put_car(&store, v2, Some(162.5));
    for id in 1..=3 {
        eff_put(&store, v2, id, 32.00);
    }

    eff_recompute(&store, v);
    let car = eff_car_of(&store, v);
    assert_eq!(car.efficiency_wh_per_km, Some(150.0));
    assert_eq!(eff_car_mutations(&store, v), 1);
    // Nothing else of the car changed.
    assert_eq!(car, eff_car(Some(150.0)));
    assert_eq!(eff_car_of(&store, v2).efficiency_wh_per_km, Some(162.5));
    assert_eq!(eff_car_mutations(&store, v2), 0);

    // A fourth session at 160.0: the mean of 150.0 and 152.0.
    eff_put(&store, v, 4, 25.60);
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(151.0));
    assert_eq!(eff_car_mutations(&store, v), 2);

    // The second vehicle's own sessions give its own value.
    eff_recompute(&store, v2);
    assert_eq!(eff_car_of(&store, v2).efficiency_wh_per_km, Some(200.0));
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(151.0));
}

#[test]
fn r8_rows_of_another_car_of_the_same_vehicle_do_not_count() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, Some(162.5));
    let connection = store.open().expect("connection");
    for id in 1..=2 {
        eff_put_session_for(&connection, v, 1, id, Some(false), true, 24.00);
    }
    for id in 3..=5 {
        eff_put_session_for(&connection, v, 2, id, Some(false), true, 32.00);
    }
    drop(connection);
    // Two figures for car 1: unchanged.
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(162.5));
    // A third for car 1 gives its own median, not car 2's.
    eff_put(&store, v, 6, 24.00);
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(150.0));
}

#[test]
fn t57_fewer_than_three_figures_leave_the_imported_value_and_write_nothing() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, Some(162.5));
    eff_put(&store, v, 1, 24.00);
    eff_put(&store, v, 2, 24.00);
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(162.5));
    assert_eq!(eff_car_mutations(&store, v), 0);
}

#[test]
fn a_session_without_a_figure_does_not_count_toward_the_three() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, None);
    eff_put(&store, v, 1, 24.00);
    eff_put(&store, v, 2, 24.00);
    // A candidate whose counter never rises gives no figure (R62 step 4).
    eff_put(&store, v, 3, 0.00);
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, None);
    assert_eq!(eff_car_mutations(&store, v), 0);
}

#[test]
fn t58_imported_dc_unknown_and_open_sessions_are_excluded() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    fill_import_watermark(&store, &vehicle, "import-t58", 1);
    eff_put_car(&store, v, Some(162.5));
    // Each with figure 140.0 (x = 22.40): an imported session, a DC session,
    // an `is_dc` None session and an open session.
    eff_put_kind(&store, v, 1, Some(false), 22.40);
    eff_put_kind(&store, v, 2, Some(true), 22.40);
    eff_put_kind(&store, v, 3, None, 22.40);
    eff_put_session_for(&store.open().expect("connection"), v, 1, 4, Some(false), false, 22.40);
    // Two Hub AC sessions at 150.0.
    eff_put(&store, v, 5, 24.00);
    eff_put(&store, v, 6, 24.00);
    eff_recompute(&store, v);
    // Two figures: unchanged.
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(162.5));
    assert_eq!(eff_car_mutations(&store, v), 0);
    // A third Hub AC session makes three, and none of the excluded rows (140.0)
    // reaches the median.
    eff_put(&store, v, 7, 24.00);
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(150.0));
}

#[test]
fn t58b_a_migrated_vehicle_is_split_by_the_charge_watermark_not_by_its_source() {
    // (a) Registered under a source of the import kind, watermark 40.
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let import = store
        .register_source(&SourceDescriptor::new("teslamate", "import-m"), 1_000)
        .expect("import source");
    let migrated = store
        .register_vehicle(&VehicleDescriptor::new(import.source_id, "vehicle-m"), 1_001)
        .expect("migrated vehicle");
    let m = migrated.vehicle_id;
    insert_watermark(&store, &import, &migrated, "charging_processes", Some(40));
    eff_put_car(&store, m, Some(162.5));
    for id in 37..=39 {
        eff_put(&store, m, id, 32.00); // imported, figure 200.0
    }
    for id in 41..=43 {
        eff_put(&store, m, id, 24.00); // collected live, figure 150.0
    }
    eff_recompute(&store, m);
    // 175.0 would mean the imported rows were counted; 162.5 unchanged would
    // mean the vehicle's source was consulted.
    assert_eq!(eff_car_of(&store, m).efficiency_wh_per_km, Some(150.0));
    assert_eq!(eff_car_mutations(&store, m), 1);

    // (b) A live vehicle with no watermark, while an import vehicle of the
    // same VIN holds watermark 50.
    let live_source = store
        .register_source(&SourceDescriptor::new("tesla_owner_api", "account-live"), 1_002)
        .expect("live source");
    let live = store
        .register_vehicle(&VehicleDescriptor::new(live_source.source_id, "vehicle-l"), 1_003)
        .expect("live vehicle");
    let other_import = store
        .register_source(&SourceDescriptor::new("teslamate", "import-n"), 1_004)
        .expect("other import source");
    let same_vin = store
        .register_vehicle(&VehicleDescriptor::new(other_import.source_id, "vehicle-n"), 1_005)
        .expect("same-VIN import vehicle");
    insert_watermark(&store, &other_import, &same_vin, "charging_processes", Some(50));
    eff_put_car(&store, live.vehicle_id, None);
    for id in 1..=3 {
        eff_put(&store, live.vehicle_id, id, 24.00);
    }
    eff_recompute(&store, live.vehicle_id);
    assert_eq!(
        eff_car_of(&store, live.vehicle_id).efficiency_wh_per_km,
        Some(150.0)
    );
}

#[test]
fn t59_only_the_newest_twenty_ac_candidates_are_used() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, None);
    // The oldest 5 at 200.0, the newest 20 at 150.0.
    for id in 1..=5 {
        eff_put(&store, v, id, 32.00);
    }
    for id in 6..=25 {
        eff_put(&store, v, id, 24.00);
    }
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(150.0));
    assert_eq!(eff_car_mutations(&store, v), 1);
    // Twenty newer DC sessions are skipped: the same twenty AC sessions are
    // used and nothing changes.
    for id in 26..=45 {
        eff_put_kind(&store, v, id, Some(true), 22.40);
    }
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(150.0));
    assert_eq!(eff_car_mutations(&store, v), 1);
}

#[test]
fn t59b_session_twenty_one_and_older_do_not_reach_the_median() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, None);
    // 22 sessions: the oldest 12 at 200.0, the newest 10 at 150.0. The newest 20
    // are 10 at 150.0 and 10 at 200.0, a median of 175.0; a window of 21 or 22
    // would give 200.0.
    for id in 1..=12 {
        eff_put(&store, v, id, 32.00);
    }
    for id in 13..=22 {
        eff_put(&store, v, id, 24.00);
    }
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(175.0));
}

#[test]
fn t60_a_second_recomputation_in_the_same_transaction_writes_nothing() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, Some(162.5));
    for id in 1..=3 {
        eff_put(&store, v, id, 24.00);
    }
    let mut connection = store.open().expect("connection");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin");
    recompute_car_efficiency(&transaction, v, 1).expect("first");
    let after_first: String = transaction
        .query_row(
            "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
            params![v.to_string()],
            |row| row.get(0),
        )
        .expect("car json");
    recompute_car_efficiency(&transaction, v, 1).expect("second");
    let after_second: String = transaction
        .query_row(
            "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
            params![v.to_string()],
            |row| row.get(0),
        )
        .expect("car json");
    transaction.commit().expect("commit");
    assert_eq!(after_first, after_second);
    assert_eq!(eff_car_mutations(&store, v), 1);
    // A later call in a new transaction is also a no-op.
    eff_recompute(&store, v);
    assert_eq!(eff_car_mutations(&store, v), 1);
}

#[test]
fn r66_no_car_row_means_nothing_is_written() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    for id in 1..=3 {
        eff_put(&store, v, id, 24.00);
    }
    eff_recompute(&store, v);
    assert!(store.materialised_car_for_vehicle(v).expect("read").is_none());
    assert_eq!(eff_car_mutations(&store, v), 0);
}

#[test]
fn r66_the_sync_mutation_carries_the_new_car_row() {
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, None);
    for id in 1..=3 {
        eff_put(&store, v, id, 24.00);
    }
    eff_recompute(&store, v);
    let (entity_id, car_id, operation, payload): (i64, i64, String, String) = store
        .open()
        .expect("connection")
        .query_row(
            "SELECT entity_id, car_id, operation, payload_json FROM sync_mutations
             WHERE vehicle_id = ?1 AND entity = 'car'",
            params![v.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the one car mutation");
    assert_eq!((entity_id, car_id, operation.as_str()), (1, 1, "upsert"));
    let published: crate::hub_pack::ProjectionCar = serde_json::from_str(&payload).expect("json");
    assert_eq!(published, eff_car(Some(150.0)));
}

#[test]
fn t64_the_scan_reads_at_most_two_hundred_rows() {
    // Store A: three Hub AC rows (ids 1 to 3, figure 150.0) and 197 newer Hub
    // DC rows (ids 4 to 200): row 1 is the 200th newest row, still read.
    let (_directory_a, store_a, vehicle_a) = eff_store();
    let a = vehicle_a.vehicle_id;
    eff_put_car(&store_a, a, Some(162.5));
    for id in 1..=3 {
        eff_put(&store_a, a, id, 24.00);
    }
    for id in 4..=200 {
        let mut row = cost_row(id, None, None, Some(true), None);
        row.car_id = 1;
        store_a
            .open()
            .expect("connection")
            .execute(
                "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
                 VALUES (?1, ?2, 1, ?3)",
                params![a.to_string(), id, serde_json::to_string(&row).expect("json")],
            )
            .expect("dc row");
    }
    eff_recompute(&store_a, a);
    assert_eq!(eff_car_of(&store_a, a).efficiency_wh_per_km, Some(150.0));

    // Store B: the same plus one more DC row (id 201): row 1 falls outside the
    // 200-row window, two figures remain and the value stays as it was.
    let (_directory_b, store_b, vehicle_b) = eff_store();
    let b = vehicle_b.vehicle_id;
    eff_put_car(&store_b, b, Some(162.5));
    for id in 1..=3 {
        eff_put(&store_b, b, id, 24.00);
    }
    for id in 4..=201 {
        let mut row = cost_row(id, None, None, Some(true), None);
        row.car_id = 1;
        store_b
            .open()
            .expect("connection")
            .execute(
                "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
                 VALUES (?1, ?2, 1, ?3)",
                params![b.to_string(), id, serde_json::to_string(&row).expect("json")],
            )
            .expect("dc row");
    }
    eff_recompute(&store_b, b);
    assert_eq!(eff_car_of(&store_b, b).efficiency_wh_per_km, Some(162.5));
    assert_eq!(eff_car_mutations(&store_b, b), 0);
}

#[test]
fn t65_an_undecodable_row_is_skipped_and_counted_and_never_blocks() {
    arm_fill_log_capture();
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, Some(162.5));
    for id in 1..=3 {
        eff_put(&store, v, id, 24.00);
    }
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, 4, 1, '{}')",
            params![v.to_string()],
        )
        .expect("undecodable row");
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(150.0));
    let logs = eff_plain(take_fill_logs());
    assert!(logs.contains("skipped=1"), "one skipped row counted: {logs}");
    assert!(logs.contains(&v.to_string()), "the vehicle id is logged: {logs}");
}

#[test]
fn t65_an_undecodable_sample_gives_its_session_no_figure_and_is_counted() {
    arm_fill_log_capture();
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, Some(162.5));
    for id in 1..=3 {
        eff_put(&store, v, id, 24.00);
    }
    // A newer AC session at 200.0 whose middle sample no longer decodes: it
    // would have made four figures with a median of 175.0.
    eff_put(&store, v, 4, 32.00);
    store
        .open()
        .expect("connection")
        .execute(
            "UPDATE materialised_charge_samples SET sample_json = '{}'
             WHERE vehicle_id = ?1 AND sample_id = 401",
            params![v.to_string()],
        )
        .expect("corrupt sample");
    eff_recompute(&store, v);
    assert_eq!(eff_car_of(&store, v).efficiency_wh_per_km, Some(150.0));
    let logs = eff_plain(take_fill_logs());
    assert!(logs.contains("skipped=1"), "one skipped session counted: {logs}");
}

#[test]
fn t65_a_clean_scan_logs_nothing() {
    arm_fill_log_capture();
    let (_directory, store, vehicle) = eff_store();
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, None);
    for id in 1..=3 {
        eff_put(&store, v, id, 24.00);
    }
    eff_recompute(&store, v);
    assert!(!eff_plain(take_fill_logs()).contains("skipped"));
}

#[test]
fn t65_a_commit_that_closes_a_charge_succeeds_beside_an_undecodable_row() {
    let mut harness = CostHarness::new();
    let v = harness.vehicle.vehicle_id;
    eff_put_car(&harness.store, v, Some(162.5));
    harness
        .store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, 90, 1, '{}')",
            params![v.to_string()],
        )
        .expect("undecodable row");
    harness.feed_all(&gap_sequence(false));
    let closed = harness
        .store
        .materialised_charge_for_vehicle(v, 1)
        .expect("read")
        .expect("the close committed its own row");
    assert!(closed.end_date_ms.is_some());
}

/// The idle half of R67: a commit that closes no charge never recomputes,
/// even when the stored sessions would change the car.
#[test]
fn r67_a_commit_that_closes_no_charge_leaves_the_car_alone() {
    let harness = CostHarness::new();
    let v = harness.vehicle.vehicle_id;
    eff_put_car(&harness.store, v, Some(162.5));
    for id in 11..=13 {
        eff_put(&harness.store, v, id, 24.00);
    }
    harness.commit_delta(&crate::lifecycle::LifecycleDelta::default());
    assert_eq!(
        eff_car_of(&harness.store, v).efficiency_wh_per_km,
        Some(162.5)
    );
    assert_eq!(eff_car_mutations(&harness.store, v), 0);
    // The same commit with one closed charge recomputes (R67).
    let mut delta = crate::lifecycle::LifecycleDelta::default();
    delta.charges.push(cost_row(21, None, None, Some(false), None));
    harness.commit_delta(&delta);
    assert_eq!(
        eff_car_of(&harness.store, v).efficiency_wh_per_km,
        Some(150.0)
    );
    assert_eq!(eff_car_mutations(&harness.store, v), 1);
}

/// What T61 compares between the uninterrupted run and the restarted one.
fn t61_run(
    restart: bool,
) -> (
    crate::hub_pack::ProjectionCharge,
    crate::hub_pack::ProjectionCar,
    i64,
) {
    let root = crate::private_tempdir().expect("tempdir");
    let mut store = HubStore::initialize(root.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            COST_T0,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "9"), COST_T0)
        .expect("vehicle");
    let v = vehicle.vehicle_id;
    eff_put_car(&store, v, Some(162.5));
    // Three closed Hub AC rows with figures 150.0, 150.0 and 160.0.
    for (id, x) in [(1, 24.00), (2, 24.00), (3, 25.60)] {
        eff_put(&store, v, id, x);
    }
    // The lifecycle state allocates above the seeded rows (C1 R10).
    let mut state = crate::lifecycle::OpenSessionState::new();
    state.next_charge_id = 4;
    state.next_charge_sample_id = 1_000;
    let blob = state.encode().expect("encode");
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: v,
            car_id: 1,
            open_session_json: &blob,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: COST_T0,
            delta: &crate::lifecycle::LifecycleDelta::default(),
        })
        .expect("seed the lifecycle state");

    let first = |level: i64, range_miles: f64, counter: f64| {
        let mut fields = cost_a7(Some(counter));
        fields["battery_level"] = serde_json::json!(level);
        fields["battery_range"] = serde_json::json!(range_miles);
        fields
    };
    let middle = |level: i64, counter: f64| {
        let mut fields = cost_a7(Some(counter));
        fields["battery_level"] = serde_json::json!(level);
        fields
    };
    let observations = [
        cost_ob(1, COST_T0, "Charging", first(50, 124.27, 0.00)),
        cost_ob(2, COST_T0 + 10 * COST_MIN, "Charging", middle(55, 1.08)),
        cost_ob(3, COST_T0 + 20 * COST_MIN, "Charging", middle(60, 2.16)),
        cost_ob(
            4,
            COST_T0 + 3 * COST_HOUR,
            "Complete",
            serde_json::json!({
                "charge_energy_added": 15.20, "charger_power": 0,
                "charger_voltage": 240, "charger_actual_current": 0,
                "battery_level": 70, "battery_range": 186.41,
            }),
        ),
    ];
    for (index, sample) in observations.iter().enumerate() {
        store
            .accept_owner_observation_and_lifecycle(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: v,
                    observed_at_ms: sample.observed_at_ms,
                    payload: sample.payload.clone(),
                },
                sample.observed_at_ms,
                1,
            )
            .expect("observation");
        if restart && index == 1 {
            drop(store);
            store = HubStore::initialize(root.path()).expect("reopened store");
        }
    }
    let charge = store
        .materialised_charge_for_vehicle(v, 4)
        .expect("read charge")
        .expect("the closed charge, allocated above the seeded rows");
    (charge, eff_car_of(&store, v), eff_car_mutations(&store, v))
}

#[test]
fn t61_a_restart_mid_charge_gives_the_same_row_and_car_as_an_uninterrupted_run() {
    let (charge, car, mutations) = t61_run(false);
    assert_eq!(charge.charge_energy_added, Some(15.20));
    assert_eq!(charge.charge_energy_used_kwh, Some(16.89));
    // The new session's figure is 15200 / 100.01 = 151.98; with 150.0, 150.0
    // and 160.0 the median of four is (150.0 + 151.98) / 2 = 150.99, 151.0.
    assert_eq!(car.efficiency_wh_per_km, Some(151.0));
    assert_eq!(mutations, 1, "one car change for the one close");
    let (restarted_charge, restarted_car, restarted_mutations) = t61_run(true);
    assert_eq!(restarted_charge, charge);
    assert_eq!(restarted_car, car);
    assert_eq!(restarted_mutations, mutations);
}
