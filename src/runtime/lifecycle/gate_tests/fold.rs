// SPDX-License-Identifier: AGPL-3.0-only

//! The gate fold (C2 R74 (b)): one charging session's gate facts, folded one
//! sample at a time in sample order. Test builds only: the measurement is not a
//! product feature (C2 section 17.0).
//!
//! The fold calls the Hub's own functions instead of restating them: C2 R26's
//! scratch `OpenCharge` (baseline `None`) is folded by `observe_charge_sample`,
//! and an `EfficiencyFold` (C2 R71) gives the figure. What the Hub does not
//! publish (timing, power, phase, counter and temperature facts) is kept in
//! fixed size: at most [`K_GATE_TYPE_TEXTS`] type texts and an overflow count,
//! whatever the session's length (C2 R74 (c)). The fold reads nothing but its
//! arguments (C2 R2).

use super::*;

/// The most distinct `fast_charger_type` texts one session's facts keep
/// (C2 R74 (b)).
pub(crate) const K_GATE_TYPE_TEXTS: usize = 8;

/// Smallest `V * I`, in W, at which a sample counts toward the phase-set
/// statistic of gate G1 (C2 section 11.1 G1: at least 3 kW).
const K_GATE_PHASE_MIN_VA: f64 = 3_000.0;

/// One counter step, in kWh, added to the physical bound of gate G4
/// (C2 section 11.1 G4 (b), C2 R76 (g)): the bound is
/// `(P + K_PHASE_ABS_KW) * dt / 3,600,000 + 0.01` kWh.
pub(super) const K_GATE_COUNTER_STEP_KWH: f64 = 0.01;

/// One usable counter reading (C2 R74 (b)): its time, the counter and the
/// sample's usable power, `None` when it has none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CounterPoint {
    pub timestamp_ms: i64,
    pub counter_kwh: f64,
    pub power_kw: Option<f64>,
}

/// One session's gate facts (C2 R74 (b)).
///
/// Fold fields come from the samples alone; the derived fields below them are
/// filled by [`ChargeGateFold::finish`] from C2 R26's scratch state and the
/// efficiency fold.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChargeGateFacts {
    pub samples: u64,
    pub first_sample_ms: Option<i64>,
    pub last_sample_ms: Option<i64>,
    pub intervals: u64,
    pub interval_ms_sum: i64,
    pub long_intervals: u64,
    pub max_power_kw: Option<f64>,
    pub powered_samples: u64,
    pub powered_off_ac_voltage: u64,
    pub dc_name_samples: u64,
    pub type_values: Vec<(String, u64)>,
    pub type_values_overflow: u64,
    pub tesla_brand: bool,
    pub other_brand: bool,
    pub phase_eligible: u64,
    pub phase_unique: u64,
    pub phase_empty: u64,
    pub phase_multi: u64,
    pub phase_label_differs: u64,
    pub usable_counters: u64,
    pub first_counter: Option<CounterPoint>,
    pub second_counter: Option<CounterPoint>,
    pub last_counter: Option<CounterPoint>,
    pub recovered_decreases: u64,
    /// Usable counters below the previous usable counter that C2 R76's test
    /// reads as a dip (`counter_restarted` false).
    pub tolerated_dips: u64,
    /// Those it reads as a restart (`counter_restarted` true).
    pub restart_reads: u64,
    /// `recovered_decreases`' pending rule with the pending value set only at
    /// a restart read.
    pub recovered_restarts: u64,
    /// Restart reads whose new counter exceeds the physical bound of G4 (b):
    /// `(p_max + K_PHASE_ABS_KW) * dt / 3,600,000 + 0.01` kWh, `dt` the time
    /// from the sample of the previous usable counter and `p_max` the larger
    /// usable power of those two samples.
    pub restart_reads_above_bound: u64,
    /// Restart reads with no usable power on either of the two samples.
    pub restart_reads_unbounded: u64,
    /// The largest usable counter of the session.
    pub largest_counter_kwh: Option<f64>,
    pub outside_temp_mean_c: Option<f64>,
    // Filled by `finish`.
    pub is_dc: Option<bool>,
    pub session_type: Option<String>,
    pub used_powered_kwh: Option<f64>,
    pub ratio_used_kwh: f64,
    pub ratio_added_kwh: f64,
    pub fully_powered: bool,
    pub energy: ChargeEnergyTotals,
    pub efficiency_wh_per_km: Option<f64>,
    /// The final decline `m - l` of C2 R76 (i): the segment maximum less the
    /// last usable counter, 0.0 when the last usable counter is the maximum.
    pub final_decline_kwh: f64,
}

