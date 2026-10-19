// SPDX-License-Identifier: AGPL-3.0-only

// The B2 fix round's tests of the stream-evidence unit (C3 R9, R28, R36, R40): the
// append-time `no_data` decision, the audit journal and what a stopped run keeps.
// They share the beds of `stream_evidence.rs`. Synthetic data only.

// ---- R40 (B2 clarification 3): a `no_data` event is decided inside its transaction ------------------

impl TickBed {
    /// The decision times the committed transactions stamped on their `no_data`
    /// events (observed and received time are the same instant), oldest first.
    fn no_data_rows(&self) -> Vec<i64> {
        SUPERVISED_COLLECTOR_TEST_SEAM
            .try_with(|seam| seam.no_data_stored())
            .expect("inside the test seam")
    }

    /// Plan the `no_data` candidates of one tick now, without committing them.
    fn plan_no_data_batch(&mut self) -> TickBatch {
        let now = self.now();
        self.scheduler.begin_tick();
        let mut batch = TickBatch::default();
        plan_no_data_events(&self.store, &mut self.scheduler, &mut batch).expect("plan");
        if !batch.hub_events.is_empty() {
            batch.first_result_at = Some(now);
        }
        batch
    }

    /// A parked read result of car 1, received at the bed's wall time.
    fn fresh_read_item(&self, timestamp_ms: i64) -> SnapshotItem {
        SnapshotItem {
            vehicle: self.scheduler.vehicles[&core_id(1)].vehicle.clone(),
            snapshot: VehicleData::for_test(
                1,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 20,
                        "latitude": 47.52,
                        "longitude": 19.0,
                        "timestamp": timestamp_ms
                    },
                    "vehicle_state": {"odometer": 1000.2}
                }),
            ),
            received_at_ms: self.wall(),
        }
    }

    fn drive_open(&self, hub: Uuid) -> bool {
        self.store
            .collector_lifecycle_snapshot(hub)
            .expect("snapshot")
            .drive_open
    }
}

#[tokio::test]
async fn r9_the_manual_collection_failure_log_names_no_tesla_id() {
    // The failure of a manual collection is logged with its class, and never with
    // the Tesla identifier of the car or a field that would name one (C3 R9).
    let log = DecisionLog::start();
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([0x31; 32]);
    let collection = ManualCollection {
        vehicles: Vec::new(),
        snapshots: Vec::new(),
        failures: vec![VehicleCollectionFailure {
            vehicle_id: VehicleId::from_test(9_876_543_210),
            error: OwnerApiError::HttpStatus(503),
        }],
    };
    // The collection itself may end as it likes; the log line is the subject.
    let _ = finish_collection(&store, &cursor_key, &collection).await;
    let text = log.contents();
    let warnings: Vec<&str> = text
        .lines()
        .filter(|line| line.contains(" WARN ") && line.contains("owner API vehicle collection failed"))
        .collect();
    assert_eq!(warnings.len(), 1, "{text}");
    assert!(!warnings[0].contains("9876543210"), "{}", warnings[0]);
    assert!(!warnings[0].contains("vehicle_id"), "{}", warnings[0]);
    assert!(warnings[0].contains("error="), "the class is still named: {}", warnings[0]);
}

