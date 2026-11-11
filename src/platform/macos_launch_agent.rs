// SPDX-License-Identifier: AGPL-3.0-only

//! Serve preflight, inherited log compaction, the persistent launchctl stop used by
//! sign-out and the installed-service helpers retained for C10.

use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[cfg(unix)]
use std::os::{
    fd::{AsFd, BorrowedFd},
    unix::fs::{FileExt, MetadataExt},
};

#[path = "macos_launch_agent/fixture_clock.rs"]
mod fixture_clock;
pub use fixture_clock::fixture_summary_now_ms;

const LABEL: &str = "com.teslatlas.hub";
const PLIST_NAME: &str = "com.teslatlas.hub.plist";
const SERVICE_UNLOAD_ATTEMPTS: usize = 100;
const SERVICE_READY_ATTEMPTS: usize = 50;
const SERVICE_READY_STABLE_OBSERVATIONS: usize = 10;
const LOCK_RELEASE_ATTEMPTS: usize = 100;
pub const DEVELOPMENT_SERVE_ENV: &str = "TESLATLAS_HUB_DEVELOPMENT";
pub const DEVELOPMENT_SERVE_MODE_ENV: &str = "TESLATLAS_HUB_DEVELOPMENT_MODE";
#[cfg(not(test))]
const SERVICE_UNLOAD_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
const SERVICE_UNLOAD_DELAY: Duration = Duration::ZERO;
#[cfg(not(test))]
const SERVICE_READY_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
const SERVICE_READY_DELAY: Duration = Duration::ZERO;
#[cfg(not(test))]
const LOCK_RELEASE_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
const LOCK_RELEASE_DELAY: Duration = Duration::ZERO;
const READINESS_CONNECT_TIMEOUT: Duration = Duration::from_millis(100);

/// Validate the configured collection authority, including receiver-only
/// Fleet ingestion where no Tesla Fleet token is owned by Hub.
pub fn preflight_hub_for_config(config: &crate::config::HubConfig) -> io::Result<()> {
    if config.collector.interval_seconds == 0 && config.collector.fleet_telemetry.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "production Hub requires an enabled collector path",
        ));
    }
    preflight_hub_for_provider(&config.data_dir, config.collector.provider)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevelopmentServeMode {
    Fixture,
    Standalone,
    PrivateLan,
}

/// Parse the process-level source-run opt-in without consulting global state.
/// An absent opt-in always selects production admission. The historical
/// fixture behavior remains the default for existing explicit development
/// launchers; standalone composition must be selected by name.
pub fn development_serve_mode(
    opt_in: Option<&OsStr>,
    mode: Option<&OsStr>,
) -> io::Result<Option<DevelopmentServeMode>> {
    match (opt_in, mode) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{DEVELOPMENT_SERVE_MODE_ENV} requires {DEVELOPMENT_SERVE_ENV}=1"),
        )),
        (Some(value), _) if value != OsStr::new("1") => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{DEVELOPMENT_SERVE_ENV} must be exactly 1 when supplied"),
        )),
        (Some(_), None) => Ok(Some(DevelopmentServeMode::Fixture)),
        (Some(_), Some(value)) if value == OsStr::new("fixture") => {
            Ok(Some(DevelopmentServeMode::Fixture))
        }
        (Some(_), Some(value)) if value == OsStr::new("standalone") => {
            Ok(Some(DevelopmentServeMode::Standalone))
        }
        (Some(_), Some(value)) if value == OsStr::new("private-lan") => {
            Ok(Some(DevelopmentServeMode::PrivateLan))
        }
        (Some(_), Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{DEVELOPMENT_SERVE_MODE_ENV} must be fixture, standalone, or private-lan"),
        )),
    }
}