impl ChargeGateFacts {
    /// Facts of a session with no sample.
    pub(crate) fn empty() -> Self {
        Self {
            samples: 0,
            first_sample_ms: None,
            last_sample_ms: None,
            intervals: 0,
            interval_ms_sum: 0,
            long_intervals: 0,
            max_power_kw: None,
            powered_samples: 0,
            powered_off_ac_voltage: 0,
            dc_name_samples: 0,
            type_values: Vec::new(),
            type_values_overflow: 0,
            tesla_brand: false,
            other_brand: false,
            phase_eligible: 0,
            phase_unique: 0,
            phase_empty: 0,
            phase_multi: 0,
            phase_label_differs: 0,
            usable_counters: 0,
            first_counter: None,
            second_counter: None,
            last_counter: None,
            recovered_decreases: 0,
            tolerated_dips: 0,
            restart_reads: 0,
            recovered_restarts: 0,
            restart_reads_above_bound: 0,
            restart_reads_unbounded: 0,
            largest_counter_kwh: None,
            outside_temp_mean_c: None,
            is_dc: None,
            session_type: None,
            used_powered_kwh: None,
            ratio_used_kwh: 0.0,
            ratio_added_kwh: 0.0,
            fully_powered: false,
            energy: ChargeEnergyTotals {
                added_kwh: None,
                used_kwh: None,
            },
            efficiency_wh_per_km: None,
            final_decline_kwh: 0.0,
        }
    }
}

/// What one sample's brand says (C2 R78 (c), the rule gate G6 uses): the brand
/// is trimmed and compared ASCII case-insensitively. `Tesla` is Tesla; absent,
/// empty, `<invalid>`, `Unknown` and `SNA` carry no brand; any other text is
/// another operator's brand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleBrand {
    Tesla,
    Other,
    NoBrand,
}

fn sample_brand(sample: &ProjectionChargeSample) -> SampleBrand {
    let Some(text) = sample.fast_charger_brand.as_deref().map(str::trim) else {
        return SampleBrand::NoBrand;
    };
    let is = |name: &str| text.eq_ignore_ascii_case(name);
    if is("Tesla") {
        SampleBrand::Tesla
    } else if text.is_empty() || is("<invalid>") || is("Unknown") || is("SNA") {
        SampleBrand::NoBrand
    } else {
        SampleBrand::Other
    }
}

/// The incremental gate fold of one session (C2 R74 (b)).
pub(crate) struct ChargeGateFold {
    scratch: OpenCharge,
    efficiency: EfficiencyFold,
    facts: ChargeGateFacts,
    previous_ms: Option<i64>,
    pending_decrease: Option<f64>,
    pending_restart: Option<f64>,
    temp_sum: f64,
    temp_count: u64,
}

impl ChargeGateFold {
    pub(crate) fn new() -> Self {
        Self {
            scratch: OpenCharge::empty(0, 0, 0),
            efficiency: EfficiencyFold::new(),
            facts: ChargeGateFacts::empty(),
            previous_ms: None,
            pending_decrease: None,
            pending_restart: None,
            temp_sum: 0.0,
            temp_count: 0,
        }
    }

