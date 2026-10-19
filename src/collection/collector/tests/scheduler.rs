// SPDX-License-Identifier: AGPL-3.0-only

// Tests of the stream-storage rules that are not part of the scheduler: the stream
// watermark, the stream transaction and the winner of concurrent equal timestamps.
// (The scheduler tests that were here were written before the clean-room rewrite and
// have been replaced by spec-derived tests in `scheduler_core.rs`.)

#[test]
fn stream_watermark_rejects_duplicate_and_old_frames_after_restart() {
    let temp = crate::private_tempdir().expect("temporary store");
    let vehicle_id = VehicleId::from_test(9);
    let first_timestamp = current_epoch_millis().expect("clock") - 60_000;
    let update = |timestamp_ms: i64, odometer: f64| crate::tesla_stream::StreamUpdate {
        tag: vehicle_id.to_string(),
        timestamp_ms,
        speed: Some(20.0),
        odometer: Some(odometer),
        soc: Some(80.0),
        elevation: Some(25.0),
        est_heading: Some(180),
        est_lat: Some(51.5),
        est_lng: Some(-0.1),
        power: Some(12),
        shift_state: Some("D".to_owned()),
        range: Some(200.0),
        est_range: Some(210.0),
        heading: Some(180.0),
    };

    let store = HubStore::initialize(temp.path()).expect("store");
    persist_stream_update(&store, vehicle_id, &update(first_timestamp, 100.0))
        .expect("first frame");
    persist_stream_update(&store, vehicle_id, &update(first_timestamp, 100.0))
        .expect("duplicate frame");
    persist_stream_update(&store, vehicle_id, &update(first_timestamp - 1, 99.0))
        .expect("old frame");
    drop(store);

    let store = HubStore::initialize(temp.path()).expect("restart");
    persist_stream_update(&store, vehicle_id, &update(first_timestamp + 1_000, 101.0))
        .expect("new frame");

    let registered = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle identity")
        .parse::<Uuid>()
        .expect("vehicle UUID");
    assert!(
        store
            .observations_for_vehicle(registered, crate::db::ObservationQuery::from_start(10),)
            .expect("pruned stream observations")
            .is_empty()
    );
    let observations = store
        .current_observations_for_vehicle(registered)
        .expect("current stream observation");
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].observed_at_ms, first_timestamp + 1_000);
    let lifecycle = store
        .load_lifecycle_state(registered)
        .expect("lifecycle state")
        .expect("open lifecycle state");
    let open = crate::lifecycle::OpenSessionState::decode(&lifecycle.open_session_json)
        .expect("open session");
    assert!(open.open_drive.expect("open drive").positions.is_empty());
    let provisional: i64 = store
        .open()
        .expect("database")
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows
                 WHERE vehicle_id = ?1 AND domain = 'position'",
            [registered.to_string()],
            |row| row.get(0),
        )
        .expect("provisional positions");
    assert_eq!(provisional, 2);

    let mut context = stream_context(&store, vehicle_id, None).expect("stream context");
    assert_eq!(
        context.last_stream_timestamp_ms,
        Some(first_timestamp + 1_000)
    );
    assert!(!context.queue_lagging);
    context.report_ingestion_health(first_timestamp + 12_000, Duration::from_secs(6));
    assert_eq!(
        context.last_stream_timestamp_ms,
        Some(first_timestamp + 12_000)
    );
    assert!(context.queue_lagging);
    context.report_ingestion_health(first_timestamp + 13_000, Duration::from_millis(500));
    assert!(!context.queue_lagging);
}

#[test]
fn stream_transaction_rolls_back_every_stage_and_retry_commits_once() {
    let points = [
        StreamFaultPoint::RawInsert,
        StreamFaultPoint::LifecycleWrite,
        StreamFaultPoint::WatermarkUpdate,
        StreamFaultPoint::Commit,
    ];
    for (index, point) in points.into_iter().enumerate() {
        let temp = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let vehicle_id = VehicleId::from_test(index as u64 + 40);
        let timestamp = current_epoch_millis().expect("clock") - 60_000;
        let update = crate::tesla_stream::StreamUpdate {
            tag: vehicle_id.to_string(),
            timestamp_ms: timestamp,
            speed: Some(20.0),
            odometer: Some(100.0),
            soc: Some(80.0),
            elevation: Some(25.0),
            est_heading: Some(180),
            est_lat: Some(51.5),
            est_lng: Some(-0.1),
            power: Some(12),
            shift_state: Some("D".to_owned()),
            range: Some(200.0),
            est_range: Some(210.0),
            heading: Some(180.0),
        };
        store.inject_stream_fault(point);
        assert!(persist_stream_update(&store, vehicle_id, &update).is_err());
        let connection = store.open().expect("database");
        for table in [
            "raw_observations",
            "current_observations",
            "stream_watermarks",
            "vehicle_lifecycle_state",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("fault count");
            assert_eq!(count, 0, "fault point {point:?} left {table}");
        }
        drop(connection);

        assert!(persist_stream_update(&store, vehicle_id, &update).expect("retry"));
        assert!(!persist_stream_update(&store, vehicle_id, &update).expect("duplicate"));
        let connection = store.open().expect("database");
        let raw: i64 = connection
            .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                row.get(0)
            })
            .expect("raw count");
        assert_eq!(raw, 0);
        for table in [
            "current_observations",
            "stream_watermarks",
            "vehicle_lifecycle_state",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("committed count");
            assert_eq!(count, 1, "missing committed {table}");
        }
    }
}

#[test]
fn concurrent_same_timestamp_has_one_committed_winner_and_restart_is_idempotent() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let vehicle_id = VehicleId::from_test(90);
    let timestamp = current_epoch_millis().expect("clock") - 60_000;
    let update = crate::tesla_stream::StreamUpdate {
        tag: vehicle_id.to_string(),
        timestamp_ms: timestamp,
        speed: Some(20.0),
        odometer: Some(100.0),
        soc: Some(80.0),
        elevation: Some(25.0),
        est_heading: Some(180),
        est_lat: Some(51.5),
        est_lng: Some(-0.1),
        power: Some(12),
        shift_state: Some("D".to_owned()),
        range: Some(200.0),
        est_range: Some(210.0),
        heading: Some(180.0),
    };
    let first = store.clone();
    let second = store.clone();
    let left_update = update.clone();
    let right_update = update.clone();
    let left = std::thread::spawn(move || persist_stream_update(&first, vehicle_id, &left_update));
    let right =
        std::thread::spawn(move || persist_stream_update(&second, vehicle_id, &right_update));
    let results = [
        left.join().expect("left").expect("left result"),
        right.join().expect("right").expect("right result"),
    ];
    assert_eq!(results.iter().filter(|value| **value).count(), 1);
    assert_eq!(results.iter().filter(|value| !**value).count(), 1);
    let registered = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle")
        .parse::<Uuid>()
        .expect("uuid");
    assert!(
        store
            .observations_for_vehicle(registered, crate::db::ObservationQuery::from_start(10),)
            .expect("pruned observations")
            .is_empty()
    );
    assert_eq!(
        store
            .current_observations_for_vehicle(registered)
            .expect("current observation")
            .len(),
        1
    );
    drop(store);
    let restarted = HubStore::initialize(temp.path()).expect("restart");
    assert!(!persist_stream_update(&restarted, vehicle_id, &update).expect("restart retry"));
}
