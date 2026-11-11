// SPDX-License-Identifier: AGPL-3.0-only

//! The service's window authority (design 2.8, rulings R5 and R9).
//!
//! The running service builds one window controller on the process-wide
//! egress gate, so opening a window is what lets this process send Tesla
//! traffic and closing it is what stops every send. A supervisor thread drives
//! the controller and the collector slot; the local control channel forwards
//! the operator's window, setup and recovery requests to [`OperatorService`].
//! The API is served whatever the collector does: the collector runs only
//! inside an Open window, and its exit never stops the service.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use super::{
    admission::OperatorRefusal,
    channel::{OperatorHandler, OperatorRequest, SetupMaterial},
    trust::{DialogConfirmation, TrustAuthority},
};
use crate::collection::window::{
    AnchorStore, CloseReason, CollectorSlot, EgressGate, GRANT_MAX_BYTES, Handback, HandbackSource,
    HighWater, HostClaim, KeychainAnchor, OffsetProbe, Phase, ResumeOutcome, SntpProbe,
    Supervisor, SystemClock, WindowConfig, WindowController, WindowDeps,
    WindowStatus,
};

/// Login-keychain service holding the Hub's consumption anchor (ruling R3).
pub const HUB_ANCHOR_SERVICE: &str = "Teslatlas Hub Window";
/// Login-keychain service holding the issuer's ledger pin (ruling R2).
pub const ISSUER_ANCHOR_SERVICE: &str = "Teslatlas Window Issuer";
/// The network time server of ruling R4 (not a Tesla endpoint).
pub const NTP_SERVER: &str = "time.apple.com:123";
const NTP_TIMEOUT: Duration = Duration::from_secs(5);

pub const WINDOW_CLOSED: &str = "window_closed";
pub const TRUST_DOCUMENT_MAX_BYTES: usize = 4096;

/// Window dependencies for the running service: the system clock and the
/// process-wide egress gate that every Tesla send in this process asks.
pub fn service_window_deps(
    anchor: Arc<dyn AnchorStore>,
    probe: Arc<dyn OffsetProbe>,
    handback: Arc<dyn HandbackSource>,
) -> WindowDeps {
    // The gate is created first so its system clock and this one share an
    // origin wherever the monotonic reading is process-scoped.
    let gate = EgressGate::process_wide().clone();
    WindowDeps {
        clock: Arc::new(SystemClock::new()),
        anchor,
        probe,
        gate,
        handback,
    }
}

pub fn service_window_config(data_dir: &Path, home: &Path, host_id: String) -> WindowConfig {
    WindowConfig {
        dir: super::window_dir(data_dir),
        claim_path: HostClaim::default_path(home),
        host_id,
    }
}

/// Starts the service's window authority for `data_dir` (it resumes or closes
/// whatever window the directory holds; design 2.9).
pub fn start_service_authority(
    data_dir: &Path,
    home: &Path,
) -> Result<(Arc<WindowController>, ResumeOutcome), OperatorRefusal> {
    let host_id = super::host::this_host_id()?;
    let (controller, outcome) = WindowController::start(
        service_window_config(data_dir, home, host_id),
        service_window_deps(
            Arc::new(KeychainAnchor::new(HUB_ANCHOR_SERVICE)),
            Arc::new(SntpProbe::new(NTP_SERVER, NTP_TIMEOUT)),
            Arc::new(StoreHighWater::new(data_dir)),
        ),
    );
    Ok((Arc::new(controller), outcome))
}

/// Per-table high-water marks read from the Hub database (read-only).
#[derive(Debug, Clone)]
pub struct StoreHighWater {
    database: PathBuf,
}

impl StoreHighWater {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            database: data_dir.join("hub.sqlite"),
        }
    }

    fn read(&self) -> Option<HighWater> {
        let connection = rusqlite::Connection::open_with_flags(
            &self.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        let greatest = |table: &str| -> Option<i64> {
            connection
                .query_row(&format!("SELECT MAX(rowid) FROM {table}"), [], |row| {
                    row.get::<_, Option<i64>>(0)
                })
                .ok()
                .flatten()
        };
        Some(HighWater {
            drives: greatest("materialised_drives"),
            charges: greatest("materialised_charges"),
            positions: greatest("materialised_positions"),
        })
    }
}

impl HandbackSource for StoreHighWater {
    fn high_water(&self) -> HighWater {
        self.read().unwrap_or_default()
    }
}

