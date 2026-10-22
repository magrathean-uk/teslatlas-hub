// SPDX-License-Identifier: AGPL-3.0-only

//! C8 Part S-b tests (C8 section 8: C8 T01 to C8 T03, C8 T10, C8 T15, C8 T41 to
//! C8 T45 and C8 T48; C8 R2, C8 R3, C8 R9, C8 R10, C8 R16a, C8 R17, C8 R28, C8
//! R47, C8 R51): the save and delete flow with its ids, tombstones and
//! replay rules, and the one reconciliation of both import paths, with the
//! relabel of an import (C8 R16) in C8 T15, C8 T42 and C8 T48. Synthetic data
//! only.

use super::b3_c8_sa::{
    Fence, c8_store, c8_vehicle, count, fence, import_in_transaction, imported, rejection,
    source_owned_of,
};
use super::*;
use crate::geo::geofence_validation::{GeofenceInactiveReason, GeofenceRejection};
use crate::geo::location::{DEFAULT_GEOFENCE_RADIUS_METRES, FIRST_HUB_GEOFENCE_ID};
use crate::hub_pack::GeofenceBillingType;

/// The first Hub fence id (the spec's `H`).
pub(super) const H: i64 = FIRST_HUB_GEOFENCE_ID;

pub(super) fn save(
    store: &HubStore,
    vehicle: Uuid,
    id: Option<i64>,
    record: Fence,
) -> Result<Fence, StoreError> {
    store.save_geofence(vehicle, id, record, false)
}

pub(super) fn clear(
    store: &HubStore,
    vehicle: Uuid,
    id: Option<i64>,
    record: Fence,
) -> Result<Fence, StoreError> {
    store.save_geofence(vehicle, id, record, true)
}

pub(super) fn imp(store: &HubStore, vehicle: Uuid, rows: &[Fence]) -> GeofenceUpsertReport {
    store.upsert_geofences(vehicle, rows).expect("import")
}

/// Relabel pages read on this thread so far (C8 R53, C8 T47): a fence change
/// that alters no presence, name, centre or radius reads none.
pub(super) fn relabel_page_reads() -> u64 {
    RELABEL_PAGE_READS.with(std::cell::Cell::get)
}

/// A radius of `radius_m` on a fence record.
pub(super) fn with_radius(record: Fence, radius_m: f64) -> Fence {
    Fence {
        radius_m: Some(radius_m),
        ..record
    }
}

/// A price on a fence record, with the billing type the save resolves.
pub(super) fn with_cost(record: Fence, cost_per_unit: f64) -> Fence {
    Fence {
        cost_per_unit: Some(cost_per_unit),
        ..record
    }
}

/// A source row with a stored tariff.
pub(super) fn imported_priced(
    id: i64,
    name: &str,
    latitude: f64,
    longitude: f64,
    radius_m: f64,
    billing_type: GeofenceBillingType,
    cost_per_unit: Option<f64>,
    session_fee: Option<f64>,
) -> Fence {
    Fence {
        billing_type: Some(billing_type),
        cost_per_unit,
        session_fee,
        ..imported(id, name, latitude, longitude, radius_m)
    }
}

/// The report of an import that touched nothing it lists, for equality checks:
/// only the fields a test names are set by the caller.
pub(super) fn report_with(source_rows: u64) -> GeofenceUpsertReport {
    GeofenceUpsertReport {
        source_rows,
        ..GeofenceUpsertReport::default()
    }
}

/// The vehicle's state a fence call may change, as SQLite `quote()` text with
/// no vehicle id, so two vehicles of equal history compare equal (C8 R44).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Durable {
    pub(super) fences: Vec<Vec<String>>,
    pub(super) marks: Vec<Vec<String>>,
    pub(super) tombstones: Vec<Vec<String>>,
    pub(super) drives: Vec<Vec<String>>,
    pub(super) charges: Vec<Vec<String>>,
    pub(super) mutations: Vec<Vec<String>>,
    pub(super) export: Vec<Vec<String>>,
}

fn quoted(connection: &Connection, sql: &str, vehicle: Uuid, columns: usize) -> Vec<Vec<String>> {
    let mut statement = connection.prepare(sql).expect("prepare");
    statement
        .query_map(params![vehicle.to_string()], |row| {
            (0..columns).map(|index| row.get(index)).collect()
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// Every table a fence call may write, for one vehicle.
pub(super) fn durable(store: &HubStore, vehicle: Uuid) -> Durable {
    let connection = store.open().expect("connection");
    Durable {
        fences: quoted(
            &connection,
            "SELECT quote(source_geofence_id), quote(name), quote(latitude), quote(longitude),
                    quote(radius_m), quote(billing_type), quote(cost_per_unit),
                    quote(session_fee), quote(source_owned)
               FROM geofences WHERE vehicle_id = ?1 ORDER BY source_geofence_id",
            vehicle,
            9,
        ),
        marks: quoted(
            &connection,
            "SELECT quote(last_hub_id) FROM geofence_id_marks WHERE vehicle_id = ?1",
            vehicle,
            1,
        ),
        tombstones: quoted(
            &connection,
            "SELECT quote(source_geofence_id) FROM deleted_imported_geofences
              WHERE vehicle_id = ?1 ORDER BY source_geofence_id",
            vehicle,
            1,
        ),
        drives: quoted(
            &connection,
            "SELECT quote(drive_id), quote(drive_json) FROM materialised_drives
              WHERE vehicle_id = ?1 ORDER BY drive_id",
            vehicle,
            2,
        ),
        charges: quoted(
            &connection,
            "SELECT quote(charge_id), quote(charge_json) FROM materialised_charges
              WHERE vehicle_id = ?1 ORDER BY charge_id",
            vehicle,
            2,
        ),
        mutations: quoted(
            &connection,
            "SELECT quote(revision), quote(entity), quote(entity_id), quote(operation),
                    quote(payload_json)
               FROM sync_mutations WHERE vehicle_id = ?1 ORDER BY revision",
            vehicle,
            5,
        ),
        export: quoted(
            &connection,
            "SELECT quote(dirty_revision) FROM export_outbox WHERE vehicle_id = ?1",
            vehicle,
            1,
        ),
    }
}

/// `sync_mutations` rows of the vehicle as `drive 1` or `charge 2`, in revision
/// order (the spec's `M`).
pub(super) fn mutation_log(store: &HubStore, vehicle: Uuid) -> Vec<String> {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(
            "SELECT entity || ' ' || entity_id FROM sync_mutations
              WHERE vehicle_id = ?1 ORDER BY revision",
        )
        .expect("prepare");
    statement
        .query_map(params![vehicle.to_string()], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// `export_outbox.dirty_revision` of the vehicle, 0 when it has no row: the
/// spec's `X` is whether a call raised it.
pub(super) fn export_revision(store: &HubStore, vehicle: Uuid) -> i64 {
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

/// The vehicle's fence entity mutations: none are ever recorded (C8 R32).
pub(super) fn geofence_entity_mutations(store: &HubStore, vehicle: Uuid) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND entity = 'geofence'",
            params![vehicle.to_string()],
            |row| row.get(0),
        )
        .expect("count")
}

fn id_mark(store: &HubStore, vehicle: Uuid) -> Option<i64> {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT last_hub_id FROM geofence_id_marks WHERE vehicle_id = ?1",
            params![vehicle.to_string()],
            |row| row.get(0),
        )
        .optional()
        .expect("mark")
}

/// A fence row written as stored data, with no mark and no validation.
fn seed_fence_row(store: &HubStore, vehicle: Uuid, id: i64, latitude: f64) {
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO geofences(
                vehicle_id, source_geofence_id, name, latitude, longitude, radius_m
             ) VALUES (?1, ?2, 'Seeded', ?3, 0.0, 50.0)",
            params![vehicle.to_string(), id, latitude],
        )
        .expect("seed fence");
}

/// A materialised drive with both endpoints, labelled at its end only.
pub(super) fn drive_row(
    id: i64,
    start: Option<(f64, f64)>,
    end: Option<(f64, f64)>,
    start_label: Option<&str>,
    end_label: Option<&str>,
) -> crate::hub_pack::ProjectionDrive {
    crate::hub_pack::ProjectionDrive {
        id,
        car_id: 1,
        optimized_at_ms: None,
        start_date_ms: 1_700_000_000_000 + id * 10_000,
        end_date_ms: 1_700_000_001_000 + id * 10_000,
        distance_km: Some(1.0),
        duration_min: Some(1),
        efficiency: None,
        outside_temp_avg: None,
        inside_temp_avg: None,
        speed_max: None,
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_address: None,
        end_address: None,
        start_geofence: start_label.map(str::to_owned),
        end_geofence: end_label.map(str::to_owned),
        start_latitude: start.map(|point| point.0),
        start_longitude: start.map(|point| point.1),
        end_latitude: end.map(|point| point.0),
        end_longitude: end.map(|point| point.1),
        start_soc: None,
        end_soc: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
        ascent: None,
        descent: None,
    }
}

pub(super) fn seed_drive(store: &HubStore, vehicle: Uuid, row: &crate::hub_pack::ProjectionDrive) {
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                vehicle.to_string(),
                row.id,
                row.car_id,
                serde_json::to_string(row).expect("drive json")
            ],
        )
        .expect("seed drive");
}

