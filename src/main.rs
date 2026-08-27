use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{ExitCode, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::future::Future;

use clap::{Parser, Subcommand};
use qrcode::{QrCode, render::unicode::Dense1x2};
use rustix::fs::{FileType, Mode, OFlags, fcntl_getfl, fcntl_setfl, fstat, open};
use rustix::process::getuid;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use teslatlas_hub::hub_user_process::AdmittedUserHub;
#[cfg(unix)]
use teslatlas_hub::protocol::CursorKey;
#[cfg(unix)]
use teslatlas_hub::teslamate_import::derive_effective_import_profile;
use teslatlas_hub::{
    collector,
    config::{CollectorProvider, HubConfig},
    credential_recovery::{RECOVERY_ENCRYPTION_KEY_BYTES, export_credentials, restore_credentials},
    credentials::{OwnerTokens, TeslaMatePostgresPassword},
    data_recovery::{create_data_backup, restore_data_backup, verify_data_backup},
    db::{HubStore, ObservationVerificationError, StoreError, TeslaMateLegacyTokenStore},
    diagnostics::{inspect_hub, log_runtime_inventory},
    fleet_api::FleetRegion,
    fleet_credentials::{
        FleetCredentialError, FleetSetupCredentials, migrate_legacy_fleet_credentials,
        persist_fleet_setup_credentials, remove_fleet_key_and_tokens, stored_fleet_scope_summary,
        validate_stored_fleet_credentials,
    },
    gpx::export_drive_gpx,
    hub_pack::GeofenceBillingType,
    owner_api::LegacyVehicleAction,
    server,
    teslamate::ReadOnlySource,
    teslamate_credentials::{
        load_or_create_cursor_key, random_encryption_key, remove_key_and_tokens,
        replace_key_and_tokens,
    },
    teslamate_import::{
        TeslaMateImportReport, TeslaMateImportRequest, TeslaMateImportScope,
        import_selected_from_postgres_with_schema_22,
        import_selected_from_postgres_with_schema_22_and_legacy_token,
    },
    teslamate_reader::{
        TeslaMateCheckSnapshot, TeslaMateLegacyTokenCiphertexts, TeslaMateReadLimits,
        TeslaMateReaderError, check_teslamate_compatibility,
    },
    teslamate_schema::{
        MAX_VALIDATED_MIGRATION, SchemaCompatibilityError, TESLAMATE_V4_MIGRATION_COUNT,
        TESLAMATE_V4_SOURCE_REVISION,
    },
    teslamate_token::{
        decrypt_legacy_owner_tokens, encrypt_legacy_owner_token_files, encrypt_legacy_owner_tokens,
    },
    teslamate_writeback::{TeslaMateCost, write_back_charge_cost},
};
use tracing_subscriber::EnvFilter;

#[cfg(unix)]
use rustix::io::Errno;

const IMMUTABLE_DIAGNOSTIC_ATTEMPTS: usize = 2;
const IMMUTABLE_DIAGNOSTIC_OPEN_ATTEMPTS: usize = 21;
#[cfg(not(test))]
const IMMUTABLE_DIAGNOSTIC_OPEN_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
const IMMUTABLE_DIAGNOSTIC_OPEN_DELAY: Duration = Duration::ZERO;

fn retryable_immutable_diagnostic_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error.downcast_ref::<StoreError>().is_some_and(|error| {
        matches!(
            error,
            StoreError::PendingCatalogueWal | StoreError::CatalogueChangedDuringImmutableCheck
        )
    })
}

fn run_immutable_diagnostic<T>(
    data_dir: &Path,
    operation: impl FnMut(&HubStore) -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    run_immutable_diagnostic_with(data_dir, operation, || {
        std::thread::sleep(IMMUTABLE_DIAGNOSTIC_OPEN_DELAY);
    })
}

fn run_immutable_diagnostic_with<T>(
    data_dir: &Path,
    mut operation: impl FnMut(&HubStore) -> Result<T, Box<dyn std::error::Error>>,
    mut wait: impl FnMut(),
) -> Result<T, Box<dyn std::error::Error>> {
    for diagnostic_attempt in 0..IMMUTABLE_DIAGNOSTIC_ATTEMPTS {
        let mut open_result = None;
        for open_attempt in 0..IMMUTABLE_DIAGNOSTIC_OPEN_ATTEMPTS {
            match HubStore::open_immutable_read_only(data_dir) {
                Err(StoreError::PendingCatalogueWal)
                    if open_attempt + 1 < IMMUTABLE_DIAGNOSTIC_OPEN_ATTEMPTS =>
                {
                    wait();
                }
                result => {
                    open_result = Some(result);
                    break;
                }
            }
        }
        let store = match open_result.expect("bounded immutable catalogue open loop completes") {
            Ok(store) => store,
            Err(error)
                if matches!(
                    error,
                    StoreError::PendingCatalogueWal
                        | StoreError::CatalogueChangedDuringImmutableCheck
                ) && diagnostic_attempt + 1 < IMMUTABLE_DIAGNOSTIC_ATTEMPTS =>
            {
                wait();
                continue;
            }
            Err(error) => return Err(Box::new(error)),
        };
        let result = operation(&store).and_then(|output| {
            store.verify_immutable_snapshot_unchanged()?;
            Ok(output)
        });
        match result {
            Err(error)
                if retryable_immutable_diagnostic_error(error.as_ref())
                    && diagnostic_attempt + 1 < IMMUTABLE_DIAGNOSTIC_ATTEMPTS =>
            {
                wait();
            }
            result => return result,
        }
    }
    unreachable!("bounded immutable diagnostic loop always returns")
}

/// A worker owned by the Unix Serve supervisor. Normal exits request
/// shutdown and await the task; cancellation of the supervisor aborts the
/// owned task rather than silently detaching it.
#[cfg(unix)]
struct MacServeWorker {
    label: &'static str,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

// Unit tests use a short stop bound so a non-cooperative fake worker can prove
// the abort path without a real wait.
#[cfg(all(unix, not(test)))]
const MACOS_SERVE_STOP_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(all(unix, test))]
const MACOS_SERVE_STOP_TIMEOUT: Duration = Duration::from_millis(50);

#[cfg(unix)]
#[derive(Debug)]
struct MacServeWorkerStopTimeout {
    label: &'static str,
}

#[cfg(unix)]
impl std::fmt::Display for MacServeWorkerStopTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Hub {} worker did not stop within {} milliseconds",
            self.label,
            MACOS_SERVE_STOP_TIMEOUT.as_millis()
        )
    }
}

#[cfg(unix)]
impl std::error::Error for MacServeWorkerStopTimeout {}

#[cfg(unix)]
impl MacServeWorker {
    fn start<F>(label: &'static str, shutdown: tokio::sync::oneshot::Sender<()>, future: F) -> Self
    where
        F: Future<Output = std::io::Result<()>> + Send + 'static,
    {
        Self {
            label,
            shutdown: Some(shutdown),
            task: tokio::spawn(future),
        }
    }

    fn request_stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    async fn wait(&mut self) -> std::io::Result<()> {
        let result = (&mut self.task).await;
        self.join_result(result)
    }

    async fn stop_and_wait(&mut self) -> std::io::Result<()> {
        self.request_stop();
        match tokio::time::timeout(MACOS_SERVE_STOP_TIMEOUT, &mut self.task).await {
            Ok(result) => self.join_result(result),
            Err(_) => {
                self.task.abort();
                let _ = (&mut self.task).await;
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    MacServeWorkerStopTimeout { label: self.label },
                ))
            }
        }
    }

    fn join_result(
        &self,
        result: Result<std::io::Result<()>, tokio::task::JoinError>,
    ) -> std::io::Result<()> {
        result.map_err(|error| {
            std::io::Error::other(format!("Hub {} worker task failed: {error}", self.label))
        })?
    }
}

#[cfg(unix)]
impl Drop for MacServeWorker {
    fn drop(&mut self) {
        self.request_stop();
        self.task.abort();
    }
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct MacCommandProxySpec {
    executable: PathBuf,
    host: String,
    port: u16,
    command_key: PathBuf,
    certificate: PathBuf,
    tls_key: PathBuf,
    session_cache: PathBuf,
}

#[cfg(unix)]
impl MacCommandProxySpec {
    fn arguments(&self) -> Vec<String> {
        vec![
            "-host".to_owned(),
            self.host.clone(),
            "-port".to_owned(),
            self.port.to_string(),
            "-key-file".to_owned(),
            self.command_key.to_string_lossy().into_owned(),
            "-cert".to_owned(),
            self.certificate.to_string_lossy().into_owned(),
            "-tls-key".to_owned(),
            self.tls_key.to_string_lossy().into_owned(),
            "-session-cache".to_owned(),
            self.session_cache.to_string_lossy().into_owned(),
        ]
    }
}

#[cfg(unix)]
struct MacCommandProxy {
    child: tokio::process::Child,
    address: std::net::SocketAddr,
}

#[cfg(unix)]
impl MacCommandProxy {
    async fn start(spec: MacCommandProxySpec) -> std::io::Result<Self> {
        let mut command = tokio::process::Command::new(&spec.executable);
        command
            .args(spec.arguments())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let child = command.spawn()?;
        let address_host = if spec.host == "localhost" {
            "127.0.0.1"
        } else {
            spec.host.as_str()
        };
        let address = std::net::SocketAddr::new(
            address_host
                .parse()
                .map_err(|_| std::io::Error::other("Fleet command proxy host is invalid"))?,
            spec.port,
        );
        Ok(Self { child, address })
    }

    async fn wait_ready(&mut self) -> std::io::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Err(std::io::Error::other(format!(
                    "Tesla command proxy exited before readiness: {status}"
                )));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Tesla command proxy did not become ready",
                ));
            }
            if let Ok(Ok(stream)) = tokio::time::timeout(
                Duration::from_millis(200),
                tokio::net::TcpStream::connect(self.address),
            )
            .await
            {
                drop(stream);
                return Ok(());
            }
        }
    }

    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    async fn stop(&mut self) -> std::io::Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.start_kill()?;
        }
        tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Tesla command proxy did not stop",
                )
            })?
            .map(|_| ())
    }
}

#[cfg(unix)]
enum MacServeControl {
    Shutdown,
    AdmissionInvalidated(std::io::Error),
}

#[cfg(unix)]
enum MacServeActiveOutcome {
    Server(std::io::Result<()>),
    Collector(std::io::Result<()>),
    Control(MacServeControl),
}

#[cfg(unix)]
fn is_macos_serve_stop_timeout(result: &std::io::Result<()>) -> bool {
    matches!(
        result,
        Err(error)
            if error.kind() == std::io::ErrorKind::TimedOut
                && error
                    .get_ref()
                    .is_some_and(|source| source.is::<MacServeWorkerStopTimeout>())
    )
}

#[cfg(unix)]
fn preserve_active_result_after_stop(
    primary: std::io::Result<()>,
    stop_result: std::io::Result<()>,
) -> std::io::Result<()> {
    if is_macos_serve_stop_timeout(&stop_result) {
        stop_result
    } else {
        primary
    }
}

/// Own the Unix process's collector and listener as one cancellation-safe
/// lifecycle.  The collector is constructed only for a positive cadence; the
/// listener is constructed only after the collector hands over its exact
/// cursor key. This accepts factories so ordering and exit can be tested
/// without real network work.
#[cfg(unix)]
async fn run_macos_serve_supervisor<C, S, CF, SF, Control>(
    collector_enabled: bool,
    collector_start: CF,
    server_start: SF,
    control: Control,
) -> std::io::Result<()>
where
    C: Future<Output = std::io::Result<()>> + Send + 'static,
    S: Future<Output = std::io::Result<()>> + Send + 'static,
    CF: FnOnce(tokio::sync::oneshot::Sender<CursorKey>, tokio::sync::oneshot::Receiver<()>) -> C,
    SF: FnOnce(Option<CursorKey>, tokio::sync::oneshot::Receiver<()>) -> S,
    Control: Future<Output = MacServeControl>,
{
    tokio::pin!(control);

    // A collector-disabled runtime has no legacy Owner collector or Tesla
    // client construction path. The server may still use its configured TLS
    // cursor credential.
    if !collector_enabled {
        let (server_shutdown_tx, server_shutdown_rx) = tokio::sync::oneshot::channel();
        let mut server = MacServeWorker::start(
            "server",
            server_shutdown_tx,
            server_start(None, server_shutdown_rx),
        );
        return tokio::select! {
            result = server.wait() => result,
            control = &mut control => {
                let server_result = server.stop_and_wait().await;
                match control {
                    MacServeControl::Shutdown => server_result,
                    MacServeControl::AdmissionInvalidated(error) => {
                        if is_macos_serve_stop_timeout(&server_result) {
                            server_result
                        } else {
                            Err(error)
                        }
                    }
                }
            }
        };
    }

    let (collector_shutdown_tx, collector_shutdown_rx) = tokio::sync::oneshot::channel();
    let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
    let mut collector = MacServeWorker::start(
        "collector",
        collector_shutdown_tx,
        collector_start(ready_tx, collector_shutdown_rx),
    );

    // The listener stays unconstructed until the collector completes its
    // startup custody and hands back the very cursor key it will use.
    let cursor_key = tokio::select! {
        received = &mut ready_rx => match received {
            Ok(cursor_key) => cursor_key,
            Err(_) => match collector.stop_and_wait().await {
                Ok(()) => return Err(std::io::Error::other("macOS collector exited before readiness")),
                Err(error) => return Err(error),
            },
        },
        result = collector.wait() => match result {
            Ok(()) => return Err(std::io::Error::other("macOS collector exited before readiness")),
            Err(error) => return Err(error),
        },
        control = &mut control => {
            let stop_result = collector.stop_and_wait().await;
            return match control {
                MacServeControl::Shutdown => {
                    if is_macos_serve_stop_timeout(&stop_result) {
                        stop_result
                    } else {
                        Ok(())
                    }
                }
                MacServeControl::AdmissionInvalidated(error) => {
                    if is_macos_serve_stop_timeout(&stop_result) {
                        stop_result
                    } else {
                        Err(error)
                    }
                }
            };
        }
    };

    let (server_shutdown_tx, server_shutdown_rx) = tokio::sync::oneshot::channel();
    let mut server = MacServeWorker::start(
        "server",
        server_shutdown_tx,
        server_start(Some(cursor_key), server_shutdown_rx),
    );

    let outcome = tokio::select! {
        result = server.wait() => MacServeActiveOutcome::Server(result),
        result = collector.wait() => MacServeActiveOutcome::Collector(result),
        control = &mut control => MacServeActiveOutcome::Control(control),
    };

    match outcome {
        MacServeActiveOutcome::Server(result) => {
            server.request_stop();
            let collector_stop_result = collector.stop_and_wait().await;
            preserve_active_result_after_stop(result, collector_stop_result)
        }
        MacServeActiveOutcome::Collector(result) => {
            collector.request_stop();
            let server_stop_result = server.stop_and_wait().await;
            let collector_result = match result {
                Ok(()) => Err(std::io::Error::other(
                    "macOS collector exited while Serve was active",
                )),
                Err(error) => Err(error),
            };
            preserve_active_result_after_stop(collector_result, server_stop_result)
        }
        MacServeActiveOutcome::Control(control) => {
            server.request_stop();
            collector.request_stop();
            let server_result = server.stop_and_wait().await;
            let collector_result = collector.stop_and_wait().await;
            if is_macos_serve_stop_timeout(&server_result) {
                return server_result;
            }
            if is_macos_serve_stop_timeout(&collector_result) {
                return collector_result;
            }
            match control {
                MacServeControl::AdmissionInvalidated(error) => Err(error),
                MacServeControl::Shutdown => match collector_result {
                    Ok(()) => server_result,
                    Err(error) => Err(error),
                },
            }
        }
    }
}

#[cfg(unix)]
async fn run_macos_serve_with_optional_proxy<C, S, CF, SF, Control>(
    proxy: Option<MacCommandProxySpec>,
    collector_enabled: bool,
    collector_start: CF,
    server_start: SF,
    control: Control,
) -> std::io::Result<()>
where
    C: Future<Output = std::io::Result<()>> + Send + 'static,
    S: Future<Output = std::io::Result<()>> + Send + 'static,
    CF: FnOnce(tokio::sync::oneshot::Sender<CursorKey>, tokio::sync::oneshot::Receiver<()>) -> C,
    SF: FnOnce(Option<CursorKey>, tokio::sync::oneshot::Receiver<()>) -> S,
    Control: Future<Output = MacServeControl>,
{
    let Some(spec) = proxy else {
        return run_macos_serve_supervisor(
            collector_enabled,
            collector_start,
            server_start,
            control,
        )
        .await;
    };

    let mut proxy = MacCommandProxy::start(spec).await?;
    if let Err(error) = proxy.wait_ready().await {
        let _ = proxy.stop().await;
        return Err(error);
    }

    let serve =
        run_macos_serve_supervisor(collector_enabled, collector_start, server_start, control);
    tokio::pin!(serve);
    let result = tokio::select! {
        result = &mut serve => result,
        result = proxy.wait() => match result {
            Ok(status) => Err(std::io::Error::other(format!(
                "Tesla command proxy exited while Serve was active: {status}"
            ))),
            Err(error) => Err(std::io::Error::other(format!(
                "Tesla command proxy wait failed: {error}"
            ))),
        },
    };
    let stop_result = proxy.stop().await;
    match (result, stop_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), _) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn mac_command_proxy_spec(
    config: &HubConfig,
) -> Result<Option<MacCommandProxySpec>, Box<dyn std::error::Error>> {
    if config.collector.provider != CollectorProvider::Fleet {
        return Ok(None);
    }
    let Some(endpoint) = config.collector.fleet_command_proxy_url.as_deref() else {
        return Ok(None);
    };
    let url = url::Url::parse(endpoint).map_err(|_| "Fleet command proxy URL cannot be parsed")?;
    let host = url
        .host_str()
        .ok_or("Fleet command proxy URL has no host")?;
    let is_loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false);
    if url.scheme() != "https"
        || !is_loopback
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("Fleet command proxy URL is not a plain loopback HTTPS root".into());
    }
    let port = url.port().unwrap_or(443);
    let address_host = if host == "localhost" {
        "127.0.0.1"
    } else {
        host
    };
    let _: std::net::SocketAddr = std::net::SocketAddr::new(
        address_host
            .parse()
            .map_err(|_| "Fleet command proxy loopback address is invalid")?,
        port,
    );

    let executable = std::env::current_exe()?
        .parent()
        .ok_or("Hub executable has no parent directory")?
        .join("tesla-http-proxy");
    require_proxy_executable(&executable)?;
    let secrets = config.data_dir.join("secrets");
    let command_key = secrets.join("fleet-command-key.pem");
    let tls_key = secrets.join("fleet-proxy-tls-key.pem");
    require_proxy_private_file(&command_key, "Fleet command key")?;
    require_proxy_private_file(&tls_key, "Fleet proxy TLS key")?;
    let certificate = config
        .collector
        .fleet_command_proxy_root_certificate_path
        .clone()
        .ok_or("Fleet command proxy root certificate is not configured")?;
    require_proxy_regular_file(&certificate, "Fleet proxy TLS certificate")?;

    Ok(Some(MacCommandProxySpec {
        executable,
        host: host.to_owned(),
        port,
        command_key,
        certificate,
        tls_key,
        session_cache: config.data_dir.join("fleet-command-session-cache.json"),
    }))
}

