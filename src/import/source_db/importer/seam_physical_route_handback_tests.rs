// SPDX-License-Identifier: AGPL-3.0-only

//! A window's data reaches 1.4 clients without another import (M03-T22,
//! DEC-137): each import keeps its raw physical stage and source sessions,
//! and after a handback the head is planned from them with the new coverage,
//! through the packs served to a fresh and to an existing 1.4 client.

use super::super::charges::{
    capture_all, charge_history, hub_charges, source_process, source_sample,
};
use super::*;
use crate::physical_publication::PhysicalV3PublicationKind;

/// The physical half of one import with its source sessions kept, as the
/// importer publishes it after the planned lineage.
fn publish_kept(
    fixture: &Fixture,
    root: &Path,
    name: &str,
    history: &SourceDbHistory,
    seam: Option<&crate::import::source_db::seam::SeamPackPlan>,
) -> crate::physical_publication::PhysicalV3Publication {
    let projected = project_vehicle(history, CAR).expect("projection");
    publish_physical_route(
        &fixture.store,
        &fixture.cursor_key,
        fixture.binding.clone(),
        Some(history_stage(root, name, history)),
        &fixture.gate,
        seam,
        Some((&projected.snapshot.drives, &projected.snapshot.charges)),
    )
    .expect("physical publication")
    .expect("physical head")
}

/// One later import of `history` (drives and positions) on both routes,
/// keeping its source sessions; returns the physical publication.
fn import_drives(
    fixture: &Fixture,
    root: &Path,
    name: &str,
    history: &SourceDbHistory,
    at_ms: i64,
) -> crate::physical_publication::PhysicalV3Publication {
    let projected = project_vehicle(history, CAR).expect("projection");
    let mut planned = plan(fixture, &projected);
    let run_id = begin_run(fixture, at_ms);
    let capture = capture_everything(fixture, run_id, &projected);
    publish_captured(fixture, &projected, run_id, capture, &mut planned).expect("planned lineage");
    publish_kept(fixture, root, name, history, Some(&planned.seam.packs))
}

/// One later import of `history` with charges on both routes, keeping its
/// source sessions; returns the physical publication.
fn import_charges(
    fixture: &Fixture,
    root: &Path,
    name: &str,
    history: &SourceDbHistory,
    at_ms: i64,
) -> crate::physical_publication::PhysicalV3Publication {
    let projected = project_vehicle(history, CAR).expect("projection");
    let mut planned = plan(fixture, &projected);
    let run_id = begin_run(fixture, at_ms);
    let capture = capture_all(fixture, run_id, &projected);
    let base_snapshot_id = fixture
        .store
        .v2_head(fixture.binding.vehicle_id)
        .expect("head")
        .expect("base")
        .0;
    publish_planned_direct_successor(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        PlannedSuccessorRun {
            run_id,
            source_id: fixture.binding.account_id,
            imported_at_ms: at_ms,
            base_snapshot_id,
            minimum_free_bytes: 0,
            fingerprint: Sha256Digest::of_bytes(format!("physical-{run_id}").as_bytes()),
            geofences: &[],
            materialised_car: &fixture.car,
            materialised_drives: &projected.snapshot.drives,
            materialised_charges: &projected.snapshot.charges,
        },
        &fixture.binding,
        &fixture.car,
        capture,
        &mut planned,
    )
    .expect("planned lineage");
    publish_kept(fixture, root, name, history, Some(&planned.seam.packs))
}

/// The handback's publication, as the service runs it after a close.
fn handback(fixture: &Fixture) -> HandbackPhysicalOutcome {
    let outcomes =
        publish_physical_after_handback(&fixture.store, &fixture.cursor_key, &fixture.gate)
            .expect("handback publication");
    assert_eq!(outcomes.len(), 1, "one vehicle with a kept stage");
    assert_eq!(outcomes[0].0, fixture.binding.vehicle_id);
    outcomes[0].1.clone()
}

fn published(kind: PhysicalV3PublicationKind) -> HandbackPhysicalOutcome {
    HandbackPhysicalOutcome::Published(kind)
}

