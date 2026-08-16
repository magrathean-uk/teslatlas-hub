use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    time::{sleep, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;

use crate::{
    credentials::{LegacyAuthManager, LegacyAuthManagerError},
    owner_api::{StreamVehicleId, VehicleId},
};

pub const GLOBAL_STREAM_ENDPOINT: &str = "wss://streaming.vn.teslamotors.com/streaming/";
pub const CHINA_STREAM_ENDPOINT: &str = "wss://streaming.vn.cloud.tesla.cn/streaming/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SILENCE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
struct SupervisorPolicy {
    connect_timeout: Duration,
    silence_timeout: Duration,
    backoff_initial: Duration,
    remote_backoff_cap: Duration,
    connect_backoff_cap: Duration,
}

const DEFAULT_POLICY: SupervisorPolicy = SupervisorPolicy {
    connect_timeout: CONNECT_TIMEOUT,
    silence_timeout: SILENCE_TIMEOUT,
    backoff_initial: Duration::from_secs(1),
    remote_backoff_cap: Duration::from_secs(10),
    connect_backoff_cap: Duration::from_secs(30),
};

pub const TESLAMATE_STREAM_FIELDS: &[&str] = &[
    "speed",
    "odometer",
    "soc",
    "elevation",
    "est_heading",
    "est_lat",
    "est_lng",
    "power",
    "shift_state",
    "range",
    "est_range",
    "heading",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamRegion {
    Global,
    China,
}

pub fn streaming_endpoint(region: StreamRegion) -> &'static str {
    match region {
        StreamRegion::Global => GLOBAL_STREAM_ENDPOINT,
        StreamRegion::China => CHINA_STREAM_ENDPOINT,
    }
}

pub fn validate_endpoint_override(value: &str) -> Result<(), StreamError> {
    let url = Url::parse(value).map_err(|_| StreamError::InvalidEndpoint)?;
    if !matches!(url.scheme(), "ws" | "wss")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() == "ws"
            && !url
                .host_str()
                .is_some_and(|host| host == "localhost" || host == "127.0.0.1" || host == "::1"))
    {
        return Err(StreamError::InvalidEndpoint);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Healthy,
    Telemetry(Box<StreamUpdate>),
    VehicleOffline,
    AuthRejected,
    TransportUnavailable,
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
}

/// Live, stream-owned prerequisite for a potentially waking `vehicle_data`
/// read. It is updated before the corresponding event is queued, so collector
/// queue latency cannot preserve stale numeric-power authority.
#[derive(Default)]
pub(crate) struct StreamPowerGate {
    confirmed: AtomicBool,
}

impl StreamPowerGate {
    pub(crate) fn is_confirmed(&self) -> bool {
        self.confirmed.load(Ordering::Acquire)
    }

    fn observe(&self, event: &StreamEvent) {
        match event {
            StreamEvent::Telemetry(update) => self
                .confirmed
                .store(update.power.is_some(), Ordering::Release),
            StreamEvent::VehicleOffline
            | StreamEvent::AuthRejected
            | StreamEvent::TransportUnavailable => self.revoke(),
            StreamEvent::Healthy => {}
        }
    }

    fn revoke(&self) {
        self.confirmed.store(false, Ordering::Release);
    }
}

/// Durable, run-scoped capability for outbound streaming operations. It owns a
/// clone of the Hub store so a spawned supervisor never borrows its collector.
/// Synchronous cancellation fence for one stream supervisor lifetime. Tokio
/// task abort drops this guard, so a started session cannot remain open merely
/// because async teardown did not run.
#[derive(Clone)]
struct Backoff {
    initial: Duration,
    current: Duration,
    cap: Duration,
}

impl Backoff {
    fn new(initial: Duration, cap: Duration) -> Self {
        Self {
            initial,
            current: initial,
            cap,
        }
    }
    fn next(&mut self) -> Duration {
        let delay = self.current;
        self.current = self.current.saturating_mul(2).min(self.cap);
        delay
    }
    fn reset(&mut self) {
        self.current = self.initial;
    }
}

/// Crate-local stream authority. It is constructed only from the collector's
/// admitted credential capability, never directly by library consumers.
pub(crate) struct TeslaStreamSupervisor {
    vehicle_id: VehicleId,
    tag: String,
    credential: StreamCredential,
    client: Option<Client>,
    endpoint: String,
    events: mpsc::Sender<StreamEvent>,
    policy: SupervisorPolicy,
    power_gate: Option<Arc<StreamPowerGate>>,
}

#[derive(Clone)]
enum StreamCredential {
    Legacy(Arc<Mutex<LegacyAuthManager>>),
}

enum StreamRunTermination {
    Orderly,
    CancelledBeforeSubscription,
    TransportEnded,
}

impl TeslaStreamSupervisor {
    pub(crate) fn new_legacy_auth(
        vehicle_id: VehicleId,
        stream_vehicle_id: StreamVehicleId,
        manager: Arc<Mutex<LegacyAuthManager>>,
        region: StreamRegion,
        endpoint: String,
        client: Client,
        events: mpsc::Sender<StreamEvent>,
    ) -> Result<Self, StreamSupervisorError> {
        let endpoint = endpoint_for_region(region, endpoint)?;
        let tag = stream_vehicle_id.to_string();
        if tag.is_empty() {
            return Err(StreamSupervisorError::InvalidTag);
        }
        Ok(Self {
            vehicle_id,
            tag,
            credential: StreamCredential::Legacy(manager),
            client: Some(client),
            endpoint,
            events,
            policy: DEFAULT_POLICY,
            power_gate: None,
        })
    }
    #[cfg(test)]
    fn with_policy(mut self, policy: SupervisorPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub(crate) fn with_health_timeout(mut self, timeout: Duration) -> Self {
        if !timeout.is_zero() {
            self.policy.silence_timeout = timeout;
        }
        self
    }

    pub(crate) fn with_power_gate(mut self, gate: Arc<StreamPowerGate>) -> Self {
        self.power_gate = Some(gate);
        self
    }

    pub(crate) async fn run(
        self,
        mut shutdown: oneshot::Receiver<()>,
    ) -> Result<(), StreamSupervisorError> {
        let result = self.run_until_termination(&mut shutdown).await;
        if let Some(gate) = self.power_gate.as_ref() {
            gate.revoke();
        }
        result.map(|_| ())
    }

    async fn run_until_termination(
        &self,
        shutdown: &mut oneshot::Receiver<()>,
    ) -> Result<StreamRunTermination, StreamSupervisorError> {
        let mut remote_backoff =
            Backoff::new(self.policy.backoff_initial, self.policy.remote_backoff_cap);
        let mut connect_backoff =
            Backoff::new(self.policy.backoff_initial, self.policy.connect_backoff_cap);
        let mut ever_subscribed = false;
        loop {
            self.assert_sensitive_access().await?;
            let endpoint = self.endpoint.clone();
            let connect_receipt =
                self.begin_stream_attempt(crate::db::OutboundRequestOperation::StreamConnect)?;
            if let Err(error) = self.assert_sensitive_access().await {
                self.complete_stream_attempt(
                    connect_receipt,
                    crate::db::OutboundRequestOutcome::AuthenticationRejected,
                )?;
                return Err(error);
            }
            let connection = timeout(self.policy.connect_timeout, connect_async(endpoint)).await;
            let (mut socket, _) = match connection {
                Ok(Ok(value)) => {
                    self.complete_stream_attempt(
                        connect_receipt,
                        crate::db::OutboundRequestOutcome::Success,
                    )?;
                    connect_backoff.reset();
                    value
                }
                Ok(Err(_)) | Err(_) => {
                    self.complete_stream_attempt(
                        connect_receipt,
                        crate::db::OutboundRequestOutcome::TransportError,
                    )?;
                    self.emit_event(StreamEvent::TransportUnavailable).await?;
                    if wait_or_shutdown(connect_backoff.next(), shutdown).await {
                        return Ok(disconnected_termination(ever_subscribed));
                    }
                    continue;
                }
            };

            // Revalidate after the unauthenticated WebSocket handshake. No
            // retained bearer is allowed to cross a potentially blocking
            // connect, and a stale macOS admission terminates before the
            // credential-bearing subscribe frame is built or sent.
            let access_token = match self.access_token().await {
                Ok(token) => token,
                Err(AccessTokenError::AuthorityUnavailable) => {
                    let _ = socket.close(None).await;
                    return Err(StreamSupervisorError::CredentialAuthorityUnavailable);
                }
                Err(AccessTokenError::Unavailable) => {
                    let _ = socket.close(None).await;
                    self.emit_event(StreamEvent::TransportUnavailable).await?;
                    if wait_or_shutdown(connect_backoff.next(), shutdown).await {
                        return Ok(disconnected_termination(ever_subscribed));
                    }
                    continue;
                }
            };
            let subscribe = subscribe_frame(&self.tag, &access_token)
                .map_err(StreamSupervisorError::InvalidEndpoint)?;
            let subscribe_receipt =
                self.begin_stream_attempt(crate::db::OutboundRequestOperation::StreamSubscribe)?;
            if let Err(error) = self.assert_sensitive_access().await {
                self.complete_stream_attempt(
                    subscribe_receipt,
                    crate::db::OutboundRequestOutcome::AuthenticationRejected,
                )?;
                let _ = socket.close(None).await;
                return Err(error);
            }
            if socket.send(Message::Text(subscribe.into())).await.is_err() {
                self.complete_stream_attempt(
                    subscribe_receipt,
                    crate::db::OutboundRequestOutcome::TransportError,
                )?;
                self.emit_event(StreamEvent::TransportUnavailable).await?;
                if wait_or_shutdown(connect_backoff.next(), shutdown).await {
                    return Ok(disconnected_termination(ever_subscribed));
                }
                continue;
            }

            let mut subscribed = false;
            let mut subscribe_receipt = Some(subscribe_receipt);
            let mut silence = Box::pin(sleep(self.policy.silence_timeout));
            let clean = loop {
                tokio::select! {
                    _ = &mut *shutdown => break true,
                    _ = &mut silence => {
                        if let Some(receipt) = subscribe_receipt.take() {
                            self.complete_stream_attempt(
                                receipt,
                                crate::db::OutboundRequestOutcome::TransportError,
                            )?;
                        }
                        self.emit_event(StreamEvent::TransportUnavailable).await?;
                        break false;
                    }
                    frame = socket.next() => match frame {
                        Some(Ok(message)) => {
                            let Some(event) = decode_message(&self.tag, message) else {
                                continue;
                            };
                            if matches!(event, StreamEvent::Healthy) {
                                subscribed = true;
                                ever_subscribed = true;
                                // A raw WebSocket handshake proves transport
                                // only. Reset remote backoff after Tesla's
                                // control hello proves a healthy stream.
                                remote_backoff.reset();
                                if let Some(receipt) = subscribe_receipt.take() {
                                    self.complete_stream_attempt(
                                        receipt,
                                        crate::db::OutboundRequestOutcome::Success,
                                    )?;
                                }
                            }
                            let valid_after_handshake = subscribed
                                && matches!(event, StreamEvent::Healthy | StreamEvent::Telemetry(_));
                            let terminal = matches!(event, StreamEvent::AuthRejected);
                            if terminal
                                && let Some(receipt) = subscribe_receipt.take() {
                                    let outcome = if matches!(event, StreamEvent::AuthRejected) {
                                        crate::db::OutboundRequestOutcome::AuthenticationRejected
                                    } else {
                                        crate::db::OutboundRequestOutcome::ProtocolError
                                    };
                                    self.complete_stream_attempt(receipt, outcome)?;
                                }
                            self.emit_event(event).await?;
                            if terminal { break false; }
                            if !valid_after_handshake {
                                continue;
                            }
                            silence
                                .as_mut()
                                .reset(tokio::time::Instant::now() + self.policy.silence_timeout);
                        }
                        Some(Err(_)) | None => {
                            if let Some(receipt) = subscribe_receipt.take() {
                                self.complete_stream_attempt(
                                    receipt,
                                    crate::db::OutboundRequestOutcome::TransportError,
                                )?;
                            }
                            self.emit_event(StreamEvent::TransportUnavailable).await?;
                            break false;
                        }
                    }
                }
            };

            if clean {
                if let Some(receipt) = subscribe_receipt.take() {
                    self.complete_stream_attempt(
                        receipt,
                        crate::db::OutboundRequestOutcome::Cancelled,
                    )?;
                }
                if !subscribed {
                    let _ = socket.close(None).await;
                    return Ok(disconnected_termination(ever_subscribed));
                }
                // The shutdown frame carries no credential. Recheck the
                // admission immediately before sending it, but do not invoke
                // the refresh path a second time during orderly shutdown.
                let unsubscribe = unsubscribe_frame(&self.tag)
                    .map_err(|_| StreamSupervisorError::OrderlyShutdownUnavailable)?;
                let receipt = self
                    .begin_stream_attempt(crate::db::OutboundRequestOperation::StreamUnsubscribe)?;
                if let Err(error) = self.assert_sensitive_access().await {
                    self.complete_stream_attempt(
                        receipt,
                        crate::db::OutboundRequestOutcome::AuthenticationRejected,
                    )?;
                    let _ = socket.close(None).await;
                    return Err(error);
                }
                if socket
                    .send(Message::Text(unsubscribe.into()))
                    .await
                    .is_err()
                {
                    self.complete_stream_attempt(
                        receipt,
                        crate::db::OutboundRequestOutcome::TransportError,
                    )?;
                    return Err(StreamSupervisorError::OrderlyShutdownUnavailable);
                }
                self.complete_stream_attempt(receipt, crate::db::OutboundRequestOutcome::Success)?;
                let _ = socket.close(None).await;
                return Ok(StreamRunTermination::Orderly);
            }
            if wait_or_shutdown(remote_backoff.next(), shutdown).await {
                return Ok(disconnected_termination(ever_subscribed));
            }
        }
    }

    async fn emit_event(&self, event: StreamEvent) -> Result<(), StreamSupervisorError> {
        if let Some(gate) = self.power_gate.as_ref() {
            gate.observe(&event);
        }
        self.events
            .send(event)
            .await
            .map_err(|_| StreamSupervisorError::EventReceiverClosed)
    }

    fn begin_stream_attempt(
        &self,
        operation: crate::db::OutboundRequestOperation,
    ) -> Result<Option<()>, StreamSupervisorError> {
        let _ = (self.vehicle_id, operation);
        Ok(Some(()))
    }

    fn complete_stream_attempt(
        &self,
        receipt: Option<()>,
        outcome: crate::db::OutboundRequestOutcome,
    ) -> Result<(), StreamSupervisorError> {
        let _ = (receipt, outcome);
        Ok(())
    }

    async fn access_token(&self) -> Result<String, AccessTokenError> {
        match &self.credential {
            StreamCredential::Legacy(manager) => {
                let mut manager = manager.lock().await;
                let client = self.client.as_ref().ok_or(AccessTokenError::Unavailable)?;
                let refresh = manager
                    .refresh_if_due(client, std::time::SystemTime::now())
                    .await;
                if refresh
                    .as_ref()
                    .is_err_and(LegacyAuthManagerError::is_sensitive_access_failure)
                {
                    return Err(AccessTokenError::AuthorityUnavailable);
                }
                refresh.map_err(|_| AccessTokenError::Unavailable)?;
                manager
                    .access_token_for_sensitive_use()
                    .map(str::to_owned)
                    .map_err(|error| {
                        if error.is_sensitive_access_failure() {
                            AccessTokenError::AuthorityUnavailable
                        } else {
                            AccessTokenError::Unavailable
                        }
                    })
            }
        }
    }

    async fn assert_sensitive_access(&self) -> Result<(), StreamSupervisorError> {
        let StreamCredential::Legacy(manager) = &self.credential;
        manager
            .lock()
            .await
            .assert_sensitive_access()
            .map_err(|_| StreamSupervisorError::CredentialAuthorityUnavailable)
    }
}

enum AccessTokenError {
    Unavailable,
    AuthorityUnavailable,
}

fn disconnected_termination(ever_subscribed: bool) -> StreamRunTermination {
    if ever_subscribed {
        StreamRunTermination::TransportEnded
    } else {
        StreamRunTermination::CancelledBeforeSubscription
    }
}

fn endpoint_for_region(region: StreamRegion, endpoint: String) -> Result<String, StreamError> {
    let endpoint = if endpoint == GLOBAL_STREAM_ENDPOINT || endpoint == CHINA_STREAM_ENDPOINT {
        streaming_endpoint(region).to_owned()
    } else {
        endpoint
    };
    validate_endpoint_override(&endpoint)?;
    Ok(endpoint)
}

async fn wait_or_shutdown(delay: Duration, shutdown: &mut oneshot::Receiver<()>) -> bool {
    tokio::select! { _ = sleep(delay) => false, _ = shutdown => true }
}

pub fn subscribe_frame(tag: &str, token: &str) -> Result<String, StreamError> {
    validate_tag(tag)?;
    if token.is_empty() {
        return Err(StreamError::InvalidToken);
    }
    serde_json::to_string(&json!({"msg_type":"data:subscribe_oauth","token":token,"value":TESLAMATE_STREAM_FIELDS.join(","),"tag":tag}))
        .map_err(|_| StreamError::FrameSerialization)
}

pub fn unsubscribe_frame(tag: &str) -> Result<String, StreamError> {
    validate_tag(tag)?;
    serde_json::to_string(&json!({"msg_type":"data:unsubscribe","tag":tag}))
        .map_err(|_| StreamError::FrameSerialization)
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamUpdate {
    pub tag: String,
    pub timestamp_ms: i64,
    pub speed: Option<i64>,
    pub odometer: Option<f64>,
    pub soc: Option<i64>,
    pub elevation: Option<i64>,
    pub est_heading: Option<i64>,
    pub est_lat: Option<f64>,
    pub est_lng: Option<f64>,
    pub power: Option<i64>,
    pub shift_state: Option<String>,
    pub range: Option<i64>,
    pub est_range: Option<i64>,
    pub heading: Option<i64>,
}

fn decode_message(tag: &str, message: Message) -> Option<StreamEvent> {
    let text = match message {
        Message::Text(text) => text.to_string(),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok()?,
        _ => return None,
    };
    let object: Value = serde_json::from_str(&text).ok()?;
    match object.get("msg_type").and_then(Value::as_str) {
        Some("control:hello") => Some(decode_control_hello(&object)),
        Some("data:update") => parse_data_update(&text)
            .ok()
            .filter(|update| update.tag == tag)
            .map(|update| StreamEvent::Telemetry(Box::new(update))),
        Some("data:error") => match object.get("value").and_then(Value::as_str) {
            Some("vehicle_offline") | Some("Vehicle is offline") => {
                Some(StreamEvent::VehicleOffline)
            }
            Some(value) if value.contains("Can't validate token") => {
                Some(StreamEvent::AuthRejected)
            }
            _ => Some(StreamEvent::TransportUnavailable),
        },
        _ => None,
    }
}

fn decode_control_hello(object: &Value) -> StreamEvent {
    match object.get("code") {
        Some(code) => match code.as_i64() {
            Some(200) => StreamEvent::Healthy,
            Some(401 | 403) => StreamEvent::AuthRejected,
            _ => StreamEvent::TransportUnavailable,
        },
        None if object
            .get("connection_timeout")
            .and_then(Value::as_i64)
            .is_some() =>
        {
            StreamEvent::Healthy
        }
        None => StreamEvent::TransportUnavailable,
    }
}

pub fn parse_data_update(frame: &str) -> Result<StreamUpdate, StreamError> {
    let object: Value =
        serde_json::from_str(frame).map_err(|_| StreamError::MalformedDataUpdate)?;
    if object.get("msg_type").and_then(Value::as_str) != Some("data:update") {
        return Err(StreamError::WrongMessageType);
    }
    let tag = object
        .get("tag")
        .and_then(Value::as_str)
        .ok_or(StreamError::MalformedDataUpdate)?;
    validate_tag(tag)?;
    let values: Vec<&str> = object
        .get("value")
        .and_then(Value::as_str)
        .ok_or(StreamError::MalformedDataUpdate)?
        .split(',')
        .collect();
    let (timestamp_ms, values) = match values.len() {
        length if length == TESLAMATE_STREAM_FIELDS.len() => {
            let timestamp_ms = object
                .get("timestamp")
                .and_then(Value::as_i64)
                .filter(|timestamp| *timestamp > 0)
                .ok_or(StreamError::InvalidTimestamp)?;
            (timestamp_ms, values.as_slice())
        }
        length if length == TESLAMATE_STREAM_FIELDS.len() + 1 => {
            if object.get("timestamp").is_some() {
                return Err(StreamError::MalformedDataUpdate);
            }
            let timestamp_ms = values[0]
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|timestamp| *timestamp > 0)
                .ok_or(StreamError::InvalidTimestamp)?;
            (timestamp_ms, &values[1..])
        }
        _ => return Err(StreamError::MalformedDataUpdate),
    };
    Ok(StreamUpdate {
        tag: tag.to_owned(),
        timestamp_ms,
        speed: optional_i64(values[0])?,
        odometer: optional_f64(values[1])?,
        soc: optional_i64(values[2])?,
        elevation: optional_i64(values[3])?,
        est_heading: optional_i64(values[4])?,
        est_lat: optional_f64(values[5])?,
        est_lng: optional_f64(values[6])?,
        power: optional_i64(values[7])?,
        shift_state: optional_state(values[8]),
        range: optional_i64(values[9])?,
        est_range: optional_i64(values[10])?,
        heading: optional_i64(values[11])?,
    })
}

fn validate_tag(tag: &str) -> Result<(), StreamError> {
    if tag.is_empty()
        || tag.len() > 128
        || !tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(StreamError::InvalidTag);
    }
    Ok(())
}
fn optional_i64(value: &str) -> Result<Option<i64>, StreamError> {
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse()
            .map(Some)
            .map_err(|_| StreamError::InvalidField)
    }
}
fn optional_f64(value: &str) -> Result<Option<f64>, StreamError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let v = value
        .parse::<f64>()
        .map_err(|_| StreamError::InvalidField)?;
    if v.is_finite() {
        Ok(Some(v))
    } else {
        Err(StreamError::InvalidField)
    }
}
fn optional_state(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{net::TcpListener, time::timeout};
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    fn legacy_supervisor(
        vehicle_id: u64,
        stream_vehicle_id: u64,
        endpoint: String,
        events: mpsc::Sender<StreamEvent>,
    ) -> Result<TeslaStreamSupervisor, StreamSupervisorError> {
        crate::crypto::install_default_provider();
        let auth = crate::legacy_auth::LegacyAuth::for_test(
            Url::parse("http://127.0.0.1:9/").unwrap(),
            "test-access",
            "test-refresh",
        )
        .with_test_schedule(2_000_000_000, 1_900_000_000);
        let manager = LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
        let client = reqwest::Client::builder().build().unwrap();
        TeslaStreamSupervisor::new_legacy_auth(
            VehicleId::from_test(vehicle_id),
            StreamVehicleId::from_test(stream_vehicle_id),
            Arc::new(Mutex::new(manager)),
            StreamRegion::Global,
            endpoint,
            client,
            events,
        )
    }

    #[test]
    fn protocol_frames_match_teslamate() {
        let value: Value =
            serde_json::from_str(&subscribe_frame("9", "fake-token").unwrap()).unwrap();
        assert_eq!(value["msg_type"], "data:subscribe_oauth");
        assert_eq!(value["tag"], "9");
        assert_eq!(value["value"], TESLAMATE_STREAM_FIELDS.join(","));
        let unsubscribe: Value = serde_json::from_str(&unsubscribe_frame("9").unwrap()).unwrap();
        assert_eq!(unsubscribe["msg_type"], "data:unsubscribe");
        assert!(unsubscribe.get("token").is_none());
    }

    #[tokio::test]
    async fn local_mock_receives_subscribe_and_unsubscribe() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            let first = ws.next().await.unwrap().unwrap();
            let Message::Text(first) = first else {
                panic!("stream subscribe must be text")
            };
            let subscribe: Value = serde_json::from_str(&first).unwrap();
            assert_eq!(subscribe["msg_type"], "data:subscribe_oauth");
            assert_eq!(subscribe["tag"], "42");
            let _ = ws
                .send(Message::Text(
                    r#"{"msg_type":"control:hello","code":200}"#.into(),
                ))
                .await;
            let second = ws.next().await.unwrap().unwrap();
            assert!(matches!(second,Message::Text(ref text) if text.contains("data:unsubscribe")));
        });
        let (events, mut received) = mpsc::channel(4);
        let supervisor = legacy_supervisor(9, 42, endpoint, events).unwrap();
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(supervisor.run(shutdown));
        assert_eq!(
            timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap(),
            Some(StreamEvent::Healthy)
        );
        stop.send(()).unwrap();
        timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalidated_sensitive_guard_blocks_subscribe_after_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            let frame = timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("client closes denied stream");
            assert!(
                !matches!(frame, Some(Ok(Message::Text(text))) if text.contains("data:subscribe_oauth")),
                "invalidated admission must block the bearer subscribe frame"
            );
        });
        let auth = crate::legacy_auth::LegacyAuth::for_test(
            Url::parse("http://127.0.0.1:9/").unwrap(),
            "test-access",
            "test-refresh",
        )
        .with_test_schedule(2_000_000_000, 1_900_000_000);
        let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let manager = LegacyAuthManager::for_test_with_sensitive_access(
            auth,
            Arc::new(|_, _| Ok(())),
            Arc::new(move || {
                (checks.fetch_add(1, Ordering::AcqRel) < 4)
                    .then_some(())
                    .ok_or(crate::credentials::CredentialError::SensitiveAccessUnavailable)
            }),
        );
        let (events, _) = mpsc::channel(1);
        crate::crypto::install_default_provider();
        let supervisor = TeslaStreamSupervisor::new_legacy_auth(
            VehicleId::from_test(9),
            StreamVehicleId::from_test(9),
            Arc::new(Mutex::new(manager)),
            StreamRegion::Global,
            endpoint,
            Client::new(),
            events,
        )
        .unwrap();
        let (_stop, shutdown) = oneshot::channel();

        assert!(matches!(
            supervisor.run(shutdown).await,
            Err(StreamSupervisorError::CredentialAuthorityUnavailable)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn observer_reconnects_with_access_token_without_refreshing() {
        let data = tempfile::tempdir().expect("data directory");
        let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
        crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key")
            .expect("private key");
        let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
        let tokens = crate::credentials::OwnerTokens::from_secret_parts(
            "observer-access".to_owned(),
            "observer-refresh".to_owned(),
        )
        .expect("observer tokens");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
                .expect("encrypt observer tokens");
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    access,
                    refresh,
                    2_000_000_000,
                    1_900_000_000,
                )
                .expect("schedule"),
            )
            .expect("store observer tokens");
        let fake = crate::fake_tesla::FakeTeslaSource::spawn_canonical(
            crate::fake_tesla::AdvanceMode::Manual,
        )
        .await
        .expect("fake Tesla");
        let manager = LegacyAuthManager::from_hub_teslamate_store_observer_with_issuer(
            store,
            data.path(),
            fake.oauth_issuer_url(),
        )
        .expect("observer manager");
        crate::crypto::install_default_provider();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
        let (reconnected, reconnected_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut first = accept_async(tcp).await.unwrap();
            let first_subscribe = first.next().await.unwrap().unwrap();
            assert!(
                matches!(first_subscribe, Message::Text(ref text) if text.contains("observer-access"))
            );
            drop(first);

            let (tcp, _) = listener.accept().await.unwrap();
            let mut second = accept_async(tcp).await.unwrap();
            let second_subscribe = second.next().await.unwrap().unwrap();
            assert!(
                matches!(second_subscribe, Message::Text(ref text) if text.contains("observer-access"))
            );
            second
                .send(Message::Text(
                    r#"{"msg_type":"control:hello","code":200}"#.into(),
                ))
                .await
                .unwrap();
            reconnected.send(()).unwrap();
            let _ = second.next().await;
        });
        let (events, _received) = mpsc::channel(4);
        let supervisor = TeslaStreamSupervisor::new_legacy_auth(
            VehicleId::from_test(9),
            StreamVehicleId::from_test(9),
            Arc::new(Mutex::new(manager)),
            StreamRegion::Global,
            endpoint,
            Client::new(),
            events,
        )
        .unwrap()
        .with_policy(SupervisorPolicy {
            connect_timeout: Duration::from_millis(100),
            silence_timeout: Duration::from_secs(1),
            backoff_initial: Duration::from_millis(5),
            remote_backoff_cap: Duration::from_millis(10),
            connect_backoff_cap: Duration::from_millis(10),
        });
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(supervisor.run(shutdown));
        timeout(Duration::from_secs(1), reconnected_rx)
            .await
            .unwrap()
            .unwrap();
        stop.send(()).unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.await.unwrap();
        assert_eq!(fake.token_refresh_request_count(), 0);
        fake.shutdown().await;
    }

    #[tokio::test]
    async fn full_event_queue_applies_backpressure_without_reconnect_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Text(ref text) if text.contains("data:subscribe_oauth")
            ));
            socket
                .send(Message::Text(
                    r#"{"msg_type":"control:hello","code":200}"#.into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#.into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    r#"{"msg_type":"data:update","tag":"9","timestamp":1700000001123,"value":"42,12345.7,80,25,180,51.5,-0.1,120,D,200,210,180"}"#.into(),
                ))
                .await
                .unwrap();
        });

        let (events, mut receiver) = mpsc::channel(1);
        let supervisor = legacy_supervisor(9, 9, endpoint, events).unwrap();
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(supervisor.run(shutdown));

        assert!(matches!(
            timeout(Duration::from_secs(1), receiver.recv())
                .await
                .expect("healthy event")
                .expect("healthy event present"),
            StreamEvent::Healthy
        ));
        // Capacity one is intentionally filled by the first telemetry frame.
        // The second frame must wait, not terminate the stream or reconnect.
        assert!(
            timeout(Duration::from_secs(1), receiver.recv())
                .await
                .expect("first telemetry")
                .is_some()
        );
        assert!(
            timeout(Duration::from_secs(1), receiver.recv())
                .await
                .expect("second telemetry")
                .is_some()
        );
        stop.send(()).expect("stop stream");
        timeout(Duration::from_secs(1), task)
            .await
            .expect("stream shutdown")
            .expect("stream task")
            .expect("orderly shutdown");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn silence_emits_transport_event_and_reconnects_with_bounded_backoff() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
        let (reconnected, reconnected_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut first = accept_async(tcp).await.unwrap();
            let message = first.next().await.unwrap().unwrap();
            assert!(
                matches!(message, Message::Text(ref text) if text.contains("data:subscribe_oauth"))
            );
            tokio::time::sleep(Duration::from_millis(35)).await;
            drop(first);

            let (tcp, _) = listener.accept().await.unwrap();
            let mut second = accept_async(tcp).await.unwrap();
            let message = second.next().await.unwrap().unwrap();
            assert!(
                matches!(message, Message::Text(ref text) if text.contains("data:subscribe_oauth"))
            );
            reconnected.send(()).unwrap();
            let message = second.next().await;
            assert!(matches!(message, Some(Ok(Message::Close(_))) | None));
        });

        let (events, mut received) = mpsc::channel(4);
        let supervisor = legacy_supervisor(9, 9, endpoint, events)
            .unwrap()
            .with_policy(SupervisorPolicy {
                connect_timeout: Duration::from_millis(100),
                silence_timeout: Duration::from_millis(20),
                backoff_initial: Duration::from_millis(5),
                remote_backoff_cap: Duration::from_millis(10),
                connect_backoff_cap: Duration::from_millis(10),
            });
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(supervisor.run(shutdown));
        assert_eq!(
            timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap(),
            Some(StreamEvent::TransportUnavailable)
        );
        timeout(Duration::from_secs(1), reconnected_rx)
            .await
            .unwrap()
            .unwrap();
        stop.send(()).unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn health_requires_hello_then_recovers_after_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut first = accept_async(tcp).await.unwrap();
            assert!(matches!(
                first.next().await.unwrap().unwrap(),
                Message::Text(ref text) if text.contains("data:subscribe_oauth")
            ));
            first
                .send(Message::Text(
                    r#"{"msg_type":"control:hello","code":200}"#.into(),
                ))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(35)).await;
            drop(first);

            let (tcp, _) = listener.accept().await.unwrap();
            let mut second = accept_async(tcp).await.unwrap();
            assert!(matches!(
                second.next().await.unwrap().unwrap(),
                Message::Text(ref text) if text.contains("data:subscribe_oauth")
            ));
            second
                .send(Message::Text(
                    r#"{"msg_type":"control:hello","code":200}"#.into(),
                ))
                .await
                .unwrap();
            assert!(matches!(
                second.next().await.unwrap().unwrap(),
                Message::Text(ref text) if text.contains("data:unsubscribe")
            ));
        });

        let (events, mut received) = mpsc::channel(8);
        let supervisor = legacy_supervisor(9, 9, endpoint, events)
            .unwrap()
            .with_policy(SupervisorPolicy {
                connect_timeout: Duration::from_millis(100),
                silence_timeout: Duration::from_millis(20),
                backoff_initial: Duration::from_millis(5),
                remote_backoff_cap: Duration::from_millis(10),
                connect_backoff_cap: Duration::from_millis(10),
            });
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(supervisor.run(shutdown));
        assert_eq!(
            timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap(),
            Some(StreamEvent::Healthy)
        );
        assert_eq!(
            timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap(),
            Some(StreamEvent::TransportUnavailable)
        );
        assert_eq!(
            timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap(),
            Some(StreamEvent::Healthy)
        );
        stop.send(()).unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn legacy_token_refresh_is_cancelled_by_owner_api_client_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (_tcp, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        let auth = crate::legacy_auth::LegacyAuth::for_test(
            Url::parse(&issuer).unwrap(),
            "old-access",
            "old-refresh",
        );
        let manager = crate::credentials::LegacyAuthManager::for_test(
            auth,
            std::sync::Arc::new(|_, _| Ok(())),
        );
        crate::crypto::install_default_provider();
        let client = Client::builder()
            .timeout(Duration::from_millis(25))
            .build()
            .unwrap();
        let (events, _) = mpsc::channel(1);
        let supervisor = TeslaStreamSupervisor::new_legacy_auth(
            crate::owner_api::VehicleId::from_test(9),
            crate::owner_api::StreamVehicleId::from_test(9),
            Arc::new(Mutex::new(manager)),
            StreamRegion::Global,
            "ws://127.0.0.1:1/streaming/".to_owned(),
            client,
            events,
        )
        .unwrap();

        assert!(
            timeout(Duration::from_secs(1), supervisor.access_token())
                .await
                .unwrap()
                .is_err()
        );
        server.await.unwrap();
    }

    #[test]
    fn text_and_utf8_binary_frames_decode_identically() {
        let hello = r#"{"msg_type":"control:hello","connection_timeout":15}"#;
        assert_eq!(
            decode_message("42", Message::Text(hello.into())),
            decode_message("42", Message::Binary(hello.as_bytes().to_vec().into()))
        );

        let update = r#"{"msg_type":"data:update","tag":"42","value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#;
        let text = decode_message("42", Message::Text(update.into()));
        let binary = decode_message("42", Message::Binary(update.as_bytes().to_vec().into()));
        assert!(matches!(text, Some(StreamEvent::Telemetry(_))));
        assert_eq!(text, binary);
    }

    #[test]
    fn non_utf8_binary_and_websocket_control_frames_are_ignored() {
        assert_eq!(
            decode_message("42", Message::Binary(vec![0xff, 0xfe].into())),
            None
        );
        assert_eq!(decode_message("42", Message::Ping(Vec::new().into())), None);
        assert_eq!(decode_message("42", Message::Pong(Vec::new().into())), None);
        assert_eq!(decode_message("42", Message::Close(None)), None);
    }

    #[test]
    fn accepts_teslamate_control_hello_without_status_code() {
        let event = decode_message(
            "9",
            Message::Text(r#"{"msg_type":"control:hello","connection_timeout":15}"#.into()),
        );

        assert_eq!(event, Some(StreamEvent::Healthy));
    }

    #[test]
    fn control_hello_auth_rejection_takes_precedence_over_timeout() {
        let event = decode_message(
            "9",
            Message::Text(
                r#"{"msg_type":"control:hello","connection_timeout":15,"code":401}"#.into(),
            ),
        );

        assert_eq!(event, Some(StreamEvent::AuthRejected));
        assert_eq!(
            decode_message(
                "9",
                Message::Text(r#"{"msg_type":"control:hello","connection_timeout":0}"#.into(),),
            ),
            Some(StreamEvent::Healthy)
        );
    }

    #[test]
    fn parses_teslamate_timestamp_first_stream_values() {
        let update = parse_data_update(
            r#"{"msg_type":"data:update","tag":"9","value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#,
        )
        .unwrap();

        assert_eq!(update.timestamp_ms, 1_700_000_000_123);
        assert_eq!(update.speed, Some(42));
        assert_eq!(update.odometer, Some(12_345.6));
        assert_eq!(update.est_lat, Some(51.5));
        assert_eq!(update.shift_state.as_deref(), Some("D"));
        assert_eq!(update.heading, Some(180));
    }

    #[test]
    fn timestamp_first_stream_values_fail_closed_on_ambiguity_or_bad_time() {
        assert_eq!(
            parse_data_update(
                r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#,
            ),
            Err(StreamError::MalformedDataUpdate)
        );
        assert_eq!(
            parse_data_update(
                r#"{"msg_type":"data:update","tag":"9","value":"not-a-time,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#,
            ),
            Err(StreamError::InvalidTimestamp)
        );
    }

    #[test]
    fn parses_nested_stream_values() {
        let update=parse_data_update(r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#).unwrap();
        assert_eq!(update.speed, Some(42));
        assert_eq!(update.est_lat, Some(51.5));
    }
}
