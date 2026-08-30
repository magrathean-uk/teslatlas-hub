// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic loopback fake Tesla Owner API + streaming source.
//!
//! Used only for local replacement proof (R04–R06). It never contacts a real
//! Tesla host. The request ledger rejects wake, command, unexpected routes,
//! and non-loopback bind attempts.

use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::{Method, StatusCode, Uri},
    response::IntoResponse,
    routing::{any, get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    net::TcpListener,
    sync::{oneshot, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use url::Url;

/// Canonical fixture car identity (loopback TeslaMate dump, car_id=1).
pub const FIXTURE_EID: u64 = 1_493_114_796_524_256;
pub const FIXTURE_VID: u64 = 182_630_373_857;
const FIXTURE_VID_TAG: &str = "182630373857";
pub const FIXTURE_VIN: &str = "LRW3F7EB0MC165515";
pub const FIXTURE_DISPLAY_NAME: &str = "Athena";
/// Deterministic fake successor pair returned by the loopback OAuth endpoint.
/// These values are test fixtures, never real credentials.
pub const FAKE_REFRESHED_ACCESS_TOKEN: &str = "qts-fake-successor-access";
pub const FAKE_REFRESHED_REFRESH_TOKEN: &str = "fake-successor-refresh";

/// Canonical SERVICE_PARITY scenario steps in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioStep {
    AsleepDiscovery,
    OfflineDiscovery,
    OnlineIdle,
    DrivePositions,
    Parked,
    ChargeSamples,
    AsleepRestart,
    OnlineSoftwareUpdate,
    UnchangedNoOp,
}

impl ScenarioStep {
    pub const ALL: [Self; 9] = [
        Self::AsleepDiscovery,
        Self::OfflineDiscovery,
        Self::OnlineIdle,
        Self::DrivePositions,
        Self::Parked,
        Self::ChargeSamples,
        Self::AsleepRestart,
        Self::OnlineSoftwareUpdate,
        Self::UnchangedNoOp,
    ];

    pub fn discovery_state(self) -> &'static str {
        match self {
            Self::AsleepDiscovery | Self::AsleepRestart => "asleep",
            Self::OfflineDiscovery => "offline",
            Self::OnlineIdle
            | Self::DrivePositions
            | Self::Parked
            | Self::ChargeSamples
            | Self::OnlineSoftwareUpdate
            | Self::UnchangedNoOp => "online",
        }
    }

    pub fn is_online(self) -> bool {
        self.discovery_state() == "online"
    }
}

/// Stable, fake-owned account of the canonical scenario state machine.
///
/// It deliberately excludes wall-clock information and payload fingerprints:
/// the ledger proves the deterministic fixture progression, not request timing
/// or sensitive client material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum ScenarioLedgerEvent {
    Initial {
        step: ScenarioStep,
    },
    AutoTransition {
        from: ScenarioStep,
        to: ScenarioStep,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditedRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub scenario_step: ScenarioStep,
    /// Exact fake response status. A controlled 503 remains distinct from a
    /// rejected wake/command attempt.
    pub response_status: u16,
    pub rejected: bool,
    pub reject_reason: Option<String>,
}

/// Redacted evidence for one streaming connection.  The fake deliberately
/// never stores a bearer token or a complete client frame: the protocol facts
/// needed by the local journey are the control message, fixture tag, and exact
/// field set only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditedStreamEvent {
    pub session_id: u64,
    pub event: StreamAuditEvent,
    pub tag: Option<String>,
    pub fields: Option<String>,
    pub scenario_step: ScenarioStep,
    pub accepted: bool,
    pub reject_reason: Option<String>,
    /// Number of live WebSocket sessions immediately after this event.  This
    /// makes the append-only ledger sufficient to prove that a reconnect did
    /// not overlap an old session.
    pub active_session_count: usize,
    /// High-water mark of concurrent live WebSocket sessions at this event.
    pub max_concurrent_session_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamAuditEvent {
    Connect,
    Subscribe,
    Hello,
    Unsubscribe,
    Disconnect,
    Rejected,
}

/// Exact live-session counters for deterministic stream outage/recovery
/// harnesses. Counts cover successful WebSocket handshakes only; rejected
/// handshakes remain visible in [`AuditedStreamEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StreamSessionStats {
    pub connection_attempts: usize,
    pub active_sessions: usize,
    pub max_concurrent_sessions: usize,
    pub accepted_connections: usize,
    pub rejected_connections: usize,
}

#[derive(Debug, Clone)]
struct FakeInner {
    step_index: usize,
    /// Vehicle-data responses remaining in the current step (drive/charge).
    substep: usize,
    requests: Vec<AuditedRequest>,
    stream_events: Vec<AuditedStreamEvent>,
    next_stream_session_id: u64,
    stream_connection_attempts: usize,
    /// Fault control defaults to available so existing canonical tests and
    /// fixtures retain their original behaviour.
    owner_vehicle_data_available: bool,
    stream_available: bool,
    active_stream_sessions: BTreeSet<u64>,
    max_concurrent_stream_sessions: usize,
    accepted_stream_connections: usize,
    rejected_stream_connections: usize,
    rejected_count: usize,
    token_refresh_requests: usize,
    /// Wall-clock virtual timestamps for ordered samples.
    base_ts_ms: i64,
    /// Most recent timestamp returned by the Owner API `vehicle_data` route.
    /// Stream reconnect telemetry is placed strictly after this value and
    /// before the next deterministic Owner API sample.
    last_owner_data_ts_ms: Option<i64>,
    /// Odometer from the most recent Owner API `vehicle_data` payload. Stream
    /// samples interpolate from this exact value to the next Owner sample so
    /// reconnects cannot overtake a lifecycle-closing observation.
    last_owner_odometer_miles: Option<f64>,
    /// Last valid interpolated telemetry range. Exact no-op Owner samples have
    /// no open interval, so the fake replays this range; the production
    /// collector rejects it as old while one-shot no-wake probes can still
    /// observe a numeric-power frame.
    last_stream_timestamps: Vec<i64>,
    /// Additive fixture offset applied to every Owner API and stream odometer.
    odometer_offset_miles: f64,
}

impl Default for FakeInner {
    fn default() -> Self {
        Self {
            step_index: 0,
            substep: 0,
            requests: Vec::new(),
            stream_events: Vec::new(),
            next_stream_session_id: 1,
            stream_connection_attempts: 0,
            owner_vehicle_data_available: true,
            stream_available: true,
            active_stream_sessions: BTreeSet::new(),
            max_concurrent_stream_sessions: 0,
            accepted_stream_connections: 0,
            rejected_stream_connections: 0,
            rejected_count: 0,
            token_refresh_requests: 0,
            // Past wall time so Owner-API observation timestamps are accepted
            // (future timestamps are clamped / rejected by the collector).
            base_ts_ms: 1_700_000_000_000,
            last_owner_data_ts_ms: None,
            last_owner_odometer_miles: None,
            last_stream_timestamps: Vec::new(),
            odometer_offset_miles: 0.0,
        }
    }
}

