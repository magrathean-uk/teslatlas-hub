// SPDX-License-Identifier: AGPL-3.0-only

//! C1-A spec tests (C1 section 8a, C1 R91 and C1 R92: C1 ST106 to C1 ST110, with C1 ST112 as an
//! audit): every place the unit reads a lower counter as a new Tesla session
//! calls the one restart test of C2 R76 (`counter_restarted`): C1 R61a's baseline,
//! C1 R65's continuity, C1 R66's close and both directions of C1 R70's condition 2.
//! Synthetic data only. `T0` is 1_760_000_000_000 ms. `V(id, t, state, energy,
//! soc)` of the spec is `cv`: an owner `vehicle_data` whose charge group carries
//! the charging state, the counter and the battery level, and whose vehicle
//! group carries the odometer (1000.0 mi throughout, so the car is unmoved and a
//! gap can be continuous, C1 R65). The storage-level companion of C1 ST107 is in
//! `src/storage/db/tests/b3_c1_a.rs`.
//!
//! C1 ST112 is a review check, not a test: in the Hub, `rg -n K_COUNTER_DIP_KWH
//! src` finds the constant only at its definition and inside
//! `counter_restarted`; `rg -n counter_restarted src` finds, outside tests, its
//! definition, C2's two calls in `fold_energy_counter` and one in
//! `fold_charge_interval`, and C1's calls in `charge_energy_baseline` (C1 R61a),
//! `charge_continues_across_gap` (C1 R65), `charge_close_step` (C1 R66) and two in
//! `after_gap_charge` (C1 R70); and no charge code compares a counter with
//! `last_energy_added`, `max_energy_added` or the snapshot's counter by `<`,
//! `<=`, `>` or `>=` except `fold_energy_counter`'s `counter >= max` and C1 R70's
//! `now > 0.0`.

use serde_json::json;

use super::charge_tests::{HOUR, MIN, SEC, T0, at, cv, parked_after_drive};
use super::*;

const ODOMETER: Option<f64> = Some(1000.0);

fn run(samples: &[LifecycleSample]) -> LifecycleStep {
    apply_samples(OpenSessionState::new(), 1, samples).expect("apply samples")
}

/// `(start, end)` of every closed charge of a delta.
fn spans(delta: &LifecycleDelta) -> Vec<(i64, Option<i64>)> {
    delta
        .charges
        .iter()
        .map(|charge| (charge.start_date_ms, charge.end_date_ms))
        .collect()
}

fn energies(delta: &LifecycleDelta) -> Vec<Option<f64>> {
    delta
        .charges
        .iter()
        .map(|charge| charge.charge_energy_added)
        .collect()
}

/// `V(id, t, state, energy, soc)` at the one odometer reading.
fn v(id: i64, t: i64, state: &str, energy: f64, soc: Option<i64>) -> LifecycleSample {
    cv(id, t, state, Some(energy), soc, ODOMETER)
}

/// The same observation with `charger_power` in the charge group.
fn powered(mut sample: LifecycleSample, kw: i64) -> LifecycleSample {
    sample.payload["provider_raw_json"]["response"]["charge_state"]["charger_power"] = json!(kw);
    sample
}

/// A discovery: no vehicle data, only the reach of the car.
fn disc(id: i64, t: i64, state: &str) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: state.to_owned(),
        payload: json!({
            "record_type": "owner_api_discovery_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": state,
        }),
    }
}

/// C1 ST106's observations: the prior `Disconnected` and five more, the fourth with
/// the counter given.
fn st106(fourth: f64) -> Vec<LifecycleSample> {
    vec![
        v(1, T0 - 10 * MIN, "Disconnected", 8.0, Some(50)),
        v(2, T0, "Charging", 10.00, None),
        v(3, T0 + 20 * SEC, "Charging", 10.04, None),
        v(4, T0 + 40 * SEC, "Charging", fourth, None),
        v(5, T0 + 60 * SEC, "Charging", 10.08, None),
        v(6, T0 + 80 * SEC, "Complete", 10.12, None),
    ]
}

