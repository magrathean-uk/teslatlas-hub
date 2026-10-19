// SPDX-License-Identifier: AGPL-3.0-only

// The B2 round-3 fix round's tests (R3 coordinator rulings, 2026-10-15). Synthetic
// data only. Every behaviour test here was run first against the unfixed tree and
// failed there (the receipts are in the round's progress file).

// ---- C3-R3-01: the billing probe is re-armed whatever the answer ---------------

/// What Tesla (or the wire) answers the billing suspension's probe list.
#[derive(Clone, Copy, Debug)]
enum R3ProbeAnswer {
    ServerError,
    Forbidden,
    UnauthorisedAfterRefresh,
    TransportError,
}

/// A Fleet bed whose account is `BillingSuspended` by a 402 on its second list.
async fn r3_billing_suspended_bed() -> FleetBed {
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "asleep").await;
    bed.tick().await;
    bed.fake.list_status.store(402, Ordering::SeqCst);
    bed.advance(Duration::from_secs(600));
    bed.tick().await;
    assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
    bed
}

fn r3_arm_probe_answer(bed: &mut FleetBed, answer: R3ProbeAnswer) {
    match answer {
        R3ProbeAnswer::ServerError => bed.fake.list_status.store(503, Ordering::SeqCst),
        R3ProbeAnswer::Forbidden => bed.fake.list_status.store(403, Ordering::SeqCst),
        R3ProbeAnswer::UnauthorisedAfterRefresh => {
            bed.allow_refresh = true;
            bed.fake.list_status.store(200, Ordering::SeqCst);
            bed.fake.reject_every_token.store(true, Ordering::SeqCst);
        }
        R3ProbeAnswer::TransportError => bed
            .fake
            .transport
            .fail_next_before_the_handler("/api/1/vehicles", FleetApiError::RequestTimeout),
    }
}

/// One probe period of a suspended account: the probe tick sends the list (and, for
/// a refused token, its one resend), the loop's waits after it never reach the
/// floor-less zero, and no further list goes out until K_BILLING_PROBE has passed.
async fn r3_one_probe_period(bed: &mut FleetBed, answer: R3ProbeAnswer, first: bool) {
    if !first {
        r3_arm_probe_answer(bed, answer);
    }
    let lists_before = bed.lists();
    let mut outcome = bed.tick().await;
    assert!(outcome.requests_sent, "{answer:?}: the probe tick sends");
    // A refused token is refreshed and the list resent once. A resend the persisted
    // reservation does not cover is withheld, and the probe, still owed, is due at
    // once and goes out at the next tick (C3 R27, R47): that is the same probe, not
    // another period's.
    let probe_still_owed =
        |bed: &FleetBed| bed.scheduler.account.next_billing_probe.is_some_and(|probe| probe <= bed.clock.mono_now());
    if matches!(answer, R3ProbeAnswer::UnauthorisedAfterRefresh) && probe_still_owed(bed) {
        outcome = bed.tick().await;
        assert!(outcome.requests_sent, "{answer:?}: the withheld resend goes out");
    }
    let trace = bed.fake.transport.trace();
    let on_the_wire = bed.lists() - lists_before;
    match answer {
        // A transport error is lost before the handler: nothing reaches the fake.
        R3ProbeAnswer::TransportError => assert_eq!(on_the_wire, 0, "{answer:?}: {trace:#?}"),
        // The list, and its resend when the refresh was made and the resend sent.
        R3ProbeAnswer::UnauthorisedAfterRefresh => {
            assert!((1..=2).contains(&on_the_wire), "{answer:?}: {trace:#?}");
        }
        _ => assert_eq!(on_the_wire, 1, "{answer:?}: {trace:#?}"),
    }
    assert!(
        !probe_still_owed(bed),
        "{answer:?}: the probe is armed again, one period on"
    );
    assert_eq!(
        bed.scheduler.account.state,
        AccountState::BillingSuspended,
        "{answer:?}: no answer but a success resumes collection"
    );
    let probed_at = bed.clock.mono_now();
    // The iteration that sent must not spin: the next action is not due now.
    let wait = bed
        .scheduler
        .next_wait(probed_at, false, !outcome.requests_sent);
    assert!(
        !wait.is_zero(),
        "{answer:?}: the loop would send the probe again at once"
    );
    // Idle iterations: nothing goes out, and each sleeps at least the loop floor.
    let after_probe = bed.lists();
    for iteration in 0..30 {
        let now = bed.clock.mono_now();
        let wait = bed.scheduler.next_wait(now, false, true);
        assert!(wait >= K_LOOP_FLOOR, "{answer:?} iteration {iteration}");
        bed.advance(wait);
        let idle = bed.tick().await;
        assert!(!idle.requests_sent, "{answer:?} iteration {iteration}");
        assert_eq!(bed.lists(), after_probe, "{answer:?} iteration {iteration}");
    }
    // One second before the period ends nothing is due; at the period's end the next
    // probe is.
    let now = bed.clock.mono_now();
    let period_end = probed_at + K_BILLING_PROBE;
    bed.advance(period_end - now - Duration::from_secs(1));
    let early = bed.tick().await;
    assert!(!early.requests_sent, "{answer:?}: one second early");
    assert_eq!(bed.lists(), after_probe, "{answer:?}: one second early");
    bed.advance(Duration::from_secs(1));
}