#[derive(Clone)]
struct AppState {
    inner: Arc<Mutex<FakeInner>>,
    /// Strict synchronous serialization for fake evidence files. The lock
    /// order is always evidence -> inner; it is never held across async I/O.
    /// This keeps each mutation and its JSONL/JSON snapshot atomically ordered.
    evidence_serialization: Arc<Mutex<()>>,
    advance_mode: AdvanceMode,
    /// When set, every audited request is appended as JSONL and status is rewritten.
    evidence_dir: Option<PathBuf>,
    /// Monotonically bumped when active/pending stream work must stop. A
    /// watch channel lets every task select on the same deterministic fault
    /// control without leaving detached connection tasks behind.
    stream_interrupt: watch::Sender<u64>,
}

/// How the scenario advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvanceMode {
    /// Advance to the next top-level step after each products() call once the
    /// current step's vehicle_data budget is exhausted (or immediately for
    /// asleep steps that never call vehicle_data).
    AutoOnDiscovery,
    /// Only advance when the harness calls [`FakeTeslaSource::advance`].
    Manual,
}

#[derive(Debug, Error)]
pub enum FakeTeslaError {
    #[error("bind failed: {0}")]
    Bind(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("server failed")]
    Server,
    #[error("invalid TESLATLAS_FAKE_TESLA_ODOMETER_OFFSET_MILES: {0}")]
    InvalidOdometerOffset(String),
}

/// Running local fake Tesla source (Owner API HTTP + streaming websocket).
pub struct FakeTeslaSource {
    http_base: Url,
    stream_endpoint: String,
    state: AppState,
    http_task: JoinHandle<()>,
    stream_task: JoinHandle<()>,
    shutdown: Option<oneshot::Sender<()>>,
    stream_shutdown: Option<oneshot::Sender<()>>,
}

impl FakeTeslaSource {
    /// Bind loopback-only HTTP + WS listeners and serve the canonical scenario.
    pub async fn spawn_canonical(advance: AdvanceMode) -> Result<Self, FakeTeslaError> {
        Self::spawn(advance, None).await
    }

    pub async fn spawn(
        advance: AdvanceMode,
        evidence_dir: Option<&Path>,
    ) -> Result<Self, FakeTeslaError> {
        let evidence_owned = evidence_dir.map(Path::to_path_buf);
        if let Some(dir) = evidence_owned.as_ref() {
            std::fs::create_dir_all(dir)?;
            // Truncate prior ledger so each serve owns a complete file.
            let _ = std::fs::write(dir.join("request-ledger.jsonl"), b"");
            let _ = std::fs::write(dir.join("stream-ledger.jsonl"), b"");
            let _ = std::fs::write(dir.join("scenario-ledger.jsonl"), b"");
        }
        let (stream_interrupt, _stream_interrupt_rx) = watch::channel(0_u64);
        let state = AppState {
            inner: Arc::new(Mutex::new(FakeInner::default())),
            evidence_serialization: Arc::new(Mutex::new(())),
            advance_mode: advance,
            evidence_dir: evidence_owned.clone(),
            stream_interrupt,
        };
        if advance == AdvanceMode::AutoOnDiscovery {
            append_scenario_event(
                evidence_owned.as_deref(),
                &ScenarioLedgerEvent::Initial {
                    step: ScenarioStep::AsleepDiscovery,
                },
            );
        }

        let http_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| FakeTeslaError::Bind(e.to_string()))?;
        let http_addr = http_listener
            .local_addr()
            .map_err(|e| FakeTeslaError::Bind(e.to_string()))?;
        assert_loopback(http_addr)?;

