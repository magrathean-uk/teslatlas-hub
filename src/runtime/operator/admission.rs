// SPDX-License-Identifier: AGPL-3.0-only

//! Command admission (moved from the binary, M02-T10; rulings R5 and R6).
//!
//! Every parsed command is routed here before the binary reads a secret path,
//! standard input or the network. Tesla code exchange and vehicle discovery
//! run only inside the service that holds the window authority, so `setup`,
//! the window verbs and the credential-recovery verbs are forwarded to it over
//! the local control channel; with no service running they are refused
//! `service_not_running` before anything else is read. They never take the
//! data-directory lifetime lock themselves. Fleet is parked: its verbs are
//! refused `provider_parked`. Import and data restore are admitted only while
//! no window is open on this host and the selected Hub's window is Closed
//! with its handback recorded (design 2.10).

use std::{fmt, os::fd::OwnedFd, path::Path};

use rustix::{
    fs::{FlockOperation, Mode, OFlags, flock, fstat, open, stat},
    io::Errno,
};

use super::cli::{Command, WindowCommand};
use crate::collection::window::{
    HostClaim, Phase, Refusal,
    state::{HubEntry, WindowStore},
};

/// Where a command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// In this process, as before.
    Local,
    /// Forwarded to the running service over the local control channel.
    Service,
    /// Read-only import or data restore: needs the window Closed first.
    Import,
    /// Window issuer verbs: on the issuer Mac, with no Hub service involved.
    Issuer,
    /// Refused with a stable code before anything is read.
    Refused(&'static str),
}

/// A refusal with a stable machine-readable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorRefusal {
    pub code: String,
    pub message: String,
}

impl OperatorRefusal {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }

    pub fn service_not_running() -> Self {
        Self::new(
            SERVICE_NOT_RUNNING,
            "the Hub service is not running; start Hub and try again",
        )
    }

    pub fn provider_parked() -> Self {
        Self::new(
            PROVIDER_PARKED,
            "Fleet API collection is parked; the Hub signs in with its own Owner login only",
        )
    }

    pub fn window(refusal: Refusal) -> Self {
        Self::new(refusal.code(), refusal.to_string())
    }
}

impl fmt::Display for OperatorRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for OperatorRefusal {}

pub const SERVICE_NOT_RUNNING: &str = "service_not_running";
pub const PROVIDER_PARKED: &str = "provider_parked";
pub const SOURCE_CREDENTIAL_DISABLED: &str = "source_credential_disabled";
pub const SETUP_INPUT_INVALID: &str = "setup_input_invalid";

/// Routes one parsed command (pure: reads nothing).
pub fn route(command: &Command) -> Route {
    match command {
        #[cfg(unix)]
        Command::Setup { .. } => Route::Service,
        #[cfg(unix)]
        Command::SetupFleet | Command::ConfigureFleetTelemetry => Route::Refused(PROVIDER_PARKED),
        Command::ExportRecoveryCredentials { .. } | Command::RestoreRecoveryCredentials { .. } => {
            Route::Service
        }
        Command::Window { command } => match command {
            WindowCommand::Status
            | WindowCommand::Trust { .. }
            | WindowCommand::Open { .. }
            | WindowCommand::Close => Route::Service,
            WindowCommand::HostId
            | WindowCommand::IssuerInit { .. }
            | WindowCommand::IssuerTrust { .. }
            | WindowCommand::Issue { .. }
            | WindowCommand::Void { .. } => Route::Issuer,
        },
        #[cfg(unix)]
        Command::Migrate { .. } => Route::Import,
        Command::RestoreData { .. } => Route::Import,
        _ => Route::Local,
    }
}

/// Whether a locally run command takes the data-directory lifetime lock.
/// Forwarded commands never do (ruling R5).
pub fn requires_user_hub_admission(command: &Command) -> bool {
    match command {
        Command::Init | Command::Serve | Command::Pair { .. } | Command::Repair => true,
        Command::Backup { .. } => true,
        #[cfg(unix)]
        Command::Bootstrap | Command::Observe { .. } | Command::Migrate { .. } => true,
        _ => false,
    }
}

