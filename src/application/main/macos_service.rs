// SPDX-License-Identifier: AGPL-3.0-only

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
const MAC_COMMAND_PROXY_RETRY_DELAY: Duration = Duration::from_millis(25);

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
            // Loopback refusal is normally immediate. Yield between attempts
            // instead of burning one CPU while the proxy starts or fails.
            tokio::time::sleep(MAC_COMMAND_PROXY_RETRY_DELAY).await;
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
