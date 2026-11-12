// SPDX-License-Identifier: AGPL-3.0-only

//! Code-enforced collection windows (M02-T03).
//!
//! The Hub sends Tesla traffic only on the host a signed, G-approved grant
//! names, only while that window is open, and only through one egress gate.
//! This module owns the grant format and its single issuer, the Hub-side
//! window state machine with its durable state and ledger, the hard expiry and
//! clock guard, the egress gate, the host claim, the handback record and the
//! supervisor logic that stops collection at expiry without an operator.
//!
//! Operator verbs, configuration and the macOS service wiring belong to M02-T10;
//! the gate call sites belong to M02-T09. Nothing here reads a configuration
//! file, flag or environment variable: the ceiling and every timing constant
//! are compiled in.

pub mod anchor;
pub mod claim;
pub mod clock;
pub mod controller;
pub mod custody;
pub mod gate;
pub mod grant;
pub mod issuer;
pub mod ledger;
pub mod refusal;
pub mod state;
pub mod supervisor;

#[cfg(test)]
mod tests;

pub use anchor::{AnchorStore, KeychainAnchor, MemoryAnchor};
pub use claim::{FileLock, HostClaim, LockMode};
pub use clock::{
    ClockGuard, ManualClock, ManualProbe, OffsetProbe, SntpProbe, SystemClock, WindowClock,
};
pub use custody::{CredentialCustody, CustodyGuard};
pub use controller::{
    HandbackSource, ImportGuard, NoHighWater, ResumeOutcome, WindowConfig, WindowController,
    WindowDeps, WindowStatus,
};
pub use gate::{EgressGate, GateRefusal, Permit, PermitKind, Provider, write_under_authority, writes_in_progress};
pub use grant::{GrantBody, SignedGrant, TrustDocument};
pub use issuer::{IssueRequest, Issuer};
pub use refusal::Refusal;
pub use state::{CloseReason, Gap, Handback, HighWater, Phase};
pub use supervisor::{CollectorSlot, StepReport, Supervisor};

/// The longest window the code authorises: four hours. It covers the M04-T04
/// journey (one drive, a charge or its useful part, sleep onset and the idle
/// sample) and bounds each window to about one managed refresh. Issuer and Hub
/// both enforce it; no configuration, flag or variable can change it.
pub const WINDOW_CEILING_SECONDS: i64 = 4 * 60 * 60;

/// The gap the issuer keeps between any two grants (any host); it absorbs
/// inter-host clock skew and is at least the ten minutes of ruling R4.
pub const GRANT_GAP_SECONDS: i64 = 15 * 60;

/// Largest grant file the Hub reads.
pub const GRANT_MAX_BYTES: usize = 4096;

/// Longest approval reason.
pub const REASON_MAX_CHARS: usize = 200;

/// How often the Hub rechecks its clock inside a window.
pub const CLOCK_RECHECK_SECONDS: i64 = 60;

/// How often the Hub measures its offset against the network time server
/// inside a window (ruling R4).
pub const NTP_RECHECK_SECONDS: u64 = 300;

/// The largest accepted offset against the network time server (ruling R4).
pub const NTP_MAX_OFFSET_SECONDS: f64 = 30.0;

/// Wall elapsed time may fall short of monotonic elapsed time, and the wall
/// clock may step back from its watermark, by this much before the clock is
/// declared invalid.
pub const DRIFT_TOLERANCE_SECONDS: i64 = 5;

/// A token exchange that began before the deadline may finish within this
/// time; the window stays Closing meanwhile and is handed back within it,
/// whatever a sign-in is doing (M02-T13 round 2).
pub const EXCHANGE_GRACE_SECONDS: i64 = 60;

/// The exchanges' grace ends this long before the grace does: the supervisor
/// then cancels every exchange permit, so no further write begins, and a write
/// already past its authority check has until the handback deadline (the
/// grace less one supervisor poll) to finish. The handback is never later.
pub const EXCHANGE_WRITE_MARGIN_SECONDS: i64 = 20;

/// The supervisor notices a state change within this time.
pub const SUPERVISOR_POLL_SECONDS: i64 = 5;

/// 2020-01-01T00:00:00Z: a host clock before it is certainly wrong.
pub const CLOCK_FLOOR_EPOCH_SECONDS: i64 = 1_577_836_800;

/// Boot times read this close are the same boot (NTP may nudge the value).
pub const BOOT_TIME_TOLERANCE_SECONDS: i64 = 5;