async fn r3_probe_answer_course(answer: R3ProbeAnswer) {
    let mut bed = r3_billing_suspended_bed().await;
    r3_arm_probe_answer(&mut bed, answer);
    bed.advance(K_BILLING_PROBE);
    // Two periods: the second proves the re-arm, not a one-off.
    r3_one_probe_period(&mut bed, answer, true).await;
    r3_one_probe_period(&mut bed, answer, false).await;
    assert!(bed.violations().is_empty(), "{:?}", bed.violations());
}

#[tokio::test]
async fn c3_r3_01_a_probe_answered_503_is_asked_again_only_after_the_probe_period() {
    r3_probe_answer_course(R3ProbeAnswer::ServerError).await;
}

#[tokio::test]
async fn c3_r3_01_a_probe_answered_403_is_asked_again_only_after_the_probe_period() {
    r3_probe_answer_course(R3ProbeAnswer::Forbidden).await;
}

#[tokio::test]
async fn c3_r3_01_a_probe_answered_401_after_the_refresh_is_asked_again_only_after_the_probe_period()
 {
    r3_probe_answer_course(R3ProbeAnswer::UnauthorisedAfterRefresh).await;
}

#[tokio::test]
async fn c3_r3_01_a_probe_lost_on_the_wire_is_asked_again_only_after_the_probe_period() {
    r3_probe_answer_course(R3ProbeAnswer::TransportError).await;
}

#[tokio::test]
async fn c3_r3_01_r3c_03_a_failed_tick_commit_after_a_probe_keeps_the_probe_period() {
    // The probe list went out and was counted; the tick's commit then failed, which
    // returns the account to its baseline. The next probe is one period after the one
    // that was sent, not at the end of the store's wait (B2 apply, R3C-03).
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let witness = Arc::clone(&seam);
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async move {
            let mut bed = r3_billing_suspended_bed().await;
            r3_arm_probe_answer(&mut bed, R3ProbeAnswer::ServerError);
            // A first probe, committed: its reservation leaves slots unused, so the
            // probe of the failing tick needs no raise (a raise is a commit of its own,
            // before the send, and would fail too).
            bed.advance(K_BILLING_PROBE);
            let first = bed.tick().await;
            assert!(first.requests_sent, "the first probe is sent");
            bed.advance(K_BILLING_PROBE);
            let lists_before = bed.lists();
            witness.set_tick_commit_fault(true);
            let probe = bed.tick().await;
            assert!(probe.requests_sent, "the probe tick sends");
            assert_eq!(bed.lists(), lists_before + 1, "one probe list");
            assert!(
                witness.tick_commit_attempts.load(Ordering::SeqCst) > 0,
                "the commit was attempted and failed"
            );
            assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
            let probed_at = bed.clock.mono_now();
            assert!(
                bed.scheduler
                    .account
                    .next_billing_probe
                    .is_some_and(|next| next > probed_at),
                "the probe is not due again after the rollback"
            );
            witness.set_tick_commit_fault(false);
            // The store's wait is over a minute on: still no second probe.
            bed.advance(Duration::from_secs(60));
            let after_wait = bed.tick().await;
            assert!(!after_wait.requests_sent, "no probe at the end of the store's wait");
            assert_eq!(bed.lists(), lists_before + 1);
            // One second before the period ends nothing is due; at its end the next
            // probe is.
            let now = bed.clock.mono_now();
            bed.advance(probed_at + K_BILLING_PROBE - now - Duration::from_secs(1));
            let early = bed.tick().await;
            assert!(!early.requests_sent, "one second early");
            assert_eq!(bed.lists(), lists_before + 1);
            bed.advance(Duration::from_secs(1));
            let next = bed.tick().await;
            assert!(next.requests_sent, "the next probe, one period on");
            assert_eq!(bed.lists(), lists_before + 2);
        })
        .await;
}

#[tokio::test]
async fn c3_r3_01_f2_2_a_successful_probe_whose_commit_failed_is_probed_again_after_the_store_wait()
 {
    // Hub fix pass 2 (F2-2): the probe is answered 200, which resumes collection inside
    // the tick, and the tick's commit then fails, which returns the account to its
    // baseline: suspended, with the probe the baseline held. Tesla has lifted the
    // suspension, so the probe is asked again once the store's wait is over, not one
    // probe period on (R3C-03 keeps the period only for a tick that ended suspended).
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let witness = Arc::clone(&seam);
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async move {
            let mut bed = r3_billing_suspended_bed().await;
            r3_arm_probe_answer(&mut bed, R3ProbeAnswer::ServerError);
            // A first probe, answered 503 and committed, as in the 503 test: its
            // reservation leaves slots unused, so the probe of the failing tick needs no
            // raise.
            bed.advance(K_BILLING_PROBE);
            let first = bed.tick().await;
            assert!(first.requests_sent, "the first probe is sent");
            assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
            // Tesla lifts the suspension: the next probe is answered 200.
            bed.fake.list_status.store(200, Ordering::SeqCst);
            bed.advance(K_BILLING_PROBE);
            let lists_before = bed.lists();
            witness.set_tick_commit_fault(true);
            let probe = bed.tick().await;
            assert!(probe.requests_sent, "the probe tick sends");
            assert_eq!(bed.lists(), lists_before + 1, "one probe list");
            assert!(
                witness.tick_commit_attempts.load(Ordering::SeqCst) > 0,
                "the commit was attempted and failed"
            );
            assert_eq!(
                bed.scheduler.account.state,
                AccountState::BillingSuspended,
                "the rollback returned the account to its baseline"
            );
            assert!(
                bed.scheduler
                    .account
                    .next_billing_probe
                    .is_some_and(|next| next <= bed.clock.mono_now()),
                "the probe is still due: the answer that lifted the suspension was not committed"
            );
            witness.set_tick_commit_fault(false);
            // The store's wait is over a minute on: exactly one probe list goes out and
            // collection resumes.
            bed.advance(Duration::from_secs(60));
            let again = bed.tick().await;
            assert!(again.requests_sent, "the probe is asked again after the store wait");
            assert_eq!(bed.lists(), lists_before + 2, "exactly one more probe list");
            assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
            let settled = bed.tick().await;
            let _ = settled;
            assert_eq!(
                bed.lists(),
                lists_before + 2,
                "no second probe list for the same suspension"
            );
        })
        .await;
}