/// The redacted window status the operator commands print.
pub fn status_json(status: &WindowStatus) -> Value {
    json!({
        "phase": status.phase,
        "trusted": status.trusted,
        "faulted": status.faulted,
        "closeReason": status.close_reason,
        "seq": status.seq,
        "grantDigest": status.grant_digest,
        "startsAt": status.starts_at,
        "deadline": status.deadline,
        "openedAt": status.opened_at,
        "closingAt": status.closing_at,
        "lastHandback": status.last_handback,
    })
}

/// What only the binary can do inside the service: the code exchange with
/// discovery, and credential recovery on the service's own store.
pub trait ServiceBackend: Send + Sync {
    /// Called only while the window is Open.
    fn setup(
        &self,
        material: SetupMaterial,
        vehicle_id: Option<i64>,
        all_vehicles: bool,
    ) -> Result<Value, OperatorRefusal>;
    /// Called only while the window is Closed, holding the import lock.
    fn recovery_export(
        &self,
        destination: &Path,
        recovery_key_file: &Path,
    ) -> Result<Value, OperatorRefusal>;
    /// Called only while the window is Closed, holding the import lock.
    fn recovery_restore(
        &self,
        source: &Path,
        recovery_key_file: &Path,
    ) -> Result<Value, OperatorRefusal>;
}

/// The service's trust authority: the issuer pin in the login keychain and
/// the owner's confirmation dialog (review finding 3).
pub fn service_trust_authority(host_id: &str) -> TrustAuthority {
    TrustAuthority::new(
        Arc::new(KeychainAnchor::new(HUB_ANCHOR_SERVICE)),
        host_id,
        Arc::new(DialogConfirmation),
    )
}

/// Answers the control channel inside the running service.
pub struct OperatorService {
    controller: Arc<WindowController>,
    backend: Arc<dyn ServiceBackend>,
    kick: Box<dyn Fn() + Send + Sync>,
    trust: TrustAuthority,
    window_dir: PathBuf,
}

impl OperatorService {
    /// `kick` wakes the supervisor after a window change; `trust` decides
    /// who may change the trusted issuer of the window in `data_dir`.
    pub fn new(
        controller: Arc<WindowController>,
        backend: Arc<dyn ServiceBackend>,
        kick: Box<dyn Fn() + Send + Sync>,
        trust: TrustAuthority,
        data_dir: &Path,
    ) -> Self {
        Self {
            controller,
            backend,
            kick,
            trust,
            window_dir: super::window_dir(data_dir),
        }
    }

    fn decode(text: &str, maximum: usize, what: &str) -> Result<Vec<u8>, OperatorRefusal> {
        let bytes = STANDARD
            .decode(text.as_bytes())
            .map_err(|_| OperatorRefusal::new("request_invalid", format!("the {what} is not valid base64")))?;
        if bytes.len() > maximum {
            return Err(OperatorRefusal::new(
                "request_invalid",
                format!("the {what} is larger than {maximum} bytes"),
            ));
        }
        Ok(bytes)
    }

    fn status(&self) -> Value {
        status_json(&self.controller.status())
    }
}

impl OperatorHandler for OperatorService {
    fn handle(&self, request: OperatorRequest) -> Result<Value, OperatorRefusal> {
        match request {
            OperatorRequest::WindowStatus => Ok(self.status()),
            OperatorRequest::WindowTrust { document } => {
                let bytes = Self::decode(&document, TRUST_DOCUMENT_MAX_BYTES, "trust document")?;
                self.trust
                    .trust(&self.controller, &self.window_dir, &bytes)?;
                (self.kick)();
                Ok(self.status())
            }
            OperatorRequest::WindowOpen { grant } => {
                let bytes = Self::decode(&grant, GRANT_MAX_BYTES, "grant")?;
                self.controller
                    .open(&bytes)
                    .map_err(OperatorRefusal::window)?;
                (self.kick)();
                Ok(self.status())
            }
            OperatorRequest::WindowClose => {
                self.controller.close(CloseReason::OperatorClose);
                (self.kick)();
                Ok(self.status())
            }
            OperatorRequest::Setup {
                material,
                vehicle_id,
                all_vehicles,
            } => {
                // Tesla code exchange and discovery run only inside an Open
                // window; the egress gate refuses every send otherwise too.
                if self.controller.status().phase != Phase::Open {
                    return Err(OperatorRefusal::new(
                        WINDOW_CLOSED,
                        "sign-in and vehicle discovery need an open collection window",
                    ));
                }
                self.backend.setup(material, vehicle_id, all_vehicles)
            }
            OperatorRequest::RecoveryExport {
                destination,
                recovery_key_file,
            } => {
                let _guard = self
                    .controller
                    .import_guard()
                    .map_err(OperatorRefusal::window)?;
                self.backend
                    .recovery_export(&destination, &recovery_key_file)
            }
            OperatorRequest::RecoveryRestore {
                source,
                recovery_key_file,
            } => {
                let _guard = self
                    .controller
                    .import_guard()
                    .map_err(OperatorRefusal::window)?;
                self.backend.recovery_restore(&source, &recovery_key_file)
            }
        }
    }
}