#[tokio::test]
async fn ct31b_a_no_data_candidate_does_not_close_a_drive_resumed_by_a_stored_stream_frame() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let id = core_id(1);
            let w0 = CORE_W0;
            // The candidate is planned an instant after `T_gap` without evidence.
            bed.advance(Duration::from_millis(600_001));
            let batch = bed.plan_no_data_batch();
            assert_eq!(batch.hub_events.len(), 1, "a candidate is planned");
            // A fresh Driving frame is stored by the stream path before the candidate's
            // transaction runs: the car resumed, and the scheduler's cache knows nothing
            // of it yet.
            assert!(
                persist_stream_update(
                    &bed.store,
                    id,
                    &frame(w0 + 300_000, Some("D"), Some(20), Some(12)),
                )
                .expect("the frame"),
                "the frame is stored"
            );
            let receipt: i64 = bed.query(
                "SELECT MAX(received_at_ms) FROM current_observations
                  WHERE record_type = 'tesla_stream_update_v1'",
            );
            assert_eq!(bed.scheduler.vehicles[&id].last_stored_obs_ms, Some(w0));
            let observation = bed.last_observation_id(hub);
            // The decision is aligned to the frame's actual receipt, just after it.
            bed.advance(Duration::from_millis(
                u64::try_from(receipt + 100 - bed.wall()).expect("forward"),
            ));
            bed.commit(batch).await;
            assert!(bed.no_data_rows().is_empty(), "no event");
            assert_eq!(bed.last_observation_id(hub), observation, "nothing closed the drive");
            assert!(bed.drive_open(hub));
            assert!(!bed.scheduler.vehicles[&id].force_closed_at_gap, "no flag");
            // The evidence is the frame's receipt: exactly `T_gap` after it there is
            // still no event, and one millisecond later there is exactly one.
            bed.advance(Duration::from_millis(600_000 - 100));
            assert_eq!(bed.wall(), receipt + 600_000);
            let batch = bed.plan_no_data_batch();
            bed.commit(batch).await;
            assert!(bed.no_data_rows().is_empty());
            bed.advance(Duration::from_millis(1));
            let batch = bed.plan_no_data_batch();
            assert_eq!(batch.hub_events.len(), 1);
            bed.commit(batch).await;
            assert_eq!(bed.no_data_rows(), vec![receipt + 600_001]);
            assert!(bed.scheduler.vehicles[&id].force_closed_at_gap);
            assert!(!bed.drive_open(hub));
        })
        .await;
}

async fn ct31c_case(mode: crate::config::CollectorMode) {
    let mut bed = TickBed::new(mode);
    let hub = bed.store_open_drive().await;
    let id = core_id(1);
    bed.advance(Duration::from_millis(600_001));
    let mut batch = bed.plan_no_data_batch();
    assert_eq!(batch.hub_events.len(), 1, "a candidate is planned");
    // A read of the same tick is received before the transaction runs.
    let item = bed.fresh_read_item(bed.wall());
    let received = item.received_at_ms;
    batch.snapshots.push(item);
    bed.commit(batch).await;
    assert!(bed.no_data_rows().is_empty(), "the read is evidence: no event");
    assert!(!bed.scheduler.vehicles[&id].force_closed_at_gap, "no flag");
    // The read's own lifecycle effects remain: it is stored, and the car's evidence is
    // its receipt.
    assert_eq!(bed.scheduler.vehicles[&id].last_stored_obs_ms, Some(received));
    assert!(bed.drive_open(hub), "the read opened or continued a drive");
}

#[tokio::test]
async fn ct31c_a_same_transaction_read_invalidates_a_no_data_candidate() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            ct31c_case(crate::config::CollectorMode::Legacy).await;
            ct31c_case(crate::config::CollectorMode::FleetPolling).await;
        })
        .await;
}

#[tokio::test]
async fn ct31d_a_same_transaction_delivered_discovery_invalidates_a_no_data_candidate() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            // A state that gives the lifecycle no reachability is delivered when it
            // differs from the current discovery row: a fresh Deliver discovery.
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let id = core_id(1);
            bed.advance(Duration::from_millis(600_001));
            let mut batch = bed.plan_no_data_batch();
            assert_eq!(batch.hub_events.len(), 1);
            let mut vehicle = bed.scheduler.vehicles[&id].vehicle.clone();
            vehicle.state = "waking".to_owned();
            // Received exactly `T_gap` after the drive's last fold, one millisecond
            // before the decision: stale enough for nothing but its own receipt to
            // matter, and not late enough to end the drive by the gap rule.
            let received = bed.wall() - 1;
            batch.discoveries.push(DiscoveryItem {
                vehicle,
                received_at_ms: received,
            });
            bed.commit(batch).await;
            assert!(bed.no_data_rows().is_empty(), "the discovery is evidence");
            assert!(!bed.scheduler.vehicles[&id].force_closed_at_gap);
            assert_eq!(bed.scheduler.vehicles[&id].last_stored_obs_ms, Some(received));
            assert!(bed.drive_open(hub), "the drive was neither ended by the gap rule nor closed");

            // A discovery that only writes the car's current row is no evidence.
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            bed.store
                .open()
                .expect("connection")
                .execute(
                    "DELETE FROM current_observations WHERE record_type = 'owner_api_discovery_v1'",
                    [],
                )
                .expect("the car has no current discovery row");
            let w0 = CORE_W0;
            bed.advance(Duration::from_millis(600_001));
            let mut batch = bed.plan_no_data_batch();
            assert_eq!(batch.hub_events.len(), 1);
            let mut vehicle = bed.scheduler.vehicles[&id].vehicle.clone();
            vehicle.state = "online".to_owned();
            // The same state as the lifecycle's last reachability, and no current row
            // yet: the action is `WriteCurrentOnly`.
            assert_eq!(
                crate::db::discovery_action("online", Some("online"), None),
                crate::db::DiscoveryAction::WriteCurrentOnly
            );
            batch.discoveries.push(DiscoveryItem {
                vehicle,
                received_at_ms: bed.wall(),
            });
            bed.commit(batch).await;
            assert_eq!(
                bed.no_data_rows(),
                vec![w0 + 600_001],
                "a current-only discovery proves nothing: the drive is closed"
            );
            assert!(bed.scheduler.vehicles[&id].force_closed_at_gap);
            assert!(!bed.drive_open(hub));
        })
        .await;
}

