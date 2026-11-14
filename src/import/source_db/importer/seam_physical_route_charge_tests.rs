// SPDX-License-Identifier: AGPL-3.0-only

//! The negotiated 1.4 route carries the seam for charges and their samples
//! (M03-T21): through the packs served to a fresh and to an existing 1.4
//! client, against the planned lineage the store publishes.

use super::super::seam_physical_route::{
    PhysicalClient, history_stage, paired, publish_physical,
};
use super::*;

/// One later import of `history` on both routes, as the importer publishes
/// it: the planned lineage, then the physical stage with the proved plan.
fn import_both(
    fixture: &Fixture,
    root: &std::path::Path,
    name: &str,
    history: &SourceDbHistory,
    at_ms: i64,
) {
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
    publish_physical(fixture, history_stage(root, name, history), Some(&planned.seam.packs))
        .expect("physical publication");
}

/// What the planned lineage gives every client: its charges and samples.
fn lineage(fixture: &Fixture) -> ChargeClient {
    let mut client = ChargeClient::default();
    for rows in charge_heads(&fixture.store, fixture.binding.vehicle_id) {
        client.apply(&rows);
    }
    client
}

/// A vehicle with an imported charge and a window in which the Hub's own
/// charge wins: a fresh 1.4 client bootstraps and an existing one takes
/// `changes-since`, and both end with exactly the charges and samples the
/// planned lineage gives: source charge 21 gone, its sample outside the
/// window under the Hub's charge, its sample inside the window gone, the
/// Hub's sample kept. An unchanged second import publishes nothing.
#[test]
fn seam_physical_route_charges_and_samples_reach_fresh_and_existing_clients() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head");
    let history = charge_history(
        vec![source_process(21, T - 10 * MINUTE, T + 20 * MINUTE)],
        vec![
            source_sample(2101, 21, T - 5 * MINUTE),
            source_sample(2102, 21, T + 5 * MINUTE),
        ],
    );
    import_both(&fixture, root.path(), "before-window", &history, T - 30 * MINUTE);
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served before the window");
    assert_eq!(existing.mirror.samples.get(&2101), Some(&(21, T - 5 * MINUTE)));

    let hub_charge = HUB_ID_BASE + 21;
    hub_charges(
        &fixture,
        (1, T, T + HOUR),
        &[(hub_charge, T, T + 50 * MINUTE)],
        &[(HUB_ID_BASE + 21, hub_charge, T + 30 * MINUTE)],
        &[(HUB_ID_BASE + 2100, T + MINUTE)],
    );
    import_both(&fixture, root.path(), "after-window", &history, T + 3 * HOUR);

    let planned = lineage(&fixture);
    assert!(!planned.charges.contains_key(&21) && planned.charges.contains_key(&hub_charge));
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("the planned head is served");
    assert_eq!(fresh.mirror.charges, planned.charges, "fresh client: the store's charges");
    assert_eq!(fresh.mirror.samples, planned.samples, "fresh client: their samples");
    assert_eq!(fresh.mirror.samples.get(&2101), Some(&(hub_charge, T - 5 * MINUTE)));
    assert!(!fresh.mirror.samples.contains_key(&2102), "inside coverage the Hub's samples stand");
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    assert_eq!(existing.mirror, fresh.mirror, "the existing client ends where a fresh one does");

    import_both(&fixture, root.path(), "unchanged", &history, T + 4 * HOUR);
    assert_eq!(existing.sync(&env, &fixture), "no_op", "an unchanged replay publishes nothing");
}

/// The refusal stays only where the 1.4 target cannot be planned, and says
/// so: the Hub's charge has no position anywhere to stand at, so the head
/// is withheld and a 1.4 client is refused as before; the planned lineage
/// still carries the import.
#[test]
fn seam_physical_route_refuses_a_target_it_cannot_plan() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head");
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served before the window");
    let hub_charge = HUB_ID_BASE + 21;
    hub_charges(
        &fixture,
        (1, T, T + HOUR),
        &[(hub_charge, T, T + 50 * MINUTE)],
        &[(HUB_ID_BASE + 21, hub_charge, T + 30 * MINUTE)],
        &[],
    );
    let history = charge_history(vec![], vec![]);
    import_both(&fixture, root.path(), "after-window", &history, T + 3 * HOUR);
    assert!(lineage(&fixture).charges.contains_key(&hub_charge));
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE)
    );
    assert_eq!(existing.sync(&env, &fixture), "406 \"schema_range_unsupported\"");
}
