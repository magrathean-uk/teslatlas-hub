// SPDX-License-Identifier: AGPL-3.0-only

// Cost and close through the store (C2-P3): the close sequence of R46 (label,
// tariff copy, free charging, cost, upsert), never overwriting a cost (R47),
// the import bound of R5, the fee once per plug-in (R44a, T49) and replay
// after a failed commit (T62). Spec tests T31 to T34, T48, T49, T62 and T63
// (its close half; the fill and the efficiency window belong to C2-P4 and
// C2-P5). Synthetic data only; the sequences are built with the spec's `Ob`
// and `A7` (`src/runtime/lifecycle/energy_tests.rs`).

use crate::lifecycle::charge_tests::at as cost_at;
use crate::lifecycle::energy_tests::{
    a7_fields as cost_a7, counter_only as cost_counter, ob as cost_ob,
};

const COST_T0: i64 = 1_760_000_000_000;
const COST_MIN: i64 = 60_000;
const COST_HOUR: i64 = 60 * COST_MIN;

/// A store with one registered vehicle and the lifecycle state a collector
/// would carry between observations.
struct CostHarness {
    _directory: tempfile::TempDir,
    store: HubStore,
    vehicle: VehicleRecord,
    state: crate::lifecycle::OpenSessionState,
}

impl CostHarness {
    fn new() -> Self {
        let directory = crate::private_tempdir().expect("tempdir");
        let store = HubStore::initialize(directory.path()).expect("store");
        let (_source, vehicle) = test_registered_vehicle(&store);
        Self {
            _directory: directory,
            store,
            vehicle,
            state: crate::lifecycle::OpenSessionState::new(),
        }
    }

    /// Save a fence through the validated owner path.
    fn fence(
        &self,
        id: i64,
        name: &str,
        latitude: f64,
        billing_type: crate::hub_pack::GeofenceBillingType,
        rate: f64,
        fee: Option<f64>,
    ) {
        self.store
            .save_geofence(
                self.vehicle.vehicle_id,
                Some(id),
                crate::teslamate_projection::TeslaMateGeofence {
                    id: 0,
                    name: name.to_owned(),
                    latitude: Some(latitude),
                    longitude: Some(20.0),
                    radius_m: Some(150.0),
                    billing_type: Some(billing_type),
                    cost_per_unit: Some(rate),
                    session_fee: fee,
                },
            )
            .expect("fence");
    }

    /// Insert a fence as an import would copy it, with no validation.
    fn imported_fence(
        &self,
        id: i64,
        latitude: f64,
        billing_type: Option<crate::hub_pack::GeofenceBillingType>,
        rate: Option<f64>,
        fee: Option<f64>,
    ) {
        self.store
            .upsert_geofences(
                self.vehicle.vehicle_id,
                &[crate::teslamate_projection::TeslaMateGeofence {
                    id,
                    name: format!("Imported {id}"),
                    latitude: Some(latitude),
                    longitude: Some(20.0),
                    radius_m: Some(150.0),
                    billing_type,
                    cost_per_unit: rate,
                    session_fee: fee,
                }],
            )
            .expect("imported fence");
    }

    fn set_free_supercharging(&self, car_id: i64) {
        self.store
            .upsert_car_settings(
                self.vehicle.vehicle_id,
                car_id,
                &crate::hub_pack::ProjectionCarSettings {
                    free_supercharging: true,
                    ..Default::default()
                },
            )
            .expect("settings");
    }

    fn commit_step(
        &self,
        step: &crate::lifecycle::LifecycleStep,
        last_id: i64,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        let blob = step.state.encode().expect("encode");
        self.store.commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: self.vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &blob,
            last_observation_id: last_id,
            quarantined: step.quarantined,
            updated_at_ms: at_ms,
            delta: &step.delta,
        })
    }

    /// Apply one observation and commit its step, as the collector does.
    fn feed(&mut self, sample: &crate::lifecycle::LifecycleSample) {
        let step = crate::lifecycle::apply_sample(self.state.clone(), 1, sample).expect("apply");
        self.commit_step(&step, sample.observation_id, sample.observed_at_ms)
            .expect("commit");
        self.state = step.state;
    }

    fn feed_all(&mut self, samples: &[crate::lifecycle::LifecycleSample]) {
        for sample in samples {
            self.feed(sample);
        }
    }

    /// Commit a hand-built delta over the current state.
    fn commit_delta(&self, delta: &crate::lifecycle::LifecycleDelta) {
        let blob = self.state.encode().expect("encode");
        self.store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: self.vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &blob,
                last_observation_id: 0,
                quarantined: false,
                updated_at_ms: COST_T0,
                delta,
            })
            .expect("commit delta");
    }

    fn charges(&self) -> Vec<crate::hub_pack::ProjectionCharge> {
        self.store
            .materialised_history(self.vehicle.vehicle_id)
            .expect("history")
            .charges
    }

    fn charge(&self, id: i64) -> crate::hub_pack::ProjectionCharge {
        self.charges()
            .into_iter()
            .find(|charge| charge.id == id)
            .unwrap_or_else(|| panic!("charge {id}"))
    }

    fn charge_mutations(&self, charge_id: i64) -> i64 {
        self.store
            .open()
            .expect("connection")
            .query_row(
                "SELECT COUNT(*) FROM sync_mutations
                 WHERE vehicle_id = ?1 AND entity = 'charge' AND entity_id = ?2",
                params![self.vehicle.vehicle_id.to_string(), charge_id],
                |row| row.get(0),
            )
            .expect("mutation count")
    }
}

