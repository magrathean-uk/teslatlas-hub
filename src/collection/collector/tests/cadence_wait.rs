// SPDX-License-Identifier: AGPL-3.0-only

// The managed configuration wait (B2 clarification 3, C3 R21, R8): an otherwise
// eligible Serve whose collector cadence is invalid takes the lease, records one
// stop gap, hands over its cursor key, renews and logs once, and collects
// nothing until the setting is corrected and the service restarted. These tests
// run the actual entry (`run_supervised_for_admitted_user`'s body, with the clients
// of a run that must build none) in every mode, or the wait itself under injected
// wall and runtime clocks. They use no fake Tesla
// source, no stream server and no network; the transport tripwire is a loopback
// listener the wait must never reach.

/// The waits log their one ERROR from a call site that a test with a captured
/// subscriber must see registered under its own dispatcher. Tests that run a wait
/// therefore take turns, so no concurrent test registers that call site first
/// without a subscriber.
static WAIT_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const WAIT_WALL0: i64 = 1_800_000_000_000;
const WAIT_MESSAGE: &str = "collector setting collector.request_timeout_seconds = 121 must be \
                            between 1 and 120 seconds inclusive";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitMode {
    Legacy,
    Provider,
    Fleet,
}

const WAIT_MODES: [WaitMode; 3] = [WaitMode::Legacy, WaitMode::Provider, WaitMode::Fleet];

/// The production supervised entry of a run that must build no Tesla-facing client
/// at all: the wait returns before any client is made, and a regression that went
/// on to a collection start would panic here instead of reaching a Tesla host.
async fn run_wait_entry<F>(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ready: oneshot::Sender<CursorKey>,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    run_supervised_with_clients_for_admitted_user(
        store,
        config,
        admission,
        ready,
        |_| -> Result<OwnerApi, CollectorError> {
            panic!("an Owner API client was built by a configuration wait")
        },
        |_| -> Result<FleetClients, CollectorError> {
            panic!("Fleet clients were built by a configuration wait")
        },
        shutdown,
    )
    .await
}

/// An imported, enabled car, with constants of this fixture only.
fn cadence_wait_seed_car(store: &HubStore) {
    use crate::teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history};
    use crate::teslamate_projection::{TeslaMateCar, TeslaMateHistory};
    let history = TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: 6_100_001,
            vid: Some(6_200_001),
            vin: Some("5YJ3E1EA7KF610001".to_owned()),
            name: Some("Cadence wait fixture".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: crate::hub_pack::ProjectionCarSettings {
                enabled: true,
                use_streaming_api: true,
                ..Default::default()
            },
        }],
        drives: Vec::new(),
        positions: Vec::new(),
        charging_processes: Vec::new(),
        charges: Vec::new(),
        addresses: Vec::new(),
        geofences: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
    };
    publish_history(
        store,
        &CursorKey::from_bytes([0xD4; 32]),
        &TeslaMateImportRequest {
            source_key: "cadence-wait-fixture".to_owned(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: current_epoch_millis().expect("clock") - 60_000,
        },
        &history,
    )
    .expect("seed car");
}

/// A configuration whose cadence is invalid in `mode` (the request timeout is above
/// its range); `base_url` points the Owner API and its stream at a
/// loopback tripwire in Provider mode.
fn cadence_wait_config(
    directory: &std::path::Path,
    mode: WaitMode,
    tripwire: Option<&std::net::SocketAddr>,
) -> HubConfig {
    let mut config = supervised_restart_test_config(directory);
    config.collector.request_timeout_seconds = 121;
    match mode {
        WaitMode::Legacy => {}
        WaitMode::Provider => {
            if let Some(address) = tripwire {
                config.collector.owner_api_base_url = Some(format!("http://{address}"));
                config.collector.stream_endpoint_override = Some(format!("ws://{address}"));
            } else {
                config.collector.owner_api_base_url =
                    Some("https://api.provider.example.test".to_owned());
            }
        }
        WaitMode::Fleet => config.collector.provider = CollectorProvider::Fleet,
    }
    assert_eq!(
        config.collector.cadence_violation().map(|violation| violation.to_string()),
        Some(WAIT_MESSAGE.to_owned()),
        "{mode:?}"
    );
    config
}

