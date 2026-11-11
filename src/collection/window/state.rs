// SPDX-License-Identifier: AGPL-3.0-only

//! Durable window state: trust record, state file, hash-chained ledger and
//! the handback record. The files hold no token, VIN or coordinate.
//!
//! Layout under `<data dir>/window/` (0700): `trust.json`, `state.json`,
//! `ledger.jsonl` and the lock file the import path shares. Backups and
//! restores never carry this directory.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    grant::{GrantBody, TrustDocument},
    ledger::{self, LedgerError, Line},
    refusal::Refusal,
};

pub const STATE_FORMAT: &str = "window-state/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Closed,
    Pending,
    Open,
    Closing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Expired,
    OperatorClose,
    ReloginNeeded,
    OwnerApiRefused,
    ClockInvalid,
    Reboot,
    StateUnverifiable,
    ClaimLost,
}

impl CloseReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::OperatorClose => "operator_close",
            Self::ReloginNeeded => "relogin_needed",
            Self::OwnerApiRefused => "owner_api_refused",
            Self::ClockInvalid => "clock_invalid",
            Self::Reboot => "reboot",
            Self::StateUnverifiable => "state_unverifiable",
            Self::ClaimLost => "claim_lost",
        }
    }
}

/// An interval inside the window when the collector was not running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub start: i64,
    pub end: i64,
}

/// Per-table Hub high-water marks (greatest row id), when the store can say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighWater {
    pub drives: Option<i64>,
    pub charges: Option<i64>,
    pub positions: Option<i64>,
}

/// The durable record written at close: what the Hub covered, so import can
/// fill the rest from the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handback {
    pub seq: u64,
    pub grant_digest: String,
    pub opened_at: Option<i64>,
    pub closed_at: i64,
    pub reason: CloseReason,
    /// Open interval minus recorded collector gaps, in seconds.
    pub coverage_seconds: i64,
    pub gaps: Vec<Gap>,
    pub high_water: HighWater,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HubEntry {
    Trusted {
        issuer_key_id: String,
        trust_epoch: u64,
        at: i64,
    },
    Consumed {
        seq: u64,
        grant_digest: String,
        host_id: String,
        starts_at: i64,
        expires_at: i64,
        at: i64,
    },
    Opened {
        seq: u64,
        at: i64,
    },
    Resumed {
        seq: u64,
        at: i64,
    },
    Closed {
        handback: Handback,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRecord {
    pub document: TrustDocument,
    pub trusted_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowState {
    pub format: String,
    pub phase: Phase,
    pub close_reason: Option<CloseReason>,
    pub grant: Option<GrantBody>,
    pub grant_digest: Option<String>,
    pub opened_at: Option<i64>,
    pub closing_at: Option<i64>,
    pub boot_time: i64,
    pub anchor_wall: i64,
    pub watermark: i64,
    pub gaps: Vec<Gap>,
}

impl WindowState {
    pub fn closed() -> Self {
        Self {
            format: STATE_FORMAT.to_owned(),
            phase: Phase::Closed,
            close_reason: None,
            grant: None,
            grant_digest: None,
            opened_at: None,
            closing_at: None,
            boot_time: 0,
            anchor_wall: 0,
            watermark: 0,
            gaps: Vec::new(),
        }
    }
}

/// Everything read from the directory, checked.
#[derive(Debug)]
pub struct Loaded {
    pub trust: Option<TrustRecord>,
    pub state: WindowState,
    pub ledger: Vec<Line<HubEntry>>,
}

#[derive(Debug, Clone)]
pub struct WindowStore {
    dir: PathBuf,
}

impl WindowStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn trust_path(&self) -> PathBuf {
        self.dir.join("trust.json")
    }

    pub fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn ledger_path(&self) -> PathBuf {
        self.dir.join("ledger.jsonl")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.dir.join("window.lock")
    }

    pub fn ensure_dir(&self) -> Result<(), Refusal> {
        ledger::create_private_dir(&self.dir).map_err(|_| Refusal::Io)
    }

    /// Reads the directory. No trust record means nothing to verify (a fresh
    /// Hub). With a trust record, a missing, corrupt or chain-broken state or
    /// ledger is `state_unverifiable`: every open stays refused until
    /// `window trust` is rerun.
    pub fn load(&self) -> Result<Loaded, Refusal> {
        let trust = match fs::read(self.trust_path()) {
            Ok(bytes) => Some(
                serde_json::from_slice::<TrustRecord>(&bytes)
                    .map_err(|_| Refusal::StateUnverifiable)?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(Refusal::StateUnverifiable),
        };
        if trust.is_none() {
            return Ok(Loaded {
                trust,
                state: WindowState::closed(),
                ledger: Vec::new(),
            });
        }
        let state: WindowState = fs::read(self.state_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(Refusal::StateUnverifiable)?;
        if state.format != STATE_FORMAT {
            return Err(Refusal::StateUnverifiable);
        }
        let ledger = ledger::read::<HubEntry>(&self.ledger_path())
            .map_err(|_: LedgerError| Refusal::StateUnverifiable)?;
        Ok(Loaded {
            trust,
            state,
            ledger,
        })
    }

    pub fn write_state(&self, state: &WindowState) -> Result<(), Refusal> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| Refusal::Io)?;
        ledger::write_atomic(&self.state_path(), &bytes).map_err(|_| Refusal::Io)
    }

    pub fn append(&self, entry: HubEntry) -> Result<Line<HubEntry>, Refusal> {
        ledger::append(&self.ledger_path(), entry).map_err(|_| Refusal::StateUnverifiable)
    }

    /// Starts a fresh Hub ledger and state under a new trust record. An older
    /// ledger is kept beside it under a numbered name, never deleted.
    pub fn reset(&self, record: &TrustRecord) -> Result<(), Refusal> {
        self.ensure_dir()?;
        let ledger_path = self.ledger_path();
        if ledger_path.exists() {
            let archive = (1..)
                .map(|number| self.dir.join(format!("ledger.{number}.jsonl.old")))
                .find(|path| !path.exists())
                .expect("an unused archive name");
            fs::rename(&ledger_path, archive).map_err(|_| Refusal::Io)?;
        }
        ledger::create(
            &ledger_path,
            HubEntry::Trusted {
                issuer_key_id: record.document.issuer_key_id.clone(),
                trust_epoch: record.document.trust_epoch,
                at: record.trusted_at,
            },
        )
        .map_err(|_| Refusal::Io)?;
        self.write_state(&WindowState::closed())?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|_| Refusal::Io)?;
        ledger::write_atomic(&self.trust_path(), &bytes).map_err(|_| Refusal::Io)
    }
}
