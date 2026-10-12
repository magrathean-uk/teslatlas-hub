// SPDX-License-Identifier: AGPL-3.0-only

// C1 integration tests through the store: the shared location of the unit's
// constants and helpers (ST92, R83), live ids above the import watermarks
// (ST93, R84) and the imported version seed through the real import path
// (ST87, R14). ST93 (d) runs through the collector in
// `src/collection/collector/tests/projection_and_outbox.rs`. Synthetic data
// only. This module sits outside `src/runtime/lifecycle/`, so every call below
// uses the `crate::lifecycle::` path another unit would use.

#[test]
fn st92_the_shared_items_are_reachable_from_outside_the_lifecycle_module() {
    const T0: i64 = 1_760_000_000_000;
    assert_eq!(crate::lifecycle::K_EARLIEST_MS, 1_325_376_000_000);
    assert_eq!(crate::lifecycle::K_SKEW_MS, 60_000);
    assert_eq!(
        crate::lifecycle::DEFAULT_OFFLINE_DRIVE_TIMEOUT,
        std::time::Duration::from_secs(600)
    );
    assert_eq!(crate::lifecycle::K_MILE_KM, 1.609_344);
    assert_eq!(crate::lifecycle::miles_to_km(100.0), 160.9344);
    assert_eq!(crate::lifecycle::miles_to_km_rounded(200.0, 2), 321.87);
    assert_eq!(crate::lifecycle::mph_to_kmh(25.4), 41);
    assert_eq!(crate::lifecycle::round_to_precision(2.875, 1), 2.9);
    for (text, canonical) in [
        ("ShiftStateD", Some("D")),
        ("DetailedChargeStateCharging", Some("Charging")),
        (" charging ", Some("Charging")),
        ("SNA", Some("SNA")),
        ("Bogus", None),
    ] {
        assert_eq!(
            crate::lifecycle::normalise_enum_text(text),
            canonical,
            "{text:?}"
        );
    }
    let numeric = serde_json::json!({"timestamp": "1760000000000"});
    assert_eq!(
        crate::lifecycle::timestamp_field(numeric.as_object().expect("object"), "timestamp"),
        Some(T0)
    );
    let group_at = |timestamp: i64| serde_json::json!({ "timestamp": timestamp });
    let future = group_at(T0 + 60_001);
    assert_eq!(
        crate::lifecycle::group_time(future.as_object(), T0),
        T0,
        "a group time beyond the skew allowance falls back to the observation"
    );
    let near_past = group_at(T0 - 5_000);
    assert_eq!(
        crate::lifecycle::group_time(near_past.as_object(), T0),
        T0 - 5_000
    );
    let edge = group_at(T0 + 60_000);
    assert_eq!(
        crate::lifecycle::group_time(edge.as_object(), T0),
        T0 + 60_000,
        "the allowance itself is inclusive"
    );
    let before_floor = group_at(crate::lifecycle::K_EARLIEST_MS - 1);
    assert_eq!(
        crate::lifecycle::group_time(before_floor.as_object(), T0),
        T0
    );
    assert_eq!(crate::lifecycle::group_time(None, T0), T0);
}

/// Store, import-kind source and vehicle, as the import seeding path finds
/// them.
fn watermark_store(label: &str) -> (tempfile::TempDir, HubStore, SourceRecord, VehicleRecord) {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", label), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-import"),
            1_001,
        )
        .expect("vehicle");
    (directory, store, source, vehicle)
}

/// An imported open charge (id 7, samples 1 and 2) carrying the watermarks of
/// ST93: drives 40, positions 500, charging processes 50, charges 900, states
/// 30 and updates 12.
fn st93_session(start: i64) -> crate::teslamate_projection::TeslaMateOpenSession {
    let mut session = imported_open_session(start);
    let watermarks = &mut session.watermarks;
    watermarks.drives.max_id = Some(40);
    watermarks.positions.max_id = Some(500);
    watermarks.charging_processes.max_id = Some(50);
    watermarks.charges.max_id = Some(900);
    watermarks.states.max_id = Some(30);
    watermarks.updates.max_id = Some(12);
    session
}

fn id_cursors(store: &HubStore, vehicle: Uuid) -> [i64; 6] {
    let record = store
        .load_lifecycle_state(vehicle)
        .expect("state")
        .expect("seeded state");
    let state =
        crate::lifecycle::OpenSessionState::decode(&record.open_session_json).expect("decode");
    [
        state.next_drive_id,
        state.next_position_id,
        state.next_charge_id,
        state.next_charge_sample_id,
        state.next_state_id,
        state.next_update_id,
    ]
}

fn put_watermark(
    store: &HubStore,
    source: Uuid,
    vehicle: Uuid,
    domain: &str,
    largest: Option<i64>,
) {
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO lifecycle_source_watermarks(
                source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms
             ) VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(source_id, vehicle_id, domain) DO UPDATE SET
                max_source_row_id = excluded.max_source_row_id",
            params![source.to_string(), vehicle.to_string(), domain, largest],
        )
        .expect("watermark row");
}

#[test]
fn st93_a_seed_raises_every_cursor_above_its_import_watermark() {
    let start = COST_T0 - COST_HOUR;
    let (_directory, store, source, vehicle) = watermark_store("st93-a");
    store
        .seed_imported_open_session(
            source.source_id,
            vehicle.vehicle_id,
            1,
            &st93_session(start),
            start,
        )
        .expect("seed");
    assert_eq!(
        id_cursors(&store, vehicle.vehicle_id),
        [41, 501, 51, 901, 31, 13]
    );
}

