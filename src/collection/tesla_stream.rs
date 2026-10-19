// SPDX-License-Identifier: AGPL-3.0-only

use std::{sync::Arc, time::Duration};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    sync::{Mutex, mpsc},
    time::Instant,
};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::{
    credentials::LegacyAuthManager,
    db::{HubStore, StoreError},
    owner_api::{StreamVehicleId, VehicleId},
};

mod echo;
mod liveness;
mod policy;
mod session;
mod wire;

pub(crate) use policy::StreamPowerGate;
use policy::{JitterSource, OsJitter, SystemWallClock, WallClock};
#[cfg(test)]
pub(crate) use session::AdmissionEvent;
pub use wire::{
    STREAM_COLUMNS, StreamUpdate, parse_data_update, subscribe_frame, unsubscribe_frame,
    validate_endpoint_override,
};

/// Tesla's global streaming host (R1): the endpoint of a supervisor whose
/// manager reports the global region (C5 decides the region, C3 builds the
/// endpoint). A wire fact of the brief.
pub const GLOBAL_STREAM_ENDPOINT: &str = "wss://streaming.vn.teslamotors.com/streaming/";
/// Tesla's China streaming host (R1), used the same way for the China region.
/// The unit never switches between the two hosts, whatever fails (R2).
pub const CHINA_STREAM_ENDPOINT: &str = "wss://streaming.vn.cloud.tesla.cn/streaming/";

/// Largest message, frame and write buffer of a stream socket, in bytes (R33).
/// A legitimate frame is a few hundred bytes (12 short values and a small
/// envelope), so 64 KiB is wide headroom while bounding memory per socket
/// (brief invariant: bounded buffers). The read buffer takes the same size.
const K_MAX_MESSAGE: usize = 64 * 1024;

/// Size at which the socket flushes its write buffer, in bytes. The library
/// requires it to be smaller than the write-buffer limit ([`K_MAX_MESSAGE`]);
/// the Hub's own frames are at most a subscribe frame of about 8.3 KiB (R8),
/// and 16 KiB keeps every one of them in a single flush.
const WRITE_FLUSH_BYTES: usize = 16 * 1024;

/// Longest wait for the server's Close frame after the Hub's own (R10). 2 s is
/// many round trips on any working link, and together with the 2 s unsubscribe
/// write it keeps every close under 5 s, the shutdown budget (R11).
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// The health timeout a supervisor uses until C3 passes the loaded one (IF a2):
/// C3's default for `stream_health_timeout_seconds` (C3 R21), and the lowest
/// value of the moving data-silence limit (R30).
const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamRegion {
    Global,
    China,
}