/// Validate Serve without weakening production preflight.
/// Existing source-run modes remain loopback-only. Private LAN is an explicit
/// local-v1 opt-in with an exact RFC1918 IPv4 HTTPS identity.
/// Fixture mode additionally requires the historical published fixture;
/// standalone permits empty/import-only state.
pub fn preflight_hub_for_serve(
    config: &crate::config::HubConfig,
    development_mode: Option<DevelopmentServeMode>,
) -> io::Result<()> {
    let Some(development_mode) = development_mode else {
        return preflight_hub_for_config(config);
    };
    if development_mode == DevelopmentServeMode::PrivateLan {
        return preflight_private_lan_serve(config);
    }
    if !config.bind.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source-run Serve requires a loopback bind address",
        ));
    }
    let tls = config.tls.as_ref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "source-run Serve requires strict TLS",
        )
    })?;
    let public_url = url::Url::parse(&tls.public_url).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "source-run Serve public URL is invalid",
        )
    })?;
    let public_ip = public_url
        .host_str()
        .and_then(|host| host.parse::<IpAddr>().ok());
    if public_url.scheme() != "https"
        || !public_ip.is_some_and(|address| address.is_loopback())
        || public_url.port_or_known_default() != Some(config.bind.port())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source-run Serve requires a matching literal loopback HTTPS public URL",
        ));
    }
    crate::server::validate_tls_identity(tls)?;

    match development_mode {
        DevelopmentServeMode::Standalone => {
            if config.collector.interval_seconds != 0 || config.collector.fleet_telemetry.is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "standalone source-run Serve requires every collector path to be disabled",
                ));
            }
            crate::db::HubStore::preflight_standalone_schema_read_only(&config.data_dir).map_err(
                |error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("standalone source-run Hub data is unavailable: {error}"),
                    )
                },
            )?;
            return Ok(());
        }
        DevelopmentServeMode::PrivateLan => unreachable!("private LAN preflight returned above"),
        DevelopmentServeMode::Fixture => {}
    }

    if config.collector.interval_seconds != 0 || config.collector.fleet_telemetry.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fixture source-run Serve requires every collector path to be disabled",
        ));
    }

    let store = crate::db::HubStore::open_read_only(&config.data_dir).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("fixture source-run Hub data is unavailable: {error}"),
        )
    })?;
    let published = store.published_vehicles().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("source-run fixture catalogue is unavailable: {error}"),
        )
    })?;
    if published.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fixture source-run Serve requires published fixture data",
        ));
    }
    let key_bytes =
        crate::legacy_owner_credentials::load_existing_cursor_key_bytes(&config.data_dir)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source-run fixture cursor key is unavailable",
                )
            })?;
    let mut key = [0_u8; 32];
    key.copy_from_slice(key_bytes.as_slice());
    let cursor_key = crate::protocol::CursorKey::from_bytes(key);
    for vehicle in published {
        let (manifest_bytes, _) = crate::updates_delivery::schema_22_signed_artifacts(
            &store,
            vehicle.vehicle_id,
            &cursor_key,
        )
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.message))?;
        let manifest: crate::protocol::SyncManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        for pack in manifest.chunks {
            let stored = store
                .pack_for_digest(pack.sha256)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "source-run fixture pack is not authorized",
                    )
                })?;
            let metadata = fs::symlink_metadata(&stored.path)?;
            if !metadata.is_file()
                || metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.mode() & 0o022 != 0
                || metadata.len() != stored.compressed_bytes
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source-run fixture pack is unavailable or unsafe",
                ));
            }
        }
    }
    Ok(())
}

fn preflight_private_lan_serve(config: &crate::config::HubConfig) -> io::Result<()> {
    let IpAddr::V4(bind_ip) = config.bind.ip() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private-LAN source-run Serve requires a literal RFC1918 IPv4 bind address",
        ));
    };
    if !bind_ip.is_private() || config.bind.port() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private-LAN source-run Serve requires a literal RFC1918 IPv4 bind address",
        ));
    }
    let tls = config.tls.as_ref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "private-LAN source-run Serve requires strict TLS",
        )
    })?;
    let expected_url = format!("https://{bind_ip}:{}", config.bind.port());
    if tls.public_url != expected_url && tls.public_url != format!("{expected_url}/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private-LAN source-run Serve requires an exact matching HTTPS public URL",
        ));
    }
    crate::server::validate_tls_identity(tls)?;
    if config.collector.fleet_telemetry.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private-LAN source-run Serve does not admit Fleet Telemetry",
        ));
    }
    if config.collector.interval_seconds == 0 {
        crate::db::HubStore::open_read_only(&config.data_dir).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("private-LAN source-run Hub data is unavailable: {error}"),
            )
        })?;
        return Ok(());
    }
    if config.collector.provider != crate::config::CollectorProvider::Legacy {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private-LAN source-run Serve admits only native Legacy collection",
        ));
    }
    preflight_hub_for_provider(&config.data_dir, crate::config::CollectorProvider::Legacy)
}