    /// Fold one sample; a session's samples arrive in sample order.
    pub(crate) fn observe(&mut self, sample: &ProjectionChargeSample) {
        self.observe_timing(sample);
        self.observe_power(sample);
        self.observe_type_and_brand(sample);
        self.observe_phases(sample);
        self.observe_counter(sample);
        if let Some(temp) = finite_reading(sample.outside_temp_c) {
            self.temp_sum += temp;
            self.temp_count += 1;
        }
        observe_charge_sample(&mut self.scratch, sample);
        self.efficiency.observe(sample);
    }

    /// The session's facts so far plus the derived fields.
    pub(crate) fn finish(&self) -> ChargeGateFacts {
        let mut facts = self.facts.clone();
        facts.outside_temp_mean_c =
            (self.temp_count > 0).then(|| self.temp_sum / self.temp_count as f64);
        facts.is_dc = self.scratch.is_dc;
        facts.session_type = self.scratch.fast_charger_type.clone();
        facts.used_powered_kwh = self.scratch.energy_used_kwh;
        facts.ratio_used_kwh = self.scratch.ratio_used_kwh;
        facts.ratio_added_kwh = self.scratch.ratio_added_kwh;
        facts.fully_powered = self.scratch.fully_powered;
        facts.energy = charge_energy_totals(&self.scratch);
        facts.efficiency_wh_per_km = self.efficiency.finish();
        facts.final_decline_kwh = self
            .scratch
            .max_energy_added
            .zip(self.scratch.last_energy_added)
            .map_or(0.0, |(maximum, last)| (maximum - last).max(0.0));
        facts
    }

    /// Sample count, first and last time, and the intervals with `dt > 0` (C2 R17).
    fn observe_timing(&mut self, sample: &ProjectionChargeSample) {
        let facts = &mut self.facts;
        facts.samples += 1;
        facts.first_sample_ms.get_or_insert(sample.timestamp_ms);
        facts.last_sample_ms = Some(sample.timestamp_ms);
        if let Some(previous) = self.previous_ms {
            let dt_ms = sample.timestamp_ms.saturating_sub(previous);
            if dt_ms > 0 {
                facts.intervals += 1;
                facts.interval_ms_sum = facts.interval_ms_sum.saturating_add(dt_ms);
                if dt_ms > K_INT_MAX_MS {
                    facts.long_intervals += 1;
                }
            }
        }
        self.previous_ms = Some(sample.timestamp_ms);
    }

    /// The largest usable power, the powered samples (`P >= 1`) and, of those,
    /// the ones whose voltage is unknown or outside the per-phase range.
    fn observe_power(&mut self, sample: &ProjectionChargeSample) {
        let Some(power) = usable_charger_power_kw(sample) else {
            return;
        };
        let facts = &mut self.facts;
        facts.max_power_kw = Some(
            facts
                .max_power_kw
                .map_or(power, |maximum| maximum.max(power)),
        );
        if power >= K_AC_EVIDENCE_MIN_KW {
            facts.powered_samples += 1;
            let in_range = finite_reading(sample.charger_voltage)
                .is_some_and(|voltage| (K_V_MIN..=K_V_MAX).contains(&voltage));
            if !in_range {
                facts.powered_off_ac_voltage += 1;
            }
        }
    }

    /// DC-named samples, the distinct trimmed type texts (at most eight, with
    /// an overflow count) and the brand flags.
    fn observe_type_and_brand(&mut self, sample: &ProjectionChargeSample) {
        let facts = &mut self.facts;
        if matches!(
            fast_charger_kind(sample.fast_charger_type.as_deref()),
            FastChargerKind::Supercharger | FastChargerKind::OtherDc
        ) {
            facts.dc_name_samples += 1;
        }
        let text = sample
            .fast_charger_type
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        if let Some(text) = text {
            if let Some(entry) = facts.type_values.iter_mut().find(|(kept, _)| kept == text) {
                entry.1 += 1;
            } else if facts.type_values.len() < K_GATE_TYPE_TEXTS {
                facts.type_values.push((text.to_owned(), 1));
            } else {
                facts.type_values_overflow += 1;
            }
        }
        match sample_brand(sample) {
            SampleBrand::Tesla => facts.tesla_brand = true,
            SampleBrand::Other => facts.other_brand = true,
            SampleBrand::NoBrand => {}
        }
    }

