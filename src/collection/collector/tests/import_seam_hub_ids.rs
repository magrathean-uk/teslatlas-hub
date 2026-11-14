// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T12, collector side: a lifecycle state seeded before the Hub's id range
//! (restored or migrated, its sessions closed) is seeded again from the
//! store's Hub-range maxima and `import_seam_links` before it allocates.
//! Synthetic data only.

use super::*;
use crate::db::hub_ids::HUB_ID_BASE;

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

#[test]
fn import_seam_restored_lifecycle_cursors_below_the_range_are_raised() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new(crate::db::IMPORT_SOURCE_KIND, "seam-collector"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "seam-car"), 1_000)
        .expect("vehicle");
    let v = vehicle.vehicle_id;
    let connection = store.open().expect("connection");
    // Imported rows keep source ids; the Hub holds drive base + 4 and charge
    // base, and the seam superseded its state base + 6.
    for (sql, id) in [
        (
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, ?2, 1, '{}')",
            41_000,
        ),
        (
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, ?2, 1, '{}')",
            HUB_ID_BASE + 4,
        ),
        (
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, ?2, 1, '{}')",
            HUB_ID_BASE,
        ),
        (
            "INSERT INTO import_seam_links(vehicle_id, entity, loser_id, winner_id, decided_seq)
             VALUES (?1, 'state', ?2, 5840, 1)",
            HUB_ID_BASE + 6,
        ),
    ] {
        connection
            .execute(sql, rusqlite::params![v.to_string(), id])
            .expect("fixture row");
    }
    drop(connection);

    // A restored state from before the range: seeded, sessions closed, every
    // cursor below the base.
    let mut restored = OpenSessionState::new();
    restored.id_cursors_seeded = true;
    restored.next_drive_id = 41_001;
    restored.next_position_id = 900_001;
    restored.next_charge_id = 843;
    restored.next_charge_sample_id = 335_317;
    restored.next_state_id = 5_841;
    restored.next_update_id = 49;
    assert!(!restored.id_cursors_in_hub_range());
    seed_lifecycle_ids_from_materialised(&store, v, &mut restored).expect("seed");
    assert!(restored.id_cursors_seeded);
    assert_eq!(
        seam_cursors(&restored),
        [
            HUB_ID_BASE + 5,
            HUB_ID_BASE,
            HUB_ID_BASE + 1,
            HUB_ID_BASE,
            HUB_ID_BASE + 7,
            HUB_ID_BASE,
        ]
    );

    // A fresh state seeds the same way; one already in the range is kept.
    let mut fresh = OpenSessionState::new();
    seed_lifecycle_ids_from_materialised(&store, v, &mut fresh).expect("fresh seed");
    assert_eq!(seam_cursors(&fresh), seam_cursors(&restored));
    let mut ahead = restored.clone();
    ahead.next_drive_id = HUB_ID_BASE + 50;
    let before = ahead.clone();
    seed_lifecycle_ids_from_materialised(&store, v, &mut ahead).expect("kept");
    assert_eq!(ahead, before, "a state in the range is not seeded again");
}

/// The Hub's own collection is never refused by the seam guard (CHK-M03-SEAM
/// group C): a vehicle that has collected rows in the Hub range, under
/// coverage in the store and with no import base, publishes its first full
/// snapshot, while an unplanned import publication on it still refuses.
#[test]
fn import_seam_the_collectors_own_publication_is_not_refused() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let t0 = 1_800_000_500_000_i64;
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
    let drive_state = |shift: &str, speed: i64, at_ms: i64| {
        json!({
            "drive_state": {
                "shift_state": shift,
                "speed": speed,
                "latitude": 47.0,
                "longitude": 19.0,
                "timestamp": at_ms
            },
            "vehicle_state": {"odometer": 1000.0 + (at_ms - t0) as f64 / 60_000.0},
            "charge_state": {"battery_level": 70, "battery_range": 200.0}
        })
    };
    let collect = |snapshots: Vec<VehicleData>, at_ms: i64| {
        let collection = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots,
            failures: vec![],
        };
        persist_collection(&store, &collection, at_ms).expect("persist");
        materialise_lifecycle_for_collection(&store, &collection, at_ms).expect("materialise");
        collection
    };
    collect(
        vec![
            VehicleData::for_test(9, drive_state("D", 25, t0)),
            VehicleData::for_test(9, drive_state("D", 30, t0 + 60_000)),
        ],
        t0,
    );
    let last = collect(
        vec![VehicleData::for_test(9, drive_state("P", 0, t0 + 120_000))],
        t0 + 120_000,
    );
    let vehicle_id = store
        .open()
        .expect("db")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("id")
        .parse::<Uuid>()
        .expect("uuid");
    let history = store.materialised_history(vehicle_id).expect("history");
    assert_eq!(history.drives.len(), 1);
    assert!(crate::db::hub_ids::is_hub_id(history.drives[0].id));
    store
        .open()
        .expect("db")
        .execute(
            "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
             VALUES (1, 'grant-1', ?1, NULL, '[]', 'open')",
            rusqlite::params![t0 - 60_000],
        )
        .expect("coverage");
    assert!(
        matches!(
            store.refuse_unplanned_import_seam(vehicle_id),
            Err(crate::db::StoreError::ImportGenerationConflict)
        ),
        "an unplanned import publication still refuses"
    );

    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let published = publish_compatibility_snapshots(
        &store,
        &publication_gate,
        &CursorKey::from_bytes([9; 32]),
        &last,
        t0 + 120_000,
    )
    .expect("the Hub's own collection publishes");
    assert_eq!(published, 1);
    let manifest = store
        .manifest_for_vehicle(vehicle_id)
        .expect("manifest")
        .expect("published");
    assert!(manifest.total_rows > 1);
}
