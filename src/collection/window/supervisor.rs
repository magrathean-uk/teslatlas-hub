// SPDX-License-Identifier: AGPL-3.0-only

//! Supervisor logic: it owns the collector slot, keeps the API serving from
//! Closed, Pending and after a collector failure, starts the collector inside
//! an Open window, restarts it with backoff, and stops it at the deadline with
//! no operator (design 2.7 and 2.8).
//!
//! The logic is synchronous and driven by `step`; the service wiring (T10)
//! calls it about every `next_wakeup_seconds`. The collector itself and its
//! lease are behind `CollectorSlot`, which T10 implements over the real
//! collector and `maintenance.rs` lease functions.

use std::sync::Arc;

use super::{
    EXCHANGE_GRACE_SECONDS,
    controller::WindowController,
    custody::CredentialCustody,
    refusal::Refusal,
    state::{CloseReason, Phase},
};

pub trait CollectorSlot {
    /// Starts the collector (taking the lease). It must stop sending when the
    /// egress gate stops issuing permits.
    fn start(&mut self, deadline_wall: i64) -> Result<(), String>;
    /// False once the collector has exited or failed.
    fn is_running(&mut self) -> bool;
    /// Signals the collector to stop; idempotent.
    fn stop(&mut self);
    /// Releases the collector lease; idempotent.
    fn release_lease(&mut self) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    pub phase: Phase,
    pub collector_running: bool,
    /// Set when this step completed a close.
    pub closed: Option<CloseReason>,
    pub fault: Option<Refusal>,
}

const BACKOFF_BASE_SECONDS: u64 = 2;
const BACKOFF_CAP_SECONDS: u64 = 60;

pub struct Supervisor<S: CollectorSlot> {
    controller: Arc<WindowController>,
    slot: S,
    failures: u32,
    retry_at_mono: u64,
    gap_start: Option<i64>,
    /// An owner sign-in's claim on the credential vault (M02-T12): while it
    /// is requested the collector is stopped and not started.
    custody: CredentialCustody,
}

impl<S: CollectorSlot> Supervisor<S> {
    pub fn new(controller: Arc<WindowController>, slot: S) -> Self {
        Self {
            controller,
            slot,
            failures: 0,
            retry_at_mono: 0,
            gap_start: None,
            custody: CredentialCustody::process_wide().clone(),
        }
    }

    /// The supervisor with a custody record of its own (tests).
    pub fn with_custody(mut self, custody: CredentialCustody) -> Self {
        self.custody = custody;
        self
    }

    pub fn slot(&self) -> &S {
        &self.slot
    }

    pub fn controller(&self) -> &Arc<WindowController> {
        &self.controller
    }

    fn note_failure(&mut self, wall: i64, mono: u64) {
        self.failures = self.failures.saturating_add(1);
        let delay = (BACKOFF_BASE_SECONDS << self.failures.min(5)).min(BACKOFF_CAP_SECONDS);
        self.retry_at_mono = mono + delay;
        self.gap_start.get_or_insert(wall);
    }

    fn end_gap(&mut self, end: i64) {
        if let Some(start) = self.gap_start.take() {
            let _ = self.controller.record_gap(start, end);
        }
    }

    /// One pass: tick the controller, then act on the phase.
    pub fn step(&mut self) -> StepReport {
        let clock = self.controller.clock().clone();
        let wall = clock.wall();
        let mono = clock.mono();
        let tick = self.controller.tick();
        let status = self.controller.status();
        let mut report = StepReport {
            phase: status.phase,
            collector_running: false,
            closed: None,
            fault: tick.err(),
        };
        match status.phase {
            Phase::Closed | Phase::Pending => {
                if self.slot.is_running() {
                    self.slot.stop();
                }
                self.failures = 0;
                self.gap_start = None;
            }
            Phase::Open if self.custody.requested() => {
                // An owner sign-in needs the vault: the collector stops and
                // stays stopped until the sign-in lets custody go.
                if self.slot.is_running() {
                    self.slot.stop();
                }
                self.gap_start.get_or_insert(wall);
            }
            Phase::Open => {
                if self.slot.is_running() {
                    self.end_gap(wall);
                    self.failures = 0;
                } else if mono >= self.retry_at_mono {
                    let deadline = status.deadline.unwrap_or(wall);
                    match self.slot.start(deadline) {
                        Ok(()) => {
                            self.end_gap(wall);
                            self.failures = 0;
                        }
                        Err(_) => self.note_failure(wall, mono),
                    }
                } else {
                    self.gap_start.get_or_insert(wall);
                }
                report.collector_running = self.slot.is_running();
            }
            Phase::Closing => {
                self.slot.stop();
                let closing_at = status.closing_at.unwrap_or(wall);
                let waited_out = wall >= closing_at + EXCHANGE_GRACE_SECONDS;
                if !self.controller.gate().exchange_in_flight() || waited_out {
                    if let Some(start) = self.gap_start.take() {
                        let _ = self.controller.record_gap(start, closing_at.max(start));
                    }
                    // The lease is released before the handback is written
                    // and the host claim let go; only then is the window Closed.
                    if self.slot.release_lease().is_ok() {
                        match self.controller.finish_close() {
                            Ok(Some(handback)) => {
                                report.closed = Some(handback.reason);
                                report.phase = Phase::Closed;
                            }
                            Ok(None) => report.phase = Phase::Closed,
                            Err(refusal) => report.fault = Some(refusal),
                        }
                    }
                }
            }
        }
        // Custody is handed over only once the collector has exited, and
        // with it released the vault.
        if self.custody.requested() && !self.slot.is_running() {
            self.custody.grant_if_requested();
        }
        report
    }
}