    /// The G1 statistic: samples with a phase set and `V * I >= 3 kW`, split by
    /// the set's size, and the single element against a known label.
    fn observe_phases(&mut self, sample: &ProjectionChargeSample) {
        let Some(set) = phase_consistency_set(sample) else {
            return;
        };
        let apparent = finite_reading(sample.charger_voltage)
            .zip(finite_reading(sample.charger_actual_current))
            .map(|(voltage, current)| voltage * current);
        if !apparent.is_some_and(|apparent| apparent >= K_GATE_PHASE_MIN_VA) {
            return;
        }
        let facts = &mut self.facts;
        facts.phase_eligible += 1;
        match set.len() {
            0 => facts.phase_empty += 1,
            1 => facts.phase_unique += 1,
            _ => facts.phase_multi += 1,
        }
        let label = sample
            .charger_phases
            .filter(|label| (1..=3).contains(label));
        if let (Some(only), Some(label)) = (set.only(), label)
            && only != label
        {
            facts.phase_label_differs += 1;
        }
    }

    /// Usable counters: the first, second and last, the largest, and the
    /// decreases below the previous usable counter (C2 R74 (b), C2 R76 (g)).
    ///
    /// A decrease is a dip or a restart by C2 R76's one test
    /// (`counter_restarted`). `recovered_decreases` keeps a pending value set at
    /// any decrease, and the next usable counter at or above it counts one
    /// recovery; `recovered_restarts` is the same rule with the pending value
    /// set only at a restart read. A restart read is also held against the
    /// physical bound of what a restarted counter can show after the time
    /// since the previous usable counter: above it, the fall was a glitch or a
    /// decline the tolerance missed; with no usable power on either sample,
    /// the bound cannot be computed and the read is left undecided.
    fn observe_counter(&mut self, sample: &ProjectionChargeSample) {
        let Some(counter) = usable_energy_counter(sample) else {
            return;
        };
        let point = CounterPoint {
            timestamp_ms: sample.timestamp_ms,
            counter_kwh: counter,
            power_kw: usable_charger_power_kw(sample),
        };
        let facts = &mut self.facts;
        let previous = facts.last_counter;
        facts.usable_counters += 1;
        match facts.usable_counters {
            1 => facts.first_counter = Some(point),
            2 => facts.second_counter = Some(point),
            _ => {}
        }
        facts.last_counter = Some(point);
        facts.largest_counter_kwh = Some(
            facts
                .largest_counter_kwh
                .map_or(counter, |largest| largest.max(counter)),
        );
        if let Some(pending) = self.pending_decrease.take()
            && counter >= pending
        {
            facts.recovered_decreases += 1;
        }
        if let Some(pending) = self.pending_restart.take()
            && counter >= pending
        {
            facts.recovered_restarts += 1;
        }
        let Some(previous) = previous.filter(|previous| counter < previous.counter_kwh) else {
            return;
        };
        self.pending_decrease = Some(previous.counter_kwh);
        if !counter_restarted(previous.counter_kwh, counter) {
            facts.tolerated_dips += 1;
            return;
        }
        facts.restart_reads += 1;
        self.pending_restart = Some(previous.counter_kwh);
        let strongest = previous
            .power_kw
            .into_iter()
            .chain(point.power_kw)
            .reduce(f64::max);
        match strongest {
            None => facts.restart_reads_unbounded += 1,
            Some(power) => {
                let dt_ms = point.timestamp_ms.saturating_sub(previous.timestamp_ms);
                let bound = (power + K_PHASE_ABS_KW) * (dt_ms as f64) / MS_PER_HOUR
                    + K_GATE_COUNTER_STEP_KWH;
                if counter > bound {
                    facts.restart_reads_above_bound += 1;
                }
            }
        }
    }
}
