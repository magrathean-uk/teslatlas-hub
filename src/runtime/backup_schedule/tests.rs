// SPDX-License-Identifier: AGPL-3.0-only

// M06-T03: the schedule, the manual trigger, the overlap refusal, restart,
// configuration and the secret rules. Fake runners drive time and outcomes;
// one test runs the real encrypted backup through the service's runner.
// Synthetic data only, in temporary stores; no network.

use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use serde_json::{Value, json};

use super::*;
use crate::{
    config::HubConfig,
    data_recovery::{self, DataRecoveryError, verify_encrypted_backup},
    db::{HubStore, StoreError},
    runtime::operator::{
        authority::ServiceBackend,
        channel::OperatorRequest,
    },
};

const PASSWORD: &str = "correct horse battery staple";
const HOUR: i64 = MS_PER_HOUR;

fn settings(enabled: bool) -> BackupSettings {
    BackupSettings {
        enabled,
        destination: Some(PathBuf::from("/backup/destination")),
        password_file: Some(PathBuf::from("/backup/password")),
        interval_ms: 6 * HOUR,
        keep: NonZeroUsize::new(3),
        retry_ms: 15 * 60 * 1000,
        startup_delay_ms: 2 * 60 * 1000,
    }
}

/// Records every job and answers from a script (success when it is empty).
#[derive(Default)]
struct Script {
    jobs: Mutex<Vec<BackupJob>>,
    outcomes: Mutex<VecDeque<Result<Value, OperatorRefusal>>>,
}

impl Script {
    fn next(&self, outcome: Result<Value, OperatorRefusal>) {
        self.outcomes.lock().unwrap().push_back(outcome);
    }
    fn count(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }
}

impl BackupRunner for Script {
    fn run(&self, job: &BackupJob) -> Result<Value, OperatorRefusal> {
        let n = self.count();
        self.jobs.lock().unwrap().push(job.clone());
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(json!({"status": "complete", "generation": format!("generation-{n}")})))
    }
}

fn scheduler(
    settings: BackupSettings,
    runner: Arc<dyn BackupRunner>,
    dir: &Path,
    started_ms: i64,
) -> BackupScheduler {
    BackupScheduler::start_at(
        settings,
        runner,
        dir.join(state::STATE_FILE),
        Box::new(|| false),
        started_ms,
        false,
    )
    .expect("scheduler")
}

#[test]
fn backup_schedule_runs_when_due_and_waits_the_interval_after_a_success() {
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    let schedule = scheduler(settings(true), script.clone(), dir.path(), 1_000);
    // Not before the start-up delay.
    match schedule.tick(1_000 + 60_000) {
        Tick::NotDue { wait_ms } => assert_eq!(wait_ms, 60_000),
        other => panic!("{other:?}"),
    }
    assert_eq!(script.count(), 0);
    let first = 1_000 + 2 * 60 * 1000;
    assert!(matches!(schedule.tick(first), Tick::Ran(Ok(()))));
    assert_eq!(script.count(), 1);
    let job = script.jobs.lock().unwrap()[0].clone();
    assert_eq!(job.destination, PathBuf::from("/backup/destination"));
    assert_eq!(job.password_file, PathBuf::from("/backup/password"));
    assert_eq!(job.keep, NonZeroUsize::new(3));
    // Nothing until a whole interval has passed.
    match schedule.tick(first + 6 * HOUR - 1) {
        Tick::NotDue { wait_ms } => assert_eq!(wait_ms, 1),
        other => panic!("{other:?}"),
    }
    assert!(matches!(schedule.tick(first + 6 * HOUR), Tick::Ran(Ok(()))));
    assert_eq!(script.count(), 2);
    assert_eq!(schedule.last().last_success_ms, Some(first + 6 * HOUR));
    assert_eq!(schedule.last().last_generation.as_deref(), Some("generation-1"));
}

