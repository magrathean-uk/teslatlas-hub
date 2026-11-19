// SPDX-License-Identifier: AGPL-3.0-only

//! Scheduled and manual encrypted backups in the service (M06-T03).
//!
//! The service runs the M06-T01 operation (`data_recovery::create_encrypted_backup`)
//! from its `[backup]` configuration on a timer, and the `backup-now` command,
//! forwarded over the control channel, runs the same operation on request.
//! One run at a time: a second request while one runs is refused, never
//! queued. The schedule survives a restart through a small state file of
//! times (see [`state`]), so a restart repeats nothing that is not due and
//! skips nothing that is overdue. The recovery password is read at each run
//! from the one source `backup.password_source` names (M06-T13): the Keychain
//! item the app wrote on the Mac ([`keychain`], M06-T12), or the owned file
//! the configuration names ([`password`]); neither falls back to the other. A
//! manual run may name a file of its own. It is never stored by the service,
//! sent over the channel or logged. A run while refused credentials
//! block backups is refused with the refusal that names
//! `purge-refused-credentials`. Rotation (keep count) is the operation's own:
//! on a destination that does not enforce ownership it keeps every
//! generation and says so in the report. A run counts as the last good
//! backup only when the operation succeeds, which it does only once the
//! complete published file has authenticated as the object it wrote (M06-T11);
//! an invalid publication is refused with its own code and records no
//! success and no generation.

pub mod keychain;
pub mod password;
pub mod state;

use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use crate::{
    data_recovery::{
        DataRecoveryError, EncryptedBackupRequest, create_encrypted_backup,
        refused_credential_backup_message,
    },
    db::HubStore,
    runtime::{
        config::{BackupConfig, BackupPasswordSource},
        operator::admission::OperatorRefusal,
    },
};
use state::ScheduleState;

#[cfg(test)]
mod tests;

pub const BACKUP_RUNNING: &str = "backup_running";
pub const BACKUP_NOT_CONFIGURED: &str = "backup_not_configured";
pub const BACKUP_PATH_INVALID: &str = "backup_path_invalid";
pub const BACKUP_BLOCKED: &str = "backup_blocked_by_refused_credentials";
pub const BACKUP_CANCELLED: &str = "backup_cancelled";
pub const BACKUP_FAILED: &str = "backup_failed";
/// The published file did not authenticate as the generation the run wrote.
pub const BACKUP_PUBLICATION_INVALID: &str = "backup_publication_invalid";

/// How the operation's error for an invalid publication begins (the
/// operation's `invalid_publication` prefix).
const INVALID_PUBLICATION_PREFIX: &str = "invalid_publication:";

const MS_PER_HOUR: i64 = 60 * 60 * 1000;
/// After a failed attempt the schedule tries again after this long (never
/// longer than the interval).
pub const RETRY_AFTER_MS: i64 = 15 * 60 * 1000;
/// The first scheduled run of a service start that has no recorded success
/// waits this long, so start-up settles first.
pub const STARTUP_DELAY_MS: i64 = 2 * 60 * 1000;
/// How long the timer waits when a run holds the slot or the service is paused.
const BUSY_RECHECK_MS: i64 = 5_000;
const LONGEST_WAIT_MS: i64 = MS_PER_HOUR;
const SHORTEST_WAIT_MS: i64 = 50;

/// What the schedule needs, in the units it works in.
#[derive(Debug, Clone)]
pub struct BackupSettings {
    pub enabled: bool,
    pub destination: Option<PathBuf>,
    /// Where an unattended run reads the password: only the configured
    /// source. None when the configuration names none.
    pub password: Option<PasswordSource>,
    pub interval_ms: i64,
    pub keep: Option<NonZeroUsize>,
    pub retry_ms: i64,
    pub startup_delay_ms: i64,
}

impl BackupSettings {
    pub fn from_config(config: &BackupConfig) -> Self {
        Self::from_config_on(config, cfg!(target_os = "macos"))
    }

    /// [`Self::from_config`] on the Mac (`macos`) or elsewhere.
    pub fn from_config_on(config: &BackupConfig, macos: bool) -> Self {
        let interval_ms = i64::from(config.interval_hours).saturating_mul(MS_PER_HOUR);
        Self {
            enabled: config.enabled,
            destination: config.destination.clone(),
            password: Self::password_on(config, macos),
            interval_ms,
            keep: NonZeroUsize::new(usize::try_from(config.keep).unwrap_or(usize::MAX)),
            retry_ms: RETRY_AFTER_MS.min(interval_ms),
            startup_delay_ms: STARTUP_DELAY_MS.min(interval_ms),
        }
    }

