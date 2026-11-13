// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T18: the store side of the seam repair (DEC-114). A store the v80 guard
//! refuses is refused before anything in it changes; the positions of a drive
//! still open when the window is handed back take part in the seam; a drive
//! kept because the Hub inherited it stays the Hub's on every later plan.

use super::*;
use crate::db::hub_ids::HUB_ID_BASE;
use crate::test_assertions::LabelledResult;

const OPEN_DRIVE: i64 = HUB_ID_BASE + 1;

/// Whether the v79 credential table has its keyring exposure column.
fn seam_repair_keyring_column(connection: &Connection) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('credential_format_state')
             WHERE name = 'keyring_exposure')",
            [],
            |row| row.get(0),
        )
        .expect("column check")
}

#[test]
fn seam_repair_store_refused_v79_store_is_unchanged_twice() {
    let test = "seam_repair_store_refused_v79_store_is_unchanged_twice";
    // A supported v79 store written before the keyring exposure column, which
    // holds a row the Hub collected before the seam (in neither catalogue).
    let temporary = crate::private_tempdir().expect("temporary store");
    {
        let store = HubStore::initialize(temporary.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("owner_api", "seam-repair-v79"), 1_000)
            .expect("source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "82").with_tesla_identity(Some(82), None),
                1_000,
            )
            .expect("vehicle");
        let connection = store.open().expect("catalogue");
        connection
            .execute(
                "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
                 VALUES (?1, 11, 82, '{}')",
                [vehicle.vehicle_id.to_string()],
            )
            .expect("pre-seam Hub charge");
        connection
            .execute_batch(
                "DROP TABLE import_seam_links;
                 DROP TABLE collection_coverage;
                 ALTER TABLE credential_format_state DROP COLUMN keyring_exposure;
                 PRAGMA user_version = 79;",
            )
            .expect("v79 image without the keyring column");
    }
    let raw = || Connection::open(temporary.path().join("hub.sqlite")).expect("raw catalogue");
    let before = super::g2_4_m1::schema_snapshot(&raw());
    assert_eq!(before.0, 79);
    assert!(!seam_repair_keyring_column(&raw()));
    for attempt in 0..2 {
        let error = HubStore::initialize(temporary.path())
            .map(|_| ())
            .labelled_err(test, "the pre-seam Hub row is refused");
        match error {
            StoreError::PreSeamCollectedRows { table, count } => {
                assert_eq!((table, count), ("materialised_charges", 1), "{test}: attempt {attempt}");
            }
            other => panic!("{test}: attempt {attempt}: expected the v80 refusal, found {other:?}"),
        }
        assert!(
            !seam_repair_keyring_column(&raw()),
            "{test}: attempt {attempt}: the credential repair did not run"
        );
        assert_eq!(
            super::g2_4_m1::schema_snapshot(&raw()),
            before,
            "{test}: attempt {attempt}: version, schema and every row are unchanged"
        );
    }
}

/// A Hub position as the lifecycle buffers it for an open drive.
fn seam_repair_open_position(
    id: i64,
    car_id: i64,
    drive_id: i64,
    date_ms: i64,
) -> crate::hub_pack::ProjectionPosition {
    serde_json::from_value(serde_json::json!({
        "id": id, "drive_id": drive_id, "car_id": car_id, "date_ms": date_ms,
        "latitude": 47.5, "longitude": 19.0
    }))
    .expect("open position")
}