#[tokio::test]
async fn ct31e_an_accepted_no_data_candidate_uses_the_transaction_decision_time() {
    for mode in [
        crate::config::CollectorMode::Legacy,
        crate::config::CollectorMode::FleetPolling,
    ] {
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        SUPERVISED_COLLECTOR_TEST_SEAM
            .scope(seam, async {
                let mut bed = TickBed::new(mode);
                let hub = bed.store_open_drive().await;
                let id = core_id(1);
                bed.advance(Duration::from_millis(600_001));
                let planned_at = bed.wall();
                let batch = bed.plan_no_data_batch();
                assert_eq!(batch.hub_events.len(), 1);
                assert_eq!(batch.hub_events[0].observed_at_ms, planned_at);
                // The tick's requests take 37 ms before its transaction runs.
                bed.advance(Duration::from_millis(37));
                bed.commit(batch).await;
                assert_eq!(
                    bed.no_data_rows(),
                    vec![planned_at + 37],
                    "{mode:?}: observed and received at the decision time"
                );
                assert!(bed.scheduler.vehicles[&id].force_closed_at_gap);
                assert!(!bed.drive_open(hub), "one close");
            })
            .await;
    }
}

#[tokio::test]
async fn ct31f_a_no_data_candidate_rechecks_the_transaction_drive_state() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let id = core_id(1);
            let w0 = CORE_W0;
            // Long past `T_gap`: the only reason left to refuse the candidate is the
            // drive's state.
            bed.advance(Duration::from_millis(700_000));
            let mut batch = bed.plan_no_data_batch();
            assert_eq!(batch.hub_events.len(), 1);
            // The old Asleep discovery of the same tick closes the drive in storage
            // while the cache still says it is open, and its receipt is stale too.
            let mut vehicle = bed.scheduler.vehicles[&id].vehicle.clone();
            vehicle.state = "asleep".to_owned();
            batch.discoveries.push(DiscoveryItem {
                vehicle,
                received_at_ms: w0 + 1_000,
            });
            assert!(bed.drive_open(hub), "storage: the drive is open before the commit");
            assert!(
                bed.scheduler.vehicles[&id]
                    .lifecycle
                    .as_ref()
                    .is_some_and(|view| view.drive_open),
                "the cache: open"
            );
            bed.commit(batch).await;
            assert!(!bed.drive_open(hub), "storage: the Asleep intake closed it");
            assert!(bed.no_data_rows().is_empty(), "no event");
            assert!(!bed.scheduler.vehicles[&id].force_closed_at_gap, "no flag");
            let drives = bed.store.materialised_history(hub).expect("history").drives;
            assert_eq!(drives.len(), 1, "one historical closure");
            assert_eq!(drives[0].end_date_ms, w0);
        })
        .await;
}