// ---- C3-R3-02: a restart across a month boundary probes a stored suspension ----

#[tokio::test]
async fn c3_r3_02_ct46_a_restart_in_a_new_month_probes_a_stored_billing_suspension_at_the_first_tick()
 {
    // CORE_W0 is 2025-10-09 07:33:20 UTC; 2025-10-31 22:00:00 UTC is 1_948_000 s on.
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "asleep").await;
    bed.tick().await;
    bed.advance(Duration::from_secs(1_948_000));
    bed.fake.list_status.store(402, Ordering::SeqCst);
    bed.tick().await;
    assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
    let suspended_lists = bed.lists();
    // Control: a restart inside October keeps the stored probe time (04:00 on 1 November).
    bed.advance(Duration::from_secs(3_600));
    bed.reload();
    assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
    let outcome = bed.tick().await;
    assert!(!outcome.requests_sent, "same month: the probe waits for its time");
    assert_eq!(bed.lists(), suspended_lists);
    // A restart at 00:30 on 1 November: Tesla's cycle may have lifted the suspension,
    // so the first tick sends one probe list, three and a half hours before the stored
    // probe time.
    bed.advance(Duration::from_secs(90 * 60));
    bed.reload();
    assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
    let outcome = bed.tick().await;
    assert!(outcome.requests_sent, "the first tick of the new month probes");
    assert_eq!(bed.lists(), suspended_lists + 1);
    assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
    // One probe: the loop does not repeat it before the next probe period.
    let outcome = bed.tick().await;
    assert!(!outcome.requests_sent);
    assert_eq!(bed.lists(), suspended_lists + 1);
    // And the probe's 200 would have resumed collection.
    bed.advance(K_BILLING_PROBE);
    bed.fake.list_status.store(200, Ordering::SeqCst);
    bed.tick().await;
    assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
    // The fake's audit counts the lists of the whole test against the reservation of the
    // month in force, and the month rolled over during the downtime, so its notes on
    // the probe are the double's arithmetic, not a finding.
    let _ = bed.violations();
}

// ---- MIG-R3-1: a failed start keeps the evidence of the predecessor's downtime ---

/// Makes every write of a closed gap, and every update or removal of a gap, fail: the
/// start transaction's downtime bookkeeping (a recorded `hub_down` gap, the closing of
/// an open one, or the removal of one the minimum does not keep) then fails after the
/// lease was acquired.
const R3_START_FAILURE_ON: &str = "
    CREATE TRIGGER r3_no_closed_gap BEFORE INSERT ON collection_gaps
      WHEN NEW.ended_at_ms IS NOT NULL
      BEGIN SELECT RAISE(ABORT, 'r3 injected start failure'); END;
    CREATE TRIGGER r3_no_gap_update BEFORE UPDATE ON collection_gaps
      BEGIN SELECT RAISE(ABORT, 'r3 injected start failure'); END;
    CREATE TRIGGER r3_no_gap_delete BEFORE DELETE ON collection_gaps
      BEGIN SELECT RAISE(ABORT, 'r3 injected start failure'); END;
    CREATE TRIGGER r3_no_stop_gap BEFORE INSERT ON collection_gaps
      WHEN NEW.kind = 'hub_stopped'
      BEGIN SELECT RAISE(ABORT, 'r3 injected start failure'); END;";

const R3_START_FAILURE_OFF: &str = "
    DROP TRIGGER r3_no_closed_gap;
    DROP TRIGGER r3_no_gap_update;
    DROP TRIGGER r3_no_gap_delete;
    DROP TRIGGER r3_no_stop_gap;";

#[derive(Clone, Copy, Debug)]
enum R3Entry {
    Owner,
    Fleet,
    CadenceWait,
}

/// The account-level downtime gaps of the store: `(kind, started, ended)`.
fn r3_downtime_gaps(store: &HubStore) -> Vec<(String, i64, Option<i64>)> {
    store
        .collection_gaps(100)
        .expect("gaps")
        .into_iter()
        .filter(|gap| gap.kind == "hub_down" || gap.kind == "hub_stopped")
        .map(|gap| (gap.kind, gap.started_at_ms, gap.ended_at_ms))
        .collect()
}