/// The lifecycle commits an open drive `drive_id` and its positions, as the
/// collector leaves them when the window is handed back mid-drive: the drive
/// in the open session, its positions in `lifecycle_open_rows`.
fn seam_repair_collect_open_drive(
    store: &HubStore,
    vehicle: &VehicleRecord,
    car_id: i64,
    drive_id: i64,
    start_ms: i64,
    positions: &[crate::hub_pack::ProjectionPosition],
) {
    let previous = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .expect("lifecycle state");
    let mut state = previous
        .as_ref()
        .map(|record| {
            crate::lifecycle::OpenSessionState::decode(&record.open_session_json).expect("decode")
        })
        .unwrap_or_else(crate::lifecycle::OpenSessionState::new);
    let last = positions.last().map(|position| position.date_ms);
    let mut open: crate::lifecycle::OpenDrive = match state.open_drive.take() {
        Some(open) => open,
        None => serde_json::from_value(serde_json::json!({
            "id": drive_id, "car_id": car_id, "start_date_ms": start_ms,
            "outside_temp_sum": 0.0, "outside_temp_count": 0
        }))
        .expect("open drive"),
    };
    open.position_count += u32::try_from(positions.len()).expect("count");
    open.last_position_date_ms = last.or(open.last_position_date_ms);
    open.last_folded_at_ms = last.or(open.last_folded_at_ms);
    open.last_motion_at_ms = last.or(open.last_motion_at_ms);
    state.open_drive = Some(open);
    state.phase = crate::lifecycle::VehiclePhase::Driving;
    let encoded = state.encode().expect("state");
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id,
            open_session_json: &encoded,
            last_observation_id: previous.as_ref().map_or(0, |record| record.last_observation_id) + 1,
            quarantined: false,
            updated_at_ms: 2_000,
            delta: &crate::lifecycle::LifecycleDelta {
                open_drive_positions: positions.to_vec(),
                ..Default::default()
            },
        })
        .expect("lifecycle commit");
}

/// The open rows of the vehicle: id, domain and parent.
fn seam_repair_open_rows(store: &HubStore, vehicle: &VehicleRecord) -> Vec<(i64, String, Option<i64>)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT source_row_id, domain, parent_source_row_id FROM lifecycle_open_rows
              WHERE vehicle_id = ?1 ORDER BY domain, source_row_id",
        )
        .expect("open rows");
    statement
        .query_map(params![vehicle.vehicle_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("open rows")
        .collect::<Result<_, _>>()
        .expect("open rows")
}

/// The id of the lifecycle's open drive, if any.
fn seam_repair_open_drive(store: &HubStore, vehicle: &VehicleRecord) -> Option<i64> {
    let record = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .expect("lifecycle state")?;
    crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
        .expect("decode")
        .open_drive
        .map(|open| open.id)
}

/// The Hub's positions of one open drive: one a minute from `first_ms`.
fn seam_repair_hub_positions(
    car_id: i64,
    drive_id: i64,
    first_ms: i64,
    count: i64,
) -> Vec<crate::hub_pack::ProjectionPosition> {
    (0..count)
        .map(|minute| {
            seam_repair_open_position(
                HUB_ID_BASE + 1 + minute,
                car_id,
                drive_id,
                first_ms + minute * SEAM_MINUTE,
            )
        })
        .collect()
}

#[test]
fn seam_repair_store_open_drive_positions_survive_handback() {
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    // The window is handed back at minute 60 while the Hub's drive, opened at
    // minute 30, is still open: its 25 positions are only in the open rows.
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + 60 * SEAM_MINUTE));
    let positions = seam_repair_hub_positions(car_id, OPEN_DRIVE, SEAM_T + 30 * SEAM_MINUTE, 25);
    seam_repair_collect_open_drive(
        &store,
        &vehicle,
        car_id,
        OPEN_DRIVE,
        SEAM_T + 30 * SEAM_MINUTE,
        &positions,
    );
    assert_eq!(seam_repair_open_rows(&store, &vehicle).len(), 25);
    // The source's completed copy of the same trip runs across the handback.
    let source = vec![seam_drive(
        70,
        car_id,
        SEAM_T + 29 * SEAM_MINUTE,
        SEAM_T + 90 * SEAM_MINUTE,
    )];
    let mut journal = Vec::new();
    for round in ["publication", "replay"] {
        seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect(round);
        assert_eq!(
            seam_stored_drives(&store, &vehicle),
            vec![(70, source[0].start_date_ms, source[0].end_date_ms)],
            "{round}: the source's copy, which covers the Hub's, wins"
        );
        assert_eq!(
            seam_stored_positions(&store, &vehicle),
            positions
                .iter()
                .map(|position| (position.id, Some(70), position.date_ms))
                .collect::<Vec<_>>(),
            "{round}: every in-window Hub position survives once, under the winning drive"
        );
        assert_eq!(seam_links(&store, &vehicle), vec![(OPEN_DRIVE, 70)], "{round}");
        assert!(seam_repair_open_rows(&store, &vehicle).is_empty(), "{round}: no open row is left");
        assert_eq!(seam_repair_open_drive(&store, &vehicle), None, "{round}: the drive is settled");
        let mutations = seam_mutations(&store, &vehicle);
        assert!(
            mutations
                .iter()
                .all(|(entity, _, operation)| entity == "position" && operation == "upsert"),
            "{round}: the positions are journalled once; the open drive was never synced"
        );
        assert_eq!(mutations.len(), 25, "{round}");
        if round == "replay" {
            assert_eq!(mutations, journal, "replay changes nothing");
        }
        journal = mutations;
    }
}

