// SPDX-License-Identifier: AGPL-3.0-only

//! Test doubles for the fresh-install journey (M02-T21, DEC-116).
//!
//! Compiled only with the test-only `journey-doubles` feature: the crate's own
//! test build enables it through its self dev-dependency, and `cargo build`,
//! debug or release, never does (a build without debug assertions refuses
//! it). Even in a test build nothing changes unless a test names a doubles
//! file in `TESLATLAS_HUB_JOURNEY_DOUBLES`. That file names a folder that holds
//! the window anchor and the issuer pin in place of the login Keychain, and
//! optionally the loopback Tesla doubles: the owner token endpoint, the Owner
//! API base and the stream. The window's network time check then reads a
//! fixed zero offset in place of the time server, and the owner's issuer
//! confirmation is accepted in place of the dialog. The window itself, its
//! grant checks and the egress gate every Tesla send asks are unchanged.

use std::{
    io::Write as _,
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use teslatlas_hub::{
    collection::window::{AnchorStore, ManualProbe, Refusal, ResumeOutcome, WindowController},
    config::HubConfig,
    runtime::operator::{
        admission::OperatorRefusal,
        authority,
        trust::{OwnerConfirmation, TrustAuthority},
    },
};

/// The variable a test sets to the path of its doubles file.
pub(crate) const ENV: &str = "TESLATLAS_HUB_JOURNEY_DOUBLES";

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct JourneyDoubles {
    /// Absolute folder of the file-backed anchor (the test's own).
    anchor_dir: PathBuf,
    /// Loopback `http` owner token endpoint.
    #[serde(default)]
    token_endpoint: Option<String>,
    /// Loopback `http` Owner API base.
    #[serde(default)]
    owner_api_base: Option<String>,
    /// Loopback `ws` stream endpoint.
    #[serde(default)]
    stream_endpoint: Option<String>,
}

/// The doubles a test named, read once per process; `None` when it named
/// none. Reading them installs the Owner API double.
pub(crate) fn active() -> Result<Option<&'static JourneyDoubles>, Box<dyn std::error::Error>> {
    static DOUBLES: OnceLock<Option<JourneyDoubles>> = OnceLock::new();
    if let Some(doubles) = DOUBLES.get() {
        return Ok(doubles.as_ref());
    }
    let loaded = match std::env::var_os(ENV) {
        None => None,
        Some(path) => {
            let doubles: JourneyDoubles = serde_json::from_slice(&std::fs::read(path)?)?;
            if !doubles.anchor_dir.is_absolute() {
                return Err("the journey anchor folder must be an absolute path".into());
            }
            Some(doubles)
        }
    };
    let doubles = DOUBLES.get_or_init(|| loaded).as_ref();
    if let Some(base) = doubles.and_then(|doubles| doubles.owner_api_base.as_deref()) {
        teslatlas_hub::owner_api::install_journey_owner_api_double(base)
            .map_err(|_| "the journey Owner API double must be a loopback http base")?;
    }
    Ok(doubles)
}

/// The owner token endpoint double, if the test named one.
pub(crate) fn token_endpoint() -> Option<&'static str> {
    active().ok().flatten().and_then(|doubles| doubles.token_endpoint.as_deref())
}

impl JourneyDoubles {
    fn anchor(&self) -> Arc<dyn AnchorStore> {
        Arc::new(FileAnchor {
            dir: self.anchor_dir.clone(),
        })
    }

    /// The served configuration with the stream double in place of Tesla's
    /// stream; the configured Owner API base, and so the collector's mode,
    /// stay as they are.
    pub(crate) fn apply(&self, config: &mut HubConfig) {
        if let Some(endpoint) = &self.stream_endpoint {
            config.collector.stream_endpoint_override = Some(endpoint.clone());
        }
    }

    /// [`authority::start_service_authority`] with the file-backed anchor and
    /// a fixed zero time offset.
    pub(crate) fn start_authority(
        &self,
        data_dir: &Path,
        home: &Path,
    ) -> Result<(Arc<WindowController>, ResumeOutcome), OperatorRefusal> {
        let host_id = teslatlas_hub::runtime::operator::host::this_host_id()?;
        let (controller, outcome) = WindowController::start(
            authority::service_window_config(data_dir, home, host_id),
            authority::service_window_deps(
                self.anchor(),
                Arc::new(ManualProbe::new(0.0)),
                Arc::new(authority::StoreHighWater::new(data_dir)),
            ),
        );
        Ok((Arc::new(controller), outcome))
    }

    /// [`authority::service_trust_authority`] with the file-backed pin and an
    /// accepted confirmation.
    pub(crate) fn trust_authority(&self, host_id: &str) -> TrustAuthority {
        TrustAuthority::new(self.anchor(), host_id, Arc::new(AcceptedConfirmation))
    }
}

/// One file per anchor name, 0600, in the test's own folder.
struct FileAnchor {
    dir: PathBuf,
}

impl FileAnchor {
    fn path(&self, name: &str) -> Result<PathBuf, Refusal> {
        let plain = !name.is_empty()
            && !name.starts_with('.')
            && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
        if !plain {
            return Err(Refusal::AnchorUnavailable);
        }
        Ok(self.dir.join(name))
    }
}

impl AnchorStore for FileAnchor {
    fn read(&self, name: &str) -> Result<Option<String>, Refusal> {
        match std::fs::read_to_string(self.path(name)?) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(Refusal::AnchorUnavailable),
        }
    }

    fn write(&self, name: &str, value: &str) -> Result<(), Refusal> {
        let path = self.path(name)?;
        let staged = self.dir.join(format!(".{name}.tmp"));
        let written = std::fs::create_dir_all(&self.dir)
            .and_then(|()| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&staged)
            })
            .and_then(|mut file| file.write_all(value.as_bytes()).and_then(|()| file.sync_all()))
            .and_then(|()| std::fs::rename(&staged, &path));
        written.map_err(|_| Refusal::AnchorUnavailable)
    }
}

/// The owner's answer, given by the test that started this process.
struct AcceptedConfirmation;

impl OwnerConfirmation for AcceptedConfirmation {
    fn confirm_issuer(&self, _: &str, _: u64, _: Option<&str>) -> bool {
        true
    }
}
