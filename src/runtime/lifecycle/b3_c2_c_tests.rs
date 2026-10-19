// SPDX-License-Identifier: AGPL-3.0-only

//! C2-C lifecycle-level spec tests (C2 section 18.2, C2 R81 with its aggregate
//! extension; C2 T88, C2 T89 and the C2 RQ-A2 equivalence): the session charger
//! kind and type of a charging session are one fold, `fold_charge_session_kind`,
//! shared by the live and seeded observer and by `charge_session_kind`; the
//! display aggregates (C2 R27) are one fold, `fold_charge_display_aggregates`,
//! shared by the live observer, the seed and `charge_display_aggregates`. The
//! store-level tests (C2 T85 to C2 T87 and C2 T90) are in
//! `src/storage/db/tests/b3_c2_c.rs`. Synthetic data only. `T0` is
//! 1_760_000_000_000 ms.

use super::*;

const T0: i64 = 1_760_000_000_000;

/// A sample builder over `ProjectionChargeSample`: every field empty.
fn sample(id: i64, timestamp_ms: i64) -> ProjectionChargeSample {
    ProjectionChargeSample {
        id,
        charge_process_id: 1,
        timestamp_ms,
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
    }
}

/// What one sequence sample says (C2 T88's notation): the charger type text, the
/// charger-present flag, and the `A7` electrical set (7 kW at 240 V, 30 A, one
/// phase) or a bare power.
#[derive(Clone, Copy, Default)]
struct Spec {
    kind: Option<&'static str>,
    present: Option<bool>,
    power: Option<f64>,
    a7: bool,
}

const fn typed(kind: &'static str) -> Spec {
    Spec {
        kind: Some(kind),
        present: None,
        power: None,
        a7: false,
    }
}

/// The samples of a sequence at `T0 + 10 s * k` with id `k + 1`, in list order.
fn sequence(specs: &[Spec]) -> Vec<ProjectionChargeSample> {
    specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let k = index as i64;
            let mut sample = sample(k + 1, T0 + 10_000 * k);
            sample.fast_charger_type = spec.kind.map(str::to_owned);
            sample.fast_charger_present = spec.present;
            sample.charger_power_kw = spec.power;
            if spec.a7 {
                sample.charger_power_kw = Some(7.0);
                sample.charger_voltage = Some(240.0);
                sample.charger_actual_current = Some(30.0);
                sample.charger_phases = Some(1);
            }
            sample
        })
        .collect()
}

/// `(is_dc, fast_charger_type)` of the live and seeded observer's own scratch
/// charge after the samples (C2 R26's scratch: `OpenCharge::empty(0, 0, 0)`).
fn observed_kind(samples: &[ProjectionChargeSample]) -> (Option<bool>, Option<String>) {
    let mut open = OpenCharge::empty(0, 0, 0);
    for sample in samples {
        observe_charge_sample(&mut open, sample);
    }
    (open.is_dc, open.fast_charger_type)
}

