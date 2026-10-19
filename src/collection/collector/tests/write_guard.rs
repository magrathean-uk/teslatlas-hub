// SPDX-License-Identifier: AGPL-3.0-only

// B2 fix round, P3b fix 2 (R-P3bF2-1, R-P3bF2-3): every store mutation a supervised
// Owner or Fleet run can reach is preceded by the run's one synchronous write guard
// (the shared fence, then the retained admission, setting the fence when it fails), with
// no await between the guard and the mutation. Each family below arms a boundary: the
// fence is set exactly when the run reaches it (an acknowledged event of the run, with
// the admission untouched and valid), the whole store is read at that moment, and the
// run must end with the sensitive failure having written nothing more. The audit of the
// guard says which boundary refused. A second set of tests leaves the fence clear and
// takes the admission away instead (the paths that used to look at the fence only). The
// data is synthetic and defined here.

/// Run `future` to its end on a runtime of its own (the scenarios are plain functions, so
/// a temporary mutant array can call them too).
fn guard_block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

fn digest_text(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Every row of every table of the store but the lease's heartbeat, the pack files with
/// their inode and modification time, the count of mutations the store's connections
/// attempted (R-P3bF3-4, the sink) and the count of the file mutations attempted under its
/// data directory by the counted helpers only (R-P3bF4-7; the list is in
/// `platform/file_attempts.rs`): what a zero-write proof compares
/// (`collector_rows_stamp` leaves identities, settings, claims, catalogues, address state
/// and pack output out).
fn whole_store_stamp(store: &HubStore) -> String {
    use rusqlite::types::ValueRef;
    let connection = store.open().expect("connection");
    let tables: Vec<String> = {
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_master
                  WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                    AND name <> 'supervised_collector_lease'
                  ORDER BY name",
            )
            .expect("tables");
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("table names")
            .collect::<Result<_, _>>()
            .expect("table names")
    };
    let mut stamp = String::new();
    for table in tables {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{table}\""))
            .expect("table rows");
        let columns = statement.column_count();
        let mut rows: Vec<String> = statement
            .query_map([], |row| {
                let mut cells = Vec::with_capacity(columns);
                for index in 0..columns {
                    cells.push(match row.get_ref(index)? {
                        ValueRef::Null => "~".to_owned(),
                        ValueRef::Integer(value) => value.to_string(),
                        ValueRef::Real(value) => value.to_string(),
                        ValueRef::Text(value) => String::from_utf8_lossy(value).into_owned(),
                        ValueRef::Blob(value) => digest_text(value),
                    });
                }
                Ok(cells.join("|"))
            })
            .expect("rows")
            .collect::<Result<_, _>>()
            .expect("rows");
        rows.sort();
        stamp.push_str(&format!(
            "{table}:{}:{}\n",
            rows.len(),
            digest_text(rows.join("\n").as_bytes())
        ));
    }
    let mut files = Vec::new();
    let mut pending = vec![store.packs_dir().to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.metadata() {
                Ok(metadata) if metadata.is_dir() => pending.push(path),
                // The inode and the modification time as well: an idempotent rewrite of a
                // pack file changes neither its name nor its length (R-P3bF3-4).
                Ok(metadata) => {
                    use std::os::unix::fs::MetadataExt as _;
                    files.push(format!(
                        "{}:{}:{}:{}.{}",
                        path.display(),
                        metadata.len(),
                        metadata.ino(),
                        metadata.mtime(),
                        metadata.mtime_nsec()
                    ));
                }
                Err(_) => {}
            }
        }
    }
    files.sort();
    stamp.push_str(&files.join("\n"));
    // The mutations the store's connections attempted, counted at the SQLite boundary
    // and independently of every guard (R-P3bF3-4): a write that leaves the rows as they
    // were (an idempotent upsert, an ignored insert, an empty delete) still moves it.
    stamp.push_str(&format!("\nsink:{}", store.sink_attempts()));
    // The file mutations attempted under the data directory through the counted helpers:
    // the terrain cache root, the pack output staging and its directory syncs, the
    // lineage inspection files and the credential files, counted independently of every
    // guard (R-P3bF4-7): a transient file that is created, filled and removed leaves the
    // files as they were and moves this. Other file mutations are not counted: the
    // resident control socket's unlink, bind and chmod and its removal when dropped, the
    // pack removals of repair, import and the updates delivery, the schema-22 publication,
    // the store opening's mkdir and chmod, and credential recovery and backup.
    stamp.push_str(&format!("\nfiles:{}", store_file_attempts(store)));
    stamp
}

/// The lines of two stamps that differ (for an assertion message).
fn stamp_difference(first: &str, second: &str) -> Vec<(String, String)> {
    let (first, second): (Vec<_>, Vec<_>) = (first.lines().collect(), second.lines().collect());
    (0..first.len().max(second.len()))
        .filter_map(|index| {
            let (left, right) = (
                first.get(index).copied().unwrap_or_default(),
                second.get(index).copied().unwrap_or_default(),
            );
            (left != right).then(|| (left.to_owned(), right.to_owned()))
        })
        .collect()
}

/// The file mutation attempts recorded under the data directory of `store`.
fn store_file_attempts(store: &HubStore) -> usize {
    store
        .database_path()
        .parent()
        .map_or(0, crate::platform::file_attempts::count_under)
}

/// A store, its retained admission and a write guard over both, with the arming of one
/// boundary: the fence is set when the run reaches it and the whole store is read then.
struct GuardBed {
    store: HubStore,
    data_dir: std::path::PathBuf,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    guard: RunWriteGuard,
    at_trip: Arc<Mutex<Option<String>>>,
}

impl GuardBed {
    /// A bed over a store of its own (the directory stays with the returned guard).
    fn new() -> (tempfile::TempDir, Self) {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let bed = Self::over(&store, temporary.path());
        (temporary, bed)
    }

    /// A bed over a store somebody else made.
    fn over(store: &HubStore, data_dir: &std::path::Path) -> Self {
        let admission =
            crate::hub_user_process::AdmittedUserHub::for_test(data_dir).expect("admit");
        let guard = RunWriteGuard::new(AdmissionFence::default(), Some(Arc::clone(&admission)));
        Self {
            store: store.clone(),
            data_dir: data_dir.to_path_buf(),
            admission,
            guard,
            at_trip: Arc::new(Mutex::new(None)),
        }
    }

    /// Arm `point`: the run's fence is set the `nth` time it is reached, and the store
    /// is read at that very moment.
    fn trip_at_nth(&self, point: &'static str, nth: usize) {
        let store = self.store.clone();
        let slot = Arc::clone(&self.at_trip);
        self.guard.audit().trip_at_nth(point, nth, move || {
            *slot.lock().expect("stamp") = Some(whole_store_stamp(&store));
        });
    }

    fn trip_at(&self, point: &'static str) {
        self.trip_at_nth(point, 1);
    }

    /// The armed boundary was reached, it refused (and nothing let a write through
    /// there), and the store has not changed since: no write followed the fence, and
    /// the cleanup of the failure wrote nothing either.
    fn assert_held(&self, point: &str) {
        let audit = self.guard.audit();
        assert!(audit.tripped(), "{point}: the boundary was never reached");
        assert!(audit.refused(point) >= 1, "{point}: the boundary did not refuse");
        let at_trip = self
            .at_trip
            .lock()
            .expect("stamp")
            .clone()
            .expect("the store was read at the trip");
        assert_eq!(
            whole_store_stamp(&self.store),
            at_trip,
            "{point}: a write followed the refused boundary"
        );
    }

    /// Take the admission away and keep the fence clear: only the guard's own
    /// revalidation can refuse now.
    fn deny_the_admission(&self) {
        swap_the_admission_lock_away(&self.data_dir);
        assert!(
            assert_runtime_sensitive_access(Some(&*self.admission)).is_err(),
            "the admission is gone"
        );
        assert!(!self.guard.fence().is_set(), "the fence is still clear");
    }
}

fn refused_with_the_sensitive_failure<T: std::fmt::Debug>(point: &str, result: &Result<T, CollectorError>) {
    assert!(
        matches!(result, Err(CollectorError::SensitiveAccessUnavailable)),
        "{point}: {result:?}"
    );
}

// ---- the guard core --------------------------------------------------------------------------------