    /// The password source a configuration names. With no
    /// `password_source` (written before M06-T13) it means what it meant: on
    /// the Mac the Keychain item whose account is `password_file`, elsewhere
    /// that file, and nothing without a `password_file`. With `keychain` the
    /// account is `password_file` or [`keychain::DEFAULT_ACCOUNT`]; with
    /// `file` it is the file.
    fn password_on(config: &BackupConfig, macos: bool) -> Option<PasswordSource> {
        let path = config.password_file.as_deref();
        match config.password_source_on(macos) {
            BackupPasswordSource::Keychain => {
                if config.password_source.is_none() && path.is_none() {
                    return None;
                }
                Some(PasswordSource::Keychain {
                    account: path.map_or_else(
                        || keychain::DEFAULT_ACCOUNT.to_owned(),
                        |path| path.to_string_lossy().into_owned(),
                    ),
                })
            }
            BackupPasswordSource::File => path.map(|path| PasswordSource::File(path.to_path_buf())),
        }
    }
}

/// Where a run reads the recovery password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordSource {
    /// An owned mode-0600 file: the `file` source, and a manual run's
    /// `--password-file` everywhere.
    File(PathBuf),
    /// The Mac Keychain item the app wrote for this configuration (the
    /// `keychain` source); its account is the configured
    /// `backup.password_file` path, which holds no file, or
    /// [`keychain::DEFAULT_ACCOUNT`] when none is configured.
    Keychain { account: String },
}

/// One run's inputs. The password stays in its file or Keychain item until
/// the run reads it.
#[derive(Debug, Clone)]
pub struct BackupJob {
    pub destination: PathBuf,
    pub password: PasswordSource,
    pub keep: Option<NonZeroUsize>,
    /// Set when the service stops; the run checks it per chunk.
    pub cancel: Arc<AtomicBool>,
}

/// What a run does. The service's is [`StoreBackupRunner`]; tests supply others.
pub trait BackupRunner: Send + Sync {
    fn run(&self, job: &BackupJob) -> Result<Value, OperatorRefusal>;
}

/// The refusal for a failed run. `blocked` is the message that names the
/// purge command while refused credentials block backups. No error text
/// carries the password.
pub fn refusal_from(error: &DataRecoveryError, blocked: Option<&'static str>) -> OperatorRefusal {
    if let Some(message) = blocked {
        return OperatorRefusal::new(BACKUP_BLOCKED, message);
    }
    match error {
        DataRecoveryError::Cancelled => {
            OperatorRefusal::new(BACKUP_CANCELLED, "the backup was cancelled")
        }
        DataRecoveryError::InvalidBackup(message)
            if message.starts_with(INVALID_PUBLICATION_PREFIX) =>
        {
            OperatorRefusal::new(BACKUP_PUBLICATION_INVALID, error.to_string())
        }
        other => OperatorRefusal::new(BACKUP_FAILED, other.to_string()),
    }
}

/// Runs the M06-T01 operation against the service's store.
pub struct StoreBackupRunner {
    store: HubStore,
    keychain: Arc<dyn keychain::PasswordKeychain>,
}

impl StoreBackupRunner {
    pub fn new(store: HubStore) -> Self {
        Self::with_keychain(store, keychain::system_keychain())
    }

    /// The runner with another Keychain (tests).
    pub fn with_keychain(store: HubStore, keychain: Arc<dyn keychain::PasswordKeychain>) -> Self {
        Self { store, keychain }
    }
}

impl BackupRunner for StoreBackupRunner {
    fn run(&self, job: &BackupJob) -> Result<Value, OperatorRefusal> {
        let password = match &job.password {
            PasswordSource::File(path) => password::read_password_file(path)?,
            PasswordSource::Keychain { account } => {
                keychain::read_keychain_password(self.keychain.as_ref(), account)?
            }
        };
        let mut request = EncryptedBackupRequest::new(&job.destination, password.as_str());
        request.keep = job.keep;
        request.cancel = Some(Arc::clone(&job.cancel));
        let report = create_encrypted_backup(&self.store, &request).map_err(|error| {
            refusal_from(&error, refused_credential_backup_message(&self.store, &error))
        })?;
        serde_json::to_value(&report)
            .map_err(|_| OperatorRefusal::new(BACKUP_FAILED, "the report is not encodable"))
    }
}