/// A closed row as `finalize_charge` leaves it (no label, tariff copy or
/// cost), for the tests that commit a delta by hand.
fn cost_row(
    id: i64,
    used: Option<f64>,
    added: Option<f64>,
    is_dc: Option<bool>,
    fast_charger_type: Option<&str>,
) -> crate::hub_pack::ProjectionCharge {
    crate::hub_pack::ProjectionCharge {
        id,
        car_id: 1,
        start_date_ms: COST_T0,
        end_date_ms: Some(COST_T0 + 10 * COST_MIN),
        charge_energy_added: added,
        charge_energy_used_kwh: used,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        cost: None,
        fast_charger_type: fast_charger_type.map(str::to_owned),
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
        start_latitude: None,
        start_longitude: None,
        start_battery_level: None,
        end_battery_level: None,
        duration_min: Some(10),
        address: None,
        location_name: None,
        geofence: None,
        is_dc,
        charge_rate_km_per_hour: None,
        max_charger_power_kw: None,
        outside_temp_avg: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
    }
}

fn cost_sample(
    id: i64,
    charge_id: i64,
    offset_ms: i64,
    counter: Option<f64>,
) -> crate::hub_pack::ProjectionChargeSample {
    crate::hub_pack::ProjectionChargeSample {
        id,
        charge_process_id: charge_id,
        timestamp_ms: COST_T0 + offset_ms,
        battery_level: None,
        usable_battery_level: None,
        charge_energy_added_kwh: counter,
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

fn kwh() -> crate::hub_pack::GeofenceBillingType {
    crate::hub_pack::GeofenceBillingType::PerKwh
}

fn per_minute() -> crate::hub_pack::GeofenceBillingType {
    crate::hub_pack::GeofenceBillingType::PerMinute
}

/// T19's first sequence: three A7 samples then a `Complete` after an offline
/// gap with counter 20.00. The first observation carries coordinates (10.0005,
/// 20.0), inside a fence at (10.0, 20.0) of radius 150 m.
fn gap_sequence(with_coordinates: bool) -> Vec<crate::lifecycle::LifecycleSample> {
    let first = cost_ob(1, COST_T0, "Charging", cost_a7(Some(0.00)));
    vec![
        if with_coordinates {
            cost_at(first, 10.0005, 20.0)
        } else {
            first
        },
        cost_ob(2, COST_T0 + 10 * COST_MIN, "Charging", cost_a7(Some(1.08))),
        cost_ob(3, COST_T0 + 20 * COST_MIN, "Charging", cost_a7(Some(2.16))),
        cost_ob(
            4,
            COST_T0 + 3 * COST_HOUR,
            "Complete",
            serde_json::json!({"charge_energy_added": 20.00, "charger_power": 0,
                               "charger_voltage": 240, "charger_actual_current": 0}),
        ),
    ]
}

#[test]
fn t31_the_close_labels_copies_the_tariff_and_prices_the_charge() {
    let mut harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    harness.feed_all(&gap_sequence(true));
    let charges = harness.charges();
    assert_eq!(charges.len(), 1);
    let charge = &charges[0];
    assert_eq!(charge.charge_energy_added, Some(20.00));
    assert_eq!(charge.charge_energy_used_kwh, Some(22.22));
    assert_eq!(charge.geofence.as_deref(), Some("F3"));
    assert_eq!(charge.billing_type, Some(kwh()));
    assert_eq!(charge.cost_per_unit, Some(0.30));
    assert_eq!(charge.session_fee, Some(1.00));
    // 22.22 * 0.30 + 1.00 = 7.666.
    assert_eq!(charge.cost, Some(7.67));
    assert_eq!(harness.charge_mutations(charge.id), 1);
}

#[test]
fn t32_no_fence_gives_no_label_copy_or_cost_and_free_charging_gives_zero() {
    let mut harness = CostHarness::new();
    harness.feed_all(&gap_sequence(false));
    let charge = &harness.charges()[0];
    assert_eq!(
        (
            charge.geofence.clone(),
            charge.billing_type,
            charge.cost_per_unit,
            charge.session_fee,
            charge.cost
        ),
        (None, None, None, None, None)
    );

    // A DC Supercharger session, no fence. With no `car_settings` row the car
    // is not entitled, so the cost is unknown.
    let dc = |base: i64, first_id: i64, last: &str| {
        let fields = |counter: f64| {
            serde_json::json!({
                "charge_energy_added": counter, "charger_power": 150,
                "charger_voltage": 2, "charger_actual_current": 0,
                "fast_charger_present": true, "fast_charger_type": "Supercharger",
            })
        };
        vec![
            cost_ob(first_id, base, "Charging", fields(0.0)),
            cost_ob(first_id + 1, base + 2 * COST_MIN, "Charging", fields(5.0)),
            cost_ob(first_id + 2, base + 4 * COST_MIN, last, fields(9.0)),
        ]
    };
    let unplug = cost_ob(
        10,
        COST_T0 + 4 * COST_HOUR,
        "Disconnected",
        serde_json::json!({}),
    );
    harness.feed(&unplug);
    harness.feed_all(&dc(COST_T0 + 5 * COST_HOUR, 11, "Complete"));
    let unentitled = harness.charges().pop().expect("second charge");
    assert_eq!(unentitled.is_dc, Some(true));
    assert_eq!(
        unentitled.fast_charger_type.as_deref(),
        Some("Supercharger")
    );
    assert_eq!(unentitled.cost, None);

    harness.set_free_supercharging(1);
    harness.feed(&cost_ob(
        20,
        COST_T0 + 6 * COST_HOUR,
        "Disconnected",
        serde_json::json!({}),
    ));
    harness.feed_all(&dc(COST_T0 + 7 * COST_HOUR, 21, "Complete"));
    let free = harness.charges().pop().expect("third charge");
    assert_eq!(free.cost, Some(0.00));
    assert_eq!(
        (free.billing_type, free.cost_per_unit, free.session_fee),
        (None, None, None)
    );
}

#[test]
fn r46_the_free_setting_of_another_car_is_not_an_entitlement() {
    let harness = CostHarness::new();
    harness.set_free_supercharging(2);
    let mut delta = crate::lifecycle::LifecycleDelta::default();
    delta.charges.push(cost_row(
        1,
        None,
        Some(9.0),
        Some(true),
        Some("Supercharger"),
    ));
    harness.commit_delta(&delta);
    assert_eq!(harness.charge(1).cost, None);
    // The same setting stored for the committing car does apply.
    harness.set_free_supercharging(1);
    let mut again = crate::lifecycle::LifecycleDelta::default();
    again.charges.push(cost_row(
        2,
        None,
        Some(9.0),
        Some(true),
        Some("Supercharger"),
    ));
    harness.commit_delta(&again);
    assert_eq!(harness.charge(2).cost, Some(0.00));
}

/// Insert a stored row the way a lifecycle commit or an import leaves one.
fn store_charge_row(harness: &CostHarness, charge: &crate::hub_pack::ProjectionCharge) {
    harness
        .store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, ?2, 1, ?3)",
            params![
                harness.vehicle.vehicle_id.to_string(),
                charge.id,
                serde_json::to_string(charge).expect("json")
            ],
        )
        .expect("insert row");
}