/// The start and end labels of one stored drive.
pub(super) fn drive_labels(
    store: &HubStore,
    vehicle: Uuid,
    id: i64,
) -> (Option<String>, Option<String>) {
    let row = store
        .materialised_drive_for_vehicle(vehicle, id)
        .expect("drive read")
        .expect("drive row");
    (row.start_geofence, row.end_geofence)
}

/// The end label of one stored drive.
pub(super) fn end_label(store: &HubStore, vehicle: Uuid, id: i64) -> Option<String> {
    drive_labels(store, vehicle, id).1
}

/// The stored fence row for `id` with `source_owned` replaced by `owned`.
fn with_owned(mut rows: Vec<Vec<String>>, id: i64, owned: &str) -> Vec<Vec<String>> {
    let key = id.to_string();
    for row in &mut rows {
        if row[0] == key {
            *row.last_mut().expect("column") = owned.to_owned();
        }
    }
    rows
}

#[test]
fn c8_t01_defaults() {
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t01");
    let saved = save(&store, vehicle, None, fence("Home", 10.0, 20.0)).expect("create");
    // C8 R7 and C8 R3: the default radius, no tariff, the first Hub id.
    assert_eq!(
        saved,
        Fence {
            id: H,
            name: "Home".to_owned(),
            latitude: Some(10.0),
            longitude: Some(20.0),
            radius_m: Some(50.0),
            billing_type: None,
            cost_per_unit: None,
            session_fee: None,
        }
    );
    assert_eq!(saved.radius_m, Some(DEFAULT_GEOFENCE_RADIUS_METRES));
    assert_eq!(store.geofences(vehicle).expect("fences"), [saved]);
    let state = durable(&store, vehicle);
    // One row, written by the Hub (`source_owned` 0), with the id mark at H.
    assert_eq!(
        state.fences,
        [[
            "2147483648",
            "'Home'",
            "10.0",
            "20.0",
            "50.0",
            "NULL",
            "NULL",
            "NULL",
            "0"
        ]]
    );
    assert_eq!(state.marks, [["2147483648"]]);
    assert!(state.tombstones.is_empty());
    // M = 0: no fence entity mutation (C8 R32); X = yes: the export is marked
    // once (C8 R28).
    assert!(mutation_log(&store, vehicle).is_empty());
    assert_eq!(export_revision(&store, vehicle), 1);
}

#[test]
fn c8_t02_id_allocation() {
    let (_temporary, store) = c8_store();
    // Imported ids never move the Hub's first id (C8 R3).
    let vehicle = c8_vehicle(&store, "t02");
    imp(
        &store,
        vehicle,
        &[
            imported(1, "P", 50.0, 0.0, 100.0),
            imported(7, "Q", 51.0, 0.0, 100.0),
        ],
    );
    let first = save(&store, vehicle, None, fence("A", 10.0, 20.0)).expect("A");
    assert_eq!(first.id, H);
    let second = save(&store, vehicle, None, fence("B", 11.0, 20.0)).expect("B");
    assert_eq!(second.id, H + 1);
    store.delete_geofence(vehicle, H + 1).expect("delete B");
    // A deleted Hub id is never given again (the mark never decreases), and a
    // Hub id needs no tombstone.
    let third = save(&store, vehicle, None, fence("C", 12.0, 20.0)).expect("C");
    assert_eq!(third.id, H + 2);
    assert_eq!(id_mark(&store, vehicle), Some(H + 2));
    assert!(durable(&store, vehicle).tombstones.is_empty());
    assert_eq!(
        store
            .geofences(vehicle)
            .expect("fences")
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        [1, 7, H, H + 2]
    );

    // A vehicle whose only fence has id H+5 and no mark: the next id is H+6.
    let by_largest = c8_vehicle(&store, "t02-largest");
    seed_fence_row(&store, by_largest, H + 5, 5.0);
    assert_eq!(id_mark(&store, by_largest), None);
    let created = save(&store, by_largest, None, fence("Next", 10.0, 20.0)).expect("next");
    assert_eq!(created.id, H + 6);
    assert_eq!(id_mark(&store, by_largest), Some(H + 6));

    // A mark above the largest stored id wins.
    let by_mark = c8_vehicle(&store, "t02-mark");
    seed_fence_row(&store, by_mark, H + 5, 5.0);
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO geofence_id_marks(vehicle_id, last_hub_id) VALUES (?1, ?2)",
            params![by_mark.to_string(), H + 9],
        )
        .expect("seed mark");
    let created = save(&store, by_mark, None, fence("Next", 10.0, 20.0)).expect("next");
    assert_eq!(created.id, H + 10);
    assert_eq!(id_mark(&store, by_mark), Some(H + 10));

    // No id is left after i64::MAX, whether the largest stored id or the mark
    // is at it; nothing is written.
    let by_exhausted_id = c8_vehicle(&store, "t02-exhausted-id");
    seed_fence_row(&store, by_exhausted_id, i64::MAX, 5.0);
    let before = durable(&store, by_exhausted_id);
    assert_eq!(
        rejection(save(
            &store,
            by_exhausted_id,
            None,
            fence("Last", 10.0, 20.0)
        )),
        GeofenceRejection::IdsExhausted
    );
    assert_eq!(durable(&store, by_exhausted_id), before);
    let by_exhausted_mark = c8_vehicle(&store, "t02-exhausted-mark");
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO geofence_id_marks(vehicle_id, last_hub_id) VALUES (?1, ?2)",
            params![by_exhausted_mark.to_string(), i64::MAX],
        )
        .expect("seed mark");
    let before = durable(&store, by_exhausted_mark);
    assert_eq!(
        rejection(save(
            &store,
            by_exhausted_mark,
            None,
            fence("Last", 10.0, 20.0)
        )),
        GeofenceRejection::IdsExhausted
    );
    assert_eq!(durable(&store, by_exhausted_mark), before);
}

#[test]
fn c8_t03_unknown_id() {
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t03");
    imp(&store, vehicle, &[imported(5, "Present", 12.0, 20.0, 50.0)]);
    save(&store, vehicle, None, fence("Hub", 11.0, 20.0)).expect("a Hub fence");
    let other = c8_vehicle(&store, "t03-other");
    imp(
        &store,
        other,
        &[imported(99, "Elsewhere", 13.0, 20.0, 50.0)],
    );

    let before = durable(&store, vehicle);
    let before_other = durable(&store, other);
    // An id the vehicle does not hold is refused, validation does not run
    // first, and the other vehicle's fence with that id does not count.
    for record in [
        fence("X", 10.0, 20.0),
        Fence {
            name: String::new(),
            latitude: None,
            ..fence("", 10.0, 20.0)
        },
    ] {
        for clear_tariff in [false, true] {
            assert!(
                matches!(
                    vehicle_save(&store, vehicle, Some(99), record.clone(), clear_tariff),
                    Err(StoreError::UnknownGeofence(99))
                ),
                "unknown edit"
            );
        }
    }
    assert!(matches!(
        store.delete_geofence(vehicle, 99),
        Err(StoreError::UnknownGeofence(99))
    ));
    assert!(matches!(
        store.delete_geofence(vehicle, H + 1),
        Err(StoreError::UnknownGeofence(unknown)) if unknown == H + 1
    ));
    // An id that is not positive holds no row either: the same refusal (C8 R2,
    // C8 R17), validation not run first.
    for id in [0, -1] {
        assert!(matches!(
            save(&store, vehicle, Some(id), fence("X", 10.0, 20.0)),
            Err(StoreError::UnknownGeofence(unknown)) if unknown == id
        ));
        assert!(matches!(
            vehicle_save(
                &store,
                vehicle,
                Some(id),
                Fence {
                    name: String::new(),
                    ..fence("", 10.0, 20.0)
                },
                true
            ),
            Err(StoreError::UnknownGeofence(unknown)) if unknown == id
        ));
        assert!(matches!(
            store.delete_geofence(vehicle, id),
            Err(StoreError::UnknownGeofence(unknown)) if unknown == id
        ));
    }
    // Tables unchanged; M = 0; X = no.
    assert_eq!(durable(&store, vehicle), before);
    assert_eq!(durable(&store, other), before_other);
}

