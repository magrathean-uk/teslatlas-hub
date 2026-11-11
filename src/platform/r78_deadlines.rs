// SPDX-License-Identifier: AGPL-3.0-only

// Included in the retained service module so the ordinary runner's absence
// classifier, PID parser and configuration identity checks remain shared.
use super::r78_lifecycle::{ReadinessBounds, ServiceBudgets};
use std::time::Instant;

fn service_timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "service deadline")
}
fn remaining_service_time(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(service_timeout())
    } else {
        Ok(remaining)
    }
}
fn service_poll(deadline: Instant, poll: Duration) -> io::Result<()> {
    std::thread::sleep(poll.min(remaining_service_time(deadline)?));
    Ok(())
}

#[cfg(not(feature = "credential-relocation-fixture"))]
fn launchctl_until(
    arguments: &[&OsStr],
    deadline: Instant,
    budgets: ServiceBudgets,
) -> io::Result<std::process::Output> {
    // In tests and in the fixture this returns before command construction.
    refuse_real_launchctl_in_unit_tests()?;
    let limit = (Instant::now() + budgets.command).min(deadline);
    super::r78_service::run_helper_until(Path::new("/bin/launchctl"), arguments, limit)
}

#[cfg(not(feature = "credential-relocation-fixture"))]
fn launchctl_output_is_present(action: &str, output: &std::process::Output) -> io::Result<bool> {
    if output.status.success() {
        return Ok(true);
    }
    let detail = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if expected_launchctl_absence(action, output.status.code(), &detail) {
        return Ok(false);
    }
    Err(io::Error::other("service helper failed"))
}

pub fn service_is_loaded_until(deadline: Instant, budgets: ServiceBudgets) -> io::Result<bool> {
    #[cfg(feature = "credential-relocation-fixture")]
    {
        remaining_service_time(deadline)?;
        let _ = budgets;
        super::r78_recording::with_fixture_backend(|backend| backend.loaded_query())
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
        let (_, service) = service_identifiers();
        let output = launchctl_until(
            &[OsStr::new("print"), OsStr::new(&service)],
            deadline,
            budgets,
        )?;
        launchctl_output_is_present("print", &output)
    }
}

pub fn transient_bootout_until(deadline: Instant, budgets: ServiceBudgets) -> io::Result<()> {
    #[cfg(feature = "credential-relocation-fixture")]
    {
        let _ = budgets;
        super::r78_recording::with_fixture_backend(|backend| {
            super::r78_lifecycle::ServiceEffects::bootout(backend, deadline)
        })
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
        let (_, service) = service_identifiers();
        let output = launchctl_until(
            &[OsStr::new("bootout"), OsStr::new(&service)],
            deadline,
            budgets,
        )?;
        // The absence answer is a successful transient stop.
        launchctl_output_is_present("bootout", &output).map(|_| ())
    }
}

pub fn wait_for_service_unloaded_until(
    deadline: Instant,
    budgets: ServiceBudgets,
) -> io::Result<()> {
    #[cfg(feature = "credential-relocation-fixture")]
    {
        let _ = budgets;
        super::r78_recording::with_fixture_backend(|backend| {
            super::r78_lifecycle::ServiceEffects::wait_unloaded(backend, deadline)
        })
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
        loop {
            if !service_is_loaded_until(deadline, budgets)? {
                return Ok(());
            }
            service_poll(deadline, budgets.readiness.poll)?;
        }
    }
}

pub fn transient_pause_until(deadline: Instant, budgets: ServiceBudgets) -> io::Result<()> {
    transient_bootout_until(deadline, budgets)?;
    wait_for_service_unloaded_until(deadline, budgets)
}