#[test]
fn t33_an_existing_cost_and_its_copy_are_kept_at_close() {
    let harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    // An imported open process that already carries a hand-entered cost and no
    // tariff copy; the import watermark covers it (R5).
    let import = harness
        .store
        .register_source(&SourceDescriptor::new("teslamate", "import-t33"), 1_000)
        .expect("import source");
    harness
        .store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO lifecycle_source_watermarks(
                source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms
             ) VALUES (?1, ?2, 'charging_processes', 41, NULL)",
            params![
                import.source_id.to_string(),
                harness.vehicle.vehicle_id.to_string()
            ],
        )
        .expect("watermark");
    let mut stored = cost_row(41, None, None, None, None);
    stored.end_date_ms = None;
    stored.cost = Some(9.99);
    store_charge_row(&harness, &stored);

    // The live close of charge 41 inside a priced fence.
    let mut delta = crate::lifecycle::LifecycleDelta::default();
    delta
        .charges
        .push(cost_row(41, Some(22.22), Some(20.00), Some(false), None));
    delta.charge_start_coordinates.push((41, 10.0005, 20.0));
    harness.commit_delta(&delta);
    let closed = harness.charge(41);
    assert_eq!(closed.cost, Some(9.99));
    assert_eq!(
        (
            closed.billing_type,
            closed.cost_per_unit,
            closed.session_fee
        ),
        (None, None, None)
    );
    // The row was closed: the live values replaced the open ones.
    assert_eq!(closed.charge_energy_added, Some(20.00));
    assert!(closed.end_date_ms.is_some());
}

#[test]
fn r47_a_stored_tariff_copy_is_kept_with_the_stored_cost() {
    let harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    let mut stored = cost_row(5, Some(10.0), Some(9.0), Some(false), None);
    stored.cost = Some(4.00);
    stored.billing_type = Some(per_minute());
    stored.cost_per_unit = Some(0.05);
    stored.session_fee = None;
    store_charge_row(&harness, &stored);
    let mut delta = crate::lifecycle::LifecycleDelta::default();
    delta
        .charges
        .push(cost_row(5, Some(10.0), Some(9.0), Some(false), None));
    delta.charge_start_coordinates.push((5, 10.0005, 20.0));
    harness.commit_delta(&delta);
    let kept = harness.charge(5);
    assert_eq!(kept.cost, Some(4.00));
    assert_eq!(kept.billing_type, Some(per_minute()));
    assert_eq!(kept.cost_per_unit, Some(0.05));
    assert_eq!(kept.session_fee, None);
}

#[test]
fn r47_a_stored_row_without_a_cost_is_priced_at_close() {
    let harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, None);
    store_charge_row(&harness, &cost_row(6, None, None, None, None));
    let mut delta = crate::lifecycle::LifecycleDelta::default();
    delta
        .charges
        .push(cost_row(6, Some(10.0), Some(9.0), Some(false), None));
    delta.charge_start_coordinates.push((6, 10.0005, 20.0));
    harness.commit_delta(&delta);
    assert_eq!(harness.charge(6).cost, Some(3.00));
}