#[test]
fn p3bf5_the_trip_sets_the_fence_before_its_action_runs() {
    // B2 fix round, P3b fix 5 (R-P3bF5-4): the stamp a test takes at its trip must be
    // taken with the fence already set, so that every other thread's boundary meets the
    // fence from the moment the stamp is read; and the trip counts as reached only once
    // the action is over, so a test that waits for it holds a finished stamp.
    let audit = Arc::new(WriteGuardAudit::default());
    let fence = AdmissionFence::default();
    let fence_in_action = Arc::new(AtomicBool::new(false));
    let tripped_in_action = Arc::new(AtomicBool::new(true));
    {
        let (fence, audit) = (fence.clone(), Arc::clone(&audit));
        let (fence_seen, tripped_seen) =
            (Arc::clone(&fence_in_action), Arc::clone(&tripped_in_action));
        audit.clone().trip_at_nth("p3bf5-point", 2, move || {
            fence_seen.store(fence.is_set(), Ordering::SeqCst);
            tripped_seen.store(audit.tripped(), Ordering::SeqCst);
        });
    }
    audit.trip("p3bf5-point", &fence);
    assert!(!fence.is_set(), "the first arrival is not the armed one");
    audit.trip("p3bf5-other", &fence);
    assert!(!fence.is_set(), "another point is not the armed one");
    audit.trip("p3bf5-point", &fence);
    assert!(
        fence_in_action.load(Ordering::SeqCst),
        "the fence was still clear when the action ran"
    );
    assert!(
        !tripped_in_action.load(Ordering::SeqCst),
        "the trip was published before the action was over"
    );
    assert!(fence.is_set());
    assert!(audit.tripped());
}

#[test]
fn p3bf2_the_guard_refuses_a_set_fence_then_a_lost_admission_and_stays_refused() {
    let (_temporary, bed) = GuardBed::new();
    // Nothing is wrong: the write passes and the audit says so.
    assert!(bed.guard.check("core-clear").is_ok());
    assert_eq!(bed.guard.audit().passed("core-clear"), 1);
    // The fence is set while the admission is valid: refused, whatever the admission says.
    bed.guard.fence().set();
    assert!(assert_runtime_sensitive_access(Some(&*bed.admission)).is_ok());
    refused_with_the_sensitive_failure("core-fence", &bed.guard.check("core-fence"));
    assert_eq!(bed.guard.audit().refused("core-fence"), 1);

    // A guard whose fence is clear but whose admission is gone: refused, and the refusal
    // sets the fence, so it is irreversible although the admission comes back.
    let (temporary, second) = GuardBed::new();
    second.deny_the_admission();
    refused_with_the_sensitive_failure("core-admission", &second.guard.check("core-admission"));
    assert!(second.guard.fence().is_set(), "the failed admission set the fence");
    restore_the_admission_lock(temporary.path());
    assert!(assert_runtime_sensitive_access(Some(&*second.admission)).is_ok());
    refused_with_the_sensitive_failure("core-after", &second.guard.check("core-after"));
    assert_eq!(second.guard.audit().passed("core-after"), 0);
}

#[test]
fn p3bf2_the_observer_the_credential_units_share_is_the_guard_itself() {
    // The Legacy and Fleet units get the guard as their loss observer: it reports a lost
    // admission too (fence still clear), sets the fence when it does, and stays set.
    let (temporary, bed) = GuardBed::new();
    let observer = bed.guard.observer("credential-core");
    assert!(!observer(), "nothing is wrong");
    bed.deny_the_admission();
    assert!(observer(), "the admission is gone: the observer says so");
    assert!(bed.guard.fence().is_set(), "and the fence is set");
    restore_the_admission_lock(temporary.path());
    assert!(observer(), "the loss stays");
    assert_eq!(bed.guard.audit().refused("credential-core"), 2);
}

// ---- the scheduled inventory and the identities -----------------------------------------------------

