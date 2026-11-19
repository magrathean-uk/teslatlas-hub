// SPDX-License-Identifier: AGPL-3.0-only

//! One shutdown budget for a serving Hub (issue #38).
//!
//! A stopping service has one monotonic deadline. It starts on the first
//! accepted stop (a signal, an invalidated admission, or a worker that ended
//! on its own) and later stop requests share it. Every stage (listener and
//! collector workers, the backup schedule, the window supervisor and its
//! handback publications, background diagnostics, the final checkpoint and
//! the Tokio teardown) waits only for what is left of it, instead of each
//! stage having its own fixed wait.
//!
//! The work stages get the total less a teardown reserve; the reserve is kept
//! for the final runtime teardown. A stage that runs out keeps ownership of
//! what it waited for and reports a timeout; it never claims the work
//! finished. A SQLite commit or a file sync cannot be interrupted from here:
//! what stays running when the process exits is left to SQLite's journal and
//! to the recovery that every such writer already has on the next start.

use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// The whole budget, from the first accepted stop to the end of the Tokio
/// teardown. Below the owner's 60-second limit.
pub const SHUTDOWN_TOTAL: Duration = Duration::from_secs(50);

/// The part of [`SHUTDOWN_TOTAL`] kept for the final process teardown.
pub const TEARDOWN_RESERVE: Duration = Duration::from_secs(10);

/// How often a bounded thread join looks again.
const JOIN_POLL: Duration = Duration::from_millis(10);

/// A monotonic clock: the time since an arbitrary fixed origin. Tests inject
/// a manual one.
pub trait MonotonicClock: Send + Sync {
    fn now(&self) -> Duration;
}

/// The system's monotonic clock.
#[derive(Debug)]
pub struct SystemMonotonic {
    origin: Instant,
}

impl Default for SystemMonotonic {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemMonotonic {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A stage that did not finish within the shutdown budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownTimedOut {
    pub stage: &'static str,
    pub budget_ms: u128,
}

impl std::fmt::Display for ShutdownTimedOut {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Hub {} did not stop within the {} millisecond shutdown budget",
            self.stage, self.budget_ms
        )
    }
}

impl std::error::Error for ShutdownTimedOut {}

impl From<ShutdownTimedOut> for std::io::Error {
    fn from(error: ShutdownTimedOut) -> Self {
        std::io::Error::new(std::io::ErrorKind::TimedOut, error)
    }
}

/// The deadline shared by every stage of one service stop.
pub struct ShutdownBudget {
    clock: Arc<dyn MonotonicClock>,
    total: Duration,
    reserve: Duration,
    started: OnceLock<Duration>,
    stopping: AtomicBool,
}

impl std::fmt::Debug for ShutdownBudget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShutdownBudget")
            .field("total", &self.total)
            .field("reserve", &self.reserve)
            .field("started", &self.started.get())
            .finish()
    }
}

impl ShutdownBudget {
    /// A budget of `total`, of which `reserve` is kept for the teardown.
    pub fn new(clock: Arc<dyn MonotonicClock>, total: Duration, reserve: Duration) -> Self {
        Self {
            clock,
            total,
            reserve: reserve.min(total),
            started: OnceLock::new(),
            stopping: AtomicBool::new(false),
        }
    }

    /// The default budget on the system clock.
    pub fn system() -> Self {
        Self::new(
            Arc::new(SystemMonotonic::default()),
            SHUTDOWN_TOTAL,
            TEARDOWN_RESERVE,
        )
    }

    /// The process's budget: the one the service stop and the final Tokio
    /// teardown share.
    pub fn process() -> Arc<Self> {
        static PROCESS: OnceLock<Arc<ShutdownBudget>> = OnceLock::new();
        Arc::clone(PROCESS.get_or_init(|| Arc::new(Self::system())))
    }

    /// Accepts a stop. The first call starts the deadline; later calls share
    /// it. Returns the monotonic time the budget started.
    pub fn begin(&self) -> Duration {
        let started = *self.started.get_or_init(|| self.clock.now());
        self.stopping.store(true, Ordering::SeqCst);
        started
    }

    /// True once a stop was accepted: new work must not start.
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// The time since the stop was accepted (zero before it).
    pub fn elapsed(&self) -> Duration {
        self.started
            .get()
            .map_or(Duration::ZERO, |started| self.clock.now().saturating_sub(*started))
    }

    /// The budget of the work stages (the total less the teardown reserve).
    pub fn work_budget(&self) -> Duration {
        self.total.saturating_sub(self.reserve)
    }

    /// What the work stages have left. Asking accepts the stop.
    pub fn remaining_for_work(&self) -> Duration {
        self.begin();
        self.work_budget().saturating_sub(self.elapsed())
    }

    /// What is left of the whole budget, the teardown reserve included.
    /// Asking accepts the stop.
    pub fn remaining_total(&self) -> Duration {
        self.begin();
        self.total.saturating_sub(self.elapsed())
    }