#[cfg(target_os = "macos")]
fn require_proxy_executable(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| format!("Tesla command proxy is missing: {}", path.display()))?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || metadata.mode() & 0o111 == 0
    {
        return Err(format!(
            "Tesla command proxy is not a safe executable: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_proxy_private_file(path: &Path, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    require_proxy_regular_file(path, label)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.mode() & 0o077 != 0 || metadata.uid() != getuid().as_raw() {
        return Err(format!("{label} has unsafe ownership or permissions").into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_proxy_regular_file(path: &Path, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| format!("{label} is missing: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!("{label} is not a regular file: {}", path.display()).into());
    }
    Ok(())
}

#[derive(Debug, Parser)]
#[command(
    name = "teslatlas-hub",
    version,
    about = "Native Teslatlas Hub service"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[cfg(unix)]
#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Print the installed service state.
    Status,
    /// Start the installed service.
    Start,
    /// Stop the installed service.
    Stop,
    /// Stop and start the installed service.
    Restart,
}

#[derive(Debug, Subcommand)]
enum ControlCommand {
    /// List paired devices without exposing bearer material.
    #[command(name = "paired-devices")]
    PairedDevices,
    /// Revoke one paired device bearer immediately.
    #[command(name = "revoke-device")]
    RevokeDevice { device_id: uuid::Uuid },
    /// Show or update the selected car's collection settings.
    Settings {
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long)]
        streaming: Option<bool>,
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        suspend_after_idle_min: Option<i64>,
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        suspend_min: Option<i64>,
        #[arg(long)]
        require_locked: Option<bool>,
        #[arg(long)]
        free_supercharging: Option<bool>,
        #[arg(long)]
        lfp_battery: Option<bool>,
    },
    /// Pause Owner API and streaming collection for the selected car.
    Pause,
    /// Resume Owner API and streaming collection for the selected car.
    Resume,
    /// Stop collection and remove the persisted Tesla account credentials.
    #[command(name = "sign-out")]
    SignOut,
    /// Explicitly wake the selected car once.
    Wake {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Start climate control once.
    #[command(name = "climate-start")]
    ClimateStart {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Stop climate control once.
    #[command(name = "climate-stop")]
    ClimateStop {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Start charging once.
    #[command(name = "charge-start")]
    ChargeStart {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Stop charging once.
    #[command(name = "charge-stop")]
    ChargeStop {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Set the charging limit percentage once.
    #[command(name = "set-charge-limit")]
    SetChargeLimit {
        #[arg(long, value_parser = clap::value_parser!(u8).range(50..=100))]
        percent: u8,
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Lock the selected car once.
    Lock {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Unlock the selected car once.
    Unlock {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Flash the selected car's lights once.
    #[command(name = "flash-lights")]
    FlashLights {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Honk the selected car's horn once.
    #[command(name = "honk-horn")]
    HonkHorn {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// List configured geofences.
    Geofences,
    /// Create a geofence, or replace one by id.
    #[command(name = "set-geofence")]
    SetGeofence {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        id: Option<i64>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        latitude: f64,
        #[arg(long)]
        longitude: f64,
        #[arg(long, default_value_t = 20.0)]
        radius_m: f64,
        #[arg(long, default_value = "per_kwh")]
        billing_type: String,
        #[arg(long)]
        cost_per_unit: Option<f64>,
        #[arg(long)]
        session_fee: Option<f64>,
        /// Fill missing costs for matching historical charges after saving.
        #[arg(long)]
        recalculate_missing_costs: bool,
    },
    /// Delete a geofence by id.
    #[command(name = "delete-geofence")]
    DeleteGeofence {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        id: i64,
    },
    /// Replace the total cost of one completed charging session.
    #[command(name = "set-charge-cost")]
    SetChargeCost {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        charge_id: i64,
        #[arg(long)]
        cost: f64,
        #[arg(long, default_value = "total")]
        mode: String,
    },
    /// Write one completed drive as TeslaMate-compatible GPX to stdout.
    #[command(name = "export-gpx")]
    ExportGpx {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        drive_id: i64,
    },
}

#[derive(Debug, Subcommand)]
enum WriteBackCommand {
    /// Copy one total cost into a TeslaMate charging process.
    #[command(name = "charge-cost")]
    ChargeCost {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        charging_process_id: i64,
        /// Exact numeric(14,2) value, for example 12.34.
        #[arg(long)]
        cost: TeslaMateCost,
        /// Commit. Omit for a locked-row dry run and rollback.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print licence, source, and independence notices.
    Legal,
    /// Initialize or migrate the local Hub database.
    Init,
    /// Create the configured local store for a packaged Linux installation.
    #[cfg(unix)]
    Bootstrap,
    /// Configure one or every vehicle directly from private Owner tokens.
    #[cfg(unix)]
    Setup {
        /// Legacy Owner API access-token file, or `-` for stdin.
        #[arg(
            long,
            required_unless_present = "tokens_stdin",
            requires = "refresh_token_file",
            conflicts_with = "tokens_stdin"
        )]
        access_token_file: Option<PathBuf>,
        /// Legacy Owner API refresh-token file, or `-` for stdin.
        #[arg(
            long,
            required_unless_present = "tokens_stdin",
            requires = "access_token_file",
            conflicts_with = "tokens_stdin"
        )]
        refresh_token_file: Option<PathBuf>,
        /// Read one bounded {accessToken,refreshToken} JSON object from stdin.
        #[arg(long, conflicts_with_all = ["access_token_file", "refresh_token_file"])]
        tokens_stdin: bool,
        /// Tesla Owner API vehicle id. Required only when discovery finds multiple vehicles.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..), conflicts_with = "all_vehicles")]
        vehicle_id: Option<i64>,
        /// Configure every vehicle returned by the Tesla account.
        #[arg(long)]
        all_vehicles: bool,
    },
    /// Configure one or every Fleet API vehicle from bounded JSON read only from stdin.
    #[cfg(unix)]
    #[command(name = "setup-fleet")]
    SetupFleet {
        /// Tesla Fleet API vehicle id. Required only when discovery finds multiple vehicles.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..), conflicts_with = "all_vehicles")]
        vehicle_id: Option<i64>,
        /// Configure every vehicle returned by the Tesla account.
        #[arg(long)]
        all_vehicles: bool,
    },
    /// Install the fixed low-cost native Fleet Telemetry policy on configured vehicles.
    #[cfg(unix)]
    #[command(name = "configure-fleet-telemetry")]
    ConfigureFleetTelemetry,
    /// Full read-only check of the Hub database, stored Tesla credentials, TLS, and collector readiness.
    Doctor,
    /// Print the redacted local status consumed by the native control app.
    Status,
    /// Validate one TeslaMate source, connection, and selected-car inventory without mutating TeslaMate or Hub.
    #[cfg(unix)]
    #[command(name = "teslamate-check")]
    TeslaMateCheck {
        /// Password-free PostgreSQL URL. Password is read from a file or stdin.
        #[arg(long)]
        source: String,
        /// Positive TeslaMate car id that will be imported.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        car_id: i64,
        /// PostgreSQL password file, or `-` for stdin.
        #[arg(long)]
        postgres_password_file: PathBuf,
    },
    /// Validate that one configured car and its credentials are ready to serve.
    #[cfg(unix)]
    Preflight,
    /// Capture the current durable observation watermark for one source car.
    #[command(name = "observation-watermark")]
    ObservationWatermark {
        /// Source/import car id selected for the cutover.
        #[arg(long)]
        car_id: i64,
    },
    /// Verify that a strictly newer observation is durably committed.
    #[command(name = "verify-observation")]
    VerifyObservation {
        /// Source/import car id selected for the cutover.
        #[arg(long)]
        car_id: i64,
        /// Observation id returned by observation-watermark.
        #[arg(long)]
        watermark: i64,
    },
    /// Start the Hub HTTP service.
    Serve,
    /// Observe one admitted car for a bounded period without installing a service.
    #[cfg(unix)]
    Observe {
        /// Observation duration in seconds.
        #[arg(long, default_value_t = 3_600, value_parser = clap::value_parser!(u64).range(1..))]
        duration_seconds: u64,
    },
    /// Install and start the minimal per-user macOS Hub LaunchAgent.
    #[cfg(target_os = "macos")]
    Install,
    /// Control the installed Hub service.
    #[cfg(unix)]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Inspect or change configured cars while the service is running.
    Control {
        /// Hub vehicle UUID. Optional only when exactly one car is published.
        #[arg(long)]
        vehicle_id: Option<uuid::Uuid>,
        #[command(subcommand)]
        command: ControlCommand,
    },
    /// Import one TeslaMate car, its history, and its opaque legacy token pair.
    #[cfg(unix)]
    Migrate {
        /// Password-free PostgreSQL URL. Password is read from a file or stdin.
        #[arg(long)]
        source: String,
        /// Positive TeslaMate car id to import.
        #[arg(long)]
        car_id: i64,
        /// PostgreSQL password file, or `-` for stdin.
        #[arg(long)]
        postgres_password_file: PathBuf,
        /// TeslaMate ENCRYPTION_KEY file, or `-` for stdin. Omit only with both fresh-token files.
        #[arg(long)]
        encryption_key_file: Option<PathBuf>,
        /// Fresh legacy access-token file when the TeslaMate key is unavailable.
        #[arg(
            long,
            requires = "refresh_token_file",
            conflicts_with = "encryption_key_file"
        )]
        access_token_file: Option<PathBuf>,
        /// Fresh legacy refresh-token file when the TeslaMate key is unavailable.
        #[arg(
            long,
            requires = "access_token_file",
            conflicts_with = "encryption_key_file"
        )]
        refresh_token_file: Option<PathBuf>,
        /// Take one live read-only snapshot, never prompt for cutover, and leave Hub stopped.
        #[arg(long)]
        online_snapshot: bool,
    },
    /// Explicit allow-listed write-back to TeslaMate PostgreSQL.
    #[cfg(unix)]
    WriteBack {
        /// Password-free PostgreSQL URL. Password is read from a file or stdin.
        #[arg(long)]
        source: String,
        /// Positive TeslaMate car id owning the target row.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        car_id: i64,
        /// PostgreSQL password file, or `-` for stdin.
        #[arg(long)]
        postgres_password_file: PathBuf,
        #[command(subcommand)]
        command: WriteBackCommand,
    },
    /// Display one short-lived, single-use device pairing QR.
    #[command(alias = "create-pairing")]
    Pair {
        /// Human label shown only to the Hub owner, such as "Bolyki iPhone".
        #[arg(long, default_value = "Teslatlas iPhone")]
        label: String,
        /// Lifetime before the invitation becomes unusable.
        #[arg(long, default_value_t = 900)]
        expires_in_seconds: u64,
        /// Print the secret-bearing machine-readable payload for explicit debugging.
        #[arg(long, hide = true)]
        json: bool,
    },
    /// Validate database integrity, report quarantined sessions, and clean orphaned packs.
    Repair,
    /// Create a data-only catalogue and immutable-pack backup generation.
    Backup {
        /// New private directory that will contain the data-backup generation.
        #[arg(long)]
        destination: PathBuf,
    },
    /// Immutably verify one completed data-backup generation.
    #[command(name = "verify-backup")]
    VerifyBackup {
        /// Existing private data-backup generation to verify without mutation.
        #[arg(long)]
        source: PathBuf,
    },
    /// Restore data and the pairing database into a new private directory.
    #[command(name = "restore-data")]
    RestoreData {
        /// Existing verified data-backup generation.
        #[arg(long)]
        source: PathBuf,
        /// New Hub data directory. Credentials and collector authority stay absent.
        #[arg(long)]
        destination: PathBuf,
    },
    /// Export decryption/signing keys into a separately encrypted, secret-bearing file.
    #[command(name = "export-recovery-credentials")]
    ExportRecoveryCredentials {
        /// New encrypted recovery file. Never store beside the default data backup.
        #[arg(long)]
        destination: PathBuf,
        /// Owned mode-0600 file containing exactly 32 random bytes.
        #[arg(long)]
        recovery_key_file: PathBuf,
    },
    /// Restore a separately encrypted key export into a data-only restore.
    #[command(name = "restore-recovery-credentials")]
    RestoreRecoveryCredentials {
        /// Existing encrypted, secret-bearing recovery file.
        #[arg(long)]
        source: PathBuf,
        /// Owned mode-0600 file containing the exact 32-byte recovery key.
        #[arg(long)]
        recovery_key_file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("teslatlas-hub: {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn control_target(
    store: &HubStore,
    requested_vehicle_id: Option<uuid::Uuid>,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let vehicles = store.published_vehicles()?;
    let vehicle_id = match requested_vehicle_id {
        Some(vehicle_id)
            if vehicles
                .iter()
                .any(|vehicle| vehicle.vehicle_id == vehicle_id) =>
        {
            vehicle_id
        }
        Some(_) => return Err("--vehicle-id does not identify a published car".into()),
        None if vehicles.len() == 1 => vehicles[0].vehicle_id,
        None => return Err("control command requires --vehicle-id with multiple cars".into()),
    };
    store.v2_projection_binding(vehicle_id)?;
    Ok(vehicle_id)
}

fn explicit_vehicle_action(
    command: &ControlCommand,
) -> Result<Option<LegacyVehicleAction>, Box<dyn std::error::Error>> {
    let (confirmed, action) = match command {
        ControlCommand::Wake { confirm } => (*confirm, LegacyVehicleAction::Wake),
        ControlCommand::ClimateStart { confirm } => (*confirm, LegacyVehicleAction::ClimateStart),
        ControlCommand::ClimateStop { confirm } => (*confirm, LegacyVehicleAction::ClimateStop),
        ControlCommand::ChargeStart { confirm } => (*confirm, LegacyVehicleAction::ChargeStart),
        ControlCommand::ChargeStop { confirm } => (*confirm, LegacyVehicleAction::ChargeStop),
        ControlCommand::SetChargeLimit { percent, confirm } => {
            (*confirm, LegacyVehicleAction::SetChargeLimit(*percent))
        }
        ControlCommand::Lock { confirm } => (*confirm, LegacyVehicleAction::Lock),
        ControlCommand::Unlock { confirm } => (*confirm, LegacyVehicleAction::Unlock),
        ControlCommand::FlashLights { confirm } => (*confirm, LegacyVehicleAction::FlashLights),
        ControlCommand::HonkHorn { confirm } => (*confirm, LegacyVehicleAction::HonkHorn),
        _ => return Ok(None),
    };
    if !confirmed {
        return Err("vehicle action requires --confirm".into());
    }
    Ok(Some(action))
}

fn validate_streaming_setting(
    provider: CollectorProvider,
    streaming: Option<bool>,
) -> Result<(), &'static str> {
    if provider == CollectorProvider::Fleet && streaming == Some(true) {
        return Err("Fleet provider does not support legacy streaming");
    }
    Ok(())
}

#[cfg(unix)]
fn validate_legacy_setup_provider(provider: CollectorProvider) -> Result<(), &'static str> {
    if provider != CollectorProvider::Legacy {
        return Err("setup requires collector.provider = \"legacy\"");
    }
    Ok(())
}

fn clear_provider_credentials(
    data_dir: &Path,
    store: &HubStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut failures = Vec::new();
    if let Err(error) = remove_fleet_key_and_tokens(data_dir, store) {
        failures.push(format!("Fleet credentials: {error}"));
    }
    if let Err(error) = remove_key_and_tokens(data_dir, store) {
        failures.push(format!("Legacy credentials: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "provider credential removal incomplete: {}",
            failures.join("; ")
        ))
        .into())
    }
}

fn persist_legacy_setup_and_drop_fleet(
    data_dir: &Path,
    store: &HubStore,
    tokens: &OwnerTokens,
) -> Result<(), Box<dyn std::error::Error>> {
    let encryption_key = random_encryption_key();
    let (access, refresh) = encrypt_legacy_owner_tokens(&encryption_key, tokens)?;
    let stored = TeslaMateLegacyTokenStore::imported(access, refresh)?;
    replace_key_and_tokens(data_dir, store, &encryption_key, &stored).map_err(|error| {
        provider_switch_outcome_ambiguous("persisting Legacy credentials", error)
    })?;
    remove_fleet_key_and_tokens(data_dir, store).map_err(|error| {
        provider_switch_outcome_ambiguous("removing previous Fleet credentials", error)
    })?;
    Ok(())
}

/// Copy TeslaMate Owner tokens into Hub. Import never writes TeslaMate
/// PostgreSQL and never deletes Fleet credentials — those stay until an
/// explicit `setup` / `setup-fleet` / `sign-out`.
fn persist_migrated_legacy_tokens(
    data_dir: &Path,
    store: &HubStore,
    encryption_key: &[u8],
    stored: &TeslaMateLegacyTokenStore,
) -> Result<(), Box<dyn std::error::Error>> {
    replace_key_and_tokens(data_dir, store, encryption_key, stored)?;
    Ok(())
}

fn persist_fleet_setup_and_drop_legacy(
    data_dir: &Path,
    store: &HubStore,
    credentials: &FleetSetupCredentials,
    now: SystemTime,
) -> Result<(), Box<dyn std::error::Error>> {
    persist_fleet_setup_credentials(store, data_dir, credentials, now).map_err(|error| {
        provider_switch_outcome_ambiguous("persisting Fleet credentials", error)
    })?;
    remove_key_and_tokens(data_dir, store).map_err(|error| {
        provider_switch_outcome_ambiguous("removing previous Legacy credentials", error)
    })?;
    Ok(())
}

#[cfg(unix)]
const PROVIDER_SWITCH_OUTCOME_AMBIGUOUS: &str = "TESLATLAS_PROVIDER_SWITCH_OUTCOME_AMBIGUOUS";

#[cfg(unix)]
const MIGRATION_OUTCOME_AMBIGUOUS: &str = "TESLATLAS_MIGRATION_OUTCOME_AMBIGUOUS";

#[cfg(unix)]
fn provider_switch_outcome_ambiguous(
    action: &str,
    error: impl std::fmt::Display,
) -> Box<dyn std::error::Error> {
    std::io::Error::other(format!(
        "{PROVIDER_SWITCH_OUTCOME_AMBIGUOUS}: {action}: {error}; Hub must remain stopped until status and diagnostics confirm the selected provider"
    ))
    .into()
}

#[cfg(unix)]
fn migration_outcome_ambiguous(
    action: &str,
    error: impl std::fmt::Display,
) -> Box<dyn std::error::Error> {
    std::io::Error::other(format!(
        "{MIGRATION_OUTCOME_AMBIGUOUS}: {action}: {error}; keep the migration handover gate and Hub stopped"
    ))
    .into()
}

async fn run_control(
    config_path: &Path,
    requested_vehicle_id: Option<uuid::Uuid>,
    command: &ControlCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = HubConfig::load(config_path)?;
    let store = HubStore::open_existing(&config.data_dir)?;
    match command {
        ControlCommand::PairedDevices => {
            println!("{}", serde_json::to_string(&store.list_paired_devices()?)?);
            return Ok(());
        }
        ControlCommand::RevokeDevice { device_id } => {
            store.revoke_device(*device_id)?;
            println!(
                "{}",
                serde_json::json!({"status": "revoked", "deviceId": device_id})
            );
            return Ok(());
        }
        ControlCommand::SignOut => {
            #[cfg(target_os = "macos")]
            teslatlas_hub::macos_launch_agent::stop_installed()?;
            #[cfg(target_os = "linux")]
            teslatlas_hub::linux_systemd::apply(teslatlas_hub::linux_systemd::ServiceAction::Stop)?;

            #[cfg(unix)]
            let _admission = AdmittedUserHub::admit(&config.data_dir)?;
            let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
            clear_provider_credentials(&config.data_dir, &store)?;
            catalogue_checkpoint.finish()?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "signed_out",
                    "service": "stopped",
                })
            );
            return Ok(());
        }
        _ => {}
    }
    let vehicle_id = control_target(&store, requested_vehicle_id)?;
    if let Some(action) = explicit_vehicle_action(command)? {
        let report =
            collector::request_resident_vehicle_action(&config.data_dir, vehicle_id, action)
                .await?;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    match command {
        ControlCommand::PairedDevices
        | ControlCommand::RevokeDevice { .. }
        | ControlCommand::SignOut => {
            unreachable!("global controls returned before vehicle selection")
        }
        ControlCommand::Wake { .. }
        | ControlCommand::ClimateStart { .. }
        | ControlCommand::ClimateStop { .. }
        | ControlCommand::ChargeStart { .. }
        | ControlCommand::ChargeStop { .. }
        | ControlCommand::SetChargeLimit { .. }
        | ControlCommand::Lock { .. }
        | ControlCommand::Unlock { .. }
        | ControlCommand::FlashLights { .. }
        | ControlCommand::HonkHorn { .. } => {
            unreachable!("vehicle actions returned before local controls")
        }
        ControlCommand::Settings {
            enabled,
            streaming,
            suspend_after_idle_min,
            suspend_min,
            require_locked,
            free_supercharging,
            lfp_battery,
        } => {
            validate_streaming_setting(config.collector.provider, *streaming)?;
            let mut settings = store.load_car_settings(vehicle_id)?;
            let changed = enabled.is_some()
                || streaming.is_some()
                || suspend_after_idle_min.is_some()
                || suspend_min.is_some()
                || require_locked.is_some()
                || free_supercharging.is_some()
                || lfp_battery.is_some();
            if let Some(value) = enabled {
                settings.enabled = *value;
            }
            if let Some(value) = streaming {
                settings.use_streaming_api = *value;
            }
            if let Some(value) = suspend_after_idle_min {
                settings.suspend_after_idle_min = *value;
            }
            if let Some(value) = suspend_min {
                settings.suspend_min = *value;
                settings.suspend_min_resolved = true;
            }
            if let Some(value) = require_locked {
                settings.req_not_unlocked = *value;
            }
            if let Some(value) = free_supercharging {
                settings.free_supercharging = *value;
            }
            if let Some(value) = lfp_battery {
                settings.lfp_battery = *value;
            }
            if changed {
                store.replace_car_settings(vehicle_id, &settings)?;
            }
            println!(
                "{}",
                serde_json::json!({
                    "status": if changed { "updated" } else { "ok" },
                    "vehicleId": vehicle_id,
                    "settings": settings,
                })
            );
        }
        ControlCommand::Pause | ControlCommand::Resume => {
            let mut settings = store.load_car_settings(vehicle_id)?;
            settings.enabled = matches!(command, ControlCommand::Resume);
            store.replace_car_settings(vehicle_id, &settings)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": if settings.enabled { "running" } else { "paused" },
                    "vehicleId": vehicle_id,
                })
            );
        }
        ControlCommand::Geofences => {
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "vehicleId": vehicle_id,
                    "geofences": store.geofences(vehicle_id)?,
                })
            );
        }
        ControlCommand::SetGeofence {
            id,
            name,
            latitude,
            longitude,
            radius_m,
            billing_type,
            cost_per_unit,
            session_fee,
            recalculate_missing_costs,
        } => {
            let billing_type = billing_type
                .parse::<GeofenceBillingType>()
                .map_err(|_| "--billing-type must be per_kwh or per_minute")?;
            let geofence = store.save_geofence(
                vehicle_id,
                *id,
                teslatlas_hub::teslamate_projection::TeslaMateGeofence {
                    id: id.unwrap_or_default(),
                    name: name.clone(),
                    latitude: Some(*latitude),
                    longitude: Some(*longitude),
                    radius_m: Some(*radius_m),
                    billing_type: Some(billing_type),
                    cost_per_unit: *cost_per_unit,
                    session_fee: *session_fee,
                },
            )?;
            let recalculated_charge_costs = if *recalculate_missing_costs {
                store.recalculate_missing_charge_costs(vehicle_id, geofence.id)?
            } else {
                0
            };
            println!(
                "{}",
                serde_json::json!({
                    "status": "updated",
                    "vehicleId": vehicle_id,
                    "geofence": geofence,
                    "recalculatedChargeCosts": recalculated_charge_costs,
                })
            );
        }
        ControlCommand::DeleteGeofence { id } => {
            store.delete_geofence(vehicle_id, *id)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "deleted",
                    "vehicleId": vehicle_id,
                    "geofenceId": id,
                })
            );
        }
        ControlCommand::SetChargeCost {
            charge_id,
            cost,
            mode,
        } => {
            let charge = match mode.as_str() {
                "total" => store.set_charge_cost(vehicle_id, *charge_id, *cost)?,
                "per_kwh" => store.set_charge_cost_rate(
                    vehicle_id,
                    *charge_id,
                    *cost,
                    GeofenceBillingType::PerKwh,
                )?,
                "per_minute" => store.set_charge_cost_rate(
                    vehicle_id,
                    *charge_id,
                    *cost,
                    GeofenceBillingType::PerMinute,
                )?,
                _ => return Err("--mode must be total, per_kwh, or per_minute".into()),
            };
            println!(
                "{}",
                serde_json::json!({
                    "status": "updated",
                    "vehicleId": vehicle_id,
                    "charge": charge,
                })
            );
        }
        ControlCommand::ExportGpx { drive_id } => {
            let stdout = std::io::stdout();
            let mut writer = std::io::BufWriter::new(stdout.lock());
            export_drive_gpx(&store, vehicle_id, *drive_id, &mut writer)?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Ensures every short-lived local writer leaves a checkpointed catalogue,
/// including error paths. Successful commands surface checkpoint failures;
/// failing commands preserve their primary error while still attempting it.
struct CatalogueCheckpointGuard {
    store: HubStore,
    armed: bool,
}

impl CatalogueCheckpointGuard {
    fn new(store: HubStore) -> Self {
        Self { store, armed: true }
    }

    fn finish(&mut self) -> Result<(), teslatlas_hub::db::StoreError> {
        let result = self.store.checkpoint_catalogue_for_immutable_read();
        self.armed = false;
        result
    }
}

impl Drop for CatalogueCheckpointGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.store.checkpoint_catalogue_for_immutable_read();
        }
    }
}