/// Drives and positions collected in a first and in a second window reach a
/// fresh and an existing 1.4 client after each handback, with no import in
/// between or after: the head each window left is refused until its handback
/// plans it. The next import of the unchanged source publishes no new head.
#[test]
fn seam_physical_route_handback_drives_reach_clients_after_each_window() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    let base = base_history();
    publish_kept(&fixture, root.path(), "base", &base, None);
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served before the window");
    assert_eq!(
        existing.mirror.drives.keys().copied().collect::<Vec<_>>(),
        vec![10]
    );

    // The first window.
    let (h1, h2) = collect(&fixture);
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE),
        "the window's head is refused until it is planned, never served stale"
    );
    assert_eq!(
        handback(&fixture),
        published(PhysicalV3PublicationKind::Rotation)
    );
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("the planned head is served");
    assert_eq!(
        fresh.mirror.drives.keys().copied().collect::<Vec<_>>(),
        vec![10, h1, h2]
    );
    assert_eq!(
        fresh.mirror.positions,
        BTreeMap::from([
            (HUB_ID_BASE + 1, (Some(h1), T + 15 * MINUTE)),
            (HUB_ID_BASE + 2, (Some(h2), T + 55 * MINUTE)),
        ]),
        "the window's positions under their drives"
    );
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    assert_eq!(
        existing.mirror, fresh.mirror,
        "the existing client ends where a fresh one does"
    );
    assert_eq!(
        handback(&fixture),
        HandbackPhysicalOutcome::Current,
        "a second run plans nothing"
    );

    // The second window, with no import since the first.
    let h3 = HUB_ID_BASE + 30;
    window(&fixture, 2, T + 2 * HOUR - 5 * MINUTE, T + 3 * HOUR);
    hub_rows(
        &fixture,
        &[(h3, T + 2 * HOUR, T + 2 * HOUR + 30 * MINUTE)],
        &[(HUB_ID_BASE + 30, h3, T + 2 * HOUR + 15 * MINUTE)],
    );
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE)
    );
    assert_eq!(
        handback(&fixture),
        published(PhysicalV3PublicationKind::Rotation)
    );
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("served after the second window");
    assert_eq!(
        fresh.mirror.drives.keys().copied().collect::<Vec<_>>(),
        vec![10, h1, h2, h3]
    );
    assert_eq!(
        fresh.mirror.positions.get(&(HUB_ID_BASE + 30)),
        Some(&(Some(h3), T + 2 * HOUR + 15 * MINUTE))
    );
    assert_eq!(existing.mirror, fresh.mirror);

    // The next import of the unchanged source.
    let replay = import_drives(&fixture, root.path(), "unchanged", &base, T + 4 * HOUR);
    assert_eq!(replay.kind, PhysicalV3PublicationKind::Unchanged);
    assert_eq!(existing.sync(&env, &fixture), "no_op");
}

/// Charges with their samples collected in a first and in a second window
/// reach a fresh and an existing 1.4 client after each handback, beside the
/// imported charge, with no import in between or after; the unchanged replay
/// publishes no new head.
#[test]
fn seam_physical_route_handback_charges_and_samples_reach_clients() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_kept(&fixture, root.path(), "base", &base_history(), None);
    let history = charge_history(
        vec![source_process(21, T - 2 * HOUR, T - 2 * HOUR + 20 * MINUTE)],
        vec![source_sample(2101, 21, T - 2 * HOUR + 5 * MINUTE)],
    );
    import_charges(&fixture, root.path(), "before-window", &history, T - HOUR);
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served before the window");
    assert_eq!(
        existing.mirror.samples.get(&2101),
        Some(&(21, T - 2 * HOUR + 5 * MINUTE))
    );

    let hub_charge = HUB_ID_BASE + 21;
    hub_charges(
        &fixture,
        (1, T, T + HOUR),
        &[(hub_charge, T, T + 50 * MINUTE)],
        &[
            (HUB_ID_BASE + 21, hub_charge, T + 10 * MINUTE),
            (HUB_ID_BASE + 22, hub_charge, T + 30 * MINUTE),
        ],
        &[(HUB_ID_BASE + 2100, T + MINUTE)],
    );
    assert!(
        PhysicalClient::bootstrap(&env, &fixture).is_err(),
        "refused until planned"
    );
    assert_eq!(
        handback(&fixture),
        published(PhysicalV3PublicationKind::Rotation)
    );
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("the planned head is served");
    assert_eq!(
        fresh.mirror.charges.keys().copied().collect::<Vec<_>>(),
        vec![21, hub_charge]
    );
    assert_eq!(
        fresh.mirror.samples.get(&2101),
        Some(&(21, T - 2 * HOUR + 5 * MINUTE))
    );
    assert_eq!(
        fresh.mirror.samples.get(&(HUB_ID_BASE + 21)),
        Some(&(hub_charge, T + 10 * MINUTE))
    );
    assert_eq!(
        fresh.mirror.samples.get(&(HUB_ID_BASE + 22)),
        Some(&(hub_charge, T + 30 * MINUTE))
    );
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    assert_eq!(
        existing.mirror, fresh.mirror,
        "the existing client ends where a fresh one does"
    );

    // The second window, with no import since the first.
    let second_charge = HUB_ID_BASE + 23;
    hub_charges(
        &fixture,
        (2, T + 2 * HOUR, T + 3 * HOUR),
        &[(second_charge, T + 2 * HOUR, T + 2 * HOUR + 40 * MINUTE)],
        &[
            (HUB_ID_BASE + 23, second_charge, T + 2 * HOUR + 10 * MINUTE),
            (HUB_ID_BASE + 24, second_charge, T + 2 * HOUR + 30 * MINUTE),
        ],
        &[],
    );
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE),
        "the first window's head is refused after the second window"
    );
    assert_eq!(
        handback(&fixture),
        published(PhysicalV3PublicationKind::Rotation)
    );
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("served after the second window");
    assert_eq!(
        fresh.mirror.charges.keys().copied().collect::<Vec<_>>(),
        vec![21, hub_charge, second_charge]
    );
    assert_eq!(
        fresh.mirror.samples.get(&(HUB_ID_BASE + 23)),
        Some(&(second_charge, T + 2 * HOUR + 10 * MINUTE))
    );
    assert_eq!(
        fresh.mirror.samples.get(&(HUB_ID_BASE + 24)),
        Some(&(second_charge, T + 2 * HOUR + 30 * MINUTE))
    );
    assert_eq!(
        fresh.mirror.samples.get(&(HUB_ID_BASE + 21)),
        Some(&(hub_charge, T + 10 * MINUTE)),
        "the first window's samples stay"
    );
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    assert_eq!(existing.mirror, fresh.mirror);

    // The next import of the unchanged source.
    let replay = import_charges(&fixture, root.path(), "unchanged", &history, T + 4 * HOUR);
    assert_eq!(replay.kind, PhysicalV3PublicationKind::Unchanged);
    assert_eq!(existing.sync(&env, &fixture), "no_op");
}

