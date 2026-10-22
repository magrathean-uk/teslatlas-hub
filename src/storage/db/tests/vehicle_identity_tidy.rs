// SPDX-License-Identifier: AGPL-3.0-only

//! C6-I2 tests (C6 section 8 Part I: C6 ST-I23 to ST-I28, with C6 R54 to R57,
//! R63, R64, R69 and R70): the schema step and the marker table, the shared
//! import literals and the one-time identity tidy at the read-write store open.
//! Synthetic data only; every store is a temporary one built through
//! `HubStore`.
//!
//! The tidy is content-blind (C6 R55), so the "before" values below are
//! arbitrary stand-ins and no test needs the replaced code's vocabulary. A
//! store's first open is stood for as the spec's tidy fixtures say: the store is
//! built through `HubStore`, the listed before `car_json` and lifecycle blob are
//! written directly by SQL and the marker row is deleted, so that the next
//! `HubStore::initialize` is the first read-write open of a store the replaced
//! code wrote.
//!
//! The "current view" half of C6 ST-I23 (`car.model_code` "3", `variant_name`
//! null) was carried to C6-S2, which owns the served car view; it is asserted
//! in `c6_st_i23_tidy_live` through the served summary's `car` view.

use super::*;
use crate::collection::teslamate_current::IMPORT_POSITION_RECORD_TYPE;
use crate::hub_pack::ProjectionCarPatch;
use crate::lifecycle::{OpenSessionState, drive_tests::T0};
use serde_json::{Value, json};

const V3: &str = "5YJ3E1EA0KFTEST01";
const VL: &str = "LRW3E7EK0MCTEST07";
const VS: &str = "5YJSA1E20HFTEST02";

// ---- fixtures ----

/// C6 section 8's `P0` (a parked response), every group stamped `t`.
fn p0(t: i64) -> Value {
    json!({
        "display_name": "Test Car",
        "vin": V3,
        "drive_state": {"timestamp": t, "shift_state": null, "speed": null, "power": 0,
            "latitude": 10.1234567, "longitude": 20.7654321, "heading": 90, "elevation": 50.4},
        "charge_state": {"timestamp": t, "battery_level": 80, "usable_battery_level": 79,
            "battery_range": 200.0, "ideal_battery_range": 210.0, "est_battery_range": 150.5,
            "charge_limit_soc": 90, "charging_state": "Disconnected",
            "conn_charge_cable": "<invalid>", "charger_power": 0, "charge_energy_added": 12.346,
            "charge_port_door_open": false, "time_to_full_charge": 0.0},
        "climate_state": {"timestamp": t, "inside_temp": 21.25, "outside_temp": 10.04,
            "is_climate_on": false},
        "vehicle_state": {"timestamp": t, "odometer": 1000.0, "car_version": "2024.14.3 aa1bb2",
            "locked": true, "sentry_mode": false, "df": 0, "dr": 0, "pf": 0, "pr": 0, "ft": 0,
            "rt": 0, "fd_window": 0, "fp_window": 0, "rd_window": 0, "rp_window": 0,
            "service_mode": false, "tpms_pressure_fl": 2.875,
            "software_update": {"status": "", "version": "", "download_perc": 0,
                "install_perc": 1}},
        "vehicle_config": {"timestamp": t, "car_type": "model3", "trim_badging": "74d",
            "exterior_color": "MidnightSilver", "wheel_type": "Pinwheel18",
            "spoiler_type": "None", "sun_roof_installed": 0}
    })
}

/// A vehicle-data payload of the owner (`fleet` false) or fleet variant, with an
/// optional root `provider_feed` (a sparse source, C1 R76).
fn vehicle_data_payload(fleet: bool, provider_feed: Option<&str>, data: Value) -> Value {
    let mut payload = json!({
        "record_type": if fleet { "fleet_api_vehicle_data_v1" } else { "owner_api_vehicle_data_v1" },
        "source_vehicle_id": "1",
        "source_vehicle_state": "online",
        "provider_raw_json": {"response": data},
    });
    if let Some(feed) = provider_feed {
        payload["provider_feed"] = json!(feed);
    }
    payload
}

/// The default before car of the tidy fixtures (C6 section 8), a live car.
fn before_car() -> ProjectionCar {
    ProjectionCar {
        id: 1,
        name: "Car".to_owned(),
        model: "M3 stand-in".to_owned(),
        vin: Some(V3.to_owned()),
        source_eid: None,
        source_vid: None,
        trim_badging: Some("74D-OLD".to_owned()),
        marketing_name: Some("Derived Name".to_owned()),
        exterior_color: Some("MidnightSilver".to_owned()),
        wheel_type: Some("Pinwheel18".to_owned()),
        spoiler_type: Some("None".to_owned()),
        source_model_text: None,
        firmware_version: Some("2024.14.3".to_owned()),
        efficiency_wh_per_km: Some(150.0),
        settings: Default::default(),
    }
}

/// The before `car_metadata` of the tidy fixtures: the three owned fields
/// (model, trim, marketing name) and every other patch field, with values the
/// tidy must leave as they are.
fn before_patch() -> ProjectionCarPatch {
    ProjectionCarPatch {
        name: Some("Test Car".to_owned()),
        model: Some("M3 stand-in".to_owned()),
        vin: Some(V3.to_owned()),
        trim_badging: Some("74D-OLD".to_owned()),
        marketing_name: Some("Derived Name".to_owned()),
        exterior_color: Some("MidnightSilver".to_owned()),
        wheel_type: Some("Pinwheel18".to_owned()),
        spoiler_type: Some("None".to_owned()),
        firmware_version: Some("2024.14.3".to_owned()),
    }
}

/// `before_patch` after the tidy: the three owned fields rederived, every other
/// field as before (C6 R56).
fn tidied_patch(model: Option<&str>, trim: Option<&str>) -> ProjectionCarPatch {
    ProjectionCarPatch {
        model: model.map(str::to_owned),
        trim_badging: trim.map(str::to_owned),
        marketing_name: None,
        ..before_patch()
    }
}

/// Everything the tidy may read or write for one vehicle, as stored bytes.
#[derive(Debug, Clone, PartialEq)]
struct Snap {
    car: Option<String>,
    blob: Option<Vec<u8>>,
    quarantined: Option<bool>,
    /// The lifecycle record's other columns, which the tidy never writes.
    lifecycle_car_id: Option<i64>,
    last_observation_id: Option<i64>,
    updated_at_ms: Option<i64>,
    /// `(completed_at_ms, summary_json)` of the tidy's marker row.
    marker: Option<(i64, String)>,
    /// How many `car` upsert sync mutations the vehicle has.
    car_mutations: i64,
    /// The export outbox's dirty revision for the vehicle.
    dirty_revision: Option<i64>,
}

