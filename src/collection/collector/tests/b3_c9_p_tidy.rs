// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

fn layout69() -> (tempfile::TempDir, HubStore, Uuid) {
    let (d, s, v) = fixture(true);
    let source = import_source(&s);
    watermark(&s, source, v, 1200, T0 + 1200000);
    s.record_terrain_import_boundary(
        v,
        ImportBoundary {
            max_position_id: 1200,
            max_date_ms: T0 + 1200000,
        },
        T1,
    )
    .unwrap();
    seed(
        &s,
        v,
        (1..=1210)
            .map(|id| {
                let mut p = point(id, if id <= 1200 { 6 } else { 7 }, 0, 20);
                if id <= 1200 {
                    p.elevation = Some(100);
                }
                p
            })
            .collect(),
    );
    s.open().unwrap().execute("INSERT INTO terrain_elevation_provenance(vehicle_id,position_id,drive_id,latitude,longitude,elevation_m,tile_name,tile_hash,dataset_source,dataset_version,status,error_code,attempts,attempted_at_ms,retry_after_ms) SELECT vehicle_id,position_id,drive_id,json_extract(position_json,'$.latitude'),json_extract(position_json,'$.longitude'),100,'N10E020','synthetic','aws','1','success',NULL,1,?1,0 FROM materialised_positions WHERE position_id<=1200",[T1-1]).unwrap();
    (d, s, v)
}
#[tokio::test]
async fn c9_st69_tidy_first() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_d, s, v) = layout69();
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let mut state = TerrainWorkerState::default();
    let r = run_terrain_enrichment_pass(
        &s,
        &source,
        &mut state,
        &Access(false),
        &Permit(true),
        clock.as_ref(),
    )
    .await
    .unwrap();
    assert!(r.access_denied);
    assert_eq!(
        scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
        1200
    );
    let r = pass(&s, &source, &mut state).await;
    assert_eq!(
        (r.tidy_rows, r.full_batch, r.filled, source.count()),
        (1000, true, 0, 0)
    );
    assert!(!state.tidy_complete);
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM materialised_positions WHERE position_id<=1000 AND json_extract(position_json,'$.elevation') IS NULL"
        ),
        1000
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM materialised_positions WHERE position_id BETWEEN 1001 AND 1200 AND json_extract(position_json,'$.elevation')=100"
        ),
        200
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE drive_id=7"
        ),
        0
    );
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.tidy_rows, r.filled, source.count()), (200, 0, 0));
    assert!(state.tidy_complete);
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, source.count()), (10, 1));
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE position_id<=1200"
        ),
        0
    );
    assert_eq!(pair(&s, v, 6), (None, None));
    for id in 1201..=1210 {
        assert_eq!(elevation(&s, v, id), Some(0));
    }
    let before = durable(&s);
    pass(&s, &source, &mut state).await;
    assert_eq!(before, durable(&s));
    // C9 ST69 (c): each pass uses the marker left by the preceding locked pass.
    let (_d, s, _v) = layout69();
    let source = Source::new(&clock);
    let mut a = TerrainWorkerState::default();
    let mut b = TerrainWorkerState::default();
    let (a, b) = tokio::join!(pass(&s, &source, &mut a), pass(&s, &source, &mut b));
    assert_eq!(a.tidy_rows + b.tidy_rows, 1200);
    assert_eq!(source.count(), 0);
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='position'"
        ),
        1200
    );
    assert_eq!(s.terrain_tidy_progress().unwrap().cleared, 1200);
    // C9 ST69 (d): a new worker enters ordinary work on a completed marker.
    let (_d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 0, 20)]);
    let source = Source::new(&clock);
    let r = pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!((r.tidy_rows, r.filled, source.count()), (0, 1, 1));
}

