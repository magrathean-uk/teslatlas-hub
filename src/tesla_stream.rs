use std::{sync::Arc, time::Duration};

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
use uuid::Uuid;

use crate::{
    credentials::{LegacyAuthManager, LegacyAuthManagerError, OwnerToken},
    legacy_auth::{
        LegacyAuthError, LegacyRefreshAuditContext, LegacyRefreshAuditOutcome,
        LegacyRefreshAuditReceipt, LegacyRefreshAuditSink,
    },
    owner_api::VehicleId,
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
    Telemetry(StreamUpdate),
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
    #[error("stream request audit is unavailable")]
    AuditUnavailable,
    #[error("stream could not complete orderly unsubscribe shutdown")]
    OrderlyShutdownUnavailable,
}

/// Durable, run-scoped capability for outbound streaming operations. It owns a
/// clone of the Hub store so a spawned supervisor never borrows its collector.
#[derive(Clone)]
pub(crate) struct StreamRequestAudit {
    store: crate::db::HubStore,
    correlation_id: Uuid,
}

struct StreamLegacyRefreshAudit {
    store: crate::db::HubStore,
    correlation_id: Uuid,
}

impl LegacyRefreshAuditSink for StreamLegacyRefreshAudit {
    fn begin_token_refresh(&self) -> Result<LegacyRefreshAuditReceipt, LegacyAuthError> {
        let receipt = self
            .store
            .begin_outbound_request(&crate::db::OutboundRequestStart {
                correlation_id: self.correlation_id,
                vehicle_tesla_id: None,
                transport: crate::db::OutboundRequestTransport::LegacyAuth,
                operation: crate::db::OutboundRequestOperation::TokenRefresh,
                safety_class: crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                precondition: crate::db::OutboundRequestPrecondition::NotRequired,
            })
            .map_err(|_| LegacyAuthError::AuditUnavailable)?;
        Ok(LegacyRefreshAuditReceipt(receipt.0))
    }

    fn complete_token_refresh(
        &self,
        receipt: LegacyRefreshAuditReceipt,
        outcome: LegacyRefreshAuditOutcome,
    ) -> Result<(), LegacyAuthError> {
        let (outcome, http_status) = match outcome {
            LegacyRefreshAuditOutcome::Success => (crate::db::OutboundRequestOutcome::Success, None),
            LegacyRefreshAuditOutcome::HttpError(status) => {
                (crate::db::OutboundRequestOutcome::HttpError, Some(status))
            }
            LegacyRefreshAuditOutcome::AuthenticationRejected => {
                (crate::db::OutboundRequestOutcome::AuthenticationRejected, Some(401))
            }
            LegacyRefreshAuditOutcome::TransportError => {
                (crate::db::OutboundRequestOutcome::TransportError, None)
            }
            LegacyRefreshAuditOutcome::ResponseTooLarge => {
                (crate::db::OutboundRequestOutcome::ResponseTooLarge, None)
            }
            LegacyRefreshAuditOutcome::ProtocolError => {
                (crate::db::OutboundRequestOutcome::ProtocolError, None)
            }
        };
        self.store
            .complete_outbound_request(
                crate::db::OutboundRequestReceiptId(receipt.0),
                &crate::db::OutboundRequestCompletion { outcome, http_status },
            )
            .map_err(|_| LegacyAuthError::AuditUnavailable)
    }
}

impl StreamRequestAudit {
    pub(crate) fn new(store: &crate::db::HubStore, correlation_id: Uuid) -> Self {
        Self {
            store: store.clone(),
            correlation_id,
        }
    }

    fn begin(
        &self,
        vehicle_id: VehicleId,
        operation: crate::db::OutboundRequestOperation,
    ) -> Result<crate::db::OutboundRequestReceiptId, StreamSupervisorError> {
        let vehicle_tesla_id = i64::try_from(vehicle_id.get())
            .map_err(|_| StreamSupervisorError::AuditUnavailable)?;
        self.store
            .begin_outbound_request(&crate::db::OutboundRequestStart {
                correlation_id: self.correlation_id,
                vehicle_tesla_id: Some(vehicle_tesla_id),
                transport: crate::db::OutboundRequestTransport::Stream,
                operation,
                safety_class: crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                precondition: crate::db::OutboundRequestPrecondition::NotRequired,
            })
            .map_err(|_| StreamSupervisorError::AuditUnavailable)
    }