#[test]
fn t34_a_fence_edit_never_rewrites_a_stored_cost_or_copy() {
    let mut harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    harness.feed_all(&gap_sequence(true));
    let id = harness.charges()[0].id;
    harness.fence(3, "F3", 10.0, kwh(), 0.40, Some(1.00));
    let charge = harness.charge(id);
    assert_eq!(charge.cost, Some(7.67));
    assert_eq!(charge.cost_per_unit, Some(0.30));
    // Deleting the fence leaves label, copy and cost as stored (R56).
    harness
        .store
        .delete_geofence(harness.vehicle.vehicle_id, 3)
        .expect("delete fence");
    let after = harness.charge(id);
    assert_eq!(after.cost, Some(7.67));
    assert_eq!(after.billing_type, Some(kwh()));
    assert_eq!(after.session_fee, Some(1.00));
}

#[test]
fn t48_a_fence_tariff_is_copied_only_when_complete_and_valid() {
    let harness = CostHarness::new();
    harness.set_free_supercharging(1);
    // F5 fee only; F6 and F7 imported with a missing rate and a negative rate;
    // F9 valid with a zero rate and a fee. (The fence table refuses a negative
    // fee, so the negative-fee case of T48 is pinned at the unit level by
    // `r40_a_fence_tariff_is_copied_only_when_complete_and_valid`.)
    harness.imported_fence(5, 12.0, None, None, Some(2.00));
    harness.imported_fence(6, 13.0, Some(kwh()), None, None);
    harness.imported_fence(7, 14.0, Some(kwh()), Some(-0.10), None);
    harness.imported_fence(9, 16.0, Some(kwh()), Some(0.00), Some(2.00));
    let mut delta = crate::lifecycle::LifecycleDelta::default();
    for (id, latitude) in [(1, 12.0), (2, 13.0), (3, 14.0), (4, 16.0)] {
        delta
            .charges
            .push(cost_row(id, Some(10.00), Some(9.00), Some(false), None));
        delta
            .charge_start_coordinates
            .push((id, latitude + 0.0005, 20.0));
    }
    // A DC Supercharger charge in F5 with the setting on.
    delta.charges.push(cost_row(
        5,
        None,
        Some(9.00),
        Some(true),
        Some("Supercharger"),
    ));
    delta.charge_start_coordinates.push((5, 12.0005, 20.0));
    harness.commit_delta(&delta);
    for id in 1..=3 {
        let row = harness.charge(id);
        assert_eq!(
            (
                row.billing_type,
                row.cost_per_unit,
                row.session_fee,
                row.cost
            ),
            (None, None, None, None),
            "charge {id}"
        );
    }
    let valid = harness.charge(4);
    assert_eq!(valid.billing_type, Some(kwh()));
    assert_eq!(valid.cost_per_unit, Some(0.00));
    assert_eq!(valid.session_fee, Some(2.00));
    assert_eq!(valid.cost, Some(2.00));
    let free = harness.charge(5);
    assert_eq!(
        (free.billing_type, free.cost_per_unit, free.session_fee),
        (None, None, None)
    );
    assert_eq!(free.cost, Some(0.00));
    assert_eq!(free.geofence.as_deref(), Some("Imported 5"));
    // The fee-only fence still labels the row.
    assert_eq!(harness.charge(1).geofence.as_deref(), Some("Imported 5"));
}

#[test]
fn r43_the_per_minute_cost_needs_an_observed_span_at_close() {
    let mut harness = CostHarness::new();
    harness.fence(4, "Per minute", 11.0, per_minute(), 0.25, None);
    // Polling every five minutes from the start to the end: 15 minutes.
    let polled: Vec<_> = (0..4)
        .map(|step| {
            let state = if step == 3 { "Complete" } else { "Charging" };
            let sample = cost_ob(
                step + 1,
                COST_T0 + step * 5 * COST_MIN,
                state,
                cost_a7(Some(step as f64 * 0.6)),
            );
            if step == 0 {
                cost_at(sample, 11.0005, 20.0)
            } else {
                sample
            }
        })
        .collect();
    harness.feed_all(&polled);
    let observed = harness.charges().pop().expect("charge");
    assert_eq!(observed.billing_type, Some(per_minute()));
    // 15 minutes at 0.25.
    assert_eq!(observed.cost, Some(3.75));

    // The same shape closed by one sample after a long silence: its end is
    // only an upper bound, so no per-minute cost is published.
    harness.feed(&cost_ob(
        20,
        COST_T0 + COST_HOUR,
        "Disconnected",
        serde_json::json!({}),
    ));
    let base = COST_T0 + 2 * COST_HOUR;
    let gap = [
        cost_at(
            cost_ob(21, base, "Charging", cost_a7(Some(0.0))),
            11.0005,
            20.0,
        ),
        cost_ob(22, base + 5 * COST_MIN, "Charging", cost_a7(Some(0.6))),
        cost_ob(23, base + 3 * COST_HOUR, "Complete", cost_counter(20.0)),
    ];
    harness.feed_all(&gap);
    let bounded = harness.charges().pop().expect("charge");
    assert_eq!(bounded.billing_type, Some(per_minute()));
    assert_eq!(bounded.cost, None);
}