#[tokio::test]
async fn stream_audit_failure_is_logged_once_without_a_durable_row() {
    // The audit transition is a journal entry of its own, one WARN at the first
    // failure of a car, whether or not any row can be written (C3 R28, R36, B2
    // clarification 3). A repeated failure adds none; a `Healthy` clears the state and
    // lets the next failure log again; the fresh lease stays service-ready, because
    // attention is not a service failure.
    let log = DecisionLog::start();
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = StreamBed::new();
            let id = core_id(1);
            let source = bed
                .store
                .register_source(&provider_source(CollectorProvider::Legacy), CORE_W0)
                .expect("source");
            let registered = bed
                .store
                .register_vehicle(
                    &VehicleDescriptor::new(source.source_id, "1")
                        .with_tesla_identity(Some(1), None),
                    CORE_W0,
                )
                .expect("vehicle");
            bed.core.scheduler.vehicles.get_mut(&id).unwrap().hub_id = Some(registered.vehicle_id);
            let hub_label = registered.vehicle_id.to_string();
            let lease_ms = current_epoch_millis().expect("clock");
            let _lease = bed
                .store
                .acquire_supervised_collector_lease(lease_ms)
                .expect("a fresh lease");

            // One run ends on an audit failure.
            async fn audit_failed_run(bed: &mut StreamBed, id: VehicleId) {
                let (_events, receiver) = mpsc::channel(4);
                let task = tokio::spawn(async {
                    Err::<(), _>(crate::tesla_stream::StreamSupervisorError::Audit(
                        StoreError::NilVehicleId,
                    ))
                });
                let mut streams = vec![VehicleStreamRuntime {
                    vehicle_id: id,
                    power_gate: Arc::new(StreamPowerGate::default()),
                    sensitive_access_failure: Arc::new(AtomicBool::new(false)),
                    events: receiver,
                    _shutdown: None,
                    task: Some(task),
                }];
                while !streams[0].task.as_ref().is_some_and(JoinHandle::is_finished) {
                    tokio::task::yield_now().await;
                }
                let mut projection = HashMap::new();
                drain_stream_events_with_cache(
                    &bed.store,
                    &mut bed.core.scheduler,
                    &mut streams,
                    &mut projection,
                )
                .await
                .expect("an audit failure is not a collector error");
            }
            let audit_lines =
                |log: &DecisionLog| log.lines("WARN", "stream_audit_failed");

            audit_failed_run(&mut bed, id).await;
            assert_eq!(audit_lines(&log), 1, "{}", log.contents());
            // The store refuses the tick that would write the row: no durable row, the
            // live attention is shown.
            seam.set_tick_commit_fault(true);
            let now = bed.core.clock.mono_now();
            bed.core.scheduler.begin_tick();
            let commit = TickCommit {
                store: &bed.store,
                cursor_key: &CursorKey::from_bytes([0x13; 32]),
                provider: CollectorProvider::Legacy,
                offline_drive_timeout: Duration::from_secs(600),
            };
            let mut identities = IdentityCache::default();
            let outcome = commit_tick(
                &commit,
                &mut identities,
                &mut bed.core.scheduler,
                TickBatch::default(),
                now,
            )
            .await
            .expect("commit");
            seam.set_tick_commit_fault(false);
            assert!(matches!(outcome, TickOutcome::Failed), "{outcome:?}");
            let stored: i64 = bed
                .store
                .open()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM collector_vehicle_state WHERE reason_code = 'stream_audit_failed'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(stored, 0, "no durable row");
            assert_eq!(
                bed.core.scheduler.status(now).vehicles[0].detail,
                Some(STATUS_STREAM_AUDIT_FAILED)
            );
            assert_eq!(
                bed.store.collector_readiness_at(lease_ms).unwrap().failure,
                Some(crate::db::CollectionAttentionCode::CollectorStreamAuditFailed),
                "the live attention"
            );
            assert!(
                bed.store.service_readiness_at(true, lease_ms).is_ok(),
                "attention does not fail a fresh lease"
            );
            // A repeated failure of the same car adds no line.
            audit_failed_run(&mut bed, id).await;
            assert_eq!(audit_lines(&log), 1, "{}", log.contents());
            // `Healthy` clears it, and the next failure is a new transition.
            bed.dispatch(StreamEvent::Healthy);
            assert!(!bed.core.scheduler.vehicles[&id].stream.audit_failed);
            audit_failed_run(&mut bed, id).await;
            assert_eq!(audit_lines(&log), 2, "{}", log.contents());
            // The entry is redacted: the car's Hub id, no Tesla id, no error text.
            let entry = log
                .contents()
                .lines()
                .find(|line| line.contains("stream_audit_failed"))
                .expect("an audit entry")
                .to_owned();
            assert!(entry.contains(&hub_label), "{entry}");
            assert!(entry.contains("collector.decision"), "{entry}");
            assert!(!entry.contains("VIN"), "{entry}");
            assert!(!entry.contains("error="), "{entry}");
            assert!(!entry.contains("vehicle_id"), "{entry}");
            assert!(bed.store.service_readiness_at(true, lease_ms).is_ok());
        })
        .await;
}

