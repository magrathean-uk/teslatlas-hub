// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

//! Journey 2, delivery and restart (M04-T02): interrupt before commit, interrupt
//! after commit and before acknowledgement, collector and Hub restart, network
//! loss, and a storage limit reached with an honest failure.
//!
//! The tests drive the Hub's durable boundary through its public API: the one
//! transaction the collector's tick opens for an observation and its lifecycle
//! (`accept_owner_observation_and_lifecycle`). The collector's HTTP side cannot be
//! reached from an integration test (the fake Tesla source and the
//! `VehicleData` constructors are crate-private), so a "delivery" here is one
//! observation handed to that boundary, and the "acknowledgement" is the
//! collector's own record that the hand-over finished.
//!
//! Interrupts are real: a child process (this test binary, re-run in a role
//! selected by `DELIVERY_RESTART_ROLE`) is killed with SIGKILL, so no destructor,
//! rollback or flush of the child runs. Data is synthetic.

use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

use rusqlite::params;
use serde_json::{Value, json};
use teslatlas_hub::db::{
    HubStore, ObservationInput, SourceDescriptor, SourceRecord, StoreError, VehicleDescriptor,
    VehicleRecord,
};

const T0: i64 = 1_760_000_000_000;
const MIN: i64 = 60_000;
const CAR_ID: i64 = 1;
const ROLE_ENV: &str = "DELIVERY_RESTART_ROLE";
const DIR_ENV: &str = "DELIVERY_RESTART_DIR";
const COUNT_ENV: &str = "DELIVERY_RESTART_COUNT";
const LIMIT_ENV: &str = "DELIVERY_RESTART_LIMIT_BYTES";
/// A child that was not killed by the parent gives up after this long.
const CHILD_STALL: Duration = Duration::from_secs(120);

// ------------------------------------------------------------------ fixtures

/// A synthetic owner `vehicle_data` snapshot: gear and speed as given (`null`
/// means the key is present with JSON null, which is Park), plus an odometer.
fn snapshot(t: i64, gear: Value, speed: Value, latitude: f64, odometer_mi: f64) -> Value {
    json!({
        "record_type": "owner_api_vehicle_data_v1",
        "source_vehicle_id": "9",
        "source_vehicle_state": "online",
        "provider_raw_json": {"response": {
            "drive_state": {"timestamp": t, "shift_state": gear, "speed": speed,
                            "latitude": latitude, "longitude": 20.0},
            "vehicle_state": {"timestamp": t, "odometer": odometer_mi},
        }},
    })
}

/// A drive of four moving snapshots one minute apart, then one parked snapshot:
/// the parked one closes the drive. Returned as (observed time, payload).
fn drive_sequence() -> Vec<(i64, Value)> {
    let mut sequence = (0..4)
        .map(|k| {
            let t = T0 + k * MIN;
            (
                t,
                snapshot(
                    t,
                    json!("D"),
                    json!(30),
                    10.0 + 0.001 * k as f64,
                    1000.0 + 0.5 * k as f64,
                ),
            )
        })
        .collect::<Vec<_>>();
    let t = T0 + 4 * MIN;
    sequence.push((t, snapshot(t, Value::Null, Value::Null, 10.004, 1002.0)));
    sequence
}

fn private_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("dr-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("private synthetic root")
}

struct Identity {
    source: SourceRecord,
    vehicle: VehicleRecord,
}

/// Register (or find again) the collector's source and car. Registration is
/// idempotent: a restarted collector calls this and must get the same identity.
fn identify(store: &HubStore) -> Identity {
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            T0,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "9"), T0)
        .expect("vehicle");
    Identity { source, vehicle }
}

fn input(identity: &Identity, observed_at_ms: i64, payload: &Value) -> ObservationInput {
    ObservationInput {
        source_id: identity.source.source_id,
        vehicle_id: identity.vehicle.vehicle_id,
        observed_at_ms,
        payload: payload.clone(),
    }
}

/// One delivery: the collector's per-tick durable transaction.
fn deliver(
    store: &HubStore,
    identity: &Identity,
    (observed_at_ms, payload): &(i64, Value),
) -> Result<teslatlas_hub::db::OwnerObservationResult, StoreError> {
    store.accept_owner_observation_and_lifecycle(
        &input(identity, *observed_at_ms, payload),
        *observed_at_ms,
        CAR_ID,
    )
}

