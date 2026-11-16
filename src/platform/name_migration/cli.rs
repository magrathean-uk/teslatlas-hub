// SPDX-License-Identifier: AGPL-3.0-only

//! `<binary> migrate-names [--complete [--start]]`: the app's start-up step
//! of the rename hand-over (see [`super::hand_over`]). It is read before the
//! command line is parsed, so the operator command tree is unchanged, and
//! prints one JSON report. Exit status: 0 done or nothing to do, 75 deferred
//! (try again), 1 failed. A development source run never touches the
//! installed Hub, so the step is skipped there.

use std::process::ExitCode;

use super::hand_over::{self, Layout, Report, Status};

/// The verb the app runs.
pub const VERB: &str = "migrate-names";

/// The step's exit status when it was deferred.
pub const DEFERRED_EXIT: u8 = 75;

/// Runs the step when the command line asks for it; `None` otherwise.
pub fn run_if_requested() -> Option<ExitCode> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) != Some(VERB) {
        return None;
    }
    let report = match parse(&arguments[1..]) {
        Some(step) => run(step),
        None => Report::failed_with("usage: migrate-names [--complete [--start]]"),
    };
    println!("{}", serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_owned()));
    Some(ExitCode::from(exit_status(report.status)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Prepare,
    Complete { start: bool },
}

fn parse(arguments: &[String]) -> Option<Step> {
    match arguments.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => Some(Step::Prepare),
        ["--complete"] => Some(Step::Complete { start: false }),
        ["--complete", "--start"] => Some(Step::Complete { start: true }),
        _ => None,
    }
}

fn exit_status(status: Status) -> u8 {
    match status {
        Status::Inert | Status::Nothing | Status::Prepared | Status::Done => 0,
        Status::Deferred => DEFERRED_EXIT,
        Status::Failed => 1,
    }
}

#[cfg(target_os = "macos")]
fn run(step: Step) -> Report {
    let development = super::env::var_os(crate::macos_launch_agent::DEVELOPMENT_SERVE_ENV);
    if development.as_deref() == Some(std::ffi::OsStr::new("1")) {
        return Report::skipped();
    }
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return Report::failed_with("HOME is not set");
    };
    let layout = Layout::production(home, format!("gui/{}", rustix::process::geteuid().as_raw()));
    match step {
        Step::Prepare => hand_over::prepare(&layout, &mut hand_over::SystemLaunchd),
        Step::Complete { start } => hand_over::complete(&layout, start, || {
            crate::macos_launch_agent::start_preflighted_installed()?;
            crate::macos_launch_agent::wait_for_current_installed_readiness()
        }),
    }
}

#[cfg(not(target_os = "macos"))]
fn run(_step: Step) -> Report {
    Report::skipped()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn the_step_takes_only_its_own_arguments() {
        assert_eq!(parse(&[]), Some(Step::Prepare));
        assert_eq!(parse(&strings(&["--complete"])), Some(Step::Complete { start: false }));
        assert_eq!(parse(&strings(&["--complete", "--start"])), Some(Step::Complete { start: true }));
        assert_eq!(parse(&strings(&["--start"])), None);
        assert_eq!(parse(&strings(&["--complete", "--start", "x"])), None);
    }

    #[test]
    fn only_a_deferral_and_a_failure_are_non_zero() {
        assert_eq!(exit_status(Status::Inert), 0);
        assert_eq!(exit_status(Status::Prepared), 0);
        assert_eq!(exit_status(Status::Done), 0);
        assert_eq!(exit_status(Status::Deferred), DEFERRED_EXIT);
        assert_eq!(exit_status(Status::Failed), 1);
    }
}