fn layout71() -> (
    tempfile::TempDir,
    HubStore,
    Uuid,
    Uuid,
    Arc<ManualClock>,
    Source,
) {
    let (d, s, v) = fixture(true);
    let imported = import_source(&s);
    watermark(&s, imported, v, 10, T0 + 10000);
    seed(
        &s,
        v,
        (11..=15).map(|id| point(id, 8, id as usize, 20)).collect(),
    );
    plant(&s, v, 15, "void", None, 0);
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    source.script(
        "N10E020",
        Script::Nodes(vec![
            (11, 100),
            (12, 110),
            (13, 120),
            (14, 130),
            (16, 140),
            (17, 150),
        ]),
    );
    (d, s, v, imported, clock, source)
}
fn import_raise(
    s: &HubStore,
    source: Uuid,
    v: Uuid,
    id: i64,
    date: i64,
    now: i64,
) -> crate::db::TerrainBoundaryRaise {
    let mut c = s.open().unwrap();
    let tx = c
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    tx.execute("INSERT INTO lifecycle_source_watermarks(source_id,vehicle_id,domain,max_source_row_id,max_timestamp_ms) VALUES (?1,?2,'positions',?3,?4) ON CONFLICT(source_id,vehicle_id,domain) DO UPDATE SET max_source_row_id=MAX(max_source_row_id,excluded.max_source_row_id),max_timestamp_ms=MAX(max_timestamp_ms,excluded.max_timestamp_ms)",params![source.to_string(),v.to_string(),id,date]).unwrap();
    let result =
        crate::db::terrain_after_import_boundary_raise_in_transaction(&tx, v, now).unwrap();
    tx.commit().unwrap();
    result
}
async fn prepare71(s: &HubStore, v: Uuid, source: &Source, state: &mut TerrainWorkerState) {
    let r = pass(s, source, state).await;
    assert_eq!((r.tidy_rows, r.filled, source.count()), (1, 0, 0));
    let p = s.terrain_tidy_progress().unwrap();
    assert_eq!((p.run, p.examined, p.cleared, p.kept), (1, 1, 0, 0));
    source.clock.set(T1 + 10000);
    let r = pass(s, source, state).await;
    assert_eq!((r.filled, source.count()), (4, 1));
    for (id, value) in [(11, 100), (12, 110), (13, 120), (14, 130)] {
        assert_eq!(elevation(s, v, id), Some(value));
        assert_eq!(provenance(s, v, id).1, 1);
    }
    assert_eq!(pair(s, v, 8), (Some(30), Some(0)));
    assert_eq!(provenance(s, v, 15).0, "void");
    assert!(state.tidy_complete);
}
fn assert71(s: &HubStore, v: Uuid) {
    for id in [11, 12] {
        assert_eq!(elevation(s, v, id), None);
    }
    for (id, value) in [(13, 120), (14, 130)] {
        assert_eq!(elevation(s, v, id), Some(value));
        assert_eq!(provenance(s, v, id).0, "success");
    }
    assert_eq!(provenance(s, v, 15).0, "void");
    assert_eq!(pair(s, v, 8), (Some(10), Some(0)));
    let p = s.terrain_tidy_progress().unwrap();
    assert_eq!(
        (
            p.run,
            p.examined,
            p.cleared,
            p.kept,
            p.requeued,
            p.removed,
            p.drives_changed
        ),
        (2, 5, 2, 2, 0, 0, 1)
    );
    assert!(p.completed_at_ms.is_some());
}
#[tokio::test]
async fn c9_st71_rearm_seen() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_d, s, v, imported, clock, source) = layout71();
    let mut state = TerrainWorkerState::default();
    prepare71(&s, v, &source, &mut state).await;
    let t2 = T1 + 20000;
    assert_eq!(
        import_raise(&s, imported, v, 12, T0 + 12000, t2),
        crate::db::TerrainBoundaryRaise::RaisedAndRearmed
    );
    let p = s.terrain_tidy_progress().unwrap();
    assert_eq!(
        (p.run, p.examined, p.armed_at_ms, p.completed_at_ms),
        (2, 0, Some(t2), None)
    );
    assert!(state.tidy_complete);
    clear_log(&s);
    clock.set(T1 + 30000);
    let calls = source.count();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let reads = crate::db::TERRAIN_MARKER_READS.with(|n| n.get());
    let writable = s.instrumentation().writable_opens.load(Ordering::SeqCst);
    let copy = s.clone();
    state.marker_observer = Some(Box::new(move |complete| {
        captured.lock().unwrap().push((
            complete,
            crate::db::TERRAIN_MARKER_READS.with(|n| n.get()),
            copy.instrumentation().writable_opens.load(Ordering::SeqCst),
        ));
    }));
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.tidy_rows, r.filled, source.count()), (5, 0, calls));
    assert!(r.wrote);
    {
        let changes = observed.lock().unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0], (false, reads + 1, writable));
        assert_eq!((changes[1].0, changes[1].1), (true, reads + 1));
        assert!(changes[1].2 > writable);
    }
    state.marker_observer = None;
    assert71(&s, v);
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='position'"
        ),
        2
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='drive'"
        ),
        1
    );
    let before = durable(&s);
    pass(&s, &source, &mut state).await;
    assert_eq!(before, durable(&s));
    assert_eq!(source.count(), calls);
}
#[tokio::test]
async fn c9_st73_marker_once() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_d, s, v, imported, clock, source) = layout71();
    let mut state = TerrainWorkerState::default();
    prepare71(&s, v, &source, &mut state).await;
    import_raise(&s, imported, v, 12, T0 + 12000, T1 + 20000);
    pass(&s, &source, &mut state).await;
    let conn = s.open().unwrap();
    let version = data_version(&conn);
    for offset in [40000, 50000, 60000] {
        clock.set(T1 + offset);
        let before = crate::db::TERRAIN_MARKER_READS.with(|n| n.get());
        let calls = source.count();
        let r = pass(&s, &source, &mut state).await;
        assert_eq!(
            crate::db::TERRAIN_MARKER_READS.with(|n| n.get()) - before,
            1
        );
        assert_eq!((r.tidy_rows, source.count()), (0, calls));
        assert_eq!(version, data_version(&conn));
    }
    reset_marker(&s);
    let before = durable(&s);
    let reads = crate::db::TERRAIN_MARKER_READS.with(|n| n.get());
    let r = pass(&s, &source, &mut state).await;
    assert_eq!(crate::db::TERRAIN_MARKER_READS.with(|n| n.get()) - reads, 1);
    assert_eq!(r.tidy_rows, 3);
    assert_eq!(before, durable(&s));
    let p = s.terrain_tidy_progress().unwrap();
    assert_eq!(
        (
            p.examined,
            p.kept,
            p.cleared,
            p.requeued,
            p.removed,
            p.drives_changed
        ),
        (3, 2, 0, 0, 0, 0)
    );
    commit(&s, v, vec![drive(9)], vec![point(16, 9, 16, 20)]);
    let reads = crate::db::TERRAIN_MARKER_READS.with(|n| n.get());
    let r = pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(crate::db::TERRAIN_MARKER_READS.with(|n| n.get()) - reads, 1);
    assert_eq!((r.tidy_rows, r.filled), (0, 1));
}
#[tokio::test]
async fn c9_st75_rearm_stops_pass() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_d, s, v, imported, clock, source) = layout71();
    let mut state = TerrainWorkerState::default();
    prepare71(&s, v, &source, &mut state).await;
    watermark(&s, imported, v, 12, T0 + 12000);
    commit(
        &s,
        v,
        vec![drive(9)],
        vec![point(16, 9, 16, 20), point(17, 9, 17, 20)],
    );
    clock.set(T1 + 30000);
    let calls = source.count();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.full_batch, r.filled, source.count()), (true, 0, calls));
    assert!(r.wrote, "the boundary raise is durable progress");
    assert!(!state.tidy_complete);
    for id in [16, 17] {
        assert_eq!(elevation(&s, v, id), None);
    }
    let p = s.terrain_tidy_progress().unwrap();
    assert_eq!((p.run, p.armed_at_ms), (2, Some(T1 + 30000)));
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.tidy_rows, source.count()), (5, calls));
    assert71(&s, v);
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, source.count()), (2, calls + 1));
    assert_eq!(elevation(&s, v, 16), Some(140));
    assert_eq!(elevation(&s, v, 17), Some(150));
    // C9 R58: a chunk's own raise invalidates the rest of this tidy pass.
    let (_d, s, v) = fixture(true);
    let imported = import_source(&s);
    watermark(&s, imported, v, 11, T0 + 11000);
    seed(
        &s,
        v,
        (11..=13)
            .map(|id| {
                let mut p = point(id, 1, 0, 20);
                p.elevation = Some(100);
                p
            })
            .collect(),
    );
    for id in 11..=13 {
        plant(&s, v, id, "success", Some(100), 0);
    }
    let source = Source::new(&clock);
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.tidy_rows, r.full_batch, source.count()), (0, true, 0));
    assert!(!state.tidy_complete);
    assert_eq!(elevation(&s, v, 11), Some(100));
    let r = pass(&s, &source, &mut state).await;
    assert_eq!(r.tidy_rows, 3);
    assert_eq!(elevation(&s, v, 11), None);
    // C9 R58: a re-arm during acquisition rejects the prepared chunk and ends the pass.
    let (_d, s, v) = fixture(false);
    let mut old = point(1, 1, 0, 20);
    old.elevation = Some(100);
    seed(&s, v, vec![old, point(2, 1, 0, 20), point(3, 1, 0, 21)]);
    plant(&s, v, 1, "success", Some(100), 0);
    let source = Source::new(&clock);
    let copy = s.clone();
    *source.after.lock().unwrap() = Some(Box::new(move |_| {
        assert_eq!(
            copy.record_terrain_import_boundary(
                v,
                ImportBoundary {
                    max_position_id: 1,
                    max_date_ms: T0 + 1000
                },
                T1
            )
            .unwrap(),
            crate::db::TerrainBoundaryRaise::RaisedAndRearmed
        );
    }));
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, r.full_batch, source.count()), (0, true, 1));
    assert!(!state.tidy_complete);
    assert_eq!(elevation(&s, v, 2), None);
    assert_eq!(elevation(&s, v, 3), None);
    source.after.lock().unwrap().take();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.tidy_rows, r.filled, source.count()), (1, 0, 1));
    assert_eq!(elevation(&s, v, 1), None);
}

