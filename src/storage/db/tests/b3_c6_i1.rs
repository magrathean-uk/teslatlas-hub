// SPDX-License-Identifier: AGPL-3.0-only

//! C6-I1 store-backed tests (C6 section 8 Part I: C6 ST-I11 to ST-I14, ST-I15 to
//! ST-I22 and ST-I28 (e), with C6 R13, R13a, R14, R15, R15a, R16, R72). The pure
//! descriptor vectors (C6 ST-I1 to ST-I10) are in
//! `src/sync/hub_pack/vehicle_identity_tests.rs`.
//!
//! What runs here is the wiring of the four descriptor functions into the
//! retained writers: the import's `project_car`, the collector's
//! `compatibility_car`, `ProjectionCarPatch::into_car` at the lifecycle commit,
//! the import finalisation upsert, the legacy and current pack writers, and the
//! settings writer that rewrites `car_json`. Synthetic data only.
//!
//! The lifecycle producer (`parse_sample`, payload to patch) is covered in
//! `src/runtime/lifecycle/b3_c6_i1_tests.rs`; the tests below build the patch
//! value directly and commit it through the real lifecycle commit, which is
//! the writer wiring of this item.

use super::*;
use crate::hub_pack::{
    ProjectionCarPatch, vehicle_model_code, vehicle_model_name, vehicle_model_year,
};

const V3: &str = "5YJ3E1EA0KFTEST01";
const V3_REFRESHED: &str = "5YJ3E1EA0PFTEST01";
const VL: &str = "LRW3E7EK0MCTEST07";

fn imported_source(
    car_id: i64,
    model: Option<&str>,
    vin: Option<&str>,
    trim: Option<&str>,
    marketing: Option<&str>,
) -> crate::teslamate_projection::TeslaMateCar {
    crate::teslamate_projection::TeslaMateCar {
        id: car_id,
        eid: 70,
        vid: Some(71),
        vin: vin.map(str::to_owned),
        name: Some("Imported name".to_owned()),
        model: model.map(str::to_owned),
        trim_badging: trim.map(str::to_owned),
        marketing_name: marketing.map(str::to_owned),
        exterior_color: Some("Black".to_owned()),
        wheel_type: Some("Pinwheel18".to_owned()),
        spoiler_type: Some("None".to_owned()),
        efficiency_wh_per_km: Some(150.0),
        settings: Default::default(),
    }
}

fn project(
    model: Option<&str>,
    vin: Option<&str>,
    trim: Option<&str>,
    marketing: Option<&str>,
) -> ProjectionCar {
    crate::teslamate_projection::project_car(&imported_source(7, model, vin, trim, marketing), None)
        .expect("project the imported car")
}

fn some(value: &str) -> Option<String> {
    Some(value.to_owned())
}

/// One vehicle-data snapshot carrying the given `vehicle_config` group, as the
/// collector's tests build one (the groups are the snapshot's fields).
fn vehicle_data(config: serde_json::Value) -> crate::owner_api::VehicleData {
    crate::owner_api::VehicleData::for_test(
        9,
        serde_json::json!({
            "vehicle_config": config,
            "vehicle_state": {"car_version": "2026.20"}
        }),
    )
}

/// `compatibility_car` for vehicle-data `vehicle_config` fields.
fn live_car(vin: &str, config: Option<serde_json::Value>) -> ProjectionCar {
    let vehicle = crate::owner_api::Vehicle::for_test(9, vin, "online");
    let data = config.map(vehicle_data);
    crate::collector::compatibility_car(&vehicle, data.as_ref(), 9)
}

/// The served current-vehicle summary for `car`, as the HTTP current payload
/// serialises it: through the real `build_current_vehicle_summary` (C6-S1's
/// summary engine, which owns the served car view), once with no observations
/// and once with a newest import-source position sample. Returns both
/// serialised summaries.
fn served_summaries(car: &ProjectionCar) -> [serde_json::Value; 2] {
    let position = crate::db::ObservationRecord {
        observation_id: 1,
        source_id: Uuid::from_u128(1),
        vehicle_id: Uuid::from_u128(2),
        observed_at_ms: 1_000,
        received_at_ms: 1_000,
        payload_sha256: Sha256Digest::of_bytes(b"c6-i1-position"),
        payload: serde_json::json!({
            "record_type": crate::collection::teslamate_current::IMPORT_POSITION_RECORD_TYPE,
            "position_id": 7,
            "battery_level": 80
        }),
    };
    let none: Vec<crate::db::ObservationRecord> = Vec::new();
    let one = vec![position];
    let serve = |observations: &[crate::db::ObservationRecord]| {
        let summary = crate::current_state::build_current_vehicle_summary(
            Uuid::from_u128(2),
            observations,
            Some(car.clone()),
            None,
            None,
            2_000,
            600_000,
        );
        serde_json::to_value(&summary).expect("serialise the served summary")
    };
    [serve(&none), serve(&one)]
}