fn p3bf2_inventory(point: &'static str) {
    let (_temporary, bed) = GuardBed::new();
    bed.trip_at(point);
    let result = accept_scheduled_provider_inventory_guarded(
        &bed.store,
        CollectorProvider::Legacy,
        Ok(vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")]),
        Some(&bed.guard),
    );
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_the_scheduled_inventory_writes_nothing_after_the_fence() {
    for point in ["inventory-source", "inventory-reconcile"] {
        p3bf2_inventory(point);
    }
}

fn p3bf2_identity(point: &'static str) {
    let (_temporary, bed) = GuardBed::new();
    bed.trip_at(point);
    let result = IdentityCache::default().resolve_guarded(
        &bed.store,
        CollectorProvider::Legacy,
        &Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online"),
        1_000,
        Some(&bed.guard),
    );
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_an_identity_registers_nothing_after_the_fence() {
    for point in ["identity-source", "identity-vehicle", "identity-car-id"] {
        p3bf2_identity(point);
    }
}

// ---- stream frames ------------------------------------------------------------------------------------

/// A scheduler with one Online car whose store has the car configured, under `bed`'s guard.
fn p3bf2_frame_rig(bed: &GuardBed) -> (Core, HashMap<VehicleId, StreamContext>) {
    guard_block_on(finish_collection_for_provider(
        &bed.store,
        &CursorKey::from_bytes([0x5D; 32]),
        &ManualCollection {
            vehicles: vec![Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online")],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Legacy,
    ))
    .expect("configure the car");
    let mut core = Core::owner();
    let t0 = core.t0;
    core.scheduler.accept_discovery(vec![core_vehicle(1, "online")], t0);
    core.scheduler.guard_writes_with(bed.guard.clone());
    (core, HashMap::new())
}

fn p3bf2_stream_frame(point: &'static str) {
    let (_temporary, bed) = GuardBed::new();
    let (mut core, mut cache) = p3bf2_frame_rig(&bed);
    bed.trip_at(point);
    let update = frame(
        current_epoch_millis().expect("clock") - 60_000,
        Some("P"),
        Some(0),
        Some(0),
    );
    let result = process_stream_telemetry_with_cache(
        &bed.store,
        &mut core.scheduler,
        core_id(1),
        &update,
        Duration::ZERO,
        &mut cache,
    );
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_a_stream_frame_and_its_identity_write_nothing_after_the_fence() {
    for point in [
        "stream-context-source",
        "stream-context-vehicle",
        "stream-context-car-id",
        "stream-frame-intake",
    ] {
        p3bf2_stream_frame(point);
    }
}

#[test]
fn p3bf2_a_stream_frame_is_written_while_the_guard_allows() {
    // The control of the family: the same rig, nothing armed, writes the frame.
    let (_temporary, bed) = GuardBed::new();
    let (mut core, mut cache) = p3bf2_frame_rig(&bed);
    let update = frame(
        current_epoch_millis().expect("clock") - 60_000,
        Some("P"),
        Some(0),
        Some(0),
    );
    let before = whole_store_stamp(&bed.store);
    let stored = process_stream_telemetry_with_cache(
        &bed.store,
        &mut core.scheduler,
        core_id(1),
        &update,
        Duration::ZERO,
        &mut cache,
    )
    .expect("the frame");
    assert!(stored);
    assert_ne!(whole_store_stamp(&bed.store), before, "a frame is a write");
    assert_eq!(bed.guard.audit().refused("stream-frame-intake"), 0);
    assert!(bed.guard.audit().passed("stream-frame-intake") >= 1);
}

#[test]
fn p3bf2_a_frame_is_refused_when_the_admission_is_gone_and_the_fence_is_clear() {
    // The drain boundaries used to look at the fence only; now the admission counts.
    let (_temporary, bed) = GuardBed::new();
    let (mut core, mut cache) = p3bf2_frame_rig(&bed);
    bed.deny_the_admission();
    let before = whole_store_stamp(&bed.store);
    let update = frame(
        current_epoch_millis().expect("clock") - 60_000,
        Some("P"),
        Some(0),
        Some(0),
    );
    let result = process_stream_telemetry_with_cache(
        &bed.store,
        &mut core.scheduler,
        core_id(1),
        &update,
        Duration::ZERO,
        &mut cache,
    );
    refused_with_the_sensitive_failure("frame-admission", &result);
    assert!(bed.guard.fence().is_set(), "the refusal set the fence");
    assert_eq!(whole_store_stamp(&bed.store), before, "no frame, no identity");

    // The budget's per-frame check of the queued flush looks at the admission too.
    let (_other, second) = GuardBed::new();
    let (mut core, _cache) = p3bf2_frame_rig(&second);
    second.deny_the_admission();
    let mut cache = HashMap::new();
    let result = store_queued_stream_frames(
        &second.store,
        &mut core.scheduler,
        core_id(1),
        vec![StreamEvent::Telemetry {
            update: Box::new(frame(
                current_epoch_millis().expect("clock") - 60_000,
                Some("P"),
                Some(0),
                Some(0),
            )),
            queued_at: Instant::now(),
        }],
        &mut cache,
        Some(&second.guard),
    );
    refused_with_the_sensitive_failure("queued-flush-admission", &result);
    assert!(second.guard.audit().refused("stream-frame") >= 1);
}

// ---- the tick: identities, the gate, the transaction and the publication -----------------------------

/// A discovery batch for car 1, which the tick bed has not registered, under `bed`'s guard.
fn p3bf2_tick_rig() -> (TickBed, GuardBed) {
    let mut tick = TickBed::owner();
    let bed = GuardBed::over(&tick.store, tick._dir.path());
    tick.scheduler.guard_writes_with(bed.guard.clone());
    (tick, bed)
}

fn p3bf2_discovery_batch(tick: &mut TickBed, register: bool) -> TickBatch {
    let now = tick.now();
    tick.scheduler.begin_tick();
    let candidates = tick
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], now);
    let received_at_ms = tick.wall();
    if register {
        for candidate in &candidates {
            tick.register(candidate);
        }
    }
    TickBatch {
        discoveries: candidates
            .into_iter()
            .map(|vehicle| DiscoveryItem {
                vehicle,
                received_at_ms,
            })
            .collect(),
        first_result_at: Some(now),
        ..TickBatch::default()
    }
}

fn p3bf2_commit(tick: &mut TickBed, batch: TickBatch) -> Result<TickOutcome, CollectorError> {
    let now = tick.now();
    let commit = TickCommit {
        store: &tick.store,
        cursor_key: &tick.cursor_key,
        provider: tick.provider,
        offline_drive_timeout: Duration::from_secs(600),
    };
    guard_block_on(commit_tick(
        &commit,
        &mut tick.identities,
        &mut tick.scheduler,
        batch,
        now,
    ))
}

fn p3bf2_tick(point: &'static str) {
    let (mut tick, bed) = p3bf2_tick_rig();
    bed.trip_at(point);
    let batch = p3bf2_discovery_batch(&mut tick, false);
    let result = p3bf2_commit(&mut tick, batch);
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_a_tick_commit_writes_nothing_after_the_fence() {
    for point in [
        "tick-commit",
        "tick-after-gate",
        "identity-source",
        "identity-vehicle",
        "identity-car-id",
        "tick-transaction",
    ] {
        p3bf2_tick(point);
    }
}

/// A snapshot batch of car 1, whose identity is registered, so the tick publishes.
fn p3bf2_snapshot_batch(tick: &mut TickBed) -> TickBatch {
    let vehicle = core_vehicle(1, "online");
    let now = tick.now();
    tick.scheduler.begin_tick();
    tick.scheduler.accept_discovery(vec![vehicle.clone()], now);
    tick.register(&vehicle);
    let item = tick.read_item(1, parked_fields());
    let mut batch = TickBatch::default();
    batch.snapshots.push(item);
    batch.note_result(now);
    batch
}

fn p3bf2_tick_publication(point: &'static str) {
    let (mut tick, bed) = p3bf2_tick_rig();
    let batch = p3bf2_snapshot_batch(&mut tick);
    bed.trip_at(point);
    let result = p3bf2_commit(&mut tick, batch);
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_the_publication_of_a_tick_writes_nothing_after_the_fence() {
    for point in [
        "publication-source",
        "publication-vehicle",
        "publication-car-id",
        "publication-seed-car",
        "publication-settings",
        "publication-sequence",
        "publication-pack",
        "publication-finalize",
    ] {
        p3bf2_tick_publication(point);
    }
}

#[test]
fn p3bf2_a_tick_waiting_for_the_publication_gate_meets_the_fence_set_meanwhile() {
    // Opus N1. The commit passes its first look, then waits for the publication gate,
    // which another holder has. The fence is set during the wait and the gate is
    // released: the commit must look again before it writes an identity or the tick.
    let (mut tick, bed) = p3bf2_tick_rig();
    let batch = p3bf2_discovery_batch(&mut tick, false);
    let holder = bed.store.try_acquire_publication_gate().expect("the gate");
    let stamp = whole_store_stamp(&bed.store);
    guard_block_on(async {
        let now = tick.now();
        let commit = TickCommit {
            store: &tick.store,
            cursor_key: &tick.cursor_key,
            provider: tick.provider,
            offline_drive_timeout: Duration::from_secs(600),
        };
        let mut commit_future = std::pin::pin!(commit_tick(
            &commit,
            &mut tick.identities,
            &mut tick.scheduler,
            batch,
            now,
        ));
        assert!(
            futures_util::poll!(commit_future.as_mut()).is_pending(),
            "the commit waits for the gate"
        );
        bed.guard.fence().set();
        drop(holder);
        let result = commit_future.await;
        refused_with_the_sensitive_failure("tick-after-gate", &result);
    });
    assert_eq!(whole_store_stamp(&bed.store), stamp, "nothing was written");
    assert_eq!(bed.guard.audit().refused("tick-after-gate"), 1);
    assert_eq!(bed.guard.audit().passed("tick-after-gate"), 0);
}

#[test]
fn p3bf2_a_tick_commit_is_refused_when_the_admission_is_gone_and_the_fence_is_clear() {
    // The tick commit used to look at the fence only.
    let (mut tick, bed) = p3bf2_tick_rig();
    let batch = p3bf2_discovery_batch(&mut tick, false);
    bed.deny_the_admission();
    let before = whole_store_stamp(&bed.store);
    let result = p3bf2_commit(&mut tick, batch);
    refused_with_the_sensitive_failure("tick-commit", &result);
    assert!(bed.guard.fence().is_set());
    assert_eq!(whole_store_stamp(&bed.store), before, "no identity, no tick");
}

// ---- the delta publication of a tick and the export replay ---------------------------------------------

fn p3bf2_tick_delta_publication(point: &'static str) {
    // The first tick publishes the base unguarded; the second finds a pending settings
    // mutation and publishes a delta behind the guard.
    let mut tick = TickBed::owner();
    let batch = p3bf2_snapshot_batch(&mut tick);
    p3bf2_commit(&mut tick, batch).expect("the base publication");
    let vehicle_id = tick.register(&core_vehicle(1, "online"));
    let binding = tick.store.v2_projection_binding(vehicle_id).expect("binding");
    tick.store
        .upsert_car_settings(
            vehicle_id,
            binding.selected_car_id,
            &crate::hub_pack::ProjectionCarSettings {
                enabled: false,
                ..crate::hub_pack::ProjectionCarSettings::default()
            },
        )
        .expect("a pending mutation");
    let bed = GuardBed::over(&tick.store, tick._dir.path());
    tick.scheduler.guard_writes_with(bed.guard.clone());
    tick.advance(Duration::from_secs(5));
    let batch = p3bf2_snapshot_batch(&mut tick);
    bed.trip_at(point);
    let result = p3bf2_commit(&mut tick, batch);
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
    if point == "export-delta-pack" || point == "export-delta-commit" {
        assert!(
            bed.guard.audit().refused("publication-sync-release") >= 1,
            "{point}: the release of the claim after the failure is a write too"
        );
    }
}

#[test]
fn p3bf2_the_delta_publication_of_a_tick_writes_nothing_after_the_fence() {
    for point in [
        "publication-sync-claim",
        "export-delta-pack",
        "export-delta-commit",
    ] {
        p3bf2_tick_delta_publication(point);
    }
}

/// The published base of car 9, its outbox cleared, and one pending settings mutation
/// with its outbox row: what the replay of the outbox has to publish.
struct ExportRig {
    temporary: tempfile::TempDir,
    store: HubStore,
    cursor_key: CursorKey,
    vehicle: Vehicle,
    vehicle_id: Uuid,
    now: i64,
}

impl ExportRig {
    fn collection(vehicle: &Vehicle, now: i64) -> ManualCollection {
        ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {"timestamp": now},
                    "vehicle_config": {"car_type": "model3"},
                    "vehicle_state": {"car_version": "2026.20"}
                }),
            )],
            failures: vec![],
        }
    }

    fn vehicle_id(store: &HubStore) -> Uuid {
        store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("vehicle UUID")
    }

    /// Car 9 stored, not published: the outbox row is pending and the car has no base.
    fn without_base() -> Self {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
        let now = 1_800_000_000_000_i64;
        let collection = Self::collection(&vehicle, now);
        persist_collection(&store, &collection, now).expect("raw observation");
        materialise_lifecycle_for_collection(&store, &collection, now).expect("lifecycle");
        let vehicle_id = Self::vehicle_id(&store);
        Self {
            temporary,
            store,
            cursor_key: CursorKey::from_bytes([0x2B; 32]),
            vehicle,
            vehicle_id,
            now,
        }
    }

    fn with_base() -> Self {
        let rig = Self::without_base();
        let publication_gate = rig
            .store
            .try_acquire_publication_gate()
            .expect("publication gate");
        publish_compatibility_snapshots(
            &rig.store,
            &publication_gate,
            &rig.cursor_key,
            &Self::collection(&rig.vehicle, rig.now),
            rig.now,
        )
        .expect("base publication");
        drop(publication_gate);
        guard_block_on(replay_export_outbox(
            &rig.store,
            &rig.cursor_key,
            std::slice::from_ref(&rig.vehicle),
            rig.now,
        ))
        .expect("clear the base outbox");
        rig.pend_a_mutation();
        rig
    }

    /// One pending settings mutation and its outbox row.
    fn pend_a_mutation(&self) {
        let binding = self
            .store
            .v2_projection_binding(self.vehicle_id)
            .expect("binding");
        self.store
            .upsert_car_settings(
                self.vehicle_id,
                binding.selected_car_id,
                &crate::hub_pack::ProjectionCarSettings {
                    enabled: false,
                    ..crate::hub_pack::ProjectionCarSettings::default()
                },
            )
            .expect("settings mutation");
        let connection = self.store.open().expect("database");
        connection
            .execute(
                "INSERT OR IGNORE INTO export_outbox(
                    vehicle_id, dirty_revision, attempts, next_attempt_ms,
                    claimed_until_ms, last_error
                 ) VALUES (?1, 1, 0, 0, 0, NULL)",
                rusqlite::params![self.vehicle_id.to_string()],
            )
            .expect("outbox row");
    }

    fn replay(&self, vehicles: &[Vehicle], guard: &RunWriteGuard) -> Result<usize, CollectorError> {
        guard_block_on(replay_export_outbox_guarded(
            &self.store,
            &self.cursor_key,
            vehicles,
            self.now + 10,
            false,
            Some(guard),
        ))
    }
}

