// SPDX-License-Identifier: AGPL-3.0-only

// Store queries behind the hub-http-v1@1.2.0 history routes: the visibility
// predicate, the closed-session filters, the keyset windows, the argument
// validation and the query plans. Synthetic rows only.

fn public_history_store() -> (tempfile::TempDir, HubStore, Uuid) {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api", "public-history-queries"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "9").with_tesla_identity(Some(9), None),
            1_000,
        )
        .expect("vehicle");
    (temporary, store, vehicle.vehicle_id)
}

fn public_history_publish(store: &HubStore, vehicle_id: Uuid) {
    store
        .open()
        .unwrap()
        .execute(
            "INSERT INTO sync_manifests(snapshot_id, vehicle_id, head_sequence, manifest_json)
             VALUES (?1, ?2, 1, ?3)",
            params![
                Uuid::new_v4().to_string(),
                vehicle_id.to_string(),
                b"{}".as_slice()
            ],
        )
        .unwrap();
}

fn public_history_drive_json(id: i64, start: i64, end: Option<i64>) -> String {
    serde_json::json!({
        "id": id, "car_id": 1, "start_date_ms": start, "end_date_ms": end,
        "optimized_at_ms": null
    })
    .to_string()
}

fn public_history_charge_json(id: i64, start: i64, end: Option<i64>) -> String {
    serde_json::json!({
        "id": id, "car_id": 1, "start_date_ms": start, "end_date_ms": end,
        "charge_energy_added": null, "start_battery_level": null,
        "end_battery_level": null, "duration_min": null, "address": null,
        "location_name": null, "geofence": null, "is_dc": null,
        "charge_rate_km_per_hour": null, "max_charger_power_kw": null,
        "outside_temp_avg": null, "start_rated_range_km": null,
        "end_rated_range_km": null
    })
    .to_string()
}

fn public_history_position_json(id: i64, drive_id: i64, date_ms: i64) -> String {
    serde_json::json!({
        "id": id, "drive_id": drive_id, "car_id": 1, "date_ms": date_ms,
        "latitude": 0.0, "longitude": 0.0, "speed": null, "power": null,
        "battery_level": null, "usable_battery_level": null, "elevation": null,
        "odometer": null, "ideal_battery_range_km": null
    })
    .to_string()
}

fn public_history_sample_json(id: i64, charge_id: i64, timestamp_ms: i64) -> String {
    serde_json::json!({
        "id": id, "charge_process_id": charge_id, "timestamp_ms": timestamp_ms,
        "battery_level": null, "usable_battery_level": null,
        "charge_energy_added_kwh": null, "charger_power_kw": null,
        "charger_voltage": null, "charger_actual_current": null,
        "charger_pilot_current": null, "charger_phases": null,
        "ideal_range_km": null, "rated_range_km": null, "outside_temp_c": null,
        "battery_heater_on": null, "battery_heater": null,
        "battery_heater_no_power": null, "not_enough_power_to_heat": null,
        "fast_charger_present": null, "fast_charger_brand": null,
        "fast_charger_type": null, "charge_cable": null
    })
    .to_string()
}