/// Held for the whole of an import or data restore; while it lives no window
/// can be opened on this host (`collector_busy`) or on this data directory
/// (`import_running`).
#[derive(Debug)]
pub struct ImportAdmission {
    _claim: OwnedFd,
    _window: Option<OwnedFd>,
}

fn ledger_synced(ledger: &[crate::collection::window::ledger::Line<HubEntry>]) -> bool {
    matches!(
        ledger.last().map(|line| &line.entry),
        None | Some(HubEntry::Trusted { .. } | HubEntry::Closed { .. })
    )
}

fn io_refusal() -> OperatorRefusal {
    OperatorRefusal::window(Refusal::Io)
}

/// Creates `dir` (and its missing parents) private to this user.
fn private_dir(dir: &Path) -> Result<(), OperatorRefusal> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|_| io_refusal())
}

/// A shared `flock` on `path` that is stable: the locked descriptor is still
/// the file at `path` after the lock is taken, so a lock file replaced
/// meanwhile cannot leave the import holding a lock nobody else checks.
/// `Ok(None)` when another holder has it exclusively.
fn stable_shared_lock(path: &Path) -> Result<Option<OwnedFd>, OperatorRefusal> {
    for _ in 0..3 {
        let fd = open(
            path,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| io_refusal())?;
        match flock(&fd, FlockOperation::NonBlockingLockShared) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => return Ok(None),
            Err(_) => return Err(io_refusal()),
        }
        let held = fstat(&fd).map_err(|_| io_refusal())?;
        match stat(path) {
            Ok(now) if now.st_dev == held.st_dev && now.st_ino == held.st_ino => {
                return Ok(Some(fd));
            }
            _ => continue,
        }
    }
    Err(io_refusal())
}

/// Admits a read-only import or data restore, binding it to this host's
/// collection authority whatever configuration selected it (review findings
/// 4 and 8). First the host claim every open window holds exclusively is
/// taken shared, so no window is open anywhere on this host for this user
/// and none can open while the admission lives; then, for a Hub data
/// directory that exists, its window lock is taken shared before its state
/// is read, so a window cannot open between the check and the lock. Both are
/// held until the admission drops. A data directory with no window yet gets
/// its lock created. Refused `window_active` while a window is Pending, Open
/// or Closing, `handback_pending` before the handback is synced, and
/// `state_unverifiable` for a store that cannot be read.
pub fn admit_import(data_dir: Option<&Path>, home: &Path) -> Result<ImportAdmission, OperatorRefusal> {
    admit_import_observed(data_dir, home, &mut || {})
}

/// [`admit_import`] with `locked` called once the locks are held and before
/// any state is read (tests pause there).
pub fn admit_import_observed(
    data_dir: Option<&Path>,
    home: &Path,
    locked: &mut dyn FnMut(),
) -> Result<ImportAdmission, OperatorRefusal> {
    let claim_path = HostClaim::default_path(home);
    if let Some(parent) = claim_path.parent() {
        private_dir(parent)?;
    }
    let claim = stable_shared_lock(&claim_path)?
        .ok_or_else(|| OperatorRefusal::window(Refusal::WindowActive))?;
    let Some(data_dir) = data_dir.filter(|dir| dir.is_dir()) else {
        locked();
        return Ok(ImportAdmission {
            _claim: claim,
            _window: None,
        });
    };
    let store = WindowStore::new(&super::window_dir(data_dir));
    private_dir(store.dir())?;
    let window = stable_shared_lock(&store.lock_path())?
        .ok_or_else(|| OperatorRefusal::window(Refusal::WindowActive))?;
    locked();
    let loaded = store.load().map_err(OperatorRefusal::window)?;
    if loaded.state.phase != Phase::Closed {
        return Err(OperatorRefusal::window(Refusal::WindowActive));
    }
    if loaded.trust.is_some() && !ledger_synced(&loaded.ledger) {
        return Err(OperatorRefusal::window(Refusal::HandbackPending));
    }
    Ok(ImportAdmission {
        _claim: claim,
        _window: Some(window),
    })
}
