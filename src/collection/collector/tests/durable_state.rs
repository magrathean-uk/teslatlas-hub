// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C3 part K3 (durable state, ticks, gaps and status): CT13, CT14,
// CT29, CT30, CT52 to CT54, CT56, CT64, CT69 and CT70, with the persistence
// half of CT58, plus tests of the rules the part owns (R4 to R10, R13, R14,
// R66). Synthetic data only; the fake sources are in-process and no test
// touches the network.

/// A log capture at DEBUG and below, to count and read the decision events. It
/// captures the thread it is started on through the one process-wide capture
/// (`crate::test_log_capture`), so async tests use the default current-thread
/// runtime. The capture is shared with the storage log tests, because a process
/// can install only one global subscriber.
struct DecisionLog;

impl DecisionLog {
    fn start() -> Self {
        crate::test_log_capture::arm();
        Self
    }

    fn contents(&self) -> String {
        crate::test_log_capture::read()
    }

    /// The lines that carry `needle` and the level word.
    fn lines(&self, level: &str, needle: &str) -> usize {
        self.contents()
            .lines()
            .filter(|line| line.contains(&format!(" {level} ")) && line.contains(needle))
            .count()
    }
}

impl Drop for DecisionLog {
    fn drop(&mut self) {
        crate::test_log_capture::disarm();
    }
}

/// A scheduler with a real store in a temporary directory, a manual clock and the
/// pieces of the tick transaction (C3 section 9: store tests use
/// `HubStore::initialize` and the tick-commit hook).
struct TickBed {
    _dir: tempfile::TempDir,
    store: HubStore,
    scheduler: VehicleScheduler,
    clock: Arc<ManualClock>,
    identities: IdentityCache,
    cursor_key: CursorKey,
    terrain: mpsc::Sender<()>,
    _terrain_rx: mpsc::Receiver<()>,
    provider: CollectorProvider,
}

impl TickBed {
    fn new(mode: crate::config::CollectorMode) -> Self {
        let dir = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(dir.path()).expect("store");
        let t0 = Instant::now();
        let clock = ManualClock::new(t0, CORE_W0);
        let mut scheduler = VehicleScheduler::new_with(
            core_cadence(),
            mode,
            core_cadence().fleet_policy(),
            t0,
            clock.clone(),
            FixedJitter::constant(0.0),
        );
        // The fingerprint every reload in these tests presents.
        scheduler.config_fingerprint = DS_FP_A.to_owned();
        let (terrain, terrain_rx) = mpsc::channel(8);
        Self {
            _dir: dir,
            store,
            scheduler,
            clock,
            identities: IdentityCache::default(),
            cursor_key: CursorKey::from_bytes([0x13; 32]),
            terrain,
            _terrain_rx: terrain_rx,
            provider: if mode == crate::config::CollectorMode::FleetPolling {
                CollectorProvider::Fleet
            } else {
                CollectorProvider::Legacy
            },
        }
    }

    fn owner() -> Self {
        Self::new(crate::config::CollectorMode::Legacy)
    }

    fn now(&self) -> Instant {
        self.clock.mono_now()
    }

    fn wall(&self) -> i64 {
        self.clock.wall_now_ms().expect("wall clock")
    }

    fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    /// Register a car in the store and give its scheduler record its Hub id, as
    /// `apply_control_settings` does in the loop.
    fn register(&mut self, vehicle: &Vehicle) -> Uuid {
        let wall = self.wall();
        let registered = self
            .identities
            .resolve(&self.store, self.provider, vehicle, wall)
            .expect("identity");
        if let Some(record) = self.scheduler.vehicles.get_mut(&vehicle.id) {
            record.hub_id = Some(registered.hub_id);
        }
        registered.hub_id
    }

    async fn commit(&mut self, batch: TickBatch) -> TickOutcome {
        let now = self.now();
        let commit = TickCommit {
            store: &self.store,
            cursor_key: &self.cursor_key,
            provider: self.provider,
            offline_drive_timeout: Duration::from_secs(600),
        };
        commit_tick(
            &commit,
            &mut self.identities,
            &mut self.scheduler,
            batch,
            now,
        )
        .await
        .expect("commit tick")
    }

    /// One tick whose only request is a list.
    async fn list(&mut self, vehicles: Vec<Vehicle>) -> TickOutcome {
        let now = self.now();
        self.scheduler.begin_tick();
        let candidates = self.scheduler.accept_discovery(vehicles, now);
        let received_at_ms = self.wall();
        for candidate in &candidates {
            self.register(candidate);
        }
        let batch = TickBatch {
            discoveries: candidates
                .into_iter()
                .map(|vehicle| DiscoveryItem {
                    vehicle,
                    received_at_ms,
                })
                .collect(),
            first_result_at: Some(now),
            ..TickBatch::default()
        };
        self.commit(batch).await
    }

    /// The snapshot item of a successful parked read of car `id`, applied to the
    /// scheduler as the loop does.
    fn read_item(&mut self, id: u64, fields: Value) -> SnapshotItem {
        let now = self.now();
        let vehicle = self.scheduler.vehicles[&core_id(id)].vehicle.clone();
        let snapshot = VehicleData::for_test(id, fields);
        self.scheduler
            .vehicle_succeeded(core_id(id), poll_phase(&snapshot), true, now);
        SnapshotItem {
            vehicle,
            snapshot,
            received_at_ms: self.wall(),
        }
    }

    /// One tick of successful parked reads of the cars due now.
    async fn read_due(&mut self) -> (usize, TickOutcome) {
        let now = self.now();
        self.scheduler.begin_tick();
        let due = self.scheduler.due_vehicles(now);
        let mut batch = TickBatch::default();
        for id in &due {
            let item = self.read_item(id.get(), parked_fields());
            batch.snapshots.push(item);
        }
        if !due.is_empty() {
            batch.note_result(now);
        }
        let outcome = self.commit(batch).await;
        (due.len(), outcome)
    }

    fn query<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .open()
            .expect("connection")
            .query_row(sql, [], |row| row.get(0))
            .expect("query")
    }

    /// The current discovery row of the car: its observation id and state.
    fn discovery_row(&self, hub: Uuid) -> Option<(i64, String)> {
        use rusqlite::OptionalExtension;
        self.store
            .open()
            .expect("connection")
            .query_row(
                "SELECT observation_id, json_extract(payload_json, '$.source_vehicle_state')
                   FROM current_observations
                  WHERE vehicle_id = ?1 AND record_type = 'owner_api_discovery_v1'",
                rusqlite::params![hub.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .expect("discovery row")
    }
}

fn write_collector_rows(store: &HubStore, write: &crate::db::CollectorRowsWrite) {
    store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(transaction, write)?;
                Ok::<_, StoreError>(((), wrote))
            },
            || Ok::<(), StoreError>(()),
        )
        .expect("collector rows");
}

fn stored_vehicle_row(
    hub: Uuid,
    phase: &str,
    hold: &str,
    reason: &str,
) -> crate::db::CollectorVehicleRow {
    crate::db::CollectorVehicleRow {
        vehicle_id: hub,
        phase: phase.to_owned(),
        hold: hold.to_owned(),
        reason_code: reason.to_owned(),
        phase_since_ms: CORE_W0,
        suspended_until_ms: None,
        next_probe_ms: None,
        last_wake_read_at_ms: None,
    }
}

const DS_FP_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DS_FP_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A fresh scheduler on its own manual clock at wall time `wall_ms`, as a
/// restarted process has: its monotonic clock starts again.
fn restarted_scheduler(
    mode: crate::config::CollectorMode,
    wall_ms: i64,
) -> (VehicleScheduler, Arc<ManualClock>, Instant) {
    let t0 = Instant::now();
    let clock = ManualClock::new(t0, wall_ms);
    let scheduler = VehicleScheduler::new_with(
        core_cadence(),
        mode,
        core_cadence().fleet_policy(),
        t0,
        clock.clone(),
        FixedJitter::constant(0.0),
    );
    (scheduler, clock, t0)
}

/// The scheduler's load for the car with Hub id `hub` and Tesla id 1.
fn snapshot_of(store: &HubStore, hub: Uuid, fingerprint: &str) -> CollectorStateSnapshot {
    CollectorStateSnapshot::from_rows(
        store.load_collector_state_rows().expect("rows"),
        fingerprint.to_owned(),
        &[(hub, 1, crate::hub_pack::ProjectionCarSettings::default())],
    )
}

// ---- CT13 -------------------------------------------------------------------

#[tokio::test]
async fn ct13_one_transaction_per_tick() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            // The first list: two cars online, both discoveries are new to the
            // lifecycle. One transaction carries both and the scheduler rows.
            let outcome = bed
                .list(vec![core_vehicle(1, "online"), core_vehicle(2, "online")])
                .await;
            assert!(matches!(
                outcome,
                TickOutcome::Committed { wrote: true, .. }
            ));
            assert_eq!(seam.tick_commits(), 1);

            // Two successful reads and one discovery that differs from the car's
            // last reachability: one more transaction, not three.
            bed.advance(Duration::from_secs(60));
            let now = bed.now();
            bed.scheduler.begin_tick();
            let mut batch = TickBatch::default();
            for id in [1, 2] {
                let item = bed.read_item(id, parked_fields());
                batch.snapshots.push(item);
            }
            let offline = bed
                .scheduler
                .accept_vehicle_state(core_id(2), "offline".to_owned(), now);
            let received_at_ms = bed.wall();
            batch.discoveries = offline
                .into_iter()
                .map(|vehicle| DiscoveryItem {
                    vehicle,
                    received_at_ms,
                })
                .collect();
            batch.note_result(now);
            let outcome = bed.commit(batch).await;
            assert!(matches!(
                outcome,
                TickOutcome::Committed {
                    wrote: true,
                    report: Some(_)
                }
            ));
            assert_eq!(seam.tick_commits(), 2, "one commit for the whole tick");
        })
        .await;
}

#[tokio::test]
async fn ct13_a_list_equal_to_the_lifecycle_commits_nothing() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            assert!(matches!(
                bed.list(vec![core_vehicle(3, "asleep")]).await,
                TickOutcome::Committed { wrote: true, .. }
            ));
            assert_eq!(seam.tick_commits(), 1);
            // The delivery made the cached view stale, so the next list reads it
            // once inside the transaction, finds the state equal and rolls back:
            // a transaction ran, nothing was committed.
            bed.advance(Duration::from_secs(30));
            assert!(matches!(
                bed.list(vec![core_vehicle(3, "asleep")]).await,
                TickOutcome::Committed { wrote: false, .. }
            ));
            assert_eq!(seam.tick_commits(), 1);
            // With the view cached the decision is made in memory: the store is
            // not touched at all.
            bed.advance(Duration::from_secs(30));
            let attempts = seam.tick_commit_attempts.load(Ordering::SeqCst);
            assert!(matches!(
                bed.list(vec![core_vehicle(3, "asleep")]).await,
                TickOutcome::Idle
            ));
            assert_eq!(seam.tick_commits(), 1);
            assert_eq!(seam.tick_commit_attempts.load(Ordering::SeqCst), attempts);
        })
        .await;
}