fn public_history_plan(store: &HubStore, sql: &str, parameters: &[&dyn rusqlite::ToSql]) -> String {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("query plan");
    statement
        .query_map(parameters, |row| row.get::<_, String>(3))
        .expect("plan rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("plan details")
        .join("\n")
}

#[test]
fn public_visibility_is_active_and_published_like_the_vehicle_list() {
    let (_temporary, store, vehicle_id) = public_history_store();
    // Active but unpublished: visible to neither the list nor the history.
    assert!(!store.vehicle_is_published_and_active(vehicle_id).unwrap());
    assert!(store.published_vehicles().unwrap().is_empty());
    public_history_publish(&store, vehicle_id);
    assert!(store.vehicle_is_published_and_active(vehicle_id).unwrap());
    assert_eq!(store.published_vehicles().unwrap().len(), 1);
    // Retired: invisible again, as in the list.
    assert!(store.retire_vehicle(vehicle_id, 2_000).unwrap());
    assert!(!store.vehicle_is_published_and_active(vehicle_id).unwrap());
    assert!(store.published_vehicles().unwrap().is_empty());
    // Unknown, and the nil UUID is a caller error.
    assert!(
        !store
            .vehicle_is_published_and_active(Uuid::new_v4())
            .unwrap()
    );
    assert!(matches!(
        store.vehicle_is_published_and_active(Uuid::nil()),
        Err(StoreError::NilVehicleId)
    ));
}

#[test]
fn drive_pages_and_items_leave_out_drives_without_an_end_date() {
    let (_temporary, store, vehicle_id) = public_history_store();
    let connection = store.open().unwrap();
    for (id, start, end) in [
        (1, 1_000, Some(1_500)),
        (2, 2_000, None),
        (3, 3_000, Some(3_500)),
    ] {
        connection
            .execute(
                "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
                 VALUES (?1, ?2, 1, ?3)",
                params![
                    vehicle_id.to_string(),
                    id,
                    public_history_drive_json(id, start, end)
                ],
            )
            .unwrap();
    }
    drop(connection);
    // The page does not need the vehicle to be published: the caller checked.
    let page = store
        .materialised_drives_page(vehicle_id, 0, i64::MAX, None, 10)
        .expect("page");
    assert_eq!(
        page.iter().map(|drive| drive.id).collect::<Vec<_>>(),
        [3, 1]
    );
    assert!(
        store
            .materialised_closed_drive_for_vehicle(vehicle_id, 2)
            .unwrap()
            .is_none(),
        "an unfinished drive reads as absent"
    );
    assert_eq!(
        store
            .materialised_closed_drive_for_vehicle(vehicle_id, 3)
            .unwrap()
            .map(|drive| drive.id),
        Some(3)
    );
    assert!(
        store
            .materialised_closed_drive_for_vehicle(vehicle_id, 4)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.materialised_closed_drive_for_vehicle(vehicle_id, 0),
        Err(StoreError::InvalidDriveId)
    ));
    assert!(matches!(
        store.materialised_closed_drive_for_vehicle(Uuid::nil(), 1),
        Err(StoreError::NilVehicleId)
    ));
}

#[test]
fn charge_pages_and_items_serve_closed_sessions_newest_first_by_keyset() {
    let (_temporary, store, vehicle_id) = public_history_store();
    let connection = store.open().unwrap();
    for (id, start, end) in [
        (1, 1_000, Some(1_500)),
        (2, 2_000, Some(2_500)),
        (3, 2_000, Some(2_600)),
        (4, 3_000, None),
        (5, 4_000, Some(4_500)),
    ] {
        connection
            .execute(
                "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
                 VALUES (?1, ?2, 1, ?3)",
                params![
                    vehicle_id.to_string(),
                    id,
                    public_history_charge_json(id, start, end)
                ],
            )
            .unwrap();
    }
    drop(connection);
    let ids = |page: Vec<crate::hub_pack::ProjectionCharge>| {
        page.into_iter().map(|charge| charge.id).collect::<Vec<_>>()
    };
    assert_eq!(
        ids(store
            .materialised_charges_page(vehicle_id, 0, i64::MAX, None, 10)
            .unwrap()),
        [5, 3, 2, 1],
        "newest first, ties by id, the unfinished charge 4 left out"
    );
    // Keyset: strictly before (2000, 3) continues with charge 2.
    assert_eq!(
        ids(store
            .materialised_charges_page(vehicle_id, 0, i64::MAX, Some((2_000, 3)), 10)
            .unwrap()),
        [2, 1]
    );
    // Half-open window on the start date.
    assert_eq!(
        ids(store
            .materialised_charges_page(vehicle_id, 2_000, 4_000, None, 10)
            .unwrap()),
        [3, 2]
    );
    assert_eq!(
        ids(store
            .materialised_charges_page(vehicle_id, 0, i64::MAX, None, 2)
            .unwrap()),
        [5, 3],
        "the limit bounds the rows"
    );
    assert!(
        store
            .materialised_charge_for_vehicle(vehicle_id, 4)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .materialised_charge_for_vehicle(vehicle_id, 5)
            .unwrap()
            .map(|charge| charge.id),
        Some(5)
    );
    assert!(
        store
            .materialised_charge_for_vehicle(vehicle_id, 9)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.materialised_charge_for_vehicle(vehicle_id, 0),
        Err(StoreError::InvalidChargeId)
    ));
}