fn vehicle_save(
    store: &HubStore,
    vehicle: Uuid,
    id: Option<i64>,
    record: Fence,
    clear_tariff: bool,
) -> Result<Fence, StoreError> {
    store.save_geofence(vehicle, id, record, clear_tariff)
}

#[test]
fn c8_t06_t09_store_edits_keep_stored_values() {
    // The S/L checkpoint precondition (C8 R7, C8 R8, C8 table 5.2): an edit
    // that omits the radius, billing type, cost or fee keeps the stored values,
    // and a save that changes nothing writes nothing.
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t09");
    // C8 T06 re-save: the stored values saved again are a no-op.
    let rounded = save(
        &store,
        vehicle,
        None,
        fence("Rounded", 51.50735123, -0.12775849),
    )
    .expect("create");
    assert_eq!(
        (rounded.latitude, rounded.longitude),
        (Some(51.507351), Some(-0.127758))
    );
    let before = durable(&store, vehicle);
    assert_eq!(
        save(&store, vehicle, Some(rounded.id), rounded.clone()).expect("re-save"),
        rounded
    );
    assert_eq!(
        durable(&store, vehicle),
        before,
        "no-op re-save writes nothing"
    );

    // C8 T09.
    imp(
        &store,
        vehicle,
        &[imported_priced(
            7,
            "Gate",
            10.0,
            20.0,
            5.0,
            GeofenceBillingType::PerMinute,
            Some(0.5),
            Some(1.0),
        )],
    );
    let gate = |state: &Durable| {
        state
            .fences
            .iter()
            .find(|row| row[0] == "7")
            .expect("row 7")
            .clone()
    };
    assert_eq!(
        gate(&durable(&store, vehicle))[8],
        "1",
        "imported: source-owned"
    );
    let edited = save(
        &store,
        vehicle,
        Some(7),
        with_cost(fence("Gate A", 10.0, 20.0), 0.6),
    )
    .expect("rename and re-price");
    assert_eq!(
        (
            edited.radius_m,
            edited.billing_type,
            edited.cost_per_unit,
            edited.session_fee
        ),
        (
            Some(5.0),
            Some(GeofenceBillingType::PerMinute),
            Some(0.6),
            Some(1.0)
        ),
        "the omitted radius, type and fee are kept"
    );
    assert_eq!(
        source_owned_of(&store.open().expect("connection"), vehicle, 7),
        0,
        "a Hub write takes the row from the import"
    );
    // The same name again: nothing to write (M = 0, X = no).
    let state = durable(&store, vehicle);
    let revision = export_revision(&store, vehicle);
    let kept = save(&store, vehicle, Some(7), fence("Gate A", 10.0, 20.0)).expect("no-op");
    assert_eq!(kept, edited);
    assert_eq!(durable(&store, vehicle), state);
    assert_eq!(export_revision(&store, vehicle), revision);
    // The clear-tariff path (C8 table 5.2): the stored tariff is the base only
    // without it.
    let cleared = clear(
        &store,
        vehicle,
        Some(7),
        with_cost(fence("Gate A", 10.0, 20.0), 0.7),
    )
    .expect("clear and re-price");
    assert_eq!(
        (
            cleared.radius_m,
            cleared.billing_type,
            cleared.cost_per_unit,
            cleared.session_fee
        ),
        (
            Some(5.0),
            Some(GeofenceBillingType::PerKwh),
            Some(0.7),
            None
        )
    );
    let emptied = clear(&store, vehicle, Some(7), fence("Gate A", 10.0, 20.0)).expect("clear");
    assert_eq!(
        (
            emptied.radius_m,
            emptied.billing_type,
            emptied.cost_per_unit,
            emptied.session_fee
        ),
        (Some(5.0), None, None, None)
    );
    // A radius given is checked even on an imported fence.
    assert_eq!(
        rejection(save(
            &store,
            vehicle,
            Some(7),
            with_radius(fence("Gate A", 10.0, 20.0), 5.0)
        )),
        GeofenceRejection::RadiusBelowMinimum
    );

    // A billing type without a cost (C8 F9): an imported fence of a type only.
    imp(
        &store,
        vehicle,
        &[Fence {
            billing_type: Some(GeofenceBillingType::PerKwh),
            ..imported(8, "Depot", 12.0, 20.0, 50.0)
        }],
    );
    let state = durable(&store, vehicle);
    let revision = export_revision(&store, vehicle);
    for (label, record) in [
        ("omitted", fence("Depot", 12.0, 20.0)),
        (
            "stored values",
            Fence {
                radius_m: Some(50.0),
                billing_type: Some(GeofenceBillingType::PerKwh),
                ..fence("Depot", 12.0, 20.0)
            },
        ),
    ] {
        let kept = save(&store, vehicle, Some(8), record).expect(label);
        assert_eq!(
            kept.billing_type,
            Some(GeofenceBillingType::PerKwh),
            "{label}"
        );
        assert_eq!(durable(&store, vehicle), state, "{label}: no-op");
        assert_eq!(export_revision(&store, vehicle), revision, "{label}");
    }
    assert_eq!(
        rejection(save(
            &store,
            vehicle,
            Some(8),
            Fence {
                billing_type: Some(GeofenceBillingType::PerMinute),
                ..fence("Depot", 12.0, 20.0)
            }
        )),
        GeofenceRejection::BillingTypeWithoutCost
    );
    let priced = save(
        &store,
        vehicle,
        Some(8),
        with_cost(fence("Depot", 12.0, 20.0), 0.4),
    )
    .expect("price the depot");
    assert_eq!(priced.billing_type, Some(GeofenceBillingType::PerKwh));
    assert_eq!(priced.cost_per_unit, Some(0.4));
    assert_eq!(priced.radius_m, Some(50.0));
}