        let ws_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| FakeTeslaError::Bind(e.to_string()))?;
        let ws_addr = ws_listener
            .local_addr()
            .map_err(|e| FakeTeslaError::Bind(e.to_string()))?;
        assert_loopback(ws_addr)?;

        let router = Router::new()
            .route("/api/1/products", get(products_handler))
            .route("/oauth2/v3/token", post(token_handler))
            .route(
                "/api/1/vehicles/{vehicle_id}/vehicle_data",
                get(vehicle_data_handler),
            )
            .fallback(any(reject_handler))
            .with_state(state.clone());

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let http_task = tokio::spawn(async move {
            axum::serve(http_listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .ok();
        });

        let stream_state = state.clone();
        let (stream_shutdown_tx, stream_shutdown_rx) = oneshot::channel::<()>();
        let stream_task = tokio::spawn(async move {
            let mut stream_shutdown_rx = stream_shutdown_rx;
            let mut sessions = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut stream_shutdown_rx => break,
                    joined = sessions.join_next(), if !sessions.is_empty() => {
                        let _ = joined;
                    }
                    accepted = ws_listener.accept() => {
                        let Ok((tcp, _)) = accepted else { break };
                        let stream_state = stream_state.clone();
                        let stream_interrupt = stream_state.stream_interrupt.subscribe();
                        sessions.spawn(async move {
                            serve_stream_connection(tcp, &stream_state, stream_interrupt).await;
                        });
                    }
                }
            }
            interrupt_stream_connections(&stream_state);
            while sessions.join_next().await.is_some() {}
        });

        let http_base = Url::parse(&format!("http://{http_addr}/"))
            .map_err(|e| FakeTeslaError::Bind(e.to_string()))?;
        let stream_endpoint = format!("ws://{ws_addr}/streaming/");

        if let Some(dir) = evidence_dir {
            std::fs::create_dir_all(dir)?;
            std::fs::write(
                dir.join("fake-tesla-bind.txt"),
                format!(
                    "httpBase={}\nstreamEndpoint={}\nbind=127.0.0.1\nnetworkScope=loopback\n",
                    http_base.as_str(),
                    stream_endpoint
                ),
            )?;
        }

        Ok(Self {
            http_base,
            stream_endpoint,
            state,
            http_task,
            stream_task,
            shutdown: Some(shutdown_tx),
            stream_shutdown: Some(stream_shutdown_tx),
        })
    }

    pub fn http_base_url(&self) -> &Url {
        &self.http_base
    }

    pub fn stream_endpoint(&self) -> &str {
        &self.stream_endpoint
    }

    /// Loopback issuer URL for tests that construct a test-only legacy auth
    /// manager. Production Tesla token routing remains canonical Tesla-only.
    pub fn oauth_issuer_url(&self) -> Url {
        self.http_base
            .join("oauth2/v3/")
            .expect("fake HTTP base is a valid URL")
    }

    /// Number of valid refresh-token requests accepted by the fake endpoint.
    /// Request bodies and token values are never retained.
    pub fn token_refresh_request_count(&self) -> usize {
        self.state
            .inner
            .lock()
            .expect("fake lock")
            .token_refresh_requests
    }

    /// Make only the no-wake `vehicle_data` Owner route return a deterministic
    /// 503. Discovery (`products`) and the fake's safety-rejection routes stay
    /// available, so a recovery test can distinguish data-path loss from
    /// discovery loss or a forbidden wake/command.
    pub fn set_owner_vehicle_data_available(&self, available: bool) {
        self.state
            .inner
            .lock()
            .expect("fake lock")
            .owner_vehicle_data_available = available;
    }

    pub fn owner_vehicle_data_available(&self) -> bool {
        self.state
            .inner
            .lock()
            .expect("fake lock")
            .owner_vehicle_data_available
    }

    /// Toggle only the fake streaming transport. Disabling it immediately
    /// interrupts all live sessions and makes future handshakes receive a
    /// deterministic 503 until restored.
    pub fn set_stream_available(&self, available: bool) {
        let changed_to_unavailable = {
            let mut inner = self.state.inner.lock().expect("fake lock");
            let changed = inner.stream_available != available;
            inner.stream_available = available;
            changed && !available
        };
        if changed_to_unavailable {
            interrupt_stream_connections(&self.state);
        }
    }

    pub fn stream_available(&self) -> bool {
        self.state.inner.lock().expect("fake lock").stream_available
    }

    /// Snapshot suitable for asserting exact outage/recovery connection
    /// behaviour without inspecting scheduler timing.
    pub fn stream_session_stats(&self) -> StreamSessionStats {
        let inner = self.state.inner.lock().expect("fake lock");
        stream_session_stats_from_inner(&inner)
    }

    pub fn current_step(&self) -> ScenarioStep {
        let inner = self.state.inner.lock().expect("fake lock");
        ScenarioStep::ALL[inner.step_index.min(ScenarioStep::ALL.len() - 1)]
    }

    pub fn advance(&self) {
        let mut inner = self.state.inner.lock().expect("fake lock");
        if inner.step_index + 1 < ScenarioStep::ALL.len() {
            inner.step_index += 1;
            inner.substep = 0;
        }
    }

    pub fn set_step(&self, step: ScenarioStep) {
        let mut inner = self.state.inner.lock().expect("fake lock");
        if let Some(idx) = ScenarioStep::ALL.iter().position(|s| *s == step) {
            inner.step_index = idx;
            inner.substep = 0;
        }
    }

    /// Set the virtual vehicle-data base timestamp (must stay in the past relative
    /// to wall clock so collector observation timestamps are accepted).
    pub fn set_base_ts_ms(&self, base_ts_ms: i64) {
        let mut inner = self.state.inner.lock().expect("fake lock");
        inner.base_ts_ms = base_ts_ms;
        inner.last_owner_data_ts_ms = None;
        inner.last_owner_odometer_miles = None;
        inner.last_stream_timestamps.clear();
    }

    /// Apply a non-negative, finite offset to all fixture odometers.
    pub fn set_odometer_offset_miles(&self, offset_miles: f64) -> Result<(), FakeTeslaError> {
        validate_odometer_offset_miles(offset_miles)?;
        self.state
            .inner
            .lock()
            .expect("fake lock")
            .odometer_offset_miles = offset_miles;
        Ok(())
    }

    pub fn base_ts_ms(&self) -> i64 {
        self.state.inner.lock().expect("fake lock").base_ts_ms
    }

    pub fn audited_requests(&self) -> Vec<AuditedRequest> {
        self.state.inner.lock().expect("fake lock").requests.clone()
    }

    pub fn audited_stream_events(&self) -> Vec<AuditedStreamEvent> {
        self.state
            .inner
            .lock()
            .expect("fake lock")
            .stream_events
            .clone()
    }

    pub fn rejected_count(&self) -> usize {
        self.state.inner.lock().expect("fake lock").rejected_count
    }

    /// Persist the full request ledger as JSON (complete snapshot for journey gates).
    pub fn write_ledger_snapshot(&self) -> Result<(), String> {
        let Some(dir) = self.state.evidence_dir.as_ref() else {
            return Ok(());
        };
        let _evidence_serialization = self
            .state
            .evidence_serialization
            .lock()
            .expect("fake evidence serialization lock");
        let requests = self.audited_requests();
        let stream_events = self.audited_stream_events();
        let step = self.current_step();
        let body = serde_json::json!({
            "step": step,
            "requestCount": requests.len(),
            "rejectedCount": self.rejected_count(),
            "requests": requests,
            "streamEventCount": stream_events.len(),
            "streamEvents": stream_events,
        });
        let path = dir.join("request-ledger.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        write_status_file(dir, step, requests.len(), self.rejected_count());
        write_stream_ledger_snapshot(
            dir,
            &self.audited_stream_events(),
            self.stream_session_stats(),
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Assert the request ledger contains only permitted GET/stream routes.
    pub fn assert_no_wake_or_command(&self) -> Result<(), String> {
        let requests = self.audited_requests();
        for req in &requests {
            if req.rejected {
                return Err(format!(
                    "rejected request recorded: {} {} ({})",
                    req.method,
                    req.path,
                    req.reject_reason.as_deref().unwrap_or("unknown")
                ));
            }
            let method = req.method.to_ascii_uppercase();
            if method != "GET" && method != "HEAD" {
                return Err(format!("non-GET request: {} {}", req.method, req.path));
            }
            let path = req.path.to_ascii_lowercase();
            if path.contains("wake") || path.contains("command") {
                return Err(format!("wake/command path: {}", req.path));
            }
        }
        Ok(())
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        // A source shutdown is an outage even if a test had already disabled
        // streaming. Bump the epoch unconditionally so pending handshakes wake.
        self.set_stream_available(false);
        interrupt_stream_connections(&self.state);
        if let Some(tx) = self.stream_shutdown.take() {
            let _ = tx.send(());
        }
        await_or_abort(&mut self.http_task).await;
        await_or_abort(&mut self.stream_task).await;
        // The stream task owns and joins every child session before this
        // final snapshot, so evidence cannot claim shutdown while a session
        // is still live.
        let _ = self.write_ledger_snapshot();
    }
}

async fn await_or_abort(task: &mut JoinHandle<()>) {
    if tokio::time::timeout(Duration::from_secs(1), &mut *task)
        .await
        .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}

fn write_status_file(dir: &Path, step: ScenarioStep, request_count: usize, rejected: usize) {
    let _ = std::fs::write(
        dir.join("fake-tesla-status.txt"),
        format!(
            "step={}\nrequestCount={}\nrejectedCount={}\n",
            serde_json::to_value(step)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("{step:?}")),
            request_count,
            rejected
        ),
    );
}

fn append_scenario_event(evidence_dir: Option<&Path>, event: &ScenarioLedgerEvent) {
    let Some(dir) = evidence_dir else {
        return;
    };
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("scenario-ledger.jsonl"))
    {
        let _ = writeln!(file, "{line}");
    }
}

fn advance_automatically(inner: &mut FakeInner) -> Option<(ScenarioStep, ScenarioStep)> {
    let from = ScenarioStep::ALL[inner.step_index.min(ScenarioStep::ALL.len() - 1)];
    if inner.step_index + 1 >= ScenarioStep::ALL.len() {
        return None;
    }
    inner.step_index += 1;
    inner.substep = 0;
    Some((from, ScenarioStep::ALL[inner.step_index]))
}

fn validate_odometer_offset_miles(offset_miles: f64) -> Result<(), FakeTeslaError> {
    if offset_miles.is_finite() && offset_miles >= 0.0 {
        return Ok(());
    }
    Err(FakeTeslaError::InvalidOdometerOffset(format!(
        "must be a finite non-negative number (got {offset_miles})"
    )))
}

fn parse_odometer_offset_miles(raw: &str) -> Result<f64, FakeTeslaError> {
    let offset_miles = raw
        .trim()
        .parse::<f64>()
        .map_err(|_| FakeTeslaError::InvalidOdometerOffset(raw.to_owned()))?;
    validate_odometer_offset_miles(offset_miles)?;
    Ok(offset_miles)
}

fn assert_loopback(addr: SocketAddr) -> Result<(), FakeTeslaError> {
    if !addr.ip().is_loopback() {
        return Err(FakeTeslaError::Bind(format!(
            "refusing non-loopback bind {addr}"
        )));
    }
    Ok(())
}

