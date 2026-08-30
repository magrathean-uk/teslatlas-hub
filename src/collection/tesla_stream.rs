// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    time::{Instant, sleep, timeout},
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        Error as WebSocketError, Message,
        protocol::{
            WebSocketConfig,
            frame::{CloseFrame, coding::CloseCode},
        },
    },
};
use url::Url;
use uuid::Uuid;

use crate::{
    credentials::{LegacyAuthManager, LegacyAuthManagerError},
    db::{
        HubStore, OutboundRequestCompletion, OutboundRequestOperation, OutboundRequestOutcome,
        OutboundRequestPrecondition, OutboundRequestReceiptId, OutboundRequestSafetyClass,
        OutboundRequestStart, OutboundRequestTransport, StoreError, StreamSessionReceiptId,
        StreamSessionTerminalOutcome,
    },
    owner_api::{StreamVehicleId, VehicleId},
};

pub const GLOBAL_STREAM_ENDPOINT: &str = "wss://streaming.vn.teslamotors.com/streaming/";
pub const CHINA_STREAM_ENDPOINT: &str = "wss://streaming.vn.cloud.tesla.cn/streaming/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SILENCE_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_READ_BUFFER_BYTES: usize = 64 * 1024;
const STREAM_WRITE_BUFFER_BYTES: usize = 64 * 1024;
const STREAM_MAX_WRITE_BUFFER_BYTES: usize = 256 * 1024;
const STREAM_MAX_FRAME_BYTES: usize = 64 * 1024;
const STREAM_MAX_MESSAGE_BYTES: usize = 256 * 1024;
const OVERSIZE_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// A peer can briefly lose the vehicle-side stream while keeping the WebSocket
/// alive. Resubscribe twice on that socket, then force a fresh transport. This
/// bounds a stale-socket loop without turning one ordinary disconnect into
/// connection churn.
const VEHICLE_DISCONNECTED_RECONNECT_LIMIT: u32 = 3;

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
    validate_endpoint_override_with_loopback_test_exception(value, false)
}

#[cfg(test)]
fn validate_test_endpoint_override(value: &str) -> Result<(), StreamError> {
    validate_endpoint_override_with_loopback_test_exception(value, true)
}

fn validate_endpoint_override_with_loopback_test_exception(
    value: &str,
    allow_loopback_plaintext: bool,
) -> Result<(), StreamError> {
    let url = Url::parse(value).map_err(|_| StreamError::InvalidEndpoint)?;
    if !matches!(url.scheme(), "ws" | "wss")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() == "ws"
            && (!allow_loopback_plaintext || !is_literal_loopback_host(url.host_str())))
    {
        return Err(StreamError::InvalidEndpoint);
    }
    Ok(())
}

fn is_literal_loopback_host(host: Option<&str>) -> bool {
    host.and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|address| address.is_loopback())
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
            StreamEvent::Telemetry { update, .. } => self
                .confirmed
                .store(update.power.is_some(), Ordering::Release),
            StreamEvent::VehicleOffline
            | StreamEvent::AuthRejected
            | StreamEvent::TransportUnavailable
            | StreamEvent::ProtocolViolation => self.revoke(),
            StreamEvent::Healthy => {}
        }
    }

    fn revoke(&self) {
        self.confirmed.store(false, Ordering::Release);
    }
}

/// Bounded reconnect delay for one stream supervisor.
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
        let delay = equal_jitter(self.current);
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
    audit_store: Option<HubStore>,
}

#[derive(Clone)]
enum StreamCredential {
    Legacy(Arc<Mutex<LegacyAuthManager>>),
}

enum StreamRunTermination {
    Orderly {
        unsubscribe_receipt_id: Option<OutboundRequestReceiptId>,
    },
    CancelledBeforeSubscription,
    TransportEnded,
}

/// Durable receipts for one supervisor lifetime. Explicit returns call
/// `finish`; task abort drops this value without rewriting the started session,
/// preserving evidence that teardown did not complete.
struct StreamSessionAudit {
    store: Option<HubStore>,
    session_id: Option<StreamSessionReceiptId>,
    correlation_id: Uuid,
    vehicle_tesla_id: i64,
}

