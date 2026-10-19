// SPDX-License-Identifier: AGPL-3.0-only

// C1 B2 amendment B (R88), spec test ST103: the collector's count of the
// children of a batch's closed sessions reads stored ids a page at a time.
// Synthetic data only.

#[test]
fn st103_the_collector_count_reads_stored_ids_in_pages() {
    use crate::db::{ClosePageStats, LifecycleCommit};
    use crate::lifecycle::{LifecycleDelta, OpenSessionState};
    const T0: i64 = 1_760_000_000_000;
    let temp = crate::private_tempdir().expect("tempdir");
    let store = crate::db::HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("tesla_owner_api", "account-test"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "vehicle-test"), 1_001)
        .expect("vehicle");
    let position = |id: i64| -> crate::hub_pack::ProjectionPosition {
        serde_json::from_value(json!({
            "id": id, "drive_id": 1, "car_id": 1, "date_ms": T0 + 10_000 * id,
            "latitude": 10.0, "longitude": 20.0
        }))
        .expect("position")
    };
    let sample = |id: i64| -> crate::hub_pack::ProjectionChargeSample {
        serde_json::from_value(json!({
            "id": id, "charge_process_id": 7, "timestamp_ms": T0 + 60_000 * id
        }))
        .expect("sample")
    };
    let state = OpenSessionState::new().encode().expect("state");
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 1_000,
            delta: &LifecycleDelta {
                open_drive_positions: (101..=113).map(position).collect(),
                open_charge_samples: (201..=213).map(sample).collect(),
                ..Default::default()
            },
        })
        .expect("commit");
    let drive: crate::hub_pack::ProjectionDrive = serde_json::from_value(json!({
        "id": 1, "car_id": 1, "start_date_ms": T0, "end_date_ms": T0 + 1_000_000
    }))
    .expect("drive");
    let charge: crate::hub_pack::ProjectionCharge = serde_json::from_value(json!({
        "id": 7, "car_id": 1, "start_date_ms": T0, "end_date_ms": T0 + 1_000_000
    }))
    .expect("charge");
    // Drive 1's in-batch ids are {107, 120}: 107 is also stored.
    let mut drive_delta = LifecycleDelta {
        drives: vec![drive],
        positions: vec![position(107)],
        open_drive_positions: vec![position(120)],
        ..Default::default()
    };
    let charge_delta = LifecycleDelta {
        charges: vec![charge],
        ..Default::default()
    };
    for page_rows in [4, 13] {
        let (positions, samples, stats) =
            count_closed_children(&store, vehicle.vehicle_id, &drive_delta, page_rows)
                .expect("drive count");
        assert_eq!((positions, samples), (14, 0), "page size {page_rows}");
        let expected = if page_rows == 4 {
            ClosePageStats { pages: 4, largest_page: 4 }
        } else {
            ClosePageStats { pages: 1, largest_page: 13 }
        };
        assert_eq!(stats, expected, "page size {page_rows}");
        let (positions, samples, stats) =
            count_closed_children(&store, vehicle.vehicle_id, &charge_delta, page_rows)
                .expect("charge count");
        assert_eq!((positions, samples), (0, 13), "page size {page_rows}");
        assert_eq!(stats, expected, "page size {page_rows}");
    }
    // Both together.
    drive_delta.charges = charge_delta.charges.clone();
    let (positions, samples, stats) =
        count_closed_children(&store, vehicle.vehicle_id, &drive_delta, 4).expect("both");
    assert_eq!((positions, samples), (14, 13));
    assert_eq!(stats, ClosePageStats { pages: 8, largest_page: 4 });
}