#[test]
fn c8_t10_replay_same_place() {
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t10");
    let first = save(&store, vehicle, None, fence("Home", 10.0, 20.0)).expect("create");
    assert_eq!(first.id, H);
    // A retried create returns the fence it made and writes nothing, the mark
    // included (C8 R9 item 1, C8 R43).
    let state = durable(&store, vehicle);
    let again = save(&store, vehicle, None, fence("Home", 10.0, 20.0)).expect("replay");
    assert_eq!(again, first);
    assert_eq!(store.geofences(vehicle).expect("fences").len(), 1);
    assert_eq!(durable(&store, vehicle), state);
    // The replay compares the tariff too: a priced create replays, and the same
    // place with another price is not a replay.
    let priced = save(
        &store,
        vehicle,
        None,
        with_radius(with_cost(fence("Priced", 12.0, 20.0), 0.3), 70.0),
    )
    .expect("priced");
    assert_eq!(priced.id, H + 1);
    let state = durable(&store, vehicle);
    assert_eq!(
        save(
            &store,
            vehicle,
            None,
            with_radius(with_cost(fence("Priced", 12.0, 20.0), 0.3), 70.0)
        )
        .expect("priced replay")
        .id,
        H + 1
    );
    assert_eq!(durable(&store, vehicle), state);
    assert_eq!(
        rejection(save(
            &store,
            vehicle,
            None,
            with_radius(with_cost(fence("Priced", 12.0, 20.0), 0.4), 70.0)
        )),
        GeofenceRejection::DuplicatePlace { existing_id: H + 1 }
    );
    assert_eq!(durable(&store, vehicle), state);

    // The same centre and radius under another name is the same place; another
    // radius is not (C8 R9 item 2).
    assert_eq!(
        rejection(save(&store, vehicle, None, fence("Other", 10.0, 20.0))),
        GeofenceRejection::DuplicatePlace { existing_id: H }
    );
    let sized = save(
        &store,
        vehicle,
        None,
        with_radius(fence("Other", 10.0, 20.0), 60.0),
    )
    .expect("another radius");
    assert_eq!(sized.id, H + 2);
    // An edit that moves a fence onto another's centre and radius is refused;
    // one that only renames it is not.
    assert_eq!(
        rejection(save(
            &store,
            vehicle,
            Some(H + 2),
            with_radius(fence("Other", 10.0, 20.0), 50.0)
        )),
        GeofenceRejection::DuplicatePlace { existing_id: H }
    );
    assert_eq!(
        store.geofences(vehicle).expect("fences")[2].radius_m,
        Some(60.0)
    );
    save(&store, vehicle, Some(H + 2), fence("Renamed", 10.0, 20.0)).expect("rename");

    // Longitudes 180 and -180 are the same place at the equator.
    let dateline = c8_vehicle(&store, "t10-dateline");
    let placed = save(
        &store,
        dateline,
        None,
        with_radius(fence("Dateline", 0.0, 180.0), 50.0),
    )
    .expect("dateline");
    assert_eq!(placed.id, H);
    assert_eq!(
        rejection(save(
            &store,
            dateline,
            None,
            with_radius(fence("Twin", 0.0, -180.0), 50.0)
        )),
        GeofenceRejection::DuplicatePlace { existing_id: H }
    );
    // An edit that moves the fence across the antimeridian (radius kept) is not
    // the same place as the fence itself: the edit target is excluded (C8 R9
    // item 2).
    let moved = save(
        &store,
        dateline,
        Some(H),
        with_radius(fence("Dateline", 0.0, -180.0), 50.0),
    )
    .expect("edit across the antimeridian");
    assert_eq!((moved.id, moved.longitude), (H, Some(-180.0)));
    assert_eq!(
        store.geofences(dateline).expect("fences")[0].longitude,
        Some(-180.0)
    );
    // Any two longitudes are the same place at a pole.
    let pole = c8_vehicle(&store, "t10-pole");
    save(
        &store,
        pole,
        None,
        with_radius(fence("Pole", 90.0, 0.0), 20.0),
    )
    .expect("pole");
    assert_eq!(
        rejection(save(
            &store,
            pole,
            None,
            with_radius(fence("Pole2", 90.0, 135.0), 20.0)
        )),
        GeofenceRejection::DuplicatePlace { existing_id: H }
    );
    // Likewise around the pole.
    let moved = save(
        &store,
        pole,
        Some(H),
        with_radius(fence("Pole", 90.0, 135.0), 20.0),
    )
    .expect("edit around the pole");
    assert_eq!((moved.id, moved.longitude), (H, Some(135.0)));

    // Imported duplicates are copied and stay renamable (a name-only edit is
    // never refused for place); an equal create returns the smallest id.
    let twins = c8_vehicle(&store, "t10-twins");
    imp(
        &store,
        twins,
        &[
            imported(1, "Twin", 10.0, 20.0, 50.0),
            imported(2, "Twin", 10.0, 20.0, 50.0),
        ],
    );
    let renamed = save(&store, twins, Some(2), fence("Renamed", 10.0, 20.0)).expect("rename");
    assert_eq!((renamed.id, renamed.radius_m), (2, Some(50.0)));
    imp(
        &store,
        twins,
        &[
            imported(3, "Same", 11.0, 20.0, 50.0),
            imported(4, "Same", 11.0, 20.0, 50.0),
        ],
    );
    let state = durable(&store, twins);
    let replayed = save(&store, twins, None, fence("Same", 11.0, 20.0)).expect("replay");
    assert_eq!(replayed.id, 3, "the smallest id of the equal fences");
    assert_eq!(durable(&store, twins), state);
    // A refusal names the smallest id too, for a create and for an edit that
    // moves a fence onto two others at one place (C8 R9 item 2).
    assert_eq!(
        rejection(save(&store, twins, None, fence("Fresh", 10.0, 20.0))),
        GeofenceRejection::DuplicatePlace { existing_id: 1 }
    );
    assert_eq!(
        rejection(save(&store, twins, Some(4), fence("Same", 10.0, 20.0))),
        GeofenceRejection::DuplicatePlace { existing_id: 1 }
    );
    assert_eq!(durable(&store, twins), state);
}

/// C8 T41: the owner's edit and deletion survive a later import; the source's
/// own changes reach a fence again once the values agree.
fn t41_reconciliation(store: &HubStore) {
    let vehicle = c8_vehicle(store, "t41");
    let a = imported(1, "A", 10.0, 20.0, 50.0);
    let gym = imported(3, "Gym", 12.0, 20.0, 50.0);
    let pool = imported(4, "Pool", 13.0, 20.0, 50.0);
    imp(
        store,
        vehicle,
        &[
            a.clone(),
            imported(2, "B", 11.0, 20.0, 50.0),
            gym.clone(),
            pool.clone(),
        ],
    );
    save(store, vehicle, Some(3), fence("Gym East", 12.0, 20.0)).expect("edit");
    store.delete_geofence(vehicle, 4).expect("delete");
    let report = imp(store, vehicle, &[a, gym.clone(), pool]);
    assert_eq!(
        report,
        GeofenceUpsertReport {
            unchanged: vec![1],
            kept_hub_values: vec![3],
            deleted_in_hub: vec![4],
            absent_from_source: vec![2],
            ..report_with(3)
        }
    );
    let rows = store.geofences(vehicle).expect("fences");
    assert_eq!(
        rows.iter()
            .map(|row| (row.id, row.name.as_str()))
            .collect::<Vec<_>>(),
        [(1, "A"), (2, "B"), (3, "Gym East")]
    );
    let connection = store.open().expect("connection");
    assert_eq!(source_owned_of(&connection, vehicle, 3), 0);
    // The values agree again: `unchanged`, and the row is the source's again.
    let report = imp(store, vehicle, &[imported(3, "Gym East", 12.0, 20.0, 50.0)]);
    assert_eq!(
        report,
        GeofenceUpsertReport {
            unchanged: vec![3],
            absent_from_source: vec![1, 2],
            ..report_with(1)
        }
    );
    assert_eq!(source_owned_of(&connection, vehicle, 3), 1);
    let report = imp(store, vehicle, &[gym]);
    assert_eq!(
        report,
        GeofenceUpsertReport {
            updated: vec![3],
            absent_from_source: vec![1, 2],
            ..report_with(1)
        }
    );
    assert_eq!(store.geofences(vehicle).expect("fences")[2].name, "Gym");
}

/// C8 T42: a fence the slice does not mention stays, and a later source change
/// still reaches a fence the import wrote.
fn t42_absent_from_source(store: &HubStore) {
    let vehicle = c8_vehicle(store, "t42");
    let a = imported(1, "A", 10.0, 20.0, 50.0);
    let b = imported(2, "B", 11.0, 20.0, 50.0);
    let c = imported(3, "C", 12.0, 20.0, 50.0);
    imp(store, vehicle, &[a.clone(), b, c.clone()]);
    let work = save(store, vehicle, None, fence("Work", 14.0, 20.0)).expect("Hub fence");
    assert_eq!(work.id, H);
    seed_drive(
        store,
        vehicle,
        &drive_row(30, None, Some((11.0004, 20.0)), None, Some("B")),
    );
    let before = durable(store, vehicle);
    let reads = relabel_page_reads();
    let report = imp(store, vehicle, &[a.clone(), c.clone()]);
    assert_eq!(
        report,
        GeofenceUpsertReport {
            unchanged: vec![1, 3],
            absent_from_source: vec![2],
            ..report_with(2)
        },
        "the Hub fence is not listed"
    );
    assert_eq!(
        relabel_page_reads(),
        reads,
        "no fence changed: no page read"
    );
    // Rows 2 and H are identical in every column and nothing else moved: no
    // mutation, no export mark, no tombstone, no label change.
    assert_eq!(durable(store, vehicle), before);
    assert!(before.tombstones.is_empty());
    assert_eq!(end_label(store, vehicle, 30).as_deref(), Some("B"));
    assert_eq!(
        store
            .geofence_name_at(vehicle, 11.0004, 20.0)
            .expect("name")
            .as_deref(),
        Some("B")
    );
    // The source's own later change still reaches the kept row.
    let revision = export_revision(store, vehicle);
    let log = mutation_log(store, vehicle).len();
    let report = imp(store, vehicle, &[a, imported(2, "B2", 11.0, 20.0, 50.0), c]);
    assert_eq!(
        report,
        GeofenceUpsertReport {
            updated: vec![2],
            unchanged: vec![1, 3],
            relabel: RelabelCounts {
                drives: 1,
                charges: 0
            },
            ..report_with(3)
        }
    );
    assert_eq!(
        store.geofences(vehicle).expect("fences")[1].name,
        "B2",
        "a kept row stays source-owned"
    );
    assert_eq!(export_revision(store, vehicle), revision + 1, "X = yes");
    assert_eq!(geofence_entity_mutations(store, vehicle), 0);
    // The relabel of this import: drive 30's end label becomes "B2" with
    // exactly its drive mutation.
    assert_eq!(end_label(store, vehicle, 30).as_deref(), Some("B2"));
    assert_eq!(mutation_log(store, vehicle)[log..], ["drive 30"]);
}