impl StreamSessionAudit {
    fn start(
        store: Option<HubStore>,
        vehicle_id: VehicleId,
    ) -> Result<Self, StreamSupervisorError> {
        let vehicle_tesla_id = i64::try_from(vehicle_id.get())
            .map_err(|_| StoreError::InvalidOutboundRequestVehicleId)?;
        let correlation_id = Uuid::new_v4();
        let session_id = store
            .as_ref()
            .map(|store| store.begin_stream_session(correlation_id, vehicle_tesla_id))
            .transpose()?;
        Ok(Self {
            store,
            session_id,
            correlation_id,
            vehicle_tesla_id,
        })
    }

    fn begin_attempt(
        &self,
        operation: OutboundRequestOperation,
    ) -> Result<Option<OutboundRequestReceiptId>, StreamSupervisorError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        store
            .begin_outbound_request(&OutboundRequestStart {
                correlation_id: self.correlation_id,
                vehicle_tesla_id: Some(self.vehicle_tesla_id),
                transport: OutboundRequestTransport::Stream,
                operation,
                safety_class: OutboundRequestSafetyClass::NonWakeEndpoint,
                precondition: OutboundRequestPrecondition::NotRequired,
            })
            .map(Some)
            .map_err(StreamSupervisorError::Audit)
    }

    fn complete_attempt(
        &self,
        receipt_id: Option<OutboundRequestReceiptId>,
        outcome: OutboundRequestOutcome,
    ) -> Result<(), StreamSupervisorError> {
        let (Some(store), Some(receipt_id)) = (self.store.as_ref(), receipt_id) else {
            return Ok(());
        };
        store
            .complete_outbound_request(
                receipt_id,
                &OutboundRequestCompletion {
                    outcome,
                    http_status: None,
                    retry_after_seconds: None,
                },
            )
            .map_err(StreamSupervisorError::Audit)
    }

    fn finish(
        &mut self,
        termination: Result<&StreamRunTermination, ()>,
    ) -> Result<(), StreamSupervisorError> {
        let (Some(store), Some(session_id)) = (self.store.as_ref(), self.session_id) else {
            return Ok(());
        };
        let result = match termination {
            Ok(StreamRunTermination::Orderly {
                unsubscribe_receipt_id: Some(receipt_id),
            }) => store
                .complete_stream_session_orderly(session_id, *receipt_id)
                .map_err(StreamSupervisorError::Audit),
            Ok(StreamRunTermination::Orderly {
                unsubscribe_receipt_id: None,
            }) => Err(StreamSupervisorError::OrderlyShutdownUnavailable),
            Ok(StreamRunTermination::CancelledBeforeSubscription) => store
                .complete_stream_session_terminal(
                    session_id,
                    StreamSessionTerminalOutcome::CancelledBeforeSubscription,
                )
                .map_err(StreamSupervisorError::Audit),
            Ok(StreamRunTermination::TransportEnded) => store
                .complete_stream_session_terminal(
                    session_id,
                    StreamSessionTerminalOutcome::TransportEnded,
                )
                .map_err(StreamSupervisorError::Audit),
            Err(()) => store
                .complete_stream_session_terminal(session_id, StreamSessionTerminalOutcome::Failed)
                .map_err(StreamSupervisorError::Audit),
        };
        if result.is_ok() {
            self.session_id = None;
        }
        result
    }
}

enum EventDelivery {
    Delivered,
    Shutdown,
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
        let endpoint = endpoint_for_region(region, endpoint, allow_loopback_plaintext)?;
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
            audit_store,
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

    #[cfg(test)]
    pub(crate) fn with_audit_store(mut self, store: HubStore) -> Self {
        self.audit_store = Some(store);
        self
    }

    pub(crate) async fn run(
        self,
        mut shutdown: oneshot::Receiver<()>,
    ) -> Result<(), StreamSupervisorError> {
        let mut audit = match StreamSessionAudit::start(self.audit_store.clone(), self.vehicle_id) {
            Ok(audit) => audit,
            Err(error) => {
                if let Some(gate) = self.power_gate.as_ref() {
                    gate.revoke();
                }
                return Err(error);
            }
        };
        let result = self.run_until_termination(&mut shutdown, &audit).await;
        let audit_result = audit.finish(match &result {
            Ok(termination) => Ok(termination),
            Err(_) => Err(()),
        });
        if let Some(gate) = self.power_gate.as_ref() {
            gate.revoke();
        }
        audit_result?;
        result.map(|_| ())
    }