#[tokio::test]
async fn ct13_a_counter_only_tick_is_one_lone_commit() {
    // The mechanism of the lone reservation commit (R5, R27): scheduler rows with
    // no result pending commit in a transaction of their own. Part K7 decides when
    // a reservation is needed and fills `PendingTick::counters`.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::new(crate::config::CollectorMode::FleetPolling);
            bed.scheduler
                .pending
                .counters
                .push(crate::db::CollectorCounterRow {
                    month: "2026-10".to_owned(),
                    mode: "fleet_polling".to_owned(),
                    category: "state".to_owned(),
                    sent: 0,
                    billable: 0,
                    reserved: 1,
                });
            assert!(matches!(
                bed.commit(TickBatch::default()).await,
                TickOutcome::Committed { wrote: true, .. }
            ));
            assert_eq!(seam.tick_commits(), 1);
            let rows = bed.store.load_collector_state_rows().unwrap();
            assert_eq!(rows.counters.len(), 1);
            assert_eq!(rows.counters[0].reserved, 1);
            // Nothing pending afterwards: no second commit.
            assert!(matches!(
                bed.commit(TickBatch::default()).await,
                TickOutcome::Idle
            ));
            assert_eq!(seam.tick_commits(), 1);
        })
        .await;
}

// ---- CT14 -------------------------------------------------------------------

#[tokio::test]
async fn ct14_a_discovery_is_stored_when_the_state_changes_and_not_when_it_repeats() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            let car = |state: &str| vec![core_vehicle(1, state)];
            bed.list(car("asleep")).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            let (first_id, state) = bed.discovery_row(hub).expect("discovery row");
            assert_eq!(state, "asleep");
            assert_eq!(seam.tick_commits(), 1);
            // Lists at +30 s and +60 s say asleep again: the same row, no commit.
            for _ in 0..2 {
                bed.advance(Duration::from_secs(30));
                bed.list(car("asleep")).await;
                assert_eq!(
                    bed.discovery_row(hub),
                    Some((first_id, "asleep".to_owned()))
                );
            }
            assert_eq!(seam.tick_commits(), 1);
            // Online is a change: a second discovery row.
            bed.advance(Duration::from_secs(30));
            bed.list(car("online")).await;
            let (second_id, state) = bed.discovery_row(hub).expect("discovery row");
            assert_eq!(state, "online");
            assert!(second_id > first_id);
            assert_eq!(seam.tick_commits(), 2);
            // No stored state is a Hub-chosen "suspended".
            let suspended: i64 = bed.query(
                "SELECT COUNT(*) FROM current_observations
                  WHERE payload_json LIKE '%\"suspended\"%'",
            );
            assert_eq!(suspended, 0);
        })
        .await;
}

#[tokio::test]
async fn ct14_c1_st81_a_wake_seen_only_through_the_stream_is_not_hidden_by_a_repeat() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            let asleep = || vec![core_vehicle(1, "asleep")];
            bed.list(asleep()).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            let registered = bed.identities.entries[&core_id(1)].registered.clone();
            assert_eq!(
                bed.store
                    .collector_lifecycle_snapshot(hub)
                    .unwrap()
                    .last_reach
                    .as_deref(),
                Some("asleep")
            );
            // A stream frame at +2 min is stored (the car is online) and the
            // stream drain reports the car as changed.
            bed.advance(Duration::from_secs(120));
            let frame_ms = bed.wall();
            let update = crate::tesla_stream::StreamUpdate {
                tag: "1".to_owned(),
                timestamp_ms: frame_ms,
                speed: None,
                odometer: None,
                soc: Some(50.0),
                elevation: None,
                est_heading: None,
                est_lat: None,
                est_lng: None,
                power: Some(0),
                shift_state: None,
                range: None,
                est_range: None,
                heading: None,
            };
            bed.store
                .accept_stream_observation_and_lifecycle(
                    &ObservationInput {
                        source_id: registered.source_id,
                        vehicle_id: hub,
                        observed_at_ms: frame_ms,
                        payload: crate::lifecycle::stream_observation_payload(&update),
                    },
                    frame_ms,
                    registered.pack_car_id,
                )
                .expect("stream frame");
            bed.scheduler.lifecycle_changed(core_id(1));
            assert_eq!(
                bed.store
                    .collector_lifecycle_snapshot(hub)
                    .unwrap()
                    .last_reach
                    .as_deref(),
                Some("online")
            );
            let commits = seam.tick_commits();
            // At +5 min the list says asleep again: that differs from the
            // lifecycle's online, so it is stored (a second asleep row).
            bed.advance(Duration::from_secs(180));
            let before = bed.discovery_row(hub).expect("row").0;
            bed.list(asleep()).await;
            let (stored_id, state) = bed.discovery_row(hub).expect("row");
            assert_eq!(state, "asleep");
            assert!(
                stored_id > before,
                "the repeat of the old asleep was stored"
            );
            assert_eq!(seam.tick_commits(), commits + 1);
            // Asleep again at +5 min 30 s: now equal, nothing is stored.
            bed.advance(Duration::from_secs(30));
            bed.list(asleep()).await;
            assert_eq!(
                bed.discovery_row(hub),
                Some((stored_id, "asleep".to_owned()))
            );
            assert_eq!(seam.tick_commits(), commits + 1);
        })
        .await;
}

#[tokio::test]
async fn ct14_a_missing_current_row_is_written_without_a_raw_row() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "asleep")]).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            let last_observation: i64 =
                bed.query("SELECT last_observation_id FROM vehicle_lifecycle_state");
            bed.store
                .open()
                .unwrap()
                .execute(
                    "DELETE FROM current_observations WHERE record_type = 'owner_api_discovery_v1'",
                    [],
                )
                .unwrap();
            assert!(bed.discovery_row(hub).is_none());
            bed.scheduler.lifecycle_changed(core_id(1));
            bed.advance(Duration::from_secs(30));
            let commits = seam.tick_commits();
            assert!(matches!(
                bed.list(vec![core_vehicle(1, "asleep")]).await,
                TickOutcome::Committed { wrote: true, .. }
            ));
            assert_eq!(bed.discovery_row(hub).expect("rewritten").1, "asleep");
            assert_eq!(seam.tick_commits(), commits + 1);
            let after: i64 = bed.query("SELECT last_observation_id FROM vehicle_lifecycle_state");
            assert_eq!(
                after, last_observation,
                "the lifecycle saw no new observation"
            );
        })
        .await;
}

#[tokio::test]
async fn a_state_without_reachability_is_stored_only_when_it_differs_from_the_current_row() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "online")]).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            bed.advance(Duration::from_secs(30));
            bed.list(vec![core_vehicle(1, "mystery")]).await;
            let (id, state) = bed.discovery_row(hub).expect("row");
            assert_eq!(state, "mystery");
            let commits = seam.tick_commits();
            for _ in 0..3 {
                bed.advance(Duration::from_secs(30));
                bed.list(vec![core_vehicle(1, "mystery")]).await;
            }
            assert_eq!(bed.discovery_row(hub), Some((id, "mystery".to_owned())));
            assert_eq!(seam.tick_commits(), commits);
        })
        .await;
}

// ---- CT29, CT30 -------------------------------------------------------------

/// A bed whose store holds one registered car (Tesla id 1) and nothing else.
fn bed_with_stored_car() -> (TickBed, Uuid) {
    let mut bed = TickBed::owner();
    let hub = bed.register(&core_vehicle(1, "online"));
    (bed, hub)
}

#[test]
fn ct29_a_restart_keeps_a_suspension() {
    let (bed, hub) = bed_with_stored_car();
    let mut row = stored_vehicle_row(hub, "suspended", "none", "suspended_idle");
    row.suspended_until_ms = Some(CORE_W0 + 30 * 60_000);
    row.phase_since_ms = CORE_W0;
    write_collector_rows(
        &bed.store,
        &crate::db::CollectorRowsWrite {
            now_ms: CORE_W0,
            vehicles: vec![row],
            ..Default::default()
        },
    );
    // The process dies at w0 + 10 min and restarts at w0 + 12 min.
    let (mut scheduler, _clock, t) =
        restarted_scheduler(crate::config::CollectorMode::Legacy, CORE_W0 + 12 * 60_000);
    scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
    let candidates = scheduler.accept_discovery(vec![core_vehicle(1, "online")], t);
    assert_eq!(candidates.len(), 1);
    {
        let car = &scheduler.vehicles[&core_id(1)];
        assert_eq!(car.phase, PollPhase::Suspended);
        assert_eq!(car.hold, VehicleHold::None);
        assert_eq!(car.suspended_until, Some(t + Duration::from_secs(18 * 60)));
        assert_eq!(car.reason, "suspended_idle", "not a first contact");
        assert_eq!(car.next_read_due, None);
        assert_eq!(car.hub_id, Some(hub));
    }
    assert!(scheduler.due_vehicles(t).is_empty());
    assert!(
        scheduler
            .due_vehicles(t + Duration::from_secs(18 * 60 - 1))
            .is_empty()
    );
    // The restart rewrites nothing: memory equals the store.
    assert!(scheduler.take_tick().is_empty());
}

#[test]
fn ct30_a_restart_with_the_wall_clock_stepped_back_clamps_the_deadline() {
    let (bed, hub) = bed_with_stored_car();
    let mut row = stored_vehicle_row(hub, "suspended", "none", "suspended_idle");
    row.suspended_until_ms = Some(CORE_W0 + 30 * 60_000);
    row.last_wake_read_at_ms = Some(CORE_W0 - 60_000);
    write_collector_rows(
        &bed.store,
        &crate::db::CollectorRowsWrite {
            now_ms: CORE_W0,
            vehicles: vec![row],
            ..Default::default()
        },
    );
    let car_after = |wall_ms: i64| {
        let (mut scheduler, _clock, t) =
            restarted_scheduler(crate::config::CollectorMode::Legacy, wall_ms);
        scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
        scheduler.accept_discovery(vec![core_vehicle(1, "online")], t);
        (scheduler.vehicles[&core_id(1)].clone(), t)
    };
    // Wall clock two hours back: the deadline is clamped to the suspension's own
    // length (30 min), not 2 h 30 min, and the wake read counts as just made, so
    // the wake-read limit holds for its full length.
    let (car, t) = car_after(CORE_W0 - 2 * 3_600_000);
    assert_eq!(car.phase, PollPhase::Suspended);
    assert_eq!(car.suspended_until, Some(t + Duration::from_secs(30 * 60)));
    assert_eq!(car.last_wake_read, Some(t));
    // Forty minutes on: the suspension has ended (the car is Unknown and gets its
    // first contact), and the wake read is older than its bound and forgotten.
    let (car, _) = car_after(CORE_W0 + 40 * 60_000);
    assert_eq!(car.phase, PollPhase::Unknown);
    assert_eq!(car.suspended_until, None);
    assert_eq!(car.last_wake_read, None);
    // Fifteen minutes on, the wake read is 16 min old: it loads as that age.
    let (car, t) = car_after(CORE_W0 + 15 * 60_000);
    assert_eq!(car.last_wake_read, Some(t - Duration::from_secs(16 * 60)));
    assert_eq!(car.suspended_until, Some(t + Duration::from_secs(15 * 60)));
}