/// C8 T43: a Hub edit survives re-imports, including a changed source.
fn t43_hub_edit_survives(store: &HubStore) {
    let vehicle = c8_vehicle(store, "t43");
    let a = imported(1, "A", 10.0, 20.0, 50.0);
    let gym = imported(3, "Gym", 12.0, 20.0, 50.0);
    imp(store, vehicle, &[a.clone(), gym.clone()]);
    seed_drive(
        store,
        vehicle,
        &drive_row(32, None, Some((12.0004, 20.0)), None, Some("Gym")),
    );
    seed_drive(
        store,
        vehicle,
        &drive_row(33, None, Some((12.0006, 20.0)), None, None),
    );
    let edited = save(
        store,
        vehicle,
        Some(3),
        with_radius(with_cost(fence("Gym East", 12.0, 20.0), 0.4), 80.0),
    )
    .expect("edit");
    assert_eq!(
        edited,
        Fence {
            id: 3,
            name: "Gym East".to_owned(),
            latitude: Some(12.0),
            longitude: Some(20.0),
            radius_m: Some(80.0),
            billing_type: Some(GeofenceBillingType::PerKwh),
            cost_per_unit: Some(0.4),
            session_fee: None,
        }
    );
    let connection = store.open().expect("connection");
    assert_eq!(source_owned_of(&connection, vehicle, 3), 0);
    for id in [32, 33] {
        assert_eq!(end_label(store, vehicle, id).as_deref(), Some("Gym East"));
    }
    let before = durable(store, vehicle);
    let report = imp(store, vehicle, &[a.clone(), gym]);
    assert_eq!(
        report,
        GeofenceUpsertReport {
            unchanged: vec![1],
            kept_hub_values: vec![3],
            ..report_with(2)
        }
    );
    assert_eq!(durable(store, vehicle), before, "M = 0, X = no, row 3 same");
    assert_eq!(
        store
            .geofence_name_at(vehicle, 12.0006, 20.0)
            .expect("name")
            .as_deref(),
        Some("Gym East")
    );
    // The source changes after the edit: the Hub's values still win.
    let report = imp(store, vehicle, &[a, imported(3, "Gym", 12.0, 20.0, 60.0)]);
    assert_eq!(report.kept_hub_values, [3]);
    assert_eq!(durable(store, vehicle), before);
}

/// C8 T44: a Hub deletion survives re-imports, including a changed source; a
/// Hub fence at the same place is a different fence.
fn t44_hub_deletion_survives(store: &HubStore) {
    let vehicle = c8_vehicle(store, "t44");
    let a = imported(1, "A", 10.0, 20.0, 50.0);
    let pool = imported(4, "Pool", 13.0, 20.0, 50.0);
    imp(store, vehicle, &[a.clone(), pool.clone()]);
    seed_drive(
        store,
        vehicle,
        &drive_row(34, None, Some((13.0004, 20.0)), None, Some("Pool")),
    );
    seed_drive(
        store,
        vehicle,
        &drive_row(35, None, None, Some("Pool"), Some("Pool")),
    );
    let log = mutation_log(store, vehicle).len();
    store.delete_geofence(vehicle, 4).expect("delete");
    assert_eq!(end_label(store, vehicle, 34), None);
    assert_eq!(
        drive_labels(store, vehicle, 35),
        (Some("Pool".to_owned()), Some("Pool".to_owned()))
    );
    assert_eq!(mutation_log(store, vehicle)[log..], ["drive 34"]);
    assert_eq!(durable(store, vehicle).tombstones, [["4"]]);

    let before = durable(store, vehicle);
    let report = imp(store, vehicle, &[a.clone(), pool]);
    assert_eq!(
        report,
        GeofenceUpsertReport {
            unchanged: vec![1],
            deleted_in_hub: vec![4],
            ..report_with(2)
        }
    );
    assert_eq!(
        durable(store, vehicle),
        before,
        "M = 0, X = no, tombstone stays"
    );
    assert_eq!(end_label(store, vehicle, 34), None);
    assert_eq!(
        store
            .geofence_name_at(vehicle, 13.0004, 20.0)
            .expect("name"),
        None
    );
    // The source changes after the delete: nothing is brought back.
    let report = imp(
        store,
        vehicle,
        &[a.clone(), imported(4, "Pool", 13.0, 20.0, 70.0)],
    );
    assert_eq!(report.deleted_in_hub, [4]);
    assert_eq!(durable(store, vehicle), before);
    // A row that is not activated is reported by its reason, ahead of the
    // tombstone: nothing is written and it is not `deleted_in_hub` (table 5.4).
    let report = imp(
        store,
        vehicle,
        &[a.clone(), imported(4, "Pool", 13.0, 20.0, 0.0)],
    );
    assert_eq!(
        report,
        GeofenceUpsertReport {
            unchanged: vec![1],
            inactive: [(GeofenceInactiveReason::RadiusNotPositive, vec![4])]
                .into_iter()
                .collect(),
            ..report_with(2)
        }
    );
    assert_eq!(durable(store, vehicle), before);
    // Bringing the place back is a Hub act with a Hub id.
    let log = mutation_log(store, vehicle).len();
    let back = save(store, vehicle, None, fence("Pool", 13.0, 20.0)).expect("Hub fence");
    assert_eq!(back.id, H);
    assert_eq!(end_label(store, vehicle, 34).as_deref(), Some("Pool"));
    assert_eq!(mutation_log(store, vehicle)[log..], ["drive 34"]);
    assert_eq!(durable(store, vehicle).tombstones, [["4"]]);
    let held = durable(store, vehicle);
    let report = imp(store, vehicle, &[a, imported(4, "Pool", 13.0, 20.0, 50.0)]);
    assert_eq!(
        report,
        GeofenceUpsertReport {
            unchanged: vec![1],
            deleted_in_hub: vec![4],
            ..report_with(2)
        },
        "the Hub fence is in no list"
    );
    assert_eq!(durable(store, vehicle), held);
}

/// C8 T45: a repeated import is a no-op (C8 R47).
fn t45_reimport_is_a_noop(store: &HubStore) {
    let vehicle = c8_vehicle(store, "t45");
    let s1 = vec![
        imported(1, "A", 10.0, 20.0, 50.0),
        imported(2, "B", 11.0, 20.0, 50.0),
        imported(3, "Gym", 12.0, 20.0, 50.0),
        imported(4, "Pool", 13.0, 20.0, 50.0),
        imported(5, "E", 15.0, 20.0, 0.0),
        imported_priced(
            6,
            "F",
            16.0,
            20.0,
            50.0,
            GeofenceBillingType::PerKwh,
            Some(f64::NAN),
            None,
        ),
        imported(7, "G", 17.0, 20.0, 50.0),
    ];
    imp(store, vehicle, &s1);
    seed_drive(
        store,
        vehicle,
        &drive_row(36, None, Some((10.0004, 20.0)), None, Some("A")),
    );
    seed_drive(
        store,
        vehicle,
        &drive_row(37, None, Some((12.0004, 20.0)), None, Some("Gym")),
    );
    save(store, vehicle, Some(3), fence("Gym East", 12.0, 20.0)).expect("edit 3");
    save(store, vehicle, Some(7), fence("G2", 17.0, 20.0)).expect("edit 7");
    store.delete_geofence(vehicle, 4).expect("delete 4");
    let work = save(store, vehicle, None, fence("Work", 14.0, 20.0)).expect("Hub fence");
    assert_eq!(work.id, H);
    // S2 is S1 without id 2 and with id 7 as the Hub edited it.
    let s2: Vec<Fence> = s1
        .iter()
        .filter(|row| row.id != 2)
        .map(|row| {
            if row.id == 7 {
                imported(7, "G2", 17.0, 20.0, 50.0)
            } else {
                row.clone()
            }
        })
        .collect();

    let before = durable(store, vehicle);
    let first = imp(store, vehicle, &s2);
    assert_eq!(
        first,
        GeofenceUpsertReport {
            unchanged: vec![1, 6, 7],
            kept_hub_values: vec![3],
            deleted_in_hub: vec![4],
            inactive: [(GeofenceInactiveReason::RadiusNotPositive, vec![5])]
                .into_iter()
                .collect(),
            tariff_not_finite: vec![6],
            absent_from_source: vec![2],
            ..report_with(6)
        }
    );
    // Only `source_owned` of row 7 changed (the values agree again); row 6
    // keeps its NULL cost; M = 0 and X = no (C8 R28).
    let between = durable(store, vehicle);
    assert_eq!(
        Durable {
            fences: with_owned(before.fences.clone(), 7, "1"),
            ..before
        },
        between
    );
    let row_6 = between
        .fences
        .iter()
        .find(|row| row[0] == "6")
        .expect("row 6");
    assert_eq!(row_6[5..8], ["'per_kwh'", "NULL", "NULL"]);
    // The second run reports the same and changes nothing.
    let second = imp(store, vehicle, &s2);
    assert_eq!(second, first);
    assert_eq!(durable(store, vehicle), between);
    assert_eq!(end_label(store, vehicle, 36).as_deref(), Some("A"));
    assert_eq!(end_label(store, vehicle, 37).as_deref(), Some("Gym East"));
    assert_eq!(geofence_entity_mutations(store, vehicle), 0);
}