fn a7_run(
    first_id: i64,
    start_ms: i64,
    counters: [f64; 3],
    last_state: &str,
) -> Vec<crate::lifecycle::LifecycleSample> {
    vec![
        cost_at(
            cost_ob(first_id, start_ms, "Charging", cost_a7(Some(counters[0]))),
            10.0005,
            20.0,
        ),
        cost_ob(
            first_id + 1,
            start_ms + 10 * COST_MIN,
            "Charging",
            cost_a7(Some(counters[1])),
        ),
        cost_ob(
            first_id + 2,
            start_ms + 20 * COST_MIN,
            last_state,
            cost_a7(Some(counters[2])),
        ),
    ]
}

/// ST54's first row: seven A7 samples ten minutes apart, counter +1.20 per
/// step, 0.00 to 7.20, the first inside fence 3.
fn first_plug_in_row() -> Vec<crate::lifecycle::LifecycleSample> {
    (0..7)
        .map(|step| {
            let state = if step == 6 { "Complete" } else { "Charging" };
            let counter = (step as f64 * 1.20 * 100.0).round() / 100.0;
            let sample = cost_ob(
                step + 1,
                COST_T0 + step * 10 * COST_MIN,
                state,
                cost_a7(Some(counter)),
            );
            if step == 0 {
                cost_at(sample, 10.0005, 20.0)
            } else {
                sample
            }
        })
        .collect()
}

#[test]
fn t49a_a_top_up_in_the_same_plug_in_waives_the_session_fee() {
    let mut harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    harness.feed_all(&first_plug_in_row());
    harness.feed_all(&a7_run(
        20,
        COST_T0 + 3 * COST_HOUR,
        [7.50, 8.58, 9.66],
        "Complete",
    ));
    let charges = harness.charges();
    assert_eq!(charges.len(), 2);
    let (first, second) = (&charges[0], &charges[1]);
    assert_eq!(first.charge_energy_added, Some(7.20));
    assert_eq!(first.session_fee, Some(1.00));
    assert_eq!(first.cost, Some(3.16));
    // Baseline 7.20 (the snapshot's counter), so 9.66 - 7.20 is new.
    assert_eq!(second.charge_energy_added, Some(2.46));
    // Row 4 of R19: 2.46 * 2.4 / 2.16.
    assert_eq!(second.charge_energy_used_kwh, Some(2.73));
    assert_eq!(second.billing_type, Some(kwh()));
    assert_eq!(second.cost_per_unit, Some(0.30));
    assert_eq!(second.session_fee, Some(0.00));
    assert_eq!(second.cost, Some(0.82));
}

#[test]
fn t49b_a_restarted_counter_is_a_new_connection_and_pays_the_fee() {
    let mut harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    harness.feed_all(&first_plug_in_row());
    harness.feed_all(&a7_run(
        20,
        COST_T0 + 3 * COST_HOUR,
        [0.30, 1.38, 2.46],
        "Complete",
    ));
    let second = harness.charges().pop().expect("second row");
    assert_eq!(second.charge_energy_added, Some(2.46));
    assert_eq!(second.session_fee, Some(1.00));
    // 2.73 * 0.30 + 1.00 = 1.819.
    assert_eq!(second.cost, Some(1.82));
}

#[test]
fn t49c_a_row_whose_counter_continues_pays_no_second_fee() {
    let harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    // The second row of a continuity split: baseline 2.00, added 38.00, so its
    // counter reaches 40.00. Its sample travels in the delta.
    let mut delta = crate::lifecycle::LifecycleDelta::default();
    delta
        .charges
        .push(cost_row(1, Some(40.0), Some(38.00), Some(false), None));
    delta.charge_start_coordinates.push((1, 10.0005, 20.0));
    delta
        .charge_samples
        .push(cost_sample(1, 1, 5 * COST_MIN, Some(40.00)));
    // The control: the same row with no sample is charged the fee.
    delta
        .charges
        .push(cost_row(2, Some(40.0), Some(38.00), Some(false), None));
    delta.charge_start_coordinates.push((2, 10.0005, 20.0));
    harness.commit_delta(&delta);
    let continuing = harness.charge(1);
    assert_eq!(continuing.session_fee, Some(0.00));
    assert_eq!(continuing.cost, Some(12.00));
    let control = harness.charge(2);
    assert_eq!(control.session_fee, Some(1.00));
    assert_eq!(control.cost, Some(13.00));
}

#[test]
fn t49c_the_samples_of_the_close_include_the_stored_open_samples() {
    // The continuation check reads the open samples stored by earlier commits,
    // so a closing observation that carries no counter still sees them.
    let mut harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    harness.feed_all(&first_plug_in_row());
    harness.feed_all(&[
        cost_at(
            cost_ob(20, COST_T0 + 3 * COST_HOUR, "Charging", cost_a7(Some(7.50))),
            10.0005,
            20.0,
        ),
        cost_ob(21, COST_T0 + 3 * COST_HOUR + 10 * COST_MIN, "Charging", cost_a7(Some(8.58))),
        cost_ob(
            22,
            COST_T0 + 3 * COST_HOUR + 20 * COST_MIN,
            "Complete",
            serde_json::json!({"charger_power": 0, "charger_voltage": 240, "charger_actual_current": 0}),
        ),
    ]);
    let second = harness.charges().pop().expect("second row");
    // Counters 7.50 and 8.58 are stored; the closing sample has none.
    assert_eq!(second.charge_energy_added, Some(1.38));
    assert_eq!(second.session_fee, Some(0.00));
}