    async fn run_until_termination(
        &self,
        shutdown: &mut oneshot::Receiver<()>,
        audit: &StreamSessionAudit,
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
                self.begin_stream_attempt(audit, OutboundRequestOperation::StreamConnect)?;
            if let Err(error) = self.assert_sensitive_access().await {
                self.complete_stream_attempt(
                    audit,
                    connect_receipt,
                    OutboundRequestOutcome::AuthenticationRejected,
                )?;
                return Err(error);
            }
            let connection = timeout(
                self.policy.connect_timeout,
                connect_async_with_config(endpoint, Some(stream_socket_config()), false),
            )
            .await;
            let (mut socket, _) = match connection {
                Ok(Ok(value)) => {
                    self.complete_stream_attempt(
                        audit,
                        connect_receipt,
                        OutboundRequestOutcome::Success,
                    )?;
                    value
                }
                Ok(Err(_)) | Err(_) => {
                    self.complete_stream_attempt(
                        audit,
                        connect_receipt,
                        OutboundRequestOutcome::TransportError,
                    )?;
                    tracing::warn!(
                        vehicle_id = self.vehicle_id.get(),
                        reason = "connect_failed",
                        recovery = "reconnect_with_backoff",
                        "Tesla stream transport unavailable"
                    );
                    if self
                        .emit_event(StreamEvent::TransportUnavailable, shutdown)
                        .await?
                        .is_shutdown()
                    {
                        return Ok(disconnected_termination(ever_subscribed));
                    }
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
                    tracing::warn!(
                        vehicle_id = self.vehicle_id.get(),
                        reason = "access_token_temporarily_unavailable",
                        recovery = "retry_with_backoff",
                        "Tesla stream transport unavailable"
                    );
                    if self
                        .emit_event(StreamEvent::TransportUnavailable, shutdown)
                        .await?
                        .is_shutdown()
                    {
                        return Ok(disconnected_termination(ever_subscribed));
                    }
                    if wait_or_shutdown(connect_backoff.next(), shutdown).await {
                        return Ok(disconnected_termination(ever_subscribed));
                    }
                    continue;
                }
            };
            let subscribe = subscribe_frame(&self.tag, &access_token)
                .map_err(StreamSupervisorError::InvalidEndpoint)?;
            let subscribe_receipt =
                self.begin_stream_attempt(audit, OutboundRequestOperation::StreamSubscribe)?;
            if let Err(error) = self.assert_sensitive_access().await {
                self.complete_stream_attempt(
                    audit,
                    subscribe_receipt,
                    OutboundRequestOutcome::AuthenticationRejected,
                )?;
                let _ = socket.close(None).await;
                return Err(error);
            }
            if socket.send(Message::Text(subscribe.into())).await.is_err() {
                self.complete_stream_attempt(
                    audit,
                    subscribe_receipt,
                    OutboundRequestOutcome::TransportError,
                )?;
                tracing::warn!(
                    vehicle_id = self.vehicle_id.get(),
                    reason = "subscribe_send_failed",
                    recovery = "reconnect_with_backoff",
                    "Tesla stream transport unavailable"
                );
                if self
                    .emit_event(StreamEvent::TransportUnavailable, shutdown)
                    .await?
                    .is_shutdown()
                {
                    return Ok(disconnected_termination(ever_subscribed));
                }
                if wait_or_shutdown(connect_backoff.next(), shutdown).await {
                    return Ok(disconnected_termination(ever_subscribed));
                }
                continue;
            }

            let mut subscribed = false;
            let mut subscribe_receipt = Some(subscribe_receipt);
            let mut consecutive_vehicle_disconnects = 0_u32;
            let mut silence = Box::pin(sleep(self.policy.silence_timeout));
            let clean = loop {
                tokio::select! {
                    _ = &mut *shutdown => break true,
                    _ = &mut silence => {
                        if let Some(receipt) = subscribe_receipt.take() {
                            self.complete_stream_attempt(
                                audit,
                                receipt,
                                OutboundRequestOutcome::TransportError,
                            )?;
                        }
                        tracing::warn!(
                            vehicle_id = self.vehicle_id.get(),
                            reason = "stream_silence_timeout",
                            silence_timeout_ms = u64::try_from(self.policy.silence_timeout.as_millis())
                                .unwrap_or(u64::MAX),
                            recovery = "reconnect_socket",
                            "Tesla stream transport unavailable"
                        );
                        if self.emit_event(StreamEvent::TransportUnavailable, shutdown).await?.is_shutdown() {
                            return Ok(disconnected_termination(ever_subscribed));
                        }
                        let _ = socket.close(None).await;
                        break false;
                    }
                    frame = socket.next() => match frame {
                        Some(Ok(message)) => {
                            let control_hello = is_control_hello_message(&message);
                            let data_error = data_error_category_for_message(&self.tag, &message);
                            let Some(event) = decode_message(&self.tag, message) else {
                                continue;
                            };
                            // Tesla's normal legacy hello carries
                            // `connection_timeout: 0`. It proves only that the
                            // socket is alive; subscription/authentication is
                            // proven by matching telemetry.
                            if control_hello && matches!(event, StreamEvent::Healthy) {
                                silence
                                    .as_mut()
                                    .reset(tokio::time::Instant::now() + self.policy.silence_timeout);
                                continue;
                            }
                            if apply_health_backoff_reset(
                                Some(&event),
                                &mut remote_backoff,
                                &mut connect_backoff,
                            ) {
                                consecutive_vehicle_disconnects = 0;
                                subscribed = true;
                                ever_subscribed = true;
                                // A raw WebSocket handshake proves transport
                                // only. Tesla's acknowledgement or valid
                                // telemetry proves a healthy stream.
                                if let Some(receipt) = subscribe_receipt.take() {
                                    self.complete_stream_attempt(
                                        audit,
                                        receipt,
                                        OutboundRequestOutcome::Success,
                                    )?;
                                }
                            }
                            let valid_after_handshake = subscribed
                                && matches!(event, StreamEvent::Telemetry { .. });
                            let terminal = matches!(event, StreamEvent::AuthRejected | StreamEvent::ProtocolViolation);
                            let should_reconnect = matches!(event, StreamEvent::TransportUnavailable);
                            if let Some(receipt) = subscribe_receipt.take() {
                                let outcome = if matches!(event, StreamEvent::AuthRejected) {
                                    OutboundRequestOutcome::AuthenticationRejected
                                } else if matches!(event, StreamEvent::ProtocolViolation) {
                                    OutboundRequestOutcome::ProtocolError
                                } else if data_error.is_some()
                                    || matches!(event, StreamEvent::TransportUnavailable)
                                {
                                    OutboundRequestOutcome::TransportError
                                } else {
                                    OutboundRequestOutcome::Success
                                };
                                self.complete_stream_attempt(audit, receipt, outcome)?;
                            }
                            if self.emit_event(event, shutdown).await?.is_shutdown() {
                                return Ok(disconnected_termination(ever_subscribed));
                            }
                            if terminal { break false; }
                            if matches!(data_error, Some(DataErrorCategory::VehicleDisconnected)) {
                                consecutive_vehicle_disconnects =
                                    consecutive_vehicle_disconnects.saturating_add(1);
                                let force_reconnect = consecutive_vehicle_disconnects
                                    >= VEHICLE_DISCONNECTED_RECONNECT_LIMIT;
                                tracing::warn!(
                                    vehicle_id = self.vehicle_id.get(),
                                    consecutive_disconnects = consecutive_vehicle_disconnects,
                                    recovery = if force_reconnect {
                                        "reconnect_socket"
                                    } else {
                                        "resubscribe_same_socket"
                                    },
                                    "Tesla stream vehicle disconnected; recovery scheduled"
                                );
                                if force_reconnect {
                                    let _ = socket.close(None).await;
                                    break false;
                                }
                                if wait_or_shutdown(remote_backoff.next(), shutdown).await {
                                    return Ok(disconnected_termination(ever_subscribed));
                                }
                                if let Err(error) = self.assert_sensitive_access().await {
                                    let _ = socket.close(None).await;
                                    return Err(error);
                                }
                                let access_token = match self.access_token().await {
                                    Ok(token) => token,
                                    Err(AccessTokenError::AuthorityUnavailable) => {
                                        let _ = socket.close(None).await;
                                        return Err(
                                            StreamSupervisorError::CredentialAuthorityUnavailable,
                                        );
                                    }
                                    Err(AccessTokenError::Unavailable) => {
                                        let _ = socket.close(None).await;
                                        break false;
                                    }
                                };
                                let subscribe = subscribe_frame(&self.tag, &access_token)
                                    .map_err(StreamSupervisorError::InvalidEndpoint)?;
                                let receipt = self.begin_stream_attempt(
                                    audit,
                                    OutboundRequestOperation::StreamSubscribe,
                                )?;
                                if let Err(error) = self.assert_sensitive_access().await {
                                    self.complete_stream_attempt(
                                        audit,
                                        receipt,
                                        OutboundRequestOutcome::AuthenticationRejected,
                                    )?;
                                    let _ = socket.close(None).await;
                                    return Err(error);
                                }
                                if socket.send(Message::Text(subscribe.into())).await.is_err() {
                                    self.complete_stream_attempt(
                                        audit,
                                        receipt,
                                        OutboundRequestOutcome::TransportError,
                                    )?;
                                    tracing::warn!(
                                        vehicle_id = self.vehicle_id.get(),
                                        reason = "resubscribe_send_failed",
                                        recovery = "reconnect_socket",
                                        "Tesla stream transport unavailable"
                                    );
                                    let _ = socket.close(None).await;
                                    break false;
                                }
                                subscribe_receipt = Some(receipt);
                                subscribed = false;
                                silence
                                    .as_mut()
                                    .reset(tokio::time::Instant::now() + self.policy.silence_timeout);
                                continue;
                            }
                            if matches!(
                                data_error,
                                Some(DataErrorCategory::VehicleOffline | DataErrorCategory::Other)
                            ) {
                                // TeslaMate keeps the socket after ordinary
                                // vehicle/unknown errors. Any valid peer frame
                                // also renews its receive timeout.
                                silence
                                    .as_mut()
                                    .reset(tokio::time::Instant::now() + self.policy.silence_timeout);
                                continue;
                            }
                            if should_reconnect { break false; }
                            if !valid_after_handshake {
                                continue;
                            }
                            silence
                                .as_mut()
                                .reset(tokio::time::Instant::now() + self.policy.silence_timeout);
                        }
                        Some(Err(WebSocketError::Capacity(_))) => {
                            if let Some(receipt) = subscribe_receipt.take() {
                                self.complete_stream_attempt(
                                    audit,
                                    receipt,
                                    OutboundRequestOutcome::ProtocolError,
                                )?;
                            }
                            // Tungstenite reports a received size violation
                            // without initiating a closing handshake. Send the
                            // required 1009 frame ourselves, wait only a
                            // bounded interval for its write, and stop this
                            // supervisor rather than reconnecting to a peer
                            // that just violated our finite wire contract.
                            let close = CloseFrame {
                                code: CloseCode::Size,
                                reason: "message exceeds Hub stream limit".into(),
                            };
                            let _ = timeout(OVERSIZE_CLOSE_TIMEOUT, socket.close(Some(close))).await;
                            if self.emit_event(StreamEvent::ProtocolViolation, shutdown).await?.is_shutdown() {
                                return Ok(disconnected_termination(ever_subscribed));
                            }
                            return Err(StreamSupervisorError::ProtocolViolation);
                        }
                        Some(Err(_)) | None => {
                            if let Some(receipt) = subscribe_receipt.take() {
                                self.complete_stream_attempt(
                                    audit,
                                    receipt,
                                    OutboundRequestOutcome::TransportError,
                                )?;
                            }
                            tracing::warn!(
                                vehicle_id = self.vehicle_id.get(),
                                reason = "socket_closed_or_read_failed",
                                recovery = "reconnect_with_backoff",
                                "Tesla stream transport unavailable"
                            );
                            if self.emit_event(StreamEvent::TransportUnavailable, shutdown).await?.is_shutdown() {
                                return Ok(disconnected_termination(ever_subscribed));
                            }
                            break false;
                        }
                    }
                }
            };

            if clean {
                if let Some(receipt) = subscribe_receipt.take() {
                    self.complete_stream_attempt(
                        audit,
                        receipt,
                        OutboundRequestOutcome::Cancelled,
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
                let receipt =
                    self.begin_stream_attempt(audit, OutboundRequestOperation::StreamUnsubscribe)?;
                if let Err(error) = self.assert_sensitive_access().await {
                    self.complete_stream_attempt(
                        audit,
                        receipt,
                        OutboundRequestOutcome::AuthenticationRejected,
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
                        audit,
                        receipt,
                        OutboundRequestOutcome::TransportError,
                    )?;
                    return Err(StreamSupervisorError::OrderlyShutdownUnavailable);
                }
                self.complete_stream_attempt(audit, receipt, OutboundRequestOutcome::Success)?;
                let _ = socket.close(None).await;
                return Ok(StreamRunTermination::Orderly {
                    unsubscribe_receipt_id: receipt,
                });
            }
            if wait_or_shutdown(remote_backoff.next(), shutdown).await {
                return Ok(disconnected_termination(ever_subscribed));
            }
        }
    }

    async fn emit_event(
        &self,
        event: StreamEvent,
        shutdown: &mut oneshot::Receiver<()>,
    ) -> Result<EventDelivery, StreamSupervisorError> {
        if let Some(gate) = self.power_gate.as_ref() {
            gate.observe(&event);
        }
        tokio::select! {
            biased;
            _ = &mut *shutdown => Ok(EventDelivery::Shutdown),
            result = self.events.send(event) => result.map(|_| EventDelivery::Delivered).map_err(|_| StreamSupervisorError::EventReceiverClosed),
        }
    }

    fn begin_stream_attempt(
        &self,
        audit: &StreamSessionAudit,
        operation: OutboundRequestOperation,
    ) -> Result<Option<OutboundRequestReceiptId>, StreamSupervisorError> {
        audit.begin_attempt(operation)
    }

    fn complete_stream_attempt(
        &self,
        audit: &StreamSessionAudit,
        receipt: Option<OutboundRequestReceiptId>,
        outcome: OutboundRequestOutcome,
    ) -> Result<(), StreamSupervisorError> {
        audit.complete_attempt(receipt, outcome)
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

fn endpoint_for_region(
    region: StreamRegion,
    endpoint: String,
    allow_loopback_plaintext: bool,
) -> Result<String, StreamError> {
    let endpoint = if endpoint == GLOBAL_STREAM_ENDPOINT || endpoint == CHINA_STREAM_ENDPOINT {
        streaming_endpoint(region).to_owned()
    } else {
        endpoint
    };
    if allow_loopback_plaintext {
        #[cfg(test)]
        validate_test_endpoint_override(&endpoint)?;
        #[cfg(not(test))]
        validate_endpoint_override(&endpoint)?;
    } else {
        validate_endpoint_override(&endpoint)?;
    }
    Ok(endpoint)
}

fn stream_socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(STREAM_READ_BUFFER_BYTES)
        .write_buffer_size(STREAM_WRITE_BUFFER_BYTES)
        .max_write_buffer_size(STREAM_MAX_WRITE_BUFFER_BYTES)
        .max_message_size(Some(STREAM_MAX_MESSAGE_BYTES))
        .max_frame_size(Some(STREAM_MAX_FRAME_BYTES))
}

fn equal_jitter(delay: Duration) -> Duration {
    let upper = delay.as_nanos().min(u128::from(u64::MAX));
    let lower = upper / 2;
    let width = upper.saturating_sub(lower);
    let mut bytes = [0_u8; 8];
    let random = if getrandom::fill(&mut bytes).is_ok() {
        u64::from_le_bytes(bytes) as u128
    } else {
        0
    };
    Duration::from_nanos(lower.saturating_add(random % width.saturating_add(1)) as u64)
}

fn resets_backoff(event: &StreamEvent) -> bool {
    matches!(event, StreamEvent::Telemetry { .. })
}

fn apply_health_backoff_reset(
    event: Option<&StreamEvent>,
    remote_backoff: &mut Backoff,
    connect_backoff: &mut Backoff,
) -> bool {
    if !event.is_some_and(resets_backoff) {
        return false;
    }
    remote_backoff.reset();
    connect_backoff.reset();
    true
}

impl EventDelivery {
    fn is_shutdown(&self) -> bool {
        matches!(self, Self::Shutdown)
    }
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
    match message {
        Message::Text(text) => serde_json::from_str::<StreamWire<'_>>(&text)
            .ok()
            .and_then(|wire| decode_wire(tag, wire)),
        Message::Binary(bytes) => {
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return Some(StreamEvent::ProtocolViolation);
            };
            serde_json::from_str::<StreamWire<'_>>(text)
                .ok()
                .and_then(|wire| decode_wire(tag, wire))
        }
        _ => None,
    }
}

fn stream_wire(message: &Message) -> Option<StreamWire<'_>> {
    let text = match message {
        Message::Text(text) => text.as_ref(),
        Message::Binary(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return None,
        },
        _ => return None,
    };
    serde_json::from_str(text).ok()
}