#[test]
fn c8_t41_to_t45_reconciliation() {
    let (_temporary, store) = c8_store();
    t41_reconciliation(&store);
    t42_absent_from_source(&store);
    t43_hub_edit_survives(&store);
    t44_hub_deletion_survives(&store);
    t45_reimport_is_a_noop(&store);
}

/// One step of C8 T48's sequence.
#[derive(Clone)]
enum Step {
    Import(Vec<Fence>),
    Save(Option<i64>, Fence),
    Delete(i64),
}

/// A vehicle imported through finalisation: the lineage the next import
/// successor extends.
struct Finalised {
    vehicle: Uuid,
    binding: ProjectionBinding,
    prior: LineageManifestV2,
    serial: u64,
}

impl Finalised {
    /// Finalise one import successor carrying `geofences`.
    fn import(&mut self, store: &HubStore, geofences: &[Fence]) {
        let delta = compatibility_successor(store, &self.binding, &self.prior, Uuid::new_v4());
        let cursor = import_delta_test_cursor(&self.binding, delta.to_sequence);
        self.serial += 1;
        store
            .finalize_import_delta_successor(
                self.vehicle,
                &delta,
                &import_delta_test_cursor_key(),
                &cursor,
                Sha256Digest::of_bytes(&self.serial.to_le_bytes()),
                geofences,
            )
            .expect("finalise the import successor");
        self.prior.head_sequence = delta.to_sequence;
        self.prior.head_digest = delta.chain_digest;
        self.prior.terminal_cursor = cursor;
        self.prior.deltas.push(delta);
    }
}

#[test]
fn c8_t48_finalisation_path() {
    // The same slices through the store-level upsert, the transaction-level
    // upsert the import finalisation calls, and a real finalisation (an import
    // successor): the same reconciliation and durable state (C8 R51).
    let (_temporary, store) = c8_store();
    let by_store = c8_vehicle(&store, "t48-store");
    let by_transaction = c8_vehicle(&store, "t48-transaction");
    let other_directory = crate::private_tempdir().expect("temporary store");
    let other = HubStore::initialize(other_directory.path()).expect("second store");
    let (fixture, binding, prior) = imported_v2_base(&other);
    let mut finalised = Finalised {
        vehicle: fixture.vehicle_id,
        binding,
        prior,
        serial: 0,
    };

    let a = imported(1, "A", 10.0, 20.0, 50.0);
    let gym = imported(3, "Gym", 12.0, 20.0, 50.0);
    let pool = imported(4, "Pool", 13.0, 20.0, 50.0);
    let slice = vec![a.clone(), gym.clone(), pool.clone()];
    let edited_slice = vec![
        a,
        imported(3, "Gym", 12.0, 20.0, 60.0),
        imported(4, "Pool", 13.0, 20.0, 70.0),
    ];
    // Per step: the step, the drive mutations it recorded (the spec's M), whether
    // it marked the export (X), and the end labels of drives 32, 33 and 34 after
    // it. The drives are those of C8 T43 and C8 T44, seeded once the first
    // import has run.
    const EAST: Option<&str> = Some("Gym East");
    let steps: [(Step, &[&str], bool, [Option<&str>; 3]); 7] = [
        (
            Step::Import(slice.clone()),
            &[],
            true,
            [Some("Gym"), None, Some("Pool")],
        ),
        (
            Step::Save(
                Some(3),
                with_radius(with_cost(fence("Gym East", 12.0, 20.0), 0.4), 80.0),
            ),
            &["drive 32", "drive 33"],
            true,
            [EAST, EAST, Some("Pool")],
        ),
        (Step::Delete(4), &["drive 34"], true, [EAST, EAST, None]),
        (Step::Import(slice.clone()), &[], false, [EAST, EAST, None]),
        (Step::Import(edited_slice), &[], false, [EAST, EAST, None]),
        (
            Step::Save(None, fence("Pool", 13.0, 20.0)),
            &["drive 34"],
            true,
            [EAST, EAST, Some("Pool")],
        ),
        (Step::Import(slice), &[], false, [EAST, EAST, Some("Pool")]),
    ];
    let routes = [
        (by_store, &store),
        (by_transaction, &store),
        (finalised.vehicle, &other),
    ];
    for (index, (step, mutations, marked, labels)) in steps.iter().enumerate() {
        let logs = routes.map(|(target, host)| mutation_log(host, target).len());
        let exports = [
            export_revision(&store, by_store),
            export_revision(&store, by_transaction),
        ];
        let (store_report, transaction_report) = match step {
            Step::Import(rows) => {
                finalised.import(&other, rows);
                (
                    Some(imp(&store, by_store, rows)),
                    Some(import_in_transaction(&store, by_transaction, rows)),
                )
            }
            Step::Save(id, record) => {
                for (target, host) in routes {
                    save(host, target, *id, record.clone()).expect("Hub save");
                }
                (None, None)
            }
            Step::Delete(id) => {
                for (target, host) in routes {
                    host.delete_geofence(target, *id).expect("Hub delete");
                }
                (None, None)
            }
        };
        if index == 0 {
            // C8 T43 (drives 32 and 33) and C8 T44 (drives 34 and 35) on every
            // route.
            for (target, host) in routes {
                for row in [
                    drive_row(32, None, Some((12.0004, 20.0)), None, Some("Gym")),
                    drive_row(33, None, Some((12.0006, 20.0)), None, None),
                    drive_row(34, None, Some((13.0004, 20.0)), None, Some("Pool")),
                    drive_row(35, None, None, Some("Pool"), Some("Pool")),
                    drive_row(36, None, Some((10.0004, 20.0)), None, Some("A")),
                ] {
                    seed_drive(host, target, &row);
                }
            }
        }
        // The two routes of one store report alike and leave equal state.
        assert_eq!(store_report, transaction_report, "step {index}: report");
        let state = durable(&store, by_store);
        assert_eq!(state, durable(&store, by_transaction), "step {index}");
        // The finalisation leaves the same fences, marks and tombstones.
        let finalised_state = durable(&other, finalised.vehicle);
        assert_eq!(state.fences, finalised_state.fences, "step {index}: fences");
        assert_eq!(state.marks, finalised_state.marks, "step {index}: marks");
        assert_eq!(
            state.tombstones, finalised_state.tombstones,
            "step {index}: tombstones"
        );
        assert_eq!(geofence_entity_mutations(&store, by_store), 0);
        assert_eq!(geofence_entity_mutations(&other, finalised.vehicle), 0);
        // M, X and the labels of the seeded drives, on every route.
        for ((target, host), log) in routes.into_iter().zip(logs) {
            let recorded: Vec<String> = mutation_log(host, target)[log..]
                .iter()
                .filter(|entry| entry.starts_with("drive "))
                .cloned()
                .collect();
            assert_eq!(recorded, *mutations, "step {index}: M of {target}");
            for (id, label) in [32, 33, 34].into_iter().zip(labels) {
                assert_eq!(
                    end_label(host, target, id).as_deref(),
                    *label,
                    "step {index}: drive {id} of {target}"
                );
            }
            assert_eq!(
                drive_labels(host, target, 35),
                (Some("Pool".to_owned()), Some("Pool".to_owned())),
                "step {index}: drive 35 of {target}"
            );
        }
        for (target, before) in [by_store, by_transaction].into_iter().zip(exports) {
            assert_eq!(
                export_revision(&store, target) - before,
                i64::from(*marked),
                "step {index}: X of {target}"
            );
        }
        // What the reconciliation decides on the way (C8 T43, C8 T44).
        if let (3 | 4, Some(report)) = (index, &store_report) {
            assert_eq!(report.kept_hub_values, [3], "step {index}");
            assert_eq!(report.deleted_in_hub, [4], "step {index}");
            assert_eq!(report.unchanged, [1], "step {index}");
            assert_eq!(report.relabel, RelabelCounts::default());
        }
        if let (6, Some(report)) = (index, &store_report) {
            assert_eq!(report.deleted_in_hub, [4]);
            assert_eq!(report.kept_hub_values, [3]);
        }
    }
    // The tombstone and the Hub's edit are still there at the end, in every
    // route; the Hub fence at the deleted place has the first Hub id.
    let end = durable(&store, by_store);
    assert_eq!(end.tombstones, [["4"]]);
    assert_eq!(end.marks, [["2147483648"]]);
    assert_eq!(
        end.fences
            .iter()
            .map(|row| (row[0].as_str(), row[1].as_str(), row[8].as_str()))
            .collect::<Vec<_>>(),
        [
            ("1", "'A'", "1"),
            ("3", "'Gym East'", "0"),
            ("2147483648", "'Pool'", "0")
        ]
    );
    // The relabel half of the import (C8 R16): the source renames fence 1, which
    // the earlier steps left source-owned, and the one drive that carries its
    // name follows it on every route, with exactly its drive mutation.
    let renamed = vec![
        imported(1, "A2", 10.0, 20.0, 50.0),
        imported(3, "Gym", 12.0, 20.0, 50.0),
        imported(4, "Pool", 13.0, 20.0, 50.0),
    ];
    let logs = routes.map(|(target, host)| mutation_log(host, target).len());
    let exports = [
        export_revision(&store, by_store),
        export_revision(&store, by_transaction),
    ];
    finalised.import(&other, &renamed);
    let by_store_report = imp(&store, by_store, &renamed);
    let by_transaction_report = import_in_transaction(&store, by_transaction, &renamed);
    assert_eq!(by_store_report, by_transaction_report);
    assert_eq!(
        (by_store_report.updated.clone(), by_store_report.relabel),
        (
            vec![1],
            RelabelCounts {
                drives: 1,
                charges: 0
            }
        )
    );
    for ((target, host), log) in routes.into_iter().zip(logs) {
        assert_eq!(
            end_label(host, target, 36).as_deref(),
            Some("A2"),
            "relabel of {target}"
        );
        assert_eq!(mutation_log(host, target)[log..], ["drive 36"], "{target}");
    }
    for (target, before) in [by_store, by_transaction].into_iter().zip(exports) {
        assert_eq!(export_revision(&store, target), before + 1, "X of {target}");
    }
    assert_eq!(
        durable(&store, by_store),
        durable(&store, by_transaction),
        "the two routes of one store"
    );
}