#[test]
fn t49d_a_counter_reset_split_charges_the_fee_on_the_second_row() {
    let mut harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    harness.feed_all(&[
        cost_at(
            cost_ob(1, COST_T0, "Charging", cost_a7(Some(0.0))),
            10.0005,
            20.0,
        ),
        cost_ob(2, COST_T0 + 10 * COST_MIN, "Charging", cost_a7(Some(2.00))),
        cost_ob(3, COST_T0 + 20 * COST_MIN, "Charging", cost_a7(Some(4.00))),
        cost_at(
            cost_ob(4, COST_T0 + 30 * COST_MIN, "Charging", cost_a7(Some(0.50))),
            10.0005,
            20.0,
        ),
        cost_ob(5, COST_T0 + 40 * COST_MIN, "Complete", cost_a7(Some(1.58))),
    ]);
    let charges = harness.charges();
    assert_eq!(charges.len(), 2);
    assert_eq!(charges[0].session_fee, Some(1.00));
    assert_eq!(charges[1].session_fee, Some(1.00));
    assert_eq!(charges[1].billing_type, Some(kwh()));
}

#[test]
fn t62_a_failed_close_commit_leaves_nothing_and_the_retry_writes_one_row() {
    let mut harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    let samples = gap_sequence(true);
    harness.feed_all(&samples[..3]);
    // Fail the transaction after the charge row and its sync mutation were
    // written: the sample insert that follows them aborts.
    harness
        .store
        .open()
        .expect("connection")
        .execute_batch(
            "CREATE TRIGGER c2_injected_fault BEFORE INSERT ON materialised_charge_samples
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .expect("trigger");
    let closing = &samples[3];
    let step = crate::lifecycle::apply_sample(harness.state.clone(), 1, closing).expect("apply");
    let charge_id = step.delta.charges[0].id;
    assert!(
        harness
            .commit_step(&step, closing.observation_id, closing.observed_at_ms)
            .is_err()
    );
    assert!(harness.charges().is_empty());
    assert_eq!(harness.charge_mutations(charge_id), 0);
    let record = harness
        .store
        .load_lifecycle_state(harness.vehicle.vehicle_id)
        .expect("load")
        .expect("state");
    assert_eq!(record.last_observation_id, 3, "the state did not advance");

    harness
        .store
        .open()
        .expect("connection")
        .execute_batch("DROP TRIGGER c2_injected_fault;")
        .expect("drop trigger");
    harness
        .commit_step(&step, closing.observation_id, closing.observed_at_ms)
        .expect("retry");
    let charges = harness.charges();
    assert_eq!(charges.len(), 1);
    assert_eq!(charges[0].cost, Some(7.67));
    assert_eq!(harness.charge_mutations(charge_id), 1);
}

#[test]
fn r6_recommitting_an_unchanged_row_adds_no_sync_mutation() {
    let mut harness = CostHarness::new();
    harness.fence(3, "F3", 10.0, kwh(), 0.30, Some(1.00));
    let samples = gap_sequence(true);
    harness.feed_all(&samples[..3]);
    let closing = &samples[3];
    let step = crate::lifecycle::apply_sample(harness.state.clone(), 1, closing).expect("apply");
    let charge_id = step.delta.charges[0].id;
    harness
        .commit_step(&step, closing.observation_id, closing.observed_at_ms)
        .expect("first commit");
    harness
        .commit_step(&step, closing.observation_id, closing.observed_at_ms)
        .expect("second commit");
    assert_eq!(harness.charges().len(), 1);
    assert_eq!(harness.charge_mutations(charge_id), 1);
}

fn imported_open_session(start: i64) -> crate::teslamate_projection::TeslaMateOpenSession {
    let process = serde_json::from_value(serde_json::json!({
        "id": 7, "car_id": 1, "position_id": null, "address_id": null,
        "geofence_id": null, "start_date_ms": start, "end_date_ms": null,
        "charge_energy_added": 99.0, "charge_energy_used_kwh": 99.0,
        "start_ideal_range_km": 151.0, "end_ideal_range_km": 152.0,
        "start_battery_level": 40, "end_battery_level": 50,
        "duration_min": null, "outside_temp_avg": 11.0,
        "start_rated_range_km": 141.0, "end_rated_range_km": 142.0,
        "cost": null
    }))
    .expect("imported charging process");
    let sample = |id: i64, at_ms: i64, energy: f64, level: i64| {
        serde_json::from_value(serde_json::json!({
            "id": id, "charging_process_id": 7, "date_ms": at_ms,
            "battery_heater": false, "battery_heater_on": false,
            "battery_heater_no_power": false, "battery_level": level,
            "usable_battery_level": level, "charge_energy_added_kwh": energy,
            "charger_actual_current": 16.0, "charger_phases": 3,
            "charger_pilot_current": 16.0, "charger_power_kw": 11.0,
            "charger_voltage": 230.0, "charge_cable": "IEC",
            "fast_charger_present": false, "fast_charger_brand": null,
            "fast_charger_type": null, "ideal_range_km": 150.0 + id as f64,
            "rated_range_km": 140.0 + id as f64,
            "not_enough_power_to_heat": false, "outside_temp_c": 10.0
        }))
        .expect("imported charge sample")
    };
    let mut session = crate::teslamate_projection::TeslaMateOpenSession {
        car_id: 1,
        charge: Some(process),
        charge_samples: vec![
            sample(1, start, 1.0, 40),
            sample(2, start + 5 * COST_MIN, 2.0, 50),
        ],
        ..Default::default()
    };
    session.watermarks.charging_processes.max_id = Some(7);
    session.watermarks.charges.max_timestamp_ms = Some(start + 5 * COST_MIN);
    session
}

#[test]
fn t63_an_imported_open_charge_is_priced_at_its_live_close() {
    let start = COST_T0 - COST_HOUR;
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    // A migrated vehicle: registered under the import source (C9 ST51).
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "import-t63"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-import"),
            1_001,
        )
        .expect("vehicle");
    store
        .upsert_car_settings(
            vehicle.vehicle_id,
            1,
            &crate::hub_pack::ProjectionCarSettings {
                free_supercharging: true,
                ..Default::default()
            },
        )
        .expect("settings");
    store
        .seed_imported_open_session(
            source.source_id,
            vehicle.vehicle_id,
            1,
            &imported_open_session(start),
            start,
        )
        .expect("seed");
    let dc = |counter: f64| {
        serde_json::json!({
            "charge_energy_added": counter, "charger_power": 120,
            "charger_voltage": 2, "charger_actual_current": 0,
            "fast_charger_present": true, "fast_charger_type": "Supercharger",
        })
    };
    feed_owner_samples(
        &store,
        &source,
        &vehicle,
        &[
            cost_ob(1, start + 7 * COST_MIN, "Charging", dc(3.00)),
            cost_ob(2, start + 9 * COST_MIN, "Complete", dc(4.00)),
        ],
    );
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(history.charges.len(), 1);
    let charge = &history.charges[0];
    assert_eq!(charge.id, 7);
    // The imported counter is not a baseline (R11): 4.00 - 0.0.
    assert_eq!(charge.charge_energy_added, Some(4.00));
    assert_eq!(charge.is_dc, Some(true));
    // The row keeps its imported id but the Hub closed it, so its cost is
    // computed (R5 exception b): free at a Supercharger.
    assert_eq!(charge.cost, Some(0.00));
    // The imported samples are carried over unchanged.
    let mut samples = history
        .charge_samples
        .iter()
        .filter(|sample| sample.charge_process_id == 7)
        .collect::<Vec<_>>();
    samples.sort_by_key(|sample| sample.id);
    assert_eq!(samples.len(), 4);
    assert_eq!(samples[0].charge_energy_added_kwh, Some(1.00));
    assert_eq!(samples[1].charge_energy_added_kwh, Some(2.00));
    // R5: the watermark makes row 7 imported and row 8 a Hub row.
    let connection = store.open().expect("connection");
    let bound = charge_import_bound(&connection, vehicle.vehicle_id).expect("bound");
    assert!(bound.is_imported(7));
    assert!(!bound.is_imported(8));
}