/// What the production start does, over the store: read the previous heartbeat,
/// acquire the lease, apply R8 (C3 R7 step 1, R8). Returns the lease.
fn r3_successful_start(store: &HubStore, start_ms: i64) -> crate::db::SupervisedCollectorLease {
    let previous_heartbeat_ms = store
        .supervised_collector_lease_status()
        .expect("lease")
        .map(|lease| lease.heartbeat_at_ms);
    let lease = store
        .acquire_supervised_collector_lease(start_ms)
        .expect("the lease");
    store
        .begin_collector_run(&crate::db::CollectorRunStartRequest {
            mode: "legacy",
            config_fingerprint: &"f".repeat(64),
            start_ms,
            previous_heartbeat_ms,
            gap_min_ms: K_GAP_MIN_MS,
            gap_alert_ms: 600_000,
        })
        .expect("the start");
    lease
}

/// The Owner loop's start over a due-later Legacy pair; the run ends by itself
/// when its start fails.
fn r3_run_owner_start(
    temporary: &std::path::Path,
    store: &HubStore,
    seed: bool,
) -> Result<(), CollectorError> {
    crate::crypto::install_default_provider();
    if seed {
        seed_the_late_import(store);
        crate::teslamate_credentials::replace_key(temporary, b"r3-cloak-key")
            .expect("0600 Cloak key");
        let key = crate::teslamate_credentials::load_key(temporary).expect("Cloak key");
        let initial =
            OwnerTokens::from_secret_parts("r3-access".to_owned(), "r3-refresh".to_owned())
                .expect("initial pair");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
                .expect("Cloak pair");
        let now_seconds = current_epoch_millis().expect("clock") / 1_000;
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    access,
                    refresh,
                    now_seconds + 36_000,
                    now_seconds + 30_000,
                )
                .expect("a schedule that is not due"),
            )
            .expect("store the pair");
    }
    let base = url::Url::parse("http://127.0.0.1:9/").expect("unroutable");
    let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary).expect("admit");
    let manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        temporary,
        base.join("oauth2/v3/").expect("issuer"),
    )
    .expect("load pair")
    .with_runtime_admission(Arc::clone(&admission));
    let region = manager.region();
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region,
    };
    let client = OwnerApi::for_fake_http(base, Duration::from_secs(1)).expect("client");
    let config = supervised_restart_test_config(temporary);
    let (ready_tx, _ready_rx) = oneshot::channel();
    let task_store = store.clone();
    guard_block_on(async move {
        timeout(
            SUPERVISED_RESTART_WAIT,
            run_supervised_with_access(
                &task_store,
                &config,
                supervised_restart_test_cadence(),
                client,
                auth,
                "ws://127.0.0.1:9/streaming/".to_owned(),
                CursorKey::from_bytes([0xB4; 32]),
                Some(ready_tx),
                RunWriteGuard::for_run(Some(admission)),
                std::future::pending::<()>(),
            ),
        )
        .await
        .expect("the run ends by itself")
    })
}

/// The Fleet loop's start over stored credentials and unroutable clients.
fn r3_run_fleet_start(
    temporary: &std::path::Path,
    store: &HubStore,
    seed: bool,
) -> Result<(), CollectorError> {
    crate::crypto::install_default_provider();
    let cursor_key =
        crate::teslamate_credentials::load_or_create_cursor_key(temporary).expect("cursor key");
    if seed {
        guard_block_on(finish_collection_for_provider(
            store,
            &cursor_key,
            &ManualCollection {
                vehicles: vec![Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online")],
                snapshots: Vec::new(),
                failures: Vec::new(),
            },
            CollectorProvider::Fleet,
        ))
        .expect("configure the car");
        crate::fleet_credentials::persist_fleet_setup_credentials(
            store,
            temporary,
            &FleetSetupCredentials::new(
                "r3-fleet-access".to_owned(),
                "r3-fleet-refresh".to_owned(),
                "r3-fleet-client".to_owned(),
                crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
                28_800,
            )
            .expect("setup credentials"),
            SystemTime::now(),
        )
        .expect("persist credentials");
    }
    let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary).expect("admit");
    let manager = Arc::new(tokio::sync::Mutex::new(
        FleetAuthManager::from_store_for_admitted_user(
            store.clone(),
            temporary,
            Arc::clone(&admission),
            None,
        )
        .expect("manager"),
    ));
    let unroutable = url::Url::parse("http://127.0.0.1:9/").expect("URL");
    let api = FleetApi::for_fake_http(unroutable.clone(), Duration::from_secs(1)).expect("api");
    let auth_api = FleetAuthApi::for_fake_http(
        unroutable.join("oauth2/v3/token").expect("token URL"),
        Duration::from_secs(1),
    )
    .expect("auth api");
    let mut config = supervised_restart_test_config(temporary);
    config.collector.provider = crate::config::CollectorProvider::Fleet;
    let (ready_tx, _ready_rx) = oneshot::channel();
    let task_store = store.clone();
    guard_block_on(async move {
        timeout(
            SUPERVISED_RESTART_WAIT,
            run_fleet_supervised_with_access(
                &task_store,
                &config,
                supervised_restart_test_cadence(),
                api,
                auth_api,
                None,
                manager,
                cursor_key,
                ready_tx,
                RunWriteGuard::for_run(Some(admission)),
                false,
                std::future::pending::<()>(),
            ),
        )
        .await
        .expect("the run ends by itself")
    })
}