/// When the next scheduled run is due (Unix milliseconds). Pure.
///
/// The base is the last success plus the interval, or, with no success, the
/// service start plus its delay. A failed latest attempt delays the run to
/// at least that attempt plus the retry wait. A recorded time in the future
/// (a clock set back) counts as now.
pub fn next_due_ms(
    settings: &BackupSettings,
    state: &ScheduleState,
    started_ms: i64,
    now_ms: i64,
) -> i64 {
    let base = match state.last_success_ms {
        Some(success) => success.min(now_ms).saturating_add(settings.interval_ms),
        None => started_ms.saturating_add(settings.startup_delay_ms),
    };
    match (state.last_failure_code.as_ref(), state.last_attempt_ms) {
        (Some(_), Some(attempt)) => base.max(attempt.min(now_ms).saturating_add(settings.retry_ms)),
        _ => base,
    }
}

/// The outcome of one timer step.
#[derive(Debug)]
pub enum Tick {
    /// The schedule is off.
    Disabled,
    /// Nothing is due yet.
    NotDue { wait_ms: i64 },
    /// Due, but another run holds the slot or the service is paused.
    Busy,
    /// A scheduled run finished (its refusal when it failed).
    Ran(Result<(), OperatorRefusal>),
}

struct Shared {
    settings: BackupSettings,
    runner: Arc<dyn BackupRunner>,
    state_path: PathBuf,
    started_ms: i64,
    running: AtomicBool,
    cancel: Arc<AtomicBool>,
    state: Mutex<ScheduleState>,
    /// The latest failure's message, kept in memory only (the state file
    /// holds codes, never text), so status can show the service's words.
    last_failure_message: Mutex<Option<String>>,
    paused: Box<dyn Fn() -> bool + Send + Sync>,
    wake: Condvar,
    wake_lock: Mutex<()>,
}

/// Holds the one run slot; dropping it frees the slot.
struct RunSlot<'a>(&'a AtomicBool);

impl<'a> RunSlot<'a> {
    fn take(flag: &'a AtomicBool) -> Result<Self, OperatorRefusal> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| Self(flag))
            .map_err(|_| {
                OperatorRefusal::new(BACKUP_RUNNING, "a backup is already running; try again when it has finished")
            })
    }
}

impl Drop for RunSlot<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Unix milliseconds now.
pub fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