fn p3bf2_export_delta(point: &'static str) {
    let rig = ExportRig::with_base();
    let bed = GuardBed::over(&rig.store, rig.temporary.path());
    bed.trip_at(point);
    let result = if point == "export-failure-delta" {
        // The delta cannot be published for a reason of its own (the pack directory is
        // not writable): the cleanup releases the sync claim, and the fence is set
        // just before the outbox row would be marked failed.
        let packs = rig.store.packs_dir().to_path_buf();
        std::fs::set_permissions(&packs, std::fs::Permissions::from_mode(0o500))
            .expect("read-only packs");
        let result = rig.replay(std::slice::from_ref(&rig.vehicle), &bed.guard);
        std::fs::set_permissions(&packs, std::fs::Permissions::from_mode(0o700))
            .expect("writable packs");
        result
    } else {
        rig.replay(std::slice::from_ref(&rig.vehicle), &bed.guard)
    };
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
    if matches!(point, "export-delta-pack" | "export-delta-commit") {
        assert!(
            bed.guard.audit().refused("export-failure-release-sync") >= 1,
            "{point}: the cleanup of the failure is a write too"
        );
    }
}

#[test]
fn p3bf2_the_export_replay_of_a_delta_writes_nothing_after_the_fence() {
    for point in [
        "export-claim",
        "export-sync-claim",
        "export-delta-pack",
        "export-delta-commit",
        "export-complete",
        "export-failure-delta",
    ] {
        p3bf2_export_delta(point);
    }
}

fn p3bf2_export_release(point: &'static str) {
    // Another holder has the sync claim: the replay releases the outbox row.
    let rig = ExportRig::with_base();
    let _held = rig
        .store
        .claim_sync_mutations(rig.vehicle_id, rig.now + 10, 100)
        .expect("the other claim")
        .expect("a pending mutation");
    let bed = GuardBed::over(&rig.store, rig.temporary.path());
    bed.trip_at(point);
    let result = rig.replay(std::slice::from_ref(&rig.vehicle), &bed.guard);
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_the_export_replay_releases_nothing_after_the_fence() {
    p3bf2_export_release("export-release");
}

fn p3bf2_export_failure(point: &'static str) {
    // The failure paths of a car that has no base: the replay marks the outbox row failed.
    let rig = ExportRig::without_base();
    let bed = GuardBed::over(&rig.store, rig.temporary.path());
    bed.trip_at(point);
    let result = match point {
        // The car is not among the discovered ones.
        "export-failure-identity" => rig.replay(&[], &bed.guard),
        // Publishing fails (the pack directory cannot be written), not the guard.
        "export-failure-publication" => {
            let packs = rig.store.packs_dir().to_path_buf();
            std::fs::set_permissions(&packs, std::fs::Permissions::from_mode(0o500))
                .expect("read-only packs");
            let result = rig.replay(std::slice::from_ref(&rig.vehicle), &bed.guard);
            std::fs::set_permissions(&packs, std::fs::Permissions::from_mode(0o700))
                .expect("writable packs");
            result
        }
        other => panic!("no such failure point: {other}"),
    };
    // A failed publication is not the run's failure: the replay answers Ok(0) when it
    // may write its failure mark, and the sensitive failure when it may not.
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_the_export_replay_marks_no_failure_after_the_fence() {
    for point in ["export-failure-identity", "export-failure-publication"] {
        p3bf2_export_failure(point);
    }
}

fn p3bf2_export_compaction(point: &'static str) {
    let rig = ExportRig::with_base();
    // Two live deltas, as in the compaction proof; the tiny limit makes the third due.
    for index in 0..2_i64 {
        rig.store
            .upsert_car_settings(
                rig.vehicle_id,
                9,
                &crate::hub_pack::ProjectionCarSettings {
                    enabled: index == 1,
                    ..crate::hub_pack::ProjectionCarSettings::default()
                },
            )
            .expect("settings mutation");
        while let Some(claim) = rig
            .store
            .claim_sync_mutations(rig.vehicle_id, rig.now + 100 + index, 100)
            .expect("claim")
        {
            publish_v2_delta(&rig.store, &rig.cursor_key, &claim).expect("a live delta");
        }
    }
    let tiny_limit = ProtocolLimits {
        max_chunks: 4,
        ..ProtocolLimits::default()
    };
    let bed = GuardBed::over(&rig.store, rig.temporary.path());
    bed.trip_at(point);
    let result = compact_v2_lineage_if_needed_with_limits_guarded(
        &rig.store,
        &rig.cursor_key,
        rig.vehicle_id,
        tiny_limit,
        Some(&bed.guard),
    );
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_the_compaction_writes_nothing_after_the_fence() {
    for point in ["export-compaction-pack", "export-compaction-commit"] {
        p3bf2_export_compaction(point);
    }
}

// ---- address enrichment ----------------------------------------------------------------------------------

/// A store with one pending address job at (1.0, 2.0), a configuration whose geocoder is
/// enabled on an unroutable HTTPS endpoint (nothing leaves the machine), and the cached
/// answer for the job's place when `cached` says so.
fn p3bf2_enrichment_rig(cached: bool) -> (tempfile::TempDir, GuardBed, HubConfig) {
    let (temporary, bed) = GuardBed::new();
    let source = bed
        .store
        .register_source(&SourceDescriptor::new("test", "enrichment"), 1_000)
        .expect("source");
    let vehicle = bed
        .store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "vehicle"), 1_000)
        .expect("vehicle");
    bed.store
        .open()
        .expect("database")
        .execute(
            "INSERT INTO address_enrichment_jobs(
                    job_key, vehicle_id, target_type, target_id, field,
                    latitude, longitude, status, attempts, next_attempt_ms,
                    lease_until_ms
                 ) VALUES (?1, ?2, 'drive', 1, 'start_address', 1.0, 2.0,
                           'pending', 0, 0, 0)",
            rusqlite::params!["enrichment-job", vehicle.vehicle_id.to_string()],
        )
        .expect("address job");
    let mut config = supervised_restart_test_config(temporary.path());
    config.geocoder = crate::config::GeocoderConfig {
        enabled: true,
        endpoint: Some("https://127.0.0.1:9/".to_owned()),
        language: "en".to_owned(),
        timeout_seconds: 1,
    };
    if cached {
        let geocoder = Geocoder::new(&config.geocoder).expect("geocoder");
        bed.store
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
    }
    (temporary, bed, config)
}

