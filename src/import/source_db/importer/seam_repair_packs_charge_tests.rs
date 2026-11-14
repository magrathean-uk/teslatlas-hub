// SPDX-License-Identifier: AGPL-3.0-only

//! The pack-side seam repair (M03-T19) for charges and charge samples, which
//! share the drives' and positions' code paths: an already packed sample
//! follows its charge's changed seam decision, a skipped charge leaves the
//! clients, and a source deletion never removes a charge the seam keeps.

use super::*;

/// A client that applied every advertised head's charges and samples.
fn charge_client(fixture: &Fixture) -> ChargeClient {
    let mut client = ChargeClient::default();
    for rows in charge_heads(&fixture.store, fixture.binding.vehicle_id) {
        client.apply(&rows);
    }
    client
}

/// An unchanged replay of `history` publishes no charge, sample or
/// tombstone.
fn assert_replay_publishes_nothing(fixture: &Fixture, history: &SourceDbHistory, at_ms: i64) {
    let before = charge_heads(&fixture.store, fixture.binding.vehicle_id).len();
    let skipped = import(fixture, history, at_ms);
    for rows in charge_heads(&fixture.store, fixture.binding.vehicle_id)
        .into_iter()
        .skip(before)
    {
        assert!(
            rows.charges.is_empty() && rows.samples.is_empty() && rows.tombstones.is_empty(),
            "an unchanged replay published {rows:?}"
        );
    }
    assert_eq!(skipped, SourceDbSeamSkipped::default());
}

/// Review finding 2 for charges: charge 21 and its samples were packed
/// before the window; the Hub's charge then wins the seam although no source
/// row changed. Clients lose charge 21, receive sample 2101 (outside
/// coverage) under the Hub's charge and lose sample 2102 (inside coverage).
#[test]
fn seam_repair_packs_charge_sample_follows_its_skipped_charge() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let history = charge_history(
        vec![source_process(21, T - 10 * MINUTE, T + 20 * MINUTE)],
        vec![
            source_sample(2101, 21, T - 5 * MINUTE),
            source_sample(2102, 21, T + 5 * MINUTE),
        ],
    );
    import(&fixture, &history, T - 30 * MINUTE);
    let packed = charge_client(&fixture);
    assert!(packed.charges.contains_key(&21));
    assert_eq!(packed.samples.get(&2101), Some(&(21, T - 5 * MINUTE)));
    assert!(packed.samples.contains_key(&2102));

    let hub_charge = HUB_ID_BASE + 21;
    collect(
        &fixture,
        &[(hub_charge, T, T + 50 * MINUTE)],
        &[(HUB_ID_BASE + 21, hub_charge, T + 30 * MINUTE)],
    );
    import(&fixture, &history, T + 3 * HOUR);
    let client = charge_client(&fixture);
    client.assert_each_charge_once("after the window");
    assert!(!client.charges.contains_key(&21), "charge 21 lost to the Hub's charge");
    assert_eq!(
        client.samples.get(&2101),
        Some(&(hub_charge, T - 5 * MINUTE)),
        "the packed sample follows its charge's winner"
    );
    assert!(!client.samples.contains_key(&2102), "inside coverage the Hub's samples stand");
    assert_eq!(client.charges, store_charges(&fixture.store, vehicle_id));
    assert_replay_publishes_nothing(&fixture, &history, T + 4 * HOUR);
}

/// Review finding 3 for charges: charge 41 was packed, then inherited and
/// extended by the Hub, whose sample holds it. The source deletes it: the
/// store keeps it, and so do the clients.
#[test]
fn seam_repair_packs_source_deletion_keeps_a_retained_charge() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let history = charge_history(
        vec![source_process(41, T - 10 * MINUTE, T + 5 * MINUTE)],
        vec![source_sample(4101, 41, T - 5 * MINUTE)],
    );
    import(&fixture, &history, T - 30 * MINUTE);
    collect(
        &fixture,
        &[(41, T - 10 * MINUTE, T + 30 * MINUTE)],
        &[(HUB_ID_BASE + 41, 41, T + 20 * MINUTE)],
    );
    assert!(charge_client(&fixture).charges.contains_key(&41));

    let deleted = charge_history(vec![], vec![]);
    import(&fixture, &deleted, T + 3 * HOUR);
    assert!(
        store_charges(&fixture.store, vehicle_id).contains_key(&41),
        "the store keeps charge 41 for the Hub's sample"
    );
    let client = charge_client(&fixture);
    assert_eq!(client.charges, store_charges(&fixture.store, vehicle_id));
    assert_eq!(client.samples.get(&(HUB_ID_BASE + 41)).map(|row| row.0), Some(41));
    assert!(!client.samples.contains_key(&4101), "the source's own sample went");
    assert_replay_publishes_nothing(&fixture, &deleted, T + 4 * HOUR);
}