/// A store with a car, ready for a wait in `mode`.
fn cadence_wait_hub() -> (tempfile::TempDir, HubStore) {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    cadence_wait_seed_car(&store);
    (temporary, store)
}

/// Everything the wait must leave alone, as text.
fn cadence_wait_state_dump(store: &HubStore) -> String {
    let rows = store.load_collector_state_rows().expect("collector rows");
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}",
        rows.account,
        rows.vehicles,
        rows.counters,
        rows.actions,
        store.load_collector_auth_row().expect("auth row")
    )
}

fn cadence_wait_gaps(store: &HubStore) -> Vec<(String, i64, Option<i64>, bool)> {
    store
        .collection_gaps(100)
        .expect("gaps")
        .into_iter()
        .map(|gap| (gap.kind, gap.started_at_ms, gap.ended_at_ms, gap.alert))
        .collect()
}

/// A previous run's leftovers: a refusal, a hold, counts, a pending owner
/// action and a credential status, so a wait that touched any of them shows.
fn cadence_wait_prepare_state(store: &HubStore) {
    let vehicle = store
        .configured_tesla_vehicles()
        .expect("configured")
        .first()
        .expect("a configured car")
        .0;
    store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(
                    transaction,
                    &crate::db::CollectorRowsWrite {
                        now_ms: WAIT_WALL0 - 600_000,
                        account: Some(crate::db::CollectorAccountRow {
                            mode: "legacy".to_owned(),
                            config_fingerprint: "c".repeat(64),
                            state: "owner_api_refused".to_owned(),
                            reason_code: "owner_api_refused".to_owned(),
                            since_ms: WAIT_WALL0 - 600_000,
                            rate_limited_until_ms: None,
                            next_probe_ms: None,
                        }),
                        vehicles: vec![crate::db::CollectorVehicleRow {
                            vehicle_id: vehicle,
                            phase: "online".to_owned(),
                            hold: "paused".to_owned(),
                            reason_code: "paused_access_refused".to_owned(),
                            phase_since_ms: WAIT_WALL0 - 600_000,
                            suspended_until_ms: None,
                            next_probe_ms: None,
                            last_wake_read_at_ms: None,
                        }],
                        counters: vec![crate::db::CollectorCounterRow {
                            month: "2026-10".to_owned(),
                            mode: "legacy".to_owned(),
                            category: "data".to_owned(),
                            sent: 9,
                            billable: 0,
                            reserved: 0,
                        }],
                        gap_inserts: vec![crate::db::CollectorGapInsert {
                            vehicle_id: None,
                            kind: "hub_stopped".to_owned(),
                            started_at_ms: WAIT_WALL0 - 600_000,
                            ended_at_ms: None,
                            alert: false,
                        }],
                        ..crate::db::CollectorRowsWrite::default()
                    },
                )?;
                Ok::<_, StoreError>(((), wrote))
            },
            || Ok(()),
        )
        .expect("previous run's rows");
    store
        .insert_collector_owner_action("retry_collection", None, WAIT_WALL0 - 500_000)
        .expect("owner action");
    store
        .write_collector_auth_row(
            &crate::db::CollectorAuthRow {
                state: "api_refused".to_owned(),
                reason: Some("owner_api".to_owned()),
                collecting: false,
                retry_at: None,
                stopped_generation: None,
                stopped_mode: Some("legacy".to_owned()),
                fuse_consecutive: 0,
                fuse_generation: None,
                fuse_last_rejection_at: None,
                fuse_pause_until: None,
                fuse_rejected_access_digest: None,
                clock_skew: false,
            },
            WAIT_WALL0 - 500_000,
        )
        .expect("auth row");
}

