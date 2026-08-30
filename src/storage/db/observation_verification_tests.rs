// SPDX-License-Identifier: AGPL-3.0-only

use rusqlite::params;

use super::*;

fn map_car(store: &HubStore, vehicle_id: Uuid, car_id: i64) {
    store
        .open()
        .expect("open mapping database")
        .execute(
            "INSERT INTO materialised_cars(vehicle_id, car_id, car_json)
                 VALUES (?1, ?2, ?3)",
            params![vehicle_id.to_string(), car_id, "{}"],
        )
        .expect("map source car");
}

#[test]
fn watermark_and_verification_use_only_durable_observation_metadata() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (source, vehicle) = {
        let source = store
            .register_source(&SourceDescriptor::new("teslamate", "cutover-test"), 1_000)
            .expect("source");
        let vehicle = store
            .register_vehicle(&VehicleDescriptor::new(source.source_id, "vin:test"), 1_001)
            .expect("vehicle");
        (source, vehicle)
    };
    map_car(&store, vehicle.vehicle_id, 17);

    let first = store
        .append_observation(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: 2_000,
                payload: serde_json::json!({
                    "record_type": "owner_api_vehicle_data_v1",
                    "secret_like": "payload must not be read"
                }),
            },
            2_001,
        )
        .expect("first observation");
    let mut connection = store.open().expect("prune connection");
    let transaction = connection.transaction().expect("prune transaction");
    prune_processed_observations(
        &transaction,
        vehicle.vehicle_id,
        first.observation.observation_id,
    )
    .expect("prune first raw observation");
    transaction.commit().expect("commit first prune");

    let read_only = HubStore::open_read_only(temporary.path()).expect("read-only store");
    let watermark = read_only.observation_watermark(17).expect("watermark");
    assert_eq!(watermark.observation_id, 1);
    assert_eq!(watermark.observed_at_ms, Some(2_000));
    assert_eq!(watermark.received_at_ms, Some(2_001));
    assert!(
        !read_only
            .verify_observation_after(17, watermark.observation_id)
            .expect("pre-cutover verification")
            .verified()
    );

    let second = store
        .append_observation(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: 3_000,
                payload: serde_json::json!({
                    "record_type": "owner_api_vehicle_data_v1",
                    "next": true
                }),
            },
            3_001,
        )
        .expect("new observation");
    let mut connection = store.open().expect("second prune connection");
    let transaction = connection.transaction().expect("second prune transaction");
    prune_processed_observations(
        &transaction,
        vehicle.vehicle_id,
        second.observation.observation_id,
    )
    .expect("prune second raw observation");
    transaction.commit().expect("commit second prune");
    let verification = read_only
        .verify_observation_after(17, watermark.observation_id)
        .expect("verification");
    assert!(verification.verified());
    assert_eq!(verification.latest_observation_id, Some(2));
    assert_eq!(verification.latest_observed_at_ms, Some(3_000));
    assert_eq!(verification.latest_received_at_ms, Some(3_001));
}

#[test]
fn source_car_mapping_fails_closed_when_missing_or_ambiguous() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("tesla_owner_api", "mapping-test"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-test"),
            1_001,
        )
        .expect("vehicle");
    assert!(matches!(
        store.observation_watermark(17),
        Err(ObservationVerificationError::NoVehicleMapping)
    ));

    map_car(&store, vehicle.vehicle_id, 17);
    let other_source = store
        .register_source(&SourceDescriptor::new("teslamate", "other"), 2_000)
        .expect("other source");
    let other_vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(other_source.source_id, "vin:other"),
            2_001,
        )
        .expect("other vehicle");
    map_car(&store, other_vehicle.vehicle_id, 17);

    assert!(matches!(
        store.observation_watermark(17),
        Err(ObservationVerificationError::AmbiguousVehicleMapping)
    ));
    let first = store
        .observation_watermark_for_vehicle(vehicle.vehicle_id, 17)
        .expect("exact first vehicle watermark");
    let second = store
        .observation_watermark_for_vehicle(other_vehicle.vehicle_id, 17)
        .expect("exact second vehicle watermark");
    assert_eq!(first.vehicle_id, vehicle.vehicle_id);
    assert_eq!(first.source_id, source.source_id);
    assert_eq!(second.vehicle_id, other_vehicle.vehicle_id);
    assert_eq!(second.source_id, other_source.source_id);
}
