// SPDX-License-Identifier: AGPL-3.0-only

//! Synthetic service state. This module never constructs a system command,
//! resolves a home directory, queries a real PID or sends a signal.

use super::{
    hub_user_process::AdmittedUserHub,
    r78_lifecycle::{ReadinessBounds, ServiceEffects, ServiceFailure},
    r78_service::require_time,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordingState {
    pub loaded: bool,
    pub disabled: bool,
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyntheticServiceIdentity {
    pub label: String,
    pub plist_path: PathBuf,
    pub domain: String,
}

pub struct RecordingBackend {
    data_dir: PathBuf,
    identity: SyntheticServiceIdentity,
    state: RecordingState,
    state_file: File,
    trace: File,
    sequence: u64,
    ended: bool,
    unwind_restart_failure: Option<ServiceFailure<io::Error>>,
}

impl RecordingBackend {
    pub fn open(
        data_dir: &Path,
        identity: SyntheticServiceIdentity,
        state: RecordingState,
        state_file: File,
        trace: File,
        run_id: &str,
    ) -> io::Result<Self> {
        if run_id.len() != 32
            || !run_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || identity.label != format!("fixture.teslatlas-relocation.{run_id}")
            || identity.domain != format!("fixture/{run_id}")
            || state
                .pid
                .is_some_and(|pid| !(900_000_000..=999_999_999).contains(&pid))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture_owner_identity_refused",
            ));
        }
        Ok(Self {
            data_dir: data_dir.to_owned(),
            identity,
            state,
            state_file,
            trace,
            sequence: 0,
            ended: false,
            unwind_restart_failure: None,
        })
    }

    pub fn identity(&self) -> &SyntheticServiceIdentity {
        &self.identity
    }
    pub fn state(&self) -> &RecordingState {
        &self.state
    }
    pub fn take_unwind_restart_failure(&mut self) -> Option<ServiceFailure<io::Error>> {
        self.unwind_restart_failure.take()
    }

    pub fn observe(&mut self, operation: &str) -> io::Result<()> {
        if self.ended
            || !matches!(
                operation,
                "loaded_query"
                    | "pid_query"
                    | "plist_decode"
                    | "secrets_exclusion"
                    | "disable"
                    | "bootout"
                    | "unload_wait_observation"
                    | "admission_probe"
                    | "bootstrap"
                    | "kickstart"
                    | "readiness_probe"
            )
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture_command_refused",
            ));
        }
        self.sequence += 1;
        serde_json::to_writer(&mut self.trace, &serde_json::json!({"seq": self.sequence, "op": operation, "label": self.identity.label, "domain": self.identity.domain, "outcome": "ok"})).map_err(io::Error::other)?;
        self.trace.write_all(b"\n")?;
        self.trace.flush()
    }

    fn persist(&mut self) -> io::Result<()> {
        self.state_file.seek(SeekFrom::Start(0))?;
        serde_json::to_writer(&mut self.state_file, &self.state).map_err(io::Error::other)?;
        let end = self.state_file.stream_position()?;
        self.state_file.set_len(end)?;
        self.state_file.flush()
    }

    pub fn loaded_query(&mut self) -> io::Result<bool> {
        self.observe("loaded_query")?;
        Ok(self.state.loaded)
    }
    pub fn pid_query(&mut self) -> io::Result<Option<u32>> {
        self.observe("pid_query")?;
        Ok(self.state.pid)
    }
    pub fn plist_decode(&mut self) -> io::Result<PathBuf> {
        self.observe("plist_decode")?;
        Ok(self.identity.plist_path.clone())
    }
    pub fn readiness_query(&mut self) -> io::Result<bool> {
        self.observe("readiness_probe")?;
        Ok(self.state.loaded && !self.state.disabled && self.state.pid.is_some())
    }

    pub fn persistent_stop(&mut self) -> io::Result<()> {
        self.observe("disable")?;
        self.state.disabled = true;
        self.persist()?;
        self.observe("bootout")?;
        self.state.loaded = false;
        self.state.pid = None;
        self.persist()?;
        self.observe("unload_wait_observation")
    }

    pub fn finish(&mut self) -> io::Result<()> {
        if !self.ended {
            serde_json::to_writer(&mut self.trace, &serde_json::json!({"op": "run_end", "systemExecutorSelected": false, "ownerTargetSubmitted": false, "childrenSpawned": 0})).map_err(io::Error::other)?;
            self.trace.write_all(b"\n")?;
            self.trace.flush()?;
            self.ended = true;
        }
        Ok(())
    }
}