    fn complete(
        &self,
        receipt: crate::db::OutboundRequestReceiptId,
        outcome: crate::db::OutboundRequestOutcome,
    ) -> Result<(), StreamSupervisorError> {
        self.store
            .complete_outbound_request(
                receipt,
                &crate::db::OutboundRequestCompletion {
                    outcome,
                    http_status: None,
                },
            )
            .map_err(|_| StreamSupervisorError::AuditUnavailable)
    }

    fn begin_session(
        &self,
        vehicle_id: VehicleId,
    ) -> Result<crate::db::StreamSessionReceiptId, StreamSupervisorError> {
        let vehicle_tesla_id = i64::try_from(vehicle_id.get())
            .map_err(|_| StreamSupervisorError::AuditUnavailable)?;
        self.store
            .begin_stream_session(self.correlation_id, vehicle_tesla_id)
            .map_err(|_| StreamSupervisorError::AuditUnavailable)
    }

    fn complete_session(
        &self,
        session: crate::db::StreamSessionReceiptId,
        unsubscribe: crate::db::OutboundRequestReceiptId,
    ) -> Result<(), StreamSupervisorError> {
        self.store
            .complete_stream_session_orderly(session, unsubscribe)
            .map_err(|_| StreamSupervisorError::AuditUnavailable)
    }

    fn legacy_refresh_context(&self) -> LegacyRefreshAuditContext {
        LegacyRefreshAuditContext::new(Arc::new(StreamLegacyRefreshAudit {
            store: self.store.clone(),
            correlation_id: self.correlation_id,
        }))
    }
}

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

pub struct TeslaStreamSupervisor {
    vehicle_id: VehicleId,
    tag: String,
    credential: StreamCredential,
    client: Option<Client>,
    endpoint: String,
    events: mpsc::Sender<StreamEvent>,
    policy: SupervisorPolicy,
    audit: Option<StreamRequestAudit>,
}

#[derive(Clone)]
enum StreamCredential {
    Token(Arc<OwnerToken>),
    Legacy(Arc<Mutex<LegacyAuthManager>>),
}

impl TeslaStreamSupervisor {
    pub fn new(
        vehicle_id: VehicleId,
        token: OwnerToken,
        region: StreamRegion,
        endpoint: String,
        events: mpsc::Sender<StreamEvent>,
    ) -> Result<Self, StreamSupervisorError> {
        Self::new_shared(vehicle_id, Arc::new(token), region, endpoint, events)
    }

    pub fn new_shared(
        vehicle_id: VehicleId,
        token: Arc<OwnerToken>,
        region: StreamRegion,
        endpoint: String,
        events: mpsc::Sender<StreamEvent>,
    ) -> Result<Self, StreamSupervisorError> {
        let endpoint = endpoint_for_region(region, endpoint)?;
        let tag = vehicle_id.to_string();
        if tag.is_empty() {
            return Err(StreamSupervisorError::InvalidTag);
        }
        Ok(Self {
            vehicle_id,
            tag,
            credential: StreamCredential::Token(token),
            client: None,
            endpoint,
            events,
            policy: DEFAULT_POLICY,
            audit: None,
        })
    }

