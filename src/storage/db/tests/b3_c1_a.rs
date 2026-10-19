// SPDX-License-Identifier: AGPL-3.0-only

//! C1-A store-level companion of C1 ST107 (C1 section 8a, C1 R91 with C1 R61a
//! and C2 R44a): a top-up across the measured boundary dip, through `HubStore`.
//! The counter dips by 0.02 kWh between two rows of one plug-in, so the second row
//! is not a new Tesla session: its baseline is the first row's last counter, its
//! energy is the growth since then, and the session fee is charged once, on the
//! first row. Synthetic data only.

use super::*;

#[test]
fn c1_st107_boundary_dip_topup() {
    let mut harness = CostHarness::new();
    // Fence 1 centred at (10.0, 20.0) with radius 50 m, per kWh at 0.30 with a
    // session fee of 1.00; every observation is at the centre.
    harness
        .store
        .save_geofence(
            harness.vehicle.vehicle_id,
            Some(1),
            crate::teslamate_projection::TeslaMateGeofence {
                id: 0,
                name: "F1".to_owned(),
                latitude: Some(10.0),
                longitude: Some(20.0),
                radius_m: Some(50.0),
                billing_type: Some(kwh()),
                cost_per_unit: Some(0.30),
                session_fee: Some(1.00),
            },
        )
        .expect("fence");
    let observe = |id: i64, offset_ms: i64, state: &str, fields: serde_json::Value| {
        cost_at(cost_ob(id, COST_T0 + offset_ms, state, fields), 10.0, 20.0)
    };
    let charging =
        |counter: f64| serde_json::json!({"charge_energy_added": counter, "charger_power": 8});
    // The prior `Disconnected` leaves a snapshot with the cable out, so row 1's
    // baseline is 0.0.
    let samples = [
        observe(
            1,
            -30 * COST_MIN,
            "Disconnected",
            serde_json::json!({"charge_energy_added": 40.0, "battery_level": 50}),
        ),
        observe(2, -20 * COST_MIN, "Charging", charging(50.70)),
        observe(3, -10 * COST_MIN, "Charging", charging(50.80)),
        observe(4, 0, "Complete", charging(50.86)),
        // Row 2: the counter dips from 50.86 to 50.84 and rises again to 50.88.
        observe(5, 121_000, "Charging", charging(50.84)),
        observe(6, 126_000, "Charging", charging(50.88)),
        observe(7, 10 * COST_MIN, "Complete", charging(51.20)),
    ];
    harness.feed_all(&samples);
    let charges = harness.charges();
    assert_eq!(charges.len(), 2);
    let (first, second) = (&charges[0], &charges[1]);
    // Row 1: [t0-20m, t0], baseline 0.0, energy 50.86, the fence's tariff with the
    // session fee.
    assert_eq!(first.start_date_ms, COST_T0 - 20 * COST_MIN);
    assert_eq!(first.end_date_ms, Some(COST_T0));
    assert_eq!(first.charge_energy_added, Some(50.86));
    assert_eq!(first.geofence.as_deref(), Some("F1"));
    assert_eq!(
        (first.billing_type, first.cost_per_unit, first.session_fee),
        (Some(kwh()), Some(0.30), Some(1.00))
    );
    // Row 2: [t0+121s, t0+10m], baseline 50.86 (no unplug, drive or movement,
    // and 50.84 is a dip against 50.86), energy 0.34. It continues row 1's
    // plug-in (C2 R44a: its largest counter 51.20 less its energy 0.34 is the
    // kept baseline 50.86), so the fee is waived: the copy is (per_kwh, 0.30,
    // 0.00). (Revision 5 gave row 2 baseline 0.0 and energy 51.20.)
    assert_eq!(second.start_date_ms, COST_T0 + 121_000);
    assert_eq!(second.end_date_ms, Some(COST_T0 + 10 * COST_MIN));
    assert_eq!(second.charge_energy_added, Some(0.34));
    assert_eq!(second.geofence.as_deref(), Some("F1"));
    assert_eq!(
        (
            second.billing_type,
            second.cost_per_unit,
            second.session_fee
        ),
        (Some(kwh()), Some(0.30), Some(0.00))
    );
}
