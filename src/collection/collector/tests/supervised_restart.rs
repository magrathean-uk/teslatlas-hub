// SPDX-License-Identifier: AGPL-3.0-only

/// The cadence most collector tests run under: the Hub's own validated defaults
/// (C3 R16, R17, R21), the table the product ships, so a test of a rule runs under
/// the values the rule was written for. Each field keeps its R16 meaning
/// (`suspended` is the Suspended state-check interval). A test that needs another
/// value overrides that field where it needs it and says why.
fn test_cadence() -> CollectorCadence {
    crate::config::CollectorConfig::default()
        .cadence()
        .expect("the default cadence is valid")
}

fn supervised_restart_test_cadence() -> CollectorCadence {
    CollectorCadence {
        // Keep a run quiescent after its initial proof transaction. This
        // makes the competing-lease assertion distinguish its work from
        // a legitimate next scheduled poll.
        driving: Duration::from_secs(1),
        driving_stream: Duration::from_secs(1),
        charging: Duration::from_secs(1),
        online: Duration::from_secs(1),
        sleeping: Duration::from_secs(1),
        offline_drive_timeout: Duration::from_secs(1),
        suspended: Duration::from_secs(1),
        updating: Duration::from_secs(1),
        inventory: Duration::from_secs(1),
        // The fake sends its finite eight-frame burst then waits for the
        // collector's orderly unsubscribe. Keep this above initial setup
        // so a silence reconnect cannot race that proof.
        stream_health_timeout: Duration::from_secs(10),
        maximum_backoff: Duration::from_secs(1),
        fleet: crate::config::FleetCadence::DEFAULT,
    }
}

fn supervised_restart_test_config(data_dir: &std::path::Path) -> HubConfig {
    HubConfig {
        data_dir: data_dir.to_path_buf(),
        bind: "127.0.0.1:39191".parse().expect("loopback bind"),
        tls: None,
        http: Default::default(),
        pairing: Default::default(),
        collector: crate::config::CollectorConfig::default(),
        geocoder: crate::config::GeocoderConfig {
            enabled: false,
            ..crate::config::GeocoderConfig::default()
        },
        teslamate: crate::config::TeslaMateConfig::default(),
        terrain: TerrainConfig {
            cache_dir: Some(data_dir.join("terrain-cache")),
            ..TerrainConfig::default()
        },
    }
}

fn seed_supervised_restart_import(store: &HubStore) {
    use crate::teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history};
    use crate::teslamate_projection::{TeslaMateCar, TeslaMateHistory};

    let imported_at_ms = current_epoch_millis().expect("clock") - 60_000;
    let history = TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: crate::fake_tesla::FIXTURE_EID as i64,
            vid: Some(crate::fake_tesla::FIXTURE_VID as i64),
            vin: Some(crate::fake_tesla::FIXTURE_VIN.to_owned()),
            name: Some("Restart fixture".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: Some("74d".to_owned()),
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
        &CursorKey::from_bytes([0xD1; 32]),
        &TeslaMateImportRequest {
            source_key: "supervised-restart-fixture".to_owned(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms,
        },
        &history,
    )
    .expect("seed selected imported car");
}

/// Deadline for the shared supervised-restart waits. Each wait returns as soon
/// as it is satisfied, so only a loaded machine ever spends the whole bound.
const SUPERVISED_RESTART_WAIT: Duration = Duration::from_secs(30);

async fn join_supervised_restart_task(
    label: &str,
    task: &mut JoinHandle<Result<(), CollectorError>>,
) -> Result<(), CollectorError> {
    match timeout(SUPERVISED_RESTART_WAIT, &mut *task).await {
        Ok(result) => result.expect("supervised collector task join"),
        Err(_) => {
            task.abort();
            let _ = task.await;
            panic!("{label} timeout");
        }
    }
}

async fn wait_for_supervised_signal_or_abort<T>(
    label: &str,
    task: &mut JoinHandle<Result<(), CollectorError>>,
    signal: oneshot::Receiver<T>,
) -> T {
    match timeout(SUPERVISED_RESTART_WAIT, signal).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => {
            task.abort();
            let _ = task.await;
            panic!("{label} dropped");
        }
        Err(_) => {
            task.abort();
            let _ = task.await;
            panic!("{label} timeout");
        }
    }
}

