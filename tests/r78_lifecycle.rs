// SPDX-License-Identifier: AGPL-3.0-only

#[path = "support/r78_recording.rs"]
mod r78_recording;

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    time::Duration,
};

use teslatlas_hub::platform::r78_lifecycle;

use r78_lifecycle::{
    Completion, PrimaryFailure, ServiceBudgets, ServiceEffects, ServiceFailure, Stage,
    with_transient_pause,
};
use r78_recording::{Operation as Op, Recording, RecordingError, Step};

#[derive(Debug, PartialEq, Eq)]
enum WorkError {
    Cancelled,
    StorageFailed,
}

fn operations(recording: &Recording) -> Vec<Op> {
    recording
        .events()
        .iter()
        .map(|event| event.operation)
        .collect()
}

#[test]
fn work_panic_releases_admission_before_restarting() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout),
        Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit),
        Step::loaded(false),
        Step::done(Op::Bootstrap),
        Step::done(Op::Kickstart),
        Step::done(Op::WaitReady),
    ]);
    let work = effects.work_recorder();
    let panic = catch_unwind(AssertUnwindSafe(|| {
        with_transient_pause(
            &mut effects,
            ServiceBudgets::SCALED_TEST,
            |_| -> Result<(), WorkError> {
                work.work_for(Duration::ZERO);
                panic!("synthetic work panic")
            },
        )
    }))
    .expect_err("the original panic must resume");
    assert_eq!(panic.downcast_ref::<&str>(), Some(&"synthetic work panic"));
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::WaitUnloaded,
            Op::Admit,
            Op::Work,
            Op::Release,
            Op::Loaded,
            Op::Bootstrap,
            Op::Kickstart,
            Op::WaitReady,
        ]
    );
    assert_eq!(effects.remaining(), 0);
    assert!(effects.unwind_restart_failures().is_empty());
}

#[test]
fn work_panic_is_preserved_if_restart_returns_an_error() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout),
        Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit),
        Step::rejected(Op::Loaded),
    ]);
    let panic = catch_unwind(AssertUnwindSafe(|| {
        with_transient_pause(
            &mut effects,
            ServiceBudgets::SCALED_TEST,
            |_| -> Result<(), WorkError> { panic!("original synthetic panic") },
        )
    }))
    .expect_err("the original panic must resume");
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"original synthetic panic")
    );
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::WaitUnloaded,
            Op::Admit,
            Op::Release,
            Op::Loaded
        ]
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn work_panic_records_restart_failure_without_replacing_panic() {
    for (restart_steps, expected) in [
        (
            vec![Step::rejected(Op::Loaded)],
            ServiceFailure::Backend {
                stage: Stage::RestartQuery,
                error: RecordingError::Rejected,
            },
        ),
        (
            vec![
                Step::loaded(true),
                Step::done(Op::Kickstart).after(Duration::from_secs(4)),
            ],
            ServiceFailure::TimedOut {
                stage: Stage::Kickstart,
            },
        ),
    ] {
        let mut steps = vec![
            Step::loaded(true),
            Step::done(Op::Bootout),
            Step::done(Op::WaitUnloaded),
            Step::done(Op::Admit),
        ];
        steps.extend(restart_steps);
        let mut effects = Recording::new(steps);
        let panic = catch_unwind(AssertUnwindSafe(|| {
            with_transient_pause(
                &mut effects,
                ServiceBudgets::SCALED_TEST,
                |_| -> Result<(), WorkError> { panic!("original synthetic panic") },
            )
        }))
        .expect_err("the original panic must resume");

        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"original synthetic panic")
        );
        assert_eq!(effects.unwind_restart_failures(), &[expected]);
        assert_eq!(operations(&effects)[4], Op::Release);
        assert_eq!(effects.remaining(), 0);
    }

    let mut effects = Recording::new([
        Step::loaded(true), Step::done(Op::Bootout), Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit), Step::rejected(Op::Loaded),
    ]);
    effects.panic_on_unwind_recording();
    let panic = catch_unwind(AssertUnwindSafe(|| {
        with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST,
            |_| -> Result<(), WorkError> { panic!("original scope panic") })
    })).expect_err("the original scope panic survives a recording hook panic");
    assert_eq!(panic.downcast_ref::<&str>(), Some(&"original scope panic"));
    assert!(matches!(effects.unwind_restart_failures(), [ServiceFailure::Backend {
        stage: Stage::RestartQuery, error: RecordingError::Rejected,
    }]));
    assert_eq!(operations(&effects)[4], Op::Release);
    assert_eq!(effects.remaining(), 0);
}