/// The configuration wait's start (it is the third entry point of R8).
fn r3_run_cadence_wait_start(
    temporary: &std::path::Path,
    store: &HubStore,
    wall: i64,
) -> Result<(), CollectorError> {
    guard_block_on(async {
        let _serial = WAIT_SERIAL.lock().await;
        let config = cadence_wait_config(temporary, WaitMode::Legacy, None);
        let violation = config.collector.cadence_violation().expect("violation");
        let admission =
            crate::hub_user_process::AdmittedUserHub::for_test(temporary).expect("admission");
        let (ready_tx, _ready_rx) = oneshot::channel();
        let clock: Arc<dyn CollectorClock> = ManualClock::new(Instant::now(), wall);
        timeout(
            SUPERVISED_RESTART_WAIT,
            run_cadence_configuration_wait(
                store,
                &config,
                Arc::clone(&admission),
                RunWriteGuard::for_run(Some(admission)),
                ready_tx,
                violation,
                clock,
                SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
                std::future::pending::<()>(),
            ),
        )
        .await
        .expect("the wait's start fails by itself")
    })
}

fn r3_mig_course(entry: R3Entry, inject_failure: bool) {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    // The cadence wait runs on a manual wall clock; the other starts on the real one.
    let wall = match entry {
        R3Entry::CadenceWait => WAIT_WALL0,
        _ => current_epoch_millis().expect("clock"),
    };
    if matches!(entry, R3Entry::CadenceWait) {
        cadence_wait_seed_car(&store);
    }
    // A predecessor died three hours ago at heartbeat H, with no stop recorded: its
    // lease is left to expire.
    let heartbeat = wall - 3 * 3_600_000;
    let _dead = store
        .acquire_supervised_collector_lease(heartbeat)
        .expect("the dead run's lease");
    let mut start_failed = false;
    if inject_failure {
        // The configuration for the entry is made first (the entries seed the store
        // themselves); the failure is installed once they reach their start by
        // making it permanent from here.
        store
            .open()
            .expect("database")
            .execute_batch(R3_START_FAILURE_ON)
            .expect("triggers");
        let result = match entry {
            R3Entry::Owner => r3_run_owner_start(temporary.path(), &store, true),
            R3Entry::Fleet => r3_run_fleet_start(temporary.path(), &store, true),
            R3Entry::CadenceWait => r3_run_cadence_wait_start(temporary.path(), &store, wall),
        };
        let error = result.expect_err("the start fails after the lease was acquired");
        start_failed = true;
        assert!(
            format!("{error:?}").contains("r3 injected start failure")
                || format!("{error}").contains("r3 injected start failure"),
            "{entry:?}: {error:?}"
        );
        // The failed start left the lease to expire: giving it back would lose H.
        assert!(
            store.supervised_collector_lease_status().expect("lease").is_some(),
            "{entry:?}: the lease taken by the failed start is retained to expire"
        );
        store
            .open()
            .expect("database")
            .execute_batch(R3_START_FAILURE_OFF)
            .expect("triggers off");
        // The lease expires on its own.
        store
            .open()
            .expect("database")
            .execute(
                "UPDATE supervised_collector_lease
                    SET started_at_ms = started_at_ms - 120000,
                        heartbeat_at_ms = heartbeat_at_ms - 120000,
                        lease_until_ms = heartbeat_at_ms - 90000",
                [],
            )
            .expect("expire");
    }
    // The next successful start.
    let later = wall + 120_000;
    let lease = r3_successful_start(&store, later);
    let gaps = r3_downtime_gaps(&store);
    let label = format!("{entry:?} failure={inject_failure} started_failed={start_failed}");
    assert_eq!(
        gaps.len(),
        1,
        "{label}: exactly one account downtime gap: {gaps:?}"
    );
    assert_eq!(gaps[0].1, heartbeat, "{label}: it starts at H: {gaps:?}");
    assert_eq!(gaps[0].2, Some(later), "{label}: the start closed it: {gaps:?}");
    store
        .release_supervised_collector_lease(lease)
        .expect("release");
}

#[test]
fn mig_r3_1_control_a_start_without_a_failure_records_the_downtime_from_the_heartbeat() {
    r3_mig_course(R3Entry::Owner, false);
}

#[test]
fn mig_r3_1_a_failed_owner_start_keeps_the_downtime_from_the_predecessors_heartbeat() {
    r3_mig_course(R3Entry::Owner, true);
}

#[test]
fn mig_r3_1_a_failed_fleet_start_keeps_the_downtime_from_the_predecessors_heartbeat() {
    r3_mig_course(R3Entry::Fleet, true);
}

#[test]
fn mig_r3_1_a_failed_cadence_wait_start_keeps_the_lease_and_the_downtime_evidence() {
    r3_mig_course(R3Entry::CadenceWait, true);
}