/// Query the per-user LaunchAgent without changing it.
pub fn service_is_loaded() -> io::Result<bool> {
    let (_, service) = service_identifiers();
    service_is_loaded_with_runner(&service, &mut real_launchctl)
}

/// Wait for a stopped Serve process to release the lifetime lock, then retain
/// the admission for the caller.
pub fn admit_after_service_stop(
    data_dir: &Path,
) -> io::Result<std::sync::Arc<crate::hub_user_process::AdmittedUserHub>> {
    wait_for_admission_with_probe(|| crate::hub_user_process::AdmittedUserHub::admit(data_dir))
}

/// Prove that the installed job is running, owns the Hub lifetime lock, and
/// returns application readiness bound to that PID and exact loaded configuration.
pub fn wait_for_installed_readiness(
    config: &crate::config::HubConfig,
    config_digest: crate::protocol::Sha256Digest,
) -> io::Result<()> {
    let (_, service) = service_identifiers();
    let mut ready_pid = None;
    wait_for_service_ready_with_probe(&mut || {
        installed_service_is_ready(config, config_digest, &service, &mut ready_pid)
    })
}

/// Identify the exact configuration served by the existing job.
pub fn wait_for_current_installed_readiness() -> io::Result<()> {
    let (config, digest) = configured_identity_from_plist(&installed_plist()?)?;
    wait_for_installed_readiness(&config, digest)
}

/// Start after the caller validated the exact loaded configuration.
pub fn start_preflighted_installed() -> io::Result<()> {
    let plist = installed_plist()?;
    let (domain, service) = service_identifiers();
    start_installed_with_runner(&plist, &domain, &service, &mut real_launchctl)
}

/// Persistently stop the per-user LaunchAgent: disable, then bootout, then wait
/// until it is unloaded. The service stays disabled across logins until the app's
/// Start Hub enables it. Never use this for a transient stop: a disabled job
/// cannot be loaded, and start_preflighted_installed issues no enable. A transient
/// stop is bootout plus wait_for_service_unloaded_with_runner.
pub fn stop_installed() -> io::Result<()> {
    let (_, service) = service_identifiers();
    stop_installed_with_runner(&service, &mut real_launchctl)
}

/// Refuse production start unless this data directory has a
/// configured car and at least one usable provider credential pair.
pub fn preflight_hub(data_dir: &Path) -> io::Result<()> {
    let store = preflight_store(data_dir)?;
    if provider_credentials_are_usable(&store, data_dir, crate::config::CollectorProvider::Legacy)
        .is_ok()
        || provider_credentials_are_usable(
            &store,
            data_dir,
            crate::config::CollectorProvider::Fleet,
        )
        .is_ok()
    {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "usable Legacy or Fleet credentials are required before install",
    ))
}

/// Validate the credentials selected by configuration without mutating them.
pub fn preflight_hub_for_provider(
    data_dir: &Path,
    provider: crate::config::CollectorProvider,
) -> io::Result<()> {
    let store = preflight_store(data_dir)?;
    provider_credentials_are_usable(&store, data_dir, provider)
}

fn preflight_store(data_dir: &Path) -> io::Result<crate::db::HubStore> {
    let store = crate::db::HubStore::open_read_only(data_dir).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Hub data is unavailable: {error}"),
        )
    })?;
    if store
        .configured_tesla_vehicles()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .is_empty()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "at least one configured vehicle is required before install",
        ));
    }
    Ok(store)
}