/// The second window's handback does not reach the store's coverage (its
/// write fails, the window's row stays open): the first window's head is
/// refused to a fresh and an existing client, nothing is planned, and once
/// the handback is written again (the supervisor's retry) the second
/// window's drive is published with no import in between.
#[test]
fn seam_physical_route_handback_failed_coverage_write_refuses_until_reconciled() {
    use crate::collection::window::controller::{
        fail_next_coverage_handback, write_coverage_handback,
    };
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_kept(&fixture, root.path(), "base", &base_history(), None);
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served before the window");
    let (h1, h2) = collect(&fixture);
    assert_eq!(
        handback(&fixture),
        published(PhysicalV3PublicationKind::Rotation)
    );
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");

    // The second window opens, collects, and its handback write fails.
    let opened_at_ms = T + 2 * HOUR - 5 * MINUTE;
    crate::db::record_coverage_open(
        &fixture.store.open().expect("catalogue"),
        2,
        "grant-2",
        opened_at_ms,
    )
    .expect("open row");
    let h3 = HUB_ID_BASE + 30;
    hub_rows(
        &fixture,
        &[(h3, T + 2 * HOUR, T + 2 * HOUR + 30 * MINUTE)],
        &[(HUB_ID_BASE + 30, h3, T + 2 * HOUR + 15 * MINUTE)],
    );
    let second = crate::db::WindowCoverage {
        seq: 2,
        grant_digest: "grant-2".to_owned(),
        opened_at_ms,
        closed_at_ms: T + 3 * HOUR,
        gaps: Vec::new(),
    };
    let data_dir = fixture._temporary.path();
    fail_next_coverage_handback();
    assert!(!write_coverage_handback(data_dir, &second), "the forced failure");
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE),
        "the first window's head is never served after the second handback"
    );
    let refused = existing.sync(&env, &fixture);
    assert!(
        refused != "no_op" && refused != "physical_changed_set",
        "the existing client is refused, not told it is current: {refused}"
    );
    assert!(
        matches!(handback(&fixture), HandbackPhysicalOutcome::Withheld(_)),
        "nothing is planned from coverage that lacks the handback"
    );

    // The supervisor writes the handback again; the window then reaches both
    // clients with no import in between.
    assert!(write_coverage_handback(data_dir, &second), "the retry");
    assert_eq!(
        handback(&fixture),
        published(PhysicalV3PublicationKind::Rotation)
    );
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("served once reconciled");
    assert_eq!(
        fresh.mirror.drives.keys().copied().collect::<Vec<_>>(),
        vec![10, h1, h2, h3]
    );
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    assert_eq!(existing.mirror, fresh.mirror);
}

/// The store's drives as (id, start, end).
fn stored_drive_ids(fixture: &Fixture) -> Vec<i64> {
    store_drives(&fixture.store, fixture.binding.vehicle_id)
        .keys()
        .copied()
        .collect()
}

/// A plan that changes the store but restates no source row: an imported
/// drive inside the window loses to the Hub's. A failure right after the
/// store transaction commits leaves the head refused; the rerun plans it
/// again and serves the new head, and the next import of the unchanged
/// source publishes nothing new.
#[test]
fn seam_repair_handback_failure_after_the_store_commit_is_refused_then_served() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    let mut drives = base_history().drives;
    drives.push(source_drive(11, T + 10 * MINUTE, T + 20 * MINUTE));
    let source = history(
        drives,
        vec![source_position(1101, Some(11), T + 12 * MINUTE)],
    );
    publish_kept(&fixture, root.path(), "base", &base_history(), None);
    import_drives(
        &fixture,
        root.path(),
        "before-window",
        &source,
        T - 30 * MINUTE,
    );
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served before the window");
    assert!(existing.mirror.drives.contains_key(&11));
    assert_eq!(stored_drive_ids(&fixture), vec![10, 11]);

    let (h1, h2) = collect(&fixture);
    FAIL_AFTER_HANDBACK_COMMIT.with(|fail| fail.set(true));
    assert!(
        publish_physical_after_handback(&fixture.store, &fixture.cursor_key, &fixture.gate)
            .is_err(),
        "the forced failure after the store commit"
    );
    assert!(
        !stored_drive_ids(&fixture).contains(&11),
        "the store transaction committed"
    );
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE),
        "the head stays refused"
    );
    assert_eq!(
        existing.sync(&env, &fixture),
        "406 \"schema_range_unsupported\""
    );

    assert_eq!(
        handback(&fixture),
        published(PhysicalV3PublicationKind::Rotation)
    );
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("the rerun serves the new head");
    assert_eq!(
        fresh.mirror.drives.keys().copied().collect::<Vec<_>>(),
        vec![10, h1, h2]
    );
    assert!(
        !fresh.mirror.positions.contains_key(&1101),
        "inside coverage the Hub's rows stand"
    );
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    assert_eq!(existing.mirror, fresh.mirror);

    let replay = import_drives(&fixture, root.path(), "unchanged", &source, T + 3 * HOUR);
    assert_eq!(replay.kind, PhysicalV3PublicationKind::Unchanged);
    assert_eq!(existing.sync(&env, &fixture), "no_op");
}

