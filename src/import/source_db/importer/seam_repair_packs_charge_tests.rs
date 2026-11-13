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
