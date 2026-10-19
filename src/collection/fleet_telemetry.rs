// SPDX-License-Identifier: AGPL-3.0-only

//! Bounded normalization for trusted-loopback Fleet Telemetry sidecars.
//!
//! This module deliberately owns no socket, credential, or database state. It
//! validates one complete protojson transaction from a local sidecar and folds
//! known Fleet fields into the Owner-API-shaped object already consumed by the
//! Hub lifecycle and current-state projectors.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;

pub const MAX_FLEET_TELEMETRY_INPUT_BYTES: usize = 256 * 1024;
pub const MAX_FLEET_TELEMETRY_FIELDS: usize = 256;
const MAX_FIELD_NAME_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 1024;
const MAX_TXID_BYTES: usize = 128;
const MAX_TX_TYPE_BYTES: usize = 64;
const EARLIEST_TIMESTAMP_MS: i64 = 946_684_800_000;
const FUTURE_SKEW_MS: i64 = 5 * 60 * 1000;
const MAX_PACK_COMPONENT_SKEW_MS: u64 = 30 * 1000;
const COMPLETE_STATE_VERSION: u32 = 2;
const DOOR_STATE_LAYOUT_FIX_FIRMWARE: (u32, u32, u32) = (2024, 44, 32);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FleetTelemetryError {
    #[error("Fleet Telemetry transaction exceeds the byte limit")]
    InputTooLarge,
    #[error("Fleet Telemetry transaction is not valid JSON")]
    InvalidJson,
    #[error("Fleet Telemetry transaction version is unsupported")]
    UnsupportedVersion,
    #[error("Fleet Telemetry VIN is invalid")]
    InvalidVin,
    #[error("Fleet Telemetry VIN does not match this accumulator")]
    VinMismatch,
    #[error("Fleet Telemetry transaction identity is invalid")]
    InvalidTransactionIdentity,
    #[error("Fleet Telemetry transaction timestamp is invalid")]
    InvalidTimestamp,
    #[error("Fleet Telemetry payload has too many fields")]
    TooManyFields,
    #[error("Fleet Telemetry field name is invalid")]
    InvalidFieldName,
    #[error("Fleet Telemetry field value is invalid")]
    InvalidFieldValue,
    #[error("Fleet Telemetry field contains a non-finite number")]
    NonFiniteNumber,
    #[error("Fleet Telemetry location is invalid")]
    InvalidCoordinates,
    #[error("existing Owner-shaped state is invalid")]
    InvalidExistingState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FleetTelemetrySnapshot {
    pub vin: String,
    pub txid: String,
    pub tx_type: String,
    pub received_at_ms: i64,
    pub timestamp_ms: i64,
    pub created_at_ms: i64,
    pub source_vehicle_state: Option<String>,
    pub owner_data: Value,
    pub updated_fields: Vec<String>,
    pub unavailable_fields: Vec<String>,
    pub unknown_fields: Vec<String>,
    pub regressed_fields: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FleetTelemetryAccumulator {
    vin: String,
    owner_data: Map<String, Value>,
    field_watermarks: BTreeMap<String, i64>,
    pack_voltage: Option<TimedPackValue>,
    pack_current: Option<TimedPackValue>,
    ac_power: Option<TimedPackValue>,
    dc_power: Option<TimedPackValue>,
    connection_id: Option<String>,
    connection_boundary_ms: Option<i64>,
    created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimedPackValue {
    value: f64,
    timestamp_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteAccumulatorState {
    version: u32,
    vin: String,
    owner_data: Map<String, Value>,
    field_watermarks: BTreeMap<String, i64>,
    pack_voltage: Option<TimedPackValue>,
    pack_current: Option<TimedPackValue>,
    ac_power: Option<TimedPackValue>,
    dc_power: Option<TimedPackValue>,
    connection_id: Option<String>,
    connection_boundary_ms: Option<i64>,
    created_at_ms: i64,
}

/// Deserialises a present, non-null string. An explicit JSON null is refused,
/// unlike an absent key, which stays `None` through `#[serde(default)]`.
fn present_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

/// Deserialises a present JSON value, null included, so an explicit null reaches
/// the field validators instead of reading as an absent key.
fn present_value<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

/// Deserialises the envelope payload as a JSON value, refusing a repeated key at
/// the payload's top level. A plain `Value` would keep the last of two equal
/// keys silently; the typed payload structs refuse them, so this keeps that rule
/// now that the payload is read as a value before its shape is chosen.
fn strict_payload<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    struct StrictPayload;

    impl<'de> serde::de::Visitor<'de> for StrictPayload {
        type Value = Value;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a JSON value")
        }

        fn visit_map<A>(self, mut access: A) -> Result<Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut object = Map::new();
            while let Some((key, value)) = access.next_entry::<String, Value>()? {
                if object.insert(key.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format_args!(
                        "duplicate payload key `{key}`"
                    )));
                }
            }
            Ok(Value::Object(object))
        }

        fn visit_seq<A>(self, mut access: A) -> Result<Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut items = Vec::new();
            while let Some(item) = access.next_element::<Value>()? {
                items.push(item);
            }
            Ok(Value::Array(items))
        }

        fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
            Ok(Value::Bool(value))
        }

        fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
            Ok(Value::from(value))
        }

        fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
            Ok(Value::from(value))
        }

        fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
            Ok(Value::from(value))
        }

        fn visit_str<E>(self, value: &str) -> Result<Value, E> {
            Ok(Value::String(value.to_owned()))
        }

        fn visit_unit<E>(self) -> Result<Value, E> {
            Ok(Value::Null)
        }
    }

    deserializer.deserialize_any(StrictPayload)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTransaction {
    version: u32,
    vin: String,
    txid: String,
    tx_type: String,
    #[serde(default, deserialize_with = "present_string")]
    device_client_version: Option<String>,
    #[serde(default, deserialize_with = "present_string")]
    firmware_version: Option<String>,
    received_at_ms: i64,
    timestamp_ms: i64,
    #[serde(deserialize_with = "strict_payload")]
    payload: Value,
}

#[derive(Debug)]
enum WirePayload {
    Vehicle(WireVehiclePayload),
    Connectivity(WireConnectivityPayload),
    Event,
}

/// Kind of record a validated receiver envelope carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiverKind {
    Vehicle,
    Connectivity,
    Event,
}