// The additional case remains inside the frozen eighteen-test selection.
#[test]
fn work_panic_without_restart_duty_only_releases_admission() {
    let mut effects = Recording::new([Step::loaded(false), Step::done(Op::Admit)]);
    let panic = catch_unwind(AssertUnwindSafe(|| {
        with_transient_pause(
            &mut effects,
            ServiceBudgets::SCALED_TEST,
            |_| -> Result<(), WorkError> { panic!("unloaded synthetic panic") },
        )
    }))
    .expect_err("the original panic must resume");
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"unloaded synthetic panic")
    );
    assert_eq!(operations(&effects), [Op::Loaded, Op::Admit, Op::Release]);
    assert_eq!(effects.remaining(), 0);
    assert!(effects.unwind_restart_failures().is_empty());

    let mut effects = Recording::new([
        Step::loaded(true), Step::done(Op::Bootout), Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit), Step::loaded(false), Step::done(Op::Bootstrap),
        Step::panicked(Op::Kickstart),
    ]);
    let panic = catch_unwind(AssertUnwindSafe(|| {
        with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST,
            |_| -> Result<(), WorkError> { panic!("original scope panic") })
    })).expect_err("the original scope panic survives a restart panic");
    assert_eq!(panic.downcast_ref::<&str>(), Some(&"original scope panic"));
    assert_eq!(effects.unwind_restart_failures().len(), 1);
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn loaded_service_is_paused_then_reloaded_after_admission_release() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout),
        Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit),
        Step::loaded(false),
        Step::done(Op::Bootstrap),
        Step::done(Op::Kickstart),
        Step::done(Op::WaitReady),
    ]);
    let start = effects.now();
    let work = effects.work_recorder();
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        work.work_for(Duration::from_secs(1));
        Ok::<_, WorkError>(42)
    });
    assert_eq!(result, Completion::Finished(Ok(42)));
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::WaitUnloaded,
            Op::Admit,
            Op::Work,
            Op::Release,
            Op::Loaded,
            Op::Bootstrap,
            Op::Kickstart,
            Op::WaitReady,
        ]
    );
    let events = effects.events();
    assert_eq!(events[0].deadline, Some(start + Duration::from_secs(3)));
    assert_eq!(events[2].deadline, Some(start + Duration::from_secs(30)));
    assert_eq!(events[6].deadline, Some(start + Duration::from_secs(4)));
    assert_eq!(
        effects.readiness(),
        Some(ServiceBudgets::SCALED_TEST.readiness)
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn still_loaded_service_is_kickstarted_without_bootstrap() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout),
        Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit),
        Step::loaded(true),
        Step::done(Op::Kickstart),
        Step::done(Op::WaitReady),
    ]);
    let work = effects.work_recorder();
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        work.work_for(Duration::ZERO);
        Err::<(), _>(WorkError::StorageFailed)
    });
    assert_eq!(
        result,
        Completion::Finished(Err(PrimaryFailure::Work(WorkError::StorageFailed)))
    );
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::WaitUnloaded,
            Op::Admit,
            Op::Work,
            Op::Release,
            Op::Loaded,
            Op::Kickstart,
            Op::WaitReady,
        ]
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn initially_unloaded_service_does_not_acquire_restart_duty() {
    let mut effects = Recording::new([Step::loaded(false), Step::done(Op::Admit)]);
    let work = effects.work_recorder();
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        work.work_for(Duration::ZERO);
        Ok::<_, WorkError>(())
    });
    assert_eq!(result, Completion::Finished(Ok(())));
    assert_eq!(
        operations(&effects),
        [Op::Loaded, Op::Admit, Op::Work, Op::Release]
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn bootout_failure_keeps_restart_duty_and_original_failure() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::rejected(Op::Bootout),
        Step::loaded(true),
        Step::done(Op::Kickstart),
        Step::done(Op::WaitReady),
    ]);
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        Ok::<_, WorkError>(())
    });
    assert_eq!(
        result,
        Completion::Finished(Err(PrimaryFailure::Service(ServiceFailure::Backend {
            stage: Stage::Bootout,
            error: RecordingError::Rejected
        })))
    );
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::Loaded,
            Op::Kickstart,
            Op::WaitReady,
        ]
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn unknown_unload_never_admits_work_but_still_restarts() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout),
        Step::rejected(Op::WaitUnloaded),
        Step::loaded(false),
        Step::done(Op::Bootstrap),
        Step::done(Op::Kickstart),
        Step::done(Op::WaitReady),
    ]);
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        Ok::<_, WorkError>(())
    });
    assert_eq!(
        result,
        Completion::Finished(Err(PrimaryFailure::Service(ServiceFailure::Backend {
            stage: Stage::Unload,
            error: RecordingError::Rejected
        })))
    );
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::WaitUnloaded,
            Op::Loaded,
            Op::Bootstrap,
            Op::Kickstart,
            Op::WaitReady,
        ]
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn unknown_initial_status_does_not_start_or_assume_a_stop() {
    let mut effects = Recording::new([Step::rejected(Op::Loaded)]);
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        Ok::<_, WorkError>(())
    });
    assert_eq!(
        result,
        Completion::Finished(Err(PrimaryFailure::Service(ServiceFailure::Backend {
            stage: Stage::InitialQuery,
            error: RecordingError::Rejected
        })))
    );
    assert_eq!(operations(&effects), [Op::Loaded]);
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn unknown_restart_status_preserves_cancelled_result_without_guessing() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout),
        Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit),
        Step::rejected(Op::Loaded),
    ]);
    let work = effects.work_recorder();
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        work.work_for(Duration::ZERO);
        Err::<(), _>(WorkError::Cancelled)
    });
    assert_eq!(
        result,
        Completion::RestartFailed {
            original: Err(PrimaryFailure::Work(WorkError::Cancelled)),
            restart: ServiceFailure::Backend {
                stage: Stage::RestartQuery,
                error: RecordingError::Rejected
            },
        }
    );
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::WaitUnloaded,
            Op::Admit,
            Op::Work,
            Op::Release,
            Op::Loaded,
        ]
    );
    assert_eq!(effects.remaining(), 0);
    assert!(effects.unwind_restart_failures().is_empty());
}