fn provider_credentials_are_usable(
    store: &crate::db::HubStore,
    data_dir: &Path,
    provider: crate::config::CollectorProvider,
) -> io::Result<()> {
    match provider {
        crate::config::CollectorProvider::Legacy => {
            let presence=store.credential_pair_presence(crate::auth::credential_envelope::CredentialSlot::OwnerApi)
                .map_err(|_|io::Error::new(io::ErrorKind::InvalidData,"credential presence unavailable"))?
                .ok_or_else(||io::Error::new(io::ErrorKind::InvalidData,"legacy Owner API credentials are required before install"))?;
            presence.header_check.map_err(|_|io::Error::new(io::ErrorKind::InvalidData,"credential header unreadable"))?;
            let secrets=crate::platform::private_directory::open_validated_read_only(&data_dir.join("secrets"))
                .map_err(|_|io::Error::new(io::ErrorKind::InvalidData,"credential directory unavailable"))?
                .ok_or_else(||io::Error::new(io::ErrorKind::InvalidData,"credential keyring missing"))?;
            crate::auth::credential_keys::check_keyring(&secrets)
                .map_err(|_|io::Error::new(io::ErrorKind::InvalidData,"credential keyring unusable"))?
                .ok_or_else(||io::Error::new(io::ErrorKind::InvalidData,"credential keyring missing"))?;
            if store.load_collector_auth_row().map_err(|_|io::Error::new(io::ErrorKind::InvalidData,"credential status unavailable"))?.is_some_and(|row|row.state=="signed_out") {
                return Err(io::Error::new(io::ErrorKind::InvalidData,"credential sign-in required"));
            }
        }
        crate::config::CollectorProvider::Fleet => {
            // Diagnostic admission (C3 R47, B2 clarification 3): the pair, key,
            // ciphertext, generation, claims and scopes must be sound, but an
            // identified unresolved refresh passes: the Hub then starts as a
            // running reconnect stop and sends nothing. The inspection reads only.
            crate::fleet_credentials::inspect_stored_fleet_credentials(store, data_dir)
                .map(drop)
                .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Fleet credentials are unusable: {error}"),
                    )
                })?;
        }
    }
    Ok(())
}

fn configured_identity_from_plist(
    plist: &Path,
) -> io::Result<(crate::config::HubConfig, crate::protocol::Sha256Digest)> {
    if fs::metadata(plist)?.len() > 1024 * 1024 {
        return Err(io::Error::other("installed plist is too large"));
    }
    let output = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", "--"])
        .arg(plist)
        .output()?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
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
    let config_path = Path::new(
        args[2]
            .as_str()
            .ok_or_else(|| io::Error::other("installed plist config path is absent"))?,
    );
    if !config_path.is_absolute() {
        return Err(io::Error::other(
            "installed plist config path must be absolute",
        ));
    }
    crate::config::HubConfig::load_with_digest(config_path).map_err(io::Error::other)
}

fn service_identifiers() -> (String, String) {
    let domain = format!("gui/{}", rustix::process::geteuid().as_raw());
    let service = format!("{domain}/{LABEL}");
    (domain, service)
}

fn installed_plist() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    let plist = home.join("Library").join("LaunchAgents").join(PLIST_NAME);
    let metadata = fs::symlink_metadata(&plist)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "installed Hub LaunchAgent plist is unsafe",
        ));
    }
    Ok(plist)
}

fn start_installed_with_runner(
    plist: &Path,
    domain: &str,
    service: &str,
    runner: &mut impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
) -> io::Result<()> {
    if !service_is_loaded_with_runner(service, runner)? {
        run_launchctl(
            runner,
            &[
                std::ffi::OsStr::new("bootstrap"),
                std::ffi::OsStr::new(domain),
                plist.as_os_str(),
            ],
        )?;
    }
    run_launchctl(
        runner,
        &[
            std::ffi::OsStr::new("kickstart"),
            std::ffi::OsStr::new("-k"),
            std::ffi::OsStr::new(service),
        ],
    )?;
    if !service_is_loaded_with_runner(service, runner)? {
        return Err(io::Error::other(
            "Hub LaunchAgent is not loaded after start",
        ));
    }
    Ok(())
}

fn stop_installed_with_runner(
    service: &str,
    runner: &mut impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
) -> io::Result<()> {
    run_launchctl(runner, &[OsStr::new("disable"), OsStr::new(service)])?;
    let bootout = [
        std::ffi::OsStr::new("bootout"),
        std::ffi::OsStr::new(service),
    ];
    let _ = runner(&bootout)?;
    wait_for_service_unloaded_with_runner(service, runner)
}

