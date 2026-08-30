// SPDX-License-Identifier: AGPL-3.0-only

//! Small per-user LaunchAgent installer for the one Hub process.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const LABEL: &str = "com.teslatlas.hub";
const PLIST_NAME: &str = "com.teslatlas.hub.plist";
const BINARY_NAME: &str = "teslatlas-hub";
const PLIST_TEMPLATE: &str = include_str!("../../packaging/com.teslatlas.hub.plist.in");
const SERVICE_UNLOAD_ATTEMPTS: usize = 100;
#[cfg(not(test))]
const SERVICE_UNLOAD_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
const SERVICE_UNLOAD_DELAY: Duration = Duration::ZERO;

pub struct InstallPaths {
    pub binary: PathBuf,
    pub plist: PathBuf,
    previous_binary: Option<PathBuf>,
    previous_plist: Option<PathBuf>,
}

/// Validate the configured store and write the binary and LaunchAgent files.
/// The caller keeps the Hub instance lock until this returns, then releases
/// it before [`start_prepared`] lets launchd start Serve.
pub fn prepare_install(data_dir: &Path, config_path: &Path) -> io::Result<InstallPaths> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    let executable = std::env::current_exe()?;
    install_files_after_preflight(data_dir, config_path, &home, &executable)
}

/// Load and request start of an already prepared LaunchAgent. The caller must
/// have released the Hub instance lock first so Serve can acquire it.
pub fn start_prepared(paths: &InstallPaths) -> io::Result<()> {
    launch(paths)
}

/// Query the per-user LaunchAgent without changing it.
pub fn service_is_loaded() -> io::Result<bool> {
    let (_, service) = service_identifiers();
    service_is_loaded_with_runner(&service, &mut real_launchctl)
}

/// Start an already-installed Hub LaunchAgent after revalidating Hub data.
pub fn start_installed(data_dir: &Path) -> io::Result<()> {
    preflight_hub(data_dir)?;
    let plist = installed_plist()?;
    let (domain, service) = service_identifiers();
    start_installed_with_runner(&plist, &domain, &service, &mut real_launchctl)
}

/// Idempotently stop the installed Hub LaunchAgent.
pub fn stop_installed() -> io::Result<()> {
    let (_, service) = service_identifiers();
    stop_installed_with_runner(&service, &mut real_launchctl)
}

/// Restart an installed Hub LaunchAgent after revalidating Hub data.
pub fn restart_installed(data_dir: &Path) -> io::Result<()> {
    preflight_hub(data_dir)?;
    let plist = installed_plist()?;
    let (domain, service) = service_identifiers();
    restart_installed_with_runner(&plist, &domain, &service, &mut real_launchctl)
}

/// Refuse to replace a running LaunchAgent unless this data directory has a
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
            let tokens = store
                .load_teslamate_legacy_tokens()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "legacy Owner API credentials are required before install",
                    )
                })?;
            crate::teslamate_credentials::load_key_for_tokens(data_dir, &tokens).map_err(
                |error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("legacy Owner API credentials are unusable: {error}"),
                    )
                },
            )?;
        }
        crate::config::CollectorProvider::Fleet => {
            crate::fleet_credentials::validate_stored_fleet_credentials(store, data_dir).map_err(
                |error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Fleet credentials are unusable: {error}"),
                    )
                },
            )?;
        }
    }
    Ok(())
}

fn install_files_after_preflight(
    data_dir: &Path,
    config_path: &Path,
    home: &Path,
    executable: &Path,
) -> io::Result<InstallPaths> {
    preflight_hub(data_dir)?;
    install_files(data_dir, config_path, home, executable)
}

