// SPDX-License-Identifier: AGPL-3.0-only

//! Shared builders of the gate tests. Synthetic data only. `T0` is
//! 1_760_000_000_000 ms.
//!
//! `b(minutes)` builds one `ProjectionChargeSample` at `T0 + minutes` whose
//! unspecified fields are `None` (the spec's `S(t, {...})`); the chained setters
//! fill fields. `a7` is the spec's AC sample `A7`: 7 kW reported at 240 V and
//! 30 A on one phase, which C2 R35 row 1 reads as 7.2 kW.

use super::fold::{ChargeGateFacts, ChargeGateFold};
use super::report::ProcessGateFacts;
use super::*;

pub(super) const T0: i64 = 1_760_000_000_000;
pub(super) const MIN: i64 = 60_000;
pub(super) const DAY: i64 = 86_400_000;

/// A sample builder over `ProjectionChargeSample`.
#[derive(Clone)]
pub(super) struct B(ProjectionChargeSample);

pub(super) fn b_ms(id: i64, at_ms: i64) -> B {
    B(ProjectionChargeSample {
        id,
        charge_process_id: 1,
        timestamp_ms: at_ms,
        battery_level: None,
        usable_battery_level: None,
        charge_energy_added_kwh: None,
        charger_power_kw: None,
        charger_voltage: None,
        charger_actual_current: None,
        charger_pilot_current: None,
        charger_phases: None,
        ideal_range_km: None,
        rated_range_km: None,
        outside_temp_c: None,
        battery_heater_on: None,
        battery_heater: None,
        battery_heater_no_power: None,
        not_enough_power_to_heat: None,
        fast_charger_present: None,
        fast_charger_brand: None,
        fast_charger_type: None,
        charge_cable: None,
    })
}

/// A sample `minutes` after `t0`; the id is `minutes + 1`, unique in a series
/// whose minutes differ.
pub(super) fn b(minutes: i64) -> B {
    b_ms(minutes + 1, T0 + minutes * MIN)
}

impl B {
    pub(super) fn c(mut self, kwh: f64) -> Self {
        self.0.charge_energy_added_kwh = Some(kwh);
        self
    }
    pub(super) fn power(mut self, kw: f64) -> Self {
        self.0.charger_power_kw = Some(kw);
        self
    }
    pub(super) fn volts(mut self, volts: f64) -> Self {
        self.0.charger_voltage = Some(volts);
        self
    }
    pub(super) fn amps(mut self, amps: f64) -> Self {
        self.0.charger_actual_current = Some(amps);
        self
    }
    pub(super) fn phases(mut self, phases: i64) -> Self {
        self.0.charger_phases = Some(phases);
        self
    }
    pub(super) fn present(mut self, present: bool) -> Self {
        self.0.fast_charger_present = Some(present);
        self
    }
    pub(super) fn kind(mut self, text: &str) -> Self {
        self.0.fast_charger_type = Some(text.to_owned());
        self
    }
    pub(super) fn brand(mut self, text: &str) -> Self {
        self.0.fast_charger_brand = Some(text.to_owned());
        self
    }
    pub(super) fn temp(mut self, celsius: f64) -> Self {
        self.0.outside_temp_c = Some(celsius);
        self
    }
    pub(super) fn level(mut self, level: i64) -> Self {
        self.0.battery_level = Some(level);
        self
    }
    pub(super) fn rated(mut self, km: f64) -> Self {
        self.0.rated_range_km = Some(km);
        self
    }
    /// The AC sample `A7`: 7 kW at 240 V, 30 A, one phase.
    pub(super) fn a7(self) -> Self {
        self.power(7.0).volts(240.0).amps(30.0).phases(1)
    }
    /// A DC sample: power with the on-board voltage and current of a DC flow.
    pub(super) fn dc(self, kw: f64) -> Self {
        self.power(kw).volts(2.0).amps(0.0)
    }
    pub(super) fn s(self) -> ProjectionChargeSample {
        self.0
    }
}

/// The gate facts of `samples` folded one at a time, in the order given.
pub(super) fn fold_all(samples: &[ProjectionChargeSample]) -> ChargeGateFacts {
    let mut fold = ChargeGateFold::new();
    for sample in samples {
        fold.observe(sample);
    }
    fold.finish()
}

/// One session of car 1: its process id and start, folded from `samples`.
pub(super) fn process(
    process_id: i64,
    start_ms: i64,
    samples: &[ProjectionChargeSample],
) -> ProcessGateFacts {
    ProcessGateFacts {
        car_id: 1,
        process_id,
        start_ms,
        end_ms: samples
            .last()
            .map_or(start_ms, |sample| sample.timestamp_ms),
        facts: fold_all(samples),
    }
}

/// A session whose facts are set directly: AC-selected with kind `Some(false)`
/// and only the named figures (the spec's "facts set directly").
pub(super) fn direct_ac(
    process_id: i64,
    start_ms: i64,
    efficiency_wh_per_km: Option<f64>,
    outside_temp_mean_c: Option<f64>,
) -> ProcessGateFacts {
    let mut facts = ChargeGateFacts::empty();
    facts.max_power_kw = Some(7.0);
    facts.powered_samples = 1;
    facts.is_dc = Some(false);
    facts.efficiency_wh_per_km = efficiency_wh_per_km;
    facts.outside_temp_mean_c = outside_temp_mean_c;
    ProcessGateFacts {
        car_id: 1,
        process_id,
        start_ms,
        end_ms: start_ms,
        facts,
    }
}

/// A figure sample of the efficiency tests: level in per cent, rated range in
/// km and counter in kWh, at `T0 + 10 min x k` with id `k + 1`.
pub(super) fn fig(k: i64, level: i64, rated: f64, counter: f64) -> ProjectionChargeSample {
    b_ms(k + 1, T0 + 10 * MIN * k)
        .level(level)
        .rated(rated)
        .c(counter)
        .s()
}