#[tokio::test]
async fn r36_a_stopped_run_keeps_its_queued_telemetry() {
    // Frames still queued when runs are stopped are each stored once, for the cars
    // whose frames are stored at all: a Disabled or Removed car's are not (R36, R62).
    let mut bed = StreamBed::new();
    let now = bed.core.t0;
    bed.core.scheduler.accept_discovery(
        vec![
            core_vehicle(1, "online"),
            core_vehicle(2, "online"),
            core_vehicle(3, "online"),
        ],
        now,
    );
    bed.core.scheduler.vehicles.get_mut(&core_id(2)).unwrap().hold = VehicleHold::Disabled;
    bed.core.scheduler.vehicles.get_mut(&core_id(3)).unwrap().hold = VehicleHold::Removed;
    let base = StreamBed::now_ms();
    let mut streams = Vec::new();
    let mut senders = Vec::new();
    for id in [1u64, 2, 3] {
        let (events, receiver) = mpsc::channel(16);
        let (shutdown, stop) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = stop.await;
            Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
        });
        streams.push(VehicleStreamRuntime {
            vehicle_id: core_id(id),
            power_gate: Arc::new(StreamPowerGate::default()),
            sensitive_access_failure: Arc::new(AtomicBool::new(false)),
            events: receiver,
            _shutdown: Some(shutdown),
            task: Some(task),
        });
        for step in 0..3 {
            events
                .send(bed.telemetry(frame(base + step * 100 + id as i64, Some("P"), Some(0), Some(0))))
                .await
                .expect("queued");
        }
        // A control event among them changes nothing.
        events.send(StreamEvent::Healthy).await.expect("queued");
        senders.push(events);
    }
    let mut cache = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let mut budget = DrainBudget::new(Instant::now());
    let store = bed.store.clone();
    stop_vehicle_streams(
        &store,
        &mut bed.core.scheduler,
        &mut streams,
        &[core_id(1), core_id(2), core_id(3)],
        &mut cache,
        &mut pending,
        &mut budget,
    )
    .await
    .expect("the stop");
    assert!(streams.is_empty());
    // Each frame of the eligible car was folded once: its cursor stands at three. The
    // others were never stored (no stream context was even made for them).
    let hub = cache[&core_id(1)].registered_vehicle_id;
    assert_eq!(
        bed.store
            .load_lifecycle_state(hub)
            .expect("lifecycle")
            .expect("state")
            .last_observation_id,
        3,
        "each frame of the eligible car once"
    );
    assert!(cache.contains_key(&core_id(1)));
    assert!(!cache.contains_key(&core_id(2)) && !cache.contains_key(&core_id(3)));
    assert_eq!(cache[&core_id(1)].last_stream_timestamp_ms, Some(base + 201));
}

// ---- R40: a current-only discovery of an earlier tick is no evidence -----------------------

impl TickBed {
    /// A tick, committed alone, whose only content is the car's discovery with `state`
    /// received now, with no `no_data` candidate: the scheduler's cached view is made
    /// stale first so the transaction decides the discovery from the store's rows.
    async fn commit_a_standalone_discovery(&mut self, state: &str) {
        let id = core_id(1);
        let mut vehicle = self.scheduler.vehicles[&id].vehicle.clone();
        vehicle.state = state.to_owned();
        self.scheduler
            .vehicles
            .get_mut(&id)
            .expect("the car")
            .lifecycle_stale = true;
        let now = self.now();
        self.scheduler.begin_tick();
        let batch = TickBatch {
            discoveries: vec![DiscoveryItem {
                vehicle,
                received_at_ms: self.wall(),
            }],
            first_result_at: Some(now),
            ..TickBatch::default()
        };
        self.commit(batch).await;
    }

    /// The evidence the store holds for the car: the newest receipt of a delivered row.
    fn stored_evidence(&self, hub: Uuid) -> Option<i64> {
        self.store
            .collector_lifecycle_snapshot(hub)
            .expect("snapshot")
            .last_stored_obs_ms
    }
}