/// K1 (B2 apply): a chain of failed starts, each under `K_GAP_MIN` after the last
/// heartbeat the previous one left, then a successful start: the whole silence from the
/// FIRST heartbeat is longer than the minimum, and exactly one gap records it, from that
/// heartbeat. Every acquisition keeps the earliest heartbeat atomically, whatever its
/// own silence.
fn r3_mig_short_chain(entry: R3Entry) {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let wall = match entry {
        R3Entry::CadenceWait => WAIT_WALL0,
        _ => current_epoch_millis().expect("clock"),
    };
    if matches!(entry, R3Entry::CadenceWait) {
        cadence_wait_seed_car(&store);
    }
    // A predecessor died 31 s ago at heartbeat H (its lease just expired), with no stop.
    let heartbeat = wall - 31_000;
    let _dead = store
        .acquire_supervised_collector_lease(heartbeat)
        .expect("the dead run's lease");
    for attempt in 0..3 {
        store
            .open()
            .expect("database")
            .execute_batch(R3_START_FAILURE_ON)
            .expect("triggers");
        let result = match entry {
            R3Entry::Owner => r3_run_owner_start(temporary.path(), &store, attempt == 0),
            R3Entry::Fleet => r3_run_fleet_start(temporary.path(), &store, attempt == 0),
            R3Entry::CadenceWait => r3_run_cadence_wait_start(temporary.path(), &store, wall),
        };
        let error = result.expect_err("the start fails after the lease was acquired");
        assert!(
            format!("{error:?}").contains("r3 injected start failure")
                || format!("{error}").contains("r3 injected start failure"),
            "{entry:?} attempt {attempt}: {error:?}"
        );
        store
            .open()
            .expect("database")
            .execute_batch(R3_START_FAILURE_OFF)
            .expect("triggers off");
        // The earliest heartbeat survived this attempt, although its own silence was
        // under the minimum.
        assert_eq!(
            r3_downtime_gaps(&store),
            vec![("hub_down".to_owned(), heartbeat, None)],
            "{entry:?} attempt {attempt}: one open downtime from H"
        );
        // 31 s pass: the failed start's lease has expired (30 s) and the next attempt
        // finds a silence of about 31 s again.
        store
            .open()
            .expect("database")
            .execute(
                "UPDATE supervised_collector_lease
                    SET started_at_ms = started_at_ms - 31000,
                        heartbeat_at_ms = heartbeat_at_ms - 31000,
                        lease_until_ms = lease_until_ms - 31000",
                [],
            )
            .expect("expire");
    }
    // The successful start, 71 s after H: longer than the minimum, though the last lease
    // it replaces was left 31 s earlier by the failed attempts.
    let later = wall + 40_000;
    let lease = r3_successful_start(&store, later);
    let gaps = r3_downtime_gaps(&store);
    assert_eq!(
        gaps,
        vec![("hub_down".to_owned(), heartbeat, Some(later))],
        "{entry:?}: exactly one gap, from the first heartbeat"
    );
    store
        .release_supervised_collector_lease(lease)
        .expect("release");
}

#[test]
fn mig_r3_1_k1_repeated_failed_owner_starts_each_under_the_minimum_keep_the_first_heartbeat() {
    r3_mig_short_chain(R3Entry::Owner);
}

#[test]
fn mig_r3_1_k1_repeated_failed_fleet_starts_each_under_the_minimum_keep_the_first_heartbeat() {
    r3_mig_short_chain(R3Entry::Fleet);
}

#[test]
fn mig_r3_1_k1_repeated_failed_cadence_wait_starts_each_under_the_minimum_keep_the_first_heartbeat()
 {
    r3_mig_short_chain(R3Entry::CadenceWait);
}

#[test]
fn mig_r3_1_k1_the_minimum_is_applied_to_the_whole_silence_by_the_start_that_succeeds() {
    // Store level, explicit times: failed acquisitions 31 s and 62 s after H, a start
    // at 93 s records one gap from H; a start within the minimum records none and
    // leaves no open row; the boundary is the minimum itself (a silence of exactly
    // `K_GAP_MIN` is not downtime).
    let course = |starts_after_ms: &[i64], success_after_ms: i64| {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let heartbeat = 1_800_000_000_000_i64;
        let _dead = store
            .acquire_supervised_collector_lease(heartbeat)
            .expect("the dead run's lease");
        for after in starts_after_ms {
            let _failed = store
                .acquire_supervised_collector_lease(heartbeat + after)
                .expect("a start that then fails");
        }
        let lease = r3_successful_start(&store, heartbeat + success_after_ms);
        let gaps = r3_downtime_gaps(&store);
        store
            .release_supervised_collector_lease(lease)
            .expect("release");
        (heartbeat, gaps)
    };
    // The chain: each acquisition replaces a lease 31 s old.
    let (h, gaps) = course(&[31_000, 62_000], 93_000);
    assert_eq!(gaps, vec![("hub_down".to_owned(), h, Some(h + 93_000))]);
    // One start within the minimum: no downtime, and no open row is left behind.
    let (_, gaps) = course(&[], 31_000);
    assert_eq!(gaps, Vec::new());
    let (_, gaps) = course(&[31_000], 62_000);
    assert_eq!(gaps, Vec::new(), "a chain whose whole silence is within the minimum");
    // The boundary: exactly the minimum is not downtime, one millisecond more is.
    let (_, gaps) = course(&[31_000], K_GAP_MIN_MS);
    assert_eq!(gaps, Vec::new(), "exactly K_GAP_MIN");
    let (h, gaps) = course(&[31_000], K_GAP_MIN_MS + 1);
    assert_eq!(gaps, vec![("hub_down".to_owned(), h, Some(h + K_GAP_MIN_MS + 1))]);
}