fn wait_for_admission_with_probe<T>(
    mut admit: impl FnMut() -> Result<T, crate::hub_user_process::UserLifetimeLockError>,
) -> io::Result<T> {
    for attempt in 0..LOCK_RELEASE_ATTEMPTS {
        match admit() {
            Ok(admission) => return Ok(admission),
            Err(crate::hub_user_process::UserLifetimeLockError::AlreadyRunning) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
        if attempt + 1 < LOCK_RELEASE_ATTEMPTS {
            std::thread::sleep(LOCK_RELEASE_DELAY);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Hub lifetime lock is still held after service stop",
    ))
}

fn wait_for_service_unloaded_with_runner(
    service: &str,
    runner: &mut impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
) -> io::Result<()> {
    for attempt in 0..SERVICE_UNLOAD_ATTEMPTS {
        if !service_is_loaded_with_runner(service, runner)? {
            return Ok(());
        }
        if attempt + 1 < SERVICE_UNLOAD_ATTEMPTS {
            std::thread::sleep(SERVICE_UNLOAD_DELAY);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Hub LaunchAgent is still loaded after stop",
    ))
}

fn service_is_loaded_with_runner(
    service: &str,
    runner: &mut impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
) -> io::Result<bool> {
    runner(&[std::ffi::OsStr::new("print"), std::ffi::OsStr::new(service)])
}

fn wait_for_service_ready_with_probe(
    probe: &mut impl FnMut() -> io::Result<bool>,
) -> io::Result<()> {
    let mut consecutive = 0;
    for attempt in 0..SERVICE_READY_ATTEMPTS {
        if probe()? {
            consecutive += 1;
            if consecutive == SERVICE_READY_STABLE_OBSERVATIONS {
                return Ok(());
            }
        } else {
            consecutive = 0;
        }
        if attempt + 1 < SERVICE_READY_ATTEMPTS {
            std::thread::sleep(SERVICE_READY_DELAY);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "replacement Hub service did not become stably ready",
    ))
}

fn installed_service_is_ready(
    config: &crate::config::HubConfig,
    config_digest: crate::protocol::Sha256Digest,
    service: &str,
    ready_pid: &mut Option<u32>,
) -> io::Result<bool> {
    let Some(pid) = real_service_running_pid(service)? else {
        *ready_pid = None;
        return Ok(false);
    };
    match crate::hub_user_process::AdmittedUserHub::admit(&config.data_dir) {
        Err(crate::hub_user_process::UserLifetimeLockError::AlreadyRunning) => {}
        Ok(admission) => {
            drop(admission);
            *ready_pid = None;
            return Ok(false);
        }
        Err(error) => return Err(io::Error::other(error)),
    }
    readiness_observation(ready_pid, pid, &mut || {
        configured_application_is_ready(config, config_digest, pid)
    })
}

fn readiness_observation(
    ready_pid: &mut Option<u32>,
    pid: u32,
    listener_probe: &mut impl FnMut() -> io::Result<bool>,
) -> io::Result<bool> {
    let listener_ready = listener_probe()?;
    let same_pid = ready_pid.is_none_or(|ready_pid| ready_pid == pid);
    *ready_pid = Some(pid);
    Ok(same_pid && listener_ready)
}

fn configured_application_is_ready(
    config: &crate::config::HubConfig,
    config_digest: crate::protocol::Sha256Digest,
    pid: u32,
) -> io::Result<bool> {
    let address = readiness_address(config.bind);
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(READINESS_CONNECT_TIMEOUT)
        .timeout(Duration::from_millis(500));
    let endpoint = if let Some(tls) = &config.tls {
        let mut pem = Vec::new();
        File::open(&tls.certificate_path)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut pem)?;
        if pem.len() > 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "readiness certificate is too large",
            ));
        }
        let certificates = reqwest::Certificate::from_pem_bundle(&pem).map_err(io::Error::other)?;
        if certificates.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "readiness certificate is empty",
            ));
        }
        let mut endpoint = reqwest::Url::parse(&tls.public_url).map_err(io::Error::other)?;
        let host = endpoint
            .host_str()
            .ok_or_else(|| io::Error::other("readiness TLS hostname is absent"))?;
        builder = builder
            .https_only(true)
            .tls_certs_only(certificates)
            .resolve(host, address);
        endpoint
            .set_port(Some(address.port()))
            .map_err(|_| io::Error::other("readiness TLS port is invalid"))?;
        endpoint.set_path("/readyz");
        endpoint
    } else {
        if !address.ip().is_loopback() {
            return Err(io::Error::other("plaintext readiness requires loopback"));
        }
        reqwest::Url::parse(&format!("http://{address}/readyz")).map_err(io::Error::other)?
    };
    // These synchronous service APIs are also called from the CLI's async
    // runtime. Give the bounded probe its own thread to avoid nested runtimes.
    std::thread::spawn(move || {
        crate::crypto::install_default_provider();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async move {
            use futures_util::StreamExt as _;
            let client = builder.build().map_err(io::Error::other)?;
            let response = match client
                .get(endpoint)
                .header("accept-encoding", "identity")
                .send()
                .await
            {
                Ok(response) => response,
                Err(_) => return Ok(false),
            };
            let status = response.status();
            let headers = response.headers().clone();
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let Ok(chunk) = chunk else {
                    return Ok(false);
                };
                if body.len().saturating_add(chunk.len()) > 16 * 1024 {
                    return Ok(false);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(application_readiness_matches(
                status,
                &headers,
                &body,
                pid,
                config_digest,
            ))
        })
    })
    .join()
    .map_err(|_| io::Error::other("readiness probe thread failed"))?
}

