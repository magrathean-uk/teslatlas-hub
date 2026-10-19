// SPDX-License-Identifier: AGPL-3.0-only

// C2 B2 amendment D (R73), spec test T69: a repeated import snapshot is a
// no-op under the raise-only charge watermark. Synthetic data only.

const T69_T0: i64 = 1_760_000_000_000;
const T69_MIN: i64 = 60_000;

/// The imported open charge process 7 of T69 with samples `ids` (id, minutes
/// before t0, energy), and the watermarks of the snapshot.
fn t69_snapshot(
    samples: &[(i64, i64, f64)],
    processes: (Option<i64>, Option<i64>),
    charges: (Option<i64>, Option<i64>),
) -> crate::teslamate_projection::TeslaMateOpenSession {
    let process = serde_json::from_value(serde_json::json!({
        "id": 7, "car_id": 1, "position_id": null, "address_id": null,
        "geofence_id": null, "start_date_ms": T69_T0 - 30 * T69_MIN, "end_date_ms": null,
        "charge_energy_added": 2.0, "charge_energy_used_kwh": null,
        "start_ideal_range_km": 151.0, "end_ideal_range_km": 152.0,
        "start_battery_level": 40, "end_battery_level": 50,
        "duration_min": null, "outside_temp_avg": 11.0,
        "start_rated_range_km": 141.0, "end_rated_range_km": 142.0,
        "cost": null
    }))
    .expect("imported charging process");
    let sample = |id: i64, minutes_before: i64, energy: f64| {
        serde_json::from_value(serde_json::json!({
            "id": id, "charging_process_id": 7, "date_ms": T69_T0 - minutes_before * T69_MIN,
            "battery_heater": false, "battery_heater_on": false,
            "battery_heater_no_power": false, "battery_level": 50,
            "usable_battery_level": 50, "charge_energy_added_kwh": energy,
            "charger_actual_current": 16.0, "charger_phases": 3,
            "charger_pilot_current": 16.0, "charger_power_kw": 11.0,
            "charger_voltage": 230.0, "charge_cable": "IEC",
            "fast_charger_present": false, "fast_charger_brand": null,
            "fast_charger_type": null, "ideal_range_km": 150.0, "rated_range_km": 140.0,
            "not_enough_power_to_heat": false, "outside_temp_c": 10.0
        }))
        .expect("imported charge sample")
    };
    let mut session = crate::teslamate_projection::TeslaMateOpenSession {
        car_id: 1,
        charge: Some(process),
        charge_samples: samples
            .iter()
            .map(|(id, minutes, energy)| sample(*id, *minutes, *energy))
            .collect(),
        ..Default::default()
    };
    session.watermarks.charging_processes.max_id = processes.0;
    session.watermarks.charging_processes.max_timestamp_ms = processes.1;
    session.watermarks.charges.max_id = charges.0;
    session.watermarks.charges.max_timestamp_ms = charges.1;
    session
}

/// Snapshot S: samples 101 (t0 - 20 min) and 102 (t0 - 10 min).
fn t69_s(processes: (Option<i64>, Option<i64>)) -> crate::teslamate_projection::TeslaMateOpenSession {
    t69_snapshot(
        &[(101, 20, 1.00), (102, 10, 2.00)],
        processes,
        (Some(102), Some(T69_T0 - 10 * T69_MIN)),
    )
}

/// Snapshot S': S plus sample 103 (t0 - 5 min, energy 3.00), `charges` (103,
/// t0 - 5 min).
fn t69_s_prime(
    processes: (Option<i64>, Option<i64>),
) -> crate::teslamate_projection::TeslaMateOpenSession {
    t69_snapshot(
        &[(101, 20, 1.00), (102, 10, 2.00), (103, 5, 3.00)],
        processes,
        (Some(103), Some(T69_T0 - 5 * T69_MIN)),
    )
}

fn t69_dirty_revision(store: &HubStore, vehicle: Uuid) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT dirty_revision FROM export_outbox WHERE vehicle_id = ?1",
            params![vehicle.to_string()],
            |row| row.get(0),
        )
        .optional()
        .expect("outbox")
        .unwrap_or(0)
}