fn record(
    state: &AppState,
    method: &str,
    path: &str,
    query: &str,
    response_status: StatusCode,
    rejected: bool,
    reason: Option<&str>,
) {
    let _evidence_serialization = state
        .evidence_serialization
        .lock()
        .expect("fake evidence serialization lock");
    let (step, request_count, rejected_count, entry) = {
        let mut inner = state.inner.lock().expect("fake lock");
        let step = ScenarioStep::ALL[inner.step_index.min(ScenarioStep::ALL.len() - 1)];
        if rejected {
            inner.rejected_count = inner.rejected_count.saturating_add(1);
        }
        let entry = AuditedRequest {
            method: method.to_owned(),
            path: path.to_owned(),
            query: query.to_owned(),
            scenario_step: step,
            response_status: response_status.as_u16(),
            rejected,
            reject_reason: reason.map(ToOwned::to_owned),
        };
        inner.requests.push(entry.clone());
        (step, inner.requests.len(), inner.rejected_count, entry)
    };
    if let Some(dir) = state.evidence_dir.as_ref() {
        if let Ok(line) = serde_json::to_string(&entry) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("request-ledger.jsonl"))
            {
                let _ = writeln!(f, "{line}");
            }
        }
        write_status_file(dir, step, request_count, rejected_count);
        // Full JSON snapshot after each request so journeys can poll without SIGTERM.
        let requests = state.inner.lock().expect("fake lock").requests.clone();
        let body = serde_json::json!({
            "step": step,
            "requestCount": request_count,
            "rejectedCount": rejected_count,
            "requests": requests,
        });
        if let Ok(bytes) = serde_json::to_vec_pretty(&body) {
            let _ = std::fs::write(dir.join("request-ledger.json"), bytes);
        }
        // A later HTTP request (for example the journey's explicit wake
        // rejection) also publishes the latest stream snapshot.  This closes
        // the small scheduling window between a collector restart and its TCP
        // disconnect being observed by the WebSocket task.
        let (stream_events, stats) = {
            let inner = state.inner.lock().expect("fake lock");
            (
                inner.stream_events.clone(),
                stream_session_stats_from_inner(&inner),
            )
        };
        let _ = write_stream_ledger_snapshot(dir, &stream_events, stats);
    }
}

fn record_stream_event(
    state: &AppState,
    session_id: u64,
    event: StreamAuditEvent,
    tag: Option<String>,
    fields: Option<String>,
    accepted: bool,
    reason: Option<&str>,
) {
    let _evidence_serialization = state
        .evidence_serialization
        .lock()
        .expect("fake evidence serialization lock");
    let (entry, stats) = {
        let mut inner = state.inner.lock().expect("fake lock");
        append_stream_event(&mut inner, session_id, event, tag, fields, accepted, reason)
    };
    persist_stream_entries_locked(state, std::slice::from_ref(&entry), stats);
}

fn append_stream_event(
    inner: &mut FakeInner,
    session_id: u64,
    event: StreamAuditEvent,
    tag: Option<String>,
    fields: Option<String>,
    accepted: bool,
    reason: Option<&str>,
) -> (AuditedStreamEvent, StreamSessionStats) {
    let step = ScenarioStep::ALL[inner.step_index.min(ScenarioStep::ALL.len() - 1)];
    if event == StreamAuditEvent::Disconnect {
        inner.active_stream_sessions.remove(&session_id);
    }
    let stats = stream_session_stats_from_inner(inner);
    let entry = AuditedStreamEvent {
        session_id,
        event,
        tag,
        fields,
        scenario_step: step,
        accepted,
        reject_reason: reason.map(ToOwned::to_owned),
        active_session_count: stats.active_sessions,
        max_concurrent_session_count: stats.max_concurrent_sessions,
    };
    inner.stream_events.push(entry.clone());
    (entry, stats)
}

/// Caller holds AppState::evidence_serialization. Appending every JSONL line
/// before replacing the full snapshot means a reader observes either the old
/// complete snapshot or an ordered prefix followed by the new complete one.
fn persist_stream_entries_locked(
    state: &AppState,
    entries: &[AuditedStreamEvent],
    stats: StreamSessionStats,
) {
    if let Some(dir) = state.evidence_dir.as_ref() {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("stream-ledger.jsonl"))
        {
            for entry in entries {
                if let Ok(line) = serde_json::to_string(entry) {
                    let _ = writeln!(file, "{line}");
                }
            }
        }
        let stream_events = state.inner.lock().expect("fake lock").stream_events.clone();
        let _ = write_stream_ledger_snapshot(dir, &stream_events, stats);
    }
}

fn write_stream_ledger_snapshot(
    dir: &Path,
    stream_events: &[AuditedStreamEvent],
    stats: StreamSessionStats,
) -> Result<(), std::io::Error> {
    let sessions = stream_events
        .iter()
        .filter(|event| event.event == StreamAuditEvent::Connect)
        .count();
    let body = serde_json::json!({
        "streamEventCount": stream_events.len(),
        "streamSessionCount": sessions,
        "streamSessionStats": stats,
        "events": stream_events,
    });
    std::fs::write(
        dir.join("stream-ledger.json"),
        serde_json::to_vec_pretty(&body).expect("stream ledger serialization"),
    )
}

fn stream_session_stats_from_inner(inner: &FakeInner) -> StreamSessionStats {
    StreamSessionStats {
        connection_attempts: inner.stream_connection_attempts,
        active_sessions: inner.active_stream_sessions.len(),
        max_concurrent_sessions: inner.max_concurrent_stream_sessions,
        accepted_connections: inner.accepted_stream_connections,
        rejected_connections: inner.rejected_stream_connections,
    }
}

fn next_stream_session(state: &AppState) -> u64 {
    let _evidence_serialization = state
        .evidence_serialization
        .lock()
        .expect("fake evidence serialization lock");
    let mut inner = state.inner.lock().expect("fake lock");
    let session_id = inner.next_stream_session_id;
    inner.next_stream_session_id = inner.next_stream_session_id.saturating_add(1);
    session_id
}

fn interrupt_stream_connections(state: &AppState) {
    state
        .stream_interrupt
        .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
}

fn append_rejected_stream_handshake(
    inner: &mut FakeInner,
    session_id: u64,
    reason: &'static str,
) -> (Vec<AuditedStreamEvent>, StreamSessionStats) {
    inner.stream_connection_attempts = inner.stream_connection_attempts.saturating_add(1);
    inner.rejected_stream_connections = inner.rejected_stream_connections.saturating_add(1);
    let (connect, _) = append_stream_event(
        inner,
        session_id,
        StreamAuditEvent::Connect,
        None,
        None,
        false,
        Some(reason),
    );
    let (rejected, stats) = append_stream_event(
        inner,
        session_id,
        StreamAuditEvent::Rejected,
        None,
        None,
        false,
        Some(reason),
    );
    (vec![connect, rejected], stats)
}

fn classify_prehandshake_unavailability(state: &AppState, session_id: u64) -> bool {
    let _evidence_serialization = state
        .evidence_serialization
        .lock()
        .expect("fake evidence serialization lock");
    let classification = {
        let mut inner = state.inner.lock().expect("fake lock");
        (!inner.stream_available)
            .then(|| append_rejected_stream_handshake(&mut inner, session_id, "stream_unavailable"))
    };
    if let Some((entries, stats)) = classification {
        persist_stream_entries_locked(state, &entries, stats);
        return true;
    }
    false
}

