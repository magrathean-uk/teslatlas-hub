// SPDX-License-Identifier: AGPL-3.0-only

//! `<binary> migrate-names [--complete [--start]]`: the app's start-up step
//! of the rename hand-over (see [`super::hand_over`]). It is read before the
//! command line is parsed, so the operator command tree is unchanged, and
//! prints one JSON report. Exit status: 0 done or nothing to do, 75 deferred
//! (try again), 1 failed. A development source run never touches the
//! installed Hub, so the step is skipped there.
//!
//! `--complete --start` confirms the current service the way pairing and the
//! history import do: the binary's own `serving-check`, run against the
//! configuration the current LaunchAgent names. The same process serving that
//! configuration counts as confirmed when it answers `ready` or
//! `between_windows` (`/readyz` 503 `collector_absent`: no collector holds its
//! lease, which is the state between windows and also the moment inside an
//! open window before the restarted collector takes its lease again). It
//! confirms that the service serves its configuration, not that a window's
//! collector has started again. Every other readiness reason is a fault,
//! reported in the step's detail.
//!
//! Before anything is started (see [`confirm_start`]): a job that is not
//! loaded is loaded from the installed file (it runs at load) and not
//! restarted after; a loaded job that runs another program than the installed
//! file names is replaced by the installed definition; a loaded job that
//! already serves is confirmed without a restart; a loaded job that is not
//! serving is loaded afresh (unloaded, waited for, loaded), never restarted
//! with `kickstart -k`: launchd holds that restart of a job that failed
//! recently until its throttle ends (about 51 s in the lab guest). The whole confirmation, from decoding the
//! installed file on, has one wall-clock limit, [`CONFIRM_LIMIT`]; past it the
//! step is refused with the note and the earlier LaunchAgent file left as they
//! are. The limit bounds the wait, not what a helper already started may
//! still do: nothing is killed or signalled, so a launchd request or check
//! already under way may still complete after it.

use std::process::ExitCode;
#[cfg(any(test, target_os = "macos"))]
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[cfg(any(test, target_os = "macos"))]
use crate::macos_launch_agent::LoadedJob;

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
            confirm_installed(CONFIRM_LIMIT, SystemCurrentService)
        }),
    }
}

/// The installed current job through launchd.
#[cfg(target_os = "macos")]
struct SystemCurrentService;

#[cfg(target_os = "macos")]
impl CurrentService for SystemCurrentService {
    fn identity(&mut self) -> io::Result<(PathBuf, PathBuf)> {
        let config = crate::macos_launch_agent::installed_config_path()?;
        Ok((std::env::current_exe()?, config))
    }

    fn job(&mut self) -> io::Result<LoadedJob> {
        crate::macos_launch_agent::installed_job()
    }

    fn reload(&mut self) -> io::Result<()> {
        crate::macos_launch_agent::reload_installed()
    }

    fn start(&mut self) -> io::Result<()> {
        crate::macos_launch_agent::load_installed()
    }
}

#[cfg(not(target_os = "macos"))]
fn run(_step: Step) -> Report {
    Report::skipped()
}

#[cfg(any(test, target_os = "macos"))]
/// How long `serving-check` may wait for a started service to serve its
/// configuration (a fresh process loads its catalogue and opens its listener
/// first). It does not wait for a window's collector: an absent collector
/// already counts.
pub const SERVING_WAIT_SECONDS: u64 = 45;

#[cfg(any(test, target_os = "macos"))]
/// The short `serving-check` that finds a loaded job already serving, so it is
/// not restarted (three observations 200 ms apart fit in it).
pub const SERVING_PROBE_SECONDS: u64 = 2;

#[cfg(any(test, target_os = "macos"))]
/// The wall-clock limit of the whole confirmation: decoding the installed file,
/// launchd's requests and every `serving-check` child together. Its worst case otherwise is the probe
/// (2 s plus its last request), the start, and the wait (45 s plus its last
/// request of at most 2.5 s).
pub const CONFIRM_LIMIT: Duration = Duration::from_secs(60);

#[cfg(any(test, target_os = "macos"))]
/// The refusal when [`CONFIRM_LIMIT`] passes.
pub const NOT_CONFIRMED_IN_TIME: &str = "the Hub service did not confirm in time that it is serving its configuration";

#[cfg(any(test, target_os = "macos"))]
/// launchd's side of the confirmation, for the installed current job.
pub(super) trait CurrentService {
    /// The Hub binary that runs `serving-check` and the configuration the
    /// installed LaunchAgent file names (decoding that file runs a helper).
    fn identity(&mut self) -> io::Result<(PathBuf, PathBuf)>;
    /// What launchd holds for the job (read only).
    fn job(&mut self) -> io::Result<LoadedJob>;
    /// Loads the installed definition afresh: unloads the loaded job, waits
    /// until it is unloaded, then loads it (it runs at load). For a stale
    /// definition and for a loaded job that is not serving.
    fn reload(&mut self) -> io::Result<()>;
    /// Loads a job that is not loaded (it runs at load); no restart follows.
    fn start(&mut self) -> io::Result<()>;
}