#[cfg(unix)]
const TESLAMATE_REQUIRED_VERSION: &str = "4.1.1";

#[cfg(unix)]
fn print_teslamate_check_success(car_id: i64, snapshot: &TeslaMateCheckSnapshot) {
    println!(
        "{}",
        serde_json::json!({
            "status": "compatible",
            "reasonCode": "exact_4_1_1",
            "requiredVersion": TESLAMATE_REQUIRED_VERSION,
            "pinnedSourceRevision": snapshot.schema.pinned_source_revision,
            "observedMigrationVersion": snapshot.schema.observed_migration_version,
            "observedMigrationCount": snapshot.schema.observed_migration_count,
            "minimumSupportedMigrationVersion": snapshot.schema.minimum_supported_migration_version,
            "maximumValidatedMigrationVersion": snapshot.schema.maximum_validated_migration_version,
            "selectedCarId": car_id,
            "connection": snapshot.connection,
            "selectedCar": snapshot.selected_car,
            "openSessions": snapshot.open_sessions,
            "selectedCarCounts": snapshot.selected_car_counts,
            "sourceTotals": snapshot.source_totals,
            "sourceTokensRelationPresent": snapshot.source_tokens_relation_present,
            "legacyTokenPair": snapshot.legacy_token_pair,
            "sourceNeverMutated": true,
            "guidance": "Exact TeslaMate 4.1.1 compatibility verified. The source is read-only and ready for migration. TeslaMate data is not deleted.",
        })
    );
}

#[cfg(unix)]
fn print_teslamate_check_failure(
    car_id: i64,
    status: &str,
    reason_code: &str,
    observed_migration_version: Option<i64>,
    guidance: &str,
) {
    println!(
        "{}",
        serde_json::json!({
            "status": status,
            "reasonCode": reason_code,
            "requiredVersion": TESLAMATE_REQUIRED_VERSION,
            "pinnedSourceRevision": TESLAMATE_V4_SOURCE_REVISION,
            "maximumValidatedMigrationVersion": MAX_VALIDATED_MIGRATION,
            "expectedMigrationCount": TESLAMATE_V4_MIGRATION_COUNT,
            "selectedCarId": car_id,
            "observedMigrationVersion": observed_migration_version,
            "guidance": guidance,
        })
    );
}

#[cfg(unix)]
fn teslamate_check_failure_details(
    error: &TeslaMateReaderError,
) -> (&'static str, &'static str, Option<i64>, &'static str) {
    match error {
        TeslaMateReaderError::Schema(SchemaCompatibilityError::LegacyMigration {
            found, ..
        }) => (
            "incompatible",
            "older_than_4_1_1",
            Some(*found),
            "Back up TeslaMate, update it to exact version 4.1.1, allow its migrations to finish, then retry.",
        ),
        TeslaMateReaderError::Schema(SchemaCompatibilityError::UnreviewedMigration {
            found,
            ..
        }) => (
            "incompatible",
            "newer_than_4_1_1",
            Some(*found),
            "This Hub build supports exact TeslaMate 4.1.1 only. Do not downgrade a live database; use a separate compatible backup or wait for a reviewed adapter.",
        ),
        TeslaMateReaderError::Schema(_) | TeslaMateReaderError::MissingMigrationVersion => (
            "incompatible",
            "schema_mismatch",
            None,
            "The source does not match the exact TeslaMate 4.1.1 migration and physical-schema contract. Do not modify or downgrade the live database.",
        ),
        TeslaMateReaderError::SelectedCarMissing { .. } => (
            "incompatible",
            "selected_car_missing",
            None,
            "Choose a car ID that exists in the compatible TeslaMate source, then retry.",
        ),
        TeslaMateReaderError::AmbiguousOpenSession { .. } => (
            "incompatible",
            "ambiguous_open_session",
            None,
            "The TeslaMate source has more than one open drive, charging process, or state. Finish or repair those sessions, then retry.",
        ),
        TeslaMateReaderError::LegacyTokenPairMissing
        | TeslaMateReaderError::LegacyTokenPairAmbiguous
        | TeslaMateReaderError::LegacyTokenPairEmpty
        | TeslaMateReaderError::LegacyTokenCiphertextTooLarge { .. } => (
            "incompatible",
            "legacy_token_pair_invalid",
            None,
            "TeslaMate must contain exactly one non-empty, bounded legacy OAuth token pair before migration. Repair or re-login to TeslaMate, then retry.",
        ),
        _ => (
            "unavailable",
            "source_unavailable",
            None,
            "Check the password-free PostgreSQL URL, read-only database credentials, network, and TLS trust, then retry.",
        ),
    }
}

#[cfg(unix)]
async fn run_teslamate_check(
    source_url: &str,
    car_id: i64,
    postgres_password_file: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = match ReadOnlySource::parse(source_url) {
        Ok(source) => source,
        Err(_) => {
            print_teslamate_check_failure(
                car_id,
                "unavailable",
                "invalid_source",
                None,
                "Use a password-free PostgreSQL URL with an explicit read-only user, then retry.",
            );
            return Err(std::io::Error::other(
                "TeslaMate compatibility check failed; see JSON report",
            )
            .into());
        }
    };
    let password = match read_migration_postgres_password(postgres_password_file) {
        Ok(password) => password,
        Err(_) => {
            print_teslamate_check_failure(
                car_id,
                "unavailable",
                "credential_unavailable",
                None,
                "Provide one safe, bounded PostgreSQL password file or stdin value, then retry.",
            );
            return Err(std::io::Error::other(
                "TeslaMate compatibility check failed; see JSON report",
            )
            .into());
        }
    };
    match check_teslamate_compatibility(&source, &password, car_id, TeslaMateReadLimits::default())
        .await
    {
        Ok(snapshot) => {
            print_teslamate_check_success(car_id, &snapshot);
            Ok(())
        }
        Err(error) => {
            let (status, reason_code, observed, guidance) = teslamate_check_failure_details(&error);
            print_teslamate_check_failure(car_id, status, reason_code, observed, guidance);
            Err(
                std::io::Error::other("TeslaMate compatibility check failed; see JSON report")
                    .into(),
            )
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = cli.config.unwrap_or_else(default_config_path);

    #[cfg(unix)]
    if let Command::TeslaMateCheck {
        source,
        car_id,
        postgres_password_file,
    } = &cli.command
    {
        return run_teslamate_check(source, *car_id, postgres_password_file).await;
    }

    #[cfg(unix)]
    if let Command::WriteBack {
        source,
        car_id,
        postgres_password_file,
        command,
    } = &cli.command
    {
        let source = ReadOnlySource::parse(source)?;
        let password = TeslaMatePostgresPassword::from_bytes(&read_migration_secret(
            postgres_password_file,
            MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES,
        )?)?;
        match command {
            WriteBackCommand::ChargeCost {
                charging_process_id,
                cost,
                apply,
            } => {
                let receipt = write_back_charge_cost(
                    &source,
                    &password,
                    *car_id,
                    *charging_process_id,
                    *cost,
                    *apply,
                )
                .await?;
                println!("{}", serde_json::to_string(&receipt)?);
            }
        }
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    if let Command::Service { command } = &cli.command {
        match command {
            ServiceCommand::Status => {
                let loaded = teslatlas_hub::macos_launch_agent::service_is_loaded()?;
                println!(
                    "{}",
                    serde_json::json!({
                        "status": if loaded { "running" } else { "stopped" },
                        "loaded": loaded,
                    })
                );
            }
            ServiceCommand::Start => {
                let config = HubConfig::load(&config_path)?;
                teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
                    &config.data_dir,
                    config.collector.provider,
                )?;
                teslatlas_hub::macos_launch_agent::start_installed(&config.data_dir)?;
                println!("{}", serde_json::json!({"status": "running"}));
            }
            ServiceCommand::Stop => {
                teslatlas_hub::macos_launch_agent::stop_installed()?;
                println!("{}", serde_json::json!({"status": "stopped"}));
            }
            ServiceCommand::Restart => {
                let config = HubConfig::load(&config_path)?;
                teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
                    &config.data_dir,
                    config.collector.provider,
                )?;
                teslatlas_hub::macos_launch_agent::restart_installed(&config.data_dir)?;
                println!("{}", serde_json::json!({"status": "running"}));
            }
        }
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    if let Command::Service { command } = &cli.command {
        let status = match command {
            ServiceCommand::Status => teslatlas_hub::linux_systemd::status()?,
            ServiceCommand::Start => teslatlas_hub::linux_systemd::apply(
                teslatlas_hub::linux_systemd::ServiceAction::Start,
            )?,
            ServiceCommand::Stop => teslatlas_hub::linux_systemd::apply(
                teslatlas_hub::linux_systemd::ServiceAction::Stop,
            )?,
            ServiceCommand::Restart => teslatlas_hub::linux_systemd::apply(
                teslatlas_hub::linux_systemd::ServiceAction::Restart,
            )?,
        };
        println!(
            "{}",
            serde_json::json!({
                "status": status.status(),
                "unit": status.unit,
                "loadState": status.load_state,
                "activeState": status.active_state,
                "subState": status.sub_state,
            })
        );
        return Ok(());
    }

    if let Command::Control {
        vehicle_id,
        command,
    } = &cli.command
    {
        return run_control(&config_path, *vehicle_id, command).await;
    }

    #[cfg(not(unix))]
    if matches!(&cli.command, Command::Serve) {
        return Err("serve requires a Unix platform".into());
    }

    #[cfg(target_os = "macos")]
    if matches!(&cli.command, Command::Install) {
        let config = HubConfig::load(&config_path)?;
        let admission = AdmittedUserHub::admit(&config.data_dir)?;
        teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
            &config.data_dir,
            config.collector.provider,
        )?;
        let installed =
            teslatlas_hub::macos_launch_agent::prepare_install(&config.data_dir, &config_path)?;
        drop(admission);
        teslatlas_hub::macos_launch_agent::start_prepared(&installed)?;
        println!("installed {}; launch requested", installed.binary.display());
        return Ok(());
    }

    // Long-lived service, import, credential, and recovery commands take the
    // local instance lock. `control` uses short SQLite transactions so it can
    // intentionally operate while the collector is running.
    #[cfg(unix)]
    let admitted_user_hub = if command_requires_user_hub_admission(&cli.command) {
        let config = HubConfig::load(&config_path)?;
        Some(AdmittedUserHub::admit(&config.data_dir)?)
    } else {
        None
    };

    #[cfg(unix)]
    if let Command::Migrate {
        source,
        car_id,
        postgres_password_file,
        encryption_key_file,
        access_token_file,
        refresh_token_file,
        online_snapshot,
    } = &cli.command
    {
        let start_hub = run_macos_migration(
            admitted_user_hub
                .as_ref()
                .ok_or("migration reached runtime without user admission")?,
            MacMigrationInput {
                config_path: &config_path,
                source_url: source,
                car_id: *car_id,
                postgres_password_file,
                encryption_key_file: encryption_key_file.as_deref(),
                access_token_file: access_token_file.as_deref(),
                refresh_token_file: refresh_token_file.as_deref(),
                online_snapshot: *online_snapshot,
            },
        )
        .await?;
        drop(admitted_user_hub);
        #[cfg(target_os = "macos")]
        if start_hub {
            let config = HubConfig::load(&config_path)?;
            teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
                &config.data_dir,
                config.collector.provider,
            )?;
            let installed =
                teslatlas_hub::macos_launch_agent::prepare_install(&config.data_dir, &config_path)?;
            teslatlas_hub::macos_launch_agent::start_prepared(&installed)?;
            println!("installed {}; launch requested", installed.binary.display());
        }
        #[cfg(target_os = "linux")]
        if start_hub {
            println!(
                "{}",
                serde_json::json!({
                    "serviceStartRequested": true,
                    "next": "sudo systemctl start teslatlas-hub.service",
                })
            );
        }
        return Ok(());
    }

    match &cli.command {
        Command::VerifyBackup { source } => {
            let report = verify_data_backup(source)?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        Command::RestoreData {
            source,
            destination,
        } => {
            let report = restore_data_backup(source, destination)?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        _ => {}
    }

    match &cli.command {
        Command::Legal => {
            println!("{}", teslatlas_hub::legal_notice());
            return Ok(());
        }
        Command::Doctor => {
            let config = HubConfig::load(&config_path)?;
            let report = run_immutable_diagnostic(&config.data_dir, |store| {
                Ok(inspect_hub(store, &config)?)
            })?;
            report.log();
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.is_ok() {
                return Err(std::io::Error::other("doctor found failures; see JSON report").into());
            }
            return Ok(());
        }
        Command::Status => {
            let config = HubConfig::load(&config_path)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let vehicles = store.published_vehicles()?;
            let configured = store.configured_tesla_vehicles()?;
            let mut vehicle_summaries = Vec::with_capacity(vehicles.len());
            for vehicle in &vehicles {
                let binding = store.v2_projection_binding(vehicle.vehicle_id)?;
                let latest =
                    store.latest_current_observation_metadata_for_vehicle(vehicle.vehicle_id)?;
                let tesla_eid = configured.iter().find_map(|(vehicle_id, eid, _)| {
                    (*vehicle_id == vehicle.vehicle_id).then_some(*eid)
                });
                vehicle_summaries.push(serde_json::json!({
                    "vehicleId": vehicle.vehicle_id,
                    "displayName": vehicle.display_name,
                    "sourceCarId": binding.selected_car_id,
                    "teslaEid": tesla_eid,
                    "latestObservationId": latest.as_ref().map_or(0, |observation| observation.observation_id),
                    "latestObservedAtMs": latest.as_ref().map(|observation| observation.observed_at_ms),
                    "latestReceivedAtMs": latest.as_ref().map(|observation| observation.received_at_ms),
                }));
            }
            let vehicle = (vehicle_summaries.len() == 1).then(|| vehicle_summaries[0].clone());
            let legacy_credentials = store.load_teslamate_legacy_tokens()?;
            let fleet_credentials = store.load_fleet_tokens()?;
            let (fleet_scope_summary, fleet_scope_status) = if fleet_credentials.is_some() {
                match stored_fleet_scope_summary(&store, &config.data_dir) {
                    Ok(summary) => (summary, Some("ready")),
                    Err(error) => {
                        let status = match error {
                            FleetCredentialError::MissingCollectionScopes => {
                                "missing_collection_scopes"
                            }
                            FleetCredentialError::InvalidAccessTokenClaims => {
                                "invalid_access_token_claims"
                            }
                            FleetCredentialError::MigrationRequired => "migration_required",
                            _ => "unavailable",
                        };
                        (None, Some(status))
                    }
                }
            } else {
                (None, None)
            };
            let selected_credentials_present = match config.collector.provider {
                CollectorProvider::Legacy => legacy_credentials.is_some(),
                CollectorProvider::Fleet => fleet_credentials.is_some(),
            };
            let readiness = store
                .service_readiness_at(config.collector.interval_seconds > 0, current_epoch_ms()?);
            let database_bytes = fs::metadata(store.database_path())?.len();
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "version": teslatlas_hub::BUILD_VERSION,
                    "database": {
                        "path": store.database_path(),
                        "bytes": database_bytes,
                    },
                    "ready": readiness.is_ok(),
                    "readinessReason": readiness.err().map(|failure| failure.code),
                    "provider": config.collector.provider,
                    "vehicle": vehicle,
                    "vehicles": vehicle_summaries,
                    "credentials": {
                        "present": selected_credentials_present,
                    },
                    "legacyCredentials": {
                        "present": legacy_credentials.is_some(),
                        "expiresAt": legacy_credentials.as_ref().map(TeslaMateLegacyTokenStore::expires_at),
                        "nextRefreshAt": legacy_credentials
                            .as_ref()
                            .map(TeslaMateLegacyTokenStore::next_refresh_at),
                    },
                    "fleetCredentials": {
                        "present": fleet_credentials.is_some(),
                        "expiresAt": fleet_credentials.as_ref().map(|credentials| credentials.expires_at()),
                        "nextRefreshAt": fleet_credentials.as_ref().map(|credentials| credentials.next_refresh_at()),
                        "scopes": fleet_scope_summary,
                        "scopeStatus": fleet_scope_status,
                    },
                    "fleetTelemetry": {
                        "enabled": config.collector.fleet_telemetry.is_some(),
                        "configured": config.collector.fleet_telemetry.is_some(),
                        "mode": if config.collector.fleet_telemetry.is_some() {
                            "native_push_configured"
                        } else {
                            "disabled"
                        },
                        "operationalState": if config.collector.fleet_telemetry.is_some() {
                            "requires_receiver_and_vehicle_receipt_proof"
                        } else {
                            "disabled"
                        },
                        "paidVehicleDataPolling": config.collector.provider == CollectorProvider::Fleet
                            && config.collector.fleet_telemetry.is_none(),
                        "deliveryPolicy": config.collector.fleet_telemetry.as_ref().map(|_| "latest"),
                    },
                })
            );
            return Ok(());
        }
        #[cfg(unix)]
        Command::Preflight => {
            let config = HubConfig::load(&config_path)?;
            run_immutable_diagnostic(&config.data_dir, |store| {
                store.catalogue_check()?;
                let configured = store.configured_tesla_vehicles()?;
                if configured.is_empty() {
                    return Err("at least one configured vehicle is required".into());
                }
                match config.collector.provider {
                    CollectorProvider::Legacy => {
                        let tokens = store
                            .load_teslamate_legacy_tokens()?
                            .ok_or("legacy Owner API credentials are required")?;
                        teslatlas_hub::teslamate_credentials::load_key_for_tokens(
                            &config.data_dir,
                            &tokens,
                        )?;
                    }
                    CollectorProvider::Fleet => {
                        validate_stored_fleet_credentials(store, &config.data_dir)?;
                    }
                }
                Ok(())
            })?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "ready",
                    "version": teslatlas_hub::BUILD_VERSION,
                    "provider": config.collector.provider,
                })
            );
            return Ok(());
        }
        Command::ObservationWatermark { car_id } => {
            let config = HubConfig::load(&config_path)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let watermark = match store.observation_watermark(*car_id) {
                Ok(watermark) => watermark,
                Err(error) => {
                    return Err(observation_command_error(
                        "observation-watermark",
                        *car_id,
                        error,
                    ));
                }
            };
            println!(
                "{}",
                serde_json::json!({
                    "status": "captured",
                    "command": "observation-watermark",
                    "carId": watermark.source_car_id,
                    "sourceId": watermark.source_id,
                    "vehicleId": watermark.vehicle_id,
                    "watermark": watermark.observation_id,
                    "observedAtMs": watermark.observed_at_ms,
                    "receivedAtMs": watermark.received_at_ms,
                })
            );
            return Ok(());
        }
        Command::VerifyObservation { car_id, watermark } => {
            let config = HubConfig::load(&config_path)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let verification = match store.verify_observation_after(*car_id, *watermark) {
                Ok(verification) => verification,
                Err(error) => {
                    return Err(observation_command_error(
                        "verify-observation",
                        *car_id,
                        error,
                    ));
                }
            };
            let verified = verification.verified();
            println!(
                "{}",
                serde_json::json!({
                    "status": if verified { "verified" } else { "not_verified" },
                    "command": "verify-observation",
                    "verified": verified,
                    "carId": verification.source_car_id,
                    "sourceId": verification.source_id,
                    "vehicleId": verification.vehicle_id,
                    "afterWatermark": verification.after_observation_id,
                    "latestObservationId": verification.latest_observation_id,
                    "latestObservedAtMs": verification.latest_observed_at_ms,
                    "latestReceivedAtMs": verification.latest_received_at_ms,
                })
            );
            if !verified {
                return Err("no strictly newer durable observation".into());
            }
            return Ok(());
        }
        _ => {}
    }

    #[cfg(unix)]
    if let Some(admission) = admitted_user_hub.as_ref() {
        admission.assert_sensitive_access()?;
    }
    let (config, config_sha256) = HubConfig::load_with_digest(&config_path)?;
    #[cfg(unix)]
    if let Some(admission) = admitted_user_hub.as_ref() {
        admission.assert_store_path(&config.data_dir)?;
    }
    let store = HubStore::initialize(&config.data_dir)?;
    let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
    match cli.command {
        Command::Init => {
            println!("initialized {}", store.database_path().display());
        }
        #[cfg(unix)]
        Command::Bootstrap => {
            migrate_legacy_fleet_credentials(&store, &config.data_dir)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "bootstrapped",
                    "version": teslatlas_hub::BUILD_VERSION,
                    "database": store.database_path(),
                })
            );
        }
        #[cfg(unix)]
        Command::Setup {
            access_token_file,
            refresh_token_file,
            tokens_stdin,
            vehicle_id,
            all_vehicles,
        } => {
            validate_legacy_setup_provider(config.collector.provider)?;
            let tokens = if tokens_stdin {
                read_setup_tokens_from_stdin()?
            } else {
                OwnerTokens::from_file_bytes(
                    read_migration_secret(
                        access_token_file
                            .as_deref()
                            .ok_or("setup access-token file is missing")?,
                        MAX_MIGRATION_TOKEN_FILE_BYTES,
                    )?,
                    read_migration_secret(
                        refresh_token_file
                            .as_deref()
                            .ok_or("setup refresh-token file is missing")?,
                        MAX_MIGRATION_TOKEN_FILE_BYTES,
                    )?,
                )?
            };
            let report = if all_vehicles {
                let report = collector::setup_native_vehicles(&store, &config, &tokens).await?;
                serde_json::json!({
                    "status": "configured",
                    "vehicles": report.vehicles,
                    "snapshotsPublished": report.snapshots_published,
                })
            } else {
                let report =
                    collector::setup_native_vehicle(&store, &config, &tokens, vehicle_id).await?;
                serde_json::json!({
                    "status": "configured",
                    "selectedVehicleId": report.selected_vehicle_id,
                    "displayName": report.display_name,
                    "snapshotsPublished": report.snapshots_published,
                })
            };
            persist_legacy_setup_and_drop_fleet(&config.data_dir, &store, &tokens)?;
            catalogue_checkpoint.finish().map_err(|error| {
                provider_switch_outcome_ambiguous("checkpointing Legacy setup", error)
            })?;
            println!("{report}");
            return Ok(());
        }
        #[cfg(unix)]
        Command::SetupFleet {
            vehicle_id,
            all_vehicles,
        } => {
            if config.collector.provider != CollectorProvider::Fleet {
                return Err("setup-fleet requires collector.provider = \"fleet\"".into());
            }
            let credentials = read_setup_fleet_from_stdin()?;
            let admission = admitted_user_hub
                .as_deref()
                .ok_or("Fleet setup reached runtime without user admission")?;
            let report = if all_vehicles {
                let report =
                    collector::setup_fleet_vehicles(&store, &config, &credentials, admission)
                        .await?;
                serde_json::json!({
                    "status": "configured",
                    "provider": "fleet",
                    "vehicles": report.vehicles,
                    "snapshotsPublished": report.snapshots_published,
                })
            } else {
                let report = collector::setup_fleet_vehicle(
                    &store,
                    &config,
                    &credentials,
                    admission,
                    vehicle_id,
                )
                .await?;
                serde_json::json!({
                    "status": "configured",
                    "provider": "fleet",
                    "selectedVehicleId": report.selected_vehicle_id,
                    "displayName": report.display_name,
                    "snapshotsPublished": report.snapshots_published,
                })
            };
            persist_fleet_setup_and_drop_legacy(
                &config.data_dir,
                &store,
                &credentials,
                SystemTime::now(),
            )?;
            catalogue_checkpoint.finish().map_err(|error| {
                provider_switch_outcome_ambiguous("checkpointing Fleet setup", error)
            })?;
            println!("{report}");
            return Ok(());
        }
        #[cfg(unix)]
        Command::ConfigureFleetTelemetry => {
            if config.collector.provider != CollectorProvider::Fleet
                || config.collector.fleet_telemetry.is_none()
            {
                return Err(
                    "configure-fleet-telemetry requires collector.provider = \"fleet\" and collector.fleet_telemetry"
                        .into(),
                );
            }
            let admission = admitted_user_hub
                .as_ref()
                .cloned()
                .ok_or("Fleet Telemetry setup reached runtime without user admission")?;
            let report =
                collector::configure_fleet_telemetry_for_admitted_user(&store, &config, admission)
                    .await?;
            catalogue_checkpoint.finish()?;
            println!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        Command::Legal
        | Command::Doctor
        | Command::Status
        | Command::TeslaMateCheck { .. }
        | Command::Control { .. }
        | Command::ObservationWatermark { .. }
        | Command::VerifyObservation { .. } => {
            unreachable!("read-only commands return before opening writable Hub state")
        }
        #[cfg(unix)]
        Command::Service { .. } => {
            unreachable!("service control returns before opening writable Hub state")
        }
        #[cfg(unix)]
        Command::Preflight => {
            unreachable!("preflight returns before opening writable Hub state")
        }
        Command::Serve => {
            #[cfg(unix)]
            {
                #[cfg(target_os = "macos")]
                {
                    store.checkpoint_catalogue_for_immutable_read()?;
                    teslatlas_hub::macos_launch_agent::preflight_hub_for_provider(
                        &config.data_dir,
                        config.collector.provider,
                    )?;
                }
                let admission =
                    admitted_user_hub.ok_or("Serve reached runtime without user admission")?;
                admission.assert_sensitive_access()?;
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm = signal(SignalKind::terminate())?;

                let collector_enabled = collector_can_start(&store, &config)?;
                log_runtime_inventory(&store, &config);
                tracing::info!(
                    collector_enabled,
                    provider = ?config.collector.provider,
                    interval_seconds = config.collector.interval_seconds,
                    bind = %config.bind,
                    "Hub serve starting (TeslaMate is not opened; stored tokens are not deleted)"
                );
                let collector_store = store.clone();
                let collector_config = config.clone();
                let collector_admission = std::sync::Arc::clone(&admission);
                let server_config = config;
                let server_admission = std::sync::Arc::clone(&admission);
                let control_admission = std::sync::Arc::clone(&admission);
                #[cfg(target_os = "macos")]
                let command_proxy = mac_command_proxy_spec(&server_config)?;
                #[cfg(not(target_os = "macos"))]
                let command_proxy = None;
                let serve_result = run_macos_serve_with_optional_proxy(
                    command_proxy,
                    collector_enabled,
                    move |ready, shutdown| async move {
                        collector::run_supervised_for_admitted_user(
                            &collector_store,
                            &collector_config,
                            collector_admission,
                            ready,
                            async move {
                                let _ = shutdown.await;
                            },
                        )
                        .await
                        .map_err(std::io::Error::other)
                    },
                    move |cursor_key, shutdown| async move {
                        server::serve_for_admitted_user(
                            store,
                            &server_config,
                            config_sha256,
                            server_admission,
                            cursor_key,
                            async move {
                                let _ = shutdown.await;
                            },
                        )
                        .await
                    },
                    async move {
                        tokio::select! {
                            error = control_admission.wait_until_invalid() => {
                                MacServeControl::AdmissionInvalidated(std::io::Error::other(error))
                            }
                            _ = tokio::signal::ctrl_c() => MacServeControl::Shutdown,
                            _ = sigterm.recv() => MacServeControl::Shutdown,
                        }
                    },
                )
                .await;
                match &serve_result {
                    Ok(()) => tracing::info!("Hub serve stopped cleanly"),
                    Err(error) => tracing::error!(%error, "Hub serve stopped unexpectedly"),
                }
                serve_result?;
            }
        }
        #[cfg(unix)]
        Command::Observe { duration_seconds } => {
            let admission =
                admitted_user_hub.ok_or("Observe reached runtime without user admission")?;
            admission.assert_sensitive_access()?;
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm = signal(SignalKind::terminate())?;

            let collector_store = store.clone();
            let collector_config = config.clone();
            let collector_admission = std::sync::Arc::clone(&admission);
            let server_config = config;
            let server_admission = std::sync::Arc::clone(&admission);
            let control_admission = std::sync::Arc::clone(&admission);
            tracing::info!(
                duration_seconds,
                provider = ?collector_config.collector.provider,
                "Hub bounded observation starting"
            );
            let observe_result = run_macos_serve_supervisor(
                true,
                move |ready, shutdown| async move {
                    collector::run_observer_for_admitted_user(
                        &collector_store,
                        &collector_config,
                        collector_admission,
                        ready,
                        async move {
                            let _ = shutdown.await;
                        },
                    )
                    .await
                    .map_err(std::io::Error::other)
                },
                move |cursor_key, shutdown| async move {
                    server::serve_for_admitted_user(
                        store,
                        &server_config,
                        config_sha256,
                        server_admission,
                        cursor_key,
                        async move {
                            let _ = shutdown.await;
                        },
                    )
                    .await
                },
                async move {
                    tokio::select! {
                        error = control_admission.wait_until_invalid() => {
                            MacServeControl::AdmissionInvalidated(std::io::Error::other(error))
                        }
                        _ = tokio::signal::ctrl_c() => MacServeControl::Shutdown,
                        _ = sigterm.recv() => MacServeControl::Shutdown,
                        _ = tokio::time::sleep(Duration::from_secs(duration_seconds)) => MacServeControl::Shutdown,
                    }
                },
            )
            .await;
            match &observe_result {
                Ok(()) => tracing::info!("Hub bounded observation stopped cleanly"),
                Err(error) => {
                    tracing::error!(%error, "Hub bounded observation stopped unexpectedly")
                }
            }
            observe_result?;
        }
        #[cfg(target_os = "macos")]
        Command::Install => unreachable!("install returns before opening Hub state"),
        #[cfg(unix)]
        Command::Migrate { .. } => {
            unreachable!("migration returns before opening common Hub state")
        }
        #[cfg(unix)]
        Command::WriteBack { .. } => {
            unreachable!("write-back returns before opening common Hub state")
        }
        Command::Pair {
            label,
            expires_in_seconds,
            json,
        } => {
            let tls = config
                .tls
                .as_ref()
                .ok_or("device pairing requires configured TLS")?;
            let mut stdout = std::io::stdout().lock();
            let created_at_ms = current_epoch_ms()?;
            execute_pairing_at(
                &store,
                PairingCommandInput {
                    label: &label,
                    expires_in_seconds,
                    json,
                    public_url: &tls.public_url,
                    certificate_path: &tls.certificate_path,
                    private_key_path: &tls.private_key_path,
                    created_at_ms,
                },
                &mut stdout,
            )
            .await?;
        }
        Command::Repair => {
            let report = store.repair()?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Backup { destination } => {
            let report = create_data_backup(&store, &destination)?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::ExportRecoveryCredentials {
            destination,
            recovery_key_file,
        } => {
            let recovery_key = read_recovery_encryption_key(&recovery_key_file)?;
            let report =
                export_credentials(&store, &config.data_dir, &destination, &recovery_key[..])?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::RestoreRecoveryCredentials {
            source,
            recovery_key_file,
        } => {
            let recovery_key = read_recovery_encryption_key(&recovery_key_file)?;
            let report = restore_credentials(&store, &config.data_dir, &source, &recovery_key[..])?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::VerifyBackup { .. } | Command::RestoreData { .. } => {
            unreachable!("immutable data-recovery commands return before writable Hub state")
        }
    }
    catalogue_checkpoint.finish()?;
    Ok(())
}

#[cfg(unix)]
fn command_requires_user_hub_admission(command: &Command) -> bool {
    matches!(
        command,
        Command::Init
            | Command::Bootstrap
            | Command::Setup { .. }
            | Command::SetupFleet { .. }
            | Command::ConfigureFleetTelemetry
            | Command::Serve
            | Command::Observe { .. }
            | Command::Migrate { .. }
            | Command::Pair { .. }
            | Command::Repair
            | Command::Backup { .. }
            | Command::ExportRecoveryCredentials { .. }
            | Command::RestoreRecoveryCredentials { .. }
    )
}

fn collector_can_start(
    store: &HubStore,
    config: &HubConfig,
) -> Result<bool, Box<dyn std::error::Error>> {
    let credentials_present = match config.collector.provider {
        CollectorProvider::Legacy => store.load_teslamate_legacy_tokens()?.is_some(),
        CollectorProvider::Fleet => store.load_fleet_tokens()?.is_some(),
    };
    Ok(config.collector.interval_seconds > 0
        && !store.configured_tesla_vehicles()?.is_empty()
        && credentials_present)
}

fn default_config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Teslatlas Hub")
            .join("config.toml");
    }
    #[cfg(target_os = "linux")]
    return PathBuf::from("/etc/teslatlas-hub/config.toml");
    #[cfg(not(target_os = "linux"))]
    PathBuf::from("config.toml")
}

#[cfg(unix)]
struct MacMigrationInput<'a> {
    config_path: &'a Path,
    source_url: &'a str,
    car_id: i64,
    postgres_password_file: &'a Path,
    encryption_key_file: Option<&'a Path>,
    access_token_file: Option<&'a Path>,
    refresh_token_file: Option<&'a Path>,
    online_snapshot: bool,
}

