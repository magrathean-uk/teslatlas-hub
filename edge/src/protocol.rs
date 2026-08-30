//! Fleet Telemetry receiver and Hub delivery wire contracts.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_RECEIVER_BODY_BYTES: usize = 256 * 1024;
pub const MAX_HUB_ACK_BODY_BYTES: usize = 128 * 1024;
const MIN_TIMESTAMP_MS: i64 = 946_684_800_000;
const MAX_EVENT_CLOCK_LEAD_MS: i64 = 5 * 60 * 1_000;
const DUPLICATE_KEY_MARKER: &str = "teslatlas_duplicate_json_key";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    #[error("input exceeds the receiver body limit")]
    InputTooLarge,
    #[error("JSON contains a duplicate object key")]
    DuplicateJsonKey,
    #[error("invalid receiver JSON")]
    InvalidJson,
    #[error("unsupported receiver envelope version")]
    UnsupportedVersion,
    #[error("invalid vehicle identity")]
    InvalidVin,
    #[error("invalid transaction identifier")]
    InvalidTransactionId,
    #[error("invalid transaction type")]
    InvalidTransactionType,
    #[error("invalid receiver timestamp")]
    InvalidTimestamp,
    #[error("invalid receiver payload")]
    InvalidPayload,
    #[error("invalid receiver version label")]
    InvalidVersionLabel,
    #[error("invalid Hub acknowledgement")]
    InvalidAcknowledgement,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverEnvelope {
    pub version: u16,
    pub vin: String,
    pub txid: String,
    pub tx_type: String,
    pub received_at_ms: i64,
    pub timestamp_ms: i64,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
}

impl ReceiverEnvelope {
    pub fn parse(input: &[u8]) -> Result<Self, ProtocolError> {
        if input.len() > MAX_RECEIVER_BODY_BYTES {
            return Err(ProtocolError::InputTooLarge);
        }
        let unique = serde_json::from_slice::<UniqueValue>(input).map_err(|error| {
            if error.to_string().contains(DUPLICATE_KEY_MARKER) {
                ProtocolError::DuplicateJsonKey
            } else {
                ProtocolError::InvalidJson
            }
        })?;
        let envelope: Self =
            serde_json::from_value(unique.0).map_err(|_| ProtocolError::InvalidJson)?;
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn record_id(&self) -> RecordId {
        let value =
            serde_json::to_value(self).expect("receiver envelope serialization cannot fail");
        let canonical =
            serde_jcs::to_vec(&value).expect("receiver envelope serialization cannot fail");
        let mut digest = Sha256::new();
        digest.update(b"teslatlas-edge-record-v1\0");
        digest.update(canonical);
        RecordId(hex::encode(digest.finalize()))
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != 1 {
            return Err(ProtocolError::UnsupportedVersion);
        }
        if !valid_vin(&self.vin) {
            return Err(ProtocolError::InvalidVin);
        }
        if !bounded_visible_ascii(&self.txid, 1, 128) {
            return Err(ProtocolError::InvalidTransactionId);
        }
        if !bounded_visible_ascii(&self.tx_type, 1, 64) {
            return Err(ProtocolError::InvalidTransactionType);
        }
        let normalized_type = self
            .tx_type
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if !matches!(
            normalized_type.as_str(),
            "v" | "data" | "vehicledata" | "connectivity"
        ) {
            return Err(ProtocolError::InvalidTransactionType);
        }
        if self.received_at_ms < MIN_TIMESTAMP_MS
            || self.timestamp_ms < MIN_TIMESTAMP_MS
            || self.timestamp_ms > self.received_at_ms.saturating_add(MAX_EVENT_CLOCK_LEAD_MS)
        {
            return Err(ProtocolError::InvalidTimestamp);
        }
        if !self.payload.is_object() {
            return Err(ProtocolError::InvalidPayload);
        }
        for label in [&self.device_client_version, &self.firmware_version]
            .into_iter()
            .flatten()
        {
            if !bounded_visible_ascii(label, 1, 64) {
                return Err(ProtocolError::InvalidVersionLabel);
            }
        }
        Ok(())
    }
}

fn valid_vin(vin: &str) -> bool {
    vin.len() == 17
        && vin.bytes().all(|byte| {
            byte.is_ascii_digit()
                || (byte.is_ascii_uppercase() && !matches!(byte, b'I' | b'O' | b'Q'))
        })
}

fn bounded_visible_ascii(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len())
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RecordId(String);

impl RecordId {
    pub fn parse(value: &str) -> Result<Self, ProtocolError> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(ProtocolError::InvalidTransactionId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RecordId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

impl fmt::Display for RecordId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubBatchRecordV1 {
    pub record_id: RecordId,
    pub received_at_ms: i64,
    pub envelope: ReceiverEnvelope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubBatchV1 {
    pub version: u16,
    pub batch_id: String,
    pub records: Vec<HubBatchRecordV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubAckV1 {
    pub version: u16,
    pub batch_id: String,
    pub accepted_record_ids: Vec<RecordId>,
}

impl HubAckV1 {
    pub fn parse(input: &[u8]) -> Result<Self, ProtocolError> {
        if input.len() > MAX_HUB_ACK_BODY_BYTES {
            return Err(ProtocolError::InputTooLarge);
        }
        let unique = serde_json::from_slice::<UniqueValue>(input).map_err(|error| {
            if error.to_string().contains(DUPLICATE_KEY_MARKER) {
                ProtocolError::DuplicateJsonKey
            } else {
                ProtocolError::InvalidJson
            }
        })?;
        let acknowledgement: Self =
            serde_json::from_value(unique.0).map_err(|_| ProtocolError::InvalidAcknowledgement)?;
        let unique_ids = acknowledgement
            .accepted_record_ids
            .iter()
            .collect::<HashSet<_>>();
        if acknowledgement.version != 1
            || !valid_lower_hex_digest(&acknowledgement.batch_id)
            || acknowledgement.accepted_record_ids.len() > 256
            || unique_ids.len() != acknowledgement.accepted_record_ids.len()
        {
            return Err(ProtocolError::InvalidAcknowledgement);
        }
        Ok(acknowledgement)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubAckResultV1 {
    pub version: u16,
    pub acknowledged_record_ids: Vec<RecordId>,
    pub unknown_record_ids: Vec<RecordId>,
}

struct UniqueValue(Value);

fn valid_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueValueVisitor).map(Self)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        UniqueValue::deserialize(deserializer).map(|value| value.0)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(value) = sequence.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = HashSet::with_capacity(object.size_hint().unwrap_or(0));
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom(DUPLICATE_KEY_MARKER));
            }
            let value = object.next_value::<UniqueValue>()?;
            values.insert(key, value.0);
        }
        Ok(Value::Object(values))
    }
}