/// A plan that restates a source row: an imported drive crossing the
/// window's start wins over the Hub's drive and widens to hold its
/// position. Nothing is applied and the head stays refused until the next
/// import, which plans and serves it.
#[test]
fn seam_repair_handback_restating_plan_applies_nothing_until_the_next_import() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    let mut drives = base_history().drives;
    drives.push(source_drive(14, T - 10 * MINUTE, T + 20 * MINUTE));
    let source = history(
        drives,
        vec![source_position(1401, Some(14), T - 5 * MINUTE)],
    );
    publish_kept(&fixture, root.path(), "base", &base_history(), None);
    import_drives(
        &fixture,
        root.path(),
        "before-window",
        &source,
        T - 30 * MINUTE,
    );
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served before the window");

    let h = HUB_ID_BASE + 14;
    window(&fixture, 1, T, T + HOUR);
    hub_rows(
        &fixture,
        &[(h, T + 5 * MINUTE, T + 30 * MINUTE)],
        &[(HUB_ID_BASE + 1401, h, T + 25 * MINUTE)],
    );
    let journal = mutation_count(&fixture.store, fixture.binding.vehicle_id);
    let stored = store_drives(&fixture.store, fixture.binding.vehicle_id);
    assert_eq!(handback(&fixture), HandbackPhysicalOutcome::Restates);
    assert_eq!(
        store_drives(&fixture.store, fixture.binding.vehicle_id),
        stored,
        "nothing applied"
    );
    assert_eq!(
        mutation_count(&fixture.store, fixture.binding.vehicle_id),
        journal
    );
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE),
        "refused until the next import"
    );
    assert_eq!(
        handback(&fixture),
        HandbackPhysicalOutcome::Restates,
        "and on every rerun"
    );

    import_drives(&fixture, root.path(), "after-window", &source, T + 3 * HOUR);
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("the import plans the head");
    assert!(fresh.mirror.drives.contains_key(&14) && !fresh.mirror.drives.contains_key(&h));
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    assert_eq!(existing.mirror, fresh.mirror);
}

/// A data directory without a cursor key (a data-only restore waiting for
/// its recovery) publishes nothing after a handback and creates no key.
#[test]
fn seam_repair_handback_without_a_cursor_key_publishes_nothing() {
    let fixture = fixture();
    let data_dir = fixture._temporary.path();
    let key = crate::legacy_owner_credentials::cursor_key_path(data_dir);
    assert!(std::fs::symlink_metadata(&key).is_err());
    crate::runtime::operator::authority::publish_window_after_handback(data_dir);
    assert!(
        std::fs::symlink_metadata(&key).is_err(),
        "no key is created"
    );
}

/// The Tesla Owner API vehicle id the collector binds a window-only base to:
/// beyond the 1.4 route's car id, as on a real Hub (DEC-150).
const WINDOW_CAR: i64 = 1_492_931_520_123_456;