#[test]
fn position_and_sample_pages_are_keyset_windows_of_one_parent() {
    let (_temporary, store, vehicle_id) = public_history_store();
    let connection = store.open().unwrap();
    for (id, drive, date) in [
        (1, 7, 1_000),
        (2, 7, 1_100),
        (3, 7, 1_100),
        (4, 7, 1_300),
        (5, 8, 1_050),
    ] {
        connection
            .execute(
                "INSERT INTO materialised_positions(
                     vehicle_id, position_id, drive_id, car_id, position_json
                 ) VALUES (?1, ?2, ?3, 1, ?4)",
                params![
                    vehicle_id.to_string(),
                    id,
                    drive,
                    public_history_position_json(id, drive, date)
                ],
            )
            .unwrap();
    }
    for (id, charge, timestamp) in [
        (1, 7, 1_000),
        (2, 7, 1_100),
        (3, 7, 1_100),
        (4, 7, 1_300),
        (5, 8, 1_050),
    ] {
        connection
            .execute(
                "INSERT INTO materialised_charge_samples(
                     vehicle_id, sample_id, charge_id, sample_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    vehicle_id.to_string(),
                    id,
                    charge,
                    public_history_sample_json(id, charge, timestamp)
                ],
            )
            .unwrap();
    }
    drop(connection);
    let position_ids = |rows: Vec<PublicPositionRow>| {
        rows.into_iter()
            .map(|row| row.position.id)
            .collect::<Vec<_>>()
    };
    let sample_ids = |rows: Vec<crate::hub_pack::ProjectionChargeSample>| {
        rows.into_iter().map(|row| row.id).collect::<Vec<_>>()
    };
    // Oldest first, ties by id, only the parent's rows.
    assert_eq!(
        position_ids(
            store
                .drive_positions_window_page(vehicle_id, 7, 0, i64::MAX, None, 10)
                .unwrap()
        ),
        [1, 2, 3, 4]
    );
    assert_eq!(
        sample_ids(
            store
                .charge_samples_page(vehicle_id, 7, 0, i64::MAX, None, 10)
                .unwrap()
        ),
        [1, 2, 3, 4]
    );
    // Keyset: strictly after (1100, 2) continues with id 3 at the same date.
    assert_eq!(
        position_ids(
            store
                .drive_positions_window_page(vehicle_id, 7, 0, i64::MAX, Some((1_100, 2)), 10)
                .unwrap()
        ),
        [3, 4]
    );
    assert_eq!(
        sample_ids(
            store
                .charge_samples_page(vehicle_id, 7, 0, i64::MAX, Some((1_100, 2)), 10)
                .unwrap()
        ),
        [3, 4]
    );
    // Half-open window [1100, 1300).
    assert_eq!(
        position_ids(
            store
                .drive_positions_window_page(vehicle_id, 7, 1_100, 1_300, None, 10)
                .unwrap()
        ),
        [2, 3]
    );
    assert_eq!(
        sample_ids(
            store
                .charge_samples_page(vehicle_id, 7, 1_100, 1_300, None, 10)
                .unwrap()
        ),
        [2, 3]
    );
    // The limit bounds the rows; another parent's rows stay apart.
    assert_eq!(
        position_ids(
            store
                .drive_positions_window_page(vehicle_id, 7, 0, i64::MAX, None, 2)
                .unwrap()
        ),
        [1, 2]
    );
    assert_eq!(
        position_ids(
            store
                .drive_positions_window_page(vehicle_id, 8, 0, i64::MAX, None, 10)
                .unwrap()
        ),
        [5]
    );
    assert!(
        store
            .drive_positions_window_page(vehicle_id, 9, 0, i64::MAX, None, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn public_page_queries_validate_their_arguments() {
    let (_temporary, store, vehicle_id) = public_history_store();
    let invalid = |result: Result<usize, StoreError>, reason: &str| match result {
        Err(StoreError::InvalidPublicPageQuery(found)) => assert_eq!(found, reason),
        other => panic!("expected invalid {reason}, got {other:?}"),
    };
    // (from, to, cursor, limit) combinations that are never valid.
    let cases: [(i64, i64, Option<(i64, i64)>, u32, &str); 9] = [
        (-1, 10, None, 1, "time range"),
        (0, -1, None, 1, "time range"),
        (5, 5, None, 1, "time range"),
        (6, 5, None, 1, "time range"),
        (0, 10, None, 0, "limit"),
        (0, 10, Some((-1, 1)), 1, "cursor"),
        (5, 10, Some((4, 1)), 1, "cursor"),
        (0, 10, Some((10, 1)), 1, "cursor"),
        (0, 10, Some((5, 0)), 1, "cursor"),
    ];
    for (from_ms, to_ms, cursor, limit, reason) in cases {
        invalid(
            store
                .drive_positions_window_page(vehicle_id, 1, from_ms, to_ms, cursor, limit)
                .map(|rows| rows.len()),
            reason,
        );
        invalid(
            store
                .charge_samples_page(vehicle_id, 1, from_ms, to_ms, cursor, limit)
                .map(|rows| rows.len()),
            reason,
        );
        invalid(
            store
                .materialised_charges_page(vehicle_id, from_ms, to_ms, cursor, limit)
                .map(|rows| rows.len()),
            reason,
        );
    }
    // The fetch bounds: one row more than the route's largest page.
    assert!(
        store
            .drive_positions_window_page(vehicle_id, 1, 0, 10, None, 2_049)
            .is_ok()
    );
    invalid(
        store
            .drive_positions_window_page(vehicle_id, 1, 0, 10, None, 2_050)
            .map(|rows| rows.len()),
        "limit",
    );
    assert!(
        store
            .charge_samples_page(vehicle_id, 1, 0, 10, None, 2_049)
            .is_ok()
    );
    invalid(
        store
            .charge_samples_page(vehicle_id, 1, 0, 10, None, 2_050)
            .map(|rows| rows.len()),
        "limit",
    );
    assert!(
        store
            .materialised_charges_page(vehicle_id, 0, 10, None, 501)
            .is_ok()
    );
    invalid(
        store
            .materialised_charges_page(vehicle_id, 0, 10, None, 502)
            .map(|rows| rows.len()),
        "limit",
    );
    // Nil vehicle and non-positive parents are caller errors.
    assert!(matches!(
        store.drive_positions_window_page(Uuid::nil(), 1, 0, 10, None, 1),
        Err(StoreError::NilVehicleId)
    ));
    assert!(matches!(
        store.drive_positions_window_page(vehicle_id, 0, 0, 10, None, 1),
        Err(StoreError::InvalidDriveId)
    ));
    assert!(matches!(
        store.charge_samples_page(Uuid::nil(), 1, 0, 10, None, 1),
        Err(StoreError::NilVehicleId)
    ));
    assert!(matches!(
        store.charge_samples_page(vehicle_id, -1, 0, 10, None, 1),
        Err(StoreError::InvalidChargeId)
    ));
    assert!(matches!(
        store.materialised_charges_page(Uuid::nil(), 0, 10, None, 1),
        Err(StoreError::NilVehicleId)
    ));
}

#[test]
fn position_pages_report_terrain_lookups_only_when_they_succeeded() {
    let (_temporary, store, vehicle_id) = public_history_store();
    let connection = store.open().unwrap();
    for id in 1..=3_i64 {
        connection
            .execute(
                "INSERT INTO materialised_positions(
                     vehicle_id, position_id, drive_id, car_id, position_json
                 ) VALUES (?1, ?2, 7, 1, ?3)",
                params![
                    vehicle_id.to_string(),
                    id,
                    public_history_position_json(id, 7, 1_000 + id)
                ],
            )
            .unwrap();
    }
    for (id, status) in [(1, "success"), (2, "void")] {
        connection
            .execute(
                "INSERT INTO terrain_elevation_provenance(
                     vehicle_id, position_id, drive_id, latitude, longitude, elevation_m,
                     tile_name, tile_hash, dataset_source, dataset_version, status,
                     error_code, attempts, attempted_at_ms, retry_after_ms
                 ) VALUES (?1, ?2, 7, 0.0, 0.0, NULL, 'tile', NULL, 'set', '2', ?3,
                           NULL, 1, 1000, 0)",
                params![vehicle_id.to_string(), id, status],
            )
            .unwrap();
    }
    drop(connection);
    let rows = store
        .drive_positions_window_page(vehicle_id, 7, 0, i64::MAX, None, 10)
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0].terrain,
        Some(PositionTerrainLookup {
            dataset: Some("set".to_owned()),
            dataset_version: Some("2".to_owned()),
            tile: Some("tile".to_owned()),
        })
    );
    assert_eq!(rows[1].terrain, None, "a void lookup is not provenance");
    assert_eq!(rows[2].terrain, None, "no lookup, no provenance");
}