#[tokio::test]
async fn c9_st67_refill() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_d, s, v) = fixture(true);
    let imported = import_source(&s);
    watermark(&s, imported, v, 10, T0 + 10000);
    s.record_terrain_import_boundary(
        v,
        ImportBoundary {
            max_position_id: 10,
            max_date_ms: T0 + 10000,
        },
        T1,
    )
    .unwrap();
    let rows = [
        (1, 1, Some(100)),
        (2, 1, None),
        (5, 1, None),
        (11, 1, Some(110)),
        (12, 1, Some(120)),
        (3, 2, Some(50)),
        (13, 2, Some(60)),
        (14, 2, Some(70)),
        (15, 3, Some(200)),
        (16, 3, Some(210)),
        (17, 3, None),
        (18, 4, Some(300)),
        (19, 4, Some(310)),
        (20, 4, None),
        (21, 5, Some(401)),
        (22, 5, Some(410)),
    ];
    seed(
        &s,
        v,
        rows.into_iter()
            .map(|(id, d, e)| {
                let mut p = point(id, d, id as usize, 20);
                p.elevation = e;
                p
            })
            .collect(),
    );
    for (id, value) in [
        (1, 100),
        (11, 110),
        (12, 120),
        (3, 55),
        (13, 60),
        (14, 70),
        (16, 210),
        (17, 215),
        (18, 300),
        (19, 310),
        (20, 320),
        (21, 400),
        (22, 410),
    ] {
        plant(&s, v, id, "success", Some(value), 0);
    }
    plant(&s, v, 5, "void", None, 0);
    s.open()
        .unwrap()
        .execute(
            "UPDATE terrain_elevation_provenance SET longitude=?1 WHERE position_id=19",
            [20.0 + 40.0 / 1200.0],
        )
        .unwrap();
    s.open().unwrap().execute("INSERT INTO terrain_elevation_provenance SELECT vehicle_id,23,5,10.5,20.5,430,'N10E020','synthetic','aws','1','success',NULL,1,?1,0 FROM terrain_elevation_provenance LIMIT 1",[T1-1]).unwrap();
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    source.script("N10E020", Script::Nodes(vec![(19, 315), (20, 305)]));
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.tidy_rows, source.count()), (15, 0));
    let p = s.terrain_tidy_progress().unwrap();
    assert_eq!(
        (
            p.examined,
            p.cleared,
            p.requeued,
            p.removed,
            p.kept,
            p.drives_changed
        ),
        (15, 5, 2, 4, 3, 5)
    );
    clear_log(&s);
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, source.count()), (2, 1));
    for (id, e) in [(19, 315), (20, 305)] {
        assert_eq!(elevation(&s, v, id), Some(e));
        assert_eq!(provenance(&s, v, id).1, 2);
    }
    assert_eq!(pair(&s, v, 4), (Some(15), Some(10)));
    for id in [1, 13, 14, 16, 17, 22] {
        assert_eq!(elevation(&s, v, id), None);
        assert_eq!(
            scalar(
                &s,
                &format!(
                    "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE position_id={id}"
                )
            ),
            0
        );
    }
    let before = durable(&s);
    pass(&s, &source, &mut state).await;
    assert_eq!(source.count(), 1);
    assert_eq!(before, durable(&s));
}