fn application_readiness_matches(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &[u8],
    pid: u32,
    config_digest: crate::protocol::Sha256Digest,
) -> bool {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ready {
        status: String,
    }
    status == reqwest::StatusCode::OK
        && headers
            .get("x-teslatlas-native-process-id")
            .and_then(|value| value.to_str().ok())
            == Some(pid.to_string().as_str())
        && headers
            .get(crate::server::NATIVE_CONFIG_DIGEST_HEADER)
            .and_then(|value| value.to_str().ok())
            == Some(config_digest.to_string().as_str())
        && serde_json::from_slice::<Ready>(body).is_ok_and(|ready| ready.status == "ready")
}

fn readiness_address(configured: SocketAddr) -> SocketAddr {
    if configured.ip().is_unspecified() {
        match configured {
            SocketAddr::V4(address) => SocketAddr::from(([127, 0, 0, 1], address.port())),
            SocketAddr::V6(address) => SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], address.port())),
        }
    } else {
        configured
    }
}

fn run_launchctl(
    runner: &mut impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
    arguments: &[&std::ffi::OsStr],
) -> io::Result<()> {
    if runner(arguments)? {
        Ok(())
    } else {
        Err(io::Error::other("launchctl failed"))
    }
}

#[cfg(test)]
const REAL_LAUNCHCTL_REFUSAL: &str = "real launchctl is unavailable in unit tests";

#[cfg(test)]
fn refuse_real_launchctl_in_unit_tests() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        REAL_LAUNCHCTL_REFUSAL,
    ))
}

#[cfg(not(test))]
fn refuse_real_launchctl_in_unit_tests() -> io::Result<()> {
    Ok(())
}

fn real_launchctl(arguments: &[&std::ffi::OsStr]) -> io::Result<bool> {
    refuse_real_launchctl_in_unit_tests()?;
    let mut command = Command::new("/bin/launchctl");
    for argument in arguments {
        command.arg(*argument);
    }
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        return Ok(true);
    }

    let action = arguments
        .first()
        .and_then(|argument| argument.to_str())
        .unwrap_or("command");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if expected_launchctl_absence(action, output.status.code(), &detail) {
        return Ok(false);
    }
    Err(io::Error::other(format!(
        "launchctl {action} failed ({}): {detail}",
        output.status
    )))
}