fn bound_of_vehicle(store: &HubStore, vehicle: Uuid) -> i64 {
    let connection = store.open().expect("connection");
    charge_import_bound(&connection, vehicle)
        .expect("bound")
        .max_imported_charge_id
}

#[test]
fn r5_a_reseed_never_lowers_the_charge_import_bound() {
    let start = COST_T0 - COST_HOUR;
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "import-r5-seed"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-r5"),
            1_001,
        )
        .expect("vehicle");
    let v = vehicle.vehicle_id;
    let mut session = imported_open_session(start);
    session.watermarks.charging_processes.max_id = Some(50);
    store
        .seed_imported_open_session(source.source_id, v, 1, &session, start)
        .expect("seed");
    assert_eq!(bound_of_vehicle(&store, v), 50);

    // A later snapshot of the same import with a smaller maximum.
    let mut lower = imported_open_session(start + COST_MIN);
    lower.watermarks.charging_processes.max_id = Some(7);
    store
        .seed_imported_open_session(source.source_id, v, 1, &lower, start + COST_MIN)
        .expect("reseed lower");
    assert_eq!(bound_of_vehicle(&store, v), 50, "a lower maximum");

    // And one that records no charge maximum at all.
    let mut absent = imported_open_session(start + 2 * COST_MIN);
    absent.watermarks.charging_processes.max_id = None;
    store
        .seed_imported_open_session(source.source_id, v, 1, &absent, start + 2 * COST_MIN)
        .expect("reseed absent");
    assert_eq!(bound_of_vehicle(&store, v), 50, "an absent maximum");

    // A higher maximum still raises it.
    let mut higher = imported_open_session(start + 3 * COST_MIN);
    higher.watermarks.charging_processes.max_id = Some(80);
    store
        .seed_imported_open_session(source.source_id, v, 1, &higher, start + 3 * COST_MIN)
        .expect("reseed higher");
    assert_eq!(bound_of_vehicle(&store, v), 80);
}