fn is_control_hello_message(message: &Message) -> bool {
    stream_wire(message).is_some_and(|wire| wire.msg_type == "control:hello")
}

fn data_error_category_for_message(tag: &str, message: &Message) -> Option<DataErrorCategory> {
    stream_wire(message).and_then(|wire| {
        (wire.msg_type == "data:error" && wire.tag.is_none_or(|frame_tag| frame_tag == tag))
            .then(|| classify_data_error(wire.error_type, wire.value))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataErrorCategory {
    VehicleDisconnected,
    VehicleOffline,
    TokenRejected,
    OwnerApiError,
    ClientError,
    Other,
}

impl DataErrorCategory {
    const fn as_str(self) -> &'static str {
        match self {
            Self::VehicleDisconnected => "vehicle_disconnected",
            Self::VehicleOffline => "vehicle_offline",
            Self::TokenRejected => "token_rejected",
            Self::OwnerApiError => "owner_api_error",
            Self::ClientError => "client_error",
            Self::Other => "other",
        }
    }
}

fn classify_data_error(error_type: Option<&str>, value: Option<&str>) -> DataErrorCategory {
    let error_type_is =
        |expected: &str| error_type.is_some_and(|kind| kind.eq_ignore_ascii_case(expected));
    let value_has =
        |needle: &str| value.is_some_and(|detail| ascii_contains_ignore_case(detail, needle));
    if error_type_is("client_error") && value_has("validate token") {
        DataErrorCategory::TokenRejected
    } else if error_type_is("client_error")
        && value.is_some_and(|detail| {
            detail
                .get(.."owner_api error:".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("owner_api error:"))
        })
    {
        DataErrorCategory::OwnerApiError
    } else if error_type_is("client_error") {
        DataErrorCategory::ClientError
    } else if error_type_is("vehicle_disconnected") {
        DataErrorCategory::VehicleDisconnected
    } else if error_type_is("vehicle_error")
        && (value_has("vehicle offline")
            || value_has("vehicle is offline")
            || value_has("vehicle_offline"))
    {
        DataErrorCategory::VehicleOffline
    } else {
        DataErrorCategory::Other
    }
}

fn ascii_contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    !needle.is_empty()
        && haystack.as_bytes().windows(needle.len()).any(|window| {
            window
                .iter()
                .zip(needle)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
        })
}

fn redacted_error_type(error_type: Option<&str>) -> String {
    let Some(error_type) = error_type else {
        return "<missing>".into();
    };
    let mut redacted = String::new();
    for character in error_type.chars().take(64) {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
            redacted.push(character);
        } else {
            redacted.push('_');
        }
    }
    if redacted.is_empty() {
        "<empty>".into()
    } else {
        redacted
    }
}

