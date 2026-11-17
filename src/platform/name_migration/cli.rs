// SPDX-License-Identifier: AGPL-3.0-only

//! `<binary> migrate-names [--complete [--start]]`: the app's start-up step
//! of the rename hand-over (see [`super::hand_over`]). It is read before the
//! command line is parsed, so the operator command tree is unchanged, and
//! prints one JSON report. Exit status: 0 done or nothing to do, 75 deferred
//! (try again), 1 failed. A development source run never touches the
//! installed Hub, so the step is skipped there.
//!
//! `--complete --start` confirms the started service the way pairing and the
//! history import do: the binary's own `serving-check`, run against the
//! configuration the current LaunchAgent names. The same process serving that
//! configuration counts as started when it answers `ready` or `between_windows`
//! (a configured car outside a collection window); every other readiness
//! reason is a fault, reported in the step's detail.

use std::process::ExitCode;
#[cfg(any(test, target_os = "macos"))]
use std::{
    ffi::OsString,
    io,
    path::Path,
    process::{Command, Stdio},
};

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
            let config = crate::macos_launch_agent::installed_config_path()?;
            crate::macos_launch_agent::start_preflighted_installed()?;
            confirm_serving(&std::env::current_exe()?, &config)
        }),
    }
}

#[cfg(not(target_os = "macos"))]
fn run(_step: Step) -> Report {
    Report::skipped()
}

#[cfg(any(test, target_os = "macos"))]
/// How long `serving-check` may wait for the started service. Inside an open
/// window the start's restart leaves the collector without its lease for about
/// 11 s in the lab; this covers that several times over. Worst case for the
/// owner: the start, then this wait and one last bounded probe, before the
/// app shows its window or its Try Again alert.
pub const SERVING_WAIT_SECONDS: u64 = 45;

#[cfg(any(test, target_os = "macos"))]
/// The `serving-check` command line for the configuration `config`.
pub(super) fn serving_check_arguments(config: &Path) -> Vec<OsString> {
    vec![
        "--config".into(),
        config.as_os_str().to_owned(),
        "serving-check".into(),
        "--wait-seconds".into(),
        SERVING_WAIT_SECONDS.to_string().into(),
    ]
}

#[cfg(any(test, target_os = "macos"))]
/// Runs `serving-check` with `program` (the Hub binary) and judges its answer.
pub(super) fn confirm_serving(program: &Path, config: &Path) -> io::Result<()> {
    let output = Command::new(program)
        .args(serving_check_arguments(config))
        .stdin(Stdio::null())
        .output()?;
    serving_verdict(output.status.success(), &output.stdout, &output.stderr)
}

#[cfg(any(test, target_os = "macos"))]
/// Started when `serving-check` exited 0 with its serving report (`ready` or
/// `between_windows`); otherwise the reason it gave on standard error.
pub(super) fn serving_verdict(success: bool, stdout: &[u8], stderr: &[u8]) -> io::Result<()> {
    #[derive(serde::Deserialize)]
    struct Serving {
        status: String,
        readiness: String,
    }
    let serving = String::from_utf8_lossy(stdout).lines().any(|line| {
        serde_json::from_str::<Serving>(line).is_ok_and(|report| {
            report.status == "serving" && matches!(report.readiness.as_str(), "ready" | "between_windows")
        })
    });
    if success && serving {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(stderr);
    let reason = stderr
        .lines()
        .find_map(|line| line.strip_prefix("tesatlas-hub: "))
        .map(|reason| reason.chars().filter(|c| !c.is_control()).take(240).collect::<String>())
        .filter(|reason| !reason.trim().is_empty())
        .unwrap_or_else(|| "the Hub service did not confirm that it is serving its configuration".to_owned());
    Err(io::Error::other(reason))
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
    fn the_serving_check_names_the_agents_configuration_and_a_wait_that_covers_a_restart() {
        let arguments = serving_check_arguments(Path::new("/h/Tesatlas Hub/config.toml"));
        let expected: Vec<OsString> = ["--config", "/h/Tesatlas Hub/config.toml", "serving-check", "--wait-seconds", "45"]
            .into_iter()
            .map(OsString::from)
            .collect();
        assert_eq!(arguments, expected);
        // The lab's collector took its lease 11 s after the start's restart; the
        // verb accepts at most 150 s.
        const { assert!(SERVING_WAIT_SECONDS >= 3 * 11 && SERVING_WAIT_SECONDS <= 150) };
    }

    #[test]
    fn a_serving_report_between_windows_or_ready_counts_as_started() {
        for readiness in ["ready", "between_windows"] {
            let stdout = format!("an info log line\n{{\"status\":\"serving\",\"readiness\":\"{readiness}\"}}\n");
            assert!(serving_verdict(true, stdout.as_bytes(), b"").is_ok(), "{readiness}");
        }
    }

    #[test]
    fn anything_but_a_serving_report_is_refused_with_the_checks_own_reason() {
        let refused = serving_verdict(
            false,
            b"",
            b"tesatlas-hub: the Hub service is running but cannot serve its history (catalogue_unavailable)\n",
        )
        .unwrap_err();
        assert_eq!(
            refused.to_string(),
            "the Hub service is running but cannot serve its history (catalogue_unavailable)"
        );
        let generic = "the Hub service did not confirm that it is serving its configuration";
        let other = br#"{"status":"serving","readiness":"collector_stale"}"#;
        assert_eq!(serving_verdict(true, other, b"").unwrap_err().to_string(), generic);
        assert_eq!(serving_verdict(true, b"", b"").unwrap_err().to_string(), generic);
        let serving = br#"{"status":"serving","readiness":"ready"}"#;
        assert_eq!(serving_verdict(false, serving, b"").unwrap_err().to_string(), generic, "a failed exit never counts");
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
