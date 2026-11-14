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