fn install_files(
    data_dir: &Path,
    config_path: &Path,
    home: &Path,
    executable: &Path,
) -> io::Result<InstallPaths> {
    let data_dir = absolute_existing(data_dir)?;
    let config_path = absolute_existing(config_path)?;
    let home = absolute_existing(home)?;
    let executable = absolute_existing(executable)?;
    let bin_dir = data_dir.join("bin");
    fs::create_dir_all(&bin_dir)?;
    set_mode(&bin_dir, 0o700)?;

    let binary = bin_dir.join(BINARY_NAME);
    let launch_agents = home.join("Library").join("LaunchAgents");
    fs::create_dir_all(&launch_agents)?;
    let plist = launch_agents.join(PLIST_NAME);
    let previous_binary = backup_existing_file(&binary, "previous", 0o700)?;
    let previous_plist = match backup_existing_plist(&plist) {
        Ok(previous_plist) => previous_plist,
        Err(error) => {
            if let Some(previous_binary) = &previous_binary {
                let _ = fs::remove_file(previous_binary);
            }
            return Err(error);
        }
    };
    let binary_for_cleanup = binary.clone();
    let plist_for_cleanup = plist.clone();
    let previous_binary_for_cleanup = previous_binary.clone();
    let previous_plist_for_cleanup = previous_plist.clone();
    let result = (|| {
        copy_atomic(&executable, &binary, 0o700)?;
        write_atomic(
            &plist,
            render_plist(&binary, &config_path)?.as_bytes(),
            0o600,
        )?;
        Ok(InstallPaths {
            binary,
            plist,
            previous_binary,
            previous_plist,
        })
    })();
    if result.is_err() {
        let restored_binary = previous_binary_for_cleanup
            .as_deref()
            .map(|previous| restore_file_from_backup(previous, &binary_for_cleanup).is_ok())
            .unwrap_or(true);
        let restored_plist = previous_plist_for_cleanup
            .as_deref()
            .map(|previous| restore_file_from_backup(previous, &plist_for_cleanup).is_ok())
            .unwrap_or(true);
        if restored_binary && restored_plist {
            if let Some(previous_binary) = &previous_binary_for_cleanup {
                let _ = fs::remove_file(previous_binary);
            }
            if let Some(previous_plist) = &previous_plist_for_cleanup {
                let _ = fs::remove_file(previous_plist);
            }
        }
    }
    result
}

