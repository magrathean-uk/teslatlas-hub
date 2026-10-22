// SPDX-License-Identifier: AGPL-3.0-only

//! C8 Part L tests (C8 section 8: C8 T16 to C8 T32, C8 T37 to C8 T40, C8 T46
//! and C8 T47; C8 R16, C8 R20 to C8 R28, C8 R31, C8 R33, C8 R48 and C8 R53): the
//! relabel of stored drives and charges after a fence save, delete or import,
//! its order, its rollback, and the served names of the labelled fields.
//! Synthetic data only. The relabel half of C8 T15 and C8 T42 and the import
//! half of C8 T48 are asserted in `b3_c8_sb`.
//! The neutral served-key half of C8 T46 moves to C6 Part C (see
//! `c8_t46_served_names_tariff_snapshot`).

use super::b3_c8_sa::{
    Fence, c8_store, c8_vehicle, count, fence, imported, predecessor_radius_bound,
};
use super::b3_c8_sb::{
    Durable, H, drive_labels, drive_row, durable, end_label, export_revision,
    geofence_entity_mutations, imp, mutation_log, relabel_page_reads, save, seed_drive, with_cost,
    with_radius,
};
use super::*;
use crate::geo::location::{Wgs84Point, great_circle_distance_m};
use crate::hub_pack::{GeofenceBillingType, ProjectionCharge};

/// A materialised charge with a start point and a label and no tariff.
fn charge_row(id: i64, start: Option<(f64, f64)>, label: Option<&str>) -> ProjectionCharge {
    ProjectionCharge {
        start_latitude: start.map(|point| point.0),
        start_longitude: start.map(|point| point.1),
        geofence: label.map(str::to_owned),
        ..cost_row(id, None, None, None, None)
    }
}

fn seed_charge(store: &HubStore, vehicle: Uuid, row: &ProjectionCharge) {
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                vehicle.to_string(),
                row.id,
                row.car_id,
                serde_json::to_string(row).expect("charge json")
            ],
        )
        .expect("seed charge");
}

fn stored_charge(store: &HubStore, vehicle: Uuid, id: i64) -> ProjectionCharge {
    store
        .materialised_charge_for_vehicle(vehicle, id)
        .expect("charge read")
        .expect("charge row")
}

fn charge_label(store: &HubStore, vehicle: Uuid, id: i64) -> Option<String> {
    stored_charge(store, vehicle, id).geofence
}