/// The streaming host of a region (R1): `Global` gives
/// [`GLOBAL_STREAM_ENDPOINT`] and `China` gives [`CHINA_STREAM_ENDPOINT`]. The
/// region itself is decided by C5, never here.
pub fn streaming_endpoint(region: StreamRegion) -> &'static str {
    match region {
        StreamRegion::Global => GLOBAL_STREAM_ENDPOINT,
        StreamRegion::China => CHINA_STREAM_ENDPOINT,
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Healthy,
    Telemetry {
        update: Box<StreamUpdate>,
        queued_at: Instant,
    },
    VehicleOffline,
    AuthRejected,
    TransportUnavailable,
    ProtocolViolation,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StreamError {
    #[error("invalid endpoint")]
    InvalidEndpoint,
    #[error("invalid tag")]
    InvalidTag,
    #[error("invalid token")]
    InvalidToken,
    #[error("frame serialization failed")]
    FrameSerialization,
    #[error("wrong message type")]
    WrongMessageType,
    #[error("malformed control hello")]
    MalformedControlHello,
    #[error("malformed data update")]
    MalformedDataUpdate,
    #[error("invalid timestamp")]
    InvalidTimestamp,
    #[error("invalid coordinates")]
    InvalidCoordinates,
    #[error("invalid field")]
    InvalidField,
}

#[derive(Debug, Error)]
pub enum StreamSupervisorError {
    #[error("stream endpoint is invalid")]
    InvalidEndpoint(#[from] StreamError),
    #[error("stream tag is invalid")]
    InvalidTag,
    #[error("stream could not complete orderly unsubscribe shutdown")]
    OrderlyShutdownUnavailable,
    #[error("stream event queue is full")]
    EventQueueFull,
    #[error("stream event receiver is closed")]
    EventReceiverClosed,
    #[error("stream credential authority is unavailable")]
    CredentialAuthorityUnavailable,
    #[error("stream peer violated the bounded wire protocol")]
    ProtocolViolation,
    #[error("stream audit failed")]
    Audit(#[from] StoreError),
}

/// The run's write guard as the stream sees it: it answers `true` once a store
/// write is refused, for good.
pub(crate) type WriteRefusal = Arc<dyn Fn() -> bool + Send + Sync>;

/// Crate-local stream authority. It is constructed only from the collector's
/// admitted credential capability, never directly by library consumers.
pub(crate) struct TeslaStreamSupervisor {
    vehicle_id: VehicleId,
    tag: String,
    credential: StreamCredential,
    /// Used only in logs (R1, R52).
    region: StreamRegion,
    client: Option<Client>,
    endpoint: String,
    /// The endpoint's host: the only part of it that may be logged. An override
    /// URL may carry a provider key in its path or query (R4, R52).
    endpoint_host: String,
    events: mpsc::Sender<StreamEvent>,
    power_gate: Option<Arc<StreamPowerGate>>,
    audit_store: Option<HubStore>,
    /// The run's write guard (B2 fix round, P3b fix 2, R-P3bF2-1): `true` once the
    /// audit rows may no longer be written. The audit's session and request receipts
    /// are store mutations like any other of a supervised run.
    write_guard: Option<WriteRefusal>,
    /// `stream_health_timeout_seconds` as C3 loaded it: the basis of the moving
    /// data-silence limit (R30).
    health_timeout: Duration,
    wall_clock: Arc<dyn WallClock>,
    jitter: std::sync::Mutex<Box<dyn JitterSource>>,
    seams: session::TestSeams,
}

#[derive(Clone)]
enum StreamCredential {
    Legacy(Arc<Mutex<LegacyAuthManager>>),
}

impl TeslaStreamSupervisor {
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn new_legacy_auth(
        vehicle_id: VehicleId,
        stream_vehicle_id: StreamVehicleId,
        manager: Arc<Mutex<LegacyAuthManager>>,
        region: StreamRegion,
        endpoint: String,
        client: Client,
        events: mpsc::Sender<StreamEvent>,
        store: HubStore,
    ) -> Result<Self, StreamSupervisorError> {
        Self::new_legacy_auth_with_endpoint_policy(
            vehicle_id,
            stream_vehicle_id,
            manager,
            region,
            endpoint,
            client,
            events,
            false,
            Some(store),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_legacy_auth_for_test(
        vehicle_id: VehicleId,
        stream_vehicle_id: StreamVehicleId,
        manager: Arc<Mutex<LegacyAuthManager>>,
        region: StreamRegion,
        endpoint: String,
        client: Client,
        events: mpsc::Sender<StreamEvent>,
    ) -> Result<Self, StreamSupervisorError> {
        Self::new_legacy_auth_with_endpoint_policy(
            vehicle_id,
            stream_vehicle_id,
            manager,
            region,
            endpoint,
            client,
            events,
            true,
            None,
        )
    }

    #[cfg(test)]
    fn new_legacy_auth_for_test_production_policy(
        vehicle_id: VehicleId,
        stream_vehicle_id: StreamVehicleId,
        manager: Arc<Mutex<LegacyAuthManager>>,
        region: StreamRegion,
        endpoint: String,
        client: Client,
        events: mpsc::Sender<StreamEvent>,
    ) -> Result<Self, StreamSupervisorError> {
        Self::new_legacy_auth_with_endpoint_policy(
            vehicle_id,
            stream_vehicle_id,
            manager,
            region,
            endpoint,
            client,
            events,
            false,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_legacy_auth_with_endpoint_policy(
        vehicle_id: VehicleId,
        stream_vehicle_id: StreamVehicleId,
        manager: Arc<Mutex<LegacyAuthManager>>,
        region: StreamRegion,
        endpoint: String,
        client: Client,
        events: mpsc::Sender<StreamEvent>,
        allow_loopback_plaintext: bool,
        audit_store: Option<HubStore>,
    ) -> Result<Self, StreamSupervisorError> {
        // R1: the endpoint is the string the supervisor is constructed with, checked
        // by R3. The region is not used to choose or rewrite it.
        #[cfg(test)]
        if allow_loopback_plaintext {
            wire::validate_endpoint_for_test_constructor(&endpoint)?;
        } else {
            validate_endpoint_override(&endpoint)?;
        }
        #[cfg(not(test))]
        {
            let _ = allow_loopback_plaintext;
            validate_endpoint_override(&endpoint)?;
        }
        let tag = stream_vehicle_id.to_string();
        wire::validate_tag(&tag).map_err(|_| StreamSupervisorError::InvalidTag)?;
        // The endpoint passed R3, so it parses; an empty host would only mean a
        // log line without one.
        let endpoint_host = url::Url::parse(&endpoint)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        Ok(Self {
            vehicle_id,
            tag,
            credential: StreamCredential::Legacy(manager),
            region,
            client: Some(client),
            endpoint,
            endpoint_host,
            events,
            power_gate: None,
            audit_store,
            write_guard: None,
            health_timeout: DEFAULT_HEALTH_TIMEOUT,
            wall_clock: Arc::new(SystemWallClock),
            jitter: std::sync::Mutex::new(Box::new(OsJitter)),
            seams: session::TestSeams::default(),
        })
    }

    /// Sets the configured health timeout C3 loaded (IF a2), the basis of the
    /// moving data-silence limit (R30).
    pub(crate) fn with_health_timeout(mut self, timeout: Duration) -> Self {
        self.health_timeout = timeout;
        self
    }

    /// Hand the run's write guard to the stream's audit: no session or request
    /// receipt is written (nor completed) once it refuses.
    pub(crate) fn with_write_guard(mut self, guard: WriteRefusal) -> Self {
        self.write_guard = Some(guard);
        self
    }

    pub(crate) fn with_power_gate(mut self, gate: Arc<StreamPowerGate>) -> Self {
        #[cfg(test)]
        gate.set_clock(Arc::clone(&self.wall_clock));
        self.power_gate = Some(gate);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_audit_store(mut self, store: HubStore) -> Self {
        self.audit_store = Some(store);
        self
    }

    /// Test seam (1.2): the wall clock behind R13 (HTTP-date), R20a, R50 and
    /// R58. It also sets the clock of the attached power gate.
    #[cfg(test)]
    pub(crate) fn with_wall_clock(mut self, clock: Arc<dyn WallClock>) -> Self {
        if let Some(gate) = &self.power_gate {
            gate.set_clock(Arc::clone(&clock));
        }
        self.wall_clock = clock;
        self
    }

    /// Test seam (1.2): the jitter draws.
    #[cfg(test)]
    pub(crate) fn with_jitter(mut self, jitter: Box<dyn JitterSource>) -> Self {
        self.jitter = std::sync::Mutex::new(jitter);
        self
    }

    /// Test seam (1.2): called at the points of the admission steps, so a test
    /// counts calls and orders them against peer events.
    #[cfg(test)]
    pub(crate) fn with_admission_hook(
        mut self,
        hook: Arc<dyn Fn(AdmissionEvent) + Send + Sync>,
    ) -> Self {
        self.seams.hook = Some(hook);
        self
    }

    /// Test seam: how the transport of each connection is made, so a test runs
    /// the session over an in-memory pipe (see `session::StreamIo`).
    #[cfg(test)]
    pub(crate) fn with_connector(mut self, connector: session::Connector) -> Self {
        self.seams.connector = Some(connector);
        self
    }

    /// Test seam: every ended session's frame counts (R52) are also pushed here.
    #[cfg(test)]
    pub(crate) fn with_summary_sink(
        mut self,
        sink: Arc<std::sync::Mutex<Vec<session::FrameCounts>>>,
    ) -> Self {
        self.seams.summaries = Some(sink);
        self
    }

    /// Test seam: stands in for a manager whose token read fails with a terminal
    /// credential error (R6a, R57 b) while the flag is set. C5's terminal states
    /// do not exist in the tree yet; a test that needs one re-runs against C5's
    /// manager when it lands.
    #[cfg(test)]
    pub(crate) fn with_terminal_credential(
        mut self,
        flag: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.seams.terminal = Some(flag);
        self
    }
}

fn stream_socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(K_MAX_MESSAGE)
        .write_buffer_size(WRITE_FLUSH_BYTES)
        .max_write_buffer_size(K_MAX_MESSAGE)
        .max_message_size(Some(K_MAX_MESSAGE))
        .max_frame_size(Some(K_MAX_MESSAGE))
}

#[cfg(test)]
mod read_loop_tests;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod test_peer;
#[cfg(test)]
#[path = "tesla_stream/tests.rs"]
mod tests;
#[cfg(test)]
mod write_guard_tests;