#[test]
fn backup_schedule_a_disabled_schedule_never_runs_but_the_manual_trigger_works() {
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    let schedule = scheduler(settings(false), script.clone(), dir.path(), 0);
    assert!(matches!(schedule.tick(10 * HOUR), Tick::Disabled));
    assert_eq!(script.count(), 0);
    let report = schedule.run_now(None, None).expect("manual run");
    assert_eq!(report["trigger"], "manual");
    assert_eq!(script.count(), 1);
}

#[test]
fn backup_schedule_a_failed_run_retries_after_the_retry_wait_and_keeps_its_code() {
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    script.next(Err(OperatorRefusal::new(BACKUP_FAILED, "the destination is full")));
    let schedule = scheduler(settings(true), script.clone(), dir.path(), 0);
    let first = 2 * 60 * 1000;
    match schedule.tick(first) {
        Tick::Ran(Err(refusal)) => assert_eq!(refusal.code, BACKUP_FAILED),
        other => panic!("{other:?}"),
    }
    assert_eq!(schedule.last().last_failure_code.as_deref(), Some(BACKUP_FAILED));
    assert_eq!(schedule.last().last_success_ms, None);
    match schedule.tick(first + 14 * 60 * 1000) {
        Tick::NotDue { wait_ms } => assert_eq!(wait_ms, 60 * 1000),
        other => panic!("{other:?}"),
    }
    assert!(matches!(schedule.tick(first + 15 * 60 * 1000), Tick::Ran(Ok(()))));
    assert_eq!(schedule.last().last_failure_code, None);
}

#[test]
fn backup_schedule_refused_credentials_refuse_the_run_and_name_the_purge_command() {
    let refusal = refusal_from(
        &DataRecoveryError::Store(StoreError::BackupWaitsForCredentialCleanup),
        Some(data_recovery::BACKUP_BLOCKED_BY_REFUSED_CREDENTIALS),
    );
    assert_eq!(refusal.code, BACKUP_BLOCKED);
    assert!(refusal.message.contains("teslatlas-hub purge-refused-credentials --confirm"));
    let other = refusal_from(&DataRecoveryError::Cancelled, None);
    assert_eq!(other.code, BACKUP_CANCELLED);
    // The schedule records the refusal and retries; it never runs a backup around it.
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    script.next(Err(refusal));
    let schedule = scheduler(settings(true), script.clone(), dir.path(), 0);
    assert!(matches!(schedule.tick(2 * 60 * 1000), Tick::Ran(Err(_))));
    assert_eq!(schedule.last().last_failure_code.as_deref(), Some(BACKUP_BLOCKED));
    let manual = schedule.run_now(None, None);
    assert!(manual.is_ok(), "the next attempt runs the operation, which decides again");
}

/// Blocks inside `run` until released.
struct Blocking {
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    runs: AtomicUsize,
}

impl BackupRunner for Blocking {
    fn run(&self, _job: &BackupJob) -> Result<Value, OperatorRefusal> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.entered.send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        Ok(json!({"status": "complete", "generation": "g"}))
    }
}

#[test]
fn backup_schedule_a_second_run_is_refused_while_one_runs_and_the_timer_waits() {
    let dir = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::channel();
    let runner = Arc::new(Blocking {
        entered: entered_tx,
        release: Mutex::new(release_rx),
        runs: AtomicUsize::new(0),
    });
    let schedule = Arc::new(scheduler(settings(true), runner.clone(), dir.path(), 0));
    let manual = {
        let schedule = Arc::clone(&schedule);
        std::thread::spawn(move || schedule.run_now(None, None))
    };
    entered_rx.recv_timeout(Duration::from_secs(10)).expect("the run started");
    // A manual request and a due scheduled run both meet the held slot.
    let refused = schedule.run_now(None, None).expect_err("overlap refused");
    assert_eq!(refused.code, BACKUP_RUNNING);
    assert!(matches!(schedule.tick(10 * HOUR), Tick::Busy));
    assert_eq!(runner.runs.load(Ordering::SeqCst), 1, "the refused requests ran nothing");
    release_tx.send(()).unwrap();
    manual.join().unwrap().expect("the first run finished");
    // The slot is free again, and the success moved the schedule on.
    assert!(matches!(schedule.tick(wall_ms() + HOUR), Tick::NotDue { .. }));
}