enum Signal {
    Kick,
    Stop,
}

/// The supervisor thread: it steps the window supervisor at least every
/// `next_wakeup_seconds` and at once after a kick, and stops the collector
/// when the service stops (the window itself survives a restart; design 2.9).
pub struct SupervisorThread {
    sender: mpsc::Sender<Signal>,
    thread: Option<JoinHandle<()>>,
}

/// How long a stopping service waits for its collector to exit.
pub const COLLECTOR_STOP_WAIT: Duration = Duration::from_secs(30);

impl SupervisorThread {
    pub fn spawn<S>(controller: Arc<WindowController>, slot: S) -> std::io::Result<Self>
    where
        S: CollectorSlot + Clone + Send + 'static,
    {
        let mut shutdown_slot = slot.clone();
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("hub-window-supervisor".to_owned())
            .spawn(move || {
                let mut supervisor = Supervisor::new(controller.clone(), slot);
                loop {
                    let report = supervisor.step();
                    if let Some(reason) = report.closed {
                        tracing::info!(reason = reason.code(), "collection window closed");
                    }
                    if let Some(fault) = report.fault {
                        tracing::warn!(code = fault.code(), "collection window fault");
                    }
                    let wait = controller.next_wakeup_seconds().max(1) as u64;
                    match receiver.recv_timeout(Duration::from_secs(wait)) {
                        Ok(Signal::Kick) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Ok(Signal::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                drop(supervisor);
                stop_for_shutdown(&mut shutdown_slot, COLLECTOR_STOP_WAIT);
            })?;
        Ok(Self {
            sender,
            thread: Some(thread),
        })
    }

    /// A cloneable wake-up for the channel handler.
    pub fn kicker(&self) -> Box<dyn Fn() + Send + Sync> {
        let sender = Mutex::new(self.sender.clone());
        Box::new(move || {
            if let Ok(sender) = sender.lock() {
                let _ = sender.send(Signal::Kick);
            }
        })
    }

    /// Stops the collector and the thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.sender.send(Signal::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SupervisorThread {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Runs a bounded observation under the window for its whole lifetime
/// (review finding 5). The window is ticked at least every `max_poll` and at
/// its deadline, so the periodic clock and time-offset checks run while the
/// observation does; once the window leaves Open the observation is told to
/// stop through the receiver it was given. Only after the observation has
/// returned, and with it released its collector lease, is a Closing window
/// handed back and Closed. Returns the observation's result and the
/// handback, if this run closed the window.
pub async fn supervise_observation<T, F, Fut>(
    controller: Arc<WindowController>,
    max_poll: Duration,
    observation: F,
) -> (T, Result<Option<Handback>, OperatorRefusal>)
where
    F: FnOnce(tokio::sync::oneshot::Receiver<()>) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut stop = Some(stop);
    let observation = observation(stopped);
    tokio::pin!(observation);
    let result = loop {
        let wait = Duration::from_secs(controller.next_wakeup_seconds().max(1) as u64).min(max_poll);
        tokio::select! {
            result = &mut observation => break result,
            () = tokio::time::sleep(wait) => {
                let ticking = Arc::clone(&controller);
                let phase = tokio::task::spawn_blocking(move || ticking.tick())
                    .await
                    .ok()
                    .and_then(Result::ok);
                if phase != Some(Phase::Open)
                    && let Some(stop) = stop.take()
                {
                    tracing::info!("the collection window left Open; stopping the observation");
                    let _ = stop.send(());
                }
            }
        }
    };
    let closing = Arc::clone(&controller);
    let closed = tokio::task::spawn_blocking(move || {
        let _ = closing.tick();
        if closing.status().phase == Phase::Closing {
            closing.finish_close().map_err(OperatorRefusal::window)
        } else {
            Ok(None)
        }
    })
    .await
    .unwrap_or_else(|_| Err(OperatorRefusal::window(crate::collection::window::Refusal::Io)));
    (result, closed)
}

/// Signals the collector to stop and waits for it to exit, up to `wait`.
fn stop_for_shutdown<S: CollectorSlot>(slot: &mut S, wait: Duration) {
    slot.stop();
    let deadline = std::time::Instant::now() + wait;
    while slot.is_running() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}