/// A fully validated receiver envelope. Parsing it touches no store.
#[derive(Debug)]
pub(crate) struct ReceiverEnvelope {
    pub(crate) vin: String,
    pub(crate) kind: ReceiverKind,
    pub(crate) txid: String,
    /// Normalised transaction type: lowercase ASCII alphanumerics only.
    pub(crate) tx_type: String,
    pub(crate) received_at_ms: i64,
    pub(crate) timestamp_ms: i64,
    /// The payload object of an alert or error record; `None` otherwise.
    pub(crate) event_payload: Option<Value>,
    raw_tx_type: String,
    device_client_version: Option<String>,
    firmware_version: Option<String>,
    payload: WirePayload,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireVehiclePayload {
    #[serde(default)]
    vin: Option<String>,
    #[serde(default, rename = "createdAt", alias = "created_at")]
    created_at: Option<Value>,
    #[serde(default, rename = "isResend", alias = "is_resend")]
    _is_resend: Option<bool>,
    data: WireData,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireConnectivityPayload {
    #[serde(
        default,
        rename = "connectionId",
        alias = "connection_id",
        alias = "ConnectionId",
        deserialize_with = "present_string"
    )]
    connection_id: Option<String>,
    #[serde(rename = "status", alias = "Status")]
    status: String,
    #[serde(
        default,
        rename = "createdAt",
        alias = "created_at",
        alias = "CreatedAt",
        deserialize_with = "present_value"
    )]
    created_at: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WireData {
    Map(BTreeMap<String, Value>),
    List(Vec<WireDatum>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDatum {
    key: String,
    value: Value,
}

enum DecodedValue {
    Scalar(Value),
    Location { latitude: f64, longitude: f64 },
    Structured(Map<String, Value>),
    Unavailable,
    Unknown,
}

#[derive(Clone, Copy)]
enum Group {
    Drive,
    Charge,
    Climate,
    Vehicle,
    Config,
    SoftwareUpdate,
}

impl FleetTelemetryAccumulator {
    /// Restore the complete accumulator without discarding ordering
    /// watermarks, the connection boundary or any half of a split sample.
    pub fn restore_complete(input: &[u8]) -> Result<Self, FleetTelemetryError> {
        if input.len() > MAX_FLEET_TELEMETRY_INPUT_BYTES {
            return Err(FleetTelemetryError::InputTooLarge);
        }
        let state: CompleteAccumulatorState =
            serde_json::from_slice(input).map_err(|_| FleetTelemetryError::InvalidExistingState)?;
        if state.version != COMPLETE_STATE_VERSION
            || state.created_at_ms < 0
            || state.field_watermarks.len() > MAX_FLEET_TELEMETRY_FIELDS + 1
            || state.field_watermarks.iter().any(|(field, timestamp)| {
                field.len() > MAX_FIELD_NAME_BYTES
                    || field.is_empty()
                    || *timestamp < EARLIEST_TIMESTAMP_MS
            })
            || state
                .connection_boundary_ms
                .is_some_and(|boundary| boundary < EARLIEST_TIMESTAMP_MS)
            || state
                .connection_id
                .as_deref()
                .is_some_and(|id| validate_identifier(id, MAX_TXID_BYTES).is_err())
            || [
                state.pack_voltage,
                state.pack_current,
                state.ac_power,
                state.dc_power,
            ]
            .into_iter()
            .flatten()
            .any(|value| !value.value.is_finite() || value.timestamp_ms < EARLIEST_TIMESTAMP_MS)
        {
            return Err(FleetTelemetryError::InvalidExistingState);
        }
        let vin = validate_vin(&state.vin)?;
        let owner_data = sanitize_existing_owner(&state.owner_data)?;
        if owner_data
            .get("vin")
            .and_then(Value::as_str)
            .is_some_and(|owner_vin| owner_vin != vin)
        {
            return Err(FleetTelemetryError::VinMismatch);
        }
        Ok(Self {
            vin,
            owner_data,
            field_watermarks: state.field_watermarks,
            pack_voltage: state.pack_voltage,
            pack_current: state.pack_current,
            ac_power: state.ac_power,
            dc_power: state.dc_power,
            connection_id: state.connection_id,
            connection_boundary_ms: state.connection_boundary_ms,
            created_at_ms: state.created_at_ms,
        })
    }

    /// Restore durable state and bind it to the configured vehicle VIN.
    /// A complete state belongs to one vehicle; accepting it for another VIN
    /// would discard every subsequent supported record.
    pub(crate) fn restore_complete_for_vin(
        input: &[u8],
        expected_vin: &str,
    ) -> Result<Self, FleetTelemetryError> {
        let accumulator = Self::restore_complete(input)?;
        if accumulator.vin != validate_vin(expected_vin)? {
            return Err(FleetTelemetryError::VinMismatch);
        }
        Ok(accumulator)
    }

    /// Versioned, bounded state used by the durable receiver.
    pub fn encode_complete(&self) -> Result<Vec<u8>, FleetTelemetryError> {
        let encoded = serde_json::to_vec(&CompleteAccumulatorState {
            version: COMPLETE_STATE_VERSION,
            vin: self.vin.clone(),
            owner_data: self.owner_data.clone(),
            field_watermarks: self.field_watermarks.clone(),
            pack_voltage: self.pack_voltage,
            pack_current: self.pack_current,
            ac_power: self.ac_power,
            dc_power: self.dc_power,
            connection_id: self.connection_id.clone(),
            connection_boundary_ms: self.connection_boundary_ms,
            created_at_ms: self.created_at_ms,
        })
        .map_err(|_| FleetTelemetryError::InvalidExistingState)?;
        if encoded.len() > MAX_FLEET_TELEMETRY_INPUT_BYTES {
            return Err(FleetTelemetryError::InputTooLarge);
        }
        Ok(encoded)
    }

    pub fn restore(vin: &str, existing_owner_data: &Value) -> Result<Self, FleetTelemetryError> {
        let vin = validate_vin(vin)?;
        let bytes = serde_json::to_vec(existing_owner_data)
            .map_err(|_| FleetTelemetryError::InvalidExistingState)?;
        if bytes.len() > MAX_FLEET_TELEMETRY_INPUT_BYTES {
            return Err(FleetTelemetryError::InputTooLarge);
        }
        let existing = existing_owner_data
            .as_object()
            .ok_or(FleetTelemetryError::InvalidExistingState)?;
        if let Some(existing_vin) = existing.get("vin") {
            let existing_vin = existing_vin
                .as_str()
                .ok_or(FleetTelemetryError::InvalidExistingState)?;
            if validate_vin(existing_vin)? != vin {
                return Err(FleetTelemetryError::VinMismatch);
            }
        }
        let owner_data = sanitize_existing_owner(existing)?;
        let created_at_ms = owner_data
            .get("created_at")
            .and_then(json_i64)
            .filter(|value| *value >= EARLIEST_TIMESTAMP_MS)
            .unwrap_or(0);
        Ok(Self {
            vin,
            owner_data,
            field_watermarks: BTreeMap::new(),
            // These are intentionally not inferred from Owner API state. After
            // restart, power resumes only after both raw Fleet signals arrive.
            pack_voltage: None,
            pack_current: None,
            ac_power: None,
            dc_power: None,
            connection_id: None,
            connection_boundary_ms: None,
            created_at_ms,
        })
    }

    pub fn empty(vin: &str) -> Result<Self, FleetTelemetryError> {
        Self::restore(vin, &json!({}))
    }

    /// Parse and apply one vehicle or connectivity record. Alert and error
    /// records are events and never enter the accumulator.
    pub fn apply_json(
        &mut self,
        input: &[u8],
    ) -> Result<FleetTelemetrySnapshot, FleetTelemetryError> {
        self.apply_envelope(parse_envelope(input)?)
    }

    pub fn owner_data(&self) -> Value {
        Value::Object(self.owner_data.clone())
    }

    pub(crate) fn apply_envelope(
        &mut self,
        envelope: ReceiverEnvelope,
    ) -> Result<FleetTelemetrySnapshot, FleetTelemetryError> {
        let ReceiverEnvelope {
            vin: transaction_vin,
            txid,
            raw_tx_type,
            received_at_ms,
            timestamp_ms,
            device_client_version,
            firmware_version,
            payload,
            ..
        } = envelope;
        if transaction_vin != self.vin {
            return Err(FleetTelemetryError::VinMismatch);
        }
        let mut next_owner_data = self.owner_data.clone();
        let mut next_field_watermarks = self.field_watermarks.clone();
        let mut next_pack_voltage = self.pack_voltage;
        let mut next_pack_current = self.pack_current;
        let mut next_ac_power = self.ac_power;
        let mut next_dc_power = self.dc_power;
        let mut next_connection_id = self.connection_id.clone();
        let mut next_connection_boundary = self.connection_boundary_ms;
        let mut pack_value_changed = false;
        let mut charger_power_changed = false;
        let mut updated = BTreeSet::new();
        let mut unavailable = BTreeSet::new();
        let mut unknown = BTreeSet::new();
        let mut regressed = BTreeSet::new();

        match payload {
            WirePayload::Event => return Err(FleetTelemetryError::InvalidTransactionIdentity),
            WirePayload::Vehicle(payload) => {
                let state_watermark = next_field_watermarks
                    .get("__connectivity")
                    .copied()
                    .or_else(|| (self.created_at_ms > 0).then_some(self.created_at_ms));
                if state_watermark.is_none_or(|watermark| timestamp_ms > watermark) {
                    // A V record proves online state at its event time. This
                    // recovers from a lost best-effort CONNECTED record without
                    // overriding a newer offline transition.
                    next_owner_data.insert("state".to_owned(), Value::String("online".to_owned()));
                    next_field_watermarks.insert("__connectivity".to_owned(), timestamp_ms);
                }
                let fields = payload.data.into_fields()?;
                let transaction_firmware_version = fields
                    .iter()
                    .find(|(name, _)| {
                        matches!(
                            normalized_key(name).as_str(),
                            "version" | "carversion" | "firmwareversion"
                        )
                    })
                    .and_then(|(_, value)| decode_value(value).ok())
                    .and_then(|value| match value {
                        DecodedValue::Scalar(value) => scalar_text(&value).ok(),
                        _ => None,
                    });
                let owner_firmware_version = next_owner_data
                    .get("vehicle_state")
                    .and_then(Value::as_object)
                    .and_then(|state| state.get("car_version"))
                    .and_then(Value::as_str);
                let door_state_layout = door_state_layout(
                    firmware_version
                        .as_deref()
                        .or(transaction_firmware_version.as_deref())
                        .or(owner_firmware_version),
                    device_client_version.as_deref(),
                );
                for (source_name, raw_value) in fields {
                    validate_field_name(&source_name)?;
                    let key = normalized_key(&source_name);
                    let Some(group) = target_group(&key) else {
                        unknown.insert(source_name);
                        continue;
                    };
                    let watermark = next_field_watermarks
                        .get(&key)
                        .copied()
                        .or_else(|| group_timestamp(&self.owner_data, group))
                        .or_else(|| {
                            matches!(key.as_str(), "packvoltage" | "packcurrent")
                                .then_some(self.created_at_ms)
                                .filter(|timestamp| *timestamp > 0)
                        });
                    // A frame stamped at or before the end of the previous
                    // connection belongs to that connection, however late it
                    // arrives.
                    if watermark.is_some_and(|watermark| timestamp_ms <= watermark)
                        || next_connection_boundary.is_some_and(|boundary| timestamp_ms <= boundary)
                    {
                        regressed.insert(source_name);
                        continue;
                    }
                    match decode_value(&raw_value)? {
                        DecodedValue::Unavailable => {
                            match key.as_str() {
                                "packvoltage" => {
                                    next_pack_voltage = None;
                                    pack_value_changed = true;
                                }
                                "packcurrent" => {
                                    next_pack_current = None;
                                    pack_value_changed = true;
                                }
                                "acchargingpower" => {
                                    next_ac_power = None;
                                    charger_power_changed = true;
                                }
                                "dcchargingpower" => {
                                    next_dc_power = None;
                                    charger_power_changed = true;
                                }
                                _ => {}
                            }
                            next_field_watermarks.insert(key, timestamp_ms);
                            unavailable.insert(source_name);
                        }
                        DecodedValue::Unknown => {
                            let tracked_field = match key.as_str() {
                                "packvoltage" => {
                                    next_pack_voltage = None;
                                    pack_value_changed = true;
                                    true
                                }
                                "packcurrent" => {
                                    next_pack_current = None;
                                    pack_value_changed = true;
                                    true
                                }
                                "acchargingpower" => {
                                    next_ac_power = None;
                                    charger_power_changed = true;
                                    true
                                }
                                "dcchargingpower" => {
                                    next_dc_power = None;
                                    charger_power_changed = true;
                                    true
                                }
                                _ => false,
                            };
                            if tracked_field {
                                next_field_watermarks.insert(key, timestamp_ms);
                            }
                            unknown.insert(source_name);
                        }
                        value => {
                            match key.as_str() {
                                "packvoltage" => {
                                    let DecodedValue::Scalar(value) = value else {
                                        return Err(FleetTelemetryError::InvalidFieldValue);
                                    };
                                    next_pack_voltage = Some(TimedPackValue {
                                        value: nonnegative(&value)?,
                                        timestamp_ms,
                                    });
                                    pack_value_changed = true;
                                }
                                "packcurrent" => {
                                    let DecodedValue::Scalar(value) = value else {
                                        return Err(FleetTelemetryError::InvalidFieldValue);
                                    };
                                    next_pack_current = Some(TimedPackValue {
                                        value: scalar_f64(&value)?,
                                        timestamp_ms,
                                    });
                                    pack_value_changed = true;
                                }
                                "acchargingpower" | "dcchargingpower" => {
                                    let DecodedValue::Scalar(value) = value else {
                                        return Err(FleetTelemetryError::InvalidFieldValue);
                                    };
                                    let timed = TimedPackValue {
                                        value: nonnegative(&value)?,
                                        timestamp_ms,
                                    };
                                    if key == "acchargingpower" {
                                        next_ac_power = Some(timed);
                                    } else {
                                        next_dc_power = Some(timed);
                                    }
                                    charger_power_changed = true;
                                }
                                "chargerpower" => {
                                    let DecodedValue::Scalar(value) = value else {
                                        return Err(FleetTelemetryError::InvalidFieldValue);
                                    };
                                    let generic = nonnegative(&value)?;
                                    // The generic signal only speaks for the
                                    // charger until a specific one arrives on
                                    // this connection.
                                    if next_ac_power.is_none() && next_dc_power.is_none() {
                                        set_number(
                                            &mut next_owner_data,
                                            Group::Charge,
                                            "charger_power",
                                            generic,
                                            timestamp_ms,
                                        );
                                    }
                                }
                                _ => map_known_field(
                                    &mut next_owner_data,
                                    &key,
                                    value,
                                    timestamp_ms,
                                    door_state_layout,
                                )?,
                            }
                            next_field_watermarks.insert(key, timestamp_ms);
                            updated.insert(source_name);
                        }
                    }
                }
                if pack_value_changed {
                    if let (Some(voltage), Some(current)) = (next_pack_voltage, next_pack_current)
                        && voltage.timestamp_ms.abs_diff(current.timestamp_ms)
                            <= MAX_PACK_COMPONENT_SKEW_MS
                    {
                        // PackCurrent is negative while the pack is discharging;
                        // Owner API drive power is positive for vehicle draw and
                        // negative for charge/regen.
                        let power_kw = -(voltage.value * current.value) / 1_000.0;
                        if !power_kw.is_finite() {
                            return Err(FleetTelemetryError::NonFiniteNumber);
                        }
                        set_number(
                            &mut next_owner_data,
                            Group::Drive,
                            "power",
                            power_kw,
                            voltage.timestamp_ms.max(current.timestamp_ms),
                        );
                    } else {
                        clear_group_field(&mut next_owner_data, Group::Drive, "power");
                    }
                }
                if charger_power_changed {
                    match chosen_charger_power(next_ac_power, next_dc_power) {
                        Some(chosen) => set_number(
                            &mut next_owner_data,
                            Group::Charge,
                            "charger_power",
                            chosen.value,
                            chosen.timestamp_ms,
                        ),
                        None => {
                            clear_group_field(&mut next_owner_data, Group::Charge, "charger_power")
                        }
                    }
                }
            }
            WirePayload::Connectivity(payload) => {
                let state = connectivity_state(&payload.status)?;
                let watermark = next_field_watermarks
                    .get("__connectivity")
                    .copied()
                    .or_else(|| (self.created_at_ms > 0).then_some(self.created_at_ms));
                if watermark.is_some_and(|watermark| timestamp_ms <= watermark) {
                    regressed.insert("Connectivity".to_owned());
                } else {
                    next_owner_data.insert("state".to_owned(), Value::String(state.to_owned()));
                    let changed_connection = payload
                        .connection_id
                        .as_deref()
                        .is_some_and(|id| self.connection_id.as_deref() != Some(id));
                    if state == "offline" || changed_connection {
                        // The connection ended: its groups and live samples
                        // go, the field watermarks stay, and the boundary
                        // keeps delayed frames of that connection out.
                        for group in [
                            Group::Drive,
                            Group::Charge,
                            Group::Climate,
                            Group::Vehicle,
                            Group::Config,
                        ] {
                            next_owner_data.remove(group_name(group));
                        }
                        next_pack_voltage = None;
                        next_pack_current = None;
                        next_ac_power = None;
                        next_dc_power = None;
                        next_connection_id = payload.connection_id.clone();
                        next_connection_boundary = Some(timestamp_ms);
                    }
                    next_field_watermarks.insert("__connectivity".to_owned(), timestamp_ms);
                    updated.insert("Connectivity".to_owned());
                }
            }
        }

        align_group_timestamps(&mut next_owner_data, &next_field_watermarks);
        let created_at_ms = self.created_at_ms.max(timestamp_ms);
        next_owner_data.insert("created_at".to_owned(), Value::Number(created_at_ms.into()));
        next_owner_data.insert("vin".to_owned(), Value::String(self.vin.clone()));
        let source_vehicle_state = next_owner_data
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_owned);

        self.owner_data = next_owner_data;
        self.field_watermarks = next_field_watermarks;
        self.pack_voltage = next_pack_voltage;
        self.pack_current = next_pack_current;
        self.ac_power = next_ac_power;
        self.dc_power = next_dc_power;
        self.connection_id = next_connection_id;
        self.connection_boundary_ms = next_connection_boundary;
        self.created_at_ms = created_at_ms;

        Ok(FleetTelemetrySnapshot {
            vin: self.vin.clone(),
            txid,
            tx_type: raw_tx_type,
            received_at_ms,
            timestamp_ms,
            created_at_ms: self.created_at_ms,
            source_vehicle_state,
            owner_data: Value::Object(self.owner_data.clone()),
            updated_fields: updated.into_iter().collect(),
            unavailable_fields: unavailable.into_iter().collect(),
            unknown_fields: unknown.into_iter().collect(),
            regressed_fields: regressed.into_iter().collect(),
        })
    }
}

/// AC above zero wins; otherwise delivered DC power; otherwise a delivered
/// zero. Each value keeps the time of the signal that supplied it.
fn chosen_charger_power(
    ac: Option<TimedPackValue>,
    dc: Option<TimedPackValue>,
) -> Option<TimedPackValue> {
    match (ac, dc) {
        (Some(ac), _) if ac.value > 0.0 => Some(ac),
        (_, Some(dc)) if dc.value > 0.0 => Some(dc),
        (Some(ac), _) => Some(ac),
        (None, dc) => dc,
    }
}

/// Every present group carries the newest watermark of its own keys (or its
/// stamped time when that is newer).
fn align_group_timestamps(owner: &mut Map<String, Value>, watermarks: &BTreeMap<String, i64>) {
    for (key, watermark) in watermarks {
        let Some(group) = target_group(key) else {
            continue;
        };
        let Some(fields) = owner
            .get_mut(group_name(group))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        if fields
            .get("timestamp")
            .and_then(json_i64)
            .is_none_or(|stamped| stamped < *watermark)
        {
            fields.insert("timestamp".to_owned(), Value::Number((*watermark).into()));
        }
    }
}

/// Parse and fully validate one receiver envelope without touching any store.
pub(crate) fn parse_envelope(input: &[u8]) -> Result<ReceiverEnvelope, FleetTelemetryError> {
    if input.len() > MAX_FLEET_TELEMETRY_INPUT_BYTES {
        return Err(FleetTelemetryError::InputTooLarge);
    }
    let transaction: WireTransaction =
        serde_json::from_slice(input).map_err(|_| FleetTelemetryError::InvalidJson)?;
    let vin = validate_envelope(&transaction)?;
    let WireTransaction {
        txid,
        tx_type: raw_tx_type,
        device_client_version,
        firmware_version,
        received_at_ms,
        timestamp_ms,
        payload,
        ..
    } = transaction;
    if !payload.is_object() {
        return Err(FleetTelemetryError::InvalidJson);
    }
    let tx_type = normalized_key(&raw_tx_type);
    let (kind, decoded, event_payload) = match tx_type.as_str() {
        "v" | "data" | "vehicledata" => {
            let payload: WireVehiclePayload =
                serde_json::from_value(payload).map_err(|_| FleetTelemetryError::InvalidJson)?;
            if let Some(payload_vin) = payload.vin.as_deref()
                && validate_vin(payload_vin)? != vin
            {
                return Err(FleetTelemetryError::VinMismatch);
            }
            if let Some(created_at) = payload.created_at.as_ref() {
                validate_created_at(created_at)?;
            }
            (ReceiverKind::Vehicle, WirePayload::Vehicle(payload), None)
        }
        "connectivity" => {
            let payload: WireConnectivityPayload =
                serde_json::from_value(payload).map_err(|_| FleetTelemetryError::InvalidJson)?;
            if let Some(connection_id) = payload.connection_id.as_deref() {
                validate_identifier(connection_id, MAX_TXID_BYTES)?;
            }
            if let Some(created_at) = payload.created_at.as_ref() {
                validate_created_at(created_at)?;
            }
            connectivity_state(&payload.status)?;
            (
                ReceiverKind::Connectivity,
                WirePayload::Connectivity(payload),
                None,
            )
        }
        "alerts" | "errors" => (ReceiverKind::Event, WirePayload::Event, Some(payload)),
        _ => return Err(FleetTelemetryError::InvalidTransactionIdentity),
    };
    Ok(ReceiverEnvelope {
        vin,
        kind,
        txid,
        tx_type,
        raw_tx_type,
        received_at_ms,
        timestamp_ms,
        event_payload,
        device_client_version,
        firmware_version,
        payload: decoded,
    })
}

fn validate_envelope(transaction: &WireTransaction) -> Result<String, FleetTelemetryError> {
    if transaction.version != 1 {
        return Err(FleetTelemetryError::UnsupportedVersion);
    }
    let vin = validate_vin(&transaction.vin)?;
    validate_identifier(&transaction.txid, MAX_TXID_BYTES)?;
    validate_identifier(&transaction.tx_type, MAX_TX_TYPE_BYTES)?;
    for version in [
        transaction.device_client_version.as_deref(),
        transaction.firmware_version.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if version.is_empty()
            || version.len() > 64
            || !version.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(FleetTelemetryError::InvalidTransactionIdentity);
        }
    }
    if transaction.received_at_ms < EARLIEST_TIMESTAMP_MS
        || transaction.timestamp_ms < EARLIEST_TIMESTAMP_MS
        || transaction.timestamp_ms > transaction.received_at_ms.saturating_add(FUTURE_SKEW_MS)
    {
        return Err(FleetTelemetryError::InvalidTimestamp);
    }
    Ok(vin)
}