#[cfg(unix)]
const MAX_MIGRATION_POSTGRES_PASSWORD_BYTES: usize = 4 * 1024;
#[cfg(unix)]
const MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES: usize = MAX_MIGRATION_POSTGRES_PASSWORD_BYTES + 2;
#[cfg(unix)]
const MAX_MIGRATION_TOKEN_BYTES: usize =
    teslatlas_hub::teslamate_token::MAX_LEGACY_TOKEN_PLAINTEXT_BYTES;
#[cfg(unix)]
const MAX_MIGRATION_TOKEN_FILE_BYTES: usize = MAX_MIGRATION_TOKEN_BYTES + 2;
#[cfg(unix)]
const MAX_SETUP_TOKENS_STDIN_BYTES: usize = MAX_MIGRATION_TOKEN_BYTES * 2 + 128;
#[cfg(unix)]
const MAX_SETUP_FLEET_STDIN_BYTES: usize = MAX_MIGRATION_TOKEN_BYTES * 2 + 1_024;
#[cfg(unix)]
const MAX_MIGRATION_ENCRYPTION_KEY_BYTES: usize = 16 * 1024;

#[cfg(unix)]
#[derive(serde::Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetupTokensStdin {
    access_token: String,
    refresh_token: String,
}

#[cfg(unix)]
#[derive(serde::Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetupFleetStdin {
    access_token: String,
    refresh_token: String,
    client_id: String,
    #[zeroize(skip)]
    region: FleetRegion,
    #[zeroize(skip)]
    expires_in_seconds: u64,
}

#[cfg(unix)]
fn migration_stop_confirmed(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y")
}

#[cfg(unix)]
fn migration_start_requested(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y")
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationSecretReadError {
    Read,
    UnsafeFile,
    IdentityChanged,
    TooLarge,
}

#[cfg(unix)]
impl std::fmt::Display for MigrationSecretReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Read => "cannot read secret",
            Self::UnsafeFile => "secret file is unsafe",
            Self::IdentityChanged => "secret file changed while reading",
            Self::TooLarge => "secret exceeds the fixed size limit",
        })
    }
}

#[cfg(unix)]
impl std::error::Error for MigrationSecretReadError {}

#[cfg(unix)]
async fn run_macos_migration(
    admission: &AdmittedUserHub,
    MacMigrationInput {
        config_path,
        source_url,
        car_id,
        postgres_password_file,
        encryption_key_file,
        access_token_file,
        refresh_token_file,
        online_snapshot,
    }: MacMigrationInput<'_>,
) -> Result<bool, Box<dyn std::error::Error>> {
    if car_id <= 0 {
        return Err("--car-id must be a positive TeslaMate car id".into());
    }
    let secret_paths = std::iter::once(Some(postgres_password_file))
        .chain([encryption_key_file, access_token_file, refresh_token_file])
        .flatten()
        .collect::<Vec<_>>();
    if secret_paths
        .iter()
        .filter(|path| **path == Path::new("-"))
        .count()
        > 1
    {
        return Err("only one migration secret may be read from stdin".into());
    }

    let config = HubConfig::load(config_path)?;
    admission.assert_sensitive_access()?;
    admission.assert_store_path(&config.data_dir)?;
    let source = ReadOnlySource::parse(source_url)?;
    let postgres_password = read_migration_postgres_password(postgres_password_file)?;
    let mut limits = config.teslamate.read_limits()?;
    let profile = derive_effective_import_profile(
        limits.parallel_copy_lanes,
        &config.teslamate.performance_profile,
        &config.data_dir,
    )?;
    limits.parallel_copy_lanes = profile.parallel_copy_lanes;
    let copy_teslamate_ciphertext = match (
        encryption_key_file,
        access_token_file,
        refresh_token_file,
    ) {
        (Some(_), None, None) => true,
        (None, Some(_), Some(_)) => false,
        _ => {
            return Err(
                "provide --encryption-key-file, or both --access-token-file and --refresh-token-file"
                    .into(),
            );
        }
    };

    let store = HubStore::initialize(&config.data_dir)?;
    let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
    let cursor_key = load_or_create_cursor_key(&config.data_dir)?;
    if !online_snapshot {
        let (initial_report, _) = import_direct_migration_snapshot(
            &store,
            &cursor_key,
            &source,
            &postgres_password,
            car_id,
            limits,
            false,
        )
        .await?;

        println!(
            "{}",
            serde_json::json!({
                "status": "initial-copy-complete",
                "captureMode": "direct",
                "selectedCarId": car_id,
                "projectedRows": initial_report.projected_rows,
                "snapshotId": initial_report.snapshot_id,
                "sequence": initial_report.sequence,
                "profileVersion": profile.version,
                "parallelCopyLanes": profile.parallel_copy_lanes,
                "profileReason": profile.reason.as_str(),
            })
        );
        print!("Stop TeslaMate now. Confirm it is stopped before final copy [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !migration_stop_confirmed(&answer) {
            return Err(
                "TeslaMate stop was not confirmed; final migration capture was not started".into(),
            );
        }
    }

    // Cutover mode re-captures after the operator stops TeslaMate. Online mode
    // performs this once while TeslaMate remains live. In both modes history
    // and source ciphertext, when selected, share this exact source snapshot.
    let (report, captured_ciphertexts) = import_direct_migration_snapshot(
        &store,
        &cursor_key,
        &source,
        &postgres_password,
        car_id,
        limits,
        copy_teslamate_ciphertext,
    )
    .await?;

    // The encrypted source pair came from the same final snapshot as history.
    let (encryption_key, access_ciphertext, refresh_ciphertext) = if copy_teslamate_ciphertext {
        let key_path = encryption_key_file.expect("validated encrypted-token input");
        let key = read_migration_encryption_key(key_path)?;
        if key.is_empty() {
            return Err("TeslaMate ENCRYPTION_KEY is empty".into());
        }
        let ciphertexts = captured_ciphertexts
            .ok_or("final migration snapshot did not retain TeslaMate credentials")?;
        // Validate compatibility without exposing either plaintext token.
        drop(decrypt_legacy_owner_tokens(
            &key,
            &ciphertexts.access,
            &ciphertexts.refresh,
        )?);
        let (access, refresh) = ciphertexts.into_parts();
        (key, access, refresh)
    } else {
        let access_path = access_token_file.expect("validated access-token input");
        let refresh_path = refresh_token_file.expect("validated refresh-token input");
        let key = random_encryption_key();
        let (access, refresh) = encrypt_legacy_owner_token_files(
            &key,
            read_migration_secret(access_path, MAX_MIGRATION_TOKEN_FILE_BYTES)?,
            read_migration_secret(refresh_path, MAX_MIGRATION_TOKEN_FILE_BYTES)?,
        )?;
        (key, access, refresh)
    };
    let stored = TeslaMateLegacyTokenStore::imported(access_ciphertext, refresh_ciphertext)?;
    let fleet_still_present = store.load_fleet_tokens()?.is_some();
    persist_migrated_legacy_tokens(&config.data_dir, &store, &encryption_key, &stored)
        .map_err(|error| migration_outcome_ambiguous("persisting imported credentials", error))?;
    tracing::info!(
        selected_car_id = car_id,
        projected_rows = report.projected_rows,
        fleet_tokens_preserved = fleet_still_present,
        "TeslaMate history imported; source PostgreSQL was not written; Fleet tokens were not deleted"
    );

    catalogue_checkpoint
        .finish()
        .map_err(|error| migration_outcome_ambiguous("checkpointing imported catalogue", error))?;

    println!(
        "{}",
        serde_json::json!({
            "status": "imported",
            "captureMode": if online_snapshot { "online-snapshot" } else { "direct" },
            "selectedCarId": car_id,
            "projectedRows": report.projected_rows,
            "snapshotId": report.snapshot_id,
            "sequence": report.sequence,
            "cutoverUnsettled": report.cutover_unsettled,
            "retryRecommended": report.cutover_unsettled,
            "sourceNeverMutated": true,
            "fleetTokensPreserved": fleet_still_present,
            "accessCiphertextBytes": stored.access().len(),
            "refreshCiphertextBytes": stored.refresh().len(),
            "profileVersion": profile.version,
            "parallelCopyLanes": profile.parallel_copy_lanes,
            "profileReason": profile.reason.as_str(),
        })
    );

    if online_snapshot {
        return Ok(false);
    }

    print!("Start Teslatlas Hub now? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let start_hub = migration_start_requested(&answer);

    Ok(start_hub)
}

#[cfg(unix)]
async fn import_direct_migration_snapshot(
    store: &HubStore,
    cursor_key: &CursorKey,
    source: &ReadOnlySource,
    postgres_password: &TeslaMatePostgresPassword,
    car_id: i64,
    limits: teslatlas_hub::teslamate_reader::TeslaMateReadLimits,
    include_legacy_token: bool,
) -> Result<
    (
        TeslaMateImportReport,
        Option<TeslaMateLegacyTokenCiphertexts>,
    ),
    Box<dyn std::error::Error>,
> {
    tracing::info!(
        host = source.host(),
        port = source.port(),
        database = source.database_name(),
        car_id,
        include_legacy_token,
        "starting TeslaMate read-only snapshot import"
    );
    let imported_at_ms = current_epoch_ms()?;
    let request = TeslaMateImportRequest {
        source_key: "teslamate".to_owned(),
        scope: TeslaMateImportScope::Selected(car_id),
        imported_at_ms,
    };
    if include_legacy_token {
        let (selected, tokens) = import_selected_from_postgres_with_schema_22_and_legacy_token(
            store,
            source,
            postgres_password,
            cursor_key,
            &request,
            limits,
        )
        .await?;
        Ok((selected.import, Some(tokens)))
    } else {
        let selected = import_selected_from_postgres_with_schema_22(
            store,
            source,
            postgres_password,
            cursor_key,
            &request,
            limits,
        )
        .await?;
        Ok((selected.import, None))
    }
}

#[cfg(unix)]
fn read_migration_secret(
    path: &Path,
    maximum: usize,
) -> Result<zeroize::Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
    if path == Path::new("-") {
        return read_bounded_migration_secret(std::io::stdin(), maximum).map_err(Into::into);
    }
    read_migration_secret_file(path, maximum).map_err(Into::into)
}

#[cfg(unix)]
fn read_setup_tokens_from_stdin() -> Result<OwnerTokens, Box<dyn std::error::Error>> {
    let bytes = read_bounded_migration_secret(std::io::stdin(), MAX_SETUP_TOKENS_STDIN_BYTES)?;
    let mut input: SetupTokensStdin = serde_json::from_slice(&bytes)?;
    OwnerTokens::from_file_bytes(
        zeroize::Zeroizing::new(std::mem::take(&mut input.access_token).into_bytes()),
        zeroize::Zeroizing::new(std::mem::take(&mut input.refresh_token).into_bytes()),
    )
    .map_err(Into::into)
}

#[cfg(unix)]
fn decode_setup_fleet_stdin(
    bytes: &[u8],
) -> Result<FleetSetupCredentials, Box<dyn std::error::Error>> {
    let mut input: SetupFleetStdin = serde_json::from_slice(bytes)?;
    let credentials = FleetSetupCredentials::new(
        std::mem::take(&mut input.access_token),
        std::mem::take(&mut input.refresh_token),
        std::mem::take(&mut input.client_id),
        input.region,
        input.expires_in_seconds,
    )?;
    credentials.require_collection_scopes()?;
    Ok(credentials)
}

#[cfg(unix)]
fn read_setup_fleet_from_stdin() -> Result<FleetSetupCredentials, Box<dyn std::error::Error>> {
    let bytes = read_bounded_migration_secret(std::io::stdin(), MAX_SETUP_FLEET_STDIN_BYTES)?;
    decode_setup_fleet_stdin(&bytes)
}

#[cfg(unix)]
fn read_recovery_encryption_key(
    path: &Path,
) -> Result<zeroize::Zeroizing<[u8; RECOVERY_ENCRYPTION_KEY_BYTES]>, Box<dyn std::error::Error>> {
    let bytes = read_migration_secret(path, RECOVERY_ENCRYPTION_KEY_BYTES)?;
    if bytes.len() != RECOVERY_ENCRYPTION_KEY_BYTES {
        return Err(format!(
            "credential-recovery encryption key must be exactly {RECOVERY_ENCRYPTION_KEY_BYTES} bytes"
        )
        .into());
    }
    let mut key = zeroize::Zeroizing::new([0_u8; RECOVERY_ENCRYPTION_KEY_BYTES]);
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(unix)]
fn read_migration_encryption_key(
    path: &Path,
) -> Result<zeroize::Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
    let mut key = read_migration_secret(path, MAX_MIGRATION_ENCRYPTION_KEY_BYTES)?;
    if key.last() == Some(&b'\n') {
        key.pop();
        if key.last() == Some(&b'\r') {
            key.pop();
        }
    }
    Ok(key)
}