#[test]
fn public_history_queries_are_bounded_and_indexed() {
    let (_temporary, store, vehicle_id) = public_history_store();
    let vehicle = vehicle_id.to_string();

    // Drives: the vehicle/time/cursor expression index, no sort (existing plan).
    let plan = public_history_plan(
        &store,
        PUBLIC_DRIVES_PAGE_SQL,
        params![vehicle, 0_i64, 10_i64, 10_i64, 10_i64, 10_i64],
    );
    assert!(plan.contains("materialised_drives_public_query"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");

    // Charges: no vehicle/time index exists. The plan is a search on the
    // vehicle prefix of the primary key (a bounded scan of ONE vehicle's
    // sessions, a few hundred rows) followed by a TEMP B-TREE sort. This
    // test documents and pins that choice (a schema change needs a migration,
    // which belongs to G2-3a): it must never become a full-table scan.
    let plan = public_history_plan(
        &store,
        PUBLIC_CHARGES_PAGE_SQL,
        params![vehicle, 0_i64, 10_i64, 10_i64, 10_i64, 10_i64],
    );
    assert!(
        plan.contains("SEARCH materialised_charges USING INDEX") && plan.contains("(vehicle_id=?)"),
        "charges must search the vehicle prefix of the primary key: {plan}"
    );
    assert!(
        !plan.contains("SCAN materialised_charges"),
        "charges must never scan the whole table: {plan}"
    );

    // Positions: the per-drive index; the join reads the terrain table by its key.
    let plan = public_history_plan(
        &store,
        PUBLIC_POSITIONS_PAGE_SQL,
        params![vehicle, 7_i64, 0_i64, 10_i64, 0_i64, 0_i64, 10_i64],
    );
    assert!(
        plan.contains("materialised_positions_drive (vehicle_id=? AND drive_id=?)"),
        "positions must use the per-drive index: {plan}"
    );
    assert!(
        plan.contains("(vehicle_id=? AND position_id=?)") && !plan.contains("SCAN t"),
        "the terrain join must read by primary key: {plan}"
    );

    // Samples: the per-charge index.
    let plan = public_history_plan(
        &store,
        PUBLIC_CHARGE_SAMPLES_PAGE_SQL,
        params![vehicle, 7_i64, 0_i64, 10_i64, 0_i64, 0_i64, 10_i64],
    );
    assert!(
        plan.contains("materialised_charge_samples_charge (vehicle_id=? AND charge_id=?)"),
        "samples must use the per-charge index: {plan}"
    );

    // Items: primary-key lookups.
    for (sql, table, key) in [
        (PUBLIC_DRIVE_ITEM_SQL, "materialised_drives", "drive_id"),
        (PUBLIC_CHARGE_ITEM_SQL, "materialised_charges", "charge_id"),
    ] {
        let plan = public_history_plan(&store, sql, params![vehicle, 7_i64]);
        assert!(
            plan.contains(&format!("SEARCH {table} USING INDEX"))
                && plan.contains(&format!("(vehicle_id=? AND {key}=?)")),
            "{table} item must be a primary-key lookup: {plan}"
        );
    }
}
