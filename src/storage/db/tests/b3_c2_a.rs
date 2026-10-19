// SPDX-License-Identifier: AGPL-3.0-only

//! C2-A store-level test (C2 section 18.1, C2 T49 (d) as revision 7 states it,
//! C2 R76 with C1 R91): a counter that falls beyond the tolerance mid-session is
//! a new Tesla session, so C1 R66 closes the first row and the second row opens
//! with baseline 0.0, its own energy, and the session fee charged again. The
//! tree's earlier C2 T49 (d) test
//! (`t49d_a_counter_reset_split_charges_the_fee_on_the_second_row`, counters
//! 0.00, 2.00, 4.00, 0.50, 1.58) stays unchanged in `cost_close.rs`; this is
//! its second case, with the stated counters and the energy and cost
//! assertions. Synthetic data only; the sequences are built with the spec's
//! `Ob` and `A7` (`src/runtime/lifecycle/energy_tests.rs`).

use super::*;

#[test]
fn c2_t49d_b3_restart_fee_energy() {
    let mut harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    // Row 1 as C2 T49 (a), but with its last observation `Ob(+60m, Charging, c
    // 7.20)`: the charge is still open.
    let mut samples = first_plug_in_row();
    samples.pop();
    samples.push(cost_ob(
        7,
        COST_T0 + 60 * COST_MIN,
        "Charging",
        cost_a7(Some(7.20)),
    ));
    // Then `Ob(+70m, Charging, c 0.30)` and `Ob(+80m, Complete, c 1.38)`, every
    // observation `A7`. The fall from 7.20 to 0.30 is a restart, so C1 R66
    // LoE-closes row 1 at +60m and row 2 opens at +70m with baseline 0.0
    // (C1 R61a). Row 2 carries its own coordinates inside fence 3.
    samples.push(cost_at(
        cost_ob(8, COST_T0 + 70 * COST_MIN, "Charging", cost_a7(Some(0.30))),
        10.0005,
        20.0,
    ));
    samples.push(cost_ob(
        9,
        COST_T0 + 80 * COST_MIN,
        "Complete",
        cost_a7(Some(1.38)),
    ));
    harness.feed_all(&samples);
    let charges = harness.charges();
    assert_eq!(charges.len(), 2, "one row per Tesla session");
    let (first, second) = (&charges[0], &charges[1]);
    assert_eq!(first.end_date_ms, Some(COST_T0 + 60 * COST_MIN));
    assert_eq!(first.charge_energy_added, Some(7.20));
    assert_eq!(first.session_fee, Some(1.00));
    assert_eq!(second.start_date_ms, COST_T0 + 70 * COST_MIN);
    // Baseline 0.0: the whole counter is new. Row 4 of C2 R19: 1.38 x 1.20 / 1.08.
    assert_eq!(second.charge_energy_added, Some(1.38));
    assert_eq!(second.charge_energy_used_kwh, Some(1.53));
    assert_eq!(second.billing_type, Some(kwh()));
    assert_eq!(second.cost_per_unit, Some(0.30));
    // The counter restarted, so this is a new connection: the fee is charged on
    // both rows. 1.53 x 0.30 + 1.00 = 1.459.
    assert_eq!(second.session_fee, Some(1.00));
    assert_eq!(second.cost, Some(1.46));
}