async fn wait_for_supervised_restart_condition(label: &str, mut condition: impl FnMut() -> bool) {
    timeout(SUPERVISED_RESTART_WAIT, async {
        while !condition() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label} timeout"));
}

/// How the lease's renewal and release behave while a started publication drains (C3
/// R8, B2 clarification 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DrainCase {
    /// A recorded ordinary stop with the admission intact: renewal continues while
    /// the publication drains, and the lease is released after it commits.
    Ordinary,
    /// The retained admission was already denied when the run ended, and the run's
    /// result says so: the renewal is fenced and joined before the drain is waited
    /// for, the result is the sensitive failure and the lease is retained.
    DeniedAtEntry,
    /// A recorded ordinary stop whose admission is denied while the publication
    /// drains: the loss fences the renewal at once, remains decisive and retains the
    /// lease; the drain still completes.
    LostDuringDrain,
    /// An unrecorded stop (the renewal is joined at entry) whose admission is denied
    /// while the publication drains: the denial is seen although no renewal watches
    /// it, the result is the sensitive failure and the lease is retained.
    UnrecordedLostDuringDrain,
    /// As above, with the renewal already finished when the finaliser is entered.
    UnrecordedLostDuringDrainRenewalFinished,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_shutdown_drains_started_publication_before_lease_release() {
    assert_supervised_shutdown_drains_publication(DrainCase::Ordinary).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_shutdown_drain_preserves_sensitive_access_failure() {
    assert_supervised_shutdown_drains_publication(DrainCase::DeniedAtEntry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_shutdown_loss_during_publication_drain_stops_renewal_and_keeps_lease() {
    assert_supervised_shutdown_drains_publication(DrainCase::LostDuringDrain).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_shutdown_unrecorded_loss_during_publication_drain_keeps_sensitive_error() {
    assert_supervised_shutdown_drains_publication(DrainCase::UnrecordedLostDuringDrain).await;
    assert_supervised_shutdown_drains_publication(DrainCase::UnrecordedLostDuringDrainRenewalFinished)
        .await;
}

async fn assert_supervised_shutdown_drains_publication(case: DrainCase) {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
    let lease = store
        .acquire_supervised_collector_lease(current_epoch_millis().expect("clock"))
        .expect("collector lease");
    let initial = store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("active lease");
    let gate = store
        .acquire_publication_gate()
        .await
        .expect("publisher gate");
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let publisher_store = store.clone();
    let publisher = tokio::task::spawn_blocking(move || {
        let _gate = gate;
        started_tx.send(()).expect("publisher started");
        release_rx
            .recv_timeout(Duration::from_secs(15))
            .expect("release paused publisher");
        publisher_store
            .open()
            .expect("publisher catalogue")
            .execute(
                "INSERT INTO hub_metadata(key, value) VALUES ('shutdown-publication-test', 'committed')",
                [],
            )
            .expect("publication commit before gate release");
    });
    timeout(Duration::from_secs(2), started_rx)
        .await
        .expect("publisher starts")
        .expect("publisher signal");
    let (_collector_state, collector_state_rx) = watch::channel(SupervisedCollectorState::Active);
    let (heartbeat_shutdown, heartbeat_stop) = oneshot::channel();
    // The heartbeat of these cases does not look at the admission itself: whether
    // renewal stops is the finaliser's doing alone.
    let mut heartbeat = tokio::spawn(run_supervised_collector_heartbeat(
        store.clone(),
        lease,
        collector_state_rx,
        heartbeat_stop,
        Duration::from_millis(5),
    ));
    let heartbeat_at = || {
        store
            .supervised_collector_lease_status()
            .expect("lease query")
            .map(|status| status.heartbeat_at_ms)
    };
    let renewal_finished_at_entry = case == DrainCase::UnrecordedLostDuringDrainRenewalFinished;
    let mut heartbeat_shutdown = Some(heartbeat_shutdown);
    if renewal_finished_at_entry {
        // The renewal has ended and been joined before the finaliser is entered.
        let _ = heartbeat_shutdown.take().expect("sender").send(());
        (&mut heartbeat).await.expect("join").expect("an orderly end");
    }
    if case == DrainCase::DeniedAtEntry {
        invalidate_the_retained_admission(temporary.path());
    }
    let unrecorded = matches!(
        case,
        DrainCase::DeniedAtEntry
            | DrainCase::UnrecordedLostDuringDrain
            | DrainCase::UnrecordedLostDuringDrainRenewalFinished
    );
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let finish_store = store.clone();
    let finish_admission = Arc::clone(&admission);
    let mut shutdown = tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(
        Arc::clone(&seam),
        async move {
            finish_supervised_collector(
                &finish_store,
                lease,
                if case == DrainCase::DeniedAtEntry {
                    Err(CollectorError::SensitiveAccessUnavailable)
                } else {
                    Ok(())
                },
                &mut heartbeat,
                heartbeat_shutdown,
                renewal_finished_at_entry,
                if unrecorded {
                    StopRecord::Unrecorded
                } else {
                    StopRecord::Recorded
                },
                AdmissionLoss::default(),
                Some(&*finish_admission),
            )
            .await
        },
    ));
    let renewal_joined = |seam: &SupervisedCollectorTestSeam| {
        seam.loop_log().contains(&LoopEvent::RenewalJoined)
    };

    match case {
        DrainCase::Ordinary => {
            wait_for_supervised_restart_condition("heartbeat during publication drain", || {
                store
                    .supervised_collector_lease_status()
                    .expect("lease query")
                    .is_some_and(|status| {
                        status.instance_id == initial.instance_id
                            && status.heartbeat_at_ms > initial.heartbeat_at_ms
                    })
            })
            .await;
            assert!(!renewal_joined(&seam), "the renewal runs while the drain lasts");
        }
        DrainCase::DeniedAtEntry | DrainCase::UnrecordedLostDuringDrain => {
            // Fenced and joined before the drain is waited for: once the join is
            // acknowledged no renewal can follow, however the machine is loaded.
            wait_for_supervised_restart_condition("the renewal is joined", || {
                renewal_joined(&seam)
            })
            .await;
            let first = heartbeat_at();
            sleep(Duration::from_millis(150)).await;
            assert_eq!(heartbeat_at(), first, "no renewal after the join");
            if case == DrainCase::UnrecordedLostDuringDrain {
                invalidate_the_retained_admission(temporary.path());
            }
        }
        DrainCase::UnrecordedLostDuringDrainRenewalFinished => {
            assert!(!renewal_joined(&seam), "there was no renewal to join");
            invalidate_the_retained_admission(temporary.path());
        }
        DrainCase::LostDuringDrain => {
            // Renewal continues while the admission holds...
            wait_for_supervised_restart_condition("renewal while draining", || {
                heartbeat_at().is_some_and(|at| at > initial.heartbeat_at_ms)
            })
            .await;
            assert!(!renewal_joined(&seam));
            // ...and stops as soon as the loss is seen, with the drain still held:
            // the finaliser acknowledges the join, and no renewal follows it.
            invalidate_the_retained_admission(temporary.path());
            wait_for_supervised_restart_condition("the renewal is joined after the loss", || {
                renewal_joined(&seam)
            })
            .await;
            let first = heartbeat_at();
            sleep(Duration::from_millis(150)).await;
            assert_eq!(heartbeat_at(), first, "no renewal after the denial");
        }
    }
    assert!(!publisher.is_finished());
    assert!(!shutdown.is_finished(), "the finaliser waits for the publication");
    assert_eq!(
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT COUNT(*) FROM hub_metadata WHERE key = 'shutdown-publication-test'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("publication marker"),
        0
    );

    release_tx.send(()).expect("release publisher");
    timeout(Duration::from_secs(2), publisher)
        .await
        .expect("publisher completes")
        .expect("publisher join");
    let result = join_supervised_restart_task("shutdown after publication", &mut shutdown).await;
    match case {
        DrainCase::Ordinary => {
            result.expect("orderly shutdown");
            assert!(
                store
                    .supervised_collector_lease_status()
                    .expect("lease query")
                    .is_none(),
                "an ordinary recorded stop releases its lease"
            );
        }
        DrainCase::DeniedAtEntry
        | DrainCase::LostDuringDrain
        | DrainCase::UnrecordedLostDuringDrain
        | DrainCase::UnrecordedLostDuringDrainRenewalFinished => {
            assert!(
                matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
                "{result:?}"
            );
            let retained = store
                .supervised_collector_lease_status()
                .expect("lease query")
                .expect("the lease is retained to expire");
            assert_eq!(retained.instance_id, initial.instance_id);
            assert_eq!(
                Some(retained.heartbeat_at_ms),
                heartbeat_at(),
                "nothing renews or releases after the denial"
            );
            assert_eq!(lease_state_text(&store), "active");
        }
    }
    assert_eq!(
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = 'shutdown-publication-test'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("publication marker"),
        "committed",
        "the publication drain completes in every case"
    );
}

/// Move the data directory's lifetime lock file aside and put a different file in its
/// place: the retained admission fails its next revalidation. [`restore_the_admission_lock`]
/// puts the original back, so the same admission passes again.
fn swap_the_admission_lock_away(data_dir: &std::path::Path) {
    use std::os::unix::fs::OpenOptionsExt;
    let lock = data_dir.join(crate::user_lifetime_lock::LOCK_FILE_NAME);
    std::fs::rename(&lock, data_dir.join("lock-moved-aside")).expect("move the lock aside");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock)
        .expect("a different lock file");
}

fn restore_the_admission_lock(data_dir: &std::path::Path) {
    let lock = data_dir.join(crate::user_lifetime_lock::LOCK_FILE_NAME);
    std::fs::remove_file(&lock).expect("remove the stand-in");
    std::fs::rename(data_dir.join("lock-moved-aside"), &lock).expect("restore the lock");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_observed_admission_loss_fences_renewal_after_revalidation_recovers() {
    // The supervisor observes the loss of the admission (its revalidation fails), and
    // the admission then passes again (the lock file is put back). The renewal's own
    // revalidation would now pass, but the observed loss is irreversible: the renewal
    // ends with the sensitive failure at its next step and writes nothing more. The
    // heartbeat here has no admission of its own to revalidate, so only the shared
    // fence can stop it.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
    let lease = store
        .acquire_supervised_collector_lease(current_epoch_millis().expect("clock"))
        .expect("collector lease");
    let initial = store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("active lease");
    let (_collector_state, collector_state_rx) = watch::channel(SupervisedCollectorState::Active);
    let (_heartbeat_shutdown, heartbeat_stop) = oneshot::channel();
    let fence = AdmissionFence::default();
    let mut heartbeat = tokio::spawn(run_supervised_collector_heartbeat_with(
        store.clone(),
        lease,
        collector_state_rx,
        heartbeat_stop,
        Duration::from_millis(5),
        Arc::new(SystemCollectorClock),
        None,
        fence.clone(),
    ));
    let heartbeat_at = || {
        store
            .supervised_collector_lease_status()
            .expect("lease query")
            .map(|status| status.heartbeat_at_ms)
    };
    wait_for_supervised_restart_condition("the first renewal", || {
        heartbeat_at().is_some_and(|at| at > initial.heartbeat_at_ms)
    })
    .await;
    // The supervisor's observation: a failed revalidation, then recovery.
    swap_the_admission_lock_away(temporary.path());
    let mut loss = AdmissionLoss::with_fence(fence);
    assert!(loss.check(Some(&*admission)), "the loss is observed");
    restore_the_admission_lock(temporary.path());
    assert!(
        assert_runtime_sensitive_access(Some(&*admission)).is_ok(),
        "the admission passes again"
    );
    assert!(loss.observed(), "the loss stays observed");
    let ended = timeout(SUPERVISED_RESTART_WAIT, &mut heartbeat)
        .await
        .expect("the renewal ends at its next step")
        .expect("heartbeat join");
    assert!(
        matches!(ended, Err(CollectorError::SensitiveAccessUnavailable)),
        "{ended:?}"
    );
    let after = heartbeat_at();
    sleep(Duration::from_millis(100)).await;
    assert_eq!(heartbeat_at(), after, "nothing renews after the observed loss");
}

/// Where the fake places the Owner samples of these tests: 15 minutes before now, so every
/// timestamp is plausible (C4 R20a) and older than the next real clock reading.
const SUPERVISED_RESTART_BASE_AGE_MS: i64 = 900_000;

/// Gives the stream a session that receives frames, once the first Owner sample is in.
///
/// The collector starts a car's stream after its first collection, and the supervisor's
/// first attempt then waits its start jitter (C4 R42) and takes the credential lock (R6),
/// so the stream's first session always starts after the first Owner sample. The fake
/// places a session's frames strictly between the last Owner sample it returned and the next
/// one, and sends none when the two are the same, as they are after a no-op sample. A silent
/// session is ended only by the data-silence rule (R30), which for a car that has never shown
/// motion waits 300 s (`H_park`), far beyond these tests' wait. So, while the collector is
/// held after its first collection, the scenario forgets the Owner sample (setting the base
/// time again opens the gap, as it was before any sample) and withholds further Owner samples,
/// which would close the gap again before the session starts. The caller restores them with
/// [`restore_owner_samples_after_stream_proof`] once frames have been stored.
fn open_stream_window_after_first_owner_sample(
    fake: &crate::fake_tesla::FakeTeslaSource,
    base_ts_ms: i64,
) {
    fake.set_owner_vehicle_data_available(false);
    fake.set_base_ts_ms(base_ts_ms);
}

fn restore_owner_samples_after_stream_proof(fake: &crate::fake_tesla::FakeTeslaSource) {
    fake.set_owner_vehicle_data_available(true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imported_legacy_pair_refreshes_then_collects_one_car_and_survives_reopen() {
    use crate::{
        credentials::{LegacyAuthManager, OwnerTokens},
        fake_tesla::{AdvanceMode, FAKE_REFRESHED_ACCESS_TOKEN, FakeTeslaSource},
        owner_api::OwnerApi,
    };

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);

    crate::teslamate_credentials::replace_key(temporary.path(), b"chain-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial =
        OwnerTokens::from_secret_parts("initial-access".to_owned(), "initial-refresh".to_owned())
            .expect("initial pair");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("Cloak initial pair");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, 2, 1)
                .expect("due refresh schedule"),
        )
        .expect("store encrypted pair");

    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    fake.set_step(crate::fake_tesla::ScenarioStep::UnchangedNoOp);
    let base_ts_ms = current_epoch_millis().expect("clock") - SUPERVISED_RESTART_BASE_AGE_MS;
    fake.set_base_ts_ms(base_ts_ms);
    let manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        temporary.path(),
        fake.oauth_issuer_url(),
    )
    .expect("load encrypted legacy pair");
    let region = manager.region();
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region,
    };
    let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
        .expect("owner client");
    let config = supervised_restart_test_config(temporary.path());
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let (finished, resume) = seam.arm_paused_collection_completion().await;
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task_store = store.clone();
    let task_config = config.clone();
    let stream_endpoint = fake.stream_endpoint().to_owned();
    let mut task = tokio::spawn(async move {
        SUPERVISED_COLLECTOR_TEST_SEAM
            .scope(seam, async move {
                run_supervised_with_access(
                    &task_store,
                    &task_config,
                    supervised_restart_test_cadence(),
                    client,
                    auth,
                    stream_endpoint,
                    CursorKey::from_bytes([0xC1; 32]),
                    Some(ready_tx),
                    RunWriteGuard::for_run(None),
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
            })
            .await
    });
    let _cursor =
        wait_for_supervised_signal_or_abort("collector readiness", &mut task, ready_rx).await;
    let _report =
        wait_for_supervised_signal_or_abort("first collection", &mut task, finished).await;
    assert_eq!(
        fake.token_refresh_request_count(),
        1,
        "one startup OAuth refresh"
    );
    assert!(fake.audited_requests().iter().any(|request| {
        request.path
            == format!(
                "/api/1/vehicles/{}/vehicle_data",
                crate::fake_tesla::FIXTURE_EID
            )
    }));

    open_stream_window_after_first_owner_sample(&fake, base_ts_ms);
    resume.send(()).expect("resume stream drain");
    wait_for_supervised_restart_condition("numeric stream persistence", || {
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT COUNT(*) FROM current_observations
                     WHERE record_type = 'tesla_stream_update_v1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("stream count")
            > 0
    })
    .await;
    restore_owner_samples_after_stream_proof(&fake);
    shutdown_tx.send(()).expect("collector shutdown");
    join_supervised_restart_task("collector join", &mut task)
        .await
        .expect("collector result");
    wait_for_supervised_restart_condition("stream socket shutdown", || {
        fake.stream_session_stats().active_sessions == 0
    })
    .await;
    assert!(
        fake.audited_stream_events()
            .iter()
            .any(|event| { event.event == crate::fake_tesla::StreamAuditEvent::Unsubscribe })
    );

    let stored = store
        .load_teslamate_legacy_tokens()
        .expect("stored pair")
        .expect("stored pair exists");
    let successor = crate::teslamate_token::decrypt_legacy_owner_tokens(
        key.as_bytes(),
        stored.access(),
        stored.refresh(),
    )
    .expect("decrypt Cloak successor");
    assert_eq!(successor.access_token(), FAKE_REFRESHED_ACCESS_TOKEN);
    assert!(stored.next_refresh_at() > 0 && stored.next_refresh_at() < stored.expires_at());

    drop(store);
    let reopened_store = HubStore::initialize(temporary.path()).expect("reopen Hub");
    let vehicle_count: i64 = reopened_store
        .open()
        .expect("reopened catalogue")
        .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
        .expect("one selected vehicle");
    assert_eq!(vehicle_count, 1);
    let reopened = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        reopened_store,
        temporary.path(),
        fake.oauth_issuer_url(),
    )
    .expect("reopen latest pair");
    assert_eq!(reopened.access_token(), FAKE_REFRESHED_ACCESS_TOKEN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_collects_and_reconnects_without_refreshing() {
    use crate::{
        credentials::{LegacyAuthManager, OwnerTokens},
        fake_tesla::{AdvanceMode, FakeTeslaSource},
        owner_api::OwnerApi,
    };

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("loopback Tesla");
    let setup_auth = LegacyAuth::for_test(
        fake.oauth_issuer_url(),
        "observer-access",
        "observer-refresh",
    );
    let setup_client =
        OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
            .expect("setup Owner client");
    setup_native_vehicle_with_client(&store, temporary.path(), &setup_client, &setup_auth, None)
        .await
        .expect("native setup");
    crate::teslamate_credentials::replace_key(temporary.path(), b"observer-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial =
        OwnerTokens::from_secret_parts("observer-access".to_owned(), "observer-refresh".to_owned())
            .expect("initial pair");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("Cloak initial pair");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, 2, 1)
                .expect("due refresh schedule"),
        )
        .expect("store encrypted pair");

    fake.set_step(crate::fake_tesla::ScenarioStep::UnchangedNoOp);
    let base_ts_ms = current_epoch_millis().expect("clock") - SUPERVISED_RESTART_BASE_AGE_MS;
    fake.set_base_ts_ms(base_ts_ms);
    let manager = LegacyAuthManager::from_hub_teslamate_store_observer_with_issuer(
        store.clone(),
        temporary.path(),
        fake.oauth_issuer_url(),
    )
    .expect("load observer pair");
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: false,
        region: StreamRegion::Global,
    };
    let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
        .expect("Owner client");
    let config = supervised_restart_test_config(temporary.path());
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let (finished, resume) = seam.arm_paused_collection_completion().await;
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task_store = store.clone();
    let task_config = config.clone();
    let stream_endpoint = fake.stream_endpoint().to_owned();
    let mut task = tokio::spawn(async move {
        SUPERVISED_COLLECTOR_TEST_SEAM
            .scope(seam, async move {
                run_supervised_with_access(
                    &task_store,
                    &task_config,
                    supervised_restart_test_cadence(),
                    client,
                    auth,
                    stream_endpoint,
                    CursorKey::from_bytes([0xC2; 32]),
                    Some(ready_tx),
                    RunWriteGuard::for_run(None),
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
            })
            .await
    });
    let _cursor =
        wait_for_supervised_signal_or_abort("observer readiness", &mut task, ready_rx).await;
    let _report =
        wait_for_supervised_signal_or_abort("observer first collection", &mut task, finished).await;
    assert_eq!(fake.token_refresh_request_count(), 0);
    assert!(fake.audited_requests().iter().any(|request| {
        request.path
            == format!(
                "/api/1/vehicles/{}/vehicle_data",
                crate::fake_tesla::FIXTURE_EID
            )
    }));

    open_stream_window_after_first_owner_sample(&fake, base_ts_ms);
    resume.send(()).expect("resume observer stream drain");
    wait_for_supervised_restart_condition("observer stream persistence", || {
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT COUNT(*) FROM current_observations
                     WHERE record_type = 'tesla_stream_update_v1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("stream count")
            > 0
    })
    .await;
    restore_owner_samples_after_stream_proof(&fake);
    let attempts_before = fake.stream_session_stats().connection_attempts;
    fake.set_stream_available(false);
    wait_for_supervised_restart_condition("observer stream reconnect attempt", || {
        fake.stream_session_stats().connection_attempts > attempts_before
    })
    .await;
    fake.set_stream_available(true);
    assert_eq!(fake.token_refresh_request_count(), 0);
    shutdown_tx.send(()).expect("observer shutdown");
    join_supervised_restart_task("observer collector join", &mut task)
        .await
        .expect("observer collector result");
    assert_eq!(fake.token_refresh_request_count(), 0);
}


// ---- B2 P3: the production entry across restarts (C3 R7, R8; C5 R57) ---------------------------
//
// The tests below run `run_supervised_for_admitted_user`, the supervised start a real
// process uses, over a clean test-local Owner API double on loopback (Provider mode)
// or over a credential unit that is stopped by construction (Legacy, with an unresolved
// attempt; Fleet, with an identified unresolved refresh), so no request ever leaves
// the machine. Stream input, where a test wants it, is injected through the seam's
// bounded queue.

/// A recording Owner API double: every request of any method and path is logged, the
/// account list and the car read answer with the status a test sets (a 403 names the
/// Fleet API, as Tesla's refusal does).
#[derive(Clone, Default)]
struct P3Wire {
    requests: Arc<Mutex<Vec<String>>>,
    /// The answer to the list and to the read: 0 is a success.
    list_status: Arc<AtomicUsize>,
    read_status: Arc<AtomicUsize>,
}

impl P3Wire {
    fn seen(&self) -> Vec<String> {
        self.requests.lock().expect("requests").clone()
    }

    fn count(&self, needle: &str) -> usize {
        self.seen().iter().filter(|line| line.contains(needle)).count()
    }
}

async fn p3_wire_answer(
    State(wire): State<P3Wire>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    wire.requests
        .lock()
        .expect("requests")
        .push(format!("{method} {path}"));
    let now = current_epoch_millis().expect("clock");
    let (status, success) = if method == axum::http::Method::GET && path == "/api/1/products" {
        (
            wire.list_status.load(Ordering::SeqCst),
            json!({"response": [{
                "id": 6_100_001, "vehicle_id": 6_200_001, "vin": "5YJ3E1EA7KF610001",
                "state": "online", "display_name": "Cadence wait fixture", "in_service": false
            }], "count": 1}),
        )
    } else if method == axum::http::Method::GET && path.ends_with("/vehicle_data") {
        (
            wire.read_status.load(Ordering::SeqCst),
            json!({"response": {
                "drive_state": {"timestamp": now},
                "charge_state": {"battery_level": 80, "timestamp": now}
            }}),
        )
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match status {
        0 | 200 => (StatusCode::OK, success.to_string()).into_response(),
        403 => (StatusCode::FORBIDDEN, r#"{"error":"use the Fleet API"}"#).into_response(),
        other => StatusCode::from_u16(u16::try_from(other).expect("status"))
            .expect("a status")
            .into_response(),
    }
}

/// The double on a loopback port, and its address.
async fn p3_wire() -> (P3Wire, std::net::SocketAddr, JoinHandle<()>) {
    let wire = P3Wire::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().fallback(p3_wire_answer).with_state(wire.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("Owner API double");
    });
    (wire, address, server)
}

/// A Provider configuration whose Owner API is the double and whose token endpoint is
/// a loopback port nothing listens on (a not-due pair never reaches it).
fn p3_provider_config(directory: &std::path::Path, address: &std::net::SocketAddr) -> HubConfig {
    let mut config = supervised_restart_test_config(directory);
    config.collector.owner_api_base_url = Some(format!("https://provider.example.test:{}/", address.port()));
    config.collector.legacy_auth.token_url = Some("https://127.0.0.1:9/oauth2/v3/token".to_owned());
    config
}

/// A pair that no external writer gave a generation (NULL), in the refreshed form so
/// no token request is due for a long while.
fn p3_store_null_generation_pair(store: &HubStore, data_dir: &std::path::Path, due: bool) {
    let key = crate::teslamate_credentials::load_key(data_dir).unwrap_or_else(|_| {
        crate::teslamate_credentials::replace_key(data_dir, b"p3-cloak-key").expect("0600 Cloak key");
        crate::teslamate_credentials::load_key(data_dir).expect("Cloak key")
    });
    let tokens = OwnerTokens::from_secret_parts("provider-access-0".to_owned(), "provider-refresh-0".to_owned())
        .expect("a pair");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens).expect("cipher");
    let (expires, next) = not_due_schedule();
    // A due pair (its access token still usable) is where a managed unit would
    // refresh and an observer must not.
    let next = if due { epoch_seconds_of(SystemTime::now()) - 100 } else { next };
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, expires, next)
                .expect("a schedule"),
        )
        .expect("store the pair");
}

/// What a production entry builds its Tesla-facing clients against in a test. Every
/// client comes from a loopback double through the production construction seam
/// (`run_supervised_with_clients_for_admitted_user` and its observer counterpart);
/// a client a scenario does not expect the run to build panics instead, so no test
/// can reach a Tesla host, whatever a fence or a stop does.
#[derive(Clone, Debug)]
enum P3Entrypoint {
    /// The supervised start of the admitted Serve: the Owner API client of the
    /// double at `owner` and the Fleet clients of the double at `fleet`, where given.
    Supervised {
        owner: Option<url::Url>,
        fleet: Option<url::Url>,
    },
    /// The observer start: the Owner API client of the double at `owner`.
    Observer { owner: url::Url },
}

/// The Fleet clients of a loopback double.
fn p3_fleet_clients(base: &url::Url) -> Result<FleetClients, CollectorError> {
    let timeout = Duration::from_secs(5);
    Ok(FleetClients {
        api: FleetApi::for_fake_http(base.clone(), timeout)?,
        auth_api: FleetAuthApi::for_fake_http(base.clone(), timeout)?,
        command_proxy: None,
    })
}

/// A running production entry and the way to stop it.
struct P3Entry {
    task: JoinHandle<Result<(), CollectorError>>,
    stop: Option<oneshot::Sender<()>>,
    seam: Arc<SupervisedCollectorTestSeam>,
}

impl P3Entry {
    /// Start the full production entry and wait for its handoff, for a run that
    /// builds no Tesla-facing client at all: a cadence configuration wait.
    async fn start(store: &HubStore, config: &HubConfig) -> Self {
        Self::spawn(
            store,
            config,
            P3Entrypoint::Supervised {
                owner: None,
                fleet: None,
            },
            Arc::new(SupervisedCollectorTestSeam::default()),
        )
        .await
    }

    /// Start the production entry of a Fleet run whose clients are those of the
    /// loopback double at `fleet` (a run over an unresolved refresh sends nothing; the
    /// double records anything that arrives).
    async fn start_fleet(store: &HubStore, config: &HubConfig, fleet: &url::Url) -> Self {
        Self::spawn(
            store,
            config,
            P3Entrypoint::Supervised {
                owner: None,
                fleet: Some(fleet.clone()),
            },
            Arc::new(SupervisedCollectorTestSeam::default()),
        )
        .await
    }

    /// Start the production Owner-wire collector of the admitted Serve, which builds its
    /// credential unit from the store as Serve does, with its Owner API client pointed at
    /// the loopback double at `base`.
    async fn start_owner_wire(
        store: &HubStore,
        config: &HubConfig,
        base: &url::Url,
        seam: Arc<SupervisedCollectorTestSeam>,
    ) -> Self {
        Self::spawn(
            store,
            config,
            P3Entrypoint::Supervised {
                owner: Some(base.clone()),
                fleet: None,
            },
            seam,
        )
        .await
    }

    /// Start the production observer collector (`run_observer_for_admitted_user`'s
    /// body) against the loopback double at `base`.
    async fn start_observer(
        store: &HubStore,
        config: &HubConfig,
        base: &url::Url,
        seam: Arc<SupervisedCollectorTestSeam>,
    ) -> Self {
        Self::spawn(
            store,
            config,
            P3Entrypoint::Observer { owner: base.clone() },
            seam,
        )
        .await
    }

    async fn spawn(
        store: &HubStore,
        config: &HubConfig,
        entry: P3Entrypoint,
        seam: Arc<SupervisedCollectorTestSeam>,
    ) -> Self {
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(&config.data_dir)
            .expect("admission");
        let (ready_tx, ready_rx) = oneshot::channel();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let task_store = store.clone();
        let task_config = config.clone();
        let task_seam = Arc::clone(&seam);
        let mut task = tokio::spawn(async move {
            SUPERVISED_COLLECTOR_TEST_SEAM
                .scope(task_seam, async move {
                    let shutdown = async move {
                        let _ = stop_rx.await;
                    };
                    let owner_client = |base: Option<url::Url>| {
                        move |_| match base {
                            Some(base) => Ok(OwnerApi::for_fake_http(base, Duration::from_secs(5))?),
                            None => panic!("an Owner API client was built where none is expected"),
                        }
                    };
                    let fleet_clients = |base: Option<url::Url>| {
                        move |_| match base {
                            Some(base) => p3_fleet_clients(&base),
                            None => panic!("Fleet clients were built where none are expected"),
                        }
                    };
                    match entry {
                        P3Entrypoint::Supervised { owner, fleet } => {
                            run_supervised_with_clients_for_admitted_user(
                                &task_store,
                                &task_config,
                                admission,
                                ready_tx,
                                owner_client(owner),
                                fleet_clients(fleet),
                                shutdown,
                            )
                            .await
                        }
                        P3Entrypoint::Observer { owner } => {
                            run_observer_with_clients_for_admitted_user(
                                &task_store,
                                &task_config,
                                admission,
                                ready_tx,
                                owner_client(Some(owner)),
                                fleet_clients(None),
                                shutdown,
                            )
                            .await
                        }
                    }
                })
                .await
        });
        match timeout(SUPERVISED_RESTART_WAIT, ready_rx).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                let result = (&mut task).await;
                panic!("the production entry ended before its handoff: {result:?}");
            }
            Err(_) => {
                task.abort();
                panic!("production entry handoff timeout");
            }
        }
        Self {
            task,
            stop: Some(stop_tx),
            seam,
        }
    }

    /// Wait for a condition, failing at once if the run ended first.
    async fn until(&mut self, label: &str, mut condition: impl FnMut() -> bool) {
        let task = &mut self.task;
        timeout(SUPERVISED_RESTART_WAIT, async {
            while !condition() {
                assert!(!task.is_finished(), "{label}: the run ended first");
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{label} timeout"));
    }

    /// Wait until the loop has made at least one complete iteration and settings
    /// refresh, so what it would have sent has been sent.
    async fn settled(&mut self) {
        let seam = Arc::clone(&self.seam);
        self.until("a settled loop", || {
            seam.loop_iterations() >= 1 && seam.settings_refreshes_completed() >= 1
        })
        .await;
        sleep(Duration::from_millis(300)).await;
    }

    async fn stop(mut self) -> Result<(), CollectorError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        join_supervised_restart_task("production entry shutdown", &mut self.task).await
    }
}

fn p3_car_row(store: &HubStore) -> Option<crate::db::CollectorVehicleRow> {
    store.load_collector_state_rows().expect("rows").vehicles.into_iter().next()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3_null_generation_keeps_identity_across_restarts() {
    p3_null_generation_restarts(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3_null_generation_observer_keeps_identity_across_restarts() {
    // The refresh-owner-elsewhere configuration, where an unbound pair actually
    // occurs: the observer constructor binds before the fingerprint too.
    p3_null_generation_restarts(true).await;
}

/// The credential unit is built (and binds an unbound pair's generation) before the
/// start computes its fingerprint, so a pair that no external writer gave a
/// generation has one identity from the first start on: a refusal and a hold that
/// real requests produced survive two restarts, and the restarts send nothing. The
/// managed and the observer constructions are both run through their production
/// entry.
async fn p3_null_generation_restarts(observer: bool) {
    let start = |store: &HubStore, config: &HubConfig, base: &url::Url| {
        let (store, config, base) = (store.clone(), config.clone(), base.clone());
        async move {
            let seam = Arc::new(SupervisedCollectorTestSeam::default());
            if observer {
                P3Entry::start_observer(&store, &config, &base, seam).await
            } else {
                P3Entry::start_owner_wire(&store, &config, &base, seam).await
            }
        }
    };
    let _serial = WAIT_SERIAL.lock().await;
    crate::crypto::install_default_provider();
    let (temporary, store) = cadence_wait_hub();
    let dir = temporary.path().to_path_buf();
    p3_store_null_generation_pair(&store, &dir, observer);
    assert_eq!(
        store.stored_legacy_credential_generation().expect("generation"),
        crate::db::StoredLegacyGeneration::Unbound
    );
    assert_eq!(store.owner_wire_credential_identity().expect("identity"), "", "NULL identity before any binding");
    let (wire, address, server) = p3_wire().await;
    let config = p3_provider_config(&dir, &address);
    let base = url::Url::parse(&format!("http://{address}/")).expect("base");
    assert_eq!(
        crate::config::collector_mode(&config.collector),
        crate::config::CollectorMode::Provider
    );

    // First start: the car's read is refused (a Fleet-named 403): the actual refusal
    // pauses the car.
    wire.read_status.store(403, Ordering::SeqCst);
    let mut run = start(&store, &config, &base).await;
    run.until("the car's refusal is stored", || {
        p3_car_row(&store).is_some_and(|row| row.hold == "paused")
    })
    .await;
    run.stop().await.expect("an orderly stop");
    let crate::db::StoredLegacyGeneration::Bound(generation) = store.stored_legacy_credential_generation().expect("generation")
    else {
        panic!("the unit's construction bound the generation");
    };
    let identity = store.owner_wire_credential_identity().expect("identity");
    assert_eq!(identity, generation.to_string());
    let fingerprint = collector_config_fingerprint(
        crate::config::CollectorMode::Provider,
        &crate::config::owner_api_origin_text(&config.collector),
        &config
            .collector
            .stream_endpoint(crate::tesla_stream::StreamRegion::Global)
            .expect("the stream endpoint"),
        &identity,
    );
    let rows = store.load_collector_state_rows().expect("rows");
    assert_eq!(
        rows.account.as_ref().map(|account| account.config_fingerprint.clone()),
        Some(fingerprint.clone()),
        "bound before the fingerprint: the stored fingerprint carries the bound identity"
    );
    assert_eq!(p3_car_row(&store).expect("car").reason_code, "paused_access_refused");

    // Second start: the account's list is refused too (a Fleet-named 403): the account
    // is refused, and the hold stands.
    wire.list_status.store(403, Ordering::SeqCst);
    let mut run = start(&store, &config, &base).await;
    run.until("the account's refusal is stored", || {
        store
            .load_collector_state_rows()
            .expect("rows")
            .account
            .is_some_and(|account| account.state == "provider_refused")
    })
    .await;
    run.stop().await.expect("an orderly stop");
    let refused = store.load_collector_state_rows().expect("rows");
    let account = refused.account.clone().expect("account");
    assert_eq!(account.state, "provider_refused");
    assert_eq!(account.config_fingerprint, fingerprint);
    let hold = p3_car_row(&store).expect("car");
    assert_eq!((hold.hold.as_str(), hold.reason_code.as_str()), ("paused", "paused_access_refused"));

    // Two restarts with the same configuration: identity, refusal and hold are
    // stable, and nothing new reaches the double.
    let traffic = wire.seen();
    for restart in 1..=2 {
        let mut run = start(&store, &config, &base).await;
        run.settled().await;
        run.stop().await.expect("an orderly stop");
        assert_eq!(wire.seen(), traffic, "restart {restart}: no new traffic");
        assert!(
            store.legacy_token_receipts_for_test().expect("receipts").is_empty(),
            "restart {restart}: no token request was ever made (observer: {observer})"
        );
        assert_eq!(store.owner_wire_credential_identity().expect("identity"), identity, "restart {restart}");
        let rows = store.load_collector_state_rows().expect("rows");
        let account_now = rows.account.expect("account");
        assert_eq!(
            (account_now.state.as_str(), account_now.config_fingerprint.as_str()),
            ("provider_refused", fingerprint.as_str()),
            "restart {restart}: the refusal stands"
        );
        let hold_now = p3_car_row(&store).expect("car");
        assert_eq!(
            (hold_now.hold.as_str(), hold_now.reason_code.as_str()),
            ("paused", "paused_access_refused"),
            "restart {restart}: the hold stands"
        );
        assert_eq!(wire.count("vehicle_data"), 1, "restart {restart}: the one refused read only");
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3_failure_after_commit_keeps_durable_lift() {
    // A fault after the start's transaction committed and before it reads anything
    // else makes the start fail, but the durable lift stands: a second handle sees the
    // neutral C5 row and C3's reset, a retry restores no refusal, and nothing claims
    // readiness without a lease.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(Arc::clone(&seam), async {
            let (_fake, base, _client, server) = auth_fake().await;
            let token_url = base.join("oauth2/v3/token").expect("token endpoint");
            let (temporary, store) = cadence_wait_hub();
            let dir = temporary.path().to_path_buf();
            let g0 = ct72_owner_stores_credentials(&store, &dir, "owner-access-0", "owner-refresh-0");
            let config = p3_config(&dir, P3Config::Legacy);
            let fingerprint = p3_fingerprint(&config, P3Config::Legacy, g0);
            p3_seed_c3(&store, "legacy", &fingerprint, "owner_api_refused");
            p3_seed_c5_refusal(&store, "legacy");
            // The owner stores new credentials: the next start lifts the refusal.
            ct72_owner_stores_credentials(&store, &dir, "owner-access-1", "owner-refresh-1");
            let unit = ct72_unit(&store, &dir, crate::legacy_auth::AuthMode::Tesla, &token_url);

            seam.set_start_commit_fault(true);
            let error = p3_start(&store, &config).err().expect("the start fails after its commit");
            assert!(
                matches!(&error, CollectorError::Store(StoreError::SyncMutation(text)) if text.contains("after commit")),
                "{error:?}"
            );
            seam.set_start_commit_fault(false);
            // A second handle sees the committed lift and C3's reset.
            let second = HubStore::open_existing(&dir).expect("a second handle");
            let auth = second.load_collector_auth_row().expect("auth row").expect("a row");
            assert_eq!((auth.state.as_str(), auth.collecting, auth.stopped_mode), ("ready", true, None));
            let account = second.load_collector_state_rows().expect("rows").account.expect("account");
            assert_eq!((account.state.as_str(), account.reason_code.as_str()), ("collecting", "config_changed"));
            // No readiness is claimed without a lease: the lift proves no start.
            assert!(
                second.service_readiness_at(true, current_epoch_millis().expect("clock")).is_err(),
                "no unconditional readiness claim"
            );
            assert!(second.supervised_collector_lease_status().expect("lease").is_none());
            // The failed start ended the run, and its unit with it. The next process
            // builds a unit over the store as it now stands: the neutral row restores no
            // refusal, and the retried start (which has nothing left to lift) adds none.
            drop(unit);
            let mut rebuilt = ct72_unit(&store, &dir, crate::legacy_auth::AuthMode::Tesla, &token_url);
            assert_ne!(ct72_state_of(&rebuilt), "api_refused");
            let retried = p3_start(&store, &config).expect("the retry");
            assert!(!retried.refusal_reset, "nothing is left to lift");
            rebuilt.apply_collector_start(&retried.credential_view());
            assert_ne!(ct72_state_of(&rebuilt), "api_refused", "the old refusal does not return");
            assert_eq!(second.load_collector_auth_row().expect("auth row").expect("a row").state, "ready");
            server.abort();
        })
        .await;
}

/// A hub for the wait-then-restart scenarios: an imported car, a stored Owner pair (not
/// due), a sound Fleet pair with an identified unresolved refresh (so a Fleet run
/// starts without sending anything), C3's standing Legacy refusal under the real
/// Legacy fingerprint, and C5's own matching refusal.
fn p3_wait_hub() -> (tempfile::TempDir, HubStore, HubConfig, HubConfig) {
    let (temporary, store) = cadence_wait_hub();
    let dir = temporary.path().to_path_buf();
    let g0 = ct72_owner_stores_credentials(&store, &dir, "owner-access-0", "owner-refresh-0");
    p3_add_unresolved_fleet_refresh(&store, &dir);
    let mut legacy = supervised_restart_test_config(&dir);
    legacy.collector.stream_region = Some(crate::tesla_stream::StreamRegion::Global);
    let mut fleet = supervised_restart_test_config(&dir);
    fleet.collector.provider = CollectorProvider::Fleet;
    let fingerprint = collector_config_fingerprint(
        crate::config::CollectorMode::Legacy,
        &crate::config::owner_api_origin_text(&legacy.collector),
        &legacy
            .collector
            .stream_endpoint(crate::tesla_stream::StreamRegion::Global)
            .expect("the stream endpoint"),
        &g0.to_string(),
    );
    p3_seed_c3(&store, "legacy", &fingerprint, "owner_api_refused");
    p3_seed_c5_refusal(&store, "legacy");
    (temporary, store, legacy, fleet)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3_cadence_wait_defers_refusal_lift_until_validated_start() {
    // A Legacy refusal stands. The owner then configures Fleet polling with a cadence
    // the configuration breaks: the managed wait is not an ordinary start, so it lifts
    // nothing and every row stays as it was. The owner then either corrects the cadence,
    // and the ordinary Fleet start lifts the refusal, or returns to Legacy before any
    // ordinary Fleet start, and the Legacy start retains the refusal and sends nothing.
    let _serial = WAIT_SERIAL.lock().await;
    crate::crypto::install_default_provider();
    for corrected_fleet_first in [true, false] {
        let (_temporary, store, legacy, fleet) = p3_wait_hub();
        let mut broken = fleet.clone();
        broken.collector.request_timeout_seconds = 121;
        assert!(broken.collector.cadence_violation().is_some());
        let rows_before = cadence_wait_state_dump(&store);
        let c5_before = store.load_collector_auth_row().expect("auth row");
        assert_eq!(c5_before.as_ref().map(|row| row.state.as_str()), Some("api_refused"));
        // The wait, through the production entry.
        let waiting = P3Entry::start(&store, &broken).await;
        sleep(Duration::from_millis(300)).await;
        waiting.stop().await.expect("an orderly wait");
        assert_eq!(
            cadence_wait_state_dump(&store),
            rows_before,
            "the wait changed no account, vehicle, count, action or credential row"
        );
        assert_eq!(store.load_collector_auth_row().expect("auth row"), c5_before);
        if corrected_fleet_first {
            // The corrected ordinary Fleet start lifts the refusal (a Fleet start sends
            // nothing here: its pair is fenced by an unresolved refresh). Its Fleet
            // clients are those of a recording loopback double, which must stay empty.
            let (wire, address, server) = p3_wire().await;
            let fleet_base = url::Url::parse(&format!("http://{address}/")).expect("base");
            let mut run = P3Entry::start_fleet(&store, &fleet, &fleet_base).await;
            run.settled().await;
            assert!(wire.seen().is_empty(), "no Fleet traffic: {:?}", wire.seen());
            let account = store.load_collector_state_rows().expect("rows").account.expect("account");
            assert_eq!(
                (account.mode.as_str(), account.state.as_str(), account.reason_code.as_str()),
                ("fleet_polling", "collecting", "config_changed"),
                "the ordinary Fleet start reset the account"
            );
            let lifted = store.load_collector_auth_row().expect("auth row").expect("a row");
            assert_eq!((lifted.state.as_str(), lifted.collecting, lifted.stopped_mode), ("ready", true, None));
            run.stop().await.expect("an orderly stop");
            assert!(wire.seen().is_empty(), "no Fleet traffic: {:?}", wire.seen());
            server.abort();
        } else {
            // Back to Legacy before an ordinary Fleet start: the Legacy start restores
            // and retains the refusal, and nothing reaches the Owner API double.
            let (wire, address, server) = p3_wire().await;
            let base = url::Url::parse(&format!("http://{address}/")).expect("base");
            let seam = Arc::new(SupervisedCollectorTestSeam::default());
            let stats = Arc::new(ProducerStats::default());
            seam.inject_stream_runs(telemetry_runs(&stats, Duration::from_millis(200)));
            let mut run = P3Entry::start_owner_wire(&store, &legacy, &base, Arc::clone(&seam)).await;
            run.settled().await;
            assert!(wire.seen().is_empty(), "no traffic: {:?}", wire.seen());
            assert_eq!(seam.stream_admissions(), 0, "no stream admission");
            let account = store.load_collector_state_rows().expect("rows").account.expect("account");
            assert_eq!(
                (account.mode.as_str(), account.state.as_str()),
                ("legacy", "owner_api_refused"),
                "C3's refusal is retained"
            );
            let kept = store.load_collector_auth_row().expect("auth row").expect("a row");
            assert_eq!((kept.state.as_str(), kept.stopped_mode.as_deref()), ("api_refused", Some("legacy")));
            run.stop().await.expect("an orderly stop");
            assert!(wire.seen().is_empty());
            server.abort();
        }
    }
}
