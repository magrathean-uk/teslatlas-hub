// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T20: the two findings of the Codex re-check of M03-T18. Promotion
//! decides the activity phase from the final open session, and a handback
//! closes the sessions still open at their last evidence through the
//! lifecycle, once. Fixtures come from the M03-T18 tests.

use super::seam_repair_store::*;
use super::*;
use crate::db::hub_ids::HUB_ID_BASE;
use crate::db::{WindowCoverage, record_coverage_handback, record_coverage_open, seal_restored_open_windows};

/// Publishes one import successor whose staged open session is `session`,
/// with no completed drive, through the real publication path.
fn seam_repair_handback_publish_session(
    store: &HubStore,
    vehicle: &VehicleRecord,
    binding: &ProjectionBinding,
    car: &ProjectionCar,
    session: &SourceDbOpenSession,
) -> Result<(), StoreError> {
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            3_000,
        )
        .expect("successor generation");
    store
        .stage_import_generation_session(run_id, session)
        .expect("successor session");
    let prior = store
        .source_db_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("prior state");
    let state = create_direct_import_projection_state(store, run_id, 64);
    let mut capture = crate::import_projection_state::SourceDbProjectionStateCapture::for_successor(
        state,
        Box::new(prior),
    );
    capture.record_car(car).expect("capture car");
    capture.seal().expect("seal successor state");
    let state = capture.into_state();
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("lineage")
        .expect("published lineage");
    let delta = imported_typed_delta(store, binding, &lineage);
    store.finalize_import_generation_delta_successors_with_projection_state_and_materialisation(
        run_id,
        binding.account_id,
        vehicle.vehicle_id,
        binding.selected_car_id,
        3_000,
        std::slice::from_ref(&delta),
        &import_delta_test_cursor_key(),
        &import_delta_test_cursor(binding, delta.to_sequence),
        Sha256Digest::of_bytes(format!("seam-handback-{run_id}").as_bytes()),
        &[],
        &state,
        car,
        &[],
    )
}

/// The lifecycle record of the vehicle and its decoded open session.
fn seam_repair_handback_lifecycle(
    store: &HubStore,
    vehicle: &VehicleRecord,
) -> (LifecycleStateRecord, crate::lifecycle::OpenSessionState) {
    let record = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .expect("lifecycle state")
        .expect("a lifecycle record");
    let state = crate::lifecycle::OpenSessionState::decode(&record.open_session_json).expect("decode");
    (record, state)
}

#[test]
fn seam_repair_handback_promotion_takes_the_phase_from_the_final_state() {
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    // A Hub charge still open after the handback (a store handed back before
    // this repair, or a late local write DEC-097 allows), with 25 samples.
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + 60 * SEAM_MINUTE));
    let samples = seam_repair_hub_samples(OPEN_CHARGE, SEAM_T + 30 * SEAM_MINUTE, 25);
    seam_repair_collect_open_charge(
        &store,
        &vehicle,
        car_id,
        OPEN_CHARGE,
        SEAM_T + 30 * SEAM_MINUTE,
        &samples,
    );
    // The import brings a newer open drive.
    let drive_start = SEAM_T + 70 * SEAM_MINUTE;
    let session = SourceDbOpenSession {
        car_id,
        drive: Some(
            serde_json::from_value(serde_json::json!({
                "id": 90, "car_id": car_id, "start_date_ms": drive_start
            }))
            .expect("source open drive"),
        ),
        drive_positions: (0..3)
            .map(|minute| {
                serde_json::from_value(serde_json::json!({
                    "id": 900 + minute, "car_id": car_id, "drive_id": 90,
                    "date_ms": drive_start + minute * SEAM_MINUTE,
                    "latitude": 47.5, "longitude": 19.0
                }))
                .expect("source open position")
            })
            .collect(),
        ..Default::default()
    };
    for round in ["publication", "replay"] {
        seam_repair_handback_publish_session(&store, &vehicle, &binding, &car, &session)
            .expect(round);
        let (record, state) = seam_repair_handback_lifecycle(&store, &vehicle);
        assert_eq!(
            state.open_drive.as_ref().map(|open| open.id),
            Some(90),
            "{round}: the source's newer open drive is adopted"
        );
        assert_eq!(
            state.open_charge.as_ref().map(|open| open.id),
            Some(OPEN_CHARGE),
            "{round}: the Hub's open charge is kept"
        );
        assert_eq!(
            state.phase,
            crate::lifecycle::VehiclePhase::Driving,
            "{round}: the persisted phase comes from the final open session"
        );
        let served = crate::current_state::build_current_vehicle_summary(
            vehicle.vehicle_id,
            &[],
            None,
            Some(&record),
            None,
            SEAM_T + 100 * SEAM_MINUTE,
            0,
        );
        assert_eq!(
            served.activity_state,
            Some(crate::current_state::ActivityState::Driving),
            "{round}: the served activity"
        );
        let open_rows = seam_repair_open_rows(&store, &vehicle);
        assert_eq!(
            open_rows
                .iter()
                .filter(|(_, domain, _)| domain == "charge_sample")
                .map(|(id, _, parent)| (*id, *parent))
                .collect::<Vec<_>>(),
            samples
                .iter()
                .map(|sample| (sample.id, Some(OPEN_CHARGE)))
                .collect::<Vec<_>>(),
            "{round}: every charge sample survives once"
        );
        assert!(seam_repair_stored_samples(&store, &vehicle).is_empty(), "{round}");
        assert_eq!(
            open_rows
                .iter()
                .filter(|(_, domain, _)| domain == "drive" || domain == "position")
                .count(),
            4,
            "{round}: the adopted drive and its positions are open rows once"
        );
    }
}