// ---- CT52 -------------------------------------------------------------------

#[test]
fn ct52_downtime_gaps_follow_the_heartbeat_and_the_graceful_stop() {
    assert_eq!(
        K_GAP_MIN,
        SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL + K_CLOCK_DIVERGENCE,
        "one heartbeat period plus the clock divergence"
    );
    assert_eq!(K_GAP_MIN, Duration::from_secs(65));
    let (bed, _hub) = bed_with_stored_car();
    let store = &bed.store;
    let start = |start_ms: i64, heartbeat: Option<i64>| {
        store
            .begin_collector_run(&crate::db::CollectorRunStartRequest {
                mode: "legacy",
                config_fingerprint: DS_FP_A,
                start_ms,
                previous_heartbeat_ms: heartbeat,
                gap_min_ms: K_GAP_MIN_MS,
                gap_alert_ms: 600_000,
            })
            .expect("start")
    };
    let gaps = || store.collection_gaps(100).expect("gaps");
    // Heartbeat at w0, start at w0 + 20 min: a `hub_down` gap from the heartbeat.
    assert!(start(CORE_W0 + 20 * 60_000, Some(CORE_W0)).hub_down_inserted);
    let down = gaps();
    assert_eq!(
        (
            down[0].kind.as_str(),
            down[0].started_at_ms,
            down[0].ended_at_ms
        ),
        ("hub_down", CORE_W0, Some(CORE_W0 + 20 * 60_000))
    );
    assert!(down[0].alert, "twenty minutes is over the alert length");
    // Exactly `K_GAP_MIN` of silence is no gap; one millisecond more is.
    store
        .open()
        .unwrap()
        .execute("DELETE FROM collection_gaps", [])
        .unwrap();
    assert!(!start(CORE_W0 + K_GAP_MIN_MS, Some(CORE_W0)).hub_down_inserted);
    assert!(gaps().is_empty());
    assert!(start(CORE_W0 + K_GAP_MIN_MS + 1, Some(CORE_W0)).hub_down_inserted);
    assert_eq!(gaps().len(), 1);
    // A graceful stop at w1, then a start at w1 + 3 h: the `hub_stopped` gap is
    // closed at the start and no `hub_down` row is added for the same silence.
    store
        .open()
        .unwrap()
        .execute("DELETE FROM collection_gaps", [])
        .unwrap();
    let w1 = CORE_W0 + 3_600_000;
    store.end_collector_run(None, w1).expect("stop");
    let started = start(w1 + 3 * 3_600_000, Some(w1 - 5_000));
    assert!(started.stopped_gap_closed && !started.hub_down_inserted);
    let all = gaps();
    assert_eq!(all.len(), 1);
    assert_eq!(
        (
            all[0].kind.as_str(),
            all[0].started_at_ms,
            all[0].ended_at_ms
        ),
        ("hub_stopped", w1, Some(w1 + 3 * 3_600_000))
    );
}

#[test]
fn the_start_path_reads_the_fingerprint_inputs_and_loads_the_rows() {
    let dir = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(dir.path()).expect("store");
    let config = supervised_restart_test_config(dir.path());
    let cadence = config.collector.cadence().expect("cadence");
    let start = start_collector_run(
        &store,
        &config,
        &cadence,
        "wss://stream.example.test/",
        Some(CORE_W0),
    )
    .expect("start");
    assert_eq!(start.mode, crate::config::CollectorMode::Legacy);
    assert_eq!(start.snapshot.config_fingerprint.len(), 64);
    // The fingerprint binds the stream endpoint (a provider URL may carry a key,
    // so only the digest is kept) and the mode.
    let other = start_collector_run(&store, &config, &cadence, "wss://other.example.test/", None)
        .expect("start");
    assert_ne!(
        start.snapshot.config_fingerprint,
        other.snapshot.config_fingerprint
    );
    assert!(!start.snapshot.config_fingerprint.contains("stream.example"));
}

#[test]
fn the_configuration_fingerprint_binds_every_input_without_ambiguity() {
    use crate::config::CollectorMode::{FleetPolling, Legacy, Provider};
    let base = collector_config_fingerprint(Legacy, "https://a.test:443", "wss://s.test", "cred");
    assert_eq!(base.len(), 64);
    assert!(
        base.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(
        base,
        collector_config_fingerprint(Legacy, "https://a.test:443", "wss://s.test", "cred")
    );
    for changed in [
        collector_config_fingerprint(Provider, "https://a.test:443", "wss://s.test", "cred"),
        collector_config_fingerprint(FleetPolling, "https://a.test:443", "wss://s.test", "cred"),
        collector_config_fingerprint(Legacy, "https://b.test:443", "wss://s.test", "cred"),
        collector_config_fingerprint(Legacy, "https://a.test:443", "wss://t.test", "cred"),
        collector_config_fingerprint(Legacy, "https://a.test:443", "wss://s.test", "cred2"),
    ] {
        assert_ne!(changed, base);
    }
    // Moving a character across a field boundary changes the digest.
    assert_ne!(
        collector_config_fingerprint(Legacy, "ab", "c", ""),
        collector_config_fingerprint(Legacy, "a", "bc", "")
    );
}

// ---- CT53 -------------------------------------------------------------------

#[tokio::test]
async fn ct53_transitions_are_emitted_once() {
    let log = DecisionLog::start();
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            // Twenty identical asleep lists: one discovery observation, one
            // `reported_asleep` event and one scheduler row write.
            bed.list(vec![core_vehicle(1, "asleep")]).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            let (observation, _) = bed.discovery_row(hub).expect("discovery row");
            let row_written_at: i64 =
                bed.query("SELECT updated_at_ms FROM collector_vehicle_state");
            for _ in 0..19 {
                bed.advance(Duration::from_secs(30));
                bed.list(vec![core_vehicle(1, "asleep")]).await;
            }
            assert_eq!(bed.discovery_row(hub).map(|row| row.0), Some(observation));
            assert_eq!(seam.tick_commits(), 1, "only the first tick wrote");
            let row_now: i64 = bed.query("SELECT updated_at_ms FROM collector_vehicle_state");
            assert_eq!(
                row_now, row_written_at,
                "the scheduler row was written once"
            );
            assert_eq!(log.lines("INFO", "reported_asleep"), 1);
            // The one event carries every field R9 names: the Hub id (never a Tesla
            // id), the phases it moved between, the time to the car's next action and
            // the gate flag.
            let asleep = log
                .contents()
                .lines()
                .find(|line| line.contains(" INFO ") && line.contains("reported_asleep"))
                .map(str::to_owned)
                .expect("the transition event");
            assert!(asleep.contains("vehicle=unmapped"), "{asleep}");
            assert!(asleep.contains("from=\"unknown\""), "{asleep}");
            assert!(asleep.contains("to=\"asleep\""), "{asleep}");
            assert!(asleep.contains("gated=false"), "{asleep}");
            assert!(!asleep.contains("VIN0000"), "{asleep}");

            // Five identical 503s: one `rest_failure` transition, the rest at DEBUG.
            let error = CollectorError::OwnerApi(OwnerApiError::HttpStatus(503));
            for _ in 0..5 {
                bed.scheduler
                    .vehicle_failed_for_error(core_id(1), &error, bed.now());
                bed.advance(Duration::from_secs(60));
            }
            assert_eq!(log.lines("INFO", "rest_failure"), 1);
            assert_eq!(log.lines("DEBUG", "rest_failure"), 4);
            // The one event names the class and the status, nothing else of the body.
            let line = log
                .contents()
                .lines()
                .find(|line| line.contains(" INFO ") && line.contains("rest_failure"))
                .map(str::to_owned)
                .expect("event");
            assert!(line.contains("http_status=503"), "{line}");
            assert!(line.contains(&format!("vehicle={hub}")), "{line}");
            assert!(line.contains("next_in_ms="), "{line}");
            assert!(line.contains("class=Transient"), "{line}");
            assert!(line.contains("collector.decision"), "{line}");
        })
        .await;
}

// ---- CT54 -------------------------------------------------------------------

impl TickBed {
    /// One iteration of the loop as the Owner loop runs it: the due reads of this
    /// tick, committed. Returns the number of requests and the outcome.
    async fn iteration(&mut self) -> (usize, TickOutcome) {
        self.read_due().await
    }

    /// Advance to the loop's next iteration: the earliest due action, never less
    /// than the idle floor after an iteration without work (C3 R11).
    fn wait_for_next(&mut self, idle: bool) {
        let wait = self.scheduler.next_wait(self.now(), false, idle);
        self.advance(wait);
    }
}

#[tokio::test]
async fn ct54_a_lasting_store_fault_backs_the_account_off_and_rolls_back() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "online")]).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            // The first read stores a snapshot, then the car is Online with a read
            // due in a minute.
            assert_eq!(bed.iteration().await.0, 1);
            let observed: i64 =
                bed.query("SELECT last_observation_id FROM vehicle_lifecycle_state");
            let committed_before = seam.tick_commits();
            bed.advance(Duration::from_secs(60));
            assert_eq!(bed.scheduler.vehicles[&core_id(1)].phase, PollPhase::Online);

            // From here every tick commit fails, for ten minutes.
            seam.set_tick_commit_fault(true);
            let started = bed.now();
            let mut request_ticks: Vec<u64> = Vec::new();
            let mut last_read = None;
            while bed.now() - started <= Duration::from_secs(600) {
                let (requests, outcome) = bed.iteration().await;
                if requests > 0 {
                    request_ticks.push((bed.now() - started).as_secs());
                    last_read = bed.scheduler.vehicles[&core_id(1)].last_read;
                    assert!(matches!(outcome, TickOutcome::Failed), "{outcome:?}");
                    // In memory the phase is unchanged and the car's read is due
                    // again, while the fact that it was read stays.
                    let car = &bed.scheduler.vehicles[&core_id(1)];
                    assert_eq!(car.phase, PollPhase::Online);
                    assert!(car.next_read_due.is_some_and(|due| due <= bed.now()));
                    assert!(!car.in_flight);
                }
                bed.wait_for_next(requests == 0);
            }
            assert_eq!(
                request_ticks,
                vec![0, 5, 15, 35, 75, 155, 305, 455],
                "store waits of 5, 10, 20, 40, 80 and then 150 s"
            );
            assert!(last_read.is_some());
            assert_eq!(seam.tick_commits(), committed_before, "nothing committed");
            assert_eq!(bed.scheduler.account.store_failures, 8);
            assert_eq!(
                bed.scheduler.account.wait.map(|(_, reason)| reason),
                Some(WaitReason::Store)
            );
            // Nothing was stored for the failed ticks.
            assert_eq!(
                bed.query::<i64>("SELECT last_observation_id FROM vehicle_lifecycle_state"),
                observed
            );

            // The fault clears. The next tick (the wait of 150 s from 455 ends at
            // 605) stores one set of rows, and the store wait resets.
            seam.set_tick_commit_fault(false);
            let mut recovered = None;
            while recovered.is_none() {
                let (requests, outcome) = bed.iteration().await;
                if requests > 0 {
                    recovered = Some((bed.now() - started, outcome));
                }
                bed.wait_for_next(requests == 0);
            }
            let (when, outcome) = recovered.unwrap();
            assert_eq!(when, Duration::from_secs(605));
            assert!(matches!(
                outcome,
                TickOutcome::Committed { wrote: true, .. }
            ));
            assert_eq!(seam.tick_commits(), committed_before + 1);
            assert_eq!(bed.scheduler.account.store_failures, 0);
            assert_eq!(bed.scheduler.account.wait, None);
            assert_eq!(
                bed.query::<i64>("SELECT last_observation_id FROM vehicle_lifecycle_state"),
                observed + 1,
                "one observation, no duplicates from the failed ticks"
            );
            let _ = hub;
        })
        .await;
}