fn snap(store: &HubStore, vehicle_id: &str) -> Snap {
    let connection = store.open().expect("catalogue");
    let car = connection
        .query_row(
            "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
            params![vehicle_id],
            |row| row.get(0),
        )
        .optional()
        .expect("read the car");
    let lifecycle: Option<(Vec<u8>, bool, i64, i64, i64)> = connection
        .query_row(
            "SELECT open_session_json, quarantined, car_id, last_observation_id, updated_at_ms
               FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
            params![vehicle_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .expect("read the lifecycle record");
    let marker = connection
        .query_row(
            "SELECT completed_at_ms, summary_json FROM store_data_steps WHERE step_key = ?1",
            params![VEHICLE_IDENTITY_TIDY_STEP_KEY],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .expect("read the marker");
    let car_mutations = connection
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND entity = 'car'",
            params![vehicle_id],
            |row| row.get(0),
        )
        .expect("count the car mutations");
    let dirty_revision = connection
        .query_row(
            "SELECT dirty_revision FROM export_outbox WHERE vehicle_id = ?1",
            params![vehicle_id],
            |row| row.get(0),
        )
        .optional()
        .expect("read the export revision");
    Snap {
        car,
        blob: lifecycle.as_ref().map(|record| record.0.clone()),
        quarantined: lifecycle.as_ref().map(|record| record.1),
        lifecycle_car_id: lifecycle.as_ref().map(|record| record.2),
        last_observation_id: lifecycle.as_ref().map(|record| record.3),
        updated_at_ms: lifecycle.as_ref().map(|record| record.4),
        marker,
        car_mutations,
        dirty_revision,
    }
}

/// The newest `car` sync mutation's payload.
fn newest_car_mutation(store: &HubStore, vehicle_id: &str) -> Option<(i64, String)> {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT entity_id, payload_json FROM sync_mutations
              WHERE vehicle_id = ?1 AND entity = 'car' ORDER BY revision DESC LIMIT 1",
            params![vehicle_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .expect("read the car mutation")
}

/// Write the before car and the before lifecycle blob by SQL: the blob the
/// accept functions left, decoded, with `car_metadata` set to `patch` and
/// re-encoded with C1's codec. `None` leaves a part as it is.
fn set_before(
    store: &HubStore,
    vehicle_id: &str,
    car: &ProjectionCar,
    patch: Option<ProjectionCarPatch>,
) {
    let connection = store.open().expect("catalogue");
    connection
        .execute(
            "UPDATE materialised_cars SET car_json = ?2 WHERE vehicle_id = ?1",
            params![vehicle_id, serde_json::to_string(car).expect("car json")],
        )
        .expect("write the before car");
    if let Some(patch) = patch {
        let stored: Vec<u8> = connection
            .query_row(
                "SELECT open_session_json FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![vehicle_id],
                |row| row.get(0),
            )
            .expect("the lifecycle record the accept left");
        let mut state = OpenSessionState::decode(&stored).expect("decode the blob");
        state.car_metadata = Some(patch);
        connection
            .execute(
                "UPDATE vehicle_lifecycle_state SET open_session_json = ?2 WHERE vehicle_id = ?1",
                params![vehicle_id, state.encode().expect("encode the blob")],
            )
            .expect("write the before blob");
    }
}

fn remove_marker(store: &HubStore) {
    store
        .open()
        .expect("catalogue")
        .execute(
            "DELETE FROM store_data_steps WHERE step_key = ?1",
            params![VEHICLE_IDENTITY_TIDY_STEP_KEY],
        )
        .expect("delete the marker");
}

fn accept(store: &HubStore, source: Uuid, vehicle: Uuid, payload: Value, t: i64, car_id: i64) {
    store
        .accept_owner_observation_and_lifecycle(
            &ObservationInput {
                source_id: source,
                vehicle_id: vehicle,
                observed_at_ms: t,
                payload,
            },
            t,
            car_id,
        )
        .expect("accept the observation");
}

/// A live store with one vehicle, `TS` of the tidy fixtures.
struct Ts {
    directory: tempfile::TempDir,
    store: HubStore,
    source: SourceRecord,
    vehicle: VehicleRecord,
}

impl Ts {
    fn new() -> Self {
        let directory = crate::private_tempdir().expect("tempdir");
        let store = HubStore::initialize(directory.path()).expect("store");
        let (source, vehicle) = test_registered_vehicle(&store);
        Self {
            directory,
            store,
            source,
            vehicle,
        }
    }

    fn vid(&self) -> String {
        self.vehicle.vehicle_id.to_string()
    }

    fn accept(&self, payload: Value, t: i64) {
        accept(
            &self.store,
            self.source.source_id,
            self.vehicle.vehicle_id,
            payload,
            t,
            1,
        );
    }

    /// The default tidy fixture: [VD(1, t0, P0)] accepted, then the default
    /// before car and blob written and the marker deleted.
    fn fixture() -> Self {
        let ts = Self::new();
        ts.accept(vehicle_data_payload(false, None, p0(T0)), T0);
        set_before(&ts.store, &ts.vid(), &before_car(), Some(before_patch()));
        remove_marker(&ts.store);
        ts
    }

    fn snap(&self) -> Snap {
        snap(&self.store, &self.vid())
    }

    /// "Open": `HubStore::initialize` on the same store.
    fn open(&self) -> Result<HubStore, StoreError> {
        HubStore::initialize(self.directory.path())
    }

    fn open_ok(&self) -> HubStore {
        self.open().expect("open the store")
    }
}

/// The car C6 ST-I23 expects after the tidy of the default fixture.
fn tidied_car() -> ProjectionCar {
    ProjectionCar {
        model: "3".to_owned(),
        trim_badging: Some("74d".to_owned()),
        marketing_name: None,
        ..before_car()
    }
}

/// The blob C6 ST-I23 expects after the tidy: the before state with only
/// `car_metadata` replaced.
fn tidied_blob(before_blob: &[u8]) -> Vec<u8> {
    let mut state = OpenSessionState::decode(before_blob).expect("decode the before blob");
    state.car_metadata = Some(tidied_patch(Some("3"), Some("74d")));
    state.encode().expect("encode the tidied blob")
}

fn counts(
    live: u64,
    imported: u64,
    unreadable: u64,
    cars: u64,
    records: u64,
    unrecognised: u64,
) -> Value {
    json!({
        "live_vehicles": live,
        "imported_vehicles_skipped": imported,
        "vehicles_skipped_unreadable": unreadable,
        "cars_changed": cars,
        "lifecycle_records_changed": records,
        "models_unrecognised": unrecognised,
    })
}

/// The marker's counts, which must hold exactly the six keys of C6 R57 and no
/// VIN, name, vehicle id or stored value.
fn marker_counts(snap: &Snap, vehicle_id: &str) -> Value {
    let (completed_at_ms, summary) = snap.marker.as_ref().expect("the marker row");
    assert!(*completed_at_ms > 0, "the marker carries a completion time");
    for private in [V3, VL, "Car", "Test Car", vehicle_id] {
        assert!(
            !summary.contains(private),
            "the summary holds no {private:?}"
        );
    }
    let value: Value = serde_json::from_str(summary).expect("the summary is JSON");
    assert_eq!(
        value.as_object().expect("an object").len(),
        6,
        "exactly six keys"
    );
    value
}

/// The lifecycle record's columns other than the blob stay as stored (C6 ST-I23:
/// `last_observation_id`, `quarantined`, `updated_at_ms`, and the record's car).
fn assert_record_columns_unchanged(before: &Snap, after: &Snap, label: &str) {
    assert!(before.last_observation_id.is_some(), "{label}: a record");
    assert_eq!(
        after.quarantined, before.quarantined,
        "{label}: quarantined"
    );
    assert_eq!(
        after.last_observation_id, before.last_observation_id,
        "{label}: last_observation_id"
    );
    assert_eq!(
        after.updated_at_ms, before.updated_at_ms,
        "{label}: updated_at_ms"
    );
    assert_eq!(
        after.lifecycle_car_id, before.lifecycle_car_id,
        "{label}: car_id"
    );
}

/// C6 ST-I23's result, shared by the first-open test and the schema-step test.
fn assert_st_i23_result(store: &HubStore, vehicle_id: &str, before: &Snap, after: &Snap) {
    assert_eq!(
        after.car.as_deref(),
        Some(
            serde_json::to_string(&tidied_car())
                .expect("car json")
                .as_str()
        ),
        "the car: model, trim and marketing name rederived, every other field as before"
    );
    let stored: Value = serde_json::from_str(after.car.as_deref().expect("a car")).expect("json");
    assert!(
        stored.get("source_model_text").is_none(),
        "no source_model_text key"
    );
    assert_eq!(
        after.blob.as_deref(),
        Some(tidied_blob(before.blob.as_deref().expect("a blob")).as_slice()),
        "the blob: car_metadata rederived, every other OpenSessionState field equal"
    );
    assert_record_columns_unchanged(before, after, "tidied");
    assert_eq!(marker_counts(after, vehicle_id), counts(1, 0, 0, 1, 1, 0));
    assert_eq!(
        after.car_mutations,
        before.car_mutations + 1,
        "one live-sync change for the car"
    );
    let (entity_id, payload) = newest_car_mutation(store, vehicle_id).expect("the car mutation");
    assert_eq!(entity_id, 1);
    assert_eq!(
        Some(payload),
        after.car,
        "the mutation carries the stored car"
    );
    assert_eq!(
        after.dirty_revision,
        before.dirty_revision.map(|revision| revision + 1),
        "the export is marked dirty once for the changed car"
    );
}

/// The one `cars` row of a pack built from `car` by the legacy writer's
/// marketing-name-carrying schema (the plain v1 `cars` table has no
/// marketing-name column): `(model, marketing)`.
fn legacy_pack_car_row(car: &ProjectionCar) -> (String, Option<String>) {
    let directory = crate::private_tempdir().expect("pack directory");
    let snapshot = ProjectionSnapshot {
        cars: vec![car.clone()],
        drives: Vec::new(),
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
    };
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: ProjectionBinding {
            installation_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            vehicle_id: Uuid::new_v4(),
            generation: 1,
            selected_car_id: car.id,
        },
        sequence: SequenceRange {
            from_exclusive: 6,
            to_inclusive: 7,
        },
        snapshot: &snapshot,
    };
    let built = ProjectionPackWriter::new(directory.path())
        .write_full_snapshot_with_states(&request, &[])
        .expect("write the legacy pack");
    let bytes = zstd::stream::decode_all(std::fs::File::open(&built.path).expect("open the pack"))
        .expect("decode the pack");
    let inspect = directory.path().join("inspect.sqlite");
    std::fs::write(&inspect, bytes).expect("write the inspection copy");
    let connection = rusqlite::Connection::open(inspect).expect("open the inspection copy");
    // The `cars` table must have the marketing-name column, so that "no
    // marketing name" is read from a column and not true by construction.
    let has_marketing_name: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('cars') WHERE name = 'marketing_name'",
            [],
            |row| row.get(0),
        )
        .expect("inspect the cars columns");
    assert_eq!(
        has_marketing_name, 1,
        "the pack's cars table has the column"
    );
    connection
        .query_row("SELECT model, marketing_name FROM cars", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("the one pack car")
}

// ---- C6 ST-I23 ----

#[test]
fn c6_st_i23_tidy_live() {
    let ts = Ts::fixture();
    let before = ts.snap();
    assert!(
        before.marker.is_none(),
        "the fixture stands for a first open"
    );
    assert_eq!(
        before.car.as_deref(),
        Some(serde_json::to_string(&before_car()).expect("json").as_str())
    );
    let store = ts.open_ok();
    let after = snap(&store, &ts.vid());
    assert_st_i23_result(&store, &ts.vid(), &before, &after);
    // The next legacy pack writes the tidied model and no marketing name.
    let stored: ProjectionCar =
        serde_json::from_str(after.car.as_deref().expect("a car")).expect("stored car");
    assert_eq!(legacy_pack_car_row(&stored), ("3".to_owned(), None));
    // The same check shows a stored marketing name, so the null above is read.
    assert_eq!(
        legacy_pack_car_row(&before_car()),
        ("M3 stand-in".to_owned(), Some("Derived Name".to_owned()))
    );
    // C6 ST-I23, served-view half (carried to C6-S2): the `current` view's
    // `car`, built from the stored car as the HTTP route reads it, has
    // `model_code` "3" and a null `variant_name`; the same view of the car
    // before the tidy shows the derived variant name, so the null is read.
    let served = |car: ProjectionCar| {
        crate::current_state::build_current_vehicle_summary(
            ts.vehicle.vehicle_id,
            &[],
            Some(car),
            None,
            None,
            T0,
            600_000,
        )
        .car
        .expect("a stored car gives a car view")
    };
    let stored_for_view = store
        .materialised_car_for_vehicle(ts.vehicle.vehicle_id)
        .expect("read the stored car")
        .expect("the stored car");
    assert_eq!(stored_for_view, stored);
    let view = served(stored_for_view);
    assert_eq!(view.model_code.as_deref(), Some("3"));
    assert_eq!(view.variant_name, None);
    assert_eq!(
        served(before_car()).variant_name.as_deref(),
        Some("Derived Name")
    );
}

// ---- C6 ST-I24 ----

/// One C6 ST-I24 case: the vehicle-data rows accepted (`None`: the default row
/// is accepted to create the car and the blob, then every current row is
/// deleted, so no vehicle-data row remains), the before car's VIN and model.
struct Case {
    /// `(fleet, provider_feed, time, data)` per row, in observation order.
    rows: Option<Vec<(bool, Option<&'static str>, i64, Value)>>,
    car_vin: Option<&'static str>,
    car_model: &'static str,
}

struct Outcome {
    car: ProjectionCar,
    state: OpenSessionState,
    counts: Value,
}

fn run_case(case: Case) -> Outcome {
    let ts = Ts::new();
    match &case.rows {
        Some(rows) => {
            for (fleet, feed, time, data) in rows {
                ts.accept(vehicle_data_payload(*fleet, *feed, data.clone()), *time);
            }
        }
        None => {
            ts.accept(vehicle_data_payload(false, None, p0(T0)), T0);
            ts.store
                .open()
                .expect("catalogue")
                .execute(
                    "DELETE FROM current_observations WHERE vehicle_id = ?1",
                    params![ts.vid()],
                )
                .expect("delete the current rows");
        }
    }
    let car = ProjectionCar {
        vin: case.car_vin.map(str::to_owned),
        model: case.car_model.to_owned(),
        ..before_car()
    };
    set_before(&ts.store, &ts.vid(), &car, Some(before_patch()));
    remove_marker(&ts.store);
    let store = ts.open_ok();
    let after = snap(&store, &ts.vid());
    let stored: ProjectionCar =
        serde_json::from_str(after.car.as_deref().expect("a car")).expect("car");
    assert_eq!(
        ProjectionCar {
            model: String::new(),
            trim_badging: None,
            marketing_name: None,
            ..stored.clone()
        },
        ProjectionCar {
            model: String::new(),
            trim_badging: None,
            marketing_name: None,
            ..car
        },
        "only the three owned fields change"
    );
    Outcome {
        car: stored,
        state: OpenSessionState::decode(after.blob.as_deref().expect("a blob")).expect("blob"),
        counts: marker_counts(&after, &ts.vid()),
    }
}

fn p0_with(t: i64, edit: impl FnOnce(&mut Value)) -> Value {
    let mut data = p0(t);
    edit(&mut data);
    data
}

fn row(t: i64, data: Value) -> Vec<(bool, Option<&'static str>, i64, Value)> {
    vec![(false, None, t, data)]
}

/// Assert the case's stored model and trim in the car and in the blob, and that
/// the marketing name is cleared in both.
fn assert_identity(outcome: &Outcome, model: &str, trim: Option<&str>, label: &str) {
    assert_eq!(outcome.car.model, model, "{label}: car model");
    assert_eq!(
        outcome.car.trim_badging.as_deref(),
        trim,
        "{label}: car trim"
    );
    assert_eq!(
        outcome.car.marketing_name, None,
        "{label}: car marketing name"
    );
    let patch = outcome.state.car_metadata.as_ref().expect("car_metadata");
    assert_eq!(
        patch.model.as_deref(),
        (!model.is_empty()).then_some(model),
        "{label}: blob model"
    );
    assert_eq!(patch.trim_badging.as_deref(), trim, "{label}: blob trim");
    assert_eq!(patch.marketing_name, None, "{label}: blob marketing name");
    assert_eq!(
        patch.exterior_color.as_deref(),
        Some("MidnightSilver"),
        "{label}: colour kept"
    );
    assert_eq!(
        patch,
        &tidied_patch((!model.is_empty()).then_some(model), trim),
        "{label}: every other patch field as before"
    );
}

#[test]
fn c6_st_i24_tidy_sources() {
    // (a) No vehicle-data rows, before model "M3 stand-in", VIN V3: a filed VIN.
    let a = run_case(Case {
        rows: None,
        car_vin: Some(V3),
        car_model: "M3 stand-in",
    });
    assert_identity(&a, "3", None, "a");
    assert_eq!(a.counts, counts(1, 0, 0, 1, 1, 0));

    // (b) No vehicle-data rows; no VIN in the stored car or in any row; the
    // blob patch's vin is not an input (C6 R56 2), so it must not decide. The
    // stored model is the text. "Model Y" is recognised, "lychee" passes
    // through and is counted, "" stays empty.
    for (model, expected, unrecognised) in
        [("Model Y", "Y", 0), ("lychee", "lychee", 1), ("", "", 0)]
    {
        let b = run_case(Case {
            rows: None,
            car_vin: None,
            car_model: model,
        });
        assert_identity(&b, expected, None, "b");
        assert_eq!(b.counts["models_unrecognised"], unrecognised, "b {model:?}");
    }

    // (c) Car VIN VL (not filed), no rows, before model "Model Y": the
    // recognised text wins over the VIN.
    let c = run_case(Case {
        rows: None,
        car_vin: Some(VL),
        car_model: "Model Y",
    });
    assert_identity(&c, "Y", None, "c");

    // (d) Car VIN None, P0 with car_type "models2": the row's root vin (V3,
    // filed) wins; without the root vin the text gives "S".
    let d1 = run_case(Case {
        rows: Some(row(
            T0,
            p0_with(T0, |data| {
                data["vehicle_config"]["car_type"] = json!("models2")
            }),
        )),
        car_vin: None,
        car_model: "M3 stand-in",
    });
    assert_identity(&d1, "3", Some("74d"), "d1");
    let d2 = run_case(Case {
        rows: Some(row(
            T0,
            p0_with(T0, |data| {
                data["vehicle_config"]["car_type"] = json!("models2");
                data.as_object_mut().expect("object").remove("vin");
            }),
        )),
        car_vin: None,
        car_model: "M3 stand-in",
    });
    assert_identity(&d2, "S", Some("74d"), "d2");

    // (e) Two rows: the fleet row is newer and states the trim; a null from a
    // full source is the newest statement, so the stored trim is no fallback; a
    // null from a sparse source is no statement.
    let newer = |trim: Value, feed: Option<&'static str>| {
        vec![
            (false, None, T0, p0(T0)),
            (
                true,
                feed,
                T0 + 60_000,
                json!({"vehicle_config": {"timestamp": T0 + 60_000, "car_type": "model3",
                                           "trim_badging": trim}}),
            ),
        ]
    };
    let e1 = run_case(Case {
        rows: Some(newer(json!("74"), None)),
        car_vin: Some(V3),
        car_model: "M3 stand-in",
    });
    assert_identity(&e1, "3", Some("74"), "e1");
    let e2 = run_case(Case {
        rows: Some(newer(Value::Null, None)),
        car_vin: Some(V3),
        car_model: "M3 stand-in",
    });
    assert_identity(&e2, "3", None, "e2");
    let e3 = run_case(Case {
        rows: Some(newer(Value::Null, Some("provider_stream"))),
        car_vin: Some(V3),
        car_model: "M3 stand-in",
    });
    assert_identity(&e3, "3", Some("74d"), "e3");

    // (f) The trim is the trimmed live text, or None when it is too long.
    let f1 = run_case(Case {
        rows: Some(row(
            T0,
            p0_with(T0, |data| {
                data["vehicle_config"]["trim_badging"] = json!(" 74d ")
            }),
        )),
        car_vin: Some(V3),
        car_model: "M3 stand-in",
    });
    assert_identity(&f1, "3", Some("74d"), "f1");
    let f2 = run_case(Case {
        rows: Some(row(
            T0,
            p0_with(T0, |data| {
                data["vehicle_config"]["trim_badging"] = json!("t".repeat(257))
            }),
        )),
        car_vin: Some(V3),
        car_model: "M3 stand-in",
    });
    assert_identity(&f2, "3", None, "f2");

    // (g) Car VIN VL and P0 car_type "lychee": the VIN code beats an
    // unrecognised text.
    let g = run_case(Case {
        rows: Some(row(
            T0,
            p0_with(T0, |data| {
                data["vehicle_config"]["car_type"] = json!("lychee")
            }),
        )),
        car_vin: Some(VL),
        car_model: "M3 stand-in",
    });
    assert_identity(&g, "3", Some("74d"), "g");
    assert_eq!(g.counts["models_unrecognised"], 0);

    // (h) The stored VIN is the VIN (C6 R56 2): a stored VS beats P0's root vin
    // V3, so the filed decoding is "S" and not "3".
    let h = run_case(Case {
        rows: Some(row(T0, p0(T0))),
        car_vin: Some(VS),
        car_model: "M3 stand-in",
    });
    assert_identity(&h, "S", Some("74d"), "h");

    // (i) The statement time orders before the observation id (C6 R23, R24): the
    // owner row (id 1, observed at t0 + 60,000) states trim "A" in a group
    // stamped t0 + 60,000; the later fleet row (id 2, observed at t0 + 120,000)
    // states trim "B" in a group stamped t0. Both stamps are plausible, so "A"
    // is the newest statement. An order by id alone or by `observed_at_ms` alone
    // would give "B".
    let config = |observed: i64, timestamp: i64, trim: &str| {
        p0_with(observed, |data| {
            data["vehicle_config"]["timestamp"] = json!(timestamp);
            data["vehicle_config"]["trim_badging"] = json!(trim);
        })
    };
    let i = run_case(Case {
        rows: Some(vec![
            (
                false,
                None,
                T0 + 60_000,
                config(T0 + 60_000, T0 + 60_000, "A"),
            ),
            (true, None, T0 + 120_000, config(T0 + 120_000, T0, "B")),
        ]),
        car_vin: Some(V3),
        car_model: "M3 stand-in",
    });
    assert_identity(&i, "3", Some("A"), "i");

    // (i2) The tie on the statement time goes to the greater observation id:
    // both rows state their trim at t0 + 60,000 (the second by its observation
    // time, its stamp being absent), so the fleet row's "B" wins.
    let i2 = run_case(Case {
        rows: Some(vec![
            (
                false,
                None,
                T0 + 60_000,
                config(T0 + 60_000, T0 + 60_000, "A"),
            ),
            (
                true,
                None,
                T0 + 60_000,
                json!({"vehicle_config": {"car_type": "model3", "trim_badging": "B"}}),
            ),
        ]),
        car_vin: Some(V3),
        car_model: "M3 stand-in",
    });
    assert_identity(&i2, "3", Some("B"), "i2");

    // (j) A group stamp later than the observation time plus the skew allowance
    // is not a statement time: the observation time stands (C6 R23, C1 R5). The
    // owner row's group is stamped far ahead, so trusting it would make "A" the
    // newest; its observation time t0 + 120,000 is before the fleet row's, so
    // "B" is.
    let j = run_case(Case {
        rows: Some(vec![
            (
                false,
                None,
                T0 + 120_000,
                config(T0 + 120_000, T0 + 120_000 + 200_000, "A"),
            ),
            (
                true,
                None,
                T0 + 180_000,
                config(T0 + 180_000, T0 + 180_000, "B"),
            ),
        ]),
        car_vin: Some(V3),
        car_model: "M3 stand-in",
    });
    assert_identity(&j, "3", Some("B"), "j");

    // (k) A row whose data object sits under `vehicle_data`, with no
    // `provider_raw_json`, states the trim and the model (C1 R3). The data holds
    // no `vin`, so the model is the `car_type` text.
    let mut payload = vehicle_data_payload(false, None, json!({}));
    payload
        .as_object_mut()
        .expect("object")
        .remove("provider_raw_json");
    payload["vehicle_data"] = p0_with(T0, |data| {
        data["vehicle_config"]["trim_badging"] = json!("VD-TRIM");
        data["vehicle_config"]["car_type"] = json!("modely");
        data.as_object_mut().expect("object").remove("vin");
    });
    let ts = Ts::new();
    ts.accept(payload, T0);
    set_before(
        &ts.store,
        &ts.vid(),
        &ProjectionCar {
            vin: None,
            ..before_car()
        },
        Some(before_patch()),
    );
    remove_marker(&ts.store);
    let store = ts.open_ok();
    let after = snap(&store, &ts.vid());
    let car: ProjectionCar =
        serde_json::from_str(after.car.as_deref().expect("a car")).expect("car");
    assert_eq!(
        (car.model.as_str(), car.trim_badging.as_deref()),
        ("Y", Some("VD-TRIM")),
        "k: the vehicle_data object is read"
    );

    // (l) No VIN in the stored car or in any row; the blob patch's vin is not
    // an input (C6 R56 2), so it must not decide. A stored model "Model Y": a
    // newest `car_type` statement of null (a full source) is a statement
    // without text, so the stored model is the text (C6 R56 3).
    let l = run_case(Case {
        rows: Some(row(
            T0,
            p0_with(T0, |data| {
                data["vehicle_config"]["car_type"] = Value::Null;
                data.as_object_mut().expect("object").remove("vin");
            }),
        )),
        car_vin: None,
        car_model: "Model Y",
    });
    assert_identity(&l, "Y", Some("74d"), "l");
}

// ---- C6 ST-I23 (b) ----

#[test]
fn c6_st_i23b_tidy_preserves_inherited_walk() {
    use crate::lifecycle::b3_c1_b::{inherited_state, old_blob, st111_obs1, st111_obs2};
    let ts = Ts::new();
    // The before blob is C1 ST111 (a)'s old blob (an open drive 1 lacking every
    // C1 R12 field, next position id 14) with `car_metadata` set as in the tidy
    // fixtures, and the drive's three open positions as ST111 (b) states them.
    let mut state = inherited_state(1000.4);
    state.car_metadata = Some(before_patch());
    let blob = old_blob(&state);
    let positions = [(11_i64, -3_i64, 100_i64), (12, -2, 101), (13, -1, 104)]
        .map(|(id, minutes, elevation)| {
            let mut position = position_row(
                id,
                Some(1),
                T0 + minutes * crate::lifecycle::drive_tests::MIN,
            );
            position.elevation = Some(elevation);
            position
        })
        .to_vec();
    ts.store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: ts.vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &blob,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: T0,
            delta: &crate::lifecycle::LifecycleDelta {
                open_drive_positions: positions,
                ..Default::default()
            },
        })
        .expect("store the old state and its open positions");
    // No vehicle-data rows (as C6 ST-I24 (a)); the before car and blob as the
    // tidy fixtures; the marker deleted.
    set_before(&ts.store, &ts.vid(), &before_car(), None);
    remove_marker(&ts.store);
    let before = ts.snap();
    let before_state =
        OpenSessionState::decode(before.blob.as_deref().expect("blob")).expect("decode");
    assert!(
        before_state
            .open_drive
            .as_ref()
            .is_some_and(|drive| drive.walk_inherited),
        "the before blob carries an earlier build's walk"
    );

    let store = ts.open_ok();
    let after = snap(&store, &ts.vid());
    let after_state =
        OpenSessionState::decode(after.blob.as_deref().expect("blob")).expect("decode");
    let mut expected = before_state.clone();
    expected.car_metadata = Some(tidied_patch(Some("3"), None));
    assert_eq!(
        after_state, expected,
        "only car_metadata changed; every other field is equal"
    );
    let drive = after_state.open_drive.as_ref().expect("the open drive");
    assert!(
        drive.walk_inherited || drive.last_folded_at_ms.is_none(),
        "the re-encoded blob still lets C1 detect the inherited walk"
    );
    assert_eq!(after_state.next_position_id, 14);
    assert_eq!(
        marker_counts(&after, &ts.vid())["lifecycle_records_changed"],
        1
    );

    // C1 ST111 (b)'s observations 1 and 2 close the drive: C1 R93's recompute
    // runs, row 1 has ascent 10 and descent 2; the drive's mutations are the
    // closing upsert (43, 7), then one upsert (10, 2) after its positions.
    feed_owner_samples(
        &store,
        &ts.source,
        &ts.vehicle,
        &[st111_obs1(), st111_obs2()],
    );
    let connection = store.open().expect("catalogue");
    let (ascent, descent): (Option<i64>, Option<i64>) = connection
        .query_row(
            "SELECT ascent, descent FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = 1",
            params![ts.vid()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("drive row 1");
    assert_eq!(
        (ascent, descent),
        (Some(10), Some(2)),
        "the corrective recompute ran at the close"
    );
    let mut statement = connection
        .prepare(
            "SELECT revision, payload_json FROM sync_mutations
              WHERE vehicle_id = ?1 AND entity = 'drive' AND entity_id = 1 ORDER BY revision",
        )
        .expect("statement");
    let drive_mutations = statement
        .query_map(params![ts.vid()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .expect("rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect");
    let totals = |payload: &str| {
        let value: Value = serde_json::from_str(payload).expect("payload");
        (value["ascent"].as_i64(), value["descent"].as_i64())
    };
    assert_eq!(
        drive_mutations.len(),
        2,
        "the closing upsert and the corrective one"
    );
    assert_eq!(totals(&drive_mutations[0].1), (Some(43), Some(7)));
    assert_eq!(totals(&drive_mutations[1].1), (Some(10), Some(2)));
    let (first_position, last_position): (i64, i64) = connection
        .query_row(
            "SELECT MIN(revision), MAX(revision) FROM sync_mutations
              WHERE vehicle_id = ?1 AND entity = 'position'",
            params![ts.vid()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("position mutations");
    assert!(
        drive_mutations[0].0 < first_position,
        "the closing upsert comes first"
    );
    assert!(
        last_position < drive_mutations[1].0,
        "the corrective upsert follows the positions"
    );
}

// ---- C6 ST-I25 ----

#[test]
fn c6_st_i25_once_restart_rollback() {
    // (1) Open again: the bytes and the marker row equal the first open's, and
    // no live-sync change is recorded.
    let ts = Ts::fixture();
    let first = ts.snap();
    let store = ts.open_ok();
    let after_first = snap(&store, &ts.vid());
    assert_st_i23_result(&store, &ts.vid(), &first, &after_first);
    let store = ts.open_ok();
    assert_eq!(
        snap(&store, &ts.vid()),
        after_first,
        "a second open changes nothing"
    );

    // (2) Delete the marker and open again: the bytes equal the first open's and
    // the marker is written again with no change counted.
    remove_marker(&store);
    let store = ts.open_ok();
    let rerun = snap(&store, &ts.vid());
    assert_eq!(rerun.car, after_first.car);
    assert_eq!(rerun.blob, after_first.blob);
    assert_eq!(
        rerun.car_mutations, after_first.car_mutations,
        "no live-sync change"
    );
    assert_eq!(rerun.dirty_revision, after_first.dirty_revision);
    assert_eq!(marker_counts(&rerun, &ts.vid()), counts(1, 0, 0, 0, 0, 0));

    // (3) An interrupted run: a trigger aborts every insert into the marker
    // table. The open fails with an error that names the step and holds no VIN;
    // nothing is written (no car, blob, mutation, export change or marker). With
    // the trigger dropped the next open gives C6 ST-I23's result.
    let ts = Ts::fixture();
    let before = ts.snap();
    ts.store
        .open()
        .expect("catalogue")
        .execute_batch(
            "CREATE TRIGGER c6_i2_abort_marker BEFORE INSERT ON store_data_steps
             BEGIN SELECT RAISE(ABORT, 'marker insert aborted'); END;",
        )
        .expect("create the trigger");
    let Err(error) = ts.open() else {
        panic!("the aborted marker insert must fail the open");
    };
    assert!(
        matches!(error, StoreError::VehicleIdentityTidy(_)),
        "{error:?}"
    );
    let text = error.to_string();
    assert!(
        text.contains(VEHICLE_IDENTITY_TIDY_STEP_KEY),
        "the text names the step: {text}"
    );
    for private in [V3, "Car", &ts.vid()] {
        assert!(!text.contains(private), "the error holds no {private:?}");
    }
    assert_eq!(
        ts.snap(),
        before,
        "a failed tidy rolls everything back, marker included"
    );
    ts.store
        .open()
        .expect("catalogue")
        .execute_batch("DROP TRIGGER c6_i2_abort_marker;")
        .expect("drop the trigger");
    let store = ts.open_ok();
    assert_st_i23_result(&store, &ts.vid(), &before, &snap(&store, &ts.vid()));

    // (4) No other open runs the tidy: a read-only open, the control path
    // (`open_existing`), the standalone preflight and the immutable open leave
    // every byte unchanged and write no marker.
    let ts = Ts::fixture();
    let before = ts.snap();
    HubStore::open_read_only(ts.directory.path()).expect("read-only open");
    HubStore::open_existing(ts.directory.path()).expect("control-path open");
    HubStore::preflight_standalone_schema_read_only(ts.directory.path()).expect("preflight");
    assert_eq!(
        ts.snap(),
        before,
        "read-only opens change nothing and write no marker"
    );
    ts.store
        .checkpoint_catalogue_for_immutable_read()
        .expect("checkpoint for the immutable open");
    let immutable =
        HubStore::open_immutable_read_only(ts.directory.path()).expect("immutable open");
    immutable
        .verify_immutable_snapshot_unchanged()
        .expect("the immutable snapshot is unchanged");
    assert_eq!(
        ts.snap(),
        before,
        "diagnostics change nothing and write no marker"
    );
}

// ---- C6 ST-I26 ----

/// Publish `car` as an import does (the direct materialised base of the import
/// finalisation path), on the vehicle `v2_base_manifest` registered.
fn import_finalise(
    store: &HubStore,
    vehicle: &VehicleRecord,
    binding: &ProjectionBinding,
    manifest: &SyncManifest,
    car: &ProjectionCar,
) {
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
        )
        .expect("staging the import");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("staging the session");
    let state = create_direct_import_projection_state(store, run_id, 32);
    let mut capture =
        crate::teslamate_projection_state::TeslaMateProjectionStateCapture::for_initial_base(state);
    capture.record_car(car).expect("capture the car");
    capture.seal().expect("seal the state");
    let state = capture.into_state();
    store
        .finalize_import_generation_with_projection_state_and_materialisation(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
            manifest,
            Sha256Digest::of_bytes(b"c6-i2-materialised-base"),
            &[],
            binding,
            &state,
            car,
            &[],
            false,
        )
        .expect("publish the imported base");
}

/// The import-evidence routes of C6 R55 that ST-I26 covers, one store each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// (a) the stored car's `source_model_text`.
    SourceModelText,
    /// (b) a current row of the import-source position type.
    PositionRow,
    /// (c) `vehicles.source_id` names a source of the import kind.
    VehicleSource,
    /// (c) only `vehicle_identity_aliases.source_id` names such a source.
    Alias,
    /// (c) `lifecycle_source_watermarks` names such a source.
    Watermark,
    /// (c) the import's projection head row.
    ProjectionHead,
    /// (c) the import's projection-state head row.
    StateHead,
    /// (c) an import generation row.
    Generation,
    /// (c) the import finalisation path of a synthetic source car (all records).
    Finalised,
}

/// A store with one vehicle whose only import evidence is `route`, holding the
/// rows [VD(1, t0, P0)] and the default before car and blob, marker deleted.
fn route_store(route: Route) -> (tempfile::TempDir, HubStore, String) {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let mut car = before_car();
    let (source, vehicle, car_id): (Uuid, Uuid, i64) = match route {
        Route::SourceModelText | Route::PositionRow | Route::Alias | Route::Watermark => {
            let (source, vehicle) = test_registered_vehicle(&store);
            (source.source_id, vehicle.vehicle_id, 1)
        }
        Route::VehicleSource => {
            let source = store
                .register_source(
                    &SourceDescriptor::new(IMPORT_SOURCE_KIND, "i2-route"),
                    1_000,
                )
                .expect("import source");
            let vehicle = store
                .register_vehicle(&VehicleDescriptor::new(source.source_id, "veh"), 1_001)
                .expect("vehicle under the import source");
            (source.source_id, vehicle.vehicle_id, 1)
        }
        Route::ProjectionHead | Route::StateHead | Route::Generation | Route::Finalised => {
            let (vehicle, binding, manifest) = v2_base_manifest(&store);
            match route {
                Route::Generation => {
                    store
                        .begin_import_generation(
                            binding.account_id,
                            vehicle.vehicle_id,
                            binding.selected_car_id,
                            2_000,
                        )
                        .expect("staging generation");
                }
                Route::Finalised => {
                    import_finalise(
                        &store,
                        &vehicle,
                        &binding,
                        &manifest,
                        &import_delta_test_car(binding.selected_car_id),
                    );
                }
                _ => {
                    let inventory = TeslaMateImportProjectionInventory {
                        source_id: binding.account_id,
                        selected_car_id: binding.selected_car_id,
                        rows: Vec::new(),
                    };
                    let state = test_projection_state(
                        directory.path(),
                        &import_delta_test_car(binding.selected_car_id),
                    );
                    store
                        .finalize_teslamate_import_snapshot_with_projection_state(
                            &manifest,
                            Sha256Digest::of_bytes(b"c6-i2-route"),
                            &[],
                            &binding,
                            &inventory,
                            &state,
                        )
                        .expect("catalogue base and state");
                }
            }
            (
                vehicle.source_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
            )
        }
    };
    let vehicle_id = vehicle.to_string();
    accept(
        &store,
        source,
        vehicle,
        vehicle_data_payload(false, None, p0(T0)),
        T0,
        car_id,
    );
    let connection = store.open().expect("catalogue");
    let count = |table: &str| -> i64 {
        connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE vehicle_id = ?1"),
                params![vehicle_id],
                |row| row.get(0),
            )
            .expect("count")
    };
    match route {
        Route::SourceModelText => car.source_model_text = Some("Model 3".to_owned()),
        Route::PositionRow => {
            store
                .record_teslamate_current(
                    &ObservationInput {
                        source_id: source,
                        vehicle_id: vehicle,
                        observed_at_ms: T0,
                        payload: json!({"record_type": IMPORT_POSITION_RECORD_TYPE,
                                        "position_id": 7, "battery_level": 80}),
                    },
                    T0,
                )
                .expect("the import-source position row");
        }
        Route::Alias => {
            // A live vehicle that an import later reused: only its alias row
            // names the import source; `vehicles.source_id` stays the live one.
            let connection = store.open().expect("catalogue");
            let import = store
                .register_source(
                    &SourceDescriptor::new(IMPORT_SOURCE_KIND, "i2-alias"),
                    1_000,
                )
                .expect("import source");
            connection
                .execute(
                    "INSERT INTO vehicle_identity_aliases(
                        alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key
                     ) VALUES ('source_key', 'import-key', ?1, ?2, 'import-key')",
                    params![vehicle_id, import.source_id.to_string()],
                )
                .expect("the import alias");
            let own: String = connection
                .query_row(
                    "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
                    params![vehicle_id],
                    |row| row.get(0),
                )
                .expect("source");
            assert_eq!(
                own,
                source.to_string(),
                "the vehicle's own source stays the live one"
            );
        }
        Route::Watermark => {
            let import = store
                .register_source(
                    &SourceDescriptor::new(IMPORT_SOURCE_KIND, "i2-watermark"),
                    1_000,
                )
                .expect("import source");
            store
                .open()
                .expect("catalogue")
                .execute(
                    "INSERT INTO lifecycle_source_watermarks(
                        source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms
                     ) VALUES (?1, ?2, ?3, 5, ?4)",
                    params![
                        import.source_id.to_string(),
                        vehicle_id,
                        WatermarkDomain::ChargingProcesses.as_str(),
                        T0
                    ],
                )
                .expect("the import watermark");
        }
        Route::ProjectionHead => {
            // Isolate the one table: the state head goes, the head stays.
            store
                .open()
                .expect("catalogue")
                .execute(
                    "DELETE FROM teslamate_import_projection_state_heads WHERE vehicle_id = ?1",
                    params![vehicle_id],
                )
                .expect("remove the state head");
            assert_eq!(count("teslamate_import_projection_heads"), 1);
            assert_eq!(count("teslamate_import_projection_state_heads"), 0);
        }
        Route::StateHead => {
            store
                .open()
                .expect("catalogue")
                .execute(
                    "DELETE FROM teslamate_import_projection_heads WHERE vehicle_id = ?1",
                    params![vehicle_id],
                )
                .expect("remove the head");
            assert_eq!(count("teslamate_import_projection_heads"), 0);
            assert_eq!(count("teslamate_import_projection_state_heads"), 1);
        }
        Route::Generation => assert_eq!(count("import_generations"), 1),
        Route::VehicleSource => {
            // Isolate the one column: registration also wrote an alias row for
            // the import source, which a catalogue from before the aliases lacks.
            store
                .open()
                .expect("catalogue")
                .execute(
                    "DELETE FROM vehicle_identity_aliases WHERE vehicle_id = ?1",
                    params![vehicle_id],
                )
                .expect("remove the aliases");
            assert_eq!(count("vehicle_identity_aliases"), 0);
        }
        Route::Finalised => {}
    }
    drop(connection);
    car.id = car_id;
    set_before(
        &store,
        &vehicle_id,
        &ProjectionCar {
            trim_badging: Some(String::new()),
            ..car
        },
        Some(before_patch()),
    );
    remove_marker(&store);
    (directory, store, vehicle_id)
}

#[test]
fn c6_st_i26_imported_untouched() {
    // The spec's store: four vehicles A, B, C and D, each with the rows
    // [VD(n, t0, P0)] and the default before car and blob. A's evidence is the
    // import finalisation path (R55 c; its before trim is Some("")), B's is a
    // car with `source_model_text` Some("Model 3") (R55 a), C's is an
    // import-source position row as well (R55 b), D is live. A, B and C also
    // hold live rows, so a vehicle with both counts as imported.
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let live = store
        .register_source(
            &SourceDescriptor::new("tesla_owner_api", "account-test"),
            1_000,
        )
        .expect("live source");
    let register = |key: &str| {
        store
            .register_vehicle(&VehicleDescriptor::new(live.source_id, key), 1_001)
            .expect("vehicle")
    };
    let (b, c, d) = (register("veh-b"), register("veh-c"), register("veh-d"));
    let (a, binding, manifest) = v2_base_manifest(&store);
    import_finalise(
        &store,
        &a,
        &binding,
        &manifest,
        &import_delta_test_car(binding.selected_car_id),
    );
    let vehicles = [
        (&a, a.source_id, binding.selected_car_id),
        (&b, live.source_id, 1),
        (&c, live.source_id, 1),
        (&d, live.source_id, 1),
    ];
    for (vehicle, source, car_id) in vehicles {
        accept(
            &store,
            source,
            vehicle.vehicle_id,
            vehicle_data_payload(false, None, p0(T0)),
            T0,
            car_id,
        );
    }
    store
        .record_teslamate_current(
            &ObservationInput {
                source_id: live.source_id,
                vehicle_id: c.vehicle_id,
                observed_at_ms: T0,
                payload: json!({"record_type": IMPORT_POSITION_RECORD_TYPE, "position_id": 7}),
            },
            T0,
        )
        .expect("C's import-source position row");
    let a_car = ProjectionCar {
        id: binding.selected_car_id,
        trim_badging: Some(String::new()),
        ..before_car()
    };
    let b_car = ProjectionCar {
        source_model_text: Some("Model 3".to_owned()),
        ..before_car()
    };
    for (vehicle, car) in [
        (&a, &a_car),
        (&b, &b_car),
        (&c, &before_car()),
        (&d, &before_car()),
    ] {
        set_before(
            &store,
            &vehicle.vehicle_id.to_string(),
            car,
            Some(before_patch()),
        );
    }
    remove_marker(&store);
    let before: Vec<Snap> = [&a, &b, &c, &d]
        .iter()
        .map(|v| snap(&store, &v.vehicle_id.to_string()))
        .collect();
    drop(store);

    let store = HubStore::initialize(directory.path()).expect("the first open");
    let after: Vec<Snap> = [&a, &b, &c, &d]
        .iter()
        .map(|v| snap(&store, &v.vehicle_id.to_string()))
        .collect();
    for (index, label) in ["A", "B", "C"].iter().enumerate() {
        assert_eq!(
            (
                &after[index].car,
                &after[index].blob,
                after[index].car_mutations,
                after[index].dirty_revision
            ),
            (
                &before[index].car,
                &before[index].blob,
                before[index].car_mutations,
                before[index].dirty_revision
            ),
            "{label} is imported: car and blob bytes and live-sync changes are unchanged"
        );
    }
    let d_id = d.vehicle_id.to_string();
    assert_eq!(
        after[3].car.as_deref(),
        Some(serde_json::to_string(&tidied_car()).expect("json").as_str()),
        "D is tidied as in C6 ST-I23"
    );
    assert_eq!(
        after[3].blob.as_deref(),
        Some(tidied_blob(before[3].blob.as_deref().expect("blob")).as_slice())
    );
    assert_eq!(after[3].car_mutations, before[3].car_mutations + 1);
    assert_eq!(marker_counts(&after[3], &d_id), counts(1, 3, 0, 1, 1, 0));

    // Every import-evidence route alone keeps the car and the blob bytes and
    // records no change: the vehicle counts as imported (none is live).
    for route in [
        Route::SourceModelText,
        Route::PositionRow,
        Route::VehicleSource,
        Route::Alias,
        Route::Watermark,
        Route::ProjectionHead,
        Route::StateHead,
        Route::Generation,
        Route::Finalised,
    ] {
        let (directory, store, vehicle_id) = route_store(route);
        let before = snap(&store, &vehicle_id);
        drop(store);
        let store = HubStore::initialize(directory.path()).expect("open");
        let after = snap(&store, &vehicle_id);
        assert_eq!(
            (
                &after.car,
                &after.blob,
                after.car_mutations,
                after.dirty_revision
            ),
            (
                &before.car,
                &before.blob,
                before.car_mutations,
                before.dirty_revision
            ),
            "route {route:?} keeps the vehicle untouched"
        );
        assert_eq!(
            marker_counts(&after, &vehicle_id),
            counts(0, 1, 0, 0, 0, 0),
            "route {route:?}"
        );
    }
}

// ---- C6 ST-I27 ----

/// One C6 ST-I27 case on the default fixture, edited before the open.
fn skip_case(edit: impl FnOnce(&Ts)) -> (Ts, Snap, Snap) {
    let ts = Ts::fixture();
    edit(&ts);
    let before = ts.snap();
    let store = ts.open_ok();
    let after = snap(&store, &ts.vid());
    (ts, before, after)
}

fn assert_skipped_whole(ts: &Ts, before: &Snap, after: &Snap, label: &str) {
    assert_eq!(after.car, before.car, "{label}: car unchanged");
    assert_eq!(after.blob, before.blob, "{label}: blob unchanged");
    assert_record_columns_unchanged(before, after, label);
    assert_eq!(
        after.car_mutations, before.car_mutations,
        "{label}: no live-sync change"
    );
    assert_eq!(
        marker_counts(after, &ts.vid()),
        counts(1, 0, 1, 0, 0, 0),
        "{label}"
    );
}

#[test]
fn c6_st_i27_skip_whole_vehicle() {
    // Blob bytes that are not JSON.
    let (ts, before, after) = skip_case(|ts| {
        ts.store
            .open()
            .expect("catalogue")
            .execute(
                "UPDATE vehicle_lifecycle_state SET open_session_json = ?2 WHERE vehicle_id = ?1",
                params![ts.vid(), b"not json".to_vec()],
            )
            .expect("write the bytes");
    });
    assert_eq!(before.blob.as_deref(), Some(b"not json".as_slice()));
    assert_skipped_whole(&ts, &before, &after, "undecodable blob");

    // A quarantined record with a decodable blob.
    let (ts, before, after) = skip_case(|ts| {
        ts.store
            .open()
            .expect("catalogue")
            .execute(
                "UPDATE vehicle_lifecycle_state SET quarantined = 1 WHERE vehicle_id = ?1",
                params![ts.vid()],
            )
            .expect("quarantine");
    });
    assert_eq!(after.quarantined, Some(true));
    assert_skipped_whole(&ts, &before, &after, "quarantined");

    // A car that does not deserialise.
    let (ts, before, after) = skip_case(|ts| {
        ts.store
            .open()
            .expect("catalogue")
            .execute(
                "UPDATE materialised_cars SET car_json = '{\"id\":1}' WHERE vehicle_id = ?1",
                params![ts.vid()],
            )
            .expect("write the car");
    });
    assert_eq!(before.car.as_deref(), Some("{\"id\":1}"));
    assert_skipped_whole(&ts, &before, &after, "car not deserialising");

    // A car whose own id is not positive cannot carry the sync mutation of a
    // change: the whole vehicle is a skip.
    let (ts, before, after) = skip_case(|ts| {
        set_before(
            &ts.store,
            &ts.vid(),
            &ProjectionCar {
                id: 0,
                ..before_car()
            },
            None,
        );
    });
    assert_skipped_whole(&ts, &before, &after, "car id not positive");

    // A vehicle whose id is not a UUID cannot be named in the sync log: a
    // changing car makes the whole vehicle a skip.
    let ts = Ts::new();
    {
        let connection = ts.store.open().expect("catalogue");
        connection
            .execute(
                "INSERT INTO vehicles(vehicle_id, source_id, source_vehicle_key, created_at_ms,
                                      last_seen_at_ms)
                 VALUES ('not-a-uuid', ?1, 'i2-not-a-uuid', 1, 1)",
                params![ts.source.source_id.to_string()],
            )
            .expect("a vehicle with a plain id");
        connection
            .execute(
                "INSERT INTO materialised_cars(vehicle_id, car_id, car_json) VALUES ('not-a-uuid', 1, ?1)",
                params![serde_json::to_string(&before_car()).expect("car json")],
            )
            .expect("its car");
    }
    remove_marker(&ts.store);
    let store = ts.open_ok();
    let stored: String = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT car_json FROM materialised_cars WHERE vehicle_id = 'not-a-uuid'",
            [],
            |row| row.get(0),
        )
        .expect("the car");
    assert_eq!(
        stored,
        serde_json::to_string(&before_car()).expect("car json"),
        "the car of a vehicle the sync log cannot name is not written"
    );
    assert_eq!(
        marker_counts(&snap(&store, &ts.vid()), &ts.vid()),
        counts(1, 0, 1, 0, 0, 0)
    );

    // `car_metadata` None: the blob bytes stay and the car is tidied as in
    // C6 ST-I23; the record counts as unchanged.
    let (ts, before, after) = skip_case(|ts| {
        set_before(&ts.store, &ts.vid(), &before_car(), None);
        let connection = ts.store.open().expect("catalogue");
        let stored: Vec<u8> = connection
            .query_row(
                "SELECT open_session_json FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![ts.vid()],
                |row| row.get(0),
            )
            .expect("blob");
        let mut state = OpenSessionState::decode(&stored).expect("decode");
        state.car_metadata = None;
        connection
            .execute(
                "UPDATE vehicle_lifecycle_state SET open_session_json = ?2 WHERE vehicle_id = ?1",
                params![ts.vid(), state.encode().expect("encode")],
            )
            .expect("write the blob");
    });
    assert_eq!(
        after.blob, before.blob,
        "no car_metadata: the blob is not written"
    );
    assert_record_columns_unchanged(&before, &after, "no car_metadata");
    assert_eq!(
        after.car.as_deref(),
        Some(serde_json::to_string(&tidied_car()).expect("json").as_str())
    );
    assert_eq!(marker_counts(&after, &ts.vid()), counts(1, 0, 0, 1, 0, 0));

    // No lifecycle record: the car is tidied.
    let (ts, before, after) = skip_case(|ts| {
        ts.store
            .open()
            .expect("catalogue")
            .execute(
                "DELETE FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![ts.vid()],
            )
            .expect("delete the record");
    });
    assert_eq!(after.blob, None);
    assert_eq!(
        after.car.as_deref(),
        Some(serde_json::to_string(&tidied_car()).expect("json").as_str())
    );
    assert_eq!(marker_counts(&after, &ts.vid()), counts(1, 0, 0, 1, 0, 0));
    assert_eq!(after.car_mutations, before.car_mutations + 1);

    // A lifecycle record whose car row is gone: the blob is tidied and no car
    // row is created.
    let (ts, before, after) = skip_case(|ts| {
        ts.store
            .open()
            .expect("catalogue")
            .execute(
                "DELETE FROM materialised_cars WHERE vehicle_id = ?1",
                params![ts.vid()],
            )
            .expect("delete the car");
    });
    assert_eq!(after.car, None, "no car row is created");
    assert_record_columns_unchanged(&before, &after, "no car row");
    assert_eq!(
        after.blob.as_deref(),
        Some(tidied_blob(before.blob.as_deref().expect("blob")).as_slice())
    );
    assert_eq!(marker_counts(&after, &ts.vid()), counts(1, 0, 0, 0, 1, 0));
    assert_eq!(after.car_mutations, before.car_mutations);

    // A blob written directly at 65,526 bytes whose `car_metadata` trim is "a",
    // with a vehicle-data trim of 200 ASCII characters: the re-encoded blob is
    // over the record's bound, so both parts are left alone.
    let ts = Ts::new();
    ts.accept(
        vehicle_data_payload(
            false,
            None,
            p0_with(T0, |data| {
                data["vehicle_config"]["trim_badging"] = json!("t".repeat(200))
            }),
        ),
        T0,
    );
    set_before(&ts.store, &ts.vid(), &before_car(), None);
    {
        let connection = ts.store.open().expect("catalogue");
        let stored: Vec<u8> = connection
            .query_row(
                "SELECT open_session_json FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![ts.vid()],
                |row| row.get(0),
            )
            .expect("blob");
        let mut state = OpenSessionState::decode(&stored).expect("decode");
        state.car_metadata = Some(ProjectionCarPatch {
            trim_badging: Some("a".to_owned()),
            name: Some(String::new()),
            ..before_patch()
        });
        let base = state.encode().expect("encode").len();
        state.car_metadata.as_mut().expect("patch").name = Some("x".repeat(65_526 - base));
        let blob = state.encode().expect("encode the large blob");
        assert_eq!(blob.len(), 65_526);
        connection
            .execute(
                "UPDATE vehicle_lifecycle_state SET open_session_json = ?2 WHERE vehicle_id = ?1",
                params![ts.vid(), blob],
            )
            .expect("write the large blob");
    }
    remove_marker(&ts.store);
    let before = ts.snap();
    let store = ts.open_ok();
    let after = snap(&store, &ts.vid());
    assert_skipped_whole(&ts, &before, &after, "oversized re-encode");
}