/// A Hub set up new and signed in that collected in one window and never
/// imported (DEC-150): the collector's base, bound to the `owner_api_compat`
/// source and the Tesla vehicle id, holds its car and Hub drive H1 with a
/// position; Hub drive H2 with a position followed as live sync. The
/// fixture's binding is the import's, for the same vehicle.
fn window_only_fixture() -> (Fixture, crate::protocol::LineageManifestV2) {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let owner = store
        .register_source(&SourceDescriptor::new("owner_api_compat", "local_installation_v1"), T)
        .expect("owner source");
    let vin = Some("5YJ3WINDOWONLY0001".to_owned());
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor { vin: vin.clone(), ..VehicleDescriptor::new(owner.source_id, "window-car") },
            T,
        )
        .expect("vehicle");
    let import = store
        .register_source(&SourceDescriptor::new(crate::db::IMPORT_SOURCE_KIND, "after-windows"), T)
        .expect("import source");
    // The import's verified car matches the collector's vehicle by VIN, as
    // the importer's identity registration does.
    let imported = store
        .register_vehicle(&VehicleDescriptor { vin, ..VehicleDescriptor::new(import.source_id, "1") }, T)
        .expect("import identity");
    assert_eq!(imported.vehicle_id, vehicle.vehicle_id, "one vehicle");
    let installation_id = store.installation_id().expect("installation");
    let window_binding = ProjectionBinding {
        installation_id,
        account_id: owner.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: owner.generation,
        selected_car_id: WINDOW_CAR,
    };
    let gate = store.try_acquire_publication_gate().expect("gate");
    let car = project_vehicle(&base_history(), CAR).expect("projection").snapshot.cars[0].clone();
    let fixture = Fixture {
        _temporary: temporary,
        store,
        binding: ProjectionBinding {
            installation_id,
            account_id: import.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: import.generation,
            selected_car_id: CAR,
        },
        car: car.clone(),
        gate,
        cursor_key: CursorKey::from_bytes([43; 32]),
    };
    window(&fixture, 1, T, T + HOUR);
    let (h1, h2) = (HUB_ID_BASE + 1, HUB_ID_BASE + 2);
    let template = project_vehicle(&base_history(), CAR).expect("projection").snapshot.drives[0]
        .clone();
    let hub_drive = |id: i64, start_ms: i64, end_ms: i64| {
        let mut drive = template.clone();
        drive.id = id;
        drive.car_id = WINDOW_CAR;
        drive.start_date_ms = start_ms;
        drive.end_date_ms = end_ms;
        drive
    };
    let hub_position = |id: i64, drive_id: i64, date_ms: i64| {
        let mut position =
            crate::lifecycle::imported_position(&source_position(id, Some(drive_id), date_ms));
        position.car_id = WINDOW_CAR;
        position.drive_id = Some(drive_id);
        position
    };
    let mut window_car = car;
    window_car.id = WINDOW_CAR;
    let drives = [hub_drive(h1, T + 10 * MINUTE, T + 20 * MINUTE), hub_drive(h2, T + 50 * MINUTE, T + 60 * MINUTE)];
    let positions = [hub_position(HUB_ID_BASE + 1, h1, T + 15 * MINUTE), hub_position(HUB_ID_BASE + 2, h2, T + 55 * MINUTE)];
    let vehicle_key = vehicle.vehicle_id.to_string();
    {
        let connection = fixture.store.open().expect("catalogue");
        connection
            .execute(
                "INSERT INTO materialised_cars(vehicle_id, car_id, car_json) VALUES (?1, ?2, ?3)",
                params![vehicle_key, WINDOW_CAR, serde_json::to_string(&window_car).expect("car")],
            )
            .expect("window car");
        for drive in &drives {
            connection
                .execute(
                    "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![vehicle_key, drive.id, WINDOW_CAR, serde_json::to_string(drive).expect("drive")],
                )
                .expect("Hub drive");
        }
        for position in &positions {
            connection
                .execute(
                    "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![vehicle_key, position.id, position.drive_id, WINDOW_CAR,
                        serde_json::to_string(position).expect("position")],
                )
                .expect("Hub position");
        }
    }
    // The collector's own base: its car, H1 and H1's position.
    let sequence = fixture
        .store
        .reserve_next_full_snapshot_sequence(&fixture.gate, vehicle.vehicle_id)
        .expect("window base sequence");
    let snapshot = crate::hub_pack::ProjectionSnapshot {
        cars: vec![window_car],
        drives: vec![drives[0].clone()],
        positions: vec![positions[0].clone()],
        charges: vec![],
        charge_samples: vec![],
    };
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: window_binding,
        sequence: SequenceRange { from_exclusive: sequence, to_inclusive: sequence },
        snapshot: &snapshot,
    };
    let pack = ProjectionPackWriter::new(fixture.store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])
        .expect("window base pack");
    let manifest = request
        .signed_manifest_with_states_and_updates(&pack, &[], &[], &fixture.cursor_key)
        .expect("window base manifest");
    fixture
        .store
        .finalize_import_snapshot_with_binding(
            &manifest,
            Sha256Digest::of_bytes(b"window base"),
            &[],
            &request.binding,
        )
        .expect("window base");
    // H2 and its position after the base, as live sync of that lineage.
    {
        let mut connection = fixture.store.open().expect("catalogue");
        let transaction = connection.transaction().expect("transaction");
        for (entity, id, json) in [
            ("drive", h2, serde_json::to_string(&drives[1]).expect("drive")),
            ("position", HUB_ID_BASE + 2, serde_json::to_string(&positions[1]).expect("position")),
        ] {
            let revision: i64 = transaction
                .query_row(
                    "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision) VALUES (?1, 2)
                     ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = next_revision + 1
                     RETURNING next_revision - 1",
                    params![vehicle_key],
                    |row| row.get(0),
                )
                .expect("revision");
            transaction
                .execute(
                    "INSERT INTO sync_mutations(vehicle_id, revision, entity, entity_id, car_id,
                        operation, payload_json, published, claimed_until_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'upsert', ?6, 0, 0)",
                    params![vehicle_key, revision, entity, id, WINDOW_CAR, json],
                )
                .expect("mutation");
        }
        transaction.commit().expect("commit");
    }
    publish_sync_mutations_ahead_of_packs(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        vehicle.vehicle_id,
    )
    .expect("live sync of the window");
    let window_lineage = fixture
        .store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("lineage")
        .expect("window lineage");
    assert!(!window_lineage.deltas.is_empty(), "H2 reached clients as a live delta");
    (fixture, window_lineage)
}

/// The importer's route for the fixture's vehicle and import source.
fn route(fixture: &Fixture) -> Result<DirectImportRoute, SourceDbImportError> {
    direct_import_route(&fixture.store, fixture.binding.vehicle_id, fixture.binding.account_id, CAR)
}