#[test]
fn backup_schedule_the_service_refuses_an_overlap_over_the_control_channel() {
    // The same one-run slot answers the forwarded command.
    struct Backend(Arc<BackupScheduler>);
    impl ServiceBackend for Backend {
        fn setup(&self, _: crate::runtime::operator::channel::SetupMaterial, _: Option<i64>, _: bool) -> Result<Value, OperatorRefusal> { unreachable!() }
        fn recovery_export(&self, _: &Path, _: &Path) -> Result<Value, OperatorRefusal> { unreachable!() }
        fn recovery_restore(&self, _: &Path, _: &Path) -> Result<Value, OperatorRefusal> { unreachable!() }
        fn refused_credentials(&self) -> Result<Value, OperatorRefusal> { unreachable!() }
        fn purge_refused_credentials(&self) -> Result<Value, OperatorRefusal> { unreachable!() }
        fn backup_now(&self, destination: Option<&Path>, password_file: Option<&Path>) -> Result<Value, OperatorRefusal> {
            self.0.run_now(destination, password_file)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::channel();
    let runner = Arc::new(Blocking { entered: entered_tx, release: Mutex::new(release_rx), runs: AtomicUsize::new(0) });
    let schedule = Arc::new(scheduler(settings(false), runner, dir.path(), 0));
    let backend = Backend(Arc::clone(&schedule));
    let first = std::thread::spawn({
        let schedule = Arc::clone(&schedule);
        move || schedule.run_now(None, None)
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let refused = backend.backup_now(None, None).expect_err("refused");
    assert_eq!(refused.code, BACKUP_RUNNING);
    release_tx.send(()).unwrap();
    first.join().unwrap().unwrap();
    // The request type carries paths only; the default backend refuses without a schedule.
    let request = OperatorRequest::BackupNow { destination: None, password_file: Some(PathBuf::from("/p")) };
    let text = serde_json::to_string(&request).unwrap();
    assert_eq!(text, r#"{"op":"backup_now","destination":null,"password_file":"/p"}"#);
}

#[test]
fn backup_schedule_manual_trigger_uses_its_paths_refuses_relative_ones_and_needs_a_destination() {
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    let schedule = scheduler(settings(false), script.clone(), dir.path(), 0);
    schedule
        .run_now(Some(Path::new("/elsewhere")), Some(Path::new("/pw")))
        .expect("overrides");
    let job = script.jobs.lock().unwrap()[0].clone();
    assert_eq!(job.destination, PathBuf::from("/elsewhere"));
    assert_eq!(job.password_file, PathBuf::from("/pw"));
    let relative = schedule.run_now(Some(Path::new("relative")), None).expect_err("relative");
    assert_eq!(relative.code, BACKUP_PATH_INVALID);
    let mut bare = settings(false);
    bare.destination = None;
    bare.password_file = None;
    let schedule = scheduler(bare, script.clone(), dir.path(), 0);
    assert_eq!(schedule.run_now(None, None).expect_err("unconfigured").code, BACKUP_NOT_CONFIGURED);
    assert_eq!(
        schedule.run_now(Some(Path::new("/d")), None).expect_err("no password file").code,
        BACKUP_NOT_CONFIGURED
    );
    assert_eq!(script.count(), 1, "refused requests ran nothing");
}

#[test]
fn backup_schedule_restart_repeats_nothing_that_is_not_due_and_skips_nothing_overdue() {
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    let ran_at = 5 * HOUR;
    {
        let schedule = scheduler(settings(true), script.clone(), dir.path(), 0);
        assert!(matches!(schedule.tick(ran_at), Tick::Ran(Ok(()))));
    }
    assert_eq!(script.count(), 1);
    // A restart soon after: the remembered success holds the next run back.
    let restarted = scheduler(settings(true), script.clone(), dir.path(), ran_at + 1_000);
    assert_eq!(restarted.last().last_success_ms, Some(ran_at));
    match restarted.tick(ran_at + 60 * 1000) {
        Tick::NotDue { wait_ms } => assert_eq!(wait_ms, 6 * HOUR - 60 * 1000),
        other => panic!("{other:?}"),
    }
    assert_eq!(script.count(), 1);
    drop(restarted);
    // A restart after the interval: the overdue run is due at once, not
    // after another whole interval.
    let late = ran_at + 20 * HOUR;
    let overdue = scheduler(settings(true), script.clone(), dir.path(), late);
    assert!(matches!(overdue.tick(late), Tick::Ran(Ok(()))));
    assert_eq!(script.count(), 2);
    drop(overdue);
    // A damaged state file is no state: the run waits only the start-up delay.
    fs::write(dir.path().join(state::STATE_FILE), b"{not json").unwrap();
    let damaged = scheduler(settings(true), script.clone(), dir.path(), 100 * HOUR);
    assert_eq!(damaged.last(), ScheduleState::default());
    assert!(matches!(damaged.tick(100 * HOUR + 2 * 60 * 1000), Tick::Ran(Ok(()))));
    // The state file is private and holds times, a code and a generation only.
    let path = dir.path().join(state::STATE_FILE);
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("/backup/"), "{text}");
}

#[test]
fn backup_schedule_the_timer_thread_runs_on_its_own_and_stops_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    let mut quick = settings(true);
    quick.interval_ms = 120;
    quick.retry_ms = 120;
    quick.startup_delay_ms = 0;
    let schedule = BackupScheduler::start(
        quick,
        script.clone(),
        dir.path().join(state::STATE_FILE),
        Box::new(|| false),
    )
    .expect("start");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while script.count() < 3 {
        assert!(std::time::Instant::now() < deadline, "the timer ran {} times", script.count());
        std::thread::sleep(Duration::from_millis(20));
    }
    let stopped = std::time::Instant::now();
    drop(schedule);
    assert!(stopped.elapsed() < Duration::from_secs(5), "stop is prompt");
    let after = script.count();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(script.count(), after, "no run after the stop");
}

#[test]
fn backup_schedule_a_paused_service_runs_no_scheduled_backup() {
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    let paused = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&paused);
    let schedule = BackupScheduler::start_at(
        settings(true),
        script.clone(),
        dir.path().join(state::STATE_FILE),
        Box::new(move || flag.load(Ordering::SeqCst)),
        0,
        false,
    )
    .unwrap();
    assert!(matches!(schedule.tick(10 * HOUR), Tick::Busy));
    assert_eq!(script.count(), 0);
    paused.store(false, Ordering::SeqCst);
    assert!(matches!(schedule.tick(10 * HOUR), Tick::Ran(Ok(()))));
}

#[test]
fn backup_schedule_due_time_is_pure_and_tolerates_a_clock_set_back() {
    let s = settings(true);
    let mut state = ScheduleState::default();
    assert_eq!(next_due_ms(&s, &state, 1_000, 5_000), 1_000 + s.startup_delay_ms);
    state.last_success_ms = Some(10 * HOUR);
    state.last_attempt_ms = Some(10 * HOUR);
    assert_eq!(next_due_ms(&s, &state, 0, 11 * HOUR), 16 * HOUR);
    // The clock went back before the last success: counted from now.
    assert_eq!(next_due_ms(&s, &state, 0, 2 * HOUR), 2 * HOUR + s.interval_ms);
    // A failure after the success never brings the run forward.
    state.last_attempt_ms = Some(11 * HOUR);
    state.last_failure_code = Some(BACKUP_FAILED.to_owned());
    assert_eq!(next_due_ms(&s, &state, 0, 12 * HOUR), 16 * HOUR);
    // An overdue run that failed waits the retry from that attempt.
    assert_eq!(next_due_ms(&s, &state, 0, 40 * HOUR), (16 * HOUR).max(11 * HOUR + s.retry_ms));
    state.last_attempt_ms = Some(39 * HOUR);
    assert_eq!(next_due_ms(&s, &state, 0, 40 * HOUR), 39 * HOUR + s.retry_ms);
}

fn parse(extra: &str) -> Result<HubConfig, crate::config::ConfigError> {
    let source = format!("data_dir = \"/data/hub\"\nbind = \"127.0.0.1:8443\"\n{extra}");
    HubConfig::from_exact_bytes(source.as_bytes()).map(|(config, _)| config)
}

#[test]
fn backup_schedule_configuration_defaults_off_and_validates() {
    let off = parse("").expect("default");
    assert!(!off.backup.enabled);
    assert_eq!((off.backup.interval_hours, off.backup.keep), (24, 7));
    let settings = BackupSettings::from_config(&off.backup);
    assert!(!settings.enabled);
    let on = parse(
        "[backup]\nenabled = true\ndestination = \"/Volumes/Backups/hub\"\npassword_file = \"/Users/me/backup-password\"\ninterval_hours = 12\nkeep = 5\n",
    )
    .expect("enabled");
    let settings = BackupSettings::from_config(&on.backup);
    assert_eq!(settings.interval_ms, 12 * HOUR);
    assert_eq!(settings.keep, NonZeroUsize::new(5));
    assert_eq!(settings.destination.as_deref(), Some(Path::new("/Volumes/Backups/hub")));
    for bad in [
        "[backup]\nenabled = true\n",
        "[backup]\nenabled = true\ndestination = \"/b\"\n",
        "[backup]\ninterval_hours = 0\n",
        "[backup]\ninterval_hours = 9000\n",
        "[backup]\nkeep = 0\n",
        "[backup]\nkeep = 1001\n",
        "[backup]\ndestination = \"relative\"\n",
        "[backup]\npassword_file = \"relative\"\n",
        "[backup]\ndestination = \"/data/hub/backups\"\n",
        "[backup]\npassword = \"correct horse battery staple\"\n",
    ] {
        assert!(parse(bad).is_err(), "{bad}");
    }
}

fn password_file(dir: &Path, name: &str, body: &[u8], mode: u32) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[test]
fn backup_schedule_password_file_rules_and_nothing_prints_the_password() {
    let dir = crate::private_tempdir().unwrap();
    let ok = password_file(dir.path(), "ok", format!("{PASSWORD}\n").as_bytes(), 0o600);
    assert_eq!(password::read_password_file(&ok).unwrap().as_str(), PASSWORD);
    let crlf = password_file(dir.path(), "crlf", format!("{PASSWORD}\r\n").as_bytes(), 0o600);
    assert_eq!(password::read_password_file(&crlf).unwrap().as_str(), PASSWORD);
    let open = password_file(dir.path(), "open", PASSWORD.as_bytes(), 0o644);
    let big = password_file(dir.path(), "big", &vec![b'a'; 2000], 0o600);
    let binary = password_file(dir.path(), "binary", &[0xff, 0xfe, 0xfd], 0o600);
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&ok, &link).unwrap();
    for bad in [&open, &big, &binary, &link, &dir.path().join("missing"), Path::new("relative")] {
        let refusal = password::read_password_file(bad).expect_err("refused");
        assert_eq!(refusal.code, password::PASSWORD_FILE_INVALID, "{bad:?}");
        assert!(!refusal.message.contains(PASSWORD), "{refusal}");
    }
    let debug = format!("{:?}", settings(true));
    assert!(!debug.contains(PASSWORD));
}

#[test]
fn backup_schedule_real_runner_writes_a_verifiable_generation_and_leaks_no_password() {
    let root = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(root.path().join("data")).expect("store");
    store.create_pairing("Schedule fixture", 1_000, 10_000).expect("pairing");
    let destination = root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
    let password = password_file(root.path(), "password", format!("{PASSWORD}\n").as_bytes(), 0o600);
    let state_dir = root.path().join("state");
    fs::create_dir(&state_dir).unwrap();
    let mut live = settings(false);
    live.destination = Some(destination.clone());
    live.password_file = Some(password.clone());
    live.keep = NonZeroUsize::new(2);
    let schedule = scheduler(live, Arc::new(StoreBackupRunner::new(store)), &state_dir, 0);
    let mut paths = Vec::new();
    for _ in 0..3 {
        let report = schedule.run_now(None, None).expect("backup");
        assert_eq!(report["status"], "encrypted_backup_created");
        assert_eq!(report["trigger"], "manual");
        assert!(!report.to_string().contains(PASSWORD));
        paths.push(PathBuf::from(report["path"].as_str().expect("path")));
    }
    let last = paths.last().unwrap();
    verify_encrypted_backup(last, PASSWORD).expect("the newest generation verifies");
    // Keep count 2: rotation removed the oldest where the volume enforces
    // ownership, and kept every generation where it does not (never fewer
    // than the keep count either way).
    let kept = paths.iter().filter(|path| path.exists()).count();
    assert!((2..=3).contains(&kept), "kept {kept}");
    assert!(last.exists());
    let state_text = fs::read_to_string(state_dir.join(state::STATE_FILE)).unwrap();
    assert!(!state_text.contains(PASSWORD));
}

// M06-T04: the read-only status the Mac app shows.
#[test]
fn backup_schedule_status_reports_the_last_generation_next_run_and_a_block() {
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    let schedule = scheduler(settings(true), script.clone(), dir.path(), 0);
    let before = schedule.status_at(0);
    assert_eq!(before["enabled"], json!(true));
    assert_eq!(before["configured"], json!(true));
    assert_eq!(before["running"], json!(false));
    assert_eq!(before["lastGeneration"], Value::Null);
    assert_eq!(before["nextRunMs"], json!(2 * 60 * 1000));
    assert_eq!(before["intervalHours"], json!(6));
    assert_eq!(before["keep"], json!(3));
    assert!(matches!(schedule.tick(2 * 60 * 1000), Tick::Ran(Ok(()))));
    let after = schedule.status_at(2 * 60 * 1000 + 1);
    assert_eq!(after["lastGeneration"], json!("generation-0"));
    assert_eq!(after["lastSuccessMs"], json!(2 * 60 * 1000));
    assert_eq!(after["nextRunMs"], json!(2 * 60 * 1000 + 6 * HOUR));
    assert_eq!(after["blocked"], json!(false));
    // A refused-credentials block carries the service's message, which names the purge.
    script.next(Err(refusal_from(
        &DataRecoveryError::Store(StoreError::BackupWaitsForCredentialCleanup),
        Some(data_recovery::BACKUP_BLOCKED_BY_REFUSED_CREDENTIALS),
    )));
    assert!(schedule.run_now(None, None).is_err());
    let blocked = schedule.status();
    assert_eq!(blocked["blocked"], json!(true));
    assert_eq!(blocked["lastFailureCode"], json!(BACKUP_BLOCKED));
    assert!(blocked["lastFailureMessage"].as_str().unwrap().contains("purge-refused-credentials"));
    assert_eq!(blocked["lastGeneration"], json!("generation-0"), "the last good generation stays");
    // No path or password in the status.
    let text = blocked.to_string();
    assert!(!text.contains("/backup/") && !text.contains(PASSWORD));
    // Off: no next run.
    let off = scheduler(settings(false), Arc::new(Script::default()), dir.path(), 0);
    assert_eq!(off.status_at(0)["nextRunMs"], Value::Null);
}