fn p3bf2_enrichment(point: &'static str) {
    let (_temporary, bed, config) = p3bf2_enrichment_rig(point == "enrichment-complete");
    bed.trip_at(point);
    let result = guard_block_on(run_address_enrichment_once_guarded(
        &bed.store,
        &config,
        &CursorKey::from_bytes([7; 32]),
        &[],
        1_000,
        Some(&bed.admission),
        Some(&bed.guard),
    ));
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_enrichment_writes_nothing_after_the_fence() {
    // The claim; the completion of a job whose answer is cached; the retry of a job
    // whose lookup failed; the geocoder's own look before it sends.
    for point in [
        "enrichment-claim",
        "enrichment-complete",
        "enrichment-retry",
        "geocoder",
    ] {
        p3bf2_enrichment(point);
    }
}

// ---- the Fleet Telemetry counter flush ---------------------------------------------------------------------

fn p3bf2_telemetry_flush(point: &'static str) {
    let (_temporary, bed) = GuardBed::new();
    let core = Core::fleet();
    let ledger = core.scheduler.request_ledger();
    ledger.record_completed(RequestCategory::State, true);
    bed.trip_at(point);
    let result = flush_telemetry_request_counts(&bed.store, &ledger, &bed.guard);
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
}

#[test]
fn p3bf2_the_telemetry_counter_flush_writes_nothing_after_the_fence() {
    p3bf2_telemetry_flush("telemetry-counter-flush");
    // The control: with nothing armed the flush writes the counts.
    let (_temporary, bed) = GuardBed::new();
    let core = Core::fleet();
    let ledger = core.scheduler.request_ledger();
    ledger.record_completed(RequestCategory::State, true);
    let before = whole_store_stamp(&bed.store);
    flush_telemetry_request_counts(&bed.store, &ledger, &bed.guard).expect("the flush");
    assert_ne!(whole_store_stamp(&bed.store), before, "the counts were written");
}

// ---- the resident control's audit receipts ------------------------------------------------------------------

fn p3bf2_resident_legacy(point: &'static str) {
    guard_block_on(async {
        let rig = ResidentRig::new(crate::legacy_auth::AuthMode::Tesla).await;
        let bed = GuardBed::over(&rig.store, &rig.dir);
        bed.trip_at(point);
        let result = execute_resident_legacy_vehicle_action_guarded(
            &rig.store,
            &rig.client,
            &rig.manager,
            &rig.fuse,
            &rig.refresh,
            rig.hub_vehicle,
            LegacyVehicleAction::Wake,
            Some(&bed.guard),
        )
        .await;
        match point {
            "resident-begin" => assert!(
                matches!(result, Err(ResidentActionExecutionError::Authentication)),
                "{point}: {result:?}"
            ),
            _ => assert!(
                matches!(result, Err(ResidentActionExecutionError::CompletionRefused)),
                "{point}: {result:?}"
            ),
        }
        bed.assert_held(point);
    });
}

#[test]
fn p3bf2_a_resident_legacy_action_writes_no_receipt_after_the_fence() {
    for point in ["resident-begin", "resident-complete"] {
        p3bf2_resident_legacy(point);
    }
}

fn p3bf2_resident_fleet(point: &'static str) {
    crate::crypto::install_default_provider();
    let (temporary, bed) = GuardBed::new();
    guard_block_on(finish_collection_for_provider(
        &bed.store,
        &CursorKey::from_bytes([0x5E; 32]),
        &ManualCollection {
            vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    ))
    .expect("the selected car");
    let (hub_vehicle, _eid, _) = bed.store.configured_tesla_vehicles().expect("cars")[0].clone();
    crate::fleet_credentials::persist_fleet_setup_credentials(
        &bed.store,
        temporary.path(),
        &FleetSetupCredentials::new(
            "resident-fleet-access".to_owned(),
            "resident-fleet-refresh".to_owned(),
            "resident-fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("Fleet credentials"),
        SystemTime::now(),
    )
    .expect("persist the Fleet credentials");
    let mut manager =
        FleetAuthManager::from_store(bed.store.clone(), temporary.path()).expect("manager");
    if point == "resident-cancel" {
        // The first look (the refresh check) passes and the token's own look fails.
        let looks = Arc::new(AtomicUsize::new(0));
        manager.share_loss_fence(Arc::new(move || looks.fetch_add(1, Ordering::SeqCst) >= 1));
    }
    let manager = Arc::new(tokio::sync::Mutex::new(manager));
    let unroutable = url::Url::parse("http://127.0.0.1:9/").expect("URL");
    let api = FleetApi::for_fake_http(unroutable.clone(), Duration::from_secs(1)).expect("api");
    let auth_api = FleetAuthApi::for_fake_http(
        unroutable.join("oauth2/v3/token").expect("token URL"),
        Duration::from_secs(1),
    )
    .expect("auth api");
    let counter = RequestLedger::standalone(&bed.store, &core_cadence().fleet_policy())
        .expect("request counter");
    bed.trip_at(point);
    let result = guard_block_on(execute_resident_fleet_vehicle_action_guarded(
        &bed.store,
        &api,
        &auth_api,
        None,
        &manager,
        hub_vehicle,
        LegacyVehicleAction::Lock,
        &counter,
        Some(&bed.guard),
    ));
    match point {
        "resident-begin" => assert!(
            matches!(result, Err(ResidentActionExecutionError::Authentication)),
            "{point}: {result:?}"
        ),
        "resident-cancel" => assert!(
            matches!(result, Err(ResidentActionExecutionError::FleetCredential(_))),
            "{point}: {result:?}"
        ),
        _ => assert!(
            matches!(result, Err(ResidentActionExecutionError::CompletionRefused)),
            "{point}: {result:?}"
        ),
    }
    bed.assert_held(point);
}

#[test]
fn p3bf2_a_resident_fleet_action_writes_no_receipt_after_the_fence() {
    for point in ["resident-begin", "resident-cancel", "resident-complete"] {
        p3bf2_resident_fleet(point);
    }
}

// ---- the starts of the Owner loop, the Fleet loop and the configuration wait --------------------------------

/// What a supervised start came to: the run's result, the store before it, at the armed
/// boundary and after it, and the lease it left.
struct StartOutcome {
    result: Result<(), CollectorError>,
    before: String,
    at_trip: Option<String>,
    after: String,
    lease_left: bool,
    audit: Arc<WriteGuardAudit>,
}

/// Run the Owner loop's start (`run_supervised_with_access`) over a store with a due
/// Legacy pair and an unroutable Owner API: the fence is set when the run reaches
/// `trip` (the `nth` time), or, with `deny`, the admission is taken away first and
/// the fence stays clear. The run is given no shutdown: it must end by itself.
fn p3bf2_owner_start(trip: Option<(&'static str, usize)>, deny: bool) -> StartOutcome {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
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
    let config = supervised_restart_test_config(temporary.path());
    let (ready_tx, _ready_rx) = oneshot::channel();
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
    if deny {
        swap_the_admission_lock_away(temporary.path());
    }
    let before = whole_store_stamp(&store);
    let task_store = store.clone();
    let task_seam = Arc::clone(&seam);
    let task_admission = Arc::clone(&admission);
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
                    RunWriteGuard::for_run(Some(task_admission)),
                    std::future::pending::<()>(),
                )
                .await
            }),
        )
        .await
        .expect("the run ends by itself")
    });
    StartOutcome {
        result,
        before,
        at_trip: at_trip.lock().expect("stamp").clone(),
        after: whole_store_stamp(&store),
        lease_left: store
            .supervised_collector_lease_status()
            .expect("lease query")
            .is_some(),
        audit: seam.write_audit(),
    }
}

fn p3bf2_owner_start_at(point: &'static str) {
    let outcome = p3bf2_owner_start(Some((point, 1)), false);
    refused_with_the_sensitive_failure(point, &outcome.result);
    assert!(outcome.audit.tripped(), "{point}: never reached");
    assert!(outcome.audit.refused(point) >= 1, "{point}: not refused");
    assert_eq!(
        Some(outcome.after.clone()),
        outcome.at_trip,
        "{point}: a write followed the refused boundary"
    );
    match point {
        "start-lease" => {
            assert_eq!(outcome.after, outcome.before, "{point}: nothing at all was written");
            assert!(!outcome.lease_left, "{point}: no lease was taken");
        }
        // The lease was taken (the one write the start made before this boundary).
        _ => assert!(outcome.lease_left, "{point}: the lease is retained to expire"),
    }
}

#[test]
fn p3bf2_the_owner_start_writes_nothing_after_the_fence() {
    // The lease, the start transaction, and the credential unit's start application
    // (its status row), which used to run before the unit shared any fence.
    for point in ["start-lease", "start-run", "credential"] {
        p3bf2_owner_start_at(point);
    }
}

#[test]
fn p3bf2_the_owner_start_writes_nothing_when_the_admission_is_gone_and_the_fence_is_clear() {
    let outcome = p3bf2_owner_start(None, true);
    refused_with_the_sensitive_failure("owner-start-denied", &outcome.result);
    assert_eq!(outcome.after, outcome.before, "nothing was written");
    assert!(!outcome.lease_left, "no lease was taken");
    assert!(outcome.audit.refused("start-lease") >= 1);
}

/// The Fleet loop's start over stored credentials and unroutable Fleet clients.
fn p3bf2_fleet_start(trip: Option<(&'static str, usize)>, deny: bool) -> StartOutcome {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
        .expect("cursor key");
    guard_block_on(finish_collection_for_provider(
        &store,
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
    let (ready_tx, _ready_rx) = oneshot::channel();
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
    if deny {
        swap_the_admission_lock_away(temporary.path());
    }
    let before = whole_store_stamp(&store);
    let task_store = store.clone();
    let task_seam = Arc::clone(&seam);
    let task_admission = Arc::clone(&admission);
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
                    RunWriteGuard::for_run(Some(task_admission)),
                    false,
                    std::future::pending::<()>(),
                )
                .await
            }),
        )
        .await
        .expect("the run ends by itself")
    });
    StartOutcome {
        result,
        before,
        at_trip: at_trip.lock().expect("stamp").clone(),
        after: whole_store_stamp(&store),
        lease_left: store
            .supervised_collector_lease_status()
            .expect("lease query")
            .is_some(),
        audit: seam.write_audit(),
    }
}