// ---- C6 ST-I28 ----

fn table_exists(store: &HubStore, table: &str) -> bool {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get(0),
        )
        .expect("table lookup")
}

fn user_version(store: &HubStore) -> i32 {
    store
        .open()
        .expect("catalogue")
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version")
}

/// Rewind a store with `rewind` to the version `steps_below` steps under C6
/// Part I's predecessor, the version taken from the named constant (C6 R63 (b)
/// 4), never from a bare number.
fn rewind_store(store: &HubStore, rewind: &str, steps_below_predecessor: i32) {
    let predecessor = STORE_DATA_STEPS_SCHEMA_VERSION - 1 - steps_below_predecessor;
    store
        .open()
        .expect("catalogue")
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {rewind} PRAGMA user_version = {predecessor}; COMMIT;"
        ))
        .expect("rewind");
}

fn definition_count(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

#[test]
fn c6_st_i28_schema_constants() {
    // (a) A new store: user_version is SCHEMA_VERSION, `store_data_steps` has
    // R57's three columns and its only row is the tidy's marker.
    let ts = Ts::new();
    const { assert!(SCHEMA_VERSION >= STORE_DATA_STEPS_SCHEMA_VERSION) };
    assert_eq!(user_version(&ts.store), SCHEMA_VERSION);
    let connection = ts.store.open().expect("catalogue");
    let columns: Vec<(String, String, bool, i64)> = connection
        .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info('store_data_steps') ORDER BY cid")
        .expect("columns")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(
        columns,
        vec![
            ("step_key".to_owned(), "TEXT".to_owned(), true, 1),
            ("completed_at_ms".to_owned(), "INTEGER".to_owned(), true, 0),
            ("summary_json".to_owned(), "TEXT".to_owned(), true, 0),
        ]
    );
    let table_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'store_data_steps'",
            [],
            |row| row.get(0),
        )
        .expect("table sql");
    assert!(
        table_sql.contains("STRICT") && table_sql.contains("json_valid(summary_json)"),
        "{table_sql}"
    );
    // The tidy's own blob bound is the table's CHECK (C6 R56 skip): a change to
    // either one is caught here.
    let lifecycle_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'vehicle_lifecycle_state'",
            [],
            |row| row.get(0),
        )
        .expect("lifecycle table sql");
    assert!(
        lifecycle_sql.contains(&format!(
            "length(open_session_json) BETWEEN 2 AND {TIDY_LIFECYCLE_BLOB_MAX_BYTES}"
        )),
        "the tidy's blob bound is the table's CHECK: {lifecycle_sql}"
    );
    let keys: Vec<String> = connection
        .prepare("SELECT step_key FROM store_data_steps")
        .expect("keys")
        .query_map([], |row| row.get(0))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(
        keys,
        vec![VEHICLE_IDENTITY_TIDY_STEP_KEY.to_owned()],
        "the tidy's marker is the only row"
    );
    // A completed marker is never re-armed by a later non-JSON write.
    assert!(
        connection
            .execute(
                "INSERT INTO store_data_steps(step_key, completed_at_ms, summary_json) VALUES ('x', 1, 'not json')",
                [],
            )
            .is_err(),
        "summary_json must be JSON"
    );
    drop(connection);

    // (b) A store rewound through Part I's own rewind constant alone, with one
    // live vehicle as in C6 ST-I23: before the step the table does not exist,
    // the standalone preflight accepts the store, a read-write open migrates it
    // to SCHEMA_VERSION, creates the table and gives C6 ST-I23's result.
    let ts = Ts::fixture();
    let before = ts.snap();
    rewind_store(&ts.store, REWIND_V75_SQL, 0);
    assert!(
        !table_exists(&ts.store, "store_data_steps"),
        "the rewound store has no table"
    );
    assert_eq!(user_version(&ts.store), STORE_DATA_STEPS_SCHEMA_VERSION - 1);
    HubStore::preflight_standalone_schema_read_only(ts.directory.path())
        .expect("the preflight accepts the predecessor shape");
    let store = ts.open_ok();
    assert_eq!(user_version(&store), SCHEMA_VERSION);
    assert!(table_exists(&store, "store_data_steps"));
    assert_st_i23_result(&store, &ts.vid(), &before, &snap(&store, &ts.vid()));

    // (b, R69) The older rewinds are cumulative: each starts with Part I's undo,
    // so a store rewound through any of them has no `store_data_steps` and a
    // read-write open migrates it forward without error, recreates the table
    // and runs the tidy once. The distance is counted from Part I's version.
    for (name, rewind, steps) in [
        ("REWIND_V74_SQL", REWIND_V74_SQL, 1),
        ("REWIND_V73_SQL", REWIND_V73_SQL, 2),
        ("REWIND_V72_SQL", REWIND_V72_SQL, 3),
        ("REWIND_V71_SQL", REWIND_V71_SQL, 4),
        ("REWIND_V70_SQL", REWIND_V70_SQL, 5),
        ("REWIND_V69_SQL", REWIND_V69_SQL, 6),
    ] {
        assert!(
            rewind.starts_with(REWIND_V75_SQL),
            "{name} starts with the undo of Part I's step"
        );
        let ts = Ts::fixture();
        let before = ts.snap();
        rewind_store(&ts.store, rewind, steps);
        assert!(
            !table_exists(&ts.store, "store_data_steps"),
            "{name} removes the table"
        );
        let store = ts.open_ok();
        assert_eq!(
            user_version(&store),
            SCHEMA_VERSION,
            "{name}: migrated forward"
        );
        assert!(
            table_exists(&store, "store_data_steps"),
            "{name}: the table is recreated"
        );
        let after = snap(&store, &ts.vid());
        assert_st_i23_result(&store, &ts.vid(), &before, &after);
        // The tidy ran once: the marker is present and a further open changes nothing.
        let again = ts.open_ok();
        assert_eq!(
            snap(&again, &ts.vid()),
            after,
            "{name}: the next open is a no-op"
        );
    }
    // A store from a later version than this build is refused, as before.
    let ts = Ts::new();
    ts.store
        .open()
        .expect("catalogue")
        .execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
        .expect("future version");
    assert!(matches!(
        HubStore::preflight_standalone_schema_read_only(ts.directory.path()),
        Err(StoreError::UnsupportedSchema(_))
    ));

    // (c, d) One definition each (C6 R64, R70): no production file that C6
    // reads or writes the import literals through holds a copy beside the
    // definition. The needles are built from the constants, so this test holds
    // no copy either, and a rename is one edit.
    let kind_double = format!("\"{IMPORT_SOURCE_KIND}\"");
    let kind_single = format!("'{IMPORT_SOURCE_KIND}'");
    let record_type = IMPORT_POSITION_RECORD_TYPE;
    let source = |path: &str| {
        std::fs::read_to_string(format!("{}/src/{path}", env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|error| panic!("read {path}: {error}"))
    };
    // The one definition of the source kind, with no other copy beside it.
    let helpers = source("storage/db/projection_helpers.rs");
    assert_eq!(
        definition_count(&helpers, &kind_double),
        1,
        "the kind has one definition"
    );
    assert!(helpers.contains(&format!(
        "pub(crate) const IMPORT_SOURCE_KIND: &str = {kind_double};"
    )));
    // The one definition of the position record type, beside its producer.
    let producer = source("collection/teslamate_current.rs");
    assert_eq!(
        definition_count(&producer, record_type),
        1,
        "the record type has one definition"
    );
    assert!(producer.contains(&format!(
        "pub(crate) const IMPORT_POSITION_RECORD_TYPE: &str = \"{record_type}\";"
    )));
    // Every producer and reader that R70 names reads them through the constants.
    for path in [
        "import/teslamate/importer.rs",
        "storage/db/observation_persistence.rs",
        "storage/db/vehicle_identity_tidy.rs",
        "storage/db/migrations/b3.rs",
        "collection/current_state.rs",
        "collection/current_state/summary_fields.rs",
    ] {
        let text = source(path);
        for needle in [kind_double.as_str(), kind_single.as_str(), record_type] {
            assert_eq!(
                definition_count(&text, needle),
                0,
                "{path} holds a copy of an import literal"
            );
        }
    }
    assert!(
        source("import/teslamate/importer.rs")
            .matches("IMPORT_SOURCE_KIND")
            .count()
            >= 3,
        "the three importer sites use the constant"
    );
    // The stored values are unchanged: the schema's CHECK still admits the
    // constant's value, and a position row stores it.
    let ts = Ts::new();
    let schema: String = ts
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'current_observations'",
            [],
            |row| row.get(0),
        )
        .expect("schema");
    assert!(
        schema.contains(record_type),
        "the CHECK admits the producer's record type"
    );
    ts.store
        .record_teslamate_current(
            &ObservationInput {
                source_id: ts.source.source_id,
                vehicle_id: ts.vehicle.vehicle_id,
                observed_at_ms: T0,
                payload: json!({"record_type": record_type, "position_id": 7}),
            },
            T0,
        )
        .expect("record the position row");
    let stored: String = ts
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT record_type FROM current_observations WHERE vehicle_id = ?1",
            params![ts.vid()],
            |row| row.get(0),
        )
        .expect("stored record type");
    assert_eq!(stored, record_type);
    // The current-source lookup binds the kind constant: a source of that kind
    // is found, one of another kind is not.
    let import = ts
        .store
        .register_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, "i2-lookup"),
            1_000,
        )
        .expect("import source");
    let imported = ts
        .store
        .register_vehicle(
            &VehicleDescriptor::new(import.source_id, "lookup-key")
                .with_tesla_identity(Some(77), None),
            1_001,
        )
        .expect("imported vehicle");
    assert_eq!(
        ts.store
            .teslamate_current_source("i2-lookup", imported.vehicle_id, "lookup-key", None)
            .expect("lookup"),
        Some(import.source_id)
    );
    assert_eq!(
        ts.store
            .teslamate_current_source("account-test", ts.vehicle.vehicle_id, "vehicle-test", None)
            .expect("lookup of a live source"),
        None
    );
}
