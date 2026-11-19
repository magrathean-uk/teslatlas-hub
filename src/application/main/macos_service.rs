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

#[cfg(unix)]
use tesatlas_hub::runtime::shutdown::ShutdownBudget;

// The service stop shares the process's one shutdown budget (issue #38).
// Unit tests use a short budget so a non-cooperative fake worker can prove
// the abort path without a real wait.
#[cfg(all(unix, not(test)))]
fn macos_serve_default_budget() -> std::sync::Arc<ShutdownBudget> {
    ShutdownBudget::process()
}
#[cfg(all(unix, test))]
fn macos_serve_default_budget() -> std::sync::Arc<ShutdownBudget> {
    std::sync::Arc::new(ShutdownBudget::new(
        std::sync::Arc::new(tesatlas_hub::runtime::shutdown::SystemMonotonic::default()),
        Duration::from_millis(60),
        Duration::from_millis(10),
    ))
}

#[cfg(unix)]
#[derive(Debug)]
struct MacServeWorkerStopTimeout {
    label: &'static str,
    budget_ms: u128,
}

#[cfg(unix)]
impl std::fmt::Display for MacServeWorkerStopTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Hub {} worker did not stop within the {} millisecond shutdown budget",
            self.label, self.budget_ms
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

    /// Requests the stop and waits only for what is left of the shared
    /// shutdown budget; the first stop starts it.
    async fn stop_and_wait(&mut self, budget: &ShutdownBudget) -> std::io::Result<()> {
        self.request_stop();
        let left = budget.remaining_for_work();
        match tokio::time::timeout(left, &mut self.task).await {
            Ok(result) => self.join_result(result),
            Err(_) => {
                self.task.abort();
                let _ = (&mut self.task).await;
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    MacServeWorkerStopTimeout {
                        label: self.label,
                        budget_ms: budget.work_budget().as_millis(),
                    },
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

    async fn stop(&mut self, budget: &ShutdownBudget) -> std::io::Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.start_kill()?;
        }
        let wait = budget.remaining_for_work().min(Duration::from_secs(5));
        tokio::time::timeout(wait, self.child.wait())
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
        primary.and(stop_result)
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
    run_macos_serve_supervisor_within(
        macos_serve_default_budget(),
        collector_enabled,
        collector_start,
        server_start,
        control,
    )
    .await
}

/// [`run_macos_serve_supervisor`] on an explicit shutdown budget. Every stop
/// below waits only for what is left of it, and both workers are stopped
/// together, not one budget after the other.
#[cfg(unix)]
async fn run_macos_serve_supervisor_within<C, S, CF, SF, Control>(
    budget: std::sync::Arc<ShutdownBudget>,
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
    let budget = budget.as_ref();
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
                let server_result = server.stop_and_wait(budget).await;
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
            Err(_) => match collector.stop_and_wait(budget).await {
                Ok(()) => return Err(std::io::Error::other("macOS collector exited before readiness")),
                Err(error) => return Err(error),
            },
        },
        result = collector.wait() => match result {
            Ok(()) => return Err(std::io::Error::other("macOS collector exited before readiness")),
            Err(error) => return Err(error),
        },
        control = &mut control => {
            let stop_result = collector.stop_and_wait(budget).await;
            return match control {
                MacServeControl::Shutdown => stop_result,
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
            let collector_stop_result = collector.stop_and_wait(budget).await;
            preserve_active_result_after_stop(result, collector_stop_result)
        }
        MacServeActiveOutcome::Collector(result) => {
            collector.request_stop();
            let server_stop_result = server.stop_and_wait(budget).await;
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
            let (server_result, collector_result) = tokio::join!(
                server.stop_and_wait(budget),
                collector.stop_and_wait(budget)
            );
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
    budget: std::sync::Arc<ShutdownBudget>,
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
        return run_macos_serve_supervisor_within(
            budget,
            collector_enabled,
            collector_start,
            server_start,
            control,
        )
        .await;
    };

    let mut proxy = MacCommandProxy::start(spec).await?;
    if let Err(error) = proxy.wait_ready().await {
        let _ = proxy.stop(&budget).await;
        return Err(error);
    }

    let serve = run_macos_serve_supervisor_within(
        std::sync::Arc::clone(&budget),
        collector_enabled,
        collector_start,
        server_start,
        control,
    );
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
    let stop_result = proxy.stop(&budget).await;
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

/// The service's collector slot for the window supervisor (design 2.8): the
/// collector runs as one task only inside an Open window and holds the
/// collector lease while it runs; the supervisor stops it at the deadline, at
/// a close or when the service stops. Its exit or failure never stops the
/// service, which keeps serving the API. Clones share one slot.
#[cfg(unix)]
#[derive(Clone)]
struct ServiceCollectorSlot {
    inner: std::sync::Arc<std::sync::Mutex<ServiceCollectorSlotInner>>,
}

#[cfg(unix)]
struct ServiceCollectorSlotInner {
    runtime: tokio::runtime::Handle,
    store: HubStore,
    config: HubConfig,
    admission: std::sync::Arc<AdmittedUserHub>,
    recovery: std::sync::Arc<tesatlas_hub::runtime::operator::authority::RecoveryGate>,
    running: Option<ServiceCollectorRun>,
}

#[cfg(unix)]
struct ServiceCollectorRun {
    task: tokio::task::JoinHandle<()>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    /// Kept so the collector's startup handoff succeeds; the listener has its
    /// own cursor key.
    _ready: tokio::sync::oneshot::Receiver<CursorKey>,
}

#[cfg(unix)]
impl ServiceCollectorSlot {
    fn new(
        runtime: tokio::runtime::Handle,
        store: HubStore,
        config: HubConfig,
        admission: std::sync::Arc<AdmittedUserHub>,
        recovery: std::sync::Arc<tesatlas_hub::runtime::operator::authority::RecoveryGate>,
    ) -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(ServiceCollectorSlotInner {
                runtime,
                store,
                config,
                admission,
                recovery,
                running: None,
            })),
        }
    }

    /// Replaces the configuration the next collector start reads: the
    /// fresh-install setup's collection interval (M02-T21, DEC-116). A
    /// collector already running keeps the configuration it started with.
    fn reload(&self, config: HubConfig) {
        self.lock().config = config;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ServiceCollectorSlotInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(unix)]