#[test]
fn c8_duplicate_import_ids_roll_back() {
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "duplicates");
    imp(&store, vehicle, &[imported(5, "Kept", 12.0, 20.0, 50.0)]);
    let before = durable(&store, vehicle);
    // Ids are unique in a slice (the source column is a key): a repeat with
    // other values, with equal values, or between rows that are not activated
    // is refused with nothing written, wherever the repeat sits in the slice.
    let slices = [
        vec![
            imported(1, "A", 10.0, 20.0, 50.0),
            imported(2, "B", 11.0, 20.0, 50.0),
            imported(1, "A2", 10.0, 20.0, 60.0),
        ],
        vec![
            imported(1, "A", 10.0, 20.0, 50.0),
            imported(1, "A", 10.0, 20.0, 50.0),
        ],
        vec![
            imported(0, "Z", 10.0, 20.0, 50.0),
            imported(0, "Y", 10.0, 20.0, 0.0),
        ],
        vec![
            imported(3, "C", 13.0, 20.0, 50.0),
            imported(5, "Kept", 12.0, 20.0, 50.0),
            imported(4, "D", 14.0, 20.0, 50.0),
            imported(3, "C", 13.0, 20.0, 40.0),
        ],
    ];
    for (index, slice) in slices.iter().enumerate() {
        assert!(
            matches!(
                store.upsert_geofences(vehicle, slice),
                Err(StoreError::InvalidGeofence)
            ),
            "slice {index}: store path"
        );
        assert_eq!(durable(&store, vehicle), before, "slice {index}: store");
        let mut connection = store.open().expect("connection");
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("begin");
        assert!(
            matches!(
                upsert_geofences_in_transaction(&transaction, vehicle, slice),
                Err(StoreError::InvalidGeofence)
            ),
            "slice {index}: transaction path"
        );
        drop(transaction);
        assert_eq!(
            durable(&store, vehicle),
            before,
            "slice {index}: transaction"
        );
    }
    // Inside one transaction an earlier import of the same transaction stays
    // while the refused call writes nothing of its own before the refusal; the
    // caller's rollback then leaves the store as it was.
    let mut connection = store.open().expect("connection");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin");
    let first = upsert_geofences_in_transaction(
        &transaction,
        vehicle,
        &[imported(9, "Nine", 19.0, 20.0, 50.0)],
    )
    .expect("first call");
    assert_eq!(first.inserted, [9]);
    assert!(matches!(
        upsert_geofences_in_transaction(&transaction, vehicle, &slices[0]),
        Err(StoreError::InvalidGeofence)
    ));
    let rows: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM geofences WHERE vehicle_id = ?1 AND source_geofence_id IN (1, 2)",
            params![vehicle.to_string()],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(rows, 0, "the refused slice wrote no row");
    let nine: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM geofences WHERE vehicle_id = ?1 AND source_geofence_id = 9",
            params![vehicle.to_string()],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(nine, 1, "the transaction's earlier import is untouched");
    drop(transaction);
    assert_eq!(durable(&store, vehicle), before);
    assert_eq!(count(&store.open().expect("connection"), "geofences"), 1);
    // The same ids in two separate slices are not duplicates.
    let report = imp(&store, vehicle, &[imported(1, "A", 10.0, 20.0, 50.0)]);
    assert_eq!(report.inserted, [1]);
    let report = imp(&store, vehicle, &[imported(1, "A", 10.0, 20.0, 50.0)]);
    assert_eq!(report.unchanged, [1]);
}

/// C8 T15: the import reconciliation, the relabel of the stored drives, the
/// report, the export mark and the absence of any fence entity mutation.
/// `import` is the import route under test.
fn t15_sequence(
    store: &HubStore,
    key: &str,
    import: impl Fn(&HubStore, Uuid, &[Fence]) -> GeofenceUpsertReport,
) {
    // 1. A fresh vehicle: import id 1, Home, centre (10.0, 20.0), radius 50.
    let vehicle = c8_vehicle(store, key);
    let home = |name: &str, radius: f64| imported(1, name, 10.0, 20.0, radius);
    let report = import(store, vehicle, &[home("Home", 50.0)]);
    assert_eq!(report.inserted, [1]);
    // 2. Collected drive 1 and drive 2, as the spec seeds them.
    seed_drive(
        store,
        vehicle,
        &drive_row(
            1,
            Some((12.0, 20.0)),
            Some((10.0004, 20.0)),
            None,
            Some("Home"),
        ),
    );
    seed_drive(
        store,
        vehicle,
        &drive_row(2, Some((12.0, 20.0)), Some((10.0009, 20.0)), None, None),
    );
    // 3. The unchanged fence: `unchanged` [1], relabel {0, 0}, no sync
    // mutation, no export mark, history as it was.
    let baseline = durable(store, vehicle);
    let marked = export_revision(store, vehicle);
    assert_eq!(marked, 1, "the first import marked the export once");
    let reads = relabel_page_reads();
    let report = import(store, vehicle, &[home("Home", 50.0)]);
    assert_eq!(
        report,
        GeofenceUpsertReport {
            unchanged: vec![1],
            ..report_with(1)
        }
    );
    assert_eq!(
        report.relabel,
        RelabelCounts {
            drives: 0,
            charges: 0
        }
    );
    assert_eq!(durable(store, vehicle), baseline);
    assert!(mutation_log(store, vehicle).is_empty());
    assert_eq!(
        relabel_page_reads(),
        reads,
        "no fence changed: no page read"
    );
    // 4. The name changes to House at radius 50: `updated` [1]; drive 1's end
    // label becomes House with exactly its drive mutation, the report counts one
    // drive and no charges, and the export is marked once.
    let report = import(store, vehicle, &[home("House", 50.0)]);
    assert_eq!(report.updated, [1]);
    assert!(report.inserted.is_empty() && report.unchanged.is_empty());
    assert_eq!(
        report.relabel,
        RelabelCounts {
            drives: 1,
            charges: 0
        }
    );
    assert_eq!(store.geofences(vehicle).expect("fences")[0].name, "House");
    assert_eq!(
        drive_labels(store, vehicle, 1),
        (None, Some("House".to_owned()))
    );
    assert_eq!(drive_labels(store, vehicle, 2), (None, None));
    assert_eq!(mutation_log(store, vehicle), ["drive 1"]);
    assert_eq!(export_revision(store, vehicle), marked + 1);
    // 5. The radius grows to 120: `updated` [1]; drive 2's end label becomes
    // House with exactly its drive mutation, the report counts one drive and no
    // charges, drive 1 is not rewritten, and the export is marked once.
    let drive_one = durable(store, vehicle).drives[0].clone();
    let report = import(store, vehicle, &[home("House", 120.0)]);
    assert_eq!(report.updated, [1]);
    assert_eq!(
        report.relabel,
        RelabelCounts {
            drives: 1,
            charges: 0
        }
    );
    assert_eq!(
        store.geofences(vehicle).expect("fences")[0].radius_m,
        Some(120.0)
    );
    assert_eq!(
        drive_labels(store, vehicle, 2),
        (None, Some("House".to_owned()))
    );
    assert_eq!(durable(store, vehicle).drives[0], drive_one);
    assert_eq!(mutation_log(store, vehicle), ["drive 1", "drive 2"]);
    assert_eq!(export_revision(store, vehicle), marked + 2);
    // 6. No geofence entity mutation, ever (C8 R32).
    assert_eq!(geofence_entity_mutations(store, vehicle), 0);
}