#[test]
fn seam_repair_store_open_drive_kept_stays_open_with_its_positions() {
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + 60 * SEAM_MINUTE));
    let positions = seam_repair_hub_positions(car_id, OPEN_DRIVE, SEAM_T + 30 * SEAM_MINUTE, 25);
    seam_repair_collect_open_drive(
        &store,
        &vehicle,
        car_id,
        OPEN_DRIVE,
        SEAM_T + 30 * SEAM_MINUTE,
        &positions,
    );
    let open_before = seam_repair_open_rows(&store, &vehicle);
    // The source began its own drive at the handback: the two only abut, so
    // the Hub's open drive stands and stays open with every position.
    let source = vec![seam_drive(
        71,
        car_id,
        SEAM_T + 60 * SEAM_MINUTE,
        SEAM_T + 90 * SEAM_MINUTE,
    )];
    for round in ["publication", "replay"] {
        seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect(round);
        assert_eq!(
            seam_stored_drives(&store, &vehicle),
            vec![(71, source[0].start_date_ms, source[0].end_date_ms)],
            "{round}"
        );
        assert_eq!(seam_repair_open_rows(&store, &vehicle), open_before, "{round}: the open rows stay");
        assert_eq!(seam_repair_open_drive(&store, &vehicle), Some(OPEN_DRIVE), "{round}");
        assert!(seam_stored_positions(&store, &vehicle).is_empty(), "{round}");
        assert!(seam_links(&store, &vehicle).is_empty(), "{round}");
        assert!(seam_mutations(&store, &vehicle).is_empty(), "{round}");
    }
}

#[test]
fn seam_repair_store_open_drive_positions_survive_restore() {
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    // A backup taken inside the window, mid-drive: the store shows the window
    // open, and the Hub's only rows are the open drive's positions.
    seam_window(&store, 1, SEAM_T, None);
    let positions = seam_repair_hub_positions(car_id, OPEN_DRIVE, SEAM_T + 30 * SEAM_MINUTE, 25);
    seam_repair_collect_open_drive(
        &store,
        &vehicle,
        car_id,
        OPEN_DRIVE,
        SEAM_T + 30 * SEAM_MINUTE,
        &positions,
    );
    let last_ms = positions.last().expect("positions").date_ms;
    let sealed = crate::db::seal_restored_open_windows(&store.open().expect("catalogue"), None)
        .expect("seal the restored window");
    assert_eq!(sealed, 1);
    let coverage = crate::db::coverage_intervals_for_vehicle(
        &store.open().expect("catalogue"),
        &vehicle.vehicle_id.to_string(),
    )
    .expect("coverage");
    assert_eq!(
        coverage,
        vec![(SEAM_T, last_ms + 1)],
        "the restored coverage holds every open position's instant"
    );
    let source = vec![seam_drive(
        70,
        car_id,
        SEAM_T + 29 * SEAM_MINUTE,
        SEAM_T + 90 * SEAM_MINUTE,
    )];
    let mut journal = Vec::new();
    for round in ["publication", "replay"] {
        seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect(round);
        assert_eq!(
            seam_stored_positions(&store, &vehicle),
            positions
                .iter()
                .map(|position| (position.id, Some(70), position.date_ms))
                .collect::<Vec<_>>(),
            "{round}: every in-window Hub position survives once, under the winning drive"
        );
        assert_eq!(seam_links(&store, &vehicle), vec![(OPEN_DRIVE, 70)], "{round}");
        assert!(seam_repair_open_rows(&store, &vehicle).is_empty(), "{round}");
        let mutations = seam_mutations(&store, &vehicle);
        if round == "replay" {
            assert_eq!(mutations, journal, "replay changes nothing");
        }
        journal = mutations;
    }
}