fn classify_failed_stream_handshake(state: &AppState, session_id: u64, reason: &'static str) {
    let _evidence_serialization = state
        .evidence_serialization
        .lock()
        .expect("fake evidence serialization lock");
    let (entries, stats) = {
        let mut inner = state.inner.lock().expect("fake lock");
        append_rejected_stream_handshake(&mut inner, session_id, reason)
    };
    persist_stream_entries_locked(state, &entries, stats);
}

fn classify_completed_stream_handshake(state: &AppState, session_id: u64) -> bool {
    let _evidence_serialization = state
        .evidence_serialization
        .lock()
        .expect("fake evidence serialization lock");
    let (accepted, entries, stats) = {
        let mut inner = state.inner.lock().expect("fake lock");
        if inner.stream_available {
            inner.stream_connection_attempts = inner.stream_connection_attempts.saturating_add(1);
            let inserted = inner.active_stream_sessions.insert(session_id);
            debug_assert!(inserted, "stream session ids are unique");
            inner.accepted_stream_connections = inner.accepted_stream_connections.saturating_add(1);
            inner.max_concurrent_stream_sessions = inner
                .max_concurrent_stream_sessions
                .max(inner.active_stream_sessions.len());
            let (connect, stats) = append_stream_event(
                &mut inner,
                session_id,
                StreamAuditEvent::Connect,
                None,
                None,
                true,
                None,
            );
            (true, vec![connect], stats)
        } else {
            // The websocket handshake completed, but the fault toggle won
            // before classification. Treat this attempt as rejected rather
            // than admitting a session after outage begins.
            let (entries, stats) =
                append_rejected_stream_handshake(&mut inner, session_id, "stream_unavailable");
            (false, entries, stats)
        }
    };
    persist_stream_entries_locked(state, &entries, stats);
    accepted
}

struct ActiveStreamSession {
    state: AppState,
    session_id: u64,
    tag: Option<String>,
    accepted_subscription: bool,
    disconnect_reason: Option<&'static str>,
    finished: bool,
}

impl ActiveStreamSession {
    fn new(state: AppState, session_id: u64) -> Self {
        Self {
            state,
            session_id,
            tag: None,
            accepted_subscription: false,
            disconnect_reason: None,
            finished: false,
        }
    }

    fn accept_subscription(&mut self, tag: String) {
        self.tag = Some(tag);
        self.accepted_subscription = true;
    }

    fn set_disconnect_reason(&mut self, reason: &'static str) {
        self.disconnect_reason = Some(reason);
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        record_stream_event(
            &self.state,
            self.session_id,
            StreamAuditEvent::Disconnect,
            self.tag.clone(),
            None,
            self.accepted_subscription,
            self.disconnect_reason,
        );
        self.finished = true;
    }
}

impl Drop for ActiveStreamSession {
    fn drop(&mut self) {
        if !self.finished {
            self.set_disconnect_reason("stream_task_cancelled");
            self.finish();
        }
    }
}

async fn reject_stream_handshake(mut tcp: tokio::net::TcpStream) {
    let _ = tcp
        .write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 18\r\nContent-Type: text/plain\r\n\r\nstream unavailable",
        )
        .await;
    let _ = tcp.shutdown().await;
}

async fn serve_stream_connection(
    tcp: tokio::net::TcpStream,
    state: &AppState,
    mut stream_interrupt: watch::Receiver<u64>,
) {
    let session_id = next_stream_session(state);
    if classify_prehandshake_unavailability(state, session_id) {
        reject_stream_handshake(tcp).await;
        return;
    }

    let mut socket = tokio::select! {
        result = accept_async(tcp) => match result {
            Ok(socket) => socket,
            Err(_) => {
                classify_failed_stream_handshake(state, session_id, "stream_handshake_failed");
                return;
            }
        },
        changed = stream_interrupt.changed() => {
            let _ = changed;
            classify_failed_stream_handshake(state, session_id, "stream_unavailable");
            return;
        }
    };

    if !classify_completed_stream_handshake(state, session_id) {
        let _ = socket.close(None).await;
        return;
    }
    let mut session = ActiveStreamSession::new(state.clone(), session_id);
    serve_stream_session(&mut socket, state, &mut stream_interrupt, &mut session).await;
    session.finish();
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictSubscribe<'a> {
    msg_type: &'a str,
    token: &'a str,
    value: &'a str,
    tag: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictUnsubscribe<'a> {
    msg_type: &'a str,
    tag: &'a str,
}

fn exact_subscribe_fields(frame: &str) -> Result<(String, String), &'static str> {
    // Struct deserialization rejects duplicate, missing, unknown, and
    // non-string fields before any authentication or scenario work occurs.
    let wire: StrictSubscribe<'_> =
        serde_json::from_str(frame).map_err(|_| "malformed_subscribe")?;
    if wire.token.is_empty()
        || wire.msg_type != "data:subscribe_oauth"
        || wire.tag != FIXTURE_VID_TAG
    {
        return Err("unexpected_subscribe");
    }
    if wire.value != crate::tesla_stream::TESLAMATE_STREAM_FIELDS.join(",") {
        return Err("unexpected_subscribe_fields");
    }
    Ok((FIXTURE_VID_TAG.to_owned(), wire.value.to_owned()))
}

#[derive(Debug, Deserialize)]
struct FakeRefreshRequest {
    grant_type: String,
    scope: String,
    client_id: String,
    refresh_token: String,
}

/// Deterministic local stand-in for Tesla's legacy refresh endpoint. It
/// validates the request shape and required constants, but never records the
/// submitted refresh token or returns it in a ledger.
async fn token_handler(State(state): State<AppState>, uri: Uri, body: String) -> impl IntoResponse {
    let valid = serde_json::from_str::<FakeRefreshRequest>(&body).is_ok_and(|request| {
        request.grant_type == "refresh_token"
            && request.scope == "openid email offline_access"
            && request.client_id == "ownerapi"
            && !request.refresh_token.trim().is_empty()
    });
    if !valid {
        record(
            &state,
            "POST",
            uri.path(),
            uri.query().unwrap_or_default(),
            StatusCode::BAD_REQUEST,
            true,
            Some("invalid_refresh_request"),
        );
        return (StatusCode::BAD_REQUEST, "invalid refresh request").into_response();
    }

    state
        .inner
        .lock()
        .expect("fake lock")
        .token_refresh_requests += 1;
    record(
        &state,
        "POST",
        uri.path(),
        uri.query().unwrap_or_default(),
        StatusCode::OK,
        false,
        None,
    );
    (
        StatusCode::OK,
        Json(json!({
            "access_token": FAKE_REFRESHED_ACCESS_TOKEN,
            "refresh_token": FAKE_REFRESHED_REFRESH_TOKEN,
            "token_type": "Bearer",
            "expires_in": 3_600,
            "created_at": 1_700_000_000_i64,
        })),
    )
        .into_response()
}

fn exact_unsubscribe_tag(frame: &str) -> Result<String, &'static str> {
    let wire: StrictUnsubscribe<'_> =
        serde_json::from_str(frame).map_err(|_| "malformed_unsubscribe")?;
    if wire.msg_type != "data:unsubscribe" || wire.tag != FIXTURE_VID_TAG {
        return Err("unexpected_unsubscribe");
    }
    Ok(FIXTURE_VID_TAG.to_owned())
}

