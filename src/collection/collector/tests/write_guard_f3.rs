// SPDX-License-Identifier: AGPL-3.0-only

// B2 fix round, P3b fix 3 (R-P3bF3-1, R-P3bF3-3, R-P3bF3-4 and the nits of the second
// check): the run's one write guard is made by the entry point, before the credential
// unit is built and before the cursor key is created, and handed down; the unit
// consults it immediately before the generation bind and before its first status row,
// and, on a migrated row (a counted rejection with no stored rejected-access digest),
// in front of the durable digest fill, which comes before the generation bind and is
// then the unit's first write; the entry consults it in front of the cursor key. The
// proofs read the whole store, the pack files and the count of mutations the store's
// connections attempted (the sink, counted at the SQLite boundary independently of every
// guard) at the very moment the armed boundary refuses, and again when the run has
// ended. The data is synthetic and defined here.

// ---- the sink counter ----------------------------------------------------------------------------------------

#[test]
fn p3bf3_the_sink_counts_a_write_that_leaves_every_row_as_it_was() {
    let (temporary, bed) = GuardBed::new();
    let first = whole_store_stamp(&bed.store);
    // The same digest recorded twice: the second write changes nothing the rows could show.
    bed.store
        .record_terrain_tile_digest("N51W001", &"ab".repeat(32), 1)
        .expect("first write");
    let after_first = whole_store_stamp(&bed.store);
    assert!(first != after_first, "a real write moves the stamp");
    bed.store
        .record_terrain_tile_digest("N51W001", &"ab".repeat(32), 1)
        .expect("idempotent write");
    let after_second = whole_store_stamp(&bed.store);
    assert!(
        after_first != after_second,
        "an idempotent write still moves the sink"
    );
    let sink_lines = |stamp: &str| stamp.lines().filter(|line| !line.starts_with("sink:")).count();
    assert_eq!(sink_lines(&after_first), sink_lines(&after_second));
    // The lease's heartbeat is the one exemption: it does not move the stamp.
    let lease = bed
        .store
        .acquire_supervised_collector_lease(current_epoch_millis().expect("clock"))
        .expect("lease");
    let with_lease = whole_store_stamp(&bed.store);
    bed.store
        .heartbeat_supervised_collector_lease(
            lease,
            SupervisedCollectorState::Active,
            current_epoch_millis().expect("clock") + 1,
        )
        .expect("renewal");
    assert!(
        whole_store_stamp(&bed.store) == with_lease,
        "a heartbeat is not a mutation of the run's"
    );
    // A pack file rewritten with the same bytes changes its inode or its time.
    let pack = temporary.path().join("packs").join("sink-probe");
    std::fs::write(&pack, b"same").expect("pack file");
    let written = whole_store_stamp(&bed.store);
    std::thread::sleep(Duration::from_millis(30));
    std::fs::write(&pack, b"same").expect("the same bytes again");
    assert!(
        whole_store_stamp(&bed.store) != written,
        "a rewrite of a pack file shows"
    );
}

// ---- the entry points: the guard exists before the credential unit and the cursor key --------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum P3bf3Entry {
    Owner,
    Observer,
    Fleet,
}

/// What an entry point came to when its run was refused at an armed boundary.
struct EntryOutcome {
    result: Result<(), CollectorError>,
    before: String,
    at_trip: Option<String>,
    after: String,
    key_file: bool,
    lease_left: bool,
    audit: Arc<WriteGuardAudit>,
    /// The key files of `secrets/` (name, inode, modification time, length) before the
    /// run and when it had ended (R-P3bF4-4).
    secrets_before: String,
    secrets_after: String,
}