impl WireData {
    fn into_fields(self) -> Result<Vec<(String, Value)>, FleetTelemetryError> {
        let fields = match self {
            Self::Map(fields) => fields.into_iter().collect::<Vec<_>>(),
            Self::List(fields) => fields
                .into_iter()
                .map(|field| (field.key, field.value))
                .collect(),
        };
        if fields.len() > MAX_FLEET_TELEMETRY_FIELDS {
            return Err(FleetTelemetryError::TooManyFields);
        }
        let mut seen = BTreeSet::new();
        for (name, _) in &fields {
            let normalized = normalized_key(name);
            if !seen.insert(normalized) {
                return Err(FleetTelemetryError::InvalidFieldName);
            }
        }
        Ok(fields)
    }
}

pub fn recommended_cheap_fields_config() -> Value {
    let mut fields = Map::new();
    for (name, seconds) in [
        ("Location", 5),
        ("VehicleSpeed", 5),
        ("GpsHeading", 5),
        ("Gear", 1),
        ("Soc", 30),
        ("BatteryLevel", 30),
        ("RatedRange", 30),
        ("EstBatteryRange", 30),
        ("IdealBatteryRange", 30),
        ("PackVoltage", 5),
        ("PackCurrent", 5),
        ("DetailedChargeState", 1),
        ("DCChargingEnergyIn", 15),
        ("ACChargingPower", 15),
        ("DCChargingPower", 15),
        ("FastChargerPresent", 15),
        ("FastChargerType", 15),
        ("ChargeAmps", 15),
        ("ChargeLimitSoc", 30),
        ("ChargePortDoorOpen", 1),
        ("Odometer", 60),
        ("InsideTemp", 60),
        ("OutsideTemp", 60),
        ("HvacPower", 60),
        ("HvacFanStatus", 60),
        ("HvacLeftTemperatureRequest", 60),
        ("HvacRightTemperatureRequest", 60),
        ("PreconditioningEnabled", 60),
        ("Locked", 1),
        ("SentryMode", 5),
        ("ServiceMode", 5),
        ("DoorState", 1),
        ("FdWindow", 1),
        ("RdWindow", 1),
        ("FpWindow", 1),
        ("RpWindow", 1),
        ("TpmsPressureFl", 300),
        ("TpmsPressureFr", 300),
        ("TpmsPressureRl", 300),
        ("TpmsPressureRr", 300),
        ("TpmsSoftWarnings", 300),
        ("Version", 3600),
        ("CarType", 3600),
        ("Trim", 3600),
        ("ExteriorColor", 3600),
        ("WheelType", 3600),
        ("SoftwareUpdateVersion", 300),
        ("SoftwareUpdateDownloadPercentComplete", 60),
        ("SoftwareUpdateInstallationPercentComplete", 60),
    ] {
        fields.insert(name.to_owned(), json!({"interval_seconds": seconds}));
    }
    json!({"fields": fields})
}