async fn products_handler(State(state): State<AppState>) -> impl IntoResponse {
    record(
        &state,
        "GET",
        "/api/1/products",
        "",
        StatusCode::OK,
        false,
        None,
    );
    let (step, body) = {
        let mut inner = state.inner.lock().expect("fake lock");
        let step = ScenarioStep::ALL[inner.step_index.min(ScenarioStep::ALL.len() - 1)];
        let body = products_body(step);
        // Auto-advance asleep steps after discovery (no vehicle_data will come).
        let transition = (state.advance_mode == AdvanceMode::AutoOnDiscovery && !step.is_online())
            .then(|| advance_automatically(&mut inner))
            .flatten();
        // Keep the append under `FakeInner`'s lock so simultaneous requests
        // cannot reorder deterministic state transitions in the JSONL ledger.
        if let Some((from, to)) = transition {
            append_scenario_event(
                state.evidence_dir.as_deref(),
                &ScenarioLedgerEvent::AutoTransition { from, to },
            );
        }
        (step, body)
    };
    let _ = step;
    (StatusCode::OK, body).into_response()
}

async fn vehicle_data_handler(
    State(state): State<AppState>,
    AxumPath(vehicle_id): AxumPath<String>,
    uri: Uri,
) -> impl IntoResponse {
    let query = uri.query().unwrap_or_default().to_owned();
    let path = format!("/api/1/vehicles/{vehicle_id}/vehicle_data");
    if vehicle_id != FIXTURE_EID.to_string() {
        record(
            &state,
            "GET",
            &path,
            &query,
            StatusCode::NOT_FOUND,
            true,
            Some("unexpected_vehicle_id"),
        );
        return (StatusCode::NOT_FOUND, "unknown vehicle".to_owned()).into_response();
    }
    if !state
        .inner
        .lock()
        .expect("fake lock")
        .owner_vehicle_data_available
    {
        // This is a controlled server fault, not an unsafe client request.
        // Keep `rejected=false` so assert_no_wake_or_command continues to
        // describe only forbidden/widened caller behaviour.
        record(
            &state,
            "GET",
            &path,
            &query,
            StatusCode::SERVICE_UNAVAILABLE,
            false,
            Some("owner_vehicle_data_unavailable"),
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "owner vehicle_data unavailable".to_owned(),
        )
            .into_response();
    }
    let step = {
        let inner = state.inner.lock().expect("fake lock");
        ScenarioStep::ALL[inner.step_index.min(ScenarioStep::ALL.len() - 1)]
    };
    if !step.is_online() {
        record(
            &state,
            "GET",
            &path,
            &query,
            StatusCode::REQUEST_TIMEOUT,
            true,
            Some("vehicle_data_while_asleep"),
        );
        return (StatusCode::REQUEST_TIMEOUT, "vehicle unavailable").into_response();
    }
    record(&state, "GET", &path, &query, StatusCode::OK, false, None);
    let body = {
        let mut inner = state.inner.lock().expect("fake lock");
        let step = ScenarioStep::ALL[inner.step_index.min(ScenarioStep::ALL.len() - 1)];
        let timestamp = vehicle_data_timestamp_ms(step, inner.substep, inner.base_ts_ms);
        let body = vehicle_data_body(
            step,
            inner.substep,
            inner.base_ts_ms,
            inner.odometer_offset_miles,
        );
        inner.last_owner_data_ts_ms = Some(timestamp);
        inner.last_owner_odometer_miles = Some(owner_odometer_miles(
            step,
            inner.substep,
            inner.odometer_offset_miles,
        ));
        let budget = vehicle_data_budget(step);
        inner.substep = inner.substep.saturating_add(1);
        let transition = (state.advance_mode == AdvanceMode::AutoOnDiscovery
            && inner.substep >= budget)
            .then(|| advance_automatically(&mut inner))
            .flatten();
        // See products_handler: this is intentionally serialized with the
        // state advance rather than appended after releasing the state lock.
        if let Some((from, to)) = transition {
            append_scenario_event(
                state.evidence_dir.as_deref(),
                &ScenarioLedgerEvent::AutoTransition { from, to },
            );
        }
        body
    };
    (StatusCode::OK, body).into_response()
}

async fn reject_handler(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
) -> impl IntoResponse {
    let path = uri.path().to_owned();
    let query = uri.query().unwrap_or_default().to_owned();
    let reason = if path.to_ascii_lowercase().contains("wake") {
        "wake_route"
    } else if path.to_ascii_lowercase().contains("command") {
        "command_route"
    } else {
        "unexpected_route"
    };
    record(
        &state,
        method.as_str(),
        &path,
        &query,
        StatusCode::FORBIDDEN,
        true,
        Some(reason),
    );
    (StatusCode::FORBIDDEN, format!("rejected:{reason}")).into_response()
}

fn products_body(step: ScenarioStep) -> String {
    json!({
        "response": [{
            "id": FIXTURE_EID,
            "vehicle_id": FIXTURE_VID,
            "vin": FIXTURE_VIN,
            "state": step.discovery_state(),
            "display_name": FIXTURE_DISPLAY_NAME,
            "in_service": false,
        }],
        "count": 1
    })
    .to_string()
}

fn vehicle_data_budget(step: ScenarioStep) -> usize {
    match step {
        ScenarioStep::DrivePositions => 4,
        ScenarioStep::ChargeSamples => 3,
        ScenarioStep::OnlineIdle
        | ScenarioStep::Parked
        | ScenarioStep::OnlineSoftwareUpdate
        | ScenarioStep::UnchangedNoOp => 1,
        ScenarioStep::AsleepDiscovery
        | ScenarioStep::OfflineDiscovery
        | ScenarioStep::AsleepRestart => 0,
    }
}

/// Offset each top-level scenario step so timestamps advance across the journey
/// even when a step only exposes a single vehicle_data substep.
fn step_offset_ms(step: ScenarioStep) -> i64 {
    let step_ms = 60_000_i64;
    match step {
        ScenarioStep::AsleepDiscovery => 0,
        ScenarioStep::OfflineDiscovery => step_ms,
        ScenarioStep::OnlineIdle => 2 * step_ms,
        ScenarioStep::DrivePositions => 3 * step_ms,
        ScenarioStep::Parked => 7 * step_ms,
        ScenarioStep::ChargeSamples => 8 * step_ms,
        ScenarioStep::AsleepRestart => 11 * step_ms,
        ScenarioStep::OnlineSoftwareUpdate => 12 * step_ms,
        ScenarioStep::UnchangedNoOp => 13 * step_ms,
    }
}

fn vehicle_data_timestamp_ms(step: ScenarioStep, substep: usize, base_ts_ms: i64) -> i64 {
    // 60s spacing keeps lifecycle position/sample ordering and drive duration
    // realistic while remaining fully deterministic.
    let step_ms = 60_000_i64;
    match step {
        // Unchanged no-op must re-emit an identical payload so re-collection is
        // acknowledged as already-present rather than a new observation.
        ScenarioStep::UnchangedNoOp => base_ts_ms.saturating_add(step_offset_ms(step)),
        _ => base_ts_ms
            .saturating_add(
                i64::try_from(substep)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(step_ms),
            )
            .saturating_add(step_offset_ms(step)),
    }
}