#[tokio::test]
async fn a_failed_commit_returns_the_tick_to_its_baseline_and_keeps_the_facts() {
    let log = DecisionLog::start();

    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.begin_tick();
    let id = core_id(1);
    assert_eq!(core.scheduler.due_vehicles(core.t0), vec![id]);
    // The tick reads the car and finds it driving.
    let snapshot = snapshot_with(&[("drive_state", "shift_state", json!("D"))]);
    core.scheduler
        .vehicle_succeeded(id, poll_phase(&snapshot), false, core.t0);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Driving);
    // A car first seen in the failed tick is forgotten with it.
    core.scheduler.begin_tick();
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")],
        core.t0,
    );
    let tick = core.scheduler.take_tick();
    core.scheduler.commit_failed(tick, core.t0);
    assert!(!core.scheduler.vehicles.contains_key(&core_id(2)));
    // The baseline is the state at the last `begin_tick`, taken after the read, so
    // the read's transition belongs to what was begun earlier and stands.
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Driving);
    assert_eq!(core.scheduler.account.store_failures, 1);
    assert_eq!(
        core.scheduler.account.wait,
        Some((core.t0 + Duration::from_secs(5), WaitReason::Store))
    );
    assert!(
        !core
            .scheduler
            .requests_allowed(core.t0 + Duration::from_secs(4))
    );
    assert!(
        core.scheduler
            .requests_allowed(core.t0 + Duration::from_secs(5))
    );
    assert_eq!(log.lines("INFO", "store_failure"), 1);
    // A second failure doubles the wait and logs at DEBUG only.
    core.scheduler.begin_tick();
    let tick = core.scheduler.take_tick();
    core.scheduler
        .commit_failed(tick, core.t0 + Duration::from_secs(5));
    assert_eq!(
        core.scheduler.account.wait,
        Some((core.t0 + Duration::from_secs(15), WaitReason::Store))
    );
    assert_eq!(log.lines("INFO", "store_failure"), 1);
    // The first success ends the back-off and logs the recovery once.
    let tick = core.scheduler.take_tick();
    core.scheduler
        .commit_succeeded(tick, core.t0 + Duration::from_secs(15));
    assert_eq!(core.scheduler.account.store_failures, 0);
    assert_eq!(core.scheduler.account.wait, None);
    assert_eq!(log.lines("INFO", "store_recovered"), 1);
}

#[test]
fn a_failed_commit_restores_every_transition_of_the_tick_but_not_its_requests() {
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    let id = core_id(1);
    core.scheduler.begin_tick();
    let due = core.scheduler.due_vehicles(core.t0);
    assert_eq!(due, vec![id]);
    let before = core.scheduler.vehicles[&id].clone();
    // The tick's results: a read that shows driving.
    let now = core.t0 + Duration::from_secs(1);
    let snapshot = snapshot_with(&[("drive_state", "shift_state", json!("D"))]);
    core.scheduler
        .vehicle_succeeded(id, poll_phase(&snapshot), false, now);
    assert_eq!(core.scheduler.vehicles[&id].phase, PollPhase::Driving);
    let tick = core.scheduler.take_tick();
    core.scheduler.commit_failed(tick, now);
    let after = &core.scheduler.vehicles[&id];
    assert_eq!(after.phase, before.phase, "the transition is dropped");
    assert_eq!(
        after.next_read_due, before.next_read_due,
        "the work stays due"
    );
    assert_eq!(after.last_success, before.last_success);
    assert_eq!(after.last_in_use, before.last_in_use);
    assert_eq!(
        after.last_read,
        Some(now),
        "the request was sent and stays counted"
    );
    assert!(!after.in_flight, "the request completed");
}

// ---- CT56 -------------------------------------------------------------------

#[tokio::test]
async fn ct56_the_decision_log_carries_the_hub_id_and_no_identifier_of_the_car() {
    let log = DecisionLog::start();
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            let mut vehicle = core_vehicle(1, "online");
            vehicle.display_name = Some("Garbage Lorry".to_owned());
            bed.list(vec![vehicle]).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            // A read with a position in the response.
            bed.advance(Duration::from_secs(1));
            let fields = {
                let mut fields = parked_fields();
                fields["drive_state"]["latitude"] = json!(47.123456);
                fields["drive_state"]["longitude"] = json!(19.654321);
                fields
            };
            let item = bed.read_item(1, fields);
            let mut batch = TickBatch::default();
            batch.snapshots.push(item);
            batch.note_result(bed.now());
            bed.begin_and_commit(batch).await;
            // A 503 with a hostile-looking body, an asleep transition, a failed commit
            // and a host sleep.
            bed.scheduler.vehicle_failed_for_error(
                core_id(1),
                &CollectorError::OwnerApi(OwnerApiError::HttpStatus(503)),
                bed.now(),
            );
            bed.advance(Duration::from_secs(30));
            bed.list(vec![core_vehicle(1, "asleep")]).await;
            seam.set_tick_commit_fault(true);
            bed.scheduler.begin_tick();
            bed.scheduler.account.state = AccountState::OwnerApiRefused;
            bed.commit(TickBatch::default()).await;
            seam.set_tick_commit_fault(false);
            bed.clock.advance_mono(Duration::from_secs(1));
            bed.clock.step_wall_ms(2 * 3_600_000);
            bed.scheduler.observe_clock(bed.now());

            let text = log.contents();
            assert!(text.contains(&hub.to_string()), "the Hub id is logged");
            for reason in [
                "first_contact_read",
                "reported_asleep",
                "rest_failure",
                "store_failure",
                "gap_opened",
            ] {
                assert!(text.contains(reason), "{reason}\n{text}");
            }
            for forbidden in [
                "VIN00000000000001",
                "Garbage Lorry",
                "source_vehicle_id",
                "47.123456",
                "19.654321",
                "Bearer",
                "https://",
                "/api/1/",
                "vehicle_data",
            ] {
                assert!(!text.contains(forbidden), "{forbidden} in the log\n{text}");
            }
        })
        .await;
}

impl TickBed {
    /// `begin_tick` and commit a prepared batch.
    async fn begin_and_commit(&mut self, batch: TickBatch) -> TickOutcome {
        self.scheduler.begin_tick();
        self.commit(batch).await
    }
}

// ---- CT64 -------------------------------------------------------------------

#[tokio::test]
async fn ct64_a_restart_keeps_a_rate_limit_wait_and_a_probe_deadline_but_no_failure_count() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "online"), core_vehicle(2, "online")])
                .await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            // A 429 with `Retry-After: 600` at w0: the account waits 600 s.
            let now = bed.now();
            bed.scheduler.begin_tick();
            bed.scheduler.account.wait =
                Some((now + Duration::from_secs(600), WaitReason::RateLimited));
            // Car 1 is Paused, its probe due in an hour; car 2 has failed five times.
            {
                let car = bed.scheduler.vehicles.get_mut(&core_id(1)).unwrap();
                car.hold = VehicleHold::Paused(PauseReason::PersistentFailure);
                car.reason = "paused_persistent_failure";
                car.next_probe_due = Some(now + Duration::from_secs(3_600));
                let other = bed.scheduler.vehicles.get_mut(&core_id(2)).unwrap();
                other.consecutive_failures = 5;
            }
            bed.commit(TickBatch::default()).await;
            let rows = bed.store.load_collector_state_rows().unwrap();
            assert_eq!(
                rows.account
                    .as_ref()
                    .and_then(|row| row.rate_limited_until_ms),
                Some(CORE_W0 + 600_000),
                "the row holds the wait as a wall deadline"
            );
            let paused = rows
                .vehicles
                .iter()
                .find(|row| row.vehicle_id == hub)
                .unwrap();
            assert_eq!(paused.hold, "paused");
            assert_eq!(paused.next_probe_ms, Some(CORE_W0 + 3_600_000));
            let other_hub = bed.scheduler.vehicles[&core_id(2)].hub_id.unwrap();

            // Drop at w0 + 10 s; reload at w0 + 20 s, the monotonic clock restarted.
            let configured = [
                (hub, 1, crate::hub_pack::ProjectionCarSettings::default()),
                (
                    other_hub,
                    2,
                    crate::hub_pack::ProjectionCarSettings::default(),
                ),
            ];
            let load_at = |wall_ms: i64| {
                let (mut scheduler, _clock, t) =
                    restarted_scheduler(crate::config::CollectorMode::Legacy, wall_ms);
                scheduler.load(
                    &CollectorStateSnapshot::from_rows(
                        bed.store.load_collector_state_rows().unwrap(),
                        DS_FP_A.to_owned(),
                        &configured,
                    ),
                    t,
                );
                scheduler.accept_discovery(
                    vec![core_vehicle(1, "online"), core_vehicle(2, "online")],
                    t,
                );
                (scheduler, t)
            };
            let (mut scheduler, t) = load_at(CORE_W0 + 20_000);
            // No request before w0 + 600 s: 580 s after the reload.
            assert!(!scheduler.requests_allowed(t));
            assert!(
                scheduler
                    .due_vehicles(t + Duration::from_secs(579))
                    .is_empty()
            );
            assert!(!scheduler.discovery_due(t + Duration::from_secs(579)));
            assert_eq!(
                scheduler.account.wait,
                Some((t + Duration::from_secs(580), WaitReason::RateLimited))
            );
            assert!(scheduler.requests_allowed(t + Duration::from_secs(580)));
            // The paused car's probe is not due at the restart, nor before an hour
            // after the pause: reloaded five minutes in, it is due in 55 minutes.
            let (scheduler, t) = load_at(CORE_W0 + 5 * 60_000);
            let car = &scheduler.vehicles[&core_id(1)];
            assert_eq!(
                car.hold,
                VehicleHold::Paused(PauseReason::PersistentFailure)
            );
            assert_eq!(car.next_probe_due, Some(t + Duration::from_secs(55 * 60)));
            // A failure count is not persisted: a restart begins a new sequence.
            assert_eq!(scheduler.vehicles[&core_id(2)].consecutive_failures, 0);
            assert_eq!(scheduler.vehicles[&core_id(2)].first_failure, None);
            // The pause's gap is open in the store, so the reload keeps it open.
            assert!(scheduler.open_gaps.contains_key(&GapKey {
                kind: GapKind::VehiclePaused,
                vehicle: Some(core_id(1)),
            }));
        })
        .await;
}