impl ServiceCollectorSlotInner {
    fn running(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|run| !run.task.is_finished())
    }
}

#[cfg(unix)]
impl tesatlas_hub::collection::window::CollectorSlot for ServiceCollectorSlot {
    fn start(&mut self, deadline_wall: i64) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.running() {
            return Ok(());
        }
        // Nothing that could create or use a key starts before the
        // credential recovery the service waits for (review finding 2).
        if inner.recovery.pending() {
            return Err(tesatlas_hub::runtime::operator::authority::CREDENTIAL_RECOVERY_PENDING.to_owned());
        }
        // Collection starts only with an admitted own login, a configured car
        // and a positive cadence; inside the window the supervisor retries.
        match collector_can_start(&inner.store, &inner.config) {
            Ok(true) => {}
            Ok(false) => return Err("collector_not_ready".to_owned()),
            Err(error) => return Err(error.to_string()),
        }
        let (ready, ready_receiver) = tokio::sync::oneshot::channel();
        let (stop, stop_receiver) = tokio::sync::oneshot::channel::<()>();
        let store = inner.store.clone();
        let config = inner.config.clone();
        let admission = std::sync::Arc::clone(&inner.admission);
        let task = inner.runtime.spawn(async move {
            tracing::info!(deadline_wall, "collector starting inside the collection window");
            let result = collector::run_supervised_for_admitted_user(
                &store,
                &config,
                admission,
                ready,
                async move {
                    let _ = stop_receiver.await;
                },
            )
            .await;
            match result {
                Ok(()) => tracing::info!("collector stopped"),
                Err(error) => tracing::warn!(%error, "collector stopped with an error"),
            }
        });
        inner.running = Some(ServiceCollectorRun {
            task,
            stop: Some(stop),
            _ready: ready_receiver,
        });
        Ok(())
    }

    fn is_running(&mut self) -> bool {
        self.lock().running()
    }

    fn stop(&mut self) {
        if let Some(run) = self.lock().running.as_mut()
            && let Some(stop) = run.stop.take()
        {
            let _ = stop.send(());
        }
    }

    /// The collector releases its lease as it exits, so the lease is free
    /// once the task has finished.
    fn release_lease(&mut self) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.running() {
            return Err("collector_stopping".to_owned());
        }
        inner.running = None;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod shutdown_error_tests {
    use super::*;

    #[tokio::test]
    async fn observation_reports_collector_cleanup_failure_before_readiness() {
        let result = run_macos_serve_supervisor(
            true,
            |ready, shutdown| async move {
                let _ready = ready;
                let _ = shutdown.await;
                Err(std::io::Error::other("synthetic collector cleanup failure"))
            },
            |_, _| async { panic!("listener must not start before readiness") },
            async { MacServeControl::Shutdown },
        )
        .await;
        let error = result.expect_err("failed collector cleanup must fail the command");
        assert!(error.to_string().contains("synthetic collector cleanup failure"));
    }

    #[tokio::test]
    async fn observation_reports_collector_cleanup_failure_after_listener_exit() {
        let result = run_macos_serve_supervisor(
            true,
            |ready, shutdown| async move {
                let _ = ready.send(CursorKey::from_bytes([17; 32]));
                let _ = shutdown.await;
                Err(std::io::Error::other("synthetic collector cleanup failure"))
            },
            |_, _| async { Ok(()) },
            std::future::pending(),
        )
        .await;
        let error = result.expect_err("failed collector cleanup must fail the command");
        assert!(error.to_string().contains("synthetic collector cleanup failure"));
    }

    /// A clock the test moves by hand.
    #[derive(Default)]
    struct HandClock(std::sync::atomic::AtomicU64);

    impl tesatlas_hub::runtime::shutdown::MonotonicClock for HandClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(std::sync::atomic::Ordering::SeqCst))
        }
    }

    /// Issue #38: two workers that ignore the stop are stopped together
    /// within one budget, not one fixed wait after the other.
    #[tokio::test]
    async fn design_later_38_two_stuck_workers_share_one_budget() {
        let budget = std::sync::Arc::new(ShutdownBudget::new(
            std::sync::Arc::new(tesatlas_hub::runtime::shutdown::SystemMonotonic::default()),
            Duration::from_millis(1100),
            Duration::from_millis(100),
        ));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let began = std::sync::Arc::new(std::sync::Mutex::new(None));
        let control_began = std::sync::Arc::clone(&began);
        let result = run_macos_serve_supervisor_within(
            std::sync::Arc::clone(&budget),
            true,
            |ready, _shutdown| async move {
                let _ = ready.send(CursorKey::from_bytes([17; 32]));
                std::future::pending::<std::io::Result<()>>().await
            },
            |_, _shutdown| async move {
                let _ = started_tx.send(());
                std::future::pending::<std::io::Result<()>>().await
            },
            async move {
                let _ = started_rx.await;
                *control_began.lock().unwrap() = Some(std::time::Instant::now());
                MacServeControl::Shutdown
            },
        )
        .await;
        let took = began.lock().unwrap().expect("control fired").elapsed();
        let error = result.expect_err("both workers outlive the budget");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("shutdown budget"), "{error}");
        assert!(took >= Duration::from_millis(900), "the budget was used: {took:?}");
        assert!(took < Duration::from_millis(1800), "one budget, not two: {took:?}");
        assert!(budget.work_exhausted());
    }

    /// Issue #38: a stop accepted late gets only what is left of the budget
    /// that started earlier (for example during the recovery wait).
    #[tokio::test]
    async fn design_later_38_a_late_stop_gets_only_the_remaining_budget() {
        let clock = std::sync::Arc::new(HandClock::default());
        let budget = std::sync::Arc::new(ShutdownBudget::new(
            std::sync::Arc::clone(&clock) as std::sync::Arc<dyn tesatlas_hub::runtime::shutdown::MonotonicClock>,
            Duration::from_secs(50),
            Duration::from_secs(10),
        ));
        budget.begin();
        clock.0.store(39_950, std::sync::atomic::Ordering::SeqCst);
        let began = std::time::Instant::now();
        let result = run_macos_serve_supervisor_within(
            std::sync::Arc::clone(&budget),
            false,
            |_ready, _shutdown| async { Ok(()) },
            |_, shutdown| async move {
                let _ = shutdown.await;
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            },
            async { MacServeControl::Shutdown },
        )
        .await;
        let error = result.expect_err("the slow server outlives the remainder");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(began.elapsed() < Duration::from_secs(2), "{:?}", began.elapsed());
    }
}