/// The first import onto the window-only base, as the importer's two passes
/// run it under one gate: the base in the window base's place, then the
/// successor that plans the seam and re-sends the Hub's rows; then the 1.4
/// head from the second pass's stage.
fn first_import_after_windows(
    fixture: &Fixture,
    root: &Path,
    history: &SourceDbHistory,
    at_ms: i64,
    physical: bool,
) -> Result<(), crate::db::StoreError> {
    let Ok(DirectImportRoute::WindowRebind(expected)) = route(fixture) else {
        panic!("a window-only base is rebound");
    };
    let projected = project_vehicle(history, CAR).expect("projection");
    let run_id = begin_run(fixture, at_ms);
    let state = capture(fixture, run_id, false, &projected).into_state();
    let sequence = fixture
        .store
        .reserve_next_full_snapshot_sequence(&fixture.gate, fixture.binding.vehicle_id)
        .expect("base sequence");
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: fixture.binding.clone(),
        sequence: SequenceRange { from_exclusive: sequence, to_inclusive: sequence },
        snapshot: &projected.snapshot,
    };
    let pack = ProjectionPackWriter::new(fixture.store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])
        .expect("import base pack");
    let manifest = request
        .signed_manifest_with_states_and_updates(&pack, &[], &[], &fixture.cursor_key)
        .expect("import base manifest");
    fixture.store.finalize_import_generation_rebinding_window_base_with_materialisation(
        &fixture.gate,
        &expected,
        run_id,
        fixture.binding.account_id,
        fixture.binding.vehicle_id,
        CAR,
        at_ms,
        &manifest,
        Sha256Digest::of_bytes(b"import base after windows"),
        &[],
        &fixture.binding,
        &state,
        &fixture.car,
        &projected.snapshot.drives,
    )
    .map_err(|error| panic!("rebind finalisation: {error:?}"))
    .ok();
    assert!(
        matches!(route(fixture), Ok(DirectImportRoute::Successor)),
        "the second pass is the successor"
    );
    let mut planned = plan(fixture, &projected);
    let run_id = begin_run(fixture, at_ms);
    let capture = capture_everything(fixture, run_id, &projected);
    publish_captured(fixture, &projected, run_id, capture, &mut planned).expect("planned lineage");
    if physical {
        publish_physical(fixture, history_stage(root, "after-windows", history), Some(&planned.seam.packs))
            .expect("physical publication")
            .expect("physical head");
    }
    Ok(())
}

/// A first import onto a Hub that collected in a window before any import is
/// accepted (DEC-150): the window base is retired and the import's base takes
/// its place; the source's history before the window is published; the Hub's
/// rows are kept, take the import's car id and reach a fresh 2.1 client and
/// a fresh 1.4 client, and the seam stands (source drive 11 loses to H1).
#[test]
fn seam_physical_route_import_after_windows_keeps_the_window_rows() {
    let (fixture, window_lineage) = window_only_fixture();
    let (h1, h2) = (HUB_ID_BASE + 1, HUB_ID_BASE + 2);
    let root = crate::private_tempdir().expect("stage root");
    first_import_after_windows(&fixture, root.path(), &successor_history(), T + 3 * HOUR, true)
        .expect("first import after windows");

    let binding = fixture.store.v2_projection_binding(fixture.binding.vehicle_id).expect("binding");
    assert_eq!(binding, fixture.binding, "the base is the import's now");
    let retired: i64 = fixture
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT COUNT(*) FROM sync_retired_lineages WHERE vehicle_id = ?1 AND head_digest = ?2",
            params![fixture.binding.vehicle_id.to_string(), window_lineage.head_digest.to_string()],
            |row| row.get(0),
        )
        .expect("retired lineages");
    assert_eq!(retired, 1, "the window base is retired, not deleted");
    let foreign_car: i64 = fixture
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT (SELECT COUNT(*) FROM materialised_drives WHERE car_id <> ?1)
                  + (SELECT COUNT(*) FROM materialised_positions WHERE car_id <> ?1)",
            params![CAR],
            |row| row.get(0),
        )
        .expect("car ids");
    assert_eq!(foreign_car, 0, "the kept Hub rows carry the import's car id");

    let lineage = client(&fixture);
    for id in [10, 12, 13, h1] {
        assert!(lineage.drives.contains_key(&id), "2.1 client holds drive {id}");
    }
    assert!(!lineage.drives.contains_key(&11), "drive 11 lost to H1");
    assert!(!lineage.drives.contains_key(&h2), "drive 13 crosses the window's end and wins over H2");
    assert!(lineage.positions.contains_key(&(HUB_ID_BASE + 1)), "H1's position is kept");
    assert!(lineage.positions.contains_key(&(HUB_ID_BASE + 2)), "H2's position is kept");
    assert!(!lineage.positions.contains_key(&1101), "inside coverage the Hub's rows stand");

    let env = paired(&fixture);
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("the 1.4 head is served");
    assert_eq!(fresh.mirror.drives, lineage.drives, "1.4 client: the lineage's drives");
    assert_eq!(fresh.mirror.positions, lineage.positions, "1.4 client: their positions");
    assert!(
        matches!(route(&fixture), Ok(DirectImportRoute::Successor)),
        "the rebind happens once"
    );
}

/// After the rebind, a second window and a second import take the normal
/// successor path: the Hub's drive H3 wins over source drive 12 and the
/// existing 1.4 client takes a changed set that ends where a fresh one does.
#[test]
fn seam_physical_route_import_after_windows_second_import_is_a_normal_successor() {
    let (fixture, _) = window_only_fixture();
    let root = crate::private_tempdir().expect("stage root");
    let history = successor_history();
    first_import_after_windows(&fixture, root.path(), &history, T + 3 * HOUR, true)
        .expect("first import after windows");
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served after the rebind");

    let h3 = HUB_ID_BASE + 30;
    window(&fixture, 2, T + 2 * HOUR - 5 * MINUTE, T + 3 * HOUR);
    hub_rows(
        &fixture,
        &[(h3, T + 2 * HOUR, T + 2 * HOUR + 30 * MINUTE)],
        &[(HUB_ID_BASE + 30, h3, T + 2 * HOUR + 15 * MINUTE)],
    );
    assert!(matches!(route(&fixture), Ok(DirectImportRoute::Successor)));
    import_both(&fixture, root.path(), "second-import", &history, T + 5 * HOUR);
    let lineage = client(&fixture);
    assert!(lineage.drives.contains_key(&h3) && !lineage.drives.contains_key(&12));
    assert!(lineage.drives.contains_key(&(HUB_ID_BASE + 1)), "the first window's rows stay");
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("served after the second import");
    assert_eq!(fresh.mirror.drives, lineage.drives);
    assert_eq!(fresh.mirror.positions, lineage.positions);
    assert_eq!(existing.mirror, fresh.mirror);
}