/// The kind of session open when coverage is handed back.
#[derive(Debug, Clone, Copy)]
enum SeamRepairHandbackSession {
    Drive,
    Charge,
}

/// How coverage is handed back.
#[derive(Debug, Clone, Copy)]
enum SeamRepairHandbackPath {
    /// The window's handback is recorded (expiry, operator close, mirror).
    Window,
    /// A store restored from a backup taken inside the window is sealed.
    Restore,
}

/// The Hub-range sync mutations of the vehicle: entity, id and operation.
fn seam_repair_handback_hub_journal(
    store: &HubStore,
    vehicle: &VehicleRecord,
) -> Vec<(String, i64, String)> {
    let mut journal: Vec<(String, i64, String)> = seam_repair_journal(store, vehicle)
        .into_iter()
        .filter(|(_, _, id, _, _)| *id >= HUB_ID_BASE)
        .map(|(_, entity, id, operation, _)| (entity, id, operation))
        .collect();
    journal.sort();
    journal
}

/// The lifecycle's observation cursor. The `handback` event is applied
/// through the ordinary intake, which prunes an observation once applied, so
/// the cursor is what records that the event was applied, and how often.
fn seam_repair_handback_cursor(store: &HubStore, vehicle: &VehicleRecord) -> i64 {
    seam_repair_handback_lifecycle(store, vehicle).0.last_observation_id
}

/// The stored charges: id, start and end.
fn seam_repair_handback_charges(store: &HubStore, vehicle: &VehicleRecord) -> Vec<(i64, i64, i64)> {
    seam_repair_charge_rows(store, vehicle)
        .into_iter()
        .map(|(id, json)| {
            let charge: serde_json::Value = serde_json::from_str(&json).expect("charge json");
            (
                id,
                charge["start_date_ms"].as_i64().expect("start"),
                charge["end_date_ms"].as_i64().expect("end"),
            )
        })
        .collect()
}