#[tokio::test]
async fn c9_tidy_only_complete_is_read_only() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_dir, store, v) = fixture(false);
    seed(&store, v, vec![point(1, 1, 0, 20)]);
    let imported = import_source(&store);
    watermark(&store, imported, v, 1, T0 + 1000);
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let conn = store.open().unwrap();
    let version = data_version(&conn);
    let before = durable(&store);
    let writes = store
        .instrumentation()
        .writable_opens
        .load(Ordering::SeqCst);
    let reads = crate::db::TERRAIN_MARKER_READS.with(|n| n.get());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let mut state = TerrainWorkerState::default();
    state.marker_observer = Some(Box::new(move |complete| {
        captured.lock().unwrap().push(complete)
    }));
    let r = run_terrain_tidy_only_pass(&store, &mut state, &Access(true), clock.as_ref())
        .await
        .unwrap();
    assert_eq!(crate::db::TERRAIN_MARKER_READS.with(|n| n.get()) - reads, 1);
    assert_eq!(
        store
            .instrumentation()
            .writable_opens
            .load(Ordering::SeqCst),
        writes
    );
    assert_eq!(version, data_version(&conn));
    assert_eq!(before, durable(&store));
    assert_eq!(source.count(), 0);
    assert_eq!(*observed.lock().unwrap(), [true]);
    assert!(state.tidy_complete);
    assert!(!r.wrote && !r.full_batch);
    assert_eq!((r.filled, r.tiles, r.tidy_rows), (0, 0, 0));
}

#[tokio::test]
async fn c9_tidy_only_pending_never_selects() {
    let _pass_serial = super::super::TERRAIN_PASS_TEST_SERIAL.lock().await;
    let (_dir, store, _) = layout69();
    let clock = ManualClock::new();
    let mut state = TerrainWorkerState::default();
    for expected in [1000, 200] {
        let reads = crate::db::TERRAIN_MARKER_READS.with(|n| n.get());
        let r = run_terrain_tidy_only_pass(&store, &mut state, &Access(true), &clock)
            .await
            .unwrap();
        assert_eq!(crate::db::TERRAIN_MARKER_READS.with(|n| n.get()) - reads, 1);
        assert_eq!((r.tidy_rows, r.tiles, r.filled), (expected, 0, 0));
        assert!(r.full_batch && r.wrote);
    }
    assert!(state.tidy_complete);
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
        0
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT COUNT(*) FROM terrain_enrichment_state WHERE cursor_position_id > 0"
        ),
        0
    );
}
