// SPDX-License-Identifier: AGPL-3.0-only

//! Bounded normalization for trusted-loopback Fleet Telemetry sidecars.
//!
//! This module deliberately owns no socket, credential, or database state. It
//! validates one complete protojson transaction from a local sidecar and folds
//! known Fleet fields into the Owner-API-shaped object already consumed by the
//! Hub lifecycle and current-state projectors.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
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
    created_at_ms: i64,
}

#[derive(Debug, Clone, Copy)]
struct TimedPackValue {
    value: f64,
    timestamp_ms: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTransaction {
    version: u32,
    vin: String,
    txid: String,
    tx_type: String,
    #[serde(
        default,
        rename = "device_client_version",
        alias = "deviceClientVersion"
    )]
    _device_client_version: Option<String>,
    #[serde(default, rename = "firmware_version", alias = "firmwareVersion")]
    _firmware_version: Option<String>,
    received_at_ms: i64,
    timestamp_ms: i64,
    payload: WirePayload,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WirePayload {
    Vehicle(WireVehiclePayload),
    Connectivity(WireConnectivityPayload),
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
        rename = "connectionId",
        alias = "connection_id",
        alias = "ConnectionId"
    )]
    connection_id: String,
    #[serde(rename = "status", alias = "Status")]
    status: String,
    #[serde(rename = "createdAt", alias = "created_at", alias = "CreatedAt")]
    created_at: Value,
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
            created_at_ms,
        })
    }

    pub fn empty(vin: &str) -> Result<Self, FleetTelemetryError> {
        Self::restore(vin, &json!({}))
    }

    pub fn apply_json(
        &mut self,
        input: &[u8],
    ) -> Result<FleetTelemetrySnapshot, FleetTelemetryError> {
        self.apply(parse_transaction(input)?)
    }

    pub fn owner_data(&self) -> Value {
        Value::Object(self.owner_data.clone())
    }

    fn apply(
        &mut self,
        transaction: WireTransaction,
    ) -> Result<FleetTelemetrySnapshot, FleetTelemetryError> {
        let transaction_vin = validate_envelope(&transaction)?;
        if transaction_vin != self.vin {
            return Err(FleetTelemetryError::VinMismatch);
        }
        let mut next_owner_data = self.owner_data.clone();
        let mut next_field_watermarks = self.field_watermarks.clone();
        let mut next_pack_voltage = self.pack_voltage;
        let mut next_pack_current = self.pack_current;
        let mut pack_value_changed = false;
        let mut updated = BTreeSet::new();
        let mut unavailable = BTreeSet::new();
        let mut unknown = BTreeSet::new();
        let mut regressed = BTreeSet::new();

        match transaction.payload {
            WirePayload::Vehicle(payload) => {
                if !matches!(
                    normalized_key(&transaction.tx_type).as_str(),
                    "v" | "data" | "vehicledata"
                ) {
                    return Err(FleetTelemetryError::InvalidTransactionIdentity);
                }
                if let Some(payload_vin) = payload.vin.as_deref()
                    && validate_vin(payload_vin)? != self.vin
                {
                    return Err(FleetTelemetryError::VinMismatch);
                }
                if let Some(created_at) = payload.created_at.as_ref() {
                    validate_created_at(created_at)?;
                }
                let state_watermark = next_field_watermarks
                    .get("__connectivity")
                    .copied()
                    .or_else(|| (self.created_at_ms > 0).then_some(self.created_at_ms));
                if state_watermark.is_none_or(|watermark| transaction.timestamp_ms > watermark) {
                    // A V record proves online state at its event time. This
                    // recovers from a lost best-effort CONNECTED record without
                    // overriding a newer offline transition.
                    next_owner_data.insert("state".to_owned(), Value::String("online".to_owned()));
                    next_field_watermarks
                        .insert("__connectivity".to_owned(), transaction.timestamp_ms);
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
                    transaction
                        ._firmware_version
                        .as_deref()
                        .or(transaction_firmware_version.as_deref())
                        .or(owner_firmware_version),
                    transaction._device_client_version.as_deref(),
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
                    if watermark.is_some_and(|watermark| transaction.timestamp_ms <= watermark) {
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
                                _ => {}
                            }
                            next_field_watermarks.insert(key, transaction.timestamp_ms);
                            unavailable.insert(source_name);
                        }
                        DecodedValue::Unknown => {
                            let pack_field = match key.as_str() {
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
                                _ => false,
                            };
                            if pack_field {
                                next_field_watermarks.insert(key, transaction.timestamp_ms);
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
                                        timestamp_ms: transaction.timestamp_ms,
                                    });
                                    pack_value_changed = true;
                                }
                                "packcurrent" => {
                                    let DecodedValue::Scalar(value) = value else {
                                        return Err(FleetTelemetryError::InvalidFieldValue);
                                    };
                                    next_pack_current = Some(TimedPackValue {
                                        value: scalar_f64(&value)?,
                                        timestamp_ms: transaction.timestamp_ms,
                                    });
                                    pack_value_changed = true;
                                }
                                _ => map_known_field(
                                    &mut next_owner_data,
                                    &key,
                                    value,
                                    transaction.timestamp_ms,
                                    door_state_layout,
                                )?,
                            }
                            next_field_watermarks.insert(key, transaction.timestamp_ms);
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
            }
            WirePayload::Connectivity(payload) => {
                if normalized_key(&transaction.tx_type) != "connectivity" {
                    return Err(FleetTelemetryError::InvalidTransactionIdentity);
                }
                validate_identifier(&payload.connection_id, MAX_TXID_BYTES)?;
                validate_created_at(&payload.created_at)?;
                let state = connectivity_state(&payload.status)?;
                let watermark = next_field_watermarks
                    .get("__connectivity")
                    .copied()
                    .or_else(|| (self.created_at_ms > 0).then_some(self.created_at_ms));
                if watermark.is_some_and(|watermark| transaction.timestamp_ms <= watermark) {
                    regressed.insert("Connectivity".to_owned());
                } else {
                    next_owner_data.insert("state".to_owned(), Value::String(state.to_owned()));
                    next_pack_voltage = None;
                    next_pack_current = None;
                    clear_group_field(&mut next_owner_data, Group::Drive, "power");
                    next_field_watermarks
                        .insert("__connectivity".to_owned(), transaction.timestamp_ms);
                    updated.insert("Connectivity".to_owned());
                }
            }
        }

        let created_at_ms = self.created_at_ms.max(transaction.timestamp_ms);
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
        self.created_at_ms = created_at_ms;

        Ok(FleetTelemetrySnapshot {
            vin: self.vin.clone(),
            txid: transaction.txid,
            tx_type: transaction.tx_type,
            received_at_ms: transaction.received_at_ms,
            timestamp_ms: transaction.timestamp_ms,
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

pub fn vin_from_json(input: &[u8]) -> Result<String, FleetTelemetryError> {
    let transaction = parse_transaction(input)?;
    validate_envelope(&transaction)
}

fn parse_transaction(input: &[u8]) -> Result<WireTransaction, FleetTelemetryError> {
    if input.len() > MAX_FLEET_TELEMETRY_INPUT_BYTES {
        return Err(FleetTelemetryError::InputTooLarge);
    }
    serde_json::from_slice(input).map_err(|_| FleetTelemetryError::InvalidJson)
}

fn validate_envelope(transaction: &WireTransaction) -> Result<String, FleetTelemetryError> {
    if transaction.version != 1 {
        return Err(FleetTelemetryError::UnsupportedVersion);
    }
    let vin = validate_vin(&transaction.vin)?;
    validate_identifier(&transaction.txid, MAX_TXID_BYTES)?;
    validate_identifier(&transaction.tx_type, MAX_TX_TYPE_BYTES)?;
    if let Some(version) = transaction._device_client_version.as_deref()
        && (version.is_empty()
            || version.len() > 64
            || !version.bytes().all(|byte| byte.is_ascii_graphic()))
    {
        return Err(FleetTelemetryError::InvalidTransactionIdentity);
    }
    if let Some(version) = transaction._firmware_version.as_deref()
        && (version.is_empty()
            || version.len() > 64
            || !version.bytes().all(|byte| byte.is_ascii_graphic()))
    {
        return Err(FleetTelemetryError::InvalidTransactionIdentity);
    }
    if transaction.received_at_ms < EARLIEST_TIMESTAMP_MS
        || transaction.timestamp_ms < EARLIEST_TIMESTAMP_MS
        || transaction.timestamp_ms > transaction.received_at_ms.saturating_add(FUTURE_SKEW_MS)
    {
        return Err(FleetTelemetryError::InvalidTimestamp);
    }
    match &transaction.payload {
        WirePayload::Vehicle(payload) => {
            if !matches!(
                normalized_key(&transaction.tx_type).as_str(),
                "v" | "data" | "vehicledata"
            ) {
                return Err(FleetTelemetryError::InvalidTransactionIdentity);
            }
            if let Some(payload_vin) = payload.vin.as_deref()
                && validate_vin(payload_vin)? != vin
            {
                return Err(FleetTelemetryError::VinMismatch);
            }
            if let Some(created_at) = payload.created_at.as_ref() {
                validate_created_at(created_at)?;
            }
        }
        WirePayload::Connectivity(payload) => {
            if normalized_key(&transaction.tx_type) != "connectivity" {
                return Err(FleetTelemetryError::InvalidTransactionIdentity);
            }
            validate_identifier(&payload.connection_id, MAX_TXID_BYTES)?;
            validate_created_at(&payload.created_at)?;
            connectivity_state(&payload.status)?;
        }
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
    if value.len() != 17 {
        return Err(FleetTelemetryError::InvalidVin);
    }
    let value = value.to_ascii_uppercase();
    if !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        || value.bytes().any(|byte| matches!(byte, b'I' | b'O' | b'Q'))
    {
        return Err(FleetTelemetryError::InvalidVin);
    }
    Ok(value)
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
        "chargeenergyadded" | "dcchargingenergyin" => set_number(
            owner,
            Group::Charge,
            "charge_energy_added",
            nonnegative(&value)?,
            timestamp_ms,
        ),
        "chargerpower" | "acchargingpower" | "dcchargingpower" => set_number(
            owner,
            Group::Charge,
            "charger_power",
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
    fields.insert("timestamp".to_owned(), Value::Number(timestamp_ms.into()));
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