fn real_service_running_pid(service: &str) -> io::Result<Option<u32>> {
    refuse_real_launchctl_in_unit_tests()?;
    let output = Command::new("/bin/launchctl")
        .arg("print")
        .arg(service)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        return Ok(launchctl_print_running_pid(&String::from_utf8_lossy(
            &output.stdout,
        )));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if expected_launchctl_absence("print", output.status.code(), &detail) {
        return Ok(None);
    }
    Err(io::Error::other(format!(
        "launchctl print failed ({}): {detail}",
        output.status
    )))
}

fn launchctl_print_running_pid(output: &str) -> Option<u32> {
    let running = output.lines().any(|line| line.trim() == "state = running");
    let pid = output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|pid| *pid > 0)
    });
    running.then_some(pid).flatten()
}

fn expected_launchctl_absence(action: &str, status: Option<i32>, detail: &str) -> bool {
    (action == "print" && status == Some(113) && detail.contains("Could not find service"))
        || (action == "bootout" && status == Some(3) && detail.contains("No such process"))
}

const INHERITED_LOG_COMPACT_ABOVE: u64 = 1 << 20;
const INHERITED_LOG_KEEP: u64 = 512 << 10;
const INHERITED_LOG_DIRECTORY: &str = "Library/Logs/Teslatlas Hub";
const INHERITED_STDOUT_LOG: &str = "hub.out.log";
const INHERITED_STDERR_LOG: &str = "hub.err.log";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InheritedLogCompaction {
    Compacted,
    BelowThreshold,
    NotRegularFile,
    SameFileAsStdout,
    NotAgentLog,
    FailedBeforeChange,
    FailedAfterTruncate,
}

fn inherited_agent_log_paths(home: Option<&Path>) -> Option<[PathBuf; 2]> {
    home.map(|home| {
        let directory = home.join(INHERITED_LOG_DIRECTORY);
        [
            directory.join(INHERITED_STDOUT_LOG),
            directory.join(INHERITED_STDERR_LOG),
        ]
    })
}

/// Best-effort startup compaction of the fixed per-user agent logs.
/// See the diagnostic-log compaction contract in spec section 2.6.
pub fn compact_inherited_logs() {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let paths = inherited_agent_log_paths(home.as_deref());
    let stdout = io::stdout();
    let stderr = io::stderr();
    let _ = compact_inherited_log_descriptors(
        stdout.as_fd(),
        stderr.as_fd(),
        paths
            .as_ref()
            .map(|paths| [paths[0].as_path(), paths[1].as_path()]),
    );
}

fn compact_inherited_log_descriptors(
    stdout: BorrowedFd<'_>,
    stderr: BorrowedFd<'_>,
    agent_logs: Option<[&Path; 2]>,
) -> [InheritedLogCompaction; 2] {
    compact_inherited_log_descriptors_with(
        stdout,
        stderr,
        agent_logs,
        &mut |len, file| compact_in_place(len, file),
        &mut align_shared_log_stderr,
    )
}

// The callbacks retain the real classification and descriptor path in fault tests.
fn compact_inherited_log_descriptors_with(
    stdout: BorrowedFd<'_>,
    stderr: BorrowedFd<'_>,
    agent_logs: Option<[&Path; 2]>,
    compact: &mut impl FnMut(u64, &mut InheritedLogFile) -> InheritedLogCompaction,
    align: &mut impl FnMut(&mut File) -> io::Result<()>,
) -> [InheritedLogCompaction; 2] {
    use InheritedLogCompaction::*;
    let mut outcomes = [FailedBeforeChange; 2];
    let mut stdout_identity = None;
    for (index, descriptor) in [stdout, stderr].into_iter().enumerate() {
        let Ok(owned) = descriptor.try_clone_to_owned() else {
            continue;
        };
        let mut log = File::from(owned);
        let Ok(metadata) = log.metadata() else {
            continue;
        };
        let identity = (metadata.dev(), metadata.ino());
        if index == 0 {
            stdout_identity = Some(identity);
        }
        outcomes[index] = if !metadata.is_file() {
            NotRegularFile
        } else if index == 1 && stdout_identity == Some(identity) {
            if matches!(outcomes[0], Compacted | FailedAfterTruncate) {
                let aligned = align(&mut log);
                if outcomes[0] == FailedAfterTruncate || aligned.is_err() {
                    FailedAfterTruncate
                } else {
                    SameFileAsStdout
                }
            } else {
                SameFileAsStdout
            }
        } else if metadata.len() <= INHERITED_LOG_COMPACT_ABOVE {
            BelowThreshold
        } else if let Some(reader) = matching_agent_log_reader(agent_logs, identity) {
            match rustix::fs::fcntl_getfl(&log) {
                Ok(flags) => compact(
                    metadata.len(),
                    &mut InheritedLogFile {
                        log,
                        reader,
                        append: flags.contains(rustix::fs::OFlags::APPEND),
                        position_for_write: |file| file.seek(SeekFrom::Start(0)).map(|_| ()),
                    },
                ),
                Err(_) => FailedBeforeChange,
            }
        } else {
            NotAgentLog
        };
    }
    outcomes
}