#[test]
fn p3bf2_the_fleet_start_writes_nothing_after_the_fence() {
    for point in ["start-lease", "start-run", "iteration-top"] {
        let outcome = p3bf2_fleet_start(Some((point, 1)), false);
        refused_with_the_sensitive_failure(point, &outcome.result);
        assert!(outcome.audit.tripped(), "{point}: never reached");
        assert!(outcome.audit.refused(point) >= 1, "{point}: not refused");
        assert_eq!(
            Some(outcome.after.clone()),
            outcome.at_trip,
            "{point}: a write followed the refused boundary"
        );
        if point == "start-lease" {
            assert_eq!(outcome.after, outcome.before);
            assert!(!outcome.lease_left);
        } else {
            assert!(outcome.lease_left, "{point}: the lease is retained to expire");
        }
    }
}

#[test]
fn p3bf2_the_fleet_start_writes_nothing_when_the_admission_is_gone_and_the_fence_is_clear() {
    let outcome = p3bf2_fleet_start(None, true);
    refused_with_the_sensitive_failure("fleet-start-denied", &outcome.result);
    assert_eq!(outcome.after, outcome.before, "nothing was written");
    assert!(!outcome.lease_left, "no lease was taken");
}

/// The configuration wait over a store with a car: the lease, the wait's gap and the
/// release (the wait ends by its shutdown).
fn p3bf2_cadence_wait(point: &'static str) {
    guard_block_on(async {
        let _serial = WAIT_SERIAL.lock().await;
        let (temporary, store) = cadence_wait_hub();
        let config = cadence_wait_config(temporary.path(), WaitMode::Legacy, None);
        let violation = config.collector.cadence_violation().expect("violation");
        if point == "cadence-start-release" {
            // The wait's own record cannot be written (not the guard's doing): the
            // lease taken for it is given back, unless the guard refuses.
            store
                .open()
                .expect("database")
                .execute("DROP TABLE collection_gaps", [])
                .expect("break the gap record");
        }
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admission");
        let (ready_tx, _ready_rx) = oneshot::channel();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        let at_trip = Arc::new(Mutex::new(None));
        {
            let observed = store.clone();
            let slot = Arc::clone(&at_trip);
            seam.trip_write_guard_at(
                point,
                1,
                Arc::new(move || {
                    *slot.lock().expect("stamp") = Some(whole_store_stamp(&observed));
                }),
            );
        }
        if point == "cadence-release" {
            let _ = stop_tx.send(());
        }
        let clock: Arc<dyn CollectorClock> = ManualClock::new(Instant::now(), WAIT_WALL0);
        let task_store = store.clone();
        let task_seam = Arc::clone(&seam);
        let result = timeout(
            SUPERVISED_RESTART_WAIT,
            SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
                run_cadence_configuration_wait(
                    &task_store,
                    &config,
                    Arc::clone(&admission),
                    RunWriteGuard::for_run(Some(Arc::clone(&admission))),
                    ready_tx,
                    violation,
                    clock,
                    SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
                    async move {
                        let _ = stop_rx.await;
                    },
                )
                .await
            }),
        )
        .await
        .expect("the wait ends");
        let audit = seam.write_audit();
        assert!(audit.tripped(), "{point}: never reached");
        assert!(audit.refused(point) >= 1, "{point}: not refused");
        assert_eq!(
            Some(whole_store_stamp(&store)),
            at_trip.lock().expect("stamp").clone(),
            "{point}: a write followed the refused boundary"
        );
        match point {
            "cadence-start-release" => {
                // The lease taken for the wait stays: giving it back is a write.
                assert!(result.is_err(), "{point}: {result:?}");
                assert!(store.supervised_collector_lease_status().expect("lease").is_some());
            }
            "cadence-release" => {
                refused_with_the_sensitive_failure(point, &result);
                assert!(store.supervised_collector_lease_status().expect("lease").is_some());
            }
            "cadence-cursor-key" => {
                refused_with_the_sensitive_failure(point, &result);
                // The key file is a durable write of its own: it was not created.
                assert!(
                    !crate::teslamate_credentials::cursor_key_path(temporary.path()).exists(),
                    "{point}: the cursor key was created after the refusal"
                );
            }
            _ => refused_with_the_sensitive_failure(point, &result),
        }
    });
}

#[test]
fn p3bf2_the_configuration_wait_writes_nothing_after_the_fence() {
    for point in [
        "cadence-lease",
        "cadence-start",
        "cadence-start-release",
        "cadence-cursor-key",
        "cadence-release",
    ] {
        p3bf2_cadence_wait(point);
    }
}

// ---- the terrain worker's pass --------------------------------------------------------------------------------

fn p3bf2_terrain_position(id: i64) -> crate::hub_pack::ProjectionPosition {
    crate::hub_pack::ProjectionPosition {
        id,
        drive_id: Some(7),
        car_id: 7,
        date_ms: id * 1_000,
        latitude: 51.0,
        longitude: -0.1,
        speed: Some(20),
        power: None,
        battery_level: Some(80),
        usable_battery_level: None,
        elevation: None,
        odometer: None,
        ideal_battery_range_km: None,
        est_battery_range_km: None,
        rated_battery_range_km: None,
        fan_status: None,
        driver_temp_setting: None,
        passenger_temp_setting: None,
        is_climate_on: None,
        is_rear_defroster_on: None,
        is_front_defroster_on: None,
        inside_temp: None,
        outside_temp: None,
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        tpms_pressure_fl: None,
        tpms_pressure_fr: None,
        tpms_pressure_rl: None,
        tpms_pressure_rr: None,
    }
}

/// A store whose car 7 has one closed drive with three positions that need an
/// elevation.
fn p3bf2_terrain_candidates(store: &HubStore) {
    let source = store
        .register_source(&SourceDescriptor::new("terrain_test", "one"), 1_000)
        .expect("source");
    let mut descriptor = VehicleDescriptor::new(source.source_id, "7");
    descriptor.display_name = Some("Terrain car".into());
    let vehicle = store.register_vehicle(&descriptor, 1_000).expect("vehicle");
    let drive = crate::hub_pack::ProjectionDrive {
        id: 7,
        car_id: 7,
        optimized_at_ms: None,
        start_date_ms: 1_000,
        end_date_ms: 3_000,
        distance_km: Some(1.0),
        duration_min: Some(1),
        efficiency: None,
        outside_temp_avg: None,
        inside_temp_avg: None,
        speed_max: Some(20),
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_address: None,
        end_address: None,
        start_geofence: None,
        end_geofence: None,
        start_latitude: Some(51.0),
        start_longitude: Some(-0.1),
        end_latitude: Some(51.0),
        end_longitude: Some(-0.1),
        start_soc: Some(80),
        end_soc: Some(79),
        start_rated_range_km: None,
        end_rated_range_km: None,
        ascent: None,
        descent: None,
    };
    let mut open = OpenSessionState::new();
    open.car_metadata = Some(crate::hub_pack::ProjectionCarPatch {
        name: Some("Terrain car".into()),
        model: Some("3".into()),
        ..Default::default()
    });
    let encoded = open.encode().expect("open state");
    store
        .commit_lifecycle_delta(&crate::db::LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 7,
            open_session_json: &encoded,
            last_observation_id: 3,
            quarantined: false,
            updated_at_ms: 3_000,
            delta: &crate::lifecycle::LifecycleDelta {
                drives: vec![drive],
                positions: (1..=3).map(p3bf2_terrain_position).collect(),
                ..Default::default()
            },
        })
        .expect("the drive and its positions");
}