#[cfg(any(test, target_os = "macos"))]
/// The step's confirmation in its real order, all of it inside `limit`: the
/// installed file is decoded, then [`confirm_start`] runs.
pub(super) fn confirm_installed(
    limit: Duration,
    service: impl CurrentService + Send + 'static,
) -> io::Result<()> {
    within(limit, move |given_up| {
        let mut service = service;
        let (program, config) = service.identity()?;
        confirm_start(&mut service, &program, &config, given_up)
    })
}

#[cfg(any(test, target_os = "macos"))]
/// Confirms the current service, loading it afresh only when it is loaded and
/// not serving. A job that is not loaded is loaded and not restarted after; a
/// stale loaded definition is replaced first, so the confirmed service runs the
/// installed binary. Once `given_up` is set no load or reload is
/// begun; one already begun, with its own launchd requests, may still finish.
pub(super) fn confirm_start(
    service: &mut dyn CurrentService,
    program: &Path,
    config: &Path,
    given_up: &AtomicBool,
) -> io::Result<()> {
    let still_wanted = || {
        if given_up.load(Ordering::SeqCst) {
            Err(io::Error::new(io::ErrorKind::TimedOut, NOT_CONFIRMED_IN_TIME))
        } else {
            Ok(())
        }
    };
    match service.job()? {
        LoadedJob::Installed if confirm_serving(program, config, SERVING_PROBE_SECONDS).is_ok() => {
            return Ok(());
        }
        LoadedJob::Stale | LoadedJob::Installed => {
            still_wanted()?;
            service.reload()?;
        }
        LoadedJob::Absent => {
            still_wanted()?;
            service.start()?;
        }
    }
    confirm_serving(program, config, SERVING_WAIT_SECONDS)
}

#[cfg(any(test, target_os = "macos"))]
/// Runs `work` on its own thread and waits for it at most `limit`. Past the
/// limit `given_up` is set and the wait ends with [`NOT_CONFIRMED_IN_TIME`].
/// It bounds the wait only: the thread and any child it waits for are neither
/// killed nor signalled, and what they already started may still take effect.
/// The thread keeps reaping its child while this process lives; a child still
/// running when this process ends runs on to its own end.
pub(super) fn within(
    limit: Duration,
    work: impl FnOnce(&AtomicBool) -> io::Result<()> + Send + 'static,
) -> io::Result<()> {
    let given_up = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&given_up);
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("rename-confirmation".to_owned())
        .spawn(move || {
            let _ = sender.send(work(&flag));
        })?;
    match receiver.recv_timeout(limit) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            given_up.store(true, Ordering::SeqCst);
            Err(io::Error::new(io::ErrorKind::TimedOut, NOT_CONFIRMED_IN_TIME))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("the confirmation ended without an answer"))
        }
    }
}

#[cfg(any(test, target_os = "macos"))]
/// The `serving-check` command line for the configuration `config`.
pub(super) fn serving_check_arguments(config: &Path, wait_seconds: u64) -> Vec<OsString> {
    vec![
        "--config".into(),
        config.as_os_str().to_owned(),
        "serving-check".into(),
        "--wait-seconds".into(),
        wait_seconds.to_string().into(),
    ]
}

#[cfg(any(test, target_os = "macos"))]
/// Runs `serving-check` with `program` (the Hub binary) and judges its answer.
pub(super) fn confirm_serving(program: &Path, config: &Path, wait_seconds: u64) -> io::Result<()> {
    let output = Command::new(program)
        .args(serving_check_arguments(config, wait_seconds))
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
    fn the_serving_check_names_the_agents_configuration_and_fits_in_the_limit() {
        let arguments = serving_check_arguments(Path::new("/h/Tesatlas Hub/config.toml"), SERVING_WAIT_SECONDS);
        let expected: Vec<OsString> = ["--config", "/h/Tesatlas Hub/config.toml", "serving-check", "--wait-seconds", "45"]
            .into_iter()
            .map(OsString::from)
            .collect();
        assert_eq!(arguments, expected);
        // The verb accepts at most 150 s. The probe, the wait and their last
        // requests (2.5 s each) fit inside the whole limit with room for launchd.
        const { assert!(SERVING_WAIT_SECONDS <= 150) };
        assert!(Duration::from_secs(SERVING_PROBE_SECONDS + SERVING_WAIT_SECONDS + 2 * 3) < CONFIRM_LIMIT);
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