#[test]
fn st93_live_charges_and_samples_take_ids_above_the_watermarks() {
    let start = COST_T0 - COST_HOUR;
    let (_directory, store, source, vehicle) = watermark_store("st93-b");
    store
        .seed_imported_open_session(
            source.source_id,
            vehicle.vehicle_id,
            1,
            &st93_session(start),
            start,
        )
        .expect("seed");
    let free = |counter: f64| serde_json::json!({"charge_energy_added": counter});
    let live = vec![
        cost_ob(1, start + 7 * COST_MIN, "Charging", free(3.00)),
        cost_ob(2, start + 9 * COST_MIN, "Complete", free(4.00)),
        cost_ob(
            3,
            start + 9 * COST_MIN + COST_HOUR,
            "Disconnected",
            serde_json::json!({}),
        ),
    ];
    let second = start + 9 * COST_MIN + 2 * COST_HOUR;
    let second_charge = vec![
        cost_ob(4, second, "Charging", cost_a7(Some(0.00))),
        cost_ob(5, second + 10 * COST_MIN, "Charging", cost_a7(Some(1.08))),
        cost_ob(6, second + 20 * COST_MIN, "Complete", cost_a7(Some(2.16))),
    ];
    feed_owner_samples(&store, &source, &vehicle, &[live, second_charge].concat());
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(
        history.charges.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![7, 51],
        "the new charge is 51, not 8"
    );
    let new_samples = history
        .charge_samples
        .iter()
        .filter(|sample| sample.charge_process_id == 51)
        .map(|sample| sample.id)
        .collect::<Vec<_>>();
    assert_eq!(new_samples.len(), 3);
    assert!(new_samples.iter().all(|id| *id > 900), "{new_samples:?}");
}

#[test]
fn st93_a_later_watermark_raises_the_cursors_of_a_seeded_state() {
    let start = COST_T0 - COST_HOUR;
    let (_directory, store, source, vehicle) = watermark_store("st93-c");
    let v = vehicle.vehicle_id;
    store
        .seed_imported_open_session(source.source_id, v, 1, &st93_session(start), start)
        .expect("seed");
    // A later promotion raises the positions watermark to 800.
    let mut later = st93_session(start);
    later.watermarks.positions.max_id = Some(800);
    store
        .seed_imported_open_session(source.source_id, v, 1, &later, start + COST_MIN)
        .expect("later seed");
    assert_eq!(id_cursors(&store, v), [41, 801, 51, 901, 31, 13]);

    // A watermark row without an id, or below a cursor, changes nothing.
    let mut lower = st93_session(start);
    lower.watermarks.positions.max_id = None;
    lower.watermarks.drives.max_id = Some(3);
    store
        .seed_imported_open_session(source.source_id, v, 1, &lower, start + 2 * COST_MIN)
        .expect("lower seed");
    assert_eq!(id_cursors(&store, v), [41, 801, 51, 901, 31, 13]);
}

#[test]
fn st93_the_watermarks_of_every_source_bound_the_cursors() {
    let start = COST_T0 - COST_HOUR;
    let (_directory, store, source, vehicle) = watermark_store("st93-d");
    let v = vehicle.vehicle_id;
    store
        .seed_imported_open_session(source.source_id, v, 1, &st93_session(start), start)
        .expect("seed");
    // Another source holds larger ids of this vehicle, and a row without one.
    let other = store
        .register_source(&SourceDescriptor::new("teslamate", "st93-other"), 1_002)
        .expect("other source")
        .source_id;
    put_watermark(&store, other, v, "positions", Some(2_000));
    put_watermark(&store, other, v, "charges", None);
    let mut changed = st93_session(start);
    changed.watermarks.states.max_id = Some(31);
    store
        .seed_imported_open_session(source.source_id, v, 1, &changed, start + COST_MIN)
        .expect("reseed");
    assert_eq!(
        id_cursors(&store, v),
        [41, 2_001, 51, 901, 32, 13],
        "positions follow the other source, charges ignore its NULL"
    );
}

#[test]
fn st87_the_installed_version_of_a_real_import_seeds_the_first_update_row() {
    for (live, rows) in [("2024.20.1 bb", 1), ("2024.14.3 aa", 0), ("2024.8.9", 0)] {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let drive = import_materialised_test_drive(7, 10, 4.0);
        let (vehicle, binding, _car, _) = direct_materialised_base_with_firmware(
            &store,
            std::slice::from_ref(&drive),
            Some("2024.14.3 aa"),
        );
        let record = store
            .load_lifecycle_state(vehicle.vehicle_id)
            .expect("state")
            .expect("seeded by the import");
        let seeded =
            crate::lifecycle::OpenSessionState::decode(&record.open_session_json).expect("decode");
        assert_eq!(seeded.last_car_version.as_deref(), Some("2024.14.3 aa"));
        // The importer keeps the version text but not the update's end date,
        // so the lower bound is the end of the newest imported drive.
        assert_eq!(seeded.last_car_version_at_ms, Some(drive.end_date_ms));

        let at = drive.end_date_ms + 2 * 24 * COST_HOUR;
        let payload = serde_json::json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "10",
            "source_vehicle_state": "online",
            "provider_raw_json": {"response": {
                "vehicle_state": {"timestamp": at, "car_version": live}
            }},
        });
        store
            .accept_owner_observation_and_lifecycle(
                &ObservationInput {
                    source_id: binding.account_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: at,
                    payload,
                },
                at,
                binding.selected_car_id,
            )
            .expect("live observation");
        let updates = store
            .materialised_history(vehicle.vehicle_id)
            .expect("history")
            .updates;
        assert_eq!(updates.len(), rows, "live {live}");
        if rows == 1 {
            assert_eq!(updates[0].start_date_ms, drive.end_date_ms);
            assert_eq!(updates[0].end_date_ms, at);
            assert_eq!(updates[0].version, "2024.20.1 bb");
        }
    }
}