/// The stored rows of one table as `(id, json)`, by id.
fn raw_rows(store: &HubStore, vehicle: Uuid, table: &str) -> Vec<(i64, String)> {
    let (id, json) = match table {
        "materialised_drives" => ("drive_id", "drive_json"),
        "materialised_charges" => ("charge_id", "charge_json"),
        other => panic!("unexpected table {other}"),
    };
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(&format!(
            "SELECT {id}, {json} FROM {table} WHERE vehicle_id = ?1 ORDER BY {id}"
        ))
        .expect("prepare");
    statement
        .query_map(params![vehicle.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// The `(entity, entity_id, payload)` of every `sync_mutations` row of the
/// vehicle, in revision order.
fn mutation_payloads(store: &HubStore, vehicle: Uuid) -> Vec<(String, i64, String)> {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(
            "SELECT entity, entity_id, payload_json FROM sync_mutations
              WHERE vehicle_id = ?1 ORDER BY revision",
        )
        .expect("prepare");
    statement
        .query_map(params![vehicle.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// The one thing a step did: the mutations it recorded in order (the spec's
/// `M`, as `drive 1` or `charge 1`), whether it marked the export (the spec's
/// `X`) and how many relabel pages it read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    mutations: Vec<String>,
    exported: bool,
    page_reads: u64,
}

fn strip_labels(row: &str, keys: &[&str]) -> serde_json::Value {
    let mut value: serde_json::Value = serde_json::from_str(row).expect("row json");
    for key in keys {
        value.as_object_mut().expect("object").remove(*key);
    }
    value
}

/// A relabel changes labels only (C8 R27) and records one mutation per rewritten
/// row whose payload is the row as stored: every row outside `mutated` is
/// byte-identical, every row inside differs from its earlier text in the label
/// fields alone.
fn assert_labels_only(
    before: &[(i64, String)],
    after: &[(i64, String)],
    entity: &str,
    keys: &[&str],
    mutated: &[i64],
    payloads: &[(String, i64, String)],
) {
    assert_eq!(
        before.iter().map(|row| row.0).collect::<Vec<_>>(),
        after.iter().map(|row| row.0).collect::<Vec<_>>(),
        "{entity}: the set of rows"
    );
    for (old, new) in before.iter().zip(after) {
        if mutated.contains(&old.0) {
            assert_ne!(old.1, new.1, "{entity} {}: rewritten", old.0);
            assert_eq!(
                strip_labels(&old.1, keys),
                strip_labels(&new.1, keys),
                "{entity} {}: labels only",
                old.0
            );
            assert!(
                payloads
                    .iter()
                    .any(|(kind, id, payload)| kind == entity && *id == old.0 && *payload == new.1),
                "{entity} {}: the mutation carries the stored row",
                old.0
            );
        } else {
            assert_eq!(old, new, "{entity} {}: not rewritten", old.0);
        }
    }
}

/// Run one fence call and check what it did to the vehicle's history.
fn run_step(store: &HubStore, vehicle: Uuid, call: impl FnOnce()) -> Outcome {
    let drives = raw_rows(store, vehicle, "materialised_drives");
    let charges = raw_rows(store, vehicle, "materialised_charges");
    let log = mutation_log(store, vehicle).len();
    let revision = export_revision(store, vehicle);
    let reads = relabel_page_reads();
    call();
    let mutations = mutation_log(store, vehicle)[log..].to_vec();
    let payloads = mutation_payloads(store, vehicle)[log..].to_vec();
    let numbered = |entity: &str| -> Vec<i64> {
        mutations
            .iter()
            .filter_map(|entry| entry.strip_prefix(&format!("{entity} ")))
            .map(|id| id.parse().expect("id"))
            .collect()
    };
    assert_labels_only(
        &drives,
        &raw_rows(store, vehicle, "materialised_drives"),
        "drive",
        &["start_geofence", "end_geofence"],
        &numbered("drive"),
        &payloads,
    );
    assert_labels_only(
        &charges,
        &raw_rows(store, vehicle, "materialised_charges"),
        "charge",
        &["geofence"],
        &numbered("charge"),
        &payloads,
    );
    // Drives come before charges, each by ascending id, and no row twice.
    let order: Vec<(u8, i64)> = mutations
        .iter()
        .map(|entry| {
            let (entity, id) = entry.split_once(' ').expect("entry");
            (u8::from(entity == "charge"), id.parse().expect("id"))
        })
        .collect();
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "fixed order: {mutations:?}"
    );
    Outcome {
        mutations,
        exported: export_revision(store, vehicle) > revision,
        page_reads: relabel_page_reads() - reads,
    }
}

/// The setup L0 of C8 section 8: collected drive 1 from (10.0, 20.0) to
/// (10.0004, 20.0), drive 2 from (10.0009, 20.0) to (10.0006, 20.0) and charge
/// 1 at (10.0001, 20.0), every label and tariff None, no fences.
fn l0(store: &HubStore, key: &str) -> Uuid {
    let vehicle = c8_vehicle(store, key);
    seed_drive(
        store,
        vehicle,
        &drive_row(1, Some((10.0, 20.0)), Some((10.0004, 20.0)), None, None),
    );
    seed_drive(
        store,
        vehicle,
        &drive_row(2, Some((10.0009, 20.0)), Some((10.0006, 20.0)), None, None),
    );
    seed_charge(store, vehicle, &charge_row(1, Some((10.0001, 20.0)), None));
    vehicle
}

/// Drive 1 start and end, drive 2 start and end and charge 1, as stored.
fn l0_labels(store: &HubStore, vehicle: Uuid) -> Vec<Option<String>> {
    let (first_start, first_end) = drive_labels(store, vehicle, 1);
    let (second_start, second_end) = drive_labels(store, vehicle, 2);
    vec![
        first_start,
        first_end,
        second_start,
        second_end,
        charge_label(store, vehicle, 1),
    ]
}

fn owned(labels: [Option<&str>; 5]) -> Vec<Option<String>> {
    labels.map(|label| label.map(str::to_owned)).to_vec()
}

/// Steps C8 T16 to C8 T22 on one store, in the spec's order; `observe` sees
/// each step by name with its outcome.
fn run_l0_to_t22(store: &HubStore, vehicle: Uuid, observe: &mut dyn FnMut(&'static str, &Outcome)) {
    let mut step = |name: &'static str, call: &mut dyn FnMut()| {
        let outcome = run_step(store, vehicle, call);
        observe(name, &outcome);
    };
    // C8 T16: create.
    step("t16", &mut || {
        let created = save(store, vehicle, None, fence("Home", 10.0, 20.0)).expect("create");
        assert_eq!(created.id, H);
    });
    // C8 T17: grow.
    step("t17", &mut || {
        save(
            store,
            vehicle,
            Some(H),
            with_radius(fence("Home", 10.0, 20.0), 70.0),
        )
        .expect("grow");
    });
    // C8 T18: a nested fence at the same centre.
    step("t18", &mut || {
        let nested = save(
            store,
            vehicle,
            None,
            with_radius(fence("Garage", 10.0, 20.0), 20.0),
        )
        .expect("nested");
        assert_eq!(nested.id, H + 1);
    });
    // C8 T19: delete while referenced.
    step("t19", &mut || {
        store.delete_geofence(vehicle, H + 1).expect("delete");
    });
    // C8 T20: rename, radius omitted.
    step("t20", &mut || {
        save(store, vehicle, Some(H), fence("House", 10.0, 20.0)).expect("rename");
    });
    // C8 T21: a price only.
    step("t21", &mut || {
        save(
            store,
            vehicle,
            Some(H),
            with_cost(fence("House", 10.0, 20.0), 0.30),
        )
        .expect("price");
    });
    // C8 T22: the same call again is a no-op.
    step("t22", &mut || {
        save(
            store,
            vehicle,
            Some(H),
            with_cost(fence("House", 10.0, 20.0), 0.30),
        )
        .expect("no-op");
    });
}

#[test]
fn c8_t16_to_t22_changes() {
    let (_temporary, store) = c8_store();
    let vehicle = l0(&store, "t16");
    assert_eq!(l0_labels(&store, vehicle), owned([None; 5]));
    // Per step: the five labels afterwards, the mutations (M) and the export (X).
    let expected: [(&str, [Option<&str>; 5], &[&str], bool); 7] = [
        (
            "t16",
            [Some("Home"), Some("Home"), None, None, Some("Home")],
            &["drive 1", "charge 1"],
            true,
        ),
        (
            "t17",
            [Some("Home"), Some("Home"), None, Some("Home"), Some("Home")],
            &["drive 2"],
            true,
        ),
        (
            "t18",
            [
                Some("Garage"),
                Some("Home"),
                None,
                Some("Home"),
                Some("Garage"),
            ],
            &["drive 1", "charge 1"],
            true,
        ),
        (
            "t19",
            [Some("Home"), Some("Home"), None, Some("Home"), Some("Home")],
            &["drive 1", "charge 1"],
            true,
        ),
        (
            "t20",
            [
                Some("House"),
                Some("House"),
                None,
                Some("House"),
                Some("House"),
            ],
            &["drive 1", "drive 2", "charge 1"],
            true,
        ),
        (
            "t21",
            [
                Some("House"),
                Some("House"),
                None,
                Some("House"),
                Some("House"),
            ],
            &[],
            true,
        ),
        (
            "t22",
            [
                Some("House"),
                Some("House"),
                None,
                Some("House"),
                Some("House"),
            ],
            &[],
            false,
        ),
    ];
    let mut seen = 0_usize;
    run_l0_to_t22(&store, vehicle, &mut |name, outcome| {
        let (expected_name, labels, mutations, exported) = &expected[seen];
        seen += 1;
        assert_eq!(name, *expected_name);
        assert_eq!(outcome.mutations, *mutations, "{name}: M");
        assert_eq!(outcome.exported, *exported, "{name}: X");
        assert_eq!(l0_labels(&store, vehicle), owned(*labels), "{name}: labels");
        // A change in a tariff only, or none, reads no history page (C8 R53).
        if matches!(name, "t21" | "t22") {
            assert_eq!(outcome.page_reads, 0, "{name}: no page read");
        } else {
            assert!(outcome.page_reads > 0, "{name}: the history was read");
        }
        if name == "t19" {
            assert!(
                durable(&store, vehicle).tombstones.is_empty(),
                "a Hub id leaves no tombstone"
            );
        }
    });
    assert_eq!(seen, 7);
    // C8 T21: the price reached the fence and no charge field (C8 R27).
    let stored = store.geofences(vehicle).expect("fences");
    assert_eq!(stored.len(), 1);
    assert_eq!(
        (stored[0].billing_type, stored[0].cost_per_unit),
        (Some(GeofenceBillingType::PerKwh), Some(0.3))
    );
    let charge = stored_charge(&store, vehicle, 1);
    assert_eq!(
        (
            charge.billing_type,
            charge.cost_per_unit,
            charge.session_fee,
            charge.cost
        ),
        (None, None, None, None)
    );
    assert_eq!(geofence_entity_mutations(&store, vehicle), 0);
}

#[test]
fn c8_t23_to_t27_geometry() {
    let (_temporary, store) = c8_store();
    // C8 T23: rows without coordinates, after the sequence to C8 T22.
    let vehicle = l0(&store, "t23");
    run_l0_to_t22(&store, vehicle, &mut |_, _| {});
    seed_drive(
        &store,
        vehicle,
        &drive_row(3, None, None, Some("House"), Some("House")),
    );
    let renamed = run_step(&store, vehicle, || {
        save(&store, vehicle, Some(H), fence("Home", 10.0, 20.0)).expect("rename");
    });
    assert_eq!(
        renamed.mutations,
        ["drive 1", "drive 2", "drive 3", "charge 1"]
    );
    assert_eq!(
        drive_labels(&store, vehicle, 3),
        (Some("Home".to_owned()), Some("Home".to_owned())),
        "the rename follows the name on a row without a point"
    );
    assert_eq!(
        l0_labels(&store, vehicle),
        owned([Some("Home"), Some("Home"), None, Some("Home"), Some("Home")])
    );
    let stored = store.geofences(vehicle).expect("fences");
    assert_eq!(
        (stored[0].billing_type, stored[0].cost_per_unit),
        (Some(GeofenceBillingType::PerKwh), Some(0.3)),
        "the fence keeps its price"
    );
    assert_eq!(stored_charge(&store, vehicle, 1).cost_per_unit, None);
    let deleted = run_step(&store, vehicle, || {
        store.delete_geofence(vehicle, H).expect("delete");
    });
    assert_eq!(deleted.mutations, ["drive 1", "drive 2", "charge 1"]);
    assert_eq!(
        drive_labels(&store, vehicle, 3),
        (Some("Home".to_owned()), Some("Home".to_owned())),
        "no point, no evidence: the label stays"
    );
    assert_eq!(l0_labels(&store, vehicle), owned([None; 5]));

    // C8 T24: a name left outside its fence is cleared when the fence goes.
    let vehicle = c8_vehicle(&store, "t24");
    let work = save(
        &store,
        vehicle,
        None,
        with_radius(fence("Work", 10.5, 20.5), 100.0),
    )
    .expect("Work");
    assert_eq!(work.id, H);
    seed_drive(
        &store,
        vehicle,
        &drive_row(4, None, Some((10.1, 20.0)), None, Some("Work")),
    );
    let grown = run_step(&store, vehicle, || {
        save(
            &store,
            vehicle,
            Some(H),
            with_radius(fence("Work", 10.5, 20.5), 200.0),
        )
        .expect("grow");
    });
    assert_eq!(grown.mutations, Vec::<String>::new());
    assert_eq!(end_label(&store, vehicle, 4).as_deref(), Some("Work"));
    let gone = run_step(&store, vehicle, || {
        store.delete_geofence(vehicle, H).expect("delete");
    });
    assert_eq!(gone.mutations, ["drive 4"]);
    assert_eq!(end_label(&store, vehicle, 4), None);

    // C8 T25: a copied label stays unless the answer at its point changes.
    let vehicle = c8_vehicle(&store, "t25");
    let house = save(&store, vehicle, None, fence("House", 10.0, 20.0)).expect("House");
    assert_eq!(house.id, H);
    seed_drive(
        &store,
        vehicle,
        &drive_row(5, None, Some((10.0004, 20.0)), None, Some("Source Home")),
    );
    let shrunk = run_step(&store, vehicle, || {
        save(
            &store,
            vehicle,
            Some(H),
            with_radius(fence("House", 10.0, 20.0), 45.0),
        )
        .expect("45 m");
    });
    assert_eq!(shrunk.mutations, Vec::<String>::new());
    assert_eq!(
        end_label(&store, vehicle, 5).as_deref(),
        Some("Source Home")
    );
    let shrunk = run_step(&store, vehicle, || {
        save(
            &store,
            vehicle,
            Some(H),
            with_radius(fence("House", 10.0, 20.0), 40.0),
        )
        .expect("40 m");
    });
    assert_eq!(shrunk.mutations, ["drive 5"]);
    assert_eq!(end_label(&store, vehicle, 5), None);

    // C8 T26: the boundary is inclusive, through the relabel.
    let vehicle = c8_vehicle(&store, "t26");
    let metres = |latitude: f64, other: f64| {
        great_circle_distance_m(
            Wgs84Point::new(latitude, 20.0).expect("point"),
            Wgs84Point::new(other, 20.0).expect("point"),
        )
    };
    let edge_radius = metres(10.0, 10.0004);
    let near_radius = metres(11.0, 11.0004).next_down();
    imp(
        &store,
        vehicle,
        &[
            imported(1, "Edge", 10.5, 20.0, edge_radius),
            imported(2, "Near", 11.5, 20.0, near_radius),
        ],
    );
    seed_drive(
        &store,
        vehicle,
        &drive_row(6, None, Some((10.0004, 20.0)), None, None),
    );
    seed_drive(
        &store,
        vehicle,
        &drive_row(7, None, Some((11.0004, 20.0)), None, None),
    );
    let edge = run_step(&store, vehicle, || {
        save(&store, vehicle, Some(1), fence("Edge", 10.0, 20.0)).expect("Edge");
    });
    assert_eq!(edge.mutations, ["drive 6"]);
    assert_eq!(end_label(&store, vehicle, 6).as_deref(), Some("Edge"));
    let near = run_step(&store, vehicle, || {
        save(&store, vehicle, Some(2), fence("Near", 11.0, 20.0)).expect("Near");
    });
    assert_eq!(near.mutations, Vec::<String>::new());
    assert_eq!(end_label(&store, vehicle, 7), None, "one ulp outside");
    assert!(near.exported, "the edit itself was written");

    // C8 T27: the antimeridian and a pole.
    let vehicle = c8_vehicle(&store, "t27");
    seed_drive(
        &store,
        vehicle,
        &drive_row(8, None, Some((0.0, -179.9997)), None, None),
    );
    seed_drive(
        &store,
        vehicle,
        &drive_row(9, None, Some((89.9999, -135.0)), None, None),
    );
    let dateline = run_step(&store, vehicle, || {
        save(&store, vehicle, None, fence("Dateline", 0.0, 180.0)).expect("Dateline");
    });
    assert_eq!(dateline.mutations, ["drive 8"]);
    assert_eq!(end_label(&store, vehicle, 8).as_deref(), Some("Dateline"));
    assert_eq!(end_label(&store, vehicle, 9), None);
    let pole = run_step(&store, vehicle, || {
        save(
            &store,
            vehicle,
            None,
            with_radius(fence("Pole", 90.0, 45.0), 20.0),
        )
        .expect("Pole");
    });
    assert_eq!(pole.mutations, ["drive 9"]);
    assert_eq!(end_label(&store, vehicle, 9).as_deref(), Some("Pole"));

    // A centre that moves in longitude alone takes a drive out of its fence:
    // the end at about 43.8 m is inside 50 m before and about 98.6 m away after.
    let vehicle = c8_vehicle(&store, "t27-longitude");
    save(
        &store,
        vehicle,
        None,
        with_radius(fence("Home", 10.0, 20.0), 50.0),
    )
    .expect("Home");
    seed_drive(
        &store,
        vehicle,
        &drive_row(18, None, Some((10.0, 20.0004)), None, Some("Home")),
    );
    let moved = run_step(&store, vehicle, || {
        save(
            &store,
            vehicle,
            Some(H),
            with_radius(fence("Home", 10.0, 19.9995), 50.0),
        )
        .expect("moved in longitude");
    });
    assert_eq!(moved.mutations, ["drive 18"]);
    assert_eq!(end_label(&store, vehicle, 18), None);
    // The same move through an import takes the drive out as well.
    let vehicle = c8_vehicle(&store, "t27-longitude-import");
    imp(&store, vehicle, &[imported(1, "Home", 10.0, 20.0, 50.0)]);
    seed_drive(
        &store,
        vehicle,
        &drive_row(18, None, Some((10.0, 20.0004)), None, Some("Home")),
    );
    let report = imp(&store, vehicle, &[imported(1, "Home", 10.0, 19.9995, 50.0)]);
    assert_eq!(report.updated, [1]);
    assert_eq!(
        report.relabel,
        RelabelCounts {
            drives: 1,
            charges: 0
        }
    );
    assert_eq!(end_label(&store, vehicle, 18), None);
}

/// The setup of C8 T38 and C8 T39: two imported fences named "Work", collected
/// drives 14 and 15 ending in them, and an imported drive outside both and one
/// without a point, all labelled "Work".
fn shared_name_vehicle(store: &HubStore, key: &str) -> Uuid {
    let vehicle = c8_vehicle(store, key);
    imp(
        store,
        vehicle,
        &[
            imported(1, "Work", 10.0, 20.0, 50.0),
            imported(2, "Work", 11.0, 20.0, 50.0),
        ],
    );
    for (id, point) in [(14, (10.0004, 20.0)), (15, (11.0004, 20.0))] {
        seed_drive(
            store,
            vehicle,
            &drive_row(id, None, Some(point), None, Some("Work")),
        );
    }
    seed_drive(
        store,
        vehicle,
        &drive_row(16, None, Some((10.5, 20.0)), None, Some("Work")),
    );
    seed_drive(
        store,
        vehicle,
        &drive_row(17, None, None, Some("Work"), Some("Work")),
    );
    vehicle
}

#[test]
fn c8_t37_to_t39_successors() {
    let (_temporary, store) = c8_store();
    // C8 T37: a rename follows the label.
    let vehicle = c8_vehicle(&store, "t37");
    let work = save(
        &store,
        vehicle,
        None,
        with_radius(fence("Work", 10.5, 20.5), 100.0),
    )
    .expect("Work");
    assert_eq!(work.id, H);
    seed_drive(
        &store,
        vehicle,
        &drive_row(10, None, Some((10.1, 20.0)), None, Some("Work")),
    );
    seed_drive(
        &store,
        vehicle,
        &drive_row(11, None, None, Some("Work"), Some("Work")),
    );
    seed_drive(
        &store,
        vehicle,
        &drive_row(12, None, Some((10.5, 20.5)), None, Some("Work")),
    );
    seed_drive(
        &store,
        vehicle,
        &drive_row(13, None, Some((10.5, 20.5)), None, Some("Source Work")),
    );
    let outcome = run_step(&store, vehicle, || {
        save(&store, vehicle, Some(H), fence("Office", 10.5, 20.5)).expect("rename");
    });
    assert_eq!(outcome.mutations, ["drive 10", "drive 11", "drive 12"]);
    assert_eq!(end_label(&store, vehicle, 10).as_deref(), Some("Office"));
    assert_eq!(
        drive_labels(&store, vehicle, 11),
        (Some("Office".to_owned()), Some("Office".to_owned()))
    );
    assert_eq!(end_label(&store, vehicle, 12).as_deref(), Some("Office"));
    assert_eq!(
        end_label(&store, vehicle, 13).as_deref(),
        Some("Source Work"),
        "a copied label that is not the fence's name is kept"
    );

    // C8 T38: a rename with a shared name.
    let vehicle = shared_name_vehicle(&store, "t38");
    let outcome = run_step(&store, vehicle, || {
        save(&store, vehicle, Some(1), fence("Office", 10.0, 20.0)).expect("rename one");
    });
    assert_eq!(outcome.mutations, ["drive 14"]);
    assert_eq!(end_label(&store, vehicle, 14).as_deref(), Some("Office"));
    for id in [15, 16] {
        assert_eq!(end_label(&store, vehicle, id).as_deref(), Some("Work"));
    }
    assert_eq!(
        drive_labels(&store, vehicle, 17),
        (Some("Work".to_owned()), Some("Work".to_owned())),
        "the name no longer says which fence it meant"
    );

    // C8 T39: a delete with a shared name.
    let vehicle = shared_name_vehicle(&store, "t39");
    let outcome = run_step(&store, vehicle, || {
        store.delete_geofence(vehicle, 2).expect("delete two");
    });
    assert_eq!(outcome.mutations, ["drive 15"]);
    assert_eq!(end_label(&store, vehicle, 15), None);
    for id in [14, 16] {
        assert_eq!(end_label(&store, vehicle, id).as_deref(), Some("Work"));
    }
    assert_eq!(
        drive_labels(&store, vehicle, 17),
        (Some("Work".to_owned()), Some("Work".to_owned())),
        "Work still names fence 1"
    );
    assert_eq!(durable(&store, vehicle).tombstones, [["2"]]);

    // C8 table 5.3 through the pure rule: every row of the table.
    let fence_at =
        |id: i64, name: &str, latitude: f64, radius_m: f64| crate::lifecycle::GeofenceFence {
            source_geofence_id: id,
            name: name.to_owned(),
            latitude,
            longitude: 20.0,
            radius_m,
            billing_type: None,
            cost_per_unit: None,
            session_fee: None,
        };
    let inside = Some((10.0, 20.0));
    let outside = Some((12.0, 20.0));
    // The containing fence changes: the new answer, whatever the label.
    let before = [fence_at(1, "A", 10.0, 50.0)];
    let after = [fence_at(1, "A", 10.0, 50.0), fence_at(2, "B", 10.0, 20.0)];
    let rule = RelabelRule::new(&before, &after);
    for stored in [None, Some("A"), Some("Z")] {
        assert_eq!(rule.label(inside, stored).as_deref(), Some("B"));
    }
    // The label equals the earlier answer: the new answer (the rename).
    let after = [fence_at(1, "A2", 10.0, 50.0)];
    let rule = RelabelRule::new(&before, &after);
    assert_eq!(rule.label(inside, Some("A")).as_deref(), Some("A2"));
    // Not the earlier answer: none stays none, a renamed name follows, a
    // removed name takes the answer at the point, an ambiguous one stays.
    assert_eq!(rule.label(inside, None), None);
    let before = [fence_at(1, "A", 10.0, 50.0), fence_at(2, "N", 11.0, 50.0)];
    let after = [fence_at(1, "A", 10.0, 50.0), fence_at(2, "N2", 11.0, 50.0)];
    let rule = RelabelRule::new(&before, &after);
    assert_eq!(rule.label(inside, Some("N")).as_deref(), Some("N2"));
    assert_eq!(rule.label(inside, None), None);
    let after = [fence_at(1, "A", 10.0, 50.0)];
    let rule = RelabelRule::new(&before, &after);
    assert_eq!(rule.label(inside, Some("N")).as_deref(), Some("A"));
    let before = [
        fence_at(1, "A", 10.0, 50.0),
        fence_at(2, "N", 11.0, 50.0),
        fence_at(3, "N", 13.0, 50.0),
    ];
    let after = [
        fence_at(1, "A", 10.0, 50.0),
        fence_at(2, "N", 11.0, 50.0),
        fence_at(3, "M", 13.0, 50.0),
    ];
    let rule = RelabelRule::new(&before, &after);
    assert_eq!(rule.label(inside, Some("N")).as_deref(), Some("N"));
    // A label no fence carried is its own successor.
    assert_eq!(
        rule.label(inside, Some("Copied")).as_deref(),
        Some("Copied")
    );
    // No usable point: only the successor applies; removed and ambiguous keep.
    assert_eq!(rule.label(None, None), None);
    assert_eq!(rule.label(None, Some("N")).as_deref(), Some("N"));
    assert_eq!(
        rule.label(Some((91.0, 20.0)), Some("N")).as_deref(),
        Some("N")
    );
    assert_eq!(rule.label(Some((f64::NAN, 20.0)), None), None);
    let before = [fence_at(1, "A", 10.0, 50.0), fence_at(2, "N", 11.0, 50.0)];
    let after = [fence_at(1, "A", 10.0, 50.0), fence_at(2, "N2", 11.0, 50.0)];
    let rule = RelabelRule::new(&before, &after);
    assert_eq!(rule.label(None, Some("N")).as_deref(), Some("N2"));
    let after = [fence_at(1, "A", 10.0, 50.0)];
    let rule = RelabelRule::new(&before, &after);
    assert_eq!(rule.label(None, Some("N")).as_deref(), Some("N"));
    // An invalid stored point is no point: a removed name stays rather than
    // taking the answer at the point (C8 table 5.3).
    assert_eq!(
        rule.label(Some((91.0, 20.0)), Some("N")).as_deref(),
        Some("N")
    );
    assert_eq!(
        rule.label(Some((10.0, 181.0)), Some("N")).as_deref(),
        Some("N")
    );
    // The containing fence is compared by id, not by name: a nested fence of
    // the same name still changes the answer, so a copied label takes it.
    let before = [fence_at(1, "A", 10.0, 50.0)];
    let after = [fence_at(1, "A", 10.0, 50.0), fence_at(2, "A", 10.0, 20.0)];
    let rule = RelabelRule::new(&before, &after);
    assert_eq!(rule.label(inside, Some("Z")).as_deref(), Some("A"));
    assert_eq!(rule.label(inside, None).as_deref(), Some("A"));
    // A point that no fence contains, before or after, with a removed name.
    let before = [fence_at(1, "A", 10.0, 50.0), fence_at(2, "N", 11.0, 50.0)];
    let after = [fence_at(1, "A", 10.0, 50.0)];
    let rule = RelabelRule::new(&before, &after);
    assert_eq!(rule.label(outside, Some("N")), None);
    assert_eq!(
        rule.label(outside, Some("Copied")).as_deref(),
        Some("Copied")
    );
}

#[test]
fn c8_t28_rollback() {
    let (_temporary, store) = c8_store();
    let vehicle = l0(&store, "t28");
    let before = durable(&store, vehicle);
    let second = store.open().expect("second connection");
    let trigger = "CREATE TRIGGER t_fail BEFORE INSERT ON sync_mutations
                   WHEN NEW.entity = 'charge'
                   BEGIN SELECT RAISE(ABORT, 'injected'); END";
    second.execute_batch(trigger).expect("trigger");
    // The save writes the fence, the id mark and the first drive mutation
    // before the charge mutation fails: nothing stays.
    let error = save(&store, vehicle, None, fence("Home", 10.0, 20.0)).expect_err("injected");
    assert!(error.to_string().contains("injected"), "{error}");
    assert_eq!(durable(&store, vehicle), before);
    assert!(store.geofences(vehicle).expect("fences").is_empty());
    assert_eq!(l0_labels(&store, vehicle), owned([None; 5]));
    second.execute_batch("DROP TRIGGER t_fail").expect("drop");
    let created = save(&store, vehicle, None, fence("Home", 10.0, 20.0)).expect("repeat");
    assert_eq!(created.id, H, "the failed call left no id mark");
    assert_eq!(
        l0_labels(&store, vehicle),
        owned([Some("Home"), Some("Home"), None, None, Some("Home")])
    );
    assert_eq!(mutation_log(&store, vehicle), ["drive 1", "charge 1"]);
    // The delete and both imports roll back as completely.
    let before = durable(&store, vehicle);
    second.execute_batch(trigger).expect("trigger again");
    assert!(store.delete_geofence(vehicle, H).is_err());
    assert_eq!(durable(&store, vehicle), before, "delete");
    assert!(
        store
            .upsert_geofences(vehicle, &[imported(1, "Gate", 10.0, 20.0, 50.0)])
            .is_err()
    );
    assert_eq!(durable(&store, vehicle), before, "store import");
    let mut connection = store.open().expect("connection");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin");
    assert!(
        upsert_geofences_in_transaction(
            &transaction,
            vehicle,
            &[imported(1, "Gate", 10.0, 20.0, 50.0)]
        )
        .is_err()
    );
    drop(transaction);
    assert_eq!(durable(&store, vehicle), before, "transaction import");
}

#[test]
fn c8_t30_determinism() {
    let (_first_directory, first) = c8_store();
    let (_second_directory, second) = c8_store();
    let first_vehicle = l0(&first, "t30");
    let second_vehicle = l0(&second, "t30");
    let mut first_outcomes = Vec::new();
    let mut second_outcomes = Vec::new();
    run_l0_to_t22(&first, first_vehicle, &mut |_, outcome| {
        first_outcomes.push(outcome.clone());
    });
    run_l0_to_t22(&second, second_vehicle, &mut |_, outcome| {
        second_outcomes.push(outcome.clone());
    });
    assert_eq!(first_outcomes, second_outcomes);
    let left = durable(&first, first_vehicle);
    let right = durable(&second, second_vehicle);
    assert_eq!(
        left.mutations.len(),
        10,
        "the sequence recorded its mutations"
    );
    assert_eq!(left.mutations, right.mutations, "sync_mutations");
    assert_eq!(left.drives, right.drives, "drive rows");
    assert_eq!(left.charges, right.charges, "charge rows");
    assert_eq!(left.fences, right.fences, "fences");
    assert_eq!(left.marks, right.marks, "id marks");
    assert_eq!(left, right);
    // The revisions run without a gap, in the one fixed order.
    let revisions: Vec<i64> = left
        .mutations
        .iter()
        .map(|row| row[0].parse().expect("revision"))
        .collect();
    assert_eq!(revisions, (1..=10).collect::<Vec<i64>>());

    // Many rows, across page boundaries: one create labels every row, drives by
    // ascending id and then charges by ascending id, the same on both stores
    // (C8 R24, C8 R44).
    let wide = |store: &HubStore, key: &str| {
        let vehicle = c8_vehicle(store, key);
        for id in 1..=300 {
            seed_drive(
                store,
                vehicle,
                &drive_row(id, Some((10.0, 20.0)), Some((10.0004, 20.0)), None, None),
            );
            seed_charge(store, vehicle, &charge_row(id, Some((10.0001, 20.0)), None));
        }
        save(store, vehicle, None, fence("Home", 10.0, 20.0)).expect("create");
        durable(store, vehicle)
    };
    let left = wide(&first, "t30-wide");
    let right = wide(&second, "t30-wide");
    assert_eq!(left, right);
    let expected: Vec<String> = (1..=300)
        .map(|id| format!("drive {id}"))
        .chain((1..=300).map(|id| format!("charge {id}")))
        .collect();
    let recorded = |state: &Durable| -> Vec<String> {
        state
            .mutations
            .iter()
            .map(|row| format!("{} {}", row[1].trim_matches('\''), row[2]))
            .collect()
    };
    assert_eq!(recorded(&left), expected);
}

#[test]
fn c8_t40_vehicle_separation() {
    let (_temporary, store) = c8_store();
    let first = c8_vehicle(&store, "t40-first");
    let second = c8_vehicle(&store, "t40-second");
    for vehicle in [first, second] {
        seed_drive(
            &store,
            vehicle,
            &drive_row(1, Some((12.0, 20.0)), Some((10.0004, 20.0)), None, None),
        );
    }
    let untouched = durable(&store, second);
    let created = save(&store, first, None, fence("Home", 10.0, 20.0)).expect("create");
    assert_eq!(created.id, H);
    assert_eq!(
        drive_labels(&store, first, 1),
        (None, Some("Home".to_owned()))
    );
    assert_eq!(mutation_log(&store, first), ["drive 1"]);
    assert_eq!(drive_labels(&store, second, 1), (None, None));
    assert_eq!(
        durable(&store, second),
        untouched,
        "rows, mutations, export"
    );
    assert!(mutation_log(&store, second).is_empty());
    assert_eq!(export_revision(&store, second), 0);
    // A delete and an import on one vehicle leave the other alone too.
    store.delete_geofence(first, H).expect("delete");
    imp(&store, first, &[imported(1, "Gate", 10.0, 20.0, 50.0)]);
    assert_eq!(durable(&store, second), untouched);
    // The second vehicle's ids are its own: its first create gets the first id.
    let created = save(&store, second, None, fence("Home", 10.0, 20.0)).expect("create");
    assert_eq!(created.id, H);
    assert_eq!(
        drive_labels(&store, second, 1),
        (None, Some("Home".to_owned()))
    );
}

#[test]
fn c8_t29_open_drive() {
    use crate::lifecycle::drive_tests::{MIN, T0, driving, parked};
    let (_temporary, store) = c8_store();
    let (source, vehicle) = test_registered_vehicle(&store);
    let key = vehicle.vehicle_id;
    // A drive opens at (10.0, 20.0) with no fence.
    let samples = [
        driving(1, T0, 10.0, Some(1000.0)),
        driving(2, T0 + MIN, 10.0002, Some(1000.2)),
        driving(3, T0 + 2 * MIN, 10.0003, Some(1000.4)),
        parked(4, T0 + 3 * MIN, 10.0004, Some(1000.5)),
    ];
    feed_owner_samples(&store, &source, &vehicle, &samples[..2]);
    assert!(
        raw_rows(&store, key, "materialised_drives").is_empty(),
        "the drive is still open"
    );
    let open_state = |store: &HubStore| {
        let connection = store.open().expect("connection");
        let state: Vec<String> = connection
            .prepare(
                "SELECT quote(open_session_json) FROM vehicle_lifecycle_state
                  WHERE vehicle_id = ?1",
            )
            .expect("prepare")
            .query_map(params![key.to_string()], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        let rows: Vec<String> = connection
            .prepare(
                "SELECT quote(row_json) FROM lifecycle_open_rows
                  WHERE vehicle_id = ?1 ORDER BY domain, source_row_id",
            )
            .expect("prepare")
            .query_map(params![key.to_string()], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        (state, rows)
    };
    let open_before = open_state(&store);
    assert!(!open_before.0.is_empty());
    let log = mutation_log(&store, key);
    // Create "Home" (10.0, 20.0) r 50 while the drive is open: the relabel
    // neither reads nor writes the open session and records no mutation.
    let outcome = run_step(&store, key, || {
        save(&store, key, None, fence("Home", 10.0, 20.0)).expect("create");
    });
    assert_eq!(outcome.mutations, Vec::<String>::new());
    assert_eq!(mutation_log(&store, key), log);
    assert_eq!(open_state(&store), open_before);
    // The drive closes at (10.0004, 20.0): labelled at its close with the set
    // current then, start and end alike.
    feed_owner_samples(&store, &source, &vehicle, &samples[2..]);
    let drives = raw_rows(&store, key, "materialised_drives");
    assert_eq!(drives.len(), 1);
    assert_eq!(
        drive_labels(&store, key, 1),
        (Some("Home".to_owned()), Some("Home".to_owned()))
    );
    let closed: Vec<String> = mutation_log(&store, key)[log.len()..]
        .iter()
        .filter(|entry| entry.starts_with("drive "))
        .cloned()
        .collect();
    assert_eq!(closed, ["drive 1"], "only the close wrote the row");
}

#[test]
fn c8_t29b_open_charge() {
    let mut harness = CostHarness::new();
    let key = harness.vehicle.vehicle_id;
    // A charge opens at (10.0001, 20.0) with no fence.
    let first = cost_ob(1, COST_T0, "Charging", cost_a7(Some(0.00)));
    let samples = [
        cost_at(first, 10.0001, 20.0),
        cost_ob(2, COST_T0 + 10 * COST_MIN, "Charging", cost_a7(Some(1.08))),
        cost_ob(3, COST_T0 + 20 * COST_MIN, "Charging", cost_a7(Some(2.16))),
        cost_ob(
            4,
            COST_T0 + 3 * COST_HOUR,
            "Complete",
            serde_json::json!({"charge_energy_added": 20.00, "charger_power": 0,
                               "charger_voltage": 240, "charger_actual_current": 0}),
        ),
    ];
    harness.feed_all(&samples[..3]);
    assert!(harness.charges().is_empty(), "the charge is still open");
    let log = mutation_log(&harness.store, key);
    // Create "Home" (10.0, 20.0) r 50 with cost 0.30 while the charge is open.
    let outcome = run_step(&harness.store, key, || {
        save(
            &harness.store,
            key,
            None,
            with_cost(fence("Home", 10.0, 20.0), 0.30),
        )
        .expect("create");
    });
    assert_eq!(outcome.mutations, Vec::<String>::new());
    assert_eq!(mutation_log(&harness.store, key), log);
    assert!(harness.charges().is_empty());
    // The charge closes: the label (C1 R81) and the tariff copy (C2 R46 step 2)
    // come from the set current at close.
    harness.feed(&samples[3]);
    let charge = harness.charges().pop().expect("the closed charge");
    assert_eq!(charge.geofence.as_deref(), Some("Home"));
    assert_eq!(charge.billing_type, Some(GeofenceBillingType::PerKwh));
    assert_eq!(charge.cost_per_unit, Some(0.30));
    assert_eq!(
        harness.charge_mutations(charge.id),
        1,
        "only the close wrote the row"
    );
}

#[test]
fn c8_t31_current_summary() {
    let (_temporary, store) = c8_store();
    let vehicle = l0(&store, "t31");
    let name_at = |store: &HubStore| {
        store
            .geofence_name_at(vehicle, 10.0004, 20.0)
            .expect("current name")
    };
    assert_eq!(name_at(&store), None);
    run_l0_to_t22(&store, vehicle, &mut |step, _| match step {
        "t16" => assert_eq!(name_at(&store).as_deref(), Some("Home")),
        "t17" | "t18" | "t19" => assert_eq!(name_at(&store).as_deref(), Some("Home")),
        "t20" | "t21" | "t22" => assert_eq!(name_at(&store).as_deref(), Some("House")),
        other => panic!("unexpected step {other}"),
    });
    store.delete_geofence(vehicle, H).expect("delete");
    assert_eq!(name_at(&store), None);
    // The summary is computed at read time: no stored row changed with it.
    assert_eq!(geofence_entity_mutations(&store, vehicle), 0);
}

#[test]
fn c8_t32_no_upgrade_relabel() {
    let predecessor = GEOFENCE_STEP_SCHEMA_VERSION - 1;
    let (temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t32");
    {
        let connection = store.open().expect("connection");
        connection
            .execute_batch(&format!(
                "BEGIN IMMEDIATE; {REWIND_V76_SQL} PRAGMA user_version = {predecessor}; COMMIT;"
            ))
            .expect("rewind");
        assert_eq!(schema_version(&connection).unwrap(), predecessor);
        assert!(
            predecessor_radius_bound(&connection) >= 100.0,
            "the predecessor shape holds a 100 m fence"
        );
        // "Big" and "Small" as stored data of the predecessor shape.
        for (id, name, latitude, radius) in [(1, "Big", 10.0, 100.0), (2, "Small", 10.0004, 20.0)] {
            connection
                .execute(
                    "INSERT INTO geofences(vehicle_id, source_geofence_id, name, latitude,
                        longitude, radius_m) VALUES (?1, ?2, ?3, ?4, 20.0, ?5)",
                    params![vehicle.to_string(), id, name, latitude, radius],
                )
                .expect("fence");
        }
    }
    // Drive 1 carries a label no fence has; drive 2 carries the answer of the
    // earlier rule.
    seed_drive(
        &store,
        vehicle,
        &drive_row(1, None, Some((10.0004, 20.0)), None, Some("Old")),
    );
    seed_drive(
        &store,
        vehicle,
        &drive_row(2, None, Some((10.0004, 20.0)), None, Some("Big")),
    );
    let rows = raw_rows(&store, vehicle, "materialised_drives");
    let counts = |store: &HubStore| {
        let connection = store.open().expect("connection");
        (
            count(&connection, "sync_mutations"),
            count(&connection, "export_outbox"),
        )
    };
    let recorded = counts(&store);
    let reads = relabel_page_reads();
    drop(store);
    let migrated = HubStore::initialize(temporary.path()).expect("migrated");
    let connection = migrated.open().expect("connection");
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    drop(connection);
    // The upgrade rewrote no label and recorded nothing (C8 R33).
    assert_eq!(raw_rows(&migrated, vehicle, "materialised_drives"), rows);
    assert_eq!(counts(&migrated), recorded);
    assert_eq!(recorded, (0, 0));
    assert_eq!(relabel_page_reads(), reads, "the upgrade read no page");
    // A later change relabels by the rules: the name follows its fence (rule 3),
    // the label no fence had stays.
    let outcome = run_step(&migrated, vehicle, || {
        save(&migrated, vehicle, Some(1), fence("Large", 10.0, 20.0)).expect("rename");
    });
    assert_eq!(outcome.mutations, ["drive 2"]);
    assert_eq!(end_label(&migrated, vehicle, 2).as_deref(), Some("Large"));
    assert_eq!(end_label(&migrated, vehicle, 1).as_deref(), Some("Old"));
}

/// The stored half of C8 T46 (C8 R48): the relabel reaches both served items
/// under the current keys, the tariff copy stays at its close-time value after
/// a later price edit, and stored rows and sync payloads hold the stored names
/// and none of the neutral keys.
///
/// Carried to C6 Part C (C6-C1): under C8 section 11 the neutral served-key
/// half of C8 T46 is written by whichever of C8 Part L and C6 Part C lands second,
/// and C6 Part C lands second. Through the drive and charge routes, after the
/// rename, `start_geofence_name`, `end_geofence_name`, `geofence_name`,
/// `tariff_type`, `tariff_rate` and `tariff_session_fee` each equal their alias
/// (`start_geofence`, `end_geofence`, `geofence`, `billing_type`,
/// `cost_per_unit`, `session_fee`), and `tariff_rate` and `cost_per_unit` stay
/// 0.3 after the price edit to 0.40. This test does not claim that half.
#[test]
fn c8_t46_served_names_tariff_snapshot() {
    use crate::api::public_query::{PublicCharge, PublicDrive};
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t46");
    let home = Fence {
        billing_type: Some(GeofenceBillingType::PerKwh),
        session_fee: Some(1.00),
        ..with_cost(fence("Home", 10.0, 20.0), 0.30)
    };
    let created = save(&store, vehicle, None, home).expect("create");
    assert_eq!(created.id, H);
    seed_drive(
        &store,
        vehicle,
        &drive_row(
            1,
            Some((10.0, 20.0)),
            Some((10.0, 20.0)),
            Some("Home"),
            Some("Home"),
        ),
    );
    let tariff_copy = ProjectionCharge {
        billing_type: Some(GeofenceBillingType::PerKwh),
        cost_per_unit: Some(0.3),
        session_fee: Some(1.0),
        ..charge_row(1, Some((10.0, 20.0)), Some("Home"))
    };
    seed_charge(&store, vehicle, &tariff_copy);
    let served = |store: &HubStore| {
        let drive = store
            .materialised_drive_for_vehicle(vehicle, 1)
            .expect("drive read")
            .expect("drive");
        let charge = store
            .materialised_charge_for_vehicle(vehicle, 1)
            .expect("charge read")
            .expect("charge");
        (
            serde_json::to_value(PublicDrive::from_projection(vehicle, drive)).expect("drive"),
            serde_json::to_value(PublicCharge::from_projection(vehicle, charge).expect("closed"))
                .expect("charge"),
        )
    };
    // The rename relabels both rows.
    let outcome = run_step(&store, vehicle, || {
        save(&store, vehicle, Some(H), fence("Garage", 10.0, 20.0)).expect("rename");
    });
    assert_eq!(outcome.mutations, ["drive 1", "charge 1"]);
    let (drive, charge) = served(&store);
    for key in ["start_geofence", "end_geofence"] {
        assert_eq!(drive[key], "Garage", "served drive {key}");
    }
    assert_eq!(charge["geofence"], "Garage");
    // The tariff copy is the one made at close (C8 R27): the rename kept it.
    assert_eq!(charge["billing_type"], "per_kwh");
    assert_eq!(charge["cost_per_unit"], 0.3);
    assert_eq!(charge["session_fee"], 1.0);
    // A later price on the fence never reaches the charge.
    let outcome = run_step(&store, vehicle, || {
        save(
            &store,
            vehicle,
            Some(H),
            with_cost(fence("Garage", 10.0, 20.0), 0.40),
        )
        .expect("price");
    });
    assert_eq!(outcome.mutations, Vec::<String>::new());
    let (_, charge) = served(&store);
    assert_eq!(charge["cost_per_unit"], 0.3);
    assert_eq!(charge["billing_type"], "per_kwh");
    assert_eq!(charge["session_fee"], 1.0);
    assert_eq!(
        store.geofences(vehicle).expect("fences")[0].cost_per_unit,
        Some(0.4)
    );
    // The stored rows and the mutation payloads hold the stored names and none
    // of the neutral served keys (C8 R48).
    let neutral = [
        "start_geofence_name",
        "end_geofence_name",
        "geofence_name",
        "tariff_type",
        "tariff_rate",
        "tariff_session_fee",
    ];
    let stored_drive: serde_json::Value =
        serde_json::from_str(&raw_rows(&store, vehicle, "materialised_drives")[0].1)
            .expect("drive json");
    let stored_charge: serde_json::Value =
        serde_json::from_str(&raw_rows(&store, vehicle, "materialised_charges")[0].1)
            .expect("charge json");
    assert_eq!(stored_drive["start_geofence"], "Garage");
    assert_eq!(stored_drive["end_geofence"], "Garage");
    assert_eq!(stored_charge["geofence"], "Garage");
    assert_eq!(stored_charge["billing_type"], "per_kwh");
    assert_eq!(stored_charge["cost_per_unit"], 0.3);
    assert_eq!(stored_charge["session_fee"], 1.0);
    let payloads = mutation_payloads(&store, vehicle);
    assert_eq!(payloads.len(), 2);
    for (entity, _, payload) in &payloads {
        let value: serde_json::Value = serde_json::from_str(payload).expect("payload");
        for key in neutral {
            assert!(value.get(key).is_none(), "{entity} payload holds {key}");
        }
        let stored = if entity == "drive" {
            &stored_drive
        } else {
            &stored_charge
        };
        assert_eq!(&value, stored, "{entity} payload equals the stored row");
    }
    for key in neutral {
        assert!(stored_drive.get(key).is_none() && stored_charge.get(key).is_none());
    }
}

#[test]
fn c8_t47_tariff_no_scan() {
    let (_temporary, store) = c8_store();
    let vehicle = c8_vehicle(&store, "t47");
    let priced = |cost: f64| Fence {
        billing_type: Some(GeofenceBillingType::PerKwh),
        cost_per_unit: Some(cost),
        ..imported(1, "Home", 10.0, 20.0, 50.0)
    };
    assert_eq!(imp(&store, vehicle, &[priced(0.30)]).inserted, [1]);
    seed_drive(
        &store,
        vehicle,
        &drive_row(1, None, Some((10.0004, 20.0)), None, Some("Home")),
    );
    seed_charge(
        &store,
        vehicle,
        &charge_row(1, Some((10.0004, 20.0)), Some("Home")),
    );
    let rows_and_log = |store: &HubStore| {
        (
            raw_rows(store, vehicle, "materialised_drives"),
            raw_rows(store, vehicle, "materialised_charges"),
            mutation_payloads(store, vehicle),
        )
    };
    let history = rows_and_log(&store);
    // (a) An import that changes only the price.
    let reads = relabel_page_reads();
    let revision = export_revision(&store, vehicle);
    let report = imp(&store, vehicle, &[priced(0.35)]);
    assert_eq!(report.updated, [1]);
    assert_eq!(report.relabel, RelabelCounts::default());
    assert_eq!(
        store.geofences(vehicle).expect("fences")[0].cost_per_unit,
        Some(0.35)
    );
    assert_eq!(relabel_page_reads(), reads, "no relabel page was read");
    assert_eq!(rows_and_log(&store), history);
    assert_eq!(
        export_revision(&store, vehicle),
        revision + 1,
        "marked once"
    );
    // (b) A Hub edit that changes only the price.
    let reads = relabel_page_reads();
    let revision = export_revision(&store, vehicle);
    let saved = save(
        &store,
        vehicle,
        Some(1),
        with_radius(with_cost(fence("Home", 10.0, 20.0), 0.40), 50.0),
    )
    .expect("price edit");
    assert_eq!(saved.cost_per_unit, Some(0.4));
    assert_eq!(saved.billing_type, Some(GeofenceBillingType::PerKwh));
    assert_eq!(relabel_page_reads(), reads, "no relabel page was read");
    assert_eq!(rows_and_log(&store), history);
    assert_eq!(
        export_revision(&store, vehicle),
        revision + 1,
        "marked once"
    );
    // (c) Control: a centre that moves is scanned, although no label changes.
    let reads = relabel_page_reads();
    save(
        &store,
        vehicle,
        Some(1),
        with_radius(with_cost(fence("Home", 10.0001, 20.0), 0.40), 50.0),
    )
    .expect("moved");
    assert!(relabel_page_reads() > reads, "the history was scanned");
    assert_eq!(rows_and_log(&store), history, "and no label changed");
    // A centre that moves in longitude alone is a change as well (C8 R53).
    let reads = relabel_page_reads();
    save(
        &store,
        vehicle,
        Some(1),
        with_radius(with_cost(fence("Home", 10.0001, 20.0001), 0.40), 50.0),
    )
    .expect("moved in longitude");
    assert!(
        relabel_page_reads() > reads,
        "the longitude move was scanned"
    );
    assert_eq!(rows_and_log(&store), history, "and no label changed");
    // An import that moves the centre is scanned too, although no label changes.
    let other = c8_vehicle(&store, "t47-import");
    imp(&store, other, &[priced(0.30)]);
    seed_drive(
        &store,
        other,
        &drive_row(1, None, Some((10.0004, 20.0)), None, Some("Home")),
    );
    let reads = relabel_page_reads();
    let report = imp(
        &store,
        other,
        &[Fence {
            latitude: Some(10.0001),
            ..priced(0.30)
        }],
    );
    assert_eq!(report.updated, [1]);
    assert_eq!(report.relabel, RelabelCounts::default());
    assert!(relabel_page_reads() > reads, "the history was scanned");
    // An import that grows the fence over a charge relabels that charge and
    // reports it in `charges`, not in `drives` (C8 R16, C8 R26): charge 2 at
    // about 89 m from the moved centre is outside 50 m and inside 120 m.
    seed_charge(&store, other, &charge_row(2, Some((10.0009, 20.0)), None));
    let history = (
        raw_rows(&store, other, "materialised_drives"),
        mutation_payloads(&store, other),
    );
    let revision = export_revision(&store, other);
    let report = imp(
        &store,
        other,
        &[Fence {
            latitude: Some(10.0001),
            radius_m: Some(120.0),
            ..priced(0.30)
        }],
    );
    assert_eq!(report.updated, [1]);
    assert_eq!(
        report.relabel,
        RelabelCounts {
            drives: 0,
            charges: 1
        }
    );
    assert_eq!(charge_label(&store, other, 2).as_deref(), Some("Home"));
    assert_eq!(mutation_log(&store, other)[history.1.len()..], ["charge 2"]);
    assert_eq!(raw_rows(&store, other, "materialised_drives"), history.0);
    assert_eq!(export_revision(&store, other), revision + 1, "marked once");
}