#[test]
fn c1_st106_dip_inside_charge() {
    // A fall of 0.02 (10.04 to 10.02) is a dip: one charge [t0, t0+80s] with five
    // samples, baseline 0.0 (the cable was out), energy 10.12. (Revision 5 split it
    // in two rows, 10.04 and 10.12.)
    let result = run(&st106(10.02));
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 80 * SEC))]);
    assert_eq!(result.delta.open_charge_samples.len(), 5);
    assert_eq!(energies(&result.delta), vec![Some(10.12)]);
    let opened = run(&st106(10.02)[..2]);
    let open = opened.state.open_charge.as_ref().expect("charge is open");
    assert_eq!(
        open.first_energy_added,
        Some(0.0),
        "baseline 0.0: unplugged"
    );
    // A fall of exactly 0.05 (10.04 to 9.99) is still a dip: the same rows.
    let edge = run(&st106(9.99));
    assert_eq!(spans(&edge.delta), vec![(T0, Some(T0 + 80 * SEC))]);
    assert_eq!(edge.delta.open_charge_samples.len(), 5);
    assert_eq!(energies(&edge.delta), vec![Some(10.12)]);
    // A fall of 0.06 (10.04 to 9.98) is a restart: two rows, [t0, t0+20s] with
    // energy 10.04 and [t0+40s, t0+80s] with baseline 0.0 (a restart against the
    // snapshot's 10.04) and energy 10.12.
    let split = run(&st106(9.98));
    assert_eq!(
        spans(&split.delta),
        vec![
            (T0, Some(T0 + 20 * SEC)),
            (T0 + 40 * SEC, Some(T0 + 80 * SEC))
        ]
    );
    assert_eq!(energies(&split.delta), vec![Some(10.04), Some(10.12)]);
    let reopened = run(&st106(9.98)[..4]);
    let open = reopened.state.open_charge.as_ref().expect("second row");
    assert_eq!(open.first_energy_added, Some(0.0), "restart against 10.04");
    assert_eq!(open.id, 2);
    // The restart observation is sampled once, in the new row.
    assert_eq!(split.delta.open_charge_samples.len(), 5);
    assert_eq!(split.delta.open_charge_samples[2].charge_process_id, 2);
}

/// C1 ST107's observations at `T0`: the top-up that continues the plug-in across
/// the measured boundary dip, at 8 kW.
fn st107() -> Vec<LifecycleSample> {
    vec![
        v(1, T0 - 30 * MIN, "Disconnected", 40.0, Some(50)),
        powered(v(2, T0 - 20 * MIN, "Charging", 50.70, None), 8),
        powered(v(3, T0 - 10 * MIN, "Charging", 50.80, None), 8),
        powered(v(4, T0, "Complete", 50.86, None), 8),
        powered(v(5, T0 + 121 * SEC, "Charging", 50.84, None), 8),
        powered(v(6, T0 + 126 * SEC, "Charging", 50.88, None), 8),
        powered(v(7, T0 + 10 * MIN, "Complete", 51.20, None), 8),
    ]
}

#[test]
fn c1_st107_boundary_dip_topup() {
    // Row 1 [t0-20m, t0] with baseline 0.0 and energy 50.86. Row 2 [t0+121s,
    // t0+10m] has baseline 50.86 (no unplug, drive or movement, and 50.84 is a dip
    // against 50.86) and energy 0.34. (Revision 5 gave row 2 baseline 0.0 and
    // energy 51.20.)
    let result = run(&st107());
    assert_eq!(
        spans(&result.delta),
        vec![
            (T0 - 20 * MIN, Some(T0)),
            (T0 + 121 * SEC, Some(T0 + 10 * MIN))
        ]
    );
    assert_eq!(energies(&result.delta), vec![Some(50.86), Some(0.34)]);
    let second = run(&st107()[..5]);
    let open = second.state.open_charge.as_ref().expect("row 2 is open");
    assert_eq!(open.id, 2);
    assert_eq!(open.first_energy_added, Some(50.86), "baseline 50.86");
    // The rows are one plug-in: three samples in row 1 and three in row 2, and
    // every sample is in the row that was open when it arrived.
    let rows = result
        .delta
        .open_charge_samples
        .iter()
        .map(|sample| sample.charge_process_id)
        .collect::<Vec<_>>();
    assert_eq!(rows, vec![1, 1, 1, 2, 2, 2]);
    // A restart against 50.86 starts from zero again: 50.80 is 0.06 below.
    let mut restarted = st107();
    restarted[4] = powered(v(5, T0 + 121 * SEC, "Charging", 50.80, None), 8);
    let result = run(&restarted);
    assert_eq!(energies(&result.delta), vec![Some(50.86), Some(51.20)]);
    // And exactly 0.05 below (50.81) is a dip again.
    restarted[4] = powered(v(5, T0 + 121 * SEC, "Charging", 50.81, None), 8);
    let result = run(&restarted);
    assert_eq!(energies(&result.delta), vec![Some(50.86), Some(0.34)]);
}

#[test]
fn c1_st108_gap_dip_continuity() {
    let gap = |third: f64| {
        vec![
            v(1, T0, "Charging", 2.00, Some(40)),
            disc(2, T0 + MIN, "offline"),
            v(3, T0 + 40 * MIN, "Complete", third, Some(41)),
        ]
    };
    // A fall of 0.02 across the gap is a dip, so continuity holds: one charge
    // [t0, t0+40m] with two samples and the maximum 2.00 as its energy. (Revision
    // 5 closed the charge at t0 and recorded a second one, [t0, t0+40m], with
    // baseline 0.0 and energy 1.98.)
    let result = run(&gap(1.98));
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 40 * MIN))]);
    assert_eq!(result.delta.open_charge_samples.len(), 2);
    assert_eq!(energies(&result.delta), vec![Some(2.00)]);
    // A fall of 0.06 (2.00 to 1.94) is a restart: continuity fails, the first
    // charge ends at t0 with one sample and energy 2.00, and C1 R70 records a second
    // charge [t0, t0+40m] with one sample, start battery 40, baseline 0.0 (a
    // restart against the snapshot's 2.00) and energy 1.94.
    let result = run(&gap(1.94));
    assert_eq!(
        spans(&result.delta),
        vec![(T0, Some(T0)), (T0, Some(T0 + 40 * MIN))]
    );
    assert_eq!(energies(&result.delta), vec![Some(2.00), Some(1.94)]);
    assert_eq!(result.delta.open_charge_samples.len(), 2);
    assert_eq!(result.delta.charges[0].end_battery_level, Some(40));
    assert_eq!(result.delta.charges[1].start_battery_level, Some(40));
    // A fall of exactly 0.05 (2.00 to 1.95) is a dip: continuity holds.
    let result = run(&gap(1.95));
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 40 * MIN))]);
    assert_eq!(energies(&result.delta), vec![Some(2.00)]);
}