fn stream_telemetry_timestamps(
    last_owner_data_ts_ms: Option<i64>,
    next_owner_data_ts_ms: i64,
    base_ts_ms: i64,
) -> Vec<i64> {
    let lower = last_owner_data_ts_ms.unwrap_or(base_ts_ms);
    let gap = i128::from(next_owner_data_ts_ms) - i128::from(lower);
    if gap <= 8 {
        return Vec::new();
    }
    (1_i128..=8)
        .map(|ordinal| {
            let timestamp = i128::from(lower) + gap * ordinal / 9;
            i64::try_from(timestamp).expect("interpolated stream timestamp fits i64")
        })
        .collect()
}

fn fixture_odometer_miles(base_miles: f64, offset_miles: f64) -> f64 {
    base_miles + offset_miles
}

/// The Owner payload uses the same odometer in both `drive_state` and
/// `vehicle_state`. Keep this fixture truth in one place so stream samples can
/// be interpolated between adjacent Owner observations.
fn owner_odometer_miles(step: ScenarioStep, substep: usize, offset_miles: f64) -> f64 {
    let base_miles = match step {
        ScenarioStep::DrivePositions => 12_345.0 + substep as f64 * 0.1,
        ScenarioStep::Parked => 12_345.4,
        ScenarioStep::ChargeSamples
        | ScenarioStep::OnlineSoftwareUpdate
        | ScenarioStep::UnchangedNoOp => 12_346.0,
        ScenarioStep::AsleepDiscovery
        | ScenarioStep::OfflineDiscovery
        | ScenarioStep::OnlineIdle
        | ScenarioStep::AsleepRestart => 12_345.0 + substep as f64,
    };
    fixture_odometer_miles(base_miles, offset_miles)
}

/// Return one interior, monotonic stream sample between exact Owner odometer
/// endpoints. Equal endpoints intentionally yield a stable stream value.
fn stream_odometer_miles(
    previous_owner_miles: f64,
    next_owner_miles: f64,
    index: usize,
    sample_count: usize,
) -> f64 {
    debug_assert!(sample_count > 0);
    debug_assert!(index < sample_count);
    previous_owner_miles
        + (next_owner_miles - previous_owner_miles) * (index + 1) as f64 / (sample_count + 1) as f64
}

fn vehicle_data_body(
    step: ScenarioStep,
    substep: usize,
    base_ts_ms: i64,
    odometer_offset_miles: f64,
) -> String {
    let ts = vehicle_data_timestamp_ms(step, substep, base_ts_ms);
    let odometer = owner_odometer_miles(step, substep, odometer_offset_miles);
    let mut drive = json!({
        "timestamp": ts,
        "shift_state": "P",
        "speed": 0,
        "power": 0,
        "latitude": 47.4979,
        "longitude": 19.0402,
        "odometer": odometer,
        "heading": 90
    });
    let mut charge = json!({
        "timestamp": ts,
        "charging_state": "Disconnected",
        "battery_level": 64,
        "charge_energy_added": 0.0,
        "charger_power": 0.0,
        "battery_range": 210.0,
        "ideal_battery_range": 220.0,
        "est_battery_range": 200.0
    });
    let mut vehicle_state = json!({
        "timestamp": ts,
        "car_version": "2026.20.1",
        "vehicle_name": FIXTURE_DISPLAY_NAME,
        "locked": true,
        "odometer": odometer,
        "software_update": {"status": ""}
    });

    match step {
        ScenarioStep::DrivePositions => {
            drive = json!({
                "timestamp": ts,
                "shift_state": "D",
                "speed": 20 + substep as i64 * 2,
                "power": 15 + substep as i64,
                "latitude": 47.4979 + (substep as f64) * 0.001,
                "longitude": 19.0402 + (substep as f64) * 0.001,
                "odometer": odometer,
                "heading": 90
            });
            vehicle_state = json!({
                "timestamp": ts,
                "car_version": "2026.20.1",
                "vehicle_name": FIXTURE_DISPLAY_NAME,
                "locked": true,
                "odometer": odometer,
                "software_update": {"status": ""}
            });
        }
        ScenarioStep::Parked => {
            // closed drive: parked at final drive odometer/coords
            drive = json!({
                "timestamp": ts,
                "shift_state": "P",
                "speed": 0,
                "power": 0,
                "latitude": 47.5019,
                "longitude": 19.0442,
                "odometer": odometer,
                "heading": 90
            });
            vehicle_state = json!({
                "timestamp": ts,
                "car_version": "2026.20.1",
                "vehicle_name": FIXTURE_DISPLAY_NAME,
                "locked": true,
                "odometer": odometer,
                "software_update": {"status": ""}
            });
        }
        ScenarioStep::ChargeSamples => {
            // Final substep is terminal Complete so the charge closes on the
            // production lifecycle path (not only via later asleep).
            let terminal = substep + 1 >= vehicle_data_budget(ScenarioStep::ChargeSamples);
            let level = 64 + substep as i64;
            let energy = 1.5 + substep as f64;
            charge = json!({
                "timestamp": ts,
                "charging_state": if terminal { "Complete" } else { "Charging" },
                "battery_level": if terminal { 80 } else { level },
                "charge_energy_added": if terminal { 12.0 } else { energy },
                "charger_power": if terminal { 0.0 } else { 11.0 },
                "battery_range": if terminal { 220.0 } else { 210.0 + substep as f64 },
                "ideal_battery_range": 220.0,
                "est_battery_range": 200.0,
                "charge_limit_soc": 80,
                "charger_voltage": 230,
                "charger_actual_current": if terminal { 0 } else { 16 }
            });
            drive = json!({
                "timestamp": ts,
                "shift_state": "P",
                "speed": 0,
                "power": if terminal { 0 } else { -11 },
                "latitude": 47.5,
                "longitude": 19.05,
                "odometer": odometer,
                "heading": 90
            });
            vehicle_state = json!({
                "timestamp": ts,
                "car_version": "2026.20.1",
                "vehicle_name": FIXTURE_DISPLAY_NAME,
                "locked": true,
                "odometer": odometer,
                "software_update": {"status": ""}
            });
        }
        ScenarioStep::OnlineSoftwareUpdate => {
            vehicle_state = json!({
                "timestamp": ts,
                "car_version": "2026.20.1",
                "vehicle_name": FIXTURE_DISPLAY_NAME,
                "locked": true,
                "odometer": odometer,
                "software_update": {
                    "status": "installing",
                    "version": "2026.32.1",
                    "expected_duration_sec": 1800
                }
            });
        }
        ScenarioStep::UnchangedNoOp => {
            // Post-update online idle with stable identity (identical on re-poll).
            vehicle_state = json!({
                "timestamp": ts,
                "car_version": "2026.32.1",
                "vehicle_name": FIXTURE_DISPLAY_NAME,
                "locked": true,
                "odometer": odometer,
                "software_update": {"status": ""}
            });
            drive = json!({
                "timestamp": ts,
                "shift_state": "P",
                "speed": 0,
                "power": 0,
                "latitude": 47.5,
                "longitude": 19.05,
                "odometer": odometer,
                "heading": 90
            });
            charge = json!({
                "timestamp": ts,
                "charging_state": "Disconnected",
                "battery_level": 80,
                "charge_energy_added": 0.0,
                "charger_power": 0.0,
                "battery_range": 220.0,
                "ideal_battery_range": 220.0,
                "est_battery_range": 210.0
            });
        }
        ScenarioStep::OnlineIdle => {}
        ScenarioStep::AsleepDiscovery
        | ScenarioStep::OfflineDiscovery
        | ScenarioStep::AsleepRestart => {}
    }

    json!({
        "response": {
            "id": FIXTURE_EID,
            "vehicle_id": FIXTURE_VID,
            "vin": FIXTURE_VIN,
            "display_name": FIXTURE_DISPLAY_NAME,
            "state": "online",
            "drive_state": drive,
            "charge_state": charge,
            "vehicle_state": vehicle_state,
            "vehicle_config": {
                "car_type": "model3",
                "trim_badging": "74d",
                "exterior_color": "SolidBlack"
            },
            "climate_state": {
                "timestamp": ts,
                "is_preconditioning": false,
                "climate_keeper_mode": "off"
            },
            "gui_settings": {
                "gui_distance_units": "km/hr",
                "gui_temperature_units": "C",
                "gui_charge_rate_units": "kW"
            }
        }
    })
    .to_string()
}