#[test]
fn storage_has_no_service_deadline_and_restart_gets_its_own_budget() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout),
        Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit),
        Step::loaded(true),
        Step::done(Op::Kickstart),
        Step::done(Op::WaitReady),
    ]);
    let start = effects.now();
    let work = effects.work_recorder();
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        work.work_for(Duration::from_secs(24 * 60 * 60));
        Err::<(), _>(WorkError::Cancelled)
    });
    assert_eq!(
        result,
        Completion::Finished(Err(PrimaryFailure::Work(WorkError::Cancelled)))
    );
    let events = effects.events();
    assert_eq!(events[5].operation, Op::Release);
    assert_eq!(events[6].operation, Op::Loaded);
    assert_eq!(
        events[6].deadline,
        Some(start + Duration::from_secs(24 * 60 * 60 + 3))
    );
    assert_eq!(
        events[8].deadline,
        Some(start + Duration::from_secs(24 * 60 * 60 + 15))
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn command_deadline_failure_still_restarts_initially_loaded_service() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout).after(Duration::from_secs(4)),
        Step::loaded(true),
        Step::done(Op::Kickstart),
        Step::done(Op::WaitReady),
    ]);
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        Ok::<_, WorkError>(())
    });
    assert_eq!(
        result,
        Completion::Finished(Err(PrimaryFailure::Service(ServiceFailure::TimedOut {
            stage: Stage::Bootout
        })))
    );
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::Loaded,
            Op::Kickstart,
            Op::WaitReady
        ]
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn timed_out_admission_is_released_before_restart() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout),
        Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit).after(Duration::from_secs(4)),
        Step::loaded(true),
        Step::done(Op::Kickstart),
        Step::done(Op::WaitReady),
    ]);
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        Ok::<_, WorkError>(())
    });
    assert_eq!(
        result,
        Completion::Finished(Err(PrimaryFailure::Service(ServiceFailure::TimedOut {
            stage: Stage::Admission
        })))
    );
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::WaitUnloaded,
            Op::Admit,
            Op::Release,
            Op::Loaded,
            Op::Kickstart,
            Op::WaitReady,
        ]
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn production_and_scaled_budgets_keep_the_frozen_values() {
    let production = ServiceBudgets::PRODUCTION;
    let scaled = ServiceBudgets::SCALED_TEST;
    assert_eq!(
        [
            production.command.as_secs(),
            production.unload.as_secs(),
            production.admission.as_secs(),
            production.stop.as_secs(),
            production.restart.as_secs(),
            production.total_service.as_secs()
        ],
        [30, 300, 30, 390, 150, 540]
    );
    assert_eq!(
        [
            scaled.command.as_secs(),
            scaled.unload.as_secs(),
            scaled.admission.as_secs(),
            scaled.stop.as_secs(),
            scaled.restart.as_secs(),
            scaled.total_service.as_secs()
        ],
        [3, 30, 3, 39, 15, 54]
    );
    assert_eq!(production.readiness.poll, Duration::from_millis(100));
    assert_eq!(production.readiness.connect, Duration::from_millis(100));
    assert_eq!(production.readiness.request, Duration::from_millis(500));
    assert_eq!(scaled.readiness.poll, Duration::from_millis(10));
    assert_eq!(scaled.readiness.connect, Duration::from_millis(10));
    assert_eq!(scaled.readiness.request, Duration::from_millis(50));
    assert_eq!(production.readiness.stable_observations, 10);
    assert_eq!(scaled.readiness.stable_observations, 10);
}