// ---- CT58 (the persistence half) ----------------------------------------------

#[test]
fn ct58_a_persisted_refusal_survives_a_restart_until_the_configuration_changes() {
    let (bed, hub) = bed_with_stored_car();
    write_collector_rows(
        &bed.store,
        &crate::db::CollectorRowsWrite {
            now_ms: CORE_W0,
            account: Some(crate::db::CollectorAccountRow {
                mode: "provider".to_owned(),
                config_fingerprint: DS_FP_A.to_owned(),
                state: "provider_refused".to_owned(),
                reason_code: "provider_gone".to_owned(),
                since_ms: CORE_W0,
                rate_limited_until_ms: None,
                next_probe_ms: None,
            }),
            gap_inserts: vec![crate::db::CollectorGapInsert {
                vehicle_id: None,
                kind: "provider_refused".to_owned(),
                started_at_ms: CORE_W0,
                ended_at_ms: None,
                alert: false,
            }],
            ..Default::default()
        },
    );
    // The same configuration: still refused, and no request is due.
    let (mut scheduler, _clock, t) =
        restarted_scheduler(crate::config::CollectorMode::Provider, CORE_W0 + 60_000);
    scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
    assert_eq!(scheduler.account.state, AccountState::ProviderRefused);
    assert_eq!(scheduler.account.state_reason, "provider_gone");
    assert!(!scheduler.discovery_due(t + Duration::from_secs(3_600)));
    assert!(scheduler.due_vehicles(t).is_empty());
    assert!(
        scheduler.take_tick().is_empty(),
        "the restart rewrites nothing"
    );
    // A different stream endpoint changes the fingerprint: the refusal resets to
    // Collecting (`config_changed`), and the next tick writes that and closes the
    // refusal's gap.
    let (mut scheduler, _clock, t) =
        restarted_scheduler(crate::config::CollectorMode::Provider, CORE_W0 + 60_000);
    scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_B), t);
    assert_eq!(scheduler.account.state, AccountState::Collecting);
    assert_eq!(scheduler.account.state_reason, "config_changed");
    assert!(scheduler.discovery_due(t));
    let tick = scheduler.take_tick();
    let row = tick
        .account
        .as_ref()
        .expect("the reset is written")
        .row
        .clone();
    assert_eq!(
        (
            row.state.as_str(),
            row.reason_code.as_str(),
            row.mode.as_str()
        ),
        ("collecting", "config_changed", "provider")
    );
    assert_eq!(row.config_fingerprint, DS_FP_B);
    assert_eq!(tick.gap_closes.len(), 1);
    assert_eq!(tick.gap_closes[0].key.kind, GapKind::ProviderRefused);
    // A mode change alone also resets (the other mode's row is not this mode's).
    let (mut scheduler, _clock, t) =
        restarted_scheduler(crate::config::CollectorMode::Legacy, CORE_W0 + 60_000);
    scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
    assert_eq!(scheduler.account.state, AccountState::Collecting);
}

// ---- CT69 -------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn ct69_a_result_waits_at_most_the_commit_age_for_its_transaction() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let dir = crate::private_tempdir().expect("temporary store");
            let store = HubStore::initialize(dir.path()).expect("store");
            let t0 = Instant::now();
            let mut scheduler = VehicleScheduler::new_with(
                core_cadence(),
                crate::config::CollectorMode::Legacy,
                core_cadence().fleet_policy(),
                t0,
                Arc::new(SystemCollectorClock),
                FixedJitter::constant(0.0),
            );
            let mut identities = IdentityCache::default();
            let cursor_key = CursorKey::from_bytes([0x69; 32]);
            let (terrain, _terrain_rx) = mpsc::channel(8);
            let cars = [1, 2, 3].map(|id| core_vehicle(id, "online"));
            scheduler.accept_discovery(cars.to_vec(), t0);
            for car in &cars {
                let registered = identities
                    .resolve(&store, CollectorProvider::Legacy, car, CORE_W0)
                    .expect("identity");
                scheduler.vehicles.get_mut(&car.id).unwrap().hub_id = Some(registered.hub_id);
            }
            let due = scheduler.due_vehicles(t0);
            assert_eq!(due, vec![core_id(1), core_id(2), core_id(3)]);
            let mut run = TickRun {
                scheduler: &mut scheduler,
                identities: &mut identities,
                commit: TickCommit {
                    store: &store,
                    cursor_key: &cursor_key,
                    provider: CollectorProvider::Legacy,
                    offline_drive_timeout: Duration::from_secs(600),
                },
                batch: TickBatch::default(),
                terrain_wake: &terrain,
                any_wrote: false,
            };
            // The fake answers cars 1 and 2 after 40 ms and car 3 after 5 s.
            let mut requests: Vec<TickFuture<'_>> = Vec::new();
            for (id, delay) in [(1u64, 40u64), (2, 40), (3, 5_000)] {
                requests.push(Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    (
                        TickKey::Read(core_id(id)),
                        Ok(TickPayload::Data(VehicleData::for_test(
                            id,
                            parked_fields(),
                        ))),
                    )
                }));
            }
            let flow = drive_requests(
                &mut run,
                requests,
                None,
                |run: &mut TickRun<'_>, key, result, now| {
                    let (TickKey::Read(id), Ok(TickPayload::Data(snapshot))) = (key, result) else {
                        panic!("a read result");
                    };
                    let vehicle = run.scheduler.vehicles[&id].vehicle.clone();
                    run.scheduler
                        .vehicle_succeeded(id, poll_phase(&snapshot), true, now);
                    run.batch.snapshots.push(SnapshotItem {
                        vehicle,
                        snapshot,
                        received_at_ms: CORE_W0,
                    });
                    Ok(ReadFlow::Continue)
                },
            )
            .await
            .expect("tick");
            assert_eq!(flow, ReadFlow::Continue);
            // The first transaction commits 100 ms after the first result (at 40 ms),
            // with the two early results; the second at 5 s with the third's.
            let times: Vec<Duration> = seam
                .tick_commit_times
                .lock()
                .unwrap()
                .iter()
                .map(|at| *at - t0)
                .collect();
            assert_eq!(
                times,
                vec![Duration::from_millis(140), Duration::from_millis(5_000)]
            );
            assert_eq!(seam.tick_commits(), 2);
            assert!(run.any_wrote);
            let stored: i64 = store
                .open()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM current_observations", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(stored, 3, "one current row per car's read");
        })
        .await;
}

// ---- CT70 -------------------------------------------------------------------

#[test]
fn ct70_a_gap_alerts_by_the_phase_at_its_start_or_by_its_length() {
    let run = |phase: &str, start_after_ms: i64| {
        let (bed, hub) = bed_with_stored_car();
        write_collector_rows(
            &bed.store,
            &crate::db::CollectorRowsWrite {
                now_ms: CORE_W0,
                vehicles: vec![stored_vehicle_row(hub, phase, "none", "collecting")],
                ..Default::default()
            },
        );
        bed.store
            .begin_collector_run(&crate::db::CollectorRunStartRequest {
                mode: "legacy",
                config_fingerprint: DS_FP_A,
                start_ms: CORE_W0 + start_after_ms,
                previous_heartbeat_ms: Some(CORE_W0),
                gap_min_ms: K_GAP_MIN_MS,
                gap_alert_ms: 600_000,
            })
            .expect("start");
        let gaps = bed.store.collection_gaps(10).expect("gaps");
        assert_eq!(gaps.len(), 1);
        (gaps[0].alert, bed)
    };
    // (a) Driving at the last stored transition: an alert whatever the length.
    assert!(run("driving", K_GAP_MIN_MS + 1_000).0);
    // (b) Every car asleep: no alert for a short gap, an alert from `T_gap` on.
    assert!(!run("asleep", K_GAP_MIN_MS + 1_000).0);
    assert!(run("asleep", 11 * 60_000).0);
    assert!(!run("asleep", 599_999).0);
    assert!(run("asleep", 600_000).0);
}