/// The processed raw rows are pruned once the lifecycle has consumed them, so
/// what proves a delivery is durable is the current observation of each record
/// type plus the materialised history. Debug text makes the comparison exact.
fn durable_state(store: &HubStore, identity: &Identity) -> String {
    let current = store
        .current_observations_for_vehicle(identity.vehicle.vehicle_id)
        .expect("current observations");
    let history = store
        .materialised_history(identity.vehicle.vehicle_id)
        .expect("history");
    format!(
        "{current:?}|{:?}|{:?}",
        history.drives, history.positions
    )
}

fn latest_observed_at(store: &HubStore, identity: &Identity) -> Option<i64> {
    store
        .current_observations_for_vehicle(identity.vehicle.vehicle_id)
        .expect("current observations")
        .iter()
        .map(|record| record.observed_at_ms)
        .max()
}

fn raw_count(store: &HubStore, identity: &Identity) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM raw_observations WHERE vehicle_id = ?1",
            params![identity.vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("raw count")
}

/// Hub dir inside a private root.
fn hub_dir(root: &Path) -> PathBuf {
    root.join("hub")
}

/// What an uninterrupted store holds after the whole drive, for comparison.
fn reference_history(sequence: &[(i64, Value)]) -> teslatlas_hub::db::MaterialisedHistory {
    let root = private_root();
    let store = HubStore::initialize(hub_dir(root.path())).expect("reference store");
    let identity = identify(&store);
    for item in sequence {
        deliver(&store, &identity, item).expect("reference delivery");
    }
    let history = store
        .materialised_history(identity.vehicle.vehicle_id)
        .expect("reference history");
    assert_eq!(history.drives.len(), 1, "the reference drive closes once");
    assert_eq!(history.positions.len(), sequence.len());
    history
}

// ------------------------------------------------------------ child plumbing