fn validate_vin(value: &str) -> Result<String, FleetTelemetryError> {
    if value.len() != 17
        || !value.bytes().all(|byte| {
            byte.is_ascii_digit()
                || (byte.is_ascii_uppercase() && !matches!(byte, b'I' | b'O' | b'Q'))
        })
    {
        return Err(FleetTelemetryError::InvalidVin);
    }
    Ok(value.to_owned())
}

fn validate_identifier(value: &str, maximum: usize) -> Result<(), FleetTelemetryError> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(FleetTelemetryError::InvalidTransactionIdentity);
    }
    Ok(())
}

fn validate_created_at(value: &Value) -> Result<(), FleetTelemetryError> {
    match value {
        Value::String(text) if valid_proto_timestamp(text) => Ok(()),
        _ => Err(FleetTelemetryError::InvalidTimestamp),
    }
}

fn valid_proto_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !(20..=30).contains(&bytes.len()) || bytes.last() != Some(&b'Z') {
        return false;
    }
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }
    for index in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
        if !bytes.get(index).is_some_and(u8::is_ascii_digit) {
            return false;
        }
    }
    let fraction = &bytes[19..bytes.len() - 1];
    if !fraction.is_empty()
        && (fraction.first() != Some(&b'.')
            || fraction.len() == 1
            || !fraction[1..].iter().all(u8::is_ascii_digit))
    {
        return false;
    }
    let parse = |range: std::ops::Range<usize>| {
        std::str::from_utf8(&bytes[range])
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
    };
    let Some(year) = parse(0..4) else {
        return false;
    };
    let Some(month) = parse(5..7) else {
        return false;
    };
    let Some(day) = parse(8..10) else {
        return false;
    };
    let Some(hour) = parse(11..13) else {
        return false;
    };
    let Some(minute) = parse(14..16) else {
        return false;
    };
    let Some(second) = parse(17..19) else {
        return false;
    };
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let maximum_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    year > 0 && (1..=maximum_day).contains(&day) && hour <= 23 && minute <= 59 && second <= 59
}