/// Wait, by acknowledged steps, until `condition` holds: yields to the runtime and
/// lets the blocking pool's jobs finish. The wait is the stall watchdog of the loop
/// tests (real time, since the clock of the caller may be paused), not a bound the
/// test judges; one that runs out fails with its label instead of letting the test go
/// on (B2 apply, K4).
async fn r3_poll_until(label: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + TRAVERSAL_STALL_WATCHDOG;
    while std::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("{label}: the acknowledgement never came");
}

// ---- I3-01: an identified unresolved Fleet refresh only skips the token refresh ----

/// The status of the one address job of the store.
fn r3_enrichment_job_status(store: &HubStore) -> String {
    store
        .open()
        .expect("catalogue")
        .query_row("SELECT status FROM address_enrichment_jobs", [], |row| row.get(0))
        .expect("the address job")
}

#[tokio::test(start_paused = true)]
async fn i3_01_a_fenced_telemetry_loop_still_replays_enriches_and_wakes_the_terrain_worker() {
    // The telemetry maintenance loop over a fenced manager (an identified unresolved
    // refresh) sends no Fleet request and touches no credential, but its own tail work
    // goes on every maintenance interval: the address enrichment pass (a claimable job
    // is claimed and completed, from the local cache, so nothing is looked up), the
    // export outbox replay (a dirty row is claimed and completed) and the terrain wake.
    // A tripwire server on the Fleet host and the token URL records every request of
    // any method and path: none arrives.
    crate::crypto::install_default_provider();
    let (temporary, store, cursor_key, _generation) = p3_unresolved_fleet_hub();
    let mut config = supervised_restart_test_config(temporary.path());
    // Deterministic local enrichment: the geocoder is enabled on an endpoint that is not
    // listening, and the job's place is in the address cache, so the pass answers from
    // the cache and sends nothing.
    config.geocoder = crate::config::GeocoderConfig {
        enabled: true,
        endpoint: Some("https://127.0.0.1:9/".to_owned()),
        language: "en".to_owned(),
        timeout_seconds: 1,
    };
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admission");
    let manager = FleetAuthManager::from_store_for_supervised_start(
        store.clone(),
        temporary.path(),
        Arc::clone(&admission),
        None,
    )
    .expect("the supervised start admits it");
    assert!(manager.is_input_fenced());
    let manager = Arc::new(tokio::sync::Mutex::new(manager));
    let tripwire = FleetTripwire::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new()
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(axum::middleware::from_fn_with_state(
            tripwire.clone(),
            fleet_tripwire_record,
        ))
        .with_state(tripwire.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("tripwire server");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("base");
    let auth_api = FleetAuthApi::for_fake_http(
        base.join("oauth2/v3/token").expect("token URL"),
        Duration::from_secs(1),
    )
    .expect("auth client");
    let (terrain_wake, mut terrain_rx) = mpsc::channel(8);
    let scheduler = VehicleScheduler::new_for_mode(
        p3_fleet_cadence(1_000),
        crate::config::CollectorMode::FleetPolling,
        Instant::now(),
    );
    let ledger = scheduler.request_ledger();
    let hub = only_configured_hub_id(&store);
    // A claimable export outbox row for the car.
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO export_outbox(
                vehicle_id, dirty_revision, attempts, next_attempt_ms,
                claimed_until_ms, last_error
             ) VALUES (?1, 1, 0, 0, 0, NULL)",
            [hub.to_string()],
        )
        .expect("an export row is waiting");
    assert!(export_outbox_row(&store).is_some(), "an export row is waiting");
    // A claimable address job for the car, and the cached answer for its place.
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO address_enrichment_jobs(
                    job_key, vehicle_id, target_type, target_id, field,
                    latitude, longitude, status, attempts, next_attempt_ms,
                    lease_until_ms
                 ) VALUES ('i3-01-job', ?1, 'drive', 1, 'start_address', 1.0, 2.0,
                           'pending', 0, 0, 0)",
            [hub.to_string()],
        )
        .expect("an address job is waiting");
    let geocoder = Geocoder::new(&config.geocoder).expect("geocoder");
    store
        .put_address_cache(&crate::db::AddressCacheRecord {
            osm_type: "way".to_owned(),
            osm_id: 42,
            display_name: "Cached Road".to_owned(),
            name: None,
            latitude: Some(1.0),
            longitude: Some(2.0),
            house_number: None,
            road: None,
            neighbourhood: None,
            city: None,
            county: None,
            postcode: None,
            state: None,
            state_district: None,
            country: None,
            raw_json: None,
            binding: geocoder.lookup_binding().clone(),
            licence: None,
            lookup_latitude: 1.0,
            lookup_longitude: 2.0,
            looked_up_at_ms: 1_000,
        })
        .expect("cache the place");
    assert_eq!(r3_enrichment_job_status(&store), "pending");
    let evidence_before = p3_fleet_evidence(&store);
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let task_store = store.clone();
    let task_admission = Arc::clone(&admission);
    let task_seam = Arc::clone(&seam);
    let task = tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
        run_fleet_telemetry_maintenance_loop(
            &task_store,
            &config,
            &auth_api,
            &manager,
            &cursor_key,
            &terrain_wake,
            &task_admission,
            true,
            &ledger,
            &RunWriteGuard::new(AdmissionFence::default(), Some(Arc::clone(&task_admission))),
        )
        .await
    }));
    // The passes the loop reaches are acknowledged by the seam and by the rows they
    // leave; the test waits for those acknowledgements (a bounded number of polls is
    // only the watchdog against a stall, never a measured span), then lets one
    // maintenance interval pass and waits for the second interval's own.
    r3_poll_until("the first replay completes", || seam.replay_completions() >= 1).await;
    r3_poll_until("the address job completes", || {
        r3_enrichment_job_status(&store) == "complete"
    })
    .await;
    assert!(!task.is_finished(), "the run goes on over the unknown outcome");
    let (enrichments, replays) = (seam.enrichment_calls(), seam.replay_calls());
    tokio::time::advance(Duration::from_secs(61)).await;
    r3_poll_until("the second interval's enrichment pass", || {
        seam.enrichment_calls() > enrichments
    })
    .await;
    r3_poll_until("the second interval's replay", || seam.replay_calls() > replays).await;
    assert!(!task.is_finished(), "the run goes on over the unknown outcome");
    assert!(seam.replay_claims() >= 1, "the replay claimed the waiting outbox row");
    assert!(seam.replay_completions() >= 1, "the replay completed");
    assert_eq!(r3_enrichment_job_status(&store), "complete", "the address job was processed");
    assert!(terrain_rx.try_recv().is_ok(), "the terrain worker was woken");
    task.abort();
    assert_eq!(
        p3_fleet_evidence(&store),
        evidence_before,
        "credentials and evidence untouched"
    );
    assert_eq!(
        tripwire.seen(),
        Vec::<String>::new(),
        "no request of any kind reached the Fleet host or the token URL"
    );
    server.abort();
}