impl Shared {
    fn execute(&self, job: &BackupJob, now_ms: i64) -> Result<Value, OperatorRefusal> {
        let _slot = RunSlot::take(&self.running)?;
        let result = self.runner.run(job);
        let mut state = lock(&self.state);
        state.last_attempt_ms = Some(now_ms);
        match &result {
            Ok(report) => {
                state.last_success_ms = Some(now_ms);
                state.last_failure_code = None;
                state.last_generation = report
                    .get("generation")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Err(refusal) => state.last_failure_code = Some(refusal.code.clone()),
        }
        *lock(&self.last_failure_message) = result.as_ref().err().map(|refusal| refusal.message.clone());
        if let Err(error) = state::store(&self.state_path, &state) {
            tracing::warn!(kind = ?error.kind(), "the backup schedule state could not be written");
        }
        drop(state);
        self.wake.notify_all();
        result
    }

    fn scheduled_job(&self) -> Option<BackupJob> {
        Some(BackupJob {
            destination: self.settings.destination.clone()?,
            password: self.settings.password.clone()?,
            keep: self.settings.keep,
            cancel: Arc::clone(&self.cancel),
        })
    }

    fn tick(&self, now_ms: i64) -> Tick {
        if !self.settings.enabled {
            return Tick::Disabled;
        }
        let due = next_due_ms(&self.settings, &lock(&self.state), self.started_ms, now_ms);
        if now_ms < due {
            return Tick::NotDue { wait_ms: due - now_ms };
        }
        if (self.paused)() {
            return Tick::Busy;
        }
        let Some(job) = self.scheduled_job() else {
            return Tick::Disabled;
        };
        match self.execute(&job, now_ms) {
            Err(refusal) if refusal.code == BACKUP_RUNNING => Tick::Busy,
            Ok(_) => {
                tracing::info!("scheduled backup finished");
                Tick::Ran(Ok(()))
            }
            Err(refusal) => {
                tracing::warn!(code = refusal.code.as_str(), "scheduled backup failed");
                Tick::Ran(Err(refusal))
            }
        }
    }

    fn timer(&self) {
        while !self.cancel.load(Ordering::SeqCst) {
            let wait_ms = match self.tick(wall_ms()) {
                Tick::Disabled => return,
                Tick::NotDue { wait_ms } => wait_ms,
                Tick::Busy => BUSY_RECHECK_MS,
                Tick::Ran(_) => {
                    next_due_ms(&self.settings, &lock(&self.state), self.started_ms, wall_ms())
                        - wall_ms()
                }
            };
            let wait = Duration::from_millis(
                u64::try_from(wait_ms.clamp(SHORTEST_WAIT_MS, LONGEST_WAIT_MS)).unwrap_or(1),
            );
            let guard = lock(&self.wake_lock);
            if self.cancel.load(Ordering::SeqCst) {
                return;
            }
            drop(
                self.wake
                    .wait_timeout(guard, wait)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }
}

/// The service's backup schedule and its manual trigger. Dropping it stops
/// the timer and cancels a run in progress.
pub struct BackupScheduler {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl BackupScheduler {
    /// Starts the schedule (a timer thread only when it is enabled). `paused`
    /// holds scheduled runs back, for example while the Hub waits for its
    /// restored keys; the manual command is guarded by the channel instead.
    pub fn start(
        settings: BackupSettings,
        runner: Arc<dyn BackupRunner>,
        state_path: PathBuf,
        paused: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> std::io::Result<Self> {
        Self::start_at(settings, runner, state_path, paused, wall_ms(), true)
    }

    fn start_at(
        settings: BackupSettings,
        runner: Arc<dyn BackupRunner>,
        state_path: PathBuf,
        paused: Box<dyn Fn() -> bool + Send + Sync>,
        started_ms: i64,
        spawn_timer: bool,
    ) -> std::io::Result<Self> {
        let enabled = settings.enabled;
        let loaded = state::load(&state_path);
        let shared = Arc::new(Shared {
            settings,
            runner,
            state_path,
            started_ms,
            running: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(loaded),
            last_failure_message: Mutex::new(None),
            paused,
            wake: Condvar::new(),
            wake_lock: Mutex::new(()),
        });
        let thread = if enabled && spawn_timer {
            let timer = Arc::clone(&shared);
            Some(
                std::thread::Builder::new()
                    .name("hub-backup-schedule".to_owned())
                    .spawn(move || timer.timer())?,
            )
        } else {
            None
        };
        Ok(Self {
            shared,
            thread: Mutex::new(thread),
        })
    }

    /// One manual run, the same operation as a scheduled one. `destination`
    /// defaults to the configuration's; a `password_file` given here is read as
    /// a file, otherwise the configured source is used. Both paths must be
    /// absolute. Returns the operation's report, or the refusal.
    pub fn run_now(
        &self,
        destination: Option<&Path>,
        password_file: Option<&Path>,
    ) -> Result<Value, OperatorRefusal> {
        let settings = &self.shared.settings;
        let destination = destination
            .map(Path::to_path_buf)
            .or_else(|| settings.destination.clone())
            .ok_or_else(|| {
                OperatorRefusal::new(
                    BACKUP_NOT_CONFIGURED,
                    "no backup destination: set backup.destination or pass --destination",
                )
            })?;
        let password = password_file
            .map(|path| PasswordSource::File(path.to_path_buf()))
            .or_else(|| settings.password.clone())
            .ok_or_else(|| {
                OperatorRefusal::new(
                    BACKUP_NOT_CONFIGURED,
                    "no backup password source: save the recovery password in Tesatlas Hub (Service details, Backups), set backup.password_source (\"keychain\" on the Mac, or \"file\" with backup.password_file), or pass --password-file",
                )
            })?;
        let file_is_relative =
            matches!(&password, PasswordSource::File(path) if !path.is_absolute());
        if !destination.is_absolute() || file_is_relative {
            return Err(OperatorRefusal::new(
                BACKUP_PATH_INVALID,
                "the backup destination and password file are named by absolute paths",
            ));
        }
        // A stopping service starts no new backup (issue #38).
        if self.shared.cancel.load(Ordering::SeqCst) {
            return Err(OperatorRefusal::new(
                BACKUP_CANCELLED,
                "the service is stopping; no backup starts",
            ));
        }
        let job = BackupJob {
            destination,
            password,
            keep: settings.keep,
            cancel: Arc::clone(&self.shared.cancel),
        };
        let mut report = self.shared.execute(&job, wall_ms())?;
        if let Value::Object(map) = &mut report {
            map.insert("trigger".to_owned(), Value::from("manual"));
        }
        Ok(report)
    }

    /// The remembered times, for status.
    pub fn last(&self) -> ScheduleState {
        lock(&self.shared.state).clone()
    }

    /// The schedule's state for `backup-status` (M06-T04): read-only, with no
    /// path and no password. `nextRunMs` is absent when the schedule is off or
    /// has no destination or password file; `blocked` is set while refused
    /// credentials block backups, with the service's message naming the purge.
    pub fn status(&self) -> Value {
        self.status_at(wall_ms())
    }

    fn status_at(&self, now_ms: i64) -> Value {
        let shared = &self.shared;
        let settings = &shared.settings;
        let state = self.last();
        let configured =
            settings.destination.is_some() && settings.password.is_some();
        let next_run_ms = (settings.enabled && configured)
            .then(|| next_due_ms(settings, &state, shared.started_ms, now_ms));
        let message = lock(&shared.last_failure_message).clone();
        let blocked = state.last_failure_code.as_deref() == Some(BACKUP_BLOCKED);
        serde_json::json!({
            "enabled": settings.enabled,
            "configured": configured,
            "intervalHours": settings.interval_ms / MS_PER_HOUR,
            "keep": settings.keep.map(NonZeroUsize::get),
            "running": shared.running.load(Ordering::SeqCst),
            "paused": (shared.paused)(),
            "lastAttemptMs": state.last_attempt_ms,
            "lastSuccessMs": state.last_success_ms,
            "lastGeneration": state.last_generation,
            "lastFailureCode": state.last_failure_code,
            "lastFailureMessage": state.last_failure_code.as_ref().and(message),
            "blocked": blocked,
            "nextRunMs": next_run_ms,
        })
    }

    /// Steps the timer once at `now_ms` (tests drive time through this).
    #[cfg(test)]
    fn tick(&self, now_ms: i64) -> Tick {
        self.shared.tick(now_ms)
    }

    /// Quiesces the schedule at once (issue #38): the timer stops, a run in
    /// progress is cancelled at its next chunk and no new run starts. Does
    /// not wait.
    pub fn request_stop(&self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
        let _guard = lock(&self.shared.wake_lock);
        self.shared.wake.notify_all();
    }

    /// Stops the timer and cancels a run in progress.
    pub fn stop(&self) {
        self.request_stop();
        let handle = lock(&self.thread).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// Stops the timer and waits for it only within what is left of the
    /// service's shutdown budget. A run still finishing a chunk when the
    /// budget ends stays owned here (a later [`Self::stop`] or the drop
    /// joins it) and the timeout is reported; the run itself sees the
    /// cancellation at its next chunk.
    pub fn stop_within(
        &self,
        budget: &crate::runtime::shutdown::ShutdownBudget,
    ) -> Result<(), crate::runtime::shutdown::ShutdownTimedOut> {
        self.request_stop();
        let handle = lock(&self.thread).take();
        let Some(handle) = handle else {
            return Ok(());
        };
        match crate::runtime::shutdown::join_within(handle, budget, "backup schedule") {
            Ok(_) => Ok(()),
            Err((handle, timeout)) => {
                *lock(&self.thread) = Some(handle);
                Err(timeout)
            }
        }
    }
}

impl Drop for BackupScheduler {
    fn drop(&mut self) {
        // A stop that already ran out of its shutdown budget is not waited
        // for again here: the process is exiting.
        let budget = crate::runtime::shutdown::ShutdownBudget::process();
        if budget.is_stopping() {
            let _ = self.stop_within(&budget);
        } else {
            self.stop();
        }
    }
}