#[test]
fn c2_t88_session_kind_cases() {
    let invalid = typed("<invalid>");
    let present = |kind: Option<&'static str>, power: Option<f64>| Spec {
        kind,
        present: Some(true),
        power,
        a7: false,
    };
    let absent_flag = Spec {
        present: Some(false),
        power: Some(7.0),
        ..Spec::default()
    };
    let bare_power = |kw: f64| Spec {
        power: Some(kw),
        ..Spec::default()
    };
    let a7_typed = |kind: &'static str| Spec {
        kind: Some(kind),
        a7: true,
        ..Spec::default()
    };
    // The fourteen fixed sequences of C2 T88, in the table's order.
    let cases: Vec<(&str, Vec<Spec>, Option<bool>, Option<&str>)> = vec![
        ("none", vec![], None, None),
        (
            "<invalid>, Supercharger, Supercharger",
            vec![invalid, typed("Supercharger"), typed("Supercharger")],
            Some(true),
            Some("Supercharger"),
        ),
        (
            "ACSingleWireCAN",
            vec![typed("ACSingleWireCAN")],
            Some(false),
            Some("ACSingleWireCAN"),
        ),
        (
            "<invalid> with the charger present",
            vec![present(Some("<invalid>"), None)],
            Some(true),
            None,
        ),
        ("{P 60}", vec![bare_power(60.0)], Some(true), None),
        ("{P 50} (no V)", vec![bare_power(50.0)], None, None),
        (
            "{present false, P 7}, then Supercharger",
            vec![absent_flag, typed("Supercharger")],
            Some(true),
            Some("Supercharger"),
        ),
        (
            "Supercharger, then {present false, P 7} (DC is sticky)",
            vec![typed("Supercharger"), absent_flag],
            Some(true),
            Some("Supercharger"),
        ),
        (
            "Combo, Supercharger (the first informative type stays)",
            vec![typed("Combo"), typed("Supercharger")],
            Some(true),
            Some("Combo"),
        ),
        (
            "FastChargerOther, NewKind",
            vec![typed("FastChargerOther"), typed("NewKind")],
            None,
            Some("NewKind"),
        ),
        (
            "Tesla with the charger present and P 7",
            vec![present(Some("Tesla"), Some(7.0))],
            Some(true),
            None,
        ),
        (
            "Tesla on an A7 sample",
            vec![a7_typed("Tesla")],
            Some(false),
            None,
        ),
        (
            "<invalid>, Tesla, Combo",
            vec![invalid, typed("Tesla"), typed("Combo")],
            Some(true),
            Some("Combo"),
        ),
        (
            "MCSingleWireCAN with the charger present",
            vec![present(Some("MCSingleWireCAN"), None)],
            Some(false),
            Some("MCSingleWireCAN"),
        ),
    ];
    assert_eq!(cases.len(), 14);
    for (label, specs, want_dc, want_type) in cases {
        let samples = sequence(&specs);
        let got = charge_session_kind(&samples);
        assert_eq!(
            (got.is_dc, got.fast_charger_type.as_deref()),
            (want_dc, want_type),
            "C2 T88 {label}"
        );
        // Each row equals the live and seeded observer's final fields (C2 T23,
        // T24, T81), which fold through the same step.
        let observed = observed_kind(&samples);
        assert_eq!(
            (observed.0, observed.1.as_deref()),
            (want_dc, want_type),
            "C2 T88 {label} (observer)"
        );
    }
}

/// A deterministic generator (SplitMix64): the fixed seed of C2 T89 and the
/// aggregate property below.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
    fn pick<T: Copy>(&mut self, choices: &[T]) -> T {
        choices[self.below(choices.len())]
    }
}

/// The seed of C2 T89's generator.
const T89_SEED: u64 = 0x0C2_C089;
const T89_SEQUENCES: usize = 10_000;

const T89_TYPES: [Option<&str>; 20] = [
    None,
    Some(""),
    Some("<invalid>"),
    Some("Unknown"),
    Some("SNA"),
    Some("Other"),
    Some("Supercharger"),
    Some("Combo"),
    Some("CHAdeMO"),
    Some("GB"),
    Some("ACSingleWireCAN"),
    Some("MCSingleWireCAN"),
    Some("Tesla"),
    Some("FastChargerSupercharger"),
    Some("fastchargercombo"),
    Some(" CHAdeMO "),
    Some("FASTCHARGERACSINGLEWIRECAN"),
    Some("FastChargerTesla"),
    Some("NewKind"),
    Some("SuperchargerX"),
];

#[test]
fn c2_t89_fold_equivalence() {
    let mut rng = SplitMix(T89_SEED);
    for index in 0..T89_SEQUENCES {
        let length = rng.below(13);
        let samples: Vec<ProjectionChargeSample> = (0..length)
            .map(|k| {
                let mut sample = sample(k as i64 + 1, T0 + 10_000 * k as i64);
                sample.fast_charger_type = rng.pick(&T89_TYPES).map(str::to_owned);
                sample.fast_charger_present = rng.pick(&[None, Some(true), Some(false)]);
                sample.fast_charger_brand = rng
                    .pick(&[None, Some("Tesla"), Some("<invalid>"), Some("BrandX")])
                    .map(str::to_owned);
                sample.charger_power_kw = rng.pick(&[
                    None,
                    Some(0.0),
                    Some(1.0),
                    Some(7.0),
                    Some(11.0),
                    Some(57.0),
                    Some(58.0),
                    Some(120.0),
                ]);
                sample.charger_voltage =
                    rng.pick(&[None, Some(2.0), Some(120.0), Some(230.0), Some(400.0)]);
                sample.charger_actual_current =
                    rng.pick(&[None, Some(0.0), Some(6.0), Some(16.0), Some(32.0)]);
                sample.charger_phases = rng.pick(&[None, Some(1), Some(3)]);
                sample.charge_energy_added_kwh = (rng.below(2) == 1).then_some(0.5 * k as f64);
                sample
            })
            .collect();
        let fail = format!("seed {T89_SEED:#x}, sequence {index}: {samples:?}");
        // (1) The function against the live and seeded observer, one sample at a
        // time.
        let observed = observed_kind(&samples);
        let whole = charge_session_kind(&samples);
        assert_eq!(
            (whole.is_dc, whole.fast_charger_type.clone()),
            observed,
            "{fail}"
        );
        // (2) The step applied page by page, the sequence split into two pages
        // at every position.
        for split in 0..=samples.len() {
            let (mut is_dc, mut fast_charger_type) = (None, None);
            for page in [&samples[..split], &samples[split..]] {
                for sample in page {
                    fold_charge_session_kind(&mut is_dc, &mut fast_charger_type, sample);
                }
            }
            assert_eq!(
                (is_dc, fast_charger_type),
                observed,
                "{fail} (split at {split})"
            );
        }
    }
}