/// Hands back mid-drive or mid-charge, twice, and checks that the session is
/// closed at its last evidence with its children, journalled once; then two
/// later imports disjoint from it leave it as it is.
fn seam_repair_handback_scenario(session: SeamRepairHandbackSession, path: SeamRepairHandbackPath) {
    let test = format!("{session:?} {path:?}");
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    let start_ms = SEAM_T + 30 * SEAM_MINUTE;
    match path {
        SeamRepairHandbackPath::Window => assert!(
            record_coverage_open(&store.open().expect("catalogue"), 1, "grant-1", SEAM_T)
                .expect("window open")
        ),
        SeamRepairHandbackPath::Restore => seam_window(&store, 1, SEAM_T, None),
    }
    // The Hub never collects again after this: 25 children a minute apart.
    let (children, last_ms) = match session {
        SeamRepairHandbackSession::Drive => {
            let positions = seam_repair_hub_positions(car_id, OPEN_DRIVE, start_ms, 25);
            seam_repair_collect_open_drive(&store, &vehicle, car_id, OPEN_DRIVE, start_ms, &positions);
            let children: Vec<(i64, i64)> = positions
                .iter()
                .map(|position| (position.id, position.date_ms))
                .collect();
            (children, positions.last().expect("positions").date_ms)
        }
        SeamRepairHandbackSession::Charge => {
            let samples = seam_repair_hub_samples(OPEN_CHARGE, start_ms, 25);
            seam_repair_collect_open_charge(&store, &vehicle, car_id, OPEN_CHARGE, start_ms, &samples);
            let children: Vec<(i64, i64)> = samples
                .iter()
                .map(|sample| (sample.id, sample.timestamp_ms))
                .collect();
            (children, samples.last().expect("samples").timestamp_ms)
        }
    };
    assert_eq!(seam_repair_open_rows(&store, &vehicle).len(), 25, "{test}");
    let window = WindowCoverage {
        seq: 1,
        grant_digest: "grant-1".to_owned(),
        opened_at_ms: SEAM_T,
        closed_at_ms: SEAM_T + 60 * SEAM_MINUTE,
        gaps: Vec::new(),
    };
    let stored = |round: &str| {
        match session {
            SeamRepairHandbackSession::Drive => {
                assert_eq!(
                    seam_stored_drives(&store, &vehicle)
                        .into_iter()
                        .filter(|(id, _, _)| *id >= HUB_ID_BASE)
                        .collect::<Vec<_>>(),
                    vec![(OPEN_DRIVE, start_ms, last_ms)],
                    "{test}, {round}: the drive is closed at its last fold"
                );
                assert_eq!(
                    seam_stored_positions(&store, &vehicle),
                    children
                        .iter()
                        .map(|(id, at)| (*id, Some(OPEN_DRIVE), *at))
                        .collect::<Vec<_>>(),
                    "{test}, {round}: every position is stored once, under its drive"
                );
            }
            SeamRepairHandbackSession::Charge => {
                assert_eq!(
                    seam_repair_handback_charges(&store, &vehicle),
                    vec![(OPEN_CHARGE, start_ms, last_ms)],
                    "{test}, {round}: the charge is closed at its newest sample"
                );
                assert_eq!(
                    seam_repair_stored_samples(&store, &vehicle),
                    children
                        .iter()
                        .map(|(id, at)| (*id, OPEN_CHARGE, *at))
                        .collect::<Vec<_>>(),
                    "{test}, {round}: every sample is stored once, under its charge"
                );
            }
        }
        assert_eq!(seam_repair_open_drive(&store, &vehicle), None, "{test}, {round}");
        assert_eq!(seam_repair_open_charge(&store, &vehicle), None, "{test}, {round}");
        assert!(seam_repair_open_rows(&store, &vehicle).is_empty(), "{test}, {round}: no open row is left");
    };
    let before = seam_repair_handback_cursor(&store, &vehicle);
    let mut handed_back = None;
    let (parent, child) = match session {
        SeamRepairHandbackSession::Drive => (("drive", OPEN_DRIVE), "position"),
        SeamRepairHandbackSession::Charge => (("charge", OPEN_CHARGE), "charge_sample"),
    };
    let mut expected: Vec<(String, i64, String)> = std::iter::once((parent.0.to_owned(), parent.1, "upsert".to_owned()))
        .chain(
            children
                .iter()
                .map(|(id, _)| (child.to_owned(), *id, "upsert".to_owned())),
        )
        .collect();
    expected.sort();
    for attempt in ["handback", "repeated handback"] {
        let connection = store.open().expect("catalogue");
        match path {
            SeamRepairHandbackPath::Window => {
                assert!(record_coverage_handback(&connection, &window).expect(attempt));
            }
            SeamRepairHandbackPath::Restore => {
                let sealed = seal_restored_open_windows(&connection, None).expect(attempt);
                assert_eq!(sealed, usize::from(attempt == "handback"), "{test}, {attempt}");
            }
        }
        stored(attempt);
        assert_eq!(
            seam_repair_handback_hub_journal(&store, &vehicle),
            expected,
            "{test}, {attempt}: the session and its children are journalled once"
        );
        let cursor = seam_repair_handback_cursor(&store, &vehicle);
        assert!(cursor > before, "{test}, {attempt}: the handback event was applied");
        assert_eq!(*handed_back.get_or_insert(cursor), cursor, "{test}, {attempt}: and only once");
    }
    // Later imports, disjoint from the handed-back session, repeatedly.
    for round in ["first import", "second import"] {
        match session {
            SeamRepairHandbackSession::Drive => seam_publish(
                &store,
                &vehicle,
                &binding,
                &car,
                &[seam_drive(71, car_id, SEAM_T + 90 * SEAM_MINUTE, SEAM_T + 120 * SEAM_MINUTE)],
                Vec::new(),
            ),
            SeamRepairHandbackSession::Charge => seam_repair_publish_charges(
                &store,
                &vehicle,
                &binding,
                &car,
                &[seam_repair_charge(71, car_id, SEAM_T + 90 * SEAM_MINUTE, SEAM_T + 120 * SEAM_MINUTE)],
            ),
        }
        .expect(round);
        stored(round);
        assert!(seam_links(&store, &vehicle).is_empty(), "{test}, {round}");
        assert!(seam_repair_charge_links(&store, &vehicle).is_empty(), "{test}, {round}");
        assert_eq!(
            seam_repair_handback_hub_journal(&store, &vehicle),
            expected,
            "{test}, {round}: nothing is journalled again"
        );
        assert_eq!(handed_back, Some(seam_repair_handback_cursor(&store, &vehicle)), "{test}, {round}");
    }
}

