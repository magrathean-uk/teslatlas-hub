//! Small per-user LaunchAgent installer for the one Hub process.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const LABEL: &str = "com.teslatlas.hub";
const PLIST_NAME: &str = "com.teslatlas.hub.plist";
const BINARY_NAME: &str = "teslatlas-hub";
const PLIST_TEMPLATE: &str = include_str!("../packaging/com.teslatlas.hub.plist.in");

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

/// Refuse to replace a running LaunchAgent unless this data directory has one
/// configured car and a usable legacy credential pair. This runs before any
/// installer file or launchctl mutation.
pub fn preflight_hub(data_dir: &Path) -> io::Result<()> {
    let store = crate::db::HubStore::open_read_only(data_dir).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Hub data is unavailable: {error}"),
        )
    })?;
    store
        .selected_tesla_eid()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "one configured vehicle is required before install",
            )
        })?;
    let tokens = store
        .load_teslamate_legacy_tokens()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "legacy Owner API credentials are required before install",
            )
        })?;
    crate::teslamate_credentials::load_key_for_tokens(data_dir, &tokens).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("legacy Owner API credentials are unusable: {error}"),
        )
    })?;
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
    if service_is_loaded_with_runner(service, runner)? {
        return Err(io::Error::other(
            "Hub LaunchAgent is still loaded after stop",
        ));
    }
    Ok(())
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
mod tests {
    use super::*;
    use crate::{
        credentials::OwnerTokens,
        db::{HubStore, TeslaMateLegacyTokenStore},
        hub_pack::ProjectionCarSettings,
        protocol::CursorKey,
        teslamate_credentials::replace_key_and_tokens,
        teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history},
        teslamate_projection::{TeslaMateCar, TeslaMateHistory},
        teslamate_token::encrypt_legacy_owner_tokens,
    };

    fn seed_selected_car(data_dir: &Path) -> HubStore {
        let store = HubStore::initialize(data_dir).expect("store");
        let history = TeslaMateHistory {
            cars: vec![TeslaMateCar {
                id: 1,
                eid: 70,
                vid: Some(71),
                vin: Some("5YJTEST0000000001".to_owned()),
                name: Some("Install fixture".to_owned()),
                model: Some("3".to_owned()),
                trim_badging: None,
                marketing_name: None,
                exterior_color: None,
                wheel_type: None,
                spoiler_type: None,
                efficiency_wh_per_km: None,
                settings: ProjectionCarSettings::default(),
            }],
            drives: Vec::new(),
            positions: Vec::new(),
            charging_processes: Vec::new(),
            charges: Vec::new(),
            addresses: Vec::new(),
            geofences: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
        };
        publish_history(
            &store,
            &CursorKey::from_bytes([7; 32]),
            &TeslaMateImportRequest {
                source_key: "install-fixture".to_owned(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms: 1_000,
            },
            &history,
        )
        .expect("imported car");
        store
    }

    fn seed_ready_hub(data_dir: &Path) {
        let store = seed_selected_car(data_dir);
        let tokens = OwnerTokens::from_secret_parts("access".to_owned(), "refresh".to_owned())
            .expect("tokens");
        let key = b"install-fixture-key";
        let (access, refresh) = encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt");
        let stored = TeslaMateLegacyTokenStore::imported(access, refresh).expect("stored tokens");
        replace_key_and_tokens(data_dir, &store, key, &stored).expect("credentials");
    }

    fn assert_no_install_artifacts(data: &Path, home: &Path) {
        assert!(!data.join("bin").exists(), "preflight wrote a binary");
        assert!(
            !home
                .join("Library")
                .join("LaunchAgents")
                .join(PLIST_NAME)
                .exists(),
            "preflight wrote a plist"
        );
    }

    #[test]
    fn launchctl_query_failure_restores_prepared_replacement_files() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let binary = temporary.path().join("teslatlas-hub");
        let plist = temporary.path().join("com.teslatlas.hub.plist");
        let previous_binary = temporary.path().join(".teslatlas-hub.previous");
        let previous_plist = temporary.path().join(".com.teslatlas.hub.plist.previous");
        fs::write(&binary, b"new binary").expect("new binary");
        fs::write(&plist, b"new plist").expect("new plist");
        fs::write(&previous_binary, b"old binary").expect("old binary");
        fs::write(&previous_plist, b"old plist").expect("old plist");
        let paths = InstallPaths {
            binary: binary.clone(),
            plist: plist.clone(),
            previous_binary: Some(previous_binary.clone()),
            previous_plist: Some(previous_plist.clone()),
        };
        let mut calls = 0;
        let mut runner = |_: &[&std::ffi::OsStr]| {
            calls += 1;
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "launchctl query denied",
            ))
        };

        let error = launch_with_runner(&paths, "gui/501", "gui/501/com.teslatlas.hub", &mut runner)
            .expect_err("query failure must abort install");
        assert!(
            error
                .to_string()
                .contains("prepared install files restored")
        );
        assert_eq!(calls, 1);
        assert_eq!(fs::read(&binary).expect("restored binary"), b"old binary");
        assert_eq!(fs::read(&plist).expect("restored plist"), b"old plist");
        assert!(!previous_binary.exists());
        assert!(!previous_plist.exists());
    }

    #[test]
    fn launchctl_query_failure_removes_new_install_files_without_backups() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let binary = temporary.path().join("teslatlas-hub");
        let plist = temporary.path().join("com.teslatlas.hub.plist");
        fs::write(&binary, b"new binary").expect("new binary");
        fs::write(&plist, b"new plist").expect("new plist");
        let paths = InstallPaths {
            binary: binary.clone(),
            plist: plist.clone(),
            previous_binary: None,
            previous_plist: None,
        };
        let mut runner = |_: &[&std::ffi::OsStr]| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "launchctl query denied",
            ))
        };

        launch_with_runner(&paths, "gui/501", "gui/501/com.teslatlas.hub", &mut runner)
            .expect_err("query failure must abort install");
        assert!(!binary.exists());
        assert!(!plist.exists());
    }

    #[test]
    fn installed_service_start_stop_and_restart_use_bounded_launchctl_sequences() {
        fn runner(
            responses: Vec<bool>,
        ) -> (
            impl FnMut(&[&std::ffi::OsStr]) -> io::Result<bool>,
            std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        ) {
            let responses = std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::from(responses),
            ));
            let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let calls_for_runner = std::sync::Arc::clone(&calls);
            let responses_for_runner = std::sync::Arc::clone(&responses);
            let run = move |arguments: &[&std::ffi::OsStr]| {
                calls_for_runner.lock().expect("calls").push(
                    arguments
                        .iter()
                        .map(|argument| argument.to_string_lossy().into_owned())
                        .collect(),
                );
                responses_for_runner
                    .lock()
                    .expect("responses")
                    .pop_front()
                    .ok_or_else(|| io::Error::other("unexpected launchctl call"))
            };
            (run, calls)
        }

        let plist = Path::new("/private/tmp/com.teslatlas.hub.plist");
        let domain = "gui/501";
        let service = "gui/501/com.teslatlas.hub";

        let (mut start_runner, start_calls) = runner(vec![false, true, true, true]);
        start_installed_with_runner(plist, domain, service, &mut start_runner)
            .expect("start service");
        assert_eq!(
            start_calls
                .lock()
                .expect("start calls")
                .iter()
                .map(|call| call[0].as_str())
                .collect::<Vec<_>>(),
            ["print", "bootstrap", "kickstart", "print"]
        );

        let (mut stop_runner, stop_calls) = runner(vec![true, false]);
        stop_installed_with_runner(service, &mut stop_runner).expect("stop service");
        assert_eq!(
            stop_calls
                .lock()
                .expect("stop calls")
                .iter()
                .map(|call| call[0].as_str())
                .collect::<Vec<_>>(),
            ["bootout", "print"]
        );

        let (mut restart_runner, restart_calls) =
            runner(vec![true, false, false, true, true, true]);
        restart_installed_with_runner(plist, domain, service, &mut restart_runner)
            .expect("restart service");
        assert_eq!(
            restart_calls
                .lock()
                .expect("restart calls")
                .iter()
                .map(|call| call[0].as_str())
                .collect::<Vec<_>>(),
            [
                "bootout",
                "print",
                "print",
                "bootstrap",
                "kickstart",
                "print"
            ]
        );
    }

    #[test]
    fn only_known_launchctl_absence_is_nonfatal() {
        assert!(expected_launchctl_absence(
            "print",
            Some(113),
            "Could not find service"
        ));
        assert!(expected_launchctl_absence(
            "bootout",
            Some(3),
            "No such process"
        ));
        assert!(!expected_launchctl_absence(
            "print",
            Some(1),
            "permission denied"
        ));
        assert!(!expected_launchctl_absence(
            "bootout",
            Some(1),
            "permission denied"
        ));
    }

    #[test]
    fn failed_replacement_restores_loaded_service_without_launchctl() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let binary = temporary.path().join("teslatlas-hub");
        let plist = temporary.path().join("com.teslatlas.hub.plist");
        let previous_binary = temporary.path().join(".teslatlas-hub.previous");
        let previous_plist = temporary.path().join(".com.teslatlas.hub.plist.previous");
        fs::write(&binary, b"new binary").expect("new binary");
        fs::write(&plist, b"new plist").expect("new plist");
        fs::write(&previous_binary, b"old binary").expect("old binary");
        fs::write(&previous_plist, b"old plist").expect("old plist");
        let paths = InstallPaths {
            binary: binary.clone(),
            plist: plist.clone(),
            previous_binary: Some(previous_binary.clone()),
            previous_plist: Some(previous_plist.clone()),
        };
        let mut outcomes = std::collections::VecDeque::from([
            true,  // print: old service loaded
            true,  // bootout: old service
            false, // bootstrap: replacement fails
            true,  // rollback bootout
            true,  // rollback bootstrap
            true,  // rollback kickstart
        ]);
        let mut calls = Vec::new();
        let result = {
            let mut runner = |arguments: &[&std::ffi::OsStr]| {
                calls.push(
                    arguments
                        .iter()
                        .map(|argument| argument.to_string_lossy().into_owned())
                        .collect::<Vec<_>>(),
                );
                outcomes
                    .pop_front()
                    .ok_or_else(|| io::Error::other("unexpected launchctl call"))
            };
            launch_with_runner(&paths, "gui/501", "gui/501/com.teslatlas.hub", &mut runner)
        };
        assert!(
            result
                .expect_err("replacement must fail")
                .to_string()
                .contains("previous Hub service restored")
        );
        assert_eq!(fs::read(&binary).expect("restored binary"), b"old binary");
        assert_eq!(fs::read(&plist).expect("restored plist"), b"old plist");
        assert!(!previous_binary.exists());
        assert!(!previous_plist.exists());
        assert_eq!(calls[0][0], "print");
        assert_eq!(calls[0][1], "gui/501/com.teslatlas.hub");
        assert_eq!(calls[2][0], "bootstrap");
        assert_eq!(calls[2][1], "gui/501");
        assert_eq!(calls[2][2], plist.display().to_string());
        assert_eq!(calls[4][0], "bootstrap");
        assert_eq!(calls[4][1], "gui/501");
        assert_eq!(calls[4][2], plist.display().to_string());
    }

    #[test]
    fn failed_replacement_restores_loaded_plist_without_binary_backup() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let binary = temporary.path().join("teslatlas-hub");
        let plist = temporary.path().join("com.teslatlas.hub.plist");
        let previous_plist = temporary.path().join(".com.teslatlas.hub.plist.previous");
        fs::write(&binary, b"new binary").expect("new binary");
        fs::write(&plist, b"new plist").expect("new plist");
        fs::write(&previous_plist, b"old plist").expect("old plist");
        let paths = InstallPaths {
            binary: binary.clone(),
            plist: plist.clone(),
            previous_binary: None,
            previous_plist: Some(previous_plist.clone()),
        };
        let mut outcomes = std::collections::VecDeque::from([
            true,  // print: old service loaded
            true,  // bootout: old service
            true,  // bootstrap: replacement
            false, // kickstart: replacement fails
            true,  // rollback bootout
            true,  // rollback bootstrap
            true,  // rollback kickstart
        ]);
        let mut runner = |_: &[&std::ffi::OsStr]| {
            outcomes
                .pop_front()
                .ok_or_else(|| io::Error::other("unexpected launchctl call"))
        };
        let error = launch_with_runner(&paths, "gui/501", "gui/501/com.teslatlas.hub", &mut runner)
            .expect_err("replacement must fail");
        assert!(error.to_string().contains("previous Hub service restored"));
        assert_eq!(fs::read(&plist).expect("restored plist"), b"old plist");
        assert_eq!(
            fs::read(&binary).expect("new binary remains"),
            b"new binary"
        );
        assert!(!previous_plist.exists());
    }

    #[test]
    fn installs_private_binary_and_minimal_absolute_plist_without_launchctl() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let data = temporary.path().join("Hub Data");
        let home = temporary.path().join("home");
        fs::create_dir_all(&home).expect("home directory");
        let config = data.join("config.toml");
        let executable = temporary.path().join("source-bin");
        seed_ready_hub(&data);
        fs::write(&config, "[hub]\n").expect("config");
        fs::write(&executable, "binary bytes").expect("source binary");

        let paths = install_files_after_preflight(&data, &config, &home, &executable)
            .expect("install files");
        assert_eq!(
            fs::read(&paths.binary).expect("installed binary"),
            b"binary bytes"
        );
        assert_eq!(
            fs::metadata(&paths.binary)
                .expect("binary mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&paths.plist)
                .expect("plist mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let plist = fs::read_to_string(&paths.plist).expect("plist");
        assert!(plist.contains(&format!("<string>{}</string>", paths.binary.display())));
        let canonical_config = config.canonicalize().expect("canonical config");
        assert!(plist.contains(&format!("<string>{}</string>", canonical_config.display())));
        assert!(plist.contains("<string>--config</string>"));
        assert!(plist.contains("<string>serve</string>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(!plist.contains("EnvironmentVariables"));
        assert!(!plist.contains("ResourceLimits"));
        assert!(!plist.contains("SERVICE_WRAPPER"));
        assert!(plist.contains("<key>ProcessType</key>\n  <string>Background</string>"));
        assert!(plist.contains("<key>Umask</key>\n  <integer>63</integer>"));
    }

    #[test]
    fn empty_hub_preflight_creates_no_install_artifacts() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let data = temporary.path().join("data");
        let home = temporary.path().join("home");
        fs::create_dir_all(&home).expect("home directory");
        let config = data.join("config.toml");
        let executable = temporary.path().join("source-bin");
        HubStore::initialize(&data).expect("empty store");
        fs::write(&config, "[hub]\n").expect("config");
        fs::write(&executable, "binary bytes").expect("source binary");

        assert!(install_files_after_preflight(&data, &config, &home, &executable).is_err());
        assert_no_install_artifacts(&data, &home);
    }

    #[test]
    fn selected_car_without_token_creates_no_install_artifacts() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let data = temporary.path().join("data");
        let home = temporary.path().join("home");
        fs::create_dir_all(&home).expect("home directory");
        let config = data.join("config.toml");
        let executable = temporary.path().join("source-bin");
        let _store = seed_selected_car(&data);
        fs::write(&config, "[hub]\n").expect("config");
        fs::write(&executable, "binary bytes").expect("source binary");

        assert!(install_files_after_preflight(&data, &config, &home, &executable).is_err());
        assert_no_install_artifacts(&data, &home);
    }
}