/// A sample row for the aggregate tests.
fn display_sample(
    id: i64,
    level: Option<i64>,
    ideal: Option<f64>,
    rated: Option<f64>,
    power: Option<f64>,
    temperature: Option<f64>,
) -> ProjectionChargeSample {
    let mut row = sample(id, T0 + 10_000 * id);
    row.battery_level = level;
    row.ideal_range_km = ideal;
    row.rated_range_km = rated;
    row.charger_power_kw = power;
    row.outside_temp_c = temperature;
    row
}

/// The six aggregate fields of an open charge.
fn open_aggregates(open: &OpenCharge) -> ChargeDisplayAggregates {
    ChargeDisplayAggregates {
        last_battery_level: open.last_battery_level,
        last_ideal_range_km: open.last_ideal_range_km,
        last_rated_range_km: open.last_rated_range_km,
        max_charger_power_kw: open.max_charger_power_kw,
        outside_temp_sum: open.outside_temp_sum,
        outside_temp_count: open.outside_temp_count,
    }
}

/// The imported process row of the seed tests, with the given end/start values
/// and average.
struct Row {
    start_level: Option<i64>,
    end_level: Option<i64>,
    start_ideal: Option<f64>,
    end_ideal: Option<f64>,
    start_rated: Option<f64>,
    end_rated: Option<f64>,
    average: Option<f64>,
}

const EMPTY_ROW: Row = Row {
    start_level: None,
    end_level: None,
    start_ideal: None,
    end_ideal: None,
    start_rated: None,
    end_rated: None,
    average: None,
};

fn imported_row(row: &Row) -> TeslaMateChargingProcess {
    let mut process: TeslaMateChargingProcess = serde_json::from_value(serde_json::json!({
        "id": 7, "car_id": 1, "position_id": null, "address_id": null,
        "geofence_id": null, "start_date_ms": T0 - 1_800_000, "end_date_ms": null,
        "charge_energy_added": null, "charge_energy_used_kwh": null,
        "start_ideal_range_km": null, "end_ideal_range_km": null,
        "start_battery_level": null, "end_battery_level": null,
        "duration_min": null, "outside_temp_avg": null,
        "start_rated_range_km": null, "end_rated_range_km": null, "cost": null
    }))
    .expect("imported charging process");
    process.start_battery_level = row.start_level;
    process.end_battery_level = row.end_level;
    process.start_ideal_range_km = row.start_ideal;
    process.end_ideal_range_km = row.end_ideal;
    process.start_rated_range_km = row.start_rated;
    process.end_rated_range_km = row.end_rated;
    process.outside_temp_avg = row.average;
    process
}

/// The imported sample row of a sample (the importer's own type), 10 s apart,
/// carrying the five aggregate inputs; the other fields are empty.
fn imported_sample(sample: &ProjectionChargeSample) -> TeslaMateCharge {
    serde_json::from_value(serde_json::json!({
        "id": sample.id, "charging_process_id": 7, "date_ms": sample.timestamp_ms,
        "battery_heater": null, "battery_heater_on": null,
        "battery_heater_no_power": null, "battery_level": sample.battery_level,
        "usable_battery_level": null, "charge_energy_added_kwh": null,
        "charger_actual_current": null, "charger_phases": null,
        "charger_pilot_current": null, "charger_power_kw": null,
        "charger_voltage": null, "charge_cable": null,
        "fast_charger_present": null, "fast_charger_brand": null,
        "fast_charger_type": null, "ideal_range_km": null, "rated_range_km": null,
        "not_enough_power_to_heat": null, "outside_temp_c": null
    }))
    .map(|mut row: TeslaMateCharge| {
        // Non-finite values cannot travel through JSON; set the five inputs
        // directly.
        row.ideal_range_km = sample.ideal_range_km;
        row.rated_range_km = sample.rated_range_km;
        row.charger_power_kw = sample.charger_power_kw;
        row.outside_temp_c = sample.outside_temp_c;
        row
    })
    .expect("imported charge sample")
}

