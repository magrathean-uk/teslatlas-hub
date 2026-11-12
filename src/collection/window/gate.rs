// SPDX-License-Identifier: AGPL-3.0-only

//! The egress gate: one chokepoint every Tesla send asks for a permit.
//!
//! The gate is closed by default and opened only by the window controller with
//! a verified grant. It also checks the clock itself, so it stops issuing
//! permits at the deadline even when no supervisor tick has run. Once it sees
//! either deadline reached it latches closed and cancels every cancellable
//! permit, so a later clock rollback cannot reopen it; only the controller
//! opens it again, for a new window. A refused request is never retried: each
//! attempt needs a new permit.
//!
//! Local writes made under a window's authority (an owner sign-in's stored
//! pair, the cars its discovery configures) go through the write fence
//! ([`write_under_authority`]): the authority is checked and the write counted
//! under one lock, so a close that has cancelled the permits sees every write
//! that passed its check still running, and every later write refuses. The
//! supervisor waits for those writes only up to its handback deadline.

use std::{
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    EXCHANGE_GRACE_SECONDS,
    clock::{SystemClock, WindowClock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Legacy,
    Fleet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermitKind {
    /// One request.
    Request,
    /// A stream holds its permit for its life and is cancelled at close.
    Stream,
    /// A token exchange: it may finish within the grace after the deadline.
    TokenExchange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GateRefusal {
    #[error("window_closed")]
    WindowClosed,
    #[error("provider_parked")]
    ProviderParked,
}

impl GateRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::WindowClosed => "window_closed",
            Self::ProviderParked => "provider_parked",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct OpenWindow {
    deadline_wall: i64,
    deadline_mono: u64,
}

#[derive(Debug, Default)]
struct GateState {
    open: Option<OpenWindow>,
    cancellable: Vec<CancellationToken>,
    /// The tokens of token-exchange permits: a close leaves them running for
    /// the grace, and only [`EgressGate::lapse_exchanges`] cancels them.
    exchanges: Vec<CancellationToken>,
    /// The wall time at which the gate saw a deadline reached; cleared only
    /// by `open`.
    expiry_observed: Option<i64>,
}

impl GateState {
    fn shut(&mut self) {
        self.open = None;
        for token in self.cancellable.drain(..) {
            token.cancel();
        }
    }
}

#[derive(Debug)]
struct GateInner {
    clock: Arc<dyn WindowClock>,
    state: Mutex<GateState>,
    exchanges: Arc<AtomicUsize>,
    /// Every permit not yet dropped, of any kind.
    held: Arc<AtomicUsize>,
}

/// A cloneable handle; clones share one gate.
#[derive(Debug, Clone)]
pub struct EgressGate {
    inner: Arc<GateInner>,
}

/// Counts a held permit while it lives.
#[derive(Debug)]
struct ExchangeGuard(Arc<AtomicUsize>);

impl ExchangeGuard {
    fn count(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter.clone())
    }
}

impl Drop for ExchangeGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Permission for one send. Request timeouts must be cut to `time_left`.
#[derive(Debug)]
pub struct Permit {
    provider: Provider,
    kind: PermitKind,
    time_left: Duration,
    cancel: CancellationToken,
    _exchange: Option<ExchangeGuard>,
    _held: ExchangeGuard,
}

impl Permit {
    pub fn provider(&self) -> Provider {
        self.provider
    }

    pub fn kind(&self) -> PermitKind {
        self.kind
    }

    pub fn time_left(&self) -> Duration {
        self.time_left
    }

    /// Cancelled when the window closes (never for a token exchange).
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

impl EgressGate {
    pub fn new(clock: Arc<dyn WindowClock>) -> Self {
        Self {
            inner: Arc::new(GateInner {
                clock,
                state: Mutex::new(GateState::default()),
                exchanges: Arc::new(AtomicUsize::new(0)),
                held: Arc::new(AtomicUsize::new(0)),
            }),
        }
    }

    /// The process-wide gate on the system clock, closed until a window opens.
    pub fn process_wide() -> &'static EgressGate {
        static GATE: OnceLock<EgressGate> = OnceLock::new();
        GATE.get_or_init(|| EgressGate::new(Arc::new(SystemClock::new())))
    }

    /// Asks for permission to send. Called immediately before every send.
    pub fn permit(&self, provider: Provider, kind: PermitKind) -> Result<Permit, GateRefusal> {
        let mut state = self.inner.state.lock().expect("gate lock");
        let window = state.open.ok_or(GateRefusal::WindowClosed)?;
        let wall = self.inner.clock.wall();
        let mono = self.inner.clock.mono();
        if wall >= window.deadline_wall || mono >= window.deadline_mono {
            state.expiry_observed = Some(wall);
            state.shut();
            return Err(GateRefusal::WindowClosed);
        }
        if provider == Provider::Fleet {
            return Err(GateRefusal::ProviderParked);
        }
        let left_wall = (window.deadline_wall - wall) as u64;
        let left_mono = window.deadline_mono - mono;
        let mut left = left_wall.min(left_mono);
        let cancel = CancellationToken::new();
        let exchange = if kind == PermitKind::TokenExchange {
            left += EXCHANGE_GRACE_SECONDS as u64;
            state.exchanges.retain(|token| !token.is_cancelled());
            state.exchanges.push(cancel.clone());
            Some(ExchangeGuard::count(&self.inner.exchanges))
        } else {
            state.cancellable.retain(|token| !token.is_cancelled());
            state.cancellable.push(cancel.clone());
            None
        };
        Ok(Permit {
            provider,
            kind,
            time_left: Duration::from_secs(left),
            cancel,
            _exchange: exchange,
            _held: ExchangeGuard::count(&self.inner.held),
        })
    }

    /// Ends the grace of every token exchange still holding its permit: each
    /// is cancelled, so one still sending is cut and every later write under
    /// it refuses (each checks its permit through the write fence). A write
    /// already past its check finishes; the supervisor waits for it only up
    /// to its handback deadline (design 2.7, M02-T13 round 2).
    pub fn lapse_exchanges(&self) {
        for token in self.inner.state.lock().expect("gate lock").exchanges.drain(..) {
            token.cancel();
        }
    }

    /// Whether a token exchange permitted before the deadline still holds its
    /// permit: sending, or storing its answer.
    pub fn exchange_in_flight(&self) -> bool {
        self.inner.exchanges.load(Ordering::SeqCst) > 0
    }

    /// Whether any permit of this gate, of any kind, is still held (a holder
    /// may still be inside a write it began under that permit).
    pub fn permits_held(&self) -> bool {
        self.inner.held.load(Ordering::SeqCst) > 0
    }

    pub fn is_open(&self) -> bool {
        self.inner.state.lock().expect("gate lock").open.is_some()
    }

    /// When (wall time) the gate saw a deadline reached since it was last
    /// opened, if it has.
    pub fn expiry_observed(&self) -> Option<i64> {
        self.inner.state.lock().expect("gate lock").expiry_observed
    }

    pub(super) fn open(&self, deadline_wall: i64, deadline_mono: u64) {
        let mut state = self.inner.state.lock().expect("gate lock");
        state.open = Some(OpenWindow {
            deadline_wall,
            deadline_mono,
        });
        state.expiry_observed = None;
    }

    /// Stops issuing permits and cancels every request and stream permit.
    pub(super) fn close(&self) {
        self.inner.state.lock().expect("gate lock").shut();
    }

    /// Latches the gate closed if a deadline has passed (called each
    /// supervisor step).
    pub(super) fn sweep(&self) {
        let mut state = self.inner.state.lock().expect("gate lock");
        let wall = self.inner.clock.wall();
        let expired = state.open.is_some_and(|window| {
            wall >= window.deadline_wall || self.inner.clock.mono() >= window.deadline_mono
        });
        if expired {
            state.expiry_observed = Some(wall);
            state.shut();
        }
    }
}

/// The process-wide write fence (M02-T13 round 2): how many writes that
/// passed their authority check are still running.
static WRITE_FENCE: Mutex<usize> = Mutex::new(0);

fn write_fence() -> MutexGuard<'static, usize> {
    WRITE_FENCE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Ends a counted write, also when it unwinds.
struct FencedWrite;

impl Drop for FencedWrite {
    fn drop(&mut self) {
        let mut writing = write_fence();
        *writing = writing.saturating_sub(1);
    }
}

/// Runs one local write under a window's authority: `live` (the holder's
/// permit check) is asked and the write counted under the fence's lock, so
/// once a close has cancelled the permit no write begins, and a close that
/// reads [`writes_in_progress`] after cancelling sees every write that passed
/// its check. `None` when the authority has ended: nothing was written.
/// Each call is one mutation (one commit or one file publication); a caller
/// that writes several times calls this before each.
pub fn write_under_authority<T>(live: &dyn Fn() -> bool, write: impl FnOnce() -> T) -> Option<T> {
    {
        let mut writing = write_fence();
        if !live() {
            return None;
        }
        *writing += 1;
    }
    let _counted = FencedWrite;
    Some(write())
}

/// How many writes that passed their authority check are still running, in
/// this process.
pub fn writes_in_progress() -> usize {
    *write_fence()
}