/// Only the collector's base on a Hub that never imported is rebound: a
/// direct-import base without projection state that the bridge cannot prove
/// unchanged keeps `rebase_required`, an import base with projection state
/// is a successor, and the rebind's own finalisation refuses an import
/// lineage.
#[test]
fn seam_physical_route_import_after_windows_rebinds_only_a_window_only_base() {
    // A direct-import base with no projection state or inventory.
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let import = store
        .register_source(&SourceDescriptor::new(crate::db::IMPORT_SOURCE_KIND, "legacy-direct"), T)
        .expect("import source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(import.source_id, "legacy-car"), T)
        .expect("vehicle");
    let binding = ProjectionBinding {
        installation_id: store.installation_id().expect("installation"),
        account_id: import.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: import.generation,
        selected_car_id: CAR,
    };
    let gate = store.try_acquire_publication_gate().expect("gate");
    let projected = project_vehicle(&base_history(), CAR).expect("projection");
    let sequence = store
        .reserve_next_full_snapshot_sequence(&gate, vehicle.vehicle_id)
        .expect("sequence");
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: binding.clone(),
        sequence: SequenceRange { from_exclusive: sequence, to_inclusive: sequence },
        snapshot: &projected.snapshot,
    };
    let cursor_key = CursorKey::from_bytes([44; 32]);
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])
        .expect("pack");
    let manifest = request
        .signed_manifest_with_states_and_updates(&pack, &[], &[], &cursor_key)
        .expect("manifest");
    store
        .finalize_import_snapshot_with_binding(&manifest, Sha256Digest::of_bytes(b"legacy"), &[], &binding)
        .expect("legacy direct base");
    assert!(!store.window_only_v2_base_is_rebindable(vehicle.vehicle_id).expect("rebindable"));
    assert!(matches!(
        direct_import_route(&store, vehicle.vehicle_id, import.source_id, CAR),
        Err(SourceDbImportError::LegacyDirectImportRebaseRequired)
    ));

    // An import base with projection state: a successor, never rebound.
    let fixture = fixture();
    assert!(matches!(route(&fixture), Ok(DirectImportRoute::Successor)));
    assert!(!fixture.store.window_only_v2_base_is_rebindable(fixture.binding.vehicle_id).expect("rebindable"));
    let expected = fixture
        .store
        .lineage_manifest_for_vehicle(fixture.binding.vehicle_id)
        .expect("lineage")
        .expect("import lineage");
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    let state = capture(&fixture, run_id, false, &projected).into_state();
    let sequence = fixture
        .store
        .reserve_next_full_snapshot_sequence(&fixture.gate, fixture.binding.vehicle_id)
        .expect("sequence");
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: fixture.binding.clone(),
        sequence: SequenceRange { from_exclusive: sequence, to_inclusive: sequence },
        snapshot: &projected.snapshot,
    };
    let pack = ProjectionPackWriter::new(fixture.store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])
        .expect("pack");
    let manifest = request
        .signed_manifest_with_states_and_updates(&pack, &[], &[], &fixture.cursor_key)
        .expect("manifest");
    let refused = fixture.store.finalize_import_generation_rebinding_window_base_with_materialisation(
        &fixture.gate,
        &expected,
        run_id,
        fixture.binding.account_id,
        fixture.binding.vehicle_id,
        CAR,
        T + 3 * HOUR,
        &manifest,
        Sha256Digest::of_bytes(b"not a window base"),
        &[],
        &fixture.binding,
        &state,
        &fixture.car,
        &projected.snapshot.drives,
    );
    assert!(
        matches!(refused, Err(crate::db::StoreError::LineageCatalogConflict)),
        "an import lineage is never rebound: {refused:?}"
    );
    assert_eq!(
        fixture.store.lineage_manifest_for_vehicle(fixture.binding.vehicle_id).expect("lineage"),
        Some(expected),
        "the refused rebind changed nothing"
    );
}

/// A Hub drive and one position under it, inside the window, stored as the
/// collector keeps them (window car id) and not yet published.
fn window_rows(fixture: &Fixture, drive_id: i64, start_ms: i64, end_ms: i64, position_ms: i64) {
    let mut drive = project_vehicle(&base_history(), CAR).expect("projection").snapshot.drives[0].clone();
    drive.id = drive_id;
    drive.car_id = WINDOW_CAR;
    drive.start_date_ms = start_ms;
    drive.end_date_ms = end_ms;
    let mut position =
        crate::lifecycle::imported_position(&source_position(drive_id, Some(drive_id), position_ms));
    position.car_id = WINDOW_CAR;
    position.drive_id = Some(drive_id);
    let connection = fixture.store.open().expect("catalogue");
    let vehicle = fixture.binding.vehicle_id.to_string();
    connection
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json) VALUES (?1, ?2, ?3, ?4)",
            params![vehicle, drive_id, WINDOW_CAR, serde_json::to_string(&drive).expect("drive")],
        )
        .expect("Hub drive");
    connection
        .execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![vehicle, drive_id, drive_id, WINDOW_CAR, serde_json::to_string(&position).expect("position")],
        )
        .expect("Hub position");
}