#[test]
fn r5_a_promotion_never_lowers_the_charge_import_bound() {
    let start = COST_T0 - COST_HOUR;
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("teslamate", "import-r5-promote"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-r5p"),
            1_001,
        )
        .expect("vehicle");
    let v = vehicle.vehicle_id;
    let promote = |session: &crate::teslamate_projection::TeslaMateOpenSession, at_ms: i64| {
        let mut connection = store.open().expect("connection");
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("begin");
        promote_imported_open_session_in_transaction(
            &transaction,
            source.source_id,
            v,
            1,
            session,
            at_ms,
            None,
        )
        .expect("promote");
        transaction.commit().expect("commit");
    };
    let mut session = imported_open_session(start);
    session.watermarks.charging_processes.max_id = Some(50);
    promote(&session, start);
    assert_eq!(bound_of_vehicle(&store, v), 50);
    let mut lower = imported_open_session(start + COST_MIN);
    lower.watermarks.charging_processes.max_id = Some(7);
    promote(&lower, start + COST_MIN);
    assert_eq!(bound_of_vehicle(&store, v), 50, "a lower maximum");
    let mut absent = imported_open_session(start + 2 * COST_MIN);
    absent.watermarks.charging_processes.max_id = None;
    promote(&absent, start + 2 * COST_MIN);
    assert_eq!(bound_of_vehicle(&store, v), 50, "an absent maximum");
}

fn insert_watermark(
    store: &HubStore,
    source: &SourceRecord,
    vehicle: &VehicleRecord,
    domain: &str,
    max_id: Option<i64>,
) {
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO lifecycle_source_watermarks(
                source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms
             ) VALUES (?1, ?2, ?3, ?4, NULL)",
            params![
                source.source_id.to_string(),
                vehicle.vehicle_id.to_string(),
                domain,
                max_id
            ],
        )
        .expect("watermark");
}

fn import_bound_of(store: &HubStore, vehicle: &VehicleRecord) -> ChargeImportBound {
    let connection = store.open().expect("connection");
    charge_import_bound(&connection, vehicle.vehicle_id).expect("bound")
}

#[test]
fn r5_the_import_bound_reads_only_import_watermarks_of_charging_processes() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (live, vehicle) = test_registered_vehicle(&store);
    let other = store
        .register_vehicle(
            &VehicleDescriptor::new(live.source_id, "vehicle-two"),
            1_002,
        )
        .expect("second vehicle");
    let import_a = store
        .register_source(&SourceDescriptor::new("teslamate", "import-a"), 1_003)
        .expect("source a");
    let import_b = store
        .register_source(&SourceDescriptor::new("teslamate", "import-b"), 1_004)
        .expect("source b");

    // No watermark: nothing is imported, and every id is a Hub row.
    let none = import_bound_of(&store, &vehicle);
    assert_eq!(
        none,
        ChargeImportBound {
            max_imported_charge_id: 0
        }
    );
    assert!(!none.is_imported(1));

    // A watermark of another domain, of a live source kind or of another
    // vehicle changes nothing.
    insert_watermark(&store, &import_a, &vehicle, "charges", Some(99));
    insert_watermark(&store, &live, &vehicle, "charging_processes", Some(77));
    insert_watermark(&store, &import_a, &other, "charging_processes", Some(60));
    assert_eq!(import_bound_of(&store, &vehicle).max_imported_charge_id, 0);
    assert_eq!(import_bound_of(&store, &other).max_imported_charge_id, 60);

    // The import watermark of the vehicle counts; the id test is inclusive.
    insert_watermark(&store, &import_a, &vehicle, "charging_processes", Some(41));
    let bound = import_bound_of(&store, &vehicle);
    assert_eq!(bound.max_imported_charge_id, 41);
    assert!(bound.is_imported(41));
    assert!(!bound.is_imported(42));

    // Two import sources: the larger wins. A NULL watermark counts as none.
    insert_watermark(&store, &import_b, &vehicle, "charging_processes", Some(50));
    assert_eq!(import_bound_of(&store, &vehicle).max_imported_charge_id, 50);
    let third = store
        .register_vehicle(
            &VehicleDescriptor::new(live.source_id, "vehicle-three"),
            1_005,
        )
        .expect("third vehicle");
    insert_watermark(&store, &import_a, &third, "charging_processes", None);
    assert_eq!(import_bound_of(&store, &third).max_imported_charge_id, 0);
}

#[test]
fn r5_the_vehicle_source_does_not_decide_which_rows_are_imported() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let import = store
        .register_source(&SourceDescriptor::new("teslamate", "import-m"), 1_000)
        .expect("source");
    let migrated = store
        .register_vehicle(
            &VehicleDescriptor::new(import.source_id, "vehicle-m"),
            1_001,
        )
        .expect("vehicle");
    // Registered under the import source, with no watermark: every row is a
    // Hub row (the G2-4 requirement of R5 is what makes this safe).
    assert_eq!(import_bound_of(&store, &migrated).max_imported_charge_id, 0);
    insert_watermark(&store, &import, &migrated, "charging_processes", Some(40));
    let bound = import_bound_of(&store, &migrated);
    assert!(bound.is_imported(39));
    assert!(!bound.is_imported(41));
}