#[tokio::test(start_paused = true)]
async fn cadence_wait_renews_past_initial_lease_and_releases_ownership() {
    let _serial = WAIT_SERIAL.lock().await;
    let (temporary, store) = cadence_wait_hub();
    let config = cadence_wait_config(temporary.path(), WaitMode::Legacy, None);
    let violation = config.collector.cadence_violation().expect("violation");
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let clock = ManualClock::new(Instant::now(), WAIT_WALL0);
    let (ready_tx, ready_rx) = oneshot::channel();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let task_store = store.clone();
    let task_config = config.clone();
    let task_clock: Arc<dyn CollectorClock> = clock.clone();
    let mut task = tokio::spawn(async move {
        run_cadence_configuration_wait(
            &task_store,
            &task_config,
            Arc::clone(&admission),
            RunWriteGuard::for_run(Some(Arc::clone(&admission))),
            ready_tx,
            violation,
            task_clock,
            SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
            async move {
                let _ = stop_rx.await;
            },
        )
        .await
    });
    ready_rx.await.expect("handoff");
    let initial = store.supervised_collector_lease_status().unwrap().expect("lease");
    assert_eq!(initial.started_at_ms, WAIT_WALL0);
    assert_eq!(initial.lease_until_ms, WAIT_WALL0 + crate::db::SUPERVISED_COLLECTOR_LEASE_MS);
    // Advance the wall and runtime clocks together in 5-second steps, to 40
    // seconds: past the initial 30-second lease, which the heartbeat keeps
    // renewing, so a competing collector is refused at every step.
    for step in 1..=8_i64 {
        clock.advance(Duration::from_secs(5));
        tokio::time::advance(Duration::from_secs(5)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        let wall = WAIT_WALL0 + step * 5_000;
        let lease = store.supervised_collector_lease_status().unwrap().expect("lease");
        assert_eq!(lease.instance_id, initial.instance_id, "step {step}");
        assert_eq!(lease.heartbeat_at_ms, wall, "renewed at step {step}");
        assert_eq!(lease.lease_until_ms, wall + crate::db::SUPERVISED_COLLECTOR_LEASE_MS);
        assert!(matches!(
            store.acquire_supervised_collector_lease(wall),
            Err(StoreError::SupervisedCollectorLeaseHeld)
        ));
    }
    assert!(!task.is_finished());
    // An orderly end stops the renewal and releases only the wait's own lease.
    stop_tx.send(()).expect("shutdown");
    task.await.expect("join").expect("an orderly wait shutdown");
    assert!(store.supervised_collector_lease_status().unwrap().is_none());
    assert_eq!(
        cadence_wait_gaps(&store)
            .iter()
            .filter(|gap| gap.0 == "hub_stopped" && gap.2.is_none())
            .count(),
        1,
        "the wait's stop stays open for the next ordinary start"
    );

    // A replacement that took over after expiry survives the old wait's cleanup:
    // the wall clock jumps past the lease with no renewal in between, a
    // competitor acquires it, and the old heartbeat finds it lost.
    let (ready_tx, ready_rx) = oneshot::channel();
    let (_keep_open, stop_rx) = oneshot::channel::<()>();
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let violation = config.collector.cadence_violation().expect("violation");
    let task_store = store.clone();
    let task_config = config.clone();
    let task_clock: Arc<dyn CollectorClock> = clock.clone();
    task = tokio::spawn(async move {
        run_cadence_configuration_wait(
            &task_store,
            &task_config,
            Arc::clone(&admission),
            RunWriteGuard::for_run(Some(Arc::clone(&admission))),
            ready_tx,
            violation,
            task_clock,
            SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
            async move {
                let _ = stop_rx.await;
            },
        )
        .await
    });
    ready_rx.await.expect("second handoff");
    clock.step_wall_ms(31_000);
    let wall = clock.wall_now_ms().expect("wall");
    let replacement = store.acquire_supervised_collector_lease(wall).expect("replacement");
    let replacement_status = store.supervised_collector_lease_status().unwrap().expect("lease");
    tokio::time::advance(Duration::from_secs(5)).await;
    let outcome = task.await.expect("join");
    assert!(
        matches!(outcome, Err(CollectorError::Store(StoreError::SupervisedCollectorLeaseLost))),
        "{outcome:?}"
    );
    let after = store.supervised_collector_lease_status().unwrap().expect("replacement survives");
    assert_eq!(after.instance_id, replacement_status.instance_id);
    assert_eq!(after.heartbeat_at_ms, replacement_status.heartbeat_at_ms);
    store.release_supervised_collector_lease(replacement).expect("replacement releases");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cadence_wait_has_no_collection_side_effects_and_requires_restart() {
    let _serial = WAIT_SERIAL.lock().await;
    for mode in WAIT_MODES {
        let (temporary, store) = cadence_wait_hub();
        cadence_wait_prepare_state(&store);
        // Credentials the corrected restart can use, left alone by the wait: a usable
        // Owner pair (Legacy, Provider) or a sound Fleet pair with an identified
        // unresolved refresh (Fleet, so that its restart sends nothing).
        match mode {
            WaitMode::Legacy | WaitMode::Provider => {
                ct72_owner_stores_credentials(&store, temporary.path(), "owner-access-0", "owner-refresh-0");
            }
            WaitMode::Fleet => {
                p3_add_unresolved_fleet_refresh(&store, temporary.path());
            }
        }
        let receipts_before: i64 = store
            .open()
            .expect("catalogue")
            .query_row("SELECT COUNT(*) FROM outbound_request_receipts", [], |row| row.get(0))
            .expect("receipts");
        // The tripwire: nothing may connect to the provider's Owner API or stream.
        let tripwire = std::net::TcpListener::bind("127.0.0.1:0").expect("tripwire");
        tripwire.set_nonblocking(true).expect("nonblocking");
        let address = tripwire.local_addr().expect("address");
        let config = cadence_wait_config(temporary.path(), mode, Some(&address));
        let state_before = cadence_wait_state_dump(&store);
        let gaps_before = cadence_wait_gaps(&store);
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        let (ready_tx, ready_rx) = oneshot::channel();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let task_store = store.clone();
        let task_config = config.clone();
        let task_seam = Arc::clone(&seam);
        let mut task = tokio::spawn(async move {
            SUPERVISED_COLLECTOR_TEST_SEAM
                .scope(task_seam, async move {
                    run_wait_entry(
                        &task_store,
                        &task_config,
                        admission,
                        ready_tx,
                        async move {
                            let _ = stop_rx.await;
                        },
                    )
                    .await
                })
                .await
        });
        let _cursor =
            wait_for_supervised_signal_or_abort("cadence wait handoff", &mut task, ready_rx).await;
        // Let the wait run for a moment: a collector would have iterated, read its
        // settings, committed a tick or sent a request by now.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!task.is_finished(), "{mode:?}: the wait stays alive");
        assert!(tripwire.accept().is_err(), "{mode:?}: nothing reached the transport");
        assert_eq!(seam.loop_iterations(), 0, "{mode:?}: no collection loop");
        assert_eq!(seam.settings_refreshes(), 0, "{mode:?}");
        assert_eq!(seam.tick_commits.load(Ordering::SeqCst), 0, "{mode:?}");
        let receipts: i64 = store
            .open()
            .expect("catalogue")
            .query_row("SELECT COUNT(*) FROM outbound_request_receipts", [], |row| row.get(0))
            .expect("receipts");
        assert_eq!(receipts, receipts_before, "{mode:?}: no request, no token exchange");
        assert!(
            !temporary.path().read_dir().expect("directory").flatten().any(|entry| {
                entry.file_name().to_string_lossy().contains("control")
                    && entry.file_name().to_string_lossy().ends_with(".sock")
            }),
            "{mode:?}: no resident control socket"
        );
        stop_tx.send(()).expect("shutdown");
        join_supervised_restart_task("cadence wait shutdown", &mut task)
            .await
            .expect("an orderly wait shutdown");
        // Account, refusal, hold, counts, owner action and credential status are
        // exactly as the previous run left them; the retry is unconsumed; the only
        // new rows are the wait's gaps.
        assert_eq!(cadence_wait_state_dump(&store), state_before, "{mode:?}");
        assert_eq!(store.pending_collector_owner_actions().unwrap().len(), 1, "{mode:?}");
        let gaps = cadence_wait_gaps(&store);
        assert_eq!(gaps.len(), gaps_before.len() + 1, "{mode:?}: the old stop closed, a new one opened");
        assert!(gaps.iter().filter(|gap| gap.2.is_none()).count() == 1, "{mode:?}");
        assert!(store.supervised_collector_lease_status().unwrap().is_none(), "{mode:?}");
        // The corrected restart runs through the production entry (B2 P3). The ordinary
        // start closes the wait's stop and records no second `hub_down`, and it, not the
        // wait, resets the stored refusal: the stored fingerprint is not the corrected
        // configuration's in any mode, and the wait above left it alone. Legacy and
        // Provider send their requests to a recording loopback double (stream input is
        // injected, bounded); Fleet starts over its identified unresolved refresh and
        // sends nothing.
        let mut corrected = config.clone();
        corrected.collector.request_timeout_seconds = 60;
        assert_eq!(corrected.collector.cadence_violation(), None);
        let (wire, wire_address, wire_server) = p3_wire().await;
        let restart_started_ms = current_epoch_millis().expect("clock");
        let stats = Arc::new(ProducerStats::default());
        let restart_seam = Arc::new(SupervisedCollectorTestSeam::default());
        let mut restart = match mode {
            WaitMode::Fleet => {
                // The Fleet clients are those of the recording double: the run sends
                // nothing (its refresh is fenced), and the double proves it.
                let fleet_base =
                    url::Url::parse(&format!("http://{wire_address}/")).expect("fleet base");
                P3Entry::start_fleet(&store, &corrected, &fleet_base).await
            }
            WaitMode::Legacy | WaitMode::Provider => {
                // The owner pair is not due, so the unit sends no token request; Legacy
                // gets a region (its stored pair is opaque), Provider a token endpoint it
                // never reaches.
                corrected.collector.stream_region = Some(crate::tesla_stream::StreamRegion::Global);
                if mode == WaitMode::Provider {
                    corrected.collector.legacy_auth.token_url =
                        Some("https://127.0.0.1:9/oauth2/v3/token".to_owned());
                    // A provider's stream URL, never connected to: the injected run
                    // takes its place.
                    corrected.collector.stream_endpoint_override =
                        Some("wss://stream.provider.example.test/streaming/".to_owned());
                }
                restart_seam.inject_stream_runs(telemetry_runs(&stats, Duration::from_millis(200)));
                let base = url::Url::parse(&format!("http://{wire_address}/")).expect("base");
                P3Entry::start_owner_wire(&store, &corrected, &base, Arc::clone(&restart_seam)).await
            }
        };
        match mode {
            WaitMode::Fleet => {
                restart.settled().await;
                assert!(
                    wire.seen().is_empty(),
                    "Fleet: the clients of the recording double saw nothing: {:?}",
                    wire.seen()
                );
            }
            WaitMode::Legacy | WaitMode::Provider => {
                restart
                    .until("the list, the read and the stream admission", || {
                        wire.count("/api/1/products") >= 1
                            && wire.count("/vehicle_data") >= 1
                            && restart_seam.stream_admissions() >= 1
                    })
                    .await;
                assert_eq!(wire.count("oauth2"), 0, "{mode:?}: no token request");
            }
        }
        // While the restarted collector runs: the wait's stop is closed, no `hub_down`
        // was recorded, and the refusal was reset by this start, in the account row and
        // in C5's.
        let gaps = cadence_wait_gaps(&store);
        assert!(
            gaps.iter().filter(|gap| gap.0 == "hub_stopped").all(|gap| gap.2.is_some()),
            "{mode:?}: the ordinary start closed the stop: {gaps:?}"
        );
        assert_eq!(gaps.iter().filter(|gap| gap.0 == "hub_down").count(), 0, "{mode:?}");
        let account_after = store.load_collector_state_rows().expect("rows").account;
        assert!(
            account_after
                .as_ref()
                .is_none_or(|account| account.state != "owner_api_refused"),
            "{mode:?}: the ordinary start, not the wait, reset the refusal: {account_after:?}"
        );
        assert!(
            store
                .load_collector_auth_row()
                .expect("auth row")
                .is_none_or(|row| row.state != "api_refused"),
            "{mode:?}: and lifted C5's own"
        );
        restart.stop().await.expect("an orderly stop of the restarted collector");
        if mode == WaitMode::Fleet {
            assert!(wire.seen().is_empty(), "Fleet: nothing arrived at any point");
        }
        wire_server.abort();
        // The restart's own orderly stop is a new open `hub_stopped` gap, started after
        // the restart began; every earlier gap stays closed.
        let gaps = cadence_wait_gaps(&store);
        let open: Vec<_> = gaps
            .iter()
            .filter(|gap| gap.2.is_none() && gap.0 != "auth_terminal")
            .collect();
        assert_eq!(open.len(), 1, "{mode:?}: {gaps:?}");
        assert!(open[0].0 == "hub_stopped" && open[0].1 >= restart_started_ms, "{mode:?}: {gaps:?}");
        // (Fleet's credential stop keeps its own `auth_terminal` gap open, as it should.)
        assert_eq!(
            gaps.iter().filter(|gap| gap.0 == "auth_terminal" && gap.2.is_none()).count(),
            usize::from(mode == WaitMode::Fleet),
            "{mode:?}: {gaps:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cadence_wait_preserves_admission_and_singleton_fences() {
    let _serial = WAIT_SERIAL.lock().await;
    // A wait that cannot be admitted, placed or sequenced is refused before the
    // handoff and before any write: no lease, no gap, no state.
    let attempt = |store: HubStore, config: HubConfig, admission: Arc<crate::hub_user_process::AdmittedUserHub>| async move {
        let (ready_tx, ready_rx) = oneshot::channel();
        let outcome = run_wait_entry(
            &store,
            &config,
            admission,
            ready_tx,
            std::future::pending::<()>(),
        )
        .await;
        (outcome, ready_rx.await.is_err())
    };
    // An admission whose lock file was replaced.
    {
        let (temporary, store) = cadence_wait_hub();
        let config = cadence_wait_config(temporary.path(), WaitMode::Legacy, None);
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let lock = temporary.path().join(crate::user_lifetime_lock::LOCK_FILE_NAME);
        fs::remove_file(&lock).expect("remove lock");
        fs::write(&lock, b"").expect("replacement lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).expect("mode");
        let (outcome, dropped) = attempt(store.clone(), config, admission).await;
        assert!(outcome.is_err(), "an invalid admission refuses");
        assert!(dropped, "no handoff");
        assert!(store.supervised_collector_lease_status().unwrap().is_none());
        assert!(cadence_wait_gaps(&store).is_empty());
    }
    // A store path that is not the admitted one.
    {
        let (temporary, store) = cadence_wait_hub();
        let other = crate::private_tempdir().expect("other directory");
        let config = cadence_wait_config(other.path(), WaitMode::Legacy, None);
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let (outcome, dropped) = attempt(store.clone(), config, admission).await;
        assert!(outcome.is_err(), "a foreign data directory refuses");
        assert!(dropped);
        assert!(store.supervised_collector_lease_status().unwrap().is_none());
        assert!(cadence_wait_gaps(&store).is_empty());
    }
    // No vehicle lineage: nothing is claimed.
    {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let config = cadence_wait_config(temporary.path(), WaitMode::Legacy, None);
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let (outcome, dropped) = attempt(store.clone(), config, admission).await;
        assert!(matches!(outcome, Err(CollectorError::SelectedVehicleMissing)));
        assert!(dropped);
        assert!(store.supervised_collector_lease_status().unwrap().is_none());
        assert!(cadence_wait_gaps(&store).is_empty());
    }
    // A live lease of another collector: the singleton fence refuses, the other
    // lease is untouched and no gap is written.
    {
        let (temporary, store) = cadence_wait_hub();
        let config = cadence_wait_config(temporary.path(), WaitMode::Legacy, None);
        let other = store
            .acquire_supervised_collector_lease(current_epoch_millis().expect("clock"))
            .expect("another collector");
        let held = store.supervised_collector_lease_status().unwrap().expect("lease");
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let (outcome, dropped) = attempt(store.clone(), config, admission).await;
        assert!(matches!(
            outcome,
            Err(CollectorError::Store(StoreError::SupervisedCollectorLeaseHeld))
        ));
        assert!(dropped);
        assert_eq!(store.supervised_collector_lease_status().unwrap(), Some(held));
        assert!(cadence_wait_gaps(&store).is_empty());
        store.release_supervised_collector_lease(other).expect("release");
    }
    // Active invalidation after the handoff: the wait ends with the
    // sensitive-access failure, its heartbeat writes nothing more, the lease is
    // retained to expire (never released) and no gap or state is written.
    {
        let (temporary, store) = cadence_wait_hub();
        let config = cadence_wait_config(temporary.path(), WaitMode::Legacy, None);
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let (ready_tx, ready_rx) = oneshot::channel();
        let (_keep_open, stop_rx) = oneshot::channel::<()>();
        let task_store = store.clone();
        let task_config = config.clone();
        let mut task = tokio::spawn(async move {
            run_wait_entry(
                &task_store,
                &task_config,
                admission,
                ready_tx,
                async move {
                    let _ = stop_rx.await;
                },
            )
            .await
        });
        let _cursor =
            wait_for_supervised_signal_or_abort("invalidation handoff", &mut task, ready_rx).await;
        // The heartbeat renews once as soon as it starts; let that settle.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let lease = store.supervised_collector_lease_status().unwrap().expect("lease");
        let gaps = cadence_wait_gaps(&store);
        let state = cadence_wait_state_dump(&store);
        let lock = temporary.path().join(crate::user_lifetime_lock::LOCK_FILE_NAME);
        fs::remove_file(&lock).expect("remove lock");
        fs::write(&lock, b"").expect("replacement lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).expect("mode");
        let outcome = join_supervised_restart_task("invalidated wait", &mut task).await;
        assert!(matches!(outcome, Err(CollectorError::SensitiveAccessUnavailable)), "{outcome:?}");
        // Retained, and not renewed: still the same instance and heartbeat.
        let after = store.supervised_collector_lease_status().unwrap().expect("lease retained");
        assert_eq!(after.instance_id, lease.instance_id);
        assert_eq!(after.heartbeat_at_ms, lease.heartbeat_at_ms, "no later heartbeat");
        assert_eq!(cadence_wait_gaps(&store), gaps);
        assert_eq!(cadence_wait_state_dump(&store), state);
    }
}

/// Run the actual entry in `mode` for a moment, capturing what it logs.
async fn cadence_wait_captured_run(mode: WaitMode, with_renewal_and_retry: bool) -> String {
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    // Other tests run in parallel without a subscriber and may have registered the
    // logging call sites as uninteresting; read the interest again under ours.
    tracing::callsite::rebuild_interest_cache();
    let (temporary, store) = cadence_wait_hub();
    let config = cadence_wait_config(temporary.path(), mode, None);
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let (ready_tx, ready_rx) = oneshot::channel();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let task_store = store.clone();
    let task_config = config.clone();
    let mut task = tokio::spawn(async move {
        run_wait_entry(
            &task_store,
            &task_config,
            admission,
            ready_tx,
            async move {
                let _ = stop_rx.await;
            },
        )
        .await
    });
    let _cursor = wait_for_supervised_signal_or_abort("logging handoff", &mut task, ready_rx).await;
    if with_renewal_and_retry {
        // Through renewal, a status read and an owner retry: none repeats or
        // bypasses the one startup error.
        tokio::time::sleep(Duration::from_millis(300)).await;
        store
            .insert_collector_owner_action("retry_collection", None, current_epoch_millis().unwrap())
            .expect("retry");
        let _ = store.collector_readiness_detail_at(current_epoch_millis().unwrap());
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    stop_tx.send(()).expect("shutdown");
    join_supervised_restart_task("logging shutdown", &mut task)
        .await
        .expect("an orderly wait shutdown");
    let text = String::from_utf8(sink.0.lock().unwrap().clone()).expect("log text");
    assert!(
        !text.contains(temporary.path().to_string_lossy().as_ref()),
        "no private path in the log: {text}"
    );
    text
}

#[tokio::test(flavor = "current_thread")]
async fn cadence_wait_logs_one_error_with_code_and_exact_message() {
    let _serial = WAIT_SERIAL.lock().await;
    for mode in WAIT_MODES {
        let text = cadence_wait_captured_run(mode, true).await;
        let errors: Vec<&str> = text
            .lines()
            .filter(|line| line.trim_start().starts_with("ERROR"))
            .collect();
        assert_eq!(errors.len(), 1, "{mode:?}: exactly one ERROR: {text}");
        assert!(errors[0].contains(WAIT_MESSAGE), "{mode:?}: {}", errors[0]);
        assert!(
            errors[0].contains("code=\"collector_cadence_invalid\""),
            "{mode:?}: {}",
            errors[0]
        );
        // Nothing of the credential or the data is in any line.
        assert!(!text.contains("refresh"), "{mode:?}: {text}");
    }
}