#[cfg(unix)]
fn read_migration_postgres_password(
    path: &Path,
) -> Result<TeslaMatePostgresPassword, Box<dyn std::error::Error>> {
    let bytes = read_migration_secret(path, MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES)?;
    TeslaMatePostgresPassword::from_bytes(&bytes).map_err(Into::into)
}

#[cfg(unix)]
fn read_bounded_migration_secret(
    reader: impl Read,
    maximum: usize,
) -> Result<zeroize::Zeroizing<Vec<u8>>, MigrationSecretReadError> {
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(maximum.min(8 * 1024)));
    reader
        .take(u64::try_from(maximum + 1).expect("secret cap fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|_| MigrationSecretReadError::Read)?;
    if bytes.len() > maximum {
        return Err(MigrationSecretReadError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn read_migration_secret_file(
    path: &Path,
    maximum: usize,
) -> Result<zeroize::Zeroizing<Vec<u8>>, MigrationSecretReadError> {
    read_migration_secret_file_with_hooks(path, maximum, || {}, || {})
}

#[cfg(unix)]
fn read_migration_secret_file_with_hooks(
    path: &Path,
    maximum: usize,
    after_open: impl FnOnce(),
    after_read: impl FnOnce(),
) -> Result<zeroize::Zeroizing<Vec<u8>>, MigrationSecretReadError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| {
        if error == Errno::LOOP {
            MigrationSecretReadError::UnsafeFile
        } else {
            MigrationSecretReadError::Read
        }
    })?;
    let initial = fstat(&descriptor).map_err(|_| MigrationSecretReadError::Read)?;
    if !safe_migration_secret_stat(&initial) {
        return Err(MigrationSecretReadError::UnsafeFile);
    }
    let flags = fcntl_getfl(&descriptor).map_err(|_| MigrationSecretReadError::Read)?;
    fcntl_setfl(&descriptor, flags & !OFlags::NONBLOCK)
        .map_err(|_| MigrationSecretReadError::Read)?;
    after_open();

    let file: fs::File = descriptor.into();
    let bytes = read_bounded_migration_secret(&file, maximum)?;
    after_read();
    let final_descriptor = fstat(&file).map_err(|_| MigrationSecretReadError::Read)?;
    if !same_migration_secret_stat(&initial, &final_descriptor) {
        return Err(MigrationSecretReadError::IdentityChanged);
    }
    let current =
        fs::symlink_metadata(path).map_err(|_| MigrationSecretReadError::IdentityChanged)?;
    if current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.uid() != initial.st_uid
        || current.dev() != initial.st_dev as u64
        || current.ino() != initial.st_ino
        || current.mode() != initial.st_mode as u32
        || current.len() != initial.st_size as u64
        || current.mtime() != initial.st_mtime
        || current.mtime_nsec() != initial.st_mtime_nsec as i64
        || !safe_migration_secret_metadata(&current)
    {
        return Err(MigrationSecretReadError::IdentityChanged);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn safe_migration_secret_stat(stat: &rustix::fs::Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode).is_file()
        && stat.st_uid == getuid().as_raw()
        && (stat.st_mode & 0o077) == 0
        && (stat.st_mode & 0o400) != 0
}

#[cfg(unix)]
fn same_migration_secret_stat(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_uid == right.st_uid
        && left.st_mode == right.st_mode
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
}

#[cfg(unix)]
fn safe_migration_secret_metadata(metadata: &fs::Metadata) -> bool {
    metadata.uid() == getuid().as_raw()
        && (metadata.mode() & 0o077) == 0
        && (metadata.mode() & 0o400) != 0
}

fn observation_command_error(
    command: &str,
    car_id: i64,
    error: ObservationVerificationError,
) -> Box<dyn std::error::Error> {
    println!(
        "{}",
        serde_json::json!({
            "status": "error",
            "command": command,
            "carId": car_id,
            "errorCode": error.code(),
        })
    );
    Box::new(std::io::Error::other(error.to_string()))
}

fn render_pairing_qr(
    pairing_uri: &str,
) -> Result<zeroize::Zeroizing<String>, qrcode::types::QrError> {
    Ok(zeroize::Zeroizing::new(
        QrCode::new(pairing_uri.as_bytes())?
            .render::<Dense1x2>()
            .quiet_zone(true)
            .build(),
    ))
}

const MAX_TLS_CERTIFICATE_CHAIN_BYTES: usize = server::MAX_TLS_CERTIFICATE_CHAIN_BYTES;
const MAX_TLS_PRIVATE_KEY_BYTES: usize = server::MAX_TLS_PRIVATE_KEY_BYTES;

#[derive(Debug, thiserror::Error)]
enum PairingCommandError {
    #[error("TLS certificate cannot be validated")]
    Certificate(#[source] std::io::Error),
    #[error("pairing expiry is invalid")]
    InvalidExpiry,
    #[error("pairing presentation cannot be constructed")]
    Presentation,
    #[error("pairing invitation cannot be persisted")]
    Persist(#[source] Box<teslatlas_hub::db::StoreError>),
    #[error("pairing invitation persistence failed and revocation could not be confirmed")]
    PersistAndRevoke {
        persist: Box<teslatlas_hub::db::StoreError>,
        revoke: Box<teslatlas_hub::db::StoreError>,
    },
    #[error("pairing presentation failed; invitation was revoked ({kind:?})")]
    Present { kind: std::io::ErrorKind },
    #[error("pairing presentation failed and invitation revocation failed ({kind:?})")]
    PresentAndRevoke {
        kind: std::io::ErrorKind,
        #[source]
        revoke: Box<teslatlas_hub::db::StoreError>,
    },
}

struct PairingCommandInput<'a> {
    label: &'a str,
    expires_in_seconds: u64,
    json: bool,
    public_url: &'a str,
    certificate_path: &'a Path,
    private_key_path: &'a Path,
    created_at_ms: i64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingJsonPresentation<'a> {
    pairing_id: uuid::Uuid,
    secret: &'a str,
    expires_at_ms: i64,
    endpoint: &'a str,
    tls_pin: &'a str,
    pairing_uri: &'a str,
}

async fn execute_pairing_at<W: Write>(
    store: &HubStore,
    input: PairingCommandInput<'_>,
    writer: &mut W,
) -> Result<(), PairingCommandError> {
    // Read and validate the exact configured identity before generating any
    // one-time secret. These bytes take the same Rustls construction path as
    // Serve, after the pairing-specific no-follow and identity checks.
    let certificate_pem = read_tls_identity_file(
        input.certificate_path,
        MAX_TLS_CERTIFICATE_CHAIN_BYTES,
        false,
    )
    .map_err(PairingCommandError::Certificate)?;
    let private_key_pem =
        read_tls_identity_file(input.private_key_path, MAX_TLS_PRIVATE_KEY_BYTES, true)
            .map_err(PairingCommandError::Certificate)?;
    let tls_pin = leaf_certificate_sha256_from_pem(&certificate_pem)
        .map_err(PairingCommandError::Certificate)?;
    teslatlas_hub::server::rustls_config_from_pem_identity(certificate_pem, private_key_pem)
        .await
        .map_err(|_| {
            PairingCommandError::Certificate(std::io::Error::other(
                "TLS identity cannot be validated",
            ))
        })?;
    if input.expires_in_seconds == 0 {
        return Err(PairingCommandError::InvalidExpiry);
    }
    let ttl_ms = input
        .expires_in_seconds
        .checked_mul(1_000)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(PairingCommandError::InvalidExpiry)?;
    let expires_at_ms = input
        .created_at_ms
        .checked_add(ttl_ms)
        .ok_or(PairingCommandError::InvalidExpiry)?;
    let invitation = store
        .prepare_pairing(input.label, input.created_at_ms, expires_at_ms)
        .map_err(|error| PairingCommandError::Persist(Box::new(error)))?;
    let pairing_uri = pairing_uri(
        input.public_url,
        &tls_pin,
        invitation.pairing_id,
        invitation.secret(),
    )
    .map_err(|_| PairingCommandError::Presentation)?;
    let presentation = build_pairing_presentation(
        input.json,
        input.public_url,
        &tls_pin,
        &pairing_uri,
        &invitation,
        input.expires_in_seconds,
    )?;
    let pairing_id = invitation.pairing_id;
    persist_and_present_pairing(
        writer,
        &presentation,
        || store.persist_pairing(input.label, &invitation),
        || store.revoke_pairing(pairing_id),
    )
}

fn build_pairing_presentation(
    json: bool,
    public_url: &str,
    tls_pin: &str,
    pairing_uri: &str,
    invitation: &teslatlas_hub::db::PairingInvitation,
    expires_in_seconds: u64,
) -> Result<zeroize::Zeroizing<Vec<u8>>, PairingCommandError> {
    let mut output = zeroize::Zeroizing::new(Vec::new());
    if json {
        let maximum_json_bytes = public_url
            .len()
            .saturating_add(tls_pin.len())
            .saturating_add(pairing_uri.len())
            .saturating_add(invitation.secret().len())
            .saturating_mul(6)
            .saturating_add(512);
        output
            .try_reserve_exact(maximum_json_bytes)
            .map_err(|_| PairingCommandError::Presentation)?;
        let document = PairingJsonPresentation {
            pairing_id: invitation.pairing_id,
            secret: invitation.secret(),
            expires_at_ms: invitation.expires_at_ms,
            endpoint: public_url,
            tls_pin,
            pairing_uri,
        };
        serde_json::to_writer(&mut *output, &document)
            .map_err(|_| PairingCommandError::Presentation)?;
        output.push(b'\n');
    } else {
        let qr = render_pairing_qr(pairing_uri).map_err(|_| PairingCommandError::Presentation)?;
        output
            .try_reserve_exact(qr.len().saturating_add(128))
            .map_err(|_| PairingCommandError::Presentation)?;
        write!(
            output,
            "Scan with Teslatlas:\n{}\nExpires in {expires_in_seconds} seconds.\n",
            qr.as_str()
        )
        .map_err(|_| PairingCommandError::Presentation)?;
    }
    Ok(output)
}

fn persist_and_present_pairing<W, P, R>(
    writer: &mut W,
    presentation: &[u8],
    persist: P,
    revoke: R,
) -> Result<(), PairingCommandError>
where
    W: Write,
    P: FnOnce() -> Result<(), teslatlas_hub::db::StoreError>,
    R: FnOnce() -> Result<(), teslatlas_hub::db::StoreError>,
{
    if let Err(persist) = persist() {
        return match revoke() {
            Ok(()) => Err(PairingCommandError::Persist(Box::new(persist))),
            Err(revoke) => Err(PairingCommandError::PersistAndRevoke {
                persist: Box::new(persist),
                revoke: Box::new(revoke),
            }),
        };
    }
    if let Err(error) = writer.write_all(presentation).and_then(|()| writer.flush()) {
        let kind = error.kind();
        return match revoke() {
            Ok(()) => Err(PairingCommandError::Present { kind }),
            Err(revoke) => Err(PairingCommandError::PresentAndRevoke {
                kind,
                revoke: Box::new(revoke),
            }),
        };
    }
    Ok(())
}

/// SHA-256 pin for the first PEM certificate (the server leaf) in the active
/// TLS chain. Pairing carries this non-secret value so an iPhone can pin the
/// exact identity before it sends its single-use claim secret.
#[cfg(test)]
fn leaf_certificate_sha256(certificate_path: &std::path::Path) -> Result<String, std::io::Error> {
    leaf_certificate_sha256_after_open(certificate_path, || {})
}

#[cfg(test)]
fn leaf_certificate_sha256_after_open(
    certificate_path: &Path,
    after_open: impl FnOnce(),
) -> Result<String, std::io::Error> {
    let pem = read_tls_identity_file_after_open(
        certificate_path,
        MAX_TLS_CERTIFICATE_CHAIN_BYTES,
        false,
        after_open,
    )?;
    leaf_certificate_sha256_from_pem(&pem)
}

fn read_tls_identity_file(
    path: &Path,
    maximum: usize,
    private: bool,
) -> Result<zeroize::Zeroizing<Vec<u8>>, std::io::Error> {
    server::read_tls_identity_file(path, maximum, private)
}

#[cfg(test)]
fn read_tls_identity_file_after_open(
    path: &Path,
    maximum: usize,
    private: bool,
    after_open: impl FnOnce(),
) -> Result<zeroize::Zeroizing<Vec<u8>>, std::io::Error> {
    server::read_tls_identity_file_after_open(path, maximum, private, after_open)
}

fn leaf_certificate_sha256_from_pem(pem: &[u8]) -> Result<String, std::io::Error> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};

    let leaf = CertificateDer::pem_slice_iter(pem)
        .next()
        .transpose()
        .map_err(|_| std::io::Error::other("TLS certificate PEM is invalid"))?
        .ok_or_else(|| std::io::Error::other("TLS certificate has no PEM leaf"))?;
    Ok(hex::encode(Sha256::digest(leaf.as_ref())))
}

fn pairing_uri(
    endpoint: &str,
    tls_pin: &str,
    pairing_id: uuid::Uuid,
    secret: &str,
) -> Result<zeroize::Zeroizing<String>, std::collections::TryReserveError> {
    const PREFIX: &str = "teslatlas-hub://pair?";
    let pairing_id = pairing_id.to_string();
    let maximum_encoded_values = endpoint
        .len()
        .saturating_add(pairing_id.len())
        .saturating_add(secret.len())
        .saturating_add(tls_pin.len())
        .saturating_mul(3);
    let maximum_uri_bytes = PREFIX
        .len()
        .saturating_add("endpoint=".len())
        .saturating_add("&pairing_id=".len())
        .saturating_add("&secret=".len())
        .saturating_add("&tls_pin=".len())
        .saturating_add(maximum_encoded_values);
    let mut allocation = String::new();
    allocation.try_reserve_exact(maximum_uri_bytes)?;
    let mut uri = zeroize::Zeroizing::new(allocation);
    uri.push_str(PREFIX);
    {
        let mut query = url::form_urlencoded::Serializer::for_suffix(&mut *uri, PREFIX.len());
        query.append_pair("endpoint", endpoint);
        query.append_pair("pairing_id", &pairing_id);
        query.append_pair("secret", secret);
        query.append_pair("tls_pin", tls_pin);
        let _ = query.finish();
    }
    debug_assert!(uri.len() <= maximum_uri_bytes);
    Ok(uri)
}

fn current_epoch_ms() -> Result<i64, std::io::Error> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| std::io::Error::other("system clock exceeds epoch milliseconds"))
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::path::PathBuf;
    use std::{
        fs,
        io::{self, Write},
        os::unix::fs::{PermissionsExt, symlink},
        path::Path,
        time::Duration,
    };
    #[cfg(target_os = "macos")]
    use std::{
        future::pending,
        net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
        process::{Child, Command as ProcessCommand, Stdio},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use clap::Parser;
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use sha2::Digest;
    #[cfg(target_os = "macos")]
    use sha2::Sha256;

    use super::{
        Cli, Command, ControlCommand, MAX_TLS_CERTIFICATE_CHAIN_BYTES, MAX_TLS_PRIVATE_KEY_BYTES,
        PairingCommandError, PairingCommandInput, execute_pairing_at, leaf_certificate_sha256,
        leaf_certificate_sha256_after_open, pairing_uri, persist_and_present_pairing,
        read_tls_identity_file, render_pairing_qr, run, run_immutable_diagnostic_with,
    };
    #[cfg(target_os = "macos")]
    use super::{
        MAX_MIGRATION_ENCRYPTION_KEY_BYTES, MAX_MIGRATION_POSTGRES_PASSWORD_BYTES,
        MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES, MAX_MIGRATION_TOKEN_BYTES,
        MAX_MIGRATION_TOKEN_FILE_BYTES, MacCommandProxySpec, MacServeControl,
        MacServeWorkerStopTimeout, MigrationSecretReadError, ServiceCommand,
        clear_provider_credentials, command_requires_user_hub_admission, decode_setup_fleet_stdin,
        migration_start_requested, migration_stop_confirmed, persist_fleet_setup_and_drop_legacy,
        persist_legacy_setup_and_drop_fleet, persist_migrated_legacy_tokens,
        read_migration_encryption_key, read_migration_postgres_password, read_migration_secret,
        read_migration_secret_file_with_hooks, run_macos_serve_supervisor,
        teslamate_check_failure_details, validate_legacy_setup_provider,
        validate_streaming_setting,
    };
    use teslatlas_hub::db::HubStore;
    #[cfg(target_os = "macos")]
    use teslatlas_hub::protocol::{
        CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V3, OpaqueCursor, PROTOCOL_V1,
    };
    #[cfg(target_os = "macos")]
    use teslatlas_hub::{
        teslamate_reader::TeslaMateReaderError, teslamate_schema::SchemaCompatibilityError,
    };
    use uuid::Uuid;

    #[cfg(target_os = "macos")]
    fn supervisor_cursor_proof(cursor_key: &CursorKey) -> String {
        OpaqueCursor::issue(
            cursor_key,
            CursorClaims {
                protocol: PROTOCOL_V1,
                schema: HUB_PROJECTION_SCHEMA_V3,
                installation_id: Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
                account_id: Uuid::from_u128(0x22222222_2222_4222_8222_222222222222),
                vehicle_id: Uuid::from_u128(0x33333333_3333_4333_8333_333333333333),
                generation: 7,
                sequence: 11,
            },
        )
        .expect("test cursor")
        .as_str()
        .to_owned()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_command_proxy_arguments_use_private_data_paths() {
        let spec = MacCommandProxySpec {
            executable: PathBuf::from(
                "/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy",
            ),
            host: "127.0.0.1".to_owned(),
            port: 4443,
            command_key: PathBuf::from("/private/data/secrets/fleet-command-key.pem"),
            certificate: PathBuf::from("/private/data/fleet-proxy-tls-cert.pem"),
            tls_key: PathBuf::from("/private/data/secrets/fleet-proxy-tls-key.pem"),
            session_cache: PathBuf::from("/private/data/fleet-command-session-cache.json"),
        };
        assert_eq!(
            spec.arguments(),
            vec![
                "-host",
                "127.0.0.1",
                "-port",
                "4443",
                "-key-file",
                "/private/data/secrets/fleet-command-key.pem",
                "-cert",
                "/private/data/fleet-proxy-tls-cert.pem",
                "-tls-key",
                "/private/data/secrets/fleet-proxy-tls-key.pem",
                "-session-cache",
                "/private/data/fleet-command-session-cache.json",
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migration_stop_and_start_prompts_are_independent_and_default_to_no() {
        assert!(!migration_stop_confirmed(""));
        assert!(!migration_stop_confirmed("n"));
        assert!(migration_stop_confirmed(" Y\n"));
        assert!(!migration_start_requested(""));
        assert!(!migration_start_requested("N"));
        assert!(migration_start_requested("y"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn teslamate_check_failures_are_redacted_and_actionable() {
        let older = TeslaMateReaderError::Schema(SchemaCompatibilityError::LegacyMigration {
            found: 1,
            minimum: 2,
        });
        let newer = TeslaMateReaderError::Schema(SchemaCompatibilityError::UnreviewedMigration {
            found: 3,
            maximum: 2,
        });
        let selected = TeslaMateReaderError::SelectedCarMissing { selected_car_id: 7 };
        let ambiguous = TeslaMateReaderError::AmbiguousOpenSession {
            drives: 2,
            charges: 1,
            states: 1,
        };

        assert_eq!(
            teslamate_check_failure_details(&older).1,
            "older_than_4_1_1"
        );
        assert_eq!(teslamate_check_failure_details(&older).2, Some(1));
        assert!(teslamate_check_failure_details(&older).3.contains("update"));
        assert_eq!(
            teslamate_check_failure_details(&newer).1,
            "newer_than_4_1_1"
        );
        assert!(
            teslamate_check_failure_details(&newer)
                .3
                .contains("Do not downgrade")
        );
        assert_eq!(
            teslamate_check_failure_details(&selected).1,
            "selected_car_missing"
        );
        assert!(!teslamate_check_failure_details(&selected).3.contains('7'));
        assert_eq!(
            teslamate_check_failure_details(&ambiguous).1,
            "ambiguous_open_session"
        );
        assert!(!teslamate_check_failure_details(&ambiguous).3.contains("2"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn onboarding_migration_cli_is_explicit_and_noninteractive() {
        let check = Cli::try_parse_from([
            "teslatlas-hub",
            "teslamate-check",
            "--source",
            "postgresql://reader@localhost/teslamate",
            "--car-id",
            "7",
            "--postgres-password-file",
            "password",
        ])
        .expect("compatibility-check CLI");
        assert!(matches!(
            check.command,
            Command::TeslaMateCheck { car_id: 7, .. }
        ));
        assert!(!command_requires_user_hub_admission(&check.command));

        let migration = Cli::try_parse_from([
            "teslatlas-hub",
            "migrate",
            "--source",
            "postgresql://reader@localhost/teslamate",
            "--car-id",
            "7",
            "--postgres-password-file",
            "password",
            "--encryption-key-file",
            "key",
            "--online-snapshot",
        ])
        .expect("online migration CLI");
        assert!(matches!(
            migration.command,
            Command::Migrate {
                online_snapshot: true,
                ..
            }
        ));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn teslamate_check_invalid_source_does_not_create_hub_target() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = temporary.path().join("absent/config.toml");
        let cli = Cli::try_parse_from([
            "teslatlas-hub",
            "--config",
            config.to_str().expect("UTF-8 config path"),
            "teslamate-check",
            "--source",
            "not-a-postgres-url",
            "--car-id",
            "7",
            "--postgres-password-file",
            "unused",
        ])
        .expect("compatibility-check CLI");

        let error = run(cli).await.expect_err("invalid source must fail");
        assert!(error.to_string().contains("see JSON report"));
        assert!(!config.exists());
        assert!(!config.parent().expect("config parent").exists());
    }

    #[cfg(unix)]
    #[test]
    fn bootstrap_and_explicit_vehicle_commands_parse() {
        let bootstrap = Cli::try_parse_from(["teslatlas-hub", "bootstrap"]).expect("bootstrap CLI");
        assert!(matches!(bootstrap.command, Command::Bootstrap));
        assert!(Cli::try_parse_from(["teslatlas-hub", "control", "wake"]).is_err());
        let wake = Cli::try_parse_from([
            "teslatlas-hub",
            "control",
            "--vehicle-id",
            "00000000-0000-0000-0000-000000000001",
            "wake",
            "--confirm",
        ])
        .expect("confirmed wake CLI");
        assert!(matches!(
            wake.command,
            Command::Control {
                vehicle_id: Some(_),
                command: ControlCommand::Wake { confirm: true }
            }
        ));
        assert!(
            Cli::try_parse_from(["teslatlas-hub", "control", "climate-start", "--confirm"]).is_ok()
        );
    }

    #[test]
    fn paired_device_controls_parse_without_vehicle_selection() {
        let list = Cli::try_parse_from(["teslatlas-hub", "control", "paired-devices"])
            .expect("paired-device list CLI");
        assert!(matches!(
            list.command,
            Command::Control {
                command: ControlCommand::PairedDevices,
                ..
            }
        ));
        let revoke = Cli::try_parse_from([
            "teslatlas-hub",
            "control",
            "revoke-device",
            "00000000-0000-0000-0000-000000000001",
        ])
        .expect("paired-device revoke CLI");
        assert!(matches!(
            revoke.command,
            Command::Control {
                command: ControlCommand::RevokeDevice { .. },
                ..
            }
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migration_secret_reader_accepts_each_exact_cap_and_rejects_next_byte() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        for (name, maximum) in [
            ("postgres", MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES),
            ("token", MAX_MIGRATION_TOKEN_FILE_BYTES),
            ("key", MAX_MIGRATION_ENCRYPTION_KEY_BYTES),
        ] {
            let path = temporary.path().join(name);
            fs::write(&path, vec![b'x'; maximum]).expect("exact secret");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .expect("safe secret mode");
            assert_eq!(
                read_migration_secret(&path, maximum)
                    .expect("exact cap")
                    .len(),
                maximum
            );
            fs::write(&path, vec![b'x'; maximum + 1]).expect("oversized secret");
            assert!(read_migration_secret(&path, maximum).is_err());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migration_encryption_key_reader_accepts_normal_line_endings() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("encryption-key");
        for ending in [b"".as_slice(), b"\n".as_slice(), b"\r\n".as_slice()] {
            let mut value = b"teslamate-key".to_vec();
            value.extend_from_slice(ending);
            fs::write(&path, value).expect("key file");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("key mode");
            assert_eq!(
                read_migration_encryption_key(&path)
                    .expect("line ending is not part of the key")
                    .as_slice(),
                b"teslamate-key"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migration_password_and_token_semantic_boundaries_include_line_endings() {
        use teslatlas_hub::teslamate_token::{
            CLOAK_ENVELOPE_OVERHEAD_BYTES, MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES,
            encrypt_legacy_owner_token_files,
        };

        let temporary = tempfile::tempdir().expect("temporary directory");
        let password_path = temporary.path().join("password");
        for ending in [b"".as_slice(), b"\n".as_slice(), b"\r\n".as_slice()] {
            let mut value = vec![b'p'; MAX_MIGRATION_POSTGRES_PASSWORD_BYTES];
            value.extend_from_slice(ending);
            fs::write(&password_path, value).expect("password file");
            fs::set_permissions(&password_path, fs::Permissions::from_mode(0o600))
                .expect("password mode");
            assert_eq!(
                read_migration_postgres_password(&password_path)
                    .expect("semantic password cap")
                    .as_str()
                    .len(),
                MAX_MIGRATION_POSTGRES_PASSWORD_BYTES
            );
        }
        fs::write(
            &password_path,
            vec![b'p'; MAX_MIGRATION_POSTGRES_PASSWORD_BYTES + 1],
        )
        .expect("oversized password");
        fs::set_permissions(&password_path, fs::Permissions::from_mode(0o600))
            .expect("password mode");
        assert!(read_migration_postgres_password(&password_path).is_err());

        assert_eq!(MAX_MIGRATION_TOKEN_BYTES, 16 * 1024);
        assert_eq!(
            MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES,
            MAX_MIGRATION_TOKEN_BYTES + CLOAK_ENVELOPE_OVERHEAD_BYTES
        );
        let access_path = temporary.path().join("access");
        let refresh_path = temporary.path().join("refresh");
        fs::write(
            &access_path,
            [vec![b'a'; MAX_MIGRATION_TOKEN_BYTES], b"\n".to_vec()].concat(),
        )
        .expect("access token");
        fs::write(
            &refresh_path,
            [vec![b'b'; MAX_MIGRATION_TOKEN_BYTES], b"\r\n".to_vec()].concat(),
        )
        .expect("refresh token");
        for path in [&access_path, &refresh_path] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("token mode");
        }
        let (access, refresh) = encrypt_legacy_owner_token_files(
            b"boundary-test-key",
            read_migration_secret(&access_path, MAX_MIGRATION_TOKEN_FILE_BYTES)
                .expect("bounded access token"),
            read_migration_secret(&refresh_path, MAX_MIGRATION_TOKEN_FILE_BYTES)
                .expect("bounded refresh token"),
        )
        .expect("semantic token cap");
        assert_eq!(access.len(), MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES);
        assert_eq!(refresh.len(), MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES);

        fs::write(&access_path, vec![b'a'; MAX_MIGRATION_TOKEN_BYTES + 1])
            .expect("oversized access token");
        fs::set_permissions(&access_path, fs::Permissions::from_mode(0o600)).expect("token mode");
        assert!(
            encrypt_legacy_owner_token_files(
                b"boundary-test-key",
                read_migration_secret(&access_path, MAX_MIGRATION_TOKEN_FILE_BYTES)
                    .expect("raw bounded access token"),
                read_migration_secret(&refresh_path, MAX_MIGRATION_TOKEN_FILE_BYTES)
                    .expect("bounded refresh token"),
            )
            .is_err()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migration_secret_files_require_private_nofollow_stable_descriptors() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("secret");
        fs::write(&path, b"migration-secret").expect("secret");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secret mode");
        assert_eq!(
            read_migration_secret(&path, 64)
                .expect("safe secret")
                .as_slice(),
            b"migration-secret"
        );

        let outside = temporary.path().join("outside");
        fs::write(&outside, b"outside-secret").expect("outside secret");
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).expect("outside mode");
        let linked = temporary.path().join("linked");
        symlink(&outside, &linked).expect("secret symlink");
        assert!(matches!(
            read_migration_secret(&linked, 64),
            Err(error) if error.downcast_ref::<MigrationSecretReadError>() == Some(&MigrationSecretReadError::UnsafeFile)
        ));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("unsafe mode");
        assert!(matches!(
            read_migration_secret(&path, 64),
            Err(error) if error.downcast_ref::<MigrationSecretReadError>() == Some(&MigrationSecretReadError::UnsafeFile)
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("safe mode");

        let replacement = temporary.path().join("replacement");
        fs::write(&replacement, b"replacement-secret").expect("replacement");
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))
            .expect("replacement mode");
        assert_eq!(
            read_migration_secret_file_with_hooks(
                &path,
                64,
                || { fs::rename(&replacement, &path).expect("replace secret") },
                || {}
            )
            .expect_err("replacement race"),
            MigrationSecretReadError::IdentityChanged
        );

        fs::write(&path, b"stable-secret").expect("restore secret");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore mode");
        assert_eq!(
            read_migration_secret_file_with_hooks(
                &path,
                64,
                || {},
                || { fs::write(&path, b"same-inode-secret-mutated").expect("mutate secret") }
            )
            .expect_err("same inode mutation"),
            MigrationSecretReadError::IdentityChanged
        );

        let error = read_migration_secret(&linked, 64).expect_err("unsafe link");
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("outside-secret"));
        assert!(!rendered.contains(&linked.display().to_string()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migration_secret_reader_rejects_a_fifo_without_waiting_for_a_writer() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("secret.fifo");
        assert!(
            ProcessCommand::new("mkfifo")
                .arg(&path)
                .status()
                .expect("run mkfifo")
                .success()
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("FIFO mode");

        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(matches!(
                    read_migration_secret(&path, 64),
                    Err(error)
                        if error.downcast_ref::<MigrationSecretReadError>()
                            == Some(&MigrationSecretReadError::UnsafeFile)
                ))
                .expect("send FIFO result");
        });
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("FIFO admission must not block")
        );
        worker.join().expect("FIFO admission worker");
    }

    #[cfg(target_os = "macos")]
    struct MacServeDropWitness {
        label: &'static str,
        drops: tokio::sync::mpsc::UnboundedSender<&'static str>,
    }

    #[cfg(target_os = "macos")]
    impl Drop for MacServeDropWitness {
        fn drop(&mut self) {
            let _ = self.drops.send(self.label);
        }
    }

    #[cfg(target_os = "macos")]
    const MACOS_SUPERVISOR_SIGTERM_CHILD_ENV: &str =
        "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_CHILD";
    #[cfg(target_os = "macos")]
    const MACOS_SUPERVISOR_SIGTERM_CHILD_BIND_ENV: &str =
        "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_BIND";
    #[cfg(target_os = "macos")]
    const MACOS_SUPERVISOR_SIGTERM_CHILD_RECEIPT_ENV: &str =
        "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_RECEIPT";
    #[cfg(target_os = "macos")]
    const MACOS_SUPERVISOR_SIGTERM_CHILD_FIXTURE_ENV: &str =
        "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_FIXTURE";
    #[cfg(target_os = "macos")]
    const MACOS_SUPERVISOR_SIGTERM_CHILD_RUN_ENV: &str =
        "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_RUN";
    #[cfg(target_os = "macos")]
    const MACOS_SUPERVISOR_SIGTERM_CHILD_TEST: &str = "tests::macos_supervisor_sigterm_child";
    #[cfg(target_os = "macos")]
    const MACOS_SUPERVISOR_SIGTERM_BOUND: Duration = Duration::from_secs(3);

    #[cfg(target_os = "macos")]
    fn macos_supervisor_sigterm_receipt(
        phase: &str,
        run: u8,
        bind: SocketAddr,
        fixture_heads_sha256: &str,
        cursor_proof: &str,
        collector_stopped: usize,
        listener_stopped: usize,
    ) -> String {
        format!(
            "phase={phase}\nrun={run}\nbind={bind}\nfixture_heads_sha256={fixture_heads_sha256}\ncursor_proof={cursor_proof}\nfake_collector_outbound=0\ncollector_stopped={collector_stopped}\nlistener_stopped={listener_stopped}\n"
        )
    }

    #[cfg(target_os = "macos")]
    async fn wait_for_macos_supervisor_sigterm_receipt(
        receipt_path: &Path,
        expected: &str,
    ) -> Result<(), String> {
        tokio::time::timeout(MACOS_SUPERVISOR_SIGTERM_BOUND, async {
            loop {
                match fs::read(receipt_path) {
                    Ok(receipt) if receipt == expected.as_bytes() => return Ok(()),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!(
                            "read SIGTERM child receipt {}: {error}",
                            receipt_path.display()
                        ));
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| {
            format!(
                "timed out waiting for SIGTERM child receipt {}",
                receipt_path.display()
            )
        })?
    }

    #[cfg(target_os = "macos")]
    async fn wait_for_macos_supervisor_sigterm_child(
        child: &mut Child,
        phase: &str,
    ) -> Result<std::process::ExitStatus, String> {
        tokio::time::timeout(MACOS_SUPERVISOR_SIGTERM_BOUND, async {
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return Ok(status),
                    Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
                    Err(error) => {
                        return Err(format!("inspect SIGTERM child during {phase}: {error}"));
                    }
                }
            }
        })
        .await
        .map_err(|_| format!("SIGTERM child did not exit during {phase}"))?
    }

    #[cfg(target_os = "macos")]
    async fn reap_macos_supervisor_sigterm_child(child: &mut Child) -> Result<(), String> {
        if child
            .try_wait()
            .map_err(|error| format!("inspect SIGTERM child cleanup: {error}"))?
            .is_some()
        {
            return Ok(());
        }
        let _ = child.kill();
        wait_for_macos_supervisor_sigterm_child(child, "forced cleanup")
            .await
            .map(|_| ())
    }

    #[cfg(target_os = "macos")]
    fn send_sigterm_to_macos_supervisor_child(child: &Child) -> Result<(), String> {
        let raw_pid = i32::try_from(child.id())
            .map_err(|_| "SIGTERM child PID does not fit i32".to_owned())?;
        let pid = rustix::process::Pid::from_raw(raw_pid)
            .ok_or_else(|| "SIGTERM child has an invalid PID".to_owned())?;
        rustix::process::kill_process(pid, rustix::process::Signal::TERM)
            .map_err(|error| format!("send SIGTERM to child: {error}"))
    }

    #[cfg(target_os = "macos")]
    async fn run_macos_supervisor_sigterm_child_cycle(
        test_binary: &Path,
        fixture_path: &Path,
        bind: SocketAddr,
        run: u8,
    ) -> Result<(), String> {
        let fixture = fs::read(fixture_path)
            .map_err(|error| format!("read stable fixture {}: {error}", fixture_path.display()))?;
        let fixture_heads_sha256 = hex::encode(Sha256::digest(&fixture));
        let expected_cursor_proof = supervisor_cursor_proof(&CursorKey::from_bytes([0xB7; 32]));
        let receipt_path = fixture_path.with_extension(format!("sigterm-receipt-{run}"));
        let mut child = ProcessCommand::new(test_binary)
            .args([
                "--exact",
                MACOS_SUPERVISOR_SIGTERM_CHILD_TEST,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(MACOS_SUPERVISOR_SIGTERM_CHILD_ENV, "1")
            .env(MACOS_SUPERVISOR_SIGTERM_CHILD_BIND_ENV, bind.to_string())
            .env(MACOS_SUPERVISOR_SIGTERM_CHILD_RECEIPT_ENV, &receipt_path)
            .env(MACOS_SUPERVISOR_SIGTERM_CHILD_FIXTURE_ENV, fixture_path)
            .env(MACOS_SUPERVISOR_SIGTERM_CHILD_RUN_ENV, run.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("spawn SIGTERM child: {error}"))?;

        let ready = macos_supervisor_sigterm_receipt(
            "ready",
            run,
            bind,
            &fixture_heads_sha256,
            &expected_cursor_proof,
            0,
            0,
        );
        let completed = macos_supervisor_sigterm_receipt(
            "stopped",
            run,
            bind,
            &fixture_heads_sha256,
            &expected_cursor_proof,
            1,
            1,
        );
        let result = async {
            wait_for_macos_supervisor_sigterm_receipt(&receipt_path, &ready).await?;
            TcpStream::connect_timeout(&bind, Duration::from_millis(250)).map_err(|error| {
                format!("SIGTERM child did not expose its loopback listener: {error}")
            })?;
            match TcpListener::bind(bind) {
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
                Err(error) => {
                    return Err(format!(
                        "unexpected loopback bind error while SIGTERM child is live: {error}"
                    ));
                }
                Ok(listener) => {
                    drop(listener);
                    return Err("SIGTERM child wrote ready before binding its listener".to_owned());
                }
            }
            send_sigterm_to_macos_supervisor_child(&child)?;
            let status = wait_for_macos_supervisor_sigterm_child(&mut child, "SIGTERM").await?;
            if !status.success() {
                return Err(format!("SIGTERM child exited unsuccessfully: {status}"));
            }
            wait_for_macos_supervisor_sigterm_receipt(&receipt_path, &completed).await?;
            let after = fs::read(fixture_path).map_err(|error| {
                format!(
                    "read fixture after SIGTERM child {}: {error}",
                    fixture_path.display()
                )
            })?;
            if after != fixture {
                return Err("SIGTERM child changed stable fixture heads".to_owned());
            }
            Ok(())
        }
        .await;

        if result.is_err() {
            reap_macos_supervisor_sigterm_child(&mut child).await?;
        }
        result
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_sigterm_child() {
        if std::env::var_os(MACOS_SUPERVISOR_SIGTERM_CHILD_ENV).is_none() {
            return;
        }

        let bind = std::env::var(MACOS_SUPERVISOR_SIGTERM_CHILD_BIND_ENV)
            .expect("SIGTERM child bind")
            .parse::<SocketAddr>()
            .expect("SIGTERM child loopback bind");
        assert!(bind.ip().is_loopback(), "SIGTERM child must bind loopback");
        let receipt_path = PathBuf::from(
            std::env::var_os(MACOS_SUPERVISOR_SIGTERM_CHILD_RECEIPT_ENV)
                .expect("SIGTERM child receipt path"),
        );
        let fixture_path = PathBuf::from(
            std::env::var_os(MACOS_SUPERVISOR_SIGTERM_CHILD_FIXTURE_ENV)
                .expect("SIGTERM child fixture path"),
        );
        let run = std::env::var(MACOS_SUPERVISOR_SIGTERM_CHILD_RUN_ENV)
            .expect("SIGTERM child run")
            .parse::<u8>()
            .expect("SIGTERM child run number");
        let fixture_heads_sha256 = hex::encode(Sha256::digest(
            fs::read(&fixture_path).expect("read SIGTERM child stable fixture"),
        ));
        let cursor_key = CursorKey::from_bytes([0xB7; 32]);
        let cursor_proof = supervisor_cursor_proof(&cursor_key);
        let ready = macos_supervisor_sigterm_receipt(
            "ready",
            run,
            bind,
            &fixture_heads_sha256,
            &cursor_proof,
            0,
            0,
        );
        let stopped = macos_supervisor_sigterm_receipt(
            "stopped",
            run,
            bind,
            &fixture_heads_sha256,
            &cursor_proof,
            1,
            1,
        );
        let collector_stopped = Arc::new(AtomicUsize::new(0));
        let server_stopped = Arc::new(AtomicUsize::new(0));
        let collector_stopped_for_task = Arc::clone(&collector_stopped);
        let server_stopped_for_task = Arc::clone(&server_stopped);
        let ready_receipt_path = receipt_path.clone();
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM listener in child");

        run_macos_serve_supervisor(
            true,
            move |ready_tx, shutdown| async move {
                ready_tx
                    .send(cursor_key)
                    .map_err(|_| std::io::Error::other("SIGTERM child lost collector readiness"))?;
                let _ = shutdown.await;
                collector_stopped_for_task.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            move |received_cursor_key, shutdown| async move {
                let received_cursor_key = received_cursor_key.ok_or_else(|| {
                    std::io::Error::other("SIGTERM child server started without collector cursor")
                })?;
                if supervisor_cursor_proof(&received_cursor_key) != cursor_proof {
                    return Err(std::io::Error::other(
                        "SIGTERM child server did not receive collector cursor",
                    ));
                }
                let listener = tokio::net::TcpListener::bind(bind).await?;
                fs::write(&ready_receipt_path, ready.as_bytes())?;
                let mut shutdown = shutdown;
                loop {
                    tokio::select! {
                        _ = &mut shutdown => break,
                        accepted = listener.accept() => {
                            let _ = accepted?;
                        }
                    }
                }
                drop(listener);
                server_stopped_for_task.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            async move {
                let _ = sigterm.recv().await;
                MacServeControl::Shutdown
            },
        )
        .await
        .expect("SIGTERM child supervisor shutdown");

        assert_eq!(collector_stopped.load(Ordering::SeqCst), 1);
        assert_eq!(server_stopped.load(Ordering::SeqCst), 1);
        fs::write(receipt_path, stopped.as_bytes()).expect("write SIGTERM child stopped receipt");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_sigterm_releases_listener_and_reruns_same_fixture() {
        let temporary = tempfile::tempdir().expect("SIGTERM lifecycle temporary root");
        let fixture_path = temporary.path().join("stable-installation-heads");
        let fixture = format!(
            "installation_id={}\nhead_id={}\n",
            Uuid::new_v4(),
            Uuid::new_v4()
        );
        fs::write(&fixture_path, fixture.as_bytes()).expect("write stable fixture heads");
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve loopback");
        let bind = reservation.local_addr().expect("reserved loopback address");
        drop(reservation);
        let test_binary = std::env::current_exe().expect("current test executable");

        run_macos_supervisor_sigterm_child_cycle(&test_binary, &fixture_path, bind, 1)
            .await
            .expect("first SIGTERM lifecycle child");
        let rebound = TcpListener::bind(bind).expect("listener rebinds after first SIGTERM");
        drop(rebound);

        run_macos_supervisor_sigterm_child_cycle(&test_binary, &fixture_path, bind, 2)
            .await
            .expect("second SIGTERM lifecycle child");
        let rebound = TcpListener::bind(bind).expect("listener rebinds after second SIGTERM");
        drop(rebound);
        assert_eq!(
            fs::read(&fixture_path).expect("read stable fixture after rerun"),
            fixture.as_bytes(),
            "SIGTERM lifecycle changed stable installation/head fixture"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_zero_cadence_never_constructs_collector() {
        let collector_calls = Arc::new(AtomicUsize::new(0));
        let collector_calls_for_factory = Arc::clone(&collector_calls);
        let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
        let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
        let (control_tx, control_rx) = tokio::sync::oneshot::channel();

        let supervisor = tokio::spawn(run_macos_serve_supervisor(
            false,
            move |_ready, _shutdown| {
                collector_calls_for_factory.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
            move |cursor_key, shutdown| async move {
                assert!(
                    cursor_key.is_none(),
                    "collector-disabled Serve received a cursor key"
                );
                let _ = server_started_tx.send(());
                let _ = shutdown.await;
                let _ = server_stopped_tx.send(());
                Ok(())
            },
            async move { control_rx.await.expect("test control") },
        ));

        server_started_rx.await.expect("server started");
        assert_eq!(collector_calls.load(Ordering::SeqCst), 0);
        assert!(control_tx.send(MacServeControl::Shutdown).is_ok());
        supervisor
            .await
            .expect("supervisor task")
            .expect("clean API-only shutdown");
        server_stopped_rx
            .await
            .expect("server stopped before return");
        assert_eq!(collector_calls.load(Ordering::SeqCst), 0);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_waits_for_ready_cursor_before_constructing_server() {
        let expected_key = CursorKey::from_bytes([61; 32]);
        let expected_proof = supervisor_cursor_proof(&expected_key);
        let (collector_started_tx, collector_started_rx) = tokio::sync::oneshot::channel();
        let (allow_ready_tx, allow_ready_rx) = tokio::sync::oneshot::channel();
        let (collector_stopped_tx, collector_stopped_rx) = tokio::sync::oneshot::channel();
        let (server_started_tx, mut server_started_rx) = tokio::sync::oneshot::channel();
        let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
        let (control_tx, control_rx) = tokio::sync::oneshot::channel();

        let supervisor = tokio::spawn(run_macos_serve_supervisor(
            true,
            move |ready, shutdown| async move {
                let _ = collector_started_tx.send(());
                let _ = allow_ready_rx.await;
                ready
                    .send(expected_key)
                    .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
                let _ = shutdown.await;
                let _ = collector_stopped_tx.send(());
                Ok(())
            },
            move |cursor_key, shutdown| async move {
                let cursor_key = cursor_key.expect("collector cursor key");
                let _ = server_started_tx.send(supervisor_cursor_proof(&cursor_key));
                let _ = shutdown.await;
                let _ = server_stopped_tx.send(());
                Ok(())
            },
            async move { control_rx.await.expect("test control") },
        ));

        collector_started_rx.await.expect("collector started");
        assert!(
            server_started_rx.try_recv().is_err(),
            "server started before collector readiness"
        );
        assert!(allow_ready_tx.send(()).is_ok());
        assert_eq!(
            server_started_rx
                .await
                .expect("server started after readiness"),
            expected_proof,
            "server did not receive the collector's exact cursor key"
        );

        assert!(control_tx.send(MacServeControl::Shutdown).is_ok());
        supervisor
            .await
            .expect("supervisor task")
            .expect("clean shutdown");
        collector_stopped_rx
            .await
            .expect("collector stopped before return");
        server_stopped_rx
            .await
            .expect("server stopped before return");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_server_error_stops_and_awaits_collector() {
        let (collector_stopped_tx, collector_stopped_rx) = tokio::sync::oneshot::channel();
        let (server_finished_tx, server_finished_rx) = tokio::sync::oneshot::channel();
        let result = run_macos_serve_supervisor(
            true,
            move |ready, shutdown| async move {
                ready
                    .send(CursorKey::from_bytes([62; 32]))
                    .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
                let _ = shutdown.await;
                let _ = collector_stopped_tx.send(());
                Ok(())
            },
            move |_cursor_key, _shutdown| async move {
                let _ = server_finished_tx.send(());
                Err(std::io::Error::other("test server failure"))
            },
            pending(),
        )
        .await
        .expect_err("server failure returns");

        assert!(result.to_string().contains("test server failure"));
        server_finished_rx.await.expect("server completed");
        collector_stopped_rx
            .await
            .expect("collector stopped before server error returned");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_collector_error_stops_and_awaits_server() {
        let (release_collector_tx, release_collector_rx) = tokio::sync::oneshot::channel();
        let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
        let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
        let supervisor = tokio::spawn(run_macos_serve_supervisor(
            true,
            move |ready, _shutdown| async move {
                ready
                    .send(CursorKey::from_bytes([63; 32]))
                    .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
                let _ = release_collector_rx.await;
                Err(std::io::Error::other("test collector failure"))
            },
            move |_cursor_key, shutdown| async move {
                let _ = server_started_tx.send(());
                let _ = shutdown.await;
                let _ = server_stopped_tx.send(());
                Ok(())
            },
            pending(),
        ));

        server_started_rx
            .await
            .expect("server started after readiness");
        assert!(release_collector_tx.send(()).is_ok());
        let result = supervisor
            .await
            .expect("supervisor task")
            .expect_err("collector failure returns");
        assert!(result.to_string().contains("test collector failure"));
        server_stopped_rx
            .await
            .expect("server stopped before collector error returned");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_admission_invalidation_stops_and_awaits_workers() {
        let (collector_stopped_tx, collector_stopped_rx) = tokio::sync::oneshot::channel();
        let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
        let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
        let (control_tx, control_rx) = tokio::sync::oneshot::channel();
        let supervisor = tokio::spawn(run_macos_serve_supervisor(
            true,
            move |ready, shutdown| async move {
                ready
                    .send(CursorKey::from_bytes([64; 32]))
                    .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
                let _ = shutdown.await;
                let _ = collector_stopped_tx.send(());
                Ok(())
            },
            move |_cursor_key, shutdown| async move {
                let _ = server_started_tx.send(());
                let _ = shutdown.await;
                let _ = server_stopped_tx.send(());
                Ok(())
            },
            async move { control_rx.await.expect("test control") },
        ));

        server_started_rx.await.expect("server started");
        assert!(
            control_tx
                .send(MacServeControl::AdmissionInvalidated(
                    std::io::Error::other("test admission invalidated",)
                ))
                .is_ok()
        );
        let result = supervisor
            .await
            .expect("supervisor task")
            .expect_err("admission invalidation returns");
        assert!(result.to_string().contains("test admission invalidated"));
        collector_stopped_rx
            .await
            .expect("collector stopped before invalidation returned");
        server_stopped_rx
            .await
            .expect("server stopped before invalidation returned");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_shutdown_stops_and_awaits_workers() {
        let (collector_stopped_tx, collector_stopped_rx) = tokio::sync::oneshot::channel();
        let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
        let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
        let (control_tx, control_rx) = tokio::sync::oneshot::channel();
        let supervisor = tokio::spawn(run_macos_serve_supervisor(
            true,
            move |ready, shutdown| async move {
                ready
                    .send(CursorKey::from_bytes([65; 32]))
                    .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
                let _ = shutdown.await;
                let _ = collector_stopped_tx.send(());
                Ok(())
            },
            move |_cursor_key, shutdown| async move {
                let _ = server_started_tx.send(());
                let _ = shutdown.await;
                let _ = server_stopped_tx.send(());
                Ok(())
            },
            async move { control_rx.await.expect("test control") },
        ));

        server_started_rx.await.expect("server started");
        assert!(control_tx.send(MacServeControl::Shutdown).is_ok());
        supervisor
            .await
            .expect("supervisor task")
            .expect("shutdown returns after both workers");
        collector_stopped_rx
            .await
            .expect("collector stopped before shutdown returned");
        server_stopped_rx
            .await
            .expect("server stopped before shutdown returned");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_shutdown_aborts_uncooperative_worker_after_stop_bound() {
        let (drops_tx, mut drops_rx) = tokio::sync::mpsc::unbounded_channel();
        let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
        let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
        let (control_tx, control_rx) = tokio::sync::oneshot::channel();
        let supervisor = tokio::spawn(run_macos_serve_supervisor(
            true,
            move |ready, _shutdown| async move {
                let _drop_witness = MacServeDropWitness {
                    label: "collector",
                    drops: drops_tx,
                };
                ready
                    .send(CursorKey::from_bytes([67; 32]))
                    .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
                pending::<()>().await;
                Ok(())
            },
            move |_cursor_key, shutdown| async move {
                let _ = server_started_tx.send(());
                let _ = shutdown.await;
                let _ = server_stopped_tx.send(());
                Ok(())
            },
            async move { control_rx.await.expect("test control") },
        ));

        server_started_rx.await.expect("server started");
        assert!(control_tx.send(MacServeControl::Shutdown).is_ok());
        let error = supervisor
            .await
            .expect("supervisor task")
            .expect_err("uncooperative collector times out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            error
                .get_ref()
                .is_some_and(|source| source.is::<MacServeWorkerStopTimeout>()),
            "stop timeout lost its typed source"
        );
        assert!(error.to_string().contains("collector worker did not stop"));
        server_stopped_rx
            .await
            .expect("cooperative server stopped before timeout returned");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), drops_rx.recv())
                .await
                .expect("collector abort timeout"),
            Some("collector"),
            "uncooperative collector was not aborted and dropped"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_supervisor_cancellation_aborts_owned_workers_without_detaching() {
        let (drops_tx, mut drops_rx) = tokio::sync::mpsc::unbounded_channel();
        let collector_drops = drops_tx.clone();
        let server_drops = drops_tx.clone();
        let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
        let supervisor = tokio::spawn(run_macos_serve_supervisor(
            true,
            move |ready, _shutdown| async move {
                let _drop_witness = MacServeDropWitness {
                    label: "collector",
                    drops: collector_drops,
                };
                ready
                    .send(CursorKey::from_bytes([66; 32]))
                    .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
                pending::<()>().await;
                Ok(())
            },
            move |_cursor_key, _shutdown| async move {
                let _drop_witness = MacServeDropWitness {
                    label: "server",
                    drops: server_drops,
                };
                let _ = server_started_tx.send(());
                pending::<()>().await;
                Ok(())
            },
            pending(),
        ));

        server_started_rx.await.expect("server started");
        supervisor.abort();
        assert!(
            supervisor
                .await
                .expect_err("supervisor cancelled")
                .is_cancelled(),
            "cancellation did not terminate the supervisor"
        );

        let mut dropped = vec![
            tokio::time::timeout(Duration::from_secs(1), drops_rx.recv())
                .await
                .expect("collector/server drop timeout")
                .expect("drop witness"),
            tokio::time::timeout(Duration::from_secs(1), drops_rx.recv())
                .await
                .expect("collector/server drop timeout")
                .expect("drop witness"),
        ];
        dropped.sort_unstable();
        assert_eq!(dropped, ["collector", "server"]);
    }

    #[tokio::test]
    async fn doctor_does_not_create_or_initialize_a_missing_data_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let data_dir = temporary.path().join("missing-hub-state");
        let config_path = temporary.path().join("config.toml");
        fs::write(
            &config_path,
            format!("data_dir = {:?}\nbind = '127.0.0.1:18443'\n", data_dir),
        )
        .expect("write config");
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
            .expect("make test config private");

        let error = run(Cli {
            config: Some(config_path),
            command: Command::Doctor,
        })
        .await
        .expect_err("doctor must fail on missing state");

        assert!(error.to_string().contains("cannot inspect hub catalogue"));
        assert!(!data_dir.exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn doctor_reports_inventory_and_does_not_delete_tokens() {
        use teslatlas_hub::{
            credentials::OwnerTokens, db::TeslaMateLegacyTokenStore, fleet_api::FleetRegion,
            fleet_credentials::FleetSetupCredentials, teslamate_credentials::random_encryption_key,
            teslamate_token::encrypt_legacy_owner_tokens,
        };

        let temporary = tempfile::tempdir().expect("temporary Hub");
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
            .expect("private data directory");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let fleet = FleetSetupCredentials::new(
            "fleet-access".to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            3_600,
        )
        .expect("Fleet credentials");
        persist_fleet_setup_and_drop_legacy(
            temporary.path(),
            &store,
            &fleet,
            std::time::SystemTime::now(),
        )
        .expect("seed Fleet");
        let legacy = OwnerTokens::from_file_bytes(
            zeroize::Zeroizing::new(b"doctor-access".to_vec()),
            zeroize::Zeroizing::new(b"doctor-refresh".to_vec()),
        )
        .expect("legacy credentials");
        let encryption_key = random_encryption_key();
        let (access, refresh) =
            encrypt_legacy_owner_tokens(&encryption_key, &legacy).expect("encrypt");
        let stored = TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
        persist_migrated_legacy_tokens(temporary.path(), &store, &encryption_key, &stored)
            .expect("copy TeslaMate tokens without deleting Fleet");

        let config_path = temporary.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                "data_dir = {:?}\nbind = '127.0.0.1:18443'\n\n[collector]\ninterval_seconds = 0\n",
                temporary.path()
            ),
        )
        .expect("write config");
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
            .expect("private config");

        run(Cli {
            config: Some(config_path),
            command: Command::Doctor,
        })
        .await
        .expect("doctor succeeds on a healthy catalogue");

        assert!(
            store
                .load_teslamate_legacy_tokens()
                .expect("legacy after doctor")
                .is_some(),
            "doctor must not delete Owner tokens"
        );
        assert!(
            store
                .load_fleet_tokens()
                .expect("Fleet after doctor")
                .is_some(),
            "doctor must not delete Fleet tokens"
        );
        let inventory = store.catalogue_inventory().expect("inventory");
        assert_eq!(inventory.journal_mode, "wal");
        assert_eq!(inventory.teslamate_legacy_token_rows, 1);
        assert_eq!(inventory.fleet_token_rows, 1);
    }

    #[test]
    fn immutable_diagnostic_waits_for_a_transient_wal() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
            .expect("protect temporary directory");
        HubStore::initialize(temporary.path()).expect("store initializes");
        let wal = temporary.path().join("hub.sqlite-wal");
        fs::write(&wal, b"pending").expect("create pending WAL witness");
        let mut waits = 0;

        run_immutable_diagnostic_with(
            temporary.path(),
            |store| {
                store.catalogue_check()?;
                Ok(())
            },
            || {
                waits += 1;
                if wal.exists() {
                    fs::remove_file(&wal).expect("settle WAL");
                }
            },
        )
        .expect("diagnostic opens after WAL settles");

        assert_eq!(waits, 1);
    }

    #[test]
    fn immutable_diagnostic_retries_a_snapshot_changed_during_checks() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
            .expect("protect temporary directory");
        HubStore::initialize(temporary.path()).expect("store initializes");
        let database = temporary.path().join("hub.sqlite");
        let original_bytes = fs::metadata(&database).expect("catalogue metadata").len();
        let mut runs = 0;
        let mut waits = 0;

        run_immutable_diagnostic_with(
            temporary.path(),
            |store| {
                runs += 1;
                store.catalogue_check()?;
                if runs == 1 {
                    let mut catalogue = fs::OpenOptions::new().append(true).open(&database)?;
                    catalogue.write_all(b"changed")?;
                    catalogue.sync_all()?;
                }
                Ok(())
            },
            || {
                waits += 1;
                fs::OpenOptions::new()
                    .write(true)
                    .open(&database)
                    .expect("open changed catalogue")
                    .set_len(original_bytes)
                    .expect("restore catalogue size");
            },
        )
        .expect("diagnostic retries a changed snapshot");

        assert_eq!(runs, 2);
        assert_eq!(waits, 1);
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[tokio::test]
    async fn serve_fails_before_initialising_state_on_an_unsupported_platform() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let data_dir = temporary.path().join("missing-hub-state");
        let config_path = temporary.path().join("config.toml");
        fs::write(
            &config_path,
            format!("data_dir = {:?}\nbind = '127.0.0.1:18443'\n", data_dir),
        )
        .expect("write config");

        let error = run(Cli {
            config: Some(config_path),
            command: Command::Serve,
        })
        .await
        .expect_err("unsupported Serve must fail explicitly");

        assert!(error.to_string().contains("not yet supported"));
        assert!(!data_dir.exists());
    }

    #[test]
    fn observation_commands_parse_their_machine_readable_inputs() {
        let watermark =
            Cli::try_parse_from(["teslatlas-hub", "observation-watermark", "--car-id", "17"])
                .expect("watermark CLI");
        assert!(matches!(
            watermark.command,
            Command::ObservationWatermark { car_id: 17 }
        ));

        let verify = Cli::try_parse_from([
            "teslatlas-hub",
            "verify-observation",
            "--car-id",
            "17",
            "--watermark",
            "42",
        ])
        .expect("verification CLI");
        assert!(matches!(
            verify.command,
            Command::VerifyObservation {
                car_id: 17,
                watermark: 42
            }
        ));
    }

    #[test]
    fn native_control_commands_parse_bounded_values() {
        let settings = Cli::try_parse_from([
            "teslatlas-hub",
            "control",
            "settings",
            "--enabled",
            "false",
            "--suspend-min",
            "12",
        ])
        .expect("settings CLI");
        assert!(matches!(
            settings.command,
            Command::Control {
                command: ControlCommand::Settings {
                    enabled: Some(false),
                    suspend_min: Some(12),
                    ..
                },
                ..
            }
        ));

        assert!(
            Cli::try_parse_from(["teslatlas-hub", "control", "export-gpx", "--drive-id", "0",])
                .is_err()
        );

        let sign_out =
            Cli::try_parse_from(["teslatlas-hub", "control", "sign-out"]).expect("sign-out CLI");
        assert!(matches!(
            sign_out.command,
            Command::Control {
                command: ControlCommand::SignOut,
                ..
            }
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn observe_command_requires_positive_duration() {
        let cli = Cli::try_parse_from(["teslatlas-hub", "observe", "--duration-seconds", "3600"])
            .expect("observe CLI");
        assert!(matches!(
            cli.command,
            Command::Observe {
                duration_seconds: 3600
            }
        ));
        assert!(
            Cli::try_parse_from(["teslatlas-hub", "observe", "--duration-seconds", "0",]).is_err()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn setup_command_accepts_private_token_files_and_positive_optional_vehicle() {
        let cli = Cli::try_parse_from([
            "teslatlas-hub",
            "setup",
            "--access-token-file",
            "access",
            "--refresh-token-file",
            "refresh",
            "--vehicle-id",
            "70",
        ])
        .expect("setup CLI");
        assert!(matches!(
            cli.command,
            Command::Setup {
                access_token_file: Some(access_token_file),
                refresh_token_file: Some(refresh_token_file),
                tokens_stdin: false,
                vehicle_id: Some(70),
                all_vehicles: false,
            } if access_token_file.as_path() == Path::new("access")
                && refresh_token_file.as_path() == Path::new("refresh")
        ));
        assert!(
            Cli::try_parse_from([
                "teslatlas-hub",
                "setup",
                "--access-token-file",
                "access",
                "--refresh-token-file",
                "refresh",
                "--vehicle-id",
                "0",
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["teslatlas-hub", "setup"]).is_err());
        assert!(
            Cli::try_parse_from(["teslatlas-hub", "setup", "--access-token-file", "access",])
                .is_err()
        );
        let stdin = Cli::try_parse_from(["teslatlas-hub", "setup", "--tokens-stdin"])
            .expect("stdin setup CLI");
        assert!(matches!(
            stdin.command,
            Command::Setup {
                access_token_file: None,
                refresh_token_file: None,
                tokens_stdin: true,
                vehicle_id: None,
                all_vehicles: false,
            }
        ));
        assert!(
            Cli::try_parse_from([
                "teslatlas-hub",
                "setup",
                "--tokens-stdin",
                "--all-vehicles",
                "--vehicle-id",
                "70",
            ])
            .is_err()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fleet_setup_is_stdin_only_and_decodes_bounded_fields() {
        let selected = Cli::try_parse_from(["teslatlas-hub", "setup-fleet", "--vehicle-id", "70"])
            .expect("Fleet setup CLI");
        assert!(matches!(
            selected.command,
            Command::SetupFleet {
                vehicle_id: Some(70),
                all_vehicles: false,
            }
        ));
        let all = Cli::try_parse_from(["teslatlas-hub", "setup-fleet", "--all-vehicles"])
            .expect("all Fleet vehicles CLI");
        assert!(matches!(
            all.command,
            Command::SetupFleet {
                vehicle_id: None,
                all_vehicles: true,
            }
        ));
        assert!(
            Cli::try_parse_from([
                "teslatlas-hub",
                "setup-fleet",
                "--all-vehicles",
                "--vehicle-id",
                "70",
            ])
            .is_err()
        );
        assert!(
            decode_setup_fleet_stdin(
                br#"{"accessToken":"e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iLCJ2ZWhpY2xlX2NtZHMiLCJ2ZWhpY2xlX2NoYXJnaW5nX2NtZHMiXX0.sig","refreshToken":"refresh","clientId":"client","region":"europe_middle_east_and_africa","expiresInSeconds":3600}"#,
            )
            .is_ok()
        );
        assert!(
            decode_setup_fleet_stdin(
                br#"{"accessToken":"e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig","refreshToken":"refresh","clientId":"client","region":"europe_middle_east_and_africa","expiresInSeconds":3600}"#,
            )
            .is_err()
        );
        assert!(
            decode_setup_fleet_stdin(
                br#"{"accessToken":"access","refreshToken":"refresh","clientId":"client","region":"eu","expiresInSeconds":3600}"#,
            )
            .is_err()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fleet_rejects_enabling_legacy_streaming() {
        use teslatlas_hub::config::CollectorProvider;

        assert!(validate_streaming_setting(CollectorProvider::Fleet, Some(true)).is_err());
        assert!(validate_streaming_setting(CollectorProvider::Fleet, Some(false)).is_ok());
        assert!(validate_streaming_setting(CollectorProvider::Legacy, Some(true)).is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn legacy_setup_requires_legacy_provider_before_mutation() {
        use teslatlas_hub::config::CollectorProvider;

        assert!(validate_legacy_setup_provider(CollectorProvider::Legacy).is_ok());
        assert!(validate_legacy_setup_provider(CollectorProvider::Fleet).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sign_out_clears_both_providers_but_preserves_cursor_key() {
        use teslatlas_hub::{
            credentials::OwnerTokens,
            db::TeslaMateLegacyTokenStore,
            fleet_api::FleetRegion,
            fleet_credentials::{FleetSetupCredentials, persist_fleet_setup_credentials},
            teslamate_credentials::{
                load_or_create_cursor_key, random_encryption_key, replace_key_and_tokens,
            },
            teslamate_token::encrypt_legacy_owner_tokens,
        };

        let temporary = tempfile::tempdir().expect("temporary Hub");
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
            .expect("private temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let cursor_before = load_or_create_cursor_key(temporary.path()).expect("cursor key");
        let cursor_proof = supervisor_cursor_proof(&cursor_before);
        let legacy = OwnerTokens::from_file_bytes(
            zeroize::Zeroizing::new(b"access".to_vec()),
            zeroize::Zeroizing::new(b"refresh".to_vec()),
        )
        .expect("legacy credentials");
        let legacy_key = random_encryption_key();
        let (access, refresh) =
            encrypt_legacy_owner_tokens(&legacy_key, &legacy).expect("encrypt legacy");
        let legacy_store =
            TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
        replace_key_and_tokens(temporary.path(), &store, &legacy_key, &legacy_store)
            .expect("persist legacy");
        let fleet = FleetSetupCredentials::new(
            "fleet-access".to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            3_600,
        )
        .expect("Fleet credentials");
        persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &fleet,
            std::time::SystemTime::now(),
        )
        .expect("persist Fleet");
        assert!(teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());

        clear_provider_credentials(temporary.path(), &store).expect("sign out");

        assert!(
            store
                .load_teslamate_legacy_tokens()
                .expect("legacy row")
                .is_none()
        );
        assert!(store.load_fleet_tokens().expect("Fleet row").is_none());
        assert!(!teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());
        let cursor_after = load_or_create_cursor_key(temporary.path()).expect("cursor remains");
        assert_eq!(supervisor_cursor_proof(&cursor_after), cursor_proof);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sign_out_attempts_legacy_removal_after_fleet_key_failure() {
        use teslatlas_hub::{
            credentials::OwnerTokens,
            db::TeslaMateLegacyTokenStore,
            teslamate_credentials::{random_encryption_key, replace_key_and_tokens},
            teslamate_token::encrypt_legacy_owner_tokens,
        };

        let temporary = tempfile::tempdir().expect("temporary Hub");
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
            .expect("private temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let legacy = OwnerTokens::from_file_bytes(
            zeroize::Zeroizing::new(b"access".to_vec()),
            zeroize::Zeroizing::new(b"refresh".to_vec()),
        )
        .expect("legacy credentials");
        let legacy_key = random_encryption_key();
        let (access, refresh) =
            encrypt_legacy_owner_tokens(&legacy_key, &legacy).expect("encrypt legacy");
        let legacy_store =
            TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
        replace_key_and_tokens(temporary.path(), &store, &legacy_key, &legacy_store)
            .expect("persist legacy");

        let invalid_fleet_key = teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path());
        fs::create_dir(&invalid_fleet_key).expect("invalid Fleet key directory");
        fs::set_permissions(&invalid_fleet_key, fs::Permissions::from_mode(0o700))
            .expect("private invalid Fleet key");

        let error = clear_provider_credentials(temporary.path(), &store)
            .expect_err("Fleet key failure must be reported");
        assert!(error.to_string().contains("Fleet credentials"));
        assert!(
            store
                .load_teslamate_legacy_tokens()
                .expect("legacy row")
                .is_none(),
            "Legacy credentials must still be removed"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn setup_clears_the_other_provider_token_generation() {
        use teslatlas_hub::{
            credentials::OwnerTokens,
            db::TeslaMateLegacyTokenStore,
            fleet_api::FleetRegion,
            fleet_credentials::FleetSetupCredentials,
            teslamate_credentials::{random_encryption_key, replace_key_and_tokens},
            teslamate_token::encrypt_legacy_owner_tokens,
        };

        let temporary = tempfile::tempdir().expect("temporary Hub");
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
            .expect("private temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let legacy = OwnerTokens::from_file_bytes(
            zeroize::Zeroizing::new(b"access".to_vec()),
            zeroize::Zeroizing::new(b"refresh".to_vec()),
        )
        .expect("legacy credentials");
        let legacy_key = random_encryption_key();
        let (access, refresh) =
            encrypt_legacy_owner_tokens(&legacy_key, &legacy).expect("encrypt legacy");
        let legacy_store =
            TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
        replace_key_and_tokens(temporary.path(), &store, &legacy_key, &legacy_store)
            .expect("persist legacy");
        let fleet = FleetSetupCredentials::new(
            "fleet-access".to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            3_600,
        )
        .expect("Fleet credentials");
        persist_fleet_setup_and_drop_legacy(
            temporary.path(),
            &store,
            &fleet,
            std::time::SystemTime::now(),
        )
        .expect("setup-fleet drops legacy");
        assert!(
            store
                .load_teslamate_legacy_tokens()
                .expect("legacy row")
                .is_none()
        );
        assert!(store.load_fleet_tokens().expect("Fleet row").is_some());
        assert!(teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());

        persist_legacy_setup_and_drop_fleet(temporary.path(), &store, &legacy)
            .expect("setup drops Fleet");
        assert!(
            store
                .load_teslamate_legacy_tokens()
                .expect("legacy row")
                .is_some()
        );
        assert!(store.load_fleet_tokens().expect("Fleet row").is_none());
        assert!(!teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migrate_copies_legacy_tokens_without_deleting_fleet() {
        use teslatlas_hub::{
            credentials::OwnerTokens, db::TeslaMateLegacyTokenStore, fleet_api::FleetRegion,
            fleet_credentials::FleetSetupCredentials, teslamate_credentials::random_encryption_key,
            teslamate_token::encrypt_legacy_owner_tokens,
        };

        let temporary = tempfile::tempdir().expect("temporary Hub");
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
            .expect("private temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let fleet = FleetSetupCredentials::new(
            "fleet-access".to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            FleetRegion::EuropeMiddleEastAndAfrica,
            3_600,
        )
        .expect("Fleet credentials");
        persist_fleet_setup_and_drop_legacy(
            temporary.path(),
            &store,
            &fleet,
            std::time::SystemTime::now(),
        )
        .expect("seed Fleet");
        assert!(store.load_fleet_tokens().expect("Fleet row").is_some());

        let legacy = OwnerTokens::from_file_bytes(
            zeroize::Zeroizing::new(b"migrate-access".to_vec()),
            zeroize::Zeroizing::new(b"migrate-refresh".to_vec()),
        )
        .expect("legacy credentials");
        let encryption_key = random_encryption_key();
        let (access, refresh) =
            encrypt_legacy_owner_tokens(&encryption_key, &legacy).expect("encrypt legacy");
        let stored = TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
        persist_migrated_legacy_tokens(temporary.path(), &store, &encryption_key, &stored)
            .expect("migrate copies TeslaMate tokens");
        assert!(
            store
                .load_teslamate_legacy_tokens()
                .expect("legacy row")
                .is_some()
        );
        assert!(
            store.load_fleet_tokens().expect("Fleet row").is_some(),
            "TeslaMate import must not delete Fleet credentials"
        );
        assert!(teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn service_commands_parse_without_live_store_admission() {
        for (name, expected) in [
            ("status", "status"),
            ("start", "start"),
            ("stop", "stop"),
            ("restart", "restart"),
        ] {
            let cli = Cli::try_parse_from(["teslatlas-hub", "service", name]).expect("service CLI");
            let Command::Service { command } = cli.command else {
                panic!("service command")
            };
            let actual = match command {
                ServiceCommand::Status => "status",
                ServiceCommand::Start => "start",
                ServiceCommand::Stop => "stop",
                ServiceCommand::Restart => "restart",
            };
            assert_eq!(actual, expected);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn long_lived_and_sensitive_commands_require_the_instance_lock() {
        assert!(command_requires_user_hub_admission(&Command::Init));
        assert!(command_requires_user_hub_admission(&Command::Setup {
            access_token_file: Some(PathBuf::from("access")),
            refresh_token_file: Some(PathBuf::from("refresh")),
            tokens_stdin: false,
            vehicle_id: None,
            all_vehicles: false,
        }));
        assert!(command_requires_user_hub_admission(&Command::SetupFleet {
            vehicle_id: None,
            all_vehicles: true,
        }));
        assert!(command_requires_user_hub_admission(&Command::Serve));
        assert!(command_requires_user_hub_admission(&Command::Observe {
            duration_seconds: 1,
        }));
        assert!(command_requires_user_hub_admission(&Command::Migrate {
            source: "postgresql://localhost/teslamate".to_owned(),
            car_id: 1,
            postgres_password_file: PathBuf::from("password"),
            encryption_key_file: Some(PathBuf::from("key")),
            access_token_file: None,
            refresh_token_file: None,
            online_snapshot: false,
        }));
        assert!(command_requires_user_hub_admission(&Command::Pair {
            label: "test phone".to_owned(),
            expires_in_seconds: 900,
            json: false,
        }));
        assert!(command_requires_user_hub_admission(&Command::Repair));
        assert!(command_requires_user_hub_admission(&Command::Backup {
            destination: PathBuf::from("backup"),
        }));
        assert!(command_requires_user_hub_admission(
            &Command::ExportRecoveryCredentials {
                destination: PathBuf::from("credentials.tthcr"),
                recovery_key_file: PathBuf::from("recovery.key"),
            }
        ));
        assert!(command_requires_user_hub_admission(
            &Command::RestoreRecoveryCredentials {
                source: PathBuf::from("credentials.tthcr"),
                recovery_key_file: PathBuf::from("recovery.key"),
            }
        ));
        assert!(!command_requires_user_hub_admission(&Command::Doctor));
        assert!(!command_requires_user_hub_admission(&Command::Legal));
        assert!(!command_requires_user_hub_admission(&Command::Status));
        assert!(!command_requires_user_hub_admission(&Command::Preflight));
        assert!(!command_requires_user_hub_admission(&Command::Service {
            command: ServiceCommand::Status,
        }));
        assert!(!command_requires_user_hub_admission(&Command::Control {
            vehicle_id: None,
            command: ControlCommand::Pause,
        }));
    }

    fn test_identity(name: &str) -> (String, zeroize::Zeroizing<String>, Vec<u8>) {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec![name.to_owned()]).expect("test TLS identity");
        (
            cert.pem(),
            zeroize::Zeroizing::new(signing_key.serialize_pem()),
            cert.der().to_vec(),
        )
    }

    fn write_private_test_file(path: &Path, bytes: impl AsRef<[u8]>) {
        fs::write(path, bytes).expect("write private test file");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("protect private test file");
    }

    fn write_test_identity(certificate_path: &Path, private_key_path: &Path) -> Vec<u8> {
        let (certificate_pem, private_key_pem, certificate_der) = test_identity("hub.example");
        write_private_test_file(certificate_path, certificate_pem);
        write_private_test_file(private_key_path, private_key_pem.as_bytes());
        certificate_der
    }

    fn write_test_certificate(certificate_path: &Path) -> Vec<u8> {
        let (certificate_pem, _private_key_pem, certificate_der) = test_identity("hub.example");
        write_private_test_file(certificate_path, certificate_pem);
        certificate_der
    }

    fn pairing_challenge_count(store: &HubStore) -> i64 {
        store
            .open()
            .expect("open Hub store")
            .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
                row.get(0)
            })
            .expect("pairing challenge count")
    }

    struct FlushFailingWriter {
        bytes: zeroize::Zeroizing<Vec<u8>>,
    }

    impl Default for FlushFailingWriter {
        fn default() -> Self {
            Self {
                bytes: zeroize::Zeroizing::new(Vec::new()),
            }
        }
    }

    impl Write for FlushFailingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "test presentation sink failed",
            ))
        }
    }

    struct WriteFailingWriter;

    impl Write for WriteFailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "test presentation sink failed",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct NeverWriter;

    impl Write for NeverWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            panic!("pairing presentation must not be written")
        }

        fn flush(&mut self) -> io::Result<()> {
            panic!("pairing presentation must not be flushed")
        }
    }

    #[tokio::test]
    async fn pairing_certificate_key_and_qr_failures_leave_no_invitation() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("Hub store");
        let certificate = temporary.path().join("leaf.pem");
        let private_key = temporary.path().join("private-key.pem");
        write_test_identity(&certificate, &private_key);
        let missing = temporary.path().join("access-secret-certificate.pem");
        let mut output = zeroize::Zeroizing::new(Vec::new());
        let certificate_error = execute_pairing_at(
            &store,
            PairingCommandInput {
                label: "test phone",
                expires_in_seconds: 900,
                json: false,
                public_url: "https://hub.example/",
                certificate_path: &missing,
                private_key_path: &private_key,
                created_at_ms: 1_000,
            },
            &mut *output,
        )
        .await
        .expect_err("missing certificate");
        assert!(matches!(
            &certificate_error,
            PairingCommandError::Certificate(_)
        ));
        assert!(!format!("{certificate_error:?}").contains("access-secret"));
        assert!(!certificate_error.to_string().contains("access-secret"));
        assert_eq!(pairing_challenge_count(&store), 0);

        let mismatched_certificate = temporary.path().join("access-secret-mismatch-cert.pem");
        let mismatched_key = temporary.path().join("access-secret-mismatch-key.pem");
        let (first_certificate_pem, _first_key_pem, _) = test_identity("hub.example");
        let (_second_certificate_pem, second_key_pem, _) = test_identity("hub.example");
        write_private_test_file(&mismatched_certificate, first_certificate_pem);
        write_private_test_file(&mismatched_key, second_key_pem.as_bytes());
        let mismatch_error = execute_pairing_at(
            &store,
            PairingCommandInput {
                label: "test phone",
                expires_in_seconds: 900,
                json: false,
                public_url: "https://hub.example/",
                certificate_path: &mismatched_certificate,
                private_key_path: &mismatched_key,
                created_at_ms: 1_000,
            },
            &mut NeverWriter,
        )
        .await
        .expect_err("mismatched certificate and key");
        assert!(matches!(
            mismatch_error,
            PairingCommandError::Certificate(_)
        ));
        assert!(!format!("{mismatch_error:?}").contains("access-secret"));
        assert!(!mismatch_error.to_string().contains("access-secret"));
        assert_eq!(pairing_challenge_count(&store), 0);

        let oversized_endpoint = format!("https://hub.example/{}", "x".repeat(16 * 1024));
        assert!(matches!(
            execute_pairing_at(
                &store,
                PairingCommandInput {
                    label: "test phone",
                    expires_in_seconds: 900,
                    json: false,
                    public_url: &oversized_endpoint,
                    certificate_path: &certificate,
                    private_key_path: &private_key,
                    created_at_ms: 1_000,
                },
                &mut *output,
            )
            .await,
            Err(PairingCommandError::Presentation)
        ));
        assert_eq!(pairing_challenge_count(&store), 0);
    }

    #[tokio::test]
    async fn pairing_flush_failure_revokes_and_success_persists_once() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("Hub store");
        let certificate = temporary.path().join("leaf.pem");
        let private_key = temporary.path().join("private-key.pem");
        let certificate_der = write_test_identity(&certificate, &private_key);

        let mut write_failure = WriteFailingWriter;
        let error = execute_pairing_at(
            &store,
            PairingCommandInput {
                label: "broken writer",
                expires_in_seconds: 900,
                json: true,
                public_url: "https://hub.example/",
                certificate_path: &certificate,
                private_key_path: &private_key,
                created_at_ms: 500,
            },
            &mut write_failure,
        )
        .await
        .expect_err("write failure");
        assert!(matches!(
            error,
            PairingCommandError::Present {
                kind: io::ErrorKind::BrokenPipe
            }
        ));
        assert_eq!(pairing_challenge_count(&store), 0);

        let mut broken = FlushFailingWriter::default();
        let error = execute_pairing_at(
            &store,
            PairingCommandInput {
                label: "broken terminal",
                expires_in_seconds: 900,
                json: true,
                public_url: "https://hub.example/",
                certificate_path: &certificate,
                private_key_path: &private_key,
                created_at_ms: 1_000,
            },
            &mut broken,
        )
        .await
        .expect_err("flush failure");
        assert!(matches!(
            error,
            PairingCommandError::Present {
                kind: io::ErrorKind::BrokenPipe
            }
        ));
        assert_eq!(pairing_challenge_count(&store), 0);

        let mut output = zeroize::Zeroizing::new(Vec::new());
        execute_pairing_at(
            &store,
            PairingCommandInput {
                label: "working terminal",
                expires_in_seconds: 900,
                json: true,
                public_url: "https://hub.example/",
                certificate_path: &certificate,
                private_key_path: &private_key,
                created_at_ms: 2_000,
            },
            &mut *output,
        )
        .await
        .expect("pairing succeeds");
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct BorrowedPresentation<'a> {
            #[serde(borrow)]
            secret: &'a str,
            #[serde(borrow)]
            tls_pin: &'a str,
        }
        let document: BorrowedPresentation<'_> =
            serde_json::from_slice(&output).expect("pairing JSON");
        assert!(!document.secret.is_empty());
        assert_eq!(
            document.tls_pin,
            hex::encode(sha2::Sha256::digest(certificate_der))
        );
        assert_eq!(pairing_challenge_count(&store), 1);
    }

    #[test]
    fn pairing_committed_then_error_is_revoked_before_any_output() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("Hub store");
        let invitation = store
            .prepare_pairing("ambiguous terminal", 1_000, 901_000)
            .expect("pairing prepares");
        let marker = zeroize::Zeroizing::new(b"pairing-secret-marker".to_vec());
        let error = persist_and_present_pairing(
            &mut NeverWriter,
            &marker,
            || {
                store.persist_pairing("ambiguous terminal", &invitation)?;
                Err(teslatlas_hub::db::StoreError::PairingRejected)
            },
            || store.revoke_pairing(invitation.pairing_id),
        )
        .expect_err("committed-then-error persistence is ambiguous");
        assert!(matches!(error, PairingCommandError::Persist(_)));
        assert_eq!(pairing_challenge_count(&store), 0);
        let debug = format!("{error:?}");
        let display = error.to_string();
        assert!(!debug.contains("pairing-secret-marker"));
        assert!(!display.contains("pairing-secret-marker"));
    }

    #[test]
    fn pairing_persist_and_revoke_failure_is_typed_and_redacted() {
        let marker = zeroize::Zeroizing::new(b"pairing-secret-marker".to_vec());
        let error = persist_and_present_pairing(
            &mut NeverWriter,
            &marker,
            || Err(teslatlas_hub::db::StoreError::PairingRejected),
            || Err(teslatlas_hub::db::StoreError::PairingRejected),
        )
        .expect_err("persistence and cleanup both fail");
        assert!(matches!(
            error,
            PairingCommandError::PersistAndRevoke { .. }
        ));
        let debug = format!("{error:?}");
        let display = error.to_string();
        assert!(!debug.contains("pairing-secret-marker"));
        assert!(!display.contains("pairing-secret-marker"));
    }

    #[test]
    fn pairing_presentation_and_revoke_failure_is_typed_and_redacted() {
        let marker = zeroize::Zeroizing::new(b"pairing-secret-marker".to_vec());
        let mut writer = FlushFailingWriter::default();
        let error = persist_and_present_pairing(
            &mut writer,
            &marker,
            || Ok(()),
            || Err(teslatlas_hub::db::StoreError::PairingRejected),
        )
        .expect_err("presentation and cleanup fail");
        assert!(matches!(
            error,
            PairingCommandError::PresentAndRevoke {
                kind: io::ErrorKind::BrokenPipe,
                ..
            }
        ));
        let debug = format!("{error:?}");
        let display = error.to_string();
        assert!(!debug.contains("pairing-secret-marker"));
        assert!(!display.contains("pairing-secret-marker"));
    }

    #[test]
    fn leaf_certificate_reader_rejects_symlink_mode_size_and_identity_races() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let target = temporary.path().join("target.pem");
        write_test_certificate(&target);
        let link = temporary.path().join("link.pem");
        symlink(&target, &link).expect("certificate symlink");
        assert!(leaf_certificate_sha256(&link).is_err());

        let unsafe_mode = temporary.path().join("unsafe.pem");
        write_test_certificate(&unsafe_mode);
        fs::set_permissions(&unsafe_mode, fs::Permissions::from_mode(0o622))
            .expect("unsafe certificate mode");
        assert!(leaf_certificate_sha256(&unsafe_mode).is_err());

        let (base, _private_key_pem, _) = test_identity("bounded.example");
        let bounded = temporary.path().join("bounded.pem");
        let mut exact = base.into_bytes();
        exact.resize(MAX_TLS_CERTIFICATE_CHAIN_BYTES, b'\n');
        write_private_test_file(&bounded, &exact);
        leaf_certificate_sha256(&bounded).expect("exact certificate cap accepted");
        exact.push(b'\n');
        write_private_test_file(&bounded, &exact);
        assert!(leaf_certificate_sha256(&bounded).is_err());

        let replaced = temporary.path().join("replaced.pem");
        write_test_certificate(&replaced);
        let old = temporary.path().join("old.pem");
        let replacement_path = replaced.clone();
        assert!(
            leaf_certificate_sha256_after_open(&replaced, || {
                fs::rename(&replacement_path, &old).expect("move opened certificate");
                write_test_certificate(&replacement_path);
            })
            .is_err()
        );

        let mutated = temporary.path().join("mutated.pem");
        let (original_pem, _private_key_pem, _) = test_identity("mutated.example");
        let mut changed_pem = original_pem.as_bytes().to_vec();
        let changed = changed_pem
            .iter_mut()
            .find(|byte| byte.is_ascii_alphanumeric())
            .expect("certificate has mutable text");
        *changed = if *changed == b'A' { b'B' } else { b'A' };
        write_private_test_file(&mutated, original_pem);
        let mutation_path = mutated.clone();
        assert!(
            leaf_certificate_sha256_after_open(&mutated, || {
                std::thread::sleep(Duration::from_millis(2));
                write_private_test_file(&mutation_path, &changed_pem);
            })
            .is_err()
        );

        let (_certificate_pem, private_key_pem, _) = test_identity("key.example");
        let key_target = temporary.path().join("private-key-target.pem");
        write_private_test_file(&key_target, private_key_pem.as_bytes());
        let key_link = temporary.path().join("private-key-link.pem");
        symlink(&key_target, &key_link).expect("private key symlink");
        assert!(read_tls_identity_file(&key_link, MAX_TLS_PRIVATE_KEY_BYTES, true).is_err());
        fs::set_permissions(&key_target, fs::Permissions::from_mode(0o640))
            .expect("unsafe private key mode");
        assert!(read_tls_identity_file(&key_target, MAX_TLS_PRIVATE_KEY_BYTES, true).is_err());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn observe_supervisor_stops_server_after_control_shutdown() {
        run_macos_serve_supervisor(
            false,
            |_ready, _shutdown| async { Ok(()) },
            |_cursor_key, shutdown| async move {
                let _ = shutdown.await;
                Ok(())
            },
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                MacServeControl::Shutdown
            },
        )
        .await
        .expect("observe supervisor shutdown");
    }

    #[test]
    fn pairing_uri_encodes_its_endpoint_as_one_query_value() {
        let pin = "a".repeat(64);
        let uri = pairing_uri(
            "https://hub.example/",
            &pin,
            Uuid::nil(),
            "0123456789abcdef",
        )
        .expect("pairing URI");
        assert!(uri.contains("endpoint=https%3A%2F%2Fhub.example%2F"));
        assert!(uri.contains("pairing_id=00000000-0000-0000-0000-000000000000"));
        assert!(uri.contains(&format!("tls_pin={pin}")));
    }

    #[test]
    fn leaf_certificate_pin_uses_rustls_line_aware_first_certificate() {
        let (first_pem, _first_key_pem, first_der) = test_identity("first.example");
        let (second_pem, _second_key_pem, second_der) = test_identity("second.example");
        let temporary = tempfile::tempdir().expect("temporary PEM root");
        let chain = temporary.path().join("chain.pem");
        write_private_test_file(&chain, format!("{first_pem}\n{second_pem}"));

        assert_eq!(
            leaf_certificate_sha256(&chain).expect("first chain leaf pin"),
            hex::encode(sha2::Sha256::digest(first_der))
        );

        let inline_marker = first_pem.replacen(
            "-----BEGIN CERTIFICATE-----",
            "x-----BEGIN CERTIFICATE-----",
            1,
        );
        write_private_test_file(&chain, format!("{inline_marker}\n{second_pem}"));
        assert_eq!(
            leaf_certificate_sha256(&chain).expect("line-aware leaf pin"),
            hex::encode(sha2::Sha256::digest(second_der))
        );
    }

    #[test]
    fn pairing_qr_renders_without_printing_the_raw_secret() {
        let uri = pairing_uri(
            "https://192.168.1.10:8443",
            &"a".repeat(64),
            Uuid::nil(),
            "0123456789abcdef",
        )
        .expect("pairing URI");
        let qr = render_pairing_qr(&uri).expect("render QR");
        assert!(qr.contains('█') || qr.contains('▀') || qr.contains('▄'));
        assert!(!qr.contains("0123456789abcdef"));
    }
}