#[test]
fn unscripted_recording_query_fails_closed_without_a_fallback() {
    let mut effects = Recording::new([]);
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        Ok::<_, WorkError>(())
    });
    assert_eq!(
        result,
        Completion::Finished(Err(PrimaryFailure::Service(ServiceFailure::Backend {
            stage: Stage::InitialQuery,
            error: RecordingError::UnexpectedStep,
        })))
    );
    assert_eq!(operations(&effects), [Op::Loaded]);
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn stop_share_caps_unload_without_cancelling_restart_duty() {
    let mut budgets = ServiceBudgets::SCALED_TEST;
    budgets.stop = Duration::from_secs(4);
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout).after(Duration::from_secs(3)),
        Step::done(Op::WaitUnloaded).after(Duration::from_secs(2)),
        Step::loaded(true),
        Step::done(Op::Kickstart),
        Step::done(Op::WaitReady),
    ]);
    let start = effects.now();
    let result = with_transient_pause(&mut effects, budgets, |_| Ok::<_, WorkError>(()));
    assert_eq!(
        result,
        Completion::Finished(Err(PrimaryFailure::Service(ServiceFailure::TimedOut {
            stage: Stage::Unload,
        })))
    );
    assert_eq!(
        effects.events()[2].deadline,
        Some(start + Duration::from_secs(4))
    );
    assert_eq!(
        operations(&effects),
        [
            Op::Loaded,
            Op::Bootout,
            Op::WaitUnloaded,
            Op::Loaded,
            Op::Kickstart,
            Op::WaitReady
        ]
    );
    assert_eq!(effects.remaining(), 0);
}

#[test]
fn restart_share_caps_readiness_and_preserves_the_work_result() {
    let mut effects = Recording::new([
        Step::loaded(true),
        Step::done(Op::Bootout),
        Step::done(Op::WaitUnloaded),
        Step::done(Op::Admit),
        Step::loaded(false).after(Duration::from_secs(3)),
        Step::done(Op::Bootstrap).after(Duration::from_secs(3)),
        Step::done(Op::Kickstart).after(Duration::from_secs(3)),
        Step::done(Op::WaitReady).after(Duration::from_secs(7)),
    ]);
    let start = effects.now();
    let work = effects.work_recorder();
    let result = with_transient_pause(&mut effects, ServiceBudgets::SCALED_TEST, |_| {
        work.work_for(Duration::ZERO);
        Ok::<_, WorkError>(42)
    });
    assert_eq!(
        result,
        Completion::RestartFailed {
            original: Ok(42),
            restart: ServiceFailure::TimedOut {
                stage: Stage::Readiness
            },
        }
    );
    let events = effects.events();
    assert_eq!(events[5].operation, Op::Release);
    assert_eq!(events[9].deadline, Some(start + Duration::from_secs(15)));
    assert_eq!(effects.remaining(), 0);
}