#[test]
fn ct70_readiness_detail_shows_an_unnotified_alert_until_it_is_acknowledged() {
    let (bed, hub) = bed_with_stored_car();
    write_collector_rows(
        &bed.store,
        &crate::db::CollectorRowsWrite {
            now_ms: CORE_W0,
            vehicles: vec![stored_vehicle_row(
                hub,
                "driving",
                "none",
                "use_evidence_snapshot",
            )],
            ..Default::default()
        },
    );
    bed.store
        .begin_collector_run(&crate::db::CollectorRunStartRequest {
            mode: "legacy",
            config_fingerprint: DS_FP_A,
            start_ms: CORE_W0 + K_GAP_MIN_MS + 1_000,
            previous_heartbeat_ms: Some(CORE_W0),
            gap_min_ms: K_GAP_MIN_MS,
            gap_alert_ms: 600_000,
        })
        .expect("start");
    // (c) The alert gap has no `notified_at_ms`: the detail names it and the Hub is
    // still healthy; after the acknowledgement it does not.
    assert_eq!(
        bed.store.collector_readiness_detail_at(CORE_W0).unwrap(),
        vec!["collector_gap_unnotified"]
    );
    assert_eq!(
        bed.store.collector_readiness_at(CORE_W0).unwrap().failure,
        None
    );
    assert_eq!(
        bed.store
            .acknowledge_collection_gaps(CORE_W0 + 100_000)
            .unwrap(),
        1
    );
    assert!(
        bed.store
            .collector_readiness_detail_at(CORE_W0)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn ct70_a_second_handle_reads_the_status_from_the_tables_after_a_commit() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")])
                .await;
            bed.advance(Duration::from_secs(1));
            // Car 1 is read and stays Online; the account is refused; car 2 is paused.
            bed.scheduler.begin_tick();
            bed.scheduler.account.state = AccountState::OwnerApiRefused;
            bed.scheduler.account.state_reason = "owner_api_refused";
            bed.scheduler.account.state_since_ms = bed.wall();
            {
                let paused = bed.scheduler.vehicles.get_mut(&core_id(2)).unwrap();
                paused.hold = VehicleHold::Paused(PauseReason::LegalPrivacy);
                paused.reason = "paused_legal_privacy";
                paused.next_probe_due = Some(bed.clock.mono_now() + Duration::from_secs(3_600));
            }
            let item = bed.read_item(1, parked_fields());
            let mut batch = TickBatch::default();
            batch.snapshots.push(item);
            batch.note_result(bed.now());
            bed.commit(batch).await;

            // A second store handle on the same directory sees what the status says.
            let reader = HubStore::initialize(bed._dir.path()).expect("second handle");
            let rows = reader.load_collector_state_rows().unwrap();
            let status = bed.scheduler.status(bed.now());
            let account = rows.account.expect("account row");
            assert_eq!(account.state, status.account_state.code());
            assert_eq!(account.reason_code, status.account_reason);
            assert_eq!(account.since_ms, status.account_since_ms);
            assert_eq!(account.mode, status.mode.code());
            assert_eq!(rows.vehicles.len(), 2);
            for vehicle in &status.vehicles {
                let row = rows
                    .vehicles
                    .iter()
                    .find(|row| Some(row.vehicle_id) == vehicle.hub_id)
                    .expect("a row per car");
                assert_eq!(row.phase, vehicle.phase.code());
                assert_eq!(row.hold, vehicle.hold.code());
                assert_eq!(row.reason_code, vehicle.reason);
                assert_eq!(row.phase_since_ms, vehicle.since_ms);
            }
            // The open gaps are the ones the status counts: the refusal and the pause.
            let open = reader.collection_gaps(10).unwrap();
            let mut kinds: Vec<&str> = open.iter().map(|gap| gap.kind.as_str()).collect();
            kinds.sort_unstable();
            assert_eq!(kinds, vec!["owner_api_refused", "vehicle_paused"]);
            assert_eq!(status.open_gaps, 2);
            // The readiness code comes from the account row, not from the lease.
            assert_eq!(
                reader.collector_readiness_at(bed.wall()).unwrap().failure,
                Some(crate::db::CollectionAttentionCode::CollectorApiRefused)
            );
        })
        .await;
}

// ---- Rules the part owns ------------------------------------------------------

#[test]
fn a_row_is_written_only_when_a_persisted_field_changes() {
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    let id = core_id(1);
    // A car whose Hub id is not known yet has no row to write, and the change stays
    // pending until it is named.
    assert!(core.scheduler.take_tick().vehicles.is_empty());
    core.scheduler.vehicles.get_mut(&id).unwrap().hub_id = Some(Uuid::new_v4());
    let tick = core.scheduler.take_tick();
    assert_eq!(tick.vehicles.len(), 1);
    let row = &tick.vehicles[0].row;
    assert_eq!(
        (
            row.phase.as_str(),
            row.hold.as_str(),
            row.reason_code.as_str()
        ),
        ("asleep", "none", "reported_asleep")
    );
    assert_eq!(row.phase_since_ms, CORE_W0);
    // Until the commit is reported the same row is offered again (a failed commit
    // retries it); after the report nothing is.
    assert_eq!(core.scheduler.take_tick().vehicles.len(), 1);
    core.scheduler.commit_succeeded(tick, core.t0);
    assert!(core.scheduler.take_tick().is_empty());
    // An unchanged list changes nothing; a changed phase changes one row.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.at(30));
    assert!(core.scheduler.take_tick().is_empty());
    core.clock.advance(Duration::from_secs(31));
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "offline")], core.at(31));
    let tick = core.scheduler.take_tick();
    assert_eq!(tick.vehicles.len(), 1);
    assert_eq!(tick.vehicles[0].row.phase, "offline");
    assert_eq!(tick.vehicles[0].row.phase_since_ms, CORE_W0 + 31_000);
}

#[test]
fn a_credential_stop_is_never_stored_as_an_account_state_but_opens_its_gap() {
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.vehicles.get_mut(&core_id(1)).unwrap().hub_id = Some(Uuid::new_v4());
    core.scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, true, core.t0);
    let tick = core.scheduler.take_tick();
    core.scheduler.commit_succeeded(tick, core.t0);
    core.scheduler.account.state = AccountState::CredentialStop;
    let tick = core.scheduler.take_tick();
    assert!(
        tick.account.is_none(),
        "derived from C5, never persisted by C3"
    );
    assert_eq!(tick.gap_opens.len(), 1);
    assert_eq!(tick.gap_opens[0].key.kind, GapKind::AuthTerminal);
    assert!(tick.gap_opens[0].alert, "the car is online");
    assert_eq!(
        tick.to_rows_write(CORE_W0).gap_inserts[0].kind,
        "auth_terminal"
    );
    core.scheduler.commit_succeeded(tick, core.t0);
    core.scheduler.account.state = AccountState::Collecting;
    let tick = core.scheduler.take_tick();
    assert_eq!(tick.gap_closes.len(), 1);
    assert_eq!(tick.gap_closes[0].key.kind, GapKind::AuthTerminal);
    assert_eq!(tick.gap_closes[0].alert_at_or_over_ms, 600_000);
}

#[test]
fn an_account_wait_becomes_a_gap_once_it_has_lasted_the_minimum_and_closes_at_the_first_success() {
    let mut core = Core::owner();
    let begin = core.t0;
    core.scheduler.account.consecutive_unreachable = 1;
    assert!(core.scheduler.take_tick().gap_opens.is_empty(), "not yet");
    core.clock.advance(Duration::from_millis(64_999));
    assert!(core.scheduler.take_tick().gap_opens.is_empty());
    core.clock.advance(Duration::from_millis(1));
    let tick = core.scheduler.take_tick();
    assert_eq!(tick.gap_opens.len(), 1);
    assert_eq!(tick.gap_opens[0].key.kind, GapKind::TeslaUnreachable);
    assert_eq!(
        tick.gap_opens[0].started_ms, CORE_W0,
        "from the wait's first failure"
    );
    assert!(!tick.gap_opens[0].alert, "no car was awake");
    core.scheduler.commit_succeeded(tick, begin);
    core.clock.advance(Duration::from_secs(120));
    core.scheduler.account.consecutive_unreachable = 0;
    let tick = core.scheduler.take_tick();
    assert_eq!(tick.gap_closes.len(), 1);
    assert_eq!(tick.gap_closes[0].ended_ms, CORE_W0 + 185_000);
    // A rate-limit episode is its own kind.
    core.scheduler.commit_succeeded(tick, begin);
    core.scheduler.account.consecutive_429 = 2;
    core.scheduler.take_tick();
    core.clock.advance(K_GAP_MIN);
    assert_eq!(
        core.scheduler.take_tick().gap_opens[0].key.kind,
        GapKind::RateLimited
    );
}

#[test]
fn a_gap_of_an_awake_car_alerts_at_its_start() {
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.vehicles.get_mut(&core_id(1)).unwrap().hub_id = Some(Uuid::new_v4());
    let snapshot = snapshot_with(&[("drive_state", "shift_state", json!("D"))]);
    core.scheduler
        .vehicle_succeeded(core_id(1), poll_phase(&snapshot), false, core.t0);
    {
        let car = core.scheduler.vehicles.get_mut(&core_id(1)).unwrap();
        car.hold = VehicleHold::Paused(PauseReason::AccessRefused);
        car.reason = "paused_access_refused";
    }
    let tick = core.scheduler.take_tick();
    let open = tick
        .gap_opens
        .iter()
        .find(|open| open.key.kind == GapKind::VehiclePaused)
        .expect("the pause's gap");
    assert_eq!(open.key.vehicle, Some(core_id(1)));
    assert!(open.hub_id.is_some());
    assert!(open.alert, "the car was driving when it was paused");
}

#[test]
fn the_identity_cache_registers_a_car_once_and_again_when_its_name_changes() {
    let dir = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(dir.path()).expect("store");
    let mut cache = IdentityCache::default();
    let mut car = core_vehicle(1, "online");
    let first = cache
        .resolve(&store, CollectorProvider::Legacy, &car, 1_000)
        .expect("identity");
    let seen = |store: &HubStore| -> i64 {
        store
            .open()
            .unwrap()
            .query_row("SELECT last_seen_at_ms FROM vehicles", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(seen(&store), 1_000);
    // An unchanged car is not registered again: nothing is written.
    let again = cache
        .resolve(&store, CollectorProvider::Legacy, &car, 9_000)
        .expect("identity");
    assert_eq!(again, first);
    assert_eq!(seen(&store), 1_000);
    // A changed display name registers again, with the same identity.
    car.display_name = Some("Renamed".to_owned());
    let renamed = cache
        .resolve(&store, CollectorProvider::Legacy, &car, 9_000)
        .expect("identity");
    assert_eq!(renamed, first);
    assert_eq!(seen(&store), 9_000);
}

#[test]
fn restored_holds_come_back_with_their_probe_deadlines_and_a_removed_car_stays_removed() {
    let (bed, hub) = bed_with_stored_car();
    let other = bed.store.register_vehicle(
        &VehicleDescriptor::new(
            bed.identities.entries[&core_id(1)].registered.source_id,
            "2",
        ),
        CORE_W0,
    );
    let other_hub = other.expect("second car").vehicle_id;
    let third = bed
        .store
        .register_vehicle(
            &VehicleDescriptor::new(
                bed.identities.entries[&core_id(1)].registered.source_id,
                "3",
            ),
            CORE_W0,
        )
        .expect("third car")
        .vehicle_id;
    let mut service = stored_vehicle_row(hub, "online", "service", "service_entered");
    service.next_probe_ms = Some(CORE_W0 + 15 * 60_000);
    let mut paused = stored_vehicle_row(
        other_hub,
        "unknown",
        "paused",
        "paused_mobile_access_disabled",
    );
    paused.next_probe_ms = Some(CORE_W0 + 60 * 60_000);
    let removed = stored_vehicle_row(third, "unknown", "removed", "vehicle_removed");
    write_collector_rows(
        &bed.store,
        &crate::db::CollectorRowsWrite {
            now_ms: CORE_W0,
            vehicles: vec![service, paused, removed],
            ..Default::default()
        },
    );
    let configured = [
        (hub, 1, crate::hub_pack::ProjectionCarSettings::default()),
        (
            other_hub,
            2,
            crate::hub_pack::ProjectionCarSettings::default(),
        ),
        (third, 3, crate::hub_pack::ProjectionCarSettings::default()),
    ];
    let (mut scheduler, _clock, t) =
        restarted_scheduler(crate::config::CollectorMode::Legacy, CORE_W0 + 5 * 60_000);
    scheduler.load(
        &CollectorStateSnapshot::from_rows(
            bed.store.load_collector_state_rows().unwrap(),
            DS_FP_A.to_owned(),
            &configured,
        ),
        t,
    );
    // The removed car is absent from the first list, so nothing schedules it.
    scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "online")],
        t,
    );
    assert!(
        !scheduler.vehicles.contains_key(&core_id(3)),
        "a removed car stays removed until a list names it"
    );
    let one = &scheduler.vehicles[&core_id(1)];
    assert_eq!(one.hold, VehicleHold::Service);
    assert_eq!(one.next_probe_due, Some(t + Duration::from_secs(10 * 60)));
    assert_eq!(
        one.phase,
        PollPhase::Unknown,
        "every other phase restores as Unknown"
    );
    let two = &scheduler.vehicles[&core_id(2)];
    assert_eq!(
        two.hold,
        VehicleHold::Paused(PauseReason::MobileAccessDisabled)
    );
    assert_eq!(two.next_probe_due, Some(t + Duration::from_secs(55 * 60)));
    // A held car is not read on a timer.
    assert!(
        scheduler
            .due_vehicles(t + Duration::from_secs(3_600))
            .is_empty()
    );
    // A later list that names the removed car brings it back (C3 R53): hold None,
    // phase Unknown, so its first-contact read follows.
    let later = t + Duration::from_secs(30);
    scheduler.accept_discovery(
        vec![
            core_vehicle(1, "online"),
            core_vehicle(2, "online"),
            core_vehicle(3, "online"),
        ],
        later,
    );
    let three = &scheduler.vehicles[&core_id(3)];
    assert_eq!(three.hold, VehicleHold::None);
    assert_eq!(three.phase, PollPhase::Unknown);
    assert_eq!(three.reason, "first_contact_read");
}

