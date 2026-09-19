// SPDX-License-Identifier: AGPL-3.0-only

//! Bounded Edge delivery-v2 pull, durable application, and contiguous ACK.

use std::{
    collections::HashSet,
    error::Error as _,
    fs,
    io::{ErrorKind, Read},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use reqwest::{Client, StatusCode, redirect::Policy};
use rustix::{
    fs::{FileType, Mode, OFlags, fstat, open},
    process::getuid,
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeOwned, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
#[cfg(unix)]
use tokio::sync::oneshot;

use crate::{
    config::EdgeCollectorConfig,
    db::{
        EdgeAcceptance, EdgeBinding, HubStore, StoreError, VerifiedEdgeGap, VerifiedEdgeItem,
        VerifiedEdgeRecord,
    },
};

const MAX_BATCH_RESPONSE_BYTES: usize = 4_195_441;
const MAX_BATCH_ITEM_BYTES: usize = 4_194_304;
const MAX_BATCH_ITEMS: usize = 1_024;
const MAX_ACK_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_PEM_BYTES: usize = 128 * 1024;
const MAX_BEARER_BYTES: usize = 4 * 1024;

fn transport_diagnostic_code(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        return "timeout";
    }
    let mut cause = error.source();
    while let Some(value) = cause {
        if let Some(io_error) = value.downcast_ref::<std::io::Error>() {
            match io_error.kind() {
                ErrorKind::ConnectionRefused => return "connect_refused",
                _ => {}
            }
        }
        let text = value.to_string().to_ascii_lowercase();
        if text.contains("certificate") || text.contains("tls") {
            return "tls";
        }
        cause = value.source();
    }
    if error.is_connect() {
        "connect"
    } else if error.is_body() {
        "body"
    } else if error.is_request() {
        "request"
    } else {
        "other"
    }
}

fn transport_failure(error: reqwest::Error) -> EdgeDeliveryError {
    tracing::warn!(
        edge_transport_code = transport_diagnostic_code(&error),
        "Edge delivery transport failed"
    );
    EdgeDeliveryError::Transport
}

#[derive(Debug, Error)]
pub enum EdgeDeliveryError {
    #[error("Edge credential or trust file is unsafe or unreadable")]
    UnsafeCredential,
    #[error("Edge TLS identity is invalid")]
    InvalidTlsIdentity,
    #[error("Edge HTTP client could not be created")]
    Client,
    #[error("Edge transport failed")]
    Transport,
    #[error("Edge rejected authentication")]
    Authentication,
    #[error("Edge response exceeded a fixed limit")]
    ResponseTooLarge,
    #[error("Edge response is invalid")]
    InvalidResponse,
    #[error("Edge delivery identity is invalid")]
    InvalidIdentity,
    #[error("Edge durable acceptance failed: {0}")]
    Store(#[from] StoreError),
    #[error("system clock is invalid")]
    Clock,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    version: u16,
    batch_id: String,
    records: Vec<BatchRecord>,
    gaps: Vec<BatchGap>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BatchRecord {
    record_id: String,
    legacy_record_id: String,
    spool_seq: u64,
    received_at_ms: i64,
    envelope: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BatchGap {
    notice_id: String,
    spool_seq: u64,
    occurred_at_ms: i64,
    reason: String,
    evidence_sha256: String,
}

#[derive(Debug, Serialize)]
struct AckRequest<'a> {
    version: u16,
    batch_id: &'a str,
    accepted_record_ids: Vec<&'a str>,
    accepted_gap_notice_ids: Vec<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckResponse {
    version: u16,
    acknowledged_record_ids: Vec<String>,
    acknowledged_gap_notice_ids: Vec<String>,
    unknown_record_ids: Vec<String>,
    unknown_gap_notice_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgePollReport {
    pub batch_id: String,
    pub accepted: Vec<EdgeAcceptance>,
}

pub struct EdgeConsumer {
    client: Client,
    base_url: String,
    bearer: String,
    binding: EdgeBinding,
    poll_interval: Duration,
    maximum_backoff: Duration,
    offline_drive_timeout: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublicationRecovery {
    None,
    Completed,
    Pending,
}

impl EdgeConsumer {
    pub fn from_config(
        config: &EdgeCollectorConfig,
        offline_drive_timeout: Duration,
    ) -> Result<Self, EdgeDeliveryError> {
        crate::crypto::install_default_provider();
        let ca = read_bounded_file(&config.ca_certificate_path, MAX_PEM_BYTES, false)?;
        let certificate = read_bounded_file(&config.client_certificate_path, MAX_PEM_BYTES, false)?;
        let private_key = read_bounded_file(&config.client_private_key_path, MAX_PEM_BYTES, true)?;
        let bearer = read_bounded_file(&config.bearer_token_path, MAX_BEARER_BYTES, true)?;
        let bearer = std::str::from_utf8(&bearer)
            .ok()
            .map(str::trim)
            .filter(|value| value.starts_with("tte1.") && value.len() <= MAX_BEARER_BYTES)
            .ok_or(EdgeDeliveryError::UnsafeCredential)?
            .to_owned();
        let mut identity_pem = certificate;
        identity_pem.extend_from_slice(b"\n");
        identity_pem.extend_from_slice(&private_key);
        let identity = reqwest::Identity::from_pem(&identity_pem)
            .map_err(|_| EdgeDeliveryError::InvalidTlsIdentity)?;
        let root = reqwest::Certificate::from_pem(&ca)
            .map_err(|_| EdgeDeliveryError::InvalidTlsIdentity)?;
        let client = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .tls_certs_only([root])
            .identity(identity)
            .connect_timeout(Duration::from_secs(config.timeout_seconds))
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .map_err(|_| EdgeDeliveryError::Client)?;
        Ok(Self {
            client,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            bearer,
            binding: EdgeBinding {
                installation_id: config.installation_id.clone(),
                lineage: config.lineage.clone(),
                source_id: config.source_id,
                vehicle_id: config.vehicle_id,
                vin: config.vin.clone(),
                car_id: config.car_id,
            },
            poll_interval: Duration::from_millis(config.poll_milliseconds),
            maximum_backoff: Duration::from_secs(config.max_backoff_seconds),
            offline_drive_timeout,
        })
    }

    pub async fn poll_once(&self, store: &HubStore) -> Result<EdgePollReport, EdgeDeliveryError> {
        let response = self
            .client
            .get(format!("{}/v2/hub/batches/next", self.base_url))
            .query(&[("max_items", "256"), ("max_bytes", "1048576")])
            .header("authorization", format!("Bearer {}", self.bearer))
            .header("accept", "application/json")
            .header("accept-encoding", "identity")
            .send()
            .await
            .map_err(transport_failure)?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(EdgeDeliveryError::Authentication);
        }
        if response.status() != StatusCode::OK {
            tracing::warn!(
                edge_transport_code = "unexpected_status",
                "Edge delivery transport failed"
            );
            return Err(EdgeDeliveryError::Transport);
        }
        let body = bounded_body(response, MAX_BATCH_RESPONSE_BYTES).await?;
        let batch: Batch = parse_unique_json(&body)?;
        validate_batch(&batch)?;
        if batch.records.is_empty() && batch.gaps.is_empty() {
            return Ok(EdgePollReport {
                batch_id: batch.batch_id,
                accepted: Vec::new(),
            });
        }

        enum Ref<'a> {
            Record(&'a BatchRecord),
            Gap(&'a BatchGap),
        }
        let mut merged = batch
            .records
            .iter()
            .map(|value| (value.spool_seq, Ref::Record(value)))
            .chain(
                batch
                    .gaps
                    .iter()
                    .map(|value| (value.spool_seq, Ref::Gap(value))),
            )
            .collect::<Vec<_>>();
        merged.sort_by_key(|(sequence, _)| *sequence);
        let mut accepted = Vec::with_capacity(merged.len());
        for (_, item) in merged {
            let verified = match item {
                Ref::Record(record) => verify_record(record, &self.binding.vin)?,
                Ref::Gap(gap) => verify_gap(gap)?,
            };
            match store.accept_verified_edge_item(
                &self.binding,
                &verified,
                now_ms()?,
                self.offline_drive_timeout,
            ) {
                Ok(result) => accepted.push(result),
                Err(error) if accepted.is_empty() => return Err(error.into()),
                Err(_) => break,
            }
        }
        if !accepted.is_empty() {
            #[cfg(feature = "edge-test-faults")]
            crate::edge_test_fault::abort_at("after_accept_commit_before_ack");
            self.acknowledge(&batch, &accepted).await?;
        }
        Ok(EdgePollReport {
            batch_id: batch.batch_id,
            accepted,
        })
    }

    async fn acknowledge(
        &self,
        batch: &Batch,
        accepted: &[EdgeAcceptance],
    ) -> Result<(), EdgeDeliveryError> {
        let accepted_ids = accepted
            .iter()
            .map(|item| item.item_id.as_str())
            .collect::<HashSet<_>>();
        let request = AckRequest {
            version: 2,
            batch_id: &batch.batch_id,
            accepted_record_ids: batch
                .records
                .iter()
                .filter(|item| accepted_ids.contains(item.record_id.as_str()))
                .map(|item| item.record_id.as_str())
                .collect(),
            accepted_gap_notice_ids: batch
                .gaps
                .iter()
                .filter(|item| accepted_ids.contains(item.notice_id.as_str()))
                .map(|item| item.notice_id.as_str())
                .collect(),
        };
        let response = self
            .client
            .post(format!("{}/v2/hub/acks", self.base_url))
            .header("authorization", format!("Bearer {}", self.bearer))
            .header("accept", "application/json")
            .header("accept-encoding", "identity")
            .json(&request)
            .send()
            .await
            .map_err(transport_failure)?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(EdgeDeliveryError::Authentication);
        }
        if response.status() != StatusCode::OK {
            tracing::warn!(
                edge_transport_code = "unexpected_status",
                "Edge delivery transport failed"
            );
            return Err(EdgeDeliveryError::Transport);
        }
        #[cfg(feature = "edge-test-faults")]
        crate::edge_test_fault::abort_at("after_ack_accepted_before_response");
        let body = bounded_body(response, MAX_ACK_RESPONSE_BYTES).await?;
        let result: AckResponse = parse_unique_json(&body)?;
        if result.version != 2
            || !result.unknown_record_ids.is_empty()
            || !result.unknown_gap_notice_ids.is_empty()
            || result.acknowledged_record_ids != request.accepted_record_ids
            || result.acknowledged_gap_notice_ids != request.accepted_gap_notice_ids
        {
            return Err(EdgeDeliveryError::InvalidResponse);
        }
        Ok(())
    }

    pub async fn run_until<F>(
        &self,
        store: &HubStore,
        cursor_key: &crate::protocol::CursorKey,
        shutdown: F,
    ) -> Result<(), EdgeDeliveryError>
    where
        F: std::future::Future<Output = ()>,
    {
        tokio::pin!(shutdown);
        let mut backoff = self.poll_interval;
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    let _ = store.set_edge_diagnostic(&self.binding, "stopped", "shutdown", now_ms()?);
                    return Ok(());
                }
                _ = std::future::ready(()) => {}
            }
            let publication = self.recover_pending_publications(store, cursor_key)?;
            tokio::select! {
                _ = &mut shutdown => {
                    let _ = store.set_edge_diagnostic(&self.binding, "stopped", "shutdown", now_ms()?);
                    return Ok(());
                }
                result = self.poll_once(store) => match result {
                    Ok(_) => {
                        backoff = self.poll_interval;
                        match publication {
                            PublicationRecovery::Pending => {}
                            PublicationRecovery::Completed => store.set_edge_diagnostic(
                                &self.binding,
                                "connected",
                                "edge publication recovered and delivery poll completed",
                                now_ms()?,
                            )?,
                            PublicationRecovery::None => store.set_edge_diagnostic(
                                &self.binding,
                                "connected",
                                "edge delivery poll completed",
                                now_ms()?,
                            )?,
                        }
                    }
                    Err(EdgeDeliveryError::Authentication) => {
                        store.set_edge_diagnostic(&self.binding, "auth_failed", "edge authentication rejected", now_ms()?)?;
                        backoff = (backoff * 2).min(self.maximum_backoff);
                    }
                    Err(error @ (EdgeDeliveryError::InvalidIdentity | EdgeDeliveryError::InvalidResponse | EdgeDeliveryError::Store(_))) => {
                        store.set_edge_diagnostic(&self.binding, "blocked_record", &error.to_string(), now_ms()?)?;
                        backoff = (backoff * 2).min(self.maximum_backoff);
                    }
                    Err(_) => {
                        store.set_edge_diagnostic(&self.binding, "backoff", "edge transport unavailable", now_ms()?)?;
                        backoff = (backoff * 2).min(self.maximum_backoff);
                    }
                }
            }
            tokio::select! { _ = &mut shutdown => return Ok(()), _ = tokio::time::sleep(backoff) => {} }
        }
    }

    fn recover_pending_publications(
        &self,
        store: &HubStore,
        cursor_key: &crate::protocol::CursorKey,
    ) -> Result<PublicationRecovery, EdgeDeliveryError> {
        if !store.edge_has_pending_publications(&self.binding)? {
            return Ok(PublicationRecovery::None);
        }
        let now = now_ms()?;
        match crate::collector::publish_edge_pending_mutations(
            store,
            cursor_key,
            self.binding.vehicle_id,
            now,
        ) {
            Ok(_) => {
                store.complete_edge_publications(&self.binding, now)?;
                Ok(PublicationRecovery::Completed)
            }
            Err(_) => {
                store.fail_edge_publications(&self.binding, now, "publication_failed")?;
                store.set_edge_diagnostic(
                    &self.binding,
                    "pending_publication",
                    "derived publication pending retry",
                    now,
                )?;
                Ok(PublicationRecovery::Pending)
            }
        }
    }
}

async fn bounded_body(
    response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, EdgeDeliveryError> {
    if response
        .content_length()
        .is_some_and(|value| value > maximum as u64)
    {
        return Err(EdgeDeliveryError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| EdgeDeliveryError::Transport)?;
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(EdgeDeliveryError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn validate_batch(batch: &Batch) -> Result<(), EdgeDeliveryError> {
    if batch.version != 2
        || !valid_digest(&batch.batch_id)
        || batch.records.len().saturating_add(batch.gaps.len()) > MAX_BATCH_ITEMS
    {
        return Err(EdgeDeliveryError::InvalidResponse);
    }
    let item_bytes = batch
        .records
        .iter()
        .map(|item| serde_json::to_vec(item).map(|v| v.len()))
        .chain(
            batch
                .gaps
                .iter()
                .map(|item| serde_json::to_vec(item).map(|v| v.len())),
        )
        .try_fold(0usize, |sum, value| {
            value.map(|value| sum.saturating_add(value))
        })
        .map_err(|_| EdgeDeliveryError::InvalidResponse)?;
    if item_bytes > MAX_BATCH_ITEM_BYTES {
        return Err(EdgeDeliveryError::ResponseTooLarge);
    }
    let mut sequences = HashSet::new();
    let mut ids = HashSet::new();
    if batch
        .records
        .iter()
        .any(|item| !sequences.insert(item.spool_seq) || !ids.insert(item.record_id.as_str()))
        || batch
            .gaps
            .iter()
            .any(|item| !sequences.insert(item.spool_seq) || !ids.insert(item.notice_id.as_str()))
    {
        return Err(EdgeDeliveryError::InvalidIdentity);
    }
    let expected = batch_digest(batch)?;
    if expected != batch.batch_id {
        return Err(EdgeDeliveryError::InvalidIdentity);
    }
    Ok(())
}

fn verify_record(
    record: &BatchRecord,
    expected_vin: &str,
) -> Result<VerifiedEdgeItem, EdgeDeliveryError> {
    if record.spool_seq == 0
        || record.received_at_ms < 0
        || !valid_digest(&record.record_id)
        || !valid_digest(&record.legacy_record_id)
    {
        return Err(EdgeDeliveryError::InvalidIdentity);
    }
    let envelope = validate_envelope(&record.envelope, expected_vin)?;
    let legacy = hash_parts(
        b"teslatlas-edge-record-v1\0",
        &[&canonical(&record.envelope)?],
    );
    let mut stable = record.envelope.clone();
    let stable_object = stable
        .as_object_mut()
        .ok_or(EdgeDeliveryError::InvalidIdentity)?;
    stable_object.remove("received_at_ms");
    stable_object.insert("version".to_owned(), Value::from(2));
    let stable_id = hash_parts(b"teslatlas-edge-record-v2\0", &[&canonical(&stable)?]);
    let payload = envelope
        .get("payload")
        .ok_or(EdgeDeliveryError::InvalidIdentity)?;
    let payload_sha256 = hex::encode(Sha256::digest(canonical(payload)?));
    if legacy != record.legacy_record_id || stable_id != record.record_id {
        return Err(EdgeDeliveryError::InvalidIdentity);
    }
    let tx_type = envelope
        .get("tx_type")
        .and_then(Value::as_str)
        .ok_or(EdgeDeliveryError::InvalidIdentity)?;
    let normalized = tx_type
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect::<String>();
    let non_projection_reason = match normalized.as_str() {
        "v" | "data" | "vehicledata" | "connectivity" => None,
        "alerts" => Some("alert_event_retained".to_owned()),
        "errors" => Some("error_event_retained".to_owned()),
        _ => return Err(EdgeDeliveryError::InvalidIdentity),
    };
    Ok(VerifiedEdgeItem::Record(VerifiedEdgeRecord {
        spool_seq: record.spool_seq,
        stable_record_id: stable_id,
        legacy_record_id: legacy,
        payload_sha256,
        edge_received_at_ms: record.received_at_ms,
        envelope_json: canonical(&record.envelope)?,
        non_projection_reason,
    }))
}

fn validate_envelope<'a>(
    value: &'a Value,
    expected_vin: &str,
) -> Result<&'a serde_json::Map<String, Value>, EdgeDeliveryError> {
    let envelope = value
        .as_object()
        .ok_or(EdgeDeliveryError::InvalidIdentity)?;
    let allowed = [
        "version",
        "vin",
        "txid",
        "tx_type",
        "device_client_version",
        "firmware_version",
        "received_at_ms",
        "timestamp_ms",
        "payload",
    ];
    if envelope.keys().any(|key| !allowed.contains(&key.as_str()))
        || envelope.get("version").and_then(Value::as_u64) != Some(1)
        || envelope.get("vin").and_then(Value::as_str) != Some(expected_vin)
        || !envelope.get("payload").is_some_and(Value::is_object)
    {
        return Err(EdgeDeliveryError::InvalidIdentity);
    }
    for (field, maximum) in [("txid", 128usize), ("tx_type", 64usize)] {
        let text = envelope
            .get(field)
            .and_then(Value::as_str)
            .ok_or(EdgeDeliveryError::InvalidIdentity)?;
        if text.is_empty()
            || text.len() > maximum
            || !text.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(EdgeDeliveryError::InvalidIdentity);
        }
    }
    for field in ["device_client_version", "firmware_version"] {
        if let Some(text) = envelope.get(field) {
            let text = text.as_str().ok_or(EdgeDeliveryError::InvalidIdentity)?;
            if text.is_empty()
                || text.len() > 64
                || !text.bytes().all(|byte| byte.is_ascii_graphic())
            {
                return Err(EdgeDeliveryError::InvalidIdentity);
            }
        }
    }
    let received = envelope
        .get("received_at_ms")
        .and_then(Value::as_i64)
        .ok_or(EdgeDeliveryError::InvalidIdentity)?;
    let timestamp = envelope
        .get("timestamp_ms")
        .and_then(Value::as_i64)
        .ok_or(EdgeDeliveryError::InvalidIdentity)?;
    if received < 946_684_800_000
        || timestamp < 946_684_800_000
        || timestamp > received.saturating_add(300_000)
    {
        return Err(EdgeDeliveryError::InvalidIdentity);
    }
    Ok(envelope)
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("unique-key JSON")
            }
            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(value)))
            }
            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::from(value)))
            }
            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::from(value)))
            }
            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value.to_owned())))
            }
            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value)))
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<UniqueValue>()? {
                    values.push(value.0);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    let value = map.next_value::<UniqueValue>()?;
                    if values.insert(key, value.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

fn verify_gap(gap: &BatchGap) -> Result<VerifiedEdgeItem, EdgeDeliveryError> {
    if gap.spool_seq == 0
        || gap.occurred_at_ms < 0
        || !valid_digest(&gap.notice_id)
        || !valid_digest(&gap.evidence_sha256)
        || !matches!(
            gap.reason.as_str(),
            "retention_expired" | "integrity_quarantine"
        )
    {
        return Err(EdgeDeliveryError::InvalidIdentity);
    }
    let seq = gap.spool_seq.to_be_bytes();
    let notice = hash_parts(
        b"teslatlas-edge-gap-notice-v2\0",
        &[&seq, gap.reason.as_bytes(), gap.evidence_sha256.as_bytes()],
    );
    if notice != gap.notice_id {
        return Err(EdgeDeliveryError::InvalidIdentity);
    }
    Ok(VerifiedEdgeItem::Gap(VerifiedEdgeGap {
        spool_seq: gap.spool_seq,
        notice_id: gap.notice_id.clone(),
        occurred_at_ms: gap.occurred_at_ms,
        reason: gap.reason.clone(),
        evidence_sha256: gap.evidence_sha256.clone(),
    }))
}

fn batch_digest(batch: &Batch) -> Result<String, EdgeDeliveryError> {
    let mut items = batch
        .records
        .iter()
        .map(|item| (item.spool_seq, b'r', item.record_id.as_str()))
        .chain(
            batch
                .gaps
                .iter()
                .map(|item| (item.spool_seq, b'g', item.notice_id.as_str())),
        )
        .collect::<Vec<_>>();
    items.sort_by_key(|item| item.0);
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-edge-batch-v2\0");
    for (sequence, kind, id) in items {
        digest.update([kind]);
        digest.update(sequence.to_be_bytes());
        digest.update(id.as_bytes());
        digest.update([0]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn hash_parts(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    for part in parts {
        digest.update(part);
    }
    hex::encode(digest.finalize())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical(value: &Value) -> Result<Vec<u8>, EdgeDeliveryError> {
    serde_jcs::to_vec(value).map_err(|_| EdgeDeliveryError::InvalidIdentity)
}

fn read_bounded_file(
    path: &Path,
    maximum: usize,
    private: bool,
) -> Result<Vec<u8>, EdgeDeliveryError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| EdgeDeliveryError::UnsafeCredential)?;
    let metadata = fstat(&descriptor).map_err(|_| EdgeDeliveryError::UnsafeCredential)?;
    let mode = metadata.st_mode as u32 & 0o777;
    if !FileType::from_raw_mode(metadata.st_mode).is_file()
        || (metadata.st_uid != getuid().as_raw() && metadata.st_uid != 0)
        || (private && mode & 0o077 != 0)
        || (!private && mode & 0o022 != 0)
    {
        return Err(EdgeDeliveryError::UnsafeCredential);
    }
    let file: fs::File = descriptor.into();
    let mut bytes = Vec::new();
    file.take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| EdgeDeliveryError::UnsafeCredential)?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(EdgeDeliveryError::UnsafeCredential);
    }
    Ok(bytes)
}

fn now_ms() -> Result<i64, EdgeDeliveryError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| EdgeDeliveryError::Clock)?;
    i64::try_from(duration.as_millis()).map_err(|_| EdgeDeliveryError::Clock)
}