fn seeded(row: &Row, samples: &[ProjectionChargeSample]) -> ChargeDisplayAggregates {
    let imported: Vec<TeslaMateCharge> = samples.iter().map(imported_sample).collect();
    open_aggregates(&open_charge_from_source(&imported_row(row), &imported))
}

#[test]
fn c2_rq_a2_aggregate_equivalence() {
    // (1) The step against the live observer and against itself, page by page,
    // on generated sequences with every filter case (C2 R81 (e), acceptance
    // items 1 and 2).
    let levels = [None, Some(0), Some(20), Some(80), Some(100)];
    let ranges = [
        None,
        Some(0.0),
        Some(150.5),
        Some(300.0),
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(f64::NEG_INFINITY),
    ];
    let powers = [
        None,
        Some(-1.0),
        Some(0.0),
        Some(7.0),
        Some(11.0),
        Some(120.0),
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(f64::NEG_INFINITY),
    ];
    let temperatures = [
        None,
        Some(-5.0),
        Some(0.0),
        Some(12.5),
        Some(21.0),
        Some(f64::NAN),
        Some(f64::INFINITY),
    ];
    let mut rng = SplitMix(0x0C2_C0A2);
    for index in 0..2_000 {
        let length = rng.below(13);
        let samples: Vec<ProjectionChargeSample> = (0..length)
            .map(|k| {
                display_sample(
                    k as i64 + 1,
                    rng.pick(&levels),
                    rng.pick(&ranges),
                    rng.pick(&ranges),
                    rng.pick(&powers),
                    rng.pick(&temperatures),
                )
            })
            .collect();
        let fail = format!("sequence {index}: {samples:?}");
        let mut open = OpenCharge::empty(0, 0, 0);
        for sample in &samples {
            observe_charge_aggregate(&mut open, sample);
        }
        let whole = charge_display_aggregates(&samples);
        assert_eq!(whole, open_aggregates(&open), "{fail}");
        for split in 0..=samples.len() {
            let mut aggregates = ChargeDisplayAggregates::default();
            for page in [&samples[..split], &samples[split..]] {
                for sample in page {
                    fold_charge_display_aggregates(&mut aggregates, sample);
                }
            }
            assert_eq!(aggregates, whole, "{fail} (split at {split})");
        }
        // The live observer does not touch anything else of the open charge.
        let mut untouched = OpenCharge::empty(0, 0, 0);
        whole.store_into(&mut untouched);
        assert_eq!(untouched, open, "{fail}");
    }

    // (2) Filter cases: a negative power is dropped; NaN and both infinities are
    // dropped for power, ranges and temperature; a negative temperature is kept;
    // the battery level is never filtered.
    let filtered = charge_display_aggregates(&[
        display_sample(1, Some(0), Some(100.0), Some(90.0), Some(-1.0), Some(-5.0)),
        display_sample(
            2,
            Some(-3),
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(f64::NAN),
            Some(f64::NEG_INFINITY),
        ),
        display_sample(
            3,
            None,
            Some(f64::NEG_INFINITY),
            Some(f64::NEG_INFINITY),
            Some(f64::INFINITY),
            Some(f64::INFINITY),
        ),
    ]);
    assert_eq!(filtered.last_battery_level, Some(-3), "never filtered");
    assert_eq!(filtered.last_ideal_range_km, Some(100.0));
    assert_eq!(filtered.last_rated_range_km, Some(90.0));
    assert_eq!(
        filtered.max_charger_power_kw, None,
        "every power was dropped"
    );
    assert_eq!(
        filtered.outside_temp_sum, -5.0,
        "a negative temperature stays"
    );
    assert_eq!(filtered.outside_temp_count, 1);
    assert_eq!(filtered.outside_temp_avg(), Some(-5.0));
    assert_eq!(ChargeDisplayAggregates::default().outside_temp_avg(), None);
    // The count saturates.
    let mut full = ChargeDisplayAggregates {
        outside_temp_count: u32::MAX,
        ..ChargeDisplayAggregates::default()
    };
    fold_charge_display_aggregates(
        &mut full,
        &display_sample(1, None, None, None, None, Some(1.0)),
    );
    assert_eq!(full.outside_temp_count, u32::MAX);

    // (3) Seed against step, in separate assertions (C2 R81 (e), acceptance item 3).
    let ordered = [
        display_sample(
            101,
            Some(40),
            Some(150.0),
            Some(140.0),
            Some(7.0),
            Some(10.0),
        ),
        display_sample(
            102,
            Some(50),
            Some(160.0),
            Some(145.0),
            Some(11.0),
            Some(12.0),
        ),
        display_sample(103, None, None, None, Some(9.0), Some(14.0)),
    ];
    // (a) Step equality: a row with no end values, no start values and no
    // average.
    assert_eq!(
        seeded(&EMPTY_ROW, &ordered),
        charge_display_aggregates(&ordered),
        "(a) step equality"
    );
    // (b) Initial state: the seeded last value is the row's end value, else its
    // start value, for a field the samples never carry; the last sample's value
    // wins where one carries it.
    let bare = [display_sample(201, None, None, None, None, None)];
    let from_end = seeded(
        &Row {
            start_level: Some(30),
            end_level: Some(44),
            start_ideal: Some(100.0),
            end_ideal: Some(120.0),
            start_rated: Some(90.0),
            end_rated: Some(110.0),
            average: None,
        },
        &bare,
    );
    assert_eq!(
        (
            from_end.last_battery_level,
            from_end.last_ideal_range_km,
            from_end.last_rated_range_km
        ),
        (Some(44), Some(120.0), Some(110.0)),
        "(b) the row's end values"
    );
    let from_start = seeded(
        &Row {
            start_level: Some(30),
            start_ideal: Some(100.0),
            start_rated: Some(90.0),
            ..EMPTY_ROW
        },
        &bare,
    );
    assert_eq!(
        (
            from_start.last_battery_level,
            from_start.last_ideal_range_km,
            from_start.last_rated_range_km
        ),
        (Some(30), Some(100.0), Some(90.0)),
        "(b) the row's start values"
    );
    let overridden = seeded(
        &Row {
            start_level: Some(30),
            end_level: Some(44),
            end_ideal: Some(120.0),
            end_rated: Some(110.0),
            ..EMPTY_ROW
        },
        &ordered,
    );
    assert_eq!(
        (
            overridden.last_battery_level,
            overridden.last_ideal_range_km,
            overridden.last_rated_range_km
        ),
        (Some(50), Some(160.0), Some(145.0)),
        "(b) the last sample's value wins"
    );
    // (c) Temperature override: a row average `a` with `n` finite-temperature
    // samples seeds count max(n, 1) and a mean of `a`; without an average the
    // step's sum and count stand.
    let with_average = seeded(
        &Row {
            average: Some(11.0),
            ..EMPTY_ROW
        },
        &ordered,
    );
    assert_eq!(with_average.outside_temp_count, 3);
    assert_eq!(with_average.outside_temp_avg(), Some(11.0));
    let no_temperatures = [display_sample(301, None, None, None, Some(7.0), None)];
    let one_average = seeded(
        &Row {
            average: Some(11.0),
            ..EMPTY_ROW
        },
        &no_temperatures,
    );
    assert_eq!(one_average.outside_temp_count, 1, "max(0, 1)");
    assert_eq!(one_average.outside_temp_avg(), Some(11.0));
    let without_average = seeded(&EMPTY_ROW, &ordered);
    assert_eq!(
        (
            without_average.outside_temp_sum,
            without_average.outside_temp_count
        ),
        (36.0, 3)
    );
    // (d) Filters: the seed drops a negative and a non-finite power, as the step
    // does.
    let dirty = [
        display_sample(401, None, None, None, Some(-4.0), None),
        display_sample(402, None, None, None, Some(f64::NAN), Some(f64::NAN)),
        display_sample(403, None, None, None, Some(5.0), Some(-2.0)),
    ];
    let dirty_seed = seeded(&EMPTY_ROW, &dirty);
    assert_eq!(dirty_seed.max_charger_power_kw, Some(5.0));
    assert_eq!(
        (dirty_seed.outside_temp_sum, dirty_seed.outside_temp_count),
        (-2.0, 1)
    );
    let negative_only = seeded(
        &EMPTY_ROW,
        &[display_sample(501, None, None, None, Some(-9.0), None)],
    );
    assert_eq!(negative_only.max_charger_power_kw, None);
}