#[test]
fn the_account_gap_counts_are_read_by_the_loop_and_shown_in_the_status() {
    let mut core = Core::owner();
    assert_eq!(core.scheduler.status(core.t0).alert_gaps_unnotified, 0);
    core.scheduler.set_alert_gap_count(2);
    let status = core.scheduler.status(core.t0);
    assert_eq!(status.alert_gaps_unnotified, 2);
    assert_eq!(status.open_gaps, 0);
    // A closed gap of the clock watch is not an open one; a stored open gap is.
    core.scheduler.pending.gaps.push(GapRecord {
        kind: GapKind::HostSuspended,
        vehicle: None,
        started_ms: CORE_W0,
        ended_ms: Some(CORE_W0 + 1),
        alert: true,
    });
    assert_eq!(core.scheduler.status(core.t0).open_gaps, 0);
    core.scheduler.open_gaps.insert(
        GapKey {
            kind: GapKind::AccountLocked,
            vehicle: None,
        },
        OpenGap {
            started_ms: CORE_W0,
            alert: false,
            hub_id: None,
        },
    );
    assert_eq!(core.scheduler.status(core.t0).open_gaps, 1);
}

#[test]
fn the_status_publication_carries_the_latest_value() {
    // The publication is one process-wide value, so retry if another test
    // publishes between the send and the read.
    let core = Core::owner();
    let receiver = collector_status_receiver();
    let mut status = core.scheduler.status(core.t0);
    status.month = "K3-PUBLICATION".to_owned();
    let mut seen = false;
    for _ in 0..200 {
        publish_collector_status(status.clone());
        if receiver
            .borrow()
            .as_ref()
            .is_some_and(|published| published.month == "K3-PUBLICATION")
        {
            seen = true;
            break;
        }
    }
    assert!(seen);
}

// ---- The supervised loop, end to end ------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_loop_stores_a_sleeping_car_once_records_its_stop_and_stays_quiet_after_a_restart() {
    use crate::{
        credentials::{LegacyAuthManager, OwnerTokens},
        fake_tesla::{AdvanceMode, FakeTeslaSource, ScenarioStep},
        owner_api::OwnerApi,
    };

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);
    crate::teslamate_credentials::replace_key(temporary.path(), b"chain-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial = OwnerTokens::from_secret_parts("k3-access".to_owned(), "k3-refresh".to_owned())
        .expect("initial pair");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("Cloak pair");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, 2, 1)
                .expect("due refresh schedule"),
        )
        .expect("store pair");
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    fake.set_step(ScenarioStep::AsleepDiscovery);
    let config = supervised_restart_test_config(temporary.path());

    let run_once = |stopping_after_discovery: bool| {
        let store = store.clone();
        let config = config.clone();
        let fake_url = fake.http_base_url().clone();
        let issuer = fake.oauth_issuer_url();
        let stream_endpoint = fake.stream_endpoint().to_owned();
        let data_dir = temporary.path().to_path_buf();
        async move {
            let manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
                store.clone(),
                &data_dir,
                issuer,
            )
            .expect("load pair");
            let region = manager.region();
            let auth = CollectionAuth::Legacy {
                manager: Arc::new(tokio::sync::Mutex::new(manager)),
                fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
                refresh: Arc::new(LegacyRefreshCoordinator::default()),
                allow_refresh: true,
                region,
            };
            let client = OwnerApi::for_fake_http(fake_url, Duration::from_secs(2)).expect("client");
            let (ready_tx, ready_rx) = oneshot::channel();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let task_store = store.clone();
            let mut task = tokio::spawn(async move {
                run_supervised_with_access(
                    &task_store,
                    &config,
                    supervised_restart_test_cadence(),
                    client,
                    auth,
                    stream_endpoint,
                    CursorKey::from_bytes([0xC3; 32]),
                    Some(ready_tx),
                    RunWriteGuard::for_run(None),
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
            });
            let _cursor =
                wait_for_supervised_signal_or_abort("collector readiness", &mut task, ready_rx)
                    .await;
            if stopping_after_discovery {
                wait_for_supervised_restart_condition("the discovery row", || {
                    store
                        .open()
                        .expect("catalogue")
                        .query_row(
                            "SELECT COUNT(*) FROM collector_vehicle_state WHERE phase = 'asleep'",
                            [],
                            |row| row.get::<_, i64>(0),
                        )
                        .expect("count")
                        == 1
                })
                .await;
            } else {
                // A restart: give the loop several lists, then stop it.
                tokio::time::sleep(Duration::from_millis(2_500)).await;
            }
            shutdown_tx.send(()).expect("shutdown");
            join_supervised_restart_task("collector join", &mut task)
                .await
                .expect("collector result");
        }
    };

    run_once(true).await;
    let connection = store.open().expect("catalogue");
    let discovery_id: i64 = connection
        .query_row(
            "SELECT observation_id FROM current_observations
              WHERE record_type = 'owner_api_discovery_v1'",
            [],
            |row| row.get(0),
        )
        .expect("one discovery row");
    let vehicle_row: (String, String, String) = connection
        .query_row(
            "SELECT phase, hold, reason_code FROM collector_vehicle_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("one scheduler row");
    assert_eq!(
        vehicle_row,
        (
            "asleep".to_owned(),
            "none".to_owned(),
            "reported_asleep".to_owned()
        )
    );
    // The graceful stop left its gap open; the lease was released.
    let gaps = store.collection_gaps(10).expect("gaps");
    assert_eq!(
        gaps.iter()
            .filter(|gap| gap.kind == "hub_stopped" && gap.ended_at_ms.is_none())
            .count(),
        1
    );
    assert!(
        store
            .supervised_collector_lease_status()
            .expect("lease")
            .is_none()
    );

    // A restart closes the stop gap, records no downtime, and a car that still
    // sleeps causes no discovery write (its lifecycle already holds `asleep`).
    run_once(false).await;
    let stopped: Vec<_> = store
        .collection_gaps(10)
        .expect("gaps")
        .into_iter()
        .filter(|gap| gap.kind == "hub_stopped")
        .collect();
    assert_eq!(
        stopped.len(),
        2,
        "one per run; the first was closed by the second start"
    );
    assert!(stopped[0].ended_at_ms.is_some());
    assert!(
        !store
            .collection_gaps(10)
            .expect("gaps")
            .iter()
            .any(|gap| gap.kind == "hub_down")
    );
    let again: i64 = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT observation_id FROM current_observations
              WHERE record_type = 'owner_api_discovery_v1'",
            [],
            |row| row.get(0),
        )
        .expect("discovery row");
    assert_eq!(again, discovery_id, "no second discovery was stored");
}

// ---- Review round: R60 on the live path, R6/R7 faults, R7 fingerprint ------------------------