fn decode_wire(tag: &str, wire: StreamWire<'_>) -> Option<StreamEvent> {
    match wire.msg_type {
        "control:hello" => Some(decode_control_hello(wire.code, wire.connection_timeout)),
        "data:update" if wire.tag != Some(tag) => None,
        "data:update" => parse_data_update_parts(tag, wire.timestamp, wire.value?)
            .ok()
            .map(|update| StreamEvent::Telemetry {
                update: Box::new(update),
                queued_at: Instant::now(),
            }),
        "data:error" if wire.tag.is_some_and(|frame_tag| frame_tag != tag) => None,
        "data:error" => {
            let category = classify_data_error(wire.error_type, wire.value);
            tracing::warn!(
                error_type = %redacted_error_type(wire.error_type),
                category = category.as_str(),
                "Tesla stream data:error"
            );
            match category {
                DataErrorCategory::VehicleOffline => Some(StreamEvent::VehicleOffline),
                DataErrorCategory::TokenRejected => Some(StreamEvent::AuthRejected),
                DataErrorCategory::VehicleDisconnected
                | DataErrorCategory::OwnerApiError
                | DataErrorCategory::ClientError
                | DataErrorCategory::Other => Some(StreamEvent::TransportUnavailable),
            }
        }
        _ => None,
    }
}