/// The `car` view the served current-vehicle summary builds from the stored
/// `car` (C6 R40, R48): the real `build_current_vehicle_summary`, no
/// observations. This is the served-view half carried from C6 ST-I11 and C6
/// ST-I12 to C6-S2, which owns the view.
fn served_car_view(car: &ProjectionCar) -> crate::current_state::CurrentCarView {
    crate::current_state::build_current_vehicle_summary(
        Uuid::from_u128(2),
        &[],
        Some(car.clone()),
        None,
        None,
        2_000,
        600_000,
    )
    .car
    .expect("a stored car gives a car view")
}

/// Whether any object inside `value`, at any depth, has the key `key`.
fn contains_key_deep(value: &serde_json::Value, key: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key(key) || map.values().any(|inner| contains_key_deep(inner, key))
        }
        serde_json::Value::Array(items) => items.iter().any(|inner| contains_key_deep(inner, key)),
        _ => false,
    }
}

fn stored_car(model: &str, trim: Option<&str>) -> ProjectionCar {
    ProjectionCar {
        id: 1,
        name: "Car".to_owned(),
        model: model.to_owned(),
        vin: None,
        source_eid: None,
        source_vid: None,
        trim_badging: trim.map(str::to_owned),
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        source_model_text: None,
        firmware_version: None,
        efficiency_wh_per_km: None,
        settings: Default::default(),
    }
}

fn car_json_bytes(store: &HubStore, vehicle_id: Uuid) -> Option<String> {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .expect("read car_json")
}

fn car_sync_mutations(store: &HubStore, vehicle_id: Uuid) -> i64 {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND entity = 'car'",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("count car mutations")
}

/// Commit one lifecycle patch through the real lifecycle commit, which applies
/// `ProjectionCarPatch::into_car` to the stored car (C6 R15a).
fn commit_patch(
    store: &HubStore,
    vehicle_id: Uuid,
    car_id: i64,
    patch: ProjectionCarPatch,
    n: i64,
) {
    let mut state = crate::lifecycle::OpenSessionState::new();
    state.last_observation_id = n;
    state.car_metadata = Some(patch);
    let encoded = state.encode().expect("encode the session");
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id,
            car_id,
            open_session_json: &encoded,
            last_observation_id: n,
            quarantined: false,
            updated_at_ms: 2_000 + n,
            delta: &crate::lifecycle::LifecycleDelta::default(),
        })
        .expect("commit the lifecycle patch");
}

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
            Sha256Digest::of_bytes(b"c6-i1-materialised-base"),
            &[],
            binding,
            &state,
            car,
            &[],
            false,
        )
        .expect("publish the imported base");
}

/// A repeat import of the same car (nothing changed at the source).
fn import_again(
    store: &HubStore,
    vehicle: &VehicleRecord,
    binding: &ProjectionBinding,
    car: &ProjectionCar,
) {
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            3_000,
        )
        .expect("staging the repeat import");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("staging the repeat session");
    store
        .promote_import_generation_with_materialisation(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            3_000,
            car,
            &[],
        )
        .expect("repeat import");
}

/// One imported store: the registered vehicle with `car` imported as its car.
fn imported_store(
    build: impl FnOnce(i64) -> ProjectionCar,
) -> (
    tempfile::TempDir,
    HubStore,
    VehicleRecord,
    ProjectionBinding,
    ProjectionCar,
) {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = build(binding.selected_car_id);
    import_finalise(&store, &vehicle, &binding, &manifest, &car);
    (directory, store, vehicle, binding, car)
}