fn child_command(role: &str, dir: &Path) -> Command {
    let exe = env::current_exe().expect("test binary");
    let mut command = Command::new(exe);
    command
        .args([
            "--exact",
            "child_role_entry",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE_ENV, role)
        .env(DIR_ENV, dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Read the child's output until a line `MARK <name>` arrives. The child never
/// proceeds past a mark on its own account: it then waits on stdin, which the
/// parent never writes, so the parent's kill is what ends it.
fn wait_for_mark(child: &mut Child, name: &str) -> String {
    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let wanted = format!("MARK {name}");
    while let Some(line) = lines.next() {
        let line = line.expect("child line");
        if let Some(rest) = line.strip_prefix(&wanted) {
            // Keep draining so the child never blocks on a full pipe.
            thread::spawn(move || for _ in lines {});
            return rest.trim().to_owned();
        }
    }
    panic!("child ended before it reached MARK {name}");
}

/// SIGKILL: the child gets no chance to commit, roll back, checkpoint or close.
fn crash(mut child: Child) {
    child.kill().expect("kill the child");
    let status = child.wait().expect("reap the child");
    assert!(!status.success(), "the child was killed, not finished");
}

fn mark(text: &str) {
    println!("\nMARK {text}");
    std::io::stdout().flush().expect("flush mark");
}

/// Block until the parent acts. It never writes, so only a kill ends this.
fn stall_for_ever() -> ! {
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    thread::sleep(CHILD_STALL);
    panic!("the parent should have killed this child");
}

/// Entry point of the child processes. Without `DELIVERY_RESTART_ROLE` (the
/// ordinary run) it does nothing.
#[test]
fn child_role_entry() {
    let Ok(role) = env::var(ROLE_ENV) else {
        return;
    };
    // libtest leaves "test name ... " unterminated on this line.
    println!();
    let dir = PathBuf::from(env::var(DIR_ENV).expect("child dir"));
    match role.as_str() {
        "uncommitted" => child_uncommitted(&dir),
        "deliver_then_stall" => child_deliver_then_stall(&dir),
        "fill_until_limit" => child_fill_until_limit(&dir),
        other => panic!("unknown role {other}"),
    }
}

/// Write a large uncommitted transaction (large enough that SQLite spills pages
/// to the write-ahead log before any commit), announce it and wait to be killed.
fn child_uncommitted(dir: &Path) {
    let store = HubStore::initialize(hub_dir(dir)).expect("child store");
    let identity = identify(&store);
    let connection = store.open().expect("child connection");
    connection
        .execute_batch("PRAGMA cache_size = 16; BEGIN IMMEDIATE")
        .expect("begin");
    let filler = "x".repeat(4096);
    for k in 0..800_i64 {
        let payload = json!({"filler": filler, "k": k}).to_string();
        let mut digest = [0_u8; 32];
        digest[..8].copy_from_slice(&k.to_be_bytes());
        connection
            .execute(
                "INSERT INTO raw_observations
                 (observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms,
                  payload_sha256, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
                params![
                    1_000 + k,
                    identity.source.source_id.to_string(),
                    identity.vehicle.vehicle_id.to_string(),
                    T0 + k,
                    digest.to_vec(),
                    payload,
                ],
            )
            .expect("uncommitted insert");
    }
    mark("uncommitted");
    stall_for_ever();
}

/// Commit the first COUNT snapshots of the drive, each in its own transaction,
/// announce the last commit, and stop before acknowledging it.
fn child_deliver_then_stall(dir: &Path) {
    let count: usize = env::var(COUNT_ENV)
        .expect("count")
        .parse()
        .expect("count digits");
    let store = HubStore::initialize(hub_dir(dir)).expect("child store");
    let identity = identify(&store);
    for item in drive_sequence().iter().take(count) {
        deliver(&store, &identity, item).expect("child delivery");
    }
    mark(&format!("committed {count}"));
    stall_for_ever();
}

/// Append large observations, acknowledging each commit on stdout, under a hard
/// file-size limit, until the Hub reports a failure; then report it.
fn child_fill_until_limit(dir: &Path) {
    let store = HubStore::initialize(hub_dir(dir)).expect("child store");
    let identity = identify(&store);
    let database = hub_dir(dir).join("hub.sqlite");
    let limit = fs::metadata(&database).expect("database size").len()
        + env::var(LIMIT_ENV)
            .expect("limit headroom")
            .parse::<u64>()
            .expect("limit digits");
    rustix::process::setrlimit(
        rustix::process::Resource::Fsize,
        rustix::process::Rlimit {
            current: Some(limit),
            maximum: Some(limit),
        },
    )
    .expect("file size limit");
    let filler = "y".repeat(8 * 1024);
    for k in 0..20_000_i64 {
        let observed = T0 + k;
        let payload = json!({"filler": filler, "k": k});
        match store.append_observation(&input(&identity, observed, &payload), observed) {
            Ok(appended) => {
                assert!(appended.inserted);
                println!("ACK {k}");
            }
            Err(error) => {
                println!("FAILED {k} {error}");
                mark("limit");
                return;
            }
        }
    }
    mark("never-limited");
}

fn payload_for_fill(k: i64) -> Value {
    json!({"filler": "y".repeat(8 * 1024), "k": k})
}

// ------------------------------------------------------------------- journey

#[test]
fn interrupt_before_commit_loses_nothing_committed_and_the_retry_gives_one_result() {
    let root = private_root();
    // One committed observation exists before the interrupted write.
    let store = HubStore::initialize(hub_dir(root.path())).expect("store");
    let identity = identify(&store);
    let sequence = drive_sequence();
    deliver(&store, &identity, &sequence[0]).expect("first delivery");
    drop(store);

    let mut child = child_command("uncommitted", root.path())
        .spawn()
        .expect("spawn child");
    wait_for_mark(&mut child, "uncommitted");
    crash(child);

    // The Hub restarts on the crashed files: the uncommitted rows are gone and
    // the committed delivery is untouched.
    let store = HubStore::initialize(hub_dir(root.path())).expect("restart after crash");
    let identity = identify(&store);
    let leftover: i64 = store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM raw_observations WHERE observation_id >= 1000",
            [],
            |row| row.get(0),
        )
        .expect("leftover rows");
    assert_eq!(leftover, 0, "nothing of the interrupted write survives");
    assert_eq!(latest_observed_at(&store, &identity), Some(sequence[0].0));

    // The collector never got its acknowledgement for the interrupted work, so
    // it delivers the rest again from where it knows it was acknowledged.
    for item in &sequence[1..] {
        let result = deliver(&store, &identity, item).expect("retry");
        assert!(result.append.inserted, "{}: first durable result", item.0);
    }
    let history = store
        .materialised_history(identity.vehicle.vehicle_id)
        .expect("history");
    let reference = reference_history(&sequence);
    assert_eq!(history.drives, reference.drives);
    assert_eq!(history.positions, reference.positions);
}

#[test]
fn interrupt_after_commit_before_acknowledgement_gives_one_logical_result_on_retry() {
    let root = private_root();
    let sequence = drive_sequence();
    let mut child = child_command("deliver_then_stall", root.path())
        .env(COUNT_ENV, sequence.len().to_string())
        .spawn()
        .expect("spawn child");
    wait_for_mark(&mut child, &format!("committed {}", sequence.len()));
    // The whole drive is committed; the child dies before it can acknowledge.
    crash(child);

    let store = HubStore::initialize(hub_dir(root.path())).expect("restart after crash");
    let identity = identify(&store);
    let before = durable_state(&store, &identity);
    let history_before = store
        .materialised_history(identity.vehicle.vehicle_id)
        .expect("history before retry");
    assert_eq!(
        latest_observed_at(&store, &identity),
        Some(sequence[4].0),
        "the last commit is durable"
    );
    assert_eq!(history_before.drives.len(), 1);
    assert_eq!(history_before.positions.len(), sequence.len());

    // The collector retries every unacknowledged delivery.
    for item in &sequence {
        let result = deliver(&store, &identity, item).expect("retry after the kill");
        assert!(!result.append.inserted, "{}: already durable", item.0);
        assert_eq!(result.drives_closed, 0, "{}: no second drive", item.0);
        assert_eq!(result.positions_materialised, 0, "{}: no new rows", item.0);
    }
    assert_eq!(
        durable_state(&store, &identity),
        before,
        "the retry changes nothing durable"
    );
    let history_after = store
        .materialised_history(identity.vehicle.vehicle_id)
        .expect("history after retry");
    assert_eq!(history_after.drives, history_before.drives);
    assert_eq!(history_after.positions, history_before.positions);
    assert_eq!(history_after.drives, reference_history(&sequence).drives);
}

#[test]
fn a_retry_in_the_same_process_is_one_logical_result_too() {
    let root = private_root();
    let store = HubStore::initialize(hub_dir(root.path())).expect("store");
    let identity = identify(&store);
    let sequence = drive_sequence();
    for item in &sequence {
        let first = deliver(&store, &identity, item).expect("first");
        let again = deliver(&store, &identity, item).expect("retry");
        assert!(first.append.inserted);
        assert!(!again.append.inserted);
        assert_eq!(
            first.append.observation, again.append.observation,
            "{}: the retry returns the original row",
            item.0
        );
    }
    let history = store
        .materialised_history(identity.vehicle.vehicle_id)
        .expect("history");
    assert_eq!(history.drives.len(), 1);
    assert_eq!(history.positions.len(), sequence.len());
}

#[test]
fn collector_restart_mid_drive_with_an_overlapping_replay_matches_an_uninterrupted_run() {
    let root = private_root();
    let sequence = drive_sequence();
    let store = HubStore::initialize(hub_dir(root.path())).expect("store");
    let identity = identify(&store);
    for item in &sequence[..3] {
        deliver(&store, &identity, item).expect("before the restart");
    }
    drop(store);

    // The collector restarts: it registers again (same identities) and replays
    // its last two unacknowledged deliveries before carrying on.
    let store = HubStore::initialize(hub_dir(root.path())).expect("restart");
    let again = identify(&store);
    assert_eq!(again.source.source_id, identity.source.source_id);
    assert_eq!(again.vehicle.vehicle_id, identity.vehicle.vehicle_id);
    for item in &sequence[1..3] {
        assert!(
            !deliver(&store, &again, item)
                .expect("replay")
                .append
                .inserted
        );
    }
    for item in &sequence[3..] {
        assert!(
            deliver(&store, &again, item)
                .expect("after the restart")
                .append
                .inserted
        );
    }
    let history = store
        .materialised_history(again.vehicle.vehicle_id)
        .expect("history");
    let reference = reference_history(&sequence);
    assert_eq!(history.drives, reference.drives);
    assert_eq!(history.positions, reference.positions);
}

#[test]
fn hub_killed_mid_drive_resumes_from_its_durable_state() {
    let root = private_root();
    let sequence = drive_sequence();
    let mut child = child_command("deliver_then_stall", root.path())
        .env(COUNT_ENV, "3")
        .spawn()
        .expect("spawn child");
    wait_for_mark(&mut child, "committed 3");
    crash(child);

    let store = HubStore::initialize(hub_dir(root.path())).expect("restart after kill");
    let identity = identify(&store);
    assert_eq!(
        latest_observed_at(&store, &identity),
        Some(sequence[2].0),
        "three commits survived"
    );
    // The open drive survives the kill: finish it and compare with a clean run.
    for item in &sequence[3..] {
        deliver(&store, &identity, item).expect("after the kill");
    }
    let history = store
        .materialised_history(identity.vehicle.vehicle_id)
        .expect("history");
    let reference = reference_history(&sequence);
    assert_eq!(history.drives, reference.drives);
    assert_eq!(history.positions, reference.positions);
}

#[test]
fn network_loss_writes_nothing_and_collection_resumes_without_a_duplicate_or_a_loss() {
    let root = private_root();
    let sequence = drive_sequence();
    let store = HubStore::initialize(hub_dir(root.path())).expect("store");
    let identity = identify(&store);
    deliver(&store, &identity, &sequence[0]).expect("before the loss");
    deliver(&store, &identity, &sequence[1]).expect("before the loss");
    let before = durable_state(&store, &identity);

    // While the network is down every poll fails and the collector has nothing
    // to deliver, so it makes no call: the Hub's durable state does not move.
    assert_eq!(durable_state(&store, &identity), before);
    drop(store);
    let store = HubStore::initialize(hub_dir(root.path())).expect("reopen during the loss");
    let identity = identify(&store);
    assert_eq!(durable_state(&store, &identity), before);

    // The network returns: the delivery after the gap lands once, and the drive
    // that spans the gap closes once with every snapshot, none duplicated.
    for item in &sequence[2..] {
        assert!(deliver(&store, &identity, item).expect("resume").append.inserted);
    }
    let history = store
        .materialised_history(identity.vehicle.vehicle_id)
        .expect("history");
    let reference = reference_history(&sequence);
    assert_eq!(history.drives, reference.drives);
    assert_eq!(history.positions, reference.positions);
}

#[test]
fn storage_limit_reached_fails_honestly_keeps_every_acknowledged_commit_and_recovers() {
    let root = private_root();
    let store = HubStore::initialize(hub_dir(root.path())).expect("store");
    let identity = identify(&store);
    drop(store);

    // The child runs under a hard file-size limit (SIGXFSZ ignored, so the
    // write returns an error as it does on a full volume instead of killing the
    // process), acknowledges each commit and reports the first failure.
    let exe = env::current_exe().expect("test binary");
    let mut child = Command::new("/bin/sh")
        .args([
            "-c",
            "trap '' XFSZ; exec \"$0\" --exact child_role_entry --nocapture --test-threads=1",
        ])
        .arg(exe)
        .env(ROLE_ENV, "fill_until_limit")
        .env(DIR_ENV, root.path())
        .env(LIMIT_ENV, (96 * 1024).to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn limited child");
    let stdout = child.stdout.take().expect("child stdout");
    let mut acknowledged: Vec<i64> = Vec::new();
    let mut failure: Option<(i64, String)> = None;
    let mut outcome = String::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("child line");
        if let Some(k) = line.strip_prefix("ACK ") {
            acknowledged.push(k.parse().expect("ack index"));
        } else if let Some(rest) = line.strip_prefix("FAILED ") {
            let (k, message) = rest.split_once(' ').unwrap_or((rest, ""));
            failure = Some((k.parse().expect("failed index"), message.to_owned()));
        } else if let Some(rest) = line.strip_prefix("MARK ") {
            outcome = rest.trim().to_owned();
        }
    }
    let status = child.wait().expect("reap the limited child");
    assert!(status.success(), "the limited child finished cleanly: {status:?}");
    assert_eq!(outcome, "limit", "the file-size limit must be reached");
    let (failed_at, message) = failure.expect("the Hub reported the failure");
    assert!(!acknowledged.is_empty(), "some commits fit under the limit");
    assert_eq!(
        acknowledged.last().copied(),
        Some(failed_at - 1),
        "acknowledgements stop exactly where the failure starts"
    );
    let lowered = message.to_lowercase();
    assert!(
        ["storage", "disk", "space", "full", "limit", "i/o", "write"]
            .iter()
            .any(|word| lowered.contains(word)),
        "the failure must name a storage problem, got {message:?}"
    );

    // Reopen without the limit. Every acknowledged commit is durable. The
    // failed delivery was never acknowledged: it is either absent or, when the
    // error came after SQLite had made the commit durable, present exactly once.
    // Either way the collector's retry leaves exactly one row for it.
    let store = HubStore::initialize(hub_dir(root.path())).expect("reopen after the limit");
    let identity_again = identify(&store);
    assert_eq!(identity_again.vehicle.vehicle_id, identity.vehicle.vehicle_id);
    let acknowledged_rows = acknowledged.len() as i64;
    let durable = raw_count(&store, &identity_again);
    assert!(
        durable == acknowledged_rows || durable == acknowledged_rows + 1,
        "durable rows {durable} must be the acknowledged {acknowledged_rows}, \
         plus at most the one failed delivery"
    );
    let observed = T0 + failed_at;
    let failed_input = input(&identity_again, observed, &payload_for_fill(failed_at));
    let retried = store
        .append_observation(&failed_input, observed)
        .expect("retry once the space is back");
    assert_eq!(
        retried.inserted,
        durable == acknowledged_rows,
        "the retry stores the failed delivery unless it was already durable"
    );
    let again = store
        .append_observation(&failed_input, observed)
        .expect("second retry");
    assert!(!again.inserted);
    assert_eq!(
        raw_count(&store, &identity_again),
        acknowledged_rows + 1,
        "one logical row for the failed delivery"
    );
}
