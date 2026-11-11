// SPDX-License-Identifier: AGPL-3.0-only

//! Installed-service effects for the sign-in scope. The scope owns all
//! admission-dependent resources; this backend retains none of them.

use super::{
    hub_user_process::AdmittedUserHub,
    r78_lifecycle::{ReadinessBounds, ServiceBudgets, ServiceEffects, ServiceFailure},
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

pub struct InstalledServiceEffects {
    data_dir: PathBuf,
    budgets: ServiceBudgets,
    observed_loaded: Option<bool>,
    unwind_restart_failure: Option<ServiceFailure<io::Error>>,
}

impl InstalledServiceEffects {
    pub fn new(data_dir: &Path, budgets: ServiceBudgets) -> Self {
        Self {
            data_dir: data_dir.to_owned(),
            budgets,
            observed_loaded: None,
            unwind_restart_failure: None,
        }
    }

    pub fn take_unwind_restart_failure(&mut self) -> Option<ServiceFailure<io::Error>> {
        self.unwind_restart_failure.take()
    }
}

impl ServiceEffects for InstalledServiceEffects {
    type Admission = Arc<AdmittedUserHub>;
    type Error = io::Error;

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn loaded(&mut self, deadline: Instant) -> io::Result<bool> {
        #[cfg(feature = "credential-relocation-fixture")]
        let loaded = {
            require_time(deadline.min(Instant::now() + self.budgets.command))?;
            super::r78_recording::with_fixture_backend(|backend| backend.loaded_query())?
        };
        #[cfg(all(target_os = "macos", not(feature = "credential-relocation-fixture")))]
        let loaded = super::macos_launch_agent::service_is_loaded_until(deadline, self.budgets)?;
        #[cfg(all(
            not(target_os = "macos"),
            not(feature = "credential-relocation-fixture")
        ))]
        let loaded = {
            require_time(deadline.min(Instant::now() + self.budgets.command))?;
            false
        };
        if self.observed_loaded.is_none() {
            self.observed_loaded = Some(loaded);
        }
        Ok(loaded)
    }

    fn bootout(&mut self, deadline: Instant) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            super::macos_launch_agent::transient_bootout_until(deadline, self.budgets)
        }
        #[cfg(not(target_os = "macos"))]
        {
            require_time(deadline)?;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "installed service unavailable",
            ))
        }
    }

    fn wait_unloaded(&mut self, deadline: Instant) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            super::macos_launch_agent::wait_for_service_unloaded_until(deadline, self.budgets)
        }
        #[cfg(not(target_os = "macos"))]
        {
            require_time(deadline)
        }
    }

    fn admit(&mut self, deadline: Instant) -> io::Result<Self::Admission> {
        require_time(deadline)?;
        // An absent job creates no restart duty and permits one attempt only.
        if self.observed_loaded != Some(true) {
            return AdmittedUserHub::admit(&self.data_dir).map_err(io::Error::other);
        }
        #[cfg(target_os = "macos")]
        {
            super::macos_launch_agent::admit_after_service_stop_until(
                &self.data_dir,
                deadline,
                self.budgets,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            AdmittedUserHub::admit(&self.data_dir).map_err(io::Error::other)
        }
    }

    fn bootstrap(&mut self, deadline: Instant) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            super::macos_launch_agent::bootstrap_installed_until(deadline, self.budgets)
        }
        #[cfg(not(target_os = "macos"))]
        {
            require_time(deadline)?;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "installed service unavailable",
            ))
        }
    }

    fn kickstart(&mut self, deadline: Instant) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            super::macos_launch_agent::kickstart_installed_until(deadline, self.budgets)
        }
        #[cfg(not(target_os = "macos"))]
        {
            require_time(deadline)?;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "installed service unavailable",
            ))
        }
    }

    fn wait_ready(&mut self, deadline: Instant, bounds: ReadinessBounds) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            super::macos_launch_agent::wait_for_current_installed_readiness_until(
                deadline,
                self.budgets,
                bounds,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            require_time(deadline)?;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "installed service unavailable",
            ))
        }
    }

    fn record_unwind_restart_failure(&mut self, failure: ServiceFailure<Self::Error>) {
        self.unwind_restart_failure = Some(failure);
    }
}

