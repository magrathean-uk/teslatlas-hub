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
//! the window is Closed with its handback recorded (design 2.10).

use std::{fmt, path::Path};

use super::cli::{Command, WindowCommand};
use crate::collection::window::{
    FileLock, LockMode, Phase, Refusal,
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
/// can be opened (`import_running`).
#[derive(Debug)]
pub struct ImportAdmission {
    _lock: Option<FileLock>,
}

fn ledger_synced(ledger: &[crate::collection::window::ledger::Line<HubEntry>]) -> bool {
    matches!(
        ledger.last().map(|line| &line.entry),
        None | Some(HubEntry::Trusted { .. } | HubEntry::Closed { .. })
    )
}

/// Admits a read-only import or data restore for `data_dir` only while its
/// window is Closed with the last handback recorded. It reads the window
/// directory and writes nothing: a Hub that never trusted an issuer has no
/// window to wait for. Refused `window_active` in Pending, Open and Closing,
/// `handback_pending` before the handback is synced, and
/// `state_unverifiable` for a store that cannot be read.
pub fn admit_import(data_dir: &Path) -> Result<ImportAdmission, OperatorRefusal> {
    let store = WindowStore::new(&super::window_dir(data_dir));
    if !store.dir().exists() {
        return Ok(ImportAdmission { _lock: None });
    }
    let loaded = store.load().map_err(OperatorRefusal::window)?;
    if loaded.state.phase != Phase::Closed {
        return Err(OperatorRefusal::window(Refusal::WindowActive));
    }
    if loaded.trust.is_some() && !ledger_synced(&loaded.ledger) {
        return Err(OperatorRefusal::window(Refusal::HandbackPending));
    }
    let lock = FileLock::acquire(&store.lock_path(), LockMode::Shared, false)
        .map_err(OperatorRefusal::window)?
        .ok_or_else(|| OperatorRefusal::window(Refusal::WindowActive))?;
    Ok(ImportAdmission { _lock: Some(lock) })
}