/// The `model` and `marketing_name` columns of the one `cars` row of a pack
/// written by the legacy (`legacy`) or the current writer.
fn pack_car_row(car: &ProjectionCar, legacy: bool) -> (String, Option<String>) {
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
    let writer = ProjectionPackWriter::new(directory.path());
    let built = if legacy {
        writer.write_full_snapshot(&request)
    } else {
        writer.write_full_snapshot_with_states_and_updates(&request, &[], &[])
    }
    .expect("write the pack");
    let bytes = zstd::stream::decode_all(std::fs::File::open(&built.path).expect("open the pack"))
        .expect("decode the pack");
    let inspect = directory.path().join("inspect.sqlite");
    std::fs::write(&inspect, bytes).expect("write the inspection copy");
    let connection = rusqlite::Connection::open(inspect).expect("open the inspection copy");
    // The legacy pack's `cars` table has no marketing-name column.
    let has_marketing_name: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('cars') WHERE name = 'marketing_name'",
            [],
            |row| row.get(0),
        )
        .expect("inspect the cars columns");
    let marketing = if has_marketing_name == 1 {
        "marketing_name"
    } else {
        "NULL"
    };
    connection
        .query_row(&format!("SELECT model, {marketing} FROM cars"), [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("the one pack car")
}

#[test]
fn c6_st_i11_to_i14_import_live() {
    // C6 ST-I11: an imported car stores the Hub code and keeps every source text
    // as the source stated it.
    let imported = project(
        Some("Model 3"),
        Some(V3),
        Some("LR"),
        Some("Long Range AWD"),
    );
    assert_eq!(imported.model, "3");
    assert_eq!(imported.source_model_text, some("Model 3"));
    assert_eq!(imported.trim_badging, some("LR"));
    assert_eq!(imported.marketing_name, some("Long Range AWD"));
    let stored = serde_json::to_value(&imported).expect("serialise the imported car");
    assert_eq!(stored["model"], "3");
    assert_eq!(stored["source_model_text"], "Model 3");
    // C6 R13a: the stored imported car keeps the source text, but the served
    // current-vehicle summary built from it never carries the key, at any depth
    // and on either input route of the constructor. The input car is untouched.
    assert!(
        stored.get("source_model_text").is_some(),
        "the stored imported car keeps its source text"
    );
    for served in served_summaries(&imported) {
        assert!(
            served.get("vehicle_id").is_some(),
            "the served summary is the real summary"
        );
        assert!(
            !contains_key_deep(&served, "source_model_text"),
            "the served summary leaves source_model_text out (C6 R13a)"
        );
    }
    assert_eq!(imported.source_model_text, some("Model 3"));
    // C6 ST-I11, served-view half (carried to C6-S2): the `car` view names the
    // code, the model and the model year of the VIN, the imported variant name,
    // and the stored model as the alias `car.model`.
    let view = served_car_view(&imported);
    assert_eq!(view.model_code.as_deref(), Some("3"));
    assert_eq!(view.model_name, Some("Model 3"));
    assert_eq!(view.model_year, Some(2019));
    assert_eq!(view.variant_name.as_deref(), Some("Long Range AWD"));
    assert_eq!(view.stored_model_text, "3");
    for served in served_summaries(&imported) {
        assert_eq!(served["car"]["model"], "3");
        assert_eq!(served["car"]["marketing_name"], "Long Range AWD");
        assert_eq!(served["car"]["trim_badging"], "LR");
    }

    // C6 ST-I12: gaps. A missing marketing name is not derived; the code falls
    // back to the VIN, then to the empty string; an unrecognised text passes
    // through; the source text is kept whenever the source stated one.
    let gap = project(Some("Model 3"), Some(V3), Some("LR"), None);
    assert_eq!(gap.marketing_name, None);
    // Served-view half of C6 ST-I12 (carried to C6-S2): a missing marketing
    // name is a null `variant_name`.
    assert_eq!(served_car_view(&gap).variant_name, None);
    for served in served_summaries(&gap) {
        assert!(served["car"]["marketing_name"].is_null());
    }
    let no_model = project(None, Some(V3), None, None);
    assert_eq!(
        (no_model.model.as_str(), &no_model.source_model_text),
        ("3", &None)
    );
    let nothing = project(None, None, None, None);
    assert_eq!(
        (nothing.model.as_str(), &nothing.source_model_text),
        ("", &None)
    );
    let view = served_car_view(&nothing);
    assert_eq!((view.model_code, view.model_name), (None, None));
    assert_eq!(view.stored_model_text, "");
    for served in served_summaries(&nothing) {
        assert_eq!(served["car"]["model"], "");
    }
    let empty_model = project(Some(""), Some(V3), None, None);
    assert_eq!(empty_model.model, "3");
    assert_eq!(empty_model.source_model_text, some(""));
    let view = served_car_view(&empty_model);
    assert_eq!(view.model_code.as_deref(), Some("3"));
    assert_eq!(view.model_year, Some(2019));
    let unknown = project(Some("lychee"), None, None, None);
    assert_eq!(unknown.model, "lychee");
    assert_eq!(unknown.source_model_text, some("lychee"));
    let view = served_car_view(&unknown);
    assert_eq!(view.model_code.as_deref(), Some("lychee"));
    assert_eq!(view.model_name, None);
    assert_eq!(view.model_year, None);
    let empty_trim = project(Some("Model 3"), Some(V3), Some(""), None);
    assert_eq!(empty_trim.trim_badging, some(""));
    // Imported bytes are retained verbatim: case, whitespace, empty strings,
    // the source's own model spelling, and the other descriptor texts.
    let verbatim = project(Some("  Model S  "), Some(V3), Some("  lr "), Some(" AWD "));
    assert_eq!(verbatim.source_model_text, some("  Model S  "));
    assert_eq!(verbatim.trim_badging, some("  lr "));
    assert_eq!(verbatim.marketing_name, some(" AWD "));
    assert_eq!(verbatim.exterior_color, some("Black"));
    assert_eq!(verbatim.wheel_type, some("Pinwheel18"));
    // A filed VIN decides over a contradicting source text (C6 R6), and the
    // source text still survives beside it.
    assert_eq!(verbatim.model, "3");

    // C6 ST-I13 (collector half): a live car carries the code, the raw trimmed
    // configuration texts, no marketing name and no source text.
    let live = live_car(
        V3,
        Some(serde_json::json!({
            "car_type": "model3", "trim_badging": " 74d ", "exterior_color": "MidnightSilver",
            "wheel_type": "Pinwheel18", "spoiler_type": "None"
        })),
    );
    assert_eq!(live.model, "3");
    assert_eq!(live.source_model_text, None);
    assert_eq!(live.marketing_name, None);
    assert_eq!(live.trim_badging, some("74d"));
    assert_eq!(live.exterior_color, some("MidnightSilver"));
    assert_eq!(live.wheel_type, some("Pinwheel18"));
    assert_eq!(live.spoiler_type, some("None"));
    let live_json = serde_json::to_value(&live).expect("serialise the live car");
    assert!(
        live_json.get("source_model_text").is_none(),
        "a live car has no source_model_text key (C6 R13a)"
    );
    // No derived variant, whatever the badge: the badge is the raw text.
    let badge = live_car(
        V3,
        Some(serde_json::json!({"car_type": "model3", "trim_badging": " p100d "})),
    );
    assert_eq!(
        (badge.trim_badging, badge.marketing_name),
        (some("p100d"), None)
    );
    // A configuration text that is empty, blank or not a string is no text.
    let blank = live_car(
        V3,
        Some(serde_json::json!({
            "car_type": "model3", "trim_badging": "  ", "exterior_color": 7, "wheel_type": ""
        })),
    );
    assert_eq!(
        (blank.trim_badging, blank.exterior_color, blank.wheel_type),
        (None, None, None)
    );
    // Without a snapshot the VIN still decides; without a VIN or a model the
    // code is the empty string (the replaced code's invented names are gone).
    assert_eq!(live_car(V3, None).model, "3");
    assert_eq!(live_car("unusable vin", None).model, "");
    assert_eq!(
        live_car(
            "unusable vin",
            Some(serde_json::json!({"car_type": "lychee"}))
        )
        .model,
        "lychee"
    );
    // A not-filed WMI: the recognised text wins, the VIN is the fallback (C6 R6).
    assert_eq!(
        live_car(VL, Some(serde_json::json!({"car_type": "modely"}))).model,
        "Y"
    );
    assert_eq!(
        live_car(VL, Some(serde_json::json!({"car_type": "lychee"}))).model,
        "3"
    );

    // C6 ST-I14: the stored car's names are kept; only `None` is filled; a patch
    // marketing name never reaches the stored car; colour follows the newest.
    let existing = ProjectionCar {
        vin: some(V3),
        source_model_text: some("Model 3"),
        marketing_name: some("Long Range AWD"),
        exterior_color: some("Black"),
        ..stored_car("3", Some("LR"))
    };
    let patch = ProjectionCarPatch {
        model: some("Y"),
        trim_badging: some("74"),
        marketing_name: some("Derived Name"),
        exterior_color: some("White"),
        ..Default::default()
    };
    let filled = patch.into_car(1, Some(&existing), None, None);
    assert_eq!(filled.model, "3");
    assert_eq!(filled.source_model_text, some("Model 3"));
    assert_eq!(filled.trim_badging, some("LR"));
    assert_eq!(filled.marketing_name, some("Long Range AWD"));
    assert_eq!(filled.exterior_color, some("White"));
    let fresh = ProjectionCarPatch {
        model: some("model3"),
        trim_badging: some("74"),
        marketing_name: some("Derived Name"),
        ..Default::default()
    }
    .into_car(1, Some(&stored_car("", None)), None, None);
    assert_eq!(fresh.model, "3");
    assert_eq!(fresh.trim_badging, some("74"));
    assert_eq!(fresh.marketing_name, None);
    // A stored empty model is filled from the patch VIN, then the stored VIN,
    // then the fallback VIN; with nothing at all it stays the empty string.
    let by_vin = ProjectionCarPatch {
        vin: some(V3),
        ..Default::default()
    }
    .into_car(1, Some(&stored_car("", None)), None, None);
    assert_eq!(by_vin.model, "3");
    let by_fallback = ProjectionCarPatch::default().into_car(1, None, None, some(V3));
    assert_eq!(by_fallback.model, "3");
    let nothing_known = ProjectionCarPatch::default().into_car(1, None, None, None);
    assert_eq!(nothing_known.model, "");
    // The live patch texts are trimmed; a blank value changes nothing.
    let trimmed = ProjectionCarPatch {
        trim_badging: some("  74d "),
        wheel_type: some("   "),
        ..Default::default()
    }
    .into_car(
        1,
        Some(&ProjectionCar {
            wheel_type: some("Apollo"),
            ..stored_car("3", None)
        }),
        None,
        None,
    );
    assert_eq!(trimmed.trim_badging, some("74d"));
    assert_eq!(trimmed.wheel_type, some("Apollo"));
}

#[test]
fn c6_st_i15_to_i22_identity_stability() {
    // C6 ST-I15 (patch half): the same patch twice gives byte-equal car_json.
    let patch = ProjectionCarPatch {
        model: some("Model 3"),
        trim_badging: some("74d"),
        marketing_name: some("Derived Name"),
        exterior_color: some("White"),
        ..Default::default()
    };
    let first = patch.into_car(1, None, some("Car"), some(V3));
    let second = patch.into_car(1, Some(&first), some("Car"), some(V3));
    assert_eq!(
        serde_json::to_string(&first).expect("first"),
        serde_json::to_string(&second).expect("second")
    );

    // C6 ST-I15 (import half): importing the same source car twice leaves the
    // stored bytes alone: the retained `IS NOT` guard writes no row and the car
    // journal gains nothing.
    let (_directory, store, vehicle, binding, car) = imported_store(|car_id| {
        crate::teslamate_projection::project_car(
            &imported_source(
                car_id,
                Some("Model 3"),
                Some(V3),
                Some("LR"),
                Some("Long Range AWD"),
            ),
            None,
        )
        .expect("project")
    });
    let after_first = car_json_bytes(&store, vehicle.vehicle_id).expect("imported car");
    assert!(after_first.contains("\"source_model_text\":\"Model 3\""));
    let mutations_before = car_sync_mutations(&store, vehicle.vehicle_id);
    store
        .open()
        .expect("catalogue")
        .execute_batch(
            "CREATE TABLE c6_i1_car_writes(n INTEGER NOT NULL);
             CREATE TRIGGER c6_i1_car_insert AFTER INSERT ON materialised_cars
                 BEGIN INSERT INTO c6_i1_car_writes VALUES (1); END;
             CREATE TRIGGER c6_i1_car_update AFTER UPDATE ON materialised_cars
                 BEGIN INSERT INTO c6_i1_car_writes VALUES (1); END;",
        )
        .expect("watch the car table");
    import_again(&store, &vehicle, &binding, &car);
    assert_eq!(
        car_json_bytes(&store, vehicle.vehicle_id),
        Some(after_first)
    );
    let writes: i64 = store
        .open()
        .expect("catalogue")
        .query_row("SELECT COUNT(*) FROM c6_i1_car_writes", [], |row| {
            row.get(0)
        })
        .expect("count the writes");
    assert_eq!(writes, 0, "an unchanged import writes no car row");
    assert_eq!(
        car_sync_mutations(&store, vehicle.vehicle_id),
        mutations_before
    );

    // C6 ST-I16: both pack writers copy the stored model verbatim, with no
    // re-derivation, including text a replaced build stored and the empty
    // string.
    for (stored, vin, expected) in [
        ("3", Some(V3), "3"),
        ("lychee", None, "lychee"),
        ("", None, ""),
        ("Model 3", None, "Model 3"),
    ] {
        let car = ProjectionCar {
            id: 10,
            vin: vin.map(str::to_owned),
            ..stored_car(stored, None)
        };
        for legacy in [true, false] {
            assert_eq!(pack_car_row(&car, legacy).0, expected, "stored {stored:?}");
        }
    }

    // C6 ST-I17: nothing derived is stored. The imported and the live car hold
    // no name or year, and the live car's pack row holds no marketing name.
    let live = live_car(
        V3,
        Some(serde_json::json!({"car_type": "model3", "trim_badging": "74d"})),
    );
    for text in [
        serde_json::to_string(&live).expect("live json"),
        car_json_bytes(&store, vehicle.vehicle_id).expect("imported json"),
    ] {
        assert!(
            !text.contains("model_name") && !text.contains("model_year"),
            "{text}"
        );
    }
    for legacy in [true, false] {
        assert_eq!(pack_car_row(&live, legacy).1, None);
    }

    // C6 ST-I18: a refreshed model keeps its code and name; the badges are the
    // raw texts. (The year differs: the pure descriptor test pins the values.)
    let early = live_car(
        V3,
        Some(serde_json::json!({"car_type": "model3", "trim_badging": "74d"})),
    );
    let refreshed = live_car(
        V3_REFRESHED,
        Some(serde_json::json!({"car_type": "model3", "trim_badging": "74"})),
    );
    assert_eq!(
        (early.model.as_str(), early.trim_badging),
        ("3", some("74d"))
    );
    assert_eq!(
        (refreshed.model.as_str(), refreshed.trim_badging),
        ("3", some("74"))
    );
    assert_eq!(vehicle_model_name("3"), Some("Model 3"));
    assert_eq!(vehicle_model_year(Some(V3)), Some(2019));
    assert_eq!(vehicle_model_year(Some(V3_REFRESHED)), Some(2023));

    // C6 ST-I19: a live car, then its import, then a lifecycle patch, then the
    // live writer again.
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let live = {
        let live_vehicle = crate::owner_api::Vehicle::for_test(9, V3, "online");
        let data = vehicle_data(serde_json::json!({
            "car_type": "model3", "trim_badging": "74d", "exterior_color": "White"
        }));
        crate::collector::compatibility_car(&live_vehicle, Some(&data), binding.selected_car_id)
    };
    store
        .persist_materialised_car_if_absent(vehicle.vehicle_id, &live)
        .expect("live car");
    let imported = ProjectionCar {
        exterior_color: some("Black"),
        ..crate::teslamate_projection::project_car(
            &imported_source(
                binding.selected_car_id,
                Some("Model 3"),
                Some(V3),
                Some("LR"),
                Some("Long Range AWD"),
            ),
            None,
        )
        .expect("project")
    };
    import_finalise(&store, &vehicle, &binding, &manifest, &imported);
    let read = |store: &HubStore| {
        store
            .materialised_car_for_vehicle(vehicle.vehicle_id)
            .expect("read the car")
            .expect("a car")
    };
    let after_import = read(&store);
    assert_eq!(after_import.model, "3");
    assert_eq!(after_import.source_model_text, some("Model 3"));
    assert_eq!(after_import.trim_badging, some("LR"));
    assert_eq!(after_import.marketing_name, some("Long Range AWD"));
    assert_eq!(after_import.exterior_color, some("Black"));
    commit_patch(
        &store,
        vehicle.vehicle_id,
        binding.selected_car_id,
        ProjectionCarPatch {
            trim_badging: some("74d"),
            exterior_color: some("White"),
            marketing_name: some("X"),
            ..Default::default()
        },
        1,
    );
    let after_patch = read(&store);
    assert_eq!(after_patch.trim_badging, some("LR"));
    assert_eq!(after_patch.marketing_name, some("Long Range AWD"));
    assert_eq!(after_patch.exterior_color, some("White"));
    assert_eq!(after_patch.source_model_text, some("Model 3"));
    let bytes = car_json_bytes(&store, vehicle.vehicle_id);
    store
        .persist_materialised_car_if_absent(vehicle.vehicle_id, &live)
        .expect("live car again");
    assert_eq!(car_json_bytes(&store, vehicle.vehicle_id), bytes);

    // C6 ST-I20: an imported empty trim stays; an imported missing trim is
    // filled by the first live statement.
    for (imported_trim, expected) in [(Some(""), some("")), (None, some("74d"))] {
        let (_directory, store, vehicle, binding, _car) = imported_store(|car_id| {
            crate::teslamate_projection::project_car(
                &imported_source(car_id, Some("Model 3"), Some(V3), imported_trim, None),
                None,
            )
            .expect("project")
        });
        commit_patch(
            &store,
            vehicle.vehicle_id,
            binding.selected_car_id,
            ProjectionCarPatch {
                trim_badging: some("74d"),
                ..Default::default()
            },
            1,
        );
        let car = store
            .materialised_car_for_vehicle(vehicle.vehicle_id)
            .expect("read")
            .expect("car");
        assert_eq!(
            car.trim_badging, expected,
            "imported trim {imported_trim:?}"
        );
    }

    // C6 ST-I21: a patch a replaced build wrote into a lifecycle blob lands as a
    // Hub code, with the raw trim and no marketing name, on a store with no
    // car, through the stored vehicle's VIN.
    let old: ProjectionCarPatch = serde_json::from_str(
        r#"{"model":"Model 3","trim_badging":"74d","marketing_name":"Derived Name"}"#,
    )
    .expect("decode the old patch");
    let direct = old.into_car(1, None, None, some(V3));
    assert_eq!(
        (
            &direct.model[..],
            &direct.trim_badging,
            &direct.marketing_name,
            &direct.source_model_text
        ),
        ("3", &some("74d"), &None, &None)
    );
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (_source, vehicle) = test_registered_vehicle(&store);
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE vehicles SET vin = ?1 WHERE vehicle_id = ?2",
            params![V3, vehicle.vehicle_id.to_string()],
        )
        .expect("give the vehicle its VIN");
    commit_patch(&store, vehicle.vehicle_id, 1, old, 1);
    let car = store
        .materialised_car_for_vehicle(vehicle.vehicle_id)
        .expect("read")
        .expect("car");
    assert_eq!(
        (
            &car.model[..],
            &car.trim_badging,
            &car.marketing_name,
            &car.source_model_text
        ),
        ("3", &some("74d"), &None, &None)
    );

    // C6 ST-I22: a stored code read back through the writer is unchanged.
    for code in ["S", "X", "3", "Y", "R", "C"] {
        let again = ProjectionCarPatch {
            model: some(code),
            ..Default::default()
        }
        .into_car(1, Some(&stored_car("", None)), None, None);
        assert_eq!(again.model, code);
        assert_eq!(
            vehicle_model_code(Some(V3), Some(code)).as_deref(),
            Some("3")
        );
    }
}