#[test]
fn c8_t15_import_relabel_store() {
    let (_temporary, store) = c8_store();
    t15_sequence(&store, "t15-store", |store, vehicle, rows| {
        store.upsert_geofences(vehicle, rows).expect("import")
    });
}

#[test]
fn c8_t15_import_relabel_transaction() {
    let (_temporary, store) = c8_store();
    t15_sequence(&store, "t15-transaction", |store, vehicle, rows| {
        import_in_transaction(store, vehicle, rows)
    });
}

/// One of the seven compared values of a fence: the record with only that
/// value changed, its column in `Durable::fences` and its stored text once
/// changed, and whether a create that differs from an existing fence in only
/// that value is a new fence (the centre or the radius differs) or the same
/// place (C8 R9 item 2).
struct FieldChange {
    field: &'static str,
    change: fn(Fence) -> Fence,
    column: usize,
    changed: &'static str,
    new_fence: bool,
}

const FIELD_CHANGES: [FieldChange; 7] = [
    FieldChange {
        field: "name",
        change: |record| Fence {
            name: "Other".to_owned(),
            ..record
        },
        column: 1,
        changed: "'Other'",
        new_fence: false,
    },
    FieldChange {
        field: "latitude",
        change: |record| Fence {
            latitude: Some(10.000001),
            ..record
        },
        column: 2,
        changed: "10.000001",
        new_fence: true,
    },
    FieldChange {
        field: "longitude",
        change: |record| Fence {
            longitude: Some(20.000001),
            ..record
        },
        column: 3,
        changed: "20.000001",
        new_fence: true,
    },
    FieldChange {
        field: "radius",
        change: |record| Fence {
            radius_m: Some(60.0),
            ..record
        },
        column: 4,
        changed: "60.0",
        new_fence: true,
    },
    FieldChange {
        field: "billing type",
        change: |record| Fence {
            billing_type: Some(GeofenceBillingType::PerMinute),
            ..record
        },
        column: 5,
        changed: "'per_minute'",
        new_fence: false,
    },
    FieldChange {
        field: "cost",
        change: |record| Fence {
            cost_per_unit: Some(0.6),
            ..record
        },
        column: 6,
        changed: "0.6",
        new_fence: false,
    },
    FieldChange {
        field: "fee",
        change: |record| Fence {
            session_fee: Some(2.0),
            ..record
        },
        column: 7,
        changed: "2.0",
        new_fence: false,
    },
];

/// A fence with all seven values given.
fn priced_base(id: i64) -> Fence {
    imported_priced(
        id,
        "Base",
        10.0,
        20.0,
        50.0,
        GeofenceBillingType::PerKwh,
        Some(0.5),
        Some(1.0),
    )
}

#[test]
fn c8_one_changed_value_is_a_change() {
    // Replay (C8 R9 item 1), no-op (C8 R10) and unchanged or updated (C8 table
    // 5.4) compare all seven values exactly: a difference in any one of them is
    // a change, never a replay, a no-op or `unchanged`.
    let (_temporary, store) = c8_store();
    for (index, change) in FIELD_CHANGES.iter().enumerate() {
        let field = change.field;
        // An import of a source-owned row that differs in this value only.
        let imported_vehicle = c8_vehicle(&store, &format!("one-import-{index}"));
        assert_eq!(
            imp(&store, imported_vehicle, &[priced_base(1)]).inserted,
            [1]
        );
        let marked = export_revision(&store, imported_vehicle);
        let report = imp(&store, imported_vehicle, &[(change.change)(priced_base(1))]);
        assert_eq!(
            report,
            GeofenceUpsertReport {
                updated: vec![1],
                ..report_with(1)
            },
            "{field}: import"
        );
        let state = durable(&store, imported_vehicle);
        assert_eq!(state.fences[0][change.column], change.changed, "{field}");
        assert_eq!(state.fences[0][8], "1", "{field}: still source-owned");
        assert_eq!(export_revision(&store, imported_vehicle), marked + 1);
        // An edit that gives all seven values and differs in this one only.
        let edited_vehicle = c8_vehicle(&store, &format!("one-edit-{index}"));
        imp(&store, edited_vehicle, &[priced_base(1)]);
        let marked = export_revision(&store, edited_vehicle);
        let record = (change.change)(Fence {
            id: 0,
            ..priced_base(1)
        });
        let saved = save(&store, edited_vehicle, Some(1), record.clone()).expect(field);
        assert_eq!(saved, Fence { id: 1, ..record }, "{field}: edit");
        let state = durable(&store, edited_vehicle);
        assert_eq!(state.fences[0][change.column], change.changed, "{field}");
        assert_eq!(state.fences[0][8], "0", "{field}: written by the Hub");
        assert_eq!(export_revision(&store, edited_vehicle), marked + 1);
        // A create that differs from an existing fence in this value only is
        // not a replay.
        let created_vehicle = c8_vehicle(&store, &format!("one-create-{index}"));
        let base = Fence {
            id: 0,
            ..priced_base(1)
        };
        let first = save(&store, created_vehicle, None, base.clone()).expect("create");
        assert_eq!(first.id, H);
        let state = durable(&store, created_vehicle);
        let outcome = save(&store, created_vehicle, None, (change.change)(base));
        if change.new_fence {
            assert_eq!(outcome.expect(field).id, H + 1, "{field}: create");
            assert_eq!(store.geofences(created_vehicle).expect("fences").len(), 2);
        } else {
            assert_eq!(
                rejection(outcome),
                GeofenceRejection::DuplicatePlace { existing_id: H },
                "{field}: create"
            );
            assert_eq!(durable(&store, created_vehicle), state, "{field}");
        }
    }
}

#[test]
fn c8_tariff_not_finite_lists_stored_rows_only() {
    // A not finite price is listed when it is what the row stores: inserted,
    // updated and unchanged rows. A row kept for the Hub or deleted in the Hub
    // stores nothing from the source (C8 R13, C8 table 5.4).
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "not-finite");
    let priced = |id: i64, name: &str, cost: f64| {
        imported_priced(
            id,
            name,
            10.0 + id as f64,
            20.0,
            50.0,
            GeofenceBillingType::PerKwh,
            Some(cost),
            None,
        )
    };
    let report = imp(
        &store,
        vehicle,
        &[
            priced(1, "One", f64::NAN),
            priced(3, "Three", 0.3),
            priced(4, "Four", 0.3),
            priced(5, "Five", 0.3),
        ],
    );
    assert_eq!(report.inserted, [1, 3, 4, 5]);
    assert_eq!(report.tariff_not_finite, [1]);
    save(&store, vehicle, Some(3), fence("Hub three", 13.0, 20.0)).expect("Hub edit");
    store.delete_geofence(vehicle, 4).expect("delete");
    let report = imp(
        &store,
        vehicle,
        &[
            priced(1, "One", f64::NAN),
            priced(3, "Three", f64::NAN),
            priced(4, "Four", f64::NAN),
            priced(5, "Five", f64::NAN),
        ],
    );
    assert_eq!(
        report,
        GeofenceUpsertReport {
            unchanged: vec![1],
            kept_hub_values: vec![3],
            deleted_in_hub: vec![4],
            updated: vec![5],
            tariff_not_finite: vec![1, 5],
            ..report_with(4)
        }
    );
}