/// Hub positions under H1, inside the window, straight into the store as
/// the collector keeps them (window car id, ids after `first`).
fn many_window_positions(fixture: &Fixture, first: i64, count: i64) {
    let mut connection = fixture.store.open().expect("catalogue");
    let transaction = connection.transaction().expect("transaction");
    let vehicle = fixture.binding.vehicle_id.to_string();
    let h1 = HUB_ID_BASE + 1;
    for offset in 0..count {
        let id = first + offset;
        let mut position = crate::lifecycle::imported_position(&source_position(
            id,
            Some(h1),
            T + 10 * MINUTE + offset % (10 * MINUTE),
        ));
        position.car_id = WINDOW_CAR;
        position.drive_id = Some(h1);
        transaction
            .execute(
                "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![vehicle, id, h1, WINDOW_CAR, serde_json::to_string(&position).expect("position")],
            )
            .expect("Hub position");
    }
    transaction.commit().expect("commit");
}

/// Review finding 1: a rebind whose journal is longer than one publication
/// batch (10,000 mutations) still publishes. The losing Hub drive H2 is
/// journalled once, as a tombstone, never as an upsert in an earlier batch
/// that reads a row the later batch removes; the import and the next
/// collector publication both go through.
#[test]
fn seam_physical_route_import_after_windows_publishes_a_journal_over_one_batch() {
    let (fixture, _) = window_only_fixture();
    let first = HUB_ID_BASE + 100;
    many_window_positions(&fixture, first, 10_050);
    let root = crate::private_tempdir().expect("stage root");
    first_import_after_windows(&fixture, root.path(), &successor_history(), T + 3 * HOUR, false)
        .expect("first import after windows");
    let lineage = client(&fixture);
    assert!(!lineage.drives.contains_key(&(HUB_ID_BASE + 2)), "H2 lost to drive 13");
    for id in [first, first + 9_999, first + 10_049] {
        assert_eq!(
            lineage.positions.get(&id).map(|(drive, _)| *drive),
            Some(Some(HUB_ID_BASE + 1)),
            "Hub position {id} reached clients under H1"
        );
    }
    let h2_journal: Vec<String> = {
        let connection = fixture.store.open().expect("catalogue");
        let mut statement = connection
            .prepare(
                "SELECT operation FROM sync_mutations
                  WHERE vehicle_id = ?1 AND entity = 'drive' AND entity_id = ?2
                    AND car_id = ?3 ORDER BY revision",
            )
            .expect("journal");
        statement
            .query_map(params![fixture.binding.vehicle_id.to_string(), HUB_ID_BASE + 2, CAR], |row| {
                row.get(0)
            })
            .expect("journal rows")
            .collect::<Result<_, _>>()
            .expect("journal rows")
    };
    assert_eq!(h2_journal, ["tombstone"], "the rebind journals H2 once");
    // The collector publishes after the import.
    let h4 = HUB_ID_BASE + 40;
    window(&fixture, 2, T + 4 * HOUR, T + 5 * HOUR);
    hub_rows(&fixture, &[(h4, T + 4 * HOUR, T + 4 * HOUR + 10 * MINUTE)], &[]);
    publish_sync_mutations_ahead_of_packs(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        fixture.binding.vehicle_id,
    )
    .expect("the collector publishes after the import");
    assert!(client(&fixture).drives.contains_key(&h4), "the next window reaches clients");
}

/// Review finding 2: a source drive that wins over a Hub drive and widens
/// around a kept Hub position reaches 2.1 clients as stored. Source drive
/// 14 [30, 44] outlasts H3 [35, 46]; H3's position at 45 is kept and moves
/// under drive 14, whose end becomes 45 in the store, the 2.1 lineage and
/// the 1.4 head alike.
#[test]
fn seam_physical_route_import_after_windows_restates_a_widened_source_drive() {
    let (fixture, _) = window_only_fixture();
    let h3 = HUB_ID_BASE + 3;
    window_rows(&fixture, h3, T + 35 * MINUTE, T + 46 * MINUTE, T + 45 * MINUTE);
    let mut history = successor_history();
    history.drives.push(source_drive(14, T + 30 * MINUTE, T + 44 * MINUTE));
    let root = crate::private_tempdir().expect("stage root");
    first_import_after_windows(&fixture, root.path(), &history, T + 3 * HOUR, true)
        .expect("first import after windows");
    let stored: ProjectionDrive = {
        let json: String = fixture
            .store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT drive_json FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = 14",
                params![fixture.binding.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("drive 14 is stored");
        serde_json::from_str(&json).expect("drive 14")
    };
    assert_eq!(stored.end_date_ms, T + 45 * MINUTE, "drive 14 widened over H3's position");
    let lineage = client(&fixture);
    assert!(!lineage.drives.contains_key(&h3), "H3 lost to drive 14");
    assert_eq!(
        lineage.drives.get(&14),
        Some(&(stored.start_date_ms, stored.end_date_ms)),
        "2.1 clients hold drive 14 as stored"
    );
    assert_eq!(
        lineage.positions.get(&(HUB_ID_BASE + 3)).map(|(drive, _)| *drive),
        Some(Some(14)),
        "H3's position is kept under drive 14"
    );
    let env = paired(&fixture);
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("the 1.4 head is served");
    assert_eq!(fresh.mirror.drives, lineage.drives, "1.4 and 2.1 clients agree");
}