impl Drop for RecordingBackend {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

impl ServiceEffects for RecordingBackend {
    type Admission = Arc<AdmittedUserHub>;
    type Error = io::Error;
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn loaded(&mut self, deadline: Instant) -> io::Result<bool> {
        require_time(deadline)?;
        self.loaded_query()
    }
    fn bootout(&mut self, deadline: Instant) -> io::Result<()> {
        require_time(deadline)?;
        self.observe("bootout")?;
        self.state.loaded = false;
        self.state.pid = None;
        self.persist()
    }
    fn wait_unloaded(&mut self, deadline: Instant) -> io::Result<()> {
        require_time(deadline)?;
        self.observe("unload_wait_observation")?;
        if self.state.loaded {
            Err(io::Error::other("fixture_service_loaded"))
        } else {
            Ok(())
        }
    }
    fn admit(&mut self, deadline: Instant) -> io::Result<Self::Admission> {
        require_time(deadline)?;
        self.observe("admission_probe")?;
        AdmittedUserHub::admit(&self.data_dir).map_err(io::Error::other)
    }
    fn bootstrap(&mut self, deadline: Instant) -> io::Result<()> {
        require_time(deadline)?;
        if self.state.disabled || self.state.loaded {
            return Err(io::Error::other("fixture_bootstrap_refused"));
        }
        self.observe("bootstrap")?;
        self.state.loaded = true;
        self.persist()
    }
    fn kickstart(&mut self, deadline: Instant) -> io::Result<()> {
        require_time(deadline)?;
        if !self.state.loaded {
            return Err(io::Error::other("fixture_service_unloaded"));
        }
        self.observe("kickstart")?;
        self.state.pid = Some(900_000_001);
        self.persist()
    }
    fn wait_ready(&mut self, deadline: Instant, bounds: ReadinessBounds) -> io::Result<()> {
        let mut consecutive = 0;
        while Instant::now() < deadline {
            if self.readiness_query()? {
                consecutive += 1;
            } else {
                consecutive = 0;
            }
            if consecutive == bounds.stable_observations {
                return Ok(());
            }
            std::thread::sleep(
                bounds
                    .poll
                    .min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "fixture_readiness_deadline",
        ))
    }
    fn record_unwind_restart_failure(&mut self, failure: ServiceFailure<Self::Error>) {
        self.unwind_restart_failure = Some(failure);
    }
}

#[cfg(feature = "credential-relocation-fixture")]
thread_local! {
    static FIXTURE_BACKEND: std::cell::RefCell<Option<RecordingBackend>> = const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "credential-relocation-fixture")]
pub fn with_fixture_backend<T>(
    operation: impl FnOnce(&mut RecordingBackend) -> io::Result<T>,
) -> io::Result<T> {
    FIXTURE_BACKEND.with(|slot| {
        let mut slot = slot
            .try_borrow_mut()
            .map_err(|_| io::Error::other("fixture_backend_missing"))?;
        operation(
            slot.as_mut()
                .ok_or_else(|| io::Error::other("fixture_backend_missing"))?,
        )
    })
}

#[cfg(feature = "credential-relocation-fixture")]
pub struct FixtureBackendGuard;
#[cfg(feature = "credential-relocation-fixture")]
impl FixtureBackendGuard {
    pub fn install(backend: RecordingBackend) -> io::Result<Self> {
        FIXTURE_BACKEND.with(|slot| {
            let mut slot = slot
                .try_borrow_mut()
                .map_err(|_| io::Error::other("fixture_backend_missing"))?;
            if slot.is_some() {
                return Err(io::Error::other("fixture_backend_already_selected"));
            }
            *slot = Some(backend);
            Ok(Self)
        })
    }
    pub fn finish(&mut self) -> io::Result<()> {
        with_fixture_backend(RecordingBackend::finish)
    }
}
#[cfg(feature = "credential-relocation-fixture")]
impl Drop for FixtureBackendGuard {
    fn drop(&mut self) {
        FIXTURE_BACKEND.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