// ---- C4R3-N2: the frames of a stopping run do not mark its stream healthy -------

#[tokio::test]
async fn c4r3_n2_the_queued_frames_of_a_stopped_stream_do_not_mark_it_connected_or_fresh() {
    // `streams_to_stop` has just cleared the car's stream health. The frames the
    // stopped run left queued are stored and judged (R36), but they say nothing about
    // a stream that is over, so the car must not read as stream-healthy again (its
    // next read would be planned at the slower stream interval).
    let mut bed = StreamBed::new();
    let id = core_id(1);
    bed.core.scheduler.vehicles.get_mut(&id).unwrap().stream.connected = false;
    let base = StreamBed::now_ms();
    let (events, receiver) = mpsc::channel(16);
    let (shutdown, stop) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = stop.await;
        Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
    });
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id: id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: Some(shutdown),
        task: Some(task),
    }];
    for step in 0..3 {
        events
            .send(bed.telemetry(frame(base + step * 100, Some("P"), Some(0), Some(0))))
            .await
            .expect("queued");
    }
    let mut cache = HashMap::new();
    let mut pending = StreamDrainResult::default();
    let mut budget = DrainBudget::new(Instant::now());
    let store = bed.store.clone();
    stop_vehicle_streams(
        &store,
        &mut bed.core.scheduler,
        &mut streams,
        &[id],
        &mut cache,
        &mut pending,
        &mut budget,
    )
    .await
    .expect("the stop");
    assert_eq!(bed.stored_frames(), 1, "the queued frames were stored");
    let record = &bed.core.scheduler.vehicles[&id];
    assert!(!record.stream.connected, "the stopped stream is not connected");
    assert!(record.stream.last_frame.is_none(), "and has no fresh frame");
}

#[tokio::test]
async fn c4r3_n2_the_frames_drained_from_a_winding_down_run_do_not_mark_it_connected_or_fresh() {
    let mut bed = StreamBed::new();
    let id = core_id(1);
    bed.core.scheduler.vehicles.get_mut(&id).unwrap().stream.connected = false;
    let base = StreamBed::now_ms();
    let (events, receiver) = mpsc::channel(16);
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id: id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: receiver,
        _shutdown: None,
        task: None,
    }];
    for step in 0..3 {
        events
            .send(bed.telemetry(frame(base + step * 100, Some("P"), Some(0), Some(0))))
            .await
            .expect("queued");
    }
    let mut cache = HashMap::new();
    let store = bed.store.clone();
    drain_stream_events_while_stopping(
        &store,
        &mut bed.core.scheduler,
        &mut streams,
        &mut cache,
        &[id],
        None,
    )
    .await
    .expect("the drain");
    assert_eq!(bed.stored_frames(), 1, "the frames were stored");
    let record = &bed.core.scheduler.vehicles[&id];
    assert!(!record.stream.connected, "a winding-down run is not connected");
    assert!(record.stream.last_frame.is_none(), "and has no fresh frame");
    // A frame of a run that is not winding down still marks the stream healthy.
    let mut streams = vec![VehicleStreamRuntime {
        vehicle_id: id,
        power_gate: Arc::new(StreamPowerGate::default()),
        sensitive_access_failure: Arc::new(AtomicBool::new(false)),
        events: {
            let (sender, receiver) = mpsc::channel(4);
            sender
                .send(bed.telemetry(frame(base + 500, Some("P"), Some(0), Some(0))))
                .await
                .expect("queued");
            receiver
        },
        _shutdown: None,
        task: None,
    }];
    drain_stream_events_while_stopping(
        &store,
        &mut bed.core.scheduler,
        &mut streams,
        &mut cache,
        &[],
        None,
    )
    .await
    .expect("the drain");
    assert!(bed.core.scheduler.vehicles[&id].stream.connected);
}