pub fn admit_after_service_stop_until(
    data_dir: &Path,
    deadline: Instant,
    budgets: ServiceBudgets,
) -> io::Result<std::sync::Arc<crate::hub_user_process::AdmittedUserHub>> {
    loop {
        remaining_service_time(deadline)?;
        match crate::hub_user_process::AdmittedUserHub::admit(data_dir) {
            Ok(admission) => return Ok(admission),
            Err(crate::hub_user_process::UserLifetimeLockError::AlreadyRunning) => {
                service_poll(deadline, budgets.readiness.poll)?
            }
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

pub fn bootstrap_installed_until(deadline: Instant, budgets: ServiceBudgets) -> io::Result<()> {
    #[cfg(feature = "credential-relocation-fixture")]
    {
        let _ = budgets;
        super::r78_recording::with_fixture_backend(|backend| {
            super::r78_lifecycle::ServiceEffects::bootstrap(backend, deadline)
        })
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
        let plist = installed_plist()?;
        let (domain, _) = service_identifiers();
        let output = launchctl_until(
            &[
                OsStr::new("bootstrap"),
                OsStr::new(&domain),
                plist.as_os_str(),
            ],
            deadline,
            budgets,
        )?;
        if launchctl_output_is_present("bootstrap", &output)? {
            Ok(())
        } else {
            Err(io::Error::other("service bootstrap failed"))
        }
    }
}

pub fn kickstart_installed_until(deadline: Instant, budgets: ServiceBudgets) -> io::Result<()> {
    #[cfg(feature = "credential-relocation-fixture")]
    {
        let _ = budgets;
        super::r78_recording::with_fixture_backend(|backend| {
            super::r78_lifecycle::ServiceEffects::kickstart(backend, deadline)
        })
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
        let (_, service) = service_identifiers();
        let output = launchctl_until(
            &[
                OsStr::new("kickstart"),
                OsStr::new("-k"),
                OsStr::new(&service),
            ],
            deadline,
            budgets,
        )?;
        if launchctl_output_is_present("kickstart", &output)? {
            Ok(())
        } else {
            Err(io::Error::other("service kickstart failed"))
        }
    }
}

pub fn start_preflighted_installed_until(
    deadline: Instant,
    budgets: ServiceBudgets,
) -> io::Result<()> {
    if !service_is_loaded_until(deadline, budgets)? {
        bootstrap_installed_until(deadline, budgets)?;
    }
    kickstart_installed_until(deadline, budgets)
}

#[cfg(not(feature = "credential-relocation-fixture"))]
fn configured_identity_from_plist_until(
    plist: &Path,
    deadline: Instant,
    budgets: ServiceBudgets,
) -> io::Result<(crate::config::HubConfig, crate::protocol::Sha256Digest)> {
    refuse_real_launchctl_in_unit_tests()?;
    if fs::metadata(plist)?.len() > 1024 * 1024 {
        return Err(io::Error::other("installed plist is too large"));
    }
    let output = super::r78_service::run_helper_until(
        Path::new("/usr/bin/plutil"),
        &[
            OsStr::new("-convert"),
            OsStr::new("json"),
            OsStr::new("-o"),
            OsStr::new("-"),
            OsStr::new("--"),
            plist.as_os_str(),
        ],
        deadline.min(Instant::now() + budgets.command),
    )?;
    if !output.status.success() {
        return Err(io::Error::other("installed plist could not be decoded"));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
    let args = value
        .get("ProgramArguments")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| io::Error::other("installed plist has no program arguments"))?;
    if args.len() != 4 || args[1].as_str() != Some("--config") || args[3].as_str() != Some("serve")
    {
        return Err(io::Error::other(
            "installed plist has unsupported program arguments",
        ));
    }
    let path = Path::new(
        args[2]
            .as_str()
            .ok_or_else(|| io::Error::other("installed plist config path is absent"))?,
    );
    if !path.is_absolute() {
        return Err(io::Error::other(
            "installed plist config path must be absolute",
        ));
    }
    remaining_service_time(deadline)?;
    crate::config::HubConfig::load_with_digest(path).map_err(io::Error::other)
}

#[cfg(not(feature = "credential-relocation-fixture"))]
fn service_running_pid_until(
    service: &str,
    deadline: Instant,
    budgets: ServiceBudgets,
) -> io::Result<Option<u32>> {
    let output = launchctl_until(
        &[OsStr::new("print"), OsStr::new(service)],
        deadline,
        budgets,
    )?;
    if !launchctl_output_is_present("print", &output)? {
        return Ok(None);
    }
    Ok(launchctl_print_running_pid(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn stable_readiness_until(
    deadline: Instant,
    bounds: ReadinessBounds,
    mut observe: impl FnMut() -> io::Result<Option<u32>>,
) -> io::Result<()> {
    let mut pid = None;
    let mut consecutive = 0;
    loop {
        remaining_service_time(deadline)?;
        match observe()? {
            Some(current) => {
                consecutive = if pid == Some(current) {
                    consecutive + 1
                } else {
                    1
                };
                pid = Some(current);
            }
            None => {
                consecutive = 0;
                pid = None;
            }
        }
        remaining_service_time(deadline)?;
        if consecutive >= bounds.stable_observations {
            return Ok(());
        }
        service_poll(deadline, bounds.poll)?;
    }
}

pub fn wait_for_current_installed_readiness_until(
    deadline: Instant,
    budgets: ServiceBudgets,
    bounds: ReadinessBounds,
) -> io::Result<()> {
    #[cfg(feature = "credential-relocation-fixture")]
    {
        let _ = budgets;
        super::r78_recording::with_fixture_backend(|backend| {
            super::r78_lifecycle::ServiceEffects::wait_ready(backend, deadline, bounds)
        })
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
        let (config, digest) =
            configured_identity_from_plist_until(&installed_plist()?, deadline, budgets)?;
        let (_, service) = service_identifiers();
        stable_readiness_until(deadline, bounds, || {
            let Some(pid) = service_running_pid_until(&service, deadline, budgets)? else {
                return Ok(None);
            };
            match crate::hub_user_process::AdmittedUserHub::admit(&config.data_dir) {
                Err(crate::hub_user_process::UserLifetimeLockError::AlreadyRunning) => {}
                Ok(admission) => {
                    drop(admission);
                    return Ok(None);
                }
                Err(error) => return Err(io::Error::other(error)),
            }
            configured_application_is_ready_with_bounds(&config, digest, pid, bounds, deadline)
                .map(|ready| ready.then_some(pid))
        })
    }
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn readiness_loop_for_test(
    deadline: Instant,
    bounds: ReadinessBounds,
    observe: impl FnMut() -> io::Result<Option<u32>>,
) -> io::Result<()> {
    stable_readiness_until(deadline, bounds, observe)
}