fn launch(paths: &InstallPaths) -> io::Result<()> {
    let (domain, service) = service_identifiers();
    let mut runner = real_launchctl;
    launch_with_runner(paths, &domain, &service, &mut runner)
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
    let bootout = [
        std::ffi::OsStr::new("bootout"),
        std::ffi::OsStr::new(service),
    ];
    let _ = runner(&bootout)?;
    wait_for_service_unloaded_with_runner(service, runner)
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

fn restart_installed_with_runner(
    plist: &Path,
    domain: &str,
    service: &str,
    runner: &mut impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
) -> io::Result<()> {
    stop_installed_with_runner(service, runner)?;
    start_installed_with_runner(plist, domain, service, runner)
}

fn launch_with_runner(
    paths: &InstallPaths,
    domain: &str,
    service: &str,
    runner: &mut impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
) -> io::Result<()> {
    let print = [std::ffi::OsStr::new("print"), std::ffi::OsStr::new(service)];
    let was_loaded = match runner(&print) {
        Ok(was_loaded) => was_loaded,
        Err(error) => {
            let kind = error.kind();
            let message = match restore_prepared_files(paths) {
                Ok(()) => format!("{error}; prepared install files restored"),
                Err(rollback) => {
                    format!("{error}; prepared install file restore failed: {rollback}")
                }
            };
            return Err(io::Error::new(kind, message));
        }
    };
    let bootout = [
        std::ffi::OsStr::new("bootout"),
        std::ffi::OsStr::new(service),
    ];
    let _ = runner(&bootout)?;
    wait_for_service_unloaded_with_runner(service, runner)?;
    let bootstrap = [
        std::ffi::OsStr::new("bootstrap"),
        std::ffi::OsStr::new(domain),
        paths.plist.as_os_str(),
    ];
    if let Err(error) = run_launchctl(runner, &bootstrap) {
        let result = with_rollback_context(error, was_loaded, paths, domain, service, runner);
        if !was_loaded {
            cleanup_backups(paths);
        }
        return Err(result);
    }
    let kickstart = [
        std::ffi::OsStr::new("kickstart"),
        std::ffi::OsStr::new("-k"),
        std::ffi::OsStr::new(service),
    ];
    if let Err(error) = run_launchctl(runner, &kickstart) {
        let result = with_rollback_context(error, was_loaded, paths, domain, service, runner);
        if !was_loaded {
            cleanup_backups(paths);
        }
        return Err(result);
    }
    cleanup_backups(paths);
    Ok(())
}

fn with_rollback_context(
    error: io::Error,
    was_loaded: bool,
    paths: &InstallPaths,
    domain: &str,
    service: &str,
    runner: &mut impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
) -> io::Error {
    let kind = error.kind();
    let rollback = if was_loaded {
        match paths.previous_plist.as_deref() {
            Some(previous_plist) => restore_previous_service(
                &paths.binary,
                paths.previous_binary.as_deref(),
                &paths.plist,
                previous_plist,
                domain,
                service,
                runner,
            ),
            None => Err(io::Error::other("previous plist backup is unavailable")),
        }
    } else {
        Ok(())
    };
    let message = match rollback {
        Ok(()) if was_loaded => format!("{error}; previous Hub service restored"),
        Ok(()) => error.to_string(),
        Err(rollback) => format!("{error}; previous Hub service restore failed: {rollback}"),
    };
    io::Error::new(kind, message)
}

fn restore_previous_service(
    binary: &Path,
    previous_binary: Option<&Path>,
    plist: &Path,
    previous_plist: &Path,
    domain: &str,
    service: &str,
    runner: &mut impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
) -> io::Result<()> {
    let bootout = [
        std::ffi::OsStr::new("bootout"),
        std::ffi::OsStr::new(service),
    ];
    let _ = runner(&bootout)?;
    wait_for_service_unloaded_with_runner(service, runner)?;
    if let Some(previous_binary) = previous_binary {
        restore_file_from_backup(previous_binary, binary)?;
        set_mode(binary, 0o700)?;
    }
    restore_file_from_backup(previous_plist, plist)?;
    set_mode(plist, 0o600)?;
    let bootstrap = [
        std::ffi::OsStr::new("bootstrap"),
        std::ffi::OsStr::new(domain),
        plist.as_os_str(),
    ];
    run_launchctl(runner, &bootstrap)?;
    let kickstart = [
        std::ffi::OsStr::new("kickstart"),
        std::ffi::OsStr::new("-k"),
        std::ffi::OsStr::new(service),
    ];
    run_launchctl(runner, &kickstart)?;
    if let Some(previous_binary) = previous_binary {
        let _ = fs::remove_file(previous_binary);
    }
    let _ = fs::remove_file(previous_plist);
    Ok(())
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

fn real_launchctl(arguments: &[&std::ffi::OsStr]) -> io::Result<bool> {
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

fn expected_launchctl_absence(action: &str, status: Option<i32>, detail: &str) -> bool {
    (action == "print" && status == Some(113) && detail.contains("Could not find service"))
        || (action == "bootout" && status == Some(3) && detail.contains("No such process"))
}

fn restore_prepared_files(paths: &InstallPaths) -> io::Result<()> {
    restore_prepared_file(&paths.binary, paths.previous_binary.as_deref(), 0o700)?;
    restore_prepared_file(&paths.plist, paths.previous_plist.as_deref(), 0o600)?;
    cleanup_backups(paths);
    Ok(())
}

fn restore_prepared_file(destination: &Path, previous: Option<&Path>, mode: u32) -> io::Result<()> {
    if let Some(previous) = previous {
        restore_file_from_backup(previous, destination)?;
        set_mode(destination, mode)
    } else {
        match fs::remove_file(destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn cleanup_backups(paths: &InstallPaths) {
    if let Some(previous_binary) = &paths.previous_binary {
        let _ = fs::remove_file(previous_binary);
    }
    if let Some(previous_plist) = &paths.previous_plist {
        let _ = fs::remove_file(previous_plist);
    }
}

fn restore_file_from_backup(previous: &Path, destination: &Path) -> io::Result<()> {
    fs::copy(previous, destination).map(|_| ())
}

fn backup_existing_plist(plist: &Path) -> io::Result<Option<PathBuf>> {
    backup_existing_file(plist, "previous", 0o600)
}

fn backup_existing_file(path: &Path, suffix: &str, mode: u32) -> io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let backup = path.with_file_name(format!(
        ".{}.{}.{}.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        std::process::id(),
        suffix,
        nonce
    ));
    fs::copy(path, &backup)?;
    set_mode(&backup, mode)?;
    Ok(Some(backup))
}

fn render_plist(binary: &Path, config_path: &Path) -> io::Result<String> {
    let binary = xml_path(binary)?;
    let config = xml_path(config_path)?;
    Ok(PLIST_TEMPLATE
        .replace("@BINARY@", &binary)
        .replace("@CONFIG@", &config))
}

fn xml_path(path: &Path) -> io::Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn absolute_existing(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize()
}

fn copy_atomic(source: &Path, destination: &Path, mode: u32) -> io::Result<()> {
    let mut input = File::open(source)?;
    write_atomic_from(destination, mode, |output| {
        io::copy(&mut input, output).map(|_| ())
    })
}

fn write_atomic(destination: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_atomic_from(destination, mode, |output| output.write_all(bytes))
}

fn write_atomic_from(
    destination: &Path,
    mode: u32,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let name = destination
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no name"))?
        .to_string_lossy();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temporary = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), nonce));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(mode);
        let mut output = options.open(&temporary)?;
        set_mode(&temporary, mode)?;
        write(&mut output)?;
        output.sync_all()?;
        fs::rename(&temporary, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

#[cfg(test)]
#[path = "macos_launch_agent/tests.rs"]
mod tests;