    pub fn new_legacy_auth(
        vehicle_id: VehicleId,
        manager: Arc<Mutex<LegacyAuthManager>>,
        region: StreamRegion,
        endpoint: String,
        client: Client,
        events: mpsc::Sender<StreamEvent>,
    ) -> Result<Self, StreamSupervisorError> {
        let endpoint = endpoint_for_region(region, endpoint)?;
        let tag = vehicle_id.to_string();
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
            audit: None,
        })
    }

    pub(crate) fn new_shared_audited(
        vehicle_id: VehicleId,
        token: Arc<OwnerToken>,
        region: StreamRegion,
        endpoint: String,
        events: mpsc::Sender<StreamEvent>,
        audit: StreamRequestAudit,
    ) -> Result<Self, StreamSupervisorError> {
        let mut supervisor = Self::new_shared(vehicle_id, token, region, endpoint, events)?;
        supervisor.audit = Some(audit);
        Ok(supervisor)
    }

    pub(crate) fn new_legacy_auth_audited(
        vehicle_id: VehicleId,
        manager: Arc<Mutex<LegacyAuthManager>>,
        region: StreamRegion,
        endpoint: String,
        client: Client,
        events: mpsc::Sender<StreamEvent>,
        audit: StreamRequestAudit,
    ) -> Result<Self, StreamSupervisorError> {
        let mut supervisor = Self::new_legacy_auth(
            vehicle_id, manager, region, endpoint, client, events,
        )?;
        supervisor.audit = Some(audit);
        Ok(supervisor)
    }

    #[cfg(test)]
    fn with_policy(mut self, policy: SupervisorPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_health_timeout(mut self, timeout: Duration) -> Self {
        if !timeout.is_zero() {
            self.policy.silence_timeout = timeout;
        }
        self
    }

    pub async fn run(
        self,
        mut shutdown: oneshot::Receiver<()>,
    ) -> Result<(), StreamSupervisorError> {
        #[cfg(not(test))]
        if self.audit.is_none() {
            return Err(StreamSupervisorError::AuditUnavailable);
        }
        let session = self.begin_stream_session()?;
        let mut remote_backoff =
            Backoff::new(self.policy.backoff_initial, self.policy.remote_backoff_cap);
        let mut connect_backoff =
            Backoff::new(self.policy.backoff_initial, self.policy.connect_backoff_cap);
        loop {
            let access_token = match self.access_token().await {
                Ok(token) => token,
                Err(AccessTokenError::AuditUnavailable) => {
                    return Err(StreamSupervisorError::AuditUnavailable);
                }
                Err(AccessTokenError::Unavailable) => {
                    let _ = self.events.send(StreamEvent::TransportUnavailable).await;
                    if wait_or_shutdown(connect_backoff.next(), &mut shutdown).await {
                        return Ok(());
                    }
                    continue;
                }
            };
            let connect_receipt = self.begin_stream_attempt(crate::db::OutboundRequestOperation::StreamConnect)?;
            let connection = timeout(
                self.policy.connect_timeout,
                connect_async(self.endpoint.clone()),
            )
            .await;
            let (mut socket, _) = match connection {
                Ok(Ok(value)) => {
                    self.complete_stream_attempt(
                        connect_receipt,
                        crate::db::OutboundRequestOutcome::Success,
                    )?;
                    remote_backoff.reset();
                    connect_backoff.reset();
                    value
                }
                Ok(Err(_)) | Err(_) => {
                    self.complete_stream_attempt(
                        connect_receipt,
                        crate::db::OutboundRequestOutcome::TransportError,
                    )?;
                    let _ = self.events.send(StreamEvent::TransportUnavailable).await;
                    if wait_or_shutdown(connect_backoff.next(), &mut shutdown).await {
                        return Ok(());
                    }
                    continue;
                }
            };

            let subscribe = subscribe_frame(&self.tag, &access_token)
                .map_err(StreamSupervisorError::InvalidEndpoint)?;
            let subscribe_receipt = self.begin_stream_attempt(
                crate::db::OutboundRequestOperation::StreamSubscribe,
            )?;
            if socket.send(Message::Text(subscribe.into())).await.is_err() {
                self.complete_stream_attempt(
                    subscribe_receipt,
                    crate::db::OutboundRequestOutcome::TransportError,
                )?;
                let _ = self.events.send(StreamEvent::TransportUnavailable).await;
                if wait_or_shutdown(connect_backoff.next(), &mut shutdown).await {
                    return Ok(());
                }
                continue;
            }

            let mut subscribed = false;
            let mut subscribe_receipt = Some(subscribe_receipt);
            let mut silence = Box::pin(sleep(self.policy.silence_timeout));
            let clean = loop {
                tokio::select! {
                    _ = &mut shutdown => break true,
                    _ = &mut silence => {
                        if let Some(receipt) = subscribe_receipt.take() {
                            self.complete_stream_attempt(
                                receipt,
                                crate::db::OutboundRequestOutcome::TransportError,
                            )?;
                        }
                        let _ = self.events.send(StreamEvent::TransportUnavailable).await;
                        break false;
                    }
                    frame = socket.next() => match frame {
                        Some(Ok(message)) => {
                            let Some(event) = decode_message(&self.tag, message) else {
                                continue;
                            };
                            if matches!(event, StreamEvent::Healthy) {
                                subscribed = true;
                                if let Some(receipt) = subscribe_receipt.take() {
                                    self.complete_stream_attempt(
                                        receipt,
                                        crate::db::OutboundRequestOutcome::Success,
                                    )?;
                                }
                            }
                            let valid_after_handshake = subscribed
                                && matches!(event, StreamEvent::Healthy | StreamEvent::Telemetry(_));
                            let terminal = matches!(event, StreamEvent::VehicleOffline | StreamEvent::AuthRejected);
                            if terminal {
                                if let Some(receipt) = subscribe_receipt.take() {
                                    let outcome = if matches!(event, StreamEvent::AuthRejected) {
                                        crate::db::OutboundRequestOutcome::AuthenticationRejected
                                    } else {
                                        crate::db::OutboundRequestOutcome::ProtocolError
                                    };
                                    self.complete_stream_attempt(receipt, outcome)?;
                                }
                            }
                            let _ = self.events.send(event).await;
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
                            let _ = self.events.send(StreamEvent::TransportUnavailable).await;
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
                let unsubscribe = unsubscribe_frame(&self.tag, &access_token)
                    .map_err(|_| StreamSupervisorError::OrderlyShutdownUnavailable)?;
                let receipt = self.begin_stream_attempt(
                    crate::db::OutboundRequestOperation::StreamUnsubscribe,
                )?;
                if socket.send(Message::Text(unsubscribe.into())).await.is_err() {
                    self.complete_stream_attempt(
                        receipt,
                        crate::db::OutboundRequestOutcome::TransportError,
                    )?;
                    return Err(StreamSupervisorError::OrderlyShutdownUnavailable);
                }
                self.complete_stream_attempt(receipt, crate::db::OutboundRequestOutcome::Success)?;
                self.complete_stream_session(session, receipt)?;
                let _ = socket.close(None).await;
                return Ok(());
            }
            if wait_or_shutdown(remote_backoff.next(), &mut shutdown).await {
                return Ok(());
            }
        }
    }

    pub fn vehicle_id(&self) -> VehicleId {
        self.vehicle_id
    }

    fn begin_stream_attempt(
        &self,
        operation: crate::db::OutboundRequestOperation,
    ) -> Result<Option<crate::db::OutboundRequestReceiptId>, StreamSupervisorError> {
        match &self.audit {
            Some(audit) => audit.begin(self.vehicle_id, operation).map(Some),
            #[cfg(test)]
            None => Ok(None),
            #[cfg(not(test))]
            None => Err(StreamSupervisorError::AuditUnavailable),
        }
    }

    fn begin_stream_session(
        &self,
    ) -> Result<Option<crate::db::StreamSessionReceiptId>, StreamSupervisorError> {
        match &self.audit {
            Some(audit) => audit.begin_session(self.vehicle_id).map(Some),
            #[cfg(test)]
            None => Ok(None),
            #[cfg(not(test))]
            None => Err(StreamSupervisorError::AuditUnavailable),
        }
    }

    fn complete_stream_attempt(
        &self,
        receipt: Option<crate::db::OutboundRequestReceiptId>,
        outcome: crate::db::OutboundRequestOutcome,
    ) -> Result<(), StreamSupervisorError> {
        match (receipt, &self.audit) {
            (Some(receipt), Some(audit)) => audit.complete(receipt, outcome),
            #[cfg(test)]
            (None, None) => Ok(()),
            _ => Err(StreamSupervisorError::AuditUnavailable),
        }
    }

    fn complete_stream_session(
        &self,
        session: Option<crate::db::StreamSessionReceiptId>,
        unsubscribe: Option<crate::db::OutboundRequestReceiptId>,
    ) -> Result<(), StreamSupervisorError> {
        match (session, unsubscribe, &self.audit) {
            (Some(session), Some(unsubscribe), Some(audit)) => {
                audit.complete_session(session, unsubscribe)
            }
            #[cfg(test)]
            (None, None, None) => Ok(()),
            _ => Err(StreamSupervisorError::AuditUnavailable),
        }
    }

    async fn access_token(&self) -> Result<String, AccessTokenError> {
        match &self.credential {
            StreamCredential::Token(token) => Ok(token.as_str().to_owned()),
            StreamCredential::Legacy(manager) => {
                let mut manager = manager.lock().await;
                let client = self.client.as_ref().ok_or(AccessTokenError::Unavailable)?;
                let refresh = match &self.audit {
                    Some(audit) => crate::legacy_auth::with_legacy_refresh_audit(
                        audit.legacy_refresh_context(),
                        manager.refresh_if_due(client, std::time::SystemTime::now()),
                    )
                    .await,
                    #[cfg(test)]
                    None => manager.refresh_if_due(client, std::time::SystemTime::now()).await,
                    #[cfg(not(test))]
                    None => return Err(AccessTokenError::AuditUnavailable),
                };
                if matches!(
                    &refresh,
                    Err(LegacyAuthManagerError::Auth(LegacyAuthError::AuditUnavailable))
                ) {
                    return Err(AccessTokenError::AuditUnavailable);
                }
                refresh.map_err(|_| AccessTokenError::Unavailable)?;
                Ok(manager.access_token().to_owned())
            }
        }
    }
}

enum AccessTokenError {
    Unavailable,
    AuditUnavailable,
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

pub fn unsubscribe_frame(tag: &str, token: &str) -> Result<String, StreamError> {
    validate_tag(tag)?;
    if token.is_empty() {
        return Err(StreamError::InvalidToken);
    }
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
    let Message::Text(text) = message else {
        return None;
    };
    let object: Value = serde_json::from_str(&text).ok()?;
    match object.get("msg_type").and_then(Value::as_str) {
        Some("control:hello") => match object.get("code").and_then(Value::as_i64) {
            Some(200) => Some(StreamEvent::Healthy),
            Some(401 | 403) => Some(StreamEvent::AuthRejected),
            _ => Some(StreamEvent::TransportUnavailable),
        },
        Some("data:update") => parse_data_update(&text)
            .ok()
            .filter(|update| update.tag == tag)
            .map(StreamEvent::Telemetry),
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
    let timestamp_ms = object
        .get("timestamp")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .ok_or(StreamError::InvalidTimestamp)?;
    let values: Vec<&str> = object
        .get("value")
        .and_then(Value::as_str)
        .ok_or(StreamError::MalformedDataUpdate)?
        .split(',')
        .collect();
    if values.len() != TESLAMATE_STREAM_FIELDS.len() {
        return Err(StreamError::MalformedDataUpdate);
    }
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

    fn token() -> OwnerToken {
        OwnerTokenForTest::make()
    }
    struct OwnerTokenForTest;
    impl OwnerTokenForTest {
        fn make() -> OwnerToken {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(crate::credentials::OWNER_TOKEN_CREDENTIAL);
            std::fs::write(&path, "fake-token").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            crate::credentials::CredentialDirectory::from_path(dir.path())
                .owner_token()
                .unwrap()
        }
    }

    #[test]
    fn protocol_frames_match_teslamate() {
        let value: Value =
            serde_json::from_str(&subscribe_frame("9", "fake-token").unwrap()).unwrap();
        assert_eq!(value["msg_type"], "data:subscribe_oauth");
        assert_eq!(value["tag"], "9");
        assert_eq!(value["value"], TESLAMATE_STREAM_FIELDS.join(","));
        let unsubscribe: Value =
            serde_json::from_str(&unsubscribe_frame("9", "fake-token").unwrap()).unwrap();
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
            assert!(
                matches!(first,Message::Text(ref text) if text.contains("data:subscribe_oauth"))
            );
            let _ = ws
                .send(Message::Text(
                    r#"{"msg_type":"control:hello","code":200}"#.into(),
                ))
                .await;
            let second = ws.next().await.unwrap().unwrap();
            assert!(matches!(second,Message::Text(ref text) if text.contains("data:unsubscribe")));
        });
        let (events, _) = mpsc::channel(4);
        let supervisor = TeslaStreamSupervisor::new(
            crate::owner_api::VehicleId::from_test(9),
            token(),
            StreamRegion::Global,
            endpoint,
            events,
        )
        .unwrap();
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(supervisor.run(shutdown));
        stop.send(()).unwrap();
        timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
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
            let message = second.next().await.unwrap().unwrap();
            assert!(
                matches!(message, Message::Text(ref text) if text.contains("data:unsubscribe"))
            );
        });

        let (events, mut received) = mpsc::channel(4);
        let supervisor = TeslaStreamSupervisor::new(
            crate::owner_api::VehicleId::from_test(9),
            token(),
            StreamRegion::Global,
            endpoint,
            events,
        )
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
        let supervisor = TeslaStreamSupervisor::new(
            crate::owner_api::VehicleId::from_test(9),
            token(),
            StreamRegion::Global,
            endpoint,
            events,
        )
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
            Arc::new(Mutex::new(manager)),
            StreamRegion::Global,
            "ws://127.0.0.1:1/streaming/".to_owned(),
            client,
            events,
        )
        .unwrap();

        assert!(timeout(Duration::from_secs(1), supervisor.access_token())
            .await
            .unwrap()
            .is_err());
        server.await.unwrap();
    }

    #[test]
    fn parses_nested_stream_values() {
        let update=parse_data_update(r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#).unwrap();
        assert_eq!(update.speed, Some(42));
        assert_eq!(update.est_lat, Some(51.5));
    }
}
