// SPDX-License-Identifier: AGPL-3.0-only

//! R78 service coordination independent of a service manager or sign-in core.
//!
//! An effects implementation must honour each supplied deadline, report an
//! unknown observation as an error, and establish stable readiness itself.
//! Storage is deliberately outside the service deadlines. This module has no
//! system executor; wiring a production backend and command caller is separate.

use std::{
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadinessBounds {
    pub poll: Duration,
    pub connect: Duration,
    pub request: Duration,
    pub stable_observations: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceBudgets {
    pub command: Duration,
    pub unload: Duration,
    pub admission: Duration,
    pub stop: Duration,
    pub restart: Duration,
    pub total_service: Duration,
    pub readiness: ReadinessBounds,
}

impl ServiceBudgets {
    pub const PRODUCTION: Self = Self {
        command: Duration::from_secs(30),
        unload: Duration::from_secs(300),
        admission: Duration::from_secs(30),
        stop: Duration::from_secs(390),
        restart: Duration::from_secs(150),
        total_service: Duration::from_secs(540),
        readiness: ReadinessBounds {
            poll: Duration::from_millis(100),
            connect: Duration::from_millis(100),
            request: Duration::from_millis(500),
            stable_observations: 10,
        },
    };

    pub const SCALED_TEST: Self = Self {
        command: Duration::from_secs(3),
        unload: Duration::from_secs(30),
        admission: Duration::from_secs(3),
        stop: Duration::from_secs(39),
        restart: Duration::from_secs(15),
        total_service: Duration::from_secs(54),
        readiness: ReadinessBounds {
            poll: Duration::from_millis(10),
            connect: Duration::from_millis(10),
            request: Duration::from_millis(50),
            stable_observations: 10,
        },
    };
}

/// All service observations and effects required by this coordinator.
/// Implementations must not turn an unknown observation into `false`.
pub trait ServiceEffects {
    type Admission;
    type Error;

    fn now(&self) -> Instant;
    fn loaded(&mut self, deadline: Instant) -> Result<bool, Self::Error>;
    fn bootout(&mut self, deadline: Instant) -> Result<(), Self::Error>;
    fn wait_unloaded(&mut self, deadline: Instant) -> Result<(), Self::Error>;
    fn admit(&mut self, deadline: Instant) -> Result<Self::Admission, Self::Error>;
    fn bootstrap(&mut self, deadline: Instant) -> Result<(), Self::Error>;
    fn kickstart(&mut self, deadline: Instant) -> Result<(), Self::Error>;
    fn wait_ready(&mut self, deadline: Instant, bounds: ReadinessBounds)
    -> Result<(), Self::Error>;

    /// Retain a restart failure for the caller when work unwinds. This must
    /// preserve the failure's stage and cause and must not panic.
    fn record_unwind_restart_failure(&mut self, failure: ServiceFailure<Self::Error>);
}

/// Evidence that the coordinator holds admission for this callback only.
///
/// The token cannot be cloned or used to obtain the underlying admission.
/// A production caller that needs admission-dependent operations requires a
/// separately reviewed interface; this synthetic seam does not provide one.
///
/// The public library path is usable by a caller:
///
/// ```
/// use std::sync::Arc;
/// use teslatlas_hub::platform::r78_lifecycle::ScopedAdmission;
/// fn accepts(_: ScopedAdmission<'_, Arc<()>>) {}
/// let _ = accepts;
/// ```
///
/// Even when the underlying admission is cloneable, the token is not:
///
/// ```compile_fail
/// use std::sync::Arc;
/// use teslatlas_hub::platform::r78_lifecycle::ScopedAdmission;
/// fn duplicate(token: ScopedAdmission<'_, Arc<()>>) {
///     let _copy = token.clone();
/// }
/// ```
///
/// The wrapped value is private and cannot be cloned through the token:
///
/// ```compile_fail
/// use std::sync::Arc;
/// use teslatlas_hub::platform::r78_lifecycle::ScopedAdmission;
/// fn extract(token: ScopedAdmission<'_, Arc<()>>) {
///     let _copy = token._admission.clone();
/// }
/// ```
///
/// A callback cannot retain the scoped token after the work returns:
///
/// ```compile_fail
/// use teslatlas_hub::platform::r78_lifecycle::{
///     ServiceBudgets, ServiceEffects, with_transient_pause,
/// };
/// fn retain<E: ServiceEffects>(effects: &mut E) {
///     let mut saved = None;
///     let _ = with_transient_pause(effects, ServiceBudgets::SCALED_TEST, |token| {
///         saved = Some(token);
///         Ok::<(), ()>(())
///     });
///     drop(saved);
/// }
/// ```
pub struct ScopedAdmission<'a, A> {
    _admission: &'a mut A,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    InitialQuery,
    Bootout,
    Unload,
    Admission,
    RestartQuery,
    Bootstrap,
    Kickstart,
    Readiness,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServiceFailure<E> {
    Backend { stage: Stage, error: E },
    TimedOut { stage: Stage },
    Panicked { stage: Stage },
}

#[derive(Debug, PartialEq, Eq)]
pub enum PrimaryFailure<E, W> {
    Service(ServiceFailure<E>),
    Work(W),
}

/// A failed restart retains the original result for the command owner to classify.
#[derive(Debug, PartialEq, Eq)]
pub enum Completion<T, E, W> {
    Finished(Result<T, PrimaryFailure<E, W>>),
    RestartFailed {
        original: Result<T, PrimaryFailure<E, W>>,
        restart: ServiceFailure<E>,
    },
}

fn bounded<E: ServiceEffects, T>(
    effects: &mut E,
    upper: Instant,
    allowance: Duration,
    stage: Stage,
    action: impl FnOnce(&mut E, Instant) -> Result<T, E::Error>,
) -> Result<T, ServiceFailure<E::Error>> {
    let now = effects.now();
    let deadline = now.checked_add(allowance).unwrap_or(upper).min(upper);
    if now >= deadline {
        return Err(ServiceFailure::TimedOut { stage });
    }
    let result = catch_unwind(AssertUnwindSafe(|| action(effects, deadline)))
        .map_err(|_| ServiceFailure::Panicked { stage })?;
    if effects.now() > deadline {
        return Err(ServiceFailure::TimedOut { stage });
    }
    result.map_err(|error| ServiceFailure::Backend { stage, error })
}

fn restart<E: ServiceEffects>(
    effects: &mut E,
    budgets: ServiceBudgets,
) -> Result<(), ServiceFailure<E::Error>> {
    let now = effects.now();
    let upper = now.checked_add(budgets.restart).unwrap_or(now);
    let loaded = bounded(
        effects,
        upper,
        budgets.command,
        Stage::RestartQuery,
        E::loaded,
    )?;
    if !loaded {
        bounded(
            effects,
            upper,
            budgets.command,
            Stage::Bootstrap,
            E::bootstrap,
        )?;
    }
    bounded(
        effects,
        upper,
        budgets.command,
        Stage::Kickstart,
        E::kickstart,
    )?;
    bounded(
        effects,
        upper,
        budgets.restart,
        Stage::Readiness,
        |effects, deadline| effects.wait_ready(deadline, budgets.readiness),
    )
}

/// Run work while the initially loaded service is transiently paused.
///
/// The initial observation creates restart duty before any stop is attempted.
/// The admission value is dropped before restart, including when work panics.
/// The scoped token cannot expose or retain the admission. The work callback
/// is not passed a service deadline or a cancellation check. If work panics
/// and restart fails, the backend retains that failure for the caller before
/// the original panic resumes.
pub fn with_transient_pause<E, T, W>(
    effects: &mut E,
    budgets: ServiceBudgets,
    work: impl for<'a> FnOnce(ScopedAdmission<'a, E::Admission>) -> Result<T, W>,
) -> Completion<T, E::Error, W>
where
    E: ServiceEffects,
{
    let now = effects.now();
    let stop_upper = now.checked_add(budgets.stop).unwrap_or(now);
    let initially_loaded = match bounded(
        effects,
        stop_upper,
        budgets.command,
        Stage::InitialQuery,
        E::loaded,
    ) {
        Ok(loaded) => loaded,
        Err(error) => return Completion::Finished(Err(PrimaryFailure::Service(error))),
    };

    // The service must be restarted even when the callback unwinds. Admission
    // belongs to this closure, so unwinding drops it before restart begins.
    let original = catch_unwind(AssertUnwindSafe(|| {
        if initially_loaded {
            bounded(
                effects,
                stop_upper,
                budgets.command,
                Stage::Bootout,
                E::bootout,
            )
            .map_err(PrimaryFailure::Service)?;
            bounded(
                effects,
                stop_upper,
                budgets.unload,
                Stage::Unload,
                E::wait_unloaded,
            )
            .map_err(PrimaryFailure::Service)?;
        }
        let mut admission = bounded(
            effects,
            stop_upper,
            budgets.admission,
            Stage::Admission,
            E::admit,
        )
        .map_err(PrimaryFailure::Service)?;
        let result = work(ScopedAdmission {
            _admission: &mut admission,
        })
        .map_err(PrimaryFailure::Work);
        drop(admission);
        result
    }));

    let restart_result = if initially_loaded {
        Some(match catch_unwind(AssertUnwindSafe(|| restart(effects, budgets))) {
            Ok(result) => result,
            Err(_) => Err(ServiceFailure::Panicked { stage: Stage::RestartQuery }),
        })
    } else {
        None
    };
    let original = match original {
        Ok(result) => result,
        Err(panic) => {
            if let Some(Err(failure)) = restart_result {
                let _ = catch_unwind(AssertUnwindSafe(|| effects.record_unwind_restart_failure(failure)));
            }
            resume_unwind(panic)
        }
    };
    match restart_result {
        Some(Err(restart)) => Completion::RestartFailed { original, restart },
        Some(Ok(())) | None => Completion::Finished(original),
    }
}