pub(crate) fn require_time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::Error::new(io::ErrorKind::TimedOut, "service deadline"))
    } else {
        Ok(())
    }
}

/// A local helper has no pipe which a hung process can fill. Output is bounded
/// when read, and every timeout kills and waits for the exact child we spawned.
pub(crate) fn run_helper_until(
    program: &Path,
    arguments: &[&std::ffi::OsStr],
    deadline: Instant,
) -> io::Result<std::process::Output> {
    #[cfg(feature = "credential-relocation-fixture")]
    {
        let _ = (program, arguments, deadline);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "fixture_child_refused",
        ))
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
        #[cfg(debug_assertions)]
        if let Some(result) = test_helper_result(program, arguments, deadline) {
            return result;
        }
        run_spawned_helper_until(program, arguments, deadline)
    }
}

#[cfg(not(feature = "credential-relocation-fixture"))]
fn run_spawned_helper_until(
    program: &Path,
    arguments: &[&std::ffi::OsStr],
    deadline: Instant,
) -> io::Result<std::process::Output> {
    {
        use std::{
            fs::File,
            io::{Read, Seek, SeekFrom},
            process::{Command, Stdio},
            time::Duration,
        };
        require_time(deadline)?;
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn()?;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
            if Instant::now() >= deadline {
                let kill = child.kill();
                let reap = child.wait();
                if let Err(error) = reap {
                    return Err(error);
                }
                if let Err(error) = kill {
                    if error.kind() != io::ErrorKind::InvalidInput {
                        return Err(error);
                    }
                }
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "service helper deadline",
                ));
            }
            std::thread::sleep(
                Duration::from_millis(2).min(deadline.saturating_duration_since(Instant::now())),
            );
        };
        fn read_output(file: &mut File) -> io::Result<Vec<u8>> {
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 1024 * 1024 {
                return Err(io::Error::other("service helper output cap"));
            }
            Ok(bytes)
        }
        require_time(deadline)?;
        Ok(std::process::Output {
            status,
            stdout: read_output(&mut stdout)?,
            stderr: read_output(&mut stderr)?,
        })
    }
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn sleeping_helper_for_test(deadline: Instant) -> io::Result<()> {
    // CT100(q) runs this from inside a test hook, which holds the thread's hook slot,
    // so the hung helper is spawned directly rather than routed through the hook again.
    #[cfg(feature = "credential-relocation-fixture")]
    {
        let _ = deadline;
        Err(io::Error::new(io::ErrorKind::Unsupported, "fixture_child_refused"))
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    {
        run_spawned_helper_until(
            Path::new("/bin/sleep"),
            &[std::ffi::OsStr::new("30")],
            deadline,
        )
        .map(|_| ())
    }
}

/// A synthetic child service performs one non-blocking admission and one vault
/// open, validates its expected pair, then retains only the lifetime admission.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn stub_admit_and_load_for_test(
    data_dir: &Path,
    expected_refresh: Option<&str>,
) -> io::Result<Arc<AdmittedUserHub>> {
    stub_admit_and_load_slot_for_test(data_dir, expected_refresh, false)
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn stub_admit_and_load_slot_for_test(
    data_dir: &Path,
    expected_refresh: Option<&str>,
    fleet: bool,
) -> io::Result<Arc<AdmittedUserHub>> {
    use crate::auth::{
        credential_envelope::CredentialSlot,
        credential_keys::{BackupExclusion, CredentialKeyError, ExclusionState},
        credential_vault::{CredentialVault, VaultOptions},
    };
    struct Excluded;
    impl BackupExclusion for Excluded {
        fn ensure_excluded(&self, _: &Path) -> Result<ExclusionState, CredentialKeyError> {
            Ok(ExclusionState::Verified)
        }
    }
    let admission = AdmittedUserHub::admit(data_dir).map_err(io::Error::other)?;
    let store = crate::db::HubStore::initialize(data_dir).map_err(io::Error::other)?;
    let vault = CredentialVault::open(
        store.clone(),
        Arc::clone(&admission),
        data_dir,
        VaultOptions {
            exclusion: Some(Arc::new(Excluded)),
            ..VaultOptions::default()
        },
    )
    .map_err(crate::auth::credential_sign_in::CredentialCommandError::from)
    .map_err(|error| io::Error::other(error.code()))?;
    if let Some(expected) = expected_refresh {
        let pair = vault
            .load(if fleet { CredentialSlot::FleetApi } else { CredentialSlot::OwnerApi })
            .map_err(crate::auth::credential_sign_in::CredentialCommandError::from)
            .map_err(|error| io::Error::other(error.code()))?;
        if !pair.is_some_and(|pair| pair.refresh.as_str() == expected) {
            return Err(io::Error::other("stub_pair_mismatch"));
        }
    }
    drop(vault);
    drop(store);
    Ok(admission)
}

#[cfg(all(debug_assertions, not(feature = "credential-relocation-fixture")))]
type TestRunner =
    Box<dyn FnMut(&Path, &[&std::ffi::OsStr], Instant) -> io::Result<std::process::Output>>;

#[cfg(all(debug_assertions, not(feature = "credential-relocation-fixture")))]
struct ServiceTestHooks {
    plist: PathBuf,
    domain: String,
    service: String,
    runner: TestRunner,
}

#[cfg(all(debug_assertions, not(feature = "credential-relocation-fixture")))]
thread_local! {
    static SERVICE_TEST_HOOKS: std::cell::RefCell<Option<ServiceTestHooks>> = const { std::cell::RefCell::new(None) };
}

/// A unit-test runner can be selected only explicitly, with a synthetic job.
/// The fixture feature does not compile this bridge or its executor bypass.
#[cfg(all(debug_assertions, not(feature = "credential-relocation-fixture")))]
#[doc(hidden)]
pub fn service_runner_for_test(
    plist: PathBuf,
    run_id: &str,
    runner: TestRunner,
) -> io::Result<impl Drop> {
    if run_id.len() != 32
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !plist.is_absolute()
        || plist.file_name().and_then(|name| name.to_str()) != Some("fixture.r78.plist")
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "synthetic service identity required",
        ));
    }
    struct Reset(Option<ServiceTestHooks>);
    impl Drop for Reset {
        fn drop(&mut self) {
            SERVICE_TEST_HOOKS.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let domain = format!("fixture/{run_id}");
    let service = format!("{domain}/fixture.r78.{run_id}");
    Ok(Reset(SERVICE_TEST_HOOKS.with(|slot| {
        slot.replace(Some(ServiceTestHooks {
            plist,
            domain,
            service,
            runner,
        }))
    })))
}

#[cfg(all(debug_assertions, not(feature = "credential-relocation-fixture")))]
pub(crate) fn test_helper_result(
    program: &Path,
    arguments: &[&std::ffi::OsStr],
    deadline: Instant,
) -> Option<io::Result<std::process::Output>> {
    SERVICE_TEST_HOOKS.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.as_mut().map(|hooks| {
            require_time(deadline)?;
            let result = (hooks.runner)(program, arguments, deadline);
            require_time(deadline)?;
            result
        })
    })
}

#[cfg(all(debug_assertions, not(feature = "credential-relocation-fixture")))]
pub(crate) fn test_identifiers() -> Option<(String, String)> {
    SERVICE_TEST_HOOKS.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|hooks| (hooks.domain.clone(), hooks.service.clone()))
    })
}

#[cfg(all(debug_assertions, not(feature = "credential-relocation-fixture")))]
pub(crate) fn test_plist() -> Option<PathBuf> {
    SERVICE_TEST_HOOKS.with(|slot| slot.borrow().as_ref().map(|hooks| hooks.plist.clone()))
}