#[tokio::test]
async fn r60_an_unresolved_row_ends_resolved_at_the_hub_default_after_one_tick() {
    // An earlier Hub left `{suspend_min: 21, resolved: 0}`. The live path used to
    // upsert the scheduler's copy with the flag forced true before the resolution
    // ran, which kept the row's own value for good (here the scheduler's 21); the
    // resolution now runs first and the row ends at the Hub default, 30, resolved.
    let unresolved = |bed: &TickBed, hub: Uuid| {
        bed.store
            .open()
            .unwrap()
            .execute(
                "UPDATE car_settings SET suspend_min = 21, suspend_min_resolved = 0
                  WHERE vehicle_id = ?1",
                [hub.to_string()],
            )
            .unwrap();
    };
    let settings = |bed: &TickBed, hub: Uuid| -> (i64, i64) {
        bed.store
            .open()
            .unwrap()
            .query_row(
                "SELECT suspend_min, suspend_min_resolved FROM car_settings WHERE vehicle_id = ?1",
                [hub.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    };
    let mut bed = TickBed::owner();
    let mut car = core_vehicle(1, "online");
    car.settings.suspend_min = 21;
    bed.list(vec![car.clone()]).await;
    let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
    // The discovery path: a state change makes the list a delivered discovery.
    unresolved(&bed, hub);
    assert_eq!(settings(&bed, hub), (21, 0));
    let mut asleep = car.clone();
    asleep.state = "asleep".to_owned();
    bed.list(vec![asleep]).await;
    assert_eq!(settings(&bed, hub), (30, 1), "discovery path");
    // The read path.
    unresolved(&bed, hub);
    bed.scheduler.begin_tick();
    let item = SnapshotItem {
        vehicle: car,
        snapshot: VehicleData::for_test(1, parked_fields()),
        received_at_ms: bed.wall(),
    };
    let now = bed.now();
    let batch = TickBatch {
        snapshots: vec![item],
        first_result_at: Some(now),
        ..TickBatch::default()
    };
    bed.commit(batch).await;
    assert_eq!(settings(&bed, hub), (30, 1), "read path");
    // An owner-set resolved value is never changed.
    bed.store
        .open()
        .unwrap()
        .execute(
            "UPDATE car_settings SET suspend_min = 45, suspend_min_resolved = 1
              WHERE vehicle_id = ?1",
            [hub.to_string()],
        )
        .unwrap();
    let mut online = core_vehicle(1, "online");
    online.settings.suspend_min = 21;
    bed.list(vec![online]).await;
    assert_eq!(settings(&bed, hub).0, 45);
}

#[test]
fn a_failed_first_commit_keeps_the_restored_hold_for_the_next_list() {
    // R6, R7: the first list of a restarted process creates the car's record and
    // applies its stored hold. If that tick's commit fails the record is forgotten
    // with the rest of the tick, and the stored hold must not go with it: the next
    // list restores it again (a first-contact read to a paused car is what R7 forbids).
    let (bed, hub) = bed_with_stored_car();
    let mut paused = stored_vehicle_row(hub, "unknown", "paused", "paused_mobile_access_disabled");
    paused.next_probe_ms = Some(CORE_W0 + 60 * 60_000);
    write_collector_rows(
        &bed.store,
        &crate::db::CollectorRowsWrite {
            now_ms: CORE_W0,
            vehicles: vec![paused],
            ..Default::default()
        },
    );
    let (mut scheduler, _clock, t) =
        restarted_scheduler(crate::config::CollectorMode::Legacy, CORE_W0 + 5 * 60_000);
    scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
    scheduler.begin_tick();
    scheduler.accept_discovery(vec![core_vehicle(1, "online")], t);
    assert_eq!(
        scheduler.vehicles[&core_id(1)].hold,
        VehicleHold::Paused(PauseReason::MobileAccessDisabled)
    );
    // The commit fails: the transitions are dropped.
    let tick = scheduler.take_tick();
    scheduler.commit_failed(tick, t);
    // The next list brings the hold back, and no read is due for the paused car.
    let later = t + Duration::from_secs(10);
    scheduler.accept_discovery(vec![core_vehicle(1, "online")], later);
    assert_eq!(
        scheduler.vehicles[&core_id(1)].hold,
        VehicleHold::Paused(PauseReason::MobileAccessDisabled),
        "the stored hold survived the failed commit"
    );
    assert!(scheduler.due_vehicles(later).is_empty());
    // A commit that succeeds finally makes the stored row the scheduler's own.
    let tick = scheduler.take_tick();
    scheduler.commit_succeeded(tick, later);
    assert!(scheduler.restored.is_empty());
}

#[tokio::test]
async fn r6_a_store_fault_before_the_transaction_is_a_failed_tick_not_a_dead_loop() {
    // The identity registration is a write transaction of its own; a store fault
    // there (here a VIN the store refuses to rebind) must take the scheduler's
    // store back-off like a failed tick transaction, not end the loop with `?`.
    let mut bed = TickBed::owner();
    bed.list(vec![core_vehicle(1, "online")]).await;
    // A second identity cache has not seen the car; the VIN it presents conflicts
    // with the stored one.
    bed.identities = IdentityCache::default();
    let mut conflicting = core_vehicle(1, "asleep");
    conflicting.vin = "VIN99999999999999".to_owned();
    let now = bed.now();
    bed.scheduler.begin_tick();
    bed.scheduler
        .apply_owner_actions(
            &[OwnerActionRow {
                id: 7,
                kind: OwnerActionKind::RetryCollection,
                vehicle: None,
            }],
            now,
        );
    let batch = TickBatch {
        discoveries: vec![DiscoveryItem {
            vehicle: conflicting,
            received_at_ms: bed.wall(),
        }],
        first_result_at: Some(now),
        ..TickBatch::default()
    };
    let outcome = bed.commit(batch).await;
    assert!(matches!(outcome, TickOutcome::Failed), "{outcome:?}");
    assert!(
        matches!(bed.scheduler.account.wait, Some((_, WaitReason::Store))),
        "the account waits out the store fault"
    );
}

#[test]
fn r7_a_new_fleet_application_or_region_changes_the_fingerprint_and_resets_a_stored_refusal() {
    // The fingerprint's credential identity on the Fleet wire is the stored
    // application id and region (a refreshed token for the same application keeps
    // it, so it does not reset a refusal). The Owner wire contributes nothing until
    // the credential unit supplies its generation (reported).
    let dir = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(dir.path()).expect("store");
    let mut config = supervised_restart_test_config(dir.path());
    config.collector.provider = crate::config::CollectorProvider::Fleet;
    let cadence = config.collector.cadence().expect("cadence");
    let store_credentials = |client: &str, region: crate::fleet_api::FleetRegion| {
        let credentials = FleetSetupCredentials::new(
            CT67_SCOPED_ACCESS.to_owned(),
            "fleet-refresh".to_owned(),
            client.to_owned(),
            region,
            28_800,
        )
        .expect("setup credentials");
        crate::fleet_credentials::persist_fleet_setup_credentials(
            &store,
            dir.path(),
            &credentials,
            SystemTime::now(),
        )
        .expect("persist credentials");
    };
    let fingerprint = || {
        start_collector_run(&store, &config, &cadence, "", None)
            .expect("start")
            .snapshot
            .config_fingerprint
    };
    store_credentials("fleet-client-a", crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica);
    let first = fingerprint();
    // A refusal stored under this fingerprint.
    write_collector_rows(
        &store,
        &crate::db::CollectorRowsWrite {
            now_ms: CORE_W0,
            account: Some(crate::db::CollectorAccountRow {
                mode: "fleet_polling".to_owned(),
                config_fingerprint: first.clone(),
                state: "fleet_access_refused".to_owned(),
                reason_code: "fleet_access_refused".to_owned(),
                since_ms: CORE_W0,
                rate_limited_until_ms: None,
                next_probe_ms: None,
            }),
            ..Default::default()
        },
    );
    // The same application again (new tokens): the fingerprint stands and so does the
    // refusal.
    store_credentials("fleet-client-a", crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica);
    assert_eq!(fingerprint(), first);
    let state = |store: &HubStore| {
        store
            .load_collector_state_rows()
            .unwrap()
            .account
            .map(|account| account.state)
    };
    assert_eq!(state(&store).as_deref(), Some("fleet_access_refused"));
    // Another application: the fingerprint changes and the next start resets the row.
    store_credentials("fleet-client-b", crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica);
    let second = fingerprint();
    assert_ne!(second, first);
    assert_eq!(state(&store).as_deref(), Some("collecting"));
    // Another region of the same application: a third fingerprint.
    store_credentials("fleet-client-b", crate::fleet_api::FleetRegion::NorthAmericaAndAsiaPacific);
    assert_ne!(fingerprint(), second);
}

#[tokio::test]
async fn r10_a_restart_rewrites_no_row_for_a_car_no_list_has_named_yet() {
    // The first settings refresh gives every configured car a record (R3), but a car
    // that has seen nothing this run changes nothing: its stored row (here asleep)
    // stands until the list that names it, so a restart is not a write per car.
    let mut bed = TickBed::owner();
    bed.list(vec![core_vehicle(1, "asleep")]).await;
    let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
    assert_eq!(
        bed.store.load_collector_state_rows().unwrap().vehicles.len(),
        1,
        "the first list stored the car's row"
    );
    let (mut restarted, _clock, t) =
        restarted_scheduler(crate::config::CollectorMode::Legacy, CORE_W0 + 60_000);
    let configured = [(hub, 1, crate::hub_pack::ProjectionCarSettings::default())];
    restarted.load(
        &CollectorStateSnapshot::from_rows(
            bed.store.load_collector_state_rows().unwrap(),
            DS_FP_A.to_owned(),
            &configured,
        ),
        t,
    );
    restarted.apply_control_settings(&configured, t);
    assert!(restarted.vehicles.contains_key(&core_id(1)));
    let tick = restarted.take_tick();
    assert!(tick.vehicles.is_empty(), "{:?}", tick.vehicles);
}

// ---- P2 (B2 fix round): the derived Fleet credential stop is never durable ---------------------------

#[tokio::test]
async fn b2_a_fleet_restart_lifts_only_the_derived_credential_stop() {
    // A Fleet credential stop is derived in memory (C3 R47, B2 clarification 3): it is
    // committed with its gap but never stored as the account's state, and a restart
    // does not restore it. Everything that is durable survives the restart: the
    // month's counts and reservations, the account's rate-limit wait and the cap.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed =
                FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
            bed.tick().await;
            assert_eq!((bed.lists(), bed.reads()), (1, 1));
            // A durable rate-limit wait, committed in a tick of its own: the account is
            // `collecting` in the store, with the wait.
            let now = bed.clock.mono_now();
            bed.scheduler.begin_tick();
            bed.scheduler.rate_limited(1_800, now);
            let wait_until = bed.scheduler.account.wait.expect("a rate-limit wait").0;
            let outcome = bed.commit(TickBatch::default()).await;
            assert!(matches!(outcome, TickOutcome::Committed { wrote: true, .. }), "{outcome:?}");
            assert_eq!(bed.stored_account_state().as_deref(), Some("collecting"));
            // The derived stop is committed, with its gap, in a tick of its own.
            bed.scheduler.begin_tick();
            bed.scheduler.enter_credential_stop(now);
            let outcome = bed.commit(TickBatch::default()).await;
            assert!(matches!(outcome, TickOutcome::Committed { wrote: true, .. }), "{outcome:?}");
            assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
            assert!(
                bed.store
                    .collection_gaps(10)
                    .expect("gaps")
                    .iter()
                    .any(|gap| gap.kind == "auth_terminal" && gap.ended_at_ms.is_none()),
                "the stop's gap is stored"
            );
            assert_eq!(
                bed.stored_account_state().as_deref(),
                Some("collecting"),
                "the stop itself is not stored"
            );
            let before = bed.scheduler.request_ledger().rows();
            let (counts_before, _) = bed.data_counts();
            // A restart: the stop is lifted, the durable facts are not.
            bed.reload();
            assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
            assert_ne!(bed.scheduler.account.state_reason, "auth_terminal");
            assert_eq!(
                bed.scheduler.account.wait.map(|(_, reason)| reason),
                Some(WaitReason::RateLimited),
                "the rate-limit wait is durable"
            );
            let after = bed.scheduler.request_ledger();
            let (counts_after, _) = ledger_data_counts(&after);
            assert_eq!(counts_after.sent, counts_before.sent, "the counts are preserved");
            assert_eq!(counts_after.billable, counts_before.billable);
            assert_eq!(
                counts_after.reserved_persisted, counts_before.reserved_persisted,
                "the reservation is preserved"
            );
            assert!(
                after.rows().iter().all(|row| before.contains(row)),
                "nothing is converted or reset by the restart"
            );
            assert_eq!(
                bed.scheduler
                    .status(bed.clock.mono_now())
                    .estimate
                    .expect("a Fleet estimate")
                    .cap_cents,
                1_000,
                "the cap"
            );
            // Ordinary discovery returns once the wait is over: the list is sent
            // again, and the stop's old gap is closed.
            let lists_before = bed.lists();
            bed.advance(wait_until.saturating_duration_since(bed.clock.mono_now()) + Duration::from_secs(1));
            bed.tick().await;
            assert_eq!(bed.lists(), lists_before + 1, "discovery returns");
            assert!(
                bed.store
                    .collection_gaps(10)
                    .expect("gaps")
                    .iter()
                    .all(|gap| gap.kind != "auth_terminal" || gap.ended_at_ms.is_some()),
                "the derived stop's gap is closed"
            );
        })
        .await;
}