fn validate_field_name(value: &str) -> Result<(), FleetTelemetryError> {
    if value.is_empty()
        || value.len() > MAX_FIELD_NAME_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(FleetTelemetryError::InvalidFieldName);
    }
    Ok(())
}

fn normalized_key(value: &str) -> String {
    value
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect()
}

fn decode_value(value: &Value) -> Result<DecodedValue, FleetTelemetryError> {
    match value {
        Value::Null => Ok(DecodedValue::Unavailable),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            Ok(DecodedValue::Scalar(value.clone()))
        }
        Value::Array(_) => Err(FleetTelemetryError::InvalidFieldValue),
        Value::Object(fields) => {
            if fields.keys().any(|key| {
                matches!(
                    normalized_key(key).as_str(),
                    "invalid" | "invalidvalue" | "notavailable" | "unavailable" | "error"
                )
            }) {
                return Ok(DecodedValue::Unavailable);
            }
            if let Some(location) = fields
                .get("locationValue")
                .or_else(|| fields.get("location_value"))
            {
                return decode_location(location);
            }
            if fields.contains_key("latitude") && fields.contains_key("longitude") {
                return decode_location(value);
            }
            for key in [
                "doorValue",
                "door_value",
                "tireLocationValue",
                "tire_location_value",
            ] {
                if let Some(value) = fields.get(key) {
                    if fields.len() != 1 {
                        return Err(FleetTelemetryError::InvalidFieldValue);
                    }
                    let value = value
                        .as_object()
                        .ok_or(FleetTelemetryError::InvalidFieldValue)?;
                    if value.len() > 16 {
                        return Err(FleetTelemetryError::InvalidFieldValue);
                    }
                    return Ok(DecodedValue::Structured(value.clone()));
                }
            }
            for key in [
                "stringValue",
                "string_value",
                "doubleValue",
                "double_value",
                "floatValue",
                "float_value",
                "intValue",
                "int_value",
                "longValue",
                "long_value",
                "booleanValue",
                "boolean_value",
                "boolValue",
                "bool_value",
                "chargingValue",
                "charging_value",
                "shiftStateValue",
                "shift_state_value",
                "sentryModeStateValue",
                "sentry_mode_state_value",
                "carTypeValue",
                "car_type_value",
                "windowStateValue",
                "window_state_value",
                "detailedChargeStateValue",
                "detailed_charge_state_value",
                "climateKeeperModeValue",
                "climate_keeper_mode_value",
                "hvacPowerValue",
                "hvac_power_value",
                "cableTypeValue",
                "cable_type_value",
                "fastChargerValue",
                "fast_charger_value",
            ] {
                if let Some(value) = fields.get(key) {
                    if fields.len() != 1 {
                        return Err(FleetTelemetryError::InvalidFieldValue);
                    }
                    return match value {
                        Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                            if value.as_str().is_some_and(is_unavailable_enum) {
                                Ok(DecodedValue::Unavailable)
                            } else {
                                Ok(DecodedValue::Scalar(value.clone()))
                            }
                        }
                        _ => Err(FleetTelemetryError::InvalidFieldValue),
                    };
                }
            }
            Ok(DecodedValue::Unknown)
        }
    }
}

fn decode_location(value: &Value) -> Result<DecodedValue, FleetTelemetryError> {
    let fields = value
        .as_object()
        .ok_or(FleetTelemetryError::InvalidCoordinates)?;
    let latitude = scalar_f64(
        fields
            .get("latitude")
            .ok_or(FleetTelemetryError::InvalidCoordinates)?,
    )?;
    let longitude = scalar_f64(
        fields
            .get("longitude")
            .ok_or(FleetTelemetryError::InvalidCoordinates)?,
    )?;
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return Err(FleetTelemetryError::InvalidCoordinates);
    }
    Ok(DecodedValue::Location {
        latitude,
        longitude,
    })
}

fn scalar_f64(value: &Value) -> Result<f64, FleetTelemetryError> {
    let value = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))
        .ok_or(FleetTelemetryError::InvalidFieldValue)?;
    if !value.is_finite() {
        return Err(FleetTelemetryError::NonFiniteNumber);
    }
    Ok(value)
}

fn scalar_i64(value: &Value) -> Result<i64, FleetTelemetryError> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
        .ok_or(FleetTelemetryError::InvalidFieldValue)
}

fn scalar_bool(value: &Value) -> Result<bool, FleetTelemetryError> {
    value
        .as_bool()
        .or_else(|| match value.as_str().map(normalized_key).as_deref() {
            Some("true" | "on" | "open") => Some(true),
            Some("false" | "off" | "closed") => Some(false),
            Some(value)
                if value.ends_with("on")
                    || value.ends_with("open")
                    || value.ends_with("opened")
                    || value.ends_with("partiallyopen")
                    || value.ends_with("armed")
                    || value.ends_with("idle")
                    || value.ends_with("aware")
                    || value.ends_with("panic")
                    || value.ends_with("quiet") =>
            {
                Some(true)
            }
            Some(value) if value.ends_with("off") || value.ends_with("closed") => Some(false),
            _ => None,
        })
        .ok_or(FleetTelemetryError::InvalidFieldValue)
}

fn hvac_power_on(value: &Value) -> Result<bool, FleetTelemetryError> {
    if let Some(value) = value.as_bool() {
        return Ok(value);
    }
    match value.as_str().map(normalized_key).as_deref() {
        Some("on" | "hvacpowerstateon" | "precondition" | "hvacpowerstateprecondition")
        | Some("overheatprotect" | "hvacpowerstateoverheatprotect") => Ok(true),
        Some("off" | "hvacpowerstateoff") => Ok(false),
        _ => Err(FleetTelemetryError::InvalidFieldValue),
    }
}

fn is_unavailable_enum(value: &str) -> bool {
    let value = normalized_key(value);
    value.ends_with("unknown")
        || value.ends_with("invalid")
        || value.ends_with("unavailable")
        || value.ends_with("notavailable")
        || value.ends_with("sna")
}

fn connectivity_state(value: &str) -> Result<&'static str, FleetTelemetryError> {
    match normalized_key(value).as_str() {
        "connected" | "online" => Ok("online"),
        "disconnected" | "offline" => Ok("offline"),
        _ => Err(FleetTelemetryError::InvalidFieldValue),
    }
}