#[derive(Deserialize)]
struct StreamWire<'a> {
    #[serde(borrow)]
    msg_type: &'a str,
    #[serde(borrow, default)]
    tag: Option<&'a str>,
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(borrow, default)]
    value: Option<&'a str>,
    #[serde(borrow, default)]
    error_type: Option<&'a str>,
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    connection_timeout: Option<i64>,
}

fn decode_control_hello(code: Option<i64>, connection_timeout: Option<i64>) -> StreamEvent {
    match code {
        Some(code) => match code {
            200 => StreamEvent::Healthy,
            401 | 403 => StreamEvent::AuthRejected,
            _ => StreamEvent::TransportUnavailable,
        },
        None if connection_timeout.is_some_and(|timeout| timeout >= 0) => StreamEvent::Healthy,
        None => StreamEvent::TransportUnavailable,
    }
}

pub fn parse_data_update(frame: &str) -> Result<StreamUpdate, StreamError> {
    let wire: StreamWire<'_> =
        serde_json::from_str(frame).map_err(|_| StreamError::MalformedDataUpdate)?;
    if wire.msg_type != "data:update" {
        return Err(StreamError::WrongMessageType);
    }
    parse_data_update_parts(
        wire.tag.ok_or(StreamError::MalformedDataUpdate)?,
        wire.timestamp,
        wire.value.ok_or(StreamError::MalformedDataUpdate)?,
    )
}