/// The vehicle's rows in the three tables a seed writes, column for column.
fn t69_rows(store: &HubStore, vehicle: Uuid) -> Vec<Vec<String>> {
    let connection = store.open().expect("connection");
    let key = vehicle.to_string();
    let mut rows = Vec::new();
    for (sql, columns) in [
        (
            "SELECT source_id, source_table, source_row_id, domain, parent_source_row_id, row_json
             FROM lifecycle_open_rows WHERE vehicle_id = ?1
             ORDER BY source_id, source_table, source_row_id",
            6,
        ),
        (
            "SELECT source_id, domain, max_source_row_id, max_timestamp_ms
             FROM lifecycle_source_watermarks WHERE vehicle_id = ?1
             ORDER BY source_id, domain",
            4,
        ),
        (
            "SELECT car_id, last_observation_id, open_session_json, quarantined, updated_at_ms
             FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
            5,
        ),
    ] {
        let mut statement = connection.prepare(sql).expect("statement");
        let table = statement
            .query_map(params![key], |row| {
                (0..columns)
                    .map(|index| {
                        row.get::<_, rusqlite::types::Value>(index)
                            .map(|value| format!("{value:?}"))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .expect("rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect");
        rows.extend(table);
    }
    rows
}

fn t69_charge_watermark(store: &HubStore, vehicle: Uuid) -> (Option<i64>, Option<i64>) {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT max_source_row_id, max_timestamp_ms FROM lifecycle_source_watermarks
             WHERE vehicle_id = ?1 AND domain = 'charging_processes'",
            params![vehicle.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("charging_processes watermark")
}

fn t69_store(label: &str) -> (tempfile::TempDir, HubStore, SourceRecord, VehicleRecord) {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", label), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "vehicle-t69"), 1_001)
        .expect("vehicle");
    (directory, store, source, vehicle)
}

fn t69_seed(
    store: &HubStore,
    source: &SourceRecord,
    vehicle: &VehicleRecord,
    session: &crate::teslamate_projection::TeslaMateOpenSession,
    updated_at_ms: i64,
) -> OpenSessionSeedReport {
    store
        .seed_imported_open_session(
            source.source_id,
            vehicle.vehicle_id,
            1,
            session,
            updated_at_ms,
        )
        .expect("seed")
}

#[test]
fn t69_a_a_repeated_snapshot_with_an_absent_charge_maximum_is_a_no_op() {
    let (_directory, store, source, vehicle) = t69_store("t69-a");
    let v = vehicle.vehicle_id;
    let r0 = t69_dirty_revision(&store, v);
    let processes = (Some(50), Some(T69_T0 - 10 * T69_MIN));
    let report = t69_seed(&store, &source, &vehicle, &t69_s(processes), T69_T0);
    assert!(!report.no_op);
    assert_eq!(bound_of_vehicle(&store, v), 50);
    assert_eq!(t69_dirty_revision(&store, v), r0 + 1);

    // S': a new sample row and a new `charges` row, an absent `charging_processes`.
    let prime = t69_s_prime((None, None));
    let report = t69_seed(&store, &source, &vehicle, &prime, T69_T0 + T69_MIN);
    assert!(!report.no_op);
    assert_eq!(bound_of_vehicle(&store, v), 50);
    assert_eq!(t69_charge_watermark(&store, v), (Some(50), Some(T69_T0 - 10 * T69_MIN)));
    assert_eq!(t69_dirty_revision(&store, v), r0 + 2);
    let after_second = t69_rows(&store, v);

    // The repeat changes nothing.
    let report = t69_seed(&store, &source, &vehicle, &prime, T69_T0 + T69_MIN);
    assert!(report.no_op, "the same snapshot again is a no-op");
    assert_eq!(t69_dirty_revision(&store, v), r0 + 2);
    assert_eq!(t69_rows(&store, v), after_second);
    assert_eq!(bound_of_vehicle(&store, v), 50);
}

#[test]
fn t69_b_a_lower_charge_maximum_is_a_no_op_when_the_merge_is_what_is_stored() {
    let (_directory, store, source, vehicle) = t69_store("t69-b");
    let v = vehicle.vehicle_id;
    let r0 = t69_dirty_revision(&store, v);
    t69_seed(
        &store,
        &source,
        &vehicle,
        &t69_s((Some(50), Some(T69_T0 - 10 * T69_MIN))),
        T69_T0,
    );
    t69_seed(&store, &source, &vehicle, &t69_s_prime((None, None)), T69_T0 + T69_MIN);
    assert_eq!(t69_dirty_revision(&store, v), r0 + 2);
    let lower = t69_s_prime((Some(7), Some(T69_T0 - 30 * T69_MIN)));
    for _ in 0..2 {
        let report = t69_seed(&store, &source, &vehicle, &lower, T69_T0 + T69_MIN);
        assert!(report.no_op);
    }
    assert_eq!(t69_dirty_revision(&store, v), r0 + 2);
    assert_eq!(bound_of_vehicle(&store, v), 50);
}

#[test]
fn t69_c_a_higher_charge_maximum_is_written_once_and_a_no_op_when_repeated() {
    let (_directory, store, source, vehicle) = t69_store("t69-c");
    let v = vehicle.vehicle_id;
    let r0 = t69_dirty_revision(&store, v);
    t69_seed(
        &store,
        &source,
        &vehicle,
        &t69_s((Some(50), Some(T69_T0 - 10 * T69_MIN))),
        T69_T0,
    );
    t69_seed(&store, &source, &vehicle, &t69_s_prime((None, None)), T69_T0 + T69_MIN);
    let higher = t69_s_prime((Some(80), Some(T69_T0 - 5 * T69_MIN)));
    let report = t69_seed(&store, &source, &vehicle, &higher, T69_T0 + T69_MIN);
    assert!(!report.no_op);
    assert_eq!(bound_of_vehicle(&store, v), 80);
    assert_eq!(t69_charge_watermark(&store, v), (Some(80), Some(T69_T0 - 5 * T69_MIN)));
    assert_eq!(t69_dirty_revision(&store, v), r0 + 3);
    // The id cursors follow the raised watermark (C1 R84).
    assert!(id_cursors(&store, v)[2] >= 81, "{:?}", id_cursors(&store, v));
    let report = t69_seed(&store, &source, &vehicle, &higher, T69_T0 + T69_MIN);
    assert!(report.no_op);
    assert_eq!(t69_dirty_revision(&store, v), r0 + 3);
}

#[test]
fn t69_d_a_snapshot_whose_only_difference_is_a_lower_charge_watermark_is_a_no_op() {
    let (_directory, store, source, vehicle) = t69_store("t69-d");
    let v = vehicle.vehicle_id;
    let r0 = t69_dirty_revision(&store, v);
    t69_seed(
        &store,
        &source,
        &vehicle,
        &t69_s((Some(50), Some(T69_T0 - 10 * T69_MIN))),
        T69_T0,
    );
    let report = t69_seed(&store, &source, &vehicle, &t69_s((None, None)), T69_T0);
    assert!(report.no_op, "nothing it would store differs");
    assert_eq!(t69_dirty_revision(&store, v), r0 + 1);
    assert_eq!(bound_of_vehicle(&store, v), 50);
}

#[test]
fn t69_e_a_changed_open_row_is_still_not_a_no_op() {
    // The merge only excuses the charging_processes watermark: a snapshot that
    // differs in an open row or another watermark is written.
    let (_directory, store, source, vehicle) = t69_store("t69-e");
    let v = vehicle.vehicle_id;
    t69_seed(
        &store,
        &source,
        &vehicle,
        &t69_s((Some(50), Some(T69_T0 - 10 * T69_MIN))),
        T69_T0,
    );
    let mut changed_sample = t69_s((None, None));
    changed_sample.charge_samples[0].charge_energy_added_kwh = Some(1.50);
    assert!(!t69_seed(&store, &source, &vehicle, &changed_sample, T69_T0).no_op);
    let mut changed_watermark = t69_s((None, None));
    changed_watermark.watermarks.states.max_id = Some(9);
    assert!(!t69_seed(&store, &source, &vehicle, &changed_watermark, T69_T0).no_op);
    assert_eq!(bound_of_vehicle(&store, v), 50);
}