/// The stored drive rows, whole.
fn seam_repair_drive_rows(store: &HubStore, vehicle: &VehicleRecord) -> Vec<(i64, String)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare("SELECT drive_id, drive_json FROM materialised_drives WHERE vehicle_id = ?1 ORDER BY drive_id")
        .expect("drives");
    statement
        .query_map(params![vehicle.vehicle_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("drive rows")
        .collect::<Result<_, _>>()
        .expect("drive rows")
}

/// Every sync mutation of the vehicle, whole.
fn seam_repair_journal(store: &HubStore, vehicle: &VehicleRecord) -> Vec<(i64, String, i64, String, String)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT revision, entity, entity_id, operation, payload_json FROM sync_mutations
              WHERE vehicle_id = ?1 ORDER BY revision",
        )
        .expect("mutations");
    statement
        .query_map(params![vehicle.vehicle_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })
        .expect("mutation rows")
        .collect::<Result<_, _>>()
        .expect("mutation rows")
}

#[test]
fn seam_repair_store_inherited_drive_stays_the_hubs_on_replay() {
    use crate::import::source_db::seam::InheritedCopy;
    let (_temporary, store, vehicle, binding, car) = seam_store();
    let car_id = binding.selected_car_id;
    seam_window(&store, 1, SEAM_T, Some(SEAM_T + 120 * SEAM_MINUTE));
    // The import before the window inherited the open drive 60; the Hub went
    // on collecting it from observations without coordinates and closed it an
    // hour after the window opened. No Hub-range position belongs to it.
    let inherited = seam_drive(60, car_id, SEAM_T - 20 * SEAM_MINUTE, SEAM_T + 60 * SEAM_MINUTE);
    seam_hub_drive(&store, &vehicle, &inherited);
    // The source closed its copy just after the window opened.
    let source = vec![
        seam_drive(59, car_id, SEAM_T - 90 * SEAM_MINUTE, SEAM_T - 80 * SEAM_MINUTE),
        seam_drive(60, car_id, SEAM_T - 20 * SEAM_MINUTE, SEAM_T + SEAM_MINUTE),
    ];
    let planned_ahead = || {
        let connection = store.open().expect("catalogue");
        let vehicle_key = vehicle.vehicle_id.to_string();
        let prior = durable_prior_imported_drive_ids_in_transaction(&connection, &vehicle_key)
            .expect("prior import");
        import_seam_plan_in_transaction(&connection, vehicle.vehicle_id, &source, &[], &prior)
            .expect("plan ahead of the packs")
            .1
            .drives
    };
    let mut kept = Vec::new();
    let mut journal = Vec::new();
    for round in ["publication", "replay", "second replay"] {
        seam_publish(&store, &vehicle, &binding, &car, &source, Vec::new()).expect(round);
        let rows = seam_repair_drive_rows(&store, &vehicle);
        assert_eq!(
            seam_stored_drives(&store, &vehicle),
            vec![
                (59, source[0].start_date_ms, source[0].end_date_ms),
                (60, inherited.start_date_ms, inherited.end_date_ms)
            ],
            "{round}: the stored row the Hub saw more of stays"
        );
        // The planner ahead of the packs, which reads the prior import that
        // now lists 60, still sees the Hub's inherited drive.
        let ahead = planned_ahead();
        assert!(
            ahead.inherited.contains(&(60, InheritedCopy::Stored)) && !ahead.imported.contains(&60),
            "{round}: the planner ahead of the packs keeps the stored copy: {ahead:?}"
        );
        let mutations = seam_repair_journal(&store, &vehicle);
        if round != "publication" {
            assert_eq!(rows, kept, "{round}: the stored row is unchanged");
            assert_eq!(mutations, journal, "{round}: the mutation journal is unchanged");
        }
        kept = rows;
        journal = mutations;
    }
}