#[test]
fn c6_st_i28e_settings_preserve_identity() {
    // The B2 settings writer rewrites `car_json` from the decoded car (C6 R72,
    // C6 R15a). Its round trip must keep every identity name and the source text
    // byte for byte, and an absent source text must stay absent (C6 R13a).
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (source, first) = test_registered_vehicle(&store);
    let second = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-test-2"),
            1_002,
        )
        .expect("second vehicle");
    let unresolved = ProjectionCarSettings {
        suspend_min: 31,
        suspend_min_resolved: false,
        ..ProjectionCarSettings::new_live()
    };
    let cars = [
        (
            first.vehicle_id,
            ProjectionCar {
                id: 1,
                vin: some(V3),
                marketing_name: None,
                ..stored_car("3", Some("74d"))
            },
        ),
        (
            second.vehicle_id,
            ProjectionCar {
                id: 2,
                vin: some(V3),
                source_model_text: some("Model 3 Long Range"),
                ..stored_car("3", Some("74d"))
            },
        ),
    ];
    let identity = |json: &str| {
        let value: serde_json::Value = serde_json::from_str(json).expect("car_json");
        [
            "model",
            "trim_badging",
            "marketing_name",
            "source_model_text",
        ]
        .map(|key| value.get(key).map(|field| field.to_string()))
    };
    let mut before = Vec::new();
    for (vehicle_id, car) in &cars {
        store
            .persist_materialised_car_if_absent(*vehicle_id, car)
            .expect("stored car");
        // The identity bytes as first stored, before any settings writer runs.
        let stored = identity(&car_json_bytes(&store, *vehicle_id).expect("stored car_json"));
        store
            .upsert_car_settings(*vehicle_id, car.id, &unresolved)
            .expect("unresolved settings");
        let json = car_json_bytes(&store, *vehicle_id).expect("car_json");
        assert_eq!(
            identity(&json),
            stored,
            "the settings upsert keeps identity"
        );
        let settings = store.load_car_settings(*vehicle_id).expect("settings row");
        assert_eq!(
            (settings.suspend_min, settings.suspend_min_resolved),
            (31, false)
        );
        let value: serde_json::Value = serde_json::from_str(&json).expect("car_json");
        assert_eq!(value["settings"]["suspend_min"], 31);
        before.push(identity(&json));
    }
    for (index, (vehicle_id, _car)) in cars.iter().enumerate() {
        assert!(
            store
                .resolve_car_suspend_min(*vehicle_id)
                .expect("resolve the suspend length"),
            "the writer changes the unresolved settings"
        );
        let settings = store.load_car_settings(*vehicle_id).expect("settings row");
        assert!(settings.suspend_min_resolved);
        assert_eq!(
            settings.suspend_min,
            ProjectionCarSettings::new_live().suspend_min
        );
        let json = car_json_bytes(&store, *vehicle_id).expect("car_json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("car_json");
        assert_eq!(value["settings"]["suspend_min"], 30);
        // Only after the settings assertions: the identity bytes are equal.
        assert_eq!(
            identity(&json),
            before[index],
            "identity bytes, car {index}"
        );
    }
    let first_json = car_json_bytes(&store, first.vehicle_id).expect("first car_json");
    assert!(
        !first_json.contains("source_model_text"),
        "an absent source text stays absent"
    );
    let second_json = car_json_bytes(&store, second.vehicle_id).expect("second car_json");
    assert!(second_json.contains("\"source_model_text\":\"Model 3 Long Range\""));
}