async fn ct31g_case(mode: crate::config::CollectorMode) {
    let discovery_type = if mode == crate::config::CollectorMode::FleetPolling {
        "fleet_api_discovery_v1"
    } else {
        "owner_api_discovery_v1"
    };
    // A current-only discovery committed in an EARLIER tick, with no candidate, is no
    // evidence: it must not postpone the closure of the drive its car has left open.
    let mut bed = TickBed::new(mode);
    let hub = bed.store_open_drive().await;
    let id = core_id(1);
    let w0 = CORE_W0;
    assert_eq!(bed.stored_evidence(hub), Some(w0), "the evidence is the car's last data");
    // The car's current discovery row is gone (the missing-row case of R14).
    bed.store
        .open()
        .expect("connection")
        .execute(
            "DELETE FROM current_observations WHERE record_type IN
             ('owner_api_discovery_v1', 'fleet_api_discovery_v1')",
            [],
        )
        .expect("the car has no current discovery row");
    bed.advance(Duration::from_millis(300_000));
    // The discovery states what the lifecycle already knows: the action is the write of
    // the current row alone.
    assert_eq!(
        crate::db::discovery_action("online", Some("online"), None),
        crate::db::DiscoveryAction::WriteCurrentOnly
    );
    bed.commit_a_standalone_discovery("online").await;
    let row_receipt: Option<i64> = bed
        .store
        .open()
        .expect("connection")
        .query_row(
            "SELECT received_at_ms FROM current_observations WHERE record_type = ?1",
            [discovery_type],
            |row| row.get(0),
        )
        .ok();
    assert_eq!(row_receipt, Some(w0 + 300_000), "presence changed: the current row is written");
    assert_eq!(
        bed.stored_evidence(hub),
        Some(w0),
        "the evidence did not: the row was delivered to no lifecycle"
    );
    assert!(bed.no_data_rows().is_empty());
    // `T_gap` after the evidence there is no event, and one millisecond later there is
    // exactly one, stamped with the decision time, closing the drive.
    bed.advance(Duration::from_millis(300_000));
    assert_eq!(bed.wall(), w0 + 600_000);
    assert_eq!(bed.scheduler.vehicles[&id].last_stored_obs_ms, Some(w0));
    // What a reload of the scheduler would read from the store agrees.
    assert_eq!(bed.stored_evidence(hub), Some(w0));
    let batch = bed.plan_no_data_batch();
    assert!(batch.hub_events.is_empty(), "not yet strictly over T_gap");
    bed.commit(batch).await;
    assert!(bed.no_data_rows().is_empty());
    bed.advance(Duration::from_millis(1));
    let batch = bed.plan_no_data_batch();
    assert_eq!(batch.hub_events.len(), 1, "the candidate is planned");
    bed.commit(batch).await;
    assert_eq!(
        bed.no_data_rows(),
        vec![w0 + 600_001],
        "one event, stamped with the decision time"
    );
    assert!(bed.scheduler.vehicles[&id].force_closed_at_gap, "the flag is set");
    assert!(!bed.drive_open(hub), "the drive is closed");
}

#[tokio::test]
async fn ct31g_a_prior_current_only_discovery_does_not_delay_no_data() {
    for mode in [
        crate::config::CollectorMode::Legacy,
        crate::config::CollectorMode::FleetPolling,
    ] {
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        SUPERVISED_COLLECTOR_TEST_SEAM.scope(seam, ct31g_case(mode)).await;
    }
}

#[tokio::test]
async fn ct31g_a_prior_delivered_discovery_advances_the_evidence() {
    // The control: a discovery that is DELIVERED to the lifecycle (its state differs
    // from the lifecycle's reachability) is evidence, whenever its tick was.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let id = core_id(1);
            let w0 = CORE_W0;
            bed.advance(Duration::from_millis(300_000));
            bed.commit_a_standalone_discovery("waking").await;
            assert_eq!(
                bed.stored_evidence(hub),
                Some(w0 + 300_000),
                "a delivered discovery advances the evidence"
            );
            bed.advance(Duration::from_millis(300_001));
            let batch = bed.plan_no_data_batch();
            assert!(batch.hub_events.is_empty(), "the evidence is 300 s newer");
            assert_eq!(
                bed.scheduler.vehicles[&id].last_stored_obs_ms,
                Some(w0 + 300_000)
            );
            bed.commit(batch).await;
            assert!(bed.no_data_rows().is_empty());
            assert!(bed.drive_open(hub));
        })
        .await;
}