#[test]
fn seam_repair_handback_mid_drive_closes_once_at_its_last_evidence() {
    seam_repair_handback_scenario(SeamRepairHandbackSession::Drive, SeamRepairHandbackPath::Window);
}

#[test]
fn seam_repair_handback_mid_charge_closes_once_at_its_last_evidence() {
    seam_repair_handback_scenario(SeamRepairHandbackSession::Charge, SeamRepairHandbackPath::Window);
}

#[test]
fn seam_repair_handback_restore_closes_the_open_drive_once() {
    seam_repair_handback_scenario(SeamRepairHandbackSession::Drive, SeamRepairHandbackPath::Restore);
}

#[test]
fn seam_repair_handback_restore_closes_the_open_charge_once() {
    seam_repair_handback_scenario(SeamRepairHandbackSession::Charge, SeamRepairHandbackPath::Restore);
}

#[test]
fn seam_repair_handback_a_window_still_open_closes_nothing() {
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    // The window running on this host, mid-drive.
    let connection = store.open().expect("catalogue");
    assert!(record_coverage_open(&connection, 2, "grant-2", SEAM_T).expect("window open"));
    let positions = seam_repair_hub_positions(car_id, OPEN_DRIVE, SEAM_T + 30 * SEAM_MINUTE, 25);
    seam_repair_collect_open_drive(
        &store,
        &vehicle,
        car_id,
        OPEN_DRIVE,
        SEAM_T + 30 * SEAM_MINUTE,
        &positions,
    );
    let open_rows = seam_repair_open_rows(&store, &vehicle);
    let cursor = seam_repair_handback_cursor(&store, &vehicle);
    let unchanged = |step: &str| {
        assert_eq!(seam_repair_open_drive(&store, &vehicle), Some(OPEN_DRIVE), "{step}");
        assert_eq!(seam_repair_open_rows(&store, &vehicle), open_rows, "{step}");
        assert!(seam_stored_drives(&store, &vehicle).is_empty(), "{step}");
        assert!(seam_repair_handback_hub_journal(&store, &vehicle).is_empty(), "{step}");
        assert_eq!(seam_repair_handback_cursor(&store, &vehicle), cursor, "{step}: no event applied");
    };
    // The running window is not sealed.
    assert_eq!(seal_restored_open_windows(&connection, Some((2, "grant-2"))).expect("seal"), 0);
    unchanged("the running window is left open");
    // A stale window of a restored backup is sealed, but the window running
    // here owns the open sessions.
    assert!(
        record_coverage_open(&connection, 1, "grant-1", SEAM_T - 120 * SEAM_MINUTE)
            .expect("stale window")
    );
    assert_eq!(seal_restored_open_windows(&connection, Some((2, "grant-2"))).expect("seal"), 1);
    unchanged("a stale window sealed while a window runs");
    // A publication refuses while a window is open in the store.
    assert!(matches!(
        seam_publish(&store, &vehicle, &binding, &car, &[], Vec::new()),
        Err(StoreError::ImportGenerationConflict)
    ));
    unchanged("a refused publication");
}