fn parse_unique_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, EdgeDeliveryError> {
    let unique: UniqueValue =
        serde_json::from_slice(bytes).map_err(|_| EdgeDeliveryError::InvalidResponse)?;
    serde_json::from_value(unique.0).map_err(|_| EdgeDeliveryError::InvalidResponse)
}

#[cfg(unix)]
#[doc(hidden)]
pub async fn run_for_admitted_user<F>(
    store: &HubStore,
    config: &crate::config::HubConfig,
    admission: std::sync::Arc<crate::hub_user_process::AdmittedUserHub>,
    ready: oneshot::Sender<crate::protocol::CursorKey>,
    shutdown: F,
) -> Result<(), EdgeDeliveryError>
where
    F: std::future::Future<Output = ()>,
{
    admission
        .assert_sensitive_access()
        .map_err(|_| EdgeDeliveryError::UnsafeCredential)?;
    admission
        .assert_store_path(&config.data_dir)
        .map_err(|_| EdgeDeliveryError::UnsafeCredential)?;
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(EdgeDeliveryError::InvalidIdentity);
    }
    let edge = config
        .collector
        .edge
        .as_ref()
        .ok_or(EdgeDeliveryError::InvalidIdentity)?;
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .map_err(|_| EdgeDeliveryError::UnsafeCredential)?;
    ready
        .send(cursor_key.clone())
        .map_err(|_| EdgeDeliveryError::Transport)?;
    let consumer = EdgeConsumer::from_config(
        edge,
        Duration::from_secs(config.collector.offline_drive_timeout_seconds),
    )?;
    consumer.run_until(store, &cursor_key, shutdown).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_profile_literal_batch_and_records_verify() {
        let bytes = include_bytes!(
            "../../../teslatlas-protocol/profiles/edge-delivery-v2/2.0.0/examples/batch.json"
        );
        let batch: Batch = serde_json::from_slice(bytes).unwrap();
        validate_batch(&batch).unwrap();
        assert!(matches!(
            verify_record(&batch.records[0], "5YJ3E1EA7KF000001").unwrap(),
            VerifiedEdgeItem::Record(_)
        ));
        assert!(matches!(
            verify_record(&batch.records[1], "5YJ3E1EA7KF000001").unwrap(),
            VerifiedEdgeItem::Record(VerifiedEdgeRecord {
                non_projection_reason: Some(_),
                ..
            })
        ));
        assert!(matches!(
            verify_gap(&batch.gaps[0]).unwrap(),
            VerifiedEdgeItem::Gap(_)
        ));
    }

    #[test]
    fn changed_payload_or_batch_identity_fails_closed() {
        let bytes = include_bytes!(
            "../../../teslatlas-protocol/profiles/edge-delivery-v2/2.0.0/examples/batch.json"
        );
        let mut batch: Batch = serde_json::from_slice(bytes).unwrap();
        batch.records[0].envelope["payload"]["data"]["Soc"]["intValue"] =
            Value::String("81".into());
        assert!(matches!(
            verify_record(&batch.records[0], "5YJ3E1EA7KF000001"),
            Err(EdgeDeliveryError::InvalidIdentity)
        ));
    }

    #[test]
    fn duplicate_keys_fail_before_batch_identity_is_considered() {
        let duplicate = br#"{"version":2,"version":2,"batch_id":"01d91f49aa9e06ae80070976797616a9ec74cf6ef22c6862f615b5a28371a0ef","records":[],"gaps":[]}"#;
        assert!(parse_unique_json::<Batch>(duplicate).is_err());
    }

    #[test]
    fn duplicate_ack_keys_fail_before_acknowledgement_is_trusted() {
        let duplicate = br#"{"version":2,"version":2,"acknowledged_record_ids":[],"acknowledged_gap_notice_ids":[],"unknown_record_ids":[],"unknown_gap_notice_ids":[]}"#;
        assert!(parse_unique_json::<AckResponse>(duplicate).is_err());
    }

    #[tokio::test]
    async fn transport_diagnostic_code_distinguishes_a_refused_connect_without_url_data() {
        crate::crypto::install_default_provider();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = Client::new()
            .get(format!("https://127.0.0.1:{port}/private-request-marker"))
            .send()
            .await
            .unwrap_err();

        let code = transport_diagnostic_code(&error);

        assert_eq!(code, "connect_refused");
        assert!(!code.contains("private-request-marker"));
    }
}