fn matching_agent_log_reader(paths: Option<[&Path; 2]>, identity: (u64, u64)) -> Option<File> {
    let paths = paths?;
    for path in paths {
        let Ok(descriptor) = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) else {
            continue;
        };
        let reader = File::from(descriptor);
        let Ok(metadata) = reader.metadata() else {
            continue;
        };
        if metadata.is_file() && (metadata.dev(), metadata.ino()) == identity {
            return Some(reader);
        }
    }
    None
}

fn align_shared_log_stderr(stderr: &mut File) -> io::Result<()> {
    align_shared_log_stderr_with(
        stderr,
        |file| {
            rustix::fs::fcntl_getfl(file)
                .map(|flags| flags.contains(rustix::fs::OFlags::APPEND))
                .map_err(Into::into)
        },
        |file| file.seek(SeekFrom::End(0)).map(|_| ()),
    )
}

fn align_shared_log_stderr_with(
    stderr: &mut File,
    is_append: impl FnOnce(&File) -> io::Result<bool>,
    seek_end: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    if !is_append(stderr)? {
        seek_end(stderr)?;
    }
    Ok(())
}

trait InheritedLogIo {
    fn read_tail(&mut self, kept: &mut [u8], offset: u64) -> io::Result<()>;
    fn prepare_write(&mut self) -> io::Result<()>;
    fn truncate(&mut self) -> io::Result<()>;
    fn reposition_after_failed_truncate(&mut self);
    fn write_kept(&mut self, kept: &[u8]) -> io::Result<()>;
    fn sync(&mut self) -> io::Result<()>;
}

fn compact_in_place(len: u64, file: &mut impl InheritedLogIo) -> InheritedLogCompaction {
    use InheritedLogCompaction::*;
    let mut kept = vec![0; INHERITED_LOG_KEEP as usize];
    if file.read_tail(&mut kept, len - INHERITED_LOG_KEEP).is_err() || file.prepare_write().is_err()
    {
        return FailedBeforeChange;
    }
    if file.truncate().is_err() {
        // Undo prepare_write so later writes do not overwrite the unchanged head.
        file.reposition_after_failed_truncate();
        return FailedBeforeChange;
    }
    if file.write_kept(&kept).is_err() || file.sync().is_err() {
        return FailedAfterTruncate;
    }
    Compacted
}

struct InheritedLogFile {
    log: File,
    reader: File,
    append: bool,
    position_for_write: fn(&mut File) -> io::Result<()>,
}

impl InheritedLogIo for InheritedLogFile {
    fn read_tail(&mut self, kept: &mut [u8], offset: u64) -> io::Result<()> {
        self.reader.read_exact_at(kept, offset)
    }
    fn prepare_write(&mut self) -> io::Result<()> {
        if !self.append {
            (self.position_for_write)(&mut self.log)?;
        }
        Ok(())
    }
    fn truncate(&mut self) -> io::Result<()> {
        self.log.set_len(0)
    }
    fn reposition_after_failed_truncate(&mut self) {
        if !self.append {
            let _ = self.log.seek(SeekFrom::End(0));
        }
    }
    fn write_kept(&mut self, kept: &[u8]) -> io::Result<()> {
        self.log.write_all(kept)
    }
    fn sync(&mut self) -> io::Result<()> {
        self.log.sync_all()
    }
}

#[cfg(test)]
#[path = "macos_launch_agent/tests.rs"]
mod tests;
