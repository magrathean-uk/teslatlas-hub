// SPDX-License-Identifier: AGPL-3.0-only

// Charge lifecycle through the store (C1 part C): restart mid-charge and the
// materialisation of an after-gap charge opened and closed in one step (ST67),
// and the stored label of a closed charge (ST64, R81). Synthetic data only;
// the sequences come from the lifecycle charge tests.

fn open_charge_sample_rows(store: &HubStore, vehicle: &VehicleRecord) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows
             WHERE vehicle_id = ?1 AND domain = 'charge_sample'",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("open charge sample rows")
}

fn charge_sequence() -> Vec<crate::lifecycle::LifecycleSample> {
    use crate::lifecycle::charge_tests::{MIN, T0, cv};
    vec![
        cv(1, T0, "Charging", Some(1.0), Some(50), Some(1000.0)),
        cv(2, T0 + 2 * MIN, "Charging", Some(2.0), Some(55), Some(1000.0)),
        cv(3, T0 + 4 * MIN, "Charging", Some(3.0), Some(60), Some(1000.0)),
        cv(4, T0 + 6 * MIN, "Complete", Some(4.0), Some(65), Some(1000.0)),
    ]
}

#[test]
fn st67_a_restart_mid_charge_gives_the_rows_of_an_uninterrupted_store() {
    let samples = charge_sequence();

    let uninterrupted_dir = crate::private_tempdir().expect("tempdir");
    let uninterrupted = HubStore::initialize(uninterrupted_dir.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&uninterrupted);
    feed_owner_samples(&uninterrupted, &source, &vehicle, &samples);
    let expected = uninterrupted
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(expected.charges.len(), 1);
    assert_eq!(expected.charge_samples.len(), 4);
    assert_eq!(expected.charges[0].charge_energy_added, Some(4.0));

    let restarted_dir = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(restarted_dir.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    feed_owner_samples(&store, &source, &vehicle, &samples[..2]);
    let record = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .expect("load")
        .expect("state");
    let state =
        crate::lifecycle::OpenSessionState::decode(&record.open_session_json).expect("decode");
    let open = state.open_charge.expect("a charge is open");
    // The children live in the open rows, not in the state blob.
    assert!(open.samples.is_empty());
    assert_eq!(open.sample_count, 2);
    assert_eq!(
        store
            .open_charge_samples(vehicle.vehicle_id, open.id)
            .expect("children")
            .len(),
        2
    );
    assert_eq!(open_charge_sample_rows(&store, &vehicle), 2);
    drop(store);

    let store = HubStore::initialize(restarted_dir.path()).expect("restart");
    feed_owner_samples(&store, &source, &vehicle, &samples[2..]);
    let actual = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(actual.charges, expected.charges);
    assert_eq!(actual.charge_samples, expected.charge_samples);
    // The open rows were reloaded at the close and no child is left behind.
    assert_eq!(open_charge_sample_rows(&store, &vehicle), 0);
}

#[test]
fn st67_an_after_gap_charge_opened_and_closed_in_one_step_materialises_its_sample() {
    use crate::lifecycle::charge_tests::{HOUR, T0, at, cv, parked_after_drive};
    let mut samples = parked_after_drive();
    samples.push(at(
        cv(3, T0 + 9 * HOUR, "Complete", Some(30.0), Some(85), Some(1000.0)),
        10.01,
        20.0,
    ));
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    feed_owner_samples(&store, &source, &vehicle, &samples);
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(history.charges.len(), 1);
    let charge = &history.charges[0];
    assert_eq!((charge.start_date_ms, charge.end_date_ms), (T0, Some(T0 + 9 * HOUR)));
    assert_eq!(history.charge_samples.len(), 1);
    assert_eq!(history.charge_samples[0].charge_process_id, charge.id);
    assert_eq!(history.charge_samples[0].timestamp_ms, T0 + 9 * HOUR);
    assert_eq!(open_charge_sample_rows(&store, &vehicle), 0);
}

#[test]
fn st64_a_closed_charge_is_labelled_from_its_start_coordinates() {
    use crate::lifecycle::charge_tests::{HOUR, MIN, T0, at, cv};
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    let fence = |name: &str, radius: f64| crate::teslamate_projection::TeslaMateGeofence {
        id: 0,
        name: name.to_owned(),
        latitude: Some(10.0),
        longitude: Some(20.0),
        radius_m: Some(radius),
        billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
        cost_per_unit: Some(0.25),
        session_fee: None,
    };
    store
        .save_geofence(vehicle.vehicle_id, None, fence("Home", 150.0))
        .expect("fence");
    // One charge inside the fence, one without start coordinates.
    let samples = [
        at(cv(1, T0, "Charging", Some(1.0), Some(50), None), 10.0005, 20.0),
        cv(2, T0 + 2 * MIN, "Complete", Some(3.0), Some(55), None),
        cv(3, T0 + HOUR, "Charging", Some(1.0), Some(50), None),
        cv(4, T0 + HOUR + 2 * MIN, "Complete", Some(3.0), Some(55), None),
    ];
    feed_owner_samples(&store, &source, &vehicle, &samples);
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(history.charges.len(), 2);
    assert_eq!(history.charges[0].geofence.as_deref(), Some("Home"));
    assert_eq!(history.charges[1].geofence, None);
}