#[test]
fn c1_st109_after_gap_tolerance_band() {
    // C1 ST61's drive: it closes at t0 leaving a snapshot with battery 40, counter
    // 5.00 and odometer 1000.0 mi, and the car goes offline; then the first
    // sample after 9 hours arrives with the counter and battery given.
    let after = |counter: f64, soc: i64| {
        let mut samples = parked_after_drive();
        samples.push(at(
            v(3, T0 + 9 * HOUR, "Complete", counter, Some(soc)),
            10.01,
            20.0,
        ));
        run(&samples)
    };
    // A fall of 0.02 (a dip) and a rise of 0.02 (within the tolerance): no
    // charge, although the battery rose by a point. (Revision 5 recorded one
    // charge with baseline 0.0; revision 6 did the same for the rise.)
    for counter in [4.98, 5.02] {
        let result = after(counter, 41);
        assert_eq!(result.delta.drives.len(), 1, "{counter}");
        assert!(result.delta.charges.is_empty(), "{counter}: no charge");
        assert!(result.delta.open_charge_samples.is_empty(), "{counter}");
        assert!(result.state.open_charge.is_none(), "{counter}");
    }
    // The edge of the band is inclusive: 0.05 below and 0.05 above record
    // nothing.
    for counter in [4.95, 5.05] {
        assert!(after(counter, 41).delta.charges.is_empty(), "{counter}");
    }
    // A rise of 0.06 (beyond the tolerance): one charge [t0, +9h], one sample,
    // start battery 40, coordinates from the observation, baseline 0.0 (after the
    // drive) and energy 5.06.
    let result = after(5.06, 41);
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 9 * HOUR))]);
    assert_eq!(result.delta.open_charge_samples.len(), 1);
    let charge = &result.delta.charges[0];
    assert_eq!(charge.start_battery_level, Some(40));
    assert_eq!(
        (charge.start_latitude, charge.start_longitude),
        (Some(10.01), Some(20.0))
    );
    assert_eq!(charge.charge_energy_added, Some(5.06));
    // A restart against 5.00 (4.90 with the battery at 85): one charge [t0, +9h],
    // baseline 0.0 (after the drive) and energy 4.90.
    let result = after(4.90, 85);
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 9 * HOUR))]);
    assert_eq!(energies(&result.delta), vec![Some(4.90)]);
    // A restart of exactly 0.06 with a higher battery also records the charge.
    let result = after(4.94, 85);
    assert_eq!(energies(&result.delta), vec![Some(4.94)]);
}

#[test]
fn c1_st110_replug() {
    // Row 1 has no snapshot (baseline 0.0) and energy 20.00. The cable is then
    // unplugged and replugged in place: row 2 has baseline 0.0 (the snapshot says
    // `unplugged`) and energy 21.00, although 20.30 is no restart against 20.00.
    // (With C1 R92's branch removed row 2 would have baseline 20.00 and energy 1.00;
    // this test changes then.)
    let samples = vec![
        v(1, T0 - 2 * HOUR, "Charging", 0.50, Some(40)),
        v(2, T0 - HOUR, "Complete", 20.00, Some(70)),
        v(3, T0 - 30 * MIN, "Disconnected", 20.00, Some(70)),
        v(4, T0, "Charging", 20.30, Some(70)),
        v(5, T0 + 30 * MIN, "Complete", 21.00, Some(72)),
    ];
    let result = run(&samples);
    assert_eq!(
        spans(&result.delta),
        vec![(T0 - 2 * HOUR, Some(T0 - HOUR)), (T0, Some(T0 + 30 * MIN))]
    );
    assert_eq!(energies(&result.delta), vec![Some(20.00), Some(21.00)]);
    let opened = run(&samples[..4]);
    let open = opened.state.open_charge.as_ref().expect("row 2 is open");
    assert_eq!(
        open.first_energy_added,
        Some(0.0),
        "baseline 0.0: unplugged"
    );
    // The unplug itself records no charge: the counter is unchanged.
    let parked = run(&samples[..3]);
    assert_eq!(parked.delta.charges.len(), 1);
    assert!(parked.state.parked_snapshot.expect("snapshot").unplugged);
}