fn parse_data_update_parts(
    tag: &str,
    timestamp: Option<i64>,
    value: &str,
) -> Result<StreamUpdate, StreamError> {
    validate_tag(tag)?;
    const MAX_FIELD_BYTES: usize = 64;
    let mut parts = value.split(',');
    let timestamp_ms = match timestamp {
        Some(timestamp) if timestamp > 0 => timestamp,
        Some(_) => return Err(StreamError::InvalidTimestamp),
        None => bounded_next_field(&mut parts, MAX_FIELD_BYTES)?
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|timestamp| *timestamp > 0)
            .ok_or(StreamError::InvalidTimestamp)?,
    };
    let mut values = [""; TESLAMATE_STREAM_FIELDS.len()];
    for field in &mut values {
        *field = bounded_next_field(&mut parts, MAX_FIELD_BYTES)?;
    }
    if parts.next().is_some() {
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
        shift_state: optional_state(values[8])?,
        range: optional_i64(values[9])?,
        est_range: optional_i64(values[10])?,
        heading: optional_i64(values[11])?,
    })
}

fn bounded_next_field<'a>(
    parts: &mut std::str::Split<'a, char>,
    maximum: usize,
) -> Result<&'a str, StreamError> {
    let field = parts.next().ok_or(StreamError::MalformedDataUpdate)?;
    (field.len() <= maximum)
        .then_some(field)
        .ok_or(StreamError::InvalidField)
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
fn optional_state(value: &str) -> Result<Option<String>, StreamError> {
    let value = value.trim();
    if value.len() > 32 || value.chars().any(char::is_control) {
        return Err(StreamError::InvalidField);
    }
    Ok((!value.is_empty()).then(|| value.to_owned()))
}

#[cfg(test)]
#[path = "tesla_stream/tests.rs"]
mod tests;