/// Record `car` as a live car mutation, claim it and write its typed delta pack
/// the way the collector's publication path does (a copy of the shape of
/// `claimed_collector_delta`, with the car given by the caller).
fn claimed_delta_for_car(
    store: &HubStore,
    vehicle_id: Uuid,
    binding: &ProjectionBinding,
    car: &ProjectionCar,
) -> (SyncMutationClaim, LineageDelta) {
    let (base_snapshot_id, head_sequence, parent_digest) = store
        .v2_head(vehicle_id)
        .expect("head lookup")
        .expect("V2 head");
    let from_sequence = u64::try_from(head_sequence).expect("non-negative sequence");
    store
        .persist_materialised_car_if_absent(vehicle_id, car)
        .expect("record the live car mutation");
    let claim = store
        .claim_sync_mutations(vehicle_id, 2_000, 100)
        .expect("claim the mutation")
        .expect("one car mutation pending");
    let to_sequence = from_sequence
        .checked_add(u64::try_from(claim.mutations.len()).expect("claim length"))
        .expect("sequence range");
    let payload = store
        .projection_delta_for_mutations(
            &claim,
            binding.clone(),
            SequenceRange {
                from_exclusive: from_sequence,
                to_inclusive: to_sequence,
            },
            parent_digest,
        )
        .expect("project the claimed mutation");
    assert_eq!(payload.cars.len(), 1);
    assert_eq!(
        payload.cars[0].model, car.model,
        "the delta copies the model"
    );
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: base_snapshot_id,
            ordinal: store
                .next_v2_pack_ordinal(base_snapshot_id)
                .expect("delta ordinal"),
            delta: &payload,
        })
        .expect("an empty stored model is valid in a delta pack (C6 R13, R16)");
    let delta = LineageDelta {
        from_sequence,
        to_sequence,
        parent_chain_digest: parent_digest,
        chain_digest: canonical_delta_chain_digest(parent_digest, pack.metadata.sha256),
        pack_digest: pack.metadata.sha256,
        pack: pack.metadata,
    };
    (claim, delta)
}

#[test]
fn c6_st_i16_an_empty_model_is_valid_in_the_delta_path_and_the_lineage_verifier() {
    // C6 R13, R14, R16: a live car with no VIN and no car type has the empty
    // string as its model, and that car reaches the delta pack writer and the
    // typed-delta lineage verifier unchanged. Both are the retained writers
    // this item relaxed, so a revert of either fails this test.
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, _base) = imported_v2_base(&store);
    let live = ProjectionCar {
        id: binding.selected_car_id,
        ..live_car("unusable vin", None)
    };
    assert_eq!(live.model, "");
    let (claim, delta) = claimed_delta_for_car(&store, vehicle.vehicle_id, &binding, &live);
    store
        .commit_v2_delta_claim(
            &claim,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
        )
        .expect("the verifier accepts an empty stored model");
    let published = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("serving verification")
        .expect("published lineage");
    assert_eq!(published.deltas, vec![delta]);

    // A cars row with no model at all (SQL NULL) cannot be written into a typed
    // delta: its schema declares `cars.model TEXT NOT NULL` and the verifier
    // checks the schema text, so the `model IS NULL` clause is a second guard
    // that no pack can reach; the test does not pretend to exercise it.
}