    /// True when the work stages have no time left.
    pub fn work_exhausted(&self) -> bool {
        self.is_stopping() && self.remaining_for_work().is_zero()
    }

    /// The timeout error for `stage`.
    pub fn timed_out(&self, stage: &'static str) -> ShutdownTimedOut {
        ShutdownTimedOut {
            stage,
            budget_ms: self.work_budget().as_millis(),
        }
    }
}

/// Joins `handle` within what is left of the budget's work stages. A thread
/// that is still running when the budget runs out is handed back, still
/// owned, with the timeout.
pub fn join_within<T>(
    handle: JoinHandle<T>,
    budget: &ShutdownBudget,
    stage: &'static str,
) -> Result<std::thread::Result<T>, (JoinHandle<T>, ShutdownTimedOut)> {
    loop {
        if handle.is_finished() {
            return Ok(handle.join());
        }
        let left = budget.remaining_for_work();
        if left.is_zero() {
            return Err((handle, budget.timed_out(stage)));
        }
        std::thread::sleep(left.min(JOIN_POLL));
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    use super::MonotonicClock;

    /// A clock that moves only when a test advances it.
    #[derive(Debug, Default)]
    pub struct ManualClock {
        nanos: AtomicU64,
    }

    impl ManualClock {
        pub fn advance(&self, by: Duration) {
            self.nanos.fetch_add(
                u64::try_from(by.as_nanos()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
        }
    }

    impl MonotonicClock for ManualClock {
        fn now(&self) -> Duration {
            Duration::from_nanos(self.nanos.load(Ordering::SeqCst))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::{test_support::ManualClock, *};

    fn budget(clock: &Arc<ManualClock>) -> ShutdownBudget {
        ShutdownBudget::new(
            Arc::clone(clock) as Arc<dyn MonotonicClock>,
            SHUTDOWN_TOTAL,
            TEARDOWN_RESERVE,
        )
    }

    #[test]
    fn design_later_38_one_budget_runs_across_every_stage() {
        let clock = Arc::new(ManualClock::default());
        let budget = budget(&clock);
        // Nothing is spent before the stop is accepted.
        clock.advance(Duration::from_secs(600));
        assert!(!budget.is_stopping());
        assert_eq!(budget.elapsed(), Duration::ZERO);
        budget.begin();
        assert!(budget.is_stopping());
        assert_eq!(budget.remaining_for_work(), Duration::from_secs(40));
        assert_eq!(budget.remaining_total(), Duration::from_secs(50));
        // Each stage takes from the same deadline, not from a fresh one.
        for (stage_cost, work_left) in [(12, 28), (20, 8), (8, 0), (5, 0)] {
            clock.advance(Duration::from_secs(stage_cost));
            assert_eq!(budget.remaining_for_work(), Duration::from_secs(work_left));
        }
        assert!(budget.work_exhausted());
        // The teardown keeps its reserve until the whole budget is spent.
        assert_eq!(budget.remaining_total(), Duration::from_secs(5));
        clock.advance(Duration::from_secs(30));
        assert_eq!(budget.remaining_total(), Duration::ZERO);
        assert_eq!(
            budget.timed_out("collector").to_string(),
            "Hub collector did not stop within the 40000 millisecond shutdown budget"
        );
    }

    #[test]
    fn design_later_38_a_stop_during_startup_and_later_stops_share_the_first_deadline() {
        let clock = Arc::new(ManualClock::default());
        let budget = Arc::new(budget(&clock));
        // A stop accepted while the service still starts (its recovery wait).
        let first = budget.begin();
        clock.advance(Duration::from_secs(15));
        let others: Vec<_> = (0..4)
            .map(|_| {
                let budget = Arc::clone(&budget);
                std::thread::spawn(move || budget.begin())
            })
            .collect();
        for other in others {
            assert_eq!(other.join().unwrap(), first, "a later stop shares the deadline");
        }
        assert_eq!(budget.remaining_for_work(), Duration::from_secs(25));
    }

    #[test]
    fn design_later_38_a_slow_thread_is_handed_back_still_owned_when_the_budget_ends() {
        let clock = Arc::new(ManualClock::default());
        let budget = budget(&clock);
        budget.begin();
        let (release, released) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            released.recv().unwrap();
            7
        });
        clock.advance(Duration::from_secs(41));
        let (worker, timeout) =
            join_within(worker, &budget, "backup schedule").expect_err("budget spent");
        assert_eq!(timeout.stage, "backup schedule");
        let error = std::io::Error::from(timeout);
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        // The caller still owns the thread and its outcome.
        assert!(!worker.is_finished());
        release.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), 7);
    }

    #[test]
    fn design_later_38_a_thread_that_ends_within_the_budget_is_joined() {
        let clock = Arc::new(ManualClock::default());
        let budget = budget(&clock);
        let worker = std::thread::spawn(|| "done");
        let joined = join_within(worker, &budget, "supervisor").expect("joined");
        assert_eq!(joined.unwrap(), "done");
        assert!(budget.is_stopping(), "waiting for a stage accepts the stop");
    }
}