/// A store whose car 7 has one closed drive with three positions that need an
/// elevation, and a terrain cache whose provider is a loopback double: `tiles` says
/// whether the double serves the tile (a zip for every request that is not AWS's) or
/// answers every request 404.
fn p3bf2_terrain_rig(
    tiles: bool,
    runtime: &tokio::runtime::Runtime,
) -> (
    tempfile::TempDir,
    GuardBed,
    TerrainCache,
    tokio::task::JoinHandle<()>,
) {
    let (temporary, bed) = GuardBed::new();
    p3bf2_terrain_candidates(&bed.store);
    let zip = {
        use std::io::Write as _;
        let mut output = std::io::Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(&mut output);
        archive
            .start_file("N51W001.hgt", zip::write::SimpleFileOptions::default())
            .expect("zip entry");
        archive
            .write_all(&vec![0_u8; crate::terrain::SRTM3_BYTES as usize])
            .expect("zip body");
        archive.finish().expect("zip");
        output.into_inner()
    };
    let (endpoint, server) = runtime.block_on(async move {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let endpoint = format!("http://{}/", listener.local_addr().expect("address"));
        let app = Router::new().route(
            "/{*path}",
            axum::routing::any(move |uri: axum::http::Uri| {
                let zip = zip.clone();
                async move {
                    if tiles && !uri.path().ends_with(".gz") {
                        axum::response::Response::new(axum::body::Body::from(zip))
                    } else {
                        axum::response::Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(axum::body::Body::empty())
                            .expect("a 404")
                    }
                }
            }),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (endpoint, server)
    });
    let mut options = crate::terrain_cache::TerrainCacheOptions {
        root: temporary.path().join("terrain-cache"),
        store: bed.store.clone(),
        min_free_bytes: 1,
        max_cache_bytes: 512 * 1024 * 1024,
        connect_timeout: P3BF2_TERRAIN_LOOKUP_BUDGET,
        read_timeout: P3BF2_TERRAIN_LOOKUP_BUDGET,
        aws_base: endpoint.clone(),
        esa_base: endpoint,
        free_space_override: None,
    };
    options.free_space_override = None;
    let cache = {
        let _entered = runtime.enter();
        TerrainCache::new(options).expect("terrain cache")
    };
    (temporary, bed, cache, server)
}

/// The pass of these proofs downloads a tile from a loopback double and must reach the
/// armed boundary whatever the machine's load: the production budget of a lookup (100 ms)
/// is a real-time bound that a loaded debug build exceeds, which ended the pass with
/// recorded failures and never reached the boundary (B2 fix round, P3b fix 4). The proof
/// is of the fence, not of the budget, so the pass is given a budget it cannot exceed.
const P3BF2_TERRAIN_LOOKUP_BUDGET: Duration = Duration::from_secs(300);

fn p3bf2_terrain_pass(point: &'static str) {
    let tiles = point != "terrain-failure";
    // The pass runs on a runtime whose server task outlives the rig's setup.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let (_temporary, bed, cache, server) = p3bf2_terrain_rig(tiles, &runtime);
    bed.trip_at(point);
    let mut fuse = TerrainFuse::default();
    let result = runtime.block_on(run_terrain_enrichment_pass(
        &bed.store,
        &cache,
        &CursorKey::from_bytes([4; 32]),
        1,
        &mut fuse,
        &bed.guard,
        P3BF2_TERRAIN_LOOKUP_BUDGET,
    ));
    refused_with_the_sensitive_failure(point, &result);
    bed.assert_held(point);
    server.abort();
}

#[test]
fn p3bf2_the_terrain_pass_writes_nothing_after_the_fence() {
    for point in [
        "terrain-lookup",
        "terrain-result",
        "terrain-failure",
        "terrain-publish",
        "terrain-publish-sequence",
        "terrain-publish-pack",
        "terrain-publish-finalize",
    ] {
        p3bf2_terrain_pass(point);
    }
}

// ---- the stream frames of the running loop ---------------------------------------------------------------

/// The Owner loop with a live stream producer: the fence is set (the admission untouched
/// and valid) the third time a frame is about to be written, and the whole store is read
/// at that moment. `held_list` holds a scheduled list first, so the frames come through
/// the drains the loop pumps while it waits; without it they come through the loop's
/// ordinary drains (Opus F2). The run must end with the sensitive failure, the producer
/// never starved before the fence, and not a row be written after it.
async fn p3bf2_loop_frames(held_list: bool) {
    let stats = Arc::new(ProducerStats::default());
    // The proof is of the frames: no unit admitted before the fence may still be in
    // flight when the store is stamped at the trip (B2 fix round, P3b fix 5, R-P3bF5-4).
    // The only one that runs beside the loop is the export replay's blocking job, at the
    // tail of an iteration. Without a held list the replay is held from the start, before
    // it takes the publication gate or starts the job (a test that held the gate itself
    // stalled the tick that needs it): the loop pumps the frames while it waits, and the
    // hold is let go once the stamp is taken, when the replay is refused at its first
    // boundary. With a held list the loop is parked in its second iteration's list, so
    // the first iteration's replay is over, and the next is a minute away. Nothing is
    // retried or widened.
    let mut bed = DrainBed::start_with_admission(1, true, |_, seam| {
        seam.inject_stream_runs(telemetry_runs(&stats, Duration::from_millis(50)));
        seam.replay_hold_before_worker
            .store(!held_list, Ordering::SeqCst);
    })
    .await;
    let seam = Arc::clone(&bed.seam);
    bed.wait_for("the stream run", || seam.stream_admissions() >= 1)
        .await;
    if held_list {
        hold_the_next_list(&mut bed).await;
    }
    let audit = bed.seam.write_audit();
    let at_trip: Arc<Mutex<Option<String>>> = Arc::default();
    let (slot, observed) = (Arc::clone(&at_trip), bed.store.clone());
    audit.trip_at_nth("stream-frame-intake", 3, move || {
        *slot.lock().expect("stamp") = Some(whole_store_stamp(&observed));
    });
    let watched = Arc::clone(&bed.seam.write_audit());
    bed.wait_for("the third frame reaches the guard", || watched.tripped())
        .await;
    // Whatever the loop does next, it ends without a write. The replay, if it was
    // reached, is let go now and meets the fence.
    seam.replay_hold_before_worker.store(false, Ordering::SeqCst);
    bed.wire.list_hold.store(false, Ordering::SeqCst);
    let result = bed.stop().await;
    refused_with_the_sensitive_failure("stream-frame-intake", &result);
    let at_trip = at_trip.lock().expect("stamp").clone();
    assert!(at_trip.is_some(), "the store was read at the trip");
    assert!(audit.refused("stream-frame-intake") >= 1);
    let after = whole_store_stamp(&bed.store);
    assert!(
        Some(&after) == at_trip.as_ref(),
        "a row was written after the refused frame (held list: {held_list}); the lines that \
         differ, at the trip then at the end: {:?}",
        stamp_difference(at_trip.as_deref().unwrap_or_default(), &after)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p3bf2_the_running_owner_loop_writes_no_frame_after_the_fence() {
    p3bf2_loop_frames(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p3bf2_the_running_owner_loop_writes_no_frame_after_the_fence_while_a_list_is_held() {
    p3bf2_loop_frames(true).await;
}

// ---- the Fleet Telemetry maintenance loop's iteration ---------------------------------------------------

#[test]
fn p3bf2_the_telemetry_maintenance_loop_ends_at_the_fence_though_the_admission_holds() {
    // The loop used to look at the admission only: a fence the supervisor set with the
    // admission intact did not end it. Now its first look is the guard's.
    crate::crypto::install_default_provider();
    guard_block_on(async {
        let (temporary, store, cursor_key, _generation) = p3_unresolved_fleet_hub();
        let config = supervised_restart_test_config(temporary.path());
        let bed = GuardBed::over(&store, temporary.path());
        let manager = FleetAuthManager::from_store_for_supervised_start(
            store.clone(),
            temporary.path(),
            Arc::clone(&bed.admission),
            None,
        )
        .expect("the supervised start admits it");
        let manager = Arc::new(tokio::sync::Mutex::new(manager));
        let auth_api = FleetAuthApi::for_fake_http(
            url::Url::parse("http://127.0.0.1:9/oauth2/v3/token").expect("token URL"),
            Duration::from_secs(1),
        )
        .expect("auth client");
        let (terrain_wake, _terrain_rx) = mpsc::channel(8);
        let ledger = Core::fleet().scheduler.request_ledger();
        let before = whole_store_stamp(&store);
        bed.guard.fence().set();
        assert!(assert_runtime_sensitive_access(Some(&*bed.admission)).is_ok());
        let result = run_fleet_telemetry_maintenance_loop(
            &store,
            &config,
            &auth_api,
            &manager,
            &cursor_key,
            &terrain_wake,
            &bed.admission,
            false,
            &ledger,
            &bed.guard,
        )
        .await;
        refused_with_the_sensitive_failure("telemetry-iteration", &result);
        assert_eq!(bed.guard.audit().refused("telemetry-iteration"), 1);
        assert_eq!(bed.guard.audit().total_passed(), 0, "no boundary let anything through");
        assert_eq!(whole_store_stamp(&store), before);
    });
}

// ---- the stream run's audit, as the loop wires it ------------------------------------------------------------

/// How many audit rows the store holds: the session rows and the request rows.
fn p3bf2_audit_rows(store: &HubStore) -> i64 {
    let connection = store.open().expect("connection");
    let count = |table: &str| -> i64 {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .expect("count")
    };
    count("stream_session_receipts") + count("outbound_request_receipts")
}

/// The loop's `ensure_vehicle_stream` starts a real stream supervisor against an endpoint
/// nothing listens on. With the run's guard handed over, the audit's first mutation is
/// the guard's to refuse: with the fence set (the admission intact) the run ends with the
/// loss of the authority and not one audit row exists; without the fence the same run
/// writes its session row (the control).
fn p3bf2_stream_run(fenced: bool) -> (i64, Option<Result<(), crate::tesla_stream::StreamSupervisorError>>) {
    crate::crypto::install_default_provider();
    guard_block_on(async move {
        let (_temporary, bed) = GuardBed::new();
        let auth_value = crate::legacy_auth::LegacyAuth::for_test(
            url::Url::parse("https://auth.tesla.com/oauth2/v3/token").unwrap(),
            "access-secret",
            "refresh-secret",
        );
        let manager = LegacyAuthManager::for_test(auth_value, Arc::new(|_, _| Ok(())));
        let auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region: StreamRegion::Global,
        };
        let client = crate::owner_api::OwnerApi::for_fake_http(
            url::Url::parse("http://127.0.0.1:9/").unwrap(),
            Duration::from_secs(1),
        )
        .expect("client");
        if fenced {
            bed.guard.fence().set();
        }
        let mut streams = Vec::new();
        assert!(ensure_vehicle_stream(
            &bed.store,
            &mut streams,
            core_id(1),
            StreamVehicleId::from_test(1),
            &auth,
            &client,
            Duration::from_secs(60),
            Some("ws://127.0.0.1:9/"),
            Some(&bed.guard),
        ));
        let mut task = streams[0].task.take().expect("the run's task");
        if fenced {
            let result = timeout(Duration::from_secs(10), &mut task)
                .await
                .expect("the run ends by itself")
                .expect("the task joins");
            return (p3bf2_audit_rows(&bed.store), Some(result));
        }
        // The control: the run reaches its first audit row, then is told to stop.
        for _ in 0..500 {
            if p3bf2_audit_rows(&bed.store) > 0 {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        let rows = p3bf2_audit_rows(&bed.store);
        if let Some(stop) = streams[0]._shutdown.take() {
            let _ = stop.send(());
        }
        let _ = timeout(Duration::from_secs(10), &mut task).await;
        task.abort();
        (rows, None)
    })
}

#[test]
fn p3bf2_a_stream_run_started_by_the_loop_writes_no_audit_row_after_the_fence() {
    let (control, _) = p3bf2_stream_run(false);
    assert!(control > 0, "the unfenced run writes its audit row");
    let (rows, result) = p3bf2_stream_run(true);
    assert_eq!(rows, 0, "no audit row after the fence");
    assert!(
        matches!(
            result,
            Some(Err(
                crate::tesla_stream::StreamSupervisorError::CredentialAuthorityUnavailable
            ))
        ),
        "{result:?}"
    );
}

// ---- the sensitive outcome the credential unit's service met (R-P3bF2-2, loop side) ---------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p3bf2_the_maintenance_raises_the_sensitive_outcome_the_units_service_met() {
    // A detached exchange ends with an unknown answer while its admission check is
    // denied (once; the admission is back at every other check). The first thing to
    // apply the result is the loop's own maintenance at the top of an iteration, not the
    // shutdown: the outcome is latched, nothing is written for it, and the maintenance
    // raises the sensitive failure that ends the run. This test calls the maintenance
    // directly; the supervisor's own ending with that outcome is
    // `p3bf3_the_supervised_run_ends_with_the_sensitive_outcome_the_unit_met_between_requests`
    // (B2 fix round, P3b fix 3, Opus N4).
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_the_late_import(&store);
    crate::teslamate_credentials::replace_key(temporary.path(), b"guard-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial =
        OwnerTokens::from_secret_parts("guard-access".to_owned(), "guard-refresh".to_owned())
            .expect("initial pair");
    let (access, refresh_bytes) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("Cloak pair");
    let now_seconds = current_epoch_millis().expect("clock") / 1_000;
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(
                access,
                refresh_bytes,
                now_seconds + 36_000,
                now_seconds - 100,
            )
            .expect("a due refresh schedule"),
        )
        .expect("store the pair");
    let wire = LateWire {
        posts: Arc::new(AtomicUsize::new(0)),
        arrived: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        answer: LateAnswer::Reset,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new()
        .route("/oauth2/v3/token", post(late_token))
        .with_state(wire.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("token endpoint");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("base");
    let mut manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        store.clone(),
        temporary.path(),
        base.join("oauth2/v3/").expect("issuer"),
    )
    .expect("load pair");
    let denial = manager.deny_sensitive_access_once_for_test();
    let region = manager.region();
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region,
    };
    let client = crate::owner_api::OwnerApi::for_fake_http(base, Duration::from_secs(2))
        .expect("client");
    let CollectionAuth::Legacy { manager, refresh, .. } = &auth;
    let caller = {
        let (manager, http) = (Arc::clone(manager), client.legacy_auth_http_client());
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&http, SystemTime::now())
                .await
        })
    };
    timeout(SUPERVISED_RESTART_WAIT, wire.arrived.notified())
        .await
        .expect("the exchange reaches the token endpoint");
    caller.abort();
    let _ = caller.await;
    let before = auth_and_receipt_stamp(&store);
    let attempts_before = manager.lock().await.status_write_attempts().len();
    denial.store(true, Ordering::SeqCst);
    wire.release.notify_one();
    // The exchange meets the denial once (the switch is taken), then ends.
    for _ in 0..500 {
        if !denial.load(Ordering::SeqCst) {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert!(!denial.load(Ordering::SeqCst), "the exchange met the denial");
    for _ in 0..500 {
        maintain_legacy_credential(&client, &auth).await;
        if refresh.has_sensitive_failure() {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert!(
        refresh.has_sensitive_failure(),
        "the maintenance raised the sensitive failure that ends the run"
    );
    assert_eq!(auth_and_receipt_stamp(&store), before, "nothing was written for it");
    assert_eq!(
        manager.lock().await.status_write_attempts().len(),
        attempts_before,
        "no status write was attempted"
    );
    assert_eq!(wire.posts.load(Ordering::SeqCst), 1, "no second request");
    server.abort();
}

// ---- the resident control socket, as the supervisors wire it ------------------------------------------------

fn p3bf2_receipt_rows(store: &HubStore) -> i64 {
    store
        .open()
        .expect("catalogue")
        .query_row("SELECT COUNT(*) FROM outbound_request_receipts", [], |row| row.get(0))
        .expect("receipts")
}

/// A resident wake-up asked of the running Owner loop through its control socket, while the
/// loop is parked on a held list. With `fenced` the run's fence is set at the very moment
/// the action reaches its receipt (the guard the supervisor handed to the socket is
/// consulted there; the credential unit has passed by then). Returns the audit receipts
/// the action left, its answer and whether the guard was consulted at the receipt.
async fn p3bf2_owner_resident(
    fenced: bool,
) -> (
    i64,
    Result<LegacyVehicleActionReport, ResidentVehicleActionError>,
    bool,
) {
    let mut bed = DrainBed::start_with_admission(1, true, |_, _| {}).await;
    hold_the_next_list(&mut bed).await;
    let hub = bed.store.configured_tesla_vehicles().expect("cars")[0].0;
    let audit = bed.seam.write_audit();
    if fenced {
        audit.trip_at_nth("resident-begin", 1, || {});
    }
    let answer =
        request_resident_vehicle_action(bed.temporary.path(), hub, LegacyVehicleAction::Wake).await;
    let receipts = p3bf2_receipt_rows(&bed.store);
    let tripped = audit.tripped();
    bed.wire.list_hold.store(false, Ordering::SeqCst);
    let _ = bed.stop().await;
    (receipts, answer, tripped)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p3bf2_the_owner_loop_hands_its_guard_to_the_resident_socket() {
    let (control, _, _) = p3bf2_owner_resident(false).await;
    assert!(control > 0, "unfenced, the action writes its audit receipt");
    let (receipts, answer, tripped) = p3bf2_owner_resident(true).await;
    assert!(tripped, "the socket consults the run's guard at the receipt");
    assert_eq!(receipts, 0, "no audit receipt after the fence: {answer:?}");
    assert!(answer.is_err(), "the action is refused: {answer:?}");
}

async fn p3bf2_fleet_resident(
    fenced: bool,
) -> (
    i64,
    Result<LegacyVehicleActionReport, ResidentVehicleActionError>,
    bool,
) {
    let mut gate = None;
    let mut bed =
        FleetLoopBed::start_with_seam("asleep", false, true, Duration::from_secs(2), |seam| {
            gate = Some(seam.arm_gate("before-enrichment"));
        })
        .await;
    let gate = gate.expect("the gate");
    gate.arrived("the loop parks before its enrichment", 1).await;
    let hub = bed.store.configured_tesla_vehicles().expect("cars")[0].0;
    let audit = bed.seam.write_audit();
    if fenced {
        audit.trip_at_nth("resident-begin", 1, || {});
    }
    let answer =
        request_resident_vehicle_action(bed.temporary.path(), hub, LegacyVehicleAction::Wake).await;
    let receipts = p3bf2_receipt_rows(&bed.store);
    let tripped = audit.tripped();
    gate.open();
    let mut task = bed.task.take().expect("task");
    if let Some(shutdown) = bed.shutdown.take() {
        let _ = shutdown.send(());
    }
    let _ = join_supervised_restart_task("the Fleet loop ends", &mut task).await;
    bed.server.abort();
    (receipts, answer, tripped)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p3bf2_the_fleet_loop_hands_its_guard_to_the_resident_socket() {
    let (control, control_answer, _) = p3bf2_fleet_resident(false).await;
    assert!(control > 0, "unfenced, the action writes its audit receipt: {control_answer:?}");
    let (receipts, answer, tripped) = p3bf2_fleet_resident(true).await;
    assert!(tripped, "the socket consults the run's guard at the receipt");
    assert_eq!(receipts, 0, "no audit receipt after the fence: {answer:?}");
    assert!(answer.is_err(), "the action is refused: {answer:?}");
}