/// The files of a directory with their inode, modification time and length: what a
/// settlement of an interrupted key replacement would change.
fn secrets_stamp(directory: &std::path::Path) -> String {
    use std::os::unix::fs::MetadataExt as _;
    let mut lines: Vec<String> = std::fs::read_dir(directory.join("secrets"))
        .expect("secrets directory")
        .flatten()
        .map(|entry| {
            let metadata = entry.metadata().expect("metadata");
            format!(
                "{}:{}:{}.{}:{}",
                entry.file_name().to_string_lossy(),
                metadata.ino(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.len()
            )
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

fn p3bf3_seed_the_owner_store(temporary: &tempfile::TempDir) -> HubStore {
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_the_late_import(&store);
    crate::teslamate_credentials::replace_key(temporary.path(), b"guard-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial =
        OwnerTokens::from_secret_parts("guard-access".to_owned(), "guard-refresh".to_owned())
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
    store
}

fn p3bf3_seed_the_fleet_store(temporary: &tempfile::TempDir) -> HubStore {
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    guard_block_on(finish_collection_for_provider(
        &store,
        &CursorKey::from_bytes([0x5D; 32]),
        &ManualCollection {
            vehicles: vec![Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online")],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    ))
    .expect("configure the car");
    crate::fleet_credentials::persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &FleetSetupCredentials::new(
            "guard-fleet-access".to_owned(),
            "guard-fleet-refresh".to_owned(),
            "guard-fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("setup credentials"),
        SystemTime::now(),
    )
    .expect("persist credentials");
    store
}

/// Run the production entry point of `kind` over a store with a pair that is not due,
/// unroutable Tesla clients and no cursor key yet: the fence is set when the run
/// reaches `trip` (the `nth` time), the admission untouched and valid. The run is given
/// no shutdown: it must end by itself.
fn p3bf3_entry(kind: P3bf3Entry, trip: (&'static str, usize)) -> EntryOutcome {
    p3bf3_entry_seeded(kind, trip, |_| {})
}

/// [`p3bf3_entry`] with `seed` run over the data directory after the store is seeded
/// (an interrupted key replacement, for one).
fn p3bf3_entry_seeded(
    kind: P3bf3Entry,
    trip: (&'static str, usize),
    seed: impl FnOnce(&std::path::Path),
) -> EntryOutcome {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = match kind {
        P3bf3Entry::Fleet => p3bf3_seed_the_fleet_store(&temporary),
        _ => p3bf3_seed_the_owner_store(&temporary),
    };
    assert!(
        !crate::teslamate_credentials::cursor_key_path(temporary.path()).exists(),
        "the cursor key does not exist yet"
    );
    seed(temporary.path());
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
    let unroutable = url::Url::parse("http://127.0.0.1:9/").expect("unroutable");
    let client = OwnerApi::for_fake_http(unroutable.clone(), Duration::from_secs(1)).expect("client");
    let fleet = (kind == P3bf3Entry::Fleet).then(|| FleetClients {
        api: FleetApi::for_fake_http(unroutable.clone(), Duration::from_secs(1)).expect("api"),
        auth_api: FleetAuthApi::for_fake_http(
            unroutable.join("oauth2/v3/token").expect("token URL"),
            Duration::from_secs(1),
        )
        .expect("auth api"),
        command_proxy: None,
    });
    let mut config = supervised_restart_test_config(temporary.path());
    if kind == P3bf3Entry::Fleet {
        config.collector.provider = CollectorProvider::Fleet;
    }
    let (ready_tx, _ready_rx) = oneshot::channel();
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let at_trip = Arc::new(Mutex::new(None));
    {
        let observed = store.clone();
        let slot = Arc::clone(&at_trip);
        seam.trip_write_guard_at(
            trip.0,
            trip.1,
            Arc::new(move || {
                *slot.lock().expect("stamp") = Some(whole_store_stamp(&observed));
            }),
        );
    }
    let before = whole_store_stamp(&store);
    let secrets_before = secrets_stamp(temporary.path());
    let task_store = store.clone();
    let task_seam = Arc::clone(&seam);
    let task_admission = Arc::clone(&admission);
    let result = guard_block_on(async move {
        timeout(
            SUPERVISED_RESTART_WAIT,
            SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
                let no_owner = |_| -> Result<OwnerApi, CollectorError> {
                    panic!("an Owner API client was built by a Fleet run")
                };
                let no_fleet = |_| -> Result<FleetClients, CollectorError> {
                    panic!("Fleet clients were built by an Owner run")
                };
                match (kind, fleet) {
                    (P3bf3Entry::Owner, _) => {
                        run_supervised_with_clients_for_admitted_user(
                            &task_store,
                            &config,
                            task_admission,
                            ready_tx,
                            move |_| Ok(client),
                            no_fleet,
                            std::future::pending::<()>(),
                        )
                        .await
                    }
                    (P3bf3Entry::Observer, _) => {
                        run_observer_with_clients_for_admitted_user(
                            &task_store,
                            &config,
                            task_admission,
                            ready_tx,
                            move |_| Ok(client),
                            no_fleet,
                            std::future::pending::<()>(),
                        )
                        .await
                    }
                    (P3bf3Entry::Fleet, clients) => {
                        run_supervised_with_clients_for_admitted_user(
                            &task_store,
                            &config,
                            task_admission,
                            ready_tx,
                            no_owner,
                            move |_| Ok(clients.expect("Fleet clients")),
                            std::future::pending::<()>(),
                        )
                        .await
                    }
                }
            }),
        )
        .await
        .expect("the run ends by itself")
    });
    EntryOutcome {
        result,
        before,
        at_trip: at_trip.lock().expect("stamp").clone(),
        after: whole_store_stamp(&store),
        key_file: crate::teslamate_credentials::cursor_key_path(temporary.path()).exists(),
        lease_left: store
            .supervised_collector_lease_status()
            .expect("lease query")
            .is_some(),
        audit: seam.write_audit(),
        secrets_before,
        secrets_after: secrets_stamp(temporary.path()),
    }
}

/// The run was refused at `point`, wrote nothing after it (rows, packs and sink), took no
/// lease and created no cursor key.
fn p3bf3_assert_refused_at(label: &str, point: &'static str, outcome: &EntryOutcome) {
    p3bf3_assert_refused(label, point, outcome, false);
}

/// [`p3bf3_assert_refused_at`], with whether the cursor key exists (the run's own
/// boundaries follow the entry's creation of it).
fn p3bf3_assert_refused(
    label: &str,
    point: &'static str,
    outcome: &EntryOutcome,
    key_created: bool,
) {
    // The Fleet constructor names the loss in its own error type; the loops treat both as
    // the loss of the sensitive access.
    assert!(
        outcome.result.as_ref().is_err_and(loses_sensitive_access),
        "{label}: {:?}",
        outcome.result
    );
    assert!(outcome.audit.tripped(), "{label}: the boundary was never reached");
    assert!(outcome.audit.refused(point) >= 1, "{label}: the boundary did not refuse");
    assert!(
        outcome.at_trip.as_ref() == Some(&outcome.after),
        "{label}: a row, a pack file or a mutation attempt followed the refused boundary"
    );
    assert!(
        outcome.key_file == key_created,
        "{label}: the cursor key's existence is not as expected ({key_created})"
    );
    assert!(!outcome.lease_left, "{label}: the run never took a lease");
}

#[test]
fn p3bf3_the_owner_entry_binds_nothing_after_the_fence() {
    // The first consultation of the unit's observer is in front of the generation bind:
    // nothing at all has been written when it refuses.
    let outcome = p3bf3_entry(P3bf3Entry::Owner, ("credential", 1));
    p3bf3_assert_refused_at("owner bind", "credential", &outcome);
    assert!(
        outcome.at_trip.as_ref() == Some(&outcome.before),
        "the bind is the first write of the unit: nothing preceded it"
    );
}

#[test]
fn p3bf3_the_owner_entry_writes_no_first_status_after_the_fence() {
    // The second is in front of the unit's first status row: the bind has been made (the
    // control that the sink is live in this path), the status is held back, and the entry
    // ends at the cursor key.
    let outcome = p3bf3_entry(P3bf3Entry::Owner, ("credential", 2));
    p3bf3_assert_refused_at("owner first status", "credential", &outcome);
    assert!(
        outcome.at_trip.as_ref() != Some(&outcome.before),
        "the generation was bound before the fence"
    );
}

#[test]
fn p3bf3_the_owner_entry_creates_no_cursor_key_after_the_fence() {
    let outcome = p3bf3_entry(P3bf3Entry::Owner, ("cursor-key", 1));
    p3bf3_assert_refused_at("owner cursor key", "cursor-key", &outcome);
}

#[test]
fn p3bf3_the_observer_entry_writes_nothing_after_the_fence() {
    for (point, nth) in [("credential", 1), ("cursor-key", 1)] {
        let outcome = p3bf3_entry(P3bf3Entry::Observer, (point, nth));
        p3bf3_assert_refused_at(point, point, &outcome);
    }
}

#[test]
fn p3bf3_the_fleet_entry_binds_nothing_and_creates_no_cursor_key_after_the_fence() {
    let outcome = p3bf3_entry(P3bf3Entry::Fleet, ("credential", 1));
    p3bf3_assert_refused_at("fleet bind", "credential", &outcome);
    assert!(
        outcome.at_trip.as_ref() == Some(&outcome.before),
        "the bind is the first write of the manager"
    );
    // The generation of the stored pair is bound already, so the second consultation
    // (the entry's, in front of the cursor key) follows a bind that changed nothing.
    let outcome = p3bf3_entry(P3bf3Entry::Fleet, ("cursor-key", 1));
    p3bf3_assert_refused_at("fleet cursor key", "cursor-key", &outcome);
}

#[test]
fn p3bf3_every_entry_hands_its_guard_to_the_run() {
    // The guard the entry made is the one the run consults: armed at the run's first
    // boundary (the lease), the audit the seam holds is tripped there, the entry ends with
    // the sensitive failure and neither a lease nor a row, a pack file or a mutation
    // attempt follows the refusal.
    for kind in [P3bf3Entry::Owner, P3bf3Entry::Observer, P3bf3Entry::Fleet] {
        let outcome = p3bf3_entry(kind, ("start-lease", 1));
        // The entry created the cursor key before it handed the run its guard.
        p3bf3_assert_refused(&format!("{kind:?} run"), "start-lease", &outcome, true);
    }
}

#[test]
fn p3bf3_the_dispatcher_hands_its_guard_to_the_configuration_wait() {
    // The dispatcher made the guard before the wait: the seam's fence trips at the
    // wait's first boundary and the wait ends with the sensitive failure having written
    // nothing (rows, packs and sink) after it.
    crate::crypto::install_default_provider();
    let (temporary, store) = cadence_wait_hub();
    let config = cadence_wait_config(temporary.path(), WaitMode::Legacy, None);
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
    let (ready_tx, _ready_rx) = oneshot::channel();
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let at_trip = Arc::new(Mutex::new(None));
    {
        let observed = store.clone();
        let slot = Arc::clone(&at_trip);
        seam.trip_write_guard_at(
            "cadence-lease",
            1,
            Arc::new(move || {
                *slot.lock().expect("stamp") = Some(whole_store_stamp(&observed));
            }),
        );
    }
    let task_store = store.clone();
    let task_seam = Arc::clone(&seam);
    let result = guard_block_on(async move {
        timeout(
            SUPERVISED_RESTART_WAIT,
            SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
                run_wait_entry(&task_store, &config, admission, ready_tx, std::future::pending::<()>())
                    .await
            }),
        )
        .await
        .expect("the wait ends by itself")
    });
    refused_with_the_sensitive_failure("dispatcher wait", &result);
    let audit = seam.write_audit();
    assert!(audit.tripped() && audit.refused("cadence-lease") >= 1);
    assert!(
        Some(whole_store_stamp(&store)) == *at_trip.lock().expect("stamp"),
        "a write followed the refused boundary"
    );
}

// ---- the terrain worker the supervisor starts (Opus N1) --------------------------------------------------------

/// Run the Owner loop (`run_supervised_with_access`) over `store` with a pair that is not
/// due and an unroutable Owner API: the fence is set when the run reaches `trip` (the
/// `nth` time), the admission untouched and valid. The run is given no shutdown: it must
/// end by itself. Returns its result, the store as it was at the trip and the audit.
fn p3bf3_run_the_owner_loop(
    temporary: &tempfile::TempDir,
    store: &HubStore,
    config: HubConfig,
    trip: (&'static str, usize),
) -> (Result<(), CollectorError>, Option<String>, Arc<WriteGuardAudit>) {
    let base = url::Url::parse("http://127.0.0.1:9/").expect("unroutable");
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
    let manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        temporary.path(),
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
    let (ready_tx, _ready_rx) = oneshot::channel();
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let at_trip = Arc::new(Mutex::new(None));
    {
        let observed = store.clone();
        let slot = Arc::clone(&at_trip);
        seam.trip_write_guard_at(
            trip.0,
            trip.1,
            Arc::new(move || {
                *slot.lock().expect("stamp") = Some(whole_store_stamp(&observed));
            }),
        );
    }
    let task_store = store.clone();
    let task_seam = Arc::clone(&seam);
    let result = guard_block_on(async move {
        timeout(
            SUPERVISED_RESTART_WAIT,
            SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
                run_supervised_with_access(
                    &task_store,
                    &config,
                    supervised_restart_test_cadence(),
                    client,
                    auth,
                    "ws://127.0.0.1:9/streaming/".to_owned(),
                    CursorKey::from_bytes([0xB3; 32]),
                    Some(ready_tx),
                    RunWriteGuard::for_run(Some(admission)),
                    std::future::pending::<()>(),
                )
                .await
            }),
        )
        .await
        .expect("the run ends by itself")
    });
    let at_trip = at_trip.lock().expect("stamp").clone();
    (result, at_trip, seam.write_audit())
}

#[test]
fn p3bf3_the_supervisor_hands_its_own_guard_to_the_terrain_worker() {
    // Terrain enabled, a pass with candidates, and the tile they lie in already cached
    // with its digest recorded, so no download is ever made (a worker holding some other
    // guard would still stay offline). The audit of the run's guard, armed at the
    // worker's first boundary, is the one the worker consults: it trips there, the run
    // ends with the sensitive failure and nothing is written, attempted or created after
    // it. A worker handed a guard of its own would never reach the armed boundary.
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = p3bf3_seed_the_owner_store(&temporary);
    p3bf2_terrain_candidates(&store);
    p3bf3_cached_terrain(&temporary, &store);
    let mut config = supervised_restart_test_config(temporary.path());
    config.terrain.enabled = true;
    let (result, at_trip, audit) =
        p3bf3_run_the_owner_loop(&temporary, &store, config, ("terrain-lookup", 1));
    assert!(audit.tripped(), "the worker never reached the run's guard");
    assert!(audit.refused("terrain-lookup") >= 1);
    assert!(
        result.as_ref().is_err_and(loses_sensitive_access),
        "the run ends with the sensitive failure: {result:?}"
    );
    assert!(
        Some(whole_store_stamp(&store)) == at_trip,
        "a row, a pack file or a mutation attempt followed the refused boundary"
    );
}

#[test]
fn p3bf4_the_terrain_cache_consults_the_guard_at_its_own_audit_point() {
    // N3: the cache's construction is a boundary of its own ("terrain-cache"), not the
    // geocoder's. Armed there, the run ends with the sensitive failure and nothing is
    // written, attempted or created after it; the geocoder's point is never reached.
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = p3bf3_seed_the_owner_store(&temporary);
    p3bf2_terrain_candidates(&store);
    p3bf3_cached_terrain(&temporary, &store);
    let mut config = supervised_restart_test_config(temporary.path());
    config.terrain.enabled = true;
    let (result, at_trip, audit) =
        p3bf3_run_the_owner_loop(&temporary, &store, config, ("terrain-cache", 1));
    assert!(audit.tripped(), "the cache never consulted the run's guard");
    assert!(audit.refused("terrain-cache") >= 1);
    assert_eq!(audit.refused("geocoder"), 0, "the cache is not the geocoder");
    assert!(
        result.as_ref().is_err_and(loses_sensitive_access),
        "the run ends with the sensitive failure: {result:?}"
    );
    assert!(
        Some(whole_store_stamp(&store)) == at_trip,
        "a row, a pack file or a file mutation attempt followed the refused boundary"
    );
}

/// The cached tile and the config of a terrain pass that makes no download (see above).
fn p3bf3_cached_terrain(temporary: &tempfile::TempDir, store: &HubStore) {
    let cache_dir = temporary.path().join("terrain-cache");
    std::fs::create_dir(&cache_dir).expect("cache root");
    std::fs::set_permissions(&cache_dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .expect("private cache root");
    let hgt = vec![0_u8; crate::terrain::SRTM3_BYTES as usize];
    std::fs::write(cache_dir.join("N51W001.hgt"), &hgt).expect("cached tile");
    std::fs::write(cache_dir.join("N51W001.source"), "aws").expect("source marker");
    store
        .record_terrain_tile_digest("N51W001", &digest_text(&hgt), 1)
        .expect("digest");
}

#[test]
fn p3bf3_the_fleet_supervisor_hands_its_own_guard_to_the_terrain_worker() {
    // The Fleet loop's start over stored credentials and unroutable Fleet clients, with
    // the terrain worker enabled over a cached tile: the run's audit trips at the worker's
    // first boundary, the run ends with the sensitive failure and nothing follows it.
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = p3bf3_seed_the_fleet_store(&temporary);
    p3bf2_terrain_candidates(&store);
    p3bf3_cached_terrain(&temporary, &store);
    let cursor_key = CursorKey::from_bytes([0x5D; 32]);
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
    let manager = Arc::new(tokio::sync::Mutex::new(
        FleetAuthManager::from_store_for_admitted_user(
            store.clone(),
            temporary.path(),
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
    let mut config = supervised_restart_test_config(temporary.path());
    config.collector.provider = crate::config::CollectorProvider::Fleet;
    config.terrain.enabled = true;
    let (ready_tx, _ready_rx) = oneshot::channel();
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let at_trip = Arc::new(Mutex::new(None));
    {
        let observed = store.clone();
        let slot = Arc::clone(&at_trip);
        seam.trip_write_guard_at(
            "terrain-lookup",
            1,
            Arc::new(move || {
                *slot.lock().expect("stamp") = Some(whole_store_stamp(&observed));
            }),
        );
    }
    let task_store = store.clone();
    let task_seam = Arc::clone(&seam);
    let result = guard_block_on(async move {
        timeout(
            SUPERVISED_RESTART_WAIT,
            SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
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
                )
                .await
            }),
        )
        .await
        .expect("the run ends by itself")
    });
    let audit = seam.write_audit();
    assert!(audit.tripped(), "the worker never reached the run's guard");
    assert!(
        result.as_ref().is_err_and(loses_sensitive_access),
        "the run ends with the sensitive failure: {result:?}"
    );
    assert!(
        Some(whole_store_stamp(&store)) == *at_trip.lock().expect("stamp"),
        "a row, a pack file or a mutation attempt followed the refused boundary"
    );
}

// ---- a loss met during an in-loop stop (R-P3bF3-3) ----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p3bf3_a_loss_met_during_an_in_loop_stop_ends_the_run_with_the_sensitive_failure() {
    // The loop's settings stop of a car whose stream run ends with an empty queue: the
    // run's fence is set by the loss at the very moment the run answers the stop signal
    // (the admission untouched and valid). No frame reaches the scheduler's intake
    // guard, so only the stop's own check of the drain budget (installed at the start of
    // the loop with the run's guard) can see it: the stop is the sensitive failure and
    // the run ends there, instead of going on to the next step of the iteration.
    let met_at_iteration = Arc::new(AtomicUsize::new(0));
    let witness = Arc::clone(&met_at_iteration);
    let factory = Arc::new(move |injection: StreamInjection| {
        let witness = Arc::clone(&witness);
        let seam = Arc::clone(&injection.seam);
        tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(Arc::clone(&seam), async move {
            let StreamInjection { events, stop, .. } = injection;
            // The queue stays open and empty; the run ends when it is told to stop.
            let _events = events;
            if stop.await.is_ok() {
                witness.store(seam.loop_iterations(), Ordering::SeqCst);
                let fence = seam.admission_fence.lock().expect("fence").clone();
                fence.expect("the supervisor registered its fence").set();
            }
            Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
        }))
    });
    let mut bed = DrainBed::start_with_admission(1, true, |_, seam| {
        seam.inject_stream_runs(factory);
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1).await;
    // The car is retired: the next application of the settings stops its stream.
    bed.store
        .open()
        .expect("connection")
        .execute(
            "UPDATE vehicles SET retired_at_ms = ?1",
            [current_epoch_millis().expect("clock")],
        )
        .expect("retire the car");
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the loop ends at the stop", &mut task).await;
    bed.server.abort();
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{result:?}"
    );
    assert!(met_at_iteration.load(Ordering::SeqCst) > 0, "the run met the stop signal");
    assert!(seam.write_audit().refused("stream-frame") >= 1, "the stop consulted the guard");
    assert_eq!(
        seam.loop_iterations(),
        met_at_iteration.load(Ordering::SeqCst),
        "the loop went no further than the iteration the stop was in"
    );
}

// ---- the run ends with the sensitive outcome the credential unit's service met (Opus N4) -----------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p3bf3_the_supervised_run_ends_with_the_sensitive_outcome_the_unit_met_between_requests() {
    // The loop's own exchange ends unknown while the unit's admission check is denied
    // (once; the admission is intact at every other look). Nobody asks for a shutdown:
    // the first to apply the finished exchange is the loop's maintenance at the top of
    // an iteration. The run ends by itself with the unit's own outcome, as the unit named
    // it (not the bare loss), the renewal is joined and the lease retained, no second
    // request is sent, and the credential rows, receipts and pair are unchanged (compared
    // by row image; this test asserts no attempt counter, the families above do).
    let mut bed = LateBed::start_with(LateAnswer::Reset, true).await;
    timeout(SUPERVISED_RESTART_WAIT, bed.wire.arrived.notified())
        .await
        .expect("the loop's exchange reaches the token endpoint");
    assert_eq!(bed.wire.posts.load(Ordering::SeqCst), 1);
    let instance = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease")
        .instance_id;
    let credential_rows_before = auth_and_receipt_stamp(&bed.store);
    bed.denial.as_ref().expect("the denial switch").store(true, Ordering::SeqCst);
    bed.wire.release.notify_one();
    let mut task = bed.task.take().expect("task");
    let result = join_supervised_restart_task("the run ends with the unit's outcome", &mut task).await;
    bed.server.abort();
    assert!(
        matches!(
            &result,
            Err(CollectorError::LegacyAuthManager(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            )))
        ),
        "the unit's own outcome is the run's result: {result:?}"
    );
    assert!(
        bed.seam.loop_log().contains(&LoopEvent::RenewalJoined),
        "the renewal was joined"
    );
    let retained = bed
        .store
        .supervised_collector_lease_status()
        .expect("lease query")
        .expect("the lease is retained to expire");
    assert_eq!(retained.instance_id, instance);
    sleep(SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL + Duration::from_millis(300)).await;
    assert_eq!(
        bed.heartbeat_at(),
        retained.heartbeat_at_ms,
        "nothing renews the retained lease"
    );
    assert_eq!(bed.wire.posts.load(Ordering::SeqCst), 1, "no second request");
    assert!(
        auth_and_receipt_stamp(&bed.store) == credential_rows_before,
        "no auth state, receipt or pair write for the outcome"
    );
}

// ---- the Legacy constructor settles no key file after the fence (R-P3bF4-4) -------------------------------

/// The two interrupted replacement states the constructor settles: the current key matches
/// the stored pair and a previous generation is left over (it is removed), and the previous
/// generation matches while the current one does not (the current is deleted and the
/// previous restored over it).
#[derive(Clone, Copy, Debug)]
enum InterruptedReplacement {
    LeftoverPrevious,
    PreviousMatches,
}

fn p3bf4_interrupt_the_replacement(state: InterruptedReplacement, data_dir: &std::path::Path) {
    match state {
        InterruptedReplacement::LeftoverPrevious => {
            crate::teslamate_credentials::leave_previous_key_for_test(data_dir, b"left-over")
                .expect("a previous generation");
        }
        InterruptedReplacement::PreviousMatches => {
            crate::teslamate_credentials::begin_key_replacement_for_test(data_dir, b"newer-key")
                .expect("a replacement that did not commit");
        }
    }
}

#[test]
fn p3bf4_the_legacy_entries_settle_no_key_file_after_the_fence() {
    // The first consultation of the unit's observer is now in front of the settlement of
    // the interrupted replacement, which removes or restores a key file. It refuses; both
    // key files are as they were (names, inodes, modification times) and no file mutation
    // was attempted under the data directory, for the managed and the observer entry.
    for kind in [P3bf3Entry::Owner, P3bf3Entry::Observer] {
        for state in [
            InterruptedReplacement::LeftoverPrevious,
            InterruptedReplacement::PreviousMatches,
        ] {
            let outcome = p3bf3_entry_seeded(kind, ("credential", 1), |dir| {
                p3bf4_interrupt_the_replacement(state, dir);
            });
            let label = format!("{kind:?} {state:?}");
            p3bf3_assert_refused_at(&label, "credential", &outcome);
            assert!(
                outcome.at_trip.as_ref() == Some(&outcome.before),
                "{label}: nothing at all was written before the first consultation"
            );
            assert_eq!(
                outcome.secrets_before, outcome.secrets_after,
                "{label}: a key file was removed, replaced or restored"
            );
            assert!(
                outcome.secrets_before.contains("teslamate-encryption.previous.key"),
                "{label}: the previous generation was seeded"
            );
        }
    }
}

#[test]
fn p3bf4_the_admitted_legacy_entry_settles_the_interrupted_replacement() {
    // The control: with the guard admitting, the same two states are settled by the
    // constructor, and the guard was consulted in front of each mutation (once for the
    // removal; twice for the restore, which deletes the current key and renames the
    // previous over it). The tripped runs above would otherwise prove nothing.
    for (state, consultations) in [
        (InterruptedReplacement::LeftoverPrevious, 1_usize),
        (InterruptedReplacement::PreviousMatches, 2),
    ] {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = p3bf3_seed_the_owner_store(&temporary);
        p3bf4_interrupt_the_replacement(state, temporary.path());
        let stored = store
            .load_teslamate_legacy_tokens()
            .expect("tokens")
            .expect("stored pair");
        let consulted = std::cell::Cell::new(0_usize);
        let may_settle = || {
            consulted.set(consulted.get() + 1);
            true
        };
        let key = crate::teslamate_credentials::load_key_for_tokens_with(
            temporary.path(),
            &stored,
            &may_settle,
        )
        .expect("the admitted settlement");
        assert_eq!(key.as_bytes(), b"guard-cloak-key", "{state:?}");
        assert_eq!(consulted.get(), consultations, "{state:?}");
        assert!(
            !secrets_stamp(temporary.path()).contains("previous"),
            "{state:?}: the previous generation is gone"
        );
    }
}

// ---- the file mutation counter (R-P3bF4-7) ------------------------------------------------------------------

#[test]
fn p3bf4_the_stamp_counts_a_transient_file_write_the_final_files_do_not_show() {
    // The control: a file of the terrain cache root is created and removed again. Rows,
    // pack files and the SQL sink are as they were, so a stamp without the file counter
    // is blind to it; the stamp with the counter is not.
    let (temporary, bed) = GuardBed::new();
    let before = whole_store_stamp(&bed.store);
    let cache = temporary.path().join("terrain-cache");
    let root = crate::platform::private_directory::open_validated(&cache, true)
        .expect("a private cache root");
    drop(root.create_exclusive(".archive.1.0.tmp").expect("a transient archive"));
    root.remove(".archive.1.0.tmp").expect("removed again");
    let after = whole_store_stamp(&bed.store);
    let without_files = |stamp: &str| {
        stamp
            .lines()
            .filter(|line| !line.starts_with("files:"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        without_files(&before),
        without_files(&after),
        "the rows, the packs and the SQL sink do not show the transient write"
    );
    assert!(before != after, "the file counter does");
}

// ---- the durable digest fill of a migrated row stands behind the guard (hub fix pass 5, F4C-01) ----------------

/// The status row a migration leaves behind for a rejection counted before the digest
/// column existed: one counted rejection of the stored pair's generation, inside the fuse
/// window, no stop and no rejected-access digest.
fn p3bf5_seed_the_migrated_count_row(store: &HubStore) {
    let pair = OwnerTokens::from_secret_parts("guard-access".to_owned(), "guard-refresh".to_owned())
        .expect("the seeded pair");
    let now_ms = current_epoch_millis().expect("clock");
    store
        .write_collector_auth_row(
            &crate::db::CollectorAuthRow {
                state: "ready".to_owned(),
                reason: None,
                collecting: true,
                retry_at: None,
                stopped_generation: None,
                stopped_mode: None,
                fuse_consecutive: 1,
                fuse_generation: Some(
                    crate::teslamate_token::legacy_refresh_credential_generation(&pair),
                ),
                fuse_last_rejection_at: Some(now_ms / 1_000 - 10),
                fuse_pause_until: None,
                fuse_rejected_access_digest: None,
                clock_skew: false,
            },
            now_ms,
        )
        .expect("the migrated row");
}

/// What the credential unit's construction came to over the migrated row.
struct MigratedRowRun {
    _temporary: tempfile::TempDir,
    store: HubStore,
    result: Result<(), LegacyAuthManagerError>,
    before: String,
    at_trip: Option<String>,
    after: String,
    sink_before: usize,
    sink_after: usize,
    audit: Arc<WriteGuardAudit>,
}

/// Build the unit of an admitted run (the constructor the entry points use, behind the
/// run's one guard and its "credential" observer) over the store seeded with the migrated
/// row; the fence is set when the run reaches `trip` (the `nth` time), the admission
/// untouched and valid. With no `trip` the guard admits.
fn p3bf5_construct_over_the_migrated_row(trip: Option<(&'static str, usize)>) -> MigratedRowRun {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = p3bf3_seed_the_owner_store(&temporary);
    p3bf5_seed_the_migrated_count_row(&store);
    let config = supervised_restart_test_config(temporary.path());
    let endpoint = AuthEndpointConfig {
        region: Some(crate::tesla_stream::StreamRegion::Global),
        ..AuthEndpointConfig::from_config(&config, false)
    };
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).expect("admit");
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let at_trip = Arc::new(Mutex::new(None));
    if let Some((point, nth)) = trip {
        let observed = store.clone();
        let slot = Arc::clone(&at_trip);
        seam.trip_write_guard_at(
            point,
            nth,
            Arc::new(move || {
                *slot.lock().expect("stamp") = Some(whole_store_stamp(&observed));
            }),
        );
    }
    let before = whole_store_stamp(&store);
    let sink_before = store.sink_attempts();
    let unit_store = store.clone();
    let data_dir = temporary.path().to_path_buf();
    let result = guard_block_on(SUPERVISED_COLLECTOR_TEST_SEAM.scope(
        Arc::clone(&seam),
        async move {
            let guard = RunWriteGuard::for_run(Some(Arc::clone(&admission)));
            LegacyAuthManager::from_hub_teslamate_store_for_admitted_user(
                unit_store,
                &data_dir,
                &endpoint,
                admission,
                Some(guard.observer("credential")),
            )
            .map(drop)
        },
    ));
    MigratedRowRun {
        result,
        before,
        at_trip: at_trip.lock().expect("stamp").clone(),
        after: whole_store_stamp(&store),
        sink_before,
        sink_after: store.sink_attempts(),
        audit: seam.write_audit(),
        store,
        _temporary: temporary,
    }
}

#[test]
fn p3bf5_null_digest_fill_stands_behind_the_write_guard() {
    // F4C-01: a count-1 row with no digest and no stop (as the migration leaves it) makes
    // the unit's construction fill the digest durably, before the generation bind. The
    // control lets the guard admit: the fill is reached, and it is the first consultation
    // of the guard and a write that moves the stamp. Armed at that first "credential"
    // consultation, the construction is refused with the sensitive failure and the whole
    // store (rows, pack files and the mutation sink) is as it was: no digest, no bind, no
    // status row. Without the guard in front of the fill, it writes after the observed loss.
    let admitted = p3bf5_construct_over_the_migrated_row(None);
    assert!(
        admitted.result.is_ok(),
        "an admitting guard lets the construction go on: {:?}",
        admitted.result
    );
    let row = admitted
        .store
        .load_collector_auth_row()
        .expect("row")
        .expect("the status row");
    assert_eq!(row.fuse_consecutive, 1);
    assert_eq!(
        row.fuse_rejected_access_digest,
        Some(crate::teslamate_token::legacy_access_token_digest("guard-access")),
        "the construction filled the digest of the stored pair's access token"
    );
    assert!(
        admitted.audit.passed("credential") >= 1,
        "the unit consulted the guard before it wrote"
    );
    assert!(admitted.before != admitted.after, "the stamp sees the fill");
    assert!(admitted.sink_after > admitted.sink_before, "and so does the sink");

    let armed = p3bf5_construct_over_the_migrated_row(Some(("credential", 1)));
    assert!(
        armed
            .result
            .as_ref()
            .err()
            .is_some_and(|error| error.is_sensitive_access_failure()),
        "the construction is refused with the sensitive failure: {:?}",
        armed.result
    );
    assert!(armed.audit.tripped(), "the fill never consulted the guard");
    assert!(armed.audit.refused("credential") >= 1, "the boundary did not refuse");
    assert_eq!(armed.audit.total_passed(), 0, "no consultation let a write through");
    assert!(
        armed.at_trip.as_ref() == Some(&armed.before),
        "the fill is the first write of the unit: nothing preceded the refused boundary"
    );
    assert!(
        armed.after == armed.before,
        "a row, a pack file or a mutation attempt followed the refused boundary: {:?}",
        stamp_difference(&armed.before, &armed.after)
    );
    assert_eq!(armed.sink_before, armed.sink_after, "no mutation was attempted");
    let row = armed
        .store
        .load_collector_auth_row()
        .expect("row")
        .expect("the status row");
    assert_eq!(row.fuse_consecutive, 1, "the counted rejection is untouched");
    assert!(row.fuse_rejected_access_digest.is_none(), "no digest was written");
}
