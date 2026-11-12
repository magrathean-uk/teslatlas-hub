// SPDX-License-Identifier: AGPL-3.0-only

//! Custody of the one admitted credential vault between the collector and an
//! owner sign-in (DEC-088, M02-T12).
//!
//! A process admits one credential vault at a time, and a running collector
//! holds it for its whole run. An owner sign-in inside an Open window asks
//! here for custody: the window supervisor stops the collector, and only once
//! the collector has exited (and with it released the vault and every refresh
//! it could make) is custody granted. While custody is held the supervisor
//! starts no collector, so the sign-in's exchange and its write are the only
//! credential work in the process. When the custody guard is dropped the
//! supervisor starts the collector again, which opens the vault afresh and so
//! loads the credential state the sign-in left.
//!
//! A sign-in that is not granted custody within its wait is refused with a
//! fixed code and leaves the collector to restart; a second sign-in while one
//! holds or awaits custody is refused at once.

use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};

/// How long a sign-in waits for the collector to stop (as a stopping
/// service waits for its collector).
pub const CUSTODY_WAIT: Duration = Duration::from_secs(30);

/// How often a waiting sign-in wakes the supervisor again.
const CUSTODY_KICK_INTERVAL: Duration = Duration::from_millis(250);

/// Refusal: another sign-in holds or awaits custody.
pub const SIGN_IN_BUSY: &str = "sign_in_busy";
/// Refusal: the collector did not stop within [`CUSTODY_WAIT`].
pub const COLLECTOR_STILL_RUNNING: &str = "collector_still_running";

#[derive(Debug, Default)]
struct State {
    /// A sign-in has asked for the vault.
    requested: bool,
    /// The supervisor saw no collector running and handed the vault over.
    granted: bool,
}

#[derive(Debug, Default)]
struct Inner {
    state: Mutex<State>,
    changed: Condvar,
}

/// A cloneable handle; clones share one custody record.
#[derive(Debug, Clone, Default)]
pub struct CredentialCustody {
    inner: Arc<Inner>,
}

/// Held by the sign-in while it has the vault; dropping it lets the
/// supervisor start the collector again.
#[derive(Debug)]
#[must_use = "custody ends when the guard is dropped"]
pub struct CustodyGuard {
    custody: CredentialCustody,
}

impl Drop for CustodyGuard {
    fn drop(&mut self) {
        let mut state = self.custody.lock();
        state.requested = false;
        state.granted = false;
        drop(state);
        self.custody.inner.changed.notify_all();
    }
}

impl CredentialCustody {
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide record the service's supervisor and its sign-in share.
    pub fn process_wide() -> &'static CredentialCustody {
        static CUSTODY: OnceLock<CredentialCustody> = OnceLock::new();
        CUSTODY.get_or_init(CredentialCustody::new)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Supervisor: whether the collector must be stopped and kept stopped.
    pub fn requested(&self) -> bool {
        self.lock().requested
    }

    /// Supervisor: no collector is running, so a requested custody is granted.
    pub fn grant_if_requested(&self) {
        let mut state = self.lock();
        if state.requested && !state.granted {
            state.granted = true;
            drop(state);
            self.inner.changed.notify_all();
        }
    }

    /// Sign-in: asks for the vault and waits, waking the supervisor through
    /// `kick`, until the collector has exited. Refused with a fixed code when
    /// another sign-in holds or awaits custody, or when the collector has not
    /// stopped within `wait`.
    pub fn acquire(&self, wait: Duration, kick: &dyn Fn()) -> Result<CustodyGuard, &'static str> {
        {
            let mut state = self.lock();
            if state.requested {
                return Err(SIGN_IN_BUSY);
            }
            state.requested = true;
            state.granted = false;
        }
        let guard = CustodyGuard { custody: self.clone() };
        let until = Instant::now() + wait;
        loop {
            kick();
            let state = self.lock();
            if state.granted {
                return Ok(guard);
            }
            let now = Instant::now();
            if now >= until {
                drop(state);
                return Err(COLLECTOR_STILL_RUNNING);
            }
            let slice = CUSTODY_KICK_INTERVAL.min(until - now);
            let (state, _) = self
                .inner
                .changed
                .wait_timeout_while(state, slice, |state| !state.granted)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.granted {
                return Ok(guard);
            }
        }
    }
}