fn scalar_text(value: &Value) -> Result<String, FleetTelemetryError> {
    let text = value
        .as_str()
        .map(str::trim)
        .filter(|text| {
            !text.is_empty() && text.len() <= MAX_TEXT_BYTES && !text.chars().any(char::is_control)
        })
        .ok_or(FleetTelemetryError::InvalidFieldValue)?;
    Ok(text.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoorStateLayout {
    Modern,
    Legacy,
    Unknown,
}

fn door_state_layout(
    firmware_version: Option<&str>,
    device_client_version: Option<&str>,
) -> DoorStateLayout {
    if let Some(version) = firmware_version.and_then(version_triplet) {
        return if version >= DOOR_STATE_LAYOUT_FIX_FIRMWARE {
            DoorStateLayout::Modern
        } else {
            DoorStateLayout::Legacy
        };
    }

    // Fleet's device_client_version is a client release, not firmware. A
    // current 1.x client is known to be paired with firmware after the Tesla
    // door-layout fix; older or unparseable metadata cannot prove the layout.
    match device_client_version.and_then(version_triplet) {
        Some((major, ..)) if (1..2024).contains(&major) => DoorStateLayout::Modern,
        Some(version) if version >= DOOR_STATE_LAYOUT_FIX_FIRMWARE => DoorStateLayout::Modern,
        _ => DoorStateLayout::Unknown,
    }
}

fn version_triplet(value: &str) -> Option<(u32, u32, u32)> {
    let value = value.trim().strip_prefix('v').unwrap_or(value);
    let mut parts = value.split('.');
    let major = version_component(parts.next()?)?;
    let minor = version_component(parts.next()?)?;
    let patch = version_component(parts.next()?)?;
    Some((major, minor, patch))
}

fn version_component(value: &str) -> Option<u32> {
    let digits = value
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn map_known_field(
    owner: &mut Map<String, Value>,
    key: &str,
    value: DecodedValue,
    timestamp_ms: i64,
    door_state_layout: DoorStateLayout,
) -> Result<(), FleetTelemetryError> {
    if key == "location" {
        let DecodedValue::Location {
            latitude,
            longitude,
        } = value
        else {
            return Err(FleetTelemetryError::InvalidCoordinates);
        };
        clear_group_field(owner, Group::Drive, "native_location_elevation");
        clear_group_field(owner, Group::Drive, "elevation");
        set_number(owner, Group::Drive, "latitude", latitude, timestamp_ms);
        set_number(owner, Group::Drive, "longitude", longitude, timestamp_ms);
        return Ok(());
    }
    if key == "doorstate" {
        let DecodedValue::Structured(fields) = value else {
            return Err(FleetTelemetryError::InvalidFieldValue);
        };
        return map_doors(owner, &fields, timestamp_ms, door_state_layout);
    }
    if key == "tpmssoftwarnings" {
        let DecodedValue::Structured(fields) = value else {
            return Err(FleetTelemetryError::InvalidFieldValue);
        };
        return map_tire_warnings(owner, &fields, timestamp_ms);
    }
    let DecodedValue::Scalar(value) = value else {
        return Err(FleetTelemetryError::InvalidFieldValue);
    };
    match key {
        "latitude" => set_checked_coordinate(owner, "latitude", &value, timestamp_ms)?,
        "longitude" => set_checked_coordinate(owner, "longitude", &value, timestamp_ms)?,
        "vehiclespeed" | "speed" => set_number(
            owner,
            Group::Drive,
            "speed",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "gear" | "shiftstate" => set_text(
            owner,
            Group::Drive,
            "shift_state",
            normalize_gear(&scalar_text(&value)?),
            timestamp_ms,
        ),
        "power" => set_number(
            owner,
            Group::Drive,
            "power",
            scalar_f64(&value)?,
            timestamp_ms,
        ),
        "heading" | "gpsheading" => set_number(
            owner,
            Group::Drive,
            "heading",
            bounded_heading(&value)?,
            timestamp_ms,
        ),
        "elevation" => set_number(
            owner,
            Group::Drive,
            "native_location_elevation",
            scalar_f64(&value)?,
            timestamp_ms,
        ),
        "batterylevel" => set_integer(
            owner,
            Group::Charge,
            "battery_level",
            bounded_percent(&value)?,
            timestamp_ms,
        ),
        "soc" | "usablebatterylevel" | "usablesoc" => set_integer(
            owner,
            Group::Charge,
            "usable_battery_level",
            bounded_percent(&value)?,
            timestamp_ms,
        ),
        "ratedrange" | "batteryrange" => set_number(
            owner,
            Group::Charge,
            "battery_range",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "idealrange" | "idealbatteryrange" => set_number(
            owner,
            Group::Charge,
            "ideal_battery_range",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "estimatedrange" | "estrange" | "estbatteryrange" => set_number(
            owner,
            Group::Charge,
            "est_battery_range",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "chargestate" | "detailedchargestate" | "chargingstate" => set_text(
            owner,
            Group::Charge,
            "charging_state",
            normalize_charge_state(&scalar_text(&value)?),
            timestamp_ms,
        ),
        // The cumulative counter is DC energy in; the AC-side signal is
        // recognised but never feeds it.
        "chargeenergyadded" => {}
        "fastchargerpresent" => set_bool(
            owner,
            Group::Charge,
            "fast_charger_present",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "fastchargertype" => set_text(
            owner,
            Group::Charge,
            "fast_charger_type",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "dcchargingenergyin" => set_number(
            owner,
            Group::Charge,
            "charge_energy_added",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "chargeractualcurrent" | "chargeamps" => set_number(
            owner,
            Group::Charge,
            "charger_actual_current",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "chargerpilotcurrent" => set_number(
            owner,
            Group::Charge,
            "charger_pilot_current",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "chargervoltage" => set_number(
            owner,
            Group::Charge,
            "charger_voltage",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "chargerphases" => set_integer(
            owner,
            Group::Charge,
            "charger_phases",
            scalar_i64(&value)?,
            timestamp_ms,
        ),
        "chargetofull" | "timetofullcharge" => set_number(
            owner,
            Group::Charge,
            "time_to_full_charge",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "chargelimitsoc" => set_integer(
            owner,
            Group::Charge,
            "charge_limit_soc",
            bounded_percent(&value)?,
            timestamp_ms,
        ),
        "chargeportdooropen" => set_bool(
            owner,
            Group::Charge,
            "charge_port_door_open",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "chargecable" | "connchargecable" => set_text(
            owner,
            Group::Charge,
            "conn_charge_cable",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "insidetemp" => set_number(
            owner,
            Group::Climate,
            "inside_temp",
            scalar_f64(&value)?,
            timestamp_ms,
        ),
        "outsidetemp" => set_number(
            owner,
            Group::Climate,
            "outside_temp",
            scalar_f64(&value)?,
            timestamp_ms,
        ),
        "hvacpower" => set_bool(
            owner,
            Group::Climate,
            "is_climate_on",
            hvac_power_on(&value)?,
            timestamp_ms,
        ),
        "isclimateon" => set_bool(
            owner,
            Group::Climate,
            "is_climate_on",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "preconditioningenabled" | "ispreconditioning" => set_bool(
            owner,
            Group::Climate,
            "is_preconditioning",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "climatekeepermode" => set_text(
            owner,
            Group::Climate,
            "climate_keeper_mode",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "hvacfanstatus" | "fanstatus" => set_integer(
            owner,
            Group::Climate,
            "fan_status",
            scalar_i64(&value)?,
            timestamp_ms,
        ),
        "hvaclefttemperaturerequest" | "drivertempsetting" => set_number(
            owner,
            Group::Climate,
            "driver_temp_setting",
            scalar_f64(&value)?,
            timestamp_ms,
        ),
        "hvacrighttemperaturerequest" | "passengertempsetting" => set_number(
            owner,
            Group::Climate,
            "passenger_temp_setting",
            scalar_f64(&value)?,
            timestamp_ms,
        ),
        "frontdefroster" | "isfrontdefrosteron" => set_bool(
            owner,
            Group::Climate,
            "is_front_defroster_on",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "reardefroster" | "isreardefrosteron" => set_bool(
            owner,
            Group::Climate,
            "is_rear_defroster_on",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "odometer" => set_number(
            owner,
            Group::Vehicle,
            "odometer",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "locked" => set_bool(
            owner,
            Group::Vehicle,
            "locked",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "sentrymode" => set_bool(
            owner,
            Group::Vehicle,
            "sentry_mode",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "isuserpresent" => set_bool(
            owner,
            Group::Vehicle,
            "is_user_present",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "servicemode" => set_bool(
            owner,
            Group::Vehicle,
            "service_mode",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "version" | "carversion" | "firmwareversion" => set_text(
            owner,
            Group::Vehicle,
            "car_version",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "doorstate" => unreachable!("structured door state handled above"),
        "driverfrontdoor" => set_open(owner, "df", &value, timestamp_ms)?,
        "driverreardoor" => set_open(owner, "dr", &value, timestamp_ms)?,
        "passengerfrontdoor" => set_open(owner, "pf", &value, timestamp_ms)?,
        "passengerreardoor" => set_open(owner, "pr", &value, timestamp_ms)?,
        "fdwindow" | "driverfrontwindow" => set_open(owner, "fd_window", &value, timestamp_ms)?,
        "rdwindow" | "driverrearwindow" => set_open(owner, "rd_window", &value, timestamp_ms)?,
        "fpwindow" | "passengerfrontwindow" => set_open(owner, "fp_window", &value, timestamp_ms)?,
        "rpwindow" | "passengerrearwindow" => set_open(owner, "rp_window", &value, timestamp_ms)?,
        "trunk" | "reartrunk" => set_open(owner, "rt", &value, timestamp_ms)?,
        "frunk" | "fronttrunk" => set_open(owner, "ft", &value, timestamp_ms)?,
        "tpmspressurefl" => set_number(
            owner,
            Group::Vehicle,
            "tpms_pressure_fl",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "tpmspressurefr" => set_number(
            owner,
            Group::Vehicle,
            "tpms_pressure_fr",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "tpmspressurerl" => set_number(
            owner,
            Group::Vehicle,
            "tpms_pressure_rl",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "tpmspressurerr" => set_number(
            owner,
            Group::Vehicle,
            "tpms_pressure_rr",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "tpmssoftwarningfl" => set_bool(
            owner,
            Group::Vehicle,
            "tpms_soft_warning_fl",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "tpmssoftwarningfr" => set_bool(
            owner,
            Group::Vehicle,
            "tpms_soft_warning_fr",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "tpmssoftwarningrl" => set_bool(
            owner,
            Group::Vehicle,
            "tpms_soft_warning_rl",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "tpmssoftwarningrr" => set_bool(
            owner,
            Group::Vehicle,
            "tpms_soft_warning_rr",
            scalar_bool(&value)?,
            timestamp_ms,
        ),
        "vehiclename" | "displayname" => set_text(
            owner,
            Group::Vehicle,
            "vehicle_name",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "cartype" | "model" => set_text(
            owner,
            Group::Config,
            "car_type",
            normalize_car_type(&scalar_text(&value)?),
            timestamp_ms,
        ),
        "trimbadging" | "trim" => set_text(
            owner,
            Group::Config,
            "trim_badging",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "exteriorcolor" => set_text(
            owner,
            Group::Config,
            "exterior_color",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "wheeltype" => set_text(
            owner,
            Group::Config,
            "wheel_type",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "spoilertype" => set_text(
            owner,
            Group::Config,
            "spoiler_type",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "softwareupdatestatus" | "updatestatus" => set_text(
            owner,
            Group::SoftwareUpdate,
            "status",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "softwareupdateversion" | "updateversion" => set_text(
            owner,
            Group::SoftwareUpdate,
            "version",
            scalar_text(&value)?,
            timestamp_ms,
        ),
        "softwareupdatedownloadpercentcomplete"
        | "softwareupdatedownloadpercent"
        | "downloadperc" => set_integer(
            owner,
            Group::SoftwareUpdate,
            "download_perc",
            bounded_percent(&value)?,
            timestamp_ms,
        ),
        "softwareupdateinstallationpercentcomplete"
        | "softwareupdateinstallpercent"
        | "installperc" => set_integer(
            owner,
            Group::SoftwareUpdate,
            "install_perc",
            bounded_percent(&value)?,
            timestamp_ms,
        ),
        "tpmssoftwarnings" => unreachable!("structured tire warnings handled above"),
        _ => return Err(FleetTelemetryError::InvalidFieldName),
    }
    Ok(())
}

fn target_group(key: &str) -> Option<Group> {
    match key {
        "location" | "latitude" | "longitude" | "vehiclespeed" | "speed" | "gear"
        | "shiftstate" | "power" | "heading" | "gpsheading" | "elevation" | "packvoltage"
        | "packcurrent" => Some(Group::Drive),
        "soc"
        | "batterylevel"
        | "usablebatterylevel"
        | "usablesoc"
        | "ratedrange"
        | "batteryrange"
        | "idealrange"
        | "idealbatteryrange"
        | "estimatedrange"
        | "estrange"
        | "estbatteryrange"
        | "chargestate"
        | "detailedchargestate"
        | "chargingstate"
        | "chargeenergyadded"
        | "dcchargingenergyin"
        | "chargerpower"
        | "fastchargerpresent"
        | "fastchargertype"
        | "acchargingpower"
        | "dcchargingpower"
        | "chargeractualcurrent"
        | "chargeamps"
        | "chargerpilotcurrent"
        | "chargervoltage"
        | "chargerphases"
        | "chargetofull"
        | "timetofullcharge"
        | "chargelimitsoc"
        | "chargeportdooropen"
        | "chargecable"
        | "connchargecable" => Some(Group::Charge),
        "insidetemp"
        | "outsidetemp"
        | "hvacpower"
        | "isclimateon"
        | "preconditioningenabled"
        | "ispreconditioning"
        | "climatekeepermode"
        | "hvacfanstatus"
        | "fanstatus"
        | "hvaclefttemperaturerequest"
        | "drivertempsetting"
        | "hvacrighttemperaturerequest"
        | "passengertempsetting"
        | "frontdefroster"
        | "isfrontdefrosteron"
        | "reardefroster"
        | "isreardefrosteron" => Some(Group::Climate),
        "cartype" | "model" | "trimbadging" | "trim" | "exteriorcolor" | "wheeltype"
        | "spoilertype" => Some(Group::Config),
        "softwareupdatestatus"
        | "updatestatus"
        | "softwareupdateversion"
        | "updateversion"
        | "softwareupdatedownloadpercentcomplete"
        | "softwareupdatedownloadpercent"
        | "downloadperc"
        | "softwareupdateinstallationpercentcomplete"
        | "softwareupdateinstallpercent"
        | "installperc" => Some(Group::SoftwareUpdate),
        "odometer"
        | "locked"
        | "sentrymode"
        | "isuserpresent"
        | "servicemode"
        | "version"
        | "carversion"
        | "firmwareversion"
        | "doorstate"
        | "driverfrontdoor"
        | "driverreardoor"
        | "passengerfrontdoor"
        | "passengerreardoor"
        | "driverfrontwindow"
        | "fdwindow"
        | "rdwindow"
        | "fpwindow"
        | "rpwindow"
        | "driverrearwindow"
        | "passengerfrontwindow"
        | "passengerrearwindow"
        | "trunk"
        | "reartrunk"
        | "frunk"
        | "fronttrunk"
        | "tpmspressurefl"
        | "tpmspressurefr"
        | "tpmspressurerl"
        | "tpmspressurerr"
        | "tpmssoftwarningfl"
        | "tpmssoftwarningfr"
        | "tpmssoftwarningrl"
        | "tpmssoftwarningrr"
        | "tpmssoftwarnings"
        | "vehiclename"
        | "displayname" => Some(Group::Vehicle),
        _ => None,
    }
}

fn group_name(group: Group) -> &'static str {
    match group {
        Group::Drive => "drive_state",
        Group::Charge => "charge_state",
        Group::Climate => "climate_state",
        Group::Vehicle | Group::SoftwareUpdate => "vehicle_state",
        Group::Config => "vehicle_config",
    }
}

fn group_timestamp(owner: &Map<String, Value>, group: Group) -> Option<i64> {
    owner
        .get(group_name(group))
        .and_then(Value::as_object)
        .and_then(|fields| fields.get("timestamp"))
        .and_then(json_i64)
}

fn group_mut(
    owner: &mut Map<String, Value>,
    group: Group,
    timestamp_ms: i64,
) -> &mut Map<String, Value> {
    let group_name = group_name(group).to_owned();
    if !owner.get(&group_name).is_some_and(Value::is_object) {
        owner.insert(group_name.clone(), Value::Object(Map::new()));
    }
    let fields = owner
        .get_mut(&group_name)
        .and_then(Value::as_object_mut)
        .expect("group was inserted as an object");
    let stamped = fields
        .get("timestamp")
        .and_then(json_i64)
        .map_or(timestamp_ms, |current| current.max(timestamp_ms));
    fields.insert("timestamp".to_owned(), Value::Number(stamped.into()));
    fields
}

fn set_number(owner: &mut Map<String, Value>, group: Group, key: &str, value: f64, at: i64) {
    let number = serde_json::Number::from_f64(value).expect("validated finite number");
    group_mut(owner, group, at).insert(key.to_owned(), Value::Number(number));
}

fn clear_group_field(owner: &mut Map<String, Value>, group: Group, key: &str) {
    if let Some(fields) = owner
        .get_mut(group_name(group))
        .and_then(Value::as_object_mut)
    {
        fields.remove(key);
    }
}

fn set_integer(owner: &mut Map<String, Value>, group: Group, key: &str, value: i64, at: i64) {
    if matches!(group, Group::SoftwareUpdate) {
        software_update_mut(owner, at).insert(key.to_owned(), Value::Number(value.into()));
    } else {
        group_mut(owner, group, at).insert(key.to_owned(), Value::Number(value.into()));
    }
}

fn set_bool(owner: &mut Map<String, Value>, group: Group, key: &str, value: bool, at: i64) {
    group_mut(owner, group, at).insert(key.to_owned(), Value::Bool(value));
}

fn set_text(owner: &mut Map<String, Value>, group: Group, key: &str, value: String, at: i64) {
    if matches!(group, Group::SoftwareUpdate) {
        software_update_mut(owner, at).insert(key.to_owned(), Value::String(value));
    } else {
        group_mut(owner, group, at).insert(key.to_owned(), Value::String(value));
    }
}

fn software_update_mut(owner: &mut Map<String, Value>, at: i64) -> &mut Map<String, Value> {
    let vehicle = group_mut(owner, Group::Vehicle, at);
    if !vehicle.get("software_update").is_some_and(Value::is_object) {
        vehicle.insert("software_update".to_owned(), Value::Object(Map::new()));
    }
    vehicle
        .get_mut("software_update")
        .and_then(Value::as_object_mut)
        .expect("software update was inserted as an object")
}

fn set_open(
    owner: &mut Map<String, Value>,
    key: &str,
    value: &Value,
    at: i64,
) -> Result<(), FleetTelemetryError> {
    set_integer(
        owner,
        Group::Vehicle,
        key,
        i64::from(scalar_bool(value)?),
        at,
    );
    Ok(())
}

fn map_doors(
    owner: &mut Map<String, Value>,
    fields: &Map<String, Value>,
    at: i64,
    layout: DoorStateLayout,
) -> Result<(), FleetTelemetryError> {
    let fields = normalized_bool_fields(fields)?;
    if layout == DoorStateLayout::Unknown {
        // Tesla swapped DriverRear and PassengerFront on older firmware. Do
        // not retain a stale value when the metadata cannot prove the layout.
        clear_group_field(owner, Group::Vehicle, "dr");
        clear_group_field(owner, Group::Vehicle, "pf");
    }
    let mapping = match layout {
        DoorStateLayout::Modern | DoorStateLayout::Unknown => [
            ("driverfront", "df"),
            ("driverrear", "dr"),
            ("passengerfront", "pf"),
            ("passengerrear", "pr"),
            ("trunkfront", "ft"),
            ("trunkrear", "rt"),
        ],
        DoorStateLayout::Legacy => [
            ("driverfront", "df"),
            ("driverrear", "pf"),
            ("passengerfront", "dr"),
            ("passengerrear", "pr"),
            ("trunkfront", "ft"),
            ("trunkrear", "rt"),
        ],
    };
    for (source, target) in mapping {
        if layout == DoorStateLayout::Unknown && matches!(source, "driverrear" | "passengerfront") {
            continue;
        }
        set_integer(
            owner,
            Group::Vehicle,
            target,
            i64::from(fields.get(source).copied().unwrap_or(false)),
            at,
        );
    }
    Ok(())
}

fn map_tire_warnings(
    owner: &mut Map<String, Value>,
    fields: &Map<String, Value>,
    at: i64,
) -> Result<(), FleetTelemetryError> {
    let fields = normalized_bool_fields(fields)?;
    for (source, target) in [
        ("frontleft", "tpms_soft_warning_fl"),
        ("frontright", "tpms_soft_warning_fr"),
        ("rearleft", "tpms_soft_warning_rl"),
        ("rearright", "tpms_soft_warning_rr"),
    ] {
        set_bool(
            owner,
            Group::Vehicle,
            target,
            fields.get(source).copied().unwrap_or(false),
            at,
        );
    }
    Ok(())
}

fn normalized_bool_fields(
    fields: &Map<String, Value>,
) -> Result<BTreeMap<String, bool>, FleetTelemetryError> {
    let mut normalized = BTreeMap::new();
    for (key, value) in fields {
        if normalized
            .insert(normalized_key(key), scalar_bool(value)?)
            .is_some()
        {
            return Err(FleetTelemetryError::InvalidFieldValue);
        }
    }
    Ok(normalized)
}

fn set_checked_coordinate(
    owner: &mut Map<String, Value>,
    key: &str,
    value: &Value,
    at: i64,
) -> Result<(), FleetTelemetryError> {
    let coordinate = scalar_f64(value)?;
    let valid = if key == "latitude" {
        (-90.0..=90.0).contains(&coordinate)
    } else {
        (-180.0..=180.0).contains(&coordinate)
    };
    if !valid {
        return Err(FleetTelemetryError::InvalidCoordinates);
    }
    set_number(owner, Group::Drive, key, coordinate, at);
    Ok(())
}

fn bounded_percent(value: &Value) -> Result<i64, FleetTelemetryError> {
    if let Ok(value) = scalar_i64(value) {
        return (0..=100)
            .contains(&value)
            .then_some(value)
            .ok_or(FleetTelemetryError::InvalidFieldValue);
    }
    let value = scalar_f64(value)?;
    if !(0.0..=100.0).contains(&value) {
        return Err(FleetTelemetryError::InvalidFieldValue);
    }
    let rounded = value.round();
    if !(0.0..=100.0).contains(&rounded) {
        return Err(FleetTelemetryError::InvalidFieldValue);
    }
    Ok(rounded as i64)
}

fn nonnegative(value: &Value) -> Result<f64, FleetTelemetryError> {
    let value = scalar_f64(value)?;
    (value >= 0.0)
        .then_some(value)
        .ok_or(FleetTelemetryError::InvalidFieldValue)
}

fn bounded_heading(value: &Value) -> Result<f64, FleetTelemetryError> {
    let value = scalar_f64(value)?;
    (0.0..=360.0)
        .contains(&value)
        .then_some(value)
        .ok_or(FleetTelemetryError::InvalidFieldValue)
}

fn normalize_gear(value: &str) -> String {
    let normalized = normalized_key(value);
    match normalized.as_str() {
        "drive" | "d" | "shiftstated" => "D",
        "reverse" | "r" | "shiftstater" => "R",
        "neutral" | "n" | "shiftstaten" => "N",
        "park" | "p" | "shiftstatep" => "P",
        _ => value,
    }
    .to_owned()
}

fn normalize_charge_state(value: &str) -> String {
    let normalized = normalized_key(value);
    if normalized.ends_with("charging") {
        "Charging".to_owned()
    } else if normalized.ends_with("starting") {
        "Starting".to_owned()
    } else if normalized.ends_with("complete") || normalized.ends_with("completed") {
        "Complete".to_owned()
    } else if normalized.ends_with("disconnected") || normalized.ends_with("unplugged") {
        "Disconnected".to_owned()
    } else if normalized.ends_with("stopped") {
        "Stopped".to_owned()
    } else if normalized.ends_with("nopower") {
        "NoPower".to_owned()
    } else {
        value.to_owned()
    }
}

fn normalize_car_type(value: &str) -> String {
    let normalized = normalized_key(value);
    match normalized.as_str() {
        "cartypemodels" => "models",
        "cartypemodelx" => "modelx",
        "cartypemodel3" => "model3",
        "cartypemodely" => "modely",
        "cartypesemitruck" => "semi",
        "cartypecybertruck" => "cybertruck",
        _ => value,
    }
    .to_owned()
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn sanitize_existing_owner(
    source: &Map<String, Value>,
) -> Result<Map<String, Value>, FleetTelemetryError> {
    let mut target = Map::new();
    for key in [
        "id",
        "vehicle_id",
        "vin",
        "display_name",
        "state",
        "created_at",
    ] {
        if let Some(value) = source.get(key).and_then(safe_scalar) {
            target.insert(key.to_owned(), value);
        }
    }
    for group in [
        "drive_state",
        "charge_state",
        "climate_state",
        "vehicle_state",
        "vehicle_config",
        "gui_settings",
    ] {
        let Some(fields) = source.get(group).and_then(Value::as_object) else {
            continue;
        };
        let mut copied = Map::new();
        for (key, value) in fields {
            if key == "software_update" {
                let Some(update) = value.as_object() else {
                    return Err(FleetTelemetryError::InvalidExistingState);
                };
                let update = update
                    .iter()
                    .filter_map(|(key, value)| safe_scalar(value).map(|value| (key.clone(), value)))
                    .collect();
                copied.insert(key.clone(), Value::Object(update));
            } else if let Some(value) = safe_scalar(value) {
                copied.insert(key.clone(), value);
            }
        }
        if !copied.is_empty() {
            target.insert(group.to_owned(), Value::Object(copied));
        }
    }
    Ok(target)
}

fn safe_scalar(value: &Value) -> Option<Value> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::String(text)
            if text.len() <= MAX_TEXT_BYTES && !text.chars().any(char::is_control) =>
        {
            Some(value.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "fleet_telemetry/tests.rs"]
mod tests;