async fn serve_stream_session(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    state: &AppState,
    stream_interrupt: &mut watch::Receiver<u64>,
    session: &mut ActiveStreamSession,
) {
    let session_id = session.session_id;

    // A stream session is useful proof only when it starts with the exact
    // TeslaMate subscription contract for the fixture vehicle.  Do not send a
    // hello or telemetry for a malformed, wrong-tag, or widened field set.
    let subscribe = tokio::select! {
        received = socket.next() => received,
        changed = stream_interrupt.changed() => {
            let _ = changed;
            session.set_disconnect_reason("stream_unavailable");
            let _ = socket.close(None).await;
            return;
        }
    };
    let Some(Ok(Message::Text(subscribe))) = subscribe else {
        session.set_disconnect_reason("missing_subscribe");
        return;
    };
    let (tag, fields) = match exact_subscribe_fields(&subscribe) {
        Ok(values) => values,
        Err(reason) => {
            record_stream_event(
                state,
                session_id,
                StreamAuditEvent::Rejected,
                None,
                None,
                false,
                Some(reason),
            );
            session.set_disconnect_reason(reason);
            let _ = socket.close(None).await;
            return;
        }
    };
    record_stream_event(
        state,
        session_id,
        StreamAuditEvent::Subscribe,
        Some(tag.clone()),
        Some(fields),
        true,
        None,
    );
    session.accept_subscription(tag.clone());
    let hello_sent = tokio::select! {
        result = socket.send(Message::Text(r#"{"msg_type":"control:hello","connection_timeout":0}"#.into())) => result.is_ok(),
        changed = stream_interrupt.changed() => {
            let _ = changed;
            session.set_disconnect_reason("stream_unavailable");
            let _ = socket.close(None).await;
            return;
        }
    };
    if !hello_sent {
        session.set_disconnect_reason("stream_send_failed");
        return;
    }
    record_stream_event(
        state,
        session_id,
        StreamAuditEvent::Hello,
        Some(tag.clone()),
        None,
        true,
        None,
    );

    // Emit enough telemetry with numeric power so pre-online confirmation
    // and drive/charge streaming succeed. Place the frames between the last
    // returned Owner API sample and the next deterministic sample. This keeps
    // reconnects newer than the durable stream watermark without letting
    // stream telemetry overtake the next Owner API observation.
    let (step, timestamps, previous_owner_odometer_miles, next_owner_odometer_miles) = {
        let mut inner = state.inner.lock().expect("fake lock");
        let step = ScenarioStep::ALL[inner.step_index.min(ScenarioStep::ALL.len() - 1)];
        let next_owner = vehicle_data_timestamp_ms(step, inner.substep, inner.base_ts_ms);
        let fresh =
            stream_telemetry_timestamps(inner.last_owner_data_ts_ms, next_owner, inner.base_ts_ms);
        let timestamps = if fresh.is_empty() {
            inner.last_stream_timestamps.clone()
        } else {
            inner.last_stream_timestamps.clone_from(&fresh);
            fresh
        };
        let next_owner_odometer_miles =
            owner_odometer_miles(step, inner.substep, inner.odometer_offset_miles);
        let previous_owner_odometer_miles = inner
            .last_owner_odometer_miles
            // Before the first Owner sample there is no measured interval. A
            // stable value is the only truthful interpolation.
            .unwrap_or(next_owner_odometer_miles);
        (
            step,
            timestamps,
            previous_owner_odometer_miles,
            next_owner_odometer_miles,
        )
    };
    let stream_sample_count = timestamps.len();
    for (i, ts) in timestamps.into_iter().enumerate() {
        let (speed, power, shift, lat, lng, soc) = match step {
            ScenarioStep::DrivePositions => (
                20 + i as i64,
                15 + i as i64,
                "D",
                47.4979 + i as f64 * 0.001,
                19.0402 + i as f64 * 0.001,
                64,
            ),
            ScenarioStep::ChargeSamples => (0, -11, "P", 47.5, 19.05, 64 + i as i64),
            _ => (0, 1, "P", 47.4979, 19.0402, 64),
        };
        // TeslaMate stream field order:
        // speed,odometer,soc,elevation,est_heading,est_lat,est_lng,power,shift_state,range,est_range,heading
        let value = format!(
            "{speed},{},{soc},100,90,{lat},{lng},{power},{shift},220,210,90",
            stream_odometer_miles(
                previous_owner_odometer_miles,
                next_owner_odometer_miles,
                i,
                stream_sample_count,
            ),
        );
        let frame = format!(
            r#"{{"msg_type":"data:update","tag":"{tag}","timestamp":{ts},"value":"{value}"}}"#,
            tag = FIXTURE_VID,
            ts = ts,
            value = value
        );
        let sent = tokio::select! {
            result = socket.send(Message::Text(frame.into())) => result.is_ok(),
            changed = stream_interrupt.changed() => {
                let _ = changed;
                session.set_disconnect_reason("stream_unavailable");
                let _ = socket.close(None).await;
                return;
            }
        };
        if !sent {
            session.set_disconnect_reason("stream_send_failed");
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(5)) => {}
            changed = stream_interrupt.changed() => {
                let _ = changed;
                session.set_disconnect_reason("stream_unavailable");
                let _ = socket.close(None).await;
                return;
            }
        }
    }

    // Drain until a validated unsubscribe or disconnect.  A process restart
    // may disconnect an in-flight subscription; that is retained distinctly
    // from an orderly unsubscribe so the journey can prove both behaviours.
    loop {
        let message = tokio::select! {
            received = socket.next() => received,
            changed = stream_interrupt.changed() => {
                let _ = changed;
                session.set_disconnect_reason("stream_unavailable");
                let _ = socket.close(None).await;
                return;
            }
        };
        let Some(msg) = message else {
            return;
        };
        match msg {
            Ok(Message::Text(text)) => match exact_unsubscribe_tag(&text) {
                Ok(unsubscribe_tag) => {
                    record_stream_event(
                        state,
                        session_id,
                        StreamAuditEvent::Unsubscribe,
                        Some(unsubscribe_tag),
                        None,
                        true,
                        None,
                    );
                    return;
                }
                Err(reason) => {
                    record_stream_event(
                        state,
                        session_id,
                        StreamAuditEvent::Rejected,
                        Some(tag),
                        None,
                        false,
                        Some(reason),
                    );
                    session.set_disconnect_reason(reason);
                    let _ = socket.close(None).await;
                    return;
                }
            },
            Ok(Message::Close(_)) | Err(_) => return,
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "fake_tesla/tests.rs"]
mod tests;