/// One later import of `history`, whose source digest is `source`, recorded
/// under the fingerprint the importer folds for it.
fn import_through_fingerprint(
    fixture: &Fixture,
    history: &SourceDbHistory,
    source: Sha256Digest,
    at_ms: i64,
) {
    let projected = project_vehicle(history, CAR).expect("projection");
    let fingerprint = super::super::seam_repair_packs::folded_fingerprint(fixture, &projected, source);
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
            fingerprint,
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
    .expect("publication");
}

/// The review's three-session case (REV-CX-M03-T19 finding 2), for charges
/// and samples: source charge B (-15 to +5 min, a sample at -10) is packed
/// first. The Hub then covers +10 to +60 with charge H, and the source adds
/// charge A (0 to +20): B overlaps A, A overlaps H, and H wins the group. B
/// leaves the clients and its packed sample follows H at once, on a fresh
/// and on an existing client; an unchanged replay takes the fingerprint path
/// and a forced one publishes nothing.
#[test]
fn seam_repair_packs_transitive_overlap_moves_a_packed_sample_at_once() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let (b, a, h) = (25, 26, HUB_ID_BASE + 25);
    let first = charge_history(
        vec![source_process(b, T - 15 * MINUTE, T + 5 * MINUTE)],
        vec![source_sample(2501, b, T - 10 * MINUTE)],
    );
    import(&fixture, &first, T - 30 * MINUTE);
    assert_eq!(charge_client(&fixture).samples.get(&2501), Some(&(b, T - 10 * MINUTE)));
    let existing_at = charge_heads(&fixture.store, vehicle_id).len();

    hub_charges(
        &fixture,
        (1, T + 10 * MINUTE, T + HOUR),
        &[(h, T + 10 * MINUTE, T + HOUR)],
        &[(HUB_ID_BASE + 2501, h, T + 30 * MINUTE)],
        &[],
    );
    let second = charge_history(
        vec![
            source_process(b, T - 15 * MINUTE, T + 5 * MINUTE),
            source_process(a, T, T + 20 * MINUTE),
        ],
        vec![
            source_sample(2501, b, T - 10 * MINUTE),
            source_sample(2601, a, T + 5 * MINUTE),
        ],
    );
    let source = Sha256Digest::of_bytes(b"seam-repair-packs three charges");
    import_through_fingerprint(&fixture, &second, source, T + 3 * HOUR);

    let fresh = charge_client(&fixture);
    fresh.assert_each_charge_once("fresh client");
    assert!(fresh.charges.contains_key(&h), "H won the group");
    assert!(
        !fresh.charges.contains_key(&b) && !fresh.charges.contains_key(&a),
        "B and A lost to H: {:?}",
        fresh.charges
    );
    assert_eq!(
        fresh.samples.get(&2501),
        Some(&(h, T - 10 * MINUTE)),
        "B's packed sample follows H at once"
    );
    assert_eq!(fresh.samples.get(&2601), Some(&(h, T + 5 * MINUTE)));
    assert_eq!(fresh.charges, store_charges(&fixture.store, vehicle_id));
    let all = charge_heads(&fixture.store, vehicle_id);
    let mut existing = ChargeClient::default();
    for rows in &all[..existing_at] {
        existing.apply(rows);
    }
    for (index, rows) in all[existing_at..].iter().enumerate() {
        existing.apply(rows);
        existing.assert_each_charge_once(&format!("existing client at head {}", existing_at + index));
    }
    assert_eq!(existing, fresh, "the existing client ends where a fresh one does");

    let projected = project_vehicle(&second, CAR).expect("projection");
    assert!(
        fixture
            .store
            .source_fingerprint_matches(
                vehicle_id,
                super::super::seam_repair_packs::folded_fingerprint(&fixture, &projected, source)
            )
            .expect("fingerprint"),
        "an unchanged replay takes the fingerprint path and publishes nothing"
    );
    assert_replay_publishes_nothing(&fixture, &second, T + 4 * HOUR);
    assert_eq!(charge_client(&fixture), fresh, "nothing was left for a later replay");
}
